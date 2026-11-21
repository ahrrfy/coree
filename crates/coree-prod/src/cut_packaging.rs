// SPDX-License-Identifier: MPL-2.0

//! Cut, laser, CNC, and packaging production engine:
//! - One-click welded `CutContour` spot hairline generator with outward bleed/offset
//! - Parametric packaging dieline generator (`TuckEndBox`, `Envelope`, `ShoppingBag`, `StickerSheet`)
//!   with separate `Cut` (solid hairline) and `Crease` (dashed fold) layers
//! - Path Length & Area Meter with material utilization and laser/vinyl job estimates

use crate::error::{ProdError, ProdResult};
use crate::scene_helpers::{
    collect_silhouette_paths, find_or_create_layer, get_page, visit_layer_objects,
    world_bounds_with_xf,
};
use coree_core::{
    apply_boolean_many, offset_polygon, Affine2D, BezierPath, BooleanOp, Color, CornerStyle,
    Document, EllipseMode, Fill, Layer, LayerId, NodePayload, ObjectId, OutlineStyle,
    ParametricShape, Point2D, Rect2D, SceneObject,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Default spot color name for plotter/cutter contour paths.
pub const DEFAULT_CUT_SPOT_NAME: &str = "CutContour";

/// Default spot color name for packaging fold/crease lines.
pub const DEFAULT_CREASE_SPOT_NAME: &str = "Crease";

/// Result returned by [`generate_cut_contour`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CutContourResult {
    pub layer_id: LayerId,
    pub object_id: ObjectId,
    pub spot_name: String,
    pub contour_path: BezierPath,
    pub cut_length_mm: f64,
}

/// Welds the silhouettes of `target_ids`, applies an outward (or inward if negative)
/// `offset_polygon` offset of `offset_mm`, and places a `Hairline` `Color::Spot` stroke
/// with overprint enabled on a dedicated `is_cut_layer = true` layer.
pub fn generate_cut_contour(
    doc: &mut Document,
    target_ids: &[ObjectId],
    offset_mm: f64,
    spot_name: &str,
) -> ProdResult<CutContourResult> {
    if target_ids.is_empty() {
        return Err(ProdError::InvalidParameter(
            "generate_cut_contour requires at least one target object".into(),
        ));
    }
    if !offset_mm.is_finite() || offset_mm.abs() > 10_000.0 {
        return Err(ProdError::InvalidParameter(
            "offset_mm must be finite and within +/- 10000 mm".into(),
        ));
    }

    // Locate page hosting the first target object (falling back to active page)
    let page_index = target_ids
        .iter()
        .find_map(|&oid| find_page_index_of_object(doc, oid))
        .unwrap_or(doc.active_page_index);
    let _ = get_page(doc, page_index)?;

    let mut silhouettes = Vec::new();
    let mut visiting = HashSet::new();
    for &oid in target_ids {
        if !doc.objects.contains_key(&oid) {
            return Err(ProdError::Core(coree_core::CoreError::ObjectNotFound(oid)));
        }
        collect_silhouette_paths(
            doc,
            oid,
            Affine2D::IDENTITY,
            0,
            &mut visiting,
            &mut silhouettes,
        );
    }

    if silhouettes.is_empty() {
        return Err(ProdError::InvalidParameter(
            "selected objects produced no vector silhouette geometry".into(),
        ));
    }

    let welded_list = apply_boolean_many(BooleanOp::CreateBoundary, &silhouettes)?;
    let welded = welded_list.into_iter().next().ok_or_else(|| {
        ProdError::InvalidParameter("silhouette weld produced empty geometry".into())
    })?;

    let contour_path = if offset_mm.abs() > 1e-6 {
        offset_polygon(&welded, offset_mm)?
    } else {
        welded
    };

    if contour_path.is_empty() {
        return Err(ProdError::InvalidParameter(
            "offset_polygon collapsed silhouette to empty geometry".into(),
        ));
    }

    let resolved_spot = if spot_name.trim().is_empty() {
        DEFAULT_CUT_SPOT_NAME.to_owned()
    } else {
        spot_name.trim().to_owned()
    };

    let layer_id = find_or_create_layer(doc, page_index, &resolved_spot, true)?;
    doc.set_layer_flags(layer_id, Some(true), None, Some(true), Some(true))?;

    let mut outline = OutlineStyle::hairline(Color::spot(
        resolved_spot.clone(),
        1.0,
        [0.0, 1.0, 0.0, 0.0],
    ));
    outline.overprint = true;

    let cut_length_mm = contour_path.path_length_mm();
    let obj = SceneObject::new(
        ObjectId(0),
        format!("{resolved_spot} ({offset_mm:+.1} mm)"),
        NodePayload::Path(contour_path.clone()),
    )
    .with_fill(Fill::None)
    .with_outline(outline);

    let object_id = doc.add_object(obj, Some(layer_id))?;

    Ok(CutContourResult {
        layer_id,
        object_id,
        spot_name: resolved_spot,
        contour_path,
        cut_length_mm,
    })
}

fn find_page_index_of_object(doc: &Document, target_id: ObjectId) -> Option<usize> {
    for (p_idx, page) in doc.pages.iter().enumerate() {
        for layer in &page.layers {
            if layer.objects.contains(&target_id) {
                return Some(p_idx);
            }
            let mut found = false;
            visit_layer_objects(doc, layer, &mut |oid, _, _| {
                if oid == target_id {
                    found = true;
                }
            });
            if found {
                return Some(p_idx);
            }
        }
    }
    None
}

// =============================================================================
// Parametric Packaging Dieline Generator
// =============================================================================

