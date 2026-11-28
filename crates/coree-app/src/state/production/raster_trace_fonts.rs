// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// Resolved raster object (including rasters nested inside imported PDF `PowerClip`s, `Group`s, or `Clone`s).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedTraceTarget {
    /// Selected top-level container or object ID (deleted if `keep_original == false`).
    pub root_id: ObjectId,
    /// Underlying raster payload.
    pub raster: coree_core::RasterLayerData,
    /// Embedded pixel asset.
    pub asset: RasterAsset,
    /// Effective world transform of the raster object.
    pub world_xf: Affine2D,
    /// Optional perspective envelope on the raster/container.
    pub persp: Option<coree_core::Perspective3x3>,
    /// Effective local crop rect in `[0..width_mm, 0..height_mm]` combining `crop_rect` & `PowerClip` bounds.
    pub effective_crop: Option<Rect2D>,
}

fn intersect_optional_rects(a: Option<Rect2D>, b: Option<Rect2D>) -> Option<Rect2D> {
    match (a, b) {
        (Some(ra), Some(rb)) => {
            let min_x = ra.min_x.max(rb.min_x);
            let min_y = ra.min_y.max(rb.min_y);
            let max_x = ra.max_x.min(rb.max_x);
            let max_y = ra.max_y.min(rb.max_y);
            Some(Rect2D {
                min_x,
                min_y,
                max_x,
                max_y,
            })
        }
        (Some(ra), None) => Some(ra),
        (None, Some(rb)) => Some(rb),
        (None, None) => None,
    }
}

/// Recursively resolves a single traceable raster inside `id` (`Raster`, `Clone`, `PowerClip`, or `Group`),
/// mapping any `PowerClip` container clip bounds into the raster's local millimeter coordinate space.
fn resolve_single_raster_in_subtree(
    doc: &Document,
    root_id: ObjectId,
    cur_id: ObjectId,
    world_clip: Option<Rect2D>,
    depth: usize,
) -> Option<ResolvedTraceTarget> {
    if depth > 16 {
        return None;
    }
    let obj = doc.object(cur_id)?;
    if !obj.visible {
        return None;
    }
    match &obj.payload {
        NodePayload::Raster(r) => {
            let asset = doc.asset(r.asset_id)?.clone();
            let world_xf = doc.world_transform_of(cur_id).unwrap_or(obj.transform);
            let local_clip_from_world = world_clip.and_then(|wc| {
                let inv = world_xf.inverse()?;
                let corners = [
                    inv.transform_point(Point2D::new(wc.min_x, wc.min_y)),
                    inv.transform_point(Point2D::new(wc.max_x, wc.min_y)),
                    inv.transform_point(Point2D::new(wc.max_x, wc.max_y)),
                    inv.transform_point(Point2D::new(wc.min_x, wc.max_y)),
                ];
                let min_x = corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
                let min_y = corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
                let max_x = corners
                    .iter()
                    .map(|p| p.x)
                    .fold(f64::NEG_INFINITY, f64::max);
                let max_y = corners
                    .iter()
                    .map(|p| p.y)
                    .fold(f64::NEG_INFINITY, f64::max);
                Some(Rect2D::new(min_x, min_y, max_x, max_y))
            });
            let merged_crop = intersect_optional_rects(
                r.crop_rect.filter(|c| c.is_finite()),
                local_clip_from_world,
            );
            Some(ResolvedTraceTarget {
                root_id,
                raster: r.clone(),
                asset,
                world_xf,
                persp: obj.perspective,
                effective_crop: merged_crop,
            })
        }
        NodePayload::Clone { .. } => {
            let (eff_payload, _, _, _) =
                doc.resolve_clone_payload_and_style_with_sym_scope(obj, None)?;
            let NodePayload::Raster(r) = eff_payload else {
                return None;
            };
            let asset = doc.asset(r.asset_id)?.clone();
            let world_xf = doc.world_transform_of(cur_id).unwrap_or(obj.transform);
            Some(ResolvedTraceTarget {
                root_id,
                raster: r.clone(),
                asset,
                world_xf,
                persp: obj.perspective,
                effective_crop: r.crop_rect.filter(|c| c.is_finite()),
            })
        }
        NodePayload::PowerClip {
            container,
            contents,
            ..
        } => {
            let pc_world_xf = doc.world_transform_of(cur_id).unwrap_or(obj.transform);
            let pc_world_bb = container.bounding_box().map(|bb| {
                let corners = [
                    pc_world_xf.transform_point(Point2D::new(bb.min_x, bb.min_y)),
                    pc_world_xf.transform_point(Point2D::new(bb.max_x, bb.min_y)),
                    pc_world_xf.transform_point(Point2D::new(bb.max_x, bb.max_y)),
                    pc_world_xf.transform_point(Point2D::new(bb.min_x, bb.max_y)),
                ];
                let min_x = corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
                let min_y = corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
                let max_x = corners
                    .iter()
                    .map(|p| p.x)
                    .fold(f64::NEG_INFINITY, f64::max);
                let max_y = corners
                    .iter()
                    .map(|p| p.y)
                    .fold(f64::NEG_INFINITY, f64::max);
                Rect2D::new(min_x, min_y, max_x, max_y)
            });
            let next_clip = intersect_optional_rects(world_clip, pc_world_bb);
            let mut found: Option<ResolvedTraceTarget> = None;
            for &cid in contents {
                if let Some(res) =
                    resolve_single_raster_in_subtree(doc, root_id, cid, next_clip, depth + 1)
                {
                    found = Some(res);
                }
            }
            found
        }
        NodePayload::Group(children) => {
            let mut found: Option<ResolvedTraceTarget> = None;
            for &cid in children {
                if let Some(res) =
                    resolve_single_raster_in_subtree(doc, root_id, cid, world_clip, depth + 1)
                {
                    found = Some(res);
                }
            }
            found
        }
        _ => None,
    }
}

