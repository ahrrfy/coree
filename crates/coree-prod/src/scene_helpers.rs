// SPDX-License-Identifier: MPL-2.0

//! Internal helpers shared across `coree-prod` modules for safe scene-graph traversal,
//! deep page/object cloning with ID remapping, and `tiny-skia` path conversion.

use crate::error::{ProdError, ProdResult};
use coree_core::{
    Affine2D, BezierPath, Document, Layer, LayerId, MasterLayerScope, NodePayload, ObjectId, Page,
    PageId, Rect2D, SceneObject,
};
use kurbo::PathEl;
use std::collections::{HashMap, HashSet};

/// Maximum recursion depth for nested groups / PowerClips.
pub(crate) const MAX_SCENE_DEPTH: usize = 64;

/// Ensures `page_index` exists in `doc` and returns a reference to the page.
pub(crate) fn get_page(doc: &Document, page_index: usize) -> ProdResult<&Page> {
    doc.pages.get(page_index).ok_or(ProdError::PageOutOfRange {
        index: page_index,
        total: doc.pages.len(),
    })
}

/// Finds an existing layer on `page_index` by case-insensitive name, or creates a new one.
pub(crate) fn find_or_create_layer(
    doc: &mut Document,
    page_index: usize,
    name: &str,
    is_cut_layer: bool,
) -> ProdResult<LayerId> {
    let page = get_page(doc, page_index)?;
    let page_id = page.id;
    if let Some(existing) = page
        .layers
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case(name))
    {
        return Ok(existing.id);
    }
    let layer_id = doc.add_layer(page_id, name, is_cut_layer, MasterLayerScope::None)?;
    Ok(layer_id)
}

/// Creates a new blank document inheriting metadata/settings/assets/symbols from `src`,
/// with zero pages initially (caller appends imposed/merged pages).
pub(crate) fn clone_document_shell(src: &Document, title: impl Into<String>) -> Document {
    let mut dst = Document::new(title, 210.0, 297.0, src.color_mode, src.dpi);
    dst.default_unit = src.default_unit;
    dst.pages.clear();
    dst.active_page_index = 0;
    dst.multipage_view = src.multipage_view;
    dst.assets = src.assets.clone();
    dst.palettes = src.palettes.clone();
    dst.document_palette = src.document_palette.clone();
    dst.color_styles = src.color_styles.clone();
    dst.object_styles = src.object_styles.clone();
    dst.symbols = src.symbols.clone();
    dst.grid = src.grid;
    dst.perspective_grid = src.perspective_grid.clone();
    dst.metadata = src.metadata.clone();
    dst
}

/// Deep-clones all objects from `src_doc.pages[src_page_index]` into `dst_doc.pages[dst_page_index]`,
/// applying `slot_transform` to top-level objects and optionally filtering by `clip_window_src`
/// (in source-page mm coordinates).
///
/// Preserves layer names and flags, and remaps internal `ObjectId` links (`Group`, `PowerClip`,
/// `TextOnPath`, `Connector`, `ParagraphText::linked_next_frame`).
pub(crate) fn clone_page_contents_into(
    src_doc: &Document,
    src_page_index: usize,
    dst_doc: &mut Document,
    dst_page_index: usize,
    slot_transform: Affine2D,
    clip_window_src: Option<Rect2D>,
) -> ProdResult<Vec<ObjectId>> {
    let src_page = get_page(src_doc, src_page_index)?;
    let dst_page_id = get_page(dst_doc, dst_page_index)?.id;

    // Collect effective layers on source page: its own layers plus applicable master layers
    // from other pages.
    let effective_layers = collect_effective_layers(src_doc, src_page_index);

    let mut id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut created_top_level: Vec<ObjectId> = Vec::new();
    let mut layer_placements: Vec<(LayerId, Vec<ObjectId>)> = Vec::new();

    for src_layer in effective_layers {
        // Ensure destination page has a matching layer
        let dst_layer_id = ensure_layer_matching(dst_doc, dst_page_id, src_layer)?;
        let mut placed_on_layer = Vec::new();

        for &top_id in &src_layer.objects {
            if let Some(win) = clip_window_src {
                if let Some(bb) = src_doc.object_bounds(top_id) {
                    if !bb.intersects(win) {
                        continue;
                    }
                }
            }
            if let Some(new_id) = clone_object_tree(
                src_doc,
                top_id,
                dst_doc,
                &mut id_map,
                true,
                slot_transform,
                0,
            ) {
                placed_on_layer.push(new_id);
                created_top_level.push(new_id);
            }
        }
        if !placed_on_layer.is_empty() {
            layer_placements.push((dst_layer_id, placed_on_layer));
        }
    }

    // Pass 2: remap all cross-object references among newly cloned objects
    let new_ids: Vec<ObjectId> = id_map.values().copied().collect();
    for new_id in new_ids {
        if let Some(obj) = dst_doc.objects.get_mut(&new_id) {
            remap_object_references(obj, &id_map);
        }
    }

    // Attach top-level clones to destination layers
    for (dst_layer_id, obj_ids) in layer_placements {
        if let Some(layer) = dst_doc.find_layer_mut(dst_layer_id) {
            layer.objects.extend(obj_ids);
        }
    }

    // Remove empty default "Layer 1" if source page did not have one and other layers were created
    if let Some(dst_page) = dst_doc.pages.get_mut(dst_page_index) {
        let src_has_layer1 = src_page.layers.iter().any(|l| l.name == "Layer 1");
        if !src_has_layer1 && dst_page.layers.len() > 1 {
            if let Some(pos) = dst_page
                .layers
                .iter()
                .position(|l| l.name == "Layer 1" && l.objects.is_empty())
            {
                dst_page.layers.remove(pos);
            }
        }
    }

    Ok(created_top_level)
}

