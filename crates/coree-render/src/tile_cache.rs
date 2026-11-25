// SPDX-License-Identifier: MIT OR Apache-2.0

//! Continuous world-space Dirty-Tile viewport rasterization cache (`256 × 256` tiles)
//! keyed by [`WorldTileKey`] with spatial R-Tree frustum culling (`coree_core::SpatialIndex`)
//! and LOD drag degradation.

use crate::backend::{CanvasBackend, PixelRect, RenderBackend, RenderBackendKind, RgbaBuffer};
use crate::renderer::{compute_page_layouts, RenderOptions, SceneRenderer};
use crate::viewport::{ViewMode, Viewport};
use coree_core::math::MM_PER_INCH;
use coree_core::{
    Affine2D, BuiltinIccProfile, Document, GuideOrientation, PerspectiveGridMode, Point2D, Rect2D,
    SpatialIndex,
};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Computes the conservative full visual world-space bounding box of `id`, including
/// [`coree_core::Effect::Symmetry`] clones, [`coree_core::Effect::Blend`] targets/paths,
/// [`coree_core::Effect::Extrude25D`] vanishing points, effect overhangs, and scaled child strokes.
#[must_use]
pub fn object_visual_world_bounds(doc: &Document, id: coree_core::ObjectId) -> Option<Rect2D> {
    doc.visual_world_bounds(id)
}

/// Tile edge length in canonical tile pixels (`256 × 256`).
pub const TILE_SIZE_PX: u32 = 256;

/// Maximum number of world tiles retained in the LRU cache across pan/zoom.
const MAX_CACHED_WORLD_TILES: usize = 1024;

/// Persistent world-space tile cache key (`IMP-02`).
///
/// Independent of screen viewport position/dimensions (`pan_mm`, `width_px`, `height_px`)
/// so panning and zooming across cached LODs never discard unaffected tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorldTileKey {
    pub lod_level: i32,
    pub tile_x: i64,
    pub tile_y: i64,
    pub view_mode: ViewMode,
    pub proof_hash: u64,
    pub icc_hash: u64,
}

/// Single cached viewport-projected tile view returned by [`DirtyTileCache::tiles`].
#[derive(Debug, Clone)]
pub struct CachedTile {
    pub col: u32,
    pub row: u32,
    pub rect_px: PixelRect,
    pub world_rect_mm: Rect2D,
    pub buffer: RgbaBuffer,
    pub dirty: bool,
}

#[derive(Debug, Clone)]
struct CachedWorldTile {
    tile_zoom: f64,
    world_rect_mm: Rect2D,
    buffer: RgbaBuffer,
    dirty: bool,
    last_used: u64,
}

/// Scene chrome, page layout, guides, and color-management state (excluding camera pan/zoom/size
/// and `lod_dragging`) so pan/zoom never invalidates cached world tiles.
#[derive(Debug, Clone, PartialEq)]
struct SceneChromeKey {
    rot_bits: u64,
    dpi_bits: u64,
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
    proof_hash: u64,
    icc_hash: u64,
}

#[inline]
fn fnv_mix(h: &mut u64, v: u64) {
    *h ^= v;
    *h = h.wrapping_mul(0x0100_0000_01b3);
}

fn compute_proof_hash(options: &RenderOptions) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    fnv_mix(&mut h, u64::from(options.soft_proof_cmyk));
    fnv_mix(&mut h, u64::from(options.gamut_warning));
    let prof_code = match options.proof_profile {
        BuiltinIccProfile::Srgb => 0u64,
        BuiltinIccProfile::DisplayP3 => 1,
        BuiltinIccProfile::IsoCoatedV2Cmyk => 2,
        BuiltinIccProfile::UsWebCoatedSwopCmyk => 3,
    };
    fnv_mix(&mut h, prof_code);
    fnv_mix(&mut h, u64::from(options.proof_intent.to_u32()));
    h
}

fn compute_icc_hash(doc: &Document, options: &RenderOptions) -> u64 {
    let mut icc_hash = 0xcbf2_9ce4_8422_2325_u64;
    for slot in [
        doc.rgb_icc_profile(),
        doc.cmyk_icc_profile(),
        doc.gray_icc_profile(),
        doc.custom_icc_profile(),
        options.display_icc_profile.as_deref(),
    ] {
        match slot {
            Some(bytes) => {
                fnv_mix(&mut icc_hash, 1);
                fnv_mix(&mut icc_hash, bytes.len() as u64);
                for chunk in bytes.chunks(8) {
                    let mut buf = [0u8; 8];
                    buf[..chunk.len()].copy_from_slice(chunk);
                    fnv_mix(&mut icc_hash, u64::from_le_bytes(buf));
                }
            }
            None => fnv_mix(&mut icc_hash, 0),
        }
    }
    fnv_mix(&mut icc_hash, u64::from(doc.rendering_intent().to_u32()));
    fnv_mix(&mut icc_hash, u64::from(doc.black_point_compensation()));
    fnv_mix(
        &mut icc_hash,
        u64::from(options.display_icc_intent.to_u32()),
    );
    icc_hash
}

impl SceneChromeKey {
    fn from(doc: &Document, vp: &Viewport, mode: ViewMode, options: &RenderOptions) -> Self {
        let active_page = doc.active_page();
        let grid = doc.grid();
        let perspective_grid = doc.perspective_grid();
        let guides = doc.guides();
        let pages = doc.pages();
        let mut guides_hash = 0xcbf2_9ce4_8422_2325_u64;
        for g in guides {
            fnv_mix(&mut guides_hash, g.id);
            match g.orientation {
                GuideOrientation::Horizontal(y) => {
                    fnv_mix(&mut guides_hash, 1);
                    fnv_mix(&mut guides_hash, y.to_bits());
                }
                GuideOrientation::Vertical(x) => {
                    fnv_mix(&mut guides_hash, 2);
                    fnv_mix(&mut guides_hash, x.to_bits());
                }
                GuideOrientation::Angled { origin, angle_deg } => {
                    fnv_mix(&mut guides_hash, 3);
                    fnv_mix(&mut guides_hash, origin.x.to_bits());
                    fnv_mix(&mut guides_hash, origin.y.to_bits());
                    fnv_mix(&mut guides_hash, angle_deg.to_bits());
                }
            }
        }
        let mut pages_hash = 0xcbf2_9ce4_8422_2325_u64;
        let ds = doc.drawing_scale();
        fnv_mix(&mut pages_hash, ds.numerator.to_bits());
        fnv_mix(&mut pages_hash, ds.denominator.to_bits());
        for p in pages {
            fnv_mix(&mut pages_hash, p.id.0);
            fnv_mix(&mut pages_hash, p.width_mm.to_bits());
            fnv_mix(&mut pages_hash, p.height_mm.to_bits());
            fnv_mix(&mut pages_hash, p.bleed_mm.to_bits());
            fnv_mix(&mut pages_hash, u64::from(p.facing_pages));
            fnv_mix(&mut pages_hash, u64::from(p.printable_background));
            let bg_repr = format!("{:?}", p.background_fill);
            fnv_mix(&mut pages_hash, bg_repr.len() as u64);
            for chunk in bg_repr.as_bytes().chunks(8) {
                let mut buf = [0u8; 8];
                buf[..chunk.len()].copy_from_slice(chunk);
                fnv_mix(&mut pages_hash, u64::from_le_bytes(buf));
            }
        }
        let proof_hash = compute_proof_hash(options);
        let icc_hash = compute_icc_hash(doc, options);
        Self {
            rot_bits: vp.rotation_deg.to_bits(),
            dpi_bits: vp.dpi.to_bits(),
            multipage_view: vp.multipage_view || doc.multipage_view(),
            view_mode: mode,
            active_page_index: doc.active_page_index(),
            page_count: pages.len(),
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
            grid_visible: grid.visible,
            grid_show_dots: grid.show_dots,
            grid_spacing_x_bits: grid.spacing_x_mm.to_bits(),
            grid_spacing_y_bits: grid.spacing_y_mm.to_bits(),
            grid_subdivisions: grid.subdivisions,
            perspective_grid_enabled: perspective_grid.enabled,
            perspective_grid_mode: perspective_grid.mode,
            perspective_grid_density: perspective_grid.density,
            perspective_grid_horizon_bits: perspective_grid.horizon_y_mm.to_bits(),
            perspective_grid_opacity_bits: perspective_grid.opacity.to_bits(),
            guides_len: guides.len(),
            guides_hash,
            pages_hash,
            proof_hash,
            icc_hash,
        }
    }
}

