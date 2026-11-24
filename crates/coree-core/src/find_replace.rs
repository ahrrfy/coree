// SPDX-License-Identifier: MIT OR Apache-2.0

//! Advanced Find & Replace engine across the active page or entire multipage document.
//!
//! Queries by object kind, fill/outline color (with CIE76 Delta-E tolerance), outline width
//! range, hairline flag, font family, text substring / exact / regex-like pattern, lens kind,
//! and layer. Applies batch selection, color replacement, outline width/hairline updates,
//! font family swaps, text replacement, and overprint toggles.

use crate::color::Color;
use crate::document::{Document, Layer, NodePayload, SceneObject};
use crate::ids::{LayerId, ObjectId};
use crate::lens::LensKind;
use crate::math::Affine2D;
use crate::style::{Fill, OutlineStyle, HAIRLINE_WIDTH_MM};
use crate::text::TextRun;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_SCENE_DEPTH: usize = 64;

/// Document search scope for [`FindQuery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FindScope {
    /// Search only `doc.active_page_index`.
    CurrentPage,
    /// Search a specific 0-based page index.
    Page(usize),
    /// Search every page in the document.
    #[default]
    WholeDocument,
}

/// Filter matching [`NodePayload`] variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectKindFilter {
    Path,
    Shape,
    BSpline,
    ArtisticMedia,
    Clone,
    ArtisticText,
    ParagraphText,
    TextOnPath,
    /// Matches any of `ArtisticText`, `ParagraphText`, or `TextOnPath`.
    AnyText,
    Raster,
    Table,
    Dimension,
    Connector,
    Group,
    PowerClip,
    SymbolInstance,
}

/// Filter matching [`LensKind`] discriminants on `SceneObject::lens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LensKindFilter {
    AnyLens,
    Brighten,
    ColorAdd,
    ColorLimit,
    CustomColorMap,
    FishEye,
    HeatMap,
    Invert,
    Magnify,
    TintedGrayscale,
    Transparency,
    Wireframe,
}

/// Color matching criterion with perceptual CIE76 Delta-E tolerance (`0.0` = exact).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorMatch {
    pub color: Color,
    /// Maximum allowed CIE76 Delta-E distance (`0.0` matches near-exact `< 0.01`, e.g. `2.5` for close shades).
    #[serde(default)]
    pub delta_e_tolerance: f32,
}

impl ColorMatch {
    /// Creates an exact color match (`delta_e_tolerance = 0.0`).
    #[must_use]
    pub fn exact(color: Color) -> Self {
        Self {
            color,
            delta_e_tolerance: 0.0,
        }
    }

    /// Creates a perceptual color match with the given CIE76 Delta-E threshold.
    #[must_use]
    pub fn within_delta_e(color: Color, delta_e_tolerance: f32) -> Self {
        let safe = if delta_e_tolerance.is_finite() {
            delta_e_tolerance.max(0.0)
        } else {
            0.0
        };
        Self {
            color,
            delta_e_tolerance: safe,
        }
    }

    /// Returns `true` if `candidate` matches `self`.
    ///
    /// Under exact matching (`delta_e_tolerance <= 1e-4`), named `Color::Spot` inks only match
    /// another `Color::Spot` with the same name and never cross-match process CMYK/RGB colors
    /// that happen to share their visual fallback.
    #[must_use]
    pub fn matches(&self, candidate: &Color) -> bool {
        let tol = if self.delta_e_tolerance.is_finite() {
            self.delta_e_tolerance.max(0.0)
        } else {
            0.0
        };
        if tol <= 1e-4 {
            return match (&self.color, candidate) {
                (Color::Spot { name: a, .. }, Color::Spot { name: b, .. }) => {
                    a.eq_ignore_ascii_case(b)
                }
                (Color::Spot { .. }, _) | (_, Color::Spot { .. }) => false,
                _ => self.color.delta_e76(candidate) <= 0.01,
            };
        }
        self.color.delta_e76(candidate) <= tol
    }
}

