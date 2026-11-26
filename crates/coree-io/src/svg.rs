// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hardened SVG import (via `usvg` with depth/size limits, converting into millimeter
//! `coree_core::Document`) and clean pure-Rust SVG 1.1 export preserving layers, `<defs>`
//! gradients, vector paths, styling, RTL/Arabic text, and embedded rasters.

use crate::atomic::atomic_write_bytes;
use crate::display_list::{DisplayListOptions, DisplayOp, DisplayTextKind, ExportDisplayList};
use crate::error::{IoError, IoLimits, IoResult};
use crate::raster::{
    decode_raster_bytes_with_limits, export_raster_to_bytes, RasterExportOptions, RasterFormat,
};
use coree_core::{
    encode_base64, Affine2D, ArtisticText, BezierPath, BlendMode, Color, ColorMode, Document, Fill,
    GradientStop, LayerId, LineCap, LineJoin, MasterLayerScope, NodePayload, ObjectId,
    OutlineStyle, Point2D, RasterLayerData, SceneObject, TextAlign, TextDirection, TextOnPath,
    TextRun, TextStyle, Unit,
};
use coree_render::{outline_arrowhead_geometries, FillRule, TextEngine};
use kurbo::{BezPath, PathEl};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use usvg::tiny_skia_path::PathSegment;

/// Canonical SVG CSS pixel-to-millimeter factor at 96 DPI (`25.4 / 96.0`).
const PX_TO_MM_96: f64 = 25.4 / 96.0;
/// Maximum expanded SVG scene objects allowed during import.
pub const MAX_SVG_OBJECTS: usize = 100_000;
/// Maximum expanded SVG path vertices/segments allowed during import.
pub const MAX_SVG_VERTICES: usize = 2_000_000;

fn cached_system_fontdb() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

/// Imports an SVG (or `.svgz`) file from `path` using default [`IoLimits`].
pub fn import_svg(path: &Path) -> IoResult<Document> {
    import_svg_with_limits(path, &IoLimits::default())
}

/// Imports an SVG (or `.svgz`) file from `path` enforcing `limits`.
pub fn import_svg_with_limits(path: &Path, limits: &IoLimits) -> IoResult<Document> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut raw = Vec::new();
    file.take((limits.max_svg_bytes as u64).saturating_add(1))
        .read_to_end(&mut raw)?;
    if raw.len() > limits.max_svg_bytes {
        return Err(IoError::LimitExceeded {
            limit: format!("max_svg_bytes={}", limits.max_svg_bytes),
            actual: format!("file exceeds {} bytes", limits.max_svg_bytes),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported SVG");

    if raw.starts_with(&[0x1f, 0x8b]) {
        // Decompress SVGZ incrementally, enforcing BOTH max_svg_bytes AND max_compression_ratio.
        let compressed_len = raw.len().max(1) as f64;
        let mut decoder = flate2::read::GzDecoder::new(raw.as_slice());
        let mut decompressed = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = decoder.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            let next_len = decompressed.len().saturating_add(n);
            if next_len > limits.max_svg_bytes {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_svg_bytes={}", limits.max_svg_bytes),
                    actual: format!("exceeds {} bytes (svgz)", limits.max_svg_bytes),
                });
            }
            if (next_len as u64) >= limits.min_ratio_check_bytes {
                let ratio = (next_len as f64) / compressed_len;
                if ratio > limits.max_compression_ratio {
                    return Err(IoError::ZipBomb {
                        reason: format!(
                            "svgz decompression ratio {ratio:.1}x exceeds limit {:.1}x ({next_len}B / {}B)",
                            limits.max_compression_ratio,
                            raw.len()
                        ),
                    });
                }
            }
            decompressed.extend_from_slice(&chunk[..n]);
        }
        let text = String::from_utf8(decompressed).map_err(|e| IoError::SvgParse(e.to_string()))?;
        return import_svg_str_named(&text, title, path.parent(), limits);
    }

    let text = String::from_utf8(raw).map_err(|e| IoError::SvgParse(e.to_string()))?;
    import_svg_str_named(&text, title, path.parent(), limits)
}

/// Imports an SVG XML string into a millimeter [`Document`] using default [`IoLimits`].
pub fn import_svg_str(svg: &str) -> IoResult<Document> {
    import_svg_str_with_limits(svg, &IoLimits::default())
}

/// Imports an SVG XML string into a millimeter [`Document`] enforcing `limits`.
pub fn import_svg_str_with_limits(svg: &str, limits: &IoLimits) -> IoResult<Document> {
    import_svg_str_named(svg, "Imported SVG", None, limits)
}

fn import_svg_str_named(
    svg: &str,
    title: &str,
    resources_dir: Option<&Path>,
    limits: &IoLimits,
) -> IoResult<Document> {
    if svg.len() > limits.max_svg_bytes {
        return Err(IoError::LimitExceeded {
            limit: format!("max_svg_bytes={}", limits.max_svg_bytes),
            actual: format!("{} bytes", svg.len()),
        });
    }

    // Parse XML first to verify element nesting depth & extract `data-layer` metadata.
    let xml_doc = usvg::roxmltree::Document::parse_with_options(
        svg,
        usvg::roxmltree::ParsingOptions {
            allow_dtd: false,
            ..Default::default()
        },
    )
    .map_err(|e| IoError::SvgParse(e.to_string()))?;

    let observed_depth = compute_xml_depth(xml_doc.root());
    if observed_depth > limits.max_svg_depth {
        return Err(IoError::LimitExceeded {
            limit: format!("max_svg_depth={}", limits.max_svg_depth),
            actual: format!("depth={observed_depth}"),
        });
    }

    precheck_svg_expansion_limits(&xml_doc, limits.max_svg_depth)?;

    let layer_meta_by_xml_id = collect_svg_layer_metadata(&xml_doc);

    let mut opts = usvg::Options {
        resources_dir: resources_dir.map(Path::to_path_buf),
        dpi: 96.0,
        font_family: "Noto Sans".to_owned(),
        fontdb: cached_system_fontdb(),
        max_depth: limits.max_svg_depth,
        ..usvg::Options::default()
    };
    // Ensure deterministic DPI for unit conversion
    opts.dpi = 96.0;

    let tree =
        usvg::Tree::from_xmltree(&xml_doc, &opts).map_err(|e| IoError::SvgParse(e.to_string()))?;

    let width_mm = (f64::from(tree.size().width()) * PX_TO_MM_96).max(1.0);
    let height_mm = (f64::from(tree.size().height()) * PX_TO_MM_96).max(1.0);

    let mut doc = Document::new(title, width_mm, height_mm, ColorMode::Rgb, 96.0);
    doc.metadata_mut().source_format = Some("svg".to_owned());
    let page_id = doc.pages()[0].id;
    let default_layer_id = doc.pages()[0].layers[0].id;

    let mut created_named_layers = false;
    let mut imported_usvg_texts: HashMap<(String, String), Vec<ImportedUsvgText>> = HashMap::new();
    let mut xml_layer_ids: HashMap<String, LayerId> = HashMap::new();
    let mut budget = SvgImportBudget::default();

    // `usvg` resolves SVG viewBox/units into 96-DPI pixels via an unnamed root wrapper group.
    // Start with `scale(PX_TO_MM_96)` and unwrap unnamed synthetic root wrapper groups so
    // top-level `<g id="..." data-layer="...">` layers are detected and mapped to `Layer`s!
    let mut top_nodes: &[usvg::Node] = tree.root().children();
    let mut root_to_mm = Affine2D::scale_uniform(PX_TO_MM_96);
    while top_nodes.len() == 1 {
        if let usvg::Node::Group(g) = &top_nodes[0] {
            if g.id().is_empty()
                && (g.opacity().get() - 1.0).abs() < 1e-4
                && g.blend_mode() == usvg::BlendMode::Normal
                && g.clip_path().is_none()
            {
                root_to_mm = root_to_mm.compose(&usvg_transform_raw(g.transform()));
                top_nodes = g.children();
                continue;
            }
        }
        break;
    }

    for child in top_nodes {
        if let usvg::Node::Group(group) = child {
            if let Some(meta) = layer_meta_by_xml_id.get(group.id()) {
                let is_cut = meta.is_cut_layer || meta.name.eq_ignore_ascii_case("cutcontour");
                let lid =
                    doc.add_layer(page_id, meta.name.clone(), is_cut, MasterLayerScope::None)?;
                xml_layer_ids.insert(group.id().to_owned(), lid);
                created_named_layers = true;
                if group.clip_path().is_some() {
                    import_usvg_node(
                        &mut doc,
                        child,
                        Some(lid),
                        root_to_mm,
                        1.0,
                        0,
                        limits,
                        &mut budget,
                        &mut imported_usvg_texts,
                    )?;
                } else {
                    let group_to_mm = root_to_mm.compose(&usvg_transform_raw(group.transform()));
                    for sub in group.children() {
                        import_usvg_node(
                            &mut doc,
                            sub,
                            Some(lid),
                            group_to_mm,
                            group.opacity().get(),
                            0,
                            limits,
                            &mut budget,
                            &mut imported_usvg_texts,
                        )?;
                    }
                }
                continue;
            }
        }
        import_usvg_node(
            &mut doc,
            child,
            Some(default_layer_id),
            root_to_mm,
            1.0,
            0,
            limits,
            &mut budget,
            &mut imported_usvg_texts,
        )?;
    }

    // Recover any raw `<text>` nodes that `usvg` dropped (e.g., missing system fonts such as
    // Helvetica/Arial in mixed-font SVGs), skipping only `<text>` elements already imported by `usvg`.
    import_fallback_xml_texts(
        &mut doc,
        &xml_doc,
        page_id,
        default_layer_id,
        &layer_meta_by_xml_id,
        &mut xml_layer_ids,
        &mut created_named_layers,
        &mut budget,
        &mut imported_usvg_texts,
    )?;

    // Release intermediate `usvg` and XML trees before refreshing palette / returning.
    drop(tree);
    drop(xml_doc);

    // Remove empty placeholder "Layer 1" when named SVG layers were created cleanly.
    if created_named_layers && doc.pages()[0].layers.len() > 1 {
        if let Some(pos) = doc.pages()[0]
            .layers
            .iter()
            .position(|l| l.id == default_layer_id && l.objects.is_empty())
        {
            doc.remove_page_layer(0, pos);
        }
    }

    doc.refresh_document_palette();
    Ok(doc)
}

fn compute_xml_depth(root: usvg::roxmltree::Node<'_, '_>) -> u32 {
    let mut max_depth = 0u32;
    let mut stack = vec![(root, 0u32)];
    while let Some((node, depth)) = stack.pop() {
        if depth > max_depth {
            max_depth = depth;
        }
        for child in node.children() {
            if child.is_element() {
                stack.push((child, depth.saturating_add(1)));
            }
        }
    }
    max_depth
}

struct SvgLayerMeta {
    name: String,
    is_cut_layer: bool,
}

fn collect_svg_layer_metadata(
    xml_doc: &usvg::roxmltree::Document<'_>,
) -> HashMap<String, SvgLayerMeta> {
    let mut map = HashMap::new();
    for node in xml_doc.descendants().filter(|n| n.is_element()) {
        if node.tag_name().name() != "g" {
            continue;
        }
        let Some(id) = node.attribute("id").filter(|s| !s.is_empty()) else {
            continue;
        };
        let explicit_cut = node
            .attribute("data-cut-layer")
            .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");
        if let Some(layer_name) = node.attribute("data-layer").filter(|s| !s.is_empty()) {
            map.insert(
                id.to_owned(),
                SvgLayerMeta {
                    name: layer_name.to_owned(),
                    is_cut_layer: explicit_cut,
                },
            );
            continue;
        }
        // Also recognize Inkscape layers (`inkscape:groupmode="layer"`)
        let is_inkscape_layer = node
            .attributes()
            .any(|a| a.name() == "groupmode" && a.value().eq_ignore_ascii_case("layer"));
        if is_inkscape_layer {
            let label = node
                .attributes()
                .find(|a| a.name() == "label")
                .map(|a| a.value())
                .filter(|s| !s.is_empty())
                .unwrap_or(id);
            map.insert(
                id.to_owned(),
                SvgLayerMeta {
                    name: label.to_owned(),
                    is_cut_layer: explicit_cut,
                },
            );
        }
    }
    map
}

#[derive(Default)]
struct SvgImportBudget {
    emitted_objects: usize,
    total_vertices: usize,
}

impl SvgImportBudget {
    fn check_and_record_object(&mut self, doc: &Document) -> IoResult<()> {
        let next = self.emitted_objects.saturating_add(1);
        if next > MAX_SVG_OBJECTS || doc.object_count() >= MAX_SVG_OBJECTS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_svg_objects={MAX_SVG_OBJECTS}"),
                actual: format!("SVG expanded object count exceeds {MAX_SVG_OBJECTS}"),
            });
        }
        self.emitted_objects = next;
        Ok(())
    }

    fn record_vertices(&mut self, count: usize) -> IoResult<()> {
        let next = self.total_vertices.saturating_add(count);
        if next > MAX_SVG_VERTICES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_svg_vertices={MAX_SVG_VERTICES}"),
                actual: format!(
                    "SVG expanded vertex/segment count {next} exceeds {MAX_SVG_VERTICES}"
                ),
            });
        }
        self.total_vertices = next;
        Ok(())
    }
}

fn emit_svg_object(
    doc: &mut Document,
    obj: SceneObject,
    target_layer: Option<LayerId>,
    budget: &mut SvgImportBudget,
) -> IoResult<ObjectId> {
    budget.check_and_record_object(doc)?;
    if let Some(lid) = target_layer {
        Ok(doc.add_object(obj, Some(lid))?)
    } else {
        Ok(doc.insert_loaded_object(obj)?)
    }
}

fn scan_svg_number(bytes: &[u8], idx: &mut usize) -> bool {
    let len = bytes.len();
    while *idx < len && (bytes[*idx].is_ascii_whitespace() || bytes[*idx] == b',') {
        *idx += 1;
    }
    if *idx >= len {
        return false;
    }
    let start = *idx;
    if matches!(bytes[*idx], b'+' | b'-') {
        *idx += 1;
    }
    let mut has_digit = false;
    while *idx < len && bytes[*idx].is_ascii_digit() {
        has_digit = true;
        *idx += 1;
    }
    if *idx < len && bytes[*idx] == b'.' {
        *idx += 1;
        while *idx < len && bytes[*idx].is_ascii_digit() {
            has_digit = true;
            *idx += 1;
        }
    }
    if !has_digit {
        *idx = start;
        return false;
    }
    if *idx < len && matches!(bytes[*idx], b'e' | b'E') {
        let exp_pos = *idx;
        *idx += 1;
        if *idx < len && matches!(bytes[*idx], b'+' | b'-') {
            *idx += 1;
        }
        let mut exp_digits = false;
        while *idx < len && bytes[*idx].is_ascii_digit() {
            exp_digits = true;
            *idx += 1;
        }
        if !exp_digits {
            *idx = exp_pos;
        }
    }
    true
}

fn scan_svg_arc_flag(bytes: &[u8], idx: &mut usize) -> bool {
    let len = bytes.len();
    while *idx < len && (bytes[*idx].is_ascii_whitespace() || bytes[*idx] == b',') {
        *idx += 1;
    }
    if *idx < len && matches!(bytes[*idx], b'0' | b'1') {
        *idx += 1;
        true
    } else {
        false
    }
}

fn estimate_svg_path_vertices(d: &str) -> usize {
    let bytes = d.as_bytes();
    let len = bytes.len();
    let mut idx = 0usize;
    let mut total = 0usize;

    while idx < len {
        while idx < len && (bytes[idx].is_ascii_whitespace() || bytes[idx] == b',') {
            idx += 1;
        }
        if idx >= len {
            break;
        }
        let cmd = bytes[idx];
        if !cmd.is_ascii_alphabetic() {
            idx += 1;
            continue;
        }
        idx += 1;

        match cmd {
            b'Z' | b'z' => {}
            b'H' | b'h' | b'V' | b'v' => {
                while scan_svg_number(bytes, &mut idx) {
                    total = total.saturating_add(1);
                }
            }
            b'M' | b'm' | b'L' | b'l' | b'T' | b't' => {
                let mut count = 0usize;
                while scan_svg_number(bytes, &mut idx) {
                    count += 1;
                }
                total = total.saturating_add(count / 2);
            }
            b'S' | b's' | b'Q' | b'q' => {
                let mut count = 0usize;
                while scan_svg_number(bytes, &mut idx) {
                    count += 1;
                }
                total = total.saturating_add(count / 4);
            }
            b'C' | b'c' => {
                let mut count = 0usize;
                while scan_svg_number(bytes, &mut idx) {
                    count += 1;
                }
                total = total.saturating_add(count / 6);
            }
            b'A' | b'a' => {
                while scan_svg_number(bytes, &mut idx) {
                    if !scan_svg_number(bytes, &mut idx)
                        || !scan_svg_number(bytes, &mut idx)
                        || !scan_svg_arc_flag(bytes, &mut idx)
                        || !scan_svg_arc_flag(bytes, &mut idx)
                        || !scan_svg_number(bytes, &mut idx)
                        || !scan_svg_number(bytes, &mut idx)
                    {
                        break;
                    }
                    total = total.saturating_add(1);
                }
            }
            _ => {}
        }
    }

    total
}

