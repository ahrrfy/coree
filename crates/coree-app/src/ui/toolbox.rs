// SPDX-License-Identifier: MPL-2.0

//! Left vertical Toolbox (`44px` single-column or `80px` two-column) featuring
//! procedural clean-room vector icons drawn via `egui::Painter` (zero proprietary
//! bitmap assets), corner flyout triangles, and interactive flyout popups for all
//! **15 Toolbox Flyout groups**.

use crate::shortcuts::shortcut_label;
use crate::state::{StudioApp, ToolId, TOOL_GROUPS};
use crate::theme::TOOLBOX_WIDTH;
use egui::{
    pos2, vec2, Area, Color32, CornerRadius, Frame, Id, Margin, Order, Pos2, Rect, RichText, Sense,
    Shape, Stroke, StrokeKind, Ui,
};

/// Renders the left vertical Toolbox rail and any open Tool Flyout popup drawer.
pub fn show_toolbox(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let width = if app.toolbox_two_column {
        80.0
    } else {
        TOOLBOX_WIDTH
    };
    let mut flyout_anchor: Option<(usize, Pos2)> = None;

    egui::Panel::left("coree_toolbox")
        .exact_size(width)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::same(4))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("toolbox_scroll")
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                    let cols = if app.toolbox_two_column { 2 } else { 1 };
                    let mut idx = 0usize;
                    while idx < TOOL_GROUPS.len() {
                        ui.horizontal(|ui| {
                            for _ in 0..cols {
                                if idx >= TOOL_GROUPS.len() {
                                    break;
                                }
                                let g = idx;
                                idx += 1;

                                let primary_tool = app.group_active_tools[g];
                                let is_active = app.active_tool.group_index() == g;
                                let has_flyout = TOOL_GROUPS[g].len() > 1;

                                let (rect, resp) =
                                    ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
                                let painter = ui.painter();

                                // Background fill & hover/active state
                                let bg = if is_active || resp.hovered() {
                                    tokens.surface_elevated
                                } else {
                                    Color32::TRANSPARENT
                                };
                                painter.rect(
                                    rect,
                                    CornerRadius::same(5),
                                    bg,
                                    if is_active {
                                        Stroke::new(1.0, tokens.accent)
                                    } else {
                                        Stroke::NONE
                                    },
                                    StrokeKind::Inside,
                                );

                                // 3px left accent bar when active (matching components.md spec)
                                if is_active {
                                    let bar = Rect::from_min_size(
                                        rect.left_top() + vec2(0.0, 4.0),
                                        vec2(3.0, rect.height() - 8.0),
                                    );
                                    painter.rect_filled(bar, CornerRadius::same(1), tokens.accent);
                                }

                                // Procedural clean-room vector glyph
                                let icon_color = if is_active {
                                    tokens.accent
                                } else {
                                    tokens.text
                                };
                                paint_tool_icon(
                                    painter,
                                    rect.shrink(6.0),
                                    primary_tool,
                                    icon_color,
                                );

                                // Bottom-right 4px corner flyout triangle
                                if has_flyout {
                                    let tri_color = if app.open_flyout_group == Some(g) {
                                        tokens.accent
                                    } else {
                                        tokens.text_muted
                                    };
                                    let br = rect.right_bottom() - vec2(2.0, 2.0);
                                    painter.add(Shape::convex_polygon(
                                        vec![br, br - vec2(5.0, 0.0), br - vec2(0.0, 5.0)],
                                        tri_color,
                                        Stroke::NONE,
                                    ));
                                }

                                // Tooltip showing tool name + active profile shortcut
                                let sc = primary_tool
                                    .shortcut_action()
                                    .map(|a| shortcut_label(app.shortcut_profile, a))
                                    .unwrap_or("");
                                let tip = if sc.is_empty() {
                                    format!(
                                        "{}{}",
                                        primary_tool.label(),
                                        if has_flyout {
                                            " (Right-click or click corner for flyout)"
                                        } else {
                                            ""
                                        }
                                    )
                                } else {
                                    format!("{} [{}]", primary_tool.label(), sc)
                                };
                                let resp = resp.on_hover_text(tip);

                                // Click corner or right-click opens flyout; normal click selects tool (or toggles flyout if already active)
                                if resp.secondary_clicked() && has_flyout {
                                    app.open_flyout_group = if app.open_flyout_group == Some(g) {
                                        None
                                    } else {
                                        Some(g)
                                    };
                                } else if resp.clicked() {
                                    let clicked_corner =
                                        resp.interact_pointer_pos().is_some_and(|p| {
                                            p.x > rect.right() - 10.0 && p.y > rect.bottom() - 10.0
                                        });
                                    if has_flyout && (clicked_corner || is_active) {
                                        app.open_flyout_group = if app.open_flyout_group == Some(g)
                                        {
                                            None
                                        } else {
                                            Some(g)
                                        };
                                    } else {
                                        app.select_tool(primary_tool);
                                    }
                                }

                                if app.open_flyout_group == Some(g) {
                                    flyout_anchor = Some((g, pos2(rect.right() + 6.0, rect.top())));
                                }
                            }
                        });
                    }
                });
        });

    // Floating Tool Flyout Drawer anchored right of the clicked Toolbox button
    if let Some((g, anchor)) = flyout_anchor {
        let ctx = ui.ctx().clone();
        Area::new(Id::unique("coree_tool_flyout"))
            .order(Order::Foreground)
            .fixed_pos(anchor)
            .show(&ctx, |ui| {
                Frame::NONE
                    .fill(tokens.surface_elevated)
                    .stroke(Stroke::new(1.0, tokens.border_input))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_min_width(210.0);
                        for &tool in TOOL_GROUPS[g] {
                            let selected = app.active_tool == tool;
                            let sc = tool
                                .shortcut_action()
                                .map(|a| shortcut_label(app.shortcut_profile, a))
                                .unwrap_or("");
                            let row_resp = ui.horizontal(|ui| {
                                let (icon_rect, _) =
                                    ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                                paint_tool_icon(
                                    ui.painter(),
                                    icon_rect.shrink(1.0),
                                    tool,
                                    if selected { tokens.accent } else { tokens.text },
                                );
                                let btn =
                                    ui.add(
                                        egui::Button::new(RichText::new(tool.label()).color(
                                            if selected { tokens.accent } else { tokens.text },
                                        ))
                                        .shortcut_text(sc)
                                        .frame(selected),
                                    );
                                btn.clicked()
                            });
                            if row_resp.inner {
                                app.select_tool(tool);
                            }
                        }
                    });
            });
    }
}

