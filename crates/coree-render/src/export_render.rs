// SPDX-License-Identifier: MIT OR Apache-2.0

//! Headless high-DPI page export & visual regression rendering entry points.

use crate::backend::{validate_surface_dimensions, CanvasBackend, RgbaBuffer, TinySkiaBackend};
use crate::error::{RenderError, RenderResult};
use crate::renderer::{RenderOptions, SceneRenderer};
use crate::viewport::{ViewMode, Viewport};
use coree_core::math::{sanitize_f64, MM_PER_INCH};
use coree_core::Document;
use image::{ImageFormat, RgbaImage};
use std::io::Cursor;
use std::path::Path;

/// Straight-RGBA rendered page output produced by [`render_page_to_rgba`].
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub dpi: f64,
    pub rgba: Vec<u8>,
}

impl RenderedImage {
    /// Reads `[r, g, b, a]` at pixel `(x, y)`, or `[0, 0, 0, 0]` if out of bounds.
    #[inline]
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        [
            self.rgba[idx],
            self.rgba[idx + 1],
            self.rgba[idx + 2],
            self.rgba[idx + 3],
        ]
    }

    /// Encodes this rendered image as PNG bytes in memory.
    pub fn encode_png(&self) -> RenderResult<Vec<u8>> {
        let img = RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| RenderError::Raster("rendered RGBA buffer length mismatch".into()))?;
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, ImageFormat::Png)
            .map_err(|e| RenderError::Raster(format!("PNG encode failed: {e}")))?;
        Ok(cursor.into_inner())
    }

    /// Writes this rendered image directly to a PNG file on disk.
    pub fn save_png(&self, path: impl AsRef<Path>) -> RenderResult<()> {
        let img = RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| RenderError::Raster("rendered RGBA buffer length mismatch".into()))?;
        img.save_with_format(path, ImageFormat::Png)
            .map_err(|e| RenderError::Raster(format!("PNG save failed: {e}")))?;
        Ok(())
    }
}

/// Renders page `page_idx` of `doc` headlessly at `dpi` into a straight-RGBA [`RenderedImage`].
pub fn render_page_to_rgba(
    doc: &Document,
    page_idx: usize,
    dpi: f64,
    view_mode: ViewMode,
) -> RenderResult<RenderedImage> {
    render_page_to_rgba_with_options(doc, page_idx, dpi, view_mode, &RenderOptions::export())
}

/// Renders page `page_idx` headlessly with custom [`RenderOptions`] (e.g. transparent background or guides).
pub fn render_page_to_rgba_with_options(
    doc: &Document,
    page_idx: usize,
    dpi: f64,
    view_mode: ViewMode,
    options: &RenderOptions,
) -> RenderResult<RenderedImage> {
    let pages = doc.pages();
    if page_idx >= pages.len() {
        return Err(RenderError::PageOutOfRange {
            index: page_idx,
            len: pages.len(),
        });
    }
    if !dpi.is_finite() || dpi <= 0.0 {
        return Err(RenderError::InvalidParameter(format!(
            "DPI must be positive and finite, got {dpi}"
        )));
    }
    let safe_dpi = sanitize_f64(dpi, 96.0).clamp(1.0, 2400.0);
    let page = &pages[page_idx];
    let px_per_mm = safe_dpi / MM_PER_INCH;
    let width = (page.width_mm * px_per_mm).round().max(1.0) as u32;
    let height = (page.height_mm * px_per_mm).round().max(1.0) as u32;

    let mut backend = TinySkiaBackend::new(width, height)?;
    let viewport = Viewport::for_export(width, height, safe_dpi);

    // Clone only active_page_index / multipage flag onto a lightweight render call
    let mut doc_view = doc.clone();
    let _ = doc_view.set_active_page_index(page_idx);
    doc_view.set_multipage_view(false);

    let mut renderer = SceneRenderer::new();
    renderer.render_document(&doc_view, &viewport, view_mode, options, &mut backend);
    let buf = backend.into_rgba();

    Ok(RenderedImage {
        width: buf.width,
        height: buf.height,
        dpi: safe_dpi,
        rgba: buf.rgba,
    })
}

