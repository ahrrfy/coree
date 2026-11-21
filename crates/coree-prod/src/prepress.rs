// SPDX-License-Identifier: MIT OR Apache-2.0

//! Prepress engine: CMYK + Spot color separations, subtractive overprint simulation,
//! print imposition (`N-Up`, `BookletSaddleStitch`, `Tiling`), and vector print marks
//! (crop marks, bleed marks, registration targets, CMYK color bars, and page slugs).

use crate::error::{ProdError, ProdResult};
use crate::scene_helpers::{
    bezier_to_skia_path, clone_document_shell, clone_page_contents_into, collect_effective_layers,
    find_or_create_layer, get_page, MAX_SCENE_DEPTH,
};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, Document, EllipseMode, Fill, LayerId,
    LineCap, LineJoin, NodePayload, ObjectId, OutlineStyle, PageId, ParametricShape, Point2D,
    Rect2D, SceneObject, TextStyle,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// Standard layer name used by [`generate_print_marks`].
pub const PRINT_MARKS_LAYER_NAME: &str = "Print Marks";

/// Standard layer name used for tiling overlap/alignment marks.
pub const TILE_MARKS_LAYER_NAME: &str = "Tile Alignment Marks";

/// Maximum allowed N-Up cells per sheet (guards against memory exhaustion).
pub const MAX_IMPOSITION_SLOTS_PER_SHEET: usize = 10_000;

/// Maximum allowed total output pages/tiles produced by a single [`impose_document`] call.
pub const MAX_IMPOSITION_PAGES: usize = 10_000;

/// Separation plate channel identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SeparationChannel {
    Cyan,
    Magenta,
    Yellow,
    Black,
    Spot(String),
}

impl SeparationChannel {
    /// Human-readable plate name.
    #[must_use]
    pub fn display_name(&self) -> String {
        match self {
            Self::Cyan => "Cyan".to_owned(),
            Self::Magenta => "Magenta".to_owned(),
            Self::Yellow => "Yellow".to_owned(),
            Self::Black => "Black".to_owned(),
            Self::Spot(name) => format!("Spot: {name}"),
        }
    }
}

/// Configuration for [`compute_separations`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SeparationOptions {
    /// Raster preview resolution in DPI (default: `72.0`).
    pub dpi: f64,
    /// Upper cap on width/height in pixels to keep memory bounded (default: `1024`).
    pub max_dimension_px: u32,
    /// Whether objects on cut layers (`is_cut_layer == true`) are separated onto their spot plate.
    pub include_cut_layers: bool,
}

impl Default for SeparationOptions {
    fn default() -> Self {
        Self {
            dpi: 72.0,
            max_dimension_px: 1024,
            include_cut_layers: true,
        }
    }
}

/// A single ink separation plate with grayscale density map and coverage statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeparationPlate {
    pub channel: SeparationChannel,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Grayscale plate map (`0` = no ink / blank paper, `255` = 100% solid ink).
    pub ink_map: Vec<u8>,
    /// Mean ink coverage across the entire page (`0.0..=100.0`%).
    pub average_coverage_pct: f64,
    /// Peak ink coverage observed at any pixel (`0.0..=100.0`%).
    pub max_coverage_pct: f64,
    /// Percentage of pixels receiving any non-zero ink (`0.0..=100.0`%).
    pub inked_area_pct: f64,
    /// Process CMYK fallback `[c, m, y, k]` for spot ink simulation.
    pub spot_fallback_cmyk: Option<[f32; 4]>,
}

/// Complete CMYK + Spot separation analysis for a single document page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeparationResult {
    pub page_id: PageId,
    pub width: u32,
    pub height: u32,
    pub plates: Vec<SeparationPlate>,
    /// Maximum process Total Area Coverage (`C + M + Y + K` in %, `0.0..=400.0`).
    pub max_tac_pct: f64,
    /// Mean process Total Area Coverage across the page (`0.0..=400.0`).
    pub mean_tac_pct: f64,
    /// Percentage of pixels whose process TAC exceeds `300.0%`.
    pub over_tac_300_pct: f64,
}

impl SeparationResult {
    /// Finds a separation plate by channel.
    #[must_use]
    pub fn plate(&self, channel: &SeparationChannel) -> Option<&SeparationPlate> {
        self.plates.iter().find(|p| &p.channel == channel)
    }

    /// Finds a named spot plate (case-sensitive).
    #[must_use]
    pub fn spot_plate(&self, name: &str) -> Option<&SeparationPlate> {
        self.plates.iter().find(|p| match &p.channel {
            SeparationChannel::Spot(n) => n == name,
            _ => false,
        })
    }
}

/// Analyzes `doc.pages[page_index]` and generates process (`Cyan`, `Magenta`, `Yellow`, `Black`)
/// plus named `Spot` color separation plates with coverage metrics and TAC statistics.
pub fn compute_separations(
    doc: &Document,
    page_index: usize,
    options: &SeparationOptions,
) -> ProdResult<SeparationResult> {
    let page = get_page(doc, page_index)?;
    validate_positive("page.width_mm", page.width_mm)?;
    validate_positive("page.height_mm", page.height_mm)?;
    if !options.dpi.is_finite() || options.dpi <= 0.0 {
        return Err(ProdError::InvalidParameter(
            "separation DPI must be positive and finite".into(),
        ));
    }
    let max_dim = options.max_dimension_px.clamp(8, 4096);
    let raw_w = (page.width_mm / 25.4 * options.dpi).round().max(1.0);
    let raw_h = (page.height_mm / 25.4 * options.dpi).round().max(1.0);
    let downscale = (f64::from(max_dim) / raw_w.max(raw_h)).min(1.0);
    let width = ((raw_w * downscale).round() as u32).max(1);
    let height = ((raw_h * downscale).round() as u32).max(1);
    let scale_x = (f64::from(width) / page.width_mm) as f32;
    let scale_y = (f64::from(height) / page.height_mm) as f32;

    let effective_layers = collect_effective_layers(doc, page_index);

    // Pass 1: discover all named Spot inks used on visible/printable layers of this page.
    let mut spot_fallbacks: BTreeMap<String, [f32; 4]> = BTreeMap::new();
    for layer in &effective_layers {
        if !layer_included_in_separation(layer, options.include_cut_layers) {
            continue;
        }
        let mut visiting = HashSet::new();
        for &oid in &layer.objects {
            collect_spots_recursive(doc, oid, 0, &mut visiting, &mut spot_fallbacks);
        }
    }

    let spot_names: Vec<String> = spot_fallbacks.keys().cloned().collect();
    let spot_index: BTreeMap<String, usize> = spot_names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), 4 + i))
        .collect();
    let total_plates = 4 + spot_names.len();
    let pixel_count = (width as usize) * (height as usize);
    let mut buffers: Vec<Vec<f32>> = vec![vec![0.0_f32; pixel_count]; total_plates];

    // Pass 2: rasterize objects in z-order into ink separation buffers.
    let mut ctx = SeparationRasterCtx {
        doc,
        width,
        height,
        scale_x,
        scale_y,
        spot_index: &spot_index,
        buffers: &mut buffers,
    };

    for layer in &effective_layers {
        if !layer_included_in_separation(layer, options.include_cut_layers) {
            continue;
        }
        let mut visiting = HashSet::new();
        for &oid in &layer.objects {
            rasterize_object_separation(&mut ctx, oid, Affine2D::IDENTITY, None, 0, &mut visiting);
        }
    }

    // Pass 3: compute TAC across process CMYK plates (indices 0..4) and package results.
    let mut max_tac_pct = 0.0_f64;
    let mut sum_tac_pct = 0.0_f64;
    let mut over_300_count = 0usize;
    for px in 0..pixel_count {
        let tac =
            f64::from(buffers[0][px] + buffers[1][px] + buffers[2][px] + buffers[3][px]) * 100.0;
        if tac > max_tac_pct {
            max_tac_pct = tac;
        }
        sum_tac_pct += tac;
        if tac > 300.0 + 1e-3 {
            over_300_count += 1;
        }
    }
    let mean_tac_pct = if pixel_count > 0 {
        sum_tac_pct / (pixel_count as f64)
    } else {
        0.0
    };
    let over_tac_300_pct = if pixel_count > 0 {
        (over_300_count as f64) / (pixel_count as f64) * 100.0
    } else {
        0.0
    };

    let mut plates = Vec::with_capacity(total_plates);
    let process_channels = [
        SeparationChannel::Cyan,
        SeparationChannel::Magenta,
        SeparationChannel::Yellow,
        SeparationChannel::Black,
    ];
    for (idx, chan) in process_channels.into_iter().enumerate() {
        plates.push(build_plate(chan, width, height, &buffers[idx], None));
    }
    for (i, spot_name) in spot_names.into_iter().enumerate() {
        let fallback = spot_fallbacks.get(&spot_name).copied();
        plates.push(build_plate(
            SeparationChannel::Spot(spot_name),
            width,
            height,
            &buffers[4 + i],
            fallback,
        ));
    }

    Ok(SeparationResult {
        page_id: page.id,
        width,
        height,
        plates,
        max_tac_pct,
        mean_tac_pct,
        over_tac_300_pct,
    })
}

