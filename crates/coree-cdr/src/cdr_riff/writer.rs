// SPDX-License-Identifier: MIT OR Apache-2.0

//! RIFF CDR writer (`20B` reversed `loda`, non-flattening payloads, real `DISP` thumbnail).

use super::dib::encode_dib_rgba;
use super::fild_outl::{encode_fild_chunk, encode_outl_chunk};
use super::loda::{
    encode_loda_args, encode_loda_curve_arg, encode_obj_props, encode_shape_loda_arg,
    PowerClipLodaMeta, CDR_CTYPE_ARTISTIC_TEXT, CDR_CTYPE_BITMAP, CDR_CTYPE_CURVE,
    CDR_CTYPE_ELLIPSE, CDR_CTYPE_GROUP_OR_LAYER, CDR_CTYPE_PARAGRAPH_TEXT, CDR_CTYPE_POLYGON,
    CDR_CTYPE_RECTANGLE, LODA_ARG_ARTISTIC_MEDIA, LODA_ARG_BITMAP_REF, LODA_ARG_BSPLINE,
    LODA_ARG_CLONE, LODA_ARG_COLOR_STYLE_LINK, LODA_ARG_CONNECTOR, LODA_ARG_CURVE,
    LODA_ARG_DIMENSION, LODA_ARG_EFFECTS, LODA_ARG_LAYER_PROPS, LODA_ARG_NAME,
    LODA_ARG_OBJECT_STYLE_LINK, LODA_ARG_OBJ_PROPS, LODA_ARG_PERSPECTIVE_3X3,
    LODA_ARG_POWERCLIP_META, LODA_ARG_SOURCE_OID, LODA_ARG_SYMBOL_INSTANCE, LODA_ARG_TABLE,
    LODA_ARG_TEXT_META, LODA_ARG_TEXT_ON_PATH,
};
use super::parser::{GuidPayload, StlyPayload, SymbPayload, CBMP_MAGIC};
use super::text_style::{encode_txsm_artistic, encode_txsm_paragraph};
use super::{RiffWriteOptions, RiffWriteOutput};
use crate::coord::{mm_to_cdr32, write_tagged_color};
use crate::error::{CdrError, CdrResult};
use crate::limits::CodecLimits;
use crate::riff::{parse_list_body, write_riff, RiffDocument, RiffEndian, RiffNode};
use coree_core::{
    math::sanitize_f32, math::sanitize_f64, Affine2D, BezierPath, Color, ColorMapMode, ColorMode,
    ColorSwatch, Document, Fill, ImageAssetId, Layer, LensConfig, LensKind, MasterLayerScope,
    NodePayload, ObjectId, Page, ParametricShape, RasterAsset, UnknownCdrChunk,
};
use std::collections::HashMap;

