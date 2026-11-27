// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::state::{world_transform_of_or_local, HandleShape};
use coree_core::{
    Affine2D, Effect, Fill, GradientStop, NodeKind, NodeRef, RasterMaskShape, TransparencyMode,
};
use coree_prod::LaserOperationKind;
use coree_tools::{
    collect_editable_fillable_targets, find_nearest_object_anchor_in_doc,
    interactive_effect_handle_points, parametric_shape_handles,
};
use egui::Shape;

fn paint_dashed_line(
    painter: &egui::Painter,
    a: Pos2,
    b: Pos2,
    dash_px: f32,
    gap_px: f32,
    stroke: Stroke,
) {
    let delta = b - a;
    let total = delta.length();
    if total < 1.0 {
        return;
    }
    let dir = delta / total;
    let step = (dash_px + gap_px).max(2.0);
    let mut dist = 0.0_f32;
    let mut guard = 0;
    while dist < total && guard < 400 {
        let end = (dist + dash_px).min(total);
        painter.line_segment([a + dir * dist, a + dir * end], stroke);
        dist += step;
        guard += 1;
    }
}

fn paint_laser_toolpath_overlay(
    painter: &egui::Painter,
    app: &StudioApp,
    tab: &crate::state::DocumentTab,
    canvas_rect: Rect,
) {
    if !app.show_laser_toolpath_overlay {
        return;
    }
    let Some(ref plan) = app.laser_plan_preview else {
        return;
    };
    let rapid_color = Color32::from_rgb(0, 215, 245);
    let mut prev_world = Point2D::ZERO;

    for (idx, seg) in plan.segments.iter().enumerate() {
        if seg.points.is_empty() || seg.operation == LaserOperationKind::Ignore {
            continue;
        }
        let first_pt = seg.points[0];
        let a = world_to_screen(canvas_rect, tab, prev_world);
        let b = world_to_screen(canvas_rect, tab, first_pt);
        paint_dashed_line(painter, a, b, 5.0, 4.0, Stroke::new(1.1, rapid_color));

        let seg_color = match seg.operation {
            LaserOperationKind::Cut => Color32::from_rgb(235, 64, 52),
            LaserOperationKind::VectorEngrave => Color32::from_rgb(52, 152, 235),
            LaserOperationKind::HatchEngrave => Color32::from_rgb(46, 204, 113),
            LaserOperationKind::Perforate => Color32::from_rgb(243, 156, 18),
            LaserOperationKind::Ignore => Color32::GRAY,
        };
        let mut screen_pts: Vec<Pos2> = seg
            .points
            .iter()
            .map(|p| world_to_screen(canvas_rect, tab, *p))
            .collect();
        if seg.closed && screen_pts.len() >= 2 {
            screen_pts.push(screen_pts[0]);
        }
        if screen_pts.len() >= 2 {
            painter.add(Shape::line(screen_pts, Stroke::new(1.8, seg_color)));
        }

        // Numbered cut-order badge (capped to first 60 segments for crisp readability)
        if idx < 60 {
            let badge_pos = b;
            painter.circle(badge_pos, 7.5, seg_color, Stroke::new(1.0, Color32::WHITE));
            painter.text(
                badge_pos,
                Align2::CENTER_CENTER,
                format!("{}", idx + 1),
                FontId::monospace(8.5),
                Color32::WHITE,
            );
        }

        prev_world = if seg.closed {
            first_pt
        } else {
            *seg.points.last().unwrap_or(&first_pt)
        };
    }
}

