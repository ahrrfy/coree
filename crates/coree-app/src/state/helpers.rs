// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(crate) use coree_tools::{
    bspline_to_bezier_path, build_world_transform_command, compute_free_transform_affine,
    enclosing_layer_of_object, is_object_editable_in_doc, point_in_polygon, three_point_arc_path,
    unconjugate_local_affine_to_world, world_inverse_transform_of, world_transform_of_or_local,
};

/// Converts a live text object (`ArtisticText`, `ParagraphText`, or `TextOnPath`) into
/// vector glyph outlines in `obj`'s local coordinate space using a shared [`TextEngine`].
pub(crate) fn outline_text_object_with_engine(
    engine: &mut TextEngine,
    obj: &SceneObject,
    doc: &Document,
) -> Option<BezierPath> {
    match &obj.payload {
        NodePayload::ArtisticText(art) => Some(engine.artistic_text_to_curves(art)),
        NodePayload::ParagraphText(para) => Some(engine.paragraph_text_to_curves(para)),
        NodePayload::TextOnPath(top) => doc.world_path_of(top.path_id).map(|wp| {
            let world_curves = engine.text_on_path_to_curves(top, &wp);
            let inv = world_inverse_transform_of(doc, obj.id);
            world_curves.transformed(&inv)
        }),
        _ => None,
    }
}

/// Collects any [`SymbolId`] directly referenced by `obj`'s payload, fill, or table cells.
pub(crate) fn collect_symbol_refs_from_object(obj: &SceneObject, out: &mut HashSet<SymbolId>) {
    if let NodePayload::SymbolInstance(sym_id) = obj.payload {
        out.insert(sym_id);
    }
    if let Fill::VectorPattern { symbol_id, .. } = obj.fill {
        out.insert(symbol_id);
    }
    if let NodePayload::Table(ref table) = obj.payload {
        for cell in &table.cells {
            if let Fill::VectorPattern { symbol_id, .. } = cell.fill {
                out.insert(symbol_id);
            }
        }
    }
}

/// Recursively copies `sym_id`, any nested `SymbolDef`s, and their referenced `RasterAsset`s
/// from `src_doc` into `dst_doc` (with cycle guard).
pub(crate) fn collect_symbol_into_snapshot(
    src_doc: &Document,
    dst_doc: &mut Document,
    sym_id: SymbolId,
    visiting_syms: &mut HashSet<SymbolId>,
) {
    if dst_doc.symbol(sym_id).is_some() || !visiting_syms.insert(sym_id) {
        return;
    }
    if let Some(sym) = src_doc.symbol(sym_id).cloned() {
        let mut nested_syms = HashSet::new();
        for sub in &sym.objects {
            let mut asset_refs = HashSet::new();
            sub.collect_asset_refs(&mut asset_refs);
            for aid in asset_refs {
                if let Some(asset) = src_doc.asset(aid) {
                    dst_doc.insert_asset_if_absent(asset.clone());
                }
            }
            collect_symbol_refs_from_object(sub, &mut nested_syms);
        }
        dst_doc.insert_symbol(sym);
        for nested_sid in nested_syms {
            collect_symbol_into_snapshot(src_doc, dst_doc, nested_sid, visiting_syms);
        }
    }
    visiting_syms.remove(&sym_id);
}

/// Computes the world-space anchor transform for a `TextOnPath` object at its host curve point.
pub(crate) fn compute_text_on_path_world_xf(
    doc: &Document,
    id: ObjectId,
    top: &TextOnPath,
) -> Option<Affine2D> {
    doc.world_path_of(top.path_id)
        .and_then(|wp| {
            let dist = if top.offset_mm.is_finite() {
                top.offset_mm.max(0.0)
            } else {
                0.0
            };
            wp.point_and_tangent_at(dist).map(|(pt, tan)| {
                let lift = if top.distance_mm.is_finite() {
                    top.distance_mm
                } else {
                    0.0
                };
                let pos = pt + tan.perp() * lift;
                let angle = tan.y.atan2(tan.x);
                Affine2D::translate(pos.x, pos.y).compose(&Affine2D::rotate(angle))
            })
        })
        .or_else(|| doc.world_transform_of(top.path_id))
        .or_else(|| doc.world_transform_of(id))
}

