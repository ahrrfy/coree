// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room ZCF (CDR X6–2025, v16–26) indirect pointer resolution,
//! JSON style sheet (`styles/document.cdss` & `loda` `0x00C9`) decoding,
//! XML document palette / color context / XMP metadata / textinfo parsing,
//! and bounded preview PNG extraction (`extract_cdr_preview_png`).

use crate::cdr_riff::{decode_dib_to_rgba_with_budget, is_cdr_form_type, is_cdt_form_type};
use crate::cdr_zip::is_zip_bytes;
use crate::cmx::is_cmx_form_type;
use crate::coord::{cdr32_to_mm, derive_spot_fallback_cmyk};
use crate::error::{CdrError, CdrResult};
use crate::limits::{read_bounded, sanitize_zip_path, ByteReader, CodecLimits};
use crate::riff::{parse_riff_with_budget, RiffDocument, RiffNode};
use coree_core::{
    math::{sanitize_f32, sanitize_f64},
    Color, ColorMode, ColorSwatch, Document, Fill, GradientStop, LineCap, LineJoin, OutlineStyle,
    Point2D, TextAlign, TextDirection, TextStyle,
};
use flate2::{write::ZlibEncoder, Compression};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Cursor, Write};
use zip::ZipArchive;

/// PNG 8-byte file signature.
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Parsed document-wide ZCF resources loaded alongside `content/root.dat`.
#[derive(Debug, Clone, Default)]
pub struct ZcfContext {
    /// `true` when `content/root.dat` used ZCF 16-byte indirect chunk pointers
    /// (native CDR X6–2025 structure).
    pub is_zcf: bool,
    /// Optional numeric version hint parsed from `styles/document.cdss` or `META-INF/metadata.xml`.
    pub version_hint: Option<u16>,
    /// Optional fallback page dimensions `(width_mm, height_mm)` from XMP metadata.
    pub metadata_page_size_mm: Option<(f64, f64)>,
    /// Parsed `styles/document.cdss` style sheet.
    pub stylesheet: CdssStyleSheet,
    /// Parsed `META-INF/textinfo.xml` page/layer/object name lists.
    pub textinfo: TextInfoXml,
}

/// Parsed fill/outline/text defaults from a single `styles/document.cdss` style entry
/// or per-object `loda` `0x00C9` JSON override.
#[derive(Debug, Clone, Default)]
pub struct CdssStyle {
    /// Explicit fill type (`Some(0)` = None, `Some(1)` = Solid, `Some(2)` = Fountain, `Some(6)` = TwoColor, `Some(7)` = Texture).
    pub fill_type: Option<u32>,
    /// Fountain sub-type (`0` = Linear, `1` = Radial, `2` = Conical, `3` = Square).
    pub fill_fountain_type: Option<u32>,
    pub fill_primary: Option<Color>,
    pub fill_secondary: Option<Color>,
    pub fill_stops: Vec<GradientStop>,
    pub fill_angle_deg: Option<f64>,
    pub fill_overprint: Option<bool>,
    /// Explicit outline type (`Some(0)` = solid/normal in JSON, `Some(1)` = none if width=0).
    pub outline_type: Option<u32>,
    pub outline_width_mm: Option<f64>,
    pub outline_color: Option<Color>,
    pub outline_cap: Option<LineCap>,
    pub outline_join: Option<LineJoin>,
    pub outline_miter_limit: Option<f64>,
    pub outline_dash: Vec<f64>,
    pub outline_behind_fill: Option<bool>,
    pub outline_scale_with_object: Option<bool>,
    pub outline_overprint: Option<bool>,
    /// Character/paragraph text defaults (e.g., from `~artistic~`, `~paragraph~`).
    pub text_style: Option<TextStyle>,
    /// Parent style name if `"parent"` is present.
    pub parent: Option<String>,
}

impl CdssStyle {
    /// Converts this style's explicit fill fields into a [`Fill`] when `fill_type` is present.
    /// Gradient coordinates are emitted normalized in `[0.0..=1.0]` space; object assembly
    /// rescales them onto the target geometry's local millimeter bounds.
    #[must_use]
    pub fn explicit_fill(&self) -> Option<Fill> {
        match self.fill_type? {
            0 => Some(Fill::None),
            1 => self.fill_primary.clone().map(Fill::Solid),
            2 => {
                let stops = if !self.fill_stops.is_empty() {
                    self.fill_stops.clone()
                } else {
                    let c0 = self.fill_primary.clone().unwrap_or(Color::BLACK);
                    let c1 = self.fill_secondary.clone().unwrap_or(Color::WHITE);
                    vec![GradientStop::new(0.0, c0), GradientStop::new(1.0, c1)]
                };
                let angle_deg = self.fill_angle_deg.unwrap_or(0.0);
                match self.fill_fountain_type.unwrap_or(0) {
                    1 => Some(Fill::RadialGradient {
                        center: Point2D::new(0.5, 0.5),
                        radius: 0.5,
                        focal: Point2D::new(0.5, 0.5),
                        stops,
                    }),
                    2 => Some(Fill::ConicalGradient {
                        center: Point2D::new(0.5, 0.5),
                        angle_deg,
                        stops,
                    }),
                    3 => Some(Fill::SquareGradient {
                        center: Point2D::new(0.5, 0.5),
                        size: 1.0,
                        angle_deg,
                        stops,
                    }),
                    _ => {
                        let rad = angle_deg.to_radians();
                        let (s, c) = rad.sin_cos();
                        Some(Fill::LinearGradient {
                            start: Point2D::new(0.5 - 0.5 * c, 0.5 - 0.5 * s),
                            end: Point2D::new(0.5 + 0.5 * c, 0.5 + 0.5 * s),
                            stops,
                        })
                    }
                }
            }
            6 => Some(Fill::TwoColorPattern {
                fg: self.fill_primary.clone().unwrap_or(Color::BLACK),
                bg: self.fill_secondary.clone().unwrap_or(Color::WHITE),
                pattern_name: "Checker".to_owned(),
                tile_w: 8.0,
                tile_h: 8.0,
            }),
            7 => Some(Fill::TextureFill {
                preset: "marble".to_owned(),
                seed: 1,
                fg: self.fill_primary.clone().unwrap_or(Color::BLACK),
                bg: self.fill_secondary.clone().unwrap_or(Color::WHITE),
            }),
            _ => None,
        }
    }

    /// Merges any explicit JSON outline fields on top of an optional base [`OutlineStyle`].
    pub fn apply_outline_overrides(&self, base: &mut OutlineStyle, base_was_explicit_none: bool) {
        if base_was_explicit_none {
            return;
        }
        if let Some(w) = self.outline_width_mm {
            if w > 0.0 {
                base.width_mm = w;
                base.is_hairline = w <= 0.08;
            }
        }
        if let Some(ref c) = self.outline_color {
            if base.width_mm > 0.0 || self.outline_width_mm.is_some_and(|w| w > 0.0) {
                base.color = c.clone();
            }
        }
        if let Some(cap) = self.outline_cap {
            base.cap = cap;
        }
        if let Some(join) = self.outline_join {
            base.join = join;
        }
        if let Some(miter) = self.outline_miter_limit {
            base.miter_limit = miter.max(1.0);
        }
        if let Some(bf) = self.outline_behind_fill {
            base.behind_fill = bf;
        }
        if let Some(swo) = self.outline_scale_with_object {
            base.scale_with_object = swo;
        }
        if let Some(op) = self.outline_overprint {
            base.overprint = op;
        }
        if !self.outline_dash.is_empty() {
            base.dash_pattern = self.outline_dash.clone();
        }
    }

    fn merge_child(&self, child: &Self) -> Self {
        let mut out = self.clone();
        if child.fill_type.is_some() {
            out.fill_type = child.fill_type;
        }
        if child.fill_fountain_type.is_some() {
            out.fill_fountain_type = child.fill_fountain_type;
        }
        if child.fill_primary.is_some() {
            out.fill_primary = child.fill_primary.clone();
        }
        if child.fill_secondary.is_some() {
            out.fill_secondary = child.fill_secondary.clone();
        }
        if !child.fill_stops.is_empty() {
            out.fill_stops = child.fill_stops.clone();
        }
        if child.fill_angle_deg.is_some() {
            out.fill_angle_deg = child.fill_angle_deg;
        }
        if child.fill_overprint.is_some() {
            out.fill_overprint = child.fill_overprint;
        }
        if child.outline_type.is_some() {
            out.outline_type = child.outline_type;
        }
        if child.outline_width_mm.is_some() {
            out.outline_width_mm = child.outline_width_mm;
        }
        if child.outline_color.is_some() {
            out.outline_color = child.outline_color.clone();
        }
        if child.outline_cap.is_some() {
            out.outline_cap = child.outline_cap;
        }
        if child.outline_join.is_some() {
            out.outline_join = child.outline_join;
        }
        if child.outline_miter_limit.is_some() {
            out.outline_miter_limit = child.outline_miter_limit;
        }
        if !child.outline_dash.is_empty() {
            out.outline_dash = child.outline_dash.clone();
        }
        if child.outline_behind_fill.is_some() {
            out.outline_behind_fill = child.outline_behind_fill;
        }
        if child.outline_scale_with_object.is_some() {
            out.outline_scale_with_object = child.outline_scale_with_object;
        }
        if child.outline_overprint.is_some() {
            out.outline_overprint = child.outline_overprint;
        }
        if child.text_style.is_some() {
            out.text_style = child.text_style.clone();
        }
        out.parent = child.parent.clone();
        out
    }
}

