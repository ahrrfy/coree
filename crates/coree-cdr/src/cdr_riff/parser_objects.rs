// SPDX-License-Identifier: MIT OR Apache-2.0

//! RIFF CDR object-list, transform (`trfd`), and lens (`lens`) decoders.

use super::fild_outl::{
    parse_fild_chunk_with_ctx, parse_outl_chunk_with_ctx, rescale_normalized_fill_to_bounds,
};
use super::legacy_v3_v5::parse_v3_v5_loda;
use super::loda::{parse_loda_chunk, CdrNativeEllipse, CdrNativeRect, ParsedLoda, ParsedObjProps};
use super::parser::{parse_group_list, register_bmp_chunk_payload, ParseContext};
use super::text_style::{parse_stlt_chunk, parse_txsm_chunk, ParsedStyleEntry};
use crate::coord::{cdr32_to_mm, read_tagged_color};
use crate::error::{CdrError, CdrResult};
use crate::limits::ByteReader;
use crate::riff::{serialize_list_body, RiffNode};
use coree_core::{
    math::{sanitize_f32, sanitize_f64},
    Affine2D, BezierPath, ColorMapMode, Document, Fill, ImageAssetId, LensConfig, LensKind,
    NodePayload, ObjectId, OutlineStyle, ParametricShape, Point2D, RasterLayerData, Rect2D,
    SceneObject, UnknownCdrChunk,
};

