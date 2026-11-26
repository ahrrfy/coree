// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust multi-format clipboard payload encoding & decoding (`SVG`, `PDF`, `[MS-EMF]`,
//! and `PNG` / `RGBA8`), wired cleanly into `coree-app` OS clipboard backends.

use crate::display_list::{compose_world_persp, transform_world_path};
use crate::error::{IoError, IoResult};
use crate::pdf::{export_pdf_to_bytes, PdfColorSpace, PdfExportOptions};
use crate::pdf_import::import_pdf_bytes;
use crate::raster::{
    decode_raster_bytes_with_limits, export_raster_to_bytes, RasterExportOptions, RasterFormat,
};
use crate::svg::{export_svg_to_string, import_svg_str_with_limits};
use crate::{resolve_export_connector_endpoints, resolve_export_world_path, ExportSymCtx};
use coree_core::{
    Affine2D, BezierPath, Color, ColorMode, Document, Fill, NodePayload, ObjectId, OutlineStyle,
    Perspective3x3, Point2D, Rect2D, SceneObject, Subpath, SymbolDef,
};
use coree_render::effects_render::apply_geometry_effects;
use coree_render::{render_page_to_rgba, TextEngine, ViewMode};
use kurbo::{BezPath, PathEl, Shape as _};
use std::collections::{HashMap, HashSet};

pub const MIME_SVG: &str = "image/svg+xml";
pub const MIME_PDF: &str = "application/pdf";
pub const MIME_EMF: &str = "image/x-emf";
pub const MIME_PNG: &str = "image/png";
pub const MIME_TEXT_PLAIN: &str = "text/plain;charset=utf-8";

const CLIPBOARD_MAX_RASTER_DIM: u32 = 2048;
const MAX_EMF_RECORDS: u32 = 200_000;
const MAX_EMF_OBJECTS: usize = 100_000;
const MAX_EMF_VERTICES: usize = 2_000_000;
const HIMETRIC_PER_MM: f64 = 100.0;
const DEFAULT_CLIPBOARD_DPI: f64 = 96.0;

const EMR_HEADER: u32 = 1;
const EMR_POLYGON: u32 = 3;
const EMR_POLYLINE: u32 = 4;
const EMR_POLYBEZIERTO: u32 = 5;
const EMR_SETWINDOWEXTEX: u32 = 9;
const EMR_SETVIEWPORTEXTEX: u32 = 11;
const EMR_EOF: u32 = 14;
const EMR_SETMAPMODE: u32 = 17;
const EMR_MOVETOEX: u32 = 27;
const EMR_SELECTOBJECT: u32 = 37;
const EMR_CREATEPEN: u32 = 38;
const EMR_CREATEBRUSHINDIRECT: u32 = 39;
const EMR_DELETEOBJECT: u32 = 40;
const EMR_ELLIPSE: u32 = 42;
const EMR_RECTANGLE: u32 = 43;
const EMR_LINETO: u32 = 54;
const EMR_BEGINPATH: u32 = 59;
const EMR_ENDPATH: u32 = 60;
const EMR_CLOSEFIGURE: u32 = 61;
const EMR_FILLPATH: u32 = 62;
const EMR_STROKEANDFILLPATH: u32 = 63;
const EMR_STROKEPATH: u32 = 64;
const EMR_STRETCHDIBITS: u32 = 81;
const EMR_SIGNATURE_EMF: u32 = 0x464D_4520; // " EMF" in little-endian
const MM_ANISOTROPIC: u32 = 8;
const BS_SOLID: u32 = 0;
const BS_NULL: u32 = 1;
const PS_SOLID: u32 = 0;
const PS_NULL: u32 = 5;

/// Multi-format encoded clipboard bundle produced on Copy/Cut.
#[derive(Debug, Clone)]
pub struct ClipboardPayload {
    pub svg: String,
    pub pdf: Vec<u8>,
    pub emf: Vec<u8>,
    pub png: Vec<u8>,
    pub rgba_width: u32,
    pub rgba_height: u32,
    pub rgba: Vec<u8>,
    pub plain_text: Option<String>,
    pub content_hash: u64,
}

/// Decoded external clipboard format selected by priority (`SVG > PDF > PNG > EMF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardDecodedFormat {
    Svg,
    Pdf,
    Emf,
    Png,
}

impl ClipboardDecodedFormat {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Svg => "SVG",
            Self::Pdf => "PDF",
            Self::Emf => "EMF",
            Self::Png => "PNG",
        }
    }
}

/// Builds a self-contained single-page [`Document`] for clipboard export from `src_doc`
/// (or an already-captured `snapshot_doc`), attaching all `roots` onto page 0 layer 0 so
/// vector/raster exporters emit non-empty artwork.
pub fn build_clipboard_document(
    src_doc: &Document,
    roots: &[ObjectId],
    snapshot_doc: Option<&Document>,
) -> IoResult<Document> {
    let source = snapshot_doc.unwrap_or(src_doc);
    let (page_w, page_h) = src_doc.active_page().map_or((210.0, 297.0), |p| {
        (p.width_mm.max(1.0), p.height_mm.max(1.0))
    });

    let mut out = Document::new(
        "Clipboard",
        page_w,
        page_h,
        source.color_mode(),
        source.dpi(),
    );
    out.clone_shell_from(source);
    let Some(target_layer_id) = out.page(0).and_then(|p| p.layers.first()).map(|l| l.id) else {
        return Err(IoError::InvalidFormat {
            format: "clipboard".into(),
            message: "clipboard document missing base layer".into(),
        });
    };

    let mut visited_objs = HashSet::new();
    let mut attached_roots = Vec::new();
    for &root_id in roots {
        if !visited_objs.contains(&root_id) && source.object(root_id).is_some() {
            copy_clipboard_subtree(source, &mut out, root_id, 0, &mut visited_objs);
            attached_roots.push(root_id);
        }
    }

    if let Some(page) = out.page_mut(0) {
        if let Some(layer) = page.layers.iter_mut().find(|l| l.id == target_layer_id) {
            layer.objects.extend(attached_roots);
        }
    }

    Ok(out)
}