#[inline]
fn compute_lod_and_zoom(zoom: f64, lod_dragging: bool) -> (i32, f64) {
    let z = if zoom.is_finite() {
        zoom.clamp(0.01, 256.0)
    } else {
        1.0
    };
    let octave = z.log2().floor() as i32;
    if lod_dragging {
        return (octave, 2.0_f64.powi(octave));
    }
    if (z - 2.0_f64.powi(octave)).abs() <= 1e-6 {
        return (octave, 2.0_f64.powi(octave));
    }
    if (z - 2.0_f64.powi(octave + 1)).abs() <= 1e-6 {
        return (octave + 1, 2.0_f64.powi(octave + 1));
    }
    let lod = 10_000 + (z * 100_000.0).round() as i32;
    (lod, z)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DocSignature {
    id_bytes: [u8; 16],
    objects_len: usize,
    pages_len: usize,
    spatial_revision: u64,
}

impl DocSignature {
    fn from(doc: &Document) -> Self {
        Self {
            id_bytes: *doc.id().as_bytes(),
            objects_len: doc.objects().len(),
            pages_len: doc.pages().len(),
            spatial_revision: doc.spatial_revision(),
        }
    }
}

/// Spatial R-Tree frustum-culled persistent world-space `256 × 256` dirty-tile cache.
pub struct DirtyTileCache {
    width_px: u32,
    height_px: u32,
    cols: u32,
    rows: u32,
    world_tiles: HashMap<WorldTileKey, CachedWorldTile>,
    active_keys: Vec<WorldTileKey>,
    active_tiles: Vec<CachedTile>,
    frame_counter: u64,
    last_chrome_key: Option<SceneChromeKey>,
    last_doc_sig: Option<DocSignature>,
    last_viewport: Viewport,
    last_tile_zoom: f64,
    last_lod_level: i32,
    backend_kind: RenderBackendKind,
    /// Cached per-page spatial indices rebuilt ONLY when `Document` mutates or pages change.
    spatial_indices: HashMap<usize, SpatialIndex>,
    spatial_dirty: bool,
}

impl Default for DirtyTileCache {
    fn default() -> Self {
        Self::new(800, 600)
    }
}

impl DirtyTileCache {
    /// Initializes a world-space dirty tile cache sized for `width_px × height_px`.
    #[must_use]
    pub fn new(width_px: u32, height_px: u32) -> Self {
        Self::with_backend_kind(width_px, height_px, RenderBackendKind::from_env())
    }

    /// Initializes a world-space dirty tile cache with an explicit [`RenderBackendKind`].
    #[must_use]
    pub fn with_backend_kind(
        width_px: u32,
        height_px: u32,
        backend_kind: RenderBackendKind,
    ) -> Self {
        let vp = Viewport::new(width_px.max(1), height_px.max(1));
        let mut cache = Self {
            width_px: vp.width_px,
            height_px: vp.height_px,
            cols: 0,
            rows: 0,
            world_tiles: HashMap::new(),
            active_keys: Vec::new(),
            active_tiles: Vec::new(),
            frame_counter: 0,
            last_chrome_key: None,
            last_doc_sig: None,
            last_viewport: vp,
            last_tile_zoom: 1.0,
            last_lod_level: 0,
            backend_kind,
            spatial_indices: HashMap::new(),
            spatial_dirty: true,
        };
        cache.sync_active_grid(&vp, ViewMode::Enhanced, 0, 0, true);
        cache
    }

    /// Returns the configured [`RenderBackendKind`].
    #[must_use]
    pub fn backend_kind(&self) -> RenderBackendKind {
        self.backend_kind
    }

    /// Total number of active tiles in the current viewport frustum.
    #[must_use]
    pub fn tile_count(&self) -> usize {
        self.active_keys.len()
    }

    /// Number of active viewport tiles currently marked dirty and awaiting re-rasterization.
    #[must_use]
    pub fn dirty_tile_count(&self) -> usize {
        self.active_keys
            .iter()
            .filter(|k| self.world_tiles.get(k).is_some_and(|t| t.dirty))
            .count()
    }

    /// Total number of world tiles currently retained across all pan/zoom LODs in the LRU pool.
    #[must_use]
    pub fn cached_world_tile_count(&self) -> usize {
        self.world_tiles.len()
    }

    /// Returns the currently active [`WorldTileKey`]s for the visible viewport.
    #[must_use]
    pub fn active_world_tile_keys(&self) -> &[WorldTileKey] {
        &self.active_keys
    }

    /// Immutable slice of active viewport tiles (for diagnostics / GPU texture upload).
    #[must_use]
    pub fn tiles(&self) -> &[CachedTile] {
        &self.active_tiles
    }

    /// Marks every cached world tile whose world bounding box intersects `rect_mm` (plus AA margin)
    /// as dirty WITHOUT marking `spatial_dirty` (`#11`).
    pub fn mark_dirty(&mut self, rect_mm: Rect2D) {
        if rect_mm.is_empty() {
            return;
        }
        let expanded = rect_mm.expand(1.0);
        for tile in self.world_tiles.values_mut() {
            if tile.world_rect_mm.intersects(expanded) {
                tile.dirty = true;
            }
        }
        for tile in &mut self.active_tiles {
            if tile.world_rect_mm.intersects(expanded) {
                tile.dirty = true;
            }
        }
    }

    /// Marks every tile intersecting `id`'s full visual world bounds (including `Effect::Symmetry`
    /// clones, `Effect::Blend` targets/paths, `Effect::Extrude25D` vanishing points, effect
    /// overhang padding, scaled child outlines, ancestor container `Symmetry`/effect bounds, and
    /// transitive scene dependents such as `Blend`, `Connector`, `TextOnPath`, `linked_next_frame`,
    /// and `SymbolInstance` references) as dirty.
    pub fn mark_object_dirty(&mut self, doc: &Document, id: coree_core::ObjectId) {
        use coree_core::{Effect, NodePayload, ObjectId};
        use std::collections::VecDeque;

        let mut dependents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        let mut wrap_frames: Vec<(ObjectId, Rect2D)> = Vec::new();
        for (&oid, obj) in doc.objects() {
            match &obj.payload {
                NodePayload::Connector(c) => {
                    if let Some(fid) = c.from_obj {
                        dependents.entry(fid).or_default().push(oid);
                    }
                    if let Some(tid) = c.to_obj {
                        dependents.entry(tid).or_default().push(oid);
                    }
                }
                NodePayload::TextOnPath(top) => {
                    dependents.entry(top.path_id).or_default().push(oid);
                }
                NodePayload::ParagraphText(p) => {
                    if let Some(next_id) = p.linked_next_frame {
                        if next_id.0 != 0 && next_id != oid {
                            dependents.entry(oid).or_default().push(next_id);
                        }
                    }
                    if p.wrap_mode != coree_core::TextWrapMode::None {
                        if let Some(wb) = doc.world_object_bounds(oid) {
                            wrap_frames.push((oid, wb));
                        }
                    }
                }
                NodePayload::Clone { master_id, .. } => {
                    dependents.entry(*master_id).or_default().push(oid);
                }
                NodePayload::SymbolInstance(sym_id) => {
                    if let Some(sym) = doc.symbol(*sym_id) {
                        for sub in &sym.objects {
                            match &sub.payload {
                                NodePayload::Connector(c) => {
                                    if let Some(fid) = c.from_obj {
                                        dependents.entry(fid).or_default().push(oid);
                                    }
                                    if let Some(tid) = c.to_obj {
                                        dependents.entry(tid).or_default().push(oid);
                                    }
                                }
                                NodePayload::TextOnPath(top) => {
                                    dependents.entry(top.path_id).or_default().push(oid);
                                }
                                NodePayload::Clone { master_id, .. }
                                    if !sym.objects.iter().any(|s| s.id == *master_id) =>
                                {
                                    dependents.entry(*master_id).or_default().push(oid);
                                }
                                _ => {}
                            }
                            for eff in &sub.effects {
                                if let Effect::Blend {
                                    target_id,
                                    along_path,
                                    ..
                                } = eff
                                {
                                    dependents.entry(*target_id).or_default().push(oid);
                                    if let Some(apid) = along_path {
                                        dependents.entry(*apid).or_default().push(oid);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            for eff in &obj.effects {
                if let Effect::Blend {
                    target_id,
                    along_path,
                    ..
                } = eff
                {
                    dependents.entry(*target_id).or_default().push(oid);
                    if let Some(apid) = along_path {
                        dependents.entry(*apid).or_default().push(oid);
                    }
                }
            }
        }

        let mut visited: HashSet<ObjectId> = HashSet::from([id]);
        let mut queue: VecDeque<(ObjectId, bool)> = VecDeque::from([(id, true)]);
        let mut any_marked = false;

        while let Some((cur, expand_children)) = queue.pop_front() {
            if let Some(bb) = object_visual_world_bounds(doc, cur) {
                self.mark_dirty(bb);
                any_marked = true;
            }
            if let Some(pid) = doc.parent_container_of(cur) {
                if visited.insert(pid) {
                    queue.push_back((pid, false));
                }
            }
            if expand_children {
                if let Some(obj) = doc.object(cur) {
                    let children: &[ObjectId] = match &obj.payload {
                        NodePayload::Group(c) => c,
                        NodePayload::PowerClip { contents, .. } => contents,
                        _ => &[],
                    };
                    for &cid in children {
                        if visited.insert(cid) {
                            queue.push_back((cid, true));
                        }
                    }
                }
            }
            if let Some(deps) = dependents.get(&cur) {
                for &dep in deps {
                    if visited.insert(dep) {
                        queue.push_back((dep, false));
                    }
                }
            }
            if !wrap_frames.is_empty()
                && doc
                    .object(cur)
                    .is_some_and(|o| !matches!(o.payload, NodePayload::ParagraphText(_)))
            {
                if let Some(cwb) = doc.world_object_bounds(cur) {
                    for &(fid, fwb) in &wrap_frames {
                        if cwb.intersects(fwb) && visited.insert(fid) {
                            queue.push_back((fid, false));
                        }
                    }
                }
            }
        }

        if !any_marked {
            self.spatial_dirty = true;
        }
    }

    /// Marks every cached world tile dirty and schedules a spatial index refresh.
    pub fn mark_all_dirty(&mut self) {
        for tile in self.world_tiles.values_mut() {
            tile.dirty = true;
        }
        for tile in &mut self.active_tiles {
            tile.dirty = true;
        }
        self.spatial_dirty = true;
    }

    /// Computes the full visual world-space bounding box of `id` (alias for [`object_visual_world_bounds`]).
    #[must_use]
    pub fn visual_world_bounds(doc: &Document, id: coree_core::ObjectId) -> Option<Rect2D> {
        object_visual_world_bounds(doc, id)
    }

    /// Re-renders ONLY dirty `256 × 256` world tiles visible in `viewport` using R-Tree frustum
    /// culling and returns the number of tiles actually re-rendered.
    pub fn render_dirty_tiles(
        &mut self,
        doc: &Document,
        viewport: &Viewport,
        view_mode: ViewMode,
        options: &RenderOptions,
        renderer: &mut SceneRenderer,
    ) -> usize {
        let chrome_key = SceneChromeKey::from(doc, viewport, view_mode, options);
        if self.last_chrome_key.as_ref() != Some(&chrome_key) {
            if self.last_chrome_key.as_ref().is_none_or(|prev| {
                prev.multipage_view != chrome_key.multipage_view
                    || prev.page_count != chrome_key.page_count
                    || prev.active_page_index != chrome_key.active_page_index
                    || prev.pages_hash != chrome_key.pages_hash
            }) {
                self.spatial_dirty = true;
            }
            self.world_tiles.clear();
            self.last_chrome_key = Some(chrome_key.clone());
        }

        let doc_sig = DocSignature::from(doc);
        if self.last_doc_sig != Some(doc_sig) {
            self.spatial_dirty = true;
            self.last_doc_sig = Some(doc_sig);
        }

        self.sync_active_grid(
            viewport,
            view_mode,
            chrome_key.proof_hash,
            chrome_key.icc_hash,
            false,
        );

        let dirty_keys: Vec<WorldTileKey> = self
            .active_keys
            .iter()
            .copied()
            .filter(|k| self.world_tiles.get(k).is_none_or(|t| t.dirty))
            .collect();
        if dirty_keys.is_empty() {
            return 0;
        }

        let multipage = viewport.multipage_view || doc.multipage_view();
        let layouts = compute_page_layouts(doc, multipage, options.multipage_gap_mm);

        // Refresh R-Tree spatial indices ONLY when document objects/pages actually changed
        if self.spatial_dirty || self.spatial_indices.is_empty() {
            self.spatial_indices.clear();
            for layout in &layouts {
                self.spatial_indices.insert(
                    layout.page_index,
                    doc.build_spatial_index(layout.page_index),
                );
            }
            self.spatial_dirty = false;
        }
        renderer.warm_page_cull_cache(doc, &layouts);

        let tile_zoom = self.last_tile_zoom;
        let s_tile = tile_zoom * viewport.dpi.clamp(1.0, 4800.0) / MM_PER_INCH;
        let mut tile_vp = *viewport;
        tile_vp.pan_mm = Point2D::ZERO;
        tile_vp.zoom = tile_zoom;
        tile_vp.rotation_deg = 0.0;
        tile_vp.width_px = TILE_SIZE_PX;
        tile_vp.height_px = TILE_SIZE_PX;

        let mut rendered_count = 0usize;
        let backend_kind = self.backend_kind;

        // Warm any unwarmed frozen lenses sequentially BEFORE Rayon across remaining tiles
        if renderer.has_unwarmed_frozen_lens(doc, multipage) {
            for &key in &dirty_keys {
                let Some(entry) = self.world_tiles.get_mut(&key) else {
                    continue;
                };
                if !entry.dirty {
                    continue;
                }
                if let Ok(mut backend) =
                    RenderBackend::new_for_kind(backend_kind, TILE_SIZE_PX, TILE_SIZE_PX)
                {
                    let tile_transform = Affine2D::translate(
                        -((key.tile_x * i64::from(TILE_SIZE_PX)) as f64),
                        -((key.tile_y * i64::from(TILE_SIZE_PX)) as f64),
                    )
                    .compose(&Affine2D::scale_uniform(s_tile));
                    renderer.render_document_with_transform(
                        doc,
                        &tile_vp,
                        tile_transform,
                        view_mode,
                        options,
                        &mut backend,
                        Some(entry.world_rect_mm),
                        Some(&self.spatial_indices),
                    );
                    entry.buffer = backend.into_rgba();
                    entry.dirty = false;
                    rendered_count += 1;
                }
                if !renderer.has_unwarmed_frozen_lens(doc, multipage) {
                    break;
                }
            }
        }

        let remaining_keys: Vec<WorldTileKey> = dirty_keys
            .into_iter()
            .filter(|k| self.world_tiles.get(k).is_some_and(|t| t.dirty))
            .collect();

        if remaining_keys.len() >= 2 {
            let rendered_counter = AtomicUsize::new(rendered_count);
            let spatial_ref = &self.spatial_indices;
            let world_rects: Vec<(WorldTileKey, Rect2D)> = remaining_keys
                .iter()
                .filter_map(|&k| self.world_tiles.get(&k).map(|t| (k, t.world_rect_mm)))
                .collect();

            let rendered_tiles: Vec<(WorldTileKey, RgbaBuffer)> = world_rects
                .into_par_iter()
                .map_init(
                    || {
                        let mut r = SceneRenderer::new();
                        r.copy_frozen_lenses_from(renderer);
                        r
                    },
                    |r, (key, world_rect_mm)| {
                        let Ok(mut backend) =
                            RenderBackend::new_for_kind(backend_kind, TILE_SIZE_PX, TILE_SIZE_PX)
                        else {
                            return (key, RgbaBuffer::new_transparent(TILE_SIZE_PX, TILE_SIZE_PX));
                        };
                        let tile_transform = Affine2D::translate(
                            -((key.tile_x * i64::from(TILE_SIZE_PX)) as f64),
                            -((key.tile_y * i64::from(TILE_SIZE_PX)) as f64),
                        )
                        .compose(&Affine2D::scale_uniform(s_tile));
                        r.render_document_with_transform(
                            doc,
                            &tile_vp,
                            tile_transform,
                            view_mode,
                            options,
                            &mut backend,
                            Some(world_rect_mm),
                            Some(spatial_ref),
                        );
                        rendered_counter.fetch_add(1, Ordering::Relaxed);
                        (key, backend.into_rgba())
                    },
                )
                .collect();

            for (key, buf) in rendered_tiles {
                if let Some(entry) = self.world_tiles.get_mut(&key) {
                    entry.buffer = buf;
                    entry.dirty = false;
                }
            }
            rendered_count = rendered_counter.load(Ordering::Relaxed);
        } else {
            for key in remaining_keys {
                let Some(entry) = self.world_tiles.get_mut(&key) else {
                    continue;
                };
                if let Ok(mut backend) =
                    RenderBackend::new_for_kind(backend_kind, TILE_SIZE_PX, TILE_SIZE_PX)
                {
                    let tile_transform = Affine2D::translate(
                        -((key.tile_x * i64::from(TILE_SIZE_PX)) as f64),
                        -((key.tile_y * i64::from(TILE_SIZE_PX)) as f64),
                    )
                    .compose(&Affine2D::scale_uniform(s_tile));
                    renderer.render_document_with_transform(
                        doc,
                        &tile_vp,
                        tile_transform,
                        view_mode,
                        options,
                        &mut backend,
                        Some(entry.world_rect_mm),
                        Some(&self.spatial_indices),
                    );
                    entry.buffer = backend.into_rgba();
                    entry.dirty = false;
                    rendered_count += 1;
                }
            }
        }

        self.refresh_active_tiles();
        rendered_count
    }

    /// Stitches visible world tiles into a contiguous full-viewport straight [`RgbaBuffer`].
    #[must_use]
    pub fn composite_to_rgba(&self) -> RgbaBuffer {
        let out_w = self.width_px.max(1);
        let out_h = self.height_px.max(1);
        let mut out = RgbaBuffer::new_transparent(out_w, out_h);

        let rot = self.last_viewport.rotation_deg;
        let s_vp = self.last_viewport.px_per_mm();
        let s_tile = self.last_tile_zoom * self.last_viewport.dpi.clamp(1.0, 4800.0) / MM_PER_INCH;
        let pan_px_x = self.last_viewport.pan_mm.x * s_vp;
        let pan_px_y = self.last_viewport.pan_mm.y * s_vp;

        // Fast exact 1:1 integer-aligned copy path (bit-for-bit identical to direct render)
        if rot.abs() <= 1e-6
            && (s_tile - s_vp).abs() <= 1e-9
            && (pan_px_x - pan_px_x.round()).abs() < 1e-4
            && (pan_px_y - pan_px_y.round()).abs() < 1e-4
        {
            let origin_x = pan_px_x.round() as i64;
            let origin_y = pan_px_y.round() as i64;
            let full_stride = (out_w as usize) * 4;
            for key in &self.active_keys {
                let Some(tile) = self.world_tiles.get(key) else {
                    continue;
                };
                if tile.buffer.is_empty() {
                    continue;
                }
                let dst_x0 = key.tile_x * i64::from(TILE_SIZE_PX) - origin_x;
                let dst_y0 = key.tile_y * i64::from(TILE_SIZE_PX) - origin_y;
                let x_start = dst_x0.max(0);
                let y_start = dst_y0.max(0);
                let x_end = (dst_x0 + i64::from(tile.buffer.width)).min(i64::from(out_w));
                let y_end = (dst_y0 + i64::from(tile.buffer.height)).min(i64::from(out_h));
                if x_end <= x_start || y_end <= y_start {
                    continue;
                }
                let copy_w = (x_end - x_start) as usize;
                let src_x0 = (x_start - dst_x0) as usize;
                let tile_stride = (tile.buffer.width as usize) * 4;
                for dy in y_start..y_end {
                    let src_y = (dy - dst_y0) as usize;
                    let src_off = src_y * tile_stride + src_x0 * 4;
                    let dst_off = (dy as usize) * full_stride + (x_start as usize) * 4;
                    out.rgba[dst_off..dst_off + copy_w * 4]
                        .copy_from_slice(&tile.buffer.rgba[src_off..src_off + copy_w * 4]);
                }
            }
            return out;
        }

        // General subpixel pan / LOD zoom / rotated viewport compositing path
        let view_mode = self
            .last_chrome_key
            .as_ref()
            .map_or(ViewMode::Enhanced, |k| k.view_mode);
        let proof_hash = self.last_chrome_key.as_ref().map_or(0, |k| k.proof_hash);
        let icc_hash = self.last_chrome_key.as_ref().map_or(0, |k| k.icc_hash);
        let lod_level = self.last_lod_level;
        let stride = (out_w as usize) * 4;

        if rot.abs() <= 1e-6 {
            const FP_SHIFT: u32 = 11;
            const FP_SCALE: u32 = 1 << FP_SHIFT; // 2048
            const FP_HALF: u32 = 1 << (FP_SHIFT * 2 - 1); // 1 << 21
            const TILE_PX_USIZE: usize = TILE_SIZE_PX as usize;
            const TILE_ROW_STRIDE: usize = TILE_PX_USIZE * 4;

            #[derive(Clone, Copy)]
            struct ColSample {
                o0: usize,
                o1: usize,
                fx_q: u32,
                inv_fx_q: u32,
            }

            #[derive(Clone, Copy)]
            struct ColSpan {
                tx_idx: usize,
                right_tx_idx: Option<usize>,
                px_start: usize,
                inner_end: usize,
                px_end: usize,
            }

            #[derive(Clone, Copy)]
            struct RowSample {
                ty_idx: usize,
                down_ty_idx: Option<usize>,
                y0: usize,
                y1: usize,
                fy_q: u32,
                inv_fy_q: u32,
                cross_down: bool,
            }

            let ratio = s_tile / s_vp.max(1e-9);
            let pan_tile_x = self.last_viewport.pan_mm.x * s_tile;
            let pan_tile_y = self.last_viewport.pan_mm.y * s_tile;
            let tile_sz = f64::from(TILE_SIZE_PX);
            let out_w_usize = out_w as usize;
            let out_h_usize = out_h as usize;

            let mut col_samples = Vec::with_capacity(out_w_usize);
            let mut col_runs: Vec<ColSpan> = Vec::new();
            let mut unique_txs: Vec<i64> = Vec::new();
            let mut cur_tx: Option<i64> = None;
            let mut cur_start = 0usize;
            let mut cur_inner_end = 0usize;

            let push_col_run = |runs: &mut Vec<ColSpan>,
                                txs: &mut Vec<i64>,
                                tx: i64,
                                start: usize,
                                inner_end: usize,
                                end: usize| {
                let tx_idx = txs.iter().position(|&t| t == tx).unwrap_or_else(|| {
                    txs.push(tx);
                    txs.len() - 1
                });
                let right_tx_idx = if inner_end < end {
                    Some(txs.iter().position(|&t| t == tx + 1).unwrap_or_else(|| {
                        txs.push(tx + 1);
                        txs.len() - 1
                    }))
                } else {
                    None
                };
                runs.push(ColSpan {
                    tx_idx,
                    right_tx_idx,
                    px_start: start,
                    inner_end,
                    px_end: end,
                });
            };

            for px in 0..out_w_usize {
                let wx = (px as f64 + 0.5) * ratio + pan_tile_x - 0.5;
                let tx = (wx / tile_sz).floor() as i64;
                let lx = wx - (tx as f64) * tile_sz;
                let cross_right = lx > 255.0;
                let (x0, x1, fx) = if cross_right {
                    (255usize, 0usize, (lx - 255.0).clamp(0.0, 1.0))
                } else {
                    let cx = lx.max(0.0);
                    let ix0 = (cx.floor() as usize).min(255);
                    let ix1 = (ix0 + 1).min(255);
                    (ix0, ix1, (cx - ix0 as f64).clamp(0.0, 1.0))
                };
                let fx_q = (fx * f64::from(FP_SCALE)).round() as u32;
                col_samples.push(ColSample {
                    o0: x0 * 4,
                    o1: x1 * 4,
                    fx_q,
                    inv_fx_q: FP_SCALE - fx_q,
                });

                match cur_tx {
                    Some(prev_tx) if prev_tx == tx => {
                        if !cross_right {
                            cur_inner_end = px + 1;
                        }
                    }
                    Some(prev_tx) => {
                        push_col_run(
                            &mut col_runs,
                            &mut unique_txs,
                            prev_tx,
                            cur_start,
                            cur_inner_end,
                            px,
                        );
                        cur_tx = Some(tx);
                        cur_start = px;
                        cur_inner_end = if cross_right { px } else { px + 1 };
                    }
                    None => {
                        cur_tx = Some(tx);
                        cur_start = px;
                        cur_inner_end = if cross_right { px } else { px + 1 };
                    }
                }
            }
            if let Some(prev_tx) = cur_tx {
                push_col_run(
                    &mut col_runs,
                    &mut unique_txs,
                    prev_tx,
                    cur_start,
                    cur_inner_end,
                    out_w_usize,
                );
            }

            let mut row_samples = Vec::with_capacity(out_h_usize);
            let mut unique_tys: Vec<i64> = Vec::new();
            for py in 0..out_h_usize {
                let wy = (py as f64 + 0.5) * ratio + pan_tile_y - 0.5;
                let ty = (wy / tile_sz).floor() as i64;
                let ly = wy - (ty as f64) * tile_sz;
                let ty_idx = unique_tys.iter().position(|&t| t == ty).unwrap_or_else(|| {
                    unique_tys.push(ty);
                    unique_tys.len() - 1
                });
                let cross_down = ly > 255.0;
                let (y0, y1, fy, down_ty_idx) = if cross_down {
                    let down_idx =
                        unique_tys
                            .iter()
                            .position(|&t| t == ty + 1)
                            .unwrap_or_else(|| {
                                unique_tys.push(ty + 1);
                                unique_tys.len() - 1
                            });
                    (
                        255usize,
                        0usize,
                        (ly - 255.0).clamp(0.0, 1.0),
                        Some(down_idx),
                    )
                } else {
                    let cy = ly.max(0.0);
                    let iy0 = (cy.floor() as usize).min(255);
                    let iy1 = (iy0 + 1).min(255);
                    (iy0, iy1, (cy - iy0 as f64).clamp(0.0, 1.0), None)
                };
                let fy_q = (fy * f64::from(FP_SCALE)).round() as u32;
                row_samples.push(RowSample {
                    ty_idx,
                    down_ty_idx,
                    y0,
                    y1,
                    fy_q,
                    inv_fy_q: FP_SCALE - fy_q,
                    cross_down,
                });
            }

            let num_tx = unique_txs.len();
            let mut tile_grid: Vec<Option<&RgbaBuffer>> =
                Vec::with_capacity(unique_tys.len() * num_tx);
            for &ty in &unique_tys {
                for &tx in &unique_txs {
                    let key = WorldTileKey {
                        lod_level,
                        tile_x: tx,
                        tile_y: ty,
                        view_mode,
                        proof_hash,
                        icc_hash,
                    };
                    tile_grid.push(self.world_tiles.get(&key).map(|t| &t.buffer));
                }
            }

            #[inline(always)]
            fn blend_px(pxs: [&[u8]; 4], weights: [u32; 4], dst: &mut [u8; 4]) {
                let [p00, p10, p01, p11] = pxs;
                let [w00, w10, w01, w11] = weights;
                for c in 0..4 {
                    let sum = u32::from(p00[c]) * w00
                        + u32::from(p10[c]) * w10
                        + u32::from(p01[c]) * w01
                        + u32::from(p11[c]) * w11;
                    dst[c] = ((sum + FP_HALF) >> (FP_SHIFT * 2)) as u8;
                }
            }

            out.rgba
                .par_chunks_mut(stride)
                .enumerate()
                .for_each(|(py, row_bytes)| {
                    let rs = row_samples[py];
                    let row_base = rs.ty_idx * num_tx;

                    for span in &col_runs {
                        let Some(buf) = tile_grid[row_base + span.tx_idx] else {
                            continue;
                        };
                        if buf.width != TILE_SIZE_PX || buf.height != TILE_SIZE_PX {
                            let tx = unique_txs[span.tx_idx];
                            let ty = unique_tys[rs.ty_idx];
                            let wy = (py as f64 + 0.5) * ratio + pan_tile_y - 0.5;
                            let ly = wy - (ty as f64) * tile_sz;
                            for (px, dst) in row_bytes[span.px_start * 4..span.px_end * 4]
                                .as_chunks_mut::<4>()
                                .0
                                .iter_mut()
                                .enumerate()
                            {
                                let wx =
                                    ((span.px_start + px) as f64 + 0.5) * ratio + pan_tile_x - 0.5;
                                *dst = buf.sample_bilinear(wx - (tx as f64) * tile_sz, ly);
                            }
                            continue;
                        }

                        let row0 =
                            &buf.rgba[rs.y0 * TILE_ROW_STRIDE..(rs.y0 + 1) * TILE_ROW_STRIDE];
                        let (row1, fy_q, inv_fy_q, down_buf_opt): (
                            &[u8],
                            u32,
                            u32,
                            Option<&RgbaBuffer>,
                        ) = if rs.cross_down {
                            let d_buf = rs
                                .down_ty_idx
                                .and_then(|dy_idx| tile_grid[dy_idx * num_tx + span.tx_idx])
                                .filter(|b| b.width == TILE_SIZE_PX && b.height == TILE_SIZE_PX);
                            if let Some(db) = d_buf {
                                (&db.rgba[0..TILE_ROW_STRIDE], rs.fy_q, rs.inv_fy_q, Some(db))
                            } else {
                                (row0, 0, FP_SCALE, None)
                            }
                        } else {
                            (
                                &buf.rgba[rs.y1 * TILE_ROW_STRIDE..(rs.y1 + 1) * TILE_ROW_STRIDE],
                                rs.fy_q,
                                rs.inv_fy_q,
                                None,
                            )
                        };

                        // Fast branchless interior span (x0 <= 255, x1 <= 255 both inside `buf`)
                        for (cs, dst) in col_samples[span.px_start..span.inner_end].iter().zip(
                            row_bytes[span.px_start * 4..span.inner_end * 4]
                                .as_chunks_mut::<4>()
                                .0
                                .iter_mut(),
                        ) {
                            let weights = [
                                cs.inv_fx_q * inv_fy_q,
                                cs.fx_q * inv_fy_q,
                                cs.inv_fx_q * fy_q,
                                cs.fx_q * fy_q,
                            ];
                            blend_px(
                                [
                                    &row0[cs.o0..cs.o0 + 4],
                                    &row0[cs.o1..cs.o1 + 4],
                                    &row1[cs.o0..cs.o0 + 4],
                                    &row1[cs.o1..cs.o1 + 4],
                                ],
                                weights,
                                dst,
                            );
                        }

                        // Trailing boundary pixel(s) where 255.0 < lx < 256.0
                        if span.inner_end < span.px_end {
                            let right_buf = span
                                .right_tx_idx
                                .and_then(|rx| tile_grid[row_base + rx])
                                .filter(|b| b.width == TILE_SIZE_PX && b.height == TILE_SIZE_PX);
                            let p00 = &row0[255 * 4..256 * 4];
                            let p01 = &row1[255 * 4..256 * 4];
                            let (p10, p11, eff_right) = if let Some(rb) = right_buf {
                                let r0 =
                                    &rb.rgba[rs.y0 * TILE_ROW_STRIDE..rs.y0 * TILE_ROW_STRIDE + 4];
                                let r1 = if rs.cross_down {
                                    let dr_buf = rs
                                        .down_ty_idx
                                        .zip(span.right_tx_idx)
                                        .and_then(|(dy_idx, rx)| tile_grid[dy_idx * num_tx + rx])
                                        .filter(|b| {
                                            b.width == TILE_SIZE_PX && b.height == TILE_SIZE_PX
                                        });
                                    if let Some(drb) = dr_buf {
                                        &drb.rgba[0..4]
                                    } else if down_buf_opt.is_some() {
                                        &rb.rgba
                                            [rs.y0 * TILE_ROW_STRIDE..rs.y0 * TILE_ROW_STRIDE + 4]
                                    } else {
                                        r0
                                    }
                                } else {
                                    &rb.rgba[rs.y1 * TILE_ROW_STRIDE..rs.y1 * TILE_ROW_STRIDE + 4]
                                };
                                (r0, r1, true)
                            } else {
                                (p00, p01, false)
                            };
                            for (cs, dst) in col_samples[span.inner_end..span.px_end].iter().zip(
                                row_bytes[span.inner_end * 4..span.px_end * 4]
                                    .as_chunks_mut::<4>()
                                    .0
                                    .iter_mut(),
                            ) {
                                let (fx_q, inv_fx_q) = if eff_right {
                                    (cs.fx_q, cs.inv_fx_q)
                                } else {
                                    (0, FP_SCALE)
                                };
                                let weights = [
                                    inv_fx_q * inv_fy_q,
                                    fx_q * inv_fy_q,
                                    inv_fx_q * fy_q,
                                    fx_q * fy_q,
                                ];
                                blend_px([p00, p10, p01, p11], weights, dst);
                            }
                        }
                    }
                });
        } else {
            let screen_to_world = self.last_viewport.screen_to_world_transform();
            let tile_sz = f64::from(TILE_SIZE_PX);
            out.rgba
                .par_chunks_mut(stride)
                .enumerate()
                .for_each(|(py, row_bytes)| {
                    for px in 0..(out_w as usize) {
                        let w = screen_to_world
                            .transform_point(Point2D::new(px as f64 + 0.5, py as f64 + 0.5));
                        let wx = w.x * s_tile - 0.5;
                        let wy = w.y * s_tile - 0.5;
                        let tx = (wx / tile_sz).floor() as i64;
                        let ty = (wy / tile_sz).floor() as i64;
                        let key = WorldTileKey {
                            lod_level,
                            tile_x: tx,
                            tile_y: ty,
                            view_mode,
                            proof_hash,
                            icc_hash,
                        };
                        let rgba = if let Some(tile) = self.world_tiles.get(&key) {
                            tile.buffer.sample_bilinear(
                                wx - (tx as f64) * tile_sz,
                                wy - (ty as f64) * tile_sz,
                            )
                        } else {
                            [0, 0, 0, 0]
                        };
                        let off = px * 4;
                        row_bytes[off..off + 4].copy_from_slice(&rgba);
                    }
                });
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

    fn sync_active_grid(
        &mut self,
        viewport: &Viewport,
        view_mode: ViewMode,
        proof_hash: u64,
        icc_hash: u64,
        force_refresh: bool,
    ) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        let frame = self.frame_counter;
        self.width_px = viewport.width_px.max(1);
        self.height_px = viewport.height_px.max(1);
        self.last_viewport = *viewport;

        let (lod_level, tile_zoom) = compute_lod_and_zoom(viewport.zoom, viewport.lod_dragging);
        self.last_lod_level = lod_level;
        self.last_tile_zoom = tile_zoom;

        let s_tile = tile_zoom * viewport.dpi.clamp(1.0, 4800.0) / MM_PER_INCH;
        let tile_sz_px = f64::from(TILE_SIZE_PX);
        let vis = viewport.visible_world_rect();
        let px0 = vis.min_x * s_tile;
        let py0 = vis.min_y * s_tile;
        let px1 = vis.max_x * s_tile;
        let py1 = vis.max_y * s_tile;

        let tx0 = (px0 / tile_sz_px).floor() as i64;
        let ty0 = (py0 / tile_sz_px).floor() as i64;
        let mut tx1 = ((px1 - 1e-6) / tile_sz_px).floor() as i64;
        let mut ty1 = ((py1 - 1e-6) / tile_sz_px).floor() as i64;
        if tx1 < tx0 {
            tx1 = tx0;
        }
        if ty1 < ty0 {
            ty1 = ty0;
        }
        tx1 = tx1.min(tx0 + 255);
        ty1 = ty1.min(ty0 + 255);

        self.cols = (tx1 - tx0 + 1).max(1) as u32;
        self.rows = (ty1 - ty0 + 1).max(1) as u32;
        let prev_keys = std::mem::take(&mut self.active_keys);
        self.active_keys
            .reserve((self.cols as usize).saturating_mul(self.rows as usize));

        let mut any_reset = false;
        let mut any_dirty = false;
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let key = WorldTileKey {
                    lod_level,
                    tile_x: tx,
                    tile_y: ty,
                    view_mode,
                    proof_hash,
                    icc_hash,
                };
                let world_rect_mm = Rect2D::from_xywh(
                    (tx as f64) * tile_sz_px / s_tile,
                    (ty as f64) * tile_sz_px / s_tile,
                    tile_sz_px / s_tile,
                    tile_sz_px / s_tile,
                );
                let needs_reset = match self.world_tiles.get(&key) {
                    Some(existing) => (existing.tile_zoom - tile_zoom).abs() > 1e-9,
                    None => true,
                };
                if needs_reset {
                    any_reset = true;
                    any_dirty = true;
                    self.world_tiles.insert(
                        key,
                        CachedWorldTile {
                            tile_zoom,
                            world_rect_mm,
                            buffer: RgbaBuffer::new_transparent(TILE_SIZE_PX, TILE_SIZE_PX),
                            dirty: true,
                            last_used: frame,
                        },
                    );
                } else if let Some(entry) = self.world_tiles.get_mut(&key) {
                    entry.last_used = frame;
                    if entry.dirty {
                        any_dirty = true;
                    }
                }
                self.active_keys.push(key);
            }
        }

        if self.world_tiles.len() > MAX_CACHED_WORLD_TILES {
            let active_set: HashSet<WorldTileKey> = self.active_keys.iter().copied().collect();
            let mut candidates: Vec<(u64, WorldTileKey)> = self
                .world_tiles
                .iter()
                .filter(|(k, _)| !active_set.contains(k))
                .map(|(k, v)| (v.last_used, *k))
                .collect();
            candidates.sort_by_key(|(used, _)| *used);
            let remove_count = self
                .world_tiles
                .len()
                .saturating_sub(MAX_CACHED_WORLD_TILES)
                .min(candidates.len());
            for (_, k) in candidates.into_iter().take(remove_count) {
                self.world_tiles.remove(&k);
            }
        }

        if self.active_keys == prev_keys
            && !any_reset
            && self.active_tiles.len() == self.active_keys.len()
        {
            let s_vp = self.last_viewport.px_per_mm();
            let origin_x = (self.last_viewport.pan_mm.x * s_vp).round() as i32;
            let origin_y = (self.last_viewport.pan_mm.y * s_vp).round() as i32;
            for (tile, key) in self.active_tiles.iter_mut().zip(self.active_keys.iter()) {
                let x = (key.tile_x * i64::from(TILE_SIZE_PX)) as i32 - origin_x;
                let y = (key.tile_y * i64::from(TILE_SIZE_PX)) as i32 - origin_y;
                tile.rect_px.x = x;
                tile.rect_px.y = y;
                if let Some(entry) = self.world_tiles.get(key) {
                    tile.dirty = entry.dirty;
                }
            }
        } else if force_refresh || !any_dirty {
            self.refresh_active_tiles();
        }
    }

    fn refresh_active_tiles(&mut self) {
        self.active_tiles.clear();
        self.active_tiles.reserve(self.active_keys.len());
        let cols = self.cols.max(1) as usize;
        let s_vp = self.last_viewport.px_per_mm();
        let origin_x = (self.last_viewport.pan_mm.x * s_vp).round() as i32;
        let origin_y = (self.last_viewport.pan_mm.y * s_vp).round() as i32;
        for (idx, key) in self.active_keys.iter().enumerate() {
            let col = (idx % cols) as u32;
            let row = (idx / cols) as u32;
            if let Some(entry) = self.world_tiles.get(key) {
                let x = (key.tile_x * i64::from(TILE_SIZE_PX)) as i32 - origin_x;
                let y = (key.tile_y * i64::from(TILE_SIZE_PX)) as i32 - origin_y;
                self.active_tiles.push(CachedTile {
                    col,
                    row,
                    rect_px: PixelRect::new(x, y, entry.buffer.width, entry.buffer.height),
                    world_rect_mm: entry.world_rect_mm,
                    buffer: entry.buffer.clone(),
                    dirty: entry.dirty,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_page_to_rgba;
    use crate::TinySkiaBackend;
    use coree_core::{
        Affine2D, BezierPath, Color, CornerStyle, Fill, ObjectId, OutlineStyle, ParametricShape,
        Point2D, SceneObject, Subpath,
    };

    #[test]
    fn parallel_dirty_tile_cache_matches_sequential_pixel_output() {
        let mut doc = Document::new_a4("Parallel Tiles");
        for i in 0u8..8 {
            let mut r = SceneObject::new(
                ObjectId(0),
                format!("TileBox {i}"),
                coree_core::NodePayload::Shape(ParametricShape::Rectangle {
                    width: 45.0,
                    height: 35.0,
                    corner_radii: [3.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::cmyk(f32::from(i) * 0.1, 0.35, 0.0, 0.0)))
            .with_transform(Affine2D::translate(
                15.0 + f64::from(i % 4) * 48.0,
                20.0 + f64::from(i / 4) * 60.0,
            ));
            r.effects.clear();
            doc.add_object(r, None).unwrap();
        }

        let vp = Viewport::new(768, 512);
        let mut opts = RenderOptions::interactive();
        opts.show_page_shadow = false;
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let tiled = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);

        let mut direct_backend = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
        renderer.render_document(&doc, &vp, ViewMode::Enhanced, &opts, &mut direct_backend);
        let direct = direct_backend.into_rgba();

        assert_eq!(tiled.width, direct.width);
        assert_eq!(tiled.height, direct.height);
        assert_eq!(
            tiled.rgba, direct.rgba,
            "Rayon parallel dirty tiles must match single-pass pixel output bit-for-bit"
        );
        let _ = render_page_to_rgba;
    }

    #[test]
    fn off_tile_frozen_lenses_are_warmed_in_first_dirty_tile_cache_pass() {
        use coree_core::{CommandOp, LensConfig, LensKind, NodePayload, SymbolDef};

        let mut doc = Document::new(
            "OffTileWarm",
            300.0,
            200.0,
            coree_core::ColorMode::Rgb,
            96.0,
        );
        let lid = doc.pages()[0].layers[0].id;
        let blue = Color::rgba8(0, 0, 255, 255);
        let green = Color::rgba8(0, 255, 0, 255);
        let make_rect =
            |id: ObjectId, x: f64, y: f64, w: f64, h: f64, fill: &Color| -> SceneObject {
                SceneObject::new(
                    id,
                    "Rect",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: w,
                        height: h,
                        corner_radii: [0.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::Solid(fill.clone()))
                .with_outline(OutlineStyle::none())
                .with_transform(Affine2D::translate(x, y))
            };

        doc.add_object(
            make_rect(ObjectId(0), 100.0, 10.0, 90.0, 50.0, &blue),
            Some(lid),
        )
        .unwrap();
        doc.add_object(
            make_rect(ObjectId(0), 100.0, 80.0, 90.0, 50.0, &green),
            Some(lid),
        )
        .unwrap();

        let mut standalone_lens =
            make_rect(ObjectId(0), 105.0, 15.0, 25.0, 25.0, &Color::TRANSPARENT);
        standalone_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        });
        let standalone_id = doc.add_object(standalone_lens, Some(lid)).unwrap();

        let mut sym_lens = make_rect(ObjectId(0), 0.0, 0.0, 25.0, 25.0, &Color::TRANSPARENT);
        sym_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        });
        let sym = SymbolDef::new("OffTileSym", vec![sym_lens]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id))
                    .with_transform(Affine2D::translate(145.0, 15.0)),
                Some(lid),
            )
            .unwrap();

        let mut iso_lens = make_rect(ObjectId(0), 105.0, 15.0, 25.0, 25.0, &Color::TRANSPARENT);
        iso_lens.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: true,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        });
        let iso_lens_id = doc.add_object(iso_lens, Some(lid)).unwrap();
        let iso_grp_id = doc.group_objects(&[iso_lens_id], "IsoGrp").unwrap();
        coree_core::apply_command(
            &mut doc,
            &CommandOp::SetOpacity {
                ids: vec![iso_grp_id],
                opacity: 0.5,
            },
        )
        .unwrap();

        let vp = Viewport::for_export(768, 512, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        assert!(renderer.has_unwarmed_frozen_lens(&doc, false));
        let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert!(
            !renderer.has_unwarmed_frozen_lens(&doc, false),
            "All off-tile standalone, SymbolInstance, and isolated Group frozen lenses must be warmed after frame 1"
        );

        coree_core::apply_command(
            &mut doc,
            &CommandOp::TransformObjects {
                ids: vec![standalone_id, inst_id],
                affine: Affine2D::translate(0.0, 70.0),
            },
        )
        .unwrap();
        cache.mark_all_dirty();
        let moved = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        let px_per_mm: f64 = 96.0 / 25.4;
        let p_standalone = moved
            .get_pixel(
                (117.5 * px_per_mm).round() as u32,
                (97.5 * px_per_mm).round() as u32,
            )
            .unwrap();
        let p_inst = moved
            .get_pixel(
                (157.5 * px_per_mm).round() as u32,
                (97.5 * px_per_mm).round() as u32,
            )
            .unwrap();
        assert_eq!(p_standalone, [255, 255, 0, 255]);
        assert_eq!(p_inst, [255, 255, 0, 255]);
    }

    #[test]
    fn world_tile_pan_and_zoom_retain_cached_tiles_and_invalidate_by_bbox() {
        let mut doc = Document::new_a4("WorldTileTest");
        let rect = SceneObject::new(
            ObjectId(0),
            "Box",
            coree_core::NodePayload::Shape(ParametricShape::Rectangle {
                width: 40.0,
                height: 40.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgba8(200, 60, 40, 255)))
        .with_transform(Affine2D::translate(10.0, 10.0));
        let rect_id = doc.add_object(rect, None).unwrap();

        let mut vp = Viewport::new(512, 512);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();

        // Initial render populates world tiles at (0,0), (1,0), (0,1), (1,1)
        let n0 = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert_eq!(n0, 4);
        assert!(cache.cached_world_tile_count() >= 4);

        // Pan slightly (by 1 tile width in mm) -> 2 overlapping tiles are retained cleanly, only 2 new tiles render
        let tile_mm = f64::from(TILE_SIZE_PX) / vp.px_per_mm();
        vp.pan_mm.x += tile_mm;
        let n1 = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert_eq!(n1, 2, "Overlapping world tiles must NOT re-render on pan");
        assert!(cache.cached_world_tile_count() >= 6);

        // Pan back to origin -> 0 tiles re-render because cached WorldTileKeys were preserved!
        vp.pan_mm.x -= tile_mm;
        let n2 = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert_eq!(
            n2, 0,
            "Panning back to visited region must hit cached world tiles"
        );

        // Zoom to 2.0 then back to 1.0 -> 0 tiles re-render on return!
        vp.zoom = 2.0;
        let n3 = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert!(n3 > 0);
        vp.zoom = 1.0;
        let n4 = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert_eq!(
            n4, 0,
            "Returning to previous zoom LOD must retain cached world tiles"
        );

        // BBox invalidation dirties only tiles intersecting the rect (at 10..50mm -> tile (0,0) only at zoom 1)
        let bb = doc.object_bounds(rect_id).unwrap();
        cache.mark_dirty(bb);
        assert_eq!(cache.dirty_tile_count(), 1);
    }

    #[test]
    fn world_tile_mark_dirty_decoupled_from_spatial_index_over_10k_objects() {
        let mut doc = Document::new_a4("SpatialDecouple10k");
        let lid = doc.pages()[0].layers[0].id;
        for i in 0..10_000 {
            let x = f64::from((i % 100) * 2);
            let y = f64::from((i / 100) * 2);
            let mut r = SceneObject::new(
                ObjectId(0),
                format!("O{i}"),
                coree_core::NodePayload::Shape(ParametricShape::Rectangle {
                    width: 1.5,
                    height: 1.5,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::rgba8(100, 140, 200, 255)))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(x, y));
            r.effects.clear();
            doc.add_object(r, Some(lid)).unwrap();
        }

        let vp = Viewport::new(512, 512);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert!(!cache.spatial_dirty);

        // Calling mark_dirty repeatedly with an unchanged Document must NOT set spatial_dirty
        for k in 0..50 {
            cache.mark_dirty(Rect2D::from_xywh(f64::from(k), 10.0, 5.0, 5.0));
            assert!(
                !cache.spatial_dirty,
                "mark_dirty must NOT force SpatialIndex rebuild (#11)"
            );
        }
        let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert!(!cache.spatial_dirty);
    }

    #[test]
    fn world_tile_fractional_zoom_fdot6_diagonal_and_bleed_guides_match_direct_render() {
        let mut doc = Document::new_a4("FractionalZoom048");
        let diag = SceneObject::new(
            ObjectId(0),
            "ShallowDiagonal",
            coree_core::NodePayload::Path(BezierPath::from_subpath(Subpath::from_points(
                [Point2D::new(5.0, 120.0), Point2D::new(205.0, 126.0)],
                false,
            ))),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::solid(Color::rgba8(20, 30, 40, 255), 0.25));
        doc.add_object(diag, None).unwrap();

        let mut vp = Viewport::new(600, 400);
        vp.zoom = 0.48;
        let mut opts = RenderOptions::interactive();
        opts.show_page_shadow = false;
        opts.show_bleed_guides = true;
        opts.show_margin_guides = true;
        opts.show_guidelines = true;

        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let tiled = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);

        let mut direct_backend = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
        renderer.render_document(&doc, &vp, ViewMode::Enhanced, &opts, &mut direct_backend);
        let direct = direct_backend.into_rgba();

        let mut max_ch_diff = 0u8;
        let mut guide_diffs = 0usize;
        let mut sum_diff = 0u64;
        for y in 0..vp.height_px {
            for x in 0..vp.width_px {
                let tp = tiled.get_pixel(x, y).unwrap();
                let dp = direct.get_pixel(x, y).unwrap();
                if tp != dp {
                    let is_guide = (tp[0].abs_diff(tp[2]) > 20) || (dp[0].abs_diff(dp[2]) > 20);
                    if is_guide {
                        guide_diffs += 1;
                    }
                    for c in 0..4 {
                        let d = tp[c].abs_diff(dp[c]);
                        max_ch_diff = max_ch_diff.max(d);
                        sum_diff += u64::from(d);
                    }
                }
            }
        }
        let mean_diff = (sum_diff as f64) / f64::from(vp.width_px * vp.height_px * 4);
        assert_eq!(
            guide_diffs, 0,
            "Bleed/margin/guideline pixels must match single-pass render bit-for-bit at zoom=0.48 (#9)"
        );
        assert!(
            max_ch_diff <= 10 && mean_diff < 0.01,
            "Shallow diagonal 1px stroke at zoom=0.48 must not suffer ~0.4px FDot6 tile seam (max_ch_diff={max_ch_diff}, mean_diff={mean_diff})"
        );
    }

    #[test]
    fn world_tile_a4_eight_guidelines_remain_dashed_and_match_direct_render() {
        let mut doc = Document::new_a4("EightGuidelinesA4");
        for y in [20.0, 40.0, 60.0, 80.0, 100.0, 120.0, 140.0, 160.0] {
            doc.add_guide(GuideOrientation::Horizontal(y), false);
        }

        let vp = Viewport::new(600, 400);
        let mut opts = RenderOptions::interactive();
        opts.show_page_shadow = false;
        opts.show_grid = false;
        opts.show_bleed_guides = false;
        opts.show_margin_guides = false;
        opts.show_guidelines = true;

        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let tiled = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);

        let mut direct_backend =
            crate::backend::TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
        renderer.render_document(&doc, &vp, ViewMode::Enhanced, &opts, &mut direct_backend);
        let direct = direct_backend.into_rgba();

        assert_eq!(
            tiled.rgba, direct.rgba,
            "8-guideline A4 tiled render must match direct render bit-for-bit"
        );

        let white = [255_u8, 255, 255, 255];
        let painted_y377 = (50..500_u32)
            .filter(|&x| direct.get_pixel(x, 377).unwrap() != white)
            .count();
        assert!(
            painted_y377 > 200 && painted_y377 < 400,
            "8 A4 guidelines must remain 6:3 dashed (~300/450 painted px at y=377), got {painted_y377}/450"
        );
    }

    #[test]
    fn world_tile_composite_to_rgba_fixed_point_matches_sample_bilinear_parity() {
        let mut doc = Document::new_a4("BilinearParity");
        let rect = SceneObject::new(
            ObjectId(0),
            "GradBox",
            coree_core::NodePayload::Shape(ParametricShape::Rectangle {
                width: 140.0,
                height: 120.0,
                corner_radii: [6.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::LinearGradient {
            start: Point2D::new(0.0, 0.0),
            end: Point2D::new(140.0, 120.0),
            stops: vec![
                coree_core::GradientStop::new(0.0, Color::rgba8(240, 50, 30, 255)),
                coree_core::GradientStop::new(0.5, Color::rgba8(40, 200, 110, 220)),
                coree_core::GradientStop::new(1.0, Color::rgba8(30, 80, 230, 255)),
            ],
        })
        .with_outline(OutlineStyle::solid(Color::rgba8(15, 25, 45, 255), 0.75))
        .with_transform(Affine2D::translate(30.0, 40.0));
        doc.add_object(rect, None).unwrap();

        let mut vp = Viewport::new(640, 480);
        vp.fit_rect(Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0), 16.0);
        vp.pan_mm.x += 1.37;
        vp.pan_mm.y -= 0.83;

        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let _ = cache.render_dirty_tiles(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        let fast = cache.composite_to_rgba();

        // Reference per-pixel sample_bilinear stitch
        let s_vp = vp.px_per_mm();
        let s_tile = cache.last_tile_zoom * vp.dpi.clamp(1.0, 4800.0) / MM_PER_INCH;
        let ratio = s_tile / s_vp.max(1e-9);
        let pan_tile_x = vp.pan_mm.x * s_tile;
        let pan_tile_y = vp.pan_mm.y * s_tile;
        let tile_sz = f64::from(TILE_SIZE_PX);
        let view_mode = ViewMode::Enhanced;
        let proof_hash = cache.last_chrome_key.as_ref().map_or(0, |k| k.proof_hash);
        let icc_hash = cache.last_chrome_key.as_ref().map_or(0, |k| k.icc_hash);
        let lod_level = cache.last_lod_level;

        let mut max_interior_diff = 0u8;
        for py in 0..vp.height_px {
            let wy = (py as f64 + 0.5) * ratio + pan_tile_y - 0.5;
            let ty = (wy / tile_sz).floor() as i64;
            let ly = wy - (ty as f64) * tile_sz;
            if ly > 255.0 {
                continue;
            }
            for px in 0..vp.width_px {
                let wx = (px as f64 + 0.5) * ratio + pan_tile_x - 0.5;
                let tx = (wx / tile_sz).floor() as i64;
                let lx = wx - (tx as f64) * tile_sz;
                if lx > 255.0 {
                    continue;
                }
                let key = WorldTileKey {
                    lod_level,
                    tile_x: tx,
                    tile_y: ty,
                    view_mode,
                    proof_hash,
                    icc_hash,
                };
                let expected = cache
                    .world_tiles
                    .get(&key)
                    .map_or([0, 0, 0, 0], |t| t.buffer.sample_bilinear(lx, ly));
                let got = fast.get_pixel(px, py).unwrap();
                for c in 0..4 {
                    max_interior_diff = max_interior_diff.max(got[c].abs_diff(expected[c]));
                }
            }
        }
        assert!(
            max_interior_diff <= 1,
            "Fixed-point composite_to_rgba must match sample_bilinear within 1 LSB, got {max_interior_diff}"
        );
    }

    #[test]
    fn world_tile_multi_guideline_dashing_a4_a0_5m_angled_and_pan_parity() {
        let assert_scenario = |label: &str,
                               w_mm: f64,
                               h_mm: f64,
                               num_guides: usize,
                               zoom: f64,
                               angled: bool,
                               pan_px: (i32, i32)| {
            let mut vp = Viewport::new(600, 400);
            vp.zoom = zoom;
            let s_vp = vp.px_per_mm();
            vp.pan_mm = Point2D::new(f64::from(pan_px.0) / s_vp, f64::from(pan_px.1) / s_vp);
            let vis = vp.visible_world_rect();

            let mut doc = Document::new_a4(label);
            if let Some(p) = doc.page_mut(0) {
                p.width_mm = w_mm;
                p.height_mm = h_mm;
            }
            let vis_guides = (num_guides / 2).max(2);
            for i in 0..vis_guides {
                let frac = (i as f64 + 1.0) / (vis_guides as f64 + 1.0);
                let y = vis.min_y + frac * vis.height();
                doc.add_guide(GuideOrientation::Horizontal(y), false);
            }
            for i in vis_guides..num_guides {
                let frac =
                    ((i - vis_guides) as f64 + 1.0) / ((num_guides - vis_guides) as f64 + 1.0);
                let y = h_mm * frac;
                doc.add_guide(GuideOrientation::Horizontal(y), false);
            }
            if angled {
                let c = vis.center();
                doc.add_guide(
                    GuideOrientation::Angled {
                        origin: c,
                        angle_deg: 35.0,
                    },
                    false,
                );
                doc.add_guide(GuideOrientation::Vertical(c.x), false);
            }

            let mut opts = RenderOptions::interactive();
            opts.show_page_shadow = false;
            opts.show_grid = false;
            opts.show_bleed_guides = false;
            opts.show_margin_guides = false;
            opts.show_guidelines = true;

            let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
            let mut renderer = SceneRenderer::new();
            let tiled = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);

            let mut direct_backend = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
            renderer.render_document(&doc, &vp, ViewMode::Enhanced, &opts, &mut direct_backend);
            let direct = direct_backend.into_rgba();

            assert_eq!(
                tiled.rgba, direct.rgba,
                "[{label}] integer-pan tiled render must match direct render bit-for-bit"
            );

            // Verify at least one visible horizontal guide row in viewport is dashed (mix of painted & background pixels)
            let bg = opts.canvas_bg.to_rgba8();
            let paper = opts.page_bg.to_rgba8();
            let mut found_dashed_row = false;
            for y in 0..vp.height_px {
                let mut painted = 0usize;
                let mut unpainted = 0usize;
                for x in 0..vp.width_px {
                    let p = direct.get_pixel(x, y).unwrap();
                    if p == bg || p == paper {
                        unpainted += 1;
                    } else {
                        painted += 1;
                    }
                }
                if painted > 20 && unpainted > 20 {
                    found_dashed_row = true;
                    break;
                }
            }
            assert!(
                found_dashed_row,
                "[{label}] visible guidelines must stay dashed rather than turning solid"
            );
        };

        // Cover A4 20g@16x, A0 20g@4x, 5m 20g@1x, 5m 10g@2x, angled + integer-pan parity
        assert_scenario("A4_20g_16x", 210.0, 297.0, 20, 16.0, false, (128, 64));
        assert_scenario("A0_20g_4x", 841.0, 1189.0, 20, 4.0, false, (64, 128));
        assert_scenario("5m_20g_1x", 5000.0, 2000.0, 20, 1.0, false, (192, 64));
        assert_scenario("5m_10g_2x", 5000.0, 2000.0, 10, 2.0, true, (256, 128));
    }

    #[test]
    fn world_tile_linked_next_frame_mark_object_dirty_invalidates_downstream_chain() {
        use coree_core::{CommandOp, NodePayload, ParagraphText, TextStyle, Vec2D};

        let mut doc = Document::new(
            "LinkedDirtyChain",
            320.0,
            320.0,
            coree_core::ColorMode::Rgb,
            96.0,
        );
        let style = TextStyle {
            font_size_pt: 14.0,
            fill: Color::rgba8(15, 25, 35, 255),
            ..TextStyle::default()
        };

        // Frame3 on-screen in tile (1, 0), Frame2 on-screen in tile (0, 0), Frame1 off-screen at (250, 250)
        let f3_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Frame3",
                    NodePayload::ParagraphText(ParagraphText::new(
                        "",
                        Vec2D::new(50.0, 50.0),
                        style.clone(),
                    )),
                )
                .with_transform(Affine2D::translate(75.0, 5.0)),
                None,
            )
            .unwrap();
        let mut p2 = ParagraphText::new("", Vec2D::new(50.0, 50.0), style.clone());
        p2.linked_next_frame = Some(f3_id);
        let f2_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Frame2", NodePayload::ParagraphText(p2))
                    .with_transform(Affine2D::translate(5.0, 5.0)),
                None,
            )
            .unwrap();
        let mut p1 = ParagraphText::new(
            "ALPHA BETA GAMMA DELTA EPSILON ZETA ETA THETA IOTA KAPPA LAMBDA MU NU XI OMICRON PI RHO SIGMA TAU",
            Vec2D::new(25.0, 12.0),
            style.clone(),
        );
        p1.linked_next_frame = Some(f2_id);
        let f1_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Frame1", NodePayload::ParagraphText(p1))
                    .with_transform(Affine2D::translate(250.0, 250.0)),
                None,
            )
            .unwrap();

        let vp = Viewport::for_export(512, 256, 96.0);
        let opts = RenderOptions::export();
        let mut cache = DirtyTileCache::new(vp.width_px, vp.height_px);
        let mut renderer = SceneRenderer::new();
        let _ = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        assert_eq!(cache.dirty_tile_count(), 0);

        let mut p1_short = ParagraphText::new("SHORT", Vec2D::new(25.0, 12.0), style);
        p1_short.linked_next_frame = Some(f2_id);
        coree_core::apply_command(
            &mut doc,
            &CommandOp::EditText {
                id: f1_id,
                payload: NodePayload::ParagraphText(p1_short),
            },
        )
        .unwrap();

        // Calling mark_object_dirty ONLY for Frame1 must transitively dirty Frame2 AND Frame3 tiles
        cache.mark_object_dirty(&doc, f1_id);
        assert!(
            cache.dirty_tile_count() >= 2,
            "mark_object_dirty(Frame1) must dirty both Frame2 and Frame3 tiles, got {}",
            cache.dirty_tile_count()
        );
        let tiled = cache.render_viewport(&doc, &vp, ViewMode::Enhanced, &opts, &mut renderer);
        let mut direct_backend = TinySkiaBackend::new(vp.width_px, vp.height_px).unwrap();
        renderer.render_document(&doc, &vp, ViewMode::Enhanced, &opts, &mut direct_backend);
        let direct = direct_backend.into_rgba();
        assert_eq!(
            tiled.rgba, direct.rgba,
            "Transitive linked_next_frame dirtying must match direct render with 0 stale diff pixels"
        );
    }
}
