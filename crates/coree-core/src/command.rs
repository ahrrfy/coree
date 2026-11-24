// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic serializable command log, transactional undo/redo history,
//! JSON replay, and Record-Macro -> Rhai script export & sandboxed execution.

use crate::boolean::{apply_boolean, apply_boolean_many, BooleanOp};
use crate::color::{Color, ColorMode, ColorPalette, ColorStyle, ColorStyleId};
use crate::dimension::{AnchorPoint, DimensionObject};
use crate::document::{
    AlignOp, DistributeOp, Document, DocumentMetadata, GridConfig, Guide, GuideOrientation,
    MasterLayerScope, NodePayload, Page, PerspectiveGridConfig, PowerClipFitMode, RasterAsset,
    RasterLayerData, SceneObject, SymbolDef, UnknownCdrChunk, ZOrderOp,
};
use crate::effects::{Effect, RasterFilter, TransparencyMode};
use crate::error::{CoreError, CoreResult};
use crate::find_replace::{find_and_replace, FindQuery, ReplaceAction};
use crate::geometry::{BezierPath, ParametricShape};
use crate::ids::{ImageAssetId, LayerId, ObjectId, ObjectStyleId, PageId, SymbolId};
use crate::lens::{LensConfig, LensKind};
use crate::math::{Affine2D, Point2D, Rect2D, Unit, Vec2D};
use crate::style::{BlendMode, Fill, ObjectStyle, OutlineStyle};
use crate::table::VectorTable;
use crate::text::TextStyle;
use rhai::{Dynamic, Engine, EvalAltResult};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

fn fill_sync_text_color(fill: &Fill) -> Option<Color> {
    match fill {
        Fill::Solid(c) => Some(c.clone()),
        Fill::None => Some(Color::TRANSPARENT),
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => stops.first().map(|s| s.color.clone()),
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => Some(fg.clone()),
        Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
        Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => None,
    }
}

fn sync_text_runs_fill(payload: &mut NodePayload, fill: &Fill) {
    let Some(col) = fill_sync_text_color(fill) else {
        return;
    };
    match payload {
        NodePayload::ArtisticText(t) => {
            for run in &mut t.runs {
                run.style.fill = col.clone();
            }
        }
        NodePayload::ParagraphText(p) => {
            for run in &mut p.runs {
                run.style.fill = col.clone();
            }
        }
        NodePayload::TextOnPath(top) => {
            for run in &mut top.text.runs {
                run.style.fill = col.clone();
            }
        }
        _ => {}
    }
}

/// Every document mutation represented as a deterministic, JSON-serializable command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandOp {
    CreateObject {
        object: Box<SceneObject>,
        layer_id: Option<LayerId>,
    },
    DeleteObjects {
        ids: Vec<ObjectId>,
    },
    TransformObjects {
        ids: Vec<ObjectId>,
        affine: Affine2D,
    },
    SetFill {
        ids: Vec<ObjectId>,
        fill: Fill,
    },
    SetOutline {
        ids: Vec<ObjectId>,
        outline: OutlineStyle,
    },
    SetLens {
        ids: Vec<ObjectId>,
        lens: Option<LensConfig>,
    },
    AddEffect {
        id: ObjectId,
        effect: Effect,
    },
    RemoveEffect {
        id: ObjectId,
        effect_index: usize,
    },
    EditPath {
        id: ObjectId,
        path: BezierPath,
    },
    BooleanOp {
        op: BooleanOp,
        ids: Vec<ObjectId>,
    },
    Group {
        ids: Vec<ObjectId>,
        name: String,
    },
    Ungroup {
        group_id: ObjectId,
    },
    SetZOrder {
        id: ObjectId,
        op: ZOrderOp,
    },
    AlignObjects {
        ids: Vec<ObjectId>,
        op: AlignOp,
    },
    DistributeObjects {
        ids: Vec<ObjectId>,
        op: DistributeOp,
    },
    AddPage {
        name: String,
        width_mm: f64,
        height_mm: f64,
        bleed_mm: f64,
    },
    DeletePage {
        page_id: PageId,
    },
    RenamePage {
        page_id: PageId,
        new_name: String,
    },
    AddLayer {
        page_id: PageId,
        name: String,
        is_cut_layer: bool,
        master_scope: MasterLayerScope,
    },
    SetLayerFlags {
        layer_id: LayerId,
        visible: Option<bool>,
        locked: Option<bool>,
        printable: Option<bool>,
        is_cut_layer: Option<bool>,
    },
    AddGuide {
        orientation: GuideOrientation,
        locked: bool,
    },
    MoveGuide {
        id: u64,
        orientation: GuideOrientation,
    },
    RemoveGuide {
        id: u64,
    },
    ToggleGuideLock {
        id: u64,
    },
    EditText {
        id: ObjectId,
        payload: NodePayload,
    },
    EditTable {
        id: ObjectId,
        table: VectorTable,
    },
    CreatePowerClip {
        container_id: ObjectId,
        content_ids: Vec<ObjectId>,
    },
    ExtractPowerClip {
        powerclip_id: ObjectId,
    },
    SetPowerClipLock {
        id: ObjectId,
        lock_contents: bool,
    },
    MoveToDesktopLayer {
        id: ObjectId,
    },
    ConvertToCurves {
        id: ObjectId,
    },
    ConvertOutlineToObject {
        id: ObjectId,
    },
    UpdateRaster {
        id: ObjectId,
        data: RasterLayerData,
    },
    DuplicateObjects {
        ids: Vec<ObjectId>,
        offset: Vec2D,
    },
    SetObjectVisibility {
        ids: Vec<ObjectId>,
        visible: bool,
    },
    SetObjectLocked {
        ids: Vec<ObjectId>,
        locked: bool,
    },
    SetObjectPrintable {
        ids: Vec<ObjectId>,
        printable: bool,
    },
    RenameObject {
        id: ObjectId,
        new_name: String,
    },
    SetOpacity {
        ids: Vec<ObjectId>,
        opacity: f32,
    },
    SetBlendMode {
        ids: Vec<ObjectId>,
        blend_mode: BlendMode,
    },
    SetDocumentProperties {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        color_mode: Option<ColorMode>,
        #[serde(default)]
        default_unit: Option<Unit>,
        #[serde(default)]
        dpi: Option<f32>,
        #[serde(default)]
        multipage_view: Option<bool>,
        #[serde(default)]
        active_page_index: Option<usize>,
        #[serde(default)]
        metadata: Option<DocumentMetadata>,
        #[serde(default)]
        custom_icc_profile: Option<Vec<u8>>,
        #[serde(default)]
        clear_custom_icc: bool,
    },
    SetPageProperties {
        page_id: PageId,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        width_mm: Option<f64>,
        #[serde(default)]
        height_mm: Option<f64>,
        #[serde(default)]
        bleed_mm: Option<f64>,
        #[serde(default)]
        facing_pages: Option<bool>,
    },
    ReorderPages {
        page_ids: Vec<PageId>,
    },
    RenameLayer {
        layer_id: LayerId,
        new_name: String,
    },
    ReorderLayerObjects {
        layer_id: LayerId,
        object_ids: Vec<ObjectId>,
    },
    SetGridConfig {
        config: GridConfig,
    },
    SetPerspectiveGrid {
        config: PerspectiveGridConfig,
    },
    UpsertColorStyle {
        style: ColorStyle,
    },
    DeleteColorStyle {
        id: ColorStyleId,
    },
    UpsertObjectStyle {
        style: ObjectStyle,
    },
    DeleteObjectStyle {
        id: ObjectStyleId,
    },
    SetObjectStyleLink {
        ids: Vec<ObjectId>,
        style_id: Option<ObjectStyleId>,
    },
    UpsertSymbol {
        symbol: SymbolDef,
    },
    DeleteSymbol {
        id: SymbolId,
    },
    InstantiateSymbol {
        symbol_id: SymbolId,
        #[serde(default)]
        layer_id: Option<LayerId>,
        #[serde(default)]
        transform: Affine2D,
        #[serde(default)]
        name: Option<String>,
    },
    UnlinkSymbolInstance {
        id: ObjectId,
    },
    FitPowerClip {
        id: ObjectId,
        mode: PowerClipFitMode,
    },
    FindAndReplace {
        query: FindQuery,
        action: ReplaceAction,
    },
    EditShape {
        id: ObjectId,
        shape: ParametricShape,
    },
    DeleteLayer {
        layer_id: LayerId,
    },
    RegisterAsset {
        asset: RasterAsset,
    },
    CreateDetachedObject {
        object: Box<SceneObject>,
    },
    EditDimension {
        id: ObjectId,
        dimension: DimensionObject,
    },
    PruneAssets {
        ids: Vec<ImageAssetId>,
    },
    ReplaceObjectInParent {
        id: ObjectId,
        replacements: Vec<SceneObject>,
    },
    Batch(Vec<CommandOp>),
}

impl CommandOp {
    /// Convenience constructor for [`CommandOp::CreateObject`].
    #[must_use]
    pub fn create_object(object: SceneObject, layer_id: Option<LayerId>) -> Self {
        Self::CreateObject {
            object: Box::new(object),
            layer_id,
        }
    }

    /// Convenience constructor for [`CommandOp::CreateDetachedObject`].
    #[must_use]
    pub fn create_detached_object(object: SceneObject) -> Self {
        Self::CreateDetachedObject {
            object: Box::new(object),
        }
    }

    /// Short human-readable label for the Undo/Redo history panel.
    #[must_use]
    pub fn default_label(&self) -> String {
        match self {
            Self::CreateObject { object, .. } => format!("Create {}", object.name),
            Self::DeleteObjects { ids } => format!("Delete {} Object(s)", ids.len()),
            Self::TransformObjects { .. } => "Transform Objects".to_owned(),
            Self::SetFill { .. } => "Set Fill".to_owned(),
            Self::SetOutline { .. } => "Set Outline".to_owned(),
            Self::SetLens { .. } => "Set Lens".to_owned(),
            Self::AddEffect { .. } => "Add Effect".to_owned(),
            Self::RemoveEffect { .. } => "Remove Effect".to_owned(),
            Self::EditPath { .. } => "Edit Path".to_owned(),
            Self::BooleanOp { op, .. } => format!("Boolean {op:?}"),
            Self::Group { name, .. } => format!("Group {name}"),
            Self::Ungroup { .. } => "Ungroup".to_owned(),
            Self::SetZOrder { op, .. } => format!("Order {op:?}"),
            Self::AlignObjects { op, .. } => format!("Align {op:?}"),
            Self::DistributeObjects { op, .. } => format!("Distribute {op:?}"),
            Self::AddPage { name, .. } => format!("Add Page {name}"),
            Self::DeletePage { .. } => "Delete Page".to_owned(),
            Self::RenamePage { new_name, .. } => format!("Rename Page {new_name}"),
            Self::AddLayer { name, .. } => format!("Add Layer {name}"),
            Self::SetLayerFlags { .. } => "Update Layer Flags".to_owned(),
            Self::AddGuide { .. } => "Add Guide".to_owned(),
            Self::MoveGuide { .. } => "Move Guide".to_owned(),
            Self::RemoveGuide { .. } => "Remove Guide".to_owned(),
            Self::ToggleGuideLock { .. } => "Toggle Guide Lock".to_owned(),
            Self::EditText { .. } => "Edit Text".to_owned(),
            Self::EditTable { .. } => "Edit Table".to_owned(),
            Self::CreatePowerClip { .. } => "Create PowerClip".to_owned(),
            Self::ExtractPowerClip { .. } => "Extract PowerClip".to_owned(),
            Self::SetPowerClipLock { .. } => "Toggle PowerClip Lock Contents".to_owned(),
            Self::MoveToDesktopLayer { .. } => "Move to Desktop Layer".to_owned(),
            Self::ConvertToCurves { .. } => "Convert to Curves".to_owned(),
            Self::ConvertOutlineToObject { .. } => "Convert Outline to Object".to_owned(),
            Self::UpdateRaster { .. } => "Update Raster".to_owned(),
            Self::DuplicateObjects { ids, .. } => format!("Duplicate {} Object(s)", ids.len()),
            Self::SetObjectVisibility { visible, .. } => {
                if *visible {
                    "Show Objects".to_owned()
                } else {
                    "Hide Objects".to_owned()
                }
            }
            Self::SetObjectLocked { locked, .. } => {
                if *locked {
                    "Lock Objects".to_owned()
                } else {
                    "Unlock Objects".to_owned()
                }
            }
            Self::SetObjectPrintable { .. } => "Set Object Printable".to_owned(),
            Self::RenameObject { new_name, .. } => format!("Rename {new_name}"),
            Self::SetOpacity { .. } => "Set Opacity".to_owned(),
            Self::SetBlendMode { .. } => "Set Blend Mode".to_owned(),
            Self::SetDocumentProperties { .. } => "Set Document Properties".to_owned(),
            Self::SetPageProperties { .. } => "Set Page Properties".to_owned(),
            Self::ReorderPages { .. } => "Reorder Pages".to_owned(),
            Self::RenameLayer { new_name, .. } => format!("Rename Layer {new_name}"),
            Self::ReorderLayerObjects { .. } => "Reorder Layer Objects".to_owned(),
            Self::SetGridConfig { .. } => "Set Grid Config".to_owned(),
            Self::SetPerspectiveGrid { .. } => "Set Perspective Grid".to_owned(),
            Self::UpsertColorStyle { style } => format!("Upsert Color Style {}", style.name),
            Self::DeleteColorStyle { .. } => "Delete Color Style".to_owned(),
            Self::UpsertObjectStyle { style } => format!("Upsert Object Style {}", style.name),
            Self::DeleteObjectStyle { .. } => "Delete Object Style".to_owned(),
            Self::SetObjectStyleLink { .. } => "Set Object Style Link".to_owned(),
            Self::UpsertSymbol { symbol } => format!("Upsert Symbol {}", symbol.name),
            Self::DeleteSymbol { .. } => "Delete Symbol".to_owned(),
            Self::InstantiateSymbol { .. } => "Instantiate Symbol".to_owned(),
            Self::UnlinkSymbolInstance { .. } => "Unlink Symbol Instance".to_owned(),
            Self::FitPowerClip { mode, .. } => format!("Fit PowerClip ({mode:?})"),
            Self::FindAndReplace { .. } => "Find & Replace".to_owned(),
            Self::EditShape { .. } => "Edit Shape".to_owned(),
            Self::DeleteLayer { .. } => "Delete Layer".to_owned(),
            Self::RegisterAsset { .. } => "Register Asset".to_owned(),
            Self::CreateDetachedObject { object } => format!("Create Nested {}", object.name),
            Self::EditDimension { .. } => "Edit Dimension".to_owned(),
            Self::PruneAssets { .. } => "Prune Unreferenced Assets".to_owned(),
            Self::ReplaceObjectInParent { .. } => "Replace Object in Parent".to_owned(),
            Self::Batch(ops) => format!("Batch ({} ops)", ops.len()),
        }
    }
}

fn color_is_finite(c: &Color) -> bool {
    match c {
        Color::Rgba { r, g, b, a } => {
            r.is_finite() && g.is_finite() && b.is_finite() && a.is_finite()
        }
        Color::Cmyk { c, m, y, k, a } => {
            c.is_finite() && m.is_finite() && y.is_finite() && k.is_finite() && a.is_finite()
        }
        Color::Hsl { h, s, l, a } => {
            h.is_finite() && s.is_finite() && l.is_finite() && a.is_finite()
        }
        Color::Lab {
            l,
            a_ch,
            b_ch,
            alpha,
        } => l.is_finite() && a_ch.is_finite() && b_ch.is_finite() && alpha.is_finite(),
        Color::Gray { v, a } => v.is_finite() && a.is_finite(),
        Color::Spot {
            tint,
            fallback_cmyk,
            alpha,
            ..
        } => tint.is_finite() && fallback_cmyk.iter().all(|v| v.is_finite()) && alpha.is_finite(),
    }
}

fn fill_is_finite(fill: &Fill) -> bool {
    match fill {
        Fill::None => true,
        Fill::Solid(c) => color_is_finite(c),
        Fill::LinearGradient { start, end, stops } => {
            start.is_finite()
                && end.is_finite()
                && stops
                    .iter()
                    .all(|s| s.offset.is_finite() && color_is_finite(&s.color))
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } => {
            center.is_finite()
                && radius.is_finite()
                && focal.is_finite()
                && stops
                    .iter()
                    .all(|s| s.offset.is_finite() && color_is_finite(&s.color))
        }
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            center.is_finite()
                && angle_deg.is_finite()
                && stops
                    .iter()
                    .all(|s| s.offset.is_finite() && color_is_finite(&s.color))
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            center.is_finite()
                && size.is_finite()
                && angle_deg.is_finite()
                && stops
                    .iter()
                    .all(|s| s.offset.is_finite() && color_is_finite(&s.color))
        }
        Fill::TwoColorPattern {
            fg,
            bg,
            tile_w,
            tile_h,
            ..
        } => color_is_finite(fg) && color_is_finite(bg) && tile_w.is_finite() && tile_h.is_finite(),
        Fill::VectorPattern { tile_w, tile_h, .. } | Fill::BitmapPattern { tile_w, tile_h, .. } => {
            tile_w.is_finite() && tile_h.is_finite()
        }
        Fill::TextureFill { fg, bg, .. } => color_is_finite(fg) && color_is_finite(bg),
        Fill::MeshFill { nodes, .. } => nodes
            .iter()
            .all(|n| n.pos.is_finite() && color_is_finite(&n.color)),
    }
}

fn outline_is_finite(o: &OutlineStyle) -> bool {
    color_is_finite(&o.color)
        && o.width_mm.is_finite()
        && o.miter_limit.is_finite()
        && o.dash_pattern.iter().all(|d| d.is_finite())
        && o.calligraphy_angle_deg.is_finite()
        && o.calligraphy_stretch.is_finite()
}

