// SPDX-License-Identifier: MPL-2.0

//! Complete scene graph, multipage/layer document model, unknown-chunk passthrough,
//! spatial R-Tree indexing, and document manipulation operations.

use crate::brand::VERSION;
use crate::color::{
    default_cmyk_palette, default_rgb_palette, Color, ColorMode, ColorPalette, ColorStyle,
};
use crate::dimension::{ConnectorObject, DimensionObject};
use crate::effects::{Effect, RasterFilter};
use crate::error::{CoreError, CoreResult};
use crate::geometry::{BezierPath, ParametricShape};
pub use crate::ids::{ImageAssetId, LayerId, ObjectId, ObjectStyleId, PageId, SymbolId};
use crate::lens::LensConfig;
use crate::math::{sanitize_f32, sanitize_f64, Affine2D, Point2D, Rect2D, Unit, Vec2D};
use crate::style::{BlendMode, Fill, ObjectStyle, OutlineStyle};
use crate::table::VectorTable;
use crate::text::{ArtisticText, ParagraphText, TextOnPath};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use rstar::{RTree, RTreeObject, AABB};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

/// Maximum recursion depth for nested groups / PowerClips / symbols to prevent stack overflow.
const MAX_SCENE_DEPTH: usize = 64;

/// Pure-Rust base64 serde adapter for binary payloads (`RasterAsset::rgba_bytes`
/// and `UnknownCdrChunk::raw_bytes`), also accepting JSON byte arrays on deserialize.
mod serde_bytes_base64 {
    use serde::de::{self, SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};
    use std::fmt;

    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = if chunk.len() > 1 {
                u32::from(chunk[1])
            } else {
                0
            };
            let b2 = if chunk.len() > 2 {
                u32::from(chunk[2])
            } else {
                0
            };
            let triple = (b0 << 16) | (b1 << 8) | b2;
            encoded.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
            encoded.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                encoded.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
            } else {
                encoded.push('=');
            }
            if chunk.len() > 2 {
                encoded.push(TABLE[(triple & 0x3F) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
        serializer.serialize_str(&encoded)
    }

    fn decode_val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    fn decode_b64(input: &str) -> Result<Vec<u8>, String> {
        let clean: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        if !clean.len().is_multiple_of(4) {
            return Err("invalid base64 length".into());
        }
        let mut out = Vec::with_capacity((clean.len() / 4) * 3);
        let (chunks, _) = clean.as_chunks::<4>();
        let num_chunks = chunks.len();
        for (idx, chunk) in chunks.iter().enumerate() {
            let v0 = decode_val(chunk[0]).ok_or("invalid base64 char")?;
            let v1 = decode_val(chunk[1]).ok_or("invalid base64 char")?;
            let pad2 = chunk[2] == b'=';
            let pad3 = chunk[3] == b'=';
            if pad2 && !pad3 {
                return Err("malformed base64 padding".into());
            }
            if (pad2 || pad3) && idx + 1 < num_chunks {
                return Err("unexpected base64 padding before final quantum".into());
            }
            let v2 = if pad2 {
                0
            } else {
                decode_val(chunk[2]).ok_or("invalid base64 char")?
            };
            let v3 = if pad3 {
                0
            } else {
                decode_val(chunk[3]).ok_or("invalid base64 char")?
            };
            let triple = (u32::from(v0) << 18)
                | (u32::from(v1) << 12)
                | (u32::from(v2) << 6)
                | u32::from(v3);
            out.push(((triple >> 16) & 0xFF) as u8);
            if !pad2 {
                out.push(((triple >> 8) & 0xFF) as u8);
            }
            if !pad3 {
                out.push((triple & 0xFF) as u8);
            }
        }
        Ok(out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        struct BytesOrB64Visitor;
        impl<'de> Visitor<'de> for BytesOrB64Visitor {
            type Value = Vec<u8>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a base64 string or byte sequence")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                decode_b64(v).map_err(E::custom)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(b) = seq.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(out)
            }
        }
        deserializer.deserialize_any(BytesOrB64Visitor)
    }
}

/// Embedded raster bitmap image stored in the document's asset table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RasterAsset {
    pub id: ImageAssetId,
    pub width: u32,
    pub height: u32,
    #[serde(with = "serde_bytes_base64")]
    pub rgba_bytes: Vec<u8>,
    pub source_dpi: f32,
    pub color_mode: ColorMode,
}

impl RasterAsset {
    /// Creates a new raster asset verifying byte buffer dimensions (`width * height * 4`).
    pub fn new(
        width: u32,
        height: u32,
        rgba_bytes: Vec<u8>,
        source_dpi: f32,
        color_mode: ColorMode,
    ) -> CoreResult<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| CoreError::InvalidParameter("raster dimensions overflow".into()))?;
        if rgba_bytes.len() != expected {
            return Err(CoreError::InvalidParameter(format!(
                "expected {expected} RGBA bytes for {width}x{height}, got {}",
                rgba_bytes.len()
            )));
        }
        Ok(Self {
            id: ImageAssetId::new(),
            width,
            height,
            rgba_bytes,
            source_dpi: sanitize_f32(source_dpi, 300.0).max(1.0),
            color_mode,
        })
    }
}

/// Raster image placement and non-destructive filter stack inside a [`SceneObject`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RasterLayerData {
    pub asset_id: ImageAssetId,
    pub width_mm: f64,
    pub height_mm: f64,
    pub filters: Vec<RasterFilter>,
    pub crop_rect: Option<Rect2D>,
}

/// Lossless passthrough container for unrecognized RIFF / `.cdr` binary chunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownCdrChunk {
    pub fourcc: [u8; 4],
    pub version_hint: u32,
    #[serde(with = "serde_bytes_base64")]
    pub raw_bytes: Vec<u8>,
}

impl UnknownCdrChunk {
    /// Returns the ASCII/lossy 4-character chunk identifier string.
    #[must_use]
    pub fn fourcc_str(&self) -> String {
        String::from_utf8_lossy(&self.fourcc).into_owned()
    }
}

/// Reusable vector symbol definition stored in [`Document::symbols`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolDef {
    pub id: SymbolId,
    pub name: String,
    pub objects: Vec<SceneObject>,
}

impl SymbolDef {
    /// Creates a new reusable symbol definition.
    #[must_use]
    pub fn new(name: impl Into<String>, objects: Vec<SceneObject>) -> Self {
        Self {
            id: SymbolId::new(),
            name: name.into(),
            objects,
        }
    }
}

/// Concrete geometric or container payload carried by a [`SceneObject`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodePayload {
    Path(BezierPath),
    Shape(ParametricShape),
    ArtisticText(ArtisticText),
    ParagraphText(ParagraphText),
    TextOnPath(TextOnPath),
    Raster(RasterLayerData),
    Table(VectorTable),
    Dimension(DimensionObject),
    Connector(ConnectorObject),
    Group(Vec<ObjectId>),
    PowerClip {
        container: Box<BezierPath>,
        contents: Vec<ObjectId>,
    },
    SymbolInstance(SymbolId),
}

/// Core scene-graph node representing any drawable object, group, or PowerClip container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneObject {
    pub id: ObjectId,
    pub name: String,
    pub payload: NodePayload,
    pub transform: Affine2D,
    pub fill: Fill,
    pub outline: OutlineStyle,
    pub lens: Option<LensConfig>,
    pub effects: Vec<Effect>,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub visible: bool,
    pub locked: bool,
    pub printable: bool,
    pub style_id: Option<ObjectStyleId>,
    pub unknown_chunks: Vec<UnknownCdrChunk>,
}

impl SceneObject {
    /// Creates a new visible, unlocked, printable scene object with default styling.
    #[must_use]
    pub fn new(id: ObjectId, name: impl Into<String>, payload: NodePayload) -> Self {
        Self {
            id,
            name: name.into(),
            payload,
            transform: Affine2D::IDENTITY,
            fill: Fill::None,
            outline: OutlineStyle::default(),
            lens: None,
            effects: Vec::new(),
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            locked: false,
            printable: true,
            style_id: None,
            unknown_chunks: Vec::new(),
        }
    }

    /// Builder helper to set the fill.
    #[must_use]
    pub fn with_fill(mut self, fill: Fill) -> Self {
        self.fill = fill;
        self
    }

    /// Builder helper to set the outline.
    #[must_use]
    pub fn with_outline(mut self, outline: OutlineStyle) -> Self {
        self.outline = outline;
        self
    }