/// Supported parametric packaging dieline templates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DielineKind {
    /// Reverse-tuck folding carton with dust flaps, top/bottom tuck flaps, and glue tab.
    ///
    /// Flat blank dimensions:
    /// - Width = `glue_flap_mm + 2 * length_mm + 2 * width_mm`
    /// - Height = `height_mm + 2 * width_mm + 2 * tuck_flap_mm`
    TuckEndBox {
        length_mm: f64,
        width_mm: f64,
        height_mm: f64,
        glue_flap_mm: f64,
        tuck_flap_mm: f64,
    },
    /// Commercial/pocket envelope blank with side glue flaps, bottom fold, and seal flap.
    ///
    /// Flat blank dimensions:
    /// - Width = `width_mm + 2 * side_flap` (`side_flap = (width_mm * 0.12).clamp(12.0, 25.0).min(width_mm * 0.45)`)
    /// - Height = `flap_mm + 2 * height_mm`
    Envelope {
        width_mm: f64,
        height_mm: f64,
        flap_mm: f64,
    },
    /// Paper shopping bag with top turnover reinforcement fold, side gussets, base fold,
    /// handle cord holes, and glue tab.
    ///
    /// Flat blank dimensions:
    /// - Width = `glue_flap + 2 * width_mm + 2 * depth_mm`
    /// - Height = `fold_mm + height_mm + base_flap`
    ShoppingBag {
        width_mm: f64,
        depth_mm: f64,
        height_mm: f64,
        fold_mm: f64,
    },
    /// Kiss-cut sticker/label sheet grid with rounded corners and backing peel-score lines.
    ///
    /// Flat grid dimensions:
    /// - Width = `cols * label_w_mm + (cols - 1) * gap_mm`
    /// - Height = `rows * label_h_mm + (rows - 1) * gap_mm`
    StickerSheet {
        label_w_mm: f64,
        label_h_mm: f64,
        corner_r_mm: f64,
        cols: u32,
        rows: u32,
        gap_mm: f64,
    },
}

/// Generated packaging dieline summary with separate `Cut` and `Crease` layer handles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DielineResult {
    pub cut_layer_id: LayerId,
    pub crease_layer_id: LayerId,
    pub cut_object_ids: Vec<ObjectId>,
    pub crease_object_ids: Vec<ObjectId>,
    pub flat_bounds: Rect2D,
    pub cut_length_mm: f64,
    pub crease_length_mm: f64,
}

/// Maximum physical dieline dimension in millimeters (100 meters, guards against float overflow).
const MAX_DIELINE_DIM_MM: f64 = 100_000.0;

fn check_pos(name: &str, v: f64) -> ProdResult<()> {
    if !v.is_finite() || v <= 0.0 || v > MAX_DIELINE_DIM_MM {
        return Err(ProdError::InvalidParameter(format!(
            "{name} must be positive and finite in (0, {MAX_DIELINE_DIM_MM}] (got {v})"
        )));
    }
    Ok(())
}

fn check_non_neg(name: &str, v: f64) -> ProdResult<()> {
    if !v.is_finite() || v < 0.0 || v > MAX_DIELINE_DIM_MM {
        return Err(ProdError::InvalidParameter(format!(
            "{name} must be non-negative and finite in [0, {MAX_DIELINE_DIM_MM}] (got {v})"
        )));
    }
    Ok(())
}

fn solid_cut_outline() -> OutlineStyle {
    let mut style = OutlineStyle::hairline(Color::spot(
        DEFAULT_CUT_SPOT_NAME,
        1.0,
        [0.0, 1.0, 0.0, 0.0],
    ));
    style.overprint = true;
    style
}

fn dashed_crease_outline() -> OutlineStyle {
    let mut style = OutlineStyle::hairline(Color::spot(
        DEFAULT_CREASE_SPOT_NAME,
        1.0,
        [0.0, 0.7, 1.0, 0.0],
    ));
    style.dash_pattern = vec![3.0, 2.0];
    style.overprint = true;
    style
}

/// Named `(cut_paths, crease_paths)` geometry pair produced by parametric dieline builders.
type DielinePaths = (Vec<(String, BezierPath)>, Vec<(String, BezierPath)>);

/// Generates a production-ready packaging dieline template on `doc.pages[page_index]`
/// with separate `Cut` (solid Hairline) and `Crease` (dashed fold lines) layers.
pub fn generate_dieline(
    doc: &mut Document,
    page_index: usize,
    kind: &DielineKind,
    origin: Point2D,
) -> ProdResult<DielineResult> {
    let _ = get_page(doc, page_index)?;
    if !origin.is_finite() {
        return Err(ProdError::InvalidParameter(
            "dieline origin must be finite".into(),
        ));
    }

    let (cut_paths, crease_paths): DielinePaths = match *kind {
        DielineKind::TuckEndBox {
            length_mm,
            width_mm,
            height_mm,
            glue_flap_mm,
            tuck_flap_mm,
        } => build_tuck_end_box(length_mm, width_mm, height_mm, glue_flap_mm, tuck_flap_mm)?,
        DielineKind::Envelope {
            width_mm,
            height_mm,
            flap_mm,
        } => build_envelope(width_mm, height_mm, flap_mm)?,
        DielineKind::ShoppingBag {
            width_mm,
            depth_mm,
            height_mm,
            fold_mm,
        } => build_shopping_bag(width_mm, depth_mm, height_mm, fold_mm)?,
        DielineKind::StickerSheet {
            label_w_mm,
            label_h_mm,
            corner_r_mm,
            cols,
            rows,
            gap_mm,
        } => build_sticker_sheet(label_w_mm, label_h_mm, corner_r_mm, cols, rows, gap_mm)?,
    };

    let cut_layer_id = find_or_create_layer(doc, page_index, "Cut", true)?;
    doc.set_layer_flags(cut_layer_id, Some(true), None, Some(true), Some(true))?;
    let crease_layer_id = find_or_create_layer(doc, page_index, "Crease", true)?;
    doc.set_layer_flags(crease_layer_id, Some(true), None, Some(true), Some(true))?;

    let xf = Affine2D::translate(origin.x, origin.y);
    let cut_style = solid_cut_outline();
    let crease_style = dashed_crease_outline();

    let mut cut_object_ids = Vec::with_capacity(cut_paths.len());
    let mut crease_object_ids = Vec::with_capacity(crease_paths.len());
    let mut flat_bounds: Option<Rect2D> = None;
    let mut cut_length_mm = 0.0;
    let mut crease_length_mm = 0.0;

    for (name, path) in cut_paths {
        let placed = path.transformed(&xf);
        cut_length_mm += placed.path_length_mm();
        if let Some(bb) = placed.bounding_box() {
            flat_bounds = Some(match flat_bounds {
                Some(prev) => prev.union(bb),
                None => bb,
            });
        }
        let obj = SceneObject::new(ObjectId(0), name, NodePayload::Path(placed))
            .with_fill(Fill::None)
            .with_outline(cut_style.clone());
        cut_object_ids.push(doc.add_object(obj, Some(cut_layer_id))?);
    }

    for (name, path) in crease_paths {
        let placed = path.transformed(&xf);
        crease_length_mm += placed.path_length_mm();
        if let Some(bb) = placed.bounding_box() {
            flat_bounds = Some(match flat_bounds {
                Some(prev) => prev.union(bb),
                None => bb,
            });
        }
        let obj = SceneObject::new(ObjectId(0), name, NodePayload::Path(placed))
            .with_fill(Fill::None)
            .with_outline(crease_style.clone());
        crease_object_ids.push(doc.add_object(obj, Some(crease_layer_id))?);
    }

    Ok(DielineResult {
        cut_layer_id,
        crease_layer_id,
        cut_object_ids,
        crease_object_ids,
        flat_bounds: flat_bounds.unwrap_or_default(),
        cut_length_mm,
        crease_length_mm,
    })
}

