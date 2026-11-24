// SPDX-License-Identifier: MIT OR Apache-2.0

//! Advanced Find & Replace engine across the active page or entire multipage document.
//!
//! Queries by object kind, fill/outline color (with CIE76 Delta-E tolerance), outline width
//! range, hairline flag, font family, text substring / exact / regex-like pattern, lens kind,
//! and layer. Applies batch selection, color replacement, outline width/hairline updates,
//! font family swaps, text replacement, and overprint toggles.

use crate::scene_helpers::visit_layer_objects;
use coree_core::{
    extract_searchable_text, scoped_page_indices, text_runs_ref, Color, Document, Fill,
    NodePayload, ObjectId, SceneObject,
};
pub use coree_core::{
    find_and_replace, find_objects, ColorMatch, FindQuery, FindReplaceReport, FindScope,
    LensKindFilter, ObjectKindFilter, ReplaceAction, TextMatchMode,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// =============================================================================
// Clean-Room Query Language (`coree-cql`)
// =============================================================================

const MAX_CQL_DEPTH: usize = 64;

/// Binary comparison operator supported by the clean-room CQL query engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CqlCompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
}

/// Literal operand in a CQL predicate expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CqlLiteral {
    Str(String),
    Number { raw: f64, mm: Option<f64> },
    Bool(bool),
}

/// Parsed abstract syntax tree for clean-room CQL expressions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CqlExpr {
    Predicate {
        field: String,
        op: CqlCompareOp,
        value: CqlLiteral,
    },
    And(Box<CqlExpr>, Box<CqlExpr>),
    Or(Box<CqlExpr>, Box<CqlExpr>),
    Not(Box<CqlExpr>),
}

/// Parses a CQL query expression such as
/// `@type == 'curve' and (@outline.hairline == true or @outline.width <= 0.5pt)`.
pub fn parse_cql_query(input: &str) -> crate::error::ProdResult<CqlExpr> {
    let tokens = tokenize_cql(input)?;
    if tokens.is_empty() {
        return Err(crate::error::ProdError::InvalidParameter(
            "empty CQL query".into(),
        ));
    }
    let mut parser = CqlParser {
        tokens: &tokens,
        pos: 0,
    };
    let expr = parser.parse_or(0)?;
    if parser.pos < parser.tokens.len() {
        return Err(crate::error::ProdError::InvalidParameter(format!(
            "unexpected trailing CQL token {:?}",
            parser.tokens[parser.pos]
        )));
    }
    Ok(expr)
}

/// Evaluates a parsed [`CqlExpr`] against a single [`SceneObject`].
#[must_use]
pub fn cql_matches_object(obj: &SceneObject, expr: &CqlExpr) -> bool {
    match expr {
        CqlExpr::And(a, b) => cql_matches_object(obj, a) && cql_matches_object(obj, b),
        CqlExpr::Or(a, b) => cql_matches_object(obj, a) || cql_matches_object(obj, b),
        CqlExpr::Not(inner) => !cql_matches_object(obj, inner),
        CqlExpr::Predicate { field, op, value } => evaluate_cql_predicate(obj, field, *op, value),
    }
}

/// Returns all [`ObjectId`]s within `scope` satisfying the compiled [`CqlExpr`].
#[must_use]
pub fn find_objects_cql(doc: &Document, scope: FindScope, expr: &CqlExpr) -> Vec<ObjectId> {
    let page_indices = scoped_page_indices(doc, scope);
    let pages = doc.pages();
    let mut matched = Vec::new();
    let mut seen = HashSet::new();

    for p_idx in page_indices {
        for layer in &pages[p_idx].layers {
            if Document::layer_applies_to_page(layer, p_idx, p_idx) {
                visit_layer_objects(doc, layer, &mut |oid, obj, _| {
                    if seen.insert(oid) && cql_matches_object(obj, expr) {
                        matched.push(oid);
                    }
                });
            }
        }
        for (owner_idx, owner_page) in pages.iter().enumerate() {
            if owner_idx == p_idx {
                continue;
            }
            for layer in &owner_page.layers {
                if Document::layer_applies_to_page(layer, owner_idx, p_idx) {
                    visit_layer_objects(doc, layer, &mut |oid, obj, _| {
                        if seen.insert(oid) && cql_matches_object(obj, expr) {
                            matched.push(oid);
                        }
                    });
                }
            }
        }
    }
    matched
}