fn layer_included_in_separation(layer: &coree_core::Layer, include_cut_layers: bool) -> bool {
    if !layer.visible {
        return false;
    }
    if layer.is_cut_layer {
        return include_cut_layers;
    }
    layer.printable
}

fn collect_spots_recursive(
    doc: &Document,
    id: ObjectId,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    spots: &mut BTreeMap<String, [f32; 4]>,
) {
    if depth > MAX_SCENE_DEPTH || !visiting.insert(id) {
        return;
    }
    if let Some(obj) = doc.objects.get(&id) {
        if obj.visible && obj.printable {
            for c in obj.fill.referenced_colors() {
                register_spot_color(&c, spots);
            }
            if obj.outline.is_visible() {
                register_spot_color(&obj.outline.color, spots);
            }
            match &obj.payload {
                NodePayload::ArtisticText(t) => {
                    for run in &t.runs {
                        register_spot_color(&run.style.fill, spots);
                    }
                }
                NodePayload::ParagraphText(t) => {
                    for run in &t.runs {
                        register_spot_color(&run.style.fill, spots);
                    }
                }
                NodePayload::TextOnPath(top) => {
                    for run in &top.text.runs {
                        register_spot_color(&run.style.fill, spots);
                    }
                }
                NodePayload::Group(children) => {
                    for &cid in children {
                        collect_spots_recursive(doc, cid, depth + 1, visiting, spots);
                    }
                }
                NodePayload::PowerClip { contents, .. } => {
                    for &cid in contents {
                        collect_spots_recursive(doc, cid, depth + 1, visiting, spots);
                    }
                }
                _ => {}
            }
        }
    }
    visiting.remove(&id);
}

fn register_spot_color(color: &Color, spots: &mut BTreeMap<String, [f32; 4]>) {
    if let Color::Spot {
        name,
        fallback_cmyk,
        ..
    } = color
    {
        if !name.eq_ignore_ascii_case("Registration") {
            spots.entry(name.clone()).or_insert(*fallback_cmyk);
        }
    }
}

struct SeparationRasterCtx<'a> {
    doc: &'a Document,
    width: u32,
    height: u32,
    scale_x: f32,
    scale_y: f32,
    spot_index: &'a BTreeMap<String, usize>,
    buffers: &'a mut [Vec<f32>],
}

fn rasterize_object_separation(
    ctx: &mut SeparationRasterCtx<'_>,
    id: ObjectId,
    parent_xf: Affine2D,
    clip_mask: Option<&[f32]>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
) {
    if depth > MAX_SCENE_DEPTH || !visiting.insert(id) {
        return;
    }
    let Some(obj) = ctx.doc.objects.get(&id) else {
        visiting.remove(&id);
        return;
    };
    if !obj.visible || !obj.printable || !obj.opacity.is_finite() || obj.opacity <= 1e-6 {
        visiting.remove(&id);
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        visiting.remove(&id);
        return;
    }

    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children {
                rasterize_object_separation(ctx, cid, world_xf, clip_mask, depth + 1, visiting);
            }
            visiting.remove(&id);
            return;
        }
        NodePayload::PowerClip {
            container,
            contents,
        } => {
            let world_container = container.transformed(&world_xf);
            // Paint container fill/stroke first if any
            paint_vector_object(ctx, obj, &world_container, &world_xf, clip_mask);
            // Build combined clip mask for clipped contents
            if let Some(container_mask) = rasterize_fill_mask(
                &world_container,
                ctx.width,
                ctx.height,
                ctx.scale_x,
                ctx.scale_y,
            ) {
                let combined_mask: Vec<f32> = match clip_mask {
                    Some(outer) => container_mask
                        .iter()
                        .zip(outer.iter())
                        .map(|(&a, &b)| a * b)
                        .collect(),
                    None => container_mask,
                };
                for &cid in contents {
                    rasterize_object_separation(
                        ctx,
                        cid,
                        world_xf,
                        Some(&combined_mask),
                        depth + 1,
                        visiting,
                    );
                }
            }
            visiting.remove(&id);
            return;
        }
        NodePayload::Raster(raster) => {
            paint_raster_object(ctx, obj, raster, &world_xf, clip_mask);
            visiting.remove(&id);
            return;
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = ctx.doc.symbols.get(sym_id) {
                for sub in &sym.objects {
                    if !sub.visible || !sub.printable || sub.opacity <= 1e-6 {
                        continue;
                    }
                    let sub_xf = world_xf.compose(&sub.transform);
                    let sub_path = match &sub.payload {
                        NodePayload::Path(p) => Some(p.transformed(&sub_xf)),
                        NodePayload::Shape(s) => Some(s.to_bezier_path().transformed(&sub_xf)),
                        _ => None,
                    };
                    if let Some(path) = sub_path {
                        paint_vector_object(ctx, sub, &path, &sub_xf, clip_mask);
                    }
                }
            }
            visiting.remove(&id);
            return;
        }
        _ => {}
    }

    let world_path = match &obj.payload {
        NodePayload::Path(p) => Some(p.transformed(&world_xf)),
        NodePayload::Shape(s) => Some(s.to_bezier_path().transformed(&world_xf)),
        NodePayload::Table(t) => Some(t.to_grid_paths().transformed(&world_xf)),
        NodePayload::ArtisticText(t) => {
            Some(BezierPath::from_rect(t.estimated_bounds()).transformed(&world_xf))
        }
        NodePayload::ParagraphText(t) => {
            Some(BezierPath::from_rect(t.bounding_box()).transformed(&world_xf))
        }
        NodePayload::TextOnPath(top) => {
            Some(BezierPath::from_rect(top.text.estimated_bounds()).transformed(&world_xf))
        }
        NodePayload::Dimension(d) => {
            Some(BezierPath::from_rect(d.bounding_box()).transformed(&world_xf))
        }
        NodePayload::Connector(c) => {
            let (a, b) = ctx
                .doc
                .resolve_connector_endpoints_with_transform(c, &world_xf);
            Some(c.route_path(a, b))
        }
        _ => None,
    };

    if let Some(path) = world_path {
        paint_vector_object(ctx, obj, &path, &world_xf, clip_mask);
    }

    visiting.remove(&id);
}

fn paint_vector_object(
    ctx: &mut SeparationRasterCtx<'_>,
    obj: &SceneObject,
    world_path: &BezierPath,
    world_xf: &Affine2D,
    clip_mask: Option<&[f32]>,
) {
    let effective_fill = resolve_effective_fill(obj);
    let fill_overprint = obj.blend_mode == BlendMode::Multiply;
    let stroke_overprint = obj.outline.overprint || obj.blend_mode == BlendMode::Multiply;

    if obj.outline.behind_fill {
        if obj.outline.is_visible() {
            paint_stroke(
                ctx,
                world_path,
                &obj.outline,
                obj.opacity,
                stroke_overprint,
                clip_mask,
            );
        }
        if effective_fill.is_visible() {
            paint_fill(
                ctx,
                world_path,
                world_xf,
                &effective_fill,
                obj.opacity,
                fill_overprint,
                clip_mask,
            );
        }
    } else {
        if effective_fill.is_visible() {
            paint_fill(
                ctx,
                world_path,
                world_xf,
                &effective_fill,
                obj.opacity,
                fill_overprint,
                clip_mask,
            );
        }
        if obj.outline.is_visible() {
            paint_stroke(
                ctx,
                world_path,
                &obj.outline,
                obj.opacity,
                stroke_overprint,
                clip_mask,
            );
        }
    }
}

fn resolve_effective_fill(obj: &SceneObject) -> Fill {
    if obj.fill.is_visible() {
        return obj.fill.clone();
    }
    match &obj.payload {
        NodePayload::ArtisticText(t) => t
            .runs
            .first()
            .map_or(Fill::None, |r| Fill::Solid(r.style.fill.clone())),
        NodePayload::ParagraphText(t) => t
            .runs
            .first()
            .map_or(Fill::None, |r| Fill::Solid(r.style.fill.clone())),
        NodePayload::TextOnPath(top) => top
            .text
            .runs
            .first()
            .map_or(Fill::None, |r| Fill::Solid(r.style.fill.clone())),
        _ => Fill::None,
    }
}