/// Recursively copies `id`, all `Group` / `PowerClip` descendants, referenced `SymbolDef`s,
/// `RasterAsset`s, and resolved `Connector`/`TextOnPath` fallback geometry from `src_doc` into
/// `dst_doc` (with cycle/depth guard).
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_subtree_into_snapshot(
    src_doc: &Document,
    dst_doc: &mut Document,
    id: ObjectId,
    depth: usize,
    copied_parent_xf: Affine2D,
    visiting: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    text_on_path_fallbacks: &mut HashMap<ObjectId, Affine2D>,
) {
    if depth > 64 || !visiting.insert(id) {
        return;
    }
    if let Some(obj) = src_doc.object(id) {
        let mut obj_copy = obj.clone();
        let effective_parent_xf = if depth == 0 {
            obj_copy.transform = copied_parent_xf.compose(&obj_copy.transform);
            Affine2D::IDENTITY
        } else {
            copied_parent_xf
        };
        let copied_world_xf = effective_parent_xf.compose(&obj_copy.transform);
        if let NodePayload::Connector(ref mut conn) = obj_copy.payload {
            let world_xf = src_doc.world_transform_of(id).unwrap_or(obj.transform);
            let (w_from, w_to) =
                src_doc.resolve_connector_endpoints_with_transform(conn, &world_xf);
            let inv_copied = copied_world_xf.inverse().unwrap_or(Affine2D::IDENTITY);
            if conn.from_obj.is_some() {
                let pt = inv_copied.transform_point(w_from);
                if pt.is_finite() {
                    conn.from_fallback = pt;
                }
            }
            if conn.to_obj.is_some() {
                let pt = inv_copied.transform_point(w_to);
                if pt.is_finite() {
                    conn.to_fallback = pt;
                }
            }
        }
        if let NodePayload::TextOnPath(ref top) = obj.payload {
            if let Some(anchor_world) = compute_text_on_path_world_xf(src_doc, id, top) {
                let inv_parent = effective_parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                let local_xf = inv_parent.compose(&anchor_world);
                if local_xf.is_finite() {
                    text_on_path_fallbacks.insert(id, local_xf);
                }
            }
        }
        dst_doc.insert_snapshot_object_if_absent(obj_copy);
        let mut asset_refs = HashSet::new();
        obj.collect_asset_refs(&mut asset_refs);
        for aid in asset_refs {
            if let Some(asset) = src_doc.asset(aid) {
                dst_doc.insert_asset_if_absent(asset.clone());
            }
        }
        let mut sym_refs = HashSet::new();
        collect_symbol_refs_from_object(obj, &mut sym_refs);
        for sid in sym_refs {
            collect_symbol_into_snapshot(src_doc, dst_doc, sid, visiting_syms);
        }
        match &obj.payload {
            NodePayload::Group(children) => {
                for &cid in children {
                    collect_subtree_into_snapshot(
                        src_doc,
                        dst_doc,
                        cid,
                        depth + 1,
                        copied_world_xf,
                        visiting,
                        visiting_syms,
                        text_on_path_fallbacks,
                    );
                }
            }
            NodePayload::PowerClip { contents, .. } => {
                for &cid in contents {
                    collect_subtree_into_snapshot(
                        src_doc,
                        dst_doc,
                        cid,
                        depth + 1,
                        copied_world_xf,
                        visiting,
                        visiting_syms,
                        text_on_path_fallbacks,
                    );
                }
            }
            _ => {}
        }
    }
    visiting.remove(&id);
}