/// Text matching semantics for [`FindQuery::text_pattern`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum TextMatchMode {
    Contains {
        #[serde(default)]
        case_sensitive: bool,
    },
    Exact {
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Lightweight regex-like pattern supporting `^`, `$`, `.`, `*`, `+`, `?`, `[abc]`.
    RegexLike {
        #[serde(default)]
        case_sensitive: bool,
    },
}

/// Declarative multi-criteria query (all `Some(...)` fields combine with logical AND).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindQuery {
    #[serde(default)]
    pub scope: FindScope,
    #[serde(default)]
    pub object_kind: Option<ObjectKindFilter>,
    #[serde(default)]
    pub fill_color: Option<ColorMatch>,
    #[serde(default)]
    pub outline_color: Option<ColorMatch>,
    #[serde(default)]
    pub outline_width_min_mm: Option<f64>,
    #[serde(default)]
    pub outline_width_max_mm: Option<f64>,
    #[serde(default)]
    pub is_hairline: Option<bool>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub text_pattern: Option<(String, TextMatchMode)>,
    #[serde(default)]
    pub lens_kind: Option<LensKindFilter>,
    #[serde(default)]
    pub layer_id: Option<LayerId>,
    #[serde(default)]
    pub layer_name: Option<String>,
}

/// Action to execute on every object matching a [`FindQuery`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ReplaceAction {
    /// Non-destructive query: returns matching IDs without mutating the document.
    SelectMatches,
    /// Replaces solid/gradient/text fill color on matching objects.
    ReplaceFillColor { color: Color },
    /// Replaces outline stroke color on matching objects.
    ReplaceOutlineColor { color: Color },
    /// Updates outline width and hairline flag on matching objects.
    ReplaceOutlineWidth { width_mm: f64, is_hairline: bool },
    /// Swaps the font family on matching text runs.
    ReplaceFontFamily { font_family: String },
    /// Replaces text occurrences inside matching text/table objects.
    ReplaceText {
        find: String,
        replace_with: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Sets `outline.overprint` on matching objects.
    ConvertOverprint { overprint: bool },
}

/// Summary returned by [`find_and_replace`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FindReplaceReport {
    pub matched_ids: Vec<ObjectId>,
    pub modified_ids: Vec<ObjectId>,
    pub replacements_applied: usize,
}

/// Returns the list of page indices targeted by `scope` within `doc`.
#[must_use]
pub fn scoped_page_indices(doc: &Document, scope: FindScope) -> Vec<usize> {
    let pages = doc.pages();
    match scope {
        FindScope::CurrentPage => {
            let active = doc.active_page_index();
            if active < pages.len() {
                vec![active]
            } else {
                Vec::new()
            }
        }
        FindScope::Page(idx) => {
            if idx < pages.len() {
                vec![idx]
            } else {
                Vec::new()
            }
        }
        FindScope::WholeDocument => (0..pages.len()).collect(),
    }
}

fn visit_layer_objects<F>(doc: &Document, layer: &Layer, visitor: &mut F)
where
    F: FnMut(ObjectId, &SceneObject, Affine2D),
{
    let mut visiting = HashSet::new();
    for &oid in &layer.objects {
        visit_object_recursive(doc, oid, Affine2D::IDENTITY, 0, &mut visiting, visitor);
    }
}

fn visit_object_recursive<F>(
    doc: &Document,
    id: ObjectId,
    parent_xf: Affine2D,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    visitor: &mut F,
) where
    F: FnMut(ObjectId, &SceneObject, Affine2D),
{
    if depth > MAX_SCENE_DEPTH || !visiting.insert(id) {
        return;
    }
    if let Some(obj) = doc.object(id) {
        let world_xf = parent_xf.compose(&obj.transform);
        visitor(id, obj, world_xf);
        match &obj.payload {
            NodePayload::Group(children) => {
                for &cid in children {
                    visit_object_recursive(doc, cid, world_xf, depth + 1, visiting, visitor);
                }
            }
            NodePayload::PowerClip { contents, .. } => {
                for &cid in contents {
                    visit_object_recursive(doc, cid, world_xf, depth + 1, visiting, visitor);
                }
            }
            _ => {}
        }
    }
    visiting.remove(&id);
}