fn estimate_svg_points_count(points: &str) -> usize {
    let bytes = points.as_bytes();
    let mut idx = 0usize;
    let mut count = 0usize;
    while scan_svg_number(bytes, &mut idx) {
        count += 1;
        if idx < bytes.len()
            && !bytes[idx].is_ascii_whitespace()
            && bytes[idx] != b','
            && !matches!(bytes[idx], b'+' | b'-' | b'.' | b'0'..=b'9')
        {
            idx += 1;
        }
    }
    count / 2
}

fn extract_url_fragment(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix("url(")?.strip_suffix(')')?.trim();
    let unquoted = rest.trim_matches(&['\'', '"'][..]).trim();
    unquoted.strip_prefix('#').filter(|s| !s.is_empty())
}

fn svg_use_href_fragment<'a>(node: usvg::roxmltree::Node<'a, '_>) -> Option<&'a str> {
    let href = node.attribute("href").or_else(|| {
        node.attributes()
            .find(|a| a.name() == "href")
            .map(|a| a.value())
    })?;
    href.trim().strip_prefix('#').filter(|s| !s.is_empty())
}

fn check_svg_precheck_limits(objs: usize, verts: usize) -> IoResult<()> {
    if objs > MAX_SVG_OBJECTS {
        return Err(IoError::LimitExceeded {
            limit: format!("max_svg_objects={MAX_SVG_OBJECTS}"),
            actual: format!("SVG pre-expanded object count {objs} exceeds {MAX_SVG_OBJECTS}"),
        });
    }
    if verts > MAX_SVG_VERTICES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_svg_vertices={MAX_SVG_VERTICES}"),
            actual: format!(
                "SVG pre-expanded vertex/segment count {verts} exceeds {MAX_SVG_VERTICES}"
            ),
        });
    }
    Ok(())
}

struct SvgPrecheckCtx<'a, 'input> {
    id_to_node: HashMap<&'a str, usvg::roxmltree::Node<'a, 'input>>,
    memo: HashMap<(usvg::roxmltree::NodeId, bool), (usize, usize)>,
    clip_memo: HashMap<usvg::roxmltree::NodeId, usize>,
    visiting: HashSet<usvg::roxmltree::NodeId>,
    max_expand_depth: usize,
    max_svg_depth: u32,
}

impl<'a, 'input> SvgPrecheckCtx<'a, 'input> {
    fn clip_path_vertices(
        &mut self,
        cp_node: usvg::roxmltree::Node<'a, 'input>,
        depth: usize,
    ) -> IoResult<usize> {
        if depth > self.max_expand_depth {
            return Err(IoError::LimitExceeded {
                limit: format!("max_svg_depth={}", self.max_svg_depth),
                actual: format!("SVG clipPath depth {depth} exceeds cap"),
            });
        }
        if let Some(&cached) = self.clip_memo.get(&cp_node.id()) {
            return Ok(cached);
        }
        if !self.visiting.insert(cp_node.id()) {
            return Ok(0);
        }
        let mut verts = 0usize;
        for child in cp_node.children().filter(|c| c.is_element()) {
            let (_co, cv) = self.node_cost(child, true, depth + 1)?;
            verts = verts.saturating_add(cv);
            check_svg_precheck_limits(0, verts)?;
        }
        if let Some(nested_cid) =
            svg_element_attr(cp_node, "clip-path").and_then(extract_url_fragment)
        {
            if let Some(&nested_cp) = self.id_to_node.get(nested_cid) {
                let ncv = self.clip_path_vertices(nested_cp, depth + 1)?;
                verts = verts.saturating_add(ncv);
                check_svg_precheck_limits(0, verts)?;
            }
        }
        self.visiting.remove(&cp_node.id());
        self.clip_memo.insert(cp_node.id(), verts);
        Ok(verts)
    }

    fn node_cost(
        &mut self,
        node: usvg::roxmltree::Node<'a, 'input>,
        via_use: bool,
        depth: usize,
    ) -> IoResult<(usize, usize)> {
        if depth > self.max_expand_depth {
            return Err(IoError::LimitExceeded {
                limit: format!("max_svg_depth={}", self.max_svg_depth),
                actual: format!("SVG <use> expansion depth {depth} exceeds cap"),
            });
        }
        if svg_element_attr(node, "display").is_some_and(|v| v.eq_ignore_ascii_case("none")) {
            return Ok((0, 0));
        }
        let tag = node.tag_name().name();
        if !via_use && is_non_rendered_svg_tag(tag) {
            return Ok((0, 0));
        }
        if via_use && is_non_rendered_svg_tag(tag) && tag != "symbol" {
            return Ok((0, 0));
        }
        if let Some(&cached) = self.memo.get(&(node.id(), via_use)) {
            check_svg_precheck_limits(cached.0, cached.1)?;
            return Ok(cached);
        }
        if !self.visiting.insert(node.id()) {
            return Ok((0, 0));
        }

        let (mut objs, mut verts) = match tag {
            "g" | "a" | "switch" | "svg" | "symbol" => {
                let mut child_objs = 0usize;
                let mut child_verts = 0usize;
                let mut elem_children = 0usize;
                for child in node.children().filter(|c| c.is_element()) {
                    elem_children += 1;
                    let (co, cv) = self.node_cost(child, false, depth + 1)?;
                    child_objs = child_objs.saturating_add(co);
                    child_verts = child_verts.saturating_add(cv);
                    check_svg_precheck_limits(child_objs, child_verts)?;
                }
                if child_objs == 0 {
                    (0, 0)
                } else if tag == "g"
                    && elem_children == 1
                    && node.attribute("id").is_none_or(str::is_empty)
                    && svg_element_attr(node, "clip-path").is_none()
                {
                    (child_objs, child_verts)
                } else {
                    (1usize.saturating_add(child_objs), child_verts)
                }
            }
            "use" => {
                if let Some(target_id) = svg_use_href_fragment(node) {
                    if let Some(&target) = self.id_to_node.get(target_id) {
                        self.node_cost(target, true, depth + 1)?
                    } else {
                        (0, 0)
                    }
                } else {
                    (0, 0)
                }
            }
            "path" => {
                let v = node.attribute("d").map_or(0, estimate_svg_path_vertices);
                if v == 0 && node.attribute("d").is_none_or(|d| d.trim().is_empty()) {
                    (0, 0)
                } else {
                    (1, v)
                }
            }
            "rect" | "circle" | "ellipse" => (1, 4),
            "line" => (1, 2),
            "polyline" | "polygon" => {
                let v = node
                    .attribute("points")
                    .map_or(0, estimate_svg_points_count);
                if v == 0 {
                    (0, 0)
                } else {
                    (1, v)
                }
            }
            "text" | "image" => (1, 0),
            _ => (0, 0),
        };

        if objs > 0 {
            if let Some(cid) = svg_element_attr(node, "clip-path").and_then(extract_url_fragment) {
                if let Some(&cp_node) = self.id_to_node.get(cid) {
                    let cp_verts = self.clip_path_vertices(cp_node, depth + 1)?;
                    verts = verts.saturating_add(cp_verts);
                }
            }
        }
        check_svg_precheck_limits(objs, verts)?;
        // Cap `objs` if an empty wrapper didn't contribute.
        let _ = &mut objs;

        self.visiting.remove(&node.id());
        self.memo.insert((node.id(), via_use), (objs, verts));
        Ok((objs, verts))
    }
}

fn precheck_svg_expansion_limits(
    xml_doc: &usvg::roxmltree::Document<'_>,
    max_svg_depth: u32,
) -> IoResult<()> {
    let mut id_to_node = HashMap::new();
    for node in xml_doc.descendants().filter(|n| n.is_element()) {
        if let Some(id) = node.attribute("id").filter(|s| !s.is_empty()) {
            id_to_node.entry(id).or_insert(node);
        }
    }

    let mut ctx = SvgPrecheckCtx {
        id_to_node,
        memo: HashMap::new(),
        clip_memo: HashMap::new(),
        visiting: HashSet::new(),
        max_expand_depth: (max_svg_depth as usize).saturating_mul(4).max(512),
        max_svg_depth,
    };

    let root_el = xml_doc.root_element();
    let mut total_objs = 0usize;
    let mut total_verts = 0usize;
    for child in root_el.children().filter(|c| c.is_element()) {
        let (co, cv) = ctx.node_cost(child, false, 1)?;
        total_objs = total_objs.saturating_add(co);
        total_verts = total_verts.saturating_add(cv);
        check_svg_precheck_limits(total_objs, total_verts)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ImportedUsvgText {
    families_lower: Vec<String>,
    pos_mm: Point2D,
}

fn normalize_svg_text_key(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[allow(clippy::too_many_arguments)]
fn import_usvg_node(
    doc: &mut Document,
    node: &usvg::Node,
    target_layer: Option<LayerId>,
    inherited_xf: Affine2D,
    inherited_opacity: f32,
    depth: u32,
    limits: &IoLimits,
    budget: &mut SvgImportBudget,
    imported_usvg_texts: &mut HashMap<(String, String), Vec<ImportedUsvgText>>,
) -> IoResult<Option<ObjectId>> {
    import_usvg_node_inner(
        doc,
        node,
        target_layer,
        inherited_xf,
        inherited_xf,
        inherited_opacity,
        depth,
        limits,
        budget,
        imported_usvg_texts,
    )
}

#[allow(clippy::too_many_arguments)]
fn import_usvg_node_inner(
    doc: &mut Document,
    node: &usvg::Node,
    target_layer: Option<LayerId>,
    local_xf_acc: Affine2D,
    world_to_mm: Affine2D,
    inherited_opacity: f32,
    depth: u32,
    limits: &IoLimits,
    budget: &mut SvgImportBudget,
    imported_usvg_texts: &mut HashMap<(String, String), Vec<ImportedUsvgText>>,
) -> IoResult<Option<ObjectId>> {
    if depth > limits.max_svg_depth {
        return Err(IoError::LimitExceeded {
            limit: format!("max_svg_depth={}", limits.max_svg_depth),
            actual: format!("depth={depth}"),
        });
    }

    match node {
        usvg::Node::Group(group) => {
            let g_xf = usvg_transform_raw(group.transform());
            let combined_local = local_xf_acc.compose(&g_xf);
            let combined_world = world_to_mm.compose(&g_xf);
            let group_opacity = (group.opacity().get() * inherited_opacity).clamp(0.0, 1.0);
            let group_blend = convert_usvg_blend_mode(group.blend_mode());

            // Flatten unnamed single-child wrapper groups created by usvg for transforms,
            // EXCEPT when the group carries a `<clipPath>` (which must stay wrapped as PowerClip).
            if group.children().len() == 1
                && group.id().is_empty()
                && group_blend == BlendMode::Normal
                && group.clip_path().is_none()
            {
                return import_usvg_node_inner(
                    doc,
                    &group.children()[0],
                    target_layer,
                    combined_local,
                    combined_world,
                    group_opacity,
                    depth + 1,
                    limits,
                    budget,
                    imported_usvg_texts,
                );
            }

            let mut clip_chain: Vec<(BezierPath, bool)> = Vec::new();
            let mut cur_cp = group.clip_path();
            let mut cp_depth = 0u32;
            while let Some(cp) = cur_cp {
                if cp_depth > limits.max_svg_depth {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_svg_depth={}", limits.max_svg_depth),
                        actual: format!("clip_path depth={cp_depth}"),
                    });
                }
                let cp_xf = usvg_transform_raw(cp.transform());
                let mut bp = BezierPath::default();
                let mut even_odd = false;
                collect_usvg_clip_group_paths(
                    cp.root(),
                    cp_xf,
                    0,
                    limits.max_svg_depth,
                    budget,
                    &mut bp,
                    &mut even_odd,
                )?;
                if !bp.is_empty() {
                    clip_chain.push((bp, even_odd));
                }
                cur_cp = cp.clip_path();
                cp_depth += 1;
            }
            let has_clip = !clip_chain.is_empty();
            let child_local_xf = if has_clip {
                combined_local
            } else {
                Affine2D::IDENTITY
            };

            let mut child_ids = Vec::new();
            for child in group.children() {
                if let Some(cid) = import_usvg_node_inner(
                    doc,
                    child,
                    None,
                    child_local_xf,
                    combined_world,
                    1.0,
                    depth + 1,
                    limits,
                    budget,
                    imported_usvg_texts,
                )? {
                    child_ids.push(cid);
                }
            }
            if child_ids.is_empty() {
                return Ok(None);
            }

            let name = if group.id().is_empty() {
                if has_clip {
                    "ClipGroup".to_owned()
                } else {
                    "Group".to_owned()
                }
            } else {
                group.id().to_owned()
            };

            if has_clip {
                let singular = combined_local.inverse().is_none();
                let mut current_ids = child_ids;
                let last_idx = clip_chain.len() - 1;
                for (idx, (mut clip_bp, even_odd)) in clip_chain.into_iter().enumerate() {
                    let is_outermost = idx == last_idx;
                    let container_xf = if singular {
                        clip_bp = clip_bp.transformed(&combined_local);
                        Affine2D::IDENTITY
                    } else {
                        combined_local
                    };
                    let mut container = SceneObject::new(
                        ObjectId(0),
                        if is_outermost {
                            name.clone()
                        } else {
                            "Clip".to_owned()
                        },
                        NodePayload::Path(clip_bp),
                    )
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::none())
                    .with_transform(container_xf);
                    container.clip_even_odd = even_odd;
                    if is_outermost {
                        container.opacity = group_opacity;
                        container.blend_mode = group_blend;
                    }
                    let pc_id = emit_svg_object(
                        doc,
                        container,
                        if is_outermost { target_layer } else { None },
                        budget,
                    )?;
                    let created_id = doc.create_powerclip(pc_id, &current_ids)?;
                    current_ids = vec![created_id];
                }
                return Ok(current_ids.into_iter().next());
            }

            let mut obj = SceneObject::new(ObjectId(0), name, NodePayload::Group(child_ids));
            obj.transform = combined_local;
            obj.opacity = group_opacity;
            obj.blend_mode = group_blend;
            obj.fill = Fill::None;
            obj.outline = OutlineStyle::none();

            let id = emit_svg_object(doc, obj, target_layer, budget)?;
            Ok(Some(id))
        }
        usvg::Node::Path(path) => {
            budget.record_vertices(path.data().verbs().len())?;
            let bez = usvg_path_to_bezier_raw(path.data());
            if bez.is_empty() {
                return Ok(None);
            }
            let unit_scale_mm = effective_affine_scale(&world_to_mm);
            let name = if path.id().is_empty() {
                "Path".to_owned()
            } else {
                path.id().to_owned()
            };
            let fill = path
                .fill()
                .map_or(Fill::None, |f| convert_usvg_fill(f, unit_scale_mm));
            let outline = path.stroke().map_or_else(OutlineStyle::none, |s| {
                convert_usvg_stroke(s, path.paint_order(), unit_scale_mm)
            });

            let mut obj = SceneObject::new(ObjectId(0), name, NodePayload::Path(bez))
                .with_fill(fill)
                .with_outline(outline)
                .with_transform(local_xf_acc);
            obj.opacity = inherited_opacity.clamp(0.0, 1.0);
            obj.visible = path.is_visible();

            let id = emit_svg_object(doc, obj, target_layer, budget)?;
            Ok(Some(id))
        }
        usvg::Node::Image(img) => match img.kind() {
            usvg::ImageKind::PNG(data)
            | usvg::ImageKind::JPEG(data)
            | usvg::ImageKind::GIF(data)
            | usvg::ImageKind::WEBP(data) => {
                let asset = decode_raster_bytes_with_limits(data, limits)?;
                let asset_id = doc.register_asset(asset);
                let width_mm = f64::from(img.size().width()).max(0.01);
                let height_mm = f64::from(img.size().height()).max(0.01);
                let name = if img.id().is_empty() {
                    "Image".to_owned()
                } else {
                    img.id().to_owned()
                };
                let mut obj = SceneObject::new(
                    ObjectId(0),
                    name,
                    NodePayload::Raster(RasterLayerData {
                        asset_id,
                        width_mm,
                        height_mm,
                        filters: Vec::new(),
                        filter_enabled: Vec::new(),
                        crop_rect: None,
                    }),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(local_xf_acc);
                obj.opacity = inherited_opacity.clamp(0.0, 1.0);
                obj.visible = img.is_visible();

                let id = emit_svg_object(doc, obj, target_layer, budget)?;
                Ok(Some(id))
            }
            usvg::ImageKind::SVG(nested) => {
                let mut child_ids = Vec::new();
                for sub in nested.root().children() {
                    if let Some(cid) = import_usvg_node_inner(
                        doc,
                        sub,
                        None,
                        Affine2D::IDENTITY,
                        world_to_mm,
                        1.0,
                        depth + 1,
                        limits,
                        budget,
                        imported_usvg_texts,
                    )? {
                        child_ids.push(cid);
                    }
                }
                if child_ids.is_empty() {
                    return Ok(None);
                }
                let mut obj =
                    SceneObject::new(ObjectId(0), "Embedded SVG", NodePayload::Group(child_ids))
                        .with_fill(Fill::None)
                        .with_outline(OutlineStyle::none())
                        .with_transform(local_xf_acc);
                obj.opacity = inherited_opacity.clamp(0.0, 1.0);
                let id = emit_svg_object(doc, obj, target_layer, budget)?;
                Ok(Some(id))
            }
        },
        usvg::Node::Text(text) => {
            // Use `local_xf_acc`'s scale to bake scaling into `font_size_pt` and remove it
            // from `obj.transform` so neither pixel-unit SVGs nor scaled `<g>`s double-scale text.
            let local_scale = effective_affine_scale(&local_xf_acc);
            let mut runs = Vec::new();
            let mut first_pos: Option<Point2D> = None;
            let mut anchor_align = TextAlign::Left;

            for chunk in text.chunks() {
                if first_pos.is_none() {
                    let x = f64::from(chunk.x().unwrap_or(0.0));
                    let y = f64::from(chunk.y().unwrap_or(0.0));
                    first_pos = Some(Point2D::new(x, y));
                    anchor_align = match chunk.anchor() {
                        usvg::TextAnchor::Start => TextAlign::Left,
                        usvg::TextAnchor::Middle => TextAlign::Center,
                        usvg::TextAnchor::End => TextAlign::Right,
                    };
                }
                let raw = chunk.text();
                if chunk.spans().is_empty() {
                    if !raw.is_empty() {
                        runs.push(TextRun::new(raw.to_owned(), TextStyle::default()));
                    }
                } else {
                    for span in chunk.spans() {
                        let slice = raw.get(span.start()..span.end()).unwrap_or("");
                        if slice.is_empty() {
                            continue;
                        }
                        let family = span
                            .font()
                            .families()
                            .first()
                            .map(font_family_to_string)
                            .unwrap_or_else(|| "Noto Sans".to_owned());
                        let font_size_mm = f64::from(span.font_size().get()) * local_scale;
                        let font_size_pt = Unit::Point.from_mm(font_size_mm, 96.0).max(1.0);
                        let fill_color = span
                            .fill()
                            .map(|f| convert_usvg_fill_primary_color(f, local_scale))
                            .unwrap_or(Color::BLACK);
                        let style = TextStyle {
                            font_family: family,
                            font_size_pt,
                            bold: span.font().weight() >= 600,
                            italic: !matches!(span.font().style(), usvg::FontStyle::Normal),
                            letter_spacing_mm: f64::from(span.letter_spacing()) * local_scale,
                            fill: fill_color,
                            direction: TextDirection::Auto,
                            align: anchor_align,
                            ..TextStyle::default()
                        };
                        runs.push(TextRun::new(slice.to_owned(), style));
                    }
                }
            }

            if runs.is_empty() {
                return Ok(None);
            }

            let norm_plain =
                normalize_svg_text_key(&runs.iter().map(|r| r.text.as_str()).collect::<String>());
            let families_lower: Vec<String> = runs
                .iter()
                .map(|r| r.style.font_family.trim().to_ascii_lowercase())
                .collect();
            let xml_id = text.id().trim().to_owned();

            let artistic = ArtisticText {
                runs,
                direction: TextDirection::Auto,
                align: anchor_align,
            };
            let pos = first_pos.unwrap_or(Point2D::ZERO);
            let name = if text.id().is_empty() {
                "Text".to_owned()
            } else {
                text.id().to_owned()
            };
            let inv_scale = if local_scale > 1e-9 {
                1.0 / local_scale
            } else {
                1.0
            };
            let text_xf = local_xf_acc
                .compose(&Affine2D::translate(pos.x, pos.y))
                .compose(&Affine2D::scale_uniform(inv_scale));
            let pos_mm = text_xf.transform_point(Point2D::ZERO);
            let mut obj = SceneObject::new(ObjectId(0), name, NodePayload::ArtisticText(artistic))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(text_xf);
            obj.opacity = inherited_opacity.clamp(0.0, 1.0);

            let obj_id = emit_svg_object(doc, obj, target_layer, budget)?;
            if !norm_plain.is_empty() {
                imported_usvg_texts
                    .entry((xml_id, norm_plain))
                    .or_default()
                    .push(ImportedUsvgText {
                        families_lower,
                        pos_mm,
                    });
            }
            Ok(Some(obj_id))
        }
    }
}

fn svg_element_attr<'a>(node: usvg::roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    if let Some(v) = node.attribute(name) {
        return Some(v.trim());
    }
    if let Some(style) = node.attribute("style") {
        for decl in style.split(';').rev() {
            if let Some((k, v)) = decl.split_once(':') {
                if k.trim().eq_ignore_ascii_case(name) {
                    return Some(v.trim());
                }
            }
        }
    }
    None
}