/// Rasterizes raw SVG bytes into a straight-RGBA [`RgbaBuffer`] of `width × height` pixels
/// using `usvg` + `resvg` (useful for SVG preview thumbnails & visual regression checks).
pub fn render_svg_to_rgba(svg_bytes: &[u8], width: u32, height: u32) -> RenderResult<RgbaBuffer> {
    validate_surface_dimensions(width, height)?;
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes, &opts)
        .map_err(|e| RenderError::Raster(format!("SVG parse failed: {e}")))?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or(RenderError::AllocationFailed { width, height })?;
    let size = tree.size();
    let sx = if size.width() > 0.0 {
        width as f32 / size.width()
    } else {
        1.0
    };
    let sy = if size.height() > 0.0 {
        height as f32 / size.height()
    } else {
        1.0
    };
    let mut pm_mut = pixmap.as_mut();
    resvg::render(&tree, tiny_skia::Transform::from_scale(sx, sy), &mut pm_mut);
    let rgba = pixmap.take_demultiplied();
    Ok(RgbaBuffer {
        width,
        height,
        rgba,
    })
}

pub(crate) fn subtree_contains_object(
    doc: &Document,
    candidate_id: coree_core::ObjectId,
    target_id: coree_core::ObjectId,
    depth: usize,
    visiting: &mut std::collections::HashSet<coree_core::ObjectId>,
) -> bool {
    if candidate_id == target_id {
        return true;
    }
    if depth > 64 || !visiting.insert(candidate_id) {
        return false;
    }
    let found = doc
        .object(candidate_id)
        .is_some_and(|obj| match &obj.payload {
            coree_core::NodePayload::Group(children) => children
                .iter()
                .any(|&cid| subtree_contains_object(doc, cid, target_id, depth + 1, visiting)),
            coree_core::NodePayload::PowerClip { contents, .. } => contents
                .iter()
                .any(|&cid| subtree_contains_object(doc, cid, target_id, depth + 1, visiting)),
            _ => false,
        });
    visiting.remove(&candidate_id);
    found
}

pub(crate) fn collect_underlying_until(
    doc: &Document,
    oid: coree_core::ObjectId,
    target_id: coree_core::ObjectId,
    out: &mut Vec<crate::lens_render::BackdropItem>,
    depth: usize,
    visiting: &mut std::collections::HashSet<coree_core::ObjectId>,
) -> bool {
    use crate::lens_render::BackdropItem;
    if oid == target_id {
        return true;
    }
    if depth > 64 || !visiting.insert(oid) {
        out.push(BackdropItem::Object(oid));
        return false;
    }
    let mut found = false;
    if let Some(obj) = doc.object(oid) {
        match &obj.payload {
            coree_core::NodePayload::Group(children) => {
                let contains = children.iter().any(|&cid| {
                    let mut sub_v = visiting.clone();
                    subtree_contains_object(doc, cid, target_id, depth + 1, &mut sub_v)
                });
                if contains {
                    let has_deform = obj.perspective.is_some()
                        || obj.effects.iter().any(|e| {
                            matches!(
                                e,
                                coree_core::Effect::Distort { .. }
                                    | coree_core::Effect::Envelope { .. }
                                    | coree_core::Effect::Perspective { .. }
                            )
                        });
                    if has_deform {
                        out.push(BackdropItem::EnterDeformedGroup {
                            group_id: oid,
                            group_sym_root_idx: None,
                            group_local_bb: doc.local_object_bounds(oid),
                            group_world_xf: None,
                        });
                    }
                    for &cid in children {
                        if collect_underlying_until(doc, cid, target_id, out, depth + 1, visiting) {
                            found = true;
                            break;
                        }
                    }
                    if has_deform && !found {
                        out.push(BackdropItem::ExitDeformedGroup);
                    }
                } else {
                    out.push(BackdropItem::Object(oid));
                }
            }
            coree_core::NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                let contains = contents.iter().any(|&cid| {
                    let mut sub_v = visiting.clone();
                    subtree_contains_object(doc, cid, target_id, depth + 1, &mut sub_v)
                });
                if contains {
                    out.push(BackdropItem::EnterPowerClip {
                        pc_id: oid,
                        sym_root_idx: None,
                    });
                    let has_deform = obj.perspective.is_some()
                        || obj.effects.iter().any(|e| {
                            matches!(
                                e,
                                coree_core::Effect::Distort { .. }
                                    | coree_core::Effect::Envelope { .. }
                                    | coree_core::Effect::Perspective { .. }
                            )
                        });
                    if has_deform {
                        let pc_local_bb = obj
                            .perspective
                            .map_or_else(
                                || (**container).clone(),
                                |p| p.transform_bezier_path(container),
                            )
                            .bounding_box();
                        out.push(BackdropItem::EnterDeformedGroup {
                            group_id: oid,
                            group_sym_root_idx: None,
                            group_local_bb: pc_local_bb,
                            group_world_xf: None,
                        });
                    }
                    for &cid in contents {
                        if collect_underlying_until(doc, cid, target_id, out, depth + 1, visiting) {
                            found = true;
                            break;
                        }
                    }
                    if has_deform && !found {
                        out.push(BackdropItem::ExitDeformedGroup);
                    }
                } else {
                    out.push(BackdropItem::Object(oid));
                }
            }
            _ => {
                out.push(BackdropItem::Object(oid));
            }
        }
    } else {
        out.push(BackdropItem::Object(oid));
    }
    visiting.remove(&oid);
    found
}

