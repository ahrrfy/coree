// SPDX-License-Identifier: MIT OR Apache-2.0

//! Complete 11-Lens non-destructive optical & color-filtering renderer.
//!
//! Supports all 11 optical & color lens types (`Brighten`, `ColorAdd`, `ColorLimit`,
//! `CustomColorMap`, `FishEye`, `HeatMap`, `Invert`, `Magnify`, `TintedGrayscale`,
//! `Transparency`, `Wireframe`) plus `frozen` snapshots, custom `viewpoint` offsets,
//! and `remove_face` blank-page background exclusion.

use crate::backend::{
    validate_surface_dimensions, BackendPaint, CanvasBackend, FillRule, PixelRect, RgbaBuffer,
    TinySkiaBackend,
};
use crate::effects_render::{apply_geometry_effects, symmetry_effect_transforms};
use crate::renderer::{
    apply_deform_chain_in_bounds, render_page_surface, DeformFrame, RenderOptions, SceneRenderer,
};
use crate::viewport::ViewMode;
use coree_core::math::sanitize_f32;
use coree_core::{
    Affine2D, BezierPath, Color, Document, Effect, Fill, LensConfig, LensKind, NodePayload,
    ObjectId, Page, Point2D, Rect2D, SceneObject, SymbolDef, SymbolId,
};
use std::collections::{HashMap, HashSet};

/// Maximum single-axis dimension for a cached frozen lens snapshot buffer.
const MAX_FROZEN_SNAPSHOT_DIM: f64 = 2048.0;

/// Maximum single-axis dimension for an offscreen lens sample backdrop.
const MAX_BACKDROP_DIM: i64 = 4096;

/// Frozen lens snapshot stored in [`SceneRenderer`] when `LensConfig::frozen == true`.
#[derive(Debug, Clone)]
pub struct FrozenLensSnapshot {
    pub buffer: RgbaBuffer,
}

/// Ordered backdrop entry preceding a [`LensConfig`] object in scene z-order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BackdropItem {
    /// A preceding sibling or container object rendered normally.
    Object(ObjectId),
    /// A preceding symbol-root object with `ObjectId(0)` at `index` inside the active `SymbolDef::objects`.
    SymObjectIndex(usize),
    /// An enclosing `PowerClip` whose underlay/fill is drawn and whose contour clips subsequent items.
    EnterPowerClip {
        pc_id: ObjectId,
        sym_root_idx: Option<usize>,
    },
    /// An enclosing deformed `Group` (`Envelope`/`Perspective`/`Distort`) whose geometry deforms subsequent items.
    EnterDeformedGroup {
        group_id: ObjectId,
        group_sym_root_idx: Option<usize>,
        group_local_bb: Option<Rect2D>,
        group_world_xf: Option<Affine2D>,
    },
    /// Exits the innermost `EnterDeformedGroup` frame.
    ExitDeformedGroup,
    /// Enters a `SymbolInstance` scope whose `sym_id` has page-mm world transform `sym_world_xf`.
    EnterSymbolInstance {
        sym_id: SymbolId,
        sym_world_xf: Affine2D,
        inst_id: ObjectId,
        inst_sym_root_idx: Option<usize>,
    },
    /// Exits the innermost `EnterSymbolInstance` scope.
    ExitSymbolInstance,
    /// Enters an `Effect::Symmetry` branch (`branch_idx > 0`) on container `obj_id`.
    EnterSymmetryBranch {
        obj_id: ObjectId,
        branch_idx: usize,
        branch_world_xf: Option<Affine2D>,
    },
    /// Exits the innermost `EnterSymmetryBranch` scope.
    ExitSymmetryBranch,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SymParentRef {
    Id(ObjectId),
    RootIndex(usize),
}

struct ActiveSymbolFrame<'a> {
    sym_def: &'a SymbolDef,
    sym_map: HashMap<ObjectId, &'a SceneObject>,
    sym_parent_map: HashMap<ObjectId, SymParentRef>,
    sym_world_xf: Affine2D,
}

impl<'a> ActiveSymbolFrame<'a> {
    fn get_ref(&self, r: SymParentRef) -> Option<&'a SceneObject> {
        match r {
            SymParentRef::Id(id) => self.sym_map.get(&id).copied(),
            SymParentRef::RootIndex(idx) => self.sym_def.objects.get(idx),
        }
    }
}

fn push_symbol_frame<'a>(
    doc: &'a Document,
    sym_stack: &mut Vec<Option<ActiveSymbolFrame<'a>>>,
    sym_id: SymbolId,
    sym_world_xf: Affine2D,
) {
    let Some(sym_def) = doc.symbol(sym_id) else {
        sym_stack.push(None);
        return;
    };
    let mut sym_map: HashMap<ObjectId, &'a SceneObject> = HashMap::new();
    let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
    for (idx, sub) in sym_def.objects.iter().enumerate() {
        if sub.id.0 != 0 {
            sym_map.insert(sub.id, sub);
        }
        let parent_ref = if sub.id.0 != 0 {
            SymParentRef::Id(sub.id)
        } else {
            SymParentRef::RootIndex(idx)
        };
        match &sub.payload {
            NodePayload::Group(c) => {
                for &cid in c {
                    if cid.0 != 0 {
                        sym_parent_map.entry(cid).or_insert(parent_ref);
                    }
                }
            }
            NodePayload::PowerClip { contents, .. } => {
                for &cid in contents {
                    if cid.0 != 0 {
                        sym_parent_map.entry(cid).or_insert(parent_ref);
                    }
                }
            }
            _ => {}
        }
    }
    sym_stack.push(Some(ActiveSymbolFrame {
        sym_def,
        sym_map,
        sym_parent_map,
        sym_world_xf,
    }));
}

fn resolve_sym_parent_world_ref(r: SymParentRef, sf: &ActiveSymbolFrame<'_>) -> Affine2D {
    let mut chain = Vec::new();
    let mut cur = r;
    let mut guard = HashSet::from([cur]);
    while let SymParentRef::Id(cur_id) = cur {
        let Some(&pid) = sf.sym_parent_map.get(&cur_id) else {
            break;
        };
        if !guard.insert(pid) || guard.len() > 64 {
            break;
        }
        if let Some(pobj) = sf.get_ref(pid) {
            chain.push(pobj.transform);
        }
        cur = pid;
    }
    let mut xf = sf.sym_world_xf;
    for local in chain.into_iter().rev() {
        xf = xf.compose(&local);
    }
    if xf.is_finite() {
        xf
    } else {
        sf.sym_world_xf
    }
}

fn resolve_sym_parent_world(oid: ObjectId, sf: &ActiveSymbolFrame<'_>) -> Affine2D {
    resolve_sym_parent_world_ref(SymParentRef::Id(oid), sf)
}

#[derive(Clone, Copy)]
struct ActiveDeformGroup<'a> {
    group_ref: SymParentRef,
    group_local_bb: Option<Rect2D>,
    group_effects: &'a [Effect],
    group_world_xf: Affine2D,
    group_to_sub: Affine2D,
}

#[allow(clippy::too_many_arguments)]
fn push_deformed_group_frame<'a>(
    doc: &'a Document,
    stack: &mut Vec<Option<ActiveDeformGroup<'a>>>,
    group_id: ObjectId,
    group_sym_root_idx: Option<usize>,
    group_local_bb: Option<Rect2D>,
    group_world_xf: Option<Affine2D>,
    local_page_xf: Affine2D,
    parent_override: Option<(ObjectId, Affine2D)>,
    active_sym: Option<&ActiveSymbolFrame<'a>>,
) {
    let group_ref = match (active_sym, group_sym_root_idx) {
        (Some(_), Some(idx)) if group_id.0 == 0 => SymParentRef::RootIndex(idx),
        _ => SymParentRef::Id(group_id),
    };
    let sym_obj = active_sym.and_then(|sf| sf.get_ref(group_ref));
    let Some(gobj) = sym_obj.or_else(|| doc.object(group_id)) else {
        stack.push(None);
        return;
    };
    let eff_world = group_world_xf.unwrap_or_else(|| {
        if let (Some(_), Some(sf)) = (sym_obj, active_sym) {
            resolve_sym_parent_world_ref(group_ref, sf).compose(&gobj.transform)
        } else {
            resolve_effective_parent_world(doc, group_id, parent_override).compose(&gobj.transform)
        }
    });
    let group_to_sub = local_page_xf.compose(&eff_world);
    stack.push(Some(ActiveDeformGroup {
        group_ref,
        group_local_bb,
        group_effects: &gobj.effects,
        group_world_xf: eff_world,
        group_to_sub,
    }));
}

fn resolve_relative_transform_to_group(
    doc: &Document,
    target_ref: SymParentRef,
    ancestor_ref: SymParentRef,
    override_opt: Option<(ObjectId, Affine2D)>,
    ancestor_world_xf: &Affine2D,
    active_sym: Option<&ActiveSymbolFrame<'_>>,
) -> Affine2D {
    if target_ref == ancestor_ref {
        return Affine2D::IDENTITY;
    }
    if let Some(sf) = active_sym {
        if let Some(target_obj) = sf.get_ref(target_ref) {
            if let SymParentRef::Id(mut cur_id) = target_ref {
                let mut chain: Vec<Affine2D> = Vec::new();
                let mut guard = HashSet::from([target_ref]);
                while let Some(obj) = sf.sym_map.get(&cur_id).copied() {
                    chain.push(obj.transform);
                    let Some(&pid) = sf.sym_parent_map.get(&cur_id) else {
                        break;
                    };
                    if pid == ancestor_ref {
                        let mut xf = Affine2D::IDENTITY;
                        for local in chain.into_iter().rev() {
                            xf = xf.compose(&local);
                        }
                        return if xf.is_finite() {
                            xf
                        } else {
                            Affine2D::IDENTITY
                        };
                    }
                    if !guard.insert(pid) || guard.len() > 64 {
                        break;
                    }
                    match pid {
                        SymParentRef::Id(next_id) => cur_id = next_id,
                        SymParentRef::RootIndex(_) => break,
                    }
                }
            }
            let target_world =
                resolve_sym_parent_world_ref(target_ref, sf).compose(&target_obj.transform);
            return ancestor_world_xf
                .inverse()
                .map(|inv| inv.compose(&target_world))
                .filter(|m| m.is_finite())
                .unwrap_or(Affine2D::IDENTITY);
        }
    }
    let SymParentRef::Id(target_id) = target_ref else {
        return Affine2D::IDENTITY;
    };
    if let SymParentRef::Id(ancestor_id) = ancestor_ref {
        let mut chain: Vec<Affine2D> = Vec::new();
        let mut cur = target_id;
        let mut guard = HashSet::from([cur]);
        while let Some(obj) = doc.object(cur) {
            chain.push(obj.transform);
            let Some(pid) = doc.parent_container_of(cur) else {
                break;
            };
            if pid == ancestor_id {
                let mut xf = Affine2D::IDENTITY;
                for local in chain.into_iter().rev() {
                    xf = xf.compose(&local);
                }
                return if xf.is_finite() {
                    xf
                } else {
                    Affine2D::IDENTITY
                };
            }
            if !guard.insert(pid) || guard.len() > 64 {
                break;
            }
            cur = pid;
        }
    }
    let target_world = doc
        .object(target_id)
        .map(|o| resolve_effective_parent_world(doc, target_id, override_opt).compose(&o.transform))
        .unwrap_or(Affine2D::IDENTITY);
    ancestor_world_xf
        .inverse()
        .map(|inv| inv.compose(&target_world))
        .filter(|m| m.is_finite())
        .unwrap_or(Affine2D::IDENTITY)
}