fn validate_outline_style(o: &OutlineStyle) -> CoreResult<()> {
    if !outline_is_finite(o) {
        return Err(CoreError::InvalidGeometry(
            "outline contains non-finite values".into(),
        ));
    }
    if o.width_mm < 0.0 || o.miter_limit < 0.0 {
        return Err(CoreError::InvalidParameter(
            "outline width_mm and miter_limit must be >= 0".into(),
        ));
    }
    Ok(())
}

fn guide_orientation_is_finite(o: GuideOrientation) -> bool {
    match o {
        GuideOrientation::Horizontal(v) | GuideOrientation::Vertical(v) => v.is_finite(),
        GuideOrientation::Angled { origin, angle_deg } => {
            origin.is_finite() && angle_deg.is_finite()
        }
    }
}

fn text_style_is_finite(s: &TextStyle) -> bool {
    s.font_size_pt.is_finite()
        && s.letter_spacing_mm.is_finite()
        && s.line_height_ratio.is_finite()
        && s.kashida_stretch.is_finite()
        && color_is_finite(&s.fill)
}

fn lens_is_finite(l: &LensConfig) -> bool {
    if !l.viewpoint.is_none_or(Point2D::is_finite) {
        return false;
    }
    match &l.kind {
        LensKind::Brighten { rate_pct } | LensKind::FishEye { rate_pct } => rate_pct.is_finite(),
        LensKind::ColorAdd { color, rate_pct }
        | LensKind::ColorLimit { color, rate_pct }
        | LensKind::Transparency { color, rate_pct } => {
            color_is_finite(color) && rate_pct.is_finite()
        }
        LensKind::CustomColorMap {
            from_color,
            to_color,
            ..
        } => color_is_finite(from_color) && color_is_finite(to_color),
        LensKind::HeatMap {
            palette_rotation_deg,
        } => palette_rotation_deg.is_finite(),
        LensKind::Invert => true,
        LensKind::Magnify { amount } => amount.is_finite(),
        LensKind::TintedGrayscale { tint } => color_is_finite(tint),
        LensKind::Wireframe {
            override_outline,
            override_fill,
        } => {
            override_outline.as_ref().is_none_or(color_is_finite)
                && override_fill.as_ref().is_none_or(color_is_finite)
        }
    }
}

fn effect_is_finite(e: &Effect) -> bool {
    match e {
        Effect::DropShadow {
            dx_mm,
            dy_mm,
            blur_mm,
            opacity,
            color,
            ..
        } => {
            dx_mm.is_finite()
                && dy_mm.is_finite()
                && blur_mm.is_finite()
                && opacity.is_finite()
                && color_is_finite(color)
        }
        Effect::BlockShadow {
            dx_mm,
            dy_mm,
            color,
            ..
        } => dx_mm.is_finite() && dy_mm.is_finite() && color_is_finite(color),
        Effect::Contour {
            offset_mm,
            fill_color,
            outline_color,
            ..
        } => offset_mm.is_finite() && color_is_finite(fill_color) && color_is_finite(outline_color),
        Effect::Blend { rotation_deg, .. } => rotation_deg.is_finite(),
        Effect::Distort {
            amplitude,
            frequency,
            center,
            ..
        } => amplitude.is_finite() && frequency.is_finite() && center.is_finite(),
        Effect::Envelope { cage, .. } => cage.iter().all(|p| p.is_finite()),
        Effect::Extrude25D {
            vanishing_point,
            depth,
            bevel_depth,
            shade_color,
            light_dir,
        } => {
            vanishing_point.is_finite()
                && depth.is_finite()
                && bevel_depth.is_finite()
                && color_is_finite(shade_color)
                && light_dir.iter().all(|v| v.is_finite())
        }
        Effect::Transparency { mode, .. } => match mode {
            TransparencyMode::Uniform(a) => a.is_finite(),
            TransparencyMode::FountainLinear {
                start,
                end,
                start_alpha,
                end_alpha,
            } => {
                start.is_finite()
                    && end.is_finite()
                    && start_alpha.is_finite()
                    && end_alpha.is_finite()
            }
            TransparencyMode::FountainRadial {
                center,
                radius,
                inner_alpha,
                outer_alpha,
            } => {
                center.is_finite()
                    && radius.is_finite()
                    && inner_alpha.is_finite()
                    && outer_alpha.is_finite()
            }
        },
        Effect::Perspective { quad } => quad.iter().all(|p| p.is_finite()),
        Effect::Symmetry { origin, .. } => origin.is_finite(),
    }
}

fn raster_filter_is_finite(f: &RasterFilter) -> bool {
    match f {
        RasterFilter::BrightnessContrast {
            brightness,
            contrast,
        } => brightness.is_finite() && contrast.is_finite(),
        RasterFilter::ToneCurve { points } => {
            points.iter().all(|(x, y)| x.is_finite() && y.is_finite())
        }
        RasterFilter::Levels {
            in_black,
            in_gamma,
            in_white,
            out_black,
            out_white,
        } => {
            in_black.is_finite()
                && in_gamma.is_finite()
                && in_white.is_finite()
                && out_black.is_finite()
                && out_white.is_finite()
        }
        RasterFilter::Hsl {
            hue_deg,
            saturation,
            lightness,
        } => hue_deg.is_finite() && saturation.is_finite() && lightness.is_finite(),
        RasterFilter::ColorBalance {
            cyan_red,
            magenta_green,
            yellow_blue,
        } => cyan_red.is_finite() && magenta_green.is_finite() && yellow_blue.is_finite(),
        RasterFilter::ReplaceColor {
            target,
            replacement,
            tolerance,
        } => color_is_finite(target) && color_is_finite(replacement) && tolerance.is_finite(),
        RasterFilter::Gamma { gamma } => gamma.is_finite(),
        RasterFilter::Desaturate
        | RasterFilter::Invert
        | RasterFilter::Posterize { .. }
        | RasterFilter::Threshold { .. } => true,
        RasterFilter::GaussianBlur { radius_px } => radius_px.is_finite(),
        RasterFilter::MotionBlur {
            distance_px,
            angle_deg,
        } => distance_px.is_finite() && angle_deg.is_finite(),
        RasterFilter::UnsharpMask {
            amount, radius_px, ..
        } => amount.is_finite() && radius_px.is_finite(),
        RasterFilter::Sharpen { amount } | RasterFilter::Noise { amount, .. } => amount.is_finite(),
        RasterFilter::Vignette {
            strength,
            radius,
            feather,
        } => strength.is_finite() && radius.is_finite() && feather.is_finite(),
        RasterFilter::Emboss { angle_deg, depth } => angle_deg.is_finite() && depth.is_finite(),
        RasterFilter::EdgeDetect { sensitivity } => sensitivity.is_finite(),
    }
}

fn text_payload_is_finite(payload: &NodePayload) -> bool {
    match payload {
        NodePayload::ArtisticText(t) => t.runs.iter().all(|r| text_style_is_finite(&r.style)),
        NodePayload::ParagraphText(p) => {
            p.frame_size.is_finite()
                && p.column_gutter_mm.is_finite()
                && p.drop_cap.is_none_or(|dc| dc.gap_mm.is_finite())
                && p.runs.iter().all(|r| text_style_is_finite(&r.style))
        }
        NodePayload::TextOnPath(top) => {
            top.offset_mm.is_finite()
                && top.distance_mm.is_finite()
                && top.text.runs.iter().all(|r| text_style_is_finite(&r.style))
        }
        _ => false,
    }
}

fn table_is_finite(tbl: &VectorTable) -> bool {
    tbl.col_widths_mm.iter().all(|v| v.is_finite())
        && tbl.row_heights_mm.iter().all(|v| v.is_finite())
        && tbl.cells.iter().all(|c| {
            c.padding_mm.is_finite()
                && text_style_is_finite(&c.text_style)
                && fill_is_finite(&c.fill)
        })
        && outline_is_finite(&tbl.border_style)
}

fn raster_is_finite(r: &RasterLayerData) -> bool {
    r.width_mm.is_finite()
        && r.height_mm.is_finite()
        && r.crop_rect.is_none_or(Rect2D::is_finite)
        && r.filters.iter().all(raster_filter_is_finite)
}

fn dimension_is_finite(d: &DimensionObject) -> bool {
    d.p1.is_finite()
        && d.p2.is_finite()
        && d.p3.is_none_or(Point2D::is_finite)
        && d.offset_mm.is_finite()
        && text_style_is_finite(&d.text_style)
}

/// Validates that a [`SceneObject`]'s transform, styling, effects, lens, and payload geometry
/// contain only finite numbers.
fn validate_scene_object_geometry_finite(object: &SceneObject) -> CoreResult<()> {
    if !object.transform.is_finite() {
        return Err(CoreError::InvalidGeometry(
            "detached object transform contains non-finite entries".into(),
        ));
    }
    if !object.opacity.is_finite()
        || !fill_is_finite(&object.fill)
        || !object.lens.as_ref().is_none_or(lens_is_finite)
        || !object.effects.iter().all(effect_is_finite)
    {
        return Err(CoreError::InvalidGeometry(
            "detached object styling, lens, or effects contain non-finite values".into(),
        ));
    }
    validate_outline_style(&object.outline)?;
    let anchor_finite = |a: AnchorPoint| match a {
        AnchorPoint::Custom(p) => p.is_finite(),
        _ => true,
    };
    let payload_finite = match &object.payload {
        NodePayload::Path(p) => p.is_finite(),
        NodePayload::Shape(s) => s.is_finite() && s.to_bezier_path().is_finite(),
        NodePayload::ArtisticText(_)
        | NodePayload::ParagraphText(_)
        | NodePayload::TextOnPath(_) => text_payload_is_finite(&object.payload),
        NodePayload::Raster(r) => raster_is_finite(r),
        NodePayload::Table(tbl) => table_is_finite(tbl),
        NodePayload::Dimension(d) => dimension_is_finite(d),
        NodePayload::Connector(c) => {
            c.from_fallback.is_finite()
                && c.to_fallback.is_finite()
                && c.corner_radius_mm.is_finite()
                && anchor_finite(c.from_anchor)
                && anchor_finite(c.to_anchor)
        }
        NodePayload::PowerClip {
            container,
            container_parametric,
            ..
        } => {
            container.is_finite()
                && container_parametric
                    .as_ref()
                    .is_none_or(|s| s.is_finite() && s.to_bezier_path().is_finite())
        }
        NodePayload::Group(_) | NodePayload::SymbolInstance(_) => true,
    };
    if !payload_finite {
        return Err(CoreError::InvalidGeometry(
            "detached object payload contains non-finite geometry".into(),
        ));
    }
    Ok(())
}

fn ensure_target_layer_unlocked(
    doc: &Document,
    layer_id: Option<LayerId>,
    err_obj_id: ObjectId,
) -> CoreResult<()> {
    match layer_id {
        Some(lid) => {
            let layer = doc.find_layer(lid).ok_or(CoreError::LayerNotFound(lid))?;
            if layer.locked {
                return Err(CoreError::ObjectLocked(err_obj_id));
            }
            Ok(())
        }
        None => {
            if let Some(page) = doc.active_page() {
                let resolved = page
                    .layers
                    .iter()
                    .rev()
                    .find(|l| l.visible && !l.locked)
                    .or_else(|| page.layers.iter().rev().find(|l| !l.locked))
                    .or_else(|| page.layers.last());
                if let Some(layer) = resolved {
                    if layer.locked {
                        return Err(CoreError::ObjectLocked(err_obj_id));
                    }
                }
            }
            Ok(())
        }
    }
}