/// Evaluates `query` against `doc` and returns all matching [`ObjectId`]s in deterministic
/// page / layer / scene order.
#[must_use]
pub fn find_objects(doc: &Document, query: &FindQuery) -> Vec<ObjectId> {
    let page_indices = scoped_page_indices(doc, query.scope);
    let pages = doc.pages();

    let mut matched = Vec::new();
    let mut seen = HashSet::new();

    for p_idx in page_indices {
        let mut scan_layer = |layer: &Layer| {
            if let Some(target_lid) = query.layer_id {
                if layer.id != target_lid {
                    return;
                }
            }
            if let Some(ref target_name) = query.layer_name {
                if !layer.name.eq_ignore_ascii_case(target_name.trim()) {
                    return;
                }
            }

            visit_layer_objects(doc, layer, &mut |oid, obj, _| {
                if seen.insert(oid) && object_matches(obj, query) {
                    matched.push(oid);
                }
            });
        };

        for layer in &pages[p_idx].layers {
            if Document::layer_applies_to_page(layer, p_idx, p_idx) {
                scan_layer(layer);
            }
        }
        for (owner_idx, owner_page) in pages.iter().enumerate() {
            if owner_idx == p_idx {
                continue;
            }
            for layer in &owner_page.layers {
                if Document::layer_applies_to_page(layer, owner_idx, p_idx) {
                    scan_layer(layer);
                }
            }
        }
    }

    matched
}

/// Finds all objects matching `query` and applies `action` to them (skipping effectively locked
/// objects, objects on locked layers, and descendants inside locked `Group`/`PowerClip` containers
/// for mutating actions).
pub fn find_and_replace(
    doc: &mut Document,
    query: &FindQuery,
    action: &ReplaceAction,
) -> FindReplaceReport {
    let matched_ids = find_objects(doc, query);
    if matches!(action, ReplaceAction::SelectMatches)
        || matched_ids.is_empty()
        || !replace_action_is_valid(action)
    {
        return FindReplaceReport {
            matched_ids,
            modified_ids: Vec::new(),
            replacements_applied: 0,
        };
    }

    let effective_locked = doc.effective_locked_object_ids();

    let mut modified_ids = Vec::new();
    let mut replacements_applied = 0usize;

    for &oid in &matched_ids {
        if effective_locked.contains(&oid) {
            continue;
        }
        let Some(obj) = doc.object_mut(oid) else {
            continue;
        };
        let changed = apply_replace_action(obj, query, action);
        if changed > 0 {
            modified_ids.push(oid);
            replacements_applied += changed;
        }
    }

    if !modified_ids.is_empty() {
        doc.touch();
    }

    FindReplaceReport {
        matched_ids,
        modified_ids,
        replacements_applied,
    }
}

/// Computes the exact [`FindReplaceReport`] that [`find_and_replace`] would produce without
/// mutating `doc` or cloning the entire [`Document`] (or its [`crate::RasterAsset`] blobs).
#[must_use]
pub fn preview_find_and_replace(
    doc: &Document,
    query: &FindQuery,
    action: &ReplaceAction,
) -> FindReplaceReport {
    let matched_ids = find_objects(doc, query);
    if matches!(action, ReplaceAction::SelectMatches)
        || matched_ids.is_empty()
        || !replace_action_is_valid(action)
    {
        return FindReplaceReport {
            matched_ids,
            modified_ids: Vec::new(),
            replacements_applied: 0,
        };
    }

    let effective_locked = doc.effective_locked_object_ids();
    let mut modified_ids = Vec::new();
    let mut replacements_applied = 0usize;

    for &oid in &matched_ids {
        if effective_locked.contains(&oid) {
            continue;
        }
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        let mut scratch = obj.clone();
        let changed = apply_replace_action(&mut scratch, query, action);
        if changed > 0 {
            modified_ids.push(oid);
            replacements_applied += changed;
        }
    }

    FindReplaceReport {
        matched_ids,
        modified_ids,
        replacements_applied,
    }
}