fn resolve_parent_rel_to_group(
    doc: &Document,
    oid: ObjectId,
    ancestor_ref: SymParentRef,
    override_opt: Option<(ObjectId, Affine2D)>,
    ancestor_world_xf: &Affine2D,
    active_sym: Option<&ActiveSymbolFrame<'_>>,
) -> Affine2D {
    if let Some(sf) = active_sym {
        if sf.sym_map.contains_key(&oid) {
            return match sf.sym_parent_map.get(&oid).copied() {
                Some(pid) if pid == ancestor_ref => Affine2D::IDENTITY,
                Some(pid) => resolve_relative_transform_to_group(
                    doc,
                    pid,
                    ancestor_ref,
                    override_opt,
                    ancestor_world_xf,
                    active_sym,
                ),
                None => ancestor_world_xf
                    .inverse()
                    .map(|inv| inv.compose(&sf.sym_world_xf))
                    .filter(|m| m.is_finite())
                    .unwrap_or(Affine2D::IDENTITY),
            };
        }
    }
    match doc.parent_container_of(oid) {
        Some(pid) if SymParentRef::Id(pid) == ancestor_ref => Affine2D::IDENTITY,
        Some(pid) => resolve_relative_transform_to_group(
            doc,
            SymParentRef::Id(pid),
            ancestor_ref,
            override_opt,
            ancestor_world_xf,
            active_sym,
        ),
        None => {
            let parent_world = resolve_effective_parent_world(doc, oid, override_opt);
            ancestor_world_xf
                .inverse()
                .map(|inv| inv.compose(&parent_world))
                .filter(|m| m.is_finite())
                .unwrap_or(Affine2D::IDENTITY)
        }
    }
}

fn sym_branch_rel_to_inner(sym_branch_xfs: &[Affine2D], inner_world_xf: &Affine2D) -> Affine2D {
    let branch_world = sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
    if branch_world == Affine2D::IDENTITY {
        return Affine2D::IDENTITY;
    }
    inner_world_xf
        .inverse()
        .map(|inv| inv.compose(&branch_world).compose(inner_world_xf))
        .filter(|m| m.is_finite())
        .unwrap_or(Affine2D::IDENTITY)
}

fn build_deform_frames<'a>(
    doc: &Document,
    stack: &[Option<ActiveDeformGroup<'a>>],
    override_opt: Option<(ObjectId, Affine2D)>,
    active_sym: Option<&ActiveSymbolFrame<'_>>,
) -> Option<(Vec<ActiveDeformGroup<'a>>, Vec<DeformFrame<'a>>)> {
    let active: Vec<ActiveDeformGroup<'a>> = stack.iter().flatten().copied().collect();
    if active.is_empty() {
        return None;
    }
    let mut frames = Vec::with_capacity(active.len());
    for (i, g) in active.iter().enumerate() {
        let rel_to_outer = if i == 0 {
            Affine2D::IDENTITY
        } else {
            active[i - 1]
                .group_world_xf
                .inverse()
                .map(|inv| inv.compose(&g.group_world_xf))
                .filter(|m| m.is_finite())
                .unwrap_or_else(|| {
                    resolve_relative_transform_to_group(
                        doc,
                        g.group_ref,
                        active[i - 1].group_ref,
                        override_opt,
                        &active[i - 1].group_world_xf,
                        active_sym,
                    )
                })
        };
        frames.push(DeformFrame {
            group_local_bb: g.group_local_bb,
            group_effects: g.group_effects,
            rel_to_outer,
        });
    }
    Some((active, frames))
}

pub(crate) fn resolve_effective_parent_world(
    doc: &Document,
    oid: ObjectId,
    override_opt: Option<(ObjectId, Affine2D)>,
) -> Affine2D {
    let Some((anchor_pid, anchor_world_xf)) = override_opt else {
        return doc
            .parent_container_of(oid)
            .and_then(|pid| doc.world_transform_of(pid))
            .filter(|m| m.is_finite())
            .unwrap_or(Affine2D::IDENTITY);
    };
    let mut chain: Vec<Affine2D> = Vec::new();
    let mut cur = oid;
    let mut guard = std::collections::HashSet::from([cur]);
    while let Some(pid) = doc.parent_container_of(cur) {
        if pid == anchor_pid {
            let mut xf = anchor_world_xf;
            for local in chain.into_iter().rev() {
                xf = xf.compose(&local);
            }
            return if xf.is_finite() { xf } else { anchor_world_xf };
        }
        if !guard.insert(pid) || guard.len() > 64 {
            break;
        }
        if let Some(pobj) = doc.object(pid) {
            chain.push(pobj.transform);
        }
        cur = pid;
    }
    doc.parent_container_of(oid)
        .and_then(|pid| doc.world_transform_of(pid))
        .filter(|m| m.is_finite())
        .unwrap_or(Affine2D::IDENTITY)
}

#[allow(clippy::too_many_arguments)]
fn resolve_powerclip_transforms(
    doc: &Document,
    pc_ref: SymParentRef,
    pc_id: ObjectId,
    pc_obj: &SceneObject,
    page_to_target: Affine2D,
    branch_xf: Affine2D,
    override_opt: Option<(ObjectId, Affine2D)>,
    active_sym: Option<&ActiveSymbolFrame<'_>>,
) -> (Affine2D, Affine2D, Affine2D) {
    if let Some(sf) = active_sym {
        if sf.get_ref(pc_ref).is_some() {
            let pc_parent_world = branch_xf.compose(&resolve_sym_parent_world_ref(pc_ref, sf));
            let parent_to_sub = page_to_target.compose(&pc_parent_world);
            let obj_to_sub = parent_to_sub.compose(&pc_obj.transform);
            return (pc_obj.transform, parent_to_sub, obj_to_sub);
        }
    }
    if let Some((anchor_pid, anchor_world_xf)) = override_opt {
        if pc_id == anchor_pid {
            let parent_to_sub = page_to_target.compose(&branch_xf).compose(&anchor_world_xf);
            return (Affine2D::IDENTITY, parent_to_sub, parent_to_sub);
        }
    }
    let pc_parent_world =
        branch_xf.compose(&resolve_effective_parent_world(doc, pc_id, override_opt));
    let parent_to_sub = page_to_target.compose(&pc_parent_world);
    let obj_to_sub = parent_to_sub.compose(&pc_obj.transform);
    (pc_obj.transform, parent_to_sub, obj_to_sub)
}

/// Sampleable backdrop buffer positioned at `(origin_x, origin_y)` in target pixel space.
struct SampleBackdrop {
    origin_x: f64,
    origin_y: f64,
    buffer: RgbaBuffer,
}

impl SampleBackdrop {
    fn sample_bilinear(&self, target_x: f64, target_y: f64) -> [u8; 4] {
        self.buffer
            .sample_bilinear(target_x - self.origin_x, target_y - self.origin_y)
    }

    fn get_pixel_at_target(&self, target_x: i64, target_y: i64) -> [u8; 4] {
        let lx = target_x - (self.origin_x as i64);
        let ly = target_y - (self.origin_y as i64);
        if lx >= 0
            && ly >= 0
            && lx < i64::from(self.buffer.width)
            && ly < i64::from(self.buffer.height)
        {
            self.buffer
                .get_pixel(lx as u32, ly as u32)
                .unwrap_or([0, 0, 0, 0])
        } else {
            [0, 0, 0, 0]
        }
    }
}

