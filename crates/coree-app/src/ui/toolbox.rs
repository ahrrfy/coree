// SPDX-License-Identifier: MIT OR Apache-2.0

//! Left vertical Toolbox (`44px` single-column or `80px` two-column) featuring
//! distinct procedural clean-room vector icons drawn via `egui::Painter` (zero
//! proprietary bitmap assets), corner flyout triangles, and interactive flyout
//! popups for all **15 Toolbox Flyout groups**.

use crate::shortcuts::shortcut_label;
use crate::state::{StudioApp, ToolId, TOOL_GROUPS};
use crate::theme::{ThemeTokens, TOOLBOX_WIDTH};
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
                    ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
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
                                    ui.allocate_exact_size(vec2(32.0, 30.0), Sense::click());
                                let painter = ui.painter();

                                // Crisp classic studio background & hover/active frame
                                let bg = if is_active {
                                    tokens.accent.gamma_multiply(if tokens.is_dark {
                                        0.20
                                    } else {
                                        0.14
                                    })
                                } else if resp.hovered() {
                                    tokens.surface_elevated
                                } else {
                                    Color32::TRANSPARENT
                                };
                                let border = if is_active {
                                    Stroke::new(1.0, tokens.accent)
                                } else if resp.hovered() {
                                    Stroke::new(1.0, tokens.border_input)
                                } else {
                                    Stroke::NONE
                                };
                                painter.rect(
                                    rect,
                                    CornerRadius::same(4),
                                    bg,
                                    border,
                                    StrokeKind::Inside,
                                );

                                // 3px left accent indicator when active
                                if is_active {
                                    let bar = Rect::from_min_size(
                                        rect.left_top() + vec2(0.0, 3.0),
                                        vec2(3.0, rect.height() - 6.0),
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
                                    rect.shrink(5.5),
                                    primary_tool,
                                    icon_color,
                                    &tokens,
                                );

                                // Bottom-right crisp corner flyout triangle
                                if has_flyout {
                                    let tri_color = if app.open_flyout_group == Some(g) || is_active
                                    {
                                        tokens.accent
                                    } else {
                                        tokens.text_muted
                                    };
                                    let br = rect.right_bottom() - vec2(2.0, 2.0);
                                    painter.add(Shape::convex_polygon(
                                        vec![br, br - vec2(5.5, 0.0), br - vec2(0.0, 5.5)],
                                        tri_color,
                                        Stroke::NONE,
                                    ));
                                }

                                // Tooltip showing tool name + active profile shortcut + double-click shortcut
                                let sc = primary_tool
                                    .shortcut_action()
                                    .map(|a| shortcut_label(app.shortcut_profile, a))
                                    .unwrap_or("");
                                let dbl_hint = match primary_tool {
                                    ToolId::Pick => "\nDouble-click: Select All objects",
                                    ToolId::Rectangle => {
                                        "\nDouble-click: Page Frame · Shift+DblClick: Selection Frame"
                                    }
                                    _ => "",
                                };
                                let tip = if sc.is_empty() {
                                    format!(
                                        "{}{}{dbl_hint}",
                                        primary_tool.label(),
                                        if has_flyout {
                                            "\nRight-click or click corner triangle for flyout"
                                        } else {
                                            ""
                                        }
                                    )
                                } else {
                                    format!(
                                        "{} [{}]{}{dbl_hint}",
                                        primary_tool.label(),
                                        sc,
                                        if has_flyout {
                                            "\nRight-click or click corner triangle for flyout"
                                        } else {
                                            ""
                                        }
                                    )
                                };
                                let resp = resp.on_hover_text(tip);

                                // Double-click quick actions (Pick = Select All, Rectangle = Page/Selection Frame)
                                // Single-click or corner/right-click selects tool or toggles flyout.
                                if resp.double_clicked() {
                                    app.open_flyout_group = None;
                                    match primary_tool {
                                        ToolId::Pick => {
                                            app.select_tool(ToolId::Pick);
                                            app.select_all();
                                        }
                                        ToolId::Rectangle => {
                                            let shift = ui.input(|i| i.modifiers.shift);
                                            app.create_page_frame_rectangle(shift);
                                        }
                                        _ => {}
                                    }
                                } else if resp.secondary_clicked() && has_flyout {
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
                                    flyout_anchor = Some((g, pos2(rect.right() + 5.0, rect.top())));
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
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_min_width(216.0);
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
                                    &tokens,
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

/// Draws crisp 2-tone clean-room geometric vector glyphs for every tool using `egui::Painter`.
fn paint_tool_icon(
    painter: &egui::Painter,
    r: Rect,
    tool: ToolId,
    color: Color32,
    tokens: &ThemeTokens,
) {
    let accent = tokens.accent;
    let amber = tokens.warning;
    let s = Stroke::new(1.45, color);
    let accent_s = Stroke::new(1.25, accent);
    let thin = Stroke::new(1.1, color);
    let c = r.center();
    let w = r.width();
    let h = r.height();

    match tool {
        ToolId::Pick => {
            let pts = vec![
                pos2(r.left() + w * 0.22, r.top() + h * 0.08),
                pos2(r.left() + w * 0.22, r.bottom() - h * 0.12),
                pos2(r.left() + w * 0.45, r.bottom() - h * 0.34),
                pos2(r.right() - w * 0.14, r.bottom() - h * 0.34),
            ];
            painter.add(Shape::convex_polygon(
                pts.clone(),
                color.gamma_multiply(0.82),
                s,
            ));
            painter.circle_filled(pts[0], 1.6, accent);
        }
        ToolId::FreehandPick => {
            painter.circle_stroke(pos2(c.x - w * 0.1, c.y - h * 0.1), w * 0.32, thin);
            let pts = vec![
                pos2(c.x + w * 0.05, c.y + h * 0.02),
                pos2(c.x + w * 0.05, r.bottom() - 1.0),
                pos2(r.right() - 1.0, r.bottom() - h * 0.24),
            ];
            painter.add(Shape::closed_line(pts, s));
        }
        ToolId::FreeTransform => {
            painter.rect_stroke(r.shrink(4.0), CornerRadius::ZERO, thin, StrokeKind::Inside);
            painter.line_segment([r.left_top(), r.left_top() + vec2(4.0, 0.0)], s);
            painter.line_segment([r.left_top(), r.left_top() + vec2(0.0, 4.0)], s);
            painter.line_segment([r.right_bottom(), r.right_bottom() - vec2(4.0, 0.0)], s);
            painter.line_segment([r.right_bottom(), r.right_bottom() - vec2(0.0, 4.0)], s);
        }
        ToolId::ShapeNode => {
            // Two-tone Bézier curve node with tangent handles and teal anchor square
            painter.line_segment(
                [
                    pos2(r.left() + 1.0, r.bottom() - 2.0),
                    pos2(r.right() - 1.0, r.top() + 2.0),
                ],
                accent_s,
            );
            painter.circle_filled(pos2(r.left() + 2.0, r.bottom() - 2.0), 1.8, accent);
            painter.circle_filled(pos2(r.right() - 2.0, r.top() + 2.0), 1.8, accent);
            painter.rect_filled(
                Rect::from_center_size(c, vec2(5.4, 5.4)),
                CornerRadius::ZERO,
                color,
            );
        }
        ToolId::Smooth | ToolId::Smear | ToolId::Twirl | ToolId::AttractRepel | ToolId::Roughen => {
            // Circular sculpt nib + deformed edge wave
            painter.circle_stroke(c, w.min(h) * 0.40, s);
            let wave = vec![
                pos2(r.left() + 2.0, c.y + 2.0),
                pos2(c.x - 2.0, c.y - 3.0),
                pos2(c.x + 2.0, c.y + 3.0),
                pos2(r.right() - 2.0, c.y - 2.0),
            ];
            painter.add(Shape::line(wave, thin));
        }
        ToolId::Crop => {
            // Classic interlocking crop corner brackets
            let inset = 3.5;
            painter.line_segment(
                [
                    pos2(r.left() + inset, r.top()),
                    pos2(r.left() + inset, r.bottom() - inset),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.left() + inset, r.bottom() - inset),
                    pos2(r.right(), r.bottom() - inset),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.left(), r.top() + inset),
                    pos2(r.right() - inset, r.top() + inset),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.right() - inset, r.top() + inset),
                    pos2(r.right() - inset, r.bottom()),
                ],
                s,
            );
        }
        ToolId::Knife => {
            painter.line_segment(
                [
                    r.left_bottom() + vec2(1.0, -1.0),
                    r.right_top() + vec2(-1.0, 1.0),
                ],
                Stroke::new(2.0, color),
            );
            painter.line_segment(
                [
                    pos2(c.x - 2.0, c.y + 2.0),
                    pos2(r.left() + 2.0, r.bottom() - 5.0),
                ],
                thin,
            );
        }
        ToolId::VirtualSegmentDelete => {
            painter.line_segment(
                [pos2(r.left() + 1.0, c.y), pos2(r.right() - 1.0, c.y)],
                thin,
            );
            painter.line_segment([c - vec2(4.0, 4.0), c + vec2(4.0, 4.0)], s);
            painter.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], s);
        }
        ToolId::Eraser => {
            let pts = vec![
                pos2(r.left() + 2.0, c.y + 1.0),
                pos2(c.x + 1.0, r.top() + 2.0),
                pos2(r.right() - 1.0, c.y - 1.0),
                pos2(c.x - 1.0, r.bottom() - 2.0),
            ];
            painter.add(Shape::closed_line(pts, s));
            painter.line_segment(
                [
                    pos2(r.left() + 2.0, r.bottom() - 1.0),
                    pos2(r.right() - 2.0, r.bottom() - 1.0),
                ],
                thin,
            );
        }
        ToolId::Zoom => {
            let lens_c = c - vec2(2.0, 2.0);
            let rad = w.min(h) * 0.32;
            painter.circle_stroke(lens_c, rad, s);
            painter.line_segment(
                [lens_c + vec2(rad * 0.72, rad * 0.72), r.right_bottom()],
                Stroke::new(1.8, color),
            );
            painter.line_segment([lens_c - vec2(3.0, 0.0), lens_c + vec2(3.0, 0.0)], thin);
            painter.line_segment([lens_c - vec2(0.0, 3.0), lens_c + vec2(0.0, 3.0)], thin);
        }
        ToolId::Pan => {
            // 4-way pan cross arrows
            painter.line_segment([pos2(c.x, r.top() + 1.5), pos2(c.x, r.bottom() - 1.5)], s);
            painter.line_segment([pos2(r.left() + 1.5, c.y), pos2(r.right() - 1.5, c.y)], s);
            painter.circle_filled(c, 2.0, color);
        }
        ToolId::Bezier | ToolId::Pen => {
            // Crisp fountain pen nib + tip anchor
            let nib = vec![
                pos2(c.x, r.top() + 1.0),
                pos2(r.right() - 3.0, c.y + 2.0),
                pos2(c.x + 2.5, r.bottom() - 2.0),
                pos2(c.x - 2.5, r.bottom() - 2.0),
                pos2(r.left() + 3.0, c.y + 2.0),
            ];
            painter.add(Shape::closed_line(nib, s));
            painter.line_segment([pos2(c.x, r.top() + 1.0), c], thin);
            painter.circle_filled(c, 1.6, color);
        }
        ToolId::TwoPointLine => {
            painter.line_segment(
                [
                    r.left_bottom() + vec2(2.0, -2.0),
                    r.right_top() + vec2(-2.0, 2.0),
                ],
                s,
            );
            painter.rect_filled(
                Rect::from_center_size(r.left_bottom() + vec2(2.0, -2.0), vec2(3.5, 3.5)),
                CornerRadius::ZERO,
                color,
            );
            painter.rect_filled(
                Rect::from_center_size(r.right_top() + vec2(-2.0, 2.0), vec2(3.5, 3.5)),
                CornerRadius::ZERO,
                color,
            );
        }
        ToolId::Freehand
        | ToolId::BSpline
        | ToolId::Polyline
        | ToolId::ThreePointCurve
        | ToolId::SmartDrawing
        | ToolId::LiveSketch
        | ToolId::ArtisticMedia => {
            let pts = vec![
                pos2(r.left() + 1.0, r.bottom() - h * 0.18),
                pos2(c.x - w * 0.14, r.top() + h * 0.18),
                pos2(c.x + w * 0.14, r.bottom() - h * 0.18),
                pos2(r.right() - 1.0, r.top() + h * 0.18),
            ];
            painter.add(Shape::line(pts, s));
        }
        ToolId::Rectangle | ToolId::ThreePointRectangle => {
            painter.rect_filled(
                r.shrink(2.2),
                CornerRadius::same(1),
                accent.gamma_multiply(0.24),
            );
            painter.rect_stroke(r.shrink(1.5), CornerRadius::same(1), s, StrokeKind::Inside);
            if tool == ToolId::ThreePointRectangle {
                painter.circle_filled(r.left_bottom() + vec2(2.0, -2.0), 1.9, amber);
                painter.circle_filled(r.right_bottom() + vec2(-2.0, -2.0), 1.9, amber);
            }
        }
        ToolId::Ellipse | ToolId::ThreePointEllipse => {
            painter.circle_filled(c, w.min(h) * 0.42, accent.gamma_multiply(0.22));
            painter.circle_stroke(c, w.min(h) * 0.44, s);
        }
        ToolId::Polygon => {
            let rad = w.min(h) * 0.45;
            let pts: Vec<Pos2> = (0..6)
                .map(|i| {
                    let a = std::f32::consts::TAU * (i as f32) / 6.0 - std::f32::consts::FRAC_PI_2;
                    pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
                })
                .collect();
            painter.add(Shape::closed_line(pts, s));
        }
        ToolId::Star | ToolId::ComplexStar => {
            let outer = w.min(h) * 0.46;
            let inner = outer * 0.42;
            let pts: Vec<Pos2> = (0..10)
                .map(|i| {
                    let rad = if i % 2 == 0 { outer } else { inner };
                    let a = std::f32::consts::TAU * (i as f32) / 10.0 - std::f32::consts::FRAC_PI_2;
                    pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
                })
                .collect();
            painter.add(Shape::closed_line(pts, s));
        }
        ToolId::Spiral => {
            let max_r = w.min(h) * 0.45;
            let pts: Vec<Pos2> = (0..28)
                .map(|i| {
                    let t = i as f32 / 27.0;
                    let a = t * std::f32::consts::TAU * 2.2;
                    let rad = t * max_r;
                    pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
                })
                .collect();
            painter.add(Shape::line(pts, thin));
        }
        ToolId::GraphPaper => {
            let g = r.shrink(1.5);
            painter.rect_stroke(g, CornerRadius::ZERO, s, StrokeKind::Inside);
            for step in [1.0 / 3.0, 2.0 / 3.0] {
                let x = g.left() + g.width() * step;
                let y = g.top() + g.height() * step;
                painter.line_segment([pos2(x, g.top()), pos2(x, g.bottom())], thin);
                painter.line_segment([pos2(g.left(), y), pos2(g.right(), y)], thin);
            }
        }
        ToolId::Impact | ToolId::BasicShapes => {
            let rad = w.min(h) * 0.45;
            for i in 0..8 {
                let a = std::f32::consts::TAU * (i as f32) / 8.0;
                painter.line_segment(
                    [
                        pos2(c.x + rad * 0.25 * a.cos(), c.y + rad * 0.25 * a.sin()),
                        pos2(c.x + rad * a.cos(), c.y + rad * a.sin()),
                    ],
                    thin,
                );
            }
        }
        ToolId::ArtisticText => {
            painter.line_segment(
                [
                    pos2(c.x, r.top() + 1.0),
                    pos2(r.left() + 2.5, r.bottom() - 1.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(c.x, r.top() + 1.0),
                    pos2(r.right() - 2.5, r.bottom() - 1.0),
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
        ToolId::ParagraphText => {
            painter.rect_stroke(r.shrink(1.0), CornerRadius::ZERO, thin, StrokeKind::Inside);
            for dy in [-3.5, 0.0, 3.5] {
                painter.line_segment(
                    [
                        pos2(r.left() + 4.0, c.y + dy),
                        pos2(r.right() - 4.0, c.y + dy),
                    ],
                    thin,
                );
            }
        }
        ToolId::Table => {
            let g = r.shrink(1.0);
            painter.rect_stroke(g, CornerRadius::ZERO, s, StrokeKind::Inside);
            painter.line_segment([pos2(g.left(), c.y), pos2(g.right(), c.y)], thin);
            painter.line_segment([pos2(c.x, g.top()), pos2(c.x, g.bottom())], thin);
        }
        ToolId::StraightConnector | ToolId::RightAngleConnector => {
            painter.line_segment([r.left_top() + vec2(3.0, 3.0), pos2(c.x, r.top() + 3.0)], s);
            painter.line_segment([pos2(c.x, r.top() + 3.0), pos2(c.x, r.bottom() - 3.0)], s);
            painter.line_segment(
                [
                    pos2(c.x, r.bottom() - 3.0),
                    r.right_bottom() - vec2(3.0, 3.0),
                ],
                s,
            );
            painter.circle_filled(r.left_top() + vec2(3.0, 3.0), 2.0, color);
            painter.circle_filled(r.right_bottom() - vec2(3.0, 3.0), 2.0, color);
        }
        ToolId::ParallelDimension
        | ToolId::HvDimension
        | ToolId::AngularDimension
        | ToolId::SegmentDimension
        | ToolId::ThreePointCallout => {
            painter.line_segment([pos2(r.left() + 1.5, c.y), pos2(r.right() - 1.5, c.y)], s);
            painter.line_segment(
                [
                    pos2(r.left() + 1.5, r.top() + 2.5),
                    pos2(r.left() + 1.5, r.bottom() - 2.5),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.right() - 1.5, r.top() + 2.5),
                    pos2(r.right() - 1.5, r.bottom() - 2.5),
                ],
                s,
            );
        }
        ToolId::Contour => {
            painter.rect_stroke(r.shrink(1.0), CornerRadius::same(1), s, StrokeKind::Inside);
            painter.rect_stroke(
                r.shrink(4.5),
                CornerRadius::same(1),
                thin,
                StrokeKind::Inside,
            );
        }
        ToolId::Blend => {
            painter.circle_stroke(r.left_top() + vec2(5.5, 5.5), 4.0, thin);
            painter.circle_stroke(c, 4.0, thin);
            painter.circle_stroke(r.right_bottom() - vec2(5.5, 5.5), 4.0, s);
        }
        ToolId::Extrude25D => {
            let f = Rect::from_min_size(r.left_top() + vec2(1.0, 4.5), vec2(w * 0.58, h * 0.58));
            let b = Rect::from_min_size(r.left_top() + vec2(5.0, 1.0), vec2(w * 0.58, h * 0.58));
            painter.rect_stroke(b, CornerRadius::ZERO, thin, StrokeKind::Inside);
            painter.rect_stroke(f, CornerRadius::ZERO, s, StrokeKind::Inside);
            painter.line_segment([f.left_top(), b.left_top()], thin);
            painter.line_segment([f.right_top(), b.right_top()], thin);
            painter.line_segment([f.right_bottom(), b.right_bottom()], thin);
        }
        ToolId::Transparency => {
            let g = r.shrink(1.5);
            painter.rect_stroke(g, CornerRadius::same(1), s, StrokeKind::Inside);
            let half = vec2(g.width() * 0.5, g.height() * 0.5);
            painter.rect_filled(
                Rect::from_min_size(g.left_top(), half),
                CornerRadius::ZERO,
                color.gamma_multiply(0.55),
            );
            painter.rect_filled(
                Rect::from_min_size(g.center(), half),
                CornerRadius::ZERO,
                color.gamma_multiply(0.25),
            );
        }
        ToolId::DropShadow | ToolId::BlockShadow | ToolId::Distort | ToolId::Envelope => {
            let r1 = Rect::from_min_size(r.left_top() + vec2(1.0, 1.0), vec2(w * 0.62, h * 0.62));
            let r2 = Rect::from_min_size(
                r.left_top() + vec2(w * 0.34, h * 0.34),
                vec2(w * 0.62, h * 0.62),
            );
            painter.rect_filled(r2, CornerRadius::same(1), accent.gamma_multiply(0.45));
            painter.rect_filled(r1, CornerRadius::same(1), color.gamma_multiply(0.18));
            painter.rect_stroke(r1, CornerRadius::same(1), s, StrokeKind::Inside);
        }
        ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
            painter.line_segment(
                [
                    r.left_bottom() + vec2(2.0, -2.0),
                    r.right_top() + vec2(-3.5, 3.5),
                ],
                Stroke::new(1.8, color),
            );
            painter.circle_filled(r.right_top() + vec2(-3.5, 3.5), 3.1, amber);
        }
        ToolId::MeshFill => {
            let g = r.shrink(1.5);
            painter.rect_filled(g, CornerRadius::same(1), accent.gamma_multiply(0.18));
            painter.rect_stroke(g, CornerRadius::same(1), thin, StrokeKind::Inside);
            painter.line_segment([pos2(g.left(), c.y), pos2(g.right(), c.y)], accent_s);
            painter.line_segment([pos2(c.x, g.top()), pos2(c.x, g.bottom())], accent_s);
            painter.circle_filled(c, 2.4, amber);
        }
        ToolId::InteractiveFill | ToolId::SmartFill => {
            let g = r.shrink(1.5);
            painter.rect_filled(g, CornerRadius::same(1), accent.gamma_multiply(0.25));
            painter.rect_stroke(g, CornerRadius::same(1), s, StrokeKind::Inside);
            painter.line_segment(
                [
                    r.left_bottom() + vec2(3.5, -3.5),
                    r.right_top() + vec2(-3.5, 3.5),
                ],
                accent_s,
            );
            painter.circle_filled(r.left_bottom() + vec2(3.5, -3.5), 2.2, color);
            painter.circle_filled(r.right_top() + vec2(-3.5, 3.5), 2.2, amber);
        }
        ToolId::LensTool => {
            painter.circle_stroke(c, w.min(h) * 0.44, s);
            painter.circle_stroke(c, w.min(h) * 0.22, thin);
            painter.circle_filled(c, 1.8, color);
        }
        ToolId::MaskSelect => {
            let g = r.shrink(2.0);
            painter.rect_stroke(g, CornerRadius::ZERO, thin, StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_center_size(g.left_top(), vec2(3.0, 3.0)),
                CornerRadius::ZERO,
                color,
            );
            painter.rect_filled(
                Rect::from_center_size(g.right_bottom(), vec2(3.0, 3.0)),
                CornerRadius::ZERO,
                color,
            );
        }
        ToolId::BitmapBrush | ToolId::BitmapEraser | ToolId::CloneStamp | ToolId::Smudge => {
            painter.line_segment(
                [r.right_top() + vec2(-2.0, 2.0), c],
                Stroke::new(2.0, color),
            );
            painter.circle_filled(r.left_bottom() + vec2(4.5, -4.5), 3.1, color);
        }
    }
}
