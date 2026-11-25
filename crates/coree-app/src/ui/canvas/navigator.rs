// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// Interactive `Navigator` pan/zoom minimap popup (`ADD-08`).
pub fn show_navigator_popup_area(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(app.ui_language);
    let screen = ui
        .ctx()
        .input(|i| i.raw.screen_rect)
        .unwrap_or_else(|| ui.ctx().content_rect());
    let anchor = if shell_dir.is_rtl {
        pos2(16.0, (screen.bottom() - 252.0).max(40.0))
    } else {
        pos2(
            (screen.right() - 238.0).max(16.0),
            (screen.bottom() - 252.0).max(40.0),
        )
    };
    let mut close_req = false;
    egui::Area::new(egui::Id::unique("coree_navigator_popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(tokens.surface_elevated)
                .stroke(Stroke::new(1.2, tokens.accent))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    close_req = navigator(ui, app);
                });
        });
    if close_req {
        app.show_navigator_popup = false;
    }
}

fn navigator(ui: &mut Ui, app: &mut StudioApp) -> bool {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut close_req = false;
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(
            RichText::new(crate::i18n::I18nKey::NavigatorTitle.tr_ui(lang))
                .strong()
                .size(11.5)
                .color(tokens.text),
        );
        ui.with_layout(
            shell_dir.opposite_horizontal_layout(egui::Align::Center),
            |ui| {
                if ui.small_button("✕").clicked() {
                    close_req = true;
                }
            },
        );
    });
    if let Some(tab) = app.active_tab_mut() {
        let page = tab.document.active_page().cloned();
        let (pw, ph) = page.as_ref().map_or((210.0, 297.0), |p| {
            (p.width_mm.max(1.0), p.height_mm.max(1.0))
        });
        let map_size = vec2(204.0, 148.0);
        let (map_rect, resp) = ui.allocate_exact_size(map_size, Sense::click_and_drag());
        let painter = ui.painter_at(map_rect);
        painter.rect_filled(map_rect, CornerRadius::same(3), tokens.canvas_well);

        let pad = 12.0_f32;
        let inner_w = (map_rect.width() - pad * 2.0).max(20.0);
        let inner_h = (map_rect.height() - pad * 2.0).max(20.0);
        let scale = (inner_w / pw as f32).min(inner_h / ph as f32).max(0.05);
        let page_screen_w = pw as f32 * scale;
        let page_screen_h = ph as f32 * scale;
        let page_origin = pos2(
            map_rect.center().x - page_screen_w * 0.5,
            map_rect.center().y - page_screen_h * 0.5,
        );
        let page_rect = Rect::from_min_size(page_origin, vec2(page_screen_w, page_screen_h));
        painter.rect(
            page_rect,
            CornerRadius::ZERO,
            Color32::WHITE,
            Stroke::new(1.0, tokens.border),
            StrokeKind::Inside,
        );

        // Viewport frame inside navigator minimap
        let vis = tab.viewport.visible_world_rect();
        let vx0 = page_origin.x + (vis.min_x as f32) * scale;
        let vy0 = page_origin.y + (vis.min_y as f32) * scale;
        let vx1 = page_origin.x + (vis.max_x as f32) * scale;
        let vy1 = page_origin.y + (vis.max_y as f32) * scale;
        let vis_rect = Rect::from_min_max(pos2(vx0, vy0), pos2(vx1, vy1)).intersect(map_rect);
        painter.rect_stroke(
            vis_rect,
            CornerRadius::same(1),
            Stroke::new(1.6, tokens.accent),
            StrokeKind::Inside,
        );

        // Click or drag anywhere in minimap to pan canvas center there
        if (resp.clicked() || resp.dragged()) && scale > 1e-6 {
            if let Some(pt) = resp.interact_pointer_pos() {
                let target_x = f64::from((pt.x - page_origin.x) / scale);
                let target_y = f64::from((pt.y - page_origin.y) / scale);
                tab.viewport.pan_mm = Point2D::new(target_x, target_y);
                tab.needs_rerender = true;
            }
        }

        ui.add_space(4.0);
        let mut zoom_pct = (tab.viewport.zoom * 100.0) as f32;
        if ui
            .add(egui::Slider::new(&mut zoom_pct, 10.0..=800.0).text("%"))
            .changed()
        {
            tab.viewport.zoom = f64::from(zoom_pct / 100.0).clamp(0.05, 64.0);
            tab.needs_rerender = true;
        }
    }
    close_req
}
