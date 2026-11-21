// SPDX-License-Identifier: MPL-2.0

//! Print Preflight Inspector: detects RGB-in-CMYK color mismatches, low-DPI raster
//! placements, live non-curved text, hairline outlines on non-cut layers, missing bleed
//! coverage, locked/hidden/non-printable layers, and excessive CMYK Total Area Coverage (TAC).

use crate::prepress::{PRINT_MARKS_LAYER_NAME, TILE_MARKS_LAYER_NAME};
use crate::scene_helpers::{world_bounds_with_xf, MAX_SCENE_DEPTH};
use coree_core::{
    Affine2D, Color, ColorMode, Document, Layer, LayerId, NodePayload, ObjectId, Page, PageId,
    Rect2D, SceneObject,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Write as _;

/// Severity classification for a preflight diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PreflightSeverity {
    Info,
    Warning,
    Error,
}

/// Specific category of preflight finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreflightIssueKind {
    RgbInCmykDocument,
    LowResolutionImage,
    LiveTextNotCurves,
    HairlineOutlineOnNonCutLayer,
    InsufficientBleedCoverage,
    HiddenOrNonPrintableLayerWithObjects,
    LockedLayerWithObjects,
    TotalInkCoverageExceeded,
    MissingRasterAsset,
    EmptyDocument,
}

/// A single actionable finding produced by [`run_preflight`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreflightIssue {
    pub severity: PreflightSeverity,
    pub kind: PreflightIssueKind,
    pub page_id: Option<PageId>,
    pub layer_id: Option<LayerId>,
    pub object_id: Option<ObjectId>,
    pub message: String,
    pub suggestion: String,
    /// Optional numeric measurement (e.g. effective DPI, stroke width in mm, or TAC %).
    pub measured_value: Option<f64>,
}

/// Configurable rule thresholds for [`run_preflight`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreflightRules {
    /// Flag RGB/HSL colors and RGB rasters when `doc.color_mode == ColorMode::Cmyk`.
    pub check_rgb_in_cmyk: bool,
    /// Minimum acceptable effective image resolution in DPI (default: `300.0`).
    pub min_dpi: f64,
    /// Emit a print-shop warning for live text objects not yet converted to curves.
    pub warn_live_text: bool,
    /// Minimum safe outline thickness in mm on non-cut layers (default: `0.1`).
    pub min_outline_width_mm: f64,
    /// Check objects near/crossing trim edges for full bleed coverage.
    pub check_bleed: bool,
    /// Distance inside the trim edge (mm) within which an artwork edge is assumed intended to bleed.
    pub bleed_edge_tolerance_mm: f64,
    /// Report locked, hidden, or non-printable layers that still contain objects.
    pub check_layer_states: bool,
    /// Maximum allowed CMYK Total Area Coverage (`C+M+Y+K` in %, default: `300.0`).
    pub max_tac_pct: f64,
}

impl Default for PreflightRules {
    fn default() -> Self {
        Self {
            check_rgb_in_cmyk: true,
            min_dpi: 300.0,
            warn_live_text: true,
            min_outline_width_mm: 0.1,
            check_bleed: true,
            bleed_edge_tolerance_mm: 1.0,
            check_layer_states: true,
            max_tac_pct: 300.0,
        }
    }
}

/// Complete preflight inspection report for a [`Document`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreflightReport {
    pub document_title: String,
    pub color_mode: ColorMode,
    pub page_count: usize,
    pub object_count: usize,
    pub issues: Vec<PreflightIssue>,
}