fn ensure_layer_matching(
    dst_doc: &mut Document,
    dst_page_id: PageId,
    src_layer: &Layer,
) -> ProdResult<LayerId> {
    if let Some(page) = dst_doc.pages.iter().find(|p| p.id == dst_page_id) {
        if let Some(existing) = page.layers.iter().find(|l| {
            l.name == src_layer.name
                && l.is_cut_layer == src_layer.is_cut_layer
                && l.printable == src_layer.printable
        }) {
            return Ok(existing.id);
        }
    }
    let lid = dst_doc.add_layer(
        dst_page_id,
        src_layer.name.clone(),
        src_layer.is_cut_layer,
        MasterLayerScope::None,
    )?;
    dst_doc.set_layer_flags(
        lid,
        Some(src_layer.visible),
        Some(src_layer.locked),
        Some(src_layer.printable),
        Some(src_layer.is_cut_layer),
    )?;
    Ok(lid)
}

fn clone_object_tree(
    src_doc: &Document,
    src_id: ObjectId,
    dst_doc: &mut Document,
    id_map: &mut HashMap<ObjectId, ObjectId>,
    is_top_level: bool,
    slot_transform: Affine2D,
    depth: usize,
) -> Option<ObjectId> {
    if depth > MAX_SCENE_DEPTH {
        return None;
    }
    if let Some(&existing) = id_map.get(&src_id) {
        return Some(existing);
    }
    let src_obj = src_doc.objects.get(&src_id)?;
    let new_id = dst_doc.alloc_object_id();
    id_map.insert(src_id, new_id);

    let mut cloned = src_obj.clone();
    cloned.id = new_id;
    if is_top_level {
        cloned.transform = slot_transform.compose(&cloned.transform);
    }

    // Recursively clone Group / PowerClip children (children keep parent-local transforms)
    match &cloned.payload {
        NodePayload::Group(children) => {
            let child_ids = children.clone();
            for cid in child_ids {
                clone_object_tree(
                    src_doc,
                    cid,
                    dst_doc,
                    id_map,
                    false,
                    Affine2D::IDENTITY,
                    depth + 1,
                );
            }
        }
        NodePayload::PowerClip { contents, .. } => {
            let content_ids = contents.clone();
            for cid in content_ids {
                clone_object_tree(
                    src_doc,
                    cid,
                    dst_doc,
                    id_map,
                    false,
                    Affine2D::IDENTITY,
                    depth + 1,
                );
            }
        }
        _ => {}
    }

    dst_doc.objects.insert(new_id, cloned);
    Some(new_id)
}

