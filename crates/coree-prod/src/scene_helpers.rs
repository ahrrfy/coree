// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal helpers shared across `coree-prod` modules for safe scene-graph traversal,
//! deep page/object cloning with ID remapping, and `tiny-skia` path conversion.

use crate::error::{ProdError, ProdResult};
use coree_core::{
    Affine2D, BezierPath, Document, Effect, Layer, LayerId, MasterLayerScope, NodePayload,
    ObjectId, Page, PageId, Perspective3x3, Rect2D, SceneObject, SymbolId,
};
use kurbo::PathEl;
use std::collections::{HashMap, HashSet};

/// Maximum recursion depth for nested groups / PowerClips.
pub(crate) const MAX_SCENE_DEPTH: usize = 64;

/// Ensures `page_index` exists in `doc` and returns a reference to the page.
pub(crate) fn get_page(doc: &Document, page_index: usize) -> ProdResult<&Page> {
    doc.page(page_index).ok_or(ProdError::PageOutOfRange {
        index: page_index,
        total: doc.pages().len(),
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
    let mut dst = Document::new(title, 210.0, 297.0, src.color_mode(), src.dpi());
    dst.clone_shell_from(src);
    dst.clear_pages_for_load();
    let _ = dst.set_active_page_index(0);
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
                if let Some(bb) = src_doc.world_object_bounds(top_id) {
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
    dst_doc.remap_object_references(&id_map);

    // Attach top-level clones to destination layers
    for (dst_layer_id, obj_ids) in layer_placements {
        if let Some(layer) = dst_doc.find_layer_mut(dst_layer_id) {
            layer.objects.extend(obj_ids);
        }
    }

    // Remove empty default "Layer 1" if source page did not have one and other layers were created
    if let Some(dst_page) = dst_doc.page(dst_page_index) {
        let src_has_layer1 = src_page.layers.iter().any(|l| l.name == "Layer 1");
        if !src_has_layer1 && dst_page.layers.len() > 1 {
            if let Some(pos) = dst_page
                .layers
                .iter()
                .position(|l| l.name == "Layer 1" && l.objects.is_empty())
            {
                dst_doc.remove_page_layer(dst_page_index, pos);
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
    if let Some(page) = dst_doc.page_by_id(dst_page_id) {
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
    let src_obj = src_doc.object(src_id)?;
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

    let _ = dst_doc.insert_loaded_object(cloned);
    Some(new_id)
}

/// Returns effective layers rendered on `page_index`: applicable master/desktop layers from other
/// pages (bottom) followed by the page's own applicable layers.
pub(crate) fn collect_effective_layers(doc: &Document, page_index: usize) -> Vec<&Layer> {
    let mut layers = Vec::new();
    let is_odd = page_index.is_multiple_of(2); // 1-based Page 1 = index 0 = Odd
    for (idx, page) in doc.pages().iter().enumerate() {
        if idx == page_index {
            continue;
        }
        for layer in &page.layers {
            let applies = layer.is_desktop_layer
                || match layer.master_scope {
                    MasterLayerScope::None => false,
                    MasterLayerScope::AllPages => true,
                    MasterLayerScope::OddPages => is_odd,
                    MasterLayerScope::EvenPages => !is_odd,
                };
            if applies {
                layers.push(layer);
            }
        }
    }
    if let Some(page) = doc.page(page_index) {
        for layer in &page.layers {
            let applies = layer.is_desktop_layer
                || match layer.master_scope {
                    MasterLayerScope::None => true,
                    MasterLayerScope::AllPages => true,
                    MasterLayerScope::OddPages => is_odd,
                    MasterLayerScope::EvenPages => !is_odd,
                };
            if applies {
                layers.push(layer);
            }
        }
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SymParentRef {
    Id(ObjectId),
    RootIndex(usize),
}

pub(crate) struct SymBoundsCtx<'a> {
    pub(crate) root_xf: &'a Affine2D,
    pub(crate) sym_objects: &'a [SceneObject],
    pub(crate) sym_map: &'a HashMap<ObjectId, &'a SceneObject>,
    pub(crate) sym_parent_map: &'a HashMap<ObjectId, SymParentRef>,
}

pub(crate) fn sym_world_transform_of(
    id: ObjectId,
    sym_root_xf: &Affine2D,
    sym_objects: &[SceneObject],
    sym_map: &HashMap<ObjectId, &SceneObject>,
    sym_parent_map: &HashMap<ObjectId, SymParentRef>,
) -> Option<Affine2D> {
    let mut chain = Vec::new();
    let mut cur = SymParentRef::Id(id);
    let mut seen = HashSet::new();
    while seen.insert(cur) {
        let obj = match cur {
            SymParentRef::Id(oid) => sym_map.get(&oid).copied()?,
            SymParentRef::RootIndex(idx) => sym_objects.get(idx)?,
        };
        chain.push(obj.transform);
        match cur {
            SymParentRef::Id(oid) => match sym_parent_map.get(&oid).copied() {
                Some(pid) => cur = pid,
                None => break,
            },
            SymParentRef::RootIndex(_) => break,
        }
    }
    let mut xf = *sym_root_xf;
    for local in chain.into_iter().rev() {
        xf = xf.compose(&local);
    }
    Some(xf)
}

pub(crate) fn world_bounds_with_sym_xf(
    doc: &Document,
    id: ObjectId,
    obj: &SceneObject,
    world_xf: &Affine2D,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
) -> Option<Rect2D> {
    let mut visiting = HashSet::new();
    let mut visiting_syms = HashSet::new();
    world_bounds_with_xf_inner(
        doc,
        id,
        obj,
        world_xf,
        sym_ctx,
        0,
        &mut visiting,
        &mut visiting_syms,
    )
}

#[allow(clippy::too_many_arguments)]
fn world_bounds_with_xf_inner(
    doc: &Document,
    id: ObjectId,
    obj: &SceneObject,
    world_xf: &Affine2D,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
) -> Option<Rect2D> {
    if depth > MAX_SCENE_DEPTH || !world_xf.is_finite() {
        return None;
    }
    let bounds = match &obj.payload {
        NodePayload::Path(p) => {
            let bp = apply_obj_perspective_to_local_path(p.clone(), obj);
            bp.transformed(world_xf).bounding_box()
        }
        NodePayload::Shape(s) => {
            let bp = apply_obj_perspective_to_local_path(s.to_bezier_path(), obj);
            bp.transformed(world_xf).bounding_box()
        }
        NodePayload::BSpline(bs) => {
            let bp = apply_obj_perspective_to_local_path(bs.to_bezier_path(), obj);
            bp.transformed(world_xf).bounding_box()
        }
        NodePayload::ArtisticMedia { spine, style } => {
            let p = coree_core::ArtisticMedia::expand_stroke(spine, style);
            let bp = apply_obj_perspective_to_local_path(p, obj);
            bp.transformed(world_xf).bounding_box()
        }
        NodePayload::Clone { .. } => doc
            .resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|c| c.sym_map))
            .and_then(|(p, f, o, in_sym)| {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                world_bounds_with_xf_inner(
                    doc,
                    tmp.id,
                    &tmp,
                    world_xf,
                    if in_sym { sym_ctx } else { None },
                    depth + 1,
                    visiting,
                    visiting_syms,
                )
            }),
        NodePayload::ArtisticText(t) => {
            apply_obj_perspective_to_local_path(BezierPath::from_rect(t.estimated_bounds()), obj)
                .transformed(world_xf)
                .bounding_box()
        }
        NodePayload::ParagraphText(t) => {
            apply_obj_perspective_to_local_path(BezierPath::from_rect(t.bounding_box()), obj)
                .transformed(world_xf)
                .bounding_box()
        }
        NodePayload::TextOnPath(top) => {
            if let Some(ctx) = sym_ctx {
                if let Some(path_obj) = ctx.sym_map.get(&top.path_id).copied() {
                    sym_world_transform_of(
                        top.path_id,
                        ctx.root_xf,
                        ctx.sym_objects,
                        ctx.sym_map,
                        ctx.sym_parent_map,
                    )
                    .and_then(|p_xf| match &path_obj.payload {
                        NodePayload::Path(p) => {
                            apply_obj_perspective_to_local_path(p.clone(), path_obj)
                                .transformed(&p_xf)
                                .bounding_box()
                        }
                        NodePayload::Shape(s) => {
                            apply_obj_perspective_to_local_path(s.to_bezier_path(), path_obj)
                                .transformed(&p_xf)
                                .bounding_box()
                        }
                        NodePayload::BSpline(bs) => {
                            apply_obj_perspective_to_local_path(bs.to_bezier_path(), path_obj)
                                .transformed(&p_xf)
                                .bounding_box()
                        }
                        NodePayload::ArtisticMedia { spine, style } => {
                            apply_obj_perspective_to_local_path(
                                coree_core::ArtisticMedia::expand_stroke(spine, style),
                                path_obj,
                            )
                            .transformed(&p_xf)
                            .bounding_box()
                        }
                        NodePayload::PowerClip { container, .. } => {
                            apply_obj_perspective_to_local_path((**container).clone(), path_obj)
                                .transformed(&p_xf)
                                .bounding_box()
                        }
                        NodePayload::Clone { .. } => doc
                            .resolve_clone_payload_and_style_with_sym_scope(
                                path_obj,
                                Some(ctx.sym_map),
                            )
                            .and_then(|(cp, cf, co, in_sym)| {
                                let mut tmp = path_obj.clone();
                                tmp.payload = cp;
                                tmp.fill = cf;
                                tmp.outline = co;
                                world_bounds_with_xf_inner(
                                    doc,
                                    top.path_id,
                                    &tmp,
                                    &p_xf,
                                    in_sym.then_some(ctx),
                                    depth + 1,
                                    visiting,
                                    visiting_syms,
                                )
                            }),
                        _ => {
                            if !visiting.insert(top.path_id) {
                                return None;
                            }
                            let r = world_bounds_with_xf_inner(
                                doc,
                                top.path_id,
                                path_obj,
                                &p_xf,
                                Some(ctx),
                                depth + 1,
                                visiting,
                                visiting_syms,
                            );
                            visiting.remove(&top.path_id);
                            r
                        }
                    })
                    .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
                } else {
                    doc.world_object_bounds(top.path_id)
                        .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
                }
            } else {
                doc.world_object_bounds(top.path_id)
                    .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
            }
        }
        NodePayload::Raster(r) => {
            let rect = match r.crop_rect {
                Some(cr) if cr.is_finite() => Some(cr),
                Some(_) => None,
                None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                    Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                }
                None => None,
            };
            rect.and_then(|rc| {
                apply_obj_perspective_to_local_path(BezierPath::from_rect(rc), obj)
                    .transformed(world_xf)
                    .bounding_box()
            })
        }
        NodePayload::Table(t) => {
            apply_obj_perspective_to_local_path(BezierPath::from_rect(t.to_bounding_box()), obj)
                .transformed(world_xf)
                .bounding_box()
        }
        NodePayload::Dimension(d) => {
            apply_obj_perspective_to_local_path(BezierPath::from_rect(d.bounding_box()), obj)
                .transformed(world_xf)
                .bounding_box()
        }
        NodePayload::Connector(c) => {
            if let Some(ctx) = sym_ctx {
                let mut resolve_sym_endpoint =
                    |oid_opt: Option<ObjectId>,
                     fallback: coree_core::Point2D,
                     anchor: coree_core::AnchorPoint| {
                        oid_opt
                            .and_then(|oid| {
                                if let Some(sub) = ctx.sym_map.get(&oid).copied() {
                                    if !visiting.insert(oid) {
                                        return None;
                                    }
                                    let res = sym_world_transform_of(
                                        oid,
                                        ctx.root_xf,
                                        ctx.sym_objects,
                                        ctx.sym_map,
                                        ctx.sym_parent_map,
                                    )
                                    .and_then(|p_xf| {
                                        world_bounds_with_xf_inner(
                                            doc,
                                            oid,
                                            sub,
                                            &p_xf,
                                            Some(ctx),
                                            depth + 1,
                                            visiting,
                                            visiting_syms,
                                        )
                                    });
                                    visiting.remove(&oid);
                                    res
                                } else {
                                    doc.world_object_bounds(oid)
                                }
                            })
                            .map_or_else(
                                || world_xf.transform_point(fallback),
                                |bb| anchor.resolve(bb),
                            )
                    };
                let from_pt = resolve_sym_endpoint(c.from_obj, c.from_fallback, c.from_anchor);
                let to_pt = resolve_sym_endpoint(c.to_obj, c.to_fallback, c.to_anchor);
                c.route_path(from_pt, to_pt).bounding_box()
            } else {
                let (a, b) = doc.resolve_connector_endpoints_with_transform(c, world_xf);
                c.route_path(a, b).bounding_box()
            }
        }
        NodePayload::PowerClip { container, .. } => {
            apply_obj_perspective_to_local_path((**container).clone(), obj)
                .transformed(world_xf)
                .bounding_box()
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visiting_syms.insert(*sym_id) {
                return None;
            }
            let res = doc.symbol(*sym_id).and_then(|sym| {
                let mut local_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
                let mut referenced_children: HashSet<ObjectId> = HashSet::new();
                for (idx, sub) in sym.objects.iter().enumerate() {
                    if sub.id.0 != 0 {
                        local_map.insert(sub.id, sub);
                    }
                    let parent_ref = if sub.id.0 != 0 {
                        SymParentRef::Id(sub.id)
                    } else {
                        SymParentRef::RootIndex(idx)
                    };
                    match &sub.payload {
                        NodePayload::Group(c) => {
                            for &cid in c {
                                referenced_children.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(parent_ref);
                                }
                            }
                        }
                        NodePayload::PowerClip { contents, .. } => {
                            for &cid in contents {
                                referenced_children.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(parent_ref);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let mut sym_visiting = HashSet::new();
                let ctx = SymBoundsCtx {
                    root_xf: world_xf,
                    sym_objects: &sym.objects,
                    sym_map: &local_map,
                    sym_parent_map: &sym_parent_map,
                };
                let mut acc: Option<Rect2D> = None;
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced_children.contains(&sub.id) {
                        continue;
                    }
                    if sub.id.0 != 0 && !sym_visiting.insert(sub.id) {
                        continue;
                    }
                    let sub_xf = world_xf.compose(&sub.transform);
                    let r_opt = world_bounds_with_xf_inner(
                        doc,
                        sub.id,
                        sub,
                        &sub_xf,
                        Some(&ctx),
                        depth + 1,
                        &mut sym_visiting,
                        visiting_syms,
                    );
                    if sub.id.0 != 0 {
                        sym_visiting.remove(&sub.id);
                    }
                    if let Some(r) = r_opt {
                        acc = Some(match acc {
                            Some(prev) => prev.union(r),
                            None => r,
                        });
                    }
                }
                acc
            });
            visiting_syms.remove(sym_id);
            res
        }
        NodePayload::Group(children) => {
            let _ = id;
            let mut acc: Option<Rect2D> = None;
            for &cid in children {
                if !visiting.insert(cid) {
                    continue;
                }
                let child_opt = sym_ctx
                    .and_then(|ctx| ctx.sym_map.get(&cid).copied())
                    .or_else(|| doc.object(cid));
                if let Some(child) = child_opt {
                    let child_xf = world_xf.compose(&child.transform);
                    if let Some(r) = world_bounds_with_xf_inner(
                        doc,
                        cid,
                        child,
                        &child_xf,
                        sym_ctx,
                        depth + 1,
                        visiting,
                        visiting_syms,
                    ) {
                        acc = Some(match acc {
                            Some(prev) => prev.union(r),
                            None => r,
                        });
                    }
                }
                visiting.remove(&cid);
            }
            acc
        }
    };
    let eff_pad = if !obj.effects.is_empty() {
        let mut p = Effect::effects_bounds_padding_mm(&obj.effects);
        for eff in &obj.effects {
            match eff {
                Effect::Contour {
                    mode: coree_core::ContourMode::Outside,
                    steps,
                    offset_mm,
                    ..
                } => {
                    p = p.max(f64::from((*steps).clamp(1, 512)) * offset_mm.abs());
                }
                Effect::DropShadow {
                    dx_mm,
                    dy_mm,
                    blur_mm,
                    ..
                } => {
                    p = p.max(dx_mm.abs().max(dy_mm.abs()) + blur_mm.abs() * 2.0);
                }
                Effect::BlockShadow { dx_mm, dy_mm, .. } => {
                    p = p.max(dx_mm.abs().max(dy_mm.abs()));
                }
                Effect::Extrude25D {
                    depth, bevel_depth, ..
                }
                | Effect::Extrude3D {
                    depth, bevel_depth, ..
                } => {
                    p = p.max(depth.abs() * 2.0 + bevel_depth.abs() + 2.0);
                }
                _ => {}
            }
        }
        p
    } else {
        0.0
    };
    bounds
        .filter(|r| r.is_finite())
        .map(|r| if eff_pad > 0.0 { r.expand(eff_pad) } else { r })
}

/// Converts a `coree_core::BezierPath` in millimeters into a `tiny_skia::Path` scaled to pixels.
pub(crate) fn bezier_to_skia_path(
    path: &BezierPath,
    scale_x: f32,
    scale_y: f32,
) -> Option<tiny_skia::Path> {
    if path.is_empty() || !scale_x.is_finite() || !scale_y.is_finite() {
        return None;
    }
    let kb = path.to_kurbo();
    let mut pb = tiny_skia::PathBuilder::new();
    let sx = f64::from(scale_x);
    let sy = f64::from(scale_y);
    let to_f32 = |x: f64, y: f64| -> Option<(f32, f32)> {
        let fx = (x * sx) as f32;
        let fy = (y * sy) as f32;
        (fx.is_finite() && fy.is_finite()).then_some((fx, fy))
    };
    for el in kb.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                let (x, y) = to_f32(p.x, p.y)?;
                pb.move_to(x, y);
            }
            PathEl::LineTo(p) => {
                let (x, y) = to_f32(p.x, p.y)?;
                pb.line_to(x, y);
            }
            PathEl::QuadTo(p1, p2) => {
                let (x1, y1) = to_f32(p1.x, p1.y)?;
                let (x2, y2) = to_f32(p2.x, p2.y)?;
                pb.quad_to(x1, y1, x2, y2);
            }
            PathEl::CurveTo(p1, p2, p3) => {
                let (x1, y1) = to_f32(p1.x, p1.y)?;
                let (x2, y2) = to_f32(p2.x, p2.y)?;
                let (x3, y3) = to_f32(p3.x, p3.y)?;
                pb.cubic_to(x1, y1, x2, y2, x3, y3);
            }
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish()
}

pub(crate) fn apply_obj_perspective_to_local_path(
    local: BezierPath,
    obj: &SceneObject,
) -> BezierPath {
    let mut cur = if let Some(persp) = obj.perspective {
        persp.transform_bezier_path(&local)
    } else {
        local
    };
    if !obj.effects.is_empty() {
        let bb = cur.bounding_box();
        cur =
            coree_render::effects_render::apply_geometry_effects_in_bounds(&cur, bb, &obj.effects);
        let sym_xfs = Effect::symmetry_transforms_for(&obj.effects);
        if sym_xfs.len() > 1 {
            let base = cur.clone();
            for xf in sym_xfs.into_iter().skip(1) {
                cur.subpaths.extend(base.transformed(&xf).subpaths);
            }
        }
    }
    cur
}

#[inline]
pub(crate) fn warp_local_to_world(
    local: &BezierPath,
    world_xf: &Affine2D,
    obj_xf: &Affine2D,
    parent_persp: Option<Perspective3x3>,
) -> BezierPath {
    if let Some(pp) = parent_persp {
        pp.compose(&Perspective3x3::from_affine(*obj_xf))
            .transform_bezier_path(local)
    } else {
        local.transformed(world_xf)
    }
}

#[inline]
pub(crate) fn compose_prod_parent_persp(
    parent_persp: Option<Perspective3x3>,
    parent_xf: &Affine2D,
    obj_xf: &Affine2D,
    own_persp: Option<Perspective3x3>,
) -> Option<Perspective3x3> {
    match (parent_persp, own_persp) {
        (Some(pp), Some(op)) => Some(
            pp.compose(&Perspective3x3::from_affine(*obj_xf))
                .compose(&op),
        ),
        (Some(pp), None) => Some(pp.compose(&Perspective3x3::from_affine(*obj_xf))),
        (None, Some(op)) => {
            let world_xf = parent_xf.compose(obj_xf);
            Some(Perspective3x3::from_affine(world_xf).compose(&op))
        }
        (None, None) => None,
    }
}

pub(crate) fn container_own_persp(
    doc: &Document,
    obj: &SceneObject,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
) -> Option<Perspective3x3> {
    let mut own = obj.perspective;
    let has_eff_persp = obj
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Perspective { .. }));
    if has_eff_persp {
        let raw_bb = match &obj.payload {
            NodePayload::SymbolInstance(sym_id) => {
                let space_world = if let Some(ctx) = sym_ctx {
                    if obj.id.0 != 0 {
                        sym_world_transform_of(
                            obj.id,
                            ctx.root_xf,
                            ctx.sym_objects,
                            ctx.sym_map,
                            ctx.sym_parent_map,
                        )
                    } else {
                        Some(ctx.root_xf.compose(&obj.transform))
                    }
                } else if obj.id.0 != 0 {
                    doc.world_transform_of(obj.id)
                } else {
                    None
                };
                doc.symbol_bounds_with_xf_and_space(
                    *sym_id,
                    &Affine2D::IDENTITY,
                    space_world.as_ref(),
                )
            }
            NodePayload::PowerClip { container, .. } => container.bounding_box(),
            _ if sym_ctx.is_none() && obj.id.0 != 0 => doc.local_object_bounds(obj.id),
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                for &cid in children {
                    let child_opt = sym_ctx
                        .and_then(|ctx| ctx.sym_map.get(&cid).copied())
                        .or_else(|| (cid.0 != 0).then(|| doc.object(cid)).flatten());
                    if let Some(child) = child_opt {
                        let local_sym_ctx = sym_ctx.map(|ctx| SymBoundsCtx {
                            root_xf: &Affine2D::IDENTITY,
                            sym_objects: ctx.sym_objects,
                            sym_map: ctx.sym_map,
                            sym_parent_map: ctx.sym_parent_map,
                        });
                        if let Some(cb) = world_bounds_with_sym_xf(
                            doc,
                            cid,
                            child,
                            &child.transform,
                            local_sym_ctx.as_ref(),
                        ) {
                            acc = Some(acc.map_or(cb, |prev| prev.union(cb)));
                        }
                    }
                }
                acc
            }
            _ => None,
        };
        let mut cur_bb = match (own, raw_bb) {
            (Some(p), Some(bb)) => p.try_transform_rect(bb),
            (None, Some(bb)) => Some(bb),
            (_, None) => None,
        };
        for eff in &obj.effects {
            if let Effect::Perspective { quad } = eff {
                if let Some(bb) = cur_bb {
                    if let Some(h) = Perspective3x3::from_rect_to_quad(bb, quad) {
                        own = Some(match own {
                            Some(prev) => h.compose(&prev),
                            None => h,
                        });
                        cur_bb = h.try_transform_rect(bb);
                    }
                }
            }
        }
    }
    own
}

pub(crate) fn ancestor_world_persp(
    doc: &Document,
    top_container_id: ObjectId,
) -> Option<Perspective3x3> {
    let mut chain = Vec::new();
    let mut cur = Some(top_container_id);
    let mut seen = HashSet::new();
    while let Some(oid) = cur {
        if !seen.insert(oid) {
            break;
        }
        chain.push(oid);
        cur = doc.parent_container_of(oid);
    }
    let mut persp: Option<Perspective3x3> = None;
    let mut parent_xf = Affine2D::IDENTITY;
    for oid in chain.into_iter().rev() {
        let obj = doc.object(oid)?;
        let own = container_own_persp(doc, obj, None);
        persp = compose_prod_parent_persp(persp, &parent_xf, &obj.transform, own);
        parent_xf = parent_xf.compose(&obj.transform);
    }
    persp
}

/// Extracts world-space closed/open vector silhouette paths for `id` (expanding groups and
/// symbol definitions hierarchically, applying perspective homographies, and falling back to
/// bounding-box rects for text/raster/table objects).
pub(crate) fn collect_silhouette_paths(
    doc: &Document,
    id: ObjectId,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    out: &mut Vec<BezierPath>,
) {
    let mut visiting_syms = HashSet::new();
    collect_silhouette_paths_by_id(
        doc,
        id,
        parent_xf,
        parent_persp,
        None,
        depth,
        visiting,
        &mut visiting_syms,
        out,
    );
}

#[allow(clippy::too_many_arguments)]
fn collect_silhouette_paths_by_id(
    doc: &Document,
    id: ObjectId,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    out: &mut Vec<BezierPath>,
) {
    if depth > MAX_SCENE_DEPTH || !visiting.insert(id) {
        return;
    }
    let obj_opt = sym_ctx
        .and_then(|ctx| ctx.sym_map.get(&id).copied())
        .or_else(|| doc.object(id));
    if let Some(obj) = obj_opt {
        collect_silhouette_paths_obj(
            doc,
            id,
            obj,
            parent_xf,
            parent_persp,
            sym_ctx,
            depth,
            visiting,
            visiting_syms,
            out,
        );
    }
    visiting.remove(&id);
}

#[allow(clippy::too_many_arguments)]
fn collect_silhouette_paths_obj(
    doc: &Document,
    id: ObjectId,
    obj: &SceneObject,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<&SymBoundsCtx<'_>>,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    out: &mut Vec<BezierPath>,
) {
    if depth > MAX_SCENE_DEPTH {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return;
    }
    match &obj.payload {
        NodePayload::Path(p) => {
            if !p.is_empty() {
                let local = apply_obj_perspective_to_local_path(p.clone(), obj);
                let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                if !wp.is_empty() {
                    out.push(wp);
                }
            }
        }
        NodePayload::Shape(s) => {
            let local = apply_obj_perspective_to_local_path(s.to_bezier_path(), obj);
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
            }
        }
        NodePayload::BSpline(bs) => {
            let local = apply_obj_perspective_to_local_path(bs.to_bezier_path(), obj);
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
            }
        }
        NodePayload::ArtisticMedia { spine, style } => {
            let expanded = coree_core::ArtisticMedia::expand_stroke(spine, style);
            let local = apply_obj_perspective_to_local_path(expanded, obj);
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
            }
        }
        NodePayload::Clone { .. } => {
            if let Some((p, f, o, in_sym)) =
                doc.resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|c| c.sym_map))
            {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                collect_silhouette_paths_obj(
                    doc,
                    tmp.id,
                    &tmp,
                    parent_xf,
                    parent_persp,
                    if in_sym { sym_ctx } else { None },
                    depth + 1,
                    visiting,
                    visiting_syms,
                    out,
                );
            }
        }
        NodePayload::PowerClip { container, .. } => {
            let local = apply_obj_perspective_to_local_path((**container).clone(), obj);
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
            }
        }
        NodePayload::ArtisticText(t) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(t.estimated_bounds()),
                obj,
            );
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
            }
        }
        NodePayload::ParagraphText(t) => {
            let local =
                apply_obj_perspective_to_local_path(BezierPath::from_rect(t.bounding_box()), obj);
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
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
                let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                if !p.is_empty() {
                    out.push(p);
                }
            }
        }
        NodePayload::Table(t) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(t.to_bounding_box()),
                obj,
            );
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
            }
        }
        NodePayload::Group(children) => {
            let own_persp = container_own_persp(doc, obj, sym_ctx);
            let child_persp =
                compose_prod_parent_persp(parent_persp, &parent_xf, &obj.transform, own_persp);
            for &cid in children {
                collect_silhouette_paths_by_id(
                    doc,
                    cid,
                    world_xf,
                    child_persp,
                    sym_ctx,
                    depth + 1,
                    visiting,
                    visiting_syms,
                    out,
                );
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visiting_syms.insert(*sym_id) {
                return;
            }
            let own_persp = container_own_persp(doc, obj, sym_ctx);
            let child_persp =
                compose_prod_parent_persp(parent_persp, &parent_xf, &obj.transform, own_persp);
            if let Some(sym) = doc.symbol(*sym_id) {
                let mut local_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
                let mut referenced_children: HashSet<ObjectId> = HashSet::new();
                for (idx, sub) in sym.objects.iter().enumerate() {
                    if sub.id.0 != 0 {
                        local_map.insert(sub.id, sub);
                    }
                    let parent_ref = if sub.id.0 != 0 {
                        SymParentRef::Id(sub.id)
                    } else {
                        SymParentRef::RootIndex(idx)
                    };
                    match &sub.payload {
                        NodePayload::Group(c) => {
                            for &cid in c {
                                referenced_children.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(parent_ref);
                                }
                            }
                        }
                        NodePayload::PowerClip { contents, .. } => {
                            for &cid in contents {
                                referenced_children.insert(cid);
                                if cid.0 != 0 {
                                    sym_parent_map.entry(cid).or_insert(parent_ref);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let mut sym_visiting = HashSet::new();
                let ctx = SymBoundsCtx {
                    root_xf: &world_xf,
                    sym_objects: &sym.objects,
                    sym_map: &local_map,
                    sym_parent_map: &sym_parent_map,
                };
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced_children.contains(&sub.id) {
                        continue;
                    }
                    if sub.id.0 != 0 && !sym_visiting.insert(sub.id) {
                        continue;
                    }
                    collect_silhouette_paths_obj(
                        doc,
                        sub.id,
                        sub,
                        world_xf,
                        child_persp,
                        Some(&ctx),
                        depth + 1,
                        &mut sym_visiting,
                        visiting_syms,
                        out,
                    );
                    if sub.id.0 != 0 {
                        sym_visiting.remove(&sub.id);
                    }
                }
            }
            visiting_syms.remove(sym_id);
        }
        NodePayload::TextOnPath(top) => {
            let host_path = if let Some(ctx) = sym_ctx {
                ctx.sym_map
                    .get(&top.path_id)
                    .copied()
                    .and_then(|path_obj| {
                        let p_xf = sym_world_transform_of(
                            top.path_id,
                            ctx.root_xf,
                            ctx.sym_objects,
                            ctx.sym_map,
                            ctx.sym_parent_map,
                        )?;
                        let local_opt = match &path_obj.payload {
                            NodePayload::Path(p) => Some(p.clone()),
                            NodePayload::Shape(s) => Some(s.to_bezier_path()),
                            NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                            NodePayload::ArtisticMedia { spine, style } => {
                                Some(coree_core::ArtisticMedia::expand_stroke(spine, style))
                            }
                            NodePayload::PowerClip { container, .. } => Some((**container).clone()),
                            NodePayload::Clone { .. } => doc
                                .resolve_clone_payload_and_style_with_sym_scope(
                                    path_obj,
                                    Some(ctx.sym_map),
                                )
                                .and_then(|(cp, _, _, _)| match cp {
                                    NodePayload::Path(p) => Some(p),
                                    NodePayload::Shape(s) => Some(s.to_bezier_path()),
                                    NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                                    NodePayload::ArtisticMedia { spine, style } => Some(
                                        coree_core::ArtisticMedia::expand_stroke(&spine, &style),
                                    ),
                                    NodePayload::PowerClip { container, .. } => Some(*container),
                                    _ => None,
                                }),
                            _ => None,
                        };
                        if let Some(local) = local_opt {
                            Some(
                                apply_obj_perspective_to_local_path(local, path_obj)
                                    .transformed(&p_xf),
                            )
                        } else {
                            if !visiting.insert(top.path_id) {
                                return None;
                            }
                            let r = world_bounds_with_xf_inner(
                                doc,
                                top.path_id,
                                path_obj,
                                &p_xf,
                                Some(ctx),
                                depth + 1,
                                visiting,
                                visiting_syms,
                            );
                            visiting.remove(&top.path_id);
                            r.map(BezierPath::from_rect)
                        }
                    })
                    .or_else(|| doc.world_path_of(top.path_id))
                    .or_else(|| {
                        doc.world_object_bounds(top.path_id)
                            .map(BezierPath::from_rect)
                    })
            } else {
                doc.world_path_of(top.path_id).or_else(|| {
                    doc.world_object_bounds(top.path_id)
                        .map(BezierPath::from_rect)
                })
            };
            if let Some(hp) = host_path {
                let has_own_persp = obj.perspective.is_some()
                    || obj
                        .effects
                        .iter()
                        .any(|e| matches!(e, Effect::Perspective { .. }));
                let warped = if parent_persp.is_some() || has_own_persp {
                    let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_host = hp.transformed(&inv_parent);
                    let warped_local = apply_obj_perspective_to_local_path(local_host, obj);
                    if let Some(pp) = parent_persp {
                        pp.transform_bezier_path(&warped_local)
                    } else {
                        warped_local.transformed(&parent_xf)
                    }
                } else {
                    hp
                };
                if let Some(bb) = warped.bounding_box() {
                    out.push(BezierPath::from_rect(bb));
                }
            } else if sym_ctx.is_none() {
                if let Some(bb) = doc.world_object_bounds(id) {
                    let local_rect = parent_xf
                        .inverse()
                        .and_then(|inv| inv.try_transform_rect(bb))
                        .unwrap_or(bb);
                    let local =
                        apply_obj_perspective_to_local_path(BezierPath::from_rect(local_rect), obj);
                    let p = if let Some(pp) = parent_persp {
                        pp.transform_bezier_path(&local)
                    } else {
                        local.transformed(&parent_xf)
                    };
                    if !p.is_empty() {
                        out.push(p);
                    }
                } else {
                    let local = apply_obj_perspective_to_local_path(
                        BezierPath::from_rect(top.text.estimated_bounds()),
                        obj,
                    );
                    let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                    if !p.is_empty() {
                        out.push(p);
                    }
                }
            } else {
                let local = apply_obj_perspective_to_local_path(
                    BezierPath::from_rect(top.text.estimated_bounds()),
                    obj,
                );
                let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                if !p.is_empty() {
                    out.push(p);
                }
            }
        }
        NodePayload::Dimension(d) => {
            let local =
                apply_obj_perspective_to_local_path(BezierPath::from_rect(d.bounding_box()), obj);
            let p = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            if !p.is_empty() {
                out.push(p);
            }
        }
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints_with_transform(c, &world_xf);
            let route = c.route_path(a, b);
            if !route.is_empty() {
                out.push(route);
            }
        }
    }
}