fn subtree_contains_only_rasters(doc: &Document, id: ObjectId, depth: usize) -> bool {
    if depth > 16 {
        return false;
    }
    let Some(obj) = doc.object(id) else {
        return false;
    };
    match &obj.payload {
        NodePayload::Raster(_) => true,
        NodePayload::Clone { .. } => doc
            .resolve_clone_payload_and_style_with_sym_scope(obj, None)
            .is_some_and(|(p, _, _, _)| matches!(p, NodePayload::Raster(_))),
        NodePayload::PowerClip { contents, .. } => {
            !contents.is_empty()
                && contents
                    .iter()
                    .all(|&c| subtree_contains_only_rasters(doc, c, depth + 1))
        }
        NodePayload::Group(children) => {
            !children.is_empty()
                && children
                    .iter()
                    .all(|&c| subtree_contains_only_rasters(doc, c, depth + 1))
        }
        _ => false,
    }
}

pub(in crate::state) fn crop_filtered_trace_asset(
    filtered_asset: RasterAsset,
    crop_rect: Option<Rect2D>,
    w_mm: f64,
    h_mm: f64,
) -> Result<(RasterAsset, f64, f64, f64, f64), String> {
    if let Some(cr) = crop_rect.filter(|c| c.is_finite()) {
        let vis_min_x = cr.min_x.max(0.0);
        let vis_min_y = cr.min_y.max(0.0);
        let vis_max_x = cr.max_x.min(w_mm);
        let vis_max_y = cr.max_y.min(h_mm);
        if vis_max_x - vis_min_x <= 1e-6 || vis_max_y - vis_min_y <= 1e-6 {
            return Err("cropped raster region is empty (outside image bounds)".to_owned());
        }
        let aw = filtered_asset.width;
        let ah = filtered_asset.height;
        if aw < 2 || ah < 2 {
            return Err("raster resolution too small to trace".to_owned());
        }
        let px_per_mm_x = f64::from(aw) / w_mm;
        let px_per_mm_y = f64::from(ah) / h_mm;
        let mut x0 = (vis_min_x * px_per_mm_x)
            .floor()
            .clamp(0.0, (aw - 1) as f64) as u32;
        let mut y0 = (vis_min_y * px_per_mm_y)
            .floor()
            .clamp(0.0, (ah - 1) as f64) as u32;
        let mut x1 = (vis_max_x * px_per_mm_x)
            .ceil()
            .clamp((x0 + 1) as f64, aw as f64) as u32;
        let mut y1 = (vis_max_y * px_per_mm_y)
            .ceil()
            .clamp((y0 + 1) as f64, ah as f64) as u32;
        if x1 - x0 < 2 {
            if x1 < aw {
                x1 += 1;
            } else {
                x0 = x0.saturating_sub(1);
            }
        }
        if y1 - y0 < 2 {
            if y1 < ah {
                y1 += 1;
            } else {
                y0 = y0.saturating_sub(1);
            }
        }
        let cw = x1 - x0;
        let ch = y1 - y0;
        let filtered_rgba = filtered_asset.to_rgba8_vec();
        let mut cropped_bytes = Vec::with_capacity((cw * ch * 4) as usize);
        for row in y0..y1 {
            let start = ((row * aw + x0) * 4) as usize;
            let end = ((row * aw + x1) * 4) as usize;
            cropped_bytes.extend_from_slice(&filtered_rgba[start..end]);
        }
        let cropped_asset = RasterAsset::new(
            cw,
            ch,
            cropped_bytes,
            filtered_asset.source_dpi,
            filtered_asset.color_mode,
        )
        .map_err(|e| e.to_string())?;
        Ok((
            cropped_asset,
            f64::from(x0) / px_per_mm_x,
            f64::from(y0) / px_per_mm_y,
            f64::from(cw) / px_per_mm_x,
            f64::from(ch) / px_per_mm_y,
        ))
    } else {
        Ok((filtered_asset, 0.0, 0.0, w_mm, h_mm))
    }
}

pub(in crate::state) fn compute_trace_placed_transform(
    xf: Affine2D,
    persp: Option<coree_core::Perspective3x3>,
    offset_x_mm: f64,
    offset_y_mm: f64,
    trace_w_mm: f64,
    trace_h_mm: f64,
    place_side_by_side: bool,
) -> Affine2D {
    let mut placed_xf = xf;
    if place_side_by_side {
        let corners = [
            Point2D::new(offset_x_mm, offset_y_mm),
            Point2D::new(offset_x_mm + trace_w_mm, offset_y_mm),
            Point2D::new(offset_x_mm + trace_w_mm, offset_y_mm + trace_h_mm),
            Point2D::new(offset_x_mm, offset_y_mm + trace_h_mm),
        ];
        let (mut min_wx, mut max_wx) = (f64::INFINITY, f64::NEG_INFINITY);
        for c in corners {
            let cp = persp.and_then(|p| p.transform_point(c)).unwrap_or(c);
            let wp = xf.transform_point(cp);
            min_wx = min_wx.min(wp.x);
            max_wx = max_wx.max(wp.x);
        }
        if (max_wx - min_wx).is_finite() {
            placed_xf.m[4] += (max_wx - min_wx).max(1.0) + 6.0;
        }
    }
    placed_xf
}