pub(crate) fn write_cdr_riff_with_options_inner(
    doc: &Document,
    opts: RiffWriteOptions,
) -> CdrResult<RiffWriteOutput> {
    let mut external_bitmaps: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut asset_id_to_num: HashMap<ImageAssetId, u32> = HashMap::new();
    let mut bmp_nodes: Vec<RiffNode> = Vec::new();

    for (idx, (&aid, asset)) in doc.assets().iter().enumerate() {
        let num_id = (idx as u32).saturating_add(1);
        asset_id_to_num.insert(aid, num_id);
        let payload = encode_bmp_chunk_payload(num_id, asset)?;
        if opts.externalize_bitmaps {
            external_bitmaps.push((num_id, payload));
        } else {
            bmp_nodes.push(RiffNode::chunk(*b"bmp ", payload));
        }
    }

    let mut doc_children: Vec<RiffNode> = Vec::new();

    // Document mcfg
    let (def_w, def_h) = doc
        .pages()
        .first()
        .map_or((210.0, 297.0), |p| (p.width_mm, p.height_mm));
    doc_children.push(RiffNode::chunk(
        *b"mcfg",
        encode_doc_mcfg(def_w, def_h, doc.color_mode(), doc.dpi(), doc.title()),
    ));

    // Embedded ICC profile
    if let Some(icc) = doc.custom_icc_profile() {
        doc_children.push(RiffNode::chunk(*b"iccp", icc.to_vec()));
    }

    // Palette chunk (`plte`)
    if !doc.document_palette().swatches.is_empty() {
        doc_children.push(RiffNode::chunk(
            *b"plte",
            encode_plte_chunk(&doc.document_palette().swatches),
        ));
    }

    // Symbol definitions (`symb`)
    if !doc.symbols().is_empty() {
        let symbols = doc
            .symbols()
            .values()
            .cloned()
            .map(|mut sym| {
                for sub in &mut sym.objects {
                    remap_scene_object_assets_to_placeholders(sub, &asset_id_to_num);
                }
                sym
            })
            .collect();
        let sp = SymbPayload { symbols };
        if let Ok(bytes) = serde_json::to_vec(&sp) {
            doc_children.push(RiffNode::chunk(*b"symb", bytes));
        }
    }

    // Color & object styles (`stly`)
    if !doc.color_styles().is_empty() || !doc.object_styles().is_empty() {
        let object_styles = doc
            .object_styles()
            .iter()
            .cloned()
            .map(|mut os| {
                if let Some(Fill::BitmapPattern {
                    ref mut image_id, ..
                }) = os.fill
                {
                    if let Some(&num_id) = asset_id_to_num.get(image_id) {
                        *image_id = cdr_placeholder_asset_id(num_id);
                    }
                }
                os
            })
            .collect();
        let st = StlyPayload {
            color_styles: doc.color_styles().to_vec(),
            object_styles,
        };
        if let Ok(bytes) = serde_json::to_vec(&st) {
            doc_children.push(RiffNode::chunk(*b"stly", bytes));
        }
    }

    // Guides, Grid, and Perspective Grid (`guid`)
    if !doc.guides().is_empty()
        || doc.grid().visible
        || doc.grid().snap
        || doc.perspective_grid().enabled
    {
        let gp = GuidPayload {
            guides: doc.guides().to_vec(),
            grid: doc.grid(),
            perspective_grid: doc.perspective_grid().clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&gp) {
            doc_children.push(RiffNode::chunk(*b"guid", bytes));
        }
    }

    // Embedded bitmaps list (`LIST bmps`)
    if !bmp_nodes.is_empty() {
        doc_children.push(RiffNode::list(*b"bmps", bmp_nodes));
    }

    // Pages (`LIST page`)
    for page in doc.pages() {
        let mut page_children = Vec::new();
        page_children.push(RiffNode::chunk(*b"mcfg", encode_page_mcfg(page)));
        for layer in &page.layers {
            let mut layer_children = Vec::new();
            layer_children.push(RiffNode::chunk(*b"layr", encode_layer_meta(layer)));
            layer_children.push(RiffNode::chunk(*b"loda", encode_layer_loda(layer)));
            for &oid in &layer.objects {
                if let Some(node) = encode_scene_object(doc, oid, &asset_id_to_num, 0)? {
                    layer_children.push(node);
                }
            }
            for unk in &layer.unknown_chunks {
                layer_children.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
            }
            page_children.push(RiffNode::list(*b"layr", layer_children));
        }
        for unk in &page.unknown_chunks {
            page_children.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
        }
        doc_children.push(RiffNode::list(*b"page", page_children));
    }

    // Re-emit document-level Unknown-Chunk Passthrough verbatim!
    for unk in doc.unknown_doc_chunks() {
        doc_children.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
    }

    let mut root_children = Vec::new();
    root_children.push(RiffNode::chunk(
        *b"vrsn",
        opts.version.to_le_bytes().to_vec(),
    ));

    // Real rendered 64x64 miniature preview DIB of page 0 in `DISP`
    let thumb_rgba = render_doc_mini_thumbnail_rgba(doc, 64, 64);
    if let Ok(preview_dib) = encode_dib_rgba(64, 64, &thumb_rgba) {
        root_children.push(RiffNode::chunk(*b"DISP", preview_dib));
    }

    let doc_list = RiffNode::list(*b"doc ", doc_children);
    if opts.compress_doc {
        root_children.push(RiffNode::cmpr_list(vec![doc_list]));
    } else {
        root_children.push(doc_list);
    }

    let riff_doc = RiffDocument {
        endian: RiffEndian::Little,
        form_type: opts.form_type,
        children: root_children,
    };
    let riff_bytes = write_riff(&riff_doc)?;

    Ok(RiffWriteOutput {
        riff_bytes,
        external_bitmaps,
    })
}