/// Parses and evaluates a CQL string query over `doc` inside `scope`.
pub fn evaluate_cql_query(
    doc: &Document,
    scope: FindScope,
    query: &str,
) -> crate::error::ProdResult<Vec<ObjectId>> {
    let expr = parse_cql_query(query)?;
    Ok(find_objects_cql(doc, scope, &expr))
}

#[derive(Debug, Clone, PartialEq)]
enum CqlToken {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Op(CqlCompareOp),
    String(String),
    Number { raw: f64, mm: Option<f64> },
    Ident(String),
}

fn tokenize_cql(input: &str) -> crate::error::ProdResult<Vec<CqlToken>> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        match ch {
            '(' => {
                out.push(CqlToken::LParen);
                i += 1;
            }
            ')' => {
                out.push(CqlToken::RParen);
                i += 1;
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    i += 2;
                } else {
                    i += 1;
                }
                out.push(CqlToken::Op(CqlCompareOp::Eq));
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    out.push(CqlToken::Op(CqlCompareOp::Ne));
                    i += 2;
                } else {
                    out.push(CqlToken::Not);
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    out.push(CqlToken::Op(CqlCompareOp::Le));
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    out.push(CqlToken::Op(CqlCompareOp::Ne));
                    i += 2;
                } else {
                    out.push(CqlToken::Op(CqlCompareOp::Lt));
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    out.push(CqlToken::Op(CqlCompareOp::Ge));
                    i += 2;
                } else {
                    out.push(CqlToken::Op(CqlCompareOp::Gt));
                    i += 1;
                }
            }
            '&' if i + 1 < chars.len() && chars[i + 1] == '&' => {
                out.push(CqlToken::And);
                i += 2;
            }
            '|' if i + 1 < chars.len() && chars[i + 1] == '|' => {
                out.push(CqlToken::Or);
                i += 2;
            }
            '\'' | '"' => {
                let quote = ch;
                i += 1;
                let mut buf = String::new();
                let mut closed = false;
                while i < chars.len() {
                    let c = chars[i];
                    if c == '\\' && i + 1 < chars.len() {
                        buf.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if c == quote {
                        closed = true;
                        i += 1;
                        break;
                    }
                    buf.push(c);
                    i += 1;
                }
                if !closed {
                    return Err(crate::error::ProdError::InvalidParameter(
                        "unterminated string literal in CQL query".into(),
                    ));
                }
                out.push(CqlToken::String(buf));
            }
            '-' | '+' | '0'..='9' => {
                let start = i;
                if ch == '-' || ch == '+' {
                    i += 1;
                }
                let mut saw_digit = false;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    if chars[i].is_ascii_digit() {
                        saw_digit = true;
                    }
                    i += 1;
                }
                if !saw_digit {
                    return Err(crate::error::ProdError::InvalidParameter(
                        "invalid numeric token in CQL query".into(),
                    ));
                }
                let num_str: String = chars[start..i].iter().collect();
                let raw: f64 = num_str.parse().map_err(|_| {
                    crate::error::ProdError::InvalidParameter(format!(
                        "invalid number '{num_str}' in CQL query"
                    ))
                })?;
                // Optional unit suffix: mm, cm, in, inch, pt, px
                let u_start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let unit: String = chars[u_start..i]
                    .iter()
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                let mm = if unit.is_empty() {
                    None
                } else {
                    let scale = match unit.as_str() {
                        "mm" => 1.0,
                        "cm" => 10.0,
                        "in" | "inch" => 25.4,
                        "pt" => 25.4 / 72.0,
                        "px" => 25.4 / 96.0,
                        other => {
                            return Err(crate::error::ProdError::InvalidParameter(format!(
                                "unknown unit suffix '{other}' in CQL query"
                            )))
                        }
                    };
                    Some(raw * scale)
                };
                out.push(CqlToken::Number { raw, mm });
            }
            '.' if chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) => {
                let start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                let raw: f64 = num_str.parse().map_err(|_| {
                    crate::error::ProdError::InvalidParameter(format!(
                        "invalid number '{num_str}' in CQL query"
                    ))
                })?;
                let u_start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let unit: String = chars[u_start..i]
                    .iter()
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                let mm = if unit.is_empty() {
                    None
                } else {
                    let scale = match unit.as_str() {
                        "mm" => 1.0,
                        "cm" => 10.0,
                        "in" | "inch" => 25.4,
                        "pt" => 25.4 / 72.0,
                        "px" => 25.4 / 96.0,
                        other => {
                            return Err(crate::error::ProdError::InvalidParameter(format!(
                                "unknown unit suffix '{other}' in CQL query"
                            )))
                        }
                    };
                    Some(raw * scale)
                };
                out.push(CqlToken::Number { raw, mm });
            }
            c if c == '@' || c == '_' || c == '.' || c.is_alphanumeric() => {
                let start = i;
                while i < chars.len()
                    && (chars[i] == '@'
                        || chars[i] == '_'
                        || chars[i] == '.'
                        || chars[i] == '-'
                        || chars[i].is_alphanumeric())
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.to_ascii_lowercase().as_str() {
                    "and" => out.push(CqlToken::And),
                    "or" => out.push(CqlToken::Or),
                    "not" => out.push(CqlToken::Not),
                    "contains" => out.push(CqlToken::Op(CqlCompareOp::Contains)),
                    _ => out.push(CqlToken::Ident(word)),
                }
            }
            other => {
                return Err(crate::error::ProdError::InvalidParameter(format!(
                    "unexpected character '{other}' in CQL query"
                )))
            }
        }
    }
    Ok(out)
}