fn build_tuck_end_box(
    length_mm: f64,
    width_mm: f64,
    height_mm: f64,
    glue_flap_mm: f64,
    tuck_flap_mm: f64,
) -> ProdResult<DielinePaths> {
    check_pos("length_mm", length_mm)?;
    check_pos("width_mm", width_mm)?;
    check_pos("height_mm", height_mm)?;
    check_pos("glue_flap_mm", glue_flap_mm)?;
    check_pos("tuck_flap_mm", tuck_flap_mm)?;

    let x0 = 0.0;
    let x1 = glue_flap_mm;
    let x2 = x1 + length_mm;
    let x3 = x2 + width_mm;
    let x4 = x3 + length_mm;
    let x5 = x4 + width_mm;

    let y_top_tuck = 0.0;
    let y_top_lid = tuck_flap_mm;
    let y_body_top = tuck_flap_mm + width_mm;
    let y_body_bot = y_body_top + height_mm;
    let y_bot_lid = y_body_bot + width_mm;
    let y_bot_tuck = y_bot_lid + tuck_flap_mm;

    let dust_h = (width_mm * 0.6).min(length_mm * 0.45);
    let y_top_dust = y_body_top - dust_h;
    let y_bot_dust = y_body_bot + dust_h;

    let glue_taper = (glue_flap_mm * 0.25).min(height_mm * 0.15).min(4.0);
    let tuck_chamfer = (tuck_flap_mm * 0.25).min(length_mm * 0.1).min(4.0);
    let dust_chamfer = (dust_h * 0.2).min(width_mm * 0.15).min(3.0);

    // Single continuous outer die-cut contour (clockwise)
    let outer_pts = vec![
        Point2D::new(x0, y_body_top + glue_taper),
        Point2D::new(x1, y_body_top),
        // Top lid + tuck flap above Front panel [x1..x2]
        Point2D::new(x1, y_top_lid),
        Point2D::new(x1 + tuck_chamfer, y_top_tuck),
        Point2D::new(x2 - tuck_chamfer, y_top_tuck),
        Point2D::new(x2, y_top_lid),
        Point2D::new(x2, y_body_top),
        // Top Right dust flap [x2..x3]
        Point2D::new(x2 + dust_chamfer, y_top_dust),
        Point2D::new(x3 - dust_chamfer, y_top_dust),
        Point2D::new(x3, y_body_top),
        // Back panel top edge [x3..x4]
        Point2D::new(x4, y_body_top),
        // Top Left dust flap [x4..x5]
        Point2D::new(x4 + dust_chamfer, y_top_dust),
        Point2D::new(x5 - dust_chamfer, y_top_dust),
        Point2D::new(x5, y_body_top),
        // Right outer edge down
        Point2D::new(x5, y_body_bot),
        // Bottom Left dust flap [x4..x5]
        Point2D::new(x5 - dust_chamfer, y_bot_dust),
        Point2D::new(x4 + dust_chamfer, y_bot_dust),
        Point2D::new(x4, y_body_bot),
        // Bottom lid + reverse tuck flap below Back panel [x3..x4]
        Point2D::new(x4, y_bot_lid),
        Point2D::new(x4 - tuck_chamfer, y_bot_tuck),
        Point2D::new(x3 + tuck_chamfer, y_bot_tuck),
        Point2D::new(x3, y_bot_lid),
        Point2D::new(x3, y_body_bot),
        // Bottom Right dust flap [x2..x3]
        Point2D::new(x3 - dust_chamfer, y_bot_dust),
        Point2D::new(x2 + dust_chamfer, y_bot_dust),
        Point2D::new(x2, y_body_bot),
        // Front panel bottom edge [x1..x2]
        Point2D::new(x1, y_body_bot),
        // Glue tab bottom bevel
        Point2D::new(x0, y_body_bot - glue_taper),
    ];

    let cut_paths = vec![(
        "TuckEndBox Outer Cut".to_owned(),
        BezierPath::from_polygon(outer_pts, true),
    )];

    let crease_lines = [
        (
            "Crease Glue Hinge",
            Point2D::new(x1, y_body_top),
            Point2D::new(x1, y_body_bot),
        ),
        (
            "Crease Panel 1-2",
            Point2D::new(x2, y_body_top),
            Point2D::new(x2, y_body_bot),
        ),
        (
            "Crease Panel 2-3",
            Point2D::new(x3, y_body_top),
            Point2D::new(x3, y_body_bot),
        ),
        (
            "Crease Panel 3-4",
            Point2D::new(x4, y_body_top),
            Point2D::new(x4, y_body_bot),
        ),
        (
            "Crease Top Lid Hinge",
            Point2D::new(x1, y_body_top),
            Point2D::new(x2, y_body_top),
        ),
        (
            "Crease Top Tuck",
            Point2D::new(x1, y_top_lid),
            Point2D::new(x2, y_top_lid),
        ),
        (
            "Crease Top Dust R",
            Point2D::new(x2, y_body_top),
            Point2D::new(x3, y_body_top),
        ),
        (
            "Crease Top Dust L",
            Point2D::new(x4, y_body_top),
            Point2D::new(x5, y_body_top),
        ),
        (
            "Crease Bottom Dust R",
            Point2D::new(x2, y_body_bot),
            Point2D::new(x3, y_body_bot),
        ),
        (
            "Crease Bottom Lid Hinge",
            Point2D::new(x3, y_body_bot),
            Point2D::new(x4, y_body_bot),
        ),
        (
            "Crease Bottom Tuck",
            Point2D::new(x3, y_bot_lid),
            Point2D::new(x4, y_bot_lid),
        ),
        (
            "Crease Bottom Dust L",
            Point2D::new(x4, y_body_bot),
            Point2D::new(x5, y_body_bot),
        ),
    ];

    let crease_paths = crease_lines
        .into_iter()
        .map(|(n, a, b)| (n.to_owned(), BezierPath::from_line(a, b)))
        .collect();

    Ok((cut_paths, crease_paths))
}

