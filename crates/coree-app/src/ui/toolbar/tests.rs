// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::state::WorkspacePreset;
use crate::theme::StudioTheme;
use coree_core::{Color, PostScriptFillPreset};
use coree_render::GlyphBlockFilter;

#[test]
fn classic_studio_chrome_renders_all_property_bars_options_laser_glyphs_and_ps_fills() {
    let ctx = egui::Context::default();
    let mut app = StudioApp::new_headless();
    app.new_sample_document();
    app.fonts_configured = true;

    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))),
        ..egui::RawInput::default()
    };

    // 1. Exercise all 3 themes + all 6 Workspace Presets + Welcome hub
    app.show_welcome = true;
    for theme in StudioTheme::ALL {
        app.set_theme(theme);
        let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();
    }
    app.show_welcome = false;
    for preset in WorkspacePreset::ALL {
        app.apply_workspace_preset(preset);
    }

    // 2. Exercise Options (Ctrl+J) across all 4 tabs + Export G-Code dialog
    app.show_options_dialog = true;
    app.export_dialog.format_index = crate::ui::dialogs::EXPORT_FMT_GCODE;
    app.export_dialog.open = true;
    for tab_idx in 0..4 {
        app.options_tab = tab_idx;
        let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();
    }
    app.show_options_dialog = false;
    app.export_dialog.open = false;

    // 3. Exercise Laser & Plugins Studio docker with live computed toolpath plan
    app.refresh_laser_plan_preview();
    app.production_state.sub_section = 5;
    app.focus_docker(DockerTab::Production);
    app.active_palette_index = 3; // Laser & Cut Colors
    app.palette_show_names = true;
    let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
    assert!(!out.shapes.is_empty());
    out.textures_delta.clear();

    // 4. Exercise PostScript Procedural Fill + Interactive Glyphs Browser
    app.apply_fill(PostScriptFillPreset::CrossHatch.to_fill(Color::KEY_BLACK, Color::WHITE, 4));
    app.properties_state.active_tab = PropertiesSubTab::Fill;
    app.focus_docker(DockerTab::Properties);
    let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
    assert!(!out.shapes.is_empty());
    out.textures_delta.clear();

    app.properties_state.active_tab = PropertiesSubTab::Text;
    app.glyph_block_filter = GlyphBlockFilter::Arabic;
    let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
    assert!(!out.shapes.is_empty());
    out.textures_delta.clear();

    // 5. Exercise contextual Property Bar across key tool modes & 9-point anchor
    for (col, row) in [(0, 0), (1, 1), (2, 2)] {
        app.object_origin_anchor = (col, row);
    }
    for tool in [
        ToolId::Pick,
        ToolId::ShapeNode,
        ToolId::Smooth,
        ToolId::Roughen,
        ToolId::BitmapBrush,
        ToolId::DropShadow,
        ToolId::Contour,
        ToolId::InteractiveFill,
        ToolId::MeshFill,
        ToolId::ColorEyedropper,
        ToolId::Crop,
        ToolId::Zoom,
        ToolId::ArtisticText,
    ] {
        app.select_tool(tool);
        if tool == ToolId::BitmapBrush {
            app.tool_settings.raster_op = RasterBrushOp::Dodge;
        }
        let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();
    }

    // 6. Exercise Command Palette search for Laser & Workspaces
    app.command_palette.open = true;
    app.command_palette.query = "lightburn".to_owned();
    let mut out = ctx.run_ui(input, |ui| app.render_ui(ui));
    assert!(!out.shapes.is_empty());
    out.textures_delta.clear();

    // Verify Laser & Cut Colors palette contains all standard laser colors + CutContour
    let lpal = crate::ui::palette_bar::laser_and_cut_palette();
    assert!(lpal.swatches.iter().any(|s| s.name.contains("Laser Cut")));
    assert!(lpal.swatches.iter().any(|s| s.name == "CutContour"));

    app.shutdown_cleanly();
}