fn color_channels_finite(c: &Color) -> bool {
    match c {
        Color::Rgba { r, g, b, a } => {
            r.is_finite() && g.is_finite() && b.is_finite() && a.is_finite()
        }
        Color::Cmyk { c, m, y, k, a } => {
            c.is_finite() && m.is_finite() && y.is_finite() && k.is_finite() && a.is_finite()
        }
        Color::Hsl { h, s, l, a } => {
            h.is_finite() && s.is_finite() && l.is_finite() && a.is_finite()
        }
        Color::Lab {
            l,
            a_ch,
            b_ch,
            alpha,
        } => l.is_finite() && a_ch.is_finite() && b_ch.is_finite() && alpha.is_finite(),
        Color::Gray { v, a } => v.is_finite() && a.is_finite(),
        Color::Spot {
            tint,
            fallback_cmyk,
            alpha,
            ..
        } => tint.is_finite() && fallback_cmyk.iter().all(|v| v.is_finite()) && alpha.is_finite(),
    }
}

fn replace_action_is_valid(action: &ReplaceAction) -> bool {
    match action {
        ReplaceAction::ReplaceFillColor { color }
        | ReplaceAction::ReplaceOutlineColor { color } => color_channels_finite(color),
        ReplaceAction::ReplaceOutlineWidth {
            width_mm,
            is_hairline,
        } => width_mm.is_finite() && (*is_hairline || *width_mm >= 0.0),
        _ => true,
    }
}

fn object_matches(obj: &SceneObject, query: &FindQuery) -> bool {
    if let Some(kind_filter) = query.object_kind {
        if !matches_object_kind(&obj.payload, kind_filter) {
            return false;
        }
    }

    if let Some(ref fill_match) = query.fill_color {
        let mut fill_colors = obj.fill.referenced_colors();
        for run in text_runs_ref(&obj.payload) {
            fill_colors.push(run.style.fill.clone());
        }
        if let NodePayload::Table(ref table) = obj.payload {
            for cell in &table.cells {
                fill_colors.extend(cell.fill.referenced_colors());
                if !cell.text.is_empty() {
                    fill_colors.push(cell.text_style.fill.clone());
                }
            }
        }
        if !fill_colors.iter().any(|c| fill_match.matches(c)) {
            return false;
        }
    }

    let outline = effective_outline(obj);

    if let Some(ref outline_match) = query.outline_color {
        if !outline.is_visible() || !outline_match.matches(&outline.color) {
            return false;
        }
    }

    if let Some(min_w) = query.outline_width_min_mm.filter(|v| v.is_finite()) {
        if !outline.is_visible() || outline.effective_width_mm() + 1e-9 < min_w {
            return false;
        }
    }
    if let Some(max_w) = query.outline_width_max_mm.filter(|v| v.is_finite()) {
        if !outline.is_visible() || outline.effective_width_mm() - 1e-9 > max_w {
            return false;
        }
    }

    if let Some(want_hairline) = query.is_hairline {
        let is_hl = outline.is_visible() && outline.is_hairline;
        if is_hl != want_hairline {
            return false;
        }
    }

    if let Some(ref family) = query.font_family {
        let target = family.trim();
        let runs_match = text_runs_ref(&obj.payload)
            .iter()
            .any(|r| r.style.font_family.trim().eq_ignore_ascii_case(target));
        let table_match = matches!(
            &obj.payload,
            NodePayload::Table(table)
                if table.cells.iter().any(|c| {
                    !c.text.is_empty() && c.text_style.font_family.trim().eq_ignore_ascii_case(target)
                })
        );
        if !runs_match && !table_match {
            return false;
        }
    }

    if let Some((ref pattern, mode)) = query.text_pattern {
        let Some(text) = extract_searchable_text(&obj.payload) else {
            return false;
        };
        if !matches_text_pattern(&text, pattern, mode) {
            return false;
        }
    }

    if let Some(lens_filter) = query.lens_kind {
        let Some(ref cfg) = obj.lens else {
            return false;
        };
        if !matches_lens_kind(&cfg.kind, lens_filter) {
            return false;
        }
    }

    true
}

#[inline]
fn effective_outline(obj: &SceneObject) -> &OutlineStyle {
    match &obj.payload {
        NodePayload::Table(tbl) => &tbl.border_style,
        _ => &obj.outline,
    }
}