fn parse_svg_unit_opacity(raw: &str) -> Option<f32> {
    let s = raw.trim();
    if let Some(pct) = s.strip_suffix('%') {
        let v: f32 = pct.trim().parse().ok()?;
        v.is_finite().then(|| (v / 100.0).clamp(0.0, 1.0))
    } else {
        let v: f32 = s.parse().ok()?;
        v.is_finite().then(|| v.clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone)]
struct FallbackSpanStyle {
    tspan_opacity: f32,
    fill_color: Option<Color>,
    fill_opacity: f32,
    stroke_color: Option<Color>,
    stroke_opacity: f32,
    stroke_width: Option<f64>,
    font_family: String,
    font_size_pt: f64,
    bold: bool,
    italic: bool,
    direction: TextDirection,
}

fn update_fallback_span_style_from_node(
    el: usvg::roxmltree::Node<'_, '_>,
    total_scale: f64,
    is_tspan: bool,
    st: &mut FallbackSpanStyle,
) {
    if is_tspan {
        if let Some(op) = svg_element_attr(el, "opacity").and_then(parse_svg_unit_opacity) {
            st.tspan_opacity = (st.tspan_opacity * op).clamp(0.0, 1.0);
        }
    }
    if let Some(fill_attr) = svg_element_attr(el, "fill") {
        if fill_attr.eq_ignore_ascii_case("none") {
            st.fill_color = None;
        } else if let Ok(c) = Color::from_hex(fill_attr) {
            st.fill_color = Some(c);
        }
    }
    if let Some(fo) = svg_element_attr(el, "fill-opacity").and_then(parse_svg_unit_opacity) {
        st.fill_opacity = fo;
    }
    if let Some(stroke_attr) = svg_element_attr(el, "stroke") {
        if stroke_attr.eq_ignore_ascii_case("none") {
            st.stroke_color = None;
        } else if let Ok(c) = Color::from_hex(stroke_attr) {
            st.stroke_color = Some(c);
        }
    }
    if let Some(so) = svg_element_attr(el, "stroke-opacity").and_then(parse_svg_unit_opacity) {
        st.stroke_opacity = so;
    }
    if let Some(sw) = svg_element_attr(el, "stroke-width").and_then(parse_svg_len_to_user) {
        if sw.is_finite() && sw >= 0.0 {
            st.stroke_width = Some(sw);
        }
    }
    if let Some(ff) = svg_element_attr(el, "font-family") {
        let primary = ff
            .split(',')
            .next()
            .unwrap_or(ff)
            .trim()
            .trim_matches(&['\'', '"'][..]);
        if !primary.is_empty() {
            st.font_family = primary.to_owned();
        }
    }
    if let Some(fs) = svg_element_attr(el, "font-size").and_then(parse_svg_len_to_user) {
        let font_size_mm = fs * total_scale;
        st.font_size_pt = Unit::Point.from_mm(font_size_mm, 96.0).max(1.0);
    }
    if let Some(fw) = svg_element_attr(el, "font-weight") {
        if fw.eq_ignore_ascii_case("bold") || fw.eq_ignore_ascii_case("bolder") {
            st.bold = true;
        } else if fw.eq_ignore_ascii_case("normal") || fw.eq_ignore_ascii_case("lighter") {
            st.bold = false;
        } else if let Ok(w) = fw.parse::<u16>() {
            st.bold = w >= 600;
        }
    }
    if let Some(fst) = svg_element_attr(el, "font-style") {
        if fst.eq_ignore_ascii_case("italic") || fst.eq_ignore_ascii_case("oblique") {
            st.italic = true;
        } else if fst.eq_ignore_ascii_case("normal") {
            st.italic = false;
        }
    }
    if let Some(dir) = svg_element_attr(el, "direction") {
        if dir.eq_ignore_ascii_case("rtl") {
            st.direction = TextDirection::Rtl;
        } else if dir.eq_ignore_ascii_case("ltr") {
            st.direction = TextDirection::Ltr;
        }
    }
}

fn collect_fallback_text_chunks(
    el: usvg::roxmltree::Node<'_, '_>,
    total_scale: f64,
    cur_style: &FallbackSpanStyle,
    out: &mut Vec<(String, FallbackSpanStyle)>,
) {
    for child in el.children() {
        if child.is_text() {
            if let Some(t) = child.text().filter(|s| !s.is_empty()) {
                out.push((t.to_owned(), cur_style.clone()));
            }
        } else if child.is_element() && child.tag_name().name() == "tspan" {
            if svg_element_attr(child, "display").is_some_and(|v| v.eq_ignore_ascii_case("none")) {
                continue;
            }
            let mut sub_style = cur_style.clone();
            update_fallback_span_style_from_node(child, total_scale, true, &mut sub_style);
            collect_fallback_text_chunks(child, total_scale, &sub_style, out);
        }
    }
}

fn parse_xml_font_families_lower(node: usvg::roxmltree::Node<'_, '_>) -> Vec<String> {
    let mut cur = Some(node);
    while let Some(el) = cur {
        if el.is_element() {
            if let Some(ff) = svg_element_attr(el, "font-family") {
                let list: Vec<String> = ff
                    .split(',')
                    .map(|p| p.trim().trim_matches(&['\'', '"'][..]).to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !list.is_empty() {
                    return list;
                }
            }
        }
        cur = el.parent();
    }
    vec!["noto sans".to_owned()]
}

fn is_non_rendered_svg_tag(tag: &str) -> bool {
    matches!(
        tag,
        "defs"
            | "clipPath"
            | "mask"
            | "pattern"
            | "symbol"
            | "marker"
            | "linearGradient"
            | "radialGradient"
            | "filter"
            | "style"
            | "title"
            | "desc"
            | "metadata"
    )
}

/// Fallback XML `<text>` extractor recovering `<text>` elements skipped by `usvg` (for example,
/// missing system fonts in mixed-font SVGs) while skipping `<text>` elements already imported by `usvg`.
/// Preserves ancestor named layers (`data-layer` / Inkscape layers), SVG `transform` chains,
/// and inherited/per-`<tspan>` `opacity` / `fill-opacity` / `stroke-opacity`.
#[allow(clippy::too_many_arguments)]
fn import_fallback_xml_texts(
    doc: &mut Document,
    xml_doc: &usvg::roxmltree::Document<'_>,
    page_id: coree_core::PageId,
    default_layer_id: LayerId,
    layer_meta_by_xml_id: &HashMap<String, SvgLayerMeta>,
    xml_layer_ids: &mut HashMap<String, LayerId>,
    created_named_layers: &mut bool,
    budget: &mut SvgImportBudget,
    imported_usvg_texts: &mut HashMap<(String, String), Vec<ImportedUsvgText>>,
) -> IoResult<()> {
    let svg_el = xml_doc.root_element();
    let (scale_x, scale_y) = compute_fallback_user_unit_scale(&svg_el);
    let root_scale = Affine2D::scale(scale_x, scale_y);

    struct FallbackCandidate<'a, 'input> {
        node: usvg::roxmltree::Node<'a, 'input>,
        chain: Vec<usvg::roxmltree::Node<'a, 'input>>,
        key: (String, String),
    }

    let mut candidates = Vec::new();
    let mut remaining_xml_counts: HashMap<(String, String), usize> = HashMap::new();

    for node in xml_doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "text")
    {
        let raw_plain: String = node
            .descendants()
            .filter(|d| d.is_text())
            .filter_map(|d| d.text())
            .collect();
        let norm_plain = normalize_svg_text_key(&raw_plain);
        if norm_plain.is_empty() {
            continue;
        }

        let mut chain = Vec::new();
        let mut cursor = Some(node);
        let mut hidden_or_defs = false;
        while let Some(cur) = cursor {
            if cur.is_element() {
                if is_non_rendered_svg_tag(cur.tag_name().name())
                    || svg_element_attr(cur, "display")
                        .is_some_and(|v| v.eq_ignore_ascii_case("none"))
                {
                    hidden_or_defs = true;
                    break;
                }
                chain.push(cur);
            }
            cursor = cur.parent();
        }
        if hidden_or_defs {
            continue;
        }
        chain.reverse();

        let xml_id = node.attribute("id").unwrap_or("").trim().to_owned();
        let key = (xml_id, norm_plain);
        *remaining_xml_counts.entry(key.clone()).or_insert(0) += 1;
        candidates.push(FallbackCandidate { node, chain, key });
    }

    for cand in candidates {
        let node = cand.node;
        let chain = cand.chain;
        let key = cand.key;
        let rem_xml = {
            let cnt = remaining_xml_counts.entry(key.clone()).or_insert(1);
            *cnt = cnt.saturating_sub(1);
            *cnt
        };

        // Walk ancestors (root -> node) to compose transforms and accumulate inherited paint/opacity.
        let mut combined_xf = root_scale;
        let mut inherited_opacity = 1.0_f32;
        let mut inherited_fill_color: Option<Color> = Some(Color::BLACK);
        let mut inherited_fill_opacity = 1.0_f32;
        let mut inherited_stroke_color: Option<Color> = None;
        let mut inherited_stroke_opacity = 1.0_f32;
        let mut inherited_stroke_width: Option<f64> = None;

        for anc in &chain {
            if let Some(tf_str) = anc.attribute("transform") {
                if let Some(parsed_xf) = parse_svg_transform_list(tf_str) {
                    combined_xf = combined_xf.compose(&parsed_xf);
                }
            }
            if let Some(op) = svg_element_attr(*anc, "opacity").and_then(parse_svg_unit_opacity) {
                inherited_opacity = (inherited_opacity * op).clamp(0.0, 1.0);
            }
            if let Some(fill_attr) = svg_element_attr(*anc, "fill") {
                if fill_attr.eq_ignore_ascii_case("none") {
                    inherited_fill_color = None;
                } else if let Ok(c) = Color::from_hex(fill_attr) {
                    inherited_fill_color = Some(c);
                }
            }
            if let Some(fo) =
                svg_element_attr(*anc, "fill-opacity").and_then(parse_svg_unit_opacity)
            {
                inherited_fill_opacity = fo;
            }
            if let Some(stroke_attr) = svg_element_attr(*anc, "stroke") {
                if stroke_attr.eq_ignore_ascii_case("none") {
                    inherited_stroke_color = None;
                } else if let Ok(c) = Color::from_hex(stroke_attr) {
                    inherited_stroke_color = Some(c);
                }
            }
            if let Some(so) =
                svg_element_attr(*anc, "stroke-opacity").and_then(parse_svg_unit_opacity)
            {
                inherited_stroke_opacity = so;
            }
            if let Some(sw) = svg_element_attr(*anc, "stroke-width").and_then(parse_svg_len_to_user)
            {
                if sw.is_finite() && sw >= 0.0 {
                    inherited_stroke_width = Some(sw);
                }
            }
        }

        let x = node
            .attribute("x")
            .and_then(parse_svg_len_to_user)
            .unwrap_or(0.0);
        let y = node
            .attribute("y")
            .and_then(parse_svg_len_to_user)
            .unwrap_or(0.0);
        let total_scale = effective_affine_scale(&combined_xf);
        let inv_scale = if total_scale > 1e-9 {
            1.0 / total_scale
        } else {
            1.0
        };
        let final_xf = combined_xf
            .compose(&Affine2D::translate(x, y))
            .compose(&Affine2D::scale_uniform(inv_scale));
        let candidate_pos_mm = final_xf.transform_point(Point2D::ZERO);

        if let Some(usvg_entries) = imported_usvg_texts.get_mut(&key) {
            if !usvg_entries.is_empty() {
                let xml_families = parse_xml_font_families_lower(node);
                let matched_idx = usvg_entries.iter().position(|entry| {
                    let family_match = entry
                        .families_lower
                        .iter()
                        .any(|ef| xml_families.iter().any(|xf| xf == ef));
                    let pos_match = entry.pos_mm.distance_to(candidate_pos_mm) < 0.5;
                    family_match || pos_match
                });
                if let Some(idx) = matched_idx {
                    usvg_entries.remove(idx);
                    continue;
                }
                if usvg_entries.len() > rem_xml {
                    usvg_entries.pop();
                    continue;
                }
            }
        }

        let mut target_layer = default_layer_id;
        for anc in &chain {
            if anc.tag_name().name() == "g" {
                if let Some(id) = anc.attribute("id") {
                    if let Some(meta) = layer_meta_by_xml_id.get(id) {
                        if let Some(&existing_lid) = xml_layer_ids.get(id) {
                            target_layer = existing_lid;
                        } else {
                            let is_cut =
                                meta.is_cut_layer || meta.name.eq_ignore_ascii_case("cutcontour");
                            let lid = doc.add_layer(
                                page_id,
                                meta.name.clone(),
                                is_cut,
                                MasterLayerScope::None,
                            )?;
                            xml_layer_ids.insert(id.to_owned(), lid);
                            *created_named_layers = true;
                            target_layer = lid;
                        }
                    }
                }
            }
        }

        let default_font_size_mm = Unit::Point.to_mm(12.0, 96.0);
        let default_font_size_pt = Unit::Point.from_mm(default_font_size_mm, 96.0).max(1.0);
        let mut base_span_style = FallbackSpanStyle {
            tspan_opacity: 1.0,
            fill_color: inherited_fill_color,
            fill_opacity: inherited_fill_opacity,
            stroke_color: inherited_stroke_color,
            stroke_opacity: inherited_stroke_opacity,
            stroke_width: inherited_stroke_width,
            font_family: "Noto Sans".to_owned(),
            font_size_pt: default_font_size_pt,
            bold: false,
            italic: false,
            direction: TextDirection::Auto,
        };
        for anc in &chain {
            update_fallback_span_style_from_node(*anc, total_scale, false, &mut base_span_style);
        }

        let mut chunks = Vec::new();
        collect_fallback_text_chunks(node, total_scale, &base_span_style, &mut chunks);
        if let Some(first) = chunks.first_mut() {
            first.0 = first.0.trim_start().to_owned();
        }
        if let Some(last) = chunks.last_mut() {
            last.0 = last.0.trim_end().to_owned();
        }
        chunks.retain(|(t, _)| !t.is_empty());
        if chunks.is_empty() {
            continue;
        }

        let first_tspan_op = chunks[0].1.tspan_opacity;
        let uniform_tspan_opacity = chunks
            .iter()
            .all(|(_, s)| (s.tspan_opacity - first_tspan_op).abs() <= 1e-4)
            .then_some(first_tspan_op);
        let obj_opacity = match uniform_tspan_opacity {
            Some(common_op) => (inherited_opacity * common_op).clamp(0.0, 1.0),
            None => inherited_opacity.clamp(0.0, 1.0),
        };

        let mut runs: Vec<TextRun> = Vec::new();
        for (txt, st) in &chunks {
            let per_run_op = if uniform_tspan_opacity.is_some() {
                1.0
            } else {
                st.tspan_opacity
            };
            let fill_color = match &st.fill_color {
                Some(c) => {
                    let [r, g, b, a] = c.to_rgba_f32();
                    Color::rgba(r, g, b, (a * st.fill_opacity * per_run_op).clamp(0.0, 1.0))
                }
                None => Color::TRANSPARENT,
            };
            let style = TextStyle {
                font_family: st.font_family.clone(),
                font_size_pt: st.font_size_pt,
                bold: st.bold,
                italic: st.italic,
                fill: fill_color,
                direction: st.direction,
                ..TextStyle::default()
            };
            if let Some(prev) = runs.last_mut().filter(|r| r.style == style) {
                prev.text.push_str(txt);
            } else {
                runs.push(TextRun::new(txt.clone(), style));
            }
        }

        let eff_stroke_style = chunks
            .iter()
            .map(|(_, s)| s)
            .find(|s| s.stroke_color.is_some())
            .unwrap_or(&chunks[0].1);
        let outline = match &eff_stroke_style.stroke_color {
            Some(c)
                if (c.alpha()
                    * eff_stroke_style.stroke_opacity
                    * if uniform_tspan_opacity.is_some() {
                        1.0
                    } else {
                        eff_stroke_style.tspan_opacity
                    })
                    > 1e-6 =>
            {
                let [r, g, b, a] = c.to_rgba_f32();
                let extra_op = if uniform_tspan_opacity.is_some() {
                    1.0
                } else {
                    eff_stroke_style.tspan_opacity
                };
                let sc = Color::rgba(
                    r,
                    g,
                    b,
                    (a * eff_stroke_style.stroke_opacity * extra_op).clamp(0.0, 1.0),
                );
                let width_mm =
                    (eff_stroke_style.stroke_width.unwrap_or(1.0) * total_scale).max(0.0);
                if width_mm > 0.0 {
                    OutlineStyle::solid(sc, width_mm)
                } else {
                    OutlineStyle::none()
                }
            }
            _ => OutlineStyle::none(),
        };

        let artistic = ArtisticText {
            runs,
            direction: base_span_style.direction,
            align: TextAlign::Left,
        };
        let mut obj = SceneObject::new(
            ObjectId(0),
            node.attribute("id").unwrap_or("Text"),
            NodePayload::ArtisticText(artistic),
        )
        .with_fill(Fill::None)
        .with_outline(outline)
        .with_transform(final_xf);
        obj.opacity = obj_opacity;

        emit_svg_object(doc, obj, Some(target_layer), budget)?;
    }
    Ok(())
}

fn parse_svg_transform_list(raw: &str) -> Option<Affine2D> {
    let mut acc = Affine2D::IDENTITY;
    let mut rest = raw.trim();
    let mut any = false;
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let close = rest[open + 1..].find(')')? + open + 1;
        let kind = rest[..open].trim().to_ascii_lowercase();
        let nums: Vec<f64> = rest[open + 1..close]
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .collect();
        let step = match kind.as_str() {
            "matrix" if nums.len() == 6 => {
                Affine2D::new(nums[0], nums[1], nums[2], nums[3], nums[4], nums[5])
            }
            "translate" if !nums.is_empty() => {
                Affine2D::translate(nums[0], *nums.get(1).unwrap_or(&0.0))
            }
            "scale" if !nums.is_empty() => {
                Affine2D::scale(nums[0], *nums.get(1).unwrap_or(&nums[0]))
            }
            "rotate" if !nums.is_empty() => {
                let rad = nums[0].to_radians();
                if nums.len() >= 3 {
                    Affine2D::rotate_about(rad, Point2D::new(nums[1], nums[2]))
                } else {
                    Affine2D::rotate(rad)
                }
            }
            _ => Affine2D::IDENTITY,
        };
        acc = acc.compose(&step);
        any = true;
        rest = rest[close + 1..].trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
    }
    any.then_some(acc)
}