/// Renders an object bearing `lens: Some(lens_cfg)` by sampling the underlying
/// rendered backdrop (or re-rendering underlying objects for `Wireframe`), applying
/// the lens transformation, honoring `viewpoint`, `frozen`, and `remove_face`,
/// and compositing clipped to the lens shape.
///
/// `lens_to_surface` maps `lens_local_path` (in the lens object's own local coordinates)
/// into `backend` surface pixels (including any ancestor `Group` or `Symmetry` transforms).
/// `page_to_surface` maps page millimeter coordinates into `backend` surface pixels and is
/// used exclusively for `underlying_ids`, `remove_face`, and `lens.viewpoint`.
#[allow(clippy::too_many_arguments)]
pub fn render_lens_object(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    obj: &SceneObject,
    lens: &LensConfig,
    lens_local_path: &BezierPath,
    lens_to_surface: Affine2D,
    page_to_surface: Affine2D,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
    underlying_ids: &[ObjectId],
    depth: usize,
) {
    let items: Vec<BackdropItem> = underlying_ids
        .iter()
        .copied()
        .map(BackdropItem::Object)
        .collect();
    render_lens_object_with_backdrop(
        renderer,
        doc,
        page,
        obj,
        lens,
        lens_local_path,
        lens_to_surface,
        page_to_surface,
        view_mode,
        options,
        backend,
        &items,
        depth,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_lens_object_with_backdrop(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    obj: &SceneObject,
    lens: &LensConfig,
    lens_local_path: &BezierPath,
    lens_to_surface: Affine2D,
    page_to_surface: Affine2D,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
    underlying: &[BackdropItem],
    depth: usize,
) {
    if !lens_to_surface.is_finite() || !page_to_surface.is_finite() {
        return;
    }
    let Some(local_bounds) = lens_local_path.bounding_box() else {
        return;
    };
    if local_bounds.width() <= 1e-9 || local_bounds.height() <= 1e-9 {
        return;
    }
    let lens_screen_bounds = lens_to_surface.transform_rect(local_bounds);
    if !lens_screen_bounds.is_finite()
        || lens_screen_bounds.is_empty()
        || lens_screen_bounds.width() <= 1e-6
        || lens_screen_bounds.height() <= 1e-6
    {
        return;
    }

    // Overflow-safe destination rect clamped to current surface (full frame or 256×256 tile)
    let lens_rect =
        PixelRect::from_f64_rect_clamped(lens_screen_bounds, 0, backend.width(), backend.height());

    let frozen_key = renderer.current_frozen_lens_key(obj.id);
    if lens.frozen {
        if lens_rect.is_none() && renderer.get_frozen_lens_key(frozen_key).is_some() {
            return;
        }
        let full_x0 = lens_screen_bounds.min_x.floor();
        let full_y0 = lens_screen_bounds.min_y.floor();
        let full_x1 = lens_screen_bounds.max_x.ceil();
        let full_y1 = lens_screen_bounds.max_y.ceil();
        let full_w_f = (full_x1 - full_x0).max(1.0);
        let full_h_f = (full_y1 - full_y0).max(1.0);

        let frozen_buf = if let Some(cached) = renderer.get_frozen_lens_key(frozen_key) {
            cached.buffer.clone()
        } else {
            // Capture the COMPLETE unclipped lens image regardless of which tile renders first!
            let snap_scale = (MAX_FROZEN_SNAPSHOT_DIM / full_w_f.max(full_h_f)).min(1.0);
            let snap_w = (full_w_f * snap_scale)
                .ceil()
                .clamp(1.0, MAX_FROZEN_SNAPSHOT_DIM) as u32;
            let snap_h = (full_h_f * snap_scale)
                .ceil()
                .clamp(1.0, MAX_FROZEN_SNAPSHOT_DIM) as u32;

            let to_snap = Affine2D::scale(snap_scale, snap_scale)
                .compose(&Affine2D::translate(-full_x0, -full_y0));
            let snap_page_xf = to_snap.compose(&page_to_surface);
            let snap_lens_xf = to_snap.compose(&lens_to_surface);
            let snap_lens_bounds = snap_lens_xf.transform_rect(local_bounds);
            let snap_rect = PixelRect::new(0, 0, snap_w, snap_h);

            let backend_ref = if lens_rect.is_some()
                && !renderer.force_offscreen_lens_backdrop
                && (snap_scale - 1.0).abs() < 1e-9
            {
                Some((backend as &dyn CanvasBackend, full_x0, full_y0))
            } else {
                None
            };

            let fresh = compute_lens_buffer(
                renderer,
                doc,
                page,
                lens,
                snap_lens_bounds,
                snap_rect,
                snap_page_xf,
                view_mode,
                options,
                backend_ref,
                underlying,
                depth,
            );
            renderer.store_frozen_lens_key(
                frozen_key,
                FrozenLensSnapshot {
                    buffer: fresh.clone(),
                },
            );
            if lens_rect.is_none() {
                return;
            }
            fresh
        };

        if lens_rect.is_some() && !frozen_buf.is_empty() {
            let sx = full_w_f / f64::from(frozen_buf.width.max(1));
            let sy = full_h_f / f64::from(frozen_buf.height.max(1));
            let is_exact_1to1 = (sx - 1.0).abs() < 1e-6 && (sy - 1.0).abs() < 1e-6;
            let xf = Affine2D::translate(full_x0, full_y0).compose(&Affine2D::scale(sx, sy));
            backend.push_clip(lens_local_path, FillRule::Winding, lens_to_surface);
            backend.draw_pixmap(&frozen_buf, xf, obj.opacity, obj.blend_mode, !is_exact_1to1);
            backend.pop_clip();
        }
        return;
    }

    renderer.remove_frozen_lens_key(frozen_key);
    let Some(lens_rect) = lens_rect else {
        return;
    };
    let backend_ref = if renderer.force_offscreen_lens_backdrop {
        None
    } else {
        Some((backend as &dyn CanvasBackend, 0.0, 0.0))
    };
    let rendered_buf = compute_lens_buffer(
        renderer,
        doc,
        page,
        lens,
        lens_screen_bounds,
        lens_rect,
        page_to_surface,
        view_mode,
        options,
        backend_ref,
        underlying,
        depth,
    );

    // Composite lens pixels strictly clipped to the lens contour
    backend.push_clip(lens_local_path, FillRule::Winding, lens_to_surface);
    backend.composite_region(lens_rect, &rendered_buf, obj.opacity, obj.blend_mode);
    backend.pop_clip();
}

#[allow(clippy::too_many_arguments)]
fn compute_lens_buffer(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    lens: &LensConfig,
    lens_screen_bounds: Rect2D,
    target_rect: PixelRect,
    page_to_target: Affine2D,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend_ref: Option<(&dyn CanvasBackend, f64, f64)>,
    underlying: &[BackdropItem],
    depth: usize,
) -> RgbaBuffer {
    let w = target_rect.width;
    let h = target_rect.height;
    if w == 0 || h == 0 {
        return RgbaBuffer::new_transparent(0, 0);
    }

    // Use UNCLIPPED lens screen bounds for center & radial extent so lenses crossing
    // 256×256 tile edges have identical geometry and radii on every tile.
    let lens_center_px = lens_screen_bounds.center();
    let rx = (lens_screen_bounds.width() * 0.5).max(1.0);
    let ry = (lens_screen_bounds.height() * 0.5).max(1.0);

    // `viewpoint` is expressed in page millimeter coordinates, NEVER parent-group local space.
    let sample_center_px = lens
        .viewpoint
        .filter(|p| p.is_finite())
        .map_or(lens_center_px, |vp| page_to_target.transform_point(vp));
    let shift_x = sample_center_px.x - lens_center_px.x;
    let shift_y = sample_center_px.y - lens_center_px.y;

    // Optional `remove_face` coverage mask of underlying artwork (excluding blank page background!).
    // For geometric lenses (`FishEye` / `Magnify`), warp-sample the silhouette mask using the exact
    // same coordinate mapping as the backdrop so magnified/warped artwork is never erroneously clipped!
    let kind = lens.kind.clamped();
    let parent_override = renderer.parent_xf_override;
    let (mut out, face_mask): (RgbaBuffer, Option<Vec<u8>>) = match &kind {
        LensKind::Wireframe {
            override_outline,
            override_fill,
        } => {
            let buf = render_wireframe_lens(
                renderer,
                doc,
                page,
                underlying,
                target_rect,
                page_to_target,
                shift_x,
                shift_y,
                override_outline.as_ref(),
                override_fill.as_ref(),
                options,
                depth,
            );
            let mask = lens.remove_face.then(|| {
                build_underlying_coverage_mask(
                    doc,
                    underlying,
                    parent_override,
                    target_rect,
                    page_to_target,
                    shift_x,
                    shift_y,
                )
            });
            (buf, mask)
        }
        LensKind::FishEye { rate_pct } => {
            let strength = f64::from(sanitize_f32(*rate_pct, 100.0).clamp(-1000.0, 1000.0)) / 100.0;
            let inv_exp = if strength >= 0.0 {
                1.0 + strength
            } else {
                1.0 / (1.0 + strength.abs())
            };
            let (sx0, sy0, sx1, sy1) = fisheye_sample_bounds(
                target_rect,
                lens_center_px,
                sample_center_px,
                rx,
                ry,
                inv_exp,
            );
            let backdrop = obtain_sample_backdrop(
                renderer,
                doc,
                page,
                underlying,
                page_to_target,
                view_mode,
                options,
                backend_ref,
                sx0,
                sy0,
                sx1,
                sy1,
                depth,
            );
            let buf = render_fisheye_lens(
                &backdrop,
                target_rect,
                lens_center_px,
                sample_center_px,
                rx,
                ry,
                inv_exp,
            );
            let mask = lens.remove_face.then(|| {
                let cov_backdrop = build_underlying_coverage_backdrop(
                    doc,
                    underlying,
                    parent_override,
                    page_to_target,
                    sx0,
                    sy0,
                    sx1,
                    sy1,
                );
                let mut warped = Vec::with_capacity((w as usize) * (h as usize));
                for ly in 0..h {
                    let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
                    for lx in 0..w {
                        let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
                        let (sx, sy) = fisheye_map_point(
                            dst_x,
                            dst_y,
                            lens_center_px,
                            sample_center_px,
                            rx,
                            ry,
                            inv_exp,
                        );
                        warped.push(cov_backdrop.sample_bilinear(sx - 0.5, sy - 0.5)[3]);
                    }
                }
                warped
            });
            (buf, mask)
        }
        LensKind::Magnify { amount } => {
            let mag = f64::from(sanitize_f32(*amount, 2.0).clamp(1.0, 100.0));
            let inv_mag = 1.0 / mag;
            let min_sx = sample_center_px.x
                + (f64::from(target_rect.x) + 0.5 - lens_center_px.x) * inv_mag
                - 0.5;
            let max_sx = sample_center_px.x
                + (f64::from(target_rect.x) + f64::from(w) - 0.5 - lens_center_px.x) * inv_mag
                - 0.5;
            let min_sy = sample_center_px.y
                + (f64::from(target_rect.y) + 0.5 - lens_center_px.y) * inv_mag
                - 0.5;
            let max_sy = sample_center_px.y
                + (f64::from(target_rect.y) + f64::from(h) - 0.5 - lens_center_px.y) * inv_mag
                - 0.5;
            let sx0 = safe_floor_i64(min_sx.min(max_sx)) - 2;
            let sx1 = safe_ceil_i64(min_sx.max(max_sx)) + 3;
            let sy0 = safe_floor_i64(min_sy.min(max_sy)) - 2;
            let sy1 = safe_ceil_i64(min_sy.max(max_sy)) + 3;

            let backdrop = obtain_sample_backdrop(
                renderer,
                doc,
                page,
                underlying,
                page_to_target,
                view_mode,
                options,
                backend_ref,
                sx0,
                sy0,
                sx1,
                sy1,
                depth,
            );
            let buf = render_magnify_lens(
                &backdrop,
                target_rect,
                lens_center_px,
                sample_center_px,
                inv_mag,
            );
            let mask = lens.remove_face.then(|| {
                let cov_backdrop = build_underlying_coverage_backdrop(
                    doc,
                    underlying,
                    parent_override,
                    page_to_target,
                    sx0,
                    sy0,
                    sx1,
                    sy1,
                );
                let mut warped = Vec::with_capacity((w as usize) * (h as usize));
                for ly in 0..h {
                    let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
                    let sy = sample_center_px.y + (dst_y - lens_center_px.y) * inv_mag;
                    for lx in 0..w {
                        let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
                        let sx = sample_center_px.x + (dst_x - lens_center_px.x) * inv_mag;
                        warped.push(cov_backdrop.sample_bilinear(sx - 0.5, sy - 0.5)[3]);
                    }
                }
                warped
            });
            (buf, mask)
        }
        _ => {
            // Color-transform lenses: Brighten, ColorAdd, ColorLimit, CustomColorMap,
            // HeatMap, Invert, TintedGrayscale, Transparency
            let has_shift = shift_x.abs() > 0.5 || shift_y.abs() > 0.5;
            let (sx0, sy0, sx1, sy1) = if has_shift {
                (
                    safe_floor_i64(f64::from(target_rect.x) + shift_x) - 2,
                    safe_floor_i64(f64::from(target_rect.y) + shift_y) - 2,
                    safe_ceil_i64(f64::from(target_rect.x) + f64::from(w) + shift_x) + 3,
                    safe_ceil_i64(f64::from(target_rect.y) + f64::from(h) + shift_y) + 3,
                )
            } else {
                (
                    i64::from(target_rect.x),
                    i64::from(target_rect.y),
                    i64::from(target_rect.x) + i64::from(w),
                    i64::from(target_rect.y) + i64::from(h),
                )
            };

            let backdrop = obtain_sample_backdrop(
                renderer,
                doc,
                page,
                underlying,
                page_to_target,
                view_mode,
                options,
                backend_ref,
                sx0,
                sy0,
                sx1,
                sy1,
                depth,
            );

            let mut buf = RgbaBuffer::new_transparent(w, h);
            for ly in 0..h {
                for lx in 0..w {
                    let src_px = if has_shift {
                        let sx = f64::from(target_rect.x) + f64::from(lx) + shift_x;
                        let sy = f64::from(target_rect.y) + f64::from(ly) + shift_y;
                        backdrop.sample_bilinear(sx, sy)
                    } else {
                        backdrop.get_pixel_at_target(
                            i64::from(target_rect.x) + i64::from(lx),
                            i64::from(target_rect.y) + i64::from(ly),
                        )
                    };
                    let mapped = apply_color_lens_pixel(&kind, src_px);
                    buf.set_pixel(lx, ly, mapped);
                }
            }
            let mask = lens.remove_face.then(|| {
                build_underlying_coverage_mask(
                    doc,
                    underlying,
                    parent_override,
                    target_rect,
                    page_to_target,
                    shift_x,
                    shift_y,
                )
            });
            (buf, mask)
        }
    };

    // Apply `remove_face` mask: zero out alpha where no underlying object sits underneath
    if let Some(mask) = face_mask {
        for (i, px) in out.rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let cov = mask.get(i).copied().unwrap_or(0);
            if cov == 0 {
                px[3] = 0;
            } else if cov < 255 {
                px[3] = ((u16::from(px[3]) * u16::from(cov) + 127) / 255) as u8;
            }
        }
    }

    out
}

/// Obtains a backdrop buffer covering `[sx0..sx1, sy0..sy1]` in target pixel space.
///
/// Uses a zero-re-render `backend.read_region` fast path whenever the requested sample
/// window lies completely inside the current surface, and seamlessly renders an unclipped
/// offscreen backdrop from `page` + `underlying_ids` whenever the sample window crosses a
/// `256×256` tile boundary, extends offscreen via `viewpoint`, or captures a frozen snapshot.
#[allow(clippy::too_many_arguments)]
fn obtain_sample_backdrop(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    underlying: &[BackdropItem],
    page_to_target: Affine2D,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend_ref: Option<(&dyn CanvasBackend, f64, f64)>,
    sx0: i64,
    sy0: i64,
    sx1: i64,
    sy1: i64,
    depth: usize,
) -> SampleBackdrop {
    if sx1 <= sx0 || sy1 <= sy0 {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_transparent(0, 0),
        };
    }

    // Fast path: sample window fits strictly within the already-rendered backend surface
    if let Some((backend, bx_off, by_off)) = backend_ref {
        if bx_off.is_finite()
            && by_off.is_finite()
            && bx_off.fract() == 0.0
            && by_off.fract() == 0.0
        {
            let b_off_x = bx_off as i64;
            let b_off_y = by_off as i64;
            let bsx0 = sx0.saturating_add(b_off_x);
            let bsy0 = sy0.saturating_add(b_off_y);
            let bsx1 = sx1.saturating_add(b_off_x);
            let bsy1 = sy1.saturating_add(b_off_y);
            if bsx0 >= 0
                && bsy0 >= 0
                && bsx1 <= i64::from(backend.width())
                && bsy1 <= i64::from(backend.height())
            {
                let rect = PixelRect::new(
                    bsx0 as i32,
                    bsy0 as i32,
                    (bsx1 - bsx0) as u32,
                    (bsy1 - bsy0) as u32,
                );
                return SampleBackdrop {
                    origin_x: sx0 as f64,
                    origin_y: sy0 as f64,
                    buffer: backend.read_region(rect),
                };
            }
        }
    }

    // Offscreen unclipped backdrop path for cross-tile / offscreen-viewpoint / frozen capture
    let bw_i64 = sx1.saturating_sub(sx0);
    let bh_i64 = sy1.saturating_sub(sy0);
    if bw_i64 <= 0
        || bh_i64 <= 0
        || bw_i64 > MAX_BACKDROP_DIM
        || bh_i64 > MAX_BACKDROP_DIM
        || validate_surface_dimensions(bw_i64 as u32, bh_i64 as u32).is_err()
    {
        if let Some((backend, bx_off, by_off)) = backend_ref {
            return SampleBackdrop {
                origin_x: -bx_off,
                origin_y: -by_off,
                buffer: backend.snapshot_rgba(),
            };
        }
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, options.page_bg.to_rgba8()),
        };
    }

    let bw = bw_i64 as u32;
    let bh = bh_i64 as u32;
    let Ok(mut sub) = TinySkiaBackend::new(bw, bh) else {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, options.page_bg.to_rgba8()),
        };
    };

    let local_page_xf = Affine2D::translate(-(sx0 as f64), -(sy0 as f64)).compose(&page_to_target);
    let px_per_mm = estimate_scale_xf(&local_page_xf);
    let anti_alias = view_mode != ViewMode::Pixels;
    let parent_override = renderer.parent_xf_override;

    sub.clear(options.canvas_bg.clone());
    if let Some(p) = page {
        render_page_surface(p, local_page_xf, px_per_mm, options, &mut sub);
    } else if options.page_bg.alpha() > 1e-6 {
        sub.clear(options.page_bg.clone());
    }

    let prev_scope_stack = std::mem::take(&mut renderer.frozen_scope_stack);
    let prev_sym_root_idx = renderer.current_sym_root_idx.take();
    let mut sym_scope_lens: Vec<usize> = Vec::new();
    let mut sym_branch_lens: Vec<usize> = Vec::new();
    let mut sym_branch_xfs: Vec<Affine2D> = vec![Affine2D::IDENTITY];
    let mut open_clips = 0usize;
    let mut deform_stack: Vec<Option<ActiveDeformGroup<'_>>> = Vec::new();
    let mut sym_stack: Vec<Option<ActiveSymbolFrame<'_>>> = Vec::new();
    for (idx, &item) in underlying.iter().enumerate() {
        let active_sym = sym_stack.last().and_then(|o| o.as_ref());
        match item {
            BackdropItem::EnterSymbolInstance {
                sym_id,
                sym_world_xf,
                inst_id,
                inst_sym_root_idx,
            } => {
                sym_scope_lens.push(renderer.frozen_scope_stack.len());
                sym_branch_xfs.push(Affine2D::IDENTITY);
                let sym_u128 = sym_id.0.as_u128();
                renderer.frozen_scope_stack.extend_from_slice(&[
                    crate::renderer::FROZEN_TAG_SYM_INST,
                    sym_u128 as u64,
                    (sym_u128 >> 64) as u64,
                    inst_id.0,
                    inst_sym_root_idx.map_or(u64::MAX, |i| i as u64),
                ]);
                push_symbol_frame(doc, &mut sym_stack, sym_id, sym_world_xf);
            }
            BackdropItem::ExitSymbolInstance => {
                if let Some(prev_len) = sym_scope_lens.pop() {
                    renderer.frozen_scope_stack.truncate(prev_len);
                }
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
                sym_stack.pop();
            }
            BackdropItem::EnterSymmetryBranch {
                obj_id,
                branch_idx,
                branch_world_xf,
            } => {
                sym_branch_lens.push(renderer.frozen_scope_stack.len());
                renderer.frozen_scope_stack.extend_from_slice(&[
                    crate::renderer::FROZEN_TAG_SYM_BRANCH,
                    obj_id.0,
                    branch_idx as u64,
                ]);
                let cur = sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                let next = branch_world_xf
                    .filter(|m| m.is_finite())
                    .map_or(cur, |w| w.compose(&cur));
                sym_branch_xfs.push(if next.is_finite() { next } else { cur });
            }
            BackdropItem::ExitSymmetryBranch => {
                if let Some(prev_len) = sym_branch_lens.pop() {
                    renderer.frozen_scope_stack.truncate(prev_len);
                }
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
            }
            BackdropItem::EnterDeformedGroup {
                group_id,
                group_sym_root_idx,
                group_local_bb,
                group_world_xf,
            } => {
                sym_branch_xfs.push(Affine2D::IDENTITY);
                push_deformed_group_frame(
                    doc,
                    &mut deform_stack,
                    group_id,
                    group_sym_root_idx,
                    group_local_bb,
                    group_world_xf,
                    local_page_xf,
                    parent_override,
                    active_sym,
                );
            }
            BackdropItem::ExitDeformedGroup => {
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
                deform_stack.pop();
            }
            BackdropItem::Object(oid) => {
                let sym_ctx = active_sym.map(|sf| (&sf.sym_map, sf.sym_world_xf));
                if let Some((active, frames)) =
                    build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                {
                    let inner = active.last().expect("non-empty active deform frames");
                    let inner_group_id = match inner.group_ref {
                        SymParentRef::Id(id) => id,
                        SymParentRef::RootIndex(_) => ObjectId(0),
                    };
                    let branch_inner_rel =
                        sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                    let rel_xf = branch_inner_rel.compose(&resolve_parent_rel_to_group(
                        doc,
                        oid,
                        inner.group_ref,
                        parent_override,
                        &inner.group_world_xf,
                        active_sym,
                    ));
                    let mut visit = HashSet::new();
                    renderer.render_group_deformed_subtree(
                        doc,
                        page,
                        &[oid],
                        inner_group_id,
                        &frames,
                        local_page_xf,
                        active[0].group_to_sub,
                        rel_xf,
                        sym_ctx,
                        px_per_mm,
                        view_mode,
                        options,
                        &mut sub,
                        &underlying[..idx],
                        true,
                        depth + 1,
                        &mut visit,
                    );
                } else {
                    let branch_xf = sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                    let raw_parent_world = match active_sym {
                        Some(sf) if sf.sym_map.contains_key(&oid) => {
                            resolve_sym_parent_world(oid, sf)
                        }
                        _ => resolve_effective_parent_world(doc, oid, parent_override),
                    };
                    let parent_world = branch_xf.compose(&raw_parent_world);
                    let parent_to_sub = local_page_xf.compose(&parent_world);
                    renderer.render_object_by_id_with_sym(
                        doc,
                        page,
                        oid,
                        local_page_xf,
                        parent_to_sub,
                        sym_ctx,
                        Affine2D::IDENTITY,
                        view_mode,
                        options,
                        &mut sub,
                        &underlying[..idx],
                        depth + 1,
                    );
                }
            }
            BackdropItem::SymObjectIndex(sym_idx) => {
                if let Some(sf) = active_sym {
                    if let Some(sub_obj) = sf.sym_def.objects.get(sym_idx) {
                        let saved_root_idx = renderer.current_sym_root_idx;
                        renderer.current_sym_root_idx = if sub_obj.id.0 == 0 {
                            Some(sym_idx)
                        } else {
                            None
                        };
                        let sym_ctx = Some((&sf.sym_map, sf.sym_world_xf));
                        if let Some((active, frames)) =
                            build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                        {
                            let inner = active.last().expect("non-empty active deform frames");
                            let branch_inner_rel =
                                sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                            let raw_rel_xf = inner
                                .group_world_xf
                                .inverse()
                                .map(|inv| inv.compose(&sf.sym_world_xf))
                                .filter(|m| m.is_finite())
                                .unwrap_or(Affine2D::IDENTITY);
                            let rel_xf = branch_inner_rel.compose(&raw_rel_xf);
                            let mut visit = HashSet::new();
                            renderer.render_deformed_scene_object(
                                doc,
                                page,
                                sub_obj,
                                &frames,
                                local_page_xf,
                                active[0].group_to_sub,
                                rel_xf,
                                sym_ctx,
                                px_per_mm,
                                view_mode,
                                options,
                                &mut sub,
                                &underlying[..idx],
                                true,
                                depth + 1,
                                &mut visit,
                            );
                        } else {
                            let parent_to_sub = local_page_xf.compose(&sf.sym_world_xf);
                            renderer.render_scene_object_with_sym(
                                doc,
                                page,
                                sub_obj,
                                local_page_xf,
                                parent_to_sub,
                                sym_ctx,
                                Affine2D::IDENTITY,
                                view_mode,
                                options,
                                &mut sub,
                                &underlying[..idx],
                                depth + 1,
                            );
                        }
                        renderer.current_sym_root_idx = saved_root_idx;
                    }
                }
            }
            BackdropItem::EnterPowerClip {
                pc_id,
                sym_root_idx,
            } => {
                let pc_ref = match (active_sym, sym_root_idx) {
                    (Some(_), Some(r_idx)) if pc_id.0 == 0 => SymParentRef::RootIndex(r_idx),
                    _ => SymParentRef::Id(pc_id),
                };
                let pc_obj = active_sym
                    .and_then(|sf| sf.get_ref(pc_ref))
                    .or_else(|| doc.object(pc_id));
                let Some(pc_obj) = pc_obj else {
                    continue;
                };
                let pc_opacity = sanitize_f32(pc_obj.opacity, 1.0).clamp(0.0, 1.0);
                if !pc_obj.visible || pc_opacity <= 1e-6 {
                    continue;
                }
                if let NodePayload::PowerClip { container, .. } = &pc_obj.payload {
                    let (deformed_container, pc_local_xf, pc_parent_to_sub, pc_obj_to_sub) =
                        if let Some((active, frames)) =
                            build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                        {
                            let inner = active.last().expect("non-empty active deform frames");
                            let branch_inner_rel =
                                sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                            let pc_rel_xf =
                                branch_inner_rel.compose(&resolve_relative_transform_to_group(
                                    doc,
                                    pc_ref,
                                    inner.group_ref,
                                    parent_override,
                                    &inner.group_world_xf,
                                    active_sym,
                                ));
                            let def = apply_deform_chain_in_bounds(
                                &apply_geometry_effects(container, &pc_obj.effects),
                                pc_rel_xf,
                                &frames,
                            );
                            let outer_to_sub = active[0].group_to_sub;
                            (def, Affine2D::IDENTITY, outer_to_sub, outer_to_sub)
                        } else {
                            let branch_xf =
                                sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                            let (pc_local_xf, pc_parent_to_sub, pc_obj_to_sub) =
                                resolve_powerclip_transforms(
                                    doc,
                                    pc_ref,
                                    pc_id,
                                    pc_obj,
                                    local_page_xf,
                                    branch_xf,
                                    parent_override,
                                    active_sym,
                                );
                            let def = apply_geometry_effects(container, &pc_obj.effects);
                            (def, pc_local_xf, pc_parent_to_sub, pc_obj_to_sub)
                        };
                    if !pc_obj.effects.is_empty() {
                        crate::effects_render::render_underlay_effects(
                            doc,
                            pc_obj,
                            &deformed_container,
                            pc_local_xf,
                            pc_parent_to_sub,
                            local_page_xf,
                            px_per_mm,
                            anti_alias,
                            &mut sub,
                        );
                    }
                    if pc_obj.outline.behind_fill && pc_obj.outline.is_visible() {
                        crate::renderer::draw_outline_styled(
                            &deformed_container,
                            pc_local_xf,
                            pc_parent_to_sub,
                            &pc_obj.outline,
                            pc_obj.opacity,
                            pc_obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            &mut sub,
                        );
                    }
                    if pc_obj.fill.is_visible() {
                        renderer.draw_fill_styled(
                            doc,
                            &deformed_container,
                            &pc_obj.fill,
                            pc_obj_to_sub,
                            pc_obj.opacity,
                            pc_obj.blend_mode,
                            anti_alias,
                            &mut sub,
                            depth + 1,
                        );
                    }
                    sub.push_clip(&deformed_container, FillRule::Winding, pc_obj_to_sub);
                    open_clips += 1;
                }
            }
        }
    }
    for _ in 0..open_clips {
        sub.pop_clip();
    }
    renderer.frozen_scope_stack = prev_scope_stack;
    renderer.current_sym_root_idx = prev_sym_root_idx;

    SampleBackdrop {
        origin_x: sx0 as f64,
        origin_y: sy0 as f64,
        buffer: sub.into_rgba(),
    }
}

