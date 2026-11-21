// SPDX-License-Identifier: MPL-2.0

//! Non-destructive live vector effect rendering engine.
//!
//! Implements all 10 [`Effect`] variants:
//! `DropShadow`, `BlockShadow`, `Contour`, `Blend`, `Distort`, `Envelope`,
//! `Extrude25D`, `Transparency`, `Perspective`, and `Symmetry`.

use crate::backend::{
    validate_surface_dimensions, BackendPaint, CanvasBackend, FillRule, PixelRect, RgbaBuffer,
    StrokeParams, TinySkiaBackend,
};
use crate::raster_engine::gaussian_blur_rgba;
use coree_core::math::{sanitize_f32, sanitize_f64};
use coree_core::{
    offset_polygon, Affine2D, BezierPath, BlendMode, Color, ContourMode, Document, Effect,
    EnvelopeMode, FeatherDirection, Fill, NodePayload, OutlineStyle, PathNode, Point2D,
    SceneObject, Subpath, TransparencyMode, Vec2D,
};

/// Applies all path-deforming live effects (`Distort`, `Envelope`, `Perspective`)
/// to a copy of `path` in local object coordinates.
#[must_use]
pub fn apply_geometry_effects(path: &BezierPath, effects: &[Effect]) -> BezierPath {
    let mut current = path.clone();
    for eff in effects {
        match eff {
            Effect::Distort {
                kind,
                amplitude,
                frequency,
                center,
            } => {
                let subdivided = subdivide_for_deformation(&current, 3.0);
                current = Effect::apply_distort_to_path(
                    &subdivided,
                    *kind,
                    *amplitude,
                    *frequency,
                    *center,
                );
            }
            Effect::Envelope { mode, cage } => {
                current = apply_envelope_to_path(&current, *mode, cage);
            }
            Effect::Perspective { quad } => {
                current = apply_perspective_to_path(&current, quad);
            }
            _ => {}
        }
    }
    current
}

/// Renders underlay effects (`DropShadow`, `BlockShadow`, `Contour::Outside`, `Extrude25D`, `Blend`)
/// that sit behind the primary object geometry.
#[allow(clippy::too_many_arguments)]
pub fn render_underlay_effects(
    doc: &Document,
    obj: &SceneObject,
    deformed_local_path: &BezierPath,
    world_xf: Affine2D,
    page_to_screen: Affine2D,
    px_per_mm: f64,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let local_to_screen = page_to_screen.compose(&world_xf);
    let world_path = deformed_local_path.transformed(&world_xf);

    for eff in &obj.effects {
        match eff {
            Effect::DropShadow {
                dx_mm,
                dy_mm,
                blur_mm,
                opacity,
                color,
                feather_direction,
            } => {
                render_drop_shadow(
                    &world_path,
                    page_to_screen,
                    *dx_mm,
                    *dy_mm,
                    *blur_mm,
                    *opacity * obj.opacity,
                    color,
                    *feather_direction,
                    px_per_mm,
                    backend,
                );
            }
            Effect::BlockShadow {
                dx_mm,
                dy_mm,
                color,
                simplify: _,
                overprint,
            } => {
                render_block_shadow(
                    &world_path,
                    page_to_screen,
                    *dx_mm,
                    *dy_mm,
                    color,
                    *overprint,
                    obj.opacity,
                    anti_alias,
                    backend,
                );
            }
            Effect::Contour {
                mode: ContourMode::Outside,
                steps,
                offset_mm,
                fill_color,
                outline_color,
            } => {
                render_contour_steps(
                    &world_path,
                    ContourMode::Outside,
                    *steps,
                    *offset_mm,
                    primary_fill_color(&obj.fill),
                    fill_color,
                    &obj.outline,
                    outline_color,
                    page_to_screen,
                    px_per_mm,
                    obj.opacity,
                    anti_alias,
                    backend,
                );
            }
            Effect::Extrude25D {
                vanishing_point,
                depth,
                bevel_depth: _,
                shade_color,
                light_dir,
            } => {
                render_extrude_25d(
                    &world_path,
                    primary_fill_color(&obj.fill),
                    *vanishing_point,
                    *depth,
                    shade_color,
                    *light_dir,
                    page_to_screen,
                    obj.opacity,
                    anti_alias,
                    backend,
                );
            }
            Effect::Blend {
                target_id,
                steps,
                rotation_deg,
                along_path,
            } => {
                render_blend_effect(
                    doc,
                    obj,
                    &world_path,
                    *target_id,
                    *steps,
                    *rotation_deg,
                    *along_path,
                    page_to_screen,
                    px_per_mm,
                    anti_alias,
                    backend,
                );
            }
            _ => {
                let _ = local_to_screen;
            }
        }
    }
}