fn compute_fallback_user_unit_scale(svg_el: &usvg::roxmltree::Node<'_, '_>) -> (f64, f64) {
    let w_mm = svg_el.attribute("width").and_then(parse_svg_dim_mm);
    let h_mm = svg_el.attribute("height").and_then(parse_svg_dim_mm);
    if let (Some(wm), Some(hm), Some(vb)) = (w_mm, h_mm, svg_el.attribute("viewBox")) {
        let nums: Vec<f64> = vb
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .collect();
        if nums.len() == 4 && nums[2] > 0.0 && nums[3] > 0.0 && wm > 0.0 && hm > 0.0 {
            return (wm / nums[2], hm / nums[3]);
        }
    }
    (PX_TO_MM_96, PX_TO_MM_96)
}

fn parse_svg_len_to_user(raw: &str) -> Option<f64> {
    let s = raw.trim();
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(s.len());
    s[..num_end].parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_svg_dim_mm(raw: &str) -> Option<f64> {
    let s = raw.trim();
    let val = if let Some(v) = s.strip_suffix("mm") {
        v.trim().parse::<f64>().ok()
    } else if let Some(v) = s.strip_suffix("cm") {
        v.trim().parse::<f64>().ok().map(|n| n * 10.0)
    } else if let Some(v) = s.strip_suffix("in") {
        v.trim().parse::<f64>().ok().map(|n| n * 25.4)
    } else if let Some(v) = s.strip_suffix("pt") {
        v.trim().parse::<f64>().ok().map(|n| n * (25.4 / 72.0))
    } else if let Some(v) = s.strip_suffix("px") {
        v.trim().parse::<f64>().ok().map(|n| n * PX_TO_MM_96)
    } else {
        s.parse::<f64>().ok().map(|n| n * PX_TO_MM_96)
    };
    val.filter(|v| v.is_finite())
}

fn font_family_to_string(fam: &usvg::FontFamily) -> String {
    match fam {
        usvg::FontFamily::Serif => "serif".to_owned(),
        usvg::FontFamily::SansSerif => "sans-serif".to_owned(),
        usvg::FontFamily::Cursive => "cursive".to_owned(),
        usvg::FontFamily::Fantasy => "fantasy".to_owned(),
        usvg::FontFamily::Monospace => "monospace".to_owned(),
        usvg::FontFamily::Named(n) => n.clone(),
    }
}

fn usvg_transform_raw(t: usvg::Transform) -> Affine2D {
    Affine2D::new(
        f64::from(t.sx),
        f64::from(t.ky),
        f64::from(t.kx),
        f64::from(t.sy),
        f64::from(t.tx),
        f64::from(t.ty),
    )
}

fn effective_affine_scale(xf: &Affine2D) -> f64 {
    let [a, b, c, d, _, _] = xf.m;
    let sx = a.hypot(b);
    let sy = c.hypot(d);
    let avg = 0.5 * (sx + sy);
    if avg.is_finite() && avg > 1e-9 {
        avg
    } else {
        1.0
    }
}

fn usvg_path_to_bezier_raw(path: &usvg::tiny_skia_path::Path) -> BezierPath {
    let mut kb = BezPath::new();
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                kb.move_to(kurbo::Point::new(f64::from(p.x), f64::from(p.y)));
            }
            PathSegment::LineTo(p) => {
                kb.line_to(kurbo::Point::new(f64::from(p.x), f64::from(p.y)));
            }
            PathSegment::QuadTo(p1, p2) => {
                kb.quad_to(
                    kurbo::Point::new(f64::from(p1.x), f64::from(p1.y)),
                    kurbo::Point::new(f64::from(p2.x), f64::from(p2.y)),
                );
            }
            PathSegment::CubicTo(p1, p2, p3) => {
                kb.curve_to(
                    kurbo::Point::new(f64::from(p1.x), f64::from(p1.y)),
                    kurbo::Point::new(f64::from(p2.x), f64::from(p2.y)),
                    kurbo::Point::new(f64::from(p3.x), f64::from(p3.y)),
                );
            }
            PathSegment::Close => kb.close_path(),
        }
    }
    BezierPath::from_kurbo(&kb)
}