fn paint_fill(
    ctx: &mut SeparationRasterCtx<'_>,
    world_path: &BezierPath,
    world_xf: &Affine2D,
    fill: &Fill,
    opacity: f32,
    overprint: bool,
    clip_mask: Option<&[f32]>,
) {
    let Some(mask) =
        rasterize_fill_mask(world_path, ctx.width, ctx.height, ctx.scale_x, ctx.scale_y)
    else {
        return;
    };

    match fill {
        Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {}
        Fill::Solid(color) => {
            composite_solid_mask(ctx, &mask, color, opacity, overprint, clip_mask);
        }
        Fill::LinearGradient { start, end, stops } => {
            let inv_world = world_xf.inverse();
            let (eval_start, eval_end) = if inv_world.is_some() {
                (*start, *end)
            } else {
                (
                    world_xf.transform_point(*start),
                    world_xf.transform_point(*end),
                )
            };
            let dx = eval_end.x - eval_start.x;
            let dy = eval_end.y - eval_start.y;
            let len_sq = dx * dx + dy * dy;
            let inv_sx = 1.0 / f64::from(ctx.scale_x);
            let inv_sy = 1.0 / f64::from(ctx.scale_y);
            let w = ctx.width as usize;
            for (px, &m) in mask.iter().enumerate() {
                let cov = m * clip_mask.map_or(1.0, |cm| cm[px]);
                if cov <= 1e-5 {
                    continue;
                }
                let world_pt = Point2D::new(
                    ((px % w) as f64 + 0.5) * inv_sx,
                    ((px / w) as f64 + 0.5) * inv_sy,
                );
                let pt = inv_world.map_or(world_pt, |inv| inv.transform_point(world_pt));
                let t = if len_sq > 1e-12 {
                    (((pt.x - eval_start.x) * dx + (pt.y - eval_start.y) * dy) / len_sq)
                        .clamp(0.0, 1.0) as f32
                } else {
                    0.0
                };
                let color = Fill::sample_stops(stops, t);
                composite_pixel_color(
                    ctx.buffers,
                    ctx.spot_index,
                    px,
                    &color,
                    cov * opacity,
                    overprint,
                );
            }
        }
        Fill::RadialGradient {
            center,
            radius,
            stops,
            ..
        } => {
            let inv_world = world_xf.inverse();
            let eval_center = if inv_world.is_some() {
                *center
            } else {
                world_xf.transform_point(*center)
            };
            let r = radius.abs().max(1e-6);
            let inv_sx = 1.0 / f64::from(ctx.scale_x);
            let inv_sy = 1.0 / f64::from(ctx.scale_y);
            let w = ctx.width as usize;
            for (px, &m) in mask.iter().enumerate() {
                let cov = m * clip_mask.map_or(1.0, |cm| cm[px]);
                if cov <= 1e-5 {
                    continue;
                }
                let world_pt = Point2D::new(
                    ((px % w) as f64 + 0.5) * inv_sx,
                    ((px / w) as f64 + 0.5) * inv_sy,
                );
                let pt = inv_world.map_or(world_pt, |inv| inv.transform_point(world_pt));
                let dist = (pt.x - eval_center.x).hypot(pt.y - eval_center.y);
                let t = (dist / r).clamp(0.0, 1.0) as f32;
                let color = Fill::sample_stops(stops, t);
                composite_pixel_color(
                    ctx.buffers,
                    ctx.spot_index,
                    px,
                    &color,
                    cov * opacity,
                    overprint,
                );
            }
        }
        Fill::ConicalGradient { stops, .. } | Fill::SquareGradient { stops, .. } => {
            let color = Fill::sample_stops(stops, 0.5);
            composite_solid_mask(ctx, &mask, &color, opacity, overprint, clip_mask);
        }
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => {
            composite_solid_mask(ctx, &mask, fg, opacity, overprint, clip_mask);
        }
        Fill::MeshFill { nodes, .. } => {
            if let Some(first) = nodes.first() {
                composite_solid_mask(ctx, &mask, &first.color, opacity, overprint, clip_mask);
            }
        }
    }
}

fn paint_stroke(
    ctx: &mut SeparationRasterCtx<'_>,
    world_path: &BezierPath,
    outline: &OutlineStyle,
    opacity: f32,
    overprint: bool,
    clip_mask: Option<&[f32]>,
) {
    let Some(mask) = rasterize_stroke_mask(
        world_path,
        outline,
        ctx.width,
        ctx.height,
        ctx.scale_x,
        ctx.scale_y,
    ) else {
        return;
    };
    composite_solid_mask(ctx, &mask, &outline.color, opacity, overprint, clip_mask);
}

fn paint_raster_object(
    ctx: &mut SeparationRasterCtx<'_>,
    obj: &SceneObject,
    raster: &coree_core::RasterLayerData,
    world_xf: &Affine2D,
    clip_mask: Option<&[f32]>,
) {
    let Some(asset) = ctx.doc.assets.get(&raster.asset_id) else {
        return;
    };
    if asset.width == 0 || asset.height == 0 || raster.width_mm <= 1e-6 || raster.height_mm <= 1e-6
    {
        return;
    }
    let Some(inv_world) = world_xf.inverse() else {
        return;
    };
    let overprint = obj.blend_mode == BlendMode::Multiply;
    let inv_sx = 1.0 / f64::from(ctx.scale_x);
    let inv_sy = 1.0 / f64::from(ctx.scale_y);
    let w = ctx.width as usize;
    let h = ctx.height as usize;

    for py in 0..h {
        let y_mm = (py as f64 + 0.5) * inv_sy;
        for px_x in 0..w {
            let idx = py * w + px_x;
            let clip_a = clip_mask.map_or(1.0, |cm| cm[idx]);
            if clip_a <= 1e-5 {
                continue;
            }
            let x_mm = (px_x as f64 + 0.5) * inv_sx;
            let local = inv_world.transform_point(Point2D::new(x_mm, y_mm));
            if local.x < 0.0
                || local.y < 0.0
                || local.x > raster.width_mm
                || local.y > raster.height_mm
            {
                continue;
            }
            let u = ((local.x / raster.width_mm) * f64::from(asset.width))
                .floor()
                .clamp(0.0, f64::from(asset.width - 1)) as usize;
            let v = ((local.y / raster.height_mm) * f64::from(asset.height))
                .floor()
                .clamp(0.0, f64::from(asset.height - 1)) as usize;
            let offset = (v * (asset.width as usize) + u) * 4;
            if offset + 3 >= asset.rgba_bytes.len() {
                continue;
            }
            let r = asset.rgba_bytes[offset];
            let g = asset.rgba_bytes[offset + 1];
            let b = asset.rgba_bytes[offset + 2];
            let a = asset.rgba_bytes[offset + 3];
            if a == 0 {
                continue;
            }
            let color = Color::rgba8(r, g, b, a);
            composite_pixel_color(
                ctx.buffers,
                ctx.spot_index,
                idx,
                &color,
                clip_a * obj.opacity,
                overprint,
            );
        }
    }
}

fn composite_solid_mask(
    ctx: &mut SeparationRasterCtx<'_>,
    mask: &[f32],
    color: &Color,
    opacity: f32,
    overprint: bool,
    clip_mask: Option<&[f32]>,
) {
    for (px, &m) in mask.iter().enumerate() {
        let cov = m * clip_mask.map_or(1.0, |cm| cm[px]);
        if cov <= 1e-5 {
            continue;
        }
        composite_pixel_color(
            ctx.buffers,
            ctx.spot_index,
            px,
            color,
            cov * opacity,
            overprint,
        );
    }
}

fn composite_pixel_color(
    buffers: &mut [Vec<f32>],
    spot_index: &BTreeMap<String, usize>,
    px: usize,
    color: &Color,
    coverage_opacity: f32,
    overprint: bool,
) {
    let alpha = (coverage_opacity * color.alpha()).clamp(0.0, 1.0);
    if alpha <= 1e-5 {
        return;
    }
    let plate_count = buffers.len();

    match color {
        Color::Spot { name, tint, .. } => {
            let t = tint.clamp(0.0, 1.0);
            if name.eq_ignore_ascii_case("Registration") {
                for buf in buffers.iter_mut().take(4) {
                    if overprint {
                        buf[px] = 1.0 - (1.0 - buf[px]) * (1.0 - t * alpha);
                    } else {
                        buf[px] = buf[px] * (1.0 - alpha) + t * alpha;
                    }
                }
                return;
            }
            let target_idx = spot_index.get(name).copied();
            if overprint {
                if let Some(si) = target_idx {
                    if t > 0.0 {
                        buffers[si][px] = 1.0 - (1.0 - buffers[si][px]) * (1.0 - t * alpha);
                    }
                }
            } else {
                // Knockout: attenuates every other plate, sets this spot plate
                for (i, buf) in buffers.iter_mut().enumerate().take(plate_count) {
                    let src = if Some(i) == target_idx { t } else { 0.0 };
                    buf[px] = buf[px] * (1.0 - alpha) + src * alpha;
                }
            }
        }
        other => {
            let cmyk = other.to_cmyk();
            if overprint {
                // Subtractive overprint: only non-zero process channels add ink; zero channels preserve underlying ink
                for (i, &src) in cmyk.iter().enumerate() {
                    if src > 1e-5 {
                        buffers[i][px] = 1.0 - (1.0 - buffers[i][px]) * (1.0 - src * alpha);
                    }
                }
            } else {
                // Standard knockout across process + spot plates
                for (i, buf) in buffers.iter_mut().enumerate().take(plate_count) {
                    let src = if i < 4 { cmyk[i] } else { 0.0 };
                    buf[px] = buf[px] * (1.0 - alpha) + src * alpha;
                }
            }
        }
    }
}