fn remap_object_references(obj: &mut SceneObject, id_map: &HashMap<ObjectId, ObjectId>) {
    match &mut obj.payload {
        NodePayload::Group(children) => {
            *children = children
                .iter()
                .filter_map(|old| id_map.get(old).copied())
                .collect();
        }
        NodePayload::PowerClip { contents, .. } => {
            *contents = contents
                .iter()
                .filter_map(|old| id_map.get(old).copied())
                .collect();
        }
        NodePayload::TextOnPath(top) => {
            if let Some(&mapped) = id_map.get(&top.path_id) {
                top.path_id = mapped;
            }
        }
        NodePayload::ParagraphText(p) => {
            if let Some(old_next) = p.linked_next_frame {
                p.linked_next_frame = id_map.get(&old_next).copied();
            }
        }
        NodePayload::Connector(c) => {
            if let Some(old_from) = c.from_obj {
                c.from_obj = id_map.get(&old_from).copied();
            }
            if let Some(old_to) = c.to_obj {
                c.to_obj = id_map.get(&old_to).copied();
            }
        }
        _ => {}
    }
}

/// Returns effective layers rendered on `page_index`: applicable master layers from other
/// pages (bottom) followed by the page's own layers.
pub(crate) fn collect_effective_layers(doc: &Document, page_index: usize) -> Vec<&Layer> {
    let mut layers = Vec::new();
    let page_num_1based = page_index + 1;
    for (idx, page) in doc.pages.iter().enumerate() {
        if idx == page_index {
            continue;
        }
        for layer in &page.layers {
            let applies = match layer.master_scope {
                MasterLayerScope::None => false,
                MasterLayerScope::AllPages => true,
                MasterLayerScope::OddPages => page_num_1based % 2 == 1,
                MasterLayerScope::EvenPages => page_num_1based.is_multiple_of(2),
            };
            if applies {
                layers.push(layer);
            }
        }
    }
    if let Some(page) = doc.pages.get(page_index) {
        layers.extend(page.layers.iter());
    }
    layers
}

/// Visits every object (including descendants inside `Group` and `PowerClip`) on `layer`,
/// invoking `visitor(object_id, scene_object, world_transform)`.
pub(crate) fn visit_layer_objects<F>(doc: &Document, layer: &Layer, visitor: &mut F)
where
    F: FnMut(ObjectId, &SceneObject, Affine2D),
{
    let mut visiting = HashSet::new();
    for &oid in &layer.objects {
        visit_object_recursive(doc, oid, Affine2D::IDENTITY, 0, &mut visiting, visitor);
    }
}

pub(crate) fn visit_object_recursive<F>(
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
    if let Some(obj) = doc.objects.get(&id) {
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

/// Computes the world-space axis-aligned bounding box of `obj` given its composed `world_xf`
/// (unlike [`Document::object_bounds`], this properly incorporates ancestor `Group` / `PowerClip`
/// transforms when inspecting descendant nodes).
pub(crate) fn world_bounds_with_xf(
    doc: &Document,
    id: ObjectId,
    obj: &SceneObject,
    world_xf: &Affine2D,
) -> Option<Rect2D> {
    if !world_xf.is_finite() {
        return None;
    }
    let bounds = match &obj.payload {
        NodePayload::Path(p) => p.transformed(world_xf).bounding_box(),
        NodePayload::Shape(s) => s.to_bezier_path().transformed(world_xf).bounding_box(),
        NodePayload::ArtisticText(t) => world_xf.try_transform_rect(t.estimated_bounds()),
        NodePayload::ParagraphText(t) => world_xf.try_transform_rect(t.bounding_box()),
        NodePayload::TextOnPath(top) => doc
            .object_bounds(top.path_id)
            .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds())),
        NodePayload::Raster(r) => {
            let rect = match r.crop_rect {
                Some(cr) if cr.is_finite() => Some(cr),
                Some(_) => None,
                None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                    Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                }
                None => None,
            };
            rect.and_then(|rc| world_xf.try_transform_rect(rc))
        }
        NodePayload::Table(t) => world_xf.try_transform_rect(t.to_bounding_box()),
        NodePayload::Dimension(d) => world_xf.try_transform_rect(d.bounding_box()),
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints(c);
            c.route_path(a, b).bounding_box()
        }
        NodePayload::PowerClip { container, .. } => container.transformed(world_xf).bounding_box(),
        NodePayload::SymbolInstance(sym_id) => doc.symbols.get(sym_id).and_then(|sym| {
            let mut acc: Option<Rect2D> = None;
            for sub in &sym.objects {
                let sub_xf = world_xf.compose(&sub.transform);
                if let Some(r) = world_bounds_with_xf(doc, id, sub, &sub_xf) {
                    acc = Some(match acc {
                        Some(prev) => prev.union(r),
                        None => r,
                    });
                }
            }
            acc
        }),
        NodePayload::Group(_) => doc.object_bounds(id),
    };
    bounds.filter(|r| r.is_finite())
}