fn collect_usvg_clip_group_paths(
    group: &usvg::Group,
    acc_xf: Affine2D,
    depth: u32,
    max_depth: u32,
    budget: &mut SvgImportBudget,
    out: &mut BezierPath,
    any_even_odd: &mut bool,
) -> IoResult<()> {
    if depth > max_depth {
        return Err(IoError::LimitExceeded {
            limit: format!("max_svg_depth={max_depth}"),
            actual: format!("clip_path depth={depth}"),
        });
    }
    let xf = acc_xf.compose(&usvg_transform_raw(group.transform()));
    for child in group.children() {
        match child {
            usvg::Node::Group(sub_g) => {
                collect_usvg_clip_group_paths(
                    sub_g,
                    xf,
                    depth + 1,
                    max_depth,
                    budget,
                    out,
                    any_even_odd,
                )?;
            }
            usvg::Node::Path(p) => {
                budget.record_vertices(p.data().verbs().len())?;
                if p.fill()
                    .is_some_and(|f| matches!(f.rule(), usvg::FillRule::EvenOdd))
                {
                    *any_even_odd = true;
                }
                let bez = usvg_path_to_bezier_raw(p.data()).transformed(&xf);
                out.subpaths.extend(bez.subpaths);
            }
            usvg::Node::Text(t) => {
                collect_usvg_clip_group_paths(
                    t.flattened(),
                    xf,
                    depth + 1,
                    max_depth,
                    budget,
                    out,
                    any_even_odd,
                )?;
            }
            usvg::Node::Image(_) => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_pattern_colors_and_shapes(
    group: &usvg::Group,
    opacity: f32,
    colors: &mut Vec<Color>,
    has_circle_or_curve: &mut bool,
    has_diag_stroke: &mut bool,
    has_axis_stroke: &mut bool,
    rect_count: &mut usize,
    depth: u32,
) {
    if depth > 16 {
        return;
    }
    let push_color = |colors: &mut Vec<Color>, c: usvg::Color, op: f32| {
        let col = Color::rgba(
            f32::from(c.red) / 255.0,
            f32::from(c.green) / 255.0,
            f32::from(c.blue) / 255.0,
            (op * opacity).clamp(0.0, 1.0),
        );
        if !colors.iter().any(|existing| {
            let ea = existing.to_rgba8();
            let cb = col.to_rgba8();
            ea == cb
        }) {
            colors.push(col);
        }
    };
    for node in group.children() {
        match node {
            usvg::Node::Group(sub) => collect_pattern_colors_and_shapes(
                sub,
                opacity,
                colors,
                has_circle_or_curve,
                has_diag_stroke,
                has_axis_stroke,
                rect_count,
                depth + 1,
            ),
            usvg::Node::Path(path) => {
                if let Some(f) = path.fill() {
                    if let usvg::Paint::Color(c) = f.paint() {
                        push_color(colors, *c, f.opacity().get());
                    }
                }
                if let Some(s) = path.stroke() {
                    if let usvg::Paint::Color(c) = s.paint() {
                        push_color(colors, *c, s.opacity().get());
                    }
                }
                let mut seg_diag = false;
                let mut seg_axis = false;
                let mut seg_curve = false;
                let mut prev: Option<(f32, f32)> = None;
                let mut line_segs = 0usize;
                for seg in path.data().segments() {
                    match seg {
                        PathSegment::MoveTo(p) => prev = Some((p.x, p.y)),
                        PathSegment::LineTo(p) => {
                            line_segs += 1;
                            if let Some((px, py)) = prev {
                                let dx = (p.x - px).abs();
                                let dy = (p.y - py).abs();
                                if dx > 1e-3 && dy > 1e-3 {
                                    seg_diag = true;
                                } else if dx > 1e-3 || dy > 1e-3 {
                                    seg_axis = true;
                                }
                            }
                            prev = Some((p.x, p.y));
                        }
                        PathSegment::QuadTo(_, p) | PathSegment::CubicTo(_, _, p) => {
                            seg_curve = true;
                            prev = Some((p.x, p.y));
                        }
                        PathSegment::Close => {}
                    }
                }
                if seg_curve {
                    *has_circle_or_curve = true;
                }
                if path.stroke().is_some() {
                    if seg_diag {
                        *has_diag_stroke = true;
                    } else if seg_axis {
                        *has_axis_stroke = true;
                    }
                } else if line_segs >= 3 && seg_axis && !seg_curve {
                    *rect_count += 1;
                }
            }
            _ => {}
        }
    }
}

fn convert_usvg_pattern(pat: &usvg::Pattern, opacity: f32, unit_scale_mm: f64) -> Fill {
    let mut colors = Vec::new();
    let mut has_circle_or_curve = false;
    let mut has_diag_stroke = false;
    let mut has_axis_stroke = false;
    let mut rect_count = 0usize;
    collect_pattern_colors_and_shapes(
        pat.root(),
        opacity,
        &mut colors,
        &mut has_circle_or_curve,
        &mut has_diag_stroke,
        &mut has_axis_stroke,
        &mut rect_count,
        0,
    );
    if colors.is_empty() {
        return Fill::None;
    }
    if colors.len() == 1 {
        return Fill::Solid(colors[0].clone());
    }
    let bg = colors[0].clone();
    let fg = colors[1].clone();
    let pattern_name = if has_circle_or_curve {
        "PolkaDots".to_owned()
    } else if has_diag_stroke && !has_axis_stroke {
        "DiagonalStripe".to_owned()
    } else if has_axis_stroke && has_diag_stroke {
        "CrossHatch".to_owned()
    } else if has_axis_stroke {
        "Grid".to_owned()
    } else if rect_count == 2 {
        "Stripe".to_owned()
    } else {
        "Checker".to_owned()
    };
    let tile_w = (f64::from(pat.rect().width()) * unit_scale_mm).max(0.5);
    let tile_h = (f64::from(pat.rect().height()) * unit_scale_mm).max(0.5);
    Fill::TwoColorPattern {
        fg,
        bg,
        pattern_name,
        tile_w,
        tile_h,
    }
}

fn convert_usvg_fill(fill: &usvg::Fill, unit_scale_mm: f64) -> Fill {
    let opacity = fill.opacity().get();
    match fill.paint() {
        usvg::Paint::Color(c) => Fill::Solid(Color::rgba(
            f32::from(c.red) / 255.0,
            f32::from(c.green) / 255.0,
            f32::from(c.blue) / 255.0,
            opacity,
        )),
        usvg::Paint::LinearGradient(lg) => {
            let t = usvg_transform_raw(lg.transform());
            let start = t.transform_point(Point2D::new(f64::from(lg.x1()), f64::from(lg.y1())));
            let end = t.transform_point(Point2D::new(f64::from(lg.x2()), f64::from(lg.y2())));
            let stops = lg
                .stops()
                .iter()
                .map(|s| {
                    GradientStop::new(
                        s.offset().get(),
                        Color::rgba(
                            f32::from(s.color().red) / 255.0,
                            f32::from(s.color().green) / 255.0,
                            f32::from(s.color().blue) / 255.0,
                            (s.opacity().get() * opacity).clamp(0.0, 1.0),
                        ),
                    )
                })
                .collect();
            Fill::LinearGradient { start, end, stops }
        }
        usvg::Paint::RadialGradient(rg) => {
            let t = usvg_transform_raw(rg.transform());
            let center = t.transform_point(Point2D::new(f64::from(rg.cx()), f64::from(rg.cy())));
            let focal = t.transform_point(Point2D::new(f64::from(rg.fx()), f64::from(rg.fy())));
            let radius_pt = t.transform_point(Point2D::new(
                f64::from(rg.cx() + rg.r().get()),
                f64::from(rg.cy()),
            ));
            let radius = center.distance_to(radius_pt).max(1e-4);
            let stops = rg
                .stops()
                .iter()
                .map(|s| {
                    GradientStop::new(
                        s.offset().get(),
                        Color::rgba(
                            f32::from(s.color().red) / 255.0,
                            f32::from(s.color().green) / 255.0,
                            f32::from(s.color().blue) / 255.0,
                            (s.opacity().get() * opacity).clamp(0.0, 1.0),
                        ),
                    )
                })
                .collect();
            Fill::RadialGradient {
                center,
                radius,
                focal,
                stops,
            }
        }
        usvg::Paint::Pattern(pat) => convert_usvg_pattern(pat, opacity, unit_scale_mm),
    }
}

fn convert_usvg_fill_primary_color(fill: &usvg::Fill, unit_scale_mm: f64) -> Color {
    match convert_usvg_fill(fill, unit_scale_mm) {
        Fill::Solid(c) => c,
        Fill::TwoColorPattern { fg, .. } => fg,
        Fill::LinearGradient { stops, .. } | Fill::RadialGradient { stops, .. } => {
            stops.first().map_or(Color::BLACK, |s| s.color.clone())
        }
        _ => Color::BLACK,
    }
}

fn convert_usvg_stroke(
    stroke: &usvg::Stroke,
    paint_order: usvg::PaintOrder,
    unit_scale_mm: f64,
) -> OutlineStyle {
    let opacity = stroke.opacity().get();
    let color = match stroke.paint() {
        usvg::Paint::Color(c) => Color::rgba(
            f32::from(c.red) / 255.0,
            f32::from(c.green) / 255.0,
            f32::from(c.blue) / 255.0,
            opacity,
        ),
        usvg::Paint::LinearGradient(lg) => lg.stops().first().map_or(Color::BLACK, |s| {
            Color::rgba(
                f32::from(s.color().red) / 255.0,
                f32::from(s.color().green) / 255.0,
                f32::from(s.color().blue) / 255.0,
                (s.opacity().get() * opacity).clamp(0.0, 1.0),
            )
        }),
        usvg::Paint::RadialGradient(rg) => rg.stops().first().map_or(Color::BLACK, |s| {
            Color::rgba(
                f32::from(s.color().red) / 255.0,
                f32::from(s.color().green) / 255.0,
                f32::from(s.color().blue) / 255.0,
                (s.opacity().get() * opacity).clamp(0.0, 1.0),
            )
        }),
        usvg::Paint::Pattern(_) => Color::BLACK,
    };

    let width_mm = (f64::from(stroke.width().get()) * unit_scale_mm).max(0.0);
    let cap = match stroke.linecap() {
        usvg::LineCap::Butt => LineCap::Butt,
        usvg::LineCap::Round => LineCap::Round,
        usvg::LineCap::Square => LineCap::Square,
    };
    let join = match stroke.linejoin() {
        usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => LineJoin::Miter,
        usvg::LineJoin::Round => LineJoin::Round,
        usvg::LineJoin::Bevel => LineJoin::Bevel,
    };
    let dash_pattern = stroke
        .dasharray()
        .map(|arr| arr.iter().map(|&v| f64::from(v) * unit_scale_mm).collect())
        .unwrap_or_default();

    let mut out = OutlineStyle::solid(color, width_mm);
    out.cap = cap;
    out.join = join;
    out.miter_limit = f64::from(stroke.miterlimit().get()).max(1.0);
    out.dash_pattern = dash_pattern;
    out.behind_fill = matches!(paint_order, usvg::PaintOrder::StrokeAndFill);
    out
}

fn convert_usvg_blend_mode(bm: usvg::BlendMode) -> BlendMode {
    match bm {
        usvg::BlendMode::Normal => BlendMode::Normal,
        usvg::BlendMode::Multiply => BlendMode::Multiply,
        usvg::BlendMode::Screen => BlendMode::Screen,
        usvg::BlendMode::Overlay => BlendMode::Overlay,
        usvg::BlendMode::Darken => BlendMode::Darken,
        usvg::BlendMode::Lighten | usvg::BlendMode::PlusLighter => BlendMode::Lighten,
        usvg::BlendMode::ColorDodge => BlendMode::ColorDodge,
        usvg::BlendMode::ColorBurn => BlendMode::ColorBurn,
        usvg::BlendMode::HardLight => BlendMode::HardLight,
        usvg::BlendMode::SoftLight => BlendMode::SoftLight,
        usvg::BlendMode::Difference => BlendMode::Difference,
        usvg::BlendMode::Exclusion => BlendMode::Exclusion,
        usvg::BlendMode::Hue => BlendMode::Hue,
        usvg::BlendMode::Saturation => BlendMode::Saturation,
        usvg::BlendMode::Color => BlendMode::Color,
        usvg::BlendMode::Luminosity => BlendMode::Luminosity,
    }
}

// =============================================================================
// Pure-Rust SVG 1.1 Exporter
// =============================================================================

/// Exports `page_index` of `doc` as a clean SVG 1.1 XML string (millimeter `viewBox`).
pub fn export_svg_to_string(doc: &Document, page_index: usize) -> IoResult<String> {
    let mut text_engine = TextEngine::new();
    let dl = ExportDisplayList::build(
        doc,
        page_index,
        &DisplayListOptions::svg(),
        &mut text_engine,
    )?;
    let page_w = if dl.width_mm.is_finite() && dl.width_mm > 0.0 {
        dl.width_mm
    } else {
        210.0
    };
    let page_h = if dl.height_mm.is_finite() && dl.height_mm > 0.0 {
        dl.height_mm
    } else {
        297.0
    };

    let mut defs = String::new();
    let mut body = String::new();
    let mut def_counter = 0usize;

    svg_render_display_ops(
        doc,
        &dl.ops,
        &mut text_engine,
        &mut defs,
        &mut body,
        &mut def_counter,
        2,
    );

    let mut out = String::with_capacity(defs.len() + body.len() + 512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" version=\"1.1\" width=\"{page_w:.4}mm\" height=\"{page_h:.4}mm\" viewBox=\"0 0 {page_w:.4} {page_h:.4}\">"
    );
    let _ = writeln!(out, "  <title>{}</title>", escape_xml_text(doc.title()));
    if !defs.is_empty() {
        out.push_str("  <defs>\n");
        out.push_str(&defs);
        out.push_str("  </defs>\n");
    }
    out.push_str(&body);
    out.push_str("</svg>\n");
    Ok(out)
}

/// Atomically exports `page_index` of `doc` to an SVG 1.1 file at `path`.
pub fn export_svg(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    let svg = export_svg_to_string(doc, page_index)?;
    atomic_write_bytes(path, svg.as_bytes())
}

fn format_svg_opacity_attr(opacity: f32) -> String {
    let safe_opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if (safe_opacity - 1.0).abs() > 1e-4 {
        format!(" opacity=\"{safe_opacity:.4}\"")
    } else {
        String::new()
    }
}

fn svg_render_display_ops(
    doc: &Document,
    ops: &[DisplayOp],
    text_engine: &mut TextEngine,
    defs: &mut String,
    body: &mut String,
    def_counter: &mut usize,
    initial_indent: usize,
) {
    let mut indent = initial_indent;
    let mut persp_stack: Vec<bool> = Vec::new();

    for op in ops {
        let parent_is_persp = persp_stack.last().copied().unwrap_or(false);
        match op {
            DisplayOp::PushLayer {
                id,
                name,
                visible,
                is_cut_layer,
                ..
            } => {
                let pad = " ".repeat(indent);
                let vis_attr = if *visible { "" } else { " display=\"none\"" };
                let cut_attr = if *is_cut_layer {
                    " data-cut-layer=\"true\""
                } else {
                    ""
                };
                let _ = writeln!(
                    body,
                    "{pad}<g id=\"layer-{}\" data-layer=\"{}\"{vis_attr}{cut_attr}>",
                    id.0,
                    escape_xml_attr(name)
                );
                indent += 2;
            }
            DisplayOp::PopLayer => {
                indent = indent.saturating_sub(2);
                let pad = " ".repeat(indent);
                let _ = writeln!(body, "{pad}</g>");
            }
            DisplayOp::PushGroup {
                id_suffix,
                local_xf,
                visible,
                is_perspective,
                ..
            } => {
                let pad = " ".repeat(indent);
                let xf_attr = if *is_perspective {
                    String::new()
                } else {
                    format_svg_transform(*local_xf)
                };
                let vis_attr = if *visible { "" } else { " display=\"none\"" };
                let _ = writeln!(body, "{pad}<g id=\"{id_suffix}\"{xf_attr}{vis_attr}>");
                indent += 2;
                persp_stack.push(*is_perspective);
            }
            DisplayOp::PopGroup => {
                persp_stack.pop();
                indent = indent.saturating_sub(2);
                let pad = " ".repeat(indent);
                let _ = writeln!(body, "{pad}</g>");
            }
            DisplayOp::PushClip {
                id_suffix,
                local_path,
                fill_rule,
                local_xf,
                visible,
                is_perspective,
                ..
            } => {
                let pad = " ".repeat(indent);
                *def_counter += 1;
                let clip_id = format!("clip-{}", *def_counter);
                let rule_attr = match fill_rule {
                    FillRule::EvenOdd => " clip-rule=\"evenodd\"",
                    FillRule::Winding => "",
                };
                let container_d = bezier_to_svg_d(local_path);
                let _ = writeln!(
                    defs,
                    "    <clipPath id=\"{clip_id}\"><path d=\"{container_d}\"{rule_attr}/></clipPath>"
                );
                let xf_attr = if *is_perspective {
                    String::new()
                } else {
                    format_svg_transform(*local_xf)
                };
                let vis_attr = if *visible { "" } else { " display=\"none\"" };
                let _ = writeln!(
                    body,
                    "{pad}<g id=\"{id_suffix}\"{xf_attr}{vis_attr} clip-path=\"url(#{clip_id})\">"
                );
                indent += 2;
                persp_stack.push(*is_perspective);
            }
            DisplayOp::PopClip => {
                persp_stack.pop();
                indent = indent.saturating_sub(2);
                let pad = " ".repeat(indent);
                let _ = writeln!(body, "{pad}</g>");
            }
            DisplayOp::PushTransparencyGroup { .. } | DisplayOp::PopTransparencyGroup => {}
            DisplayOp::DrawVector {
                id_suffix,
                local_path,
                fill,
                outline,
                local_xf,
                opacity,
                blend_mode,
                visible,
                is_perspective,
                emit_arrowheads,
                ..
            } => {
                let d = bezier_to_svg_d(local_path);
                if d.is_empty() {
                    continue;
                }
                let pad = " ".repeat(indent);
                let skip_xf = parent_is_persp
                    || (*is_perspective
                        && (id_suffix.ends_with("-bg") || id_suffix.ends_with("-fg")));
                let xf_attr = if skip_xf {
                    String::new()
                } else {
                    format_svg_transform(*local_xf)
                };
                let op_attr = format_svg_opacity_attr(*opacity);
                let blend_attr = format_svg_blend_style(*blend_mode);
                let vis_attr = if *visible { "" } else { " display=\"none\"" };
                let paint_attrs = format_svg_paint_attrs_with_doc(
                    doc,
                    fill,
                    outline,
                    defs,
                    def_counter,
                    text_engine,
                );
                let _ = writeln!(
                    body,
                    "{pad}<path id=\"{id_suffix}\" d=\"{d}\"{paint_attrs}{xf_attr}{op_attr}{blend_attr}{vis_attr}/>"
                );
                if *emit_arrowheads && outline.is_visible() {
                    write_svg_arrowheads(
                        local_path,
                        outline,
                        &xf_attr,
                        &op_attr,
                        &blend_attr,
                        vis_attr,
                        body,
                        &pad,
                    );
                }
            }
            DisplayOp::DrawRaster {
                id_suffix,
                asset_id,
                inline_rgba,
                rect_mm,
                local_xf,
                width_mm,
                height_mm,
                opacity,
                blend_mode,
                visible,
                ..
            } => {
                let pad = " ".repeat(indent);
                let op_attr = format_svg_opacity_attr(*opacity);
                let blend_attr = format_svg_blend_style(*blend_mode);
                let vis_attr = if *visible { "" } else { " display=\"none\"" };
                let xf_attr = format_svg_transform(*local_xf);
                if let Some(rgba_arc) = inline_rgba {
                    if let Ok(png) = export_raster_to_bytes(
                        rgba_arc.0,
                        rgba_arc.1,
                        &rgba_arc.2,
                        &RasterExportOptions::new(RasterFormat::Png, 150.0),
                    ) {
                        let b64 = encode_base64(&png);
                        let (x, y) = rect_mm.map_or((0.0, 0.0), |r| (r.min_x, r.min_y));
                        let _ = writeln!(
                            body,
                            "{pad}<image id=\"{id_suffix}\" x=\"{x:.4}\" y=\"{y:.4}\" width=\"{:.4}\" height=\"{:.4}\" preserveAspectRatio=\"none\" href=\"data:image/png;base64,{b64}\"{xf_attr}{op_attr}{blend_attr}{vis_attr}/>",
                            *width_mm, *height_mm
                        );
                    }
                } else if let Some(aid) = asset_id {
                    if let Some(asset) = doc.asset(*aid) {
                        if let Ok(png) = export_raster_to_bytes(
                            asset.width,
                            asset.height,
                            &asset.to_rgba8_vec(),
                            &RasterExportOptions::new(RasterFormat::Png, asset.source_dpi),
                        ) {
                            let b64 = encode_base64(&png);
                            let _ = writeln!(
                                body,
                                "{pad}<image id=\"{id_suffix}\" x=\"0\" y=\"0\" width=\"{:.4}\" height=\"{:.4}\" preserveAspectRatio=\"none\" href=\"data:image/png;base64,{b64}\"{xf_attr}{op_attr}{blend_attr}{vis_attr}/>",
                                *width_mm, *height_mm
                            );
                        }
                    }
                }
            }
            DisplayOp::DrawTextRuns {
                id_suffix,
                kind,
                artistic,
                fallback_curves,
                svg_fallback_curves,
                outline,
                local_xf,
                sym_xf,
                opacity,
                blend_mode,
                visible,
                is_perspective,
                ..
            } => {
                let pad = " ".repeat(indent);
                let op_attr = format_svg_opacity_attr(*opacity);
                let blend_attr = format_svg_blend_style(*blend_mode);
                let vis_attr = if *visible { "" } else { " display=\"none\"" };
                if *is_perspective {
                    let xf_attr = if parent_is_persp {
                        String::new()
                    } else {
                        format_svg_transform(*local_xf)
                    };
                    for (p_idx, piece) in svg_fallback_curves.iter().enumerate() {
                        let d = bezier_to_svg_d(&piece.path);
                        if d.is_empty() {
                            continue;
                        }
                        let paint_attrs = format_svg_paint_attrs_with_doc(
                            doc,
                            &Fill::Solid(piece.fill.clone()),
                            outline,
                            defs,
                            def_counter,
                            text_engine,
                        );
                        let _ = writeln!(
                            body,
                            "{pad}<path id=\"{id_suffix}-g{p_idx}\" d=\"{d}\"{paint_attrs}{xf_attr}{op_attr}{blend_attr}{vis_attr}/>"
                        );
                    }
                } else if let DisplayTextKind::OnPath {
                    text_on_path,
                    local_host_path: Some(local_host),
                } = kind
                {
                    write_svg_text_on_path(
                        id_suffix,
                        text_on_path,
                        local_host,
                        *sym_xf,
                        &op_attr,
                        &blend_attr,
                        vis_attr,
                        defs,
                        body,
                        def_counter,
                        &pad,
                    );
                } else if matches!(
                    kind,
                    DisplayTextKind::Paragraph {
                        needs_doc_flow: true
                    }
                ) {
                    let xf_attr = format_svg_transform(*local_xf);
                    for (p_idx, piece) in fallback_curves.iter().enumerate() {
                        let d = bezier_to_svg_d(&piece.path);
                        if d.is_empty() {
                            continue;
                        }
                        let paint_attrs = format_svg_paint_attrs_with_doc(
                            doc,
                            &Fill::Solid(piece.fill.clone()),
                            outline,
                            defs,
                            def_counter,
                            text_engine,
                        );
                        let _ = writeln!(
                            body,
                            "{pad}<path id=\"{id_suffix}-g{p_idx}\" d=\"{d}\"{paint_attrs}{xf_attr}{op_attr}{blend_attr}{vis_attr}/>"
                        );
                    }
                } else {
                    let xf_attr = format_svg_transform(*local_xf);
                    write_svg_artistic_text(
                        id_suffix,
                        artistic,
                        &xf_attr,
                        &op_attr,
                        &blend_attr,
                        vis_attr,
                        body,
                        &pad,
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_svg_arrowheads(
    path: &BezierPath,
    outline: &OutlineStyle,
    xf_attr: &str,
    op_attr: &str,
    blend_attr: &str,
    vis_attr: &str,
    body: &mut String,
    pad: &str,
) {
    let heads = outline_arrowhead_geometries(path, outline);
    if heads.is_empty() {
        return;
    }
    let hex = color_to_svg_rgb(&outline.color);
    for (geom, filled) in heads {
        let d = bezier_to_svg_d(&geom);
        if d.is_empty() {
            continue;
        }
        if filled {
            let _ = writeln!(
                body,
                "{pad}<path d=\"{d}\" fill=\"{hex}\" stroke=\"none\"{xf_attr}{op_attr}{blend_attr}{vis_attr}/>"
            );
        } else {
            let _ = writeln!(
                body,
                "{pad}<path d=\"{d}\" fill=\"none\" stroke=\"{hex}\" stroke-width=\"{:.4}\" stroke-linecap=\"round\"{xf_attr}{op_attr}{blend_attr}{vis_attr}/>",
                outline.effective_width_mm().max(0.2)
            );
        }
    }
}

fn split_artistic_lines(text: &ArtisticText) -> Vec<Vec<(String, TextStyle)>> {
    let mut lines: Vec<Vec<(String, TextStyle)>> = vec![Vec::new()];
    for run in &text.runs {
        let mut first_part = true;
        for part in run.text.split('\n') {
            if !first_part {
                lines.push(Vec::new());
            }
            first_part = false;
            if !part.is_empty() {
                if let Some(cur) = lines.last_mut() {
                    cur.push((part.to_owned(), run.style.clone()));
                }
            }
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn write_svg_text_on_path(
    id_suffix: &str,
    top: &TextOnPath,
    local_host: &BezierPath,
    sym_xf: Affine2D,
    op_attr: &str,
    blend_attr: &str,
    vis_attr: &str,
    defs: &mut String,
    body: &mut String,
    def_counter: &mut usize,
    pad: &str,
) {
    let dir_attr = if top.text.effective_direction() == TextDirection::Rtl {
        " direction=\"rtl\" unicode-bidi=\"embed\""
    } else {
        ""
    };
    let first_style = top
        .text
        .runs
        .first()
        .map(|r| r.style.clone())
        .unwrap_or_default();
    let sym_attr = format_svg_transform(sym_xf);
    let start_off = if top.offset_mm.is_finite() && top.offset_mm.abs() > 1e-4 {
        format!(" startOffset=\"{:.4}\"", top.offset_mm)
    } else {
        String::new()
    };
    let lines = split_artistic_lines(&top.text);
    let default_line_h =
        (first_style.font_size_mm() * first_style.line_height_ratio.max(1.0)).max(2.0);
    let mut y_acc = -top.distance_mm;

    for (line_idx, line_runs) in lines.iter().enumerate() {
        let line_max_h = line_runs
            .iter()
            .map(|(_, st)| (st.font_size_mm() * st.line_height_ratio.max(1.0)).max(2.0))
            .fold(default_line_h, f64::max);
        let host_line = if y_acc.abs() > 1e-4 {
            local_host.transformed(&Affine2D::translate(0.0, y_acc))
        } else {
            local_host.clone()
        };
        y_acc += line_max_h;

        *def_counter += 1;
        let path_def_id = format!("textpath-{}", *def_counter);
        let _ = writeln!(
            defs,
            "    <path id=\"{path_def_id}\" d=\"{}\"/>",
            bezier_to_svg_d(&host_line)
        );
        let elem_id = if line_idx == 0 {
            id_suffix.to_owned()
        } else {
            format!("{id_suffix}-l{line_idx}")
        };
        let line_style = line_runs.first().map(|(_, st)| st).unwrap_or(&first_style);
        let text_weight = if line_runs.len() == 1 && line_style.bold {
            " font-weight=\"bold\""
        } else {
            ""
        };
        let text_italic = if line_runs.len() == 1 && line_style.italic {
            " font-style=\"italic\""
        } else {
            ""
        };
        let _ = write!(
            body,
            "{pad}<text id=\"{elem_id}\" font-family=\"{}\" font-size=\"{:.4}\" fill=\"{}\"{text_weight}{text_italic}{dir_attr}{sym_attr}{op_attr}{blend_attr}{vis_attr}><textPath href=\"#{path_def_id}\"{start_off}>",
            escape_xml_attr(&line_style.font_family),
            line_style.font_size_mm(),
            color_to_svg_rgb(&line_style.fill),
        );
        if line_runs.len() == 1 {
            body.push_str(&escape_xml_text(&line_runs[0].0));
        } else {
            for (seg_text, st) in line_runs {
                let weight = if st.bold { " font-weight=\"bold\"" } else { "" };
                let italic = if st.italic {
                    " font-style=\"italic\""
                } else {
                    ""
                };
                let _ = write!(
                    body,
                    "<tspan font-family=\"{}\" font-size=\"{:.4}\" fill=\"{}\"{weight}{italic}>{}</tspan>",
                    escape_xml_attr(&st.font_family),
                    st.font_size_mm(),
                    color_to_svg_rgb(&st.fill),
                    escape_xml_text(seg_text),
                );
            }
        }
        body.push_str("</textPath></text>\n");
    }
}

#[allow(clippy::too_many_arguments)]
fn write_svg_artistic_text(
    id_suffix: &str,
    text: &ArtisticText,
    xf_attr: &str,
    op_attr: &str,
    blend_attr: &str,
    vis_attr: &str,
    body: &mut String,
    pad: &str,
) {
    let dir = text.effective_direction();
    let dir_attr = if dir == TextDirection::Rtl {
        " direction=\"rtl\" unicode-bidi=\"embed\""
    } else {
        ""
    };
    let anchor = match text.align {
        TextAlign::Center => " text-anchor=\"middle\"",
        TextAlign::Right => " text-anchor=\"end\"",
        _ => "",
    };
    let primary_style = text
        .runs
        .first()
        .map(|r| r.style.clone())
        .unwrap_or_default();
    let single_line_single_run = text.runs.len() == 1 && !text.runs[0].text.contains('\n');
    let text_weight = if single_line_single_run && primary_style.bold {
        " font-weight=\"bold\""
    } else {
        ""
    };
    let text_italic = if single_line_single_run && primary_style.italic {
        " font-style=\"italic\""
    } else {
        ""
    };
    let _ = write!(
        body,
        "{pad}<text id=\"{id_suffix}\" x=\"0\" y=\"{:.4}\" font-family=\"{}\" font-size=\"{:.4}\" fill=\"{}\"{text_weight}{text_italic}{anchor}{dir_attr}{xf_attr}{op_attr}{blend_attr}{vis_attr}>",
        primary_style.font_size_mm(),
        escape_xml_attr(&primary_style.font_family),
        primary_style.font_size_mm(),
        color_to_svg_rgb(&primary_style.fill),
    );
    if single_line_single_run {
        body.push_str(&escape_xml_text(&text.runs[0].text));
    } else {
        let lines = split_artistic_lines(text);
        let default_lh =
            (primary_style.font_size_mm() * primary_style.line_height_ratio.max(1.0)).max(2.0);
        for (line_idx, line_runs) in lines.iter().enumerate() {
            let line_lh = line_runs
                .iter()
                .map(|(_, st)| (st.font_size_mm() * st.line_height_ratio.max(1.0)).max(2.0))
                .fold(default_lh, f64::max);
            for (run_idx, (seg, st)) in line_runs.iter().enumerate() {
                let weight = if st.bold { " font-weight=\"bold\"" } else { "" };
                let italic = if st.italic {
                    " font-style=\"italic\""
                } else {
                    ""
                };
                let pos_attr = if line_idx > 0 && run_idx == 0 {
                    format!(" x=\"0\" dy=\"{line_lh:.4}\"")
                } else {
                    String::new()
                };
                let _ = write!(
                    body,
                    "<tspan{pos_attr} font-family=\"{}\" font-size=\"{:.4}\" fill=\"{}\"{weight}{italic}>{}</tspan>",
                    escape_xml_attr(&st.font_family),
                    st.font_size_mm(),
                    color_to_svg_rgb(&st.fill),
                    escape_xml_text(seg),
                );
            }
        }
    }
    body.push_str("</text>\n");
}

fn format_svg_paint_attrs_with_doc(
    doc: &Document,
    fill: &Fill,
    outline: &OutlineStyle,
    defs: &mut String,
    def_counter: &mut usize,
    text_engine: &mut TextEngine,
) -> String {
    let mut s = String::new();

    match fill {
        Fill::None => s.push_str(" fill=\"none\""),
        Fill::Solid(c) => {
            let _ = write!(s, " fill=\"{}\"", color_to_svg_rgb(c));
            if (c.alpha() - 1.0).abs() > 1e-4 {
                let _ = write!(s, " fill-opacity=\"{:.4}\"", c.alpha());
            }
        }
        Fill::LinearGradient { start, end, stops } => {
            *def_counter += 1;
            let gid = format!("lg-{}", *def_counter);
            let _ = writeln!(
                defs,
                "    <linearGradient id=\"{gid}\" gradientUnits=\"userSpaceOnUse\" x1=\"{:.4}\" y1=\"{:.4}\" x2=\"{:.4}\" y2=\"{:.4}\">",
                start.x, start.y, end.x, end.y
            );
            for stop in stops {
                let _ = writeln!(
                    defs,
                    "      <stop offset=\"{:.2}%\" stop-color=\"{}\" stop-opacity=\"{:.4}\"/>",
                    stop.offset * 100.0,
                    color_to_svg_rgb(&stop.color),
                    stop.color.alpha()
                );
            }
            defs.push_str("    </linearGradient>\n");
            let _ = write!(s, " fill=\"url(#{gid})\"");
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } => {
            *def_counter += 1;
            let gid = format!("rg-{}", *def_counter);
            let _ = writeln!(
                defs,
                "    <radialGradient id=\"{gid}\" gradientUnits=\"userSpaceOnUse\" cx=\"{:.4}\" cy=\"{:.4}\" r=\"{:.4}\" fx=\"{:.4}\" fy=\"{:.4}\">",
                center.x, center.y, radius.max(1e-4), focal.x, focal.y
            );
            for stop in stops {
                let _ = writeln!(
                    defs,
                    "      <stop offset=\"{:.2}%\" stop-color=\"{}\" stop-opacity=\"{:.4}\"/>",
                    stop.offset * 100.0,
                    color_to_svg_rgb(&stop.color),
                    stop.color.alpha()
                );
            }
            defs.push_str("    </radialGradient>\n");
            let _ = write!(s, " fill=\"url(#{gid})\"");
        }
        Fill::TwoColorPattern {
            fg,
            bg,
            pattern_name,
            tile_w,
            tile_h,
        } => {
            *def_counter += 1;
            let pid = format!("pat-{}", *def_counter);
            let tw = tile_w.max(1.0);
            let th = tile_h.max(1.0);
            let fg_hex = color_to_svg_rgb(fg);
            let bg_hex = color_to_svg_rgb(bg);
            let sw = (tw.min(th) * 0.14).max(0.25);
            let _ = writeln!(
                defs,
                "    <pattern id=\"{pid}\" patternUnits=\"userSpaceOnUse\" width=\"{tw:.4}\" height=\"{th:.4}\" data-pattern-name=\"{}\">",
                escape_xml_attr(pattern_name)
            );
            let _ = writeln!(
                defs,
                "      <rect width=\"{tw:.4}\" height=\"{th:.4}\" fill=\"{bg_hex}\"/>"
            );
            let pname = pattern_name.to_ascii_lowercase();
            if pname.contains("dot") || pname.contains("polka") || pname.contains("circle") {
                let r = (tw.min(th) * 0.22).max(0.2);
                let _ = writeln!(
                    defs,
                    "      <circle cx=\"{:.4}\" cy=\"{:.4}\" r=\"{r:.4}\" fill=\"{fg_hex}\"/>",
                    tw * 0.5,
                    th * 0.5
                );
            } else if pname.contains("diagonal") || pname.contains("slant") {
                let _ = writeln!(
                    defs,
                    "      <path d=\"M 0 {th:.4} L {tw:.4} 0 M {:.4} {:.4} L {:.4} {:.4} M {:.4} {:.4} L {:.4} {:.4}\" stroke=\"{fg_hex}\" stroke-width=\"{sw:.4}\" fill=\"none\"/>",
                    -tw * 0.5,
                    th * 0.5,
                    tw * 0.5,
                    -th * 0.5,
                    tw * 0.5,
                    th * 1.5,
                    tw * 1.5,
                    th * 0.5,
                );
            } else if pname.contains("cross") {
                let _ = writeln!(
                    defs,
                    "      <path d=\"M 0 {th:.4} L {tw:.4} 0 M 0 0 L {tw:.4} {th:.4} M 0 {:.4} H {tw:.4}\" stroke=\"{fg_hex}\" stroke-width=\"{sw:.4}\" fill=\"none\"/>",
                    th * 0.5,
                );
            } else if pname.contains("grid") || pname.contains("hatch") {
                let _ = writeln!(
                    defs,
                    "      <path d=\"M 0 {:.4} H {tw:.4} M {:.4} 0 V {th:.4}\" stroke=\"{fg_hex}\" stroke-width=\"{sw:.4}\" fill=\"none\"/>",
                    th * 0.5,
                    tw * 0.5,
                );
            } else if pname.contains("brick") {
                let _ = writeln!(
                    defs,
                    "      <path d=\"M 0 {:.4} H {tw:.4} M 0 {th:.4} H {tw:.4} M {:.4} 0 V {:.4} M {tw:.4} {:.4} V {th:.4}\" stroke=\"{fg_hex}\" stroke-width=\"{sw:.4}\" fill=\"none\"/>",
                    th * 0.5,
                    tw * 0.5,
                    th * 0.5,
                    th * 0.5,
                );
            } else if pname.contains("stripe") || pname.contains("vert") || pname.contains("horiz")
            {
                if pname.contains("vert") {
                    let _ = writeln!(
                        defs,
                        "      <rect x=\"0\" y=\"0\" width=\"{:.4}\" height=\"{th:.4}\" fill=\"{fg_hex}\"/>",
                        tw * 0.5
                    );
                } else {
                    let _ = writeln!(
                        defs,
                        "      <rect x=\"0\" y=\"0\" width=\"{tw:.4}\" height=\"{:.4}\" fill=\"{fg_hex}\"/>",
                        th * 0.5
                    );
                }
            } else {
                let _ = writeln!(
                    defs,
                    "      <rect width=\"{:.4}\" height=\"{:.4}\" fill=\"{fg_hex}\"/>",
                    tw * 0.5,
                    th * 0.5
                );
                let _ = writeln!(
                    defs,
                    "      <rect x=\"{:.4}\" y=\"{:.4}\" width=\"{:.4}\" height=\"{:.4}\" fill=\"{fg_hex}\"/>",
                    tw * 0.5,
                    th * 0.5,
                    tw * 0.5,
                    th * 0.5
                );
            }
            defs.push_str("    </pattern>\n");
            let _ = write!(s, " fill=\"url(#{pid})\"");
        }
        Fill::BitmapPattern {
            image_id,
            tile_w,
            tile_h,
        } => {
            if let Some(asset) = doc.asset(*image_id) {
                if let Ok(png) = export_raster_to_bytes(
                    asset.width,
                    asset.height,
                    &asset.to_rgba8_vec(),
                    &RasterExportOptions::new(RasterFormat::Png, asset.source_dpi),
                ) {
                    *def_counter += 1;
                    let pid = format!("bpat-{}", *def_counter);
                    let tw = tile_w.max(1.0);
                    let th = tile_h.max(1.0);
                    let b64 = encode_base64(&png);
                    let _ = writeln!(
                        defs,
                        "    <pattern id=\"{pid}\" patternUnits=\"userSpaceOnUse\" width=\"{tw:.4}\" height=\"{th:.4}\">"
                    );
                    let _ = writeln!(
                        defs,
                        "      <image x=\"0\" y=\"0\" width=\"{tw:.4}\" height=\"{th:.4}\" preserveAspectRatio=\"none\" href=\"data:image/png;base64,{b64}\"/>"
                    );
                    defs.push_str("    </pattern>\n");
                    let _ = write!(s, " fill=\"url(#{pid})\"");
                } else {
                    s.push_str(" fill=\"none\"");
                }
            } else {
                s.push_str(" fill=\"none\"");
            }
        }
        Fill::VectorPattern {
            symbol_id,
            tile_w,
            tile_h,
        } => {
            if let Some(sym) = doc.symbol(*symbol_id) {
                *def_counter += 1;
                let pid = format!("vpat-{}", *def_counter);
                let tw = tile_w.max(1.0);
                let th = tile_h.max(1.0);
                let sym_ops = ExportDisplayList::build_symbol(
                    doc,
                    sym,
                    &DisplayListOptions::svg(),
                    text_engine,
                );
                let mut pat_body = String::new();
                svg_render_display_ops(
                    doc,
                    &sym_ops,
                    text_engine,
                    defs,
                    &mut pat_body,
                    def_counter,
                    6,
                );
                let _ = writeln!(
                    defs,
                    "    <pattern id=\"{pid}\" patternUnits=\"userSpaceOnUse\" width=\"{tw:.4}\" height=\"{th:.4}\">"
                );
                defs.push_str(&pat_body);
                defs.push_str("    </pattern>\n");
                let _ = write!(s, " fill=\"url(#{pid})\"");
            } else {
                s.push_str(" fill=\"none\"");
            }
        }
        Fill::ConicalGradient { stops, .. } | Fill::SquareGradient { stops, .. } => {
            let fallback = stops.first().map_or(Color::BLACK, |st| st.color.clone());
            let _ = write!(s, " fill=\"{}\"", color_to_svg_rgb(&fallback));
        }
        Fill::TextureFill { fg, .. } => {
            let _ = write!(s, " fill=\"{}\"", color_to_svg_rgb(fg));
        }
        Fill::MeshFill { nodes, .. } => {
            let fallback = nodes.first().map_or(Color::BLACK, |n| n.color.clone());
            let _ = write!(s, " fill=\"{}\"", color_to_svg_rgb(&fallback));
        }
    }

    if outline.is_visible() {
        let _ = write!(
            s,
            " stroke=\"{}\" stroke-width=\"{:.4}\"",
            color_to_svg_rgb(&outline.color),
            outline.effective_width_mm()
        );
        if (outline.color.alpha() - 1.0).abs() > 1e-4 {
            let _ = write!(s, " stroke-opacity=\"{:.4}\"", outline.color.alpha());
        }
        let cap = match outline.cap {
            LineCap::Butt => "butt",
            LineCap::Round => "round",
            LineCap::Square => "square",
        };
        let join = match outline.join {
            LineJoin::Miter => "miter",
            LineJoin::Round => "round",
            LineJoin::Bevel => "bevel",
        };
        let _ = write!(s, " stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\"");
        if (outline.miter_limit - 4.0).abs() > 1e-3 {
            let _ = write!(s, " stroke-miterlimit=\"{:.3}\"", outline.miter_limit);
        }
        if !outline.dash_pattern.is_empty() {
            let dashes: Vec<String> = outline
                .dash_pattern
                .iter()
                .filter(|d| d.is_finite() && **d >= 0.0)
                .map(|d| format!("{d:.4}"))
                .collect();
            if !dashes.is_empty() {
                let _ = write!(s, " stroke-dasharray=\"{}\"", dashes.join(" "));
            }
        }
        if outline.behind_fill {
            s.push_str(" paint-order=\"stroke fill\"");
        }
    } else {
        s.push_str(" stroke=\"none\"");
    }

    s
}

fn bezier_to_svg_d(path: &BezierPath) -> String {
    let kb = path.to_kurbo();
    let mut d = String::new();
    for el in kb.elements() {
        let pt_ok = |p: kurbo::Point| p.x.is_finite() && p.y.is_finite();
        match *el {
            PathEl::MoveTo(p) if pt_ok(p) => {
                if !d.is_empty() {
                    d.push(' ');
                }
                let _ = write!(d, "M {:.4} {:.4}", p.x, p.y);
            }
            PathEl::LineTo(p) if pt_ok(p) => {
                if !d.is_empty() {
                    d.push(' ');
                }
                let _ = write!(d, "L {:.4} {:.4}", p.x, p.y);
            }
            PathEl::QuadTo(p1, p2) if pt_ok(p1) && pt_ok(p2) => {
                if !d.is_empty() {
                    d.push(' ');
                }
                let _ = write!(d, "Q {:.4} {:.4} {:.4} {:.4}", p1.x, p1.y, p2.x, p2.y);
            }
            PathEl::CurveTo(p1, p2, p3) if pt_ok(p1) && pt_ok(p2) && pt_ok(p3) => {
                if !d.is_empty() {
                    d.push(' ');
                }
                let _ = write!(
                    d,
                    "C {:.4} {:.4} {:.4} {:.4} {:.4} {:.4}",
                    p1.x, p1.y, p2.x, p2.y, p3.x, p3.y
                );
            }
            PathEl::ClosePath if !d.is_empty() => {
                d.push_str(" Z");
            }
            _ => {}
        }
    }
    d
}

fn format_svg_transform(xf: Affine2D) -> String {
    if !xf.is_finite() {
        return String::new();
    }
    let [a, b, c, d, tx, ty] = xf.m;
    let is_identity = (a - 1.0).abs() < 1e-6
        && b.abs() < 1e-6
        && c.abs() < 1e-6
        && (d - 1.0).abs() < 1e-6
        && tx.abs() < 1e-6
        && ty.abs() < 1e-6;
    if is_identity {
        String::new()
    } else {
        format!(" transform=\"matrix({a:.6} {b:.6} {c:.6} {d:.6} {tx:.4} {ty:.4})\"")
    }
}

fn format_svg_blend_style(bm: BlendMode) -> String {
    let css = match bm {
        BlendMode::Normal => return String::new(),
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::SoftLight => "soft-light",
        BlendMode::HardLight => "hard-light",
        BlendMode::ColorDodge => "color-dodge",
        BlendMode::ColorBurn => "color-burn",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
    };
    format!(" style=\"mix-blend-mode:{css}\"")
}

fn color_to_svg_rgb(color: &Color) -> String {
    let [r, g, b, _] = color.to_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn escape_xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_xml_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        CornerStyle, Effect, MasterLayerScope, ParametricShape, RasterAsset, Rect2D, TextRun,
    };

    #[test]
    fn svg_export_and_reimport_roundtrip_preserves_layers_gradients_strokes_and_rtl() {
        let mut doc = Document::new("SVG Roundtrip", 100.0, 80.0, ColorMode::Rgb, 96.0);
        let page_id = doc.pages()[0].id;
        let artwork_layer = doc
            .add_layer(page_id, "Artwork", false, MasterLayerScope::None)
            .unwrap();

        let mut stroke = OutlineStyle::solid(Color::rgba8(10, 20, 30, 255), 1.5);
        stroke.dash_pattern = vec![4.0, 2.0];
        stroke.cap = LineCap::Round;
        stroke.join = LineJoin::Bevel;

        let rect = SceneObject::new(
            ObjectId(0),
            "Gradient Box",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 40.0,
                height: 25.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::LinearGradient {
            start: Point2D::new(0.0, 0.0),
            end: Point2D::new(40.0, 25.0),
            stops: vec![
                GradientStop::new(0.0, Color::rgba8(255, 0, 0, 255)),
                GradientStop::new(1.0, Color::rgba8(0, 0, 255, 255)),
            ],
        })
        .with_outline(stroke)
        .with_transform(Affine2D::translate(10.0, 12.0));
        doc.add_object(rect, Some(artwork_layer)).unwrap();

        let arabic = SceneObject::new(
            ObjectId(0),
            "Arabic Label",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "مرحبا كوري",
                TextStyle {
                    direction: TextDirection::Auto,
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(15.0, 50.0));
        doc.add_object(arabic, Some(artwork_layer)).unwrap();

        let asset =
            RasterAsset::new(2, 2, [0, 255, 128, 255].repeat(4), 96.0, ColorMode::Rgb).unwrap();
        let asset_id = doc.register_asset(asset);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Bitmap",
                NodePayload::Raster(RasterLayerData {
                    asset_id,
                    width_mm: 10.0,
                    height_mm: 10.0,
                    filters: vec![],
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            ),
            Some(artwork_layer),
        )
        .unwrap();

        let svg_str = export_svg_to_string(&doc, 0).unwrap();
        assert!(svg_str.contains("width=\"100.0000mm\""));
        assert!(svg_str.contains("height=\"80.0000mm\""));
        assert!(svg_str.contains("data-layer=\"Artwork\""));
        assert!(svg_str.contains("<linearGradient"));
        assert!(svg_str.contains("direction=\"rtl\""));
        assert!(svg_str.contains("data:image/png;base64,"));

        let imported = import_svg_str(&svg_str).unwrap();
        assert!((imported.pages()[0].width_mm - 100.0).abs() < 0.2);
        assert!((imported.pages()[0].height_mm - 80.0).abs() < 0.2);
        assert!(imported.pages()[0]
            .layers
            .iter()
            .any(|l| l.name == "Artwork"));
        assert!(
            imported
                .objects()
                .values()
                .any(|o| matches!(o.fill, Fill::LinearGradient { .. })),
            "expected LinearGradient to round-trip"
        );
        assert!(
            imported
                .objects()
                .values()
                .any(|o| !o.outline.dash_pattern.is_empty()),
            "expected stroke dash pattern to round-trip"
        );
        assert!(
            !imported.assets().is_empty(),
            "expected embedded PNG image to round-trip"
        );
    }

    #[test]
    fn svg_import_rejects_excessive_nesting_depth() {
        let mut svg =
            String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">");
        for _ in 0..40 {
            svg.push_str("<g>");
        }
        svg.push_str("<rect width=\"5\" height=\"5\"/>");
        for _ in 0..40 {
            svg.push_str("</g>");
        }
        svg.push_str("</svg>");

        let limits = IoLimits {
            max_svg_depth: 16,
            ..IoLimits::default()
        };
        let err = import_svg_str_with_limits(&svg, &limits).unwrap_err();
        assert!(matches!(err, IoError::LimitExceeded { .. }));
    }

    #[test]
    fn svg_patterns_symmetry_and_arrowheads_export() {
        let mut doc = Document::new("SVG Effects", 120.0, 90.0, ColorMode::Rgb, 96.0);
        let asset =
            RasterAsset::new(2, 2, [255, 128, 0, 255].repeat(4), 96.0, ColorMode::Rgb).unwrap();
        let asset_id = doc.register_asset(asset);

        let mut line = SceneObject::new(
            ObjectId(0),
            "Arrow",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(10.0, 10.0),
                Point2D::new(40.0, 10.0),
            )),
        )
        .with_outline(OutlineStyle {
            end_arrow: coree_core::ArrowheadStyle::Triangle,
            ..OutlineStyle::solid(Color::BLACK, 1.0)
        });
        line.effects.push(Effect::Symmetry {
            axes: 2,
            mirror: false,
            origin: Point2D::new(50.0, 40.0),
        });
        doc.add_object(line, None).unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "PatBox",
                NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    60.0, 20.0, 30.0, 30.0,
                ))),
            )
            .with_fill(Fill::BitmapPattern {
                image_id: asset_id,
                tile_w: 5.0,
                tile_h: 5.0,
            }),
            None,
        )
        .unwrap();

        let svg = export_svg_to_string(&doc, 0).unwrap();
        assert!(svg.contains("<pattern id=\"bpat-"));
        assert!(svg.contains("fill=\"url(#bpat-"));
        assert!(svg.contains("-sym1\""));
    }

    #[test]
    fn svg_two_color_patterns_dynamic_geometry_and_import_roundtrip() {
        let mut doc = Document::new("TwoColorPat", 120.0, 90.0, ColorMode::Rgb, 96.0);
        let variants = [
            "DiagonalStripe",
            "PolkaDots",
            "CrossHatch",
            "Grid",
            "Brick",
            "Stripe",
            "Checker",
        ];
        for (idx, name) in variants.iter().enumerate() {
            let x = 5.0 + idx as f64 * 15.0;
            doc.add_object(
                SceneObject::new(
                    ObjectId(0),
                    *name,
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        x, 10.0, 12.0, 12.0,
                    ))),
                )
                .with_fill(Fill::TwoColorPattern {
                    fg: Color::rgba8(20, 40, 200, 255),
                    bg: Color::rgba8(245, 240, 220, 255),
                    pattern_name: (*name).to_owned(),
                    tile_w: 4.0,
                    tile_h: 4.0,
                }),
                None,
            )
            .unwrap();
        }

        let svg = export_svg_to_string(&doc, 0).unwrap();
        assert!(svg.contains("<circle "));
        assert!(svg.contains("stroke=\"#1428c8\""));
        assert!(svg.contains("data-pattern-name=\"DiagonalStripe\""));
        assert!(svg.contains("data-pattern-name=\"PolkaDots\""));
        assert!(svg.contains("data-pattern-name=\"CrossHatch\""));

        let imported = import_svg_str(&svg).unwrap();
        let pat_count = imported
            .objects()
            .values()
            .filter(|o| matches!(o.fill, Fill::TwoColorPattern { .. }))
            .count();
        assert_eq!(
            pat_count,
            variants.len(),
            "all TwoColorPattern tiles must re-import"
        );
    }

    #[test]
    fn svg_text_on_path_multiline_multirun_tspans_18a() {
        let mut doc = Document::new("TOP 18a", 120.0, 90.0, ColorMode::Rgb, 96.0);
        let host_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Host",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(10.0, 30.0),
                        Point2D::new(100.0, 30.0),
                    )),
                )
                .with_outline(OutlineStyle::solid(Color::BLACK, 0.3)),
                None,
            )
            .unwrap();

        let artistic = ArtisticText {
            runs: vec![
                TextRun::new(
                    "BoldRed",
                    TextStyle {
                        font_family: "Noto Sans".into(),
                        font_size_pt: 12.0,
                        bold: true,
                        fill: Color::rgba8(200, 20, 20, 255),
                        ..TextStyle::default()
                    },
                ),
                TextRun::new(
                    " ItalicBlue\nSecondLine",
                    TextStyle {
                        font_family: "DejaVu Sans".into(),
                        font_size_pt: 10.0,
                        italic: true,
                        fill: Color::rgba8(10, 40, 210, 255),
                        ..TextStyle::default()
                    },
                ),
            ],
            direction: TextDirection::Ltr,
            align: TextAlign::Left,
        };
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "MultiTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: artistic,
                    path_id: host_id,
                    offset_mm: 2.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            None,
        )
        .unwrap();

        let svg = export_svg_to_string(&doc, 0).unwrap();
        assert_eq!(
            svg.matches("<textPath ").count(),
            2,
            "2 lines => 2 <textPath> elems"
        );
        assert!(svg.contains("<tspan "));
        assert!(svg.contains("font-weight=\"bold\""));
        assert!(svg.contains("font-style=\"italic\""));
        assert!(svg.contains("#c81414"));
        assert!(svg.contains("#0a28d2"));
        assert!(svg.contains("SecondLine"));
        // Verify valid SVG via usvg re-import
        let imported = import_svg_str(&svg).unwrap();
        assert!(!imported.objects().is_empty());
    }

    #[test]
    fn svg_single_run_bold_italic_emits_attributes_on_text_element() {
        let mut doc = Document::new("SingleRunStyle", 120.0, 90.0, ColorMode::Rgb, 96.0);
        let bold_italic = TextStyle {
            font_family: "Noto Sans".into(),
            font_size_pt: 12.0,
            bold: true,
            italic: true,
            fill: Color::rgba8(20, 30, 40, 255),
            ..TextStyle::default()
        };
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "ArtBoldItalic",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "SoloBoldItalic",
                    bold_italic.clone(),
                )),
            )
            .with_transform(Affine2D::translate(10.0, 20.0)),
            None,
        )
        .unwrap();

        let host_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Host",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(10.0, 50.0),
                        Point2D::new(100.0, 50.0),
                    )),
                )
                .with_outline(OutlineStyle::solid(Color::BLACK, 0.25)),
                None,
            )
            .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "TopBoldItalic",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("PathBoldItalic", bold_italic),
                    path_id: host_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            None,
        )
        .unwrap();

        let svg = export_svg_to_string(&doc, 0).expect("export SVG");
        let art_line = svg
            .lines()
            .find(|l| l.contains("SoloBoldItalic"))
            .expect("ArtisticText <text> line");
        assert!(
            art_line.contains("font-weight=\"bold\"") && art_line.contains("font-style=\"italic\""),
            "single-run ArtisticText <text> missing bold/italic attrs: {art_line}"
        );
        let top_line = svg
            .lines()
            .find(|l| l.contains("PathBoldItalic"))
            .expect("TextOnPath <text> line");
        assert!(
            top_line.contains("font-weight=\"bold\"") && top_line.contains("font-style=\"italic\""),
            "single-run TextOnPath <text> missing bold/italic attrs: {top_line}"
        );
        let imported = import_svg_str(&svg).expect("re-import SVG");
        assert!(!imported.objects().is_empty());
    }

    #[test]
    fn svg_powerclip_and_clone_powerclip_roundtrip_preserves_clip_bounds() {
        let mut doc = Document::new("PowerClip SVG RT", 160.0, 80.0, ColorMode::Rgb, 72.0);
        let inner = SceneObject::new(
            ObjectId(0),
            "InnerRed",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 80.0, 80.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(230, 26, 26, 255)))
        .with_outline(OutlineStyle::none());
        let inner_id = doc.add_object(inner, None).unwrap();

        let container = SceneObject::new(
            ObjectId(0),
            "PowerClipMaster",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                20.0, 20.0, 40.0, 40.0,
            ))),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());
        let container_id = doc.add_object(container, None).unwrap();
        let master_id = doc.create_powerclip(container_id, &[inner_id]).unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "CloneOfPowerClip",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(80.0, 0.0)),
            None,
        )
        .unwrap();

        let svg = export_svg_to_string(&doc, 0).expect("export PowerClip SVG");
        assert!(svg.contains("<clipPath "), "SVG must emit <clipPath>");
        assert!(
            svg.contains("clip-path=\"url(#"),
            "SVG must reference clip-path"
        );

        let imported = import_svg_str(&svg).expect("import PowerClip SVG");
        let pc_count = imported
            .objects()
            .values()
            .filter(|o| matches!(o.payload, NodePayload::PowerClip { .. }))
            .count();
        assert_eq!(
            pc_count, 2,
            "both PowerClip master and translated Clone->PowerClip must import as PowerClip"
        );

        let rendered =
            coree_render::render_page_to_rgba(&imported, 0, 72.0, coree_render::ViewMode::Enhanced)
                .expect("render imported PowerClip SVG");
        let px_at = |x_mm: f64, y_mm: f64| -> [u8; 4] {
            let px = ((x_mm * 72.0 / 25.4).round() as u32).min(rendered.width - 1);
            let py = ((y_mm * 72.0 / 25.4).round() as u32).min(rendered.height - 1);
            let idx = ((py * rendered.width + px) * 4) as usize;
            [
                rendered.rgba[idx],
                rendered.rgba[idx + 1],
                rendered.rgba[idx + 2],
                rendered.rgba[idx + 3],
            ]
        };

        assert_eq!(
            px_at(40.0, 40.0),
            [230, 26, 26, 255],
            "master_in must be red"
        );
        assert_eq!(
            px_at(10.0, 10.0),
            [255, 255, 255, 255],
            "master_out must be clipped to white"
        );
        assert_eq!(
            px_at(120.0, 40.0),
            [230, 26, 26, 255],
            "clone_in must be red"
        );
        assert_eq!(
            px_at(90.0, 10.0),
            [255, 255, 255, 255],
            "clone_out must be clipped to white"
        );

        // Also test unnamed single-child <g clip-path="..."> and <clipPath transform="...">
        // plus <use> referencing a clipped group and <use> inside <clipPath>.
        let single_child_svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="100mm" viewBox="0 0 100 100">
  <defs>
    <rect id="clip_sub" x="5" y="5" width="25" height="25"/>
    <clipPath id="c_tf" transform="translate(15, 15)">
      <use href="#clip_sub"/>
    </clipPath>
    <g id="def_pc" clip-path="url(#c_tf)">
      <rect x="0" y="0" width="60" height="60" fill="#1020e0"/>
    </g>
  </defs>
  <g clip-path="url(#c_tf)">
    <rect x="0" y="0" width="60" height="60" fill="#e61a1a"/>
  </g>
  <use href="#def_pc" transform="translate(40, 40)"/>