impl StudioApp {
    /// Returns `true` if any currently selected object is (or contains inside a `PowerClip`/`Group`) a raster bitmap.
    #[must_use]
    pub fn selection_has_traceable_raster(&self) -> bool {
        self.active_tab().is_some_and(|tab| {
            tab.selection.objects.iter().any(|&id| {
                resolve_single_raster_in_subtree(&tab.document, id, id, None, 0).is_some()
            })
        })
    }

    /// Returns `true` if the non-empty selection consists exclusively of rasters or `PowerClip`/`Group` wrappers around rasters.
    #[must_use]
    pub fn selection_is_exclusively_traceable_raster(&self) -> bool {
        self.active_tab().is_some_and(|tab| {
            !tab.selection.objects.is_empty()
                && tab
                    .selection
                    .objects
                    .iter()
                    .all(|&id| subtree_contains_only_rasters(&tab.document, id, 0))
        })
    }

    /// Finds the active raster target from the current selection (supporting imported PDF `PowerClip`s and `Group`s),
    /// falling back to the first visible editable raster on the active page.
    #[must_use]
    pub(crate) fn find_trace_target_in_tab(tab: &DocumentTab) -> Option<ResolvedTraceTarget> {
        for &sel_id in &tab.selection.objects {
            if let Some(target) =
                resolve_single_raster_in_subtree(&tab.document, sel_id, sel_id, None, 0)
            {
                return Some(target);
            }
        }
        let active_page_idx = tab.document.active_page_index();
        for (p_idx, page) in tab.document.pages().iter().enumerate() {
            for layer in &page.layers {
                if !layer.visible
                    || layer.locked
                    || !Document::layer_applies_to_page(layer, p_idx, active_page_idx)
                {
                    continue;
                }
                for &oid in layer.objects.iter().rev() {
                    if is_object_editable_in_doc(&tab.document, oid) {
                        if let Some(target) =
                            resolve_single_raster_in_subtree(&tab.document, oid, oid, None, 0)
                        {
                            return Some(target);
                        }
                    }
                }
            }
        }
        None
    }