/// Converts a `coree_core::BezierPath` in millimeters into a `tiny_skia::Path` scaled to pixels.
pub(crate) fn bezier_to_skia_path(
    path: &BezierPath,
    scale_x: f32,
    scale_y: f32,
) -> Option<tiny_skia::Path> {
    if path.is_empty() {
        return None;
    }
    let kb = path.to_kurbo();
    let mut pb = tiny_skia::PathBuilder::new();
    let sx = f64::from(scale_x);
    let sy = f64::from(scale_y);
    for el in kb.elements() {
        match *el {
            PathEl::MoveTo(p) => pb.move_to((p.x * sx) as f32, (p.y * sy) as f32),
            PathEl::LineTo(p) => pb.line_to((p.x * sx) as f32, (p.y * sy) as f32),
            PathEl::QuadTo(p1, p2) => pb.quad_to(
                (p1.x * sx) as f32,
                (p1.y * sy) as f32,
                (p2.x * sx) as f32,
                (p2.y * sy) as f32,
            ),
            PathEl::CurveTo(p1, p2, p3) => pb.cubic_to(
                (p1.x * sx) as f32,
                (p1.y * sy) as f32,
                (p2.x * sx) as f32,
                (p2.y * sy) as f32,
                (p3.x * sx) as f32,
                (p3.y * sy) as f32,
            ),
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish()
}

/// Extracts world-space closed/open vector silhouette paths for `id` (expanding groups and
/// falling back to bounding-box rects for text/raster/table objects).
pub(crate) fn collect_silhouette_paths(
    doc: &Document,
    id: ObjectId,
    parent_xf: Affine2D,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    out: &mut Vec<BezierPath>,
) {
    if depth > MAX_SCENE_DEPTH || !visiting.insert(id) {
        return;
    }
    if let Some(obj) = doc.objects.get(&id) {
        let world_xf = parent_xf.compose(&obj.transform);
        match &obj.payload {
            NodePayload::Path(p) => {
                if !p.is_empty() {
                    out.push(p.transformed(&world_xf));
                }
            }
            NodePayload::Shape(s) => {
                let p = s.to_bezier_path().transformed(&world_xf);
                if !p.is_empty() {
                    out.push(p);
                }
            }
            NodePayload::PowerClip { container, .. } => {
                let p = container.transformed(&world_xf);
                if !p.is_empty() {
                    out.push(p);
                }
            }
            NodePayload::ArtisticText(t) => {
                out.push(BezierPath::from_rect(t.estimated_bounds()).transformed(&world_xf));
            }
            NodePayload::ParagraphText(t) => {
                out.push(BezierPath::from_rect(t.bounding_box()).transformed(&world_xf));
            }
            NodePayload::Raster(r) => {
                let rect = r
                    .crop_rect
                    .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm));
                out.push(BezierPath::from_rect(rect).transformed(&world_xf));
            }
            NodePayload::Table(t) => {
                out.push(BezierPath::from_rect(t.to_bounding_box()).transformed(&world_xf));
            }
            NodePayload::Group(children) => {
                for &cid in children {
                    collect_silhouette_paths(doc, cid, world_xf, depth + 1, visiting, out);
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if let Some(sym) = doc.symbols.get(sym_id) {
                    for sub in &sym.objects {
                        let sub_xf = world_xf.compose(&sub.transform);
                        match &sub.payload {
                            NodePayload::Path(p) => out.push(p.transformed(&sub_xf)),
                            NodePayload::Shape(s) => {
                                out.push(s.to_bezier_path().transformed(&sub_xf))
                            }
                            _ => {}
                        }
                    }
                }
            }
            NodePayload::TextOnPath(top) => {
                if let Some(bb) = doc.object_bounds(id) {
                    out.push(BezierPath::from_rect(bb));
                } else {
                    out.push(
                        BezierPath::from_rect(top.text.estimated_bounds()).transformed(&world_xf),
                    );
                }
            }
            NodePayload::Dimension(_) | NodePayload::Connector(_) => {}
        }
    }
    visiting.remove(&id);
}