</svg>"##;
        let sc_doc = import_svg_str(single_child_svg).expect("import single-child clip SVG");
        let sc_ren =
            coree_render::render_page_to_rgba(&sc_doc, 0, 72.0, coree_render::ViewMode::Enhanced)
                .expect("render single-child clip SVG");
        let sc_px = |x_mm: f64, y_mm: f64| -> [u8; 4] {
            let px = ((x_mm * 72.0 / 25.4).round() as u32).min(sc_ren.width - 1);
            let py = ((y_mm * 72.0 / 25.4).round() as u32).min(sc_ren.height - 1);
            let idx = ((py * sc_ren.width + px) * 4) as usize;
            [
                sc_ren.rgba[idx],
                sc_ren.rgba[idx + 1],
                sc_ren.rgba[idx + 2],
                sc_ren.rgba[idx + 3],
            ]
        };
        assert_eq!(sc_px(35.0, 35.0), [230, 26, 26, 255]);
        assert_eq!(sc_px(10.0, 10.0), [255, 255, 255, 255]);
        assert_eq!(sc_px(75.0, 75.0), [16, 32, 224, 255]);
        assert_eq!(sc_px(50.0, 50.0), [255, 255, 255, 255]);
    }

    #[test]
    fn tier5_svg_import_preserves_clip_rule_evenodd_donut() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="100mm" viewBox="0 0 100 100">
  <defs>
    <clipPath id="donut">
      <path d="M10,10 H90 V90 H10 Z M30,30 H70 V70 H30 Z" clip-rule="evenodd" fill-rule="evenodd"/>
    </clipPath>
  </defs>
  <g clip-path="url(#donut)">
    <rect x="0" y="0" width="100" height="100" fill="#ff0000"/>
  </g>
