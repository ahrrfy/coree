// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Appends a node to the active Bézier/Pen draft (pulling symmetric `handle_in`/`handle_out` when `handle_out` is `Some`).
    pub fn bezier_add_point(&mut self, pos: Point2D, handle_out: Option<Point2D>) {
        if let Some(tab) = self.active_tab_mut() {
            tab.tool_core.bezier_add_point(pos, handle_out);
        }
    }

    /// Commits the drafted `bezier_draft_nodes` as a real cubic [`BezierPath`] (open or closed).
    pub fn bezier_finish(&mut self, closed: bool) -> Option<ObjectId> {
        if self
            .active_tab()
            .is_none_or(|t| t.tool_core.bezier_draft_nodes.len() < 2)
        {
            return None;
        }
        self.ensure_active_creation_layer()?;
        let nodes = {
            let tab = self.active_tab_mut()?;
            let nodes = std::mem::take(&mut tab.tool_core.bezier_draft_nodes);
            tab.tool_core.poly_points_mm.clear();
            nodes
        };
        let path = BezierPath::from_subpath(Subpath { nodes, closed });
        let fill = if closed {
            self.default_fill.clone()
        } else {
            Fill::None
        };
        let obj = SceneObject::new(
            ObjectId(0),
            self.active_tool.label(),
            NodePayload::Path(path),
        )
        .with_fill(fill)
        .with_outline(self.default_outline.clone());
        self.create_object_on_active_layer(obj)
    }

    /// Builds a smooth Catmull-Rom / B-spline cubic [`BezierPath`] through `pts` and commits it.
    pub fn commit_bspline_points(&mut self, pts: &[Point2D], closed: bool) -> Option<ObjectId> {
        if pts.len() < 2 {
            return None;
        }
        let path = bspline_to_bezier_path(pts, closed);
        let fill = if closed {
            self.default_fill.clone()
        } else {
            Fill::None
        };
        let obj = SceneObject::new(ObjectId(0), "B-Spline", NodePayload::Path(path))
            .with_fill(fill)
            .with_outline(self.default_outline.clone());
        self.create_object_on_active_layer(obj)
    }

    /// Commits a 3-Point Curve: chord `(p1, p2)` bent cleanly through control apex `p3`
    /// as an exact degree-elevated quadratic->cubic Bézier arc.
    pub fn commit_three_point_curve(
        &mut self,
        p1: Point2D,
        p2: Point2D,
        p3: Point2D,
    ) -> Option<ObjectId> {
        if p1.distance_to(p2) < 0.25 {
            return None;
        }
        let path = three_point_arc_path(p1, p2, p3);
        let obj = SceneObject::new(ObjectId(0), "3-Point Curve", NodePayload::Path(path))
            .with_fill(Fill::None)
            .with_outline(self.default_outline.clone());
        let created_id = self.create_object_on_active_layer(obj)?;
        if let Some(tab) = self.active_tab_mut() {
            tab.tool_core.three_point_curve_chord = None;
        }
        Some(created_id)
    }

    /// Commits a 3-Point Rectangle or 3-Point Ellipse from baseline `p1 -> p2` and height point `p3`.
    pub fn commit_three_point_shape(
        &mut self,
        tool: ToolId,
        p1: Point2D,
        p2: Point2D,
        p3: Point2D,
    ) -> Option<ObjectId> {
        let base = p2 - p1;
        let width = base.length();
        if width < 0.5 {
            return None;
        }
        let angle = base.y.atan2(base.x);
        let u = base / width;
        let perp = Vec2D::new(-u.y, u.x);
        let signed_h = (p3 - p1).dot(perp);
        let height = signed_h.abs().max(1.0);
        let ts = &self.tool_settings;

        let (name, payload, xf) = if tool == ToolId::ThreePointEllipse {
            let center = p1 + u * (width * 0.5) + perp * (signed_h * 0.5);
            (
                "3-Point Ellipse",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: width * 0.5,
                    ry: height * 0.5,
                    mode: ts.ellipse_mode,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
                Affine2D::translate(center.x, center.y).compose(&Affine2D::rotate(angle)),
            )
        } else {
            let origin = if signed_h >= 0.0 {
                p1
            } else {
                p1 + perp * signed_h
            };
            (
                "3-Point Rectangle",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width,
                    height,
                    corner_radii: [ts.rect_corner_radius_mm; 4],
                    corner_style: ts.rect_corner_style,
                    lock_corners: true,
                }),
                Affine2D::translate(origin.x, origin.y).compose(&Affine2D::rotate(angle)),
            )
        };
        let obj = SceneObject::new(ObjectId(0), name, payload)
            .with_fill(self.default_fill.clone())
            .with_outline(self.default_outline.clone())
            .with_transform(xf);
        let created_id = self.create_object_on_active_layer(obj)?;
        if let Some(tab) = self.active_tab_mut() {
            tab.tool_core.three_point_shape_chord = None;
        }
        Some(created_id)
    }

    /// Confirms an in-progress interactive tool action (`Enter` key): commits Crop box,
    /// open Bézier/Pen/B-Spline/Polyline path, or inline text edit.
    pub fn confirm_active_tool_action(&mut self) -> bool {
        if self
            .active_tab()
            .is_some_and(|t| t.inline_text_edit.is_some())
        {
            return self.commit_inline_text_edit();
        }
        if let Some(crop) = self.active_tab().and_then(|t| t.pending_crop_rect) {
            return self.apply_crop_rect(crop);
        }
        if self
            .active_tab()
            .is_some_and(|t| t.tool_core.bezier_draft_nodes.len() >= 2)
        {
            return self.bezier_finish(false).is_some();
        }
        if self
            .active_tab()
            .is_some_and(|t| t.tool_core.poly_points_mm.len() >= 2)
        {
            let pts = self
                .active_tab_mut()
                .map(|t| std::mem::take(&mut t.tool_core.poly_points_mm))
                .unwrap_or_default();
            if self.active_tool == ToolId::BSpline {
                return self.commit_bspline_points(&pts, false).is_some();
            }
            let path = BezierPath::from_polygon(pts, false);
            let obj = SceneObject::new(
                ObjectId(0),
                self.active_tool.label(),
                NodePayload::Path(path),
            )
            .with_fill(Fill::None)
            .with_outline(self.default_outline.clone());
            return self.create_object_on_active_layer(obj).is_some();
        }
        false
    }

    // -------------------------------------------------------------------------
    // Direct On-Canvas Inline Text Editing (ArtisticText & ParagraphText)
    // -------------------------------------------------------------------------

    /// Opens live on-canvas text editing for an existing text object or a fresh text insertion.
    pub fn begin_inline_text_edit(
        &mut self,
        object_id: Option<ObjectId>,
        world_pos_mm: Point2D,
        is_paragraph: bool,
        box_size_mm: Vec2D,
    ) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let (buf, resolved_pos, resolved_para, resolved_size) = if let Some(oid) = object_id {
            if let Some(obj) = tab.document.object(oid) {
                let pos = Point2D::new(obj.transform.m[4], obj.transform.m[5]);
                match &obj.payload {
                    NodePayload::ArtisticText(art) => {
                        (art.plain_text(), pos, false, Vec2D::new(80.0, 16.0))
                    }
                    NodePayload::ParagraphText(para) => {
                        (para.plain_text(), pos, true, para.frame_size)
                    }
                    _ => (String::new(), world_pos_mm, is_paragraph, box_size_mm),
                }
            } else {
                (String::new(), world_pos_mm, is_paragraph, box_size_mm)
            }
        } else {
            (String::new(), world_pos_mm, is_paragraph, box_size_mm)
        };
        tab.inline_text_edit = Some(InlineTextEditState {
            object_id,
            world_pos_mm: resolved_pos,
            buffer: buf,
            is_paragraph: resolved_para,
            box_size_mm: Vec2D::new(resolved_size.x.max(24.0), resolved_size.y.max(12.0)),
            just_opened: true,
        });
    }

    /// Commits the active on-canvas inline text edit via undoable `CommandOp`.
    pub fn commit_inline_text_edit(&mut self) -> bool {
        let Some(edit) = self
            .active_tab_mut()
            .and_then(|t| t.inline_text_edit.take())
        else {
            return false;
        };
        if edit.buffer.trim().is_empty() {
            return false;
        }
        if let Some(oid) = edit.object_id {
            let payload_opt = self.active_tab().and_then(|tab| {
                let obj = tab.document.object(oid)?;
                match &obj.payload {
                    NodePayload::ArtisticText(art) => {
                        let style = art
                            .runs
                            .first()
                            .map(|r| r.style.clone())
                            .unwrap_or_else(|| self.default_text_style.clone());
                        Some(NodePayload::ArtisticText(ArtisticText::from_plain(
                            edit.buffer.clone(),
                            style,
                        )))
                    }
                    NodePayload::ParagraphText(para) => {
                        let style = para
                            .runs
                            .first()
                            .map(|r| r.style.clone())
                            .unwrap_or_else(|| self.default_text_style.clone());
                        let mut updated = para.clone();
                        updated.runs = vec![coree_core::TextRun::new(edit.buffer.clone(), style)];
                        Some(NodePayload::ParagraphText(updated))
                    }
                    _ => None,
                }
            });
            if let Some(payload) = payload_opt {
                return match self.execute_command(CommandOp::EditText { id: oid, payload }) {
                    Ok(()) => true,
                    Err(e) => {
                        self.push_toast(e, true);
                        false
                    }
                };
            }
            return false;
        }

        let (name, payload) = if edit.is_paragraph {
            (
                "Paragraph Text",
                NodePayload::ParagraphText(coree_core::ParagraphText::new(
                    edit.buffer,
                    edit.box_size_mm,
                    self.default_text_style.clone(),
                )),
            )
        } else {
            (
                "Artistic Text",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    edit.buffer,
                    self.default_text_style.clone(),
                )),
            )
        };
        let obj = SceneObject::new(ObjectId(0), name, payload)
            .with_fill(Fill::Solid(self.default_text_style.fill.clone()))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(
                edit.world_pos_mm.x,
                edit.world_pos_mm.y,
            ));
        self.create_object_on_active_layer(obj).is_some()
    }

    /// Cancels the active on-canvas inline text edit without modifying the document.
    pub fn cancel_inline_text_edit(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.inline_text_edit = None;
        }
    }

    // -------------------------------------------------------------------------
    // Connectors with Live Object Anchor Snapping
    // -------------------------------------------------------------------------

    /// Finds the nearest connectable object & bounding-box anchor (`Top`/`Bottom`/`Left`/`Right`/`Center`) near `pt`.
    #[must_use]
    pub fn find_nearest_object_anchor(
        doc: &Document,
        page_index: usize,
        pt: Point2D,
        tol_mm: f64,
    ) -> Option<(ObjectId, AnchorPoint, Point2D)> {
        coree_tools::find_nearest_object_anchor(doc, page_index, pt, tol_mm)
    }

    /// Creates a live-snapped Straight or Right-Angle connector from `start_mm` to `end_mm`.
    pub fn commit_connector_drag(
        &mut self,
        start_mm: Point2D,
        end_mm: Point2D,
        right_angle: bool,
    ) -> Option<ObjectId> {
        let (from_snap, to_snap) = {
            let tab = self.active_tab()?;
            let pidx = tab.document.active_page_index();
            let from_s = Self::find_nearest_object_anchor(&tab.document, pidx, start_mm, 8.0);
            let to_s = Self::find_nearest_object_anchor(&tab.document, pidx, end_mm, 8.0);
            (from_s, to_s)
        };
        let (from_obj, from_anchor, from_fallback) = match from_snap {
            Some((oid, anc, pos)) => (Some(oid), anc, pos),
            None => (None, AnchorPoint::Center, start_mm),
        };
        let (to_obj, to_anchor, to_fallback) = match to_snap {
            Some((oid, anc, pos)) if Some(oid) != from_obj => (Some(oid), anc, pos),
            _ => (None, AnchorPoint::Center, end_mm),
        };
        let conn = ConnectorObject {
            from_obj,
            from_anchor,
            from_fallback,
            to_obj,
            to_anchor,
            to_fallback,
            style: if right_angle {
                ConnectorStyle::RightAngle
            } else {
                ConnectorStyle::Straight
            },
            corner_radius_mm: 2.0,
        };
        let obj = SceneObject::new(
            ObjectId(0),
            if right_angle {
                "Right-Angle Connector"
            } else {
                "Straight Connector"
            },
            NodePayload::Connector(conn),
        )
        .with_fill(Fill::None)
        .with_outline(self.default_outline.clone());
        self.create_object_on_active_layer(obj)
    }

    // -------------------------------------------------------------------------
    // All 7 Interactive Vector Effect Tools + Interactive Transparency
    // -------------------------------------------------------------------------

    /// Replaces effect at `effect_index` (or appends when `None`) on `object_id` cleanly through `CommandHistory`.
    pub fn replace_effect_on_object(
        &mut self,
        object_id: ObjectId,
        effect_index: Option<usize>,
        new_effect: Effect,
    ) -> bool {
        let cmd = if let Some(idx) = effect_index {
            CommandOp::Batch(vec![
                CommandOp::RemoveEffect {
                    id: object_id,
                    effect_index: idx,
                },
                CommandOp::AddEffect {
                    id: object_id,
                    effect: new_effect,
                },
            ])
        } else {
            CommandOp::AddEffect {
                id: object_id,
                effect: new_effect,
            }
        };
        self.execute_command(cmd).is_ok()
    }

    /// Copies an [`Effect::Envelope`] from `source_id` onto `target_id` by remapping the
    /// source envelope cage proportionally into `target_id`'s local bounds via `CommandOp`.
    pub fn copy_envelope_from_object(&mut self, target_id: ObjectId, source_id: ObjectId) -> bool {
        if target_id == source_id {
            return false;
        }
        let Some(tab) = self.active_tab() else {
            return false;
        };
        let Some(src_obj) = tab.document.object(source_id) else {
            return false;
        };
        let Some((mode, src_cage)) = src_obj.effects.iter().rev().find_map(|e| match e {
            Effect::Envelope { mode, cage } if cage.len() >= 4 => Some((*mode, cage.clone())),
            _ => None,
        }) else {
            return false;
        };
        let Some(src_bb) = Self::object_local_bounds(&tab.document, source_id) else {
            return false;
        };
        let Some(dst_bb) = Self::object_local_bounds(&tab.document, target_id) else {
            return false;
        };
        let src_w = src_bb.width().max(1e-6);
        let src_h = src_bb.height().max(1e-6);
        let dst_w = dst_bb.width().max(1e-6);
        let dst_h = dst_bb.height().max(1e-6);
        let remapped: Vec<Point2D> = src_cage
            .into_iter()
            .map(|p| {
                let u = (p.x - src_bb.min_x) / src_w;
                let v = (p.y - src_bb.min_y) / src_h;
                Point2D::new(dst_bb.min_x + u * dst_w, dst_bb.min_y + v * dst_h)
            })
            .collect();
        let existing_idx = tab.document.object(target_id).and_then(|o| {
            o.effects
                .iter()
                .position(|e| matches!(e, Effect::Envelope { .. }))
        });
        let new_eff = Effect::Envelope {
            mode,
            cage: remapped,
        };
        let cmd = match existing_idx {
            Some(idx) => CommandOp::Batch(vec![
                CommandOp::RemoveEffect {
                    id: target_id,
                    effect_index: idx,
                },
                CommandOp::AddEffect {
                    id: target_id,
                    effect: new_eff,
                },
            ]),
            None => CommandOp::AddEffect {
                id: target_id,
                effect: new_eff,
            },
        };
        self.execute_command(cmd).is_ok()
    }

    /// Ensures `object_id` has an 8-point Coons `Effect::Envelope` and returns its index in `obj.effects`.
    pub fn ensure_envelope_on_object(&mut self, object_id: ObjectId) -> Option<usize> {
        let existing = self.active_tab().and_then(|tab| {
            tab.document
                .object(object_id)?
                .effects
                .iter()
                .position(|e| matches!(e, Effect::Envelope { .. }))
        });
        if existing.is_some() {
            return existing;
        }
        let local_bb = self
            .active_tab()
            .and_then(|t| Self::object_local_bounds(&t.document, object_id))
            .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
        let cx = local_bb.center().x;
        let cy = local_bb.center().y;
        let cage = vec![
            Point2D::new(local_bb.min_x, local_bb.min_y),
            Point2D::new(cx, local_bb.min_y),
            Point2D::new(local_bb.max_x, local_bb.min_y),
            Point2D::new(local_bb.max_x, cy),
            Point2D::new(local_bb.max_x, local_bb.max_y),
            Point2D::new(cx, local_bb.max_y),
            Point2D::new(local_bb.min_x, local_bb.max_y),
            Point2D::new(local_bb.min_x, cy),
        ];
        let eff = Effect::Envelope {
            mode: self.effects_state.envelope_mode,
            cage,
        };
        if self
            .execute_command(CommandOp::AddEffect {
                id: object_id,
                effect: eff,
            })
            .is_ok()
        {
            self.active_tab().and_then(|tab| {
                tab.document
                    .object(object_id)?
                    .effects
                    .iter()
                    .position(|e| matches!(e, Effect::Envelope { .. }))
            })
        } else {
            None
        }
    }

    /// Applies any of the 7 Interactive Effect tools (`DropShadow`, `BlockShadow`, `Contour`,
    /// `Blend`, `Distort`, `Envelope`, `Extrude25D`) or `Transparency` from an on-canvas drag vector,
    /// delegating directly to `coree_tools::builders::build_interactive_effect_output` (`#7`).
    pub fn apply_interactive_effect_drag(
        &mut self,
        tool: ToolId,
        start_mm: Point2D,
        end_mm: Point2D,
    ) -> bool {
        let creation_layer_id = self.active_creation_layer_or_err().ok().flatten();
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();

        let Some(out) = self.tabs.get(self.active_tab).map(|tab| {
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            coree_tools::builders::build_interactive_effect_output(&ctx, tool, start_mm, end_mm)
        }) else {
            return false;
        };

        let applied = matches!(&out, ToolOutput::Commit { ops, .. } if !ops.is_empty());
        self.apply_tool_output(out);
        applied
    }

    // -------------------------------------------------------------------------
    // Eyedropper Sample -> Apply Dual Workflow
    // -------------------------------------------------------------------------

    /// Classic Studio Eyedropper Sample -> Apply workflow:
    /// - When `!eyedropper_apply_mode` or `force_sample` (`Shift+Click`): samples Fill
    ///   (and Outline + Effects in `AttributesEyedropper` mode) and enters Apply mode.
    /// - When `eyedropper_apply_mode` and `!force_sample`: applies the sampled Fill
    ///   (and Outline + Effects) to the clicked object via undoable `CommandOp`.
    pub fn eyedropper_click(
        &mut self,
        world_pt: Point2D,
        attributes_mode: bool,
        force_sample: bool,
    ) -> bool {
        let tol = self
            .active_tab()
            .map(|t| (6.0 / t.viewport.px_per_mm()).max(1.5))
            .unwrap_or(2.0);
        let Some(oid) = self.hit_test_canvas_point(world_pt, tol) else {
            return false;
        };

        if force_sample || !self.tool_settings.eyedropper_apply_mode {
            let sampled = self.active_tab().and_then(|tab| {
                let obj = tab.document.object(oid)?;
                Some((obj.fill.clone(), obj.outline.clone(), obj.effects.clone()))
            });
            let Some((fill, outline, effects)) = sampled else {
                return false;
            };
            self.default_fill = fill;
            if attributes_mode {
                self.default_outline = outline;
                self.tool_settings.sampled_effects = effects;
                self.push_toast(
                    "Sampled Fill, Outline & Effects — click objects to apply (Shift+Click to re-sample)",
                    false,
                );
            } else {
                self.push_toast(
                    "Sampled Fill color — click objects to apply (Shift+Click to re-sample)",
                    false,
                );
            }
            self.tool_settings.eyedropper_apply_mode = true;
            return true;
        }

        // Apply mode!
        let mut ops = vec![CommandOp::SetFill {
            ids: vec![oid],
            fill: self.default_fill.clone(),
        }];
        if attributes_mode {
            ops.push(CommandOp::SetOutline {
                ids: vec![oid],
                outline: self.default_outline.clone(),
            });
            for eff in &self.tool_settings.sampled_effects {
                ops.push(CommandOp::AddEffect {
                    id: oid,
                    effect: eff.clone(),
                });
            }
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast(
                if attributes_mode {
                    "Applied sampled attributes"
                } else {
                    "Applied sampled color"
                },
                false,
            );
            true
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // Interactive Fill & Interactive Mesh Fill
    // -------------------------------------------------------------------------

    /// Applies an interactive linear fountain fill along `start_mm -> end_mm` on selected/hit objects.
    pub fn apply_interactive_fill_drag(&mut self, start_mm: Point2D, end_mm: Point2D) -> bool {
        let target_ids: Vec<ObjectId> = self
            .active_tab()
            .map(|t| {
                if !t.selection.objects.is_empty() {
                    t.selection.objects.clone()
                } else {
                    hit_test_point(&t.document, t.document.active_page_index(), start_mm, 8.0)
                        .into_iter()
                        .collect()
                }
            })
            .unwrap_or_default();
        if target_ids.is_empty() {
            return false;
        }
        let per_obj_ops: Vec<CommandOp> = self
            .active_tab()
            .map(|t| {
                target_ids
                    .iter()
                    .copied()
                    .filter_map(|oid| {
                        if !is_object_editable_in_doc(&t.document, oid) {
                            return None;
                        }
                        let inv = world_inverse_transform_of(&t.document, oid);
                        let fill = Fill::LinearGradient {
                            start: inv.transform_point(start_mm),
                            end: inv.transform_point(end_mm),
                            stops: vec![
                                coree_core::GradientStop::new(
                                    0.0,
                                    self.properties_state.grad_start_color.clone(),
                                ),
                                coree_core::GradientStop::new(
                                    1.0,
                                    self.properties_state.grad_end_color.clone(),
                                ),
                            ],
                        };
                        Some(CommandOp::SetFill {
                            ids: vec![oid],
                            fill,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if per_obj_ops.is_empty() {
            return false;
        }
        let cmd = if per_obj_ops.len() == 1 {
            per_obj_ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(per_obj_ops)
        };
        self.execute_command(cmd).is_ok()
    }

    /// Initializes `Fill::MeshFill` on the selected/clicked object if needed, and warps/recolors the nearest mesh node.
    pub fn mesh_fill_interact(
        &mut self,
        start_mm: Point2D,
        end_mm: Point2D,
        recolor: Option<Color>,
    ) -> bool {
        let oid = self.active_tab().and_then(|tab| {
            if !tab.selection.objects.is_empty() {
                tab.selection
                    .objects
                    .iter()
                    .copied()
                    .find(|&id| is_object_editable_in_doc(&tab.document, id))
            } else {
                hit_test_point(
                    &tab.document,
                    tab.document.active_page_index(),
                    start_mm,
                    8.0,
                )
                .filter(|&id| is_object_editable_in_doc(&tab.document, id))
            }
        });
        let Some(oid) = oid else {
            return false;
        };
        if let Some(tab) = self.active_tab_mut() {
            if !tab.selection.contains(oid) {
                tab.selection.select_single(&tab.document, oid);
            }
        }
        let rows = self.properties_state.mesh_rows.max(2);
        let cols = self.properties_state.mesh_cols.max(2);

        let updated_fill = self.active_tab().and_then(|tab| {
            let obj = tab.document.object(oid)?;
            let inv = world_inverse_transform_of(&tab.document, oid);
            let mut mesh = match &obj.fill {
                Fill::MeshFill { .. } => obj.fill.clone(),
                _ => {
                    let local_bb = Self::object_local_bounds(&tab.document, oid)
                        .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
                    Self::build_mesh_fill_for_bounds(local_bb, rows, cols)
                }
            };
            if let Fill::MeshFill { ref mut nodes, .. } = mesh {
                let ls = inv.transform_point(start_mm);
                let le = inv.transform_point(end_mm);
                if let Some((best_idx, _)) = nodes.iter().enumerate().min_by(|a, b| {
                    a.1.pos
                        .distance_to(ls)
                        .partial_cmp(&b.1.pos.distance_to(ls))
                        .unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    if ls.distance_to(le) > 0.25 {
                        nodes[best_idx].pos = le;
                    }
                    if let Some(col) = recolor {
                        nodes[best_idx].color = col;
                    }
                }
            }
            Some(mesh)
        });
        let Some(fill) = updated_fill else {
            return false;
        };
        if self
            .execute_command(CommandOp::SetFill {
                ids: vec![oid],
                fill,
            })
            .is_ok()
        {
            self.push_toast("Updated Mesh Fill grid", false);
            true
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // Integrated Bitmap Studio (All 8 Raster Brush Ops + Auto-Created 150 DPI Layer)
    // -------------------------------------------------------------------------
}