fn build_envelope(width_mm: f64, height_mm: f64, flap_mm: f64) -> ProdResult<DielinePaths> {
    check_pos("width_mm", width_mm)?;
    check_pos("height_mm", height_mm)?;
    check_pos("flap_mm", flap_mm)?;

    let side_flap = (width_mm * 0.12).clamp(12.0, 25.0).min(width_mm * 0.45);
    let x0 = 0.0;
    let x1 = side_flap;
    let x2 = side_flap + width_mm;
    let x3 = x2 + side_flap;

    let y0 = 0.0;
    let y1 = flap_mm;
    let y2 = flap_mm + height_mm;
    let y3 = y2 + height_mm;

    let flap_taper = (width_mm * 0.12).min(flap_mm * 0.8);
    let side_taper = (side_flap * 0.45).min(height_mm * 0.2);
    let bot_taper = (width_mm * 0.02).min(4.0);

    let outer_pts = vec![
        // Top seal flap
        Point2D::new(x1, y1),
        Point2D::new(x1 + flap_taper, y0),
        Point2D::new(x2 - flap_taper, y0),
        Point2D::new(x2, y1),
        // Right side glue flap
        Point2D::new(x3, y1 + side_taper),
        Point2D::new(x3, y2 - side_taper),
        Point2D::new(x2, y2),
        // Bottom body fold panel
        Point2D::new(x2 - bot_taper, y3),
        Point2D::new(x1 + bot_taper, y3),
        Point2D::new(x1, y2),
        // Left side glue flap
        Point2D::new(x0, y2 - side_taper),
        Point2D::new(x0, y1 + side_taper),
    ];

    let cut_paths = vec![(
        "Envelope Outer Cut".to_owned(),
        BezierPath::from_polygon(outer_pts, true),
    )];

    let crease_lines = [
        (
            "Crease Top Seal Flap",
            Point2D::new(x1, y1),
            Point2D::new(x2, y1),
        ),
        (
            "Crease Bottom Fold",
            Point2D::new(x1, y2),
            Point2D::new(x2, y2),
        ),
        (
            "Crease Left Flap",
            Point2D::new(x1, y1),
            Point2D::new(x1, y2),
        ),
        (
            "Crease Right Flap",
            Point2D::new(x2, y1),
            Point2D::new(x2, y2),
        ),
    ];
    let crease_paths = crease_lines
        .into_iter()
        .map(|(n, a, b)| (n.to_owned(), BezierPath::from_line(a, b)))
        .collect();

    Ok((cut_paths, crease_paths))
}

fn build_shopping_bag(
    width_mm: f64,
    depth_mm: f64,
    height_mm: f64,
    fold_mm: f64,
) -> ProdResult<DielinePaths> {
    check_pos("width_mm", width_mm)?;
    check_pos("depth_mm", depth_mm)?;
    check_pos("height_mm", height_mm)?;
    check_non_neg("fold_mm", fold_mm)?;

    let glue_flap = (depth_mm * 0.3).clamp(15.0, 25.0).min(width_mm * 0.5);
    let base_flap = (depth_mm * 0.5 + 10.0).max(10.0);

    let x0 = 0.0;
    let x1 = glue_flap;
    let x2 = x1 + width_mm;
    let x3 = x2 + depth_mm;
    let x4 = x3 + width_mm;
    let x5 = x4 + depth_mm;

    let y0 = 0.0;
    let y1 = fold_mm;
    let y2 = fold_mm + height_mm;
    let y3 = y2 + base_flap;

    let glue_taper = (glue_flap * 0.25).min(height_mm * 0.1).min(5.0);

    let outer_pts = vec![
        Point2D::new(x0, y0 + glue_taper),
        Point2D::new(x1, y0),
        Point2D::new(x5, y0),
        Point2D::new(x5, y3),
        Point2D::new(x1, y3),
        Point2D::new(x0, y3 - glue_taper),
    ];

    let mut cut_paths = vec![(
        "ShoppingBag Outer Cut".to_owned(),
        BezierPath::from_polygon(outer_pts, true),
    )];

    // Handle cord punch holes on Front and Back panels
    let hole_r = (width_mm * 0.015).clamp(2.0, 4.0);
    let hole_y = y1 + (height_mm * 0.08).clamp(hole_r * 2.5, 25.0);
    if hole_y + hole_r < y2 {
        let handle_span = width_mm * 0.4;
        let front_cx = x1 + width_mm * 0.5;
        let back_cx = x3 + width_mm * 0.5;
        let hole_centers = [
            (
                "Front Handle Hole L",
                Point2D::new(front_cx - handle_span * 0.5, hole_y),
            ),
            (
                "Front Handle Hole R",
                Point2D::new(front_cx + handle_span * 0.5, hole_y),
            ),
            (
                "Back Handle Hole L",
                Point2D::new(back_cx - handle_span * 0.5, hole_y),
            ),
            (
                "Back Handle Hole R",
                Point2D::new(back_cx + handle_span * 0.5, hole_y),
            ),
        ];
        for (name, center) in hole_centers {
            let mut hole = ParametricShape::Ellipse {
                rx: hole_r,
                ry: hole_r,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }
            .to_bezier_path();
            hole.transform(&Affine2D::translate(center.x, center.y));
            cut_paths.push((name.to_owned(), hole));
        }
    }

    let mut crease_paths = Vec::new();
    if fold_mm > 1e-6 {
        crease_paths.push((
            "Crease Top Turnover Fold".to_owned(),
            BezierPath::from_line(Point2D::new(x0, y1), Point2D::new(x5, y1)),
        ));
    }
    crease_paths.push((
        "Crease Base Fold".to_owned(),
        BezierPath::from_line(Point2D::new(x0, y2), Point2D::new(x5, y2)),
    ));

    // Vertical panel & gusset creases
    let v_creases = [
        ("Crease Glue Hinge", x1),
        ("Crease Front-RightGusset", x2),
        ("Crease RightGusset Center", x2 + depth_mm * 0.5),
        ("Crease RightGusset-Back", x3),
        ("Crease Back-LeftGusset", x4),
        ("Crease LeftGusset Center", x4 + depth_mm * 0.5),
    ];
    for (name, x) in v_creases {
        crease_paths.push((
            name.to_owned(),
            BezierPath::from_line(Point2D::new(x, y0), Point2D::new(x, y3)),
        ));
    }

    // Diagonal 45-degree gusset base fold creases
    let half_d = depth_mm * 0.5;
    let diag_h = half_d.min(base_flap);
    let diag_lines = [
        (
            "Crease RightGusset Base Diag L",
            Point2D::new(x2 + half_d, y2),
            Point2D::new(x2 + half_d - diag_h, y2 + diag_h),
        ),
        (
            "Crease RightGusset Base Diag R",
            Point2D::new(x2 + half_d, y2),
            Point2D::new(x2 + half_d + diag_h, y2 + diag_h),
        ),
        (
            "Crease LeftGusset Base Diag L",
            Point2D::new(x4 + half_d, y2),
            Point2D::new(x4 + half_d - diag_h, y2 + diag_h),
        ),
        (
            "Crease LeftGusset Base Diag R",
            Point2D::new(x4 + half_d, y2),
            Point2D::new(x4 + half_d + diag_h, y2 + diag_h),
        ),
    ];
    for (name, a, b) in diag_lines {
        crease_paths.push((name.to_owned(), BezierPath::from_line(a, b)));
    }

    Ok((cut_paths, crease_paths))
}