</svg>"##;
        let doc = import_svg_str(svg).expect("import evenodd donut clip SVG");
        let pc = doc
            .objects()
            .values()
            .find(|o| matches!(o.payload, NodePayload::PowerClip { .. }))
            .expect("expected PowerClip container");
        assert!(
            pc.clip_even_odd,
            "SVG clip-rule='evenodd' must set clip_even_odd=true on imported PowerClip"
        );
        let ren =
            coree_render::render_page_to_rgba(&doc, 0, 72.0, coree_render::ViewMode::Enhanced)
                .expect("render evenodd donut SVG");
        let px_at = |x_mm: f64, y_mm: f64| -> [u8; 4] {
            let px = ((x_mm * 72.0 / 25.4).round() as u32).min(ren.width - 1);
            let py = ((y_mm * 72.0 / 25.4).round() as u32).min(ren.height - 1);
            let idx = ((py * ren.width + px) * 4) as usize;
            [
                ren.rgba[idx],
                ren.rgba[idx + 1],
                ren.rgba[idx + 2],
                ren.rgba[idx + 3],
            ]
        };
        assert_eq!(
            px_at(20.0, 20.0),
            [255, 0, 0, 255],
            "outer ring must be red"
        );
        assert_eq!(
            px_at(50.0, 50.0),
            [255, 255, 255, 255],
            "even-odd inner hole must reveal paper white"
        );
    }

    #[test]
    fn fallback_xml_text_preserves_content_and_inherited_opacity_to_eps() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
  <rect x="5" y="5" width="20" height="10" fill="#000" opacity="0.2"/>
  <text x="10" y="30" font-family="Helvetica" font-size="16" fill="#000" opacity="0.2">TEST</text>
  <g opacity="0.5">
    <text x="10" y="42" font-family="NoSuchMissingFont" font-size="12" fill="#000" fill-opacity="0.4"><tspan>SUB</tspan><tspan>SPAN</tspan></text>
  </g>