fn rasterize_fill_mask(
    path: &BezierPath,
    width: u32,
    height: u32,
    scale_x: f32,
    scale_y: f32,
) -> Option<Vec<f32>> {
    let sk_path = bezier_to_skia_path(path, scale_x, scale_y)?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;
    pixmap.fill_path(
        &sk_path,
        &paint,
        tiny_skia::FillRule::EvenOdd,
        tiny_skia::Transform::identity(),
        None,
    );
    Some(
        pixmap
            .pixels()
            .iter()
            .map(|p| f32::from(p.alpha()) / 255.0)
            .collect(),
    )
}

fn rasterize_stroke_mask(
    path: &BezierPath,
    outline: &OutlineStyle,
    width: u32,
    height: u32,
    scale_x: f32,
    scale_y: f32,
) -> Option<Vec<f32>> {
    let sk_path = bezier_to_skia_path(path, scale_x, scale_y)?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;

    let avg_scale = 0.5 * (scale_x + scale_y);
    let mut stroke = tiny_skia::Stroke {
        width: ((outline.effective_width_mm() as f32) * avg_scale).max(1.0),
        miter_limit: (outline.miter_limit as f32).max(1.0),
        line_cap: match outline.cap {
            LineCap::Butt => tiny_skia::LineCap::Butt,
            LineCap::Round => tiny_skia::LineCap::Round,
            LineCap::Square => tiny_skia::LineCap::Square,
        },
        line_join: match outline.join {
            LineJoin::Miter => tiny_skia::LineJoin::Miter,
            LineJoin::Round => tiny_skia::LineJoin::Round,
            LineJoin::Bevel => tiny_skia::LineJoin::Bevel,
        },
        dash: None,
    };
    if !outline.dash_pattern.is_empty() {
        let dashes: Vec<f32> = outline
            .dash_pattern
            .iter()
            .filter(|d| d.is_finite() && **d > 0.0)
            .map(|&d| ((d as f32) * avg_scale).clamp(0.5, 10_000.0))
            .collect();
        if !dashes.is_empty() {
            stroke.dash = tiny_skia::StrokeDash::new(dashes, 0.0);
        }
    }

    pixmap.stroke_path(
        &sk_path,
        &paint,
        &stroke,
        tiny_skia::Transform::identity(),
        None,
    );
    Some(
        pixmap
            .pixels()
            .iter()
            .map(|p| f32::from(p.alpha()) / 255.0)
            .collect(),
    )
}

fn build_plate(
    channel: SeparationChannel,
    width: u32,
    height: u32,
    buf: &[f32],
    spot_fallback_cmyk: Option<[f32; 4]>,
) -> SeparationPlate {
    let mut ink_map = Vec::with_capacity(buf.len());
    let mut sum = 0.0_f64;
    let mut max_val = 0.0_f64;
    let mut inked = 0usize;

    for &v in buf {
        let clamped = f64::from(v.clamp(0.0, 1.0));
        sum += clamped;
        if clamped > max_val {
            max_val = clamped;
        }
        let byte = (clamped * 255.0).round() as u8;
        if byte > 0 {
            inked += 1;
        }
        ink_map.push(byte);
    }

    let n = buf.len().max(1) as f64;
    let name = channel.display_name();
    SeparationPlate {
        channel,
        name,
        width,
        height,
        ink_map,
        average_coverage_pct: (sum / n) * 100.0,
        max_coverage_pct: max_val * 100.0,
        inked_area_pct: (inked as f64 / n) * 100.0,
        spot_fallback_cmyk,
    }
}

