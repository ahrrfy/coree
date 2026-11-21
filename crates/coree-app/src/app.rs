// SPDX-License-Identifier: MPL-2.0

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
use std::path::PathBuf;

/// Launches the native desktop `eframe` window (`Wayland` / `X11` / `Win32`).
pub fn run_desktop_gui(initial_files: Vec<PathBuf>) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!(
                "{DISPLAY_NAME} — Vector Graphics & Print Production Studio"
            ))
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