pub(super) fn paint_canvas_overlays(ui: &egui::Ui, app: &StudioApp, canvas_rect: Rect) {
    let Some(tab) = app.active_tab() else {
        return;
    };
    let tokens = app.theme.tokens();
    let painter = ui.painter().with_clip_rect(canvas_rect);

    // 0. Live Laser Toolpath Preview Overlay + Active PowerClip Edit Boundary
    paint_laser_toolpath_overlay(&painter, app, tab, canvas_rect);
    if let Some(pc_id) = tab.powerclip_edit_id {
        if let Some(pc_bb) = tab.document.world_object_bounds(pc_id) {
            let a = world_to_screen(canvas_rect, tab, pc_bb.min());
            let b = world_to_screen(canvas_rect, tab, pc_bb.max());
            let pr = Rect::from_two_pos(a, b);
            let dash = Stroke::new(1.6, tokens.accent_secondary);
            paint_dashed_line(&painter, pr.left_top(), pr.right_top(), 6.0, 4.0, dash);
            paint_dashed_line(&painter, pr.right_top(), pr.right_bottom(), 6.0, 4.0, dash);
            paint_dashed_line(
                &painter,
                pr.right_bottom(),
                pr.left_bottom(),
                6.0,
                4.0,
                dash,
            );
            paint_dashed_line(&painter, pr.left_bottom(), pr.left_top(), 6.0, 4.0, dash);
        }
    }

    // 1. Live Dynamic Snap Guidelines
    for g in &tab.active_guides {
        let a = world_to_screen(canvas_rect, tab, g.start);
        let b = world_to_screen(canvas_rect, tab, g.end);
        painter.line_segment([a, b], Stroke::new(1.0, tokens.accent_secondary));
    }

    // 2. Selection Bounding Box + Dual-Click Scale/Move vs Rotate/Skew Handles + Pivot Crosshair
    if let Some(bb) = tab.selection.bounding_box(&tab.document) {
        let min_s = world_to_screen(canvas_rect, tab, bb.min());
        let max_s = world_to_screen(canvas_rect, tab, bb.max());
        let sel_rect = Rect::from_two_pos(min_s, max_s);

        painter.rect_stroke(
            sel_rect,
            CornerRadius::ZERO,
            Stroke::new(1.2, tokens.accent),
            StrokeKind::Outside,
        );

        if app.active_tool != ToolId::ShapeNode {
            let handles = selection_handle_positions(canvas_rect, tab, bb);
            match tab.selection.transform_mode {
                TransformMode::ScaleAndMove => {
                    for (_, pos) in handles {
                        painter.rect(
                            Rect::from_center_size(pos, vec2(7.0, 7.0)),
                            CornerRadius::ZERO,
                            Color32::WHITE,
                            Stroke::new(1.5, tokens.accent),
                            StrokeKind::Inside,
                        );
                    }
                }
                TransformMode::RotateAndSkew => {
                    for (h, pos) in handles {
                        let is_corner = matches!(
                            h,
                            ScaleHandle::TopLeft
                                | ScaleHandle::TopRight
                                | ScaleHandle::BottomLeft
                                | ScaleHandle::BottomRight
                        );
                        if is_corner {
                            painter.circle(
                                pos,
                                5.0,
                                tokens.surface_elevated,
                                Stroke::new(1.5, tokens.accent_secondary),
                            );
                        } else {
                            painter.rect(
                                Rect::from_center_size(pos, vec2(8.0, 5.0)),
                                CornerRadius::same(1),
                                tokens.accent_secondary,
                                Stroke::new(1.0, tokens.on_accent_secondary),
                                StrokeKind::Inside,
                            );
                        }
                    }
                    let pv = world_to_screen(canvas_rect, tab, tab.selection.pivot);
                    painter.circle_stroke(pv, 6.0, Stroke::new(1.5, tokens.accent_secondary));
                    painter.line_segment(
                        [pv - vec2(9.0, 0.0), pv + vec2(9.0, 0.0)],
                        Stroke::new(1.2, tokens.accent_secondary),
                    );
                    painter.line_segment(
                        [pv - vec2(0.0, 9.0), pv + vec2(0.0, 9.0)],
                        Stroke::new(1.2, tokens.accent_secondary),
                    );
                }
            }
        }
    }

    // 3. `F10` Shape / Node Editing Overlay (Viewport-Culled + Screen-Cell LOD for 40k–200k+ Node Paths)
    if app.active_tool == ToolId::ShapeNode {
        let cull_rect = canvas_rect.expand(16.0);
        let node_budget = app.resource_budget.node_overlay_budget.max(400);
        let mut unselected_visible_total = 0usize;
        for &oid in &tab.selection.objects {
            if let Some(obj) = tab.document.object(oid) {
                if let NodePayload::Path(p) = &obj.payload {
                    let world_xf = world_transform_of_or_local(&tab.document, oid);
                    for sp in &p.subpaths {
                        for node in &sp.nodes {
                            let sp_pt = world_to_screen(
                                canvas_rect,
                                tab,
                                world_xf.transform_point(node.pos),
                            );
                            if cull_rect.contains(sp_pt) {
                                unselected_visible_total += 1;
                            }
                        }
                    }
                }
            }
        }
        let draw_unselected_handles = unselected_visible_total <= 240;
        let use_cell_decimation = unselected_visible_total > node_budget;
        let mut occupied_cells: std::collections::HashSet<(i32, i32)> = if use_cell_decimation {
            std::collections::HashSet::with_capacity(node_budget.min(8192))
        } else {
            std::collections::HashSet::new()
        };
        let mut drawn_unselected = 0usize;
        let mut any_decimated = false;

        for &oid in &tab.selection.objects {
            let Some(obj) = tab.document.object(oid) else {
                continue;
            };
            let world_xf = world_transform_of_or_local(&tab.document, oid);
            match &obj.payload {
                NodePayload::Shape(shape) => {
                    for (_, wpt) in parametric_shape_handles(shape, &world_xf) {
                        let p = world_to_screen(canvas_rect, tab, wpt);
                        if !cull_rect.contains(p) {
                            continue;
                        }
                        let diamond = vec![
                            p + vec2(0.0, -6.0),
                            p + vec2(6.0, 0.0),
                            p + vec2(0.0, 6.0),
                            p + vec2(-6.0, 0.0),
                        ];
                        painter.add(Shape::convex_polygon(
                            diamond,
                            Color32::WHITE,
                            Stroke::new(1.6, tokens.accent_secondary),
                        ));
                    }
                }
                NodePayload::Path(p) => {
                    for (sp_idx, sp) in p.subpaths.iter().enumerate() {
                        for (n_idx, node) in sp.nodes.iter().enumerate() {
                            let nref = NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: n_idx,
                            };
                            let selected = tab.selection.node_selection.contains(nref);
                            let a_pos = world_to_screen(
                                canvas_rect,
                                tab,
                                world_xf.transform_point(node.pos),
                            );
                            if !selected && !cull_rect.contains(a_pos) {
                                continue;
                            }
                            if !selected && use_cell_decimation {
                                if drawn_unselected >= node_budget {
                                    any_decimated = true;
                                    continue;
                                }
                                let cell = ((a_pos.x * 0.2).floor() as i32, (a_pos.y * 0.2).floor() as i32);
                                if !occupied_cells.insert(cell) {
                                    any_decimated = true;
                                    continue;
                                }
                            }
                            if selected || draw_unselected_handles {
                                for h in [node.handle_in, node.handle_out].into_iter().flatten() {
                                    let h_pos = world_to_screen(
                                        canvas_rect,
                                        tab,
                                        world_xf.transform_point(h),
                                    );
                                    painter.line_segment(
                                        [a_pos, h_pos],
                                        Stroke::new(1.0, tokens.accent),
                                    );
                                    painter.circle(
                                        h_pos,
                                        3.5,
                                        tokens.accent,
                                        Stroke::new(1.0, Color32::WHITE),
                                    );
                                }
                            }
                            let fill = if selected {
                                tokens.accent
                            } else {
                                drawn_unselected += 1;
                                Color32::WHITE
                            };
                            if node.kind == NodeKind::Cusp {
                                painter.rect(
                                    Rect::from_center_size(a_pos, vec2(7.0, 7.0)),
                                    CornerRadius::ZERO,
                                    fill,
                                    Stroke::new(1.5, tokens.accent),
                                    StrokeKind::Inside,
                                );
                            } else {
                                painter.circle(a_pos, 4.0, fill, Stroke::new(1.5, tokens.accent));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if any_decimated || (!draw_unselected_handles && unselected_visible_total > 240) {
            let banner = crate::i18n::I18nKey::CanvasNodesLodBanner.tr_ui(app.ui_language);
            let pill_rect = Rect::from_min_size(
                pos2(canvas_rect.left() + 12.0, canvas_rect.bottom() - 32.0),
                vec2(310.0, 22.0),
            );
            painter.rect(
                pill_rect,
                CornerRadius::same(6),
                tokens.surface_elevated.gamma_multiply(0.92),
                Stroke::new(1.0, tokens.border),
                StrokeKind::Inside,
            );
            painter.text(
                pill_rect.center(),
                Align2::CENTER_CENTER,
                banner,
                FontId::proportional(10.5),
                tokens.text_muted,
            );
        }
    }

    // 3b. Selected Canvas Guide Highlight & Delete cue
    if let Some(sel_gid) = tab.selected_guide_id {
        if let Some(g) = tab.document.guide(sel_gid) {
            let guide_stroke = Stroke::new(1.6, tokens.danger);
            match g.orientation {
                GuideOrientation::Horizontal(y_mm) => {
                    let sy = world_to_screen(canvas_rect, tab, Point2D::new(0.0, y_mm)).y;
                    if sy >= canvas_rect.top() && sy <= canvas_rect.bottom() {
                        painter.line_segment(
                            [pos2(canvas_rect.left(), sy), pos2(canvas_rect.right(), sy)],
                            guide_stroke,
                        );
                        painter.rect_filled(
                            Rect::from_center_size(
                                pos2(canvas_rect.left() + 8.0, sy),
                                vec2(8.0, 8.0),
                            ),
                            CornerRadius::same(1),
                            tokens.danger,
                        );
                    }
                }
                GuideOrientation::Vertical(x_mm) => {
                    let sx = world_to_screen(canvas_rect, tab, Point2D::new(x_mm, 0.0)).x;
                    if sx >= canvas_rect.left() && sx <= canvas_rect.right() {
                        painter.line_segment(
                            [pos2(sx, canvas_rect.top()), pos2(sx, canvas_rect.bottom())],
                            guide_stroke,
                        );
                        painter.rect_filled(
                            Rect::from_center_size(
                                pos2(sx, canvas_rect.top() + 8.0),
                                vec2(8.0, 8.0),
                            ),
                            CornerRadius::same(1),
                            tokens.danger,
                        );
                    }
                }
                GuideOrientation::Angled { origin, angle_deg } => {
                    let rad = angle_deg.to_radians();
                    let p0 = world_to_screen(canvas_rect, tab, origin);
                    let dir = vec2(rad.cos() as f32, rad.sin() as f32) * 2000.0;
                    painter.line_segment([p0 - dir, p0 + dir], guide_stroke);
                }
            }
        }
    }

    // 4. Interactive Fill (all 4 types + mid-stops), Transparency, Persistent Effect Handles, Envelope & Mesh Overlays
    if app.active_tool == ToolId::InteractiveFill && !tab.selection.objects.is_empty() {
        let active_fill_drag_oid = match &tab.tool_core.drag_state {
            ActiveDragState::DraggingGradientHandle { object_id, .. }
            | ActiveDragState::DraggingGradientFocal { object_id, .. }
            | ActiveDragState::DraggingGradientStop { object_id, .. } => Some(*object_id),
            _ => None,
        };
        for oid in collect_editable_fillable_targets(&tab.document, &tab.selection.objects) {
            if Some(oid) == active_fill_drag_oid {
                continue;
            }
            if let Some(obj) = tab.document.object(oid) {
                let world_xf = world_transform_of_or_local(&tab.document, oid);
                match &obj.fill {
                    Fill::LinearGradient { start, end, stops } => {
                        let a = world_to_screen(canvas_rect, tab, world_xf.transform_point(*start));
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(*end));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *start,
                            *end,
                            stops,
                        );
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    Fill::RadialGradient {
                        center,
                        radius,
                        focal,
                        stops,
                    } => {
                        let a =
                            world_to_screen(canvas_rect, tab, world_xf.transform_point(*center));
                        let r_pt = Point2D::new(center.x + radius.max(1.0), center.y);
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(r_pt));
                        let r_px = a.distance(b).max(2.0);
                        painter.circle_stroke(a, r_px, Stroke::new(1.2, tokens.accent));
                        painter.line_segment([a, b], Stroke::new(1.8, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *center,
                            r_pt,
                            stops,
                        );
                        if (*focal - *center).length() > 0.2 {
                            let f =
                                world_to_screen(canvas_rect, tab, world_xf.transform_point(*focal));
                            paint_dashed_line(
                                &painter,
                                a,
                                f,
                                4.0,
                                3.0,
                                Stroke::new(1.2, tokens.accent_secondary),
                            );
                            painter.circle(
                                f,
                                4.5,
                                tokens.accent_secondary,
                                Stroke::new(1.5, Color32::WHITE),
                            );
                        }
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    Fill::ConicalGradient {
                        center,
                        angle_deg,
                        stops,
                    } => {
                        let a =
                            world_to_screen(canvas_rect, tab, world_xf.transform_point(*center));
                        let rad = angle_deg.to_radians();
                        let e_pt =
                            Point2D::new(center.x + 25.0 * rad.cos(), center.y + 25.0 * rad.sin());
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(e_pt));
                        painter.circle_stroke(
                            a,
                            a.distance(b).max(16.0),
                            Stroke::new(1.2, tokens.accent),
                        );
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *center,
                            e_pt,
                            stops,
                        );
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    Fill::SquareGradient {
                        center,
                        size,
                        angle_deg,
                        stops,
                    } => {
                        let a =
                            world_to_screen(canvas_rect, tab, world_xf.transform_point(*center));
                        let rad = angle_deg.to_radians();
                        let half = size.max(2.0) * 0.5;
                        let e_pt =
                            Point2D::new(center.x + half * rad.cos(), center.y + half * rad.sin());
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(e_pt));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *center,
                            e_pt,
                            stops,
                        );
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    _ => {}
                }
            }
        }
    } else if app.active_tool == ToolId::MeshFill && !tab.selection.objects.is_empty() {
        let active_mesh_drag_oid = match &tab.tool_core.drag_state {
            ActiveDragState::DraggingMeshNode { object_id, .. }
            | ActiveDragState::DraggingMeshHandle { object_id, .. } => Some(*object_id),
            _ => None,
        };
        for oid in collect_editable_fillable_targets(&tab.document, &tab.selection.objects) {
            if Some(oid) == active_mesh_drag_oid {
                continue;
            }
            if let Some(obj) = tab.document.object(oid) {
                let world_xf = world_transform_of_or_local(&tab.document, oid);
                if let Fill::MeshFill { rows, cols, nodes } = &obj.fill {
                    let spts: Vec<Pos2> = nodes
                        .iter()
                        .map(|n| world_to_screen(canvas_rect, tab, world_xf.transform_point(n.pos)))
                        .collect();
                    for r in 0..*rows {
                        for c in 0..*cols {
                            let idx = r * cols + c;
                            if c + 1 < *cols && idx + 1 < spts.len() {
                                painter.line_segment(
                                    [spts[idx], spts[idx + 1]],
                                    Stroke::new(1.0, tokens.accent.gamma_multiply(0.8)),
                                );
                            }
                            if r + 1 < *rows && idx + cols < spts.len() {
                                painter.line_segment(
                                    [spts[idx], spts[idx + cols]],
                                    Stroke::new(1.0, tokens.accent.gamma_multiply(0.8)),
                                );
                            }
                        }
                    }
                    for (idx, (mn, &sp)) in nodes.iter().zip(spts.iter()).enumerate() {
                        let is_sel = tab.selected_mesh_node == Some((oid, idx));
                        if is_sel {
                            for rel in [mn.handle_n, mn.handle_s, mn.handle_e, mn.handle_w]
                                .into_iter()
                                .flatten()
                            {
                                let h_world = world_xf.transform_point(Point2D::new(
                                    mn.pos.x + rel.x,
                                    mn.pos.y + rel.y,
                                ));
                                let hsp = world_to_screen(canvas_rect, tab, h_world);
                                painter.line_segment(
                                    [sp, hsp],
                                    Stroke::new(1.2, tokens.accent_secondary),
                                );
                                let diamond = vec![
                                    hsp + vec2(0.0, -5.0),
                                    hsp + vec2(5.0, 0.0),
                                    hsp + vec2(0.0, 5.0),
                                    hsp + vec2(-5.0, 0.0),
                                ];
                                painter.add(Shape::convex_polygon(
                                    diamond,
                                    Color32::WHITE,
                                    Stroke::new(1.4, tokens.accent_secondary),
                                ));
                            }
                        }
                        painter.circle(
                            sp,
                            if is_sel { 5.5 } else { 4.2 },
                            core_color_to_egui(&mn.color),
                            Stroke::new(
                                if is_sel { 2.0 } else { 1.3 },
                                if is_sel {
                                    tokens.accent_secondary
                                } else {
                                    Color32::WHITE
                                },
                            ),
                        );
                    }
                }
            }
        }
    } else if let Some(&oid) = tab.selection.objects.first() {
        if let Some(obj) = tab.document.object(oid) {
            let world_xf = world_transform_of_or_local(&tab.document, oid);
            if matches!(
                app.active_tool,
                ToolId::DropShadow
                    | ToolId::BlockShadow
                    | ToolId::Contour
                    | ToolId::Extrude25D
                    | ToolId::Blend
                    | ToolId::Distort
            ) {
                for eff in &obj.effects {
                    if let Some((anchor_w, knob_w)) =
                        interactive_effect_handle_points(&tab.document, obj, eff, app.active_tool)
                    {
                        let sa = world_to_screen(canvas_rect, tab, anchor_w);
                        let sb = world_to_screen(canvas_rect, tab, knob_w);
                        paint_dashed_line(
                            &painter,
                            sa,
                            sb,
                            5.0,
                            3.0,
                            Stroke::new(1.5, tokens.accent_secondary),
                        );
                        painter.rect(
                            Rect::from_center_size(sa, vec2(7.0, 7.0)),
                            CornerRadius::same(1),
                            Color32::WHITE,
                            Stroke::new(1.5, tokens.accent_secondary),
                            StrokeKind::Inside,
                        );
                        painter.circle(
                            sb,
                            5.5,
                            tokens.accent_secondary,
                            Stroke::new(1.5, Color32::WHITE),
                        );
                    }
                }
            }
            if app.active_tool == ToolId::Transparency {
                for eff in &obj.effects {
                    if let Effect::Transparency {
                        mode: TransparencyMode::FountainLinear { start, end, .. },
                        ..
                    } = eff
                    {
                        let a = world_to_screen(canvas_rect, tab, world_xf.transform_point(*start));
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(*end));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent_secondary));
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(1),
                            Color32::WHITE,
                            Stroke::new(1.5, Color32::BLACK),
                            StrokeKind::Inside,
                        );
                        painter.rect(
                            Rect::from_center_size(b, vec2(9.0, 9.0)),
                            CornerRadius::same(1),
                            Color32::BLACK,
                            Stroke::new(1.5, Color32::WHITE),
                            StrokeKind::Inside,
                        );
                    }
                }
            }
            if app.active_tool == ToolId::Envelope {
                for eff in &obj.effects {
                    if let Effect::Envelope { cage, .. } = eff {
                        if cage.len() >= 4 {
                            let spts: Vec<Pos2> = cage
                                .iter()
                                .map(|&p| {
                                    world_to_screen(canvas_rect, tab, world_xf.transform_point(p))
                                })
                                .collect();
                            for i in 0..spts.len() {
                                paint_dashed_line(
                                    &painter,
                                    spts[i],
                                    spts[(i + 1) % spts.len()],
                                    4.0,
                                    3.0,
                                    Stroke::new(1.3, tokens.cutcontour),
                                );
                            }
                            for &p in &spts {
                                painter.rect(
                                    Rect::from_center_size(p, vec2(8.0, 8.0)),
                                    CornerRadius::same(1),
                                    Color32::WHITE,
                                    Stroke::new(1.5, tokens.cutcontour),
                                    StrokeKind::Inside,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // 5. Pending Interactive Crop Overlay (Darkened Mask Outside + Draggable Handles)
    if let Some(crop) = tab.pending_crop_rect {
        let cmin = world_to_screen(canvas_rect, tab, crop.min());
        let cmax = world_to_screen(canvas_rect, tab, crop.max());
        let cr = Rect::from_two_pos(cmin, cmax).intersect(canvas_rect);
        let mask_col = Color32::from_rgba_unmultiplied(0, 0, 0, 128);
        painter.rect_filled(
            Rect::from_min_max(canvas_rect.min, pos2(canvas_rect.right(), cr.top())),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(canvas_rect.left(), cr.bottom()), canvas_rect.max),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_filled(
            Rect::from_min_max(
                pos2(canvas_rect.left(), cr.top()),
                pos2(cr.left(), cr.bottom()),
            ),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_filled(
            Rect::from_min_max(
                pos2(cr.right(), cr.top()),
                pos2(canvas_rect.right(), cr.bottom()),
            ),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_stroke(
            cr,
            CornerRadius::ZERO,
            Stroke::new(1.8, tokens.accent_secondary),
            StrokeKind::Outside,
        );
        for (_, hpos) in selection_handle_positions(canvas_rect, tab, crop) {
            painter.rect(
                Rect::from_center_size(hpos, vec2(8.0, 8.0)),
                CornerRadius::ZERO,
                Color32::WHITE,
                Stroke::new(1.5, tokens.accent_secondary),
                StrokeKind::Inside,
            );
        }
        painter.text(
            pos2(
                cr.center().x,
                (cr.top() - 12.0).max(canvas_rect.top() + 10.0),
            ),
            Align2::CENTER_CENTER,
            format!(
                "Crop {:.1} × {:.1} mm · Enter / Double-Click to apply · Esc to cancel",
                crop.width(),
                crop.height()
            ),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
    }

    // 6. Active Bitmap Mask Selection Overlay (`MaskSelect` — Rect, Ellipse, Lasso, Magic Wand)
    if let Some(ref mask) = tab.raster_mask {
        let stroke = Stroke::new(1.4, Color32::WHITE);
        match &mask.shape {
            RasterMaskShape::Rectangle(mrect) => {
                let a = world_to_screen(canvas_rect, tab, mrect.min());
                let b = world_to_screen(canvas_rect, tab, mrect.max());
                let r = Rect::from_two_pos(a, b);
                paint_dashed_line(&painter, r.left_top(), r.right_top(), 5.0, 4.0, stroke);
                paint_dashed_line(&painter, r.right_top(), r.right_bottom(), 5.0, 4.0, stroke);
                paint_dashed_line(
                    &painter,
                    r.right_bottom(),
                    r.left_bottom(),
                    5.0,
                    4.0,
                    stroke,
                );
                paint_dashed_line(&painter, r.left_bottom(), r.left_top(), 5.0, 4.0, stroke);
            }
            RasterMaskShape::Ellipse(mrect) => {
                let c = mrect.center();
                let rx = mrect.width() * 0.5;
                let ry = mrect.height() * 0.5;
                let steps = 36usize;
                for i in 0..steps {
                    let t0 = (i as f64 / steps as f64) * std::f64::consts::TAU;
                    let t1 = ((i + 1) as f64 / steps as f64) * std::f64::consts::TAU;
                    let p0 = world_to_screen(
                        canvas_rect,
                        tab,
                        Point2D::new(c.x + rx * t0.cos(), c.y + ry * t0.sin()),
                    );
                    let p1 = world_to_screen(
                        canvas_rect,
                        tab,
                        Point2D::new(c.x + rx * t1.cos(), c.y + ry * t1.sin()),
                    );
                    paint_dashed_line(&painter, p0, p1, 4.0, 3.0, stroke);
                }
            }
            RasterMaskShape::Lasso(pts) if pts.len() >= 2 => {
                for i in 0..pts.len() {
                    let p0 = world_to_screen(canvas_rect, tab, pts[i]);
                    let p1 = world_to_screen(canvas_rect, tab, pts[(i + 1) % pts.len()]);
                    paint_dashed_line(&painter, p0, p1, 4.0, 3.0, stroke);
                }
            }
            RasterMaskShape::MagicWand { seed_mm, .. } => {
                let sp = world_to_screen(canvas_rect, tab, *seed_mm);
                painter.circle_stroke(sp, 6.0, stroke);
                painter.line_segment([pos2(sp.x - 8.0, sp.y), pos2(sp.x + 8.0, sp.y)], stroke);
                painter.line_segment([pos2(sp.x, sp.y - 8.0), pos2(sp.x, sp.y + 8.0)], stroke);
            }
            _ => {}
        }
    } else if let Some(mrect) = tab.raster_mask_rect_mm {
        let a = world_to_screen(canvas_rect, tab, mrect.min());
        let b = world_to_screen(canvas_rect, tab, mrect.max());
        let r = Rect::from_two_pos(a, b);
        paint_dashed_line(
            &painter,
            r.left_top(),
            r.right_top(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
        paint_dashed_line(
            &painter,
            r.right_top(),
            r.right_bottom(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
        paint_dashed_line(
            &painter,
            r.right_bottom(),
            r.left_bottom(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
        paint_dashed_line(
            &painter,
            r.left_bottom(),
            r.left_top(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
    }

    // 7. Layer 3 ToolCore Transient Overlay (previews, transformed outlines, marquees, drafts, guides)
    paint_transient_overlay(&painter, canvas_rect, tab, &tab.transient_overlay, &tokens);

    // 11. Live Circular Brush Cursor (All 5 Sculpt Tools, Vector Eraser & Bitmap Studio Brushes)
    if let Some(hover) = ui.input(|i| i.pointer.hover_pos()) {
        if canvas_rect.contains(hover) {
            let brush_radius_px = match app.active_tool {
                ToolId::Smooth
                | ToolId::Smear
                | ToolId::Twirl
                | ToolId::AttractRepel
                | ToolId::Roughen => Some(
                    (app.tool_settings.sculpt_radius_mm * tab.viewport.logical_px_per_mm()) as f32,
                ),
                ToolId::Eraser => Some(
                    (app.tool_settings.eraser_radius_mm * tab.viewport.logical_px_per_mm()) as f32,
                ),
                ToolId::BitmapBrush
                | ToolId::BitmapEraser
                | ToolId::CloneStamp
                | ToolId::Smudge => Some(app.tool_settings.brush_radius_px),
                _ => None,
            };
            if let Some(r_px) = brush_radius_px {
                let clamped = r_px.clamp(4.0, 220.0);
                painter.circle_stroke(hover, clamped, Stroke::new(1.3, tokens.accent_secondary));
                painter.circle_stroke(
                    hover,
                    (clamped - 1.2).max(2.0),
                    Stroke::new(0.8, Color32::WHITE.gamma_multiply(0.75)),
                );
            }
            // Eyedropper Sample/Apply mode pill near cursor
            if matches!(
                app.active_tool,
                ToolId::ColorEyedropper | ToolId::AttributesEyedropper
            ) {
                let lbl = if app.tool_settings.eyedropper_apply_mode {
                    "Apply"
                } else {
                    "Sample"
                };
                painter.text(
                    hover + vec2(14.0, -12.0),
                    Align2::LEFT_CENTER,
                    lbl,
                    FontId::monospace(10.0),
                    tokens.accent_secondary,
                );
            }
            // Connector snap target ring highlight
            if matches!(
                app.active_tool,
                ToolId::StraightConnector | ToolId::RightAngleConnector
            ) {
                if let Some((_, _, snap_pt)) = find_nearest_object_anchor_in_doc(
                    &tab.document,
                    tab.document.active_page_index(),
                    tab.powerclip_edit_id,
                    app.treat_all_objects_as_filled,
                    tab.cursor_world_mm,
                    8.0,
                ) {
                    let sp = world_to_screen(canvas_rect, tab, snap_pt);
                    painter.circle_stroke(sp, 6.0, Stroke::new(2.0, tokens.accent_secondary));
                }
            }
        }
    }
}

// =============================================================================
// Calibrated Top & Left 20px Rulers
// =============================================================================

pub(super) fn paint_rulers(
    ui: &egui::Ui,
    app: &StudioApp,
    corner: Rect,
    top: Rect,
    left: Rect,
    canvas_rect: Rect,
) {
    let tokens = app.theme.tokens();
    let painter = ui.painter();

    painter.rect(
        corner,
        CornerRadius::ZERO,
        tokens.surface,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );
    painter.rect(
        top,
        CornerRadius::ZERO,
        tokens.surface,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );
    painter.rect(
        left,
        CornerRadius::ZERO,
        tokens.surface,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );

    let Some(tab) = app.active_tab() else {
        return;
    };
    let u = tab.document.default_unit();
    let sf = tab.document.drawing_scale().scale_factor();
    painter.text(
        corner.center(),
        Align2::CENTER_CENTER,
        u.suffix(),
        FontId::monospace(8.5),
        tokens.text_muted,
    );

    let px_per_mm = tab.viewport.logical_px_per_mm().max(0.05);
    // Choose a clean human tick step in mm based on current zoom
    let step_mm = if px_per_mm >= 8.0 {
        5.0
    } else if px_per_mm >= 3.0 {
        10.0
    } else if px_per_mm >= 1.2 {
        20.0
    } else if px_per_mm >= 0.5 {
        50.0
    } else {
        100.0
    };

    let vis = tab.viewport.visible_world_rect();

    // Horizontal (top) ruler ticks
    let top_painter = painter.with_clip_rect(top);
    let start_x = (vis.min_x / step_mm).floor() * step_mm;
    let mut x = start_x;
    let mut guard = 0;
    while x <= vis.max_x + step_mm && guard < 300 {
        let sx = world_to_screen(canvas_rect, tab, Point2D::new(x, 0.0)).x;
        if sx >= top.left() && sx <= top.right() {
            top_painter.line_segment(
                [pos2(sx, top.bottom() - 7.0), pos2(sx, top.bottom())],
                Stroke::new(1.0, tokens.border_input),
            );
            let val = u.from_mm(x * sf, f64::from(tab.document.dpi()));
            top_painter.text(
                pos2(sx + 2.0, top.top() + 2.0),
                Align2::LEFT_TOP,
                format!("{val:.0}"),
                FontId::monospace(9.0),
                tokens.text_muted,
            );
        }
        x += step_mm;
        guard += 1;
    }

    // Vertical (left) ruler ticks
    let left_painter = painter.with_clip_rect(left);
    let start_y = (vis.min_y / step_mm).floor() * step_mm;
    let mut y = start_y;
    guard = 0;
    while y <= vis.max_y + step_mm && guard < 300 {
        let sy = world_to_screen(canvas_rect, tab, Point2D::new(0.0, y)).y;
        if sy >= left.top() && sy <= left.bottom() {
            left_painter.line_segment(
                [pos2(left.right() - 7.0, sy), pos2(left.right(), sy)],
                Stroke::new(1.0, tokens.border_input),
            );
            let val = u.from_mm(y * sf, f64::from(tab.document.dpi()));
            left_painter.text(
                pos2(left.left() + 1.0, sy + 2.0),
                Align2::LEFT_TOP,
                format!("{val:.0}"),
                FontId::monospace(8.0),
                tokens.text_muted,
            );
        }
        y += step_mm;
        guard += 1;
    }
}

pub(super) fn paint_toasts(ui: &egui::Ui, app: &StudioApp, canvas_rect: Rect) {
    if app.toasts.is_empty() {
        return;
    }
    let tokens = app.theme.tokens();
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(app.ui_language);
    let painter = ui.painter();
    let mut y = canvas_rect.top() + 10.0;
    for toast in app.toasts.iter().rev().take(3) {
        let accent = if toast.is_error {
            tokens.danger
        } else {
            tokens.accent
        };
        let galley = painter.layout_no_wrap(
            toast.message.clone(),
            FontId::proportional(11.5),
            tokens.text,
        );
        let box_w = galley.size().x + 20.0;
        let box_h = galley.size().y + 10.0;
        let toast_x = if shell_dir.is_rtl {
            (canvas_rect.left() + 12.0)
                .min((canvas_rect.right() - box_w - 8.0).max(canvas_rect.left()))
        } else {
            (canvas_rect.right() - box_w - 12.0).max(canvas_rect.left() + 8.0)
        };
        let rect = Rect::from_min_size(pos2(toast_x, y), vec2(box_w, box_h));
        painter.rect(
            rect,
            CornerRadius::same(6),
            tokens.surface_elevated,
            Stroke::new(1.0, accent),
            StrokeKind::Inside,
        );
        painter.galley(rect.left_top() + vec2(10.0, 5.0), galley, tokens.text);
        y += box_h + 6.0;
    }
}

pub(super) fn paint_transient_overlay(
    painter: &egui::Painter,
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    overlay: &TransientOverlay,
    tokens: &crate::theme::ThemeTokens,
) {
    for g in &overlay.active_guides {
        let a = world_to_screen(canvas_rect, tab, g.start);
        let b = world_to_screen(canvas_rect, tab, g.end);
        painter.line_segment([a, b], Stroke::new(1.0, tokens.accent_secondary));
    }

    for p in &overlay.preview_paths {
        let (width, col) = match p.stroke_kind {
            OverlayStrokeKind::Accent => (1.6, tokens.accent),
            OverlayStrokeKind::Secondary => (1.6, tokens.accent_secondary),
            OverlayStrokeKind::Danger => (1.8, tokens.danger),
            OverlayStrokeKind::Guide => (1.2, tokens.accent),
            OverlayStrokeKind::MarqueeFill => (1.1, tokens.accent),
            OverlayStrokeKind::CropMask => (1.5, tokens.accent_secondary),
            OverlayStrokeKind::DashedGuide => (1.2, tokens.accent_secondary),
        };
        let stroke = Stroke::new(width, col);
        for poly in p.path.flatten(0.4) {
            let spts: Vec<Pos2> = poly
                .into_iter()
                .map(|pt| world_to_screen(canvas_rect, tab, pt))
                .collect();
            if spts.len() < 2 {
                continue;
            }
            if p.closed_fill && spts.len() >= 3 {
                let fill = col.gamma_multiply(0.14);
                if spts.len() <= 5 {
                    painter.add(Shape::convex_polygon(spts, fill, stroke));
                } else {
                    painter.add(Shape::closed_line(spts, stroke));
                }
            } else if p.stroke_kind == OverlayStrokeKind::DashedGuide {
                for seg in spts.windows(2) {
                    paint_dashed_line(painter, seg[0], seg[1], 5.0, 4.0, stroke);
                }
            } else {
                painter.add(Shape::line(spts, stroke));
            }
        }
    }

    for h in &overlay.preview_handles {
        let sp = world_to_screen(canvas_rect, tab, h.pos_mm);
        let col = h
            .color_hint
            .as_ref()
            .map_or(tokens.accent, core_color_to_egui);
        let fill = if h.selected { col } else { Color32::WHITE };
        match h.shape {
            HandleShape::Square => {
                painter.rect(
                    Rect::from_center_size(sp, vec2(7.0, 7.0)),
                    CornerRadius::ZERO,
                    fill,
                    Stroke::new(1.5, col),
                    StrokeKind::Inside,
                );
            }
            HandleShape::Circle => {
                painter.circle(sp, 4.0, fill, Stroke::new(1.5, col));
            }
            HandleShape::Diamond => {
                let diamond = vec![
                    sp + vec2(0.0, -6.0),
                    sp + vec2(6.0, 0.0),
                    sp + vec2(0.0, 6.0),
                    sp + vec2(-6.0, 0.0),
                ];
                painter.add(Shape::convex_polygon(diamond, fill, Stroke::new(1.5, col)));
            }
            HandleShape::Crosshair => {
                painter.circle_stroke(sp, 6.0, Stroke::new(1.5, col));
                painter.line_segment(
                    [sp - vec2(9.0, 0.0), sp + vec2(9.0, 0.0)],
                    Stroke::new(1.2, col),
                );
                painter.line_segment(
                    [sp - vec2(0.0, 9.0), sp + vec2(0.0, 9.0)],
                    Stroke::new(1.2, col),
                );
            }
        }
    }
}

fn draw_gradient_mid_stops(
    painter: &egui::Painter,
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    xf: &Affine2D,
    start_l: Point2D,
    end_l: Point2D,
    stops: &[GradientStop],
) {
    for (s_idx, st) in stops.iter().enumerate() {
        if s_idx == 0 || s_idx + 1 == stops.len() {
            continue;
        }
        let t = f64::from(st.offset);
        let w_pt = xf.transform_point(Point2D::new(
            start_l.x + (end_l.x - start_l.x) * t,
            start_l.y + (end_l.y - start_l.y) * t,
        ));
        let sp = world_to_screen(canvas_rect, tab, w_pt);
        painter.rect(
            Rect::from_center_size(sp, vec2(8.0, 8.0)),
            CornerRadius::same(1),
            core_color_to_egui(&st.color),
            Stroke::new(1.5, Color32::WHITE),
            StrokeKind::Inside,
        );
    }
}