struct CqlParser<'a> {
    tokens: &'a [CqlToken],
    pos: usize,
}

impl<'a> CqlParser<'a> {
    fn peek(&self) -> Option<&CqlToken> {
        self.tokens.get(self.pos)
    }

    fn parse_or(&mut self, depth: usize) -> crate::error::ProdResult<CqlExpr> {
        if depth > MAX_CQL_DEPTH {
            return Err(crate::error::ProdError::InvalidParameter(
                "CQL nesting exceeds maximum depth".into(),
            ));
        }
        let mut lhs = self.parse_and(depth + 1)?;
        while matches!(self.peek(), Some(CqlToken::Or)) {
            self.pos += 1;
            let rhs = self.parse_and(depth + 1)?;
            lhs = CqlExpr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self, depth: usize) -> crate::error::ProdResult<CqlExpr> {
        let mut lhs = self.parse_not(depth + 1)?;
        while matches!(self.peek(), Some(CqlToken::And)) {
            self.pos += 1;
            let rhs = self.parse_not(depth + 1)?;
            lhs = CqlExpr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_not(&mut self, depth: usize) -> crate::error::ProdResult<CqlExpr> {
        if depth > MAX_CQL_DEPTH {
            return Err(crate::error::ProdError::InvalidParameter(
                "CQL nesting exceeds maximum depth".into(),
            ));
        }
        if matches!(self.peek(), Some(CqlToken::Not)) {
            self.pos += 1;
            let inner = self.parse_not(depth + 1)?;
            return Ok(CqlExpr::Not(Box::new(inner)));
        }
        self.parse_primary(depth + 1)
    }

    fn parse_primary(&mut self, depth: usize) -> crate::error::ProdResult<CqlExpr> {
        match self.peek().cloned() {
            Some(CqlToken::LParen) => {
                self.pos += 1;
                let expr = self.parse_or(depth + 1)?;
                if !matches!(self.peek(), Some(CqlToken::RParen)) {
                    return Err(crate::error::ProdError::InvalidParameter(
                        "missing closing ')' in CQL query".into(),
                    ));
                }
                self.pos += 1;
                Ok(expr)
            }
            Some(CqlToken::Ident(raw_field)) => {
                self.pos += 1;
                let field = raw_field.trim_start_matches('@').to_ascii_lowercase();
                if let Some(CqlToken::Op(op)) = self.peek().cloned() {
                    self.pos += 1;
                    let value = match self.peek().cloned() {
                        Some(CqlToken::String(s)) => {
                            self.pos += 1;
                            CqlLiteral::Str(s)
                        }
                        Some(CqlToken::Number { raw, mm }) => {
                            self.pos += 1;
                            CqlLiteral::Number { raw, mm }
                        }
                        Some(CqlToken::Ident(word)) => {
                            self.pos += 1;
                            if word.eq_ignore_ascii_case("true") {
                                CqlLiteral::Bool(true)
                            } else if word.eq_ignore_ascii_case("false") {
                                CqlLiteral::Bool(false)
                            } else {
                                CqlLiteral::Str(word)
                            }
                        }
                        other => {
                            return Err(crate::error::ProdError::InvalidParameter(format!(
                                "expected CQL literal value, got {other:?}"
                            )))
                        }
                    };
                    Ok(CqlExpr::Predicate { field, op, value })
                } else {
                    // Unary property flag shorthand e.g. `@outline.hairline` or `@locked`
                    Ok(CqlExpr::Predicate {
                        field,
                        op: CqlCompareOp::Eq,
                        value: CqlLiteral::Bool(true),
                    })
                }
            }
            other => Err(crate::error::ProdError::InvalidParameter(format!(
                "unexpected CQL token {other:?}"
            ))),
        }
    }
}

fn cql_color_model(c: &Color) -> &'static str {
    match c {
        Color::Rgba { .. } => "rgb",
        Color::Cmyk { .. } => "cmyk",
        Color::Hsl { .. } => "hsl",
        Color::Lab { .. } => "lab",
        Color::Gray { .. } => "gray",
        Color::Spot { .. } => "spot",
    }
}

fn cql_color_matches_text(c: &Color, needle: &str, op: CqlCompareOp) -> bool {
    let model = cql_color_model(c);
    let [r, g, b, _] = c.to_rgba8();
    let hex = format!("#{r:02x}{g:02x}{b:02x}");
    let spot_name = match c {
        Color::Spot { name, .. } => name.as_str(),
        _ => "",
    };
    let n_low = needle.trim().to_ascii_lowercase();
    match op {
        CqlCompareOp::Eq => {
            model == n_low
                || (!spot_name.is_empty() && spot_name.eq_ignore_ascii_case(needle.trim()))
                || hex == n_low
        }
        CqlCompareOp::Contains => {
            model.contains(&n_low)
                || spot_name.to_lowercase().contains(&needle.to_lowercase())
                || hex.contains(&n_low)
        }
        _ => false,
    }
}

fn cql_type_aliases(payload: &NodePayload) -> &'static [&'static str] {
    match payload {
        NodePayload::Path(_) => &["curve", "path", "bezier"],
        NodePayload::Shape(shape) => match shape {
            coree_core::ParametricShape::Rectangle { .. } => &["rectangle", "shape"],
            coree_core::ParametricShape::Ellipse { .. } => &["ellipse", "shape"],
            coree_core::ParametricShape::Polygon { .. } => &["polygon", "shape"],
            coree_core::ParametricShape::Star { .. } => &["star", "shape"],
            coree_core::ParametricShape::Spiral { .. } => &["spiral", "shape"],
            _ => &["shape"],
        },
        NodePayload::ArtisticText(_) => &["artistictext", "text", "artistic"],
        NodePayload::ParagraphText(_) => &["paragraphtext", "text", "paragraph"],
        NodePayload::TextOnPath(_) => &["textonpath", "text"],
        NodePayload::Raster(_) => &["bitmap", "raster", "image"],
        NodePayload::Table(_) => &["table"],
        NodePayload::Dimension(_) => &["dimension"],
        NodePayload::Connector(_) => &["connector"],
        NodePayload::Group(_) => &["group"],
        NodePayload::PowerClip { .. } => &["powerclip"],
        NodePayload::SymbolInstance(_) => &["symbol", "symbolinstance"],
    }
}

fn cql_fill_type_aliases(fill: &Fill) -> &'static [&'static str] {
    match fill {
        Fill::None => &["none"],
        Fill::Solid(_) => &["solid", "uniform"],
        Fill::LinearGradient { .. } => &["linear", "fountain", "gradient"],
        Fill::RadialGradient { .. } => &["radial", "fountain", "gradient"],
        Fill::ConicalGradient { .. } => &["conical", "fountain", "gradient"],
        Fill::SquareGradient { .. } => &["square", "fountain", "gradient"],
        Fill::TwoColorPattern { .. } => &["twocolorpattern", "pattern"],
        Fill::VectorPattern { .. } => &["vectorpattern", "pattern"],
        Fill::BitmapPattern { .. } => &["bitmappattern", "pattern"],
        Fill::TextureFill { .. } => &["texture", "texturefill"],
        Fill::MeshFill { .. } => &["mesh", "meshfill"],
    }
}