/// Recursively builds deterministic, undoable [`CommandOp`]s to clone an object subtree
/// (including `Group` and `PowerClip` hierarchies) with cycle/depth guards.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_duplicate_tree_ops(
    doc: &Document,
    src_id: ObjectId,
    layer_id: Option<LayerId>,
    delta_xf: Affine2D,
    depth: usize,
    next_id: &mut u64,
    visiting: &mut HashSet<ObjectId>,
    id_map: &mut HashMap<ObjectId, ObjectId>,
    ops: &mut Vec<CommandOp>,
) -> Option<ObjectId> {
    let mut deferred_locks = Vec::new();
    let res = build_duplicate_tree_ops_inner(
        doc,
        src_id,
        layer_id,
        delta_xf,
        depth,
        next_id,
        visiting,
        id_map,
        ops,
        &mut deferred_locks,
    );
    if !deferred_locks.is_empty() {
        ops.push(CommandOp::SetObjectLocked {
            ids: deferred_locks,
            locked: true,
        });
    }
    res
}

#[allow(clippy::too_many_arguments)]
fn build_duplicate_tree_ops_inner(
    doc: &Document,
    src_id: ObjectId,
    layer_id: Option<LayerId>,
    delta_xf: Affine2D,
    depth: usize,
    next_id: &mut u64,
    visiting: &mut HashSet<ObjectId>,
    id_map: &mut HashMap<ObjectId, ObjectId>,
    ops: &mut Vec<CommandOp>,
    deferred_locks: &mut Vec<ObjectId>,
) -> Option<ObjectId> {
    if depth > 64 || !visiting.insert(src_id) {
        return None;
    }
    let orig = match doc.object(src_id) {
        Some(o) => o,
        None => {
            visiting.remove(&src_id);
            return None;
        }
    };

    let result_id = match &orig.payload {
        NodePayload::Group(children) => {
            let mut new_children = Vec::with_capacity(children.len());
            for &cid in children {
                if let Some(nid) = build_duplicate_tree_ops_inner(
                    doc,
                    cid,
                    layer_id,
                    Affine2D::IDENTITY,
                    depth + 1,
                    next_id,
                    visiting,
                    id_map,
                    ops,
                    deferred_locks,
                ) {
                    new_children.push(nid);
                }
            }
            if new_children.is_empty() {
                None
            } else {
                let group_id = ObjectId(*next_id);
                *next_id = next_id.saturating_add(1);
                id_map.insert(src_id, group_id);
                ops.push(CommandOp::Group {
                    ids: new_children,
                    name: orig.name.clone(),
                });
                let grp_xf = delta_xf.compose(&orig.transform);
                if grp_xf != Affine2D::IDENTITY {
                    ops.push(CommandOp::TransformObjects {
                        ids: vec![group_id],
                        affine: grp_xf,
                    });
                }
                if orig.lens.is_some() {
                    ops.push(CommandOp::SetLens {
                        ids: vec![group_id],
                        lens: orig.lens.clone(),
                    });
                }
                for eff in &orig.effects {
                    ops.push(CommandOp::AddEffect {
                        id: group_id,
                        effect: eff.clone(),
                    });
                }
                if orig.style_id.is_some() {
                    ops.push(CommandOp::SetObjectStyleLink {
                        ids: vec![group_id],
                        style_id: orig.style_id,
                    });
                }
                if orig.fill != Fill::None || orig.style_id.is_some() {
                    ops.push(CommandOp::SetFill {
                        ids: vec![group_id],
                        fill: orig.fill.clone(),
                    });
                }
                if orig.outline != OutlineStyle::none() || orig.style_id.is_some() {
                    ops.push(CommandOp::SetOutline {
                        ids: vec![group_id],
                        outline: orig.outline.clone(),
                    });
                }
                if (orig.opacity - 1.0).abs() > 1e-6 || orig.style_id.is_some() {
                    ops.push(CommandOp::SetOpacity {
                        ids: vec![group_id],
                        opacity: orig.opacity,
                    });
                }
                if orig.blend_mode != BlendMode::Normal || orig.style_id.is_some() {
                    ops.push(CommandOp::SetBlendMode {
                        ids: vec![group_id],
                        blend_mode: orig.blend_mode,
                    });
                }
                if !orig.visible {
                    ops.push(CommandOp::SetObjectVisibility {
                        ids: vec![group_id],
                        visible: false,
                    });
                }
                if !orig.printable {
                    ops.push(CommandOp::SetObjectPrintable {
                        ids: vec![group_id],
                        printable: false,
                    });
                }
                if orig.locked {
                    deferred_locks.push(group_id);
                }
                Some(group_id)
            }
        }
        NodePayload::PowerClip {
            container,
            container_parametric,
            lock_contents,
            contents,
        } => {
            let container_id = ObjectId(*next_id);
            *next_id = next_id.saturating_add(1);
            id_map.insert(src_id, container_id);

            let mut container_obj = orig.clone();
            container_obj.id = container_id;
            container_obj.locked = false;
            container_obj.payload = match container_parametric {
                Some(shape) => NodePayload::Shape(shape.clone()),
                None => NodePayload::Path((**container).clone()),
            };
            container_obj.transform = delta_xf.compose(&orig.transform);
            let container_xf = container_obj.transform;
            ops.push(CommandOp::create_object(container_obj, layer_id));

            let mut new_contents = Vec::with_capacity(contents.len());
            for &cid in contents {
                // `create_powerclip` converts child transforms by `inv(container_xf)`, so
                // pre-compose `container_xf` onto each cloned child to preserve exact local coords.
                if let Some(nid) = build_duplicate_tree_ops_inner(
                    doc,
                    cid,
                    layer_id,
                    container_xf,
                    depth + 1,
                    next_id,
                    visiting,
                    id_map,
                    ops,
                    deferred_locks,
                ) {
                    new_contents.push(nid);
                }
            }
            ops.push(CommandOp::CreatePowerClip {
                container_id,
                content_ids: new_contents,
            });
            if !*lock_contents {
                ops.push(CommandOp::SetPowerClipLock {
                    id: container_id,
                    lock_contents: false,
                });
            }
            if orig.locked {
                deferred_locks.push(container_id);
            }
            Some(container_id)
        }
        _ => {
            let new_id = ObjectId(*next_id);
            *next_id = next_id.saturating_add(1);
            id_map.insert(src_id, new_id);
            let mut cloned = orig.clone();
            cloned.id = new_id;
            if orig.locked {
                cloned.locked = false;
                deferred_locks.push(new_id);
            }
            cloned.transform = delta_xf.compose(&cloned.transform);
            ops.push(CommandOp::create_object(cloned, layer_id));
            Some(new_id)
        }
    };

    visiting.remove(&src_id);
    result_id
}