/// Draws clean-room geometric vector glyphs for every tool using `egui::Painter`.
fn paint_tool_icon(painter: &egui::Painter, r: Rect, tool: ToolId, color: Color32) {
    let s = Stroke::new(1.5, color);
    let c = r.center();
    let w = r.width();
    let h = r.height();

    match tool {
        ToolId::Pick | ToolId::FreehandPick | ToolId::FreeTransform => {
            // Pointer arrow polygon
            let pts = vec![
                pos2(r.left() + w * 0.2, r.top() + h * 0.1),
                pos2(r.left() + w * 0.2, r.bottom() - h * 0.15),
                pos2(r.left() + w * 0.45, r.bottom() - h * 0.35),
                pos2(r.right() - w * 0.15, r.bottom() - h * 0.35),
            ];
            painter.add(Shape::closed_line(pts, s));
        }
        ToolId::ShapeNode
        | ToolId::Smooth
        | ToolId::Smear
        | ToolId::Twirl
        | ToolId::AttractRepel
        | ToolId::Roughen => {
            // Curve with anchor square + handles
            painter.line_segment([r.left_bottom(), c], s);
            painter.line_segment([c, r.right_top()], s);
            painter.rect_filled(
                Rect::from_center_size(c, vec2(5.0, 5.0)),
                CornerRadius::ZERO,
                color,
            );
        }
        ToolId::Crop | ToolId::Knife | ToolId::VirtualSegmentDelete | ToolId::Eraser => {
            // Diagonal blade / crop lines
            painter.line_segment(
                [
                    r.left_top() + vec2(3.0, 0.0),
                    r.left_bottom() - vec2(-3.0, 3.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    r.left_bottom() - vec2(-3.0, 3.0),
                    r.right_bottom() - vec2(0.0, 3.0),
                ],
                s,
            );
            painter.line_segment([r.left_bottom(), r.right_top()], s);
        }
        ToolId::Zoom | ToolId::Pan => {
            // Magnifying glass
            let lens_c = c - vec2(2.0, 2.0);
            painter.circle_stroke(lens_c, w * 0.32, s);
            painter.line_segment([lens_c + vec2(w * 0.22, h * 0.22), r.right_bottom()], s);
        }
        ToolId::Freehand
        | ToolId::TwoPointLine
        | ToolId::Bezier
        | ToolId::Pen
        | ToolId::BSpline
        | ToolId::Polyline
        | ToolId::ThreePointCurve
        | ToolId::SmartDrawing
        | ToolId::LiveSketch
        | ToolId::ArtisticMedia => {
            // Smooth wave / pen nib
            let pts = vec![
                pos2(r.left(), r.bottom() - h * 0.2),
                pos2(c.x - w * 0.1, r.top() + h * 0.2),
                pos2(c.x + w * 0.1, r.bottom() - h * 0.2),
                pos2(r.right(), r.top() + h * 0.2),
            ];
            painter.add(Shape::line(pts, s));
        }
        ToolId::Rectangle | ToolId::ThreePointRectangle => {
            painter.rect_stroke(r.shrink(1.5), CornerRadius::same(2), s, StrokeKind::Inside);
        }
        ToolId::Ellipse | ToolId::ThreePointEllipse => {
            painter.circle_stroke(c, w.min(h) * 0.44, s);
        }
        ToolId::Polygon
        | ToolId::Star
        | ToolId::ComplexStar
        | ToolId::Spiral
        | ToolId::GraphPaper
        | ToolId::Impact
        | ToolId::BasicShapes => {
            // Hexagon polygon
            let rad = w.min(h) * 0.45;
            let pts: Vec<Pos2> = (0..6)
                .map(|i| {
                    let a = std::f32::consts::TAU * (i as f32) / 6.0 - std::f32::consts::FRAC_PI_2;
                    pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
                })
                .collect();
            painter.add(Shape::closed_line(pts, s));
        }
        ToolId::ArtisticText | ToolId::ParagraphText | ToolId::Table => {
            // Letter 'A' / Table grid
            if tool == ToolId::Table {
                painter.rect_stroke(r.shrink(1.0), CornerRadius::ZERO, s, StrokeKind::Inside);
                painter.line_segment([pos2(r.left() + 1.0, c.y), pos2(r.right() - 1.0, c.y)], s);
                painter.line_segment([pos2(c.x, r.top() + 1.0), pos2(c.x, r.bottom() - 1.0)], s);
            } else {
                painter.line_segment(
                    [
                        pos2(c.x, r.top() + 1.0),
                        pos2(r.left() + 2.0, r.bottom() - 1.0),
                    ],
                    s,
                );
                painter.line_segment(
                    [
                        pos2(c.x, r.top() + 1.0),
                        pos2(r.right() - 2.0, r.bottom() - 1.0),
                    ],
                    s,
                );
                painter.line_segment(
                    [
                        pos2(r.left() + 5.0, c.y + 3.0),
                        pos2(r.right() - 5.0, c.y + 3.0),
                    ],
                    s,
                );
            }
        }
        ToolId::ParallelDimension
        | ToolId::HvDimension
        | ToolId::AngularDimension
        | ToolId::SegmentDimension
        | ToolId::ThreePointCallout
        | ToolId::StraightConnector
        | ToolId::RightAngleConnector => {
            // Dimension double-arrow bar
            painter.line_segment([pos2(r.left() + 1.0, c.y), pos2(r.right() - 1.0, c.y)], s);
            painter.line_segment(
                [
                    pos2(r.left() + 1.0, r.top() + 3.0),
                    pos2(r.left() + 1.0, r.bottom() - 3.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.right() - 1.0, r.top() + 3.0),
                    pos2(r.right() - 1.0, r.bottom() - 3.0),
                ],
                s,
            );
        }
        ToolId::DropShadow
        | ToolId::BlockShadow
        | ToolId::Contour
        | ToolId::Blend
        | ToolId::Distort
        | ToolId::Envelope
        | ToolId::Extrude25D
        | ToolId::Transparency => {
            // Overlapping offset squares (shadow/blend icon)
            let r1 = Rect::from_min_size(r.left_top() + vec2(1.0, 1.0), vec2(w * 0.6, h * 0.6));
            let r2 = Rect::from_min_size(
                r.left_top() + vec2(w * 0.35, h * 0.35),
                vec2(w * 0.6, h * 0.6),
            );
            painter.rect_filled(r2, CornerRadius::same(1), color.gamma_multiply(0.35));
            painter.rect_stroke(r1, CornerRadius::same(1), s, StrokeKind::Inside);
        }
        ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
            // Eyedropper diagonal stem + bulb
            painter.line_segment(
                [
                    r.left_bottom() + vec2(2.0, -2.0),
                    r.right_top() + vec2(-3.0, 3.0),
                ],
                s,
            );
            painter.circle_filled(r.right_top() + vec2(-3.0, 3.0), 3.0, color);
        }
        ToolId::InteractiveFill | ToolId::MeshFill | ToolId::SmartFill => {
            // Gradient fill vector bar with end dots
            painter.rect_stroke(r.shrink(2.0), CornerRadius::same(2), s, StrokeKind::Inside);
            painter.line_segment(
                [
                    r.left_bottom() + vec2(4.0, -4.0),
                    r.right_top() + vec2(-4.0, 4.0),
                ],
                s,
            );
            painter.circle_filled(r.left_bottom() + vec2(4.0, -4.0), 2.2, color);
            painter.circle_filled(r.right_top() + vec2(-4.0, 4.0), 2.2, color);
        }
        ToolId::LensTool => {
            // Concentric optical lens rings
            painter.circle_stroke(c, w * 0.44, s);
            painter.circle_stroke(c, w * 0.22, s);
            painter.circle_filled(c, 1.8, color);
        }
        ToolId::BitmapBrush
        | ToolId::BitmapEraser
        | ToolId::CloneStamp
        | ToolId::Smudge
        | ToolId::MaskSelect => {
            // Paint brush stroke + tip
            painter.line_segment(
                [r.right_top() + vec2(-2.0, 2.0), c],
                Stroke::new(2.0, color),
            );
            painter.circle_filled(r.left_bottom() + vec2(4.5, -4.5), 3.2, color);
        }
    }
}
