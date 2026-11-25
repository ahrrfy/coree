// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

mod draw_effects;
mod raster_guides;
mod shape_nodes;

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
                let mode = self.tool_settings.mask_mode;
                let feather_px = self.tool_settings.mask_feather_px.max(0.0);
                let inverted = self.tool_settings.mask_inverted;
                let shape = match mode {
                    RasterMaskMode::Ellipse => RasterMaskShape::Ellipse(rect),
                    _ => RasterMaskShape::Rectangle(rect),
                };
                if let Some(tab) = self.active_tab_mut() {
                    tab.raster_mask_rect_mm = Some(rect);
                    tab.tool_core.raster_mask_rect_mm = Some(rect);
                    tab.raster_mask = Some(RasterMaskSelection {
                        shape,
                        feather_px,
                        inverted,
                    });
                    tab.needs_rerender = true;
                }
                self.push_toast(
                    format!(
                        "Set {} bitmap mask ({:.1} × {:.1} mm)",
                        mode.label(),
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
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let hits = {
            let Some(tab) = self.active_tab_mut() else {
                return Vec::new();
            };
            let hits = coree_tools::compute_lasso_hits_scoped(
                &tab.document,
                tab.powerclip_edit_id,
                polygon,
            );
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
        };
        self.maybe_fire_selection_change(&before_sel);
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
}
