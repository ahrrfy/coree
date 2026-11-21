// SPDX-License-Identifier: MIT OR Apache-2.0

//! Dirty-Tile viewport rasterization cache (`256 × 256` tiles) with spatial
//! R-Tree frustum culling (`coree_core::SpatialIndex`) and LOD drag degradation.

use crate::backend::{CanvasBackend, PixelRect, RgbaBuffer, TinySkiaBackend};
use crate::renderer::{compute_page_layouts, RenderOptions, SceneRenderer};
use crate::viewport::{ViewMode, Viewport};
use coree_core::{
    Affine2D, BuiltinIccProfile, Document, GuideOrientation, PerspectiveGridMode, Rect2D,
    RenderingIntent, SpatialIndex,
};
use std::collections::HashMap;

/// Tile edge length in screen pixels (`256 × 256`).
pub const TILE_SIZE_PX: u32 = 256;

/// Single cached tile inside [`DirtyTileCache`].
#[derive(Debug, Clone)]
pub struct CachedTile {
    pub col: u32,
    pub row: u32,
    pub rect_px: PixelRect,
    pub world_rect_mm: Rect2D,
    pub buffer: RgbaBuffer,
    pub dirty: bool,
}

/// Snapshot of viewport, active page, overlay, and display state used to auto-invalidate
/// cached tiles whenever camera, page selection, multipage mode, or render options change.
#[derive(Debug, Clone, PartialEq)]
struct ViewportKey {
    width_px: u32,
    height_px: u32,
    pan_x_bits: u64,
    pan_y_bits: u64,
    zoom_bits: u64,
    rot_bits: u64,
    dpi_bits: u64,
    lod_dragging: bool,
    multipage_view: bool,
    view_mode: ViewMode,
    active_page_index: usize,
    page_count: usize,
    active_page_w_bits: u64,
    active_page_h_bits: u64,
    active_page_bleed_bits: u64,
    show_page_shadow: bool,
    show_bleed_guides: bool,
    show_margin_guides: bool,
    margin_mm_bits: u64,
    show_grid: bool,
    show_guidelines: bool,
    show_perspective_grid: bool,
    canvas_bg: [u8; 4],
    page_bg: [u8; 4],
    wireframe_stroke_color: [u8; 4],
    multipage_gap_bits: u64,
    grid_visible: bool,
    grid_show_dots: bool,
    grid_spacing_x_bits: u64,
    grid_spacing_y_bits: u64,
    grid_subdivisions: u32,
    perspective_grid_enabled: bool,
    perspective_grid_mode: PerspectiveGridMode,
    perspective_grid_density: u32,
    perspective_grid_horizon_bits: u64,
    perspective_grid_opacity_bits: u32,
    guides_len: usize,
    guides_hash: u64,
    pages_hash: u64,
    soft_proof_cmyk: bool,
    gamut_warning: bool,
    proof_profile: BuiltinIccProfile,
    proof_intent: RenderingIntent,
}