pub(crate) fn find_object_page_index(doc: &Document, obj_id: coree_core::ObjectId) -> usize {
    let mut top_id = obj_id;
    let mut guard = std::collections::HashSet::from([top_id]);
    while let Some(pid) = doc.parent_container_of(top_id) {
        if !guard.insert(pid) {
            break;
        }
        top_id = pid;
    }
    let pages = doc.pages();
    let active_idx = doc.active_page_index().min(pages.len().saturating_sub(1));
    let mut master_match: Option<usize> = None;
    for (p_idx, page) in pages.iter().enumerate() {
        for layer in &page.layers {
            if layer.objects.contains(&top_id) {
                let is_master = layer.is_desktop_layer
                    || layer.master_scope != coree_core::MasterLayerScope::None;
                if !is_master {
                    return p_idx;
                }
                if Document::layer_applies_to_page(layer, p_idx, active_idx) {
                    master_match = Some(active_idx);
                } else if master_match.is_none() {
                    master_match = Some(p_idx);
                }
            }
        }
    }
    master_match.unwrap_or(active_idx)
}

pub(crate) fn object_or_subtree_has_lens(
    doc: &Document,
    obj: &coree_core::SceneObject,
    depth: usize,
    visiting: &mut std::collections::HashSet<coree_core::ObjectId>,
) -> bool {
    let mut visiting_syms = std::collections::HashSet::new();
    object_or_subtree_has_lens_with_sym(doc, obj, None, depth, visiting, &mut visiting_syms)
}