/// Parsed `styles/document.cdss` style sheet.
#[derive(Debug, Clone, Default)]
pub struct CdssStyleSheet {
    pub version: Option<u16>,
    pub styles: HashMap<String, CdssStyle>,
}

impl CdssStyleSheet {
    /// Resolves a style by name, recursively merging its `"parent"` inheritance chain
    /// up to 16 levels deep with cycle protection.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<CdssStyle> {
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cur_name = name;
        while seen.insert(cur_name.to_owned()) && chain.len() < 16 {
            let st = self.styles.get(cur_name)?;
            chain.push(st);
            match st.parent.as_deref() {
                Some(p) if !p.is_empty() => cur_name = p,
                _ => break,
            }
        }
        let mut merged = (*chain.last()?).clone();
        for child in chain.into_iter().rev().skip(1) {
            merged = merged.merge_child(child);
        }
        Some(merged)
    }

    /// Returns the default graphic style (`~graphic~`), if defined.
    #[must_use]
    pub fn default_graphic(&self) -> Option<&CdssStyle> {
        self.styles.get("~graphic~")
    }

    /// Returns the default artistic text style (`~artistic~`), if defined.
    #[must_use]
    pub fn default_artistic_text(&self) -> Option<&TextStyle> {
        self.styles
            .get("~artistic~")
            .and_then(|s| s.text_style.as_ref())
    }

    /// Returns the default paragraph text style (`~paragraph~`), if defined.
    #[must_use]
    pub fn default_paragraph_text(&self) -> Option<&TextStyle> {
        self.styles
            .get("~paragraph~")
            .and_then(|s| s.text_style.as_ref())
    }
}

/// Parsed `META-INF/textinfo.xml` name tables.
#[derive(Debug, Clone, Default)]
pub struct TextInfoXml {
    pub page_names: Vec<String>,
    pub layer_names: Vec<String>,
    pub object_names: Vec<String>,
}

// =============================================================================
// ZCF 16-Byte Indirect Pointer Detection & Bounded Dereferencing
// =============================================================================

/// Detects whether a parsed `content/root.dat` RIFF tree uses CDR X6–2025 (ZCF)
/// 16-byte indirect chunk pointers (`file_index`, `data_len`, `data_offset`, `reserved`).
#[must_use]
pub fn is_zcf_indirect_tree(tree: &RiffDocument, data_file_count: usize) -> bool {
    // Definitive marker #1: top-level `vrsn` or `fver` is an inline 16-byte ZCF pointer
    // (`file_index == 0xFFFF_FFFF` and `data_len <= 8`).
    for tag in [b"vrsn", b"fver"] {
        if let Some(raw) = tree.find_chunk(tag) {
            if is_inline_zcf_pointer(raw) {
                return true;
            }
        }
    }

    // Definitive marker #2: `CDRO`/`CDTO` root form with at least one valid 16-byte ZCF pointer.
    (&tree.form_type == b"CDRO" || &tree.form_type == b"CDTO")
        && tree_has_zcf_pointer(&tree.children, data_file_count)
}

fn is_inline_zcf_pointer(raw: &[u8]) -> bool {
    if raw.len() < 16 {
        return false;
    }
    let file_index = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let data_len = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    file_index == u32::MAX && data_len <= raw.len().saturating_sub(8)
}

fn is_external_zcf_pointer(raw: &[u8], data_file_count: usize) -> bool {
    if raw.len() != 16 {
        return false;
    }
    let file_index = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    let data_len = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
    file_index < data_file_count && data_len > 0
}

fn tree_has_zcf_pointer(nodes: &[RiffNode], data_file_count: usize) -> bool {
    for node in nodes {
        match node {
            RiffNode::Chunk { data, .. } => {
                if is_inline_zcf_pointer(data) || is_external_zcf_pointer(data, data_file_count) {
                    return true;
                }
            }
            RiffNode::List { children, .. } => {
                if tree_has_zcf_pointer(children, data_file_count) {
                    return true;
                }
            }
        }
    }
    false
}

/// Resolves all 16-byte ZCF indirect leaf chunks inside `tree` against the deduplicated
/// `data_pool` indexed by `file_index_map` (preserving exact 0-based `dataFileList.dat` indices).
///
/// Strictly enforces [`CodecLimits`] tree depth, bounds checks, and cumulative byte budget
/// (`budget_remaining`) BEFORE copying any slice from `data_pool`, preventing pointer-aliasing
/// memory bombs.
pub fn resolve_zcf_riff_tree(
    tree: RiffDocument,
    data_pool: &[Vec<u8>],
    file_index_map: &[Option<usize>],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
    warnings: &mut Vec<String>,
) -> CdrResult<RiffDocument> {
    let mut resolved_children = Vec::with_capacity(tree.children.len());
    for child in tree.children {
        resolved_children.push(resolve_zcf_node(
            child,
            data_pool,
            file_index_map,
            limits,
            1,
            budget_remaining,
            warnings,
        )?);
    }
    Ok(RiffDocument {
        endian: tree.endian,
        form_type: tree.form_type,
        children: resolved_children,
    })
}

fn resolve_zcf_node(
    node: RiffNode,
    data_pool: &[Vec<u8>],
    file_index_map: &[Option<usize>],
    limits: &CodecLimits,
    depth: usize,
    budget_remaining: &mut u64,
    warnings: &mut Vec<String>,
) -> CdrResult<RiffNode> {
    limits.check_depth(depth)?;
    match node {
        RiffNode::List {
            list_type,
            children,
            compressed,
        } => {
            let mut resolved = Vec::with_capacity(children.len());
            for child in children {
                resolved.push(resolve_zcf_node(
                    child,
                    data_pool,
                    file_index_map,
                    limits,
                    depth + 1,
                    budget_remaining,
                    warnings,
                )?);
            }
            Ok(RiffNode::List {
                list_type,
                children: resolved,
                compressed,
            })
        }
        RiffNode::Chunk {
            fourcc,
            data,
            compressed,
        } => {
            let resolved_data = resolve_zcf_chunk_payload(
                &fourcc,
                data,
                data_pool,
                file_index_map,
                limits,
                budget_remaining,
                warnings,
            )?;
            Ok(RiffNode::Chunk {
                fourcc,
                data: resolved_data,
                compressed,
            })
        }
    }
}

fn resolve_zcf_chunk_payload(
    fourcc: &[u8; 4],
    raw: Vec<u8>,
    data_pool: &[Vec<u8>],
    file_index_map: &[Option<usize>],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
    warnings: &mut Vec<String>,
) -> CdrResult<Vec<u8>> {
    if raw.len() < 16 {
        return Ok(raw);
    }

    let file_index = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let data_len_u32 = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
    let data_len = usize::try_from(data_len_u32)
        .map_err(|_| CdrError::ArithmeticOverflow("ZCF pointer data_len"))?;

    if file_index == u32::MAX {
        // Inline ZCF data lives in bytes `8..8+data_len`.
        let end = 8usize
            .checked_add(data_len)
            .ok_or(CdrError::ArithmeticOverflow("ZCF inline data_len"))?;
        if end <= raw.len() {
            return Ok(raw[8..end].to_vec());
        }
        warnings.push(format!(
            "ZCF inline chunk '{}' declared length {} exceeding {} available bytes",
            crate::riff::fourcc_to_string(fourcc),
            data_len,
            raw.len().saturating_sub(8)
        ));
        return Ok(raw);
    }

    if raw.len() != 16 {
        return Ok(raw);
    }

    let data_offset_u32 = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let data_offset = usize::try_from(data_offset_u32)
        .map_err(|_| CdrError::ArithmeticOverflow("ZCF pointer data_offset"))?;

    let fidx =
        usize::try_from(file_index).map_err(|_| CdrError::ArithmeticOverflow("ZCF file_index"))?;

    let Some(Some(pool_idx)) = file_index_map.get(fidx).copied() else {
        let msg = format!(
            "ZCF chunk '{}' references missing/out-of-bounds data file index {}",
            crate::riff::fourcc_to_string(fourcc),
            file_index
        );
        warnings.push(msg.clone());
        return Err(CdrError::InvalidChunk {
            fourcc: crate::riff::fourcc_to_string(fourcc),
            reason: msg,
        });
    };
    let Some(blob) = data_pool.get(pool_idx) else {
        return Err(CdrError::InvalidChunk {
            fourcc: crate::riff::fourcc_to_string(fourcc),
            reason: format!(
                "ZCF chunk references invalid data pool index {pool_idx} (file_index={file_index})"
            ),
        });
    };

    let end = data_offset
        .checked_add(data_len)
        .ok_or(CdrError::ArithmeticOverflow("ZCF data slice end"))?;
    if end > blob.len() {
        return Err(CdrError::UnexpectedEof {
            context: "ZCF external data file slice",
            needed: end,
            available: blob.len(),
        });
    }

    // Charge resolved copy against cumulative budget BEFORE allocating to defeat aliasing bombs!
    limits.consume_budget(budget_remaining, u64::from(data_len_u32))?;
    Ok(blob[data_offset..end].to_vec())
}

// =============================================================================
// CDR Color String & Style Sheet (`styles/document.cdss` + `0x00C9`) Parsers
// =============================================================================

