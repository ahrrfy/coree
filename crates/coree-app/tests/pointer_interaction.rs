// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! End-to-end headless GUI pointer-interaction integration tests (`egui::Context::run_ui`)
//! proving `StudioApp::render_ui` -> `ToolCore` -> `TransientOverlay` -> `CommandHistory`
//! -> `DocumentInvalidation::Targeted` without mid-drag `Document` mutation or full-cache
//! invalidations.

use coree_app::state::StudioApp;
use coree_core::{
    Affine2D, Color, ColorMode, CommandOp, CornerStyle, Document, Fill, NodePayload, ObjectId,
    OutlineStyle, ParametricShape, Point2D, SceneObject,
};
use egui::{pos2, vec2, Context, Event, Modifiers, Pos2, RawInput, Rect};

struct UiDriver {
    ctx: Context,
    app: StudioApp,
    time: f64,
    screen: Rect,
}

impl UiDriver {
    fn new() -> Self {
        let mut app = StudioApp::new_headless();
        app.new_document(Document::new(
            "Pointer Test",
            210.0,
            297.0,
            ColorMode::Cmyk,
            300.0,
        ));
        app.show_welcome = false;
        app.show_dockers = false;
        app.fonts_configured = true;
        {
            let tab = app.active_tab_mut().unwrap();
            tab.snap_options.grid = false;
            tab.snap_options.guidelines = false;
            tab.snap_options.page_edges = false;
            tab.snap_options.objects = false;
            tab.snap_options.nodes = false;
            tab.snap_options.dynamic_guides = false;
        }
        let mut driver = Self {
            ctx: Context::default(),
            app,
            time: 0.0,
            screen: Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0)),
        };
        // Frame 0: establish layout, canvas_rect, and initial full tile render
        driver.step(Vec::new(), Modifiers::NONE);
        driver
    }

    fn step(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        let _ = self.step_with_output(events, modifiers);
    }

    fn step_with_output(
        &mut self,
        mut events: Vec<Event>,
        modifiers: Modifiers,
    ) -> egui::FullOutput {
        self.time += 0.02;
        if modifiers != Modifiers::NONE {
            events.insert(0, Event::ModifiersChanged(modifiers));
        }
        let input = RawInput {
            screen_rect: Some(self.screen),
            time: Some(self.time),
            events,
            ..RawInput::default()
        };
        let mut out = self.ctx.run_ui(input, |ui| self.app.render_ui(ui));
        out.textures_delta.clear();
        out
    }
    fn world_to_pos2(&self, world: Point2D) -> Pos2 {
        let [x, y] = self
            .app
            .active_tab()
            .unwrap()
            .world_to_canvas_screen_pos(world)
            .expect("canvas_rect must be populated after first render_ui frame");
        pos2(x, y)
    }

    fn add_rect(&mut self, name: &str, x: f64, y: f64, w: f64, h: f64, color: Color) -> ObjectId {
        let lid = self.app.active_tab().unwrap().active_layer_id;
        let obj = SceneObject::new(
            ObjectId(0),
            name,
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(color))
        .with_outline(OutlineStyle::default())
        .with_transform(Affine2D::translate(x, y));
        self.app
            .execute_command(CommandOp::create_object(obj, lid))
            .unwrap();
        *self
            .app
            .active_tab()
            .unwrap()
            .document
            .objects()
            .keys()
            .last()
            .unwrap()
    }
}

fn find_text_rect_in_shapes(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<Rect> {
    fn walk(shape: &egui::Shape, needle: &str) -> Option<Rect> {
        match shape {
            egui::Shape::Text(t) if t.galley.text().contains(needle) => {
                Some(t.visual_bounding_rect())
            }
            egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
            _ => None,
        }
    }
    shapes.iter().find_map(|cs| walk(&cs.shape, needle))
}

fn render_fresh_tile_rgba(d: &UiDriver) -> Vec<u8> {
    use coree_render::{DirtyTileCache, SceneRenderer};

    let tab = d.app.active_tab().unwrap();
    let mut fresh = DirtyTileCache::new(tab.viewport.width_px, tab.viewport.height_px);
    let mut renderer = SceneRenderer::new();
    fresh.render_dirty_tiles(
        &tab.document,
        &tab.viewport,
        tab.view_mode,
        &tab.render_options,
        &mut renderer,
    );
    fresh.composite_to_rgba().rgba
}

#[path = "pointer_interaction/canvas_overlays.rs"]
mod canvas_overlays;
#[path = "pointer_interaction/core_and_zoom.rs"]
mod core_and_zoom;
#[path = "pointer_interaction/w5_interactions.rs"]
mod w5_interactions;