fn copy_clipboard_subtree(
    src: &Document,
    dst: &mut Document,
    id: ObjectId,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > 64 || !visited.insert(id) {
        return;
    }
    let Some(obj) = src.object(id) else {
        return;
    };
    dst.insert_snapshot_object_if_absent(obj.clone());

    let mut asset_refs = HashSet::new();
    obj.collect_asset_refs(&mut asset_refs);
    for aid in asset_refs {
        if let Some(asset) = src.asset(aid) {
            dst.insert_asset_if_absent(asset.clone());
        }
    }

    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children {
                copy_clipboard_subtree(src, dst, cid, depth + 1, visited);
            }
        }
        NodePayload::PowerClip { contents, .. } => {
            for &cid in contents {
                copy_clipboard_subtree(src, dst, cid, depth + 1, visited);
            }
        }
        NodePayload::Clone { master_id, .. } => {
            if master_id.0 != 0 {
                copy_clipboard_subtree(src, dst, *master_id, depth + 1, visited);
            }
        }
        NodePayload::TextOnPath(top) => {
            if top.path_id.0 != 0 {
                copy_clipboard_subtree(src, dst, top.path_id, depth + 1, visited);
            }
        }
        NodePayload::Connector(conn) => {
            if let Some(fid) = conn.from_obj {
                if fid.0 != 0 {
                    copy_clipboard_subtree(src, dst, fid, depth + 1, visited);
                }
            }
            if let Some(tid) = conn.to_obj {
                if tid.0 != 0 {
                    copy_clipboard_subtree(src, dst, tid, depth + 1, visited);
                }
            }
        }
        NodePayload::SymbolInstance(sym_id) if dst.symbol(*sym_id).is_none() => {
            if let Some(sym) = src.symbol(*sym_id) {
                dst.insert_symbol(sym.clone());
                let mut sym_assets = HashSet::new();
                for sub in &sym.objects {
                    sub.collect_asset_refs(&mut sym_assets);
                }
                for aid in sym_assets {
                    if let Some(asset) = src.asset(aid) {
                        dst.insert_asset_if_absent(asset.clone());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Encodes `page_index` of `doc` as UTF-8 SVG XML (`image/svg+xml`).
pub fn encode_clipboard_svg(doc: &Document, page_index: usize) -> IoResult<String> {
    export_svg_to_string(doc, page_index)
}

/// Encodes `page_index` of `doc` as a print-accurate PDF byte stream (`application/pdf`).
pub fn encode_clipboard_pdf(doc: &Document, page_index: usize) -> IoResult<Vec<u8>> {
    let color_space = match doc.color_mode() {
        ColorMode::Cmyk => PdfColorSpace::DeviceCmyk,
        ColorMode::Rgb => PdfColorSpace::DeviceRgb,
    };
    let opts = PdfExportOptions {
        color_space,
        include_bleed: false,
        compress_streams: true,
        pages: Some(vec![page_index]),
        ..Default::default()
    };
    export_pdf_to_bytes(doc, &opts)
}

/// Encodes `page_index` of `doc` as a valid `[MS-EMF]` Enhanced Metafile (`image/x-emf`).
pub fn encode_clipboard_emf(doc: &Document, page_index: usize) -> IoResult<Vec<u8>> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
    })?;
    let page_w_mm = if page.width_mm.is_finite() && page.width_mm > 0.0 {
        page.width_mm
    } else {
        210.0
    };
    let page_h_mm = if page.height_mm.is_finite() && page.height_mm > 0.0 {
        page.height_mm
    } else {
        297.0
    };

    let w_himetric = (page_w_mm * HIMETRIC_PER_MM)
        .round()
        .clamp(100.0, 10_000_000.0) as i32;
    let h_himetric = (page_h_mm * HIMETRIC_PER_MM)
        .round()
        .clamp(100.0, 10_000_000.0) as i32;
    let w_dev = (page_w_mm * DEFAULT_CLIPBOARD_DPI / 25.4)
        .round()
        .clamp(1.0, 100_000.0) as i32;
    let h_dev = (page_h_mm * DEFAULT_CLIPBOARD_DPI / 25.4)
        .round()
        .clamp(1.0, 100_000.0) as i32;

    let mut out = Vec::with_capacity(4096);
    out.resize(88, 0u8);

    let mut n_records: u32 = 1;
    let mut bounds_min_x = i32::MAX;
    let mut bounds_min_y = i32::MAX;
    let mut bounds_max_x = i32::MIN;
    let mut bounds_max_y = i32::MIN;
    let mut max_handle: u32 = 0;

    emit_emr_u32_1(&mut out, EMR_SETMAPMODE, MM_ANISOTROPIC, &mut n_records);
    emit_emr_i32_2(
        &mut out,
        EMR_SETWINDOWEXTEX,
        w_himetric,
        h_himetric,
        &mut n_records,
    );
    emit_emr_i32_2(&mut out, EMR_SETVIEWPORTEXTEX, w_dev, h_dev, &mut n_records);

    let mut text_engine = TextEngine::new();
    let mut visited = HashSet::new();
    for layer in &page.layers {
        if !layer.visible {
            continue;
        }
        for &oid in &layer.objects {
            if let Some(obj) = doc.object(oid) {
                emit_emf_object(
                    doc,
                    obj,
                    Affine2D::IDENTITY,
                    None,
                    None,
                    0,
                    &mut visited,
                    &mut text_engine,
                    &mut out,
                    &mut n_records,
                    &mut max_handle,
                    &mut bounds_min_x,
                    &mut bounds_min_y,
                    &mut bounds_max_x,
                    &mut bounds_max_y,
                );
            }
        }
    }

    // EMR_EOF (type 14, size 20): nPalEntries=0, offPalEntries=16, nSizeLast=20
    out.extend_from_slice(&EMR_EOF.to_le_bytes());
    out.extend_from_slice(&20u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&20u32.to_le_bytes());
    n_records += 1;

    let (bx0, by0, bx1, by1) = if bounds_min_x <= bounds_max_x && bounds_min_y <= bounds_max_y {
        let to_dev_x = |hx: i32| -> i32 {
            ((f64::from(hx) * f64::from(w_dev)) / f64::from(w_himetric.max(1))).round() as i32
        };
        let to_dev_y = |hy: i32| -> i32 {
            ((f64::from(hy) * f64::from(h_dev)) / f64::from(h_himetric.max(1))).round() as i32
        };
        (
            to_dev_x(bounds_min_x),
            to_dev_y(bounds_min_y),
            to_dev_x(bounds_max_x),
            to_dev_y(bounds_max_y),
        )
    } else {
        (0, 0, w_dev, h_dev)
    };

    let n_bytes = u32::try_from(out.len()).map_err(|_| IoError::InvalidFormat {
        format: "emf".into(),
        message: "EMF stream exceeds u32::MAX bytes".into(),
    })?;
    let n_handles = u16::try_from(max_handle.saturating_add(1).max(1)).unwrap_or(u16::MAX);
    let w_mm_i32 = page_w_mm.round().clamp(1.0, 100_000.0) as i32;
    let h_mm_i32 = page_h_mm.round().clamp(1.0, 100_000.0) as i32;

    patch_emr_header(
        &mut out[0..88],
        [bx0, by0, bx1, by1],
        [0, 0, w_himetric, h_himetric],
        n_bytes,
        n_records,
        n_handles,
        (w_dev, h_dev),
        (w_mm_i32, h_mm_i32),
    );

    Ok(out)
}

fn build_emf_symbol_maps(sym: &SymbolDef) -> (HashMap<ObjectId, &SceneObject>, HashSet<ObjectId>) {
    let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
    let mut referenced: HashSet<ObjectId> = HashSet::new();
    for sub in &sym.objects {
        if sub.id.0 != 0 {
            child_map.insert(sub.id, sub);
        }
        match &sub.payload {
            NodePayload::Group(c) => referenced.extend(c.iter().copied()),
            NodePayload::PowerClip { contents, .. } => referenced.extend(contents.iter().copied()),
            _ => {}
        }
    }
    (child_map, referenced)
}

#[inline]
fn warp_emf_world_path(
    local_bp: BezierPath,
    obj: &SceneObject,
    world_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
) -> BezierPath {
    let bp = if let Some(persp) = obj.perspective {
        persp.transform_bezier_path(&local_bp)
    } else {
        local_bp
    };
    let deformed = apply_geometry_effects(&bp, &obj.effects);
    transform_world_path(&deformed, obj.transform, world_xf, parent_persp)
}

#[allow(clippy::too_many_arguments)]
fn emit_emf_object(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<ExportSymCtx<'_>>,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
    text_engine: &mut TextEngine,
    out: &mut Vec<u8>,
    n_records: &mut u32,
    max_handle: &mut u32,
    min_x: &mut i32,
    min_y: &mut i32,
    max_x: &mut i32,
    max_y: &mut i32,
) {
    if depth > 64 || !obj.visible || (obj.id.0 != 0 && !visited.insert(obj.id)) {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    let child_persp = compose_world_persp(parent_persp, obj.transform, world_xf, obj.perspective);

    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children {
                let child_opt = sym_ctx
                    .and_then(|(m, _, _)| m.get(&cid).copied())
                    .or_else(|| doc.object(cid));
                if let Some(child) = child_opt {
                    emit_emf_object(
                        doc,
                        child,
                        world_xf,
                        child_persp,
                        sym_ctx,
                        depth + 1,
                        visited,
                        text_engine,
                        out,
                        n_records,
                        max_handle,
                        min_x,
                        min_y,
                        max_x,
                        max_y,
                    );
                }
            }
        }
        NodePayload::PowerClip {
            container,
            contents,
            ..
        } => {
            let warped_container =
                warp_emf_world_path((**container).clone(), obj, world_xf, parent_persp);
            emit_emf_bezier_shape(
                &warped_container,
                Affine2D::IDENTITY,
                &obj.fill,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
            for &cid in contents {
                let child_opt = sym_ctx
                    .and_then(|(m, _, _)| m.get(&cid).copied())
                    .or_else(|| doc.object(cid));
                if let Some(child) = child_opt {
                    emit_emf_object(
                        doc,
                        child,
                        world_xf,
                        child_persp,
                        sym_ctx,
                        depth + 1,
                        visited,
                        text_engine,
                        out,
                        n_records,
                        max_handle,
                        min_x,
                        min_y,
                        max_x,
                        max_y,
                    );
                }
            }
        }
        NodePayload::Path(bp) => {
            let warped = warp_emf_world_path(bp.clone(), obj, world_xf, parent_persp);
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &obj.fill,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::Shape(shape) => {
            let warped = warp_emf_world_path(shape.to_bezier_path(), obj, world_xf, parent_persp);
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &obj.fill,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::BSpline(bs) => {
            let warped = warp_emf_world_path(bs.to_bezier_path(), obj, world_xf, parent_persp);
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &obj.fill,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::ArtisticMedia { spine, style } => {
            let warped = warp_emf_world_path(
                coree_core::ArtisticMedia::expand_stroke(spine, style),
                obj,
                world_xf,
                parent_persp,
            );
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &obj.fill,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::Clone { .. } => {
            if let Some((p, f, o)) = crate::display_list::resolve_display_list_clone(
                doc,
                obj,
                sym_ctx.map(|(m, _, _)| m),
            ) {
                let mut resolved = obj.clone();
                resolved.id = ObjectId(0);
                resolved.payload = p;
                resolved.fill = f;
                resolved.outline = o;
                emit_emf_object(
                    doc,
                    &resolved,
                    parent_xf,
                    parent_persp,
                    sym_ctx,
                    depth + 1,
                    visited,
                    text_engine,
                    out,
                    n_records,
                    max_handle,
                    min_x,
                    min_y,
                    max_x,
                    max_y,
                );
            }
        }
        NodePayload::ArtisticText(text) => {
            let warped = warp_emf_world_path(
                text_engine.artistic_text_to_curves(text),
                obj,
                world_xf,
                parent_persp,
            );
            let fill = text
                .runs
                .first()
                .map(|r| Fill::Solid(r.style.fill.clone()))
                .unwrap_or_else(|| obj.fill.clone());
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &fill,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::ParagraphText(para) => {
            let warped = warp_emf_world_path(
                text_engine.paragraph_text_to_curves(para),
                obj,
                world_xf,
                parent_persp,
            );
            let fill = para
                .runs
                .first()
                .map(|r| Fill::Solid(r.style.fill.clone()))
                .unwrap_or_else(|| obj.fill.clone());
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &fill,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::TextOnPath(top) => {
            if let Some(host_bp) = resolve_export_world_path(doc, top.path_id, sym_ctx) {
                let bp = text_engine.text_on_path_to_curves(top, &host_bp);
                let fill = top
                    .text
                    .runs
                    .first()
                    .map(|r| Fill::Solid(r.style.fill.clone()))
                    .unwrap_or_else(|| obj.fill.clone());
                emit_emf_bezier_shape(
                    &bp,
                    Affine2D::IDENTITY,
                    &fill,
                    &obj.outline,
                    out,
                    n_records,
                    max_handle,
                    min_x,
                    min_y,
                    max_x,
                    max_y,
                );
            }
        }
        NodePayload::Raster(raster) => {
            if let Some(asset) = doc.asset(raster.asset_id) {
                let active_filters = raster.active_filters();
                let filtered = (!active_filters.is_empty())
                    .then(|| coree_render::apply_raster_filters(asset, &active_filters));
                let active_asset = filtered.as_ref().unwrap_or(asset);
                let p0 = world_xf.transform_point(Point2D::new(0.0, 0.0));
                let p1 = world_xf.transform_point(Point2D::new(
                    raster.width_mm.max(0.1),
                    raster.height_mm.max(0.1),
                ));
                let x0 = mm_to_himetric(p0.x.min(p1.x));
                let y0 = mm_to_himetric(p0.y.min(p1.y));
                let x1 = mm_to_himetric(p0.x.max(p1.x));
                let y1 = mm_to_himetric(p0.y.max(p1.y));
                *min_x = (*min_x).min(x0);
                *min_y = (*min_y).min(y0);
                *max_x = (*max_x).max(x1);
                *max_y = (*max_y).max(y1);
                emit_emf_stretch_dibits(
                    active_asset.width,
                    active_asset.height,
                    &active_asset.to_rgba8_vec(),
                    obj.opacity,
                    [x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)],
                    out,
                    n_records,
                );
            } else {
                let bp = BezierPath::from_rect(Rect2D::from_xywh(
                    0.0,
                    0.0,
                    raster.width_mm.max(1.0),
                    raster.height_mm.max(1.0),
                ));
                emit_emf_bezier_shape(
                    &bp,
                    world_xf,
                    &obj.fill,
                    &obj.outline,
                    out,
                    n_records,
                    max_handle,
                    min_x,
                    min_y,
                    max_x,
                    max_y,
                );
            }
        }
        NodePayload::Connector(conn) => {
            let (a, b) = resolve_export_connector_endpoints(doc, conn, &world_xf, sym_ctx);
            let bp = conn.route_path(a, b);
            emit_emf_bezier_shape(
                &bp,
                Affine2D::IDENTITY,
                &Fill::None,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                let (child_map, referenced) = build_emf_symbol_maps(sym);
                let mut sym_visited = HashSet::new();
                for sym_obj in &sym.objects {
                    if sym_obj.id.0 != 0 && referenced.contains(&sym_obj.id) {
                        continue;
                    }
                    emit_emf_object(
                        doc,
                        sym_obj,
                        world_xf,
                        child_persp,
                        Some((&child_map, &sym.objects, world_xf)),
                        depth + 1,
                        &mut sym_visited,
                        text_engine,
                        out,
                        n_records,
                        max_handle,
                        min_x,
                        min_y,
                        max_x,
                        max_y,
                    );
                }
            }
        }
        NodePayload::Table(table) => {
            let total_w: f64 = table.col_widths_mm.iter().copied().sum::<f64>().max(1.0);
            let total_h: f64 = table.row_heights_mm.iter().copied().sum::<f64>().max(1.0);
            let mut bp = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, total_w, total_h));
            let mut x_acc = 0.0;
            for w in table
                .col_widths_mm
                .iter()
                .take(table.cols.saturating_sub(1))
            {
                x_acc += *w;
                bp.subpaths.push(Subpath::from_points(
                    [Point2D::new(x_acc, 0.0), Point2D::new(x_acc, total_h)],
                    false,
                ));
            }
            let mut y_acc = 0.0;
            for h in table
                .row_heights_mm
                .iter()
                .take(table.rows.saturating_sub(1))
            {
                y_acc += *h;
                bp.subpaths.push(Subpath::from_points(
                    [Point2D::new(0.0, y_acc), Point2D::new(total_w, y_acc)],
                    false,
                ));
            }
            let warped = warp_emf_world_path(bp, obj, world_xf, parent_persp);
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &obj.fill,
                &table.border_style,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
        NodePayload::Dimension(dim) => {
            let warped = warp_emf_world_path(
                BezierPath::from_line(dim.p1, dim.p2),
                obj,
                world_xf,
                parent_persp,
            );
            emit_emf_bezier_shape(
                &warped,
                Affine2D::IDENTITY,
                &Fill::None,
                &obj.outline,
                out,
                n_records,
                max_handle,
                min_x,
                min_y,
                max_x,
                max_y,
            );
        }
    }

    if obj.id.0 != 0 {
        visited.remove(&obj.id);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_emf_bezier_shape(
    bp: &BezierPath,
    xf: Affine2D,
    fill: &Fill,
    outline: &OutlineStyle,
    out: &mut Vec<u8>,
    n_records: &mut u32,
    max_handle: &mut u32,
    min_x: &mut i32,
    min_y: &mut i32,
    max_x: &mut i32,
    max_y: &mut i32,
) {
    let mut transformed = bp.clone();
    transformed.transform(&xf);
    let kurbo_path = transformed.to_kurbo();
    let els: Vec<PathEl> = kurbo_path.elements().to_vec();
    if els.is_empty() {
        return;
    }

    let fill_rgb = primary_fill_colorref(fill);
    let has_fill = fill_rgb.is_some();
    let has_stroke = outline.is_visible() && outline.width_mm > 0.0;
    if !has_fill && !has_stroke {
        return;
    }

    let brush_handle = 1u32;
    let pen_handle = 2u32;
    *max_handle = (*max_handle).max(pen_handle);

    // EMR_CREATEBRUSHINDIRECT (type 39, size 24)
    out.extend_from_slice(&EMR_CREATEBRUSHINDIRECT.to_le_bytes());
    out.extend_from_slice(&24u32.to_le_bytes());
    out.extend_from_slice(&brush_handle.to_le_bytes());
    out.extend_from_slice(&(if has_fill { BS_SOLID } else { BS_NULL }).to_le_bytes());
    out.extend_from_slice(&fill_rgb.unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    *n_records += 1;
    emit_emr_u32_1(out, EMR_SELECTOBJECT, brush_handle, n_records);

    // EMR_CREATEPEN (type 38, size 28)
    let pen_width_himetric = if has_stroke {
        (outline.width_mm * HIMETRIC_PER_MM)
            .round()
            .clamp(1.0, 100_000.0) as i32
    } else {
        0
    };
    let pen_colorref = color_to_colorref(&outline.color);
    out.extend_from_slice(&EMR_CREATEPEN.to_le_bytes());
    out.extend_from_slice(&28u32.to_le_bytes());
    out.extend_from_slice(&pen_handle.to_le_bytes());
    out.extend_from_slice(&(if has_stroke { PS_SOLID } else { PS_NULL }).to_le_bytes());
    out.extend_from_slice(&pen_width_himetric.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&pen_colorref.to_le_bytes());
    *n_records += 1;
    emit_emr_u32_1(out, EMR_SELECTOBJECT, pen_handle, n_records);

    emit_emr_empty(out, EMR_BEGINPATH, n_records);

    let mut path_min_x = i32::MAX;
    let mut path_min_y = i32::MAX;
    let mut path_max_x = i32::MIN;
    let mut path_max_y = i32::MIN;
    let mut record_pt = |x: i32, y: i32| {
        path_min_x = path_min_x.min(x);
        path_min_y = path_min_y.min(y);
        path_max_x = path_max_x.max(x);
        path_max_y = path_max_y.max(y);
        *min_x = (*min_x).min(x);
        *min_y = (*min_y).min(y);
        *max_x = (*max_x).max(x);
        *max_y = (*max_y).max(y);
    };

    let mut cur = Point2D::ZERO;
    for el in els {
        match el {
            PathEl::MoveTo(p) => {
                cur = Point2D::new(p.x, p.y);
                let x = mm_to_himetric(p.x);
                let y = mm_to_himetric(p.y);
                record_pt(x, y);
                emit_emr_i32_2(out, EMR_MOVETOEX, x, y, n_records);
            }
            PathEl::LineTo(p) => {
                cur = Point2D::new(p.x, p.y);
                let x = mm_to_himetric(p.x);
                let y = mm_to_himetric(p.y);
                record_pt(x, y);
                emit_emr_i32_2(out, EMR_LINETO, x, y, n_records);
            }
            PathEl::QuadTo(c, p) => {
                let c1 = Point2D::new(
                    cur.x + (2.0 / 3.0) * (c.x - cur.x),
                    cur.y + (2.0 / 3.0) * (c.y - cur.y),
                );
                let c2 = Point2D::new(
                    p.x + (2.0 / 3.0) * (c.x - p.x),
                    p.y + (2.0 / 3.0) * (c.y - p.y),
                );
                cur = Point2D::new(p.x, p.y);
                let pts = [
                    (mm_to_himetric(c1.x), mm_to_himetric(c1.y)),
                    (mm_to_himetric(c2.x), mm_to_himetric(c2.y)),
                    (mm_to_himetric(p.x), mm_to_himetric(p.y)),
                ];
                for &(px, py) in &pts {
                    record_pt(px, py);
                }
                emit_emr_polybezierto(out, &pts, n_records);
            }
            PathEl::CurveTo(c1, c2, p) => {
                cur = Point2D::new(p.x, p.y);
                let pts = [
                    (mm_to_himetric(c1.x), mm_to_himetric(c1.y)),
                    (mm_to_himetric(c2.x), mm_to_himetric(c2.y)),
                    (mm_to_himetric(p.x), mm_to_himetric(p.y)),
                ];
                for &(px, py) in &pts {
                    record_pt(px, py);
                }
                emit_emr_polybezierto(out, &pts, n_records);
            }
            PathEl::ClosePath => {
                emit_emr_empty(out, EMR_CLOSEFIGURE, n_records);
            }
        }
    }

    emit_emr_empty(out, EMR_ENDPATH, n_records);
    let bbox = if path_min_x <= path_max_x && path_min_y <= path_max_y {
        [path_min_x, path_min_y, path_max_x, path_max_y]
    } else {
        [0, 0, 0, 0]
    };
    let paint_rec = match (has_fill, has_stroke) {
        (true, true) => EMR_STROKEANDFILLPATH,
        (true, false) => EMR_FILLPATH,
        (false, true) => EMR_STROKEPATH,
        (false, false) => EMR_STROKEPATH,
    };
    emit_emr_rect(out, paint_rec, bbox, n_records);

    emit_emr_u32_1(out, EMR_DELETEOBJECT, brush_handle, n_records);
    emit_emr_u32_1(out, EMR_DELETEOBJECT, pen_handle, n_records);
}

fn emit_emf_stretch_dibits(
    src_w: u32,
    src_h: u32,
    rgba: &[u8],
    opacity: f32,
    dest_xywh_himetric: [i32; 4],
    out: &mut Vec<u8>,
    n_records: &mut u32,
) {
    if src_w == 0 || src_h == 0 || rgba.len() < (src_w as usize) * (src_h as usize) * 4 {
        return;
    }
    let max_dim = 256u32;
    let scale = (src_w.max(src_h) as f64 / f64::from(max_dim)).max(1.0);
    let dst_w = ((src_w as f64 / scale).round() as u32).clamp(1, max_dim);
    let dst_h = ((src_h as f64 / scale).round() as u32).clamp(1, max_dim);
    let stride = (dst_w as usize * 3 + 3) & !3;
    let img_bytes = stride * dst_h as usize;
    let rsize = 80u32 + 40u32 + img_bytes as u32;
    let [xd, yd, cwd, chd] = dest_xywh_himetric;

    out.extend_from_slice(&EMR_STRETCHDIBITS.to_le_bytes());
    out.extend_from_slice(&rsize.to_le_bytes());
    for v in [xd, yd, xd.saturating_add(cwd), yd.saturating_add(chd)] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&xd.to_le_bytes());
    out.extend_from_slice(&yd.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes()); // xSrc
    out.extend_from_slice(&0i32.to_le_bytes()); // ySrc
    out.extend_from_slice(&(dst_w as i32).to_le_bytes());
    out.extend_from_slice(&(dst_h as i32).to_le_bytes());
    out.extend_from_slice(&80u32.to_le_bytes()); // offBmiSrc
    out.extend_from_slice(&40u32.to_le_bytes()); // cbBmiSrc
    out.extend_from_slice(&120u32.to_le_bytes()); // offBitsSrc
    out.extend_from_slice(&(img_bytes as u32).to_le_bytes()); // cbBitsSrc
    out.extend_from_slice(&0u32.to_le_bytes()); // iUsageSrc = DIB_RGB_COLORS
    out.extend_from_slice(&0x00CC_0020u32.to_le_bytes()); // SRCCOPY
    out.extend_from_slice(&cwd.to_le_bytes());
    out.extend_from_slice(&chd.to_le_bytes());

    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(dst_w as i32).to_le_bytes());
    out.extend_from_slice(&(dst_h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(img_bytes as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    let op = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let mut row = vec![0u8; stride];
    for dy in (0..dst_h).rev() {
        row.fill(0);
        let sy = ((u64::from(dy) * u64::from(src_h)) / u64::from(dst_h)).min(u64::from(src_h - 1))
            as usize;
        for dx in 0..dst_w {
            let sx = ((u64::from(dx) * u64::from(src_w)) / u64::from(dst_w))
                .min(u64::from(src_w - 1)) as usize;
            let idx = (sy * src_w as usize + sx) * 4;
            let a = (f32::from(rgba[idx + 3]) / 255.0) * op;
            let blend = |ch: u8| -> u8 {
                (f32::from(ch) * a + 255.0 * (1.0 - a))
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            let out_off = dx as usize * 3;
            row[out_off] = blend(rgba[idx + 2]);
            row[out_off + 1] = blend(rgba[idx + 1]);
            row[out_off + 2] = blend(rgba[idx]);
        }
        out.extend_from_slice(&row);
    }
    *n_records += 1;
}

fn mm_to_himetric(mm: f64) -> i32 {
    if !mm.is_finite() {
        return 0;
    }
    (mm * HIMETRIC_PER_MM)
        .round()
        .clamp(-100_000_000.0, 100_000_000.0) as i32
}

fn himetric_to_mm(v: i32) -> f64 {
    f64::from(v) / HIMETRIC_PER_MM
}

fn color_to_colorref(c: &Color) -> u32 {
    let [r, g, b, _a] = c.to_rgba8();
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}

fn colorref_to_color(cr: u32) -> Color {
    let r = (cr & 0xFF) as u8;
    let g = ((cr >> 8) & 0xFF) as u8;
    let b = ((cr >> 16) & 0xFF) as u8;
    Color::rgba8(r, g, b, 255)
}

fn primary_fill_colorref(fill: &Fill) -> Option<u32> {
    match fill {
        Fill::None => None,
        Fill::Solid(c) => {
            if c.alpha() <= 0.0 {
                None
            } else {
                Some(color_to_colorref(c))
            }
        }
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => stops.first().map(|s| color_to_colorref(&s.color)),
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => {
            Some(color_to_colorref(fg))
        }
        Fill::MeshFill { nodes, .. } => nodes.first().map(|n| color_to_colorref(&n.color)),
        Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {
            Some(color_to_colorref(&Color::rgba8(180, 180, 180, 255)))
        }
    }
}

fn emit_emr_empty(out: &mut Vec<u8>, rtype: u32, n_records: &mut u32) {
    out.extend_from_slice(&rtype.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    *n_records += 1;
}

fn emit_emr_u32_1(out: &mut Vec<u8>, rtype: u32, val: u32, n_records: &mut u32) {
    out.extend_from_slice(&rtype.to_le_bytes());
    out.extend_from_slice(&12u32.to_le_bytes());
    out.extend_from_slice(&val.to_le_bytes());
    *n_records += 1;
}

fn emit_emr_i32_2(out: &mut Vec<u8>, rtype: u32, a: i32, b: i32, n_records: &mut u32) {
    out.extend_from_slice(&rtype.to_le_bytes());
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&a.to_le_bytes());
    out.extend_from_slice(&b.to_le_bytes());
    *n_records += 1;
}

fn emit_emr_rect(out: &mut Vec<u8>, rtype: u32, rect: [i32; 4], n_records: &mut u32) {
    out.extend_from_slice(&rtype.to_le_bytes());
    out.extend_from_slice(&24u32.to_le_bytes());
    for v in rect {
        out.extend_from_slice(&v.to_le_bytes());
    }
    *n_records += 1;
}

fn emit_emr_polybezierto(out: &mut Vec<u8>, pts: &[(i32, i32)], n_records: &mut u32) {
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for &(x, y) in pts {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    let count = pts.len() as u32;
    let rsize = 28u32 + count * 8u32;
    out.extend_from_slice(&EMR_POLYBEZIERTO.to_le_bytes());
    out.extend_from_slice(&rsize.to_le_bytes());
    for v in [min_x, min_y, max_x, max_y] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&count.to_le_bytes());
    for &(x, y) in pts {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
    }
    *n_records += 1;
}

#[allow(clippy::too_many_arguments)]
fn patch_emr_header(
    hdr: &mut [u8],
    bounds: [i32; 4],
    frame: [i32; 4],
    n_bytes: u32,
    n_records: u32,
    n_handles: u16,
    szl_device: (i32, i32),
    szl_mm: (i32, i32),
) {
    hdr[0..4].copy_from_slice(&EMR_HEADER.to_le_bytes());
    hdr[4..8].copy_from_slice(&88u32.to_le_bytes());
    for (idx, v) in bounds.into_iter().enumerate() {
        let off = 8 + idx * 4;
        hdr[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    for (idx, v) in frame.into_iter().enumerate() {
        let off = 24 + idx * 4;
        hdr[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    hdr[40..44].copy_from_slice(&EMR_SIGNATURE_EMF.to_le_bytes());
    hdr[44..48].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    hdr[48..52].copy_from_slice(&n_bytes.to_le_bytes());
    hdr[52..56].copy_from_slice(&n_records.to_le_bytes());
    hdr[56..58].copy_from_slice(&n_handles.to_le_bytes());
    hdr[58..60].copy_from_slice(&0u16.to_le_bytes());
    hdr[60..64].copy_from_slice(&0u32.to_le_bytes());
    hdr[64..68].copy_from_slice(&0u32.to_le_bytes());
    hdr[68..72].copy_from_slice(&0u32.to_le_bytes());
    hdr[72..76].copy_from_slice(&szl_device.0.to_le_bytes());
    hdr[76..80].copy_from_slice(&szl_device.1.to_le_bytes());
    hdr[80..84].copy_from_slice(&szl_mm.0.to_le_bytes());
    hdr[84..88].copy_from_slice(&szl_mm.1.to_le_bytes());
}

/// Renders `page_index` of `doc` to bounded PNG bytes + raw RGBA8 pixels (`image/png`).
pub fn encode_clipboard_png_rgba(
    doc: &Document,
    page_index: usize,
    requested_dpi: f64,
) -> IoResult<(Vec<u8>, u32, u32, Vec<u8>)> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
    })?;
    let max_side_mm = page.width_mm.max(page.height_mm).max(1.0);
    let max_dpi = (f64::from(CLIPBOARD_MAX_RASTER_DIM) * 25.4) / max_side_mm;
    let safe_dpi = requested_dpi.clamp(12.0, max_dpi.max(12.0));
    let rendered =
        render_page_to_rgba(doc, page_index, safe_dpi, ViewMode::Enhanced).map_err(|e| {
            IoError::InvalidFormat {
                format: "png".into(),
                message: format!("clipboard raster render failed: {e}"),
            }
        })?;
    let png = export_raster_to_bytes(
        rendered.width,
        rendered.height,
        &rendered.rgba,
        &RasterExportOptions::new(RasterFormat::Png, safe_dpi as f32),
    )?;
    Ok((png, rendered.width, rendered.height, rendered.rgba))
}

/// Encodes all multi-format clipboard representations (`SVG`, `PDF`, `[MS-EMF]`, `PNG`+`RGBA8`)
/// for `page_index` of `doc` into a single [`ClipboardPayload`].
pub fn encode_clipboard_payload(doc: &Document, page_index: usize) -> IoResult<ClipboardPayload> {
    let svg = encode_clipboard_svg(doc, page_index)?;
    let pdf = encode_clipboard_pdf(doc, page_index)?;
    let emf = encode_clipboard_emf(doc, page_index)?;
    let (png, rgba_width, rgba_height, rgba) =
        encode_clipboard_png_rgba(doc, page_index, DEFAULT_CLIPBOARD_DPI)?;
    let plain_text = extract_plain_text(doc);
    let content_hash = fnv1a_64(&[svg.as_bytes(), &pdf, &emf, &png]);
    Ok(ClipboardPayload {
        svg,
        pdf,
        emf,
        png,
        rgba_width,
        rgba_height,
        rgba,
        plain_text,
        content_hash,
    })
}

fn extract_plain_text(doc: &Document) -> Option<String> {
    let mut lines = Vec::new();
    for obj in doc.objects().values() {
        match &obj.payload {
            NodePayload::ArtisticText(t) => {
                let s: String = t.runs.iter().map(|r| r.text.as_str()).collect();
                if !s.trim().is_empty() {
                    lines.push(s);
                }
            }
            NodePayload::ParagraphText(p) => {
                let s: String = p.runs.iter().map(|r| r.text.as_str()).collect();
                if !s.trim().is_empty() {
                    lines.push(s);
                }
            }
            NodePayload::TextOnPath(top) => {
                let s: String = top.text.runs.iter().map(|r| r.text.as_str()).collect();
                if !s.trim().is_empty() {
                    lines.push(s);
                }
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn fnv1a_64(slices: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in slices {
        for &b in *s {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Decodes UTF-8 SVG clipboard bytes (`image/svg+xml`) into a [`Document`].
pub fn decode_clipboard_svg(bytes: &[u8]) -> IoResult<Document> {
    let text = std::str::from_utf8(bytes).map_err(|e| IoError::InvalidFormat {
        format: "svg".into(),
        message: format!("clipboard SVG is not valid UTF-8: {e}"),
    })?;
    import_svg_str_with_limits(text, &Default::default())
}

/// Decodes PDF clipboard bytes (`application/pdf`) into a [`Document`].
pub fn decode_clipboard_pdf(bytes: &[u8]) -> IoResult<Document> {
    import_pdf_bytes(bytes, "Clipboard PDF")
}

/// Decodes PNG clipboard bytes (`image/png`) into a [`Document`] with a single raster object.
pub fn decode_clipboard_png(bytes: &[u8]) -> IoResult<Document> {
    let asset = decode_raster_bytes_with_limits(bytes, &Default::default())?;
    let dpi = f64::from(asset.source_dpi).clamp(12.0, 2400.0);
    let w_mm = (f64::from(asset.width) * 25.4 / dpi).max(1.0);
    let h_mm = (f64::from(asset.height) * 25.4 / dpi).max(1.0);
    let mut doc = Document::new(
        "Clipboard PNG",
        w_mm,
        h_mm,
        ColorMode::Rgb,
        asset.source_dpi,
    );
    let asset_id = doc.register_asset(asset);
    let obj_id = doc.alloc_object_id();
    let obj = SceneObject::new(
        obj_id,
        "Clipboard Image",
        NodePayload::Raster(coree_core::RasterLayerData {
            asset_id,
            width_mm: w_mm,
            height_mm: h_mm,
            filters: Vec::new(),
            filter_enabled: Vec::new(),
            crop_rect: None,
        }),
    );
    let layer_id = doc.page(0).and_then(|p| p.layers.first()).map(|l| l.id);
    doc.add_object(obj, layer_id)?;
    Ok(doc)
}

/// Wraps raw `RGBA8` pixel data (e.g. from `arboard::ImageData`) into a [`Document`].
pub fn decode_clipboard_rgba(width: u32, height: u32, rgba: &[u8], dpi: f64) -> IoResult<Document> {
    let png = export_raster_to_bytes(
        width,
        height,
        rgba,
        &RasterExportOptions::new(RasterFormat::Png, dpi as f32),
    )?;
    decode_clipboard_png(&png)
}

/// Decodes a `[MS-EMF]` byte stream (`image/x-emf`) into a [`Document`] containing vector paths.
pub fn decode_clipboard_emf(bytes: &[u8]) -> IoResult<Document> {
    if bytes.len() < 88 {
        return Err(IoError::InvalidFormat {
            format: "emf".into(),
            message: "EMF clipboard payload shorter than 88-byte EMR_HEADER".into(),
        });
    }
    let rec_type = read_u32_le(bytes, 0);
    let rec_size = read_u32_le(bytes, 4);
    let sig = &bytes[40..44];
    if rec_type != EMR_HEADER || rec_size < 88 || sig != b" EMF" {
        return Err(IoError::InvalidFormat {
            format: "emf".into(),
            message: format!("invalid EMR_HEADER (type={rec_type}, size={rec_size})"),
        });
    }
    let n_bytes = read_u32_le(bytes, 48) as usize;
    let n_records = read_u32_le(bytes, 52);
    if n_bytes < rec_size as usize || n_bytes > bytes.len() || n_records < 2 {
        return Err(IoError::InvalidFormat {
            format: "emf".into(),
            message: format!(
                "invalid EMF size/record counts (nBytes={n_bytes}, nRecords={n_records})"
            ),
        });
    }
    if n_records > MAX_EMF_RECORDS {
        return Err(IoError::LimitExceeded {
            limit: format!("max_emf_records={MAX_EMF_RECORDS}"),
            actual: format!("nRecords={n_records}"),
        });
    }

    let frame_l = read_i32_le(bytes, 24);
    let frame_t = read_i32_le(bytes, 28);
    let frame_r = read_i32_le(bytes, 32);
    let frame_b = read_i32_le(bytes, 36);
    let page_w_mm = himetric_to_mm((frame_r - frame_l).abs()).clamp(10.0, 5000.0);
    let page_h_mm = himetric_to_mm((frame_b - frame_t).abs()).clamp(10.0, 5000.0);

    let mut doc = Document::new("Clipboard EMF", page_w_mm, page_h_mm, ColorMode::Rgb, 96.0);
    let layer_id = doc.page(0).and_then(|p| p.layers.first()).map(|l| l.id);

    let mut brushes: HashMap<u32, Fill> = HashMap::new();
    let mut pens: HashMap<u32, OutlineStyle> = HashMap::new();
    let mut current_fill = Fill::None;
    let mut current_outline = OutlineStyle::none();

    let mut in_path = false;
    let mut kurbo_builder = BezPath::new();
    let mut total_vertices = 0usize;
    let mut emitted_objects = 0usize;

    let emf_add_object = |doc: &mut Document,
                          layer_id: Option<coree_core::LayerId>,
                          obj: SceneObject,
                          emitted_objects: &mut usize|
     -> IoResult<()> {
        if *emitted_objects >= MAX_EMF_OBJECTS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_emf_objects={MAX_EMF_OBJECTS}"),
                actual: "EMF emitted object count exceeded".into(),
            });
        }
        *emitted_objects += 1;
        doc.add_object(obj, layer_id)?;
        Ok(())
    };

    let mut pos = 0usize;
    let mut walked = 0u32;
    let mut saw_eof = false;
    while pos < n_bytes {
        if walked >= MAX_EMF_RECORDS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_emf_records={MAX_EMF_RECORDS}"),
                actual: format!("walked={walked}"),
            });
        }
        if pos + 8 > n_bytes {
            return Err(IoError::InvalidFormat {
                format: "emf".into(),
                message: "truncated EMF record header".into(),
            });
        }
        let rtype = read_u32_le(bytes, pos);
        let rsize = read_u32_le(bytes, pos + 4) as usize;
        if rsize < 8 || !rsize.is_multiple_of(4) || pos + rsize > n_bytes {
            return Err(IoError::InvalidFormat {
                format: "emf".into(),
                message: format!("invalid EMF record type={rtype} size={rsize} at {pos}"),
            });
        }
        let rec = &bytes[pos..pos + rsize];
        walked += 1;
        pos += rsize;

        match rtype {
            EMR_EOF => {
                if rsize < 20 {
                    return Err(IoError::InvalidFormat {
                        format: "emf".into(),
                        message: "invalid EMR_EOF size".into(),
                    });
                }
                saw_eof = true;
                break;
            }
            EMR_CREATEBRUSHINDIRECT if rsize >= 24 => {
                let handle = read_u32_le(rec, 8);
                let style = read_u32_le(rec, 12);
                let colorref = read_u32_le(rec, 16);
                let fill = if style == BS_NULL {
                    Fill::None
                } else {
                    Fill::Solid(colorref_to_color(colorref))
                };
                brushes.insert(handle, fill);
            }
            EMR_CREATEPEN if rsize >= 28 => {
                let handle = read_u32_le(rec, 8);
                let style = read_u32_le(rec, 12);
                let width_himetric = read_i32_le(rec, 16);
                let colorref = read_u32_le(rec, 24);
                let outline = if style == PS_NULL || width_himetric <= 0 {
                    OutlineStyle::none()
                } else {
                    OutlineStyle::solid(
                        colorref_to_color(colorref),
                        himetric_to_mm(width_himetric).max(0.05),
                    )
                };
                pens.insert(handle, outline);
            }
            EMR_SELECTOBJECT if rsize >= 12 => {
                let handle = read_u32_le(rec, 8);
                if let Some(fill) = brushes.get(&handle) {
                    current_fill = fill.clone();
                }
                if let Some(outline) = pens.get(&handle) {
                    current_outline = outline.clone();
                }
            }
            EMR_DELETEOBJECT if rsize >= 12 => {
                let handle = read_u32_le(rec, 8);
                brushes.remove(&handle);
                pens.remove(&handle);
            }
            EMR_BEGINPATH => {
                in_path = true;
                kurbo_builder = BezPath::new();
            }
            EMR_MOVETOEX if rsize >= 16 && in_path => {
                total_vertices = total_vertices.saturating_add(1);
                if total_vertices > MAX_EMF_VERTICES {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_emf_vertices={MAX_EMF_VERTICES}"),
                        actual: format!("EMF vertex count {total_vertices} exceeded cap"),
                    });
                }
                kurbo_builder.move_to(kurbo::Point::new(
                    himetric_to_mm(read_i32_le(rec, 8)),
                    himetric_to_mm(read_i32_le(rec, 12)),
                ));
            }
            EMR_LINETO if rsize >= 16 && in_path => {
                total_vertices = total_vertices.saturating_add(1);
                if total_vertices > MAX_EMF_VERTICES {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_emf_vertices={MAX_EMF_VERTICES}"),
                        actual: format!("EMF vertex count {total_vertices} exceeded cap"),
                    });
                }
                kurbo_builder.line_to(kurbo::Point::new(
                    himetric_to_mm(read_i32_le(rec, 8)),
                    himetric_to_mm(read_i32_le(rec, 12)),
                ));
            }
            EMR_POLYBEZIERTO if rsize >= 28 && in_path => {
                let cptl = read_u32_le(rec, 24) as usize;
                let max_pts = (rsize - 28) / 8;
                let count = cptl.min(max_pts);
                total_vertices = total_vertices.saturating_add(count / 3);
                if total_vertices > MAX_EMF_VERTICES {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_emf_vertices={MAX_EMF_VERTICES}"),
                        actual: format!("EMF vertex count {total_vertices} exceeded cap"),
                    });
                }
                let mut i = 0;
                while i + 2 < count {
                    let off = 28 + i * 8;
                    let c1 = kurbo::Point::new(
                        himetric_to_mm(read_i32_le(rec, off)),
                        himetric_to_mm(read_i32_le(rec, off + 4)),
                    );
                    let c2 = kurbo::Point::new(
                        himetric_to_mm(read_i32_le(rec, off + 8)),
                        himetric_to_mm(read_i32_le(rec, off + 12)),
                    );
                    let end = kurbo::Point::new(
                        himetric_to_mm(read_i32_le(rec, off + 16)),
                        himetric_to_mm(read_i32_le(rec, off + 20)),
                    );
                    kurbo_builder.curve_to(c1, c2, end);
                    i += 3;
                }
            }
            EMR_CLOSEFIGURE if in_path => {
                kurbo_builder.close_path();
            }
            EMR_ENDPATH => {
                in_path = false;
            }
            EMR_FILLPATH | EMR_STROKEPATH | EMR_STROKEANDFILLPATH => {
                let bp = BezierPath::from_kurbo(&kurbo_builder);
                if !bp.subpaths.is_empty() {
                    let obj_fill = if rtype == EMR_STROKEPATH {
                        Fill::None
                    } else {
                        current_fill.clone()
                    };
                    let obj_outline = if rtype == EMR_FILLPATH {
                        OutlineStyle::none()
                    } else {
                        current_outline.clone()
                    };
                    let oid = doc.alloc_object_id();
                    let obj =
                        SceneObject::new(oid, format!("EMF Path {}", oid.0), NodePayload::Path(bp))
                            .with_fill(obj_fill)
                            .with_outline(obj_outline);
                    emf_add_object(&mut doc, layer_id, obj, &mut emitted_objects)?;
                    kurbo_builder = BezPath::new();
                }
            }
            EMR_RECTANGLE | EMR_ELLIPSE if rsize >= 24 => {
                let l = himetric_to_mm(read_i32_le(rec, 8));
                let t = himetric_to_mm(read_i32_le(rec, 12));
                let r = himetric_to_mm(read_i32_le(rec, 16));
                let b = himetric_to_mm(read_i32_le(rec, 20));
                let rect = Rect2D::from_xywh(l.min(r), t.min(b), (r - l).abs(), (b - t).abs());
                let bp = if rtype == EMR_ELLIPSE {
                    let el = kurbo::Ellipse::from_rect(rect.to_kurbo());
                    BezierPath::from_kurbo(&BezPath::from_iter(el.path_elements(0.1)))
                } else {
                    BezierPath::from_rect(rect)
                };
                let oid = doc.alloc_object_id();
                let obj =
                    SceneObject::new(oid, format!("EMF Shape {}", oid.0), NodePayload::Path(bp))
                        .with_fill(current_fill.clone())
                        .with_outline(current_outline.clone());
                emf_add_object(&mut doc, layer_id, obj, &mut emitted_objects)?;
            }
            EMR_POLYGON | EMR_POLYLINE if rsize >= 28 => {
                let cptl = read_u32_le(rec, 24) as usize;
                let count = cptl.min((rsize - 28) / 8);
                total_vertices = total_vertices.saturating_add(count);
                if total_vertices > MAX_EMF_VERTICES {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_emf_vertices={MAX_EMF_VERTICES}"),
                        actual: format!("EMF vertex count {total_vertices} exceeded cap"),
                    });
                }
                let mut pts = Vec::with_capacity(count);
                for idx in 0..count {
                    let off = 28 + idx * 8;
                    pts.push(Point2D::new(
                        himetric_to_mm(read_i32_le(rec, off)),
                        himetric_to_mm(read_i32_le(rec, off + 4)),
                    ));
                }
                if !pts.is_empty() {
                    let bp = BezierPath::from_polygon(pts, rtype == EMR_POLYGON);
                    let oid = doc.alloc_object_id();
                    let obj =
                        SceneObject::new(oid, format!("EMF Poly {}", oid.0), NodePayload::Path(bp))
                            .with_fill(if rtype == EMR_POLYGON {
                                current_fill.clone()
                            } else {
                                Fill::None
                            })
                            .with_outline(current_outline.clone());
                    emf_add_object(&mut doc, layer_id, obj, &mut emitted_objects)?;
                }
            }
            EMR_STRETCHDIBITS if rsize >= 120 => {
                let x_dest = himetric_to_mm(read_i32_le(rec, 24));
                let y_dest = himetric_to_mm(read_i32_le(rec, 28));
                let off_bmi = read_u32_le(rec, 48) as usize;
                let cb_bmi = read_u32_le(rec, 52) as usize;
                let off_bits = read_u32_le(rec, 56) as usize;
                let cb_bits = read_u32_le(rec, 60) as usize;
                let w_mm = himetric_to_mm(read_i32_le(rec, 72)).max(0.1);
                let h_mm = himetric_to_mm(read_i32_le(rec, 76)).max(0.1);
                if off_bmi >= 80 && off_bmi + cb_bmi == off_bits && off_bits + cb_bits <= rsize {
                    if let Some(asset) =
                        crate::wmf_emf::parse_dib_24bpp_to_asset(&rec[off_bmi..off_bits + cb_bits])
                    {
                        let asset_id = doc.register_asset(asset);
                        let oid = doc.alloc_object_id();
                        let obj = SceneObject::new(
                            oid,
                            format!("EMF Raster {}", oid.0),
                            NodePayload::Raster(coree_core::RasterLayerData {
                                asset_id,
                                width_mm: w_mm,
                                height_mm: h_mm,
                                filters: Vec::new(),
                                filter_enabled: Vec::new(),
                                crop_rect: None,
                            }),
                        )
                        .with_transform(Affine2D::translate(x_dest, y_dest));
                        emf_add_object(&mut doc, layer_id, obj, &mut emitted_objects)?;
                    }
                }
            }
            _ => {}
        }
    }

    if !saw_eof || walked != n_records {
        return Err(IoError::InvalidFormat {
            format: "emf".into(),
            message: format!("EMF EOF/record count invalid (saw_eof={saw_eof}, walked={walked}, nRecords={n_records})"),
        });
    }

    Ok(doc)
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn read_i32_le(buf: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Decodes clipboard payloads using vector-first priority (`SVG > PDF > PNG > EMF`).
pub fn decode_clipboard_auto(
    svg: Option<&[u8]>,
    pdf: Option<&[u8]>,
    emf: Option<&[u8]>,
    png: Option<&[u8]>,
) -> IoResult<(Document, ClipboardDecodedFormat)> {
    let mut last_err: Option<IoError> = None;
    if let Some(bytes) = svg.filter(|b| !b.is_empty()) {
        match decode_clipboard_svg(bytes) {
            Ok(doc) => return Ok((doc, ClipboardDecodedFormat::Svg)),
            Err(err) => last_err = Some(err),
        }
    }
    if let Some(bytes) = pdf.filter(|b| !b.is_empty()) {
        match decode_clipboard_pdf(bytes) {
            Ok(doc) => return Ok((doc, ClipboardDecodedFormat::Pdf)),
            Err(err) => last_err = Some(err),
        }
    }
    if let Some(bytes) = png.filter(|b| !b.is_empty()) {
        match decode_clipboard_png(bytes) {
            Ok(doc) => return Ok((doc, ClipboardDecodedFormat::Png)),
            Err(err) => last_err = Some(err),
        }
    }
    if let Some(bytes) = emf.filter(|b| !b.is_empty()) {
        match decode_clipboard_emf(bytes) {
            Ok(doc) => return Ok((doc, ClipboardDecodedFormat::Emf)),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| IoError::InvalidFormat {
        format: "clipboard".into(),
        message: "no supported clipboard payload present".into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{ArtisticText, EllipseMode, ParametricShape, TextStyle};

    fn sample_clipboard_doc() -> Document {
        let mut doc = Document::new_a4("Clipboard Sample");
        doc.set_color_mode(ColorMode::Rgb);
        let lid = doc.page(0).unwrap().layers[0].id;

        let r_id = doc.alloc_object_id();
        let rect = SceneObject::new(
            r_id,
            "BrandRect",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                15.0, 20.0, 50.0, 30.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(32, 128, 210, 255)))
        .with_outline(OutlineStyle::solid(Color::rgba8(10, 20, 30, 255), 0.5));
        doc.add_object(rect, Some(lid)).unwrap();

        let e_id = doc.alloc_object_id();
        let ellipse = SceneObject::new(
            e_id,
            "AccentEllipse",
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 18.0,
                ry: 12.0,
                start_deg: 0.0,
                end_deg: 360.0,
                mode: EllipseMode::Full,
            }),
        )
        .with_transform(Affine2D::translate(100.0, 80.0))
        .with_fill(Fill::Solid(Color::rgba8(240, 180, 30, 255)))
        .with_outline(OutlineStyle::none());
        doc.add_object(ellipse, Some(lid)).unwrap();

        let t_id = doc.alloc_object_id();
        let txt = SceneObject::new(
            t_id,
            "Label",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Hello Vector",
                TextStyle::default(),
            )),
        )
        .with_transform(Affine2D::translate(20.0, 75.0));
        doc.add_object(txt, Some(lid)).unwrap();

        doc
    }

    #[test]
    fn clipboard_payload_encodes_valid_svg_pdf_emf_and_png() {
        let doc = sample_clipboard_doc();
        let payload = encode_clipboard_payload(&doc, 0).expect("encode clipboard payload");

        assert!(payload.svg.contains("<svg"));
        assert!(payload.svg.contains("viewBox="));
        assert!(payload.pdf.starts_with(b"%PDF-1."));
        assert!(payload.pdf.windows(5).any(|w| w == b"%%EOF"));
        assert!(payload
            .png
            .starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
        assert!(payload.rgba_width > 0 && payload.rgba_height > 0);
        assert_eq!(
            payload.rgba.len(),
            (payload.rgba_width as usize) * (payload.rgba_height as usize) * 4
        );
        assert_eq!(payload.plain_text.as_deref(), Some("Hello Vector"));

        let emf = &payload.emf;
        assert!(emf.len() >= 88);
        let rec_type = read_u32_le(emf, 0);
        let rec_size = read_u32_le(emf, 4);
        assert_eq!(rec_type, 1);
        assert_eq!(rec_size, 88);
        assert_eq!(&emf[40..44], b" EMF");
        let n_bytes = read_u32_le(emf, 48) as usize;
        let n_records = read_u32_le(emf, 52);
        assert_eq!(n_bytes, emf.len());
        assert!(n_records >= 3);

        let mut pos = 0usize;
        let mut walked = 0u32;
        let mut saw_eof = false;
        while pos < n_bytes {
            let rt = read_u32_le(emf, pos);
            let rs = read_u32_le(emf, pos + 4) as usize;
            assert!(rs >= 8 && rs.is_multiple_of(4) && pos + rs <= n_bytes);
            walked += 1;
            pos += rs;
            if rt == EMR_EOF {
                assert!(rs >= 20);
                saw_eof = true;
                break;
            }
        }
        assert!(saw_eof);
        assert_eq!(pos, n_bytes);
        assert_eq!(walked, n_records);
    }

    #[test]
    fn clipboard_svg_pdf_emf_png_roundtrip_decode_works() {
        let doc = sample_clipboard_doc();
        let payload = encode_clipboard_payload(&doc, 0).expect("encode payload");

        let (svg_doc, fmt_svg) =
            decode_clipboard_auto(Some(payload.svg.as_bytes()), None, None, None)
                .expect("decode SVG");
        assert_eq!(fmt_svg, ClipboardDecodedFormat::Svg);
        assert!(!svg_doc.objects().is_empty());

        let (pdf_doc, fmt_pdf) =
            decode_clipboard_auto(None, Some(&payload.pdf), None, None).expect("decode PDF");
        assert_eq!(fmt_pdf, ClipboardDecodedFormat::Pdf);
        assert!(!pdf_doc.objects().is_empty());

        let (emf_doc, fmt_emf) =
            decode_clipboard_auto(None, None, Some(&payload.emf), None).expect("decode EMF");
        assert_eq!(fmt_emf, ClipboardDecodedFormat::Emf);
        assert!(emf_doc.objects().len() >= 2);

        let (png_doc, fmt_png) =
            decode_clipboard_auto(None, None, None, Some(&payload.png)).expect("decode PNG");
        assert_eq!(fmt_png, ClipboardDecodedFormat::Png);
        assert_eq!(png_doc.objects().len(), 1);

        let (_auto_doc, fmt_auto) = decode_clipboard_auto(
            Some(payload.svg.as_bytes()),
            Some(&payload.pdf),
            Some(&payload.emf),
            Some(&payload.png),
        )
        .expect("decode auto");
        assert_eq!(fmt_auto, ClipboardDecodedFormat::Svg);
    }

    #[test]
    fn clipboard_build_document_attaches_roots_to_layer_zero() {
        let mut src = Document::new_a4("Source");
        let lid = src.page(0).unwrap().layers[0].id;
        let oid = src.alloc_object_id();
        let rect = SceneObject::new(
            oid,
            "Box",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 10.0, 20.0, 20.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(200, 40, 40, 255)));
        src.add_object(rect, Some(lid)).unwrap();

        let mut snap = Document::new_a4("Snap");
        snap.insert_snapshot_object_if_absent(src.object(oid).unwrap().clone());
        assert!(snap.page(0).unwrap().layers[0].objects.is_empty());

        let export_doc = build_clipboard_document(&src, &[oid], Some(&snap)).unwrap();
        assert_eq!(export_doc.page(0).unwrap().layers[0].objects, vec![oid]);
        let svg = encode_clipboard_svg(&export_doc, 0).unwrap();
        assert!(svg.contains("<path"));
    }

    #[test]
    fn clipboard_subtree_copies_symbols_clones_and_emits_emf() {
        let mut src = Document::new_a4("ClipSym");
        let lid = src.page(0).unwrap().layers[0].id;
        let master_id = src.alloc_object_id();
        src.add_object(
            SceneObject::new(
                master_id,
                "Master",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    5.0, 5.0, 12.0, 12.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgba8(20, 140, 220, 255))),
            Some(lid),
        )
        .unwrap();
        let clone_id = src.alloc_object_id();
        src.add_object(
            SceneObject::new(
                clone_id,
                "CloneObj",
                NodePayload::Clone {
                    master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(30.0, 0.0)),
            Some(lid),
        )
        .unwrap();

        let sym_id = coree_core::SymbolId::new();
        let child_id = ObjectId(8801);
        let grp_id = ObjectId(8802);
        let sym_child = SceneObject::new(
            child_id,
            "SymChild",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 6.0, 6.0))),
        )
        .with_fill(Fill::Solid(Color::rgba8(220, 80, 20, 255)));
        let sym_grp = SceneObject::new(grp_id, "SymGroup", NodePayload::Group(vec![child_id]));
        src.insert_symbol(SymbolDef {
            id: sym_id,
            name: "SymClip".into(),
            objects: vec![sym_child, sym_grp],
        });
        let inst_id = src.alloc_object_id();
        src.add_object(
            SceneObject::new(inst_id, "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(60.0, 10.0)),
            Some(lid),
        )
        .unwrap();

        let clip_doc = build_clipboard_document(&src, &[clone_id, inst_id], None).unwrap();
        assert!(
            clip_doc.object(master_id).is_some(),
            "Clone master must be copied"
        );
        assert!(
            clip_doc.symbol(sym_id).is_some(),
            "SymbolDef must be copied"
        );
        let emf = encode_clipboard_emf(&clip_doc, 0).unwrap();
        assert!(emf.len() > 88);

        // Verify visible TextOnPath host path in SymbolDef is not hidden by referenced set
        let sym_ref = clip_doc.symbol(sym_id).unwrap();
        let (_, referenced) = build_emf_symbol_maps(sym_ref);
        assert!(referenced.contains(&child_id));
        let path_id = ObjectId(8803);
        let top_id = ObjectId(8804);
        let sym_top = SymbolDef {
            id: coree_core::SymbolId::new(),
            name: "SymTop".into(),
            objects: vec![
                SceneObject::new(
                    path_id,
                    "HostPath",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 20.0, 10.0,
                    ))),
                ),
                SceneObject::new(
                    top_id,
                    "Top",
                    NodePayload::TextOnPath(coree_core::TextOnPath {
                        text: coree_core::ArtisticText::from_plain(
                            "OnPath",
                            coree_core::TextStyle::default(),
                        ),
                        path_id,
                        offset_mm: 0.0,
                        distance_mm: 0.0,
                        flip_vertical: false,
                        flip_horizontal: false,
                    }),
                ),
            ],
        };
        let (_, top_ref) = build_emf_symbol_maps(&sym_top);
        assert!(!top_ref.contains(&path_id));
    }

    #[test]
    fn clipboard_emf_resolves_clone_inside_symbol_def() {
        let mut doc = Document::new_a4("EmfSymClone");
        let lid = doc.page(0).unwrap().layers[0].id;
        let sym_id = coree_core::SymbolId::new();
        let sym_master_id = ObjectId(9901);
        let sym_clone_id = ObjectId(9902);
        let sym_master = SceneObject::new(
            sym_master_id,
            "SymMaster",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 10.0, 10.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(20, 120, 220, 255)))
        .with_outline(OutlineStyle::none());
        let sym_clone = SceneObject::new(
            sym_clone_id,
            "SymClone",
            NodePayload::Clone {
                master_id: sym_master_id,
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_transform(Affine2D::translate(30.0, 0.0));
        doc.insert_symbol(SymbolDef {
            id: sym_id,
            name: "SymWithClone".into(),
            objects: vec![sym_master, sym_clone],
        });
        let inst_id = doc.alloc_object_id();
        doc.add_object(
            SceneObject::new(inst_id, "Inst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(10.0, 15.0)),
            Some(lid),
        )
        .unwrap();

        let emf = encode_clipboard_emf(&doc, 0).expect("encode EMF with SymbolDef Clone");
        let (decoded, fmt) =
            decode_clipboard_auto(None, None, Some(&emf), None).expect("decode EMF");
        assert_eq!(fmt, ClipboardDecodedFormat::Emf);
        assert_eq!(
            decoded.objects().len(),
            2,
            "both SymbolDef master and Clone must emit EMF shapes"
        );
        let mut xs: Vec<f64> = decoded
            .objects()
            .keys()
            .filter_map(|id| decoded.world_object_bounds(*id).map(|b| b.min_x))
            .collect();
        xs.sort_by(f64::total_cmp);
        assert!((xs[0] - 10.0).abs() < 0.5, "master at x=10, got {:?}", xs);
        assert!((xs[1] - 40.0).abs() < 0.5, "clone at x=40, got {:?}", xs);
    }

    #[test]
    fn clipboard_emf_chained_symbol_clone_preserves_overrides_skips_unlinked_and_avoids_text_on_path_double_xf(
    ) {
        let mut doc = Document::new("EmfChainedClone", 160.0, 100.0, ColorMode::Rgb, 300.0);
        let lid = doc.page(0).unwrap().layers[0].id;
        let sym_id = coree_core::SymbolId::new();
        let master_id = ObjectId(9911);
        let clone_id = ObjectId(9912);
        let chained_id = ObjectId(9913);
        let unlinked_id = ObjectId(9914);
        let host_id = ObjectId(9915);
        let top_id = ObjectId(9916);

        let master = SceneObject::new(
            master_id,
            "SymMaster",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 10.0, 10.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(255, 0, 0, 255)))
        .with_outline(OutlineStyle::solid(Color::BLACK, 0.4));
        let clone = SceneObject::new(
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
        let chained = SceneObject::new(
            chained_id,
            "SymChained",
            NodePayload::Clone {
                master_id: clone_id,
                link_fill: false,
                link_outline: false,
                link_shape: true,
            },
        )
        .with_fill(Fill::Solid(Color::rgba8(0, 0, 255, 255)))
        .with_outline(OutlineStyle::solid(Color::rgba8(0, 255, 0, 255), 1.0))
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
        let host = SceneObject::new(
            host_id,
            "Host",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 40.0),
                Point2D::new(30.0, 40.0),
            )),
        )
        .with_outline(OutlineStyle::none());
        let top = SceneObject::new(
            top_id,
            "Top",
            NodePayload::TextOnPath(coree_core::TextOnPath {
                text: coree_core::ArtisticText::from_plain(
                    "AB",
                    coree_core::TextStyle {
                        font_size_pt: 14.0,
                        fill: Color::BLACK,
                        ..coree_core::TextStyle::default()
                    },
                ),
                path_id: host_id,
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );

        doc.insert_symbol(SymbolDef {
            id: sym_id,
            name: "ChainedEmfSym".into(),
            objects: vec![master, clone, chained, unlinked, host, top],
        });
        doc.add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(10.0, 15.0)),
            Some(lid),
        )
        .unwrap();

        let emf = encode_clipboard_emf(&doc, 0).expect("encode EMF");
        let (decoded, fmt) =
            decode_clipboard_auto(None, None, Some(&emf), None).expect("decode EMF");
        assert_eq!(fmt, ClipboardDecodedFormat::Emf);

        let rects: Vec<_> = decoded
            .objects()
            .iter()
            .filter_map(|(id, o)| {
                let bb = decoded.world_object_bounds(*id)?;
                if bb.height() >= 8.0 && bb.height() <= 12.0 {
                    Some((bb.min_x, o))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            rects.len(),
            3,
            "expected 3 rectangles (master, clone, chained) and 0 for link_shape=false, got {}",
            rects.len()
        );
        let (_, chained_obj) = rects
            .iter()
            .find(|(min_x, _)| (*min_x - 70.0).abs() < 0.8)
            .expect("chained clone at x=70");
        match &chained_obj.fill {
            Fill::Solid(c) => {
                let [r, g, b, _] = c.to_rgba8();
                assert!(
                    b > 200 && r < 30 && g < 30,
                    "chained fill must be Blue, got {:?}",
                    [r, g, b]
                );
            }
            other => panic!("expected solid blue fill, got {other:?}"),
        }
        let [or, og, ob, _] = chained_obj.outline.color.to_rgba8();
        assert!(
            og > 200 && or < 30 && ob < 30,
            "chained outline must be Green, got {:?}",
            [or, og, ob]
        );

        // TextOnPath host is at local y=40, inst offset (10,15) => world y<=55, x<=40 (not double-transformed to x=20..50, y=70)
        let text_objs: Vec<_> = decoded
            .objects()
            .keys()
            .filter_map(|id| {
                let bb = decoded.world_object_bounds(*id)?;
                (bb.min_y > 35.0).then_some(bb)
            })
            .collect();
        assert!(
            !text_objs.is_empty(),
            "expected decoded TextOnPath glyph shapes"
        );
        for bb in &text_objs {
            assert!(
                bb.max_y <= 57.0 && bb.min_x < 19.0,
                "TextOnPath must not be double-transformed by sym_root_xf (10,15): {bb:?}"
            );
        }
    }

    #[test]
    fn emf_record_and_object_caps_reject_bomb() {
        let n_rects = 250_000u32;
        let rec_size = 24usize;
        let eof_size = 20usize;
        let n_bytes = 88 + (n_rects as usize) * rec_size + eof_size;
        let mut emf = vec![0u8; 88];
        emf[0..4].copy_from_slice(&EMR_HEADER.to_le_bytes());
        emf[4..8].copy_from_slice(&88u32.to_le_bytes());
        emf[24..28].copy_from_slice(&0i32.to_le_bytes());
        emf[28..32].copy_from_slice(&0i32.to_le_bytes());
        emf[32..36].copy_from_slice(&10_000i32.to_le_bytes());
        emf[36..40].copy_from_slice(&10_000i32.to_le_bytes());
        emf[40..44].copy_from_slice(b" EMF");
        emf[44..48].copy_from_slice(&0x10000u32.to_le_bytes());
        emf[48..52].copy_from_slice(&(n_bytes as u32).to_le_bytes());
        emf[52..56].copy_from_slice(&(2 + n_rects).to_le_bytes());
        for _ in 0..n_rects {
            emf.extend_from_slice(&43u32.to_le_bytes()); // EMR_RECTANGLE
            emf.extend_from_slice(&24u32.to_le_bytes());
            for v in [0i32, 0, 1000, 1000] {
                emf.extend_from_slice(&v.to_le_bytes());
            }
        }
        emf.extend_from_slice(&EMR_EOF.to_le_bytes());
        emf.extend_from_slice(&20u32.to_le_bytes());
        emf.extend_from_slice(&0u32.to_le_bytes());
        emf.extend_from_slice(&0x10u32.to_le_bytes());
        emf.extend_from_slice(&20u32.to_le_bytes());

        let err = decode_clipboard_emf(&emf).unwrap_err();
        assert!(
            matches!(err, IoError::LimitExceeded { .. }),
            "expected LimitExceeded on 250k EMR_RECTANGLE bomb, got {err:?}"
        );
    }
}