/// Parses a CDR X6–2025 style color string such as:
/// - `"CMYK,USER,0,0,0,100,100,00000000-0000-0000-0000-000000000000"`
/// - `"CMYK255,USER,0,255,255,0,100,cccd19cb-..."`
/// - `"RGB255,USER,255,128,0,100,..."`
/// - `"RGB,USER,100,50,0,100,..."`
/// - `"GRAY,USER,50,100,..."` / `"GRAY255,USER,128,100,..."`
/// - `"LAB,USER,55,20,-30,100,..."`
/// - `"HSB,USER,210,80,90,100,..."` / `"HLS,USER,210,50,80,100,..."`
#[must_use]
pub fn parse_cdr_color_string(spec: &str) -> Option<Color> {
    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let model = parts[0].to_ascii_uppercase();
    if !matches!(model.as_str(), "SPOT" | "FIXED" | "PANTONE") && parts.len() < 4 {
        return None;
    }
    // Numeric channels begin at index 2 (after `MODEL,PALETTE_KIND,...`).
    // If index 1 itself is numeric (short form), start at index 1.
    let start = if parts[1].parse::<f64>().is_ok() {
        1
    } else {
        2
    };
    let nums: Vec<f64> = parts[start..]
        .iter()
        .filter_map(|p| p.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .collect();

    match model.as_str() {
        "CMYK" => {
            if nums.len() < 4 {
                return None;
            }
            let alpha = nums
                .get(4)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Cmyk {
                c: (nums[0] / 100.0).clamp(0.0, 1.0) as f32,
                m: (nums[1] / 100.0).clamp(0.0, 1.0) as f32,
                y: (nums[2] / 100.0).clamp(0.0, 1.0) as f32,
                k: (nums[3] / 100.0).clamp(0.0, 1.0) as f32,
                a: alpha,
            })
        }
        "CMYK255" => {
            if nums.len() < 4 {
                return None;
            }
            let alpha = nums
                .get(4)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Cmyk {
                c: (nums[0] / 255.0).clamp(0.0, 1.0) as f32,
                m: (nums[1] / 255.0).clamp(0.0, 1.0) as f32,
                y: (nums[2] / 255.0).clamp(0.0, 1.0) as f32,
                k: (nums[3] / 255.0).clamp(0.0, 1.0) as f32,
                a: alpha,
            })
        }
        "RGB255" | "BGR" => {
            if nums.len() < 3 {
                return None;
            }
            let (r, g, b) = if model == "BGR" {
                (nums[2], nums[1], nums[0])
            } else {
                (nums[0], nums[1], nums[2])
            };
            let alpha = nums
                .get(3)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Rgba {
                r: (r / 255.0).clamp(0.0, 1.0) as f32,
                g: (g / 255.0).clamp(0.0, 1.0) as f32,
                b: (b / 255.0).clamp(0.0, 1.0) as f32,
                a: alpha,
            })
        }
        "RGB" => {
            if nums.len() < 3 {
                return None;
            }
            let scale = if nums[0] > 100.0 || nums[1] > 100.0 || nums[2] > 100.0 {
                255.0
            } else if nums[0] <= 1.0 && nums[1] <= 1.0 && nums[2] <= 1.0 {
                1.0
            } else {
                100.0
            };
            let alpha = nums
                .get(3)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Rgba {
                r: (nums[0] / scale).clamp(0.0, 1.0) as f32,
                g: (nums[1] / scale).clamp(0.0, 1.0) as f32,
                b: (nums[2] / scale).clamp(0.0, 1.0) as f32,
                a: alpha,
            })
        }
        "GRAY" | "GRAYSCALE" => {
            let v = *nums.first()?;
            let scale = if v > 100.0 { 255.0 } else { 100.0 };
            let alpha = nums
                .get(1)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Gray {
                v: (v / scale).clamp(0.0, 1.0) as f32,
                a: alpha,
            })
        }
        "GRAY255" => {
            let v = *nums.first()?;
            let alpha = nums
                .get(1)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Gray {
                v: (v / 255.0).clamp(0.0, 1.0) as f32,
                a: alpha,
            })
        }
        "LAB" => {
            if nums.len() < 3 {
                return None;
            }
            let alpha = nums
                .get(3)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Lab {
                l: nums[0].clamp(0.0, 100.0) as f32,
                a_ch: nums[1].clamp(-128.0, 127.0) as f32,
                b_ch: nums[2].clamp(-128.0, 127.0) as f32,
                alpha,
            })
        }
        "HLS" | "HSL" | "HSB" => {
            if nums.len() < 3 {
                return None;
            }
            let h = nums[0].rem_euclid(360.0) as f32;
            let (s, l) = if model == "HSB" {
                let sv = (nums[1] / 100.0).clamp(0.0, 1.0);
                let bv = (nums[2] / 100.0).clamp(0.0, 1.0);
                let lv = bv * (1.0 - sv * 0.5);
                let sl = if lv <= 1e-6 || lv >= 1.0 - 1e-6 {
                    0.0
                } else {
                    (bv - lv) / lv.min(1.0 - lv)
                };
                (sl as f32, lv as f32)
            } else {
                // ZCF HLS order is H, L, S (0..100)
                let lv = (nums[1] / 100.0).clamp(0.0, 1.0) as f32;
                let sv = (nums[2] / 100.0).clamp(0.0, 1.0) as f32;
                (sv, lv)
            };
            let alpha = nums
                .get(3)
                .map_or(1.0_f32, |&a| (a / 100.0).clamp(0.0, 1.0) as f32);
            Some(Color::Hsl { h, s, l, a: alpha })
        }
        "SPOT" | "FIXED" | "PANTONE" => {
            let name = parts
                .get(1)
                .copied()
                .filter(|s| !s.is_empty() && s.parse::<f64>().is_err())
                .unwrap_or("Spot")
                .to_owned();
            let (tint, fallback_cmyk, alpha) = if nums.len() >= 5 {
                let t = (nums[0] / 100.0).clamp(0.0, 1.0) as f32;
                let max_c = nums[1..5].iter().copied().fold(0.0_f64, f64::max);
                let scale = if max_c > 100.0 {
                    255.0
                } else if max_c > 1.0 {
                    100.0
                } else {
                    1.0
                };
                let cmyk = [
                    (nums[1] / scale).clamp(0.0, 1.0) as f32,
                    (nums[2] / scale).clamp(0.0, 1.0) as f32,
                    (nums[3] / scale).clamp(0.0, 1.0) as f32,
                    (nums[4] / scale).clamp(0.0, 1.0) as f32,
                ];
                let a = nums
                    .get(5)
                    .map_or(1.0_f32, |&v| (v / 100.0).clamp(0.0, 1.0) as f32);
                (t, cmyk, a)
            } else if nums.len() == 4 {
                let max_c = nums[0..4].iter().copied().fold(0.0_f64, f64::max);
                let scale = if max_c > 100.0 {
                    255.0
                } else if max_c > 1.0 {
                    100.0
                } else {
                    1.0
                };
                let cmyk = [
                    (nums[0] / scale).clamp(0.0, 1.0) as f32,
                    (nums[1] / scale).clamp(0.0, 1.0) as f32,
                    (nums[2] / scale).clamp(0.0, 1.0) as f32,
                    (nums[3] / scale).clamp(0.0, 1.0) as f32,
                ];
                (1.0_f32, cmyk, 1.0_f32)
            } else {
                let t = nums
                    .first()
                    .map_or(1.0_f32, |&v| (v / 100.0).clamp(0.0, 1.0) as f32);
                (t, derive_spot_fallback_cmyk(&name), 1.0_f32)
            };
            Some(Color::Spot {
                name,
                tint,
                fallback_cmyk,
                alpha,
            })
        }
        _ => None,
    }
}

/// Parses `styles/document.cdss` JSON style sheet bytes.
#[must_use]
pub fn parse_document_cdss(bytes: &[u8]) -> CdssStyleSheet {
    let Ok(root) = serde_json::from_slice::<Value>(bytes) else {
        return CdssStyleSheet::default();
    };
    let mut sheet = CdssStyleSheet::default();
    if let Some(v) = root.get("version").and_then(Value::as_u64) {
        if let Ok(v16) = u16::try_from(v) {
            if v16 > 0 {
                sheet.version = Some(v16);
            }
        }
    }
    if let Some(style_map) = root.get("style").and_then(Value::as_object) {
        for (name, val) in style_map {
            if let Some(parsed) = parse_cdss_style_value(val) {
                sheet.styles.insert(name.clone(), parsed);
            }
        }
    }
    sheet
}

/// Maximum JSON payload length accepted for a single `loda` `0x00C9` style override (`256 KiB`).
const MAX_C9_STYLE_JSON_BYTES: usize = 256 * 1024;

/// Parses an object's `loda` `0x00C9` JSON style override chunk payload
/// (`u32 flags`, `u32 json_len`, UTF-8 JSON bytes).
#[must_use]
pub fn parse_c9_style_override(raw: &[u8]) -> Option<CdssStyle> {
    let json_bytes = if raw.len() >= 8 {
        let json_len = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
        if json_len > 0 && 8usize.saturating_add(json_len) <= raw.len() {
            &raw[8..8 + json_len]
        } else {
            let brace = raw.iter().position(|&b| b == b'{')?;
            &raw[brace..]
        }
    } else {
        return None;
    };

    if json_bytes.len() > MAX_C9_STYLE_JSON_BYTES {
        return None;
    }

    let val: Value = serde_json::from_slice(json_bytes).ok()?;
    parse_cdss_style_value(&val)
}

fn json_u32(obj: &serde_json::Map<String, Value>, key: &str) -> Option<u32> {
    let v = obj.get(key)?;
    if let Some(n) = v.as_u64() {
        u32::try_from(n).ok()
    } else if let Some(s) = v.as_str() {
        s.trim().parse::<u32>().ok()
    } else {
        None
    }
}

fn json_f64(obj: &serde_json::Map<String, Value>, key: &str) -> Option<f64> {
    let v = obj.get(key)?;
    let f = if let Some(n) = v.as_f64() {
        n
    } else {
        let s = v.as_str()?;
        s.trim().parse::<f64>().ok()?
    };
    f.is_finite().then_some(f)
}