/// Applies a single [`CommandOp`] deterministically to `doc`.
pub fn apply_command(doc: &mut Document, op: &CommandOp) -> CoreResult<()> {
    match op {
        CommandOp::CreateObject { object, layer_id } => {
            validate_scene_object_geometry_finite(object)?;
            ensure_target_layer_unlocked(doc, *layer_id, object.id)?;
            doc.add_object((**object).clone(), *layer_id)?;
        }
        CommandOp::DeleteObjects { ids } => {
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
                if let Some(locked_id) = doc.first_locked_in_subtree(id) {
                    return Err(CoreError::ObjectLocked(locked_id));
                }
            }
            for &id in ids {
                if doc.has_object(id) {
                    doc.remove_object(id)?;
                }
            }
        }
        CommandOp::TransformObjects { ids, affine } => {
            if !affine.is_finite() {
                return Err(CoreError::InvalidGeometry(
                    "transform contains non-finite entries".into(),
                ));
            }
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                doc.transform_object(id, *affine)?;
            }
        }
        CommandOp::SetFill { ids, fill } => {
            if !fill_is_finite(fill) {
                return Err(CoreError::InvalidGeometry(
                    "fill contains non-finite values".into(),
                ));
            }
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.fill = fill.clone();
                    sync_text_runs_fill(&mut obj.payload, fill);
                }
            }
            if !ids.is_empty() {
                if let Fill::Solid(ref col) = fill {
                    doc.document_palette_mut_raw()
                        .add_unique(col.to_hex(), col.clone());
                }
            }
            doc.touch();
        }
        CommandOp::SetOutline { ids, outline } => {
            validate_outline_style(outline)?;
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.outline = outline.clone();
                    if let NodePayload::Table(ref mut tbl) = obj.payload {
                        tbl.border_style = outline.clone();
                    }
                }
            }
            doc.touch();
        }
        CommandOp::SetLens { ids, lens } => {
            if let Some(ref l) = lens {
                if !lens_is_finite(l) {
                    return Err(CoreError::InvalidGeometry(
                        "lens contains non-finite values".into(),
                    ));
                }
            }
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.lens = lens.clone();
                }
            }
            doc.touch();
        }
        CommandOp::AddEffect { id, effect } => {
            if !effect_is_finite(effect) {
                return Err(CoreError::InvalidGeometry(
                    "effect contains non-finite values".into(),
                ));
            }
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            if let Some(obj) = doc.object_mut(*id) {
                obj.effects.push(effect.clone());
            }
            doc.touch();
        }
        CommandOp::RemoveEffect { id, effect_index } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            if *effect_index >= obj.effects.len() {
                return Err(CoreError::InvalidParameter(
                    "effect index out of range".into(),
                ));
            }
            obj.effects.remove(*effect_index);
            doc.touch();
        }
        CommandOp::EditPath { id, path } => {
            if !path.is_finite() {
                return Err(CoreError::InvalidGeometry(
                    "edited path contains non-finite coordinates".into(),
                ));
            }
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            if !matches!(obj.payload, NodePayload::Path(_)) {
                return Err(CoreError::InvalidParameter(format!(
                    "EditPath requires a Path object; cannot overwrite non-Path payload on {id} (use ConvertToCurves first)"
                )));
            }
            obj.payload = NodePayload::Path(path.clone());
            doc.touch();
        }
        CommandOp::BooleanOp { op, ids } => {
            let mut seen = HashSet::with_capacity(ids.len());
            let unique_ids: Vec<ObjectId> =
                ids.iter().copied().filter(|id| seen.insert(*id)).collect();
            if unique_ids.is_empty() {
                return Err(CoreError::BooleanOpFailed("no objects selected".into()));
            }
            let effective_locked = doc.effective_locked_object_ids();
            let mut world_paths = Vec::with_capacity(unique_ids.len());
            let mut sources = Vec::with_capacity(unique_ids.len());
            for &id in &unique_ids {
                let obj = doc
                    .object(id)
                    .cloned()
                    .ok_or(CoreError::ObjectNotFound(id))?;
                let layer_id = doc.layer_of_object(id);
                let parent_id = doc.parent_container_of(id);
                if *op != BooleanOp::CreateBoundary {
                    if effective_locked.contains(&id) {
                        return Err(CoreError::ObjectLocked(id));
                    }
                } else {
                    let layer_locked = layer_id
                        .and_then(|lid| doc.find_layer(lid))
                        .is_some_and(|l| l.locked);
                    let parent_locked =
                        parent_id.is_some_and(|pid| effective_locked.contains(&pid));
                    if layer_locked || parent_locked {
                        return Err(CoreError::ObjectLocked(id));
                    }
                }
                if *op != BooleanOp::CreateBoundary
                    && !matches!(obj.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                {
                    return Err(CoreError::BooleanOpFailed(format!(
                        "object {id} is not a vector path/shape"
                    )));
                }
                let wp = doc.world_path_of(id).ok_or_else(|| {
                    CoreError::BooleanOpFailed(format!("object {id} is not a vector path/shape"))
                })?;
                sources.push((obj, layer_id, parent_id));
                world_paths.push(wp);
            }

            let build_result_obj = |doc: &mut Document,
                                    src: &SceneObject,
                                    parent_id: Option<ObjectId>,
                                    name: String,
                                    world_path: BezierPath,
                                    keep_locked: bool| {
                let parent_inv = parent_id
                    .and_then(|pid| doc.world_transform_of(pid))
                    .and_then(|xf| xf.inverse());
                let local_path = if let Some(inv_p) = parent_inv {
                    world_path.transformed(&inv_p)
                } else {
                    world_path
                };
                let src_world = doc.world_transform_of(src.id).unwrap_or(src.transform);
                let bake_xf = match parent_inv {
                    Some(inv_p) => inv_p.compose(&src_world),
                    None => src_world,
                };
                let new_id = doc.alloc_object_id();
                let mut new_obj = SceneObject::new(new_id, name, NodePayload::Path(local_path));
                new_obj.fill = src.fill.clone();
                new_obj.outline = src.outline.clone();
                new_obj.blend_mode = src.blend_mode;
                new_obj.opacity = src.opacity;
                new_obj.effects = src.effects.clone();
                new_obj.lens = src.lens.clone();
                new_obj.style_id = src.style_id;
                new_obj.unknown_chunks = src.unknown_chunks.clone();
                new_obj.visible = src.visible;
                new_obj.printable = src.printable;
                new_obj.locked = keep_locked && src.locked;
                if bake_xf.is_finite()
                    && bake_xf
                        .m
                        .iter()
                        .zip(Affine2D::IDENTITY.m.iter())
                        .any(|(a, b)| (a - b).abs() > 1e-12)
                {
                    let dec = bake_xf.decompose();
                    let avg_scale = 0.5 * (dec.scale.x.abs() + dec.scale.y.abs());
                    let transform_angle_deg = |angle_deg: &mut f64| {
                        let rad = angle_deg.to_radians();
                        let dir = bake_xf.transform_vec(Vec2D::new(rad.cos(), rad.sin()));
                        if dir.length_sq() > 1e-18 && dir.is_finite() {
                            *angle_deg = dir.angle_rad().to_degrees();
                        }
                    };
                    match &mut new_obj.fill {
                        Fill::LinearGradient { start, end, .. } => {
                            *start = bake_xf.transform_point(*start);
                            *end = bake_xf.transform_point(*end);
                        }
                        Fill::RadialGradient {
                            center,
                            radius,
                            focal,
                            ..
                        } => {
                            *center = bake_xf.transform_point(*center);
                            *focal = bake_xf.transform_point(*focal);
                            if avg_scale.is_finite() && avg_scale > 1e-9 {
                                *radius *= avg_scale;
                            }
                        }
                        Fill::ConicalGradient {
                            center, angle_deg, ..
                        } => {
                            *center = bake_xf.transform_point(*center);
                            transform_angle_deg(angle_deg);
                        }
                        Fill::SquareGradient {
                            center,
                            size,
                            angle_deg,
                            ..
                        } => {
                            *center = bake_xf.transform_point(*center);
                            if avg_scale.is_finite() && avg_scale > 1e-9 {
                                *size *= avg_scale;
                            }
                            transform_angle_deg(angle_deg);
                        }
                        Fill::MeshFill { nodes, .. } => {
                            for node in nodes {
                                node.pos = bake_xf.transform_point(node.pos);
                            }
                        }
                        _ => {}
                    }
                    for eff in &mut new_obj.effects {
                        match eff {
                            Effect::Perspective { quad } => {
                                for pt in quad {
                                    *pt = bake_xf.transform_point(*pt);
                                }
                            }
                            Effect::Envelope { cage, .. } => {
                                for pt in cage {
                                    *pt = bake_xf.transform_point(*pt);
                                }
                            }
                            Effect::Distort { center, .. } => {
                                *center = bake_xf.transform_point(*center);
                            }
                            Effect::Transparency { mode, .. } => match mode {
                                TransparencyMode::FountainLinear { start, end, .. } => {
                                    *start = bake_xf.transform_point(*start);
                                    *end = bake_xf.transform_point(*end);
                                }
                                TransparencyMode::FountainRadial { center, radius, .. } => {
                                    *center = bake_xf.transform_point(*center);
                                    if avg_scale.is_finite() && avg_scale > 1e-9 {
                                        *radius *= avg_scale;
                                    }
                                }
                                TransparencyMode::Uniform(_) => {}
                            },
                            _ => {}
                        }
                    }
                }
                doc.objects_mut().insert(new_id, new_obj);
                new_id
            };

            if *op == BooleanOp::Simplify {
                // Trim each source by all upper sources while preserving each source's own
                // styling, metadata, and exact z-order slot.
                let mut trimmed_items: Vec<(
                    SceneObject,
                    Option<LayerId>,
                    Option<ObjectId>,
                    BezierPath,
                )> = Vec::with_capacity(sources.len());
                for i in 0..world_paths.len() {
                    let mut current = world_paths[i].clone();
                    for upper in &world_paths[(i + 1)..] {
                        let trimmed = apply_boolean(BooleanOp::Trim, &current, upper)?;
                        current = trimmed.into_iter().next().unwrap_or_default();
                        if current.is_empty() {
                            break;
                        }
                    }
                    trimmed_items.push((sources[i].0.clone(), sources[i].1, sources[i].2, current));
                }
                let mut pending = Vec::with_capacity(trimmed_items.len());
                let mut doomed_ids: Vec<ObjectId> = Vec::with_capacity(trimmed_items.len());
                for (src, lid, pid, path) in trimmed_items {
                    let src_id = src.id;
                    doomed_ids.push(src_id);
                    let new_id = (!path.is_empty())
                        .then(|| build_result_obj(doc, &src, pid, src.name.clone(), path, true));
                    pending.push((src_id, lid, pid, new_id));
                }
                doc.cleanup_dangling_refs_for_ids(&doomed_ids);
                for (src_id, lid, pid, new_id_opt) in pending {
                    if let Some(new_id) = new_id_opt {
                        if lid.is_some() || pid.is_some() {
                            doc.replace_in_parent(src_id, &[new_id]);
                        } else if let Some(obj) = doc.objects_mut().remove(&new_id) {
                            let _ = doc.add_object(obj, None)?;
                        }
                    }
                    if doc.has_object(src_id) {
                        doc.remove_object(src_id)?;
                    }
                }
                doc.touch();
            } else if *op == BooleanOp::BreakApart {
                let mut source_parts = Vec::with_capacity(sources.len());
                for ((src, lid, pid), wp) in sources.iter().zip(world_paths.iter()) {
                    let parts =
                        apply_boolean_many(BooleanOp::BreakApart, std::slice::from_ref(wp))?;
                    source_parts.push((src, *lid, *pid, parts));
                }
                let mut pending = Vec::with_capacity(source_parts.len());
                let mut doomed_ids: Vec<ObjectId> = Vec::with_capacity(source_parts.len());
                for (src, lid, pid, parts) in source_parts {
                    let mut new_ids = Vec::with_capacity(parts.len());
                    let multi = parts.len() > 1;
                    for (idx, part) in parts.into_iter().enumerate() {
                        let name = if multi {
                            format!("{} ({})", src.name, idx + 1)
                        } else {
                            src.name.clone()
                        };
                        let nid = build_result_obj(doc, src, pid, name, part, true);
                        new_ids.push(nid);
                    }
                    doomed_ids.push(src.id);
                    pending.push((src.id, lid, pid, new_ids));
                }
                doc.cleanup_dangling_refs_for_ids(&doomed_ids);
                for (src_id, lid, pid, new_ids) in pending {
                    if !new_ids.is_empty() {
                        if lid.is_some() || pid.is_some() {
                            doc.replace_in_parent(src_id, &new_ids);
                        } else {
                            for nid in new_ids {
                                if let Some(obj) = doc.objects_mut().remove(&nid) {
                                    let _ = doc.add_object(obj, None)?;
                                }
                            }
                        }
                    }
                    if doc.has_object(src_id) {
                        doc.remove_object(src_id)?;
                    }
                }
                doc.touch();
            } else {
                let (ref style_src, layer_id, parent_id) = if *op == BooleanOp::FrontMinusBack {
                    sources.last().expect("sources non-empty")
                } else {
                    &sources[0]
                };
                let anchor_id = style_src.id;
                let results = apply_boolean_many(*op, &world_paths)?;
                let res_len = results.len();
                let mut new_ids = Vec::with_capacity(res_len);
                for (idx, path) in results.into_iter().enumerate() {
                    let name = if *op == BooleanOp::CreateBoundary {
                        if res_len == 1 {
                            format!("{} (Boundary)", style_src.name)
                        } else {
                            format!("{} (Boundary {})", style_src.name, idx + 1)
                        }
                    } else if res_len == 1 {
                        style_src.name.clone()
                    } else {
                        format!("{} ({op:?} {})", style_src.name, idx + 1)
                    };
                    let nid = build_result_obj(
                        doc,
                        style_src,
                        *parent_id,
                        name,
                        path,
                        *op != BooleanOp::CreateBoundary,
                    );
                    new_ids.push(nid);
                }

                if *op == BooleanOp::CreateBoundary {
                    if !new_ids.is_empty() {
                        if layer_id.is_some() || parent_id.is_some() {
                            doc.insert_siblings_after(anchor_id, &new_ids);
                        } else {
                            for nid in new_ids {
                                if let Some(obj) = doc.objects_mut().remove(&nid) {
                                    let _ = doc.add_object(obj, None)?;
                                }
                            }
                        }
                    }
                } else {
                    doc.cleanup_dangling_refs_for_ids(&unique_ids);
                    if !new_ids.is_empty() {
                        if layer_id.is_some() || parent_id.is_some() {
                            doc.replace_in_parent(anchor_id, &new_ids);
                        } else {
                            for nid in new_ids {
                                if let Some(obj) = doc.objects_mut().remove(&nid) {
                                    let _ = doc.add_object(obj, None)?;
                                }
                            }
                        }
                    }
                    for &id in &unique_ids {
                        if doc.has_object(id) {
                            doc.remove_object(id)?;
                        }
                    }
                }
                doc.touch();
            }
        }
        CommandOp::Group { ids, name } => {
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            doc.group_objects(ids, name.clone())?;
        }
        CommandOp::Ungroup { group_id } => {
            if !doc.has_object(*group_id) {
                return Err(CoreError::ObjectNotFound(*group_id));
            }
            if doc.is_effectively_locked(*group_id) {
                return Err(CoreError::ObjectLocked(*group_id));
            }
            if let Some(locked_child) = doc.first_locked_in_subtree(*group_id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
            doc.ungroup_object(*group_id)?;
        }
        CommandOp::SetZOrder { id, op } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            doc.set_z_order(*id, *op)?;
        }
        CommandOp::AlignObjects { ids, op } => {
            doc.align_objects(ids, *op)?;
        }
        CommandOp::DistributeObjects { ids, op } => {
            doc.distribute_objects(ids, *op)?;
        }
        CommandOp::AddPage {
            name,
            width_mm,
            height_mm,
            bleed_mm,
        } => {
            if !width_mm.is_finite() || *width_mm <= 0.0 {
                return Err(CoreError::InvalidParameter(
                    "page width_mm must be finite and > 0".into(),
                ));
            }
            if !height_mm.is_finite() || *height_mm <= 0.0 {
                return Err(CoreError::InvalidParameter(
                    "page height_mm must be finite and > 0".into(),
                ));
            }
            if !bleed_mm.is_finite() || *bleed_mm < 0.0 {
                return Err(CoreError::InvalidParameter(
                    "page bleed_mm must be finite and >= 0".into(),
                ));
            }
            doc.add_page(name.clone(), *width_mm, *height_mm, *bleed_mm);
        }
        CommandOp::DeletePage { page_id } => {
            doc.delete_page(*page_id)?;
        }
        CommandOp::RenamePage { page_id, new_name } => {
            doc.rename_page(*page_id, new_name.clone())?;
        }
        CommandOp::AddLayer {
            page_id,
            name,
            is_cut_layer,
            master_scope,
        } => {
            doc.add_layer(*page_id, name.clone(), *is_cut_layer, *master_scope)?;
        }
        CommandOp::SetLayerFlags {
            layer_id,
            visible,
            locked,
            printable,
            is_cut_layer,
        } => {
            doc.set_layer_flags(*layer_id, *visible, *locked, *printable, *is_cut_layer)?;
        }
        CommandOp::AddGuide {
            orientation,
            locked,
        } => {
            if !guide_orientation_is_finite(*orientation) {
                return Err(CoreError::InvalidGeometry(
                    "guide orientation contains non-finite values".into(),
                ));
            }
            doc.add_guide(*orientation, *locked);
        }
        CommandOp::MoveGuide { id, orientation } => {
            if !guide_orientation_is_finite(*orientation) {
                return Err(CoreError::InvalidGeometry(
                    "guide orientation contains non-finite values".into(),
                ));
            }
            if let Some(g) = doc.guides_mut().iter_mut().find(|g| g.id == *id) {
                if !g.locked {
                    g.orientation = *orientation;
                    doc.touch();
                }
            }
        }
        CommandOp::RemoveGuide { id } => {
            let _ = doc.remove_guide(*id);
        }
        CommandOp::ToggleGuideLock { id } => {
            if let Some(g) = doc.guides_mut().iter_mut().find(|g| g.id == *id) {
                g.locked = !g.locked;
                doc.touch();
            }
        }
        CommandOp::EditText { id, payload } => {
            if !matches!(
                payload,
                NodePayload::ArtisticText(_)
                    | NodePayload::ParagraphText(_)
                    | NodePayload::TextOnPath(_)
            ) {
                return Err(CoreError::InvalidParameter(
                    "EditText payload must be ArtisticText, ParagraphText, or TextOnPath".into(),
                ));
            }
            if !text_payload_is_finite(payload) {
                return Err(CoreError::InvalidGeometry(
                    "edited text payload contains non-finite values".into(),
                ));
            }
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            if matches!(
                obj.payload,
                NodePayload::Group(_) | NodePayload::PowerClip { .. }
            ) {
                return Err(CoreError::InvalidParameter(format!(
                    "EditText cannot overwrite container object {id}"
                )));
            }
            obj.payload = payload.clone();
            doc.touch();
        }
        CommandOp::EditTable { id, table } => {
            if !table_is_finite(table) {
                return Err(CoreError::InvalidGeometry(
                    "edited table contains non-finite values".into(),
                ));
            }
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            if matches!(
                obj.payload,
                NodePayload::Group(_) | NodePayload::PowerClip { .. }
            ) {
                return Err(CoreError::InvalidParameter(format!(
                    "EditTable cannot overwrite container object {id}"
                )));
            }
            obj.payload = NodePayload::Table(table.clone());
            obj.outline = table.border_style.clone();
            doc.touch();
        }
        CommandOp::CreatePowerClip {
            container_id,
            content_ids,
        } => {
            let effective_locked = doc.effective_locked_object_ids();
            if !doc.has_object(*container_id) {
                return Err(CoreError::ObjectNotFound(*container_id));
            }
            if effective_locked.contains(container_id) {
                return Err(CoreError::ObjectLocked(*container_id));
            }
            if let Some(locked_child) = doc.first_locked_in_subtree(*container_id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
            for &cid in content_ids {
                if !doc.has_object(cid) {
                    return Err(CoreError::ObjectNotFound(cid));
                }
                if effective_locked.contains(&cid) {
                    return Err(CoreError::ObjectLocked(cid));
                }
                if let Some(locked_child) = doc.first_locked_in_subtree(cid) {
                    return Err(CoreError::ObjectLocked(locked_child));
                }
            }
            doc.create_powerclip(*container_id, content_ids)?;
        }
        CommandOp::ExtractPowerClip { powerclip_id } => {
            if !doc.has_object(*powerclip_id) {
                return Err(CoreError::ObjectNotFound(*powerclip_id));
            }
            if doc.is_effectively_locked(*powerclip_id) {
                return Err(CoreError::ObjectLocked(*powerclip_id));
            }
            if let Some(locked_child) = doc.first_locked_in_subtree(*powerclip_id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
            doc.extract_powerclip(*powerclip_id)?;
        }
        CommandOp::SetPowerClipLock { id, lock_contents } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            doc.set_powerclip_lock_contents(*id, *lock_contents)?;
        }
        CommandOp::MoveToDesktopLayer { id } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            if let Some(locked_child) = doc.first_locked_in_subtree(*id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
            doc.move_object_to_desktop_layer(*id)?;
        }
        CommandOp::ConvertToCurves { id } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            doc.convert_to_curves(*id)?;
        }
        CommandOp::ConvertOutlineToObject { id } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            doc.convert_outline_to_object(*id)?;
        }
        CommandOp::UpdateRaster { id, data } => {
            if !raster_is_finite(data) {
                return Err(CoreError::InvalidGeometry(
                    "updated raster payload contains non-finite values".into(),
                ));
            }
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            if matches!(
                obj.payload,
                NodePayload::Group(_) | NodePayload::PowerClip { .. }
            ) {
                return Err(CoreError::InvalidParameter(format!(
                    "UpdateRaster cannot overwrite container object {id}"
                )));
            }
            obj.payload = NodePayload::Raster(data.clone());
            doc.touch();
        }
        CommandOp::DuplicateObjects { ids, offset } => {
            doc.duplicate_objects(ids, *offset)?;
        }
        CommandOp::SetObjectVisibility { ids, visible } => {
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.visible = *visible;
                }
            }
            doc.touch();
        }
        CommandOp::SetObjectLocked { ids, locked } => {
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
            }
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.locked = *locked;
                }
            }
            doc.touch();
        }
        CommandOp::SetObjectPrintable { ids, printable } => {
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.printable = *printable;
                }
            }
            doc.touch();
        }
        CommandOp::RenameObject { id, new_name } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            obj.name = new_name.clone();
            doc.touch();
        }
        CommandOp::SetOpacity { ids, opacity } => {
            if !opacity.is_finite() {
                return Err(CoreError::InvalidParameter(
                    "opacity must be a finite value".into(),
                ));
            }
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            let clamped = opacity.clamp(0.0, 1.0);
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.opacity = clamped;
                }
            }
            doc.touch();
        }
        CommandOp::SetBlendMode { ids, blend_mode } => {
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                if let Some(obj) = doc.object_mut(id) {
                    obj.blend_mode = *blend_mode;
                }
            }
            doc.touch();
        }
        CommandOp::SetDocumentProperties {
            title,
            color_mode,
            default_unit,
            dpi,
            multipage_view,
            active_page_index,
            metadata,
            custom_icc_profile,
            clear_custom_icc,
        } => {
            if let Some(d) = dpi {
                if !d.is_finite() || *d < 1.0 || *d > 2400.0 {
                    return Err(CoreError::InvalidParameter(
                        "dpi must be finite and within [1.0, 2400.0]".into(),
                    ));
                }
            }
            if let Some(idx) = active_page_index {
                let len = doc.pages().len();
                if *idx >= len {
                    return Err(CoreError::PageIndexOutOfRange { index: *idx, len });
                }
            }
            if let Some(bytes) = custom_icc_profile {
                doc.set_custom_icc_profile(bytes.clone())?;
            } else if *clear_custom_icc {
                doc.clear_custom_icc_profile();
            }
            if let Some(t) = title {
                doc.set_title(t.clone());
            }
            if let Some(cm) = color_mode {
                doc.set_color_mode(*cm);
            }
            if let Some(u) = default_unit {
                doc.set_default_unit(*u);
            }
            if let Some(d) = dpi {
                doc.set_dpi(*d);
            }
            if let Some(mv) = multipage_view {
                doc.set_multipage_view(*mv);
            }
            if let Some(idx) = active_page_index {
                doc.set_active_page_index(*idx)?;
            }
            if let Some(meta) = metadata {
                *doc.metadata_mut() = meta.clone();
            }
            doc.touch();
        }
        CommandOp::SetPageProperties {
            page_id,
            name,
            width_mm,
            height_mm,
            bleed_mm,
            facing_pages,
        } => {
            if let Some(w) = width_mm {
                if !w.is_finite() || *w <= 0.0 {
                    return Err(CoreError::InvalidParameter(
                        "page width_mm must be finite and > 0".into(),
                    ));
                }
            }
            if let Some(h) = height_mm {
                if !h.is_finite() || *h <= 0.0 {
                    return Err(CoreError::InvalidParameter(
                        "page height_mm must be finite and > 0".into(),
                    ));
                }
            }
            if let Some(b) = bleed_mm {
                if !b.is_finite() || *b < 0.0 {
                    return Err(CoreError::InvalidParameter(
                        "page bleed_mm must be finite and >= 0".into(),
                    ));
                }
            }
            let page = doc
                .pages_mut()
                .iter_mut()
                .find(|p| p.id == *page_id)
                .ok_or(CoreError::PageNotFound(*page_id))?;
            if let Some(n) = name {
                page.name = n.clone();
            }
            if let Some(w) = width_mm {
                page.width_mm = *w;
            }
            if let Some(h) = height_mm {
                page.height_mm = *h;
            }
            if let Some(b) = bleed_mm {
                page.bleed_mm = *b;
            }
            if let Some(fp) = facing_pages {
                page.facing_pages = *fp;
            }
            doc.touch();
        }
        CommandOp::ReorderPages { page_ids } => {
            if page_ids.len() != doc.pages().len() {
                return Err(CoreError::InvalidParameter(
                    "ReorderPages length must match existing page count".into(),
                ));
            }
            let active_id = doc.active_page().map(|p| p.id);
            let existing: std::collections::HashMap<PageId, Page> =
                doc.pages().iter().cloned().map(|p| (p.id, p)).collect();
            let mut seen = HashSet::with_capacity(page_ids.len());
            let mut reordered = Vec::with_capacity(page_ids.len());
            for pid in page_ids {
                if !seen.insert(*pid) {
                    return Err(CoreError::InvalidParameter(format!(
                        "duplicate page ID in ReorderPages: {pid}"
                    )));
                }
                let p = existing
                    .get(pid)
                    .cloned()
                    .ok_or(CoreError::PageNotFound(*pid))?;
                reordered.push(p);
            }
            *doc.pages_mut() = reordered;
            if let Some(aid) = active_id {
                if let Some(new_idx) = doc.pages().iter().position(|p| p.id == aid) {
                    doc.set_active_page_index_raw(new_idx);
                }
            }
            doc.touch();
        }
        CommandOp::RenameLayer { layer_id, new_name } => {
            let layer = doc
                .find_layer_mut(*layer_id)
                .ok_or(CoreError::LayerNotFound(*layer_id))?;
            layer.name = new_name.clone();
            doc.touch();
        }
        CommandOp::ReorderLayerObjects {
            layer_id,
            object_ids,
        } => {
            let layer = doc
                .find_layer_mut(*layer_id)
                .ok_or(CoreError::LayerNotFound(*layer_id))?;
            if layer.locked {
                return Err(CoreError::InvalidParameter(format!(
                    "layer {layer_id} is locked"
                )));
            }
            if object_ids.len() != layer.objects.len() {
                return Err(CoreError::InvalidParameter(
                    "ReorderLayerObjects length must match existing layer objects".into(),
                ));
            }
            let existing: HashSet<ObjectId> = layer.objects.iter().copied().collect();
            let mut seen = HashSet::with_capacity(object_ids.len());
            for oid in object_ids {
                if !seen.insert(*oid) || !existing.contains(oid) {
                    return Err(CoreError::InvalidParameter(format!(
                        "ReorderLayerObjects IDs must be a permutation of layer {layer_id}"
                    )));
                }
            }
            layer.objects = object_ids.clone();
            doc.touch();
        }
        CommandOp::SetGridConfig { config } => {
            if !config.spacing_x_mm.is_finite()
                || config.spacing_x_mm <= 0.0
                || !config.spacing_y_mm.is_finite()
                || config.spacing_y_mm <= 0.0
            {
                return Err(CoreError::InvalidParameter(
                    "grid spacing must be finite and > 0".into(),
                ));
            }
            let mut cfg = *config;
            cfg.subdivisions = cfg.subdivisions.max(1);
            doc.set_grid(cfg);
        }
        CommandOp::SetPerspectiveGrid { config } => {
            if !config.horizon_y_mm.is_finite()
                || !config.opacity.is_finite()
                || !(0.0..=1.0).contains(&config.opacity)
                || config.vanishing_points.iter().any(|p| !p.is_finite())
            {
                return Err(CoreError::InvalidParameter(
                    "perspective grid contains invalid or non-finite values".into(),
                ));
            }
            doc.set_perspective_grid(config.clone());
        }
        CommandOp::UpsertColorStyle { style } => {
            if !color_is_finite(&style.base_color)
                || !style.derived_colors.iter().all(color_is_finite)
            {
                return Err(CoreError::InvalidGeometry(
                    "color style contains non-finite values".into(),
                ));
            }
            doc.upsert_color_style(style.clone());
        }
        CommandOp::DeleteColorStyle { id } => {
            doc.delete_color_style(*id)?;
        }
        CommandOp::UpsertObjectStyle { style } => {
            if let Some(ref fill) = style.fill {
                if !fill_is_finite(fill) {
                    return Err(CoreError::InvalidGeometry(
                        "object style fill contains non-finite values".into(),
                    ));
                }
            }
            if let Some(ref outline) = style.outline {
                validate_outline_style(outline)?;
            }
            if let Some(op) = style.opacity {
                if !op.is_finite() || !(0.0..=1.0).contains(&op) {
                    return Err(CoreError::InvalidParameter(
                        "object style opacity must be finite and within [0.0, 1.0]".into(),
                    ));
                }
            }
            doc.upsert_object_style(style.clone());
        }
        CommandOp::DeleteObjectStyle { id } => {
            doc.delete_object_style(*id)?;
        }
        CommandOp::SetObjectStyleLink { ids, style_id } => {
            let style = if let Some(sid) = style_id {
                let st = doc.object_style(*sid).cloned().ok_or_else(|| {
                    CoreError::InvalidParameter(format!("object style not found: {sid}"))
                })?;
                if st.opacity.is_some_and(|op| !op.is_finite()) {
                    return Err(CoreError::InvalidParameter(
                        "object style opacity must be finite".into(),
                    ));
                }
                Some(st)
            } else {
                None
            };
            let effective_locked = doc.effective_locked_object_ids();
            for &id in ids {
                if !doc.has_object(id) {
                    return Err(CoreError::ObjectNotFound(id));
                }
                if effective_locked.contains(&id) {
                    return Err(CoreError::ObjectLocked(id));
                }
            }
            for &id in ids {
                let Some(obj) = doc.object_mut(id) else {
                    continue;
                };
                obj.style_id = *style_id;
                if let Some(ref st) = style {
                    if let Some(ref f) = st.fill {
                        obj.fill = f.clone();
                        sync_text_runs_fill(&mut obj.payload, f);
                    }
                    if let Some(ref o) = st.outline {
                        obj.outline = o.clone();
                        if let NodePayload::Table(ref mut tbl) = obj.payload {
                            tbl.border_style = o.clone();
                        }
                    }
                    if let Some(bm) = st.blend_mode {
                        obj.blend_mode = bm;
                    }
                    if let Some(op) = st.opacity {
                        if op.is_finite() {
                            obj.opacity = op.clamp(0.0, 1.0);
                        }
                    }
                }
            }
            doc.touch();
        }
        CommandOp::UpsertSymbol { symbol } => {
            for sub in &symbol.objects {
                validate_scene_object_geometry_finite(sub)?;
            }
            doc.upsert_symbol(symbol.clone());
        }
        CommandOp::DeleteSymbol { id } => {
            doc.delete_symbol(*id)?;
        }
        CommandOp::InstantiateSymbol {
            symbol_id,
            layer_id,
            transform,
            name,
        } => {
            if !transform.is_finite() {
                return Err(CoreError::InvalidGeometry(
                    "symbol instance transform contains non-finite entries".into(),
                ));
            }
            let sym = doc
                .symbol(*symbol_id)
                .ok_or(CoreError::SymbolNotFound(*symbol_id))?;
            let inst_name = name
                .clone()
                .unwrap_or_else(|| format!("{} Instance", sym.name));
            ensure_target_layer_unlocked(doc, *layer_id, ObjectId(0))?;
            let obj = SceneObject::new(
                ObjectId(0),
                inst_name,
                NodePayload::SymbolInstance(*symbol_id),
            )
            .with_transform(*transform);
            doc.add_object(obj, *layer_id)?;
        }
        CommandOp::UnlinkSymbolInstance { id } => {
            doc.unlink_symbol_instance(*id)?;
        }
        CommandOp::FitPowerClip { id, mode } => {
            doc.fit_powerclip(*id, *mode)?;
        }
        CommandOp::FindAndReplace { query, action } => {
            if query
                .fill_color
                .as_ref()
                .is_some_and(|m| !color_is_finite(&m.color) || !m.delta_e_tolerance.is_finite())
                || query
                    .outline_color
                    .as_ref()
                    .is_some_and(|m| !color_is_finite(&m.color) || !m.delta_e_tolerance.is_finite())
            {
                return Err(CoreError::InvalidGeometry(
                    "FindAndReplace query color filter contains non-finite values".into(),
                ));
            }
            if query.outline_width_min_mm.is_some_and(|w| !w.is_finite())
                || query.outline_width_max_mm.is_some_and(|w| !w.is_finite())
            {
                return Err(CoreError::InvalidGeometry(
                    "FindAndReplace query outline width bounds must be finite".into(),
                ));
            }
            match action {
                ReplaceAction::ReplaceFillColor { color }
                | ReplaceAction::ReplaceOutlineColor { color }
                    if !color_is_finite(color) =>
                {
                    return Err(CoreError::InvalidGeometry(
                        "FindAndReplace color contains non-finite channels".into(),
                    ));
                }
                ReplaceAction::ReplaceOutlineWidth {
                    width_mm,
                    is_hairline,
                } if !width_mm.is_finite() || (!*is_hairline && *width_mm < 0.0) => {
                    return Err(CoreError::InvalidGeometry(
                        "FindAndReplace outline width must be finite and >= 0".into(),
                    ));
                }
                _ => {}
            }
            let _ = find_and_replace(doc, query, action);
        }
        CommandOp::EditShape { id, shape } => {
            if !shape.is_finite() || !shape.to_bezier_path().is_finite() {
                return Err(CoreError::InvalidGeometry(
                    "edited parametric shape contains non-finite coordinates".into(),
                ));
            }
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            if !matches!(obj.payload, NodePayload::Shape(_)) {
                return Err(CoreError::InvalidParameter(format!(
                    "EditShape requires a Shape object; cannot overwrite non-Shape payload on {id}"
                )));
            }
            obj.payload = NodePayload::Shape(shape.clone());
            doc.touch();
        }
        CommandOp::DeleteLayer { layer_id } => {
            doc.delete_layer(*layer_id)?;
        }
        CommandOp::RegisterAsset { asset } => {
            let expected = (asset.width as usize)
                .checked_mul(asset.height as usize)
                .and_then(|px| px.checked_mul(4));
            if expected != Some(asset.rgba_bytes.len()) {
                return Err(CoreError::InvalidParameter(format!(
                    "raster asset RGBA byte length mismatch for {}x{}: got {}",
                    asset.width,
                    asset.height,
                    asset.rgba_bytes.len()
                )));
            }
            if !asset.source_dpi.is_finite() || asset.source_dpi <= 0.0 {
                return Err(CoreError::InvalidParameter(
                    "raster asset source_dpi must be positive and finite".into(),
                ));
            }
            doc.insert_asset_if_absent(asset.clone());
            doc.touch();
        }
        CommandOp::CreateDetachedObject { object } => {
            if object.id.0 != 0 && doc.has_object(object.id) {
                return Err(CoreError::InvalidParameter(format!(
                    "detached object ID {} already exists in document",
                    object.id
                )));
            }
            validate_scene_object_geometry_finite(object)?;
            doc.insert_loaded_object((**object).clone())?;
            doc.touch();
        }
        CommandOp::EditDimension { id, dimension } => {
            if !dimension_is_finite(dimension) {
                return Err(CoreError::InvalidGeometry(
                    "edited dimension contains non-finite values".into(),
                ));
            }
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            let obj = doc.object_mut(*id).ok_or(CoreError::ObjectNotFound(*id))?;
            if !matches!(obj.payload, NodePayload::Dimension(_)) {
                return Err(CoreError::InvalidParameter(format!(
                    "EditDimension requires a Dimension object; cannot overwrite non-Dimension payload on {id}"
                )));
            }
            obj.payload = NodePayload::Dimension(dimension.clone());
            doc.touch();
        }
        CommandOp::PruneAssets { ids } => {
            if !ids.is_empty() {
                let live_refs = doc.referenced_asset_ids();
                let mut removed_any = false;
                for id in ids {
                    if !live_refs.contains(id) && doc.assets_mut().remove(id).is_some() {
                        removed_any = true;
                    }
                }
                if removed_any {
                    doc.touch();
                }
            }
        }
        CommandOp::ReplaceObjectInParent { id, replacements } => {
            if !doc.has_object(*id) {
                return Err(CoreError::ObjectNotFound(*id));
            }
            if doc.is_effectively_locked(*id) {
                return Err(CoreError::ObjectLocked(*id));
            }
            if let Some(locked_child) = doc.first_locked_in_subtree(*id) {
                return Err(CoreError::ObjectLocked(locked_child));
            }
            let mut seen_ids = HashSet::new();
            for rep in replacements {
                validate_scene_object_geometry_finite(rep)?;
                if rep.id.0 != 0
                    && (rep.id == *id || doc.has_object(rep.id) || !seen_ids.insert(rep.id))
                {
                    return Err(CoreError::InvalidParameter(format!(
                        "replacement object ID {} collides with existing object",
                        rep.id
                    )));
                }
            }
            let mut new_ids = Vec::with_capacity(replacements.len());
            for rep in replacements {
                let new_id = doc.insert_loaded_object(rep.clone())?;
                new_ids.push(new_id);
            }
            doc.cleanup_dangling_refs_for_ids(&[*id]);
            doc.replace_in_parent(*id, &new_ids);
            doc.remove_object(*id)?;
        }
        CommandOp::Batch(ops) => {
            for sub in ops {
                apply_command(doc, sub)?;
            }
        }
    }
    Ok(())
}

