// SPDX-License-Identifier: MIT OR Apache-2.0

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
    apply_boolean_many, offset_polygon, Affine2D, ArtisticMedia, BevelStyle, BezierPath, BlendMode,
    BooleanOp, Color, ContourMode, Document, Effect, EnvelopeMode, ExtrudeLight, FeatherDirection,
    Fill, IccColorPipeline, NodePayload, OutlineStyle, PathNode, Perspective3x3, Point2D,
    SceneObject, Subpath, TransparencyMode, Vec2D,
};

#[inline]
fn icc_srgb(color: &Color, icc: &IccColorPipeline) -> Color {
    let [r, g, b, a] = color.to_rgba_f32_icc(icc);
    Color::rgba(r, g, b, a)
}

#[inline]
fn icc_lerp_result_to_srgb(color: &Color, icc: &IccColorPipeline) -> Color {
    match color {
        Color::Rgba { .. } => color.clone(),
        _ => icc_srgb(color, icc),
    }
}

/// Applies all path-deforming live effects (`Distort`, `Envelope`, `Perspective`)
/// to a copy of `path` in local object coordinates.
#[must_use]
pub fn apply_geometry_effects(path: &BezierPath, effects: &[Effect]) -> BezierPath {
    apply_geometry_effects_in_bounds(path, None, effects)
}

/// Applies all path-deforming live effects (`Distort`, `Envelope`, `Perspective`)
/// to `path` using an optional shared reference bounding box `ref_bounds` (used for
/// `Group`-level `Envelope`/`Perspective` so sibling children share the group's cage).
#[must_use]
pub fn apply_geometry_effects_in_bounds(
    path: &BezierPath,
    ref_bounds: Option<coree_core::Rect2D>,
    effects: &[Effect],
) -> BezierPath {
    let mut current = path.clone();
    let mut cur_ref = ref_bounds.filter(|r| r.is_finite());
    for eff in effects {
        match eff {
            Effect::Distort {
                kind,
                amplitude,
                frequency,
                center,
            } => {
                let max_seg =
                    (2.5 / sanitize_f64(*frequency, 1.0).clamp(1.0, 20.0).sqrt()).clamp(0.35, 2.0);
                let subdivided = subdivide_for_deformation(&current, max_seg);
                current = Effect::apply_distort_to_path(
                    &subdivided,
                    *kind,
                    *amplitude,
                    *frequency,
                    *center,
                );
                if let Some(rbb) = cur_ref {
                    let ref_sub = subdivide_for_deformation(&BezierPath::from_rect(rbb), max_seg);
                    cur_ref = Effect::apply_distort_to_path(
                        &ref_sub, *kind, *amplitude, *frequency, *center,
                    )
                    .bounding_box();
                }
            }
            Effect::Envelope { mode, cage } => {
                current = apply_envelope_to_path_in_bounds(&current, cur_ref, *mode, cage);
                if let Some(rbb) = cur_ref {
                    cur_ref = apply_envelope_to_path_in_bounds(
                        &BezierPath::from_rect(rbb),
                        Some(rbb),
                        *mode,
                        cage,
                    )
                    .bounding_box();
                }
            }
            Effect::Perspective { quad } => {
                current = apply_perspective_to_path_in_bounds(&current, cur_ref, quad);
                if let Some(rbb) = cur_ref {
                    cur_ref = apply_perspective_to_path_in_bounds(
                        &BezierPath::from_rect(rbb),
                        Some(rbb),
                        quad,
                    )
                    .bounding_box();
                }
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
    parent_to_screen: Affine2D,
    page_to_surface: Affine2D,
    px_per_mm: f64,
    anti_alias: bool,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    if !world_xf.is_finite() || !parent_to_screen.is_finite() || !page_to_surface.is_finite() {
        return;
    }
    let world_path = deformed_local_path.transformed(&world_xf);
    // Compute true page-mm path for page-space effects (`Blend`, `Extrude25D`) even inside Groups!
    let page_world_xf = page_to_surface
        .inverse()
        .map(|inv| inv.compose(&parent_to_screen).compose(&world_xf))
        .filter(|m| m.is_finite())
        .unwrap_or(world_xf);
    let page_world_path = deformed_local_path.transformed(&page_world_xf);

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
                    parent_to_screen,
                    *dx_mm,
                    *dy_mm,
                    *blur_mm,
                    *opacity * obj.opacity,
                    color,
                    *feather_direction,
                    px_per_mm,
                    icc,
                    backend,
                );
            }
            Effect::BlockShadow {
                dx_mm,
                dy_mm,
                color,
                simplify,
                overprint,
            } => {
                render_block_shadow(
                    &world_path,
                    parent_to_screen,
                    *dx_mm,
                    *dy_mm,
                    color,
                    *simplify,
                    *overprint,
                    obj.opacity,
                    anti_alias,
                    icc,
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
                    parent_to_screen,
                    px_per_mm,
                    obj.opacity,
                    anti_alias,
                    icc,
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
                    &page_world_path,
                    primary_fill_color(&obj.fill),
                    *vanishing_point,
                    *depth,
                    shade_color,
                    *light_dir,
                    page_to_surface,
                    obj.opacity,
                    anti_alias,
                    icc,
                    backend,
                );
            }
            Effect::Blend {
                target_id,
                steps,
                rotation_deg,
                along_path,
                rotate_along_path,
            } => {
                render_blend_effect(
                    doc,
                    obj,
                    &page_world_path,
                    *target_id,
                    *steps,
                    *rotation_deg,
                    *along_path,
                    *rotate_along_path,
                    page_to_surface,
                    px_per_mm,
                    anti_alias,
                    icc,
                    backend,
                );
            }
            Effect::Extrude3D {
                vanishing_point,
                depth,
                bevel_depth: _,
                shade_color,
                rotation_3d,
                lights,
            } => {
                render_extrude_3d(
                    &page_world_path,
                    shade_color,
                    *vanishing_point,
                    *depth,
                    *rotation_3d,
                    lights,
                    page_to_surface,
                    obj.opacity,
                    anti_alias,
                    icc,
                    backend,
                );
            }
            _ => {}
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
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    if !world_xf.is_finite() || !page_to_screen.is_finite() {
        return;
    }
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
                        icc,
                        backend,
                    );
                }
            }
            Effect::Extrude25D { bevel_depth, .. } | Effect::Extrude3D { bevel_depth, .. } => {
                let bevel = sanitize_f64(*bevel_depth, 0.0).max(0.0);
                if bevel > 0.05 && !world_path.is_empty() {
                    if let Ok(inner) = offset_polygon(&world_path, -bevel.min(4.0)) {
                        if !inner.is_empty() {
                            let base_color = primary_fill_color(&obj.fill);
                            let highlight = base_color.lerp_icc(&Color::WHITE, 0.35, icc);
                            let paint = BackendPaint {
                                shader: crate::backend::PaintShader::Solid(
                                    icc_lerp_result_to_srgb(&highlight, icc),
                                ),
                                opacity: obj.opacity * 0.75,
                                blend_mode: BlendMode::Normal,
                                anti_alias,
                            };
                            backend.fill_path(&inner, &paint, FillRule::EvenOdd, page_to_screen);
                        }
                    }
                }
            }
            Effect::InnerShadow {
                dx_mm,
                dy_mm,
                blur_mm,
                opacity,
                color,
                feather_direction,
            } => {
                render_inner_shadow(
                    &world_path,
                    page_to_screen,
                    *dx_mm,
                    *dy_mm,
                    *blur_mm,
                    *opacity * obj.opacity,
                    color,
                    *feather_direction,
                    px_per_mm,
                    icc,
                    backend,
                );
            }
            Effect::Bevel {
                style,
                distance_mm,
                softness_mm,
                light_angle_deg,
                altitude_deg,
                highlight_color,
                shadow_color,
                intensity,
            } => {
                render_bevel_effect(
                    &world_path,
                    *style,
                    *distance_mm,
                    *softness_mm,
                    *light_angle_deg,
                    *altitude_deg,
                    highlight_color,
                    shadow_color,
                    *intensity * obj.opacity,
                    page_to_screen,
                    px_per_mm,
                    anti_alias,
                    icc,
                    backend,
                );
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
    Effect::symmetry_transforms_for(effects)
}