/// Simulates physical CMYK + Spot subtractive overprinting into an RGBA8 preview buffer
/// (`width * height * 4` bytes).
#[must_use]
pub fn simulate_overprint_rgba(sep: &SeparationResult) -> Vec<u8> {
    let Some(pixel_count) = (sep.width as usize).checked_mul(sep.height as usize) else {
        return Vec::new();
    };
    let Some(byte_len) = pixel_count.checked_mul(4) else {
        return Vec::new();
    };
    let c_map = sep
        .plate(&SeparationChannel::Cyan)
        .map(|p| p.ink_map.as_slice());
    let m_map = sep
        .plate(&SeparationChannel::Magenta)
        .map(|p| p.ink_map.as_slice());
    let y_map = sep
        .plate(&SeparationChannel::Yellow)
        .map(|p| p.ink_map.as_slice());
    let k_map = sep
        .plate(&SeparationChannel::Black)
        .map(|p| p.ink_map.as_slice());

    let spot_plates: Vec<(&[u8], [f32; 4])> = sep
        .plates
        .iter()
        .filter_map(|p| {
            if matches!(p.channel, SeparationChannel::Spot(_)) {
                let fb = p.spot_fallback_cmyk.unwrap_or([0.0, 1.0, 0.0, 0.0]);
                Some((p.ink_map.as_slice(), fb))
            } else {
                None
            }
        })
        .collect();

    let mut out = vec![0u8; byte_len];
    out.par_chunks_exact_mut(4)
        .enumerate()
        .for_each(|(px, rgba)| {
            let c = c_map.and_then(|b| b.get(px)).copied().unwrap_or(0) as f32 / 255.0;
            let m = m_map.and_then(|b| b.get(px)).copied().unwrap_or(0) as f32 / 255.0;
            let y = y_map.and_then(|b| b.get(px)).copied().unwrap_or(0) as f32 / 255.0;
            let k = k_map.and_then(|b| b.get(px)).copied().unwrap_or(0) as f32 / 255.0;

            let inv_k = 1.0 - k;
            let mut r = (1.0 - c) * inv_k;
            let mut g = (1.0 - m) * inv_k;
            let mut b = (1.0 - y) * inv_k;

            for &(s_map, fb) in &spot_plates {
                let density = s_map.get(px).copied().unwrap_or(0) as f32 / 255.0;
                if density > 0.0 {
                    let s_inv_k = 1.0 - fb[3] * density;
                    let tr = (1.0 - fb[0] * density) * s_inv_k;
                    let tg = (1.0 - fb[1] * density) * s_inv_k;
                    let tb = (1.0 - fb[2] * density) * s_inv_k;
                    r *= tr;
                    g *= tg;
                    b *= tb;
                }
            }

            rgba[0] = (r.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[1] = (g.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[2] = (b.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[3] = 255;
        });
    out
}

// =============================================================================
// Print Imposition: N-Up, Saddle-Stitch Booklet & Large-Format Tiling
// =============================================================================

/// Print sheet imposition layout mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImpositionMode {
    /// Grid imposition (`cols × rows`) for business cards, stickers, and labels.
    ///
    /// A single-page source document is stepped-and-repeated across all `cols * rows` cells;
    /// a multi-page source document flows pages sequentially across sheets.
    NUp {
        cols: u32,
        rows: u32,
        gap_x_mm: f64,
        gap_y_mm: f64,
        sheet_w_mm: f64,
        sheet_h_mm: f64,
    },
    /// Automatic 4-up saddle-stitch booklet signature imposition.
    ///
    /// Pads blank pages (`None`) up to the next multiple of 4 and pairs spreads in
    /// front/back press order (`[N, 1]`, `[2, N-1]`, `[N-2, 3]`, `[4, N-3]`, ...).
    BookletSaddleStitch { sheet_w_mm: f64, sheet_h_mm: f64 },
    /// Splits large-format pages (billboards, banners, signage) into overlapping printable
    /// tiles with automatic alignment overlap marks.
    Tiling {
        tile_w_mm: f64,
        tile_h_mm: f64,
        overlap_mm: f64,
    },
}

/// Computes the 0-based `(left_page, right_page)` spread sequence for saddle-stitch booklet
/// imposition, padding blank pages (`None`) to a multiple of 4.
///
/// Spread order alternates Front, Back for each physical sheet:
/// - 4 pages -> `[(Some(3), Some(0)), (Some(1), Some(2))]`
/// - 6 pages -> `[(None, Some(0)), (Some(1), None), (Some(5), Some(2)), (Some(3), Some(4))]`
/// - 8 pages -> `[(Some(7), Some(0)), (Some(1), Some(6)), (Some(5), Some(2)), (Some(3), Some(4))]`
#[must_use]
pub fn booklet_signature_order(page_count: usize) -> Vec<(Option<usize>, Option<usize>)> {
    if page_count == 0 {
        return Vec::new();
    }
    let total = page_count.div_ceil(4) * 4;
    let sheets = total / 4;
    let page_opt = |one_based: usize| -> Option<usize> {
        if one_based >= 1 && one_based <= page_count {
            Some(one_based - 1)
        } else {
            None
        }
    };

    let mut spreads = Vec::with_capacity(sheets * 2);
    for s in 0..sheets {
        // Sheet Front: [total - 2s, 2s + 1]
        let front_left = total - 2 * s;
        let front_right = 2 * s + 1;
        spreads.push((page_opt(front_left), page_opt(front_right)));

        // Sheet Back: [2s + 2, total - 2s - 1]
        let back_left = 2 * s + 2;
        let back_right = total - 2 * s - 1;
        spreads.push((page_opt(back_left), page_opt(back_right)));
    }
    spreads
}

/// Imposes `doc` into a new press-ready [`Document`] according to `mode`.
pub fn impose_document(doc: &Document, mode: &ImpositionMode) -> ProdResult<Document> {
    if doc.pages.is_empty() {
        return Err(ProdError::EmptyDocument(
            "cannot impose a document with zero pages".into(),
        ));
    }
    match *mode {
        ImpositionMode::NUp {
            cols,
            rows,
            gap_x_mm,
            gap_y_mm,
            sheet_w_mm,
            sheet_h_mm,
        } => impose_nup(doc, cols, rows, gap_x_mm, gap_y_mm, sheet_w_mm, sheet_h_mm),
        ImpositionMode::BookletSaddleStitch {
            sheet_w_mm,
            sheet_h_mm,
        } => impose_booklet(doc, sheet_w_mm, sheet_h_mm),
        ImpositionMode::Tiling {
            tile_w_mm,
            tile_h_mm,
            overlap_mm,
        } => impose_tiling(doc, tile_w_mm, tile_h_mm, overlap_mm),
    }
}

fn validate_positive(name: &str, val: f64) -> ProdResult<()> {
    if !val.is_finite() || val <= 0.0 {
        return Err(ProdError::InvalidParameter(format!(
            "{name} must be positive and finite (got {val})"
        )));
    }
    Ok(())
}

fn validate_non_negative(name: &str, val: f64) -> ProdResult<()> {
    if !val.is_finite() || val < 0.0 {
        return Err(ProdError::InvalidParameter(format!(
            "{name} must be non-negative and finite (got {val})"
        )));
    }
    Ok(())
}

fn impose_nup(
    doc: &Document,
    cols: u32,
    rows: u32,
    gap_x_mm: f64,
    gap_y_mm: f64,
    sheet_w_mm: f64,
    sheet_h_mm: f64,
) -> ProdResult<Document> {
    if cols == 0 || rows == 0 {
        return Err(ProdError::InvalidParameter(
            "N-Up cols and rows must both be >= 1".into(),
        ));
    }
    validate_non_negative("gap_x_mm", gap_x_mm)?;
    validate_non_negative("gap_y_mm", gap_y_mm)?;
    validate_positive("sheet_w_mm", sheet_w_mm)?;
    validate_positive("sheet_h_mm", sheet_h_mm)?;

    let per_sheet = (cols as usize)
        .checked_mul(rows as usize)
        .ok_or_else(|| ProdError::InvalidParameter("N-Up grid overflow".into()))?;
    if per_sheet > MAX_IMPOSITION_SLOTS_PER_SHEET {
        return Err(ProdError::InvalidParameter(format!(
            "N-Up grid {cols}x{rows} ({per_sheet} slots) exceeds safety cap {MAX_IMPOSITION_SLOTS_PER_SHEET}"
        )));
    }
    let src_w = doc.pages[0].width_mm;
    let src_h = doc.pages[0].height_mm;
    validate_positive("source page width_mm", src_w)?;
    validate_positive("source page height_mm", src_h)?;

    let grid_w = f64::from(cols) * src_w + f64::from(cols - 1) * gap_x_mm;
    let grid_h = f64::from(rows) * src_h + f64::from(rows - 1) * gap_y_mm;
    let origin_x = (sheet_w_mm - grid_w) * 0.5;
    let origin_y = (sheet_h_mm - grid_h) * 0.5;

    // Step-and-repeat for 1-page designs; sequential flow for multi-page documents.
    let slot_sources: Vec<usize> = if doc.pages.len() == 1 {
        vec![0; per_sheet]
    } else {
        (0..doc.pages.len()).collect()
    };

    let sheet_count = slot_sources.len().div_ceil(per_sheet);
    if sheet_count > MAX_IMPOSITION_PAGES {
        return Err(ProdError::InvalidParameter(format!(
            "N-Up imposition would produce {sheet_count} sheets (exceeds cap {MAX_IMPOSITION_PAGES})"
        )));
    }
    let mut out = clone_document_shell(doc, format!("{} (N-Up {}x{})", doc.title, cols, rows));

    for sheet_idx in 0..sheet_count {
        out.add_page(
            format!("Sheet {}", sheet_idx + 1),
            sheet_w_mm,
            sheet_h_mm,
            0.0,
        );
        let dst_page_idx = out.pages.len() - 1;

        for slot in 0..per_sheet {
            let global_slot = sheet_idx * per_sheet + slot;
            let Some(&src_page_idx) = slot_sources.get(global_slot) else {
                break;
            };
            let r = (slot / (cols as usize)) as f64;
            let c = (slot % (cols as usize)) as f64;
            let tx = origin_x + c * (src_w + gap_x_mm);
            let ty = origin_y + r * (src_h + gap_y_mm);
            clone_page_contents_into(
                doc,
                src_page_idx,
                &mut out,
                dst_page_idx,
                Affine2D::translate(tx, ty),
                None,
            )?;
        }
    }

    Ok(out)
}

fn impose_booklet(doc: &Document, sheet_w_mm: f64, sheet_h_mm: f64) -> ProdResult<Document> {
    validate_positive("sheet_w_mm", sheet_w_mm)?;
    validate_positive("sheet_h_mm", sheet_h_mm)?;
    for p in &doc.pages {
        validate_positive("page.width_mm", p.width_mm)?;
        validate_positive("page.height_mm", p.height_mm)?;
    }

    let spreads = booklet_signature_order(doc.pages.len());
    if spreads.len() > MAX_IMPOSITION_PAGES {
        return Err(ProdError::InvalidParameter(format!(
            "booklet imposition would produce {} spreads (exceeds cap {MAX_IMPOSITION_PAGES})",
            spreads.len()
        )));
    }
    let half_w = sheet_w_mm * 0.5;
    let mut out = clone_document_shell(doc, format!("{} (Booklet)", doc.title));

    for (spread_idx, (left_opt, right_opt)) in spreads.iter().enumerate() {
        let sheet_num = (spread_idx / 2) + 1;
        let side = if spread_idx % 2 == 0 { "Front" } else { "Back" };
        let fmt_p = |p: Option<usize>| match p {
            Some(i) => format!("P{}", i + 1),
            None => "Blank".to_owned(),
        };
        let page_name = format!(
            "Sheet {sheet_num} {side} ({} | {})",
            fmt_p(*left_opt),
            fmt_p(*right_opt)
        );
        out.add_page(page_name, sheet_w_mm, sheet_h_mm, 0.0);
        let dst_page_idx = out.pages.len() - 1;

        // Place left half flush against the center spine (x = half_w)
        if let Some(src_idx) = *left_opt {
            let p = &doc.pages[src_idx];
            let scale = (half_w / p.width_mm).min(sheet_h_mm / p.height_mm).min(1.0);
            let placed_w = p.width_mm * scale;
            let placed_h = p.height_mm * scale;
            let tx = half_w - placed_w;
            let ty = (sheet_h_mm - placed_h) * 0.5;
            let xf = Affine2D::translate(tx, ty).compose(&Affine2D::scale_uniform(scale));
            clone_page_contents_into(doc, src_idx, &mut out, dst_page_idx, xf, None)?;
        }

        // Place right half flush against the center spine (x = half_w)
        if let Some(src_idx) = *right_opt {
            let p = &doc.pages[src_idx];
            let scale = (half_w / p.width_mm).min(sheet_h_mm / p.height_mm).min(1.0);
            let placed_h = p.height_mm * scale;
            let tx = half_w;
            let ty = (sheet_h_mm - placed_h) * 0.5;
            let xf = Affine2D::translate(tx, ty).compose(&Affine2D::scale_uniform(scale));
            clone_page_contents_into(doc, src_idx, &mut out, dst_page_idx, xf, None)?;
        }
    }

    Ok(out)
}

fn impose_tiling(
    doc: &Document,
    tile_w_mm: f64,
    tile_h_mm: f64,
    overlap_mm: f64,
) -> ProdResult<Document> {
    validate_positive("tile_w_mm", tile_w_mm)?;
    validate_positive("tile_h_mm", tile_h_mm)?;
    validate_non_negative("overlap_mm", overlap_mm)?;
    if overlap_mm >= tile_w_mm || overlap_mm >= tile_h_mm {
        return Err(ProdError::InvalidParameter(
            "tiling overlap_mm must be strictly smaller than both tile_w_mm and tile_h_mm".into(),
        ));
    }

    let step_x = tile_w_mm - overlap_mm;
    let step_y = tile_h_mm - overlap_mm;

    // Validate tile grid counts upfront before allocating any output pages
    let mut page_grids: Vec<(usize, usize)> = Vec::with_capacity(doc.pages.len());
    let mut total_tiles = 0usize;
    for src_page in &doc.pages {
        validate_positive("page.width_mm", src_page.width_mm)?;
        validate_positive("page.height_mm", src_page.height_mm)?;
        let cols_f = if src_page.width_mm <= tile_w_mm + 1e-6 {
            1.0
        } else {
            1.0 + ((src_page.width_mm - tile_w_mm) / step_x).ceil()
        };
        let rows_f = if src_page.height_mm <= tile_h_mm + 1e-6 {
            1.0
        } else {
            1.0 + ((src_page.height_mm - tile_h_mm) / step_y).ceil()
        };
        if !cols_f.is_finite()
            || !rows_f.is_finite()
            || cols_f > MAX_IMPOSITION_PAGES as f64
            || rows_f > MAX_IMPOSITION_PAGES as f64
        {
            return Err(ProdError::InvalidParameter(format!(
                "tiling grid exceeds maximum {MAX_IMPOSITION_PAGES} tiles"
            )));
        }
        let cols = cols_f as usize;
        let rows = rows_f as usize;
        let tiles = cols
            .checked_mul(rows)
            .ok_or_else(|| ProdError::InvalidParameter("tiling tile count overflow".into()))?;
        total_tiles = total_tiles
            .checked_add(tiles)
            .filter(|&t| t <= MAX_IMPOSITION_PAGES)
            .ok_or_else(|| {
                ProdError::InvalidParameter(format!(
                    "tiling would generate more than {MAX_IMPOSITION_PAGES} total tiles"
                ))
            })?;
        page_grids.push((rows, cols));
    }

    let mut out = clone_document_shell(doc, format!("{} (Tiled)", doc.title));

    for (src_idx, src_page) in doc.pages.iter().enumerate() {
        let (rows, cols) = page_grids[src_idx];

        for r in 0..rows {
            for c in 0..cols {
                let tile_ox = (c as f64) * step_x;
                let tile_oy = (r as f64) * step_y;
                let tile_rect_src = Rect2D::from_xywh(tile_ox, tile_oy, tile_w_mm, tile_h_mm);

                out.add_page(
                    format!("{} — Tile R{}C{}", src_page.name, r + 1, c + 1),
                    tile_w_mm,
                    tile_h_mm,
                    0.0,
                );
                let dst_page_idx = out.pages.len() - 1;
                clone_page_contents_into(
                    doc,
                    src_idx,
                    &mut out,
                    dst_page_idx,
                    Affine2D::translate(-tile_ox, -tile_oy),
                    Some(tile_rect_src.expand(1.0)),
                )?;

                add_tile_alignment_marks(
                    &mut out,
                    dst_page_idx,
                    r,
                    c,
                    rows,
                    cols,
                    tile_w_mm,
                    tile_h_mm,
                    overlap_mm,
                    &src_page.name,
                )?;
            }
        }
    }

    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn add_tile_alignment_marks(
    doc: &mut Document,
    page_index: usize,
    row: usize,
    col: usize,
    total_rows: usize,
    total_cols: usize,
    tile_w_mm: f64,
    tile_h_mm: f64,
    overlap_mm: f64,
    src_page_name: &str,
) -> ProdResult<()> {
    let marks_layer = find_or_create_layer(doc, page_index, TILE_MARKS_LAYER_NAME, false)?;
    let mut dashed_guide = OutlineStyle::solid(Color::KEY_BLACK, 0.2);
    dashed_guide.dash_pattern = vec![4.0, 2.0];
    let solid_mark = OutlineStyle::solid(Color::KEY_BLACK, 0.2);

    let add_line = |doc: &mut Document,
                    name: &str,
                    a: Point2D,
                    b: Point2D,
                    style: OutlineStyle|
     -> ProdResult<()> {
        let obj = SceneObject::new(
            ObjectId(0),
            name,
            NodePayload::Path(BezierPath::from_line(a, b)),
        )
        .with_outline(style);
        doc.add_object(obj, Some(marks_layer))?;
        Ok(())
    };

    if overlap_mm > 0.0 {
        if col > 0 {
            add_line(
                doc,
                "Left Overlap Line",
                Point2D::new(overlap_mm, 0.0),
                Point2D::new(overlap_mm, tile_h_mm),
                dashed_guide.clone(),
            )?;
            let cross =
                build_registration_target(Point2D::new(overlap_mm * 0.5, tile_h_mm * 0.5), 2.0);
            doc.add_object(
                SceneObject::new(ObjectId(0), "Left Overlap Target", NodePayload::Path(cross))
                    .with_outline(solid_mark.clone()),
                Some(marks_layer),
            )?;
        }
        if col + 1 < total_cols {
            let x = tile_w_mm - overlap_mm;
            add_line(
                doc,
                "Right Overlap Line",
                Point2D::new(x, 0.0),
                Point2D::new(x, tile_h_mm),
                dashed_guide.clone(),
            )?;
            let cross = build_registration_target(
                Point2D::new(tile_w_mm - overlap_mm * 0.5, tile_h_mm * 0.5),
                2.0,
            );
            doc.add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Right Overlap Target",
                    NodePayload::Path(cross),
                )
                .with_outline(solid_mark.clone()),
                Some(marks_layer),
            )?;
        }
        if row > 0 {
            add_line(
                doc,
                "Top Overlap Line",
                Point2D::new(0.0, overlap_mm),
                Point2D::new(tile_w_mm, overlap_mm),
                dashed_guide.clone(),
            )?;
            let cross =
                build_registration_target(Point2D::new(tile_w_mm * 0.5, overlap_mm * 0.5), 2.0);
            doc.add_object(
                SceneObject::new(ObjectId(0), "Top Overlap Target", NodePayload::Path(cross))
                    .with_outline(solid_mark.clone()),
                Some(marks_layer),
            )?;
        }
        if row + 1 < total_rows {
            let y = tile_h_mm - overlap_mm;
            add_line(
                doc,
                "Bottom Overlap Line",
                Point2D::new(0.0, y),
                Point2D::new(tile_w_mm, y),
                dashed_guide,
            )?;
            let cross = build_registration_target(
                Point2D::new(tile_w_mm * 0.5, tile_h_mm - overlap_mm * 0.5),
                2.0,
            );
            doc.add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Bottom Overlap Target",
                    NodePayload::Path(cross),
                )
                .with_outline(solid_mark),
                Some(marks_layer),
            )?;
        }
    }

    // Tile identification label
    let label = format!(
        "{src_page_name} | Tile R{}C{} of {}x{} (Overlap {overlap_mm:.1}mm)",
        row + 1,
        col + 1,
        total_rows,
        total_cols
    );
    let slug = SceneObject::new(
        ObjectId(0),
        "Tile Slug",
        NodePayload::ArtisticText(ArtisticText::from_plain(
            label,
            TextStyle {
                font_size_pt: 8.0,
                fill: Color::KEY_BLACK,
                ..TextStyle::default()
            },
        )),
    )
    .with_transform(Affine2D::translate(3.0, 3.0))
    .with_outline(OutlineStyle::none());
    doc.add_object(slug, Some(marks_layer))?;
    Ok(())
}