impl PreflightReport {
    /// Number of [`PreflightSeverity::Error`] issues.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == PreflightSeverity::Error)
            .count()
    }

    /// Number of [`PreflightSeverity::Warning`] issues.
    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == PreflightSeverity::Warning)
            .count()
    }

    /// Number of [`PreflightSeverity::Info`] issues.
    #[must_use]
    pub fn info_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == PreflightSeverity::Info)
            .count()
    }

    /// Returns `true` if any error-level issue was found.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
    }

    /// Returns `true` if there are zero error-level issues.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.has_errors()
    }

    /// Returns `true` if the report contains an issue of `kind`.
    #[must_use]
    pub fn has_issue_kind(&self, kind: PreflightIssueKind) -> bool {
        self.issues.iter().any(|i| i.kind == kind)
    }

    /// Renders a clean, print-shop-ready plain-text preflight report (`preflight.txt`).
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "=== COREE PREFLIGHT REPORT ===");
        let _ = writeln!(out, "Document : {}", self.document_title);
        let _ = writeln!(out, "Mode     : {:?}", self.color_mode);
        let _ = writeln!(out, "Pages    : {}", self.page_count);
        let _ = writeln!(out, "Objects  : {}", self.object_count);
        let _ = writeln!(
            out,
            "Summary  : {} Error(s), {} Warning(s), {} Info",
            self.error_count(),
            self.warning_count(),
            self.info_count()
        );
        let _ = writeln!(
            out,
            "Status   : {}",
            if self.passed() { "PASS" } else { "FAIL" }
        );
        let _ = writeln!(out);

        if self.issues.is_empty() {
            let _ = writeln!(
                out,
                "No preflight issues detected. Document is press-ready."
            );
        } else {
            for (idx, issue) in self.issues.iter().enumerate() {
                let sev = match issue.severity {
                    PreflightSeverity::Error => "ERROR",
                    PreflightSeverity::Warning => "WARN ",
                    PreflightSeverity::Info => "INFO ",
                };
                let loc = format_location(issue.page_id, issue.layer_id, issue.object_id);
                let _ = writeln!(
                    out,
                    "{:>3}. [{sev}] {:?} ({loc}): {}",
                    idx + 1,
                    issue.kind,
                    issue.message
                );
                let _ = writeln!(out, "     Fix: {}", issue.suggestion);
            }
        }
        out
    }
}

fn format_location(
    page_id: Option<PageId>,
    layer_id: Option<LayerId>,
    object_id: Option<ObjectId>,
) -> String {
    let mut parts = Vec::new();
    if let Some(p) = page_id {
        parts.push(p.to_string());
    }
    if let Some(l) = layer_id {
        parts.push(l.to_string());
    }
    if let Some(o) = object_id {
        parts.push(o.to_string());
    }
    if parts.is_empty() {
        "document".to_owned()
    } else {
        parts.join(" / ")
    }
}

/// Runs a comprehensive print-production preflight inspection across all pages and layers.
#[must_use]
pub fn run_preflight(doc: &Document, rules: &PreflightRules) -> PreflightReport {
    let defaults = PreflightRules::default();
    let safe_rules = PreflightRules {
        min_dpi: if rules.min_dpi.is_finite() && rules.min_dpi >= 0.0 {
            rules.min_dpi
        } else {
            defaults.min_dpi
        },
        min_outline_width_mm: if rules.min_outline_width_mm.is_finite()
            && rules.min_outline_width_mm >= 0.0
        {
            rules.min_outline_width_mm
        } else {
            defaults.min_outline_width_mm
        },
        bleed_edge_tolerance_mm: if rules.bleed_edge_tolerance_mm.is_finite()
            && rules.bleed_edge_tolerance_mm >= 0.0
        {
            rules.bleed_edge_tolerance_mm
        } else {
            defaults.bleed_edge_tolerance_mm
        },
        max_tac_pct: if rules.max_tac_pct.is_finite() && rules.max_tac_pct >= 0.0 {
            rules.max_tac_pct
        } else {
            defaults.max_tac_pct
        },
        ..rules.clone()
    };

    let mut issues = Vec::new();

    if doc.pages.is_empty() {
        issues.push(PreflightIssue {
            severity: PreflightSeverity::Error,
            kind: PreflightIssueKind::EmptyDocument,
            page_id: None,
            layer_id: None,
            object_id: None,
            message: "Document contains no pages".into(),
            suggestion: "Add at least one page before exporting for print.".into(),
            measured_value: None,
        });
    }

    for page in &doc.pages {
        for layer in &page.layers {
            check_layer(doc, page, layer, &safe_rules, &mut issues);
        }
    }

    PreflightReport {
        document_title: doc.title.clone(),
        color_mode: doc.color_mode,
        page_count: doc.pages.len(),
        object_count: doc.objects.len(),
        issues,
    }
}

fn is_marks_layer(layer: &Layer) -> bool {
    layer.name.eq_ignore_ascii_case(PRINT_MARKS_LAYER_NAME)
        || layer.name.eq_ignore_ascii_case(TILE_MARKS_LAYER_NAME)
}