/// Renders a 2D miniature preview thumbnail of page 0 objects for `DISP` and `previews/thumbnail.png`.
pub(crate) fn render_doc_mini_thumbnail_rgba(doc: &Document, w: u32, h: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for px in rgba.as_chunks_mut::<4>().0 {
        *px = [232, 235, 240, 255];
    }
    let Some(page) = doc.pages().first() else {
        return rgba;
    };
    let pw = page.width_mm.max(1.0);
    let ph = page.height_mm.max(1.0);
    let pad = 4.0_f64;
    let avail_w = (f64::from(w) - 2.0 * pad).max(4.0);
    let avail_h = (f64::from(h) - 2.0 * pad).max(4.0);
    let scale = (avail_w / pw).min(avail_h / ph);
    let sheet_w = (pw * scale).round().max(2.0);
    let sheet_h = (ph * scale).round().max(2.0);
    let ox = ((f64::from(w) - sheet_w) * 0.5).floor();
    let oy = ((f64::from(h) - sheet_h) * 0.5).floor();

    let paint_rect = |buf: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, col: [u8; 4]| {
        let a = f32::from(col[3]) / 255.0;
        let inv = 1.0 - a;
        let sx0 = x0.max(0) as u32;
        let sy0 = y0.max(0) as u32;
        let sx1 = (x1.max(0) as u32).min(w);
        let sy1 = (y1.max(0) as u32).min(h);
        for py in sy0..sy1 {
            for px_x in sx0..sx1 {
                let idx = ((py * w + px_x) as usize) * 4;
                buf[idx] = (f32::from(col[0]) * a + f32::from(buf[idx]) * inv).round() as u8;
                buf[idx + 1] =
                    (f32::from(col[1]) * a + f32::from(buf[idx + 1]) * inv).round() as u8;
                buf[idx + 2] =
                    (f32::from(col[2]) * a + f32::from(buf[idx + 2]) * inv).round() as u8;
                buf[idx + 3] = 255;
            }
        }
    };

    let px0 = ox as i32;
    let py0 = oy as i32;
    let px1 = (ox + sheet_w) as i32;
    let py1 = (oy + sheet_h) as i32;
    paint_rect(
        &mut rgba,
        px0 - 1,
        py0 - 1,
        px1 + 1,
        py1 + 1,
        [160, 166, 178, 255],
    );
    paint_rect(&mut rgba, px0, py0, px1, py1, [255, 255, 255, 255]);

    for layer in &page.layers {
        if !layer.visible {
            continue;
        }
        for &oid in &layer.objects {
            let Some(obj) = doc.object(oid) else {
                continue;
            };
            if !obj.visible {
                continue;
            }
            let Some(bb) = doc.object_bounds(oid) else {
                continue;
            };
            let fill_c = match &obj.fill {
                Fill::Solid(c) => Some(c.clone()),
                Fill::LinearGradient { stops, .. }
                | Fill::RadialGradient { stops, .. }
                | Fill::ConicalGradient { stops, .. }
                | Fill::SquareGradient { stops, .. } => stops.first().map(|s| s.color.clone()),
                Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => Some(fg.clone()),
                Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
                Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => None,
            }
            .or_else(|| obj.outline.is_visible().then(|| obj.outline.color.clone()))
            .unwrap_or_else(|| match obj.payload {
                NodePayload::Raster(_) => Color::rgb(0.35, 0.55, 0.78),
                _ => Color::rgb(0.25, 0.29, 0.35),
            });
            let [rf, gf, bf, af] = fill_c.to_rgba_f32();
            let col = [
                (rf * 255.0).round().clamp(0.0, 255.0) as u8,
                (gf * 255.0).round().clamp(0.0, 255.0) as u8,
                (bf * 255.0).round().clamp(0.0, 255.0) as u8,
                ((af * obj.opacity.clamp(0.0, 1.0)) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8,
            ];
            let rx0 = (ox + bb.min_x * scale).floor() as i32;
            let ry0 = (oy + bb.min_y * scale).floor() as i32;
            let rx1 = ((ox + bb.max_x * scale).ceil() as i32).max(rx0 + 1);
            let ry1 = ((oy + bb.max_y * scale).ceil() as i32).max(ry0 + 1);
            paint_rect(&mut rgba, rx0, ry0, rx1, ry1, col);
        }
    }
    rgba
}

fn encode_bmp_chunk_payload(bmp_id: u32, asset: &RasterAsset) -> CdrResult<Vec<u8>> {
    let dib = encode_dib_rgba(asset.width, asset.height, &asset.rgba_bytes)?;
    let dpi_x100 = (sanitize_f32(asset.source_dpi, 300.0).max(1.0) * 100.0).round() as u32;
    let cm_byte = if asset.color_mode == ColorMode::Rgb {
        1u8
    } else {
        0u8
    };
    let mut out = Vec::with_capacity(13usize.saturating_add(dib.len()));
    out.extend_from_slice(CBMP_MAGIC);
    out.extend_from_slice(&bmp_id.to_le_bytes());
    out.extend_from_slice(&dpi_x100.to_le_bytes());
    out.push(cm_byte);
    out.extend_from_slice(&dib);
    Ok(out)
}

fn encode_doc_mcfg(w_mm: f64, h_mm: f64, mode: ColorMode, dpi: f32, title: &str) -> Vec<u8> {
    let tb = title.as_bytes();
    let tlen = tb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(15 + tlen);
    out.extend_from_slice(&mm_to_cdr32(w_mm).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(h_mm).to_le_bytes());
    out.push(if mode == ColorMode::Rgb { 1 } else { 0 });
    out.extend_from_slice(&sanitize_f32(dpi, 300.0).to_le_bytes());
    out.extend_from_slice(&(tlen as u16).to_le_bytes());
    out.extend_from_slice(&tb[..tlen]);
    out
}

fn encode_plte_chunk(swatches: &[ColorSwatch]) -> Vec<u8> {
    let count = swatches.len().min(u16::MAX as usize);
    let mut out = Vec::new();
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for sw in &swatches[..count] {
        let nb = sw.name.as_bytes();
        let nlen = nb.len().min(u16::MAX as usize);
        out.extend_from_slice(&(nlen as u16).to_le_bytes());
        out.extend_from_slice(&nb[..nlen]);
        write_tagged_color(&mut out, &sw.color);
    }
    out
}

fn encode_page_mcfg(page: &Page) -> Vec<u8> {
    let nb = page.name.as_bytes();
    let nlen = nb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(15 + nlen);
    out.extend_from_slice(&mm_to_cdr32(page.width_mm).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(page.height_mm).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(page.bleed_mm).to_le_bytes());
    out.push(u8::from(page.facing_pages));
    out.extend_from_slice(&(nlen as u16).to_le_bytes());
    out.extend_from_slice(&nb[..nlen]);
    out
}

fn layer_flags_and_scope(layer: &Layer) -> (u8, u8) {
    let mut flags = 0u8;
    if layer.visible {
        flags |= 0x01;
    }
    if layer.locked {
        flags |= 0x02;
    }
    if layer.printable {
        flags |= 0x04;
    }
    if layer.is_cut_layer {
        flags |= 0x08;
    }
    let ms = match layer.master_scope {
        MasterLayerScope::None => 0u8,
        MasterLayerScope::AllPages => 1,
        MasterLayerScope::OddPages => 2,
        MasterLayerScope::EvenPages => 3,
    };
    (flags, ms)
}

fn encode_layer_meta(layer: &Layer) -> Vec<u8> {
    let (flags, ms) = layer_flags_and_scope(layer);
    let nb = layer.name.as_bytes();
    let nlen = nb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(4 + nlen);
    out.push(flags);
    out.push(ms);
    out.extend_from_slice(&(nlen as u16).to_le_bytes());
    out.extend_from_slice(&nb[..nlen]);
    out
}

fn encode_layer_loda(layer: &Layer) -> Vec<u8> {
    let (flags, ms) = layer_flags_and_scope(layer);
    let ctype = if layer.is_desktop_layer {
        super::loda::CDR_CTYPE_LAYER_DESKTOP
    } else {
        CDR_CTYPE_GROUP_OR_LAYER
    };
    let args = vec![
        (LODA_ARG_NAME, layer.name.as_bytes().to_vec()),
        (LODA_ARG_LAYER_PROPS, vec![flags, ms]),
    ];
    encode_loda_args(ctype, &args)
}

fn encode_scene_object(
    doc: &Document,
    oid: ObjectId,
    bmp_map: &HashMap<ImageAssetId, u32>,
    depth: usize,
) -> CdrResult<Option<RiffNode>> {
    if depth > crate::limits::MAX_TREE_DEPTH {
        return Err(CdrError::DepthLimitExceeded {
            depth,
            limit: crate::limits::MAX_TREE_DEPTH,
        });
    }
    let Some(obj) = doc.object(oid) else {
        return Ok(None);
    };

    if let NodePayload::Group(children) = &obj.payload {
        let mut grp_nodes = Vec::new();
        let mut g_args = vec![
            (LODA_ARG_OBJ_PROPS, encode_obj_props(obj)),
            (LODA_ARG_SOURCE_OID, obj.id.0.to_le_bytes().to_vec()),
        ];
        if !obj.effects.is_empty() {
            if let Ok(eb) = serde_json::to_vec(&obj.effects) {
                g_args.push((LODA_ARG_EFFECTS, eb));
            }
        }
        if let Some(sid) = obj.style_id {
            g_args.push((LODA_ARG_OBJECT_STYLE_LINK, sid.0.as_bytes().to_vec()));
        }
        if let Some(persp) = obj.perspective {
            if let Ok(pb) = serde_json::to_vec(&persp) {
                g_args.push((LODA_ARG_PERSPECTIVE_3X3, pb));
            }
        }
        if obj.color_style_id.is_some() || obj.color_style_slot.is_some() {
            if let Ok(cb) = serde_json::to_vec(&(obj.color_style_id, obj.color_style_slot)) {
                g_args.push((LODA_ARG_COLOR_STYLE_LINK, cb));
            }
        }
        let mut deferred_unknown_chunks = Vec::new();
        for unk in &obj.unknown_chunks {
            if &unk.fourcc == b"LODA" && unk.raw_bytes.len() >= 4 {
                let arg_typ = u32::from_le_bytes([
                    unk.raw_bytes[0],
                    unk.raw_bytes[1],
                    unk.raw_bytes[2],
                    unk.raw_bytes[3],
                ]);
                g_args.push((arg_typ, unk.raw_bytes[4..].to_vec()));
            } else {
                deferred_unknown_chunks.push(unk);
            }
        }
        grp_nodes.push(RiffNode::chunk(
            *b"loda",
            encode_loda_args(CDR_CTYPE_GROUP_OR_LAYER, &g_args),
        ));
        if obj.transform != Affine2D::IDENTITY {
            grp_nodes.push(RiffNode::chunk(*b"trfd", encode_trfd_chunk(&obj.transform)));
        }
        if let Some(ref lens) = obj.lens {
            grp_nodes.push(RiffNode::chunk(*b"lens", encode_lens_chunk(lens)));
        }
        for &cid in children {
            if let Some(cn) = encode_scene_object(doc, cid, bmp_map, depth + 1)? {
                grp_nodes.push(cn);
            }
        }
        for unk in deferred_unknown_chunks {
            grp_nodes.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
        }
        return Ok(Some(RiffNode::list(*b"grp ", grp_nodes)));
    }

    let mut obj_nodes: Vec<RiffNode> = Vec::new();
    let mut loda_args: Vec<(u32, Vec<u8>)> = Vec::new();
    loda_args.push((LODA_ARG_OBJ_PROPS, encode_obj_props(obj)));
    loda_args.push((LODA_ARG_SOURCE_OID, obj.id.0.to_le_bytes().to_vec()));

    if !obj.effects.is_empty() {
        if let Ok(eb) = serde_json::to_vec(&obj.effects) {
            loda_args.push((LODA_ARG_EFFECTS, eb));
        }
    }
    if let Some(sid) = obj.style_id {
        loda_args.push((LODA_ARG_OBJECT_STYLE_LINK, sid.0.as_bytes().to_vec()));
    }
    if let Some(persp) = obj.perspective {
        if let Ok(pb) = serde_json::to_vec(&persp) {
            loda_args.push((LODA_ARG_PERSPECTIVE_3X3, pb));
        }
    }
    if obj.color_style_id.is_some() || obj.color_style_slot.is_some() {
        if let Ok(cb) = serde_json::to_vec(&(obj.color_style_id, obj.color_style_slot)) {
            loda_args.push((LODA_ARG_COLOR_STYLE_LINK, cb));
        }
    }

    let chunk_type;
    match &obj.payload {
        NodePayload::Path(path) => {
            chunk_type = CDR_CTYPE_CURVE;
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(path)));
        }
        NodePayload::Shape(shape) => {
            chunk_type = match shape {
                ParametricShape::Rectangle { .. } => CDR_CTYPE_RECTANGLE,
                ParametricShape::Ellipse { .. } => CDR_CTYPE_ELLIPSE,
                ParametricShape::Polygon { .. } | ParametricShape::Star { .. } => CDR_CTYPE_POLYGON,
                _ => CDR_CTYPE_CURVE,
            };
            encode_shape_loda_arg(shape, &mut loda_args);
        }
        NodePayload::ArtisticText(text) => {
            chunk_type = CDR_CTYPE_ARTISTIC_TEXT;
            obj_nodes.push(RiffNode::chunk(*b"txsm", encode_txsm_artistic(text)));
        }
        NodePayload::ParagraphText(para) => {
            chunk_type = CDR_CTYPE_PARAGRAPH_TEXT;
            let mut tm = Vec::with_capacity(if para.linked_next_frame.is_some() {
                26
            } else {
                17
            });
            tm.push(1u8);
            tm.extend_from_slice(&mm_to_cdr32(para.frame_size.x).to_le_bytes());
            tm.extend_from_slice(&mm_to_cdr32(para.frame_size.y).to_le_bytes());
            tm.extend_from_slice(&para.columns.to_le_bytes());
            tm.extend_from_slice(&mm_to_cdr32(para.column_gutter_mm).to_le_bytes());
            if let Some(next_id) = para.linked_next_frame {
                tm.push(1u8);
                tm.extend_from_slice(&next_id.0.to_le_bytes());
            }
            loda_args.push((LODA_ARG_TEXT_META, tm));
            obj_nodes.push(RiffNode::chunk(*b"txsm", encode_txsm_paragraph(para)));
        }
        NodePayload::TextOnPath(top) => {
            chunk_type = CDR_CTYPE_ARTISTIC_TEXT;
            if let Ok(top_bytes) = serde_json::to_vec(top) {
                loda_args.push((LODA_ARG_TEXT_ON_PATH, top_bytes));
            }
            obj_nodes.push(RiffNode::chunk(*b"txsm", encode_txsm_artistic(&top.text)));
        }
        NodePayload::Raster(r) => {
            chunk_type = CDR_CTYPE_BITMAP;
            let bmp_id = bmp_map.get(&r.asset_id).copied().unwrap_or(0);
            let mut br = Vec::with_capacity(29);
            br.extend_from_slice(&bmp_id.to_le_bytes());
            br.extend_from_slice(&mm_to_cdr32(r.width_mm).to_le_bytes());
            br.extend_from_slice(&mm_to_cdr32(r.height_mm).to_le_bytes());
            if let Some(cr) = r.crop_rect {
                br.push(1);
                br.extend_from_slice(&mm_to_cdr32(cr.min_x).to_le_bytes());
                br.extend_from_slice(&mm_to_cdr32(cr.min_y).to_le_bytes());
                br.extend_from_slice(&mm_to_cdr32(cr.max_x).to_le_bytes());
                br.extend_from_slice(&mm_to_cdr32(cr.max_y).to_le_bytes());
            } else {
                br.push(0);
            }
            loda_args.push((LODA_ARG_BITMAP_REF, br));
        }
        NodePayload::PowerClip {
            container,
            container_parametric,
            lock_contents,
            contents,
        } => {
            chunk_type = CDR_CTYPE_CURVE;
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(container)));
            let pm = PowerClipLodaMeta {
                container_parametric: container_parametric.clone(),
                lock_contents: *lock_contents,
            };
            if let Ok(pm_bytes) = serde_json::to_vec(&pm) {
                loda_args.push((LODA_ARG_POWERCLIP_META, pm_bytes));
            }
            let mut pwr_nodes = Vec::new();
            for &cid in contents {
                if let Some(cn) = encode_scene_object(doc, cid, bmp_map, depth + 1)? {
                    pwr_nodes.push(cn);
                }
            }
            obj_nodes.push(RiffNode::list(*b"pwrp", pwr_nodes));
        }
        NodePayload::Table(t) => {
            chunk_type = CDR_CTYPE_CURVE;
            let mut t_clone = t.clone();
            for cell in &mut t_clone.cells {
                if let Fill::BitmapPattern {
                    ref mut image_id, ..
                } = cell.fill
                {
                    if let Some(&num_id) = bmp_map.get(image_id) {
                        *image_id = cdr_placeholder_asset_id(num_id);
                    }
                }
            }
            if let Ok(tb) = serde_json::to_vec(&t_clone) {
                loda_args.push((LODA_ARG_TABLE, tb));
            }
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&t.to_grid_paths())));
        }
        NodePayload::Dimension(d) => {
            chunk_type = CDR_CTYPE_CURVE;
            if let Ok(db) = serde_json::to_vec(d) {
                loda_args.push((LODA_ARG_DIMENSION, db));
            }
            loda_args.push((
                LODA_ARG_CURVE,
                encode_loda_curve_arg(&BezierPath::from_rect(d.bounding_box())),
            ));
        }
        NodePayload::Connector(c) => {
            chunk_type = CDR_CTYPE_CURVE;
            if let Ok(cb) = serde_json::to_vec(c) {
                loda_args.push((LODA_ARG_CONNECTOR, cb));
            }
            let (a, b) = doc.resolve_connector_endpoints(c);
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&c.route_path(a, b))));
        }
        NodePayload::SymbolInstance(sym_id) => {
            chunk_type = CDR_CTYPE_CURVE;
            loda_args.push((LODA_ARG_SYMBOL_INSTANCE, sym_id.0.as_bytes().to_vec()));
            if let Some(sym) = doc.symbol(*sym_id) {
                let mut merged = BezierPath::new();
                for sub in &sym.objects {
                    match &sub.payload {
                        NodePayload::Path(p) => merged.subpaths.extend(p.subpaths.clone()),
                        NodePayload::Shape(s) => {
                            merged.subpaths.extend(s.to_bezier_path().subpaths)
                        }
                        NodePayload::BSpline(bs) => {
                            merged.subpaths.extend(bs.to_bezier_path().subpaths)
                        }
                        _ => {}
                    }
                }
                if !merged.subpaths.is_empty() {
                    loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&merged)));
                }
            }
        }
        NodePayload::BSpline(bs) => {
            chunk_type = CDR_CTYPE_CURVE;
            if let Ok(bb) = serde_json::to_vec(bs) {
                loda_args.push((LODA_ARG_BSPLINE, bb));
            }
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&bs.to_bezier_path())));
        }
        NodePayload::ArtisticMedia { spine, style } => {
            chunk_type = CDR_CTYPE_CURVE;
            if let Ok(ab) = serde_json::to_vec(&(spine, style)) {
                loda_args.push((LODA_ARG_ARTISTIC_MEDIA, ab));
            }
            let expanded = coree_core::ArtisticMedia::expand_stroke(spine, style);
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&expanded)));
        }
        NodePayload::Clone {
            master_id,
            link_fill,
            link_outline,
            link_shape,
        } => {
            chunk_type = CDR_CTYPE_CURVE;
            if let Ok(cb) =
                serde_json::to_vec(&(*master_id, *link_fill, *link_outline, *link_shape))
            {
                loda_args.push((LODA_ARG_CLONE, cb));
            }
        }
        NodePayload::Group(_) => unreachable!(),
    }

    // Re-inject any unknown `LODA` args back into `loda_args` before serializing the 20B chunk
    let mut deferred_unknown_chunks = Vec::new();
    for unk in &obj.unknown_chunks {
        if &unk.fourcc == b"LODA" && unk.raw_bytes.len() >= 4 {
            let arg_typ = u32::from_le_bytes([
                unk.raw_bytes[0],
                unk.raw_bytes[1],
                unk.raw_bytes[2],
                unk.raw_bytes[3],
            ]);
            loda_args.push((arg_typ, unk.raw_bytes[4..].to_vec()));
        } else {
            deferred_unknown_chunks.push(unk);
        }
    }

    obj_nodes.insert(
        0,
        RiffNode::chunk(*b"loda", encode_loda_args(chunk_type, &loda_args)),
    );

    if obj.transform != Affine2D::IDENTITY {
        obj_nodes.push(RiffNode::chunk(*b"trfd", encode_trfd_chunk(&obj.transform)));
    }
    if obj.fill.is_visible() {
        obj_nodes.push(RiffNode::chunk(
            *b"fild",
            encode_fild_chunk(0, &obj.fill, bmp_map),
        ));
    }
    if obj.outline.is_visible() {
        obj_nodes.push(RiffNode::chunk(
            *b"outl",
            encode_outl_chunk(0, &obj.outline),
        ));
    }
    if let Some(ref lens) = obj.lens {
        obj_nodes.push(RiffNode::chunk(*b"lens", encode_lens_chunk(lens)));
    }

    for unk in deferred_unknown_chunks {
        obj_nodes.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
    }

    Ok(Some(RiffNode::list(*b"obj ", obj_nodes)))
}