/// Modulates an offscreen-rendered `buffer` (located at `rect_px` on screen) with a
/// [`TransparencyMode`] alpha mask in object-local coordinates.
pub fn apply_transparency_mask(
    buffer: &mut RgbaBuffer,
    rect_px: PixelRect,
    screen_to_local: Affine2D,
    mode: &TransparencyMode,
) {
    let w = buffer.width;
    let h = buffer.height;
    let apply_at = |buffer: &mut RgbaBuffer, f: &dyn Fn(Point2D) -> f32| {
        for ly in 0..h {
            let sy = f64::from(rect_px.y) + f64::from(ly) + 0.5;
            for lx in 0..w {
                let sx = f64::from(rect_px.x) + f64::from(lx) + 0.5;
                let lp = screen_to_local.transform_point(Point2D::new(sx, sy));
                let mask_a = f(lp).clamp(0.0, 1.0);
                let idx = ((ly as usize) * (w as usize) + (lx as usize)) * 4;
                buffer.rgba[idx + 3] = (f32::from(buffer.rgba[idx + 3]) * mask_a)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    };

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
            apply_at(buffer, &|lp| {
                let t = ((lp - *start).dot(dir) / len_sq).clamp(0.0, 1.0) as f32;
                a0 + (a1 - a0) * t
            });
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
            apply_at(buffer, &|lp| {
                let t = (lp.distance_to(*center) / r).clamp(0.0, 1.0) as f32;
                a0 + (a1 - a0) * t
            });
        }
        TransparencyMode::Pattern {
            pattern_name,
            tile_w,
            tile_h,
            fg_opacity,
            bg_opacity,
        } => {
            let tw = sanitize_f64(*tile_w, 10.0).max(0.25);
            let th = sanitize_f64(*tile_h, 10.0).max(0.25);
            let fga = sanitize_f32(*fg_opacity, 1.0).clamp(0.0, 1.0);
            let bga = sanitize_f32(*bg_opacity, 0.0).clamp(0.0, 1.0);
            let kind = pattern_name.to_ascii_lowercase();
            apply_at(buffer, &|lp| {
                let u = lp.x.rem_euclid(tw) / tw;
                let v = lp.y.rem_euclid(th) / th;
                let is_fg = if kind.contains("stripe") || kind.contains("line") {
                    (v * 4.0).fract() < 0.5
                } else if kind.contains("dot") || kind.contains("polka") {
                    (u - 0.5).hypot(v - 0.5) <= 0.25
                } else if kind.contains("cross") || kind.contains("hatch") || kind.contains("grid")
                {
                    u < 0.125 || v < 0.125
                } else if kind.contains("diag") {
                    ((u + v) * 2.0).fract() < 0.35
                } else if kind.contains("brick") {
                    let row = (v * 2.0).floor() as i64;
                    let off = if (row & 1) != 0 { 0.5 } else { 0.0 };
                    (v * 2.0).fract() < 0.15 || (u + off).fract() < 0.1
                } else {
                    (u < 0.5) ^ (v < 0.5)
                };
                if is_fg {
                    fga
                } else {
                    bga
                }
            });
        }
        TransparencyMode::Texture {
            preset,
            scale,
            fg_opacity,
            bg_opacity,
        } => {
            let sc = sanitize_f64(*scale, 1.0).abs().max(0.05);
            let fga = sanitize_f32(*fg_opacity, 1.0).clamp(0.0, 1.0);
            let bga = sanitize_f32(*bg_opacity, 0.0).clamp(0.0, 1.0);
            let preset_lc = preset.to_ascii_lowercase();
            let ps_preset = coree_core::PostScriptFillPreset::from_preset_str(preset);
            apply_at(buffer, &|lp| {
                let sx = lp.x / sc;
                let sy = lp.y / sc;
                let n = if let Some(ps) = ps_preset {
                    crate::renderer::eval_postscript_fill(
                        ps,
                        0,
                        Point2D::new(sx, sy),
                        Point2D::ZERO,
                        0.05,
                    )
                } else {
                    crate::renderer::eval_procedural_texture(&preset_lc, 0, sx, sy)
                };
                bga + (fga - bga) * n.clamp(0.0, 1.0)
            });
        }
        TransparencyMode::Conical {
            center,
            angle_deg,
            start_opacity,
            end_opacity,
        } => {
            let base_deg = sanitize_f64(*angle_deg, 0.0);
            let a0 = sanitize_f32(*start_opacity, 1.0).clamp(0.0, 1.0);
            let a1 = sanitize_f32(*end_opacity, 0.0).clamp(0.0, 1.0);
            apply_at(buffer, &|lp| {
                let dx = lp.x - center.x;
                let dy = lp.y - center.y;
                let deg = dy.atan2(dx).to_degrees();
                let t = ((deg - base_deg).rem_euclid(360.0) / 360.0) as f32;
                a0 + (a1 - a0) * t
            });
        }
        TransparencyMode::Square {
            center,
            size,
            angle_deg,
            start_opacity,
            end_opacity,
        } => {
            let half = (sanitize_f64(*size, 20.0).abs() * 0.5).max(1e-4);
            let rot = -sanitize_f64(*angle_deg, 0.0).to_radians();
            let (s, c) = rot.sin_cos();
            let a0 = sanitize_f32(*start_opacity, 1.0).clamp(0.0, 1.0);
            let a1 = sanitize_f32(*end_opacity, 0.0).clamp(0.0, 1.0);
            apply_at(buffer, &|lp| {
                let dx = lp.x - center.x;
                let dy = lp.y - center.y;
                let rx = dx * c - dy * s;
                let ry = dx * s + dy * c;
                let cheb = (rx.abs().max(ry.abs()) / half).clamp(0.0, 1.0) as f32;
                a0 + (a1 - a0) * cheb
            });
        }
    }
}