// =============================================================================
// Print Marks Generator
// =============================================================================

/// Configuration for [`generate_print_marks`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintMarksOptions {
    pub crop_marks: bool,
    pub bleed_marks: bool,
    pub registration_targets: bool,
    pub color_calibration_bars: bool,
    pub page_info_slug: bool,
    pub mark_length_mm: f64,
    pub mark_offset_mm: f64,
    pub stroke_width_mm: f64,
    pub layer_name: String,
}

impl Default for PrintMarksOptions {
    fn default() -> Self {
        Self {
            crop_marks: true,
            bleed_marks: true,
            registration_targets: true,
            color_calibration_bars: true,
            page_info_slug: true,
            mark_length_mm: 6.0,
            mark_offset_mm: 3.0,
            stroke_width_mm: 0.125,
            layer_name: PRINT_MARKS_LAYER_NAME.to_owned(),
        }
    }
}

/// Summary of generated vector print marks added to a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintMarksResult {
    pub layer_id: LayerId,
    pub crop_mark_ids: Vec<ObjectId>,
    pub bleed_mark_ids: Vec<ObjectId>,
    pub registration_target_ids: Vec<ObjectId>,
    pub color_bar_ids: Vec<ObjectId>,
    pub page_slug_id: Option<ObjectId>,
}

impl PrintMarksResult {
    /// Total number of mark objects created.
    #[must_use]
    pub fn total_objects(&self) -> usize {
        self.crop_mark_ids.len()
            + self.bleed_mark_ids.len()
            + self.registration_target_ids.len()
            + self.color_bar_ids.len()
            + usize::from(self.page_slug_id.is_some())
    }
}