/// Evaluates all color-space lens variants accurately on a single RGBA8 pixel.
fn apply_color_lens_pixel(kind: &LensKind, px: [u8; 4]) -> [u8; 4] {
    match kind {
        LensKind::HeatMap {
            palette_rotation_deg,
        } => {
            // 5-stop thermal palette (deep-blue -> cyan -> green -> yellow -> red)
            // with smooth ramp rotation by `palette_rotation_deg`.
            let col = Color::rgba8(px[0], px[1], px[2], px[3]);
            let lum = col.luminance().clamp(0.0, 1.0);
            let rot_frac = sanitize_f32(*palette_rotation_deg, 0.0).rem_euclid(360.0) / 360.0;
            let t = (lum + rot_frac).rem_euclid(1.0);
            let thermal = sample_heatmap_5stop(t);
            [thermal[0], thermal[1], thermal[2], px[3]]
        }
        other => {
            let col = Color::rgba8(px[0], px[1], px[2], px[3]);
            other.apply_color(&col).to_rgba8()
        }
    }
}

/// 5-stop thermal infrared palette: Deep Blue -> Cyan -> Green -> Yellow -> Red.
fn sample_heatmap_5stop(t: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 5] = [
        [0.05, 0.05, 0.55], // 0.00: cold deep blue
        [0.00, 0.80, 0.90], // 0.25: cool cyan
        [0.10, 0.85, 0.20], // 0.50: warm green
        [1.00, 0.92, 0.10], // 0.75: hot yellow
        [0.95, 0.12, 0.08], // 1.00: blazing red
    ];
    let scaled = t.clamp(0.0, 1.0) * 4.0;
    let idx = (scaled.floor() as usize).min(3);
    let frac = scaled - idx as f32;
    let a = STOPS[idx];
    let b = STOPS[idx + 1];
    [
        ((a[0] + (b[0] - a[0]) * frac) * 255.0).round() as u8,
        ((a[1] + (b[1] - a[1]) * frac) * 255.0).round() as u8,
        ((a[2] + (b[2] - a[2]) * frac) * 255.0).round() as u8,
    ]
}

