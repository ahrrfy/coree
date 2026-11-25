// SPDX-License-Identifier: MIT OR Apache-2.0

//! Prepress engine: CMYK + Spot color separations, subtractive overprint simulation,
//! print imposition (`N-Up`, `BookletSaddleStitch`, `Tiling`), and vector print marks
//! (crop marks, bleed marks, registration targets, CMYK color bars, and page slugs).

use crate::error::{ProdError, ProdResult};
use crate::scene_helpers::{
    apply_obj_perspective_to_local_path, bezier_to_skia_path, clone_document_shell,
    clone_page_contents_into, collect_effective_layers, compose_prod_parent_persp,
    container_own_persp, find_or_create_layer, get_page, warp_local_to_world, SymBoundsCtx,
    SymParentRef, MAX_SCENE_DEPTH,
};
use coree_core::{
    apply_boolean, Affine2D, ArtisticText, BezierPath, BlendMode, BooleanOp, Color, Document,
    Effect, EllipseMode, Fill, LayerId, LineCap, LineJoin, NodePayload, ObjectId, OutlineStyle,
    PageId, ParametricShape, Perspective3x3, Point2D, Rect2D, SceneObject, SymbolId, TextStyle,
};
use coree_io::display_list::{DisplayListOptions, DisplayOp, ExportDisplayList};
use coree_render::{render_isolated_object_to_rgba_with_options, TextEngine};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use kurbo::PathEl;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;

/// Type alias for prepress trapping configuration.
pub type Trapping = TrappingSettings;

/// Type alias for prepress halftone screening configuration.
pub type Halftone = HalftoneSettings;

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

/// Vector geometry primitive on a single separated ink plate (from `ExportDisplayList`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPlatePrimitive {
    pub path: BezierPath,
    pub fill_density: Option<f32>,
    pub stroke_density: Option<f32>,
    pub stroke_width_mm: f64,
    pub overprint: bool,
    #[serde(default)]
    pub clip_paths: Vec<BezierPath>,
    #[serde(default)]
    pub z_order: usize,
}

/// Raster image / isolated composite primitive on a single separated ink plate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RasterPlatePrimitive {
    pub width: u32,
    pub height: u32,
    /// Per-pixel ink density `0..=255` (`0` = no ink, `255` = 100% ink) for this plate.
    pub ink_channel: Vec<u8>,
    /// Optional per-pixel alpha mask `0..=255` (`255` = opaque). Empty when fully opaque.
    pub alpha_mask: Vec<u8>,
    pub bl: Point2D,
    pub br: Point2D,
    pub tl: Point2D,
    pub overprint: bool,
    #[serde(default)]
    pub clip_paths: Vec<BezierPath>,
    #[serde(default)]
    pub z_order: usize,
}

/// A single ink separation plate with vector plate ops, grayscale density map, and coverage stats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeparationPlate {
    pub channel: SeparationChannel,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Grayscale plate map (`0` = no ink / blank paper, `255` = 100% solid ink).
    pub ink_map: Vec<u8>,
    /// Crisp resolution-independent vector primitives for this ink channel.
    #[serde(default)]
    pub vector_ops: Vec<VectorPlatePrimitive>,
    /// Separated raster / isolated composite primitives for this ink channel.
    #[serde(default)]
    pub raster_ops: Vec<RasterPlatePrimitive>,
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
    compute_separations_with_text_outliner(doc, page_index, options, |_, _| None)
}