fn unknown_chunk_to_riff_node(unk: &UnknownCdrChunk, endian: RiffEndian) -> RiffNode {
    if &unk.fourcc == b"LIST" && unk.raw_bytes.len() >= 4 {
        if let Ok(list_node) = parse_list_body(&unk.raw_bytes, endian, &CodecLimits::default()) {
            return list_node;
        }
    }
    RiffNode::chunk(unk.fourcc, unk.raw_bytes.clone())
}

fn encode_trfd_chunk(xf: &Affine2D) -> Vec<u8> {
    let mut out = Vec::with_capacity(52);
    out.extend_from_slice(&0x0008u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    let [a, b, c, d, tx, ty] = xf.m;
    out.extend_from_slice(&sanitize_f64(a, 1.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(b, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(c, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(d, 1.0).to_le_bytes());
    out.extend_from_slice(
        &(sanitize_f64(tx, 0.0) * coree_core::math::CDR_UNITS_PER_MM).to_le_bytes(),
    );
    out.extend_from_slice(
        &(sanitize_f64(ty, 0.0) * coree_core::math::CDR_UNITS_PER_MM).to_le_bytes(),
    );
    out
}

fn encode_lens_chunk(lens: &LensConfig) -> Vec<u8> {
    let mut out = Vec::new();
    let kind_id = match &lens.kind {
        LensKind::Brighten { .. } => 1u8,
        LensKind::ColorAdd { .. } => 2,
        LensKind::ColorLimit { .. } => 3,
        LensKind::CustomColorMap { .. } => 4,
        LensKind::FishEye { .. } => 5,
        LensKind::HeatMap { .. } => 6,
        LensKind::Invert => 7,
        LensKind::Magnify { .. } => 8,
        LensKind::TintedGrayscale { .. } => 9,
        LensKind::Transparency { .. } => 10,
        LensKind::Wireframe { .. } => 11,
    };
    out.push(kind_id);
    let mut flags = 0u8;
    if lens.frozen {
        flags |= 0x01;
    }
    if lens.remove_face {
        flags |= 0x02;
    }
    if lens.viewpoint.is_some() {
        flags |= 0x04;
    }
    out.push(flags);
    if let Some(vp) = lens.viewpoint {
        out.extend_from_slice(&mm_to_cdr32(vp.x).to_le_bytes());
        out.extend_from_slice(&mm_to_cdr32(vp.y).to_le_bytes());
    }
    match &lens.kind {
        LensKind::Brighten { rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 0.0).to_le_bytes());
        }
        LensKind::ColorAdd { color, rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 50.0).to_le_bytes());
            write_tagged_color(&mut out, color);
        }
        LensKind::ColorLimit { color, rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 50.0).to_le_bytes());
            write_tagged_color(&mut out, color);
        }
        LensKind::CustomColorMap {
            from_color,
            to_color,
            palette_mode,
        } => {
            out.push(match palette_mode {
                ColorMapMode::Direct => 0,
                ColorMapMode::ForwardRainbow => 1,
                ColorMapMode::ReverseRainbow => 2,
            });
            write_tagged_color(&mut out, from_color);
            write_tagged_color(&mut out, to_color);
        }
        LensKind::FishEye { rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 100.0).to_le_bytes());
        }
        LensKind::HeatMap {
            palette_rotation_deg,
        } => {
            out.extend_from_slice(&sanitize_f32(*palette_rotation_deg, 0.0).to_le_bytes());
        }
        LensKind::Invert => {}
        LensKind::Magnify { amount } => {
            out.extend_from_slice(&sanitize_f32(*amount, 2.0).to_le_bytes());
        }
        LensKind::TintedGrayscale { tint } => {
            write_tagged_color(&mut out, tint);
        }
        LensKind::Transparency { color, rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 50.0).to_le_bytes());
            write_tagged_color(&mut out, color);
        }
        LensKind::Wireframe {
            override_outline,
            override_fill,
        } => {
            if let Some(c) = override_outline {
                out.push(1);
                write_tagged_color(&mut out, c);
            } else {
                out.push(0);
            }
            if let Some(c) = override_fill {
                out.push(1);
                write_tagged_color(&mut out, c);
            } else {
                out.push(0);
            }
        }
    }
    out
}