    /// Traces the selected raster object via `coree_render::trace_raster_to_mm` and adds grouped vector curves.
    pub fn trace_selected_raster(&mut self) -> Result<usize, String> {
        let layer_id = self.active_creation_layer_or_err()?;
        let cfg = self.trace_state.to_config();
        let is_centerline = matches!(cfg.mode, TraceMode::CenterlineTrace);
        let place_side_by_side = self.trace_state.place_side_by_side;
        let keep_original = self.trace_state.keep_original;
        let (target_id, traced, offset_x_mm, offset_y_mm, placed_xf, persp) = {
            let tab = self
                .tabs
                .get(self.active_tab)
                .ok_or_else(|| "no active document".to_owned())?;

            let resolved = Self::find_trace_target_in_tab(tab)
                .ok_or_else(|| "select a raster bitmap object to run Vector Trace".to_owned())?;
            let target_id = resolved.root_id;
            let asset = resolved.asset;
            let filters = resolved.raster.active_filters();
            let crop_rect = resolved.effective_crop;
            let w_mm = resolved.raster.width_mm;
            let h_mm = resolved.raster.height_mm;
            let xf = resolved.world_xf;
            let persp = resolved.persp;

            if !w_mm.is_finite() || w_mm <= 0.0 || !h_mm.is_finite() || h_mm <= 0.0 {
                return Err("raster has invalid dimensions".to_owned());
            }

            let filtered_asset = if filters.is_empty() {
                asset
            } else {
                coree_render::apply_raster_filters(&asset, &filters)
            };

            let (trace_asset, offset_x_mm, offset_y_mm, trace_w_mm, trace_h_mm) =
                crop_filtered_trace_asset(filtered_asset, crop_rect, w_mm, h_mm)?;

            let traced = trace_raster_to_mm(&trace_asset, trace_w_mm, trace_h_mm, &cfg);
            if traced.is_empty() {
                return Err("trace produced 0 contours (adjust detail or colors)".to_owned());
            }

            let placed_xf = compute_trace_placed_transform(
                xf,
                persp,
                offset_x_mm,
                offset_y_mm,
                trace_w_mm,
                trace_h_mm,
                place_side_by_side,
            );
            (
                target_id,
                traced,
                offset_x_mm,
                offset_y_mm,
                placed_xf,
                persp,
            )
        };

        self.commit_traced_contours(
            target_id,
            layer_id,
            traced,
            offset_x_mm,
            offset_y_mm,
            placed_xf,
            persp,
            is_centerline,
            keep_original,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_traced_contours(
        &mut self,
        target_id: ObjectId,
        layer_id: Option<LayerId>,
        traced: Vec<(BezierPath, Color)>,
        offset_x_mm: f64,
        offset_y_mm: f64,
        placed_xf: Affine2D,
        persp: Option<coree_core::Perspective3x3>,
        is_centerline: bool,
        keep_original: bool,
    ) -> Result<usize, String> {
        let count = traced.len();
        let (ops, created_ids) = {
            let tab = self
                .tabs
                .get(self.active_tab)
                .ok_or_else(|| "no active document".to_owned())?;
            let mut next_id = tab.document.next_id().max(1);
            let mut ops = Vec::with_capacity(count + 2);
            let mut created_ids = Vec::with_capacity(count);
            for (idx, (mut path, color)) in traced.into_iter().enumerate() {
                if offset_x_mm.abs() > 1e-9 || offset_y_mm.abs() > 1e-9 {
                    path.transform(&Affine2D::translate(offset_x_mm, offset_y_mm));
                }
                if let Some(p) = persp {
                    path = p.transform_bezier_path(&path);
                }
                let oid = ObjectId(next_id);
                next_id = next_id.saturating_add(1);
                let mut obj = SceneObject::new(
                    oid,
                    format!("Traced Curve {}", idx + 1),
                    NodePayload::Path(path),
                )
                .with_transform(placed_xf);
                if is_centerline {
                    obj.fill = Fill::None;
                    obj.outline = OutlineStyle::solid(color, 0.25);
                } else {
                    obj.fill = Fill::Solid(color);
                    obj.outline = OutlineStyle::none();
                }
                created_ids.push(oid);
                ops.push(CommandOp::create_object(obj, layer_id));
            }
            if created_ids.len() > 1 {
                ops.push(CommandOp::Group {
                    ids: created_ids.clone(),
                    name: "Vector Trace Result".to_owned(),
                });
            }
            if !keep_original && tab.document.has_object(target_id) {
                ops.push(CommandOp::DeleteObjects {
                    ids: vec![target_id],
                });
            }
            (ops, created_ids)
        };

        let cmd = if ops.len() == 1 {
            ops.into_iter().next().expect("len == 1")
        } else {
            CommandOp::Batch(ops)
        };
        self.execute_command(cmd)?;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if created_ids.len() > 1 {
                if let Some(gid) = tab.document.parent_container_of(created_ids[0]) {
                    tab.selection.select_single(&tab.document, gid);
                }
            } else if let Some(&single) = created_ids.first() {
                tab.selection.select_single(&tab.document, single);
            }
            tab.needs_rerender = true;
        }
        self.push_toast(format!("Vector Trace created {count} curve(s)"), false);
        Ok(count)
    }

    /// Converts the first selected vector object into an embedded raster bitmap at `dpi`.
    /// Uses `coree_render::convert_object_to_bitmap_with_bounds` so the resulting `RasterLayerData`
    /// shares the exact padded world rect (`4.0 + spatial_bounds_padding_mm + effect_bounds_padding_mm`
    /// plus `Blend`/`Extrude25D` extents) and aligns 1:1 in world space with zero shrink or shift.
    pub fn convert_selection_to_bitmap(&mut self) {
        let Some(layer_id) = self.ensure_active_creation_layer() else {
            return;
        };
        let dpi = self.bitmap_state.convert_dpi.clamp(36.0, 1200.0);
        let plan = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return;
            };
            let Some(&target_id) = tab.selection.objects.first() else {
                self.push_toast("Select an object to Convert to Bitmap", false);
                return;
            };
            match convert_object_to_bitmap_with_bounds(&tab.document, target_id, dpi) {
                Ok((asset, padded)) => {
                    let expected_id = ObjectId(tab.document.next_id().max(1));
                    let aid = asset.id;
                    let raster_obj = SceneObject::new(
                        ObjectId(0),
                        "Rasterized Bitmap",
                        NodePayload::Raster(coree_core::RasterLayerData {
                            asset_id: aid,
                            width_mm: padded.width().max(1.0),
                            height_mm: padded.height().max(1.0),
                            filters: Vec::new(),
                            filter_enabled: Vec::new(),
                            crop_rect: None,
                        }),
                    )
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::none())
                    .with_transform(Affine2D::translate(padded.min_x, padded.min_y));
                    Ok((
                        expected_id,
                        CommandOp::Batch(vec![
                            CommandOp::RegisterAsset { asset },
                            CommandOp::create_object(raster_obj, layer_id),
                        ]),
                    ))
                }
                Err(e) => Err(format!("Convert to Bitmap failed: {e}")),
            }
        };
        match plan {
            Ok((expected_id, cmd)) => match self.execute_command(cmd) {
                Ok(()) => {
                    if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                        if tab.document.has_object(expected_id) {
                            tab.selection.select_single(&tab.document, expected_id);
                        }
                    }
                    self.checkpoint_wal_snapshot_active();
                    self.push_toast(format!("Converted to bitmap at {dpi:.0} DPI"), false);
                }
                Err(e) => self.push_toast(format!("Convert to Bitmap failed: {e}"), true),
            },
            Err(err) => self.push_toast(err, true),
        }
    }

    /// Appends a non-destructive [`RasterFilter`] to the selected raster object.
    pub fn apply_raster_filter_to_selection(&mut self, filter: RasterFilter) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let target = tab.selection.objects.iter().copied().find(|id| {
            tab.document
                .object(*id)
                .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
        });
        let Some(id) = target else {
            self.push_toast("Select a raster bitmap first", false);
            return;
        };
        let mut data = match &tab.document.objects()[&id].payload {
            NodePayload::Raster(r) => r.clone(),
            _ => return,
        };
        data.filters.push(filter);
        if !data.filter_enabled.is_empty() {
            data.filter_enabled.push(true);
        }
        match self.execute_command(CommandOp::UpdateRaster { id, data }) {
            Ok(()) => self.push_toast("Applied non-destructive bitmap filter", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Resamples the selected raster object's underlying bitmap to `(new_w, new_h)`.
    pub fn resample_selected_bitmap(&mut self, new_w: u32, new_h: u32) {
        let algo = self.bitmap_state.resample_algo;
        self.resample_selected_bitmap_with_algorithm(new_w, new_h, algo);
    }

    /// Resamples the selected raster object's underlying bitmap using `algorithm`.
    pub fn resample_selected_bitmap_with_algorithm(
        &mut self,
        new_w: u32,
        new_h: u32,
        algorithm: coree_render::ResampleAlgorithm,
    ) {
        let plan: Result<CommandOp, String> = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return;
            };
            let target = tab.selection.objects.iter().copied().find(|id| {
                tab.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
            });
            let Some(id) = target else {
                self.push_toast("Select a raster bitmap to Resample", false);
                return;
            };
            let mut data = match &tab.document.objects()[&id].payload {
                NodePayload::Raster(r) => r.clone(),
                _ => return,
            };
            let Some(orig) = tab.document.asset(data.asset_id) else {
                return;
            };
            match coree_render::resample_with_algorithm(
                orig,
                new_w.clamp(4, 8192),
                new_h.clamp(4, 8192),
                algorithm,
            ) {
                Ok(mut resized) => {
                    let new_asset_id = coree_core::ImageAssetId::new();
                    resized.id = new_asset_id;
                    data.asset_id = new_asset_id;
                    Ok(CommandOp::Batch(vec![
                        CommandOp::RegisterAsset { asset: resized },
                        CommandOp::UpdateRaster { id, data },
                    ]))
                }
                Err(e) => Err(format!("Resample failed: {e}")),
            }
        };
        match plan {
            Ok(cmd) => match self.execute_command(cmd) {
                Ok(()) => {
                    self.checkpoint_wal_snapshot_active();
                    self.push_toast(format!("Resampled bitmap to {new_w}x{new_h} px"), false);
                }
                Err(e) => self.push_toast(format!("Resample failed: {e}"), true),
            },
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Converts the selected raster bitmap between 8-bit (`Bit8`) and 16-bit (`Bit16`) channels.
    pub fn convert_selected_bitmap_depth(&mut self, depth: coree_core::PixelDepth) {
        let plan = self.active_tab().and_then(|tab| {
            let id = tab.selection.objects.iter().copied().find(|id| {
                tab.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
            })?;
            let NodePayload::Raster(mut data) = tab.document.object(id)?.payload.clone() else {
                return None;
            };
            let orig = tab.document.asset(data.asset_id)?;
            let mut converted = coree_render::convert_raster_depth(orig, depth);
            let new_id = coree_core::ImageAssetId::new();
            converted.id = new_id;
            data.asset_id = new_id;
            Some(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset: converted },
                CommandOp::UpdateRaster { id, data },
            ]))
        });
        let Some(cmd) = plan else {
            self.push_toast("Select a raster bitmap to convert bit depth", false);
            return;
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast(format!("Converted bitmap to {:?}", depth), false);
        }
    }

    /// Converts the selected raster bitmap's color mode (`Rgb` / `Cmyk`) through the active ICC pipeline.
    pub fn convert_selected_bitmap_color_mode(&mut self, mode: ColorMode) {
        let plan = self.active_tab().and_then(|tab| {
            let id = tab.selection.objects.iter().copied().find(|id| {
                tab.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
            })?;
            let NodePayload::Raster(mut data) = tab.document.object(id)?.payload.clone() else {
                return None;
            };
            let orig = tab.document.asset(data.asset_id)?;
            let icc = tab.document.icc_pipeline();
            let mut converted = coree_render::convert_raster_color_mode(orig, mode, Some(&icc));
            let new_id = coree_core::ImageAssetId::new();
            converted.id = new_id;
            data.asset_id = new_id;
            Some(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset: converted },
                CommandOp::UpdateRaster { id, data },
            ]))
        });
        let Some(cmd) = plan else {
            self.push_toast("Select a raster bitmap to convert color mode", false);
            return;
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast(format!("Converted bitmap to {mode:?}"), false);
        }
    }

    /// Computes a live histogram for the currently selected raster bitmap (resolving `Clone`,
    /// applying active `filters` and `crop_rect`, and caching by content signature).
    #[must_use]
    pub fn selected_bitmap_histogram(&self, bins: usize) -> Option<coree_render::RasterHistogram> {
        let tab = self.active_tab()?;
        let (id, data) = tab.selection.objects.iter().copied().find_map(|oid| {
            let obj = tab.document.object(oid)?;
            if let NodePayload::Raster(ref r) = obj.payload {
                return Some((oid, r.clone()));
            }
            tab.document
                .resolve_clone_payload_and_style_with_sym_scope(obj, None)
                .and_then(|(p, _, _, _)| match p {
                    NodePayload::Raster(r) => Some((oid, r)),
                    _ => None,
                })
        })?;
        let asset = tab.document.asset(data.asset_id)?;

        // Compute FNV-1a cache key covering object, asset, bins, filters, and crop rect
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(0x0100_0000_01b3);
        };
        mix(id.0);
        mix(data.asset_id.0.as_u128() as u64);
        mix((data.asset_id.0.as_u128() >> 64) as u64);
        mix(bins as u64);
        mix(data.filters.len() as u64);
        for b in format!("{:?}{:?}", data.filters, data.filter_enabled).as_bytes() {
            mix(u64::from(*b));
        }
        if let Some(cr) = data.crop_rect {
            mix(1);
            mix(cr.min_x.to_bits());
            mix(cr.min_y.to_bits());
            mix(cr.max_x.to_bits());
            mix(cr.max_y.to_bits());
        } else {
            mix(0);
        }
        if let Some((cached_key, ref cached_hist)) = *self.bitmap_state.histogram_cache.borrow() {
            if cached_key == h {
                return Some(cached_hist.clone());
            }
        }

        let filtered;
        let active_f = data.active_filters();
        let filtered_ref = if active_f.is_empty() {
            asset
        } else {
            filtered = coree_render::apply_raster_filters(asset, &active_f);
            &filtered
        };

        let hist = if let Some(cr) = data.crop_rect.filter(|c| c.is_finite()) {
            let w_mm = data.width_mm.max(1e-6);
            let h_mm = data.height_mm.max(1e-6);
            let aw = filtered_ref.width;
            let ah = filtered_ref.height;
            if aw > 0 && ah > 0 {
                let px_per_mm_x = f64::from(aw) / w_mm;
                let px_per_mm_y = f64::from(ah) / h_mm;
                let x0 = (cr.min_x.max(0.0) * px_per_mm_x)
                    .floor()
                    .clamp(0.0, (aw - 1) as f64) as u32;
                let y0 = (cr.min_y.max(0.0) * px_per_mm_y)
                    .floor()
                    .clamp(0.0, (ah - 1) as f64) as u32;
                let x1 = (cr.max_x.min(w_mm) * px_per_mm_x)
                    .ceil()
                    .clamp((x0 + 1) as f64, aw as f64) as u32;
                let y1 = (cr.max_y.min(h_mm) * px_per_mm_y)
                    .ceil()
                    .clamp((y0 + 1) as f64, ah as f64) as u32;
                let cw = (x1 - x0).max(1);
                let ch = (y1 - y0).max(1);
                let rgba = filtered_ref.to_rgba8_vec();
                let mut cropped_bytes = Vec::with_capacity((cw * ch * 4) as usize);
                for row in y0..y1 {
                    let s = ((row * aw + x0) * 4) as usize;
                    let e = ((row * aw + x1) * 4) as usize;
                    cropped_bytes.extend_from_slice(&rgba[s..e]);
                }
                if let Ok(cropped) = RasterAsset::new(
                    cw,
                    ch,
                    cropped_bytes,
                    filtered_ref.source_dpi,
                    filtered_ref.color_mode,
                ) {
                    coree_render::compute_histogram(&cropped, bins)
                } else {
                    coree_render::compute_histogram(filtered_ref, bins)
                }
            } else {
                coree_render::compute_histogram(filtered_ref, bins)
            }
        } else {
            coree_render::compute_histogram(filtered_ref, bins)
        };
        *self.bitmap_state.histogram_cache.borrow_mut() = Some((h, hist.clone()));
        Some(hist)
    }

    /// Runs Cutout Lab background removal on the selected raster bitmap using the active
    /// selection mask (or automatic corner-seeded foreground extraction when no mask is active).
    pub fn run_cutout_lab_active(&mut self) {
        let feather_px = self.bitmap_state.cutout_feather_px.max(0.0);
        let bg_tol = self.bitmap_state.cutout_bg_threshold.max(1.0);
        let default_feather = self.tool_settings.mask_feather_px;
        let default_inverted = self.tool_settings.mask_inverted;
        let plan: Result<CommandOp, String> = (|| {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let oid = tab
                .selection
                .objects
                .iter()
                .copied()
                .find(|id| {
                    tab.document
                        .object(*id)
                        .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
                })
                .ok_or_else(|| "Select a raster bitmap for Cutout Lab".to_owned())?;
            let obj = tab
                .document
                .object(oid)
                .ok_or_else(|| "raster object missing".to_owned())?;
            let NodePayload::Raster(ref r) = obj.payload else {
                return Err("object is not a raster".to_owned());
            };
            let orig = tab
                .document
                .asset(r.asset_id)
                .ok_or_else(|| "raster asset missing".to_owned())?;
            if orig.width == 0 || orig.height == 0 {
                return Err("raster asset is empty".to_owned());
            }
            let mut new_asset = orig.clone();
            new_asset.id = ImageAssetId::new();
            let mut updated_data = r.clone();
            updated_data.asset_id = new_asset.id;

            let aw = f64::from(new_asset.width);
            let ah = f64::from(new_asset.height);
            let w_mm = r.width_mm.max(1e-3);
            let h_mm = r.height_mm.max(1e-3);
            let inv_xf = world_inverse_transform_of(&tab.document, oid);
            let inv_persp = obj.perspective.and_then(|p| p.inverse());
            let to_px = |p: Point2D| {
                let mut lp = inv_xf.transform_point(p);
                if let Some(ref ih) = inv_persp {
                    lp = ih.transform_point_or(lp);
                }
                Point2D::new((lp.x / w_mm) * aw, (lp.y / h_mm) * ah)
            };

            let fg_mask = Self::build_raster_mask_for_asset(
                &new_asset,
                tab.raster_mask.as_ref(),
                tab.raster_mask_rect_mm,
                &to_px,
                default_feather,
                default_inverted,
            )
            .unwrap_or_else(|| {
                let mut bg = RasterMask::from_magic_wand(&new_asset, 0, 0, bg_tol, true);
                bg.invert();
                bg
            });
            cutout_lab(&mut new_asset, &fg_mask, feather_px);
            Ok(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset: new_asset },
                CommandOp::UpdateRaster {
                    id: oid,
                    data: updated_data,
                },
            ]))
        })();

        match plan {
            Ok(cmd) => match self.execute_command(cmd) {
                Ok(()) => {
                    self.checkpoint_wal_snapshot_active();
                    self.push_toast("Applied Cutout Lab alpha matting", false);
                }
                Err(e) => self.push_toast(format!("Cutout Lab failed: {e}"), true),
            },
            Err(err) => self.push_toast(err, true),
        }
    }

    /// Scans system fonts into SQLite `font_index` and refreshes the Font Manager list.
    pub fn refresh_font_manager(&mut self, rescan: bool) {
        let Some(ref db) = self.storage else {
            return;
        };
        if rescan || !self.font_manager_state.scanned_once {
            if let Ok(rep) = db.scan_system_fonts() {
                self.font_manager_state.quarantined_files = rep.quarantined_files;
            }
            self.font_manager_state.scanned_once = true;
        }
        let query = FontQuery {
            family_query: (!self.font_manager_state.search_query.trim().is_empty())
                .then(|| self.font_manager_state.search_query.trim().to_owned()),
            arabic_only: self.font_manager_state.arabic_only,
            kashida_only: self.font_manager_state.kashida_only,
            favorites_only: self.font_manager_state.favorites_only,
            collection_tag: self.font_manager_state.active_collection_tag.clone(),
            limit: Some(200),
        };
        if let Ok(fonts) = db.search_fonts(&query) {
            self.font_manager_state.indexed_fonts = fonts;
        }
        if let Ok(subs) = db.list_font_substitutions() {
            self.font_manager_state.substitutions = subs;
        }
    }

    /// Toggles the favorite flag on an indexed font face in SQLite and refreshes the list.
    pub fn toggle_font_favorite(&mut self, font_id: &str, favorite: bool) -> bool {
        let updated = self
            .storage
            .as_ref()
            .and_then(|db| db.set_font_favorite(font_id, favorite).ok())
            .unwrap_or(false);
        if updated {
            self.refresh_font_manager(false);
        }
        updated
    }

    /// Finds the closest PANOSE perceptual substitute in `font_index` and caches it in `FontManagerState`.
    pub fn find_best_panose_substitute(
        &mut self,
        target_panose: &PanoseClassification,
        target_weight: u16,
        target_italic: bool,
        require_arabic: bool,
    ) -> Option<(FontRecord, u32)> {
        let best = self
            .storage
            .as_ref()
            .and_then(|db| {
                db.find_best_panose_substitute(
                    target_panose,
                    target_weight,
                    target_italic,
                    require_arabic,
                )
                .ok()
            })
            .flatten();
        if let Some((ref rec, _)) = best {
            self.font_manager_state.substitute_family_input = rec.family_name.clone();
        }
        self.font_manager_state.panose_suggestion = best.clone();
        best
    }

    /// Persists a missing-font substitution rule in SQLite `font_substitutions` and refreshes state.
    pub fn set_font_substitution(
        &mut self,
        missing_family: &str,
        substitute_family: &str,
        panose: Option<&PanoseClassification>,
    ) -> Result<(), String> {
        let Some(ref db) = self.storage else {
            return Err("No SQLite storage database open".into());
        };
        db.set_font_substitution(missing_family, substitute_family, panose)
            .map_err(|e| e.to_string())?;
        self.refresh_font_manager(false);
        self.push_toast(
            format!(
                "Saved font substitution '{}' -> '{}'",
                missing_family.trim(),
                substitute_family.trim()
            ),
            false,
        );
        Ok(())
    }

    /// Lists all persisted font substitution rules from SQLite `font_substitutions`.
    #[must_use]
    pub fn list_font_substitutions(&self) -> Vec<FontSubstitutionRecord> {
        self.storage
            .as_ref()
            .and_then(|db| db.list_font_substitutions().ok())
            .unwrap_or_default()
    }

    /// Deletes a persisted font substitution rule and refreshes `FontManagerState`.
    pub fn remove_font_substitution(&mut self, missing_family: &str) -> bool {
        let removed = self
            .storage
            .as_ref()
            .and_then(|db| db.remove_font_substitution(missing_family).ok())
            .unwrap_or(false);
        if removed {
            self.refresh_font_manager(false);
        }
        removed
    }

    /// Replaces `missing_family` (case-insensitive) with `substitute_family` across text objects
    /// in the active document via undoable, WAL-journaled `CommandOp::EditText` operations.
    pub fn apply_font_substitution_to_active_document(
        &mut self,
        missing_family: &str,
        substitute_family: &str,
    ) -> usize {
        let missing = missing_family.trim();
        let substitute = substitute_family.trim();
        if missing.is_empty() || substitute.is_empty() {
            return 0;
        }
        let ops: Vec<CommandOp> = self
            .active_tab()
            .map(|tab| {
                let mut collected = Vec::new();
                for (&oid, obj) in tab.document.objects() {
                    let mut payload = obj.payload.clone();
                    let mut changed = false;
                    match &mut payload {
                        NodePayload::ArtisticText(t) => {
                            for run in &mut t.runs {
                                if run.style.font_family.trim().eq_ignore_ascii_case(missing) {
                                    run.style.font_family = substitute.to_owned();
                                    changed = true;
                                }
                            }
                        }
                        NodePayload::ParagraphText(p) => {
                            for run in &mut p.runs {
                                if run.style.font_family.trim().eq_ignore_ascii_case(missing) {
                                    run.style.font_family = substitute.to_owned();
                                    changed = true;
                                }
                            }
                        }
                        NodePayload::TextOnPath(top) => {
                            for run in &mut top.text.runs {
                                if run.style.font_family.trim().eq_ignore_ascii_case(missing) {
                                    run.style.font_family = substitute.to_owned();
                                    changed = true;
                                }
                            }
                        }
                        _ => {}
                    }
                    if changed {
                        collected.push(CommandOp::EditText { id: oid, payload });
                    }
                }
                collected
            })
            .unwrap_or_default();
        let count = ops.len();
        if count == 1 {
            if let Some(op) = ops.into_iter().next() {
                let _ = self.execute_command(op);
            }
        } else if count > 1 {
            let _ = self.execute_command(CommandOp::Batch(ops));
        }
        if count > 0 {
            self.push_toast(
                format!("Substituted '{missing}' -> '{substitute}' in {count} text object(s)"),
                false,
            );
        }
        count
    }

    /// Inserts `glyph` into the active inline text editor, the selected text object, or a new Artistic Text object.
    pub fn insert_glyph_at_selection(&mut self, glyph: &str) {
        if glyph.is_empty() {
            return;
        }
        // 1. Active inline on-canvas text edit buffer
        if let Some(tab) = self.active_tab_mut() {
            if let Some(ref mut edit) = tab.inline_text_edit {
                edit.buffer.push_str(glyph);
                return;
            }
        }
        // 2. Append into selected ArtisticText / ParagraphText / TextOnPath
        let edit_op = self.active_tab().and_then(|tab| {
            let oid = tab.selection.objects.iter().copied().find(|id| {
                tab.document.object(*id).is_some_and(|o| {
                    matches!(
                        o.payload,
                        NodePayload::ArtisticText(_)
                            | NodePayload::ParagraphText(_)
                            | NodePayload::TextOnPath(_)
                    )
                })
            })?;
            let obj = tab.document.object(oid)?;
            let mut payload = obj.payload.clone();
            match &mut payload {
                NodePayload::ArtisticText(art) => {
                    if let Some(last) = art.runs.last_mut() {
                        last.text.push_str(glyph);
                    } else {
                        art.runs.push(coree_core::TextRun::new(
                            glyph.to_owned(),
                            self.default_text_style.clone(),
                        ));
                    }
                }
                NodePayload::ParagraphText(para) => {
                    if let Some(last) = para.runs.last_mut() {
                        last.text.push_str(glyph);
                    } else {
                        para.runs.push(coree_core::TextRun::new(
                            glyph.to_owned(),
                            self.default_text_style.clone(),
                        ));
                    }
                }
                NodePayload::TextOnPath(top) => {
                    if let Some(last) = top.text.runs.last_mut() {
                        last.text.push_str(glyph);
                    } else {
                        top.text.runs.push(coree_core::TextRun::new(
                            glyph.to_owned(),
                            self.default_text_style.clone(),
                        ));
                    }
                }
                _ => return None,
            }
            Some(CommandOp::EditText { id: oid, payload })
        });
        if let Some(op) = edit_op {
            match self.execute_command(op) {
                Ok(()) => self.push_toast(format!("Inserted glyph '{glyph}'"), false),
                Err(e) => self.push_toast(e, true),
            }
            return;
        }
        // 3. Otherwise create a new ArtisticText object at page center
        let Some(_layer_id) = self.ensure_active_creation_layer() else {
            return;
        };
        let pos = self
            .active_tab()
            .and_then(|t| t.document.active_page())
            .map_or(Point2D::new(80.0, 80.0), |p| p.trim_rect().center());
        let art = ArtisticText::from_plain(glyph.to_owned(), self.default_text_style.clone());
        let obj = SceneObject::new(ObjectId(0), "Glyph", NodePayload::ArtisticText(art))
            .with_fill(Fill::Solid(self.default_text_style.fill.clone()))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(pos.x, pos.y));
        if self.create_object_on_active_layer(obj).is_some() {
            self.push_toast(format!("Inserted glyph '{glyph}'"), false);
        }
    }
}