/// Compact reversible scene delta stored on the undo/redo stack instead of duplicating
/// the entire object table twice per edit.
#[derive(Debug, Clone)]
struct ObjectDelta {
    id: ObjectId,
    before: Option<SceneObject>,
    after: Option<SceneObject>,
}

/// Sparse reversible delta for embedded raster image assets.
#[derive(Debug, Clone)]
struct AssetDelta {
    id: ImageAssetId,
    before: Option<RasterAsset>,
    after: Option<RasterAsset>,
}

/// Sparse reversible delta for reusable symbol definitions.
#[derive(Debug, Clone)]
struct SymbolDelta {
    id: SymbolId,
    before: Option<SymbolDef>,
    after: Option<SymbolDef>,
}

#[derive(Debug, Clone)]
struct DocumentDelta {
    title: Option<(String, String)>,
    color_mode: Option<(ColorMode, ColorMode)>,
    default_unit: Option<(Unit, Unit)>,
    dpi: Option<(f32, f32)>,
    pages: Option<(Vec<Page>, Vec<Page>)>,
    active_page_index: Option<(usize, usize)>,
    multipage_view: Option<(bool, bool)>,
    objects: Vec<ObjectDelta>,
    assets: Vec<AssetDelta>,
    palettes: Option<(Vec<ColorPalette>, Vec<ColorPalette>)>,
    document_palette: Option<(ColorPalette, ColorPalette)>,
    color_styles: Option<(Vec<ColorStyle>, Vec<ColorStyle>)>,
    object_styles: Option<(Vec<ObjectStyle>, Vec<ObjectStyle>)>,
    symbols: Vec<SymbolDelta>,
    guides: Option<(Vec<Guide>, Vec<Guide>)>,
    grid: Option<(GridConfig, GridConfig)>,
    perspective_grid: Option<(PerspectiveGridConfig, PerspectiveGridConfig)>,
    metadata: Option<(DocumentMetadata, DocumentMetadata)>,
    unknown_doc_chunks: Option<(Vec<UnknownCdrChunk>, Vec<UnknownCdrChunk>)>,
    unknown_data_files: UnknownDataFilesDelta,
    custom_icc_profile: CustomIccBytesDelta,
    custom_icc_name: Option<(Option<String>, Option<String>)>,
    next_id: Option<(u64, u64)>,
}