/// Generates crisp vector Crop Marks, Bleed Marks, Registration Targets, CMYK Color
/// Calibration Bars, and a Page Info Slug on a dedicated print-marks layer.
pub fn generate_print_marks(
    doc: &mut Document,
    page_index: usize,
    options: &PrintMarksOptions,
) -> ProdResult<PrintMarksResult> {
    let page = get_page(doc, page_index)?;
    let w = page.width_mm;
    let h = page.height_mm;
    let bleed = if page.bleed_mm.is_finite() {
        page.bleed_mm.max(0.0)
    } else {
        0.0
    };
    validate_positive("page.width_mm", w)?;
    validate_positive("page.height_mm", h)?;
    let page_name = page.name.clone();
    let total_pages = doc.pages.len();
    let doc_title = doc.title.clone();
    let color_mode = doc.color_mode;
    let dpi = doc.dpi;

    validate_positive("mark_length_mm", options.mark_length_mm)?;
    validate_non_negative("mark_offset_mm", options.mark_offset_mm)?;
    validate_positive("stroke_width_mm", options.stroke_width_mm)?;

    let layer_name = if options.layer_name.trim().is_empty() {
        PRINT_MARKS_LAYER_NAME
    } else {
        options.layer_name.as_str()
    };
    let layer_id = find_or_create_layer(doc, page_index, layer_name, false)?;

    // Registration ink prints on all C/M/Y/K plates (100% each).
    let reg_color = Color::spot("Registration", 1.0, [1.0, 1.0, 1.0, 1.0]);
    let mark_stroke = OutlineStyle::solid(reg_color.clone(), options.stroke_width_mm);
    let offset = options.mark_offset_mm.max(bleed);
    let len = options.mark_length_mm;

    let mut result = PrintMarksResult {
        layer_id,
        crop_mark_ids: Vec::new(),
        bleed_mark_ids: Vec::new(),
        registration_target_ids: Vec::new(),
        color_bar_ids: Vec::new(),
        page_slug_id: None,
    };

    // 1. Crop Marks (8 vector lines at the 4 page trim corners)
    if options.crop_marks {
        let crop_lines = [
            // Top-left
            (
                "Crop Mark TL-H",
                Point2D::new(-offset - len, 0.0),
                Point2D::new(-offset, 0.0),
            ),
            (
                "Crop Mark TL-V",
                Point2D::new(0.0, -offset - len),
                Point2D::new(0.0, -offset),
            ),
            // Top-right
            (
                "Crop Mark TR-H",
                Point2D::new(w + offset, 0.0),
                Point2D::new(w + offset + len, 0.0),
            ),
            (
                "Crop Mark TR-V",
                Point2D::new(w, -offset - len),
                Point2D::new(w, -offset),
            ),
            // Bottom-right
            (
                "Crop Mark BR-H",
                Point2D::new(w + offset, h),
                Point2D::new(w + offset + len, h),
            ),
            (
                "Crop Mark BR-V",
                Point2D::new(w, h + offset),
                Point2D::new(w, h + offset + len),
            ),
            // Bottom-left
            (
                "Crop Mark BL-H",
                Point2D::new(-offset - len, h),
                Point2D::new(-offset, h),
            ),
            (
                "Crop Mark BL-V",
                Point2D::new(0.0, h + offset),
                Point2D::new(0.0, h + offset + len),
            ),
        ];
        for (name, a, b) in crop_lines {
            let obj = SceneObject::new(
                ObjectId(0),
                name,
                NodePayload::Path(BezierPath::from_line(a, b)),
            )
            .with_outline(mark_stroke.clone());
            result
                .crop_mark_ids
                .push(doc.add_object(obj, Some(layer_id))?);
        }
    }

    // 2. Bleed Marks (when bleed > 0)
    if options.bleed_marks && bleed > 1e-6 {
        let b_len = len * 0.65;
        let b_off = offset + 1.5;
        let bleed_lines = [
            (
                "Bleed Mark TL-H",
                Point2D::new(-b_off - b_len, -bleed),
                Point2D::new(-b_off, -bleed),
            ),
            (
                "Bleed Mark TL-V",
                Point2D::new(-bleed, -b_off - b_len),
                Point2D::new(-bleed, -b_off),
            ),
            (
                "Bleed Mark TR-H",
                Point2D::new(w + b_off, -bleed),
                Point2D::new(w + b_off + b_len, -bleed),
            ),
            (
                "Bleed Mark TR-V",
                Point2D::new(w + bleed, -b_off - b_len),
                Point2D::new(w + bleed, -b_off),
            ),
            (
                "Bleed Mark BR-H",
                Point2D::new(w + b_off, h + bleed),
                Point2D::new(w + b_off + b_len, h + bleed),
            ),
            (
                "Bleed Mark BR-V",
                Point2D::new(w + bleed, h + b_off),
                Point2D::new(w + bleed, h + b_off + b_len),
            ),
            (
                "Bleed Mark BL-H",
                Point2D::new(-b_off - b_len, h + bleed),
                Point2D::new(-b_off, h + bleed),
            ),
            (
                "Bleed Mark BL-V",
                Point2D::new(-bleed, h + b_off),
                Point2D::new(-bleed, h + b_off + b_len),
            ),
        ];
        for (name, a, b) in bleed_lines {
            let obj = SceneObject::new(
                ObjectId(0),
                name,
                NodePayload::Path(BezierPath::from_line(a, b)),
            )
            .with_outline(mark_stroke.clone());
            result
                .bleed_mark_ids
                .push(doc.add_object(obj, Some(layer_id))?);
        }
    }

    // 3. Registration Targets (4 bullseyes outside all 4 page sides)
    if options.registration_targets {
        let reg_r = 2.0;
        let center_dist = offset + len * 0.5 + reg_r * 0.5;
        let centers = [
            (
                "Registration Target Top",
                Point2D::new(w * 0.5, -center_dist),
            ),
            (
                "Registration Target Bottom",
                Point2D::new(w * 0.5, h + center_dist),
            ),
            (
                "Registration Target Left",
                Point2D::new(-center_dist, h * 0.5),
            ),
            (
                "Registration Target Right",
                Point2D::new(w + center_dist, h * 0.5),
            ),
        ];
        for (name, c) in centers {
            let path = build_registration_target(c, reg_r);
            let obj = SceneObject::new(ObjectId(0), name, NodePayload::Path(path))
                .with_outline(mark_stroke.clone());
            result
                .registration_target_ids
                .push(doc.add_object(obj, Some(layer_id))?);
        }
    }

    // 4. CMYK Color Calibration Bars + 10-step grayscale density strip
    if options.color_calibration_bars {
        let patches: Vec<(&str, Color)> = vec![
            ("C 100%", Color::cmyk(1.0, 0.0, 0.0, 0.0)),
            ("M 100%", Color::cmyk(0.0, 1.0, 0.0, 0.0)),
            ("Y 100%", Color::cmyk(0.0, 0.0, 1.0, 0.0)),
            ("K 100%", Color::cmyk(0.0, 0.0, 0.0, 1.0)),
            ("C+M Blue", Color::cmyk(1.0, 1.0, 0.0, 0.0)),
            ("M+Y Red", Color::cmyk(0.0, 1.0, 1.0, 0.0)),
            ("C+Y Green", Color::cmyk(1.0, 0.0, 1.0, 0.0)),
            ("K 10%", Color::cmyk(0.0, 0.0, 0.0, 0.1)),
            ("K 30%", Color::cmyk(0.0, 0.0, 0.0, 0.3)),
            ("K 50%", Color::cmyk(0.0, 0.0, 0.0, 0.5)),
            ("K 70%", Color::cmyk(0.0, 0.0, 0.0, 0.7)),
            ("K 90%", Color::cmyk(0.0, 0.0, 0.0, 0.9)),
        ];
        let patch_w = 5.0_f64.min((w * 0.8) / (patches.len() as f64)).max(2.0);
        let patch_h = 4.0;
        let bar_y = -offset - patch_h - 1.5;
        let start_x = 8.0_f64.min(w * 0.1);
        let border = OutlineStyle::solid(Color::KEY_BLACK, 0.1);

        for (i, (label, color)) in patches.into_iter().enumerate() {
            let x = start_x + (i as f64) * patch_w;
            let rect = BezierPath::from_rect(Rect2D::from_xywh(x, bar_y, patch_w, patch_h));
            let obj = SceneObject::new(
                ObjectId(0),
                format!("Color Bar {label}"),
                NodePayload::Path(rect),
            )
            .with_fill(Fill::Solid(color))
            .with_outline(border.clone());
            result
                .color_bar_ids
                .push(doc.add_object(obj, Some(layer_id))?);
        }
    }

    // 5. Page Info Slug
    if options.page_info_slug {
        let slug_text = format!(
            "{doc_title} | {page_name} ({}/{total_pages}) | {w:.1}x{h:.1} mm | Bleed {bleed:.1} mm | {color_mode:?} {dpi:.0} DPI",
            page_index + 1
        );
        let slug_obj = SceneObject::new(
            ObjectId(0),
            "Page Info Slug",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                slug_text,
                TextStyle {
                    font_size_pt: 7.0,
                    fill: reg_color,
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(4.0, h + offset + 2.0))
        .with_outline(OutlineStyle::none());
        result.page_slug_id = Some(doc.add_object(slug_obj, Some(layer_id))?);
    }

    Ok(result)
}

/// Builds a compound vector registration bullseye (circle + crosshair lines) centered at `center`.
fn build_registration_target(center: Point2D, radius_mm: f64) -> BezierPath {
    let mut circle = ParametricShape::Ellipse {
        rx: radius_mm,
        ry: radius_mm,
        mode: EllipseMode::Full,
        start_deg: 0.0,
        end_deg: 360.0,
    }
    .to_bezier_path();
    circle.transform(&Affine2D::translate(center.x, center.y));

    let cross = radius_mm * 1.6;
    let h_line = BezierPath::from_line(
        Point2D::new(center.x - cross, center.y),
        Point2D::new(center.x + cross, center.y),
    );
    let v_line = BezierPath::from_line(
        Point2D::new(center.x, center.y - cross),
        Point2D::new(center.x, center.y + cross),
    );

    let mut subpaths = circle.subpaths;
    subpaths.extend(h_line.subpaths);
    subpaths.extend(v_line.subpaths);
    BezierPath { subpaths }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{ColorMode, CornerStyle};

    fn rect_obj(x: f64, y: f64, w: f64, h: f64, fill: Fill, outline: OutlineStyle) -> SceneObject {
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
        .with_outline(outline)
        .with_transform(Affine2D::translate(x, y))
    }

    #[test]
    fn cmyk_spot_separations_and_overprint_simulation() {
        let mut doc = Document::new("Sep Test", 100.0, 100.0, ColorMode::Cmyk, 300.0);
        // Full-page Cyan background
        doc.add_object(
            rect_obj(
                0.0,
                0.0,
                100.0,
                100.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
        // Left half: Magenta overprinting (BlendMode::Multiply) -> C=100%, M=100% (Blue!)
        let mut overprint_mag = rect_obj(
            0.0,
            0.0,
            50.0,
            100.0,
            Fill::Solid(Color::MAGENTA),
            OutlineStyle::none(),
        );
        overprint_mag.blend_mode = BlendMode::Multiply;
        doc.add_object(overprint_mag, None).unwrap();

        // Right quarter: Yellow knocking out Cyan -> C=0%, Y=100%
        doc.add_object(
            rect_obj(
                50.0,
                0.0,
                50.0,
                50.0,
                Fill::Solid(Color::YELLOW),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();

        // CutContour Spot outline on cut layer
        let cut_layer = doc
            .add_layer(
                doc.pages[0].id,
                "CutContour",
                true,
                coree_core::MasterLayerScope::None,
            )
            .unwrap();
        doc.add_object(
            rect_obj(
                10.0,
                10.0,
                80.0,
                80.0,
                Fill::None,
                OutlineStyle::cut_contour(),
            ),
            Some(cut_layer),
        )
        .unwrap();

        let opts = SeparationOptions {
            dpi: 25.4, // 1 px per mm => 100x100 px
            max_dimension_px: 256,
            include_cut_layers: true,
        };
        let sep = compute_separations(&doc, 0, &opts).unwrap();
        assert_eq!(sep.width, 100);
        assert_eq!(sep.height, 100);
        assert_eq!(sep.plates.len(), 5); // C, M, Y, K, Spot: CutContour

        let c_plate = sep.plate(&SeparationChannel::Cyan).unwrap();
        let m_plate = sep.plate(&SeparationChannel::Magenta).unwrap();
        let y_plate = sep.plate(&SeparationChannel::Yellow).unwrap();
        let spot_plate = sep.spot_plate("CutContour").unwrap();

        // Pixel (25, 50) is inside overprinted Cyan+Magenta -> both C and M == 255!
        let idx_over = 50 * 100 + 25;
        assert_eq!(c_plate.ink_map[idx_over], 255);
        assert_eq!(m_plate.ink_map[idx_over], 255);

        // Pixel (75, 25) is inside knocked-out Yellow -> C == 0, Y == 255!
        let idx_knock = 25 * 100 + 75;
        assert_eq!(c_plate.ink_map[idx_knock], 0);
        assert_eq!(y_plate.ink_map[idx_knock], 255);

        // Spot CutContour plate has non-zero ink
        assert!(spot_plate.inked_area_pct > 0.0);

        // Overprint RGBA simulation: (25, 50) has C=1, M=1, Y=0, K=0 => R=0, G=0, B=255 (Blue!)
        let rgba = simulate_overprint_rgba(&sep);
        assert_eq!(rgba.len(), 100 * 100 * 4);
        let o = idx_over * 4;
        assert_eq!(
            (rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]),
            (0, 0, 255, 255)
        );
        // (75, 25) has Y=1 => R=255, G=255, B=0 (Yellow!)
        let k = idx_knock * 4;
        assert_eq!(
            (rgba[k], rgba[k + 1], rgba[k + 2], rgba[k + 3]),
            (255, 255, 0, 255)
        );
    }

    #[test]
    fn booklet_signature_ordering_for_4_6_and_8_pages() {
        assert_eq!(
            booklet_signature_order(4),
            vec![(Some(3), Some(0)), (Some(1), Some(2))]
        );
        assert_eq!(
            booklet_signature_order(6),
            vec![
                (None, Some(0)),
                (Some(1), None),
                (Some(5), Some(2)),
                (Some(3), Some(4)),
            ]
        );
        assert_eq!(
            booklet_signature_order(8),
            vec![
                (Some(7), Some(0)),
                (Some(1), Some(6)),
                (Some(5), Some(2)),
                (Some(3), Some(4)),
            ]
        );
    }

    #[test]
    fn imposition_nup_booklet_and_tiling() {
        // 1. N-Up step-and-repeat for 1-page business card (85x55 -> 2x5 on A4)
        let mut card = Document::new("Business Card", 85.0, 55.0, ColorMode::Cmyk, 300.0);
        card.add_object(
            rect_obj(
                0.0,
                0.0,
                85.0,
                55.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
        let nup = impose_document(
            &card,
            &ImpositionMode::NUp {
                cols: 2,
                rows: 5,
                gap_x_mm: 4.0,
                gap_y_mm: 2.0,
                sheet_w_mm: 210.0,
                sheet_h_mm: 297.0,
            },
        )
        .unwrap();
        assert_eq!(nup.pages.len(), 1);
        assert_eq!(nup.objects.len(), 10);

        // 2. Booklet Saddle-Stitch on 6-page A5 document -> 4 imposed A4 spreads
        let mut booklet_src = Document::new("Zine", 148.5, 210.0, ColorMode::Cmyk, 300.0);
        for p in 0..6 {
            if p > 0 {
                booklet_src.add_page(format!("Page {}", p + 1), 148.5, 210.0, 0.0);
            }
            let lid = booklet_src.pages[p].layers[0].id;
            booklet_src
                .add_object(
                    SceneObject::new(
                        ObjectId(0),
                        format!("P{}", p + 1),
                        NodePayload::ArtisticText(ArtisticText::from_plain(
                            format!("Page {}", p + 1),
                            TextStyle::default(),
                        )),
                    ),
                    Some(lid),
                )
                .unwrap();
        }
        let booklet = impose_document(
            &booklet_src,
            &ImpositionMode::BookletSaddleStitch {
                sheet_w_mm: 297.0,
                sheet_h_mm: 210.0,
            },
        )
        .unwrap();
        assert_eq!(booklet.pages.len(), 4);
        assert!(booklet.pages[0].name.contains("Blank | P1"));
        assert!(booklet.pages[1].name.contains("P2 | Blank"));
        assert!(booklet.pages[2].name.contains("P6 | P3"));
        assert!(booklet.pages[3].name.contains("P4 | P5"));
        assert_eq!(booklet.objects.len(), 6);

        // 3. Large-format Tiling: 1000x600 billboard into 400x350 tiles with 20mm overlap
        let mut banner = Document::new("Billboard", 1000.0, 600.0, ColorMode::Cmyk, 150.0);
        banner
            .add_object(
                rect_obj(
                    0.0,
                    0.0,
                    1000.0,
                    600.0,
                    Fill::Solid(Color::MAGENTA),
                    OutlineStyle::none(),
                ),
                None,
            )
            .unwrap();
        let tiled = impose_document(
            &banner,
            &ImpositionMode::Tiling {
                tile_w_mm: 400.0,
                tile_h_mm: 350.0,
                overlap_mm: 20.0,
            },
        )
        .unwrap();
        // cols = 1 + ceil((1000 - 400) / 380) = 1 + 2 = 3; rows = 1 + ceil((600 - 350) / 330) = 2 => 6 tiles
        assert_eq!(tiled.pages.len(), 6);
        assert!(tiled.pages[0]
            .layers
            .iter()
            .any(|l| l.name == TILE_MARKS_LAYER_NAME && !l.objects.is_empty()));
    }

    #[test]
    fn print_marks_creates_all_mark_types_on_dedicated_layer() {
        let mut doc = Document::new_a4("Flyer");
        let report = generate_print_marks(&mut doc, 0, &PrintMarksOptions::default()).unwrap();
        assert_eq!(report.crop_mark_ids.len(), 8);
        assert_eq!(report.bleed_mark_ids.len(), 8);
        assert_eq!(report.registration_target_ids.len(), 4);
        assert!(report.color_bar_ids.len() >= 10);
        assert!(report.page_slug_id.is_some());
        let layer = doc.find_layer(report.layer_id).unwrap();
        assert_eq!(layer.name, PRINT_MARKS_LAYER_NAME);
        assert_eq!(layer.objects.len(), report.total_objects());
    }
}