fn json_bool(obj: &serde_json::Map<String, Value>, key: &str) -> Option<bool> {
    let v = obj.get(key)?;
    if let Some(b) = v.as_bool() {
        Some(b)
    } else if let Some(n) = v.as_u64() {
        Some(n != 0)
    } else if let Some(s) = v.as_str() {
        match s.trim() {
            "1" | "true" | "True" => Some(true),
            "0" | "false" | "False" => Some(false),
            _ => None,
        }
    } else {
        None
    }
}

fn parse_cdss_style_value(val: &Value) -> Option<CdssStyle> {
    let obj = val.as_object()?;
    let mut style = CdssStyle {
        parent: obj
            .get("parent")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        ..CdssStyle::default()
    };

    if let Some(fill) = obj.get("fill").and_then(Value::as_object) {
        style.fill_type = json_u32(fill, "type");
        style.fill_fountain_type = json_u32(fill, "fountainType")
            .or_else(|| json_u32(fill, "gradientType"))
            .or_else(|| {
                fill.get("fountainType")
                    .or_else(|| fill.get("gradientType"))
                    .and_then(Value::as_str)
                    .and_then(|s| match s.trim().to_ascii_lowercase().as_str() {
                        "linear" => Some(0),
                        "radial" | "elliptical" => Some(1),
                        "conical" => Some(2),
                        "square" => Some(3),
                        _ => None,
                    })
            });
        style.fill_primary = fill
            .get("primaryColor")
            .and_then(Value::as_str)
            .and_then(parse_cdr_color_string);
        style.fill_secondary = fill
            .get("secondaryColor")
            .and_then(Value::as_str)
            .and_then(parse_cdr_color_string);
        style.fill_overprint = json_bool(fill, "overprint");
        if let Some(ang) = json_f64(fill, "angle") {
            style.fill_angle_deg = Some(if ang.abs() > 360.0 {
                ang / 1_000_000.0
            } else {
                ang
            });
        }
        // Optional fountain stops (`fountainFill` / `stops`)
        if let Some(stops_arr) = fill
            .get("stops")
            .and_then(Value::as_array)
            .or_else(|| fill.get("fountainStops").and_then(Value::as_array))
        {
            for st in stops_arr {
                if let Some(so) = st.as_object() {
                    let pos = json_f64(so, "position")
                        .or_else(|| json_f64(so, "offset"))
                        .unwrap_or(0.0);
                    let norm_pos = if pos > 1.0 { pos / 100.0 } else { pos };
                    if let Some(color) = so
                        .get("color")
                        .and_then(Value::as_str)
                        .and_then(parse_cdr_color_string)
                    {
                        style
                            .fill_stops
                            .push(GradientStop::new(norm_pos.clamp(0.0, 1.0) as f32, color));
                    }
                }
            }
        }
    }

    if let Some(outl) = obj.get("outline").and_then(Value::as_object) {
        style.outline_type = json_u32(outl, "type");
        if let Some(w_raw) = json_f64(outl, "width") {
            if w_raw >= 0.0 {
                style.outline_width_mm = Some(w_raw / coree_core::math::CDR_UNITS_PER_MM);
            }
        }
        style.outline_color = outl
            .get("color")
            .and_then(Value::as_str)
            .and_then(parse_cdr_color_string);
        style.outline_cap = json_u32(outl, "endCaps").map(|c| match c {
            1 => LineCap::Round,
            2 => LineCap::Square,
            _ => LineCap::Butt,
        });
        style.outline_join = json_u32(outl, "joinType").map(|j| match j {
            1 => LineJoin::Round,
            2 => LineJoin::Bevel,
            _ => LineJoin::Miter,
        });
        style.outline_miter_limit = json_f64(outl, "miterLimit").map(|m| m.max(1.0));
        style.outline_behind_fill = json_bool(outl, "behindFill");
        style.outline_scale_with_object = json_bool(outl, "scaleWithObject");
        style.outline_overprint = json_bool(outl, "overprint");
        if let Some(dashes) = outl
            .get("dashPattern")
            .or_else(|| outl.get("dashes"))
            .and_then(Value::as_array)
        {
            for d in dashes {
                if let Some(v) = d.as_f64().filter(|x| x.is_finite() && *x > 0.0) {
                    style
                        .outline_dash
                        .push((v / coree_core::math::CDR_UNITS_PER_MM).max(0.2));
                }
            }
        }
    }

    if let Some(ch) = obj.get("character").and_then(Value::as_object) {
        let mut ts = TextStyle::default();
        if let Some(cfill) = ch.get("fill").and_then(Value::as_object) {
            if let Some(c) = cfill
                .get("primaryColor")
                .and_then(Value::as_str)
                .and_then(parse_cdr_color_string)
            {
                ts.fill = c;
            }
        }
        // Read both `latin` and `RTL` script blocks independently (supporting separate Latin vs Arabic fonts)
        let latin_block = ch.get("latin").and_then(Value::as_object);
        let rtl_block = ch.get("RTL").and_then(Value::as_object);
        let primary_block = latin_block.or(rtl_block);
        if let Some(fb) = primary_block {
            if let Some(fname) = fb.get("font").and_then(Value::as_str) {
                if !fname.trim().is_empty() {
                    ts.font_family = fname.trim().to_owned();
                }
            }
            if let Some(sz_cdr) = json_f64(fb, "size") {
                // CDR stores character height in 0.1 µm (`84667` = 8.4667 mm = 24 pt).
                let pt = (sz_cdr / coree_core::math::CDR_UNITS_PER_MM) * (72.0 / 25.4);
                if pt.is_finite() && pt >= 1.0 {
                    ts.font_size_pt = pt;
                }
            }
            if let Some(w) = json_u32(fb, "weight") {
                // CDR weight can be Windows LOGFONT (100..=900) or OS/2 class (e.g. 400=normal, 700=bold)
                let norm_w = if w <= 12 {
                    match w {
                        0..=2 => 300,
                        3 | 4 => 400,
                        5 | 6 => 600,
                        7 | 8 => 700,
                        _ => 900,
                    }
                } else {
                    w.clamp(100, 900)
                };
                ts.set_weight(norm_w as u16);
            }
            if let Some(it) = json_bool(fb, "italic") {
                ts.italic = it;
            }
            if let Some(ul) = json_u32(fb, "underline") {
                ts.underline = ul != 0;
            }
            if let Some(so) = json_u32(fb, "strikeout") {
                ts.strikethrough = so != 0;
            }
            if let Some(cs) = json_u32(fb, "charset") {
                if cs == 178 {
                    ts.direction = TextDirection::Rtl;
                }
            }
        }
        if let Some(rb) = rtl_block {
            if let Some(fname) = rb.get("font").and_then(Value::as_str) {
                let trimmed = fname.trim();
                if !trimmed.is_empty() {
                    ts.bidi_font_family = Some(trimmed.to_owned());
                    ts.direction = TextDirection::Auto;
                }
            }
            if latin_block.is_none() {
                if let Some(cs) = json_u32(rb, "charset") {
                    if cs == 178 {
                        ts.direction = TextDirection::Rtl;
                    }
                }
            }
        }
        if let Some(para) = obj.get("paragraph").and_then(Value::as_object) {
            if let Some(al) = json_u32(para, "alignment") {
                ts.align = match al {
                    1 => TextAlign::Left,
                    2 => TextAlign::Center,
                    3 => TextAlign::Right,
                    4 => TextAlign::Justify,
                    5 => TextAlign::ForceJustifyKashida,
                    _ => TextAlign::Left,
                };
            }
        }
        style.text_style = Some(ts);
    }

    Some(style)
}

// =============================================================================
// XML Auxiliary Parsers (`docPalette.xml`, `color.xml`, `textinfo.xml`, XMP)
// =============================================================================

#[derive(Debug, Clone)]
struct XmlElem {
    tag: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<XmlElem>,
}

impl XmlElem {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn parse_xml_strict(xml: &str) -> Option<Vec<XmlElem>> {
    const MAX_XML_NODES: usize = 65_536;
    const MAX_XML_DEPTH: usize = 128;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut roots: Vec<XmlElem> = Vec::new();
    let mut stack: Vec<XmlElem> = Vec::new();
    let mut total_nodes = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::DocType(_)) | Err(_) => return None,
            Ok(Event::Comment(_)) | Ok(Event::Decl(_)) | Ok(Event::PI(_)) => {}
            Ok(Event::Start(e)) => {
                total_nodes += 1;
                if total_nodes > MAX_XML_NODES || stack.len() >= MAX_XML_DEPTH {
                    return None;
                }
                let tag = std::str::from_utf8(e.name().as_ref()).ok()?.to_owned();
                let mut attrs = Vec::new();
                for a in e.attributes() {
                    let attr = a.ok()?;
                    let k = std::str::from_utf8(attr.key.as_ref()).ok()?.to_owned();
                    let v = attr
                        .decoded_and_normalized_value(
                            quick_xml::XmlVersion::default(),
                            reader.decoder(),
                        )
                        .ok()?
                        .into_owned();
                    attrs.push((k, v));
                }
                stack.push(XmlElem {
                    tag,
                    attrs,
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            Ok(Event::Empty(e)) => {
                total_nodes += 1;
                if total_nodes > MAX_XML_NODES || stack.len() >= MAX_XML_DEPTH {
                    return None;
                }
                let tag = std::str::from_utf8(e.name().as_ref()).ok()?.to_owned();
                let mut attrs = Vec::new();
                for a in e.attributes() {
                    let attr = a.ok()?;
                    let k = std::str::from_utf8(attr.key.as_ref()).ok()?.to_owned();
                    let v = attr
                        .decoded_and_normalized_value(
                            quick_xml::XmlVersion::default(),
                            reader.decoder(),
                        )
                        .ok()?
                        .into_owned();
                    attrs.push((k, v));
                }
                let elem = XmlElem {
                    tag,
                    attrs,
                    text: String::new(),
                    children: Vec::new(),
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(elem);
                } else {
                    roots.push(elem);
                }
            }
            Ok(Event::End(e)) => {
                let eq = e.name();
                let end_tag = std::str::from_utf8(eq.as_ref()).ok()?;
                let elem = stack.pop()?;
                if elem.tag != end_tag {
                    return None;
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(elem);
                } else {
                    roots.push(elem);
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(top) = stack.last_mut() {
                    let txt = t.xml10_content().ok()?;
                    top.text.push_str(&txt);
                }
            }
            Ok(Event::CData(c)) => {
                if let Some(top) = stack.last_mut() {
                    let txt = c.decode().ok()?;
                    top.text.push_str(&txt);
                }
            }
            Ok(Event::GeneralRef(r)) => {
                let resolved = if let Ok(Some(ch)) = r.resolve_char_ref() {
                    ch.to_string()
                } else {
                    let rb: &[u8] = &r;
                    match rb {
                        b"amp" => "&".to_owned(),
                        b"lt" => "<".to_owned(),
                        b"gt" => ">".to_owned(),
                        b"quot" => "\"".to_owned(),
                        b"apos" => "'".to_owned(),
                        _ => return None,
                    }
                };
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&resolved);
                }
            }
        }
    }