impl ViewportKey {
    fn from(doc: &Document, vp: &Viewport, mode: ViewMode, options: &RenderOptions) -> Self {
        let active_page = doc.pages.get(doc.active_page_index);
        let mut guides_hash = 0xcbf2_9ce4_8422_2325_u64;
        let mix = |h: &mut u64, v: u64| {
            *h ^= v;
            *h = h.wrapping_mul(0x0100_0000_01b3);
        };
        for g in &doc.guides {
            mix(&mut guides_hash, g.id);
            match g.orientation {
                GuideOrientation::Horizontal(y) => {
                    mix(&mut guides_hash, 1);
                    mix(&mut guides_hash, y.to_bits());
                }
                GuideOrientation::Vertical(x) => {
                    mix(&mut guides_hash, 2);
                    mix(&mut guides_hash, x.to_bits());
                }
                GuideOrientation::Angled { origin, angle_deg } => {
                    mix(&mut guides_hash, 3);
                    mix(&mut guides_hash, origin.x.to_bits());
                    mix(&mut guides_hash, origin.y.to_bits());
                    mix(&mut guides_hash, angle_deg.to_bits());
                }
            }
        }
        let mut pages_hash = 0xcbf2_9ce4_8422_2325_u64;
        for p in &doc.pages {
            mix(&mut pages_hash, p.id.0);
            mix(&mut pages_hash, p.width_mm.to_bits());
            mix(&mut pages_hash, p.height_mm.to_bits());
            mix(&mut pages_hash, p.bleed_mm.to_bits());
        }
        Self {
            width_px: vp.width_px,
            height_px: vp.height_px,
            pan_x_bits: vp.pan_mm.x.to_bits(),
            pan_y_bits: vp.pan_mm.y.to_bits(),
            zoom_bits: vp.zoom.to_bits(),
            rot_bits: vp.rotation_deg.to_bits(),
            dpi_bits: vp.dpi.to_bits(),
            lod_dragging: vp.lod_dragging,
            multipage_view: vp.multipage_view || doc.multipage_view,
            view_mode: mode,
            active_page_index: doc.active_page_index,
            page_count: doc.pages.len(),
            active_page_w_bits: active_page.map_or(0, |p| p.width_mm.to_bits()),
            active_page_h_bits: active_page.map_or(0, |p| p.height_mm.to_bits()),
            active_page_bleed_bits: active_page.map_or(0, |p| p.bleed_mm.to_bits()),
            show_page_shadow: options.show_page_shadow,
            show_bleed_guides: options.show_bleed_guides,
            show_margin_guides: options.show_margin_guides,
            margin_mm_bits: options.margin_mm.to_bits(),
            show_grid: options.show_grid,
            show_guidelines: options.show_guidelines,
            show_perspective_grid: options.show_perspective_grid,
            canvas_bg: options.canvas_bg.to_rgba8(),
            page_bg: options.page_bg.to_rgba8(),
            wireframe_stroke_color: options.wireframe_stroke_color.to_rgba8(),
            multipage_gap_bits: options.multipage_gap_mm.to_bits(),
            grid_visible: doc.grid.visible,
            grid_show_dots: doc.grid.show_dots,
            grid_spacing_x_bits: doc.grid.spacing_x_mm.to_bits(),
            grid_spacing_y_bits: doc.grid.spacing_y_mm.to_bits(),
            grid_subdivisions: doc.grid.subdivisions,
            perspective_grid_enabled: doc.perspective_grid.enabled,
            perspective_grid_mode: doc.perspective_grid.mode,
            perspective_grid_density: doc.perspective_grid.density,
            perspective_grid_horizon_bits: doc.perspective_grid.horizon_y_mm.to_bits(),
            perspective_grid_opacity_bits: doc.perspective_grid.opacity.to_bits(),
            guides_len: doc.guides.len(),
            guides_hash,
            pages_hash,
            soft_proof_cmyk: options.soft_proof_cmyk,
            gamut_warning: options.gamut_warning,
            proof_profile: options.proof_profile,
            proof_intent: options.proof_intent,
        }
    }
}

/// Spatial R-Tree frustum-culled `256 × 256` dirty-tile cache for smooth 60fps canvas rendering.
pub struct DirtyTileCache {
    width_px: u32,
    height_px: u32,
    cols: u32,
    rows: u32,
    tiles: Vec<CachedTile>,
    last_key: Option<ViewportKey>,
    /// Cached per-page spatial indices rebuilt when explicitly invalidated or on full dirty.
    spatial_indices: HashMap<usize, SpatialIndex>,
    spatial_dirty: bool,
}

impl Default for DirtyTileCache {
    fn default() -> Self {
        Self::new(800, 600)
    }
}

impl DirtyTileCache {
    /// Initializes a dirty tile grid covering `width_px × height_px`.
    #[must_use]
    pub fn new(width_px: u32, height_px: u32) -> Self {
        let mut cache = Self {
            width_px: 0,
            height_px: 0,
            cols: 0,
            rows: 0,
            tiles: Vec::new(),
            last_key: None,
            spatial_indices: HashMap::new(),
            spatial_dirty: true,
        };
        cache.rebuild_grid(&Viewport::new(width_px.max(1), height_px.max(1)));
        cache
    }

    /// Total number of tiles in the current viewport grid.
    #[must_use]
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Number of tiles currently marked dirty and awaiting re-rasterization.
    #[must_use]
    pub fn dirty_tile_count(&self) -> usize {
        self.tiles.iter().filter(|t| t.dirty).count()
    }

    /// Immutable slice of cached tiles (for diagnostics / GPU texture upload).
    #[must_use]
    pub fn tiles(&self) -> &[CachedTile] {
        &self.tiles
    }

    /// Marks every tile whose world bounding box intersects `rect_mm` (plus AA margin) as dirty.
    pub fn mark_dirty(&mut self, rect_mm: Rect2D) {
        if rect_mm.is_empty() {
            return;
        }
        let expanded = rect_mm.expand(1.0);
        for tile in &mut self.tiles {
            if tile.world_rect_mm.intersects(expanded) {
                tile.dirty = true;
            }
        }
        self.spatial_dirty = true;
    }

    /// Marks every tile in the viewport dirty and schedules a spatial index refresh.
    pub fn mark_all_dirty(&mut self) {
        for tile in &mut self.tiles {
            tile.dirty = true;
        }
        self.spatial_dirty = true;
    }