fn cql_cmp_str(actual: &str, op: CqlCompareOp, lit: &CqlLiteral) -> bool {
    let CqlLiteral::Str(ref s) = lit else {
        return false;
    };
    match op {
        CqlCompareOp::Eq => actual.eq_ignore_ascii_case(s),
        CqlCompareOp::Ne => !actual.eq_ignore_ascii_case(s),
        CqlCompareOp::Contains => actual.to_lowercase().contains(&s.to_lowercase()),
        CqlCompareOp::Lt => actual.to_lowercase() < s.to_lowercase(),
        CqlCompareOp::Le => actual.to_lowercase() <= s.to_lowercase(),
        CqlCompareOp::Gt => actual.to_lowercase() > s.to_lowercase(),
        CqlCompareOp::Ge => actual.to_lowercase() >= s.to_lowercase(),
    }
}

fn cql_cmp_bool(actual: bool, op: CqlCompareOp, lit: &CqlLiteral) -> bool {
    let CqlLiteral::Bool(b) = lit else {
        return false;
    };
    match op {
        CqlCompareOp::Eq => actual == *b,
        CqlCompareOp::Ne => actual != *b,
        _ => false,
    }
}

fn cql_cmp_num(actual: f64, op: CqlCompareOp, lit: &CqlLiteral, is_length_mm: bool) -> bool {
    let CqlLiteral::Number { raw, mm } = lit else {
        return false;
    };
    let target = if is_length_mm {
        mm.unwrap_or(*raw)
    } else {
        *raw
    };
    if !actual.is_finite() || !target.is_finite() {
        return false;
    }
    const EPS: f64 = 1e-6;
    match op {
        CqlCompareOp::Eq => (actual - target).abs() <= EPS,
        CqlCompareOp::Ne => (actual - target).abs() > EPS,
        CqlCompareOp::Lt => actual + EPS < target,
        CqlCompareOp::Le => actual - EPS <= target,
        CqlCompareOp::Gt => actual - EPS > target,
        CqlCompareOp::Ge => actual + EPS >= target,
        CqlCompareOp::Contains => false,
    }
}