pub(crate) fn object_or_subtree_has_lens_with_sym(
    doc: &Document,
    obj: &coree_core::SceneObject,
    sym_map: Option<&std::collections::HashMap<coree_core::ObjectId, &coree_core::SceneObject>>,
    depth: usize,
    visiting: &mut std::collections::HashSet<coree_core::ObjectId>,
    visiting_syms: &mut std::collections::HashSet<coree_core::SymbolId>,
) -> bool {
    if obj.lens.is_some() {
        return true;
    }
    if depth > 64 {
        return false;
    }
    match &obj.payload {
        coree_core::NodePayload::Group(children) => children.iter().any(|&cid| {
            if !visiting.insert(cid) {
                return false;
            }
            let hit = sym_map
                .and_then(|m| m.get(&cid).copied())
                .or_else(|| doc.object(cid))
                .is_some_and(|c| {
                    object_or_subtree_has_lens_with_sym(
                        doc,
                        c,
                        sym_map,
                        depth + 1,
                        visiting,
                        visiting_syms,
                    )
                });
            visiting.remove(&cid);
            hit
        }),
        coree_core::NodePayload::PowerClip { contents, .. } => contents.iter().any(|&cid| {
            if !visiting.insert(cid) {
                return false;
            }
            let hit = sym_map
                .and_then(|m| m.get(&cid).copied())
                .or_else(|| doc.object(cid))
                .is_some_and(|c| {
                    object_or_subtree_has_lens_with_sym(
                        doc,
                        c,
                        sym_map,
                        depth + 1,
                        visiting,
                        visiting_syms,
                    )
                });
            visiting.remove(&cid);
            hit
        }),
        coree_core::NodePayload::SymbolInstance(sym_id) => {
            if !visiting_syms.insert(*sym_id) {
                return false;
            }
            let hit = doc.symbol(*sym_id).is_some_and(|sym| {
                let mut child_map: std::collections::HashMap<
                    coree_core::ObjectId,
                    &coree_core::SceneObject,
                > = std::collections::HashMap::new();
                let mut referenced: std::collections::HashSet<coree_core::ObjectId> =
                    std::collections::HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        child_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        coree_core::NodePayload::Group(c) => {
                            referenced.extend(c.iter().copied());
                        }
                        coree_core::NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                let mut sym_visiting = std::collections::HashSet::new();
                sym.objects.iter().any(|sub| {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        return false;
                    }
                    if sub.id.0 != 0 && !sym_visiting.insert(sub.id) {
                        return false;
                    }
                    let h = object_or_subtree_has_lens_with_sym(
                        doc,
                        sub,
                        Some(&child_map),
                        depth + 1,
                        &mut sym_visiting,
                        visiting_syms,
                    );
                    if sub.id.0 != 0 {
                        sym_visiting.remove(&sub.id);
                    }
                    h
                })
            });
            visiting_syms.remove(sym_id);
            hit
        }
        _ => false,
    }
}

