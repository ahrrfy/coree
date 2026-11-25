// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Returns a selected or hovered editable `NodePayload::Raster` object, or automatically
    /// creates a clean 150 DPI transparent page raster layer on an unlocked visible layer via
    /// `CommandOp::create_object` so raster tools NEVER silently no-op or paint locked/hidden layers!
    pub fn ensure_raster_target_at(&mut self, world_pt: Point2D) -> Option<ObjectId> {
        let existing = self.active_tab().and_then(|tab| {
            tab.selection
                .objects
                .iter()
                .copied()
                .find(|&id| {
                    is_object_editable_in_doc(&tab.document, id)
                        && tab
                            .document
                            .object(id)
                            .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
                })
                .or_else(|| {
                    hit_test_point(
                        &tab.document,
                        tab.document.active_page_index(),
                        world_pt,
                        4.0,
                    )
                    .filter(|&id| {
                        is_object_editable_in_doc(&tab.document, id)
                            && tab
                                .document
                                .object(id)
                                .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
                    })
                })
                .or_else(|| {
                    tab.document.active_page().and_then(|p| {
                        p.layers
                            .iter()
                            .rev()
                            .filter(|l| l.visible && !l.locked)
                            .find_map(|l| {
                                l.objects.iter().rev().copied().find(|&id| {
                                    is_object_editable_in_doc(&tab.document, id)
                                        && tab.document.object(id).is_some_and(|o| {
                                            matches!(o.payload, NodePayload::Raster(_))
                                        })
                                })
                            })
                    })
                })
        });
        if existing.is_some() {
            return existing;
        }

        // Auto-create a clean 150 DPI transparent page raster layer on an unlocked visible layer
        // (bounded <= 1600px per axis for instant response).
        let Some((page_w, page_h, color_mode, layer_id)) = self.active_tab().and_then(|tab| {
            let (w, h) = tab.document.active_page().map_or((210.0, 297.0), |p| {
                (p.width_mm.max(10.0), p.height_mm.max(10.0))
            });
            let target_layer = tab
                .active_layer_id
                .filter(|lid| {
                    tab.document
                        .find_layer(*lid)
                        .is_some_and(|l| l.visible && !l.locked)
                })
                .or_else(|| {
                    tab.document.active_page().and_then(|p| {
                        p.layers
                            .iter()
                            .rev()
                            .find(|l| l.visible && !l.locked)
                            .map(|l| l.id)
                    })
                })?;
            Some((w, h, tab.document.color_mode(), Some(target_layer)))
        }) else {
            self.push_toast(
                crate::i18n::I18nKey::ErrorLayerLocked.tr_ui(self.ui_language),
                true,
            );
            return None;
        };
        let dpi = 150.0_f64;
        let raw_w = (page_w / 25.4 * dpi).round().max(16.0);
        let raw_h = (page_h / 25.4 * dpi).round().max(16.0);
        let cap_scale = (1600.0 / raw_w.max(raw_h)).min(1.0);
        let px_w = ((raw_w * cap_scale).round() as u32).clamp(16, 1600);
        let px_h = ((raw_h * cap_scale).round() as u32).clamp(16, 1600);
        let rgba = vec![0u8; (px_w as usize) * (px_h as usize) * 4];
        let asset = RasterAsset::new(px_w, px_h, rgba, dpi as f32, color_mode).ok()?;
        let asset_id = asset.id;

        let raster_obj = SceneObject::new(
            ObjectId(0),
            "Bitmap Layer (150 DPI)",
            NodePayload::Raster(RasterLayerData {
                asset_id,
                width_mm: page_w,
                height_mm: page_h,
                filters: Vec::new(),
                filter_enabled: Vec::new(),
                crop_rect: None,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());

        if self
            .execute_command(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset },
                CommandOp::create_object(raster_obj, layer_id),
            ]))
            .is_ok()
        {
            let created_id = self
                .active_tab()
                .and_then(|t| t.document.objects().keys().last().copied());
            if let (Some(tab), Some(id)) = (self.active_tab_mut(), created_id) {
                tab.selection.select_single(&tab.document, id);
            }
            self.push_toast(
                format!("Created 150 DPI transparent raster layer ({px_w}×{px_h} px)"),
                false,
            );
            created_id
        } else {
            None
        }
    }

    /// Executes any of the 8 raster brush/fill operations (`Paint`, `Eraser`, `CloneStamp`,
    /// `Smudge`, `Healing`, `Dodge`, `Burn`, `FloodFill`) along `points` in world mm.
    ///
    /// Uses copy-on-write cloning into a fresh [`ImageAssetId`] committed via
    /// [`CommandOp::UpdateRaster`] so `Ctrl+Z` / `Ctrl+Shift+Z` undo and redo every
    /// raster stroke cleanly, and safely rejects degenerate `0x0` assets.
    pub fn apply_raster_tool_stroke(&mut self, tool: ToolId, points: &[Point2D]) -> bool {
        self.apply_raster_tool_stroke_with_stylus(tool, points, &[])
    }

    /// Applies a pressure-, tilt-, and twist-sensitive raster brush stroke (`IMP-03`).
    pub fn apply_raster_tool_stroke_with_stylus(
        &mut self,
        tool: ToolId,
        points: &[Point2D],
        samples: &[StylusSample],
    ) -> bool {
        if points.is_empty() {
            return false;
        }
        let Some(oid) = self.ensure_raster_target_at(points[0]) else {
            return false;
        };

        let eraser_tip = samples.iter().any(|s| s.is_eraser);
        let op = if eraser_tip
            && matches!(
                tool,
                ToolId::BitmapBrush | ToolId::CloneStamp | ToolId::Smudge
            ) {
            RasterBrushOp::Eraser
        } else {
            match tool {
                ToolId::BitmapEraser => RasterBrushOp::Eraser,
                ToolId::CloneStamp => RasterBrushOp::CloneStamp,
                ToolId::Smudge => RasterBrushOp::Smudge,
                _ => self.tool_settings.raster_op,
            }
        };
        let r_px = self.tool_settings.brush_radius_px;
        let hard = self.tool_settings.brush_hardness;
        let col = self.tool_settings.brush_color.clone();
        let smudge_str = self.tool_settings.smudge_strength;
        let db_tone = self.tool_settings.dodge_burn_tone;
        let db_exp = self.tool_settings.dodge_burn_exposure;
        let flood_tol = self.tool_settings.flood_tolerance;
        let clone_src_world = self.tool_settings.clone_source_mm;
        let curve = PressureCurve::from_json(&self.tool_settings.pressure_curve_json);

        let default_feather = self.tool_settings.mask_feather_px;
        let default_inverted = self.tool_settings.mask_inverted;
        let prepared = self.active_tab().and_then(|tab| {
            if !is_object_editable_in_doc(&tab.document, oid) {
                return None;
            }
            let obj = tab.document.object(oid)?;
            let NodePayload::Raster(ref r) = obj.payload else {
                return None;
            };
            let orig_asset = tab.document.asset(r.asset_id)?;
            if orig_asset.width == 0 || orig_asset.height == 0 {
                return None;
            }
            let mut cloned_asset = orig_asset.clone();
            cloned_asset.id = ImageAssetId::new();
            let mut updated_data = r.clone();
            updated_data.asset_id = cloned_asset.id;
            Some((
                cloned_asset,
                updated_data,
                r.width_mm.max(1e-3),
                r.height_mm.max(1e-3),
                world_inverse_transform_of(&tab.document, oid),
                obj.perspective.and_then(|p| p.inverse()),
                tab.raster_mask.clone(),
                tab.raster_mask_rect_mm,
            ))
        });
        let Some((
            mut new_asset,
            updated_data,
            w_mm,
            h_mm,
            inv_xf,
            inv_persp,
            mask_sel,
            mask_rect_mm,
        )) = prepared
        else {
            return false;
        };

        let aw = f64::from(new_asset.width);
        let ah = f64::from(new_asset.height);
        let to_px = |p: Point2D| {
            let mut lp = inv_xf.transform_point(p);
            if let Some(ref ih) = inv_persp {
                lp = ih.transform_point_or(lp);
            }
            Point2D::new((lp.x / w_mm) * aw, (lp.y / h_mm) * ah)
        };
        let px_pts: Vec<Point2D> = points.iter().copied().map(to_px).collect();
        let stylus_pts: Vec<StylusPoint> = px_pts
            .iter()
            .enumerate()
            .map(|(i, &pos)| {
                samples.get(i).copied().map_or_else(
                    || StylusPoint::uniform(pos),
                    |s| StylusPoint::from_sample(pos, s, &curve),
                )
            })
            .collect();
        let src_origin = clone_src_world
            .map(to_px)
            .or_else(|| {
                px_pts
                    .first()
                    .map(|p| Point2D::new((p.x - 16.0).max(0.0), (p.y - 16.0).max(0.0)))
            })
            .unwrap_or(Point2D::ZERO);

        let active_mask = Self::build_raster_mask_for_asset(
            &new_asset,
            mask_sel.as_ref(),
            mask_rect_mm,
            &to_px,
            default_feather,
            default_inverted,
        );

        match op {
            RasterBrushOp::Paint => {
                paint_brush_stroke_stylus(
                    &mut new_asset,
                    &stylus_pts,
                    r_px,
                    hard,
                    col,
                    1.0,
                    active_mask.as_ref(),
                );
            }
            RasterBrushOp::Eraser => {
                erase_stroke_stylus_masked(
                    &mut new_asset,
                    &stylus_pts,
                    r_px,
                    hard,
                    active_mask.as_ref(),
                );
            }
            RasterBrushOp::CloneStamp => {
                clone_stamp_stroke_stylus_masked(
                    &mut new_asset,
                    src_origin,
                    &stylus_pts,
                    r_px,
                    hard,
                    active_mask.as_ref(),
                );
            }
            RasterBrushOp::Smudge => {
                smudge_stroke_stylus_masked(
                    &mut new_asset,
                    &stylus_pts,
                    r_px,
                    smudge_str,
                    active_mask.as_ref(),
                );
            }
            RasterBrushOp::Healing => {
                apply_healing_stroke_stylus_masked(
                    &mut new_asset,
                    src_origin,
                    &stylus_pts,
                    r_px,
                    hard,
                    active_mask.as_ref(),
                );
            }
            RasterBrushOp::Dodge => {
                apply_dodge_burn_stroke_stylus_masked(
                    &mut new_asset,
                    &stylus_pts,
                    r_px,
                    hard,
                    DodgeBurnMode::Dodge,
                    db_tone,
                    db_exp,
                    active_mask.as_ref(),
                );
            }
            RasterBrushOp::Burn => {
                apply_dodge_burn_stroke_stylus_masked(
                    &mut new_asset,
                    &stylus_pts,
                    r_px,
                    hard,
                    DodgeBurnMode::Burn,
                    db_tone,
                    db_exp,
                    active_mask.as_ref(),
                );
            }
            RasterBrushOp::FloodFill => {
                if let Some(seed) = px_pts.first() {
                    let max_x = i64::from(new_asset.width).saturating_sub(1).max(0);
                    let max_y = i64::from(new_asset.height).saturating_sub(1).max(0);
                    let sx = (seed.x.round() as i64).clamp(0, max_x) as u32;
                    let sy = (seed.y.round() as i64).clamp(0, max_y) as u32;
                    let _ = raster_flood_fill_masked(
                        &mut new_asset,
                        sx,
                        sy,
                        col,
                        flood_tol,
                        active_mask.as_ref(),
                    );
                }
            }
        }

        if self
            .execute_command(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset: new_asset },
                CommandOp::UpdateRaster {
                    id: oid,
                    data: updated_data,
                },
            ]))
            .is_ok()
        {
            self.checkpoint_wal_snapshot_active();
            true
        } else {
            false
        }
    }

    pub(in crate::state) fn build_raster_mask_for_asset(
        asset: &RasterAsset,
        mask_sel: Option<&RasterMaskSelection>,
        mask_rect_mm: Option<Rect2D>,
        to_px: &dyn Fn(Point2D) -> Point2D,
        default_feather_px: f32,
        default_inverted: bool,
    ) -> Option<RasterMask> {
        let mut built = if let Some(sel) = mask_sel {
            let mut m = match &sel.shape {
                RasterMaskShape::Rectangle(mr) => {
                    let quad = [
                        to_px(Point2D::new(mr.min_x, mr.min_y)),
                        to_px(Point2D::new(mr.max_x, mr.min_y)),
                        to_px(Point2D::new(mr.max_x, mr.max_y)),
                        to_px(Point2D::new(mr.min_x, mr.max_y)),
                    ];
                    RasterMask::from_lasso(asset.width, asset.height, &quad)
                }
                RasterMaskShape::Ellipse(mr) => {
                    let c = mr.center();
                    let rx = mr.width() * 0.5;
                    let ry = mr.height() * 0.5;
                    let poly: Vec<Point2D> = (0..48)
                        .map(|i| {
                            let theta = (f64::from(i) / 48.0) * std::f64::consts::TAU;
                            to_px(Point2D::new(c.x + rx * theta.cos(), c.y + ry * theta.sin()))
                        })
                        .collect();
                    RasterMask::from_lasso(asset.width, asset.height, &poly)
                }
                RasterMaskShape::Lasso(pts) => {
                    let poly: Vec<Point2D> = pts.iter().copied().map(to_px).collect();
                    RasterMask::from_lasso(asset.width, asset.height, &poly)
                }
                RasterMaskShape::MagicWand {
                    seed_mm,
                    tolerance,
                    contiguous,
                } => {
                    let seed = to_px(*seed_mm);
                    let max_x = i64::from(asset.width).saturating_sub(1).max(0);
                    let max_y = i64::from(asset.height).saturating_sub(1).max(0);
                    let sx = (seed.x.round() as i64).clamp(0, max_x) as u32;
                    let sy = (seed.y.round() as i64).clamp(0, max_y) as u32;
                    RasterMask::from_magic_wand(asset, sx, sy, *tolerance, *contiguous)
                }
            };
            let feather = if sel.feather_px > 0.0 {
                sel.feather_px
            } else {
                default_feather_px.max(0.0)
            };
            if feather > 0.0 {
                m.feather(feather);
            }
            if sel.inverted {
                m.invert();
            }
            Some(m)
        } else if let Some(mr) = mask_rect_mm {
            let quad = [
                to_px(Point2D::new(mr.min_x, mr.min_y)),
                to_px(Point2D::new(mr.max_x, mr.min_y)),
                to_px(Point2D::new(mr.max_x, mr.max_y)),
                to_px(Point2D::new(mr.min_x, mr.max_y)),
            ];
            let mut m = RasterMask::from_lasso(asset.width, asset.height, &quad);
            if default_feather_px > 0.0 {
                m.feather(default_feather_px);
            }
            if default_inverted {
                m.invert();
            }
            Some(m)
        } else {
            None
        };
        if let Some(ref mut m) = built {
            if mask_sel.is_some() && default_inverted && !mask_sel.is_some_and(|s| s.inverted) {
                m.invert();
            }
        }
        built
    }

    /// Creates an ArtisticText object at `pos_mm`.
    pub fn insert_artistic_text_at(&mut self, pos_mm: Point2D) {
        let text = ArtisticText::from_plain(
            self.tool_settings.text_insert_draft.clone(),
            self.default_text_style.clone(),
        );
        let obj = SceneObject::new(
            ObjectId(0),
            "Artistic Text",
            NodePayload::ArtisticText(text),
        )
        .with_fill(Fill::Solid(self.default_text_style.fill.clone()))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(pos_mm.x, pos_mm.y));
        let _ = self.create_object_on_active_layer(obj);
    }

    /// Computes the object-local bounding box of `id` (before `obj.transform`), matching
    /// `coree-render`'s local coordinate space for `MeshFill`, `Distort`, `Envelope`, and `Perspective`.
    #[must_use]
    pub fn object_local_bounds(doc: &Document, id: ObjectId) -> Option<Rect2D> {
        doc.local_object_bounds(id)
    }

    /// Object-local bounding box of the primary selected object on the active tab.
    #[must_use]
    pub fn selected_local_bounds(&self) -> Option<Rect2D> {
        let tab = self.active_tab()?;
        let id = *tab.selection.objects.first()?;
        Self::object_local_bounds(&tab.document, id)
    }

    /// Helper to build a default `MeshFill` grid in **object-local** coordinates matching
    /// `coree-tools` (`rows` x `cols` node grid, clamped `2..=32`, with `1/3`-chord neighbor handles).
    #[must_use]
    pub fn build_mesh_fill_for_bounds(local_bounds: Rect2D, rows: usize, cols: usize) -> Fill {
        coree_tools::build_mesh_fill_for_bounds(local_bounds, rows, cols)
    }

    /// Adds a document guide line via undoable [`CommandOp::AddGuide`].
    pub fn add_guide(&mut self, orientation: GuideOrientation) {
        let _ = self.execute_command(CommandOp::AddGuide {
            orientation,
            locked: false,
        });
    }

    /// Builds the current effect from [`EffectsDockerState`].
    /// Geometry-deforming effects (`Distort`, `Envelope`, `Perspective`) use **object-local**
    /// coordinates matching `coree_render::apply_geometry_effects`, and `Envelope` emits a full
    /// 8-point `[TL, T_mid, TR, R_mid, BR, B_mid, BL, L_mid]` Coons cage.
    #[must_use]
    pub fn build_selected_effect(&self) -> Effect {
        let es = &self.effects_state;
        let local_bb = self
            .selected_local_bounds()
            .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
        match es.selected_effect_kind {
            0 => Effect::DropShadow {
                dx_mm: es.dx_mm,
                dy_mm: es.dy_mm,
                blur_mm: es.blur_mm.max(0.0),
                opacity: es.opacity.clamp(0.0, 1.0),
                color: es.color.clone(),
                feather_direction: FeatherDirection::Middle,
            },
            1 => Effect::BlockShadow {
                dx_mm: es.dx_mm,
                dy_mm: es.dy_mm,
                color: es.color.clone(),
                simplify: es.block_shadow_simplify,
                overprint: false,
            },
            2 => Effect::Contour {
                mode: es.contour_mode,
                steps: es.steps.clamp(1, 512),
                offset_mm: es.offset_mm.max(0.2),
                fill_color: es.color.clone(),
                outline_color: Color::BLACK,
            },
            3 => {
                let target_id = self
                    .active_tab()
                    .and_then(|t| t.selection.objects.get(1).copied())
                    .unwrap_or(ObjectId(1));
                Effect::Blend {
                    target_id,
                    steps: es.steps.clamp(1, 200),
                    rotation_deg: 0.0,
                    along_path: None,
                    rotate_along_path: es.rotate_along_path,
                }
            }
            4 => Effect::Distort {
                kind: es.distort_kind,
                amplitude: es.amplitude,
                frequency: es.frequency.max(0.2),
                center: local_bb.center(),
            },
            5 => {
                let w = local_bb.width();
                let h = local_bb.height();
                let x0 = local_bb.min_x;
                let x1 = x0 + w / 3.0;
                let x2 = x0 + 2.0 * w / 3.0;
                let x3 = local_bb.max_x;
                let y0 = local_bb.min_y;
                let y1 = y0 + h / 3.0;
                let y2 = y0 + 2.0 * h / 3.0;
                let y3 = local_bb.max_y;
                let bulge_y = (h * 0.15).clamp(2.0, 12.0);
                // 12-point Coons cubic cage (3 cubic points per edge)
                Effect::Envelope {
                    mode: es.envelope_mode,
                    cage: vec![
                        Point2D::new(x0, y0),
                        Point2D::new(x1, y0 - bulge_y),
                        Point2D::new(x2, y0 - bulge_y),
                        Point2D::new(x3, y0),
                        Point2D::new(x3, y1),
                        Point2D::new(x3, y2),
                        Point2D::new(x3, y3),
                        Point2D::new(x2, y3 + bulge_y),
                        Point2D::new(x1, y3 + bulge_y),
                        Point2D::new(x0, y3),
                        Point2D::new(x0, y2),
                        Point2D::new(x0, y1),
                    ],
                }
            }
            6 => Effect::Extrude25D {
                vanishing_point: Point2D::new(140.0, 40.0),
                depth: es.extrude_depth.max(1.0),
                bevel_depth: 1.0,
                shade_color: es.color.clone(),
                light_dir: [0.4, -0.5, 0.75],
            },
            7 => {
                let c = local_bb.center();
                let sz = local_bb.width().min(local_bb.height()).max(10.0);
                let mode = match es.transp_kind {
                    1 => TransparencyMode::FountainLinear {
                        start: Point2D::new(local_bb.min_x, c.y),
                        end: Point2D::new(local_bb.max_x, c.y),
                        start_alpha: es.opacity.clamp(0.0, 1.0),
                        end_alpha: 0.1,
                    },
                    2 => TransparencyMode::Pattern {
                        pattern_name: "checker".to_owned(),
                        tile_w: 8.0,
                        tile_h: 8.0,
                        fg_opacity: es.opacity.clamp(0.0, 1.0),
                        bg_opacity: 0.1,
                    },
                    3 => TransparencyMode::Texture {
                        preset: "marble".to_owned(),
                        scale: 6.0,
                        fg_opacity: es.opacity.clamp(0.0, 1.0),
                        bg_opacity: 0.1,
                    },
                    4 => TransparencyMode::Conical {
                        center: c,
                        angle_deg: 0.0,
                        start_opacity: es.opacity.clamp(0.0, 1.0),
                        end_opacity: 0.1,
                    },
                    5 => TransparencyMode::Square {
                        center: c,
                        size: sz,
                        angle_deg: 0.0,
                        start_opacity: es.opacity.clamp(0.0, 1.0),
                        end_opacity: 0.1,
                    },
                    _ => TransparencyMode::Uniform(es.opacity.clamp(0.0, 1.0)),
                };
                Effect::Transparency {
                    mode,
                    blend_mode: es.blend_mode,
                }
            }
            8 => Effect::Perspective {
                quad: [
                    Point2D::new(local_bb.min_x + local_bb.width() * 0.12, local_bb.min_y),
                    Point2D::new(local_bb.max_x - local_bb.width() * 0.12, local_bb.min_y),
                    local_bb.max(),
                    Point2D::new(local_bb.min_x, local_bb.max_y),
                ],
            },
            9 => Effect::Symmetry {
                axes: es.symmetry_axes.clamp(2, 24),
                mirror: es.symmetry_mirror,
                origin: self
                    .active_tab()
                    .and_then(|t| t.selection.bounding_box(&t.document))
                    .map_or(Point2D::new(105.0, 148.0), |b| b.center()),
            },
            10 => Effect::InnerShadow {
                dx_mm: es.dx_mm,
                dy_mm: es.dy_mm,
                blur_mm: es.blur_mm.max(0.0),
                opacity: es.opacity.clamp(0.0, 1.0),
                color: es.color.clone(),
                feather_direction: es.inner_shadow_feather,
            },
            11 => Effect::Bevel {
                style: es.bevel_style,
                distance_mm: es.bevel_distance_mm.max(0.2),
                softness_mm: es.bevel_softness_mm.max(0.0),
                light_angle_deg: es.bevel_light_angle_deg,
                altitude_deg: es.bevel_altitude_deg,
                highlight_color: es.bevel_highlight_color.clone(),
                shadow_color: es.bevel_shadow_color.clone(),
                intensity: es.bevel_intensity.clamp(0.0, 1.0),
            },
            _ => Effect::Extrude3D {
                vanishing_point: Point2D::new(140.0, 40.0),
                depth: es.extrude_depth.max(1.0),
                bevel_depth: es.bevel_distance_mm.max(0.0),
                shade_color: es.color.clone(),
                rotation_3d: es.extrude3d_rotation,
                lights: vec![coree_core::ExtrudeLight {
                    dir: es.extrude3d_light_dir,
                    intensity: es.extrude3d_light_intensity.clamp(0.0, 3.0),
                }],
            },
        }
    }

    /// Applies an artistic media stroke expansion onto a freehand curve.
    #[must_use]
    pub fn expand_artistic_stroke(&self, centerline: &BezierPath) -> BezierPath {
        coree_core::ArtisticMedia::expand_stroke(
            centerline,
            &ArtisticMediaStyle::Preset {
                max_width_mm: self.tool_settings.artistic_media_width_mm,
                profile: self.tool_settings.artistic_media_profile.clone(),
            },
        )
    }

    /// Returns the production CutContour spot palette.
    #[must_use]
    pub fn cut_production_palette() -> ColorPalette {
        let mut pal = ColorPalette::new("CutContour & Production", ColorMode::Cmyk);
        pal.swatches = vec![
            ColorSwatch::new("CutContour", Color::cut_contour_spot()),
            ColorSwatch::new(
                "PerfCutContour",
                Color::spot("PerfCutContour", 1.0, [1.0, 0.0, 0.0, 0.0]),
            ),
            ColorSwatch::new("Crease", Color::spot("Crease", 1.0, [0.0, 0.7, 1.0, 0.0])),
            ColorSwatch::new(
                "Spot White",
                Color::spot("SpotWhite", 1.0, [0.1, 0.0, 0.0, 0.0]),
            ),
            ColorSwatch::new(
                "Spot Varnish",
                Color::spot("SpotVarnish", 1.0, [0.0, 0.0, 1.0, 0.0]),
            ),
            ColorSwatch::new(
                "Foil Gold",
                Color::spot("FoilGold", 1.0, [0.0, 0.25, 0.85, 0.1]),
            ),
            ColorSwatch::new(
                "Registration",
                Color::spot("Registration", 1.0, [1.0, 1.0, 1.0, 1.0]),
            ),
        ];
        pal
    }

    /// Inspects the active document into its `inspection_report` (for the CDR Inspector docker).
    /// When `force` is true, or when the report is missing or the document is dirty, recomputes
    /// fresh structural metadata (preserving on-disk binary RIFF/ZIP details when clean).
    pub fn refresh_inspector_for_active(&mut self, force: bool) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if force || tab.inspection_report.is_none() || tab.dirty {
                if !tab.dirty {
                    if let Some(ref src_path) = tab.source_path {
                        if let Ok(rep) = inspect_any_file(src_path) {
                            tab.inspection_report = Some(rep);
                            return;
                        }
                    }
                }
                tab.inspection_report = Some(inspect_in_memory_document(&tab.document));
            }
        }
    }

    /// Adds a new layer on the active page.
    pub fn add_layer_active(&mut self, name: String, is_cut: bool, scope: MasterLayerScope) {
        let Some(page_id) = self
            .active_tab()
            .and_then(|t| t.document.active_page())
            .map(|p| p.id)
        else {
            return;
        };
        let _ = self.execute_command(CommandOp::AddLayer {
            page_id,
            name,
            is_cut_layer: is_cut,
            master_scope: scope,
        });
    }
}