type UnknownDataFilesDelta = Option<(BTreeMap<String, Vec<u8>>, BTreeMap<String, Vec<u8>>)>;
type CustomIccBytesDelta = Option<(Option<Vec<u8>>, Option<Vec<u8>>)>;

/// Full rollback snapshot kept temporarily during an active command/transaction.
#[derive(Debug, Clone)]
struct DocumentStateSnapshot {
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
    unknown_data_files: BTreeMap<String, Vec<u8>>,
    custom_icc_profile: Option<Vec<u8>>,
    custom_icc_name: Option<String>,
    next_id: u64,
}

impl DocumentStateSnapshot {
    fn capture(doc: &Document) -> Self {
        Self {
            title: doc.title().to_owned(),
            color_mode: doc.color_mode(),
            default_unit: doc.default_unit(),
            dpi: doc.dpi(),
            pages: doc.pages().to_vec(),
            active_page_index: doc.active_page_index(),
            multipage_view: doc.multipage_view(),
            objects: doc.objects().clone(),
            assets: doc.assets().clone(),
            palettes: doc.palettes().to_vec(),
            document_palette: doc.document_palette().clone(),
            color_styles: doc.color_styles().to_vec(),
            object_styles: doc.object_styles().to_vec(),
            symbols: doc.symbols().clone(),
            guides: doc.guides().to_vec(),
            grid: doc.grid(),
            perspective_grid: doc.perspective_grid().clone(),
            metadata: doc.metadata().clone(),
            unknown_doc_chunks: doc.unknown_doc_chunks().to_vec(),
            unknown_data_files: doc.unknown_data_files().clone(),
            custom_icc_profile: doc.custom_icc_profile().map(<[u8]>::to_vec),
            custom_icc_name: doc.custom_icc_name().map(str::to_owned),
            next_id: doc.next_id(),
        }
    }

    fn restore_into(&self, doc: &mut Document) {
        doc.set_title(self.title.clone());
        doc.set_color_mode(self.color_mode);
        doc.set_default_unit(self.default_unit);
        doc.set_dpi(self.dpi);
        *doc.pages_mut() = self.pages.clone();
        doc.set_active_page_index_raw(self.active_page_index);
        doc.set_multipage_view(self.multipage_view);
        *doc.objects_mut() = self.objects.clone();
        *doc.assets_mut() = self.assets.clone();
        *doc.palettes_mut_raw() = self.palettes.clone();
        *doc.document_palette_mut_raw() = self.document_palette.clone();
        *doc.color_styles_mut() = self.color_styles.clone();
        *doc.object_styles_mut() = self.object_styles.clone();
        *doc.symbols_mut() = self.symbols.clone();
        *doc.guides_mut() = self.guides.clone();
        doc.set_grid(self.grid);
        doc.set_perspective_grid(self.perspective_grid.clone());
        *doc.unknown_doc_chunks_mut_raw() = self.unknown_doc_chunks.clone();
        *doc.unknown_data_files_mut_raw() = self.unknown_data_files.clone();
        doc.set_custom_icc_raw(
            self.custom_icc_profile.clone(),
            self.custom_icc_name.clone(),
        );
        doc.set_next_id_raw(self.next_id);
        // Restore exact metadata (including `modified_at`) last so setters above do not perturb it.
        *doc.metadata_mut() = self.metadata.clone();
    }

    fn into_delta(self, after: &Document) -> DocumentDelta {
        let mut object_deltas = Vec::new();
        for (id, before_obj) in &self.objects {
            match after.objects().get(id) {
                Some(after_obj) if after_obj == before_obj => {}
                Some(after_obj) => object_deltas.push(ObjectDelta {
                    id: *id,
                    before: Some(before_obj.clone()),
                    after: Some(after_obj.clone()),
                }),
                None => object_deltas.push(ObjectDelta {
                    id: *id,
                    before: Some(before_obj.clone()),
                    after: None,
                }),
            }
        }
        for (id, after_obj) in after.objects() {
            if !self.objects.contains_key(id) {
                object_deltas.push(ObjectDelta {
                    id: *id,
                    before: None,
                    after: Some(after_obj.clone()),
                });
            }
        }

        let mut asset_deltas = Vec::new();
        for (id, before_asset) in &self.assets {
            match after.assets().get(id) {
                Some(after_asset) if after_asset == before_asset => {}
                Some(after_asset) => asset_deltas.push(AssetDelta {
                    id: *id,
                    before: Some(before_asset.clone()),
                    after: Some(after_asset.clone()),
                }),
                None => asset_deltas.push(AssetDelta {
                    id: *id,
                    before: Some(before_asset.clone()),
                    after: None,
                }),
            }
        }
        for (id, after_asset) in after.assets() {
            if !self.assets.contains_key(id) {
                asset_deltas.push(AssetDelta {
                    id: *id,
                    before: None,
                    after: Some(after_asset.clone()),
                });
            }
        }

        let mut symbol_deltas = Vec::new();
        for (id, before_sym) in &self.symbols {
            match after.symbols().get(id) {
                Some(after_sym) if after_sym == before_sym => {}
                Some(after_sym) => symbol_deltas.push(SymbolDelta {
                    id: *id,
                    before: Some(before_sym.clone()),
                    after: Some(after_sym.clone()),
                }),
                None => symbol_deltas.push(SymbolDelta {
                    id: *id,
                    before: Some(before_sym.clone()),
                    after: None,
                }),
            }
        }
        for (id, after_sym) in after.symbols() {
            if !self.symbols.contains_key(id) {
                symbol_deltas.push(SymbolDelta {
                    id: *id,
                    before: None,
                    after: Some(after_sym.clone()),
                });
            }
        }

        let after_icc_profile = after.custom_icc_profile().map(<[u8]>::to_vec);
        let after_icc_name = after.custom_icc_name().map(str::to_owned);

        DocumentDelta {
            title: (self.title != after.title()).then(|| (self.title, after.title().to_owned())),
            color_mode: (self.color_mode != after.color_mode())
                .then_some((self.color_mode, after.color_mode())),
            default_unit: (self.default_unit != after.default_unit())
                .then_some((self.default_unit, after.default_unit())),
            dpi: (self.dpi.to_bits() != after.dpi().to_bits()).then_some((self.dpi, after.dpi())),
            pages: (self.pages != after.pages()).then(|| (self.pages, after.pages().to_vec())),
            active_page_index: (self.active_page_index != after.active_page_index())
                .then_some((self.active_page_index, after.active_page_index())),
            multipage_view: (self.multipage_view != after.multipage_view())
                .then_some((self.multipage_view, after.multipage_view())),
            objects: object_deltas,
            assets: asset_deltas,
            palettes: (self.palettes != after.palettes())
                .then(|| (self.palettes, after.palettes().to_vec())),
            document_palette: (self.document_palette != *after.document_palette())
                .then(|| (self.document_palette, after.document_palette().clone())),
            color_styles: (self.color_styles != after.color_styles())
                .then(|| (self.color_styles, after.color_styles().to_vec())),
            object_styles: (self.object_styles != after.object_styles())
                .then(|| (self.object_styles, after.object_styles().to_vec())),
            symbols: symbol_deltas,
            guides: (self.guides != after.guides()).then(|| (self.guides, after.guides().to_vec())),
            grid: (self.grid != after.grid()).then_some((self.grid, after.grid())),
            perspective_grid: (self.perspective_grid != *after.perspective_grid())
                .then(|| (self.perspective_grid, after.perspective_grid().clone())),
            metadata: (self.metadata != *after.metadata())
                .then(|| (self.metadata, after.metadata().clone())),
            unknown_doc_chunks: (self.unknown_doc_chunks != after.unknown_doc_chunks())
                .then(|| (self.unknown_doc_chunks, after.unknown_doc_chunks().to_vec())),
            unknown_data_files: (self.unknown_data_files != *after.unknown_data_files())
                .then(|| (self.unknown_data_files, after.unknown_data_files().clone())),
            custom_icc_profile: (self.custom_icc_profile != after_icc_profile)
                .then_some((self.custom_icc_profile, after_icc_profile)),
            custom_icc_name: (self.custom_icc_name != after_icc_name)
                .then_some((self.custom_icc_name, after_icc_name)),
            next_id: (self.next_id != after.next_id()).then_some((self.next_id, after.next_id())),
        }
    }
}

impl DocumentDelta {
    fn apply_before(&self, doc: &mut Document) {
        if let Some((b, _)) = &self.title {
            doc.set_title(b.clone());
        }
        if let Some((b, _)) = self.color_mode {
            doc.set_color_mode(b);
        }
        if let Some((b, _)) = self.default_unit {
            doc.set_default_unit(b);
        }
        if let Some((b, _)) = self.dpi {
            doc.set_dpi(b);
        }
        if let Some((b, _)) = &self.pages {
            *doc.pages_mut() = b.clone();
        }
        if let Some((b, _)) = self.active_page_index {
            doc.set_active_page_index_raw(b);
        }
        if let Some((b, _)) = self.multipage_view {
            doc.set_multipage_view(b);
        }
        for d in &self.objects {
            match &d.before {
                Some(obj) => {
                    doc.objects_mut().insert(d.id, obj.clone());
                }
                None => {
                    doc.objects_mut().remove(&d.id);
                }
            }
        }
        for d in &self.assets {
            match &d.before {
                Some(asset) => {
                    doc.assets_mut().insert(d.id, asset.clone());
                }
                None => {
                    doc.assets_mut().remove(&d.id);
                }
            }
        }
        if let Some((b, _)) = &self.palettes {
            *doc.palettes_mut_raw() = b.clone();
        }
        if let Some((b, _)) = &self.document_palette {
            *doc.document_palette_mut_raw() = b.clone();
        }
        if let Some((b, _)) = &self.color_styles {
            *doc.color_styles_mut() = b.clone();
        }
        if let Some((b, _)) = &self.object_styles {
            *doc.object_styles_mut() = b.clone();
        }
        for d in &self.symbols {
            match &d.before {
                Some(sym) => {
                    doc.symbols_mut().insert(d.id, sym.clone());
                }
                None => {
                    doc.symbols_mut().remove(&d.id);
                }
            }
        }
        if let Some((b, _)) = &self.guides {
            *doc.guides_mut() = b.clone();
        }
        if let Some((b, _)) = self.grid {
            doc.set_grid(b);
        }
        if let Some((b, _)) = &self.perspective_grid {
            doc.set_perspective_grid(b.clone());
        }
        if let Some((b, _)) = &self.unknown_doc_chunks {
            *doc.unknown_doc_chunks_mut_raw() = b.clone();
        }
        if let Some((b, _)) = &self.unknown_data_files {
            *doc.unknown_data_files_mut_raw() = b.clone();
        }
        if self.custom_icc_profile.is_some() || self.custom_icc_name.is_some() {
            let prof = self.custom_icc_profile.as_ref().map_or_else(
                || doc.custom_icc_profile().map(<[u8]>::to_vec),
                |(b, _)| b.clone(),
            );
            let name = self.custom_icc_name.as_ref().map_or_else(
                || doc.custom_icc_name().map(str::to_owned),
                |(b, _)| b.clone(),
            );
            doc.set_custom_icc_raw(prof, name);
        }
        if let Some((b, _)) = self.next_id {
            doc.set_next_id_raw(b);
        }
        if let Some((b, _)) = &self.metadata {
            *doc.metadata_mut() = b.clone();
        }
    }

    fn apply_after(&self, doc: &mut Document) {
        if let Some((_, a)) = &self.title {
            doc.set_title(a.clone());
        }
        if let Some((_, a)) = self.color_mode {
            doc.set_color_mode(a);
        }
        if let Some((_, a)) = self.default_unit {
            doc.set_default_unit(a);
        }
        if let Some((_, a)) = self.dpi {
            doc.set_dpi(a);
        }
        if let Some((_, a)) = &self.pages {
            *doc.pages_mut() = a.clone();
        }
        if let Some((_, a)) = self.active_page_index {
            doc.set_active_page_index_raw(a);
        }
        if let Some((_, a)) = self.multipage_view {
            doc.set_multipage_view(a);
        }
        for d in &self.objects {
            match &d.after {
                Some(obj) => {
                    doc.objects_mut().insert(d.id, obj.clone());
                }
                None => {
                    doc.objects_mut().remove(&d.id);
                }
            }
        }
        for d in &self.assets {
            match &d.after {
                Some(asset) => {
                    doc.assets_mut().insert(d.id, asset.clone());
                }
                None => {
                    doc.assets_mut().remove(&d.id);
                }
            }
        }
        if let Some((_, a)) = &self.palettes {
            *doc.palettes_mut_raw() = a.clone();
        }
        if let Some((_, a)) = &self.document_palette {
            *doc.document_palette_mut_raw() = a.clone();
        }
        if let Some((_, a)) = &self.color_styles {
            *doc.color_styles_mut() = a.clone();
        }
        if let Some((_, a)) = &self.object_styles {
            *doc.object_styles_mut() = a.clone();
        }
        for d in &self.symbols {
            match &d.after {
                Some(sym) => {
                    doc.symbols_mut().insert(d.id, sym.clone());
                }
                None => {
                    doc.symbols_mut().remove(&d.id);
                }
            }
        }
        if let Some((_, a)) = &self.guides {
            *doc.guides_mut() = a.clone();
        }
        if let Some((_, a)) = self.grid {
            doc.set_grid(a);
        }
        if let Some((_, a)) = &self.perspective_grid {
            doc.set_perspective_grid(a.clone());
        }
        if let Some((_, a)) = &self.unknown_doc_chunks {
            *doc.unknown_doc_chunks_mut_raw() = a.clone();
        }
        if let Some((_, a)) = &self.unknown_data_files {
            *doc.unknown_data_files_mut_raw() = a.clone();
        }
        if self.custom_icc_profile.is_some() || self.custom_icc_name.is_some() {
            let prof = self.custom_icc_profile.as_ref().map_or_else(
                || doc.custom_icc_profile().map(<[u8]>::to_vec),
                |(_, a)| a.clone(),
            );
            let name = self.custom_icc_name.as_ref().map_or_else(
                || doc.custom_icc_name().map(str::to_owned),
                |(_, a)| a.clone(),
            );
            doc.set_custom_icc_raw(prof, name);
        }
        if let Some((_, a)) = self.next_id {
            doc.set_next_id_raw(a);
        }
        if let Some((_, a)) = &self.metadata {
            *doc.metadata_mut() = a.clone();
        }
    }
}

/// Targeted or full-document canvas cache invalidation summary produced by [`CommandHistory`].
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DocumentInvalidation {
    /// Whole-viewport / multi-page / style / symbol / guide / grid invalidation required.
    #[default]
    All,
    /// Only the listed objects and world-space bounding rectangles (`before ∪ after`) changed.
    Targeted {
        object_ids: Vec<ObjectId>,
        dirty_rects_mm: Vec<Rect2D>,
    },
}

enum BeforeBounds {
    Rect(Rect2D),
    Empty,
    Unresolved,
}

fn pages_change_is_only_layer_objects(
    before: &[Page],
    after: &[Page],
    reordered_ids: &mut Vec<ObjectId>,
) -> bool {
    if before.len() != after.len() {
        return false;
    }
    for (bp, ap) in before.iter().zip(after.iter()) {
        if bp.id != ap.id
            || (bp.width_mm - ap.width_mm).abs() > 1e-9
            || (bp.height_mm - ap.height_mm).abs() > 1e-9
            || (bp.bleed_mm - ap.bleed_mm).abs() > 1e-9
            || bp.facing_pages != ap.facing_pages
            || bp.layers.len() != ap.layers.len()
        {
            return false;
        }
        for (bl, al) in bp.layers.iter().zip(ap.layers.iter()) {
            if bl.id != al.id
                || bl.visible != al.visible
                || bl.printable != al.printable
                || bl.master_scope != al.master_scope
                || bl.is_cut_layer != al.is_cut_layer
                || bl.is_desktop_layer != al.is_desktop_layer
            {
                return false;
            }
            if bl.objects != al.objects {
                for &id in bl.objects.iter().chain(al.objects.iter()) {
                    if !reordered_ids.contains(&id) {
                        reordered_ids.push(id);
                    }
                }
            }
        }
    }
    true
}

