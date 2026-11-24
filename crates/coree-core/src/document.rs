// SPDX-License-Identifier: MIT OR Apache-2.0

//! Complete scene graph, multipage/layer document model, unknown-chunk passthrough,
//! spatial R-Tree indexing, and document manipulation operations.

use crate::brand::VERSION;
use crate::color::{
    default_cmyk_palette, default_rgb_palette, Color, ColorMode, ColorPalette, ColorStyle,
    ColorStyleId,
};
use crate::dimension::{AnchorPoint, ConnectorObject, DimensionObject};
use crate::effects::{DistortKind, Effect, EnvelopeMode, RasterFilter};
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
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

/// Maximum recursion depth for nested groups / PowerClips / symbols to prevent stack overflow.
const MAX_SCENE_DEPTH: usize = 64;

/// Encodes arbitrary bytes as standard unpadded/padded Base64.
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    serde_bytes_base64::encode_raw(bytes)
}

/// Decodes standard Base64 ASCII into raw bytes.
pub fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    serde_bytes_base64::decode_b64(input)
}

/// Pure-Rust base64 serde adapter for binary payloads (`RasterAsset::rgba_bytes`
/// and `UnknownCdrChunk::raw_bytes`), also accepting JSON byte arrays on deserialize.
mod serde_bytes_base64 {
    use serde::de::{self, SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};
    use std::fmt;

    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(super) fn encode_raw(bytes: &[u8]) -> String {
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
        encoded
    }

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_raw(bytes))
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

    pub(super) fn decode_b64(input: &str) -> Result<Vec<u8>, String> {
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
        #[serde(default)]
        container_parametric: Option<ParametricShape>,
        #[serde(default = "default_true_lock")]
        lock_contents: bool,
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

    /// Collects any [`ImageAssetId`] directly referenced by this object's payload or fill.
    pub fn collect_asset_refs(&self, out: &mut HashSet<ImageAssetId>) {
        match &self.payload {
            NodePayload::Raster(r) => {
                out.insert(r.asset_id);
            }
            NodePayload::Table(table) => {
                for cell in &table.cells {
                    if let Fill::BitmapPattern { image_id, .. } = cell.fill {
                        out.insert(image_id);
                    }
                }
            }
            _ => {}
        }
        if let Fill::BitmapPattern { image_id, .. } = self.fill {
            out.insert(image_id);
        }
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

const fn default_true_lock() -> bool {
    true
}

/// Optional base64 serde wrapper for `Option<Vec<u8>>` ICC binary profile bytes.
mod opt_serde_bytes_base64 {
    use super::serde_bytes_base64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(val: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match val {
            Some(bytes) => serde_bytes_base64::serialize(bytes, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        #[derive(Deserialize)]
        struct Helper(#[serde(with = "serde_bytes_base64")] Vec<u8>);
        Option::<Helper>::deserialize(d).map(|opt| opt.map(|h| h.0))
    }
}

/// Base64 serde wrapper for `BTreeMap<String, Vec<u8>>` unknown ZCF data blobs.
mod map_serde_bytes_base64 {
    use super::serde_bytes_base64;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    #[derive(Serialize, Deserialize)]
    struct BytesWrapper(#[serde(with = "serde_bytes_base64")] Vec<u8>);

    pub fn serialize<S: Serializer>(
        val: &BTreeMap<String, Vec<u8>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let wrapped: BTreeMap<&String, BytesWrapper> = val
            .iter()
            .map(|(k, v)| (k, BytesWrapper(v.clone())))
            .collect();
        wrapped.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<String, Vec<u8>>, D::Error> {
        let wrapped = BTreeMap::<String, BytesWrapper>::deserialize(d)?;
        Ok(wrapped.into_iter().map(|(k, v)| (k, v.0)).collect())
    }
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
    /// Marks a cross-page shared `Desktop` off-page holding layer.
    #[serde(default)]
    pub is_desktop_layer: bool,
    pub objects: Vec<ObjectId>,
    #[serde(default)]
    pub unknown_chunks: Vec<UnknownCdrChunk>,
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
            is_desktop_layer: false,
            objects: Vec::new(),
            unknown_chunks: Vec::new(),
        }
    }

    /// Creates a cross-page shared `"Desktop"` layer for off-page workspace objects.
    #[must_use]
    pub fn desktop(id: LayerId) -> Self {
        Self {
            id,
            name: "Desktop".to_owned(),
            visible: true,
            locked: false,
            printable: true,
            master_scope: MasterLayerScope::AllPages,
            is_cut_layer: false,
            is_desktop_layer: true,
            objects: Vec::new(),
            unknown_chunks: Vec::new(),
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
    #[serde(default)]
    pub unknown_chunks: Vec<UnknownCdrChunk>,
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
            unknown_chunks: Vec::new(),
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

/// Fitting strategy for [`Document::fit_powerclip`] and [`crate::command::CommandOp::FitPowerClip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PowerClipFitMode {
    FitContents,
    CenterContents,
    StretchToFill,
}

fn affine_cache_key(xf: &Affine2D) -> [u64; 6] {
    let norm = |v: f64| {
        if v == 0.0 {
            0.0_f64.to_bits()
        } else {
            v.to_bits()
        }
    };
    [
        norm(xf.m[0]),
        norm(xf.m[1]),
        norm(xf.m[2]),
        norm(xf.m[3]),
        norm(xf.m[4]),
        norm(xf.m[5]),
    ]
}

struct DocBoundsCtx {
    visiting_depth: HashMap<ObjectId, usize>,
    cache: HashMap<(ObjectId, [u64; 6]), Option<Rect2D>>,
    parent_world_cache: HashMap<ObjectId, Affine2D>,
}

impl DocBoundsCtx {
    fn new() -> Self {
        Self {
            visiting_depth: HashMap::new(),
            cache: HashMap::new(),
            parent_world_cache: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SymParentRef {
    Id(ObjectId),
    RootIndex(usize),
}

struct SymBoundsCtx<'a, 'b> {
    root_xf: &'a Affine2D,
    sym_objects: &'a [SceneObject],
    sym_map: &'a HashMap<ObjectId, &'a SceneObject>,
    sym_parent_map: &'a HashMap<ObjectId, SymParentRef>,
    visiting_sym_depth: &'b mut HashMap<ObjectId, usize>,
    sym_bounds_cache: &'b mut HashMap<(ObjectId, [u64; 6]), Option<Rect2D>>,
}

/// Root headless document model for `coree`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    id: Uuid,
    title: String,
    color_mode: ColorMode,
    default_unit: Unit,
    dpi: f32,
    pages: Vec<Page>,
    active_page_index: usize,
    multipage_view: bool,
    objects: BTreeMap<ObjectId, SceneObject>,
    assets: BTreeMap<ImageAssetId, RasterAsset>,
    palettes: Vec<ColorPalette>,
    document_palette: ColorPalette,
    color_styles: Vec<ColorStyle>,
    object_styles: Vec<ObjectStyle>,
    symbols: BTreeMap<SymbolId, SymbolDef>,
    guides: Vec<Guide>,
    grid: GridConfig,
    perspective_grid: PerspectiveGridConfig,
    metadata: DocumentMetadata,
    unknown_doc_chunks: Vec<UnknownCdrChunk>,
    #[serde(default, with = "map_serde_bytes_base64")]
    unknown_data_files: BTreeMap<String, Vec<u8>>,
    #[serde(default, with = "opt_serde_bytes_base64")]
    custom_icc_profile: Option<Vec<u8>>,
    #[serde(default)]
    custom_icc_name: Option<String>,
    next_id: u64,
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
            unknown_data_files: BTreeMap::new(),
            custom_icc_profile: None,
            custom_icc_name: None,
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

    // -------------------------------------------------------------------------
    // Encapsulation Getters & Validated Mutators (REM-10 / VGCore)
    // -------------------------------------------------------------------------

    /// Document UUID.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Document title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Document primary color mode.
    #[must_use]
    pub fn color_mode(&self) -> ColorMode {
        self.color_mode
    }

    /// Default display unit.
    #[must_use]
    pub fn default_unit(&self) -> Unit {
        self.default_unit
    }

    /// Document rasterization DPI.
    #[must_use]
    pub fn dpi(&self) -> f32 {
        self.dpi
    }

    /// Immutable slice of document pages.
    #[must_use]
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// Immutable reference to page at `index`.
    #[must_use]
    pub fn page(&self, index: usize) -> Option<&Page> {
        self.pages.get(index)
    }

    /// Immutable reference to page with `id`.
    #[must_use]
    pub fn page_by_id(&self, id: PageId) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// Zero-based active page index.
    #[must_use]
    pub fn active_page_index(&self) -> usize {
        self.active_page_index
    }

    /// Whether multipage canvas view is enabled.
    #[must_use]
    pub fn multipage_view(&self) -> bool {
        self.multipage_view
    }

    /// Immutable map of all scene objects keyed by [`ObjectId`].
    #[must_use]
    pub fn objects(&self) -> &BTreeMap<ObjectId, SceneObject> {
        &self.objects
    }

    /// Immutable reference to scene object `id`.
    #[must_use]
    pub fn object(&self, id: ObjectId) -> Option<&SceneObject> {
        self.objects.get(&id)
    }

    /// Returns `true` if `id` exists in the scene object map.
    #[must_use]
    pub fn has_object(&self, id: ObjectId) -> bool {
        self.objects.contains_key(&id)
    }

    /// Number of scene objects in the document.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Immutable map of embedded raster assets.
    #[must_use]
    pub fn assets(&self) -> &BTreeMap<ImageAssetId, RasterAsset> {
        &self.assets
    }

    /// Immutable reference to raster asset `id`.
    #[must_use]
    pub fn asset(&self, id: ImageAssetId) -> Option<&RasterAsset> {
        self.assets.get(&id)
    }

    /// Immutable slice of document color palettes.
    #[must_use]
    pub fn palettes(&self) -> &[ColorPalette] {
        &self.palettes
    }

    /// Immutable reference to the per-document swatch palette.
    #[must_use]
    pub fn document_palette(&self) -> &ColorPalette {
        &self.document_palette
    }

    /// Immutable slice of linked color styles.
    #[must_use]
    pub fn color_styles(&self) -> &[ColorStyle] {
        &self.color_styles
    }

    /// Finds a linked color style by [`ColorStyleId`].
    #[must_use]
    pub fn color_style(&self, id: ColorStyleId) -> Option<&ColorStyle> {
        self.color_styles.iter().find(|s| s.id == id)
    }

    /// Immutable slice of named object styles.
    #[must_use]
    pub fn object_styles(&self) -> &[ObjectStyle] {
        &self.object_styles
    }

    /// Finds a named object style by [`ObjectStyleId`].
    #[must_use]
    pub fn object_style(&self, id: ObjectStyleId) -> Option<&ObjectStyle> {
        self.object_styles.iter().find(|s| s.id == id)
    }

    /// Immutable map of reusable symbol definitions.
    #[must_use]
    pub fn symbols(&self) -> &BTreeMap<SymbolId, SymbolDef> {
        &self.symbols
    }

    /// Immutable reference to symbol definition `id`.
    #[must_use]
    pub fn symbol(&self, id: SymbolId) -> Option<&SymbolDef> {
        self.symbols.get(&id)
    }

    /// Immutable slice of document guides.
    #[must_use]
    pub fn guides(&self) -> &[Guide] {
        &self.guides
    }

    /// Immutable reference to a guide by ID.
    #[must_use]
    pub fn guide(&self, guide_id: u64) -> Option<&Guide> {
        self.guides.iter().find(|g| g.id == guide_id)
    }

    /// Document grid configuration.
    #[must_use]
    pub fn grid(&self) -> GridConfig {
        self.grid
    }

    /// Document perspective grid configuration.
    #[must_use]
    pub fn perspective_grid(&self) -> &PerspectiveGridConfig {
        &self.perspective_grid
    }

    /// Document metadata.
    #[must_use]
    pub fn metadata(&self) -> &DocumentMetadata {
        &self.metadata
    }

    /// Preserved unknown binary chunks from imported `.cdr` files.
    #[must_use]
    pub fn unknown_doc_chunks(&self) -> &[UnknownCdrChunk] {
        &self.unknown_doc_chunks
    }

    /// Preserved unknown external ZCF `content/data/*.dat` blobs keyed by relative archive path.
    #[must_use]
    pub fn unknown_data_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.unknown_data_files
    }

    /// Attached custom ICC profile bytes, if any.
    #[must_use]
    pub fn custom_icc_profile(&self) -> Option<&[u8]> {
        self.custom_icc_profile.as_deref()
    }

    /// Description name of the attached custom ICC profile, if any.
    #[must_use]
    pub fn custom_icc_name(&self) -> Option<&str> {
        self.custom_icc_name.as_deref()
    }

    /// Next monotonic ID counter value.
    #[must_use]
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Updates document title.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
        self.touch();
    }

    /// Updates document primary color mode.
    pub fn set_color_mode(&mut self, color_mode: ColorMode) {
        self.color_mode = color_mode;
        self.touch();
    }

    /// Updates default display unit.
    pub fn set_default_unit(&mut self, unit: Unit) {
        self.default_unit = unit;
        self.touch();
    }

    /// Updates document rasterization DPI (clamped to finite `>= 1.0`).
    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = sanitize_f32(dpi, 300.0).max(1.0);
        self.touch();
    }

    /// Sets the active page index after bounds validation.
    pub fn set_active_page_index(&mut self, index: usize) -> CoreResult<()> {
        if index >= self.pages.len() {
            return Err(CoreError::PageIndexOutOfRange {
                index,
                len: self.pages.len(),
            });
        }
        self.active_page_index = index;
        Ok(())
    }

    /// Enables or disables multipage view mode.
    pub fn set_multipage_view(&mut self, enabled: bool) {
        self.multipage_view = enabled;
    }

    /// Updates document grid configuration with finite/positive sanitization.
    pub fn set_grid(&mut self, mut grid: GridConfig) {
        grid.spacing_x_mm = sanitize_f64(grid.spacing_x_mm, 10.0).max(0.01);
        grid.spacing_y_mm = sanitize_f64(grid.spacing_y_mm, 10.0).max(0.01);
        grid.subdivisions = grid.subdivisions.max(1);
        self.grid = grid;
        self.touch();
    }

    /// Updates document perspective grid configuration with finite sanitization.
    pub fn set_perspective_grid(&mut self, mut config: PerspectiveGridConfig) {
        config.horizon_y_mm = sanitize_f64(config.horizon_y_mm, 100.0);
        config.vanishing_points.retain(|p| p.is_finite());
        config.density = config.density.max(1);
        config.opacity = sanitize_f32(config.opacity, 0.35).clamp(0.0, 1.0);
        self.perspective_grid = config;
        self.touch();
    }

    /// Mutable access to document authorship/provenance metadata.
    pub fn metadata_mut(&mut self) -> &mut DocumentMetadata {
        &mut self.metadata
    }

    /// Clears any attached custom ICC profile and profile name.
    pub fn clear_custom_icc_profile(&mut self) {
        self.custom_icc_profile = None;
        self.custom_icc_name = None;
        self.touch();
    }

    /// Mutable access to document palettes list.
    pub fn palettes_mut(&mut self) -> &mut Vec<ColorPalette> {
        &mut self.palettes
    }

    /// Mutable access to the per-document swatch palette.
    pub fn document_palette_mut(&mut self) -> &mut ColorPalette {
        &mut self.document_palette
    }

    /// Appends an unknown binary chunk to the document passthrough buffer.
    pub fn push_unknown_doc_chunk(&mut self, chunk: UnknownCdrChunk) {
        self.unknown_doc_chunks.push(chunk);
    }

    /// Appends multiple unknown binary chunks to the document passthrough buffer.
    pub fn extend_unknown_doc_chunks<I: IntoIterator<Item = UnknownCdrChunk>>(&mut self, iter: I) {
        self.unknown_doc_chunks.extend(iter);
    }

    /// Records an unrecognized ZCF `content/data/*.dat` blob for lossless round-trip passthrough.
    pub fn insert_unknown_data_file(&mut self, path: impl Into<String>, bytes: Vec<u8>) {
        self.unknown_data_files.insert(path.into(), bytes);
    }

    /// Replaces the unknown ZCF data-file passthrough map.
    pub fn replace_unknown_data_files(&mut self, files: BTreeMap<String, Vec<u8>>) {
        self.unknown_data_files = files;
    }

    /// Updates a guide's orientation (used during guide dragging/cancellation).
    pub fn set_guide_orientation(&mut self, guide_id: u64, orientation: GuideOrientation) -> bool {
        if let Some(g) = self.guides.iter_mut().find(|g| g.id == guide_id) {
            g.orientation = orientation;
            true
        } else {
            false
        }
    }

    /// Updates page dimensions at `page_index` with positive finite validation.
    pub fn set_page_dimensions(
        &mut self,
        page_index: usize,
        width_mm: f64,
        height_mm: f64,
    ) -> CoreResult<()> {
        if !width_mm.is_finite() || width_mm <= 0.0 || !height_mm.is_finite() || height_mm <= 0.0 {
            return Err(CoreError::InvalidParameter(
                "page dimensions must be positive finite numbers".into(),
            ));
        }
        let len = self.pages.len();
        let page = self
            .pages
            .get_mut(page_index)
            .ok_or(CoreError::PageIndexOutOfRange {
                index: page_index,
                len,
            })?;
        page.width_mm = width_mm;
        page.height_mm = height_mm;
        self.touch();
        Ok(())
    }

    /// Updates page bleed at `page_index` with non-negative finite validation.
    pub fn set_page_bleed(&mut self, page_index: usize, bleed_mm: f64) -> CoreResult<()> {
        if !bleed_mm.is_finite() || bleed_mm < 0.0 {
            return Err(CoreError::InvalidParameter(
                "page bleed must be a non-negative finite number".into(),
            ));
        }
        let len = self.pages.len();
        let page = self
            .pages
            .get_mut(page_index)
            .ok_or(CoreError::PageIndexOutOfRange {
                index: page_index,
                len,
            })?;
        page.bleed_mm = bleed_mm;
        self.touch();
        Ok(())
    }

    /// Mutable reference to page at `index`.
    #[must_use]
    pub fn page_mut(&mut self, index: usize) -> Option<&mut Page> {
        self.pages.get_mut(index)
    }

    /// Clears all pages prior to loading/imposing pages into a document.
    pub fn clear_pages_for_load(&mut self) {
        self.pages.clear();
        self.active_page_index = 0;
    }

    /// Appends a pre-constructed [`Page`] during import/imposition and syncs `next_id`.
    pub fn push_loaded_page(&mut self, page: Page) {
        self.next_id = self.next_id.max(page.id.0.saturating_add(1));
        for layer in &page.layers {
            self.next_id = self.next_id.max(layer.id.0.saturating_add(1));
        }
        self.pages.push(page);
    }

    /// Removes layer `layer_index` on `page_index` if in range and more than one layer exists.
    pub fn remove_page_layer(&mut self, page_index: usize, layer_index: usize) -> Option<Layer> {
        let page = self.pages.get_mut(page_index)?;
        if layer_index < page.layers.len() && page.layers.len() > 1 {
            Some(page.layers.remove(layer_index))
        } else {
            None
        }
    }

    /// Retains layers on `page_index` matching predicate `f` (preserving at least one if empty).
    pub fn retain_page_layers<F: FnMut(&Layer) -> bool>(&mut self, page_index: usize, f: F) {
        if let Some(page) = self.pages.get_mut(page_index) {
            page.layers.retain(f);
        }
    }

    /// Removes all empty, unlocked layers across pages that have at least one other layer.
    pub fn remove_empty_unlocked_layers(&mut self) -> usize {
        let mut removed = 0usize;
        for page in &mut self.pages {
            let mut idx = 0usize;
            while idx < page.layers.len() && page.layers.len() > 1 {
                if page.layers[idx].objects.is_empty() && !page.layers[idx].locked {
                    page.layers.remove(idx);
                    removed += 1;
                } else {
                    idx += 1;
                }
            }
        }
        if removed > 0 {
            self.touch();
        }
        removed
    }

    /// Inserts a pre-built object into `self.objects` without adding it to a layer
    /// (for nested `Group`/`PowerClip` children during import or deep page cloning).
    pub fn insert_loaded_object(&mut self, mut object: SceneObject) -> CoreResult<ObjectId> {
        if object.id.0 == 0 {
            object.id = self.alloc_object_id();
        } else {
            self.next_id = self.next_id.max(object.id.0.saturating_add(1));
        }
        let id = object.id;
        self.objects.insert(id, object);
        Ok(id)
    }

    /// Mutable access to an object during document load reference remapping.
    pub fn object_mut_for_load(&mut self, id: ObjectId) -> Option<&mut SceneObject> {
        self.objects.get_mut(&id)
    }

    /// Inserts `object` into `self.objects` only if its `ObjectId` is not already present.
    pub fn insert_snapshot_object_if_absent(&mut self, object: SceneObject) {
        self.next_id = self.next_id.max(object.id.0.saturating_add(1));
        self.objects.entry(object.id).or_insert(object);
    }

    /// Inserts or replaces `object` in `self.objects` during snapshot/delta recovery.
    pub fn upsert_snapshot_object(&mut self, object: SceneObject) {
        self.next_id = self.next_id.max(object.id.0.saturating_add(1));
        self.objects.insert(object.id, object);
    }

    /// Removes `id` directly from `self.objects` during differential snapshot recovery.
    pub fn remove_snapshot_object(&mut self, id: ObjectId) -> Option<SceneObject> {
        self.objects.remove(&id)
    }

    /// Replaces the document's color styles during snapshot/delta recovery.
    pub fn replace_color_styles(&mut self, styles: Vec<ColorStyle>) {
        self.color_styles = styles;
    }

    /// Replaces the document's object styles during snapshot/delta recovery.
    pub fn replace_object_styles(&mut self, styles: Vec<ObjectStyle>) {
        self.object_styles = styles;
    }

    /// Replaces the document's symbol definitions during snapshot/delta recovery.
    pub fn replace_symbols(&mut self, symbols: BTreeMap<SymbolId, SymbolDef>) {
        self.symbols = symbols;
    }

    /// Replaces the document's guides during snapshot/delta recovery.
    pub fn replace_guides(&mut self, guides: Vec<Guide>) {
        self.guides = guides;
    }

    /// Replaces the document's metadata during snapshot/delta recovery.
    pub fn set_metadata(&mut self, metadata: DocumentMetadata) {
        self.metadata = metadata;
    }

    /// Replaces the document's palette list during snapshot/delta recovery.
    pub fn replace_palettes(&mut self, palettes: Vec<ColorPalette>) {
        self.palettes = palettes;
    }

    /// Replaces the per-document swatch palette during snapshot/delta recovery.
    pub fn replace_document_palette(&mut self, palette: ColorPalette) {
        self.document_palette = palette;
    }

    /// Advances `next_id` to at least `min_next_id` during snapshot/delta recovery.
    pub fn ensure_next_id_at_least(&mut self, min_next_id: u64) {
        self.next_id = self.next_id.max(min_next_id);
    }

    /// Inserts `asset` only if its [`ImageAssetId`] is not already present.
    pub fn insert_asset_if_absent(&mut self, asset: RasterAsset) {
        self.assets.entry(asset.id).or_insert(asset);
    }

    /// Inserts or replaces `asset` keyed by `asset.id`.
    pub fn replace_asset(&mut self, asset: RasterAsset) {
        self.assets.insert(asset.id, asset);
        self.touch();
    }

    /// Removes a raster asset by [`ImageAssetId`].
    pub fn remove_asset(&mut self, id: ImageAssetId) -> Option<RasterAsset> {
        self.assets.remove(&id)
    }

    /// Extracts all assets out of the document (used for split native container saving).
    pub fn take_assets(&mut self) -> BTreeMap<ImageAssetId, RasterAsset> {
        std::mem::take(&mut self.assets)
    }

    /// Restores the document's asset table (used when loading split native containers).
    pub fn restore_assets(&mut self, assets: BTreeMap<ImageAssetId, RasterAsset>) {
        self.assets = assets;
    }

    /// Inserts or updates a symbol definition in `self.symbols`.
    pub fn insert_symbol(&mut self, symbol: SymbolDef) {
        self.symbols.insert(symbol.id, symbol);
    }

    /// Copies document-level configuration/resources (settings, assets, palettes, styles,
    /// symbols, grids, metadata) from `src` while keeping `self.id`, `self.title`, and pages.
    pub fn clone_shell_from(&mut self, src: &Document) {
        self.color_mode = src.color_mode;
        self.default_unit = src.default_unit;
        self.dpi = src.dpi;
        self.multipage_view = src.multipage_view;
        self.assets = src.assets.clone();
        self.palettes = src.palettes.clone();
        self.document_palette = src.document_palette.clone();
        self.color_styles = src.color_styles.clone();
        self.object_styles = src.object_styles.clone();
        self.symbols = src.symbols.clone();
        self.grid = src.grid;
        self.perspective_grid = src.perspective_grid.clone();
        self.metadata = src.metadata.clone();
        self.unknown_data_files = src.unknown_data_files.clone();
        self.custom_icc_profile = src.custom_icc_profile.clone();
        self.custom_icc_name = src.custom_icc_name.clone();
    }

    /// Updates `id`'s transform directly (intended for headless builders/imposition helpers).
    pub fn set_object_transform(&mut self, id: ObjectId, transform: Affine2D) -> CoreResult<()> {
        if !transform.is_finite() {
            return Err(CoreError::InvalidGeometry(
                "transform contains non-finite entries".into(),
            ));
        }
        let obj = self
            .objects
            .get_mut(&id)
            .ok_or(CoreError::ObjectNotFound(id))?;
        obj.transform = transform;
        self.touch();
        Ok(())
    }

    /// Replaces `old_child` with `new_child` inside a `Group` or `PowerClip` container `parent_id`.
    pub fn replace_container_child(
        &mut self,
        parent_id: ObjectId,
        old_child: ObjectId,
        new_child: ObjectId,
    ) -> bool {
        let Some(parent) = self.objects.get_mut(&parent_id) else {
            return false;
        };
        let list = match &mut parent.payload {
            NodePayload::Group(c) => Some(c),
            NodePayload::PowerClip { contents, .. } => Some(contents),
            _ => None,
        };
        if let Some(children) = list {
            if let Some(pos) = children.iter().position(|&id| id == old_child) {
                children[pos] = new_child;
            } else if !children.contains(&new_child) {
                children.push(new_child);
            }
            return true;
        }
        false
    }

    /// Remaps all cross-object [`ObjectId`] references inside every object in `id_map.values()`
    /// (used after cloning clipboard/page subtrees).
    pub fn remap_object_references(&mut self, id_map: &HashMap<ObjectId, ObjectId>) {
        let new_ids: Vec<ObjectId> = id_map.values().copied().collect();
        for nid in new_ids {
            if let Some(obj) = self.objects.get_mut(&nid) {
                Self::remap_single_object_refs(obj, id_map, false);
            }
        }
    }

    /// Returns the set of all effectively locked [`ObjectId`]s across the document:
    /// every object placed on a locked layer (`layer.locked == true`), every object with
    /// `obj.locked == true`, and all nested `Group` / `PowerClip` descendants thereof.
    #[must_use]
    pub fn effective_locked_object_ids(&self) -> HashSet<ObjectId> {
        fn mark_locked_subtree(
            objects: &BTreeMap<ObjectId, SceneObject>,
            id: ObjectId,
            depth: usize,
            visiting: &mut HashSet<ObjectId>,
            min_depth: &mut HashMap<ObjectId, usize>,
            locked: &mut HashSet<ObjectId>,
        ) {
            if depth > MAX_SCENE_DEPTH || !visiting.insert(id) {
                return;
            }
            if min_depth.get(&id).is_some_and(|&prev| prev <= depth) {
                visiting.remove(&id);
                return;
            }
            min_depth.insert(id, depth);
            locked.insert(id);
            if let Some(obj) = objects.get(&id) {
                let children: &[ObjectId] = match &obj.payload {
                    NodePayload::Group(c) => c,
                    NodePayload::PowerClip { contents, .. } => contents,
                    _ => &[],
                };
                for &cid in children {
                    mark_locked_subtree(objects, cid, depth + 1, visiting, min_depth, locked);
                }
            }
            visiting.remove(&id);
        }

        let mut locked = HashSet::new();
        let mut visiting = HashSet::new();
        let mut min_depth = HashMap::new();
        for page in &self.pages {
            for layer in &page.layers {
                if layer.locked {
                    for &oid in &layer.objects {
                        mark_locked_subtree(
                            &self.objects,
                            oid,
                            0,
                            &mut visiting,
                            &mut min_depth,
                            &mut locked,
                        );
                    }
                }
            }
        }
        for (&oid, obj) in &self.objects {
            if obj.locked {
                mark_locked_subtree(
                    &self.objects,
                    oid,
                    0,
                    &mut visiting,
                    &mut min_depth,
                    &mut locked,
                );
            }
        }
        locked
    }

    /// Returns `true` if `id` is directly locked (`obj.locked`), resides on a locked layer
    /// (`layer.locked`), or has a locked ancestor `Group` / `PowerClip` container.
    #[must_use]
    pub fn is_effectively_locked(&self, id: ObjectId) -> bool {
        if self.objects.get(&id).is_some_and(|o| o.locked) {
            return true;
        }
        self.effective_locked_object_ids().contains(&id)
    }

    /// Maps all [`Color`] values across effectively unlocked objects' fills, outlines, text runs,
    /// tables, and dimension annotations (skipping locked objects, objects on locked layers, and
    /// descendants of locked `Group`/`PowerClip` containers). Returns the number of colors modified.
    pub fn map_unlocked_object_colors<F: FnMut(&mut Color) -> bool>(&mut self, mut f: F) -> usize {
        fn map_fill_colors<F: FnMut(&mut Color) -> bool>(fill: &mut Fill, f: &mut F) -> usize {
            let mut count = 0usize;
            match fill {
                Fill::Solid(c) => {
                    if f(c) {
                        count += 1;
                    }
                }
                Fill::LinearGradient { stops, .. }
                | Fill::RadialGradient { stops, .. }
                | Fill::ConicalGradient { stops, .. }
                | Fill::SquareGradient { stops, .. } => {
                    for stop in stops {
                        if f(&mut stop.color) {
                            count += 1;
                        }
                    }
                }
                Fill::TwoColorPattern { fg, bg, .. } | Fill::TextureFill { fg, bg, .. } => {
                    if f(fg) {
                        count += 1;
                    }
                    if f(bg) {
                        count += 1;
                    }
                }
                Fill::MeshFill { nodes, .. } => {
                    for node in nodes {
                        if f(&mut node.color) {
                            count += 1;
                        }
                    }
                }
                Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {}
            }
            count
        }

        let locked_ids = self.effective_locked_object_ids();
        let mut converted = 0usize;
        for (id, obj) in &mut self.objects {
            if locked_ids.contains(id) {
                continue;
            }
            converted += map_fill_colors(&mut obj.fill, &mut f);
            if obj.outline.is_visible() && f(&mut obj.outline.color) {
                converted += 1;
            }
            match &mut obj.payload {
                NodePayload::ArtisticText(t) => {
                    for r in &mut t.runs {
                        if f(&mut r.style.fill) {
                            converted += 1;
                        }
                    }
                }
                NodePayload::ParagraphText(t) => {
                    for r in &mut t.runs {
                        if f(&mut r.style.fill) {
                            converted += 1;
                        }
                    }
                }
                NodePayload::TextOnPath(top) => {
                    for r in &mut top.text.runs {
                        if f(&mut r.style.fill) {
                            converted += 1;
                        }
                    }
                }
                NodePayload::Table(table) => {
                    for cell in &mut table.cells {
                        converted += map_fill_colors(&mut cell.fill, &mut f);
                        if f(&mut cell.text_style.fill) {
                            converted += 1;
                        }
                    }
                    if table.border_style.is_visible() && f(&mut table.border_style.color) {
                        converted += 1;
                    }
                }
                NodePayload::Dimension(dim) => {
                    if f(&mut dim.text_style.fill) {
                        converted += 1;
                    }
                }
                _ => {}
            }
        }
        if converted > 0 {
            self.touch();
        }
        converted
    }

    /// Recursively applies a text placeholder substitution function across `id` (and nested
    /// `Group`/`PowerClip` children) on text runs and table cells.
    pub fn substitute_object_text_and_table_placeholders<F: FnMut(&str) -> String>(
        &mut self,
        id: ObjectId,
        mut replacer: F,
    ) {
        self.substitute_object_text_inner(id, &mut replacer, 0);
    }

    fn substitute_object_text_inner<F: FnMut(&str) -> String>(
        &mut self,
        id: ObjectId,
        replacer: &mut F,
        depth: usize,
    ) {
        if depth > MAX_SCENE_DEPTH {
            return;
        }
        let children: Vec<ObjectId> = match self.objects.get_mut(&id) {
            None => return,
            Some(obj) => match &mut obj.payload {
                NodePayload::ArtisticText(t) => {
                    for run in &mut t.runs {
                        if run.text.contains("<<") {
                            run.text = replacer(&run.text);
                        }
                    }
                    Vec::new()
                }
                NodePayload::ParagraphText(t) => {
                    for run in &mut t.runs {
                        if run.text.contains("<<") {
                            run.text = replacer(&run.text);
                        }
                    }
                    Vec::new()
                }
                NodePayload::TextOnPath(top) => {
                    for run in &mut top.text.runs {
                        if run.text.contains("<<") {
                            run.text = replacer(&run.text);
                        }
                    }
                    Vec::new()
                }
                NodePayload::Table(table) => {
                    for cell in &mut table.cells {
                        cell.text = replacer(&cell.text);
                    }
                    Vec::new()
                }
                NodePayload::Group(c) | NodePayload::PowerClip { contents: c, .. } => c.clone(),
                _ => Vec::new(),
            },
        };
        for cid in children {
            self.substitute_object_text_inner(cid, replacer, depth + 1);
        }
    }

    // --- Crate-internal mutators for `command.rs` and `find_replace.rs` ---

    pub(crate) fn object_mut(&mut self, id: ObjectId) -> Option<&mut SceneObject> {
        self.objects.get_mut(&id)
    }

    pub(crate) fn objects_mut(&mut self) -> &mut BTreeMap<ObjectId, SceneObject> {
        &mut self.objects
    }

    pub(crate) fn pages_mut(&mut self) -> &mut Vec<Page> {
        &mut self.pages
    }

    pub(crate) fn guides_mut(&mut self) -> &mut Vec<Guide> {
        &mut self.guides
    }

    pub(crate) fn symbols_mut(&mut self) -> &mut BTreeMap<SymbolId, SymbolDef> {
        &mut self.symbols
    }

    pub(crate) fn color_styles_mut(&mut self) -> &mut Vec<ColorStyle> {
        &mut self.color_styles
    }

    pub(crate) fn object_styles_mut(&mut self) -> &mut Vec<ObjectStyle> {
        &mut self.object_styles
    }

    pub(crate) fn assets_mut(&mut self) -> &mut BTreeMap<ImageAssetId, RasterAsset> {
        &mut self.assets
    }

    pub(crate) fn palettes_mut_raw(&mut self) -> &mut Vec<ColorPalette> {
        &mut self.palettes
    }

    pub(crate) fn document_palette_mut_raw(&mut self) -> &mut ColorPalette {
        &mut self.document_palette
    }

    pub(crate) fn unknown_doc_chunks_mut_raw(&mut self) -> &mut Vec<UnknownCdrChunk> {
        &mut self.unknown_doc_chunks
    }

    pub(crate) fn unknown_data_files_mut_raw(&mut self) -> &mut BTreeMap<String, Vec<u8>> {
        &mut self.unknown_data_files
    }

    pub(crate) fn set_next_id_raw(&mut self, next_id: u64) {
        self.next_id = next_id;
    }

    pub(crate) fn set_active_page_index_raw(&mut self, idx: usize) {
        self.active_page_index = idx;
    }

    pub(crate) fn set_custom_icc_raw(&mut self, profile: Option<Vec<u8>>, name: Option<String>) {
        self.custom_icc_profile = profile;
        self.custom_icc_name = name;
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
                    .find(|l| l.visible && !l.locked)
                    .or_else(|| page.layers.iter().rev().find(|l| !l.locked))
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

    /// Cleans all relational [`ObjectId`] references across surviving objects BEFORE `doomed`
    /// objects are removed from `self.objects`.
    fn cleanup_dangling_refs_before_removal(&mut self, doomed: &HashSet<ObjectId>) {
        self.cleanup_dangling_refs_before_removal_impl(doomed, true);
    }

    /// Cleans all relational [`ObjectId`] references across surviving objects for `ids` and
    /// their `Group`/`PowerClip` descendants while leaving container `children`/`contents`
    /// membership intact (`detach_from_containers = false`) so `replace_in_parent` still
    /// locates them after cleanup.
    pub(crate) fn cleanup_dangling_refs_for_ids(&mut self, ids: &[ObjectId]) {
        let mut doomed = HashSet::new();
        for &id in ids {
            if let Some(obj) = self.objects.get(&id) {
                if doomed.insert(id) {
                    self.collect_descendants_from_payload(&obj.payload, &mut doomed, 0);
                }
            }
        }
        if !doomed.is_empty() {
            self.cleanup_dangling_refs_before_removal_impl(&doomed, false);
        }
    }

    /// Public relational cleanup helper for external crates (such as `coree-prod` print-merge)
    /// that splice replacement objects into a parent container/layer slot before calling
    /// [`Document::remove_object`], ensuring `Connector`/`Blend`/`TextOnPath` fallbacks capture
    /// the full ancestor transform while `ids` are still attached.
    pub fn cleanup_dangling_refs_for_object_ids(&mut self, ids: &[ObjectId]) {
        self.cleanup_dangling_refs_for_ids(ids);
    }

    fn remove_orphaned_contents_before_overwrite(
        &mut self,
        owner_id: ObjectId,
        orphaned_contents: &[ObjectId],
    ) {
        if orphaned_contents.is_empty() {
            return;
        }
        let mut doomed = HashSet::new();
        for &cid in orphaned_contents {
            if cid != owner_id && self.objects.contains_key(&cid) && doomed.insert(cid) {
                if let Some(cobj) = self.objects.get(&cid) {
                    self.collect_descendants_from_payload(&cobj.payload, &mut doomed, 0);
                }
            }
        }
        doomed.remove(&owner_id);
        if !doomed.is_empty() {
            self.cleanup_dangling_refs_before_removal(&doomed);
            for &cid in &doomed {
                self.objects.remove(&cid);
            }
            self.detach_from_parents(&doomed);
        }
    }

    fn cleanup_dangling_refs_before_removal_impl(
        &mut self,
        doomed: &HashSet<ObjectId>,
        detach_from_containers: bool,
    ) {
        if doomed.is_empty() {
            return;
        }

        // Pre-pass: collect ParagraphText overflow chain and capture geometric anchors for
        // surviving Connectors and TextOnPath objects whose targets are doomed.
        let mut para_next: HashMap<ObjectId, Option<ObjectId>> = HashMap::new();
        let mut connector_fallbacks: HashMap<ObjectId, (Option<Point2D>, Option<Point2D>)> =
            HashMap::new();
        let mut text_on_path_xfs: HashMap<ObjectId, Option<Affine2D>> = HashMap::new();

        for (&id, obj) in &self.objects {
            if doomed.contains(&id) {
                if let NodePayload::ParagraphText(ref p) = obj.payload {
                    para_next.insert(id, p.linked_next_frame);
                }
                continue;
            }
            match &obj.payload {
                NodePayload::ParagraphText(p) => {
                    para_next.insert(id, p.linked_next_frame);
                }
                NodePayload::Connector(conn) => {
                    let from_doomed = conn.from_obj.is_some_and(|f| doomed.contains(&f));
                    let to_doomed = conn.to_obj.is_some_and(|t| doomed.contains(&t));
                    if from_doomed || to_doomed {
                        let world_xf = self.world_transform_of(id).unwrap_or(obj.transform);
                        let (w_from, w_to) =
                            self.resolve_connector_endpoints_with_transform(conn, &world_xf);
                        let inv_world = world_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                        let f_local = if from_doomed {
                            let pt = inv_world.transform_point(w_from);
                            pt.is_finite().then_some(pt)
                        } else {
                            None
                        };
                        let t_local = if to_doomed {
                            let pt = inv_world.transform_point(w_to);
                            pt.is_finite().then_some(pt)
                        } else {
                            None
                        };
                        connector_fallbacks.insert(id, (f_local, t_local));
                    }
                }
                NodePayload::TextOnPath(top) if doomed.contains(&top.path_id) => {
                    let anchor_world = self
                        .world_path_of(top.path_id)
                        .and_then(|wp| {
                            let dist = if top.offset_mm.is_finite() {
                                top.offset_mm.max(0.0)
                            } else {
                                0.0
                            };
                            wp.point_and_tangent_at(dist).map(|(pt, tan)| {
                                let lift = if top.distance_mm.is_finite() {
                                    top.distance_mm
                                } else {
                                    0.0
                                };
                                let pos = pt + tan.perp() * lift;
                                let angle = tan.y.atan2(tan.x);
                                Affine2D::translate(pos.x, pos.y).compose(&Affine2D::rotate(angle))
                            })
                        })
                        .or_else(|| self.world_transform_of(top.path_id));
                    let inv_parent = self
                        .parent_container_of(id)
                        .and_then(|pid| self.world_transform_of(pid))
                        .and_then(|xf| xf.inverse())
                        .unwrap_or(Affine2D::IDENTITY);
                    let local_xf = anchor_world
                        .map(|w| inv_parent.compose(&w))
                        .filter(|xf| xf.is_finite());
                    text_on_path_xfs.insert(id, local_xf);
                }
                _ => {}
            }
        }

        // Mutation pass over all surviving objects
        let surviving_ids: Vec<ObjectId> = self
            .objects
            .keys()
            .copied()
            .filter(|id| !doomed.contains(id))
            .collect();

        for id in surviving_ids {
            let Some(obj) = self.objects.get_mut(&id) else {
                continue;
            };
            match &mut obj.payload {
                NodePayload::Group(children)
                | NodePayload::PowerClip {
                    contents: children, ..
                } => {
                    if detach_from_containers {
                        children.retain(|cid| !doomed.contains(cid));
                    }
                }
                NodePayload::Connector(conn) => {
                    if let Some((f_local, t_local)) = connector_fallbacks.get(&id) {
                        if conn.from_obj.is_some_and(|f| doomed.contains(&f)) {
                            if let Some(pt) = f_local {
                                conn.from_fallback = *pt;
                            }
                            conn.from_obj = None;
                        }
                        if conn.to_obj.is_some_and(|t| doomed.contains(&t)) {
                            if let Some(pt) = t_local {
                                conn.to_fallback = *pt;
                            }
                            conn.to_obj = None;
                        }
                    }
                }
                NodePayload::TextOnPath(top) => {
                    if doomed.contains(&top.path_id) {
                        let art = top.text.clone();
                        obj.payload = NodePayload::ArtisticText(art);
                        if let Some(Some(new_xf)) = text_on_path_xfs.get(&id) {
                            obj.transform = *new_xf;
                        }
                    }
                }
                NodePayload::ParagraphText(para) => {
                    if let Some(mut cur) = para.linked_next_frame {
                        if doomed.contains(&cur) {
                            let mut visited = HashSet::new();
                            visited.insert(id);
                            let mut resolved = None;
                            while doomed.contains(&cur) {
                                if !visited.insert(cur) {
                                    break;
                                }
                                match para_next.get(&cur).copied().flatten() {
                                    Some(nxt) => {
                                        if !doomed.contains(&nxt) {
                                            if nxt != id
                                                && !visited.contains(&nxt)
                                                && para_next.contains_key(&nxt)
                                            {
                                                resolved = Some(nxt);
                                            }
                                            break;
                                        }
                                        cur = nxt;
                                    }
                                    None => break,
                                }
                            }
                            para.linked_next_frame = resolved;
                        }
                    }
                }
                _ => {}
            }

            // Clean dangling Blend effects
            obj.effects.retain(|eff| match eff {
                Effect::Blend { target_id, .. } => !doomed.contains(target_id),
                _ => true,
            });
            for eff in &mut obj.effects {
                if let Effect::Blend { along_path, .. } = eff {
                    if along_path.is_some_and(|p| doomed.contains(&p)) {
                        *along_path = None;
                    }
                }
            }
            // Extension hook for W2 ADD-02 live Clone objects:
            // detach/promote any surviving Clone referencing a doomed master_id here.
        }
    }

    /// Removes `id` (and any nested Group/PowerClip descendants) from all layers,
    /// parent containers, relational object links, and the object map.
    pub fn remove_object(&mut self, id: ObjectId) -> CoreResult<SceneObject> {
        let obj = self
            .objects
            .get(&id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(id))?;
        let mut to_remove = HashSet::new();
        to_remove.insert(id);
        self.collect_descendants_from_payload(&obj.payload, &mut to_remove, 0);

        self.cleanup_dangling_refs_before_removal(&to_remove);

        for &cid in &to_remove {
            self.objects.remove(&cid);
        }
        self.detach_from_parents(&to_remove);
        self.touch();
        Ok(obj)
    }

    /// Returns `Some(locked_id)` if `id` or any nested `Group`/`PowerClip` descendant is locked.
    #[must_use]
    pub fn first_locked_in_subtree(&self, id: ObjectId) -> Option<ObjectId> {
        let obj = self.objects.get(&id)?;
        if obj.locked {
            return Some(id);
        }
        let mut subtree = HashSet::new();
        subtree.insert(id);
        self.collect_descendants_from_payload(&obj.payload, &mut subtree, 0);
        let mut sorted: Vec<ObjectId> = subtree.into_iter().collect();
        sorted.sort_by_key(|oid| oid.0);
        sorted
            .into_iter()
            .find(|&oid| self.objects.get(&oid).is_some_and(|o| o.locked))
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

    fn remap_single_object_refs(
        obj: &mut SceneObject,
        id_map: &HashMap<ObjectId, ObjectId>,
        clear_unmapped_linked_frame: bool,
    ) {
        match &mut obj.payload {
            NodePayload::Group(children) => {
                *children = children
                    .iter()
                    .map(|old| id_map.get(old).copied().unwrap_or(*old))
                    .collect();
            }
            NodePayload::PowerClip { contents, .. } => {
                *contents = contents
                    .iter()
                    .map(|old| id_map.get(old).copied().unwrap_or(*old))
                    .collect();
            }
            NodePayload::TextOnPath(top) => {
                if let Some(&mapped) = id_map.get(&top.path_id) {
                    top.path_id = mapped;
                }
            }
            NodePayload::Connector(conn) => {
                if let Some(old_from) = conn.from_obj {
                    if let Some(&mapped) = id_map.get(&old_from) {
                        conn.from_obj = Some(mapped);
                    }
                }
                if let Some(old_to) = conn.to_obj {
                    if let Some(&mapped) = id_map.get(&old_to) {
                        conn.to_obj = Some(mapped);
                    }
                }
            }
            NodePayload::ParagraphText(para) => {
                if let Some(old_next) = para.linked_next_frame {
                    if let Some(&mapped) = id_map.get(&old_next) {
                        para.linked_next_frame = Some(mapped);
                    } else if clear_unmapped_linked_frame {
                        para.linked_next_frame = None;
                    }
                }
            }
            _ => {}
        }
        for eff in &mut obj.effects {
            if let Effect::Blend {
                target_id,
                along_path,
                ..
            } = eff
            {
                if let Some(&mapped) = id_map.get(target_id) {
                    *target_id = mapped;
                }
                if let Some(old_path) = *along_path {
                    if let Some(&mapped) = id_map.get(&old_path) {
                        *along_path = Some(mapped);
                    }
                }
            }
        }
        // Extension hook for W2 ADD-02 live Clone objects: remap master_id if in id_map.
    }

    /// Deep-duplicates `ids` (including nested Group / PowerClip children and cross-object
    /// `Blend`/`TextOnPath`/`Connector`/`ParagraphText` links) translated by `offset`.
    pub fn duplicate_objects(
        &mut self,
        ids: &[ObjectId],
        offset: Vec2D,
    ) -> CoreResult<Vec<ObjectId>> {
        if !offset.is_finite() {
            return Err(CoreError::InvalidGeometry(
                "duplicate offset contains non-finite values".into(),
            ));
        }
        for &src_id in ids {
            if !self.objects.contains_key(&src_id) {
                return Err(CoreError::ObjectNotFound(src_id));
            }
            let mut cur = src_id;
            let mut chain_guard = HashSet::new();
            chain_guard.insert(cur);
            while let Some(pid) = self.parent_container_of(cur) {
                if !chain_guard.insert(pid) {
                    return Err(CoreError::CycleDetected(format!(
                        "cyclic container detected for {src_id}"
                    )));
                }
                cur = pid;
            }
        }

        let target_ids = self.filter_topmost_selection(ids);
        let mut target_layers: Vec<(Option<LayerId>, Option<LayerId>)> =
            Vec::with_capacity(target_ids.len());
        for &src_id in &target_ids {
            let orig_layer = self.layer_of_object(src_id);
            let enclosing_layer = orig_layer.or_else(|| {
                let mut cur = src_id;
                let mut seen = HashSet::new();
                seen.insert(cur);
                while let Some(pid) = self.parent_container_of(cur) {
                    if !seen.insert(pid) {
                        break;
                    }
                    if let Some(lid) = self.layer_of_object(pid) {
                        return Some(lid);
                    }
                    cur = pid;
                }
                None
            });
            if let Some(lid) = enclosing_layer {
                if self.find_layer(lid).is_some_and(|l| l.locked) {
                    return Err(CoreError::ObjectLocked(src_id));
                }
            } else if let Some(page) = self.active_page() {
                let fallback_layer = page
                    .layers
                    .iter()
                    .rev()
                    .find(|l| !l.locked)
                    .or_else(|| page.layers.last());
                if let Some(layer) = fallback_layer {
                    if layer.locked {
                        return Err(CoreError::ObjectLocked(src_id));
                    }
                }
            }
            target_layers.push((orig_layer, enclosing_layer));
        }

        let mut created = Vec::with_capacity(target_ids.len());
        let mut id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
        let mut visiting: HashSet<ObjectId> = HashSet::new();
        let delta_xf = Affine2D::translate(offset.x, offset.y);
        let saved_next_id = self.next_id;
        let mut added_to_layers: Vec<(LayerId, ObjectId)> = Vec::new();

        for (&src_id, &(orig_layer, enclosing_layer)) in target_ids.iter().zip(target_layers.iter())
        {
            let orig_world_xf = self.world_transform_of(src_id);
            let already_duplicated = id_map.contains_key(&src_id);
            let new_id = match self.duplicate_recursive(src_id, &mut visiting, &mut id_map, 0) {
                Ok(nid) => nid,
                Err(err) => {
                    for (lid, nid) in added_to_layers {
                        if let Some(layer) = self.find_layer_mut(lid) {
                            layer.objects.retain(|&x| x != nid);
                        }
                    }
                    for nid in id_map.values() {
                        self.objects.remove(nid);
                    }
                    self.next_id = saved_next_id;
                    return Err(err);
                }
            };

            if !already_duplicated {
                if let Some(obj) = self.objects.get_mut(&new_id) {
                    if orig_layer.is_none() {
                        let base = orig_world_xf.unwrap_or(obj.transform);
                        obj.transform = delta_xf.compose(&base);
                    } else {
                        obj.transform = delta_xf.compose(&obj.transform);
                    }
                }
                if let Some(lid) = orig_layer {
                    if let Some(layer) = self.find_layer_mut(lid) {
                        if !layer.objects.contains(&new_id) {
                            layer.objects.push(new_id);
                            added_to_layers.push((lid, new_id));
                        }
                    }
                } else if let Some(enc_lid) = enclosing_layer {
                    let target_page = self
                        .pages
                        .iter_mut()
                        .find(|p| p.layers.iter().any(|l| l.id == enc_lid));
                    if let Some(page) = target_page {
                        let target_idx = page
                            .layers
                            .iter()
                            .position(|l| l.id == enc_lid && !l.locked)
                            .or_else(|| page.layers.iter().rposition(|l| !l.locked))
                            .or_else(|| (!page.layers.is_empty()).then_some(page.layers.len() - 1));
                        if let Some(idx) = target_idx {
                            let layer = &mut page.layers[idx];
                            let lid = layer.id;
                            if !layer.objects.contains(&new_id) {
                                layer.objects.push(new_id);
                                added_to_layers.push((lid, new_id));
                            }
                        }
                    }
                } else if let Some(page) = self.active_page_mut() {
                    let target_idx = page
                        .layers
                        .iter()
                        .rposition(|l| !l.locked)
                        .or_else(|| (!page.layers.is_empty()).then_some(page.layers.len() - 1));
                    if let Some(idx) = target_idx {
                        let layer = &mut page.layers[idx];
                        let lid = layer.id;
                        if !layer.objects.contains(&new_id) {
                            layer.objects.push(new_id);
                            added_to_layers.push((lid, new_id));
                        }
                    }
                }
            }
            created.push(new_id);
        }

        let new_ids: Vec<ObjectId> = id_map.values().copied().collect();
        for nid in new_ids {
            if let Some(obj) = self.objects.get_mut(&nid) {
                Self::remap_single_object_refs(obj, &id_map, true);
            }
        }

        self.touch();
        Ok(created)
    }

    fn duplicate_recursive(
        &mut self,
        src_id: ObjectId,
        visiting: &mut HashSet<ObjectId>,
        id_map: &mut HashMap<ObjectId, ObjectId>,
        depth: usize,
    ) -> CoreResult<ObjectId> {
        if visiting.contains(&src_id) {
            return Err(CoreError::CycleDetected(format!(
                "cyclic container detected while duplicating {src_id}"
            )));
        }
        if let Some(&existing) = id_map.get(&src_id) {
            return Ok(existing);
        }
        if depth > MAX_SCENE_DEPTH {
            return Err(CoreError::CycleDetected(
                "scene nesting depth exceeded".into(),
            ));
        }
        visiting.insert(src_id);

        let cloned_res = (|| -> CoreResult<ObjectId> {
            let mut cloned = self
                .objects
                .get(&src_id)
                .cloned()
                .ok_or(CoreError::ObjectNotFound(src_id))?;
            let new_id = self.alloc_object_id();
            cloned.id = new_id;
            id_map.insert(src_id, new_id);

            match &mut cloned.payload {
                NodePayload::Group(children) => {
                    let orig_children = children.clone();
                    let mut new_children = Vec::with_capacity(orig_children.len());
                    for cid in orig_children {
                        new_children.push(self.duplicate_recursive(
                            cid,
                            visiting,
                            id_map,
                            depth + 1,
                        )?);
                    }
                    *children = new_children;
                }
                NodePayload::PowerClip { contents, .. } => {
                    let orig_contents = contents.clone();
                    let mut new_contents = Vec::with_capacity(orig_contents.len());
                    for cid in orig_contents {
                        new_contents.push(self.duplicate_recursive(
                            cid,
                            visiting,
                            id_map,
                            depth + 1,
                        )?);
                    }
                    *contents = new_contents;
                }
                _ => {}
            }

            self.objects.insert(new_id, cloned);
            Ok(new_id)
        })();

        visiting.remove(&src_id);
        cloned_res
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
    pub(crate) fn replace_in_parent(&mut self, old_id: ObjectId, replacements: &[ObjectId]) {
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
    pub(crate) fn insert_siblings_after(&mut self, anchor_id: ObjectId, siblings: &[ObjectId]) {
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
        for &id in ids {
            if !self.objects.contains_key(&id) {
                return Err(CoreError::ObjectNotFound(id));
            }
        }
        let unique_ids = self.filter_topmost_selection(ids);
        if unique_ids.is_empty() {
            return Err(CoreError::InvalidParameter(
                "cannot create an empty group".into(),
            ));
        }
        let seen: HashSet<ObjectId> = unique_ids.iter().copied().collect();

        let mut target_layer = self.layer_of_object(unique_ids[0]);
        let target_parent = if target_layer.is_none() {
            self.parent_container_of(unique_ids[0])
                .filter(|pid| !seen.contains(pid))
        } else {
            None
        };
        if target_layer.is_none() && target_parent.is_none() {
            target_layer = unique_ids.iter().find_map(|&id| self.layer_of_object(id));
        }
        if let Some(pid) = target_parent {
            if self.is_effectively_locked(pid) {
                return Err(CoreError::ObjectLocked(pid));
            }
        } else if let Some(lid) = target_layer {
            if self.find_layer(lid).is_some_and(|l| l.locked) {
                return Err(CoreError::ObjectLocked(unique_ids[0]));
            }
        }

        // Capture each grouped object's world transform and the inverse world transform of the
        // target container BEFORE detaching so mixed-container/layer groups preserve world coords.
        let child_worlds: Vec<(ObjectId, Affine2D)> = unique_ids
            .iter()
            .filter_map(|&cid| {
                let xf = self
                    .world_transform_of(cid)
                    .or_else(|| self.objects.get(&cid).map(|o| o.transform))?;
                Some((cid, xf))
            })
            .collect();
        let inv_target_parent = target_parent
            .and_then(|pid| self.world_transform_of(pid))
            .and_then(|xf| xf.inverse())
            .unwrap_or(Affine2D::IDENTITY);

        // Detach grouped children from all prior layers and parent containers
        self.detach_from_parents(&seen);

        for (cid, child_world) in child_worlds {
            if let Some(child) = self.objects.get_mut(&cid) {
                child.transform = inv_target_parent.compose(&child_world);
            }
        }

        let group_id = self.alloc_object_id();
        let group_obj = SceneObject::new(group_id, name, NodePayload::Group(unique_ids))
            .with_outline(OutlineStyle::none());

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
        if self.is_effectively_locked(group_id) {
            return Err(CoreError::ObjectLocked(group_id));
        }
        let NodePayload::Group(children) = group.payload else {
            return Err(CoreError::InvalidParameter(format!(
                "{group_id} is not a Group"
            )));
        };
        if let Some(locked_child) = self.first_locked_in_subtree(group_id) {
            return Err(CoreError::ObjectLocked(locked_child));
        }

        // Clean up any surviving Connector / Blend references to `group_id` BEFORE baking
        // `group_xf` into children and removing `group_id`, so Connector fallbacks see exact
        // pre-ungroup world bounds of the group.
        let doomed = HashSet::from([group_id]);
        self.cleanup_dangling_refs_before_removal_impl(&doomed, false);

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

    /// Returns IDs from `ids` with any redundant descendants removed when an ancestor
    /// (`Group` or `PowerClip`) is also present in `ids`, preserving order and deduplicating.
    #[must_use]
    pub fn filter_topmost_selection(&self, ids: &[ObjectId]) -> Vec<ObjectId> {
        let id_set: HashSet<ObjectId> = ids.iter().copied().collect();
        let mut seen = HashSet::new();
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            if !seen.insert(id) {
                continue;
            }
            let mut cur = id;
            let mut has_selected_ancestor = false;
            let mut chain_guard = HashSet::new();
            chain_guard.insert(cur);
            while let Some(pid) = self.parent_container_of(cur) {
                if !chain_guard.insert(pid) {
                    break;
                }
                if id_set.contains(&pid) {
                    has_selected_ancestor = true;
                    break;
                }
                cur = pid;
            }
            if !has_selected_ancestor {
                out.push(id);
            }
        }
        out
    }

    /// Translates `id` by `(dx, dy)` in **world space**, conjugating by its parent container's
    /// world transform if `id` is nested inside a rotated/scaled `Group` or `PowerClip`.
    fn apply_world_translation(&mut self, id: ObjectId, dx: f64, dy: f64) -> CoreResult<()> {
        if !dx.is_finite() || !dy.is_finite() {
            return Err(CoreError::InvalidGeometry(
                "world translation contains non-finite values".into(),
            ));
        }
        if dx == 0.0 && dy == 0.0 {
            return Ok(());
        }
        let world_delta = Affine2D::translate(dx, dy);
        let local_delta = if let Some(pid) = self.parent_container_of(id) {
            if let Some(pw) = self.world_transform_of(pid) {
                if let Some(inv_pw) = pw.inverse() {
                    inv_pw.compose(&world_delta).compose(&pw)
                } else {
                    world_delta
                }
            } else {
                world_delta
            }
        } else {
            world_delta
        };
        self.transform_object(id, local_delta)
    }

    /// Aligns `ids` relative to their combined world bounding box (or active page for `CenterPage`).
    pub fn align_objects(&mut self, ids: &[ObjectId], op: AlignOp) -> CoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        for &id in ids {
            if !self.objects.contains_key(&id) {
                return Err(CoreError::ObjectNotFound(id));
            }
        }
        let target_ids = self.filter_topmost_selection(ids);

        let reference = if op == AlignOp::CenterPage {
            self.active_page()
                .map_or(Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0), Page::trim_rect)
        } else {
            let Some(bb) = self.combined_bounds(&target_ids) else {
                return Ok(());
            };
            bb
        };

        let locked_ids = self.effective_locked_object_ids();
        for &id in &target_ids {
            let Some(bb) = self.world_object_bounds(id) else {
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
            if !locked_ids.contains(&id) && self.first_locked_in_subtree(id).is_none() {
                let _ = self.apply_world_translation(id, dx, dy);
            }
        }
        self.touch();
        Ok(())
    }

    /// Evenly distributes 3 or more objects along the selected horizontal or vertical metric
    /// in world space.
    pub fn distribute_objects(&mut self, ids: &[ObjectId], op: DistributeOp) -> CoreResult<()> {
        if ids.len() < 3 {
            return Ok(());
        }
        for &id in ids {
            if !self.objects.contains_key(&id) {
                return Err(CoreError::ObjectNotFound(id));
            }
        }
        let target_ids = self.filter_topmost_selection(ids);
        let mut items: Vec<(ObjectId, Rect2D)> = Vec::with_capacity(target_ids.len());
        for &id in &target_ids {
            if let Some(bb) = self.world_object_bounds(id) {
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

        let locked_ids = self.effective_locked_object_ids();
        for (id, dx, dy) in deltas {
            if !locked_ids.contains(&id) && self.first_locked_in_subtree(id).is_none() {
                let _ = self.apply_world_translation(id, dx, dy);
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
        if self.is_effectively_locked(container_id) {
            return Err(CoreError::ObjectLocked(container_id));
        }
        if let Some(locked_child) = self.first_locked_in_subtree(container_id) {
            return Err(CoreError::ObjectLocked(locked_child));
        }

        // Extract clipping contour, optional parametric container shape, and existing clipped children
        let (clip_path, container_parametric, lock_contents, existing_contents) =
            match &container_obj.payload {
                NodePayload::Path(p) => (p.clone(), None, true, Vec::new()),
                NodePayload::Shape(s) => (s.to_bezier_path(), Some(s.clone()), true, Vec::new()),
                NodePayload::PowerClip {
                    container,
                    container_parametric,
                    lock_contents,
                    contents,
                } => (
                    (**container).clone(),
                    container_parametric.clone(),
                    *lock_contents,
                    contents.clone(),
                ),
                _ => {
                    return Err(CoreError::InvalidGeometry(
                        "PowerClip container must be a Path or ParametricShape".into(),
                    ));
                }
            };
        let existing_set: HashSet<ObjectId> = existing_contents.iter().copied().collect();

        // Validate newly added contents and guard cycles across all supplied IDs, then reduce
        // to topmost selections so `[parent_group, child]` preserves `child` inside `parent_group`.
        for &cid in content_ids {
            if cid == container_id {
                return Err(CoreError::CycleDetected(
                    "object cannot be PowerClipped inside itself".into(),
                ));
            }
            if !self.objects.contains_key(&cid) {
                return Err(CoreError::ObjectNotFound(cid));
            }
            if self.is_effectively_locked(cid) {
                return Err(CoreError::ObjectLocked(cid));
            }
            if let Some(locked_child) = self.first_locked_in_subtree(cid) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
            if self.contains_descendant(cid, container_id, 0) {
                return Err(CoreError::CycleDetected(
                    "PowerClip would create a cyclic hierarchy".into(),
                ));
            }
        }
        let topmost_ids = self.filter_topmost_selection(content_ids);
        let new_contents: Vec<ObjectId> = topmost_ids
            .into_iter()
            .filter(|cid| !existing_set.contains(cid))
            .collect();

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
                container_parametric,
                lock_contents,
                contents: merged_contents,
            };
        }
        self.touch();
        Ok(container_id)
    }

    /// Toggles `lock_contents` on an existing [`NodePayload::PowerClip`].
    pub fn set_powerclip_lock_contents(
        &mut self,
        powerclip_id: ObjectId,
        locked: bool,
    ) -> CoreResult<()> {
        let obj = self
            .objects
            .get_mut(&powerclip_id)
            .ok_or(CoreError::ObjectNotFound(powerclip_id))?;
        if let NodePayload::PowerClip { lock_contents, .. } = &mut obj.payload {
            *lock_contents = locked;
            self.touch();
            Ok(())
        } else {
            Err(CoreError::InvalidParameter(format!(
                "{powerclip_id} is not a PowerClip"
            )))
        }
    }

    /// Transforms object `id` by `affine` in world/parent space, keeping unlocked PowerClip
    /// contents anchored in place when `lock_contents == false`.
    pub fn transform_object(&mut self, id: ObjectId, affine: Affine2D) -> CoreResult<()> {
        if !affine.is_finite() {
            return Err(CoreError::InvalidGeometry(
                "transform contains non-finite entries".into(),
            ));
        }
        if !self.objects.contains_key(&id) {
            return Err(CoreError::ObjectNotFound(id));
        }
        if self.is_effectively_locked(id) {
            return Err(CoreError::ObjectLocked(id));
        }
        let check_subtree_lock = matches!(
            self.objects.get(&id).map(|o| &o.payload),
            Some(NodePayload::Group(_))
                | Some(NodePayload::PowerClip {
                    lock_contents: true,
                    ..
                })
        );
        if check_subtree_lock {
            if let Some(locked_child) = self.first_locked_in_subtree(id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
        } else if let Some(obj) = self.objects.get(&id) {
            if matches!(
                obj.payload,
                NodePayload::PowerClip {
                    lock_contents: false,
                    ..
                }
            ) && affine.compose(&obj.transform).inverse().is_none()
            {
                if let Some(locked_child) = self.first_locked_in_subtree(id) {
                    return Err(CoreError::ObjectLocked(locked_child));
                }
            }
        }
        let (old_xf, unlocked_contents) = {
            let obj = self
                .objects
                .get_mut(&id)
                .ok_or(CoreError::ObjectNotFound(id))?;
            let old = obj.transform;
            obj.transform = affine.compose(&old);
            let unlocked = match &obj.payload {
                NodePayload::PowerClip {
                    lock_contents: false,
                    contents,
                    ..
                } => contents.clone(),
                _ => Vec::new(),
            };
            (old, unlocked)
        };
        if !unlocked_contents.is_empty() {
            let new_xf = self.objects[&id].transform;
            if let Some(inv_new) = new_xf.inverse() {
                let compensate = inv_new.compose(&old_xf);
                for cid in unlocked_contents {
                    if let Some(child) = self.objects.get_mut(&cid) {
                        child.transform = compensate.compose(&child.transform);
                    }
                }
            }
        }
        self.touch();
        Ok(())
    }

    /// Extracts all clipped contents from `powerclip_id` back into its enclosing layer
    /// or parent container and restores `powerclip_id` to its original parametric shape (if any)
    /// or an ordinary [`NodePayload::Path`].
    pub fn extract_powerclip(&mut self, powerclip_id: ObjectId) -> CoreResult<Vec<ObjectId>> {
        let obj = self
            .objects
            .get(&powerclip_id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(powerclip_id))?;
        if self.is_effectively_locked(powerclip_id) {
            return Err(CoreError::ObjectLocked(powerclip_id));
        }
        let NodePayload::PowerClip {
            container,
            container_parametric,
            lock_contents: _,
            contents,
        } = obj.payload
        else {
            return Err(CoreError::InvalidParameter(format!(
                "{powerclip_id} is not a PowerClip"
            )));
        };
        if let Some(locked_child) = self.first_locked_in_subtree(powerclip_id) {
            return Err(CoreError::ObjectLocked(locked_child));
        }

        let parent_xf = obj.transform;

        for &cid in &contents {
            if let Some(child) = self.objects.get_mut(&cid) {
                child.transform = parent_xf.compose(&child.transform);
            }
        }

        if let Some(target) = self.objects.get_mut(&powerclip_id) {
            target.payload = match container_parametric {
                Some(shape) => NodePayload::Shape(shape),
                None => NodePayload::Path(*container),
            };
        }

        self.insert_siblings_after(powerclip_id, &contents);
        self.touch();
        Ok(contents)
    }

    /// Fits, centers, or stretches all clipped contents of `powerclip_id` relative to its
    /// container contour in container-local coordinates (`#42`).
    pub fn fit_powerclip(
        &mut self,
        powerclip_id: ObjectId,
        mode: PowerClipFitMode,
    ) -> CoreResult<()> {
        let obj = self
            .objects
            .get(&powerclip_id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(powerclip_id))?;
        if self.is_effectively_locked(powerclip_id) {
            return Err(CoreError::ObjectLocked(powerclip_id));
        }
        if let Some(locked_child) = self.first_locked_in_subtree(powerclip_id) {
            return Err(CoreError::ObjectLocked(locked_child));
        }
        let NodePayload::PowerClip {
            container,
            contents,
            ..
        } = obj.payload
        else {
            return Err(CoreError::InvalidParameter(format!(
                "{powerclip_id} is not a PowerClip"
            )));
        };
        if contents.is_empty() {
            return Ok(());
        }

        let c_bb = container.bounding_box().ok_or_else(|| {
            CoreError::InvalidGeometry("PowerClip container has empty bounds".into())
        })?;
        let c_w = c_bb.width();
        let c_h = c_bb.height();
        let c_center = c_bb.center();
        if !c_bb.is_finite() || c_w <= 1e-9 || c_h <= 1e-9 {
            return Err(CoreError::InvalidGeometry(
                "PowerClip container has zero or non-finite area".into(),
            ));
        }

        let pc_world = self.world_transform_of(powerclip_id);
        let inv_pc = pc_world.and_then(|xf| xf.inverse());
        let pc_space = pc_world.as_ref().zip(inv_pc.as_ref());
        let mut contents_bb: Option<Rect2D> = None;
        let mut bounds_ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        for &cid in &contents {
            if let (Some(bb), _) = self.object_bounds_inner_with_space(
                cid,
                &Affine2D::IDENTITY,
                0,
                &mut bounds_ctx,
                &mut visiting_syms,
                pc_space,
            ) {
                contents_bb = Some(match contents_bb {
                    Some(prev) => prev.union(bb),
                    None => bb,
                });
            }
        }
        let Some(s_bb) = contents_bb else {
            return Ok(());
        };
        let s_w = s_bb.width();
        let s_h = s_bb.height();
        let s_center = s_bb.center();

        let fit_xf = match mode {
            PowerClipFitMode::CenterContents => {
                Affine2D::translate(c_center.x - s_center.x, c_center.y - s_center.y)
            }
            PowerClipFitMode::FitContents => {
                if s_w <= 1e-9 && s_h <= 1e-9 {
                    Affine2D::translate(c_center.x - s_center.x, c_center.y - s_center.y)
                } else {
                    let sx = if s_w > 1e-9 { c_w / s_w } else { f64::INFINITY };
                    let sy = if s_h > 1e-9 { c_h / s_h } else { f64::INFINITY };
                    let s = sx.min(sy);
                    Affine2D::translate(c_center.x, c_center.y)
                        .compose(&Affine2D::scale(s, s))
                        .compose(&Affine2D::translate(-s_center.x, -s_center.y))
                }
            }
            PowerClipFitMode::StretchToFill => {
                let sx = if s_w > 1e-9 { c_w / s_w } else { 1.0 };
                let sy = if s_h > 1e-9 { c_h / s_h } else { 1.0 };
                Affine2D::translate(c_center.x, c_center.y)
                    .compose(&Affine2D::scale(sx, sy))
                    .compose(&Affine2D::translate(-s_center.x, -s_center.y))
            }
        };

        if !fit_xf.is_finite() {
            return Err(CoreError::InvalidGeometry(
                "FitPowerClip produced a non-finite transform".into(),
            ));
        }

        for cid in contents {
            if let Some(child) = self.objects.get_mut(&cid) {
                child.transform = fit_xf.compose(&child.transform);
            }
        }
        self.touch();
        Ok(())
    }

    /// Deep-clones a [`SymbolDef`]'s object list into `self.objects` with fresh IDs and remapped
    /// internal references, returning the top-level cloned child IDs.
    fn materialize_symbol_objects(&mut self, sym_objects: &[SceneObject]) -> Vec<ObjectId> {
        let mut referenced_children: HashSet<ObjectId> = HashSet::new();
        for sub in sym_objects {
            match &sub.payload {
                NodePayload::Group(c) => referenced_children.extend(c.iter().copied()),
                NodePayload::PowerClip { contents, .. } => {
                    referenced_children.extend(contents.iter().copied());
                }
                _ => {}
            }
        }

        let mut id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
        let mut cloned_list: Vec<(bool, SceneObject)> = Vec::with_capacity(sym_objects.len());
        for sub in sym_objects {
            let new_id = self.alloc_object_id();
            if sub.id.0 != 0 {
                id_map.insert(sub.id, new_id);
            }
            let is_top = sub.id.0 == 0 || !referenced_children.contains(&sub.id);
            let mut cloned = sub.clone();
            cloned.id = new_id;
            cloned_list.push((is_top, cloned));
        }

        let mut top_ids = Vec::new();
        for (is_top, mut cloned) in cloned_list {
            Self::remap_single_object_refs(&mut cloned, &id_map, false);
            let nid = cloned.id;
            self.objects.insert(nid, cloned);
            if is_top {
                top_ids.push(nid);
            }
        }
        top_ids
    }

    /// Converts a [`NodePayload::SymbolInstance`] into an independent [`NodePayload::Group`]
    /// in-place, preserving its ID, transform, layer/parent z-order, effects, lens, and styles.
    pub fn unlink_symbol_instance(&mut self, id: ObjectId) -> CoreResult<ObjectId> {
        self.unlink_symbol_instance_inner(id, true)
    }

    fn unlink_symbol_instance_inner(
        &mut self,
        id: ObjectId,
        check_lock: bool,
    ) -> CoreResult<ObjectId> {
        let obj = self
            .objects
            .get(&id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(id))?;
        if check_lock && self.is_effectively_locked(id) {
            return Err(CoreError::ObjectLocked(id));
        }
        let NodePayload::SymbolInstance(sym_id) = obj.payload else {
            return Err(CoreError::InvalidParameter(format!(
                "{id} is not a SymbolInstance"
            )));
        };
        let sym = self
            .symbols
            .get(&sym_id)
            .cloned()
            .ok_or(CoreError::SymbolNotFound(sym_id))?;

        let top_children = self.materialize_symbol_objects(&sym.objects);
        if let Some(target) = self.objects.get_mut(&id) {
            target.payload = NodePayload::Group(top_children);
        }
        self.touch();
        Ok(id)
    }

    /// Inserts or updates a [`SymbolDef`] in the document symbol library.
    pub fn upsert_symbol(&mut self, symbol: SymbolDef) {
        self.symbols.insert(symbol.id, symbol);
        self.touch();
    }

    /// Deletes [`SymbolId`] `id`, automatically unlinking any referencing `SymbolInstance`s into
    /// independent `Group`s (both in scene objects and inside other `SymbolDef`s) and clearing
    /// any `Fill::VectorPattern` references to `id`.
    pub fn delete_symbol(&mut self, id: SymbolId) -> CoreResult<SymbolDef> {
        let doomed_sym = self
            .symbols
            .get(&id)
            .cloned()
            .ok_or(CoreError::SymbolNotFound(id))?;
        let locked_ids = self.effective_locked_object_ids();
        for (oid, obj) in &self.objects {
            if !locked_ids.contains(oid) {
                continue;
            }
            let references_symbol = matches!(obj.payload, NodePayload::SymbolInstance(sid) if sid == id)
                || matches!(obj.fill, Fill::VectorPattern { symbol_id, .. } if symbol_id == id)
                || matches!(
                    &obj.payload,
                    NodePayload::Table(tbl)
                        if tbl.cells.iter().any(|c| {
                            matches!(c.fill, Fill::VectorPattern { symbol_id, .. } if symbol_id == id)
                        })
                );
            if references_symbol {
                return Err(CoreError::ObjectLocked(*oid));
            }
        }
        let mut sanitized_doomed_objects = doomed_sym.objects.clone();
        for sub in &mut sanitized_doomed_objects {
            if matches!(sub.payload, NodePayload::SymbolInstance(sid) if sid == id) {
                sub.payload = NodePayload::Group(Vec::new());
            }
            if matches!(sub.fill, Fill::VectorPattern { symbol_id, .. } if symbol_id == id) {
                sub.fill = Fill::None;
            }
            if let NodePayload::Table(ref mut table) = sub.payload {
                for cell in &mut table.cells {
                    if matches!(cell.fill, Fill::VectorPattern { symbol_id, .. } if symbol_id == id)
                    {
                        cell.fill = Fill::None;
                    }
                }
            }
        }

        let instance_ids: Vec<ObjectId> = self
            .objects
            .iter()
            .filter_map(|(&oid, obj)| {
                matches!(obj.payload, NodePayload::SymbolInstance(sid) if sid == id).then_some(oid)
            })
            .collect();
        for oid in instance_ids {
            let top_children = self.materialize_symbol_objects(&sanitized_doomed_objects);
            if let Some(target) = self.objects.get_mut(&oid) {
                target.payload = NodePayload::Group(top_children);
            }
        }

        // Clear VectorPattern fills referencing `id` across objects, object styles, and other symbols
        let clear_vpat = |fill: &mut Fill| {
            if matches!(fill, Fill::VectorPattern { symbol_id, .. } if *symbol_id == id) {
                *fill = Fill::None;
            }
        };
        for obj in self.objects.values_mut() {
            clear_vpat(&mut obj.fill);
            if let NodePayload::Table(ref mut table) = obj.payload {
                for cell in &mut table.cells {
                    clear_vpat(&mut cell.fill);
                }
            }
        }
        for style in &mut self.object_styles {
            if let Some(ref mut fill) = style.fill {
                clear_vpat(fill);
            }
        }
        for (&sym_k, sym) in &mut self.symbols {
            if sym_k == id {
                continue;
            }
            let mut next_local_id = sym
                .objects
                .iter()
                .map(|o| o.id.0)
                .max()
                .unwrap_or(0)
                .saturating_add(1)
                .max(1);
            let mut extra_objects: Vec<SceneObject> = Vec::new();
            for sub in &mut sym.objects {
                clear_vpat(&mut sub.fill);
                if let NodePayload::Table(ref mut table) = sub.payload {
                    for cell in &mut table.cells {
                        clear_vpat(&mut cell.fill);
                    }
                }
                if matches!(sub.payload, NodePayload::SymbolInstance(sid) if sid == id) {
                    let mut referenced_children: HashSet<ObjectId> = HashSet::new();
                    for ds in &sanitized_doomed_objects {
                        match &ds.payload {
                            NodePayload::Group(c) => {
                                referenced_children.extend(c.iter().copied());
                            }
                            NodePayload::PowerClip { contents, .. } => {
                                referenced_children.extend(contents.iter().copied());
                            }
                            _ => {}
                        }
                    }
                    let mut local_id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
                    let mut cloned_list: Vec<(bool, SceneObject)> =
                        Vec::with_capacity(sanitized_doomed_objects.len());
                    for ds in &sanitized_doomed_objects {
                        let new_id = ObjectId(next_local_id);
                        next_local_id = next_local_id.saturating_add(1);
                        if ds.id.0 != 0 {
                            local_id_map.insert(ds.id, new_id);
                        }
                        let is_top = ds.id.0 == 0 || !referenced_children.contains(&ds.id);
                        let mut cloned = ds.clone();
                        cloned.id = new_id;
                        cloned_list.push((is_top, cloned));
                    }
                    let mut top_ids = Vec::new();
                    for (is_top, mut cloned) in cloned_list {
                        Self::remap_single_object_refs(&mut cloned, &local_id_map, false);
                        if is_top {
                            top_ids.push(cloned.id);
                        }
                        extra_objects.push(cloned);
                    }
                    sub.payload = NodePayload::Group(top_ids);
                }
            }
            sym.objects.extend(extra_objects);
        }

        let removed = self
            .symbols
            .remove(&id)
            .ok_or(CoreError::SymbolNotFound(id))?;
        self.touch();
        Ok(removed)
    }

    /// Inserts or updates a [`ColorStyle`] in `self.color_styles`.
    pub fn upsert_color_style(&mut self, style: ColorStyle) {
        if let Some(existing) = self.color_styles.iter_mut().find(|s| s.id == style.id) {
            *existing = style;
        } else {
            self.color_styles.push(style);
        }
        self.touch();
    }

    /// Removes a [`ColorStyle`] by ID.
    pub fn delete_color_style(&mut self, id: ColorStyleId) -> CoreResult<ColorStyle> {
        let pos = self
            .color_styles
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| CoreError::InvalidParameter(format!("color style not found: {id:?}")))?;
        let removed = self.color_styles.remove(pos);
        self.touch();
        Ok(removed)
    }

    /// Inserts or updates an [`ObjectStyle`] and propagates its `Some(...)` overrides to all
    /// unlocked objects linked to `style.id`.
    pub fn upsert_object_style(&mut self, mut style: ObjectStyle) {
        if let Some(op) = style.opacity {
            style.opacity = if op.is_finite() {
                Some(op.clamp(0.0, 1.0))
            } else {
                None
            };
        }
        if let Some(existing) = self.object_styles.iter_mut().find(|s| s.id == style.id) {
            *existing = style.clone();
        } else {
            self.object_styles.push(style.clone());
        }
        let locked_ids = self.effective_locked_object_ids();
        for (id, obj) in &mut self.objects {
            if !locked_ids.contains(id) && obj.style_id == Some(style.id) {
                if let Some(ref f) = style.fill {
                    obj.fill = f.clone();
                }
                if let Some(ref o) = style.outline {
                    obj.outline = o.clone();
                }
                if let Some(bm) = style.blend_mode {
                    obj.blend_mode = bm;
                }
                if let Some(op) = style.opacity {
                    if op.is_finite() {
                        obj.opacity = op.clamp(0.0, 1.0);
                    }
                }
            }
        }
        self.touch();
    }

    /// Deletes [`ObjectStyle`] `id` and clears `style_id = None` on any referencing objects.
    pub fn delete_object_style(&mut self, id: ObjectStyleId) -> CoreResult<ObjectStyle> {
        let pos = self
            .object_styles
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| CoreError::InvalidParameter(format!("object style not found: {id}")))?;
        let removed = self.object_styles.remove(pos);
        for obj in self.objects.values_mut() {
            if obj.style_id == Some(id) {
                obj.style_id = None;
            }
        }
        self.touch();
        Ok(removed)
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
        self.convert_to_curves_with_outliner(id, |_, _| None)
    }

    /// Converts object `id` into editable [`NodePayload::Path`] Bézier paths using an optional
    /// external font-shaping `text_outliner` for real glyph curves on text objects.
    pub fn convert_to_curves_with_outliner<F>(
        &mut self,
        id: ObjectId,
        mut text_outliner: F,
    ) -> CoreResult<()>
    where
        F: FnMut(&SceneObject, &Self) -> Option<BezierPath>,
    {
        let obj = self
            .objects
            .get(&id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(id))?;
        if self.is_effectively_locked(id) {
            return Err(CoreError::ObjectLocked(id));
        }
        if matches!(obj.payload, NodePayload::PowerClip { .. }) {
            if let Some(locked_child) = self.first_locked_in_subtree(id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
        }
        let mut orphaned_contents: Vec<ObjectId> = Vec::new();
        let mut reset_transform = false;
        let path = match &obj.payload {
            NodePayload::Path(p) => p.clone(),
            NodePayload::Shape(shape) => shape.to_bezier_path(),
            NodePayload::Table(table) => table.to_grid_paths(),
            NodePayload::Connector(conn) => {
                let mut bounds_ctx = DocBoundsCtx::new();
                bounds_ctx.visiting_depth.insert(id, 0);
                let mut visiting_syms = HashSet::new();
                let world_xf = self.world_transform_of(id).unwrap_or(obj.transform);
                let ((a, b), _) = self.resolve_connector_endpoints_inner(
                    conn,
                    &world_xf,
                    0,
                    &mut bounds_ctx,
                    &mut visiting_syms,
                );
                let world_route = conn.route_path(a, b);
                if let Some(inv) = world_xf.inverse() {
                    world_route.transformed(&inv)
                } else {
                    reset_transform = true;
                    world_route
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                orphaned_contents = contents.clone();
                (**container).clone()
            }
            NodePayload::ArtisticText(t) => text_outliner(&obj, self)
                .filter(|p| !p.is_empty() && p.is_finite())
                .unwrap_or_else(|| BezierPath::from_rect(t.estimated_bounds())),
            NodePayload::ParagraphText(t) => text_outliner(&obj, self)
                .filter(|p| !p.is_empty() && p.is_finite())
                .unwrap_or_else(|| BezierPath::from_rect(t.bounding_box())),
            NodePayload::TextOnPath(top) => text_outliner(&obj, self)
                .filter(|p| !p.is_empty() && p.is_finite())
                .unwrap_or_else(|| BezierPath::from_rect(top.text.estimated_bounds())),
            _ => {
                return Err(CoreError::InvalidGeometry(format!(
                    "object {id} cannot be converted to curves"
                )));
            }
        };
        self.remove_orphaned_contents_before_overwrite(id, &orphaned_contents);
        if let Some(target) = self.objects.get_mut(&id) {
            target.payload = NodePayload::Path(path);
            if reset_transform {
                target.transform = Affine2D::IDENTITY;
            }
        }
        self.touch();
        Ok(())
    }

    /// Converts an object's visible outline stroke into an independent closed filled path object
    /// (`Ctrl+Shift+Q`: *Convert Outline to Object*).
    ///
    /// - If the object also has a visible fill, preserves the filled object (clearing its outline)
    ///   and adds a new filled `"<Name> (Outline)"` object right above it, returning `Ok(Some(new_id))`.
    /// - If the object has no visible fill (`Fill::None`), converts the object itself in-place into
    ///   the stroked contour shape filled with the outline color, returning `Ok(None)`.
    pub fn convert_outline_to_object(&mut self, id: ObjectId) -> CoreResult<Option<ObjectId>> {
        let obj = self
            .objects
            .get(&id)
            .cloned()
            .ok_or(CoreError::ObjectNotFound(id))?;
        if self.is_effectively_locked(id) {
            return Err(CoreError::ObjectLocked(id));
        }
        if !obj.fill.is_visible() && matches!(obj.payload, NodePayload::PowerClip { .. }) {
            if let Some(locked_child) = self.first_locked_in_subtree(id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
        }
        if !obj.outline.is_visible() {
            return Err(CoreError::InvalidGeometry(format!(
                "object {id} has no visible outline to convert"
            )));
        }

        let world_xf = self.world_transform_of(id).unwrap_or(obj.transform);
        let parent_world = self
            .parent_container_of(id)
            .and_then(|pid| self.world_transform_of(pid));

        let mut orphaned_contents: Vec<ObjectId> = Vec::new();
        let mut connector_world_route: Option<BezierPath> = None;
        let local_path = match &obj.payload {
            NodePayload::Path(p) => p.clone(),
            NodePayload::Shape(s) => s.to_bezier_path(),
            NodePayload::Table(t) => t.to_grid_paths(),
            NodePayload::Connector(conn) => {
                let mut bounds_ctx = DocBoundsCtx::new();
                bounds_ctx.visiting_depth.insert(id, 0);
                let mut visiting_syms = HashSet::new();
                let ((a, b), _) = self.resolve_connector_endpoints_inner(
                    conn,
                    &world_xf,
                    0,
                    &mut bounds_ctx,
                    &mut visiting_syms,
                );
                let world_route = conn.route_path(a, b);
                connector_world_route = Some(world_route.clone());
                if let Some(inv) = world_xf.inverse() {
                    world_route.transformed(&inv)
                } else {
                    world_route
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                if !obj.fill.is_visible() {
                    orphaned_contents = contents.clone();
                }
                (**container).clone()
            }
            _ => {
                return Err(CoreError::InvalidGeometry(format!(
                    "object {id} outline cannot be converted to an object directly"
                )));
            }
        };

        // Stroke in world space so parent Group/PowerClip scaling never distorts
        // `scale_with_object == false` strokes, then map back into parent-local space.
        let outline_path = if let Some(world_route) = connector_world_route {
            let eff_width = obj.outline.scaled_width_mm_for_transform(&world_xf);
            let world_outline_path =
                crate::boolean::stroke_to_closed_path(&world_route, &obj.outline, Some(eff_width))?;
            if let Some(pw) = parent_world {
                if let Some(inv_pw) = pw.inverse() {
                    world_outline_path.transformed(&inv_pw)
                } else {
                    world_outline_path
                }
            } else {
                world_outline_path
            }
        } else if world_xf.inverse().is_some() {
            let world_path = local_path.transformed(&world_xf);
            let eff_width = obj.outline.scaled_width_mm_for_transform(&world_xf);
            let world_outline_path =
                crate::boolean::stroke_to_closed_path(&world_path, &obj.outline, Some(eff_width))?;
            if let Some(pw) = parent_world {
                if let Some(inv_pw) = pw.inverse() {
                    world_outline_path.transformed(&inv_pw)
                } else {
                    let transformed_path = local_path.transformed(&obj.transform);
                    let local_eff = obj.outline.scaled_width_mm_for_transform(&obj.transform);
                    crate::boolean::stroke_to_closed_path(
                        &transformed_path,
                        &obj.outline,
                        Some(local_eff),
                    )?
                }
            } else {
                world_outline_path
            }
        } else {
            let transformed_path = local_path.transformed(&obj.transform);
            let eff_width = obj.outline.scaled_width_mm_for_transform(&obj.transform);
            crate::boolean::stroke_to_closed_path(&transformed_path, &obj.outline, Some(eff_width))?
        };

        if obj.fill.is_visible() {
            let mut new_obj = SceneObject::new(
                ObjectId(0),
                format!("{} (Outline)", obj.name),
                NodePayload::Path(outline_path),
            )
            .with_fill(Fill::Solid(obj.outline.color.clone()))
            .with_outline(OutlineStyle::none());
            new_obj.blend_mode = obj.blend_mode;
            new_obj.opacity = obj.opacity;

            if let Some(orig) = self.objects.get_mut(&id) {
                orig.outline = OutlineStyle::none();
            }
            let new_id =
                if self.layer_of_object(id).is_some() || self.parent_container_of(id).is_some() {
                    let new_id = self.alloc_object_id();
                    new_obj.id = new_id;
                    self.objects.insert(new_id, new_obj);
                    self.insert_siblings_after(id, &[new_id]);
                    new_id
                } else {
                    self.add_object(new_obj, None)?
                };
            self.touch();
            Ok(Some(new_id))
        } else {
            let stroke_color = obj.outline.color.clone();
            self.remove_orphaned_contents_before_overwrite(id, &orphaned_contents);
            if let Some(target) = self.objects.get_mut(&id) {
                target.payload = NodePayload::Path(outline_path);
                target.transform = Affine2D::IDENTITY;
                target.fill = Fill::Solid(stroke_color);
                target.outline = OutlineStyle::none();
            }
            self.touch();
            Ok(None)
        }
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
        let mut ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        self.resolve_connector_endpoints_inner(conn, world_xf, 0, &mut ctx, &mut visiting_syms)
            .0
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

    fn cached_parent_world_xf(&self, oid: ObjectId, ctx: &mut DocBoundsCtx) -> Affine2D {
        if let Some(&xf) = ctx.parent_world_cache.get(&oid) {
            return xf;
        }
        let xf = self
            .parent_container_of(oid)
            .and_then(|pid| self.world_transform_of(pid))
            .unwrap_or(Affine2D::IDENTITY);
        ctx.parent_world_cache.insert(oid, xf);
        xf
    }

    fn resolve_connector_endpoints_inner(
        &self,
        conn: &ConnectorObject,
        world_xf: &Affine2D,
        depth: usize,
        ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
    ) -> ((Point2D, Point2D), usize) {
        let mut min_backedge = usize::MAX;
        let from_pt = conn
            .from_obj
            .and_then(|oid| {
                let parent_xf = self.cached_parent_world_xf(oid, ctx);
                let (bb, backedge) = self.object_bounds_inner_with_space(
                    oid,
                    &parent_xf,
                    depth + 1,
                    ctx,
                    visiting_syms,
                    None,
                );
                min_backedge = min_backedge.min(backedge);
                bb
            })
            .map_or_else(
                || world_xf.transform_point(conn.from_fallback),
                |bb| conn.from_anchor.resolve(bb),
            );
        let to_pt = conn
            .to_obj
            .and_then(|oid| {
                let parent_xf = self.cached_parent_world_xf(oid, ctx);
                let (bb, backedge) = self.object_bounds_inner_with_space(
                    oid,
                    &parent_xf,
                    depth + 1,
                    ctx,
                    visiting_syms,
                    None,
                );
                min_backedge = min_backedge.min(backedge);
                bb
            })
            .map_or_else(
                || world_xf.transform_point(conn.to_fallback),
                |bb| conn.to_anchor.resolve(bb),
            );
        ((from_pt, to_pt), min_backedge)
    }

    /// Computes the parent-container-local axis-aligned bounding box of `id`, accounting for its
    /// own `transform` and any nested group children (without walking ancestor containers;
    /// use [`Self::world_object_bounds`] when `id` may live inside a Group/PowerClip).
    #[must_use]
    pub fn object_bounds(&self, id: ObjectId) -> Option<Rect2D> {
        let mut ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        self.object_bounds_with_ctx(id, &mut ctx, &mut visiting_syms)
    }

    fn object_bounds_with_ctx(
        &self,
        id: ObjectId,
        ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
    ) -> Option<Rect2D> {
        let parent_world = self
            .parent_container_of(id)
            .and_then(|pid| self.world_transform_of(pid));
        let inv_parent = parent_world.and_then(|xf| xf.inverse());
        let parent_space = parent_world.as_ref().zip(inv_parent.as_ref());
        self.object_bounds_inner_with_space(
            id,
            &Affine2D::IDENTITY,
            0,
            ctx,
            visiting_syms,
            parent_space,
        )
        .0
    }

    /// Computes the full world-space axis-aligned bounding box of `id`, including any
    /// enclosing ancestor `Group` or `PowerClip` container transforms.
    #[must_use]
    pub fn world_object_bounds(&self, id: ObjectId) -> Option<Rect2D> {
        let mut ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        self.world_object_bounds_with_visited(id, 0, &mut ctx, &mut visiting_syms)
            .0
    }

    /// Computes the conservative full visual world-space bounding box of `id`, including
    /// [`Effect::Symmetry`] clones, [`Effect::Blend`] targets/paths, [`Effect::Extrude25D`]
    /// vanishing points, effect overhang padding, scaled outlines, and `TextOnPath` glyph
    /// padding across `id` and any nested `Group`/`PowerClip`/`SymbolInstance` descendants.
    #[must_use]
    pub fn visual_world_bounds(&self, id: ObjectId) -> Option<Rect2D> {
        let mut bounds_ctx = DocBoundsCtx::new();
        let mut visiting = HashSet::new();
        let mut visiting_syms = HashSet::new();
        let parent_world = self.cached_parent_world_xf(id, &mut bounds_ctx);
        self.visual_world_bounds_inner(
            id,
            &parent_world,
            0,
            &mut visiting,
            &mut visiting_syms,
            &mut bounds_ctx,
        )
        .filter(|r| r.is_finite())
    }

    /// Computes [`Self::visual_world_bounds`] evaluated under an explicit enclosing
    /// parent container transform `parent_world_xf` instead of looking up `parent_container_of(id)`.
    #[must_use]
    pub fn visual_world_bounds_with_parent(
        &self,
        id: ObjectId,
        parent_world_xf: Affine2D,
    ) -> Option<Rect2D> {
        if !parent_world_xf.is_finite() {
            return None;
        }
        let mut bounds_ctx = DocBoundsCtx::new();
        let mut visiting = HashSet::new();
        let mut visiting_syms = HashSet::new();
        self.visual_world_bounds_inner(
            id,
            &parent_world_xf,
            0,
            &mut visiting,
            &mut visiting_syms,
            &mut bounds_ctx,
        )
        .filter(|r| r.is_finite())
    }

    fn single_obj_spatial_padding_mm(&self, obj: &SceneObject, world_xf: &Affine2D) -> f64 {
        let stroke_pad = if obj.outline.is_visible() {
            obj.outline
                .scaled_width_mm_for_transform(world_xf)
                .max(obj.outline.effective_width_mm())
                * 0.5
        } else {
            0.0
        };
        let top_pad = if let NodePayload::TextOnPath(top) = &obj.payload {
            let dist = if top.distance_mm.is_finite() {
                top.distance_mm.abs()
            } else {
                0.0
            };
            let max_font = top
                .text
                .runs
                .iter()
                .map(|r| r.style.font_size_mm())
                .filter(|v| v.is_finite())
                .fold(4.0_f64, f64::max);
            dist + max_font * 1.5
        } else {
            0.0
        };
        let pad = stroke_pad.max(top_pad);
        if pad.is_finite() {
            pad
        } else {
            0.0
        }
    }

    fn apply_deform_effects_world_bounds(
        effects: &[Effect],
        obj_world_xf: &Affine2D,
        local_h: Option<f64>,
        acc: &mut Rect2D,
    ) {
        if !obj_world_xf.is_finite() {
            return;
        }
        let union_pt = |local_pt: Point2D, out: &mut Rect2D| {
            if !local_pt.is_finite() {
                return;
            }
            let wp = obj_world_xf.transform_point(local_pt);
            if wp.is_finite() {
                let pr = Rect2D::new(wp.x, wp.y, wp.x, wp.y);
                *out = out.union(pr);
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
                        if matches!(kind, DistortKind::Twister) && center.is_finite() {
                            if let Some(inv_xf) = obj_world_xf.inverse() {
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
                                        if let Some(wb) = obj_world_xf.try_transform_rect(sq) {
                                            *acc = acc.union(wb);
                                        }
                                        cur_local_h =
                                            Some(cur_local_h.unwrap_or(0.0).max(2.0 * r_max));
                                    }
                                }
                            }
                        } else {
                            let pad = 2.0 * amplitude.abs();
                            let world_pad_x =
                                (obj_world_xf.m[0].abs() + obj_world_xf.m[2].abs()) * pad;
                            let world_pad_y =
                                (obj_world_xf.m[1].abs() + obj_world_xf.m[3].abs()) * pad;
                            if world_pad_x.is_finite() && world_pad_y.is_finite() {
                                *acc = Rect2D::new(
                                    acc.min_x - world_pad_x,
                                    acc.min_y - world_pad_y,
                                    acc.max_x + world_pad_x,
                                    acc.max_y + world_pad_y,
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
                    if cage.len() >= 8 {
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
                            if matches!(mode, EnvelopeMode::SingleArc | EnvelopeMode::DoubleArc) {
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

    fn has_4pt_arc_envelope(effects: &[Effect]) -> bool {
        effects.iter().any(|e| {
            matches!(
                e,
                Effect::Envelope {
                    mode: EnvelopeMode::SingleArc | EnvelopeMode::DoubleArc,
                    cage,
                } if cage.len() >= 4 && cage.len() < 8
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn sym_local_object_bounds(
        &self,
        sub: &SceneObject,
        rel_xf: &Affine2D,
        sub_world_xf: &Affine2D,
        target_world_xf: &Affine2D,
        depth: usize,
        ctx: &mut SymBoundsCtx<'_, '_>,
        bounds_ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
        visiting_local: &mut HashSet<ObjectId>,
    ) -> Option<Rect2D> {
        if depth > MAX_SCENE_DEPTH || !rel_xf.is_finite() || !target_world_xf.is_finite() {
            return None;
        }
        let inv_target_world = target_world_xf.inverse();
        let local_space = inv_target_world.as_ref().map(|inv| (target_world_xf, inv));
        let bounds = match &sub.payload {
            NodePayload::Path(p) => p.transformed(rel_xf).bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().transformed(rel_xf).bounding_box(),
            NodePayload::ArtisticText(t) => rel_xf.try_transform_rect(t.estimated_bounds()),
            NodePayload::ParagraphText(t) => rel_xf.try_transform_rect(t.bounding_box()),
            NodePayload::PowerClip { container, .. } => {
                container.transformed(rel_xf).bounding_box()
            }
            NodePayload::Table(t) => rel_xf.try_transform_rect(t.to_bounding_box()),
            NodePayload::Dimension(d) => rel_xf.try_transform_rect(d.bounding_box()),
            NodePayload::Raster(r) => {
                let rc = match r.crop_rect {
                    Some(cr) if cr.is_finite() => Some(cr),
                    Some(_) => None,
                    None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                        Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                    }
                    None => None,
                };
                rc.and_then(|rect| rel_xf.try_transform_rect(rect))
            }
            NodePayload::SymbolInstance(sym_id) => {
                self.symbol_bounds_inner(
                    *sym_id,
                    rel_xf,
                    depth + 1,
                    bounds_ctx,
                    visiting_syms,
                    local_space,
                )
                .0
            }
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                for &cid in children {
                    if !visiting_local.insert(cid) {
                        continue;
                    }
                    if let Some(child) = ctx.sym_map.get(&cid).copied() {
                        let child_rel = rel_xf.compose(&child.transform);
                        let child_world = sub_world_xf.compose(&child.transform);
                        if let Some(cb) = self.sym_local_object_bounds(
                            child,
                            &child_rel,
                            &child_world,
                            target_world_xf,
                            depth + 1,
                            ctx,
                            bounds_ctx,
                            visiting_syms,
                            visiting_local,
                        ) {
                            acc = Some(match acc {
                                Some(prev) => prev.union(cb),
                                None => cb,
                            });
                        }
                    }
                    visiting_local.remove(&cid);
                }
                acc
            }
            NodePayload::TextOnPath(top) => {
                if let Some(path_obj) = ctx.sym_map.get(&top.path_id).copied() {
                    Self::sym_world_transform_of(
                        top.path_id,
                        ctx.root_xf,
                        ctx.sym_objects,
                        ctx.sym_map,
                        ctx.sym_parent_map,
                    )
                    .and_then(|p_world| {
                        let p_local = inv_target_world
                            .as_ref()
                            .map_or(p_world, |inv| inv.compose(&p_world));
                        match &path_obj.payload {
                            NodePayload::Path(p) => p.transformed(&p_local).bounding_box(),
                            NodePayload::Shape(s) => {
                                s.to_bezier_path().transformed(&p_local).bounding_box()
                            }
                            NodePayload::PowerClip { container, .. } => {
                                container.transformed(&p_local).bounding_box()
                            }
                            _ => {
                                let (wbb, _) = self.sym_object_bounds_with_xf(
                                    top.path_id,
                                    &p_world,
                                    depth + 1,
                                    ctx,
                                    bounds_ctx,
                                    visiting_syms,
                                    None,
                                );
                                match (wbb, inv_target_world.as_ref()) {
                                    (Some(r), Some(inv)) => inv.try_transform_rect(r),
                                    (r, _) => r,
                                }
                            }
                        }
                    })
                    .or_else(|| rel_xf.try_transform_rect(top.text.estimated_bounds()))
                } else if let Some(inv) = inv_target_world.as_ref() {
                    self.world_path_of(top.path_id)
                        .and_then(|wp| wp.transformed(inv).bounding_box())
                        .or_else(|| {
                            self.world_object_bounds_with_visited(
                                top.path_id,
                                depth + 1,
                                bounds_ctx,
                                visiting_syms,
                            )
                            .0
                            .and_then(|r| inv.try_transform_rect(r))
                        })
                        .or_else(|| rel_xf.try_transform_rect(top.text.estimated_bounds()))
                } else {
                    self.world_path_of(top.path_id)
                        .and_then(|wp| wp.bounding_box())
                        .or_else(|| rel_xf.try_transform_rect(top.text.estimated_bounds()))
                }
            }
            NodePayload::Connector(c) => {
                let mut resolve_ep =
                    |oid_opt: Option<ObjectId>, anchor: &AnchorPoint, fallback: Point2D| {
                        oid_opt
                            .and_then(|oid| {
                                if ctx.sym_map.contains_key(&oid) {
                                    let p_xf = Self::sym_world_transform_of(
                                        oid,
                                        ctx.root_xf,
                                        ctx.sym_objects,
                                        ctx.sym_map,
                                        ctx.sym_parent_map,
                                    )?;
                                    self.sym_object_bounds_with_xf(
                                        oid,
                                        &p_xf,
                                        depth + 1,
                                        ctx,
                                        bounds_ctx,
                                        visiting_syms,
                                        None,
                                    )
                                    .0
                                } else {
                                    self.world_object_bounds_with_visited(
                                        oid,
                                        depth + 1,
                                        bounds_ctx,
                                        visiting_syms,
                                    )
                                    .0
                                }
                            })
                            .map_or_else(
                                || sub_world_xf.transform_point(fallback),
                                |bb| anchor.resolve(bb),
                            )
                    };
                let from_pt = resolve_ep(c.from_obj, &c.from_anchor, c.from_fallback);
                let to_pt = resolve_ep(c.to_obj, &c.to_anchor, c.to_fallback);
                let route = c.route_path(from_pt, to_pt);
                if let Some(inv) = inv_target_world.as_ref() {
                    route.transformed(inv).bounding_box()
                } else {
                    route.bounding_box()
                }
            }
        };
        bounds.filter(|r| r.is_finite())
    }

    fn visual_world_bounds_inner(
        &self,
        id: ObjectId,
        parent_world_xf: &Affine2D,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        bounds_ctx: &mut DocBoundsCtx,
    ) -> Option<Rect2D> {
        if depth > MAX_SCENE_DEPTH || !parent_world_xf.is_finite() || !visiting.insert(id) {
            return None;
        }
        let obj = match self.objects.get(&id) {
            Some(o) => o,
            None => {
                visiting.remove(&id);
                return None;
            }
        };
        let obj_world_xf = parent_world_xf.compose(&obj.transform);
        if !obj_world_xf.is_finite() {
            visiting.remove(&id);
            return None;
        }
        let base_bb = match &obj.payload {
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                for &cid in children {
                    if let Some(cb) = self.visual_world_bounds_inner(
                        cid,
                        &obj_world_xf,
                        depth + 1,
                        visiting,
                        visiting_syms,
                        bounds_ctx,
                    ) {
                        acc = Some(match acc {
                            Some(prev) => prev.union(cb),
                            None => cb,
                        });
                    }
                }
                acc
            }
            NodePayload::SymbolInstance(sym_id) => self.symbol_visual_bounds_inner(
                *sym_id,
                &obj_world_xf,
                depth + 1,
                visiting,
                visiting_syms,
                bounds_ctx,
            ),
            NodePayload::TextOnPath(top) => {
                let doc_parent_world = self.cached_parent_world_xf(id, bounds_ctx);
                if *parent_world_xf != doc_parent_world {
                    let delta_xf = parent_world_xf
                        .compose(&doc_parent_world.inverse().unwrap_or(Affine2D::IDENTITY));
                    self.world_path_of(top.path_id)
                        .and_then(|wp| wp.transformed(&delta_xf).bounding_box())
                        .or_else(|| obj_world_xf.try_transform_rect(top.text.estimated_bounds()))
                } else {
                    self.object_bounds_inner_with_space(
                        id,
                        parent_world_xf,
                        depth,
                        bounds_ctx,
                        visiting_syms,
                        None,
                    )
                    .0
                }
            }
            _ => {
                self.object_bounds_inner_with_space(
                    id,
                    parent_world_xf,
                    depth,
                    bounds_ctx,
                    visiting_syms,
                    None,
                )
                .0
            }
        };
        visiting.remove(&id);

        let mut acc = base_bb?;
        let local_h = Self::has_4pt_arc_envelope(&obj.effects)
            .then(|| self.local_object_bounds(id).map(|b| b.height()))
            .flatten();
        Self::apply_deform_effects_world_bounds(&obj.effects, &obj_world_xf, local_h, &mut acc);
        if obj
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Symmetry { .. }))
        {
            let inv_parent = parent_world_xf.inverse().unwrap_or(Affine2D::IDENTITY);
            let seed = acc;
            for sym_xf in Effect::symmetry_transforms_for(&obj.effects) {
                let world_sym = parent_world_xf.compose(&sym_xf).compose(&inv_parent);
                if world_sym.is_finite() {
                    if let Some(sb) = world_sym.try_transform_rect(seed) {
                        acc = acc.union(sb);
                    }
                }
            }
        }
        for eff in &obj.effects {
            match eff {
                Effect::Blend {
                    target_id,
                    along_path,
                    ..
                } => {
                    if let Some(tb) = self
                        .world_object_bounds_with_visited(
                            *target_id,
                            depth + 1,
                            bounds_ctx,
                            visiting_syms,
                        )
                        .0
                    {
                        acc = acc.union(tb);
                    }
                    if let Some(apid) = along_path {
                        if let Some(ab) = self
                            .world_object_bounds_with_visited(
                                *apid,
                                depth + 1,
                                bounds_ctx,
                                visiting_syms,
                            )
                            .0
                        {
                            acc = acc.union(ab);
                        }
                    }
                }
                Effect::Extrude25D {
                    vanishing_point, ..
                } if vanishing_point.is_finite() => {
                    let vp_rect = Rect2D::from_xywh(vanishing_point.x, vanishing_point.y, 0.1, 0.1);
                    acc = acc.union(vp_rect);
                }
                _ => {}
            }
        }
        let pad = Effect::effects_bounds_padding_mm(&obj.effects)
            + self.single_obj_spatial_padding_mm(obj, &obj_world_xf);
        let out = if pad > 0.0 { acc.expand(pad) } else { acc };
        out.is_finite().then_some(out)
    }

    fn symbol_visual_bounds_inner(
        &self,
        sym_id: SymbolId,
        world_xf: &Affine2D,
        depth: usize,
        visiting_doc: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        bounds_ctx: &mut DocBoundsCtx,
    ) -> Option<Rect2D> {
        if depth > MAX_SCENE_DEPTH || !world_xf.is_finite() || !visiting_syms.insert(sym_id) {
            return None;
        }
        let res = self.symbols.get(&sym_id).and_then(|sym| {
            let mut sym_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
            let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
            let mut referenced_children: HashSet<ObjectId> = HashSet::new();
            for (idx, sub) in sym.objects.iter().enumerate() {
                if sub.id.0 != 0 {
                    sym_map.insert(sub.id, sub);
                }
                let parent_ref = if sub.id.0 != 0 {
                    SymParentRef::Id(sub.id)
                } else {
                    SymParentRef::RootIndex(idx)
                };
                match &sub.payload {
                    NodePayload::Group(c) => {
                        for &cid in c {
                            referenced_children.insert(cid);
                            if cid.0 != 0 {
                                sym_parent_map.entry(cid).or_insert(parent_ref);
                            }
                        }
                    }
                    NodePayload::PowerClip { contents, .. } => {
                        for &cid in contents {
                            referenced_children.insert(cid);
                            if cid.0 != 0 {
                                sym_parent_map.entry(cid).or_insert(parent_ref);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut visiting_sym_depth: HashMap<ObjectId, usize> = HashMap::new();
            let mut sym_bounds_cache: HashMap<(ObjectId, [u64; 6]), Option<Rect2D>> =
                HashMap::new();
            let mut ctx = SymBoundsCtx {
                root_xf: world_xf,
                sym_objects: &sym.objects,
                sym_map: &sym_map,
                sym_parent_map: &sym_parent_map,
                visiting_sym_depth: &mut visiting_sym_depth,
                sym_bounds_cache: &mut sym_bounds_cache,
            };
            let mut visiting_sym_visual: HashSet<ObjectId> = HashSet::new();
            let mut acc: Option<Rect2D> = None;
            for sub in &sym.objects {
                if sub.id.0 != 0 && referenced_children.contains(&sub.id) {
                    continue;
                }
                if let Some(bb) = self.sym_visual_object_bounds(
                    sub,
                    world_xf,
                    depth + 1,
                    &mut ctx,
                    &mut visiting_sym_visual,
                    visiting_doc,
                    visiting_syms,
                    bounds_ctx,
                ) {
                    acc = Some(match acc {
                        Some(prev) => prev.union(bb),
                        None => bb,
                    });
                }
            }
            acc
        });
        visiting_syms.remove(&sym_id);
        res
    }

    #[allow(clippy::too_many_arguments)]
    fn sym_visual_object_bounds(
        &self,
        sub: &SceneObject,
        parent_xf: &Affine2D,
        depth: usize,
        ctx: &mut SymBoundsCtx<'_, '_>,
        visiting_sym_visual: &mut HashSet<ObjectId>,
        visiting_doc: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        bounds_ctx: &mut DocBoundsCtx,
    ) -> Option<Rect2D> {
        if depth > MAX_SCENE_DEPTH || !parent_xf.is_finite() || !sub.transform.is_finite() {
            return None;
        }
        if sub.id.0 != 0 && !visiting_sym_visual.insert(sub.id) {
            return None;
        }
        let sub_xf = parent_xf.compose(&sub.transform);
        if !sub_xf.is_finite() {
            if sub.id.0 != 0 {
                visiting_sym_visual.remove(&sub.id);
            }
            return None;
        }
        let base_bb = match &sub.payload {
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                for &cid in children {
                    if let Some(child) = ctx.sym_map.get(&cid).copied() {
                        if let Some(r) = self.sym_visual_object_bounds(
                            child,
                            &sub_xf,
                            depth + 1,
                            ctx,
                            visiting_sym_visual,
                            visiting_doc,
                            visiting_syms,
                            bounds_ctx,
                        ) {
                            acc = Some(match acc {
                                Some(prev) => prev.union(r),
                                None => r,
                            });
                        }
                    }
                }
                acc
            }
            NodePayload::SymbolInstance(sym_id) => self.symbol_visual_bounds_inner(
                *sym_id,
                &sub_xf,
                depth + 1,
                visiting_doc,
                visiting_syms,
                bounds_ctx,
            ),
            _ if sub.id.0 != 0 => {
                self.sym_object_bounds_with_xf(
                    sub.id,
                    &sub_xf,
                    depth,
                    ctx,
                    bounds_ctx,
                    visiting_syms,
                    None,
                )
                .0
            }
            _ => {
                self.payload_bounds_with_ctx(
                    &sub.payload,
                    &sub_xf,
                    depth,
                    bounds_ctx,
                    visiting_syms,
                    Some(ctx),
                    None,
                )
                .0
            }
        };
        if sub.id.0 != 0 {
            visiting_sym_visual.remove(&sub.id);
        }
        let mut acc = base_bb?;
        let local_h = if Self::has_4pt_arc_envelope(&sub.effects) {
            let mut visiting_local = HashSet::new();
            if sub.id.0 != 0 {
                visiting_local.insert(sub.id);
            }
            self.sym_local_object_bounds(
                sub,
                &Affine2D::IDENTITY,
                &sub_xf,
                &sub_xf,
                depth,
                ctx,
                bounds_ctx,
                visiting_syms,
                &mut visiting_local,
            )
            .map(|b| b.height())
        } else {
            None
        };
        Self::apply_deform_effects_world_bounds(&sub.effects, &sub_xf, local_h, &mut acc);
        if sub
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Symmetry { .. }))
        {
            let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
            let seed = acc;
            for sym_xf in Effect::symmetry_transforms_for(&sub.effects) {
                let world_sym = parent_xf.compose(&sym_xf).compose(&inv_parent);
                if world_sym.is_finite() {
                    if let Some(sb) = world_sym.try_transform_rect(seed) {
                        acc = acc.union(sb);
                    }
                }
            }
        }
        for eff in &sub.effects {
            match eff {
                Effect::Blend {
                    target_id,
                    along_path,
                    ..
                } => {
                    let resolve_blend_ref =
                        |ref_id: ObjectId,
                         ctx: &mut SymBoundsCtx<'_, '_>,
                         bounds_ctx: &mut DocBoundsCtx,
                         visiting_syms: &mut HashSet<SymbolId>| {
                            if ctx.sym_map.contains_key(&ref_id) {
                                if let Some(p_xf) = Self::sym_world_transform_of(
                                    ref_id,
                                    ctx.root_xf,
                                    ctx.sym_objects,
                                    ctx.sym_map,
                                    ctx.sym_parent_map,
                                ) {
                                    return self
                                        .sym_object_bounds_with_xf(
                                            ref_id,
                                            &p_xf,
                                            depth + 1,
                                            ctx,
                                            bounds_ctx,
                                            visiting_syms,
                                            None,
                                        )
                                        .0;
                                }
                            }
                            self.world_object_bounds_with_visited(
                                ref_id,
                                depth + 1,
                                bounds_ctx,
                                visiting_syms,
                            )
                            .0
                        };
                    if let Some(tb) = resolve_blend_ref(*target_id, ctx, bounds_ctx, visiting_syms)
                    {
                        acc = acc.union(tb);
                    }
                    if let Some(apid) = along_path {
                        if let Some(ab) = resolve_blend_ref(*apid, ctx, bounds_ctx, visiting_syms) {
                            acc = acc.union(ab);
                        }
                    }
                }
                Effect::Extrude25D {
                    vanishing_point, ..
                } if vanishing_point.is_finite() => {
                    let vp_rect = Rect2D::from_xywh(vanishing_point.x, vanishing_point.y, 0.1, 0.1);
                    acc = acc.union(vp_rect);
                }
                _ => {}
            }
        }
        let pad = Effect::effects_bounds_padding_mm(&sub.effects)
            + self.single_obj_spatial_padding_mm(sub, &sub_xf);
        let out = if pad > 0.0 { acc.expand(pad) } else { acc };
        out.is_finite().then_some(out)
    }

    /// Computes the local-space bounding box of [`SymbolDef`] `sym_id` directly in
    /// symbol-local coordinates (with zero AABB-of-AABB distortion).
    #[must_use]
    pub fn symbol_local_bounds(&self, sym_id: SymbolId) -> Option<Rect2D> {
        self.symbol_bounds_with_xf(sym_id, &Affine2D::IDENTITY)
    }

    /// Computes the bounding box of [`SymbolDef`] `sym_id` directly under transform `xf`
    /// (with zero AABB-of-AABB distortion).
    #[must_use]
    pub fn symbol_bounds_with_xf(&self, sym_id: SymbolId, xf: &Affine2D) -> Option<Rect2D> {
        let mut ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        self.symbol_bounds_inner(sym_id, xf, 0, &mut ctx, &mut visiting_syms, None)
            .0
            .filter(|r| r.is_finite())
    }

    /// Computes the bounding box of [`SymbolDef`] `sym_id` directly under transform `xf`
    /// in an enclosing frame whose world transform is `space_world_xf`, projecting external
    /// `TextOnPath`/`Connector` references into that frame before taking bounding boxes.
    #[must_use]
    pub fn symbol_bounds_with_xf_and_space(
        &self,
        sym_id: SymbolId,
        xf: &Affine2D,
        space_world_xf: Option<&Affine2D>,
    ) -> Option<Rect2D> {
        let inv_space = space_world_xf.and_then(|w| w.inverse());
        let local_space = space_world_xf.zip(inv_space.as_ref());
        let mut ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        self.symbol_bounds_inner(sym_id, xf, 0, &mut ctx, &mut visiting_syms, local_space)
            .0
            .filter(|r| r.is_finite())
    }

    /// Computes the object-local bounding box of `id` (prior to `obj.transform` and any
    /// ancestor container transforms) directly in `id`-local coordinates with zero
    /// AABB-of-AABB distortion, matching `coree-render` effect/mesh local space.
    #[must_use]
    pub fn local_object_bounds(&self, id: ObjectId) -> Option<Rect2D> {
        let obj = self.objects.get(&id)?;
        let bounds = match &obj.payload {
            NodePayload::Path(p) => p.bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().bounding_box(),
            NodePayload::ArtisticText(t) => Some(t.estimated_bounds()),
            NodePayload::ParagraphText(t) => Some(t.bounding_box()),
            NodePayload::TextOnPath(top) => self
                .world_path_of(top.path_id)
                .and_then(|wp| {
                    let inv = self.world_transform_of(id)?.inverse()?;
                    wp.transformed(&inv).bounding_box()
                })
                .or_else(|| Some(top.text.estimated_bounds())),
            NodePayload::Raster(r) => match r.crop_rect {
                Some(cr) if cr.is_finite() => Some(cr),
                Some(_) => None,
                None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                    Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                }
                None => None,
            },
            NodePayload::Table(t) => Some(t.to_bounding_box()),
            NodePayload::Dimension(d) => Some(d.bounding_box()),
            NodePayload::PowerClip { container, .. } => container.bounding_box(),
            NodePayload::SymbolInstance(sym_id) => {
                let obj_world = self.world_transform_of(id);
                let inv_obj_world = obj_world.and_then(|xf| xf.inverse());
                let local_space = obj_world.as_ref().zip(inv_obj_world.as_ref());
                let mut ctx = DocBoundsCtx::new();
                ctx.visiting_depth.insert(id, 0);
                let mut visiting_syms = HashSet::new();
                self.symbol_bounds_inner(
                    *sym_id,
                    &Affine2D::IDENTITY,
                    0,
                    &mut ctx,
                    &mut visiting_syms,
                    local_space,
                )
                .0
            }
            NodePayload::Connector(c) => {
                let mut ctx = DocBoundsCtx::new();
                ctx.visiting_depth.insert(id, 0);
                let mut visiting_syms = HashSet::new();
                if let Some(world_xf) = self.world_transform_of(id) {
                    if let Some(inv_world) = world_xf.inverse() {
                        let ((a, b), _) = self.resolve_connector_endpoints_inner(
                            c,
                            &world_xf,
                            0,
                            &mut ctx,
                            &mut visiting_syms,
                        );
                        c.route_path(a, b).transformed(&inv_world).bounding_box()
                    } else if c.from_obj.is_none() && c.to_obj.is_none() {
                        c.route_path(c.from_fallback, c.to_fallback).bounding_box()
                    } else {
                        None
                    }
                } else if c.from_obj.is_none() && c.to_obj.is_none() {
                    c.route_path(c.from_fallback, c.to_fallback).bounding_box()
                } else {
                    None
                }
            }
            NodePayload::Group(children) => {
                let obj_world = self.world_transform_of(id);
                let inv_obj_world = obj_world.and_then(|xf| xf.inverse());
                let local_space = obj_world.as_ref().zip(inv_obj_world.as_ref());
                let mut ctx = DocBoundsCtx::new();
                ctx.visiting_depth.insert(id, 0);
                let mut visiting_syms = HashSet::new();
                let mut acc: Option<Rect2D> = None;
                for &cid in children {
                    if let (Some(bb), _) = self.object_bounds_inner_with_space(
                        cid,
                        &Affine2D::IDENTITY,
                        1,
                        &mut ctx,
                        &mut visiting_syms,
                        local_space,
                    ) {
                        acc = Some(match acc {
                            Some(prev) => prev.union(bb),
                            None => bb,
                        });
                    }
                }
                acc
            }
        };
        bounds.filter(|r| r.is_finite())
    }

    /// Returns conservative world-space margin (in mm) needed around `id`'s bounding box so
    /// wide outlines and `TextOnPath` glyph ascenders/offset are not clipped during spatial
    /// culling or bitmap rasterization.
    #[must_use]
    pub fn spatial_bounds_padding_mm(&self, id: ObjectId) -> f64 {
        let mut visiting = HashSet::new();
        let mut visiting_syms = HashSet::new();
        self.spatial_bounds_padding_inner(id, 0, &mut visiting, &mut visiting_syms)
    }

    fn spatial_bounds_padding_inner(
        &self,
        id: ObjectId,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
    ) -> f64 {
        if depth > MAX_SCENE_DEPTH || !visiting.insert(id) {
            return 0.0;
        }
        let pad = if let Some(obj) = self.objects.get(&id) {
            let world_xf = self.world_transform_of(id).unwrap_or(obj.transform);
            let stroke_pad = if obj.outline.is_visible() {
                obj.outline.scaled_width_mm_for_transform(&world_xf) * 0.5
            } else {
                0.0
            };
            let payload_pad = match &obj.payload {
                NodePayload::TextOnPath(top) => {
                    let dist = if top.distance_mm.is_finite() {
                        top.distance_mm.abs()
                    } else {
                        0.0
                    };
                    let max_font = top
                        .text
                        .runs
                        .iter()
                        .map(|r| r.style.font_size_mm())
                        .filter(|v| v.is_finite())
                        .fold(4.0_f64, f64::max);
                    dist + max_font * 1.5
                }
                NodePayload::Group(children) => children.iter().fold(0.0_f64, |acc, &cid| {
                    acc.max(self.spatial_bounds_padding_inner(
                        cid,
                        depth + 1,
                        visiting,
                        visiting_syms,
                    ))
                }),
                NodePayload::PowerClip { contents, .. } => {
                    contents.iter().fold(0.0_f64, |acc, &cid| {
                        acc.max(self.spatial_bounds_padding_inner(
                            cid,
                            depth + 1,
                            visiting,
                            visiting_syms,
                        ))
                    })
                }
                NodePayload::SymbolInstance(sym_id) if visiting_syms.insert(*sym_id) => {
                    let sym_pad = self.symbols.get(sym_id).map_or(0.0, |sym| {
                        sym.objects.iter().fold(0.0_f64, |acc, sub| {
                            if let NodePayload::TextOnPath(top) = &sub.payload {
                                let dist = if top.distance_mm.is_finite() {
                                    top.distance_mm.abs()
                                } else {
                                    0.0
                                };
                                let max_font = top
                                    .text
                                    .runs
                                    .iter()
                                    .map(|r| r.style.font_size_mm())
                                    .filter(|v| v.is_finite())
                                    .fold(4.0_f64, f64::max);
                                acc.max(dist + max_font * 1.5)
                            } else {
                                acc
                            }
                        })
                    });
                    visiting_syms.remove(sym_id);
                    sym_pad
                }
                _ => 0.0,
            };
            stroke_pad.max(payload_pad)
        } else {
            0.0
        };
        visiting.remove(&id);
        if pad.is_finite() {
            pad
        } else {
            0.0
        }
    }

    fn world_object_bounds_with_visited(
        &self,
        id: ObjectId,
        depth: usize,
        ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
    ) -> (Option<Rect2D>, usize) {
        let parent_xf = self.cached_parent_world_xf(id, ctx);
        self.object_bounds_inner_with_space(id, &parent_xf, depth, ctx, visiting_syms, None)
    }

    fn object_bounds_inner_with_space(
        &self,
        id: ObjectId,
        parent_xf: &Affine2D,
        depth: usize,
        ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
        pc_space: Option<(&Affine2D, &Affine2D)>,
    ) -> (Option<Rect2D>, usize) {
        if depth > MAX_SCENE_DEPTH {
            return (None, 0);
        }
        if !parent_xf.is_finite() {
            return (None, usize::MAX);
        }
        if let Some(&active_depth) = ctx.visiting_depth.get(&id) {
            return (None, active_depth);
        }
        let cache_key = pc_space
            .is_none()
            .then(|| (id, affine_cache_key(parent_xf)));
        if let Some(key) = cache_key {
            if let Some(&cached) = ctx.cache.get(&key) {
                return (cached, usize::MAX);
            }
        }
        let obj = match self.objects.get(&id) {
            Some(o) => o,
            None => return (None, usize::MAX),
        };
        let world_xf = parent_xf.compose(&obj.transform);
        if !world_xf.is_finite() {
            return (None, usize::MAX);
        }

        let my_depth = ctx.visiting_depth.len();
        ctx.visiting_depth.insert(id, my_depth);
        let (raw_bounds, min_backedge) = self.payload_bounds_with_ctx(
            &obj.payload,
            &world_xf,
            depth,
            ctx,
            visiting_syms,
            None,
            pc_space,
        );
        ctx.visiting_depth.remove(&id);
        let bounds = raw_bounds.filter(|r| r.is_finite());
        if min_backedge == usize::MAX {
            if let Some(key) = cache_key {
                ctx.cache.insert(key, bounds);
            }
            (bounds, usize::MAX)
        } else if min_backedge >= my_depth {
            (bounds, usize::MAX)
        } else {
            (bounds, min_backedge)
        }
    }

    fn symbol_bounds_inner(
        &self,
        sym_id: SymbolId,
        world_xf: &Affine2D,
        depth: usize,
        bounds_ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
        pc_space: Option<(&Affine2D, &Affine2D)>,
    ) -> (Option<Rect2D>, usize) {
        if depth > MAX_SCENE_DEPTH {
            return (None, 0);
        }
        if !world_xf.is_finite() {
            return (None, usize::MAX);
        }
        if !visiting_syms.insert(sym_id) {
            return (None, 0);
        }
        let mut overall_backedge = usize::MAX;
        let res = self.symbols.get(&sym_id).and_then(|sym| {
            let mut sym_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
            let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
            let mut referenced_children: HashSet<ObjectId> = HashSet::new();
            for (idx, sub) in sym.objects.iter().enumerate() {
                if sub.id.0 != 0 {
                    sym_map.insert(sub.id, sub);
                }
                let parent_ref = if sub.id.0 != 0 {
                    SymParentRef::Id(sub.id)
                } else {
                    SymParentRef::RootIndex(idx)
                };
                match &sub.payload {
                    NodePayload::Group(c) => {
                        for &cid in c {
                            referenced_children.insert(cid);
                            if cid.0 != 0 {
                                sym_parent_map.entry(cid).or_insert(parent_ref);
                            }
                        }
                    }
                    NodePayload::PowerClip { contents, .. } => {
                        for &cid in contents {
                            referenced_children.insert(cid);
                            if cid.0 != 0 {
                                sym_parent_map.entry(cid).or_insert(parent_ref);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut visiting_sym_depth: HashMap<ObjectId, usize> = HashMap::new();
            let mut sym_bounds_cache: HashMap<(ObjectId, [u64; 6]), Option<Rect2D>> =
                HashMap::new();
            let mut ctx = SymBoundsCtx {
                root_xf: world_xf,
                sym_objects: &sym.objects,
                sym_map: &sym_map,
                sym_parent_map: &sym_parent_map,
                visiting_sym_depth: &mut visiting_sym_depth,
                sym_bounds_cache: &mut sym_bounds_cache,
            };
            let mut acc: Option<Rect2D> = None;
            for sub in &sym.objects {
                if sub.id.0 != 0 && referenced_children.contains(&sub.id) {
                    continue;
                }
                let sub_xf = world_xf.compose(&sub.transform);
                if !sub_xf.is_finite() {
                    continue;
                }
                let (bb, backedge) = if sub.id.0 != 0 {
                    self.sym_object_bounds_with_xf(
                        sub.id,
                        &sub_xf,
                        depth + 1,
                        &mut ctx,
                        bounds_ctx,
                        visiting_syms,
                        pc_space,
                    )
                } else {
                    self.payload_bounds_with_ctx(
                        &sub.payload,
                        &sub_xf,
                        depth + 1,
                        bounds_ctx,
                        visiting_syms,
                        Some(&mut ctx),
                        pc_space,
                    )
                };
                if backedge == 0 {
                    overall_backedge = 0;
                }
                if let Some(r) = bb {
                    acc = Some(match acc {
                        Some(prev) => prev.union(r),
                        None => r,
                    });
                }
            }
            acc
        });
        visiting_syms.remove(&sym_id);
        (res, overall_backedge)
    }

    #[allow(clippy::too_many_arguments)]
    fn sym_object_bounds_with_xf(
        &self,
        id: ObjectId,
        sub_xf: &Affine2D,
        depth: usize,
        ctx: &mut SymBoundsCtx<'_, '_>,
        bounds_ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
        pc_space: Option<(&Affine2D, &Affine2D)>,
    ) -> (Option<Rect2D>, usize) {
        if depth > MAX_SCENE_DEPTH {
            return (None, 0);
        }
        if !sub_xf.is_finite() {
            return (None, usize::MAX);
        }
        if let Some(&active_depth) = ctx.visiting_sym_depth.get(&id) {
            return (None, active_depth);
        }
        let cache_key = pc_space.is_none().then(|| (id, affine_cache_key(sub_xf)));
        if let Some(key) = cache_key {
            if let Some(&cached) = ctx.sym_bounds_cache.get(&key) {
                return (cached, usize::MAX);
            }
        }
        let sub = match ctx.sym_map.get(&id).copied() {
            Some(s) => s,
            None => return (None, usize::MAX),
        };
        let my_depth = ctx.visiting_sym_depth.len();
        ctx.visiting_sym_depth.insert(id, my_depth);
        let (raw_bounds, min_backedge) = self.payload_bounds_with_ctx(
            &sub.payload,
            sub_xf,
            depth,
            bounds_ctx,
            visiting_syms,
            Some(ctx),
            pc_space,
        );
        ctx.visiting_sym_depth.remove(&id);
        let bounds = raw_bounds.filter(|r| r.is_finite());
        if min_backedge == usize::MAX {
            if let Some(key) = cache_key {
                ctx.sym_bounds_cache.insert(key, bounds);
            }
            (bounds, usize::MAX)
        } else if min_backedge >= my_depth {
            (bounds, usize::MAX)
        } else {
            (bounds, min_backedge)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sym_object_bounds_inner(
        &self,
        id: ObjectId,
        parent_xf: &Affine2D,
        depth: usize,
        ctx: &mut SymBoundsCtx<'_, '_>,
        bounds_ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
        pc_space: Option<(&Affine2D, &Affine2D)>,
    ) -> (Option<Rect2D>, usize) {
        if depth > MAX_SCENE_DEPTH {
            return (None, 0);
        }
        if !parent_xf.is_finite() {
            return (None, usize::MAX);
        }
        let sub = match ctx.sym_map.get(&id).copied() {
            Some(s) => s,
            None => return (None, usize::MAX),
        };
        let sub_xf = parent_xf.compose(&sub.transform);
        if !sub_xf.is_finite() {
            return (None, usize::MAX);
        }
        self.sym_object_bounds_with_xf(id, &sub_xf, depth, ctx, bounds_ctx, visiting_syms, pc_space)
    }

    fn sym_world_transform_of(
        id: ObjectId,
        sym_root_xf: &Affine2D,
        sym_objects: &[SceneObject],
        sym_map: &HashMap<ObjectId, &SceneObject>,
        sym_parent_map: &HashMap<ObjectId, SymParentRef>,
    ) -> Option<Affine2D> {
        let mut chain = Vec::new();
        let mut cur = SymParentRef::Id(id);
        let mut seen = HashSet::new();
        while seen.insert(cur) {
            let obj = match cur {
                SymParentRef::Id(oid) => sym_map.get(&oid).copied()?,
                SymParentRef::RootIndex(idx) => sym_objects.get(idx)?,
            };
            chain.push(obj.transform);
            match cur {
                SymParentRef::Id(oid) => match sym_parent_map.get(&oid).copied() {
                    Some(pid) => cur = pid,
                    None => break,
                },
                SymParentRef::RootIndex(_) => break,
            }
        }
        let mut xf = *sym_root_xf;
        for local in chain.into_iter().rev() {
            xf = xf.compose(&local);
        }
        xf.is_finite().then_some(xf)
    }

    #[allow(clippy::too_many_arguments)]
    fn payload_bounds_with_ctx(
        &self,
        payload: &NodePayload,
        world_xf: &Affine2D,
        depth: usize,
        bounds_ctx: &mut DocBoundsCtx,
        visiting_syms: &mut HashSet<SymbolId>,
        sym_ctx: Option<&mut SymBoundsCtx<'_, '_>>,
        pc_space: Option<(&Affine2D, &Affine2D)>,
    ) -> (Option<Rect2D>, usize) {
        if depth > MAX_SCENE_DEPTH {
            return (None, 0);
        }
        if !world_xf.is_finite() {
            return (None, usize::MAX);
        }
        let mut min_backedge = usize::MAX;
        let bounds = match payload {
            NodePayload::Path(p) => p.transformed(world_xf).bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().transformed(world_xf).bounding_box(),
            NodePayload::ArtisticText(t) => world_xf.try_transform_rect(t.estimated_bounds()),
            NodePayload::ParagraphText(t) => world_xf.try_transform_rect(t.bounding_box()),
            NodePayload::TextOnPath(top) => {
                if let Some(ctx) = sym_ctx {
                    if let Some(path_obj) = ctx.sym_map.get(&top.path_id).copied() {
                        Self::sym_world_transform_of(
                            top.path_id,
                            ctx.root_xf,
                            ctx.sym_objects,
                            ctx.sym_map,
                            ctx.sym_parent_map,
                        )
                        .and_then(|p_xf| match &path_obj.payload {
                            NodePayload::Path(p) => p.transformed(&p_xf).bounding_box(),
                            NodePayload::Shape(s) => {
                                s.to_bezier_path().transformed(&p_xf).bounding_box()
                            }
                            _ => {
                                let (bb, backedge) = self.sym_object_bounds_with_xf(
                                    top.path_id,
                                    &p_xf,
                                    depth + 1,
                                    ctx,
                                    bounds_ctx,
                                    visiting_syms,
                                    pc_space,
                                );
                                min_backedge = min_backedge.min(backedge);
                                bb
                            }
                        })
                        .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
                    } else if let Some((_, inv_pc)) = pc_space {
                        self.world_path_of(top.path_id)
                            .and_then(|wp| wp.transformed(inv_pc).bounding_box())
                            .or_else(|| {
                                let (wbb, backedge) = self.world_object_bounds_with_visited(
                                    top.path_id,
                                    depth + 1,
                                    bounds_ctx,
                                    visiting_syms,
                                );
                                min_backedge = min_backedge.min(backedge);
                                wbb.and_then(|r| inv_pc.try_transform_rect(r))
                            })
                            .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
                    } else {
                        self.world_path_of(top.path_id)
                            .and_then(|wp| wp.bounding_box())
                            .or_else(|| {
                                let (wbb, backedge) = self.world_object_bounds_with_visited(
                                    top.path_id,
                                    depth + 1,
                                    bounds_ctx,
                                    visiting_syms,
                                );
                                min_backedge = min_backedge.min(backedge);
                                wbb
                            })
                            .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
                    }
                } else if let Some((_, inv_pc)) = pc_space {
                    self.world_path_of(top.path_id)
                        .and_then(|wp| wp.transformed(inv_pc).bounding_box())
                        .or_else(|| {
                            let (wbb, backedge) = self.world_object_bounds_with_visited(
                                top.path_id,
                                depth + 1,
                                bounds_ctx,
                                visiting_syms,
                            );
                            min_backedge = min_backedge.min(backedge);
                            wbb.and_then(|r| inv_pc.try_transform_rect(r))
                        })
                        .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
                } else {
                    self.world_path_of(top.path_id)
                        .and_then(|wp| wp.bounding_box())
                        .or_else(|| {
                            let (wbb, backedge) = self.world_object_bounds_with_visited(
                                top.path_id,
                                depth + 1,
                                bounds_ctx,
                                visiting_syms,
                            );
                            min_backedge = min_backedge.min(backedge);
                            wbb
                        })
                        .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds()))
                }
            }
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
                if let Some(ctx) = sym_ctx {
                    let mut resolve_ep =
                        |oid_opt: Option<ObjectId>, anchor: &AnchorPoint, fallback: Point2D| {
                            if let Some((pc_world, _)) = pc_space {
                                if let Some(oid) = oid_opt {
                                    if ctx.sym_map.contains_key(&oid) {
                                        if let Some(p_xf) = Self::sym_world_transform_of(
                                            oid,
                                            ctx.root_xf,
                                            ctx.sym_objects,
                                            ctx.sym_map,
                                            ctx.sym_parent_map,
                                        ) {
                                            let (bb, backedge) = self.sym_object_bounds_with_xf(
                                                oid,
                                                &p_xf,
                                                depth + 1,
                                                ctx,
                                                bounds_ctx,
                                                visiting_syms,
                                                pc_space,
                                            );
                                            min_backedge = min_backedge.min(backedge);
                                            if let Some(bb) = bb {
                                                return pc_world
                                                    .transform_point(anchor.resolve(bb));
                                            }
                                        }
                                    } else {
                                        let (bb, backedge) = self.world_object_bounds_with_visited(
                                            oid,
                                            depth + 1,
                                            bounds_ctx,
                                            visiting_syms,
                                        );
                                        min_backedge = min_backedge.min(backedge);
                                        if let Some(bb) = bb {
                                            return anchor.resolve(bb);
                                        }
                                    }
                                }
                                pc_world.compose(world_xf).transform_point(fallback)
                            } else {
                                oid_opt
                                    .and_then(|oid| {
                                        if ctx.sym_map.contains_key(&oid) {
                                            let p_xf = Self::sym_world_transform_of(
                                                oid,
                                                ctx.root_xf,
                                                ctx.sym_objects,
                                                ctx.sym_map,
                                                ctx.sym_parent_map,
                                            )?;
                                            let (bb, backedge) = self.sym_object_bounds_with_xf(
                                                oid,
                                                &p_xf,
                                                depth + 1,
                                                ctx,
                                                bounds_ctx,
                                                visiting_syms,
                                                None,
                                            );
                                            min_backedge = min_backedge.min(backedge);
                                            bb
                                        } else {
                                            let (bb, backedge) = self
                                                .world_object_bounds_with_visited(
                                                    oid,
                                                    depth + 1,
                                                    bounds_ctx,
                                                    visiting_syms,
                                                );
                                            min_backedge = min_backedge.min(backedge);
                                            bb
                                        }
                                    })
                                    .map_or_else(
                                        || world_xf.transform_point(fallback),
                                        |bb| anchor.resolve(bb),
                                    )
                            }
                        };
                    let from_pt = resolve_ep(c.from_obj, &c.from_anchor, c.from_fallback);
                    let to_pt = resolve_ep(c.to_obj, &c.to_anchor, c.to_fallback);
                    let route = c.route_path(from_pt, to_pt);
                    if let Some((_, inv_pc)) = pc_space {
                        route.transformed(inv_pc).bounding_box()
                    } else {
                        route.bounding_box()
                    }
                } else if let Some((pc_world, inv_pc)) = pc_space {
                    let conn_world = pc_world.compose(world_xf);
                    let ((a, b), backedge) = self.resolve_connector_endpoints_inner(
                        c,
                        &conn_world,
                        depth,
                        bounds_ctx,
                        visiting_syms,
                    );
                    min_backedge = min_backedge.min(backedge);
                    c.route_path(a, b).transformed(inv_pc).bounding_box()
                } else {
                    let ((a, b), backedge) = self.resolve_connector_endpoints_inner(
                        c,
                        world_xf,
                        depth,
                        bounds_ctx,
                        visiting_syms,
                    );
                    min_backedge = min_backedge.min(backedge);
                    c.route_path(a, b).bounding_box()
                }
            }
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                if let Some(ctx) = sym_ctx {
                    for &cid in children {
                        let (bb, backedge) = self.sym_object_bounds_inner(
                            cid,
                            world_xf,
                            depth + 1,
                            ctx,
                            bounds_ctx,
                            visiting_syms,
                            pc_space,
                        );
                        min_backedge = min_backedge.min(backedge);
                        if let Some(r) = bb {
                            acc = Some(match acc {
                                Some(prev) => prev.union(r),
                                None => r,
                            });
                        }
                    }
                } else {
                    for &cid in children {
                        let (bb, backedge) = self.object_bounds_inner_with_space(
                            cid,
                            world_xf,
                            depth + 1,
                            bounds_ctx,
                            visiting_syms,
                            pc_space,
                        );
                        min_backedge = min_backedge.min(backedge);
                        if let Some(r) = bb {
                            acc = Some(match acc {
                                Some(prev) => prev.union(r),
                                None => r,
                            });
                        }
                    }
                }
                acc
            }
            NodePayload::PowerClip { container, .. } => {
                container.transformed(world_xf).bounding_box()
            }
            NodePayload::SymbolInstance(sym_id) => {
                let (bb, backedge) = self.symbol_bounds_inner(
                    *sym_id,
                    world_xf,
                    depth + 1,
                    bounds_ctx,
                    visiting_syms,
                    pc_space,
                );
                min_backedge = min_backedge.min(backedge);
                bb
            }
        };
        (bounds.filter(|r| r.is_finite()), min_backedge)
    }

    /// Combined world-space bounding box enclosing all objects in `ids`.
    #[must_use]
    pub fn combined_bounds(&self, ids: &[ObjectId]) -> Option<Rect2D> {
        let mut ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        let mut acc: Option<Rect2D> = None;
        for &id in ids {
            if let (Some(bb), _) =
                self.world_object_bounds_with_visited(id, 0, &mut ctx, &mut visiting_syms)
            {
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
        let mut visited_doc = HashSet::new();
        let mut visiting_syms = HashSet::new();
        for &id in ids {
            let parent_xf = self
                .parent_container_of(id)
                .and_then(|pid| self.world_transform_of(pid))
                .filter(|m| m.is_finite())
                .unwrap_or(Affine2D::IDENTITY);
            self.collect_world_paths(
                id,
                &parent_xf,
                0,
                &mut visited_doc,
                &mut visiting_syms,
                &mut flat_paths,
            );
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
        visited_doc: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        out: &mut Vec<BezierPath>,
    ) {
        if depth > MAX_SCENE_DEPTH || !parent_xf.is_finite() || !visited_doc.insert(id) {
            return;
        }
        let Some(obj) = self.objects.get(&id) else {
            return;
        };
        let world_xf = parent_xf.compose(&obj.transform);
        if !world_xf.is_finite() {
            return;
        }
        self.collect_obj_world_paths(
            obj,
            &world_xf,
            depth,
            visited_doc,
            visiting_syms,
            None,
            None,
            out,
        );
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn collect_obj_world_paths(
        &self,
        obj: &SceneObject,
        world_xf: &Affine2D,
        depth: usize,
        visited_doc: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        sym_ctx: Option<(
            &Affine2D,
            &[SceneObject],
            &HashMap<ObjectId, &SceneObject>,
            &HashMap<ObjectId, SymParentRef>,
        )>,
        mut sym_visited: Option<&mut HashSet<ObjectId>>,
        out: &mut Vec<BezierPath>,
    ) {
        if depth > MAX_SCENE_DEPTH || !world_xf.is_finite() {
            return;
        }
        match &obj.payload {
            NodePayload::Path(p) => out.push(p.transformed(world_xf)),
            NodePayload::Shape(s) => out.push(s.to_bezier_path().transformed(world_xf)),
            NodePayload::Table(t) => out.push(t.to_grid_paths().transformed(world_xf)),
            NodePayload::Connector(c) => {
                if let Some((sym_root_xf, sym_objects, sym_map, sym_parent_map)) = sym_ctx {
                    let mut bounds_ctx = DocBoundsCtx::new();
                    let mut visiting_sym_depth = HashMap::new();
                    let mut sym_bounds_cache = HashMap::new();
                    let mut s_ctx = SymBoundsCtx {
                        root_xf: sym_root_xf,
                        sym_objects,
                        sym_map,
                        sym_parent_map,
                        visiting_sym_depth: &mut visiting_sym_depth,
                        sym_bounds_cache: &mut sym_bounds_cache,
                    };
                    let resolve_ep =
                        |oid_opt: Option<ObjectId>,
                         anchor: &AnchorPoint,
                         fallback: Point2D,
                         bounds_ctx: &mut DocBoundsCtx,
                         s_ctx: &mut SymBoundsCtx<'_, '_>,
                         visiting_syms: &mut HashSet<SymbolId>| {
                            oid_opt
                                .and_then(|oid| {
                                    if s_ctx.sym_map.contains_key(&oid) {
                                        let p_xf = Self::sym_world_transform_of(
                                            oid,
                                            s_ctx.root_xf,
                                            s_ctx.sym_objects,
                                            s_ctx.sym_map,
                                            s_ctx.sym_parent_map,
                                        )?;
                                        self.sym_object_bounds_with_xf(
                                            oid,
                                            &p_xf,
                                            depth + 1,
                                            s_ctx,
                                            bounds_ctx,
                                            visiting_syms,
                                            None,
                                        )
                                        .0
                                    } else {
                                        self.world_object_bounds_with_visited(
                                            oid,
                                            depth + 1,
                                            bounds_ctx,
                                            visiting_syms,
                                        )
                                        .0
                                    }
                                })
                                .map_or_else(
                                    || world_xf.transform_point(fallback),
                                    |bb| anchor.resolve(bb),
                                )
                        };
                    let a = resolve_ep(
                        c.from_obj,
                        &c.from_anchor,
                        c.from_fallback,
                        &mut bounds_ctx,
                        &mut s_ctx,
                        visiting_syms,
                    );
                    let b = resolve_ep(
                        c.to_obj,
                        &c.to_anchor,
                        c.to_fallback,
                        &mut bounds_ctx,
                        &mut s_ctx,
                        visiting_syms,
                    );
                    out.push(c.route_path(a, b));
                } else {
                    let (a, b) = self.resolve_connector_endpoints_with_transform(c, world_xf);
                    out.push(c.route_path(a, b));
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                out.push(container.transformed(world_xf));
                for &cid in contents {
                    if let Some((_, _, sym_map, _)) = sym_ctx {
                        if let Some(sub) = sym_map.get(&cid).copied() {
                            if let Some(ref mut sv) = sym_visited {
                                if !sv.insert(cid) {
                                    continue;
                                }
                            }
                            let sub_xf = world_xf.compose(&sub.transform);
                            self.collect_obj_world_paths(
                                sub,
                                &sub_xf,
                                depth + 1,
                                visited_doc,
                                visiting_syms,
                                sym_ctx,
                                sym_visited.as_deref_mut(),
                                out,
                            );
                        }
                    } else {
                        self.collect_world_paths(
                            cid,
                            world_xf,
                            depth + 1,
                            visited_doc,
                            visiting_syms,
                            out,
                        );
                    }
                }
            }
            NodePayload::Group(children) => {
                for &cid in children {
                    if let Some((_, _, sym_map, _)) = sym_ctx {
                        if let Some(sub) = sym_map.get(&cid).copied() {
                            if let Some(ref mut sv) = sym_visited {
                                if !sv.insert(cid) {
                                    continue;
                                }
                            }
                            let sub_xf = world_xf.compose(&sub.transform);
                            self.collect_obj_world_paths(
                                sub,
                                &sub_xf,
                                depth + 1,
                                visited_doc,
                                visiting_syms,
                                sym_ctx,
                                sym_visited.as_deref_mut(),
                                out,
                            );
                        }
                    } else {
                        self.collect_world_paths(
                            cid,
                            world_xf,
                            depth + 1,
                            visited_doc,
                            visiting_syms,
                            out,
                        );
                    }
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if !visiting_syms.insert(*sym_id) {
                    return;
                }
                if let Some(sym) = self.symbols.get(sym_id) {
                    let mut sym_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                    let mut sym_parent_map: HashMap<ObjectId, SymParentRef> = HashMap::new();
                    let mut referenced_children: HashSet<ObjectId> = HashSet::new();
                    for (idx, sub) in sym.objects.iter().enumerate() {
                        if sub.id.0 != 0 {
                            sym_map.insert(sub.id, sub);
                        }
                        let parent_ref = if sub.id.0 != 0 {
                            SymParentRef::Id(sub.id)
                        } else {
                            SymParentRef::RootIndex(idx)
                        };
                        match &sub.payload {
                            NodePayload::Group(c) => {
                                for &cid in c {
                                    referenced_children.insert(cid);
                                    if cid.0 != 0 {
                                        sym_parent_map.entry(cid).or_insert(parent_ref);
                                    }
                                }
                            }
                            NodePayload::PowerClip { contents, .. } => {
                                for &cid in contents {
                                    referenced_children.insert(cid);
                                    if cid.0 != 0 {
                                        sym_parent_map.entry(cid).or_insert(parent_ref);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    let mut local_sym_visited = HashSet::new();
                    for sub in &sym.objects {
                        if sub.id.0 != 0
                            && (referenced_children.contains(&sub.id)
                                || !local_sym_visited.insert(sub.id))
                        {
                            continue;
                        }
                        let sub_xf = world_xf.compose(&sub.transform);
                        self.collect_obj_world_paths(
                            sub,
                            &sub_xf,
                            depth + 1,
                            visited_doc,
                            visiting_syms,
                            Some((world_xf, &sym.objects, &sym_map, &sym_parent_map)),
                            Some(&mut local_sym_visited),
                            out,
                        );
                    }
                }
                visiting_syms.remove(sym_id);
            }
            _ => {}
        }
    }

    /// Returns `true` if `layer` (owned by page index `p_idx`) applies to `target_page_index`.
    #[must_use]
    pub fn layer_applies_to_page(layer: &Layer, p_idx: usize, target_page_index: usize) -> bool {
        let is_odd = target_page_index.is_multiple_of(2); // 1-based Page 1 = index 0 = Odd
        layer.is_desktop_layer
            || match layer.master_scope {
                MasterLayerScope::None => p_idx == target_page_index,
                MasterLayerScope::AllPages => true,
                MasterLayerScope::OddPages => is_odd,
                MasterLayerScope::EvenPages => !is_odd,
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
        let mut bounds_ctx = DocBoundsCtx::new();
        let mut visiting_syms = HashSet::new();
        // Include cross-page shared Desktop / AllPages / OddPages / EvenPages layers from other pages,
        // matching `collect_effective_layers` applicability rules.
        for (p_idx, p) in self.pages.iter().enumerate() {
            if p_idx == page_index {
                continue;
            }
            for layer in &p.layers {
                if !layer.visible || !Self::layer_applies_to_page(layer, p_idx, page_index) {
                    continue;
                }
                for &oid in &layer.objects {
                    if let Some(obj) = self.objects.get(&oid) {
                        if obj.visible {
                            if let Some(bounds) = self
                                .object_bounds_with_ctx(oid, &mut bounds_ctx, &mut visiting_syms)
                                .filter(|b| b.is_finite())
                            {
                                let pad = self.spatial_bounds_padding_mm(oid);
                                entries.push(SpatialObjectEntry {
                                    id: oid,
                                    z_order: z,
                                    bounds: if pad > 0.0 {
                                        bounds.expand(pad)
                                    } else {
                                        bounds
                                    },
                                });
                            }
                        }
                    }
                    z += 1;
                }
            }
        }
        for layer in &page.layers {
            if !layer.visible || !Self::layer_applies_to_page(layer, page_index, page_index) {
                continue;
            }
            for &oid in &layer.objects {
                if let Some(obj) = self.objects.get(&oid) {
                    if obj.visible {
                        if let Some(bounds) = self
                            .object_bounds_with_ctx(oid, &mut bounds_ctx, &mut visiting_syms)
                            .filter(|b| b.is_finite())
                        {
                            let pad = self.spatial_bounds_padding_mm(oid);
                            entries.push(SpatialObjectEntry {
                                id: oid,
                                z_order: z,
                                bounds: if pad > 0.0 {
                                    bounds.expand(pad)
                                } else {
                                    bounds
                                },
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
    /// skipping objects residing on locked or invisible layers (including cross-page master/desktop layers).
    #[must_use]
    pub fn hit_test(&self, pt: Point2D, tolerance_mm: f64) -> Option<ObjectId> {
        let mut locked_layer_ids = HashSet::new();
        let active_idx = self.active_page_index;
        for (p_idx, p) in self.pages.iter().enumerate() {
            for layer in &p.layers {
                let applies = Self::layer_applies_to_page(layer, p_idx, active_idx);
                if applies && (layer.locked || !layer.visible) {
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

    /// Deletes the page with `page_id` and cleans up its objects and relational links
    /// (fails if it's the last page).
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
        self.cleanup_dangling_refs_before_removal(&to_remove);
        for &oid in &to_remove {
            self.objects.remove(&oid);
        }
        self.detach_from_parents(&to_remove);
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

    /// Deletes `layer_id` from its owning page (preserving at least one layer per page,
    /// rejecting locked layers or layers containing locked objects, and cleaning up any
    /// objects/descendants and relational references on that layer).
    pub fn delete_layer(&mut self, layer_id: LayerId) -> CoreResult<Layer> {
        let mut target_pos = None;
        for (p_idx, page) in self.pages.iter().enumerate() {
            if let Some(l_idx) = page.layers.iter().position(|l| l.id == layer_id) {
                target_pos = Some((p_idx, l_idx));
                break;
            }
        }
        let (page_idx, layer_idx) = target_pos.ok_or(CoreError::LayerNotFound(layer_id))?;
        if self.pages[page_idx].layers.len() <= 1 {
            return Err(CoreError::CannotDeleteLastLayer);
        }
        if self.pages[page_idx].layers[layer_idx].locked {
            return Err(CoreError::InvalidParameter(format!(
                "layer {layer_id} is locked"
            )));
        }

        let mut to_remove = HashSet::new();
        for &oid in &self.pages[page_idx].layers[layer_idx].objects {
            if to_remove.insert(oid) {
                if let Some(obj) = self.objects.get(&oid) {
                    self.collect_descendants_from_payload(&obj.payload, &mut to_remove, 0);
                }
            }
        }
        for &oid in &to_remove {
            if self.objects.get(&oid).is_some_and(|o| o.locked) {
                return Err(CoreError::ObjectLocked(oid));
            }
        }

        let removed = self.pages[page_idx].layers.remove(layer_idx);
        if !to_remove.is_empty() {
            self.cleanup_dangling_refs_before_removal(&to_remove);
            for &oid in &to_remove {
                self.objects.remove(&oid);
            }
            self.detach_from_parents(&to_remove);
        }
        self.touch();
        Ok(removed)
    }

    /// Finds the cross-page shared `"Desktop"` layer ID, if present.
    #[must_use]
    pub fn desktop_layer_id(&self) -> Option<LayerId> {
        self.pages
            .iter()
            .flat_map(|p| &p.layers)
            .find(|l| l.is_desktop_layer || l.name.eq_ignore_ascii_case("Desktop"))
            .map(|l| l.id)
    }

    /// Returns an existing `"Desktop"` layer or creates a cross-page shared `"Desktop"`
    /// layer on Page 0 (`is_desktop_layer = true`, `master_scope = AllPages`).
    pub fn ensure_desktop_layer(&mut self) -> LayerId {
        if let Some(id) = self.desktop_layer_id() {
            if let Some(l) = self.find_layer_mut(id) {
                l.is_desktop_layer = true;
                l.master_scope = MasterLayerScope::AllPages;
            }
            return id;
        }
        let lid = LayerId(self.alloc_id());
        if let Some(p0) = self.pages.first_mut() {
            p0.layers.push(Layer::desktop(lid));
        }
        self.touch();
        lid
    }

    /// Moves `id` to the cross-page shared `"Desktop"` layer so it remains accessible across pages.
    pub fn move_object_to_desktop_layer(&mut self, id: ObjectId) -> CoreResult<LayerId> {
        if !self.objects.contains_key(&id) {
            return Err(CoreError::ObjectNotFound(id));
        }
        if self.is_effectively_locked(id) {
            return Err(CoreError::ObjectLocked(id));
        }
        if let Some(locked_child) = self.first_locked_in_subtree(id) {
            return Err(CoreError::ObjectLocked(locked_child));
        }
        if let Some(existing_desktop) = self.desktop_layer_id() {
            if self.find_layer(existing_desktop).is_some_and(|l| l.locked) {
                return Err(CoreError::ObjectLocked(id));
            }
        }
        let nested_world_xf = self
            .parent_container_of(id)
            .and_then(|_| self.world_transform_of(id));
        let desktop_id = self.ensure_desktop_layer();
        let mut set = HashSet::new();
        set.insert(id);
        self.detach_from_parents(&set);
        if let Some(wxf) = nested_world_xf {
            if let Some(obj) = self.objects.get_mut(&id) {
                obj.transform = wxf;
            }
        }
        if let Some(layer) = self.find_layer_mut(desktop_id) {
            if !layer.objects.contains(&id) {
                layer.objects.push(id);
            }
        }
        self.touch();
        Ok(desktop_id)
    }

    /// Validates and attaches a custom binary ICC profile (`.icc` / `.icm`) to this document.
    pub fn set_custom_icc_profile(&mut self, bytes: Vec<u8>) -> CoreResult<crate::icc::IccProfile> {
        let prof = crate::icc::IccProfile::from_bytes(&bytes)?;
        let desc = if prof.description.trim().is_empty() {
            "Custom ICC Profile".to_owned()
        } else {
            prof.description.clone()
        };
        self.custom_icc_name = Some(desc);
        self.custom_icc_profile = Some(bytes);
        self.touch();
        Ok(prof)
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

    /// Collects every [`ImageAssetId`] currently referenced by any object, object style, or symbol in `self`.
    #[must_use]
    pub fn referenced_asset_ids(&self) -> HashSet<ImageAssetId> {
        let mut refs = HashSet::new();
        for obj in self.objects.values() {
            obj.collect_asset_refs(&mut refs);
        }
        for sym in self.symbols.values() {
            for sub in &sym.objects {
                sub.collect_asset_refs(&mut refs);
            }
        }
        for style in &self.object_styles {
            if let Some(Fill::BitmapPattern { image_id, .. }) = style.fill {
                refs.insert(image_id);
            }
        }
        refs
    }

    /// Removes orphaned [`RasterAsset`] entries from `self.assets` that are neither referenced
    /// by any live document object/symbol/style nor present in `extra_kept` (such as active undo/redo history).
    /// Returns the count of pruned assets.
    pub fn prune_unreferenced_assets(&mut self, extra_kept: &HashSet<ImageAssetId>) -> usize {
        self.prune_unreferenced_assets_returning(extra_kept).len()
    }

    /// Removes orphaned [`RasterAsset`] entries from `self.assets`, returning the pruned [`RasterAsset`]s
    /// so command history deltas can restore them losslessly on `Undo`.
    pub fn prune_unreferenced_assets_returning(
        &mut self,
        extra_kept: &HashSet<ImageAssetId>,
    ) -> Vec<RasterAsset> {
        if self.assets.is_empty() {
            return Vec::new();
        }
        let mut live = self.referenced_asset_ids();
        live.extend(extra_kept.iter().copied());
        let doomed_ids: Vec<ImageAssetId> = self
            .assets
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        let mut pruned = Vec::with_capacity(doomed_ids.len());
        for id in doomed_ids {
            if let Some(asset) = self.assets.remove(&id) {
                pruned.push(asset);
            }
        }
        pruned
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

    #[test]
    fn master_odd_even_spatial_index_grouped_connector_and_asset_gc() {
        use crate::command::{CommandHistory, CommandOp};
        use crate::dimension::{AnchorPoint, ConnectorObject, ConnectorStyle};

        let mut doc = Document::new_a4("Atomic Bugs Regression");
        let p1 = doc.pages[0].id;
        let _p2 = doc.add_page("Page 2", 210.0, 297.0, 3.0);
        let p3 = doc.add_page("Page 3", 210.0, 297.0, 3.0);
        let _ = p3;

        // 1. OddPages / EvenPages master layers appear on matching pages in build_spatial_index
        let odd_layer = doc
            .add_layer(p1, "OddFooter", false, MasterLayerScope::OddPages)
            .unwrap();
        let even_layer = doc
            .add_layer(p1, "EvenFooter", false, MasterLayerScope::EvenPages)
            .unwrap();
        let odd_obj = doc
            .add_object(
                make_rect_obj(10.0, 10.0, 20.0, 10.0, Color::CYAN),
                Some(odd_layer),
            )
            .unwrap();
        let even_obj = doc
            .add_object(
                make_rect_obj(10.0, 30.0, 20.0, 10.0, Color::MAGENTA),
                Some(even_layer),
            )
            .unwrap();

        let p1_ids: Vec<ObjectId> = doc
            .build_spatial_index(0)
            .query_rect(Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));
        assert!(
            p1_ids.contains(&odd_obj),
            "odd master included on owning Page 1"
        );
        assert!(
            !p1_ids.contains(&even_obj),
            "even master excluded on owning Page 1"
        );
        assert_eq!(doc.hit_test(Point2D::new(20.0, 15.0), 1.0), Some(odd_obj));
        assert_eq!(doc.hit_test(Point2D::new(20.0, 35.0), 1.0), None);

        let p2_ids: Vec<ObjectId> = doc
            .build_spatial_index(1)
            .query_rect(Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));
        assert!(!p2_ids.contains(&odd_obj), "odd master skipped on Page 2");
        assert!(p2_ids.contains(&even_obj), "even master included on Page 2");

        let p3_ids: Vec<ObjectId> = doc
            .build_spatial_index(2)
            .query_rect(Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));
        assert!(p3_ids.contains(&odd_obj), "odd master included on Page 3");
        assert!(!p3_ids.contains(&even_obj), "even master skipped on Page 3");

        // 2. Connector anchored to grouped child resolves full ancestor world coordinates
        doc.active_page_index = 0;
        let child = doc
            .add_object(make_rect_obj(10.0, 10.0, 20.0, 10.0, Color::YELLOW), None)
            .unwrap();
        let partner = doc
            .add_object(make_rect_obj(100.0, 100.0, 20.0, 10.0, Color::CYAN), None)
            .unwrap();
        let grp = doc.group_objects(&[child, partner], "Group").unwrap();
        doc.transform_object(grp, Affine2D::translate(50.0, 40.0))
            .unwrap();
        let conn = ConnectorObject {
            style: ConnectorStyle::Straight,
            from_obj: Some(child),
            to_obj: None,
            from_anchor: AnchorPoint::TopLeft,
            to_anchor: AnchorPoint::TopLeft,
            from_fallback: Point2D::ZERO,
            to_fallback: Point2D::new(200.0, 200.0),
            corner_radius_mm: 0.0,
        };
        let (start_pt, _) = doc.resolve_connector_endpoints(&conn);
        assert!(
            (start_pt.x - 60.0).abs() < 1e-6 && (start_pt.y - 50.0).abs() < 1e-6,
            "grouped target top-left should include ancestor group offset (got {start_pt:?})"
        );

        // 3. CommandHistory keeps undoable raster asset alive while pruning replaced redo-orphaned assets
        let a1 = doc
            .register_asset(RasterAsset::new(2, 2, vec![10; 16], 300.0, ColorMode::Rgb).unwrap());
        let a2 = RasterAsset::new(2, 2, vec![20; 16], 300.0, ColorMode::Rgb).unwrap();
        let a2_id = a2.id;
        let a3 = RasterAsset::new(2, 2, vec![30; 16], 300.0, ColorMode::Rgb).unwrap();
        let a3_id = a3.id;

        let raster_obj = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Bmp",
                    NodePayload::Raster(RasterLayerData {
                        asset_id: a1,
                        width_mm: 10.0,
                        height_mm: 10.0,
                        filters: Vec::new(),
                        crop_rect: None,
                    }),
                ),
                None,
            )
            .unwrap();

        let mut history = CommandHistory::new(2);
        doc.assets.insert(a2_id, a2);
        history
            .execute(
                &mut doc,
                CommandOp::UpdateRaster {
                    id: raster_obj,
                    data: RasterLayerData {
                        asset_id: a2_id,
                        width_mm: 10.0,
                        height_mm: 10.0,
                        filters: Vec::new(),
                        crop_rect: None,
                    },
                },
            )
            .unwrap();
        history.undo(&mut doc).unwrap();
        // Now overwrite redo branch with a3: a2 should be garbage-collected automatically!
        doc.assets.insert(a3_id, a3);
        history
            .execute(
                &mut doc,
                CommandOp::UpdateRaster {
                    id: raster_obj,
                    data: RasterLayerData {
                        asset_id: a3_id,
                        width_mm: 10.0,
                        height_mm: 10.0,
                        filters: Vec::new(),
                        crop_rect: None,
                    },
                },
            )
            .unwrap();
        assert!(doc.assets.contains_key(&a1), "undo entry must preserve a1");
        assert!(doc.assets.contains_key(&a3_id), "live object uses a3");
        assert!(
            !doc.assets.contains_key(&a2_id),
            "evicted redo asset a2 should be reclaimed"
        );
    }
}
