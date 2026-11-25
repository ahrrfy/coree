// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Executes an interactive drag-box gesture for EVERY SINGLE one of the 62 [`ToolId`] variants
    /// (exhaustive match — ZERO silent `_ => return` tools!).
    pub fn commit_drawn_box(&mut self, rect: Rect2D) {
        if !rect.is_finite() || (rect.width() < 0.5 && rect.height() < 0.5) {
            return;
        }
        if self.active_tab().is_none() {
            return;
        }
        let fill = self.default_fill.clone();
        let outline = self.default_outline.clone();
        let ts = &self.tool_settings;

        let (name, payload, xf) = match self.active_tool {
            // Group 0: Pick / FreehandPick / FreeTransform
            ToolId::Pick => {
                if let Some(tab) = self.active_tab_mut() {
                    let hits = coree_render::hit_test_rect(
                        &tab.document,
                        tab.document.active_page_index(),
                        rect,
                        false,
                    );
                    tab.selection.select_many(&tab.document, &hits);
                    tab.metrics_dirty = true;
                }
                return;
            }
            ToolId::FreehandPick => {
                let poly = vec![
                    rect.min(),
                    Point2D::new(rect.max_x, rect.min_y),
                    rect.max(),
                    Point2D::new(rect.min_x, rect.max_y),
                ];
                let _ = self.select_by_lasso(&poly, false);
                return;
            }
            ToolId::FreeTransform => {
                let _ =
                    self.apply_free_transform(rect.min(), rect.center(), rect.max(), false, false);
                return;
            }

            // Group 1: Shape / Node & 5 Vector Sculpting Tools
            ToolId::ShapeNode => {
                let _ = self.select_nodes_in_rect(rect, false);
                return;
            }
            ToolId::Smooth
            | ToolId::Smear
            | ToolId::Twirl
            | ToolId::AttractRepel
            | ToolId::Roughen => {
                let _ = self.apply_sculpt_stroke(
                    self.active_tool,
                    &[rect.min(), rect.center(), rect.max()],
                    false,
                );
                return;
            }

            // Group 2: Crop & Cutting
            ToolId::Crop => {
                let _ = self.apply_crop_rect(rect);
                return;
            }
            ToolId::Knife => {
                let _ = self.apply_knife_cut(rect.min(), rect.max());
                return;
            }
            ToolId::VirtualSegmentDelete => {
                let _ = self.apply_virtual_segment_delete_at(rect.center());
                return;
            }
            ToolId::Eraser => {
                let _ = self.apply_vector_eraser_stroke(&[rect.min(), rect.center(), rect.max()]);
                return;
            }

            // Group 3: Zoom & Pan
            ToolId::Zoom => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.fit_rect(rect, 12.0);
                    tab.needs_rerender = true;
                }
                self.complete_one_shot_zoom_if_active();
                return;
            }
            ToolId::Pan => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.pan_mm.x -= rect.width() * 0.5;
                    tab.viewport.pan_mm.y -= rect.height() * 0.5;
                    tab.needs_rerender = true;
                }
                return;
            }

            // Group 4: Curves & Drawing
            ToolId::Freehand => {
                let path =
                    BezierPath::from_polygon(vec![rect.min(), rect.center(), rect.max()], false);
                (
                    "Freehand Curve".to_owned(),
                    NodePayload::Path(path),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::TwoPointLine => {
                let path = BezierPath::from_line(rect.min(), rect.max());
                (
                    "Line".to_owned(),
                    NodePayload::Path(path),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::Bezier | ToolId::Pen => {
                let p0 = rect.min();
                let p1 = rect.max();
                let c1 = Point2D::new(rect.max_x, rect.min_y);
                let c2 = Point2D::new(rect.min_x, rect.max_y);
                let sp = Subpath {
                    nodes: vec![
                        PathNode::new(p0, None, Some(c1), NodeKind::Smooth),
                        PathNode::new(p1, Some(c2), None, NodeKind::Smooth),
                    ],
                    closed: false,
                };
                (
                    self.active_tool.label().to_owned(),
                    NodePayload::Path(BezierPath::from_subpath(sp)),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::BSpline => {
                let pts = vec![
                    rect.min(),
                    Point2D::new(rect.center().x, rect.min_y),
                    Point2D::new(rect.center().x, rect.max_y),
                    rect.max(),
                ];
                let _ = self.commit_bspline_points(&pts, false);
                return;
            }
            ToolId::Polyline => {
                let path = BezierPath::from_polygon(
                    vec![rect.min(), Point2D::new(rect.max_x, rect.min_y), rect.max()],
                    false,
                );
                (
                    "Polyline".to_owned(),
                    NodePayload::Path(path),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::ThreePointCurve => {
                let p1 = Point2D::new(rect.min_x, rect.max_y);
                let p2 = Point2D::new(rect.max_x, rect.max_y);
                let p3 = Point2D::new(rect.center().x, rect.min_y);
                let _ = self.commit_three_point_curve(p1, p2, p3);
                return;
            }
            ToolId::SmartDrawing => {
                let pts = vec![
                    rect.min(),
                    Point2D::new(rect.max_x, rect.min_y),
                    rect.max(),
                    Point2D::new(rect.min_x, rect.max_y),
                    rect.min(),
                ];
                let recognized = coree_core::SmartDrawing::recognize_rough_stroke(&pts);
                (
                    "Smart Shape".to_owned(),
                    NodePayload::Path(recognized.to_bezier_path()),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::LiveSketch => {
                let stroke = vec![rect.min(), rect.center(), rect.max()];
                let path = coree_core::LiveSketch::average_strokes(&[stroke], 24);
                (
                    "LiveSketch Curve".to_owned(),
                    NodePayload::Path(path),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::ArtisticMedia => {
                let center = BezierPath::from_line(rect.min(), rect.max());
                let expanded = self.expand_artistic_stroke(&center);
                (
                    "Artistic Media".to_owned(),
                    NodePayload::Path(expanded),
                    Affine2D::IDENTITY,
                )
            }

            // Group 5: Rectangle
            ToolId::Rectangle | ToolId::ThreePointRectangle => (
                "Rectangle".to_owned(),
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: rect.width(),
                    height: rect.height(),
                    corner_radii: [ts.rect_corner_radius_mm; 4],
                    corner_style: ts.rect_corner_style,
                    lock_corners: true,
                }),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),

            // Group 6: Ellipse
            ToolId::Ellipse | ToolId::ThreePointEllipse => (
                "Ellipse".to_owned(),
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: rect.width() * 0.5,
                    ry: rect.height() * 0.5,
                    mode: ts.ellipse_mode,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),

            // Group 7: Object / Parametric Shapes
            ToolId::Polygon => (
                "Polygon".to_owned(),
                NodePayload::Shape(ParametricShape::Polygon {
                    sides: ts.polygon_sides.clamp(3, 64),
                    radius_x: rect.width() * 0.5,
                    radius_y: rect.height() * 0.5,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::Star | ToolId::ComplexStar => (
                "Star".to_owned(),
                NodePayload::Shape(ParametricShape::Star {
                    points: ts.star_points.clamp(3, 64),
                    outer_radius: (rect.width().max(rect.height())) * 0.5,
                    inner_ratio: ts.star_inner_ratio.clamp(0.1, 0.9),
                    complex: self.active_tool == ToolId::ComplexStar,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::Spiral => (
                "Spiral".to_owned(),
                NodePayload::Shape(ParametricShape::Spiral {
                    revolutions: ts.spiral_revolutions.clamp(1.0, 30.0),
                    radius: (rect.width().max(rect.height())) * 0.5,
                    logarithmic: ts.spiral_logarithmic,
                    expansion: 1.5,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::GraphPaper => (
                "Graph Paper".to_owned(),
                NodePayload::Shape(ParametricShape::GraphPaper {
                    cols: ts.graph_cols.clamp(1, 50),
                    rows: ts.graph_rows.clamp(1, 50),
                    width: rect.width(),
                    height: rect.height(),
                }),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),
            ToolId::Impact => (
                "Impact Rays".to_owned(),
                NodePayload::Shape(ParametricShape::Impact {
                    rays: ts.impact_rays.clamp(4, 128),
                    inner_radius: (rect.width().min(rect.height())) * 0.15,
                    outer_radius: (rect.width().max(rect.height())) * 0.5,
                    style: ts.impact_style,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::BasicShapes => (
                format!("{:?}", ts.basic_shape_kind),
                NodePayload::Shape(ParametricShape::BasicShape {
                    kind: ts.basic_shape_kind,
                    width: rect.width(),
                    height: rect.height(),
                }),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),

            // Group 8: Text & Table
            ToolId::ArtisticText => {
                self.insert_artistic_text_at(rect.min());
                return;
            }
            ToolId::ParagraphText => (
                "Paragraph Text".to_owned(),
                NodePayload::ParagraphText(coree_core::ParagraphText::new(
                    ts.text_insert_draft.clone(),
                    Vec2D::new(rect.width().max(20.0), rect.height().max(12.0)),
                    self.default_text_style.clone(),
                )),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),
            ToolId::Table => {
                let rows = ts.table_rows.clamp(1, 30);
                let cols = ts.table_cols.clamp(1, 30);
                (
                    format!("Table {rows}x{cols}"),
                    NodePayload::Table(VectorTable::new(
                        rows,
                        cols,
                        (rect.width() / cols as f64).max(4.0),
                        (rect.height() / rows as f64).max(4.0),
                    )),
                    Affine2D::translate(rect.min_x, rect.min_y),
                )
            }

            // Group 9: Dimensions & Connectors
            ToolId::ParallelDimension
            | ToolId::HvDimension
            | ToolId::AngularDimension
            | ToolId::SegmentDimension
            | ToolId::ThreePointCallout => {
                let kind = match self.active_tool {
                    ToolId::HvDimension => {
                        if rect.width() >= rect.height() {
                            DimensionKind::Horizontal
                        } else {
                            DimensionKind::Vertical
                        }
                    }
                    ToolId::AngularDimension => DimensionKind::Angular,
                    ToolId::SegmentDimension => DimensionKind::Segment,
                    ToolId::ThreePointCallout => DimensionKind::Callout3Pt,
                    _ => DimensionKind::Parallel,
                };
                let mut dim = if kind == DimensionKind::Angular {
                    coree_core::DimensionObject::angular(
                        rect.min(),
                        Point2D::new(rect.max_x, rect.min_y),
                        rect.max(),
                        ts.dimension_offset_mm,
                    )
                } else {
                    coree_core::DimensionObject::linear(
                        kind,
                        rect.min(),
                        rect.max(),
                        ts.dimension_offset_mm,
                    )
                };
                if kind == DimensionKind::Callout3Pt {
                    dim.p3 = Some(Point2D::new(rect.max_x + 10.0, rect.min_y));
                    dim.label_override = Some("Callout".to_owned());
                }
                (
                    format!("{kind:?} Dimension"),
                    NodePayload::Dimension(dim),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::StraightConnector | ToolId::RightAngleConnector => {
                let _ = self.commit_connector_drag(
                    rect.min(),
                    rect.max(),
                    self.active_tool == ToolId::RightAngleConnector,
                );
                return;
            }

            // Group 10: Interactive Vector Effects + Transparency
            ToolId::DropShadow
            | ToolId::BlockShadow
            | ToolId::Contour
            | ToolId::Blend
            | ToolId::Distort
            | ToolId::Envelope
            | ToolId::Extrude25D
            | ToolId::Transparency => {
                let _ =
                    self.apply_interactive_effect_drag(self.active_tool, rect.min(), rect.max());
                return;
            }

            // Group 11: Eyedroppers
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                let _ = self.eyedropper_click(
                    rect.center(),
                    self.active_tool == ToolId::AttributesEyedropper,
                    false,
                );
                return;
            }

            // Group 12: Interactive, Mesh & Smart Fills
            ToolId::InteractiveFill => {
                let _ = self.apply_interactive_fill_drag(rect.min(), rect.max());
                return;
            }
            ToolId::MeshFill => {
                let _ = self.mesh_fill_interact(rect.min(), rect.max(), None);
                return;
            }
            ToolId::SmartFill => {
                let Some(layer_id) = self.ensure_active_creation_layer() else {
                    return;
                };
                let created = self.active_tab().and_then(|tab| {
                    coree_render::create_smart_fill_object(
                        &tab.document,
                        tab.document.active_page_index(),
                        rect.center(),
                        self.default_fill.clone(),
                        self.default_outline.clone(),
                    )
                });
                if let Some(obj) = created {
                    match self.execute_command(CommandOp::create_object(obj, layer_id)) {
                        Ok(()) => self.push_toast("Created Smart Fill region", false),
                        Err(e) => self.push_toast(e, true),
                    }
                }
                return;
            }

            // Group 13: Lens Tool
            ToolId::LensTool => {
                let lens = self.lens_state.build_config();
                let mut obj = SceneObject::new(
                    ObjectId(0),
                    format!("{} Lens", lens.kind.name()),
                    NodePayload::Shape(ParametricShape::Ellipse {
                        rx: rect.width() * 0.5,
                        ry: rect.height() * 0.5,
                        mode: EllipseMode::Full,
                        start_deg: 0.0,
                        end_deg: 360.0,
                    }),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.25))
                .with_transform(Affine2D::translate(rect.center().x, rect.center().y));
                obj.lens = Some(lens);
                let _ = self.create_object_on_active_layer(obj);
                return;
            }

            // Group 14: Integrated Bitmap Studio
            ToolId::BitmapBrush | ToolId::BitmapEraser | ToolId::CloneStamp | ToolId::Smudge => {
                let _ = self.apply_raster_tool_stroke(self.active_tool, &[rect.min(), rect.max()]);
                return;
            }
            ToolId::MaskSelect => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.raster_mask_rect_mm = Some(rect);
                    tab.needs_rerender = true;
                }
                self.push_toast(
                    format!(
                        "Set bitmap mask ({:.1} × {:.1} mm)",
                        rect.width(),
                        rect.height()
                    ),
                    false,
                );
                return;
            }
        };

        let is_open_stroke = matches!(
            self.active_tool,
            ToolId::Freehand
                | ToolId::TwoPointLine
                | ToolId::Bezier
                | ToolId::Pen
                | ToolId::BSpline
                | ToolId::Polyline
                | ToolId::ThreePointCurve
                | ToolId::LiveSketch
                | ToolId::Spiral
                | ToolId::ParallelDimension
                | ToolId::HvDimension
                | ToolId::AngularDimension
                | ToolId::SegmentDimension
                | ToolId::ThreePointCallout
                | ToolId::StraightConnector
                | ToolId::RightAngleConnector
        );
        let obj = SceneObject::new(ObjectId(0), name, payload)
            .with_fill(if is_open_stroke { Fill::None } else { fill })
            .with_outline(if self.active_tool == ToolId::ArtisticMedia {
                OutlineStyle::none()
            } else {
                outline
            })
            .with_transform(xf);
        let _ = self.create_object_on_active_layer(obj);
    }

    // -------------------------------------------------------------------------
    // Complete 62-Tool Interactive Canvas Engines (Shared by UI & Headless Tests)
    // -------------------------------------------------------------------------

    /// Hit-tests a world coordinate on the active page, optionally treating unfilled
    /// closed objects as filled when `treat_all_objects_as_filled` is enabled.
    #[must_use]
    pub fn hit_test_canvas_point(&self, world_pt: Point2D, tol_mm: f64) -> Option<ObjectId> {
        let tab = self.active_tab()?;
        coree_tools::hit_test_canvas_point_in_doc(
            &tab.document,
            tab.powerclip_edit_id,
            self.treat_all_objects_as_filled,
            world_pt,
            tol_mm,
        )
    }

    /// Selects all visible unlocked objects on the active page intersecting the freehand `polygon` lasso.
    pub fn select_by_lasso(&mut self, polygon: &[Point2D], additive: bool) -> Vec<ObjectId> {
        if polygon.len() < 3 {
            return Vec::new();
        }
        let Some(tab) = self.active_tab_mut() else {
            return Vec::new();
        };
        let hits =
            coree_tools::compute_lasso_hits_scoped(&tab.document, tab.powerclip_edit_id, polygon);
        if additive {
            let mut combined = tab.selection.objects.clone();
            for id in &hits {
                if !combined.contains(id) {
                    combined.push(*id);
                }
            }
            tab.selection.select_many(&tab.document, &combined);
        } else {
            tab.selection.select_many(&tab.document, &hits);
        }
        tab.metrics_dirty = true;
        hits
    }

    /// Stamps a duplicate of the currently selected objects at `offset` (used by Left-Drag + Right-Click).
    /// Unlike `duplicate_selection`, keeps the original dragged objects selected so the drag continues seamlessly.
    pub fn stamp_duplicate_at_offset(&mut self, offset: Vec2D) -> Result<Vec<ObjectId>, String> {
        self.stamp_duplicate_with_affine(Affine2D::translate(offset.x, offset.y))
    }

    /// Stamps a duplicate of the currently selected objects with transform `delta_xf`,
    /// preserving the original selection so an active drag can continue uninterrupted.
    pub fn stamp_duplicate_with_affine(
        &mut self,
        delta_xf: Affine2D,
    ) -> Result<Vec<ObjectId>, String> {
        let orig_selection = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let created = self.duplicate_selection_with_affine(delta_xf)?;
        if let Some(tab) = self.active_tab_mut() {
            tab.selection.select_many(&tab.document, &orig_selection);
            tab.metrics_dirty = true;
        }
        Ok(created)
    }

    /// Stamps a duplicate during an active live-preview drag (`MovingSelection`,
    /// `ScalingSelection`, `RotatingSelection`, `SkewingSelection`, or `FreeTransforming`)
    /// WITHOUT polluting `CommandHistory` snapshots with uncommitted mid-drag transforms!
    /// Stamps a persistent duplicate at the live drag transform without interrupting the active
    /// pointer gesture. Since [`ToolCore`] never mutates `Document` mid-drag, `tab.document`
    /// already holds the exact pre-drag state.
    pub fn stamp_duplicate_during_active_drag(&mut self) -> Result<Vec<ObjectId>, String> {
        let live_affine = self
            .active_tab()
            .and_then(|tab| tab.tool_core.active_live_world_transform());
        let Some(live_affine) = live_affine else {
            return Ok(Vec::new());
        };
        self.stamp_duplicate_with_affine(live_affine)
    }

    /// Applies an interactive `FreeTransform` around `anchor_mm` from `start_mm` to `current_mm`.
    pub fn apply_free_transform(
        &mut self,
        anchor_mm: Point2D,
        start_mm: Point2D,
        current_mm: Point2D,
        snap_15: bool,
        mirror: bool,
    ) -> bool {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let ids = if ids.is_empty() {
            if let Some(hit) = self.hit_test_canvas_point(anchor_mm, 6.0) {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.select_single(&tab.document, hit);
                }
                vec![hit]
            } else {
                return false;
            }
        } else {
            ids
        };
        let Some(affine) =
            compute_free_transform_affine(anchor_mm, start_mm, current_mm, snap_15, mirror)
        else {
            return false;
        };
        self.apply_world_transform_to_selection(&ids, affine)
    }

    // -------------------------------------------------------------------------
    // `F10` Shape / Node Editor Operations (All 11 Property Bar Actions + Parametric Shapes)
    // -------------------------------------------------------------------------

    /// Marquee-selects all Bézier nodes of selected paths (or selects objects) inside `rect`.
    pub fn select_nodes_in_rect(&mut self, rect: Rect2D, additive: bool) -> usize {
        let Some(tab) = self.active_tab_mut() else {
            return 0;
        };
        if tab.selection.is_empty() {
            let hits = coree_render::hit_test_rect(
                &tab.document,
                tab.document.active_page_index(),
                rect,
                false,
            );
            tab.selection.select_many(&tab.document, &hits);
            return hits.len();
        }
        if !additive {
            tab.selection.node_selection.clear();
        }
        let mut count = 0usize;
        for &oid in &tab.selection.objects {
            let Some(obj) = tab.document.object(oid) else {
                continue;
            };
            let NodePayload::Path(ref path) = obj.payload else {
                continue;
            };
            for (sp_idx, sp) in path.subpaths.iter().enumerate() {
                for (n_idx, node) in sp.nodes.iter().enumerate() {
                    let wp = obj.transform.transform_point(node.pos);
                    if rect.contains_point(wp) {
                        let nref = NodeRef {
                            object_id: oid,
                            subpath_index: sp_idx,
                            node_index: n_idx,
                        };
                        if !tab.selection.node_selection.contains(nref) {
                            tab.selection.node_selection.nodes.push(nref);
                        }
                        count += 1;
                    }
                }
            }
        }
        count
    }

    /// Commits a live `ParametricShape` handle edit cleanly through `CommandHistory` + WAL
    /// while preserving the object's exact `ObjectId`, Z-order, and parent container (`Layer`,
    /// `Group`, or `PowerClip`) without ever duplicating nested objects onto top-level layers.
    pub fn commit_parametric_shape_edit(
        &mut self,
        object_id: ObjectId,
        orig_shape: ParametricShape,
        new_shape: ParametricShape,
    ) -> bool {
        if orig_shape == new_shape {
            return false;
        }
        let ready = self.active_tab().is_some_and(|tab| {
            is_object_editable_in_doc(&tab.document, object_id)
                && (tab.document.layer_of_object(object_id).is_some()
                    || tab.document.parent_container_of(object_id).is_some())
                && tab
                    .document
                    .object(object_id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Shape(_)))
        });
        if !ready {
            return false;
        }
        let committed = self
            .execute_command(CommandOp::EditShape {
                id: object_id,
                shape: new_shape,
            })
            .is_ok();
        if committed {
            self.checkpoint_wal_snapshot_active();
        }
        committed
    }

    fn build_edit_path_op(doc: &Document, id: ObjectId, path: BezierPath) -> CommandOp {
        if doc
            .object(id)
            .is_some_and(|o| matches!(o.payload, NodePayload::Path(_)))
        {
            CommandOp::EditPath { id, path }
        } else {
            CommandOp::Batch(vec![
                CommandOp::ConvertToCurves { id },
                CommandOp::EditPath { id, path },
            ])
        }
    }

    /// Helper: collects editable `BezierPath`s for selected objects (converting live `ParametricShape`s if needed).
    fn selected_editable_paths(&self) -> Vec<(ObjectId, BezierPath)> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        tab.selection
            .objects
            .iter()
            .copied()
            .filter_map(|oid| {
                if !is_object_editable_in_doc(&tab.document, oid) {
                    return None;
                }
                let obj = tab.document.object(oid)?;
                match &obj.payload {
                    NodePayload::Path(p) => Some((oid, p.clone())),
                    NodePayload::Shape(s) => Some((oid, s.to_bezier_path())),
                    _ => None,
                }
            })
            .collect()
    }

    /// Shape Property Bar: inserts a new node on each selected segment (or segment 0 of selected path).
    pub fn shape_add_node(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        let mut newly_selected = Vec::new();

        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, mut refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    // Sort descending so inserting nodes doesn't invalidate earlier indices
                    refs.sort_unstable();
                    refs.dedup();
                    refs.reverse();
                    let mut changed = false;
                    for (sp_idx, n_idx) in refs {
                        let seg_count = path.subpaths.get(sp_idx).map_or(0, Subpath::segment_count);
                        if seg_count == 0 {
                            continue;
                        }
                        let seg_idx = n_idx.min(seg_count - 1);
                        if let Ok(ins) = path.add_node_at(sp_idx, seg_idx, 0.5) {
                            changed = true;
                            newly_selected.push(NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: ins,
                            });
                        }
                    }
                    if changed {
                        ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                    }
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    if path
                        .subpaths
                        .first()
                        .is_some_and(|sp| sp.segment_count() > 0)
                    {
                        if let Ok(ins) = path.add_node_at(0, 0, 0.5) {
                            newly_selected.push(NodeRef {
                                object_id: oid,
                                subpath_index: 0,
                                node_index: ins,
                            });
                            ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                        }
                    }
                }
            }
        }

        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.nodes = newly_selected;
            }
            self.push_toast("Added Bézier node", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: deletes all currently selected Bézier nodes cleanly.
    pub fn shape_delete_selected_nodes(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        if selected_nodes.is_empty() {
            return false;
        }
        let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
        for nr in selected_nodes {
            by_obj
                .entry(nr.object_id)
                .or_default()
                .push((nr.subpath_index, nr.node_index));
        }
        let mut ops = Vec::new();
        if let Some(tab) = self.active_tab() {
            for (oid, mut refs) in by_obj {
                let Some(obj) = tab.document.object(oid) else {
                    continue;
                };
                if obj.locked {
                    continue;
                }
                let NodePayload::Path(ref orig) = obj.payload else {
                    continue;
                };
                let mut path = orig.clone();
                refs.sort_unstable();
                refs.dedup();
                refs.reverse();
                let mut changed = false;
                for (sp_idx, n_idx) in refs {
                    if path.delete_node(sp_idx, n_idx).is_ok() {
                        changed = true;
                    }
                }
                if changed {
                    ops.push(CommandOp::EditPath { id: oid, path });
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.clear();
            }
            self.push_toast("Deleted selected node(s)", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: joins two selected endpoint nodes (or closes an open subpath).
    pub fn shape_join_nodes(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();

        // 1. Two nodes on the same path selected -> call BezierPath::join_nodes directly
        if selected_nodes.len() >= 2 && selected_nodes[0].object_id == selected_nodes[1].object_id {
            let oid = selected_nodes[0].object_id;
            let edited = self.active_tab().and_then(|tab| {
                let obj = tab.document.object(oid)?;
                if obj.locked {
                    return None;
                }
                let NodePayload::Path(ref p) = obj.payload else {
                    return None;
                };
                let mut path = p.clone();
                path.join_nodes(
                    selected_nodes[0].subpath_index,
                    selected_nodes[0].node_index,
                    selected_nodes[1].subpath_index,
                    selected_nodes[1].node_index,
                )
                .ok()?;
                Some(path)
            });
            if let Some(path) = edited {
                if self
                    .execute_command(CommandOp::EditPath { id: oid, path })
                    .is_ok()
                {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.selection.node_selection.clear();
                    }
                    self.push_toast("Joined nodes", false);
                    return true;
                }
            }
        }

        // 2. Fallback: join endpoints of any open subpath on the primary selected path
        let fallback_op = self.active_tab().and_then(|tab| {
            for (oid, mut path) in self.selected_editable_paths() {
                let mut joined = false;
                for sp_idx in 0..path.subpaths.len() {
                    if !path.subpaths[sp_idx].closed && path.subpaths[sp_idx].nodes.len() >= 2 {
                        let last = path.subpaths[sp_idx].nodes.len() - 1;
                        if path.join_nodes(sp_idx, 0, sp_idx, last).is_ok() {
                            joined = true;
                        }
                    }
                }
                if joined {
                    return Some(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
            None
        });
        if let Some(cmd) = fallback_op {
            if self.execute_command(cmd).is_ok() {
                self.push_toast("Joined open curve endpoints", false);
                return true;
            }
        }
        false
    }

    /// Shape Property Bar: breaks curve open at selected node(s) (or node 0 of selected path).
    pub fn shape_break_curve(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, mut refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    refs.sort_unstable();
                    refs.dedup();
                    refs.reverse();
                    let mut changed = false;
                    for (sp_idx, n_idx) in refs {
                        if path.break_at_node(sp_idx, n_idx).is_ok() {
                            changed = true;
                        }
                    }
                    if changed {
                        ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                    }
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    if !path.subpaths.is_empty() && path.break_at_node(0, 0).is_ok() {
                        ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                    }
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.clear();
            }
            self.push_toast("Broke curve at node", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: converts selected segment(s) (or all segments of selected path) to straight lines.
    pub fn shape_convert_to_line(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    for (sp_idx, n_idx) in refs {
                        let sc = path.subpaths.get(sp_idx).map_or(0, Subpath::segment_count);
                        if sc > 0 {
                            let _ = path.convert_segment_to_line(sp_idx, n_idx.min(sc - 1));
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    for sp_idx in 0..path.subpaths.len() {
                        let sc = path.subpaths[sp_idx].segment_count();
                        for seg_idx in 0..sc {
                            let _ = path.convert_segment_to_line(sp_idx, seg_idx);
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Converted segment(s) to line", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: converts selected segment(s) (or all segments of selected path) to cubic curves.
    pub fn shape_convert_to_curve(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    for (sp_idx, n_idx) in refs {
                        let sc = path.subpaths.get(sp_idx).map_or(0, Subpath::segment_count);
                        if sc > 0 {
                            let _ = path.convert_segment_to_curve(sp_idx, n_idx.min(sc - 1));
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    for sp_idx in 0..path.subpaths.len() {
                        let sc = path.subpaths[sp_idx].segment_count();
                        for seg_idx in 0..sc {
                            let _ = path.convert_segment_to_curve(sp_idx, seg_idx);
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Converted segment(s) to curve", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: sets continuity (`Cusp`, `Smooth`, `Symmetrical`) on selected nodes.
    pub fn shape_set_node_smoothness(&mut self, kind: NodeSmoothness) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    for (sp_idx, n_idx) in refs {
                        if kind != NodeKind::Cusp {
                            if let Some(sp) = path.subpaths.get_mut(sp_idx) {
                                let n = sp.nodes.len();
                                if n_idx < n && n >= 2 {
                                    let needs_h = sp.nodes[n_idx].handle_in.is_none()
                                        && sp.nodes[n_idx].handle_out.is_none();
                                    if needs_h {
                                        let prev = sp.nodes[(n_idx + n - 1) % n].pos;
                                        let next = sp.nodes[(n_idx + 1) % n].pos;
                                        let tang = (next - prev) * 0.2;
                                        if tang.length() > 1e-6 {
                                            let pos = sp.nodes[n_idx].pos;
                                            sp.nodes[n_idx].handle_in = Some(pos - tang);
                                            sp.nodes[n_idx].handle_out = Some(pos + tang);
                                        }
                                    }
                                }
                            }
                        }
                        let _ = path.set_node_kind(sp_idx, n_idx, kind);
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    for sp_idx in 0..path.subpaths.len() {
                        let len = path.subpaths[sp_idx].nodes.len();
                        for n_idx in 0..len {
                            let _ = path.set_node_kind(sp_idx, n_idx, kind);
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast(format!("Set node smoothness: {kind:?}"), false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: reverses traversal direction of all subpaths on selected path(s).
    pub fn shape_reverse_subpaths(&mut self) -> bool {
        let paths = self.selected_editable_paths();
        let ops: Vec<CommandOp> = if let Some(tab) = self.active_tab() {
            paths
                .into_iter()
                .map(|(oid, mut path)| {
                    for sp_idx in 0..path.subpaths.len() {
                        let _ = path.reverse_subpath(sp_idx);
                    }
                    Self::build_edit_path_op(&tab.document, oid, path)
                })
                .collect()
        } else {
            Vec::new()
        };
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Reversed subpath direction", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: closes all open subpaths on selected path(s).
    pub fn shape_close_curve(&mut self) -> bool {
        let paths = self.selected_editable_paths();
        let ops: Vec<CommandOp> = if let Some(tab) = self.active_tab() {
            paths
                .into_iter()
                .map(|(oid, mut path)| {
                    for sp_idx in 0..path.subpaths.len() {
                        let _ = path.auto_close(sp_idx);
                    }
                    Self::build_edit_path_op(&tab.document, oid, path)
                })
                .collect()
        } else {
            Vec::new()
        };
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Closed curve", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: selects all Bézier nodes across currently selected path objects.
    pub fn shape_select_all_nodes(&mut self) -> usize {
        let Some(tab) = self.active_tab_mut() else {
            return 0;
        };
        tab.selection.node_selection.clear();
        for &oid in &tab.selection.objects {
            let Some(obj) = tab.document.object(oid) else {
                continue;
            };
            let NodePayload::Path(ref path) = obj.payload else {
                continue;
            };
            for (sp_idx, sp) in path.subpaths.iter().enumerate() {
                for n_idx in 0..sp.nodes.len() {
                    tab.selection.node_selection.nodes.push(NodeRef {
                        object_id: oid,
                        subpath_index: sp_idx,
                        node_index: n_idx,
                    });
                }
            }
        }
        tab.selection.node_selection.nodes.len()
    }

    /// Shape Property Bar: simplifies / reduces redundant nodes within `tolerance_mm`.
    pub fn shape_reduce_nodes(&mut self, tolerance_mm: f64) -> bool {
        let tol = if tolerance_mm.is_finite() {
            tolerance_mm.clamp(0.01, 50.0)
        } else {
            0.5
        };
        let paths = self.selected_editable_paths();
        let ops: Vec<CommandOp> = if let Some(tab) = self.active_tab() {
            paths
                .into_iter()
                .map(|(oid, mut path)| {
                    path.reduce_nodes(tol);
                    Self::build_edit_path_op(&tab.document, oid, path)
                })
                .collect()
        } else {
            Vec::new()
        };
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.clear();
            }
            self.push_toast(format!("Reduced nodes (tol {tol:.2} mm)"), false);
            true
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // 5 Vector Sculpting Tools (Smooth, Smear, Twirl, AttractRepel, Roughen)
    // -------------------------------------------------------------------------

    /// Applies any of the 5 vector sculpting tools (`Smooth`, `Smear`, `Twirl`, `AttractRepel`,
    /// `Roughen`) along `points` in world mm, auto-converting `ParametricShape` to `BezierPath`
    /// and committing undoably via `CommandOp::EditPath`.
    pub fn apply_sculpt_stroke(
        &mut self,
        tool: ToolId,
        points: &[Point2D],
        shift_invert: bool,
    ) -> bool {
        if points.is_empty() {
            return false;
        }
        let radius_mm = self.tool_settings.sculpt_radius_mm.clamp(1.0, 200.0);
        let strength = self.tool_settings.sculpt_strength.clamp(0.05, 1.0);

        // Locate target vector object: selected first, else hovered within brush radius
        let target_id = self.active_tab().and_then(|tab| {
            let sel = tab.selection.objects.iter().copied().find(|&id| {
                is_object_editable_in_doc(&tab.document, id)
                    && tab.document.object(id).is_some_and(|o| {
                        matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                    })
            });
            if sel.is_some() {
                return sel;
            }
            for &pt in points {
                if let Some(hit) = hit_test_point(
                    &tab.document,
                    tab.document.active_page_index(),
                    pt,
                    radius_mm,
                ) {
                    if is_object_editable_in_doc(&tab.document, hit)
                        && tab.document.object(hit).is_some_and(|o| {
                            matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                        })
                    {
                        return Some(hit);
                    }
                }
            }
            None
        });
        let Some(oid) = target_id else {
            return false;
        };

        let edited_op = self.active_tab().and_then(|tab| {
            if !is_object_editable_in_doc(&tab.document, oid) {
                return None;
            }
            let obj = tab.document.object(oid)?;
            let mut path = match &obj.payload {
                NodePayload::Path(p) => p.clone(),
                NodePayload::Shape(s) => s.to_bezier_path(),
                _ => return None,
            };
            let inv = world_inverse_transform_of(&tab.document, oid);
            let scale_est = ((inv.m[0].hypot(inv.m[1]) + inv.m[2].hypot(inv.m[3])) * 0.5).max(1e-3);
            let local_radius = radius_mm * scale_est;

            for (idx, &pt) in points.iter().enumerate() {
                let lp = inv.transform_point(pt);
                let prev_lp = if idx > 0 {
                    inv.transform_point(points[idx - 1])
                } else {
                    lp
                };
                match tool {
                    ToolId::Smooth => {
                        path.smooth_near(lp, local_radius, strength);
                    }
                    ToolId::Smear => {
                        let delta = if idx > 0 {
                            lp - prev_lp
                        } else if points.len() >= 2 {
                            (inv.transform_point(points[1]) - lp) * 0.5
                        } else {
                            Vec2D::new(local_radius * 0.25, 0.0)
                        };
                        path.smear_near(lp, delta, local_radius);
                    }
                    ToolId::Twirl => {
                        let sign = if shift_invert { -1.0 } else { 1.0 };
                        let angle = sign * (0.45 * strength + 0.15);
                        path.twirl_near(lp, local_radius, angle);
                    }
                    ToolId::AttractRepel => {
                        path.attract_repel_near(lp, local_radius, strength, !shift_invert);
                    }
                    ToolId::Roughen => {
                        let amp = (local_radius * 0.18 * strength).max(0.4);
                        path.roughen_near(
                            lp,
                            local_radius,
                            amp,
                            0x51A7_u64.wrapping_add(idx as u64),
                        );
                    }
                    _ => return None,
                }
            }
            Some(Self::build_edit_path_op(&tab.document, oid, path))
        });

        let Some(cmd) = edited_op else {
            return false;
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.select_single(&tab.document, oid);
            }
            true
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // Real Crop, Knife, Virtual Segment Delete & Vector Eraser
    // -------------------------------------------------------------------------

    /// Performs a REAL undoable crop on the active page: removes objects completely outside
    /// `crop`, translates all remaining active-page objects by `(-crop.min_x, -crop.min_y)`,
    /// and resizes the active page to `(crop.width(), crop.height())`.
    pub fn apply_crop_rect(&mut self, crop: Rect2D) -> bool {
        if !crop.is_finite() || crop.width() < 1.0 || crop.height() < 1.0 {
            return false;
        }
        let plan = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return false;
            };
            let page_idx = tab.document.active_page_index();
            let Some(page) = tab.document.page(page_idx) else {
                return false;
            };
            let page_id = page.id;
            let mut outside_ids = Vec::new();
            let mut inside_ids = Vec::new();
            let mut seen = HashSet::new();
            for layer in &page.layers {
                if layer.locked {
                    continue;
                }
                for &oid in &layer.objects {
                    if !seen.insert(oid) {
                        continue;
                    }
                    if let Some(obj) = tab.document.object(oid) {
                        if obj.locked {
                            continue;
                        }
                    }
                    let intersects = tab
                        .document
                        .world_object_bounds(oid)
                        .is_none_or(|bb| bb.intersects(crop));
                    if intersects {
                        inside_ids.push(oid);
                    } else {
                        outside_ids.push(oid);
                    }
                }
            }

            let mut ops = Vec::new();
            if !outside_ids.is_empty() {
                ops.push(CommandOp::DeleteObjects {
                    ids: outside_ids.clone(),
                });
            }
            if !inside_ids.is_empty() && (crop.min_x.abs() > 1e-6 || crop.min_y.abs() > 1e-6) {
                ops.push(CommandOp::TransformObjects {
                    ids: inside_ids,
                    affine: Affine2D::translate(-crop.min_x, -crop.min_y),
                });
            }
            let new_w = crop.width().clamp(1.0, 50_000.0);
            let new_h = crop.height().clamp(1.0, 50_000.0);
            ops.push(CommandOp::SetPageProperties {
                page_id,
                name: None,
                width_mm: Some(new_w),
                height_mm: Some(new_h),
                bleed_mm: None,
                facing_pages: None,
            });
            (ops, new_w, new_h, outside_ids.len())
        };

        let (ops, w, h, removed) = plan;
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().expect("len == 1")
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_err() {
            return false;
        }
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.pending_crop_rect = None;
            tab.tool_core.pending_crop_rect = None;
            tab.transient_overlay = TransientOverlay::default();
            tab.fit_active_page();
        }
        self.checkpoint_wal_snapshot_active();
        self.push_toast(
            format!("Cropped page to {w:.1} × {h:.1} mm ({removed} outside object(s) removed)"),
            false,
        );
        true
    }

    /// Slices the selected (or line-intersected) vector shape along segment `a -> b`.
    pub fn apply_knife_cut(&mut self, a: Point2D, b: Point2D) -> usize {
        #[allow(clippy::too_many_arguments)]
        fn collect_cuttable_targets(
            doc: &Document,
            id: ObjectId,
            a: Point2D,
            b: Point2D,
            cut_bb: Rect2D,
            depth: usize,
            visiting: &mut HashSet<ObjectId>,
            out: &mut Vec<ObjectId>,
        ) {
            if depth > 64 || !visiting.insert(id) {
                return;
            }
            if !is_object_editable_in_doc(doc, id) {
                return;
            }
            let Some(obj) = doc.object(id) else {
                return;
            };
            match &obj.payload {
                NodePayload::Path(_) | NodePayload::Shape(_) => {
                    if doc
                        .world_object_bounds(id)
                        .is_some_and(|wb| wb.intersects(cut_bb))
                    {
                        out.push(id);
                    }
                }
                NodePayload::Group(children) => {
                    for &cid in children.iter().rev() {
                        collect_cuttable_targets(doc, cid, a, b, cut_bb, depth + 1, visiting, out);
                    }
                }
                NodePayload::PowerClip { contents, .. } => {
                    let Some(pc_wp) = doc.world_path_of(id) else {
                        return;
                    };
                    if !pc_wp.bounding_box().is_some_and(|bb| bb.intersects(cut_bb)) {
                        return;
                    }
                    let enters_pc = pc_wp.contains_point(a)
                        || pc_wp.contains_point(b)
                        || coree_core::knife_cut(&pc_wp, a, b, true).len() > 1
                        || coree_core::knife_cut(&pc_wp, a, b, false).len() > 1;
                    if enters_pc {
                        for &cid in contents.iter().rev() {
                            collect_cuttable_targets(
                                doc,
                                cid,
                                a,
                                b,
                                cut_bb,
                                depth + 1,
                                visiting,
                                out,
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        let cut_bb = Rect2D::from_points(a, b).expand(1.0);
        let knife_plan = self.active_tab().and_then(|tab| {
            let mut visiting = HashSet::new();
            let mut candidates = Vec::new();
            if tab.selection.objects.is_empty() {
                let hits = coree_render::hit_test_rect(
                    &tab.document,
                    tab.document.active_page_index(),
                    cut_bb,
                    false,
                );
                for hit_id in hits {
                    collect_cuttable_targets(
                        &tab.document,
                        hit_id,
                        a,
                        b,
                        cut_bb,
                        0,
                        &mut visiting,
                        &mut candidates,
                    );
                }
            } else {
                for &id in &tab.selection.objects {
                    collect_cuttable_targets(
                        &tab.document,
                        id,
                        a,
                        b,
                        cut_bb,
                        0,
                        &mut visiting,
                        &mut candidates,
                    );
                }
            }

            let mut chosen: Option<(ObjectId, &SceneObject, Vec<BezierPath>)> = None;
            for oid in candidates {
                if !is_object_editable_in_doc(&tab.document, oid) {
                    continue;
                }
                let target_layer_id =
                    enclosing_layer_of_object(&tab.document, oid).or(tab.active_layer_id);
                if target_layer_id
                    .is_some_and(|lid| tab.document.find_layer(lid).is_some_and(|l| l.locked))
                {
                    continue;
                }
                let Some(wp) = tab.document.world_path_of(oid) else {
                    continue;
                };
                let Some(obj) = tab.document.object(oid) else {
                    continue;
                };
                let pieces = coree_core::knife_cut(&wp, a, b, true);
                if pieces.len() > 1 {
                    chosen = Some((oid, obj, pieces));
                    break;
                }
            }

            let (oid, obj, pieces) = chosen?;
            let obj_world = world_transform_of_or_local(&tab.document, oid);
            let (inv_piece_xf, preserved_transform) = if let Some(inv_world) = obj_world.inverse() {
                (inv_world, obj.transform)
            } else {
                let inv_parent = tab
                    .document
                    .parent_container_of(oid)
                    .and_then(|pid| tab.document.world_transform_of(pid))
                    .and_then(|xf| xf.inverse())
                    .unwrap_or(Affine2D::IDENTITY);
                (inv_parent, Affine2D::IDENTITY)
            };

            let mut next_id = tab.document.next_id().max(1);
            let mut new_ids = Vec::with_capacity(pieces.len());
            let mut replacements = Vec::with_capacity(pieces.len());
            for (i, piece) in pieces.into_iter().enumerate() {
                let nid = ObjectId(next_id);
                next_id = next_id.saturating_add(1);
                new_ids.push(nid);
                let local_piece = piece.transformed(&inv_piece_xf);
                let mut new_obj = SceneObject::new(
                    nid,
                    format!("{} Slice {}", obj.name, i + 1),
                    NodePayload::Path(local_piece),
                );
                new_obj.transform = preserved_transform;
                new_obj.fill = obj.fill.clone();
                new_obj.outline = obj.outline.clone();
                new_obj.effects = obj.effects.clone();
                new_obj.lens = obj.lens.clone();
                new_obj.style_id = obj.style_id;
                new_obj.opacity = obj.opacity;
                new_obj.blend_mode = obj.blend_mode;
                new_obj.visible = obj.visible;
                new_obj.printable = obj.printable;
                new_obj.unknown_chunks = obj.unknown_chunks.clone();
                replacements.push(new_obj);
            }
            Some((
                CommandOp::ReplaceObjectInParent {
                    id: oid,
                    replacements,
                },
                new_ids,
            ))
        });

        if let Some((op, new_ids)) = knife_plan {
            let count = new_ids.len();
            if self.execute_command(op).is_ok() {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.select_many(&tab.document, &new_ids);
                    tab.metrics_dirty = true;
                }
                self.push_toast(format!("Knife sliced shape into {count} piece(s)"), false);
                return count;
            }
        }
        0
    }

    /// Deletes the curve segment between intersections nearest to `pt`.
    pub fn apply_virtual_segment_delete_at(&mut self, pt: Point2D) -> bool {
        let edit_op = self.active_tab().and_then(|tab| {
            let tol = (8.0 / tab.viewport.px_per_mm()).max(2.0);
            let oid = hit_test_point(&tab.document, tab.document.active_page_index(), pt, tol)?;
            if !is_object_editable_in_doc(&tab.document, oid) {
                return None;
            }
            if !tab
                .document
                .object(oid)
                .is_some_and(|o| matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_)))
            {
                return None;
            }
            let target_path = tab.document.world_path_of(oid)?;
            let cutters: Vec<BezierPath> = tab
                .document
                .objects()
                .keys()
                .filter(|&&id| id != oid)
                .filter_map(|&id| tab.document.world_path_of(id))
                .collect();
            let updated_world = coree_core::virtual_segment_delete(&target_path, &cutters, pt, tol);
            let inv = world_inverse_transform_of(&tab.document, oid);
            Some(Self::build_edit_path_op(
                &tab.document,
                oid,
                updated_world.transformed(&inv),
            ))
        });
        if let Some(op) = edit_op {
            if self.execute_command(op).is_ok() {
                self.push_toast("Deleted virtual curve segment", false);
                return true;
            }
        }
        false
    }

    /// Vector Eraser: erases a circular swath along `points` from the selected (or touched) vector shape.
    pub fn apply_vector_eraser_stroke(&mut self, points: &[Point2D]) -> bool {
        if points.is_empty() {
            return false;
        }
        let radius = self.tool_settings.eraser_radius_mm.max(0.25);
        let step = (radius * 0.4).max(0.25);
        let edit = self.active_tab().and_then(|tab| {
            let is_editable_vector = |id: ObjectId| {
                tab.document.object(id).is_some_and(|o| {
                    matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                }) && tab.document.world_path_of(id).is_some()
            };
            let oid = tab
                .selection
                .objects
                .iter()
                .copied()
                .find(|&id| is_editable_vector(id))
                .or_else(|| {
                    points.iter().find_map(|&p| {
                        hit_test_point(&tab.document, tab.document.active_page_index(), p, radius)
                            .filter(|&id| is_editable_vector(id))
                    })
                })?;
            if !is_object_editable_in_doc(&tab.document, oid) {
                return None;
            }
            let wp = tab.document.world_path_of(oid)?;
            let mut centers = Vec::with_capacity(points.len());
            centers.push(points[0]);
            for seg in points.windows(2) {
                let dist = seg[0].distance_to(seg[1]);
                let steps = ((dist / step).ceil() as usize).clamp(1, 256);
                for k in 1..=steps {
                    let t = k as f64 / steps as f64;
                    centers.push(seg[0].lerp(seg[1], t));
                }
            }
            let mut pieces = vec![wp];
            for center in centers {
                let mut next_pieces = Vec::new();
                for piece in &pieces {
                    if !piece.is_empty() {
                        next_pieces.extend(coree_core::erase_circle(piece, center, radius));
                    }
                }
                pieces = next_pieces;
                if pieces.is_empty() {
                    break;
                }
            }
            let combined = BezierPath {
                subpaths: pieces.into_iter().flat_map(|p| p.subpaths).collect(),
            };
            let inv = world_inverse_transform_of(&tab.document, oid);
            Some(Self::build_edit_path_op(
                &tab.document,
                oid,
                combined.transformed(&inv),
            ))
        });
        if let Some(op) = edit {
            self.execute_command(op).is_ok()
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // Bezier / Pen, B-Spline, 3-Point Curve, 3-Point Rect/Ellipse & Enter Confirm
    // -------------------------------------------------------------------------

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
    /// `Blend`, `Distort`, `Envelope`, `Extrude25D`) or `Transparency` from an on-canvas drag vector.
    pub fn apply_interactive_effect_drag(
        &mut self,
        tool: ToolId,
        start_mm: Point2D,
        end_mm: Point2D,
    ) -> bool {
        // Resolve primary target object (editable selection first; only fall back to hit-test when selection is empty)
        let target_id = self.active_tab().and_then(|tab| {
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
        let Some(oid) = target_id else {
            self.push_toast(
                "Select or click an object to apply this interactive effect",
                false,
            );
            return false;
        };
        if let Some(tab) = self.active_tab_mut() {
            if !tab.selection.contains(oid) {
                tab.selection.select_single(&tab.document, oid);
            }
        }

        let delta = end_mm - start_mm;
        let dist = delta.length();
        let es = self.effects_state.clone();

        let (existing_idx, new_effect) = match tool {
            ToolId::DropShadow => {
                let idx = self.active_tab().and_then(|t| {
                    t.document
                        .object(oid)?
                        .effects
                        .iter()
                        .position(|e| matches!(e, Effect::DropShadow { .. }))
                });
                let dx = if dist > 0.25 { delta.x } else { es.dx_mm };
                let dy = if dist > 0.25 { delta.y } else { es.dy_mm };
                (
                    idx,
                    Effect::DropShadow {
                        dx_mm: dx,
                        dy_mm: dy,
                        blur_mm: es.blur_mm.max(0.5),
                        opacity: es.opacity.clamp(0.1, 1.0),
                        color: es.color,
                        feather_direction: FeatherDirection::Middle,
                    },
                )
            }
            ToolId::BlockShadow => {
                let idx = self.active_tab().and_then(|t| {
                    t.document
                        .object(oid)?
                        .effects
                        .iter()
                        .position(|e| matches!(e, Effect::BlockShadow { .. }))
                });
                let dx = if dist > 0.25 { delta.x } else { es.dx_mm };
                let dy = if dist > 0.25 { delta.y } else { es.dy_mm };
                (
                    idx,
                    Effect::BlockShadow {
                        dx_mm: dx,
                        dy_mm: dy,
                        color: es.color,
                        simplify: true,
                        overprint: false,
                    },
                )
            }
            ToolId::Contour => {
                let (idx, wb) = self
                    .active_tab()
                    .map(|t| {
                        let i = t.document.object(oid).and_then(|o| {
                            o.effects
                                .iter()
                                .position(|e| matches!(e, Effect::Contour { .. }))
                        });
                        (i, t.document.world_object_bounds(oid))
                    })
                    .unwrap_or((None, None));
                let (mode, offset_mm) = if let Some(bb) = wb {
                    let c = bb.center();
                    let rx = (bb.width() * 0.5).max(1.0);
                    let ry = (bb.height() * 0.5).max(1.0);
                    let rel = ((end_mm.x - c.x) / rx).hypot((end_mm.y - c.y) / ry);
                    if rel < 0.25 {
                        (ContourMode::ToCenter, (dist / 3.0).clamp(0.5, 25.0))
                    } else if bb.contains_point(end_mm) {
                        (ContourMode::Inside, (dist / 2.5).clamp(0.5, 25.0))
                    } else {
                        (ContourMode::Outside, (dist / 2.5).clamp(0.5, 25.0))
                    }
                } else {
                    (es.contour_mode, dist.clamp(0.5, 25.0))
                };
                (
                    idx,
                    Effect::Contour {
                        mode,
                        steps: es.steps.clamp(1, 50),
                        offset_mm,
                        fill_color: es.color,
                        outline_color: Color::BLACK,
                    },
                )
            }
            ToolId::Blend => {
                let (idx, target_b) = self
                    .active_tab()
                    .map(|t| {
                        let i = t.document.object(oid).and_then(|o| {
                            o.effects
                                .iter()
                                .position(|e| matches!(e, Effect::Blend { .. }))
                        });
                        let hit_b = hit_test_point(
                            &t.document,
                            t.document.active_page_index(),
                            end_mm,
                            12.0,
                        )
                        .filter(|&id| id != oid)
                        .or_else(|| t.selection.objects.iter().copied().find(|&id| id != oid))
                        .or_else(|| t.document.objects().keys().copied().find(|&id| id != oid));
                        (i, hit_b)
                    })
                    .unwrap_or((None, None));
                let Some(target_id) = target_b else {
                    self.push_toast("Drag from one object to a second object to Blend", false);
                    return false;
                };
                (
                    idx,
                    Effect::Blend {
                        target_id,
                        steps: es.steps.clamp(1, 200),
                        rotation_deg: 0.0,
                        along_path: None,
                        rotate_along_path: es.rotate_along_path,
                    },
                )
            }
            ToolId::Distort => {
                let (idx, local_center) = self
                    .active_tab()
                    .map(|t| {
                        let obj = t.document.object(oid);
                        let i = obj.and_then(|o| {
                            o.effects
                                .iter()
                                .position(|e| matches!(e, Effect::Distort { .. }))
                        });
                        let inv = world_inverse_transform_of(&t.document, oid);
                        (i, inv.transform_point(start_mm))
                    })
                    .unwrap_or((None, Point2D::ZERO));
                (
                    idx,
                    Effect::Distort {
                        kind: es.distort_kind,
                        amplitude: dist.clamp(2.0, 120.0),
                        frequency: es.frequency.max(0.5),
                        center: local_center,
                    },
                )
            }
            ToolId::Envelope => {
                let Some(idx) = self.ensure_envelope_on_object(oid) else {
                    return false;
                };
                // Warp the nearest cage node toward local end_mm
                let warped = self.active_tab().and_then(|tab| {
                    let obj = tab.document.object(oid)?;
                    let Effect::Envelope { mode, mut cage } = obj.effects.get(idx)?.clone() else {
                        return None;
                    };
                    let inv = world_inverse_transform_of(&tab.document, oid);
                    let ls = inv.transform_point(start_mm);
                    let le = inv.transform_point(end_mm);
                    if let Some((best_i, _)) = cage.iter().enumerate().min_by(|a, b| {
                        a.1.distance_to(ls)
                            .partial_cmp(&b.1.distance_to(ls))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                        cage[best_i] = le;
                    }
                    Some(Effect::Envelope { mode, cage })
                });
                if let Some(eff) = warped {
                    return self.replace_effect_on_object(oid, Some(idx), eff);
                }
                return true;
            }
            ToolId::Extrude25D => {
                let idx = self.active_tab().and_then(|t| {
                    t.document
                        .object(oid)?
                        .effects
                        .iter()
                        .position(|e| matches!(e, Effect::Extrude25D { .. }))
                });
                (
                    idx,
                    Effect::Extrude25D {
                        vanishing_point: end_mm,
                        depth: dist.clamp(2.0, 200.0),
                        bevel_depth: 1.0,
                        shade_color: es.color,
                        light_dir: [0.4, -0.5, 0.75],
                    },
                )
            }
            ToolId::Transparency => {
                let (idx, local_start, local_end) = self
                    .active_tab()
                    .map(|t| {
                        let obj = t.document.object(oid);
                        let i = obj.and_then(|o| {
                            o.effects
                                .iter()
                                .position(|e| matches!(e, Effect::Transparency { .. }))
                        });
                        let inv = world_inverse_transform_of(&t.document, oid);
                        (
                            i,
                            inv.transform_point(start_mm),
                            inv.transform_point(end_mm),
                        )
                    })
                    .unwrap_or((None, start_mm, end_mm));
                let mode = if local_start.distance_to(local_end) < 0.5 {
                    TransparencyMode::Uniform(es.opacity.clamp(0.05, 0.95))
                } else {
                    TransparencyMode::FountainLinear {
                        start: local_start,
                        end: local_end,
                        start_alpha: 1.0,
                        end_alpha: 0.0,
                    }
                };
                (
                    idx,
                    Effect::Transparency {
                        mode,
                        blend_mode: es.blend_mode,
                    },
                )
            }
            _ => return false,
        };

        let ok = self.replace_effect_on_object(oid, existing_idx, new_effect);
        if ok {
            self.push_toast(format!("Applied {}", tool.label()), false);
        }
        ok
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
            self.push_toast("Cannot paint: all layers are locked or hidden", true);
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
        if points.is_empty() {
            return false;
        }
        let Some(oid) = self.ensure_raster_target_at(points[0]) else {
            return false;
        };

        let op = match tool {
            ToolId::BitmapEraser => RasterBrushOp::Eraser,
            ToolId::CloneStamp => RasterBrushOp::CloneStamp,
            ToolId::Smudge => RasterBrushOp::Smudge,
            _ => self.tool_settings.raster_op,
        };
        let r_px = self.tool_settings.brush_radius_px;
        let hard = self.tool_settings.brush_hardness;
        let col = self.tool_settings.brush_color.clone();
        let smudge_str = self.tool_settings.smudge_strength;
        let db_tone = self.tool_settings.dodge_burn_tone;
        let db_exp = self.tool_settings.dodge_burn_exposure;
        let flood_tol = self.tool_settings.flood_tolerance;
        let clone_src_world = self.tool_settings.clone_source_mm;

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
                tab.raster_mask_rect_mm,
            ))
        });
        let Some((mut new_asset, updated_data, w_mm, h_mm, inv_xf, mask_rect_mm)) = prepared else {
            return false;
        };

        let aw = f64::from(new_asset.width);
        let ah = f64::from(new_asset.height);
        let to_px = |p: Point2D| {
            let lp = inv_xf.transform_point(p);
            Point2D::new((lp.x / w_mm) * aw, (lp.y / h_mm) * ah)
        };
        let px_pts: Vec<Point2D> = points.iter().copied().map(to_px).collect();
        let src_origin = clone_src_world
            .map(to_px)
            .or_else(|| {
                px_pts
                    .first()
                    .map(|p| Point2D::new((p.x - 16.0).max(0.0), (p.y - 16.0).max(0.0)))
            })
            .unwrap_or(Point2D::ZERO);

        let active_mask = mask_rect_mm.map(|mr| {
            let p0 = to_px(mr.min());
            let p1 = to_px(mr.max());
            let px_rect = Rect2D::from_points(p0, p1);
            RasterMask::from_rectangle(new_asset.width, new_asset.height, px_rect)
        });
        let orig_before_mask = active_mask.as_ref().map(|_| new_asset.clone());

        match op {
            RasterBrushOp::Paint => {
                if let Some(ref mask) = active_mask {
                    paint_brush_stroke_masked(
                        &mut new_asset,
                        &px_pts,
                        r_px,
                        hard,
                        col,
                        1.0,
                        Some(mask),
                    );
                } else {
                    paint_brush_stroke(&mut new_asset, &px_pts, r_px, hard, col, 1.0);
                }
            }
            RasterBrushOp::Eraser => {
                erase_stroke(&mut new_asset, &px_pts, r_px, hard);
            }
            RasterBrushOp::CloneStamp => {
                clone_stamp_stroke(&mut new_asset, src_origin, &px_pts, r_px, hard);
            }
            RasterBrushOp::Smudge => {
                smudge_stroke(&mut new_asset, &px_pts, r_px, smudge_str);
            }
            RasterBrushOp::Healing => {
                apply_healing_stroke(&mut new_asset, src_origin, &px_pts, r_px, hard);
            }
            RasterBrushOp::Dodge => {
                apply_dodge_burn_stroke(
                    &mut new_asset,
                    &px_pts,
                    r_px,
                    hard,
                    DodgeBurnMode::Dodge,
                    db_tone,
                    db_exp,
                );
            }
            RasterBrushOp::Burn => {
                apply_dodge_burn_stroke(
                    &mut new_asset,
                    &px_pts,
                    r_px,
                    hard,
                    DodgeBurnMode::Burn,
                    db_tone,
                    db_exp,
                );
            }
            RasterBrushOp::FloodFill => {
                if let Some(seed) = px_pts.first() {
                    let max_x = i64::from(new_asset.width).saturating_sub(1).max(0);
                    let max_y = i64::from(new_asset.height).saturating_sub(1).max(0);
                    let sx = (seed.x.round() as i64).clamp(0, max_x) as u32;
                    let sy = (seed.y.round() as i64).clamp(0, max_y) as u32;
                    let _ = raster_flood_fill(&mut new_asset, sx, sy, col, flood_tol);
                }
            }
        }
        if !matches!(op, RasterBrushOp::Paint) {
            if let (Some(mask), Some(orig_asset)) = (&active_mask, &orig_before_mask) {
                for y in 0..new_asset.height {
                    for x in 0..new_asset.width {
                        let m = f32::from(mask.get(x, y)) / 255.0;
                        if m >= 1.0 - 1e-4 {
                            continue;
                        }
                        if let (Some(orig_px), Some(new_px)) = (
                            orig_asset.read_pixel_f32(x, y),
                            new_asset.read_pixel_f32(x, y),
                        ) {
                            let blended = if m <= 1e-4 {
                                orig_px
                            } else {
                                [
                                    orig_px[0] + (new_px[0] - orig_px[0]) * m,
                                    orig_px[1] + (new_px[1] - orig_px[1]) * m,
                                    orig_px[2] + (new_px[2] - orig_px[2]) * m,
                                    orig_px[3] + (new_px[3] - orig_px[3]) * m,
                                ]
                            };
                            if blended != new_px {
                                new_asset.write_pixel_f32(x, y, blended);
                            }
                        }
                    }
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