/// Analyzes `doc.pages[page_index]` and generates CMYK + Spot color separation plates while
/// converting live text objects (`ArtisticText`, `ParagraphText`, `TextOnPath`, and symbol text)
/// via `text_outliner` into true vector glyph curves in each object's local coordinate space.
pub fn compute_separations_with_text_outliner<F>(
    doc: &Document,
    page_index: usize,
    options: &SeparationOptions,
    mut text_outliner: F,
) -> ProdResult<SeparationResult>
where
    F: FnMut(&SceneObject, &Document) -> Option<BezierPath>,
{
    let page = get_page(doc, page_index)?;
    validate_positive("page.width_mm", page.width_mm)?;
    validate_positive("page.height_mm", page.height_mm)?;
    if !options.dpi.is_finite() || options.dpi <= 0.0 {
        return Err(ProdError::InvalidParameter(
            "separation DPI must be positive and finite".into(),
        ));
    }
    let raw_w = (page.width_mm / 25.4 * options.dpi).round().max(1.0);
    let raw_h = (page.height_mm / 25.4 * options.dpi).round().max(1.0);
    let downscale = if options.max_dimension_px == 0 {
        1.0
    } else {
        let max_dim = options.max_dimension_px.max(8);
        (f64::from(max_dim) / raw_w.max(raw_h)).min(1.0)
    };
    let width = ((raw_w * downscale).round() as u32).max(1);
    let height = ((raw_h * downscale).round() as u32).max(1);
    let scale_x = (f64::from(width) / page.width_mm) as f32;
    let scale_y = (f64::from(height) / page.height_mm) as f32;

    let vector_plates = compute_vector_separations(doc, page_index, options.include_cut_layers)?;
    let effective_layers = collect_effective_layers(doc, page_index);

    // Pass 1: discover all named Spot inks used on visible/printable layers of this page.
    let mut spot_fallbacks: BTreeMap<String, [f32; 4]> = BTreeMap::new();
    for vp in &vector_plates {
        if let SeparationChannel::Spot(ref name) = vp.channel {
            if let Some(fb) = vp.spot_fallback_cmyk {
                spot_fallbacks.entry(name.clone()).or_insert(fb);
            }
        }
    }
    for layer in &effective_layers {
        if !layer_included_in_separation(layer, options.include_cut_layers) {
            continue;
        }
        let mut visiting = HashSet::new();
        for &oid in &layer.objects {
            collect_spots_recursive(doc, oid, None, 0, &mut visiting, &mut spot_fallbacks);
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
        page_index,
        width,
        height,
        scale_x,
        scale_y,
        spot_index: &spot_index,
        buffers: &mut buffers,
        text_outliner: &mut text_outliner,
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
        let mut plate = build_plate(chan.clone(), width, height, &buffers[idx], None);
        if let Some(vp) = vector_plates.iter().find(|v| v.channel == chan) {
            plate.vector_ops = vp.vector_ops.clone();
            plate.raster_ops = vp.raster_ops.clone();
        }
        plates.push(plate);
    }
    for (i, spot_name) in spot_names.into_iter().enumerate() {
        let fallback = spot_fallbacks.get(&spot_name).copied();
        let chan = SeparationChannel::Spot(spot_name);
        let mut plate = build_plate(chan.clone(), width, height, &buffers[4 + i], fallback);
        if let Some(vp) = vector_plates.iter().find(|v| v.channel == chan) {
            plate.vector_ops = vp.vector_ops.clone();
            plate.raster_ops = vp.raster_ops.clone();
        }
        plates.push(plate);
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

fn collect_symbol_spots(
    doc: &Document,
    sym_id: SymbolId,
    spots: &mut BTreeMap<String, [f32; 4]>,
    visiting_syms: &mut HashSet<SymbolId>,
) {
    if !visiting_syms.insert(sym_id) {
        return;
    }
    if let Some(sym) = doc.symbol(sym_id) {
        for sub in &sym.objects {
            for c in sub.fill.referenced_colors() {
                register_spot_color(&c, spots);
            }
            if sub.outline.is_visible() {
                register_spot_color(&sub.outline.color, spots);
            }
            if let Fill::VectorPattern { symbol_id, .. } = &sub.fill {
                collect_symbol_spots(doc, *symbol_id, spots, visiting_syms);
            }
            if let NodePayload::SymbolInstance(nested_id) = &sub.payload {
                collect_symbol_spots(doc, *nested_id, spots, visiting_syms);
            }
        }
    }
    visiting_syms.remove(&sym_id);
}

fn sample_pattern_representative_color(doc: &Document, fill: &Fill) -> Option<Color> {
    match fill {
        Fill::VectorPattern { symbol_id, .. } => {
            let sym = doc.symbol(*symbol_id)?;
            for sub in &sym.objects {
                if !sub.visible {
                    continue;
                }
                if let Some(c) = sub.fill.referenced_colors().into_iter().next() {
                    return Some(c);
                }
                if sub.outline.is_visible() {
                    return Some(sub.outline.color.clone());
                }
            }
            None
        }
        Fill::BitmapPattern { image_id, .. } => {
            let asset = doc.asset(*image_id)?;
            let rgba = asset.to_rgba8_vec();
            let mut sum_r = 0u64;
            let mut sum_g = 0u64;
            let mut sum_b = 0u64;
            let mut count = 0u64;
            for px in rgba.as_chunks::<4>().0 {
                if px[3] > 0 {
                    sum_r += u64::from(px[0]);
                    sum_g += u64::from(px[1]);
                    sum_b += u64::from(px[2]);
                    count += 1;
                }
            }
            sum_r
                .checked_div(count)
                .map(|r| Color::rgba8(r as u8, (sum_g / count) as u8, (sum_b / count) as u8, 255))
        }
        _ => None,
    }
}

/// Computes resolution-independent **vector** CMYK + Spot separation plates by lowering
/// the full scene graph through `ExportDisplayList` (`CdrPsi`) without any pixel cap.
pub fn compute_vector_separations(
    doc: &Document,
    page_index: usize,
    include_cut_layers: bool,
) -> ProdResult<Vec<SeparationPlate>> {
    let page = get_page(doc, page_index)?;
    validate_positive("page.width_mm", page.width_mm)?;
    validate_positive("page.height_mm", page.height_mm)?;
    let mut text_engine = TextEngine::new();
    let dl_opts = DisplayListOptions {
        shape_pdf_text_runs: false,
        ..DisplayListOptions::pdf()
    };
    let dl = ExportDisplayList::build(doc, page_index, &dl_opts, &mut text_engine)
        .map_err(|e| ProdError::InvalidParameter(format!("display list build failed: {e}")))?;

    let mut spots: BTreeMap<String, [f32; 4]> = BTreeMap::new();
    let mut ops_by_channel: BTreeMap<SeparationChannel, Vec<VectorPlatePrimitive>> =
        BTreeMap::new();
    let mut raster_ops_by_channel: BTreeMap<SeparationChannel, Vec<RasterPlatePrimitive>> =
        BTreeMap::new();
    let mut plate_has_content: BTreeMap<SeparationChannel, bool> = BTreeMap::new();
    for ch in [
        SeparationChannel::Cyan,
        SeparationChannel::Magenta,
        SeparationChannel::Yellow,
        SeparationChannel::Black,
    ] {
        ops_by_channel.insert(ch.clone(), Vec::new());
        raster_ops_by_channel.insert(ch.clone(), Vec::new());
        plate_has_content.insert(ch, false);
    }

    let mut layer_active_stack: Vec<bool> = Vec::new();
    let mut active_ops: Vec<&DisplayOp> = Vec::new();
    for op in &dl.ops {
        match op {
            DisplayOp::PushLayer {
                visible,
                printable,
                is_cut_layer,
                ..
            } => {
                let inc = *visible
                    && if *is_cut_layer {
                        include_cut_layers
                    } else {
                        *printable
                    };
                layer_active_stack.push(inc);
            }
            DisplayOp::PopLayer => {
                layer_active_stack.pop();
            }
            other => {
                if layer_active_stack.last().copied().unwrap_or(true) {
                    active_ops.push(other);
                }
            }
        }
    }

    for op in &active_ops {
        match op {
            DisplayOp::DrawVector { fill, outline, .. } => {
                for c in fill.referenced_colors() {
                    register_spot_color(&c, &mut spots);
                }
                if let Fill::VectorPattern { symbol_id, .. } = fill {
                    let mut vs = HashSet::new();
                    collect_symbol_spots(doc, *symbol_id, &mut spots, &mut vs);
                }
                if outline.is_visible() {
                    register_spot_color(&outline.color, &mut spots);
                }
            }
            DisplayOp::DrawTextRuns {
                shaped_runs,
                fallback_curves,
                artistic,
                outline,
                ..
            } => {
                for gd in fallback_curves {
                    register_spot_color(&gd.fill, &mut spots);
                }
                for run in shaped_runs {
                    register_spot_color(&run.fill, &mut spots);
                }
                for run in &artistic.runs {
                    register_spot_color(&run.style.fill, &mut spots);
                }
                if outline.is_visible() {
                    register_spot_color(&outline.color, &mut spots);
                }
            }
            _ => {}
        }
    }
    for name in spots.keys() {
        let ch = SeparationChannel::Spot(name.clone());
        ops_by_channel.entry(ch.clone()).or_default();
        raster_ops_by_channel.entry(ch.clone()).or_default();
        plate_has_content.entry(ch).or_insert(false);
    }

    let mut next_z = 0usize;
    let emit_color_prim =
        |ops_by_channel: &mut BTreeMap<SeparationChannel, Vec<VectorPlatePrimitive>>,
         plate_has_content: &mut BTreeMap<SeparationChannel, bool>,
         next_z: &mut usize,
         path: &BezierPath,
         fill_col: Option<&Color>,
         stroke_col: Option<(&Color, f64)>,
         opacity: f32,
         overprint: bool,
         clip_stack: &[BezierPath]| {
            if path.is_empty() {
                return;
            }
            let z = *next_z;
            *next_z += 1;
            let mut fill_densities: BTreeMap<SeparationChannel, f32> = BTreeMap::new();
            if let Some(fc) = fill_col {
                accumulate_channel_densities(fc, opacity, &mut fill_densities);
            }
            let mut stroke_densities: BTreeMap<SeparationChannel, f32> = BTreeMap::new();
            let stroke_w = if let Some((sc, w_mm)) = stroke_col {
                accumulate_channel_densities(sc, opacity, &mut stroke_densities);
                w_mm
            } else {
                0.0
            };
            let opaque_fill_ko = !overprint
                && fill_col.is_some_and(|fc| (opacity * fc.alpha()).clamp(0.0, 1.0) >= 0.99);
            let opaque_stroke_ko = !overprint
                && stroke_col.is_some_and(|(sc, _)| (opacity * sc.alpha()).clamp(0.0, 1.0) >= 0.99);

            let mut touched: HashSet<SeparationChannel> = fill_densities.keys().cloned().collect();
            touched.extend(stroke_densities.keys().cloned());
            let all_channels: Vec<SeparationChannel> = ops_by_channel.keys().cloned().collect();
            for ch in all_channels {
                let fd = fill_densities.get(&ch).copied().filter(|&v| v > 1e-4);
                let sd = stroke_densities.get(&ch).copied().filter(|&v| v > 1e-4);
                if fd.is_some() || sd.is_some() {
                    ops_by_channel
                        .entry(ch.clone())
                        .or_default()
                        .push(VectorPlatePrimitive {
                            path: path.clone(),
                            fill_density: fd,
                            stroke_density: sd,
                            stroke_width_mm: stroke_w,
                            overprint,
                            clip_paths: clip_stack.to_vec(),
                            z_order: z,
                        });
                    plate_has_content.insert(ch, true);
                } else if !overprint
                    && plate_has_content.get(&ch).copied().unwrap_or(false)
                    && !touched.is_empty()
                    && (opaque_fill_ko || opaque_stroke_ko)
                {
                    let ko_fd = opaque_fill_ko.then_some(0.0);
                    let ko_sd = opaque_stroke_ko.then_some(0.0);
                    ops_by_channel
                        .entry(ch)
                        .or_default()
                        .push(VectorPlatePrimitive {
                            path: path.clone(),
                            fill_density: ko_fd,
                            stroke_density: ko_sd,
                            stroke_width_mm: stroke_w,
                            overprint: false,
                            clip_paths: clip_stack.to_vec(),
                            z_order: z,
                        });
                }
            }
        };

    let icc = doc.icc_pipeline();
    let mut clip_stack: Vec<BezierPath> = Vec::new();
    let mut clip_pushed_stack: Vec<bool> = Vec::new();

    for op in &active_ops {
        match op {
            DisplayOp::PushClip { path, visible, .. } => {
                if *visible && !path.is_empty() {
                    clip_stack.push(path.clone());
                    clip_pushed_stack.push(true);
                } else {
                    clip_pushed_stack.push(false);
                }
            }
            DisplayOp::PopClip => {
                if clip_pushed_stack.pop().unwrap_or(false) {
                    clip_stack.pop();
                }
            }
            DisplayOp::DrawVector {
                path,
                fill,
                outline,
                opacity,
                blend_mode,
                visible,
                ..
            } => {
                if !*visible || *opacity <= 1e-5 {
                    continue;
                }
                let fill_col = match fill {
                    Fill::None => None,
                    Fill::Solid(c) => Some(c.clone()),
                    Fill::LinearGradient { stops, .. }
                    | Fill::RadialGradient { stops, .. }
                    | Fill::ConicalGradient { stops, .. }
                    | Fill::SquareGradient { stops, .. } => Some(Fill::sample_stops(stops, 0.5)),
                    Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => {
                        Some(fg.clone())
                    }
                    Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
                    Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {
                        sample_pattern_representative_color(doc, fill)
                    }
                };
                let stroke_col = outline
                    .is_visible()
                    .then(|| (&outline.color, outline.effective_width_mm()));
                let overprint = *blend_mode == BlendMode::Multiply || outline.overprint;
                emit_color_prim(
                    &mut ops_by_channel,
                    &mut plate_has_content,
                    &mut next_z,
                    path,
                    fill_col.as_ref(),
                    stroke_col,
                    *opacity,
                    overprint,
                    &clip_stack,
                );
            }
            DisplayOp::DrawTextRuns {
                fallback_curves,
                outline,
                world_xf,
                opacity,
                blend_mode,
                visible,
                is_perspective,
                ..
            } => {
                if !*visible || *opacity <= 1e-5 {
                    continue;
                }
                let overprint = *blend_mode == BlendMode::Multiply || outline.overprint;
                let stroke_col = outline
                    .is_visible()
                    .then(|| (&outline.color, outline.effective_width_mm()));
                let curve_xf = if *is_perspective {
                    Affine2D::IDENTITY
                } else {
                    *world_xf
                };
                for gd in fallback_curves {
                    let wp = gd.path.transformed(&curve_xf);
                    emit_color_prim(
                        &mut ops_by_channel,
                        &mut plate_has_content,
                        &mut next_z,
                        &wp,
                        Some(&gd.fill),
                        stroke_col,
                        *opacity,
                        overprint,
                        &clip_stack,
                    );
                }
            }
            DisplayOp::DrawRaster {
                asset_id,
                inline_rgba,
                bl,
                br,
                tl,
                opacity,
                blend_mode,
                visible,
                ..
            } => {
                if !*visible || *opacity <= 1e-5 {
                    continue;
                }
                let owned_rgba;
                let (rw, rh, rgba_slice): (u32, u32, &[u8]) =
                    if let Some(ref arc_rgba) = inline_rgba {
                        (arc_rgba.0, arc_rgba.1, &arc_rgba.2)
                    } else if let Some(aid) = asset_id {
                        let Some(asset) = doc.asset(*aid) else {
                            continue;
                        };
                        owned_rgba = asset.to_rgba8_vec();
                        (asset.width, asset.height, &owned_rgba)
                    } else {
                        continue;
                    };
                let Some(px_count) = (rw as usize).checked_mul(rh as usize) else {
                    continue;
                };
                if px_count == 0 || rgba_slice.len() < px_count * 4 {
                    continue;
                }
                let mut c_buf = vec![0u8; px_count];
                let mut m_buf = vec![0u8; px_count];
                let mut y_buf = vec![0u8; px_count];
                let mut k_buf = vec![0u8; px_count];
                let mut a_buf = vec![0u8; px_count];
                let mut has_c = false;
                let mut has_m = false;
                let mut has_y = false;
                let mut has_k = false;
                let mut has_alpha = false;
                let mut has_any_visible = false;
                for (i, px) in rgba_slice[..px_count * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .enumerate()
                {
                    let eff_a = ((f32::from(px[3]) / 255.0) * *opacity).clamp(0.0, 1.0);
                    let a_u8 = (eff_a * 255.0).round() as u8;
                    a_buf[i] = a_u8;
                    if a_u8 < 255 {
                        has_alpha = true;
                    }
                    if a_u8 > 0 {
                        has_any_visible = true;
                        let cmyk = icc.srgb_to_cmyk([
                            f32::from(px[0]) / 255.0,
                            f32::from(px[1]) / 255.0,
                            f32::from(px[2]) / 255.0,
                        ]);
                        let cb = (cmyk[0].clamp(0.0, 1.0) * 255.0).round() as u8;
                        let mb = (cmyk[1].clamp(0.0, 1.0) * 255.0).round() as u8;
                        let yb = (cmyk[2].clamp(0.0, 1.0) * 255.0).round() as u8;
                        let kb = (cmyk[3].clamp(0.0, 1.0) * 255.0).round() as u8;
                        c_buf[i] = cb;
                        m_buf[i] = mb;
                        y_buf[i] = yb;
                        k_buf[i] = kb;
                        has_c |= cb > 0;
                        has_m |= mb > 0;
                        has_y |= yb > 0;
                        has_k |= kb > 0;
                    }
                }
                if !has_any_visible {
                    continue;
                }
                let overprint = *blend_mode == BlendMode::Multiply;
                let alpha_mask = if has_alpha { a_buf } else { Vec::new() };
                let z = next_z;
                next_z += 1;
                let process_bufs = [
                    (SeparationChannel::Cyan, has_c, c_buf),
                    (SeparationChannel::Magenta, has_m, m_buf),
                    (SeparationChannel::Yellow, has_y, y_buf),
                    (SeparationChannel::Black, has_k, k_buf),
                ];
                for (ch, has_ink, buf) in process_bufs {
                    let needs_ko =
                        !overprint && plate_has_content.get(&ch).copied().unwrap_or(false);
                    if has_ink || needs_ko {
                        raster_ops_by_channel.entry(ch.clone()).or_default().push(
                            RasterPlatePrimitive {
                                width: rw,
                                height: rh,
                                ink_channel: buf,
                                alpha_mask: alpha_mask.clone(),
                                bl: *bl,
                                br: *br,
                                tl: *tl,
                                overprint,
                                clip_paths: clip_stack.clone(),
                                z_order: z,
                            },
                        );
                        if has_ink {
                            plate_has_content.insert(ch, true);
                        }
                    }
                }
                if !overprint {
                    for spot_name in spots.keys() {
                        let ch = SeparationChannel::Spot(spot_name.clone());
                        if plate_has_content.get(&ch).copied().unwrap_or(false) {
                            raster_ops_by_channel.entry(ch).or_default().push(
                                RasterPlatePrimitive {
                                    width: rw,
                                    height: rh,
                                    ink_channel: vec![0u8; px_count],
                                    alpha_mask: alpha_mask.clone(),
                                    bl: *bl,
                                    br: *br,
                                    tl: *tl,
                                    overprint: false,
                                    clip_paths: clip_stack.clone(),
                                    z_order: z,
                                },
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut plates = Vec::with_capacity(ops_by_channel.len());
    for (chan, vops) in ops_by_channel {
        let fb = match &chan {
            SeparationChannel::Spot(n) => spots.get(n).copied(),
            _ => None,
        };
        let rops = raster_ops_by_channel.remove(&chan).unwrap_or_default();
        let name = chan.display_name();
        plates.push(SeparationPlate {
            channel: chan,
            name,
            width: 0,
            height: 0,
            ink_map: Vec::new(),
            vector_ops: vops,
            raster_ops: rops,
            average_coverage_pct: 0.0,
            max_coverage_pct: 0.0,
            inked_area_pct: 0.0,
            spot_fallback_cmyk: fb,
        });
    }
    Ok(plates)
}

fn accumulate_channel_densities(
    color: &Color,
    opacity: f32,
    out: &mut BTreeMap<SeparationChannel, f32>,
) {
    let a = (opacity * color.alpha()).clamp(0.0, 1.0);
    if a <= 1e-5 {
        return;
    }
    match color {
        Color::Spot { name, tint, .. } => {
            let t = (tint.clamp(0.0, 1.0) * a).clamp(0.0, 1.0);
            if name.eq_ignore_ascii_case("Registration") {
                for ch in [
                    SeparationChannel::Cyan,
                    SeparationChannel::Magenta,
                    SeparationChannel::Yellow,
                    SeparationChannel::Black,
                ] {
                    out.entry(ch).and_modify(|v| *v = (*v).max(t)).or_insert(t);
                }
            } else if t > 1e-4 {
                out.entry(SeparationChannel::Spot(name.clone()))
                    .and_modify(|v| *v = (*v).max(t))
                    .or_insert(t);
            }
        }
        other => {
            let cmyk = other.to_cmyk();
            let chans = [
                SeparationChannel::Cyan,
                SeparationChannel::Magenta,
                SeparationChannel::Yellow,
                SeparationChannel::Black,
            ];
            for (ch, val) in chans.into_iter().zip(cmyk) {
                let d = (val * a).clamp(0.0, 1.0);
                if d > 1e-4 {
                    out.entry(ch).and_modify(|v| *v = (*v).max(d)).or_insert(d);
                }
            }
        }
    }
}

/// Exports vector-separated CMYK + Spot ink plates to a multi-page PDF file where each
/// plate page carries pure vector artwork, `/Separation` / `/DeviceCMYK` colorants,
/// registration/crop marks, and plate name slugs verifiable by `gs -sDEVICE=tiffsep` and `pdftotext`.
pub fn export_separated_plates_pdf(
    doc: &Document,
    page_index: usize,
    options: &SeparationOptions,
    out_path: &Path,
) -> ProdResult<Vec<String>> {
    let (bytes, names) = export_separated_plates_pdf_bytes(doc, page_index, options)?;
    std::fs::write(out_path, &bytes).map_err(|e| {
        ProdError::InvalidParameter(format!(
            "failed to write separated plates PDF '{}': {e}",
            out_path.display()
        ))
    })?;
    Ok(names)
}

/// Exports vector-separated CMYK + Spot plates into an in-memory multi-page PDF byte buffer.
pub fn export_separated_plates_pdf_bytes(
    doc: &Document,
    page_index: usize,
    options: &SeparationOptions,
) -> ProdResult<(Vec<u8>, Vec<String>)> {
    let page = get_page(doc, page_index)?;
    validate_positive("page.width_mm", page.width_mm)?;
    validate_positive("page.height_mm", page.height_mm)?;
    let plates = compute_vector_separations(doc, page_index, options.include_cut_layers)?;

    const MM_TO_PT: f64 = 72.0 / 25.4;
    let margin_mm = 12.0;
    let w_pt = (page.width_mm + 2.0 * margin_mm) * MM_TO_PT;
    let h_pt = (page.height_mm + 2.0 * margin_mm) * MM_TO_PT;
    let mx_pt = margin_mm * MM_TO_PT;
    let my_pt = margin_mm * MM_TO_PT;

    let mut pdf = Vec::<u8>::new();
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets: Vec<usize> = Vec::new();
    let push_obj = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| -> usize {
        offsets.push(pdf.len());
        let obj_num = offsets.len();
        pdf.extend_from_slice(format!("{obj_num} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
        obj_num
    };

    // Object 1: Helvetica font for plate slugs
    let font_obj = push_obj(
        &mut pdf,
        &mut offsets,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    let gs_op_obj = push_obj(
        &mut pdf,
        &mut offsets,
        b"<< /Type /ExtGState /op true /OP true /OPM 1 >>",
    );
    let gs_ko_obj = push_obj(
        &mut pdf,
        &mut offsets,
        b"<< /Type /ExtGState /op false /OP false /OPM 0 >>",
    );

    let mut plate_names = Vec::with_capacity(plates.len());
    let mut page_objs = Vec::with_capacity(plates.len());

    for plate in &plates {
        plate_names.push(plate.name.clone());
        let mut cs_dict = String::new();
        let cs_name = "CSInk";
        if let SeparationChannel::Spot(sname) = &plate.channel {
            let fb = plate.spot_fallback_cmyk.unwrap_or([0.0, 1.0, 0.0, 0.0]);
            let esc = sname.replace(' ', "#20");
            let spot_fn_obj = push_obj(
                &mut pdf,
                &mut offsets,
                format!(
                    "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [{:.4} {:.4} {:.4} {:.4}] /N 1 >>",
                    fb[0], fb[1], fb[2], fb[3]
                )
                .as_bytes(),
            );
            let cs_obj = push_obj(
                &mut pdf,
                &mut offsets,
                format!("[/Separation /{esc} /DeviceCMYK {spot_fn_obj} 0 R]").as_bytes(),
            );
            cs_dict = format!("/ColorSpace << /{cs_name} {cs_obj} 0 R >> ");
        }

        let mut xobject_entries: Vec<String> = Vec::new();
        let mut stream = String::new();
        let slug = format!(
            "coree Separation Plate: {} ({:.1}x{:.1} mm)",
            plate.name, page.width_mm, page.height_mm
        );
        let safe_slug = slug.replace('(', "\\(").replace(')', "\\)");
        stream.push_str("q\n");
        stream.push_str(&format!(
            "BT /F1 8 Tf 0 0 0 1 k {mx_pt:.3} {:.3} Td ({safe_slug}) Tj ET\n",
            my_pt - 9.0
        ));
        // Crop marks around page trim box
        let trim_r = mx_pt + page.width_mm * MM_TO_PT;
        let trim_t = my_pt + page.height_mm * MM_TO_PT;
        stream.push_str("0.25 w 0 0 0 1 K\n");
        for (x, y, dx, dy) in [
            (mx_pt, my_pt, -8.0, -8.0),
            (trim_r, my_pt, 8.0, -8.0),
            (mx_pt, trim_t, -8.0, 8.0),
            (trim_r, trim_t, 8.0, 8.0),
        ] {
            stream.push_str(&format!(
                "{:.3} {y:.3} m {:.3} {y:.3} l S\n{x:.3} {:.3} m {x:.3} {:.3} l S\n",
                x + dx * 0.25,
                x + dx,
                y + dy * 0.25,
                y + dy
            ));
        }

        let to_pdf_xy = |p: Point2D| -> (f64, f64) {
            (
                mx_pt + p.x * MM_TO_PT,
                my_pt + (page.height_mm - p.y) * MM_TO_PT,
            )
        };
        let encode_path = |path: &BezierPath| -> String {
            let mut ops = String::new();
            let kb = path.to_kurbo();
            let mut cur_pt = kurbo::Point::ZERO;
            let mut subpath_start = kurbo::Point::ZERO;
            for el in kb.elements() {
                match *el {
                    PathEl::MoveTo(p) => {
                        cur_pt = p;
                        subpath_start = p;
                        let (x, y) = to_pdf_xy(Point2D::new(p.x, p.y));
                        ops.push_str(&format!("{x:.3} {y:.3} m\n"));
                    }
                    PathEl::LineTo(p) => {
                        cur_pt = p;
                        let (x, y) = to_pdf_xy(Point2D::new(p.x, p.y));
                        ops.push_str(&format!("{x:.3} {y:.3} l\n"));
                    }
                    PathEl::QuadTo(p1, p2) => {
                        let cubic = kurbo::QuadBez::new(cur_pt, p1, p2).raise();
                        cur_pt = p2;
                        let (x1, y1) = to_pdf_xy(Point2D::new(cubic.p1.x, cubic.p1.y));
                        let (x2, y2) = to_pdf_xy(Point2D::new(cubic.p2.x, cubic.p2.y));
                        let (x3, y3) = to_pdf_xy(Point2D::new(cubic.p3.x, cubic.p3.y));
                        ops.push_str(&format!(
                            "{x1:.3} {y1:.3} {x2:.3} {y2:.3} {x3:.3} {y3:.3} c\n"
                        ));
                    }
                    PathEl::CurveTo(p1, p2, p3) => {
                        cur_pt = p3;
                        let (x1, y1) = to_pdf_xy(Point2D::new(p1.x, p1.y));
                        let (x2, y2) = to_pdf_xy(Point2D::new(p2.x, p2.y));
                        let (x3, y3) = to_pdf_xy(Point2D::new(p3.x, p3.y));
                        ops.push_str(&format!(
                            "{x1:.3} {y1:.3} {x2:.3} {y2:.3} {x3:.3} {y3:.3} c\n"
                        ));
                    }
                    PathEl::ClosePath => {
                        cur_pt = subpath_start;
                        ops.push_str("h\n");
                    }
                }
            }
            ops
        };
        let emit_ink_cmd = |density: f32, stroke: bool| -> String {
            let d = density.clamp(0.0, 1.0);
            match &plate.channel {
                SeparationChannel::Cyan => {
                    format!("{d:.4} 0 0 0 {}", if stroke { "K" } else { "k" })
                }
                SeparationChannel::Magenta => {
                    format!("0 {d:.4} 0 0 {}", if stroke { "K" } else { "k" })
                }
                SeparationChannel::Yellow => {
                    format!("0 0 {d:.4} 0 {}", if stroke { "K" } else { "k" })
                }
                SeparationChannel::Black => {
                    format!("0 0 0 {d:.4} {}", if stroke { "K" } else { "k" })
                }
                SeparationChannel::Spot(_) => {
                    format!(
                        "/{cs_name} {} {d:.4} {}",
                        if stroke { "CS" } else { "cs" },
                        if stroke { "SCN" } else { "scn" }
                    )
                }
            }
        };

        #[derive(Clone, Copy)]
        enum PlateDrawItem {
            Vec(usize, usize),
            Ras(usize, usize),
        }
        let mut draw_order: Vec<PlateDrawItem> =
            Vec::with_capacity(plate.vector_ops.len() + plate.raster_ops.len());
        for (i, v) in plate.vector_ops.iter().enumerate() {
            draw_order.push(PlateDrawItem::Vec(v.z_order, i));
        }
        for (i, r) in plate.raster_ops.iter().enumerate() {
            draw_order.push(PlateDrawItem::Ras(r.z_order, i));
        }
        draw_order.sort_by_key(|item| match *item {
            PlateDrawItem::Vec(z, i) => (z, 0u8, i),
            PlateDrawItem::Ras(z, i) => (z, 1u8, i),
        });

        for item in draw_order {
            match item {
                PlateDrawItem::Vec(_, idx) => {
                    let prim = &plate.vector_ops[idx];
                    let path_ops = encode_path(&prim.path);
                    if path_ops.is_empty() {
                        continue;
                    }
                    stream.push_str("q\n");
                    for cp in &prim.clip_paths {
                        let cp_ops = encode_path(cp);
                        if !cp_ops.is_empty() {
                            stream.push_str(&cp_ops);
                            stream.push_str("W* n\n");
                        }
                    }
                    stream.push_str(if prim.overprint {
                        "/GSop gs\n"
                    } else {
                        "/GSko gs\n"
                    });
                    if let Some(fd) = prim.fill_density {
                        stream.push_str(&emit_ink_cmd(fd, false));
                        stream.push('\n');
                        stream.push_str(&path_ops);
                        stream.push_str("f*\n");
                    }
                    if let Some(sd) = prim.stroke_density {
                        let lw = (prim.stroke_width_mm * MM_TO_PT).max(0.25);
                        stream.push_str(&format!("{lw:.3} w\n"));
                        stream.push_str(&emit_ink_cmd(sd, true));
                        stream.push('\n');
                        stream.push_str(&path_ops);
                        stream.push_str("S\n");
                    }
                    stream.push_str("Q\n");
                }
                PlateDrawItem::Ras(_, idx) => {
                    let rprim = &plate.raster_ops[idx];
                    let px_count = (rprim.width as usize).saturating_mul(rprim.height as usize);
                    if px_count == 0 || rprim.ink_channel.len() < px_count {
                        continue;
                    }
                    let smask_ref = if rprim.alpha_mask.len() >= px_count {
                        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
                        enc.write_all(&rprim.alpha_mask[..px_count]).map_err(|e| {
                            ProdError::InvalidParameter(format!("zlib compress smask failed: {e}"))
                        })?;
                        let comp = enc.finish().map_err(|e| {
                            ProdError::InvalidParameter(format!("zlib finish smask failed: {e}"))
                        })?;
                        let mut sb = format!(
                            "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
                             /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
                            rprim.width,
                            rprim.height,
                            comp.len()
                        )
                        .into_bytes();
                        sb.extend_from_slice(&comp);
                        sb.extend_from_slice(b"\nendstream");
                        let sm_id = push_obj(&mut pdf, &mut offsets, &sb);
                        format!(" /SMask {sm_id} 0 R")
                    } else {
                        String::new()
                    };
                    let mut cmyk_bytes = Vec::with_capacity(px_count * 4);
                    let ch_slot = match &plate.channel {
                        SeparationChannel::Cyan => Some(0usize),
                        SeparationChannel::Magenta => Some(1usize),
                        SeparationChannel::Yellow => Some(2usize),
                        SeparationChannel::Black => Some(3usize),
                        SeparationChannel::Spot(_) => None,
                    };
                    for &v in &rprim.ink_channel[..px_count] {
                        let mut px = [0u8; 4];
                        if let Some(s) = ch_slot {
                            px[s] = v;
                        }
                        cmyk_bytes.extend_from_slice(&px);
                    }
                    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
                    enc.write_all(&cmyk_bytes).map_err(|e| {
                        ProdError::InvalidParameter(format!(
                            "zlib compress raster plate failed: {e}"
                        ))
                    })?;
                    let comp = enc.finish().map_err(|e| {
                        ProdError::InvalidParameter(format!("zlib finish raster plate failed: {e}"))
                    })?;
                    let mut ib = format!(
                        "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
                         /ColorSpace /DeviceCMYK /BitsPerComponent 8{smask_ref} /Filter /FlateDecode /Length {} >>\nstream\n",
                        rprim.width,
                        rprim.height,
                        comp.len()
                    )
                    .into_bytes();
                    ib.extend_from_slice(&comp);
                    ib.extend_from_slice(b"\nendstream");
                    let im_id = push_obj(&mut pdf, &mut offsets, &ib);
                    let im_name = format!("Im{}", xobject_entries.len() + 1);
                    xobject_entries.push(format!("/{im_name} {im_id} 0 R"));

                    let (x0, y0) = to_pdf_xy(rprim.bl);
                    let (x1, y1) = to_pdf_xy(rprim.br);
                    let (x2, y2) = to_pdf_xy(rprim.tl);
                    stream.push_str("q\n");
                    for cp in &rprim.clip_paths {
                        let cp_ops = encode_path(cp);
                        if !cp_ops.is_empty() {
                            stream.push_str(&cp_ops);
                            stream.push_str("W* n\n");
                        }
                    }
                    stream.push_str(if rprim.overprint {
                        "/GSop gs\n"
                    } else {
                        "/GSko gs\n"
                    });
                    stream.push_str(&format!(
                        "{:.4} {:.4} {:.4} {:.4} {:.4} {:.4} cm /{im_name} Do\nQ\n",
                        x1 - x0,
                        y1 - y0,
                        x2 - x0,
                        y2 - y0,
                        x0,
                        y0
                    ));
                }
            }
        }
        stream.push_str("Q\n");

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(stream.as_bytes()).map_err(|e| {
            ProdError::InvalidParameter(format!("zlib compress separation plate failed: {e}"))
        })?;
        let compressed = enc.finish().map_err(|e| {
            ProdError::InvalidParameter(format!("zlib finish separation plate failed: {e}"))
        })?;
        let mut s_body = format!(
            "<< /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        )
        .into_bytes();
        s_body.extend_from_slice(&compressed);
        s_body.extend_from_slice(b"\nendstream");
        let content_obj = push_obj(&mut pdf, &mut offsets, &s_body);

        let xobj_dict = if xobject_entries.is_empty() {
            String::new()
        } else {
            format!("/XObject << {} >> ", xobject_entries.join(" "))
        };
        let gs_dict = format!("/ExtGState << /GSop {gs_op_obj} 0 R /GSko {gs_ko_obj} 0 R >> ");
        let page_body = format!(
            "<< /Type /Page /Parent {{PAGES_REF}} /MediaBox [0 0 {w_pt:.3} {h_pt:.3}] \
             /Resources << /Font << /F1 {font_obj} 0 R >> {gs_dict}{cs_dict}{xobj_dict}>> /Contents {content_obj} 0 R >>"
        );
        page_objs.push((offsets.len(), page_body));
        offsets.push(0); // placeholder for page object
    }

    let pages_obj_num = offsets.len() + 1;
    let kids: String = (0..page_objs.len())
        .map(|i| format!("{} 0 R", page_objs[i].0 + 1))
        .collect::<Vec<_>>()
        .join(" ");
    for (slot_idx, raw_body) in page_objs {
        offsets[slot_idx] = pdf.len();
        let obj_num = slot_idx + 1;
        let resolved = raw_body.replace("{PAGES_REF}", &format!("{pages_obj_num} 0 R"));
        pdf.extend_from_slice(format!("{obj_num} 0 obj\n{resolved}\nendobj\n").as_bytes());
    }
    let pages_obj = push_obj(
        &mut pdf,
        &mut offsets,
        format!(
            "<< /Type /Pages /Count {} /Kids [{kids}] >>",
            plate_names.len()
        )
        .as_bytes(),
    );
    let catalog_obj = push_obj(
        &mut pdf,
        &mut offsets,
        format!("<< /Type /Catalog /Pages {pages_obj} 0 R >>").as_bytes(),
    );

    let xref_offset = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {catalog_obj} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    Ok((pdf, plate_names))
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
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    spots: &mut BTreeMap<String, [f32; 4]>,
) {
    if depth > MAX_SCENE_DEPTH || (id.0 != 0 && !visiting.insert(id)) {
        return;
    }
    let obj_opt = sym_ctx
        .and_then(|c| c.sym_map.get(&id).copied())
        .or_else(|| doc.object(id));
    if let Some(obj) = obj_opt {
        collect_spots_for_obj(doc, obj, sym_ctx, depth, visiting, spots);
    }
    if id.0 != 0 {
        visiting.remove(&id);
    }
}

fn collect_spots_for_obj(
    doc: &Document,
    obj: &SceneObject,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    spots: &mut BTreeMap<String, [f32; 4]>,
) {
    if depth > MAX_SCENE_DEPTH || !obj.visible || !obj.printable {
        return;
    }
    for c in obj.fill.referenced_colors() {
        register_spot_color(&c, spots);
    }
    if obj.outline.is_visible() {
        register_spot_color(&obj.outline.color, spots);
    }
    for eff in &obj.effects {
        match eff {
            Effect::DropShadow { color, .. }
            | Effect::InnerShadow { color, .. }
            | Effect::BlockShadow { color, .. } => register_spot_color(color, spots),
            Effect::Contour {
                fill_color,
                outline_color,
                ..
            } => {
                register_spot_color(fill_color, spots);
                register_spot_color(outline_color, spots);
            }
            Effect::Bevel {
                highlight_color,
                shadow_color,
                ..
            } => {
                register_spot_color(highlight_color, spots);
                register_spot_color(shadow_color, spots);
            }
            Effect::Extrude25D { shade_color, .. } | Effect::Extrude3D { shade_color, .. } => {
                register_spot_color(shade_color, spots);
            }
            _ => {}
        }
    }
    match &obj.payload {
        NodePayload::Clone { .. } => {
            if let Some((p, f, o, in_sym)) =
                doc.resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|c| c.sym_map))
            {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                collect_spots_for_obj(
                    doc,
                    &tmp,
                    if in_sym { sym_ctx } else { None },
                    depth + 1,
                    visiting,
                    spots,
                );
            }
        }
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
        NodePayload::Table(table) => {
            if table.border_style.is_visible() {
                register_spot_color(&table.border_style.color, spots);
            }
            for cell in &table.cells {
                if !cell.is_covered() {
                    for c in cell.fill.referenced_colors() {
                        register_spot_color(&c, spots);
                    }
                    register_spot_color(&cell.text_style.fill, spots);
                }
            }
        }
        NodePayload::Dimension(d) => {
            register_spot_color(&d.text_style.fill, spots);
        }
        NodePayload::Group(children) => {
            for &cid in children {
                collect_spots_recursive(doc, cid, sym_ctx, depth + 1, visiting, spots);
            }
        }
        NodePayload::PowerClip { contents, .. } => {
            for &cid in contents {
                collect_spots_recursive(doc, cid, sym_ctx, depth + 1, visiting, spots);
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                let mut local_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for (idx, sub) in sym.objects.iter().enumerate() {
                    if sub.id.0 != 0 {
                        local_map.insert(sub.id, sub);
                    }
                    let pref = if sub.id.0 != 0 {
                        SymParentRef::Id(sub.id)
                    } else {
                        SymParentRef::RootIndex(idx)
                    };
                    match &sub.payload {
                        NodePayload::Group(c) => {
                            for &cid in c {
                                referenced.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(pref);
                                }
                            }
                        }
                        NodePayload::PowerClip { contents, .. } => {
                            for &cid in contents {
                                referenced.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(pref);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let ctx = SymBoundsCtx {
                    root_xf: &Affine2D::IDENTITY,
                    sym_objects: &sym.objects,
                    sym_map: &local_map,
                    sym_parent_map: &sym_parent_map,
                };
                let mut sym_visiting = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        continue;
                    }
                    if sub.id.0 != 0 && !sym_visiting.insert(sub.id) {
                        continue;
                    }
                    collect_spots_for_obj(
                        doc,
                        sub,
                        Some(&ctx),
                        depth + 1,
                        &mut sym_visiting,
                        spots,
                    );
                    if sub.id.0 != 0 {
                        sym_visiting.remove(&sub.id);
                    }
                }
            }
        }
        _ => {}
    }
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
    page_index: usize,
    width: u32,
    height: u32,
    scale_x: f32,
    scale_y: f32,
    spot_index: &'a BTreeMap<String, usize>,
    buffers: &'a mut [Vec<f32>],
    text_outliner: &'a mut dyn FnMut(&SceneObject, &Document) -> Option<BezierPath>,
}

fn rasterize_object_separation(
    ctx: &mut SeparationRasterCtx<'_>,
    id: ObjectId,
    parent_xf: Affine2D,
    clip_mask: Option<&[f32]>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
) {
    rasterize_object_separation_sym(ctx, id, parent_xf, None, clip_mask, depth, visiting);
}

fn rasterize_object_separation_sym(
    ctx: &mut SeparationRasterCtx<'_>,
    id: ObjectId,
    parent_xf: Affine2D,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    clip_mask: Option<&[f32]>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
) {
    if depth > MAX_SCENE_DEPTH || (id.0 != 0 && !visiting.insert(id)) {
        return;
    }
    let obj_opt = sym_ctx
        .and_then(|c| c.sym_map.get(&id).copied())
        .or_else(|| ctx.doc.object(id));
    if let Some(obj) = obj_opt {
        rasterize_scene_obj_separation(ctx, obj, parent_xf, sym_ctx, clip_mask, depth, visiting);
    }
    if id.0 != 0 {
        visiting.remove(&id);
    }
}

fn rasterize_scene_obj_separation(
    ctx: &mut SeparationRasterCtx<'_>,
    obj: &SceneObject,
    parent_xf: Affine2D,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    clip_mask: Option<&[f32]>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
) {
    if depth > MAX_SCENE_DEPTH
        || !obj.visible
        || !obj.printable
        || !obj.opacity.is_finite()
        || obj.opacity <= 1e-6
    {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return;
    }

    let needs_isolated_raster = obj.lens.is_some()
        || !obj.effects.is_empty()
        || matches!(
            obj.fill,
            Fill::VectorPattern { .. }
                | Fill::BitmapPattern { .. }
                | Fill::ConicalGradient { .. }
                | Fill::SquareGradient { .. }
                | Fill::TwoColorPattern { .. }
                | Fill::TextureFill { .. }
                | Fill::MeshFill { .. }
        );
    if needs_isolated_raster {
        let dpi = (f64::from(ctx.scale_x) * 25.4).max(12.0);
        let iso_res = if sym_ctx.is_none() && obj.id.0 != 0 && ctx.doc.object(obj.id).is_some() {
            render_isolated_object_to_rgba_with_options(
                ctx.doc,
                ctx.page_index,
                obj.id,
                parent_xf,
                dpi,
                false,
            )
            .ok()
            .flatten()
        } else {
            render_isolated_sym_object_to_rgba(ctx, obj, parent_xf, sym_ctx, dpi)
        };
        if let Some((bb, buf)) = iso_res {
            paint_rgba_quad_into_buffers(ctx, &buf.rgba, buf.width, buf.height, bb, clip_mask);
            return;
        }
    }

    match &obj.payload {
        NodePayload::Clone { .. } => {
            if let Some((p, f, o, in_sym)) = ctx
                .doc
                .resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|c| c.sym_map))
            {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                rasterize_scene_obj_separation(
                    ctx,
                    &tmp,
                    parent_xf,
                    if in_sym { sym_ctx } else { None },
                    clip_mask,
                    depth + 1,
                    visiting,
                );
            }
            return;
        }
        NodePayload::Group(children) => {
            for &cid in children {
                rasterize_object_separation_sym(
                    ctx,
                    cid,
                    world_xf,
                    sym_ctx,
                    clip_mask,
                    depth + 1,
                    visiting,
                );
            }
            return;
        }
        NodePayload::PowerClip {
            container,
            contents,
            ..
        } => {
            let local_c = apply_obj_perspective_to_local_path((**container).clone(), obj);
            let world_container = local_c.transformed(&world_xf);
            paint_vector_object(ctx, obj, &world_container, &world_xf, clip_mask);
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
                    rasterize_object_separation_sym(
                        ctx,
                        cid,
                        world_xf,
                        sym_ctx,
                        Some(&combined_mask),
                        depth + 1,
                        visiting,
                    );
                }
            }
            return;
        }
        NodePayload::Raster(raster) => {
            paint_raster_object(ctx, obj, raster, &world_xf, clip_mask);
            return;
        }
        NodePayload::Table(table) => {
            let overprint = obj.blend_mode == BlendMode::Multiply;
            for r in 0..table.rows {
                for c in 0..table.cols {
                    let Some(cell) = table.cell(r, c) else {
                        continue;
                    };
                    if cell.is_covered() || !cell.fill.is_visible() {
                        continue;
                    }
                    if let Some(cr) = table.cell_rect(r, c) {
                        let cell_path =
                            apply_obj_perspective_to_local_path(BezierPath::from_rect(cr), obj)
                                .transformed(&world_xf);
                        paint_fill(
                            ctx,
                            &cell_path,
                            &world_xf,
                            &cell.fill,
                            obj.opacity,
                            overprint,
                            clip_mask,
                        );
                    }
                }
            }
            let grid_path = apply_obj_perspective_to_local_path(table.to_grid_paths(), obj)
                .transformed(&world_xf);
            if table.border_style.is_visible() {
                paint_stroke(
                    ctx,
                    &grid_path,
                    &table.border_style,
                    obj.opacity,
                    table.border_style.overprint || overprint,
                    clip_mask,
                );
            } else if obj.outline.is_visible() {
                paint_stroke(
                    ctx,
                    &grid_path,
                    &obj.outline,
                    obj.opacity,
                    obj.outline.overprint || overprint,
                    clip_mask,
                );
            }
            return;
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = ctx.doc.symbol(*sym_id) {
                let mut local_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for (idx, sub) in sym.objects.iter().enumerate() {
                    if sub.id.0 != 0 {
                        local_map.insert(sub.id, sub);
                    }
                    let pref = if sub.id.0 != 0 {
                        SymParentRef::Id(sub.id)
                    } else {
                        SymParentRef::RootIndex(idx)
                    };
                    match &sub.payload {
                        NodePayload::Group(c) => {
                            for &cid in c {
                                referenced.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(pref);
                                }
                            }
                        }
                        NodePayload::PowerClip { contents, .. } => {
                            for &cid in contents {
                                referenced.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(pref);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let s_ctx = SymBoundsCtx {
                    root_xf: &world_xf,
                    sym_objects: &sym.objects,
                    sym_map: &local_map,
                    sym_parent_map: &sym_parent_map,
                };
                let mut sym_vis = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        continue;
                    }
                    if sub.id.0 != 0 && !sym_vis.insert(sub.id) {
                        continue;
                    }
                    rasterize_scene_obj_separation(
                        ctx,
                        sub,
                        world_xf,
                        Some(&s_ctx),
                        clip_mask,
                        depth + 1,
                        &mut sym_vis,
                    );
                    if sub.id.0 != 0 {
                        sym_vis.remove(&sub.id);
                    }
                }
            }
            return;
        }
        _ => {}
    }

    let world_path = match &obj.payload {
        NodePayload::Path(p) => {
            Some(apply_obj_perspective_to_local_path(p.clone(), obj).transformed(&world_xf))
        }
        NodePayload::Shape(s) => Some(
            apply_obj_perspective_to_local_path(s.to_bezier_path(), obj).transformed(&world_xf),
        ),
        NodePayload::BSpline(bs) => Some(
            apply_obj_perspective_to_local_path(bs.to_bezier_path(), obj).transformed(&world_xf),
        ),
        NodePayload::ArtisticMedia { spine, style } => {
            let expanded = coree_core::ArtisticMedia::expand_stroke(spine, style);
            Some(apply_obj_perspective_to_local_path(expanded, obj).transformed(&world_xf))
        }
        NodePayload::ArtisticText(t) => outline_text_path_for_separation(
            ctx,
            obj,
            &world_xf,
            t.estimated_bounds(),
            !t.plain_text().trim().is_empty(),
        ),
        NodePayload::ParagraphText(t) => outline_text_path_for_separation(
            ctx,
            obj,
            &world_xf,
            t.bounding_box(),
            !t.plain_text().trim().is_empty(),
        ),
        NodePayload::TextOnPath(top) => outline_text_path_for_separation(
            ctx,
            obj,
            &world_xf,
            top.text.estimated_bounds(),
            !top.text.plain_text().trim().is_empty(),
        ),
        NodePayload::Dimension(d) => Some(
            apply_obj_perspective_to_local_path(BezierPath::from_rect(d.bounding_box()), obj)
                .transformed(&world_xf),
        ),
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
}

fn paint_rgba_quad_into_buffers(
    ctx: &mut SeparationRasterCtx<'_>,
    rgba: &[u8],
    rw: u32,
    rh: u32,
    world_bb: Rect2D,
    clip_mask: Option<&[f32]>,
) {
    if rw == 0
        || rh == 0
        || !world_bb.is_finite()
        || world_bb.width() <= 1e-6
        || world_bb.height() <= 1e-6
    {
        return;
    }
    let px0 = ((world_bb.min_x * f64::from(ctx.scale_x)).floor() as i64)
        .clamp(0, i64::from(ctx.width)) as u32;
    let py0 = ((world_bb.min_y * f64::from(ctx.scale_y)).floor() as i64)
        .clamp(0, i64::from(ctx.height)) as u32;
    let px1 = ((world_bb.max_x * f64::from(ctx.scale_x)).ceil() as i64)
        .clamp(0, i64::from(ctx.width)) as u32;
    let py1 = ((world_bb.max_y * f64::from(ctx.scale_y)).ceil() as i64)
        .clamp(0, i64::from(ctx.height)) as u32;
    let inv_sx = 1.0 / f64::from(ctx.scale_x);
    let inv_sy = 1.0 / f64::from(ctx.scale_y);
    for py in py0..py1 {
        let y_mm = (f64::from(py) + 0.5) * inv_sy;
        let v = (((y_mm - world_bb.min_y) / world_bb.height()) * f64::from(rh))
            .floor()
            .clamp(0.0, f64::from(rh - 1)) as usize;
        for px in px0..px1 {
            let idx = (py as usize) * (ctx.width as usize) + (px as usize);
            let clip_a = clip_mask.map_or(1.0, |cm| cm[idx]);
            if clip_a <= 1e-5 {
                continue;
            }
            let x_mm = (f64::from(px) + 0.5) * inv_sx;
            let u = (((x_mm - world_bb.min_x) / world_bb.width()) * f64::from(rw))
                .floor()
                .clamp(0.0, f64::from(rw - 1)) as usize;
            let si = (v * (rw as usize) + u) * 4;
            if si + 3 < rgba.len() && rgba[si + 3] > 0 {
                let col = Color::rgba8(rgba[si], rgba[si + 1], rgba[si + 2], rgba[si + 3]);
                composite_pixel_color(ctx.buffers, ctx.spot_index, idx, &col, clip_a, false);
            }
        }
    }
}

fn render_isolated_sym_object_to_rgba(
    ctx: &SeparationRasterCtx<'_>,
    obj: &SceneObject,
    parent_xf: Affine2D,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    dpi: f64,
) -> Option<(Rect2D, coree_render::RgbaBuffer)> {
    let page = ctx.doc.page(ctx.page_index)?;
    let mut tmp_doc = clone_document_shell(ctx.doc, "SepSymIso");
    let p_id = tmp_doc.add_page("P1", page.width_mm, page.height_mm, page.bleed_mm);
    let l_id = tmp_doc.page_by_id(p_id)?.layers[0].id;
    if let Some(sc) = sym_ctx {
        for sub in sc.sym_objects {
            if sub.id.0 != 0 {
                let _ = tmp_doc.insert_loaded_object(sub.clone());
            }
        }
    }
    let mut obj_copy = obj.clone();
    if obj_copy.id.0 == 0 {
        obj_copy.id = tmp_doc.alloc_object_id();
    }
    let target_id = obj_copy.id;
    let _ = tmp_doc.insert_loaded_object(obj_copy);
    if let Some(layer) = tmp_doc.find_layer_mut(l_id) {
        layer.objects.push(target_id);
    }
    render_isolated_object_to_rgba_with_options(&tmp_doc, 0, target_id, parent_xf, dpi, false)
        .ok()
        .flatten()
}

fn outline_text_path_for_separation(
    ctx: &mut SeparationRasterCtx<'_>,
    obj: &SceneObject,
    world_xf: &Affine2D,
    fallback_rect: Rect2D,
    has_non_whitespace: bool,
) -> Option<BezierPath> {
    if let Some(local_path) = (ctx.text_outliner)(obj, ctx.doc) {
        if local_path.is_finite() {
            if !local_path.is_empty() {
                let warped = apply_obj_perspective_to_local_path(local_path, obj);
                return Some(warped.transformed(world_xf));
            }
            if !has_non_whitespace {
                return None;
            }
        }
    }
    let fb = apply_obj_perspective_to_local_path(BezierPath::from_rect(fallback_rect), obj);
    Some(fb.transformed(world_xf))
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
    let Some(raw_asset) = ctx.doc.asset(raster.asset_id) else {
        return;
    };
    let active_filters = raster.active_filters();
    let filtered = (!active_filters.is_empty())
        .then(|| coree_render::apply_raster_filters(raw_asset, &active_filters));
    let asset = filtered.as_ref().unwrap_or(raw_asset);
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
            let Some([r, g, b, a]) = asset.read_pixel_rgba8(u as u32, v as u32) else {
                continue;
            };
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
        vector_ops: Vec::new(),
        raster_ops: Vec::new(),
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
    /// Perfect-bound book signature imposition (`signature_pages` pages per folded signature,
    /// progressive spine/creep & milling allowance compensation).
    PerfectBound {
        sheet_w_mm: f64,
        sheet_h_mm: f64,
        signature_pages: usize,
        spine_thickness_mm: f64,
        creep_mm: f64,
        milling_allowance_mm: f64,
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
    if doc.pages().is_empty() {
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
        ImpositionMode::PerfectBound {
            sheet_w_mm,
            sheet_h_mm,
            signature_pages,
            spine_thickness_mm,
            creep_mm,
            milling_allowance_mm,
        } => impose_perfect_bound(
            doc,
            sheet_w_mm,
            sheet_h_mm,
            signature_pages,
            spine_thickness_mm,
            creep_mm,
            milling_allowance_mm,
        ),
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
    let src_w = doc.pages()[0].width_mm;
    let src_h = doc.pages()[0].height_mm;
    validate_positive("source page width_mm", src_w)?;
    validate_positive("source page height_mm", src_h)?;

    let grid_w = f64::from(cols) * src_w + f64::from(cols - 1) * gap_x_mm;
    let grid_h = f64::from(rows) * src_h + f64::from(rows - 1) * gap_y_mm;
    let origin_x = (sheet_w_mm - grid_w) * 0.5;
    let origin_y = (sheet_h_mm - grid_h) * 0.5;

    // Step-and-repeat for 1-page designs; sequential flow for multi-page documents.
    let slot_sources: Vec<usize> = if doc.pages().len() == 1 {
        vec![0; per_sheet]
    } else {
        (0..doc.pages().len()).collect()
    };

    let sheet_count = slot_sources.len().div_ceil(per_sheet);
    if sheet_count > MAX_IMPOSITION_PAGES {
        return Err(ProdError::InvalidParameter(format!(
            "N-Up imposition would produce {sheet_count} sheets (exceeds cap {MAX_IMPOSITION_PAGES})"
        )));
    }
    let mut out = clone_document_shell(doc, format!("{} (N-Up {}x{})", doc.title(), cols, rows));

    for sheet_idx in 0..sheet_count {
        out.add_page(
            format!("Sheet {}", sheet_idx + 1),
            sheet_w_mm,
            sheet_h_mm,
            0.0,
        );
        let dst_page_idx = out.pages().len() - 1;

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
    for p in doc.pages() {
        validate_positive("page.width_mm", p.width_mm)?;
        validate_positive("page.height_mm", p.height_mm)?;
    }

    let spreads = booklet_signature_order(doc.pages().len());
    if spreads.len() > MAX_IMPOSITION_PAGES {
        return Err(ProdError::InvalidParameter(format!(
            "booklet imposition would produce {} spreads (exceeds cap {MAX_IMPOSITION_PAGES})",
            spreads.len()
        )));
    }
    let half_w = sheet_w_mm * 0.5;
    let mut out = clone_document_shell(doc, format!("{} (Booklet)", doc.title()));

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
        let dst_page_idx = out.pages().len() - 1;

        // Place left half flush against the center spine (x = half_w)
        if let Some(src_idx) = *left_opt {
            let p = &doc.pages()[src_idx];
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
            let p = &doc.pages()[src_idx];
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
    let mut page_grids: Vec<(usize, usize)> = Vec::with_capacity(doc.pages().len());
    let mut total_tiles = 0usize;
    for src_page in doc.pages() {
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

    let mut out = clone_document_shell(doc, format!("{} (Tiled)", doc.title()));

    for (src_idx, src_page) in doc.pages().iter().enumerate() {
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
                let dst_page_idx = out.pages().len() - 1;
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

fn impose_perfect_bound(
    doc: &Document,
    sheet_w_mm: f64,
    sheet_h_mm: f64,
    signature_pages: usize,
    spine_thickness_mm: f64,
    creep_mm: f64,
    milling_allowance_mm: f64,
) -> ProdResult<Document> {
    validate_positive("sheet_w_mm", sheet_w_mm)?;
    validate_positive("sheet_h_mm", sheet_h_mm)?;
    validate_non_negative("spine_thickness_mm", spine_thickness_mm)?;
    validate_non_negative("creep_mm", creep_mm)?;
    validate_non_negative("milling_allowance_mm", milling_allowance_mm)?;
    if signature_pages < 4 || !signature_pages.is_multiple_of(4) {
        return Err(ProdError::InvalidParameter(
            "perfect_bound signature_pages must be a multiple of 4 (>= 4)".into(),
        ));
    }
    for p in doc.pages() {
        validate_positive("page.width_mm", p.width_mm)?;
        validate_positive("page.height_mm", p.height_mm)?;
    }

    let total_pages = doc.pages().len();
    let num_sigs = total_pages.div_ceil(signature_pages);
    let half_w = sheet_w_mm * 0.5;
    let gutter_half = (milling_allowance_mm * 0.5).max(0.0);
    let mut out = clone_document_shell(doc, format!("{} (PerfectBound)", doc.title()));

    for sig_idx in 0..num_sigs {
        let sig_start = sig_idx * signature_pages;
        let sig_end = (sig_start + signature_pages).min(total_pages);
        let sig_count = sig_end - sig_start;
        let spreads = booklet_signature_order(sig_count);
        let sig_sheets = spreads.len() / 2;

        for (spread_idx, (left_opt, right_opt)) in spreads.iter().enumerate() {
            if out.pages().len() >= MAX_IMPOSITION_PAGES {
                return Err(ProdError::InvalidParameter(format!(
                    "perfect_bound imposition exceeds cap {MAX_IMPOSITION_PAGES}"
                )));
            }
            let sheet_in_sig = spread_idx / 2;
            let side = if spread_idx % 2 == 0 { "Front" } else { "Back" };
            let creep_shift = if sig_sheets > 1 {
                creep_mm * (sheet_in_sig as f64 / (sig_sheets - 1) as f64)
            } else {
                0.0
            };
            let spine_offset = gutter_half + creep_shift * 0.5;
            let page_name = format!(
                "Sig {} Sheet {} {} (spine={:.2}mm)",
                sig_idx + 1,
                sheet_in_sig + 1,
                side,
                spine_thickness_mm
            );
            out.add_page(page_name, sheet_w_mm, sheet_h_mm, 0.0);
            let dst_page_idx = out.pages().len() - 1;

            if let Some(rel_idx) = *left_opt {
                let src_idx = sig_start + rel_idx;
                let p = &doc.pages()[src_idx];
                let scale = ((half_w - spine_offset).max(1.0) / p.width_mm)
                    .min(sheet_h_mm / p.height_mm)
                    .min(1.0);
                let placed_w = p.width_mm * scale;
                let placed_h = p.height_mm * scale;
                let tx = half_w - spine_offset - placed_w;
                let ty = (sheet_h_mm - placed_h) * 0.5;
                let xf = Affine2D::translate(tx, ty).compose(&Affine2D::scale_uniform(scale));
                clone_page_contents_into(doc, src_idx, &mut out, dst_page_idx, xf, None)?;
            }
            if let Some(rel_idx) = *right_opt {
                let src_idx = sig_start + rel_idx;
                let p = &doc.pages()[src_idx];
                let scale = ((half_w - spine_offset).max(1.0) / p.width_mm)
                    .min(sheet_h_mm / p.height_mm)
                    .min(1.0);
                let placed_h = p.height_mm * scale;
                let tx = half_w + spine_offset;
                let ty = (sheet_h_mm - placed_h) * 0.5;
                let xf = Affine2D::translate(tx, ty).compose(&Affine2D::scale_uniform(scale));
                clone_page_contents_into(doc, src_idx, &mut out, dst_page_idx, xf, None)?;
            }

            // Emit spine fold & milling guide marks on Print Marks layer
            let marks_layer =
                find_or_create_layer(&mut out, dst_page_idx, PRINT_MARKS_LAYER_NAME, false)?;
            let fold_line =
                BezierPath::from_line(Point2D::new(half_w, 0.0), Point2D::new(half_w, 5.0));
            out.add_object(
                SceneObject::new(ObjectId(0), "Spine Fold Top", NodePayload::Path(fold_line))
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::solid(Color::cmyk(1.0, 1.0, 1.0, 1.0), 0.15)),
                Some(marks_layer),
            )?;
        }
    }
    Ok(out)
}

// =============================================================================
// Prepress Trapping (Spread / Choke) & Halftone Screening
// =============================================================================

/// Trap placement strategy for adjacent contrasting ink boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TrapPlacement {
    #[default]
    Spread,
    Choke,
    Centerline,
}

/// Configuration for automatic prepress spread/choke color trapping.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrappingSettings {
    /// Trap overlap width in millimeters (default: `0.15` mm).
    pub trap_width_mm: f64,
    /// Minimum neutral density difference required to generate a trap (default: `0.10`).
    pub step_limit: f64,
    /// Density threshold above which dark/black regions act as chokes (default: `0.85`).
    pub black_color_limit: f64,
    pub placement: TrapPlacement,
    pub ignore_bitmaps: bool,
}

impl Default for TrappingSettings {
    fn default() -> Self {
        Self {
            trap_width_mm: 0.15,
            step_limit: 0.10,
            black_color_limit: 0.85,
            placement: TrapPlacement::Spread,
            ignore_bitmaps: true,
        }
    }
}

/// Generated trap geometry between two overlapping/abutting objects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrapZone {
    pub source_id: ObjectId,
    pub target_id: ObjectId,
    pub spread_object_id: ObjectId,
    pub trap_path: BezierPath,
    pub trap_color: Color,
    pub trap_width_mm: f64,
    pub is_choke: bool,
}

/// Summary of computed or applied prepress traps for a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrappingResult {
    pub traps: Vec<TrapZone>,
    pub warnings: Vec<String>,
}

fn color_neutral_density(c: &Color) -> f64 {
    let [cy, mg, yl, bk] = c.to_cmyk();
    // Industry neutral density approximation (K heaviest, Y lightest)
    (0.61 * f64::from(cy) + 0.76 * f64::from(mg) + 0.16 * f64::from(yl) + 1.70 * f64::from(bk))
        .clamp(0.0, 2.5)
}

fn trappable_fill_color(obj: &SceneObject) -> Option<Color> {
    let col = match &obj.fill {
        Fill::Solid(c) if c.alpha() > 1e-4 => Some(c.clone()),
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => {
            (!stops.is_empty()).then(|| Fill::sample_stops(stops, 0.5))
        }
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } if fg.alpha() > 1e-4 => {
            Some(fg.clone())
        }
        Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
        _ => None,
    };
    if col.is_some() {
        return col;
    }
    match &obj.payload {
        NodePayload::ArtisticText(t) => t
            .runs
            .first()
            .filter(|r| r.style.fill.alpha() > 1e-4)
            .map(|r| r.style.fill.clone()),
        NodePayload::ParagraphText(t) => t
            .runs
            .first()
            .filter(|r| r.style.fill.alpha() > 1e-4)
            .map(|r| r.style.fill.clone()),
        NodePayload::TextOnPath(top) => top
            .text
            .runs
            .first()
            .filter(|r| r.style.fill.alpha() > 1e-4)
            .map(|r| r.style.fill.clone()),
        _ => None,
    }
}

struct TrapLeafEntry {
    leaf_uid: usize,
    report_id: ObjectId,
    path: BezierPath,
    bb: Rect2D,
    color: Color,
    nd: f64,
}

#[allow(clippy::too_many_arguments)]
fn collect_trapping_entries_by_id(
    doc: &Document,
    id: ObjectId,
    fallback_owner_id: ObjectId,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    settings: &TrappingSettings,
    text_engine: &mut TextEngine,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    next_uid: &mut usize,
    entries: &mut Vec<TrapLeafEntry>,
) {
    if depth > MAX_SCENE_DEPTH || (id.0 != 0 && !visiting.insert(id)) {
        return;
    }
    let obj_opt = sym_ctx
        .and_then(|c| c.sym_map.get(&id).copied())
        .or_else(|| doc.object(id));
    if let Some(obj) = obj_opt {
        collect_trapping_entries_obj(
            doc,
            obj,
            fallback_owner_id,
            parent_xf,
            parent_persp,
            sym_ctx,
            settings,
            text_engine,
            depth,
            visiting,
            visiting_syms,
            next_uid,
            entries,
        );
    }
    if id.0 != 0 {
        visiting.remove(&id);
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_trapping_entries_obj(
    doc: &Document,
    obj: &SceneObject,
    fallback_owner_id: ObjectId,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    settings: &TrappingSettings,
    text_engine: &mut TextEngine,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    next_uid: &mut usize,
    entries: &mut Vec<TrapLeafEntry>,
) {
    if depth > MAX_SCENE_DEPTH
        || !obj.visible
        || !obj.printable
        || !obj.opacity.is_finite()
        || obj.opacity <= 1e-5
    {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return;
    }
    let report_id = if obj.id.0 != 0 {
        obj.id
    } else {
        fallback_owner_id
    };

    match &obj.payload {
        NodePayload::Clone { .. } => {
            if let Some((p, f, o, in_sym)) =
                doc.resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|c| c.sym_map))
            {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                collect_trapping_entries_obj(
                    doc,
                    &tmp,
                    report_id,
                    parent_xf,
                    parent_persp,
                    if in_sym { sym_ctx } else { None },
                    settings,
                    text_engine,
                    depth + 1,
                    visiting,
                    visiting_syms,
                    next_uid,
                    entries,
                );
            }
            return;
        }
        NodePayload::Group(children) => {
            let own = container_own_persp(doc, obj, sym_ctx);
            let child_persp =
                compose_prod_parent_persp(parent_persp, &parent_xf, &obj.transform, own);
            for &cid in children {
                collect_trapping_entries_by_id(
                    doc,
                    cid,
                    report_id,
                    world_xf,
                    child_persp,
                    sym_ctx,
                    settings,
                    text_engine,
                    depth + 1,
                    visiting,
                    visiting_syms,
                    next_uid,
                    entries,
                );
            }
            return;
        }
        NodePayload::PowerClip {
            container,
            contents,
            ..
        } => {
            let local_c = apply_obj_perspective_to_local_path((**container).clone(), obj);
            let world_container =
                warp_local_to_world(&local_c, &world_xf, &obj.transform, parent_persp);
            if let Some(col) = trappable_fill_color(obj) {
                if let Some(bb) = world_container.bounding_box() {
                    let uid = *next_uid;
                    *next_uid += 1;
                    let nd = color_neutral_density(&col);
                    entries.push(TrapLeafEntry {
                        leaf_uid: uid,
                        report_id,
                        path: world_container.clone(),
                        bb,
                        color: col,
                        nd,
                    });
                }
            }
            let own = container_own_persp(doc, obj, sym_ctx);
            let child_persp =
                compose_prod_parent_persp(parent_persp, &parent_xf, &obj.transform, own);
            let start_len = entries.len();
            for &cid in contents {
                collect_trapping_entries_by_id(
                    doc,
                    cid,
                    report_id,
                    world_xf,
                    child_persp,
                    sym_ctx,
                    settings,
                    text_engine,
                    depth + 1,
                    visiting,
                    visiting_syms,
                    next_uid,
                    entries,
                );
            }
            if !world_container.is_empty() {
                let mut clipped = Vec::with_capacity(entries.len() - start_len);
                for mut e in entries.drain(start_len..) {
                    if let Ok(parts) =
                        apply_boolean(BooleanOp::Intersect, &e.path, &world_container)
                    {
                        let cp = BezierPath {
                            subpaths: parts.into_iter().flat_map(|p| p.subpaths).collect(),
                        };
                        if let Some(bb) = cp.bounding_box() {
                            if !cp.is_empty() {
                                e.path = cp;
                                e.bb = bb;
                                clipped.push(e);
                            }
                        }
                    }
                }
                entries.extend(clipped);
            }
            return;
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visiting_syms.insert(*sym_id) {
                return;
            }
            if let Some(sym) = doc.symbol(*sym_id) {
                let mut local_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for (idx, sub) in sym.objects.iter().enumerate() {
                    if sub.id.0 != 0 {
                        local_map.insert(sub.id, sub);
                    }
                    let pref = if sub.id.0 != 0 {
                        SymParentRef::Id(sub.id)
                    } else {
                        SymParentRef::RootIndex(idx)
                    };
                    match &sub.payload {
                        NodePayload::Group(c) => {
                            for &cid in c {
                                referenced.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(pref);
                                }
                            }
                        }
                        NodePayload::PowerClip { contents, .. } => {
                            for &cid in contents {
                                referenced.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(pref);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let s_ctx = SymBoundsCtx {
                    root_xf: &world_xf,
                    sym_objects: &sym.objects,
                    sym_map: &local_map,
                    sym_parent_map: &sym_parent_map,
                };
                let own = container_own_persp(doc, obj, sym_ctx);
                let child_persp =
                    compose_prod_parent_persp(parent_persp, &parent_xf, &obj.transform, own);
                let mut sym_vis = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        continue;
                    }
                    if sub.id.0 != 0 && !sym_vis.insert(sub.id) {
                        continue;
                    }
                    collect_trapping_entries_obj(
                        doc,
                        sub,
                        report_id,
                        world_xf,
                        child_persp,
                        Some(&s_ctx),
                        settings,
                        text_engine,
                        depth + 1,
                        &mut sym_vis,
                        visiting_syms,
                        next_uid,
                        entries,
                    );
                    if sub.id.0 != 0 {
                        sym_vis.remove(&sub.id);
                    }
                }
            }
            visiting_syms.remove(sym_id);
            return;
        }
        NodePayload::Table(table) => {
            for r in 0..table.rows {
                for c in 0..table.cols {
                    let Some(cell) = table.cell(r, c) else {
                        continue;
                    };
                    if cell.is_covered() {
                        continue;
                    }
                    let cell_col = match &cell.fill {
                        Fill::Solid(cl) if cl.alpha() > 1e-4 => Some(cl.clone()),
                        Fill::LinearGradient { stops, .. } | Fill::RadialGradient { stops, .. }
                            if !stops.is_empty() =>
                        {
                            Some(Fill::sample_stops(stops, 0.5))
                        }
                        _ => None,
                    };
                    if let (Some(col), Some(cr)) = (cell_col, table.cell_rect(r, c)) {
                        let local =
                            apply_obj_perspective_to_local_path(BezierPath::from_rect(cr), obj);
                        let wp =
                            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                        if let Some(bb) = wp.bounding_box() {
                            let uid = *next_uid;
                            *next_uid += 1;
                            let nd = color_neutral_density(&col);
                            entries.push(TrapLeafEntry {
                                leaf_uid: uid,
                                report_id,
                                path: wp,
                                bb,
                                color: col,
                                nd,
                            });
                        }
                    }
                }
            }
            return;
        }
        _ => {}
    }

    if settings.ignore_bitmaps && matches!(obj.payload, NodePayload::Raster(_)) {
        return;
    }
    let Some(col) = trappable_fill_color(obj) else {
        return;
    };
    let uid = *next_uid;
    *next_uid += 1;
    let nd = color_neutral_density(&col);
    let mut paths: Vec<BezierPath> = Vec::new();
    match &obj.payload {
        NodePayload::Path(p) => {
            if !p.is_empty() {
                let local = apply_obj_perspective_to_local_path(p.clone(), obj);
                paths.push(warp_local_to_world(
                    &local,
                    &world_xf,
                    &obj.transform,
                    parent_persp,
                ));
            }
        }
        NodePayload::Shape(s) => {
            let local = apply_obj_perspective_to_local_path(s.to_bezier_path(), obj);
            paths.push(warp_local_to_world(
                &local,
                &world_xf,
                &obj.transform,
                parent_persp,
            ));
        }
        NodePayload::BSpline(bs) => {
            let local = apply_obj_perspective_to_local_path(bs.to_bezier_path(), obj);
            paths.push(warp_local_to_world(
                &local,
                &world_xf,
                &obj.transform,
                parent_persp,
            ));
        }
        NodePayload::ArtisticMedia { spine, style } => {
            let expanded = coree_core::ArtisticMedia::expand_stroke(spine, style);
            let local = apply_obj_perspective_to_local_path(expanded, obj);
            paths.push(warp_local_to_world(
                &local,
                &world_xf,
                &obj.transform,
                parent_persp,
            ));
        }
        NodePayload::ArtisticText(t) => {
            let pieces = text_engine.layout_artistic_pieces(t);
            let combined = BezierPath {
                subpaths: pieces.into_iter().flat_map(|g| g.path.subpaths).collect(),
            };
            if !combined.is_empty() {
                let local = apply_obj_perspective_to_local_path(combined, obj);
                paths.push(warp_local_to_world(
                    &local,
                    &world_xf,
                    &obj.transform,
                    parent_persp,
                ));
            }
        }
        NodePayload::ParagraphText(t) => {
            let pieces = text_engine.layout_paragraph_in_doc(doc, obj.id, t);
            let combined = BezierPath {
                subpaths: pieces.into_iter().flat_map(|g| g.path.subpaths).collect(),
            };
            if !combined.is_empty() {
                let local = apply_obj_perspective_to_local_path(combined, obj);
                paths.push(warp_local_to_world(
                    &local,
                    &world_xf,
                    &obj.transform,
                    parent_persp,
                ));
            }
        }
        NodePayload::Raster(r) => {
            let rect = match r.crop_rect {
                Some(cr) if cr.is_finite() => Some(cr),
                Some(_) => None,
                None if r.width_mm.is_finite()
                    && r.width_mm > 0.0
                    && r.height_mm.is_finite()
                    && r.height_mm > 0.0 =>
                {
                    Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                }
                None => None,
            };
            if let Some(rc) = rect {
                let local = apply_obj_perspective_to_local_path(BezierPath::from_rect(rc), obj);
                paths.push(warp_local_to_world(
                    &local,
                    &world_xf,
                    &obj.transform,
                    parent_persp,
                ));
            }
        }
        _ => {
            let mut visit = HashSet::new();
            crate::scene_helpers::collect_silhouette_paths(
                doc,
                obj.id,
                parent_xf,
                parent_persp,
                depth,
                &mut visit,
                &mut paths,
            );
        }
    }
    for p in paths {
        if let Some(bb) = p.bounding_box() {
            if !p.is_empty() {
                entries.push(TrapLeafEntry {
                    leaf_uid: uid,
                    report_id,
                    path: p,
                    bb,
                    color: col.clone(),
                    nd,
                });
            }
        }
    }
}

/// Computes spread/choke trap zones between abutting or overlapping contrasting fills on `page_index`.
pub fn compute_trapping(
    doc: &Document,
    page_index: usize,
    settings: &TrappingSettings,
) -> ProdResult<TrappingResult> {
    let _page = get_page(doc, page_index)?;
    validate_positive("trap_width_mm", settings.trap_width_mm)?;
    let trap_w = settings.trap_width_mm.clamp(0.01, 5.0);

    let mut entries: Vec<TrapLeafEntry> = Vec::new();
    let mut text_engine = TextEngine::new();
    let mut next_uid = 1usize;
    let layers = collect_effective_layers(doc, page_index);
    for layer in &layers {
        if !layer.visible || !layer.printable || layer.is_cut_layer {
            continue;
        }
        let mut visit = HashSet::new();
        let mut visit_syms = HashSet::new();
        for &oid in &layer.objects {
            collect_trapping_entries_by_id(
                doc,
                oid,
                oid,
                Affine2D::IDENTITY,
                None,
                None,
                settings,
                &mut text_engine,
                0,
                &mut visit,
                &mut visit_syms,
                &mut next_uid,
                &mut entries,
            );
        }
    }

    let mut traps = Vec::new();
    let mut warnings = Vec::new();
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            let a = &entries[i];
            let b = &entries[j];
            if a.leaf_uid == b.leaf_uid {
                continue;
            }
            if (a.nd - b.nd).abs() < settings.step_limit {
                continue;
            }
            if a.bb.expand(trap_w * 2.0).intersection(b.bb).is_none() {
                continue;
            }
            let (light, dark) = if a.nd < b.nd { (a, b) } else { (b, a) };

            match settings.placement {
                TrapPlacement::Choke => {
                    let dark_rim = match coree_core::offset_polygon(&dark.path, -trap_w) {
                        Ok(shrunk) if !shrunk.is_empty() => {
                            match apply_boolean(BooleanOp::Trim, &dark.path, &shrunk) {
                                Ok(parts) => BezierPath {
                                    subpaths: parts.into_iter().flat_map(|p| p.subpaths).collect(),
                                },
                                Err(_) => continue,
                            }
                        }
                        _ => dark.path.clone(),
                    };
                    if dark_rim.is_empty() {
                        continue;
                    }
                    let overlap_trap = apply_boolean(BooleanOp::Intersect, &dark_rim, &light.path)
                        .ok()
                        .map(|parts| BezierPath {
                            subpaths: parts.into_iter().flat_map(|p| p.subpaths).collect(),
                        })
                        .filter(|p| !p.is_empty());
                    let trap_only = if let Some(ov) = overlap_trap {
                        Some(ov)
                    } else if let Ok(expanded_light) =
                        coree_core::offset_polygon(&light.path, trap_w)
                    {
                        apply_boolean(BooleanOp::Intersect, &dark_rim, &expanded_light)
                            .ok()
                            .map(|parts| BezierPath {
                                subpaths: parts.into_iter().flat_map(|p| p.subpaths).collect(),
                            })
                            .filter(|p| !p.is_empty())
                    } else {
                        None
                    };
                    if let Some(trap_path) = trap_only {
                        traps.push(TrapZone {
                            source_id: light.report_id,
                            target_id: dark.report_id,
                            spread_object_id: dark.report_id,
                            trap_path,
                            trap_color: light.color.clone(),
                            trap_width_mm: trap_w,
                            is_choke: true,
                        });
                    }
                }
                TrapPlacement::Centerline => {
                    let half = (trap_w * 0.5).max(0.005);
                    let (Ok(exp_l), Ok(exp_d)) = (
                        coree_core::offset_polygon(&light.path, half),
                        coree_core::offset_polygon(&dark.path, half),
                    ) else {
                        continue;
                    };
                    if let Ok(parts) = apply_boolean(BooleanOp::Intersect, &exp_l, &exp_d) {
                        let trap_only = BezierPath {
                            subpaths: parts.into_iter().flat_map(|p| p.subpaths).collect(),
                        };
                        if !trap_only.is_empty() {
                            traps.push(TrapZone {
                                source_id: light.report_id,
                                target_id: dark.report_id,
                                spread_object_id: light.report_id,
                                trap_path: trap_only,
                                trap_color: light.color.clone(),
                                trap_width_mm: trap_w,
                                is_choke: false,
                            });
                        }
                    }
                }
                TrapPlacement::Spread => {
                    let is_choke = dark.nd >= settings.black_color_limit;
                    let expanded = match coree_core::offset_polygon(&light.path, trap_w) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    if let Ok(band_parts) =
                        apply_boolean(BooleanOp::Intersect, &expanded, &dark.path)
                    {
                        let band = BezierPath {
                            subpaths: band_parts.into_iter().flat_map(|p| p.subpaths).collect(),
                        };
                        if !band.is_empty() {
                            if let Ok(trap_parts) =
                                apply_boolean(BooleanOp::Trim, &band, &light.path)
                            {
                                let trap_only = BezierPath {
                                    subpaths: trap_parts
                                        .into_iter()
                                        .flat_map(|p| p.subpaths)
                                        .collect(),
                                };
                                if !trap_only.is_empty() {
                                    traps.push(TrapZone {
                                        source_id: light.report_id,
                                        target_id: dark.report_id,
                                        spread_object_id: light.report_id,
                                        trap_path: trap_only,
                                        trap_color: light.color.clone(),
                                        trap_width_mm: trap_w,
                                        is_choke,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if traps.is_empty() && entries.len() >= 2 {
        warnings.push("No contrasting abutting ink pairs exceeded step_limit".to_owned());
    }
    Ok(TrappingResult { traps, warnings })
}

/// Computes and inserts overprint trap objects onto a dedicated `Prepress Traps` layer.
pub fn apply_trapping(
    doc: &mut Document,
    page_index: usize,
    settings: &TrappingSettings,
) -> ProdResult<TrappingResult> {
    let result = compute_trapping(doc, page_index, settings)?;
    if !result.traps.is_empty() {
        let layer_id = find_or_create_layer(doc, page_index, "Prepress Traps", false)?;
        for (idx, z) in result.traps.iter().enumerate() {
            let mut obj = SceneObject::new(
                ObjectId(0),
                format!("Trap_{}_{}", idx + 1, z.spread_object_id.0),
                NodePayload::Path(z.trap_path.clone()),
            )
            .with_fill(Fill::Solid(z.trap_color.clone()))
            .with_outline(OutlineStyle::none());
            obj.blend_mode = BlendMode::Multiply;
            doc.add_object(obj, Some(layer_id))?;
        }
    }
    Ok(result)
}

/// Halftone dot cell geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum HalftoneDotShape {
    #[default]
    Round,
    Elliptical,
    Line,
    Square,
    Diamond,
}

/// Per-plate AM halftone screen frequency, rosette angle, and dot shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HalftoneScreen {
    pub lpi: f64,
    pub angle_deg: f64,
    pub dot_shape: HalftoneDotShape,
}

/// Full CMYK + Spot rosette halftone screen configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HalftoneSettings {
    pub dpi: f64,
    pub cyan: HalftoneScreen,
    pub magenta: HalftoneScreen,
    pub yellow: HalftoneScreen,
    pub black: HalftoneScreen,
    pub spot_base_angle_deg: f64,
}

impl Default for HalftoneSettings {
    fn default() -> Self {
        Self {
            dpi: 300.0,
            cyan: HalftoneScreen {
                lpi: 133.0,
                angle_deg: 15.0,
                dot_shape: HalftoneDotShape::Round,
            },
            magenta: HalftoneScreen {
                lpi: 133.0,
                angle_deg: 75.0,
                dot_shape: HalftoneDotShape::Round,
            },
            yellow: HalftoneScreen {
                lpi: 133.0,
                angle_deg: 0.0,
                dot_shape: HalftoneDotShape::Round,
            },
            black: HalftoneScreen {
                lpi: 133.0,
                angle_deg: 45.0,
                dot_shape: HalftoneDotShape::Round,
            },
            spot_base_angle_deg: 22.5,
        }
    }
}

/// Screened binary/1-bit-density halftone preview plate (`0` = paper, `255` = ink dot).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HalftonePlate {
    pub channel: SeparationChannel,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub screen: HalftoneScreen,
    pub screened_bitmap: Vec<u8>,
}

/// Screens a continuous-tone [`SeparationPlate`] into an AM halftone dot bitmap using
/// rotated spot-function threshold matrices (`Round`, `Elliptical`, `Line`, `Square`, `Diamond`).
pub fn apply_halftone_screen(
    plate: &SeparationPlate,
    dpi: f64,
    screen: &HalftoneScreen,
) -> ProdResult<HalftonePlate> {
    validate_positive("dpi", dpi)?;
    validate_positive("screen.lpi", screen.lpi)?;
    let w = plate.width as usize;
    let h = plate.height as usize;
    if plate.ink_map.len() != w.saturating_mul(h) {
        return Err(ProdError::InvalidParameter(
            "separation plate ink_map dimensions mismatch".into(),
        ));
    }
    let rad = screen.angle_deg.to_radians();
    let (sin_a, cos_a) = rad.sin_cos();
    let cells_per_px = (screen.lpi / dpi).clamp(0.01, 0.5);
    let mut screened = vec![0u8; w * h];

    for y in 0..h {
        let fy = (y as f64) * cells_per_px;
        for x in 0..w {
            let fx = (x as f64) * cells_per_px;
            let rx = fx * cos_a - fy * sin_a;
            let ry = fx * sin_a + fy * cos_a;
            // Normalized [-1, 1] cell coordinates
            let u = rx.rem_euclid(1.0) * 2.0 - 1.0;
            let v = ry.rem_euclid(1.0) * 2.0 - 1.0;
            let spot_val = match screen.dot_shape {
                HalftoneDotShape::Round => 1.0 - 0.5 * (u * u + v * v),
                HalftoneDotShape::Elliptical => 1.0 - (0.35 * u * u + 0.65 * v * v),
                HalftoneDotShape::Line => 1.0 - v.abs(),
                HalftoneDotShape::Square => 1.0 - u.abs().max(v.abs()),
                HalftoneDotShape::Diamond => 1.0 - 0.5 * (u.abs() + v.abs()),
            }
            .clamp(0.0, 1.0);
            let ink = f64::from(plate.ink_map[y * w + x]) / 255.0;
            screened[y * w + x] = if ink > 0.0 && ink >= (1.0 - spot_val) {
                255
            } else {
                0
            };
        }
    }

    Ok(HalftonePlate {
        channel: plate.channel.clone(),
        name: plate.name.clone(),
        width: plate.width,
        height: plate.height,
        screen: *screen,
        screened_bitmap: screened,
    })
}

/// Applies rosette halftone screening across every CMYK and Spot plate in `sep`.
pub fn generate_halftone_preview(
    sep: &SeparationResult,
    settings: &HalftoneSettings,
) -> ProdResult<Vec<HalftonePlate>> {
    let mut out = Vec::with_capacity(sep.plates.len());
    let mut spot_idx = 0usize;
    for plate in &sep.plates {
        let screen = match &plate.channel {
            SeparationChannel::Cyan => settings.cyan,
            SeparationChannel::Magenta => settings.magenta,
            SeparationChannel::Yellow => settings.yellow,
            SeparationChannel::Black => settings.black,
            SeparationChannel::Spot(_) => {
                let angle = (settings.spot_base_angle_deg + (spot_idx as f64) * 30.0) % 180.0;
                spot_idx += 1;
                HalftoneScreen {
                    lpi: settings.black.lpi,
                    angle_deg: angle,
                    dot_shape: settings.black.dot_shape,
                }
            }
        };
        out.push(apply_halftone_screen(plate, settings.dpi, &screen)?);
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
    let total_pages = doc.pages().len();
    let doc_title = doc.title().to_owned();
    let color_mode = doc.color_mode();
    let dpi = doc.dpi();

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
                doc.pages()[0].id,
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
        assert_eq!(nup.pages().len(), 1);
        assert_eq!(nup.object_count(), 10);

        // 2. Booklet Saddle-Stitch on 6-page A5 document -> 4 imposed A4 spreads
        let mut booklet_src = Document::new("Zine", 148.5, 210.0, ColorMode::Cmyk, 300.0);
        for p in 0..6 {
            if p > 0 {
                booklet_src.add_page(format!("Page {}", p + 1), 148.5, 210.0, 0.0);
            }
            let lid = booklet_src.pages()[p].layers[0].id;
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
        assert_eq!(booklet.pages().len(), 4);
        assert!(booklet.pages()[0].name.contains("Blank | P1"));
        assert!(booklet.pages()[1].name.contains("P2 | Blank"));
        assert!(booklet.pages()[2].name.contains("P6 | P3"));
        assert!(booklet.pages()[3].name.contains("P4 | P5"));
        assert_eq!(booklet.object_count(), 6);

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
        assert_eq!(tiled.pages().len(), 6);
        assert!(tiled.pages()[0]
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

    #[test]
    fn text_outliner_renders_glyph_contours_and_symbol_spot_text() {
        let mut doc = Document::new("GlyphSep", 100.0, 100.0, ColorMode::Cmyk, 150.0);

        // Live ArtisticText positioned at (20, 20)
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Letter O",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "O",
                    TextStyle {
                        font_size_pt: 72.0,
                        fill: Color::KEY_BLACK,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(20.0, 20.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

        // Symbol containing Spot-ink ArtisticText
        let sym_text = SceneObject::new(
            ObjectId(1),
            "SymText",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "I",
                TextStyle {
                    font_size_pt: 36.0,
                    fill: Color::spot("SpotGold", 1.0, [0.0, 0.2, 0.9, 0.0]),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none());
        let sym = coree_core::SymbolDef::new("Badge", vec![sym_text]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "BadgeInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(Affine2D::translate(60.0, 20.0)),
            None,
        )
        .unwrap();

        let opts = SeparationOptions {
            dpi: 25.4,
            max_dimension_px: 256,
            include_cut_layers: true,
        };
        let sep = compute_separations_with_text_outliner(&doc, 0, &opts, |obj, _| {
            if matches!(obj.payload, NodePayload::ArtisticText(_)) {
                // Hollow ring glyph in local coords (0..20 x 0..20) with inner hole (6..14 x 6..14)
                let outer = coree_core::Subpath::from_points(
                    [
                        Point2D::new(0.0, 0.0),
                        Point2D::new(20.0, 0.0),
                        Point2D::new(20.0, 20.0),
                        Point2D::new(0.0, 20.0),
                    ],
                    true,
                );
                let inner = coree_core::Subpath::from_points(
                    [
                        Point2D::new(6.0, 6.0),
                        Point2D::new(6.0, 14.0),
                        Point2D::new(14.0, 14.0),
                        Point2D::new(14.0, 6.0),
                    ],
                    true,
                );
                Some(BezierPath {
                    subpaths: vec![outer, inner],
                })
            } else {
                None
            }
        })
        .unwrap();

        let k_plate = sep.plate(&SeparationChannel::Black).unwrap();
        // Stroke of ring at (22, 22) has black ink
        assert_eq!(k_plate.ink_map[22 * 100 + 22], 255);
        // Inner counter hole at (30, 30) must be clear (0 ink, NOT filled bounding box!)
        assert_eq!(k_plate.ink_map[30 * 100 + 30], 0);
        // Spot ink inside symbol text was discovered and rasterized!
        let spot_plate = sep.spot_plate("SpotGold").unwrap();
        assert_eq!(spot_plate.ink_map[22 * 100 + 62], 255);
        assert_eq!(spot_plate.ink_map[30 * 100 + 70], 0);
    }

    #[test]
    fn vector_separations_and_separated_pdf_export_cover_cmyk_and_spots() {
        let mut doc = Document::new("SepVec", 100.0, 100.0, coree_core::ColorMode::Cmyk, 300.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "CyanBox",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 40.0,
                    height: 40.0,
                    corner_radii: [0.0; 4],
                    corner_style: coree_core::CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::CYAN))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "SpotSeal",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 15.0,
                    ry: 15.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_fill(Fill::Solid(Color::spot(
                "Pantone485",
                1.0,
                [0.0, 0.95, 1.0, 0.0],
            )))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(70.0, 50.0)),
            None,
        )
        .unwrap();

        let plates = compute_vector_separations(&doc, 0, true).unwrap();
        assert_eq!(plates.len(), 5);
        assert!(!plates[0].vector_ops.is_empty());
        let spot = plates
            .iter()
            .find(|p| p.channel == SeparationChannel::Spot("Pantone485".to_owned()))
            .unwrap();
        assert!(!spot.vector_ops.is_empty());

        let (pdf_bytes, names) =
            export_separated_plates_pdf_bytes(&doc, 0, &SeparationOptions::default()).unwrap();
        assert_eq!(names.len(), 5);
        assert!(pdf_bytes.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&pdf_bytes);
        assert!(text.contains("Pantone485"));
    }

    #[test]
    fn perfect_bound_imposition_creates_signatures_and_spine_creep() {
        let mut doc = Document::new("Book8", 148.0, 210.0, coree_core::ColorMode::Cmyk, 300.0);
        for i in 1..8 {
            doc.add_page(format!("P{}", i + 1), 148.0, 210.0, 0.0);
        }
        for idx in 0..doc.pages().len() {
            let lid = doc.pages()[idx].layers[0].id;
            doc.add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("PageBox{}", idx + 1),
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 40.0,
                        height: 40.0,
                        corner_radii: [0.0; 4],
                        corner_style: coree_core::CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::Solid(Color::CYAN))
                .with_transform(Affine2D::translate(20.0, 20.0)),
                Some(lid),
            )
            .unwrap();
        }
        let imposed = impose_document(
            &doc,
            &ImpositionMode::PerfectBound {
                sheet_w_mm: 320.0,
                sheet_h_mm: 240.0,
                signature_pages: 4,
                spine_thickness_mm: 6.0,
                creep_mm: 1.0,
                milling_allowance_mm: 2.0,
            },
        )
        .unwrap();
        // 8 pages in 4-page signatures => 2 signatures * 2 sides = 4 press sheets
        assert_eq!(imposed.pages().len(), 4);
        assert!(imposed.pages()[0].name.contains("Sig 1"));
        assert!(imposed.pages()[2].name.contains("Sig 2"));
    }

    #[test]
    fn trapping_spreads_light_yellow_into_dark_black_and_halftone_screens() {
        let mut doc = Document::new("TrapHalf", 100.0, 100.0, coree_core::ColorMode::Cmyk, 300.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Yellow",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 40.0,
                    height: 40.0,
                    corner_radii: [0.0; 4],
                    corner_style: coree_core::CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::cmyk(0.0, 0.0, 1.0, 0.0)))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Black",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 40.0,
                    height: 40.0,
                    corner_radii: [0.0; 4],
                    corner_style: coree_core::CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::cmyk(0.0, 0.0, 0.0, 1.0)))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(40.0, 0.0)),
            None,
        )
        .unwrap();

        let res = apply_trapping(&mut doc, 0, &TrappingSettings::default()).unwrap();
        assert!(
            !res.traps.is_empty(),
            "Expected trap between abutting yellow and black"
        );

        let sep = compute_separations(
            &doc,
            0,
            &SeparationOptions {
                dpi: 50.0,
                max_dimension_px: 256,
                include_cut_layers: true,
            },
        )
        .unwrap();
        let screened = generate_halftone_preview(&sep, &HalftoneSettings::default()).unwrap();
        assert_eq!(screened.len(), 4);
        assert!(screened[2].screened_bitmap.contains(&255));
    }

    #[test]
    fn vector_separations_outline_live_text_honor_clips_rasters_and_overprint_knockout() {
        let mut doc = Document::new(
            "VecSepFull",
            100.0,
            100.0,
            coree_core::ColorMode::Cmyk,
            300.0,
        );

        // 1. Live ArtisticText "O" (with counter hole) must emit real glyph curves, NOT an AABB rectangle
        let mut txt = ArtisticText::from_plain(
            "O",
            TextStyle {
                font_size_pt: 48.0,
                fill: Color::KEY_BLACK,
                ..TextStyle::default()
            },
        );
        if let Some(r) = txt.runs.first_mut() {
            r.style.font_family = "DejaVu Sans".into();
        }
        let est = txt.estimated_bounds();
        doc.add_object(
            SceneObject::new(ObjectId(0), "LiveO", NodePayload::ArtisticText(txt))
                .with_transform(Affine2D::translate(10.0, 20.0))
                .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

        // 2. Knockout vs Overprint: Cyan base + non-overprinting Magenta on top + overprinting Yellow on top
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "CyanBase",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 40.0,
                    height: 40.0,
                    corner_radii: [0.0; 4],
                    corner_style: coree_core::CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::CYAN))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(50.0, 10.0)),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "MagKnockout",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 20.0,
                    corner_radii: [0.0; 4],
                    corner_style: coree_core::CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::MAGENTA))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(55.0, 15.0)),
            None,
        )
        .unwrap();
        let mut yel_op = SceneObject::new(
            ObjectId(0),
            "YelOverprint",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 10.0,
                height: 10.0,
                corner_radii: [0.0; 4],
                corner_style: coree_core::CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::YELLOW))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(60.0, 20.0));
        yel_op.blend_mode = BlendMode::Multiply;
        doc.add_object(yel_op, None).unwrap();

        // 3. PowerClip clipping a wide child
        let child_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "WideChild",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 50.0,
                        height: 20.0,
                        corner_radii: [0.0; 4],
                        corner_style: coree_core::CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::Solid(Color::CYAN))
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();
        let clip_rect = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 15.0, 15.0));
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "ClipContainer",
                NodePayload::PowerClip {
                    container: Box::new(clip_rect),
                    container_parametric: None,
                    contents: vec![child_id],
                    lock_contents: true,
                },
            )
            .with_transform(Affine2D::translate(10.0, 60.0)),
            None,
        )
        .unwrap();

        // 4. MeshFill / isolated composite produces RasterPlatePrimitive on separated plates
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "MeshObj",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 20.0,
                    corner_radii: [0.0; 4],
                    corner_style: coree_core::CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::MeshFill {
                rows: 2,
                cols: 2,
                nodes: vec![
                    coree_core::MeshNode::new(0, 0, Point2D::new(0.0, 0.0), Color::CYAN),
                    coree_core::MeshNode::new(0, 1, Point2D::new(20.0, 0.0), Color::MAGENTA),
                    coree_core::MeshNode::new(1, 0, Point2D::new(0.0, 20.0), Color::YELLOW),
                    coree_core::MeshNode::new(1, 1, Point2D::new(20.0, 20.0), Color::KEY_BLACK),
                ],
            })
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(70.0, 60.0)),
            None,
        )
        .unwrap();

        let plates = compute_vector_separations(&doc, 0, true).unwrap();
        let k_plate = plates
            .iter()
            .find(|p| p.channel == SeparationChannel::Black)
            .unwrap();
        let aabb_world = BezierPath::from_rect(est).transformed(&Affine2D::translate(10.0, 20.0));
        assert!(
            !k_plate.vector_ops.is_empty(),
            "Live text must produce glyph outline vector ops"
        );
        assert!(
            k_plate.vector_ops.iter().any(|op| {
                op.path.subpaths.len() >= 2 || op.path.subpaths.iter().any(|s| s.nodes.len() > 4)
            }) && k_plate.vector_ops.iter().all(|op| op.path != aabb_world),
            "Live text must emit real glyph curves, never an AABB rectangle"
        );

        let c_plate = plates
            .iter()
            .find(|p| p.channel == SeparationChannel::Cyan)
            .unwrap();
        assert!(
            c_plate
                .vector_ops
                .iter()
                .any(|op| op.fill_density == Some(0.0) && !op.overprint),
            "Non-overprinting Magenta box must emit explicit knockout on Cyan plate"
        );
        assert!(
            c_plate
                .vector_ops
                .iter()
                .any(|op| !op.clip_paths.is_empty()),
            "PowerClip contents must retain clip_paths"
        );
        assert!(
            !c_plate.raster_ops.is_empty(),
            "MeshFill / isolated raster must be emitted into raster_ops"
        );

        let (pdf_bytes, _) =
            export_separated_plates_pdf_bytes(&doc, 0, &SeparationOptions::default()).unwrap();
        let pdf_txt = String::from_utf8_lossy(&pdf_bytes);
        assert!(pdf_txt.contains("/GSko") && pdf_txt.contains("/GSop"));
        assert!(pdf_txt.contains("/XObject"));
    }

    #[test]
    fn compute_trapping_expands_containers_clones_symbols_and_choke_geometry() {
        let mut doc = Document::new("TrapAll", 200.0, 200.0, coree_core::ColorMode::Cmyk, 300.0);
        let rect = |w: f64, h: f64| {
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: coree_core::CornerStyle::Rounded,
                lock_corners: true,
            })
        };
        let yellow = Color::cmyk(0.0, 0.0, 1.0, 0.0);
        let black = Color::cmyk(0.0, 0.0, 0.0, 1.0);

        // 1. Group containing abutting Yellow & Black
        let g1 = doc
            .add_object(
                SceneObject::new(ObjectId(0), "GY", rect(20.0, 20.0))
                    .with_fill(Fill::Solid(yellow.clone()))
                    .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();
        let g2 = doc
            .add_object(
                SceneObject::new(ObjectId(0), "GB", rect(20.0, 20.0))
                    .with_fill(Fill::Solid(black.clone()))
                    .with_outline(OutlineStyle::none())
                    .with_transform(Affine2D::translate(20.0, 0.0)),
                None,
            )
            .unwrap();
        doc.group_objects(&[g1, g2], "TrapGrp").unwrap();

        // 2. SymbolInstance containing abutting Yellow & Black
        let s1 = SceneObject::new(ObjectId(101), "SY", rect(20.0, 20.0))
            .with_fill(Fill::Solid(yellow.clone()))
            .with_outline(OutlineStyle::none());
        let s2 = SceneObject::new(ObjectId(102), "SB", rect(20.0, 20.0))
            .with_fill(Fill::Solid(black.clone()))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(20.0, 0.0));
        let sym = coree_core::SymbolDef::new("TrapSym", vec![s1, s2]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "TrapSymInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(Affine2D::translate(0.0, 40.0)),
            None,
        )
        .unwrap();

        // 3. Live Clone (`link_fill: true`) abutting a Black rect
        let c_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "TrapClone",
                    NodePayload::Clone {
                        master_id: g1,
                        link_fill: true,
                        link_outline: true,
                        link_shape: true,
                    },
                )
                .with_transform(Affine2D::translate(0.0, 80.0)),
                None,
            )
            .unwrap();
        doc.add_object(
            SceneObject::new(ObjectId(0), "CloneBlack", rect(20.0, 20.0))
                .with_fill(Fill::Solid(black.clone()))
                .with_outline(OutlineStyle::none())
                .with_transform(Affine2D::translate(20.0, 80.0)),
            None,
        )
        .unwrap();

        let res = compute_trapping(&doc, 0, &TrappingSettings::default()).unwrap();
        assert!(
            res.traps
                .iter()
                .any(|t| t.source_id == g1 && t.target_id == g2),
            "Expected trap inside Group"
        );
        assert!(
            res.traps
                .iter()
                .any(|t| t.source_id == ObjectId(101) && t.target_id == ObjectId(102)),
            "Expected trap inside SymbolInstance"
        );
        assert!(
            res.traps.iter().any(|t| t.source_id == c_id),
            "Expected trap for live Clone with link_fill=true"
        );

        // 4. Overlapping Yellow (0..60) + Black (20..50) distinguishes Choke vs Spread!
        let mut overlap_doc = Document::new(
            "ChokeVsSpread",
            100.0,
            100.0,
            coree_core::ColorMode::Cmyk,
            300.0,
        );
        overlap_doc
            .add_object(
                SceneObject::new(ObjectId(0), "BgY", rect(60.0, 60.0))
                    .with_fill(Fill::Solid(yellow))
                    .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();
        let blk_id = overlap_doc
            .add_object(
                SceneObject::new(ObjectId(0), "FgB", rect(30.0, 30.0))
                    .with_fill(Fill::Solid(black))
                    .with_outline(OutlineStyle::none())
                    .with_transform(Affine2D::translate(15.0, 15.0)),
                None,
            )
            .unwrap();
        let spread_res = compute_trapping(
            &overlap_doc,
            0,
            &TrappingSettings {
                placement: TrapPlacement::Spread,
                trap_width_mm: 0.5,
                ..TrappingSettings::default()
            },
        )
        .unwrap();
        let choke_res = compute_trapping(
            &overlap_doc,
            0,
            &TrappingSettings {
                placement: TrapPlacement::Choke,
                trap_width_mm: 0.5,
                ..TrappingSettings::default()
            },
        )
        .unwrap();
        assert!(
            spread_res.traps.is_empty(),
            "Yellow background already covers Black so Spread adds no outer trap"
        );
        assert_eq!(choke_res.traps.len(), 1);
        assert!(choke_res.traps[0].is_choke);
        assert_eq!(choke_res.traps[0].spread_object_id, blk_id);
    }

    #[test]
    fn compute_separations_preserves_text_effects_and_symbol_meshfill() {
        let mut doc = Document::new("SepFxSym", 100.0, 100.0, coree_core::ColorMode::Cmyk, 300.0);
        // Live text with DropShadow (offset +10, +10) even with text_outliner returning Some(...)
        let mut txt = SceneObject::new(
            ObjectId(0),
            "ShadowText",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "A",
                TextStyle {
                    font_size_pt: 24.0,
                    fill: Color::CYAN,
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(10.0, 10.0))
        .with_outline(OutlineStyle::none());
        txt.effects.push(Effect::DropShadow {
            dx_mm: 12.0,
            dy_mm: 12.0,
            blur_mm: 1.0,
            opacity: 1.0,
            color: Color::MAGENTA,
            feather_direction: coree_core::FeatherDirection::Outside,
        });
        doc.add_object(txt, None).unwrap();

        // SymbolInstance containing a MeshFill child
        let sym_mesh = SceneObject::new(
            ObjectId(1),
            "SymMesh",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: coree_core::CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::MeshFill {
            rows: 2,
            cols: 2,
            nodes: vec![
                coree_core::MeshNode::new(0, 0, Point2D::new(0.0, 0.0), Color::YELLOW),
                coree_core::MeshNode::new(0, 1, Point2D::new(20.0, 0.0), Color::YELLOW),
                coree_core::MeshNode::new(1, 0, Point2D::new(0.0, 20.0), Color::YELLOW),
                coree_core::MeshNode::new(1, 1, Point2D::new(20.0, 20.0), Color::YELLOW),
            ],
        })
        .with_outline(OutlineStyle::none());
        let sym = coree_core::SymbolDef::new("MeshSym", vec![sym_mesh]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "SymMeshInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(Affine2D::translate(60.0, 60.0)),
            None,
        )
        .unwrap();

        let opts = SeparationOptions {
            dpi: 25.4,
            max_dimension_px: 256,
            include_cut_layers: true,
        };
        let sep = compute_separations_with_text_outliner(&doc, 0, &opts, |_, _| {
            Some(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0)))
        })
        .unwrap();

        let m_plate = sep.plate(&SeparationChannel::Magenta).unwrap();
        assert!(
            m_plate.max_coverage_pct > 5.0,
            "Magenta DropShadow on live text must render even when text_outliner returns Some"
        );
        let y_plate = sep.plate(&SeparationChannel::Yellow).unwrap();
        assert!(
            y_plate.ink_map[70 * 100 + 70] > 128,
            "Yellow MeshFill inside SymbolInstance must render onto Yellow separation plate"
        );
    }

    #[test]
    fn separations_apply_raster_filters_for_raster_and_vector_plates() {
        use coree_core::{RasterAsset, RasterFilter, RasterLayerData};

        let make_doc = |filters: Vec<RasterFilter>| -> Document {
            let mut doc =
                Document::new("SepRasterFx", 60.0, 60.0, coree_core::ColorMode::Cmyk, 72.0);
            let asset = RasterAsset::new(
                8,
                8,
                [220u8, 40, 20, 255].repeat(64),
                72.0,
                coree_core::ColorMode::Rgb,
            )
            .unwrap();
            let aid = doc.register_asset(asset);
            doc.add_object(
                SceneObject::new(
                    ObjectId(0),
                    "RasterFx",
                    NodePayload::Raster(RasterLayerData {
                        asset_id: aid,
                        width_mm: 40.0,
                        height_mm: 40.0,
                        crop_rect: None,
                        filters,
                        filter_enabled: Vec::new(),
                    }),
                )
                .with_transform(Affine2D::translate(10.0, 10.0)),
                None,
            )
            .unwrap();
            doc
        };

        let doc_id = make_doc(Vec::new());
        let doc_fx = make_doc(vec![
            RasterFilter::ChannelMixer {
                red: [0.0, 1.0, 0.0],
                green: [1.0, 0.0, 0.0],
                blue: [0.0, 0.0, 1.0],
                monochrome: false,
            },
            RasterFilter::ToneCurve {
                points: vec![(0.0, 0.0), (0.5, 0.85), (1.0, 1.0)],
            },
        ]);

        // 1. Rasterized CMYK separations (`paint_raster_object`)
        let sep_opts = SeparationOptions {
            dpi: 25.4,
            max_dimension_px: 128,
            include_cut_layers: true,
        };
        let sep_id = compute_separations(&doc_id, 0, &sep_opts).unwrap();
        let sep_fx = compute_separations(&doc_fx, 0, &sep_opts).unwrap();
        let c_id = sep_id
            .plate(&SeparationChannel::Cyan)
            .unwrap()
            .max_coverage_pct;
        let m_id = sep_id
            .plate(&SeparationChannel::Magenta)
            .unwrap()
            .max_coverage_pct;
        let c_fx = sep_fx
            .plate(&SeparationChannel::Cyan)
            .unwrap()
            .max_coverage_pct;
        let m_fx = sep_fx
            .plate(&SeparationChannel::Magenta)
            .unwrap()
            .max_coverage_pct;
        assert!(
            m_id > c_id && c_fx > m_fx && c_fx > c_id + 40.0 && m_id > m_fx + 40.0,
            "compute_separations must reflect red->green CMYK shift (id C={c_id:.1}% M={m_id:.1}%, fx C={c_fx:.1}% M={m_fx:.1}%)"
        );

        // 2. Vector CMYK separations (`ExportDisplayList` -> `compute_vector_separations`) + PDF plates
        let vec_id = compute_vector_separations(&doc_id, 0, true).unwrap();
        let vec_fx = compute_vector_separations(&doc_fx, 0, true).unwrap();
        let ink_byte = |plates: &[SeparationPlate], ch: SeparationChannel| -> u8 {
            plates
                .iter()
                .find(|p| p.channel == ch)
                .and_then(|p| p.raster_ops.first())
                .and_then(|r| r.ink_channel.first().copied())
                .unwrap_or(0)
        };
        let v_c_id = ink_byte(&vec_id, SeparationChannel::Cyan);
        let v_m_id = ink_byte(&vec_id, SeparationChannel::Magenta);
        let v_c_fx = ink_byte(&vec_fx, SeparationChannel::Cyan);
        let v_m_fx = ink_byte(&vec_fx, SeparationChannel::Magenta);
        assert!(
            v_m_id > v_c_id && v_c_fx > v_m_fx && v_c_fx > v_c_id.saturating_add(100) && v_m_id > v_m_fx.saturating_add(100),
            "compute_vector_separations must reflect red->green CMYK shift (id C={v_c_id} M={v_m_id}, fx C={v_c_fx} M={v_m_fx})"
        );

        let pdf_plates_id = export_separated_plates_pdf_bytes(&doc_id, 0, &sep_opts).unwrap();
        let pdf_plates_fx = export_separated_plates_pdf_bytes(&doc_fx, 0, &sep_opts).unwrap();
        assert_ne!(
            pdf_plates_id, pdf_plates_fx,
            "separated PDF plates must differ when RasterFilters are applied"
        );
    }
}
