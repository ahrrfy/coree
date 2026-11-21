// SPDX-License-Identifier: MPL-2.0

//! Advanced Find & Replace engine across the active page or entire multipage document.
//!
//! Queries by object kind, fill/outline color (with CIE76 Delta-E tolerance), outline width
//! range, hairline flag, font family, text substring / exact / regex-like pattern, lens kind,
//! and layer. Applies batch selection, color replacement, outline width/hairline updates,
//! font family swaps, text replacement, and overprint toggles.

use crate::scene_helpers::visit_layer_objects;
use coree_core::{
    Color, Document, Fill, LayerId, LensKind, NodePayload, ObjectId, SceneObject, TextRun,
    HAIRLINE_WIDTH_MM,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
pub struct ColorMatch {
    pub color: Color,
    /// Maximum allowed CIE76 Delta-E distance (`0.0` matches near-exact `< 0.01`, e.g. `2.5` for close shades).
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
pub enum TextMatchMode {
    Contains {
        case_sensitive: bool,
    },
    Exact {
        case_sensitive: bool,
    },
    /// Lightweight regex-like pattern supporting `^`, `$`, `.`, `*`, `+`, `?`, `[abc]`.
    RegexLike {
        case_sensitive: bool,
    },
}

/// Declarative multi-criteria query (all `Some(...)` fields combine with logical AND).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FindQuery {
    pub scope: FindScope,
    pub object_kind: Option<ObjectKindFilter>,
    pub fill_color: Option<ColorMatch>,
    pub outline_color: Option<ColorMatch>,
    pub outline_width_min_mm: Option<f64>,
    pub outline_width_max_mm: Option<f64>,
    pub is_hairline: Option<bool>,
    pub font_family: Option<String>,
    pub text_pattern: Option<(String, TextMatchMode)>,
    pub lens_kind: Option<LensKindFilter>,
    pub layer_id: Option<LayerId>,
    pub layer_name: Option<String>,
}

/// Action to execute on every object matching a [`FindQuery`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Evaluates `query` against `doc` and returns all matching [`ObjectId`]s in deterministic
/// page / layer / scene order.
#[must_use]
pub fn find_objects(doc: &Document, query: &FindQuery) -> Vec<ObjectId> {
    let page_indices: Vec<usize> = match query.scope {
        FindScope::CurrentPage => {
            if doc.active_page_index < doc.pages.len() {
                vec![doc.active_page_index]
            } else {
                Vec::new()
            }
        }
        FindScope::Page(idx) => {
            if idx < doc.pages.len() {
                vec![idx]
            } else {
                Vec::new()
            }
        }
        FindScope::WholeDocument => (0..doc.pages.len()).collect(),
    };

    let mut matched = Vec::new();
    let mut seen = HashSet::new();

    for p_idx in page_indices {
        let page = &doc.pages[p_idx];
        for layer in &page.layers {
            if let Some(target_lid) = query.layer_id {
                if layer.id != target_lid {
                    continue;
                }
            }
            if let Some(ref target_name) = query.layer_name {
                if !layer.name.eq_ignore_ascii_case(target_name.trim()) {
                    continue;
                }
            }

            visit_layer_objects(doc, layer, &mut |oid, obj, _| {
                if seen.insert(oid) && object_matches(obj, query) {
                    matched.push(oid);
                }
            });
        }
    }

    matched
}

/// Finds all objects matching `query` and applies `action` to them (skipping locked objects
/// for mutating actions).
pub fn find_and_replace(
    doc: &mut Document,
    query: &FindQuery,
    action: &ReplaceAction,
) -> FindReplaceReport {
    let matched_ids = find_objects(doc, query);
    if matches!(action, ReplaceAction::SelectMatches) || matched_ids.is_empty() {
        return FindReplaceReport {
            matched_ids,
            modified_ids: Vec::new(),
            replacements_applied: 0,
        };
    }

    // Collect objects residing on locked layers so batch replacement also respects layer locks
    let mut locked_layer_ids: HashSet<ObjectId> = HashSet::new();
    for page in &doc.pages {
        for layer in &page.layers {
            if layer.locked {
                visit_layer_objects(doc, layer, &mut |oid, _, _| {
                    locked_layer_ids.insert(oid);
                });
            }
        }
    }

    let mut modified_ids = Vec::new();
    let mut replacements_applied = 0usize;

    for &oid in &matched_ids {
        if locked_layer_ids.contains(&oid) {
            continue;
        }
        let Some(obj) = doc.objects.get_mut(&oid) else {
            continue;
        };
        if obj.locked {
            continue;
        }
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

fn object_matches(obj: &SceneObject, query: &FindQuery) -> bool {
    if let Some(kind_filter) = query.object_kind {
        if !matches_object_kind(&obj.payload, kind_filter) {
            return false;
        }
    }

    if let Some(ref fill_match) = query.fill_color {
        let mut fill_colors = obj.fill.referenced_colors();
        // Also check text run fills when fill is on text runs
        for run in text_runs_ref(&obj.payload) {
            fill_colors.push(run.style.fill.clone());
        }
        if !fill_colors.iter().any(|c| fill_match.matches(c)) {
            return false;
        }
    }

    if let Some(ref outline_match) = query.outline_color {
        if !obj.outline.is_visible() || !outline_match.matches(&obj.outline.color) {
            return false;
        }
    }

    if let Some(min_w) = query.outline_width_min_mm.filter(|v| v.is_finite()) {
        if !obj.outline.is_visible() || obj.outline.effective_width_mm() + 1e-9 < min_w {
            return false;
        }
    }
    if let Some(max_w) = query.outline_width_max_mm.filter(|v| v.is_finite()) {
        if !obj.outline.is_visible() || obj.outline.effective_width_mm() - 1e-9 > max_w {
            return false;
        }
    }

    if let Some(want_hairline) = query.is_hairline {
        let is_hl = obj.outline.is_visible() && obj.outline.is_hairline;
        if is_hl != want_hairline {
            return false;
        }
    }

    if let Some(ref family) = query.font_family {
        let runs = text_runs_ref(&obj.payload);
        let target = family.trim();
        if runs.is_empty()
            || !runs
                .iter()
                .any(|r| r.style.font_family.trim().eq_ignore_ascii_case(target))
        {
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

fn matches_object_kind(payload: &NodePayload, filter: ObjectKindFilter) -> bool {
    match filter {
        ObjectKindFilter::Path => matches!(payload, NodePayload::Path(_)),
        ObjectKindFilter::Shape => matches!(payload, NodePayload::Shape(_)),
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

fn text_runs_ref(payload: &NodePayload) -> &[TextRun] {
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

fn extract_searchable_text(payload: &NodePayload) -> Option<String> {
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

fn apply_replace_action(obj: &mut SceneObject, query: &FindQuery, action: &ReplaceAction) -> usize {
    match action {
        ReplaceAction::SelectMatches => 0,
        ReplaceAction::ReplaceFillColor { color } => {
            let mut changes = 0usize;
            let matcher = query.fill_color.as_ref();
            let should_replace = |c: &Color| matcher.is_none_or(|m| m.matches(c));

            match &mut obj.fill {
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

            if let Some(runs) = text_runs_mut(&mut obj.payload) {
                for run in runs {
                    if should_replace(&run.style.fill) {
                        run.style.fill = color.clone();
                        changes += 1;
                    }
                }
            } else if changes == 0 && matches!(obj.fill, Fill::None) && matcher.is_none() {
                obj.fill = Fill::Solid(color.clone());
                changes += 1;
            }
            changes
        }
        ReplaceAction::ReplaceOutlineColor { color } => {
            obj.outline.color = color.clone();
            1
        }
        ReplaceAction::ReplaceOutlineWidth {
            width_mm,
            is_hairline,
        } => {
            obj.outline.is_hairline = *is_hairline;
            obj.outline.width_mm = if *is_hairline {
                HAIRLINE_WIDTH_MM
            } else if width_mm.is_finite() {
                width_mm.max(0.0)
            } else {
                0.0
            };
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
        let lower_hay: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
        let lower_ndl: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
        if lower_ndl.is_empty() {
            return (haystack.to_owned(), 0);
        }
        // Simple character-aligned case-insensitive replacement (safe for ASCII & Unicode)
        let hay_chars: Vec<char> = haystack.chars().collect();
        if hay_chars.len() != lower_hay.len() {
            // Fallback when case folding expands characters (e.g. 'ß' -> "ss")
            let hay_l = haystack.to_lowercase();
            let ndl_l = needle.to_lowercase();
            let count = hay_l.matches(&ndl_l).count();
            return if count == 0 {
                (haystack.to_owned(), 0)
            } else {
                (hay_l.replace(&ndl_l, replacement), count)
            };
        }
        let mut out = String::with_capacity(haystack.len());
        let mut i = 0usize;
        let mut count = 0usize;
        let nlen = lower_ndl.len();
        while i < hay_chars.len() {
            if i + nlen <= hay_chars.len() && lower_hay[i..i + nlen] == lower_ndl[..] {
                out.push_str(replacement);
                i += nlen;
                count += 1;
            } else {
                out.push(hay_chars[i]);
                i += 1;
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
    let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");
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

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        ArtisticText, CornerStyle, LensConfig, OutlineStyle, ParametricShape, TextStyle,
    };

    #[test]
    fn find_and_replace_across_multipage_document() {
        let mut doc = Document::new_a4("Catalog");
        let p2 = doc.add_page("Page 2", 210.0, 297.0, 3.0);
        let p1_layer = doc.pages[0].layers[0].id;
        let p2_layer = doc.pages[1].layers[0].id;
        assert_eq!(doc.pages[1].id, p2);

        // Page 1: blue rect + red-ish rect + Arial text + lens rect
        let r1 = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Cyan Rect",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 40.0,
                        height: 20.0,
                        corner_radii: [0.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::Solid(Color::rgb(0.95, 0.1, 0.1)))
                .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.25)),
                Some(p1_layer),
            )
            .unwrap();

        let t1 = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Title",
                    NodePayload::ArtisticText(ArtisticText::from_plain(
                        "Catalog 2025 Edition",
                        TextStyle {
                            font_family: "Arial".to_owned(),
                            fill: Color::KEY_BLACK,
                            ..TextStyle::default()
                        },
                    )),
                ),
                Some(p1_layer),
            )
            .unwrap();

        // Page 2: slightly different red (within Delta-E 5) + Invert lens + Arial text
        let mut r2_obj = SceneObject::new(
            ObjectId(0),
            "Near Red Lens",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 30.0,
                height: 30.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.93, 0.12, 0.11)))
        .with_outline(OutlineStyle::hairline(Color::KEY_BLACK));
        r2_obj.lens = Some(LensConfig::new(LensKind::Invert));
        let r2 = doc.add_object(r2_obj, Some(p2_layer)).unwrap();

        let t2 = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Footer",
                    NodePayload::ArtisticText(ArtisticText::from_plain(
                        "Copyright 2025 coree",
                        TextStyle {
                            font_family: "Arial".to_owned(),
                            fill: Color::KEY_BLACK,
                            ..TextStyle::default()
                        },
                    )),
                ),
                Some(p2_layer),
            )
            .unwrap();

        // 1. Delta-E color find & replace across both pages
        let red_query = FindQuery {
            scope: FindScope::WholeDocument,
            fill_color: Some(ColorMatch::within_delta_e(Color::rgb(0.95, 0.1, 0.1), 5.0)),
            ..FindQuery::default()
        };
        let rep_color = find_and_replace(
            &mut doc,
            &red_query,
            &ReplaceAction::ReplaceFillColor {
                color: Color::cmyk(0.0, 1.0, 1.0, 0.0),
            },
        );
        assert_eq!(rep_color.matched_ids, vec![r1, r2]);
        assert_eq!(rep_color.modified_ids, vec![r1, r2]);
        assert_eq!(
            doc.objects[&r1].fill,
            Fill::Solid(Color::cmyk(0.0, 1.0, 1.0, 0.0))
        );
        assert_eq!(
            doc.objects[&r2].fill,
            Fill::Solid(Color::cmyk(0.0, 1.0, 1.0, 0.0))
        );

        // 2. Font family swap across whole document
        let font_query = FindQuery {
            scope: FindScope::WholeDocument,
            font_family: Some("Arial".to_owned()),
            ..FindQuery::default()
        };
        let rep_font = find_and_replace(
            &mut doc,
            &font_query,
            &ReplaceAction::ReplaceFontFamily {
                font_family: "Cairo".to_owned(),
            },
        );
        assert_eq!(rep_font.matched_ids, vec![t1, t2]);
        let NodePayload::ArtisticText(ref at1) = doc.objects[&t1].payload else {
            panic!();
        };
        assert_eq!(at1.runs[0].style.font_family, "Cairo");

        // 3. Regex-like text search + replace "2025" -> "2026"
        let text_query = FindQuery {
            scope: FindScope::WholeDocument,
            text_pattern: Some((
                r"20\d\d".to_owned(),
                TextMatchMode::RegexLike {
                    case_sensitive: true,
                },
            )),
            ..FindQuery::default()
        };
        let rep_text = find_and_replace(
            &mut doc,
            &text_query,
            &ReplaceAction::ReplaceText {
                find: "2025".to_owned(),
                replace_with: "2026".to_owned(),
                case_sensitive: true,
            },
        );
        assert_eq!(rep_text.matched_ids, vec![t1, t2]);
        let NodePayload::ArtisticText(ref at2) = doc.objects[&t2].payload else {
            panic!();
        };
        assert_eq!(at2.plain_text(), "Copyright 2026 coree");

        // 4. Hairline + Lens query + ConvertOverprint + ReplaceOutlineWidth
        let lens_hairline_q = FindQuery {
            scope: FindScope::WholeDocument,
            is_hairline: Some(true),
            lens_kind: Some(LensKindFilter::Invert),
            ..FindQuery::default()
        };
        let rep_ov = find_and_replace(
            &mut doc,
            &lens_hairline_q,
            &ReplaceAction::ConvertOverprint { overprint: true },
        );
        assert_eq!(rep_ov.matched_ids, vec![r2]);
        assert!(doc.objects[&r2].outline.overprint);

        let rep_w = find_and_replace(
            &mut doc,
            &lens_hairline_q,
            &ReplaceAction::ReplaceOutlineWidth {
                width_mm: 0.5,
                is_hairline: false,
            },
        );
        assert_eq!(rep_w.modified_ids, vec![r2]);
        assert!(!doc.objects[&r2].outline.is_hairline);
        assert!((doc.objects[&r2].outline.width_mm - 0.5).abs() < 1e-9);
    }
}