#[inline]
fn effective_outline(obj: &SceneObject) -> &coree_core::OutlineStyle {
    match &obj.payload {
        NodePayload::Table(tbl) => &tbl.border_style,
        _ => &obj.outline,
    }
}

fn evaluate_cql_predicate(
    obj: &SceneObject,
    field: &str,
    op: CqlCompareOp,
    value: &CqlLiteral,
) -> bool {
    match field {
        "type" | "kind" => {
            let CqlLiteral::Str(ref s) = value else {
                return false;
            };
            let aliases = cql_type_aliases(&obj.payload);
            match op {
                CqlCompareOp::Eq => aliases.iter().any(|a| a.eq_ignore_ascii_case(s)),
                CqlCompareOp::Ne => !aliases.iter().any(|a| a.eq_ignore_ascii_case(s)),
                CqlCompareOp::Contains => {
                    aliases.iter().any(|a| a.contains(&s.to_ascii_lowercase()))
                }
                _ => false,
            }
        }
        "name" => cql_cmp_str(&obj.name, op, value),
        "visible" => cql_cmp_bool(obj.visible, op, value),
        "locked" => cql_cmp_bool(obj.locked, op, value),
        "printable" => cql_cmp_bool(obj.printable, op, value),
        "has_lens" | "lens" => cql_cmp_bool(obj.lens.is_some(), op, value),
        "opacity" => cql_cmp_num(f64::from(obj.opacity), op, value, false),
        "fill.type" => {
            let CqlLiteral::Str(ref s) = value else {
                return false;
            };
            let mut aliases: Vec<&'static str> = Vec::new();
            if obj.fill.is_visible() {
                aliases.extend_from_slice(cql_fill_type_aliases(&obj.fill));
            }
            for run in text_runs_ref(&obj.payload) {
                if run.style.fill.alpha() > 1e-6 {
                    aliases.extend_from_slice(&["solid", "uniform"]);
                }
            }
            if let NodePayload::Table(ref table) = obj.payload {
                for cell in &table.cells {
                    if cell.fill.is_visible() {
                        aliases.extend_from_slice(cql_fill_type_aliases(&cell.fill));
                    }
                    if !cell.text.is_empty() && cell.text_style.fill.alpha() > 1e-6 {
                        aliases.extend_from_slice(&["solid", "uniform"]);
                    }
                }
            }
            if aliases.is_empty() {
                aliases.push("none");
            }
            match op {
                CqlCompareOp::Eq => aliases.iter().any(|a| a.eq_ignore_ascii_case(s)),
                CqlCompareOp::Ne => !aliases.iter().any(|a| a.eq_ignore_ascii_case(s)),
                CqlCompareOp::Contains => {
                    let needle = s.to_ascii_lowercase();
                    aliases.iter().any(|a| a.contains(&needle))
                }
                _ => false,
            }
        }
        "fill.model" => {
            let mut models: Vec<&'static str> = obj
                .fill
                .referenced_colors()
                .iter()
                .map(cql_color_model)
                .collect();
            for run in text_runs_ref(&obj.payload) {
                models.push(cql_color_model(&run.style.fill));
            }
            if let NodePayload::Table(ref table) = obj.payload {
                for cell in &table.cells {
                    models.extend(cell.fill.referenced_colors().iter().map(cql_color_model));
                    if !cell.text.is_empty() {
                        models.push(cql_color_model(&cell.text_style.fill));
                    }
                }
            }
            if models.is_empty() {
                cql_cmp_str("none", op, value)
            } else if op == CqlCompareOp::Ne {
                !models
                    .iter()
                    .any(|m| cql_cmp_str(m, CqlCompareOp::Eq, value))
            } else {
                models.iter().any(|m| cql_cmp_str(m, op, value))
            }
        }
        "fill.overprint" => {
            cql_cmp_bool(obj.blend_mode == coree_core::BlendMode::Multiply, op, value)
        }
        "outline.width" => {
            let outline = effective_outline(obj);
            let w = if outline.is_visible() {
                outline.effective_width_mm()
            } else {
                0.0
            };
            cql_cmp_num(w, op, value, true)
        }
        "outline.hairline" | "hairline" => {
            let outline = effective_outline(obj);
            cql_cmp_bool(outline.is_visible() && outline.is_hairline, op, value)
        }
        "outline.overprint" | "overprint" => {
            cql_cmp_bool(effective_outline(obj).overprint, op, value)
        }
        "outline.model" => {
            let outline = effective_outline(obj);
            let model = if outline.is_visible() {
                cql_color_model(&outline.color)
            } else {
                "none"
            };
            cql_cmp_str(model, op, value)
        }
        "color" => {
            let CqlLiteral::Str(ref s) = value else {
                return false;
            };
            let mut colors = obj.fill.referenced_colors();
            for run in text_runs_ref(&obj.payload) {
                colors.push(run.style.fill.clone());
            }
            if let NodePayload::Table(ref table) = obj.payload {
                for cell in &table.cells {
                    colors.extend(cell.fill.referenced_colors());
                    if !cell.text.is_empty() {
                        colors.push(cell.text_style.fill.clone());
                    }
                }
            }
            let outline = effective_outline(obj);
            if outline.is_visible() {
                colors.push(outline.color.clone());
            }
            if op == CqlCompareOp::Ne {
                !colors
                    .iter()
                    .any(|c| cql_color_matches_text(c, s, CqlCompareOp::Eq))
            } else {
                colors.iter().any(|c| cql_color_matches_text(c, s, op))
            }
        }
        "text" => {
            let txt = extract_searchable_text(&obj.payload).unwrap_or_default();
            cql_cmp_str(&txt, op, value)
        }
        "text.len" | "text.count" => {
            let len = extract_searchable_text(&obj.payload).map_or(0, |s| s.chars().count());
            cql_cmp_num(len as f64, op, value, false)
        }
        "font" | "text.font" => {
            let mut fonts: Vec<&str> = text_runs_ref(&obj.payload)
                .iter()
                .map(|r| r.style.font_family.trim())
                .collect();
            if let NodePayload::Table(ref table) = obj.payload {
                for cell in &table.cells {
                    if !cell.text.is_empty() {
                        fonts.push(cell.text_style.font_family.trim());
                    }
                }
            }
            if fonts.is_empty() {
                return false;
            }
            if op == CqlCompareOp::Ne {
                !fonts
                    .iter()
                    .any(|f| cql_cmp_str(f, CqlCompareOp::Eq, value))
            } else {
                fonts.iter().any(|f| cql_cmp_str(f, op, value))
            }
        }
        _ => false,
    }
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
        let p1_layer = doc.pages()[0].layers[0].id;
        let p2_layer = doc.pages()[1].layers[0].id;
        assert_eq!(doc.pages()[1].id, p2);

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
        r2_obj.lens = Some(LensConfig::new(coree_core::LensKind::Invert));
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
            doc.object(r1).unwrap().fill,
            Fill::Solid(Color::cmyk(0.0, 1.0, 1.0, 0.0))
        );
        assert_eq!(
            doc.object(r2).unwrap().fill,
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
        let NodePayload::ArtisticText(ref at1) = doc.object(t1).unwrap().payload else {
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
        let NodePayload::ArtisticText(ref at2) = doc.object(t2).unwrap().payload else {
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
        assert!(doc.object(r2).unwrap().outline.overprint);

        let rep_w = find_and_replace(
            &mut doc,
            &lens_hairline_q,
            &ReplaceAction::ReplaceOutlineWidth {
                width_mm: 0.5,
                is_hairline: false,
            },
        );
        assert_eq!(rep_w.modified_ids, vec![r2]);
        assert!(!doc.object(r2).unwrap().outline.is_hairline);
        assert!((doc.object(r2).unwrap().outline.width_mm - 0.5).abs() < 1e-9);
    }

    #[test]
    fn cql_parser_and_evaluator_handles_units_models_and_boolean_logic() {
        let mut doc = Document::new_a4("CQL Suite");

        // CutContour hairline rectangle
        let cut = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "CutLine",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 50.0,
                        height: 30.0,
                        corner_radii: [0.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::cut_contour()),
                None,
            )
            .unwrap();

        // Thick 2pt CMYK curve with overprint
        let mut thick_curve = SceneObject::new(
            ObjectId(0),
            "Heavy Stroke",
            NodePayload::Path(coree_core::BezierPath::from_rect(
                coree_core::Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0),
            )),
        )
        .with_fill(Fill::Solid(Color::CYAN));
        thick_curve.outline = OutlineStyle::solid(Color::KEY_BLACK, 2.0 * 25.4 / 72.0);
        thick_curve.outline.overprint = true;
        let c1 = doc.add_object(thick_curve, None).unwrap();

        // Spot Arabic text
        let t1 = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Title",
                    NodePayload::ArtisticText(ArtisticText::from_plain(
                        "شعار تجريبي",
                        TextStyle {
                            font_family: "Cairo".to_owned(),
                            fill: Color::spot("Pantone185", 1.0, [0.0, 0.9, 0.8, 0.0]),
                            ..TextStyle::default()
                        },
                    )),
                )
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();

        assert_eq!(
            evaluate_cql_query(
                &doc,
                FindScope::WholeDocument,
                "@color == 'CutContour' and @outline.hairline"
            )
            .unwrap(),
            vec![cut]
        );
        assert_eq!(
            evaluate_cql_query(
                &doc,
                FindScope::WholeDocument,
                "@type == 'curve' and @outline.width >= 1.9pt and @outline.overprint == true"
            )
            .unwrap(),
            vec![c1]
        );
        assert_eq!(
            evaluate_cql_query(
                &doc,
                FindScope::WholeDocument,
                "(@fill.model == 'spot' or @fill.model == 'rgb') and @text contains 'تجريبي' and not @locked"
            )
            .unwrap(),
            vec![t1]
        );
        assert!(parse_cql_query("@type == 'unclosed").is_err());
    }
}