    if !stack.is_empty() {
        return None;
    }
    Some(roots)
}

fn find_first_elem<'a>(nodes: &'a [XmlElem], tag: &str) -> Option<&'a XmlElem> {
    for n in nodes {
        if n.tag == tag {
            return Some(n);
        }
        if let Some(found) = find_first_elem(&n.children, tag) {
            return Some(found);
        }
    }
    None
}

fn collect_elems<'a>(nodes: &'a [XmlElem], tag: &str, out: &mut Vec<&'a XmlElem>) {
    for n in nodes {
        if n.tag == tag {
            out.push(n);
        }
        collect_elems(&n.children, tag, out);
    }
}

/// Parses CDR `color/docPalette.xml` (and clean-room `color/documentPalette.xml`)
/// into [`ColorSwatch`] entries using structural XML parsing (no DTD/entity expansion).
#[must_use]
pub fn parse_doc_palette_xml(xml: &str) -> Vec<ColorSwatch> {
    let Some(roots) = parse_xml_strict(xml) else {
        return Vec::new();
    };
    let mut swatches = Vec::new();

    // Native CDR XML format: `<color cs="CMYK" name="Red" tints="0,1,1,0"/>`
    let mut color_elems = Vec::new();
    collect_elems(&roots, "color", &mut color_elems);
    for el in color_elems {
        let cs = el.attr("cs").unwrap_or_default();
        let name = el.attr("name").unwrap_or_default();
        let tints = el.attr("tints").unwrap_or_default();
        if let Some(color) = parse_palette_color(cs, name, tints) {
            let label = if name.is_empty() {
                format!("Color {}", swatches.len() + 1)
            } else {
                name.to_owned()
            };
            swatches.push(ColorSwatch::new(label, color));
        }
    }

    // Clean-room synthetic fallback: `<swatch name="..." hex="#RRGGBBAA"/>`
    if swatches.is_empty() {
        let mut swatch_elems = Vec::new();
        collect_elems(&roots, "swatch", &mut swatch_elems);
        for el in swatch_elems {
            let name = el.attr("name").unwrap_or_default();
            if let Some(hex) = el.attr("hex") {
                if let Ok(color) = Color::from_hex(hex) {
                    let label = if name.is_empty() {
                        hex.to_owned()
                    } else {
                        name.to_owned()
                    };
                    swatches.push(ColorSwatch::new(label, color));
                }
            }
        }
    }

    swatches
}