/// Maximum total labels allowed in a single [`DielineKind::StickerSheet`] (guards against OOM).
pub const MAX_STICKER_CELLS: usize = 10_000;

fn build_sticker_sheet(
    label_w_mm: f64,
    label_h_mm: f64,
    corner_r_mm: f64,
    cols: u32,
    rows: u32,
    gap_mm: f64,
) -> ProdResult<DielinePaths> {
    check_pos("label_w_mm", label_w_mm)?;
    check_pos("label_h_mm", label_h_mm)?;
    check_non_neg("corner_r_mm", corner_r_mm)?;
    check_non_neg("gap_mm", gap_mm)?;
    if cols == 0 || rows == 0 {
        return Err(ProdError::InvalidParameter(
            "StickerSheet cols and rows must both be >= 1".into(),
        ));
    }
    let total_cells = (cols as usize)
        .checked_mul(rows as usize)
        .filter(|&n| n <= MAX_STICKER_CELLS)
        .ok_or_else(|| {
            ProdError::InvalidParameter(format!(
                "StickerSheet {cols}x{rows} exceeds maximum {MAX_STICKER_CELLS} labels"
            ))
        })?;

    let r = corner_r_mm.min(0.5 * label_w_mm.min(label_h_mm));
    let total_w = f64::from(cols) * label_w_mm + f64::from(cols - 1) * gap_mm;

    let mut cut_paths = Vec::with_capacity(total_cells);
    for row in 0..rows {
        for col in 0..cols {
            let x = f64::from(col) * (label_w_mm + gap_mm);
            let y = f64::from(row) * (label_h_mm + gap_mm);
            let mut label = ParametricShape::Rectangle {
                width: label_w_mm,
                height: label_h_mm,
                corner_radii: [r; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }
            .to_bezier_path();
            label.transform(&Affine2D::translate(x, y));
            cut_paths.push((format!("Sticker R{}C{}", row + 1, col + 1), label));
        }
    }

    // Backing-liner peel-score crease lines across each row centerline
    let mut crease_paths = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mid_y = f64::from(row) * (label_h_mm + gap_mm) + label_h_mm * 0.5;
        crease_paths.push((
            format!("Backing Peel Score Row {}", row + 1),
            BezierPath::from_line(Point2D::new(0.0, mid_y), Point2D::new(total_w, mid_y)),
        ));
    }

    Ok((cut_paths, crease_paths))
}

// =============================================================================
// Path Length & Area Meter
// =============================================================================

/// Complete production geometry, area, material utilization, and laser/vinyl job metrics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProductionMetrics {
    pub object_count: usize,
    pub node_count: usize,
    pub subpath_count: usize,
    /// Total length of all vector paths (`cut + crease + artwork`) in mm & meters.
    pub total_vector_length_mm: f64,
    pub total_vector_length_m: f64,
    /// Total solid die-cut / laser-cut path length in mm & meters.
    pub cut_length_mm: f64,
    pub cut_length_m: f64,
    /// Total dashed fold / crease / score path length in mm & meters.
    pub crease_length_mm: f64,
    pub crease_length_m: f64,
    /// Total non-cut artwork vector path length in mm.
    pub artwork_length_mm: f64,
    /// Total enclosed filled surface area in mm² & m².
    pub filled_area_mm2: f64,
    pub filled_area_m2: f64,
    /// Total enclosed area of closed cut contours in mm² (used for utilization if unfilled).
    pub cut_enclosed_area_mm2: f64,
    /// Combined world-space bounding box of measured geometry.
    pub bounding_box: Option<Rect2D>,
    /// Bounding sheet rectangle area (`width * height`) in mm² & m².
    pub bounding_sheet_area_mm2: f64,
    pub bounding_sheet_area_m2: f64,
    /// Material utilization percentage (`0.0..=100.0`).
    pub material_utilization_pct: f64,
    /// Estimated laser cutter job time in seconds (assuming 25 mm/s cut, 45 mm/s score + pierces).
    pub estimated_laser_time_sec: f64,
    /// Estimated vinyl plotter job time in seconds (assuming 150 mm/s blade speed + pen lifts).
    pub estimated_vinyl_time_sec: f64,
    /// Estimated vinyl roll linear consumption in meters (`bounding height / 1000`).
    pub estimated_vinyl_roll_m: f64,
}