fn delta_requires_full_invalidation(
    delta: &DocumentDelta,
    reordered_ids: &mut Vec<ObjectId>,
) -> bool {
    if delta.color_mode.is_some()
        || delta.active_page_index.is_some()
        || delta.multipage_view.is_some()
        || delta.color_styles.is_some()
        || delta.object_styles.is_some()
        || !delta.symbols.is_empty()
        || delta.guides.is_some()
        || delta.grid.is_some()
        || delta.perspective_grid.is_some()
        || delta.custom_icc_profile.is_some()
    {
        return true;
    }
    if let Some((ref bp, ref ap)) = delta.pages {
        if !pages_change_is_only_layer_objects(bp, ap, reordered_ids) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    label: String,
    op: CommandOp,
    delta: DocumentDelta,
    invalidation: DocumentInvalidation,
}

#[derive(Debug, Clone)]
struct ActiveTransaction {
    label: String,
    before: DocumentStateSnapshot,
    ops: Vec<CommandOp>,
}

/// Transactional Undo/Redo stack, deterministic command journal, and Rhai macro recorder.
#[derive(Debug, Clone)]
pub struct CommandHistory {
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    active_tx: Option<ActiveTransaction>,
    max_history: usize,
    /// Tracks whether the most recent operation pushed a fresh `HistoryEntry`
    /// so `gc_after_commit` never mutates an older entry after `undo`/`redo`/empty-tx.
    pending_gc_on_last_entry: bool,
    /// Sorted asset IDs pruned during the most recent `gc_after_commit` call so WAL
    /// callers can journal `CommandOp::PruneAssets` deterministically.
    last_gc_pruned: Vec<ImageAssetId>,
    /// Targeted or full invalidation emitted by the most recent `execute`/`undo`/`redo`.
    last_invalidation: DocumentInvalidation,
    /// When `true`, every committed command is recorded for Rhai macro export.
    pub recording_macro: bool,
    recorded_ops: Vec<CommandOp>,
}

impl Default for CommandHistory {
    fn default() -> Self {
        Self::new(200)
    }
}

impl CommandHistory {
    /// Creates an empty command history with the given undo retention limit.
    #[must_use]
    pub fn new(max_history: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            active_tx: None,
            max_history: max_history.max(1),
            pending_gc_on_last_entry: false,
            last_gc_pruned: Vec::new(),
            last_invalidation: DocumentInvalidation::All,
            recording_macro: false,
            recorded_ops: Vec::new(),
        }
    }

    /// Returns the targeted or full invalidation summary produced by the most recent
    /// [`Self::execute`], [`Self::undo`], or [`Self::redo`].
    #[must_use]
    pub const fn last_invalidation(&self) -> &DocumentInvalidation {
        &self.last_invalidation
    }

    /// Returns the configured undo stack retention limit.
    #[must_use]
    pub const fn max_history(&self) -> usize {
        self.max_history
    }

    /// Drains and returns the sorted list of [`ImageAssetId`]s pruned by the most recent
    /// [`Self::gc_after_commit`] call (used to keep WAL command journals synchronized across
    /// `max_history` eviction).
    pub fn take_last_gc_pruned(&mut self) -> Vec<ImageAssetId> {
        std::mem::take(&mut self.last_gc_pruned)
    }

    /// Collects every [`ImageAssetId`] currently referenced by undo/redo deltas or the active transaction
    /// so [`Document::prune_unreferenced_assets`] never deletes raster data still needed for `Ctrl+Z`/`Ctrl+Y`.
    #[must_use]
    pub fn referenced_asset_ids(&self) -> std::collections::HashSet<ImageAssetId> {
        fn collect_op_asset_refs(op: &CommandOp, refs: &mut HashSet<ImageAssetId>) {
            match op {
                CommandOp::RegisterAsset { asset } => {
                    refs.insert(asset.id);
                }
                CommandOp::CreateObject { object, .. }
                | CommandOp::CreateDetachedObject { object } => {
                    object.collect_asset_refs(refs);
                }
                CommandOp::UpdateRaster { data, .. } => {
                    refs.insert(data.asset_id);
                }
                CommandOp::SetFill {
                    fill: Fill::BitmapPattern { image_id, .. },
                    ..
                } => {
                    refs.insert(*image_id);
                }
                CommandOp::UpsertObjectStyle { style } => {
                    if let Some(Fill::BitmapPattern { image_id, .. }) = style.fill {
                        refs.insert(image_id);
                    }
                }
                CommandOp::UpsertSymbol { symbol } => {
                    for sub in &symbol.objects {
                        sub.collect_asset_refs(refs);
                    }
                }
                CommandOp::EditTable { table, .. } => {
                    for cell in &table.cells {
                        if let Fill::BitmapPattern { image_id, .. } = cell.fill {
                            refs.insert(image_id);
                        }
                    }
                }
                CommandOp::Batch(sub_ops) => {
                    for sub in sub_ops {
                        collect_op_asset_refs(sub, refs);
                    }
                }
                _ => {}
            }
        }

        let mut refs = std::collections::HashSet::new();
        for entry in self.undo_stack.iter().chain(self.redo_stack.iter()) {
            collect_op_asset_refs(&entry.op, &mut refs);
            for od in &entry.delta.objects {
                if let Some(ref b) = od.before {
                    b.collect_asset_refs(&mut refs);
                }
                if let Some(ref a) = od.after {
                    a.collect_asset_refs(&mut refs);
                }
            }
            for sd in &entry.delta.symbols {
                if let Some(ref b) = sd.before {
                    for sub in &b.objects {
                        sub.collect_asset_refs(&mut refs);
                    }
                }
                if let Some(ref a) = sd.after {
                    for sub in &a.objects {
                        sub.collect_asset_refs(&mut refs);
                    }
                }
            }
            if let Some((ref before_styles, ref after_styles)) = entry.delta.object_styles {
                for style in before_styles.iter().chain(after_styles.iter()) {
                    if let Some(crate::style::Fill::BitmapPattern { image_id, .. }) = style.fill {
                        refs.insert(image_id);
                    }
                }
            }
        }
        if let Some(ref tx) = self.active_tx {
            for op in &tx.ops {
                collect_op_asset_refs(op, &mut refs);
            }
            for obj in tx.before.objects.values() {
                obj.collect_asset_refs(&mut refs);
            }
            for sym in tx.before.symbols.values() {
                for sub in &sym.objects {
                    sub.collect_asset_refs(&mut refs);
                }
            }
            for style in &tx.before.object_styles {
                if let Some(crate::style::Fill::BitmapPattern { image_id, .. }) = style.fill {
                    refs.insert(image_id);
                }
            }
        }
        refs
    }

    /// Garbage-collects unreferenced [`crate::RasterAsset`] blobs from `doc` that are no longer
    /// used by the live document or by any entry on the Undo/Redo stacks.
    pub fn gc_document_assets(&self, doc: &mut Document) -> usize {
        let kept = self.referenced_asset_ids();
        doc.prune_unreferenced_assets(&kept)
    }

    /// Garbage-collects unreferenced assets immediately after pushing/committing an entry,
    /// recording any pruned pre-existing assets in that entry's `delta.assets` so `undo`
    /// preserves exact `before` parity.
    pub fn gc_after_commit(&mut self, doc: &mut Document) -> usize {
        let kept = self.referenced_asset_ids();
        let pruned = doc.prune_unreferenced_assets_returning(&kept);
        let count = pruned.len();
        let mut pruned_ids: Vec<ImageAssetId> = pruned.iter().map(|a| a.id).collect();
        pruned_ids.sort_by_key(|id| id.0);
        self.last_gc_pruned = pruned_ids;
        let should_record = std::mem::take(&mut self.pending_gc_on_last_entry);
        if should_record {
            if let Some(last) = self.undo_stack.last_mut() {
                for asset in pruned {
                    let id = asset.id;
                    if let Some(pos) = last.delta.assets.iter().position(|d| d.id == id) {
                        if last.delta.assets[pos].before.is_some() {
                            last.delta.assets[pos].after = None;
                        } else {
                            last.delta.assets.remove(pos);
                        }
                    } else {
                        last.delta.assets.push(AssetDelta {
                            id,
                            before: Some(asset),
                            after: None,
                        });
                    }
                }
            }
        }
        count
    }

    /// Executes `op` on `doc`, recording it in the undo history (or active transaction).
    pub fn execute(&mut self, doc: &mut Document, op: CommandOp) -> CoreResult<()> {
        if let Some(tx) = self.active_tx.as_mut() {
            let rollback = DocumentStateSnapshot::capture(doc);
            if let Err(err) = apply_command(doc, &op) {
                rollback.restore_into(doc);
                return Err(err);
            }
            tx.ops.push(op);
            return Ok(());
        }

        let before = DocumentStateSnapshot::capture(doc);
        if let Err(err) = apply_command(doc, &op) {
            // Roll back partial mutation on error (e.g. failed batch or locked object)
            before.restore_into(doc);
            return Err(err);
        }

        let delta = before.into_delta(doc);
        let invalidation = Self::compute_invalidation_with_doc_and_delta(doc, &delta);
        self.last_invalidation = invalidation.clone();
        if self.recording_macro {
            self.recorded_ops.push(op.clone());
        }
        let label = op.default_label();
        self.push_entry(HistoryEntry {
            label,
            op,
            delta,
            invalidation,
        });
        self.gc_after_commit(doc);
        Ok(())
    }

    fn compute_invalidation_with_doc_and_delta(
        after_doc: &Document,
        delta: &DocumentDelta,
    ) -> DocumentInvalidation {
        let mut reordered_ids = Vec::new();
        if delta_requires_full_invalidation(delta, &mut reordered_ids) {
            return DocumentInvalidation::All;
        }
        let mut object_ids: Vec<ObjectId> = delta.objects.iter().map(|d| d.id).collect();
        let mut object_id_set: HashSet<ObjectId> = object_ids.iter().copied().collect();
        for rid in reordered_ids {
            if object_id_set.insert(rid) {
                object_ids.push(rid);
            }
        }

        // Propagate invalidation to relational dependents (`TextOnPath`, `Connector`,
        // `Effect::Blend`, and `SymbolInstance`s referencing changed objects):
        // 1. Expand downward into `Group`/`PowerClip` children of changed/reordered objects
        //    so dependents attached to moved/deleted descendants are discovered.
        // 2. Walk upward through enclosing `Group`/`PowerClip` ancestors (without
        //    re-expanding untouched sibling children) and add every ancestor to both
        //    `affected_set` and `object_ids` so container bounds/effects and dependents
        //    attached to enclosing `Group`s are covered across execute/undo/redo.
        let mut affected_set: HashSet<ObjectId> = object_id_set.clone();
        let mut expand_queue: std::collections::VecDeque<(ObjectId, usize)> =
            affected_set.iter().copied().map(|id| (id, 0)).collect();
        let mut expanded_visited: HashSet<ObjectId> = HashSet::new();
        while let Some((cur_id, depth)) = expand_queue.pop_front() {
            if depth > 64 || !expanded_visited.insert(cur_id) {
                continue;
            }
            let od_before = delta
                .objects
                .iter()
                .find(|d| d.id == cur_id)
                .and_then(|d| d.before.as_ref());
            let after_obj = after_doc.object(cur_id);
            if let (Some(before_obj), Some(after_obj)) = (od_before, after_obj) {
                if let Some(diff_children) =
                    Self::container_children_only_diff(before_obj, after_obj)
                {
                    for cid in diff_children {
                        affected_set.insert(cid);
                        if !expanded_visited.contains(&cid) {
                            expand_queue.push_back((cid, depth + 1));
                        }
                    }
                    continue;
                }
            }
            let mut enqueue_children = |obj: &SceneObject| match &obj.payload {
                NodePayload::Group(children) => {
                    for &cid in children {
                        affected_set.insert(cid);
                        if !expanded_visited.contains(&cid) {
                            expand_queue.push_back((cid, depth + 1));
                        }
                    }
                }
                NodePayload::PowerClip { contents, .. } => {
                    for &cid in contents {
                        affected_set.insert(cid);
                        if !expanded_visited.contains(&cid) {
                            expand_queue.push_back((cid, depth + 1));
                        }
                    }
                }
                _ => {}
            };
            if let Some(after_obj) = after_obj {
                enqueue_children(after_obj);
            }
            if let Some(before_obj) = od_before {
                enqueue_children(before_obj);
            }
        }

        let mut upward_visited: HashSet<ObjectId> = HashSet::new();
        let walk_upward_ancestors =
            |seed_id: ObjectId,
             affected_set: &mut HashSet<ObjectId>,
             object_id_set: &mut HashSet<ObjectId>,
             object_ids: &mut Vec<ObjectId>,
             upward_visited: &mut HashSet<ObjectId>| {
                let mut q: std::collections::VecDeque<(ObjectId, usize)> =
                    std::collections::VecDeque::from([(seed_id, 0)]);
                while let Some((cur_id, depth)) = q.pop_front() {
                    if depth > 64 || !upward_visited.insert(cur_id) {
                        continue;
                    }
                    for pid in [
                        after_doc.parent_container_of(cur_id),
                        Self::before_parent_of(cur_id, delta, after_doc),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        let is_container = after_doc
                            .object(pid)
                            .or_else(|| Self::before_object_of(pid, delta, after_doc))
                            .is_some_and(|p| {
                                matches!(
                                    p.payload,
                                    NodePayload::Group(_) | NodePayload::PowerClip { .. }
                                )
                            });
                        if is_container {
                            affected_set.insert(pid);
                            if object_id_set.insert(pid) {
                                object_ids.push(pid);
                            }
                            if !upward_visited.contains(&pid) {
                                q.push_back((pid, depth + 1));
                            }
                        }
                    }
                }
            };

        let initial_seeds: Vec<ObjectId> = object_ids.clone();
        for sid in initial_seeds {
            walk_upward_ancestors(
                sid,
                &mut affected_set,
                &mut object_id_set,
                &mut object_ids,
                &mut upward_visited,
            );
        }

        loop {
            let mut newly_added = Vec::new();
            for (&oid, obj) in after_doc.objects() {
                if !affected_set.contains(&oid) {
                    let mut visiting_syms = HashSet::new();
                    if Self::object_depends_on_any(
                        obj,
                        &affected_set,
                        after_doc,
                        0,
                        &mut visiting_syms,
                    ) {
                        affected_set.insert(oid);
                        if object_id_set.insert(oid) {
                            object_ids.push(oid);
                        }
                        newly_added.push(oid);
                    }
                }
            }
            for d in &delta.objects {
                if let Some(ref before_obj) = d.before {
                    if !affected_set.contains(&d.id) {
                        let mut visiting_syms = HashSet::new();
                        if Self::object_depends_on_any(
                            before_obj,
                            &affected_set,
                            after_doc,
                            0,
                            &mut visiting_syms,
                        ) {
                            affected_set.insert(d.id);
                            if object_id_set.insert(d.id) {
                                object_ids.push(d.id);
                            }
                            newly_added.push(d.id);
                        }
                    }
                }
            }
            if newly_added.is_empty() {
                break;
            }
            for dep_id in newly_added {
                walk_upward_ancestors(
                    dep_id,
                    &mut affected_set,
                    &mut object_id_set,
                    &mut object_ids,
                    &mut upward_visited,
                );
            }
        }

        let mut dirty_rects_mm = Vec::with_capacity(object_ids.len() * 2);
        for &oid in &object_ids {
            if let Some(bb) = after_doc.visual_world_bounds(oid) {
                dirty_rects_mm.push(bb);
            }
        }

        for &oid in &object_ids {
            if let Some(before_obj) = Self::before_object_of(oid, delta, after_doc) {
                let Some(world_xf) = Self::before_world_transform_of(oid, delta, after_doc) else {
                    return DocumentInvalidation::All;
                };
                let mut visiting_objs = HashSet::new();
                visiting_objs.insert(oid);
                let mut visiting_syms = HashSet::new();
                match Self::conservative_before_bounds(
                    before_obj,
                    &world_xf,
                    None,
                    delta,
                    after_doc,
                    0,
                    &mut visiting_objs,
                    &mut visiting_syms,
                    true,
                ) {
                    BeforeBounds::Rect(bb) => dirty_rects_mm.push(bb),
                    BeforeBounds::Empty => {}
                    BeforeBounds::Unresolved => return DocumentInvalidation::All,
                }
            }
        }
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        }
    }

    fn object_depends_on_any(
        obj: &SceneObject,
        affected: &HashSet<ObjectId>,
        after_doc: &Document,
        depth: usize,
        visiting_syms: &mut HashSet<SymbolId>,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        match &obj.payload {
            NodePayload::TextOnPath(top) if affected.contains(&top.path_id) => return true,
            NodePayload::Connector(conn)
                if conn.from_obj.is_some_and(|id| affected.contains(&id))
                    || conn.to_obj.is_some_and(|id| affected.contains(&id)) =>
            {
                return true;
            }
            NodePayload::SymbolInstance(sym_id) if visiting_syms.insert(*sym_id) => {
                let dep = after_doc.symbol(*sym_id).is_some_and(|sym| {
                    sym.objects.iter().any(|sub| {
                        Self::object_depends_on_any(
                            sub,
                            affected,
                            after_doc,
                            depth + 1,
                            visiting_syms,
                        )
                    })
                });
                visiting_syms.remove(sym_id);
                if dep {
                    return true;
                }
            }
            _ => {}
        }
        obj.effects.iter().any(|eff| match eff {
            Effect::Blend {
                target_id,
                along_path,
                ..
            } => {
                affected.contains(target_id) || along_path.is_some_and(|id| affected.contains(&id))
            }
            _ => false,
        })
    }

    fn before_object_of<'a>(
        id: ObjectId,
        delta: &'a DocumentDelta,
        after_doc: &'a Document,
    ) -> Option<&'a SceneObject> {
        if let Some(od) = delta.objects.iter().find(|d| d.id == id) {
            od.before.as_ref()
        } else {
            after_doc.object(id)
        }
    }

    fn before_parent_of(
        id: ObjectId,
        delta: &DocumentDelta,
        after_doc: &Document,
    ) -> Option<ObjectId> {
        for od in &delta.objects {
            if let Some(ref before_obj) = od.before {
                let contains = match &before_obj.payload {
                    NodePayload::Group(children) => children.contains(&id),
                    NodePayload::PowerClip { contents, .. } => contents.contains(&id),
                    _ => false,
                };
                if contains {
                    return Some(od.id);
                }
            }
        }
        if let Some(pid) = after_doc.parent_container_of(id) {
            if delta.objects.iter().all(|od| od.id != pid) {
                return Some(pid);
            }
        }
        None
    }

    fn before_world_transform_of(
        id: ObjectId,
        delta: &DocumentDelta,
        after_doc: &Document,
    ) -> Option<Affine2D> {
        let mut chain = Vec::new();
        let mut visited = HashSet::new();
        let mut cur = id;
        for _ in 0..65 {
            if !visited.insert(cur) {
                return None;
            }
            let obj = Self::before_object_of(cur, delta, after_doc)?;
            chain.push(obj.transform);
            if let Some(pid) = Self::before_parent_of(cur, delta, after_doc) {
                cur = pid;
            } else {
                break;
            }
        }
        let mut xf = Affine2D::IDENTITY;
        for t in chain.into_iter().rev() {
            xf = xf.compose(&t);
        }
        xf.is_finite().then_some(xf)
    }

    fn container_children_only_diff(
        before: &SceneObject,
        after: &SceneObject,
    ) -> Option<Vec<ObjectId>> {
        if before.transform != after.transform
            || before.fill != after.fill
            || before.outline != after.outline
            || before.lens != after.lens
            || before.effects != after.effects
            || before.blend_mode != after.blend_mode
            || before.opacity != after.opacity
            || before.visible != after.visible
            || before.printable != after.printable
            || before.style_id != after.style_id
        {
            return None;
        }
        let (b_list, a_list): (&[ObjectId], &[ObjectId]) = match (&before.payload, &after.payload) {
            (NodePayload::Group(bc), NodePayload::Group(ac)) => (bc.as_slice(), ac.as_slice()),
            (
                NodePayload::PowerClip {
                    container: bc,
                    container_parametric: bp,
                    lock_contents: bl,
                    contents: b_contents,
                },
                NodePayload::PowerClip {
                    container: ac,
                    container_parametric: ap,
                    lock_contents: al,
                    contents: a_contents,
                },
            ) if bc == ac && bp == ap && bl == al => (b_contents.as_slice(), a_contents.as_slice()),
            _ => return None,
        };
        let b_set: HashSet<ObjectId> = b_list.iter().copied().collect();
        let a_set: HashSet<ObjectId> = a_list.iter().copied().collect();
        let mut diff = Vec::new();
        for &id in b_list {
            if !a_set.contains(&id) && !diff.contains(&id) {
                diff.push(id);
            }
        }
        for &id in a_list {
            if !b_set.contains(&id) && !diff.contains(&id) {
                diff.push(id);
            }
        }
        Some(diff)
    }

    fn before_object_world_bounds_with_mode(
        id: ObjectId,
        delta: &DocumentDelta,
        after_doc: &Document,
        depth: usize,
        visiting_objs: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        include_effects_and_pad: bool,
    ) -> BeforeBounds {
        if depth > 64 || !visiting_objs.insert(id) {
            return BeforeBounds::Empty;
        }
        let res = if let Some(obj) = Self::before_object_of(id, delta, after_doc) {
            match Self::before_world_transform_of(id, delta, after_doc) {
                Some(wxf) => Self::conservative_before_bounds(
                    obj,
                    &wxf,
                    None,
                    delta,
                    after_doc,
                    depth,
                    visiting_objs,
                    visiting_syms,
                    include_effects_and_pad,
                ),
                None => BeforeBounds::Unresolved,
            }
        } else {
            BeforeBounds::Unresolved
        };
        visiting_objs.remove(&id);
        res
    }

    fn before_object_geometric_world_bounds(
        id: ObjectId,
        delta: &DocumentDelta,
        after_doc: &Document,
        depth: usize,
        visiting_objs: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
    ) -> BeforeBounds {
        Self::before_object_world_bounds_with_mode(
            id,
            delta,
            after_doc,
            depth,
            visiting_objs,
            visiting_syms,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn conservative_before_bounds(
        obj: &SceneObject,
        world_xf: &Affine2D,
        sym_map: Option<&std::collections::HashMap<ObjectId, &SceneObject>>,
        delta: &DocumentDelta,
        after_doc: &Document,
        depth: usize,
        visiting_objs: &mut HashSet<ObjectId>,
        visiting_syms: &mut HashSet<SymbolId>,
        include_effects_and_pad: bool,
    ) -> BeforeBounds {
        if depth > 64 || !world_xf.is_finite() {
            return BeforeBounds::Unresolved;
        }
        let mut acc = match &obj.payload {
            NodePayload::Path(p) => match p.transformed(world_xf).bounding_box() {
                Some(r) => r,
                None => return BeforeBounds::Empty,
            },
            NodePayload::Shape(s) => {
                match s.to_bezier_path().transformed(world_xf).bounding_box() {
                    Some(r) => r,
                    None => return BeforeBounds::Empty,
                }
            }
            NodePayload::PowerClip { container, .. } => {
                match container.transformed(world_xf).bounding_box() {
                    Some(r) => r,
                    None => return BeforeBounds::Empty,
                }
            }
            NodePayload::ArtisticText(t) => match world_xf.try_transform_rect(t.estimated_bounds())
            {
                Some(r) => r,
                None => return BeforeBounds::Unresolved,
            },
            NodePayload::ParagraphText(t) => match world_xf.try_transform_rect(t.bounding_box()) {
                Some(r) => r,
                None => return BeforeBounds::Unresolved,
            },
            NodePayload::Table(t) => match world_xf.try_transform_rect(t.to_bounding_box()) {
                Some(r) => r,
                None => return BeforeBounds::Unresolved,
            },
            NodePayload::Dimension(d) => match world_xf.try_transform_rect(d.bounding_box()) {
                Some(r) => r,
                None => return BeforeBounds::Unresolved,
            },
            NodePayload::Raster(r) => {
                let local_r = match r.crop_rect {
                    Some(cr) if cr.is_finite() => cr,
                    Some(_) => return BeforeBounds::Unresolved,
                    None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                        Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm)
                    }
                    None => return BeforeBounds::Unresolved,
                };
                match world_xf.try_transform_rect(local_r) {
                    Some(rect) => rect,
                    None => return BeforeBounds::Unresolved,
                }
            }
            NodePayload::Group(children) => {
                let mut g_acc: Option<Rect2D> = None;
                for &cid in children {
                    let child_opt = sym_map
                        .and_then(|m| m.get(&cid).copied())
                        .or_else(|| Self::before_object_of(cid, delta, after_doc));
                    let Some(child) = child_opt else {
                        return BeforeBounds::Unresolved;
                    };
                    if !visiting_objs.insert(cid) {
                        continue;
                    }
                    let cxf = world_xf.compose(&child.transform);
                    let child_res = Self::conservative_before_bounds(
                        child,
                        &cxf,
                        sym_map,
                        delta,
                        after_doc,
                        depth + 1,
                        visiting_objs,
                        visiting_syms,
                        include_effects_and_pad,
                    );
                    visiting_objs.remove(&cid);
                    match child_res {
                        BeforeBounds::Rect(cb) => {
                            g_acc = Some(match g_acc {
                                Some(prev) => prev.union(cb),
                                None => cb,
                            });
                        }
                        BeforeBounds::Empty => {}
                        BeforeBounds::Unresolved => return BeforeBounds::Unresolved,
                    }
                }
                match g_acc {
                    Some(r) => r,
                    None => return BeforeBounds::Empty,
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                let sym_opt = delta
                    .symbols
                    .iter()
                    .find(|sd| sd.id == *sym_id)
                    .and_then(|sd| sd.before.as_ref())
                    .or_else(|| after_doc.symbol(*sym_id));
                let Some(sym) = sym_opt else {
                    return BeforeBounds::Unresolved;
                };
                if !visiting_syms.insert(*sym_id) {
                    return BeforeBounds::Empty;
                }
                let mut local_sym_map: std::collections::HashMap<ObjectId, &SceneObject> =
                    std::collections::HashMap::new();
                let mut referenced_children: HashSet<ObjectId> = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        local_sym_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        NodePayload::Group(c) => {
                            referenced_children.extend(c.iter().copied());
                        }
                        NodePayload::PowerClip { contents, .. } => {
                            referenced_children.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                let mut s_acc: Option<Rect2D> = None;
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced_children.contains(&sub.id) {
                        continue;
                    }
                    let sub_xf = world_xf.compose(&sub.transform);
                    let sub_res = Self::conservative_before_bounds(
                        sub,
                        &sub_xf,
                        Some(&local_sym_map),
                        delta,
                        after_doc,
                        depth + 1,
                        visiting_objs,
                        visiting_syms,
                        include_effects_and_pad,
                    );
                    match sub_res {
                        BeforeBounds::Rect(rb) => {
                            s_acc = Some(match s_acc {
                                Some(prev) => prev.union(rb),
                                None => rb,
                            });
                        }
                        BeforeBounds::Empty => {}
                        BeforeBounds::Unresolved => {
                            visiting_syms.remove(sym_id);
                            return BeforeBounds::Unresolved;
                        }
                    }
                }
                visiting_syms.remove(sym_id);
                match s_acc {
                    Some(r) => r,
                    None => return BeforeBounds::Empty,
                }
            }
            NodePayload::Connector(conn) => {
                // Connectors resolve anchors against unpadded geometric world bounds
                // (`Document::resolve_connector_endpoints_inner` -> `object_bounds_inner_with_space`),
                // never shadow/outline/effect-inflated visual bounds.
                let from_pt = conn
                    .from_obj
                    .and_then(|fid| {
                        match Self::before_object_geometric_world_bounds(
                            fid,
                            delta,
                            after_doc,
                            depth + 1,
                            visiting_objs,
                            visiting_syms,
                        ) {
                            BeforeBounds::Rect(bb) => Some(bb),
                            _ => after_doc.world_object_bounds(fid),
                        }
                    })
                    .map_or_else(
                        || world_xf.transform_point(conn.from_fallback),
                        |bb| conn.from_anchor.resolve(bb),
                    );
                let to_pt = conn
                    .to_obj
                    .and_then(|tid| {
                        match Self::before_object_geometric_world_bounds(
                            tid,
                            delta,
                            after_doc,
                            depth + 1,
                            visiting_objs,
                            visiting_syms,
                        ) {
                            BeforeBounds::Rect(bb) => Some(bb),
                            _ => after_doc.world_object_bounds(tid),
                        }
                    })
                    .map_or_else(
                        || world_xf.transform_point(conn.to_fallback),
                        |bb| conn.to_anchor.resolve(bb),
                    );
                match conn.route_path(from_pt, to_pt).bounding_box() {
                    Some(r) if r.is_finite() => r,
                    _ => return BeforeBounds::Unresolved,
                }
            }
            NodePayload::TextOnPath(top) => {
                let host_rect = match Self::before_object_geometric_world_bounds(
                    top.path_id,
                    delta,
                    after_doc,
                    depth + 1,
                    visiting_objs,
                    visiting_syms,
                ) {
                    BeforeBounds::Rect(r) => Some(r),
                    _ => after_doc
                        .world_object_bounds(top.path_id)
                        .or_else(|| world_xf.try_transform_rect(top.text.estimated_bounds())),
                };
                let Some(hb) = host_rect else {
                    return BeforeBounds::Unresolved;
                };
                if include_effects_and_pad {
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
                    hb.expand(dist + max_font * 1.5)
                } else {
                    hb
                }
            }
        };
        if !include_effects_and_pad {
            return if acc.is_finite() {
                BeforeBounds::Rect(acc)
            } else {
                BeforeBounds::Unresolved
            };
        }
        for eff in &obj.effects {
            match eff {
                Effect::DropShadow {
                    dx_mm,
                    dy_mm,
                    blur_mm,
                    ..
                } => {
                    let shadow = Rect2D::from_xywh(
                        acc.min_x + dx_mm - blur_mm,
                        acc.min_y + dy_mm - blur_mm,
                        acc.width() + blur_mm * 2.0,
                        acc.height() + blur_mm * 2.0,
                    );
                    acc = acc.union(shadow);
                }
                Effect::BlockShadow { dx_mm, dy_mm, .. } => {
                    let shadow = Rect2D::from_xywh(
                        acc.min_x + dx_mm,
                        acc.min_y + dy_mm,
                        acc.width(),
                        acc.height(),
                    );
                    acc = acc.union(shadow);
                }
                Effect::Contour { offset_mm, .. } => {
                    acc = acc.expand(offset_mm.abs());
                }
                Effect::Extrude25D {
                    vanishing_point, ..
                } => {
                    if vanishing_point.is_finite() {
                        acc = acc.union(Rect2D::from_points(*vanishing_point, *vanishing_point));
                    }
                }
                Effect::Distort { amplitude, .. } => {
                    acc = acc.expand(amplitude.abs());
                }
                Effect::Blend {
                    target_id,
                    along_path,
                    ..
                } => {
                    if let BeforeBounds::Rect(tb) = Self::before_object_geometric_world_bounds(
                        *target_id,
                        delta,
                        after_doc,
                        depth + 1,
                        visiting_objs,
                        visiting_syms,
                    ) {
                        acc = acc.union(tb);
                    } else if let Some(tb) = after_doc.world_object_bounds(*target_id) {
                        acc = acc.union(tb);
                    }
                    if let Some(apid) = along_path {
                        if let BeforeBounds::Rect(ab) = Self::before_object_geometric_world_bounds(
                            *apid,
                            delta,
                            after_doc,
                            depth + 1,
                            visiting_objs,
                            visiting_syms,
                        ) {
                            acc = acc.union(ab);
                        } else if let Some(ab) = after_doc.world_object_bounds(*apid) {
                            acc = acc.union(ab);
                        }
                    }
                }
                _ => {}
            }
        }
        if obj
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Symmetry { .. }))
        {
            let seed = acc;
            for sym_xf in Effect::symmetry_transforms_for(&obj.effects) {
                if let Some(sb) = sym_xf.try_transform_rect(seed) {
                    acc = acc.union(sb);
                }
            }
        }
        let sw = if obj.outline.is_visible() {
            obj.outline.scaled_width_mm_for_transform(world_xf)
        } else {
            0.0
        };
        let eff_pad = Effect::effects_bounds_padding_mm(&obj.effects);
        let pad = (sw * 0.5).max(0.0) + eff_pad + 2.0;
        let out = acc.expand(pad);
        if out.is_finite() {
            BeforeBounds::Rect(out)
        } else {
            BeforeBounds::Unresolved
        }
    }

    /// Begins a multi-command transaction (e.g. interactive drag or multi-step wizard)
    /// that commits as a single merged undo step.
    pub fn begin_transaction(
        &mut self,
        doc: &Document,
        label: impl Into<String>,
    ) -> CoreResult<()> {
        if self.active_tx.is_some() {
            return Err(CoreError::TransactionAlreadyActive);
        }
        self.pending_gc_on_last_entry = false;
        self.last_gc_pruned.clear();
        self.active_tx = Some(ActiveTransaction {
            label: label.into(),
            before: DocumentStateSnapshot::capture(doc),
            ops: Vec::new(),
        });
        Ok(())
    }

    /// Commits the active transaction, merging all executed commands into a single history entry.
    pub fn commit_transaction(&mut self, doc: &Document) -> CoreResult<()> {
        let tx = self
            .active_tx
            .take()
            .ok_or(CoreError::TransactionNotActive)?;
        if tx.ops.is_empty() {
            self.pending_gc_on_last_entry = false;
            self.last_gc_pruned.clear();
            return Ok(());
        }
        let merged_op = if tx.ops.len() == 1 {
            tx.ops.into_iter().next().expect("len == 1")
        } else {
            CommandOp::Batch(tx.ops)
        };
        if self.recording_macro {
            self.recorded_ops.push(merged_op.clone());
        }
        let delta = tx.before.into_delta(doc);
        let invalidation = Self::compute_invalidation_with_doc_and_delta(doc, &delta);
        self.last_invalidation = invalidation.clone();
        self.push_entry(HistoryEntry {
            label: tx.label,
            op: merged_op,
            delta,
            invalidation,
        });
        Ok(())
    }

    /// Cancels the active transaction and rolls `doc` back to its pre-transaction state.
    pub fn cancel_transaction(&mut self, doc: &mut Document) -> CoreResult<()> {
        let tx = self
            .active_tx
            .take()
            .ok_or(CoreError::TransactionNotActive)?;
        self.pending_gc_on_last_entry = false;
        self.last_gc_pruned.clear();
        self.last_invalidation = DocumentInvalidation::All;
        tx.before.restore_into(doc);
        Ok(())
    }

    fn push_entry(&mut self, entry: HistoryEntry) {
        self.redo_stack.clear();
        self.undo_stack.push(entry);
        if self.undo_stack.len() > self.max_history {
            self.undo_stack.remove(0);
        }
        self.pending_gc_on_last_entry = true;
    }

    /// Undoes the most recent command or transaction.
    ///
    /// Fails with [`CoreError::TransactionAlreadyActive`] if called while a transaction is open.
    pub fn undo(&mut self, doc: &mut Document) -> CoreResult<String> {
        if self.active_tx.is_some() {
            return Err(CoreError::TransactionAlreadyActive);
        }
        let entry = self.undo_stack.pop().ok_or(CoreError::EmptyUndoStack)?;
        let label = entry.label.clone();
        entry.delta.apply_before(doc);
        self.last_invalidation = entry.invalidation.clone();
        self.redo_stack.push(entry);
        self.pending_gc_on_last_entry = false;
        self.last_gc_pruned.clear();
        Ok(label)
    }

    /// Redoes the most recently undone command or transaction.
    ///
    /// Fails with [`CoreError::TransactionAlreadyActive`] if called while a transaction is open.
    pub fn redo(&mut self, doc: &mut Document) -> CoreResult<String> {
        if self.active_tx.is_some() {
            return Err(CoreError::TransactionAlreadyActive);
        }
        let entry = self.redo_stack.pop().ok_or(CoreError::EmptyRedoStack)?;
        let label = entry.label.clone();
        entry.delta.apply_after(doc);
        self.last_invalidation = entry.invalidation.clone();
        self.undo_stack.push(entry);
        self.pending_gc_on_last_entry = false;
        self.last_gc_pruned.clear();
        Ok(label)
    }

    /// Returns `true` if at least one command can be undone.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Returns `true` if at least one command can be redone.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Labels of the undo stack from oldest to most recent.
    #[must_use]
    pub fn undo_labels(&self) -> Vec<&str> {
        self.undo_stack.iter().map(|e| e.label.as_str()).collect()
    }

    /// Labels of the redo stack from oldest to next-to-redo.
    #[must_use]
    pub fn redo_labels(&self) -> Vec<&str> {
        self.redo_stack.iter().map(|e| e.label.as_str()).collect()
    }

    /// Returns the ordered deterministic command log currently applied to the document.
    #[must_use]
    pub fn command_log(&self) -> Vec<CommandOp> {
        self.undo_stack.iter().map(|e| e.op.clone()).collect()
    }

    /// Exports the applied command log as formatted JSON.
    pub fn export_log_json(&self) -> CoreResult<String> {
        Ok(serde_json::to_string_pretty(&self.command_log())?)
    }

    /// Parses a JSON command log previously exported by [`Self::export_log_json`].
    pub fn import_log_json(json: &str) -> CoreResult<Vec<CommandOp>> {
        Ok(serde_json::from_str(json)?)
    }

    /// Deterministically replays a sequence of [`CommandOp`]s onto `doc`.
    pub fn replay_log(doc: &mut Document, ops: &[CommandOp]) -> CoreResult<()> {
        for (index, op) in ops.iter().enumerate() {
            if let Err(err) = apply_command(doc, op) {
                return Err(CoreError::CommandReplayFailed {
                    index,
                    reason: err.to_string(),
                });
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Record Macro -> Rhai Script Engine
    // -------------------------------------------------------------------------

    /// Starts recording subsequent executed commands into a Rhai macro buffer.
    pub fn start_macro_recording(&mut self) {
        self.recorded_ops.clear();
        self.recording_macro = true;
    }

    /// Stops macro recording and emits an executable Rhai script reproducing every recorded command.
    pub fn stop_macro_to_rhai(&mut self) -> String {
        self.recording_macro = false;
        let ops = std::mem::take(&mut self.recorded_ops);
        let mut script = String::from("// coree recorded macro (Rhai)\n");
        for op in &ops {
            emit_op_rhai(op, &mut script);
        }
        script
    }
}

fn format_id_array(ids: &[ObjectId]) -> String {
    let items: Vec<String> = ids.iter().map(|id| id.0.to_string()).collect();
    format!("[{}]", items.join(", "))
}

fn escape_rhai_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn emit_op_rhai(op: &CommandOp, out: &mut String) {
    match op {
        CommandOp::Batch(sub_ops) => {
            out.push_str("// begin batch\n");
            for sub in sub_ops {
                emit_op_rhai(sub, out);
            }
            out.push_str("// end batch\n");
        }
        CommandOp::TransformObjects { ids, affine } => {
            let [a, b, c, d, tx, ty] = affine.m;
            out.push_str(&format!(
                "transform_objects({}, [{a:?}, {b:?}, {c:?}, {d:?}, {tx:?}, {ty:?}]);\n",
                format_id_array(ids)
            ));
        }
        CommandOp::SetFill {
            ids,
            fill: Fill::Solid(color @ Color::Rgba { .. }),
        } => {
            let hex = color.to_hex_rgba();
            out.push_str(&format!(
                "set_fill_hex({}, \"{hex}\");\n",
                format_id_array(ids)
            ));
        }
        CommandOp::AlignObjects { ids, op } => {
            let mode = match op {
                AlignOp::Left => "left",
                AlignOp::CenterH => "center_h",
                AlignOp::Right => "right",
                AlignOp::Top => "top",
                AlignOp::CenterV => "center_v",
                AlignOp::Bottom => "bottom",
                AlignOp::CenterPage => "center_page",
            };
            out.push_str(&format!(
                "align_objects({}, \"{mode}\");\n",
                format_id_array(ids)
            ));
        }
        CommandOp::DeleteObjects { ids } => {
            out.push_str(&format!("delete_objects({});\n", format_id_array(ids)));
        }
        CommandOp::Group { ids, name } => {
            out.push_str(&format!(
                "group_objects({}, \"{}\");\n",
                format_id_array(ids),
                escape_rhai_str(name)
            ));
        }
        CommandOp::ConvertToCurves { id } => {
            out.push_str(&format!("convert_to_curves({});\n", id.0));
        }
        CommandOp::AddPage {
            name,
            width_mm,
            height_mm,
            bleed_mm,
        } => {
            out.push_str(&format!(
                "add_page(\"{}\", {width_mm:?}, {height_mm:?}, {bleed_mm:?});\n",
                escape_rhai_str(name)
            ));
        }
        other => {
            if let Ok(json) = serde_json::to_string(other) {
                out.push_str(&format!(
                    "exec_command_json(\"{}\");\n",
                    escape_rhai_str(&json)
                ));
            }
        }
    }
}

/// Converts a Rhai integer array into typed [`ObjectId`] values (skipping negatives).
#[must_use]
pub fn rhai_array_to_ids(arr: rhai::Array) -> Vec<ObjectId> {
    arr.into_iter()
        .filter_map(|v: Dynamic| v.as_int().ok().map(|n| ObjectId(n.max(0) as u64)))
        .collect()
}

/// Runs a Rhai macro script (such as one emitted by [`CommandHistory::stop_macro_to_rhai`])
/// inside a sandboxed `rhai::Engine`, returning the number of commands executed.
pub fn run_rhai_macro(
    doc: &mut Document,
    history: &mut CommandHistory,
    script: &str,
) -> CoreResult<usize> {
    let queued: Rc<RefCell<Vec<CommandOp>>> = Rc::new(RefCell::new(Vec::new()));
    let mut engine = Engine::new();
    // Sandbox safety limits against infinite loops or runaway memory
    engine.set_max_operations(100_000);
    engine.set_max_expr_depths(32, 32);
    engine.set_max_string_size(1_048_576);

    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "transform_objects",
            move |ids: rhai::Array, m: rhai::Array| -> Result<(), Box<EvalAltResult>> {
                if m.len() != 6 {
                    return Err(format!(
                        "transform_objects expects a 6-element affine array, got {}",
                        m.len()
                    )
                    .into());
                }
                let mut coeffs = [0.0_f64; 6];
                for (i, v) in m.into_iter().enumerate() {
                    coeffs[i] = v
                        .as_float()
                        .or_else(|_| v.as_int().map(|n| n as f64))
                        .map_err(|_| -> Box<EvalAltResult> {
                            format!("transform_objects matrix entry {i} must be numeric").into()
                        })?;
                }
                q.borrow_mut().push(CommandOp::TransformObjects {
                    ids: rhai_array_to_ids(ids),
                    affine: Affine2D { m: coeffs },
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "set_fill_hex",
            move |ids: rhai::Array, hex: &str| -> Result<(), Box<EvalAltResult>> {
                let color = Color::from_hex(hex)
                    .map_err(|e| -> Box<EvalAltResult> { format!("set_fill_hex: {e}").into() })?;
                q.borrow_mut().push(CommandOp::SetFill {
                    ids: rhai_array_to_ids(ids),
                    fill: Fill::Solid(color),
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "align_objects",
            move |ids: rhai::Array, mode: &str| -> Result<(), Box<EvalAltResult>> {
                let op = match mode {
                    "left" => AlignOp::Left,
                    "center_h" => AlignOp::CenterH,
                    "right" => AlignOp::Right,
                    "top" => AlignOp::Top,
                    "center_v" => AlignOp::CenterV,
                    "bottom" => AlignOp::Bottom,
                    "center_page" => AlignOp::CenterPage,
                    other => {
                        return Err(format!("align_objects: unknown align mode '{other}'").into());
                    }
                };
                q.borrow_mut().push(CommandOp::AlignObjects {
                    ids: rhai_array_to_ids(ids),
                    op,
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn("delete_objects", move |ids: rhai::Array| {
            q.borrow_mut().push(CommandOp::DeleteObjects {
                ids: rhai_array_to_ids(ids),
            });
        });
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn("group_objects", move |ids: rhai::Array, name: &str| {
            q.borrow_mut().push(CommandOp::Group {
                ids: rhai_array_to_ids(ids),
                name: name.to_owned(),
            });
        });
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "convert_to_curves",
            move |id: i64| -> Result<(), Box<EvalAltResult>> {
                if id <= 0 {
                    return Err(format!("convert_to_curves: invalid object id {id}").into());
                }
                q.borrow_mut().push(CommandOp::ConvertToCurves {
                    id: ObjectId(id as u64),
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        let dyn_to_f64 = |v: Dynamic, field: &str| -> Result<f64, Box<EvalAltResult>> {
            v.as_float()
                .or_else(|_| v.as_int().map(|n| n as f64))
                .map_err(|_| format!("add_page: '{field}' must be numeric").into())
        };
        engine.register_fn(
            "add_page",
            move |name: &str,
                  width_mm: Dynamic,
                  height_mm: Dynamic,
                  bleed_mm: Dynamic|
                  -> Result<(), Box<EvalAltResult>> {
                q.borrow_mut().push(CommandOp::AddPage {
                    name: name.to_owned(),
                    width_mm: dyn_to_f64(width_mm, "width_mm")?,
                    height_mm: dyn_to_f64(height_mm, "height_mm")?,
                    bleed_mm: dyn_to_f64(bleed_mm, "bleed_mm")?,
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "exec_command_json",
            move |json: &str| -> Result<(), Box<EvalAltResult>> {
                let op: CommandOp =
                    serde_json::from_str(json).map_err(|e| -> Box<EvalAltResult> {
                        format!("exec_command_json: invalid CommandOp JSON: {e}").into()
                    })?;
                q.borrow_mut().push(op);
                Ok(())
            },
        );
    }

    engine
        .run(script)
        .map_err(|e| CoreError::MacroError(e.to_string()))?;

    let ops = queued.borrow().clone();
    let count = ops.len();
    if count > 0 {
        history.begin_transaction(doc, "Run Macro")?;
        for op in ops {
            if let Err(err) = history.execute(doc, op) {
                let _ = history.cancel_transaction(doc);
                return Err(err);
            }
        }
        history.commit_transaction(doc)?;
        history.gc_after_commit(doc);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{CornerStyle, ParametricShape};
    use crate::lens::{LensConfig, LensKind};
    use crate::math::Point2D;

    fn sample_rect(name: &str, x: f64, y: f64) -> SceneObject {
        SceneObject::new(
            ObjectId(0),
            name,
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 10.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_transform(Affine2D::translate(x, y))
    }

    #[test]
    fn undo_redo_transaction_merging_and_deterministic_json_replay() {
        let initial = Document::new_a4("Replay Test");
        let mut doc = initial.clone();
        let mut hist = CommandHistory::default();

        // Execute commands inside a merged transaction
        hist.begin_transaction(&doc, "Create & Style Pair").unwrap();
        hist.execute(
            &mut doc,
            CommandOp::create_object(sample_rect("R1", 0.0, 0.0), None),
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::create_object(sample_rect("R2", 10.0, 0.0), None),
        )
        .unwrap();
        hist.commit_transaction(&doc).unwrap();
        assert_eq!(hist.undo_labels(), vec!["Create & Style Pair"]);
        assert_eq!(doc.objects().len(), 2);

        let ids: Vec<ObjectId> = doc.objects().keys().copied().collect();
        hist.execute(
            &mut doc,
            CommandOp::SetLens {
                ids: vec![ids[0]],
                lens: Some(LensConfig::new(LensKind::Invert)),
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::BooleanOp {
                op: BooleanOp::Weld,
                ids: ids.clone(),
            },
        )
        .unwrap();
        assert_eq!(doc.objects().len(), 1);

        // Undo boolean + lens + merged transaction
        hist.undo(&mut doc).unwrap();
        assert_eq!(doc.objects().len(), 2);
        hist.undo(&mut doc).unwrap();
        assert!(doc.object(ids[0]).unwrap().lens.is_none());
        hist.undo(&mut doc).unwrap();
        assert!(doc.objects().is_empty());
        assert!(hist.undo(&mut doc).is_err());

        // Redo everything back
        while hist.can_redo() {
            hist.redo(&mut doc).unwrap();
        }
        assert_eq!(doc.objects().len(), 1);

        // Export JSON log and deterministically replay onto a clean clone of initial
        let json = hist.export_log_json().unwrap();
        let parsed = CommandHistory::import_log_json(&json).unwrap();
        let mut replayed = initial;
        CommandHistory::replay_log(&mut replayed, &parsed).unwrap();

        assert_eq!(replayed.objects(), doc.objects());
        assert_eq!(replayed.pages(), doc.pages());
        assert_eq!(replayed.next_id(), doc.next_id());
    }

    #[test]
    fn record_macro_to_rhai_and_execute() {
        let mut doc = Document::new_a4("Macro Test");
        let mut hist = CommandHistory::default();

        let id = doc
            .add_object(sample_rect("Target", 5.0, 5.0), None)
            .unwrap();

        hist.start_macro_recording();
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![id],
                affine: Affine2D::translate(15.0, 25.0),
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::SetFill {
                ids: vec![id],
                fill: Fill::Solid(Color::from_hex("#ff3366").unwrap()),
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::AddGuide {
                orientation: GuideOrientation::Vertical(42.0),
                locked: false,
            },
        )
        .unwrap();

        let rhai_script = hist.stop_macro_to_rhai();
        assert!(rhai_script.contains("transform_objects"));
        assert!(rhai_script.contains("set_fill_hex"));
        assert!(rhai_script.contains("exec_command_json"));

        // Re-run recorded Rhai script on a fresh document with same object ID
        let mut doc2 = Document::new_a4("Macro Target");
        let mut obj = sample_rect("Target", 5.0, 5.0);
        obj.id = id;
        doc2.add_object(obj, None).unwrap();
        let mut hist2 = CommandHistory::default();

        let executed = run_rhai_macro(&mut doc2, &mut hist2, &rhai_script).unwrap();
        assert_eq!(executed, 3);
        let bb = doc2.object_bounds(id).unwrap();
        assert!((bb.min_x - 20.0).abs() < 1e-6);
        assert!((bb.min_y - 30.0).abs() < 1e-6);
        assert_eq!(doc2.guides().len(), 1);
        assert_eq!(
            doc2.object(id).unwrap().fill,
            Fill::Solid(Color::from_hex("#ff3366").unwrap())
        );

        // Sandbox blocks infinite loop scripts cleanly
        let err = run_rhai_macro(&mut doc2, &mut hist2, "loop {}");
        assert!(err.is_err());

        // Non-finite transform fails without mutating document
        let before_bb = doc2.object_bounds(id).unwrap();
        let bad = hist2.execute(
            &mut doc2,
            CommandOp::TransformObjects {
                ids: vec![id],
                affine: Affine2D::translate(f64::NAN, Point2D::ZERO.y),
            },
        );
        assert!(bad.is_err());
        assert_eq!(doc2.object_bounds(id).unwrap(), before_bb);
    }

    #[test]
    fn delta_history_powerclip_unlock_and_desktop_layer_roundtrip() {
        let mut doc = Document::new_a4("Delta & Desktop");
        doc.add_page("Page 2", 210.0, 297.0, 3.0);
        let mut hist = CommandHistory::default();

        // Populate 200 background objects to verify delta storage does not bloat per command
        let mut created_ids = Vec::with_capacity(200);
        for i in 0..200 {
            let mut r = sample_rect(&format!("R{i}"), 20.0, 20.0);
            r.transform = Affine2D::translate(f64::from(i), 10.0);
            let obj_id = doc.add_object(r, None).unwrap();
            created_ids.push(obj_id);
        }

        // Single object move -> compact ObjectDelta only tracks that single changed ID
        let moved_id = created_ids[0];
        let orig_x = doc.object_bounds(moved_id).unwrap().min_x;
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![moved_id],
                affine: Affine2D::translate(25.0, 0.0),
            },
        )
        .unwrap();
        assert_eq!(hist.undo_stack.len(), 1);
        assert_eq!(hist.undo_stack[0].delta.objects.len(), 1);
        assert!((doc.object_bounds(moved_id).unwrap().min_x - (orig_x + 25.0)).abs() < 1e-6);

        hist.undo(&mut doc).unwrap();
        assert!((doc.object_bounds(moved_id).unwrap().min_x - orig_x).abs() < 1e-6);

        // Move object to cross-page Desktop layer -> visible in page 1 spatial index too
        let desktop_obj = created_ids[1];
        hist.execute(&mut doc, CommandOp::MoveToDesktopLayer { id: desktop_obj })
            .unwrap();
        let desktop_lid = doc.desktop_layer_id().expect("desktop layer created");
        assert_eq!(doc.layer_of_object(desktop_obj), Some(desktop_lid));
        let idx_page1 = doc.build_spatial_index(1);
        assert!(!idx_page1.is_empty());
        doc.set_active_page_index(1).unwrap();
        assert_eq!(
            doc.hit_test(doc.object_bounds(desktop_obj).unwrap().center(), 1.0),
            Some(desktop_obj)
        );
        doc.set_active_page_index(0).unwrap();

        // PowerClip lock_contents = false keeps child anchored in world space when container moves
        let child_id = created_ids[2];
        let container_id = created_ids[3];
        let child_bb_before = doc.world_object_bounds(child_id).unwrap();
        let child_xf_before = doc.world_transform_of(child_id).unwrap();
        hist.execute(
            &mut doc,
            CommandOp::CreatePowerClip {
                container_id,
                content_ids: vec![child_id],
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::SetPowerClipLock {
                id: container_id,
                lock_contents: false,
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![container_id],
                affine: Affine2D::translate(40.0, 15.0),
            },
        )
        .unwrap();
        let child_bb_after = doc.world_object_bounds(child_id).unwrap();
        let child_xf_after = doc.world_transform_of(child_id).unwrap();
        assert!((child_bb_after.min_x - child_bb_before.min_x).abs() < 1e-4);
        assert!((child_bb_after.min_y - child_bb_before.min_y).abs() < 1e-4);
        assert!((child_xf_after.m[4] - child_xf_before.m[4]).abs() < 1e-4);
        assert!((child_xf_after.m[5] - child_xf_before.m[5]).abs() < 1e-4);
    }
}