/// Renders overlay effects (`Contour::Inside`, `Contour::ToCenter`, and `Extrude25D` front bevel)
/// that sit inside/on top of the primary fill.
#[allow(clippy::too_many_arguments)]
pub fn render_overlay_effects(
    obj: &SceneObject,
    deformed_local_path: &BezierPath,
    world_xf: Affine2D,
    page_to_screen: Affine2D,
    px_per_mm: f64,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let world_path = deformed_local_path.transformed(&world_xf);
    for eff in &obj.effects {
        match eff {
            Effect::Contour {
                mode,
                steps,
                offset_mm,
                fill_color,
                outline_color,
            } => {
                if matches!(mode, ContourMode::Inside | ContourMode::ToCenter) {
                    render_contour_steps(
                        &world_path,
                        *mode,
                        *steps,
                        *offset_mm,
                        primary_fill_color(&obj.fill),
                        fill_color,
                        &obj.outline,
                        outline_color,
                        page_to_screen,
                        px_per_mm,
                        obj.opacity,
                        anti_alias,
                        backend,
                    );
                }
            }
            Effect::Extrude25D { bevel_depth, .. } => {
                let bevel = sanitize_f64(*bevel_depth, 0.0).max(0.0);
                if bevel > 0.05 && !world_path.is_empty() {
                    if let Ok(inner) = offset_polygon(&world_path, -bevel.min(4.0)) {
                        if !inner.is_empty() {
                            let base_color = primary_fill_color(&obj.fill);
                            let highlight = base_color.lerp(&Color::WHITE, 0.35);
                            let paint = BackendPaint {
                                shader: crate::backend::PaintShader::Solid(highlight),
                                opacity: obj.opacity * 0.75,
                                blend_mode: BlendMode::Normal,
                                anti_alias,
                            };
                            backend.fill_path(&inner, &paint, FillRule::EvenOdd, page_to_screen);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extracts the active [`Effect::Transparency`] configuration from `effects`, if any.
#[must_use]
pub fn find_transparency_effect(effects: &[Effect]) -> Option<(&TransparencyMode, BlendMode)> {
    effects.iter().rev().find_map(|eff| match eff {
        Effect::Transparency { mode, blend_mode } => Some((mode, *blend_mode)),
        _ => None,
    })
}

/// Extracts symmetry transforms if `effects` contains an [`Effect::Symmetry`], or `[IDENTITY]`.
#[must_use]
pub fn symmetry_effect_transforms(effects: &[Effect]) -> Vec<Affine2D> {
    for eff in effects {
        if let Effect::Symmetry {
            axes,
            mirror,
            origin,
        } = eff
        {
            return Effect::symmetry_transforms(*axes, *mirror, *origin);
        }
    }
    vec![Affine2D::IDENTITY]
}

/// Modulates an offscreen-rendered `buffer` (located at `rect_px` on screen) with a
/// [`TransparencyMode`] alpha mask in world coordinates.
pub fn apply_transparency_mask(
    buffer: &mut RgbaBuffer,
    rect_px: PixelRect,
    screen_to_world: Affine2D,
    mode: &TransparencyMode,
) {
    match mode {
        TransparencyMode::Uniform(alpha) => {
            let a = sanitize_f32(*alpha, 1.0).clamp(0.0, 1.0);
            for px in buffer.rgba.as_chunks_mut::<4>().0 {
                px[3] = (f32::from(px[3]) * a).round().clamp(0.0, 255.0) as u8;
            }
        }
        TransparencyMode::FountainLinear {
            start,
            end,
            start_alpha,
            end_alpha,
        } => {
            let dir = *end - *start;
            let len_sq = dir.length_sq().max(1e-9);
            let a0 = sanitize_f32(*start_alpha, 1.0).clamp(0.0, 1.0);
            let a1 = sanitize_f32(*end_alpha, 0.0).clamp(0.0, 1.0);
            let w = buffer.width;
            let h = buffer.height;
            for ly in 0..h {
                for lx in 0..w {
                    let sp = Point2D::new(
                        f64::from(rect_px.x) + f64::from(lx) + 0.5,
                        f64::from(rect_px.y) + f64::from(ly) + 0.5,
                    );
                    let wp = screen_to_world.transform_point(sp);
                    let t = ((wp - *start).dot(dir) / len_sq).clamp(0.0, 1.0) as f32;
                    let mask_a = a0 + (a1 - a0) * t;
                    let idx = ((ly as usize) * (w as usize) + (lx as usize)) * 4;
                    buffer.rgba[idx + 3] = (f32::from(buffer.rgba[idx + 3]) * mask_a)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            }
        }
        TransparencyMode::FountainRadial {
            center,
            radius,
            inner_alpha,
            outer_alpha,
        } => {
            let r = sanitize_f64(*radius, 10.0).max(1e-4);
            let a0 = sanitize_f32(*inner_alpha, 1.0).clamp(0.0, 1.0);
            let a1 = sanitize_f32(*outer_alpha, 0.0).clamp(0.0, 1.0);
            let w = buffer.width;
            let h = buffer.height;
            for ly in 0..h {
                for lx in 0..w {
                    let sp = Point2D::new(
                        f64::from(rect_px.x) + f64::from(lx) + 0.5,
                        f64::from(rect_px.y) + f64::from(ly) + 0.5,
                    );
                    let wp = screen_to_world.transform_point(sp);
                    let t = (wp.distance_to(*center) / r).clamp(0.0, 1.0) as f32;
                    let mask_a = a0 + (a1 - a0) * t;
                    let idx = ((ly as usize) * (w as usize) + (lx as usize)) * 4;
                    buffer.rgba[idx + 3] = (f32::from(buffer.rgba[idx + 3]) * mask_a)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// Computes extra world-mm padding needed around an object for frustum culling (DirtyTileCache).
#[must_use]
pub fn effect_bounds_padding_mm(effects: &[Effect]) -> f64 {
    let mut pad = 0.0_f64;
    for eff in effects {
        match eff {
            Effect::DropShadow {
                dx_mm,
                dy_mm,
                blur_mm,
                ..
            } => {
                pad = pad.max(dx_mm.abs() + blur_mm.abs() * 3.0);
                pad = pad.max(dy_mm.abs() + blur_mm.abs() * 3.0);
            }
            Effect::BlockShadow { dx_mm, dy_mm, .. } => {
                pad = pad.max(dx_mm.abs().max(dy_mm.abs()) + 2.0);
            }
            Effect::Contour {
                mode: ContourMode::Outside,
                steps,
                offset_mm,
                ..
            } => {
                pad = pad.max(f64::from(*steps) * offset_mm.abs() + 2.0);
            }
            Effect::Extrude25D { depth, .. } => {
                pad = pad.max(depth.abs() * 2.0 + 10.0);
            }
            Effect::Distort { amplitude, .. } => {
                pad = pad.max(amplitude.abs() * 2.0 + 2.0);
            }
            _ => {}
        }
    }
    pad
}

#[allow(clippy::too_many_arguments)]
fn render_drop_shadow(
    world_path: &BezierPath,
    page_to_screen: Affine2D,
    dx_mm: f64,
    dy_mm: f64,
    blur_mm: f64,
    opacity: f32,
    color: &Color,
    feather: FeatherDirection,
    px_per_mm: f64,
    backend: &mut dyn CanvasBackend,
) {
    let op = sanitize_f32(opacity, 0.5).clamp(0.0, 1.0);
    if op <= 1e-5 || world_path.is_empty() {
        return;
    }
    let shifted_world = world_path.transformed(&Affine2D::translate(
        sanitize_f64(dx_mm, 0.0),
        sanitize_f64(dy_mm, 0.0),
    ));
    let Some(wb) = shifted_world.bounding_box() else {
        return;
    };
    let blur_px = (sanitize_f64(blur_mm, 0.0).max(0.0) * px_per_mm).min(128.0) as f32;
    let pad_px = ((blur_px.ceil() as i32) * 3 + 2).min(256);

    let sb = page_to_screen.transform_rect(wb);
    let Some(clamped) =
        PixelRect::from_f64_rect_clamped(sb, pad_px, backend.width(), backend.height())
    else {
        return;
    };

    // Expand offscreen silhouette buffer by full Gaussian kernel radius beyond `clamped`
    // (NOT clipped to `backend` edges) so tiled rendering produces identical blur values
    // across 256×256 tile boundaries with zero seams!
    let desired_margin = if blur_px > 0.2 {
        (blur_px.ceil() as u32 + 2).min(130)
    } else {
        0
    };
    let candidate_w = clamped
        .width
        .saturating_add(desired_margin.saturating_mul(2));
    let candidate_h = clamped
        .height
        .saturating_add(desired_margin.saturating_mul(2));
    let (margin, work_w, work_h) = if validate_surface_dimensions(candidate_w, candidate_h).is_ok()
    {
        (desired_margin, candidate_w, candidate_h)
    } else {
        (0, clamped.width, clamped.height)
    };

    let work_x = i64::from(clamped.x) - i64::from(margin);
    let work_y = i64::from(clamped.y) - i64::from(margin);

    let Ok(mut sub) = TinySkiaBackend::new(work_w, work_h) else {
        return;
    };
    sub.clear(Color::TRANSPARENT);
    let local_xf = Affine2D::translate(-(work_x as f64), -(work_y as f64)).compose(&page_to_screen);
    let [cr, cg, cb, ca] = color.to_rgba8();
    let solid = BackendPaint::solid(Color::rgba8(cr, cg, cb, 255));
    sub.fill_path(&shifted_world, &solid, FillRule::Winding, local_xf);

    let orig_buf = sub.into_rgba();
    let blurred = if blur_px > 0.2 {
        gaussian_blur_rgba(&orig_buf.rgba, orig_buf.width, orig_buf.height, blur_px)
    } else {
        orig_buf.rgba.clone()
    };

    // Crop the interior `[margin .. margin + clamped.width, margin .. margin + clamped.height]`
    let eff_alpha_scale = (f32::from(ca) / 255.0) * op;
    let cw = clamped.width as usize;
    let ch = clamped.height as usize;
    let ww = work_w as usize;
    let m = margin as usize;
    let mut final_rgba = Vec::with_capacity(cw * ch * 4);
    for ly in 0..ch {
        let src_row_off = (ly + m) * ww + m;
        for lx in 0..cw {
            let pidx = (src_row_off + lx) * 4;
            let oa = orig_buf.rgba[pidx + 3];
            let ba = blurred[pidx + 3];
            let shaped_a = match feather {
                FeatherDirection::Middle | FeatherDirection::Average => ba,
                FeatherDirection::Inside => ba.min(oa),
                FeatherDirection::Outside => oa.max(ba),
            };
            let out_a = (f32::from(shaped_a) * eff_alpha_scale)
                .round()
                .clamp(0.0, 255.0) as u8;
            final_rgba.extend_from_slice(&[cr, cg, cb, out_a]);
        }
    }
    let shadow_buf = RgbaBuffer {
        width: clamped.width,
        height: clamped.height,
        rgba: final_rgba,
    };
    backend.composite_region(clamped, &shadow_buf, 1.0, BlendMode::Normal);
}

#[allow(clippy::too_many_arguments)]
fn render_block_shadow(
    world_path: &BezierPath,
    page_to_screen: Affine2D,
    dx_mm: f64,
    dy_mm: f64,
    color: &Color,
    overprint: bool,
    opacity: f32,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let delta = Vec2D::new(sanitize_f64(dx_mm, 0.0), sanitize_f64(dy_mm, 0.0));
    if delta.length() <= 1e-6 || world_path.is_empty() {
        return;
    }
    let paint = BackendPaint {
        shader: crate::backend::PaintShader::Solid(color.clone()),
        opacity,
        blend_mode: if overprint {
            BlendMode::Multiply
        } else {
            BlendMode::Normal
        },
        anti_alias,
    };

    let mut subpaths = Vec::new();
    for poly in world_path.flatten(0.2) {
        if poly.len() < 2 {
            continue;
        }
        // Swept side quads along every flattened edge, consistently oriented CW so winding never cancels!
        for win in poly.windows(2) {
            let p0 = win[0];
            let p1 = win[1];
            let q1 = p1 + delta;
            let q0 = p0 + delta;
            let mut quad = vec![p0, p1, q1, q0];
            if signed_poly_area(&quad) < 0.0 {
                quad.reverse();
            }
            if signed_poly_area(&quad).abs() > 1e-6 {
                subpaths.push(Subpath::from_points(quad, true));
            }
        }
        // Back translated cap (oriented CW)
        if poly.len() >= 3 {
            let mut shifted: Vec<Point2D> = poly.iter().map(|&p| p + delta).collect();
            if signed_poly_area(&shifted) < 0.0 {
                shifted.reverse();
            }
            if signed_poly_area(&shifted).abs() > 1e-6 {
                subpaths.push(Subpath::from_points(shifted, true));
            }
        }
    }
    let swept = BezierPath { subpaths };
    backend.fill_path(&swept, &paint, FillRule::Winding, page_to_screen);
}

fn signed_poly_area(pts: &[Point2D]) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut area2 = 0.0;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        area2 += a.x * b.y - b.x * a.y;
    }
    0.5 * area2
}

#[allow(clippy::too_many_arguments)]
fn render_contour_steps(
    world_path: &BezierPath,
    mode: ContourMode,
    steps: u32,
    offset_mm: f64,
    base_fill: Color,
    target_fill: &Color,
    base_outline: &OutlineStyle,
    target_outline: &Color,
    page_to_screen: Affine2D,
    px_per_mm: f64,
    opacity: f32,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let step_mm = sanitize_f64(offset_mm, 1.0).abs().max(0.05);
    let max_steps = match mode {
        ContourMode::ToCenter | ContourMode::Inside | ContourMode::Outside => steps.clamp(1, 48),
    };
    let sign = match mode {
        ContourMode::Outside => 1.0,
        ContourMode::Inside | ContourMode::ToCenter => -1.0,
    };

    // Build stepped polygons
    let mut rings: Vec<(u32, BezierPath)> = Vec::new();
    for k in 1..=max_steps {
        let delta = sign * step_mm * f64::from(k);
        if let Ok(ring) = offset_polygon(world_path, delta) {
            if ring.is_empty() {
                if mode == ContourMode::ToCenter {
                    break;
                }
                continue;
            }
            rings.push((k, ring));
        } else if mode == ContourMode::ToCenter {
            break;
        }
    }

    if rings.is_empty() {
        return;
    }

    // Outside contours draw outermost first (back-to-front); inside contours draw step 1..N
    if mode == ContourMode::Outside {
        rings.reverse();
    }

    let total = rings.len() as f32;
    let stroke_w = if base_outline.is_visible() {
        (base_outline.effective_width_mm() as f32).max((1.0 / px_per_mm.max(1e-3)) as f32)
    } else {
        0.0
    };

    for (k, ring) in rings {
        let t = (k as f32 / total.max(1.0)).clamp(0.0, 1.0);
        let step_fill = base_fill.lerp(target_fill, t);
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(step_fill),
            opacity,
            blend_mode: BlendMode::Normal,
            anti_alias,
        };
        backend.fill_path(&ring, &paint, FillRule::EvenOdd, page_to_screen);

        if stroke_w > 0.0 || target_outline.alpha() > 1e-4 {
            let out_col = base_outline.color.lerp(target_outline, t);
            let mut st = StrokeParams::from_outline(
                base_outline,
                stroke_w.max(0.25),
                opacity,
                BlendMode::Normal,
                anti_alias,
            );
            st.color = out_col;
            backend.stroke_path(&ring, &st, page_to_screen);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_extrude_25d(
    world_path: &BezierPath,
    base_color: Color,
    vanishing_point: Point2D,
    depth: f64,
    shade_color: &Color,
    light_dir: [f32; 3],
    page_to_screen: Affine2D,
    opacity: f32,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    if world_path.is_empty() || !vanishing_point.is_finite() {
        return;
    }
    let d = sanitize_f64(depth, 10.0).max(0.5);
    let extrude_frac = (d / (d + 40.0)).clamp(0.05, 0.92);

    // Normalize directional light vector
    let lx = sanitize_f32(light_dir[0], 0.4);
    let ly = sanitize_f32(light_dir[1], -0.6);
    let lz = sanitize_f32(light_dir[2], 0.7);
    let l_len = (lx * lx + ly * ly + lz * lz).sqrt().max(1e-5);
    let ldir = [lx / l_len, ly / l_len, lz / l_len];

    struct SideFace {
        quad: [Point2D; 4],
        dist_to_vp: f64,
        color: Color,
    }

    let mut faces = Vec::new();
    for poly in world_path.flatten(0.3) {
        if poly.len() < 2 {
            continue;
        }
        for win in poly.windows(2) {
            let p0 = win[0];
            let p1 = win[1];
            if p0.distance_to(p1) <= 1e-6 {
                continue;
            }
            let q0 = p0.lerp(vanishing_point, extrude_frac);
            let q1 = p1.lerp(vanishing_point, extrude_frac);

            let edge = (p1 - p0).normalize_or_zero();
            let normal_2d = Vec2D::new(edge.y, -edge.x);
            let ndotl =
                (normal_2d.x as f32 * ldir[0] + normal_2d.y as f32 * ldir[1] + 0.35 * ldir[2])
                    .clamp(-1.0, 1.0);
            let lit_t = (0.5 + 0.5 * ndotl).clamp(0.0, 1.0);
            let face_col = shade_color.lerp(&base_color, lit_t * 0.85);

            let mid = p0.midpoint(p1).midpoint(q0.midpoint(q1));
            faces.push(SideFace {
                quad: [p0, p1, q1, q0],
                dist_to_vp: mid.distance_to(vanishing_point),
                color: face_col,
            });
        }
    }

    // Painter's sort: faces closest to vanishing point (farthest back) draw first
    faces.sort_by(|a, b| a.dist_to_vp.total_cmp(&b.dist_to_vp));

    for face in faces {
        let quad_path = BezierPath::from_polygon(face.quad, true);
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(face.color),
            opacity,
            blend_mode: BlendMode::Normal,
            anti_alias,
        };
        backend.fill_path(&quad_path, &paint, FillRule::Winding, page_to_screen);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_blend_effect(
    doc: &Document,
    src_obj: &SceneObject,
    src_world_path: &BezierPath,
    target_id: coree_core::ObjectId,
    steps: u32,
    rotation_deg: f64,
    along_path: Option<coree_core::ObjectId>,
    page_to_screen: Affine2D,
    px_per_mm: f64,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let n_steps = steps.clamp(1, 64);
    let Some(target_obj) = doc.objects.get(&target_id) else {
        return;
    };
    let Some(target_world_path) = doc.world_path_of(target_id) else {
        return;
    };

    const SAMPLES: usize = 64;
    let Some(src_ring) = resample_primary_contour(src_world_path, SAMPLES) else {
        return;
    };
    let Some(dst_ring) = resample_primary_contour(&target_world_path, SAMPLES) else {
        return;
    };
    let src_center = centroid(&src_ring);
    let dst_center = centroid(&dst_ring);
    let host_path = along_path.and_then(|pid| doc.world_path_of(pid));

    let src_fill = primary_fill_color(&src_obj.fill);
    let dst_fill = primary_fill_color(&target_obj.fill);
    let rot_total_rad = sanitize_f64(rotation_deg, 0.0).to_radians();

    for i in 1..=n_steps {
        let t = f64::from(i) / f64::from(n_steps + 1);
        let tf = t as f32;
        let center_lerp = src_center.lerp(dst_center, t);
        let target_center = host_path
            .as_ref()
            .and_then(|hp| hp.point_and_tangent_at_t(t))
            .map_or(center_lerp, |(pt, _)| pt);
        let angle = rot_total_rad * t;

        let mut pts = Vec::with_capacity(SAMPLES);
        for k in 0..SAMPLES {
            let local_src = src_ring[k] - src_center;
            let local_dst = dst_ring[k] - dst_center;
            let local_interp = Vec2D::new(
                local_src.x + (local_dst.x - local_src.x) * t,
                local_src.y + (local_dst.y - local_src.y) * t,
            );
            let rotated = if angle.abs() > 1e-6 {
                local_interp.rotate(angle)
            } else {
                local_interp
            };
            pts.push(target_center + rotated);
        }

        let morph_path = BezierPath::from_polygon(pts, true);
        let step_fill = src_fill.lerp(&dst_fill, tf);
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(step_fill),
            opacity: src_obj.opacity + (target_obj.opacity - src_obj.opacity) * tf,
            blend_mode: src_obj.blend_mode,
            anti_alias,
        };
        backend.fill_path(&morph_path, &paint, FillRule::Winding, page_to_screen);

        if src_obj.outline.is_visible() || target_obj.outline.is_visible() {
            let w_mm = src_obj.outline.effective_width_mm() * (1.0 - t)
                + target_obj.outline.effective_width_mm() * t;
            if w_mm > 1e-5 {
                let mut st = StrokeParams::from_outline(
                    &src_obj.outline,
                    (w_mm as f32).max((0.5 / px_per_mm.max(1e-3)) as f32),
                    paint.opacity,
                    src_obj.blend_mode,
                    anti_alias,
                );
                st.color = src_obj.outline.color.lerp(&target_obj.outline.color, tf);
                backend.stroke_path(&morph_path, &st, page_to_screen);
            }
        }
    }
}

/// Applies Envelope bilinear/Coons cage deformation to `path`.
fn apply_envelope_to_path(path: &BezierPath, mode: EnvelopeMode, cage: &[Point2D]) -> BezierPath {
    if cage.len() < 4 || path.is_empty() {
        return path.clone();
    }
    let Some(bb) = path.bounding_box() else {
        return path.clone();
    };
    let w = bb.width().max(1e-6);
    let h = bb.height().max(1e-6);

    // Support 4-corner cage [TL, TR, BR, BL] or 8-point cage [TL, T_mid, TR, R_mid, BR, B_mid, BL, L_mid]
    let (tl, tr, br, bl, mids) = if cage.len() >= 8 {
        (
            cage[0],
            cage[2],
            cage[4],
            cage[6],
            Some((cage[1], cage[3], cage[5], cage[7])),
        )
    } else {
        (cage[0], cage[1], cage[2], cage[3], None)
    };

    let map_pt = |p: Point2D| -> Point2D {
        let u = ((p.x - bb.min_x) / w).clamp(0.0, 1.0);
        let v = ((p.y - bb.min_y) / h).clamp(0.0, 1.0);
        if let Some((tm, rm, bm, lm)) = mids {
            // Quadratic Coons patch through 8-point envelope cage
            let top = eval_quad(tl, tm, tr, u);
            let bottom = eval_quad(bl, bm, br, u);
            let left = eval_quad(tl, lm, bl, v);
            let right = eval_quad(tr, rm, br, v);
            let ruled_u = top.lerp(bottom, v);
            let ruled_v = left.lerp(right, u);
            let bilin = tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            Point2D::new(
                ruled_u.x + ruled_v.x - bilin.x,
                ruled_u.y + ruled_v.y - bilin.y,
            )
        } else {
            let base = tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            match mode {
                EnvelopeMode::Straight | EnvelopeMode::Unconstrained => base,
                EnvelopeMode::SingleArc => {
                    let arc = (u * std::f64::consts::PI).sin() * 0.08 * h;
                    Point2D::new(base.x, base.y - arc * (1.0 - 2.0 * v))
                }
                EnvelopeMode::DoubleArc => {
                    let wave = (u * std::f64::consts::TAU).sin() * 0.06 * h;
                    Point2D::new(base.x, base.y + wave)
                }
            }
        }
    };

    let mut sub = subdivide_for_deformation(path, 2.5);
    for sp in &mut sub.subpaths {
        for node in &mut sp.nodes {
            node.pos = map_pt(node.pos);
            node.handle_in = node.handle_in.map(map_pt);
            node.handle_out = node.handle_out.map(map_pt);
        }
    }
    sub
}

/// Applies true 3x3 projective homography warp onto `quad` `[TL, TR, BR, BL]`.
fn apply_perspective_to_path(path: &BezierPath, quad: &[Point2D; 4]) -> BezierPath {
    if path.is_empty() || !quad.iter().all(|p| p.is_finite()) {
        return path.clone();
    }
    let Some(bb) = path.bounding_box() else {
        return path.clone();
    };
    let w = bb.width().max(1e-6);
    let h = bb.height().max(1e-6);
    let homography = solve_unit_square_homography(quad);

    let map_pt = |p: Point2D| -> Point2D {
        let u = (p.x - bb.min_x) / w;
        let v = (p.y - bb.min_y) / h;
        if let Some(hm) = homography {
            let denom = hm[6] * u + hm[7] * v + 1.0;
            if denom.abs() > 1e-6 {
                let x = (hm[0] * u + hm[1] * v + hm[2]) / denom;
                let y = (hm[3] * u + hm[4] * v + hm[5]) / denom;
                if x.is_finite() && y.is_finite() {
                    return Point2D::new(x, y);
                }
            }
        }
        Effect::map_perspective_uv(quad, u, v)
    };

    let mut sub = subdivide_for_deformation(path, 2.5);
    for sp in &mut sub.subpaths {
        for node in &mut sp.nodes {
            node.pos = map_pt(node.pos);
            node.handle_in = node.handle_in.map(map_pt);
            node.handle_out = node.handle_out.map(map_pt);
        }
    }
    sub
}

/// Solves the exact 2D projective homography mapping `[0,1]²` -> `quad[TL, TR, BR, BL]`.
fn solve_unit_square_homography(q: &[Point2D; 4]) -> Option<[f64; 8]> {
    // Source corners: (0,0)->q0, (1,0)->q1, (1,1)->q2, (0,1)->q3
    let src = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    let mut a = [[0.0_f64; 9]; 8];
    for i in 0..4 {
        let (u, v) = src[i];
        let (x, y) = (q[i].x, q[i].y);
        a[2 * i] = [u, v, 1.0, 0.0, 0.0, 0.0, -x * u, -x * v, x];
        a[2 * i + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -y * u, -y * v, y];
    }
    // Gaussian elimination with partial pivoting
    for col in 0..8 {
        let mut pivot_row = col;
        let mut max_val = a[col][col].abs();
        for (r, row) in a.iter().enumerate().skip(col + 1) {
            if row[col].abs() > max_val {
                max_val = row[col].abs();
                pivot_row = r;
            }
        }
        if max_val < 1e-10 {
            return None;
        }
        a.swap(col, pivot_row);
        let div = a[col][col];
        for cell in a[col].iter_mut().skip(col) {
            *cell /= div;
        }
        for r in 0..8 {
            if r == col {
                continue;
            }
            let factor = a[r][col];
            for c in col..9 {
                a[r][c] -= factor * a[col][c];
            }
        }
    }
    let mut res = [0.0_f64; 8];
    for i in 0..8 {
        if !a[i][8].is_finite() {
            return None;
        }
        res[i] = a[i][8];
    }
    Some(res)
}

fn eval_quad(p0: Point2D, p1: Point2D, p2: Point2D, t: f64) -> Point2D {
    let it = 1.0 - t;
    Point2D::new(
        it * it * p0.x + 2.0 * it * t * p1.x + t * t * p2.x,
        it * it * p0.y + 2.0 * it * t * p1.y + t * t * p2.y,
    )
}

fn subdivide_for_deformation(path: &BezierPath, max_seg_mm: f64) -> BezierPath {
    let mut out = path.clone();
    let max_len = max_seg_mm.max(0.5);
    for sp_idx in 0..out.subpaths.len() {
        let mut guard = 0usize;
        while guard < 128 {
            guard += 1;
            let seg_opt = {
                let sp = &out.subpaths[sp_idx];
                (0..sp.segment_count()).find(|&s| {
                    sp.segment_node_indices(s).is_some_and(|(i0, i1)| {
                        sp.nodes[i0].pos.distance_to(sp.nodes[i1].pos) > max_len
                    })
                })
            };
            if let Some(seg) = seg_opt {
                if out.add_node_at(sp_idx, seg, 0.5).is_err() {
                    break;
                }
            } else {
                break;
            }
        }
    }
    out
}

fn primary_fill_color(fill: &Fill) -> Color {
    match fill {
        Fill::Solid(c) => c.clone(),
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => {
            stops.first().map_or(Color::gray(0.5), |s| s.color.clone())
        }
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => fg.clone(),
        Fill::MeshFill { nodes, .. } => nodes.first().map_or(Color::gray(0.5), |n| n.color.clone()),
        _ => Color::gray(0.5),
    }
}

fn resample_primary_contour(path: &BezierPath, count: usize) -> Option<Vec<Point2D>> {
    let total = path.path_length_mm();
    if total <= 1e-6 || count < 3 {
        return None;
    }
    let mut pts = Vec::with_capacity(count);
    for i in 0..count {
        let d = (i as f64 / count as f64) * total;
        if let Some((pt, _)) = path.point_and_tangent_at(d) {
            pts.push(pt);
        }
    }
    if pts.len() == count {
        Some(pts)
    } else {
        None
    }
}

fn centroid(pts: &[Point2D]) -> Point2D {
    if pts.is_empty() {
        return Point2D::ZERO;
    }
    let sx: f64 = pts.iter().map(|p| p.x).sum();
    let sy: f64 = pts.iter().map(|p| p.y).sum();
    Point2D::new(sx / pts.len() as f64, sy / pts.len() as f64)
}

/// Helper to build a single world-space `BezierPath` from an object's base geometry (if any).
#[must_use]
pub fn extract_local_geometry_path(obj: &SceneObject) -> Option<BezierPath> {
    match &obj.payload {
        NodePayload::Path(p) => Some(p.clone()),
        NodePayload::Shape(s) => Some(s.to_bezier_path()),
        NodePayload::PowerClip { container, .. } => Some((**container).clone()),
        _ => None,
    }
}

/// Ensures every node in `path` is valid & finite (guard against NaN effect parameters).
#[must_use]
pub fn sanitize_bezier_path(path: BezierPath) -> BezierPath {
    let subpaths = path
        .subpaths
        .into_iter()
        .filter_map(|sp| {
            let nodes: Vec<PathNode> = sp.nodes.into_iter().filter(PathNode::is_finite).collect();
            (!nodes.is_empty()).then_some(Subpath {
                nodes,
                closed: sp.closed,
            })
        })
        .collect();
    BezierPath { subpaths }
}