#[inline]
fn fisheye_map_point(
    dst_x: f64,
    dst_y: f64,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    rx: f64,
    ry: f64,
    inv_exp: f64,
) -> (f64, f64) {
    let nx = (dst_x - lens_center_px.x) / rx;
    let ny = (dst_y - lens_center_px.y) / ry;
    let r = nx.hypot(ny);
    if r > 1e-9 && r < 1.0 {
        let r_src = r.powf(inv_exp);
        let scale = r_src / r;
        (
            sample_center_px.x + nx * scale * rx,
            sample_center_px.y + ny * scale * ry,
        )
    } else {
        (
            sample_center_px.x + (dst_x - lens_center_px.x),
            sample_center_px.y + (dst_y - lens_center_px.y),
        )
    }
}

/// Computes the exact sample-space pixel bounding box required to evaluate FishEye
/// over `target_rect`, checking perimeter edges and axis cross-sections through `lens_center_px`.
fn fisheye_sample_bounds(
    target_rect: PixelRect,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    rx: f64,
    ry: f64,
    inv_exp: f64,
) -> (i64, i64, i64, i64) {
    let mut min_sx = f64::INFINITY;
    let mut max_sx = f64::NEG_INFINITY;
    let mut min_sy = f64::INFINITY;
    let mut max_sy = f64::NEG_INFINITY;

    let mut include = |dst_x: f64, dst_y: f64| {
        let (sx, sy) = fisheye_map_point(
            dst_x,
            dst_y,
            lens_center_px,
            sample_center_px,
            rx,
            ry,
            inv_exp,
        );
        if sx.is_finite() && sy.is_finite() {
            min_sx = min_sx.min(sx - 0.5);
            max_sx = max_sx.max(sx - 0.5);
            min_sy = min_sy.min(sy - 0.5);
            max_sy = max_sy.max(sy - 0.5);
        }
    };

    let x_start = f64::from(target_rect.x) + 0.5;
    let x_end = f64::from(target_rect.x) + f64::from(target_rect.width) - 0.5;
    let y_start = f64::from(target_rect.y) + 0.5;
    let y_end = f64::from(target_rect.y) + f64::from(target_rect.height) - 0.5;

    for lx in 0..target_rect.width {
        let dx = f64::from(target_rect.x) + f64::from(lx) + 0.5;
        include(dx, y_start);
        include(dx, y_end);
        if lens_center_px.y >= y_start && lens_center_px.y <= y_end {
            include(dx, lens_center_px.y);
        }
    }
    for ly in 0..target_rect.height {
        let dy = f64::from(target_rect.y) + f64::from(ly) + 0.5;
        include(x_start, dy);
        include(x_end, dy);
        if lens_center_px.x >= x_start && lens_center_px.x <= x_end {
            include(lens_center_px.x, dy);
        }
    }

    if !min_sx.is_finite() || !max_sx.is_finite() || !min_sy.is_finite() || !max_sy.is_finite() {
        return (0, 0, 0, 0);
    }

    (
        safe_floor_i64(min_sx) - 2,
        safe_floor_i64(min_sy) - 2,
        safe_ceil_i64(max_sx) + 3,
        safe_ceil_i64(max_sy) + 3,
    )
}

/// True radial barrel (`rate_pct > 0`) / pincushion (`rate_pct < 0`) geometric warp.
fn render_fisheye_lens(
    backdrop: &SampleBackdrop,
    target_rect: PixelRect,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    rx: f64,
    ry: f64,
    inv_exp: f64,
) -> RgbaBuffer {
    let w = target_rect.width;
    let h = target_rect.height;
    let mut out = RgbaBuffer::new_transparent(w, h);

    for ly in 0..h {
        let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
        for lx in 0..w {
            let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
            let (sx, sy) = fisheye_map_point(
                dst_x,
                dst_y,
                lens_center_px,
                sample_center_px,
                rx,
                ry,
                inv_exp,
            );
            let px = backdrop.sample_bilinear(sx - 0.5, sy - 0.5);
            out.set_pixel(lx, ly, px);
        }
    }
    out
}

/// True optical magnification lens (`amount` in `1.0..=100.0`) sampling a zoomed sub-region.
fn render_magnify_lens(
    backdrop: &SampleBackdrop,
    target_rect: PixelRect,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    inv_mag: f64,
) -> RgbaBuffer {
    let w = target_rect.width;
    let h = target_rect.height;
    let mut out = RgbaBuffer::new_transparent(w, h);

    for ly in 0..h {
        let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
        let sy = sample_center_px.y + (dst_y - lens_center_px.y) * inv_mag;
        for lx in 0..w {
            let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
            let sx = sample_center_px.x + (dst_x - lens_center_px.x) * inv_mag;
            let px = backdrop.sample_bilinear(sx - 0.5, sy - 0.5);
            out.set_pixel(lx, ly, px);
        }
    }
    out
}