/// Computes production vector cut length, crease length, filled area, bounding sheet area,
/// material utilization %, and estimated laser/vinyl job metrics.
///
/// When `selection` is `Some(ids)`, only those objects (and their nested group children)
/// are measured; when `None`, all visible objects across the document are measured.
#[must_use]
pub fn compute_production_metrics(
    doc: &Document,
    selection: Option<&[ObjectId]>,
) -> ProductionMetrics {
    let mut metrics = ProductionMetrics::default();
    let mut cut_subpaths = 0usize;

    match selection {
        Some(ids) => {
            let mut visited = HashSet::new();
            for &oid in ids {
                let layer = find_layer_of_any_object(doc, oid);
                accumulate_object_metrics(
                    doc,
                    oid,
                    layer,
                    Affine2D::IDENTITY,
                    None,
                    0,
                    &mut visited,
                    &mut metrics,
                    &mut cut_subpaths,
                );
            }
        }
        None => {
            let mut visited = HashSet::new();
            for page in &doc.pages {
                for layer in &page.layers {
                    if !layer.visible {
                        continue;
                    }
                    for &oid in &layer.objects {
                        accumulate_object_metrics(
                            doc,
                            oid,
                            Some(layer),
                            Affine2D::IDENTITY,
                            None,
                            0,
                            &mut visited,
                            &mut metrics,
                            &mut cut_subpaths,
                        );
                    }
                }
            }
        }
    }

    metrics.total_vector_length_mm =
        metrics.cut_length_mm + metrics.crease_length_mm + metrics.artwork_length_mm;
    metrics.total_vector_length_m = metrics.total_vector_length_mm / 1_000.0;
    metrics.cut_length_m = metrics.cut_length_mm / 1_000.0;
    metrics.crease_length_m = metrics.crease_length_mm / 1_000.0;
    metrics.filled_area_m2 = metrics.filled_area_mm2 / 1_000_000.0;

    if let Some(bb) = metrics.bounding_box {
        let sheet_area = bb.width() * bb.height();
        if sheet_area.is_finite() && sheet_area >= 0.0 {
            metrics.bounding_sheet_area_mm2 = sheet_area;
            metrics.bounding_sheet_area_m2 = sheet_area / 1_000_000.0;
            let utilized_area = if metrics.filled_area_mm2 > 1e-6 {
                metrics.filled_area_mm2
            } else {
                metrics.cut_enclosed_area_mm2
            };
            if sheet_area > 1e-6 && utilized_area.is_finite() {
                metrics.material_utilization_pct =
                    (utilized_area / sheet_area * 100.0).clamp(0.0, 100.0);
            }
        }
        if bb.height().is_finite() && bb.height() >= 0.0 {
            metrics.estimated_vinyl_roll_m = bb.height() / 1_000.0;
        }
    }

    // Realistic laser cutter & vinyl plotter time estimates
    let cut_len_for_machine = if metrics.cut_length_mm > 0.0 || metrics.crease_length_mm > 0.0 {
        metrics.cut_length_mm
    } else {
        metrics.total_vector_length_mm
    };
    let subpaths_for_lifts = if cut_subpaths > 0 {
        cut_subpaths
    } else {
        metrics.subpath_count
    };
    metrics.estimated_laser_time_sec = cut_len_for_machine / 25.0
        + metrics.crease_length_mm / 45.0
        + (subpaths_for_lifts as f64) * 0.25;
    metrics.estimated_vinyl_time_sec =
        cut_len_for_machine / 150.0 + (subpaths_for_lifts as f64) * 0.15;

    metrics
}

