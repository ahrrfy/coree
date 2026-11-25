// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(super) const PAGE_PRESETS: [(&str, f64, f64); 6] = [
    ("A4", 210.0, 297.0),
    ("A3", 297.0, 420.0),
    ("Letter", 215.9, 279.4),
    ("A5", 148.0, 210.0),
    ("100×100", 100.0, 100.0),
    ("Laser 300×200", 300.0, 200.0),
];

pub(super) const COMMON_FONTS: [&str; 8] = [
    "Noto Sans",
    "Noto Sans Arabic",
    "Noto Naskh Arabic",
    "DejaVu Sans",
    "DejaVu Serif",
    "Liberation Sans",
    "Arial",
    "Tahoma",
];

#[derive(Clone, Copy)]
pub(super) enum StdIcon {
    NewDoc,
    OpenDoc,
    SaveDoc,
    ImportDoc,
    ExportDoc,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    PreviewF9,
}

pub(super) fn std_icon_btn(
    ui: &mut Ui,
    tokens: &ThemeTokens,
    icon: StdIcon,
    tooltip: &str,
    enabled: bool,
    active: bool,
) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(23.0, 22.0), sense);
    let painter = ui.painter();

    let bg = if active {
        tokens.accent.gamma_multiply(0.24)
    } else if enabled && resp.hovered() {
        tokens.surface_elevated
    } else {
        Color32::TRANSPARENT
    };
    let border = if active {
        Stroke::new(1.0, tokens.accent)
    } else if enabled && resp.hovered() {
        Stroke::new(1.0, tokens.border_input)
    } else {
        Stroke::NONE
    };
    painter.rect(rect, CornerRadius::same(3), bg, border, StrokeKind::Inside);

    let color = if !enabled {
        tokens.text_muted.gamma_multiply(0.45)
    } else if active {
        tokens.accent
    } else {
        tokens.text
    };
    paint_std_icon(painter, rect.shrink(4.0), icon, color);

    let resp = resp.on_hover_text(tooltip);
    enabled && resp.clicked()
}