/// Wireframe Lens: re-renders all underlying vector objects in Wireframe mode with optional
/// override outline & fill colors, respecting custom `viewpoint` offset.
#[allow(clippy::too_many_arguments)]
fn render_wireframe_lens(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    underlying: &[BackdropItem],
    target_rect: PixelRect,
    page_to_target: Affine2D,
    shift_x: f64,
    shift_y: f64,
    override_outline: Option<&Color>,
    override_fill: Option<&Color>,
    options: &RenderOptions,
    depth: usize,
) -> RgbaBuffer {
    let Ok(mut sub_backend) = TinySkiaBackend::new(target_rect.width, target_rect.height) else {
        return RgbaBuffer::new_transparent(target_rect.width, target_rect.height);
    };
    let bg = override_fill
        .cloned()
        .unwrap_or_else(|| options.page_bg.clone());
    sub_backend.clear(bg);

    let mut wire_opts = options.clone();
    if let Some(ov_out) = override_outline {
        wire_opts.wireframe_stroke_color = ov_out.clone();
    }

    let local_xf = Affine2D::translate(
        -f64::from(target_rect.x) - shift_x,
        -f64::from(target_rect.y) - shift_y,
    )
    .compose(&page_to_target);
    let px_per_mm = estimate_scale_xf(&local_xf);
    let parent_override = renderer.parent_xf_override;

    let mut open_clips = 0usize;
    let mut deform_stack: Vec<Option<ActiveDeformGroup<'_>>> = Vec::new();
    let mut sym_stack: Vec<Option<ActiveSymbolFrame<'_>>> = Vec::new();
    let mut sym_branch_xfs: Vec<Affine2D> = vec![Affine2D::IDENTITY];
    for &item in underlying {
        let active_sym = sym_stack.last().and_then(|o| o.as_ref());
        match item {
            BackdropItem::EnterSymbolInstance {
                sym_id,
                sym_world_xf,
                ..
            } => {
                sym_branch_xfs.push(Affine2D::IDENTITY);
                push_symbol_frame(doc, &mut sym_stack, sym_id, sym_world_xf);
            }
            BackdropItem::ExitSymbolInstance => {
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
                sym_stack.pop();
            }
            BackdropItem::EnterSymmetryBranch {
                branch_world_xf, ..
            } => {
                let cur = sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                let next = branch_world_xf
                    .filter(|m| m.is_finite())
                    .map_or(cur, |w| w.compose(&cur));
                sym_branch_xfs.push(if next.is_finite() { next } else { cur });
            }
            BackdropItem::ExitSymmetryBranch => {
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
            }
            BackdropItem::EnterDeformedGroup {
                group_id,
                group_sym_root_idx,
                group_local_bb,
                group_world_xf,
            } => {
                sym_branch_xfs.push(Affine2D::IDENTITY);
                push_deformed_group_frame(
                    doc,
                    &mut deform_stack,
                    group_id,
                    group_sym_root_idx,
                    group_local_bb,
                    group_world_xf,
                    local_xf,
                    parent_override,
                    active_sym,
                );
            }
            BackdropItem::ExitDeformedGroup => {
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
                deform_stack.pop();
            }
            BackdropItem::Object(oid) => {
                let sym_ctx = active_sym.map(|sf| (&sf.sym_map, sf.sym_world_xf));
                if let Some((active, frames)) =
                    build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                {
                    let inner = active.last().expect("non-empty active deform frames");
                    let inner_group_id = match inner.group_ref {
                        SymParentRef::Id(id) => id,
                        SymParentRef::RootIndex(_) => ObjectId(0),
                    };
                    let branch_inner_rel =
                        sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                    let rel_xf = branch_inner_rel.compose(&resolve_parent_rel_to_group(
                        doc,
                        oid,
                        inner.group_ref,
                        parent_override,
                        &inner.group_world_xf,
                        active_sym,
                    ));
                    let mut visit = HashSet::new();
                    renderer.render_group_deformed_subtree(
                        doc,
                        page,
                        &[oid],
                        inner_group_id,
                        &frames,
                        local_xf,
                        active[0].group_to_sub,
                        rel_xf,
                        sym_ctx,
                        px_per_mm,
                        ViewMode::Wireframe,
                        &wire_opts,
                        &mut sub_backend,
                        &[],
                        false,
                        depth + 1,
                        &mut visit,
                    );
                } else {
                    let branch_xf = sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                    let raw_parent_world = match active_sym {
                        Some(sf) if sf.sym_map.contains_key(&oid) => {
                            resolve_sym_parent_world(oid, sf)
                        }
                        _ => resolve_effective_parent_world(doc, oid, parent_override),
                    };
                    let parent_world = branch_xf.compose(&raw_parent_world);
                    let parent_to_sub = local_xf.compose(&parent_world);
                    renderer.render_object_by_id_with_sym(
                        doc,
                        page,
                        oid,
                        local_xf,
                        parent_to_sub,
                        sym_ctx,
                        Affine2D::IDENTITY,
                        ViewMode::Wireframe,
                        &wire_opts,
                        &mut sub_backend,
                        &[],
                        depth + 1,
                    );
                }
            }
            BackdropItem::SymObjectIndex(sym_idx) => {
                if let Some(sf) = active_sym {
                    if let Some(sub_obj) = sf.sym_def.objects.get(sym_idx) {
                        let sym_ctx = Some((&sf.sym_map, sf.sym_world_xf));
                        if let Some((active, frames)) =
                            build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                        {
                            let inner = active.last().expect("non-empty active deform frames");
                            let branch_inner_rel =
                                sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                            let raw_rel_xf = inner
                                .group_world_xf
                                .inverse()
                                .map(|inv| inv.compose(&sf.sym_world_xf))
                                .filter(|m| m.is_finite())
                                .unwrap_or(Affine2D::IDENTITY);
                            let rel_xf = branch_inner_rel.compose(&raw_rel_xf);
                            let mut visit = HashSet::new();
                            renderer.render_deformed_scene_object(
                                doc,
                                page,
                                sub_obj,
                                &frames,
                                local_xf,
                                active[0].group_to_sub,
                                rel_xf,
                                sym_ctx,
                                px_per_mm,
                                ViewMode::Wireframe,
                                &wire_opts,
                                &mut sub_backend,
                                &[],
                                false,
                                depth + 1,
                                &mut visit,
                            );
                        } else {
                            let parent_to_sub = local_xf.compose(&sf.sym_world_xf);
                            renderer.render_scene_object_with_sym(
                                doc,
                                page,
                                sub_obj,
                                local_xf,
                                parent_to_sub,
                                sym_ctx,
                                Affine2D::IDENTITY,
                                ViewMode::Wireframe,
                                &wire_opts,
                                &mut sub_backend,
                                &[],
                                depth + 1,
                            );
                        }
                    }
                }
            }
            BackdropItem::EnterPowerClip {
                pc_id,
                sym_root_idx,
            } => {
                let pc_ref = match (active_sym, sym_root_idx) {
                    (Some(_), Some(r_idx)) if pc_id.0 == 0 => SymParentRef::RootIndex(r_idx),
                    _ => SymParentRef::Id(pc_id),
                };
                let pc_obj = active_sym
                    .and_then(|sf| sf.get_ref(pc_ref))
                    .or_else(|| doc.object(pc_id));
                let Some(pc_obj) = pc_obj else {
                    continue;
                };
                let pc_opacity = sanitize_f32(pc_obj.opacity, 1.0).clamp(0.0, 1.0);
                if !pc_obj.visible || pc_opacity <= 1e-6 {
                    continue;
                }
                if let NodePayload::PowerClip { container, .. } = &pc_obj.payload {
                    let (deformed_container, pc_obj_to_sub) = if let Some((active, frames)) =
                        build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                    {
                        let inner = active.last().expect("non-empty active deform frames");
                        let branch_inner_rel =
                            sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                        let pc_rel_xf =
                            branch_inner_rel.compose(&resolve_relative_transform_to_group(
                                doc,
                                pc_ref,
                                inner.group_ref,
                                parent_override,
                                &inner.group_world_xf,
                                active_sym,
                            ));
                        let def = apply_deform_chain_in_bounds(
                            &apply_geometry_effects(container, &pc_obj.effects),
                            pc_rel_xf,
                            &frames,
                        );
                        (def, active[0].group_to_sub)
                    } else {
                        let branch_xf =
                            sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                        let (_, _, pc_obj_to_sub) = resolve_powerclip_transforms(
                            doc,
                            pc_ref,
                            pc_id,
                            pc_obj,
                            local_xf,
                            branch_xf,
                            parent_override,
                            active_sym,
                        );
                        (
                            apply_geometry_effects(container, &pc_obj.effects),
                            pc_obj_to_sub,
                        )
                    };
                    crate::renderer::stroke_wireframe(
                        &deformed_container,
                        pc_obj_to_sub,
                        px_per_mm,
                        &wire_opts,
                        &mut sub_backend,
                    );
                    sub_backend.push_clip(&deformed_container, FillRule::Winding, pc_obj_to_sub);
                    open_clips += 1;
                }
            }
        }
    }
    for _ in 0..open_clips {
        sub_backend.pop_clip();
    }

    sub_backend.into_rgba()
}