pub(super) fn parse_object_list(
    children: &[RiffNode],
    depth: usize,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<Option<ObjectId>> {
    ctx.limits.check_depth(depth)?;

    let mut loda_data: Option<ParsedLoda> = None;
    let mut synthetic_transform = Affine2D::IDENTITY;
    let mut real_trfd: Option<CdrNativeMatrix> = None;
    let mut inline_fill: Option<Fill> = None;
    let mut inline_fill_normalized = false;
    let mut inline_outline: Option<OutlineStyle> = None;
    let mut inline_outline_explicit_none = false;
    let mut inline_style: Option<ParsedStyleEntry> = None;
    let mut inline_bmp_id: Option<ImageAssetId> = None;
    let mut txsm_payload: Option<Vec<u8>> = None;
    let mut lens: Option<LensConfig> = None;
    let mut powerclip_children: Vec<ObjectId> = Vec::new();
    let mut unknown_chunks: Vec<UnknownCdrChunk> = Vec::new();

    let mut flat_chunks: Vec<(&[u8; 4], &[u8])> = Vec::new();
    for node in children {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => flat_chunks.push((fourcc, data.as_slice())),
            RiffNode::List {
                list_type,
                children: sub_ch,
                ..
            } => match list_type {
                b"lgob" => {
                    for gc in sub_ch {
                        match gc {
                            RiffNode::Chunk { fourcc, data, .. } => {
                                flat_chunks.push((fourcc, data.as_slice()));
                            }
                            RiffNode::List {
                                list_type: [b't', b'r', b'f', b'l'],
                                children: gch,
                                ..
                            } => {
                                for tc in gch {
                                    match tc {
                                        RiffNode::Chunk { fourcc, data, .. } => {
                                            flat_chunks.push((fourcc, data.as_slice()));
                                        }
                                        RiffNode::List {
                                            list_type: tlt,
                                            children: tch,
                                            ..
                                        } => {
                                            let raw = serialize_list_body(tlt, tch, ctx.endian)?;
                                            unknown_chunks.push(UnknownCdrChunk {
                                                fourcc: *b"LIST",
                                                version_hint: u32::from(ctx.version),
                                                raw_bytes: raw,
                                            });
                                        }
                                    }
                                }
                            }
                            RiffNode::List {
                                list_type: sub_lt,
                                children: sub_ch2,
                                ..
                            } => {
                                let raw = serialize_list_body(sub_lt, sub_ch2, ctx.endian)?;
                                unknown_chunks.push(UnknownCdrChunk {
                                    fourcc: *b"LIST",
                                    version_hint: u32::from(ctx.version),
                                    raw_bytes: raw,
                                });
                            }
                        }
                    }
                }
                b"trfl" => {
                    for tc in sub_ch {
                        match tc {
                            RiffNode::Chunk { fourcc, data, .. } => {
                                flat_chunks.push((fourcc, data.as_slice()));
                            }
                            RiffNode::List {
                                list_type: tlt,
                                children: tch,
                                ..
                            } => {
                                let raw = serialize_list_body(tlt, tch, ctx.endian)?;
                                unknown_chunks.push(UnknownCdrChunk {
                                    fourcc: *b"LIST",
                                    version_hint: u32::from(ctx.version),
                                    raw_bytes: raw,
                                });
                            }
                        }
                    }
                }
                b"pwrp" => {
                    for pnode in sub_ch {
                        if let RiffNode::List {
                            list_type: plt,
                            children: pch,
                            ..
                        } = pnode
                        {
                            match plt {
                                b"obj " => {
                                    if let Some(cid) = parse_object_list(pch, depth + 1, doc, ctx)?
                                    {
                                        powerclip_children.push(cid);
                                    }
                                }
                                b"grp " => {
                                    if let Some(cid) = parse_group_list(pch, depth + 1, doc, ctx)? {
                                        powerclip_children.push(cid);
                                    }
                                }
                                _ => {
                                    let raw = serialize_list_body(plt, pch, ctx.endian)?;
                                    unknown_chunks.push(UnknownCdrChunk {
                                        fourcc: *b"LIST",
                                        version_hint: u32::from(ctx.version),
                                        raw_bytes: raw,
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {
                    let raw = serialize_list_body(list_type, sub_ch, ctx.endian)?;
                    unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            },
        }
    }

    for (fourcc, data) in flat_chunks {
        match fourcc {
            b"loda" => {
                if ctx.is_v3_v5 {
                    if let Some(v3) =
                        parse_v3_v5_loda(data, ctx.current_page_w_mm, ctx.current_page_h_mm)
                    {
                        loda_data = Some(v3);
                        continue;
                    }
                }
                match parse_loda_chunk(data) {
                    Ok(parsed) => loda_data = Some(parsed),
                    Err(e) => ctx.warnings.push(format!("malformed loda chunk: {e}")),
                }
            }
            b"trfd" => {
                if is_cdr_native_trfd(data) {
                    match parse_cdr_native_trfd(data) {
                        Ok(m) => real_trfd = Some(m),
                        Err(e) => ctx.warnings.push(format!("malformed real trfd chunk: {e}")),
                    }
                } else {
                    match parse_synthetic_trfd_chunk(data) {
                        Ok(xf) => synthetic_transform = xf,
                        Err(e) => ctx.warnings.push(format!("malformed trfd chunk: {e}")),
                    }
                }
            }
            b"fild" => match parse_fild_chunk_with_ctx(data, ctx.is_zcf, Some(ctx)) {
                Ok((id, fill, is_norm)) => {
                    if id != 0 {
                        if is_norm {
                            ctx.normalized_fills.insert(id);
                        }
                        ctx.fills.insert(id, fill.clone());
                    }
                    inline_fill_normalized = is_norm;
                    inline_fill = Some(fill);
                }
                Err(e) => ctx.warnings.push(format!("malformed fild chunk: {e}")),
            },
            b"outl" => match parse_outl_chunk_with_ctx(data, ctx.is_zcf, Some(ctx)) {
                Ok((id, outl, is_none)) => {
                    if id != 0 {
                        if is_none {
                            ctx.outlines_explicit_none.insert(id);
                        }
                        ctx.outlines.insert(id, outl.clone());
                    }
                    inline_outline_explicit_none = is_none;
                    inline_outline = Some(outl);
                }
                Err(e) => ctx.warnings.push(format!("malformed outl chunk: {e}")),
            },
            b"stlt" => match parse_stlt_chunk(data) {
                Ok((id, entry)) => {
                    if id != 0 {
                        ctx.styles.insert(id, entry.clone());
                    }
                    inline_style = Some(entry);
                }
                Err(e) => ctx.warnings.push(format!("malformed stlt chunk: {e}")),
            },
            b"txsm" | b"txst" => {
                txsm_payload = Some(data.to_vec());
            }
            b"bmp " => {
                let asset_id = register_bmp_chunk_payload(data, None, doc, ctx)?;
                inline_bmp_id = Some(asset_id);
            }
            b"lens" => match parse_lens_chunk(data) {
                Ok(l) => lens = Some(l),
                Err(e) => ctx.warnings.push(format!("malformed lens chunk: {e}")),
            },
            b"flgs" | b"spid" | b"bbox" | b"obbx" | b"ftil" | b"usdn" => {}
            _ => {
                unknown_chunks.push(UnknownCdrChunk {
                    fourcc: *fourcc,
                    version_hint: u32::from(ctx.version),
                    raw_bytes: data.to_vec(),
                });
            }
        }
    }

    let mut loda = loda_data.unwrap_or_default();
    for (arg_typ, arg_bytes) in std::mem::take(&mut loda.unknown_args) {
        let mut raw = Vec::with_capacity(4 + arg_bytes.len());
        raw.extend_from_slice(&arg_typ.to_le_bytes());
        raw.extend_from_slice(&arg_bytes);
        unknown_chunks.push(UnknownCdrChunk {
            fourcc: *b"LODA",
            version_hint: u32::from(ctx.version),
            raw_bytes: raw,
        });
    }

    let mut fill_is_normalized = if inline_fill.is_some() {
        inline_fill_normalized
    } else {
        loda.fill_id
            .is_some_and(|id| ctx.normalized_fills.contains(&id))
    };
    let mut fill = inline_fill
        .or_else(|| loda.fill_id.and_then(|id| ctx.fills.get(&id).cloned()))
        .unwrap_or(Fill::None);
    let mut fill_explicit_from_c9 = false;
    if let Some(ref c9) = loda.style_json {
        if let Some(explicit_fill) = c9.explicit_fill() {
            fill = explicit_fill;
            fill_is_normalized = true;
            fill_explicit_from_c9 = true;
        }
    }
    if matches!(fill, Fill::None) && !fill_explicit_from_c9 {
        if let Some(sid) = loda.style_id {
            if let Some(resolved_style) = ctx.zcf.stylesheet.resolve(&sid.to_string()) {
                if let Some(st_fill) = resolved_style.explicit_fill() {
                    fill = st_fill;
                    fill_is_normalized = true;
                }
            }
        }
    }

    let outline_explicit_none = inline_outline_explicit_none
        || loda
            .outline_id
            .is_some_and(|id| ctx.outlines_explicit_none.contains(&id));
    let mut outline = inline_outline
        .or_else(|| {
            loda.outline_id
                .and_then(|id| ctx.outlines.get(&id).cloned())
        })
        .unwrap_or_else(OutlineStyle::none);
    if let Some(ref c9) = loda.style_json {
        c9.apply_outline_overrides(&mut outline, outline_explicit_none);
    }

    if txsm_payload.is_none() {
        if let Some(tid) = loda.text_stream_id {
            if let Some(raw) = ctx.text_streams.get(&tid) {
                txsm_payload = Some(raw.clone());
            }
        }
    }

    let mut final_transform = synthetic_transform;
    let mut resolved_curve = loda.curve;
    let mut resolved_shape = loda.shape;

    if loda.is_real_cdr {
        let m = real_trfd.unwrap_or(CdrNativeMatrix::IDENTITY);
        let page_xf = m.to_page_mm_affine(ctx.current_page_w_mm, ctx.current_page_h_mm);

        if let Some(rect) = loda.real_rect {
            let (shape, rect_xf) =
                convert_cdr_native_rect(rect, m, ctx.current_page_w_mm, ctx.current_page_h_mm);
            resolved_shape = Some(shape);
            final_transform = rect_xf;
        } else if let Some(ell) = loda.real_ellipse {
            let (shape, ell_xf) =
                convert_cdr_native_ellipse(ell, m, ctx.current_page_w_mm, ctx.current_page_h_mm);
            resolved_shape = Some(shape);
            final_transform = ell_xf;
        } else if let Some(curve) = resolved_curve.take() {
            resolved_curve = Some(curve.transformed(&page_xf));
            final_transform = Affine2D::IDENTITY;
        } else {
            final_transform = page_xf;
        }
    }

    // Resolve payload (structured clean-room non-flattened types take priority over fallback curves)
    let payload: Option<NodePayload> = if let Some(table) = loda.table {
        Some(NodePayload::Table(table))
    } else if let Some(dim) = loda.dimension {
        Some(NodePayload::Dimension(dim))
    } else if let Some(conn) = loda.connector {
        Some(NodePayload::Connector(conn))
    } else if let Some(sym_id) = loda.symbol_instance {
        Some(NodePayload::SymbolInstance(sym_id))
    } else if let Some(bs) = loda.bspline {
        Some(NodePayload::BSpline(bs))
    } else if let Some((spine, style)) = loda.artistic_media {
        Some(NodePayload::ArtisticMedia { spine, style })
    } else if let Some((master_id, link_fill, link_outline, link_shape)) = loda.clone_ref {
        Some(NodePayload::Clone {
            master_id,
            link_fill,
            link_outline,
            link_shape,
        })
    } else if let Some(top) = loda.text_on_path {
        Some(NodePayload::TextOnPath(top))
    } else if let Some(raw_txsm) = txsm_payload {
        let cdss_default = if loda.text_meta.is_some_and(|m| m.is_paragraph) {
            ctx.zcf.stylesheet.default_paragraph_text()
        } else {
            ctx.zcf.stylesheet.default_artistic_text()
        };
        match parse_txsm_chunk(
            &raw_txsm,
            loda.style_id,
            inline_style.as_ref(),
            loda.text_meta,
            &ctx.styles,
            &ctx.fonts,
            cdss_default,
        ) {
            Ok(p) => Some(p),
            Err(e) => {
                ctx.warnings.push(format!("malformed txsm chunk: {e}"));
                None
            }
        }
    } else if let Some(bmp_ref) = loda.bitmap_ref {
        let asset_id = inline_bmp_id.or_else(|| ctx.bitmaps.get(&bmp_ref.bmp_id).copied());
        if asset_id.is_none() {
            ctx.warnings.push(format!(
                "missing bitmap reference bmp_id={} skipped instead of substituting another image",
                bmp_ref.bmp_id
            ));
        }
        asset_id.map(|aid| {
            NodePayload::Raster(RasterLayerData {
                asset_id: aid,
                width_mm: bmp_ref.width_mm,
                height_mm: bmp_ref.height_mm,
                filters: Vec::new(),
                crop_rect: bmp_ref.crop_rect,
            })
        })
    } else if let Some(aid) = inline_bmp_id {
        let (w_mm, h_mm) = doc.asset(aid).map_or((50.0, 50.0), |a| {
            let dpi = f64::from(a.source_dpi.max(1.0));
            (
                f64::from(a.width) * 25.4 / dpi,
                f64::from(a.height) * 25.4 / dpi,
            )
        });
        Some(NodePayload::Raster(RasterLayerData {
            asset_id: aid,
            width_mm: w_mm,
            height_mm: h_mm,
            filters: Vec::new(),
            crop_rect: None,
        }))
    } else if !powerclip_children.is_empty() || loda.powerclip_meta.is_some() {
        let pm = loda.powerclip_meta;
        let container_parametric = pm
            .as_ref()
            .and_then(|m| m.container_parametric.clone())
            .or(resolved_shape);
        let container = resolved_curve
            .or_else(|| {
                container_parametric
                    .as_ref()
                    .map(ParametricShape::to_bezier_path)
            })
            .unwrap_or_else(|| BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0)));
        Some(NodePayload::PowerClip {
            container: Box::new(container),
            container_parametric,
            lock_contents: pm.is_none_or(|m| m.lock_contents),
            contents: powerclip_children,
        })
    } else if let Some(shape) = resolved_shape {
        Some(NodePayload::Shape(shape))
    } else {
        resolved_curve.map(NodePayload::Path)
    };

    let Some(payload) = payload else {
        doc.extend_unknown_doc_chunks(unknown_chunks);
        return Ok(None);
    };

    if fill_is_normalized {
        let local_bb = match &payload {
            NodePayload::Path(p) => p.bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().bounding_box(),
            NodePayload::PowerClip { container, .. } => container.bounding_box(),
            _ => None,
        };
        if let Some(bb) = local_bb {
            rescale_normalized_fill_to_bounds(&mut fill, bb);
        }
    }

    let oid = doc.alloc_object_id();
    if let Some(src_id) = loda.source_oid {
        ctx.oid_remap.insert(src_id, oid);
    }
    let props = loda.props.unwrap_or_else(|| {
        let default_name = match &payload {
            NodePayload::Shape(ParametricShape::Rectangle { .. }) => "Rectangle",
            NodePayload::Shape(ParametricShape::Ellipse { .. }) => "Ellipse",
            NodePayload::Shape(ParametricShape::Polygon { .. }) => "Polygon",
            NodePayload::Path(_) => "Curve",
            NodePayload::ArtisticText(_) => "Artistic Text",
            NodePayload::ParagraphText(_) => "Paragraph Text",
            NodePayload::Raster(_) => "Bitmap",
            _ => "Object",
        };
        ParsedObjProps {
            name: loda.real_name.unwrap_or_else(|| default_name.to_owned()),
            ..ParsedObjProps::default()
        }
    });
    let mut obj = SceneObject::new(oid, props.name, payload);
    obj.transform = final_transform;
    obj.fill = fill;
    obj.outline = outline;
    obj.lens = lens;
    obj.effects = loda.effects;
    obj.style_id = loda.object_style_id;
    obj.perspective = loda.perspective;
    if let Some((cs_id, cs_slot)) = loda.color_style_link {
        obj.color_style_id = cs_id;
        obj.color_style_slot = cs_slot;
    }
    obj.blend_mode = props.blend_mode;
    obj.opacity = props.opacity;
    obj.visible = props.visible;
    obj.locked = props.locked;
    obj.printable = props.printable;
    obj.unknown_chunks = unknown_chunks;

    let _ = doc.insert_loaded_object(obj);
    Ok(Some(oid))
}

#[derive(Debug, Clone, Copy)]
struct CdrNativeMatrix {
    v0: f64,
    v1: f64,
    x0_mm: f64,
    v3: f64,
    v4: f64,
    y0_mm: f64,
}

impl CdrNativeMatrix {
    const IDENTITY: Self = Self {
        v0: 1.0,
        v1: 0.0,
        x0_mm: 0.0,
        v3: 0.0,
        v4: 1.0,
        y0_mm: 0.0,
    };

    fn is_finite(self) -> bool {
        self.v0.is_finite()
            && self.v1.is_finite()
            && self.x0_mm.is_finite()
            && self.v3.is_finite()
            && self.v4.is_finite()
            && self.y0_mm.is_finite()
            && self.v0.hypot(self.v3).is_finite()
            && self.v1.hypot(self.v4).is_finite()
    }

    fn sanitized(self) -> Self {
        if self.is_finite() {
            self
        } else {
            Self::IDENTITY
        }
    }

    fn compose(self, other: Self) -> Self {
        let composed = Self {
            v0: self.v0 * other.v0 + self.v1 * other.v3,
            v1: self.v0 * other.v1 + self.v1 * other.v4,
            x0_mm: self.v0 * other.x0_mm + self.v1 * other.y0_mm + self.x0_mm,
            v3: self.v3 * other.v0 + self.v4 * other.v3,
            v4: self.v3 * other.v1 + self.v4 * other.v4,
            y0_mm: self.v3 * other.x0_mm + self.v4 * other.y0_mm + self.y0_mm,
        };
        composed.sanitized()
    }

    fn transform_point_cdr_mm(self, x_mm: f64, y_mm: f64) -> (f64, f64) {
        let tx = self.v0 * x_mm + self.v1 * y_mm + self.x0_mm;
        let ty = self.v3 * x_mm + self.v4 * y_mm + self.y0_mm;
        (sanitize_f64(tx, 0.0), sanitize_f64(ty, 0.0))
    }

    fn to_page_mm_affine(self, page_w_mm: f64, page_h_mm: f64) -> Affine2D {
        let m = self.sanitized();
        let xf = Affine2D::new(
            m.v0,
            -m.v3,
            m.v1,
            -m.v4,
            m.x0_mm + page_w_mm * 0.5,
            page_h_mm * 0.5 - m.y0_mm,
        );
        if xf.is_finite() {
            xf
        } else {
            Affine2D::IDENTITY
        }
    }
}

fn convert_cdr_native_rect(
    rect: CdrNativeRect,
    m: CdrNativeMatrix,
    page_w_mm: f64,
    page_h_mm: f64,
) -> (ParametricShape, Affine2D) {
    let m = m.sanitized();
    let sx = {
        let raw = m.v0.hypot(m.v3);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let sy = {
        let raw = m.v1.hypot(m.v4);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let world_w = sanitize_f64((rect.local_w_mm * sx).abs(), 0.0).max(0.0);
    let world_h = sanitize_f64((rect.local_h_mm * sy).abs(), 0.0).max(0.0);
    let max_r = sanitize_f64(0.5 * world_w.min(world_h), 0.0).max(0.0);

    let mut radii = rect.corners.map(|c| {
        let r = if rect.relative_corners {
            c * max_r
        } else {
            c * sx.min(sy)
        };
        sanitize_f64(r, 0.0).clamp(0.0, max_r)
    });

    if m.v1.abs() < 1e-6 && m.v3.abs() < 1e-6 {
        if m.v0 < 0.0 {
            radii.swap(0, 1);
            radii.swap(2, 3);
        }
        if m.v4 < 0.0 {
            radii.swap(0, 3);
            radii.swap(1, 2);
        }
        let page_xf = m.to_page_mm_affine(page_w_mm, page_h_mm);
        let p0 = page_xf.transform_point(Point2D::new(0.0, 0.0));
        let p1 = page_xf.transform_point(Point2D::new(rect.local_w_mm, rect.local_h_mm));
        let min_x = sanitize_f64(p0.x.min(p1.x), 0.0);
        let min_y = sanitize_f64(p0.y.min(p1.y), 0.0);
        let lock_corners = radii.iter().all(|&r| (r - radii[0]).abs() < 1e-3);
        let shape = ParametricShape::Rectangle {
            width: world_w,
            height: world_h,
            corner_radii: radii,
            corner_style: rect.corner_style,
            lock_corners,
        };
        return (shape, Affine2D::translate(min_x, min_y));
    }

    let page_xf = m.to_page_mm_affine(page_w_mm, page_h_mm);
    let to_cdr_local = Affine2D::new(1.0 / sx, 0.0, 0.0, -1.0 / sy, 0.0, rect.local_h_mm);
    let rect_xf = page_xf.compose(&to_cdr_local);
    let rect_xf = if rect_xf.is_finite() {
        rect_xf
    } else {
        Affine2D::IDENTITY
    };
    let lock_corners = radii.iter().all(|&r| (r - radii[0]).abs() < 1e-3);
    let shape = ParametricShape::Rectangle {
        width: world_w,
        height: world_h,
        corner_radii: radii,
        corner_style: rect.corner_style,
        lock_corners,
    };
    (shape, rect_xf)
}

fn convert_cdr_native_ellipse(
    ell: CdrNativeEllipse,
    m: CdrNativeMatrix,
    page_w_mm: f64,
    page_h_mm: f64,
) -> (ParametricShape, Affine2D) {
    let m = m.sanitized();
    let sx = {
        let raw = m.v0.hypot(m.v3);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let sy = {
        let raw = m.v1.hypot(m.v4);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let rx = sanitize_f64((ell.local_w_mm * 0.5 * sx).abs(), 0.0).max(0.0);
    let ry = sanitize_f64((ell.local_h_mm * 0.5 * sy).abs(), 0.0).max(0.0);
    let (cx_cdr, cy_cdr) = m.transform_point_cdr_mm(ell.local_w_mm * 0.5, ell.local_h_mm * 0.5);
    let cx = sanitize_f64(cx_cdr + page_w_mm * 0.5, 0.0);
    let cy = sanitize_f64(page_h_mm * 0.5 - cy_cdr, 0.0);

    let shape = ParametricShape::Ellipse {
        rx,
        ry,
        mode: ell.mode,
        start_deg: sanitize_f64(ell.start_deg, 0.0),
        end_deg: sanitize_f64(ell.end_deg, 360.0),
    };
    if m.v1.abs() < 1e-6 && m.v3.abs() < 1e-6 {
        return (shape, Affine2D::translate(cx, cy));
    }
    let rot_a = sanitize_f64(m.v0 / sx, 1.0);
    let rot_b = sanitize_f64(-m.v3 / sx, 0.0);
    let rot_c = sanitize_f64(m.v1 / sy, 0.0);
    let rot_d = sanitize_f64(-m.v4 / sy, 1.0);
    let xf = Affine2D::new(rot_a, rot_b, rot_c, rot_d, cx, cy);
    (
        shape,
        if xf.is_finite() {
            xf
        } else {
            Affine2D::translate(cx, cy)
        },
    )
}

pub(super) fn is_cdr_native_trfd(data: &[u8]) -> bool {
    if data.len() < 56 {
        return false;
    }
    let chunk_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let num_args = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let args_off = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    chunk_len == data.len()
        && (1..=64).contains(&num_args)
        && args_off >= 12
        && args_off
            .checked_add(num_args.saturating_mul(4))
            .is_some_and(|end| end <= data.len())
}

fn parse_cdr_native_trfd(data: &[u8]) -> CdrResult<CdrNativeMatrix> {
    let mut r = ByteReader::new(data);
    let _chunk_len = r.read_u32_le("trfd len")?;
    let num_args = usize::try_from(r.read_u32_le("trfd num_args")?)
        .map_err(|_| CdrError::ArithmeticOverflow("trfd num_args"))?;
    let args_off = usize::try_from(r.read_u32_le("trfd args_off")?)
        .map_err(|_| CdrError::ArithmeticOverflow("trfd args_off"))?;

    let mut offsets = Vec::with_capacity(num_args);
    for i in 0..num_args {
        let p = args_off + i * 4;
        if p + 4 > data.len() {
            break;
        }
        offsets.push(u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize);
    }

    let mut acc = CdrNativeMatrix::IDENTITY;
    for start in offsets {
        if start >= data.len() {
            continue;
        }
        let matrix_off = if start.checked_add(64).is_some_and(|e| e <= data.len())
            && u16::from_le_bytes([data[start + 8], data[start + 9]]) == 0x0008
        {
            Some(start + 16)
        } else if start.checked_add(56).is_some_and(|e| e <= data.len())
            && u16::from_le_bytes([data[start], data[start + 1]]) == 0x0008
        {
            Some(start + 8)
        } else {
            None
        };

        if let Some(moff) = matrix_off {
            if moff.checked_add(48).is_some_and(|e| e <= data.len()) {
                let mut mr = ByteReader::new(&data[moff..moff + 48]);
                let v0 = sanitize_f64(mr.read_f64_le("trfd v0")?, 1.0);
                let v1 = sanitize_f64(mr.read_f64_le("trfd v1")?, 0.0);
                let x0 = sanitize_f64(mr.read_f64_le("trfd x0")?, 0.0);
                let v3 = sanitize_f64(mr.read_f64_le("trfd v3")?, 0.0);
                let v4 = sanitize_f64(mr.read_f64_le("trfd v4")?, 1.0);
                let y0 = sanitize_f64(mr.read_f64_le("trfd y0")?, 0.0);
                let step = CdrNativeMatrix {
                    v0,
                    v1,
                    x0_mm: x0 / coree_core::math::CDR_UNITS_PER_MM,
                    v3,
                    v4,
                    y0_mm: y0 / coree_core::math::CDR_UNITS_PER_MM,
                }
                .sanitized();
                acc = step.compose(acc);
            }
        }
    }

    Ok(acc)
}

pub(super) fn parse_synthetic_trfd_chunk(data: &[u8]) -> CdrResult<Affine2D> {
    let mut r = ByteReader::new(data);
    if data.len() >= 52 {
        let tag = r.read_u16_le("trfd tag")?;
        let _reserved = r.read_u16_le("trfd reserved")?;
        if tag != 0x0008 {
            r.seek(0, "trfd rewind")?;
        }
    }
    let a = sanitize_f64(r.read_f64_le("trfd a")?, 1.0);
    let b = sanitize_f64(r.read_f64_le("trfd b")?, 0.0);
    let c = sanitize_f64(r.read_f64_le("trfd c")?, 0.0);
    let d = sanitize_f64(r.read_f64_le("trfd d")?, 1.0);
    let tx_cdr = sanitize_f64(r.read_f64_le("trfd tx")?, 0.0);
    let ty_cdr = sanitize_f64(r.read_f64_le("trfd ty")?, 0.0);
    Ok(Affine2D::new(
        a,
        b,
        c,
        d,
        tx_cdr / coree_core::math::CDR_UNITS_PER_MM,
        ty_cdr / coree_core::math::CDR_UNITS_PER_MM,
    ))
}

pub(super) fn parse_lens_chunk(data: &[u8]) -> CdrResult<LensConfig> {
    let mut r = ByteReader::new(data);
    let lens_type = r.read_u8("lens type")?;
    let flags = r.read_u8("lens flags")?;
    let frozen = (flags & 0x01) != 0;
    let remove_face = (flags & 0x02) != 0;
    let has_viewpoint = (flags & 0x04) != 0;
    let has_frozen_snapshot = (flags & 0x08) != 0;
    let viewpoint = if has_viewpoint {
        let vx = cdr32_to_mm(r.read_i32_le("lens vp x")?);
        let vy = cdr32_to_mm(r.read_i32_le("lens vp y")?);
        Some(Point2D::new(vx, vy))
    } else {
        None
    };

    let kind = match lens_type {
        1 => LensKind::Brighten {
            rate_pct: sanitize_f32(r.read_f32_le("lens brighten rate")?, 0.0),
        },
        2 => {
            let rate_pct = sanitize_f32(r.read_f32_le("lens color_add rate")?, 50.0);
            let color = read_tagged_color(&mut r, "lens color_add color")?;
            LensKind::ColorAdd { color, rate_pct }
        }
        3 => {
            let rate_pct = sanitize_f32(r.read_f32_le("lens color_limit rate")?, 50.0);
            let color = read_tagged_color(&mut r, "lens color_limit color")?;
            LensKind::ColorLimit { color, rate_pct }
        }
        4 => {
            let palette_mode = match r.read_u8("lens color_map mode")? {
                1 => ColorMapMode::ForwardRainbow,
                2 => ColorMapMode::ReverseRainbow,
                _ => ColorMapMode::Direct,
            };
            let from_color = read_tagged_color(&mut r, "lens color_map from")?;
            let to_color = read_tagged_color(&mut r, "lens color_map to")?;
            LensKind::CustomColorMap {
                from_color,
                to_color,
                palette_mode,
            }
        }
        5 => LensKind::FishEye {
            rate_pct: sanitize_f32(r.read_f32_le("lens fisheye rate")?, 100.0),
        },
        6 => LensKind::HeatMap {
            palette_rotation_deg: sanitize_f32(r.read_f32_le("lens heatmap rot")?, 0.0),
        },
        7 => LensKind::Invert,
        8 => LensKind::Magnify {
            amount: sanitize_f32(r.read_f32_le("lens magnify amount")?, 2.0),
        },
        9 => LensKind::TintedGrayscale {
            tint: read_tagged_color(&mut r, "lens tinted_gray color")?,
        },
        10 => {
            let rate_pct = sanitize_f32(r.read_f32_le("lens transparency rate")?, 50.0);
            let color = read_tagged_color(&mut r, "lens transparency color")?;
            LensKind::Transparency { color, rate_pct }
        }
        11 => {
            let has_outl = r.read_u8("lens wf has_outline")? != 0;
            let override_outline = if has_outl {
                Some(read_tagged_color(&mut r, "lens wf outline")?)
            } else {
                None
            };
            let has_fill = r.read_u8("lens wf has_fill")? != 0;
            let override_fill = if has_fill {
                Some(read_tagged_color(&mut r, "lens wf fill")?)
            } else {
                None
            };
            LensKind::Wireframe {
                override_outline,
                override_fill,
            }
        }
        other => {
            return Err(CdrError::InvalidChunk {
                fourcc: "lens".to_owned(),
                reason: format!("unknown lens type {other}"),
            });
        }
    };

    let frozen_snapshot = if has_frozen_snapshot && r.remaining() >= 4 {
        let snap_len = r.read_u32_le("lens frozen_snapshot len")? as usize;
        if snap_len > 0 && snap_len <= r.remaining() && snap_len <= 32 * 1024 * 1024 {
            Some(r.read_bytes(snap_len, "lens frozen_snapshot")?.to_vec())
        } else {
            None
        }
    } else {
        None
    };

    Ok(LensConfig {
        kind: kind.clamped(),
        frozen,
        viewpoint,
        remove_face,
        frozen_snapshot,
    })
}