fn matches_object_kind(payload: &NodePayload, filter: ObjectKindFilter) -> bool {
    match filter {
        ObjectKindFilter::Path => matches!(payload, NodePayload::Path(_)),
        ObjectKindFilter::Shape => matches!(payload, NodePayload::Shape(_)),
        ObjectKindFilter::BSpline => matches!(payload, NodePayload::BSpline(_)),
        ObjectKindFilter::ArtisticMedia => matches!(payload, NodePayload::ArtisticMedia { .. }),
        ObjectKindFilter::Clone => matches!(payload, NodePayload::Clone { .. }),
        ObjectKindFilter::ArtisticText => matches!(payload, NodePayload::ArtisticText(_)),
        ObjectKindFilter::ParagraphText => matches!(payload, NodePayload::ParagraphText(_)),
        ObjectKindFilter::TextOnPath => matches!(payload, NodePayload::TextOnPath(_)),
        ObjectKindFilter::AnyText => matches!(
            payload,
            NodePayload::ArtisticText(_)
                | NodePayload::ParagraphText(_)
                | NodePayload::TextOnPath(_)
        ),
        ObjectKindFilter::Raster => matches!(payload, NodePayload::Raster(_)),
        ObjectKindFilter::Table => matches!(payload, NodePayload::Table(_)),
        ObjectKindFilter::Dimension => matches!(payload, NodePayload::Dimension(_)),
        ObjectKindFilter::Connector => matches!(payload, NodePayload::Connector(_)),
        ObjectKindFilter::Group => matches!(payload, NodePayload::Group(_)),
        ObjectKindFilter::PowerClip => matches!(payload, NodePayload::PowerClip { .. }),
        ObjectKindFilter::SymbolInstance => matches!(payload, NodePayload::SymbolInstance(_)),
    }
}

fn matches_lens_kind(kind: &LensKind, filter: LensKindFilter) -> bool {
    match filter {
        LensKindFilter::AnyLens => true,
        LensKindFilter::Brighten => matches!(kind, LensKind::Brighten { .. }),
        LensKindFilter::ColorAdd => matches!(kind, LensKind::ColorAdd { .. }),
        LensKindFilter::ColorLimit => matches!(kind, LensKind::ColorLimit { .. }),
        LensKindFilter::CustomColorMap => matches!(kind, LensKind::CustomColorMap { .. }),
        LensKindFilter::FishEye => matches!(kind, LensKind::FishEye { .. }),
        LensKindFilter::HeatMap => matches!(kind, LensKind::HeatMap { .. }),
        LensKindFilter::Invert => matches!(kind, LensKind::Invert),
        LensKindFilter::Magnify => matches!(kind, LensKind::Magnify { .. }),
        LensKindFilter::TintedGrayscale => matches!(kind, LensKind::TintedGrayscale { .. }),
        LensKindFilter::Transparency => matches!(kind, LensKind::Transparency { .. }),
        LensKindFilter::Wireframe => matches!(kind, LensKind::Wireframe { .. }),
    }
}

#[must_use]
pub fn text_runs_ref(payload: &NodePayload) -> &[TextRun] {
    match payload {
        NodePayload::ArtisticText(t) => &t.runs,
        NodePayload::ParagraphText(t) => &t.runs,
        NodePayload::TextOnPath(top) => &top.text.runs,
        _ => &[],
    }
}

fn text_runs_mut(payload: &mut NodePayload) -> Option<&mut [TextRun]> {
    match payload {
        NodePayload::ArtisticText(t) => Some(&mut t.runs),
        NodePayload::ParagraphText(t) => Some(&mut t.runs),
        NodePayload::TextOnPath(top) => Some(&mut top.text.runs),
        _ => None,
    }
}