fn rasterize_backdrop_coverage(
    doc: &Document,
    underlying: &[BackdropItem],
    parent_override: Option<(ObjectId, Affine2D)>,
    local_xf: Affine2D,
    mask_backend: &mut dyn CanvasBackend,
) {
    let solid = BackendPaint::solid(Color::WHITE);
    let mut open_clips = 0usize;
    let mut deform_stack: Vec<Option<ActiveDeformGroup<'_>>> = Vec::new();
    let mut sym_stack: Vec<Option<ActiveSymbolFrame<'_>>> = Vec::new();
    let mut sym_branch_xfs: Vec<Affine2D> = vec![Affine2D::IDENTITY];
    for &item in underlying {
        let active_sym = sym_stack.last().and_then(|o| o.as_ref());
        match item {
            BackdropItem::EnterSymbolInstance {
                sym_id,
                sym_world_xf,
                ..
            } => {
                sym_branch_xfs.push(Affine2D::IDENTITY);
                push_symbol_frame(doc, &mut sym_stack, sym_id, sym_world_xf);
            }
            BackdropItem::ExitSymbolInstance => {
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
                sym_stack.pop();
            }
            BackdropItem::EnterSymmetryBranch {
                branch_world_xf, ..
            } => {
                let cur = sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                let next = branch_world_xf
                    .filter(|m| m.is_finite())
                    .map_or(cur, |w| w.compose(&cur));
                sym_branch_xfs.push(if next.is_finite() { next } else { cur });
            }
            BackdropItem::ExitSymmetryBranch => {
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
            }
            BackdropItem::EnterDeformedGroup {
                group_id,
                group_sym_root_idx,
                group_local_bb,
                group_world_xf,
            } => {
                sym_branch_xfs.push(Affine2D::IDENTITY);
                push_deformed_group_frame(
                    doc,
                    &mut deform_stack,
                    group_id,
                    group_sym_root_idx,
                    group_local_bb,
                    group_world_xf,
                    local_xf,
                    parent_override,
                    active_sym,
                );
            }
            BackdropItem::ExitDeformedGroup => {
                if sym_branch_xfs.len() > 1 {
                    sym_branch_xfs.pop();
                }
                deform_stack.pop();
            }
            BackdropItem::Object(oid) => {
                let sym_ctx = active_sym.map(|sf| (&sf.sym_map, sf.sym_world_xf));
                let target_obj = active_sym
                    .and_then(|sf| sf.sym_map.get(&oid).copied())
                    .or_else(|| doc.object(oid));
                let Some(target_obj) = target_obj else {
                    continue;
                };
                let target_opacity = sanitize_f32(target_obj.opacity, 1.0).clamp(0.0, 1.0);
                if !target_obj.visible || target_opacity <= 1e-6 {
                    continue;
                }
                if let Some((active, frames)) =
                    build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                {
                    let inner = active.last().expect("non-empty active deform frames");
                    let branch_inner_rel =
                        sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                    let rel_xf = branch_inner_rel.compose(&resolve_parent_rel_to_group(
                        doc,
                        oid,
                        inner.group_ref,
                        parent_override,
                        &inner.group_world_xf,
                        active_sym,
                    ));
                    let parent_world = inner.group_world_xf.compose(&rel_xf);
                    for sym_xf in symmetry_effect_transforms(&target_obj.effects) {
                        let c_xf = rel_xf.compose(&sym_xf).compose(&target_obj.transform);
                        let obj_world_xf =
                            parent_world.compose(&sym_xf).compose(&target_obj.transform);
                        rasterize_silhouette_payload(
                            doc,
                            target_obj,
                            c_xf,
                            obj_world_xf,
                            &frames,
                            active[0].group_to_sub,
                            sym_ctx,
                            &solid,
                            mask_backend,
                            0,
                        );
                    }
                } else {
                    let branch_xf = sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                    let raw_parent_world = match active_sym {
                        Some(sf) if sf.sym_map.contains_key(&oid) => {
                            resolve_sym_parent_world(oid, sf)
                        }
                        _ => resolve_effective_parent_world(doc, oid, parent_override),
                    };
                    let parent_world = branch_xf.compose(&raw_parent_world);
                    let parent_to_sub = local_xf.compose(&parent_world);
                    for sym_xf in symmetry_effect_transforms(&target_obj.effects) {
                        let sub_xf = parent_to_sub
                            .compose(&sym_xf)
                            .compose(&target_obj.transform);
                        let obj_world_xf =
                            parent_world.compose(&sym_xf).compose(&target_obj.transform);
                        rasterize_silhouette_payload(
                            doc,
                            target_obj,
                            sub_xf,
                            obj_world_xf,
                            &[],
                            Affine2D::IDENTITY,
                            sym_ctx,
                            &solid,
                            mask_backend,
                            0,
                        );
                    }
                }
            }
            BackdropItem::SymObjectIndex(sym_idx) => {
                if let Some(sf) = active_sym {
                    if let Some(target_obj) = sf.sym_def.objects.get(sym_idx) {
                        let target_opacity = sanitize_f32(target_obj.opacity, 1.0).clamp(0.0, 1.0);
                        if !target_obj.visible || target_opacity <= 1e-6 {
                            continue;
                        }
                        let sym_ctx = Some((&sf.sym_map, sf.sym_world_xf));
                        if let Some((active, frames)) =
                            build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                        {
                            let inner = active.last().expect("non-empty active deform frames");
                            let branch_inner_rel =
                                sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                            let raw_rel_xf = inner
                                .group_world_xf
                                .inverse()
                                .map(|inv| inv.compose(&sf.sym_world_xf))
                                .filter(|m| m.is_finite())
                                .unwrap_or(Affine2D::IDENTITY);
                            let rel_xf = branch_inner_rel.compose(&raw_rel_xf);
                            let parent_world = inner.group_world_xf.compose(&rel_xf);
                            for sym_xf in symmetry_effect_transforms(&target_obj.effects) {
                                let c_xf = rel_xf.compose(&sym_xf).compose(&target_obj.transform);
                                let obj_world_xf =
                                    parent_world.compose(&sym_xf).compose(&target_obj.transform);
                                rasterize_silhouette_payload(
                                    doc,
                                    target_obj,
                                    c_xf,
                                    obj_world_xf,
                                    &frames,
                                    active[0].group_to_sub,
                                    sym_ctx,
                                    &solid,
                                    mask_backend,
                                    0,
                                );
                            }
                        } else {
                            let parent_to_sub = local_xf.compose(&sf.sym_world_xf);
                            for sym_xf in symmetry_effect_transforms(&target_obj.effects) {
                                let sub_xf = parent_to_sub
                                    .compose(&sym_xf)
                                    .compose(&target_obj.transform);
                                let obj_world_xf = sf
                                    .sym_world_xf
                                    .compose(&sym_xf)
                                    .compose(&target_obj.transform);
                                rasterize_silhouette_payload(
                                    doc,
                                    target_obj,
                                    sub_xf,
                                    obj_world_xf,
                                    &[],
                                    Affine2D::IDENTITY,
                                    sym_ctx,
                                    &solid,
                                    mask_backend,
                                    0,
                                );
                            }
                        }
                    }
                }
            }
            BackdropItem::EnterPowerClip {
                pc_id,
                sym_root_idx,
            } => {
                let pc_ref = match (active_sym, sym_root_idx) {
                    (Some(_), Some(r_idx)) if pc_id.0 == 0 => SymParentRef::RootIndex(r_idx),
                    _ => SymParentRef::Id(pc_id),
                };
                let pc_obj = active_sym
                    .and_then(|sf| sf.get_ref(pc_ref))
                    .or_else(|| doc.object(pc_id));
                let Some(pc_obj) = pc_obj else {
                    continue;
                };
                let pc_opacity = sanitize_f32(pc_obj.opacity, 1.0).clamp(0.0, 1.0);
                if !pc_obj.visible || pc_opacity <= 1e-6 {
                    continue;
                }
                if let NodePayload::PowerClip { container, .. } = &pc_obj.payload {
                    let (deformed_container, pc_obj_to_sub) = if let Some((active, frames)) =
                        build_deform_frames(doc, &deform_stack, parent_override, active_sym)
                    {
                        let inner = active.last().expect("non-empty active deform frames");
                        let branch_inner_rel =
                            sym_branch_rel_to_inner(&sym_branch_xfs, &inner.group_world_xf);
                        let pc_rel_xf =
                            branch_inner_rel.compose(&resolve_relative_transform_to_group(
                                doc,
                                pc_ref,
                                inner.group_ref,
                                parent_override,
                                &inner.group_world_xf,
                                active_sym,
                            ));
                        let def = apply_deform_chain_in_bounds(
                            &apply_geometry_effects(container, &pc_obj.effects),
                            pc_rel_xf,
                            &frames,
                        );
                        (def, active[0].group_to_sub)
                    } else {
                        let branch_xf =
                            sym_branch_xfs.last().copied().unwrap_or(Affine2D::IDENTITY);
                        let (_, _, pc_obj_to_sub) = resolve_powerclip_transforms(
                            doc,
                            pc_ref,
                            pc_id,
                            pc_obj,
                            local_xf,
                            branch_xf,
                            parent_override,
                            active_sym,
                        );
                        (
                            apply_geometry_effects(container, &pc_obj.effects),
                            pc_obj_to_sub,
                        )
                    };
                    if fill_is_visible(&pc_obj.fill) || pc_obj.outline.is_visible() {
                        mask_backend.fill_path(
                            &deformed_container,
                            &solid,
                            FillRule::Winding,
                            pc_obj_to_sub,
                        );
                    }
                    mask_backend.push_clip(&deformed_container, FillRule::Winding, pc_obj_to_sub);
                    open_clips += 1;
                }
            }
        }
    }
    for _ in 0..open_clips {
        mask_backend.pop_clip();
    }
}

/// Rasterizes the silhouette coverage of all underlying objects (strictly below the lens)
/// into a 1-channel coverage mask so `LensConfig::remove_face` hides empty page background.
fn build_underlying_coverage_mask(
    doc: &Document,
    underlying: &[BackdropItem],
    parent_override: Option<(ObjectId, Affine2D)>,
    target_rect: PixelRect,
    page_to_target: Affine2D,
    shift_x: f64,
    shift_y: f64,
) -> Vec<u8> {
    let w = target_rect.width;
    let h = target_rect.height;
    let Ok(mut mask_backend) = TinySkiaBackend::new(w, h) else {
        return vec![255u8; (w as usize) * (h as usize)];
    };
    mask_backend.clear(Color::TRANSPARENT);
    let local_xf = Affine2D::translate(
        -f64::from(target_rect.x) - shift_x,
        -f64::from(target_rect.y) - shift_y,
    )
    .compose(&page_to_target);
    rasterize_backdrop_coverage(
        doc,
        underlying,
        parent_override,
        local_xf,
        &mut mask_backend,
    );

    let buf = mask_backend.into_rgba();
    buf.rgba.as_chunks::<4>().0.iter().map(|px| px[3]).collect()
}

/// Builds a sampleable silhouette coverage backdrop over `[sx0..sx1, sy0..sy1]` for warped `remove_face`.
#[allow(clippy::too_many_arguments)]
fn build_underlying_coverage_backdrop(
    doc: &Document,
    underlying: &[BackdropItem],
    parent_override: Option<(ObjectId, Affine2D)>,
    page_to_target: Affine2D,
    sx0: i64,
    sy0: i64,
    sx1: i64,
    sy1: i64,
) -> SampleBackdrop {
    let bw_i64 = sx1.saturating_sub(sx0);
    let bh_i64 = sy1.saturating_sub(sy0);
    if bw_i64 <= 0
        || bh_i64 <= 0
        || bw_i64 > MAX_BACKDROP_DIM
        || bh_i64 > MAX_BACKDROP_DIM
        || validate_surface_dimensions(bw_i64 as u32, bh_i64 as u32).is_err()
    {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, [255, 255, 255, 255]),
        };
    }
    let Ok(mut mask_backend) = TinySkiaBackend::new(bw_i64 as u32, bh_i64 as u32) else {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, [255, 255, 255, 255]),
        };
    };
    mask_backend.clear(Color::TRANSPARENT);
    let local_xf = Affine2D::translate(-(sx0 as f64), -(sy0 as f64)).compose(&page_to_target);
    rasterize_backdrop_coverage(
        doc,
        underlying,
        parent_override,
        local_xf,
        &mut mask_backend,
    );
    SampleBackdrop {
        origin_x: sx0 as f64,
        origin_y: sy0 as f64,
        buffer: mask_backend.into_rgba(),
    }
}