</svg>"##;
        let doc = import_svg_str(svg).expect("import fallback XML text SVG");
        let texts: Vec<_> = doc
            .objects()
            .values()
            .filter_map(|o| match &o.payload {
                NodePayload::ArtisticText(at) => {
                    Some((at.plain_text(), o.opacity, at.runs[0].style.fill.alpha()))
                }
                _ => None,
            })
            .collect();
        assert!(
            texts
                .iter()
                .any(|(t, op, _)| t == "TEST" && (*op - 0.2).abs() < 1e-4),
            "expected 'TEST' with opacity ~0.2 (never 'TESTTEST'), got: {texts:?}"
        );
        assert!(
            texts.iter().any(|(t, op, fa)| t == "SUBSPAN"
                && (*op - 0.5).abs() < 1e-4
                && (*fa - 0.4).abs() < 1e-4),
            "expected 'SUBSPAN' with opacity ~0.5 and fill_alpha ~0.4, got: {texts:?}"
        );

        let eps = crate::export_eps_to_string(&doc, 0).expect("export SVG->EPS");
        assert!(
            eps.contains("(TEST) show"),
            "EPS must contain '(TEST) show':\n{eps}"
        );
        assert!(
            !eps.contains("TESTTEST"),
            "EPS must not contain duplicated TESTTEST"
        );
        assert!(
            eps.contains("0.8000 0.8000 0.8000 setrgbcolor"),
            "EPS must blend opaque black at 0.2 opacity to 0.8000 gray:\n{eps}"
        );
        assert!(
            !eps.contains("0.0000 0.0000 0.0000 setrgbcolor"),
            "EPS must not emit un-tinted 0.0000 black:\n{eps}"
        );
    }

    #[test]
    fn mixed_font_svg_preserves_usvg_and_fallback_texts_to_eps_and_pdf() {
        let mixed_helv = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
  <defs><text x="0" y="0" font-family="Helvetica">IGNORED_DEF</text></defs>
  <text x="10" y="20" font-family="Noto Sans" font-size="12" fill="#000" opacity="0.2">KNOWN</text>
  <text x="10" y="40" font-family="Helvetica" font-size="12" fill="#000" opacity="0.2">HELVETICA_TEXT</text>
</svg>"##;
        let doc = import_svg_str(mixed_helv).expect("import mixed Noto Sans + Helvetica SVG");
        assert_eq!(
            doc.objects().len(),
            2,
            "mixed-font SVG must keep both Noto Sans and Helvetica texts (and ignore <defs>)"
        );
        let texts: Vec<_> = doc
            .objects()
            .values()
            .filter_map(|o| match &o.payload {
                NodePayload::ArtisticText(at) => Some((at.plain_text(), o.opacity)),
                _ => None,
            })
            .collect();
        assert!(
            texts
                .iter()
                .any(|(t, op)| t == "KNOWN" && (*op - 0.2).abs() < 1e-4),
            "missing Noto Sans KNOWN text: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|(t, op)| t == "HELVETICA_TEXT" && (*op - 0.2).abs() < 1e-4),
            "missing Helvetica HELVETICA_TEXT: {texts:?}"
        );

        let eps = crate::export_eps_to_string(&doc, 0).expect("export mixed SVG -> EPS");
        assert!(
            eps.contains("(KNOWN) show"),
            "EPS must contain '(KNOWN) show':\n{eps}"
        );
        assert!(
            eps.contains("(HELVETICA_TEXT) show"),
            "EPS must contain '(HELVETICA_TEXT) show':\n{eps}"
        );
        assert!(
            eps.contains("0.8000 0.8000 0.8000 setrgbcolor"),
            "EPS must blend 0.2 opacity black to 0.8000 gray:\n{eps}"
        );
        assert!(
            !eps.contains("0.0000 0.0000 0.0000 setrgbcolor"),
            "EPS must not emit un-tinted 0.0000 black:\n{eps}"
        );
        let pdf_bytes = crate::export_pdf_to_bytes(&doc, &crate::PdfExportOptions::default())
            .expect("export mixed SVG -> PDF");
        assert!(
            pdf_bytes.starts_with(b"%PDF-"),
            "PDF export must emit valid %PDF- header"
        );

        let arial_dejavu = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
  <text x="10" y="20" font-family="DejaVu Sans" font-size="12" fill="#000">DEJAVU</text>
  <text x="10" y="40" font-family="Arial" font-size="12" fill="#000">ARIAL_TEXT</text>
</svg>"##;
        let doc2 = import_svg_str(arial_dejavu).expect("import mixed DejaVu Sans + Arial SVG");
        assert_eq!(
            doc2.objects().len(),
            2,
            "mixed DejaVu Sans + Arial SVG must preserve both text objects"
        );
        let names2: Vec<String> = doc2
            .objects()
            .values()
            .filter_map(|o| match &o.payload {
                NodePayload::ArtisticText(at) => Some(at.plain_text()),
                _ => None,
            })
            .collect();
        assert!(names2.contains(&"DEJAVU".to_owned()), "got {names2:?}");
        assert!(names2.contains(&"ARIAL_TEXT".to_owned()), "got {names2:?}");

        // Identical text without id where missing font precedes installed font
        let same_text = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
  <text x="10" y="20" font-family="Helvetica" font-size="12" fill="#000">SAME</text>
  <text x="10" y="40" font-family="Noto Sans" font-size="12" fill="#000">SAME</text>
</svg>"##;
        let doc3 = import_svg_str(same_text).expect("import identical text mixed-font SVG");
        assert_eq!(
            doc3.objects().len(),
            2,
            "identical text with mixed fonts must keep exactly 2 objects"
        );
    }

    #[test]
    fn fallback_xml_tspan_preserves_per_tspan_opacity_and_fill_opacity() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
  <text x="10" y="20" font-family="Helvetica" font-size="16" fill="#000"><tspan opacity="0.2">TSPAN_OP</tspan></text>
  <text x="10" y="40" font-family="NoSuchMissingFont" font-size="12" fill="#000"><tspan opacity="0.2">FIRST</tspan><tspan opacity="0.8">SECOND</tspan></text>
</svg>"##;
        let doc = import_svg_str(svg).expect("import tspan opacity fallback SVG");
        assert_eq!(doc.objects().len(), 2);

        let tspan_op = doc
            .objects()
            .values()
            .find(|o| matches!(&o.payload, NodePayload::ArtisticText(at) if at.plain_text() == "TSPAN_OP"))
            .expect("expected TSPAN_OP object");
        assert!(
            (tspan_op.opacity - 0.2).abs() < 1e-4,
            "single-opacity <tspan opacity='0.2'> must fold to ~0.2, got {}",
            tspan_op.opacity
        );

        let multi_tspan = doc
            .objects()
            .values()
            .find_map(|o| match &o.payload {
                NodePayload::ArtisticText(at) if at.plain_text() == "FIRSTSECOND" => Some(at),
                _ => None,
            })
            .expect("expected FIRSTSECOND multi-tspan object");
        assert_eq!(multi_tspan.runs.len(), 2);
        assert!((multi_tspan.runs[0].style.fill.alpha() - 0.2).abs() < 1e-4);
        assert!((multi_tspan.runs[1].style.fill.alpha() - 0.8).abs() < 1e-4);

        let eps = crate::export_eps_to_string(&doc, 0).expect("export tspan opacity SVG -> EPS");
        assert!(
            eps.contains("(TSPAN_OP) show"),
            "EPS must contain '(TSPAN_OP) show':\n{eps}"
        );
        assert!(
            eps.contains("0.8000 0.8000 0.8000 setrgbcolor"),
            "EPS must blend <tspan opacity='0.2'> black to 0.8000 gray:\n{eps}"
        );
    }

    #[test]
    fn svg_200k_use_bomb_rejects_with_limit_exceeded() {
        let g1_uses = "<use href=\"#p\"/>".repeat(10);
        let g2_uses = "<use href=\"#g1\"/>".repeat(10);
        let g3_uses = "<use href=\"#g2\"/>".repeat(10);
        let g4_uses = "<use href=\"#g3\"/>".repeat(10);
        let top_uses = "<use href=\"#g4\"/>".repeat(20);
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <defs>
    <path id="p" d="M0,0 L1,1"/>
    <g id="g1">{g1_uses}</g>
    <g id="g2">{g2_uses}</g>
    <g id="g3">{g3_uses}</g>
    <g id="g4">{g4_uses}</g>
  </defs>
  {top_uses}
</svg>"#
        );
        let err = import_svg_str(&svg).unwrap_err();
        assert!(
            matches!(err, IoError::LimitExceeded { ref limit, .. } if limit.contains("max_svg_objects")),
            "expected LimitExceeded(max_svg_objects) on 200k <use> bomb, got {err:?}"
        );
    }

    #[test]
    fn svg_use_vertex_amplification_bomb_rejects_with_limit_exceeded() {
        let mut d = String::from("M0,0");
        for i in 1..=200 {
            d.push_str(&format!(" L{},{}", i % 10, i / 10));
        }
        let g1_uses = "<use href=\"#p\"/>".repeat(10);
        let g2_uses = "<use href=\"#g1\"/>".repeat(10);
        let g3_uses = "<use href=\"#g2\"/>".repeat(10);
        let g4_uses = "<use href=\"#g3\"/>".repeat(10);
        let top_uses = "<use href=\"#g4\"/>".repeat(10);
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <defs>
    <path id="p" d="{d}"/>
    <g id="g1">{g1_uses}</g>
    <g id="g2">{g2_uses}</g>
    <g id="g3">{g3_uses}</g>
    <g id="g4">{g4_uses}</g>
  </defs>
  {top_uses}
</svg>"#
        );
        let err = import_svg_str(&svg).unwrap_err();
        assert!(
            matches!(err, IoError::LimitExceeded { ref limit, .. } if limit.contains("max_svg_vertices")),
            "expected LimitExceeded(max_svg_vertices) on 100k x 200-seg <use> bomb, got {err:?}"
        );

        // Cyclic <use> must terminate safely without stack overflow or hang
        let cyclic = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <defs>
    <g id="a"><use href="#b"/></g>
    <g id="b"><use href="#a"/></g>
  </defs>
  <use href="#a"/>
  <rect x="0" y="0" width="10" height="10" fill="#000"/>
</svg>"##;
        let _ = import_svg_str(cyclic);
    }
}
