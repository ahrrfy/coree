// SPDX-License-Identifier: MIT OR Apache-2.0

//! `eframe 0.37` desktop application shell wrapping [`StudioApp`].

use crate::shortcuts::poll_egui_shortcuts;
use crate::state::StudioApp;
use crate::theme::{apply_theme, configure_arabic_capable_fonts};
use crate::ui::{
    canvas::show_canvas_and_status, command_palette::show_command_palette, dialogs::show_dialogs,
    dockers::show_dockers_panel, menu_bar::show_menu_bar_and_tabs, palette_bar::show_palette_bar,
    toolbar::show_toolbars, toolbox::show_toolbox, welcome::show_welcome_hub,
};
use coree_core::brand::DISPLAY_NAME;
use coree_core::{
    Affine2D, BezierPath, BlendMode, Color, CornerStyle, EllipseMode, GradientStop, LineCap,
    LineJoin, ParametricShape, Point2D, Rect2D,
};
use coree_render::{
    BackendPaint, CanvasBackend, FillRule, PaintShader, StrokeParams, TinySkiaBackend,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Generates a crisp `64x64` straight-RGBA clean-room `coree` geometric vector emblem
/// (teal/emerald Bézier C-emblem with vector anchor nodes & control handles on a dark
/// charcoal rounded badge) using pure-Rust `tiny_skia` via [`TinySkiaBackend`] with
/// zero external runtime asset files.
#[must_use]
pub fn build_app_icon_data() -> egui::IconData {
    const SIZE: u32 = 64;
    let Ok(mut backend) = TinySkiaBackend::new(SIZE, SIZE) else {
        return egui::IconData {
            rgba: vec![0u8; (SIZE as usize) * (SIZE as usize) * 4],
            width: SIZE,
            height: SIZE,
        };
    };

    backend.clear(Color::TRANSPARENT);

    let charcoal_top = Color::rgba8(0x20, 0x26, 0x30, 0xFF);
    let charcoal_bot = Color::rgba8(0x11, 0x15, 0x1B, 0xFF);
    let rim_slate = Color::rgba8(0x2C, 0x42, 0x4D, 0xFF);
    let emerald = Color::rgba8(0x10, 0xB9, 0x81, 0xFF);
    let teal_bright = Color::rgba8(0x2D, 0xD4, 0xBF, 0xFF);
    let mint_glow = Color::rgba8(0x34, 0xD3, 0x99, 0xFF);
    let node_white = Color::rgba8(0xF1, 0xF5, 0xF9, 0xFF);
    let dark_ink = Color::rgba8(0x11, 0x15, 0x1B, 0xFF);

    // 1. Dark charcoal rounded-square studio badge
    let badge_shape = ParametricShape::Rectangle {
        width: 58.0,
        height: 58.0,
        corner_radii: [13.0; 4],
        corner_style: CornerStyle::Rounded,
        lock_corners: true,
    }
    .to_bezier_path();
    let badge_xf = Affine2D::translate(3.0, 3.0);
    let badge_paint = BackendPaint {
        shader: PaintShader::LinearGradient {
            start: Point2D::new(0.0, 0.0),
            end: Point2D::new(58.0, 58.0),
            stops: vec![
                GradientStop::new(0.0, charcoal_top),
                GradientStop::new(1.0, charcoal_bot),
            ],
        },
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        anti_alias: true,
    };
    backend.fill_path(&badge_shape, &badge_paint, FillRule::Winding, badge_xf);
    backend.stroke_path(
        &badge_shape,
        &solid_stroke(&rim_slate, 1.5, LineCap::Round, LineJoin::Round),
        badge_xf,
    );

    // Geometry anchor coordinates for the Bézier C-emblem
    let cx = 31.0_f64;
    let cy = 32.0_f64;
    let r = 16.5_f64;
    let top_deg = 318.0_f64;
    let bot_deg = 42.0_f64;
    let top_tip = Point2D::new(
        cx + r * top_deg.to_radians().cos(),
        cy + r * top_deg.to_radians().sin(),
    );
    let bot_tip = Point2D::new(
        cx + r * bot_deg.to_radians().cos(),
        cy + r * bot_deg.to_radians().sin(),
    );
    let left_apex = Point2D::new(cx - r, cy);
    let top_handle = Point2D::new(51.5, 13.0);
    let bot_handle = Point2D::new(51.5, 51.0);

    // 2. Vector Bézier tangent handle arms
    let top_arm = BezierPath::from_polygon([top_tip, top_handle], false);
    let bot_arm = BezierPath::from_polygon([bot_tip, bot_handle], false);
    let handle_stroke = solid_stroke(&teal_bright, 1.4, LineCap::Round, LineJoin::Round);
    backend.stroke_path(&top_arm, &handle_stroke, Affine2D::IDENTITY);
    backend.stroke_path(&bot_arm, &handle_stroke, Affine2D::IDENTITY);

    // 3. Geometric teal/emerald Bézier C-arc emblem
    let c_arc = ParametricShape::Ellipse {
        rx: r,
        ry: r,
        mode: EllipseMode::Arc,
        start_deg: bot_deg,
        end_deg: top_deg,
    }
    .to_bezier_path();
    let arc_xf = Affine2D::translate(cx, cy);
    backend.stroke_path(
        &c_arc,
        &solid_stroke(&emerald, 7.2, LineCap::Round, LineJoin::Round),
        arc_xf,
    );
    let mut highlight_stroke = solid_stroke(&teal_bright, 2.2, LineCap::Round, LineJoin::Round);
    highlight_stroke.opacity = 0.85;
    backend.stroke_path(&c_arc, &highlight_stroke, arc_xf);

    // 4. Inner precision vector node diamond reticle
    let diamond = BezierPath::from_polygon(
        [
            Point2D::new(40.0, 26.0),
            Point2D::new(46.0, 32.0),
            Point2D::new(40.0, 38.0),
            Point2D::new(34.0, 32.0),
        ],
        true,
    );
    backend.fill_path(
        &diamond,
        &BackendPaint::solid(mint_glow),
        FillRule::Winding,
        Affine2D::IDENTITY,
    );
    let pin = circle_path(1.9);
    backend.fill_path(
        &pin,
        &BackendPaint::solid(dark_ink.clone()),
        FillRule::Winding,
        Affine2D::translate(40.0, 32.0),
    );

    // 5. Crisp white square Bézier anchor nodes (tips + left apex)
    let anchor_box = BezierPath::from_rect(Rect2D::from_xywh(-2.5, -2.5, 5.0, 5.0));
    let anchor_border = solid_stroke(&dark_ink, 1.2, LineCap::Butt, LineJoin::Miter);
    let anchor_paint = BackendPaint::solid(node_white);
    for pt in [top_tip, bot_tip, left_apex] {
        let xf = Affine2D::translate(pt.x, pt.y);
        backend.fill_path(&anchor_box, &anchor_paint, FillRule::Winding, xf);
        backend.stroke_path(&anchor_box, &anchor_border, xf);
    }

    // 6. Teal circular Bézier control handle dots
    let handle_dot = circle_path(2.6);
    let handle_paint = BackendPaint::solid(teal_bright);
    for pt in [top_handle, bot_handle] {
        let xf = Affine2D::translate(pt.x, pt.y);
        backend.fill_path(&handle_dot, &handle_paint, FillRule::Winding, xf);
        backend.stroke_path(&handle_dot, &anchor_border, xf);
    }

    let buf = backend.into_rgba();
    egui::IconData {
        rgba: buf.rgba,
        width: buf.width,
        height: buf.height,
    }
}

fn solid_stroke(color: &Color, width: f32, cap: LineCap, join: LineJoin) -> StrokeParams {
    StrokeParams {
        color: color.clone(),
        width,
        cap,
        join,
        miter_limit: 4.0,
        dash_pattern: Vec::new(),
        dash_offset: 0.0,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        anti_alias: true,
    }
}

fn circle_path(radius: f64) -> BezierPath {
    ParametricShape::Ellipse {
        rx: radius,
        ry: radius,
        mode: EllipseMode::Full,
        start_deg: 0.0,
        end_deg: 360.0,
    }
    .to_bezier_path()
}

/// Launches the native desktop `eframe` window (`Wayland` / `X11` / `Win32`).
pub fn run_desktop_gui(initial_files: Vec<PathBuf>) -> Result<(), String> {
    let app_icon = Arc::new(build_app_icon_data());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!(
                "{DISPLAY_NAME} — Vector Graphics & Print Production Studio"
            ))
            .with_icon(app_icon)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 620.0]),
        ..eframe::NativeOptions::default()
    };

    eframe::run_native(
        DISPLAY_NAME,
        options,
        Box::new(move |cc| {
            let mut app = StudioApp::new_desktop(initial_files);
            configure_arabic_capable_fonts(&cc.egui_ctx);
            app.fonts_configured = true;
            apply_theme(&cc.egui_ctx, app.theme);
            app.theme_dirty = false;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| format!("failed to launch desktop studio: {e}"))
}

impl StudioApp {
    /// Renders one complete studio frame into `ui` (also callable headlessly in tests).
    pub fn render_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        if !self.fonts_configured {
            configure_arabic_capable_fonts(&ctx);
            self.fonts_configured = true;
        }
        if self.theme_dirty {
            apply_theme(&ctx, self.theme);
            self.theme_dirty = false;
        }

        // Poll async background jobs, prune expired toasts, tick crash-recovery autosave
        self.tick_background_jobs();
        self.prune_toasts();
        self.tick_autosave();

        // Keep repainting smoothly while background jobs are running
        if !self.active_jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        // Handle OS drag-and-drop files onto the studio window
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        for path in dropped {
            if let Err(err) = self.open_file_path(&path) {
                self.push_toast(err, true);
            }
        }

        // Process global keyboard shortcuts (unless Command Palette is open and consuming keys)
        if !self.command_palette.open {
            let actions = poll_egui_shortcuts(&ctx, self.shortcut_profile);
            for act in actions {
                self.handle_shortcut(act);
            }
        }

        // 1. Top Menu Bar + Multi-Document Tab Strip
        show_menu_bar_and_tabs(ui, self);

        if self.show_welcome {
            // Welcome & Crash Recovery Hub (`S01`)
            show_welcome_hub(ui, self);
        } else {
            // 2. Standard Toolbar + Context-Sensitive Property Bar
            show_toolbars(ui, self);

            // 3. Left 44px Toolbox Rail (+ Flyouts for all 15 tool groups)
            show_toolbox(ui, self);

            // 4. Right 32px Color Palette Swatch Bar
            show_palette_bar(ui, self);

            // 5. Right Resizable `egui_dock` Dockers Host
            show_dockers_panel(ui, self);

            // 6. Bottom Status Bar + Page Tabs + Central Rulers & Interactive Canvas Viewport
            show_canvas_and_status(ui, self);
        }

        // 7. Modals & Command Palette overlay
        show_dialogs(&ctx, self);
        show_command_palette(&ctx, self);
    }
}

impl eframe::App for StudioApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render_ui(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown_cleanly();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_generates_crisp_64x64_rgba_vector_emblem() {
        let icon = build_app_icon_data();
        assert_eq!(icon.width, 64);
        assert_eq!(icon.height, 64);
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);

        // Corner pixel (0, 0) outside rounded badge must be transparent
        assert_eq!(icon.rgba[3], 0, "outer corner should be transparent");

        // Badge interior and emerald/teal vector emblem pixels must be opaque & colored
        let mut opaque_count = 0usize;
        let mut teal_emerald_count = 0usize;
        for px in icon.rgba.as_chunks::<4>().0 {
            if px[3] > 200 {
                opaque_count += 1;
                if px[1] >= 150 && u16::from(px[1]) > u16::from(px[0]) + 40 {
                    teal_emerald_count += 1;
                }
            }
        }
        assert!(
            opaque_count > 2500,
            "badge should occupy most of the 64x64 icon (opaque={opaque_count})"
        );
        assert!(
            teal_emerald_count > 150,
            "teal/emerald C-emblem should render crisp colored pixels (count={teal_emerald_count})"
        );
    }
}