#[inline]
fn fill_is_visible(fill: &Fill) -> bool {
    match fill {
        Fill::None => false,
        Fill::Solid(c) => c.alpha() > 1e-6,
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => stops.iter().any(|s| s.color.alpha() > 1e-6),
        Fill::TwoColorPattern { fg, bg, .. } | Fill::TextureFill { fg, bg, .. } => {
            fg.alpha() > 1e-6 || bg.alpha() > 1e-6
        }
        Fill::MeshFill { nodes, .. } => nodes.iter().any(|n| n.color.alpha() > 1e-6),
        Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_silhouette_payload(
    doc: &Document,
    obj: &SceneObject,
    world_xf: Affine2D,
    obj_world_xf: Affine2D,
    deform_frames: &[DeformFrame<'_>],
    outer_to_sub: Affine2D,
    sym_ctx: Option<(&HashMap<ObjectId, &SceneObject>, Affine2D)>,
    paint: &BackendPaint,
    backend: &mut dyn CanvasBackend,
    depth: usize,
) {
    let obj_opacity = sanitize_f32(obj.opacity, 1.0).clamp(0.0, 1.0);
    if depth > 64 || !obj.visible || obj_opacity <= 1e-6 || !world_xf.is_finite() {
        return;
    }
    let mut fill_geom = |base_path: &BezierPath| {
        let local_geom = apply_geometry_effects(base_path, &obj.effects);
        if deform_frames.is_empty() {
            backend.fill_path(&local_geom, paint, FillRule::Winding, world_xf);
        } else {
            let deformed = apply_deform_chain_in_bounds(&local_geom, world_xf, deform_frames);
            backend.fill_path(&deformed, paint, FillRule::Winding, outer_to_sub);
        }
    };
    let own_fill_or_outline =
        fill_is_visible(&obj.fill) || obj.outline.is_visible() || obj.lens.is_some();
    match &obj.payload {
        NodePayload::Path(p) => {
            if own_fill_or_outline {
                fill_geom(p);
            }
        }
        NodePayload::Shape(s) => {
            if own_fill_or_outline {
                fill_geom(&s.to_bezier_path());
            }
        }
        NodePayload::PowerClip {
            container,
            contents,
            ..
        } => {
            if own_fill_or_outline {
                fill_geom(container);
            } else if !contents.is_empty() {
                let local_geom = apply_geometry_effects(container, &obj.effects);
                if deform_frames.is_empty() {
                    backend.push_clip(&local_geom, FillRule::Winding, world_xf);
                } else {
                    let deformed =
                        apply_deform_chain_in_bounds(&local_geom, world_xf, deform_frames);
                    backend.push_clip(&deformed, FillRule::Winding, outer_to_sub);
                }
                for &cid in contents {
                    if let Some((map, _)) = sym_ctx {
                        if let Some(sub) = map.get(&cid).copied() {
                            let sub_opacity = sanitize_f32(sub.opacity, 1.0).clamp(0.0, 1.0);
                            if sub.visible && sub_opacity > 1e-6 {
                                for sym_xf in symmetry_effect_transforms(&sub.effects) {
                                    let c_xf = world_xf.compose(&sym_xf).compose(&sub.transform);
                                    let child_world =
                                        obj_world_xf.compose(&sym_xf).compose(&sub.transform);
                                    rasterize_silhouette_payload(
                                        doc,
                                        sub,
                                        c_xf,
                                        child_world,
                                        deform_frames,
                                        outer_to_sub,
                                        sym_ctx,
                                        paint,
                                        backend,
                                        depth + 1,
                                    );
                                }
                            }
                        }
                    } else {
                        rasterize_object_silhouette_with_deforms(
                            doc,
                            cid,
                            world_xf,
                            obj_world_xf,
                            deform_frames,
                            outer_to_sub,
                            paint,
                            backend,
                            depth + 1,
                        );
                    }
                }
                backend.pop_clip();
            }
        }
        NodePayload::Table(t) => {
            let obj_fill_vis = fill_is_visible(&obj.fill);
            let border_vis = t.border_style.is_visible();
            for r in 0..t.rows {
                for c in 0..t.cols {
                    let Some(cell) = t.cell(r, c) else {
                        continue;
                    };
                    let Some(rect) = t.cell_rect(r, c) else {
                        continue;
                    };
                    let cell_vis = obj_fill_vis
                        || border_vis
                        || fill_is_visible(&cell.fill)
                        || (!cell.text.trim().is_empty() && cell.text_style.fill.alpha() > 1e-6);
                    if cell_vis {
                        fill_geom(&BezierPath::from_rect(rect));
                    }
                }
            }
        }
        NodePayload::Dimension(d) => fill_geom(&BezierPath::from_rect(d.bounding_box())),
        NodePayload::Raster(r) => {
            if doc.asset(r.asset_id).is_some() {
                let rect = BezierPath::from_rect(r.crop_rect.unwrap_or_else(|| {
                    coree_core::Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm)
                }));
                fill_geom(&rect);
            }
        }
        NodePayload::ArtisticText(t) => {
            let has_visible_text = !t.plain_text().trim().is_empty()
                && (obj.outline.is_visible()
                    || fill_is_visible(&obj.fill)
                    || t.runs
                        .iter()
                        .any(|r| !r.text.trim().is_empty() && r.style.fill.alpha() > 1e-6));
            if has_visible_text {
                fill_geom(&BezierPath::from_rect(t.estimated_bounds()));
            }
        }
        NodePayload::ParagraphText(t) => {
            let has_visible_text = !t.plain_text().trim().is_empty()
                && (obj.outline.is_visible()
                    || fill_is_visible(&obj.fill)
                    || t.runs
                        .iter()
                        .any(|r| !r.text.trim().is_empty() && r.style.fill.alpha() > 1e-6));
            if has_visible_text {
                fill_geom(&BezierPath::from_rect(t.bounding_box()));
            }
        }
        NodePayload::Group(children) => {
            let group_has_deform = obj.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::Distort { .. } | Effect::Envelope { .. } | Effect::Perspective { .. }
                )
            });
            if group_has_deform {
                let group_local_bb = if sym_ctx.is_none() && obj.id.0 != 0 {
                    doc.local_object_bounds(obj.id)
                } else {
                    let mut v = HashSet::new();
                    SceneRenderer::compute_children_local_bounds(
                        doc,
                        children,
                        sym_ctx,
                        Affine2D::IDENTITY,
                        obj_world_xf,
                        depth + 1,
                        &mut v,
                    )
                };
                let (next_outer_to_sub, rel_to_outer) = if deform_frames.is_empty() {
                    (world_xf, Affine2D::IDENTITY)
                } else {
                    (outer_to_sub, world_xf)
                };
                let mut extended = Vec::with_capacity(deform_frames.len() + 1);
                extended.extend_from_slice(deform_frames);
                extended.push(DeformFrame {
                    group_local_bb,
                    group_effects: &obj.effects,
                    rel_to_outer,
                });
                for &cid in children {
                    if let Some((map, _)) = sym_ctx {
                        if let Some(sub) = map.get(&cid).copied() {
                            let sub_opacity = sanitize_f32(sub.opacity, 1.0).clamp(0.0, 1.0);
                            if sub.visible && sub_opacity > 1e-6 {
                                for sym_xf in symmetry_effect_transforms(&sub.effects) {
                                    let c_xf = sym_xf.compose(&sub.transform);
                                    let child_world =
                                        obj_world_xf.compose(&sym_xf).compose(&sub.transform);
                                    rasterize_silhouette_payload(
                                        doc,
                                        sub,
                                        c_xf,
                                        child_world,
                                        &extended,
                                        next_outer_to_sub,
                                        sym_ctx,
                                        paint,
                                        backend,
                                        depth + 1,
                                    );
                                }
                            }
                        }
                    } else {
                        rasterize_object_silhouette_with_deforms(
                            doc,
                            cid,
                            Affine2D::IDENTITY,
                            obj_world_xf,
                            &extended,
                            next_outer_to_sub,
                            paint,
                            backend,
                            depth + 1,
                        );
                    }
                }
            } else {
                for &cid in children {
                    if let Some((map, _)) = sym_ctx {
                        if let Some(sub) = map.get(&cid).copied() {
                            let sub_opacity = sanitize_f32(sub.opacity, 1.0).clamp(0.0, 1.0);
                            if sub.visible && sub_opacity > 1e-6 {
                                for sym_xf in symmetry_effect_transforms(&sub.effects) {
                                    let c_xf = world_xf.compose(&sym_xf).compose(&sub.transform);
                                    let child_world =
                                        obj_world_xf.compose(&sym_xf).compose(&sub.transform);
                                    rasterize_silhouette_payload(
                                        doc,
                                        sub,
                                        c_xf,
                                        child_world,
                                        deform_frames,
                                        outer_to_sub,
                                        sym_ctx,
                                        paint,
                                        backend,
                                        depth + 1,
                                    );
                                }
                            }
                        }
                    } else {
                        rasterize_object_silhouette_with_deforms(
                            doc,
                            cid,
                            world_xf,
                            obj_world_xf,
                            deform_frames,
                            outer_to_sub,
                            paint,
                            backend,
                            depth + 1,
                        );
                    }
                }
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        child_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                        NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        continue;
                    }
                    let sub_opacity = sanitize_f32(sub.opacity, 1.0).clamp(0.0, 1.0);
                    if !sub.visible || sub_opacity <= 1e-6 {
                        continue;
                    }
                    for sym_xf in symmetry_effect_transforms(&sub.effects) {
                        let sub_xf = world_xf.compose(&sym_xf).compose(&sub.transform);
                        let child_world = obj_world_xf.compose(&sym_xf).compose(&sub.transform);
                        rasterize_silhouette_payload(
                            doc,
                            sub,
                            sub_xf,
                            child_world,
                            deform_frames,
                            outer_to_sub,
                            Some((&child_map, obj_world_xf)),
                            paint,
                            backend,
                            depth + 1,
                        );
                    }
                }
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_object_silhouette_with_deforms(
    doc: &Document,
    id: ObjectId,
    parent_xf: Affine2D,
    parent_world_xf: Affine2D,
    deform_frames: &[DeformFrame<'_>],
    outer_to_sub: Affine2D,
    paint: &BackendPaint,
    backend: &mut dyn CanvasBackend,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    let Some(obj) = doc.object(id) else {
        return;
    };
    let obj_opacity = sanitize_f32(obj.opacity, 1.0).clamp(0.0, 1.0);
    if !obj.visible || obj_opacity <= 1e-6 {
        return;
    }
    let sym_transforms = symmetry_effect_transforms(&obj.effects);
    for sym_xf in sym_transforms {
        let world_xf = parent_xf.compose(&sym_xf).compose(&obj.transform);
        let obj_world_xf = parent_world_xf.compose(&sym_xf).compose(&obj.transform);
        rasterize_silhouette_payload(
            doc,
            obj,
            world_xf,
            obj_world_xf,
            deform_frames,
            outer_to_sub,
            None,
            paint,
            backend,
            depth,
        );
    }
}

#[inline]
fn safe_floor_i64(v: f64) -> i64 {
    if !v.is_finite() {
        0
    } else {
        v.floor().clamp(-1_000_000_000.0, 1_000_000_000.0) as i64
    }
}

#[inline]
fn safe_ceil_i64(v: f64) -> i64 {
    if !v.is_finite() {
        0
    } else {
        v.ceil().clamp(-1_000_000_000.0, 1_000_000_000.0) as i64
    }
}

#[inline]
fn estimate_scale_xf(xf: &Affine2D) -> f64 {
    let sx = xf.m[0].hypot(xf.m[1]);
    let sy = xf.m[2].hypot(xf.m[3]);
    (0.5 * (sx + sy)).max(1e-4)
}
