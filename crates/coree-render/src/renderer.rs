// SPDX-License-Identifier: MIT OR Apache-2.0

//! Full scene graph renderer supporting single-page and Multipage View, Master
//! layers (`AllPages`/`OddPages`/`EvenPages`), all 11 [`Fill`] variants (including
//! Coons-patch [`Fill::MeshFill`], Conical/Square gradients, Patterns & Textures),
//! complete [`OutlineStyle`] (hairlines, dashes, arrowheads, behind-fill,
//! scale-with-object, calligraphy), `PowerClip`, `VectorTable`, `DimensionObject`,
//! `ConnectorObject`, `Group`, `SymbolInstance`, live Lenses, Effects, and Arabic RTL text.

use crate::backend::{
    BackendPaint, CanvasBackend, FillRule, PaintShader, PixelRect, RgbaBuffer, StrokeParams,
    TinySkiaBackend, MAX_SURFACE_DIM_PX, MAX_SURFACE_PIXELS,
};
use crate::effects_render::{
    apply_deform_point_steps, apply_geometry_effects, apply_geometry_effects_in_bounds,
    apply_transparency_mask, effect_bounds_padding_mm, find_transparency_effect,
    prepare_deform_point_steps, render_overlay_effects, render_underlay_effects,
    symmetry_effect_transforms, DeformPointStep,
};
use crate::hit_test::resolve_connector_endpoints_with_sym;
use crate::lens_render::{render_lens_object_with_backdrop, BackdropItem, FrozenLensSnapshot};
use crate::raster_engine::apply_raster_filters;
use crate::text_layout::{draw_text_pieces_icc, ShapedGlyphDraw, TextEngine};
use crate::viewport::{ViewMode, Viewport};
use coree_core::math::{sanitize_f32, sanitize_f64};
use coree_core::{
    Affine2D, ArrowheadStyle, ArtisticText, BezierPath, BlendMode, BuiltinIccProfile, Color,
    DimensionKind, DimensionObject, DisplayTransformLut3D, Document, Effect, Fill, GradientStop,
    GuideOrientation, IccColorPipeline, IccProfile, Layer, MasterLayerScope, MeshNode, NodePayload,
    ObjectId, OutlineStyle, Page, Perspective3x3, PerspectiveGridMode, Point2D,
    PostScriptFillPreset, Rect2D, RenderingIntent, SceneObject, SoftProofLut3D, SpatialIndex,
    SpatialObjectEntry, Subpath, SymbolDef, SymbolId, Vec2D, VectorTable,
};
use std::sync::Arc;

#[inline]
pub(crate) fn icc_srgb(color: &Color, icc: &IccColorPipeline) -> Color {
    let [r, g, b, a] = color.to_rgba_f32_icc(icc);
    Color::rgba(r, g, b, a)
}

#[inline]
fn icc_outline(outline: &OutlineStyle, icc: &IccColorPipeline) -> OutlineStyle {
    let mut out = outline.clone();
    out.color = icc_srgb(&outline.color, icc);
    out
}
use std::collections::{HashMap, HashSet};

/// Maximum scene recursion depth matching `coree-core`.
const MAX_RENDER_DEPTH: usize = 64;

pub(crate) type SymRenderCtx<'a> = (
    &'a HashMap<ObjectId, &'a SceneObject>,
    &'a [SceneObject],
    Affine2D,
);

#[derive(Debug, Clone, Copy)]
pub(crate) struct DeformFrame<'a> {
    pub(crate) pre_perspective: Option<Perspective3x3>,
    pub(crate) group_local_bb: Option<Rect2D>,
    pub(crate) group_effects: &'a [Effect],
    pub(crate) rel_to_outer: Affine2D,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MeshDeformCtx<'a> {
    pub(crate) raw_local_bb: Option<Rect2D>,
    pub(crate) pre_perspective: Option<Perspective3x3>,
    pub(crate) pre_effect_bb: Option<Rect2D>,
    pub(crate) effects: &'a [Effect],
    pub(crate) c_xf: Affine2D,
    pub(crate) deform_frames: &'a [DeformFrame<'a>],
}

#[inline]
pub(crate) fn apply_deform_chain_in_bounds(
    path: &BezierPath,
    c_xf: Affine2D,
    frames: &[DeformFrame<'_>],
) -> BezierPath {
    let mut cur = path.transformed(&c_xf);
    for frame in frames.iter().rev() {
        if let Some(persp) = frame.pre_perspective {
            cur = persp.transform_bezier_path(&cur);
        }
        cur = apply_geometry_effects_in_bounds(&cur, frame.group_local_bb, frame.group_effects);
        if frame.rel_to_outer != Affine2D::IDENTITY {
            cur = cur.transformed(&frame.rel_to_outer);
        }
    }
    cur
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum RenderSymParentRef {
    Id(ObjectId),
    RootIndex(usize),
}

fn sym_object_world_xf(
    id: ObjectId,
    sym_root_xf: &Affine2D,
    sym_map: &HashMap<ObjectId, &SceneObject>,
    sym_objects: &[SceneObject],
) -> Option<Affine2D> {
    let mut chain = Vec::new();
    let mut cur = RenderSymParentRef::Id(id);
    let mut seen = HashSet::new();
    while seen.insert(cur) {
        let obj = match cur {
            RenderSymParentRef::Id(oid) => sym_map.get(&oid).copied()?,
            RenderSymParentRef::RootIndex(idx) => sym_objects.get(idx)?,
        };
        chain.push(obj.transform);
        match cur {
            RenderSymParentRef::Id(oid) => {
                let parent = sym_objects.iter().enumerate().find_map(|(idx, po)| {
                    let contains = match &po.payload {
                        NodePayload::Group(c) => c.contains(&oid),
                        NodePayload::PowerClip { contents, .. } => contents.contains(&oid),
                        _ => false,
                    };
                    contains.then_some(if po.id.0 != 0 {
                        RenderSymParentRef::Id(po.id)
                    } else {
                        RenderSymParentRef::RootIndex(idx)
                    })
                });
                match parent {
                    Some(pid) => cur = pid,
                    None => break,
                }
            }
            RenderSymParentRef::RootIndex(_) => break,
        }
    }
    let mut xf = *sym_root_xf;
    for local in chain.into_iter().rev() {
        xf = xf.compose(&local);
    }
    xf.is_finite().then_some(xf)
}

pub(crate) fn resolve_sym_world_path(
    doc: &Document,
    path_id: ObjectId,
    sym_ctx: Option<SymRenderCtx<'_>>,
) -> Option<BezierPath> {
    if let Some((sym_map, sym_objects, sym_root_xf)) = sym_ctx {
        if let Some(sub) = sym_map.get(&path_id).copied() {
            let p_xf = sym_object_world_xf(path_id, &sym_root_xf, sym_map, sym_objects)?;
            let local = match &sub.payload {
                NodePayload::Path(p) => Some(p.clone()),
                NodePayload::Shape(s) => Some(s.to_bezier_path()),
                NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                NodePayload::ArtisticMedia { spine, style } => {
                    Some(coree_core::ArtisticMedia::expand_stroke(spine, style))
                }
                NodePayload::Clone { .. } => doc
                    .resolve_clone_payload_and_style_with_sym(sub, Some(sym_map))
                    .and_then(|(eff_p, _, _)| match eff_p {
                        NodePayload::Path(p) => Some(p),
                        NodePayload::Shape(s) => Some(s.to_bezier_path()),
                        NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                        NodePayload::ArtisticMedia { spine, style } => {
                            Some(coree_core::ArtisticMedia::expand_stroke(&spine, &style))
                        }
                        NodePayload::PowerClip { container, .. } => Some(*container),
                        _ => None,
                    }),
                NodePayload::PowerClip { container, .. } => Some((**container).clone()),
                _ => None,
            }?;
            let mut warped = if let Some(persp) = sub.perspective {
                persp.transform_bezier_path(&local)
            } else {
                local
            };
            for eff in &sub.effects {
                if let Effect::Perspective { quad } = eff {
                    if let Some(bb) = warped.bounding_box() {
                        if let Some(h) = Perspective3x3::from_rect_to_quad(bb, quad) {
                            warped = h.transform_bezier_path(&warped);
                        }
                    }
                }
            }
            return Some(warped.transformed(&p_xf));
        }
    }
    doc.world_path_of(path_id)
}

/// Configurable chrome, guides, color proofing, and surface styling for [`SceneRenderer`].
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub show_page_shadow: bool,
    pub show_bleed_guides: bool,
    pub show_margin_guides: bool,
    pub margin_mm: f64,
    pub show_grid: bool,
    pub show_guidelines: bool,
    pub show_perspective_grid: bool,
    pub canvas_bg: Color,
    pub page_bg: Color,
    pub wireframe_stroke_color: Color,
    pub multipage_gap_mm: f64,
    /// When `true`, soft-proofs rendered artwork through `proof_profile` CMYK press gamut.
    pub soft_proof_cmyk: bool,
    /// When `true`, overlays high-visibility cyan-green warning tint on out-of-CMYK-gamut pixels.
    pub gamut_warning: bool,
    /// Built-in ICC CMYK press profile used for soft-proofing & gamut warnings.
    pub proof_profile: BuiltinIccProfile,
    /// ICC rendering intent used when `soft_proof_cmyk` is enabled.
    pub proof_intent: RenderingIntent,
    /// Optional OS monitor/display RGB ICC profile bytes (`DisplayTransformLut3D`).
    pub display_icc_profile: Option<Vec<u8>>,
    /// ICC rendering intent used when applying `display_icc_profile`.
    pub display_icc_intent: RenderingIntent,
    /// When `true`, suppresses Spot `CutContour` strokes during rasterization (used when vector
    /// `/CutContour` overlays are emitted separately in PDF/EPS export).
    pub suppress_spot_cutcontour: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::interactive()
    }
}

impl RenderOptions {
    /// Interactive studio viewport options (pasteboard background, page shadow, guides, grid).
    #[must_use]
    pub fn interactive() -> Self {
        Self {
            show_page_shadow: true,
            show_bleed_guides: true,
            show_margin_guides: false,
            margin_mm: 10.0,
            show_grid: true,
            show_guidelines: true,
            show_perspective_grid: true,
            canvas_bg: Color::rgba8(224, 226, 230, 255),
            page_bg: Color::WHITE,
            wireframe_stroke_color: Color::BLACK,
            multipage_gap_mm: 20.0,
            soft_proof_cmyk: false,
            gamut_warning: false,
            proof_profile: BuiltinIccProfile::IsoCoatedV2Cmyk,
            proof_intent: RenderingIntent::RelativeColorimetric,
            display_icc_profile: None,
            display_icc_intent: RenderingIntent::RelativeColorimetric,
            suppress_spot_cutcontour: false,
        }
    }

    /// Clean headless export options (no pasteboard shadow, guides, grid, or UI chrome).
    #[must_use]
    pub fn export() -> Self {
        Self {
            show_page_shadow: false,
            show_bleed_guides: false,
            show_margin_guides: false,
            margin_mm: 0.0,
            show_grid: false,
            show_guidelines: false,
            show_perspective_grid: false,
            canvas_bg: Color::WHITE,
            page_bg: Color::WHITE,
            wireframe_stroke_color: Color::BLACK,
            multipage_gap_mm: 20.0,
            soft_proof_cmyk: false,
            gamut_warning: false,
            proof_profile: BuiltinIccProfile::IsoCoatedV2Cmyk,
            proof_intent: RenderingIntent::RelativeColorimetric,
            display_icc_profile: None,
            display_icc_intent: RenderingIntent::RelativeColorimetric,
            suppress_spot_cutcontour: false,
        }
    }
}

#[inline]
pub(crate) fn is_cutcontour_spot(color: &Color) -> bool {
    matches!(
        color,
        Color::Spot { name, .. } if name.trim().eq_ignore_ascii_case("CutContour")
    )
}

/// Positioned page slot in canvas world millimeters (for single-page or Multipage View).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageLayout {
    pub page_index: usize,
    pub origin_mm: Point2D,
    pub width_mm: f64,
    pub height_mm: f64,
}

impl PageLayout {
    /// World-space bounding rectangle of this page.
    #[must_use]
    pub fn rect_mm(&self) -> Rect2D {
        Rect2D::from_xywh(
            self.origin_mm.x,
            self.origin_mm.y,
            self.width_mm,
            self.height_mm,
        )
    }
}

/// Computes world-space page placements for single-page or side-by-side Multipage View.
#[must_use]
pub fn compute_page_layouts(doc: &Document, multipage_view: bool, gap_mm: f64) -> Vec<PageLayout> {
    let pages = doc.pages();
    if pages.is_empty() {
        return Vec::new();
    }
    if !multipage_view {
        let idx = doc.active_page_index().min(pages.len() - 1);
        let p = &pages[idx];
        return vec![PageLayout {
            page_index: idx,
            origin_mm: Point2D::ZERO,
            width_mm: p.width_mm,
            height_mm: p.height_mm,
        }];
    }

    let gap = sanitize_f64(gap_mm, 20.0).max(0.0);
    let max_h = pages.iter().map(|p| p.height_mm).fold(0.0_f64, f64::max);
    let mut cursor_x = 0.0_f64;
    let mut layouts = Vec::with_capacity(pages.len());
    for (idx, p) in pages.iter().enumerate() {
        let y = (max_h - p.height_mm) * 0.5;
        layouts.push(PageLayout {
            page_index: idx,
            origin_mm: Point2D::new(cursor_x, y),
            width_mm: p.width_mm,
            height_mm: p.height_mm,
        });
        cursor_x += p.width_mm + gap;
    }
    layouts
}

pub(crate) const FROZEN_TAG_SYM_INST: u64 = 0x5359_4d49;
pub(crate) const FROZEN_TAG_SYM_BRANCH: u64 = 0x5359_4d42;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FrozenLensKey {
    pub object_id: ObjectId,
    pub scope_hash: u64,
}