/// Remaps cross-object references (`TextOnPath`, `Connector`, `ParagraphText`, `Effect::Blend`)
/// inside queued duplicate [`CommandOp`]s according to `id_map`.
pub(crate) fn remap_duplicate_ops(ops: &mut [CommandOp], id_map: &HashMap<ObjectId, ObjectId>) {
    if id_map.is_empty() {
        return;
    }
    for op in ops {
        match op {
            CommandOp::CreateObject { object, .. } | CommandOp::CreateDetachedObject { object } => {
                match &mut object.payload {
                    NodePayload::TextOnPath(top) => {
                        if let Some(&mapped) = id_map.get(&top.path_id) {
                            top.path_id = mapped;
                        }
                    }
                    NodePayload::Connector(conn) => {
                        if let Some(fid) = conn.from_obj {
                            if let Some(&mapped) = id_map.get(&fid) {
                                conn.from_obj = Some(mapped);
                            }
                        }
                        if let Some(tid) = conn.to_obj {
                            if let Some(&mapped) = id_map.get(&tid) {
                                conn.to_obj = Some(mapped);
                            }
                        }
                    }
                    NodePayload::ParagraphText(para) => {
                        if let Some(next_id) = para.linked_next_frame {
                            if let Some(&mapped) = id_map.get(&next_id) {
                                para.linked_next_frame = Some(mapped);
                            }
                        }
                    }
                    _ => {}
                }
                for eff in &mut object.effects {
                    if let Effect::Blend {
                        target_id,
                        along_path,
                        ..
                    } = eff
                    {
                        if let Some(&mapped) = id_map.get(target_id) {
                            *target_id = mapped;
                        }
                        if let Some(ap) = *along_path {
                            if let Some(&mapped) = id_map.get(&ap) {
                                *along_path = Some(mapped);
                            }
                        }
                    }
                }
            }
            CommandOp::AddEffect {
                effect:
                    Effect::Blend {
                        target_id,
                        along_path,
                        ..
                    },
                ..
            } => {
                if let Some(&mapped) = id_map.get(target_id) {
                    *target_id = mapped;
                }
                if let Some(ap) = *along_path {
                    if let Some(&mapped) = id_map.get(&ap) {
                        *along_path = Some(mapped);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Remaps copied cross-object [`ObjectId`] references using `id_map`, clears dangling
/// `ObjectStyleId`s missing in `target_doc`, and sanitizes unmapped cross-object references
/// when pasting into a different document (or when the target object no longer exists).
#[allow(clippy::too_many_arguments)]
pub(crate) fn remap_and_sanitize_paste_ops(
    ops: &mut Vec<CommandOp>,
    id_map: &HashMap<ObjectId, ObjectId>,
    snapshot_doc: &Document,
    text_on_path_fallbacks: &HashMap<ObjectId, Affine2D>,
    shift: Affine2D,
    target_doc: &Document,
    same_doc: bool,
) {
    let mut rev_id_map: HashMap<ObjectId, ObjectId> = HashMap::with_capacity(id_map.len());
    for (&src_id, &dst_id) in id_map {
        rev_id_map.insert(dst_id, src_id);
    }
    let mut container_xfs: HashMap<ObjectId, Affine2D> = HashMap::new();
    let keep_unmapped = |oid: ObjectId| -> bool { same_doc && target_doc.has_object(oid) };

    let orig_ops = std::mem::take(ops);
    ops.reserve(orig_ops.len());
    for mut op in orig_ops {
        match op {
            CommandOp::SetObjectStyleLink {
                style_id: Some(sid),
                ..
            } if target_doc.object_style(sid).is_none() => {
                continue;
            }
            CommandOp::CreateObject { ref mut object, .. }
            | CommandOp::CreateDetachedObject { ref mut object } => {
                if object
                    .style_id
                    .is_some_and(|sid| target_doc.object_style(sid).is_none())
                {
                    object.style_id = None;
                }
                match &mut object.payload {
                    NodePayload::TextOnPath(top) => {
                        if let Some(&mapped) = id_map.get(&top.path_id) {
                            top.path_id = mapped;
                        } else {
                            let art = top.text.clone();
                            object.payload = NodePayload::ArtisticText(art);
                            if let Some(&src_id) = rev_id_map.get(&object.id) {
                                if let Some(&fallback_xf) = text_on_path_fallbacks.get(&src_id) {
                                    let delta_xf = match snapshot_doc.parent_container_of(src_id) {
                                        None => shift,
                                        Some(pid) => {
                                            if snapshot_doc.object(pid).is_some_and(|p| {
                                                matches!(p.payload, NodePayload::PowerClip { .. })
                                            }) {
                                                id_map
                                                    .get(&pid)
                                                    .and_then(|nid| container_xfs.get(nid).copied())
                                                    .unwrap_or(Affine2D::IDENTITY)
                                            } else {
                                                Affine2D::IDENTITY
                                            }
                                        }
                                    };
                                    let next_xf = delta_xf.compose(&fallback_xf);
                                    if next_xf.is_finite() {
                                        object.transform = next_xf;
                                    }
                                }
                            }
                        }
                    }
                    NodePayload::Connector(conn) => {
                        if let Some(fid) = conn.from_obj {
                            if let Some(&mapped) = id_map.get(&fid) {
                                conn.from_obj = Some(mapped);
                            } else if !keep_unmapped(fid) {
                                conn.from_obj = None;
                            }
                        }
                        if let Some(tid) = conn.to_obj {
                            if let Some(&mapped) = id_map.get(&tid) {
                                conn.to_obj = Some(mapped);
                            } else if !keep_unmapped(tid) {
                                conn.to_obj = None;
                            }
                        }
                    }
                    NodePayload::ParagraphText(para) => {
                        if let Some(next_id) = para.linked_next_frame {
                            if let Some(&mapped) = id_map.get(&next_id) {
                                para.linked_next_frame = Some(mapped);
                            } else if !keep_unmapped(next_id) {
                                para.linked_next_frame = None;
                            }
                        }
                    }
                    _ => {}
                }
                let mut kept_effects = Vec::with_capacity(object.effects.len());
                for mut eff in std::mem::take(&mut object.effects) {
                    if let Effect::Blend {
                        ref mut target_id,
                        ref mut along_path,
                        ..
                    } = eff
                    {
                        if let Some(&mapped) = id_map.get(target_id) {
                            *target_id = mapped;
                        } else if !keep_unmapped(*target_id) {
                            continue;
                        }
                        if let Some(ap) = *along_path {
                            if let Some(&mapped) = id_map.get(&ap) {
                                *along_path = Some(mapped);
                            } else if !keep_unmapped(ap) {
                                *along_path = None;
                            }
                        }
                    }
                    kept_effects.push(eff);
                }
                object.effects = kept_effects;
                container_xfs.insert(object.id, object.transform);
                ops.push(op);
            }
            CommandOp::AddEffect {
                effect:
                    Effect::Blend {
                        ref mut target_id,
                        ref mut along_path,
                        ..
                    },
                ..
            } => {
                if let Some(&mapped) = id_map.get(target_id) {
                    *target_id = mapped;
                } else if !keep_unmapped(*target_id) {
                    continue;
                }
                if let Some(ap) = *along_path {
                    if let Some(&mapped) = id_map.get(&ap) {
                        *along_path = Some(mapped);
                    } else if !keep_unmapped(ap) {
                        *along_path = None;
                    }
                }
                ops.push(op);
            }
            other => ops.push(other),
        }
    }
}

/// Lightweight fuzzy subsequence scorer for Command Palette search.
pub(crate) fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if haystack.contains(needle) {
        let bonus = if haystack.starts_with(needle) {
            100
        } else {
            50
        };
        return Some(bonus - (haystack.len() as i32 - needle.len() as i32).min(40));
    }
    let mut score = 0i32;
    let mut h_chars = haystack.chars().peekable();
    let mut consecutive = 0i32;
    for nc in needle.chars() {
        let mut matched = false;
        for hc in h_chars.by_ref() {
            if hc == nc {
                matched = true;
                consecutive += 1;
                score += 5 + consecutive * 2;
                break;
            }
            consecutive = 0;
        }
        if !matched {
            return None;
        }
    }
    Some(score)
}

fn finite_egui_rect(r: egui::Rect) -> egui::Rect {
    if r.is_finite() {
        r
    } else {
        egui::Rect::ZERO
    }
}

/// Replaces `egui_dock`'s initial `Rect::NOTHING` (`±inf` sentinel coordinates, which
/// `serde_json` emits as `null` and fails to deserialize) with finite `Rect::ZERO` before
/// writing JSON to SQLite or `.coreews` bundles.
pub(crate) fn sanitize_dock_state_for_serde(state: &DockState<DockerTab>) -> DockState<DockerTab> {
    let mut cloned = state.clone();
    for surface in cloned.iter_surfaces_mut() {
        for node in surface.iter_nodes_mut() {
            match node {
                egui_dock::Node::Leaf(leaf) => {
                    leaf.rect = finite_egui_rect(leaf.rect);
                    leaf.viewport = finite_egui_rect(leaf.viewport);
                    if !leaf.scroll.is_finite() {
                        leaf.scroll = 0.0;
                    }
                }
                egui_dock::Node::Vertical(sp) | egui_dock::Node::Horizontal(sp) => {
                    sp.rect = finite_egui_rect(sp.rect);
                    if !sp.fraction.is_finite() {
                        sp.fraction = 0.5;
                    }
                }
                egui_dock::Node::Empty => {}
            }
        }
    }
    cloned
}