/// Renders an isolated [`coree_core::SceneObject`] (including deformations, drop/block shadows,
/// contours, 2.5D extrusions, blends, conical/square/mesh/pattern/texture fills, transparency
/// masks, symmetry clones, and live optical lenses) into a cropped straight-RGBA [`RgbaBuffer`]
/// paired with its page-space bounding rectangle in millimeters.
pub fn render_isolated_object_to_rgba(
    doc: &Document,
    page_idx: usize,
    obj_id: coree_core::ObjectId,
    parent_xf: coree_core::Affine2D,
    dpi: f64,
) -> RenderResult<Option<(coree_core::Rect2D, RgbaBuffer)>> {
    use crate::effects_render::{
        apply_geometry_effects, extract_local_geometry_path, symmetry_effect_transforms,
    };
    use crate::renderer::{collect_effective_layers, max_object_effect_padding_mm};
    use coree_core::{Affine2D, Color, Effect, NodePayload, Rect2D};
    use std::collections::HashSet;

    let pages = doc.pages();
    if page_idx >= pages.len() || !parent_xf.is_finite() {
        return Ok(None);
    }
    let Some(obj) = doc.object(obj_id) else {
        return Ok(None);
    };
    if !obj.visible || !obj.transform.is_finite() {
        return Ok(None);
    }
    let safe_dpi = sanitize_f64(dpi, 300.0).clamp(36.0, 600.0);
    let page = &pages[page_idx];
    let effective_parent_xf = if parent_xf == Affine2D::IDENTITY {
        doc.parent_container_of(obj_id)
            .and_then(|pid| doc.world_transform_of(pid))
            .filter(|m| m.is_finite())
            .unwrap_or(Affine2D::IDENTITY)
    } else {
        parent_xf
    };
    let inv_effective_parent = effective_parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);

    // Estimate object-local bounds including vector geometry deformations & Blends
    let mut renderer = SceneRenderer::new();
    renderer.force_offscreen_lens_backdrop = true;
    if parent_xf != Affine2D::IDENTITY {
        renderer.parent_xf_override = doc
            .parent_container_of(obj_id)
            .map(|pid| (pid, effective_parent_xf))
            .or_else(|| {
                effective_parent_xf
                    .compose(&obj.transform)
                    .is_finite()
                    .then(|| (obj_id, effective_parent_xf.compose(&obj.transform)))
            });
    }
    let resolved_obj_owned;
    let obj_eff: &coree_core::SceneObject =
        if matches!(obj.payload, coree_core::NodePayload::Clone { .. }) {
            if let Some((p, f, o)) = doc.resolve_clone_payload_and_style(obj) {
                let mut c = obj.clone();
                c.payload = p;
                c.fill = f;
                c.outline = o;
                resolved_obj_owned = c;
                &resolved_obj_owned
            } else {
                obj
            }
        } else {
            obj
        };
    let use_visual_bounds = matches!(
        obj_eff.payload,
        NodePayload::Group(_)
            | NodePayload::SymbolInstance(_)
            | NodePayload::Table(_)
            | NodePayload::Dimension(_)
    ) || (matches!(
        obj_eff.payload,
        NodePayload::TextOnPath(_) | NodePayload::Connector(_)
    ) && obj_eff.effects.iter().any(|e| {
        matches!(
            e,
            Effect::Distort { .. } | Effect::Envelope { .. } | Effect::Perspective { .. }
        )
    }));
    let page_bb: Option<Rect2D> = if use_visual_bounds {
        if parent_xf == Affine2D::IDENTITY {
            doc.visual_world_bounds(obj_id)
        } else {
            doc.visual_world_bounds_with_parent(obj_id, effective_parent_xf)
        }
    } else {
        let local_bb = match &obj_eff.payload {
            NodePayload::ArtisticText(at) => {
                Some(renderer.text_engine_mut().measure_artistic_bounds(at))
            }
            _ => extract_local_geometry_path(obj_eff)
                .and_then(|p| apply_geometry_effects(&p, &obj_eff.effects).bounding_box()),
        };
        let doc_world_bb = if local_bb.is_none() {
            match &obj_eff.payload {
                NodePayload::TextOnPath(top) if parent_xf != Affine2D::IDENTITY => {
                    let doc_parent_world = doc
                        .parent_container_of(obj_id)
                        .and_then(|pid| doc.world_transform_of(pid))
                        .filter(|m| m.is_finite())
                        .unwrap_or(Affine2D::IDENTITY);
                    let delta_xf = effective_parent_xf
                        .compose(&doc_parent_world.inverse().unwrap_or(Affine2D::IDENTITY));
                    doc.world_path_of(top.path_id)
                        .and_then(|wp| wp.transformed(&delta_xf).bounding_box())
                        .or_else(|| {
                            doc.visual_world_bounds_with_parent(obj_id, effective_parent_xf)
                        })
                }
                NodePayload::Connector(_) if parent_xf != Affine2D::IDENTITY => {
                    doc.visual_world_bounds_with_parent(obj_id, effective_parent_xf)
                }
                NodePayload::TextOnPath(_) | NodePayload::Connector(_) => {
                    doc.world_object_bounds(obj_id)
                }
                _ if parent_xf == Affine2D::IDENTITY => doc.world_object_bounds(obj_id),
                _ => doc.visual_world_bounds_with_parent(obj_id, effective_parent_xf),
            }
        } else {
            None
        };
        let doc_obj_bb: Option<Rect2D> = None;

        let mut acc_bb: Option<Rect2D> = None;
        let sym_xfs = symmetry_effect_transforms(&obj.effects);
        for sym_xf in sym_xfs {
            let obj_page_xf = effective_parent_xf.compose(&sym_xf).compose(&obj.transform);
            if !obj_page_xf.is_finite() {
                continue;
            }
            if let Some(ref lbb) = local_bb {
                let tb = obj_page_xf.transform_rect(*lbb);
                acc_bb = Some(match acc_bb {
                    Some(acc) => acc.union(tb),
                    None => tb,
                });
            } else if let Some(wbb) = doc_world_bb {
                let world_sym_xf = if sym_xf == Affine2D::IDENTITY {
                    Affine2D::IDENTITY
                } else {
                    let w = effective_parent_xf
                        .compose(&sym_xf)
                        .compose(&inv_effective_parent);
                    if w.is_finite() {
                        w
                    } else {
                        sym_xf
                    }
                };
                if let Some(tb) = world_sym_xf.try_transform_rect(wbb) {
                    acc_bb = Some(match acc_bb {
                        Some(acc) => acc.union(tb),
                        None => tb,
                    });
                }
            } else if let Some(dbb) = doc_obj_bb {
                // `doc.object_bounds` returns parent-local bounds; apply `effective_parent_xf * sym_xf`
                let tb = effective_parent_xf.compose(&sym_xf).transform_rect(dbb);
                acc_bb = Some(match acc_bb {
                    Some(acc) => acc.union(tb),
                    None => tb,
                });
            }
        }

        for eff in &obj.effects {
            match eff {
                Effect::Blend { target_id, .. } => {
                    if let Some(tb) = doc.world_object_bounds(*target_id) {
                        acc_bb = Some(match acc_bb {
                            Some(acc) => acc.union(tb),
                            None => tb,
                        });
                    }
                }
                Effect::Extrude25D {
                    vanishing_point, ..
                } if vanishing_point.is_finite() => {
                    let vp_rect = Rect2D::from_xywh(vanishing_point.x, vanishing_point.y, 0.1, 0.1);
                    acc_bb = Some(match acc_bb {
                        Some(acc) => acc.union(vp_rect),
                        None => vp_rect,
                    });
                }
                _ => {}
            }
        }
        acc_bb
    };

    let Some(raw_bb) = page_bb else {
        return Ok(None);
    };
    let pad = max_object_effect_padding_mm(doc, obj, effective_parent_xf)
        + obj.outline.effective_width_mm().max(0.5) * 2.0
        + doc.spatial_bounds_padding_mm(obj_id)
        + 2.0;
    let padded = raw_bb.expand(pad);
    // Guard against runaway oversized coordinates while keeping generous bleed margin
    let safe_limit = Rect2D::from_xywh(
        -page.width_mm,
        -page.height_mm,
        page.width_mm * 3.0,
        page.height_mm * 3.0,
    );
    let Some(rect_mm) = padded.intersection(safe_limit) else {
        return Ok(None);
    };
    let w_mm = rect_mm.width().max(0.1);
    let h_mm = rect_mm.height().max(0.1);

    let base_px_per_mm = safe_dpi / MM_PER_INCH;
    let raw_w = (w_mm * base_px_per_mm).ceil();
    let raw_h = (h_mm * base_px_per_mm).ceil();
    let max_dim = 2048.0_f64;
    let scale_down = if raw_w.max(raw_h) > max_dim {
        max_dim / raw_w.max(raw_h)
    } else {
        1.0
    };
    let px_per_mm = base_px_per_mm * scale_down;
    let px_w = (w_mm * px_per_mm).ceil().clamp(1.0, 2048.0) as u32;
    let px_h = (h_mm * px_per_mm).ceil().clamp(1.0, 2048.0) as u32;

    let mut underlying_ids = Vec::new();
    let mut lens_visit = HashSet::from([obj_id]);
    if object_or_subtree_has_lens(doc, obj, 0, &mut lens_visit) {
        let layers = collect_effective_layers(doc, page_idx);
        'layer_loop: for layer in layers {
            if !layer.visible {
                continue;
            }
            for &oid in &layer.objects {
                let mut sub_visit = HashSet::new();
                if collect_underlying_until(
                    doc,
                    oid,
                    obj_id,
                    &mut underlying_ids,
                    0,
                    &mut sub_visit,
                ) {
                    break 'layer_loop;
                }
            }
        }
    }

    let mut backend = TinySkiaBackend::new(px_w, px_h)?;
    backend.clear(Color::TRANSPARENT);
    let page_to_surface = Affine2D::scale(px_per_mm, px_per_mm)
        .compose(&Affine2D::translate(-rect_mm.min_x, -rect_mm.min_y));
    let parent_to_surface = page_to_surface.compose(&effective_parent_xf);

    renderer.render_scene_object_with_backdrop(
        doc,
        Some(page),
        obj,
        page_to_surface,
        parent_to_surface,
        ViewMode::Enhanced,
        &RenderOptions::export(),
        &mut backend,
        &underlying_ids,
        0,
    );
    let buf = backend.into_rgba();
    Ok(Some((rect_mm, buf)))
}