    /// Builder helper to set the transform.
    #[must_use]
    pub fn with_transform(mut self, transform: Affine2D) -> Self {
        self.transform = transform;
        self
    }
}

/// Master-layer sharing scope across multipage documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MasterLayerScope {
    #[default]
    None,
    AllPages,
    OddPages,
    EvenPages,
}

/// Layer within a page holding an ordered z-stack of top-level object IDs (index 0 = back).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub printable: bool,
    pub master_scope: MasterLayerScope,
    /// Marks a dedicated cutting/plotter layer (`CutContour` / dielines).
    pub is_cut_layer: bool,
    pub objects: Vec<ObjectId>,
}

impl Layer {
    /// Creates a standard visible, unlocked, printable layer.
    #[must_use]
    pub fn new(id: LayerId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            visible: true,
            locked: false,
            printable: true,
            master_scope: MasterLayerScope::None,
            is_cut_layer: false,
            objects: Vec::new(),
        }
    }
}

/// Orientation and placement of a canvas guide line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum GuideOrientation {
    Horizontal(f64),
    Vertical(f64),
    Angled { origin: Point2D, angle_deg: f64 },
}

/// Snappable document guide line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub id: u64,
    pub orientation: GuideOrientation,
    pub locked: bool,
}

/// Document grid visibility and snap configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridConfig {
    pub visible: bool,
    pub snap: bool,
    pub spacing_x_mm: f64,
    pub spacing_y_mm: f64,
    pub subdivisions: u32,
    pub show_dots: bool,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            visible: false,
            snap: false,
            spacing_x_mm: 10.0,
            spacing_y_mm: 10.0,
            subdivisions: 2,
            show_dots: false,
        }
    }
}

/// Projection mode for the interactive perspective drawing grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PerspectiveGridMode {
    OnePoint,
    #[default]
    TwoPoint,
    ThreePoint,
    Isometric,
}

/// Interactive perspective grid overlay configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerspectiveGridConfig {
    pub enabled: bool,
    pub mode: PerspectiveGridMode,
    pub horizon_y_mm: f64,
    pub vanishing_points: Vec<Point2D>,
    pub density: u32,
    pub opacity: f32,
}

impl Default for PerspectiveGridConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: PerspectiveGridMode::TwoPoint,
            horizon_y_mm: 100.0,
            vanishing_points: vec![Point2D::new(-40.0, 100.0), Point2D::new(250.0, 100.0)],
            density: 12,
            opacity: 0.35,
        }
    }
}

/// Single page inside a multipage [`Document`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: PageId,
    pub name: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub bleed_mm: f64,
    pub facing_pages: bool,
    pub layers: Vec<Layer>,
}

impl Page {
    /// Creates a page with one default `"Layer 1"`.
    #[must_use]
    pub fn new(
        id: PageId,
        name: impl Into<String>,
        width_mm: f64,
        height_mm: f64,
        bleed_mm: f64,
        default_layer_id: LayerId,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            width_mm: sanitize_f64(width_mm, 210.0).max(1.0),
            height_mm: sanitize_f64(height_mm, 297.0).max(1.0),
            bleed_mm: sanitize_f64(bleed_mm, 0.0).max(0.0),
            facing_pages: false,
            layers: vec![Layer::new(default_layer_id, "Layer 1")],
        }
    }

    /// Page trim box `[0, 0, width_mm, height_mm]`.
    #[must_use]
    pub fn trim_rect(&self) -> Rect2D {
        Rect2D::from_xywh(0.0, 0.0, self.width_mm, self.height_mm)
    }

    /// Page bleed box expanded by `bleed_mm` on all four sides.
    #[must_use]
    pub fn bleed_rect(&self) -> Rect2D {
        self.trim_rect().expand(self.bleed_mm.max(0.0))
    }
}

/// Authorship, timestamp, and format provenance metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub author: String,
    pub subject: String,
    pub keywords: Vec<String>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub source_format: Option<String>,
    pub app_version: String,
}

impl Default for DocumentMetadata {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            author: String::new(),
            subject: String::new(),
            keywords: Vec::new(),
            notes: String::new(),
            created_at: now,
            modified_at: now,
            source_format: None,
            app_version: VERSION.to_owned(),
        }
    }
}

/// Layer z-stack ordering operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZOrderOp {
    BringToFront,
    SendToBack,
    BringForward,
    SendBackward,
}

/// Alignment target operation for selected objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AlignOp {
    Left,
    CenterH,
    Right,
    Top,
    CenterV,
    Bottom,
    CenterPage,
}

/// Distribution spacing operation for 3+ selected objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DistributeOp {
    Left,
    CenterH,
    Right,
    SpacingH,
    Top,
    CenterV,
    Bottom,
    SpacingV,
}

/// Aggregated curve measurements powering the live Path Length & Area Meter.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PathMetrics {
    pub total_length_mm: f64,
    pub total_area_mm2: f64,
    pub node_count: usize,
    pub subpath_count: usize,
}

/// Entry stored inside [`SpatialIndex`] for fast R-Tree hit-testing and dirty-tile culling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpatialObjectEntry {
    pub id: ObjectId,
    pub z_order: usize,
    pub bounds: Rect2D,
}

impl RTreeObject for SpatialObjectEntry {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(
            [self.bounds.min_x, self.bounds.min_y],
            [self.bounds.max_x, self.bounds.max_y],
        )
    }
}

/// R-Tree accelerated spatial index built from a document page's visible objects.
#[derive(Debug, Clone)]
pub struct SpatialIndex {
    tree: RTree<SpatialObjectEntry>,
}

impl SpatialIndex {
    /// Returns all object IDs whose bounding box intersects `rect`, ordered top-to-bottom (frontmost first).
    #[must_use]
    pub fn query_rect(&self, rect: Rect2D) -> Vec<ObjectId> {
        if !rect.is_finite() {
            return Vec::new();
        }
        let env = AABB::from_corners([rect.min_x, rect.min_y], [rect.max_x, rect.max_y]);
        let mut hits: Vec<SpatialObjectEntry> = self
            .tree
            .locate_in_envelope_intersecting(env)
            .copied()
            .collect();
        hits.sort_by_key(|a| std::cmp::Reverse(a.z_order));
        hits.into_iter().map(|e| e.id).collect()
    }

    /// Returns all object IDs whose bounding box is within `tolerance_mm` of `pt`, frontmost first.
    #[must_use]
    pub fn query_point(&self, pt: Point2D, tolerance_mm: f64) -> Vec<ObjectId> {
        if !pt.is_finite() {
            return Vec::new();
        }
        let tol = sanitize_f64(tolerance_mm, 1.0).abs();
        self.query_rect(Rect2D::new(pt.x - tol, pt.y - tol, pt.x + tol, pt.y + tol))
    }

    /// Number of indexed objects.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tree.size()
    }

    /// Returns `true` if no objects are indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tree.size() == 0
    }
}

/// Root headless document model for `coree`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub title: String,
    pub color_mode: ColorMode,
    pub default_unit: Unit,
    pub dpi: f32,
    pub pages: Vec<Page>,
    pub active_page_index: usize,
    pub multipage_view: bool,
    pub objects: BTreeMap<ObjectId, SceneObject>,
    pub assets: BTreeMap<ImageAssetId, RasterAsset>,
    pub palettes: Vec<ColorPalette>,
    pub document_palette: ColorPalette,
    pub color_styles: Vec<ColorStyle>,
    pub object_styles: Vec<ObjectStyle>,
    pub symbols: BTreeMap<SymbolId, SymbolDef>,
    pub guides: Vec<Guide>,
    pub grid: GridConfig,
    pub perspective_grid: PerspectiveGridConfig,
    pub metadata: DocumentMetadata,
    pub unknown_doc_chunks: Vec<UnknownCdrChunk>,
    pub next_id: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self::new_a4("Untitled")
    }
}

