// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Comprehensive TDD integration & pixel-verification test suite for `coree-render`.

use coree_core::geometry::EllipseMode;
use coree_core::{
    Affine2D, AnchorPoint, ArrowheadStyle, ArtisticText, BezierPath, BlendMode, BuiltinIccProfile,
    Color, ColorMapMode, ColorMode, ConnectorObject, ConnectorStyle, ContourMode, CornerStyle,
    DimensionKind, DimensionObject, DistortKind, Document, Effect, EnvelopeMode, FeatherDirection,
    Fill, GradientStop, GuideOrientation, LensConfig, LensKind, MasterLayerScope, MeshNode,
    NodePayload, ObjectId, OutlineStyle, ParagraphText, ParametricShape, Point2D,
    PostScriptFillPreset, RasterAsset, RasterFilter, RasterLayerData, Rect2D, SceneObject,
    SymbolDef, TextAlign, TextDirection, TextOnPath, TextStyle, TransparencyMode, Vec2D,
    VectorTable,
};
use coree_render::{
    apply_cutout_background_removal, apply_dodge_burn_stroke, apply_healing_stroke,
    apply_kashida_elongation, apply_raster_filter, clone_stamp_stroke, compute_page_layouts,
    compute_smart_fill, convert_object_to_bitmap, convert_object_to_bitmap_with_bounds,
    erase_stroke, hit_test_node, hit_test_point, hit_test_rect, list_glyphs_for_block,
    paint_brush_stroke, paragraph_to_curves, raster_flood_fill, render_page_to_rgba,
    render_page_to_rgba_with_options, render_svg_to_rgba, resample, smudge_stroke,
    text_on_path_to_curves, text_to_curves, trace_raster, trace_raster_to_mm, BackendPaint,
    CanvasBackend, DirtyTileCache, DodgeBurnMode, DynamicGuideKind, FillRule, GlyphBlockFilter,
    OutlinePreset, PixelRect, RasterMask, RenderOptions, RgbaBuffer, SceneRenderer, SnapEngine,
    SnapOptions, StrokeParams, TextEngine, TinySkiaBackend, ToneRange, TraceConfig, TraceMode,
    ViewMode, Viewport, TILE_SIZE_PX,
};

fn rect_obj(x: f64, y: f64, w: f64, h: f64, fill: Fill) -> SceneObject {
    SceneObject::new(
        ObjectId(0),
        "Rect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: w,
            height: h,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(fill)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(x, y))
}

fn make_rect(x: f64, y: f64, w: f64, h: f64, color: Color) -> SceneObject {
    SceneObject::new(
        ObjectId(0),
        "Rect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: w,
            height: h,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(color))
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(x, y))
}

fn make_circle(cx: f64, cy: f64, r: f64, color: Color) -> SceneObject {
    SceneObject::new(
        ObjectId(0),
        "Circle",
        NodePayload::Shape(ParametricShape::Ellipse {
            rx: r,
            ry: r,
            mode: EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }),
    )
    .with_fill(Fill::Solid(color))
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(cx, cy))
}

fn pixel_at_mm(img: &coree_render::RenderedImage, x_mm: f64, y_mm: f64) -> [u8; 4] {
    let px_per_mm = img.dpi / 25.4;
    let x = (x_mm * px_per_mm).round() as u32;
    let y = (y_mm * px_per_mm).round() as u32;
    img.pixel(x, y)
}

fn count_non_white_pixels(rgba: &[u8]) -> usize {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[3] > 10 && (px[0] < 245 || px[1] < 245 || px[2] < 245))
        .count()
}

// =============================================================================
// 1. CanvasBackend + DirtyTileCache Parity & LOD Dragging
// =============================================================================

#[test]
fn tinyskia_backend_and_dirty_tile_cache_exact_parity() {
    // Backend primitives: clear, clip, fill, stroke, read_region, composite_region
    let mut backend = TinySkiaBackend::new(100, 100).unwrap();
    backend.clear(Color::WHITE);
    let clip_rect = BezierPath::from_rect(Rect2D::from_xywh(20.0, 20.0, 40.0, 40.0));
    backend.push_clip(&clip_rect, FillRule::Winding, Affine2D::IDENTITY);
    let big = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));
    backend.fill_path(
        &big,
        &BackendPaint::solid(Color::rgba8(200, 40, 50, 255)),
        FillRule::Winding,
        Affine2D::IDENTITY,
    );
    backend.pop_clip();

    let sub = backend.read_region(PixelRect::new(30, 30, 10, 10));
    assert_eq!(sub.get_pixel(5, 5), Some([200, 40, 50, 255]));
    let full = backend.into_rgba();
    assert_eq!(full.get_pixel(10, 10), Some([255, 255, 255, 255]));
    assert_eq!(full.get_pixel(35, 35), Some([200, 40, 50, 255]));

    // Build a multi-object document spanning multiple 256x256 tiles (600x400 viewport = 3x2 = 6 tiles)
    let mut doc = Document::new("TileParity", 160.0, 100.0, ColorMode::Rgb, 96.0);
    // Rectangles crossing tile boundaries (x=256px ~ 67.7mm, y=256px ~ 67.7mm)
    doc.add_object(
        make_rect(10.0, 10.0, 40.0, 30.0, Color::rgba8(220, 30, 40, 255)),
        None,
    )
    .unwrap();
    let r2 = doc
        .add_object(
            make_rect(60.0, 40.0, 50.0, 45.0, Color::rgba8(30, 120, 230, 255)),
            None,
        )
        .unwrap();

    let viewport = Viewport::new(600, 400);
    let opts = RenderOptions::interactive();
    let mut renderer = SceneRenderer::new();

    // 1. Exact bit-for-bit pixel parity on polygonal/rectangular artwork crossing tile borders!
    let mut direct_backend = TinySkiaBackend::new(600, 400).unwrap();
    renderer.render_document(
        &doc,
        &viewport,
        ViewMode::Enhanced,
        &opts,
        &mut direct_backend,
    );
    let direct_rgba = direct_backend.into_rgba();

    let mut cache = DirtyTileCache::new(600, 400);
    assert_eq!(cache.tile_count(), 6); // ceil(600/256)=3, ceil(400/256)=2
    assert_eq!(TILE_SIZE_PX, 256);
    let initial_rendered =
        cache.render_dirty_tiles(&doc, &viewport, ViewMode::Enhanced, &opts, &mut renderer);
    assert_eq!(initial_rendered, 6);
    assert_eq!(cache.dirty_tile_count(), 0);
    let tiled_rgba = cache.composite_to_rgba();

    assert_eq!(direct_rgba.width, tiled_rgba.width);
    assert_eq!(direct_rgba.height, tiled_rgba.height);
    assert_eq!(
        direct_rgba.rgba, tiled_rgba.rgba,
        "DirtyTileCache must match full direct render bit-for-bit on tile-crossing polygons"
    );

    // No-op when nothing is dirty
    assert_eq!(
        cache.render_dirty_tiles(&doc, &viewport, ViewMode::Enhanced, &opts, &mut renderer),
        0
    );

    // 2. Add a curved circle crossing both x=256 and y=256 tile seams and verify parity
    //    (tiny-skia's 4x4 supersampled cubic EdgeClipper may quantize 1px AA curve rim by 1/4 step)
    let c_id = doc
        .add_object(
            make_circle(55.0, 65.0, 15.0, Color::rgba8(40, 190, 60, 255)),
            None,
        )
        .unwrap();
    cache.mark_dirty(doc.object_bounds(c_id).unwrap());
    let circle_tiles =
        cache.render_dirty_tiles(&doc, &viewport, ViewMode::Enhanced, &opts, &mut renderer);
    assert!(circle_tiles > 0 && circle_tiles <= 4);

    let mut direct_with_circle = TinySkiaBackend::new(600, 400).unwrap();
    renderer.render_document(
        &doc,
        &viewport,
        ViewMode::Enhanced,
        &opts,
        &mut direct_with_circle,
    );
    let d_buf = direct_with_circle.into_rgba();
    let t_buf = cache.composite_to_rgba();
    let total_px = (d_buf.width * d_buf.height) as usize;
    let diff_px = d_buf
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(t_buf.rgba.as_chunks::<4>().0.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        (diff_px as f64) / (total_px as f64) < 0.001,
        "Expected >99.9% exact pixel parity across curved tile seams, differed on {diff_px}/{total_px}"
    );
    // Circle interior is 100% bit-exact
    assert_eq!(d_buf.get_pixel(208, 246), t_buf.get_pixel(208, 246));

    // 3. Mutate r2 and mark ONLY r2's rect dirty -> fewer than all 6 tiles re-render,
    //    and output matches a freshly rendered DirtyTileCache bit-for-bit!
    let r2_bounds = doc.object_bounds(r2).unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetFill {
            ids: vec![r2],
            fill: Fill::Solid(Color::rgba8(240, 200, 10, 255)),
        },
    )
    .unwrap();
    cache.mark_dirty(r2_bounds);
    let dirty_count = cache.dirty_tile_count();
    assert!(
        dirty_count > 0 && dirty_count < 6,
        "expected partial tile invalidation, got {dirty_count}/6"
    );
    let re_rendered =
        cache.render_dirty_tiles(&doc, &viewport, ViewMode::Enhanced, &opts, &mut renderer);
    assert_eq!(re_rendered, dirty_count);

    let mut fresh_cache = DirtyTileCache::new(600, 400);
    let fresh_rgba =
        fresh_cache.render_viewport(&doc, &viewport, ViewMode::Enhanced, &opts, &mut renderer);
    assert_eq!(cache.composite_to_rgba().rgba, fresh_rgba.rgba);

    // 4. LOD dragging mode toggles cleanly
    let mut lod_vp = viewport;
    lod_vp.lod_dragging = true;
    let lod_buf = cache.render_viewport(&doc, &lod_vp, ViewMode::Enhanced, &opts, &mut renderer);
    assert_eq!(lod_buf.width, 600);
    assert!(count_non_white_pixels(&lod_buf.rgba) > 1000);
}

// =============================================================================
// 2. ALL 11 Optical & Color Lenses + Frozen + Viewpoint + RemoveFace
// =============================================================================

#[test]
fn all_eleven_lenses_produce_distinct_expected_pixels() {
    let make_scene = |lens_cfg: Option<LensConfig>| -> Document {
        let mut doc = Document::new("Lenses", 100.0, 100.0, ColorMode::Rgb, 72.0);
        // Backdrop: Red left half [10..50], Blue right half [50..90], tiny yellow square at center (45..55)
        doc.add_object(
            make_rect(10.0, 10.0, 40.0, 80.0, Color::rgba8(220, 40, 40, 255)),
            None,
        )
        .unwrap();
        doc.add_object(
            make_rect(50.0, 10.0, 40.0, 80.0, Color::rgba8(40, 80, 220, 255)),
            None,
        )
        .unwrap();
        doc.add_object(
            make_rect(47.0, 47.0, 6.0, 6.0, Color::rgba8(250, 230, 20, 255)),
            None,
        )
        .unwrap();

        if let Some(cfg) = lens_cfg {
            let mut lens_obj = make_rect(25.0, 25.0, 50.0, 50.0, Color::TRANSPARENT);
            lens_obj.lens = Some(cfg);
            doc.add_object(lens_obj, None).unwrap();
        }
        doc
    };

    let base = render_page_to_rgba(&make_scene(None), 0, 72.0, ViewMode::Enhanced).unwrap();
    let base_red = pixel_at_mm(&base, 35.0, 50.0);
    assert_eq!(base_red, [220, 40, 40, 255]);

    // 1. Brighten
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::Brighten { rate_pct: 50.0 }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let px = pixel_at_mm(&img, 35.0, 50.0);
    assert!(
        px[0] > base_red[0] && px[1] > base_red[1],
        "Brighten failed: {px:?}"
    );

    // 2. ColorAdd
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::ColorAdd {
            color: Color::rgba8(0, 255, 0, 255),
            rate_pct: 100.0,
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let px = pixel_at_mm(&img, 35.0, 50.0);
    assert_eq!(px[0], 220);
    assert_eq!(px[1], 255, "ColorAdd should saturate green channel: {px:?}");

    // 3. ColorLimit
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::ColorLimit {
            color: Color::rgba8(255, 0, 0, 255),
            rate_pct: 100.0,
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let px = pixel_at_mm(&img, 65.0, 50.0); // over blue half (40, 80, 220)
    assert!(
        px[2] < 10,
        "ColorLimit red filter must block blue channel: {px:?}"
    );

    // 4. CustomColorMap (Direct vs ForwardRainbow)
    let img_dir = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::CustomColorMap {
            from_color: Color::rgba8(10, 0, 80, 255),
            to_color: Color::rgba8(255, 240, 30, 255),
            palette_mode: ColorMapMode::Direct,
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let img_rbw = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::CustomColorMap {
            from_color: Color::rgba8(10, 0, 80, 255),
            to_color: Color::rgba8(255, 240, 30, 255),
            palette_mode: ColorMapMode::ForwardRainbow,
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    assert_ne!(pixel_at_mm(&img_dir, 35.0, 50.0), base_red);
    assert_ne!(
        pixel_at_mm(&img_dir, 35.0, 50.0),
        pixel_at_mm(&img_rbw, 35.0, 50.0)
    );

    // 5. FishEye (radial distortion shifts the yellow center outward!)
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::FishEye { rate_pct: 250.0 }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    // Base yellow square was [47..53], so at x=43, y=50 base is red; with strong barrel fisheye,
    // the yellow square magnifies outward significantly!
    let yellow_base_cnt = count_yellow(&base.rgba);
    let yellow_fish_cnt = count_yellow(&img.rgba);
    assert!(
        yellow_fish_cnt > yellow_base_cnt * 2,
        "FishEye barrel must magnify central yellow feature: base={yellow_base_cnt}, fisheye={yellow_fish_cnt}"
    );

    // 6. HeatMap (plus rotation difference)
    let hm0 = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::HeatMap {
            palette_rotation_deg: 0.0,
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let hm180 = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::HeatMap {
            palette_rotation_deg: 180.0,
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    assert_ne!(pixel_at_mm(&hm0, 35.0, 50.0), base_red);
    assert_ne!(
        pixel_at_mm(&hm0, 35.0, 50.0),
        pixel_at_mm(&hm180, 35.0, 50.0)
    );

    // 7. Invert
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::Invert))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let inv_px = pixel_at_mm(&img, 35.0, 50.0);
    assert_eq!(inv_px, [35, 215, 215, 255], "Invert must flip RGB exactly");

    // 8. Magnify (3x expands the 6x6 yellow square to ~18x18 -> ~9x more yellow pixels!)
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::Magnify { amount: 3.0 }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let yellow_mag_cnt = count_yellow(&img.rgba);
    assert!(
        yellow_mag_cnt > yellow_base_cnt * 5,
        "3x Magnify must enlarge central yellow square: base={yellow_base_cnt}, mag={yellow_mag_cnt}"
    );

    // 9. TintedGrayscale
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::TintedGrayscale {
            tint: Color::rgba8(0, 0, 0, 255),
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let px = pixel_at_mm(&img, 35.0, 50.0);
    assert!(
        (i32::from(px[0]) - i32::from(px[1])).abs() <= 2
            && (i32::from(px[1]) - i32::from(px[2])).abs() <= 2,
        "Black-tinted grayscale must neutralize RGB channels: {px:?}"
    );

    // 10. Transparency
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::Transparency {
            color: Color::rgba8(0, 0, 255, 255),
            rate_pct: 50.0,
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let px = pixel_at_mm(&img, 35.0, 50.0);
    assert!(
        px[0] > 90 && px[0] < 130 && px[2] > 120 && px[2] < 170,
        "Transparency blend mismatch: {px:?}"
    );

    // 11. Wireframe
    let img = render_page_to_rgba(
        &make_scene(Some(LensConfig::new(LensKind::Wireframe {
            override_outline: Some(Color::rgba8(0, 180, 0, 255)),
            override_fill: Some(Color::WHITE),
        }))),
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    // Interior of red square inside wireframe lens becomes white fill!
    assert_eq!(pixel_at_mm(&img, 35.0, 50.0), [255, 255, 255, 255]);
    // Boundary edge at x=50 inside lens shows green wireframe stroke!
    let has_green_wire = img
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[1] > 150 && p[0] < 40 && p[2] < 40);
    assert!(
        has_green_wire,
        "Wireframe lens must render green override outline"
    );
}

fn count_yellow(rgba: &[u8]) -> usize {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 220 && p[1] > 200 && p[2] < 60)
        .count()
}

#[test]
fn lens_frozen_viewpoint_and_remove_face_work_accurately() {
    // A. Viewpoint & RemoveFace
    let mut doc = Document::new("LensSpecial", 100.0, 100.0, ColorMode::Rgb, 72.0);
    // Red square only on left [10..40, 20..80]; right side [50..90] is blank white page!
    doc.add_object(
        make_rect(10.0, 20.0, 30.0, 60.0, Color::rgba8(240, 20, 20, 255)),
        None,
    )
    .unwrap();

    // Lens 1: Invert with remove_face = true overlapping BOTH red [25..40] and blank page [40..60]
    let mut rf_lens = make_rect(25.0, 30.0, 35.0, 40.0, Color::TRANSPARENT);
    rf_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: true,

        frozen_snapshot: None,
    });
    doc.add_object(rf_lens, None).unwrap();

    let img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    // Over red square (32, 50): inverted to cyan!
    let over_red = pixel_at_mm(&img, 32.0, 50.0);
    assert!(
        over_red[0] < 40 && over_red[1] > 200 && over_red[2] > 200,
        "expected inverted red: {over_red:?}"
    );
    // Over blank page (50, 50): remove_face MUST leave blank white untouched (NOT inverted to black)!
    let over_blank = pixel_at_mm(&img, 50.0, 50.0);
    assert_eq!(
        over_blank,
        [255, 255, 255, 255],
        "remove_face must exclude blank page background"
    );

    // B. Viewpoint: lens sitting on blank right area [65..85, 30..50] looking at red center (25, 50)
    let mut doc_vp = Document::new("LensVP", 100.0, 100.0, ColorMode::Rgb, 72.0);
    doc_vp
        .add_object(
            make_rect(10.0, 20.0, 30.0, 60.0, Color::rgba8(240, 20, 20, 255)),
            None,
        )
        .unwrap();
    let mut vp_lens = make_rect(65.0, 30.0, 20.0, 20.0, Color::TRANSPARENT);
    vp_lens.lens = Some(LensConfig {
        kind: LensKind::Magnify { amount: 1.0 },
        frozen: false,
        viewpoint: Some(Point2D::new(25.0, 50.0)),
        remove_face: false,

        frozen_snapshot: None,
    });
    doc_vp.add_object(vp_lens, None).unwrap();
    let img_vp = render_page_to_rgba(&doc_vp, 0, 72.0, ViewMode::Enhanced).unwrap();
    let sampled = pixel_at_mm(&img_vp, 75.0, 40.0);
    assert!(
        sampled[0] > 220 && sampled[1] < 40,
        "viewpoint must sample red square: {sampled:?}"
    );

    // C. Frozen snapshot persists when lens is moved over a different color!
    let mut doc_fr = Document::new("LensFrozen", 100.0, 100.0, ColorMode::Rgb, 72.0);
    doc_fr
        .add_object(
            make_rect(10.0, 10.0, 35.0, 80.0, Color::rgba8(240, 20, 20, 255)),
            None,
        )
        .unwrap();
    doc_fr
        .add_object(
            make_rect(55.0, 10.0, 35.0, 80.0, Color::rgba8(20, 20, 240, 255)),
            None,
        )
        .unwrap();
    let mut fr_lens = make_rect(15.0, 30.0, 25.0, 30.0, Color::TRANSPARENT);
    fr_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: true,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let lens_id = doc_fr.add_object(fr_lens, None).unwrap();

    let mut renderer = SceneRenderer::new();
    let vp = Viewport::for_export(283, 283, 72.0);
    let opts = RenderOptions::export();
    let mut b1 = TinySkiaBackend::new(283, 283).unwrap();
    renderer.render_document(&doc_fr, &vp, ViewMode::Enhanced, &opts, &mut b1);
    assert!(renderer.get_frozen_lens(lens_id).is_some());

    // Move frozen lens from red side to blue side (x: 15 -> 60)
    doc_fr
        .set_object_transform(lens_id, Affine2D::translate(60.0, 30.0))
        .unwrap();
    let mut b2 = TinySkiaBackend::new(283, 283).unwrap();
    renderer.render_document(&doc_fr, &vp, ViewMode::Enhanced, &opts, &mut b2);
    let after = b2.into_rgba();
    let px_per_mm = 72.0 / 25.4;
    let moved_px = after
        .get_pixel((72.0 * px_per_mm) as u32, (45.0 * px_per_mm) as u32)
        .unwrap();
    // Frozen inverted red is cyan [~15, ~235, ~235], whereas live inverted blue would be yellow [~235, ~235, ~15]!
    assert!(
        moved_px[0] < 40 && moved_px[1] > 200 && moved_px[2] > 200,
        "Frozen lens should carry cyan snapshot after moving over blue: {moved_px:?}"
    );
}

// =============================================================================
// 3. Mesh Fill, All Fill Types, Effects, PowerClip, BlendModes, Tables, Multipage
// =============================================================================

#[test]
fn mesh_fill_effects_powerclip_and_blendmodes_render_cleanly() {
    // A. MeshFill smooth 4-corner Coons interpolation
    let mut doc = Document::new("Mesh", 80.0, 80.0, ColorMode::Rgb, 72.0);
    let mesh_obj = SceneObject::new(
        ObjectId(0),
        "MeshRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 60.0,
            height: 60.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::MeshFill {
        rows: 2,
        cols: 2,
        nodes: vec![
            MeshNode {
                row: 0,
                col: 0,
                pos: Point2D::new(0.0, 0.0),
                color: Color::rgba8(255, 0, 0, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
            MeshNode {
                row: 0,
                col: 1,
                pos: Point2D::new(60.0, 0.0),
                color: Color::rgba8(0, 255, 0, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
            MeshNode {
                row: 1,
                col: 0,
                pos: Point2D::new(0.0, 60.0),
                color: Color::rgba8(0, 0, 255, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
            MeshNode {
                row: 1,
                col: 1,
                pos: Point2D::new(60.0, 60.0),
                color: Color::rgba8(255, 255, 0, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
        ],
    })
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(10.0, 10.0));
    doc.add_object(mesh_obj, None).unwrap();

    let mesh_img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    let tl = pixel_at_mm(&mesh_img, 12.0, 12.0);
    let tr = pixel_at_mm(&mesh_img, 68.0, 12.0);
    let bl = pixel_at_mm(&mesh_img, 12.0, 68.0);
    let mid = pixel_at_mm(&mesh_img, 40.0, 40.0);
    assert!(
        tl[0] > 230 && tl[1] < 30,
        "TL mesh corner should be red: {tl:?}"
    );
    assert!(
        tr[1] > 230 && tr[0] < 30,
        "TR mesh corner should be green: {tr:?}"
    );
    assert!(
        bl[2] > 230 && bl[0] < 30,
        "BL mesh corner should be blue: {bl:?}"
    );
    assert!(
        mid[0] > 90 && mid[1] > 90 && mid[2] > 30,
        "Center should smoothly blend all 4 corners: {mid:?}"
    );

    // B. DropShadow + BlockShadow + Contour + Extrude25D + Blend + PowerClip + BlendMode::Multiply
    let mut doc2 = Document::new("Effects", 200.0, 140.0, ColorMode::Rgb, 72.0);

    // 1. DropShadow
    let mut ds = make_rect(10.0, 10.0, 25.0, 25.0, Color::rgba8(230, 50, 50, 255));
    ds.effects.push(Effect::DropShadow {
        dx_mm: 6.0,
        dy_mm: 6.0,
        blur_mm: 2.0,
        opacity: 0.75,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    doc2.add_object(ds, None).unwrap();

    // 2. BlockShadow
    let mut bs = make_rect(60.0, 10.0, 25.0, 25.0, Color::rgba8(50, 180, 50, 255));
    bs.effects.push(Effect::BlockShadow {
        dx_mm: 8.0,
        dy_mm: 8.0,
        color: Color::rgba8(20, 20, 20, 255),
        simplify: true,
        overprint: false,
    });
    doc2.add_object(bs, None).unwrap();

    // 3. Outside Contour
    let mut ct = make_rect(115.0, 15.0, 20.0, 20.0, Color::rgba8(255, 220, 0, 255));
    ct.effects.push(Effect::Contour {
        mode: ContourMode::Outside,
        steps: 3,
        offset_mm: 2.5,
        fill_color: Color::rgba8(20, 80, 220, 255),
        outline_color: Color::BLACK,
    });
    doc2.add_object(ct, None).unwrap();

    // 4. Extrude25D
    let mut ext = make_rect(10.0, 65.0, 25.0, 25.0, Color::rgba8(220, 120, 30, 255));
    ext.effects.push(Effect::Extrude25D {
        vanishing_point: Point2D::new(60.0, 50.0),
        depth: 18.0,
        bevel_depth: 1.0,
        shade_color: Color::rgba8(50, 20, 10, 255),
        light_dir: [0.5, -0.5, 0.7],
    });
    doc2.add_object(ext, None).unwrap();

    // 5. Blend between two shapes
    let b_target = doc2
        .add_object(
            make_rect(115.0, 70.0, 16.0, 16.0, Color::rgba8(0, 0, 255, 255)),
            None,
        )
        .unwrap();
    let mut b_src = make_rect(65.0, 70.0, 16.0, 16.0, Color::rgba8(255, 0, 0, 255));
    b_src.effects.push(Effect::Blend {
        target_id: b_target,
        steps: 4,
        rotation_deg: 0.0,
        along_path: None,

        rotate_along_path: true,
    });
    doc2.add_object(b_src, None).unwrap();

    // 6. PowerClip: big blue rect clipped inside small circle at (165, 80) r=12
    let pc_container = doc2
        .add_object(make_circle(165.0, 80.0, 12.0, Color::WHITE), None)
        .unwrap();
    let pc_content = doc2
        .add_object(
            make_rect(140.0, 55.0, 50.0, 50.0, Color::rgba8(10, 40, 230, 255)),
            None,
        )
        .unwrap();
    doc2.create_powerclip(pc_container, &[pc_content]).unwrap();

    // 7. BlendMode::Multiply: Cyan (0,255,255) * Yellow (255,255,0) = Green (0,255,0)!
    doc2.add_object(
        make_rect(10.0, 110.0, 30.0, 20.0, Color::rgba8(0, 255, 255, 255)),
        None,
    )
    .unwrap();
    let mut mul_obj = make_rect(20.0, 110.0, 30.0, 20.0, Color::rgba8(255, 255, 0, 255));
    mul_obj.blend_mode = BlendMode::Multiply;
    doc2.add_object(mul_obj, None).unwrap();

    let img2 = render_page_to_rgba(&doc2, 0, 72.0, ViewMode::Enhanced).unwrap();
    // DropShadow visible at (38, 38) outside rect [10..35, 10..35]
    let ds_px = pixel_at_mm(&img2, 38.0, 38.0);
    assert!(
        ds_px[0] < 200 && ds_px[1] < 200,
        "DropShadow must darken offset area: {ds_px:?}"
    );
    // BlockShadow solid dark at (89, 39)
    let bs_px = pixel_at_mm(&img2, 89.0, 39.0);
    assert!(
        bs_px[0] < 40 && bs_px[1] < 40,
        "BlockShadow must fill swept region: {bs_px:?}"
    );
    // Contour outside visible at (110, 25) (5mm left of [115..135])
    let ct_px = pixel_at_mm(&img2, 110.0, 25.0);
    assert!(
        ct_px[2] > 100 && ct_px[0] < 160,
        "Outside Contour must appear around shape: {ct_px:?}"
    );
    // Extrude25D side visible right of [10..35, 65..90] toward VP (60, 50)
    let ext_px = pixel_at_mm(&img2, 40.0, 75.0);
    assert_ne!(
        ext_px,
        [255, 255, 255, 255],
        "Extrude25D side face must render: {ext_px:?}"
    );
    // Blend midpoint at (98, 78) is purple (mix of red & blue!)
    let blend_px = pixel_at_mm(&img2, 98.0, 78.0);
    assert!(
        blend_px[0] > 60 && blend_px[2] > 60 && blend_px[1] < 40,
        "Blend intermediate must appear: {blend_px:?}"
    );
    // PowerClip: inside circle (165, 80) is blue; outside circle (145, 60) is white!
    let pc_in = pixel_at_mm(&img2, 165.0, 80.0);
    let pc_out = pixel_at_mm(&img2, 145.0, 60.0);
    assert!(
        pc_in[2] > 200 && pc_in[0] < 30,
        "PowerClip interior must show blue content: {pc_in:?}"
    );
    assert_eq!(
        pc_out,
        [255, 255, 255, 255],
        "PowerClip exterior must be clipped clean"
    );
    // Multiply overlap at (30, 120): Green!
    let mul_px = pixel_at_mm(&img2, 30.0, 120.0);
    assert!(
        mul_px[0] < 10 && mul_px[1] > 240 && mul_px[2] < 10,
        "Cyan*Yellow Multiply must be green: {mul_px:?}"
    );
}

#[test]
fn multipage_master_layers_tables_dimensions_connectors_and_all_fills() {
    let mut doc = Document::new("Multi", 100.0, 100.0, ColorMode::Rgb, 72.0);
    let p1 = doc.pages()[0].id;
    let _p2 = doc.add_page("Page 2", 100.0, 100.0, 3.0);
    let _p3 = doc.add_page("Page 3", 100.0, 100.0, 3.0);

    let odd_layer = doc
        .add_layer(p1, "OddMaster", false, MasterLayerScope::OddPages)
        .unwrap();
    let even_layer = doc
        .add_layer(p1, "EvenMaster", false, MasterLayerScope::EvenPages)
        .unwrap();
    let all_layer = doc
        .add_layer(p1, "AllMaster", false, MasterLayerScope::AllPages)
        .unwrap();

    doc.add_object(
        make_rect(5.0, 5.0, 15.0, 15.0, Color::rgba8(220, 0, 0, 255)),
        Some(odd_layer),
    )
    .unwrap();
    doc.add_object(
        make_rect(25.0, 5.0, 15.0, 15.0, Color::rgba8(0, 200, 0, 255)),
        Some(even_layer),
    )
    .unwrap();
    doc.add_object(
        make_rect(45.0, 5.0, 15.0, 15.0, Color::rgba8(0, 0, 220, 255)),
        Some(all_layer),
    )
    .unwrap();

    // Verify MasterLayerScope across pages 0 (Page 1, odd), 1 (Page 2, even), 2 (Page 3, odd)
    let img0 = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    let img1 = render_page_to_rgba(&doc, 1, 72.0, ViewMode::Enhanced).unwrap();
    let img2 = render_page_to_rgba(&doc, 2, 72.0, ViewMode::Enhanced).unwrap();

    assert_eq!(pixel_at_mm(&img0, 12.0, 12.0), [220, 0, 0, 255]); // Odd visible on P1
    assert_eq!(pixel_at_mm(&img0, 32.0, 12.0), [255, 255, 255, 255]); // Even hidden on P1
    assert_eq!(pixel_at_mm(&img0, 52.0, 12.0), [0, 0, 220, 255]); // All visible on P1

    assert_eq!(pixel_at_mm(&img1, 12.0, 12.0), [255, 255, 255, 255]); // Odd hidden on P2
    assert_eq!(pixel_at_mm(&img1, 32.0, 12.0), [0, 200, 0, 255]); // Even visible on P2
    assert_eq!(pixel_at_mm(&img1, 52.0, 12.0), [0, 0, 220, 255]); // All visible on P2

    assert_eq!(pixel_at_mm(&img2, 12.0, 12.0), [220, 0, 0, 255]); // Odd visible on P3
    assert_eq!(pixel_at_mm(&img2, 32.0, 12.0), [255, 255, 255, 255]); // Even hidden on P3

    // Verify Multipage View layouts
    let layouts = compute_page_layouts(&doc, true, 20.0);
    assert_eq!(layouts.len(), 3);
    assert!((layouts[1].origin_mm.x - 120.0).abs() < 1e-6);
    assert!((layouts[2].origin_mm.x - 240.0).abs() < 1e-6);

    // Table, Dimension, Connector, SymbolInstance, Conical/Square/Pattern/Texture fills
    let mut doc_rich = Document::new("Rich", 160.0, 120.0, ColorMode::Rgb, 72.0);
    let sym = SymbolDef::new(
        "DotSym",
        vec![make_circle(5.0, 5.0, 3.0, Color::rgba8(200, 40, 120, 255))],
    );
    let sym_id = sym.id;
    doc_rich.insert_symbol(sym);

    let fills = vec![
        Fill::ConicalGradient {
            center: Point2D::new(10.0, 10.0),
            angle_deg: 0.0,
            stops: vec![
                GradientStop::new(0.0, Color::rgba8(255, 0, 0, 255)),
                GradientStop::new(0.5, Color::rgba8(0, 255, 0, 255)),
                GradientStop::new(1.0, Color::rgba8(0, 0, 255, 255)),
            ],
        },
        Fill::SquareGradient {
            center: Point2D::new(10.0, 10.0),
            size: 20.0,
            angle_deg: 25.0,
            stops: vec![
                GradientStop::new(0.0, Color::rgba8(255, 255, 0, 255)),
                GradientStop::new(1.0, Color::rgba8(120, 0, 200, 255)),
            ],
        },
        Fill::TwoColorPattern {
            fg: Color::BLACK,
            bg: Color::WHITE,
            pattern_name: "checker".into(),
            tile_w: 5.0,
            tile_h: 5.0,
        },
        Fill::VectorPattern {
            symbol_id: sym_id,
            tile_w: 10.0,
            tile_h: 10.0,
        },
        Fill::TextureFill {
            preset: "marble".into(),
            seed: 42,
            fg: Color::rgba8(30, 60, 180, 255),
            bg: Color::rgba8(220, 230, 250, 255),
        },
    ];
    for (i, fill) in fills.into_iter().enumerate() {
        let mut obj = make_rect(5.0 + (i as f64) * 28.0, 5.0, 20.0, 20.0, Color::WHITE);
        obj.fill = fill;
        doc_rich.add_object(obj, None).unwrap();
    }

    let mut tbl = VectorTable::new(2, 2, 18.0, 10.0);
    tbl.cell_mut(0, 0).unwrap().text = "A1".into();
    tbl.cell_mut(0, 0).unwrap().fill = Fill::Solid(Color::rgba8(200, 230, 255, 255));
    let tbl_id = doc_rich
        .add_object(
            SceneObject::new(ObjectId(0), "Table", NodePayload::Table(tbl))
                .with_transform(Affine2D::translate(10.0, 40.0)),
            None,
        )
        .unwrap();

    let dim = DimensionObject::linear(
        DimensionKind::Horizontal,
        Point2D::new(10.0, 80.0),
        Point2D::new(60.0, 80.0),
        8.0,
    );
    doc_rich
        .add_object(
            SceneObject::new(ObjectId(0), "Dim", NodePayload::Dimension(dim)),
            None,
        )
        .unwrap();

    let box2 = doc_rich
        .add_object(
            make_rect(90.0, 45.0, 20.0, 15.0, Color::rgba8(40, 160, 80, 255)),
            None,
        )
        .unwrap();
    let mut conn = SceneObject::new(
        ObjectId(0),
        "Conn",
        NodePayload::Connector(ConnectorObject::between_objects(
            tbl_id,
            AnchorPoint::Right,
            box2,
            AnchorPoint::Left,
            ConnectorStyle::RoundedRightAngle,
        )),
    );
    conn.outline = OutlineStyle {
        end_arrow: ArrowheadStyle::Triangle,
        ..OutlineStyle::solid(Color::BLACK, 0.8)
    };
    doc_rich.add_object(conn, None).unwrap();

    let rich_img = render_page_to_rgba(&doc_rich, 0, 72.0, ViewMode::Enhanced).unwrap();
    assert!(count_non_white_pixels(&rich_img.rgba) > 4000);
    // Verify PNG encoding succeeds
    let png = rich_img.encode_png().unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
}

// =============================================================================
// 4. Arabic RTL Text + Kashida + Fit Text to Path + text_to_curves
// =============================================================================

#[test]
fn arabic_rtl_kashida_fit_text_to_path_and_text_to_curves() {
    let arabic_str = "مرحبا بالعالم";

    // Kashida elongation inserts tatweel only at valid dual-joining sites
    let elongated = apply_kashida_elongation(arabic_str, 2);
    assert!(
        elongated.contains('\u{0640}'),
        "Kashida elongation must insert Arabic tatweel U+0640: {elongated}"
    );
    // Must never break Lam-Alef (`لا`)
    let lam_alef = apply_kashida_elongation("سلام", 2);
    assert!(
        !lam_alef.contains("ل\u{0640}ا"),
        "Kashida must never split لا ligature: {lam_alef}"
    );

    let normal_text = ArtisticText::from_plain(
        arabic_str,
        TextStyle {
            font_size_pt: 28.0,
            direction: TextDirection::Rtl,
            align: TextAlign::Right,
            fill: Color::BLACK,
            ..TextStyle::default()
        },
    );
    let kashida_text = ArtisticText::from_plain(
        arabic_str,
        TextStyle {
            font_size_pt: 28.0,
            direction: TextDirection::Rtl,
            align: TextAlign::ForceJustifyKashida,
            kashida_stretch: 1.0,
            fill: Color::BLACK,
            ..TextStyle::default()
        },
    );

    // Text-to-Curves returns valid non-empty vector Bézier curves
    let normal_curves = text_to_curves(&normal_text);
    let kashida_curves = text_to_curves(&kashida_text);
    assert!(
        !normal_curves.is_empty(),
        "Arabic text_to_curves must produce vector subpaths"
    );
    assert!(normal_curves.is_finite());
    assert!(
        normal_curves.area_mm2() > 1.0,
        "Arabic glyph curves must have positive enclosed area"
    );
    let normal_w = normal_curves.bounding_box().unwrap().width();
    let kashida_w = kashida_curves.bounding_box().unwrap().width();
    assert!(
        kashida_w > normal_w,
        "Kashida-justified Arabic text should be wider than normal: normal={normal_w:.2}mm, kashida={kashida_w:.2}mm"
    );

    // Fit Text to Path along a curved semicircular arc
    let host_arc = ParametricShape::Ellipse {
        rx: 35.0,
        ry: 35.0,
        mode: EllipseMode::Arc,
        start_deg: 200.0,
        end_deg: 340.0,
    }
    .to_bezier_path()
    .transformed(&Affine2D::translate(55.0, 55.0));

    let top = TextOnPath {
        text: normal_text.clone(),
        path_id: ObjectId(1),
        offset_mm: 5.0,
        distance_mm: 2.0,
        flip_vertical: false,
        flip_horizontal: false,
    };
    let on_path_curves = text_on_path_to_curves(&top, &host_arc);
    assert!(
        !on_path_curves.is_empty() && on_path_curves.area_mm2() > 1.0,
        "Arabic Fit Text to Path must produce valid vector curves"
    );

    // Render Arabic ArtisticText + Multi-column ParagraphText + TextOnPath onto a page
    let mut doc = Document::new("ArabicPage", 140.0, 120.0, ColorMode::Rgb, 96.0);
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "ArabicTitle",
            NodePayload::ArtisticText(kashida_text),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(10.0, 10.0)),
        None,
    )
    .unwrap();

    let mut para = ParagraphText::new(
        "مرحبا بالعالم في استوديو التصميم المتجهي الحديث للتصميم والطباعة الاحترافية",
        Vec2D::new(110.0, 35.0),
        TextStyle {
            font_size_pt: 14.0,
            direction: TextDirection::Rtl,
            align: TextAlign::ForceJustifyKashida,
            fill: Color::rgba8(20, 40, 100, 255),
            ..TextStyle::default()
        },
    );
    para.columns = 2;
    doc.add_object(
        SceneObject::new(ObjectId(0), "ArabicPara", NodePayload::ParagraphText(para))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(15.0, 35.0)),
        None,
    )
    .unwrap();

    let arc_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ArcPath", NodePayload::Path(host_arc))
                .with_outline(OutlineStyle::solid(Color::gray(0.7), 0.3)),
            None,
        )
        .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "ArabicOnPath",
            NodePayload::TextOnPath(TextOnPath {
                text: normal_text,
                path_id: arc_id,
                offset_mm: 4.0,
                distance_mm: 2.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        )
        .with_outline(OutlineStyle::none()),
        None,
    )
    .unwrap();

    let rendered = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let ink_pixels = count_non_white_pixels(&rendered.rgba);
    assert!(
        ink_pixels > 500,
        "Arabic RTL/Kashida/Paragraph/TextOnPath must render rich visible pixels, got {ink_pixels}"
    );
}

// =============================================================================
// 5. Raster Studio: Brush, Eraser, Clone Stamp, Smudge, Masks, All 19 Filters
// =============================================================================

#[test]
fn raster_studio_brushes_masks_all_19_filters_and_vector_to_bitmap() {
    let mut asset =
        RasterAsset::new(64, 64, vec![255u8; 64 * 64 * 4], 96.0, ColorMode::Rgb).unwrap();

    // 1. Brush stroke paints red band across y=16
    paint_brush_stroke(
        &mut asset,
        &[Point2D::new(8.0, 16.0), Point2D::new(56.0, 16.0)],
        6.0,
        0.9,
        Color::rgba8(240, 20, 20, 255),
        1.0,
    );
    let idx = (16 * 64 + 32) * 4;
    assert!(asset.to_rgba8_vec()[idx] > 220 && asset.to_rgba8_vec()[idx + 1] < 40);

    // 2. Clone stamp copies red band down to y=44
    clone_stamp_stroke(
        &mut asset,
        Point2D::new(32.0, 16.0),
        &[Point2D::new(32.0, 44.0)],
        5.0,
        0.9,
    );
    let clone_idx = (44 * 64 + 32) * 4;
    assert!(asset.to_rgba8_vec()[clone_idx] > 200 && asset.to_rgba8_vec()[clone_idx + 1] < 50);

    // 3. Smudge pulls red from y=16 downward toward y=24
    smudge_stroke(
        &mut asset,
        &[Point2D::new(20.0, 16.0), Point2D::new(20.0, 24.0)],
        4.0,
        0.85,
    );
    let smudge_idx = (23 * 64 + 20) * 4;
    assert!(
        asset.to_rgba8_vec()[smudge_idx + 1] < 245,
        "Smudge must drag red pigment downward"
    );

    // 4. Erase reduces alpha to 0 at (48, 16)
    erase_stroke(&mut asset, &[Point2D::new(48.0, 16.0)], 4.0, 1.0);
    let erase_idx = (16 * 64 + 48) * 4;
    assert_eq!(asset.to_rgba8_vec()[erase_idx + 3], 0);

    // 5. Masks: Rectangle, Lasso, Magic Wand + Cutout background removal
    let rect_mask = RasterMask::from_rectangle(64, 64, Rect2D::from_xywh(10.0, 10.0, 20.0, 20.0));
    assert_eq!(rect_mask.selected_pixel_count(), 400);

    let lasso = RasterMask::from_lasso(
        64,
        64,
        &[
            Point2D::new(10.0, 10.0),
            Point2D::new(30.0, 10.0),
            Point2D::new(30.0, 30.0),
            Point2D::new(10.0, 30.0),
        ],
    );
    assert!(lasso.selected_pixel_count() >= 380);

    // Magic Wand on white corner selects white background, excluding painted red stripe
    let mut wand_asset =
        RasterAsset::new(32, 32, vec![255u8; 32 * 32 * 4], 96.0, ColorMode::Rgb).unwrap();
    paint_brush_stroke(
        &mut wand_asset,
        &[Point2D::new(16.0, 16.0)],
        6.0,
        1.0,
        Color::rgba8(20, 40, 220, 255),
        1.0,
    );
    let mut bg_mask = RasterMask::from_magic_wand(&wand_asset, 0, 0, 20.0, true);
    assert_eq!(bg_mask.get(0, 0), 255);
    assert_eq!(bg_mask.get(16, 16), 0);
    bg_mask.invert(); // now foreground mask!
    apply_cutout_background_removal(&mut wand_asset, &bg_mask, 1.0);
    assert_eq!(
        wand_asset.to_rgba8_vec()[3],
        0,
        "Cutout must remove background pixel (0,0)"
    );
    assert!(
        wand_asset.to_rgba8_vec()[(16 * 32 + 16) * 4 + 3] > 200,
        "Cutout must keep foreground"
    );

    // 6. Test ALL 19 RasterFilter implementations
    let mut test_img =
        RasterAsset::new(32, 32, vec![0u8; 32 * 32 * 4], 96.0, ColorMode::Rgb).unwrap();
    for y in 0..32u32 {
        for x in 0..32u32 {
            let idx = ((y * 32 + x) * 4) as usize;
            let px = if x < 16 {
                [200, 60, 40, 255]
            } else {
                [40, 120, 220, 255]
            };
            test_img.tiles[0].make_mut()[idx..idx + 4].copy_from_slice(&px);
        }
    }

    // Exact assertions on key filters
    let inv = apply_raster_filter(&test_img, &RasterFilter::Invert);
    assert_eq!(&inv.to_rgba8_vec()[0..4], &[55, 195, 215, 255]);

    let blurred = apply_raster_filter(&test_img, &RasterFilter::GaussianBlur { radius_px: 3.0 });
    // Boundary column x=15 blends red (200) toward blue (40)
    let b_idx = (16 * 32 + 15) * 4;
    assert!(
        blurred.to_rgba8_vec()[b_idx] < 180 && blurred.to_rgba8_vec()[b_idx] > 80,
        "GaussianBlur must smooth sharp step edge: {:?}",
        &blurred.to_rgba8_vec()[b_idx..b_idx + 4]
    );

    let hsl = apply_raster_filter(
        &test_img,
        &RasterFilter::Hsl {
            hue_deg: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        },
    );
    assert!(
        hsl.to_rgba8_vec()[1] > hsl.to_rgba8_vec()[0],
        "HSL +120° hue shift should shift red into green: {:?}",
        &hsl.to_rgba8_vec()[0..4]
    );

    let curve = apply_raster_filter(
        &test_img,
        &RasterFilter::ToneCurve {
            points: vec![(0.0, 1.0), (1.0, 0.0)],
        },
    );
    assert!(
        curve.to_rgba8_vec()[0] < 80 && curve.to_rgba8_vec()[2] > 180,
        "Inverting ToneCurve failed"
    );

    // Smoke-verify all remaining filters produce valid altered buffers
    let all_other_filters = [
        RasterFilter::BrightnessContrast {
            brightness: 0.2,
            contrast: 0.3,
        },
        RasterFilter::Levels {
            in_black: 0.1,
            in_gamma: 1.2,
            in_white: 0.9,
            out_black: 0.0,
            out_white: 1.0,
        },
        RasterFilter::ColorBalance {
            cyan_red: 0.3,
            magenta_green: -0.2,
            yellow_blue: 0.2,
        },
        RasterFilter::ReplaceColor {
            target: Color::rgba8(200, 60, 40, 255),
            replacement: Color::rgba8(20, 240, 20, 255),
            tolerance: 0.25,
        },
        RasterFilter::Gamma { gamma: 2.2 },
        RasterFilter::Desaturate,
        RasterFilter::Posterize { levels: 3 },
        RasterFilter::Threshold { cutoff: 110 },
        RasterFilter::MotionBlur {
            distance_px: 6.0,
            angle_deg: 0.0,
        },
        RasterFilter::UnsharpMask {
            amount: 1.2,
            radius_px: 2.0,
            threshold: 4,
        },
        RasterFilter::Sharpen { amount: 1.0 },
        RasterFilter::Noise {
            amount: 0.15,
            monochrome: false,
            seed: 7,
        },
        RasterFilter::Vignette {
            strength: 0.7,
            radius: 0.4,
            feather: 0.5,
        },
        RasterFilter::Emboss {
            angle_deg: 45.0,
            depth: 1.5,
        },
        RasterFilter::EdgeDetect { sensitivity: 1.5 },
    ];
    for f in &all_other_filters {
        let out = apply_raster_filter(&test_img, f);
        assert_eq!(out.to_rgba8_vec().len(), test_img.to_rgba8_vec().len());
        assert_ne!(
            out.to_rgba8_vec(),
            test_img.to_rgba8_vec(),
            "filter {f:?} had no effect"
        );
    }

    // 7. Resample & convert_object_to_bitmap
    let resized = resample(&test_img, 16, 16).unwrap();
    assert_eq!((resized.width, resized.height), (16, 16));

    let mut doc = Document::new("Vec2Bmp", 100.0, 100.0, ColorMode::Rgb, 150.0);
    let obj_id = doc
        .add_object(
            make_circle(40.0, 40.0, 15.0, Color::rgba8(20, 180, 90, 255)),
            None,
        )
        .unwrap();
    let bmp = convert_object_to_bitmap(&doc, obj_id, 150.0).unwrap();
    assert!(bmp.width > 100 && bmp.height > 100);
    let green_px = bmp
        .to_rgba8_vec()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 200 && p[1] > 150)
        .count();
    assert!(green_px > 5000);
}

// =============================================================================
// 6. Bitmap Vector Tracing: OutlineTrace & CenterlineTrace
// =============================================================================

#[test]
fn vector_trace_outline_and_centerline_vectorize_synthetic_shapes() {
    // A. OutlineTrace: 80x80 white canvas with a red square [10..34, 15..39] & blue circle at (58, 52) r=14
    let w = 80u32;
    let h = 80u32;
    let mut buf = vec![255u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            if (10..34).contains(&x) && (15..39).contains(&y) {
                buf[idx..idx + 4].copy_from_slice(&[230, 20, 20, 255]);
            } else {
                let dx = f64::from(x) - 58.0;
                let dy = f64::from(y) - 52.0;
                if dx.hypot(dy) <= 14.0 {
                    buf[idx..idx + 4].copy_from_slice(&[20, 40, 230, 255]);
                }
            }
        }
    }
    let asset = RasterAsset::new(w, h, buf, 96.0, ColorMode::Rgb).unwrap();
    let traced = trace_raster_to_mm(
        &asset,
        80.0,
        80.0,
        &TraceConfig {
            mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
            max_colors: 4,
            smoothing: 0.4,
            detail: 0.85,
            remove_background: true,
            corner_threshold: 55.0,
            speckle_size: 0,
        },
    );
    assert_eq!(
        traced.len(),
        2,
        "Should trace exactly 2 non-background color layers (red & blue)"
    );
    for (path, _col) in &traced {
        assert!(!path.is_empty());
        assert!(path.subpaths.iter().all(|sp| sp.closed));
        assert!(
            path.area_mm2() > 350.0,
            "Traced shape area too small: {}",
            path.area_mm2()
        );
    }

    // B. CenterlineTrace (Zhang-Suen skeletonization): thick black L-stroke on white canvas
    let mut line_buf = vec![255u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            // Horizontal bar y in [18..26], x in [12..64] + vertical leg x in [12..20], y in [18..66]
            let on_h = (18..26).contains(&y) && (12..64).contains(&x);
            let on_v = (12..20).contains(&x) && (18..66).contains(&y);
            if on_h || on_v {
                line_buf[idx..idx + 4].copy_from_slice(&[10, 10, 10, 255]);
            }
        }
    }
    let line_asset = RasterAsset::new(w, h, line_buf, 96.0, ColorMode::Rgb).unwrap();
    let centerlines = trace_raster(
        &line_asset,
        &TraceConfig {
            mode: TraceMode::CenterlineTrace,
            max_colors: 2,
            smoothing: 0.3,
            detail: 0.85,
            remove_background: true,
            corner_threshold: 50.0,
            speckle_size: 0,
        },
    );
    assert!(
        !centerlines.is_empty(),
        "CenterlineTrace must produce skeleton paths"
    );
    let (cl_path, _) = &centerlines[0];
    assert!(
        cl_path.subpaths.iter().any(|sp| !sp.closed),
        "CenterlineTrace of open L-shape must yield open stroke subpath(s)"
    );
    assert!(cl_path.path_length_mm() > 10.0);
}

// =============================================================================
// 7. Smart Fill on Overlapping Rectangles & Circles
// =============================================================================

#[test]
fn smart_fill_extracts_exact_enclosed_overlap_region() {
    // Two overlapping rectangles: A=[10..50, 10..50], B=[30..70, 20..60]
    // Exact overlap = [30..50, 20..50] -> width=20, height=30, area=600 mm²
    let mut doc = Document::new("SmartFill", 100.0, 100.0, ColorMode::Rgb, 96.0);
    doc.add_object(make_rect(10.0, 10.0, 40.0, 40.0, Color::TRANSPARENT), None)
        .unwrap();
    doc.add_object(make_rect(30.0, 20.0, 40.0, 40.0, Color::TRANSPARENT), None)
        .unwrap();

    let overlap = compute_smart_fill(
        &doc,
        0,
        Point2D::new(40.0, 35.0),
        Fill::Solid(Color::CYAN),
        OutlineStyle::none(),
    )
    .expect("click inside overlap must produce enclosed vector path");

    let bb = overlap.bounding_box().unwrap();
    assert!((bb.min_x - 30.0).abs() < 0.8, "min_x={}", bb.min_x);
    assert!((bb.max_x - 50.0).abs() < 0.8, "max_x={}", bb.max_x);
    assert!((bb.min_y - 20.0).abs() < 0.8, "min_y={}", bb.min_y);
    assert!((bb.max_y - 50.0).abs() < 0.8, "max_y={}", bb.max_y);
    assert!(
        (overlap.area_mm2() - 600.0).abs() < 35.0,
        "expected overlap area ~600 mm², got {:.1}",
        overlap.area_mm2()
    );

    // Click in open exterior space returns None!
    assert!(compute_smart_fill(
        &doc,
        0,
        Point2D::new(3.0, 3.0),
        Fill::Solid(Color::CYAN),
        OutlineStyle::none()
    )
    .is_none());

    // Two overlapping circles of radius 20 centered at (30, 50) and (50, 50)
    // Exact analytical lens area = 2*r²*acos(d/(2r)) - (d/2)*sqrt(4r² - d²) for r=20, d=20 ≈ 491.3 mm²
    let mut doc_c = Document::new("SmartFillCircles", 100.0, 100.0, ColorMode::Rgb, 96.0);
    doc_c
        .add_object(make_circle(30.0, 50.0, 20.0, Color::TRANSPARENT), None)
        .unwrap();
    doc_c
        .add_object(make_circle(50.0, 50.0, 20.0, Color::TRANSPARENT), None)
        .unwrap();
    let lens = compute_smart_fill(
        &doc_c,
        0,
        Point2D::new(40.0, 50.0),
        Fill::Solid(Color::MAGENTA),
        OutlineStyle::none(),
    )
    .expect("click in circle overlap lens must produce closed BezierPath");
    assert!(
        (lens.area_mm2() - 491.3).abs() < 35.0,
        "expected circle lens area ~491 mm², got {:.1}",
        lens.area_mm2()
    );
}

// =============================================================================
// 8. Spatial Hit-Testing, SnapEngine with Dynamic Guides, and SVG Helper
// =============================================================================

#[test]
fn hit_test_and_snap_engine_and_svg_render() {
    let mut doc = Document::new("SnapHit", 200.0, 200.0, ColorMode::Rgb, 96.0);
    doc.set_grid(coree_core::GridConfig {
        visible: true,
        spacing_x_mm: 10.0,
        spacing_y_mm: 10.0,
        subdivisions: 1,
        ..doc.grid()
    });
    doc.add_guide(GuideOrientation::Vertical(75.0), false);
    doc.add_guide(GuideOrientation::Horizontal(120.0), false);

    let back_id = doc
        .add_object(make_rect(20.0, 20.0, 40.0, 30.0, Color::CYAN), None)
        .unwrap();
    let front_id = doc
        .add_object(make_rect(40.0, 30.0, 40.0, 30.0, Color::MAGENTA), None)
        .unwrap();

    // Hit-test point returns frontmost object on overlap
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(50.0, 40.0), 1.0),
        Some(front_id)
    );
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(25.0, 25.0), 1.0),
        Some(back_id)
    );
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(150.0, 150.0), 1.0),
        None
    );

    // Hit-test rect enclosed vs intersecting
    let partial = hit_test_rect(&doc, 0, Rect2D::from_xywh(15.0, 15.0, 50.0, 40.0), false);
    assert_eq!(partial.len(), 2);
    let enclosed = hit_test_rect(&doc, 0, Rect2D::from_xywh(15.0, 15.0, 50.0, 40.0), true);
    assert_eq!(enclosed, vec![back_id]);

    // Hit-test node
    let nh = hit_test_node(&doc, &[back_id], Point2D::new(20.5, 20.4), 2.0).unwrap();
    assert_eq!(nh.node.object_id, back_id);
    assert_eq!(nh.position_mm, Point2D::new(20.0, 20.0));

    // SnapEngine: snap to grid
    let (snapped_grid, g_grid) = SnapEngine::snap_point(
        &doc,
        0,
        Point2D::new(131.2, 148.9),
        &[],
        &SnapOptions {
            grid: true,
            guidelines: false,
            page_edges: false,
            objects: false,
            nodes: false,
            dynamic_guides: true,
            tolerance_mm: 3.0,
            ..SnapOptions::default()
        },
    );
    assert!((snapped_grid.x - 130.0).abs() < 1e-6);
    assert!((snapped_grid.y - 150.0).abs() < 1e-6);
    assert!(g_grid.iter().any(|g| g.kind == DynamicGuideKind::Grid));

    // SnapEngine: snap to guideline
    let (snapped_guide, g_guide) = SnapEngine::snap_point(
        &doc,
        0,
        Point2D::new(76.0, 119.2),
        &[],
        &SnapOptions {
            grid: false,
            guidelines: true,
            page_edges: false,
            objects: false,
            nodes: false,
            dynamic_guides: true,
            tolerance_mm: 2.5,
            ..SnapOptions::default()
        },
    );
    assert!((snapped_guide.x - 75.0).abs() < 1e-6);
    assert!((snapped_guide.y - 120.0).abs() < 1e-6);
    assert!(g_guide
        .iter()
        .any(|g| g.kind == DynamicGuideKind::GuideLine));

    // SnapEngine: snap to object edge with dynamic alignment guide!
    let (snapped_obj, g_obj) = SnapEngine::snap_point(
        &doc,
        0,
        Point2D::new(60.8, 100.0), // near right edge x=60 of back_id
        &[front_id],
        &SnapOptions {
            grid: false,
            guidelines: false,
            page_edges: false,
            objects: true,
            nodes: false,
            dynamic_guides: true,
            tolerance_mm: 2.5,
            ..SnapOptions::default()
        },
    );
    assert!((snapped_obj.x - 60.0).abs() < 1e-6);
    assert!(g_obj.iter().any(|g| g.kind == DynamicGuideKind::AlignmentX));

    // Raster layer with non-destructive filter renders through SceneRenderer
    let mut doc_r = Document::new("RasterNode", 60.0, 60.0, ColorMode::Rgb, 72.0);
    let mut raw = vec![0u8; 16 * 16 * 4];
    for px in raw.as_chunks_mut::<4>().0 {
        *px = [255, 0, 0, 255];
    }
    let asset_id =
        doc_r.register_asset(RasterAsset::new(16, 16, raw, 72.0, ColorMode::Rgb).unwrap());
    doc_r
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Photo",
                NodePayload::Raster(RasterLayerData {
                    asset_id,
                    width_mm: 40.0,
                    height_mm: 40.0,
                    filters: vec![RasterFilter::Invert],
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    let r_img = render_page_to_rgba_with_options(
        &doc_r,
        0,
        72.0,
        ViewMode::Enhanced,
        &RenderOptions::export(),
    )
    .unwrap();
    let r_px = pixel_at_mm(&r_img, 30.0, 30.0);
    assert_eq!(
        r_px,
        [0, 255, 255, 255],
        "Non-destructive RasterFilter::Invert must apply at render time"
    );

    // SVG preview rasterizer smoke check
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect x="5" y="5" width="30" height="30" fill="#00aa55"/></svg>"##;
    let svg_buf: RgbaBuffer = render_svg_to_rgba(svg, 40, 40).unwrap();
    assert_eq!(svg_buf.get_pixel(20, 20), Some([0, 170, 85, 255]));
}

#[test]
fn distort_envelope_perspective_symmetry_fountain_transparency_and_outline_styles() {
    let mut doc = Document::new("ExtraEffects", 180.0, 120.0, ColorMode::Rgb, 72.0);

    // 1. Distort (Twister)
    let mut dist = make_rect(10.0, 10.0, 30.0, 30.0, Color::rgba8(200, 60, 60, 255));
    dist.effects.push(Effect::Distort {
        kind: DistortKind::Twister,
        amplitude: 45.0,
        frequency: 1.5,
        center: Point2D::new(15.0, 15.0),
    });
    doc.add_object(dist, None).unwrap();

    // 2. Envelope (SingleArc)
    let mut env = make_rect(55.0, 10.0, 30.0, 30.0, Color::rgba8(60, 180, 90, 255));
    env.effects.push(Effect::Envelope {
        mode: EnvelopeMode::SingleArc,
        cage: vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(30.0, 4.0),
            Point2D::new(30.0, 26.0),
            Point2D::new(0.0, 30.0),
        ],
    });
    doc.add_object(env, None).unwrap();

    // 3. Perspective homography warp
    let mut persp = make_rect(100.0, 10.0, 30.0, 30.0, Color::rgba8(50, 90, 220, 255));
    persp.effects.push(Effect::Perspective {
        quad: [
            Point2D::new(4.0, 0.0),
            Point2D::new(26.0, 0.0),
            Point2D::new(30.0, 30.0),
            Point2D::new(0.0, 30.0),
        ],
    });
    doc.add_object(persp, None).unwrap();

    // 4. Symmetry (4-fold around (155, 25))
    let mut sym = make_rect(155.0, 10.0, 10.0, 10.0, Color::rgba8(180, 40, 200, 255));
    sym.effects.push(Effect::Symmetry {
        axes: 4,
        mirror: true,
        origin: Point2D::new(155.0, 25.0),
    });
    doc.add_object(sym, None).unwrap();

    // 5. FountainLinear Transparency (fades from 1.0 on local left to 0.0 on local right)
    let mut trans = make_rect(10.0, 60.0, 50.0, 30.0, Color::rgba8(220, 20, 20, 255));
    trans.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::new(0.0, 15.0),
            end: Point2D::new(50.0, 15.0),
            start_alpha: 1.0,
            end_alpha: 0.0,
        },
        blend_mode: BlendMode::Normal,
    });
    doc.add_object(trans, None).unwrap();

    // 6. Outline options: behind_fill + dashed + calligraphy + arrowheads + scale_with_object
    let mut line_obj = SceneObject::new(
        ObjectId(0),
        "CalligLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(80.0, 75.0),
            Point2D::new(160.0, 75.0),
        )),
    );
    line_obj.outline = OutlineStyle {
        color: Color::rgba8(20, 20, 20, 255),
        width_mm: 3.0,
        is_hairline: false,
        dash_pattern: vec![4.0, 2.0],
        start_arrow: ArrowheadStyle::Diamond,
        end_arrow: ArrowheadStyle::Stealth,
        behind_fill: true,
        scale_with_object: true,
        calligraphy_angle_deg: 35.0,
        calligraphy_stretch: 0.35,
        ..OutlineStyle::none()
    };
    doc.add_object(line_obj, None).unwrap();

    let img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    // Fountain transparency fades from red on left (14, 75) to near-white on right (57, 75)
    let left_px = pixel_at_mm(&img, 14.0, 75.0);
    let right_px = pixel_at_mm(&img, 57.0, 75.0);
    assert!(
        left_px[1] < 80 && right_px[1] > 210,
        "FountainLinear transparency must fade left->right: left={left_px:?}, right={right_px:?}"
    );
    // Symmetry replicated purple square into opposite quadrant (148, 32)
    let sym_px = pixel_at_mm(&img, 148.0, 32.0);
    assert!(
        sym_px[0] > 140 && sym_px[2] > 160,
        "Symmetry replica missing: {sym_px:?}"
    );

    // Wireframe and Pixels view modes render cleanly
    let wire = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Wireframe).unwrap();
    let pix = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Pixels).unwrap();
    assert!(count_non_white_pixels(&wire.rgba) > 100);
    assert!(count_non_white_pixels(&pix.rgba) > 1000);

    // Direct StrokeParams test on CanvasBackend
    let mut b = TinySkiaBackend::new(64, 64).unwrap();
    b.clear(Color::WHITE);
    let st = StrokeParams::from_outline(
        &OutlineStyle::solid(Color::BLACK, 2.0),
        2.0,
        1.0,
        BlendMode::Normal,
        true,
    );
    b.stroke_path(
        &BezierPath::from_line(Point2D::new(4.0, 32.0), Point2D::new(60.0, 32.0)),
        &st,
        Affine2D::IDENTITY,
    );
    assert_eq!(b.into_rgba().get_pixel(32, 32), Some([0, 0, 0, 255]));
}

#[test]
fn edge_cases_nan_safety_viewport_math_and_remaining_variants() {
    // 1. Dimension & error guardrails
    assert!(TinySkiaBackend::new(0, 100).is_err());
    assert!(TinySkiaBackend::new(20_000, 100).is_err());
    assert!(TinySkiaBackend::new(10_000, 10_000).is_err()); // exceeds MAX_SURFACE_PIXELS

    let doc = Document::new("Empty", 50.0, 50.0, ColorMode::Rgb, 72.0);
    assert!(render_page_to_rgba(&doc, 5, 72.0, ViewMode::Enhanced).is_err());
    assert!(render_page_to_rgba(&doc, 0, 0.0, ViewMode::Enhanced).is_err());
    assert!(render_page_to_rgba(&doc, 0, f64::NAN, ViewMode::Enhanced).is_err());

    // 2. Viewport camera transforms, rotation, fit_rect, zoom_at_screen_point
    let mut vp = Viewport::new(800, 600);
    vp.fit_rect(Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0), 20.0);
    let page_center_px = vp.world_to_screen(Point2D::new(105.0, 148.5));
    assert!((page_center_px.x - 400.0).abs() < 1e-4);
    assert!((page_center_px.y - 300.0).abs() < 1e-4);

    let anchor_screen = Point2D::new(250.0, 180.0);
    let world_before = vp.screen_to_world(anchor_screen);
    vp.zoom_at_screen_point(anchor_screen, 2.0);
    let world_after = vp.screen_to_world(anchor_screen);
    assert!(world_before.distance_to(world_after) < 1e-6);

    vp.rotation_deg = 30.0;
    let p_rot = Point2D::new(45.0, 60.0);
    let roundtrip = vp.screen_to_world(vp.world_to_screen(p_rot));
    assert!(p_rot.distance_to(roundtrip) < 1e-6);

    // 3. Remaining effects & overlays: Contour Inside/ToCenter, Blend along_path,
    //    FountainRadial transparency, 8-pt Envelope, PushPull/Zipper Distort,
    //    PerspectiveGrid (TwoPoint + Isometric), Angular/Callout3Pt Dimensions,
    //    BitmapPattern, Group opacity, and Multipage rendering!
    let mut doc2 = Document::new("AllVariants", 140.0, 100.0, ColorMode::Rgb, 72.0);
    doc2.set_grid(coree_core::GridConfig {
        visible: true,
        show_dots: true,
        ..doc2.grid()
    });
    let mut pg = doc2.perspective_grid().clone();
    pg.enabled = true;
    doc2.set_perspective_grid(pg);
    doc2.add_guide(
        GuideOrientation::Angled {
            origin: Point2D::new(70.0, 50.0),
            angle_deg: 45.0,
        },
        false,
    );

    // Bitmap pattern asset
    let mut tile = vec![0u8; 8 * 8 * 4];
    for (i, px) in tile.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *px = if i % 2 == 0 {
            [20, 180, 220, 255]
        } else {
            [240, 240, 240, 255]
        };
    }
    let bmp_id = doc2.register_asset(RasterAsset::new(8, 8, tile, 72.0, ColorMode::Rgb).unwrap());
    let mut bmp_rect = make_rect(5.0, 5.0, 20.0, 20.0, Color::WHITE);
    bmp_rect.fill = Fill::BitmapPattern {
        image_id: bmp_id,
        tile_w: 5.0,
        tile_h: 5.0,
    };
    doc2.add_object(bmp_rect, None).unwrap();

    // Contour::ToCenter + Distort::Zipper
    let mut ct_in = make_rect(32.0, 5.0, 24.0, 24.0, Color::rgba8(220, 40, 40, 255));
    ct_in.effects.push(Effect::Contour {
        mode: ContourMode::ToCenter,
        steps: 4,
        offset_mm: 2.0,
        fill_color: Color::rgba8(255, 240, 40, 255),
        outline_color: Color::BLACK,
    });
    doc2.add_object(ct_in, None).unwrap();

    // FountainRadial Transparency (object-local center around make_circle origin)
    let mut rad_t = make_circle(78.0, 18.0, 12.0, Color::rgba8(30, 80, 230, 255));
    rad_t.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainRadial {
            center: Point2D::ZERO,
            radius: 12.0,
            inner_alpha: 1.0,
            outer_alpha: 0.0,
        },
        blend_mode: BlendMode::Normal,
    });
    doc2.add_object(rad_t, None).unwrap();

    // Blend along_path
    let guide_path_id = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BlendPath",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(15.0, 55.0),
                    Point2D::new(85.0, 75.0),
                )),
            )
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();
    let end_id = doc2
        .add_object(
            make_circle(85.0, 75.0, 6.0, Color::rgba8(20, 200, 40, 255)),
            None,
        )
        .unwrap();
    let mut start_b = make_rect(10.0, 50.0, 10.0, 10.0, Color::rgba8(230, 50, 180, 255));
    start_b.effects.push(Effect::Blend {
        target_id: end_id,
        steps: 5,
        rotation_deg: 90.0,
        along_path: Some(guide_path_id),

        rotate_along_path: true,
    });
    doc2.add_object(start_b, None).unwrap();

    // Angular + Callout3Pt dimensions
    doc2.add_object(
        SceneObject::new(
            ObjectId(0),
            "AngDim",
            NodePayload::Dimension(DimensionObject::angular(
                Point2D::new(105.0, 35.0),
                Point2D::new(130.0, 35.0),
                Point2D::new(105.0, 10.0),
                12.0,
            )),
        ),
        None,
    )
    .unwrap();
    let mut callout = DimensionObject::linear(
        DimensionKind::Callout3Pt,
        Point2D::new(105.0, 65.0),
        Point2D::new(115.0, 55.0),
        0.0,
    );
    callout.p3 = Some(Point2D::new(130.0, 55.0));
    callout.prefix = "Note".into();
    doc2.add_object(
        SceneObject::new(ObjectId(0), "Callout", NodePayload::Dimension(callout)),
        None,
    )
    .unwrap();

    // Group with opacity < 1.0
    let g1 = doc2
        .add_object(
            make_rect(100.0, 75.0, 15.0, 15.0, Color::rgba8(200, 0, 0, 255)),
            None,
        )
        .unwrap();
    let g2 = doc2
        .add_object(
            make_rect(108.0, 80.0, 15.0, 15.0, Color::rgba8(0, 0, 200, 255)),
            None,
        )
        .unwrap();
    let grp = doc2.group_objects(&[g1, g2], "TranslucentGroup").unwrap();
    coree_core::apply_command(
        &mut doc2,
        &coree_core::CommandOp::SetOpacity {
            ids: vec![grp],
            opacity: 0.5,
        },
    )
    .unwrap();

    let mut opts = RenderOptions::interactive();
    opts.show_margin_guides = true;
    let img = render_page_to_rgba_with_options(&doc2, 0, 72.0, ViewMode::Normal, &opts).unwrap();
    assert!(count_non_white_pixels(&img.rgba) > 1000);

    // QuickTrace & LineArt preset
    let asset = RasterAsset::new(img.width, img.height, img.rgba, 72.0, ColorMode::Rgb).unwrap();
    let quick = trace_raster(
        &asset,
        &TraceConfig {
            mode: TraceMode::QuickTrace,
            max_colors: 4,
            ..TraceConfig::default()
        },
    );
    assert!(!quick.is_empty());

    // SnapEngine angled guide + page edge + node snapping
    let (snapped_edge, g_edge) = SnapEngine::snap_point(
        &doc2,
        0,
        Point2D::new(1.2, 50.0),
        &[],
        &SnapOptions {
            page_edges: true,
            objects: false,
            nodes: false,
            guidelines: false,
            grid: false,
            dynamic_guides: true,
            tolerance_mm: 2.5,
            ..SnapOptions::default()
        },
    );
    assert!(snapped_edge.x.abs() < 1e-6);
    assert!(g_edge.iter().any(|g| g.kind == DynamicGuideKind::PageEdge));
}

// =============================================================================
// 9. Audit Regression Tests (Findings #2 - #7)
// =============================================================================

#[test]
fn regression_2_pixel_rect_extreme_zoom_no_overflow() {
    // Direct i64 overflow-safe clamping tests
    let huge = PixelRect::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX);
    assert_eq!(
        huge.clamp_to_bounds(800, 600),
        Some(PixelRect::new(0, 0, 800, 600))
    );

    let big_f64 = Rect2D::new(-2.5e9, -2.5e9, 200.0, 200.0);
    assert_eq!(
        PixelRect::from_f64_rect_clamped(big_f64, 10, 800, 600),
        Some(PixelRect::new(0, 0, 210, 210))
    );
    assert_eq!(
        PixelRect::from_f64_rect_clamped(Rect2D::new(900.0, 900.0, 1000.0, 1000.0), 0, 800, 600),
        None
    );

    // High-zoom MeshFill + DropShadow rendering where sb.min_x < -2.2e9 while sb.max_x is inside viewport
    let mut doc = Document::new("HighZoom", 2000.0, 2000.0, ColorMode::Rgb, 96.0);
    // At dpi=96 and max zoom=256.0 (967.56 px/mm), width=3_000_000mm spans ~2.90e9 pixels (> i32::MAX)!
    let mut mesh_obj = make_rect(
        -2_998_500.0,
        -1_499_250.0,
        3_000_000.0,
        3_000_000.0,
        Color::WHITE,
    );
    mesh_obj.fill = Fill::MeshFill {
        rows: 2,
        cols: 2,
        nodes: vec![
            MeshNode {
                row: 0,
                col: 0,
                pos: Point2D::new(0.0, 0.0),
                color: Color::rgba8(220, 40, 40, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
            MeshNode {
                row: 0,
                col: 1,
                pos: Point2D::new(3_000_000.0, 0.0),
                color: Color::rgba8(40, 60, 220, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
            MeshNode {
                row: 1,
                col: 0,
                pos: Point2D::new(0.0, 3_000_000.0),
                color: Color::rgba8(40, 200, 60, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
            MeshNode {
                row: 1,
                col: 1,
                pos: Point2D::new(3_000_000.0, 3_000_000.0),
                color: Color::rgba8(200, 40, 200, 255),

                alpha: 1.0,
                handle_n: None,
                handle_s: None,
                handle_e: None,
                handle_w: None,
            },
        ],
    };
    mesh_obj.effects.push(Effect::DropShadow {
        dx_mm: 0.01,
        dy_mm: 0.01,
        blur_mm: 0.01,
        opacity: 0.6,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    doc.add_object(mesh_obj, None).unwrap();

    // Pan so the right edge x=1500mm lands at ~58px inside the 128x128 viewport while min_x is ~-2.90e9!
    let mut vp = Viewport::new(128, 128);
    vp.zoom = 256.0;
    vp.pan_mm = Point2D::new(1500.0 - 0.06, 750.0);

    let mut backend = TinySkiaBackend::new(128, 128).unwrap();
    let mut renderer = SceneRenderer::new();
    renderer.render_document(
        &doc,
        &vp,
        ViewMode::Enhanced,
        &RenderOptions::export(),
        &mut backend,
    );
    let buf = backend.into_rgba();
    let non_white = count_non_white_pixels(&buf.rgba);
    assert!(
        non_white > 1000,
        "Extreme zoom MeshFill+DropShadow must not vanish due to i32 overflow (non_white={non_white})"
    );
}

#[test]
fn regression_3_tile_cache_invalidation_and_cross_tile_lenses_and_dropshadow() {
    // -------------------------------------------------------------------------
    // A. DirtyTileCache invalidation on active_page_index, multipage_view, and RenderOptions
    // -------------------------------------------------------------------------
    let mut doc = Document::new("MultiPageCache", 100.0, 100.0, ColorMode::Rgb, 96.0);
    doc.add_object(
        make_rect(10.0, 10.0, 40.0, 40.0, Color::rgba8(230, 30, 30, 255)),
        None,
    )
    .unwrap();
    let _p1 = doc.add_page("Page 2", 100.0, 100.0, 0.0);
    doc.set_active_page_index(1).unwrap();
    doc.add_object(
        make_rect(10.0, 10.0, 40.0, 40.0, Color::rgba8(30, 60, 230, 255)),
        None,
    )
    .unwrap();
    doc.set_active_page_index(0).unwrap();

    let vp = Viewport::new(300, 300);
    let mut opts = RenderOptions::export();
    let mut renderer = SceneRenderer::new();
    let mut cache = DirtyTileCache::new(300, 300);

    let buf_p0 = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    let sample_p0 = buf_p0.get_pixel(100, 100).unwrap();
    assert!(
        sample_p0[0] > 200 && sample_p0[2] < 50,
        "Page 0 should be red: {sample_p0:?}"
    );

    // Switch active page WITHOUT calling mark_all_dirty()
    doc.set_active_page_index(1).unwrap();
    let rerendered_page =
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    assert!(
        rerendered_page > 0,
        "Switching active_page_index must invalidate tile cache"
    );
    let buf_p1 = cache.composite_to_rgba();
    let sample_p1 = buf_p1.get_pixel(100, 100).unwrap();
    assert!(
        sample_p1[2] > 200 && sample_p1[0] < 50,
        "Page 1 should be blue: {sample_p1:?}"
    );

    // Toggle doc.multipage_view WITHOUT touching viewport.multipage_view or mark_all_dirty()
    doc.set_multipage_view(true);
    let rerendered_mp =
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    assert!(
        rerendered_mp > 0,
        "Toggling doc.multipage_view must invalidate tile cache"
    );
    doc.set_multipage_view(false);
    let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);

    // Mutate RenderOptions (page_bg) WITHOUT calling mark_all_dirty()
    opts.page_bg = Color::rgba8(240, 240, 180, 255);
    let rerendered_opts =
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    assert!(
        rerendered_opts > 0,
        "Changing RenderOptions must invalidate tile cache"
    );
    let buf_bg = cache.composite_to_rgba();
    let bg_px = buf_bg.get_pixel(250, 250).unwrap();
    assert_eq!(bg_px, [240, 240, 180, 255]);

    // -------------------------------------------------------------------------
    // B. Cross-tile blurred DropShadow: exact 0-diff parity across x=256 seam!
    // -------------------------------------------------------------------------
    let mut ds_doc = Document::new("CrossTileShadow", 150.0, 90.0, ColorMode::Rgb, 96.0);
    // At 96 DPI (3.7795 px/mm), x=256px is at 67.73mm. Place rect [52..84, 20..52] right across x=256!
    let mut shadow_rect = make_rect(52.0, 20.0, 32.0, 32.0, Color::rgba8(40, 120, 220, 255));
    shadow_rect.effects.push(Effect::DropShadow {
        dx_mm: 5.0,
        dy_mm: 5.0,
        blur_mm: 6.0,
        opacity: 0.7,
        color: Color::rgba8(10, 10, 20, 255),
        feather_direction: FeatherDirection::Middle,
    });
    ds_doc.add_object(shadow_rect, None).unwrap();

    let vp_wide = Viewport::new(512, 320);
    let export_opts = RenderOptions::export();
    let mut direct_be = TinySkiaBackend::new(512, 320).unwrap();
    let mut r_direct = SceneRenderer::new();
    r_direct.render_document(
        &ds_doc,
        &vp_wide,
        ViewMode::Enhanced,
        &export_opts,
        &mut direct_be,
    );
    let direct_ds = direct_be.into_rgba();

    let mut r_tiled = SceneRenderer::new();
    let mut cache_ds = DirtyTileCache::new(512, 320);
    let tiled_ds = cache_ds.render_viewport(
        &ds_doc,
        &vp_wide,
        ViewMode::Enhanced,
        &export_opts,
        &mut r_tiled,
    );

    assert_eq!(
        direct_ds.rgba, tiled_ds.rgba,
        "Cross-tile blurred DropShadow must match full-frame render bit-for-bit with zero tile seam!"
    );

    // -------------------------------------------------------------------------
    // C. Cross-tile FishEye, Magnify, Viewpoint (across tiles), and Frozen Lens parity!
    // -------------------------------------------------------------------------
    let build_lens_doc = |lens_obj: SceneObject| -> Document {
        let mut d = Document::new("CrossTileLens", 150.0, 90.0, ColorMode::Rgb, 96.0);
        // Distinct left-of-seam (red, x < 67.73mm) and right-of-seam (green, x >= 67.73mm) halves,
        // plus a crisp blue accent strip near x = 20..35mm (inside Tile 0)
        d.add_object(
            make_rect(0.0, 0.0, 67.73, 90.0, Color::rgba8(220, 40, 40, 255)),
            None,
        )
        .unwrap();
        d.add_object(
            make_rect(67.73, 0.0, 82.27, 90.0, Color::rgba8(30, 190, 50, 255)),
            None,
        )
        .unwrap();
        d.add_object(
            make_rect(15.0, 25.0, 25.0, 30.0, Color::rgba8(30, 50, 230, 255)),
            None,
        )
        .unwrap();
        d.add_object(lens_obj, None).unwrap();
        d
    };

    let compare_direct_vs_tiled = |d: &Document, label: &str| {
        let mut be = TinySkiaBackend::new(512, 320).unwrap();
        let mut rd = SceneRenderer::new();
        rd.render_document(d, &vp_wide, ViewMode::Enhanced, &export_opts, &mut be);
        let full = be.into_rgba();

        let mut rt = SceneRenderer::new();
        let mut tc = DirtyTileCache::new(512, 320);
        let tiled = tc.render_viewport(d, &vp_wide, ViewMode::Enhanced, &export_opts, &mut rt);

        let diff_px: usize = full
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(tiled.rgba.as_chunks::<4>().0.iter())
            .filter(|(a, b)| (0..4).any(|c| (i16::from(a[c]) - i16::from(b[c])).abs() > 2))
            .count();
        assert!(
            diff_px == 0,
            "{label}: expected 0 mismatched pixels between direct and tiled render, got {diff_px}"
        );
        (full, tiled)
    };

    // C1. Cross-tile rectangular FishEye centered right on x=256px (67.73mm)
    let mut fisheye_obj = make_rect(50.0, 20.0, 36.0, 36.0, Color::WHITE);
    fisheye_obj.lens = Some(LensConfig {
        kind: LensKind::FishEye { rate_pct: 120.0 },
        frozen: false,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    compare_direct_vs_tiled(&build_lens_doc(fisheye_obj), "Cross-tile FishEye");

    // C2. Cross-tile Magnify crossing x=256px
    let mut mag_obj = make_rect(50.0, 20.0, 36.0, 36.0, Color::WHITE);
    mag_obj.lens = Some(LensConfig {
        kind: LensKind::Magnify { amount: 2.5 },
        frozen: false,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    compare_direct_vs_tiled(&build_lens_doc(mag_obj), "Cross-tile Magnify");

    // C3. Viewpoint lens sitting strictly in Tile 1 (x=[85..115mm]) sampling blue box in Tile 0 (x=27.5, y=40mm)
    let mut vp_lens = make_rect(85.0, 25.0, 30.0, 30.0, Color::WHITE);
    vp_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: Some(Point2D::new(27.5, 40.0)),
        remove_face: false,

        frozen_snapshot: None,
    });
    let (_, tiled_vp) = compare_direct_vs_tiled(&build_lens_doc(vp_lens), "Cross-tile Viewpoint");
    // Center of lens at x=100mm (~378px), y=40mm (~151px) must show inverted blue = yellow!
    let vp_px = tiled_vp.get_pixel(378, 151).unwrap();
    assert!(
        vp_px[0] > 200 && vp_px[1] > 180 && vp_px[2] < 45,
        "Cross-tile viewpoint Invert of blue box must be yellow, got {vp_px:?}"
    );

    // C4. Frozen lens crossing x=256px: Tile 0 renders first, Tile 1 renders second.
    // Must cache the FULL unclipped lens so left half is cyan (inverted red) and right half is magenta (inverted green)!
    let mut frozen_obj = make_rect(52.0, 20.0, 32.0, 32.0, Color::WHITE);
    frozen_obj.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: true,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let mut frozen_doc = build_lens_doc(frozen_obj);
    let frozen_id = *frozen_doc.pages()[0].layers[0].objects.last().unwrap();
    let (_, tiled_frozen) = compare_direct_vs_tiled(&frozen_doc, "Cross-tile Frozen Lens");

    // Verify left of seam (x=220px, y=135px) is inverted red = cyan, right of seam (x=290px, y=135px) is inverted green = magenta
    let left_px = tiled_frozen.get_pixel(220, 135).unwrap();
    let right_px = tiled_frozen.get_pixel(290, 135).unwrap();
    assert!(
        left_px[0] < 50 && left_px[1] > 200 && left_px[2] > 200,
        "Frozen lens left half must be cyan (inverted red), got {left_px:?}"
    );
    assert!(
        right_px[0] > 200 && right_px[1] < 80 && right_px[2] > 190,
        "Frozen lens right half must be magenta (inverted green), NOT stretched left half! Got {right_px:?}"
    );

    // Now move the frozen lens inside a persistent renderer+cache and confirm it carries BOTH halves cleanly!
    let mut persist_renderer = SceneRenderer::new();
    let mut persist_cache = DirtyTileCache::new(512, 320);
    let _ = persist_cache.render_viewport(
        &frozen_doc,
        &vp_wide,
        ViewMode::Enhanced,
        &export_opts,
        &mut persist_renderer,
    );
    frozen_doc
        .set_object_transform(frozen_id, Affine2D::translate(10.0, 50.0))
        .unwrap();
    persist_cache.mark_all_dirty();
    let moved_frozen = persist_cache.render_viewport(
        &frozen_doc,
        &vp_wide,
        ViewMode::Enhanced,
        &export_opts,
        &mut persist_renderer,
    );
    // Moved lens spans x=[10..42mm] (~38..159px), y=[50..82mm] (~189..310px); left half cyan, right half magenta
    let moved_left = moved_frozen.get_pixel(60, 250).unwrap();
    let moved_right = moved_frozen.get_pixel(135, 250).unwrap();
    assert!(
        moved_left[0] < 50 && moved_left[1] > 200 && moved_left[2] > 200,
        "Moved frozen lens left half must stay cyan, got {moved_left:?}"
    );
    assert!(
        moved_right[0] > 200 && moved_right[1] < 80 && moved_right[2] > 190,
        "Moved frozen lens right half must stay magenta, got {moved_right:?}"
    );
}

#[test]
fn regression_4_grouped_and_symmetry_lenses_no_double_transform() {
    // 1. Grouped Wireframe lens over page artwork: underlying object must NOT be shifted by Group transform!
    let mut doc = Document::new("GroupedLens", 120.0, 80.0, ColorMode::Rgb, 96.0);
    // Red square at [10..35, 10..35]
    doc.add_object(
        make_rect(10.0, 10.0, 25.0, 25.0, Color::rgba8(220, 30, 30, 255)),
        None,
    )
    .unwrap();
    // Blue square at [60..78, 15..35] (covers ONLY left half of [60..90, 10..40]!)
    doc.add_object(
        make_rect(60.0, 15.0, 18.0, 20.0, Color::rgba8(30, 50, 230, 255)),
        None,
    )
    .unwrap();

    // Put Wireframe lens at local [0..30, 0..30] inside a Group translated by (+60, +10)
    let mut wf_lens = make_rect(0.0, 0.0, 30.0, 30.0, Color::WHITE);
    wf_lens.lens = Some(LensConfig {
        kind: LensKind::Wireframe {
            override_outline: Some(Color::rgba8(0, 180, 0, 255)),
            override_fill: Some(Color::rgba8(255, 250, 180, 255)),
        },
        frozen: false,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let wf_id = doc.add_object(wf_lens, None).unwrap();
    let grp_id = doc.group_objects(&[wf_id], "LensGroup").unwrap();
    doc.set_object_transform(grp_id, Affine2D::translate(60.0, 10.0))
        .unwrap();

    let img_wf = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    // Blue square's right edge sits at x=78mm, y=15..35mm, strictly inside the grouped lens [60..90, 10..40]!
    // Check that green wireframe pixels appear along x=78mm inside the lens!
    let px_per_mm = img_wf.dpi / 25.4;
    let edge_x = (78.0 * px_per_mm).round() as u32;
    let mid_y = (25.0 * px_per_mm).round() as u32;
    let mut found_green_wire = false;
    for dx in -2_i32..=2 {
        let px = img_wf.pixel((edge_x as i32 + dx) as u32, mid_y);
        if px[1] > 140 && px[0] < 100 && px[2] < 100 {
            found_green_wire = true;
            break;
        }
    }
    assert!(
        found_green_wire,
        "Grouped Wireframe lens must render underlying blue square edge at x=78mm without double-transform!"
    );

    // 2. Grouped Invert lens with `remove_face = true`: must invert blue box [60..78] to yellow
    //    while leaving blank page [82..88] pure white!
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetLens {
            ids: vec![wf_id],
            lens: Some(LensConfig {
                kind: LensKind::Invert,
                frozen: false,
                viewpoint: None,
                remove_face: true,

                frozen_snapshot: None,
            }),
        },
    )
    .unwrap();
    let img_rf = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let over_blue = pixel_at_mm(&img_rf, 68.0, 25.0);
    let over_blank = pixel_at_mm(&img_rf, 84.0, 25.0);
    assert!(
        over_blue[0] > 200 && over_blue[1] > 180 && over_blue[2] < 45,
        "Grouped remove_face Invert over blue box must be yellow, got {over_blue:?}"
    );
    assert!(
        over_blank[0] > 245 && over_blank[1] > 245 && over_blank[2] > 245,
        "Grouped remove_face Invert over blank page must stay white, got {over_blank:?}"
    );

    // 3. Grouped Invert lens with page-space `viewpoint = (22.5, 22.5)` (center of red square):
    //    must sample the red square at (22.5, 22.5) and show cyan inside the grouped lens at (75, 25)!
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetLens {
            ids: vec![wf_id],
            lens: Some(LensConfig {
                kind: LensKind::Invert,
                frozen: false,
                viewpoint: Some(Point2D::new(22.5, 22.5)),
                remove_face: false,

                frozen_snapshot: None,
            }),
        },
    )
    .unwrap();
    let img_vp = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let vp_px = pixel_at_mm(&img_vp, 75.0, 25.0);
    assert!(
        vp_px[0] < 50 && vp_px[1] > 200 && vp_px[2] > 200,
        "Grouped lens viewpoint=(22.5,22.5) must sample red square in page space -> cyan, got {vp_px:?}"
    );

    // 4. Symmetry lens with `viewpoint` pointing at red square (22.5, 22.5):
    //    BOTH symmetry instances must sample (22.5, 22.5) -> cyan (not double-rotated/shifted)!
    let mut sym_doc = Document::new("SymLens", 120.0, 80.0, ColorMode::Rgb, 96.0);
    sym_doc
        .add_object(
            make_rect(10.0, 10.0, 25.0, 25.0, Color::rgba8(220, 30, 30, 255)),
            None,
        )
        .unwrap();
    let mut sym_lens = make_rect(70.0, 15.0, 20.0, 20.0, Color::WHITE);
    sym_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: Some(Point2D::new(22.5, 22.5)),
        remove_face: false,

        frozen_snapshot: None,
    });
    sym_lens.effects.push(Effect::Symmetry {
        axes: 2,
        mirror: false,
        origin: Point2D::new(60.0, 40.0),
    });
    sym_doc.add_object(sym_lens, None).unwrap();

    let img_sym = render_page_to_rgba(&sym_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    // Original instance centered at (80, 25); 180-deg rotated twin centered at (40, 55)!
    let inst1 = pixel_at_mm(&img_sym, 80.0, 25.0);
    let inst2 = pixel_at_mm(&img_sym, 40.0, 55.0);
    assert!(
        inst1[0] < 50 && inst1[1] > 200 && inst1[2] > 200,
        "Symmetry lens instance 1 must sample red square -> cyan, got {inst1:?}"
    );
    assert!(
        inst2[0] < 50 && inst2[1] > 200 && inst2[2] > 200,
        "Symmetry lens instance 2 must sample red square -> cyan without double-transform, got {inst2:?}"
    );
}

#[test]
fn regression_5_adversarial_malformed_inputs_no_oom_or_panic() {
    // 1. Extreme stroke coordinates & huge radii on valid RasterAsset must NOT OOM/overflow
    let mut asset =
        RasterAsset::new(32, 32, vec![200u8; 32 * 32 * 4], 96.0, ColorMode::Rgb).unwrap();
    let extreme_pts = [
        Point2D::new(f64::NAN, 0.0),
        Point2D::new(0.0, 0.0),
        Point2D::new(1e12, 0.0),
    ];
    paint_brush_stroke(&mut asset, &extreme_pts, 1e9, 0.5, Color::BLACK, 1.0);
    erase_stroke(&mut asset, &extreme_pts, 1e9, 0.5);
    clone_stamp_stroke(&mut asset, Point2D::new(5.0, 5.0), &extreme_pts, 1e9, 0.5);
    smudge_stroke(&mut asset, &extreme_pts, 1e9, 0.8);

    // 2. Oversized resample must cleanly return Err(InvalidDimensions) instead of allocating
    let res = resample(&asset, 50_000, 50_000);
    assert!(
        matches!(
            res,
            Err(coree_render::RenderError::InvalidDimensions { .. })
        ),
        "Oversized resample must reject dimensions safely, got {res:?}"
    );

    // 3. Malformed RasterAsset with mismatched rgba_bytes.len() must NEVER panic anywhere
    let mut malformed = RasterAsset::new(2, 2, vec![255u8; 16], 96.0, ColorMode::Rgb).unwrap();
    malformed.width = 64;
    malformed.height = 64;
    malformed.tiles[0].make_mut().truncate(12); // len 12 != 64*64*4

    paint_brush_stroke(
        &mut malformed,
        &[Point2D::new(10.0, 10.0), Point2D::new(20.0, 20.0)],
        4.0,
        0.5,
        Color::BLACK,
        1.0,
    );
    erase_stroke(&mut malformed, &[Point2D::new(10.0, 10.0)], 4.0, 0.5);
    clone_stamp_stroke(
        &mut malformed,
        Point2D::new(0.0, 0.0),
        &[Point2D::new(10.0, 10.0)],
        4.0,
        0.5,
    );
    smudge_stroke(
        &mut malformed,
        &[Point2D::new(5.0, 5.0), Point2D::new(15.0, 15.0)],
        4.0,
        0.5,
    );
    let wand = RasterMask::from_magic_wand(&malformed, 10, 10, 20.0, true);
    assert_eq!(wand.selected_pixel_count(), 0);
    apply_cutout_background_removal(&mut malformed, &RasterMask::empty(64, 64), 2.0);
    let _ = apply_raster_filter(&malformed, &RasterFilter::Invert);
    let _ = apply_raster_filter(&malformed, &RasterFilter::GaussianBlur { radius_px: 3.0 });
    assert!(resample(&malformed, 32, 32).is_err());
    assert!(trace_raster(&malformed, &TraceConfig::default()).is_empty());

    // 4. Non-finite vertices in RasterMask::from_lasso must be filtered cleanly
    let lasso_mask = RasterMask::from_lasso(
        32,
        32,
        &[
            Point2D::new(f64::NAN, f64::INFINITY),
            Point2D::new(4.0, 4.0),
            Point2D::new(24.0, 4.0),
            Point2D::new(f64::NEG_INFINITY, 12.0),
            Point2D::new(24.0, 24.0),
            Point2D::new(4.0, 24.0),
        ],
    );
    assert_eq!(lasso_mask.get(14, 14), 255);
    assert_eq!(lasso_mask.get(1, 1), 0);

    // 5. SmartFill with non-finite / extreme coordinates must NOT overflow or hang
    let mut sf_doc = Document::new("SmartFillAdversarial", 100.0, 100.0, ColorMode::Rgb, 96.0);
    sf_doc
        .add_object(make_rect(20.0, 20.0, 40.0, 40.0, Color::TRANSPARENT), None)
        .unwrap();
    let bad_path = BezierPath {
        subpaths: vec![coree_core::Subpath::from_points(
            [
                Point2D::new(f64::NAN, 10.0),
                Point2D::new(1e15, -1e15),
                Point2D::new(f64::INFINITY, 30.0),
            ],
            false,
        )],
    };
    sf_doc
        .add_object(
            SceneObject::new(ObjectId(0), "BadPath", NodePayload::Path(bad_path)),
            None,
        )
        .unwrap();
    // Must terminate cleanly without panic or hang and still extract the valid square!
    let sf_res = compute_smart_fill(
        &sf_doc,
        0,
        Point2D::new(40.0, 40.0),
        Fill::Solid(Color::BLACK),
        OutlineStyle::none(),
    );
    assert!(sf_res.is_some());
}

#[test]
fn regression_6_hit_test_locked_layer_grouped_objects_and_snap_angled_guide() {
    let mut doc = Document::new("HitSnapRegression", 200.0, 200.0, ColorMode::Rgb, 96.0);

    // 1. Locked layer MUST block hit_test_point & hit_test_rect even when obj.locked == false
    let page0_id = doc.pages()[0].id;
    let locked_layer_id = doc
        .add_layer(page0_id, "LockedLayer", false, MasterLayerScope::None)
        .unwrap();
    let locked_obj_id = doc
        .add_object(
            make_rect(10.0, 10.0, 30.0, 30.0, Color::BLACK),
            Some(locked_layer_id),
        )
        .unwrap();
    assert!(!doc.object(locked_obj_id).unwrap().locked);
    doc.set_layer_flags(locked_layer_id, None, Some(true), None, None)
        .unwrap();

    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(25.0, 25.0), 1.0),
        None,
        "Objects on a locked layer must not be hit by hit_test_point"
    );
    assert!(
        hit_test_rect(&doc, 0, Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0), false).is_empty(),
        "Objects on a locked layer must not be returned by hit_test_rect"
    );

    // 2. Grouped translated non-path objects (ArtisticText, Table, Raster) must hit at world coords
    let table_obj = SceneObject::new(
        ObjectId(0),
        "GroupedTable",
        NodePayload::Table(VectorTable::new(2, 2, 15.0, 10.0)),
    );
    let text_obj = SceneObject::new(
        ObjectId(0),
        "GroupedText",
        NodePayload::ArtisticText(ArtisticText::from_plain(
            "Hello World",
            TextStyle::default(),
        )),
    )
    .with_transform(Affine2D::translate(0.0, 30.0));

    let tid = doc.add_object(table_obj, None).unwrap();
    let txid = doc.add_object(text_obj, None).unwrap();
    let grp_id = doc.group_objects(&[tid, txid], "MovedGroup").unwrap();
    doc.set_object_transform(grp_id, Affine2D::translate(100.0, 80.0))
        .unwrap();

    // Should hit the table inside the translated group at (115, 90), NOT at local (15, 10)
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(115.0, 90.0), 1.0),
        Some(grp_id),
        "Grouped Table must be hit at its group-transformed world coordinates"
    );
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(15.0, 10.0), 1.0),
        None,
        "Grouped Table must NOT be hit at stale untransformed local coordinates"
    );

    // 3. SnapEngine with `dynamic_guides = false`: in-range angled guide must NOT clobber matched X snap!
    let mut snap_doc = Document::new("SnapAngled", 200.0, 200.0, ColorMode::Rgb, 96.0);
    snap_doc.add_guide(GuideOrientation::Vertical(20.0), false);
    snap_doc.add_guide(
        GuideOrientation::Angled {
            origin: Point2D::new(20.5, 50.0),
            angle_deg: 45.0,
        },
        false,
    );

    let (snapped, guides) = SnapEngine::snap_point(
        &snap_doc,
        0,
        Point2D::new(20.4, 50.0),
        &[],
        &SnapOptions {
            grid: false,
            guidelines: true,
            page_edges: false,
            objects: false,
            nodes: false,
            dynamic_guides: false,
            tolerance_mm: 2.0,
            ..SnapOptions::default()
        },
    );
    assert!(guides.is_empty());
    assert!(
        (snapped.x - 20.0).abs() < 1e-6 && (snapped.y - 50.0).abs() < 1e-6,
        "Angled guide must NOT clobber already-matched vertical X snap when dynamic_guides=false, got {snapped:?}"
    );
}

#[test]
fn regression_7_vector_pattern_bevel_wireframe_dims_trace_and_arabic() {
    // 1. Self-referencing VectorPattern symbol must not stack overflow or re-init FontSystem
    let mut pat_doc = Document::new("SelfRefPattern", 60.0, 60.0, ColorMode::Rgb, 72.0);
    let mut sym = SymbolDef::new("RecursivePattern", Vec::new());
    let sym_id = sym.id;
    let self_ref_obj =
        make_rect(0.0, 0.0, 10.0, 10.0, Color::BLACK).with_fill(Fill::VectorPattern {
            symbol_id: sym_id,
            tile_w: 10.0,
            tile_h: 10.0,
        });
    sym.objects.push(self_ref_obj);
    pat_doc.insert_symbol(sym);
    let host_obj = make_rect(5.0, 5.0, 40.0, 40.0, Color::WHITE).with_fill(Fill::VectorPattern {
        symbol_id: sym_id,
        tile_w: 10.0,
        tile_h: 10.0,
    });
    pat_doc.add_object(host_obj, None).unwrap();
    let _ = render_page_to_rgba(&pat_doc, 0, 72.0, ViewMode::Enhanced).unwrap();

    // 2. Extrude25D front bevel highlight must render ON TOP of opaque front fill!
    let render_extrude = |bevel_depth: f64| {
        let mut d = Document::new("BevelTest", 80.0, 80.0, ColorMode::Rgb, 96.0);
        let mut rect = make_rect(20.0, 20.0, 30.0, 30.0, Color::rgba8(20, 40, 180, 255));
        rect.effects.push(Effect::Extrude25D {
            vanishing_point: Point2D::new(70.0, 10.0),
            depth: 12.0,
            bevel_depth,
            shade_color: Color::rgba8(10, 15, 50, 255),
            light_dir: [0.4, -0.6, 0.7],
        });
        d.add_object(rect, None).unwrap();
        render_page_to_rgba(&d, 0, 96.0, ViewMode::Enhanced).unwrap()
    };
    let img_no_bevel = render_extrude(0.0);
    let img_bevel = render_extrude(2.5);
    let px_no_bevel = pixel_at_mm(&img_no_bevel, 35.0, 35.0);
    let px_bevel = pixel_at_mm(&img_bevel, 35.0, 35.0);
    assert!(
        px_bevel[0] > px_no_bevel[0] + 25 && px_bevel[1] > px_no_bevel[1] + 25,
        "Extrude25D front bevel must visibly brighten opaque front face: no_bevel={px_no_bevel:?}, bevel={px_bevel:?}"
    );

    // 3. DimensionKind::Angular & Callout3Pt must honor ViewMode::Wireframe stroke color!
    let mut dim_doc = Document::new("DimWireframe", 100.0, 100.0, ColorMode::Rgb, 96.0);
    let ang = DimensionObject::angular(
        Point2D::new(20.0, 50.0),
        Point2D::new(50.0, 50.0),
        Point2D::new(20.0, 20.0),
        18.0,
    );
    dim_doc
        .add_object(
            SceneObject::new(ObjectId(0), "AngularDim", NodePayload::Dimension(ang))
                .with_outline(OutlineStyle::solid(Color::rgba8(230, 20, 20, 255), 1.0)),
            None,
        )
        .unwrap();
    let mut callout = DimensionObject::linear(
        DimensionKind::Callout3Pt,
        Point2D::new(60.0, 60.0),
        Point2D::new(75.0, 75.0),
        0.0,
    );
    callout.p3 = Some(Point2D::new(90.0, 75.0));
    dim_doc
        .add_object(
            SceneObject::new(ObjectId(0), "CalloutDim", NodePayload::Dimension(callout))
                .with_outline(OutlineStyle::solid(Color::rgba8(230, 20, 20, 255), 1.0)),
            None,
        )
        .unwrap();

    let mut wf_opts = RenderOptions::export();
    wf_opts.wireframe_stroke_color = Color::rgba8(0, 200, 60, 255);
    let img_dim_wf =
        render_page_to_rgba_with_options(&dim_doc, 0, 96.0, ViewMode::Wireframe, &wf_opts).unwrap();
    let green_px = img_dim_wf
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[1] > 150 && px[0] < 80)
        .count();
    let red_px = img_dim_wf
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 180 && px[1] < 80)
        .count();
    assert!(
        green_px > 50 && red_px == 0,
        "Angular & Callout3Pt dimensions must use wireframe stroke color in Wireframe mode (green={green_px}, red={red_px})"
    );

    // 4. Trace: OutlinePreset::LineArt must cap palette to <= 2 colors even when max_colors = 16;
    //    and CenterlineTrace must thin edge-touching strokes cleanly!
    let w = 40u32;
    let h = 40u32;
    let mut multi = vec![255u8; (w * h * 4) as usize];
    let colors = [
        [220u8, 20, 20, 255],
        [20, 200, 20, 255],
        [20, 40, 220, 255],
        [220, 200, 20, 255],
    ];
    for y in 0..h {
        for x in 0..w {
            let quad = usize::from(x >= w / 2) + 2 * usize::from(y >= h / 2);
            let idx = ((y * w + x) * 4) as usize;
            multi[idx..idx + 4].copy_from_slice(&colors[quad]);
        }
    }
    let multi_asset = RasterAsset::new(w, h, multi, 96.0, ColorMode::Rgb).unwrap();
    let line_art = trace_raster(
        &multi_asset,
        &TraceConfig {
            mode: TraceMode::OutlineTrace(OutlinePreset::LineArt),
            max_colors: 16,
            remove_background: false,
            ..TraceConfig::default()
        },
    );
    let unique_colors: std::collections::HashSet<[u8; 4]> =
        line_art.iter().map(|(_, c)| c.to_rgba8()).collect();
    assert!(
        !unique_colors.is_empty() && unique_colors.len() <= 2,
        "LineArt preset must cap distinct traced colors to <= 2, got {}",
        unique_colors.len()
    );

    // Edge-touching 6px-thick horizontal bar spanning x=0..40, y=0..6
    let mut edge_buf = vec![255u8; (w * h * 4) as usize];
    for y in 0..6 {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            edge_buf[idx..idx + 4].copy_from_slice(&[15, 15, 15, 255]);
        }
    }
    let edge_asset = RasterAsset::new(w, h, edge_buf, 96.0, ColorMode::Rgb).unwrap();
    let cl_edge = trace_raster_to_mm(
        &edge_asset,
        40.0,
        40.0,
        &TraceConfig {
            mode: TraceMode::CenterlineTrace,
            ..TraceConfig::default()
        },
    );
    assert!(
        !cl_edge.is_empty(),
        "CenterlineTrace must thin edge-touching strokes into a valid skeleton"
    );
    let cl_bb = cl_edge[0].0.bounding_box().unwrap();
    assert!(
        cl_bb.height() < 3.0 && cl_bb.width() > 20.0,
        "Thinned edge-touching bar should collapse to a thin horizontal centerline, got {cl_bb:?}"
    );

    // 5. ParagraphText `letter_spacing_mm` must actually widen shaped layout!
    let mut para_tight =
        ParagraphText::new("ABCDEF", Vec2D::new(120.0, 40.0), TextStyle::default());
    para_tight.runs[0].style.letter_spacing_mm = 0.0;
    let mut para_wide = ParagraphText::new("ABCDEF", Vec2D::new(120.0, 40.0), TextStyle::default());
    para_wide.runs[0].style.letter_spacing_mm = 4.0;
    let bb_tight = paragraph_to_curves(&para_tight).bounding_box().unwrap();
    let bb_wide = paragraph_to_curves(&para_wide).bounding_box().unwrap();
    assert!(
        bb_wide.width() > bb_tight.width() + 10.0,
        "ParagraphText letter_spacing_mm must widen rendered glyphs (tight={}, wide={})",
        bb_tight.width(),
        bb_wide.width()
    );

    // 6. Arabic Kashida joining rules: must NOT insert Tatweel after right-joining 'ژ'
    //    and must NOT split Lam-Alef Wasla 'لٱ' ligature!
    let k_zhe = apply_kashida_elongation("رژيم", 6);
    assert!(
        !k_zhe.contains("ژ\u{0640}"),
        "Kashida must NOT be inserted after right-joining-only 'ژ': {k_zhe:?}"
    );
    assert!(
        k_zhe.contains("ي\u{0640}"),
        "Kashida should instead insert after dual-joining 'ي': {k_zhe:?}"
    );

    let k_lam_wasla = apply_kashida_elongation("لٱم", 4);
    assert!(
        !k_lam_wasla.contains("ل\u{0640}ٱ"),
        "Kashida must NOT split Lam-Alef Wasla 'لٱ' ligature: {k_lam_wasla:?}"
    );
}

#[test]
fn icc_live_cmyk_soft_proofing_gamut_warning_and_tile_cache_invalidation() {
    let mut doc = Document::new("SoftProof", 90.0, 30.0, ColorMode::Rgb, 96.0);
    // Swatch 1: Pure neon sRGB green (0,255,0) — OUT of CMYK gamut
    doc.add_object(
        rect_obj(
            0.0,
            0.0,
            30.0,
            30.0,
            Fill::Solid(Color::rgba8(0, 255, 0, 255)),
        ),
        None,
    )
    .unwrap();
    // Swatch 2: Pure electric sRGB blue (0,0,255) — OUT of CMYK gamut
    doc.add_object(
        rect_obj(
            30.0,
            0.0,
            30.0,
            30.0,
            Fill::Solid(Color::rgba8(0, 0, 255, 255)),
        ),
        None,
    )
    .unwrap();
    // Swatch 3: Neutral mid-gray (128,128,128) — strictly IN CMYK gamut
    doc.add_object(
        rect_obj(
            60.0,
            0.0,
            30.0,
            30.0,
            Fill::Solid(Color::rgba8(128, 128, 128, 255)),
        ),
        None,
    )
    .unwrap();

    let raw_opts = RenderOptions::export();
    let img_raw =
        render_page_to_rgba_with_options(&doc, 0, 96.0, ViewMode::Enhanced, &raw_opts).unwrap();
    let mut iso_opts = RenderOptions::export();
    iso_opts.soft_proof_cmyk = true;
    iso_opts.proof_profile = BuiltinIccProfile::IsoCoatedV2Cmyk;
    let img_iso =
        render_page_to_rgba_with_options(&doc, 0, 96.0, ViewMode::Enhanced, &iso_opts).unwrap();

    let mut swop_opts = iso_opts.clone();
    swop_opts.proof_profile = BuiltinIccProfile::UsWebCoatedSwopCmyk;
    let img_swop =
        render_page_to_rgba_with_options(&doc, 0, 96.0, ViewMode::Enhanced, &swop_opts).unwrap();

    let mut warn_opts = iso_opts.clone();
    warn_opts.gamut_warning = true;
    let img_warn =
        render_page_to_rgba_with_options(&doc, 0, 96.0, ViewMode::Enhanced, &warn_opts).unwrap();

    let raw_green = pixel_at_mm(&img_raw, 15.0, 15.0);
    let iso_green = pixel_at_mm(&img_iso, 15.0, 15.0);
    let swop_green = pixel_at_mm(&img_swop, 15.0, 15.0);
    let warn_green = pixel_at_mm(&img_warn, 15.0, 15.0);
    let warn_blue = pixel_at_mm(&img_warn, 45.0, 15.0);
    let warn_gray = pixel_at_mm(&img_warn, 75.0, 15.0);

    assert_eq!(raw_green, [0, 255, 0, 255]);
    assert!(
        iso_green[1] < 220,
        "ISO Coated v2 soft-proof must noticeably dull neon green: {iso_green:?}"
    );
    assert_ne!(
        iso_green, swop_green,
        "ISO Coated v2 and US Web Coated SWOP must render distinct soft-proof colors"
    );
    assert!(
        warn_green[2] > 100 && warn_blue[1] > 150,
        "Gamut warning must tint out-of-gamut green & blue: green={warn_green:?}, blue={warn_blue:?}"
    );
    assert!(
        (i32::from(warn_gray[0]) - i32::from(warn_gray[1])).abs() < 15
            && (i32::from(warn_gray[1]) - i32::from(warn_gray[2])).abs() < 15,
        "In-gamut neutral gray must NOT trigger gamut warning tint: {warn_gray:?}"
    );

    // DirtyTileCache must auto-invalidate when toggling soft_proof_cmyk, gamut_warning, or moving a guide!
    let mut cache = DirtyTileCache::new(300, 120);
    let mut renderer = SceneRenderer::new();
    let vp = Viewport::new(300, 120);
    let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &raw_opts, &mut renderer);
    assert_eq!(
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &raw_opts, &mut renderer),
        0
    );
    assert!(
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &iso_opts, &mut renderer) > 0,
        "Enabling soft_proof_cmyk must invalidate tile cache"
    );
    assert!(
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &warn_opts, &mut renderer) > 0,
        "Enabling gamut_warning must invalidate tile cache"
    );
    let gid = doc.add_guide(GuideOrientation::Vertical(20.0), false);
    let mut g_opts = RenderOptions::interactive();
    let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &g_opts, &mut renderer);
    // Move existing guide in-place without changing guides.len()
    doc.set_guide_orientation(gid, GuideOrientation::Vertical(55.0));
    g_opts.show_guidelines = true;
    assert!(
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &g_opts, &mut renderer) > 0,
        "Moving an existing guide must invalidate DirtyTileCache"
    );
}

#[test]
fn raster_studio_healing_dodge_burn_and_scanline_flood_fill() {
    // 1. Healing Brush: left half is dark textured blue; right half is uniform bright warm orange.
    //    Healing from left -> right must preserve bright warm orange ambient color while transferring texture!
    let w = 64u32;
    let h = 32u32;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            if x < 32 {
                // Dark blue with +/- 28 high-frequency checkerboard texture
                let tex: i16 = if ((x / 2) + (y / 2)) % 2 == 0 {
                    28
                } else {
                    -28
                };
                rgba[idx] = (40 + tex).clamp(0, 255) as u8;
                rgba[idx + 1] = (60 + tex).clamp(0, 255) as u8;
                rgba[idx + 2] = (140 + tex).clamp(0, 255) as u8;
                rgba[idx + 3] = 255;
            } else {
                // Uniform bright warm orange (225, 130, 65)
                rgba[idx..idx + 4].copy_from_slice(&[225, 130, 65, 255]);
            }
        }
    }
    let mut heal_asset = RasterAsset::new(w, h, rgba, 96.0, ColorMode::Rgb).unwrap();
    apply_healing_stroke(
        &mut heal_asset,
        Point2D::new(16.0, 16.0),
        &[Point2D::new(48.0, 16.0)],
        8.0,
        0.7,
    );
    // Inspect healed center patch [44..52, 12..20]: must remain warm (R > B + 80) AND have texture variance!
    let mut min_r = 255u8;
    let mut max_r = 0u8;
    let mut sum_r = 0u32;
    let mut sum_b = 0u32;
    let mut cnt = 0u32;
    for y in 12..20 {
        for x in 44..52 {
            let idx = ((y * w + x) * 4) as usize;
            let r = heal_asset.to_rgba8_vec()[idx];
            let b = heal_asset.to_rgba8_vec()[idx + 2];
            min_r = min_r.min(r);
            max_r = max_r.max(r);
            sum_r += u32::from(r);
            sum_b += u32::from(b);
            cnt += 1;
        }
    }
    assert!(
        (sum_r / cnt) > (sum_b / cnt) + 80,
        "Healing brush must match destination warm tone, not paste dark blue (avg_r={}, avg_b={})",
        sum_r / cnt,
        sum_b / cnt
    );
    assert!(
        max_r.saturating_sub(min_r) >= 20,
        "Healing brush must transfer high-frequency texture variance (min_r={min_r}, max_r={max_r})"
    );

    // 2. Dodge & Burn across Shadows, Midtones, Highlights
    let mut db_rgba = Vec::with_capacity(60 * 20 * 4);
    for _y in 0..20 {
        for x in 0..60 {
            let v = if x < 20 {
                35u8 // Shadows band
            } else if x < 40 {
                128u8 // Midtones band
            } else {
                225u8 // Highlights band
            };
            db_rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let base_db = RasterAsset::new(60, 20, db_rgba, 96.0, ColorMode::Rgb).unwrap();
    let stroke = [Point2D::new(5.0, 10.0), Point2D::new(55.0, 10.0)];

    // Dodge Shadows: dark band must brighten far more than highlights band
    let mut dodge_sh = base_db.clone();
    apply_dodge_burn_stroke(
        &mut dodge_sh,
        &stroke,
        6.0,
        0.8,
        DodgeBurnMode::Dodge,
        ToneRange::Shadows,
        0.8,
    );
    let sh_delta = i32::from(dodge_sh.to_rgba8_vec()[(10 * 60 + 10) * 4]) - 35;
    let hi_delta = i32::from(dodge_sh.to_rgba8_vec()[(10 * 60 + 50) * 4]) - 225;
    assert!(
        sh_delta > 30 && sh_delta > hi_delta * 5,
        "Dodge(Shadows) must strongly lift shadows vs highlights: sh_delta={sh_delta}, hi_delta={hi_delta}"
    );

    // Burn Highlights: bright band must darken far more than dark shadows band
    let mut burn_hi = base_db.clone();
    apply_dodge_burn_stroke(
        &mut burn_hi,
        &stroke,
        6.0,
        0.8,
        DodgeBurnMode::Burn,
        ToneRange::Highlights,
        0.8,
    );
    let burn_hi_delta = 225 - i32::from(burn_hi.to_rgba8_vec()[(10 * 60 + 50) * 4]);
    let burn_sh_delta = 35 - i32::from(burn_hi.to_rgba8_vec()[(10 * 60 + 10) * 4]);
    assert!(
        burn_hi_delta > 40 && burn_hi_delta > burn_sh_delta * 5,
        "Burn(Highlights) must strongly darken highlights vs shadows: hi={burn_hi_delta}, sh={burn_sh_delta}"
    );

    // Dodge Midtones: midtone band rises cleanly
    let mut dodge_mid = base_db;
    apply_dodge_burn_stroke(
        &mut dodge_mid,
        &stroke,
        6.0,
        0.8,
        DodgeBurnMode::Dodge,
        ToneRange::Midtones,
        0.7,
    );
    assert!(dodge_mid.to_rgba8_vec()[(10 * 60 + 30) * 4] > 165);

    // 3. Contiguous Scanline Flood Fill (Paint Bucket) with tolerance & large image stress test
    let fw = 64u32;
    let fh = 64u32;
    let mut fb = vec![240u8; (fw * fh * 4) as usize];
    for px in fb.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    // Draw a closed black box border at x=16..=47, y=16..=47 (30x30 interior = 900 pixels)
    for x in 16..=47 {
        for y in [16u32, 47u32] {
            let idx = ((y * fw + x) * 4) as usize;
            fb[idx..idx + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
    }
    for y in 16..=47 {
        for x in [16u32, 47u32] {
            let idx = ((y * fw + x) * 4) as usize;
            fb[idx..idx + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
    }
    // Slightly noisy interior pixel (225, 225, 225) within tolerance 20 of (240,240,240)
    let n_idx = ((25 * fw + 25) * 4) as usize;
    fb[n_idx..n_idx + 4].copy_from_slice(&[225, 225, 225, 255]);

    let mut bucket_asset = RasterAsset::new(fw, fh, fb, 96.0, ColorMode::Rgb).unwrap();
    assert_eq!(
        raster_flood_fill(&mut bucket_asset, 999, 999, Color::CYAN, 10),
        0
    );
    let filled = raster_flood_fill(
        &mut bucket_asset,
        30,
        30,
        Color::rgba8(220, 40, 80, 255),
        20,
    );
    assert_eq!(
        filled,
        30 * 30,
        "Exact enclosed 30x30 pocket must be filled"
    );
    assert_eq!(
        &bucket_asset.to_rgba8_vec()[n_idx..n_idx + 4],
        &[220, 40, 80, 255],
        "Pixel within tolerance=20 must be filled"
    );
    // Exterior pixel outside border must stay untouched (240, 240, 240)
    assert_eq!(&bucket_asset.to_rgba8_vec()[0..4], &[240, 240, 240, 255]);

    // Large 512x512 flood fill confirms zero stack overflow
    let mut big =
        RasterAsset::new(512, 512, vec![255u8; 512 * 512 * 4], 96.0, ColorMode::Rgb).unwrap();
    let big_count = raster_flood_fill(&mut big, 256, 256, Color::rgba8(10, 20, 30, 255), 0);
    assert_eq!(big_count, 512 * 512);
}

#[test]
fn all_ten_postscript_procedural_fills_render_crisply_at_multiple_dpis() {
    let fg = Color::rgba8(20, 30, 180, 255);
    let bg = Color::rgba8(245, 240, 220, 255);
    let mut fingerprints: Vec<Vec<u8>> = Vec::with_capacity(10);

    for preset in PostScriptFillPreset::ALL {
        let mut doc = Document::new("PSFill", 40.0, 40.0, ColorMode::Rgb, 96.0);
        doc.add_object(
            rect_obj(
                0.0,
                0.0,
                40.0,
                40.0,
                preset.to_fill(fg.clone(), bg.clone(), 0),
            ),
            None,
        )
        .unwrap();
        let img = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
        let fg_count = img
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[2] > 120 && px[0] < 100)
            .count();
        let bg_count = img
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] > 180 && px[1] > 180)
            .count();
        assert!(
            fg_count > 100 && bg_count > 100,
            "PostScript preset {preset:?} must render both fg and bg pixels (fg={fg_count}, bg={bg_count})"
        );
        fingerprints.push(img.rgba);
    }

    // All 10 presets must produce distinct rendered images
    for i in 0..fingerprints.len() {
        for j in (i + 1)..fingerprints.len() {
            assert_ne!(
                fingerprints[i],
                fingerprints[j],
                "PostScript presets {:?} and {:?} rendered identically",
                PostScriptFillPreset::ALL[i],
                PostScriptFillPreset::ALL[j]
            );
        }
    }

    // High-DPI (300 DPI) crispness check on HexGrid
    let mut hi_doc = Document::new("PSHiDpi", 20.0, 20.0, ColorMode::Rgb, 300.0);
    hi_doc
        .add_object(
            rect_obj(
                0.0,
                0.0,
                20.0,
                20.0,
                PostScriptFillPreset::HexGrid.to_fill(fg, bg, 7),
            ),
            None,
        )
        .unwrap();
    let hi_img = render_page_to_rgba(&hi_doc, 0, 300.0, ViewMode::Enhanced).unwrap();
    assert!(hi_img.width > 200 && hi_img.height > 200);
}

#[test]
fn interactive_font_glyph_catalog_filters_search_and_bounds() {
    // 1. Every GlyphBlockFilter returns non-empty catalog entries
    for filter in GlyphBlockFilter::ALL_FILTERS {
        let items = list_glyphs_for_block(None, filter, "", 50);
        assert!(
            !items.is_empty(),
            "GlyphBlockFilter::{filter:?} returned no entries"
        );
        assert!(items.len() <= 50);
        if filter != GlyphBlockFilter::All {
            assert!(items.iter().all(|e| e.block == filter));
        }
    }

    // 2. Arabic block contains Alef, Kashida, and Tashkeel; Presentation Forms contains Allah/Bismillah
    let arabic = list_glyphs_for_block(None, GlyphBlockFilter::Arabic, "", 200);
    assert!(arabic
        .iter()
        .any(|e| e.ch == 'ا' && e.codepoint_hex == "U+0627"));
    assert!(arabic.iter().any(|e| e.ch == '\u{0640}'));

    let ar_forms = list_glyphs_for_block(None, GlyphBlockFilter::ArabicPresentationForms, "", 200);
    assert!(ar_forms.iter().any(|e| e.ch == '\u{FDF2}')); // Allah ligature
    assert!(ar_forms.iter().any(|e| e.ch == '\u{FEFB}')); // Lam-Alef ligature

    // 3. Search by name, hex codepoint, and direct glyph char
    let by_name = list_glyphs_for_block(None, GlyphBlockFilter::All, "alef", 50);
    assert!(by_name.iter().any(|e| e.ch == 'ا'));
    let by_hex = list_glyphs_for_block(None, GlyphBlockFilter::Arrows, "2192", 50);
    assert_eq!(by_hex.len(), 1);
    assert_eq!(by_hex[0].ch, '\u{2192}');
    let by_char = list_glyphs_for_block(None, GlyphBlockFilter::All, "★", 10);
    assert!(by_char.iter().any(|e| e.ch == '★'));

    // 4. max_items = 0 returns empty; TextEngine method matches free function
    assert!(list_glyphs_for_block(None, GlyphBlockFilter::All, "", 0).is_empty());
    let mut engine = TextEngine::new();
    let via_engine = engine.list_glyphs_for_block(None, GlyphBlockFilter::Math, "infinity", 10);
    assert!(via_engine.iter().any(|e| e.ch == '∞'));
}

#[test]
fn regression_8_audit_fixes_lenses_effects_arabic_diacritics_mesh_powerclip_and_degenerate_matrices(
) {
    // 1. Magnify Lens with `remove_face = true`: magnified artwork extending outside the
    //    unmagnified object's original bounds must remain VISIBLE (not clipped by unwarped mask!),
    //    while blank white page areas stay transparent/untouched!
    let mut doc = Document::new("MagnifyRemoveFace", 100.0, 100.0, ColorMode::Rgb, 96.0);
    // Small 10x10 red square centered at (50, 50) -> spans [45..55, 45..55]
    doc.add_object(
        rect_obj(
            45.0,
            45.0,
            10.0,
            10.0,
            Fill::Solid(Color::rgba8(220, 20, 20, 255)),
        ),
        None,
    )
    .unwrap();
    // 3x Magnify lens covering [20..80, 20..80] -> magnified square should span [35..65, 35..65]!
    let mut mag_lens = rect_obj(20.0, 20.0, 60.0, 60.0, Fill::None);
    mag_lens.lens = Some(LensConfig {
        kind: LensKind::Magnify { amount: 3.0 },
        frozen: false,
        viewpoint: None,
        remove_face: true,

        frozen_snapshot: None,
    });
    doc.add_object(mag_lens, None).unwrap();

    let img_mag = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    // At (40, 50): outside original [45..55], INSIDE 3x magnified [35..65] -> MUST BE RED!
    let magnified_edge = pixel_at_mm(&img_mag, 40.0, 50.0);
    assert!(
        magnified_edge[0] > 180 && magnified_edge[1] < 60,
        "Magnify+remove_face must keep magnified pixels at (40,50), got {magnified_edge:?}"
    );
    // At (24, 24): blank page outside magnified area -> MUST REMAIN WHITE!
    let blank_corner = pixel_at_mm(&img_mag, 24.0, 24.0);
    assert_eq!(blank_corner, [255, 255, 255, 255]);

    // 2. Degenerate singular matrices, NaN opacity, NaN Envelope cage, and zero-size shapes never panic
    let mut deg_doc = Document::new("Degenerate", 80.0, 80.0, ColorMode::Rgb, 96.0);
    let mut singular = rect_obj(10.0, 10.0, 20.0, 20.0, Fill::Solid(Color::CYAN));
    singular.transform = Affine2D::scale(0.0, 0.0);
    deg_doc.add_object(singular, None).unwrap();

    let mut nan_op = rect_obj(20.0, 20.0, 20.0, 20.0, Fill::Solid(Color::MAGENTA));
    nan_op.opacity = f32::NAN;
    deg_doc.add_object(nan_op, None).unwrap();

    let mut nan_env = rect_obj(30.0, 30.0, 20.0, 20.0, Fill::Solid(Color::YELLOW));
    nan_env.effects.push(Effect::Envelope {
        mode: EnvelopeMode::Straight,
        cage: vec![
            Point2D::new(f64::NAN, 0.0),
            Point2D::new(20.0, 0.0),
            Point2D::new(20.0, 20.0),
            Point2D::new(0.0, 20.0),
        ],
    });
    deg_doc.add_object(nan_env, None).unwrap();

    let deg_img = render_page_to_rgba(&deg_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    assert_eq!(deg_img.width, (80.0 * 96.0 / 25.4_f64).round() as u32);

    // 3. Arabic RTL with combining diacritics (tashkeel) on a closed circular path + letter spacing
    let circle_host = ParametricShape::Ellipse {
        rx: 25.0,
        ry: 25.0,
        mode: EllipseMode::Full,
        start_deg: 0.0,
        end_deg: 360.0,
    }
    .to_bezier_path()
    .transformed(&Affine2D::translate(50.0, 50.0));
    let vocalized_arabic = ArtisticText::from_plain(
        "بِسْمِ اللَّهِ",
        TextStyle {
            font_size_pt: 18.0,
            direction: TextDirection::Rtl,
            letter_spacing_mm: 1.5,
            ..TextStyle::default()
        },
    );
    let top_closed = TextOnPath {
        text: vocalized_arabic.clone(),
        path_id: ObjectId(1),
        offset_mm: 140.0, // wraps around closed circle seam cleanly!
        distance_mm: 2.0,
        flip_vertical: false,
        flip_horizontal: false,
    };
    let top_curves = text_on_path_to_curves(&top_closed, &circle_host);
    assert!(top_curves.is_finite());
    assert!(
        !top_curves.is_empty(),
        "Arabic text with tashkeel wrapping across closed circle seam must emit curves"
    );
    let spaced_curves = text_to_curves(&vocalized_arabic);
    assert!(spaced_curves.is_finite() && !spaced_curves.is_empty());

    // 4. Sparse MeshFill (single red node on 2x2 grid) must fill unpopulated cells from nearest
    //    supplied node instead of opaque white!
    let mut mesh_doc = Document::new("SparseMesh", 40.0, 40.0, ColorMode::Rgb, 96.0);
    let sparse_mesh = Fill::MeshFill {
        rows: 2,
        cols: 2,
        nodes: vec![MeshNode {
            row: 0,
            col: 0,
            pos: Point2D::new(0.0, 0.0),
            color: Color::rgba8(200, 30, 30, 255),

            alpha: 1.0,
            handle_n: None,
            handle_s: None,
            handle_e: None,
            handle_w: None,
        }],
    };
    mesh_doc
        .add_object(rect_obj(5.0, 5.0, 30.0, 30.0, sparse_mesh), None)
        .unwrap();
    let mesh_img = render_page_to_rgba(&mesh_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let far_corner = pixel_at_mm(&mesh_img, 30.0, 30.0);
    assert!(
        far_corner[0] > 170 && far_corner[1] < 60 && far_corner[2] < 60,
        "Sparse MeshFill must inherit nearest node color (red) instead of white: {far_corner:?}"
    );

    // 5. PowerClip container overlay effect (Contour::Inside) renders visibly!
    let mut pc_doc = Document::new("PowerClipOverlay", 60.0, 60.0, ColorMode::Rgb, 96.0);
    let mut container = rect_obj(
        10.0,
        10.0,
        40.0,
        40.0,
        Fill::Solid(Color::rgba8(20, 40, 200, 255)),
    );
    container.effects.push(Effect::Contour {
        mode: ContourMode::Inside,
        steps: 2,
        offset_mm: 4.0,
        fill_color: Color::rgba8(240, 220, 20, 255),
        outline_color: Color::TRANSPARENT,
    });
    let cid = pc_doc.add_object(container, None).unwrap();
    let inner = pc_doc
        .add_object(
            rect_obj(
                12.0,
                12.0,
                8.0,
                8.0,
                Fill::Solid(Color::rgba8(20, 200, 40, 255)),
            ),
            None,
        )
        .unwrap();
    pc_doc.create_powerclip(cid, &[inner]).unwrap();
    let pc_img = render_page_to_rgba(&pc_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let pc_center = pixel_at_mm(&pc_img, 30.0, 30.0);
    assert!(
        pc_center[0] > 180 && pc_center[1] > 160,
        "PowerClip Contour::Inside overlay must paint yellow center: {pc_center:?}"
    );

    // 6. Moved standalone Connector renders AND hit-tests at its transformed position!
    let mut conn_doc = Document::new("MovedConn", 100.0, 60.0, ColorMode::Rgb, 96.0);
    let conn = ConnectorObject {
        from_obj: None,
        from_anchor: AnchorPoint::Center,
        from_fallback: Point2D::new(5.0, 10.0),
        to_obj: None,
        to_anchor: AnchorPoint::Center,
        to_fallback: Point2D::new(35.0, 10.0),
        style: ConnectorStyle::Straight,
        corner_radius_mm: 0.0,
    };
    let conn_id = conn_doc
        .add_object(
            SceneObject::new(ObjectId(0), "MovedLink", NodePayload::Connector(conn))
                .with_outline(OutlineStyle::solid(Color::rgba8(220, 20, 20, 255), 2.5))
                .with_transform(Affine2D::translate(40.0, 25.0)),
            None,
        )
        .unwrap();
    // Transformed line runs from (45, 35) to (75, 35)
    assert_eq!(
        hit_test_point(&conn_doc, 0, Point2D::new(60.0, 35.0), 2.0),
        Some(conn_id)
    );
    assert_eq!(
        hit_test_point(&conn_doc, 0, Point2D::new(20.0, 10.0), 2.0),
        None
    );
    let conn_img = render_page_to_rgba(&conn_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let conn_px = pixel_at_mm(&conn_img, 60.0, 35.0);
    assert!(
        conn_px[0] > 180 && conn_px[1] < 60,
        "Transformed standalone Connector must render red at (60,35): {conn_px:?}"
    );
}

#[test]
fn r16_text_on_path_nested_group_and_convert_to_bitmap() {
    let mut doc = Document::new("R16TOPGroup", 220.0, 250.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    // Host line y = 20 in [0..80], TextOnPath inside Translate(70, 90) Group -> world y = 110 (never 200!)
    let host_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "HostLine",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 20.0),
                    Point2D::new(80.0, 20.0),
                )),
            )
            .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();
    let style = TextStyle {
        font_size_pt: 24.0,
        fill: Color::rgba8(200, 10, 10, 255),
        ..TextStyle::default()
    };
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("HELLO", style.clone()),
                    path_id: host_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();
    let grp_id = doc.group_objects(&[host_id, top_id], "TOPGrp").unwrap();
    doc.transform_object(grp_id, Affine2D::translate(70.0, 90.0))
        .unwrap();

    let bmp = convert_object_to_bitmap(&doc, top_id, 96.0).unwrap();
    let non_zero_bmp = bmp
        .to_rgba8_vec()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 20)
        .count();
    assert!(
        non_zero_bmp > 50,
        "convert_object_to_bitmap on nested TextOnPath must have > 50 ink pixels, got {non_zero_bmp}"
    );

    let img = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let px_per_mm: f64 = 96.0 / 25.4;
    let mut ink_near_110 = 0usize;
    let mut ink_near_200 = 0usize;
    for py in 0..img.height {
        let y_mm = f64::from(py) / px_per_mm;
        for px in 0..img.width {
            let x_mm = f64::from(px) / px_per_mm;
            let i = ((py * img.width + px) * 4) as usize;
            let [r, g, b, _] = [
                img.rgba[i],
                img.rgba[i + 1],
                img.rgba[i + 2],
                img.rgba[i + 3],
            ];
            if r > 180 && g < 100 && b < 100 && (70.0..=160.0).contains(&x_mm) {
                if (95.0..=118.0).contains(&y_mm) {
                    ink_near_110 += 1;
                }
                if (185.0..=210.0).contains(&y_mm) {
                    ink_near_200 += 1;
                }
            }
        }
    }
    assert!(
        ink_near_110 > 30,
        "nested TextOnPath must render at y ~ 110 mm, got {ink_near_110}"
    );
    assert_eq!(
        ink_near_200, 0,
        "nested TextOnPath must not double-translate to y ~ 200 mm"
    );

    // Rotated Group variant + effects on TextOnPath
    let mut top_eff = doc.object(top_id).unwrap().clone();
    top_eff.effects.push(Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 1.0,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    let eff_top = doc.add_object(top_eff, Some(lid)).unwrap();
    let rot_grp = doc.group_objects(&[eff_top], "RotTOPGrp").unwrap();
    doc.transform_object(
        rot_grp,
        Affine2D::translate(30.0, 40.0).compose(&Affine2D::rotate(0.35)),
    )
    .unwrap();
    let eff_bmp = convert_object_to_bitmap(&doc, eff_top, 96.0).unwrap();
    let eff_ink = eff_bmp
        .to_rgba8_vec()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 20)
        .count();
    assert!(
        eff_ink > 50,
        "nested TextOnPath with effects must render into bitmap, got {eff_ink}"
    );
}

#[test]
fn r16_isolated_group_and_transparency_with_connector_and_symbol() {
    // 1. Isolated Group (opacity = 0.5) containing a Connector nested inside Translate(60, 60) Group
    let mut doc = Document::new("R16IsoGroupConn", 160.0, 160.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;
    let mut conn = ConnectorObject::between_objects(
        ObjectId(999),
        AnchorPoint::Center,
        ObjectId(999),
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn.from_obj = None;
    conn.to_obj = None;
    conn.from_fallback = Point2D::new(10.0, 20.0);
    conn.to_fallback = Point2D::new(50.0, 20.0);
    let conn_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Conn", NodePayload::Connector(conn))
                .with_outline(OutlineStyle::solid(Color::rgba8(220, 20, 20, 255), 3.0)),
            Some(lid),
        )
        .unwrap();
    let inner_id = doc.group_objects(&[conn_id], "IsoGrp").unwrap();
    coree_core::CommandHistory::new(8)
        .execute(
            &mut doc,
            coree_core::CommandOp::SetOpacity {
                ids: vec![inner_id],
                opacity: 0.5,
            },
        )
        .unwrap();
    let outer_id = doc.group_objects(&[inner_id], "OuterGrp").unwrap();
    doc.transform_object(outer_id, Affine2D::translate(60.0, 60.0))
        .unwrap();

    let bmp = convert_object_to_bitmap(&doc, inner_id, 96.0).unwrap();
    let max_alpha = bmp
        .to_rgba8_vec()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3])
        .max()
        .unwrap_or(0);
    assert!(
        (100..=145).contains(&max_alpha),
        "isolated Group opacity 0.5 containing Connector must have max_alpha in [100, 145], got {max_alpha}"
    );

    // 2. Isolated Group (opacity = 0.5) AND Effect::Transparency inside a SymbolDef
    let mut sdoc = Document::new("R16SymIso", 120.0, 120.0, ColorMode::Rgb, 96.0);
    let slid = sdoc.pages()[0].layers[0].id;
    let child_rect = SceneObject::new(
        ObjectId(10),
        "SymChild",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(20, 40, 220, 255)))
    .with_outline(OutlineStyle::none());
    let mut grp_in_sym = SceneObject::new(
        ObjectId(11),
        "SymIsoGrp",
        NodePayload::Group(vec![ObjectId(10)]),
    );
    grp_in_sym.opacity = 0.5;
    let mut trans_rect = SceneObject::new(
        ObjectId(12),
        "SymTransRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(20, 200, 40, 255)))
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(45.0, 0.0));
    trans_rect.effects.push(Effect::Transparency {
        mode: TransparencyMode::Uniform(0.5),
        blend_mode: BlendMode::Normal,
    });
    let sym = SymbolDef::new("IsoSym", vec![child_rect, grp_in_sym, trans_rect]);
    let sym_id = sym.id;
    sdoc.upsert_symbol(sym);
    let inst_id = sdoc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            Some(slid),
        )
        .unwrap();
    let sym_bmp = convert_object_to_bitmap(&sdoc, inst_id, 96.0).unwrap();
    let sym_max_alpha = sym_bmp
        .to_rgba8_vec()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3])
        .max()
        .unwrap_or(0);
    assert!(
        sym_max_alpha > 80 && sym_max_alpha <= 145,
        "SymbolInstance with opacity=0.5 Group & Uniform(0.5) Transparency must cap alpha <= 145, got {sym_max_alpha}"
    );

    // 3. render_isolated_object_to_rgba on ArtisticText with obj.transform + nested Connector
    let at_id = sdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TransformedAT",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "EXPORT",
                    TextStyle {
                        font_size_pt: 20.0,
                        fill: Color::BLACK,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(35.0, 60.0)),
            Some(slid),
        )
        .unwrap();
    let (at_bb, at_buf) =
        coree_render::render_isolated_object_to_rgba(&sdoc, 0, at_id, Affine2D::IDENTITY, 96.0)
            .unwrap()
            .expect("ArtisticText isolated render");
    assert!(
        at_bb.min_x >= 20.0 && at_bb.min_y >= 30.0,
        "isolated ArtisticText bounds must include obj.transform: {at_bb:?}"
    );
    let at_ink = at_buf
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 20)
        .count();
    assert!(at_ink > 30, "isolated ArtisticText must render visible ink");

    let (conn_bb, conn_buf) = coree_render::render_isolated_object_to_rgba(
        &doc,
        0,
        conn_id,
        Affine2D::translate(60.0, 60.0),
        96.0,
    )
    .unwrap()
    .expect("nested Connector isolated render");
    assert!(
        (conn_bb.min_x - 60.0).abs() < 15.0 && (conn_bb.min_y - 70.0).abs() < 15.0,
        "nested Connector isolated bounds must not double-apply parent_xf: {conn_bb:?}"
    );
    let conn_ink = conn_buf
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 20)
        .count();
    assert!(conn_ink > 30, "nested Connector must render visible ink");
}

#[test]
fn r16_hit_test_non_aabb_rotated_and_symbol_and_rect() {
    let rot45 =
        Affine2D::translate(50.0, 50.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_4));
    // Center of a 100x10 bar transformed by rot45: (50,5) -> (50 + 50*cos45 - 5*sin45, 50 + 50*sin45 + 5*cos45)
    let on_bar = rot45.transform_point(Point2D::new(50.0, 5.0));
    // Empty corner inside world AABB of the 45-deg rotated 100x10 bar
    let empty_aabb_corner = Point2D::new(50.0, 120.0);

    // Test rotated Raster, ArtisticText, ParagraphText, Table, and SymbolInstance
    for kind in 0..5 {
        let mut doc = Document::new("R16HitNonAABB", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let obj = match kind {
            0 => {
                let asset =
                    RasterAsset::new(10, 10, vec![255u8; 400], 96.0, ColorMode::Rgb).unwrap();
                let aid = doc.register_asset(asset);
                SceneObject::new(
                    ObjectId(0),
                    "RotRaster",
                    NodePayload::Raster(RasterLayerData {
                        asset_id: aid,
                        width_mm: 100.0,
                        height_mm: 10.0,
                        crop_rect: None,
                        filters: Vec::new(),
                        filter_enabled: Vec::new(),
                    }),
                )
            }
            1 => SceneObject::new(
                ObjectId(0),
                "RotAT",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "WIDE TEXT BAR",
                    TextStyle {
                        font_size_pt: 24.0,
                        ..TextStyle::default()
                    },
                )),
            ),
            2 => SceneObject::new(
                ObjectId(0),
                "RotPara",
                NodePayload::ParagraphText(ParagraphText::new(
                    "Para Bar",
                    Vec2D::new(100.0, 10.0),
                    TextStyle::default(),
                )),
            ),
            3 => SceneObject::new(
                ObjectId(0),
                "RotTable",
                NodePayload::Table(VectorTable::new(1, 2, 50.0, 10.0)),
            ),
            _ => {
                // SymbolInstance with two separated 10x10 rects at x=0 and x=90
                let left = SceneObject::new(
                    ObjectId(1),
                    "Left",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 10.0, 10.0,
                    ))),
                )
                .with_fill(Fill::Solid(Color::BLACK));
                let right = SceneObject::new(
                    ObjectId(2),
                    "Right",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        90.0, 0.0, 10.0, 10.0,
                    ))),
                )
                .with_fill(Fill::Solid(Color::BLACK));
                let grp = SceneObject::new(
                    ObjectId(3),
                    "SymGrp",
                    NodePayload::Group(vec![ObjectId(1), ObjectId(2)]),
                );
                let sym = SymbolDef::new("SplitSym", vec![left, right, grp]);
                let sym_id = sym.id;
                doc.upsert_symbol(sym);
                SceneObject::new(ObjectId(0), "RotSym", NodePayload::SymbolInstance(sym_id))
            }
        };
        let oid = doc
            .add_object(obj.with_transform(rot45), Some(lid))
            .unwrap();

        if kind == 4 {
            // SymbolInstance: left & right rects hit, gap at x=50 misses, empty corner misses!
            let on_left = rot45.transform_point(Point2D::new(5.0, 5.0));
            assert_eq!(hit_test_point(&doc, 0, on_left, 0.5), Some(oid));
            assert_eq!(
                hit_test_point(&doc, 0, on_bar, 0.5),
                None,
                "SymbolInstance gap must not hit"
            );
            assert_eq!(hit_test_point(&doc, 0, empty_aabb_corner, 0.5), None);
        } else {
            if kind == 1 {
                // ArtisticText estimated bounds center
                let at_center = match &doc.object(oid).unwrap().payload {
                    NodePayload::ArtisticText(at) => {
                        rot45.transform_point(at.estimated_bounds().center())
                    }
                    _ => unreachable!(),
                };
                assert_eq!(hit_test_point(&doc, 0, at_center, 0.5), Some(oid));
            } else if kind == 2 {
                // Unfilled ParagraphText ("Para Bar" at x=0..40.6 inside 100x10 frame):
                // hits on wrapped text, misses empty interior at x=75
                let on_para_text = rot45.transform_point(Point2D::new(20.0, 5.0));
                let empty_para_interior = rot45.transform_point(Point2D::new(75.0, 5.0));
                assert_eq!(
                    hit_test_point(&doc, 0, on_para_text, 0.5),
                    Some(oid),
                    "kind 2 rotated ParagraphText text span must hit"
                );
                assert_eq!(
                    hit_test_point(&doc, 0, empty_para_interior, 0.5),
                    None,
                    "kind 2 unfilled ParagraphText empty frame interior must NOT hit"
                );
            } else {
                assert_eq!(
                    hit_test_point(&doc, 0, on_bar, 0.5),
                    Some(oid),
                    "kind {kind} rotated body must hit"
                );
            }
            assert_eq!(
                hit_test_point(&doc, 0, empty_aabb_corner, 0.5),
                None,
                "kind {kind} empty rotated AABB corner must NOT false-hit"
            );
        }

        // hit_test_rect(..., false) must ignore empty AABB corner and catch intersecting marquee
        let empty_marquee = Rect2D::from_xywh(48.0, 118.0, 4.0, 4.0);
        assert!(
            hit_test_rect(&doc, 0, empty_marquee, false).is_empty(),
            "kind {kind} empty AABB corner marquee must return empty"
        );
        let hit_pt = if kind == 4 {
            rot45.transform_point(Point2D::new(5.0, 5.0))
        } else if kind == 2 {
            rot45.transform_point(Point2D::new(20.0, 5.0))
        } else if kind == 1 {
            match &doc.object(oid).unwrap().payload {
                NodePayload::ArtisticText(at) => {
                    rot45.transform_point(at.estimated_bounds().center())
                }
                _ => unreachable!(),
            }
        } else {
            on_bar
        };
        let hit_marquee = Rect2D::from_xywh(hit_pt.x - 1.0, hit_pt.y - 1.0, 2.0, 2.0);
        assert_eq!(hit_test_rect(&doc, 0, hit_marquee, false), vec![oid]);
    }

    // Diagonal TextOnPath inside Translate(30, 20) Group
    let mut tdoc = Document::new("R16HitTOP", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let tlid = tdoc.pages()[0].layers[0].id;
    let host_id = tdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "DiagHost",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(100.0, 100.0),
                )),
            ),
            Some(tlid),
        )
        .unwrap();
    let top_id = tdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "DiagTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("DIAGONAL", TextStyle::default()),
                    path_id: host_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(tlid),
        )
        .unwrap();
    let grp_id = tdoc.group_objects(&[host_id, top_id], "DiagGrp").unwrap();
    tdoc.transform_object(grp_id, Affine2D::translate(30.0, 20.0))
        .unwrap();
    // On diagonal (80, 70) -> hits grp_id; at empty AABB corner (120, 25) -> None!
    assert_eq!(
        hit_test_point(&tdoc, 0, Point2D::new(80.0, 70.0), 1.0),
        Some(grp_id)
    );
    assert_eq!(
        hit_test_point(&tdoc, 0, Point2D::new(120.0, 25.0), 1.0),
        None
    );
}

#[test]
fn r17_transparency_fountain_linear_and_radial_with_transformed_and_grouped_objects() {
    let mut doc = Document::new("R17 Transparency", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    // 1. FountainLinear on object with non-identity obj.transform = translate(80, 80)
    let mut lin_obj = rect_obj(
        80.0,
        80.0,
        80.0,
        40.0,
        Fill::Solid(Color::rgba8(220, 30, 30, 255)),
    );
    lin_obj.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::new(0.0, 20.0),
            end: Point2D::new(80.0, 20.0),
            start_alpha: 1.0,
            end_alpha: 0.0,
        },
        blend_mode: BlendMode::Normal,
    });
    doc.add_object(lin_obj, Some(lid)).unwrap();

    // 2. FountainRadial inside a rotated + translated Group
    let mut rad_obj = rect_obj(
        0.0,
        0.0,
        40.0,
        40.0,
        Fill::Solid(Color::rgba8(30, 30, 220, 255)),
    );
    rad_obj.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainRadial {
            center: Point2D::new(20.0, 20.0),
            radius: 20.0,
            inner_alpha: 1.0,
            outer_alpha: 0.0,
        },
        blend_mode: BlendMode::Normal,
    });
    let rad_id = doc.add_object(rad_obj, Some(lid)).unwrap();
    let grp_id = doc.group_objects(&[rad_id], "RotTransGrp").unwrap();
    doc.transform_object(
        grp_id,
        Affine2D::translate(40.0, 30.0).compose(&Affine2D::rotate_deg(45.0)),
    )
    .unwrap();

    let mut opts = RenderOptions::export();
    opts.page_bg = Color::TRANSPARENT;
    let img = render_page_to_rgba_with_options(&doc, 0, 25.4, ViewMode::Enhanced, &opts).unwrap();
    assert_eq!((img.width, img.height), (200, 200));

    let sample = |buf: &[u8], w: u32, x: u32, y: u32| -> [u8; 4] {
        let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };

    // Over white page sheet: FountainLinear left edge (82, 100) is red; right edge (158, 100) fades to near-white
    let left_px = sample(&img.rgba, img.width, 82, 100);
    let right_px = sample(&img.rgba, img.width, 158, 100);
    assert!(
        left_px[0] > 180 && left_px[1] < 60,
        "FountainLinear left edge must be opaque red, got {left_px:?}"
    );
    assert!(
        right_px[1] > 235 && right_px[2] > 235,
        "FountainLinear right edge must fade to near-white over paper, got {right_px:?}"
    );

    // Also render onto a transparent surface directly to verify true alpha ramp [>200 -> <40]
    let lin_id = doc.pages()[0].layers[0].objects[0];
    let mut tback = TinySkiaBackend::new(200, 200).unwrap();
    tback.clear(Color::TRANSPARENT);
    SceneRenderer::new().render_single_object(
        &doc,
        lin_id,
        &Viewport::for_export(200, 200, 25.4),
        ViewMode::Enhanced,
        &opts,
        &mut tback,
    );
    let tbuf = tback.into_rgba();
    let t_left = sample(&tbuf.rgba, tbuf.width, 82, 100);
    let t_right = sample(&tbuf.rgba, tbuf.width, 158, 100);
    assert!(
        t_left[3] > 200 && t_left[0] > 180,
        "transparent FountainLinear left edge must be opaque red, got {t_left:?}"
    );
    assert!(
        t_right[3] < 40,
        "transparent FountainLinear right edge must fade to low alpha, got {t_right:?}"
    );

    // FountainRadial inside rotated Group: world center of rotated [0..40, 0..40] around (40,30)
    let mut rback = TinySkiaBackend::new(200, 200).unwrap();
    rback.clear(Color::TRANSPARENT);
    SceneRenderer::new().render_single_object(
        &doc,
        grp_id,
        &Viewport::for_export(200, 200, 25.4),
        ViewMode::Enhanced,
        &opts,
        &mut rback,
    );
    let rbuf = rback.into_rgba();
    let rad_center = doc.world_object_bounds(rad_id).unwrap().center();
    let cx = rad_center.x.round() as u32;
    let cy = rad_center.y.round() as u32;
    let center_px = sample(&rbuf.rgba, rbuf.width, cx, cy);
    assert!(
        center_px[3] > 200 && center_px[2] > 180,
        "FountainRadial center at ({cx}, {cy}) must be opaque blue, got {center_px:?}"
    );
    // Corner at radius >= 20 from center must fade toward 0
    let corner = doc
        .world_transform_of(rad_id)
        .unwrap()
        .transform_point(Point2D::new(2.0, 2.0));
    let corner_px = sample(
        &rbuf.rgba,
        rbuf.width,
        corner.x.round() as u32,
        corner.y.round() as u32,
    );
    assert!(
        corner_px[3] < 40,
        "FountainRadial outer corner must fade to low alpha, got {corner_px:?}"
    );
}

#[test]
fn r17_isolated_group_600_dpi_low_zoom_and_symbol_and_symmetry_text_on_path() {
    // 1. Isolated Group (opacity = 0.5) with scale_with_object wide stroke at 600 DPI and low zoom
    let mut doc = Document::new("R17 Iso600", 50.0, 50.0, ColorMode::Rgb, 600.0);
    let lid = doc.pages()[0].layers[0].id;
    let mut wide_rect = rect_obj(
        15.0,
        15.0,
        20.0,
        20.0,
        Fill::Solid(Color::rgba8(0, 120, 255, 255)),
    );
    wide_rect.outline = OutlineStyle {
        color: Color::rgba8(255, 0, 0, 255),
        width_mm: 10.0,
        scale_with_object: true,
        ..OutlineStyle::default()
    };
    let rid = doc.add_object(wide_rect, Some(lid)).unwrap();
    let grp_id = doc.group_objects(&[rid], "IsoWideGrp").unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetOpacity {
            ids: vec![grp_id],
            opacity: 0.5,
        },
    )
    .unwrap();

    let mut opts = RenderOptions::export();
    opts.page_bg = Color::TRANSPARENT;
    let px_per_mm_600 = 600.0_f64 / 25.4;
    let dim_600 = (50.0 * px_per_mm_600).round() as u32;
    let mut back_600 = TinySkiaBackend::new(dim_600, dim_600).unwrap();
    back_600.clear(Color::TRANSPARENT);
    SceneRenderer::new().render_single_object(
        &doc,
        grp_id,
        &Viewport::for_export(dim_600, dim_600, 600.0),
        ViewMode::Enhanced,
        &opts,
        &mut back_600,
    );
    let hi_dpi = back_600.into_rgba();
    let max_alpha_600 = hi_dpi
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3])
        .max()
        .unwrap_or(0);
    assert!(
        (115..=142).contains(&max_alpha_600),
        "600 DPI isolated Group (opacity=0.5) must preserve ~128 alpha without MAX_SURFACE_DIM_PX fallback, got {max_alpha_600}"
    );

    // Low zoom (1.27 DPI -> 0.05 px/mm) on 2000mm page with 100mm wide stroke
    let mut lo_doc = Document::new("R17 LowZoom", 2000.0, 2000.0, ColorMode::Rgb, 96.0);
    let lo_lid = lo_doc.pages()[0].layers[0].id;
    let mut lo_rect = rect_obj(
        800.0,
        800.0,
        200.0,
        200.0,
        Fill::Solid(Color::rgba8(0, 200, 0, 255)),
    );
    lo_rect.outline = OutlineStyle {
        color: Color::rgba8(0, 0, 255, 255),
        width_mm: 160.0,
        scale_with_object: false,
        ..OutlineStyle::default()
    };
    let lo_rid = lo_doc.add_object(lo_rect, Some(lo_lid)).unwrap();
    let lo_grp = lo_doc.group_objects(&[lo_rid], "LowZoomIso").unwrap();
    coree_core::apply_command(
        &mut lo_doc,
        &coree_core::CommandOp::SetOpacity {
            ids: vec![lo_grp],
            opacity: 0.5,
        },
    )
    .unwrap();
    let lo_img =
        render_page_to_rgba_with_options(&lo_doc, 0, 1.27, ViewMode::Enhanced, &opts).unwrap();
    let blue_ink = lo_img
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 180 && p[2] > 180 && p[3] > 60)
        .count();
    assert!(
        blue_ink > 10,
        "low-zoom isolated Group must render wide stroke without clipping, got {blue_ink} blue px"
    );

    // 2. TextOnPath inside SymbolInstance renders on symbol's host curve (y ~ 90), not fallback origin
    let mut sdoc = Document::new("R17 SymTOP", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let sym_host_id = ObjectId(7001);
    let sym_host = SceneObject::new(
        sym_host_id,
        "SymHost",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 30.0),
            Point2D::new(90.0, 30.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    let blue_text = TextStyle {
        fill: Color::rgba8(0, 0, 220, 255),
        ..TextStyle::default()
    };
    let mut sym_top = SceneObject::new(
        ObjectId(7002),
        "SymTOP",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("SYMBOL_CURVE", blue_text.clone()),
            path_id: sym_host_id,
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 0, 220, 255)))
    .with_outline(OutlineStyle::none());
    sym_top.transform = Affine2D::translate(0.0, 0.0);
    let sym = SymbolDef::new("SymWithTOP", vec![sym_host, sym_top]);
    let sym_id = sym.id;
    sdoc.insert_symbol(sym);
    sdoc.add_object(
        SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id))
            .with_transform(Affine2D::translate(20.0, 60.0)),
        None,
    )
    .unwrap();

    let sym_img =
        render_page_to_rgba_with_options(&sdoc, 0, 25.4, ViewMode::Enhanced, &opts).unwrap();
    let ink_at_host_y90 = (75..100)
        .flat_map(|y| (20..120).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let i = (y * sym_img.width as usize + x) * 4;
            sym_img.rgba[i + 3] > 30 && sym_img.rgba[i] < 120 && sym_img.rgba[i + 2] > 150
        })
        .count();
    let ink_at_origin_y30 = (0..45)
        .flat_map(|y| (0..100).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let i = (y * sym_img.width as usize + x) * 4;
            sym_img.rgba[i + 3] > 30
                && (sym_img.rgba[i] < 245 || sym_img.rgba[i + 1] < 245 || sym_img.rgba[i + 2] < 245)
        })
        .count();
    assert!(
        ink_at_host_y90 > 20 && ink_at_origin_y30 == 0,
        "SymbolInstance TextOnPath must render at host curve y=90 (ink={ink_at_host_y90}), not origin y<=45 (ink={ink_at_origin_y30})"
    );

    // 3. TextOnPath with Effect::Symmetry (4-fold around (100, 100)) renders in all 4 quadrants
    let mut ydoc = Document::new("R17 SymEffTOP", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let ylid = ydoc.pages()[0].layers[0].id;
    let yhost = ydoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "YHost",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(30.0, 40.0),
                    Point2D::new(90.0, 40.0),
                )),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::none()),
            Some(ylid),
        )
        .unwrap();
    let mut ytop = SceneObject::new(
        ObjectId(0),
        "YSymTOP",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("SYM_PATH", blue_text),
            path_id: yhost,
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 60, 220, 255)))
    .with_outline(OutlineStyle::none());
    ytop.effects.push(Effect::Symmetry {
        axes: 4,
        mirror: true,
        origin: Point2D::new(100.0, 100.0),
    });
    ydoc.add_object(ytop, Some(ylid)).unwrap();
    let yimg = render_page_to_rgba_with_options(&ydoc, 0, 25.4, ViewMode::Enhanced, &opts).unwrap();
    let count_quad = |x0: usize, x1: usize, y0: usize, y1: usize| -> usize {
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let i = (y * yimg.width as usize + x) * 4;
                yimg.rgba[i + 3] > 30 && yimg.rgba[i] < 120 && yimg.rgba[i + 2] > 150
            })
            .count()
    };
    let tl = count_quad(10, 95, 10, 95);
    let tr = count_quad(105, 190, 10, 95);
    let bl = count_quad(10, 95, 105, 190);
    let br = count_quad(105, 190, 105, 190);
    assert!(
        tl > 20 && tr > 20 && bl > 20 && br > 20,
        "TextOnPath + Effect::Symmetry must render in all 4 quadrants (tl={tl}, tr={tr}, bl={bl}, br={br})"
    );
}

#[test]
fn r17_convert_object_to_bitmap_captures_blend_and_wide_drop_shadow() {
    // 1. Effect::Blend target + intermediate steps captured without clipping
    let mut bdoc = Document::new("R17 BlendBmp", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let lid = bdoc.pages()[0].layers[0].id;
    let target_id = bdoc
        .add_object(
            rect_obj(
                110.0,
                20.0,
                20.0,
                20.0,
                Fill::Solid(Color::rgba8(0, 0, 255, 255)),
            ),
            Some(lid),
        )
        .unwrap();
    let mut src = rect_obj(
        10.0,
        20.0,
        20.0,
        20.0,
        Fill::Solid(Color::rgba8(255, 0, 0, 255)),
    );
    src.effects.push(Effect::Blend {
        target_id,
        steps: 6,
        rotation_deg: 0.0,
        along_path: None,

        rotate_along_path: true,
    });
    let src_id = bdoc.add_object(src, Some(lid)).unwrap();

    let (blend_asset, blend_bounds) =
        convert_object_to_bitmap_with_bounds(&bdoc, src_id, 96.0).unwrap();
    assert!(
        blend_bounds.min_x <= 6.5 && blend_bounds.max_x >= 133.5,
        "Blend padded bounds {blend_bounds:?} must span source [10..30] through target [110..130]"
    );
    assert!(
        blend_asset.width >= 470,
        "Blend bitmap width must span full ~128mm at 96 DPI (>=470px), got {}",
        blend_asset.width
    );
    // Verify intermediate blend step around world x=70 is rendered in the bitmap
    let px_per_mm: f64 = 96.0 / 25.4;
    let mid_x = ((70.0 - blend_bounds.min_x) * px_per_mm).round() as u32;
    let mid_y = ((30.0 - blend_bounds.min_y) * px_per_mm).round() as u32;
    let mid_ink = ((mid_x.saturating_sub(25))..=(mid_x + 25).min(blend_asset.width - 1))
        .filter(|&x| {
            let idx = ((mid_y as usize) * (blend_asset.width as usize) + (x as usize)) * 4;
            blend_asset.to_rgba8_vec()[idx + 3] > 30
        })
        .count();
    assert!(
        mid_ink > 0,
        "Blend bitmap must contain intermediate step pixels around world x=70mm"
    );

    // 2. Wide DropShadow (dx=20, dy=15, blur=4) captured without clipping
    let mut sdoc = Document::new("R17 ShadowBmp", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let slid = sdoc.pages()[0].layers[0].id;
    let mut sh_obj = rect_obj(
        20.0,
        20.0,
        20.0,
        20.0,
        Fill::Solid(Color::rgba8(20, 180, 20, 255)),
    );
    sh_obj.effects.push(Effect::DropShadow {
        dx_mm: 20.0,
        dy_mm: 15.0,
        blur_mm: 4.0,
        opacity: 0.8,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Outside,
    });
    let sh_id = sdoc.add_object(sh_obj, Some(slid)).unwrap();
    let (sh_asset, sh_bounds) = convert_object_to_bitmap_with_bounds(&sdoc, sh_id, 96.0).unwrap();
    assert!(
        sh_bounds.max_x >= 60.0 && sh_bounds.max_y >= 55.0,
        "DropShadow padded bounds {sh_bounds:?} must cover shifted shadow [40..60, 35..55]"
    );
    let sx = ((50.0 - sh_bounds.min_x) * px_per_mm).round() as u32;
    let sy = ((45.0 - sh_bounds.min_y) * px_per_mm).round() as u32;
    let sidx = ((sy as usize) * (sh_asset.width as usize) + (sx as usize)) * 4;
    assert!(
        sh_asset.to_rgba8_vec()[sidx + 3] > 20,
        "DropShadow bitmap must contain visible shadow pixel at (50, 45), got alpha={}",
        sh_asset.to_rgba8_vec()[sidx + 3]
    );
}

#[test]
fn w1b_r18_dirty_tile_cache_group_child_drop_shadow_seam_and_symmetry_mark_object_dirty() {
    // 1. Non-isolated Group containing child with DropShadow crossing x=256px (64mm at 101.6 DPI) seam
    let mut doc = Document::new("R18 GroupShadow Seam", 210.0, 297.0, ColorMode::Rgb, 101.6);
    let lid = doc.pages()[0].layers[0].id;
    let mut child = rect_obj(
        25.0,
        80.0,
        20.0,
        20.0,
        Fill::Solid(Color::rgba8(30, 120, 220, 255)),
    );
    child.effects.push(Effect::DropShadow {
        dx_mm: 25.0,
        dy_mm: 0.0,
        blur_mm: 3.0,
        opacity: 0.85,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Outside,
    });
    let child_id = doc.add_object(child, Some(lid)).unwrap();
    let _grp_id = doc.group_objects(&[child_id], "ShadowGrp").unwrap();

    let vp = Viewport::for_export(768, 512, 101.6);
    let mut opts = RenderOptions::interactive();
    opts.show_page_shadow = false;
    opts.show_grid = false;

    let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
    let mut renderer_tiled = SceneRenderer::new();
    let tiled = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer_tiled);

    let mut direct_backend = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
    let mut renderer_direct = SceneRenderer::new();
    renderer_direct.render_document(&doc, &vp, ViewMode::Enhanced, &opts, &mut direct_backend);
    let direct = direct_backend.into_rgba();

    assert_eq!(
        tiled.rgba, direct.rgba,
        "DirtyTileCache with Group-child DropShadow crossing 256px tile seam must match direct render bit-for-bit"
    );

    // 2. Effect::Symmetry invalidation via mark_object_dirty & object_visual_world_bounds
    let mut sdoc = Document::new("R18 Symmetry Dirty", 210.0, 297.0, ColorMode::Rgb, 101.6);
    let slid = sdoc.pages()[0].layers[0].id;
    let mut sym_rect = rect_obj(
        10.0,
        70.0,
        20.0,
        20.0,
        Fill::Solid(Color::rgba8(240, 20, 20, 255)),
    );
    sym_rect.effects.push(Effect::Symmetry {
        axes: 2,
        mirror: false,
        origin: Point2D::new(80.0, 80.0),
    });
    let sid = sdoc.add_object(sym_rect, Some(slid)).unwrap();
    let vis_bb = coree_render::object_visual_world_bounds(&sdoc, sid).unwrap();
    assert!(
        vis_bb.max_x >= 150.0,
        "object_visual_world_bounds must include symmetry clone [130..150], got {vis_bb:?}"
    );

    let mut scache = DirtyTileCache::new(vp.width_px, vp.height_px);
    let mut sr1 = SceneRenderer::new();
    let _ = scache.render_viewport(&sdoc, &vp, ViewMode::Enhanced, &opts, &mut sr1);

    // Edit fill from red to blue and invalidate ONLY via mark_object_dirty
    coree_core::apply_command(
        &mut sdoc,
        &coree_core::CommandOp::SetFill {
            ids: vec![sid],
            fill: Fill::Solid(Color::rgba8(20, 30, 240, 255)),
        },
    )
    .unwrap();
    scache.mark_object_dirty(&sdoc, sid);
    let updated_tiled = scache.render_viewport(&sdoc, &vp, ViewMode::Enhanced, &opts, &mut sr1);

    let mut s_direct_backend = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
    let mut sr2 = SceneRenderer::new();
    sr2.render_document(&sdoc, &vp, ViewMode::Enhanced, &opts, &mut s_direct_backend);
    let s_direct = s_direct_backend.into_rgba();
    assert_eq!(
        updated_tiled.rgba, s_direct.rgba,
        "mark_object_dirty on Effect::Symmetry must invalidate clone tiles bit-for-bit"
    );
}

#[test]
fn w1b_r18_hit_test_connector_inside_symbol_instance() {
    use coree_core::{AnchorPoint, ConnectorObject, ConnectorStyle, SymbolDef};

    let mut doc = Document::new_a4("R18 Symbol Connector HitTest");
    let b1_id = ObjectId(9001);
    let b2_id = ObjectId(9002);
    let c_id = ObjectId(9003);
    let b1 = SceneObject::new(
        b1_id,
        "Box1",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 10.0, 10.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    let b2 = SceneObject::new(
        b2_id,
        "Box2",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            70.0, 10.0, 10.0, 10.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    let mut conn = ConnectorObject::between_objects(
        b1_id,
        AnchorPoint::Center,
        b2_id,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn.from_fallback = Point2D::ZERO;
    conn.to_fallback = Point2D::ZERO;
    let sym = SymbolDef::new(
        "SymWithConn",
        vec![
            b1,
            b2,
            SceneObject::new(c_id, "SymConn", NodePayload::Connector(conn))
                .with_outline(OutlineStyle::solid(Color::BLACK, 1.0)),
        ],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let inst_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(20.0, 40.0)),
            None,
        )
        .unwrap();

    // Connector runs horizontally at y=55 from x=35 to x=95 in world mm
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(65.0, 55.0), 1.5),
        Some(inst_id),
        "hit_test_point must hit Connector inside SymbolInstance"
    );
    assert_eq!(
        hit_test_rect(&doc, 0, Rect2D::from_xywh(60.0, 50.0, 10.0, 10.0), false),
        vec![inst_id],
        "hit_test_rect must hit Connector inside SymbolInstance"
    );
}

#[test]
fn w1b_r18_render_isolated_object_non_frozen_lens_master_and_grouped_top_connector() {
    use coree_core::{
        AnchorPoint, ConnectorObject, ConnectorStyle, LensConfig, LensKind, MasterLayerScope,
        TextOnPath,
    };

    // 1. Non-frozen Lens::Invert over a Master-layer opaque red rectangle on Page 2
    let mut doc = Document::new("R18 Isolated Lens", 120.0, 120.0, ColorMode::Rgb, 96.0);
    let p1_id = doc.pages()[0].id;
    let master_lid = doc
        .add_layer(p1_id, "MasterBg", false, MasterLayerScope::AllPages)
        .unwrap();
    doc.add_object(
        rect_obj(
            20.0,
            20.0,
            60.0,
            60.0,
            Fill::Solid(Color::rgba8(255, 0, 0, 255)),
        ),
        Some(master_lid),
    )
    .unwrap();
    let _p2_id = doc.add_page("Page 2", 120.0, 120.0, 3.0);
    let p2_lid = doc.pages()[1].layers[0].id;
    let mut lens_obj = rect_obj(40.0, 40.0, 20.0, 20.0, Fill::None);
    lens_obj.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let lens_id = doc.add_object(lens_obj, Some(p2_lid)).unwrap();

    let (lens_bb, lens_buf) =
        coree_render::render_isolated_object_to_rgba(&doc, 1, lens_id, Affine2D::IDENTITY, 96.0)
            .unwrap()
            .expect("non-frozen Invert lens isolated render");
    let cx =
        (((50.0 - lens_bb.min_x) / lens_bb.width()) * f64::from(lens_buf.width)).floor() as u32;
    let cy =
        (((50.0 - lens_bb.min_y) / lens_bb.height()) * f64::from(lens_buf.height)).floor() as u32;
    let cidx = ((cy as usize) * (lens_buf.width as usize) + (cx as usize)) * 4;
    let center_px = &lens_buf.rgba[cidx..cidx + 4];
    assert!(
        center_px[3] > 200 && center_px[0] < 30 && center_px[1] > 220 && center_px[2] > 220,
        "non-frozen Invert lens over Master red rect must produce opaque cyan at center, got {center_px:?}"
    );
    // Corner outside the lens must remain transparent [0,0,0,0]
    assert_eq!(&lens_buf.rgba[0..4], &[0, 0, 0, 0]);

    // 2. Grouped+rotated TextOnPath and Connector called with parent_xf = IDENTITY
    let mut gdoc = Document::new("R18 Grouped Isolated", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let glid = gdoc.pages()[0].layers[0].id;
    let host = gdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "HostPath",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(50.0, 0.0),
                )),
            ),
            Some(glid),
        )
        .unwrap();
    let top_id = gdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "GroupedTop",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain(
                        "ISOLATED",
                        TextStyle {
                            font_size_pt: 16.0,
                            fill: Color::BLACK,
                            ..TextStyle::default()
                        },
                    ),
                    path_id: host,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(glid),
        )
        .unwrap();
    let b_a = gdoc
        .add_object(
            rect_obj(0.0, 20.0, 10.0, 10.0, Fill::Solid(Color::CYAN)),
            Some(glid),
        )
        .unwrap();
    let b_b = gdoc
        .add_object(
            rect_obj(40.0, 20.0, 10.0, 10.0, Fill::Solid(Color::CYAN)),
            Some(glid),
        )
        .unwrap();
    let mut gconn = ConnectorObject::between_objects(
        b_a,
        AnchorPoint::Center,
        b_b,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    gconn.from_fallback = Point2D::ZERO;
    gconn.to_fallback = Point2D::ZERO;
    let conn_id = gdoc
        .add_object(
            SceneObject::new(ObjectId(0), "GroupedConn", NodePayload::Connector(gconn))
                .with_outline(OutlineStyle::solid(Color::BLACK, 1.0)),
            Some(glid),
        )
        .unwrap();
    let grp = gdoc
        .group_objects(&[host, top_id, b_a, b_b, conn_id], "RotGroup")
        .unwrap();
    gdoc.transform_object(
        grp,
        Affine2D::translate(80.0, 80.0).compose(&Affine2D::rotate_deg(45.0)),
    )
    .unwrap();

    let (top_bb, top_buf) =
        coree_render::render_isolated_object_to_rgba(&gdoc, 0, top_id, Affine2D::IDENTITY, 96.0)
            .unwrap()
            .expect("grouped TextOnPath isolated render");
    let top_ink = top_buf
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 20)
        .count();
    assert!(
        top_bb.min_x >= 60.0 && top_bb.min_y >= 60.0 && top_ink > 30,
        "grouped TextOnPath isolated render must reflect world bounds & ink: bb={top_bb:?}, ink={top_ink}"
    );

    let (conn_bb, conn_buf) =
        coree_render::render_isolated_object_to_rgba(&gdoc, 0, conn_id, Affine2D::IDENTITY, 96.0)
            .unwrap()
            .expect("grouped Connector isolated render");
    let conn_ink = conn_buf
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 20)
        .count();
    assert!(
        conn_bb.min_x >= 50.0 && conn_bb.min_y >= 60.0 && conn_ink > 20,
        "grouped Connector isolated render must reflect world bounds & ink: bb={conn_bb:?}, ink={conn_ink}"
    );
}

#[test]
fn w1b_r18_oversized_transparency_effect_and_group_symmetry_text_on_path_connector() {
    use coree_core::{AnchorPoint, ConnectorObject, ConnectorStyle, TextOnPath, TransparencyMode};

    // 1. Oversized object (> MAX_SURFACE_DIM_PX = 16384) with Effect::Transparency FountainLinear
    struct LargeSurfaceBackend {
        w: u32,
        h: u32,
        drawn: Option<coree_render::RgbaBuffer>,
    }
    impl CanvasBackend for LargeSurfaceBackend {
        fn width(&self) -> u32 {
            self.w
        }
        fn height(&self) -> u32 {
            self.h
        }
        fn clear(&mut self, _color: Color) {}
        fn set_flatten_tolerance(&mut self, _tolerance: Option<f64>) {}
        fn fill_path(
            &mut self,
            _path: &BezierPath,
            _paint: &coree_render::BackendPaint,
            _rule: FillRule,
            _transform: Affine2D,
        ) {
        }
        fn stroke_path(
            &mut self,
            _path: &BezierPath,
            _stroke: &coree_render::StrokeParams,
            _transform: Affine2D,
        ) {
        }
        fn draw_pixmap(
            &mut self,
            image: &coree_render::RgbaBuffer,
            _transform: Affine2D,
            _opacity: f32,
            _blend_mode: BlendMode,
            _smooth: bool,
        ) {
            self.drawn = Some(image.clone());
        }
        fn push_clip(&mut self, _path: &BezierPath, _rule: FillRule, _transform: Affine2D) {}
        fn pop_clip(&mut self) {}
        fn read_region(&self, rect: coree_render::PixelRect) -> coree_render::RgbaBuffer {
            coree_render::RgbaBuffer::new_transparent(rect.width, rect.height)
        }
        fn composite_region(
            &mut self,
            _rect: coree_render::PixelRect,
            src: &coree_render::RgbaBuffer,
            _opacity: f32,
            _blend_mode: BlendMode,
        ) {
            self.drawn = Some(src.clone());
        }
        fn snapshot_rgba(&self) -> coree_render::RgbaBuffer {
            coree_render::RgbaBuffer::new_transparent(1, 1)
        }
        fn into_rgba(self) -> coree_render::RgbaBuffer {
            self.drawn
                .unwrap_or_else(|| coree_render::RgbaBuffer::new_transparent(1, 1))
        }
    }

    let mut big_doc = Document::new("R18 Oversized Trans", 4600.0, 40.0, ColorMode::Rgb, 96.0);
    let big_lid = big_doc.pages()[0].layers[0].id;
    let mut wide_rect = rect_obj(
        10.0,
        5.0,
        4500.0,
        20.0,
        Fill::Solid(Color::rgba8(220, 30, 30, 255)),
    );
    wide_rect.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::new(0.0, 0.0),
            end: Point2D::new(4500.0, 0.0),
            start_alpha: 1.0,
            end_alpha: 0.2,
        },
        blend_mode: BlendMode::Normal,
    });
    big_doc.add_object(wide_rect, Some(big_lid)).unwrap();
    let mut opts = RenderOptions::export();
    opts.page_bg = Color::TRANSPARENT;
    opts.canvas_bg = Color::TRANSPARENT;
    let mut big_backend = LargeSurfaceBackend {
        w: 17500,
        h: 160,
        drawn: None,
    };
    let big_vp = Viewport::for_export(17500, 160, 96.0);
    let mut renderer = SceneRenderer::new();
    renderer.render_document(
        &big_doc,
        &big_vp,
        ViewMode::Enhanced,
        &opts,
        &mut big_backend,
    );
    let big_rgba = big_backend
        .drawn
        .expect("Oversized (>16384px) Effect::Transparency must render via downscaled buffer");
    let mid_y = big_rgba.height / 2;
    let left_a = big_rgba.get_pixel(big_rgba.width / 10, mid_y).unwrap()[3];
    let right_a = big_rgba
        .get_pixel((big_rgba.width * 9) / 10, mid_y)
        .unwrap()[3];
    assert!(
        left_a > 180 && right_a > 40 && left_a > right_a + 80,
        "Oversized FountainLinear Effect::Transparency must preserve gradient ramp: left_a={left_a}, right_a={right_a}"
    );

    // 2. Group-level Effect::Symmetry propagates into child TextOnPath and Connector
    let mut sym_doc = Document::new("R18 GroupSym", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let sym_lid = sym_doc.pages()[0].layers[0].id;
    let hp = sym_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "SymHost",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(20.0, 30.0),
                    Point2D::new(60.0, 30.0),
                )),
            ),
            Some(sym_lid),
        )
        .unwrap();
    let g_top = sym_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "GrpTop",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain(
                        "MMMM",
                        TextStyle {
                            font_size_pt: 18.0,
                            fill: Color::rgba8(20, 40, 220, 255),
                            ..TextStyle::default()
                        },
                    ),
                    path_id: hp,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(sym_lid),
        )
        .unwrap();
    let b1 = sym_doc
        .add_object(rect_obj(20.0, 50.0, 10.0, 10.0, Fill::None), Some(sym_lid))
        .unwrap();
    let b2 = sym_doc
        .add_object(rect_obj(50.0, 50.0, 10.0, 10.0, Fill::None), Some(sym_lid))
        .unwrap();
    let g_conn = sym_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "GrpConn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    b1,
                    AnchorPoint::Center,
                    b2,
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            )
            .with_outline(OutlineStyle::solid(Color::rgba8(220, 20, 20, 255), 1.5)),
            Some(sym_lid),
        )
        .unwrap();
    let grp = sym_doc
        .group_objects(&[hp, g_top, b1, b2, g_conn], "SymGroup")
        .unwrap();
    coree_core::apply_command(
        &mut sym_doc,
        &coree_core::CommandOp::AddEffect {
            id: grp,
            effect: Effect::Symmetry {
                axes: 2,
                mirror: false,
                origin: Point2D::new(100.0, 100.0),
            },
        },
    )
    .unwrap();

    let mut sym_opts = RenderOptions::export();
    sym_opts.page_bg = Color::WHITE;
    let rendered = coree_render::render_page_to_rgba_with_options(
        &sym_doc,
        0,
        96.0,
        ViewMode::Enhanced,
        &sym_opts,
    )
    .unwrap();
    // 2-fold 180-deg symmetry about (100,100) maps y=25..35 -> y=165..175 (TextOnPath)
    // and y=55 -> y=145, x=140..175 (Connector)
    let px_per_mm: f64 = 96.0 / 25.4;
    let mut top_mirror_blue = 0usize;
    let mut conn_mirror_red = 0usize;
    for y_mm in 160..=178 {
        let py = (f64::from(y_mm) * px_per_mm).round() as u32;
        for x_mm in 135..=185 {
            let px = (f64::from(x_mm) * px_per_mm).round() as u32;
            let p = rendered.pixel(px, py);
            if p[2] > 150 && p[0] < 100 {
                top_mirror_blue += 1;
            }
        }
    }
    for y_mm in 140..=150 {
        let py = (f64::from(y_mm) * px_per_mm).round() as u32;
        for x_mm in 140..=175 {
            let px = (f64::from(x_mm) * px_per_mm).round() as u32;
            let p = rendered.pixel(px, py);
            if p[0] > 150 && p[2] < 100 {
                conn_mirror_red += 1;
            }
        }
    }
    assert!(
        top_mirror_blue > 10,
        "Group-level Effect::Symmetry must mirror child TextOnPath, found {top_mirror_blue} blue px"
    );
    assert!(
        conn_mirror_red > 10,
        "Group-level Effect::Symmetry must mirror child Connector, found {conn_mirror_red} red px"
    );
}

#[test]
fn w1b_r19_rotated_drop_shadow_tile_seam_invariance_and_symmetry_child_dirty() {
    // 1. 12-deg rotated DropShadow (FeatherDirection::Outside, dx=30, blur=4) crossing x=256/y=256 seams
    //    both inside a Group and ungrouped must render with 0 diff px vs single-pass SceneRenderer.
    for grouped in [true, false] {
        let mut doc = Document::new("R19 RotShadow", 200.0, 200.0, ColorMode::Rgb, 101.6);
        let lid = doc.pages()[0].layers[0].id;
        let mut sh = rect_obj(
            0.0,
            0.0,
            24.0,
            24.0,
            Fill::Solid(Color::rgba8(40, 120, 240, 255)),
        )
        .with_transform(
            Affine2D::translate(34.0, 34.0).compose(&Affine2D::rotate(12.0_f64.to_radians())),
        );
        sh.effects.push(Effect::DropShadow {
            dx_mm: 30.0,
            dy_mm: 24.0,
            blur_mm: 4.0,
            opacity: 0.8,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Outside,
        });
        let rid = doc.add_object(sh, Some(lid)).unwrap();
        if grouped {
            doc.group_objects(&[rid], "RotShadowGroup").unwrap();
        }

        let vp = Viewport::for_export(768, 512, 101.6);
        let mut opts = RenderOptions::export();
        opts.page_bg = Color::WHITE;

        let mut direct_back = TinySkiaBackend::new(768, 512).unwrap();
        SceneRenderer::new().render_document(
            &doc,
            &vp,
            ViewMode::Enhanced,
            &opts,
            &mut direct_back,
        );
        let direct = direct_back.into_rgba();

        let mut cache = DirtyTileCache::new(768, 512);
        let mut renderer = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        let tiled = cache.composite_to_rgba();

        let diffs: Vec<_> = direct
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(tiled.rgba.as_chunks::<4>().0.iter())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i % 768, i / 768, *a, *b))
            .collect();
        assert_eq!(
            diffs.len(),
            0,
            "12-deg rotated DropShadow (grouped={grouped}) must have 0 diff px across 256px seams: {diffs:?}"
        );
    }

    // 2. Edit child fill inside Group with Effect::Symmetry + mark_object_dirty(child_id) -> 0 diff px
    let mut sdoc = Document::new("R19 SymChildDirty", 200.0, 200.0, ColorMode::Rgb, 101.6);
    let slid = sdoc.pages()[0].layers[0].id;
    let r1 = sdoc
        .add_object(
            rect_obj(
                10.0,
                10.0,
                20.0,
                20.0,
                Fill::Solid(Color::rgba8(220, 20, 20, 255)),
            ),
            Some(slid),
        )
        .unwrap();
    let grp_id = sdoc.group_objects(&[r1], "SymGrp").unwrap();
    coree_core::apply_command(
        &mut sdoc,
        &coree_core::CommandOp::AddEffect {
            id: grp_id,
            effect: Effect::Symmetry {
                axes: 2,
                mirror: false,
                origin: Point2D::new(80.0, 80.0),
            },
        },
    )
    .unwrap();

    let svp = Viewport::for_export(768, 768, 101.6);
    let mut sopts = RenderOptions::export();
    sopts.page_bg = Color::WHITE;
    let mut scache = DirtyTileCache::new(768, 768);
    let mut srenderer = SceneRenderer::new();
    scache.render_dirty_tiles(&sdoc, &svp, ViewMode::Enhanced, &sopts, &mut srenderer);

    // Mutate ONLY child `r1` and call mark_object_dirty(&sdoc, r1)
    coree_core::apply_command(
        &mut sdoc,
        &coree_core::CommandOp::SetFill {
            ids: vec![r1],
            fill: Fill::Solid(Color::rgba8(20, 40, 220, 255)),
        },
    )
    .unwrap();
    scache.mark_object_dirty(&sdoc, r1);
    scache.render_dirty_tiles(&sdoc, &svp, ViewMode::Enhanced, &sopts, &mut srenderer);
    let tiled_after = scache.composite_to_rgba();

    let mut direct_after_back = TinySkiaBackend::new(768, 768).unwrap();
    SceneRenderer::new().render_document(
        &sdoc,
        &svp,
        ViewMode::Enhanced,
        &sopts,
        &mut direct_after_back,
    );
    let direct_after = direct_after_back.into_rgba();
    let sym_diff = tiled_after
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(direct_after.rgba.as_chunks::<4>().0.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        sym_diff, 0,
        "mark_object_dirty on child inside Symmetry Group must invalidate clone tiles (0 diff px)"
    );
}

#[test]
fn w1b_r19_intra_group_lens_isolated_render_and_convert_object_to_bitmap_and_hit_test_cycle() {
    // 1. Non-frozen Lens::Invert inside translated Group with preceding Red sibling over Master Green
    let mut doc = Document::new("R19 IntraGroupLens", 120.0, 120.0, ColorMode::Rgb, 96.0);
    let p0_id = doc.pages()[0].id;
    let master_lid = doc
        .add_layer(p0_id, "MasterGreen", false, MasterLayerScope::AllPages)
        .unwrap();
    doc.add_object(
        rect_obj(
            0.0,
            0.0,
            120.0,
            120.0,
            Fill::Solid(Color::rgba8(0, 255, 0, 255)),
        ),
        Some(master_lid),
    )
    .unwrap();
    let lid = doc.pages()[0].layers[0].id;
    let below_in_grp = doc
        .add_object(
            rect_obj(
                0.0,
                0.0,
                30.0,
                30.0,
                Fill::Solid(Color::rgba8(255, 0, 0, 255)),
            ),
            Some(lid),
        )
        .unwrap();
    let mut lens_obj = rect_obj(0.0, 0.0, 20.0, 20.0, Fill::None);
    lens_obj.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let lens_id = doc.add_object(lens_obj, Some(lid)).unwrap();
    let grp_id = doc
        .group_objects(&[below_in_grp, lens_id], "LensGrp")
        .unwrap();
    doc.transform_object(grp_id, Affine2D::translate(30.0, 30.0))
        .unwrap();

    // Isolated render of lens_id must sample Red sibling (at 30..60) -> Cyan [0,255,255,255]
    let (lbb, lbuf) =
        coree_render::render_isolated_object_to_rgba(&doc, 0, lens_id, Affine2D::IDENTITY, 96.0)
            .unwrap()
            .expect("isolated intra-group lens");
    let cx = (((40.0 - lbb.min_x) / lbb.width()) * f64::from(lbuf.width)).floor() as u32;
    let cy = (((40.0 - lbb.min_y) / lbb.height()) * f64::from(lbuf.height)).floor() as u32;
    let lpx = lbuf.get_pixel(cx, cy).unwrap();
    assert!(
        lpx[3] > 240 && lpx[0] < 15 && lpx[1] > 240 && lpx[2] > 240,
        "Intra-group Invert lens over Red sibling must render Cyan [0,255,255,255], got {lpx:?}"
    );

    // 2. convert_object_to_bitmap_with_bounds on non-frozen Lens::Invert over Red rect -> opaque Cyan
    let (bmp_lens, bmp_lens_bb) =
        convert_object_to_bitmap_with_bounds(&doc, lens_id, 96.0).unwrap();
    let bcx = (((40.0 - bmp_lens_bb.min_x) / bmp_lens_bb.width()) * f64::from(bmp_lens.width))
        .floor() as u32;
    let bcy = (((40.0 - bmp_lens_bb.min_y) / bmp_lens_bb.height()) * f64::from(bmp_lens.height))
        .floor() as u32;
    let bidx = ((bcy as usize) * (bmp_lens.width as usize) + (bcx as usize)) * 4;
    let bpx = &bmp_lens.to_rgba8_vec()[bidx..bidx + 4];
    assert!(
        bpx[3] > 240 && bpx[0] < 15 && bpx[1] > 240 && bpx[2] > 240,
        "convert_object_to_bitmap_with_bounds on non-frozen Invert lens must be opaque Cyan, got {bpx:?}"
    );

    // 3. convert_object_to_bitmap_with_bounds on Group whose child has Effect::Symmetry
    let mut sdoc = Document::new("R19 BmpChildSym", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let slid = sdoc.pages()[0].layers[0].id;
    let mut cshape = rect_obj(
        10.0,
        10.0,
        20.0,
        20.0,
        Fill::Solid(Color::rgba8(0, 120, 220, 255)),
    );
    cshape.effects.push(Effect::Symmetry {
        axes: 2,
        mirror: false,
        origin: Point2D::new(80.0, 80.0),
    });
    let cid = sdoc.add_object(cshape, Some(slid)).unwrap();
    let sgrp = sdoc.group_objects(&[cid], "ChildSymGrp").unwrap();
    let (_sym_bmp, sym_bb) = convert_object_to_bitmap_with_bounds(&sdoc, sgrp, 96.0).unwrap();
    assert!(
        sym_bb.max_x >= 150.0 && sym_bb.max_y >= 150.0,
        "convert_object_to_bitmap_with_bounds on Group with child Symmetry must cover mirrored clone [130..150]: {sym_bb:?}"
    );

    // 4. hit_test.rs sym_sub_world_bounds on asymmetric CycleA <-> CycleB inside SymbolDef
    let mut hdoc = Document::new("R19 HitCycle", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let hlid = hdoc.pages()[0].layers[0].id;
    let cyc_a = SceneObject::new(
        ObjectId(1),
        "SymCycA",
        NodePayload::Connector(ConnectorObject {
            from_obj: None,
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::new(0.0, 0.0),
            to_obj: Some(ObjectId(2)),
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(120.0, 120.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    );
    let cyc_b = SceneObject::new(
        ObjectId(2),
        "SymCycB",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(ObjectId(1)),
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::new(10.0, 10.0),
            to_obj: None,
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(100.0, 100.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    );
    // Outer connector referencing from_obj=CycleA and to_obj=CycleB in same cache
    let outer_conn = SceneObject::new(
        ObjectId(3),
        "OuterConn",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(ObjectId(1)),
            from_anchor: AnchorPoint::BottomRight,
            from_fallback: Point2D::ZERO,
            to_obj: Some(ObjectId(2)),
            to_anchor: AnchorPoint::TopLeft,
            to_fallback: Point2D::ZERO,
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    )
    .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    let hit_sym_id = coree_core::SymbolId::new();
    hdoc.upsert_symbol(SymbolDef {
        id: hit_sym_id,
        name: "HitCycleSym".into(),
        objects: vec![cyc_a, cyc_b, outer_conn],
    });
    let inst = hdoc
        .add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(hit_sym_id)),
            Some(hlid),
        )
        .unwrap();
    // OuterConn routes from CycleA bottom-right (100,100) to CycleB top-left (10,10), passing through (55,55)
    assert_eq!(
        coree_render::hit_test_point(&hdoc, 0, Point2D::new(55.0, 55.0), 3.0),
        Some(inst),
        "hit_test_point must hit SymbolInstance connector over cyclic SymCycA <-> SymCycB"
    );
}

fn count_rgba_diff(a: &RgbaBuffer, b: &RgbaBuffer) -> usize {
    a.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba.as_chunks::<4>().0.iter())
        .filter(|(p, q)| p != q)
        .count()
}

fn render_direct_rgba(doc: &Document, vp: &Viewport, opts: &RenderOptions) -> RgbaBuffer {
    let mut back = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
    SceneRenderer::new().render_document(doc, vp, ViewMode::Enhanced, opts, &mut back);
    back.into_rgba()
}

#[test]
fn w1b_r20_isolated_group_and_powerclip_lens_and_explicit_parent_xf() {
    use coree_core::{apply_command, CommandOp, LensConfig, LensKind};
    use coree_render::{
        convert_object_to_bitmap_with_bounds, render_isolated_object_to_rgba, DirtyTileCache,
        RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let green = Color::rgba8(0, 255, 0, 255);

    // 1. Isolated Group(opacity=0.5) containing frozen=false AND frozen=true Lens::Invert crossing x=256px seam
    for frozen in [false, true] {
        let mut doc = Document::new("R20 IsoGrpLens", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(make_rect(10.0, 10.0, 150.0, 150.0, red.clone()), Some(lid))
            .unwrap();
        let mut lens = make_rect(30.0, 20.0, 80.0, 80.0, Color::TRANSPARENT);
        let mut cfg = LensConfig::new(LensKind::Invert);
        cfg.frozen = frozen;
        lens.lens = Some(cfg);
        let lens_id = doc.add_object(lens, Some(lid)).unwrap();
        let grp_id = doc.group_objects(&[lens_id], "IsoGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::SetOpacity {
                ids: vec![grp_id],
                opacity: 0.5,
            },
        )
        .unwrap();

        let vp = Viewport::for_export(768, 768, 96.0);
        let opts = RenderOptions::export();
        let full = render_direct_rgba(&doc, &vp, &opts);
        let cx = ((70.0 / 25.4) * 96.0) as u32;
        let cy = ((60.0 / 25.4) * 96.0) as u32;
        let px = full.get_pixel(cx, cy).unwrap();
        // 0.5 * Cyan [0,255,255] over Red [255,0,0] => [~128, ~128, ~128, 255] (never pure Red [255,0,0,255])
        assert!(
            px[0] < 180 && px[1] > 80 && px[2] > 80,
            "Group(opacity=0.5) Lens::Invert(frozen={frozen}) must blend Cyan over Red, got {px:?}"
        );

        let mut cache = DirtyTileCache::new(768, 768);
        let mut r = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);
        let tiled = cache.composite_to_rgba();
        let diff = count_rgba_diff(&full, &tiled);
        assert_eq!(
            diff, 0,
            "Group(opacity=0.5) Lens::Invert(frozen={frozen}) must have 0 diff px across 256px tile seam"
        );
    }

    // 2. Red-filled PowerClip over Master Green containing Lens::Invert(frozen=false & true)
    for frozen in [false, true] {
        let mut doc = Document::new("R20 PowerClipLens", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let mlid = doc
            .add_layer(
                doc.pages()[0].id,
                "Master",
                true,
                coree_core::MasterLayerScope::AllPages,
            )
            .unwrap();
        doc.add_object(make_rect(0.0, 0.0, 200.0, 200.0, green.clone()), Some(mlid))
            .unwrap();
        let lid = doc.pages()[0].layers[0].id;
        let mut lens = make_rect(40.0, 15.0, 60.0, 60.0, Color::TRANSPARENT);
        let mut cfg = LensConfig::new(LensKind::Invert);
        cfg.frozen = frozen;
        lens.lens = Some(cfg);
        let lens_id = doc.add_object(lens, Some(lid)).unwrap();

        let pc_container = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "RedPCContainer",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        20.0, 10.0, 100.0, 100.0,
                    ))),
                )
                .with_fill(Fill::Solid(red.clone())),
                Some(lid),
            )
            .unwrap();
        let _pc_id = doc.create_powerclip(pc_container, &[lens_id]).unwrap();

        // 2a. convert_object_to_bitmap_with_bounds on child Lens must sample Red fill -> Cyan [0,255,255,255]
        let (bmp, _bb) = convert_object_to_bitmap_with_bounds(&doc, lens_id, 96.0).unwrap();
        let bidx =
            (((bmp.height / 2) as usize) * (bmp.width as usize) + (bmp.width / 2) as usize) * 4;
        let bpx = &bmp.to_rgba8_vec()[bidx..bidx + 4];
        assert_eq!(
            bpx,
            [0, 255, 255, 255],
            "convert_object_to_bitmap_with_bounds(frozen={frozen}) inside Red PowerClip must invert Red -> Cyan"
        );

        // 2b. render_isolated_object_to_rgba on child Lens must sample Red fill -> Cyan [0,255,255,255]
        let (_iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, lens_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let ipx = iso_buf
            .get_pixel(iso_buf.width / 2, iso_buf.height / 2)
            .unwrap();
        assert_eq!(
            ipx,
            [0, 255, 255, 255],
            "render_isolated_object_to_rgba(frozen={frozen}) inside Red PowerClip must invert Red -> Cyan"
        );

        // 2c. Full-page vs DirtyTileCache across x=256px seam == 0 diff px
        let vp = Viewport::for_export(768, 768, 96.0);
        let opts = RenderOptions::export();
        let full = render_direct_rgba(&doc, &vp, &opts);
        let mut cache = DirtyTileCache::new(768, 768);
        let mut r = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);
        let tiled = cache.composite_to_rgba();
        assert_eq!(
            count_rgba_diff(&full, &tiled),
            0,
            "Red PowerClip Lens(frozen={frozen}) must have 0 diff px across x=256px seam"
        );
    }

    // 3. render_isolated_object_to_rgba with explicit parent_xf=translate(50,50) on intra-group Lens over Red sibling
    let mut gdoc = Document::new("R20 ExplicitParentXf", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let glid = gdoc.pages()[0].layers[0].id;
    let red_sib = gdoc
        .add_object(make_rect(0.0, 0.0, 40.0, 40.0, red), Some(glid))
        .unwrap();
    let mut g_lens = make_rect(10.0, 10.0, 20.0, 20.0, Color::TRANSPARENT);
    g_lens.lens = Some(LensConfig::new(LensKind::Invert));
    let g_lens_id = gdoc.add_object(g_lens, Some(glid)).unwrap();
    let _grp = gdoc.group_objects(&[red_sib, g_lens_id], "SibGrp").unwrap();

    let (iso_bb, iso_buf) =
        render_isolated_object_to_rgba(&gdoc, 0, g_lens_id, Affine2D::translate(50.0, 50.0), 96.0)
            .unwrap()
            .unwrap();
    assert!(
        iso_bb.min_x > 50.0 && iso_bb.min_y > 50.0,
        "explicit parent_xf must shift isolated bbox to ~[57..83]: {iso_bb:?}"
    );
    let cpx = iso_buf
        .get_pixel(iso_buf.width / 2, iso_buf.height / 2)
        .unwrap();
    assert_eq!(
        cpx,
        [0, 255, 255, 255],
        "render_isolated_object_to_rgba with explicit parent_xf must shift Red sibling to sample Cyan"
    );
}

#[test]
fn w1b_r20_symbol_connector_dag_render_and_mutually_recursive_symbols() {
    use coree_core::{AnchorPoint, ConnectorObject, ConnectorStyle, SymbolDef, SymbolId};
    use coree_render::{RenderOptions, SceneRenderer, TinySkiaBackend};

    let red = Color::rgba8(255, 0, 0, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);

    let mut doc = Document::new("R20 SymRenderDAG", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    // 1. N=40 diamond Connector DAG inside SymbolDef rendered via SceneRenderer::render_document < 50ms
    let mut sym_objs = Vec::new();
    let base = SceneObject::new(
        ObjectId(1),
        "Base",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 20.0, 20.0,
        ))),
    )
    .with_fill(Fill::Solid(blue));
    sym_objs.push(base);
    let mut prev_id = ObjectId(1);
    for i in 2..=41u64 {
        let conn = ConnectorObject::between_objects(
            prev_id,
            AnchorPoint::Left,
            prev_id,
            AnchorPoint::Right,
            ConnectorStyle::Straight,
        );
        let cid = ObjectId(i);
        sym_objs.push(
            SceneObject::new(cid, format!("C{i}"), NodePayload::Connector(conn))
                .with_outline(OutlineStyle::solid(Color::BLACK, 1.0)),
        );
        prev_id = cid;
    }
    let dag_sym = SymbolDef::new("DiamondDagSym", sym_objs);
    let dag_sym_id = dag_sym.id;
    doc.insert_symbol(dag_sym);
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "DagInst",
            NodePayload::SymbolInstance(dag_sym_id),
        ),
        Some(lid),
    )
    .unwrap();

    // 2. Mutually recursive SymbolDefs (S1 <-> S2)
    let s1_id = SymbolId::new();
    let s2_id = SymbolId::new();
    doc.upsert_symbol(SymbolDef {
        id: s1_id,
        name: "S1".into(),
        objects: vec![
            SceneObject::new(
                ObjectId(1),
                "R1",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    50.0, 50.0, 20.0, 20.0,
                ))),
            )
            .with_fill(Fill::Solid(red)),
            SceneObject::new(ObjectId(2), "RefS2", NodePayload::SymbolInstance(s2_id)),
        ],
    });
    doc.upsert_symbol(SymbolDef {
        id: s2_id,
        name: "S2".into(),
        objects: vec![
            SceneObject::new(
                ObjectId(1),
                "R2",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    80.0, 50.0, 20.0, 20.0,
                ))),
            )
            .with_fill(Fill::Solid(green)),
            SceneObject::new(ObjectId(2), "RefS1", NodePayload::SymbolInstance(s1_id)),
        ],
    });
    doc.add_object(
        SceneObject::new(ObjectId(0), "RecInst", NodePayload::SymbolInstance(s1_id)),
        Some(lid),
    )
    .unwrap();

    let vp = Viewport::for_export(512, 512, 96.0);
    let opts = RenderOptions::export();
    let mut backend = TinySkiaBackend::new(512, 512).unwrap();
    let mut renderer = SceneRenderer::new();
    let t0 = std::time::Instant::now();
    renderer.render_document(&doc, &vp, ViewMode::Enhanced, &opts, &mut backend);
    let elapsed = t0.elapsed();
    let limit_ms: u128 = if cfg!(debug_assertions) { 500 } else { 50 };
    assert!(
        elapsed.as_millis() < limit_ms,
        "SceneRenderer::render_document on N=40 diamond Connector DAG + S1<->S2 cycle must finish in < {limit_ms}ms, took {elapsed:?}"
    );
}

#[test]
fn w1b_r20_dirty_tile_cache_dependents_and_group_deform_effects() {
    use coree_core::{
        apply_command, AnchorPoint, ArtisticText, CommandOp, ConnectorObject, ConnectorStyle,
        Effect, EnvelopeMode, TextOnPath, TextStyle,
    };
    use coree_render::{
        convert_object_to_bitmap_with_bounds, DirtyTileCache, RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);
    let gray = Color::rgba8(128, 128, 128, 255);

    let vp = Viewport::for_export(512, 512, 96.0);
    let opts = RenderOptions::export();

    // 1. Blend target move -> mark_object_dirty(target) -> 0 diff px
    {
        let mut doc = Document::new("R20 BlendDirty", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let b_tgt = doc
            .add_object(make_rect(90.0, 20.0, 25.0, 25.0, blue.clone()), Some(lid))
            .unwrap();
        let mut b_src = make_rect(10.0, 20.0, 25.0, 25.0, red.clone());
        b_src.effects.push(Effect::Blend {
            target_id: b_tgt,
            steps: 5,
            rotation_deg: 0.0,
            along_path: None,

            rotate_along_path: true,
        });
        doc.add_object(b_src, Some(lid)).unwrap();

        let mut cache = DirtyTileCache::new(512, 512);
        let mut r = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);

        cache.mark_object_dirty(&doc, b_tgt);
        doc.set_object_transform(b_tgt, Affine2D::translate(0.0, 60.0))
            .unwrap();
        cache.mark_object_dirty(&doc, b_tgt);

        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);
        let tiled = cache.composite_to_rgba();
        let fresh = render_direct_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_rgba_diff(&fresh, &tiled),
            0,
            "Moving Blend target + mark_object_dirty must yield 0 diff px"
        );
    }

    // 2. Connector endpoint move -> mark_object_dirty(endpoint) -> 0 diff px
    {
        let mut doc = Document::new("R20 ConnDirty", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let s1 = doc
            .add_object(make_rect(10.0, 10.0, 20.0, 20.0, red.clone()), Some(lid))
            .unwrap();
        let s2 = doc
            .add_object(make_rect(90.0, 10.0, 20.0, 20.0, blue), Some(lid))
            .unwrap();
        let conn = ConnectorObject::between_objects(
            s1,
            AnchorPoint::Center,
            s2,
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        );
        doc.add_object(
            SceneObject::new(ObjectId(0), "Conn", NodePayload::Connector(conn))
                .with_outline(OutlineStyle::solid(Color::BLACK, 2.0)),
            Some(lid),
        )
        .unwrap();

        let mut cache = DirtyTileCache::new(512, 512);
        let mut r = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);

        cache.mark_object_dirty(&doc, s2);
        doc.set_object_transform(s2, Affine2D::translate(0.0, 70.0))
            .unwrap();
        cache.mark_object_dirty(&doc, s2);

        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);
        let tiled = cache.composite_to_rgba();
        let mut fresh_cache = DirtyTileCache::new(512, 512);
        fresh_cache.render_dirty_tiles(
            &doc,
            &vp,
            ViewMode::Enhanced,
            &opts,
            &mut SceneRenderer::new(),
        );
        let fresh_tiled = fresh_cache.composite_to_rgba();
        assert_eq!(
            count_rgba_diff(&fresh_tiled, &tiled),
            0,
            "Moving Connector endpoint + mark_object_dirty must match fresh DirtyTileCache with 0 diff px"
        );
        // Also verify old connector midpoint (50mm, 20mm) is completely cleared (white background)
        let old_mid_x = ((50.0 / 25.4) * 96.0) as u32;
        let old_mid_y = ((20.0 / 25.4) * 96.0) as u32;
        assert_eq!(
            tiled.get_pixel(old_mid_x, old_mid_y).unwrap(),
            [255, 255, 255, 255],
            "Old connector pixels must be cleared after moving endpoint"
        );
    }

    // 3. TextOnPath host move -> mark_object_dirty(host) -> 0 diff px
    {
        let mut doc = Document::new("R20 TopDirty", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let host = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Host",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(10.0, 20.0),
                        Point2D::new(110.0, 20.0),
                    )),
                )
                .with_outline(OutlineStyle::solid(gray, 0.5)),
                Some(lid),
            )
            .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Top",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("ONPATH", TextStyle::default()),
                    path_id: host,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();

        let mut cache = DirtyTileCache::new(512, 512);
        let mut r = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);

        cache.mark_object_dirty(&doc, host);
        doc.set_object_transform(host, Affine2D::translate(0.0, 60.0))
            .unwrap();
        cache.mark_object_dirty(&doc, host);

        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);
        let tiled = cache.composite_to_rgba();
        let fresh = render_direct_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_rgba_diff(&fresh, &tiled),
            0,
            "Moving TextOnPath host + mark_object_dirty must yield 0 diff px"
        );
    }

    // 4. Group with Effect::Envelope and Effect::Perspective renders ink inside deformed cage corners
    for eff in [
        Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(0.0, 0.0),
                Point2D::new(90.0, 0.0),
                Point2D::new(90.0, 80.0),
                Point2D::new(0.0, 80.0),
            ],
        },
        Effect::Perspective {
            quad: [
                Point2D::new(0.0, 0.0),
                Point2D::new(90.0, 0.0),
                Point2D::new(90.0, 80.0),
                Point2D::new(0.0, 80.0),
            ],
        },
    ] {
        let mut doc = Document::new("R20 GrpDeform", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let child = doc
            .add_object(make_rect(0.0, 0.0, 40.0, 30.0, red.clone()), Some(lid))
            .unwrap();
        let grp = doc.group_objects(&[child], "DeformGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: eff,
            },
        )
        .unwrap();

        let (bmp, bb) = convert_object_to_bitmap_with_bounds(&doc, grp, 96.0).unwrap();
        assert!(
            bb.max_x >= 90.0 && bb.max_y >= 80.0,
            "Group deform bounds {bb:?}"
        );
        let px_per_mm: f64 = 96.0 / 25.4;
        let mut corner_ink_px = 0usize;
        for y in 0..bmp.height {
            let wy = bb.min_y + (f64::from(y) + 0.5) / px_per_mm;
            if !(60.0..=75.0).contains(&wy) {
                continue;
            }
            for x in 0..bmp.width {
                let wx = bb.min_x + (f64::from(x) + 0.5) / px_per_mm;
                if !(60.0..=80.0).contains(&wx) {
                    continue;
                }
                let idx = ((y as usize) * (bmp.width as usize) + (x as usize)) * 4;
                if bmp.to_rgba8_vec()[idx + 3] > 200 {
                    corner_ink_px += 1;
                }
            }
        }
        assert!(
            corner_ink_px > 500,
            "Group with Envelope/Perspective must render deformed ink in expanded corner [60..80, 60..75], got {corner_ink_px} px"
        );
    }
}

#[test]
fn w1b_r21_group_deform_child_features_isolated_bounds_and_sym_external_conn() {
    use coree_core::{
        apply_command, AnchorPoint, CommandOp, ConnectorObject, ConnectorStyle, LensConfig,
        LensKind, SymbolDef, TableCell, TransparencyMode, VectorTable,
    };
    use coree_render::{
        convert_object_to_bitmap_with_bounds, render_isolated_object_to_rgba, DirtyTileCache,
        RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);
    let black = Color::rgba8(0, 0, 0, 255);
    let sample_mm = |buf: &coree_render::RgbaBuffer, x_mm: f64, y_mm: f64| -> [u8; 4] {
        let px = ((x_mm / 25.4) * 96.0) as u32;
        let py = ((y_mm / 25.4) * 96.0) as u32;
        buf.get_pixel(px, py).unwrap()
    };

    let cage = vec![
        Point2D::new(0.0, 0.0),
        Point2D::new(120.0, 0.0),
        Point2D::new(120.0, 80.0),
        Point2D::new(0.0, 80.0),
    ];

    // 1. challenger2-r20-2 (a): Group(Envelope | Perspective | Distort) + [Red, Lens::Invert] => Cyan [0,255,255,255]
    for eff in [
        Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: cage.clone(),
        },
        Effect::Perspective {
            quad: [cage[0], cage[1], cage[2], cage[3]],
        },
        Effect::Distort {
            kind: coree_core::DistortKind::PushPull,
            amplitude: 5.0,
            frequency: 2.0,
            center: Point2D::new(60.0, 40.0),
        },
    ] {
        let mut doc = Document::new("R21 GrpDeformLens", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let r = doc
            .add_object(make_rect(10.0, 10.0, 100.0, 60.0, red.clone()), Some(lid))
            .unwrap();
        let mut lens = make_rect(20.0, 20.0, 80.0, 40.0, Color::TRANSPARENT);
        lens.lens = Some(LensConfig::new(LensKind::Invert));
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        let grp = doc.group_objects(&[r, l_id], "DeformLensGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: eff,
            },
        )
        .unwrap();
        let vp = Viewport::for_export(600, 600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let px = sample_mm(&buf, 60.0, 40.0);
        assert_eq!(
            px,
            [0, 255, 255, 255],
            "Group deform + child Lens::Invert must invert Red -> Cyan, got {px:?}"
        );
    }

    // 2. challenger2-r20-2 (b): Group + Envelope + child DropShadow preserves shadow ink
    {
        let mut doc = Document::new("R21 GrpEnvChildShadow", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut child = make_rect(10.0, 10.0, 40.0, 30.0, red.clone());
        child.effects.push(Effect::DropShadow {
            dx_mm: 20.0,
            dy_mm: 20.0,
            blur_mm: 1.0,
            opacity: 0.9,
            color: black.clone(),
            feather_direction: coree_core::FeatherDirection::Middle,
        });
        let c_id = doc.add_object(child, Some(lid)).unwrap();
        let grp = doc.group_objects(&[c_id], "EnvShadowGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 10.0),
                        Point2D::new(80.0, 10.0),
                        Point2D::new(80.0, 60.0),
                        Point2D::new(10.0, 60.0),
                    ],
                },
            },
        )
        .unwrap();
        let vp = Viewport::for_export(500, 500, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let px = sample_mm(&buf, 70.0, 55.0);
        assert_ne!(
            px,
            [255, 255, 255, 255],
            "Group+Envelope must preserve child DropShadow ink at (70,55)mm, got {px:?}"
        );
    }

    // 3. challenger2-r20-2 (c): Group + Envelope + child VectorTable deforms Table into cage
    {
        let mut doc = Document::new("R21 GrpEnvTable", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut tbl = VectorTable::new(2, 2, 20.0, 15.0);
        tbl.cells[0] = TableCell {
            fill: Fill::Solid(blue.clone()),
            ..tbl.cells[0].clone()
        };
        tbl.cells[3] = TableCell {
            fill: Fill::Solid(red.clone()),
            ..tbl.cells[3].clone()
        };
        let t_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Tbl", NodePayload::Table(tbl))
                    .with_transform(Affine2D::translate(10.0, 10.0)),
                Some(lid),
            )
            .unwrap();
        let grp = doc.group_objects(&[t_id], "EnvTblGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: cage.clone(),
                },
            },
        )
        .unwrap();
        let vp = Viewport::for_export(600, 600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let px = sample_mm(&buf, 100.0, 65.0);
        assert_ne!(
            px,
            [255, 255, 255, 255],
            "Group+Envelope must deform child VectorTable into expanded corner (100,65)mm, got {px:?}"
        );
    }

    // 4. challenger2-r20-2 (d): Group + Envelope + child Effect::Transparency blends properly
    {
        let mut doc = Document::new("R21 GrpEnvTrans", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let bg = doc
            .add_object(make_rect(10.0, 10.0, 40.0, 30.0, blue.clone()), Some(lid))
            .unwrap();
        let mut fg = make_rect(10.0, 10.0, 40.0, 30.0, red.clone());
        fg.effects.push(Effect::Transparency {
            mode: TransparencyMode::Uniform(0.5),
            blend_mode: BlendMode::Normal,
        });
        let fg_id = doc.add_object(fg, Some(lid)).unwrap();
        let grp = doc.group_objects(&[bg, fg_id], "EnvTransGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage,
                },
            },
        )
        .unwrap();
        let vp = Viewport::for_export(600, 600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let px = sample_mm(&buf, 60.0, 40.0);
        assert!(
            px[0] > 80 && px[2] > 80 && px[0] < 200,
            "Group+Envelope+child Transparency(0.5) must blend Red over Blue, got {px:?}"
        );
    }

    // 5. challenger2-r20-3: render_isolated_object_to_rgba on Group + Envelope (IDENTITY & translate(50,50))
    {
        let mut doc = Document::new("R21 IsoGrpEnv", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let child = doc
            .add_object(make_rect(10.0, 10.0, 20.0, 20.0, red.clone()), Some(lid))
            .unwrap();
        let grp = doc.group_objects(&[child], "IsoEnvGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 10.0),
                        Point2D::new(60.0, 10.0),
                        Point2D::new(60.0, 50.0),
                        Point2D::new(10.0, 50.0),
                    ],
                },
            },
        )
        .unwrap();

        let (bb_id, buf_id) =
            render_isolated_object_to_rgba(&doc, 0, grp, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        assert!(
            bb_id.max_x >= 60.0 && bb_id.max_y >= 50.0,
            "render_isolated_object_to_rgba(IDENTITY) Group+Envelope must enclose [10..60,10..50], got {bb_id:?}"
        );
        let px_per_mm: f64 = 96.0 / 25.4;
        let ix = ((50.0 - bb_id.min_x) * px_per_mm) as u32;
        let iy = ((40.0 - bb_id.min_y) * px_per_mm) as u32;
        assert_eq!(
            buf_id.get_pixel(ix, iy).unwrap(),
            [255, 0, 0, 255],
            "Isolated Group+Envelope must render Red ink at (50,40)mm"
        );

        let (bb_tr, buf_tr) =
            render_isolated_object_to_rgba(&doc, 0, grp, Affine2D::translate(50.0, 50.0), 96.0)
                .unwrap()
                .unwrap();
        assert!(
            bb_tr.max_x >= 110.0 && bb_tr.max_y >= 100.0,
            "render_isolated_object_to_rgba(translate(50,50)) Group+Envelope must enclose [60..110,60..100], got {bb_tr:?}"
        );
        let tx = ((100.0 - bb_tr.min_x) * px_per_mm) as u32;
        let ty = ((90.0 - bb_tr.min_y) * px_per_mm) as u32;
        assert_eq!(
            buf_tr.get_pixel(tx, ty).unwrap(),
            [255, 0, 0, 255],
            "Isolated Group+Envelope with parent_xf=translate(50,50) must render Red ink at (100,90)mm"
        );
    }

    // 6. challenger2-r20-1 (render): convert_object_to_bitmap_with_bounds on Group at [100..160, 100..150]
    {
        let mut doc = Document::new("R21 FarBmp", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let child = doc
            .add_object(make_rect(100.0, 100.0, 40.0, 30.0, red.clone()), Some(lid))
            .unwrap();
        let grp = doc.group_objects(&[child], "FarGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(100.0, 100.0),
                        Point2D::new(160.0, 100.0),
                        Point2D::new(160.0, 150.0),
                        Point2D::new(100.0, 150.0),
                    ],
                },
            },
        )
        .unwrap();
        let (_bmp, bb) = convert_object_to_bitmap_with_bounds(&doc, grp, 96.0).unwrap();
        assert!(
            bb.min_x >= 95.0 && bb.min_y >= 95.0,
            "convert_object_to_bitmap_with_bounds for Far Group+Envelope must have min_x/y >= 95, got {bb:?}"
        );
    }

    // 7. challenger2-r20-4 (tile cache): SymbolDef Connector to external scene object across x=256px seam
    {
        let mut doc = Document::new("R21 SymExtConn", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let ext_a = doc
            .add_object(make_rect(10.0, 60.0, 10.0, 10.0, red.clone()), Some(lid))
            .unwrap();
        let ext_b = doc
            .add_object(make_rect(40.0, 60.0, 10.0, 10.0, blue), Some(lid))
            .unwrap();
        let sym_conn = SceneObject::new(
            ObjectId(1),
            "ExtConn",
            NodePayload::Connector(ConnectorObject::between_objects(
                ext_a,
                AnchorPoint::Center,
                ext_b,
                AnchorPoint::Center,
                ConnectorStyle::Straight,
            )),
        )
        .with_outline(OutlineStyle::solid(black, 1.0));
        let sym = SymbolDef::new("ExtConnSym", vec![sym_conn]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
            Some(lid),
        )
        .unwrap();

        let vp = Viewport::for_export(512, 512, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(512, 512);
        let mut r = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);

        cache.mark_object_dirty(&doc, ext_b);
        doc.set_object_transform(ext_b, Affine2D::translate(80.0, 0.0))
            .unwrap();
        cache.mark_object_dirty(&doc, ext_b);
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut r);

        let tiled = cache.composite_to_rgba();
        let fresh = render_direct_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_rgba_diff(&fresh, &tiled),
            0,
            "SymbolDef Connector to external object moved across x=256px seam must yield 0 diff px"
        );
    }

    // 8. Reviewer 1 R20 Caveat (renderer.rs:945-949): Group(opacity=0.5)+SingleArc(base_h=200>cage_h=20) inside SymbolDef
    {
        let mut doc = Document::new("R21 SymGrpSingleArc", 500.0, 500.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut leaf = make_rect(0.0, 0.0, 100.0, 200.0, red);
        leaf.id = ObjectId(1);
        let mut grp = SceneObject::new(
            ObjectId(2),
            "SymArcGrp",
            NodePayload::Group(vec![ObjectId(1)]),
        );
        grp.opacity = 0.5;
        grp.effects.push(Effect::Envelope {
            mode: EnvelopeMode::SingleArc,
            cage: vec![
                Point2D::new(0.0, 350.0),
                Point2D::new(100.0, 350.0),
                Point2D::new(100.0, 370.0),
                Point2D::new(0.0, 370.0),
            ],
        });
        let sym = SymbolDef::new("SymIsoArc", vec![leaf, grp]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "SymIsoInst",
                NodePayload::SymbolInstance(sym_id),
            ),
            Some(lid),
        )
        .unwrap();
        let vp = Viewport::for_export(600, 1600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let apex_px = sample_mm(&buf, 50.0, 338.0);
        assert_ne!(
            apex_px,
            [255, 255, 255, 255],
            "Isolated Group(opacity=0.5)+SingleArc inside SymbolDef must render apex ink at (50,338)mm, got {apex_px:?}"
        );
    }
}

#[test]
fn w1b_r22_deformed_group_offscreen_lens_remove_face_tile_seams_and_isolated_bounds() {
    use coree_core::{
        apply_command, AnchorPoint, CommandOp, ConnectorObject, ConnectorStyle, DimensionKind,
        DimensionObject, LensConfig, LensKind, SymbolDef, TableCell, VectorTable,
    };
    use coree_render::{
        render_isolated_object_to_rgba, DirtyTileCache, RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let black = Color::rgba8(0, 0, 0, 255);
    let sample_mm = |buf: &coree_render::RgbaBuffer, x_mm: f64, y_mm: f64| -> [u8; 4] {
        let px = ((x_mm / 25.4) * 96.0) as u32;
        let py = ((y_mm / 25.4) * 96.0) as u32;
        buf.get_pixel(px, py).unwrap()
    };

    let shifted_cage = vec![
        Point2D::new(80.0, 80.0),
        Point2D::new(140.0, 80.0),
        Point2D::new(140.0, 140.0),
        Point2D::new(80.0, 140.0),
    ];

    // 1. Shifted Group(Envelope) + [Red, Lens::Invert] in render_isolated_object_to_rgba & OuterGroup(opacity=0.5)
    {
        let mut doc = Document::new("R22 Iso & OuterOp", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let r = doc
            .add_object(make_rect(10.0, 10.0, 40.0, 40.0, red.clone()), Some(lid))
            .unwrap();
        let mut lens = make_rect(10.0, 10.0, 40.0, 40.0, Color::TRANSPARENT);
        lens.lens = Some(LensConfig::new(LensKind::Invert));
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        let grp = doc.group_objects(&[r, l_id], "ShiftedEnvLens").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: shifted_cage.clone(),
                },
            },
        )
        .unwrap();

        // 1a. render_isolated_object_to_rgba -> Cyan [0, 255, 255, 255] at (110, 110)mm
        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, grp, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let px_per_mm: f64 = 96.0 / 25.4;
        let ix = ((110.0 - iso_bb.min_x) * px_per_mm) as u32;
        let iy = ((110.0 - iso_bb.min_y) * px_per_mm) as u32;
        assert_eq!(
            iso_buf.get_pixel(ix, iy).unwrap(),
            [0, 255, 255, 255],
            "render_isolated_object_to_rgba on shifted Group(Envelope)+[Red,Lens::Invert] must render Cyan"
        );

        // 1b. OuterGroup(opacity=0.5) -> ~[128, 255, 255, 255]
        let outer = doc.group_objects(&[grp], "OuterOp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::SetOpacity {
                ids: vec![outer],
                opacity: 0.5,
            },
        )
        .unwrap();
        let vp = Viewport::for_export(600, 600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let px = sample_mm(&buf, 110.0, 110.0);
        assert!(
            px[0].abs_diff(128) <= 3 && px[1] >= 252 && px[2] >= 252,
            "OuterGroup(opacity=0.5)->Group(Envelope)+[Red,Lens::Invert] must blend Cyan over White (~128,255,255), got {px:?}"
        );
    }

    // 2. Group(opacity=0.5, Envelope) + PowerClip(Red) -> Lens::Invert
    {
        let mut doc = Document::new("R22 EnvOpPowerClip", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let cont = doc
            .add_object(make_rect(10.0, 10.0, 40.0, 40.0, red.clone()), Some(lid))
            .unwrap();
        let mut lens = make_rect(10.0, 10.0, 40.0, 40.0, Color::TRANSPARENT);
        lens.lens = Some(LensConfig::new(LensKind::Invert));
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        let pc = doc.create_powerclip(cont, &[l_id]).unwrap();
        let grp = doc.group_objects(&[pc], "EnvOpPCGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::SetOpacity {
                ids: vec![grp],
                opacity: 0.5,
            },
        )
        .unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: shifted_cage.clone(),
                },
            },
        )
        .unwrap();
        let vp = Viewport::for_export(600, 600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let px = sample_mm(&buf, 110.0, 110.0);
        assert!(
            px[0].abs_diff(128) <= 3 && px[1] >= 252 && px[2] >= 252,
            "Group(opacity=0.5,Envelope)+PowerClip(Red)->Lens::Invert must render ~[128,255,255], got {px:?}"
        );
    }

    // 3. Direct render_document on shifted Group(Envelope)+[Red, Lens::Invert(remove_face=true)]
    {
        let mut doc = Document::new("R22 RemoveFace", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let r = doc
            .add_object(make_rect(10.0, 10.0, 40.0, 40.0, red.clone()), Some(lid))
            .unwrap();
        let mut lens = make_rect(10.0, 10.0, 40.0, 40.0, Color::TRANSPARENT);
        let mut cfg = LensConfig::new(LensKind::Invert);
        cfg.remove_face = true;
        lens.lens = Some(cfg);
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        let grp = doc.group_objects(&[r, l_id], "RmFaceGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: shifted_cage.clone(),
                },
            },
        )
        .unwrap();
        let vp = Viewport::for_export(600, 600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        assert_eq!(
            sample_mm(&buf, 110.0, 110.0),
            [0, 255, 255, 255],
            "remove_face=true inside shifted Group(Envelope) must invert deformed Red -> Cyan"
        );
    }

    // 4. DirtyTileCache Lens::frozen=true & Lens::Magnify(2x) across x=256px seam (0 diff px)
    for kind in [
        ("FrozenInvert", true, LensKind::Invert),
        ("Magnify2x", false, LensKind::Magnify { amount: 2.0 }),
    ] {
        let mut doc = Document::new(kind.0, 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let r = doc
            .add_object(make_rect(10.0, 10.0, 40.0, 50.0, red.clone()), Some(lid))
            .unwrap();
        let mut lens = make_rect(10.0, 10.0, 40.0, 50.0, Color::TRANSPARENT);
        let mut cfg = LensConfig::new(kind.2);
        cfg.frozen = kind.1;
        lens.lens = Some(cfg);
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        let grp = doc.group_objects(&[r, l_id], "TileSeamLensGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 10.0),
                        Point2D::new(130.0, 10.0),
                        Point2D::new(130.0, 90.0),
                        Point2D::new(10.0, 90.0),
                    ],
                },
            },
        )
        .unwrap();

        let vp = Viewport::for_export(512, 512, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(512, 512);
        let mut sr = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut sr);
        let tiled = cache.composite_to_rgba();
        let direct = render_direct_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_rgba_diff(&direct, &tiled),
            0,
            "{}: DirtyTileCache vs direct render across x=256px must have 0 diff px",
            kind.0
        );
    }

    // 5. Nested deformed Group: OuterGroup(Envelope1)->InnerGroup(Envelope2)->[Red, Lens::Invert]
    {
        let mut doc = Document::new("R22 NestedDeformLens", 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let r = doc
            .add_object(make_rect(10.0, 10.0, 20.0, 20.0, red.clone()), Some(lid))
            .unwrap();
        let mut lens = make_rect(10.0, 10.0, 20.0, 20.0, Color::TRANSPARENT);
        lens.lens = Some(LensConfig::new(LensKind::Invert));
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        let inner = doc.group_objects(&[r, l_id], "InnerEnv").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: inner,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 10.0),
                        Point2D::new(50.0, 10.0),
                        Point2D::new(50.0, 50.0),
                        Point2D::new(10.0, 50.0),
                    ],
                },
            },
        )
        .unwrap();
        let outer = doc.group_objects(&[inner], "OuterEnv").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: outer,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: shifted_cage,
                },
            },
        )
        .unwrap();

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, outer, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let px_per_mm: f64 = 96.0 / 25.4;
        let ix = ((110.0 - iso_bb.min_x) * px_per_mm) as u32;
        let iy = ((110.0 - iso_bb.min_y) * px_per_mm) as u32;
        assert_eq!(
            iso_buf.get_pixel(ix, iy).unwrap(),
            [0, 255, 255, 255],
            "Nested deformed Groups + Lens::Invert in render_isolated_object_to_rgba must render Cyan"
        );
    }

    // 6. Group(Envelope)->SubGrp(opacity=0.5 | Multiply)->Leaf(DropShadow dx=-20) crossing x=256px (0 diff px)
    for (label, opacity, blend) in [
        ("Opacity0.5", 0.5_f32, BlendMode::Normal),
        ("Multiply", 1.0_f32, BlendMode::Multiply),
    ] {
        let mut doc = Document::new(label, 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let bg = doc
            .add_object(make_rect(40.0, 20.0, 70.0, 50.0, green.clone()), Some(lid))
            .unwrap();
        let mut leaf = make_rect(72.0, 20.0, 30.0, 30.0, red.clone());
        leaf.effects.push(Effect::DropShadow {
            dx_mm: -20.0,
            dy_mm: 0.0,
            blur_mm: 2.0,
            opacity: 0.85,
            color: black.clone(),
            feather_direction: coree_core::FeatherDirection::Middle,
        });
        let leaf_id = doc.add_object(leaf, Some(lid)).unwrap();
        let sub_grp = doc.group_objects(&[leaf_id], "SubIsoGrp").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::SetOpacity {
                ids: vec![sub_grp],
                opacity,
            },
        )
        .unwrap();
        apply_command(
            &mut doc,
            &CommandOp::SetBlendMode {
                ids: vec![sub_grp],
                blend_mode: blend,
            },
        )
        .unwrap();
        let outer = doc.group_objects(&[bg, sub_grp], "OuterEnvShadow").unwrap();
        apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: outer,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(40.0, 20.0),
                        Point2D::new(110.0, 20.0),
                        Point2D::new(110.0, 70.0),
                        Point2D::new(40.0, 70.0),
                    ],
                },
            },
        )
        .unwrap();

        let vp = Viewport::for_export(512, 512, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(512, 512);
        let mut sr = SceneRenderer::new();
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut sr);
        let tiled = cache.composite_to_rgba();
        let direct = render_direct_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_rgba_diff(&direct, &tiled),
            0,
            "Group(Envelope)->SubGrp({label})->Leaf(DropShadow dx=-20) across x=256px must have 0 diff px"
        );
    }

    // 7. render_isolated_object_to_rgba on deformed Table, Dimension, and SymbolInstance(Group+Envelope)
    {
        let mut doc = Document::new(
            "R22 IsoDeformedPayloads",
            300.0,
            300.0,
            ColorMode::Rgb,
            96.0,
        );
        let lid = doc.pages()[0].layers[0].id;
        let rot_tr = Affine2D::translate(50.0, 50.0).compose(&Affine2D::rotate_deg(30.0));

        // 7a. Table(20x20) + Envelope([0..60, 0..50])
        let mut tbl = VectorTable::new(2, 2, 10.0, 10.0);
        for cell in &mut tbl.cells {
            *cell = TableCell {
                fill: Fill::Solid(red.clone()),
                ..cell.clone()
            };
        }
        let mut tbl_obj = SceneObject::new(ObjectId(0), "DeformTable", NodePayload::Table(tbl));
        tbl_obj.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(0.0, 0.0),
                Point2D::new(60.0, 0.0),
                Point2D::new(60.0, 50.0),
                Point2D::new(0.0, 50.0),
            ],
        });
        let tbl_id = doc.add_object(tbl_obj, Some(lid)).unwrap();
        let (t_id_bb, _) =
            render_isolated_object_to_rgba(&doc, 0, tbl_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        assert!(
            t_id_bb.max_x >= 60.0 && t_id_bb.max_y >= 50.0,
            "Isolated deformed Table(IDENTITY) must enclose [0..60,0..50], got {t_id_bb:?}"
        );
        let exp_t_corner = rot_tr.transform_point(Point2D::new(60.0, 50.0));
        let (t_rot_bb, _) = render_isolated_object_to_rgba(&doc, 0, tbl_id, rot_tr, 96.0)
            .unwrap()
            .unwrap();
        assert!(
            t_rot_bb.contains_point(exp_t_corner),
            "Isolated deformed Table(translate*rotate) {t_rot_bb:?} must enclose {exp_t_corner:?}"
        );

        // 7b. Dimension + Envelope([10..70, 5..60])
        let mut dim_obj = SceneObject::new(
            ObjectId(0),
            "DeformDim",
            NodePayload::Dimension(DimensionObject::linear(
                DimensionKind::Horizontal,
                Point2D::new(10.0, 10.0),
                Point2D::new(20.0, 10.0),
                5.0,
            )),
        );
        dim_obj.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(10.0, 5.0),
                Point2D::new(70.0, 5.0),
                Point2D::new(70.0, 60.0),
                Point2D::new(10.0, 60.0),
            ],
        });
        let dim_id = doc.add_object(dim_obj, Some(lid)).unwrap();
        let (d_id_bb, _) =
            render_isolated_object_to_rgba(&doc, 0, dim_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        assert!(
            d_id_bb.max_x >= 70.0 && d_id_bb.max_y >= 60.0,
            "Isolated deformed Dimension(IDENTITY) must enclose [10..70,5..60], got {d_id_bb:?}"
        );
        let exp_d_corner = rot_tr.transform_point(Point2D::new(70.0, 60.0));
        let (d_rot_bb, _) = render_isolated_object_to_rgba(&doc, 0, dim_id, rot_tr, 96.0)
            .unwrap()
            .unwrap();
        assert!(
            d_rot_bb.contains_point(exp_d_corner),
            "Isolated deformed Dimension(translate*rotate) {d_rot_bb:?} must enclose {exp_d_corner:?}"
        );

        // 7c. SymbolInstance(Group([10..30, 10..30]) + Envelope([10..60, 10..50]))
        let mut sym_leaf = make_rect(10.0, 10.0, 20.0, 20.0, blue);
        sym_leaf.id = ObjectId(201);
        let mut sym_grp = SceneObject::new(
            ObjectId(202),
            "SymEnvGrp",
            NodePayload::Group(vec![ObjectId(201)]),
        );
        sym_grp.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(10.0, 10.0),
                Point2D::new(60.0, 10.0),
                Point2D::new(60.0, 50.0),
                Point2D::new(10.0, 50.0),
            ],
        });
        let sym = SymbolDef::new("IsoDeformSym", vec![sym_leaf, sym_grp]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let inst = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "IsoDeformSymInst",
                    NodePayload::SymbolInstance(sym_id),
                ),
                Some(lid),
            )
            .unwrap();
        let (s_id_bb, s_id_buf) =
            render_isolated_object_to_rgba(&doc, 0, inst, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        assert!(
            s_id_bb.max_x >= 60.0 && s_id_bb.max_y >= 50.0,
            "Isolated SymbolInstance(Group+Envelope)(IDENTITY) must enclose [10..60,10..50], got {s_id_bb:?}"
        );
        let px_per_mm: f64 = 96.0 / 25.4;
        let sx = ((50.0 - s_id_bb.min_x) * px_per_mm) as u32;
        let sy = ((40.0 - s_id_bb.min_y) * px_per_mm) as u32;
        assert_eq!(
            s_id_buf.get_pixel(sx, sy).unwrap(),
            [0, 0, 255, 255],
            "Isolated SymbolInstance(Group+Envelope) must render Blue ink at deformed (50,40)mm"
        );
        let exp_s_corner = rot_tr.transform_point(Point2D::new(60.0, 50.0));
        let (s_rot_bb, _) = render_isolated_object_to_rgba(&doc, 0, inst, rot_tr, 96.0)
            .unwrap()
            .unwrap();
        assert!(
            s_rot_bb.contains_point(exp_s_corner),
            "Isolated SymbolInstance(Group+Envelope)(translate*rotate) {s_rot_bb:?} must enclose {exp_s_corner:?}"
        );
    }

    // 8. reviewer-r21-2: compute_children_local_bounds inside SymbolDef Group(opacity=0.5, SingleArc) with Connector
    {
        let mut doc = Document::new("R22 SymGrpConnArc", 500.0, 500.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut a = make_rect(0.0, 0.0, 10.0, 10.0, red.clone());
        a.id = ObjectId(101);
        let mut b = make_rect(90.0, 190.0, 10.0, 10.0, red);
        b.id = ObjectId(102);
        let mut conn = SceneObject::new(
            ObjectId(103),
            "SymArcConn",
            NodePayload::Connector(ConnectorObject::between_objects(
                ObjectId(101),
                AnchorPoint::Center,
                ObjectId(102),
                AnchorPoint::Center,
                ConnectorStyle::Straight,
            )),
        )
        .with_outline(OutlineStyle::solid(black, 2.0));
        conn.id = ObjectId(103);
        let mut grp = SceneObject::new(
            ObjectId(104),
            "SymArcConnGrp",
            NodePayload::Group(vec![ObjectId(101), ObjectId(102), ObjectId(103)]),
        );
        grp.opacity = 0.5;
        grp.effects.push(Effect::Envelope {
            mode: EnvelopeMode::SingleArc,
            cage: vec![
                Point2D::new(0.0, 350.0),
                Point2D::new(100.0, 350.0),
                Point2D::new(100.0, 370.0),
                Point2D::new(0.0, 370.0),
            ],
        });
        let sym = SymbolDef::new("SymConnArc", vec![a, b, conn, grp]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "SymConnArcInst",
                NodePayload::SymbolInstance(sym_id),
            ),
            Some(lid),
        )
        .unwrap();
        let vp = Viewport::for_export(600, 1600, 96.0);
        let buf = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        let mid_px = sample_mm(&buf, 50.0, 360.0);
        assert_ne!(
            mid_px,
            [255, 255, 255, 255],
            "SymbolDef Group(opacity=0.5,SingleArc) with Connector must render deformed ink, got {mid_px:?}"
        );
    }
}

#[test]
fn w1b_r23_symbol_lens_backdrop_scaled_distort_tiles_connector_effects_and_top_parent_xf() {
    use coree_core::{
        AnchorPoint, BlendMode, ConnectorObject, ConnectorStyle, DistortKind, EnvelopeMode,
        LensConfig, LensKind, ParametricShape, TextOnPath,
    };
    use coree_render::{
        convert_object_to_bitmap_with_bounds, render_isolated_object_to_rgba, DirtyTileCache,
        RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);
    let black = Color::BLACK;

    let make_rect = |x: f64, y: f64, w: f64, h: f64, fill: Color| -> SceneObject {
        SceneObject::new(
            ObjectId(0),
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(fill))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(x, y))
    };

    let render_direct_rgba =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut r = SceneRenderer::new();
            let mut b = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
            r.render_document(doc, vp, ViewMode::Enhanced, opts, &mut b);
            b.into_rgba()
        };

    let render_tiled_rgba =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
            cache.mark_all_dirty();
            let mut r = SceneRenderer::new();
            cache.render_viewport(doc, vp, ViewMode::Enhanced, opts, &mut r)
        };

    let count_diff = |a: &coree_render::RgbaBuffer, b: &coree_render::RgbaBuffer| -> usize {
        a.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.rgba.as_chunks::<4>().0.iter())
            .filter(|(p, q)| p != q)
            .count()
    };

    let sample_mm = |buf: &coree_render::RgbaBuffer, x_mm: f64, y_mm: f64| -> [u8; 4] {
        let px_per_mm: f64 = 96.0 / 25.4;
        let px = (x_mm * px_per_mm).round() as u32;
        let py = (y_mm * px_per_mm).round() as u32;
        buf.get_pixel(px, py).unwrap()
    };

    let ink_bbox_mm = |bb: Rect2D, buf: &coree_render::RgbaBuffer| -> Option<Rect2D> {
        let px_per_mm_x = f64::from(buf.width) / bb.width().max(1e-6);
        let px_per_mm_y = f64::from(buf.height) / bb.height().max(1e-6);
        let mut acc: Option<Rect2D> = None;
        for y in 0..buf.height {
            for x in 0..buf.width {
                if buf.get_pixel(x, y).unwrap()[3] > 20 {
                    let mx = bb.min_x + (f64::from(x) + 0.5) / px_per_mm_x;
                    let my = bb.min_y + (f64::from(y) + 0.5) / px_per_mm_y;
                    let pt_r = Rect2D::new(mx, my, mx, my);
                    acc = Some(match acc {
                        Some(prev) => prev.union(pt_r),
                        None => pt_r,
                    });
                }
            }
        }
        acc
    };

    // 1. challenger2-r22-1 + R2 minors: SymbolInstance + Lens (flat & Group(Envelope), frozen, remove_face, opacity, Magnify)
    for use_deformed_group in [false, true] {
        for &(frozen, remove_face, lens_op, is_magnify) in &[
            (true, false, 1.0_f32, false),
            (false, true, 1.0_f32, false),
            (false, false, 0.5_f32, false),
            (false, false, 1.0_f32, true),
        ] {
            let mut doc = Document::new("R23 SymLens", 300.0, 300.0, ColorMode::Rgb, 96.0);
            let lid = doc.pages()[0].layers[0].id;
            let mut bg_red = make_rect(10.0, 10.0, 40.0, 40.0, red.clone());
            bg_red.id = ObjectId(101);
            let lens_kind = if is_magnify {
                LensKind::Magnify { amount: 2.0 }
            } else {
                LensKind::Invert
            };
            let mut lens_obj = SceneObject::new(
                ObjectId(102),
                "SymLens",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 40.0,
                    height: 40.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(30.0, 10.0));
            lens_obj.opacity = lens_op;
            lens_obj.lens = Some(LensConfig {
                kind: lens_kind,
                frozen,
                viewpoint: None,
                remove_face,

                frozen_snapshot: None,
            });
            let objs = if use_deformed_group {
                let mut grp = SceneObject::new(
                    ObjectId(103),
                    "SymDefGrp",
                    NodePayload::Group(vec![ObjectId(101), ObjectId(102)]),
                );
                grp.effects.push(Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 10.0),
                        Point2D::new(120.0, 10.0),
                        Point2D::new(120.0, 90.0),
                        Point2D::new(10.0, 90.0),
                    ],
                });
                vec![bg_red, lens_obj, grp]
            } else {
                vec![bg_red, lens_obj]
            };
            let sym = SymbolDef::new("LensSym", objs);
            let sym_id = sym.id;
            doc.insert_symbol(sym);
            let inst = doc
                .add_object(
                    SceneObject::new(
                        ObjectId(0),
                        "LensSymInst",
                        NodePayload::SymbolInstance(sym_id),
                    ),
                    Some(lid),
                )
                .unwrap();

            // Sample point inside overlap (both Red & Lens) and lens-only region (outside Red)
            let (ov_x, ov_y, empty_x, empty_y) = if use_deformed_group {
                (65.0, 50.0, 105.0, 50.0)
            } else {
                (40.0, 30.0, 60.0, 30.0)
            };

            let vp = Viewport::for_export(600, 600, 96.0);
            let opts = RenderOptions::export();
            let direct = render_direct_rgba(&doc, &vp, &opts);
            let tiled = render_tiled_rgba(&doc, &vp, &opts);
            let diff = count_diff(&direct, &tiled);
            assert_eq!(
                diff, 0,
                "SymbolInstance(def_grp={use_deformed_group}, frozen={frozen}, rm={remove_face}, op={lens_op}, mag={is_magnify}) DirtyTileCache diff must be 0, got {diff}"
            );

            let (iso_bb, iso_buf) =
                render_isolated_object_to_rgba(&doc, 0, inst, Affine2D::IDENTITY, 96.0)
                    .unwrap()
                    .unwrap();
            let ix = ((ov_x - iso_bb.min_x) / iso_bb.width() * f64::from(iso_buf.width)) as u32;
            let iy = ((ov_y - iso_bb.min_y) / iso_bb.height() * f64::from(iso_buf.height)) as u32;
            let iso_px = iso_buf.get_pixel(ix, iy).unwrap();
            let dir_px = sample_mm(&direct, ov_x, ov_y);

            if is_magnify {
                assert_eq!(
                    dir_px,
                    [255, 0, 0, 255],
                    "SymbolInstance Magnify overlap must be Red"
                );
                assert_eq!(
                    iso_px,
                    [255, 0, 0, 255],
                    "Isolated SymbolInstance Magnify overlap must be Red"
                );
            } else if (lens_op - 0.5).abs() < 0.05 {
                // 50%-opaque Cyan [0, 255, 255] over Red [255, 0, 0] inside SymbolInstance = [128, 128, 128, 255]
                assert_eq!(dir_px, [128, 128, 128, 255]);
                assert_eq!(iso_px, [128, 128, 128, 255]);
            } else {
                assert_eq!(
                    dir_px,
                    [0, 255, 255, 255],
                    "SymbolInstance Invert(frozen={frozen}, rm={remove_face}) overlap must be Cyan"
                );
                assert_eq!(
                    iso_px,
                    [0, 255, 255, 255],
                    "Isolated SymbolInstance Invert(frozen={frozen}, rm={remove_face}) overlap must be Cyan"
                );
                if remove_face {
                    // Empty area under lens (outside Red) must remain transparent/white page, not inverted black!
                    let empty_dir = sample_mm(&direct, empty_x, empty_y);
                    assert_eq!(
                        empty_dir,
                        [255, 255, 255, 255],
                        "remove_face=true outside Red must preserve white page"
                    );
                    let ex = ((empty_x - iso_bb.min_x) / iso_bb.width() * f64::from(iso_buf.width))
                        as u32;
                    let ey = ((empty_y - iso_bb.min_y) / iso_bb.height()
                        * f64::from(iso_buf.height)) as u32;
                    assert_eq!(
                        iso_buf.get_pixel(ex, ey).unwrap()[3],
                        0,
                        "Isolated remove_face=true outside Red must have alpha=0"
                    );
                }
            }

            if !use_deformed_group && frozen {
                let (bmp, _) = convert_object_to_bitmap_with_bounds(&doc, inst, 96.0).unwrap();
                assert!(
                    bmp.to_rgba8_vec()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|p| p == &[0, 255, 255, 255]),
                    "convert_object_to_bitmap on SymbolInstance(frozen Lens) must contain Cyan"
                );
            }
        }
    }

    // 1b. SymbolInstance wrapping Group(opacity=0.5, Envelope) + [Red, Invert] (render_isolated_object_to_rgba + DirtyTileCache)
    {
        let mut doc = Document::new("R23 SymGrpOp05Lens", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut bg_red = make_rect(10.0, 10.0, 40.0, 40.0, red.clone());
        bg_red.id = ObjectId(301);
        let mut lens_obj = SceneObject::new(
            ObjectId(302),
            "SymGrpLens",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 40.0,
                height: 40.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(30.0, 10.0));
        lens_obj.lens = Some(LensConfig::new(LensKind::Invert));
        let mut grp = SceneObject::new(
            ObjectId(303),
            "SymOp05EnvGrp",
            NodePayload::Group(vec![ObjectId(301), ObjectId(302)]),
        );
        grp.opacity = 0.5;
        grp.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(10.0, 10.0),
                Point2D::new(120.0, 10.0),
                Point2D::new(120.0, 90.0),
                Point2D::new(10.0, 90.0),
            ],
        });
        let sym = SymbolDef::new("SymOp05Lens", vec![bg_red, lens_obj, grp]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let inst = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "SymOp05LensInst",
                    NodePayload::SymbolInstance(sym_id),
                ),
                Some(lid),
            )
            .unwrap();
        let vp = Viewport::for_export(600, 600, 96.0);
        let opts = RenderOptions::export();
        let direct = render_direct_rgba(&doc, &vp, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);
        assert_eq!(sample_mm(&direct, 65.0, 50.0), [128, 255, 255, 255]);
        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, inst, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let ix = ((65.0 - iso_bb.min_x) / iso_bb.width() * f64::from(iso_buf.width)) as u32;
        let iy = ((50.0 - iso_bb.min_y) / iso_bb.height() * f64::from(iso_buf.height)) as u32;
        assert_eq!(iso_buf.get_pixel(ix, iy).unwrap(), [0, 255, 255, 128]);
    }

    // 2. challenger2-r22-2: 10x scaled PushPull Distort & off-center Twister in DirtyTileCache (0 diff px)
    for distort_kind in [DistortKind::PushPull, DistortKind::Twister] {
        let mut doc = Document::new("R23 ScaledDistortTiles", 800.0, 800.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut leaf = make_rect(4.0, 4.0, 4.0, 4.0, red.clone());
        leaf.effects.push(Effect::Distort {
            kind: distort_kind,
            amplitude: if matches!(distort_kind, DistortKind::PushPull) {
                50.0
            } else {
                15.0
            },
            frequency: 4.0,
            center: if matches!(distort_kind, DistortKind::PushPull) {
                Point2D::new(6.0, 6.0)
            } else {
                Point2D::new(-4.0, 6.0)
            },
        });
        let leaf_id = doc.add_object(leaf, Some(lid)).unwrap();
        let grp = doc.group_objects(&[leaf_id], "ScaledDistortGrp").unwrap();
        doc.set_object_transform(grp, Affine2D::scale(10.0, 10.0))
            .unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::SetOpacity {
                ids: vec![grp],
                opacity: 0.5,
            },
        )
        .unwrap();

        let vp = Viewport::for_export(800, 800, 96.0);
        let opts = RenderOptions::export();
        let direct = render_direct_rgba(&doc, &vp, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        let big_diffs: Vec<_> = direct
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(tiled.rgba.as_chunks::<4>().0.iter())
            .enumerate()
            .filter(|(_, (p, q))| p.iter().zip(q.iter()).any(|(&a, &b)| a.abs_diff(b) > 16))
            .collect();
        assert_eq!(
            big_diffs.len(),
            0,
            "10x scaled Distort({distort_kind:?}) DirtyTileCache across 800x800 tiles must have 0 dropped/mismatched (>16 LSB) pixels, got {} (first={:?})",
            big_diffs.len(),
            big_diffs.first()
        );
    }

    // 3. challenger2-r22-3: Standalone Connector honors obj.effects (Envelope)
    {
        let mut doc = Document::new("R23 ConnEffects", 120.0, 100.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut conn = ConnectorObject::between_objects(
            ObjectId(999),
            AnchorPoint::Center,
            ObjectId(999),
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        );
        conn.from_obj = None;
        conn.to_obj = None;
        conn.from_fallback = Point2D::new(10.0, 15.0);
        conn.to_fallback = Point2D::new(30.0, 25.0);
        let mut conn_obj = SceneObject::new(ObjectId(0), "EnvConn", NodePayload::Connector(conn))
            .with_outline(OutlineStyle::solid(blue, 2.0));
        conn_obj.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(10.0, 10.0),
                Point2D::new(90.0, 10.0),
                Point2D::new(90.0, 70.0),
                Point2D::new(10.0, 70.0),
            ],
        });
        let conn_id = doc.add_object(conn_obj, Some(lid)).unwrap();

        let vp = Viewport::for_export(454, 378, 96.0);
        let direct = render_direct_rgba(&doc, &vp, &RenderOptions::export());
        // Diagonal (10,15)->(30,25) mapped into [10..90, 10..70] lands on (10,10)->(90,70), passing through (50,40)!
        assert_eq!(
            sample_mm(&direct, 50.0, 40.0),
            [0, 0, 255, 255],
            "Standalone Connector+Envelope must render deformed blue ink at (50,40)mm"
        );
        assert_eq!(
            sample_mm(&direct, 20.0, 20.0),
            [255, 255, 255, 255],
            "Standalone Connector+Envelope must not render at undeformed (20,20)mm"
        );

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, conn_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let ink = ink_bbox_mm(iso_bb, &iso_buf).unwrap();
        assert!(
            ink.min_x < 12.0 && ink.max_x > 85.0 && ink.min_y < 12.0 && ink.max_y > 65.0,
            "Isolated standalone Connector+Envelope ink bbox {ink:?} must span deformed [10..90, 10..70]"
        );
    }

    // 4. challenger2-r22-4: Standalone TextOnPath with non-identity parent_xf in render_isolated_object_to_rgba
    {
        let mut doc = Document::new("R23 TopParentXf", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let host = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Host",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(10.0, 20.0),
                        Point2D::new(70.0, 20.0),
                    )),
                )
                .with_outline(OutlineStyle::none()),
                Some(lid),
            )
            .unwrap();
        let mut top_obj = SceneObject::new(
            ObjectId(0),
            "EnvTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain(
                    "HELLO",
                    TextStyle {
                        font_size_pt: 24.0,
                        fill: black,
                        ..TextStyle::default()
                    },
                ),
                path_id: host,
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        top_obj.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(10.0, 10.0),
                Point2D::new(90.0, 10.0),
                Point2D::new(90.0, 70.0),
                Point2D::new(10.0, 70.0),
            ],
        });
        let top_id = doc.add_object(top_obj, Some(lid)).unwrap();

        let rot_tr = Affine2D::translate(50.0, 50.0).compose(&Affine2D::rotate_deg(30.0));
        let (rot_bb, rot_buf) = render_isolated_object_to_rgba(&doc, 0, top_id, rot_tr, 96.0)
            .unwrap()
            .unwrap();
        let rot_ink = ink_bbox_mm(rot_bb, &rot_buf).expect("non-empty rotated TextOnPath ink");
        let exp_cage = rot_tr
            .try_transform_rect(Rect2D::from_xywh(10.0, 10.0, 80.0, 60.0))
            .unwrap();
        assert!(
            rot_ink.min_x >= exp_cage.min_x - 2.0
                && rot_ink.min_y >= exp_cage.min_y - 2.0
                && rot_ink.max_x <= exp_cage.max_x + 2.0
                && rot_ink.max_y <= exp_cage.max_y + 2.0,
            "Standalone TextOnPath+Envelope under parent_xf={rot_tr:?} ink {rot_ink:?} must lie inside transformed cage {exp_cage:?}"
        );
        assert!(
            rot_ink.min_x > 20.0 && rot_ink.min_y > 55.0,
            "Standalone TextOnPath+Envelope under parent_xf must be translated+rotated (not left at [10..90,10..70]): {rot_ink:?}"
        );

        // Also test undeformed TextOnPath under non-identity parent_xf
        let plain_top_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "PlainTOP",
                    NodePayload::TextOnPath(TextOnPath {
                        text: ArtisticText::from_plain(
                            "WORLD",
                            TextStyle {
                                font_size_pt: 20.0,
                                fill: Color::BLACK,
                                ..TextStyle::default()
                            },
                        ),
                        path_id: host,
                        offset_mm: 0.0,
                        distance_mm: 0.0,
                        flip_vertical: false,
                        flip_horizontal: false,
                    }),
                ),
                Some(lid),
            )
            .unwrap();
        let (p_rot_bb, p_rot_buf) =
            render_isolated_object_to_rgba(&doc, 0, plain_top_id, rot_tr, 96.0)
                .unwrap()
                .unwrap();
        let p_rot_ink = ink_bbox_mm(p_rot_bb, &p_rot_buf).expect("non-empty plain rotated TOP ink");
        assert!(
            p_rot_ink.min_x > 35.0 && p_rot_ink.min_y > 50.0,
            "Standalone undeformed TextOnPath under parent_xf must transform ink into {p_rot_bb:?}, got {p_rot_ink:?}"
        );
    }

    // Suppress unused BlendMode import if needed by verifying lens blend mode compatibility
    let _ = BlendMode::Normal;
}

#[test]
fn w1b_r24_frozen_lens_key_isolation_for_symbols_zero_ids_and_symmetry() {
    use coree_core::{LensConfig, LensKind, ParametricShape, SymbolDef};
    use coree_render::{DirtyTileCache, RenderOptions, SceneRenderer};

    let red = Color::rgba8(255, 0, 0, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);

    let make_rect = |id: ObjectId, x: f64, y: f64, w: f64, h: f64, fill: &Color| -> SceneObject {
        SceneObject::new(
            id,
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(fill.clone()))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(x, y))
    };

    let render_direct_rgba =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut r = SceneRenderer::new();
            let mut b = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
            r.render_document(doc, vp, ViewMode::Enhanced, opts, &mut b);
            b.into_rgba()
        };

    let render_tiled_rgba =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
            cache.mark_all_dirty();
            let mut r = SceneRenderer::new();
            cache.render_viewport(doc, vp, ViewMode::Enhanced, opts, &mut r)
        };

    let count_diff = |a: &coree_render::RgbaBuffer, b: &coree_render::RgbaBuffer| -> usize {
        a.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.rgba.as_chunks::<4>().0.iter())
            .filter(|(p, q)| p != q)
            .count()
    };

    let sample_mm = |buf: &coree_render::RgbaBuffer, x_mm: f64, y_mm: f64| -> [u8; 4] {
        let px_per_mm: f64 = 96.0 / 25.4;
        let px = (x_mm * px_per_mm).round() as u32;
        let py = (y_mm * px_per_mm).round() as u32;
        buf.get_pixel(px, py).unwrap()
    };

    let opts = RenderOptions::export();
    // 520x260px at 96 DPI spans two 256px tile columns (seam at ~67.73mm)
    let vp = Viewport::for_export(520, 260, 96.0);

    // 1. Two SymbolInstances of the same SymbolDef containing a frozen Lens (ObjectId(77))
    // positioned over Red (crossing x=256px seam) and Green (in second tile column)
    {
        let mut doc = Document::new("TwoSymInstFrozen", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(ObjectId(0), 5.0, 5.0, 75.0, 50.0, &red),
            Some(lid),
        )
        .unwrap();
        doc.add_object(
            make_rect(ObjectId(0), 85.0, 5.0, 45.0, 50.0, &green),
            Some(lid),
        )
        .unwrap();

        let mut sym_lens = make_rect(ObjectId(77), 0.0, 0.0, 30.0, 30.0, &Color::TRANSPARENT);
        sym_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let sym = SymbolDef::new("FrozenLensSym", vec![sym_lens]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);

        doc.add_object(
            SceneObject::new(ObjectId(0), "Inst1", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(52.0, 15.0)),
            Some(lid),
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(ObjectId(0), "Inst2", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(92.0, 15.0)),
            Some(lid),
        )
        .unwrap();

        let direct = render_direct_rgba(&doc, &vp, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_diff(&direct, &tiled),
            0,
            "Two SymbolInstances with frozen lens must produce 0 diff px between render_document and DirtyTileCache"
        );
        // Inst1 over Red -> Cyan [0, 255, 255, 255] on both sides of x=256px seam (67.73mm)
        assert_eq!(sample_mm(&direct, 60.0, 30.0), [0, 255, 255, 255]);
        assert_eq!(sample_mm(&direct, 72.0, 30.0), [0, 255, 255, 255]);
        // Inst2 over Green -> Magenta [255, 0, 255, 255]
        assert_eq!(sample_mm(&direct, 105.0, 30.0), [255, 0, 255, 255]);
    }

    // 2. Two ObjectId(0) frozen:true lenses inside a single SymbolDef over Red and Blue
    {
        let mut doc = Document::new("TwoZeroIdFrozenInSym", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(ObjectId(0), 5.0, 5.0, 50.0, 50.0, &red),
            Some(lid),
        )
        .unwrap();
        doc.add_object(
            make_rect(ObjectId(0), 65.0, 5.0, 50.0, 50.0, &blue),
            Some(lid),
        )
        .unwrap();

        let mut lens1 = make_rect(ObjectId(0), 10.0, 10.0, 30.0, 30.0, &Color::TRANSPARENT);
        lens1.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let mut lens2 = make_rect(ObjectId(0), 70.0, 10.0, 30.0, 30.0, &Color::TRANSPARENT);
        lens2.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let sym = SymbolDef::new("DualZeroIdFrozenSym", vec![lens1, lens2]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
            Some(lid),
        )
        .unwrap();

        let direct = render_direct_rgba(&doc, &vp, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_diff(&direct, &tiled),
            0,
            "Dual ObjectId(0) frozen lenses in one SymbolDef must produce 0 diff px"
        );
        assert_eq!(sample_mm(&direct, 25.0, 25.0), [0, 255, 255, 255]);
        assert_eq!(sample_mm(&direct, 85.0, 25.0), [255, 255, 0, 255]);
    }

    // 3. Effect::Symmetry on Lens(frozen=true) over Red (left) and Green (right)
    {
        let mut doc = Document::new("SymmetryFrozenLens", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(ObjectId(0), 5.0, 5.0, 50.0, 50.0, &red),
            Some(lid),
        )
        .unwrap();
        doc.add_object(
            make_rect(ObjectId(0), 65.0, 5.0, 50.0, 50.0, &green),
            Some(lid),
        )
        .unwrap();

        let mut sym_lens = make_rect(ObjectId(0), 15.0, 15.0, 30.0, 30.0, &Color::TRANSPARENT);
        sym_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        sym_lens.effects.push(Effect::Symmetry {
            axes: 1,
            mirror: true,
            origin: Point2D::new(60.0, 30.0),
        });
        doc.add_object(sym_lens, Some(lid)).unwrap();

        let direct = render_direct_rgba(&doc, &vp, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        assert_eq!(
            count_diff(&direct, &tiled),
            0,
            "Effect::Symmetry on frozen lens must produce 0 diff px between render_document and DirtyTileCache"
        );
        assert_eq!(sample_mm(&direct, 30.0, 30.0), [0, 255, 255, 255]);
        assert_eq!(sample_mm(&direct, 90.0, 30.0), [255, 0, 255, 255]);
    }
}

#[test]
fn w1b_r24_symbol_instance_opacity_blend_mode_and_deformed_zero_id_roots() {
    use coree_core::{BlendMode, EnvelopeMode, LensConfig, LensKind, ParametricShape, SymbolDef};
    use coree_render::{
        render_isolated_object_to_rgba, DirtyTileCache, RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);

    let make_rect = |id: ObjectId, x: f64, y: f64, w: f64, h: f64, fill: &Color| -> SceneObject {
        SceneObject::new(
            id,
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(fill.clone()))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(x, y))
    };

    let render_rgba = |doc: &Document,
                       vp: &Viewport,
                       mode: ViewMode,
                       opts: &RenderOptions|
     -> coree_render::RgbaBuffer {
        let mut r = SceneRenderer::new();
        let mut b = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
        r.render_document(doc, vp, mode, opts, &mut b);
        b.into_rgba()
    };

    let render_tiled_rgba =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
            cache.mark_all_dirty();
            let mut r = SceneRenderer::new();
            cache.render_viewport(doc, vp, ViewMode::Enhanced, opts, &mut r)
        };

    let count_diff = |a: &coree_render::RgbaBuffer, b: &coree_render::RgbaBuffer| -> usize {
        a.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.rgba.as_chunks::<4>().0.iter())
            .filter(|(p, q)| p != q)
            .count()
    };

    let sample_mm = |buf: &coree_render::RgbaBuffer, x_mm: f64, y_mm: f64| -> [u8; 4] {
        let px_per_mm: f64 = 96.0 / 25.4;
        let px = (x_mm * px_per_mm).round() as u32;
        let py = (y_mm * px_per_mm).round() as u32;
        buf.get_pixel(px, py).unwrap()
    };

    let opts = RenderOptions::export();
    let vp = Viewport::for_export(520, 700, 96.0);

    // 1. Standalone SymbolInstance with opacity=0.5 (direct, tiled, isolated, and wireframe)
    {
        let mut doc = Document::new("SymOpacity", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let sym = SymbolDef::new(
            "RedSym",
            vec![make_rect(ObjectId(0), 10.0, 10.0, 80.0, 60.0, &red)],
        );
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let mut inst_obj =
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id));
        inst_obj.opacity = 0.5;
        let inst_id = doc.add_object(inst_obj, Some(lid)).unwrap();

        let direct = render_rgba(&doc, &vp, ViewMode::Enhanced, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);
        let px = sample_mm(&direct, 50.0, 40.0);
        assert!(
            px[0] == 255 && (px[1] as i16 - 128).abs() <= 2 && (px[2] as i16 - 128).abs() <= 2,
            "SymbolInstance(opacity=0.5) over white must render ~[255,128,128,255], got {px:?}"
        );

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, inst_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let iso_px = iso_buf
            .get_pixel(iso_buf.width / 2, iso_buf.height / 2)
            .unwrap();
        assert!(
            iso_bb.contains_point(Point2D::new(50.0, 40.0))
                && iso_px[0] == 255
                && iso_px[1] == 0
                && iso_px[2] == 0
                && (iso_px[3] as i16 - 128).abs() <= 2,
            "Isolated SymbolInstance(opacity=0.5) must have alpha ~128, got {iso_px:?}"
        );

        let wire = render_rgba(&doc, &vp, ViewMode::Wireframe, &opts);
        assert_ne!(
            sample_mm(&wire, 10.0, 40.0),
            [255, 255, 255, 255],
            "Wireframe mode must still stroke SymbolInstance outline"
        );
    }

    // 2. SymbolInstance with blend_mode=Multiply over Blue
    {
        let mut doc = Document::new("SymMultiply", 300.0, 300.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(ObjectId(0), 5.0, 5.0, 100.0, 80.0, &blue),
            Some(lid),
        )
        .unwrap();
        let sym = SymbolDef::new(
            "RedSym",
            vec![make_rect(ObjectId(0), 10.0, 10.0, 80.0, 60.0, &red)],
        );
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let mut inst_obj =
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id));
        inst_obj.blend_mode = BlendMode::Multiply;
        doc.add_object(inst_obj, Some(lid)).unwrap();

        let direct = render_rgba(&doc, &vp, ViewMode::Enhanced, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);
        assert_eq!(
            sample_mm(&direct, 50.0, 40.0),
            [0, 0, 0, 255],
            "Red SymbolInstance with Multiply over Blue must produce Black"
        );
    }

    // 3. Group(Envelope) -> SymbolInstance(opacity=0.5) and SymbolInstance with ObjectId(0) roots (shape + frozen lens)
    {
        let mut doc = Document::new(
            "DefGrpSymZeroIdAndOpacity",
            300.0,
            300.0,
            ColorMode::Rgb,
            96.0,
        );
        let lid = doc.pages()[0].layers[0].id;
        let sym_green = SymbolDef::new(
            "ZeroGreenSym",
            vec![make_rect(ObjectId(0), 10.0, 10.0, 80.0, 60.0, &green)],
        );
        let sym_green_id = sym_green.id;
        doc.insert_symbol(sym_green);

        let mut zero_lens = make_rect(ObjectId(0), 10.0, 10.0, 80.0, 60.0, &Color::TRANSPARENT);
        zero_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let sym_lens = SymbolDef::new(
            "ZeroRedFrozenLensSym",
            vec![
                make_rect(ObjectId(0), 10.0, 10.0, 80.0, 60.0, &red),
                zero_lens,
            ],
        );
        let sym_lens_id = sym_lens.id;
        doc.insert_symbol(sym_lens);

        let mut inst_green = SceneObject::new(
            ObjectId(0),
            "InstGreen",
            NodePayload::SymbolInstance(sym_green_id),
        );
        inst_green.opacity = 0.5;
        let inst_green_id = doc.add_object(inst_green, Some(lid)).unwrap();
        let grp_green_id = doc.group_objects(&[inst_green_id], "EnvGrpGreen").unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: grp_green_id,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 10.0),
                        Point2D::new(90.0, 10.0),
                        Point2D::new(110.0, 80.0),
                        Point2D::new(10.0, 80.0),
                    ],
                },
            },
        )
        .unwrap();

        let inst_lens_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "InstLens",
                    NodePayload::SymbolInstance(sym_lens_id),
                )
                .with_transform(Affine2D::translate(0.0, 90.0)),
                Some(lid),
            )
            .unwrap();
        let grp_lens_id = doc.group_objects(&[inst_lens_id], "EnvGrpLens").unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: grp_lens_id,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 100.0),
                        Point2D::new(90.0, 100.0),
                        Point2D::new(110.0, 170.0),
                        Point2D::new(10.0, 170.0),
                    ],
                },
            },
        )
        .unwrap();

        let direct = render_rgba(&doc, &vp, ViewMode::Enhanced, &opts);
        let tiled = render_tiled_rgba(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);

        let gpx = sample_mm(&direct, 50.0, 45.0);
        assert!(
            (gpx[0] as i16 - 128).abs() <= 2 && gpx[1] == 255 && (gpx[2] as i16 - 128).abs() <= 2,
            "Deformed Group(Envelope) -> SymbolInstance(ObjectId(0) Green, opacity=0.5) must render ~[128,255,128,255], got {gpx:?}"
        );
        // Extended envelope corner at x=95, y=72 must contain deformed green ink (not dropped!)
        let gpx_ext = sample_mm(&direct, 95.0, 72.0);
        assert!(
            gpx_ext[0] < 200 && gpx_ext[1] == 255,
            "Deformed Envelope shear region must contain ObjectId(0) symbol root ink, got {gpx_ext:?}"
        );

        let lpx = sample_mm(&direct, 50.0, 135.0);
        assert_eq!(
            lpx,
            [0, 255, 255, 255],
            "Deformed Group(Envelope) -> SymbolInstance(ObjectId(0) Red + ObjectId(0) frozen Invert) must render Cyan"
        );
        let (_, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, grp_lens_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let has_cyan = iso_buf
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] < 20 && p[1] > 235 && p[2] > 235 && p[3] > 235);
        assert!(
            has_cyan,
            "Isolated Group(Envelope)->SymbolInstance(ObjectId(0) frozen Invert) must contain Cyan"
        );
    }
}

#[test]
fn w1b_r25_symmetry_container_chained_frozen_lenses_and_isolated_backdrop() {
    use coree_core::{CommandOp, LensConfig, LensKind, ParametricShape, SymbolDef};
    use coree_render::{
        render_isolated_object_to_rgba, DirtyTileCache, RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let make_rect = |id: ObjectId, x: f64, y: f64, w: f64, h: f64, fill: &Color| -> SceneObject {
        SceneObject::new(
            id,
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(fill.clone()))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(x, y))
    };
    let render_direct =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut r = SceneRenderer::new();
            let mut b = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
            r.render_document(doc, vp, ViewMode::Enhanced, opts, &mut b);
            b.into_rgba()
        };
    let render_tiled =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
            cache.mark_all_dirty();
            let mut r = SceneRenderer::new();
            cache.render_viewport(doc, vp, ViewMode::Enhanced, opts, &mut r)
        };
    let count_diff = |a: &coree_render::RgbaBuffer, b: &coree_render::RgbaBuffer| -> usize {
        a.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.rgba.as_chunks::<4>().0.iter())
            .filter(|(p, q)| p != q)
            .count()
    };
    let sample_mm = |buf: &coree_render::RgbaBuffer, x_mm: f64, y_mm: f64| -> [u8; 4] {
        let px_per_mm: f64 = 96.0 / 25.4;
        let px = (x_mm * px_per_mm).round() as u32;
        let py = (y_mm * px_per_mm).round() as u32;
        buf.get_pixel(px, py).unwrap()
    };

    let opts = RenderOptions::export();
    let vp = Viewport::for_export(560, 300, 96.0);

    // P4b & P4c: SymbolInstance with Effect::Symmetry (opacity=0.99 and opacity=1.0)
    // containing chained [Lens1(Invert, frozen=true), Lens2(Invert, frozen=true)] over Red left / Green right
    for &opacity in &[0.99_f32, 1.0_f32] {
        let mut doc = Document::new("SymInstSymChain", 300.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(ObjectId(0), 10.0, 10.0, 45.0, 45.0, &red),
            Some(lid),
        )
        .unwrap();
        doc.add_object(
            make_rect(ObjectId(0), 80.0, 10.0, 45.0, 45.0, &green),
            Some(lid),
        )
        .unwrap();

        let mut l1 = make_rect(ObjectId(0), 15.0, 15.0, 35.0, 35.0, &Color::TRANSPARENT);
        l1.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let mut l2 = make_rect(ObjectId(0), 15.0, 15.0, 35.0, 35.0, &Color::TRANSPARENT);
        l2.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let sym = SymbolDef::new("ChainedFrozenSym", vec![l1, l2]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);

        let mut inst = SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id));
        inst.opacity = opacity;
        inst.effects.push(Effect::Symmetry {
            axes: 1,
            mirror: true,
            origin: Point2D::new(67.5, 32.5),
        });
        let inst_id = doc.add_object(inst, Some(lid)).unwrap();

        let direct = render_direct(&doc, &vp, &opts);
        let tiled = render_tiled(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);

        let left_px = sample_mm(&direct, 32.5, 32.5);
        let right_px = sample_mm(&direct, 102.5, 32.5);
        assert!(
            left_px[0] >= 250 && left_px[1] <= 5 && left_px[2] <= 5,
            "Left branch (opacity={opacity}) must double-invert Red back to Red, got {left_px:?}"
        );
        assert!(
            right_px[0] <= 5 && right_px[1] >= 250 && right_px[2] <= 5,
            "Right branch (opacity={opacity}) must double-invert Green back to Green, got {right_px:?}"
        );

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, inst_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let px_per_mm: f64 = 96.0 / 25.4;
        let rx = ((102.5 - iso_bb.min_x) * px_per_mm).round() as u32;
        let ry = ((32.5 - iso_bb.min_y) * px_per_mm).round() as u32;
        let iso_right = iso_buf.get_pixel(rx, ry).unwrap();
        assert!(
            iso_right[0] <= 5 && iso_right[1] >= 250 && iso_right[2] <= 5 && iso_right[3] >= 250,
            "Isolated right branch (opacity={opacity}) must render Green, got {iso_right:?}"
        );
    }

    // P5b: Group(Effect::Symmetry, opacity=0.99) containing [Lens1(Invert, frozen=true), Lens2(Invert, frozen=false)]
    {
        let mut doc = Document::new("GrpSymChain", 300.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(ObjectId(0), 10.0, 10.0, 45.0, 45.0, &red),
            Some(lid),
        )
        .unwrap();
        doc.add_object(
            make_rect(ObjectId(0), 80.0, 10.0, 45.0, 45.0, &green),
            Some(lid),
        )
        .unwrap();

        let mut l1 = make_rect(ObjectId(0), 15.0, 15.0, 35.0, 35.0, &Color::TRANSPARENT);
        l1.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let mut l2 = make_rect(ObjectId(0), 15.0, 15.0, 35.0, 35.0, &Color::TRANSPARENT);
        l2.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: false,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let l1_id = doc.add_object(l1, Some(lid)).unwrap();
        let l2_id = doc.add_object(l2, Some(lid)).unwrap();
        let grp_id = doc.group_objects(&[l1_id, l2_id], "SymGrp").unwrap();
        coree_core::apply_command(
            &mut doc,
            &CommandOp::SetOpacity {
                ids: vec![grp_id],
                opacity: 0.99,
            },
        )
        .unwrap();
        coree_core::apply_command(
            &mut doc,
            &CommandOp::AddEffect {
                id: grp_id,
                effect: Effect::Symmetry {
                    axes: 1,
                    mirror: true,
                    origin: Point2D::new(67.5, 32.5),
                },
            },
        )
        .unwrap();

        let direct = render_direct(&doc, &vp, &opts);
        let tiled = render_tiled(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);

        let left_px = sample_mm(&direct, 32.5, 32.5);
        let right_px = sample_mm(&direct, 102.5, 32.5);
        assert!(
            left_px[0] >= 250 && left_px[1] <= 5 && left_px[2] <= 5,
            "Group left branch must double-invert Red to Red, got {left_px:?}"
        );
        assert!(
            right_px[0] <= 5 && right_px[1] >= 250 && right_px[2] <= 5,
            "Group right branch must double-invert Green to Green, got {right_px:?}"
        );

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, grp_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let px_per_mm: f64 = 96.0 / 25.4;
        let rx = ((102.5 - iso_bb.min_x) * px_per_mm).round() as u32;
        let ry = ((32.5 - iso_bb.min_y) * px_per_mm).round() as u32;
        let iso_right = iso_buf.get_pixel(rx, ry).unwrap();
        assert!(
            iso_right[0] <= 5 && iso_right[1] >= 250 && iso_right[2] <= 5 && iso_right[3] >= 250,
            "Isolated Group right branch must render Green, got {iso_right:?}"
        );
    }
}

#[test]
fn w1b_r25_symbol_def_zero_id_powerclip_and_group_containers() {
    use coree_core::{EnvelopeMode, LensConfig, LensKind, ParametricShape, SymbolDef};
    use coree_render::{
        render_isolated_object_to_rgba, DirtyTileCache, RenderOptions, SceneRenderer,
    };

    let red = Color::rgba8(255, 0, 0, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let make_rect = |id: ObjectId, x: f64, y: f64, w: f64, h: f64, fill: &Color| -> SceneObject {
        SceneObject::new(
            id,
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(fill.clone()))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(x, y))
    };
    let render_direct =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut r = SceneRenderer::new();
            let mut b = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
            r.render_document(doc, vp, ViewMode::Enhanced, opts, &mut b);
            b.into_rgba()
        };
    let render_tiled =
        |doc: &Document, vp: &Viewport, opts: &RenderOptions| -> coree_render::RgbaBuffer {
            let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
            cache.mark_all_dirty();
            let mut r = SceneRenderer::new();
            cache.render_viewport(doc, vp, ViewMode::Enhanced, opts, &mut r)
        };
    let count_diff = |a: &coree_render::RgbaBuffer, b: &coree_render::RgbaBuffer| -> usize {
        a.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.rgba.as_chunks::<4>().0.iter())
            .filter(|(p, q)| p != q)
            .count()
    };
    let sample_mm = |buf: &coree_render::RgbaBuffer, x_mm: f64, y_mm: f64| -> [u8; 4] {
        let px_per_mm: f64 = 96.0 / 25.4;
        let px = (x_mm * px_per_mm).round() as u32;
        let py = (y_mm * px_per_mm).round() as u32;
        buf.get_pixel(px, py).unwrap()
    };

    let opts = RenderOptions::export();
    // 520x300px crosses x=256px tile seam (~67.73mm)
    let vp = Viewport::for_export(520, 300, 96.0);

    // 1. SymbolDef with ObjectId(0) PowerClip(fill=Green) -> [ObjectId(2) Lens::Invert(frozen=true)]
    {
        let mut doc = Document::new("ZeroIdPowerClipInSym", 300.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let mut inner_lens = make_rect(ObjectId(2), 55.0, 10.0, 40.0, 40.0, &Color::TRANSPARENT);
        inner_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let pc = SceneObject::new(
            ObjectId(0),
            "ZeroPC",
            NodePayload::PowerClip {
                container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                    55.0, 10.0, 40.0, 40.0,
                ))),
                container_parametric: None,
                lock_contents: true,
                contents: vec![ObjectId(2)],
            },
        )
        .with_fill(Fill::Solid(green.clone()))
        .with_outline(OutlineStyle::none());
        let sym = SymbolDef::new("ZeroPCSym", vec![pc, inner_lens]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
                Some(lid),
            )
            .unwrap();

        let direct = render_direct(&doc, &vp, &opts);
        let tiled = render_tiled(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);
        assert_eq!(sample_mm(&direct, 75.0, 30.0), [255, 0, 255, 255]);
        assert_eq!(sample_mm(&tiled, 75.0, 30.0), [255, 0, 255, 255]);

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, inst_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let px_per_mm: f64 = 96.0 / 25.4;
        let ix = ((75.0 - iso_bb.min_x) * px_per_mm).round() as u32;
        let iy = ((30.0 - iso_bb.min_y) * px_per_mm).round() as u32;
        assert_eq!(iso_buf.get_pixel(ix, iy).unwrap(), [255, 0, 255, 255]);
    }

    // 2. SymbolDef with ObjectId(0) Group(Envelope) -> [ObjectId(1) Red, ObjectId(2) Lens(frozen/remove_face)]
    for &(frozen, remove_face) in &[(true, false), (false, true)] {
        let mut doc = Document::new("ZeroIdDefGrpInSym", 300.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let red_child = make_rect(ObjectId(1), 50.0, 10.0, 30.0, 30.0, &red);
        let mut lens_child = make_rect(ObjectId(2), 50.0, 10.0, 30.0, 30.0, &Color::TRANSPARENT);
        lens_child.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen,
            viewpoint: None,
            remove_face,

            frozen_snapshot: None,
        });
        let mut def_grp = SceneObject::new(
            ObjectId(0),
            "ZeroEnvGrp",
            NodePayload::Group(vec![ObjectId(1), ObjectId(2)]),
        );
        def_grp.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(50.0, 10.0),
                Point2D::new(100.0, 10.0),
                Point2D::new(100.0, 40.0),
                Point2D::new(50.0, 40.0),
            ],
        });
        let sym = SymbolDef::new("ZeroEnvSym", vec![def_grp, red_child, lens_child]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
                Some(lid),
            )
            .unwrap();

        let direct = render_direct(&doc, &vp, &opts);
        let tiled = render_tiled(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);
        assert_eq!(sample_mm(&direct, 85.0, 25.0), [0, 255, 255, 255]);
        assert_eq!(sample_mm(&tiled, 85.0, 25.0), [0, 255, 255, 255]);

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, inst_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let px_per_mm: f64 = 96.0 / 25.4;
        let ix = ((85.0 - iso_bb.min_x) * px_per_mm).round() as u32;
        let iy = ((25.0 - iso_bb.min_y) * px_per_mm).round() as u32;
        assert_eq!(iso_buf.get_pixel(ix, iy).unwrap(), [0, 255, 255, 255]);
    }

    // 3. SymbolDef with undeformed ObjectId(0) Group(translate(50, 0)) -> [ObjectId(1) Red, ObjectId(2) Lens(frozen/remove_face)]
    for &(frozen, remove_face) in &[(true, false), (false, true)] {
        let mut doc = Document::new("ZeroIdTransGrpInSym", 300.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let red_child = make_rect(ObjectId(1), 10.0, 10.0, 35.0, 35.0, &red);
        let mut lens_child = make_rect(ObjectId(2), 10.0, 10.0, 35.0, 35.0, &Color::TRANSPARENT);
        lens_child.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen,
            viewpoint: None,
            remove_face,

            frozen_snapshot: None,
        });
        let trans_grp = SceneObject::new(
            ObjectId(0),
            "ZeroTransGrp",
            NodePayload::Group(vec![ObjectId(1), ObjectId(2)]),
        )
        .with_transform(Affine2D::translate(50.0, 0.0));
        let sym = SymbolDef::new("ZeroTransSym", vec![trans_grp, red_child, lens_child]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
                Some(lid),
            )
            .unwrap();

        let direct = render_direct(&doc, &vp, &opts);
        let tiled = render_tiled(&doc, &vp, &opts);
        assert_eq!(count_diff(&direct, &tiled), 0);
        assert_eq!(sample_mm(&direct, 75.0, 25.0), [0, 255, 255, 255]);
        assert_eq!(sample_mm(&tiled, 75.0, 25.0), [0, 255, 255, 255]);

        let (iso_bb, iso_buf) =
            render_isolated_object_to_rgba(&doc, 0, inst_id, Affine2D::IDENTITY, 96.0)
                .unwrap()
                .unwrap();
        let px_per_mm: f64 = 96.0 / 25.4;
        let ix = ((75.0 - iso_bb.min_x) * px_per_mm).round() as u32;
        let iy = ((25.0 - iso_bb.min_y) * px_per_mm).round() as u32;
        assert_eq!(iso_buf.get_pixel(ix, iy).unwrap(), [0, 255, 255, 255]);
    }
}

#[test]
fn w1b_r25_dirty_tile_cache_warms_off_tile_frozen_lenses_and_persists_on_move() {
    use coree_core::{CommandOp, LensConfig, LensKind, ParametricShape, SymbolDef};
    use coree_render::{DirtyTileCache, RenderOptions, SceneRenderer};

    let blue = Color::rgba8(0, 0, 255, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let make_rect = |id: ObjectId, x: f64, y: f64, w: f64, h: f64, fill: &Color| -> SceneObject {
        SceneObject::new(
            id,
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(fill.clone()))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(x, y))
    };

    let mut doc = Document::new("OffTileFrozenMove", 300.0, 200.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;
    doc.add_object(
        make_rect(ObjectId(0), 100.0, 10.0, 90.0, 50.0, &blue),
        Some(lid),
    )
    .unwrap();
    doc.add_object(
        make_rect(ObjectId(0), 100.0, 80.0, 90.0, 50.0, &green),
        Some(lid),
    )
    .unwrap();

    // Place both standalone Lens(frozen=true) and SymbolInstance(Lens(frozen=true)) at x > 100mm (outside Tile (0,0))
    let mut standalone = make_rect(ObjectId(0), 105.0, 15.0, 25.0, 25.0, &Color::TRANSPARENT);
    standalone.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: true,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let standalone_id = doc.add_object(standalone, Some(lid)).unwrap();

    let mut sym_lens = make_rect(ObjectId(0), 0.0, 0.0, 25.0, 25.0, &Color::TRANSPARENT);
    sym_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: true,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let sym = SymbolDef::new("OffTileMoveSym", vec![sym_lens]);
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let inst_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(145.0, 15.0)),
            Some(lid),
        )
        .unwrap();

    let vp = Viewport::for_export(768, 512, 96.0);
    let opts = RenderOptions::export();
    let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
    let mut renderer = SceneRenderer::new();

    let frame1 = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    let px_per_mm: f64 = 96.0 / 25.4;
    let p1_s = frame1
        .get_pixel(
            (117.5 * px_per_mm).round() as u32,
            (27.5 * px_per_mm).round() as u32,
        )
        .unwrap();
    let p1_i = frame1
        .get_pixel(
            (157.5 * px_per_mm).round() as u32,
            (27.5 * px_per_mm).round() as u32,
        )
        .unwrap();
    assert_eq!(p1_s, [255, 255, 0, 255]);
    assert_eq!(p1_i, [255, 255, 0, 255]);

    // Move both frozen lenses over the Green rect and re-render via DirtyTileCache
    coree_core::apply_command(
        &mut doc,
        &CommandOp::TransformObjects {
            ids: vec![standalone_id, inst_id],
            affine: Affine2D::translate(0.0, 70.0),
        },
    )
    .unwrap();
    cache.mark_object_dirty(&doc, standalone_id);
    cache.mark_object_dirty(&doc, inst_id);
    let frame2 = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    let p2_s = frame2
        .get_pixel(
            (117.5 * px_per_mm).round() as u32,
            (97.5 * px_per_mm).round() as u32,
        )
        .unwrap();
    let p2_i = frame2
        .get_pixel(
            (157.5 * px_per_mm).round() as u32,
            (97.5 * px_per_mm).round() as u32,
        )
        .unwrap();
    assert_eq!(
        p2_s,
        [255, 255, 0, 255],
        "Moved off-tile standalone frozen lens must preserve its Yellow snapshot over Green"
    );
    assert_eq!(
        p2_i,
        [255, 255, 0, 255],
        "Moved off-tile SymbolInstance frozen lens must preserve its Yellow snapshot over Green"
    );
}

#[test]
fn w1b_r26_deformed_outer_group_wrapping_symmetry_containers_and_off_vp_isolated_warm() {
    let px_per_mm: f64 = 96.0 / 25.4;
    let red = Color::rgba8(255, 0, 0, 255);
    let green = Color::rgba8(0, 255, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);
    let white = Color::WHITE;

    let make_rect = |id: ObjectId, x: f64, y: f64, w: f64, h: f64, fill: Fill| -> SceneObject {
        SceneObject::new(
            id,
            "Rect",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(x, y, w, h))),
        )
        .with_fill(fill)
        .with_outline(OutlineStyle::none())
    };

    let render_both = |doc: &Document, w: u32, h: u32| -> (RgbaBuffer, RgbaBuffer) {
        let vp = Viewport::for_export(w, h, 96.0);
        let opts = RenderOptions::export();
        let mut r1 = SceneRenderer::new();
        let mut b1 = TinySkiaBackend::new(w, h).unwrap();
        r1.render_document(doc, &vp, ViewMode::Enhanced, &opts, &mut b1);
        let direct = b1.into_rgba();

        let mut cache = DirtyTileCache::new(w, h);
        let mut r2 = SceneRenderer::new();
        let tiled = cache.render_viewport(doc, &vp, ViewMode::Enhanced, &opts, &mut r2);
        (direct, tiled)
    };

    let diff_count = |a: &RgbaBuffer, b: &RgbaBuffer| -> usize {
        a.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.rgba.as_chunks::<4>().0.iter())
            .filter(|(pa, pb)| pa != pb)
            .count()
    };

    // 1. BUG-A1: Group(Envelope) -> Group(Effect::Symmetry, opacity=0.99) -> [Red, Lens(Invert, frozen=true)]
    //    spanning x=256px (67.73mm) tile seam over Blue background.
    {
        let mut doc = Document::new_a4("R26 EnvThenSymGrp Frozen");
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(
                ObjectId(0),
                0.0,
                0.0,
                160.0,
                80.0,
                Fill::Solid(blue.clone()),
            ),
            Some(lid),
        )
        .unwrap();

        let r_id = doc
            .add_object(
                make_rect(
                    ObjectId(0),
                    40.0,
                    15.0,
                    15.0,
                    20.0,
                    Fill::Solid(red.clone()),
                ),
                Some(lid),
            )
            .unwrap();
        let mut lens = make_rect(ObjectId(0), 40.0, 15.0, 15.0, 20.0, Fill::None);
        lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::Group {
                ids: vec![r_id, l_id],
                name: "InnerSymGrp".into(),
            },
        )
        .unwrap();
        let sym_grp_id = doc.parent_container_of(r_id).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::SetOpacity {
                ids: vec![sym_grp_id],
                opacity: 0.99,
            },
        )
        .unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: sym_grp_id,
                effect: Effect::Symmetry {
                    axes: 2,
                    mirror: false,
                    origin: Point2D::new(67.73, 25.0),
                },
            },
        )
        .unwrap();
        // Anchor covering [40..95.46, 15..35] so Envelope identity cage matches full bounds
        let anchor_id = doc
            .add_object(
                make_rect(ObjectId(0), 40.0, 15.0, 55.46, 20.0, Fill::None),
                Some(lid),
            )
            .unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::Group {
                ids: vec![anchor_id, sym_grp_id],
                name: "OuterEnvGrp".into(),
            },
        )
        .unwrap();
        let env_grp_id = doc.parent_container_of(sym_grp_id).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: env_grp_id,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(40.0, 15.0),
                        Point2D::new(95.46, 15.0),
                        Point2D::new(95.46, 35.0),
                        Point2D::new(40.0, 35.0),
                    ],
                },
            },
        )
        .unwrap();

        let (direct, tiled) = render_both(&doc, 512, 256);
        assert_eq!(
            diff_count(&direct, &tiled),
            0,
            "BUG-A1: DirtyTileCache must match render_document with 0 diff pixels"
        );
        let b1_x = (87.96 * px_per_mm).round() as u32;
        let b1_y = (25.0 * px_per_mm).round() as u32;
        let px_d = direct.get_pixel(b1_x, b1_y).unwrap();
        let px_t = tiled.get_pixel(b1_x, b1_y).unwrap();
        assert!(
            px_d[0] <= 5 && px_d[1] >= 250 && px_d[2] >= 250,
            "BUG-A1 direct Branch 1 must be Cyan (~[0,255,255]), got {px_d:?}"
        );
        assert_eq!(px_d, px_t);

        let (iso_bb, iso_buf) = coree_render::render_isolated_object_to_rgba(
            &doc,
            0,
            env_grp_id,
            Affine2D::IDENTITY,
            96.0,
        )
        .unwrap()
        .unwrap();
        let iso_x = ((87.96 - iso_bb.min_x) * px_per_mm).round() as u32;
        let iso_y = ((25.0 - iso_bb.min_y) * px_per_mm).round() as u32;
        let px_iso = iso_buf.get_pixel(iso_x, iso_y).unwrap();
        assert!(
            px_iso[0] <= 5 && px_iso[1] >= 250 && px_iso[2] >= 250,
            "BUG-A1 isolated Branch 1 must be Cyan (~[0,255,255]), got {px_iso:?}"
        );
    }

    // 2. BUG-A2: Group(Envelope) -> PowerClip(fill=Green, Symmetry) -> [Lens(Invert, frozen=true)]
    //    crossing x=256px (67.73mm) over Blue background -> Magenta [255, 0, 255, 255] on BOTH Branch 0 and Branch 1.
    {
        let mut doc = Document::new_a4("R26 EnvThenSymPC Frozen");
        let lid = doc.pages()[0].layers[0].id;
        doc.add_object(
            make_rect(
                ObjectId(0),
                0.0,
                0.0,
                160.0,
                80.0,
                Fill::Solid(blue.clone()),
            ),
            Some(lid),
        )
        .unwrap();

        let mut lens = make_rect(ObjectId(0), 15.0, 15.0, 30.0, 20.0, Fill::None);
        lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        let pc_id = doc
            .add_object(
                make_rect(
                    ObjectId(0),
                    15.0,
                    15.0,
                    30.0,
                    20.0,
                    Fill::Solid(green.clone()),
                ),
                Some(lid),
            )
            .unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::CreatePowerClip {
                container_id: pc_id,
                content_ids: vec![l_id],
            },
        )
        .unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: pc_id,
                effect: Effect::Symmetry {
                    axes: 2,
                    mirror: false,
                    origin: Point2D::new(50.0, 25.0),
                },
            },
        )
        .unwrap();
        let anchor_id = doc
            .add_object(
                make_rect(ObjectId(0), 15.0, 15.0, 70.0, 20.0, Fill::None),
                Some(lid),
            )
            .unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::Group {
                ids: vec![anchor_id, pc_id],
                name: "OuterEnvGrp".into(),
            },
        )
        .unwrap();
        let env_grp_id = doc.parent_container_of(pc_id).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: env_grp_id,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(15.0, 15.0),
                        Point2D::new(85.0, 15.0),
                        Point2D::new(85.0, 35.0),
                        Point2D::new(15.0, 35.0),
                    ],
                },
            },
        )
        .unwrap();

        let (direct, tiled) = render_both(&doc, 512, 256);
        assert_eq!(
            diff_count(&direct, &tiled),
            0,
            "BUG-A2: DirtyTileCache must match render_document with 0 diff pixels"
        );
        let b0_x = (30.0 * px_per_mm).round() as u32;
        let b0_y = (25.0 * px_per_mm).round() as u32;
        let b1_x = (70.0 * px_per_mm).round() as u32;
        let b1_y = (25.0 * px_per_mm).round() as u32;
        assert_eq!(
            direct.get_pixel(b0_x, b0_y).unwrap(),
            [255, 0, 255, 255],
            "BUG-A2 direct Branch 0 must be Magenta"
        );
        assert_eq!(
            tiled.get_pixel(b0_x, b0_y).unwrap(),
            [255, 0, 255, 255],
            "BUG-A2 tiled Branch 0 must be Magenta"
        );
        assert_eq!(
            direct.get_pixel(b1_x, b1_y).unwrap(),
            [255, 0, 255, 255],
            "BUG-A2 direct Branch 1 must be Magenta"
        );
        assert_eq!(
            tiled.get_pixel(b1_x, b1_y).unwrap(),
            [255, 0, 255, 255],
            "BUG-A2 tiled Branch 1 must be Magenta"
        );

        let (iso_bb, iso_buf) = coree_render::render_isolated_object_to_rgba(
            &doc,
            0,
            env_grp_id,
            Affine2D::IDENTITY,
            96.0,
        )
        .unwrap()
        .unwrap();
        let iso_b0_x = ((30.0 - iso_bb.min_x) * px_per_mm).round() as u32;
        let iso_b0_y = ((25.0 - iso_bb.min_y) * px_per_mm).round() as u32;
        let iso_b1_x = ((70.0 - iso_bb.min_x) * px_per_mm).round() as u32;
        let iso_b1_y = ((25.0 - iso_bb.min_y) * px_per_mm).round() as u32;
        assert_eq!(
            iso_buf.get_pixel(iso_b0_x, iso_b0_y).unwrap(),
            [255, 0, 255, 255],
            "BUG-A2 isolated Branch 0 must be Magenta"
        );
        assert_eq!(
            iso_buf.get_pixel(iso_b1_x, iso_b1_y).unwrap(),
            [255, 0, 255, 255],
            "BUG-A2 isolated Branch 1 must be Magenta"
        );
    }

    // 3. BUG-A3 + 1A-9: Group(Envelope) & SymbolDef(ObjectId(0) Group(Envelope)) wrapping
    //    Group(Symmetry) -> [Red, Lens(Invert, remove_face=true)] + Wireframe lens.
    {
        let mut doc = Document::new_a4("R26 SymDef ZeroEnv Sym RmFace");
        let lid = doc.pages()[0].layers[0].id;
        let sym_red = make_rect(
            ObjectId(1),
            15.0,
            15.0,
            15.0,
            20.0,
            Fill::Solid(red.clone()),
        );
        let mut sym_lens = make_rect(ObjectId(2), 15.0, 15.0, 15.0, 20.0, Fill::None);
        sym_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: false,
            viewpoint: None,
            remove_face: true,

            frozen_snapshot: None,
        });
        let mut sym_inner = SceneObject::new(
            ObjectId(3),
            "InnerSym",
            NodePayload::Group(vec![ObjectId(1), ObjectId(2)]),
        );
        sym_inner.effects.push(Effect::Symmetry {
            axes: 2,
            mirror: false,
            origin: Point2D::new(50.0, 25.0),
        });
        let sym_anchor = make_rect(ObjectId(4), 15.0, 15.0, 70.0, 20.0, Fill::None);
        let mut sym_zero_env = SceneObject::new(
            ObjectId(0),
            "ZeroEnv",
            NodePayload::Group(vec![ObjectId(4), ObjectId(3)]),
        );
        sym_zero_env.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(15.0, 15.0),
                Point2D::new(85.0, 15.0),
                Point2D::new(85.0, 35.0),
                Point2D::new(15.0, 35.0),
            ],
        });
        let sym = SymbolDef::new(
            "ZeroEnvSym",
            vec![sym_red, sym_lens, sym_inner, sym_anchor, sym_zero_env],
        );
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
                Some(lid),
            )
            .unwrap();

        let (direct, tiled) = render_both(&doc, 384, 256);
        assert_eq!(diff_count(&direct, &tiled), 0);
        let b1_x = (77.5 * px_per_mm).round() as u32;
        let b1_y = (25.0 * px_per_mm).round() as u32;
        assert_eq!(
            direct.get_pixel(b1_x, b1_y).unwrap(),
            [0, 255, 255, 255],
            "1A-9: Branch 1 remove_face inside SymbolDef(ZeroEnv->Sym) must be Cyan"
        );
        assert_eq!(tiled.get_pixel(b1_x, b1_y).unwrap(), [0, 255, 255, 255]);

        let (iso_bb, iso_buf) = coree_render::render_isolated_object_to_rgba(
            &doc,
            0,
            inst_id,
            Affine2D::IDENTITY,
            96.0,
        )
        .unwrap()
        .unwrap();
        let iso_x = ((77.5 - iso_bb.min_x) * px_per_mm).round() as u32;
        let iso_y = ((25.0 - iso_bb.min_y) * px_per_mm).round() as u32;
        assert_eq!(iso_buf.get_pixel(iso_x, iso_y).unwrap(), [0, 255, 255, 255]);

        // Also verify Wireframe lens inside Group(Envelope) -> Group(Symmetry) draws Branch 1 edges
        let mut wf_doc = Document::new_a4("R26 EnvSym WireframeLens");
        let wlid = wf_doc.pages()[0].layers[0].id;
        let wr = wf_doc
            .add_object(
                make_rect(
                    ObjectId(0),
                    15.0,
                    15.0,
                    15.0,
                    20.0,
                    Fill::Solid(red.clone()),
                ),
                Some(wlid),
            )
            .unwrap();
        let mut wl = make_rect(ObjectId(0), 10.0, 10.0, 25.0, 30.0, Fill::None);
        wl.lens = Some(LensConfig {
            kind: LensKind::Wireframe {
                override_outline: Some(blue.clone()),
                override_fill: Some(white),
            },
            frozen: false,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let wl_id = wf_doc.add_object(wl, Some(wlid)).unwrap();
        coree_core::apply_command(
            &mut wf_doc,
            &coree_core::CommandOp::Group {
                ids: vec![wr, wl_id],
                name: "WSym".into(),
            },
        )
        .unwrap();
        let w_sym_id = wf_doc.parent_container_of(wr).unwrap();
        coree_core::apply_command(
            &mut wf_doc,
            &coree_core::CommandOp::AddEffect {
                id: w_sym_id,
                effect: Effect::Symmetry {
                    axes: 2,
                    mirror: false,
                    origin: Point2D::new(50.0, 25.0),
                },
            },
        )
        .unwrap();
        let w_anc = wf_doc
            .add_object(
                make_rect(ObjectId(0), 10.0, 10.0, 80.0, 30.0, Fill::None),
                Some(wlid),
            )
            .unwrap();
        coree_core::apply_command(
            &mut wf_doc,
            &coree_core::CommandOp::Group {
                ids: vec![w_anc, w_sym_id],
                name: "WEnv".into(),
            },
        )
        .unwrap();
        let w_env_id = wf_doc.parent_container_of(w_sym_id).unwrap();
        coree_core::apply_command(
            &mut wf_doc,
            &coree_core::CommandOp::AddEffect {
                id: w_env_id,
                effect: Effect::Envelope {
                    mode: EnvelopeMode::Straight,
                    cage: vec![
                        Point2D::new(10.0, 10.0),
                        Point2D::new(90.0, 10.0),
                        Point2D::new(90.0, 40.0),
                        Point2D::new(10.0, 40.0),
                    ],
                },
            },
        )
        .unwrap();
        let (wf_d, wf_t) = render_both(&wf_doc, 384, 256);
        assert_eq!(diff_count(&wf_d, &wf_t), 0);
        // Left branch inner rect edge x=15mm vs Right branch inner rect edge x=70mm should both have blue strokes
        let has_blue_near = |buf: &RgbaBuffer, x_mm: f64| -> bool {
            let cx = (x_mm * px_per_mm).round() as i32;
            let cy = (25.0 * px_per_mm).round() as i32;
            (-3..=3).any(|dx| {
                let px = buf.get_pixel((cx + dx) as u32, cy as u32).unwrap();
                px[2] > px[0] && px[2] >= 200
            })
        };
        assert!(has_blue_near(&wf_d, 15.0));
        assert!(
            has_blue_near(&wf_d, 70.0),
            "Branch 1 Wireframe lens inside Envelope->Symmetry must draw blue wireframe stroke on reflected child"
        );
    }

    // 4. Minor (T5-AdvCaseC-OffVpIsoGrpFrozen): Off-viewport Group(opacity=0.5) containing
    //    Lens(Invert, frozen=true) warms its snapshot on frame 1 in DirtyTileCache and retains it when moved.
    {
        let mut doc = Document::new_a4("R26 OffVp IsoGrp Frozen");
        let lid = doc.pages()[0].layers[0].id;
        // Green backdrop inside viewport (x=0..80mm)
        doc.add_object(
            make_rect(ObjectId(0), 0.0, 0.0, 80.0, 60.0, Fill::Solid(green)),
            Some(lid),
        )
        .unwrap();
        // Red backdrop completely off-viewport (x=150..180mm; viewport is 384px = 101.6mm wide)
        doc.add_object(
            make_rect(ObjectId(0), 150.0, 10.0, 30.0, 30.0, Fill::Solid(red)),
            Some(lid),
        )
        .unwrap();
        let mut lens = make_rect(ObjectId(0), 150.0, 10.0, 30.0, 30.0, Fill::None);
        lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let l_id = doc.add_object(lens, Some(lid)).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::Group {
                ids: vec![l_id],
                name: "OffVpIsoGrp".into(),
            },
        )
        .unwrap();
        let grp_id = doc.parent_container_of(l_id).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::SetOpacity {
                ids: vec![grp_id],
                opacity: 0.5,
            },
        )
        .unwrap();

        let vp = Viewport::for_export(384, 256, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();

        let _frame1 = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert!(
            renderer.get_frozen_lens(l_id).is_some(),
            "Off-viewport isolated Group(opacity=0.5) must warm descendant frozen lens in Frame 1"
        );

        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::TransformObjects {
                ids: vec![grp_id],
                affine: Affine2D::translate(-130.0, 0.0),
            },
        )
        .unwrap();
        cache.mark_object_dirty(&doc, grp_id);
        let frame2 = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        let px = frame2
            .get_pixel(
                (35.0 * px_per_mm).round() as u32,
                (25.0 * px_per_mm).round() as u32,
            )
            .unwrap();
        // Frozen Red->Cyan [0,255,255] at opacity=0.5 over Green [0,255,0] => ~[0,255,128,255] (NOT gray [128,128,128])
        assert!(
            px[0] <= 5 && px[1] >= 250 && px[2] >= 120 && px[2] <= 136,
            "Moved off-viewport isolated Group frozen lens must retain Cyan snapshot over Green (~[0,255,128]), got {px:?}"
        );
    }
}

#[test]
fn w1b_r27_zero_opacity_frozen_lens_does_not_stay_unwarmed() {
    let mut doc = Document::new_a4("R27 Zero Opacity Frozen Lens");
    let lid = doc.pages()[0].layers[0].id;

    // Backdrop
    doc.add_object(
        rect_obj(
            0.0,
            0.0,
            80.0,
            60.0,
            Fill::Solid(Color::rgba8(0, 255, 0, 255)),
        ),
        Some(lid),
    )
    .unwrap();

    // 1. Standalone Lens(frozen=true) with opacity = 0.0
    let mut zero_lens = rect_obj(10.0, 10.0, 20.0, 20.0, Fill::None);
    zero_lens.opacity = 0.0;
    zero_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: true,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let zero_lens_id = doc.add_object(zero_lens, Some(lid)).unwrap();

    // 2. Group(opacity = 0.0) containing an otherwise-visible Lens(frozen=true)
    let mut child_lens = rect_obj(40.0, 10.0, 20.0, 20.0, Fill::None);
    child_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: true,
        viewpoint: None,
        remove_face: false,

        frozen_snapshot: None,
    });
    let child_lens_id = doc.add_object(child_lens, Some(lid)).unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::Group {
            ids: vec![child_lens_id],
            name: "ZeroOpacityGrp".into(),
        },
    )
    .unwrap();
    let zero_grp_id = doc.parent_container_of(child_lens_id).unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetOpacity {
            ids: vec![zero_grp_id],
            opacity: 0.0,
        },
    )
    .unwrap();

    let vp = Viewport::for_export(384, 256, 96.0);
    let opts = RenderOptions::export();
    let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
    let mut renderer = SceneRenderer::new();

    assert!(
        !renderer.has_unwarmed_frozen_lens(&doc, false),
        "opacity == 0.0 Group/Lens(frozen=true) must be skipped by has_unwarmed_frozen_lens before render"
    );
    let _frame = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    assert!(
        !renderer.has_unwarmed_frozen_lens(&doc, false),
        "opacity == 0.0 Group/Lens(frozen=true) must not leave has_unwarmed_frozen_lens == true after rendering"
    );
    assert!(renderer.get_frozen_lens(zero_lens_id).is_none());
    assert!(renderer.get_frozen_lens(child_lens_id).is_none());
}

#[test]
fn w1b_r28_powerclip_with_lens_ignores_child_frozen_lens_warm_check() {
    for pc_frozen in [false, true] {
        let mut doc = Document::new(
            "R28 PowerClip Lens WarmCheck",
            100.0,
            100.0,
            ColorMode::Rgb,
            96.0,
        );
        let lid = doc.pages()[0].layers[0].id;

        doc.add_object(
            rect_obj(
                0.0,
                0.0,
                80.0,
                80.0,
                Fill::Solid(Color::rgba8(240, 20, 20, 255)),
            ),
            Some(lid),
        )
        .unwrap();

        let mut child_lens = rect_obj(15.0, 15.0, 20.0, 20.0, Fill::None);
        child_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let child_id = doc.add_object(child_lens, Some(lid)).unwrap();

        let mut pc = rect_obj(10.0, 10.0, 40.0, 40.0, Fill::None);
        pc.outline = OutlineStyle::none();
        pc.lens = Some(LensConfig {
            kind: LensKind::Brighten { rate_pct: 20.0 },
            frozen: pc_frozen,
            viewpoint: None,
            remove_face: false,

            frozen_snapshot: None,
        });
        let pc_id = doc.add_object(pc, Some(lid)).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::CreatePowerClip {
                container_id: pc_id,
                content_ids: vec![child_id],
            },
        )
        .unwrap();

        let vp = Viewport::for_export(384, 384, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();

        assert_eq!(renderer.has_unwarmed_frozen_lens(&doc, false), pc_frozen);
        let _frame = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert!(
            !renderer.has_unwarmed_frozen_lens(&doc, false),
            "PowerClip with own lens(frozen={pc_frozen}) must not report child frozen lens as unwarmed"
        );
    }
}

#[test]
fn w1b_r28_remove_face_ignores_invisible_and_zero_opacity_artwork() {
    let mut doc = Document::new(
        "R28 RemoveFace Invisible",
        100.0,
        100.0,
        ColorMode::Rgb,
        72.0,
    );
    let lid = doc.pages()[0].layers[0].id;

    // 1. Invisible Fill::None + OutlineStyle::none() rect under Lens(Invert, remove_face=true)
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "InvisibleRect",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                5.0, 5.0, 20.0, 20.0,
            ))),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none()),
        Some(lid),
    )
    .unwrap();

    // 2. opacity == 0.0 red rect under Lens(Invert, remove_face=true)
    let mut zero_op = rect_obj(
        30.0,
        5.0,
        20.0,
        20.0,
        Fill::Solid(Color::rgba8(255, 0, 0, 255)),
    );
    zero_op.opacity = 0.0;
    doc.add_object(zero_op, Some(lid)).unwrap();

    // 3. Empty invisible PowerClip & empty invisible VectorTable
    let empty_pc = SceneObject::new(
        ObjectId(0),
        "EmptyInvisiblePC",
        NodePayload::PowerClip {
            container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                55.0, 5.0, 20.0, 20.0,
            ))),
            container_parametric: None,
            contents: Vec::new(),
            lock_contents: false,
        },
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    doc.add_object(empty_pc, Some(lid)).unwrap();

    let mut empty_tbl = coree_core::VectorTable::new(1, 1, 20.0, 20.0);
    empty_tbl.border_style = OutlineStyle::none();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "EmptyInvisibleTable",
            NodePayload::Table(empty_tbl),
        )
        .with_transform(Affine2D::translate(75.0, 5.0))
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none()),
        Some(lid),
    )
    .unwrap();

    let mut lens = rect_obj(0.0, 0.0, 98.0, 30.0, Fill::None);
    lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: true,

        frozen_snapshot: None,
    });
    doc.add_object(lens, Some(lid)).unwrap();

    // 4. Deformed Group(Distort) -> [Invisible anchor, Group(Symmetry) -> [Partial Red, Lens(Invert, remove_face=true)]]
    let anchor = SceneObject::new(
        ObjectId(0),
        "Anchor",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 40.0, 80.0, 40.0,
        ))),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    let anchor_id = doc.add_object(anchor, Some(lid)).unwrap();

    let red = rect_obj(
        15.0,
        45.0,
        15.0,
        30.0,
        Fill::Solid(Color::rgba8(255, 0, 0, 255)),
    );
    let red_id = doc.add_object(red, Some(lid)).unwrap();
    let mut sym_lens = rect_obj(10.0, 40.0, 35.0, 40.0, Fill::None);
    sym_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: true,

        frozen_snapshot: None,
    });
    let sym_lens_id = doc.add_object(sym_lens, Some(lid)).unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::Group {
            ids: vec![red_id, sym_lens_id],
            name: "SymGrp".into(),
        },
    )
    .unwrap();
    let sym_grp_id = doc.parent_container_of(red_id).unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::AddEffect {
            id: sym_grp_id,
            effect: Effect::Symmetry {
                axes: 2,
                mirror: false,
                origin: Point2D::new(50.0, 60.0),
            },
        },
    )
    .unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::Group {
            ids: vec![anchor_id, sym_grp_id],
            name: "DistortGrp".into(),
        },
    )
    .unwrap();
    let dist_grp_id = doc.parent_container_of(sym_grp_id).unwrap();
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::AddEffect {
            id: dist_grp_id,
            effect: Effect::Distort {
                kind: coree_core::DistortKind::PushPull,
                amplitude: 0.0,
                frequency: 1.0,
                center: Point2D::new(50.0, 60.0),
            },
        },
    )
    .unwrap();

    let img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    for &(x_mm, y_mm, label) in &[
        (15.0, 15.0, "Fill::None+Outline::none"),
        (40.0, 15.0, "opacity==0.0"),
        (65.0, 15.0, "empty invisible PowerClip"),
        (85.0, 15.0, "empty invisible Table"),
        (37.0, 60.0, "Branch0 uncovered over invisible anchor"),
        (63.0, 60.0, "Branch1 uncovered over invisible anchor"),
    ] {
        let px = pixel_at_mm(&img, x_mm, y_mm);
        assert_eq!(
            px,
            [255, 255, 255, 255],
            "{label} at ({x_mm}, {y_mm}) must leave blank page untouched, got {px:?}"
        );
    }
    assert_eq!(pixel_at_mm(&img, 22.0, 60.0), [0, 255, 255, 255]);
    assert_eq!(pixel_at_mm(&img, 77.0, 60.0), [0, 255, 255, 255]);
}

#[test]
fn w2_exit_r2_group_and_symbol_instance_effects_and_perspective_render() {
    use coree_core::{BevelStyle, ExtrudeLight, FeatherDirection, Perspective3x3, SymbolDef};

    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(20.0, 20.0, 40.0, 40.0),
        &[
            Point2D::new(26.0, 22.0),
            Point2D::new(56.0, 26.0),
            Point2D::new(62.0, 58.0),
            Point2D::new(16.0, 56.0),
        ],
    )
    .expect("valid perspective quad");

    let effects_to_test: Vec<(&str, Option<Effect>, Option<Perspective3x3>)> = vec![
        (
            "InnerShadow",
            Some(Effect::InnerShadow {
                dx_mm: 3.0,
                dy_mm: 3.0,
                blur_mm: 2.0,
                opacity: 0.85,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Inside,
            }),
            None,
        ),
        (
            "Bevel",
            Some(Effect::Bevel {
                style: BevelStyle::SoftEdge,
                distance_mm: 3.0,
                softness_mm: 0.8,
                light_angle_deg: 135.0,
                altitude_deg: 45.0,
                highlight_color: Color::WHITE,
                shadow_color: Color::BLACK,
                intensity: 0.9,
            }),
            None,
        ),
        (
            "Extrude3D",
            Some(Effect::Extrude3D {
                vanishing_point: Point2D::new(90.0, 10.0),
                depth: 14.0,
                bevel_depth: 1.0,
                shade_color: Color::rgba8(20, 30, 60, 255),
                rotation_3d: [18.0, -22.0, 0.0],
                lights: vec![ExtrudeLight {
                    dir: [0.4, -0.6, 0.7],
                    intensity: 1.0,
                }],
            }),
            None,
        ),
        (
            "DropShadow",
            Some(Effect::DropShadow {
                dx_mm: 5.0,
                dy_mm: 5.0,
                blur_mm: 2.0,
                opacity: 0.75,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Middle,
            }),
            None,
        ),
        ("Perspective3x3", None, Some(persp)),
    ];

    for is_symbol in [false, true] {
        let make_base = |eff: Option<Effect>, p: Option<Perspective3x3>| {
            let mut doc = Document::new("ContainerFx", 120.0, 120.0, ColorMode::Rgb, 96.0);
            let lid = doc.pages()[0].layers[0].id;
            let child_fill = Fill::Solid(Color::rgba8(40, 130, 220, 255));
            let target_id = if is_symbol {
                let sym =
                    SymbolDef::new("Badge", vec![rect_obj(20.0, 20.0, 40.0, 40.0, child_fill)]);
                let sid = sym.id;
                doc.insert_symbol(sym);
                let inst = SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sid));
                doc.add_object(inst, Some(lid)).unwrap()
            } else {
                let c = doc
                    .add_object(rect_obj(20.0, 20.0, 40.0, 40.0, child_fill), Some(lid))
                    .unwrap();
                doc.group_objects(&[c], "Grp").unwrap()
            };
            if let Some(e) = eff {
                coree_core::apply_command(
                    &mut doc,
                    &coree_core::CommandOp::AddEffect {
                        id: target_id,
                        effect: e,
                    },
                )
                .unwrap();
            }
            if p.is_some() {
                coree_core::apply_command(
                    &mut doc,
                    &coree_core::CommandOp::SetObjectPerspective {
                        ids: vec![target_id],
                        perspective: p,
                    },
                )
                .unwrap();
            }
            (doc, target_id)
        };

        let (plain_doc, plain_id) = make_base(None, None);
        let plain_page = render_page_to_rgba(&plain_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
        let (_, plain_iso) = coree_render::render_isolated_object_to_rgba(
            &plain_doc,
            0,
            plain_id,
            Affine2D::IDENTITY,
            96.0,
        )
        .unwrap()
        .expect("plain isolated");

        for (label, eff, p) in &effects_to_test {
            let (fx_doc, fx_id) = make_base(eff.clone(), *p);
            let fx_page = render_page_to_rgba(&fx_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
            let page_diff = plain_page
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .zip(fx_page.rgba.as_chunks::<4>().0.iter())
                .filter(|(a, b)| a != b)
                .count();
            assert!(
                page_diff > 50,
                "{} with {label} must visually differ from plain in render_page_to_rgba (diff={page_diff})",
                if is_symbol { "SymbolInstance" } else { "Group" }
            );

            let (_, fx_iso) = coree_render::render_isolated_object_to_rgba(
                &fx_doc,
                0,
                fx_id,
                Affine2D::IDENTITY,
                96.0,
            )
            .unwrap()
            .expect("fx isolated");
            assert_ne!(
                fx_iso.rgba,
                plain_iso.rgba,
                "{} with {label} must differ in render_isolated_object_to_rgba",
                if is_symbol { "SymbolInstance" } else { "Group" }
            );
        }
    }
}

#[test]
fn w2_exit_r4_powerclip_perspective_and_effects_deform_contents() {
    use coree_core::Perspective3x3;
    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 24.0, 24.0),
        &[
            Point2D::new(8.0, 0.0),
            Point2D::new(16.0, 0.0),
            Point2D::new(24.0, 24.0),
            Point2D::new(0.0, 24.0),
        ],
    )
    .expect("valid trapezoid homography");

    let red_bbox_px = |rgba: &[u8], w: u32, h: u32| -> (usize, Option<(u32, u32, u32, u32)>) {
        let mut count = 0usize;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let r = rgba[idx];
                let g = rgba[idx + 1];
                let b = rgba[idx + 2];
                let a = rgba[idx + 3];
                if a > 128 && r > 180 && g < 80 && b < 80 {
                    count += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        let bb = (count > 0).then_some((min_x, min_y, max_x, max_y));
        (count, bb)
    };

    for with_shadow in [false, true] {
        let mut doc = Document::new("PCPerspRender", 160.0, 160.0, ColorMode::Rgb, 150.0);
        let lid = doc.pages()[0].layers[0].id;
        let inner = SceneObject::new(
            ObjectId(0),
            "RedInner",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                52.0, 42.0, 20.0, 8.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(220, 20, 20, 255)));
        let inner_id = doc.add_object(inner, Some(lid)).unwrap();
        let container_obj = SceneObject::new(
            ObjectId(0),
            "TrapezoidPC",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 24.0, 24.0,
            ))),
        )
        .with_transform(Affine2D::translate(50.0, 40.0))
        .with_fill(Fill::Solid(Color::rgba8(245, 220, 40, 255)));
        let pc_id = doc.add_object(container_obj, Some(lid)).unwrap();
        doc.create_powerclip(pc_id, &[inner_id]).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::SetObjectPerspective {
                ids: vec![pc_id],
                perspective: Some(persp),
            },
        )
        .unwrap();
        if with_shadow {
            coree_core::apply_command(
                &mut doc,
                &coree_core::CommandOp::AddEffect {
                    id: pc_id,
                    effect: Effect::DropShadow {
                        dx_mm: 3.0,
                        dy_mm: 3.0,
                        blur_mm: 2.0,
                        opacity: 0.6,
                        color: Color::BLACK,
                        feather_direction: FeatherDirection::Outside,
                    },
                },
            )
            .unwrap();
        }

        let page_img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
        let (page_red_count, page_red_bb) =
            red_bbox_px(&page_img.rgba, page_img.width, page_img.height);
        let (min_x, min_y, max_x, max_y) = page_red_bb.expect("red warped inner slice");
        let px_per_mm = 150.0 / 25.4;
        let min_x_mm = f64::from(min_x) / px_per_mm;
        let min_y_mm = f64::from(min_y) / px_per_mm;
        let max_x_mm = f64::from(max_x + 1) / px_per_mm;
        let max_y_mm = f64::from(max_y + 1) / px_per_mm;

        // Warped red rect corners: (58.47, 40.71)..(65.53, 40.71) at top, (57.38, 44.62)..(66.62, 44.62) at bottom.
        // Unwarped would span x=55..69, y=42..50 (~3015 px).
        assert!(
            page_red_count > 600 && page_red_count < 1400,
            "with_shadow={with_shadow}: expected warped trapezoid slice (~960 px), got {page_red_count}"
        );
        assert!(
            min_x_mm >= 56.5 && max_x_mm <= 67.5 && min_y_mm >= 40.0 && max_y_mm <= 45.5,
            "with_shadow={with_shadow}: red bbox ({min_x_mm:.2}, {min_y_mm:.2})..({max_x_mm:.2}, {max_y_mm:.2}) mm did not warp!"
        );

        let (iso_rect, iso_buf) =
            coree_render::render_isolated_object_to_rgba(&doc, 0, pc_id, Affine2D::IDENTITY, 150.0)
                .unwrap()
                .expect("isolated PowerClip");
        let (iso_red_count, iso_red_bb) = red_bbox_px(&iso_buf.rgba, iso_buf.width, iso_buf.height);
        let (ix0, iy0, ix1, iy1) = iso_red_bb.expect("isolated red warped slice");
        let iso_min_x_mm = iso_rect.min_x + f64::from(ix0) / px_per_mm;
        let iso_min_y_mm = iso_rect.min_y + f64::from(iy0) / px_per_mm;
        let iso_max_x_mm = iso_rect.min_x + f64::from(ix1 + 1) / px_per_mm;
        let iso_max_y_mm = iso_rect.min_y + f64::from(iy1 + 1) / px_per_mm;
        assert!(
            (iso_red_count as isize - page_red_count as isize).abs() <= 80,
            "with_shadow={with_shadow}: isolated red px {iso_red_count} vs page {page_red_count}"
        );
        assert!(
            (iso_min_x_mm - min_x_mm).abs() < 0.5
                && (iso_max_x_mm - max_x_mm).abs() < 0.5
                && (iso_min_y_mm - min_y_mm).abs() < 0.5
                && (iso_max_y_mm - max_y_mm).abs() < 0.5,
            "with_shadow={with_shadow}: isolated bbox ({iso_min_x_mm:.2},{iso_min_y_mm:.2})..({iso_max_x_mm:.2},{iso_max_y_mm:.2}) != page ({min_x_mm:.2},{min_y_mm:.2})..({max_x_mm:.2},{max_y_mm:.2})"
        );
    }

    // Nested PowerClip(Perspective3x3) inside translated Group(Perspective3x3)
    let mut nested_doc = Document::new("NestedPCPersp", 160.0, 160.0, ColorMode::Rgb, 150.0);
    let lid = nested_doc.pages()[0].layers[0].id;
    let inner = SceneObject::new(
        ObjectId(0),
        "RedInner",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            2.0, 2.0, 20.0, 8.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(220, 20, 20, 255)));
    let inner_id = nested_doc.add_object(inner, Some(lid)).unwrap();
    let pc = SceneObject::new(
        ObjectId(0),
        "NestedPC",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 24.0, 24.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(245, 220, 40, 255)));
    let pc_id = nested_doc.add_object(pc, Some(lid)).unwrap();
    nested_doc.create_powerclip(pc_id, &[inner_id]).unwrap();
    coree_core::apply_command(
        &mut nested_doc,
        &coree_core::CommandOp::SetObjectPerspective {
            ids: vec![pc_id],
            perspective: Some(persp),
        },
    )
    .unwrap();
    let gid = nested_doc.group_objects(&[pc_id], "OuterGrp").unwrap();
    coree_core::apply_command(
        &mut nested_doc,
        &coree_core::CommandOp::TransformObjects {
            ids: vec![gid],
            affine: Affine2D::translate(50.0, 40.0),
        },
    )
    .unwrap();
    let grp_persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 24.0, 24.0),
        &[
            Point2D::new(2.0, 2.0),
            Point2D::new(22.0, 2.0),
            Point2D::new(24.0, 24.0),
            Point2D::new(0.0, 24.0),
        ],
    )
    .unwrap();
    coree_core::apply_command(
        &mut nested_doc,
        &coree_core::CommandOp::SetObjectPerspective {
            ids: vec![gid],
            perspective: Some(grp_persp),
        },
    )
    .unwrap();

    let nested_img = render_page_to_rgba(&nested_doc, 0, 150.0, ViewMode::Enhanced).unwrap();
    let (nested_red, nested_bb) =
        red_bbox_px(&nested_img.rgba, nested_img.width, nested_img.height);
    let (_, _, _, max_y) = nested_bb.expect("nested warped red slice");
    let max_y_mm = f64::from(max_y + 1) / (150.0 / 25.4);
    assert!(
        nested_red > 400 && nested_red < 1200 && max_y_mm < 47.0,
        "nested Group(Persp)->PowerClip(Persp) must warp contents cleanly (red={nested_red}, max_y_mm={max_y_mm:.2})"
    );
}

#[test]
fn w2_exit_r5_symbol_text_on_path_bspline_and_own_persp_text_render() {
    use coree_core::{ArtisticText, BSplineCurve, Perspective3x3, TextOnPath, TextStyle};

    let mut doc = Document::new("W2ExitR5Render", 160.0, 160.0, ColorMode::Rgb, 150.0);
    let bs_host = SceneObject::new(
        ObjectId(10),
        "SymBSpline",
        NodePayload::BSpline(BSplineCurve::from_control_points(
            vec![
                Point2D::new(5.0, 20.0),
                Point2D::new(20.0, 20.0),
                Point2D::new(35.0, 20.0),
                Point2D::new(50.0, 20.0),
            ],
            3,
            false,
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    let sym_top = SceneObject::new(
        ObjectId(11),
        "SymTOP",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain(
                "BSPL",
                TextStyle {
                    fill: Color::rgba8(220, 20, 20, 255),
                    font_size_pt: 14.0,
                    ..Default::default()
                },
            ),
            path_id: ObjectId(10),
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    let sym = SymbolDef::new("BSplineTOPSym", vec![bs_host, sym_top]);
    let sid = sym.id;
    doc.insert_symbol(sym);
    doc.add_object(
        SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(50.0, 60.0)),
        None,
    )
    .unwrap();

    // Also add standalone ArtisticText with own Perspective3x3 shifted right into x=30..70, y=10..25
    let mut art = SceneObject::new(
        ObjectId(0),
        "OwnPerspArt",
        NodePayload::ArtisticText(ArtisticText::from_plain(
            "ART",
            TextStyle {
                fill: Color::rgba8(20, 180, 40, 255),
                font_size_pt: 16.0,
                ..Default::default()
            },
        )),
    )
    .with_transform(Affine2D::translate(10.0, 10.0));
    art.perspective = Some(
        Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 20.0, 10.0),
            &[
                Point2D::new(20.0, 5.0),
                Point2D::new(50.0, 5.0),
                Point2D::new(50.0, 18.0),
                Point2D::new(20.0, 18.0),
            ],
        )
        .unwrap(),
    );
    doc.add_object(art, None).unwrap();

    let img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
    let px_per_mm = 150.0 / 25.4;
    let mut red_min_y = f64::INFINITY;
    let mut green_min_x = f64::INFINITY;
    for y in 0..img.height {
        for x in 0..img.width {
            let idx = ((y * img.width + x) * 4) as usize;
            let [r, g, b, a] = [
                img.rgba[idx],
                img.rgba[idx + 1],
                img.rgba[idx + 2],
                img.rgba[idx + 3],
            ];
            if a > 128 && r > 160 && g < 80 && b < 80 {
                red_min_y = red_min_y.min(f64::from(y) / px_per_mm);
            }
            if a > 128 && g > 140 && r < 80 && b < 80 {
                green_min_x = green_min_x.min(f64::from(x) / px_per_mm);
            }
        }
    }
    assert!(
        red_min_y.is_finite() && red_min_y > 70.0,
        "BSpline TextOnPath inside SymbolDef must render along y~80mm, got red_min_y={red_min_y:.2}"
    );
    assert!(
        green_min_x.is_finite() && green_min_x >= 28.0,
        "Own-perspective ArtisticText must warp horizontally to x>=30mm, got green_min_x={green_min_x:.2}"
    );
}

#[test]
fn w2_exit_r8_symbol_zero_id_persp_group_text_on_path_pixel_parity() {
    use coree_core::{ArtisticText, Perspective3x3, TextOnPath, TextStyle};

    let mut doc = Document::new("W2ExitR8_P5_Render", 240.0, 240.0, ColorMode::Rgb, 150.0);
    let host = SceneObject::new(
        ObjectId(20),
        "Host20",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, 5.0),
            Point2D::new(30.0, 5.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    let mut top = SceneObject::new(
        ObjectId(21),
        "Top21",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain(
                "ZERO",
                TextStyle {
                    fill: Color::rgba8(220, 20, 20, 255),
                    font_size_pt: 14.0,
                    ..Default::default()
                },
            ),
            path_id: ObjectId(20),
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    )
    .with_outline(OutlineStyle::none());
    top.perspective = Some(
        Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 30.0, 10.0),
            &[
                Point2D::new(20.0, 20.0),
                Point2D::new(60.0, 20.0),
                Point2D::new(60.0, 40.0),
                Point2D::new(20.0, 40.0),
            ],
        )
        .unwrap(),
    );
    let mut zero_grp = SceneObject::new(
        ObjectId(0),
        "ZeroGrp",
        NodePayload::Group(vec![ObjectId(20), ObjectId(21)]),
    )
    .with_transform(Affine2D::translate(10.0, 10.0));
    zero_grp.perspective = Some(
        Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(20.0, 20.0, 40.0, 20.0),
            &[
                Point2D::new(100.0, 100.0),
                Point2D::new(180.0, 100.0),
                Point2D::new(180.0, 140.0),
                Point2D::new(100.0, 140.0),
            ],
        )
        .unwrap(),
    );
    let sym = SymbolDef::new("P5RenderSym", vec![zero_grp, host, top]);
    let sid = sym.id;
    doc.insert_symbol(sym);
    let inst_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P5Inst", NodePayload::SymbolInstance(sid))
                .with_transform(Affine2D::translate(20.0, 20.0)),
            None,
        )
        .unwrap();

    let wb = doc.world_object_bounds(inst_id).unwrap();
    let img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
    let px_per_mm = 150.0 / 25.4;
    let mut red_min_x = f64::INFINITY;
    let mut red_max_x = f64::NEG_INFINITY;
    let mut red_min_y = f64::INFINITY;
    let mut red_max_y = f64::NEG_INFINITY;
    let mut red_px = 0usize;
    for y in 0..img.height {
        for x in 0..img.width {
            let idx = ((y * img.width + x) * 4) as usize;
            let [r, g, b, a] = [
                img.rgba[idx],
                img.rgba[idx + 1],
                img.rgba[idx + 2],
                img.rgba[idx + 3],
            ];
            if a > 128 && r > 150 && g < 90 && b < 90 {
                red_px += 1;
                red_min_x = red_min_x.min(f64::from(x) / px_per_mm);
                red_max_x = red_max_x.max(f64::from(x) / px_per_mm);
                red_min_y = red_min_y.min(f64::from(y) / px_per_mm);
                red_max_y = red_max_y.max(f64::from(y) / px_per_mm);
            }
        }
    }
    assert!(red_px > 40, "must render visible red glyph pixels");
    assert!(
        red_min_x >= 128.0 && red_max_x <= 212.0 && red_min_y >= 130.0 && red_max_y <= 155.0,
        "ObjectId(0) Group(translate, persp) + TextOnPath(sub.persp) must render inside (128..212, 130..155) matching bounds {wb:?}, got ({red_min_x:.2}..{red_max_x:.2}, {red_min_y:.2}..{red_max_y:.2})"
    );
}

#[test]
fn cmyk_icc_pipeline_respected_by_conical_square_blend_contour_extrude_and_lenses() {
    use coree_core::{generate_minimal_cmyk_icc_v2, RenderingIntent};

    let cmyk_bytes =
        generate_minimal_cmyk_icc_v2("ISO Coated v2", BuiltinIccProfile::IsoCoatedV2Cmyk);
    let fogra_path = std::path::Path::new("/usr/share/color/icc/colord/FOGRA39L_coated.icc");
    let active_cmyk_bytes = if fogra_path.is_file() {
        std::fs::read(fogra_path).unwrap_or(cmyk_bytes)
    } else {
        cmyk_bytes
    };

    let cyan = Color::cmyk(1.0, 0.0, 0.0, 0.0);
    let magenta = Color::cmyk(0.0, 1.0, 0.0, 0.0);
    let mid_cm = Color::cmyk(0.5, 0.5, 0.0, 0.0);

    for intent in [
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::AbsoluteColorimetric,
    ] {
        let mut doc = Document::new("CmykIccCallers", 140.0, 100.0, ColorMode::Cmyk, 72.0);
        doc.set_cmyk_icc_profile(active_cmyk_bytes.clone()).unwrap();
        doc.set_rendering_intent(intent);
        let icc = doc.icc_pipeline();
        let expected_cyan = icc.color_to_rgba8(&cyan);
        let expected_magenta = icc.color_to_rgba8(&magenta);
        let expected_mid_cm = icc.color_to_rgba8(&mid_cm);

        assert_ne!(
            [expected_cyan[0], expected_cyan[1], expected_cyan[2]],
            [0, 255, 255],
            "ICC profile must not map CMYK cyan to naive (0,255,255)"
        );

        let assert_px_close = |label: &str, got: [u8; 4], exp: [u8; 4]| {
            let dr = i32::from(got[0]).abs_diff(i32::from(exp[0]));
            let dg = i32::from(got[1]).abs_diff(i32::from(exp[1]));
            let db = i32::from(got[2]).abs_diff(i32::from(exp[2]));
            assert!(
                dr <= 2 && dg <= 2 && db <= 2,
                "{intent:?} {label}: got {got:?}, expected {exp:?} (ICC bypass?)"
            );
        };

        // 1. ConicalGradient (all stops CMYK cyan)
        doc.add_object(
            rect_obj(
                5.0,
                5.0,
                20.0,
                20.0,
                Fill::ConicalGradient {
                    center: Point2D::new(10.0, 10.0),
                    angle_deg: 0.0,
                    stops: vec![
                        GradientStop {
                            offset: 0.0,
                            color: cyan.clone(),
                        },
                        GradientStop {
                            offset: 1.0,
                            color: cyan.clone(),
                        },
                    ],
                },
            ),
            None,
        )
        .unwrap();

        // 2. SquareGradient (all stops CMYK cyan)
        doc.add_object(
            rect_obj(
                30.0,
                5.0,
                20.0,
                20.0,
                Fill::SquareGradient {
                    center: Point2D::new(10.0, 10.0),
                    size: 20.0,
                    angle_deg: 0.0,
                    stops: vec![
                        GradientStop {
                            offset: 0.0,
                            color: cyan.clone(),
                        },
                        GradientStop {
                            offset: 1.0,
                            color: cyan.clone(),
                        },
                    ],
                },
            ),
            None,
        )
        .unwrap();

        // 3. Contour::Outside (cyan base -> magenta target, 1 step)
        let mut contour_obj = make_rect(60.0, 10.0, 10.0, 10.0, cyan.clone());
        contour_obj.effects.push(Effect::Contour {
            mode: ContourMode::Outside,
            steps: 1,
            offset_mm: 5.0,
            fill_color: magenta.clone(),
            outline_color: Color::TRANSPARENT,
        });
        doc.add_object(contour_obj, None).unwrap();

        // 4. Blend (cyan -> magenta, 1 step, midpoint CMYK(0.5, 0.5, 0, 0))
        let dst_id = doc
            .add_object(make_rect(115.0, 5.0, 20.0, 20.0, magenta.clone()), None)
            .unwrap();
        let mut src_blend = make_rect(85.0, 5.0, 20.0, 20.0, cyan.clone());
        src_blend.effects.push(Effect::Blend {
            target_id: dst_id,
            steps: 1,
            rotation_deg: 0.0,
            along_path: None,
            rotate_along_path: false,
        });
        doc.add_object(src_blend, None).unwrap();

        // 5. Extrude25D (CMYK cyan base + CMYK cyan shade)
        let mut ext_obj = make_rect(5.0, 40.0, 20.0, 20.0, cyan.clone());
        ext_obj.effects.push(Effect::Extrude25D {
            vanishing_point: Point2D::new(50.0, 80.0),
            depth: 20.0,
            bevel_depth: 0.0,
            shade_color: cyan.clone(),
            light_dir: [0.0, 0.0, 1.0],
        });
        doc.add_object(ext_obj, None).unwrap();

        // 6. CustomColorMap::Direct lens over a white rect -> maps white to CMYK cyan
        doc.add_object(make_rect(35.0, 40.0, 20.0, 20.0, Color::WHITE), None)
            .unwrap();
        let mut cmap_lens = make_rect(35.0, 40.0, 20.0, 20.0, Color::TRANSPARENT);
        cmap_lens.lens = Some(LensConfig {
            kind: LensKind::CustomColorMap {
                from_color: cyan.clone(),
                to_color: cyan.clone(),
                palette_mode: ColorMapMode::Direct,
            },
            frozen: false,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        });
        doc.add_object(cmap_lens, None).unwrap();

        // 7. Wireframe lens with override_fill = CMYK cyan
        let mut wire_lens = make_rect(65.0, 40.0, 20.0, 20.0, Color::TRANSPARENT);
        wire_lens.lens = Some(LensConfig {
            kind: LensKind::Wireframe {
                override_outline: None,
                override_fill: Some(cyan.clone()),
            },
            frozen: false,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        });
        doc.add_object(wire_lens, None).unwrap();

        let img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
        assert_px_close(
            "ConicalGradient",
            pixel_at_mm(&img, 15.0, 15.0),
            expected_cyan,
        );
        assert_px_close(
            "SquareGradient",
            pixel_at_mm(&img, 40.0, 15.0),
            expected_cyan,
        );
        assert_px_close(
            "Contour::Outside",
            pixel_at_mm(&img, 57.5, 15.0),
            expected_magenta,
        );
        assert_px_close(
            "Blend midpoint",
            pixel_at_mm(&img, 110.0, 15.0),
            expected_mid_cm,
        );

        // Extrude25D side face under center-bottom (15mm, 62mm) should not be naive cyan (0, 255, 255)
        let ext_px = pixel_at_mm(&img, 15.0, 62.0);
        assert!(
            ext_px[1] < 200,
            "{intent:?} Extrude25D side face bypassed CMYK ICC pipeline: {ext_px:?}"
        );
        assert_px_close(
            "CustomColorMap::Direct",
            pixel_at_mm(&img, 45.0, 50.0),
            expected_cyan,
        );
        assert_px_close(
            "Wireframe override_fill",
            pixel_at_mm(&img, 75.0, 50.0),
            expected_cyan,
        );
    }
}

#[test]
fn w3a_r9_isolated_object_render_matches_page_under_custom_3slot_icc() {
    use coree_core::{generate_minimal_cmyk_icc_v2, RenderingIntent};
    use coree_render::render_isolated_object_to_rgba;

    fn build_adobe_wide_rgb_icc_v2() -> Vec<u8> {
        let mut tags: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();
        let xyz_tag = |xyz: [f64; 3]| -> Vec<u8> {
            let mut v = Vec::with_capacity(20);
            v.extend_from_slice(b"XYZ ");
            v.extend_from_slice(&0u32.to_be_bytes());
            for c in xyz {
                v.extend_from_slice(&((c * 65536.0).round() as i32).to_be_bytes());
            }
            v
        };
        let curv_gamma = |gamma: f64| -> Vec<u8> {
            let mut v = Vec::with_capacity(14);
            v.extend_from_slice(b"curv");
            v.extend_from_slice(&0u32.to_be_bytes());
            v.extend_from_slice(&1u32.to_be_bytes());
            v.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
            v
        };
        let mut desc = Vec::new();
        let label = b"WideGamutAdobeRenderTest";
        desc.extend_from_slice(b"desc");
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&((label.len() + 1) as u32).to_be_bytes());
        desc.extend_from_slice(label);
        desc.push(0);
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&0u16.to_be_bytes());
        desc.push(0);
        desc.resize(desc.len() + 67, 0);

        tags.push((b"desc", desc));
        tags.push((b"wtpt", xyz_tag([0.95047, 1.0, 1.08883])));
        tags.push((b"rXYZ", xyz_tag([0.60974, 0.31111, 0.01947])));
        tags.push((b"gXYZ", xyz_tag([0.20528, 0.62567, 0.06087])));
        tags.push((b"bXYZ", xyz_tag([0.14919, 0.06322, 0.74457])));
        tags.push((b"rTRC", curv_gamma(2.19921875)));
        tags.push((b"gTRC", curv_gamma(2.19921875)));
        tags.push((b"bTRC", curv_gamma(2.19921875)));

        let header_size = 128usize;
        let tag_table_size = 4 + tags.len() * 12;
        let mut data_offset = (header_size + tag_table_size + 3) & !3;
        let mut table = Vec::with_capacity(tag_table_size);
        table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut body = Vec::new();
        for (sig, payload) in &tags {
            let aligned_off = data_offset + body.len();
            table.extend_from_slice(*sig);
            table.extend_from_slice(&(aligned_off as u32).to_be_bytes());
            table.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(payload);
            while body.len() % 4 != 0 {
                body.push(0);
            }
        }
        data_offset = (header_size + table.len() + 3) & !3;
        let total_size = data_offset + body.len();
        let mut out = vec![0u8; 128];
        out[0..4].copy_from_slice(&(total_size as u32).to_be_bytes());
        out[4..8].copy_from_slice(b"core");
        out[8] = 2;
        out[12..16].copy_from_slice(b"mntr");
        out[16..20].copy_from_slice(b"RGB ");
        out[20..24].copy_from_slice(b"XYZ ");
        out[36..40].copy_from_slice(b"acsp");
        out[68..72].copy_from_slice(&((0.9642_f64 * 65536.0).round() as i32).to_be_bytes());
        out[72..76].copy_from_slice(&((1.0_f64 * 65536.0).round() as i32).to_be_bytes());
        out[76..80].copy_from_slice(&((0.8249_f64 * 65536.0).round() as i32).to_be_bytes());
        out.extend_from_slice(&table);
        out.resize(data_offset, 0);
        out.extend_from_slice(&body);
        out
    }

    fn build_gray_gamma10_icc_v2() -> Vec<u8> {
        let mut tags: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();
        let mut desc = Vec::new();
        let label = b"LinearGrayGamma1.0";
        desc.extend_from_slice(b"desc");
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&((label.len() + 1) as u32).to_be_bytes());
        desc.extend_from_slice(label);
        desc.push(0);
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&0u16.to_be_bytes());
        desc.push(0);
        desc.resize(desc.len() + 67, 0);

        let mut ktrc = Vec::with_capacity(14);
        ktrc.extend_from_slice(b"curv");
        ktrc.extend_from_slice(&0u32.to_be_bytes());
        ktrc.extend_from_slice(&1u32.to_be_bytes());
        ktrc.extend_from_slice(&((1.0_f64 * 256.0).round() as u16).to_be_bytes());

        tags.push((b"desc", desc));
        tags.push((b"kTRC", ktrc));

        let header_size = 128usize;
        let tag_table_size = 4 + tags.len() * 12;
        let data_offset = (header_size + tag_table_size + 3) & !3;
        let mut table = Vec::with_capacity(tag_table_size);
        table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut body = Vec::new();
        for (sig, payload) in &tags {
            let aligned_off = data_offset + body.len();
            table.extend_from_slice(*sig);
            table.extend_from_slice(&(aligned_off as u32).to_be_bytes());
            table.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(payload);
            while body.len() % 4 != 0 {
                body.push(0);
            }
        }
        let total_size = data_offset + body.len();
        let mut out = vec![0u8; 128];
        out[0..4].copy_from_slice(&(total_size as u32).to_be_bytes());
        out[4..8].copy_from_slice(b"core");
        out[8] = 2;
        out[12..16].copy_from_slice(b"mntr");
        out[16..20].copy_from_slice(b"GRAY");
        out[20..24].copy_from_slice(b"XYZ ");
        out[36..40].copy_from_slice(b"acsp");
        out[68..72].copy_from_slice(&((0.9642_f64 * 65536.0).round() as i32).to_be_bytes());
        out[72..76].copy_from_slice(&((1.0_f64 * 65536.0).round() as i32).to_be_bytes());
        out[76..80].copy_from_slice(&((0.8249_f64 * 65536.0).round() as i32).to_be_bytes());
        out.extend_from_slice(&table);
        out.resize(data_offset, 0);
        out.extend_from_slice(&body);
        out
    }

    struct IccProfileSet<'a> {
        label: &'a str,
        rgb: Vec<u8>,
        cmyk: Vec<u8>,
        gray: Vec<u8>,
        intent: RenderingIntent,
    }

    let mut profile_sets = vec![IccProfileSet {
        label: "OfflineSynthetic3Slot",
        rgb: build_adobe_wide_rgb_icc_v2(),
        cmyk: generate_minimal_cmyk_icc_v2("TestSWOP", BuiltinIccProfile::UsWebCoatedSwopCmyk),
        gray: build_gray_gamma10_icc_v2(),
        intent: RenderingIntent::RelativeColorimetric,
    }];

    let prophoto = std::path::Path::new("/usr/share/color/icc/colord/ProPhotoRGB.icc");
    let swop = std::path::Path::new("/usr/share/color/icc/colord/SWOP_TR003_coated_3.icc");
    let sgray = std::path::Path::new("/usr/share/color/icc/ghostscript/sgray.icc");
    if prophoto.is_file() && swop.is_file() && sgray.is_file() {
        profile_sets.push(IccProfileSet {
            label: "SystemProPhotoSwopSGray",
            rgb: std::fs::read(prophoto).unwrap(),
            cmyk: std::fs::read(swop).unwrap(),
            gray: std::fs::read(sgray).unwrap(),
            intent: RenderingIntent::RelativeColorimetric,
        });
    }
    let adobe_sys = std::path::Path::new("/usr/share/color/icc/colord/AdobeRGB1998.icc");
    let fogra_sys = std::path::Path::new("/usr/share/color/icc/colord/FOGRA39L_coated.icc");
    if adobe_sys.is_file() && fogra_sys.is_file() && sgray.is_file() {
        profile_sets.push(IccProfileSet {
            label: "SystemAdobeFograSGray",
            rgb: std::fs::read(adobe_sys).unwrap(),
            cmyk: std::fs::read(fogra_sys).unwrap(),
            gray: std::fs::read(sgray).unwrap(),
            intent: RenderingIntent::Perceptual,
        });
    }

    let dpi = 150.0;
    let px_per_mm = dpi / 25.4;

    for IccProfileSet {
        label,
        rgb: rgb_icc,
        cmyk: cmyk_icc,
        gray: gray_icc,
        intent,
    } in profile_sets
    {
        for color_mode in [ColorMode::Rgb, ColorMode::Cmyk] {
            let mut doc = Document::new(label, 160.0, 120.0, color_mode, dpi as f32);
            doc.set_rendering_intent(intent);
            doc.set_black_point_compensation(true);
            doc.set_rgb_icc_profile(rgb_icc.clone()).unwrap();
            doc.set_cmyk_icc_profile(cmyk_icc.clone()).unwrap();
            doc.set_gray_icc_profile(gray_icc.clone()).unwrap();

            // 3 distinct color-space backdrop rows (CMYK, RGB, Gray) covering x=10..128 (leaving x=130..150 over white paper)
            doc.add_object(
                make_rect(10.0, 10.0, 118.0, 30.0, Color::cmyk(0.65, 0.15, 0.25, 0.08)),
                None,
            )
            .unwrap();
            doc.add_object(
                make_rect(10.0, 45.0, 118.0, 30.0, Color::rgb(0.72, 0.45, 0.18)),
                None,
            )
            .unwrap();
            doc.add_object(make_rect(10.0, 80.0, 118.0, 30.0, Color::gray(0.45)), None)
                .unwrap();

            let lens_specs: Vec<(&str, f64, LensConfig)> = vec![
                (
                    "BrightenLive",
                    12.0,
                    LensConfig::new(LensKind::Brighten { rate_pct: 0.0 }),
                ),
                (
                    "BrightenFrozen",
                    32.0,
                    LensConfig {
                        kind: LensKind::Brighten { rate_pct: 0.0 },
                        frozen: true,
                        viewpoint: None,
                        remove_face: false,
                        frozen_snapshot: None,
                    },
                ),
                (
                    "ColorAdd",
                    52.0,
                    LensConfig::new(LensKind::ColorAdd {
                        color: Color::cmyk(0.1, 0.8, 0.2, 0.0),
                        rate_pct: 40.0,
                    }),
                ),
                (
                    "Transparency",
                    72.0,
                    LensConfig::new(LensKind::Transparency {
                        color: Color::rgb(0.15, 0.80, 0.35),
                        rate_pct: 60.0,
                    }),
                ),
                (
                    "CustomColorMap",
                    92.0,
                    LensConfig::new(LensKind::CustomColorMap {
                        from_color: Color::cmyk(0.8, 0.1, 0.1, 0.0),
                        to_color: Color::rgb(0.9, 0.2, 0.5),
                        palette_mode: ColorMapMode::Direct,
                    }),
                ),
                (
                    "TintedGrayscale",
                    112.0,
                    LensConfig::new(LensKind::TintedGrayscale {
                        tint: Color::cmyk(0.0, 0.85, 0.75, 0.0),
                    }),
                ),
            ];

            let mut check_points: Vec<(&str, ObjectId, f64, f64)> = Vec::new();
            for (name, x_mm, cfg) in lens_specs {
                let mut obj = make_rect(x_mm, 10.0, 16.0, 100.0, Color::TRANSPARENT);
                obj.lens = Some(cfg);
                let oid = doc.add_object(obj, None).unwrap();
                check_points.push((name, oid, x_mm + 8.0, 25.0));
                check_points.push((name, oid, x_mm + 8.0, 60.0));
                check_points.push((name, oid, x_mm + 8.0, 95.0));
            }

            // ConicalGradient + InnerShadow + Effect::Transparency isolated effect object
            let mut eff_obj = rect_obj(
                130.0,
                10.0,
                20.0,
                30.0,
                Fill::ConicalGradient {
                    center: Point2D::new(10.0, 15.0),
                    angle_deg: 0.0,
                    stops: vec![
                        GradientStop {
                            offset: 0.0,
                            color: Color::cmyk(0.9, 0.1, 0.1, 0.0),
                        },
                        GradientStop {
                            offset: 1.0,
                            color: Color::cmyk(0.1, 0.8, 0.6, 0.0),
                        },
                    ],
                },
            );
            eff_obj.effects.push(Effect::InnerShadow {
                dx_mm: 2.0,
                dy_mm: 2.0,
                blur_mm: 1.5,
                opacity: 0.75,
                color: Color::cmyk(0.8, 0.2, 0.1, 0.3),
                feather_direction: FeatherDirection::Inside,
            });
            eff_obj.effects.push(Effect::Transparency {
                mode: TransparencyMode::Uniform(0.65),
                blend_mode: BlendMode::Normal,
            });
            let eff_oid = doc.add_object(eff_obj, None).unwrap();

            let page_img = render_page_to_rgba(&doc, 0, dpi, ViewMode::Enhanced).unwrap();

            // Control doc with default ICC profiles to confirm active ICC changes pixel outputs
            let mut default_doc = doc.clone();
            default_doc.clear_rgb_icc_profile();
            default_doc.clear_cmyk_icc_profile();
            default_doc.clear_gray_icc_profile();
            default_doc.clear_custom_icc_profile();

            let mut max_default_diff = 0u8;
            let mut iso_cache: std::collections::HashMap<ObjectId, (Rect2D, RgbaBuffer)> =
                std::collections::HashMap::new();
            for (name, oid, x_mm, y_mm) in check_points {
                let (rect_mm, iso_buf) = iso_cache.entry(oid).or_insert_with(|| {
                    render_isolated_object_to_rgba(&doc, 0, oid, Affine2D::IDENTITY, dpi)
                        .unwrap()
                        .unwrap()
                });
                let sx = ((x_mm - rect_mm.min_x) * px_per_mm).round() as u32;
                let sy = ((y_mm - rect_mm.min_y) * px_per_mm).round() as u32;
                let iso_px = iso_buf.get_pixel(sx, sy).unwrap();
                let page_px = pixel_at_mm(&page_img, x_mm, y_mm);
                for c in 0..4 {
                    assert!(
                        iso_px[c].abs_diff(page_px[c]) <= 1,
                        "{label}/{color_mode:?} {name} @ ({x_mm},{y_mm}) ch {c}: iso={iso_px:?} vs page={page_px:?}"
                    );
                }

                let (def_rect, def_buf) =
                    render_isolated_object_to_rgba(&default_doc, 0, oid, Affine2D::IDENTITY, dpi)
                        .unwrap()
                        .unwrap();
                let dsx = ((x_mm - def_rect.min_x) * px_per_mm).round() as u32;
                let dsy = ((y_mm - def_rect.min_y) * px_per_mm).round() as u32;
                let def_px = def_buf.get_pixel(dsx, dsy).unwrap();
                for c in 0..3 {
                    max_default_diff = max_default_diff.max(iso_px[c].abs_diff(def_px[c]));
                }
            }
            assert!(
                max_default_diff >= 8,
                "{label}/{color_mode:?} isolated lens renders must diverge from default ICC (max_diff={max_default_diff})"
            );

            // Also verify effect object (ConicalGradient + InnerShadow + Transparency) matches page when composited over white
            // Sample at (145.0, 25.0), away from the ConicalGradient center singularity at (140.0, 25.0) and InnerShadow edge
            let (eff_rect, eff_buf) =
                render_isolated_object_to_rgba(&doc, 0, eff_oid, Affine2D::IDENTITY, dpi)
                    .unwrap()
                    .unwrap();
            let esx = ((145.0 - eff_rect.min_x) * px_per_mm).round() as u32;
            let esy = ((25.0 - eff_rect.min_y) * px_per_mm).round() as u32;
            let eff_iso = eff_buf.get_pixel(esx, esy).unwrap();
            // Composite straight-alpha eff_iso over white background (matching RenderOptions::export() white page)
            let a = f32::from(eff_iso[3]) / 255.0;
            let eff_over_white = [
                (f32::from(eff_iso[0]) * a + 255.0 * (1.0 - a)).round() as u8,
                (f32::from(eff_iso[1]) * a + 255.0 * (1.0 - a)).round() as u8,
                (f32::from(eff_iso[2]) * a + 255.0 * (1.0 - a)).round() as u8,
                255,
            ];
            let eff_page = pixel_at_mm(&page_img, 145.0, 25.0);
            for c in 0..4 {
                assert!(
                    eff_over_white[c].abs_diff(eff_page[c]) <= 2,
                    "{label}/{color_mode:?} EffectObj ch {c}: iso_over_white={eff_over_white:?} vs page={eff_page:?}"
                );
            }

            // Verify DirtyTileCache ViewportKey hashes document ICC changes without mark_all_dirty()
            let mut cache = DirtyTileCache::new(240, 180);
            let mut renderer = SceneRenderer::new();
            let vp = Viewport::new(240, 180);
            let opts = RenderOptions::export();
            let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
            assert_eq!(
                cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer),
                0
            );
            assert!(
                cache.render_dirty_tiles(
                    &default_doc,
                    &vp,
                    ViewMode::Enhanced,
                    &opts,
                    &mut renderer
                ) > 0,
                "ViewportKey must invalidate DirtyTileCache when document ICC profiles change"
            );
        }
    }
}

#[test]
fn display_icc_transform_shifts_pixels_and_invalidates_tile_cache() {
    use coree_core::{
        generate_minimal_display_p3_icc_v2, generate_minimal_srgb_icc_v2, DisplayTransformLut3D,
    };

    let srgb_bytes = generate_minimal_srgb_icc_v2("sRGB");
    let srgb_prof = coree_core::IccProfile::from_bytes(&srgb_bytes).unwrap();
    let srgb_lut = DisplayTransformLut3D::build_from_profile(
        &srgb_prof,
        coree_core::RenderingIntent::RelativeColorimetric,
    )
    .unwrap();
    let mut px = [180u8, 90, 45, 255];
    srgb_lut.apply_rgba(&mut px);
    assert!(
        px[0].abs_diff(180) <= 5 && px[1].abs_diff(90) <= 5 && px[2].abs_diff(45) <= 5,
        "sRGB gamma-2.2 display LUT must closely preserve sRGB values: got {px:?}"
    );

    let p3_bytes = generate_minimal_display_p3_icc_v2("Display P3");
    let mut doc = Document::new_a4("DisplayIccRender");
    let rect = SceneObject::new(
        ObjectId(0),
        "Red",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 80.0,
            height: 80.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(220, 40, 30, 255)))
    .with_transform(Affine2D::translate(10.0, 10.0));
    doc.add_object(rect, None).unwrap();

    let vp = Viewport::new(200, 200);
    let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
    let mut renderer = SceneRenderer::new();
    let opts_plain = RenderOptions::export();
    let img_plain =
        cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts_plain, &mut renderer);

    let mut opts_disp = RenderOptions::export();
    opts_disp.display_icc_profile = Some(p3_bytes);
    let rerendered =
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts_disp, &mut renderer);
    assert!(
        rerendered > 0,
        "Changing display_icc_profile must invalidate DirtyTileCache"
    );
    let img_disp = cache.composite_to_rgba();
    assert_ne!(
        img_plain.get_pixel(60, 60),
        img_disp.get_pixel(60, 60),
        "DisplayP3 monitor profile must shift rendered sRGB red into DisplayP3 coordinates"
    );
}

#[test]
fn w3_mesh_fill_with_inward_handles_warps_interior_coons_under_envelope() {
    use coree_core::{Effect, EnvelopeMode, MeshNode};

    let build_doc = |with_envelope: bool| -> Document {
        let mut doc = Document::new_a4("MeshEnvelopeWarp");
        let mesh = Fill::MeshFill {
            rows: 2,
            cols: 2,
            nodes: vec![
                MeshNode {
                    row: 0,
                    col: 0,
                    pos: Point2D::new(0.0, 0.0),
                    color: Color::rgb(1.0, 0.0, 0.0),
                    handle_n: None,
                    handle_s: Some(Point2D::new(0.0, 20.0)),
                    handle_e: Some(Point2D::new(20.0, 15.0)),
                    handle_w: None,
                    alpha: 1.0,
                },
                MeshNode {
                    row: 0,
                    col: 1,
                    pos: Point2D::new(80.0, 0.0),
                    color: Color::rgb(0.0, 0.0, 1.0),
                    handle_n: None,
                    handle_s: Some(Point2D::new(0.0, 20.0)),
                    handle_e: None,
                    handle_w: Some(Point2D::new(-20.0, 15.0)),
                    alpha: 1.0,
                },
                MeshNode {
                    row: 1,
                    col: 0,
                    pos: Point2D::new(0.0, 80.0),
                    color: Color::rgb(0.0, 1.0, 0.0),
                    handle_n: Some(Point2D::new(0.0, -20.0)),
                    handle_s: None,
                    handle_e: Some(Point2D::new(20.0, -15.0)),
                    handle_w: None,
                    alpha: 1.0,
                },
                MeshNode {
                    row: 1,
                    col: 1,
                    pos: Point2D::new(80.0, 80.0),
                    color: Color::rgb(1.0, 1.0, 0.0),
                    handle_n: Some(Point2D::new(0.0, -20.0)),
                    handle_s: None,
                    handle_e: None,
                    handle_w: Some(Point2D::new(-20.0, -15.0)),
                    alpha: 1.0,
                },
            ],
        };
        let mut obj = SceneObject::new(
            ObjectId(0),
            "CoonsMesh",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 80.0,
                height: 80.0,
                corner_radii: [4.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(mesh)
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(20.0, 20.0));
        if with_envelope {
            obj.effects.push(Effect::Envelope {
                mode: EnvelopeMode::SingleArc,
                cage: vec![
                    Point2D::new(0.0, 0.0),
                    Point2D::new(80.0, 6.0),
                    Point2D::new(80.0, 80.0),
                    Point2D::new(0.0, 74.0),
                ],
            });
        }
        doc.add_object(obj, None).unwrap();
        doc
    };

    let doc_plain = build_doc(false);
    let doc_env = build_doc(true);
    let mut vp = Viewport::new(320, 320);
    vp.fit_rect(Rect2D::from_xywh(10.0, 10.0, 100.0, 100.0), 8.0);
    let opts = RenderOptions::export();

    let mut b_plain = TinySkiaBackend::new(320, 320).unwrap();
    let mut b_env = TinySkiaBackend::new(320, 320).unwrap();
    let mut renderer = SceneRenderer::new();
    renderer.render_document(&doc_plain, &vp, ViewMode::Enhanced, &opts, &mut b_plain);
    renderer.render_document(&doc_env, &vp, ViewMode::Enhanced, &opts, &mut b_env);
    let img_plain = b_plain.into_rgba();
    let img_env = b_env.into_rgba();

    let sum_abs: u64 = img_plain
        .rgba
        .iter()
        .zip(img_env.rgba.iter())
        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
        .sum();
    let mean_abs = (sum_abs as f64) / (img_plain.rgba.len() as f64);
    assert!(
        mean_abs > 1.0,
        "MeshFill + Envelope must warp interior Coons patch pixels (not just clip mask), got mean_abs_diff={mean_abs:.4}"
    );
}

#[test]
fn w3_deformed_group_clone_mesh_fill_single_perspective() {
    use coree_core::{Effect, EnvelopeMode, MeshNode, Perspective3x3};

    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 80.0, 80.0),
        &[
            Point2D::new(8.0, 4.0),
            Point2D::new(76.0, 12.0),
            Point2D::new(70.0, 76.0),
            Point2D::new(12.0, 74.0),
        ],
    )
    .expect("valid perspective quad");

    let mesh = Fill::MeshFill {
        rows: 2,
        cols: 2,
        nodes: vec![
            MeshNode {
                row: 0,
                col: 0,
                pos: Point2D::new(0.0, 0.0),
                color: Color::rgb(1.0, 0.0, 0.0),
                handle_n: None,
                handle_s: Some(Point2D::new(0.0, 20.0)),
                handle_e: Some(Point2D::new(20.0, 15.0)),
                handle_w: None,
                alpha: 1.0,
            },
            MeshNode {
                row: 0,
                col: 1,
                pos: Point2D::new(80.0, 0.0),
                color: Color::rgb(0.0, 0.0, 1.0),
                handle_n: None,
                handle_s: Some(Point2D::new(0.0, 20.0)),
                handle_e: None,
                handle_w: Some(Point2D::new(-20.0, 15.0)),
                alpha: 1.0,
            },
            MeshNode {
                row: 1,
                col: 0,
                pos: Point2D::new(0.0, 80.0),
                color: Color::rgb(0.0, 1.0, 0.0),
                handle_n: Some(Point2D::new(0.0, -20.0)),
                handle_s: None,
                handle_e: Some(Point2D::new(20.0, -15.0)),
                handle_w: None,
                alpha: 1.0,
            },
            MeshNode {
                row: 1,
                col: 1,
                pos: Point2D::new(80.0, 80.0),
                color: Color::rgb(1.0, 1.0, 0.0),
                handle_n: Some(Point2D::new(0.0, -20.0)),
                handle_s: None,
                handle_e: None,
                handle_w: Some(Point2D::new(-20.0, -15.0)),
                alpha: 1.0,
            },
        ],
    };

    let master_shape = NodePayload::Shape(ParametricShape::Rectangle {
        width: 80.0,
        height: 80.0,
        corner_radii: [4.0; 4],
        corner_style: CornerStyle::Rounded,
        lock_corners: true,
    });

    let build_doc = |use_clone: bool, with_persp: bool| -> Document {
        let mut doc = Document::new_a4("DeformedCloneMeshPersp");
        let child_id = if use_clone {
            let mut master = SceneObject::new(ObjectId(0), "MasterMesh", master_shape.clone())
                .with_fill(mesh.clone())
                .with_outline(OutlineStyle::none());
            master.visible = false;
            let master_id = doc.add_object(master, None).unwrap();

            let mut clone_obj = SceneObject::new(
                ObjectId(0),
                "CloneMesh",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(20.0, 20.0));
            if with_persp {
                clone_obj.perspective = Some(persp);
            }
            doc.add_object(clone_obj, None).unwrap()
        } else {
            let mut direct = SceneObject::new(ObjectId(0), "DirectMesh", master_shape.clone())
                .with_fill(mesh.clone())
                .with_outline(OutlineStyle::none())
                .with_transform(Affine2D::translate(20.0, 20.0));
            if with_persp {
                direct.perspective = Some(persp);
            }
            doc.add_object(direct, None).unwrap()
        };

        let mut grp = SceneObject::new(
            ObjectId(0),
            "DeformedGroup",
            NodePayload::Group(vec![child_id]),
        );
        grp.effects.push(Effect::Envelope {
            mode: EnvelopeMode::SingleArc,
            cage: vec![
                Point2D::new(20.0, 20.0),
                Point2D::new(100.0, 28.0),
                Point2D::new(100.0, 100.0),
                Point2D::new(20.0, 92.0),
            ],
        });
        doc.add_object(grp, None).unwrap();
        doc
    };

    let doc_direct = build_doc(false, true);
    let doc_clone = build_doc(true, true);
    let doc_clone_no_persp = build_doc(true, false);

    let mut vp = Viewport::new(320, 320);
    vp.fit_rect(Rect2D::from_xywh(10.0, 10.0, 100.0, 100.0), 8.0);
    let opts = RenderOptions::export();

    let mut b_direct = TinySkiaBackend::new(320, 320).unwrap();
    let mut b_clone = TinySkiaBackend::new(320, 320).unwrap();
    let mut b_no_persp = TinySkiaBackend::new(320, 320).unwrap();
    let mut renderer = SceneRenderer::new();
    renderer.render_document(&doc_direct, &vp, ViewMode::Enhanced, &opts, &mut b_direct);
    renderer.render_document(&doc_clone, &vp, ViewMode::Enhanced, &opts, &mut b_clone);
    renderer.render_document(
        &doc_clone_no_persp,
        &vp,
        ViewMode::Enhanced,
        &opts,
        &mut b_no_persp,
    );

    let img_direct = b_direct.into_rgba();
    let img_clone = b_clone.into_rgba();
    let img_no_persp = b_no_persp.into_rgba();

    assert_eq!(
        img_clone.rgba, img_direct.rgba,
        "Deformed-group Clone with MeshFill+Perspective must match direct render pixel-for-pixel (no double perspective)"
    );

    let sum_abs: u64 = img_clone
        .rgba
        .iter()
        .zip(img_no_persp.rgba.iter())
        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
        .sum();
    let mean_abs = (sum_abs as f64) / (img_clone.rgba.len() as f64);
    assert!(
        mean_abs > 1.0,
        "Perspective on deformed-group Clone must visibly warp MeshFill, got mean_abs_diff={mean_abs:.4}"
    );
}

#[test]
fn w3_clone_with_lens_and_remove_face_clone_backdrop_render() {
    use coree_core::{Effect, EnvelopeMode, LensConfig, LensKind};

    let rect_payload = |w: f64, h: f64| {
        NodePayload::Shape(ParametricShape::Rectangle {
            width: w,
            height: h,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        })
    };

    // 1. Standalone Clone carrying Lens::Invert over Red backdrop must match direct Lens::Invert
    let build_standalone = |use_clone: bool| -> Document {
        let mut doc = Document::new("CloneLensStandalone", 100.0, 100.0, ColorMode::Rgb, 72.0);
        doc.add_object(
            make_rect(10.0, 10.0, 80.0, 80.0, Color::rgba8(255, 0, 0, 255)),
            None,
        )
        .unwrap();
        let lens_cfg = LensConfig {
            kind: LensKind::Invert,
            frozen: false,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        };
        let mut lens_obj = if use_clone {
            let mut master = SceneObject::new(ObjectId(0), "LensMaster", rect_payload(40.0, 40.0))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none());
            master.visible = false;
            let master_id = doc.add_object(master, None).unwrap();
            SceneObject::new(
                ObjectId(0),
                "LensClone",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(30.0, 30.0))
        } else {
            SceneObject::new(ObjectId(0), "LensDirect", rect_payload(40.0, 40.0))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(Affine2D::translate(30.0, 30.0))
        };
        lens_obj.lens = Some(lens_cfg);
        doc.add_object(lens_obj, None).unwrap();
        doc
    };

    let img_sa_direct =
        render_page_to_rgba(&build_standalone(false), 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_sa_clone =
        render_page_to_rgba(&build_standalone(true), 0, 72.0, ViewMode::Enhanced).unwrap();
    assert_eq!(
        img_sa_clone.rgba, img_sa_direct.rgba,
        "Standalone Clone with Lens::Invert must match direct Lens::Invert pixel-for-pixel"
    );
    let px_sa = pixel_at_mm(&img_sa_clone, 50.0, 50.0);
    assert!(
        px_sa[0] < 20 && px_sa[1] > 235 && px_sa[2] > 235,
        "Standalone Clone with Lens::Invert over Red must produce Cyan, got {px_sa:?}"
    );

    // 2. Deformed Group(Envelope) containing Clone carrying Lens::Invert over Red backdrop
    let build_deformed = |use_clone: bool| -> Document {
        let mut doc = Document::new("CloneLensDeformed", 120.0, 120.0, ColorMode::Rgb, 72.0);
        doc.add_object(
            make_rect(10.0, 10.0, 100.0, 100.0, Color::rgba8(255, 0, 0, 255)),
            None,
        )
        .unwrap();
        let lens_cfg = LensConfig {
            kind: LensKind::Invert,
            frozen: false,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        };
        let mut lens_obj = if use_clone {
            let mut master = SceneObject::new(ObjectId(0), "LensMaster", rect_payload(40.0, 40.0))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none());
            master.visible = false;
            let master_id = doc.add_object(master, None).unwrap();
            SceneObject::new(
                ObjectId(0),
                "LensClone",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(30.0, 30.0))
        } else {
            SceneObject::new(ObjectId(0), "LensDirect", rect_payload(40.0, 40.0))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(Affine2D::translate(30.0, 30.0))
        };
        lens_obj.lens = Some(lens_cfg);
        let child_id = doc.add_object(lens_obj, None).unwrap();
        let mut grp = SceneObject::new(ObjectId(0), "EnvGroup", NodePayload::Group(vec![child_id]));
        grp.effects.push(Effect::Envelope {
            mode: EnvelopeMode::Straight,
            cage: vec![
                Point2D::new(30.0, 30.0),
                Point2D::new(90.0, 30.0),
                Point2D::new(90.0, 90.0),
                Point2D::new(30.0, 90.0),
            ],
        });
        doc.add_object(grp, None).unwrap();
        doc
    };

    let img_df_direct =
        render_page_to_rgba(&build_deformed(false), 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_df_clone =
        render_page_to_rgba(&build_deformed(true), 0, 72.0, ViewMode::Enhanced).unwrap();
    assert_eq!(
        img_df_clone.rgba, img_df_direct.rgba,
        "Deformed-group Clone with Lens::Invert must match direct Lens::Invert pixel-for-pixel"
    );
    let px_df_ext = pixel_at_mm(&img_df_clone, 82.0, 82.0);
    assert!(
        px_df_ext[0] < 20 && px_df_ext[1] > 235 && px_df_ext[2] > 235,
        "Deformed-group Clone with Lens::Invert must warp lens region to cover (82,82)mm, got {px_df_ext:?}"
    );

    // 3. Lens::Invert(remove_face=true) over a NodePayload::Clone backdrop exercises rasterize_silhouette_payload
    let mut doc_rf = Document::new(
        "CloneBackdropRemoveFace",
        100.0,
        100.0,
        ColorMode::Rgb,
        72.0,
    );
    let mut red_master = SceneObject::new(ObjectId(0), "RedMaster", rect_payload(30.0, 60.0))
        .with_fill(Fill::Solid(Color::rgba8(240, 20, 20, 255)))
        .with_outline(OutlineStyle::none());
    red_master.visible = false;
    let red_master_id = doc_rf.add_object(red_master, None).unwrap();
    let red_clone = SceneObject::new(
        ObjectId(0),
        "RedCloneBackdrop",
        NodePayload::Clone {
            master_id: red_master_id,
            link_fill: true,
            link_outline: true,
            link_shape: true,
        },
    )
    .with_transform(Affine2D::translate(10.0, 20.0));
    doc_rf.add_object(red_clone, None).unwrap();

    let mut rf_lens = make_rect(25.0, 30.0, 35.0, 40.0, Color::TRANSPARENT);
    rf_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: true,
        frozen_snapshot: None,
    });
    doc_rf.add_object(rf_lens, None).unwrap();

    let img_rf = render_page_to_rgba(&doc_rf, 0, 72.0, ViewMode::Enhanced).unwrap();
    let over_clone_red = pixel_at_mm(&img_rf, 32.0, 50.0);
    assert!(
        over_clone_red[0] < 40 && over_clone_red[1] > 200 && over_clone_red[2] > 200,
        "remove_face Lens over Clone backdrop must invert Clone Red -> Cyan, got {over_clone_red:?}"
    );
    let over_blank = pixel_at_mm(&img_rf, 50.0, 50.0);
    assert_eq!(
        over_blank,
        [255, 255, 255, 255],
        "remove_face Lens must leave blank page untouched outside Clone backdrop"
    );
}

#[test]
fn w3_deformed_group_clone_of_group_opacity_matches_direct() {
    let build_doc = |use_clone: bool| -> Document {
        let mut doc = Document::new(
            "DeformedCloneGroupOpacity",
            120.0,
            120.0,
            ColorMode::Rgb,
            72.0,
        );
        doc.add_object(
            make_rect(0.0, 0.0, 120.0, 120.0, Color::rgba8(0, 0, 255, 255)),
            None,
        )
        .unwrap();

        let child_id = if use_clone {
            let page_id = doc.pages()[0].id;
            let hidden_lid = doc
                .add_layer(page_id, "HiddenMasters", false, MasterLayerScope::None)
                .unwrap();
            doc.set_layer_flags(hidden_lid, Some(false), None, None, None)
                .unwrap();
            let inner_rect = make_rect(0.0, 0.0, 60.0, 60.0, Color::rgba8(255, 0, 0, 255));
            let inner_id = doc.add_object(inner_rect, Some(hidden_lid)).unwrap();
            let master_grp =
                SceneObject::new(ObjectId(0), "MasterGrp", NodePayload::Group(vec![inner_id]));
            let master_id = doc.add_object(master_grp, Some(hidden_lid)).unwrap();
            let mut clone_grp = SceneObject::new(
                ObjectId(0),
                "CloneGrp",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(20.0, 20.0));
            clone_grp.opacity = 0.5;
            doc.add_object(clone_grp, Some(hidden_lid)).unwrap()
        } else {
            let page_id = doc.pages()[0].id;
            let hidden_lid = doc
                .add_layer(
                    page_id,
                    "DetachedChildStorage",
                    false,
                    MasterLayerScope::None,
                )
                .unwrap();
            doc.set_layer_flags(hidden_lid, Some(false), None, None, None)
                .unwrap();
            let inner_rect = make_rect(0.0, 0.0, 60.0, 60.0, Color::rgba8(255, 0, 0, 255));
            let inner_id = doc.add_object(inner_rect, Some(hidden_lid)).unwrap();
            let mut direct_grp =
                SceneObject::new(ObjectId(0), "DirectGrp", NodePayload::Group(vec![inner_id]))
                    .with_transform(Affine2D::translate(20.0, 20.0));
            direct_grp.opacity = 0.5;
            doc.add_object(direct_grp, Some(hidden_lid)).unwrap()
        };

        let mut outer = SceneObject::new(
            ObjectId(0),
            "DeformedOuter",
            NodePayload::Group(vec![child_id]),
        );
        outer.effects.push(Effect::Envelope {
            mode: EnvelopeMode::SingleArc,
            cage: vec![
                Point2D::new(20.0, 20.0),
                Point2D::new(100.0, 26.0),
                Point2D::new(100.0, 100.0),
                Point2D::new(20.0, 94.0),
            ],
        });
        doc.add_object(outer, None).unwrap();
        doc
    };

    let img_direct = render_page_to_rgba(&build_doc(false), 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_clone = render_page_to_rgba(&build_doc(true), 0, 72.0, ViewMode::Enhanced).unwrap();
    assert_eq!(
        img_clone.rgba, img_direct.rgba,
        "Deformed Group(Envelope) containing Clone(Group, opacity=0.5) must match direct pixel-for-pixel"
    );
    let center = pixel_at_mm(&img_clone, 55.0, 55.0);
    assert!(
        (110..=146).contains(&center[0]) && center[1] < 10 && (110..=146).contains(&center[2]),
        "Expected 50% Red over Blue (~[128, 0, 128, 255]), got {center:?}"
    );
}

#[test]
fn w3_deformed_group_clone_of_text_with_transparency_matches_direct() {
    let text_style = TextStyle {
        font_size_pt: 28.0,
        fill: Color::rgba8(220, 20, 20, 255),
        ..TextStyle::default()
    };
    let build_doc = |use_clone: bool| -> Document {
        let mut doc = Document::new(
            "DeformedCloneTextTransparency",
            120.0,
            100.0,
            ColorMode::Rgb,
            72.0,
        );
        let page_id = doc.pages()[0].id;
        let hidden_lid = doc
            .add_layer(page_id, "HiddenStorage", false, MasterLayerScope::None)
            .unwrap();
        doc.set_layer_flags(hidden_lid, Some(false), None, None, None)
            .unwrap();
        let trans = Effect::Transparency {
            mode: TransparencyMode::Uniform(0.5),
            blend_mode: BlendMode::Normal,
        };
        let child_id = if use_clone {
            let master_txt = SceneObject::new(
                ObjectId(0),
                "MasterText",
                NodePayload::ArtisticText(ArtisticText::from_plain("COREE", text_style.clone())),
            );
            let master_id = doc.add_object(master_txt, Some(hidden_lid)).unwrap();
            let mut clone_txt = SceneObject::new(
                ObjectId(0),
                "CloneText",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(12.0, 25.0));
            clone_txt.effects.push(trans);
            doc.add_object(clone_txt, Some(hidden_lid)).unwrap()
        } else {
            let mut direct_txt = SceneObject::new(
                ObjectId(0),
                "DirectText",
                NodePayload::ArtisticText(ArtisticText::from_plain("COREE", text_style.clone())),
            )
            .with_transform(Affine2D::translate(12.0, 25.0));
            direct_txt.effects.push(trans);
            doc.add_object(direct_txt, Some(hidden_lid)).unwrap()
        };

        let mut outer = SceneObject::new(
            ObjectId(0),
            "DeformedOuter",
            NodePayload::Group(vec![child_id]),
        );
        outer.effects.push(Effect::Envelope {
            mode: EnvelopeMode::SingleArc,
            cage: vec![
                Point2D::new(10.0, 15.0),
                Point2D::new(110.0, 22.0),
                Point2D::new(110.0, 82.0),
                Point2D::new(10.0, 75.0),
            ],
        });
        doc.add_object(outer, None).unwrap();
        doc
    };

    let img_direct = render_page_to_rgba(&build_doc(false), 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_clone = render_page_to_rgba(&build_doc(true), 0, 72.0, ViewMode::Enhanced).unwrap();
    let non_white = img_clone
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] < 250 || px[1] < 250 || px[2] < 250)
        .count();
    assert!(
        non_white > 100,
        "Deformed Clone(ArtisticText) with Effect::Transparency must render visible pixels, got {non_white}"
    );
    assert_eq!(
        img_clone.rgba, img_direct.rgba,
        "Deformed Group(Envelope) containing Clone(ArtisticText) with Effect::Transparency must match direct pixel-for-pixel"
    );
}

#[test]
fn w3_group_dropshadow_containing_clone_of_text_matches_direct() {
    let text_style = TextStyle {
        font_size_pt: 28.0,
        fill: Color::rgba8(20, 60, 220, 255),
        ..TextStyle::default()
    };
    let build_doc = |use_clone: bool| -> Document {
        let mut doc = Document::new(
            "GroupDropShadowCloneText",
            130.0,
            100.0,
            ColorMode::Rgb,
            72.0,
        );
        let page_id = doc.pages()[0].id;
        let hidden_lid = doc
            .add_layer(page_id, "HiddenStorage", false, MasterLayerScope::None)
            .unwrap();
        doc.set_layer_flags(hidden_lid, Some(false), None, None, None)
            .unwrap();
        let child_id = if use_clone {
            let master_txt = SceneObject::new(
                ObjectId(0),
                "MasterText",
                NodePayload::ArtisticText(ArtisticText::from_plain("SHADOW", text_style.clone())),
            );
            let master_id = doc.add_object(master_txt, Some(hidden_lid)).unwrap();
            let clone_txt = SceneObject::new(
                ObjectId(0),
                "CloneText",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(15.0, 30.0));
            doc.add_object(clone_txt, Some(hidden_lid)).unwrap()
        } else {
            let direct_txt = SceneObject::new(
                ObjectId(0),
                "DirectText",
                NodePayload::ArtisticText(ArtisticText::from_plain("SHADOW", text_style.clone())),
            )
            .with_transform(Affine2D::translate(15.0, 30.0));
            doc.add_object(direct_txt, Some(hidden_lid)).unwrap()
        };

        let mut grp = SceneObject::new(
            ObjectId(0),
            "ShadowGroup",
            NodePayload::Group(vec![child_id]),
        );
        grp.effects.push(Effect::DropShadow {
            dx_mm: 6.0,
            dy_mm: 6.0,
            blur_mm: 2.0,
            opacity: 0.8,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Middle,
        });
        doc.add_object(grp, None).unwrap();
        doc
    };

    let img_direct = render_page_to_rgba(&build_doc(false), 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_clone = render_page_to_rgba(&build_doc(true), 0, 72.0, ViewMode::Enhanced).unwrap();
    let dark_shadow = img_clone
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] < 200 && px[1] < 200 && px[2] < 200 && px[2].abs_diff(px[0]) < 20)
        .count();
    assert!(
        dark_shadow > 50,
        "Group(DropShadow) containing Clone(ArtisticText) must cast visible shadow pixels, got {dark_shadow}"
    );
    assert_eq!(
        img_clone.rgba, img_direct.rgba,
        "Group(DropShadow) containing Clone(ArtisticText) must cast exact same shadow as direct"
    );
}

#[test]
fn w3_linked_paragraph_frame_dirty_tile_cache_invalidation_matches_direct_render() {
    use coree_core::{CommandHistory, CommandOp, DocumentInvalidation};

    let apply_inv =
        |cache: &mut DirtyTileCache, doc: &Document, inv: &DocumentInvalidation| match inv {
            DocumentInvalidation::All => cache.mark_all_dirty(),
            DocumentInvalidation::Targeted {
                object_ids,
                dirty_rects_mm,
            } => {
                for &r in dirty_rects_mm {
                    cache.mark_dirty(r);
                }
                for &id in object_ids {
                    cache.mark_object_dirty(doc, id);
                }
            }
        };

    // 1. Exact Challenger repro: Frame2 on-screen (5,5), Frame1 off-screen (250,250) -> EditText via CommandHistory
    {
        let mut doc = Document::new("ChallengerRepro", 320.0, 320.0, ColorMode::Rgb, 96.0);
        let style = TextStyle {
            font_size_pt: 14.0,
            fill: Color::rgba8(20, 20, 20, 255),
            ..TextStyle::default()
        };
        let f2_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Frame2",
                    NodePayload::ParagraphText(ParagraphText::new(
                        "",
                        Vec2D::new(55.0, 55.0),
                        style.clone(),
                    )),
                )
                .with_transform(Affine2D::translate(5.0, 5.0)),
                None,
            )
            .unwrap();
        let mut p1 = ParagraphText::new(
            "FIRST WORD SECOND THIRD FOURTH FIFTH SIXTH SEVENTH",
            Vec2D::new(25.0, 12.0),
            style.clone(),
        );
        p1.linked_next_frame = Some(f2_id);
        let f1_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Frame1", NodePayload::ParagraphText(p1))
                    .with_transform(Affine2D::translate(250.0, 250.0)),
                None,
            )
            .unwrap();

        let vp = Viewport::for_export(256, 256, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let initial = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        let initial_ink = initial
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] < 240 || px[1] < 240 || px[2] < 240)
            .count();
        assert!(
            initial_ink > 100,
            "Frame2 must initially display overflow text from Frame1, got {initial_ink}"
        );

        let mut hist = CommandHistory::default();
        let mut p1_short = ParagraphText::new("SHORT", Vec2D::new(25.0, 12.0), style);
        p1_short.linked_next_frame = Some(f2_id);
        hist.execute(
            &mut doc,
            CommandOp::EditText {
                id: f1_id,
                payload: NodePayload::ParagraphText(p1_short),
            },
        )
        .unwrap();

        let inv = hist.last_invalidation().clone();
        match &inv {
            DocumentInvalidation::Targeted { object_ids, .. } => {
                assert!(
                    object_ids.contains(&f1_id) && object_ids.contains(&f2_id),
                    "Targeted invalidation must include both Frame1 and Frame2: {object_ids:?}"
                );
            }
            DocumentInvalidation::All => panic!("Expected Targeted invalidation"),
        }
        apply_inv(&mut cache, &doc, &inv);
        assert!(
            cache.dirty_tile_count() >= 1,
            "dirty_tile_count must be >= 1 after Targeted invalidation"
        );

        let tiled = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        let direct = render_page_to_rgba(&doc, 0, 96.0, ViewMode::Enhanced).unwrap();
        let diff_px = (0..vp.height_px)
            .flat_map(|y| (0..vp.width_px).map(move |x| (x, y)))
            .filter(|&(x, y)| tiled.get_pixel(x, y).unwrap() != direct.pixel(x, y))
            .count();
        assert_eq!(
            diff_px, 0,
            "Edited linked Frame1 must leave 0 stale diff pixels in Frame2 vs direct render"
        );
    }

    // 2. 3-frame chain Frame1 (offscreen) -> Frame2 (tile 0,0) -> Frame3 (tile 1,0):
    //    EditText, Unlink, DeleteObjects, and Undo/Redo all maintain 0 stale diff pixels.
    {
        let mut doc = Document::new("Chain3Frame", 320.0, 320.0, ColorMode::Rgb, 96.0);
        let style = TextStyle {
            font_size_pt: 14.0,
            fill: Color::rgba8(10, 40, 160, 255),
            ..TextStyle::default()
        };
        let f3_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "F3",
                    NodePayload::ParagraphText(ParagraphText::new(
                        "",
                        Vec2D::new(50.0, 50.0),
                        style.clone(),
                    )),
                )
                .with_transform(Affine2D::translate(75.0, 5.0)),
                None,
            )
            .unwrap();
        let mut p2 = ParagraphText::new("", Vec2D::new(50.0, 50.0), style.clone());
        p2.linked_next_frame = Some(f3_id);
        let f2_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "F2", NodePayload::ParagraphText(p2))
                    .with_transform(Affine2D::translate(5.0, 5.0)),
                None,
            )
            .unwrap();
        let mut p1 = ParagraphText::new(
            "ONE TWO THREE FOUR FIVE SIX SEVEN EIGHT NINE TEN ELEVEN TWELVE THIRTEEN FOURTEEN FIFTEEN SIXTEEN SEVENTEEN EIGHTEEN",
            Vec2D::new(25.0, 12.0),
            style.clone(),
        );
        p1.linked_next_frame = Some(f2_id);
        let f1_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "F1", NodePayload::ParagraphText(p1))
                    .with_transform(Affine2D::translate(250.0, 250.0)),
                None,
            )
            .unwrap();

        let vp = Viewport::for_export(512, 256, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let _ = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);

        let assert_zero_diff = |cache: &mut DirtyTileCache,
                                renderer: &mut SceneRenderer,
                                doc: &Document,
                                label: &str| {
            let tiled = cache.render_viewport(doc, &vp, ViewMode::Enhanced, &opts, renderer);
            let mut direct_backend = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
            renderer.render_document(doc, &vp, ViewMode::Enhanced, &opts, &mut direct_backend);
            let direct = direct_backend.into_rgba();
            assert_eq!(
                count_rgba_diff(&direct, &tiled),
                0,
                "[{label}] 3-frame linked invalidation must yield 0 diff pixels"
            );
        };

        let mut hist = CommandHistory::default();

        // Unlink F1 -> F2
        let mut p1_unlinked = ParagraphText::new("ONLY F1", Vec2D::new(25.0, 12.0), style);
        p1_unlinked.linked_next_frame = None;
        hist.execute(
            &mut doc,
            CommandOp::EditText {
                id: f1_id,
                payload: NodePayload::ParagraphText(p1_unlinked),
            },
        )
        .unwrap();
        apply_inv(&mut cache, &doc, hist.last_invalidation());
        assert_zero_diff(&mut cache, &mut renderer, &doc, "unlink execute");

        // Undo unlink
        hist.undo(&mut doc).unwrap();
        apply_inv(&mut cache, &doc, hist.last_invalidation());
        assert_zero_diff(&mut cache, &mut renderer, &doc, "unlink undo");

        // Delete F1
        hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![f1_id] })
            .unwrap();
        apply_inv(&mut cache, &doc, hist.last_invalidation());
        assert_zero_diff(&mut cache, &mut renderer, &doc, "delete execute");

        // Undo delete
        hist.undo(&mut doc).unwrap();
        apply_inv(&mut cache, &doc, hist.last_invalidation());
        assert_zero_diff(&mut cache, &mut renderer, &doc, "delete undo");
    }
}

#[test]
fn w5a_snap_five_geometric_modes_and_extension_guides() {
    let mut doc = Document::new("W5ASnaps", 200.0, 200.0, ColorMode::Rgb, 96.0);
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Circle",
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 20.0,
                ry: 20.0,
                start_deg: 0.0,
                end_deg: 360.0,
                mode: coree_core::EllipseMode::Full,
            }),
        )
        .with_transform(Affine2D::translate(50.0, 50.0)),
        None,
    )
    .unwrap();
    let l1 = BezierPath::from_line(Point2D::new(100.0, 20.0), Point2D::new(140.0, 60.0));
    let l2 = BezierPath::from_line(Point2D::new(100.0, 60.0), Point2D::new(140.0, 20.0));
    doc.add_object(
        SceneObject::new(ObjectId(0), "L1", NodePayload::Path(l1)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(ObjectId(0), "L2", NodePayload::Path(l2)),
        None,
    )
    .unwrap();
    let at = ArtisticText::from_plain("ABC", TextStyle::default());
    doc.add_object(
        SceneObject::new(ObjectId(0), "Txt", NodePayload::ArtisticText(at))
            .with_transform(Affine2D::translate(20.0, 120.0)),
        None,
    )
    .unwrap();

    let opts = SnapOptions {
        grid: false,
        page_edges: false,
        guidelines: false,
        objects: false,
        nodes: false,
        dynamic_guides: true,
        tangent: true,
        perpendicular: true,
        quadrant: true,
        intersection: true,
        baseline: true,
        tolerance_mm: 3.0,
    };

    let (quad_pt, _) =
        SnapEngine::snap_point_with_reference(&doc, 0, Point2D::new(70.5, 50.3), None, &[], &opts);
    assert!(quad_pt.distance_to(Point2D::new(70.0, 50.0)) < 0.01);

    let (tan_pt, _) = SnapEngine::snap_point_with_reference(
        &doc,
        0,
        Point2D::new(50.2, 69.8),
        Some(Point2D::new(50.0, 100.0)),
        &[],
        &opts,
    );
    assert!(tan_pt.distance_to(Point2D::new(50.0, 70.0)) < 0.01);

    let (inter_pt, _) =
        SnapEngine::snap_point_with_reference(&doc, 0, Point2D::new(120.4, 40.3), None, &[], &opts);
    assert!(inter_pt.distance_to(Point2D::new(120.0, 40.0)) < 0.01);

    let expected_base_y = 120.0 + TextStyle::default().font_size_mm() * 0.85;
    let (base_pt, base_guides) = SnapEngine::snap_point_with_reference(
        &doc,
        0,
        Point2D::new(25.0, expected_base_y + 0.3),
        None,
        &[],
        &opts,
    );
    assert!((base_pt.y - expected_base_y).abs() < 0.01);
    assert!(base_guides.iter().any(|g| matches!(
        g.kind,
        DynamicGuideKind::Baseline | DynamicGuideKind::AlignmentY
    )));
}

#[test]
fn w5a_drawing_scale_dimension_render_and_tile_cache_invalidation() {
    let mut doc = Document::new("W5ADrawScale", 120.0, 80.0, ColorMode::Rgb, 96.0);
    let dim = DimensionObject::linear(
        DimensionKind::Horizontal,
        Point2D::new(20.0, 40.0),
        Point2D::new(50.0, 40.0),
        10.0,
    );
    doc.add_object(
        SceneObject::new(ObjectId(0), "Dim", NodePayload::Dimension(dim))
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.4)),
        None,
    )
    .unwrap();

    let vp = Viewport::for_export(400, 260, 96.0);
    let opts = RenderOptions::export();
    let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
    let mut renderer = SceneRenderer::new();

    let img_1_1 = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetDocumentProperties {
            title: None,
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
            active_page_index: None,
            metadata: None,
            custom_icc_profile: None,
            clear_custom_icc: false,
            rgb_icc_profile: None,
            clear_rgb_icc: false,
            cmyk_icc_profile: None,
            clear_cmyk_icc: false,
            gray_icc_profile: None,
            clear_gray_icc: false,
            rendering_intent: None,
            black_point_compensation: None,
            drawing_scale: Some(coree_core::DrawingScale::new(1.0, 100.0)),
        },
    )
    .unwrap();
    let img_1_100 = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    assert!(
        count_rgba_diff(&img_1_1, &img_1_100) > 20,
        "1:100 drawing_scale must re-render dimension label and invalidate tile cache"
    );
}

#[test]
fn w5a_facing_pages_spread_layout_and_autopage_number() {
    let mut doc = Document::new("W5AFacing", 100.0, 140.0, ColorMode::Rgb, 96.0);
    doc.add_page("P2", 100.0, 140.0, 0.0);
    doc.add_page("P3", 100.0, 140.0, 0.0);

    let non_facing = compute_page_layouts(&doc, true, 20.0);
    assert!((non_facing[1].origin_mm.y - non_facing[2].origin_mm.y).abs() < 1e-6);
    assert!((non_facing[2].origin_mm.x - non_facing[1].origin_mm.x - 120.0).abs() < 1e-6);

    let p0_id = doc.page(0).unwrap().id;
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetPageProperties {
            page_id: p0_id,
            name: None,
            width_mm: None,
            height_mm: None,
            bleed_mm: None,
            facing_pages: Some(true),
            background_fill: None,
            printable_background: None,
        },
    )
    .unwrap();
    let facing = compute_page_layouts(&doc, true, 20.0);
    assert!(
        (facing[2].origin_mm.x - (facing[1].origin_mm.x + facing[1].width_mm)).abs() < 1e-6,
        "facing pages P2 and P3 must touch at zero-gap spine"
    );
    assert!(
        facing[1].origin_mm.y > facing[0].origin_mm.y + 140.0,
        "spread 1 must stack below cover recto page"
    );

    let mut at = ArtisticText::default();
    at.runs.push(
        coree_core::TextRun::new(
            "0",
            TextStyle {
                font_size_pt: 24.0,
                fill: Color::BLACK,
                ..TextStyle::default()
            },
        )
        .with_field(coree_core::TextField::AutoPageNumber),
    );
    let master_id = doc
        .add_layer(
            p0_id,
            "Folio",
            false,
            coree_core::MasterLayerScope::AllPages,
        )
        .unwrap();
    doc.add_object(
        SceneObject::new(ObjectId(0), "PageNum", NodePayload::ArtisticText(at))
            .with_transform(Affine2D::translate(30.0, 40.0)),
        Some(master_id),
    )
    .unwrap();

    let p0_img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    let p1_img = render_page_to_rgba(&doc, 1, 72.0, ViewMode::Enhanced).unwrap();
    let page_num_diff = p0_img
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(p1_img.rgba.as_chunks::<4>().0.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        page_num_diff > 15,
        "AutoPageNumber on master layer must render '1' on page 0 vs '2' on page 1 (diff={page_num_diff})"
    );
}

#[test]
fn w5a_simple_wireframe_page_bg_and_carryover_fixes() {
    // 1. SimpleWireframe hides Blend/Contour/Extrude/Raster-X vs Wireframe & Enhanced
    let mut doc = Document::new("W5ASimpleWF", 100.0, 100.0, ColorMode::Rgb, 96.0);
    let mut r1 = SceneObject::new(
        ObjectId(0),
        "Src",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgb(0.0, 0.0, 1.0)))
    .with_transform(Affine2D::translate(10.0, 10.0));
    let r2 = SceneObject::new(
        ObjectId(0),
        "Dst",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgb(1.0, 0.0, 0.0)))
    .with_transform(Affine2D::translate(60.0, 60.0));
    let r2_id = doc.add_object(r2, None).unwrap();
    r1.effects.push(Effect::Blend {
        target_id: r2_id,
        steps: 5,
        rotation_deg: 0.0,
        along_path: None,
        rotate_along_path: false,
    });
    doc.add_object(r1, None).unwrap();

    let simple = render_page_to_rgba(&doc, 0, 72.0, ViewMode::SimpleWireframe).unwrap();
    let wf = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Wireframe).unwrap();
    let enh = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    let simple_ink = count_non_white_pixels(&simple.rgba);
    let wf_ink = count_non_white_pixels(&wf.rgba);
    let enh_ink = count_non_white_pixels(&enh.rgba);
    assert!(simple_ink > 20 && simple_ink < wf_ink && wf_ink < enh_ink);

    // 2. Page background_fill + printable_background
    let p0_id = doc.page(0).unwrap().id;
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetPageProperties {
            page_id: p0_id,
            name: None,
            width_mm: None,
            height_mm: None,
            bleed_mm: None,
            facing_pages: None,
            background_fill: Some(Fill::Solid(Color::rgb(0.2, 0.8, 0.3))),
            printable_background: Some(false),
        },
    )
    .unwrap();
    let exp_no_bg = render_page_to_rgba_with_options(
        &doc,
        0,
        72.0,
        ViewMode::Enhanced,
        &RenderOptions::export(),
    )
    .unwrap();
    assert_eq!(pixel_at_mm(&exp_no_bg, 2.0, 2.0), [255, 255, 255, 255]);
    coree_core::apply_command(
        &mut doc,
        &coree_core::CommandOp::SetPageProperties {
            page_id: p0_id,
            name: None,
            width_mm: None,
            height_mm: None,
            bleed_mm: None,
            facing_pages: None,
            background_fill: None,
            printable_background: Some(true),
        },
    )
    .unwrap();
    let exp_with_bg = render_page_to_rgba_with_options(
        &doc,
        0,
        72.0,
        ViewMode::Enhanced,
        &RenderOptions::export(),
    )
    .unwrap();
    assert!(pixel_at_mm(&exp_with_bg, 2.0, 2.0)[1] > 180);

    // 3. #19a RTL DropCap + ContourLeft & #19b cyclic linked_next_frame determinism
    let mut rtl_doc = Document::new("RtlDrop", 120.0, 120.0, ColorMode::Rgb, 96.0);
    rtl_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Obs",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 25.0,
                    height: 60.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(75.0, 10.0)),
            None,
        )
        .unwrap();
    let mut para = ParagraphText::new(
        "مرحبا بكم في اختبار النص العربي مع الحرف الاستهلالي والالتفاف",
        Vec2D::new(90.0, 60.0),
        TextStyle {
            font_size_pt: 12.0,
            ..TextStyle::default()
        },
    );
    para.wrap_mode = coree_core::TextWrapMode::ContourLeft;
    para.drop_cap = Some(coree_core::DropCapConfig {
        enabled: true,
        lines: 2,
        gap_mm: 1.5,
    });
    let pid = rtl_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "RtlPara",
                NodePayload::ParagraphText(para.clone()),
            )
            .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    let mut te = coree_render::TextEngine::new();
    let pieces = te.layout_paragraph_in_doc(&rtl_doc, pid, &para);
    assert!(
        pieces.len() > 2,
        "RTL paragraph with DropCap must lay out glyphs"
    );
    for p in pieces.iter().skip(1) {
        if let Some(bb) = p.path.bounding_box() {
            assert!(
                bb.max_x <= 65.0 && bb.min_x >= 0.0,
                "RTL body text after DropCap must wrap left of ContourLeft obstacle [65..90]: {bb:?}"
            );
        }
    }

    // #19b cyclic linked_next_frame determinism
    let mut cyc = Document::new("Cycle", 100.0, 100.0, ColorMode::Rgb, 96.0);
    let mut f1 = ParagraphText::new(
        "ALPHA BETA GAMMA DELTA",
        Vec2D::new(40.0, 40.0),
        TextStyle::default(),
    );
    let mut f2 = ParagraphText::new("", Vec2D::new(40.0, 40.0), TextStyle::default());
    f1.linked_next_frame = Some(ObjectId(2));
    f2.linked_next_frame = Some(ObjectId(1));
    let id1 = cyc
        .add_object(
            SceneObject::new(ObjectId(0), "F1", NodePayload::ParagraphText(f1.clone())),
            None,
        )
        .unwrap();
    let id2 = cyc
        .add_object(
            SceneObject::new(ObjectId(0), "F2", NodePayload::ParagraphText(f2)),
            None,
        )
        .unwrap();
    let c1 = te.layout_paragraph_in_doc(&cyc, id1, &f1);
    let c2 = te.layout_paragraph_in_doc(&cyc, id1, &f1);
    assert_eq!(c1.len(), c2.len());
    assert_ne!(id1, id2);

    // 4. #20d tilted stylus smudge_stroke_stylus_masked covers tilted ellipse
    let mut asset =
        RasterAsset::new(32, 32, vec![255u8; 32 * 32 * 4], 96.0, ColorMode::Rgb).unwrap();
    for x in 0..10u32 {
        for y in 0..32u32 {
            asset.write_pixel_f32(x, y, [1.0, 0.0, 0.0, 1.0]);
        }
    }
    let pts = vec![
        coree_render::StylusPoint {
            pos: Point2D::new(4.0, 16.0),
            pressure: 1.0,
            tilt_x_deg: 75.0,
            tilt_y_deg: 0.0,
            twist_deg: 0.0,
        },
        coree_render::StylusPoint {
            pos: Point2D::new(14.0, 16.0),
            pressure: 1.0,
            tilt_x_deg: 75.0,
            tilt_y_deg: 0.0,
            twist_deg: 0.0,
        },
    ];
    coree_render::smudge_stroke_stylus_masked(&mut asset, &pts, 4.0, 0.9, None);
    let px_far = asset.read_pixel_f32(19, 16).unwrap();
    assert!(
        px_far[1] < 0.99 || px_far[2] < 0.99,
        "tilted smudge ellipse must reach beyond untilted radius=4: {px_far:?}"
    );
}

#[test]
fn autopagenumber_expanded_width_not_culled_by_placeholder_bbox() {
    use coree_core::{ArtisticText, MasterLayerScope, TextField, TextStyle};

    let mut doc = Document::new("AutoPageNumberCull", 100.0, 50.0, ColorMode::Rgb, 72.0);
    let p0_id = doc.pages()[0].id;
    let master_lid = doc
        .add_layer(p0_id, "MasterNum", false, MasterLayerScope::AllPages)
        .unwrap();
    for p in 2..=100 {
        doc.add_page(format!("Page {p}"), 100.0, 50.0, 0.0);
    }
    let style = TextStyle {
        font_size_pt: 24.0,
        fill: Color::BLACK,
        ..TextStyle::default()
    };
    let mut at = ArtisticText::from_plain("1", style);
    at.runs[0].field = Some(TextField::AutoPageNumber);
    doc.add_object(
        SceneObject::new(ObjectId(0), "PageNum", NodePayload::ArtisticText(at))
            .with_transform(Affine2D::translate(10.0, 10.0))
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::none()),
        Some(master_lid),
    )
    .unwrap();

    // Render page 0 ("1") and page 99 ("100") at 72 DPI; "100" is wider than placeholder "1"
    // and must not be clipped by SpatialIndex placeholder tile culling.
    let img_1 = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_100 = render_page_to_rgba(&doc, 99, 72.0, ViewMode::Enhanced).unwrap();
    let px_1 = count_non_white_pixels(&img_1.rgba);
    let px_100 = count_non_white_pixels(&img_100.rgba);
    assert!(
        px_1 > 50,
        "Page 1 AutoPageNumber rendered too few pixels: {px_1}"
    );
    assert!(
        px_100 > px_1 + 80,
        "Page 100 AutoPageNumber ('100') must render full 3-digit width without culling: px_1={px_1}, px_100={px_100}"
    );
}

#[test]
fn extreme_scale_bezier_curve_1e15_renders_and_flattens_under_budget() {
    let mut kb = kurbo::BezPath::new();
    kb.move_to((0.0, 0.0));
    kb.curve_to((0.0, 10.0), (10.0, 10.0), (10.0, 0.0));
    kb.close_path();
    let curve = BezierPath::from_kurbo(&kb);

    let t0 = std::time::Instant::now();
    let mut backend = TinySkiaBackend::new(256, 256).unwrap();
    backend.clear(Color::WHITE);
    let paint = BackendPaint {
        shader: coree_render::PaintShader::Solid(Color::BLACK),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        anti_alias: true,
    };
    // 1. Disjoint 1e15 transform (like extreme_xf.coree)
    backend.fill_path(
        &curve,
        &paint,
        FillRule::Winding,
        Affine2D::new(1e15, 0.0, 0.0, 1e15, 1e15, -1e15),
    );
    // 2. Viewport-intersecting 1e14 transform
    backend.fill_path(
        &curve,
        &paint,
        FillRule::Winding,
        Affine2D::new(1e14, 0.0, 0.0, 1e14, -1e14, 0.0),
    );
    backend.push_clip(
        &curve,
        FillRule::EvenOdd,
        Affine2D::new(1e15, 0.0, 0.0, 1e15, 1e15, -1e15),
    );
    backend.pop_clip();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "extreme 1e15 fill_path/push_clip took {elapsed:?}"
    );
    let buf = backend.into_rgba();
    assert_eq!(buf.rgba.len(), 256 * 256 * 4);
}

#[test]
fn symbol_powerclip_blend_under_lens_backdrop_renders_all_blend_steps() {
    let mut doc = Document::new("SymPCBlendLens", 120.0, 120.0, ColorMode::Rgb, 72.0);
    let src_id = ObjectId(301);
    let dst_id = ObjectId(302);
    let mut s_obj = SceneObject::new(
        src_id,
        "BlendStart",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 20.0, 12.0, 12.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(240, 20, 20, 255)))
    .with_outline(OutlineStyle::none());
    s_obj.effects.push(Effect::Blend {
        target_id: dst_id,
        steps: 5,
        rotation_deg: 0.0,
        along_path: None,
        rotate_along_path: false,
    });
    let d_obj = SceneObject::new(
        dst_id,
        "BlendEnd",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            70.0, 20.0, 12.0, 12.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(20, 40, 240, 255)))
    .with_outline(OutlineStyle::none());
    let pc_obj = SceneObject::new(
        ObjectId(303),
        "SymPC",
        NodePayload::PowerClip {
            container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 100.0, 60.0,
            ))),
            container_parametric: None,
            lock_contents: true,
            contents: vec![src_id, dst_id],
        },
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    // Also a PowerClip whose container itself carries the Blend inside a SymbolDef
    let sym = SymbolDef::new("PCSym", vec![s_obj, d_obj, pc_obj]);
    let sym_id = sym.id;
    let mut hist = coree_core::CommandHistory::new(16);
    hist.execute(
        &mut doc,
        coree_core::CommandOp::UpsertSymbol { symbol: sym },
    )
    .unwrap();

    let l0 = doc.pages()[0].layers[0].id;
    doc.add_object(
        SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
            .with_transform(Affine2D::translate(5.0, 10.0)),
        Some(l0),
    )
    .unwrap();

    // Cover entire page with a Brighten lens so backdrop renderer renders PowerClip inside Symbol!
    let mut lens_obj = SceneObject::new(
        ObjectId(0),
        "BrightenLens",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 120.0, 120.0,
        ))),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    lens_obj.lens = Some(LensConfig::new(LensKind::Brighten { rate_pct: 5.0 }));
    doc.add_object(lens_obj, Some(l0)).unwrap();

    let img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    // Middle of the blend at x = 45..55 mm, y = 36 mm (10+20+6=36) must be non-white!
    let mid_px = pixel_at_mm(&img, 51.0, 36.0);
    assert!(
        mid_px[0] < 245 || mid_px[1] < 245 || mid_px[2] < 245,
        "Blend step inside Symbol->PowerClip under Lens backdrop must render, got {mid_px:?}"
    );
}

#[test]
fn deformed_symbol_instance_blend_warps_intermediate_steps() {
    let mut doc = Document::new("DeformedSymBlend", 140.0, 140.0, ColorMode::Rgb, 72.0);
    let src_id = ObjectId(401);
    let dst_id = ObjectId(402);
    let mut s_obj = SceneObject::new(
        src_id,
        "S",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 10.0, 10.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(200, 30, 30, 255)))
    .with_outline(OutlineStyle::none());
    s_obj.effects.push(Effect::Blend {
        target_id: dst_id,
        steps: 3,
        rotation_deg: 0.0,
        along_path: None,
        rotate_along_path: false,
    });
    let d_obj = SceneObject::new(
        dst_id,
        "D",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            50.0, 10.0, 10.0, 10.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(30, 30, 220, 255)))
    .with_outline(OutlineStyle::none());

    let sym = SymbolDef::new("BlendSym", vec![s_obj, d_obj]);
    let sym_id = sym.id;
    let mut hist = coree_core::CommandHistory::new(16);
    hist.execute(
        &mut doc,
        coree_core::CommandOp::UpsertSymbol { symbol: sym },
    )
    .unwrap();

    // Shift destination downward heavily via Envelope on the SymbolInstance so blend steps shift down!
    let mut inst = SceneObject::new(
        ObjectId(0),
        "WarpedInst",
        NodePayload::SymbolInstance(sym_id),
    );
    inst.effects.push(Effect::Envelope {
        mode: EnvelopeMode::Unconstrained,
        cage: vec![
            Point2D::new(10.0, 60.0),
            Point2D::new(60.0, 60.0),
            Point2D::new(60.0, 80.0),
            Point2D::new(10.0, 80.0),
        ],
    });
    let l0 = doc.pages()[0].layers[0].id;
    doc.add_object(inst, Some(l0)).unwrap();

    let img = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    // Original undeformed y = 15 mm must be white; deformed y = 70 mm at x = 35 mm must be painted!
    let old_mid = pixel_at_mm(&img, 35.0, 15.0);
    let warped_mid = pixel_at_mm(&img, 35.0, 70.0);
    assert_eq!(
        old_mid,
        [255, 255, 255, 255],
        "undeformed y=15 must stay white"
    );
    assert!(
        warped_mid[0] < 245 || warped_mid[1] < 245 || warped_mid[2] < 245,
        "deformed SymbolInstance blend midpoint at y=70 must be painted, got {warped_mid:?}"
    );
}

#[test]
fn extreme_numeric_zoom256_zero_font_huge_steps_and_singular_transparency_mask() {
    let mut doc = Document::new("ExtremeNumeric", 100.0, 100.0, ColorMode::Rgb, 72.0);
    let l0 = doc.pages()[0].layers[0].id;

    // 1. InnerShadow blur_mm = 1e6 + degenerate FountainLinear (start == end) + singular transform
    let mut sh = make_rect(10.0, 10.0, 20.0, 20.0, Color::rgba8(240, 20, 20, 255));
    sh.effects.push(Effect::InnerShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 1e6,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    sh.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::new(5.0, 5.0),
            end: Point2D::new(5.0, 5.0),
            start_alpha: 0.2,
            end_alpha: 0.9,
        },
        blend_mode: BlendMode::Multiply,
    });
    doc.add_object(sh, Some(l0)).unwrap();

    let mut sing = make_rect(0.0, 0.0, 10.0, 10.0, Color::rgba8(20, 40, 240, 255));
    sing.transform = Affine2D::new(0.0, 0.0, 0.0, 0.0, 30.0, 30.0);
    sing.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::ZERO,
            end: Point2D::ZERO,
            start_alpha: 0.5,
            end_alpha: 0.5,
        },
        blend_mode: BlendMode::Normal,
    });
    doc.add_object(sing, Some(l0)).unwrap();

    // 2. 0.0pt RTL self-cyclic ParagraphText
    let rtl_style = TextStyle {
        font_size_pt: 0.0,
        fill: Color::BLACK,
        ..TextStyle::default()
    };
    let mut pt_cyc = ParagraphText::new("مرحبا", Vec2D::new(20.0, 20.0), rtl_style);
    pt_cyc.direction = TextDirection::Rtl;
    let p_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CyclicZeroPtRTL",
                NodePayload::ParagraphText(pt_cyc.clone()),
            )
            .with_transform(Affine2D::translate(10.0, 50.0)),
            Some(l0),
        )
        .unwrap();
    pt_cyc.linked_next_frame = Some(p_id);
    let mut hist = coree_core::CommandHistory::new(4);
    hist.execute(
        &mut doc,
        coree_core::CommandOp::EditText {
            id: p_id,
            payload: NodePayload::ParagraphText(pt_cyc),
        },
    )
    .unwrap();

    let mut vp = Viewport::for_export(256, 256, 96.0);
    vp.zoom = 256.0;
    vp.pan_mm = Point2D::new(20.0, 20.0);
    let mut renderer = SceneRenderer::new();
    let mut backend = TinySkiaBackend::new(256, 256).unwrap();
    let t0 = std::time::Instant::now();
    renderer.render_document(
        &doc,
        &vp,
        ViewMode::Enhanced,
        &RenderOptions::export(),
        &mut backend,
    );
    let elapsed = t0.elapsed();
    let max_ms = if cfg!(debug_assertions) { 1500 } else { 500 };
    assert!(
        elapsed < std::time::Duration::from_millis(max_ms),
        "extreme numeric stress took too long: {elapsed:?}"
    );
    let buf = backend.into_rgba();
    assert_eq!(buf.rgba.len(), 256 * 256 * 4);
}

#[test]
fn extreme_1e15_stacked_lenses_and_export_session_stacked_lens_parity() {
    use coree_core::{ColorMapMode, EllipseMode, LensConfig, LensKind, ParametricShape};
    use coree_render::{
        render_isolated_object_to_rgba_with_options, render_page_to_rgba, ExportIsolationSession,
    };

    // 1. Extreme 1e15-transformed stacked lenses (incl. frozen + remove_face + FishEye + Magnify)
    // must complete well under 1s in both render_page_to_rgba and ExportIsolationSession.
    let mut doc_1e15 = Document::new("Render1e15Lens", 210.0, 297.0, ColorMode::Rgb, 72.0);
    let l0 = doc_1e15.pages()[0].layers[0].id;
    let xf_1e15 = Affine2D::new(1e15, 0.0, 0.0, 1e15, 1e15, 1e15);
    doc_1e15
        .add_object(
            make_rect(0.0, 0.0, 50.0, 50.0, Color::rgba8(220, 40, 40, 255)).with_transform(xf_1e15),
            Some(l0),
        )
        .unwrap();
    let lens_cfgs = [
        LensConfig::new(LensKind::Brighten { rate_pct: 35.0 }),
        LensConfig {
            kind: LensKind::ColorAdd {
                color: Color::rgb(0.2, 0.5, 0.8),
                rate_pct: 45.0,
            },
            frozen: true,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        },
        LensConfig {
            kind: LensKind::CustomColorMap {
                from_color: Color::rgb(0.1, 0.0, 0.4),
                to_color: Color::rgb(1.0, 0.9, 0.2),
                palette_mode: ColorMapMode::ForwardRainbow,
            },
            frozen: false,
            viewpoint: None,
            remove_face: true,
            frozen_snapshot: None,
        },
        LensConfig::new(LensKind::FishEye { rate_pct: 120.0 }),
        LensConfig {
            kind: LensKind::Magnify { amount: 2.5 },
            frozen: true,
            viewpoint: Some(Point2D::new(80.0, 90.0)),
            remove_face: true,
            frozen_snapshot: None,
        },
        LensConfig::new(LensKind::Invert),
    ];
    let mut lens_ids = Vec::new();
    for (idx, cfg) in lens_cfgs.into_iter().enumerate() {
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("Lens{}", idx + 1),
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 6.0,
                ry: 6.0,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
        )
        .with_transform(xf_1e15);
        obj.lens = Some(cfg);
        lens_ids.push(doc_1e15.add_object(obj, Some(l0)).unwrap());
    }

    let t0 = std::time::Instant::now();
    let rendered = render_page_to_rgba(&doc_1e15, 0, 96.0, ViewMode::Enhanced).unwrap();
    assert!(!rendered.rgba.is_empty());
    let mut session = ExportIsolationSession::new();
    for &oid in &lens_ids {
        let res = session
            .render_isolated_object_with_options(
                &doc_1e15,
                0,
                oid,
                Affine2D::IDENTITY,
                300.0,
                false,
            )
            .unwrap();
        assert!(
            res.is_none(),
            "1e15 off-scale Lens must skip isolated 2048px rasterization"
        );
    }
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "1e15 stacked-lens render + isolation took {:?}",
        t0.elapsed()
    );

    // 2. Normal in-page stacked Lenses: ExportIsolationSession (cached O(N)) must be pixel-identical
    // to uncached render_isolated_object_to_rgba_with_options across stacked overlapping lenses.
    let mut norm_doc = Document::new("StackedParity", 100.0, 100.0, ColorMode::Rgb, 72.0);
    let nl0 = norm_doc.pages()[0].layers[0].id;
    norm_doc
        .add_object(
            make_rect(10.0, 10.0, 80.0, 80.0, Color::rgba8(240, 20, 40, 255)),
            Some(nl0),
        )
        .unwrap();
    let mut l1 = make_rect(20.0, 20.0, 60.0, 60.0, Color::TRANSPARENT);
    l1.fill = Fill::None;
    l1.lens = Some(LensConfig::new(LensKind::Brighten { rate_pct: 40.0 }));
    let l1_id = norm_doc.add_object(l1, Some(nl0)).unwrap();

    let mut l2 = make_rect(25.0, 25.0, 50.0, 50.0, Color::TRANSPARENT);
    l2.fill = Fill::None;
    l2.lens = Some(LensConfig {
        kind: LensKind::ColorAdd {
            color: Color::rgb(0.0, 0.5, 0.8),
            rate_pct: 50.0,
        },
        frozen: true,
        viewpoint: None,
        remove_face: false,
        frozen_snapshot: None,
    });
    let l2_id = norm_doc.add_object(l2, Some(nl0)).unwrap();

    let mut l3 = make_rect(30.0, 30.0, 40.0, 40.0, Color::TRANSPARENT);
    l3.fill = Fill::None;
    l3.lens = Some(LensConfig::new(LensKind::Invert));
    let l3_id = norm_doc.add_object(l3, Some(nl0)).unwrap();

    let mut norm_session = ExportIsolationSession::new();
    for oid in [l1_id, l2_id, l3_id] {
        let (r_sess, b_sess) = norm_session
            .render_isolated_object_with_options(&norm_doc, 0, oid, Affine2D::IDENTITY, 150.0, true)
            .unwrap()
            .expect("normal lens rendered");
        let (r_uncached, b_uncached) = render_isolated_object_to_rgba_with_options(
            &norm_doc,
            0,
            oid,
            Affine2D::IDENTITY,
            150.0,
            true,
        )
        .unwrap()
        .expect("uncached lens rendered");
        assert_eq!(r_sess, r_uncached);
        assert_eq!(b_sess.width, b_uncached.width);
        assert_eq!(b_sess.height, b_uncached.height);
        assert_eq!(
            b_sess.rgba, b_uncached.rgba,
            "ExportIsolationSession must match uncached render_isolated_object_to_rgba_with_options for lens {oid:?}"
        );
    }
}

#[test]
fn extreme_scale_1e15_intersecting_stroke_and_clip_tinyskia_and_wgpu() {
    use coree_core::{LineCap, LineJoin};
    use coree_render::{PaintShader, WgpuBackend};

    let mut kb_closed = kurbo::BezPath::new();
    kb_closed.move_to((0.0, 0.0));
    kb_closed.curve_to((10.0, 20.0), (40.0, 30.0), (50.0, 50.0));
    kb_closed.close_path();
    let closed_curve = BezierPath::from_kurbo(&kb_closed);

    let mut kb_open = kurbo::BezPath::new();
    kb_open.move_to((0.0, 0.0));
    kb_open.curve_to((10.0, 20.0), (40.0, 30.0), (50.0, 50.0));
    let open_curve = BezierPath::from_kurbo(&kb_open);

    let dpi_xf = Affine2D::scale_uniform(150.0 / 25.4);
    let xf_origin = Affine2D::new(1e15, 0.0, 0.0, 1e15, 0.0, 0.0);
    let xf_straddle = Affine2D::new(1e15, 0.0, 0.0, 1e15, -2.5e16, -2.5e16);
    let xf_cancel = Affine2D::translate(-1e15, 0.0);
    let pre_closed = closed_curve.transformed(&xf_origin);
    let pre_open = open_curve.transformed(&xf_origin);
    let pre_offset = closed_curve.transformed(&Affine2D::translate(1e15, 0.0));

    let t0 = std::time::Instant::now();
    for use_wgpu in [false, true] {
        let mut backend: Box<dyn CanvasBackend> = if use_wgpu {
            Box::new(WgpuBackend::new(128, 128).unwrap())
        } else {
            Box::new(TinySkiaBackend::new(128, 128).unwrap())
        };
        backend.clear(Color::WHITE);

        let stroke_cases = [
            // Thick AA
            StrokeParams {
                color: Color::BLACK,
                width: 2.0,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                dash_pattern: vec![],
                dash_offset: 0.0,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                anti_alias: true,
            },
            // Thin AA (effective_px <= 1.05 in pre-transformed regime)
            StrokeParams {
                color: Color::BLACK,
                width: 0.1,
                cap: LineCap::Round,
                join: LineJoin::Round,
                miter_limit: 4.0,
                dash_pattern: vec![],
                dash_offset: 0.0,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                anti_alias: true,
            },
            // Non-AA
            StrokeParams {
                color: Color::BLACK,
                width: 2.0,
                cap: LineCap::Square,
                join: LineJoin::Bevel,
                miter_limit: 4.0,
                dash_pattern: vec![],
                dash_offset: 0.0,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                anti_alias: false,
            },
            // Dashed AA
            StrokeParams {
                color: Color::BLACK,
                width: 2.0,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                dash_pattern: vec![4.0, 2.0],
                dash_offset: 1.0,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                anti_alias: true,
            },
            // Dashed Non-AA
            StrokeParams {
                color: Color::BLACK,
                width: 2.0,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                dash_pattern: vec![4.0, 2.0],
                dash_offset: 1.0,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                anti_alias: false,
            },
        ];

        for st in &stroke_cases {
            // (a) Local path + intersecting 1e15 transforms
            backend.stroke_path(&closed_curve, st, xf_origin);
            backend.stroke_path(&open_curve, st, xf_origin);
            backend.stroke_path(&closed_curve, st, xf_straddle);
            // (b) Pre-transformed 1e15 path + normal DPI transform (scale_with_object = false)
            backend.stroke_path(&pre_closed, st, dpi_xf);
            backend.stroke_path(&pre_open, st, dpi_xf);
            // (c) 1e15 local path + translate(-1e15, 0) cancellation guard
            backend.stroke_path(&pre_offset, st, xf_cancel);
        }

        // Active 1e15 intersecting push_clip wrapping stroke_path
        backend.push_clip(&closed_curve, FillRule::EvenOdd, xf_origin);
        backend.stroke_path(&pre_closed, &stroke_cases[0], dpi_xf);
        backend.pop_clip();

        // draw_pixmap with 1e15 transform
        let img = RgbaBuffer::new_filled(4, 4, [20, 40, 220, 255]);
        backend.draw_pixmap(&img, xf_origin, 0.8, BlendMode::Normal, true);

        // LinearGradient at 1e20 scale (must not drop or panic)
        let grad_paint = BackendPaint {
            shader: PaintShader::LinearGradient {
                start: Point2D::new(0.0, 0.0),
                end: Point2D::new(50.0, 50.0),
                stops: vec![
                    GradientStop::new(0.0, Color::rgba8(200, 10, 10, 255)),
                    GradientStop::new(1.0, Color::rgba8(10, 10, 200, 255)),
                ],
            },
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: true,
        };
        backend.fill_path(
            &closed_curve,
            &grad_paint,
            FillRule::Winding,
            Affine2D::new(1e20, 0.0, 0.0, 1e20, 0.0, 0.0),
        );

        let buf = backend.snapshot_rgba();
        assert_eq!(buf.rgba.len(), 128 * 128 * 4);
        assert!(
            buf.rgba.as_chunks::<4>().0.iter().any(|p| p[0] < 240 || p[1] < 240 || p[2] < 240),
            "intersecting 1e15 stroke/pixmap/gradient must render non-white pixels (use_wgpu={use_wgpu})"
        );
    }

    // Direct render_svg_to_rgba on the exact 171-byte 1e15 stroked SVG
    let svg_171 = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><path d="M 0 0 C 10 20 40 30 50 50 Z" transform="matrix(1e15 0 0 1e15 0 0)" stroke="black" fill="none"/></svg>"#;
    let svg_buf = render_svg_to_rgba(svg_171, 100, 100).expect("render_svg_to_rgba on 1e15 SVG");
    assert_eq!(svg_buf.rgba.len(), 100 * 100 * 4);
    assert!(
        svg_buf.rgba.as_chunks::<4>().0.iter().any(|p| p[3] > 0),
        "1e15 stroked SVG must produce visible non-transparent stroke pixels"
    );

    let elapsed = t0.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "1e15 intersecting stroke & clip suite took {elapsed:?}"
    );
}