fn parse_palette_color(cs: &str, name: &str, tints: &str) -> Option<Color> {
    let vals: Vec<f32> = tints
        .split(',')
        .filter_map(|s| s.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .collect();
    let max_v = vals.iter().copied().fold(0.0_f32, f32::max);
    let norm = |v: f32| -> f32 {
        if max_v > 1.0 && max_v <= 100.0 {
            (v / 100.0).clamp(0.0, 1.0)
        } else if max_v > 100.0 {
            (v / 255.0).clamp(0.0, 1.0)
        } else {
            v.clamp(0.0, 1.0)
        }
    };

    match cs.to_ascii_uppercase().as_str() {
        "CMYK" => {
            if vals.len() < 4 {
                return None;
            }
            Some(Color::cmyk(
                norm(vals[0]),
                norm(vals[1]),
                norm(vals[2]),
                norm(vals[3]),
            ))
        }
        "RGB" => {
            if vals.len() < 3 {
                return None;
            }
            Some(Color::rgb(norm(vals[0]), norm(vals[1]), norm(vals[2])))
        }
        "GRAY" | "GRAYSCALE" => {
            let v = *vals.first()?;
            Some(Color::gray(norm(v)))
        }
        "LAB" => {
            if vals.len() < 3 {
                return None;
            }
            Some(Color::lab(
                sanitize_f32(vals[0], 0.0).clamp(0.0, 100.0),
                sanitize_f32(vals[1], 0.0).clamp(-128.0, 127.0),
                sanitize_f32(vals[2], 0.0).clamp(-128.0, 127.0),
            ))
        }
        "HSB" | "HSL" | "HLS" => {
            if vals.len() < 3 {
                return None;
            }
            Some(Color::Hsl {
                h: vals[0].rem_euclid(360.0),
                s: norm(vals[1]),
                l: norm(vals[2]),
                a: 1.0,
            })
        }
        "SPOT" | "FIXED" | "PANTONE" => {
            let fallback = if vals.len() >= 4 {
                [norm(vals[0]), norm(vals[1]), norm(vals[2]), norm(vals[3])]
            } else {
                derive_spot_fallback_cmyk(name)
            };
            Some(Color::spot(
                if name.is_empty() { "Spot" } else { name },
                1.0,
                fallback,
            ))
        }
        _ => None,
    }
}

/// Parses `color/color.xml` and returns the document's primary [`ColorMode`].
#[must_use]
pub fn parse_color_context_xml(xml: &str) -> Option<ColorMode> {
    let roots = parse_xml_strict(xml)?;
    let model = extract_xml_tag_simple(&roots, "ColorModel")?;
    if model.eq_ignore_ascii_case("rgb") {
        Some(ColorMode::Rgb)
    } else if model.eq_ignore_ascii_case("cmyk") {
        Some(ColorMode::Cmyk)
    } else {
        None
    }
}

/// Parses `META-INF/textinfo.xml` to recover `<PageNames>`, `<LayerNames>`, and `<ObjectNames>`.
#[must_use]
pub fn parse_textinfo_xml(xml: &str) -> TextInfoXml {
    let Some(roots) = parse_xml_strict(xml) else {
        return TextInfoXml::default();
    };
    TextInfoXml {
        page_names: extract_rdf_li_list(&roots, "PageNames"),
        layer_names: extract_rdf_li_list(&roots, "LayerNames"),
        object_names: extract_rdf_li_list(&roots, "ObjectNames"),
    }
}

/// Applies XMP/RDF `META-INF/metadata.xml` fields onto `doc` and `zcf_ctx`.
pub fn apply_xmp_metadata_xml(
    xml: &str,
    doc: &mut Document,
    is_template: &mut bool,
    zcf_ctx: &mut ZcfContext,
) {
    let Some(roots) = parse_xml_strict(xml) else {
        return;
    };
    if let Some(fmt) = extract_xml_tag_simple(&roots, "dc:format") {
        if fmt.to_ascii_lowercase().contains("template") {
            *is_template = true;
        }
    }
    if let Some(ver_str) = extract_xml_tag_simple(&roots, "cdr:CoreVersion") {
        if let Ok(v) = ver_str.trim().parse::<u16>() {
            if v > 0 {
                zcf_ctx.version_hint = Some(v);
            }
        }
    }
    if let Some(author) = extract_rdf_li_list(&roots, "dc:creator")
        .into_iter()
        .find(|s| !s.is_empty())
        .or_else(|| extract_xml_tag_simple(&roots, "crl:LastAuthor").filter(|s| !s.is_empty()))
    {
        doc.metadata_mut().author = author;
    }
    if let Some(title) = extract_rdf_li_list(&roots, "dc:title")
        .into_iter()
        .find(|s| !s.is_empty())
    {
        doc.set_title(title);
    }
    if let Some(subj) = extract_rdf_li_list(&roots, "crl:Subject")
        .into_iter()
        .find(|s| !s.is_empty())
    {
        doc.metadata_mut().subject = subj;
    }
    if let Some(notes) = extract_rdf_li_list(&roots, "crl:Notes")
        .into_iter()
        .find(|s| !s.is_empty())
    {
        doc.metadata_mut().notes = notes;
    }
    let kws = extract_rdf_li_list(&roots, "crl:Keywords");
    if !kws.is_empty() {
        doc.metadata_mut().keywords = kws;
    }
    if let (Some(w_str), Some(h_str)) = (
        extract_xml_tag_simple(&roots, "cdrinfo:PageWidth"),
        extract_xml_tag_simple(&roots, "cdrinfo:PageHeight"),
    ) {
        if let (Ok(w), Ok(h)) = (w_str.trim().parse::<i32>(), h_str.trim().parse::<i32>()) {
            let wm = sanitize_f64(cdr32_to_mm(w), 0.0);
            let hm = sanitize_f64(cdr32_to_mm(h), 0.0);
            if wm > 1.0 && hm > 1.0 {
                zcf_ctx.metadata_page_size_mm = Some((wm, hm));
            }
        }
    }
}

fn extract_xml_tag_simple(roots: &[XmlElem], tag: &str) -> Option<String> {
    let elem = find_first_elem(roots, tag)?;
    Some(elem.text.trim().to_owned())
}

fn extract_rdf_li_list(roots: &[XmlElem], container_tag: &str) -> Vec<String> {
    let Some(container) = find_first_elem(roots, container_tag) else {
        return Vec::new();
    };
    let mut li_elems = Vec::new();
    collect_elems(&container.children, "rdf:li", &mut li_elems);
    li_elems
        .into_iter()
        .map(|el| el.text.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

// =============================================================================
// Bounded CDR Preview PNG Extraction (ZIP `previews/*.png`, `.bmp`, RIFF `DISP`)
// =============================================================================

/// Extracts an embedded preview image as valid PNG bytes from any `.cdr`/`.cdt`
/// container (ZIP/OPC X4–2025 or classic RIFF v4–X3), strictly enforcing [`CodecLimits`].
///
/// Extraction priority:
/// 1. ZIP `previews/thumbnail.png`
/// 2. ZIP `previews/page1.png` (or first `previews/page*.png`)
/// 3. ZIP `metadata/thumbnails/thumbnail.png` / `preview/thumbnail.png`
/// 4. ZIP `preview/thumbnail.bmp` / `previews/thumbnail.bmp` (decoded & re-encoded as PNG)
/// 5. ZIP `content/riffData.cdr` `DISP` chunk (DIB -> PNG)
/// 6. Classic RIFF `DISP` chunk (strips optional 4-byte `CF_DIB` tag, decodes DIB -> PNG)
///
/// Returns `Ok(None)` if the file is valid but carries no embedded preview image.
pub fn extract_cdr_preview_png(bytes: &[u8], limits: &CodecLimits) -> CdrResult<Option<Vec<u8>>> {
    if is_zip_bytes(bytes) {
        return extract_zip_preview_png(bytes, limits);
    }

    if bytes.len() >= 12 && (&bytes[0..4] == b"RIFF" || &bytes[0..4] == b"RIFX") {
        let form = [bytes[8], bytes[9], bytes[10], bytes[11]];
        if is_cdr_form_type(&form) || is_cdt_form_type(&form) || is_cmx_form_type(&form) {
            let mut budget = limits.max_uncompressed_bytes;
            let tree = parse_riff_with_budget(bytes, limits, &mut budget, true)?;
            if let Some(disp) = tree
                .find_chunk(b"DISP")
                .or_else(|| tree.find_chunk(b"disp"))
            {
                return decode_disp_to_png(disp, limits, &mut budget).map(Some);
            }
            return Ok(None);
        }
    }

    Err(CdrError::UnsupportedFormat(
        "input bytes do not match a supported CDR/CDT container for preview extraction".to_owned(),
    ))
}

fn extract_zip_preview_png(bytes: &[u8], limits: &CodecLimits) -> CdrResult<Option<Vec<u8>>> {
    let archive_len = bytes.len() as u64;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| CdrError::ZipError(format!("invalid ZIP: {e}")))?;

    let entry_count = archive.len();
    limits.check_zip_entries(entry_count)?;

    let mut index_by_path: HashMap<String, usize> = HashMap::with_capacity(entry_count);
    let mut declared_total: u64 = 0;
    let mut declared_comp_total: u64 = 0;

    for i in 0..entry_count {
        let entry = archive
            .by_index(i)
            .map_err(|e| CdrError::ZipError(format!("ZIP entry #{i}: {e}")))?;
        let clean = sanitize_zip_path(entry.name_raw())?;
        let comp = entry.compressed_size();
        let uncomp = entry.size();
        if !entry.is_dir() {
            if comp > archive_len {
                return Err(CdrError::ZipError(format!(
                    "forged ZIP compressed_size ({comp}) exceeds archive length ({archive_len})"
                )));
            }
            declared_comp_total = declared_comp_total
                .checked_add(comp)
                .ok_or(CdrError::ArithmeticOverflow("ZIP compressed total"))?;
            if declared_comp_total > archive_len {
                return Err(CdrError::ZipError(format!(
                    "cumulative ZIP compressed sizes ({declared_comp_total}) exceed archive length ({archive_len})"
                )));
            }
            if uncomp > limits.max_uncompressed_bytes {
                return Err(CdrError::UncompressedSizeLimitExceeded {
                    limit: limits.max_uncompressed_bytes,
                });
            }
            limits.check_compression_ratio(comp, uncomp)?;
            declared_total = declared_total
                .checked_add(uncomp)
                .ok_or(CdrError::ArithmeticOverflow("ZIP uncompressed total"))?;
            if declared_total > limits.max_uncompressed_bytes {
                return Err(CdrError::UncompressedSizeLimitExceeded {
                    limit: limits.max_uncompressed_bytes,
                });
            }
        }
        let lower = clean.to_ascii_lowercase();
        if index_by_path.insert(lower, i).is_some() {
            return Err(CdrError::ZipError(format!(
                "duplicate ZIP entry path '{clean}'"
            )));
        }
    }

    let mut budget = limits.max_uncompressed_bytes;

    // 1. Direct embedded PNG preview candidates in priority order
    let mut png_candidates: Vec<&str> = vec![
        "previews/thumbnail.png",
        "previews/page1.png",
        "metadata/thumbnails/thumbnail.png",
        "preview/thumbnail.png",
        "thumbnail.png",
    ];
    let mut extra_pages: Vec<String> = index_by_path
        .keys()
        .filter(|k| k.starts_with("previews/page") && k.ends_with(".png"))
        .cloned()
        .collect();
    extra_pages.sort();
    for p in &extra_pages {
        if !png_candidates.contains(&p.as_str()) {
            png_candidates.push(p.as_str());
        }
    }

    for cand in png_candidates {
        if let Some(&idx) = index_by_path.get(cand) {
            let entry = archive
                .by_index(idx)
                .map_err(|e| CdrError::ZipError(format!("ZIP read '{cand}': {e}")))?;
            let comp = entry.compressed_size().min(archive_len);
            let data = read_bounded(entry, limits, &mut budget, Some(comp))?;
            validate_png_header(&data, limits)?;
            return Ok(Some(data));
        }
    }

    // 2. Embedded BMP preview candidates (`preview/thumbnail.bmp`, `previews/thumbnail.bmp`)
    for cand in [
        "preview/thumbnail.bmp",
        "previews/thumbnail.bmp",
        "metadata/thumbnails/thumbnail.bmp",
    ] {
        if let Some(&idx) = index_by_path.get(cand) {
            let entry = archive
                .by_index(idx)
                .map_err(|e| CdrError::ZipError(format!("ZIP read '{cand}': {e}")))?;
            let comp = entry.compressed_size().min(archive_len);
            let bmp_bytes = read_bounded(entry, limits, &mut budget, Some(comp))?;
            let (w, h, rgba) = decode_dib_to_rgba_with_budget(&bmp_bytes, limits, &mut budget)?;
            return encode_png_rgba(w, h, &rgba).map(Some);
        }
    }

    // 3. Fallback: X4–X5 `content/riffData.cdr` `DISP` chunk
    if let Some(&idx) = index_by_path.get("content/riffdata.cdr") {
        let entry = archive
            .by_index(idx)
            .map_err(|e| CdrError::ZipError(format!("ZIP read riffData.cdr: {e}")))?;
        let comp = entry.compressed_size().min(archive_len);
        let riff_bytes = read_bounded(entry, limits, &mut budget, Some(comp))?;
        let tree = parse_riff_with_budget(&riff_bytes, limits, &mut budget, false)?;
        if let Some(disp) = tree.find_chunk(b"DISP") {
            return decode_disp_to_png(disp, limits, &mut budget).map(Some);
        }
    }

    Ok(None)
}

fn decode_disp_to_png(disp: &[u8], limits: &CodecLimits, budget: &mut u64) -> CdrResult<Vec<u8>> {
    // Classic CDR `DISP` chunks prepend a 4-byte Windows clipboard format (`8` = `CF_DIB`).
    let dib_slice = if disp.len() >= 44 {
        let cf_tag = u32::from_le_bytes([disp[0], disp[1], disp[2], disp[3]]);
        let next_hdr = u32::from_le_bytes([disp[4], disp[5], disp[6], disp[7]]);
        if cf_tag == 8 && (40..=256).contains(&next_hdr) {
            &disp[4..]
        } else {
            disp
        }
    } else {
        disp
    };
    let (w, h, rgba) = decode_dib_to_rgba_with_budget(dib_slice, limits, budget)?;
    encode_png_rgba(w, h, &rgba)
}

/// Validates PNG signature and IHDR dimensions against [`CodecLimits::validate_bitmap_dims`].
pub(crate) fn validate_png_header(png: &[u8], limits: &CodecLimits) -> CdrResult<(u32, u32)> {
    if png.len() < 33 || &png[0..8] != PNG_SIGNATURE {
        return Err(CdrError::InvalidChunk {
            fourcc: "PNG ".to_owned(),
            reason: "invalid PNG signature or truncated IHDR".to_owned(),
        });
    }
    let mut r = ByteReader::new(&png[8..]);
    let ihdr_len = r.read_u32_be("PNG IHDR len")?;
    let ihdr_tag = r.read_fourcc("PNG IHDR tag")?;
    if ihdr_len != 13 || &ihdr_tag != b"IHDR" {
        return Err(CdrError::InvalidChunk {
            fourcc: "PNG ".to_owned(),
            reason: "missing or malformed PNG IHDR chunk".to_owned(),
        });
    }
    let width = r.read_u32_be("PNG width")?;
    let height = r.read_u32_be("PNG height")?;
    limits.validate_bitmap_dims(width, height)?;
    Ok((width, height))
}

/// Pure-Rust RGBA8 PNG encoder (`IHDR` + filtered `IDAT` Zlib + `IEND` with IEEE CRC-32).
pub(crate) fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> CdrResult<Vec<u8>> {
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|w| w.checked_mul(4))
        .ok_or(CdrError::ArithmeticOverflow("PNG row bytes"))?;
    let expected = usize::try_from(height)
        .ok()
        .and_then(|h| h.checked_mul(row_bytes))
        .ok_or(CdrError::ArithmeticOverflow("PNG RGBA bytes"))?;
    if width == 0 || height == 0 || rgba.len() != expected {
        return Err(CdrError::InvalidChunk {
            fourcc: "PNG ".to_owned(),
            reason: format!(
                "invalid RGBA buffer len {} for {}x{} PNG",
                rgba.len(),
                width,
                height
            ),
        });
    }

    let raw_stride = row_bytes
        .checked_add(1)
        .ok_or(CdrError::ArithmeticOverflow("PNG scanline stride"))?;
    let raw_len = raw_stride
        .checked_mul(height as usize)
        .ok_or(CdrError::ArithmeticOverflow("PNG raw scanline buffer"))?;
    let mut raw = Vec::with_capacity(raw_len);
    for y in 0..height as usize {
        raw.push(0u8); // Filter type 0 (None)
        let start = y * row_bytes;
        raw.extend_from_slice(&rgba[start..start + row_bytes]);
    }

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&raw)
        .map_err(|e| CdrError::DecompressionFailed(format!("PNG zlib write failed: {e}")))?;
    let idat = enc
        .finish()
        .map_err(|e| CdrError::DecompressionFailed(format!("PNG zlib finish failed: {e}")))?;

    let mut out = Vec::with_capacity(64usize.saturating_add(idat.len()));
    out.extend_from_slice(PNG_SIGNATURE);

    // IHDR (13 bytes: width, height, bit_depth=8, color_type=6 RGBA, comp=0, filter=0, interlace=0)
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = 6;
    ihdr[10] = 0;
    ihdr[11] = 0;
    ihdr[12] = 0;
    write_png_chunk(&mut out, b"IHDR", &ihdr)?;
    write_png_chunk(&mut out, b"IDAT", &idat)?;
    write_png_chunk(&mut out, b"IEND", &[])?;
    Ok(out)
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) -> CdrResult<()> {
    let len = u32::try_from(data.len())
        .map_err(|_| CdrError::ArithmeticOverflow("PNG chunk length exceeds u32"))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let crc = png_crc32(chunk_type, data);
    out.extend_from_slice(&crc.to_be_bytes());
    Ok(())
}