impl Document {
    /// Creates a new document with custom page size (mm), color mode, and DPI.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        width_mm: f64,
        height_mm: f64,
        color_mode: ColorMode,
        dpi: f32,
    ) -> Self {
        let mut next_id = 1u64;
        let page_id = PageId(next_id);
        next_id += 1;
        let layer_id = LayerId(next_id);
        next_id += 1;

        let page = Page::new(page_id, "Page 1", width_mm, height_mm, 3.0, layer_id);
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            color_mode,
            default_unit: Unit::Millimeter,
            dpi: sanitize_f32(dpi, 300.0).max(1.0),
            pages: vec![page],
            active_page_index: 0,
            multipage_view: false,
            objects: BTreeMap::new(),
            assets: BTreeMap::new(),
            palettes: vec![default_cmyk_palette(), default_rgb_palette()],
            document_palette: ColorPalette::new("Document Palette", color_mode),
            color_styles: Vec::new(),
            object_styles: Vec::new(),
            symbols: BTreeMap::new(),
            guides: Vec::new(),
            grid: GridConfig::default(),
            perspective_grid: PerspectiveGridConfig::default(),
            metadata: DocumentMetadata::default(),
            unknown_doc_chunks: Vec::new(),
            next_id,
        }
    }

    /// Creates a standard A4 (`210 × 297 mm`) CMYK print document at 300 DPI.
    #[must_use]
    pub fn new_a4(title: impl Into<String>) -> Self {
        Self::new(title, 210.0, 297.0, ColorMode::Cmyk, 300.0)
    }

    /// Creates a standard US Letter (`215.9 × 279.4 mm`) CMYK document at 300 DPI.
    #[must_use]
    pub fn new_letter(title: impl Into<String>) -> Self {
        Self::new(title, 215.9, 279.4, ColorMode::Cmyk, 300.0)
    }

    /// Allocates the next deterministic monotonic numeric ID.
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        id
    }

    /// Allocates a new [`ObjectId`].
    pub fn alloc_object_id(&mut self) -> ObjectId {
        ObjectId(self.alloc_id())
    }

    /// Touches `metadata.modified_at`.
    pub fn touch(&mut self) {
        self.metadata.modified_at = Utc::now();
    }

    /// Returns a reference to the active page.
    #[must_use]
    pub fn active_page(&self) -> Option<&Page> {
        self.pages.get(self.active_page_index)
    }

    /// Returns a mutable reference to the active page.
    #[must_use]
    pub fn active_page_mut(&mut self) -> Option<&mut Page> {
        self.pages.get_mut(self.active_page_index)
    }

    /// Finds an immutable layer anywhere in the document by [`LayerId`].
    #[must_use]
    pub fn find_layer(&self, layer_id: LayerId) -> Option<&Layer> {
        self.pages
            .iter()
            .flat_map(|p| &p.layers)
            .find(|l| l.id == layer_id)
    }

    /// Finds a mutable layer anywhere in the document by [`LayerId`].
    #[must_use]
    pub fn find_layer_mut(&mut self, layer_id: LayerId) -> Option<&mut Layer> {
        self.pages
            .iter_mut()
            .flat_map(|p| &mut p.layers)
            .find(|l| l.id == layer_id)
    }

    /// Finds the [`LayerId`] containing top-level object `id`, if any.
    #[must_use]
    pub fn layer_of_object(&self, id: ObjectId) -> Option<LayerId> {
        for page in &self.pages {
            for layer in &page.layers {
                if layer.objects.contains(&id) {
                    return Some(layer.id);
                }
            }
        }
        None
    }

    /// Inserts `object` into the document and appends it to `layer_id` (or the active
    /// page's topmost unlocked layer when `layer_id` is `None`).
    ///
    /// If `object.id == ObjectId(0)`, a deterministic ID is allocated automatically.
    pub fn add_object(
        &mut self,
        mut object: SceneObject,
        layer_id: Option<LayerId>,
    ) -> CoreResult<ObjectId> {
        if object.id.0 == 0 {
            object.id = self.alloc_object_id();
        } else {
            self.next_id = self.next_id.max(object.id.0.saturating_add(1));
        }
        let id = object.id;

        let target_layer_id = match layer_id {
            Some(lid) => {
                if self.find_layer(lid).is_none() {
                    return Err(CoreError::LayerNotFound(lid));
                }
                lid
            }
            None => {
                let page = self.active_page().ok_or(CoreError::PageIndexOutOfRange {
                    index: self.active_page_index,
                    len: self.pages.len(),
                })?;
                page.layers
                    .iter()
                    .rev()
                    .find(|l| !l.locked)
                    .or_else(|| page.layers.last())
                    .map(|l| l.id)
                    .ok_or_else(|| {
                        CoreError::InvalidParameter("active page has no layers".into())
                    })?
            }
        };

        self.objects.insert(id, object);
        if let Some(layer) = self.find_layer_mut(target_layer_id) {
            if !layer.objects.contains(&id) {
                layer.objects.push(id);
            }
        }
        self.touch();
        Ok(id)
    }

    /// Removes `id` (and any nested Group/PowerClip descendants) from all layers,
    /// parent containers, and the object map.
    pub fn remove_object(&mut self, id: ObjectId) -> CoreResult<SceneObject> {
        let obj = self
            .objects
            .remove(&id)
            .ok_or(CoreError::ObjectNotFound(id))?;
        let mut to_remove = HashSet::new();
        to_remove.insert(id);
        self.collect_descendants_from_payload(&obj.payload, &mut to_remove, 0);
        for &cid in &to_remove {
            if cid != id {
                self.objects.remove(&cid);
            }
        }
        for page in &mut self.pages {
            for layer in &mut page.layers {
                layer.objects.retain(|oid| !to_remove.contains(oid));
            }
        }
        for other in self.objects.values_mut() {
            match &mut other.payload {
                NodePayload::Group(children)
                | NodePayload::PowerClip {
                    contents: children, ..
                } => {
                    children.retain(|cid| !to_remove.contains(cid));
                }
                _ => {}
            }
        }
        self.touch();
        Ok(obj)
    }

    fn collect_descendants_from_payload(
        &self,
        payload: &NodePayload,
        visited: &mut HashSet<ObjectId>,
        depth: usize,
    ) {
        if depth > MAX_SCENE_DEPTH {
            return;
        }
        let children: &[ObjectId] = match payload {
            NodePayload::Group(c) => c,
            NodePayload::PowerClip { contents, .. } => contents,
            _ => return,
        };
        for &cid in children {
            if visited.insert(cid) {
                if let Some(child) = self.objects.get(&cid) {
                    self.collect_descendants_from_payload(&child.payload, visited, depth + 1);
                }
            }
        }
    }

    /// Deep-duplicates `ids` (including nested Group / PowerClip children) translated by `offset`.
    pub fn duplicate_objects(
        &mut self,
        ids: &[ObjectId],
        offset: Vec2D,
    ) -> CoreResult<Vec<ObjectId>> {
        let mut created = Vec::with_capacity(ids.len());
        let delta_xf = Affine2D::translate(offset.x, offset.y);
        for &src_id in ids {
            let layer_id = self.layer_of_object(src_id);
            let new_id = self.duplicate_recursive(src_id, 0)?;
            if let Some(obj) = self.objects.get_mut(&new_id) {
                obj.transform = delta_xf.compose(&obj.transform);
            }
            if let Some(lid) = layer_id {
                if let Some(layer) = self.find_layer_mut(lid) {
                    layer.objects.push(new_id);
                }
            } else if let Some(page) = self.active_page_mut() {
                if let Some(layer) = page.layers.last_mut() {
                    layer.objects.push(new_id);
                }
            }
            created.push(new_id);
        }
        self.touch();
        Ok(created)
    }

    fn duplicate_recursive(&mut self, src_id: ObjectId, depth: usize) -> CoreResult<ObjectId> {
        if depth > MAX_SCENE_DEPTH {
            return Err(CoreError::CycleDetected(
                "scene nesting depth exceeded".into(),
            ));
        }
        let mut cloned = self
            .objects
            .get(&src_id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(src_id))?;
        let new_id = self.alloc_object_id();
        cloned.id = new_id;

        match &mut cloned.payload {
            NodePayload::Group(children) => {
                let orig_children = children.clone();
                let mut new_children = Vec::with_capacity(orig_children.len());
                for cid in orig_children {
                    new_children.push(self.duplicate_recursive(cid, depth + 1)?);
                }
                *children = new_children;
            }
            NodePayload::PowerClip { contents, .. } => {
                let orig_contents = contents.clone();
                let mut new_contents = Vec::with_capacity(orig_contents.len());
                for cid in orig_contents {
                    new_contents.push(self.duplicate_recursive(cid, depth + 1)?);
                }
                *contents = new_contents;
            }
            _ => {}
        }

        self.objects.insert(new_id, cloned);
        Ok(new_id)
    }

    /// Finds the parent container (`Group` or `PowerClip`) holding `child_id`, if any.
    #[must_use]
    pub fn parent_container_of(&self, child_id: ObjectId) -> Option<ObjectId> {
        for (&pid, obj) in &self.objects {
            let children: &[ObjectId] = match &obj.payload {
                NodePayload::Group(c) => c,
                NodePayload::PowerClip { contents, .. } => contents,
                _ => continue,
            };
            if children.contains(&child_id) {
                return Some(pid);
            }
        }
        None
    }

    /// Removes all IDs in `ids` from every layer and every parent container (`Group`/`PowerClip`).
    fn detach_from_parents(&mut self, ids: &HashSet<ObjectId>) {
        for page in &mut self.pages {
            for layer in &mut page.layers {
                layer.objects.retain(|oid| !ids.contains(oid));
            }
        }
        for obj in self.objects.values_mut() {
            match &mut obj.payload {
                NodePayload::Group(children)
                | NodePayload::PowerClip {
                    contents: children, ..
                } => {
                    children.retain(|cid| !ids.contains(cid));
                }
                _ => {}
            }
        }
    }

    /// Replaces `old_id` with `replacements` in its enclosing layer or parent container.
    fn replace_in_parent(&mut self, old_id: ObjectId, replacements: &[ObjectId]) {
        if let Some(lid) = self.layer_of_object(old_id) {
            if let Some(layer) = self.find_layer_mut(lid) {
                if let Some(pos) = layer.objects.iter().position(|&oid| oid == old_id) {
                    layer.objects.remove(pos);
                    for (i, &cid) in replacements.iter().enumerate() {
                        if !layer.objects.contains(&cid) {
                            layer.objects.insert(pos + i, cid);
                        }
                    }
                    return;
                }
            }
        }
        if let Some(pid) = self.parent_container_of(old_id) {
            if let Some(parent) = self.objects.get_mut(&pid) {
                let list = match &mut parent.payload {
                    NodePayload::Group(c) => Some(c),
                    NodePayload::PowerClip { contents, .. } => Some(contents),
                    _ => None,
                };
                if let Some(children) = list {
                    if let Some(pos) = children.iter().position(|&oid| oid == old_id) {
                        children.remove(pos);
                        for (i, &cid) in replacements.iter().enumerate() {
                            if !children.contains(&cid) {
                                children.insert(pos + i, cid);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Inserts `siblings` right after `anchor_id` inside `anchor_id`'s enclosing layer or parent container.
    fn insert_siblings_after(&mut self, anchor_id: ObjectId, siblings: &[ObjectId]) {
        if let Some(lid) = self.layer_of_object(anchor_id) {
            if let Some(layer) = self.find_layer_mut(lid) {
                let mut insert_pos = layer
                    .objects
                    .iter()
                    .position(|&oid| oid == anchor_id)
                    .map_or(layer.objects.len(), |p| p + 1);
                for &cid in siblings {
                    if !layer.objects.contains(&cid) {
                        layer.objects.insert(insert_pos, cid);
                        insert_pos += 1;
                    }
                }
                return;
            }
        }
        if let Some(pid) = self.parent_container_of(anchor_id) {
            if let Some(parent) = self.objects.get_mut(&pid) {
                let list = match &mut parent.payload {
                    NodePayload::Group(c) => Some(c),
                    NodePayload::PowerClip { contents, .. } => Some(contents),
                    _ => None,
                };
                if let Some(children) = list {
                    let mut insert_pos = children
                        .iter()
                        .position(|&oid| oid == anchor_id)
                        .map_or(children.len(), |p| p + 1);
                    for &cid in siblings {
                        if !children.contains(&cid) {
                            children.insert(insert_pos, cid);
                            insert_pos += 1;
                        }
                    }
                }
            }
        }
    }

    /// Groups `ids` into a new [`NodePayload::Group`] object placed in the first object's layer or parent container.
    pub fn group_objects(
        &mut self,
        ids: &[ObjectId],
        name: impl Into<String>,
    ) -> CoreResult<ObjectId> {
        if ids.is_empty() {
            return Err(CoreError::InvalidParameter(
                "cannot create an empty group".into(),
            ));
        }
        let mut unique_ids = Vec::with_capacity(ids.len());
        let mut seen = HashSet::new();
        for &id in ids {
            if !self.objects.contains_key(&id) {
                return Err(CoreError::ObjectNotFound(id));
            }
            if seen.insert(id) {
                unique_ids.push(id);
            }
        }

        let target_layer = self.layer_of_object(unique_ids[0]);
        let target_parent = if target_layer.is_none() {
            self.parent_container_of(unique_ids[0])
                .filter(|pid| !seen.contains(pid))
        } else {
            None
        };

        // Detach grouped children from all prior layers and parent containers
        self.detach_from_parents(&seen);

        let group_id = self.alloc_object_id();
        let group_obj = SceneObject::new(group_id, name, NodePayload::Group(unique_ids));

        if let Some(pid) = target_parent {
            self.objects.insert(group_id, group_obj);
            if let Some(parent) = self.objects.get_mut(&pid) {
                match &mut parent.payload {
                    NodePayload::Group(c) | NodePayload::PowerClip { contents: c, .. } => {
                        c.push(group_id);
                    }
                    _ => {}
                }
            }
            self.touch();
            Ok(group_id)
        } else {
            self.add_object(group_obj, target_layer)
        }
    }

    /// Ungroups `group_id`, baking the group's transform into its children and restoring
    /// them to the group's enclosing layer or parent container in z-order.
    pub fn ungroup_object(&mut self, group_id: ObjectId) -> CoreResult<Vec<ObjectId>> {
        let group = self
            .objects
            .get(&group_id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(group_id))?;
        let NodePayload::Group(children) = group.payload else {
            return Err(CoreError::InvalidParameter(format!(
                "{group_id} is not a Group"
            )));
        };

        let group_xf = group.transform;

        // Bake parent transform into children
        for &cid in &children {
            if let Some(child) = self.objects.get_mut(&cid) {
                child.transform = group_xf.compose(&child.transform);
            }
        }

        // Replace group entry in its layer or parent container with its children
        self.replace_in_parent(group_id, &children);
        self.objects.remove(&group_id);
        self.touch();
        Ok(children)
    }

    /// Reorders `id` within its layer according to `op`.
    pub fn set_z_order(&mut self, id: ObjectId, op: ZOrderOp) -> CoreResult<()> {
        if !self.objects.contains_key(&id) {
            return Err(CoreError::ObjectNotFound(id));
        }
        let lid = self.layer_of_object(id).ok_or_else(|| {
            CoreError::InvalidParameter(format!("{id} is not on a top-level layer"))
        })?;
        let layer = self
            .find_layer_mut(lid)
            .ok_or(CoreError::LayerNotFound(lid))?;
        let Some(pos) = layer.objects.iter().position(|&oid| oid == id) else {
            return Err(CoreError::ObjectNotFound(id));
        };
        let last = layer.objects.len().saturating_sub(1);
        match op {
            ZOrderOp::BringToFront => {
                let item = layer.objects.remove(pos);
                layer.objects.push(item);
            }
            ZOrderOp::SendToBack => {
                let item = layer.objects.remove(pos);
                layer.objects.insert(0, item);
            }
            ZOrderOp::BringForward => {
                if pos < last {
                    layer.objects.swap(pos, pos + 1);
                }
            }
            ZOrderOp::SendBackward => {
                if pos > 0 {
                    layer.objects.swap(pos, pos - 1);
                }
            }
        }
        self.touch();
        Ok(())
    }

    /// Aligns `ids` relative to their combined bounding box (or active page for `CenterPage`).
    pub fn align_objects(&mut self, ids: &[ObjectId], op: AlignOp) -> CoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        for &id in ids {
            if !self.objects.contains_key(&id) {
                return Err(CoreError::ObjectNotFound(id));
            }
        }

        let reference = if op == AlignOp::CenterPage {
            self.active_page()
                .map_or(Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0), Page::trim_rect)
        } else {
            let Some(bb) = self.combined_bounds(ids) else {
                return Ok(());
            };
            bb
        };

        for &id in ids {
            let Some(bb) = self.object_bounds(id) else {
                continue;
            };
            let (dx, dy) = match op {
                AlignOp::Left => (reference.min_x - bb.min_x, 0.0),
                AlignOp::CenterH => (reference.center().x - bb.center().x, 0.0),
                AlignOp::Right => (reference.max_x - bb.max_x, 0.0),
                AlignOp::Top => (0.0, reference.min_y - bb.min_y),
                AlignOp::CenterV => (0.0, reference.center().y - bb.center().y),
                AlignOp::Bottom => (0.0, reference.max_y - bb.max_y),
                AlignOp::CenterPage => (
                    reference.center().x - bb.center().x,
                    reference.center().y - bb.center().y,
                ),
            };
            if let Some(obj) = self.objects.get_mut(&id) {
                if !obj.locked {
                    obj.transform = Affine2D::translate(dx, dy).compose(&obj.transform);
                }
            }
        }
        self.touch();
        Ok(())
    }

    /// Evenly distributes 3 or more objects along the selected horizontal or vertical metric.
    pub fn distribute_objects(&mut self, ids: &[ObjectId], op: DistributeOp) -> CoreResult<()> {
        if ids.len() < 3 {
            return Ok(());
        }
        let mut items: Vec<(ObjectId, Rect2D)> = Vec::with_capacity(ids.len());
        for &id in ids {
            if !self.objects.contains_key(&id) {
                return Err(CoreError::ObjectNotFound(id));
            }
            if let Some(bb) = self.object_bounds(id) {
                items.push((id, bb));
            }
        }
        if items.len() < 3 {
            return Ok(());
        }

        let is_horizontal = matches!(
            op,
            DistributeOp::Left
                | DistributeOp::CenterH
                | DistributeOp::Right
                | DistributeOp::SpacingH
        );
        items.sort_by(|a, b| {
            let ka = if is_horizontal {
                a.1.center().x
            } else {
                a.1.center().y
            };
            let kb = if is_horizontal {
                b.1.center().x
            } else {
                b.1.center().y
            };
            ka.total_cmp(&kb)
        });

        let n = items.len();
        let deltas: Vec<(ObjectId, f64, f64)> = match op {
            DistributeOp::Left => {
                let start = items[0].1.min_x;
                let end = items[n - 1].1.min_x;
                let step = (end - start) / ((n - 1) as f64);
                items
                    .iter()
                    .enumerate()
                    .map(|(i, &(id, bb))| (id, start + step * (i as f64) - bb.min_x, 0.0))
                    .collect()
            }
            DistributeOp::CenterH => {
                let start = items[0].1.center().x;
                let end = items[n - 1].1.center().x;
                let step = (end - start) / ((n - 1) as f64);
                items
                    .iter()
                    .enumerate()
                    .map(|(i, &(id, bb))| (id, start + step * (i as f64) - bb.center().x, 0.0))
                    .collect()
            }
            DistributeOp::Right => {
                let start = items[0].1.max_x;
                let end = items[n - 1].1.max_x;
                let step = (end - start) / ((n - 1) as f64);
                items
                    .iter()
                    .enumerate()
                    .map(|(i, &(id, bb))| (id, start + step * (i as f64) - bb.max_x, 0.0))
                    .collect()
            }
            DistributeOp::SpacingH => {
                let min_x = items[0].1.min_x;
                let max_x = items[n - 1].1.max_x;
                let total_w: f64 = items.iter().map(|(_, bb)| bb.width()).sum();
                let gap = ((max_x - min_x) - total_w) / ((n - 1) as f64);
                let mut cursor = min_x;
                let mut out = Vec::with_capacity(n);
                for &(id, bb) in &items {
                    out.push((id, cursor - bb.min_x, 0.0));
                    cursor += bb.width() + gap;
                }
                out
            }
            DistributeOp::Top => {
                let start = items[0].1.min_y;
                let end = items[n - 1].1.min_y;
                let step = (end - start) / ((n - 1) as f64);
                items
                    .iter()
                    .enumerate()
                    .map(|(i, &(id, bb))| (id, 0.0, start + step * (i as f64) - bb.min_y))
                    .collect()
            }
            DistributeOp::CenterV => {
                let start = items[0].1.center().y;
                let end = items[n - 1].1.center().y;
                let step = (end - start) / ((n - 1) as f64);
                items
                    .iter()
                    .enumerate()
                    .map(|(i, &(id, bb))| (id, 0.0, start + step * (i as f64) - bb.center().y))
                    .collect()
            }
            DistributeOp::Bottom => {
                let start = items[0].1.max_y;
                let end = items[n - 1].1.max_y;
                let step = (end - start) / ((n - 1) as f64);
                items
                    .iter()
                    .enumerate()
                    .map(|(i, &(id, bb))| (id, 0.0, start + step * (i as f64) - bb.max_y))
                    .collect()
            }
            DistributeOp::SpacingV => {
                let min_y = items[0].1.min_y;
                let max_y = items[n - 1].1.max_y;
                let total_h: f64 = items.iter().map(|(_, bb)| bb.height()).sum();
                let gap = ((max_y - min_y) - total_h) / ((n - 1) as f64);
                let mut cursor = min_y;
                let mut out = Vec::with_capacity(n);
                for &(id, bb) in &items {
                    out.push((id, 0.0, cursor - bb.min_y));
                    cursor += bb.height() + gap;
                }
                out
            }
        };

        for (id, dx, dy) in deltas {
            if let Some(obj) = self.objects.get_mut(&id) {
                if !obj.locked {
                    obj.transform = Affine2D::translate(dx, dy).compose(&obj.transform);
                }
            }
        }
        self.touch();
        Ok(())
    }

    /// Converts `container_id` into a PowerClip clipping container holding `content_ids`.
    pub fn create_powerclip(
        &mut self,
        container_id: ObjectId,
        content_ids: &[ObjectId],
    ) -> CoreResult<ObjectId> {
        let container_obj = self
            .objects
            .get(&container_id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(container_id))?;

        // Extract clipping contour and any already-clipped children from container
        let (clip_path, existing_contents) = match &container_obj.payload {
            NodePayload::Path(p) => (p.clone(), Vec::new()),
            NodePayload::Shape(s) => (s.to_bezier_path(), Vec::new()),
            NodePayload::PowerClip {
                container,
                contents,
            } => ((**container).clone(), contents.clone()),
            _ => {
                return Err(CoreError::InvalidGeometry(
                    "PowerClip container must be a Path or ParametricShape".into(),
                ));
            }
        };
        let existing_set: HashSet<ObjectId> = existing_contents.iter().copied().collect();

        // Validate newly added contents, deduplicate against existing contents, and guard cycles
        let mut new_contents = Vec::new();
        for &cid in content_ids {
            if cid == container_id {
                return Err(CoreError::CycleDetected(
                    "object cannot be PowerClipped inside itself".into(),
                ));
            }
            if !self.objects.contains_key(&cid) {
                return Err(CoreError::ObjectNotFound(cid));
            }
            if self.contains_descendant(cid, container_id, 0) {
                return Err(CoreError::CycleDetected(
                    "PowerClip would create a cyclic hierarchy".into(),
                ));
            }
            if !existing_set.contains(&cid) && !new_contents.contains(&cid) {
                new_contents.push(cid);
            }
        }

        // Capture true world transforms of container & newly added contents BEFORE detaching
        // so nested PowerClips inside Groups/PowerClips preserve exact world placement.
        let container_world = self
            .world_transform_of(container_id)
            .unwrap_or(container_obj.transform);
        let child_worlds: Vec<(ObjectId, Affine2D)> = new_contents
            .iter()
            .filter_map(|&cid| {
                let xf = self
                    .world_transform_of(cid)
                    .or_else(|| self.objects.get(&cid).map(|o| o.transform))?;
                Some((cid, xf))
            })
            .collect();

        // Detach newly added content objects from prior layers and parent containers
        let content_set: HashSet<ObjectId> = new_contents.iter().copied().collect();
        self.detach_from_parents(&content_set);

        // Convert world-space transforms of newly added contents into container-local space
        let inv_container = container_world.inverse().unwrap_or(Affine2D::IDENTITY);
        for (cid, child_world) in child_worlds {
            if let Some(child) = self.objects.get_mut(&cid) {
                child.transform = inv_container.compose(&child_world);
            }
        }

        if let Some(obj) = self.objects.get_mut(&container_id) {
            let mut merged_contents = existing_contents;
            merged_contents.extend(new_contents);
            obj.payload = NodePayload::PowerClip {
                container: Box::new(clip_path),
                contents: merged_contents,
            };
        }
        self.touch();
        Ok(container_id)
    }

    /// Extracts all clipped contents from `powerclip_id` back into its enclosing layer
    /// or parent container and restores `powerclip_id` to an ordinary [`NodePayload::Path`].
    pub fn extract_powerclip(&mut self, powerclip_id: ObjectId) -> CoreResult<Vec<ObjectId>> {
        let obj = self
            .objects
            .get(&powerclip_id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(powerclip_id))?;
        let NodePayload::PowerClip {
            container,
            contents,
        } = obj.payload
        else {
            return Err(CoreError::InvalidParameter(format!(
                "{powerclip_id} is not a PowerClip"
            )));
        };

        let parent_xf = obj.transform;

        for &cid in &contents {
            if let Some(child) = self.objects.get_mut(&cid) {
                child.transform = parent_xf.compose(&child.transform);
            }
        }

        if let Some(target) = self.objects.get_mut(&powerclip_id) {
            target.payload = NodePayload::Path(*container);
        }

        self.insert_siblings_after(powerclip_id, &contents);
        self.touch();
        Ok(contents)
    }

    fn contains_descendant(&self, root: ObjectId, target: ObjectId, depth: usize) -> bool {
        if depth > MAX_SCENE_DEPTH {
            return false;
        }
        let Some(obj) = self.objects.get(&root) else {
            return false;
        };
        let children: &[ObjectId] = match &obj.payload {
            NodePayload::Group(c) => c,
            NodePayload::PowerClip { contents, .. } => contents,
            _ => return false,
        };
        for &cid in children {
            if cid == target || self.contains_descendant(cid, target, depth + 1) {
                return true;
            }
        }
        false
    }

    /// Converts a shape, table, connector, or PowerClip container into an editable [`NodePayload::Path`].
    ///
    /// When converting a [`NodePayload::PowerClip`], any clipped child objects are recursively
    /// removed to avoid leaking orphaned entries (call [`Self::extract_powerclip`] first if you
    /// wish to preserve them).
    pub fn convert_to_curves(&mut self, id: ObjectId) -> CoreResult<()> {
        let obj = self
            .objects
            .get(&id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(id))?;
        let mut orphaned_contents: Vec<ObjectId> = Vec::new();
        let mut reset_transform = false;
        let path = match &obj.payload {
            NodePayload::Path(p) => p.clone(),
            NodePayload::Shape(shape) => shape.to_bezier_path(),
            NodePayload::Table(table) => table.to_grid_paths(),
            NodePayload::Connector(conn) => {
                let mut visiting = HashSet::new();
                visiting.insert(id);
                let (a, b) =
                    self.resolve_connector_endpoints_inner(conn, &obj.transform, 0, &mut visiting);
                let world_route = conn.route_path(a, b);
                if let Some(inv) = obj.transform.inverse() {
                    world_route.transformed(&inv)
                } else {
                    reset_transform = true;
                    world_route
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
            } => {
                orphaned_contents = contents.clone();
                (**container).clone()
            }
            NodePayload::ArtisticText(t) => BezierPath::from_rect(t.estimated_bounds()),
            NodePayload::ParagraphText(t) => BezierPath::from_rect(t.bounding_box()),
            _ => {
                return Err(CoreError::InvalidGeometry(format!(
                    "object {id} cannot be converted to curves"
                )));
            }
        };
        if let Some(target) = self.objects.get_mut(&id) {
            target.payload = NodePayload::Path(path);
            if reset_transform {
                target.transform = Affine2D::IDENTITY;
            }
        }
        for cid in orphaned_contents {
            if self.objects.contains_key(&cid) {
                let _ = self.remove_object(cid);
            }
        }
        self.touch();
        Ok(())
    }

    /// Resolves world-space endpoints for a [`ConnectorObject`].
    #[must_use]
    pub fn resolve_connector_endpoints(&self, conn: &ConnectorObject) -> (Point2D, Point2D) {
        self.resolve_connector_endpoints_with_transform(conn, &Affine2D::IDENTITY)
    }

    /// Resolves world-space endpoints for a [`ConnectorObject`] whose own transform is `world_xf`.
    #[must_use]
    pub fn resolve_connector_endpoints_with_transform(
        &self,
        conn: &ConnectorObject,
        world_xf: &Affine2D,
    ) -> (Point2D, Point2D) {
        let mut visiting = HashSet::new();
        self.resolve_connector_endpoints_inner(conn, world_xf, 0, &mut visiting)
    }

    /// Computes the full composed world-space affine transform of `id`, walking up any
    /// ancestor `Group` or `PowerClip` containers. Returns `None` if `id` does not exist
    /// or has a non-finite/cyclic ancestor chain.
    #[must_use]
    pub fn world_transform_of(&self, id: ObjectId) -> Option<Affine2D> {
        let mut chain = Vec::new();
        let mut cur = id;
        let mut seen = HashSet::new();
        for _ in 0..=MAX_SCENE_DEPTH {
            if !seen.insert(cur) {
                return None;
            }
            let obj = self.objects.get(&cur)?;
            chain.push(obj.transform);
            match self.parent_container_of(cur) {
                Some(pid) => cur = pid,
                None => break,
            }
        }
        let mut acc = Affine2D::IDENTITY;
        for xf in chain.into_iter().rev() {
            acc = acc.compose(&xf);
        }
        acc.is_finite().then_some(acc)
    }

    fn resolve_connector_endpoints_inner(
        &self,
        conn: &ConnectorObject,
        world_xf: &Affine2D,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) -> (Point2D, Point2D) {
        let from_pt = conn
            .from_obj
            .and_then(|oid| self.object_bounds_inner(oid, &Affine2D::IDENTITY, depth + 1, visiting))
            .map_or_else(
                || world_xf.transform_point(conn.from_fallback),
                |bb| conn.from_anchor.resolve(bb),
            );
        let to_pt = conn
            .to_obj
            .and_then(|oid| self.object_bounds_inner(oid, &Affine2D::IDENTITY, depth + 1, visiting))
            .map_or_else(
                || world_xf.transform_point(conn.to_fallback),
                |bb| conn.to_anchor.resolve(bb),
            );
        (from_pt, to_pt)
    }

    /// Computes the world-space axis-aligned bounding box of `id`, accounting for its
    /// transform and any nested group children. Returns `None` if empty, cyclic, or non-finite.
    #[must_use]
    pub fn object_bounds(&self, id: ObjectId) -> Option<Rect2D> {
        let mut visiting = HashSet::new();
        self.object_bounds_inner(id, &Affine2D::IDENTITY, 0, &mut visiting)
    }

    fn object_bounds_inner(
        &self,
        id: ObjectId,
        parent_xf: &Affine2D,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
    ) -> Option<Rect2D> {
        if depth > MAX_SCENE_DEPTH || !parent_xf.is_finite() || !visiting.insert(id) {
            return None;
        }
        let obj = match self.objects.get(&id) {
            Some(o) => o,
            None => {
                visiting.remove(&id);
                return None;
            }
        };
        let world_xf = parent_xf.compose(&obj.transform);
        if !world_xf.is_finite() {
            visiting.remove(&id);
            return None;
        }

        let bounds = match &obj.payload {
            NodePayload::Path(p) => p.transformed(&world_xf).bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().transformed(&world_xf).bounding_box(),
            NodePayload::ArtisticText(t) => world_xf.try_transform_rect(t.estimated_bounds()),
            NodePayload::ParagraphText(t) => world_xf.try_transform_rect(t.bounding_box()),
            NodePayload::TextOnPath(top) => self
                .world_path_of(top.path_id)
                .and_then(|wp| wp.bounding_box())
                .or_else(|| {
                    self.object_bounds_inner(top.path_id, &Affine2D::IDENTITY, depth + 1, visiting)
                })
                .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds())),
            NodePayload::Raster(r) => {
                let rect = match r.crop_rect {
                    Some(cr) if cr.is_finite() => Some(cr),
                    Some(_) => None,
                    None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                        Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                    }
                    None => None,
                };
                rect.and_then(|rc| world_xf.try_transform_rect(rc))
            }
            NodePayload::Table(t) => world_xf.try_transform_rect(t.to_bounding_box()),
            NodePayload::Dimension(d) => world_xf.try_transform_rect(d.bounding_box()),
            NodePayload::Connector(c) => {
                let (a, b) = self.resolve_connector_endpoints_inner(c, &world_xf, depth, visiting);
                c.route_path(a, b).bounding_box()
            }
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                for &cid in children {
                    if let Some(bb) = self.object_bounds_inner(cid, &world_xf, depth + 1, visiting)
                    {
                        acc = Some(match acc {
                            Some(prev) => prev.union(bb),
                            None => bb,
                        });
                    }
                }
                acc
            }
            NodePayload::PowerClip { container, .. } => {
                container.transformed(&world_xf).bounding_box()
            }
            NodePayload::SymbolInstance(sym_id) => self.symbols.get(sym_id).and_then(|sym| {
                let mut acc: Option<Rect2D> = None;
                for sub in &sym.objects {
                    let sub_xf = world_xf.compose(&sub.transform);
                    if !sub_xf.is_finite() {
                        continue;
                    }
                    let bb = match &sub.payload {
                        NodePayload::Path(p) => p.transformed(&sub_xf).bounding_box(),
                        NodePayload::Shape(s) => {
                            s.to_bezier_path().transformed(&sub_xf).bounding_box()
                        }
                        _ => None,
                    };
                    if let Some(r) = bb.filter(|r| r.is_finite()) {
                        acc = Some(match acc {
                            Some(prev) => prev.union(r),
                            None => r,
                        });
                    }
                }
                acc
            }),
        };
        visiting.remove(&id);
        bounds.filter(|r| r.is_finite())
    }

    /// Combined world-space bounding box enclosing all objects in `ids`.
    #[must_use]
    pub fn combined_bounds(&self, ids: &[ObjectId]) -> Option<Rect2D> {
        let mut acc: Option<Rect2D> = None;
        for &id in ids {
            if let Some(bb) = self.object_bounds(id) {
                acc = Some(match acc {
                    Some(prev) => prev.union(bb),
                    None => bb,
                });
            }
        }
        acc
    }

    /// Computes live path length (mm), enclosed area (mm²), node count, and subpath count
    /// across `ids` (expanding groups/shapes and running in parallel via `rayon`).
    #[must_use]
    pub fn path_metrics(&self, ids: &[ObjectId]) -> PathMetrics {
        let mut flat_paths: Vec<BezierPath> = Vec::new();
        for &id in ids {
            self.collect_world_paths(id, &Affine2D::IDENTITY, 0, &mut flat_paths);
        }

        let (len, area, nodes, subpaths) = flat_paths
            .par_iter()
            .map(|p| {
                (
                    p.path_length_mm(),
                    p.area_mm2(),
                    p.node_count(),
                    p.subpaths.len(),
                )
            })
            .reduce(
                || (0.0, 0.0, 0, 0),
                |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2, a.3 + b.3),
            );

        PathMetrics {
            total_length_mm: len,
            total_area_mm2: area,
            node_count: nodes,
            subpath_count: subpaths,
        }
    }

    /// Extracts world-space [`BezierPath`] representation of `id` if it is a curve/shape,
    /// including any ancestor `Group` or `PowerClip` transforms.
    #[must_use]
    pub fn world_path_of(&self, id: ObjectId) -> Option<BezierPath> {
        let obj = self.objects.get(&id)?;
        let local = match &obj.payload {
            NodePayload::Path(p) => p.clone(),
            NodePayload::Shape(s) => s.to_bezier_path(),
            NodePayload::PowerClip { container, .. } => (**container).clone(),
            _ => return None,
        };
        let world_xf = self.world_transform_of(id).unwrap_or(obj.transform);
        Some(local.transformed(&world_xf))
    }

    fn collect_world_paths(
        &self,
        id: ObjectId,
        parent_xf: &Affine2D,
        depth: usize,
        out: &mut Vec<BezierPath>,
    ) {
        if depth > MAX_SCENE_DEPTH {
            return;
        }
        let Some(obj) = self.objects.get(&id) else {
            return;
        };
        let world_xf = parent_xf.compose(&obj.transform);
        match &obj.payload {
            NodePayload::Path(p) => out.push(p.transformed(&world_xf)),
            NodePayload::Shape(s) => out.push(s.to_bezier_path().transformed(&world_xf)),
            NodePayload::PowerClip { container, .. } => out.push(container.transformed(&world_xf)),
            NodePayload::Group(children) => {
                for &cid in children {
                    self.collect_world_paths(cid, &world_xf, depth + 1, out);
                }
            }
            _ => {}
        }
    }

    /// Builds an R-Tree [`SpatialIndex`] for all visible, unlocked objects on `page_index`.
    #[must_use]
    pub fn build_spatial_index(&self, page_index: usize) -> SpatialIndex {
        let Some(page) = self.pages.get(page_index) else {
            return SpatialIndex { tree: RTree::new() };
        };
        let mut entries = Vec::new();
        let mut z = 0usize;
        for layer in &page.layers {
            if !layer.visible {
                continue;
            }
            for &oid in &layer.objects {
                if let Some(obj) = self.objects.get(&oid) {
                    if obj.visible {
                        if let Some(bounds) = self.object_bounds(oid).filter(|b| b.is_finite()) {
                            entries.push(SpatialObjectEntry {
                                id: oid,
                                z_order: z,
                                bounds,
                            });
                        }
                    }
                }
                z += 1;
            }
        }
        SpatialIndex {
            tree: RTree::bulk_load(entries),
        }
    }

    /// Returns the topmost visible, unlocked object hit near `pt` on the active page,
    /// skipping objects residing on locked or invisible layers.
    #[must_use]
    pub fn hit_test(&self, pt: Point2D, tolerance_mm: f64) -> Option<ObjectId> {
        let mut locked_layer_ids = HashSet::new();
        if let Some(page) = self.pages.get(self.active_page_index) {
            for layer in &page.layers {
                if layer.locked || !layer.visible {
                    locked_layer_ids.extend(layer.objects.iter().copied());
                }
            }
        }
        let index = self.build_spatial_index(self.active_page_index);
        for oid in index.query_point(pt, tolerance_mm) {
            if locked_layer_ids.contains(&oid) {
                continue;
            }
            if let Some(obj) = self.objects.get(&oid) {
                if obj.visible && !obj.locked {
                    return Some(oid);
                }
            }
        }
        None
    }

    // -------------------------------------------------------------------------
    // Pages, Layers, Guides, Assets, Palettes
    // -------------------------------------------------------------------------

    /// Computes the tight world bounding box of all visible objects on `page_idx`,
    /// shifts all objects on that page so the visible content's top-left corner sits at
    /// `(margin_mm, margin_mm)`, resizes the page to `(content_w + 2*margin_mm, content_h + 2*margin_mm)`
    /// clamped to `[1.0, 50_000.0]` mm, and returns the new `(width_mm, height_mm)`.
    pub fn autofit_page_to_content(
        &mut self,
        page_idx: usize,
        margin_mm: f64,
    ) -> CoreResult<(f64, f64)> {
        if page_idx >= self.pages.len() {
            return Err(CoreError::PageIndexOutOfRange {
                index: page_idx,
                len: self.pages.len(),
            });
        }
        let margin = sanitize_f64(margin_mm, 0.0).clamp(0.0, 5_000.0);

        let mut visible_ids = Vec::new();
        let mut all_page_obj_ids = Vec::new();
        let mut seen = HashSet::new();
        for layer in &self.pages[page_idx].layers {
            for &oid in &layer.objects {
                if seen.insert(oid) {
                    all_page_obj_ids.push(oid);
                    if layer.visible {
                        if let Some(obj) = self.objects.get(&oid) {
                            if obj.visible {
                                visible_ids.push(oid);
                            }
                        }
                    }
                }
            }
        }

        let bounds = self.combined_bounds(&visible_ids).ok_or_else(|| {
            CoreError::InvalidGeometry("page has no visible content to autofit".into())
        })?;
        if !bounds.is_finite() {
            return Err(CoreError::InvalidGeometry(
                "visible content bounds are non-finite".into(),
            ));
        }

        let dx = margin - bounds.min_x;
        let dy = margin - bounds.min_y;
        let shift = Affine2D::translate(dx, dy);
        for oid in all_page_obj_ids {
            if let Some(obj) = self.objects.get_mut(&oid) {
                obj.transform = shift.compose(&obj.transform);
            }
        }

        let new_w = (bounds.width() + 2.0 * margin).clamp(1.0, 50_000.0);
        let new_h = (bounds.height() + 2.0 * margin).clamp(1.0, 50_000.0);
        if let Some(page) = self.pages.get_mut(page_idx) {
            page.width_mm = new_w;
            page.height_mm = new_h;
        }
        self.touch();
        Ok((new_w, new_h))
    }

    /// Appends a new page and returns its [`PageId`].
    pub fn add_page(
        &mut self,
        name: impl Into<String>,
        width_mm: f64,
        height_mm: f64,
        bleed_mm: f64,
    ) -> PageId {
        let page_id = PageId(self.alloc_id());
        let layer_id = LayerId(self.alloc_id());
        self.pages.push(Page::new(
            page_id, name, width_mm, height_mm, bleed_mm, layer_id,
        ));
        self.touch();
        page_id
    }

    /// Deletes the page with `page_id` and cleans up its objects (fails if it's the last page).
    pub fn delete_page(&mut self, page_id: PageId) -> CoreResult<Page> {
        if self.pages.len() <= 1 {
            return Err(CoreError::CannotDeleteLastPage);
        }
        let idx = self
            .pages
            .iter()
            .position(|p| p.id == page_id)
            .ok_or(CoreError::PageNotFound(page_id))?;
        let removed = self.pages.remove(idx);
        let mut to_remove = HashSet::new();
        for layer in &removed.layers {
            for &oid in &layer.objects {
                if to_remove.insert(oid) {
                    if let Some(obj) = self.objects.get(&oid) {
                        self.collect_descendants_from_payload(&obj.payload, &mut to_remove, 0);
                    }
                }
            }
        }
        for oid in to_remove {
            self.objects.remove(&oid);
        }
        if self.active_page_index >= self.pages.len() {
            self.active_page_index = self.pages.len() - 1;
        }
        self.touch();
        Ok(removed)
    }

    /// Renames `page_id`.
    pub fn rename_page(&mut self, page_id: PageId, new_name: impl Into<String>) -> CoreResult<()> {
        let page = self
            .pages
            .iter_mut()
            .find(|p| p.id == page_id)
            .ok_or(CoreError::PageNotFound(page_id))?;
        page.name = new_name.into();
        self.touch();
        Ok(())
    }

    /// Adds a new layer to `page_id` and returns its [`LayerId`].
    pub fn add_layer(
        &mut self,
        page_id: PageId,
        name: impl Into<String>,
        is_cut_layer: bool,
        master_scope: MasterLayerScope,
    ) -> CoreResult<LayerId> {
        let layer_id = LayerId(self.alloc_id());
        let page = self
            .pages
            .iter_mut()
            .find(|p| p.id == page_id)
            .ok_or(CoreError::PageNotFound(page_id))?;
        let mut layer = Layer::new(layer_id, name);
        layer.is_cut_layer = is_cut_layer;
        layer.master_scope = master_scope;
        page.layers.push(layer);
        self.touch();
        Ok(layer_id)
    }

    /// Updates visibility/lock/print/cut-layer flags on `layer_id`.
    pub fn set_layer_flags(
        &mut self,
        layer_id: LayerId,
        visible: Option<bool>,
        locked: Option<bool>,
        printable: Option<bool>,
        is_cut_layer: Option<bool>,
    ) -> CoreResult<()> {
        let layer = self
            .find_layer_mut(layer_id)
            .ok_or(CoreError::LayerNotFound(layer_id))?;
        if let Some(v) = visible {
            layer.visible = v;
        }
        if let Some(l) = locked {
            layer.locked = l;
        }
        if let Some(p) = printable {
            layer.printable = p;
        }
        if let Some(c) = is_cut_layer {
            layer.is_cut_layer = c;
        }
        self.touch();
        Ok(())
    }

    /// Adds a new guide line and returns its guide ID.
    pub fn add_guide(&mut self, orientation: GuideOrientation, locked: bool) -> u64 {
        let id = self.alloc_id();
        self.guides.push(Guide {
            id,
            orientation,
            locked,
        });
        self.touch();
        id
    }

    /// Removes guide `#guide_id`.
    pub fn remove_guide(&mut self, guide_id: u64) -> CoreResult<Guide> {
        let pos = self
            .guides
            .iter()
            .position(|g| g.id == guide_id)
            .ok_or(CoreError::GuideNotFound(guide_id))?;
        let g = self.guides.remove(pos);
        self.touch();
        Ok(g)
    }

    /// Registers a raster image asset in the document and returns its [`ImageAssetId`].
    pub fn register_asset(&mut self, asset: RasterAsset) -> ImageAssetId {
        let id = asset.id;
        self.assets.insert(id, asset);
        self.touch();
        id
    }

    /// Scans all objects in the document and refreshes `self.document_palette` with every
    /// unique fill and outline color in use.
    pub fn refresh_document_palette(&mut self) {
        self.document_palette.swatches.clear();
        let mut colors: Vec<Color> = Vec::new();
        for obj in self.objects.values() {
            colors.extend(obj.fill.referenced_colors());
            if obj.outline.is_visible() {
                colors.push(obj.outline.color.clone());
            }
        }
        for color in colors {
            let label = match &color {
                Color::Spot { name, .. } => name.clone(),
                other => other.to_hex(),
            };
            self.document_palette.add_unique(label, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::CornerStyle;

    fn make_rect_obj(x: f64, y: f64, w: f64, h: f64, color: Color) -> SceneObject {
        SceneObject::new(
            ObjectId(0),
            "Rect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(color))
        .with_transform(Affine2D::translate(x, y))
    }

    #[test]
    fn document_scene_graph_group_align_distribute_powerclip_and_metrics() {
        let mut doc = Document::new_a4("Test Poster");
        let a = doc
            .add_object(make_rect_obj(0.0, 0.0, 20.0, 10.0, Color::CYAN), None)
            .unwrap();
        let b = doc
            .add_object(make_rect_obj(50.0, 30.0, 20.0, 10.0, Color::MAGENTA), None)
            .unwrap();
        let c = doc
            .add_object(make_rect_obj(120.0, 60.0, 20.0, 10.0, Color::YELLOW), None)
            .unwrap();

        // Metrics (3 rectangles: perimeter 60mm each = 180mm, area 200mm² each = 600mm²)
        let metrics = doc.path_metrics(&[a, b, c]);
        assert!((metrics.total_length_mm - 180.0).abs() < 0.1);
        assert!((metrics.total_area_mm2 - 600.0).abs() < 0.1);

        // Align Left
        doc.align_objects(&[a, b, c], AlignOp::Left).unwrap();
        assert!((doc.object_bounds(b).unwrap().min_x).abs() < 1e-6);
        assert!((doc.object_bounds(c).unwrap().min_x).abs() < 1e-6);

        // Distribute vertically
        doc.distribute_objects(&[a, b, c], DistributeOp::Top)
            .unwrap();
        assert!((doc.object_bounds(b).unwrap().min_y - 30.0).abs() < 1e-6);

        // Group & ungroup
        let grp = doc.group_objects(&[a, b], "Pair").unwrap();
        assert!(doc.objects.contains_key(&grp));
        let restored = doc.ungroup_object(grp).unwrap();
        assert_eq!(restored, vec![a, b]);

        // PowerClip & extract
        let pc = doc.create_powerclip(c, &[a]).unwrap();
        assert!(matches!(
            doc.objects[&pc].payload,
            NodePayload::PowerClip { .. }
        ));
        // Self-clipping cycle must fail
        assert!(doc.create_powerclip(pc, &[pc]).is_err());
        let extracted = doc.extract_powerclip(pc).unwrap();
        assert_eq!(extracted, vec![a]);

        // Convert to curves
        doc.convert_to_curves(b).unwrap();
        assert!(matches!(doc.objects[&b].payload, NodePayload::Path(_)));

        // Spatial R-Tree hit test
        assert_eq!(doc.hit_test(Point2D::new(5.0, 5.0), 1.0), Some(a));

        // Document palette refresh
        doc.refresh_document_palette();
        assert!(doc.document_palette.swatches.len() >= 3);
    }

    #[test]
    fn unknown_chunk_and_raster_asset_base64_roundtrip() {
        let mut doc = Document::new_a4("Passthrough");
        let asset = RasterAsset::new(2, 2, vec![255; 16], 300.0, ColorMode::Rgb).unwrap();
        let asset_id = doc.register_asset(asset);
        doc.unknown_doc_chunks.push(UnknownCdrChunk {
            fourcc: *b"vrsn",
            version_hint: 1800,
            raw_bytes: vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01],
        });

        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.assets[&asset_id].rgba_bytes, vec![255; 16]);
        assert_eq!(back.unknown_doc_chunks[0].fourcc_str(), "vrsn");
        assert_eq!(
            back.unknown_doc_chunks[0].raw_bytes,
            vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01]
        );
    }
}
