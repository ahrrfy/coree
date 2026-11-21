// SPDX-License-Identifier: MPL-2.0

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
    compute_smart_fill, convert_object_to_bitmap, erase_stroke, hit_test_node, hit_test_point,
    hit_test_rect, list_glyphs_for_block, paint_brush_stroke, paragraph_to_curves,
    raster_flood_fill, render_page_to_rgba, render_page_to_rgba_with_options, render_svg_to_rgba,
    resample, smudge_stroke, text_on_path_to_curves, text_to_curves, trace_raster,
    trace_raster_to_mm, BackendPaint, CanvasBackend, DirtyTileCache, DodgeBurnMode,
    DynamicGuideKind, FillRule, GlyphBlockFilter, OutlinePreset, PixelRect, RasterMask,
    RenderOptions, RgbaBuffer, SceneRenderer, SnapEngine, SnapOptions, StrokeParams, TextEngine,
    TinySkiaBackend, ToneRange, TraceConfig, TraceMode, ViewMode, Viewport, TILE_SIZE_PX,
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
    doc.objects.get_mut(&r2).unwrap().fill = Fill::Solid(Color::rgba8(240, 200, 10, 255));
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
    });
    let lens_id = doc_fr.add_object(fr_lens, None).unwrap();

    let mut renderer = SceneRenderer::new();
    let vp = Viewport::for_export(283, 283, 72.0);
    let opts = RenderOptions::export();
    let mut b1 = TinySkiaBackend::new(283, 283).unwrap();
    renderer.render_document(&doc_fr, &vp, ViewMode::Enhanced, &opts, &mut b1);
    assert!(renderer.get_frozen_lens(lens_id).is_some());

    // Move frozen lens from red side to blue side (x: 15 -> 60)
    doc_fr.objects.get_mut(&lens_id).unwrap().transform = Affine2D::translate(60.0, 30.0);
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
            },
            MeshNode {
                row: 0,
                col: 1,
                pos: Point2D::new(60.0, 0.0),
                color: Color::rgba8(0, 255, 0, 255),
            },
            MeshNode {
                row: 1,
                col: 0,
                pos: Point2D::new(0.0, 60.0),
                color: Color::rgba8(0, 0, 255, 255),
            },
            MeshNode {
                row: 1,
                col: 1,
                pos: Point2D::new(60.0, 60.0),
                color: Color::rgba8(255, 255, 0, 255),
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
    let p1 = doc.pages[0].id;
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
    doc_rich.symbols.insert(sym_id, sym);

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
    assert!(asset.rgba_bytes[idx] > 220 && asset.rgba_bytes[idx + 1] < 40);

    // 2. Clone stamp copies red band down to y=44
    clone_stamp_stroke(
        &mut asset,
        Point2D::new(32.0, 16.0),
        &[Point2D::new(32.0, 44.0)],
        5.0,
        0.9,
    );
    let clone_idx = (44 * 64 + 32) * 4;
    assert!(asset.rgba_bytes[clone_idx] > 200 && asset.rgba_bytes[clone_idx + 1] < 50);

    // 3. Smudge pulls red from y=16 downward toward y=24
    smudge_stroke(
        &mut asset,
        &[Point2D::new(20.0, 16.0), Point2D::new(20.0, 24.0)],
        4.0,
        0.85,
    );
    let smudge_idx = (23 * 64 + 20) * 4;
    assert!(
        asset.rgba_bytes[smudge_idx + 1] < 245,
        "Smudge must drag red pigment downward"
    );

    // 4. Erase reduces alpha to 0 at (48, 16)
    erase_stroke(&mut asset, &[Point2D::new(48.0, 16.0)], 4.0, 1.0);
    let erase_idx = (16 * 64 + 48) * 4;
    assert_eq!(asset.rgba_bytes[erase_idx + 3], 0);

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
        wand_asset.rgba_bytes[3], 0,
        "Cutout must remove background pixel (0,0)"
    );
    assert!(
        wand_asset.rgba_bytes[(16 * 32 + 16) * 4 + 3] > 200,
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
            test_img.rgba_bytes[idx..idx + 4].copy_from_slice(&px);
        }
    }

    // Exact assertions on key filters
    let inv = apply_raster_filter(&test_img, &RasterFilter::Invert);
    assert_eq!(&inv.rgba_bytes[0..4], &[55, 195, 215, 255]);

    let blurred = apply_raster_filter(&test_img, &RasterFilter::GaussianBlur { radius_px: 3.0 });
    // Boundary column x=15 blends red (200) toward blue (40)
    let b_idx = (16 * 32 + 15) * 4;
    assert!(
        blurred.rgba_bytes[b_idx] < 180 && blurred.rgba_bytes[b_idx] > 80,
        "GaussianBlur must smooth sharp step edge: {:?}",
        &blurred.rgba_bytes[b_idx..b_idx + 4]
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
        hsl.rgba_bytes[1] > hsl.rgba_bytes[0],
        "HSL +120° hue shift should shift red into green: {:?}",
        &hsl.rgba_bytes[0..4]
    );

    let curve = apply_raster_filter(
        &test_img,
        &RasterFilter::ToneCurve {
            points: vec![(0.0, 1.0), (1.0, 0.0)],
        },
    );
    assert!(
        curve.rgba_bytes[0] < 80 && curve.rgba_bytes[2] > 180,
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
        assert_eq!(out.rgba_bytes.len(), test_img.rgba_bytes.len());
        assert_ne!(
            out.rgba_bytes, test_img.rgba_bytes,
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
        .rgba_bytes
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
    doc.grid.visible = true;
    doc.grid.spacing_x_mm = 10.0;
    doc.grid.spacing_y_mm = 10.0;
    doc.grid.subdivisions = 1;
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

    // 5. FountainLinear Transparency (fades from 1.0 on left to 0.0 on right)
    let mut trans = make_rect(10.0, 60.0, 50.0, 30.0, Color::rgba8(220, 20, 20, 255));
    trans.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::new(10.0, 75.0),
            end: Point2D::new(60.0, 75.0),
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
    doc2.grid.visible = true;
    doc2.grid.show_dots = true;
    doc2.perspective_grid.enabled = true;
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

    // FountainRadial Transparency
    let mut rad_t = make_circle(78.0, 18.0, 12.0, Color::rgba8(30, 80, 230, 255));
    rad_t.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainRadial {
            center: Point2D::new(78.0, 18.0),
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
    doc2.objects.get_mut(&grp).unwrap().opacity = 0.5;

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
            },
            MeshNode {
                row: 0,
                col: 1,
                pos: Point2D::new(3_000_000.0, 0.0),
                color: Color::rgba8(40, 60, 220, 255),
            },
            MeshNode {
                row: 1,
                col: 0,
                pos: Point2D::new(0.0, 3_000_000.0),
                color: Color::rgba8(40, 200, 60, 255),
            },
            MeshNode {
                row: 1,
                col: 1,
                pos: Point2D::new(3_000_000.0, 3_000_000.0),
                color: Color::rgba8(200, 40, 200, 255),
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
    doc.active_page_index = 1;
    doc.add_object(
        make_rect(10.0, 10.0, 40.0, 40.0, Color::rgba8(30, 60, 230, 255)),
        None,
    )
    .unwrap();
    doc.active_page_index = 0;

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
    doc.active_page_index = 1;
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
    doc.multipage_view = true;
    let rerendered_mp =
        cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
    assert!(
        rerendered_mp > 0,
        "Toggling doc.multipage_view must invalidate tile cache"
    );
    doc.multipage_view = false;
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
    });
    compare_direct_vs_tiled(&build_lens_doc(fisheye_obj), "Cross-tile FishEye");

    // C2. Cross-tile Magnify crossing x=256px
    let mut mag_obj = make_rect(50.0, 20.0, 36.0, 36.0, Color::WHITE);
    mag_obj.lens = Some(LensConfig {
        kind: LensKind::Magnify { amount: 2.5 },
        frozen: false,
        viewpoint: None,
        remove_face: false,
    });
    compare_direct_vs_tiled(&build_lens_doc(mag_obj), "Cross-tile Magnify");

    // C3. Viewpoint lens sitting strictly in Tile 1 (x=[85..115mm]) sampling blue box in Tile 0 (x=27.5, y=40mm)
    let mut vp_lens = make_rect(85.0, 25.0, 30.0, 30.0, Color::WHITE);
    vp_lens.lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: Some(Point2D::new(27.5, 40.0)),
        remove_face: false,
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
    });
    let mut frozen_doc = build_lens_doc(frozen_obj);
    let frozen_id = *frozen_doc.pages[0].layers[0].objects.last().unwrap();
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
    frozen_doc.objects.get_mut(&frozen_id).unwrap().transform = Affine2D::translate(10.0, 50.0);
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
    });
    let wf_id = doc.add_object(wf_lens, None).unwrap();
    let grp_id = doc.group_objects(&[wf_id], "LensGroup").unwrap();
    doc.objects.get_mut(&grp_id).unwrap().transform = Affine2D::translate(60.0, 10.0);

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
    doc.objects.get_mut(&wf_id).unwrap().lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: true,
    });
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
    doc.objects.get_mut(&wf_id).unwrap().lens = Some(LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: Some(Point2D::new(22.5, 22.5)),
        remove_face: false,
    });
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
    malformed.rgba_bytes.truncate(12); // len 12 != 64*64*4

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
    let page0_id = doc.pages[0].id;
    let locked_layer_id = doc
        .add_layer(page0_id, "LockedLayer", false, MasterLayerScope::None)
        .unwrap();
    let locked_obj_id = doc
        .add_object(
            make_rect(10.0, 10.0, 30.0, 30.0, Color::BLACK),
            Some(locked_layer_id),
        )
        .unwrap();
    assert!(!doc.objects[&locked_obj_id].locked);
    doc.pages[0]
        .layers
        .iter_mut()
        .find(|l| l.id == locked_layer_id)
        .unwrap()
        .locked = true;

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
    doc.objects.get_mut(&grp_id).unwrap().transform = Affine2D::translate(100.0, 80.0);

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
    pat_doc.symbols.insert(sym_id, sym);
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
    doc.guides
        .iter_mut()
        .find(|g| g.id == gid)
        .unwrap()
        .orientation = GuideOrientation::Vertical(55.0);
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
            let r = heal_asset.rgba_bytes[idx];
            let b = heal_asset.rgba_bytes[idx + 2];
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
    let sh_delta = i32::from(dodge_sh.rgba_bytes[(10 * 60 + 10) * 4]) - 35;
    let hi_delta = i32::from(dodge_sh.rgba_bytes[(10 * 60 + 50) * 4]) - 225;
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
    let burn_hi_delta = 225 - i32::from(burn_hi.rgba_bytes[(10 * 60 + 50) * 4]);
    let burn_sh_delta = 35 - i32::from(burn_hi.rgba_bytes[(10 * 60 + 10) * 4]);
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
    assert!(dodge_mid.rgba_bytes[(10 * 60 + 30) * 4] > 165);

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
        &bucket_asset.rgba_bytes[n_idx..n_idx + 4],
        &[220, 40, 80, 255],
        "Pixel within tolerance=20 must be filled"
    );
    // Exterior pixel outside border must stay untouched (240, 240, 240)
    assert_eq!(&bucket_asset.rgba_bytes[0..4], &[240, 240, 240, 255]);

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