fn png_crc32(chunk_type: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in chunk_type.iter().chain(data.iter()) {
        crc ^= u32::from(b);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::decompress_zlib_bounded;

    #[test]
    fn parse_cdr_color_string_all_models() {
        let black =
            parse_cdr_color_string("CMYK,USER,0,0,0,100,100,00000000-0000-0000-0000-000000000000")
                .unwrap();
        assert_eq!(black, Color::cmyk(0.0, 0.0, 0.0, 1.0));

        let red = parse_cdr_color_string(
            "CMYK255,USER,0,255,255,0,100,cccd19cb-4675-4a5e-8bda-d0bbbaab8af0",
        )
        .unwrap();
        assert_eq!(red, Color::cmyk(0.0, 1.0, 1.0, 0.0));

        let rgb = parse_cdr_color_string("RGB255,USER,255,128,0,50,0000").unwrap();
        match rgb {
            Color::Rgba { r, g, b, a } => {
                assert!((r - 1.0).abs() < 1e-3);
                assert!((g - 128.0 / 255.0).abs() < 1e-3);
                assert!(b.abs() < 1e-3);
                assert!((a - 0.5).abs() < 1e-3);
            }
            other => panic!("expected Rgba, got {other:?}"),
        }

        let gray = parse_cdr_color_string("GRAY,USER,50,100,0000").unwrap();
        assert!(matches!(gray, Color::Gray { v, .. } if (v - 0.5).abs() < 1e-3));

        let lab = parse_cdr_color_string("LAB,USER,60,25,-30,100,0000").unwrap();
        assert!(matches!(lab, Color::Lab { l, a_ch, b_ch, .. }
            if (l - 60.0).abs() < 1e-3 && (a_ch - 25.0).abs() < 1e-3 && (b_ch + 30.0).abs() < 1e-3));
    }

    #[test]
    fn parse_document_cdss_and_c9_overrides() {
        let cdss_json = br#"{
            "version": 2300,
            "style": {
                "~graphic~": {
                    "fill": { "primaryColor": "CMYK,USER,0,0,0,100,100,0000" },
                    "outline": { "width": "762", "color": "CMYK255,USER,0,255,255,0,100,0000" }
                },
                "~artistic~": {
                    "character": {
                        "fill": { "type": "1", "primaryColor": "CMYK,USER,0,0,0,100,100,0000" },
                        "RTL": { "font": "Noto Naskh Arabic", "size": "84667", "charset": "178", "weight": "700" }
                    }
                }
            }
        }"#;
        let sheet = parse_document_cdss(cdss_json);
        assert_eq!(sheet.version, Some(2300));
        let graphic = sheet.default_graphic().unwrap();
        // No explicit fill "type" -> explicit_fill() must be None!
        assert!(graphic.explicit_fill().is_none());
        assert!((graphic.outline_width_mm.unwrap() - 0.0762).abs() < 1e-5);
        let art = sheet.default_artistic_text().unwrap();
        assert_eq!(art.font_family, "Noto Naskh Arabic");
        assert_eq!(art.direction, TextDirection::Rtl);
        assert!(art.bold);
        assert!((art.font_size_pt - 24.0).abs() < 0.05);

        // `0x00C9` JSON with explicit solid fill (`type="1"`)
        let c9_json = br#"{"fill":{"type":"1","primaryColor":"CMYK,USER,100,0,0,0,100,0000"},"outline":{"width":"2500","color":"CMYK,USER,0,0,0,100,100,0000","endCaps":"1","joinType":"2"}}"#;
        let mut raw = Vec::new();
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&(c9_json.len() as u32).to_le_bytes());
        raw.extend_from_slice(c9_json);
        let parsed = parse_c9_style_override(&raw).unwrap();
        assert_eq!(parsed.explicit_fill(), Some(Fill::Solid(Color::CYAN)));
        let mut outl = OutlineStyle::none();
        parsed.apply_outline_overrides(&mut outl, false);
        assert!((outl.width_mm - 0.25).abs() < 1e-5);
        assert_eq!(outl.cap, LineCap::Round);
        assert_eq!(outl.join, LineJoin::Bevel);
    }

    #[test]
    fn parse_doc_palette_and_textinfo_and_xmp() {
        let pal_xml = r#"<?xml version="1.0"?>
<palette name="Document Palette"><colors><page>
<color cs="CMYK" name="Black" tints="0,0,0,1"/>
<color cs="CMYK" name="Red" tints="0,1,1,0"/>
<color cs="CMYK" name="Dark Brown" tints="0,0.2,0.2,0.6"/>
<color cs="CMYK" name="White" tints="0,0,0,0"/>
<color cs="CMYK" name="Yellow" tints="0,0,1,0"/>
</page></colors></palette>"#;
        let swatches = parse_doc_palette_xml(pal_xml);
        assert_eq!(swatches.len(), 5);
        assert_eq!(swatches[0].name, "Black");
        assert_eq!(swatches[1].name, "Red");
        assert_eq!(swatches[2].name, "Dark Brown");
        assert_eq!(swatches[2].color, Color::cmyk(0.0, 0.2, 0.2, 0.6));
        assert_eq!(swatches[3].name, "White");
        assert_eq!(swatches[4].name, "Yellow");

        let cc_xml = "<Color><ColorContext><ColorModel>Cmyk</ColorModel></ColorContext></Color>";
        assert_eq!(parse_color_context_xml(cc_xml), Some(ColorMode::Cmyk));

        let ti_xml = r#"<x:xmpmeta><rdf:RDF><rdf:Description>
            <LayerNames><rdf:Bag><rdf:li>Artwork</rdf:li><rdf:li>Cut</rdf:li></rdf:Bag></LayerNames>
            <PageNames><rdf:Bag><rdf:li>Cover</rdf:li></rdf:Bag></PageNames>
        </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let ti = parse_textinfo_xml(ti_xml);
        assert_eq!(ti.layer_names, vec!["Artwork", "Cut"]);
        assert_eq!(ti.page_names, vec!["Cover"]);
    }

    #[test]
    fn encode_png_rgba_produces_valid_ihdr_crc_and_zlib_idat() {
        let rgba = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 128,
        ];
        let png = encode_png_rgba(2, 2, &rgba).unwrap();
        let (w, h) = validate_png_header(&png, &CodecLimits::default()).unwrap();
        assert_eq!((w, h), (2, 2));

        // Walk every chunk and verify IEEE CRC-32 + IDAT Zlib scanlines
        let mut pos = 8usize;
        let mut found_idat = false;
        let mut found_iend = false;
        while pos + 12 <= png.len() {
            let len =
                u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
            let tag = [png[pos + 4], png[pos + 5], png[pos + 6], png[pos + 7]];
            let payload = &png[pos + 8..pos + 8 + len];
            let crc_stored = u32::from_be_bytes([
                png[pos + 8 + len],
                png[pos + 9 + len],
                png[pos + 10 + len],
                png[pos + 11 + len],
            ]);
            assert_eq!(
                crc_stored,
                png_crc32(&tag, payload),
                "CRC mismatch on {tag:?}"
            );
            if &tag == b"IDAT" {
                found_idat = true;
                let mut budget = 64 * 1024;
                let raw =
                    decompress_zlib_bounded(payload, &CodecLimits::default(), &mut budget).unwrap();
                assert_eq!(raw.len(), 2 * (1 + 2 * 4));
                assert_eq!(raw[0], 0);
                assert_eq!(&raw[1..9], &rgba[0..8]);
                assert_eq!(raw[9], 0);
                assert_eq!(&raw[10..18], &rgba[8..16]);
            } else if &tag == b"IEND" {
                found_iend = true;
            }
            pos += 12 + len;
        }
        assert!(found_idat && found_iend);
    }

    #[test]
    fn zcf_pointer_aliasing_bomb_and_oob_rejected() {
        // 1. Aliasing bomb: 100 pointers all pointing at the same 4096-byte slice under 32 KiB limit
        let blob = vec![0xABu8; 4096];
        let mut ptr = Vec::with_capacity(16);
        ptr.extend_from_slice(&0u32.to_le_bytes()); // file_index = 0
        ptr.extend_from_slice(&4096u32.to_le_bytes()); // data_len = 4096
        ptr.extend_from_slice(&0u32.to_le_bytes()); // offset = 0
        ptr.extend_from_slice(&0u32.to_le_bytes()); // reserved = 0

        let chunks: Vec<RiffNode> = (0..100)
            .map(|_| RiffNode::chunk(*b"loda", ptr.clone()))
            .collect();
        let tree = RiffDocument::new(*b"CDRO", chunks);
        let tight = CodecLimits {
            max_uncompressed_bytes: 32 * 1024,
            ..CodecLimits::default()
        };
        let mut budget = tight.max_uncompressed_bytes;
        let mut warnings = Vec::new();
        let err = resolve_zcf_riff_tree(
            tree,
            std::slice::from_ref(&blob),
            &[Some(0)],
            &tight,
            &mut budget,
            &mut warnings,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CdrError::UncompressedSizeLimitExceeded { .. }
        ));

        // 2. Out-of-bounds offset + arithmetic overflow rejected cleanly
        let mut oob_ptr = Vec::with_capacity(16);
        oob_ptr.extend_from_slice(&0u32.to_le_bytes());
        oob_ptr.extend_from_slice(&100u32.to_le_bytes());
        oob_ptr.extend_from_slice(&4050u32.to_le_bytes()); // 4050 + 100 > 4096
        oob_ptr.extend_from_slice(&0u32.to_le_bytes());
        let oob_tree = RiffDocument::new(*b"CDRO", vec![RiffNode::chunk(*b"loda", oob_ptr)]);
        let mut budget2 = CodecLimits::default().max_uncompressed_bytes;
        let err2 = resolve_zcf_riff_tree(
            oob_tree,
            &[blob],
            &[Some(0)],
            &CodecLimits::default(),
            &mut budget2,
            &mut warnings,
        )
        .unwrap_err();
        assert!(matches!(err2, CdrError::UnexpectedEof { .. }));
    }

    #[test]
    fn cdss_parent_inheritance_resolves_multi_level_and_guards_cycles() {
        let json = br##"{
          "version": 2300,
          "style": {
            "base": {
              "fill": { "type": "2", "fountainType": "radial", "primaryColor": "RGB,USER,255,0,0,100", "secondaryColor": "RGB,USER,0,0,255,100" },
              "outline": { "width": "10000", "overprint": true }
            },
            "child": {
              "parent": "base",
              "fill": { "angle": "45" }
            },
            "cycA": { "parent": "cycB", "outline": { "width": "5000" } },
            "cycB": { "parent": "cycA" }
          }
        }"##;
        let sheet = parse_document_cdss(json);
        let resolved = sheet.resolve("child").unwrap();
        assert_eq!(resolved.fill_type, Some(2));
        assert_eq!(resolved.fill_fountain_type, Some(1));
        assert_eq!(resolved.fill_angle_deg, Some(45.0));
        assert_eq!(resolved.outline_overprint, Some(true));
        assert!(matches!(
            resolved.explicit_fill(),
            Some(Fill::RadialGradient { .. })
        ));

        let cyc = sheet.resolve("cycA").unwrap();
        assert!((cyc.outline_width_mm.unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn cdss_parses_latin_and_rtl_character_blocks_with_numeric_weight() {
        let json = br##"{
          "version": 2400,
          "style": {
            "heading": {
              "character": {
                "latin": { "font": "Inter", "weight": 700, "size": "84667" },
                "RTL": { "font": "Noto Sans Arabic", "charset": 178 }
              }
            }
          }
        }"##;
        let sheet = parse_document_cdss(json);
        let style = sheet.resolve("heading").unwrap();
        let ts = style.text_style.expect("character TextStyle parsed");
        assert_eq!(ts.font_family, "Inter");
        assert_eq!(ts.effective_weight(), 700);
        assert!(ts.bold);
        assert_eq!(ts.bidi_font_family.as_deref(), Some("Noto Sans Arabic"));
        assert!((ts.font_size_pt - 24.0).abs() < 0.2);
    }

    #[test]
    fn oob_file_index_errors_strictly() {
        let mut ptr = Vec::with_capacity(16);
        ptr.extend_from_slice(&99u32.to_le_bytes()); // out-of-bounds file_index
        ptr.extend_from_slice(&24u32.to_le_bytes());
        ptr.extend_from_slice(&0u32.to_le_bytes());
        ptr.extend_from_slice(&0u32.to_le_bytes());

        let tree = RiffDocument::new(*b"CDRO", vec![RiffNode::chunk(*b"loda", ptr)]);
        let mut budget = CodecLimits::default().max_uncompressed_bytes;
        let mut warnings = Vec::new();
        let err = resolve_zcf_riff_tree(
            tree,
            &[],
            &[],
            &CodecLimits::default(),
            &mut budget,
            &mut warnings,
        )
        .unwrap_err();
        assert!(matches!(err, CdrError::InvalidChunk { .. }));
    }

    #[test]
    fn xml_parsers_reject_doctype_and_handle_comments_entities_and_attr_gt() {
        let dtd_xml = r#"<?xml version="1.0"?>
<!DOCTYPE palette [ <!ENTITY bomb "AAAA"> ]>
<palette><color cs="RGB" name="Red" tints="255,0,0"/></palette>"#;
        assert!(parse_doc_palette_xml(dtd_xml).is_empty());

        let pal_xml = r#"<?xml version="1.0"?>
<!-- <color cs="RGB" name="Ignored" tints="0,0,0"/> -->
<palette><color cs="SPOT" name="A &amp; B &gt; C" tints="0,100,0,0"/></palette>"#;
        let swatches = parse_doc_palette_xml(pal_xml);
        assert_eq!(swatches.len(), 1);
        assert_eq!(swatches[0].name, "A & B > C");
        assert_eq!(
            swatches[0].color,
            Color::spot("A & B > C", 1.0, [0.0, 1.0, 0.0, 0.0])
        );

        let tricky_ti = r#"<x:xmpmeta><rdf:RDF><rdf:Description>
<!-- <PageNames><rdf:Bag><rdf:li>CommentedOut</rdf:li></rdf:Bag></PageNames> -->
<PageNames><rdf:Bag><rdf:li>P1 &amp; &lt;/rdf:li&gt;</rdf:li></rdf:Bag></PageNames>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let ti = parse_textinfo_xml(tricky_ti);
        assert_eq!(ti.page_names, vec!["P1 & </rdf:li>"]);
    }

    #[test]
    fn spot_and_fixed_colors_decode_non_black_fallbacks() {
        let spot = parse_cdr_color_string("SPOT,PantoneMagenta,100,0,100,0,0").unwrap();
        assert_eq!(
            spot,
            Color::Spot {
                name: "PantoneMagenta".to_owned(),
                tint: 1.0,
                fallback_cmyk: [0.0, 1.0, 0.0, 0.0],
                alpha: 1.0,
            }
        );
        let fixed = parse_cdr_color_string("FIXED,WarmGold,50,0,25,100,0").unwrap();
        assert_eq!(
            fixed,
            Color::Spot {
                name: "WarmGold".to_owned(),
                tint: 0.5,
                fallback_cmyk: [0.0, 0.25, 1.0, 0.0],
                alpha: 1.0,
            }
        );
        let fallback_only = parse_cdr_color_string("SPOT,BrandCyan").unwrap();
        if let Color::Spot { fallback_cmyk, .. } = fallback_only {
            assert_ne!(fallback_cmyk, [0.0, 0.0, 0.0, 1.0]);
        } else {
            panic!("expected Spot");
        }
    }

    #[test]
    fn extract_cdr_preview_png_supports_cmx_disp() {
        let dib = crate::cdr_riff::encode_dib_rgba(2, 2, &[200u8; 16]).unwrap();
        let tree = RiffDocument::new(*b"CMX2", vec![RiffNode::chunk(*b"disp", dib)]);
        let cmx_bytes = crate::riff::write_riff(&tree).unwrap();
        let png = extract_cdr_preview_png(&cmx_bytes, &CodecLimits::default())
            .unwrap()
            .expect("cmx disp preview");
        assert!(png.starts_with(PNG_SIGNATURE));
    }
}