fn check_layer(
    doc: &Document,
    page: &Page,
    layer: &Layer,
    rules: &PreflightRules,
    issues: &mut Vec<PreflightIssue>,
) {
    if rules.check_layer_states && !layer.objects.is_empty() {
        if !layer.visible || !layer.printable {
            let state = match (layer.visible, layer.printable) {
                (false, false) => "hidden and non-printable",
                (false, true) => "hidden",
                (true, false) => "non-printable",
                (true, true) => "",
            };
            issues.push(PreflightIssue {
                severity: PreflightSeverity::Warning,
                kind: PreflightIssueKind::HiddenOrNonPrintableLayerWithObjects,
                page_id: Some(page.id),
                layer_id: Some(layer.id),
                object_id: None,
                message: format!(
                    "Layer '{}' is {state} but contains {} object(s)",
                    layer.name,
                    layer.objects.len()
                ),
                suggestion:
                    "Enable visibility/printability if artwork should print, or remove unused objects."
                        .into(),
                measured_value: Some(layer.objects.len() as f64),
            });
        }
        if layer.locked {
            issues.push(PreflightIssue {
                severity: PreflightSeverity::Info,
                kind: PreflightIssueKind::LockedLayerWithObjects,
                page_id: Some(page.id),
                layer_id: Some(layer.id),
                object_id: None,
                message: format!(
                    "Layer '{}' is locked and contains {} object(s)",
                    layer.name,
                    layer.objects.len()
                ),
                suggestion: "Verify locked layer contents are final before plate output.".into(),
                measured_value: Some(layer.objects.len() as f64),
            });
        }
    }

    let marks = is_marks_layer(layer);
    let mut visiting = HashSet::new();
    for &oid in &layer.objects {
        check_object_recursive(
            doc,
            page,
            layer,
            oid,
            Affine2D::IDENTITY,
            None,
            marks,
            rules,
            0,
            &mut visiting,
            issues,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn check_object_recursive(
    doc: &Document,
    page: &Page,
    layer: &Layer,
    oid: ObjectId,
    parent_xf: Affine2D,
    clip_bounds: Option<Rect2D>,
    is_marks: bool,
    rules: &PreflightRules,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    issues: &mut Vec<PreflightIssue>,
) {
    if depth > MAX_SCENE_DEPTH || !visiting.insert(oid) {
        return;
    }
    if let Some(obj) = doc.objects.get(&oid) {
        let world_xf = parent_xf.compose(&obj.transform);
        check_object(
            doc,
            page,
            layer,
            oid,
            obj,
            world_xf,
            clip_bounds,
            is_marks,
            rules,
            issues,
        );
        match &obj.payload {
            NodePayload::Group(children) => {
                for &cid in children {
                    check_object_recursive(
                        doc,
                        page,
                        layer,
                        cid,
                        world_xf,
                        clip_bounds,
                        is_marks,
                        rules,
                        depth + 1,
                        visiting,
                        issues,
                    );
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
            } => {
                let container_bb = container.transformed(&world_xf).bounding_box();
                let child_clip = match (clip_bounds, container_bb) {
                    (Some(a), Some(b)) => a.intersection(b),
                    (None, Some(b)) => Some(b),
                    (Some(a), None) => Some(a),
                    (None, None) => None,
                };
                for &cid in contents {
                    check_object_recursive(
                        doc,
                        page,
                        layer,
                        cid,
                        world_xf,
                        child_clip,
                        is_marks,
                        rules,
                        depth + 1,
                        visiting,
                        issues,
                    );
                }
            }
            _ => {}
        }
    }
    visiting.remove(&oid);
}

#[allow(clippy::too_many_arguments)]
fn check_object(
    doc: &Document,
    page: &Page,
    layer: &Layer,
    oid: ObjectId,
    obj: &SceneObject,
    world_xf: Affine2D,
    clip_bounds: Option<Rect2D>,
    is_marks: bool,
    rules: &PreflightRules,
    issues: &mut Vec<PreflightIssue>,
) {
    // 1. Collect all vector/text colors referenced directly by this object
    let mut colors: Vec<(&str, &Color)> = Vec::new();
    match &obj.fill {
        coree_core::Fill::None
        | coree_core::Fill::VectorPattern { .. }
        | coree_core::Fill::BitmapPattern { .. } => {}
        coree_core::Fill::Solid(c) => colors.push(("fill", c)),
        coree_core::Fill::LinearGradient { stops, .. }
        | coree_core::Fill::RadialGradient { stops, .. }
        | coree_core::Fill::ConicalGradient { stops, .. }
        | coree_core::Fill::SquareGradient { stops, .. } => {
            for s in stops {
                colors.push(("gradient fill", &s.color));
            }
        }
        coree_core::Fill::TwoColorPattern { fg, bg, .. }
        | coree_core::Fill::TextureFill { fg, bg, .. } => {
            colors.push(("pattern foreground", fg));
            colors.push(("pattern background", bg));
        }
        coree_core::Fill::MeshFill { nodes, .. } => {
            for n in nodes {
                colors.push(("mesh node", &n.color));
            }
        }
    }
    if obj.outline.is_visible() {
        colors.push(("outline", &obj.outline.color));
    }
    match &obj.payload {
        NodePayload::ArtisticText(t) => {
            for run in &t.runs {
                colors.push(("text fill", &run.style.fill));
            }
        }
        NodePayload::ParagraphText(t) => {
            for run in &t.runs {
                colors.push(("text fill", &run.style.fill));
            }
        }
        NodePayload::TextOnPath(top) => {
            for run in &top.text.runs {
                colors.push(("text fill", &run.style.fill));
            }
        }
        _ => {}
    }

    // 1a. RGB inside CMYK document check
    if rules.check_rgb_in_cmyk && doc.color_mode == ColorMode::Cmyk && !is_marks {
        let mut flagged_rgb = false;
        for (source, color) in &colors {
            if is_screen_rgb_color(color) && !flagged_rgb {
                flagged_rgb = true;
                issues.push(PreflightIssue {
                    severity: PreflightSeverity::Error,
                    kind: PreflightIssueKind::RgbInCmykDocument,
                    page_id: Some(page.id),
                    layer_id: Some(layer.id),
                    object_id: Some(oid),
                    message: format!(
                        "Object '{}' uses RGB/HSL color in {source} inside a CMYK document",
                        obj.name
                    ),
                    suggestion: "Convert object color to CMYK or a named Spot swatch.".into(),
                    measured_value: None,
                });
            }
        }
    }

    // 1b. Total Ink Coverage (TAC > max_tac_pct) check
    if !is_marks && !layer.is_cut_layer {
        let mut worst_tac = 0.0_f64;
        for &(_, color) in &colors {
            if let Color::Cmyk { c, m, y, k, a } = *color {
                if a.is_finite()
                    && a > 1e-4
                    && c.is_finite()
                    && m.is_finite()
                    && y.is_finite()
                    && k.is_finite()
                {
                    let tac = f64::from(c + m + y + k) * 100.0;
                    if tac.is_finite() && tac > worst_tac {
                        worst_tac = tac;
                    }
                }
            }
        }
        if worst_tac > rules.max_tac_pct + 1e-4 {
            issues.push(PreflightIssue {
                severity: PreflightSeverity::Warning,
                kind: PreflightIssueKind::TotalInkCoverageExceeded,
                page_id: Some(page.id),
                layer_id: Some(layer.id),
                object_id: Some(oid),
                message: format!(
                    "Object '{}' has CMYK Total Ink Coverage of {worst_tac:.1}% (limit {:.1}%)",
                    obj.name, rules.max_tac_pct
                ),
                suggestion:
                    "Reduce ink sum (e.g. rich black C40 M30 Y30 K100 = 200%) to prevent smearing."
                        .into(),
                measured_value: Some(worst_tac),
            });
        }
    }

    // 2. Raster image checks (missing asset, effective DPI, RGB raster in CMYK doc)
    if let NodePayload::Raster(raster) = &obj.payload {
        match doc.assets.get(&raster.asset_id) {
            None => {
                issues.push(PreflightIssue {
                    severity: PreflightSeverity::Error,
                    kind: PreflightIssueKind::MissingRasterAsset,
                    page_id: Some(page.id),
                    layer_id: Some(layer.id),
                    object_id: Some(oid),
                    message: format!(
                        "Raster object '{}' references missing asset {}",
                        obj.name, raster.asset_id
                    ),
                    suggestion: "Relink or re-embed the missing raster image.".into(),
                    measured_value: None,
                });
            }
            Some(asset) => {
                if rules.check_rgb_in_cmyk
                    && doc.color_mode == ColorMode::Cmyk
                    && asset.color_mode == ColorMode::Rgb
                {
                    issues.push(PreflightIssue {
                        severity: PreflightSeverity::Error,
                        kind: PreflightIssueKind::RgbInCmykDocument,
                        page_id: Some(page.id),
                        layer_id: Some(layer.id),
                        object_id: Some(oid),
                        message: format!(
                            "Raster image '{}' is in RGB mode inside a CMYK document",
                            obj.name
                        ),
                        suggestion: "Convert bitmap asset to CMYK before press output.".into(),
                        measured_value: None,
                    });
                }

                if world_xf.is_finite()
                    && raster.width_mm.is_finite()
                    && raster.height_mm.is_finite()
                    && raster.width_mm > 0.0
                    && raster.height_mm > 0.0
                {
                    let dec = world_xf.decompose();
                    let placed_w = (raster.width_mm * dec.scale.x.abs()).max(1e-6);
                    let placed_h = (raster.height_mm * dec.scale.y.abs()).max(1e-6);
                    let eff_dpi_x = f64::from(asset.width) / (placed_w / 25.4);
                    let eff_dpi_y = f64::from(asset.height) / (placed_h / 25.4);
                    let effective_dpi = eff_dpi_x.min(eff_dpi_y);

                    if effective_dpi.is_finite() && effective_dpi + 1e-3 < rules.min_dpi {
                        let sev = if effective_dpi < rules.min_dpi * 0.5 {
                            PreflightSeverity::Error
                        } else {
                            PreflightSeverity::Warning
                        };
                        issues.push(PreflightIssue {
                            severity: sev,
                            kind: PreflightIssueKind::LowResolutionImage,
                            page_id: Some(page.id),
                            layer_id: Some(layer.id),
                            object_id: Some(oid),
                            message: format!(
                                "Image '{}' effective resolution is {effective_dpi:.1} DPI (minimum {:.0} DPI)",
                                obj.name, rules.min_dpi
                            ),
                            suggestion:
                                "Replace with a higher-resolution bitmap or reduce placed dimensions."
                                    .into(),
                            measured_value: Some(effective_dpi),
                        });
                    }
                }
            }
        }
    }

    // 3. Live text not converted to curves
    if rules.warn_live_text
        && !is_marks
        && matches!(
            obj.payload,
            NodePayload::ArtisticText(_)
                | NodePayload::ParagraphText(_)
                | NodePayload::TextOnPath(_)
        )
    {
        issues.push(PreflightIssue {
            severity: PreflightSeverity::Warning,
            kind: PreflightIssueKind::LiveTextNotCurves,
            page_id: Some(page.id),
            layer_id: Some(layer.id),
            object_id: Some(oid),
            message: format!(
                "Live text object '{}' has not been converted to vector curves",
                obj.name
            ),
            suggestion: "Convert text to curves or include fonts via Collect for Output.".into(),
            measured_value: None,
        });
    }

    // 4. Hairline outlines (< min_outline_width_mm) on non-cut layers
    if !layer.is_cut_layer && !is_marks && obj.outline.is_visible() {
        let eff_w = obj.outline.effective_width_mm();
        if obj.outline.is_hairline || eff_w + 1e-6 < rules.min_outline_width_mm {
            issues.push(PreflightIssue {
                severity: PreflightSeverity::Warning,
                kind: PreflightIssueKind::HairlineOutlineOnNonCutLayer,
                page_id: Some(page.id),
                layer_id: Some(layer.id),
                object_id: Some(oid),
                message: format!(
                    "Object '{}' has a hairline/thin outline ({eff_w:.4} mm < {:.2} mm) on non-cut layer '{}'",
                    obj.name, rules.min_outline_width_mm, layer.name
                ),
                suggestion:
                    "Increase outline width to at least 0.1 mm or move cutting paths to a cut layer."
                        .into(),
                measured_value: Some(eff_w),
            });
        }
    }

    // 5. Bleed coverage check (using composed world_xf and clipped by enclosing PowerClip if any)
    if rules.check_bleed
        && !is_marks
        && !layer.is_cut_layer
        && !matches!(obj.payload, NodePayload::Group(_))
    {
        let raw_bb = world_bounds_with_xf(doc, oid, obj, &world_xf);
        let visible_bb = match (raw_bb, clip_bounds) {
            (Some(bb), Some(clip)) => bb.intersection(clip),
            (Some(bb), None) => Some(bb),
            (None, _) => None,
        };
        if let Some(bb) = visible_bb {
            let trim = page.trim_rect();
            let bleed = page.bleed_rect();
            if bb.intersects(trim) {
                let eps = 0.05;
                let tol = rules.bleed_edge_tolerance_mm.max(0.0);
                let lacking_bleed = if page.bleed_mm > 1e-6 {
                    let bad_left = bb.min_x <= trim.min_x + tol && bb.min_x > bleed.min_x + eps;
                    let bad_right = bb.max_x >= trim.max_x - tol && bb.max_x < bleed.max_x - eps;
                    let bad_top = bb.min_y <= trim.min_y + tol && bb.min_y > bleed.min_y + eps;
                    let bad_bottom = bb.max_y >= trim.max_y - tol && bb.max_y < bleed.max_y - eps;
                    bad_left || bad_right || bad_top || bad_bottom
                } else {
                    // Page has 0 bleed configured, yet artwork extends beyond page trim box
                    bb.min_x < trim.min_x - eps
                        || bb.max_x > trim.max_x + eps
                        || bb.min_y < trim.min_y - eps
                        || bb.max_y > trim.max_y + eps
                };

                if lacking_bleed {
                    issues.push(PreflightIssue {
                        severity: PreflightSeverity::Warning,
                        kind: PreflightIssueKind::InsufficientBleedCoverage,
                        page_id: Some(page.id),
                        layer_id: Some(layer.id),
                        object_id: Some(oid),
                        message: format!(
                            "Object '{}' approaches or crosses page trim without full {:.1} mm bleed coverage",
                            obj.name, page.bleed_mm
                        ),
                        suggestion:
                            "Extend bleeding artwork to the full page bleed box (typically 3 mm) or pull safely inside trim."
                                .into(),
                        measured_value: Some(page.bleed_mm),
                    });
                }
            }
        }
    }
}

fn is_screen_rgb_color(color: &Color) -> bool {
    match *color {
        Color::Rgba { a, .. } | Color::Hsl { a, .. } => a > 1e-4,
        Color::Cmyk { .. } | Color::Gray { .. } | Color::Lab { .. } | Color::Spot { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        ArtisticText, CornerStyle, Fill, MasterLayerScope, OutlineStyle, ParametricShape,
        RasterAsset, RasterLayerData, TextStyle,
    };

    fn rect(x: f64, y: f64, w: f64, h: f64) -> SceneObject {
        SceneObject::new(
            ObjectId(0),
            "Box",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_transform(Affine2D::translate(x, y))
    }

    #[test]
    fn preflight_detects_every_issue_class() {
        let mut doc = Document::new_a4("Preflight Stress Test");
        let page_id = doc.pages[0].id;
        doc.pages[0].bleed_mm = 3.0;

        // 1. RGB object inside CMYK document
        let rgb_id = doc
            .add_object(
                rect(20.0, 20.0, 30.0, 30.0)
                    .with_fill(Fill::Solid(Color::rgb(1.0, 0.2, 0.1)))
                    .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();

        // 2. Low-resolution raster image (100px across 25.4mm = 100 DPI < 300 DPI)
        let asset =
            RasterAsset::new(100, 100, vec![128; 100 * 100 * 4], 100.0, ColorMode::Cmyk).unwrap();
        let asset_id = doc.register_asset(asset);
        let img_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "LowRes Photo",
                    NodePayload::Raster(RasterLayerData {
                        asset_id,
                        width_mm: 25.4,
                        height_mm: 25.4,
                        filters: Vec::new(),
                        crop_rect: None,
                    }),
                )
                .with_transform(Affine2D::translate(60.0, 20.0))
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();

        // 3. Live text not converted to curves
        let text_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Headline",
                    NodePayload::ArtisticText(ArtisticText::from_plain(
                        "Print Shop",
                        TextStyle {
                            fill: Color::KEY_BLACK,
                            ..TextStyle::default()
                        },
                    )),
                )
                .with_transform(Affine2D::translate(30.0, 80.0))
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();

        // 4. Hairline outline on a non-cut layer
        let hairline_id = doc
            .add_object(
                rect(30.0, 120.0, 40.0, 20.0)
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::hairline(Color::KEY_BLACK)),
                None,
            )
            .unwrap();

        // 5. High Total Ink Coverage (TAC = 360% > 300%)
        let tac_id = doc
            .add_object(
                rect(90.0, 120.0, 30.0, 30.0)
                    .with_fill(Fill::Solid(Color::cmyk(0.9, 0.9, 0.9, 0.9)))
                    .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();

        // 6. Object bleeding past trim (-1mm) without reaching full -3mm bleed
        let bleed_id = doc
            .add_object(
                rect(-1.0, 160.0, 50.0, 20.0)
                    .with_fill(Fill::Solid(Color::CYAN))
                    .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();

        // 7. Hidden & locked non-printable layer containing an object
        let hidden_layer = doc
            .add_layer(page_id, "Notes", false, MasterLayerScope::None)
            .unwrap();
        doc.add_object(
            rect(40.0, 200.0, 20.0, 10.0)
                .with_fill(Fill::Solid(Color::CYAN))
                .with_outline(OutlineStyle::none()),
            Some(hidden_layer),
        )
        .unwrap();
        doc.set_layer_flags(hidden_layer, Some(false), Some(true), Some(false), None)
            .unwrap();

        // 8. Hairline on a legitimate cut layer must NOT trigger hairline warning!
        let cut_layer = doc
            .add_layer(page_id, "CutContour", true, MasterLayerScope::None)
            .unwrap();
        let cut_obj_id = doc
            .add_object(
                rect(25.0, 25.0, 50.0, 50.0)
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::cut_contour()),
                Some(cut_layer),
            )
            .unwrap();

        let report = run_preflight(&doc, &PreflightRules::default());
        assert!(report.has_errors());
        assert!(!report.passed());

        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::RgbInCmykDocument
                && i.object_id == Some(rgb_id)
                && i.page_id == Some(page_id)));
        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::LowResolutionImage
                && i.object_id == Some(img_id)
                && i.measured_value.unwrap() < 105.0));
        assert!(report.issues.iter().any(
            |i| i.kind == PreflightIssueKind::LiveTextNotCurves && i.object_id == Some(text_id)
        ));
        assert!(report.issues.iter().any(|i| i.kind
            == PreflightIssueKind::HairlineOutlineOnNonCutLayer
            && i.object_id == Some(hairline_id)));
        assert!(!report.issues.iter().any(|i| i.kind
            == PreflightIssueKind::HairlineOutlineOnNonCutLayer
            && i.object_id == Some(cut_obj_id)));
        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::TotalInkCoverageExceeded
                && i.object_id == Some(tac_id)
                && (i.measured_value.unwrap() - 360.0).abs() < 0.5));
        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::InsufficientBleedCoverage
                && i.object_id == Some(bleed_id)));
        assert!(report.issues.iter().any(|i| i.kind
            == PreflightIssueKind::HiddenOrNonPrintableLayerWithObjects
            && i.layer_id == Some(hidden_layer)));
        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::LockedLayerWithObjects
                && i.layer_id == Some(hidden_layer)));

        let txt = report.to_plain_text();
        assert!(txt.contains("COREE PREFLIGHT REPORT"));
        assert!(txt.contains("FAIL"));
    }

    #[test]
    fn clean_cmyk_document_passes_preflight() {
        let mut doc = Document::new_a4("Clean Print Job");
        doc.pages[0].bleed_mm = 3.0;
        // Full bleed background from -3..-3 to 213..300
        doc.add_object(
            rect(-3.0, -3.0, 216.0, 303.0)
                .with_fill(Fill::Solid(Color::cmyk(0.2, 0.1, 0.0, 0.0)))
                .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();
        // Safe interior shape with 0.5mm CMYK outline
        doc.add_object(
            rect(30.0, 30.0, 100.0, 80.0)
                .with_fill(Fill::Solid(Color::cmyk(0.4, 0.3, 0.3, 1.0))) // 200% TAC
                .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.5)),
            None,
        )
        .unwrap();

        let report = run_preflight(&doc, &PreflightRules::default());
        assert!(report.passed(), "unexpected issues: {:?}", report.issues);
        assert_eq!(report.error_count(), 0);
        assert_eq!(report.warning_count(), 0);
    }
}