/// Computes extra world-mm padding needed around an object for frustum culling (DirtyTileCache).
#[must_use]
pub fn effect_bounds_padding_mm(effects: &[Effect]) -> f64 {
    Effect::effects_bounds_padding_mm(effects)
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
    icc: &IccColorPipeline,
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

    // Anchor offscreen shadow rasterization + blur to the UNCLIPPED shadow bounds whenever
    // within safe surface dimensions so `tiny_skia` edge clipping and `f32` `local_xf`
    // rounding match bit-for-bit across `256×256` tile boundaries (even for rotated shadows!).
    let full_x0 = (sb.min_x.floor() - f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_y0 = (sb.min_y.floor() - f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_x1 = (sb.max_x.ceil() + f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_y1 = (sb.max_y.ceil() + f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_w_i64 = full_x1.saturating_sub(full_x0);
    let full_h_i64 = full_y1.saturating_sub(full_y0);

    let (work_x, work_y, work_w, work_h, crop_x, crop_y) = if full_w_i64 > 0
        && full_h_i64 > 0
        && full_w_i64 <= 4096
        && full_h_i64 <= 4096
        && validate_surface_dimensions(full_w_i64 as u32, full_h_i64 as u32).is_ok()
    {
        let cx = (i64::from(clamped.x) - full_x0).max(0) as usize;
        let cy = (i64::from(clamped.y) - full_y0).max(0) as usize;
        (
            full_x0,
            full_y0,
            full_w_i64 as u32,
            full_h_i64 as u32,
            cx,
            cy,
        )
    } else {
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
        let (margin, ww, wh) = if validate_surface_dimensions(candidate_w, candidate_h).is_ok() {
            (desired_margin, candidate_w, candidate_h)
        } else {
            (0, clamped.width, clamped.height)
        };
        (
            i64::from(clamped.x) - i64::from(margin),
            i64::from(clamped.y) - i64::from(margin),
            ww,
            wh,
            margin as usize,
            margin as usize,
        )
    };

    let Ok(mut sub) = TinySkiaBackend::new(work_w, work_h) else {
        return;
    };
    sub.clear(Color::TRANSPARENT);
    let local_xf = Affine2D::translate(-(work_x as f64), -(work_y as f64)).compose(&page_to_screen);
    let [cr, cg, cb, ca] = color.to_rgba8_icc(icc);
    let solid = BackendPaint::solid(Color::rgba8(cr, cg, cb, 255));
    sub.fill_path(&shifted_world, &solid, FillRule::Winding, local_xf);

    let orig_buf = sub.into_rgba();
    let blurred = if blur_px > 0.2 {
        gaussian_blur_rgba(&orig_buf.rgba, orig_buf.width, orig_buf.height, blur_px)
    } else {
        orig_buf.rgba.clone()
    };

    // Crop the interior `[crop_x .. crop_x + clamped.width, crop_y .. crop_y + clamped.height]`
    let eff_alpha_scale = (f32::from(ca) / 255.0) * op;
    let cw = clamped.width as usize;
    let ch = clamped.height as usize;
    let ww = work_w as usize;
    let wh = work_h as usize;
    let mut final_rgba = Vec::with_capacity(cw * ch * 4);
    for ly in 0..ch {
        let sy = ly + crop_y;
        for lx in 0..cw {
            let sx = lx + crop_x;
            if sx < ww && sy < wh {
                let pidx = (sy * ww + sx) * 4;
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
            } else {
                final_rgba.extend_from_slice(&[cr, cg, cb, 0]);
            }
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
    simplify: bool,
    overprint: bool,
    opacity: f32,
    anti_alias: bool,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    let delta = Vec2D::new(sanitize_f64(dx_mm, 0.0), sanitize_f64(dy_mm, 0.0));
    if delta.length() <= 1e-6 || world_path.is_empty() {
        return;
    }
    let paint = BackendPaint {
        shader: crate::backend::PaintShader::Solid(icc_srgb(color, icc)),
        opacity,
        blend_mode: if overprint {
            BlendMode::Multiply
        } else {
            BlendMode::Normal
        },
        anti_alias,
    };

    let mut swept = build_block_shadow_geometry(world_path, delta);
    if simplify && !swept.subpaths.is_empty() {
        let pieces: Vec<BezierPath> = swept
            .subpaths
            .iter()
            .cloned()
            .map(BezierPath::from_subpath)
            .collect();
        if let Ok(welded) = apply_boolean_many(BooleanOp::Weld, &pieces) {
            if !welded.is_empty() {
                let mut merged = Vec::new();
                for w in welded {
                    merged.extend(w.subpaths);
                }
                if !merged.is_empty() {
                    swept = BezierPath { subpaths: merged };
                }
            }
        }
    }
    if !swept.is_empty() {
        backend.fill_path(&swept, &paint, FillRule::Winding, page_to_screen);
    }
}

fn build_block_shadow_geometry(world_path: &BezierPath, delta: Vec2D) -> BezierPath {
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
    BezierPath { subpaths }
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
fn compute_contour_rings(
    world_path: &BezierPath,
    mode: ContourMode,
    steps: u32,
    offset_mm: f64,
    base_fill: &Color,
    target_fill: &Color,
    base_outline: &OutlineStyle,
    target_outline: &Color,
    icc: &IccColorPipeline,
) -> Vec<(u32, BezierPath, Color, Color)> {
    let step_mm = sanitize_f64(offset_mm, 1.0).abs().max(0.05);
    let max_steps = steps.clamp(1, 512);
    let sign = match mode {
        ContourMode::Outside => 1.0,
        ContourMode::Inside | ContourMode::ToCenter => -1.0,
    };

    let mut rings: Vec<(u32, BezierPath)> = Vec::new();
    for k in 1..=max_steps {
        let delta = sign * step_mm * f64::from(k);
        if let Ok(ring) = offset_polygon(world_path, delta) {
            let collapsed = ring.is_empty()
                || ring
                    .bounding_box()
                    .is_none_or(|b| b.width() < 1e-4 || b.height() < 1e-4);
            if collapsed {
                if matches!(mode, ContourMode::ToCenter | ContourMode::Inside) {
                    break;
                }
                continue;
            }
            rings.push((k, ring));
        } else if matches!(mode, ContourMode::ToCenter | ContourMode::Inside) {
            break;
        }
    }
    if mode == ContourMode::Outside {
        rings.reverse();
    }
    let max_k = rings.iter().map(|(k, _)| *k).max().unwrap_or(1) as f32;
    rings
        .into_iter()
        .map(|(k, ring)| {
            let t = (k as f32 / max_k.max(1.0)).clamp(0.0, 1.0);
            (
                k,
                ring,
                base_fill.lerp_icc(target_fill, t, icc),
                base_outline.color.lerp_icc(target_outline, t, icc),
            )
        })
        .collect()
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
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    let rings = compute_contour_rings(
        world_path,
        mode,
        steps,
        offset_mm,
        &base_fill,
        target_fill,
        base_outline,
        target_outline,
        icc,
    );
    if rings.is_empty() {
        return;
    }

    let stroke_w = if base_outline.is_visible() {
        (base_outline.effective_width_mm() as f32).max((1.0 / px_per_mm.max(1e-3)) as f32)
    } else {
        0.0
    };

    for (_k, ring, step_fill, out_col) in rings {
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(icc_lerp_result_to_srgb(&step_fill, icc)),
            opacity,
            blend_mode: BlendMode::Normal,
            anti_alias,
        };
        backend.fill_path(&ring, &paint, FillRule::EvenOdd, page_to_screen);

        if stroke_w > 0.0 || target_outline.alpha() > 1e-4 {
            let mut st = StrokeParams::from_outline(
                base_outline,
                stroke_w.max(0.25),
                opacity,
                BlendMode::Normal,
                anti_alias,
            );
            st.color = icc_lerp_result_to_srgb(&out_col, icc);
            backend.stroke_path(&ring, &st, page_to_screen);
        }
    }
}

fn compute_extrude_25d_faces(
    world_path: &BezierPath,
    base_color: Color,
    vanishing_point: Point2D,
    depth: f64,
    shade_color: &Color,
    light_dir: [f32; 3],
    icc: &IccColorPipeline,
) -> Vec<([Point2D; 4], Color)> {
    if world_path.is_empty() || !vanishing_point.is_finite() {
        return Vec::new();
    }
    let d = sanitize_f64(depth, 10.0).max(0.5);
    let extrude_frac = (d / (d + 40.0)).clamp(0.05, 0.92);
    let lx = sanitize_f32(light_dir[0], 0.4);
    let ly = sanitize_f32(light_dir[1], -0.6);
    let lz = sanitize_f32(light_dir[2], 0.7);
    let l_len = (lx * lx + ly * ly + lz * lz).sqrt().max(1e-5);
    let ldir = [lx / l_len, ly / l_len, lz / l_len];

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
            let face_col = shade_color.lerp_icc(&base_color, lit_t * 0.85, icc);
            let mid = p0.midpoint(p1).midpoint(q0.midpoint(q1));
            faces.push(([p0, p1, q1, q0], mid.distance_to(vanishing_point), face_col));
        }
    }
    faces.sort_by(|a, b| a.1.total_cmp(&b.1));
    faces.into_iter().map(|(q, _, c)| (q, c)).collect()
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
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    for (quad, color) in compute_extrude_25d_faces(
        world_path,
        base_color,
        vanishing_point,
        depth,
        shade_color,
        light_dir,
        icc,
    ) {
        let quad_path = BezierPath::from_polygon(quad, true);
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(icc_lerp_result_to_srgb(&color, icc)),
            opacity,
            blend_mode: BlendMode::Normal,
            anti_alias,
        };
        backend.fill_path(&quad_path, &paint, FillRule::Winding, page_to_screen);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_inner_shadow(
    world_path: &BezierPath,
    page_to_screen: Affine2D,
    dx_mm: f64,
    dy_mm: f64,
    blur_mm: f64,
    opacity: f32,
    color: &Color,
    feather: FeatherDirection,
    px_per_mm: f64,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    let op = sanitize_f32(opacity, 0.5).clamp(0.0, 1.0);
    if op <= 1e-5 || world_path.is_empty() {
        return;
    }
    let Some(wb) = world_path.bounding_box() else {
        return;
    };
    let blur_px = (sanitize_f64(blur_mm, 0.0).max(0.0) * px_per_mm * 2.0).min(128.0) as f32;
    let pad_px = ((blur_px.ceil() as i32) * 3 + 2).min(256);
    let sb = page_to_screen.transform_rect(wb);
    let Some(clamped) = PixelRect::from_f64_rect_clamped(sb, 2, backend.width(), backend.height())
    else {
        return;
    };

    let full_x0 = (sb.min_x.floor() - f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_y0 = (sb.min_y.floor() - f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_x1 = (sb.max_x.ceil() + f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_y1 = (sb.max_y.ceil() + f64::from(pad_px)).clamp(-1e9, 1e9) as i64;
    let full_w_i64 = full_x1.saturating_sub(full_x0);
    let full_h_i64 = full_y1.saturating_sub(full_y0);
    let (work_x, work_y, work_w, work_h, crop_x, crop_y) = if full_w_i64 > 0
        && full_h_i64 > 0
        && full_w_i64 <= 4096
        && full_h_i64 <= 4096
        && validate_surface_dimensions(full_w_i64 as u32, full_h_i64 as u32).is_ok()
    {
        (
            full_x0,
            full_y0,
            full_w_i64 as u32,
            full_h_i64 as u32,
            (i64::from(clamped.x) - full_x0).max(0) as usize,
            (i64::from(clamped.y) - full_y0).max(0) as usize,
        )
    } else {
        (
            i64::from(clamped.x),
            i64::from(clamped.y),
            clamped.width,
            clamped.height,
            0usize,
            0usize,
        )
    };

    let Ok(mut mask_sub) = TinySkiaBackend::new(work_w, work_h) else {
        return;
    };
    let Ok(mut cast_sub) = TinySkiaBackend::new(work_w, work_h) else {
        return;
    };
    mask_sub.clear(Color::TRANSPARENT);
    cast_sub.clear(Color::TRANSPARENT);

    let local_xf = Affine2D::translate(-(work_x as f64), -(work_y as f64)).compose(&page_to_screen);
    let white = BackendPaint::solid(Color::WHITE);
    mask_sub.fill_path(world_path, &white, FillRule::EvenOdd, local_xf);

    let shifted_world = world_path.transformed(&Affine2D::translate(
        sanitize_f64(dx_mm, 0.0),
        sanitize_f64(dy_mm, 0.0),
    ));
    cast_sub.fill_path(&shifted_world, &white, FillRule::EvenOdd, local_xf);

    let mask_buf = mask_sub.into_rgba();
    let cast_buf = cast_sub.into_rgba();
    let blurred_cast = if blur_px > 0.2 {
        gaussian_blur_rgba(&cast_buf.rgba, cast_buf.width, cast_buf.height, blur_px)
    } else {
        cast_buf.rgba.clone()
    };

    let [cr, cg, cb, ca] = color.to_rgba8_icc(icc);
    let eff_alpha_scale = (f32::from(ca) / 255.0) * op;
    let cw = clamped.width as usize;
    let ch = clamped.height as usize;
    let ww = work_w as usize;
    let wh = work_h as usize;
    let mut final_rgba = Vec::with_capacity(cw * ch * 4);
    for ly in 0..ch {
        let sy = ly + crop_y;
        for lx in 0..cw {
            let sx = lx + crop_x;
            if sx < ww && sy < wh {
                let pidx = (sy * ww + sx) * 4;
                let inside_a = f32::from(mask_buf.rgba[pidx + 3]) / 255.0;
                let raw_inv = 255_u8.saturating_sub(cast_buf.rgba[pidx + 3]);
                let blur_inv = 255_u8.saturating_sub(blurred_cast[pidx + 3]);
                let shaped = match feather {
                    FeatherDirection::Middle | FeatherDirection::Average => blur_inv,
                    FeatherDirection::Inside => blur_inv.max(raw_inv),
                    FeatherDirection::Outside => blur_inv.min(raw_inv),
                };
                let out_a = (inside_a * f32::from(shaped) * eff_alpha_scale)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                final_rgba.extend_from_slice(&[cr, cg, cb, out_a]);
            } else {
                final_rgba.extend_from_slice(&[cr, cg, cb, 0]);
            }
        }
    }
    backend.composite_region(
        clamped,
        &RgbaBuffer {
            width: clamped.width,
            height: clamped.height,
            rgba: final_rgba,
        },
        1.0,
        BlendMode::Normal,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_bevel_effect(
    world_path: &BezierPath,
    style: BevelStyle,
    distance_mm: f64,
    softness_mm: f64,
    light_angle_deg: f64,
    altitude_deg: f64,
    highlight_color: &Color,
    shadow_color: &Color,
    intensity: f32,
    page_to_screen: Affine2D,
    px_per_mm: f64,
    anti_alias: bool,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    let inten = sanitize_f32(intensity, 0.75).clamp(0.0, 1.0);
    if inten <= 1e-5 || world_path.is_empty() {
        return;
    }
    let dist = sanitize_f64(distance_mm, 2.5).clamp(0.15, 25.0);
    let Some(wb) = world_path.bounding_box() else {
        return;
    };
    let ang = sanitize_f64(light_angle_deg, 135.0).to_radians();
    let alt = sanitize_f64(altitude_deg, 45.0)
        .clamp(5.0, 89.0)
        .to_radians();
    let light_dir = Vec2D::new(ang.cos(), ang.sin());
    let alt_boost = alt.cos().clamp(0.25, 1.0);
    let soft_px = (sanitize_f64(softness_mm, 0.5).max(0.0) * px_per_mm).min(24.0) as f32;
    let expand_mm = match style {
        BevelStyle::Emboss => dist + 1.0,
        BevelStyle::SoftEdge => 1.0,
    };
    let sb = page_to_screen.transform_rect(wb.expand(expand_mm));
    let pad_px = ((soft_px.ceil() as i32) * 2 + 2).min(64);
    let Some(clamped) =
        PixelRect::from_f64_rect_clamped(sb, pad_px, backend.width(), backend.height())
    else {
        return;
    };
    let Ok(mut bevel_sub) = TinySkiaBackend::new(clamped.width, clamped.height) else {
        return;
    };
    bevel_sub.clear(Color::TRANSPARENT);
    let local_xf =
        Affine2D::translate(-f64::from(clamped.x), -f64::from(clamped.y)).compose(&page_to_screen);

    let clip_inside = matches!(style, BevelStyle::SoftEdge);
    if clip_inside {
        bevel_sub.push_clip(world_path, FillRule::EvenOdd, local_xf);
    }

    let c_center = wb.center();
    for poly in world_path.flatten(0.15) {
        if poly.len() < 3 {
            continue;
        }
        let n = poly.len();
        for i in 0..(n - 1) {
            let p0 = poly[i];
            let p1 = poly[i + 1];
            let seg = p1 - p0;
            if seg.length() <= 1e-5 {
                continue;
            }
            let dir = seg / seg.length();
            let cand_n = Vec2D::new(-dir.y, dir.x);
            let mid = p0.lerp(p1, 0.5);
            let inward = if (c_center - mid).dot(cand_n) >= 0.0 {
                cand_n
            } else {
                cand_n * -1.0
            };
            let outward = inward * -1.0;
            let raw_dot = outward.dot(light_dir) * alt_boost;
            let (is_hi, strength) = (raw_dot >= 0.0, raw_dot.abs());
            let col = if is_hi {
                icc_srgb(highlight_color, icc)
            } else {
                icc_srgb(shadow_color, icc)
            };
            let edge_alpha = (strength as f32 * inten * 0.85).clamp(0.05, 0.95);
            let (q0, q1, q2, q3) = match style {
                BevelStyle::Emboss => (
                    p0 - inward * (dist * 0.5),
                    p1 - inward * (dist * 0.5),
                    p1 + inward * (dist * 0.5),
                    p0 + inward * (dist * 0.5),
                ),
                BevelStyle::SoftEdge => (p0, p1, p1 + inward * dist, p0 + inward * dist),
            };
            let quad = BezierPath::from_polygon([q0, q1, q2, q3], true);
            let paint = BackendPaint {
                shader: crate::backend::PaintShader::Solid(col),
                opacity: edge_alpha,
                blend_mode: BlendMode::Normal,
                anti_alias,
            };
            bevel_sub.fill_path(&quad, &paint, FillRule::Winding, local_xf);
        }
    }
    if clip_inside {
        bevel_sub.pop_clip();
    }

    let mut buf = bevel_sub.into_rgba();
    if soft_px > 0.25 {
        buf.rgba = gaussian_blur_rgba(&buf.rgba, buf.width, buf.height, soft_px);
    }
    backend.composite_region(clamped, &buf, 1.0, BlendMode::Normal);
}

fn rotate_pt_3d(p: [f64; 3], rot_deg: [f64; 3]) -> [f64; 3] {
    let rx = sanitize_f64(rot_deg[0], 0.0).to_radians();
    let ry = sanitize_f64(rot_deg[1], 0.0).to_radians();
    let rz = sanitize_f64(rot_deg[2], 0.0).to_radians();

    let (sx, cx) = rx.sin_cos();
    let (sy, cy) = ry.sin_cos();
    let (sz, cz) = rz.sin_cos();

    let (x0, y0, z0) = (p[0], p[1], p[2]);
    // Rx
    let y1 = y0 * cx - z0 * sx;
    let z1 = y0 * sx + z0 * cx;
    let x1 = x0;
    // Ry
    let x2 = x1 * cy + z1 * sy;
    let z2 = -x1 * sy + z1 * cy;
    let y2 = y1;
    // Rz
    let x3 = x2 * cz - y2 * sz;
    let y3 = x2 * sz + y2 * cz;
    [x3, y3, z2]
}

fn shade_color_by_lights(
    base: &Color,
    normal: [f64; 3],
    lights: &[ExtrudeLight],
    icc: &IccColorPipeline,
) -> Color {
    let [br, bg, bb, ba] = base.to_rgba8_icc(icc);
    let mut r_acc = f64::from(br) / 255.0 * 0.22;
    let mut g_acc = f64::from(bg) / 255.0 * 0.22;
    let mut b_acc = f64::from(bb) / 255.0 * 0.22;
    let default_lights = [ExtrudeLight {
        dir: [0.45, -0.6, 0.65],
        intensity: 0.9,
    }];
    let used = if lights.is_empty() {
        &default_lights[..]
    } else {
        lights
    };
    let n_len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2])
        .sqrt()
        .max(1e-9);
    let n = [normal[0] / n_len, normal[1] / n_len, normal[2] / n_len];
    for l in used {
        let ld = [
            f64::from(l.dir[0]),
            f64::from(l.dir[1]),
            f64::from(l.dir[2]),
        ];
        let l_len = (ld[0] * ld[0] + ld[1] * ld[1] + ld[2] * ld[2])
            .sqrt()
            .max(1e-9);
        let dot = ((n[0] * ld[0] + n[1] * ld[1] + n[2] * ld[2]) / l_len).max(0.0);
        let inten = f64::from(sanitize_f32(l.intensity, 0.8).clamp(0.0, 3.0));
        r_acc += (f64::from(br) / 255.0) * dot * inten;
        g_acc += (f64::from(bg) / 255.0) * dot * inten;
        b_acc += (f64::from(bb) / 255.0) * dot * inten;
    }
    Color::rgba(
        r_acc.clamp(0.0, 1.0) as f32,
        g_acc.clamp(0.0, 1.0) as f32,
        b_acc.clamp(0.0, 1.0) as f32,
        f32::from(ba) / 255.0,
    )
}

fn compute_extrude_3d_faces(
    world_path: &BezierPath,
    base_color: &Color,
    vanishing_point: Point2D,
    depth: f64,
    rotation_3d: [f64; 3],
    lights: &[ExtrudeLight],
    icc: &IccColorPipeline,
) -> Vec<(Vec<Point2D>, Color)> {
    let mut out = Vec::new();
    let Some(wb) = world_path.bounding_box() else {
        return out;
    };
    let d = sanitize_f64(depth, 15.0).clamp(0.5, 500.0);
    let center = wb.center();
    let focal = 250.0;
    let project_3d = |p3: [f64; 3]| -> Point2D {
        let z_eff = (p3[2] + d * 0.5).clamp(-focal * 0.85, focal * 2.0);
        let persp_scale = focal / (focal - z_eff * 0.45);
        let raw_2d = Point2D::new(
            center.x + p3[0] * persp_scale,
            center.y + p3[1] * persp_scale,
        );
        let back_t = ((-p3[2]) / d).clamp(0.0, 1.0) * 0.35;
        if vanishing_point.is_finite() {
            raw_2d.lerp(vanishing_point, back_t)
        } else {
            raw_2d
        }
    };

    let mut tagged: Vec<(f64, Vec<Point2D>, Color)> = Vec::new();
    for poly in world_path.flatten(0.2) {
        if poly.len() < 3 {
            continue;
        }
        let front3: Vec<[f64; 3]> = poly
            .iter()
            .map(|p| {
                let r = rotate_pt_3d([p.x - center.x, p.y - center.y, d * 0.5], rotation_3d);
                [r[0], r[1], r[2] - d * 0.5]
            })
            .collect();
        let back3: Vec<[f64; 3]> = poly
            .iter()
            .map(|p| {
                let r = rotate_pt_3d([p.x - center.x, p.y - center.y, -d * 0.5], rotation_3d);
                [r[0], r[1], r[2] - d * 0.5]
            })
            .collect();
        let back2: Vec<Point2D> = back3.iter().map(|&p| project_3d(p)).collect();
        let back_avg_z = back3.iter().map(|p| p[2]).sum::<f64>() / back3.len() as f64;
        let back_col = shade_color_by_lights(base_color, [0.0, 0.0, -1.0], lights, icc);
        tagged.push((back_avg_z, back2, back_col));

        for i in 0..(poly.len() - 1) {
            let f0 = front3[i];
            let f1 = front3[i + 1];
            let b1 = back3[i + 1];
            let b0 = back3[i];
            let u = [f1[0] - f0[0], f1[1] - f0[1], f1[2] - f0[2]];
            let v = [b0[0] - f0[0], b0[1] - f0[1], b0[2] - f0[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let avg_z = (f0[2] + f1[2] + b1[2] + b0[2]) * 0.25;
            let quad = vec![
                project_3d(f0),
                project_3d(f1),
                project_3d(b1),
                project_3d(b0),
            ];
            let shade = shade_color_by_lights(base_color, n, lights, icc);
            tagged.push((avg_z, quad, shade));
        }
    }
    tagged.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, quad, shade) in tagged {
        out.push((quad, shade));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn render_extrude_3d(
    world_path: &BezierPath,
    base_color: &Color,
    vanishing_point: Point2D,
    depth: f64,
    rotation_3d: [f64; 3],
    lights: &[ExtrudeLight],
    page_to_screen: Affine2D,
    opacity: f32,
    anti_alias: bool,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    for (quad, color) in compute_extrude_3d_faces(
        world_path,
        base_color,
        vanishing_point,
        depth,
        rotation_3d,
        lights,
        icc,
    ) {
        let quad_path = BezierPath::from_polygon(quad, true);
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(icc_lerp_result_to_srgb(&color, icc)),
            opacity,
            blend_mode: BlendMode::Normal,
            anti_alias,
        };
        backend.fill_path(&quad_path, &paint, FillRule::Winding, page_to_screen);
    }
}

struct ComputedBlendStep {
    path: BezierPath,
    fill: Color,
    opacity: f32,
    outline_width_mm: Option<f64>,
    outline_color: Color,
}

#[allow(clippy::too_many_arguments)]
fn compute_blend_steps(
    doc: &Document,
    src_obj: &SceneObject,
    src_world_path: &BezierPath,
    target_id: coree_core::ObjectId,
    steps: u32,
    rotation_deg: f64,
    along_path: Option<coree_core::ObjectId>,
    rotate_along_path: bool,
    icc: &IccColorPipeline,
) -> Vec<ComputedBlendStep> {
    let n_steps = steps.clamp(1, 999);
    let Some(target_obj) = doc.object(target_id) else {
        return Vec::new();
    };
    let Some(target_world_path) = doc.world_path_of(target_id) else {
        return Vec::new();
    };

    const SAMPLES: usize = 72;
    type BlendContourPair = (Vec<Point2D>, Vec<Point2D>, Point2D, Point2D, bool);
    let mut contour_pairs: Vec<BlendContourPair> = Vec::new();
    let pair_count = src_world_path
        .subpaths
        .len()
        .min(target_world_path.subpaths.len());
    for s_idx in 0..pair_count {
        let s_closed = src_world_path.subpaths[s_idx].closed;
        let t_closed = target_world_path.subpaths[s_idx].closed;
        let s_bp = BezierPath::from_subpath(src_world_path.subpaths[s_idx].clone());
        let t_bp = BezierPath::from_subpath(target_world_path.subpaths[s_idx].clone());
        if let (Some(sr), Some(dr)) = (
            resample_primary_contour(&s_bp, SAMPLES),
            resample_primary_contour(&t_bp, SAMPLES),
        ) {
            let sc = centroid(&sr);
            let dc = centroid(&dr);
            contour_pairs.push((sr, dr, sc, dc, s_closed && t_closed));
        }
    }
    if contour_pairs.is_empty() {
        let s_closed = src_world_path.subpaths.first().is_none_or(|s| s.closed);
        let Some(sr) = resample_primary_contour(src_world_path, SAMPLES) else {
            return Vec::new();
        };
        let Some(dr) = resample_primary_contour(&target_world_path, SAMPLES) else {
            return Vec::new();
        };
        let sc = centroid(&sr);
        let dc = centroid(&dr);
        contour_pairs.push((sr, dr, sc, dc, s_closed));
    }

    let src_root = centroid(&contour_pairs.iter().map(|p| p.2).collect::<Vec<_>>());
    let dst_root = centroid(&contour_pairs.iter().map(|p| p.3).collect::<Vec<_>>());
    let host_path = along_path.and_then(|pid| doc.world_path_of(pid));

    let src_fill = primary_fill_color(&src_obj.fill);
    let dst_fill = primary_fill_color(&target_obj.fill);
    let rot_total_rad = sanitize_f64(rotation_deg, 0.0).to_radians();
    let has_outline = src_obj.outline.is_visible() || target_obj.outline.is_visible();

    let mut out = Vec::with_capacity(n_steps as usize);
    for i in 1..=n_steps {
        let t = f64::from(i) / f64::from(n_steps + 1);
        let tf = t as f32;
        let center_lerp = src_root.lerp(dst_root, t);
        let (target_center, path_tangent_rad) = host_path
            .as_ref()
            .and_then(|hp| hp.point_and_tangent_at_t(t))
            .map_or((center_lerp, 0.0), |(pt, tangent)| {
                let tang_ang = if rotate_along_path && tangent.length_sq() > 1e-12 {
                    tangent.y.atan2(tangent.x)
                } else {
                    0.0
                };
                (pt, tang_ang)
            });
        let angle = rot_total_rad * t + path_tangent_rad;

        let mut step_subpaths = Vec::with_capacity(contour_pairs.len());
        for (src_ring, dst_ring, _, _, is_closed) in &contour_pairs {
            let mut pts = Vec::with_capacity(SAMPLES);
            for k in 0..SAMPLES {
                let local_src = src_ring[k] - src_root;
                let local_dst = dst_ring[k] - dst_root;
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
            step_subpaths.push(coree_core::fit_cubic_bezier_subpath(&pts, *is_closed, 0.03));
        }

        let w_mm = if has_outline {
            let w = src_obj.outline.effective_width_mm() * (1.0 - t)
                + target_obj.outline.effective_width_mm() * t;
            (w > 1e-5).then_some(w)
        } else {
            None
        };
        out.push(ComputedBlendStep {
            path: BezierPath {
                subpaths: step_subpaths,
            },
            fill: src_fill.lerp_icc(&dst_fill, tf, icc),
            opacity: src_obj.opacity + (target_obj.opacity - src_obj.opacity) * tf,
            outline_width_mm: w_mm,
            outline_color: src_obj
                .outline
                .color
                .lerp_icc(&target_obj.outline.color, tf, icc),
        });
    }
    out
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
    rotate_along_path: bool,
    page_to_screen: Affine2D,
    px_per_mm: f64,
    anti_alias: bool,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    for step in compute_blend_steps(
        doc,
        src_obj,
        src_world_path,
        target_id,
        steps,
        rotation_deg,
        along_path,
        rotate_along_path,
        icc,
    ) {
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(icc_lerp_result_to_srgb(&step.fill, icc)),
            opacity: step.opacity,
            blend_mode: src_obj.blend_mode,
            anti_alias,
        };
        backend.fill_path(&step.path, &paint, FillRule::Winding, page_to_screen);

        if let Some(w_mm) = step.outline_width_mm {
            let mut st = StrokeParams::from_outline(
                &src_obj.outline,
                (w_mm as f32).max((0.5 / px_per_mm.max(1e-3)) as f32),
                step.opacity,
                src_obj.blend_mode,
                anti_alias,
            );
            st.color = icc_lerp_result_to_srgb(&step.outline_color, icc);
            backend.stroke_path(&step.path, &st, page_to_screen);
        }
    }
}

/// Applies Envelope bilinear/Coons cage deformation to `path` (supports 4-point, 8-point
/// quadratic, and 12-point 4-curve cubic Bézier Coons patch cages).
#[must_use]
pub fn apply_envelope_to_path(
    path: &BezierPath,
    mode: EnvelopeMode,
    cage: &[Point2D],
) -> BezierPath {
    if cage.len() < 4 || path.is_empty() || !cage.iter().all(|p| p.is_finite()) {
        return path.clone();
    }
    let Some(bb) = path.bounding_box() else {
        return path.clone();
    };
    let w = bb.width().max(1e-6);
    let h = bb.height().max(1e-6);

    let map_pt = |p: Point2D| -> Point2D {
        let u = ((p.x - bb.min_x) / w).clamp(0.0, 1.0);
        let v = ((p.y - bb.min_y) / h).clamp(0.0, 1.0);
        if cage.len() >= 12 {
            // 12-point 4-curve cubic Bézier Coons patch:
            // [0:TL, 1:T1, 2:T2, 3:TR, 4:R1, 5:R2, 6:BR, 7:B1, 8:B2, 9:BL, 10:L1, 11:L2]
            let (tl, tr, br, bl) = (cage[0], cage[3], cage[6], cage[9]);
            if mode == EnvelopeMode::Straight {
                return tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            }
            let top = eval_cubic(tl, cage[1], cage[2], tr, u);
            let bottom = eval_cubic(bl, cage[8], cage[7], br, u);
            let left = eval_cubic(tl, cage[11], cage[10], bl, v);
            let right = eval_cubic(tr, cage[4], cage[5], br, v);
            let ruled_u = top.lerp(bottom, v);
            let ruled_v = left.lerp(right, u);
            let bilin = tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            Point2D::new(
                ruled_u.x + ruled_v.x - bilin.x,
                ruled_u.y + ruled_v.y - bilin.y,
            )
        } else if cage.len() >= 8 {
            let (tl, tr, br, bl) = (cage[0], cage[2], cage[4], cage[6]);
            if mode == EnvelopeMode::Straight {
                return tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            }
            let (tm, rm, bm, lm) = (cage[1], cage[3], cage[5], cage[7]);
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
            let (tl, tr, br, bl) = (cage[0], cage[1], cage[2], cage[3]);
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

/// Applies Envelope bilinear/Coons cage deformation to `path`.
fn apply_envelope_to_path_in_bounds(
    path: &BezierPath,
    ref_bounds: Option<coree_core::Rect2D>,
    mode: EnvelopeMode,
    cage: &[Point2D],
) -> BezierPath {
    if cage.len() < 4 || path.is_empty() || !cage.iter().all(|p| p.is_finite()) {
        return path.clone();
    }
    let Some(bb) = ref_bounds
        .filter(|r| r.is_finite())
        .or_else(|| path.bounding_box())
    else {
        return path.clone();
    };
    let w = bb.width().max(1e-6);
    let h = bb.height().max(1e-6);

    let map_pt = |p: Point2D| -> Point2D {
        let u = ((p.x - bb.min_x) / w).clamp(0.0, 1.0);
        let v = ((p.y - bb.min_y) / h).clamp(0.0, 1.0);
        if cage.len() >= 12 {
            // 12-point 4-curve cubic Bézier Coons patch:
            // [0:TL, 1:T1, 2:T2, 3:TR, 4:R1, 5:R2, 6:BR, 7:B1, 8:B2, 9:BL, 10:L1, 11:L2]
            let (tl, tr, br, bl) = (cage[0], cage[3], cage[6], cage[9]);
            if mode == EnvelopeMode::Straight {
                return tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            }
            let top = eval_cubic(tl, cage[1], cage[2], tr, u);
            let bottom = eval_cubic(bl, cage[8], cage[7], br, u);
            let left = eval_cubic(tl, cage[11], cage[10], bl, v);
            let right = eval_cubic(tr, cage[4], cage[5], br, v);
            let ruled_u = top.lerp(bottom, v);
            let ruled_v = left.lerp(right, u);
            let bilin = tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            Point2D::new(
                ruled_u.x + ruled_v.x - bilin.x,
                ruled_u.y + ruled_v.y - bilin.y,
            )
        } else if cage.len() >= 8 {
            // Quadratic Coons patch through 8-point envelope cage [TL, T_mid, TR, R_mid, BR, B_mid, BL, L_mid]
            let (tl, tr, br, bl) = (cage[0], cage[2], cage[4], cage[6]);
            if mode == EnvelopeMode::Straight {
                return tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
            }
            let (tm, rm, bm, lm) = (cage[1], cage[3], cage[5], cage[7]);
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
            let (tl, tr, br, bl) = (cage[0], cage[1], cage[2], cage[3]);
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
fn apply_perspective_to_path_in_bounds(
    path: &BezierPath,
    ref_bounds: Option<coree_core::Rect2D>,
    quad: &[Point2D; 4],
) -> BezierPath {
    if path.is_empty() || !quad.iter().all(|p| p.is_finite()) {
        return path.clone();
    }
    let Some(bb) = ref_bounds
        .filter(|r| r.is_finite())
        .or_else(|| path.bounding_box())
    else {
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
    let h = Perspective3x3::from_unit_square_to_quad(q)?;
    let d = h.m[8];
    if d.abs() <= 1e-10 {
        return None;
    }
    Some([
        h.m[0] / d,
        h.m[1] / d,
        h.m[2] / d,
        h.m[3] / d,
        h.m[4] / d,
        h.m[5] / d,
        h.m[6] / d,
        h.m[7] / d,
    ])
}

fn eval_quad(p0: Point2D, p1: Point2D, p2: Point2D, t: f64) -> Point2D {
    let it = 1.0 - t;
    Point2D::new(
        it * it * p0.x + 2.0 * it * t * p1.x + t * t * p2.x,
        it * it * p0.y + 2.0 * it * t * p1.y + t * t * p2.y,
    )
}

fn eval_cubic(p0: Point2D, p1: Point2D, p2: Point2D, p3: Point2D, t: f64) -> Point2D {
    let it = 1.0 - t;
    let it2 = it * it;
    let t2 = t * t;
    let b0 = it2 * it;
    let b1 = 3.0 * it2 * t;
    let b2 = 3.0 * it * t2;
    let b3 = t2 * t;
    Point2D::new(
        b0 * p0.x + b1 * p1.x + b2 * p2.x + b3 * p3.x,
        b0 * p0.y + b1 * p1.y + b2 * p2.y + b3 * p3.y,
    )
}

fn subdivide_for_deformation(path: &BezierPath, max_seg_mm: f64) -> BezierPath {
    let mut out = path.clone();
    let max_len = max_seg_mm.max(0.35);
    for sp_idx in 0..out.subpaths.len() {
        let mut guard = 0usize;
        while guard < 512 {
            guard += 1;
            let seg_opt = {
                let sp = &out.subpaths[sp_idx];
                (0..sp.segment_count()).find(|&s| {
                    sp.segment_node_indices(s).is_some_and(|(i0, i1)| {
                        let a = &sp.nodes[i0];
                        let b = &sp.nodes[i1];
                        let chord = a.pos.distance_to(b.pos);
                        if chord > max_len {
                            return true;
                        }
                        let c1_len = a.handle_out.map_or(0.0, |h| a.pos.distance_to(h));
                        let c2_len = b.handle_in.map_or(0.0, |h| b.pos.distance_to(h));
                        c1_len + c2_len > max_len * 1.15
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
    let base = match &obj.payload {
        NodePayload::Path(p) => Some(p.clone()),
        NodePayload::Shape(s) => Some(s.to_bezier_path()),
        NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
        NodePayload::ArtisticMedia { spine, style } => {
            Some(ArtisticMedia::expand_stroke(spine, style))
        }
        NodePayload::PowerClip { container, .. } => Some((**container).clone()),
        _ => None,
    }?;
    Some(if let Some(persp) = obj.perspective {
        persp.transform_bezier_path(&base)
    } else {
        base
    })
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

/// Breaks all non-destructive [`Effect`]s on `obj` apart into standalone editable vector
/// [`SceneObject`] primitives (underlay shadows/extrusions/contours/blends, deformed base shape,
/// overlay inside-contours/bevels, and Symmetry copies), matching `Ctrl+K` (*Break
/// Effect Group Apart*).
///
/// If `obj` has no effects, returns an empty vector.
#[must_use]
pub fn lower_effects_to_primitives(
    doc: &Document,
    obj: &SceneObject,
    world_xf: Affine2D,
    alloc_id: &mut dyn FnMut() -> coree_core::ObjectId,
) -> Vec<SceneObject> {
    if obj.effects.is_empty() {
        return Vec::new();
    }
    let Some(local_base) = extract_local_geometry_path(obj) else {
        return Vec::new();
    };
    let deformed_local = sanitize_bezier_path(apply_geometry_effects(&local_base, &obj.effects));
    let sym_transforms = symmetry_effect_transforms(&obj.effects);
    let icc = doc.icc_pipeline();
    let mut out = Vec::new();

    for (sym_idx, sym_xf) in sym_transforms.into_iter().enumerate() {
        let effective_world_xf = sym_xf.compose(&world_xf);
        let world_path = deformed_local.transformed(&effective_world_xf);
        let suffix = if sym_idx > 0 {
            format!(" #{sym_idx}")
        } else {
            String::new()
        };

        // 1. Underlay primitives (DropShadow soft vector approximation, BlockShadow, Contour::Outside, Extrude25D, Blend)
        for eff in &obj.effects {
            match eff {
                Effect::DropShadow {
                    dx_mm,
                    dy_mm,
                    blur_mm,
                    opacity,
                    color,
                    ..
                } => {
                    let shadow_xf =
                        Affine2D::translate(sanitize_f64(*dx_mm, 0.0), sanitize_f64(*dy_mm, 0.0));
                    let spread = sanitize_f64(*blur_mm, 0.0).max(0.0) * 0.25;
                    let shadow_path = if spread > 0.05 {
                        offset_polygon(&world_path, spread)
                            .unwrap_or_else(|_| world_path.clone())
                            .transformed(&shadow_xf)
                    } else {
                        world_path.transformed(&shadow_xf)
                    };
                    let mut sh = SceneObject::new(
                        alloc_id(),
                        format!("{} DropShadow{suffix}", obj.name),
                        NodePayload::Path(shadow_path),
                    );
                    sh.fill = Fill::Solid(color.clone());
                    sh.outline = OutlineStyle::none();
                    sh.opacity = (obj.opacity * sanitize_f32(*opacity, 0.5)).clamp(0.05, 1.0);
                    out.push(sh);
                }
                Effect::BlockShadow {
                    dx_mm,
                    dy_mm,
                    color,
                    simplify,
                    overprint,
                } => {
                    let delta = Vec2D::new(sanitize_f64(*dx_mm, 0.0), sanitize_f64(*dy_mm, 0.0));
                    let mut swept = build_block_shadow_geometry(&world_path, delta);
                    if *simplify && !swept.subpaths.is_empty() {
                        let pieces: Vec<BezierPath> = swept
                            .subpaths
                            .iter()
                            .cloned()
                            .map(BezierPath::from_subpath)
                            .collect();
                        if let Ok(welded) = apply_boolean_many(BooleanOp::Weld, &pieces) {
                            let mut merged = Vec::new();
                            for w in welded {
                                merged.extend(w.subpaths);
                            }
                            if !merged.is_empty() {
                                swept = BezierPath { subpaths: merged };
                            }
                        }
                    }
                    if !swept.is_empty() {
                        let mut bs = SceneObject::new(
                            alloc_id(),
                            format!("{} BlockShadow{suffix}", obj.name),
                            NodePayload::Path(swept),
                        );
                        bs.fill = Fill::Solid(color.clone());
                        bs.outline = OutlineStyle::none();
                        bs.opacity = obj.opacity;
                        if *overprint {
                            bs.blend_mode = BlendMode::Multiply;
                        }
                        out.push(bs);
                    }
                }
                Effect::Contour {
                    mode: ContourMode::Outside,
                    steps,
                    offset_mm,
                    fill_color,
                    outline_color,
                } => {
                    emit_lowered_contour_objects(
                        &world_path,
                        ContourMode::Outside,
                        *steps,
                        *offset_mm,
                        obj,
                        fill_color,
                        outline_color,
                        &suffix,
                        &icc,
                        alloc_id,
                        &mut out,
                    );
                }
                Effect::Extrude25D {
                    vanishing_point,
                    depth,
                    shade_color,
                    light_dir,
                    ..
                } => {
                    emit_lowered_extrude_objects(
                        &world_path,
                        obj,
                        *vanishing_point,
                        *depth,
                        shade_color,
                        *light_dir,
                        &suffix,
                        &icc,
                        alloc_id,
                        &mut out,
                    );
                }
                Effect::Extrude3D {
                    vanishing_point,
                    depth,
                    bevel_depth: _,
                    shade_color,
                    rotation_3d,
                    lights,
                } => {
                    for (quad, shade) in compute_extrude_3d_faces(
                        &world_path,
                        shade_color,
                        *vanishing_point,
                        *depth,
                        *rotation_3d,
                        lights,
                        &icc,
                    ) {
                        let mut face = SceneObject::new(
                            alloc_id(),
                            format!("{} Extrude3D{suffix}", obj.name),
                            NodePayload::Path(BezierPath::from_polygon(quad, true)),
                        );
                        face.fill = Fill::Solid(shade);
                        face.outline = OutlineStyle::none();
                        face.opacity = obj.opacity;
                        out.push(face);
                    }
                }
                Effect::Blend {
                    target_id,
                    steps,
                    rotation_deg,
                    along_path,
                    rotate_along_path,
                } => {
                    emit_lowered_blend_objects(
                        doc,
                        obj,
                        &world_path,
                        *target_id,
                        *steps,
                        *rotation_deg,
                        *along_path,
                        *rotate_along_path,
                        &suffix,
                        &icc,
                        alloc_id,
                        &mut out,
                    );
                }
                _ => {}
            }
        }

        // 2. Primary object converted to deformed editable Path with live effects cleared
        let mut base = SceneObject::new(
            alloc_id(),
            format!("{}{suffix}", obj.name),
            NodePayload::Path(world_path.clone()),
        );
        base.fill = obj.fill.clone();
        base.outline = obj.outline.clone();
        base.lens = obj.lens.clone();
        base.blend_mode = obj.blend_mode;
        base.opacity = obj.opacity;
        base.visible = obj.visible;
        base.locked = obj.locked;
        base.printable = obj.printable;
        base.style_id = obj.style_id;
        out.push(base);

        // 3. Overlay primitives (Contour::Inside / ToCenter, Extrude25D bevel highlight)
        for eff in &obj.effects {
            match eff {
                Effect::Contour {
                    mode: mode @ (ContourMode::Inside | ContourMode::ToCenter),
                    steps,
                    offset_mm,
                    fill_color,
                    outline_color,
                } => {
                    emit_lowered_contour_objects(
                        &world_path,
                        *mode,
                        *steps,
                        *offset_mm,
                        obj,
                        fill_color,
                        outline_color,
                        &suffix,
                        &icc,
                        alloc_id,
                        &mut out,
                    );
                }
                Effect::Extrude25D { bevel_depth, .. } | Effect::Extrude3D { bevel_depth, .. } => {
                    let bevel = sanitize_f64(*bevel_depth, 0.0).max(0.0);
                    if bevel > 0.05 && !world_path.is_empty() {
                        if let Ok(inner) = offset_polygon(&world_path, -bevel.min(4.0)) {
                            if !inner.is_empty() {
                                let base_color = primary_fill_color(&obj.fill);
                                let mut bv = SceneObject::new(
                                    alloc_id(),
                                    format!("{} Bevel{suffix}", obj.name),
                                    NodePayload::Path(inner),
                                );
                                bv.fill =
                                    Fill::Solid(base_color.lerp_icc(&Color::WHITE, 0.35, &icc));
                                bv.outline = OutlineStyle::none();
                                bv.opacity = obj.opacity * 0.75;
                                out.push(bv);
                            }
                        }
                    }
                }
                Effect::Bevel {
                    distance_mm,
                    highlight_color,
                    intensity,
                    ..
                } => {
                    let bevel = sanitize_f64(*distance_mm, 0.0).max(0.0);
                    if bevel > 0.05 && !world_path.is_empty() {
                        if let Ok(inner) = offset_polygon(&world_path, -bevel.min(6.0)) {
                            if !inner.is_empty() {
                                let mut bv = SceneObject::new(
                                    alloc_id(),
                                    format!("{} Bevel{suffix}", obj.name),
                                    NodePayload::Path(inner),
                                );
                                bv.fill = Fill::Solid(highlight_color.clone());
                                bv.outline = OutlineStyle::none();
                                bv.opacity =
                                    (obj.opacity * sanitize_f32(*intensity, 0.5)).clamp(0.05, 1.0);
                                out.push(bv);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn emit_lowered_contour_objects(
    world_path: &BezierPath,
    mode: ContourMode,
    steps: u32,
    offset_mm: f64,
    obj: &SceneObject,
    target_fill: &Color,
    target_outline: &Color,
    suffix: &str,
    icc: &IccColorPipeline,
    alloc_id: &mut dyn FnMut() -> coree_core::ObjectId,
    out: &mut Vec<SceneObject>,
) {
    let base_fill = primary_fill_color(&obj.fill);
    for (k, ring, step_fill, out_col) in compute_contour_rings(
        world_path,
        mode,
        steps,
        offset_mm,
        &base_fill,
        target_fill,
        &obj.outline,
        target_outline,
        icc,
    ) {
        let mut co = SceneObject::new(
            alloc_id(),
            format!("{} Contour {k}{suffix}", obj.name),
            NodePayload::Path(ring),
        );
        co.fill = Fill::Solid(step_fill);
        co.outline = if obj.outline.is_visible() || target_outline.alpha() > 1e-4 {
            let mut st = obj.outline.clone();
            st.color = out_col;
            st.width_mm = st.width_mm.max(0.25);
            st
        } else {
            OutlineStyle::none()
        };
        co.opacity = obj.opacity;
        out.push(co);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_lowered_extrude_objects(
    world_path: &BezierPath,
    obj: &SceneObject,
    vanishing_point: Point2D,
    depth: f64,
    shade_color: &Color,
    light_dir: [f32; 3],
    suffix: &str,
    icc: &IccColorPipeline,
    alloc_id: &mut dyn FnMut() -> coree_core::ObjectId,
    out: &mut Vec<SceneObject>,
) {
    let base_color = primary_fill_color(&obj.fill);
    for (idx, (quad, col)) in compute_extrude_25d_faces(
        world_path,
        base_color,
        vanishing_point,
        depth,
        shade_color,
        light_dir,
        icc,
    )
    .into_iter()
    .enumerate()
    {
        let mut ex = SceneObject::new(
            alloc_id(),
            format!("{} Extrude {}{suffix}", obj.name, idx + 1),
            NodePayload::Path(BezierPath::from_polygon(quad, true)),
        );
        ex.fill = Fill::Solid(col);
        ex.outline = OutlineStyle::none();
        ex.opacity = obj.opacity;
        out.push(ex);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_lowered_blend_objects(
    doc: &Document,
    src_obj: &SceneObject,
    src_world_path: &BezierPath,
    target_id: coree_core::ObjectId,
    steps: u32,
    rotation_deg: f64,
    along_path: Option<coree_core::ObjectId>,
    rotate_along_path: bool,
    suffix: &str,
    icc: &IccColorPipeline,
    alloc_id: &mut dyn FnMut() -> coree_core::ObjectId,
    out: &mut Vec<SceneObject>,
) {
    for (i, step) in compute_blend_steps(
        doc,
        src_obj,
        src_world_path,
        target_id,
        steps,
        rotation_deg,
        along_path,
        rotate_along_path,
        icc,
    )
    .into_iter()
    .enumerate()
    {
        let mut bo = SceneObject::new(
            alloc_id(),
            format!("{} Blend {}{suffix}", src_obj.name, i + 1),
            NodePayload::Path(step.path),
        );
        bo.fill = Fill::Solid(step.fill);
        bo.opacity = step.opacity;
        bo.blend_mode = src_obj.blend_mode;
        if let Some(w_mm) = step.outline_width_mm {
            let mut st = src_obj.outline.clone();
            st.width_mm = w_mm;
            st.color = step.outline_color;
            bo.outline = st;
        } else {
            bo.outline = OutlineStyle::none();
        }
        out.push(bo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, ObjectId, ParametricShape};

    #[test]
    fn lower_effects_to_primitives_decomposes_shadow_contour_and_symmetry() {
        let mut doc = Document::new_a4("Effects Lowering");
        let mut next_id = 100u64;
        let mut obj = SceneObject::new(
            ObjectId(1),
            "Logo Badge",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 40.0,
                height: 20.0,
                corner_radii: [2.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        );
        obj.effects.push(Effect::DropShadow {
            dx_mm: 2.0,
            dy_mm: 2.0,
            blur_mm: 1.5,
            opacity: 0.6,
            color: Color::BLACK,
            feather_direction: coree_core::FeatherDirection::Outside,
        });
        obj.effects.push(Effect::Contour {
            mode: ContourMode::Outside,
            steps: 2,
            offset_mm: 2.5,
            fill_color: Color::CYAN,
            outline_color: Color::BLACK,
        });
        obj.effects.push(Effect::Symmetry {
            axes: 2,
            mirror: false,
            origin: Point2D::ZERO,
        });
        doc.add_object(obj.clone(), None).unwrap();

        let pieces = lower_effects_to_primitives(
            &doc,
            doc.object(obj.id).unwrap(),
            Affine2D::IDENTITY,
            &mut || {
                let id = ObjectId(next_id);
                next_id += 1;
                id
            },
        );
        // Symmetry=2 * (DropShadow + 2 Contour rings + base Path) = 8 independent vector objects
        assert_eq!(pieces.len(), 8);
        assert!(pieces.iter().all(|p| p.effects.is_empty()));
        assert!(pieces
            .iter()
            .any(|p| p.name.contains("DropShadow") && matches!(p.payload, NodePayload::Path(_))));
        assert!(pieces.iter().any(|p| p.name.contains("Contour")));
    }

    #[test]
    fn add01_inner_shadow_bevel_and_extrude3d_render_cleanly() {
        let mut backend = TinySkiaBackend::new(160, 160).unwrap();
        backend.clear(Color::WHITE);
        let rect_path =
            BezierPath::from_rect(coree_core::Rect2D::from_xywh(10.0, 10.0, 60.0, 60.0));
        let mut obj = SceneObject::new(
            ObjectId(1),
            "ADD-01 Test",
            NodePayload::Path(rect_path.clone()),
        );
        obj.fill = Fill::Solid(Color::CYAN);
        obj.effects.push(Effect::Extrude3D {
            vanishing_point: Point2D::new(90.0, 10.0),
            depth: 12.0,
            bevel_depth: 1.0,
            shade_color: Color::rgb(0.2, 0.5, 0.9),
            rotation_3d: [12.0, -18.0, 0.0],
            lights: vec![ExtrudeLight {
                dir: [0.5, -0.5, 0.7],
                intensity: 1.0,
            }],
        });
        obj.effects.push(Effect::InnerShadow {
            dx_mm: 4.0,
            dy_mm: 4.0,
            blur_mm: 2.0,
            opacity: 0.7,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Inside,
        });
        obj.effects.push(Effect::Bevel {
            style: BevelStyle::SoftEdge,
            distance_mm: 4.0,
            softness_mm: 1.0,
            light_angle_deg: 135.0,
            altitude_deg: 45.0,
            highlight_color: Color::WHITE,
            shadow_color: Color::BLACK,
            intensity: 0.85,
        });

        let xf = Affine2D::scale(2.0, 2.0);
        let icc = IccColorPipeline::default();
        render_underlay_effects(
            &Document::new_a4("E"),
            &obj,
            &rect_path,
            Affine2D::IDENTITY,
            xf,
            xf,
            2.0,
            true,
            &icc,
            &mut backend,
        );
        backend.fill_path(
            &rect_path,
            &BackendPaint::solid(Color::CYAN),
            FillRule::EvenOdd,
            xf,
        );
        render_overlay_effects(
            &obj,
            &rect_path,
            Affine2D::IDENTITY,
            xf,
            2.0,
            true,
            &icc,
            &mut backend,
        );
        let rgba = backend.into_rgba().rgba;
        assert!(rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|px| px[0] < 240 && px[3] > 200));
    }

    #[test]
    fn w3a_envelope_12pt_blend_tangent_contour_gt48_blockshadow_simplify_and_transparency() {
        let mut rect = BezierPath::from_rect(coree_core::Rect2D::new(0.0, 0.0, 40.0, 20.0));
        rect.subpaths[0].closed = true;
        // 12-pt Coons envelope cage with bowed top curve
        let cage = vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(13.0, -8.0),
            Point2D::new(27.0, -8.0),
            Point2D::new(40.0, 0.0),
            Point2D::new(40.0, 6.0),
            Point2D::new(40.0, 14.0),
            Point2D::new(40.0, 20.0),
            Point2D::new(27.0, 20.0),
            Point2D::new(13.0, 20.0),
            Point2D::new(0.0, 20.0),
            Point2D::new(0.0, 14.0),
            Point2D::new(0.0, 6.0),
        ];
        let env = apply_envelope_to_path(&rect, EnvelopeMode::SingleArc, &cage);
        let env_bb = env.bounding_box().unwrap();
        assert!(env_bb.min_y < -2.0, "12pt envelope bow missing: {env_bb:?}");

        // Contour > 48 steps lowers without clamping at 48
        let mut doc = Document::new_a4("Eff");
        let r1 = SceneObject::new(
            ObjectId(1),
            "R1",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 30.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: coree_core::CornerStyle::Rounded,
                lock_corners: true,
            }),
        );
        let id1 = doc.add_object(r1, None).unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: id1,
                effect: Effect::Contour {
                    mode: ContourMode::Outside,
                    steps: 64,
                    offset_mm: 0.4,
                    fill_color: Color::rgb(1.0, 0.0, 0.0),
                    outline_color: Color::BLACK,
                },
            },
        )
        .unwrap();
        let mut next = 500u64;
        let lowered = lower_effects_to_primitives(
            &doc,
            doc.object(id1).unwrap(),
            Affine2D::IDENTITY,
            &mut || {
                next += 1;
                ObjectId(next)
            },
        );
        assert!(
            lowered.len() >= 64,
            "expected >= 64 contour steps, got {}",
            lowered.len()
        );

        // BlockShadow simplify=true vs simplify=false
        let bs_plain = Effect::BlockShadow {
            dx_mm: 6.0,
            dy_mm: 6.0,
            color: Color::BLACK,
            simplify: false,
            overprint: false,
        };
        let bs_simp = Effect::BlockShadow {
            dx_mm: 6.0,
            dy_mm: 6.0,
            color: Color::BLACK,
            simplify: true,
            overprint: false,
        };
        assert_ne!(bs_plain, bs_simp);
    }
}