#[must_use]
pub fn extract_searchable_text(payload: &NodePayload) -> Option<String> {
    match payload {
        NodePayload::ArtisticText(t) => Some(t.plain_text()),
        NodePayload::ParagraphText(t) => Some(t.plain_text()),
        NodePayload::TextOnPath(top) => Some(top.text.plain_text()),
        NodePayload::Table(table) => Some(
            table
                .cells
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

fn replace_fill_colors(fill: &mut Fill, color: &Color, matcher: Option<&ColorMatch>) -> usize {
    let should_replace = |c: &Color| matcher.is_none_or(|m| m.matches(c));
    let mut changes = 0usize;
    match fill {
        Fill::Solid(c) if should_replace(c) => {
            *c = color.clone();
            changes += 1;
        }
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => {
            for stop in stops {
                if should_replace(&stop.color) {
                    stop.color = color.clone();
                    changes += 1;
                }
            }
        }
        Fill::TwoColorPattern { fg, bg, .. } | Fill::TextureFill { fg, bg, .. } => {
            if should_replace(fg) {
                *fg = color.clone();
                changes += 1;
            }
            if should_replace(bg) {
                *bg = color.clone();
                changes += 1;
            }
        }
        Fill::MeshFill { nodes, .. } => {
            for n in nodes {
                if should_replace(&n.color) {
                    n.color = color.clone();
                    changes += 1;
                }
            }
        }
        _ => {}
    }
    changes
}

fn apply_replace_action(obj: &mut SceneObject, query: &FindQuery, action: &ReplaceAction) -> usize {
    match action {
        ReplaceAction::SelectMatches => 0,
        ReplaceAction::ReplaceFillColor { color } => {
            let matcher = query.fill_color.as_ref();
            let mut changes = replace_fill_colors(&mut obj.fill, color, matcher);

            if let Some(runs) = text_runs_mut(&mut obj.payload) {
                for run in runs {
                    if matcher.is_none_or(|m| m.matches(&run.style.fill)) {
                        run.style.fill = color.clone();
                        changes += 1;
                    }
                }
            } else if let NodePayload::Table(ref mut table) = obj.payload {
                for cell in &mut table.cells {
                    changes += replace_fill_colors(&mut cell.fill, color, matcher);
                    if !cell.text.is_empty()
                        && matcher.is_none_or(|m| m.matches(&cell.text_style.fill))
                    {
                        cell.text_style.fill = color.clone();
                        changes += 1;
                    }
                }
                if changes == 0 && matches!(obj.fill, Fill::None) && matcher.is_none() {
                    obj.fill = Fill::Solid(color.clone());
                    changes += 1;
                }
            } else if changes == 0 && matches!(obj.fill, Fill::None) && matcher.is_none() {
                obj.fill = Fill::Solid(color.clone());
                changes += 1;
            }
            changes
        }
        ReplaceAction::ReplaceOutlineColor { color } => {
            obj.outline.color = color.clone();
            if let NodePayload::Table(ref mut tbl) = obj.payload {
                tbl.border_style.color = color.clone();
            }
            1
        }
        ReplaceAction::ReplaceOutlineWidth {
            width_mm,
            is_hairline,
        } => {
            let resolved_width = if *is_hairline {
                HAIRLINE_WIDTH_MM
            } else if width_mm.is_finite() {
                width_mm.max(0.0)
            } else {
                0.0
            };
            obj.outline.is_hairline = *is_hairline;
            obj.outline.width_mm = resolved_width;
            if let NodePayload::Table(ref mut tbl) = obj.payload {
                tbl.border_style.is_hairline = *is_hairline;
                tbl.border_style.width_mm = resolved_width;
            }
            1
        }
        ReplaceAction::ReplaceFontFamily { font_family } => {
            let mut changes = 0usize;
            if let Some(runs) = text_runs_mut(&mut obj.payload) {
                for run in runs {
                    let matches_filter = query.font_family.as_ref().is_none_or(|f| {
                        run.style.font_family.trim().eq_ignore_ascii_case(f.trim())
                    });
                    if matches_filter {
                        run.style.font_family = font_family.clone();
                        changes += 1;
                    }
                }
            } else if let NodePayload::Table(ref mut table) = obj.payload {
                for cell in &mut table.cells {
                    if !cell.text.is_empty() {
                        let matches_filter = query.font_family.as_ref().is_none_or(|f| {
                            cell.text_style
                                .font_family
                                .trim()
                                .eq_ignore_ascii_case(f.trim())
                        });
                        if matches_filter {
                            cell.text_style.font_family = font_family.clone();
                            changes += 1;
                        }
                    }
                }
            }
            changes
        }
        ReplaceAction::ReplaceText {
            find,
            replace_with,
            case_sensitive,
        } => {
            if find.is_empty() {
                return 0;
            }
            let mut changes = 0usize;
            if let Some(runs) = text_runs_mut(&mut obj.payload) {
                for run in runs {
                    let (updated, count) =
                        replace_substring(&run.text, find, replace_with, *case_sensitive);
                    if count > 0 {
                        run.text = updated;
                        changes += count;
                    }
                }
            } else if let NodePayload::Table(table) = &mut obj.payload {
                for cell in &mut table.cells {
                    let (updated, count) =
                        replace_substring(&cell.text, find, replace_with, *case_sensitive);
                    if count > 0 {
                        cell.text = updated;
                        changes += count;
                    }
                }
            }
            changes
        }
        ReplaceAction::ConvertOverprint { overprint } => {
            obj.outline.overprint = *overprint;
            if let NodePayload::Table(ref mut tbl) = obj.payload {
                tbl.border_style.overprint = *overprint;
            }
            1
        }
    }
}

fn replace_substring(
    haystack: &str,
    needle: &str,
    replacement: &str,
    case_sensitive: bool,
) -> (String, usize) {
    if case_sensitive {
        let count = haystack.matches(needle).count();
        if count == 0 {
            (haystack.to_owned(), 0)
        } else {
            (haystack.replace(needle, replacement), count)
        }
    } else {
        let lower_ndl: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
        if lower_ndl.is_empty() {
            return (haystack.to_owned(), 0);
        }
        let hay_chars: Vec<char> = haystack.chars().collect();
        let mut lower_hay: Vec<char> = Vec::with_capacity(hay_chars.len());
        let mut lower_to_orig_char: Vec<usize> = Vec::with_capacity(hay_chars.len());
        let mut orig_char_lower_start: Vec<usize> = Vec::with_capacity(hay_chars.len() + 1);
        for (orig_idx, &ch) in hay_chars.iter().enumerate() {
            orig_char_lower_start.push(lower_hay.len());
            for lc in ch.to_lowercase() {
                lower_hay.push(lc);
                lower_to_orig_char.push(orig_idx);
            }
        }
        orig_char_lower_start.push(lower_hay.len());

        let mut out = String::with_capacity(haystack.len());
        let mut orig_i = 0usize;
        let mut count = 0usize;
        let nlen = lower_ndl.len();
        while orig_i < hay_chars.len() {
            let li = orig_char_lower_start[orig_i];
            let end_li = li + nlen;
            let at_end_boundary = end_li == lower_hay.len()
                || (end_li < lower_hay.len()
                    && lower_to_orig_char[end_li] != lower_to_orig_char[end_li - 1]);
            if end_li <= lower_hay.len()
                && at_end_boundary
                && lower_hay[li..end_li] == lower_ndl[..]
            {
                out.push_str(replacement);
                count += 1;
                orig_i = lower_to_orig_char[end_li - 1] + 1;
            } else {
                out.push(hay_chars[orig_i]);
                orig_i += 1;
            }
        }
        (out, count)
    }
}

// --- Pure-Rust bounded regex-like pattern matcher (no ReDoS, zero external crates) ---

#[derive(Debug, Clone)]
enum Token {
    Literal(char),
    Any,
    /// Inclusive `(lo, hi)` Unicode scalar ranges plus negation flag (O(1) memory even for `[\u{0}-\u{10FFFF}]`).
    Class(Vec<(char, char)>, bool),
}

#[derive(Debug, Clone)]
struct Atom {
    token: Token,
    min: usize,
    max: Option<usize>,
}

fn matches_text_pattern(text: &str, pattern: &str, mode: TextMatchMode) -> bool {
    match mode {
        TextMatchMode::Contains { case_sensitive } => {
            if case_sensitive {
                text.contains(pattern)
            } else {
                text.to_lowercase().contains(&pattern.to_lowercase())
            }
        }
        TextMatchMode::Exact { case_sensitive } => {
            if case_sensitive {
                text == pattern
            } else {
                text.to_lowercase() == pattern.to_lowercase()
            }
        }
        TextMatchMode::RegexLike { case_sensitive } => {
            let t_str = if case_sensitive {
                text.to_owned()
            } else {
                text.to_lowercase()
            };
            let p_str = if case_sensitive {
                pattern.to_owned()
            } else {
                pattern.to_lowercase()
            };
            regex_like_is_match(&t_str, &p_str)
        }
    }
}

fn regex_like_is_match(text: &str, pattern: &str) -> bool {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern
        .strip_suffix('$')
        .is_some_and(|prefix| prefix.chars().rev().take_while(|&c| c == '\\').count() % 2 == 0);
    let mut pat_slice = pattern;
    if anchored_start {
        pat_slice = &pat_slice[1..];
    }
    if anchored_end && !pat_slice.is_empty() {
        pat_slice = &pat_slice[..pat_slice.len() - 1];
    }
    let atoms = compile_atoms(pat_slice);
    let chars: Vec<char> = text.chars().collect();
    let max_start = if anchored_start { 0 } else { chars.len() };
    for start in 0..=max_start {
        let mut memo = HashSet::new();
        if match_atoms(&chars, start, &atoms, 0, anchored_end, &mut memo) {
            return true;
        }
    }
    false
}

fn compile_atoms(pattern: &str) -> Vec<Atom> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    let mut atoms = Vec::new();
    while i < chars.len() {
        let token = match chars[i] {
            '\\' if i + 1 < chars.len() => {
                i += 1;
                match chars[i] {
                    'd' => Token::Class(vec![('0', '9')], false),
                    c => Token::Literal(c),
                }
            }
            '.' => Token::Any,
            '[' => {
                i += 1;
                let negated = i < chars.len() && chars[i] == '^';
                if negated {
                    i += 1;
                }
                let mut ranges = Vec::new();
                while i < chars.len() && chars[i] != ']' {
                    if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != ']' {
                        let a = chars[i];
                        let b = chars[i + 2];
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        ranges.push((lo, hi));
                        i += 3;
                    } else {
                        ranges.push((chars[i], chars[i]));
                        i += 1;
                    }
                }
                Token::Class(ranges, negated)
            }
            c => Token::Literal(c),
        };
        i += 1;
        let (min, max) = if i < chars.len() {
            match chars[i] {
                '*' => {
                    i += 1;
                    (0, None)
                }
                '+' => {
                    i += 1;
                    (1, None)
                }
                '?' => {
                    i += 1;
                    (0, Some(1))
                }
                _ => (1, Some(1)),
            }
        } else {
            (1, Some(1))
        };
        atoms.push(Atom { token, min, max });
    }
    atoms
}

fn token_matches(token: &Token, ch: char) -> bool {
    match token {
        Token::Literal(c) => *c == ch,
        Token::Any => ch != '\n',
        Token::Class(ranges, negated) => {
            ranges.iter().any(|&(lo, hi)| ch >= lo && ch <= hi) != *negated
        }
    }
}

fn match_atoms(
    chars: &[char],
    pos: usize,
    atoms: &[Atom],
    ai: usize,
    anchored_end: bool,
    memo: &mut HashSet<(usize, usize)>,
) -> bool {
    if !memo.insert((pos, ai)) {
        return false;
    }
    if ai == atoms.len() {
        return !anchored_end || pos == chars.len();
    }
    let atom = &atoms[ai];
    let mut max_rep = 0usize;
    let limit = atom.max.unwrap_or(chars.len().saturating_sub(pos));
    while max_rep < limit
        && pos + max_rep < chars.len()
        && token_matches(&atom.token, chars[pos + max_rep])
    {
        max_rep += 1;
    }
    if max_rep < atom.min {
        return false;
    }
    for rep in (atom.min..=max_rep).rev() {
        if match_atoms(chars, pos + rep, atoms, ai + 1, anchored_end, memo) {
            return true;
        }
    }
    false
}