fn find_layer_of_any_object(doc: &Document, target_id: ObjectId) -> Option<&Layer> {
    for page in &doc.pages {
        for layer in &page.layers {
            if layer.objects.contains(&target_id) {
                return Some(layer);
            }
            let mut found = false;
            visit_layer_objects(doc, layer, &mut |oid, _, _| {
                if oid == target_id {
                    found = true;
                }
            });
            if found {
                return Some(layer);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathRole {
    Cut,
    Crease,
    Artwork,
}

fn classify_path_role(layer: Option<&Layer>, obj: &SceneObject) -> PathRole {
    let layer_is_crease = layer.is_some_and(|l| {
        let lower = l.name.to_ascii_lowercase();
        lower.contains("crease") || lower.contains("fold") || lower.contains("score")
    });
    let spot_is_crease = matches!(
        &obj.outline.color,
        Color::Spot { name, .. }
            if name.to_ascii_lowercase().contains("crease")
                || name.to_ascii_lowercase().contains("fold")
    );
    let spot_is_cut = matches!(
        &obj.outline.color,
        Color::Spot { name, .. }
            if name.to_ascii_lowercase().contains("cut")
                || name.to_ascii_lowercase().contains("die")
    );
    let layer_is_cut = layer.is_some_and(|l| l.is_cut_layer);

    if layer_is_crease || spot_is_crease || (layer_is_cut && !obj.outline.dash_pattern.is_empty()) {
        PathRole::Crease
    } else if layer_is_cut || spot_is_cut {
        PathRole::Cut
    } else {
        PathRole::Artwork
    }
}

#[allow(clippy::too_many_arguments)]
fn accumulate_object_metrics(
    doc: &Document,
    id: ObjectId,
    layer: Option<&Layer>,
    parent_xf: Affine2D,
    clip_bounds: Option<Rect2D>,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
    metrics: &mut ProductionMetrics,
    cut_subpaths: &mut usize,
) {
    if depth > crate::scene_helpers::MAX_SCENE_DEPTH || !visited.insert(id) {
        return;
    }
    let Some(obj) = doc.objects.get(&id) else {
        return;
    };
    if !obj.visible {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return;
    }

    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children {
                accumulate_object_metrics(
                    doc,
                    cid,
                    layer,
                    world_xf,
                    clip_bounds,
                    depth + 1,
                    visited,
                    metrics,
                    cut_subpaths,
                );
            }
            return;
        }
        NodePayload::PowerClip {
            container,
            contents,
        } => {
            let path = container.transformed(&world_xf);
            let container_bb = path.bounding_box();
            let child_clip = match (clip_bounds, container_bb) {
                (Some(a), Some(b)) => a.intersection(b),
                (None, Some(b)) => Some(b),
                (Some(a), None) => Some(a),
                (None, None) => None,
            };
            record_path_metrics(obj, layer, &path, clip_bounds, metrics, cut_subpaths);
            for &cid in contents {
                accumulate_object_metrics(
                    doc,
                    cid,
                    layer,
                    world_xf,
                    child_clip,
                    depth + 1,
                    visited,
                    metrics,
                    cut_subpaths,
                );
            }
            return;
        }
        _ => {}
    }

    let world_path = match &obj.payload {
        NodePayload::Path(p) => Some(p.transformed(&world_xf)),
        NodePayload::Shape(s) => Some(s.to_bezier_path().transformed(&world_xf)),
        NodePayload::Table(t) => Some(t.to_grid_paths().transformed(&world_xf)),
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints_with_transform(c, &world_xf);
            Some(c.route_path(a, b))
        }
        _ => None,
    };

    if let Some(path) = world_path {
        record_path_metrics(obj, layer, &path, clip_bounds, metrics, cut_subpaths);
    } else if let Some(bb) = world_bounds_with_xf(doc, id, obj, &world_xf) {
        let effective_bb = match clip_bounds {
            Some(clip) => bb.intersection(clip),
            None => Some(bb),
        };
        metrics.object_count += 1;
        if let Some(ebb) = effective_bb {
            metrics.bounding_box = Some(match metrics.bounding_box {
                Some(prev) => prev.union(ebb),
                None => ebb,
            });
        }
    }
}

fn record_path_metrics(
    obj: &SceneObject,
    layer: Option<&Layer>,
    path: &BezierPath,
    clip_bounds: Option<Rect2D>,
    metrics: &mut ProductionMetrics,
    cut_subpaths: &mut usize,
) {
    if path.is_empty() {
        return;
    }
    let raw_len = path.path_length_mm();
    let len = if raw_len.is_finite() && raw_len >= 0.0 {
        raw_len
    } else {
        0.0
    };
    let raw_area = path.area_mm2();
    let area = if raw_area.is_finite() && raw_area >= 0.0 {
        raw_area
    } else {
        0.0
    };
    let role = classify_path_role(layer, obj);

    metrics.object_count += 1;
    metrics.node_count += path.node_count();
    metrics.subpath_count += path.subpaths.len();

    match role {
        PathRole::Cut => {
            metrics.cut_length_mm += len;
            metrics.cut_enclosed_area_mm2 += area;
            *cut_subpaths += path.subpaths.len();
        }
        PathRole::Crease => {
            metrics.crease_length_mm += len;
            *cut_subpaths += path.subpaths.len();
        }
        PathRole::Artwork => {
            metrics.artwork_length_mm += len;
        }
    }

    if obj.fill.is_visible() {
        metrics.filled_area_mm2 += area;
    }

    if let Some(bb) = path.bounding_box() {
        let effective_bb = match clip_bounds {
            Some(clip) => bb.intersection(clip),
            None => Some(bb),
        };
        if let Some(ebb) = effective_bb {
            metrics.bounding_box = Some(match metrics.bounding_box {
                Some(prev) => prev.union(ebb),
                None => ebb,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::ColorMode;

    fn rect_obj(x: f64, y: f64, w: f64, h: f64, fill: Fill) -> SceneObject {
        SceneObject::new(
            ObjectId(0),
            "Artwork",
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

    #[test]
    fn cut_contour_welds_and_offsets_on_dedicated_cut_layer() {
        let mut doc = Document::new_a4("Sticker Cut");
        let a = doc
            .add_object(
                rect_obj(10.0, 10.0, 40.0, 20.0, Fill::Solid(Color::CYAN)),
                None,
            )
            .unwrap();
        let b = doc
            .add_object(
                rect_obj(30.0, 10.0, 40.0, 20.0, Fill::Solid(Color::MAGENTA)),
                None,
            )
            .unwrap();

        // Union is 60x20; outward offset by 2mm => 64x24 (area ~1536 mm² minus tiny corner rounding)
        let res = generate_cut_contour(&mut doc, &[a, b], 2.0, "CutContour").unwrap();
        let cut_layer = doc.find_layer(res.layer_id).unwrap();
        assert!(cut_layer.is_cut_layer);
        assert_eq!(cut_layer.name, "CutContour");

        let obj = &doc.objects[&res.object_id];
        assert!(obj.outline.is_hairline);
        assert!(obj.outline.overprint);
        assert!(matches!(
            &obj.outline.color,
            Color::Spot { name, .. } if name == "CutContour"
        ));

        let bb = doc.object_bounds(res.object_id).unwrap();
        assert!((bb.width() - 64.0).abs() < 0.25, "width was {}", bb.width());
        assert!(
            (bb.height() - 24.0).abs() < 0.25,
            "height was {}",
            bb.height()
        );
        assert!(res.cut_length_mm > 160.0);
    }

    #[test]
    fn all_four_parametric_dielines_have_exact_dimensions_and_cut_crease_layers() {
        // 1. TuckEndBox: L=60, W=30, H=100, G=12, T=15
        // Expected flat width = 12 + 2*60 + 2*30 = 192; height = 100 + 2*30 + 2*15 = 190
        let mut doc1 = Document::new("Carton", 300.0, 300.0, ColorMode::Cmyk, 300.0);
        let box_res = generate_dieline(
            &mut doc1,
            0,
            &DielineKind::TuckEndBox {
                length_mm: 60.0,
                width_mm: 30.0,
                height_mm: 100.0,
                glue_flap_mm: 12.0,
                tuck_flap_mm: 15.0,
            },
            Point2D::new(10.0, 10.0),
        )
        .unwrap();
        assert!((box_res.flat_bounds.width() - 192.0).abs() < 1e-6);
        assert!((box_res.flat_bounds.height() - 190.0).abs() < 1e-6);
        assert!(doc1.find_layer(box_res.cut_layer_id).unwrap().is_cut_layer);
        assert!(
            doc1.find_layer(box_res.crease_layer_id)
                .unwrap()
                .is_cut_layer
        );
        assert!(!box_res.cut_object_ids.is_empty());
        assert_eq!(box_res.crease_object_ids.len(), 12);
        // Verify Cut is solid hairline & Crease is dashed hairline
        let cut_obj = &doc1.objects[&box_res.cut_object_ids[0]];
        assert!(cut_obj.outline.is_hairline && cut_obj.outline.dash_pattern.is_empty());
        let crease_obj = &doc1.objects[&box_res.crease_object_ids[0]];
        assert!(crease_obj.outline.is_hairline && !crease_obj.outline.dash_pattern.is_empty());

        // 2. Envelope: W=220, H=110, F=35 -> side_flap = 220*0.12 = 25 (clamped)
        // Flat width = 220 + 50 = 270; height = 35 + 2*110 = 255
        let mut doc2 = Document::new("DL Envelope", 350.0, 350.0, ColorMode::Cmyk, 300.0);
        let env_res = generate_dieline(
            &mut doc2,
            0,
            &DielineKind::Envelope {
                width_mm: 220.0,
                height_mm: 110.0,
                flap_mm: 35.0,
            },
            Point2D::ZERO,
        )
        .unwrap();
        assert!((env_res.flat_bounds.width() - 270.0).abs() < 1e-6);
        assert!((env_res.flat_bounds.height() - 255.0).abs() < 1e-6);
        assert_eq!(env_res.crease_object_ids.len(), 4);

        // 3. ShoppingBag: W=200, D=80, H=250, fold=40 -> G=24, B=50
        // Flat width = 24 + 400 + 160 = 584; height = 40 + 250 + 50 = 340
        let mut doc3 = Document::new("Luxury Bag", 700.0, 450.0, ColorMode::Cmyk, 300.0);
        let bag_res = generate_dieline(
            &mut doc3,
            0,
            &DielineKind::ShoppingBag {
                width_mm: 200.0,
                depth_mm: 80.0,
                height_mm: 250.0,
                fold_mm: 40.0,
            },
            Point2D::ZERO,
        )
        .unwrap();
        assert!((bag_res.flat_bounds.width() - 584.0).abs() < 1e-6);
        assert!((bag_res.flat_bounds.height() - 340.0).abs() < 1e-6);
        assert_eq!(bag_res.cut_object_ids.len(), 5); // outer + 4 handle holes
        assert!(bag_res.crease_object_ids.len() >= 10);

        // 4. StickerSheet: 50x30 labels, r=3, 3x4 grid, gap=5 -> W = 3*50+10 = 160, H = 4*30+15 = 135
        let mut doc4 = Document::new_a4("Stickers");
        let st_res = generate_dieline(
            &mut doc4,
            0,
            &DielineKind::StickerSheet {
                label_w_mm: 50.0,
                label_h_mm: 30.0,
                corner_r_mm: 3.0,
                cols: 3,
                rows: 4,
                gap_mm: 5.0,
            },
            Point2D::new(15.0, 20.0),
        )
        .unwrap();
        assert!((st_res.flat_bounds.width() - 160.0).abs() < 1e-4);
        assert!((st_res.flat_bounds.height() - 135.0).abs() < 1e-4);
        assert_eq!(st_res.cut_object_ids.len(), 12);
        assert_eq!(st_res.crease_object_ids.len(), 4);
    }

    #[test]
    fn production_metrics_computes_cut_crease_area_and_utilization() {
        let mut doc = Document::new_a4("Vinyl Job");
        // Two 50x40 filled squares placed at (0,0) and (50,0) => bounding sheet 100x40 = 4000 mm²
        // Total filled area = 2 * 2000 = 4000 mm² => 100% utilization
        doc.add_object(
            rect_obj(0.0, 0.0, 50.0, 40.0, Fill::Solid(Color::CYAN)),
            None,
        )
        .unwrap();
        doc.add_object(
            rect_obj(50.0, 0.0, 50.0, 40.0, Fill::Solid(Color::MAGENTA)),
            None,
        )
        .unwrap();

        // Add a Cut line (100 mm) and a Crease line (40 mm) inside the same 100x40 box
        let cut_layer = doc
            .add_layer(
                doc.pages[0].id,
                "Cut",
                true,
                coree_core::MasterLayerScope::None,
            )
            .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Die Line",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(100.0, 0.0),
                )),
            )
            .with_outline(solid_cut_outline()),
            Some(cut_layer),
        )
        .unwrap();
        let crease_layer = doc
            .add_layer(
                doc.pages[0].id,
                "Crease",
                true,
                coree_core::MasterLayerScope::None,
            )
            .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Fold Line",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(50.0, 0.0),
                    Point2D::new(50.0, 40.0),
                )),
            )
            .with_outline(dashed_crease_outline()),
            Some(crease_layer),
        )
        .unwrap();

        let m = compute_production_metrics(&doc, None);
        assert!((m.cut_length_mm - 100.0).abs() < 1e-4);
        assert!((m.cut_length_m - 0.1).abs() < 1e-6);
        assert!((m.crease_length_mm - 40.0).abs() < 1e-4);
        assert!((m.crease_length_m - 0.04).abs() < 1e-6);
        assert!((m.filled_area_mm2 - 4000.0).abs() < 1e-2);
        assert!((m.filled_area_m2 - 0.004).abs() < 1e-6);
        assert!((m.bounding_sheet_area_mm2 - 4000.0).abs() < 1e-2);
        assert!((m.material_utilization_pct - 100.0).abs() < 1e-2);
        assert!(m.estimated_laser_time_sec > 0.0);
        assert!(m.estimated_vinyl_time_sec > 0.0);
    }
}