fn cdr_placeholder_asset_id(num_id: u32) -> ImageAssetId {
    ImageAssetId(uuid::Uuid::from_u128(
        0xC0EE_0000_0000_0000_0000_0000_0000_0000u128 | u128::from(num_id),
    ))
}

fn remap_scene_object_assets_to_placeholders(
    obj: &mut coree_core::SceneObject,
    asset_id_to_num: &HashMap<ImageAssetId, u32>,
) {
    match &mut obj.payload {
        NodePayload::Raster(ref mut r) => {
            if let Some(&num_id) = asset_id_to_num.get(&r.asset_id) {
                r.asset_id = cdr_placeholder_asset_id(num_id);
            }
        }
        NodePayload::Table(ref mut t) => {
            for cell in &mut t.cells {
                if let Fill::BitmapPattern {
                    ref mut image_id, ..
                } = cell.fill
                {
                    if let Some(&num_id) = asset_id_to_num.get(image_id) {
                        *image_id = cdr_placeholder_asset_id(num_id);
                    }
                }
            }
        }
        _ => {}
    }
    if let Fill::BitmapPattern {
        ref mut image_id, ..
    } = obj.fill
    {
        if let Some(&num_id) = asset_id_to_num.get(image_id) {
            *image_id = cdr_placeholder_asset_id(num_id);
        }
    }
}