    /// Re-renders ONLY dirty `256 × 256` tiles using R-Tree frustum culling and returns
    /// the number of tiles actually re-rendered.
    pub fn render_dirty_tiles(
        &mut self,
        doc: &Document,
        viewport: &Viewport,
        view_mode: ViewMode,
        options: &RenderOptions,
        renderer: &mut SceneRenderer,
    ) -> usize {
        let key = ViewportKey::from(doc, viewport, view_mode, options);
        if self.last_key.as_ref() != Some(&key) {
            if self.last_key.as_ref().is_none_or(|prev| {
                prev.multipage_view != key.multipage_view
                    || prev.page_count != key.page_count
                    || prev.active_page_index != key.active_page_index
                    || prev.pages_hash != key.pages_hash
            }) {
                self.spatial_dirty = true;
            }
            self.rebuild_grid(viewport);
            self.last_key = Some(key);
        }

        if self.dirty_tile_count() == 0 {
            return 0;
        }

        // Refresh R-Tree spatial indices when document objects changed
        if self.spatial_dirty || self.spatial_indices.is_empty() {
            self.spatial_indices.clear();
            let multipage = viewport.multipage_view || doc.multipage_view;
            let layouts = compute_page_layouts(doc, multipage, options.multipage_gap_mm);
            for layout in layouts {
                self.spatial_indices.insert(
                    layout.page_index,
                    doc.build_spatial_index(layout.page_index),
                );
            }
            self.spatial_dirty = false;
        }

        let world_to_screen = viewport.world_to_screen_transform();
        let mut rendered_count = 0usize;

        for tile in &mut self.tiles {
            if !tile.dirty {
                continue;
            }
            let Ok(mut backend) = TinySkiaBackend::new(tile.rect_px.width, tile.rect_px.height)
            else {
                continue;
            };
            let tile_transform =
                Affine2D::translate(-f64::from(tile.rect_px.x), -f64::from(tile.rect_px.y))
                    .compose(&world_to_screen);

            renderer.render_document_with_transform(
                doc,
                viewport,
                tile_transform,
                view_mode,
                options,
                &mut backend,
                Some(tile.world_rect_mm),
                Some(&self.spatial_indices),
            );

            tile.buffer = backend.into_rgba();
            tile.dirty = false;
            rendered_count += 1;
        }

        rendered_count
    }

    /// Stitches all cached tiles into a contiguous full-viewport straight [`RgbaBuffer`].
    #[must_use]
    pub fn composite_to_rgba(&self) -> RgbaBuffer {
        let mut out = RgbaBuffer::new_transparent(self.width_px, self.height_px);
        let full_stride = (self.width_px as usize) * 4;
        for tile in &self.tiles {
            if tile.buffer.is_empty() {
                continue;
            }
            let tx = tile.rect_px.x.max(0) as usize;
            let ty = tile.rect_px.y.max(0) as usize;
            let tw = tile.buffer.width as usize;
            let th = tile.buffer.height as usize;
            let tile_stride = tw * 4;
            for row in 0..th {
                let dst_y = ty + row;
                if dst_y >= self.height_px as usize {
                    break;
                }
                let copy_w = tw.min((self.width_px as usize).saturating_sub(tx));
                let src_off = row * tile_stride;
                let dst_off = dst_y * full_stride + tx * 4;
                out.rgba[dst_off..dst_off + copy_w * 4]
                    .copy_from_slice(&tile.buffer.rgba[src_off..src_off + copy_w * 4]);
            }
        }
        out
    }

    /// Convenience helper: updates dirty tiles and returns the stitched viewport [`RgbaBuffer`].
    #[must_use]
    pub fn render_viewport(
        &mut self,
        doc: &Document,
        viewport: &Viewport,
        view_mode: ViewMode,
        options: &RenderOptions,
        renderer: &mut SceneRenderer,
    ) -> RgbaBuffer {
        let _ = self.render_dirty_tiles(doc, viewport, view_mode, options, renderer);
        self.composite_to_rgba()
    }

    fn rebuild_grid(&mut self, viewport: &Viewport) {
        self.width_px = viewport.width_px.max(1);
        self.height_px = viewport.height_px.max(1);
        self.cols = self.width_px.div_ceil(TILE_SIZE_PX);
        self.rows = self.height_px.div_ceil(TILE_SIZE_PX);
        let screen_to_world = viewport.screen_to_world_transform();

        self.tiles.clear();
        self.tiles
            .reserve((self.cols as usize).saturating_mul(self.rows as usize));

        for row in 0..self.rows {
            let y = row * TILE_SIZE_PX;
            let h = TILE_SIZE_PX.min(self.height_px.saturating_sub(y));
            for col in 0..self.cols {
                let x = col * TILE_SIZE_PX;
                let w = TILE_SIZE_PX.min(self.width_px.saturating_sub(x));
                let rect_px = PixelRect::new(x as i32, y as i32, w, h);
                let screen_rect =
                    Rect2D::from_xywh(f64::from(x), f64::from(y), f64::from(w), f64::from(h));
                let world_rect_mm = screen_to_world.transform_rect(screen_rect);
                self.tiles.push(CachedTile {
                    col,
                    row,
                    rect_px,
                    world_rect_mm,
                    buffer: RgbaBuffer::new_transparent(w, h),
                    dirty: true,
                });
            }
        }
    }
}