pub(super) fn paint_std_icon(painter: &egui::Painter, r: Rect, icon: StdIcon, color: Color32) {
    let s = Stroke::new(1.35, color);
    let thin = Stroke::new(1.05, color);
    let c = r.center();

    match icon {
        StdIcon::NewDoc => {
            let pts = vec![
                r.left_top() + vec2(2.0, 0.0),
                pos2(r.right() - 4.0, r.top()),
                pos2(r.right() - 1.0, r.top() + 3.5),
                r.right_bottom() - vec2(1.0, 0.0),
                r.left_bottom() + vec2(2.0, 0.0),
            ];
            painter.add(Shape::closed_line(pts, s));
        }
        StdIcon::OpenDoc => {
            let folder = Rect::from_min_max(pos2(r.left(), r.top() + 3.0), r.right_bottom());
            painter.rect_stroke(folder, CornerRadius::same(1), s, StrokeKind::Inside);
            painter.line_segment(
                [
                    pos2(r.left() + 1.0, r.top() + 1.5),
                    pos2(c.x, r.top() + 1.5),
                ],
                s,
            );
        }
        StdIcon::SaveDoc => {
            painter.rect_stroke(r.shrink(0.5), CornerRadius::same(1), s, StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(r.left() + 3.0, r.top() + 1.0),
                    pos2(r.right() - 3.0, c.y),
                ),
                CornerRadius::ZERO,
                color.gamma_multiply(0.35),
            );
            painter.rect_stroke(
                Rect::from_min_max(
                    pos2(r.left() + 3.5, c.y + 1.5),
                    pos2(r.right() - 3.5, r.bottom() - 1.0),
                ),
                CornerRadius::ZERO,
                thin,
                StrokeKind::Inside,
            );
        }
        StdIcon::ImportDoc => {
            painter.line_segment([pos2(c.x, r.top()), pos2(c.x, r.bottom() - 3.0)], s);
            painter.line_segment([pos2(c.x - 3.0, c.y), pos2(c.x, r.bottom() - 2.5)], s);
            painter.line_segment([pos2(c.x + 3.0, c.y), pos2(c.x, r.bottom() - 2.5)], s);
            painter.line_segment([r.left_bottom(), r.right_bottom()], s);
        }
        StdIcon::ExportDoc => {
            painter.line_segment([pos2(c.x, r.bottom() - 2.0), pos2(c.x, r.top() + 1.0)], s);
            painter.line_segment([pos2(c.x - 3.0, c.y - 1.0), pos2(c.x, r.top() + 1.0)], s);
            painter.line_segment([pos2(c.x + 3.0, c.y - 1.0), pos2(c.x, r.top() + 1.0)], s);
            painter.line_segment([r.left_bottom(), r.right_bottom()], s);
        }
        StdIcon::Undo => {
            let pts = vec![
                pos2(r.left() + 2.0, c.y - 1.0),
                pos2(r.right() - 2.0, c.y - 1.0),
                pos2(r.right() - 1.0, r.bottom() - 2.0),
            ];
            painter.add(Shape::line(pts, s));
            painter.line_segment(
                [
                    pos2(r.left() + 5.0, r.top() + 2.0),
                    pos2(r.left() + 1.5, c.y - 1.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.left() + 5.0, c.y + 2.5),
                    pos2(r.left() + 1.5, c.y - 1.0),
                ],
                s,
            );
        }
        StdIcon::Redo => {
            let pts = vec![
                pos2(r.right() - 2.0, c.y - 1.0),
                pos2(r.left() + 2.0, c.y - 1.0),
                pos2(r.left() + 1.0, r.bottom() - 2.0),
            ];
            painter.add(Shape::line(pts, s));
            painter.line_segment(
                [
                    pos2(r.right() - 5.0, r.top() + 2.0),
                    pos2(r.right() - 1.5, c.y - 1.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.right() - 5.0, c.y + 2.5),
                    pos2(r.right() - 1.5, c.y - 1.0),
                ],
                s,
            );
        }
        StdIcon::Cut => {
            painter.line_segment(
                [
                    r.left_top() + vec2(2.0, 1.0),
                    r.right_bottom() - vec2(1.0, 2.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    r.right_top() + vec2(-2.0, 1.0),
                    r.left_bottom() + vec2(1.0, -2.0),
                ],
                s,
            );
            painter.circle_stroke(r.left_bottom() + vec2(3.0, -2.5), 2.0, thin);
            painter.circle_stroke(r.right_bottom() + vec2(-3.0, -2.5), 2.0, thin);
        }
        StdIcon::Copy => {
            let b = Rect::from_min_size(r.left_top(), vec2(r.width() * 0.65, r.height() * 0.72));
            let f = Rect::from_min_size(
                r.left_top() + vec2(r.width() * 0.32, r.height() * 0.26),
                vec2(r.width() * 0.65, r.height() * 0.72),
            );
            painter.rect_stroke(b, CornerRadius::same(1), thin, StrokeKind::Inside);
            painter.rect_stroke(f, CornerRadius::same(1), s, StrokeKind::Inside);
        }
        StdIcon::Paste => {
            let board = Rect::from_min_max(
                pos2(r.left() + 1.5, r.top() + 2.5),
                r.right_bottom() - vec2(1.5, 0.0),
            );
            painter.rect_stroke(board, CornerRadius::same(1), s, StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_center_size(pos2(c.x, r.top() + 2.0), vec2(5.5, 2.5)),
                CornerRadius::same(1),
                color,
            );
        }
        StdIcon::PreviewF9 => {
            painter.rect_stroke(r.shrink(0.5), CornerRadius::same(1), s, StrokeKind::Inside);
            painter.line_segment(
                [r.left_top() + vec2(2.5, 2.5), r.left_top() + vec2(5.5, 2.5)],
                thin,
            );
            painter.line_segment(
                [r.left_top() + vec2(2.5, 2.5), r.left_top() + vec2(2.5, 5.5)],
                thin,
            );
            painter.line_segment(
                [
                    r.right_bottom() - vec2(2.5, 2.5),
                    r.right_bottom() - vec2(5.5, 2.5),
                ],
                thin,
            );
            painter.line_segment(
                [
                    r.right_bottom() - vec2(2.5, 2.5),
                    r.right_bottom() - vec2(2.5, 5.5),
                ],
                thin,
            );
        }
    }
}

pub(super) fn pressure_curve_label(json: &str, lang: crate::i18n::StudioLanguage) -> String {
    if json.contains("0.75") {
        I18nKey::TbPressureSoft.tr_ui(lang)
    } else if json.contains("0.25") {
        I18nKey::TbPressureFirm.tr_ui(lang)
    } else if json == "[[0.0,0.0],[1.0,1.0]]" {
        I18nKey::TbPressureLinear.tr_ui(lang)
    } else {
        I18nKey::TbPagePresetCustom.tr_ui(lang)
    }
}