#[inline]
pub(crate) fn compute_frozen_scope_hash(stack: &[u64], sym_root_idx: Option<usize>) -> u64 {
    if stack.is_empty() && sym_root_idx.is_none() {
        return 0;
    }
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for &v in stack {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    if let Some(idx) = sym_root_idx {
        h ^= 0xa5;
        h = h.wrapping_mul(0x0100_0000_01b3);
        for b in (idx as u64).to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    if h == 0 {
        1
    } else {
        h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PageCullCacheKey {
    doc_id: [u8; 16],
    spatial_revision: u64,
    objects_len: usize,
    symbols_len: usize,
    pages_len: usize,
    page_index: usize,
    layers_sig_hash: u64,
}

#[derive(Debug, Clone)]
struct PageCullMeta {
    max_effect_pad: f64,
    page_has_lens: bool,
    /// Strictly ordered ascending by `z_order` matching `Document::build_spatial_index`.
    always_include: Vec<(usize, ObjectId)>,
    always_include_set: HashSet<ObjectId>,
}

fn compute_layers_sig_hash(layers: &[&Layer]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    let mix = |h: &mut u64, v: u64| {
        *h ^= v;
        *h = h.wrapping_mul(0x0100_0000_01b3);
    };
    mix(&mut h, layers.len() as u64);
    for layer in layers {
        mix(&mut h, layer.id.0);
        mix(&mut h, u64::from(layer.visible));
        mix(&mut h, u64::from(layer.is_desktop_layer));
        let scope_code = match layer.master_scope {
            MasterLayerScope::None => 0u64,
            MasterLayerScope::AllPages => 1,
            MasterLayerScope::OddPages => 2,
            MasterLayerScope::EvenPages => 3,
        };
        mix(&mut h, scope_code);
        mix(&mut h, layer.objects.len() as u64);
        if let Some(&first) = layer.objects.first() {
            mix(&mut h, first.0);
        }
        if let Some(&last) = layer.objects.last() {
            mix(&mut h, last.0);
        }
    }
    h
}

/// Stateful Pure-Rust 2D scene renderer caching typography, soft-proof LUTs, and frozen lens snapshots.
pub struct SceneRenderer {
    text_engine: TextEngine,
    frozen_lenses: HashMap<FrozenLensKey, FrozenLensSnapshot>,
    page_cull_cache: HashMap<PageCullCacheKey, Arc<PageCullMeta>>,
    sym_bounds_cache: HashMap<ObjectId, Option<Rect2D>>,
    soft_proof_cache: Option<(
        Option<u64>,
        BuiltinIccProfile,
        RenderingIntent,
        SoftProofLut3D,
    )>,
    display_lut_cache: Option<(u64, RenderingIntent, Option<DisplayTransformLut3D>)>,
    pub(crate) active_icc: IccColorPipeline,
    pub(crate) force_offscreen_lens_backdrop: bool,
    pub(crate) parent_xf_override: Option<(ObjectId, Affine2D)>,
    pub(crate) frozen_scope_stack: Vec<u64>,
    pub(crate) current_sym_root_idx: Option<usize>,
    active_symbols: HashSet<SymbolId>,
}

impl Default for SceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneRenderer {
    /// Creates a new scene renderer with initialized system font discovery.
    #[must_use]
    pub fn new() -> Self {
        Self {
            text_engine: TextEngine::new(),
            frozen_lenses: HashMap::new(),
            page_cull_cache: HashMap::new(),
            sym_bounds_cache: HashMap::new(),
            soft_proof_cache: None,
            display_lut_cache: None,
            active_icc: IccColorPipeline::default(),
            force_offscreen_lens_backdrop: false,
            parent_xf_override: None,
            frozen_scope_stack: Vec::new(),
            current_sym_root_idx: None,
            active_symbols: HashSet::new(),
        }
    }

    pub(crate) fn copy_frozen_lenses_from(&mut self, other: &Self) {
        self.frozen_lenses = other.frozen_lenses.clone();
        self.page_cull_cache = other.page_cull_cache.clone();
        self.active_icc = other.active_icc.clone();
        self.force_offscreen_lens_backdrop = other.force_offscreen_lens_backdrop;
        self.parent_xf_override = other.parent_xf_override;
    }

    pub(crate) fn warm_page_cull_cache(&mut self, doc: &Document, layouts: &[PageLayout]) {
        for layout in layouts {
            let layers = collect_effective_layers(doc, layout.page_index);
            let _ = self.get_or_build_page_cull_meta(doc, layout.page_index, &layers);
        }
    }

    fn get_or_build_page_cull_meta(
        &mut self,
        doc: &Document,
        page_index: usize,
        layers: &[&Layer],
    ) -> Arc<PageCullMeta> {
        let key = PageCullCacheKey {
            doc_id: *doc.id().as_bytes(),
            spatial_revision: doc.spatial_revision(),
            objects_len: doc.objects().len(),
            symbols_len: doc.symbols().len(),
            pages_len: doc.pages().len(),
            page_index,
            layers_sig_hash: compute_layers_sig_hash(layers),
        };
        if let Some(meta) = self.page_cull_cache.get(&key) {
            return Arc::clone(meta);
        }

        let mut max_effect_pad = 0.0_f64;
        let mut page_has_lens = false;
        let mut always_include = Vec::new();
        let mut always_include_set = HashSet::new();
        let mut visiting_objs = HashSet::new();
        let mut visiting_syms = HashSet::new();
        let mut z = 0usize;

        for layer in layers {
            if !layer.visible {
                continue;
            }
            let is_master = layer.is_desktop_layer || layer.master_scope != MasterLayerScope::None;
            for &oid in &layer.objects {
                if let Some(obj) = doc.object(oid) {
                    visiting_objs.clear();
                    visiting_objs.insert(oid);
                    visiting_syms.clear();
                    max_effect_pad = max_effect_pad.max(max_object_effect_padding_inner(
                        doc,
                        obj,
                        Affine2D::IDENTITY,
                        None,
                        &mut visiting_objs,
                        &mut visiting_syms,
                        0,
                    ));

                    visiting_objs.clear();
                    visiting_objs.insert(oid);
                    visiting_syms.clear();
                    let (nonlocal, has_lens) = object_nonlocal_and_lens_info(
                        doc,
                        obj,
                        &mut visiting_objs,
                        &mut visiting_syms,
                        0,
                    );
                    if has_lens {
                        page_has_lens = true;
                    }
                    if is_master || nonlocal {
                        always_include.push((z, oid));
                        always_include_set.insert(oid);
                    }
                }
                z += 1;
            }
        }

        let meta = Arc::new(PageCullMeta {
            max_effect_pad,
            page_has_lens,
            always_include,
            always_include_set,
        });
        if self.page_cull_cache.len() >= 64 {
            self.page_cull_cache.clear();
        }
        self.page_cull_cache.insert(key, Arc::clone(&meta));
        meta
    }

    pub(crate) fn get_or_build_soft_proof_lut(
        &mut self,
        custom_profile_bytes: Option<&[u8]>,
        fallback: BuiltinIccProfile,
        intent: RenderingIntent,
    ) -> &SoftProofLut3D {
        let profile_hash = custom_profile_bytes.map(fnv1a_64);
        let cache_hit = self
            .soft_proof_cache
            .as_ref()
            .is_some_and(|(h, f, i, _)| *h == profile_hash && *f == fallback && *i == intent);
        if !cache_hit {
            let parsed = custom_profile_bytes.and_then(|bytes| IccProfile::from_bytes(bytes).ok());
            let lut =
                SoftProofLut3D::build_for_profile_or_builtin(parsed.as_ref(), fallback, intent);
            self.soft_proof_cache = Some((profile_hash, fallback, intent, lut));
        }
        &self.soft_proof_cache.as_ref().expect("populated").3
    }

    pub(crate) fn get_or_build_display_lut(
        &mut self,
        display_profile_bytes: &[u8],
        intent: RenderingIntent,
    ) -> Option<&DisplayTransformLut3D> {
        let profile_hash = fnv1a_64(display_profile_bytes);
        let cache_hit = self
            .display_lut_cache
            .as_ref()
            .is_some_and(|(h, i, _)| *h == profile_hash && *i == intent);
        if !cache_hit {
            let lut = IccProfile::from_bytes(display_profile_bytes)
                .ok()
                .and_then(|prof| DisplayTransformLut3D::build_from_profile(&prof, intent));
            self.display_lut_cache = Some((profile_hash, intent, lut));
        }
        self.display_lut_cache.as_ref().and_then(|c| c.2.as_ref())
    }

    /// Mutable access to the internal [`TextEngine`].
    pub fn text_engine_mut(&mut self) -> &mut TextEngine {
        &mut self.text_engine
    }

    /// Returns a cached frozen lens snapshot if one exists for top-level `id`.
    #[must_use]
    pub fn get_frozen_lens(&self, id: ObjectId) -> Option<&FrozenLensSnapshot> {
        self.frozen_lenses.get(&FrozenLensKey {
            object_id: id,
            scope_hash: 0,
        })
    }

    /// Stores a frozen lens snapshot for top-level `id`.
    pub fn store_frozen_lens(&mut self, id: ObjectId, snap: FrozenLensSnapshot) {
        self.frozen_lenses.insert(
            FrozenLensKey {
                object_id: id,
                scope_hash: 0,
            },
            snap,
        );
    }

    /// Removes any cached frozen lens snapshots associated with `id`.
    pub fn remove_frozen_lens(&mut self, id: ObjectId) {
        self.frozen_lenses.retain(|k, _| k.object_id != id);
    }

    /// Clears all cached frozen lens snapshots.
    pub fn clear_frozen_lenses(&mut self) {
        self.frozen_lenses.clear();
    }

    #[inline]
    pub(crate) fn current_frozen_lens_key(&self, id: ObjectId) -> FrozenLensKey {
        FrozenLensKey {
            object_id: id,
            scope_hash: compute_frozen_scope_hash(
                &self.frozen_scope_stack,
                if id.0 == 0 {
                    self.current_sym_root_idx
                } else {
                    None
                },
            ),
        }
    }

    pub(crate) fn get_frozen_lens_key(&self, key: FrozenLensKey) -> Option<&FrozenLensSnapshot> {
        self.frozen_lenses.get(&key)
    }

    pub(crate) fn store_frozen_lens_key(&mut self, key: FrozenLensKey, snap: FrozenLensSnapshot) {
        self.frozen_lenses.insert(key, snap);
    }

    pub(crate) fn remove_frozen_lens_key(&mut self, key: FrozenLensKey) {
        self.frozen_lenses.remove(&key);
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_frozen_warm_check(
        &self,
        doc: &Document,
        obj: &SceneObject,
        sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
        sym_root_idx: Option<usize>,
        scope_stack: &mut Vec<u64>,
        visiting_objs: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        expand_own_sym: bool,
        already_entered_sym_inst: bool,
        depth: usize,
    ) -> bool {
        let obj_opacity = sanitize_f32(obj.opacity, 1.0).clamp(0.0, 1.0);
        if depth > MAX_RENDER_DEPTH || !obj.visible || obj_opacity <= 1e-6 {
            return false;
        }
        let resolved_clone;
        let obj = if matches!(obj.payload, NodePayload::Clone { .. }) {
            let Some((p, f, o)) = doc.resolve_clone_payload_and_style_with_sym(obj, sym_map) else {
                return false;
            };
            let mut tmp = obj.clone();
            tmp.payload = p;
            tmp.fill = f;
            tmp.outline = o;
            resolved_clone = tmp;
            &resolved_clone
        } else {
            obj
        };
        let sym_count = if expand_own_sym {
            symmetry_effect_transforms(&obj.effects).len()
        } else {
            1
        };
        for branch_idx in 0..sym_count {
            let prev_len = scope_stack.len();
            if expand_own_sym && branch_idx > 0 {
                scope_stack.extend_from_slice(&[
                    FROZEN_TAG_SYM_BRANCH,
                    obj.id.0,
                    branch_idx as u64,
                ]);
            }
            if obj
                .lens
                .as_ref()
                .is_some_and(|l| l.frozen && l.frozen_snapshot.is_none())
                && matches!(
                    obj.payload,
                    NodePayload::Path(_)
                        | NodePayload::Shape(_)
                        | NodePayload::BSpline(_)
                        | NodePayload::ArtisticMedia { .. }
                        | NodePayload::PowerClip { .. }
                )
            {
                let key = FrozenLensKey {
                    object_id: obj.id,
                    scope_hash: compute_frozen_scope_hash(
                        scope_stack,
                        if obj.id.0 == 0 { sym_root_idx } else { None },
                    ),
                };
                if !self.frozen_lenses.contains_key(&key) {
                    scope_stack.truncate(prev_len);
                    return true;
                }
            }
            let hit = match &obj.payload {
                NodePayload::Group(children) => children.iter().any(|&cid| {
                    if !visiting_objs.insert(cid) {
                        return false;
                    }
                    let cobj = sym_map
                        .and_then(|m| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    let found = cobj.is_some_and(|c| {
                        self.visit_frozen_warm_check(
                            doc,
                            c,
                            sym_map,
                            None,
                            scope_stack,
                            visiting_objs,
                            visiting_syms,
                            true,
                            false,
                            depth + 1,
                        )
                    });
                    visiting_objs.remove(&cid);
                    found
                }),
                NodePayload::PowerClip { contents, .. } => {
                    if obj.lens.is_some() {
                        false
                    } else {
                        contents.iter().any(|&cid| {
                            if !visiting_objs.insert(cid) {
                                return false;
                            }
                            let cobj = sym_map
                                .and_then(|m| m.get(&cid).copied())
                                .or_else(|| doc.object(cid));
                            let found = cobj.is_some_and(|c| {
                                self.visit_frozen_warm_check(
                                    doc,
                                    c,
                                    sym_map,
                                    None,
                                    scope_stack,
                                    visiting_objs,
                                    visiting_syms,
                                    true,
                                    false,
                                    depth + 1,
                                )
                            });
                            visiting_objs.remove(&cid);
                            found
                        })
                    }
                }
                NodePayload::SymbolInstance(sym_id) => {
                    if !visiting_syms.insert(*sym_id) {
                        false
                    } else {
                        let found = doc.symbol(*sym_id).is_some_and(|sym| {
                            let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                            let mut referenced: HashSet<ObjectId> = HashSet::new();
                            for sub in &sym.objects {
                                if sub.id.0 != 0 {
                                    child_map.insert(sub.id, sub);
                                }
                                match &sub.payload {
                                    NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                                    NodePayload::PowerClip { contents, .. } => {
                                        referenced.extend(contents.iter().copied());
                                    }
                                    _ => {}
                                }
                            }
                            let sym_prev_len = scope_stack.len();
                            if !already_entered_sym_inst {
                                let sym_u128 = sym_id.0.as_u128();
                                scope_stack.extend_from_slice(&[
                                    FROZEN_TAG_SYM_INST,
                                    sym_u128 as u64,
                                    (sym_u128 >> 64) as u64,
                                    obj.id.0,
                                    sym_root_idx.map_or(u64::MAX, |i| i as u64),
                                ]);
                            }
                            let saved_objs = std::mem::take(visiting_objs);
                            let inner_hit = sym.objects.iter().enumerate().any(|(idx, sub)| {
                                if sub.id.0 != 0 && referenced.contains(&sub.id) {
                                    return false;
                                }
                                if sub.id.0 != 0 && !visiting_objs.insert(sub.id) {
                                    return false;
                                }
                                let r_idx = if sub.id.0 == 0 { Some(idx) } else { None };
                                let res = self.visit_frozen_warm_check(
                                    doc,
                                    sub,
                                    Some(&child_map),
                                    r_idx,
                                    scope_stack,
                                    visiting_objs,
                                    visiting_syms,
                                    true,
                                    false,
                                    depth + 1,
                                );
                                if sub.id.0 != 0 {
                                    visiting_objs.remove(&sub.id);
                                }
                                res
                            });
                            *visiting_objs = saved_objs;
                            scope_stack.truncate(sym_prev_len);
                            inner_hit
                        });
                        visiting_syms.remove(sym_id);
                        found
                    }
                }
                _ => false,
            };
            scope_stack.truncate(prev_len);
            if hit {
                return true;
            }
        }
        false
    }

    fn subtree_has_unwarmed_frozen_lens_in_branch(
        &self,
        doc: &Document,
        obj: &SceneObject,
        sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
        already_entered_sym_inst: bool,
    ) -> bool {
        let mut scope_stack = self.frozen_scope_stack.clone();
        let mut visiting_objs = if obj.id.0 != 0 {
            HashSet::from([obj.id])
        } else {
            HashSet::new()
        };
        let mut visiting_syms = HashSet::new();
        self.visit_frozen_warm_check(
            doc,
            obj,
            sym_map,
            self.current_sym_root_idx,
            &mut scope_stack,
            &mut visiting_objs,
            &mut visiting_syms,
            false,
            already_entered_sym_inst,
            0,
        )
    }

    /// Returns `true` if any visible frozen lens in `doc` lacks a cached snapshot.
    #[must_use]
    pub fn has_unwarmed_frozen_lens(&self, doc: &Document, multipage: bool) -> bool {
        let layouts = compute_page_layouts(doc, multipage, 20.0);
        let mut scope_stack = Vec::new();
        let mut visiting_objs = HashSet::new();
        let mut visiting_syms = HashSet::new();
        for layout in layouts {
            for layer in collect_effective_layers(doc, layout.page_index) {
                if !layer.visible {
                    continue;
                }
                for &oid in &layer.objects {
                    let Some(obj) = doc.object(oid) else {
                        continue;
                    };
                    visiting_objs.clear();
                    visiting_objs.insert(oid);
                    visiting_syms.clear();
                    if self.visit_frozen_warm_check(
                        doc,
                        obj,
                        None,
                        None,
                        &mut scope_stack,
                        &mut visiting_objs,
                        &mut visiting_syms,
                        true,
                        false,
                        0,
                    ) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Renders `doc` into `backend` according to `viewport`, `view_mode`, and `options`.
    pub fn render_document(
        &mut self,
        doc: &Document,
        viewport: &Viewport,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
    ) {
        self.render_document_with_transform(
            doc,
            viewport,
            viewport.world_to_screen_transform(),
            view_mode,
            options,
            backend,
            None,
            None,
        );
    }

    /// Renders `doc` with an explicit `world_to_surface` transform and optional spatial culling
    /// (used internally by [`crate::tile_cache::DirtyTileCache`] for 256×256 tile rendering).
    #[allow(clippy::too_many_arguments)]
    pub fn render_document_with_transform(
        &mut self,
        doc: &Document,
        viewport: &Viewport,
        world_to_surface: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        cull_world_rect: Option<Rect2D>,
        page_spatial_indices: Option<&HashMap<usize, SpatialIndex>>,
    ) {
        self.active_icc = doc.icc_pipeline();
        backend.set_flatten_tolerance(viewport.lod_flatten_tolerance_mm());
        backend.clear(icc_srgb(&options.canvas_bg, &self.active_icc));

        let multipage = viewport.multipage_view || doc.multipage_view();
        let layouts = compute_page_layouts(doc, multipage, options.multipage_gap_mm);
        let px_per_mm = viewport.px_per_mm();

        for layout in &layouts {
            let Some(page) = doc.page(layout.page_index) else {
                continue;
            };
            let page_to_surface = world_to_surface
                .compose(&Affine2D::translate(layout.origin_mm.x, layout.origin_mm.y));

            // 1. Page drop shadow & paper surface
            render_page_surface(
                page,
                page_to_surface,
                px_per_mm,
                options,
                &self.active_icc,
                backend,
            );

            // 2. Collect ordered layers: applicable master layers + page's own layers
            let layers = collect_effective_layers(doc, layout.page_index);

            // Fast R-Tree frustum-culled path (cached PageCullMeta + zero 10k scan when no lenses)
            if let (Some(cull_world), Some(index)) = (
                cull_world_rect,
                page_spatial_indices.and_then(|m| m.get(&layout.page_index)),
            ) {
                let meta = self.get_or_build_page_cull_meta(doc, layout.page_index, &layers);
                let local_cull =
                    cull_world.translate(Vec2D::new(-layout.origin_mm.x, -layout.origin_mm.y));
                let expanded = local_cull.expand(meta.max_effect_pad + 4.0);

                if !meta.page_has_lens {
                    let hits: Vec<SpatialObjectEntry> =
                        index.query_rect_entries_back_to_front(expanded);
                    let mut hi = 0usize;
                    let mut ai = 0usize;
                    while hi < hits.len() && ai < meta.always_include.len() {
                        let hz = hits[hi].z_order;
                        let (az, aoid) = meta.always_include[ai];
                        let oid = if hz < az {
                            let id = hits[hi].id;
                            hi += 1;
                            id
                        } else if az < hz {
                            ai += 1;
                            aoid
                        } else {
                            hi += 1;
                            ai += 1;
                            aoid
                        };
                        self.render_object_by_id_with_backdrop(
                            doc,
                            Some(page),
                            oid,
                            page_to_surface,
                            page_to_surface,
                            view_mode,
                            options,
                            backend,
                            &[],
                            0,
                        );
                    }
                    while hi < hits.len() {
                        let oid = hits[hi].id;
                        hi += 1;
                        self.render_object_by_id_with_backdrop(
                            doc,
                            Some(page),
                            oid,
                            page_to_surface,
                            page_to_surface,
                            view_mode,
                            options,
                            backend,
                            &[],
                            0,
                        );
                    }
                    while ai < meta.always_include.len() {
                        let oid = meta.always_include[ai].1;
                        ai += 1;
                        self.render_object_by_id_with_backdrop(
                            doc,
                            Some(page),
                            oid,
                            page_to_surface,
                            page_to_surface,
                            view_mode,
                            options,
                            backend,
                            &[],
                            0,
                        );
                    }
                    continue;
                }

                let mut culled_set: HashSet<ObjectId> =
                    index.query_rect(expanded).into_iter().collect();
                culled_set.extend(meta.always_include_set.iter().copied());

                let mut rendered_so_far: Vec<BackdropItem> = Vec::new();
                for layer in &layers {
                    if !layer.visible {
                        continue;
                    }
                    for &oid in &layer.objects {
                        if culled_set.contains(&oid) {
                            self.render_object_by_id_with_backdrop(
                                doc,
                                Some(page),
                                oid,
                                page_to_surface,
                                page_to_surface,
                                view_mode,
                                options,
                                backend,
                                &rendered_so_far,
                                0,
                            );
                        }
                        rendered_so_far.push(BackdropItem::Object(oid));
                    }
                }
            } else {
                // 3. Full-page render in strict back-to-front z-order, tracking underlying objects for Lenses
                let mut rendered_so_far: Vec<BackdropItem> = Vec::new();
                for layer in &layers {
                    if !layer.visible {
                        continue;
                    }
                    for &oid in &layer.objects {
                        self.render_object_by_id_with_backdrop(
                            doc,
                            Some(page),
                            oid,
                            page_to_surface,
                            page_to_surface,
                            view_mode,
                            options,
                            backend,
                            &rendered_so_far,
                            0,
                        );
                        rendered_so_far.push(BackdropItem::Object(oid));
                    }
                }
            }
        }

        // 4. Optional live ICC CMYK Soft-Proofing, Out-of-Gamut Warning, and OS Display ICC pass
        // over rendered artwork (applied BEFORE page chrome overlays so guides/grids stay crisp).
        let needs_soft_proof =
            (options.soft_proof_cmyk || options.gamut_warning) && view_mode != ViewMode::Wireframe;
        let has_display_icc = options.display_icc_profile.is_some();
        if needs_soft_proof || has_display_icc {
            let mut buf = backend.snapshot_rgba();
            if !buf.is_empty() {
                if needs_soft_proof {
                    let lut = self.get_or_build_soft_proof_lut(
                        doc.cmyk_icc_profile().or(doc.custom_icc_profile()),
                        options.proof_profile,
                        options.proof_intent,
                    );
                    lut.apply_rgba(
                        &mut buf.rgba,
                        options.soft_proof_cmyk,
                        options.gamut_warning,
                    );
                }
                if let Some(ref disp_bytes) = options.display_icc_profile {
                    if let Some(disp_lut) =
                        self.get_or_build_display_lut(disp_bytes, options.display_icc_intent)
                    {
                        disp_lut.apply_rgba(&mut buf.rgba);
                    }
                }
                backend.clear(Color::TRANSPARENT);
                backend.composite_region(
                    PixelRect::new(0, 0, buf.width, buf.height),
                    &buf,
                    1.0,
                    BlendMode::Normal,
                );
            }
        }

        // 5. Page overlays: Grid, Bleed/Margin guides, Perspective Grid, Guidelines
        for layout in &layouts {
            let Some(page) = doc.page(layout.page_index) else {
                continue;
            };
            let page_to_surface = world_to_surface
                .compose(&Affine2D::translate(layout.origin_mm.x, layout.origin_mm.y));
            render_page_overlays(doc, page, page_to_surface, px_per_mm, options, backend);
        }

        backend.set_flatten_tolerance(None);
    }

    /// Renders a single scene object cleanly (used by `convert_object_to_bitmap`).
    pub fn render_single_object(
        &mut self,
        doc: &Document,
        obj_id: ObjectId,
        viewport: &Viewport,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
    ) {
        self.active_icc = doc.icc_pipeline();
        let xf = viewport.world_to_screen_transform();
        let parent_world = doc
            .parent_container_of(obj_id)
            .and_then(|pid| doc.world_transform_of(pid))
            .unwrap_or(Affine2D::IDENTITY);
        let parent_to_screen = xf.compose(&parent_world);
        let has_lens = doc.object(obj_id).is_some_and(|obj| {
            let mut visit = HashSet::from([obj_id]);
            crate::export_render::object_or_subtree_has_lens(doc, obj, 0, &mut visit)
        });
        if has_lens {
            let page_idx = crate::export_render::find_object_page_index(doc, obj_id);
            let page = doc.pages().get(page_idx);
            let mut underlying_items = Vec::new();
            let layers = collect_effective_layers(doc, page_idx);
            'layer_loop: for layer in layers {
                if !layer.visible {
                    continue;
                }
                for &oid in &layer.objects {
                    let mut sub_visit = HashSet::new();
                    if crate::export_render::collect_underlying_until(
                        doc,
                        oid,
                        obj_id,
                        &mut underlying_items,
                        0,
                        &mut sub_visit,
                    ) {
                        break 'layer_loop;
                    }
                }
            }
            let prev_force = self.force_offscreen_lens_backdrop;
            self.force_offscreen_lens_backdrop = true;
            self.render_object_by_id_with_backdrop(
                doc,
                page,
                obj_id,
                xf,
                parent_to_screen,
                view_mode,
                options,
                backend,
                &underlying_items,
                0,
            );
            self.force_offscreen_lens_backdrop = prev_force;
        } else {
            self.render_object_by_id_with_backdrop(
                doc,
                None,
                obj_id,
                xf,
                parent_to_screen,
                view_mode,
                options,
                backend,
                &[],
                0,
            );
        }
    }

    /// Recursive scene-graph object renderer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_object_by_id_with_backdrop(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        id: ObjectId,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        depth: usize,
    ) {
        self.render_object_by_id_with_sym(
            doc,
            page,
            id,
            page_to_surface,
            parent_to_screen,
            None,
            Affine2D::IDENTITY,
            view_mode,
            options,
            backend,
            underlying,
            depth,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_object_by_id_with_sym(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        id: ObjectId,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        ancestor_world_sym_xf: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        depth: usize,
    ) {
        if depth > MAX_RENDER_DEPTH {
            return;
        }
        let Some(obj) = sym_ctx
            .and_then(|(m, _, _)| m.get(&id).copied())
            .or_else(|| doc.object(id))
        else {
            return;
        };
        self.render_scene_object_with_sym(
            doc,
            page,
            obj,
            page_to_surface,
            parent_to_screen,
            sym_ctx,
            ancestor_world_sym_xf,
            view_mode,
            options,
            backend,
            underlying,
            depth,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_scene_object_with_backdrop(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        depth: usize,
    ) {
        self.render_scene_object_with_sym(
            doc,
            page,
            obj,
            page_to_surface,
            parent_to_screen,
            None,
            Affine2D::IDENTITY,
            view_mode,
            options,
            backend,
            underlying,
            depth,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_scene_object_with_sym(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        ancestor_world_sym_xf: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        depth: usize,
    ) {
        let obj_opacity = sanitize_f32(obj.opacity, 1.0).clamp(0.0, 1.0);
        if !obj.visible || obj_opacity <= 1e-6 || depth > MAX_RENDER_DEPTH {
            return;
        }
        let stripped_cutcontour_obj: Option<Box<SceneObject>> =
            if options.suppress_spot_cutcontour && is_cutcontour_spot(&obj.outline.color) {
                let mut cloned = obj.clone();
                cloned.outline = OutlineStyle::none();
                Some(Box::new(cloned))
            } else {
                None
            };
        let obj = stripped_cutcontour_obj.as_deref().unwrap_or(obj);
        if !parent_to_screen.is_finite() || !obj.transform.is_finite() {
            return;
        }
        let px_per_mm = estimate_scale(&parent_to_screen);
        let sym_transforms = symmetry_effect_transforms(&obj.effects);
        let parent_world = page_to_surface
            .inverse()
            .map(|inv| inv.compose(&parent_to_screen))
            .filter(|m| m.is_finite())
            .unwrap_or(Affine2D::IDENTITY);
        let inv_parent_world = parent_world.inverse().unwrap_or(Affine2D::IDENTITY);

        let resolved_clone = if matches!(obj.payload, NodePayload::Clone { .. }) {
            doc.resolve_clone_payload_and_style_with_sym(obj, sym_ctx.map(|(m, _, _)| m))
        } else {
            None
        };
        let eff_payload = resolved_clone.as_ref().map_or(&obj.payload, |(p, _, _)| p);
        let is_container = matches!(
            eff_payload,
            NodePayload::Group(_) | NodePayload::PowerClip { .. } | NodePayload::SymbolInstance(_)
        );
        for (sym_branch_idx, sym_xf) in sym_transforms.into_iter().enumerate() {
            let scope_prev_len = self.frozen_scope_stack.len();
            let own_world_sym_xf = if sym_xf == Affine2D::IDENTITY {
                Affine2D::IDENTITY
            } else {
                let w = parent_world.compose(&sym_xf).compose(&inv_parent_world);
                if w.is_finite() {
                    w
                } else {
                    sym_xf
                }
            };
            let mut branch_underlying = if sym_branch_idx > 0 {
                self.frozen_scope_stack.extend_from_slice(&[
                    FROZEN_TAG_SYM_BRANCH,
                    obj.id.0,
                    sym_branch_idx as u64,
                ]);
                is_container.then(|| {
                    let branch_world_xf = if matches!(eff_payload, NodePayload::SymbolInstance(_)) {
                        None
                    } else {
                        Some(own_world_sym_xf)
                    };
                    let mut v = underlying.to_vec();
                    v.push(BackdropItem::EnterSymmetryBranch {
                        obj_id: obj.id,
                        branch_idx: sym_branch_idx,
                        branch_world_xf,
                    });
                    v
                })
            } else {
                None
            };
            let effective_underlying = branch_underlying.as_deref().unwrap_or(underlying);
            let world_sym_xf = own_world_sym_xf.compose(&ancestor_world_sym_xf);
            let effective_parent = parent_to_screen.compose(&sym_xf);

            // Check if this object uses an Effect::Transparency alpha mask
            let trans_info = if view_mode != ViewMode::Wireframe {
                find_transparency_effect(&obj.effects)
            } else {
                None
            };
            if let Some((trans_mode, trans_blend)) = trans_info {
                self.render_with_transparency_effect(
                    doc,
                    page,
                    obj,
                    page_to_surface,
                    effective_parent,
                    sym_ctx,
                    world_sym_xf,
                    view_mode,
                    options,
                    backend,
                    effective_underlying,
                    depth,
                    trans_mode,
                    trans_blend,
                );
            } else {
                self.render_scene_object_core(
                    doc,
                    page,
                    obj,
                    page_to_surface,
                    effective_parent,
                    sym_ctx,
                    world_sym_xf,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    effective_underlying,
                    depth,
                );
            }
            if let Some(ref mut v) = branch_underlying {
                v.push(BackdropItem::ExitSymmetryBranch);
            }
            self.frozen_scope_stack.truncate(scope_prev_len);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compute_object_screen_bounds(
        doc: &Document,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        ancestor_world_sym: Affine2D,
        expand_own_sym: bool,
        visited_doc: &mut HashSet<ObjectId>,
        visited_syms: &mut HashSet<SymbolId>,
        depth: usize,
    ) -> Option<Rect2D> {
        if depth > MAX_RENDER_DEPTH
            || !obj.visible
            || !page_to_surface.is_finite()
            || !parent_to_screen.is_finite()
            || !obj.transform.is_finite()
        {
            return None;
        }
        if expand_own_sym
            && obj
                .effects
                .iter()
                .any(|e| matches!(e, Effect::Symmetry { .. }))
        {
            let parent_world = page_to_surface
                .inverse()
                .map(|inv| inv.compose(&parent_to_screen))
                .filter(|m| m.is_finite())
                .unwrap_or(Affine2D::IDENTITY);
            let inv_parent_world = parent_world.inverse().unwrap_or(Affine2D::IDENTITY);
            let mut acc: Option<Rect2D> = None;
            for sxf in symmetry_effect_transforms(&obj.effects) {
                let own_wsxf = if sxf == Affine2D::IDENTITY {
                    Affine2D::IDENTITY
                } else {
                    let w = parent_world.compose(&sxf).compose(&inv_parent_world);
                    if w.is_finite() {
                        w
                    } else {
                        sxf
                    }
                };
                let combined_wsxf = own_wsxf.compose(&ancestor_world_sym);
                if let Some(sb) = Self::compute_object_screen_bounds(
                    doc,
                    obj,
                    page_to_surface,
                    parent_to_screen.compose(&sxf),
                    sym_ctx,
                    combined_wsxf,
                    false,
                    visited_doc,
                    visited_syms,
                    depth,
                ) {
                    acc = Some(match acc {
                        Some(prev) => prev.union(sb),
                        None => sb,
                    });
                }
            }
            return acc;
        }
        let obj_to_screen = parent_to_screen.compose(&obj.transform);
        if !obj_to_screen.is_finite() {
            return None;
        }
        let active_world_sym = ancestor_world_sym;
        let obj_to_world = page_to_surface
            .inverse()
            .map(|inv| inv.compose(&obj_to_screen))
            .filter(|m| m.is_finite())
            .unwrap_or(obj.transform);

        let apply_persp = |p: &BezierPath| -> BezierPath {
            if let Some(persp) = obj.perspective {
                persp.transform_bezier_path(p)
            } else {
                p.clone()
            }
        };
        let mut base_sb: Option<Rect2D> = match &obj.payload {
            NodePayload::Path(p) => apply_geometry_effects(&apply_persp(p), &obj.effects)
                .transformed(&obj_to_screen)
                .bounding_box(),
            NodePayload::Shape(s) => {
                apply_geometry_effects(&apply_persp(&s.to_bezier_path()), &obj.effects)
                    .transformed(&obj_to_screen)
                    .bounding_box()
            }
            NodePayload::BSpline(bs) => {
                apply_geometry_effects(&apply_persp(&bs.to_bezier_path()), &obj.effects)
                    .transformed(&obj_to_screen)
                    .bounding_box()
            }
            NodePayload::ArtisticMedia { spine, style } => apply_geometry_effects(
                &apply_persp(&coree_core::ArtisticMedia::expand_stroke(spine, style)),
                &obj.effects,
            )
            .transformed(&obj_to_screen)
            .bounding_box(),
            NodePayload::Clone { .. } => doc
                .resolve_clone_payload_and_style_with_sym(obj, sym_ctx.map(|(m, _, _)| m))
                .and_then(|(p, f, o)| {
                    let mut tmp = obj.clone();
                    tmp.payload = p;
                    tmp.fill = f;
                    tmp.outline = o;
                    Self::compute_object_screen_bounds(
                        doc,
                        &tmp,
                        page_to_surface,
                        parent_to_screen,
                        sym_ctx,
                        ancestor_world_sym,
                        false,
                        visited_doc,
                        visited_syms,
                        depth + 1,
                    )
                }),
            NodePayload::PowerClip { container, .. } => {
                apply_geometry_effects(&apply_persp(container), &obj.effects)
                    .transformed(&obj_to_screen)
                    .bounding_box()
            }
            NodePayload::ArtisticText(t) => {
                let local_bb = if let Some(persp) = obj.perspective {
                    persp.try_transform_rect(t.estimated_bounds())?
                } else {
                    t.estimated_bounds()
                };
                let mut sb = obj_to_screen.try_transform_rect(local_bb)?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(local_bb.height()),
                    &mut sb,
                );
                Some(sb)
            }
            NodePayload::ParagraphText(t) => {
                let local_bb = if let Some(persp) = obj.perspective {
                    persp.try_transform_rect(t.bounding_box())?
                } else {
                    t.bounding_box()
                };
                let mut sb = obj_to_screen.try_transform_rect(local_bb)?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(local_bb.height()),
                    &mut sb,
                );
                Some(sb)
            }
            NodePayload::Raster(r) => {
                let full = Rect2D::from_xywh(
                    0.0,
                    0.0,
                    sanitize_f64(r.width_mm, 50.0).max(0.1),
                    sanitize_f64(r.height_mm, 50.0).max(0.1),
                );
                let rc = r
                    .crop_rect
                    .filter(|c| c.is_finite())
                    .and_then(|c| c.intersection(full))
                    .unwrap_or(full);
                let local_bb = if let Some(persp) = obj.perspective {
                    persp.try_transform_rect(rc)?
                } else {
                    rc
                };
                let mut sb = obj_to_screen.try_transform_rect(local_bb)?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(local_bb.height()),
                    &mut sb,
                );
                Some(sb)
            }
            NodePayload::Table(t) => {
                let r = t.to_bounding_box();
                let mut sb = obj_to_screen.try_transform_rect(r)?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(r.height()),
                    &mut sb,
                );
                Some(sb)
            }
            NodePayload::Dimension(d) => {
                let r = d.bounding_box();
                let mut sb = obj_to_screen.try_transform_rect(r)?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(r.height()),
                    &mut sb,
                );
                Some(sb)
            }
            NodePayload::TextOnPath(top) => {
                if let Some(host_path) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                    let parent_world = page_to_surface
                        .inverse()
                        .map(|inv| inv.compose(&parent_to_screen))
                        .filter(|m| m.is_finite())
                        .unwrap_or(Affine2D::IDENTITY);
                    let doc_parent_world = doc
                        .parent_container_of(obj.id)
                        .and_then(|pid| doc.world_transform_of(pid))
                        .filter(|m| m.is_finite())
                        .unwrap_or(Affine2D::IDENTITY);
                    let delta_xf = if sym_ctx.is_some() {
                        active_world_sym
                    } else {
                        parent_world
                            .compose(&doc_parent_world.inverse().unwrap_or(Affine2D::IDENTITY))
                    };
                    let text_surface_xf = page_to_surface.compose(&delta_xf);
                    let dist = sanitize_f64(top.distance_mm, 0.0).abs();
                    let max_font = top
                        .text
                        .runs
                        .iter()
                        .map(|r| r.style.font_size_mm())
                        .filter(|v| v.is_finite())
                        .fold(4.0_f64, f64::max);
                    let text_margin_mm = dist + max_font * 1.5;
                    let mut sb = host_path
                        .transformed(&text_surface_xf)
                        .bounding_box()
                        .map(|r| r.expand(text_margin_mm * estimate_scale(&text_surface_xf)))?;
                    let deform_xf = text_surface_xf.compose(&obj.transform);
                    Self::expand_screen_bounds_for_deforms(
                        &obj.effects,
                        &deform_xf,
                        host_path.bounding_box().map(|r| r.height()),
                        &mut sb,
                    );
                    Some(sb)
                } else {
                    let local_bb = if let Some(persp) = obj.perspective {
                        persp.try_transform_rect(top.text.estimated_bounds())?
                    } else {
                        top.text.estimated_bounds()
                    };
                    let mut sb = obj_to_screen.try_transform_rect(local_bb)?;
                    Self::expand_screen_bounds_for_deforms(
                        &obj.effects,
                        &obj_to_screen,
                        Some(local_bb.height()),
                        &mut sb,
                    );
                    Some(sb)
                }
            }
            NodePayload::Connector(conn) => {
                let base_world_xf = active_world_sym
                    .inverse()
                    .map(|inv| inv.compose(&obj_to_world))
                    .filter(|m| m.is_finite())
                    .unwrap_or(obj_to_world);
                let mut sym_visiting = HashMap::new();
                let mut sym_cache = HashMap::new();
                let ((a, b), _) = resolve_connector_endpoints_with_sym(
                    doc,
                    conn,
                    &base_world_xf,
                    sym_ctx.map(|(m, _, xf)| (m, xf)),
                    &mut sym_visiting,
                    visited_syms,
                    &mut sym_cache,
                    depth + 1,
                );
                let route = conn.route_path(a, b);
                if let Some(inv) = base_world_xf.inverse() {
                    apply_geometry_effects(&route.transformed(&inv), &obj.effects)
                        .transformed(&obj_to_screen)
                        .bounding_box()
                } else {
                    let conn_surface_xf = page_to_surface.compose(&active_world_sym);
                    route.transformed(&conn_surface_xf).bounding_box()
                }
            }
            NodePayload::Group(children) => {
                let raw_local_bb = if sym_ctx.is_none() && obj.id.0 != 0 {
                    doc.local_object_bounds(obj.id)
                } else {
                    let mut visit = HashSet::new();
                    Self::compute_children_local_bounds(
                        doc,
                        children,
                        sym_ctx,
                        Affine2D::IDENTITY,
                        obj_to_world,
                        depth + 1,
                        &mut visit,
                    )
                };
                let mut acc: Option<Rect2D> = if let Some(persp) = obj.perspective {
                    raw_local_bb
                        .and_then(|lb| persp.try_transform_rect(lb))
                        .and_then(|wb| obj_to_screen.try_transform_rect(wb))
                } else {
                    let mut acc: Option<Rect2D> = None;
                    for &cid in children {
                        let from_sym = sym_ctx.and_then(|(m, _, _)| m.get(&cid).copied());
                        let child_obj = if from_sym.is_some() {
                            from_sym
                        } else {
                            if !visited_doc.insert(cid) {
                                continue;
                            }
                            doc.object(cid)
                        };
                        if let Some(cobj) = child_obj {
                            if let Some(cb) = Self::compute_object_screen_bounds(
                                doc,
                                cobj,
                                page_to_surface,
                                obj_to_screen,
                                sym_ctx,
                                active_world_sym,
                                true,
                                visited_doc,
                                visited_syms,
                                depth + 1,
                            ) {
                                acc = Some(match acc {
                                    Some(prev) => prev.union(cb),
                                    None => cb,
                                });
                            }
                        }
                        if from_sym.is_none() {
                            visited_doc.remove(&cid);
                        }
                    }
                    acc
                };
                if let Some(ref mut sb) = acc {
                    let local_h = if let Some(persp) = obj.perspective {
                        raw_local_bb
                            .and_then(|lb| persp.try_transform_rect(lb))
                            .map(|b| b.height())
                    } else {
                        raw_local_bb.map(|b| b.height())
                    };
                    Self::expand_screen_bounds_for_deforms(
                        &obj.effects,
                        &obj_to_screen,
                        local_h,
                        sb,
                    );
                }
                acc
            }
            NodePayload::SymbolInstance(sym_id) => {
                if !visited_syms.insert(*sym_id) {
                    None
                } else {
                    let raw_local_bb = doc.symbol_bounds_with_xf_and_space(
                        *sym_id,
                        &Affine2D::IDENTITY,
                        Some(&obj_to_world),
                    );
                    let mut acc: Option<Rect2D> = if let Some(persp) = obj.perspective {
                        raw_local_bb
                            .and_then(|lb| persp.try_transform_rect(lb))
                            .and_then(|wb| obj_to_screen.try_transform_rect(wb))
                    } else {
                        doc.symbol(*sym_id).and_then(|sym| {
                            let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                            let mut referenced: HashSet<ObjectId> = HashSet::new();
                            for sub in &sym.objects {
                                if sub.id.0 != 0 {
                                    child_map.insert(sub.id, sub);
                                }
                                match &sub.payload {
                                    NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                                    NodePayload::PowerClip { contents, .. } => {
                                        referenced.extend(contents.iter().copied());
                                    }
                                    _ => {}
                                }
                            }
                            let mut acc: Option<Rect2D> = None;
                            for sub in &sym.objects {
                                if sub.id.0 != 0 && referenced.contains(&sub.id) {
                                    continue;
                                }
                                if let Some(sb) = Self::compute_object_screen_bounds(
                                    doc,
                                    sub,
                                    page_to_surface,
                                    obj_to_screen,
                                    Some((&child_map, &sym.objects, obj_to_world)),
                                    Affine2D::IDENTITY,
                                    true,
                                    visited_doc,
                                    visited_syms,
                                    depth + 1,
                                ) {
                                    acc = Some(match acc {
                                        Some(prev) => prev.union(sb),
                                        None => sb,
                                    });
                                }
                            }
                            acc
                        })
                    };
                    if let Some(ref mut sb) = acc {
                        let local_h = if let Some(persp) = obj.perspective {
                            raw_local_bb
                                .and_then(|lb| persp.try_transform_rect(lb))
                                .map(|b| b.height())
                        } else {
                            raw_local_bb.map(|b| b.height())
                        };
                        Self::expand_screen_bounds_for_deforms(
                            &obj.effects,
                            &obj_to_screen,
                            local_h,
                            sb,
                        );
                    }
                    visited_syms.remove(sym_id);
                    acc
                }
            }
        };

        let scale = estimate_scale(&obj_to_screen).max(estimate_scale(&page_to_surface));
        for eff in &obj.effects {
            if let Effect::Blend { target_id, .. } = eff {
                if let Some(tbb) = doc.world_object_bounds(*target_id) {
                    if let Some(tsb) = page_to_surface.try_transform_rect(tbb) {
                        base_sb = Some(match base_sb {
                            Some(prev) => prev.union(tsb),
                            None => tsb,
                        });
                    }
                }
            }
        }
        let stroke_pad_mm = if obj.outline.is_visible() {
            obj.outline.scaled_width_mm_for_transform(&obj_to_world)
        } else {
            0.0
        };
        let pad_px = (effect_bounds_padding_mm(&obj.effects) + stroke_pad_mm + 2.0) * scale;
        base_sb.map(|r| r.expand(pad_px.max(4.0)))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_with_transparency_effect(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        world_sym_xf: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        depth: usize,
        trans_mode: &coree_core::TransparencyMode,
        trans_blend: BlendMode,
    ) {
        let obj_to_screen = parent_to_screen.compose(&obj.transform);
        let Some(screen_to_local) = obj_to_screen.inverse() else {
            return;
        };
        let mut visited_doc = HashSet::new();
        let mut visited_syms = HashSet::new();
        let Some(sb) = Self::compute_object_screen_bounds(
            doc,
            obj,
            page_to_surface,
            parent_to_screen,
            sym_ctx,
            world_sym_xf,
            false,
            &mut visited_doc,
            &mut visited_syms,
            depth,
        ) else {
            return;
        };
        let Some(clamped) =
            PixelRect::from_f64_rect_clamped(sb, 0, backend.width(), backend.height())
        else {
            if view_mode != ViewMode::Wireframe
                && self.subtree_has_unwarmed_frozen_lens_in_branch(
                    doc,
                    obj,
                    sym_ctx.map(|(m, _, _)| m),
                    false,
                )
            {
                if let Ok(mut dummy) = TinySkiaBackend::new(1, 1) {
                    let px_per_mm = estimate_scale(&parent_to_screen);
                    let prev_force = self.force_offscreen_lens_backdrop;
                    self.force_offscreen_lens_backdrop = true;
                    self.render_scene_object_core(
                        doc,
                        page,
                        obj,
                        page_to_surface,
                        parent_to_screen,
                        sym_ctx,
                        world_sym_xf,
                        px_per_mm,
                        view_mode,
                        options,
                        &mut dummy,
                        underlying,
                        depth,
                    );
                    self.force_offscreen_lens_backdrop = prev_force;
                }
            }
            return;
        };
        let has_lens = {
            let mut v = HashSet::from([obj.id]);
            let mut vs = HashSet::new();
            crate::export_render::object_or_subtree_has_lens_with_sym(
                doc,
                obj,
                sym_ctx.map(|(m, _, _)| m),
                0,
                &mut v,
                &mut vs,
            )
        };
        if let Ok(mut sub) = TinySkiaBackend::new(clamped.width, clamped.height) {
            sub.clear(Color::TRANSPARENT);
            let shift = Affine2D::translate(-f64::from(clamped.x), -f64::from(clamped.y));
            let local_page = shift.compose(&page_to_surface);
            let local_parent = shift.compose(&parent_to_screen);
            let px_per_mm = estimate_scale(&parent_to_screen);
            let prev_force = self.force_offscreen_lens_backdrop;
            if has_lens {
                self.force_offscreen_lens_backdrop = true;
            }
            self.render_scene_object_core(
                doc,
                page,
                obj,
                local_page,
                local_parent,
                sym_ctx,
                world_sym_xf,
                px_per_mm,
                view_mode,
                options,
                &mut sub,
                underlying,
                depth,
            );
            self.force_offscreen_lens_backdrop = prev_force;

            let mut buf = sub.into_rgba();
            apply_transparency_mask(&mut buf, clamped, screen_to_local, trans_mode);
            backend.composite_region(clamped, &buf, 1.0, trans_blend);
            return;
        }
        // Downscale isolation buffer if `clamped` exceeds `MAX_SURFACE_DIM_PX` /
        // `MAX_SURFACE_PIXELS` so `Effect::Transparency` objects are never silently dropped.
        let dim_scale = (f64::from(MAX_SURFACE_DIM_PX)
            / f64::from(clamped.width.max(clamped.height).max(1)))
        .min(1.0);
        let px_total = (f64::from(clamped.width) * f64::from(clamped.height)).max(1.0);
        let px_scale = ((MAX_SURFACE_PIXELS as f64) / px_total).sqrt().min(1.0);
        let ds = (dim_scale.min(px_scale) * 0.999).clamp(1e-4, 1.0);
        let buf_w = ((f64::from(clamped.width) * ds).floor() as u32).clamp(1, MAX_SURFACE_DIM_PX);
        let buf_h = ((f64::from(clamped.height) * ds).floor() as u32).clamp(1, MAX_SURFACE_DIM_PX);
        if let Ok(mut sub) = TinySkiaBackend::new(buf_w, buf_h) {
            sub.clear(Color::TRANSPARENT);
            let sx = f64::from(buf_w) / f64::from(clamped.width);
            let sy = f64::from(buf_h) / f64::from(clamped.height);
            let shift = Affine2D::scale(sx, sy).compose(&Affine2D::translate(
                -f64::from(clamped.x),
                -f64::from(clamped.y),
            ));
            let local_page = shift.compose(&page_to_surface);
            let local_parent = shift.compose(&parent_to_screen);
            let px_per_mm = estimate_scale(&local_parent);
            let prev_force = self.force_offscreen_lens_backdrop;
            if has_lens {
                self.force_offscreen_lens_backdrop = true;
            }
            self.render_scene_object_core(
                doc,
                page,
                obj,
                local_page,
                local_parent,
                sym_ctx,
                world_sym_xf,
                px_per_mm,
                view_mode,
                options,
                &mut sub,
                underlying,
                depth,
            );
            self.force_offscreen_lens_backdrop = prev_force;
            let mut buf = sub.into_rgba();
            let buf_to_local = shift
                .compose(&obj_to_screen)
                .inverse()
                .unwrap_or(screen_to_local);
            let buf_rect = PixelRect::new(0, 0, buf_w, buf_h);
            apply_transparency_mask(&mut buf, buf_rect, buf_to_local, trans_mode);
            let draw_xf = Affine2D::translate(f64::from(clamped.x), f64::from(clamped.y))
                .compose(&Affine2D::scale(1.0 / sx, 1.0 / sy));
            backend.draw_pixmap(&buf, draw_xf, 1.0, trans_blend, true);
        }
    }

    fn extract_lens_local_path(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        sym_ctx: Option<SymRenderCtx<'_>>,
    ) -> Option<BezierPath> {
        match &obj.payload {
            NodePayload::Path(p) => Some(p.clone()),
            NodePayload::Shape(s) => Some(s.to_bezier_path()),
            NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
            NodePayload::ArtisticMedia { spine, style } => {
                Some(coree_core::ArtisticMedia::expand_stroke(spine, style))
            }
            NodePayload::PowerClip { container, .. } => Some((**container).clone()),
            NodePayload::ArtisticText(t) => {
                let mut combined = BezierPath::new();
                for piece in self.text_engine.layout_artistic_pieces(t) {
                    combined.subpaths.extend(piece.path.subpaths);
                }
                (!combined.subpaths.is_empty()).then_some(combined)
            }
            NodePayload::ParagraphText(t) => {
                let mut combined = BezierPath::new();
                for piece in self.text_engine.layout_paragraph_in_doc(doc, obj.id, t) {
                    combined.subpaths.extend(piece.path.subpaths);
                }
                (!combined.subpaths.is_empty()).then_some(combined)
            }
            NodePayload::TextOnPath(top) => {
                let mut combined = BezierPath::new();
                if let Some(host) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                    let obj_world = if let Some((sym_map, sym_objects, sym_root_xf)) = sym_ctx {
                        sym_object_world_xf(obj.id, &sym_root_xf, sym_map, sym_objects)
                            .unwrap_or_else(|| sym_root_xf.compose(&obj.transform))
                    } else {
                        doc.world_transform_of(obj.id).unwrap_or(obj.transform)
                    };
                    let inv = obj_world.inverse().unwrap_or(Affine2D::IDENTITY);
                    for piece in self.text_engine.layout_text_on_path_pieces(top, &host) {
                        combined
                            .subpaths
                            .extend(piece.path.transformed(&inv).subpaths);
                    }
                }
                (!combined.subpaths.is_empty()).then_some(combined)
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_scene_object_core(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        world_sym_xf: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        depth: usize,
    ) {
        if depth > MAX_RENDER_DEPTH {
            return;
        }
        let stripped_cutcontour_obj: Option<Box<SceneObject>> =
            if options.suppress_spot_cutcontour && is_cutcontour_spot(&obj.outline.color) {
                let mut cloned = obj.clone();
                cloned.outline = OutlineStyle::none();
                Some(Box::new(cloned))
            } else {
                None
            };
        let obj = stripped_cutcontour_obj.as_deref().unwrap_or(obj);
        let anti_alias = view_mode != ViewMode::Pixels;
        let obj_to_screen = parent_to_screen.compose(&obj.transform);

        // Handle Lens objects first (all 11 optical & color lenses)
        if view_mode != ViewMode::Wireframe {
            if let Some(lens_cfg) = obj.lens.clone() {
                if let Some(local_path) = self.extract_lens_local_path(doc, obj, sym_ctx) {
                    let local_persp = if let Some(persp) = obj.perspective {
                        persp.transform_bezier_path(&local_path)
                    } else {
                        local_path
                    };
                    let deformed = apply_geometry_effects(&local_persp, &obj.effects);
                    render_lens_object_with_backdrop(
                        self,
                        doc,
                        page,
                        obj,
                        &lens_cfg,
                        &deformed,
                        obj_to_screen,
                        page_to_surface,
                        view_mode,
                        options,
                        backend,
                        underlying,
                        depth,
                    );
                    if obj.outline.is_visible() {
                        draw_outline_styled(
                            &deformed,
                            obj.transform,
                            parent_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    return;
                }
            }
        }

        match &obj.payload {
            NodePayload::Path(path) => {
                self.render_vector_geometry(
                    doc,
                    obj,
                    path,
                    page_to_surface,
                    parent_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::Shape(shape) => {
                let path = shape.to_bezier_path();
                self.render_vector_geometry(
                    doc,
                    obj,
                    &path,
                    page_to_surface,
                    parent_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::BSpline(bs) => {
                let path = bs.to_bezier_path();
                self.render_vector_geometry(
                    doc,
                    obj,
                    &path,
                    page_to_surface,
                    parent_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::ArtisticMedia { spine, style } => {
                let path = coree_core::ArtisticMedia::expand_stroke(spine, style);
                self.render_vector_geometry(
                    doc,
                    obj,
                    &path,
                    page_to_surface,
                    parent_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::Clone { .. } => {
                if let Some((p, f, o)) =
                    doc.resolve_clone_payload_and_style_with_sym(obj, sym_ctx.map(|(m, _, _)| m))
                {
                    let mut resolved = obj.clone();
                    resolved.payload = p;
                    resolved.fill = f;
                    resolved.outline =
                        if options.suppress_spot_cutcontour && is_cutcontour_spot(&o.color) {
                            OutlineStyle::none()
                        } else {
                            o
                        };
                    self.render_scene_object_core(
                        doc,
                        page,
                        &resolved,
                        page_to_surface,
                        parent_to_screen,
                        sym_ctx,
                        world_sym_xf,
                        px_per_mm,
                        view_mode,
                        options,
                        backend,
                        underlying,
                        depth + 1,
                    );
                }
            }
            NodePayload::ArtisticText(text) => {
                if (obj.effects.is_empty() && obj.perspective.is_none())
                    || view_mode == ViewMode::Wireframe
                {
                    self.text_engine.render_artistic_text_icc(
                        backend,
                        text,
                        obj_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        view_mode,
                        px_per_mm,
                        &self.active_icc,
                    );
                } else {
                    let mut pieces = self.text_engine.layout_artistic_pieces(text);
                    if pieces.is_empty() && !text.plain_text().trim().is_empty() {
                        self.text_engine.render_artistic_text_icc(
                            backend,
                            text,
                            obj_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            view_mode,
                            px_per_mm,
                            &self.active_icc,
                        );
                    } else {
                        if let Some(persp) = obj.perspective {
                            for piece in &mut pieces {
                                piece.path = persp.transform_bezier_path(&piece.path);
                            }
                        }
                        if obj.effects.is_empty() {
                            draw_text_pieces_icc(
                                backend,
                                &pieces,
                                obj_to_screen,
                                &obj.outline,
                                obj.opacity,
                                obj.blend_mode,
                                view_mode,
                                px_per_mm,
                                &self.active_icc,
                            );
                        } else {
                            self.render_text_pieces_with_effects(
                                doc,
                                obj,
                                &pieces,
                                None,
                                obj.transform,
                                page_to_surface,
                                parent_to_screen,
                                px_per_mm,
                                view_mode,
                                backend,
                            );
                        }
                    }
                }
            }
            NodePayload::ParagraphText(para) => {
                if (obj.effects.is_empty() && obj.perspective.is_none())
                    || view_mode == ViewMode::Wireframe
                {
                    self.text_engine.render_paragraph_text_in_doc_icc(
                        backend,
                        doc,
                        obj.id,
                        para,
                        obj_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        view_mode,
                        px_per_mm,
                        &self.active_icc,
                    );
                } else {
                    let mut pieces = self.text_engine.layout_paragraph_in_doc(doc, obj.id, para);
                    if let Some(persp) = obj.perspective {
                        for piece in &mut pieces {
                            piece.path = persp.transform_bezier_path(&piece.path);
                        }
                    }
                    if obj.effects.is_empty() {
                        draw_text_pieces_icc(
                            backend,
                            &pieces,
                            obj_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            view_mode,
                            px_per_mm,
                            &self.active_icc,
                        );
                    } else {
                        self.render_text_pieces_with_effects(
                            doc,
                            obj,
                            &pieces,
                            if obj.perspective.is_some() {
                                None
                            } else {
                                Some(para.bounding_box())
                            },
                            obj.transform,
                            page_to_surface,
                            parent_to_screen,
                            px_per_mm,
                            view_mode,
                            backend,
                        );
                    }
                }
            }
            NodePayload::TextOnPath(top) => {
                let delta_xf = if sym_ctx.is_some() {
                    world_sym_xf
                } else {
                    let actual_parent_world = page_to_surface
                        .inverse()
                        .map(|inv| inv.compose(&parent_to_screen))
                        .filter(|m| m.is_finite())
                        .unwrap_or(Affine2D::IDENTITY);
                    let doc_parent_world = doc
                        .parent_container_of(obj.id)
                        .and_then(|pid| doc.world_transform_of(pid))
                        .filter(|m| m.is_finite())
                        .unwrap_or(Affine2D::IDENTITY);
                    actual_parent_world
                        .compose(&doc_parent_world.inverse().unwrap_or(Affine2D::IDENTITY))
                };
                if (obj.effects.is_empty() && obj.perspective.is_none())
                    || view_mode == ViewMode::Wireframe
                {
                    if let Some(host_path) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                        let text_surface_xf = page_to_surface.compose(&delta_xf);
                        self.text_engine.render_text_on_path_icc(
                            backend,
                            top,
                            &host_path,
                            text_surface_xf,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            view_mode,
                            px_per_mm,
                            &self.active_icc,
                        );
                    } else {
                        self.text_engine.render_artistic_text_icc(
                            backend,
                            &top.text,
                            obj_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            view_mode,
                            px_per_mm,
                            &self.active_icc,
                        );
                    }
                } else if let Some(host_path) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                    let mut pieces = self.text_engine.layout_text_on_path_pieces(top, &host_path);
                    if let Some(persp) = obj.perspective {
                        let text_surface_xf = page_to_surface.compose(&delta_xf);
                        let inv_parent = parent_to_screen
                            .inverse()
                            .map(|inv| inv.compose(&text_surface_xf))
                            .filter(|m| m.is_finite())
                            .unwrap_or(Affine2D::IDENTITY);
                        for piece in &mut pieces {
                            let local_piece = piece.path.transformed(&inv_parent);
                            piece.path = persp.transform_bezier_path(&local_piece);
                        }
                        if obj.effects.is_empty() {
                            draw_text_pieces_icc(
                                backend,
                                &pieces,
                                parent_to_screen,
                                &obj.outline,
                                obj.opacity,
                                obj.blend_mode,
                                view_mode,
                                px_per_mm,
                                &self.active_icc,
                            );
                        } else {
                            self.render_text_pieces_with_effects(
                                doc,
                                obj,
                                &pieces,
                                None,
                                Affine2D::IDENTITY,
                                page_to_surface,
                                parent_to_screen,
                                px_per_mm,
                                view_mode,
                                backend,
                            );
                        }
                    } else {
                        self.render_text_pieces_with_effects(
                            doc,
                            obj,
                            &pieces,
                            None,
                            delta_xf,
                            page_to_surface,
                            page_to_surface,
                            px_per_mm,
                            view_mode,
                            backend,
                        );
                    }
                } else {
                    let mut pieces = self.text_engine.layout_artistic_pieces(&top.text);
                    if let Some(persp) = obj.perspective {
                        for piece in &mut pieces {
                            piece.path = persp.transform_bezier_path(&piece.path);
                        }
                    }
                    if obj.effects.is_empty() {
                        draw_text_pieces_icc(
                            backend,
                            &pieces,
                            obj_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            view_mode,
                            px_per_mm,
                            &self.active_icc,
                        );
                    } else {
                        self.render_text_pieces_with_effects(
                            doc,
                            obj,
                            &pieces,
                            None,
                            obj.transform,
                            page_to_surface,
                            parent_to_screen,
                            px_per_mm,
                            view_mode,
                            backend,
                        );
                    }
                }
            }
            NodePayload::Raster(raster) => {
                render_raster_object(
                    doc,
                    obj,
                    raster,
                    page_to_surface,
                    parent_to_screen,
                    obj_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    &self.active_icc,
                    backend,
                );
            }
            NodePayload::Table(table) => {
                self.render_table_object(
                    doc,
                    obj,
                    table,
                    obj_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::Dimension(dim) => {
                self.render_dimension_object(
                    doc,
                    obj,
                    dim,
                    obj_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                );
            }
            NodePayload::Connector(conn) => {
                let page_world_xf = page_to_surface
                    .inverse()
                    .map(|inv| inv.compose(&parent_to_screen).compose(&obj.transform))
                    .filter(|m| m.is_finite())
                    .unwrap_or(obj.transform);
                let base_world_xf = world_sym_xf
                    .inverse()
                    .map(|inv| inv.compose(&page_world_xf))
                    .filter(|m| m.is_finite())
                    .unwrap_or(page_world_xf);
                let mut sym_visiting = HashMap::new();
                let mut sym_visited_syms = HashSet::new();
                let mut local_cache = HashMap::new();
                let cache_ref = if sym_ctx.is_some() {
                    &mut self.sym_bounds_cache
                } else {
                    &mut local_cache
                };
                let ((a, b), _) = resolve_connector_endpoints_with_sym(
                    doc,
                    conn,
                    &base_world_xf,
                    sym_ctx.map(|(m, _, xf)| (m, xf)),
                    &mut sym_visiting,
                    &mut sym_visited_syms,
                    cache_ref,
                    depth + 1,
                );
                let route = conn.route_path(a, b);
                let (draw_path, draw_local_xf, draw_parent_to_screen) =
                    if let Some(inv) = base_world_xf.inverse() {
                        (
                            apply_geometry_effects(&route.transformed(&inv), &obj.effects),
                            obj.transform,
                            parent_to_screen,
                        )
                    } else {
                        (
                            route,
                            Affine2D::IDENTITY,
                            page_to_surface.compose(&world_sym_xf),
                        )
                    };
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(
                        &draw_path,
                        draw_parent_to_screen.compose(&draw_local_xf),
                        px_per_mm,
                        options,
                        backend,
                    );
                } else {
                    if !obj.effects.is_empty() {
                        render_underlay_effects(
                            doc,
                            obj,
                            &draw_path,
                            draw_local_xf,
                            draw_parent_to_screen,
                            page_to_surface,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if obj.outline.is_visible() {
                        draw_outline_styled(
                            &draw_path,
                            draw_local_xf,
                            draw_parent_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if !obj.effects.is_empty() {
                        render_overlay_effects(
                            obj,
                            &draw_path,
                            draw_local_xf,
                            draw_parent_to_screen,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                }
            }
            NodePayload::Group(children) => {
                let sym_map = sym_ctx.map(|(m, _, _)| m);
                let track_child_lens = children.iter().any(|&cid| {
                    sym_map
                        .and_then(|m| m.get(&cid).copied())
                        .or_else(|| doc.object(cid))
                        .is_some_and(|c| {
                            let mut v = HashSet::from([cid]);
                            let mut vs = HashSet::new();
                            crate::export_render::object_or_subtree_has_lens_with_sym(
                                doc, c, sym_map, 0, &mut v, &mut vs,
                            )
                        })
                });
                let needs_isolation = view_mode != ViewMode::Wireframe
                    && (obj.opacity < 0.999 || obj.blend_mode != BlendMode::Normal);
                if needs_isolation {
                    let mut visited_doc = HashSet::new();
                    let mut visited_syms = HashSet::new();
                    if let Some(sb) = Self::compute_object_screen_bounds(
                        doc,
                        obj,
                        page_to_surface,
                        parent_to_screen,
                        sym_ctx,
                        world_sym_xf,
                        false,
                        &mut visited_doc,
                        &mut visited_syms,
                        depth,
                    ) {
                        let Some(clamped) = PixelRect::from_f64_rect_clamped(
                            sb,
                            0,
                            backend.width(),
                            backend.height(),
                        ) else {
                            if track_child_lens
                                && self.subtree_has_unwarmed_frozen_lens_in_branch(
                                    doc,
                                    obj,
                                    sym_ctx.map(|(m, _, _)| m),
                                    false,
                                )
                            {
                                if let Ok(mut dummy) = TinySkiaBackend::new(1, 1) {
                                    let prev_force = self.force_offscreen_lens_backdrop;
                                    self.force_offscreen_lens_backdrop = true;
                                    self.render_group_children(
                                        doc,
                                        page,
                                        obj,
                                        children,
                                        page_to_surface,
                                        parent_to_screen,
                                        obj_to_screen,
                                        sym_ctx,
                                        world_sym_xf,
                                        px_per_mm,
                                        view_mode,
                                        options,
                                        &mut dummy,
                                        underlying,
                                        track_child_lens,
                                        depth,
                                    );
                                    self.force_offscreen_lens_backdrop = prev_force;
                                }
                            }
                            // Group is completely outside the current surface/tile; do NOT fall through and drop opacity/blend_mode.
                            return;
                        };
                        if let Ok(mut sub) = TinySkiaBackend::new(clamped.width, clamped.height) {
                            sub.clear(Color::TRANSPARENT);
                            let shift =
                                Affine2D::translate(-f64::from(clamped.x), -f64::from(clamped.y));
                            let sub_page = shift.compose(&page_to_surface);
                            let sub_parent = shift.compose(&parent_to_screen);
                            let sub_xf = shift.compose(&obj_to_screen);
                            let prev_force = self.force_offscreen_lens_backdrop;
                            if track_child_lens {
                                self.force_offscreen_lens_backdrop = true;
                            }
                            self.render_group_children(
                                doc,
                                page,
                                obj,
                                children,
                                sub_page,
                                sub_parent,
                                sub_xf,
                                sym_ctx,
                                world_sym_xf,
                                px_per_mm,
                                view_mode,
                                options,
                                &mut sub,
                                underlying,
                                track_child_lens,
                                depth,
                            );
                            self.force_offscreen_lens_backdrop = prev_force;
                            let buf = sub.into_rgba();
                            backend.composite_region(clamped, &buf, obj.opacity, obj.blend_mode);
                            return;
                        }
                        // Downscale isolation buffer if `clamped` exceeds `MAX_SURFACE_DIM_PX` /
                        // `MAX_SURFACE_PIXELS` so opacity and blend_mode are never dropped.
                        let dim_scale = (f64::from(MAX_SURFACE_DIM_PX)
                            / f64::from(clamped.width.max(clamped.height).max(1)))
                        .min(1.0);
                        let px_total =
                            (f64::from(clamped.width) * f64::from(clamped.height)).max(1.0);
                        let px_scale = ((MAX_SURFACE_PIXELS as f64) / px_total).sqrt().min(1.0);
                        let ds = (dim_scale.min(px_scale) * 0.999).clamp(1e-4, 1.0);
                        let buf_w = ((f64::from(clamped.width) * ds).floor() as u32)
                            .clamp(1, MAX_SURFACE_DIM_PX);
                        let buf_h = ((f64::from(clamped.height) * ds).floor() as u32)
                            .clamp(1, MAX_SURFACE_DIM_PX);
                        if let Ok(mut sub) = TinySkiaBackend::new(buf_w, buf_h) {
                            sub.clear(Color::TRANSPARENT);
                            let sx = f64::from(buf_w) / f64::from(clamped.width);
                            let sy = f64::from(buf_h) / f64::from(clamped.height);
                            let shift = Affine2D::scale(sx, sy).compose(&Affine2D::translate(
                                -f64::from(clamped.x),
                                -f64::from(clamped.y),
                            ));
                            let sub_page = shift.compose(&page_to_surface);
                            let sub_parent = shift.compose(&parent_to_screen);
                            let sub_xf = shift.compose(&obj_to_screen);
                            let sub_px_per_mm = estimate_scale(&sub_parent);
                            let prev_force = self.force_offscreen_lens_backdrop;
                            if track_child_lens {
                                self.force_offscreen_lens_backdrop = true;
                            }
                            self.render_group_children(
                                doc,
                                page,
                                obj,
                                children,
                                sub_page,
                                sub_parent,
                                sub_xf,
                                sym_ctx,
                                world_sym_xf,
                                sub_px_per_mm,
                                view_mode,
                                options,
                                &mut sub,
                                underlying,
                                track_child_lens,
                                depth,
                            );
                            self.force_offscreen_lens_backdrop = prev_force;
                            let buf = sub.into_rgba();
                            let draw_xf =
                                Affine2D::translate(f64::from(clamped.x), f64::from(clamped.y))
                                    .compose(&Affine2D::scale(1.0 / sx, 1.0 / sy));
                            backend.draw_pixmap(&buf, draw_xf, obj.opacity, obj.blend_mode, true);
                        }
                        return;
                    } else {
                        return;
                    }
                }
                self.render_group_children(
                    doc,
                    page,
                    obj,
                    children,
                    page_to_surface,
                    parent_to_screen,
                    obj_to_screen,
                    sym_ctx,
                    world_sym_xf,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    underlying,
                    track_child_lens,
                    depth,
                );
            }
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                let pc_has_deform = obj.perspective.is_some()
                    || obj.effects.iter().any(|e| {
                        matches!(
                            e,
                            Effect::Distort { .. }
                                | Effect::Envelope { .. }
                                | Effect::Perspective { .. }
                        )
                    });
                let persp_container = if let Some(persp) = obj.perspective {
                    persp.transform_bezier_path(container)
                } else {
                    (**container).clone()
                };
                let pc_local_bb = pc_has_deform
                    .then(|| persp_container.bounding_box())
                    .flatten();
                let deformed_container =
                    apply_geometry_effects_in_bounds(&persp_container, pc_local_bb, &obj.effects);
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(
                        &deformed_container,
                        obj_to_screen,
                        px_per_mm,
                        options,
                        backend,
                    );
                    if pc_has_deform {
                        let frames = [DeformFrame {
                            pre_perspective: obj.perspective,
                            group_local_bb: pc_local_bb,
                            group_effects: &obj.effects,
                            rel_to_outer: Affine2D::IDENTITY,
                        }];
                        let mut visit = HashSet::new();
                        self.render_group_deformed_subtree(
                            doc,
                            page,
                            contents,
                            obj.id,
                            &frames,
                            page_to_surface,
                            obj_to_screen,
                            Affine2D::IDENTITY,
                            sym_ctx,
                            px_per_mm,
                            view_mode,
                            options,
                            backend,
                            underlying,
                            false,
                            depth + 1,
                            &mut visit,
                        );
                    } else {
                        for &cid in contents {
                            self.render_object_by_id_with_sym(
                                doc,
                                page,
                                cid,
                                page_to_surface,
                                obj_to_screen,
                                sym_ctx,
                                world_sym_xf,
                                view_mode,
                                options,
                                backend,
                                underlying,
                                depth + 1,
                            );
                        }
                    }
                    return;
                }

                // Underlay effects on PowerClip container
                if !obj.effects.is_empty() {
                    render_underlay_effects(
                        doc,
                        obj,
                        &deformed_container,
                        obj.transform,
                        parent_to_screen,
                        page_to_surface,
                        px_per_mm,
                        anti_alias,
                        &self.active_icc,
                        backend,
                    );
                }

                // Behind-fill outline of container
                if obj.outline.behind_fill && obj.outline.is_visible() {
                    draw_outline_styled(
                        &deformed_container,
                        obj.transform,
                        parent_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        px_per_mm,
                        anti_alias,
                        &self.active_icc,
                        backend,
                    );
                }

                // Container background fill
                if obj.fill.is_visible() {
                    let mesh_deform = pc_has_deform.then(|| MeshDeformCtx {
                        raw_local_bb: container.bounding_box(),
                        pre_perspective: obj.perspective,
                        pre_effect_bb: pc_local_bb,
                        effects: &obj.effects,
                        c_xf: Affine2D::IDENTITY,
                        deform_frames: &[],
                    });
                    self.draw_fill_styled_with_deform(
                        doc,
                        &deformed_container,
                        &obj.fill,
                        obj_to_screen,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                        backend,
                        depth,
                        mesh_deform.as_ref(),
                    );
                }

                // Clip contents strictly to container contour!
                backend.push_clip(&deformed_container, FillRule::Winding, obj_to_screen);
                let sym_map = sym_ctx.map(|(m, _, _)| m);
                let track_pc_lens = contents.iter().any(|&cid| {
                    sym_map
                        .and_then(|m| m.get(&cid).copied())
                        .or_else(|| doc.object(cid))
                        .is_some_and(|c| {
                            let mut v = HashSet::from([cid]);
                            let mut vs = HashSet::new();
                            crate::export_render::object_or_subtree_has_lens_with_sym(
                                doc, c, sym_map, 0, &mut v, &mut vs,
                            )
                        })
                });
                let pc_sym_root_idx = if obj.id.0 == 0 {
                    self.current_sym_root_idx
                } else {
                    None
                };
                let saved_sym_root_idx = self.current_sym_root_idx.take();
                let pc_world_xf = page_to_surface
                    .inverse()
                    .map(|inv| inv.compose(&obj_to_screen))
                    .filter(|m| m.is_finite())
                    .unwrap_or(obj.transform);
                let mut pc_underlying = track_pc_lens.then(|| {
                    let mut v = underlying.to_vec();
                    v.push(BackdropItem::EnterPowerClip {
                        pc_id: obj.id,
                        sym_root_idx: pc_sym_root_idx,
                    });
                    if pc_has_deform {
                        v.push(BackdropItem::EnterDeformedGroup {
                            group_id: obj.id,
                            group_sym_root_idx: pc_sym_root_idx,
                            group_local_bb: pc_local_bb,
                            group_world_xf: Some(pc_world_xf),
                        });
                    }
                    v
                });
                if pc_has_deform {
                    let frames = [DeformFrame {
                        pre_perspective: obj.perspective,
                        group_local_bb: pc_local_bb,
                        group_effects: &obj.effects,
                        rel_to_outer: Affine2D::IDENTITY,
                    }];
                    let u_ref = pc_underlying.as_deref().unwrap_or(underlying);
                    let mut visit = HashSet::new();
                    self.render_group_deformed_subtree(
                        doc,
                        page,
                        contents,
                        obj.id,
                        &frames,
                        page_to_surface,
                        obj_to_screen,
                        Affine2D::IDENTITY,
                        sym_ctx,
                        px_per_mm,
                        view_mode,
                        options,
                        backend,
                        u_ref,
                        track_pc_lens,
                        depth + 1,
                        &mut visit,
                    );
                    if let Some(ref mut v) = pc_underlying {
                        v.push(BackdropItem::ExitDeformedGroup);
                    }
                } else {
                    for &cid in contents {
                        let child_u = pc_underlying.as_deref().unwrap_or(underlying);
                        self.render_object_by_id_with_sym(
                            doc,
                            page,
                            cid,
                            page_to_surface,
                            obj_to_screen,
                            sym_ctx,
                            world_sym_xf,
                            view_mode,
                            options,
                            backend,
                            child_u,
                            depth + 1,
                        );
                        if let Some(ref mut u) = pc_underlying {
                            u.push(BackdropItem::Object(cid));
                        }
                    }
                }
                self.current_sym_root_idx = saved_sym_root_idx;
                backend.pop_clip();

                // Overlay effects on PowerClip container (e.g. Contour::Inside, Extrude bevel)
                if !obj.effects.is_empty() {
                    render_overlay_effects(
                        obj,
                        &deformed_container,
                        obj.transform,
                        parent_to_screen,
                        px_per_mm,
                        anti_alias,
                        &self.active_icc,
                        backend,
                    );
                }

                // Foreground container outline
                if !obj.outline.behind_fill && obj.outline.is_visible() {
                    draw_outline_styled(
                        &deformed_container,
                        obj.transform,
                        parent_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        px_per_mm,
                        anti_alias,
                        &self.active_icc,
                        backend,
                    );
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if !self.active_symbols.insert(*sym_id) {
                    return;
                }
                if let Some(sym) = doc.symbol(*sym_id) {
                    let sym_root_world_xf = page_to_surface
                        .inverse()
                        .map(|inv| inv.compose(&obj_to_screen))
                        .filter(|m| m.is_finite())
                        .unwrap_or(obj.transform);
                    let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                    let mut referenced: HashSet<ObjectId> = HashSet::new();
                    for sub in &sym.objects {
                        if sub.id.0 != 0 {
                            child_map.insert(sub.id, sub);
                        }
                        match &sub.payload {
                            NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                            NodePayload::PowerClip { contents, .. } => {
                                referenced.extend(contents.iter().copied());
                            }
                            _ => {}
                        }
                    }
                    let sym_has_lens = sym.objects.iter().any(|sub| {
                        if sub.id.0 != 0 && referenced.contains(&sub.id) {
                            return false;
                        }
                        let mut v = if sub.id.0 != 0 {
                            HashSet::from([sub.id])
                        } else {
                            HashSet::new()
                        };
                        let mut vs = HashSet::new();
                        crate::export_render::object_or_subtree_has_lens_with_sym(
                            doc,
                            sub,
                            Some(&child_map),
                            0,
                            &mut v,
                            &mut vs,
                        )
                    });
                    let obj_opacity = sanitize_f32(obj.opacity, 1.0).clamp(0.0, 1.0);
                    let needs_isolation = view_mode != ViewMode::Wireframe
                        && (obj_opacity < 0.999 || obj.blend_mode != BlendMode::Normal);
                    let parent_sym_root_idx = self.current_sym_root_idx.take();
                    let scope_prev_len = self.frozen_scope_stack.len();
                    let sym_u128 = sym_id.0.as_u128();
                    self.frozen_scope_stack.extend_from_slice(&[
                        FROZEN_TAG_SYM_INST,
                        sym_u128 as u64,
                        (sym_u128 >> 64) as u64,
                        obj.id.0,
                        parent_sym_root_idx.map_or(u64::MAX, |i| i as u64),
                    ]);

                    let render_roots =
                        |renderer: &mut Self,
                         target_page_to_surface: Affine2D,
                         target_parent_to_screen: Affine2D,
                         target_obj_to_screen: Affine2D,
                         target_px_per_mm: f64,
                         target_backend: &mut dyn CanvasBackend| {
                            renderer.render_symbol_instance_contents(
                                doc,
                                page,
                                obj,
                                *sym_id,
                                sym,
                                &child_map,
                                &referenced,
                                sym_root_world_xf,
                                sym_has_lens,
                                parent_sym_root_idx,
                                target_page_to_surface,
                                target_parent_to_screen,
                                target_obj_to_screen,
                                target_px_per_mm,
                                view_mode,
                                options,
                                target_backend,
                                underlying,
                                depth,
                            );
                        };

                    if needs_isolation {
                        let mut visited_doc = HashSet::new();
                        let mut visited_syms = HashSet::new();
                        if let Some(sb) = Self::compute_object_screen_bounds(
                            doc,
                            obj,
                            page_to_surface,
                            parent_to_screen,
                            sym_ctx,
                            world_sym_xf,
                            false,
                            &mut visited_doc,
                            &mut visited_syms,
                            depth,
                        ) {
                            if let Some(clamped) = PixelRect::from_f64_rect_clamped(
                                sb,
                                0,
                                backend.width(),
                                backend.height(),
                            ) {
                                if let Ok(mut sub) =
                                    TinySkiaBackend::new(clamped.width, clamped.height)
                                {
                                    sub.clear(Color::TRANSPARENT);
                                    let shift = Affine2D::translate(
                                        -f64::from(clamped.x),
                                        -f64::from(clamped.y),
                                    );
                                    let sub_page = shift.compose(&page_to_surface);
                                    let sub_parent = shift.compose(&parent_to_screen);
                                    let sub_obj = shift.compose(&obj_to_screen);
                                    let prev_force = self.force_offscreen_lens_backdrop;
                                    if sym_has_lens {
                                        self.force_offscreen_lens_backdrop = true;
                                    }
                                    render_roots(
                                        self, sub_page, sub_parent, sub_obj, px_per_mm, &mut sub,
                                    );
                                    self.force_offscreen_lens_backdrop = prev_force;
                                    let buf = sub.into_rgba();
                                    backend.composite_region(
                                        clamped,
                                        &buf,
                                        obj_opacity,
                                        obj.blend_mode,
                                    );
                                } else {
                                    let dim_scale = (f64::from(MAX_SURFACE_DIM_PX)
                                        / f64::from(clamped.width.max(clamped.height).max(1)))
                                    .min(1.0);
                                    let px_total = (f64::from(clamped.width)
                                        * f64::from(clamped.height))
                                    .max(1.0);
                                    let px_scale =
                                        ((MAX_SURFACE_PIXELS as f64) / px_total).sqrt().min(1.0);
                                    let ds = (dim_scale.min(px_scale) * 0.999).clamp(1e-4, 1.0);
                                    let buf_w = ((f64::from(clamped.width) * ds).floor() as u32)
                                        .clamp(1, MAX_SURFACE_DIM_PX);
                                    let buf_h = ((f64::from(clamped.height) * ds).floor() as u32)
                                        .clamp(1, MAX_SURFACE_DIM_PX);
                                    if let Ok(mut sub) = TinySkiaBackend::new(buf_w, buf_h) {
                                        sub.clear(Color::TRANSPARENT);
                                        let sx = f64::from(buf_w) / f64::from(clamped.width);
                                        let sy = f64::from(buf_h) / f64::from(clamped.height);
                                        let shift =
                                            Affine2D::scale(sx, sy).compose(&Affine2D::translate(
                                                -f64::from(clamped.x),
                                                -f64::from(clamped.y),
                                            ));
                                        let sub_page = shift.compose(&page_to_surface);
                                        let sub_parent = shift.compose(&parent_to_screen);
                                        let sub_obj = shift.compose(&obj_to_screen);
                                        let sub_px_per_mm = estimate_scale(&sub_parent);
                                        let prev_force = self.force_offscreen_lens_backdrop;
                                        if sym_has_lens {
                                            self.force_offscreen_lens_backdrop = true;
                                        }
                                        render_roots(
                                            self,
                                            sub_page,
                                            sub_parent,
                                            sub_obj,
                                            sub_px_per_mm,
                                            &mut sub,
                                        );
                                        self.force_offscreen_lens_backdrop = prev_force;
                                        let buf = sub.into_rgba();
                                        let draw_xf = Affine2D::translate(
                                            f64::from(clamped.x),
                                            f64::from(clamped.y),
                                        )
                                        .compose(&Affine2D::scale(1.0 / sx, 1.0 / sy));
                                        backend.draw_pixmap(
                                            &buf,
                                            draw_xf,
                                            obj_opacity,
                                            obj.blend_mode,
                                            true,
                                        );
                                    }
                                }
                            } else if sym_has_lens
                                && self.subtree_has_unwarmed_frozen_lens_in_branch(
                                    doc,
                                    obj,
                                    sym_ctx.map(|(m, _, _)| m),
                                    true,
                                )
                            {
                                if let Ok(mut dummy) = TinySkiaBackend::new(1, 1) {
                                    let prev_force = self.force_offscreen_lens_backdrop;
                                    self.force_offscreen_lens_backdrop = true;
                                    render_roots(
                                        self,
                                        page_to_surface,
                                        parent_to_screen,
                                        obj_to_screen,
                                        px_per_mm,
                                        &mut dummy,
                                    );
                                    self.force_offscreen_lens_backdrop = prev_force;
                                }
                            }
                        }
                    } else {
                        render_roots(
                            self,
                            page_to_surface,
                            parent_to_screen,
                            obj_to_screen,
                            px_per_mm,
                            backend,
                        );
                    }

                    self.frozen_scope_stack.truncate(scope_prev_len);
                    self.current_sym_root_idx = parent_sym_root_idx;
                }
                self.active_symbols.remove(sym_id);
            }
        }
    }

    fn expand_screen_bounds_for_deforms(
        effects: &[Effect],
        obj_to_screen: &Affine2D,
        local_h: Option<f64>,
        acc: &mut Rect2D,
    ) {
        if !obj_to_screen.is_finite() {
            return;
        }
        let union_pt = |local_pt: Point2D, out: &mut Rect2D| {
            if !local_pt.is_finite() {
                return;
            }
            let sp = obj_to_screen.transform_point(local_pt);
            if sp.is_finite() {
                *out = out.union(Rect2D::new(sp.x, sp.y, sp.x, sp.y));
            }
        };
        let eval_quad = |p0: Point2D, p1: Point2D, p2: Point2D, t: f64| -> Point2D {
            let it = 1.0 - t;
            Point2D::new(
                it * it * p0.x + 2.0 * it * t * p1.x + t * t * p2.x,
                it * it * p0.y + 2.0 * it * t * p1.y + t * t * p2.y,
            )
        };
        let mut cur_local_h = local_h.filter(|v| v.is_finite() && *v > 0.0);
        for eff in effects {
            match eff {
                Effect::Perspective { quad } => {
                    let (mut q_min_y, mut q_max_y) = (f64::INFINITY, f64::NEG_INFINITY);
                    for &p in quad {
                        union_pt(p, acc);
                        if p.is_finite() {
                            q_min_y = q_min_y.min(p.y);
                            q_max_y = q_max_y.max(p.y);
                        }
                    }
                    if q_min_y.is_finite() && q_max_y.is_finite() {
                        let q_h = (q_max_y - q_min_y).max(1e-6);
                        cur_local_h = Some(cur_local_h.unwrap_or(0.0).max(q_h));
                    }
                }
                Effect::Distort {
                    kind,
                    amplitude,
                    center,
                    ..
                } => {
                    if amplitude.is_finite() && amplitude.abs() > 1e-9 {
                        if matches!(kind, coree_core::DistortKind::Twister) && center.is_finite() {
                            if let Some(inv_xf) = obj_to_screen.inverse() {
                                if let Some(local_acc) = inv_xf.try_transform_rect(*acc) {
                                    let corners = [
                                        Point2D::new(local_acc.min_x, local_acc.min_y),
                                        Point2D::new(local_acc.max_x, local_acc.min_y),
                                        Point2D::new(local_acc.max_x, local_acc.max_y),
                                        Point2D::new(local_acc.min_x, local_acc.max_y),
                                    ];
                                    let r_max = corners
                                        .iter()
                                        .map(|c| (*c - *center).length())
                                        .fold(0.0_f64, f64::max);
                                    if r_max.is_finite() && r_max > 0.0 {
                                        let sq = Rect2D::new(
                                            center.x - r_max,
                                            center.y - r_max,
                                            center.x + r_max,
                                            center.y + r_max,
                                        );
                                        if let Some(sb) = obj_to_screen.try_transform_rect(sq) {
                                            *acc = acc.union(sb);
                                        }
                                        cur_local_h =
                                            Some(cur_local_h.unwrap_or(0.0).max(2.0 * r_max));
                                    }
                                }
                            }
                        } else {
                            let pad = 2.0 * amplitude.abs();
                            let screen_pad_x =
                                (obj_to_screen.m[0].abs() + obj_to_screen.m[2].abs()) * pad;
                            let screen_pad_y =
                                (obj_to_screen.m[1].abs() + obj_to_screen.m[3].abs()) * pad;
                            if screen_pad_x.is_finite() && screen_pad_y.is_finite() {
                                *acc = Rect2D::new(
                                    acc.min_x - screen_pad_x,
                                    acc.min_y - screen_pad_y,
                                    acc.max_x + screen_pad_x,
                                    acc.max_y + screen_pad_y,
                                );
                            }
                            cur_local_h = Some(cur_local_h.unwrap_or(0.0) + pad);
                        }
                    }
                }
                Effect::Envelope { mode, cage } if cage.len() >= 4 => {
                    let (mut c_min_y, mut c_max_y) = (f64::INFINITY, f64::NEG_INFINITY);
                    for &p in cage {
                        union_pt(p, acc);
                        if p.is_finite() {
                            c_min_y = c_min_y.min(p.y);
                            c_max_y = c_max_y.max(p.y);
                        }
                    }
                    if cage.len() >= 12 {
                        let (tl, tr, br, bl) = (cage[0], cage[3], cage[6], cage[9]);
                        if cage[..12].iter().all(|p| p.is_finite()) {
                            let eval_cubic =
                                |p0: Point2D, p1: Point2D, p2: Point2D, p3: Point2D, t: f64| {
                                    let mt = 1.0 - t;
                                    let mt2 = mt * mt;
                                    let t2 = t * t;
                                    let b0 = mt2 * mt;
                                    let b1 = 3.0 * mt2 * t;
                                    let b2 = 3.0 * mt * t2;
                                    let b3 = t2 * t;
                                    Point2D::new(
                                        b0 * p0.x + b1 * p1.x + b2 * p2.x + b3 * p3.x,
                                        b0 * p0.y + b1 * p1.y + b2 * p2.y + b3 * p3.y,
                                    )
                                };
                            let eval_coons12 = |u: f64, v: f64| -> Point2D {
                                if *mode == coree_core::EnvelopeMode::Straight {
                                    return tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
                                }
                                let top = eval_cubic(tl, cage[1], cage[2], tr, u);
                                let bottom = eval_cubic(bl, cage[8], cage[7], br, u);
                                let left = eval_cubic(tl, cage[11], cage[10], bl, v);
                                let right = eval_cubic(tr, cage[4], cage[5], br, v);
                                let ruled_u = top.lerp(bottom, v);
                                let ruled_v = left.lerp(right, u);
                                let bilin = tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
                                Point2D::new(
                                    ruled_u.x + ruled_v.x - bilin.x,
                                    ruled_u.y + ruled_v.y - bilin.y,
                                )
                            };
                            let (mut s_min_y, mut s_max_y) = (f64::INFINITY, f64::NEG_INFINITY);
                            for iu in 0..=16 {
                                let u = f64::from(iu) * 0.0625;
                                for iv in 0..=16 {
                                    let v = f64::from(iv) * 0.0625;
                                    let pt = eval_coons12(u, v);
                                    if pt.is_finite() {
                                        s_min_y = s_min_y.min(pt.y);
                                        s_max_y = s_max_y.max(pt.y);
                                        union_pt(pt, acc);
                                    }
                                }
                            }
                            if s_min_y.is_finite() && s_max_y.is_finite() {
                                let env_h = (s_max_y - s_min_y).max(1e-6);
                                cur_local_h = Some(cur_local_h.unwrap_or(0.0).max(env_h));
                            }
                        } else if c_min_y.is_finite() && c_max_y.is_finite() {
                            let env_h = (c_max_y - c_min_y).max(1e-6);
                            cur_local_h = Some(cur_local_h.unwrap_or(0.0).max(env_h));
                        }
                    } else if cage.len() >= 8 {
                        let (tl, tm, tr, rm, br, bm, bl, lm) = (
                            cage[0], cage[1], cage[2], cage[3], cage[4], cage[5], cage[6], cage[7],
                        );
                        if [tl, tm, tr, rm, br, bm, bl, lm]
                            .iter()
                            .all(|p| p.is_finite())
                        {
                            let eval_coons = |u: f64, v: f64| -> Point2D {
                                let top = eval_quad(tl, tm, tr, u);
                                let bottom = eval_quad(bl, bm, br, u);
                                let left = eval_quad(tl, lm, bl, v);
                                let right = eval_quad(tr, rm, br, v);
                                let ruled_u = top.lerp(bottom, v);
                                let ruled_v = left.lerp(right, u);
                                let bilin = tl.lerp(tr, u).lerp(bl.lerp(br, u), v);
                                Point2D::new(
                                    ruled_u.x + ruled_v.x - bilin.x,
                                    ruled_u.y + ruled_v.y - bilin.y,
                                )
                            };
                            let quad_vertex_t = |f0: f64, f_half: f64, f1: f64| -> Option<f64> {
                                let a = 2.0 * f0 - 4.0 * f_half + 2.0 * f1;
                                let b = -3.0 * f0 + 4.0 * f_half - f1;
                                if a.abs() > 1e-12 {
                                    let t = -b / (2.0 * a);
                                    if t > 0.0 && t < 1.0 && t.is_finite() {
                                        return Some(t);
                                    }
                                }
                                None
                            };
                            let (mut s_min_y, mut s_max_y) = (c_min_y, c_max_y);
                            let mut record_pt = |pt: Point2D| {
                                s_min_y = s_min_y.min(pt.y);
                                s_max_y = s_max_y.max(pt.y);
                                union_pt(pt, acc);
                            };
                            for iu in 0..=16 {
                                let u = f64::from(iu) * 0.0625;
                                for iv in 0..=16 {
                                    let v = f64::from(iv) * 0.0625;
                                    record_pt(eval_coons(u, v));
                                }
                                let p0 = eval_coons(u, 0.0);
                                let pm = eval_coons(u, 0.5);
                                let p1 = eval_coons(u, 1.0);
                                for vt in [
                                    quad_vertex_t(p0.x, pm.x, p1.x),
                                    quad_vertex_t(p0.y, pm.y, p1.y),
                                ]
                                .into_iter()
                                .flatten()
                                {
                                    record_pt(eval_coons(u, vt));
                                    let q0 = eval_coons(0.0, vt);
                                    let qm = eval_coons(0.5, vt);
                                    let q1 = eval_coons(1.0, vt);
                                    for ut in [
                                        quad_vertex_t(q0.x, qm.x, q1.x),
                                        quad_vertex_t(q0.y, qm.y, q1.y),
                                    ]
                                    .into_iter()
                                    .flatten()
                                    {
                                        record_pt(eval_coons(ut, vt));
                                    }
                                }
                            }
                            if s_min_y.is_finite() && s_max_y.is_finite() {
                                let env_h = (s_max_y - s_min_y).max(1e-6);
                                cur_local_h = Some(cur_local_h.unwrap_or(0.0).max(env_h));
                            }
                        } else if c_min_y.is_finite() && c_max_y.is_finite() {
                            let env_h = (c_max_y - c_min_y).max(1e-6);
                            cur_local_h = Some(cur_local_h.unwrap_or(0.0).max(env_h));
                        }
                    } else {
                        let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
                        for &p in &cage[..4] {
                            if p.is_finite() {
                                min_y = min_y.min(p.y);
                                max_y = max_y.max(p.y);
                            }
                        }
                        if min_y.is_finite() && max_y.is_finite() {
                            let cage_h = (max_y - min_y).max(1e-6);
                            if matches!(
                                mode,
                                coree_core::EnvelopeMode::SingleArc
                                    | coree_core::EnvelopeMode::DoubleArc
                            ) {
                                let h_ref = cur_local_h
                                    .filter(|v| v.is_finite() && *v > 0.0)
                                    .unwrap_or(cage_h)
                                    .max(cage_h);
                                let arc_pad = h_ref * 0.08;
                                for &p in &cage[..4] {
                                    union_pt(Point2D::new(p.x, p.y - arc_pad), acc);
                                    union_pt(Point2D::new(p.x, p.y + arc_pad), acc);
                                }
                                cur_local_h = Some(h_ref.max(cage_h + 2.0 * arc_pad));
                            } else {
                                cur_local_h = Some(cur_local_h.unwrap_or(0.0).max(cage_h));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_group_children(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        children: &[ObjectId],
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        obj_to_screen: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        world_sym_xf: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        track_child_lens: bool,
        depth: usize,
    ) {
        let anti_alias = view_mode != ViewMode::Pixels;
        let group_has_deform = obj.perspective.is_some()
            || obj.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::Distort { .. } | Effect::Envelope { .. } | Effect::Perspective { .. }
                )
            });
        let group_has_underlay = view_mode != ViewMode::Wireframe
            && obj.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::DropShadow { .. }
                        | Effect::BlockShadow { .. }
                        | Effect::Contour {
                            mode: coree_core::ContourMode::Outside,
                            ..
                        }
                        | Effect::Extrude25D { .. }
                        | Effect::Extrude3D { .. }
                        | Effect::Blend { .. }
                )
            });
        let group_has_overlay = view_mode != ViewMode::Wireframe
            && obj.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::Contour {
                        mode: coree_core::ContourMode::Inside | coree_core::ContourMode::ToCenter,
                        ..
                    } | Effect::Extrude25D { .. }
                        | Effect::Extrude3D { .. }
                        | Effect::InnerShadow { .. }
                        | Effect::Bevel { .. }
                )
            });

        if !group_has_deform && !group_has_underlay && !group_has_overlay {
            let saved_sym_root_idx = self.current_sym_root_idx.take();
            let mut child_underlying = track_child_lens.then(|| underlying.to_vec());
            for &cid in children {
                let child_u = child_underlying.as_deref().unwrap_or(underlying);
                self.render_object_by_id_with_sym(
                    doc,
                    page,
                    cid,
                    page_to_surface,
                    obj_to_screen,
                    sym_ctx,
                    world_sym_xf,
                    view_mode,
                    options,
                    backend,
                    child_u,
                    depth + 1,
                );
                if let Some(ref mut u) = child_underlying {
                    u.push(BackdropItem::Object(cid));
                }
            }
            self.current_sym_root_idx = saved_sym_root_idx;
            return;
        }

        let group_world_xf = page_to_surface
            .inverse()
            .map(|inv| inv.compose(&obj_to_screen))
            .filter(|m| m.is_finite())
            .unwrap_or(obj.transform);
        let raw_group_local_bb = if sym_ctx.is_none() && obj.id.0 != 0 {
            doc.local_object_bounds(obj.id)
        } else {
            let mut visit = HashSet::new();
            Self::compute_children_local_bounds(
                doc,
                children,
                sym_ctx,
                Affine2D::IDENTITY,
                group_world_xf,
                depth + 1,
                &mut visit,
            )
        };
        let group_local_bb = if let Some(persp) = obj.perspective {
            raw_group_local_bb.and_then(|lb| persp.try_transform_rect(lb))
        } else {
            raw_group_local_bb
        };

        let mut proxy_holder: Option<SceneObject> = None;
        let mut deformed_sil_holder: Option<BezierPath> = None;
        if group_has_underlay || group_has_overlay {
            let mut visit = HashSet::new();
            let mut sil = self.collect_group_local_silhouette(
                doc,
                children,
                sym_ctx,
                Affine2D::IDENTITY,
                depth + 1,
                &mut visit,
            );
            if let Some(persp) = obj.perspective {
                sil = persp.transform_bezier_path(&sil);
            }
            let deformed_sil = apply_geometry_effects_in_bounds(&sil, group_local_bb, &obj.effects);
            let mut proxy = obj.clone();
            if !proxy.fill.is_visible() {
                proxy.fill = Self::first_visible_child_fill(doc, children, sym_ctx)
                    .unwrap_or(Fill::Solid(Color::BLACK));
            }
            deformed_sil_holder = Some(deformed_sil);
            proxy_holder = Some(proxy);
        }

        if group_has_underlay {
            if let (Some(proxy), Some(deformed_sil)) = (&proxy_holder, &deformed_sil_holder) {
                render_underlay_effects(
                    doc,
                    proxy,
                    deformed_sil,
                    obj.transform,
                    parent_to_screen,
                    page_to_surface,
                    px_per_mm,
                    anti_alias,
                    &self.active_icc,
                    backend,
                );
            }
        }

        let group_sym_root_idx = if obj.id.0 == 0 {
            self.current_sym_root_idx
        } else {
            None
        };
        let saved_sym_root_idx = self.current_sym_root_idx.take();
        if group_has_deform {
            let mut group_underlying = track_child_lens.then(|| {
                let mut v = underlying.to_vec();
                v.push(BackdropItem::EnterDeformedGroup {
                    group_id: obj.id,
                    group_sym_root_idx,
                    group_local_bb,
                    group_world_xf: Some(group_world_xf),
                });
                v
            });
            let u_ref = group_underlying.as_deref().unwrap_or(underlying);
            let frames = [DeformFrame {
                pre_perspective: obj.perspective,
                group_local_bb,
                group_effects: &obj.effects,
                rel_to_outer: Affine2D::IDENTITY,
            }];
            let mut visit = HashSet::new();
            self.render_group_deformed_subtree(
                doc,
                page,
                children,
                obj.id,
                &frames,
                page_to_surface,
                obj_to_screen,
                Affine2D::IDENTITY,
                sym_ctx,
                px_per_mm,
                view_mode,
                options,
                backend,
                u_ref,
                track_child_lens,
                depth + 1,
                &mut visit,
            );
            if let Some(ref mut v) = group_underlying {
                v.push(BackdropItem::ExitDeformedGroup);
            }
        } else {
            let mut child_underlying = track_child_lens.then(|| underlying.to_vec());
            for &cid in children {
                let child_u = child_underlying.as_deref().unwrap_or(underlying);
                self.render_object_by_id_with_sym(
                    doc,
                    page,
                    cid,
                    page_to_surface,
                    obj_to_screen,
                    sym_ctx,
                    world_sym_xf,
                    view_mode,
                    options,
                    backend,
                    child_u,
                    depth + 1,
                );
                if let Some(ref mut u) = child_underlying {
                    u.push(BackdropItem::Object(cid));
                }
            }
        }
        self.current_sym_root_idx = saved_sym_root_idx;

        if group_has_overlay {
            if let (Some(proxy), Some(deformed_sil)) = (&proxy_holder, &deformed_sil_holder) {
                render_overlay_effects(
                    proxy,
                    deformed_sil,
                    obj.transform,
                    parent_to_screen,
                    px_per_mm,
                    anti_alias,
                    &self.active_icc,
                    backend,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_symbol_instance_contents(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        sym_id: SymbolId,
        sym: &SymbolDef,
        child_map: &HashMap<ObjectId, &SceneObject>,
        referenced: &HashSet<ObjectId>,
        sym_root_world_xf: Affine2D,
        sym_has_lens: bool,
        parent_sym_root_idx: Option<usize>,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        obj_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        depth: usize,
    ) {
        let saved_sym_bounds_cache = std::mem::take(&mut self.sym_bounds_cache);
        let anti_alias = view_mode != ViewMode::Pixels;
        let sym_has_deform = obj.perspective.is_some()
            || obj.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::Distort { .. } | Effect::Envelope { .. } | Effect::Perspective { .. }
                )
            });
        let sym_has_underlay = view_mode != ViewMode::Wireframe
            && obj.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::DropShadow { .. }
                        | Effect::BlockShadow { .. }
                        | Effect::Contour {
                            mode: coree_core::ContourMode::Outside,
                            ..
                        }
                        | Effect::Extrude25D { .. }
                        | Effect::Extrude3D { .. }
                        | Effect::Blend { .. }
                )
            });
        let sym_has_overlay = view_mode != ViewMode::Wireframe
            && obj.effects.iter().any(|e| {
                matches!(
                    e,
                    Effect::Contour {
                        mode: coree_core::ContourMode::Inside | coree_core::ContourMode::ToCenter,
                        ..
                    } | Effect::Extrude25D { .. }
                        | Effect::Extrude3D { .. }
                        | Effect::InnerShadow { .. }
                        | Effect::Bevel { .. }
                )
            });

        let raw_sym_local_bb = if sym_has_deform || sym_has_underlay || sym_has_overlay {
            doc.symbol_bounds_with_xf_and_space(
                sym_id,
                &Affine2D::IDENTITY,
                Some(&sym_root_world_xf),
            )
        } else {
            None
        };
        let sym_local_bb = if let Some(persp) = obj.perspective {
            raw_sym_local_bb.and_then(|lb| persp.try_transform_rect(lb))
        } else {
            raw_sym_local_bb
        };

        let mut proxy_holder: Option<SceneObject> = None;
        let mut deformed_sil_holder: Option<BezierPath> = None;
        if sym_has_underlay || sym_has_overlay {
            let mut sil = self.collect_symbol_local_silhouette(
                doc,
                sym,
                (child_map, &sym.objects, sym_root_world_xf),
                referenced,
                Affine2D::IDENTITY,
                depth + 1,
            );
            if let Some(persp) = obj.perspective {
                sil = persp.transform_bezier_path(&sil);
            }
            let deformed_sil = apply_geometry_effects_in_bounds(&sil, sym_local_bb, &obj.effects);
            let mut proxy = obj.clone();
            if !proxy.fill.is_visible() {
                proxy.fill = Self::first_visible_symbol_fill(doc, sym, child_map, referenced)
                    .unwrap_or(Fill::Solid(Color::BLACK));
            }
            deformed_sil_holder = Some(deformed_sil);
            proxy_holder = Some(proxy);
        }

        if sym_has_underlay {
            if let (Some(proxy), Some(deformed_sil)) = (&proxy_holder, &deformed_sil_holder) {
                render_underlay_effects(
                    doc,
                    proxy,
                    deformed_sil,
                    obj.transform,
                    parent_to_screen,
                    page_to_surface,
                    px_per_mm,
                    anti_alias,
                    &self.active_icc,
                    backend,
                );
            }
        }

        let mut sym_underlying = sym_has_lens.then(|| {
            let mut v = underlying.to_vec();
            v.push(BackdropItem::EnterSymbolInstance {
                sym_id,
                sym_world_xf: sym_root_world_xf,
                inst_id: obj.id,
                inst_sym_root_idx: parent_sym_root_idx,
            });
            v
        });

        if sym_has_deform {
            let frames = [DeformFrame {
                pre_perspective: obj.perspective,
                group_local_bb: sym_local_bb,
                group_effects: &obj.effects,
                rel_to_outer: Affine2D::IDENTITY,
            }];
            let mut sym_visit = HashSet::new();
            for (idx, sub) in sym.objects.iter().enumerate() {
                if sub.id.0 != 0 && referenced.contains(&sub.id) {
                    continue;
                }
                if sub.id.0 != 0 && !sym_visit.insert(sub.id) {
                    if let Some(ref mut u) = sym_underlying {
                        u.push(BackdropItem::Object(sub.id));
                    }
                    continue;
                }
                self.current_sym_root_idx = if sub.id.0 == 0 { Some(idx) } else { None };
                let sub_u = sym_underlying.as_deref().unwrap_or(underlying);
                self.render_deformed_scene_object(
                    doc,
                    page,
                    sub,
                    &frames,
                    page_to_surface,
                    obj_to_screen,
                    Affine2D::IDENTITY,
                    Some((child_map, &sym.objects, sym_root_world_xf)),
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    sub_u,
                    sym_has_lens,
                    depth + 1,
                    &mut sym_visit,
                );
                if sub.id.0 != 0 {
                    sym_visit.remove(&sub.id);
                }
                if let Some(ref mut u) = sym_underlying {
                    if sub.id.0 != 0 {
                        u.push(BackdropItem::Object(sub.id));
                    } else {
                        u.push(BackdropItem::SymObjectIndex(idx));
                    }
                }
            }
        } else {
            for (idx, sub) in sym.objects.iter().enumerate() {
                if sub.id.0 != 0 && referenced.contains(&sub.id) {
                    continue;
                }
                self.current_sym_root_idx = if sub.id.0 == 0 { Some(idx) } else { None };
                let sub_u = sym_underlying.as_deref().unwrap_or(underlying);
                self.render_scene_object_with_sym(
                    doc,
                    page,
                    sub,
                    page_to_surface,
                    obj_to_screen,
                    Some((child_map, &sym.objects, sym_root_world_xf)),
                    Affine2D::IDENTITY,
                    view_mode,
                    options,
                    backend,
                    sub_u,
                    depth + 1,
                );
                if let Some(ref mut u) = sym_underlying {
                    if sub.id.0 != 0 {
                        u.push(BackdropItem::Object(sub.id));
                    } else {
                        u.push(BackdropItem::SymObjectIndex(idx));
                    }
                }
            }
        }
        if let Some(ref mut u) = sym_underlying {
            u.push(BackdropItem::ExitSymbolInstance);
        }
        self.sym_bounds_cache = saved_sym_bounds_cache;

        if sym_has_overlay {
            if let (Some(proxy), Some(deformed_sil)) = (&proxy_holder, &deformed_sil_holder) {
                render_overlay_effects(
                    proxy,
                    deformed_sil,
                    obj.transform,
                    parent_to_screen,
                    px_per_mm,
                    anti_alias,
                    &self.active_icc,
                    backend,
                );
            }
        }
    }

    fn first_visible_symbol_fill(
        doc: &Document,
        sym: &SymbolDef,
        child_map: &HashMap<ObjectId, &SceneObject>,
        referenced: &HashSet<ObjectId>,
    ) -> Option<Fill> {
        for sub in &sym.objects {
            if sub.id.0 != 0 && referenced.contains(&sub.id) {
                continue;
            }
            if !sub.visible {
                continue;
            }
            if sub.fill.is_visible() {
                return Some(sub.fill.clone());
            }
            let ctx = Some((child_map, sym.objects.as_slice(), Affine2D::IDENTITY));
            match &sub.payload {
                NodePayload::Group(c) => {
                    if let Some(f) = Self::first_visible_child_fill(doc, c, ctx) {
                        return Some(f);
                    }
                }
                NodePayload::Clone { .. } => {
                    if let Some((_, f, _)) =
                        doc.resolve_clone_payload_and_style_with_sym(sub, Some(child_map))
                    {
                        if f.is_visible() {
                            return Some(f);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn first_visible_child_fill(
        doc: &Document,
        children: &[ObjectId],
        sym_ctx: Option<SymRenderCtx<'_>>,
    ) -> Option<Fill> {
        for &cid in children {
            let cobj = sym_ctx
                .and_then(|(m, _, _)| m.get(&cid).copied())
                .or_else(|| doc.object(cid))?;
            if !cobj.visible {
                continue;
            }
            if cobj.fill.is_visible() {
                return Some(cobj.fill.clone());
            }
            match &cobj.payload {
                NodePayload::Group(sub) => {
                    if let Some(f) = Self::first_visible_child_fill(doc, sub, sym_ctx) {
                        return Some(f);
                    }
                }
                NodePayload::Clone { .. } => {
                    if let Some((_, f, _)) = doc
                        .resolve_clone_payload_and_style_with_sym(cobj, sym_ctx.map(|(m, _, _)| m))
                    {
                        if f.is_visible() {
                            return Some(f);
                        }
                    }
                }
                NodePayload::SymbolInstance(sym_id) => {
                    if let Some(sym) = doc.symbol(*sym_id) {
                        let mut child_map = HashMap::new();
                        let mut referenced = HashSet::new();
                        for sub in &sym.objects {
                            if sub.id.0 != 0 {
                                child_map.insert(sub.id, sub);
                            }
                            match &sub.payload {
                                NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                                NodePayload::PowerClip { contents, .. } => {
                                    referenced.extend(contents.iter().copied());
                                }
                                _ => {}
                            }
                        }
                        if let Some(f) =
                            Self::first_visible_symbol_fill(doc, sym, &child_map, &referenced)
                        {
                            return Some(f);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    pub(crate) fn compute_children_local_bounds(
        doc: &Document,
        children: &[ObjectId],
        sym_ctx: Option<SymRenderCtx<'_>>,
        rel_xf: Affine2D,
        target_world_xf: Affine2D,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) -> Option<Rect2D> {
        if depth > MAX_RENDER_DEPTH || !rel_xf.is_finite() || !target_world_xf.is_finite() {
            return None;
        }
        let inv_target_world = target_world_xf.inverse();
        let mut acc: Option<Rect2D> = None;
        for &cid in children {
            if !visiting.insert(cid) {
                continue;
            }
            let cobj = sym_ctx
                .and_then(|(m, _, _)| m.get(&cid).copied())
                .or_else(|| doc.object(cid));
            if let Some(raw_obj) = cobj {
                let resolved_clone;
                let obj = if matches!(raw_obj.payload, NodePayload::Clone { .. }) {
                    if let Some((p, f, o)) = doc.resolve_clone_payload_and_style_with_sym(
                        raw_obj,
                        sym_ctx.map(|(m, _, _)| m),
                    ) {
                        let mut c = raw_obj.clone();
                        c.payload = p;
                        c.fill = f;
                        c.outline = o;
                        resolved_clone = c;
                        &resolved_clone
                    } else {
                        raw_obj
                    }
                } else {
                    raw_obj
                };
                let apply_obj_persp = |bp: BezierPath| {
                    let p = if let Some(persp) = obj.perspective {
                        persp.transform_bezier_path(&bp)
                    } else {
                        bp
                    };
                    apply_geometry_effects(&p, &obj.effects)
                };
                for sxf in symmetry_effect_transforms(&obj.effects) {
                    let c_xf = rel_xf.compose(&sxf).compose(&obj.transform);
                    let cb = match &obj.payload {
                        NodePayload::Path(p) => {
                            apply_obj_persp(p.clone()).transformed(&c_xf).bounding_box()
                        }
                        NodePayload::Shape(s) => apply_obj_persp(s.to_bezier_path())
                            .transformed(&c_xf)
                            .bounding_box(),
                        NodePayload::BSpline(bs) => apply_obj_persp(bs.to_bezier_path())
                            .transformed(&c_xf)
                            .bounding_box(),
                        NodePayload::ArtisticMedia { spine, style } => {
                            apply_obj_persp(coree_core::ArtisticMedia::expand_stroke(spine, style))
                                .transformed(&c_xf)
                                .bounding_box()
                        }
                        NodePayload::PowerClip { container, .. } => {
                            apply_obj_persp((**container).clone())
                                .transformed(&c_xf)
                                .bounding_box()
                        }
                        NodePayload::ArtisticText(t) => {
                            apply_obj_persp(BezierPath::from_rect(t.estimated_bounds()))
                                .transformed(&c_xf)
                                .bounding_box()
                        }
                        NodePayload::ParagraphText(t) => {
                            apply_obj_persp(BezierPath::from_rect(t.bounding_box()))
                                .transformed(&c_xf)
                                .bounding_box()
                        }
                        NodePayload::Table(t) => {
                            apply_obj_persp(BezierPath::from_rect(t.to_bounding_box()))
                                .transformed(&c_xf)
                                .bounding_box()
                        }
                        NodePayload::Dimension(d) => {
                            apply_obj_persp(BezierPath::from_rect(d.bounding_box()))
                                .transformed(&c_xf)
                                .bounding_box()
                        }
                        NodePayload::Raster(r) => {
                            let rc = match r.crop_rect {
                                Some(cr) if cr.is_finite() => Some(cr),
                                Some(_) => None,
                                None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                                    Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                                }
                                None => None,
                            };
                            rc.and_then(|rect| {
                                apply_obj_persp(BezierPath::from_rect(rect))
                                    .transformed(&c_xf)
                                    .bounding_box()
                            })
                        }
                        NodePayload::TextOnPath(top) => {
                            resolve_sym_world_path(doc, top.path_id, sym_ctx)
                                .and_then(|wp| {
                                    let inv = target_world_xf
                                        .compose(&rel_xf)
                                        .inverse()
                                        .or(inv_target_world)?;
                                    let sym_local = rel_xf.compose(&sxf);
                                    apply_obj_persp(wp.transformed(&inv))
                                        .transformed(&sym_local)
                                        .bounding_box()
                                })
                                .or_else(|| {
                                    apply_obj_persp(BezierPath::from_rect(
                                        top.text.estimated_bounds(),
                                    ))
                                    .transformed(&c_xf)
                                    .bounding_box()
                                })
                        }
                        NodePayload::Connector(conn) => {
                            let obj_world_xf = target_world_xf.compose(&c_xf);
                            let mut sym_visiting = HashMap::new();
                            let mut sym_visited_syms = HashSet::new();
                            let mut sym_cache = HashMap::new();
                            let ((a, b), _) = resolve_connector_endpoints_with_sym(
                                doc,
                                conn,
                                &obj_world_xf,
                                sym_ctx.map(|(m, _, xf)| (m, xf)),
                                &mut sym_visiting,
                                &mut sym_visited_syms,
                                &mut sym_cache,
                                depth + 1,
                            );
                            inv_target_world.and_then(|inv| {
                                conn.route_path(a, b).transformed(&inv).bounding_box()
                            })
                        }
                        NodePayload::SymbolInstance(sym_id) => doc.symbol_bounds_with_xf_and_space(
                            *sym_id,
                            &c_xf,
                            Some(&target_world_xf),
                        ),
                        NodePayload::Group(sub) => Self::compute_children_local_bounds(
                            doc,
                            sub,
                            sym_ctx,
                            c_xf,
                            target_world_xf,
                            depth + 1,
                            visiting,
                        ),
                        NodePayload::Clone { .. } => None,
                    };
                    if let Some(r) = cb {
                        acc = Some(match acc {
                            Some(prev) => prev.union(r),
                            None => r,
                        });
                    }
                }
            }
            visiting.remove(&cid);
        }
        acc
    }

    fn collect_single_object_local_silhouette(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        sym_ctx: Option<SymRenderCtx<'_>>,
        rel_xf: Affine2D,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) -> BezierPath {
        let mut out = BezierPath::default();
        if depth > MAX_RENDER_DEPTH || !obj.visible || !rel_xf.is_finite() {
            return out;
        }
        if matches!(obj.payload, NodePayload::Clone { .. }) {
            if let Some((p, f, o)) =
                doc.resolve_clone_payload_and_style_with_sym(obj, sym_ctx.map(|(m, _, _)| m))
            {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                return self.collect_single_object_local_silhouette(
                    doc,
                    &tmp,
                    sym_ctx,
                    rel_xf,
                    depth + 1,
                    visiting,
                );
            }
            return out;
        }
        let apply_own_persp = |bp: BezierPath| -> BezierPath {
            if let Some(persp) = obj.perspective {
                persp.transform_bezier_path(&bp)
            } else {
                bp
            }
        };
        for sym_xf in symmetry_effect_transforms(&obj.effects) {
            let c_xf = rel_xf.compose(&sym_xf).compose(&obj.transform);
            match &obj.payload {
                NodePayload::Path(p) => {
                    let def = apply_geometry_effects(&apply_own_persp(p.clone()), &obj.effects)
                        .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::Shape(s) => {
                    let def =
                        apply_geometry_effects(&apply_own_persp(s.to_bezier_path()), &obj.effects)
                            .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::BSpline(bs) => {
                    let def =
                        apply_geometry_effects(&apply_own_persp(bs.to_bezier_path()), &obj.effects)
                            .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::ArtisticMedia { spine, style } => {
                    let def = apply_geometry_effects(
                        &apply_own_persp(coree_core::ArtisticMedia::expand_stroke(spine, style)),
                        &obj.effects,
                    )
                    .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::PowerClip { container, .. } => {
                    let def = apply_geometry_effects(
                        &apply_own_persp((**container).clone()),
                        &obj.effects,
                    )
                    .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::ArtisticText(t) => {
                    let p = self.text_engine.artistic_text_to_curves(t);
                    let def = apply_geometry_effects(&apply_own_persp(p), &obj.effects)
                        .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::ParagraphText(t) => {
                    let p = self.text_engine.paragraph_text_to_curves(t);
                    let def = apply_geometry_effects(&apply_own_persp(p), &obj.effects)
                        .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::Table(t) => {
                    let p = apply_own_persp(BezierPath::from_rect(t.to_bounding_box()));
                    let def = apply_geometry_effects(&p, &obj.effects).transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::Dimension(d) => {
                    let p = apply_own_persp(BezierPath::from_rect(d.bounding_box()));
                    let def = apply_geometry_effects(&p, &obj.effects).transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::Raster(r) => {
                    let rc = r
                        .crop_rect
                        .filter(|cr| cr.is_finite())
                        .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm));
                    let p = apply_own_persp(BezierPath::from_rect(rc)).transformed(&c_xf);
                    out.subpaths.extend(p.subpaths);
                }
                NodePayload::Group(sub) => {
                    let mut sub_sil = self.collect_group_local_silhouette(
                        doc,
                        sub,
                        sym_ctx,
                        Affine2D::IDENTITY,
                        depth + 1,
                        visiting,
                    );
                    sub_sil = apply_own_persp(sub_sil);
                    let sub_bb = sub_sil.bounding_box();
                    let def = apply_geometry_effects_in_bounds(&sub_sil, sub_bb, &obj.effects)
                        .transformed(&c_xf);
                    out.subpaths.extend(def.subpaths);
                }
                NodePayload::SymbolInstance(sym_id) if self.active_symbols.insert(*sym_id) => {
                    if let Some(sym) = doc.symbol(*sym_id) {
                        let mut child_map = HashMap::new();
                        let mut referenced = HashSet::new();
                        for sub in &sym.objects {
                            if sub.id.0 != 0 {
                                child_map.insert(sub.id, sub);
                            }
                            match &sub.payload {
                                NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                                NodePayload::PowerClip { contents, .. } => {
                                    referenced.extend(contents.iter().copied());
                                }
                                _ => {}
                            }
                        }
                        let world_sym = sym_ctx.map_or(Affine2D::IDENTITY, |(_, _, w)| w);
                        let mut sym_sil = self.collect_symbol_local_silhouette(
                            doc,
                            sym,
                            (&child_map, &sym.objects, world_sym),
                            &referenced,
                            Affine2D::IDENTITY,
                            depth + 1,
                        );
                        sym_sil = apply_own_persp(sym_sil);
                        let sym_bb = sym_sil.bounding_box();
                        let def = apply_geometry_effects_in_bounds(&sym_sil, sym_bb, &obj.effects)
                            .transformed(&c_xf);
                        out.subpaths.extend(def.subpaths);
                    }
                    self.active_symbols.remove(sym_id);
                }
                _ => {}
            }
        }
        out
    }

    fn collect_symbol_local_silhouette(
        &mut self,
        doc: &Document,
        sym: &SymbolDef,
        sym_ctx: SymRenderCtx<'_>,
        referenced: &HashSet<ObjectId>,
        rel_xf: Affine2D,
        depth: usize,
    ) -> BezierPath {
        let mut out = BezierPath::default();
        if depth > MAX_RENDER_DEPTH || !rel_xf.is_finite() {
            return out;
        }
        let mut sym_visit = HashSet::new();
        for sub in &sym.objects {
            if sub.id.0 != 0 && referenced.contains(&sub.id) {
                continue;
            }
            if sub.id.0 != 0 && !sym_visit.insert(sub.id) {
                continue;
            }
            let sil = self.collect_single_object_local_silhouette(
                doc,
                sub,
                Some(sym_ctx),
                rel_xf,
                depth + 1,
                &mut sym_visit,
            );
            out.subpaths.extend(sil.subpaths);
            if sub.id.0 != 0 {
                sym_visit.remove(&sub.id);
            }
        }
        out
    }

    fn collect_group_local_silhouette(
        &mut self,
        doc: &Document,
        children: &[ObjectId],
        sym_ctx: Option<SymRenderCtx<'_>>,
        rel_xf: Affine2D,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) -> BezierPath {
        let mut out = BezierPath::default();
        if depth > MAX_RENDER_DEPTH || !rel_xf.is_finite() {
            return out;
        }
        for &cid in children {
            if !visiting.insert(cid) {
                continue;
            }
            let cobj = sym_ctx
                .and_then(|(m, _, _)| m.get(&cid).copied())
                .or_else(|| doc.object(cid));
            if let Some(obj) = cobj {
                let sil = self.collect_single_object_local_silhouette(
                    doc,
                    obj,
                    sym_ctx,
                    rel_xf,
                    depth + 1,
                    visiting,
                );
                out.subpaths.extend(sil.subpaths);
            }
            visiting.remove(&cid);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_deformed_scene_object(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        deform_frames: &[DeformFrame<'_>],
        page_to_surface: Affine2D,
        group_to_screen: Affine2D,
        rel_xf: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        child_u: &[BackdropItem],
        track_child_lens: bool,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) {
        if depth > MAX_RENDER_DEPTH || !rel_xf.is_finite() || deform_frames.is_empty() {
            return;
        }
        let obj_opacity = sanitize_f32(obj.opacity, 1.0).clamp(0.0, 1.0);
        if !obj.visible || obj_opacity <= 1e-6 || !obj.transform.is_finite() {
            return;
        }
        let resolved_clone;
        let obj = if matches!(obj.payload, NodePayload::Clone { .. }) {
            let Some((p, f, o)) =
                doc.resolve_clone_payload_and_style_with_sym(obj, sym_ctx.map(|(m, _, _)| m))
            else {
                return;
            };
            let mut tmp = obj.clone();
            tmp.payload = p;
            tmp.fill = f;
            tmp.outline = o;
            resolved_clone = tmp;
            &resolved_clone
        } else {
            obj
        };
        let is_container = matches!(
            obj.payload,
            NodePayload::Group(_) | NodePayload::PowerClip { .. } | NodePayload::SymbolInstance(_)
        );
        let outer_world_xf = page_to_surface
            .inverse()
            .map(|inv| inv.compose(&group_to_screen))
            .filter(|m| m.is_finite())
            .unwrap_or(Affine2D::IDENTITY);
        let inner_to_outer_rel = deform_frames
            .iter()
            .skip(1)
            .fold(Affine2D::IDENTITY, |acc, f| acc.compose(&f.rel_to_outer));
        let c_parent_world = outer_world_xf.compose(&inner_to_outer_rel).compose(&rel_xf);
        let inv_c_parent_world = c_parent_world.inverse().unwrap_or(Affine2D::IDENTITY);
        for (sym_branch_idx, sym_xf) in symmetry_effect_transforms(&obj.effects)
            .into_iter()
            .enumerate()
        {
            let scope_prev_len = self.frozen_scope_stack.len();
            let own_world_sym_xf = if sym_xf == Affine2D::IDENTITY {
                Affine2D::IDENTITY
            } else {
                let w = c_parent_world.compose(&sym_xf).compose(&inv_c_parent_world);
                if w.is_finite() {
                    w
                } else {
                    sym_xf
                }
            };
            let mut branch_underlying = if sym_branch_idx > 0 {
                self.frozen_scope_stack.extend_from_slice(&[
                    FROZEN_TAG_SYM_BRANCH,
                    obj.id.0,
                    sym_branch_idx as u64,
                ]);
                (track_child_lens && is_container).then(|| {
                    let branch_world_xf = if matches!(obj.payload, NodePayload::SymbolInstance(_)) {
                        None
                    } else {
                        Some(own_world_sym_xf)
                    };
                    let mut v = child_u.to_vec();
                    v.push(BackdropItem::EnterSymmetryBranch {
                        obj_id: obj.id,
                        branch_idx: sym_branch_idx,
                        branch_world_xf,
                    });
                    v
                })
            } else {
                None
            };
            let effective_child_u = branch_underlying.as_deref().unwrap_or(child_u);
            let c_xf = rel_xf.compose(&sym_xf).compose(&obj.transform);
            if c_xf.is_finite() {
                let trans_info = if view_mode != ViewMode::Wireframe {
                    find_transparency_effect(&obj.effects)
                } else {
                    None
                };
                let isolate_child_group = view_mode != ViewMode::Wireframe
                    && trans_info.is_none()
                    && matches!(
                        obj.payload,
                        NodePayload::Group(_) | NodePayload::SymbolInstance(_)
                    )
                    && (obj_opacity < 0.999 || obj.blend_mode != BlendMode::Normal);

                if trans_info.is_some() || isolate_child_group {
                    if let Some(clamped) = self
                        .compute_deformed_child_screen_bounds(
                            doc,
                            obj,
                            deform_frames,
                            page_to_surface,
                            group_to_screen,
                            rel_xf,
                            sym_xf,
                            c_xf,
                            sym_ctx,
                            depth,
                        )
                        .and_then(|sb| {
                            PixelRect::from_f64_rect_clamped(
                                sb,
                                0,
                                backend.width(),
                                backend.height(),
                            )
                        })
                    {
                        let child_to_screen =
                            group_to_screen.compose(&inner_to_outer_rel).compose(&c_xf);
                        let screen_to_local =
                            child_to_screen.inverse().unwrap_or(Affine2D::IDENTITY);
                        let has_lens = {
                            let mut v = if obj.id.0 != 0 {
                                HashSet::from([obj.id])
                            } else {
                                HashSet::new()
                            };
                            let mut vs = HashSet::new();
                            crate::export_render::object_or_subtree_has_lens_with_sym(
                                doc,
                                obj,
                                sym_ctx.map(|(m, _, _)| m),
                                0,
                                &mut v,
                                &mut vs,
                            )
                        };
                        let (comp_op, comp_blend) = match trans_info {
                            Some((_, tb)) => (1.0_f32, tb),
                            None => (obj_opacity, obj.blend_mode),
                        };
                        if let Ok(mut sub) = TinySkiaBackend::new(clamped.width, clamped.height) {
                            sub.clear(Color::TRANSPARENT);
                            let shift =
                                Affine2D::translate(-f64::from(clamped.x), -f64::from(clamped.y));
                            let sub_page = shift.compose(&page_to_surface);
                            let sub_group = shift.compose(&group_to_screen);
                            let prev_force = self.force_offscreen_lens_backdrop;
                            if has_lens {
                                self.force_offscreen_lens_backdrop = true;
                            }
                            self.render_deformed_child_core(
                                doc,
                                page,
                                obj,
                                deform_frames,
                                sub_page,
                                sub_group,
                                rel_xf,
                                sym_xf,
                                c_xf,
                                sym_ctx,
                                px_per_mm,
                                view_mode,
                                options,
                                &mut sub,
                                effective_child_u,
                                track_child_lens,
                                depth,
                                visiting,
                            );
                            self.force_offscreen_lens_backdrop = prev_force;
                            let mut buf = sub.into_rgba();
                            if let Some((trans_mode, _)) = trans_info {
                                apply_transparency_mask(
                                    &mut buf,
                                    clamped,
                                    screen_to_local,
                                    trans_mode,
                                );
                            }
                            backend.composite_region(clamped, &buf, comp_op, comp_blend);
                        } else {
                            let dim_scale = (f64::from(MAX_SURFACE_DIM_PX)
                                / f64::from(clamped.width.max(clamped.height).max(1)))
                            .min(1.0);
                            let px_total =
                                (f64::from(clamped.width) * f64::from(clamped.height)).max(1.0);
                            let px_scale = ((MAX_SURFACE_PIXELS as f64) / px_total).sqrt().min(1.0);
                            let ds = (dim_scale.min(px_scale) * 0.999).clamp(1e-4, 1.0);
                            let buf_w = ((f64::from(clamped.width) * ds).floor() as u32)
                                .clamp(1, MAX_SURFACE_DIM_PX);
                            let buf_h = ((f64::from(clamped.height) * ds).floor() as u32)
                                .clamp(1, MAX_SURFACE_DIM_PX);
                            if let Ok(mut sub) = TinySkiaBackend::new(buf_w, buf_h) {
                                sub.clear(Color::TRANSPARENT);
                                let sx = f64::from(buf_w) / f64::from(clamped.width);
                                let sy = f64::from(buf_h) / f64::from(clamped.height);
                                let shift = Affine2D::scale(sx, sy).compose(&Affine2D::translate(
                                    -f64::from(clamped.x),
                                    -f64::from(clamped.y),
                                ));
                                let sub_page = shift.compose(&page_to_surface);
                                let sub_group = shift.compose(&group_to_screen);
                                let sub_px_per_mm = estimate_scale(&sub_group);
                                let prev_force = self.force_offscreen_lens_backdrop;
                                if has_lens {
                                    self.force_offscreen_lens_backdrop = true;
                                }
                                self.render_deformed_child_core(
                                    doc,
                                    page,
                                    obj,
                                    deform_frames,
                                    sub_page,
                                    sub_group,
                                    rel_xf,
                                    sym_xf,
                                    c_xf,
                                    sym_ctx,
                                    sub_px_per_mm,
                                    view_mode,
                                    options,
                                    &mut sub,
                                    effective_child_u,
                                    track_child_lens,
                                    depth,
                                    visiting,
                                );
                                self.force_offscreen_lens_backdrop = prev_force;
                                let mut buf = sub.into_rgba();
                                if let Some((trans_mode, _)) = trans_info {
                                    let buf_to_local = shift
                                        .compose(&child_to_screen)
                                        .inverse()
                                        .unwrap_or(screen_to_local);
                                    let buf_rect = PixelRect::new(0, 0, buf_w, buf_h);
                                    apply_transparency_mask(
                                        &mut buf,
                                        buf_rect,
                                        buf_to_local,
                                        trans_mode,
                                    );
                                }
                                let draw_xf =
                                    Affine2D::translate(f64::from(clamped.x), f64::from(clamped.y))
                                        .compose(&Affine2D::scale(1.0 / sx, 1.0 / sy));
                                backend.draw_pixmap(&buf, draw_xf, comp_op, comp_blend, true);
                            }
                        }
                    } else if view_mode != ViewMode::Wireframe
                        && self.subtree_has_unwarmed_frozen_lens_in_branch(
                            doc,
                            obj,
                            sym_ctx.map(|(m, _, _)| m),
                            false,
                        )
                    {
                        if let Ok(mut dummy) = TinySkiaBackend::new(1, 1) {
                            let prev_force = self.force_offscreen_lens_backdrop;
                            self.force_offscreen_lens_backdrop = true;
                            self.render_deformed_child_core(
                                doc,
                                page,
                                obj,
                                deform_frames,
                                page_to_surface,
                                group_to_screen,
                                rel_xf,
                                sym_xf,
                                c_xf,
                                sym_ctx,
                                px_per_mm,
                                view_mode,
                                options,
                                &mut dummy,
                                effective_child_u,
                                track_child_lens,
                                depth,
                                visiting,
                            );
                            self.force_offscreen_lens_backdrop = prev_force;
                        }
                    }
                } else {
                    self.render_deformed_child_core(
                        doc,
                        page,
                        obj,
                        deform_frames,
                        page_to_surface,
                        group_to_screen,
                        rel_xf,
                        sym_xf,
                        c_xf,
                        sym_ctx,
                        px_per_mm,
                        view_mode,
                        options,
                        backend,
                        effective_child_u,
                        track_child_lens,
                        depth,
                        visiting,
                    );
                }
            }
            if let Some(ref mut v) = branch_underlying {
                v.push(BackdropItem::ExitSymmetryBranch);
            }
            self.frozen_scope_stack.truncate(scope_prev_len);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_group_deformed_subtree(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        children: &[ObjectId],
        _group_id: ObjectId,
        deform_frames: &[DeformFrame<'_>],
        page_to_surface: Affine2D,
        group_to_screen: Affine2D,
        rel_xf: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying: &[BackdropItem],
        track_child_lens: bool,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) {
        if depth > MAX_RENDER_DEPTH || !rel_xf.is_finite() || deform_frames.is_empty() {
            return;
        }
        let mut child_underlying = track_child_lens.then(|| underlying.to_vec());
        for &cid in children {
            if !visiting.insert(cid) {
                if let Some(ref mut u) = child_underlying {
                    u.push(BackdropItem::Object(cid));
                }
                continue;
            }
            let cobj = sym_ctx
                .and_then(|(m, _, _)| m.get(&cid).copied())
                .or_else(|| doc.object(cid));
            if let Some(obj) = cobj {
                let child_u = child_underlying.as_deref().unwrap_or(underlying);
                self.render_deformed_scene_object(
                    doc,
                    page,
                    obj,
                    deform_frames,
                    page_to_surface,
                    group_to_screen,
                    rel_xf,
                    sym_ctx,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    child_u,
                    track_child_lens,
                    depth,
                    visiting,
                );
            }
            visiting.remove(&cid);
            if let Some(ref mut u) = child_underlying {
                u.push(BackdropItem::Object(cid));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_deformed_child_screen_bounds(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        deform_frames: &[DeformFrame<'_>],
        page_to_surface: Affine2D,
        group_to_screen: Affine2D,
        rel_xf: Affine2D,
        sym_xf: Affine2D,
        c_xf: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        depth: usize,
    ) -> Option<Rect2D> {
        let mut visiting_bounds = HashSet::new();
        let mut visiting_syms = HashSet::new();
        self.compute_deformed_child_screen_bounds_inner(
            doc,
            obj,
            deform_frames,
            page_to_surface,
            group_to_screen,
            rel_xf,
            sym_xf,
            c_xf,
            sym_ctx,
            depth,
            &mut visiting_bounds,
            &mut visiting_syms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_deformed_child_screen_bounds_inner(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        deform_frames: &[DeformFrame<'_>],
        page_to_surface: Affine2D,
        group_to_screen: Affine2D,
        rel_xf: Affine2D,
        sym_xf: Affine2D,
        c_xf: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        depth: usize,
        visiting_bounds: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
    ) -> Option<Rect2D> {
        if depth > MAX_RENDER_DEPTH || !c_xf.is_finite() || !group_to_screen.is_finite() {
            return None;
        }
        let scale = estimate_scale(&group_to_screen).max(estimate_scale(&page_to_surface));
        let inner_to_outer_rel = deform_frames
            .iter()
            .skip(1)
            .fold(Affine2D::IDENTITY, |acc, f| acc.compose(&f.rel_to_outer));
        let inner_to_screen = group_to_screen.compose(&inner_to_outer_rel);
        let obj_to_screen = inner_to_screen.compose(&c_xf);
        let group_world_xf = page_to_surface
            .inverse()
            .map(|inv| inv.compose(&group_to_screen))
            .filter(|m| m.is_finite())
            .unwrap_or(Affine2D::IDENTITY);
        let inner_world_xf = group_world_xf.compose(&inner_to_outer_rel);
        let mut extra_top_pad = 0.0_f64;

        let apply_own_persp = |p: &BezierPath| -> BezierPath {
            if let Some(persp) = obj.perspective {
                persp.transform_bezier_path(p)
            } else {
                p.clone()
            }
        };
        let mut base_sb: Option<Rect2D> = match &obj.payload {
            NodePayload::Path(p) => {
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&apply_own_persp(p), &obj.effects),
                    c_xf,
                    deform_frames,
                );
                deformed.transformed(&group_to_screen).bounding_box()
            }
            NodePayload::Shape(s) => {
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&apply_own_persp(&s.to_bezier_path()), &obj.effects),
                    c_xf,
                    deform_frames,
                );
                deformed.transformed(&group_to_screen).bounding_box()
            }
            NodePayload::BSpline(bs) => {
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&apply_own_persp(&bs.to_bezier_path()), &obj.effects),
                    c_xf,
                    deform_frames,
                );
                deformed.transformed(&group_to_screen).bounding_box()
            }
            NodePayload::ArtisticMedia { spine, style } => {
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(
                        &apply_own_persp(&coree_core::ArtisticMedia::expand_stroke(spine, style)),
                        &obj.effects,
                    ),
                    c_xf,
                    deform_frames,
                );
                deformed.transformed(&group_to_screen).bounding_box()
            }
            NodePayload::Clone { .. } => {
                return doc
                    .resolve_clone_payload_and_style_with_sym(obj, sym_ctx.map(|(m, _, _)| m))
                    .and_then(|(p, f, o)| {
                        let mut tmp = obj.clone();
                        tmp.payload = p;
                        tmp.fill = f;
                        tmp.outline = o;
                        self.compute_deformed_child_screen_bounds_inner(
                            doc,
                            &tmp,
                            deform_frames,
                            page_to_surface,
                            group_to_screen,
                            rel_xf,
                            sym_xf,
                            c_xf,
                            sym_ctx,
                            depth + 1,
                            visiting_bounds,
                            visiting_syms,
                        )
                    });
            }
            NodePayload::PowerClip { container, .. } => {
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&apply_own_persp(container), &obj.effects),
                    c_xf,
                    deform_frames,
                );
                deformed.transformed(&group_to_screen).bounding_box()
            }
            NodePayload::ArtisticText(t) => {
                let mut bb = apply_deform_chain_in_bounds(
                    &apply_own_persp(&BezierPath::from_rect(t.estimated_bounds())),
                    c_xf,
                    deform_frames,
                )
                .transformed(&group_to_screen)
                .bounding_box()
                .or_else(|| obj_to_screen.try_transform_rect(t.estimated_bounds()))?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(t.estimated_bounds().height()),
                    &mut bb,
                );
                Some(bb)
            }
            NodePayload::ParagraphText(t) => {
                let mut bb = apply_deform_chain_in_bounds(
                    &apply_own_persp(&BezierPath::from_rect(t.bounding_box())),
                    c_xf,
                    deform_frames,
                )
                .transformed(&group_to_screen)
                .bounding_box()
                .or_else(|| obj_to_screen.try_transform_rect(t.bounding_box()))?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(t.bounding_box().height()),
                    &mut bb,
                );
                Some(bb)
            }
            NodePayload::Table(t) => {
                let r = t.to_bounding_box();
                let mut bb = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(
                        &apply_own_persp(&BezierPath::from_rect(r)),
                        &obj.effects,
                    ),
                    c_xf,
                    deform_frames,
                )
                .transformed(&group_to_screen)
                .bounding_box()
                .or_else(|| obj_to_screen.try_transform_rect(r))?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(r.height()),
                    &mut bb,
                );
                Some(bb)
            }
            NodePayload::Dimension(d) => {
                let r = d.bounding_box();
                let mut bb = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(
                        &apply_own_persp(&BezierPath::from_rect(r)),
                        &obj.effects,
                    ),
                    c_xf,
                    deform_frames,
                )
                .transformed(&group_to_screen)
                .bounding_box()
                .or_else(|| obj_to_screen.try_transform_rect(r))?;
                Self::expand_screen_bounds_for_deforms(
                    &obj.effects,
                    &obj_to_screen,
                    Some(r.height()),
                    &mut bb,
                );
                Some(bb)
            }
            NodePayload::Raster(r) => {
                let rc = match r.crop_rect {
                    Some(cr) if cr.is_finite() => Some(cr),
                    Some(_) => None,
                    None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                        Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                    }
                    None => None,
                }?;
                apply_deform_chain_in_bounds(
                    &apply_own_persp(&BezierPath::from_rect(rc)),
                    c_xf,
                    deform_frames,
                )
                .transformed(&group_to_screen)
                .bounding_box()
                .or_else(|| obj_to_screen.try_transform_rect(rc))
            }
            NodePayload::TextOnPath(top) => {
                let dist = sanitize_f64(top.distance_mm, 0.0).abs();
                let max_font = top
                    .text
                    .runs
                    .iter()
                    .map(|r| r.style.font_size_mm())
                    .filter(|v| v.is_finite())
                    .fold(4.0_f64, f64::max);
                extra_top_pad = dist + max_font * 1.5;
                if let Some(host_path) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                    let top_parent_world_xf = inner_world_xf.compose(&rel_xf);
                    let inv_top_parent_world =
                        top_parent_world_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let path_in_parent =
                        apply_own_persp(&host_path.transformed(&inv_top_parent_world));
                    let sym_local_xf = rel_xf.compose(&sym_xf);
                    let deformed = apply_deform_chain_in_bounds(
                        &apply_geometry_effects(&path_in_parent, &obj.effects),
                        if sym_local_xf.is_finite() {
                            sym_local_xf
                        } else {
                            sym_xf
                        },
                        deform_frames,
                    );
                    deformed.transformed(&group_to_screen).bounding_box()
                } else {
                    let est = top.text.estimated_bounds();
                    let mut bb = apply_deform_chain_in_bounds(
                        &apply_own_persp(&BezierPath::from_rect(est)),
                        c_xf,
                        deform_frames,
                    )
                    .transformed(&group_to_screen)
                    .bounding_box()
                    .or_else(|| obj_to_screen.try_transform_rect(est))?;
                    Self::expand_screen_bounds_for_deforms(
                        &obj.effects,
                        &obj_to_screen,
                        Some(est.height()),
                        &mut bb,
                    );
                    Some(bb)
                }
            }
            NodePayload::Connector(conn) => {
                let inv_inner_world = inner_world_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                let obj_world_xf = inner_world_xf.compose(&c_xf);
                let mut sym_visiting = HashMap::new();
                let mut sym_visited_syms = HashSet::new();
                let mut sym_cache = HashMap::new();
                let ((a, b), _) = resolve_connector_endpoints_with_sym(
                    doc,
                    conn,
                    &obj_world_xf,
                    sym_ctx.map(|(m, _, xf)| (m, xf)),
                    &mut sym_visiting,
                    &mut sym_visited_syms,
                    &mut sym_cache,
                    depth + 1,
                );
                let route_in_inner = conn.route_path(a, b).transformed(&inv_inner_world);
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&route_in_inner, &obj.effects),
                    Affine2D::IDENTITY,
                    deform_frames,
                );
                deformed.transformed(&group_to_screen).bounding_box()
            }
            NodePayload::Group(sub) => {
                let sub_has_deform = obj.perspective.is_some()
                    || obj.effects.iter().any(|e| {
                        matches!(
                            e,
                            Effect::Distort { .. }
                                | Effect::Envelope { .. }
                                | Effect::Perspective { .. }
                        )
                    });
                let sub_local_bb = if sub_has_deform {
                    let raw_bb = if sym_ctx.is_none() && obj.id.0 != 0 {
                        doc.local_object_bounds(obj.id)
                    } else {
                        let mut v = HashSet::new();
                        Self::compute_children_local_bounds(
                            doc,
                            sub,
                            sym_ctx,
                            Affine2D::IDENTITY,
                            inner_world_xf.compose(&c_xf),
                            depth + 1,
                            &mut v,
                        )
                    };
                    if let Some(persp) = obj.perspective {
                        raw_bb.and_then(|lb| persp.try_transform_rect(lb))
                    } else {
                        raw_bb
                    }
                } else {
                    None
                };
                let mut extended_frames: Vec<DeformFrame<'_>> = Vec::new();
                let (active_frames, child_rel_xf) = if sub_has_deform {
                    extended_frames.extend_from_slice(deform_frames);
                    extended_frames.push(DeformFrame {
                        pre_perspective: obj.perspective,
                        group_local_bb: sub_local_bb,
                        group_effects: &obj.effects,
                        rel_to_outer: c_xf,
                    });
                    (extended_frames.as_slice(), Affine2D::IDENTITY)
                } else {
                    (deform_frames, c_xf)
                };
                let mut acc: Option<Rect2D> = None;
                for &cid in sub {
                    if !visiting_bounds.insert(cid) {
                        continue;
                    }
                    let cobj = sym_ctx
                        .and_then(|(m, _, _)| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    if let Some(child) = cobj {
                        if child.visible && child.transform.is_finite() {
                            for sxf in symmetry_effect_transforms(&child.effects) {
                                let child_c_xf =
                                    child_rel_xf.compose(&sxf).compose(&child.transform);
                                if let Some(cb) = self.compute_deformed_child_screen_bounds_inner(
                                    doc,
                                    child,
                                    active_frames,
                                    page_to_surface,
                                    group_to_screen,
                                    child_rel_xf,
                                    sxf,
                                    child_c_xf,
                                    sym_ctx,
                                    depth + 1,
                                    visiting_bounds,
                                    visiting_syms,
                                ) {
                                    acc = Some(match acc {
                                        Some(prev) => prev.union(cb),
                                        None => cb,
                                    });
                                }
                            }
                        }
                    }
                    visiting_bounds.remove(&cid);
                }
                acc
            }
            NodePayload::SymbolInstance(sym_id) => {
                if !visiting_syms.insert(*sym_id) {
                    None
                } else {
                    let sym_root_world_xf = inner_world_xf.compose(&c_xf);
                    let sub_has_deform = obj.perspective.is_some()
                        || obj.effects.iter().any(|e| {
                            matches!(
                                e,
                                Effect::Distort { .. }
                                    | Effect::Envelope { .. }
                                    | Effect::Perspective { .. }
                            )
                        });
                    let sub_local_bb = if sub_has_deform {
                        let raw_bb = doc.symbol_bounds_with_xf_and_space(
                            *sym_id,
                            &Affine2D::IDENTITY,
                            Some(&sym_root_world_xf),
                        );
                        if let Some(persp) = obj.perspective {
                            raw_bb.and_then(|lb| persp.try_transform_rect(lb))
                        } else {
                            raw_bb
                        }
                    } else {
                        None
                    };
                    let mut extended_frames: Vec<DeformFrame<'_>> = Vec::new();
                    let (active_frames, child_rel_xf) = if sub_has_deform {
                        extended_frames.extend_from_slice(deform_frames);
                        extended_frames.push(DeformFrame {
                            pre_perspective: obj.perspective,
                            group_local_bb: sub_local_bb,
                            group_effects: &obj.effects,
                            rel_to_outer: c_xf,
                        });
                        (extended_frames.as_slice(), Affine2D::IDENTITY)
                    } else {
                        (deform_frames, c_xf)
                    };
                    let res = doc.symbol(*sym_id).and_then(|sym| {
                        let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                        let mut referenced: HashSet<ObjectId> = HashSet::new();
                        for sub in &sym.objects {
                            if sub.id.0 != 0 {
                                child_map.insert(sub.id, sub);
                            }
                            match &sub.payload {
                                NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                                NodePayload::PowerClip { contents, .. } => {
                                    referenced.extend(contents.iter().copied());
                                }
                                _ => {}
                            }
                        }
                        let mut acc: Option<Rect2D> = None;
                        for sub in &sym.objects {
                            if sub.id.0 != 0 && referenced.contains(&sub.id) {
                                continue;
                            }
                            if !sub.visible || !sub.transform.is_finite() {
                                continue;
                            }
                            for sxf in symmetry_effect_transforms(&sub.effects) {
                                let sub_c_xf = child_rel_xf.compose(&sxf).compose(&sub.transform);
                                if let Some(sb) = self.compute_deformed_child_screen_bounds_inner(
                                    doc,
                                    sub,
                                    active_frames,
                                    page_to_surface,
                                    group_to_screen,
                                    child_rel_xf,
                                    sxf,
                                    sub_c_xf,
                                    Some((&child_map, &sym.objects, sym_root_world_xf)),
                                    depth + 1,
                                    visiting_bounds,
                                    visiting_syms,
                                ) {
                                    acc = Some(match acc {
                                        Some(prev) => prev.union(sb),
                                        None => sb,
                                    });
                                }
                            }
                        }
                        acc
                    });
                    visiting_syms.remove(sym_id);
                    res
                }
            }
        };

        for eff in &obj.effects {
            match eff {
                Effect::Blend {
                    target_id,
                    along_path,
                    ..
                } => {
                    for ref_id in std::iter::once(*target_id).chain(along_path.iter().copied()) {
                        if let Some(tbb) = doc.world_object_bounds(ref_id) {
                            if let Some(tsb) = page_to_surface.try_transform_rect(tbb) {
                                base_sb = Some(match base_sb {
                                    Some(prev) => prev.union(tsb),
                                    None => tsb,
                                });
                            }
                        }
                    }
                }
                Effect::Extrude25D {
                    vanishing_point, ..
                } if vanishing_point.is_finite() => {
                    let vp = page_to_surface.transform_point(*vanishing_point);
                    if vp.is_finite() {
                        let vpr = Rect2D::new(vp.x, vp.y, vp.x, vp.y);
                        base_sb = Some(match base_sb {
                            Some(prev) => prev.union(vpr),
                            None => vpr,
                        });
                    }
                }
                _ => {}
            }
        }
        let obj_world_xf = inner_world_xf.compose(&c_xf);
        let stroke_pad_mm = if obj.outline.is_visible() {
            obj.outline
                .scaled_width_mm_for_transform(&obj_world_xf)
                .max(obj.outline.effective_width_mm())
        } else {
            0.0
        };
        let pad_px =
            (effect_bounds_padding_mm(&obj.effects) + stroke_pad_mm + extra_top_pad + 4.0) * scale;
        base_sb.map(|r| r.expand(pad_px.max(4.0)))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_deformed_child_core(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        deform_frames: &[DeformFrame<'_>],
        page_to_surface: Affine2D,
        group_to_screen: Affine2D,
        rel_xf: Affine2D,
        sym_xf: Affine2D,
        c_xf: Affine2D,
        sym_ctx: Option<SymRenderCtx<'_>>,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        child_u: &[BackdropItem],
        track_child_lens: bool,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) {
        if depth > MAX_RENDER_DEPTH {
            return;
        }
        let stripped_cutcontour_obj: Option<Box<SceneObject>> =
            if options.suppress_spot_cutcontour && is_cutcontour_spot(&obj.outline.color) {
                let mut cloned = obj.clone();
                cloned.outline = OutlineStyle::none();
                Some(Box::new(cloned))
            } else {
                None
            };
        let obj = stripped_cutcontour_obj.as_deref().unwrap_or(obj);
        let anti_alias = view_mode != ViewMode::Pixels;
        let inner_to_outer_rel = deform_frames
            .iter()
            .skip(1)
            .fold(Affine2D::IDENTITY, |acc, f| acc.compose(&f.rel_to_outer));

        let apply_own_persp = |p: &BezierPath| -> BezierPath {
            if let Some(persp) = obj.perspective {
                persp.transform_bezier_path(p)
            } else {
                p.clone()
            }
        };
        if view_mode != ViewMode::Wireframe {
            if let Some(lens_cfg) = obj.lens.clone() {
                if let Some(local_path) = self.extract_lens_local_path(doc, obj, sym_ctx) {
                    let deformed = apply_deform_chain_in_bounds(
                        &apply_geometry_effects(&apply_own_persp(&local_path), &obj.effects),
                        c_xf,
                        deform_frames,
                    );
                    if !obj.effects.is_empty() {
                        render_underlay_effects(
                            doc,
                            obj,
                            &deformed,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            page_to_surface,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    render_lens_object_with_backdrop(
                        self,
                        doc,
                        page,
                        obj,
                        &lens_cfg,
                        &deformed,
                        group_to_screen,
                        page_to_surface,
                        view_mode,
                        options,
                        backend,
                        child_u,
                        depth,
                    );
                    if !obj.effects.is_empty() {
                        render_overlay_effects(
                            obj,
                            &deformed,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if obj.outline.is_visible() {
                        draw_outline_styled(
                            &deformed,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    return;
                }
            }
        }

        let apply_own_persp = |p: &BezierPath| -> BezierPath {
            if let Some(persp) = obj.perspective {
                persp.transform_bezier_path(p)
            } else {
                p.clone()
            }
        };
        match &obj.payload {
            NodePayload::Path(p) => {
                let persp_path = apply_own_persp(p);
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&persp_path, &obj.effects),
                    c_xf,
                    deform_frames,
                );
                let mesh_deform = MeshDeformCtx {
                    raw_local_bb: p.bounding_box(),
                    pre_perspective: obj.perspective,
                    pre_effect_bb: persp_path.bounding_box(),
                    effects: &obj.effects,
                    c_xf,
                    deform_frames,
                };
                self.render_deformed_leaf(
                    doc,
                    obj,
                    &deformed,
                    Some(&mesh_deform),
                    page_to_surface,
                    group_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    anti_alias,
                    backend,
                    depth,
                );
            }
            NodePayload::Shape(s) => {
                let raw_path = s.to_bezier_path();
                let persp_path = apply_own_persp(&raw_path);
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&persp_path, &obj.effects),
                    c_xf,
                    deform_frames,
                );
                let mesh_deform = MeshDeformCtx {
                    raw_local_bb: raw_path.bounding_box(),
                    pre_perspective: obj.perspective,
                    pre_effect_bb: persp_path.bounding_box(),
                    effects: &obj.effects,
                    c_xf,
                    deform_frames,
                };
                self.render_deformed_leaf(
                    doc,
                    obj,
                    &deformed,
                    Some(&mesh_deform),
                    page_to_surface,
                    group_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    anti_alias,
                    backend,
                    depth,
                );
            }
            NodePayload::BSpline(bs) => {
                let raw_path = bs.to_bezier_path();
                let persp_path = apply_own_persp(&raw_path);
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&persp_path, &obj.effects),
                    c_xf,
                    deform_frames,
                );
                let mesh_deform = MeshDeformCtx {
                    raw_local_bb: raw_path.bounding_box(),
                    pre_perspective: obj.perspective,
                    pre_effect_bb: persp_path.bounding_box(),
                    effects: &obj.effects,
                    c_xf,
                    deform_frames,
                };
                self.render_deformed_leaf(
                    doc,
                    obj,
                    &deformed,
                    Some(&mesh_deform),
                    page_to_surface,
                    group_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    anti_alias,
                    backend,
                    depth,
                );
            }
            NodePayload::ArtisticMedia { spine, style } => {
                let raw_path = coree_core::ArtisticMedia::expand_stroke(spine, style);
                let persp_path = apply_own_persp(&raw_path);
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&persp_path, &obj.effects),
                    c_xf,
                    deform_frames,
                );
                let mesh_deform = MeshDeformCtx {
                    raw_local_bb: raw_path.bounding_box(),
                    pre_perspective: obj.perspective,
                    pre_effect_bb: persp_path.bounding_box(),
                    effects: &obj.effects,
                    c_xf,
                    deform_frames,
                };
                self.render_deformed_leaf(
                    doc,
                    obj,
                    &deformed,
                    Some(&mesh_deform),
                    page_to_surface,
                    group_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    anti_alias,
                    backend,
                    depth,
                );
            }
            NodePayload::Clone { .. } => {
                if let Some((p, f, o)) =
                    doc.resolve_clone_payload_and_style_with_sym(obj, sym_ctx.map(|(m, _, _)| m))
                {
                    let mut tmp = obj.clone();
                    tmp.payload = p;
                    tmp.fill = f;
                    tmp.outline =
                        if options.suppress_spot_cutcontour && is_cutcontour_spot(&o.color) {
                            OutlineStyle::none()
                        } else {
                            o
                        };
                    self.render_deformed_child_core(
                        doc,
                        page,
                        &tmp,
                        deform_frames,
                        page_to_surface,
                        group_to_screen,
                        rel_xf,
                        sym_xf,
                        c_xf,
                        sym_ctx,
                        px_per_mm,
                        view_mode,
                        options,
                        backend,
                        child_u,
                        track_child_lens,
                        depth + 1,
                        visiting,
                    );
                }
            }
            NodePayload::ArtisticText(text) => {
                let mut pieces = self.text_engine.layout_artistic_pieces(text);
                if let Some(persp) = obj.perspective {
                    for piece in &mut pieces {
                        piece.path = persp.transform_bezier_path(&piece.path);
                    }
                }
                self.render_deformed_text_pieces(
                    doc,
                    obj,
                    &pieces,
                    deform_frames,
                    c_xf,
                    page_to_surface,
                    group_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    anti_alias,
                    backend,
                    depth,
                );
            }
            NodePayload::ParagraphText(para) => {
                let mut pieces = self.text_engine.layout_paragraph_in_doc(doc, obj.id, para);
                if let Some(persp) = obj.perspective {
                    for piece in &mut pieces {
                        piece.path = persp.transform_bezier_path(&piece.path);
                    }
                }
                self.render_deformed_text_pieces(
                    doc,
                    obj,
                    &pieces,
                    deform_frames,
                    c_xf,
                    page_to_surface,
                    group_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    anti_alias,
                    backend,
                    depth,
                );
            }
            NodePayload::TextOnPath(top) => {
                if let Some(host_path) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                    let group_world_xf = page_to_surface
                        .inverse()
                        .map(|inv| inv.compose(&group_to_screen))
                        .filter(|m| m.is_finite())
                        .unwrap_or(Affine2D::IDENTITY);
                    let inner_world_xf = group_world_xf.compose(&inner_to_outer_rel);
                    let top_parent_world_xf = inner_world_xf.compose(&rel_xf);
                    let inv_top_parent_world =
                        top_parent_world_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let mut pieces = self.text_engine.layout_text_on_path_pieces(top, &host_path);
                    for piece in &mut pieces {
                        let local_piece = piece.path.transformed(&inv_top_parent_world);
                        piece.path = if let Some(persp) = obj.perspective {
                            persp.transform_bezier_path(&local_piece)
                        } else {
                            local_piece
                        };
                    }
                    let sym_local_xf = rel_xf.compose(&sym_xf);
                    self.render_deformed_text_pieces(
                        doc,
                        obj,
                        &pieces,
                        deform_frames,
                        if sym_local_xf.is_finite() {
                            sym_local_xf
                        } else {
                            sym_xf
                        },
                        page_to_surface,
                        group_to_screen,
                        px_per_mm,
                        view_mode,
                        options,
                        anti_alias,
                        backend,
                        depth,
                    );
                } else {
                    let mut pieces = self.text_engine.layout_artistic_pieces(&top.text);
                    if let Some(persp) = obj.perspective {
                        for piece in &mut pieces {
                            piece.path = persp.transform_bezier_path(&piece.path);
                        }
                    }
                    self.render_deformed_text_pieces(
                        doc,
                        obj,
                        &pieces,
                        deform_frames,
                        c_xf,
                        page_to_surface,
                        group_to_screen,
                        px_per_mm,
                        view_mode,
                        options,
                        anti_alias,
                        backend,
                        depth,
                    );
                }
            }
            NodePayload::Table(table) => {
                let deform_path = |p: &BezierPath, local_xf: Affine2D| -> BezierPath {
                    apply_deform_chain_in_bounds(
                        &apply_geometry_effects(&apply_own_persp(p), &obj.effects),
                        local_xf,
                        deform_frames,
                    )
                };
                let deformed_grid = deform_path(&table.to_grid_paths(), c_xf);
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&deformed_grid, group_to_screen, px_per_mm, options, backend);
                    return;
                }
                if !obj.effects.is_empty() {
                    render_underlay_effects(
                        doc,
                        obj,
                        &deformed_grid,
                        Affine2D::IDENTITY,
                        group_to_screen,
                        page_to_surface,
                        px_per_mm,
                        anti_alias,
                        &self.active_icc,
                        backend,
                    );
                }
                for r in 0..table.rows {
                    for c in 0..table.cols {
                        let Some(cell) = table.cell(r, c) else {
                            continue;
                        };
                        let Some(rect) = table.cell_rect(r, c) else {
                            continue;
                        };
                        let cell_path = BezierPath::from_rect(rect);
                        let deformed_cell = deform_path(&cell_path, c_xf);
                        if cell.fill.is_visible() {
                            self.draw_fill_styled(
                                doc,
                                &deformed_cell,
                                &cell.fill,
                                group_to_screen,
                                obj.opacity,
                                obj.blend_mode,
                                anti_alias,
                                backend,
                                depth,
                            );
                        } else if obj.fill.is_visible() {
                            self.draw_fill_styled(
                                doc,
                                &deformed_cell,
                                &obj.fill,
                                group_to_screen,
                                obj.opacity,
                                obj.blend_mode,
                                anti_alias,
                                backend,
                                depth,
                            );
                        }
                        if table.border_style.is_visible() {
                            let w = if table.border_style.is_hairline {
                                table
                                    .border_style
                                    .effective_width_mm()
                                    .max(1.0 / px_per_mm.max(1e-3))
                                    as f32
                            } else {
                                table.border_style.effective_width_mm() as f32
                            };
                            let mut st = StrokeParams::from_outline(
                                &table.border_style,
                                w,
                                obj.opacity,
                                obj.blend_mode,
                                anti_alias,
                            );
                            st.color = icc_srgb(&table.border_style.color, &self.active_icc);
                            backend.stroke_path(&deformed_cell, &st, group_to_screen);
                        }
                        if !cell.text.trim().is_empty() {
                            let pad = sanitize_f64(cell.padding_mm, 1.5).max(0.0);
                            let at = ArtisticText::from_plain(
                                cell.text.clone(),
                                cell.text_style.clone(),
                            );
                            let text_pieces = self.text_engine.layout_artistic_pieces(&at);
                            let cell_text_xf = c_xf
                                .compose(&Affine2D::translate(rect.min_x + pad, rect.min_y + pad));
                            backend.push_clip(&deformed_cell, FillRule::Winding, group_to_screen);
                            for piece in &text_pieces {
                                let deformed_glyph = deform_path(&piece.path, cell_text_xf);
                                let fill = Fill::Solid(piece.fill.clone());
                                self.draw_fill_styled(
                                    doc,
                                    &deformed_glyph,
                                    &fill,
                                    group_to_screen,
                                    obj.opacity,
                                    obj.blend_mode,
                                    anti_alias,
                                    backend,
                                    depth,
                                );
                            }
                            backend.pop_clip();
                        }
                    }
                }
                if !obj.effects.is_empty() {
                    render_overlay_effects(
                        obj,
                        &deformed_grid,
                        Affine2D::IDENTITY,
                        group_to_screen,
                        px_per_mm,
                        anti_alias,
                        &self.active_icc,
                        backend,
                    );
                }
            }
            NodePayload::Dimension(dim) => {
                let deform_path = |p: &BezierPath, local_xf: Affine2D| -> BezierPath {
                    apply_deform_chain_in_bounds(
                        &apply_geometry_effects(&apply_own_persp(p), &obj.effects),
                        local_xf,
                        deform_frames,
                    )
                };
                let dim_outline = if obj.outline.is_visible() {
                    icc_outline(&obj.outline, &self.active_icc)
                } else {
                    OutlineStyle::solid(Color::BLACK, 0.25)
                };
                let stroke_w = (dim_outline.effective_width_mm().max(0.2)) as f32;
                let stroke = StrokeParams::from_outline(
                    &dim_outline,
                    stroke_w,
                    obj.opacity,
                    obj.blend_mode,
                    anti_alias,
                );
                let label_text =
                    dim.compute_measurement_with_dpi(doc.default_unit(), f64::from(doc.dpi()));
                let mut stroke_paths = Vec::new();
                let mut arrow_paths = Vec::new();
                let label_anchor = match dim.kind {
                    DimensionKind::Parallel
                    | DimensionKind::Horizontal
                    | DimensionKind::Vertical
                    | DimensionKind::Segment => {
                        let (d1, d2, ext_dir) = match dim.kind {
                            DimensionKind::Horizontal => {
                                let y_line = dim.p1.y.min(dim.p2.y) - dim.offset_mm;
                                (
                                    Point2D::new(dim.p1.x, y_line),
                                    Point2D::new(dim.p2.x, y_line),
                                    Vec2D::new(0.0, -1.0),
                                )
                            }
                            DimensionKind::Vertical => {
                                let x_line = dim.p1.x.min(dim.p2.x) - dim.offset_mm;
                                (
                                    Point2D::new(x_line, dim.p1.y),
                                    Point2D::new(x_line, dim.p2.y),
                                    Vec2D::new(-1.0, 0.0),
                                )
                            }
                            _ => {
                                let dir = (dim.p2 - dim.p1)
                                    .normalized()
                                    .unwrap_or(Vec2D::new(1.0, 0.0));
                                let perp = Vec2D::new(dir.y, -dir.x);
                                let off = perp * dim.offset_mm;
                                (dim.p1 + off, dim.p2 + off, perp)
                            }
                        };
                        let overshoot = ext_dir * 1.5;
                        stroke_paths.push(BezierPath::from_line(dim.p1, d1 + overshoot));
                        stroke_paths.push(BezierPath::from_line(dim.p2, d2 + overshoot));
                        stroke_paths.push(BezierPath::from_line(d1, d2));
                        if let Some(dir) = (d2 - d1).normalized() {
                            if let Some((g, filled)) = arrowhead_geometry(
                                d1,
                                -dir,
                                &ArrowheadStyle::Triangle,
                                stroke_w.max(0.35),
                            ) {
                                arrow_paths.push((g, filled));
                            }
                            if let Some((g, filled)) = arrowhead_geometry(
                                d2,
                                dir,
                                &ArrowheadStyle::Triangle,
                                stroke_w.max(0.35),
                            ) {
                                arrow_paths.push((g, filled));
                            }
                        }
                        d1.midpoint(d2) + ext_dir * 1.2
                    }
                    DimensionKind::Angular => {
                        let v = dim.p1;
                        let r1 = (dim.p2 - v).normalized().unwrap_or(Vec2D::new(1.0, 0.0));
                        let r2 = dim
                            .p3
                            .map_or(Vec2D::new(1.0, 0.0), |p| (p - v).normalize_or_zero());
                        let rad = dim.offset_mm.abs().max(5.0);
                        stroke_paths.push(BezierPath::from_line(v, v + r1 * (rad + 2.0)));
                        stroke_paths.push(BezierPath::from_line(v, v + r2 * (rad + 2.0)));
                        let a1 = r1.angle_rad();
                        let mut a2 = r2.angle_rad();
                        let mut sweep = a2 - a1;
                        while sweep > std::f64::consts::PI {
                            sweep -= std::f64::consts::TAU;
                        }
                        while sweep < -std::f64::consts::PI {
                            sweep += std::f64::consts::TAU;
                        }
                        a2 = a1 + sweep;
                        let steps = 24;
                        let arc_pts: Vec<Point2D> = (0..=steps)
                            .map(|i| {
                                let a = a1 + (a2 - a1) * (i as f64 / steps as f64);
                                v + Vec2D::from_angle(a) * rad
                            })
                            .collect();
                        stroke_paths.push(BezierPath::from_polygon(arc_pts, false));
                        let mid_a = 0.5 * (a1 + a2);
                        v + Vec2D::from_angle(mid_a) * (rad + 3.0)
                    }
                    DimensionKind::Callout3Pt => {
                        let mut pts = vec![dim.p1, dim.p2];
                        let anchor = if let Some(p3) = dim.p3 {
                            pts.push(p3);
                            p3
                        } else {
                            dim.p2
                        };
                        stroke_paths.push(BezierPath::from_polygon(pts, false));
                        anchor
                    }
                };
                for sp in &stroke_paths {
                    let def = deform_path(sp, c_xf);
                    if view_mode == ViewMode::Wireframe {
                        stroke_wireframe(&def, group_to_screen, px_per_mm, options, backend);
                    } else {
                        backend.stroke_path(&def, &stroke, group_to_screen);
                    }
                }
                if view_mode != ViewMode::Wireframe {
                    let arrow_paint = BackendPaint {
                        shader: PaintShader::Solid(dim_outline.color.clone()),
                        opacity: obj.opacity,
                        blend_mode: obj.blend_mode,
                        anti_alias,
                    };
                    for (ap, filled) in &arrow_paths {
                        let def = deform_path(ap, c_xf);
                        if *filled {
                            backend.fill_path(
                                &def,
                                &arrow_paint,
                                FillRule::Winding,
                                group_to_screen,
                            );
                        } else {
                            backend.stroke_path(&def, &stroke, group_to_screen);
                        }
                    }
                }
                if !label_text.trim().is_empty() {
                    let at = ArtisticText::from_plain(label_text, dim.text_style.clone());
                    let pieces = self.text_engine.layout_artistic_pieces(&at);
                    let label_xf = c_xf.compose(&Affine2D::translate(
                        label_anchor.x,
                        label_anchor.y - dim.text_style.font_size_mm(),
                    ));
                    for piece in &pieces {
                        let def = deform_path(&piece.path, label_xf);
                        if view_mode == ViewMode::Wireframe {
                            stroke_wireframe(&def, group_to_screen, px_per_mm, options, backend);
                        } else {
                            let fill = Fill::Solid(piece.fill.clone());
                            self.draw_fill_styled(
                                doc,
                                &def,
                                &fill,
                                group_to_screen,
                                obj.opacity,
                                obj.blend_mode,
                                anti_alias,
                                backend,
                                depth,
                            );
                        }
                    }
                }
            }
            NodePayload::Connector(conn) => {
                let group_world_xf = page_to_surface
                    .inverse()
                    .map(|inv| inv.compose(&group_to_screen))
                    .filter(|m| m.is_finite())
                    .unwrap_or(Affine2D::IDENTITY);
                let inner_world_xf = group_world_xf.compose(&inner_to_outer_rel);
                let inv_inner_world = inner_world_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                let obj_world_xf = inner_world_xf.compose(&c_xf);
                let mut sym_visiting = HashMap::new();
                let mut sym_visited_syms = HashSet::new();
                let mut sym_cache = HashMap::new();
                let ((a, b), _) = resolve_connector_endpoints_with_sym(
                    doc,
                    conn,
                    &obj_world_xf,
                    sym_ctx.map(|(m, _, xf)| (m, xf)),
                    &mut sym_visiting,
                    &mut sym_visited_syms,
                    &mut sym_cache,
                    depth + 1,
                );
                let route = conn.route_path(a, b);
                let route_in_inner = route.transformed(&inv_inner_world);
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&route_in_inner, &obj.effects),
                    Affine2D::IDENTITY,
                    deform_frames,
                );
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&deformed, group_to_screen, px_per_mm, options, backend);
                } else {
                    if !obj.effects.is_empty() {
                        render_underlay_effects(
                            doc,
                            obj,
                            &deformed,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            page_to_surface,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if obj.outline.is_visible() {
                        draw_outline_styled(
                            &deformed,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if !obj.effects.is_empty() {
                        render_overlay_effects(
                            obj,
                            &deformed,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                }
            }
            NodePayload::Group(sub) => {
                let sub_has_deform = obj.perspective.is_some()
                    || obj.effects.iter().any(|e| {
                        matches!(
                            e,
                            Effect::Distort { .. }
                                | Effect::Envelope { .. }
                                | Effect::Perspective { .. }
                        )
                    });
                let outer_world_xf = page_to_surface
                    .inverse()
                    .map(|inv| inv.compose(&group_to_screen))
                    .filter(|m| m.is_finite())
                    .unwrap_or(Affine2D::IDENTITY);
                let sub_world_xf = outer_world_xf.compose(&inner_to_outer_rel).compose(&c_xf);
                let sub_local_bb = if sub_has_deform {
                    let raw_bb = if sym_ctx.is_none() && obj.id.0 != 0 {
                        doc.local_object_bounds(obj.id)
                    } else {
                        let mut v = HashSet::new();
                        Self::compute_children_local_bounds(
                            doc,
                            sub,
                            sym_ctx,
                            Affine2D::IDENTITY,
                            sub_world_xf,
                            depth + 1,
                            &mut v,
                        )
                    };
                    if let Some(persp) = obj.perspective {
                        raw_bb.and_then(|lb| persp.try_transform_rect(lb))
                    } else {
                        raw_bb
                    }
                } else {
                    None
                };
                let mut extended_frames: Vec<DeformFrame<'_>> = Vec::new();
                let (next_frames, next_rel_xf) = if sub_has_deform {
                    extended_frames.extend_from_slice(deform_frames);
                    extended_frames.push(DeformFrame {
                        pre_perspective: obj.perspective,
                        group_local_bb: sub_local_bb,
                        group_effects: &obj.effects,
                        rel_to_outer: c_xf,
                    });
                    (extended_frames.as_slice(), Affine2D::IDENTITY)
                } else {
                    (deform_frames, c_xf)
                };
                let child_has_underlay = view_mode != ViewMode::Wireframe
                    && obj.effects.iter().any(|e| {
                        matches!(
                            e,
                            Effect::DropShadow { .. }
                                | Effect::BlockShadow { .. }
                                | Effect::Contour {
                                    mode: coree_core::ContourMode::Outside,
                                    ..
                                }
                                | Effect::Extrude25D { .. }
                                | Effect::Extrude3D { .. }
                                | Effect::Blend { .. }
                        )
                    });
                let child_has_overlay = view_mode != ViewMode::Wireframe
                    && obj.effects.iter().any(|e| {
                        matches!(
                            e,
                            Effect::Contour {
                                mode: coree_core::ContourMode::Inside
                                    | coree_core::ContourMode::ToCenter,
                                ..
                            } | Effect::Extrude25D { .. }
                                | Effect::Extrude3D { .. }
                                | Effect::InnerShadow { .. }
                                | Effect::Bevel { .. }
                        )
                    });
                let mut child_proxy: Option<SceneObject> = None;
                let mut child_sil: Option<BezierPath> = None;
                if child_has_underlay || child_has_overlay {
                    let mut v = HashSet::new();
                    let sil = self.collect_group_local_silhouette(
                        doc,
                        sub,
                        sym_ctx,
                        Affine2D::IDENTITY,
                        depth + 1,
                        &mut v,
                    );
                    let def_sil = apply_deform_chain_in_bounds(&sil, next_rel_xf, next_frames);
                    let mut proxy = obj.clone();
                    if !proxy.fill.is_visible() {
                        proxy.fill = Self::first_visible_child_fill(doc, sub, sym_ctx)
                            .unwrap_or(Fill::Solid(Color::BLACK));
                    }
                    child_sil = Some(def_sil);
                    child_proxy = Some(proxy);
                }
                if child_has_underlay {
                    if let (Some(proxy), Some(def_sil)) = (&child_proxy, &child_sil) {
                        render_underlay_effects(
                            doc,
                            proxy,
                            def_sil,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            page_to_surface,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                }
                let group_sym_root_idx = if obj.id.0 == 0 {
                    self.current_sym_root_idx
                } else {
                    None
                };
                let saved_sym_root_idx = self.current_sym_root_idx.take();
                let mut sub_underlying = (sub_has_deform && track_child_lens).then(|| {
                    let mut v = child_u.to_vec();
                    v.push(BackdropItem::EnterDeformedGroup {
                        group_id: obj.id,
                        group_sym_root_idx,
                        group_local_bb: sub_local_bb,
                        group_world_xf: Some(sub_world_xf),
                    });
                    v
                });
                let sub_u = sub_underlying.as_deref().unwrap_or(child_u);
                self.render_group_deformed_subtree(
                    doc,
                    page,
                    sub,
                    obj.id,
                    next_frames,
                    page_to_surface,
                    group_to_screen,
                    next_rel_xf,
                    sym_ctx,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    sub_u,
                    track_child_lens,
                    depth + 1,
                    visiting,
                );
                self.current_sym_root_idx = saved_sym_root_idx;
                if let Some(ref mut v) = sub_underlying {
                    v.push(BackdropItem::ExitDeformedGroup);
                }
                if child_has_overlay {
                    if let (Some(proxy), Some(def_sil)) = (&child_proxy, &child_sil) {
                        render_overlay_effects(
                            proxy,
                            def_sil,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                let pc_has_deform = obj.perspective.is_some()
                    || obj.effects.iter().any(|e| {
                        matches!(
                            e,
                            Effect::Distort { .. }
                                | Effect::Envelope { .. }
                                | Effect::Perspective { .. }
                        )
                    });
                let persp_container = apply_own_persp(container);
                let pc_local_bb = pc_has_deform
                    .then(|| persp_container.bounding_box())
                    .flatten();
                let deformed_container = apply_deform_chain_in_bounds(
                    &apply_geometry_effects_in_bounds(&persp_container, pc_local_bb, &obj.effects),
                    c_xf,
                    deform_frames,
                );
                let mut extended_frames: Vec<DeformFrame<'_>> = Vec::new();
                let (next_frames, next_rel_xf) = if pc_has_deform {
                    extended_frames.extend_from_slice(deform_frames);
                    extended_frames.push(DeformFrame {
                        pre_perspective: obj.perspective,
                        group_local_bb: pc_local_bb,
                        group_effects: &obj.effects,
                        rel_to_outer: c_xf,
                    });
                    (extended_frames.as_slice(), Affine2D::IDENTITY)
                } else {
                    (deform_frames, c_xf)
                };
                let sym_map = sym_ctx.map(|(m, _, _)| m);
                let track_pc_lens = track_child_lens
                    && contents.iter().any(|&cid| {
                        sym_map
                            .and_then(|m| m.get(&cid).copied())
                            .or_else(|| doc.object(cid))
                            .is_some_and(|c| {
                                let mut v = HashSet::from([cid]);
                                let mut vs = HashSet::new();
                                crate::export_render::object_or_subtree_has_lens_with_sym(
                                    doc, c, sym_map, 0, &mut v, &mut vs,
                                )
                            })
                    });
                let pc_sym_root_idx = if obj.id.0 == 0 {
                    self.current_sym_root_idx
                } else {
                    None
                };
                let saved_sym_root_idx = self.current_sym_root_idx.take();
                let outer_world_xf = page_to_surface
                    .inverse()
                    .map(|inv| inv.compose(&group_to_screen))
                    .filter(|m| m.is_finite())
                    .unwrap_or(Affine2D::IDENTITY);
                let pc_world_xf = outer_world_xf.compose(&inner_to_outer_rel).compose(&c_xf);
                let mut pc_underlying = track_pc_lens.then(|| {
                    let mut v = child_u.to_vec();
                    v.push(BackdropItem::EnterPowerClip {
                        pc_id: obj.id,
                        sym_root_idx: pc_sym_root_idx,
                    });
                    if pc_has_deform {
                        v.push(BackdropItem::EnterDeformedGroup {
                            group_id: obj.id,
                            group_sym_root_idx: pc_sym_root_idx,
                            group_local_bb: pc_local_bb,
                            group_world_xf: Some(pc_world_xf),
                        });
                    }
                    v
                });
                let pc_u = pc_underlying.as_deref().unwrap_or(child_u);
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(
                        &deformed_container,
                        group_to_screen,
                        px_per_mm,
                        options,
                        backend,
                    );
                    self.render_group_deformed_subtree(
                        doc,
                        page,
                        contents,
                        obj.id,
                        next_frames,
                        page_to_surface,
                        group_to_screen,
                        next_rel_xf,
                        sym_ctx,
                        px_per_mm,
                        view_mode,
                        options,
                        backend,
                        pc_u,
                        track_pc_lens,
                        depth + 1,
                        visiting,
                    );
                    self.current_sym_root_idx = saved_sym_root_idx;
                } else {
                    if !obj.effects.is_empty() {
                        render_underlay_effects(
                            doc,
                            obj,
                            &deformed_container,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            page_to_surface,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if obj.outline.behind_fill && obj.outline.is_visible() {
                        draw_outline_styled(
                            &deformed_container,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if obj.fill.is_visible() {
                        let mesh_deform = Some(MeshDeformCtx {
                            raw_local_bb: container.bounding_box(),
                            pre_perspective: obj.perspective,
                            pre_effect_bb: pc_local_bb,
                            effects: &obj.effects,
                            c_xf,
                            deform_frames,
                        });
                        self.draw_fill_styled_with_deform(
                            doc,
                            &deformed_container,
                            &obj.fill,
                            group_to_screen,
                            obj.opacity,
                            obj.blend_mode,
                            anti_alias,
                            backend,
                            depth,
                            mesh_deform.as_ref(),
                        );
                    }
                    backend.push_clip(&deformed_container, FillRule::Winding, group_to_screen);
                    self.render_group_deformed_subtree(
                        doc,
                        page,
                        contents,
                        obj.id,
                        next_frames,
                        page_to_surface,
                        group_to_screen,
                        next_rel_xf,
                        sym_ctx,
                        px_per_mm,
                        view_mode,
                        options,
                        backend,
                        pc_u,
                        track_pc_lens,
                        depth + 1,
                        visiting,
                    );
                    self.current_sym_root_idx = saved_sym_root_idx;
                    if let Some(ref mut v) = pc_underlying {
                        if pc_has_deform {
                            v.push(BackdropItem::ExitDeformedGroup);
                        }
                    }
                    backend.pop_clip();
                    if !obj.effects.is_empty() {
                        render_overlay_effects(
                            obj,
                            &deformed_container,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                    if !obj.outline.behind_fill && obj.outline.is_visible() {
                        draw_outline_styled(
                            &deformed_container,
                            Affine2D::IDENTITY,
                            group_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            &self.active_icc,
                            backend,
                        );
                    }
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if self.active_symbols.insert(*sym_id) {
                    if let Some(sym) = doc.symbol(*sym_id) {
                        let sym_root_world_xf = page_to_surface
                            .inverse()
                            .map(|inv| {
                                inv.compose(&group_to_screen)
                                    .compose(&inner_to_outer_rel)
                                    .compose(&c_xf)
                            })
                            .filter(|m| m.is_finite())
                            .unwrap_or(c_xf);
                        let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                        let mut referenced: HashSet<ObjectId> = HashSet::new();
                        for sub in &sym.objects {
                            if sub.id.0 != 0 {
                                child_map.insert(sub.id, sub);
                            }
                            match &sub.payload {
                                NodePayload::Group(c) => {
                                    referenced.extend(c.iter().copied());
                                }
                                NodePayload::PowerClip { contents, .. } => {
                                    referenced.extend(contents.iter().copied());
                                }
                                _ => {}
                            }
                        }
                        let sub_has_deform = obj.perspective.is_some()
                            || obj.effects.iter().any(|e| {
                                matches!(
                                    e,
                                    Effect::Distort { .. }
                                        | Effect::Envelope { .. }
                                        | Effect::Perspective { .. }
                                )
                            });
                        let sub_local_bb = if sub_has_deform {
                            let raw_bb = doc.symbol_bounds_with_xf_and_space(
                                *sym_id,
                                &Affine2D::IDENTITY,
                                Some(&sym_root_world_xf),
                            );
                            if let Some(persp) = obj.perspective {
                                raw_bb.and_then(|lb| persp.try_transform_rect(lb))
                            } else {
                                raw_bb
                            }
                        } else {
                            None
                        };
                        let mut extended_frames: Vec<DeformFrame<'_>> = Vec::new();
                        let (next_frames, next_rel_xf) = if sub_has_deform {
                            extended_frames.extend_from_slice(deform_frames);
                            extended_frames.push(DeformFrame {
                                pre_perspective: obj.perspective,
                                group_local_bb: sub_local_bb,
                                group_effects: &obj.effects,
                                rel_to_outer: c_xf,
                            });
                            (extended_frames.as_slice(), Affine2D::IDENTITY)
                        } else {
                            (deform_frames, c_xf)
                        };
                        let child_has_underlay = view_mode != ViewMode::Wireframe
                            && obj.effects.iter().any(|e| {
                                matches!(
                                    e,
                                    Effect::DropShadow { .. }
                                        | Effect::BlockShadow { .. }
                                        | Effect::Contour {
                                            mode: coree_core::ContourMode::Outside,
                                            ..
                                        }
                                        | Effect::Extrude25D { .. }
                                        | Effect::Extrude3D { .. }
                                        | Effect::Blend { .. }
                                )
                            });
                        let child_has_overlay = view_mode != ViewMode::Wireframe
                            && obj.effects.iter().any(|e| {
                                matches!(
                                    e,
                                    Effect::Contour {
                                        mode: coree_core::ContourMode::Inside
                                            | coree_core::ContourMode::ToCenter,
                                        ..
                                    } | Effect::Extrude25D { .. }
                                        | Effect::Extrude3D { .. }
                                        | Effect::InnerShadow { .. }
                                        | Effect::Bevel { .. }
                                )
                            });
                        let mut child_proxy: Option<SceneObject> = None;
                        let mut child_sil: Option<BezierPath> = None;
                        if child_has_underlay || child_has_overlay {
                            let sil = self.collect_symbol_local_silhouette(
                                doc,
                                sym,
                                (&child_map, &sym.objects, sym_root_world_xf),
                                &referenced,
                                Affine2D::IDENTITY,
                                depth + 1,
                            );
                            let def_sil =
                                apply_deform_chain_in_bounds(&sil, next_rel_xf, next_frames);
                            let mut proxy = obj.clone();
                            if !proxy.fill.is_visible() {
                                proxy.fill = Self::first_visible_symbol_fill(
                                    doc,
                                    sym,
                                    &child_map,
                                    &referenced,
                                )
                                .unwrap_or(Fill::Solid(Color::BLACK));
                            }
                            child_sil = Some(def_sil);
                            child_proxy = Some(proxy);
                        }
                        if child_has_underlay {
                            if let (Some(proxy), Some(def_sil)) = (&child_proxy, &child_sil) {
                                render_underlay_effects(
                                    doc,
                                    proxy,
                                    def_sil,
                                    Affine2D::IDENTITY,
                                    group_to_screen,
                                    page_to_surface,
                                    px_per_mm,
                                    anti_alias,
                                    &self.active_icc,
                                    backend,
                                );
                            }
                        }
                        let sym_has_lens = track_child_lens
                            && sym.objects.iter().any(|sub| {
                                if sub.id.0 != 0 && referenced.contains(&sub.id) {
                                    return false;
                                }
                                let mut v = if sub.id.0 != 0 {
                                    HashSet::from([sub.id])
                                } else {
                                    HashSet::new()
                                };
                                let mut vs = HashSet::new();
                                crate::export_render::object_or_subtree_has_lens_with_sym(
                                    doc,
                                    sub,
                                    Some(&child_map),
                                    0,
                                    &mut v,
                                    &mut vs,
                                )
                            });
                        let parent_sym_root_idx = self.current_sym_root_idx.take();
                        let scope_prev_len = self.frozen_scope_stack.len();
                        let sym_u128 = sym_id.0.as_u128();
                        self.frozen_scope_stack.extend_from_slice(&[
                            FROZEN_TAG_SYM_INST,
                            sym_u128 as u64,
                            (sym_u128 >> 64) as u64,
                            obj.id.0,
                            parent_sym_root_idx.map_or(u64::MAX, |i| i as u64),
                        ]);
                        let mut sym_underlying = sym_has_lens.then(|| {
                            let mut v = child_u.to_vec();
                            v.push(BackdropItem::EnterSymbolInstance {
                                sym_id: *sym_id,
                                sym_world_xf: sym_root_world_xf,
                                inst_id: obj.id,
                                inst_sym_root_idx: parent_sym_root_idx,
                            });
                            v
                        });
                        let mut sym_visit = HashSet::new();
                        for (idx, sub) in sym.objects.iter().enumerate() {
                            if sub.id.0 != 0 && referenced.contains(&sub.id) {
                                continue;
                            }
                            if sub.id.0 != 0 && !sym_visit.insert(sub.id) {
                                if let Some(ref mut u) = sym_underlying {
                                    u.push(BackdropItem::Object(sub.id));
                                }
                                continue;
                            }
                            self.current_sym_root_idx =
                                if sub.id.0 == 0 { Some(idx) } else { None };
                            let sub_u = sym_underlying.as_deref().unwrap_or(child_u);
                            self.render_deformed_scene_object(
                                doc,
                                page,
                                sub,
                                next_frames,
                                page_to_surface,
                                group_to_screen,
                                next_rel_xf,
                                Some((&child_map, &sym.objects, sym_root_world_xf)),
                                px_per_mm,
                                view_mode,
                                options,
                                backend,
                                sub_u,
                                sym_has_lens,
                                depth + 1,
                                &mut sym_visit,
                            );
                            if sub.id.0 != 0 {
                                sym_visit.remove(&sub.id);
                            }
                            if let Some(ref mut u) = sym_underlying {
                                if sub.id.0 != 0 {
                                    u.push(BackdropItem::Object(sub.id));
                                } else {
                                    u.push(BackdropItem::SymObjectIndex(idx));
                                }
                            }
                        }
                        if let Some(ref mut u) = sym_underlying {
                            u.push(BackdropItem::ExitSymbolInstance);
                        }
                        self.frozen_scope_stack.truncate(scope_prev_len);
                        self.current_sym_root_idx = parent_sym_root_idx;
                        if child_has_overlay {
                            if let (Some(proxy), Some(def_sil)) = (&child_proxy, &child_sil) {
                                render_overlay_effects(
                                    proxy,
                                    def_sil,
                                    Affine2D::IDENTITY,
                                    group_to_screen,
                                    px_per_mm,
                                    anti_alias,
                                    &self.active_icc,
                                    backend,
                                );
                            }
                        }
                    }
                    self.active_symbols.remove(sym_id);
                }
            }
            NodePayload::Raster(_) => {
                self.render_scene_object_core(
                    doc,
                    page,
                    obj,
                    page_to_surface,
                    group_to_screen
                        .compose(&inner_to_outer_rel)
                        .compose(&rel_xf)
                        .compose(&sym_xf),
                    sym_ctx,
                    Affine2D::IDENTITY,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    child_u,
                    depth + 1,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_deformed_leaf(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        deformed_in_group: &BezierPath,
        mesh_deform: Option<&MeshDeformCtx<'_>>,
        page_to_surface: Affine2D,
        group_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        anti_alias: bool,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        if view_mode == ViewMode::Wireframe {
            stroke_wireframe(
                deformed_in_group,
                group_to_screen,
                px_per_mm,
                options,
                backend,
            );
            return;
        }
        if !obj.effects.is_empty() {
            render_underlay_effects(
                doc,
                obj,
                deformed_in_group,
                Affine2D::IDENTITY,
                group_to_screen,
                page_to_surface,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }
        if obj.outline.behind_fill && obj.outline.is_visible() {
            draw_outline_styled(
                deformed_in_group,
                Affine2D::IDENTITY,
                group_to_screen,
                &obj.outline,
                obj.opacity,
                obj.blend_mode,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }
        if obj.fill.is_visible() {
            self.draw_fill_styled_with_deform(
                doc,
                deformed_in_group,
                &obj.fill,
                group_to_screen,
                obj.opacity,
                obj.blend_mode,
                anti_alias,
                backend,
                depth,
                mesh_deform,
            );
        }
        if !obj.effects.is_empty() {
            render_overlay_effects(
                obj,
                deformed_in_group,
                Affine2D::IDENTITY,
                group_to_screen,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }
        if !obj.outline.behind_fill && obj.outline.is_visible() {
            draw_outline_styled(
                deformed_in_group,
                Affine2D::IDENTITY,
                group_to_screen,
                &obj.outline,
                obj.opacity,
                obj.blend_mode,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_deformed_text_pieces(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        pieces: &[ShapedGlyphDraw],
        deform_frames: &[DeformFrame<'_>],
        c_xf: Affine2D,
        page_to_surface: Affine2D,
        group_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        anti_alias: bool,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        let deformed_pieces: Vec<(&ShapedGlyphDraw, BezierPath)> = pieces
            .iter()
            .map(|piece| {
                let deformed = apply_deform_chain_in_bounds(
                    &apply_geometry_effects(&piece.path, &obj.effects),
                    c_xf,
                    deform_frames,
                );
                (piece, deformed)
            })
            .collect();
        if view_mode == ViewMode::Wireframe {
            for (_, deformed) in &deformed_pieces {
                stroke_wireframe(deformed, group_to_screen, px_per_mm, options, backend);
            }
            return;
        }
        let combined_deformed = if !obj.effects.is_empty() {
            Some(BezierPath {
                subpaths: deformed_pieces
                    .iter()
                    .flat_map(|(_, d)| d.subpaths.iter().cloned())
                    .collect(),
            })
        } else {
            None
        };
        if let Some(ref combined) = combined_deformed {
            render_underlay_effects(
                doc,
                obj,
                combined,
                Affine2D::IDENTITY,
                group_to_screen,
                page_to_surface,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }
        for (piece, deformed) in &deformed_pieces {
            let fill = Fill::Solid(piece.fill.clone());
            self.draw_fill_styled(
                doc,
                deformed,
                &fill,
                group_to_screen,
                obj.opacity,
                obj.blend_mode,
                anti_alias,
                backend,
                depth,
            );
            if obj.outline.is_visible() {
                draw_outline_styled(
                    deformed,
                    Affine2D::IDENTITY,
                    group_to_screen,
                    &obj.outline,
                    obj.opacity,
                    obj.blend_mode,
                    px_per_mm,
                    anti_alias,
                    &self.active_icc,
                    backend,
                );
            }
        }
        if let Some(ref combined) = combined_deformed {
            render_overlay_effects(
                obj,
                combined,
                Affine2D::IDENTITY,
                group_to_screen,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_vector_geometry(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        local_path: &BezierPath,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        let persp_path = if let Some(persp) = obj.perspective {
            persp.transform_bezier_path(local_path)
        } else {
            local_path.clone()
        };
        let deformed = apply_geometry_effects(&persp_path, &obj.effects);
        let obj_to_screen = parent_to_screen.compose(&obj.transform);
        let anti_alias = view_mode != ViewMode::Pixels;

        if view_mode == ViewMode::Wireframe {
            stroke_wireframe(&deformed, obj_to_screen, px_per_mm, options, backend);
            return;
        }

        // Underlay live effects (DropShadow, BlockShadow, Contour::Outside, Extrude25D, Blend)
        if !obj.effects.is_empty() {
            render_underlay_effects(
                doc,
                obj,
                &deformed,
                obj.transform,
                parent_to_screen,
                page_to_surface,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }

        // Behind-fill outline
        if obj.outline.behind_fill && obj.outline.is_visible() {
            draw_outline_styled(
                &deformed,
                obj.transform,
                parent_to_screen,
                &obj.outline,
                obj.opacity,
                obj.blend_mode,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }

        // Fill (all 11 Fill variants!)
        if obj.fill.is_visible() {
            let has_deform = obj.perspective.is_some()
                || obj.effects.iter().any(|e| {
                    matches!(
                        e,
                        Effect::Distort { .. }
                            | Effect::Envelope { .. }
                            | Effect::Perspective { .. }
                    )
                });
            let mesh_deform = has_deform.then(|| MeshDeformCtx {
                raw_local_bb: local_path.bounding_box(),
                pre_perspective: obj.perspective,
                pre_effect_bb: persp_path.bounding_box(),
                effects: &obj.effects,
                c_xf: Affine2D::IDENTITY,
                deform_frames: &[],
            });
            self.draw_fill_styled_with_deform(
                doc,
                &deformed,
                &obj.fill,
                obj_to_screen,
                obj.opacity,
                obj.blend_mode,
                anti_alias,
                backend,
                depth,
                mesh_deform.as_ref(),
            );
        }

        // Overlay effects (Contour::Inside / ToCenter)
        if !obj.effects.is_empty() {
            render_overlay_effects(
                obj,
                &deformed,
                obj.transform,
                parent_to_screen,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }

        // Foreground outline
        if !obj.outline.behind_fill && obj.outline.is_visible() {
            draw_outline_styled(
                &deformed,
                obj.transform,
                parent_to_screen,
                &obj.outline,
                obj.opacity,
                obj.blend_mode,
                px_per_mm,
                anti_alias,
                &self.active_icc,
                backend,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_table_object(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        table: &VectorTable,
        obj_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        let anti_alias = view_mode != ViewMode::Pixels;
        if view_mode == ViewMode::Wireframe {
            stroke_wireframe(
                &table.to_grid_paths(),
                obj_to_screen,
                px_per_mm,
                options,
                backend,
            );
            return;
        }

        // Render cell backgrounds, borders, and cell text
        for r in 0..table.rows {
            for c in 0..table.cols {
                let Some(cell) = table.cell(r, c) else {
                    continue;
                };
                let Some(rect) = table.cell_rect(r, c) else {
                    continue;
                };
                let cell_path = BezierPath::from_rect(rect);
                if cell.fill.is_visible() {
                    self.draw_fill_styled(
                        doc,
                        &cell_path,
                        &cell.fill,
                        obj_to_screen,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                        backend,
                        depth,
                    );
                } else if obj.fill.is_visible() {
                    self.draw_fill_styled(
                        doc,
                        &cell_path,
                        &obj.fill,
                        obj_to_screen,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                        backend,
                        depth,
                    );
                }

                if table.border_style.is_visible() {
                    let w = if table.border_style.is_hairline {
                        table
                            .border_style
                            .effective_width_mm()
                            .max(1.0 / px_per_mm.max(1e-3)) as f32
                    } else {
                        table.border_style.effective_width_mm() as f32
                    };
                    let mut st = StrokeParams::from_outline(
                        &table.border_style,
                        w,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                    );
                    st.color = icc_srgb(&table.border_style.color, &self.active_icc);
                    backend.stroke_path(&cell_path, &st, obj_to_screen);
                }

                if !cell.text.trim().is_empty() {
                    let pad = sanitize_f64(cell.padding_mm, 1.5).max(0.0);
                    let text_xf = obj_to_screen
                        .compose(&Affine2D::translate(rect.min_x + pad, rect.min_y + pad));
                    let at = ArtisticText::from_plain(cell.text.clone(), cell.text_style.clone());
                    backend.push_clip(&cell_path, FillRule::Winding, obj_to_screen);
                    self.text_engine.render_artistic_text_icc(
                        backend,
                        &at,
                        text_xf,
                        &OutlineStyle::none(),
                        obj.opacity,
                        obj.blend_mode,
                        view_mode,
                        px_per_mm,
                        &self.active_icc,
                    );
                    backend.pop_clip();
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_dimension_object(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        dim: &DimensionObject,
        obj_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
    ) {
        let anti_alias = view_mode != ViewMode::Pixels;
        let dim_outline = if obj.outline.is_visible() {
            icc_outline(&obj.outline, &self.active_icc)
        } else {
            OutlineStyle::solid(Color::BLACK, 0.25)
        };
        let stroke_w = (dim_outline.effective_width_mm().max(0.2)) as f32;
        let stroke = StrokeParams::from_outline(
            &dim_outline,
            stroke_w,
            obj.opacity,
            obj.blend_mode,
            anti_alias,
        );

        let label_text = dim.compute_measurement_with_dpi(doc.default_unit(), f64::from(doc.dpi()));
        let label_anchor = match dim.kind {
            DimensionKind::Parallel
            | DimensionKind::Horizontal
            | DimensionKind::Vertical
            | DimensionKind::Segment => {
                let (d1, d2, ext_dir) = match dim.kind {
                    DimensionKind::Horizontal => {
                        let y_line = dim.p1.y.min(dim.p2.y) - dim.offset_mm;
                        (
                            Point2D::new(dim.p1.x, y_line),
                            Point2D::new(dim.p2.x, y_line),
                            Vec2D::new(0.0, -1.0),
                        )
                    }
                    DimensionKind::Vertical => {
                        let x_line = dim.p1.x.min(dim.p2.x) - dim.offset_mm;
                        (
                            Point2D::new(x_line, dim.p1.y),
                            Point2D::new(x_line, dim.p2.y),
                            Vec2D::new(-1.0, 0.0),
                        )
                    }
                    _ => {
                        let dir = (dim.p2 - dim.p1)
                            .normalized()
                            .unwrap_or(Vec2D::new(1.0, 0.0));
                        let perp = Vec2D::new(dir.y, -dir.x);
                        let off = perp * dim.offset_mm;
                        (dim.p1 + off, dim.p2 + off, perp)
                    }
                };

                // Extension lines with slight overshoot
                let overshoot = ext_dir * 1.5;
                let ext1 = BezierPath::from_line(dim.p1, d1 + overshoot);
                let ext2 = BezierPath::from_line(dim.p2, d2 + overshoot);
                let main_line = BezierPath::from_line(d1, d2);

                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&ext1, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&ext2, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&main_line, obj_to_screen, px_per_mm, options, backend);
                } else {
                    backend.stroke_path(&ext1, &stroke, obj_to_screen);
                    backend.stroke_path(&ext2, &stroke, obj_to_screen);
                    backend.stroke_path(&main_line, &stroke, obj_to_screen);

                    // Dimension end arrowheads
                    if let Some(dir) = (d2 - d1).normalized() {
                        draw_arrowhead(
                            d1,
                            -dir,
                            &ArrowheadStyle::Triangle,
                            &dim_outline.color,
                            stroke_w.max(0.35),
                            obj.opacity,
                            obj.blend_mode,
                            obj_to_screen,
                            anti_alias,
                            backend,
                        );
                        draw_arrowhead(
                            d2,
                            dir,
                            &ArrowheadStyle::Triangle,
                            &dim_outline.color,
                            stroke_w.max(0.35),
                            obj.opacity,
                            obj.blend_mode,
                            obj_to_screen,
                            anti_alias,
                            backend,
                        );
                    }
                }
                d1.midpoint(d2) + ext_dir * 1.2
            }
            DimensionKind::Angular => {
                let v = dim.p1;
                let r1 = (dim.p2 - v).normalized().unwrap_or(Vec2D::new(1.0, 0.0));
                let r2 = dim
                    .p3
                    .map_or(Vec2D::new(1.0, 0.0), |p| (p - v).normalize_or_zero());
                let rad = dim.offset_mm.abs().max(5.0);
                let ray1 = BezierPath::from_line(v, v + r1 * (rad + 2.0));
                let ray2 = BezierPath::from_line(v, v + r2 * (rad + 2.0));

                let a1 = r1.angle_rad();
                let mut a2 = r2.angle_rad();
                let mut sweep = a2 - a1;
                while sweep > std::f64::consts::PI {
                    sweep -= std::f64::consts::TAU;
                }
                while sweep < -std::f64::consts::PI {
                    sweep += std::f64::consts::TAU;
                }
                a2 = a1 + sweep;
                let steps = 24;
                let arc_pts: Vec<Point2D> = (0..=steps)
                    .map(|i| {
                        let a = a1 + (a2 - a1) * (i as f64 / steps as f64);
                        v + Vec2D::from_angle(a) * rad
                    })
                    .collect();
                let arc_path = BezierPath::from_polygon(arc_pts, false);

                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&ray1, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&ray2, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&arc_path, obj_to_screen, px_per_mm, options, backend);
                } else {
                    backend.stroke_path(&ray1, &stroke, obj_to_screen);
                    backend.stroke_path(&ray2, &stroke, obj_to_screen);
                    backend.stroke_path(&arc_path, &stroke, obj_to_screen);
                }
                let mid_a = 0.5 * (a1 + a2);
                v + Vec2D::from_angle(mid_a) * (rad + 3.0)
            }
            DimensionKind::Callout3Pt => {
                let mut pts = vec![dim.p1, dim.p2];
                let anchor = if let Some(p3) = dim.p3 {
                    pts.push(p3);
                    p3
                } else {
                    dim.p2
                };
                let leader = BezierPath::from_polygon(pts, false);
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&leader, obj_to_screen, px_per_mm, options, backend);
                } else {
                    backend.stroke_path(&leader, &stroke, obj_to_screen);
                }
                anchor
            }
        };

        // Draw formatted measurement label
        if !label_text.trim().is_empty() {
            let at = ArtisticText::from_plain(label_text, dim.text_style.clone());
            let label_xf = obj_to_screen.compose(&Affine2D::translate(
                label_anchor.x,
                label_anchor.y - dim.text_style.font_size_mm(),
            ));
            self.text_engine.render_artistic_text_icc(
                backend,
                &at,
                label_xf,
                &OutlineStyle::none(),
                obj.opacity,
                obj.blend_mode,
                view_mode,
                px_per_mm,
                &self.active_icc,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_text_pieces_with_effects(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        pieces: &[ShapedGlyphDraw],
        clip_rect_mm: Option<Rect2D>,
        base_xf: Affine2D,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        backend: &mut dyn CanvasBackend,
    ) {
        if pieces.is_empty() {
            return;
        }
        let anti_alias = view_mode != ViewMode::Pixels;
        let combined_local = BezierPath {
            subpaths: pieces
                .iter()
                .flat_map(|p| p.path.subpaths.iter().cloned())
                .collect(),
        };
        let deformed_local = apply_geometry_effects(&combined_local, &obj.effects);
        let draw_xf = parent_to_screen.compose(&base_xf);

        render_underlay_effects(
            doc,
            obj,
            &deformed_local,
            base_xf,
            parent_to_screen,
            page_to_surface,
            px_per_mm,
            anti_alias,
            &self.active_icc,
            backend,
        );

        let has_deform = obj.effects.iter().any(|e| {
            matches!(
                e,
                coree_core::Effect::Distort { .. }
                    | coree_core::Effect::Envelope { .. }
                    | coree_core::Effect::Perspective { .. }
            )
        });

        let clipped = clip_rect_mm.is_some();
        if let Some(cr) = clip_rect_mm {
            let clip_path = apply_geometry_effects(&BezierPath::from_rect(cr), &obj.effects);
            backend.push_clip(&clip_path, FillRule::Winding, draw_xf);
        }

        if has_deform {
            let fill = pieces
                .first()
                .map_or(Fill::Solid(Color::BLACK), |p| Fill::Solid(p.fill.clone()));
            self.draw_fill_styled(
                doc,
                &deformed_local,
                &fill,
                draw_xf,
                obj.opacity,
                obj.blend_mode,
                anti_alias,
                backend,
                0,
            );
            if obj.outline.is_visible() {
                draw_outline_styled(
                    &deformed_local,
                    base_xf,
                    parent_to_screen,
                    &obj.outline,
                    obj.opacity,
                    obj.blend_mode,
                    px_per_mm,
                    anti_alias,
                    &self.active_icc,
                    backend,
                );
            }
        } else {
            draw_text_pieces_icc(
                backend,
                pieces,
                draw_xf,
                &obj.outline,
                obj.opacity,
                obj.blend_mode,
                view_mode,
                px_per_mm,
                &self.active_icc,
            );
        }

        if clipped {
            backend.pop_clip();
        }

        render_overlay_effects(
            obj,
            &deformed_local,
            base_xf,
            parent_to_screen,
            px_per_mm,
            anti_alias,
            &self.active_icc,
            backend,
        );
    }

    /// Deformation context passed down to `render_mesh_fill` so Coons patch control points
    /// and interior `(u, v)` samples warp accurately under `Perspective3x3`, `Effect::{Envelope, Perspective, Distort}`,
    /// and enclosing `Group`/`PowerClip`/`SymbolInstance` `DeformFrame`s.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_fill_styled(
        &mut self,
        doc: &Document,
        local_path: &BezierPath,
        fill: &Fill,
        local_to_screen: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        anti_alias: bool,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        self.draw_fill_styled_with_deform(
            doc,
            local_path,
            fill,
            local_to_screen,
            opacity,
            blend_mode,
            anti_alias,
            backend,
            depth,
            None,
        );
    }

    /// Renders all 11 [`Fill`] variants onto `backend` with optional live deformation context for `MeshFill`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_fill_styled_with_deform(
        &mut self,
        doc: &Document,
        local_path: &BezierPath,
        fill: &Fill,
        local_to_screen: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        anti_alias: bool,
        backend: &mut dyn CanvasBackend,
        depth: usize,
        mesh_deform: Option<&MeshDeformCtx<'_>>,
    ) {
        let op = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
        if op <= 1e-6 || local_path.is_empty() {
            return;
        }

        match fill {
            Fill::None => {}
            Fill::Solid(color) => {
                let paint = BackendPaint {
                    shader: PaintShader::Solid(icc_srgb(color, &self.active_icc)),
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::LinearGradient { start, end, stops } => {
                let resolved_stops: Vec<GradientStop> = stops
                    .iter()
                    .map(|s| GradientStop {
                        offset: s.offset,
                        color: icc_srgb(&s.color, &self.active_icc),
                    })
                    .collect();
                let paint = BackendPaint {
                    shader: PaintShader::LinearGradient {
                        start: *start,
                        end: *end,
                        stops: resolved_stops,
                    },
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::RadialGradient {
                center,
                radius,
                focal,
                stops,
            } => {
                let resolved_stops: Vec<GradientStop> = stops
                    .iter()
                    .map(|s| GradientStop {
                        offset: s.offset,
                        color: icc_srgb(&s.color, &self.active_icc),
                    })
                    .collect();
                let paint = BackendPaint {
                    shader: PaintShader::RadialGradient {
                        focal: *focal,
                        center: *center,
                        radius: *radius,
                        stops: resolved_stops,
                    },
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::ConicalGradient {
                center,
                angle_deg,
                stops,
            } => {
                let icc = self.active_icc.clone();
                let all_cmyk = !stops.is_empty()
                    && stops.iter().all(|s| matches!(s.color, Color::Cmyk { .. }));
                let resolved_stops = if all_cmyk {
                    Vec::new()
                } else {
                    stops
                        .iter()
                        .map(|s| GradientStop {
                            offset: s.offset,
                            color: icc_srgb(&s.color, &icc),
                        })
                        .collect()
                };
                render_pixel_shader_fill(
                    local_path,
                    local_to_screen,
                    op,
                    blend_mode,
                    backend,
                    |local_pt| {
                        let dx = local_pt.x - center.x;
                        let dy = local_pt.y - center.y;
                        let deg = dy.atan2(dx).to_degrees();
                        let t = (deg - sanitize_f64(*angle_deg, 0.0)).rem_euclid(360.0) / 360.0;
                        if all_cmyk {
                            Fill::sample_stops_icc(stops, t as f32, &icc).to_rgba8_icc(&icc)
                        } else {
                            Fill::sample_stops(&resolved_stops, t as f32).to_rgba8()
                        }
                    },
                );
            }
            Fill::SquareGradient {
                center,
                size,
                angle_deg,
                stops,
            } => {
                let half = (sanitize_f64(*size, 20.0).abs() * 0.5).max(1e-4);
                let rot = -sanitize_f64(*angle_deg, 0.0).to_radians();
                let (s, c) = rot.sin_cos();
                let icc = self.active_icc.clone();
                let all_cmyk = !stops.is_empty()
                    && stops.iter().all(|s| matches!(s.color, Color::Cmyk { .. }));
                let resolved_stops = if all_cmyk {
                    Vec::new()
                } else {
                    stops
                        .iter()
                        .map(|s| GradientStop {
                            offset: s.offset,
                            color: icc_srgb(&s.color, &icc),
                        })
                        .collect()
                };
                render_pixel_shader_fill(
                    local_path,
                    local_to_screen,
                    op,
                    blend_mode,
                    backend,
                    |local_pt| {
                        let dx = local_pt.x - center.x;
                        let dy = local_pt.y - center.y;
                        let rx = dx * c - dy * s;
                        let ry = dx * s + dy * c;
                        let cheb = (rx.abs().max(ry.abs()) / half).clamp(0.0, 1.0);
                        if all_cmyk {
                            Fill::sample_stops_icc(stops, cheb as f32, &icc).to_rgba8_icc(&icc)
                        } else {
                            Fill::sample_stops(&resolved_stops, cheb as f32).to_rgba8()
                        }
                    },
                );
            }
            Fill::TwoColorPattern {
                fg,
                bg,
                pattern_name,
                tile_w,
                tile_h,
            } => {
                let tw = sanitize_f64(*tile_w, 10.0).max(0.5);
                let th = sanitize_f64(*tile_h, 10.0).max(0.5);
                let tile_px = 64u32;
                let fg_srgb = icc_srgb(fg, &self.active_icc);
                let bg_srgb = icc_srgb(bg, &self.active_icc);
                let tile =
                    generate_two_color_pattern_tile(pattern_name, &fg_srgb, &bg_srgb, tile_px);
                let tile_xf = Affine2D::scale(tw / f64::from(tile_px), th / f64::from(tile_px));
                let paint = BackendPaint {
                    shader: PaintShader::PixmapPattern {
                        tile,
                        tile_transform: tile_xf,
                    },
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::VectorPattern {
                symbol_id,
                tile_w,
                tile_h,
            } => {
                let tw = sanitize_f64(*tile_w, 15.0).max(0.5);
                let th = sanitize_f64(*tile_h, 15.0).max(0.5);
                let tile_px = 64u32;
                if let Some(sym) = doc.symbol(*symbol_id) {
                    let tile = self.rasterize_symbol_tile(doc, sym, tw, th, tile_px, depth + 1);
                    let tile_xf = Affine2D::scale(tw / f64::from(tile_px), th / f64::from(tile_px));
                    let paint = BackendPaint {
                        shader: PaintShader::PixmapPattern {
                            tile,
                            tile_transform: tile_xf,
                        },
                        opacity: op,
                        blend_mode,
                        anti_alias,
                    };
                    backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
                }
            }
            Fill::BitmapPattern {
                image_id,
                tile_w,
                tile_h,
            } => {
                if let Some(asset) = doc.asset(*image_id) {
                    if let Ok(tile) =
                        RgbaBuffer::from_vec(asset.width, asset.height, asset.to_rgba8_vec())
                    {
                        let tw = sanitize_f64(*tile_w, 20.0).max(0.5);
                        let th = sanitize_f64(*tile_h, 20.0).max(0.5);
                        let tile_xf = Affine2D::scale(
                            tw / f64::from(asset.width.max(1)),
                            th / f64::from(asset.height.max(1)),
                        );
                        let paint = BackendPaint {
                            shader: PaintShader::PixmapPattern {
                                tile,
                                tile_transform: tile_xf,
                            },
                            opacity: op,
                            blend_mode,
                            anti_alias,
                        };
                        backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
                    }
                }
            }
            Fill::TextureFill {
                preset,
                seed,
                fg,
                bg,
            } => {
                let fg_rgba = fg.to_rgba_f32_icc(&self.active_icc);
                let bg_rgba = bg.to_rgba_f32_icc(&self.active_icc);
                let seed_val = *seed;
                if let Some(ps_preset) = PostScriptFillPreset::from_preset_str(preset) {
                    let aa_mm = (1.0 / estimate_scale(&local_to_screen)).clamp(1e-4, 2.0);
                    let center = local_path
                        .bounding_box()
                        .map_or(Point2D::ZERO, |b| b.center());
                    render_pixel_shader_fill(
                        local_path,
                        local_to_screen,
                        op,
                        blend_mode,
                        backend,
                        |local_pt| {
                            let n =
                                eval_postscript_fill(ps_preset, seed_val, local_pt, center, aa_mm);
                            [
                                ((bg_rgba[0] + (fg_rgba[0] - bg_rgba[0]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                                ((bg_rgba[1] + (fg_rgba[1] - bg_rgba[1]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                                ((bg_rgba[2] + (fg_rgba[2] - bg_rgba[2]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                                ((bg_rgba[3] + (fg_rgba[3] - bg_rgba[3]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                            ]
                        },
                    );
                } else {
                    let preset_lc = preset.to_ascii_lowercase();
                    render_pixel_shader_fill(
                        local_path,
                        local_to_screen,
                        op,
                        blend_mode,
                        backend,
                        |local_pt| {
                            let n = eval_procedural_texture(
                                &preset_lc, seed_val, local_pt.x, local_pt.y,
                            );
                            [
                                ((bg_rgba[0] + (fg_rgba[0] - bg_rgba[0]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                                ((bg_rgba[1] + (fg_rgba[1] - bg_rgba[1]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                                ((bg_rgba[2] + (fg_rgba[2] - bg_rgba[2]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                                ((bg_rgba[3] + (fg_rgba[3] - bg_rgba[3]) * n) * 255.0)
                                    .round()
                                    .clamp(0.0, 255.0) as u8,
                            ]
                        },
                    );
                }
            }
            Fill::MeshFill { rows, cols, nodes } => {
                render_mesh_fill(
                    local_path,
                    *rows,
                    *cols,
                    nodes,
                    local_to_screen,
                    op,
                    blend_mode,
                    &self.active_icc,
                    backend,
                    mesh_deform,
                );
            }
        }
    }

    fn rasterize_symbol_tile(
        &mut self,
        doc: &Document,
        sym: &SymbolDef,
        tile_w: f64,
        tile_h: f64,
        tile_px: u32,
        depth: usize,
    ) -> RgbaBuffer {
        if depth > MAX_RENDER_DEPTH {
            return RgbaBuffer::new_transparent(tile_px, tile_px);
        }
        let Ok(mut sub) = TinySkiaBackend::new(tile_px, tile_px) else {
            return RgbaBuffer::new_transparent(tile_px, tile_px);
        };
        sub.clear(Color::TRANSPARENT);
        let scale = Affine2D::scale(
            f64::from(tile_px) / tile_w.max(0.1),
            f64::from(tile_px) / tile_h.max(0.1),
        );
        let opts = RenderOptions::export();
        let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
        let mut referenced: HashSet<ObjectId> = HashSet::new();
        for sub in &sym.objects {
            if sub.id.0 != 0 {
                child_map.insert(sub.id, sub);
            }
            match &sub.payload {
                NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                NodePayload::PowerClip { contents, .. } => {
                    referenced.extend(contents.iter().copied());
                }
                _ => {}
            }
        }
        let sym_ctx = Some((&child_map, sym.objects.as_slice(), Affine2D::IDENTITY));
        for obj in &sym.objects {
            if obj.id.0 != 0 && referenced.contains(&obj.id) {
                continue;
            }
            self.render_scene_object_with_sym(
                doc,
                None,
                obj,
                scale,
                scale,
                sym_ctx,
                Affine2D::IDENTITY,
                ViewMode::Enhanced,
                &opts,
                &mut sub,
                &[],
                depth + 1,
            );
        }
        sub.into_rgba()
    }
}

/// Evaluates an arbitrary per-pixel local-space shader clipped cleanly to `local_path`.
fn render_pixel_shader_fill<F>(
    local_path: &BezierPath,
    local_to_screen: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    backend: &mut dyn CanvasBackend,
    shader: F,
) where
    F: Fn(Point2D) -> [u8; 4],
{
    let Some(lb) = local_path.bounding_box() else {
        return;
    };
    let Some(screen_to_local) = local_to_screen.inverse() else {
        return;
    };
    let sb = local_to_screen.transform_rect(lb);
    let Some(clamped) = PixelRect::from_f64_rect_clamped(sb, 0, backend.width(), backend.height())
    else {
        return;
    };

    let mut buf = RgbaBuffer::new_transparent(clamped.width, clamped.height);
    for ly in 0..clamped.height {
        let sy = f64::from(clamped.y) + f64::from(ly) + 0.5;
        for lx in 0..clamped.width {
            let sx = f64::from(clamped.x) + f64::from(lx) + 0.5;
            let lp = screen_to_local.transform_point(Point2D::new(sx, sy));
            buf.set_pixel(lx, ly, shader(lp));
        }
    }

    backend.push_clip(local_path, FillRule::Winding, local_to_screen);
    backend.composite_region(clamped, &buf, opacity, blend_mode);
    backend.pop_clip();
}

/// True bicubic Coons-patch subdivided MeshFill rasterizer with tangent handles, per-node alpha,
/// CMYK/Lab color interpolation, `1x1` guard, adaptive subdivision, seam-free triangle coverage,
/// and live `Perspective3x3` / `Envelope` / `Perspective` / `Distort` / `DeformFrame` warping.
#[allow(clippy::too_many_arguments)]
fn render_mesh_fill(
    local_path: &BezierPath,
    rows: usize,
    cols: usize,
    nodes: &[MeshNode],
    local_to_screen: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
    mesh_deform: Option<&MeshDeformCtx<'_>>,
) {
    let Some(lb) = local_path.bounding_box() else {
        return;
    };
    let sb = local_to_screen.transform_rect(lb);
    let Some(clamped) = PixelRect::from_f64_rect_clamped(sb, 0, backend.width(), backend.height())
    else {
        return;
    };

    // Guard degenerate <= 1x1 mesh grids cleanly
    if rows <= 1 && cols <= 1 {
        let solid_col = nodes.first().map_or(Color::WHITE, |n| {
            let [r, g, b, a] = n.color.to_rgba_f32_icc(icc);
            let na = sanitize_f32(n.alpha, 1.0).clamp(0.0, 1.0);
            Color::rgba(r, g, b, (a * na).clamp(0.0, 1.0))
        });
        let paint = BackendPaint {
            shader: PaintShader::Solid(solid_col),
            opacity,
            blend_mode,
            anti_alias: true,
        };
        backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
        return;
    }

    let r_cnt = rows.clamp(2, 32);
    let c_cnt = cols.clamp(2, 32);
    let raw_lb = mesh_deform
        .and_then(|d| d.raw_local_bb)
        .filter(|r| r.is_finite())
        .or_else(|| {
            mesh_deform
                .and_then(|d| d.pre_effect_bb)
                .filter(|r| r.is_finite())
        })
        .unwrap_or(lb);

    let mut grid_pos = vec![vec![Point2D::ZERO; c_cnt]; r_cnt];
    let mut grid_handles: Vec<Vec<[Option<Point2D>; 4]>> = vec![vec![[None; 4]; c_cnt]; r_cnt];
    let mut grid_color: Vec<Vec<Color>> = vec![vec![Color::WHITE; c_cnt]; r_cnt];
    let mut grid_alpha: Vec<Vec<f32>> = vec![vec![1.0_f32; c_cnt]; r_cnt];
    let mut populated = vec![vec![false; c_cnt]; r_cnt];

    for (r, pos_row) in grid_pos.iter_mut().enumerate() {
        let v = r as f64 / (r_cnt - 1) as f64;
        for (c, slot) in pos_row.iter_mut().enumerate() {
            let u = c as f64 / (c_cnt - 1) as f64;
            *slot = Point2D::new(
                raw_lb.min_x + u * raw_lb.width(),
                raw_lb.min_y + v * raw_lb.height(),
            );
        }
    }
    let margin = raw_lb.width().max(raw_lb.height()).max(1.0) * 4.0;
    let clamp_pt = |p: Point2D| -> Option<Point2D> {
        p.is_finite().then(|| {
            Point2D::new(
                p.x.clamp(raw_lb.min_x - margin, raw_lb.max_x + margin),
                p.y.clamp(raw_lb.min_y - margin, raw_lb.max_y + margin),
            )
        })
    };
    for node in nodes {
        let r_idx = node.row.min(r_cnt - 1);
        let c_idx = node.col.min(c_cnt - 1);
        if node.row < r_cnt && node.col < c_cnt {
            let base = clamp_pt(node.pos).unwrap_or(grid_pos[r_idx][c_idx]);
            grid_pos[r_idx][c_idx] = base;
            // Relative tangent offsets [N, S, E, W] -> absolute control points in pre-deformation local space.
            let abs_handle = |h: Point2D| -> Option<Point2D> {
                h.is_finite()
                    .then(|| Point2D::new(base.x + h.x, base.y + h.y))
            };
            grid_handles[r_idx][c_idx] = [
                node.handle_n.and_then(abs_handle),
                node.handle_s.and_then(abs_handle),
                node.handle_e.and_then(abs_handle),
                node.handle_w.and_then(abs_handle),
            ];
            grid_color[r_idx][c_idx] = node.color.clone();
            grid_alpha[r_idx][c_idx] = sanitize_f32(node.alpha, 1.0).clamp(0.0, 1.0);
            populated[r_idx][c_idx] = true;
        }
    }
    // Fill any unpopulated sparse grid cells from nearest supplied node
    if !nodes.is_empty() {
        for r in 0..r_cnt {
            for c in 0..c_cnt {
                if populated[r][c] {
                    continue;
                }
                let mut best: Option<(usize, usize, usize)> = None;
                for (sr, pop_row) in populated.iter().enumerate().take(r_cnt) {
                    for (sc, &is_pop) in pop_row.iter().enumerate().take(c_cnt) {
                        if !is_pop {
                            continue;
                        }
                        let dist_sq = sr.abs_diff(r).pow(2) + sc.abs_diff(c).pow(2);
                        if best.is_none_or(|(bd, _, _)| dist_sq < bd) {
                            best = Some((dist_sq, sr, sc));
                        }
                    }
                }
                if let Some((_, sr, sc)) = best {
                    grid_color[r][c] = grid_color[sr][sc].clone();
                    grid_alpha[r][c] = grid_alpha[sr][sc];
                }
            }
        }
    }

    let prepared_obj_steps: Vec<DeformPointStep> = mesh_deform
        .map(|d| prepare_deform_point_steps(d.pre_effect_bb.or(Some(raw_lb)), d.effects))
        .unwrap_or_default();
    let prepared_frames: Vec<(Option<Perspective3x3>, Vec<DeformPointStep>, Affine2D)> =
        mesh_deform
            .map(|d| {
                d.deform_frames
                    .iter()
                    .rev()
                    .map(|frame| {
                        (
                            frame.pre_perspective,
                            prepare_deform_point_steps(frame.group_local_bb, frame.group_effects),
                            frame.rel_to_outer,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    let pre_perspective = mesh_deform.and_then(|d| d.pre_perspective);
    let c_xf = mesh_deform.map_or(Affine2D::IDENTITY, |d| d.c_xf);
    let has_mesh_deform = pre_perspective.is_some()
        || !prepared_obj_steps.is_empty()
        || c_xf != Affine2D::IDENTITY
        || !prepared_frames.is_empty();

    let deform_local_pt = |mut p: Point2D| -> Point2D {
        if !has_mesh_deform {
            return p;
        }
        if let Some(persp) = pre_perspective {
            if let Some(tp) = persp.transform_point(p) {
                p = tp;
            }
        }
        if !prepared_obj_steps.is_empty() {
            p = apply_deform_point_steps(p, &prepared_obj_steps);
        }
        if c_xf != Affine2D::IDENTITY {
            p = c_xf.transform_point(p);
        }
        for (frame_persp, frame_steps, rel_to_outer) in &prepared_frames {
            if let Some(fp) = frame_persp {
                if let Some(tp) = fp.transform_point(p) {
                    p = tp;
                }
            }
            if !frame_steps.is_empty() {
                p = apply_deform_point_steps(p, frame_steps);
            }
            if *rel_to_outer != Affine2D::IDENTITY {
                p = rel_to_outer.transform_point(p);
            }
        }
        p
    };

    let mut buf = RgbaBuffer::new_transparent(clamped.width, clamped.height);
    let to_buf_px = |p: Point2D| -> Point2D {
        let s = local_to_screen.transform_point(p);
        Point2D::new(s.x - f64::from(clamped.x), s.y - f64::from(clamped.y))
    };

    let eval_cubic_pt = |c0: Point2D, c1: Point2D, c2: Point2D, c3: Point2D, t: f64| -> Point2D {
        let it = 1.0 - t;
        let it2 = it * it;
        let t2 = t * t;
        let b0 = it2 * it;
        let b1 = 3.0 * it2 * t;
        let b2 = 3.0 * it * t2;
        let b3 = t2 * t;
        Point2D::new(
            b0 * c0.x + b1 * c1.x + b2 * c2.x + b3 * c3.x,
            b0 * c0.y + b1 * c1.y + b2 * c2.y + b3 * c3.y,
        )
    };

    let catmull_tangent = |prev: Point2D, next: Point2D| -> Vec2D { (next - prev) * (1.0 / 6.0) };

    for r in 0..(r_cnt - 1) {
        for c in 0..(c_cnt - 1) {
            let p00 = grid_pos[r][c];
            let p10 = grid_pos[r][c + 1];
            let p01 = grid_pos[r + 1][c];
            let p11 = grid_pos[r + 1][c + 1];

            // Neighbor Catmull-Rom chords in pre-deformation local space when explicit handles are absent
            let p_l0 = grid_pos[r][c.saturating_sub(1)];
            let p_r0 = grid_pos[r][(c + 2).min(c_cnt - 1)];
            let p_l1 = grid_pos[r + 1][c.saturating_sub(1)];
            let p_r1 = grid_pos[r + 1][(c + 2).min(c_cnt - 1)];
            let p_u0 = grid_pos[r.saturating_sub(1)][c];
            let p_d0 = grid_pos[(r + 2).min(r_cnt - 1)][c];
            let p_u1 = grid_pos[r.saturating_sub(1)][c + 1];
            let p_d1 = grid_pos[(r + 2).min(r_cnt - 1)][c + 1];

            // Edge cubic control points: [N:0, S:1, E:2, W:3].
            // Boundary endpoints use 1/3 chord for uniform straight parameterization;
            // interior endpoints use Catmull-Rom (next - prev) / 6 for C1 continuity.
            let t_c1 = grid_handles[r][c][2].unwrap_or_else(|| {
                if c > 0 {
                    p00 + catmull_tangent(p_l0, p10)
                } else {
                    p00 + (p10 - p00) * (1.0 / 3.0)
                }
            });
            let t_c2 = grid_handles[r][c + 1][3].unwrap_or_else(|| {
                if c + 2 < c_cnt {
                    p10 - catmull_tangent(p00, p_r0)
                } else {
                    p10 - (p10 - p00) * (1.0 / 3.0)
                }
            });
            let b_c1 = grid_handles[r + 1][c][2].unwrap_or_else(|| {
                if c > 0 {
                    p01 + catmull_tangent(p_l1, p11)
                } else {
                    p01 + (p11 - p01) * (1.0 / 3.0)
                }
            });
            let b_c2 = grid_handles[r + 1][c + 1][3].unwrap_or_else(|| {
                if c + 2 < c_cnt {
                    p11 - catmull_tangent(p01, p_r1)
                } else {
                    p11 - (p11 - p01) * (1.0 / 3.0)
                }
            });
            let l_c1 = grid_handles[r][c][1].unwrap_or_else(|| {
                if r > 0 {
                    p00 + catmull_tangent(p_u0, p01)
                } else {
                    p00 + (p01 - p00) * (1.0 / 3.0)
                }
            });
            let l_c2 = grid_handles[r + 1][c][0].unwrap_or_else(|| {
                if r + 2 < r_cnt {
                    p01 - catmull_tangent(p00, p_d0)
                } else {
                    p01 - (p01 - p00) * (1.0 / 3.0)
                }
            });
            let r_c1 = grid_handles[r][c + 1][1].unwrap_or_else(|| {
                if r > 0 {
                    p10 + catmull_tangent(p_u1, p11)
                } else {
                    p10 + (p11 - p10) * (1.0 / 3.0)
                }
            });
            let r_c2 = grid_handles[r + 1][c + 1][0].unwrap_or_else(|| {
                if r + 2 < r_cnt {
                    p11 - catmull_tangent(p10, p_d1)
                } else {
                    p11 - (p11 - p10) * (1.0 / 3.0)
                }
            });

            let eval_pos = |u: f64, v: f64| -> Point2D {
                let top = eval_cubic_pt(p00, t_c1, t_c2, p10, u);
                let bot = eval_cubic_pt(p01, b_c1, b_c2, p11, u);
                let left = eval_cubic_pt(p00, l_c1, l_c2, p01, v);
                let right = eval_cubic_pt(p10, r_c1, r_c2, p11, v);
                let ruled_u = top.lerp(bot, v);
                let ruled_v = left.lerp(right, u);
                let bilin = p00.lerp(p10, u).lerp(p01.lerp(p11, u), v);
                let coons = |a: f64, b: f64, c: f64| a + b - c;
                let local_pt = Point2D::new(
                    coons(ruled_u.x, ruled_v.x, bilin.x),
                    coons(ruled_u.y, ruled_v.y, bilin.y),
                );
                to_buf_px(deform_local_pt(local_pt))
            };

            let col00 = &grid_color[r][c];
            let col10 = &grid_color[r][c + 1];
            let col01 = &grid_color[r + 1][c];
            let col11 = &grid_color[r + 1][c + 1];
            let na00 = grid_alpha[r][c];
            let na10 = grid_alpha[r][c + 1];
            let na01 = grid_alpha[r + 1][c];
            let na11 = grid_alpha[r + 1][c + 1];

            let all_cmyk = matches!(
                (col00, col10, col01, col11),
                (
                    Color::Cmyk { .. },
                    Color::Cmyk { .. },
                    Color::Cmyk { .. },
                    Color::Cmyk { .. }
                )
            );
            let lab_corners = if all_cmyk {
                None
            } else {
                Some([
                    icc_srgb(col00, icc).to_lab(),
                    icc_srgb(col10, icc).to_lab(),
                    icc_srgb(col01, icc).to_lab(),
                    icc_srgb(col11, icc).to_lab(),
                ])
            };
            let rgba_corners = [
                col00.to_rgba_f32_icc(icc),
                col10.to_rgba_f32_icc(icc),
                col01.to_rgba_f32_icc(icc),
                col11.to_rgba_f32_icc(icc),
            ];

            let eval_col = |u: f64, v: f64| -> [f32; 4] {
                let su = (u * u * (3.0 - 2.0 * u)) as f32;
                let sv = (v * v * (3.0 - 2.0 * v)) as f32;
                let bilerp = |k00: f32, k10: f32, k01: f32, k11: f32| -> f32 {
                    let top = k00 + (k10 - k00) * su;
                    let bot = k01 + (k11 - k01) * su;
                    top + (bot - top) * sv
                };
                let eff_a = bilerp(
                    rgba_corners[0][3] * na00,
                    rgba_corners[1][3] * na10,
                    rgba_corners[2][3] * na01,
                    rgba_corners[3][3] * na11,
                )
                .clamp(0.0, 1.0);
                if all_cmyk {
                    if let (
                        Color::Cmyk {
                            c: c00,
                            m: m00,
                            y: y00,
                            k: k00,
                            ..
                        },
                        Color::Cmyk {
                            c: c10,
                            m: m10,
                            y: y10,
                            k: k10,
                            ..
                        },
                        Color::Cmyk {
                            c: c01,
                            m: m01,
                            y: y01,
                            k: k01,
                            ..
                        },
                        Color::Cmyk {
                            c: c11,
                            m: m11,
                            y: y11,
                            k: k11,
                            ..
                        },
                    ) = (col00, col10, col01, col11)
                    {
                        let cc = bilerp(*c00, *c10, *c01, *c11).clamp(0.0, 1.0);
                        let mm = bilerp(*m00, *m10, *m01, *m11).clamp(0.0, 1.0);
                        let yy = bilerp(*y00, *y10, *y01, *y11).clamp(0.0, 1.0);
                        let kk = bilerp(*k00, *k10, *k01, *k11).clamp(0.0, 1.0);
                        let rgb = icc.cmyk_to_srgb([cc, mm, yy, kk]);
                        return [rgb[0], rgb[1], rgb[2], eff_a];
                    }
                }
                if let Some(labs) = &lab_corners {
                    let l =
                        bilerp(labs[0][0], labs[1][0], labs[2][0], labs[3][0]).clamp(0.0, 100.0);
                    let a = bilerp(labs[0][1], labs[1][1], labs[2][1], labs[3][1]);
                    let b = bilerp(labs[0][2], labs[1][2], labs[2][2], labs[3][2]);
                    let rgb = Color::Lab {
                        l,
                        a_ch: a,
                        b_ch: b,
                        alpha: eff_a,
                    }
                    .to_rgba_f32();
                    return [rgb[0], rgb[1], rgb[2], eff_a];
                }
                [
                    bilerp(
                        rgba_corners[0][0],
                        rgba_corners[1][0],
                        rgba_corners[2][0],
                        rgba_corners[3][0],
                    )
                    .clamp(0.0, 1.0),
                    bilerp(
                        rgba_corners[0][1],
                        rgba_corners[1][1],
                        rgba_corners[2][1],
                        rgba_corners[3][1],
                    )
                    .clamp(0.0, 1.0),
                    bilerp(
                        rgba_corners[0][2],
                        rgba_corners[1][2],
                        rgba_corners[2][2],
                        rgba_corners[3][2],
                    )
                    .clamp(0.0, 1.0),
                    eff_a,
                ]
            };

            let q00 = eval_pos(0.0, 0.0);
            let q10 = eval_pos(1.0, 0.0);
            let q01 = eval_pos(0.0, 1.0);
            let q11 = eval_pos(1.0, 1.0);
            let qm0 = eval_pos(0.5, 0.0);
            let qm1 = eval_pos(0.5, 1.0);
            let patch_px_span = q00
                .distance_to(q10)
                .max(q01.distance_to(q11))
                .max(q00.distance_to(q01))
                .max(q10.distance_to(q11))
                .max(q00.distance_to(qm0) + qm0.distance_to(q10))
                .max(q01.distance_to(qm1) + qm1.distance_to(q11));
            let sub_divs = (patch_px_span / 4.0).ceil().clamp(10.0, 24.0) as usize;

            for sv in 0..sub_divs {
                let v0 = sv as f64 / sub_divs as f64;
                let v1 = (sv + 1) as f64 / sub_divs as f64;
                for su in 0..sub_divs {
                    let u0 = su as f64 / sub_divs as f64;
                    let u1 = (su + 1) as f64 / sub_divs as f64;

                    let a = (eval_pos(u0, v0), eval_col(u0, v0));
                    let b = (eval_pos(u1, v0), eval_col(u1, v0));
                    let d = (eval_pos(u1, v1), eval_col(u1, v1));
                    let e = (eval_pos(u0, v1), eval_col(u0, v1));

                    rasterize_gouraud_triangle(&mut buf, a, b, d);
                    rasterize_gouraud_triangle(&mut buf, a, d, e);
                }
            }
        }
    }

    backend.push_clip(local_path, FillRule::Winding, local_to_screen);
    backend.composite_region(clamped, &buf, opacity, blend_mode);
    backend.pop_clip();
}

/// Rasterizes a Gouraud-shaded triangle with barycentric RGBA color interpolation.
fn rasterize_gouraud_triangle(
    buf: &mut RgbaBuffer,
    v0: (Point2D, [f32; 4]),
    v1: (Point2D, [f32; 4]),
    v2: (Point2D, [f32; 4]),
) {
    let (p0, c0) = v0;
    let (p1, c1) = v1;
    let (p2, c2) = v2;
    if !p0.is_finite() || !p1.is_finite() || !p2.is_finite() {
        return;
    }
    let area = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
    if !area.is_finite() || area.abs() <= 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;

    let bw = f64::from(buf.width);
    let bh = f64::from(buf.height);
    let min_x = p0.x.min(p1.x).min(p2.x).floor().clamp(0.0, bw) as u32;
    let max_x = p0.x.max(p1.x).max(p2.x).ceil().clamp(0.0, bw) as u32;
    let min_y = p0.y.min(p1.y).min(p2.y).floor().clamp(0.0, bh) as u32;
    let max_y = p0.y.max(p1.y).max(p2.y).ceil().clamp(0.0, bh) as u32;

    for y in min_y..max_y {
        let py = f64::from(y) + 0.5;
        for x in min_x..max_x {
            let px = f64::from(x) + 0.5;
            let w0 = ((p1.x - px) * (p2.y - py) - (p2.x - px) * (p1.y - py)) * inv_area;
            let w1 = ((p2.x - px) * (p0.y - py) - (p0.x - px) * (p2.y - py)) * inv_area;
            let w2 = 1.0 - w0 - w1;
            if w0 >= -1e-3 && w1 >= -1e-3 && w2 >= -1e-3 {
                let raw0 = w0.clamp(0.0, 1.0) as f32;
                let raw1 = w1.clamp(0.0, 1.0) as f32;
                let raw2 = w2.clamp(0.0, 1.0) as f32;
                let wsum = (raw0 + raw1 + raw2).max(1e-6);
                let w0f = raw0 / wsum;
                let w1f = raw1 / wsum;
                let w2f = raw2 / wsum;
                let r = ((c0[0] * w0f + c1[0] * w1f + c2[0] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let g = ((c0[1] * w0f + c1[1] * w1f + c2[1] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let b = ((c0[2] * w0f + c1[2] * w1f + c2[2] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let a = ((c0[3] * w0f + c1[3] * w1f + c2[3] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                buf.set_pixel(x, y, [r, g, b, a]);
            }
        }
    }
}

/// Renders an [`OutlineStyle`] honoring `is_hairline`, `scale_with_object`, `dash_pattern`,
/// `calligraphy_stretch`/`calligraphy_angle_deg`, and `start_arrow`/`end_arrow` arrowheads!
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_outline_styled(
    local_path: &BezierPath,
    obj_xf: Affine2D,
    parent_to_screen: Affine2D,
    outline: &OutlineStyle,
    opacity: f32,
    blend_mode: BlendMode,
    px_per_mm: f64,
    anti_alias: bool,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    if !outline.is_visible() || opacity <= 1e-6 || local_path.is_empty() {
        return;
    }
    let resolved_outline = icc_outline(outline, icc);
    let outline = &resolved_outline;

    // Choose whether stroke width scales with object transform or stays constant in page mm
    let (draw_path, draw_xf, effective_scale) = if outline.scale_with_object {
        let composed = parent_to_screen.compose(&obj_xf);
        (local_path.clone(), composed, estimate_scale(&composed))
    } else {
        (local_path.transformed(&obj_xf), parent_to_screen, px_per_mm)
    };

    let width_mm = if outline.is_hairline {
        outline
            .effective_width_mm()
            .max(1.0 / effective_scale.max(1e-3))
    } else {
        outline.effective_width_mm()
    };

    // Calligraphic elliptical nib stroke when `calligraphy_stretch != 1.0`
    let stretch = sanitize_f64(outline.calligraphy_stretch, 1.0).clamp(0.05, 1.0);
    if (stretch - 1.0).abs() > 0.03 {
        draw_calligraphic_outline(
            &draw_path, draw_xf, outline, width_mm, stretch, opacity, blend_mode, anti_alias,
            backend,
        );
    } else {
        let params =
            StrokeParams::from_outline(outline, width_mm as f32, opacity, blend_mode, anti_alias);
        backend.stroke_path(&draw_path, &params, draw_xf);
    }

    // Start & end arrowheads on open subpaths
    if outline.start_arrow != ArrowheadStyle::None || outline.end_arrow != ArrowheadStyle::None {
        for sp in &draw_path.subpaths {
            if sp.closed || sp.nodes.len() < 2 {
                continue;
            }
            let single = BezierPath::from_subpath(sp.clone());
            let total_len = single.path_length_mm();
            if total_len <= 1e-6 {
                continue;
            }
            if outline.start_arrow != ArrowheadStyle::None {
                if let Some((pt, tangent)) = single.point_and_tangent_at(0.0) {
                    draw_arrowhead(
                        pt,
                        -tangent,
                        &outline.start_arrow,
                        &outline.color,
                        width_mm as f32,
                        opacity,
                        blend_mode,
                        draw_xf,
                        anti_alias,
                        backend,
                    );
                }
            }
            if outline.end_arrow != ArrowheadStyle::None {
                if let Some((pt, tangent)) = single.point_and_tangent_at(total_len) {
                    draw_arrowhead(
                        pt,
                        tangent,
                        &outline.end_arrow,
                        &outline.color,
                        width_mm as f32,
                        opacity,
                        blend_mode,
                        draw_xf,
                        anti_alias,
                        backend,
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_calligraphic_outline(
    path: &BezierPath,
    transform: Affine2D,
    outline: &OutlineStyle,
    width_mm: f64,
    stretch: f64,
    opacity: f32,
    blend_mode: BlendMode,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let angle = sanitize_f64(outline.calligraphy_angle_deg, 45.0).to_radians();
    let nib_major = Vec2D::from_angle(angle) * (0.5 * width_mm);
    let nib_minor = nib_major.perp() * stretch;
    let paint = BackendPaint {
        shader: PaintShader::Solid(outline.color.clone()),
        opacity,
        blend_mode,
        anti_alias,
    };

    for sp in &path.subpaths {
        let single = BezierPath::from_subpath(sp.clone());
        let len = single.path_length_mm();
        if len <= 1e-6 {
            continue;
        }
        let steps = ((len / 0.5).ceil() as usize).clamp(8, 256);
        let mut left = Vec::with_capacity(steps + 1);
        let mut right = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let d = (i as f64 / steps as f64) * len;
            let Some((pt, tangent)) = single.point_and_tangent_at(d) else {
                continue;
            };
            let n = tangent.perp();
            let proj = (nib_major.dot(n).hypot(nib_minor.dot(n))).max(width_mm * 0.08);
            left.push(pt + n * proj);
            right.push(pt - n * proj);
        }
        if left.len() >= 2 {
            right.reverse();
            left.extend(right);
            let ribbon = BezierPath::from_polygon(left, true);
            backend.fill_path(&ribbon, &paint, FillRule::Winding, transform);
        }
    }
}

/// Builds the geometry `(path, is_filled)` for an arrowhead at `tip` pointing along `forward_dir`.
#[must_use]
pub fn arrowhead_geometry(
    tip: Point2D,
    forward_dir: Vec2D,
    style: &ArrowheadStyle,
    stroke_w_mm: f32,
) -> Option<(BezierPath, bool)> {
    coree_core::arrowhead_geometry(tip, forward_dir, style, stroke_w_mm)
}

/// Collects all start and end arrowhead `(path, is_filled)` geometries for open subpaths of `path`.
#[must_use]
pub fn outline_arrowhead_geometries(
    path: &BezierPath,
    outline: &OutlineStyle,
) -> Vec<(BezierPath, bool)> {
    let mut out = Vec::new();
    if outline.start_arrow == ArrowheadStyle::None && outline.end_arrow == ArrowheadStyle::None {
        return out;
    }
    let width_mm = outline.effective_width_mm() as f32;
    for sp in &path.subpaths {
        if sp.closed || sp.nodes.len() < 2 {
            continue;
        }
        let single = BezierPath::from_subpath(sp.clone());
        let total_len = single.path_length_mm();
        if total_len <= 1e-6 {
            continue;
        }
        if outline.start_arrow != ArrowheadStyle::None {
            if let Some((pt, tangent)) = single.point_and_tangent_at(0.0) {
                if let Some(geom) = arrowhead_geometry(pt, -tangent, &outline.start_arrow, width_mm)
                {
                    out.push(geom);
                }
            }
        }
        if outline.end_arrow != ArrowheadStyle::None {
            if let Some((pt, tangent)) = single.point_and_tangent_at(total_len) {
                if let Some(geom) = arrowhead_geometry(pt, tangent, &outline.end_arrow, width_mm) {
                    out.push(geom);
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn draw_arrowhead(
    tip: Point2D,
    forward_dir: Vec2D,
    style: &ArrowheadStyle,
    color: &Color,
    stroke_w_mm: f32,
    opacity: f32,
    blend_mode: BlendMode,
    transform: Affine2D,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let Some((geom, filled)) = arrowhead_geometry(tip, forward_dir, style, stroke_w_mm) else {
        return;
    };
    if filled {
        let paint = BackendPaint {
            shader: PaintShader::Solid(color.clone()),
            opacity,
            blend_mode,
            anti_alias,
        };
        backend.fill_path(&geom, &paint, FillRule::Winding, transform);
    } else {
        let st = StrokeParams {
            color: color.clone(),
            width: stroke_w_mm.max(0.2),
            cap: coree_core::LineCap::Round,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
            opacity,
            blend_mode,
            anti_alias,
        };
        backend.stroke_path(&geom, &st, transform);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_raster_object(
    doc: &Document,
    obj: &SceneObject,
    raster: &coree_core::RasterLayerData,
    page_to_surface: Affine2D,
    parent_to_screen: Affine2D,
    obj_to_screen: Affine2D,
    px_per_mm: f64,
    view_mode: ViewMode,
    options: &RenderOptions,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    let w_mm = sanitize_f64(raster.width_mm, 50.0).max(0.1);
    let h_mm = sanitize_f64(raster.height_mm, 50.0).max(0.1);
    let full_rect = Rect2D::from_xywh(0.0, 0.0, w_mm, h_mm);
    let clip_rect = raster
        .crop_rect
        .filter(|r| r.is_finite())
        .unwrap_or(full_rect);

    let mut frame_path = BezierPath::from_rect(clip_rect);
    if let Some(persp) = obj.perspective {
        frame_path = persp.transform_bezier_path(&frame_path);
    }
    let pre_eff_bb = frame_path.bounding_box();
    let deformed_frame = apply_geometry_effects_in_bounds(&frame_path, pre_eff_bb, &obj.effects);

    if view_mode == ViewMode::Wireframe {
        let mut wf = deformed_frame.clone();
        // Diagonal X indicator for wireframe raster placeholders
        let mut diag = BezierPath::default();
        diag.subpaths.push(Subpath::from_points(
            [
                Point2D::new(clip_rect.min_x, clip_rect.min_y),
                Point2D::new(clip_rect.max_x, clip_rect.max_y),
            ],
            false,
        ));
        diag.subpaths.push(Subpath::from_points(
            [
                Point2D::new(clip_rect.max_x, clip_rect.min_y),
                Point2D::new(clip_rect.min_x, clip_rect.max_y),
            ],
            false,
        ));
        if let Some(persp) = obj.perspective {
            diag = persp.transform_bezier_path(&diag);
        }
        let diag_def = apply_geometry_effects_in_bounds(&diag, pre_eff_bb, &obj.effects);
        wf.subpaths.extend(diag_def.subpaths);
        stroke_wireframe(&wf, obj_to_screen, px_per_mm, options, backend);
        return;
    }

    let Some(asset) = doc.asset(raster.asset_id) else {
        return;
    };
    let anti_alias = view_mode != ViewMode::Pixels;
    if !obj.effects.is_empty() {
        render_underlay_effects(
            doc,
            obj,
            &deformed_frame,
            obj.transform,
            parent_to_screen,
            page_to_surface,
            px_per_mm,
            anti_alias,
            icc,
            backend,
        );
    }

    let filtered = if raster.filters.is_empty() {
        None
    } else {
        Some(apply_raster_filters(asset, &raster.filters))
    };
    let active_asset = filtered.as_ref().unwrap_or(asset);
    let smooth = view_mode != ViewMode::Pixels;

    // Compose obj.perspective + any Effect::Perspective into a single local homography
    let mut combined_persp = obj.perspective;
    let mut cur_bb = pre_eff_bb.unwrap_or(clip_rect);
    for eff in &obj.effects {
        if let Effect::Perspective { quad } = eff {
            if let Some(eff_h) = Perspective3x3::from_rect_to_quad(cur_bb, quad) {
                combined_persp = Some(match combined_persp {
                    Some(p) => eff_h.compose(&p),
                    None => eff_h,
                });
                if let Some(next_bb) = eff_h.try_transform_rect(cur_bb) {
                    cur_bb = next_bb;
                }
            }
        }
    }

    if let Some(persp) = combined_persp {
        if let (Some(inv_persp), Some(warped_bb)) =
            (persp.inverse(), persp.try_transform_rect(clip_rect))
        {
            let scale_est = estimate_scale(&obj_to_screen).max(px_per_mm).max(0.25);
            let out_w = ((warped_bb.width() * scale_est).ceil() as u32).clamp(1, 2048);
            let out_h = ((warped_bb.height() * scale_est).ceil() as u32).clamp(1, 2048);
            let src_rgba = active_asset.to_rgba8_vec();
            let sw = active_asset.width as usize;
            let sh = active_asset.height as usize;
            let mut warped = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
            let step_x = warped_bb.width() / f64::from(out_w);
            let step_y = warped_bb.height() / f64::from(out_h);
            for py in 0..out_h {
                let wy = warped_bb.min_y + (f64::from(py) + 0.5) * step_y;
                for px in 0..out_w {
                    let wx = warped_bb.min_x + (f64::from(px) + 0.5) * step_x;
                    let Some(src_mm) = inv_persp.transform_point(Point2D::new(wx, wy)) else {
                        continue;
                    };
                    if src_mm.x < clip_rect.min_x
                        || src_mm.x > clip_rect.max_x
                        || src_mm.y < clip_rect.min_y
                        || src_mm.y > clip_rect.max_y
                    {
                        continue;
                    }
                    let u = (src_mm.x / w_mm) * f64::from(active_asset.width);
                    let v = (src_mm.y / h_mm) * f64::from(active_asset.height);
                    if u < 0.0
                        || v < 0.0
                        || u >= f64::from(active_asset.width)
                        || v >= f64::from(active_asset.height)
                    {
                        continue;
                    }
                    let x0 = (u.floor() as usize).min(sw.saturating_sub(1));
                    let y0 = (v.floor() as usize).min(sh.saturating_sub(1));
                    let x1 = (x0 + 1).min(sw.saturating_sub(1));
                    let y1 = (y0 + 1).min(sh.saturating_sub(1));
                    let fx = (u - x0 as f64) as f32;
                    let fy = (v - y0 as f64) as f32;
                    let i00 = (y0 * sw + x0) * 4;
                    let i10 = (y0 * sw + x1) * 4;
                    let i01 = (y1 * sw + x0) * 4;
                    let i11 = (y1 * sw + x1) * 4;
                    let dst_i = ((py as usize) * (out_w as usize) + (px as usize)) * 4;
                    for c in 0..4 {
                        let top = f32::from(src_rgba[i00 + c]) * (1.0 - fx)
                            + f32::from(src_rgba[i10 + c]) * fx;
                        let bot = f32::from(src_rgba[i01 + c]) * (1.0 - fx)
                            + f32::from(src_rgba[i11 + c]) * fx;
                        warped[dst_i + c] =
                            (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            if let Ok(buf) = RgbaBuffer::from_vec(out_w, out_h, warped) {
                let draw_xf = obj_to_screen
                    .compose(&Affine2D::translate(warped_bb.min_x, warped_bb.min_y))
                    .compose(&Affine2D::scale(step_x, step_y));
                backend.draw_pixmap(&buf, draw_xf, obj.opacity, obj.blend_mode, smooth);
            }
        }
    } else if let Ok(buf) = RgbaBuffer::from_vec(
        active_asset.width,
        active_asset.height,
        active_asset.to_rgba8_vec(),
    ) {
        let sx = w_mm / f64::from(active_asset.width.max(1));
        let sy = h_mm / f64::from(active_asset.height.max(1));
        let img_xf = obj_to_screen.compose(&Affine2D::scale(sx, sy));
        if raster.crop_rect.is_some() {
            let crop_path = BezierPath::from_rect(clip_rect);
            backend.push_clip(&crop_path, FillRule::Winding, obj_to_screen);
            backend.draw_pixmap(&buf, img_xf, obj.opacity, obj.blend_mode, smooth);
            backend.pop_clip();
        } else {
            backend.draw_pixmap(&buf, img_xf, obj.opacity, obj.blend_mode, smooth);
        }
    }

    if !obj.effects.is_empty() {
        render_overlay_effects(
            obj,
            &deformed_frame,
            obj.transform,
            parent_to_screen,
            px_per_mm,
            anti_alias,
            icc,
            backend,
        );
    }
}

pub(crate) fn render_page_surface(
    page: &Page,
    page_to_surface: Affine2D,
    px_per_mm: f64,
    options: &RenderOptions,
    icc: &IccColorPipeline,
    backend: &mut dyn CanvasBackend,
) {
    let page_rect = page.trim_rect();
    let page_path = BezierPath::from_rect(page_rect);

    if options.show_page_shadow {
        let shadow_off = (3.5 / px_per_mm.max(0.2)).clamp(1.0, 5.0);
        let shadow_rect =
            BezierPath::from_rect(page_rect.translate(Vec2D::new(shadow_off, shadow_off)));
        let shadow_paint = BackendPaint::solid(Color::rgba8(40, 45, 55, 65));
        backend.fill_path(
            &shadow_rect,
            &shadow_paint,
            FillRule::Winding,
            page_to_surface,
        );
    }

    if options.page_bg.alpha() > 1e-6 {
        let page_paint = BackendPaint::solid(icc_srgb(&options.page_bg, icc));
        backend.fill_path(&page_path, &page_paint, FillRule::Winding, page_to_surface);
    }

    if options.show_page_shadow {
        let border = StrokeParams {
            color: Color::rgba8(140, 145, 155, 255),
            width: (1.0 / px_per_mm.max(1e-3)) as f32,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        backend.stroke_path(&page_path, &border, page_to_surface);
    }
}

fn render_page_overlays(
    doc: &Document,
    page: &Page,
    page_to_surface: Affine2D,
    px_per_mm: f64,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
) {
    let hairline_mm = (1.0 / px_per_mm.max(1e-3)) as f32;

    // 1. Document Grid
    let grid = doc.grid();
    if options.show_grid && grid.visible {
        let sx = sanitize_f64(grid.spacing_x_mm, 10.0).max(0.5);
        let sy = sanitize_f64(grid.spacing_y_mm, 10.0).max(0.5);
        let subs = grid.subdivisions.clamp(1, 10);
        let step_x = sx / f64::from(subs);
        let step_y = sy / f64::from(subs);

        let grid_stroke = StrokeParams {
            color: Color::rgba8(180, 195, 220, 120),
            width: hairline_mm,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        if grid.show_dots {
            let dot_paint = BackendPaint::solid(Color::rgba8(130, 150, 185, 180));
            let r = f64::from(hairline_mm) * 0.8;
            let mut y = 0.0;
            let mut guard_y = 0usize;
            while y <= page.height_mm + 1e-6 && guard_y < 400 {
                let mut x = 0.0;
                let mut guard_x = 0usize;
                while x <= page.width_mm + 1e-6 && guard_x < 400 {
                    let dot = BezierPath::from_rect(Rect2D::from_center_size(
                        Point2D::new(x, y),
                        r * 2.0,
                        r * 2.0,
                    ));
                    backend.fill_path(&dot, &dot_paint, FillRule::Winding, page_to_surface);
                    x += step_x;
                    guard_x += 1;
                }
                y += step_y;
                guard_y += 1;
            }
        } else {
            let mut subpaths = Vec::new();
            let mut x = 0.0;
            let mut gx = 0usize;
            while x <= page.width_mm + 1e-6 && gx < 500 {
                subpaths.push(Subpath::from_points(
                    [Point2D::new(x, 0.0), Point2D::new(x, page.height_mm)],
                    false,
                ));
                x += step_x;
                gx += 1;
            }
            let mut y = 0.0;
            let mut gy = 0usize;
            while y <= page.height_mm + 1e-6 && gy < 500 {
                subpaths.push(Subpath::from_points(
                    [Point2D::new(0.0, y), Point2D::new(page.width_mm, y)],
                    false,
                ));
                y += step_y;
                gy += 1;
            }
            backend.stroke_path(&BezierPath { subpaths }, &grid_stroke, page_to_surface);
        }
    }

    // 2. Bleed & Margin Guides
    if options.show_bleed_guides && page.bleed_mm > 1e-4 {
        let bleed_path = BezierPath::from_rect(page.bleed_rect());
        let st = StrokeParams {
            color: Color::rgba8(220, 40, 90, 180),
            width: hairline_mm,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: vec![hairline_mm * 5.0, hairline_mm * 3.0],
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        backend.stroke_path(&bleed_path, &st, page_to_surface);
    }

    if options.show_margin_guides && options.margin_mm > 1e-4 {
        let m = options.margin_mm;
        if page.width_mm > 2.0 * m && page.height_mm > 2.0 * m {
            let margin_path = BezierPath::from_rect(Rect2D::from_xywh(
                m,
                m,
                page.width_mm - 2.0 * m,
                page.height_mm - 2.0 * m,
            ));
            let st = StrokeParams {
                color: Color::rgba8(60, 140, 230, 160),
                width: hairline_mm,
                cap: coree_core::LineCap::Butt,
                join: coree_core::LineJoin::Miter,
                miter_limit: 4.0,
                dash_pattern: vec![hairline_mm * 4.0, hairline_mm * 4.0],
                dash_offset: 0.0,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                anti_alias: false,
            };
            backend.stroke_path(&margin_path, &st, page_to_surface);
        }
    }

    // 3. Perspective Grid
    if options.show_perspective_grid && doc.perspective_grid().enabled {
        render_perspective_grid(
            doc.perspective_grid(),
            page,
            page_to_surface,
            hairline_mm,
            backend,
        );
    }

    // 4. Guidelines (clipped to visible surface bounds and phase-anchored to integer multiples of period_mm)
    let guides = doc.guides();
    if options.show_guidelines && !guides.is_empty() {
        let dash_on = hairline_mm * 6.0;
        let dash_off = hairline_mm * 3.0;
        let period_mm = (f64::from(dash_on) + f64::from(dash_off)).max(1e-6);
        let guide_stroke = StrokeParams {
            color: Color::rgba8(0, 165, 225, 200),
            width: hairline_mm,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: vec![dash_on, dash_off],
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        let surf_rect = Rect2D::from_xywh(
            -4.0,
            -4.0,
            f64::from(backend.width()) + 8.0,
            f64::from(backend.height()) + 8.0,
        );
        let vis_page = page_to_surface
            .inverse()
            .filter(|m| m.is_finite())
            .map(|inv| inv.transform_rect(surf_rect))
            .filter(|r| r.is_finite());
        let align_span = |min_s: f64, max_s: f64| -> (f64, f64) {
            let k0 = (min_s / period_mm).floor();
            let start = k0 * period_mm;
            let end = max_s.max(start + period_mm);
            (start, end)
        };
        let extent = page.width_mm.max(page.height_mm) * 4.0;
        let mut subpaths = Vec::with_capacity(guides.len());
        for guide in guides {
            match guide.orientation {
                GuideOrientation::Horizontal(y) => {
                    if let Some(vis) = vis_page {
                        if y < vis.min_y || y > vis.max_y {
                            continue;
                        }
                        let (x0, x1) = align_span(vis.min_x, vis.max_x);
                        subpaths.push(Subpath::from_points(
                            [Point2D::new(x0, y), Point2D::new(x1, y)],
                            false,
                        ));
                    } else {
                        subpaths.push(Subpath::from_points(
                            [Point2D::new(-extent, y), Point2D::new(extent, y)],
                            false,
                        ));
                    }
                }
                GuideOrientation::Vertical(x) => {
                    if let Some(vis) = vis_page {
                        if x < vis.min_x || x > vis.max_x {
                            continue;
                        }
                        let (y0, y1) = align_span(vis.min_y, vis.max_y);
                        subpaths.push(Subpath::from_points(
                            [Point2D::new(x, y0), Point2D::new(x, y1)],
                            false,
                        ));
                    } else {
                        subpaths.push(Subpath::from_points(
                            [Point2D::new(x, -extent), Point2D::new(x, extent)],
                            false,
                        ));
                    }
                }
                GuideOrientation::Angled { origin, angle_deg } => {
                    let dir = Vec2D::from_angle(angle_deg.to_radians());
                    if let Some(vis) = vis_page {
                        let mut s_min = f64::NEG_INFINITY;
                        let mut s_max = f64::INFINITY;
                        let mut clip_slab =
                            |orig_c: f64, dir_c: f64, min_c: f64, max_c: f64| -> bool {
                                if dir_c.abs() <= 1e-12 {
                                    orig_c >= min_c && orig_c <= max_c
                                } else {
                                    let t0 = (min_c - orig_c) / dir_c;
                                    let t1 = (max_c - orig_c) / dir_c;
                                    let (lo, hi) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
                                    s_min = s_min.max(lo);
                                    s_max = s_max.min(hi);
                                    s_min <= s_max
                                }
                            };
                        if !clip_slab(origin.x, dir.x, vis.min_x, vis.max_x)
                            || !clip_slab(origin.y, dir.y, vis.min_y, vis.max_y)
                            || !s_min.is_finite()
                            || !s_max.is_finite()
                        {
                            continue;
                        }
                        let (s0, s1) = align_span(s_min, s_max);
                        subpaths.push(Subpath::from_points(
                            [origin + dir * s0, origin + dir * s1],
                            false,
                        ));
                    } else {
                        let span = dir * extent;
                        subpaths.push(Subpath::from_points([origin - span, origin + span], false));
                    }
                }
            }
        }
        if !subpaths.is_empty() {
            backend.stroke_path(&BezierPath { subpaths }, &guide_stroke, page_to_surface);
        }
    }
}

fn render_perspective_grid(
    pg: &coree_core::PerspectiveGridConfig,
    page: &Page,
    page_to_surface: Affine2D,
    hairline_mm: f32,
    backend: &mut dyn CanvasBackend,
) {
    let op = sanitize_f32(pg.opacity, 0.35).clamp(0.05, 1.0);
    let st = StrokeParams {
        color: Color::rgba(0.2, 0.45, 0.85, op),
        width: hairline_mm,
        cap: coree_core::LineCap::Butt,
        join: coree_core::LineJoin::Miter,
        miter_limit: 4.0,
        dash_pattern: Vec::new(),
        dash_offset: 0.0,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        anti_alias: true,
    };
    let density = pg.density.clamp(4, 60);
    let mut subpaths = Vec::new();
    let max_dim = page.width_mm.max(page.height_mm) * 2.0;

    match pg.mode {
        PerspectiveGridMode::Isometric => {
            let step = (page.width_mm / f64::from(density)).max(5.0);
            for deg in [30.0_f64, 150.0, 90.0] {
                let dir = Vec2D::from_angle(deg.to_radians());
                let perp = dir.perp();
                let count = (max_dim / step) as i32;
                for i in -count..=count {
                    let anchor = Point2D::new(page.width_mm * 0.5, page.height_mm * 0.5)
                        + perp * (f64::from(i) * step);
                    subpaths.push(Subpath::from_points(
                        [anchor - dir * max_dim, anchor + dir * max_dim],
                        false,
                    ));
                }
            }
        }
        PerspectiveGridMode::OnePoint
        | PerspectiveGridMode::TwoPoint
        | PerspectiveGridMode::ThreePoint => {
            // Horizon line
            subpaths.push(Subpath::from_points(
                [
                    Point2D::new(-page.width_mm, pg.horizon_y_mm),
                    Point2D::new(page.width_mm * 2.0, pg.horizon_y_mm),
                ],
                false,
            ));
            // Radiating rays from every vanishing point
            for &vp in &pg.vanishing_points {
                if !vp.is_finite() {
                    continue;
                }
                for i in 0..density {
                    let angle = (f64::from(i) / f64::from(density)) * std::f64::consts::TAU;
                    let end = vp + Vec2D::from_angle(angle) * max_dim;
                    subpaths.push(Subpath::from_points([vp, end], false));
                }
            }
        }
    }

    let clip = BezierPath::from_rect(page.trim_rect());
    backend.push_clip(&clip, FillRule::Winding, page_to_surface);
    backend.stroke_path(&BezierPath { subpaths }, &st, page_to_surface);
    backend.pop_clip();
}

pub(crate) fn stroke_wireframe(
    path: &BezierPath,
    transform: Affine2D,
    px_per_mm: f64,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
) {
    let wire = StrokeParams {
        color: options.wireframe_stroke_color.clone(),
        width: (1.0 / px_per_mm.max(1e-3)) as f32,
        cap: coree_core::LineCap::Butt,
        join: coree_core::LineJoin::Miter,
        miter_limit: 4.0,
        dash_pattern: Vec::new(),
        dash_offset: 0.0,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        anti_alias: true,
    };
    backend.stroke_path(path, &wire, transform);
}

/// Collects all layers visible on `page_index`, including applicable master layers across the document.
pub(crate) fn collect_effective_layers(doc: &Document, page_index: usize) -> Vec<&Layer> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let is_odd = page_index.is_multiple_of(2); // 1-based Page 1 = index 0 = Odd

    for page in doc.pages() {
        for layer in &page.layers {
            let applies = layer.is_desktop_layer
                || match layer.master_scope {
                    MasterLayerScope::None => false,
                    MasterLayerScope::AllPages => true,
                    MasterLayerScope::OddPages => is_odd,
                    MasterLayerScope::EvenPages => !is_odd,
                };
            if applies && seen.insert(layer.id) {
                out.push(layer);
            }
        }
    }
    if let Some(page) = doc.page(page_index) {
        for layer in &page.layers {
            if !layer.is_desktop_layer
                && layer.master_scope == MasterLayerScope::None
                && seen.insert(layer.id)
            {
                out.push(layer);
            }
        }
    }
    out
}

fn object_nonlocal_and_lens_info(
    doc: &Document,
    obj: &SceneObject,
    visiting_objs: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    depth: usize,
) -> (bool, bool) {
    object_nonlocal_and_lens_info_inner(doc, obj, None, visiting_objs, visiting_syms, depth)
}

fn object_nonlocal_and_lens_info_inner(
    doc: &Document,
    obj: &SceneObject,
    sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
    visiting_objs: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    depth: usize,
) -> (bool, bool) {
    if depth > MAX_RENDER_DEPTH {
        return (false, false);
    }
    let self_lens = obj.lens.is_some();
    let self_nonlocal = self_lens || obj.effects.iter().any(|e| {
        matches!(
            e,
            coree_core::Effect::Blend { .. }
                | coree_core::Effect::Symmetry { .. }
                | coree_core::Effect::Perspective { .. }
                | coree_core::Effect::Envelope { .. }
                | coree_core::Effect::Distort { .. }
        ) || matches!(
            e,
            coree_core::Effect::Extrude25D { vanishing_point, .. } if !vanishing_point.is_finite()
        )
    });
    let mut nonlocal = self_nonlocal;
    let mut has_lens = self_lens;
    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children {
                if !visiting_objs.insert(cid) {
                    continue;
                }
                if let Some(c) = sym_map
                    .and_then(|m| m.get(&cid).copied())
                    .or_else(|| doc.object(cid))
                {
                    let (cn, cl) = object_nonlocal_and_lens_info_inner(
                        doc,
                        c,
                        sym_map,
                        visiting_objs,
                        visiting_syms,
                        depth + 1,
                    );
                    nonlocal |= cn;
                    has_lens |= cl;
                }
                visiting_objs.remove(&cid);
                if nonlocal && has_lens {
                    break;
                }
            }
        }
        NodePayload::PowerClip { contents, .. } => {
            for &cid in contents {
                if !visiting_objs.insert(cid) {
                    continue;
                }
                if let Some(c) = sym_map
                    .and_then(|m| m.get(&cid).copied())
                    .or_else(|| doc.object(cid))
                {
                    let (cn, cl) = object_nonlocal_and_lens_info_inner(
                        doc,
                        c,
                        sym_map,
                        visiting_objs,
                        visiting_syms,
                        depth + 1,
                    );
                    nonlocal |= cn;
                    has_lens |= cl;
                }
                visiting_objs.remove(&cid);
                if nonlocal && has_lens {
                    break;
                }
            }
        }
        NodePayload::Clone { .. } => {
            if let Some((p, f, o)) = doc.resolve_clone_payload_and_style_with_sym(obj, sym_map) {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                let (mn, ml) = object_nonlocal_and_lens_info_inner(
                    doc,
                    &tmp,
                    sym_map,
                    visiting_objs,
                    visiting_syms,
                    depth + 1,
                );
                nonlocal |= mn;
                has_lens |= ml;
            }
        }
        NodePayload::SymbolInstance(sym_id) if visiting_syms.insert(*sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        child_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                        NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                let mut sym_visiting = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        continue;
                    }
                    if sub.id.0 != 0 && !sym_visiting.insert(sub.id) {
                        continue;
                    }
                    let (sn, sl) = object_nonlocal_and_lens_info_inner(
                        doc,
                        sub,
                        Some(&child_map),
                        &mut sym_visiting,
                        visiting_syms,
                        depth + 1,
                    );
                    if sub.id.0 != 0 {
                        sym_visiting.remove(&sub.id);
                    }
                    nonlocal |= sn;
                    has_lens |= sl;
                    if nonlocal && has_lens {
                        break;
                    }
                }
            }
            visiting_syms.remove(sym_id);
        }
        _ => {}
    }
    (nonlocal, has_lens)
}

pub(crate) fn max_object_effect_padding_mm(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: Affine2D,
) -> f64 {
    let mut visiting_objs = HashSet::new();
    let mut visiting_syms = HashSet::new();
    if obj.id.0 != 0 {
        visiting_objs.insert(obj.id);
    }
    max_object_effect_padding_inner(
        doc,
        obj,
        parent_xf,
        None,
        &mut visiting_objs,
        &mut visiting_syms,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn max_object_effect_padding_inner(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: Affine2D,
    sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
    visiting_objs: &mut HashSet<ObjectId>,
    visiting_syms: &mut HashSet<SymbolId>,
    depth: usize,
) -> f64 {
    if depth > MAX_RENDER_DEPTH || !parent_xf.is_finite() || !obj.transform.is_finite() {
        return 0.0;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return 0.0;
    }
    let mut max_pad = effect_bounds_padding_mm(&obj.effects);
    let lin_norm =
        (world_xf.m[0].abs() + world_xf.m[2].abs()).max(world_xf.m[1].abs() + world_xf.m[3].abs());
    if lin_norm.is_finite() && lin_norm > 1.0 {
        for eff in &obj.effects {
            match eff {
                Effect::Distort { amplitude, .. } if amplitude.is_finite() => {
                    max_pad = max_pad.max(lin_norm * (2.0 * amplitude.abs() + 2.0));
                }
                Effect::DropShadow {
                    dx_mm,
                    dy_mm,
                    blur_mm,
                    ..
                } => {
                    let base =
                        (dx_mm.abs() + blur_mm.abs() * 3.0).max(dy_mm.abs() + blur_mm.abs() * 3.0);
                    if base.is_finite() {
                        max_pad = max_pad.max(lin_norm * base);
                    }
                }
                Effect::BlockShadow { dx_mm, dy_mm, .. } => {
                    let base = dx_mm.abs().max(dy_mm.abs()) + 2.0;
                    if base.is_finite() {
                        max_pad = max_pad.max(lin_norm * base);
                    }
                }
                Effect::Contour {
                    mode: coree_core::ContourMode::Outside,
                    steps,
                    offset_mm,
                    ..
                } => {
                    let base = f64::from(*steps) * offset_mm.abs() + 2.0;
                    if base.is_finite() {
                        max_pad = max_pad.max(lin_norm * base);
                    }
                }
                _ => {}
            }
        }
    }
    if obj.outline.is_visible() {
        let stroke_w = obj
            .outline
            .scaled_width_mm_for_transform(&world_xf)
            .max(obj.outline.effective_width_mm());
        if stroke_w.is_finite() {
            max_pad = max_pad.max(stroke_w * 4.0);
        }
    }
    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children {
                if !visiting_objs.insert(cid) {
                    continue;
                }
                if let Some(child) = sym_map
                    .and_then(|m| m.get(&cid).copied())
                    .or_else(|| doc.object(cid))
                {
                    max_pad = max_pad.max(max_object_effect_padding_inner(
                        doc,
                        child,
                        world_xf,
                        sym_map,
                        visiting_objs,
                        visiting_syms,
                        depth + 1,
                    ));
                }
                visiting_objs.remove(&cid);
            }
        }
        NodePayload::PowerClip { contents, .. } => {
            for &cid in contents {
                if !visiting_objs.insert(cid) {
                    continue;
                }
                if let Some(child) = sym_map
                    .and_then(|m| m.get(&cid).copied())
                    .or_else(|| doc.object(cid))
                {
                    max_pad = max_pad.max(max_object_effect_padding_inner(
                        doc,
                        child,
                        world_xf,
                        sym_map,
                        visiting_objs,
                        visiting_syms,
                        depth + 1,
                    ));
                }
                visiting_objs.remove(&cid);
            }
        }
        NodePayload::SymbolInstance(sym_id) if visiting_syms.insert(*sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        child_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                        NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                let mut sym_visiting = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        continue;
                    }
                    if sub.id.0 != 0 && !sym_visiting.insert(sub.id) {
                        continue;
                    }
                    max_pad = max_pad.max(max_object_effect_padding_inner(
                        doc,
                        sub,
                        world_xf,
                        Some(&child_map),
                        &mut sym_visiting,
                        visiting_syms,
                        depth + 1,
                    ));
                    if sub.id.0 != 0 {
                        sym_visiting.remove(&sub.id);
                    }
                }
            }
            visiting_syms.remove(sym_id);
        }
        NodePayload::Clone { .. } => {
            if let Some((p, f, o)) = doc.resolve_clone_payload_and_style_with_sym(obj, sym_map) {
                let mut tmp = obj.clone();
                tmp.payload = p;
                tmp.fill = f;
                tmp.outline = o;
                max_pad = max_pad.max(max_object_effect_padding_inner(
                    doc,
                    &tmp,
                    parent_xf,
                    sym_map,
                    visiting_objs,
                    visiting_syms,
                    depth + 1,
                ));
            }
        }
        _ => {}
    }
    if max_pad.is_finite() {
        max_pad
    } else {
        0.0
    }
}

fn estimate_scale(xf: &Affine2D) -> f64 {
    let sx = xf.m[0].hypot(xf.m[1]);
    let sy = xf.m[2].hypot(xf.m[3]);
    (0.5 * (sx + sy)).max(1e-4)
}

fn generate_two_color_pattern_tile(name: &str, fg: &Color, bg: &Color, size: u32) -> RgbaBuffer {
    let fg_px = fg.to_rgba8();
    let bg_px = bg.to_rgba8();
    let mut buf = RgbaBuffer::new_filled(size, size, bg_px);
    let kind = name.to_ascii_lowercase();
    let half = size / 2;

    for y in 0..size {
        for x in 0..size {
            let is_fg = if kind.contains("stripe") || kind.contains("line") {
                y % (size / 4).max(2) < (size / 8).max(1)
            } else if kind.contains("dot") || kind.contains("polka") {
                let dx = x as f64 - f64::from(half);
                let dy = y as f64 - f64::from(half);
                dx.hypot(dy) <= f64::from(size) * 0.25
            } else if kind.contains("cross") || kind.contains("hatch") || kind.contains("grid") {
                x < 4 || y < 4
            } else if kind.contains("diag") {
                ((x + y) % (size / 2).max(2)) < (size / 6).max(1)
            } else if kind.contains("brick") {
                let row_h = (size / 2).max(2);
                let offset = if (y / row_h) % 2 == 1 { half } else { 0 };
                y % row_h < 3 || ((x + offset) % size) < 3
            } else {
                // Default checkerboard
                (x < half) ^ (y < half)
            };
            if is_fg {
                buf.set_pixel(x, y, fg_px);
            }
        }
    }
    buf
}

/// Analytic resolution-independent PostScript procedural fill evaluator returning `0.0..=1.0`
/// (`0.0` = background color, `1.0` = foreground color) with smooth `aa_mm` anti-aliased edges.
pub(crate) fn eval_postscript_fill(
    preset: PostScriptFillPreset,
    seed: u64,
    pt: Point2D,
    center: Point2D,
    aa_mm: f64,
) -> f32 {
    let smoothstep = |edge0: f64, edge1: f64, x: f64| -> f64 {
        let t = ((x - edge0) / (edge1 - edge0).max(1e-9)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let sdf_fill = |signed_dist: f64| -> f32 {
        // Negative inside fg, positive outside fg
        (1.0 - smoothstep(-aa_mm * 0.5, aa_mm * 0.5, signed_dist)) as f32
    };

    // Deterministic seed phase/scale modulation (`seed == 0` stays canonical 1.0x unshifted)
    let phase_x = if seed == 0 {
        0.0
    } else {
        hash_grid(seed as i64, 17, seed) * 4.0
    };
    let phase_y = if seed == 0 {
        0.0
    } else {
        hash_grid(31, seed as i64, seed) * 4.0
    };
    let x = pt.x + phase_x;
    let y = pt.y + phase_y;

    match preset {
        PostScriptFillPreset::Hatch => {
            // Crisp 45° parallel hatching lines
            let period = 3.0;
            let half_stroke = 0.42;
            let u = (x + y) * std::f64::consts::FRAC_1_SQRT_2;
            let cell = u.rem_euclid(period) - period * 0.5;
            sdf_fill(cell.abs() - half_stroke)
        }
        PostScriptFillPreset::CrossHatch => {
            // Perpendicular +45° and -45° overlapping hatch lines
            let period = 3.2;
            let half_stroke = 0.36;
            let u = (x + y) * std::f64::consts::FRAC_1_SQRT_2;
            let v = (x - y) * std::f64::consts::FRAC_1_SQRT_2;
            let du = (u.rem_euclid(period) - period * 0.5).abs() - half_stroke;
            let dv = (v.rem_euclid(period) - period * 0.5).abs() - half_stroke;
            sdf_fill(du.min(dv))
        }
        PostScriptFillPreset::Checker => {
            // Crisp anti-aliased checkerboard squares
            let cell = 4.0;
            let gx = (x / cell).floor() as i64;
            let gy = (y / cell).floor() as i64;
            let fx = x.rem_euclid(cell);
            let fy = y.rem_euclid(cell);
            let edge_d = fx.min(cell - fx).min(fy.min(cell - fy));
            let is_fg = ((gx ^ gy) & 1) == 0;
            let signed = if is_fg { -edge_d } else { edge_d };
            sdf_fill(signed)
        }
        PostScriptFillPreset::PolkaDots => {
            // Staggered brick-lattice circular dots
            let pitch = 4.0;
            let radius = 1.15;
            let row = (y / pitch).floor() as i64;
            let stagger = if (row & 1) != 0 { pitch * 0.5 } else { 0.0 };
            let cx = (x + stagger).rem_euclid(pitch) - pitch * 0.5;
            let cy = y.rem_euclid(pitch) - pitch * 0.5;
            sdf_fill(cx.hypot(cy) - radius)
        }
        PostScriptFillPreset::Waves => {
            // Sinusoidal wave bands
            let pitch = 3.5;
            let amp = 0.95;
            let freq = std::f64::consts::TAU / 8.0;
            let warped_y = y + amp * (x * freq).sin();
            let cell = warped_y.rem_euclid(pitch) - pitch * 0.5;
            // Scale signed distance by gradient magnitude for uniform stroke thickness on slopes
            let slope = amp * freq * (x * freq).cos();
            let grad_len = (1.0 + slope * slope).sqrt();
            let dist = (cell.abs() - 0.72) / grad_len;
            sdf_fill(dist)
        }
        PostScriptFillPreset::Bricks => {
            // Running-bond masonry bricks (fg = brick face, bg = mortar joints)
            let brick_w = 8.0;
            let brick_h = 4.0;
            let mortar = 0.45;
            let row = (y / brick_h).floor() as i64;
            let offset = if (row & 1) != 0 { brick_w * 0.5 } else { 0.0 };
            let lx = (x + offset).rem_euclid(brick_w);
            let ly = y.rem_euclid(brick_h);
            let dx = lx.min(brick_w - lx);
            let dy = ly.min(brick_h - ly);
            let dist_from_joint = dx.min(dy);
            // Positive inside mortar -> bg; negative inside brick -> fg
            sdf_fill(mortar * 0.5 - dist_from_joint)
        }
        PostScriptFillPreset::RadialBurst => {
            // 16 alternating sunburst wedges radiating from the object center
            let rays = 16.0;
            let dx = pt.x - center.x;
            let dy = pt.y - center.y;
            let r = dx.hypot(dy);
            let angle = dy.atan2(dx) + phase_x * 0.2;
            let sector = std::f64::consts::TAU / rays;
            let local_a = angle.rem_euclid(sector) - sector * 0.5;
            // Convert angular distance to perpendicular mm distance for zoom-invariant crisp AA!
            let signed_mm = (local_a.abs() - sector * 0.25) * r.max(0.2);
            sdf_fill(signed_mm)
        }
        PostScriptFillPreset::HexGrid => {
            // Exact honeycomb hexagonal lattice wall SDF
            let s = 3.2; // hex circumradius-like scale
            let sqrt3 = 3.0_f64.sqrt();
            let px = x / s;
            let py = y / s;
            // Dual-lattice nearest hex center in normalized coordinates
            let r_w = 1.0;
            let r_h = sqrt3;
            let ax = px.rem_euclid(r_w) - 0.5 * r_w;
            let ay = py.rem_euclid(r_h) - 0.5 * r_h;
            let bx = (px - 0.5 * r_w).rem_euclid(r_w) - 0.5 * r_w;
            let by = (py - 0.5 * r_h).rem_euclid(r_h) - 0.5 * r_h;
            let (hx, hy) = if ax * ax + ay * ay < bx * bx + by * by {
                (ax.abs(), ay.abs())
            } else {
                (bx.abs(), by.abs())
            };
            // Distance from hex center to hex boundary (apothem = 0.5)
            let hex_d = hx.max(hx * 0.5 + hy * (sqrt3 * 0.5));
            let wall_dist_mm = (0.5 - hex_d) * s;
            let half_wall = 0.24;
            sdf_fill(wall_dist_mm - half_wall)
        }
        PostScriptFillPreset::Woodgrain => {
            // Warped concentric wood growth rings + fine longitudinal grain fibers
            let sx = (x - center.x) * 0.35;
            let sy = (y - center.y) * 0.12;
            let warp = fbm2d(x * 0.08, y * 0.08, seed.wrapping_add(73), 3) * 2.2;
            let ring_coord = (sx.hypot(sy) * 2.4 + warp).rem_euclid(1.0);
            let band = (ring_coord - 0.5).abs() * 2.0; // 0..1 sawtooth-like ring profile
            let grain = fbm2d(x * 0.6, y * 0.09, seed.wrapping_add(199), 2) * 0.22;
            let val = (smoothstep(0.18, 0.82, band) * 0.82 + grain).clamp(0.0, 1.0);
            val as f32
        }
        PostScriptFillPreset::HalftoneDots => {
            // Angled 45° PostScript halftone dot screen with smoothly modulated dot radius
            let pitch = 2.8;
            let u = (x + y) * std::f64::consts::FRAC_1_SQRT_2;
            let v = (x - y) * std::f64::consts::FRAC_1_SQRT_2;
            let cu = u.rem_euclid(pitch) - pitch * 0.5;
            let cv = v.rem_euclid(pitch) - pitch * 0.5;
            // Smooth spatial tone modulation across the shape (0.22..0.46 of pitch)
            let tone =
                0.5 + 0.38 * ((pt.x - center.x) * 0.12).sin() * ((pt.y - center.y) * 0.12).cos();
            let radius = pitch * (0.18 + 0.28 * tone.clamp(0.0, 1.0));
            sdf_fill(cu.hypot(cv) - radius)
        }
    }
}

/// Deterministic 2D value-noise FBM procedural texture evaluator returning `0.0..=1.0`.
pub(crate) fn eval_procedural_texture(preset: &str, seed: u64, x: f64, y: f64) -> f32 {
    let fx = x * 0.15;
    let fy = y * 0.15;
    let n = fbm2d(fx, fy, seed, 4);
    if preset.contains("marble") {
        (0.5 + 0.5 * (fx * 2.0 + n * 6.0).sin()).clamp(0.0, 1.0) as f32
    } else if preset.contains("wood") {
        let rings = ((fx * 0.6).hypot(fy * 0.6) * 4.0 + n * 2.5).fract();
        rings.clamp(0.0, 1.0) as f32
    } else if preset.contains("wave") {
        (0.5 + 0.5 * (fx * 1.5 + (fy * 1.5).cos() + n * 2.0).sin()).clamp(0.0, 1.0) as f32
    } else if preset.contains("granite") {
        fbm2d(x * 0.45, y * 0.45, seed, 3).clamp(0.0, 1.0) as f32
    } else {
        // Clouds / default smooth FBM
        n.clamp(0.0, 1.0) as f32
    }
}

fn fbm2d(x: f64, y: f64, seed: u64, octaves: u32) -> f64 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for o in 0..octaves {
        sum += amp * value_noise_2d(x * freq, y * freq, seed.wrapping_add(u64::from(o) * 1013));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    if norm > 0.0 {
        sum / norm
    } else {
        0.5
    }
}

fn value_noise_2d(x: f64, y: f64, seed: u64) -> f64 {
    let ix = x.floor() as i64;
    let iy = y.floor() as i64;
    let fx = x - ix as f64;
    let fy = y - iy as f64;
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);

    let v00 = hash_grid(ix, iy, seed);
    let v10 = hash_grid(ix + 1, iy, seed);
    let v01 = hash_grid(ix, iy + 1, seed);
    let v11 = hash_grid(ix + 1, iy + 1, seed);

    let top = v00 + (v10 - v00) * ux;
    let bot = v01 + (v11 - v01) * ux;
    top + (bot - top) * uy
}

fn hash_grid(ix: i64, iy: i64, seed: u64) -> f64 {
    let mut z = (ix as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (iy as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9)
        ^ seed.wrapping_mul(0x94d0_49bb_1331_11eb);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z as f64) / (u64::MAX as f64)
}

#[inline]
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        ColorMode, CornerStyle, Effect, FeatherDirection, ParametricShape, TextStyle,
    };

    #[test]
    fn text_drop_shadow_renders_non_glyph_shadow_pixels() {
        let mut plain_doc = Document::new("TextEffects", 80.0, 50.0, ColorMode::Rgb, 96.0);
        let txt = ArtisticText::from_plain(
            "HI",
            TextStyle {
                font_size_pt: 36.0,
                fill: Color::BLACK,
                ..TextStyle::default()
            },
        );
        let obj = SceneObject::new(ObjectId(0), "ShadowText", NodePayload::ArtisticText(txt))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(15.0, 25.0));
        let id = plain_doc.add_object(obj, None).unwrap();

        let mut fx_doc = plain_doc.clone();
        coree_core::apply_command(
            &mut fx_doc,
            &coree_core::CommandOp::AddEffect {
                id,
                effect: Effect::DropShadow {
                    dx_mm: 4.0,
                    dy_mm: 4.0,
                    blur_mm: 1.5,
                    opacity: 0.75,
                    color: Color::rgb(1.0, 0.0, 0.0),
                    feather_direction: FeatherDirection::Middle,
                },
            },
        )
        .unwrap();

        let mut vp = Viewport::new(320, 200);
        vp.fit_rect(Rect2D::from_xywh(0.0, 0.0, 80.0, 50.0), 4.0);
        let mut r = SceneRenderer::new();
        let mut b_plain = TinySkiaBackend::new(320, 200).unwrap();
        r.render_document(
            &plain_doc,
            &vp,
            ViewMode::Enhanced,
            &RenderOptions::default(),
            &mut b_plain,
        );
        let mut b_fx = TinySkiaBackend::new(320, 200).unwrap();
        r.render_document(
            &fx_doc,
            &vp,
            ViewMode::Enhanced,
            &RenderOptions::default(),
            &mut b_fx,
        );

        let rgba_fx = b_fx.into_rgba();
        let rgba_plain = b_plain.into_rgba();
        let red_shadow_px = rgba_fx
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(rgba_plain.rgba.as_chunks::<4>().0.iter())
            .filter(|(f, p)| f[0] > f[1].saturating_add(20) && p[1] > f[1].saturating_add(20))
            .count();
        assert!(
            red_shadow_px > 20,
            "red DropShadow on live ArtisticText should render red-tinted shadow pixels on white paper (diff={red_shadow_px})"
        );
    }

    #[test]
    fn high_step_blend_supports_up_to_999_steps() {
        let mut doc = Document::new("Blend999", 100.0, 60.0, ColorMode::Rgb, 96.0);
        let s1 = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "R1",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 10.0,
                        height: 10.0,
                        corner_radii: [0.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::Solid(Color::CYAN))
                .with_transform(Affine2D::translate(10.0, 20.0)),
                None,
            )
            .unwrap();
        let s2 = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "R2",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 10.0,
                        height: 10.0,
                        corner_radii: [0.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::Solid(Color::MAGENTA))
                .with_transform(Affine2D::translate(70.0, 20.0)),
                None,
            )
            .unwrap();
        coree_core::apply_command(
            &mut doc,
            &coree_core::CommandOp::AddEffect {
                id: s1,
                effect: Effect::Blend {
                    target_id: s2,
                    steps: 120,
                    rotation_deg: 0.0,
                    along_path: None,
                    rotate_along_path: true,
                },
            },
        )
        .unwrap();
        let mut next = 1000u64;
        let obj = doc.object(s1).unwrap();
        let lowered = crate::effects_render::lower_effects_to_primitives(
            &doc,
            obj,
            obj.transform,
            &mut || {
                next += 1;
                ObjectId(next)
            },
        );
        assert!(
            lowered.len() >= 120,
            "Blend should honor >64 steps up to 999 (got {})",
            lowered.len()
        );
    }

    #[test]
    fn shadow_sym_render_page_and_isolated_preserve_clones_chained_override_and_text_on_path() {
        let mut doc = Document::new("ShadowSym", 140.0, 60.0, ColorMode::Rgb, 72.0);
        let sym_id = SymbolId::new();
        let master_id = ObjectId(101);
        let clone_id = ObjectId(102);
        let clone2_id = ObjectId(103);
        let unlinked_id = ObjectId(104);
        let top_id = ObjectId(105);

        let master = SceneObject::new(
            master_id,
            "SymMaster",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 15.0, 20.0, 20.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(255, 0, 0, 255)))
        .with_outline(OutlineStyle::none());

        let mut clone1 = SceneObject::new(
            clone_id,
            "SymClone",
            NodePayload::Clone {
                master_id,
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_transform(Affine2D::translate(30.0, 0.0));
        clone1.effects.push(Effect::InnerShadow {
            dx_mm: 1.0,
            dy_mm: 1.0,
            blur_mm: 1.0,
            opacity: 0.5,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Middle,
        });

        let clone2 = SceneObject::new(
            clone2_id,
            "SymClone2",
            NodePayload::Clone {
                master_id: clone_id,
                link_fill: false,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_fill(Fill::Solid(Color::rgba8(0, 0, 255, 255)))
        .with_transform(Affine2D::translate(60.0, 0.0));

        let unlinked = SceneObject::new(
            unlinked_id,
            "SymUnlinked",
            NodePayload::Clone {
                master_id,
                link_fill: true,
                link_outline: true,
                link_shape: false,
            },
        )
        .with_transform(Affine2D::translate(90.0, 0.0));

        let top = SceneObject::new(
            top_id,
            "SymTop",
            NodePayload::TextOnPath(coree_core::TextOnPath {
                text: ArtisticText::from_plain(
                    "HHHH",
                    TextStyle {
                        font_size_pt: 18.0,
                        fill: Color::BLACK,
                        ..TextStyle::default()
                    },
                ),
                path_id: clone_id,
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );

        doc.upsert_symbol(SymbolDef {
            id: sym_id,
            name: "ShadowSym".into(),
            objects: vec![master, clone1, clone2, unlinked, top],
        });
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
                None,
            )
            .unwrap();

        let dpi = 72.0;
        let px_per_mm = dpi / 25.4;
        let rendered = crate::render_page_to_rgba(&doc, 0, dpi, ViewMode::Enhanced).unwrap();
        let sample_page = |x_mm: f64, y_mm: f64| -> [u8; 4] {
            let px = (x_mm * px_per_mm).round() as u32;
            let py = (y_mm * px_per_mm).round() as u32;
            let idx = ((py * rendered.width + px) * 4) as usize;
            [
                rendered.rgba[idx],
                rendered.rgba[idx + 1],
                rendered.rgba[idx + 2],
                rendered.rgba[idx + 3],
            ]
        };

        let p_master = sample_page(20.0, 25.0);
        let p_clone1 = sample_page(50.0, 25.0);
        let p_clone2 = sample_page(80.0, 25.0);
        let p_unlinked = sample_page(110.0, 25.0);
        assert!(
            p_master[0] > 180 && p_master[1] < 60 && p_master[2] < 60,
            "master={p_master:?}"
        );
        assert!(
            p_clone1[0] > 180 && p_clone1[1] < 60 && p_clone1[2] < 60,
            "clone1={p_clone1:?}"
        );
        assert!(
            p_clone2[2] > 180 && p_clone2[0] < 60 && p_clone2[1] < 60,
            "clone2={p_clone2:?}"
        );
        assert_eq!(p_unlinked, [255, 255, 255, 255], "unlinked={p_unlinked:?}");

        let top_dark_px = (0..20)
            .flat_map(|dx| {
                (0..12).map(move |dy| (40.5 + f64::from(dx) * 0.9, 9.5 + f64::from(dy) * 0.45))
            })
            .filter(|&(x, y)| {
                let p = sample_page(x, y);
                p[0] < 120 && p[1] < 120 && p[2] < 120
            })
            .count();
        assert!(
            top_dark_px > 5,
            "TextOnPath targeting SymClone missing in direct render: {top_dark_px}"
        );

        let (iso_rect, iso_buf) = crate::render_isolated_object_to_rgba_with_options(
            &doc,
            0,
            inst_id,
            Affine2D::IDENTITY,
            dpi,
            true,
        )
        .unwrap()
        .expect("isolated render");
        assert!(
            iso_rect.min_x <= 10.5 && iso_rect.max_x >= 89.5 && iso_rect.max_x < 105.0,
            "iso_rect={iso_rect:?}"
        );
        let sample_iso = |x_mm: f64, y_mm: f64| -> [u8; 4] {
            let px = (((x_mm - iso_rect.min_x) / iso_rect.width()) * f64::from(iso_buf.width))
                .floor()
                .clamp(0.0, f64::from(iso_buf.width - 1)) as u32;
            let py = (((y_mm - iso_rect.min_y) / iso_rect.height()) * f64::from(iso_buf.height))
                .floor()
                .clamp(0.0, f64::from(iso_buf.height - 1)) as u32;
            let idx = ((py * iso_buf.width + px) * 4) as usize;
            [
                iso_buf.rgba[idx],
                iso_buf.rgba[idx + 1],
                iso_buf.rgba[idx + 2],
                iso_buf.rgba[idx + 3],
            ]
        };
        let i_master = sample_iso(20.0, 25.0);
        let i_clone1 = sample_iso(50.0, 25.0);
        let i_clone2 = sample_iso(80.0, 25.0);
        assert!(
            i_master[0] > 180 && i_master[1] < 60 && i_master[3] > 200,
            "iso master={i_master:?}"
        );
        assert!(
            i_clone1[0] > 180 && i_clone1[1] < 60 && i_clone1[3] > 200,
            "iso clone1={i_clone1:?}"
        );
        assert!(
            i_clone2[2] > 180 && i_clone2[0] < 60 && i_clone2[3] > 200,
            "iso clone2={i_clone2:?}"
        );
    }
}
