// SPDX-License-Identifier: MIT OR Apache-2.0

//! RIFF CDR document/page/layer/group/object tree parser (`v3`–`v26`).

use super::dib::{decode_bmpt_alpha_mask, decode_dib_to_rgba_with_budget};
use super::fild_outl::{
    parse_arrw_list, parse_bmpf_list, parse_clrs_list, parse_fild_chunk_with_ctx,
    parse_outl_chunk_with_ctx, parse_vpat_list,
};
use super::legacy_v3_v5::{is_legacy_v3_v5, parse_v3_v5_doc_mcfg, parse_v3_v5_page_mcfg};
use super::loda::{
    extract_real_loda_name, is_cdr_native_loda, parse_loda_chunk, parse_real_layer_loda,
    CDR_CTYPE_LAYER_DESKTOP, CDR_CTYPE_LAYER_GRID, CDR_CTYPE_LAYER_GUIDES,
};
use super::parser_objects::{
    is_cdr_native_trfd, parse_lens_chunk, parse_object_list, parse_synthetic_trfd_chunk,
};
use super::text_style::{parse_font_list, parse_stlt_chunk, ParsedFontEntry, ParsedStyleEntry};
use super::{is_cdr_form_type, is_cdt_form_type, version_from_form_type, ParsedRiffCdr};
use crate::coord::{cdr32_to_mm, decode_cdr_text, read_tagged_color, CODEPAGE_UTF8};
use crate::error::{CdrError, CdrResult};
use crate::limits::{ByteReader, CodecLimits};
use crate::riff::{fourcc_to_string, serialize_list_body, RiffDocument, RiffEndian, RiffNode};
use crate::zcf::ZcfContext;
use coree_core::{
    math::sanitize_f32, Affine2D, ArrowheadStyle, BlendMode, Color, ColorMode, ColorStyle,
    ColorSwatch, Document, Effect, Fill, GridConfig, Guide, ImageAssetId, Layer, LayerId,
    LensConfig, MasterLayerScope, NodePayload, ObjectId, ObjectStyle, OutlineStyle, Page, PageId,
    PerspectiveGridConfig, RasterAsset, SceneObject, SymbolDef, Unit, UnknownCdrChunk,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// Tagged bitmap chunk header magic
pub(crate) const CBMP_MAGIC: &[u8; 4] = b"CBMP";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StlyPayload {
    pub color_styles: Vec<ColorStyle>,
    pub object_styles: Vec<ObjectStyle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GuidPayload {
    pub guides: Vec<Guide>,
    pub grid: GridConfig,
    pub perspective_grid: PerspectiveGridConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SymbPayload {
    pub symbols: Vec<SymbolDef>,
}

pub(crate) struct ParseContext<'a> {
    pub limits: &'a CodecLimits,
    pub budget_remaining: &'a mut u64,
    pub endian: RiffEndian,
    pub version: u16,
    pub default_codepage: Option<u16>,
    pub is_zcf: bool,
    pub is_v3_v5: bool,
    pub doc_page_width_mm: f64,
    pub doc_page_height_mm: f64,
    pub current_page_w_mm: f64,
    pub current_page_h_mm: f64,
    pub zcf: &'a ZcfContext,
    pub fills: HashMap<u32, Fill>,
    pub normalized_fills: HashSet<u32>,
    pub outlines: HashMap<u32, OutlineStyle>,
    pub outlines_explicit_none: HashSet<u32>,
    pub styles: HashMap<u32, ParsedStyleEntry>,
    pub fonts: HashMap<u32, ParsedFontEntry>,
    pub bitmaps: HashMap<u32, ImageAssetId>,
    pub asset_id_remap: HashMap<ImageAssetId, ImageAssetId>,
    pub oid_remap: HashMap<ObjectId, ObjectId>,
    pub arrows: HashMap<u32, ArrowheadStyle>,
    pub pattern_bitmaps: HashMap<u32, ImageAssetId>,
    pub vector_patterns: HashMap<u32, Fill>,
    pub color_styles: HashMap<u32, Color>,
    pub text_streams: HashMap<u32, Vec<u8>>,
    pub master_layers: Vec<Layer>,
    pub total_vertices: usize,
    pub warnings: Vec<String>,
}

/// Decodes a pre-parsed (and for ZCF v16–26, already dereferenced) [`RiffDocument`]
/// into a [`ParsedRiffCdr`], merging any document-wide [`ZcfContext`] resources.
pub(crate) fn parse_cdr_riff_tree_with_options(
    riff_tree: RiffDocument,
    limits: &CodecLimits,
    external_bitmaps: &HashMap<u32, Vec<u8>>,
    budget_remaining: &mut u64,
    zcf: &ZcfContext,
    default_codepage: Option<u16>,
) -> CdrResult<ParsedRiffCdr> {
    let is_template = is_cdt_form_type(&riff_tree.form_type);
    if !is_cdr_form_type(&riff_tree.form_type) && !is_template {
        return Err(CdrError::UnsupportedFormat(format!(
            "RIFF form '{}' is not a .cdr/.cdt container",
            fourcc_to_string(&riff_tree.form_type)
        )));
    }

    let mut version = version_from_form_type(&riff_tree.form_type);
    if let Some(v) = zcf.version_hint {
        if v > 0 {
            version = v;
        }
    }
    if let Some(fver_bytes) = riff_tree.find_chunk(b"fver") {
        if fver_bytes.len() >= 2 {
            let v = u16::from_le_bytes([fver_bytes[0], fver_bytes[1]]);
            if v > 0 && v != u16::MAX {
                version = v;
            }
        }
    }
    if let Some(vrsn_bytes) = riff_tree.find_chunk(b"vrsn") {
        if vrsn_bytes.len() >= 2 {
            let mut vr = ByteReader::new(vrsn_bytes);
            if let Ok(v) = vr.read_u16_le("vrsn") {
                if v > 0 && v != u16::MAX {
                    version = v;
                }
            }
        }
    }

    let (init_w, init_h) = zcf.metadata_page_size_mm.unwrap_or((210.0, 297.0));
    let mut doc = Document::new_a4("Imported CDR");
    doc.clear_pages_for_load();
    doc.metadata_mut().source_format = Some(if is_template {
        format!("CDT RIFF v{}", version / 100)
    } else {
        format!("CDR RIFF v{}", version / 100)
    });

    let is_v3_v5 = is_legacy_v3_v5(version, &riff_tree.form_type);
    let mut ctx = ParseContext {
        limits,
        budget_remaining,
        endian: riff_tree.endian,
        version,
        default_codepage,
        is_zcf: zcf.is_zcf || &riff_tree.form_type == b"CDRO" || &riff_tree.form_type == b"CDTO",
        is_v3_v5,
        doc_page_width_mm: init_w,
        doc_page_height_mm: init_h,
        current_page_w_mm: init_w,
        current_page_h_mm: init_h,
        zcf,
        fills: HashMap::new(),
        normalized_fills: HashSet::new(),
        outlines: HashMap::new(),
        outlines_explicit_none: HashSet::new(),
        styles: HashMap::new(),
        fonts: HashMap::new(),
        bitmaps: HashMap::new(),
        asset_id_remap: HashMap::new(),
        oid_remap: HashMap::new(),
        arrows: HashMap::new(),
        pattern_bitmaps: HashMap::new(),
        vector_patterns: HashMap::new(),
        color_styles: HashMap::new(),
        text_streams: HashMap::new(),
        master_layers: Vec::new(),
        total_vertices: 0,
        warnings: Vec::new(),
    };

    // Pre-register any external bitmaps supplied by a CDR X6+ ZIP container
    for (&ext_id, payload) in external_bitmaps {
        if let Err(e) = register_bmp_chunk_payload(payload, Some(ext_id), &mut doc, &mut ctx) {
            if matches!(
                e,
                CdrError::InvalidBitmapDimensions { .. }
                    | CdrError::UncompressedSizeLimitExceeded { .. }
            ) {
                return Err(e);
            }
            ctx.warnings
                .push(format!("skipped external bitmap #{ext_id}: {e}"));
        }
    }

    let mut found_doc_list = false;
    let mut is_template_flag = is_template;
    let mut top_nodes = Vec::new();
    flatten_cmpr_nodes(&riff_tree.children, &mut top_nodes);

    // Pass 1 (top-level): parse `LIST doc ` and any top-level metadata/resources BEFORE pages
    for &node in &top_nodes {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"vrsn" | b"fver" | b"DISP" => {}
                b"tmpt" => {
                    is_template_flag = true;
                }
                b"iccp" | b"PROF" => {
                    let _ = doc.set_custom_icc_profile(data.clone());
                }
                _ => {
                    doc.push_unknown_doc_chunk(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(version),
                        raw_bytes: data.clone(),
                    });
                }
            },
            RiffNode::List {
                list_type,
                children,
                ..
            } => match list_type {
                b"doc " => {
                    found_doc_list = true;
                    parse_doc_list(children, &mut doc, &mut ctx)?;
                }
                b"page" => {}
                _ => {
                    let raw = serialize_list_body(list_type, children, ctx.endian)?;
                    doc.push_unknown_doc_chunk(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(version),
                        raw_bytes: raw,
                    });
                }
            },
        }
    }

    // Pass 2 (top-level): parse top-level `LIST page` containers (CDR X6–2025 `CDRO` layout)
    for &node in &top_nodes {
        if let RiffNode::List {
            list_type,
            children: page_ch,
            ..
        } = node
        {
            if list_type == b"page" {
                parse_page_list(page_ch, &mut doc, &mut ctx)?;
            }
        }
    }

    if !found_doc_list {
        ctx.warnings
            .push("CDR stream contained no 'LIST doc ' container; initialized default page".into());
    }

    if doc.pages().is_empty() {
        let pid = PageId(doc.alloc_id());
        let lid = LayerId(doc.alloc_id());
        doc.push_loaded_page(Page::new(
            pid,
            "Page 1",
            ctx.doc_page_width_mm,
            ctx.doc_page_height_mm,
            3.0,
            lid,
        ));
    }

    // Attach any non-empty Master Page layers onto the first document page
    if !ctx.master_layers.is_empty() {
        let master_layers = std::mem::take(&mut ctx.master_layers);
        if let Some(p0) = doc.page_mut(0) {
            p0.layers.extend(master_layers);
        }
    }

    // Apply optional `META-INF/textinfo.xml` page names when binary chunks had defaults
    for (idx, pname) in ctx.zcf.textinfo.page_names.iter().enumerate() {
        if let Some(page) = doc.page_mut(idx) {
            if !pname.is_empty() {
                page.name = pname.clone();
            }
        }
    }

    // Remap cross-object ObjectIds and ImageAssetIds after full document load
    remap_loaded_references(&mut doc, &ctx);

    if doc.document_palette().swatches.is_empty() {
        doc.refresh_document_palette();
    }

    Ok(ParsedRiffCdr {
        document: doc,
        version,
        is_template: is_template_flag,
        warnings: ctx.warnings,
        riff_tree,
    })
}

fn remap_loaded_references(doc: &mut Document, ctx: &ParseContext<'_>) {
    if !ctx.oid_remap.is_empty() || !ctx.asset_id_remap.is_empty() {
        let oids: Vec<ObjectId> = doc.objects().keys().copied().collect();
        for oid in oids {
            if let Some(obj) = doc.object_mut_for_load(oid) {
                match &mut obj.payload {
                    NodePayload::TextOnPath(ref mut top) => {
                        if let Some(&mapped) = ctx.oid_remap.get(&top.path_id) {
                            top.path_id = mapped;
                        }
                    }
                    NodePayload::Connector(ref mut conn) => {
                        if let Some(ref mut fid) = conn.from_obj {
                            if let Some(&mapped) = ctx.oid_remap.get(fid) {
                                *fid = mapped;
                            }
                        }
                        if let Some(ref mut tid) = conn.to_obj {
                            if let Some(&mapped) = ctx.oid_remap.get(tid) {
                                *tid = mapped;
                            }
                        }
                    }
                    NodePayload::ParagraphText(ref mut para) => {
                        if let Some(ref mut next_id) = para.linked_next_frame {
                            if let Some(&mapped) = ctx.oid_remap.get(next_id) {
                                *next_id = mapped;
                            }
                        }
                    }
                    NodePayload::Clone {
                        ref mut master_id, ..
                    } => {
                        if let Some(&mapped) = ctx.oid_remap.get(master_id) {
                            *master_id = mapped;
                        }
                    }
                    NodePayload::Raster(ref mut r) => {
                        if let Some(&mapped) = ctx.asset_id_remap.get(&r.asset_id) {
                            r.asset_id = mapped;
                        }
                    }
                    NodePayload::Table(ref mut t) => {
                        for cell in &mut t.cells {
                            if let Fill::BitmapPattern {
                                ref mut image_id, ..
                            } = cell.fill
                            {
                                if let Some(&mapped) = ctx.asset_id_remap.get(image_id) {
                                    *image_id = mapped;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                for eff in &mut obj.effects {
                    if let Effect::Blend {
                        ref mut target_id,
                        ref mut along_path,
                        ..
                    } = eff
                    {
                        if let Some(&mapped) = ctx.oid_remap.get(target_id) {
                            *target_id = mapped;
                        }
                        if let Some(ref mut path_oid) = along_path {
                            if let Some(&mapped) = ctx.oid_remap.get(path_oid) {
                                *path_oid = mapped;
                            }
                        }
                    }
                }
                if let Fill::BitmapPattern {
                    ref mut image_id, ..
                } = obj.fill
                {
                    if let Some(&mapped) = ctx.asset_id_remap.get(image_id) {
                        *image_id = mapped;
                    }
                }
            }
        }
    }
    if !ctx.asset_id_remap.is_empty() {
        let syms: Vec<SymbolDef> = doc
            .symbols()
            .values()
            .cloned()
            .map(|mut s| {
                for sub in &mut s.objects {
                    match &mut sub.payload {
                        NodePayload::Raster(ref mut r) => {
                            if let Some(&mapped) = ctx.asset_id_remap.get(&r.asset_id) {
                                r.asset_id = mapped;
                            }
                        }
                        NodePayload::Table(ref mut t) => {
                            for cell in &mut t.cells {
                                if let Fill::BitmapPattern {
                                    ref mut image_id, ..
                                } = cell.fill
                                {
                                    if let Some(&mapped) = ctx.asset_id_remap.get(image_id) {
                                        *image_id = mapped;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    if let Fill::BitmapPattern {
                        ref mut image_id, ..
                    } = sub.fill
                    {
                        if let Some(&mapped) = ctx.asset_id_remap.get(image_id) {
                            *image_id = mapped;
                        }
                    }
                }
                s
            })
            .collect();
        for s in syms {
            doc.insert_symbol(s);
        }
        let styles: Vec<ObjectStyle> = doc
            .object_styles()
            .iter()
            .cloned()
            .map(|mut os| {
                if let Some(Fill::BitmapPattern {
                    ref mut image_id, ..
                }) = os.fill
                {
                    if let Some(&mapped) = ctx.asset_id_remap.get(image_id) {
                        *image_id = mapped;
                    }
                }
                os
            })
            .collect();
        for os in styles {
            doc.upsert_object_style(os);
        }
    }
}

fn flatten_cmpr_nodes<'a>(nodes: &'a [RiffNode], out: &mut Vec<&'a RiffNode>) {
    for node in nodes {
        if let RiffNode::List {
            list_type,
            children,
            ..
        } = node
        {
            if list_type == b"cmpr" {
                flatten_cmpr_nodes(children, out);
                continue;
            }
        }
        out.push(node);
    }
}

fn parse_doc_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<()> {
    let mut flat_nodes = Vec::new();
    flatten_cmpr_nodes(children, &mut flat_nodes);

    // Pass 1: load ALL shared document resources
    for &node in &flat_nodes {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"mcfg" => parse_doc_mcfg(data, doc, ctx),
                b"plte" => parse_plte_chunk(data, doc, ctx),
                b"fild" => {
                    if let Ok((id, fill, is_norm)) =
                        parse_fild_chunk_with_ctx(data, ctx.is_zcf, Some(ctx))
                    {
                        if is_norm {
                            ctx.normalized_fills.insert(id);
                        }
                        ctx.fills.insert(id, fill);
                    }
                }
                b"outl" => {
                    if let Ok((id, outl, is_none)) =
                        parse_outl_chunk_with_ctx(data, ctx.is_zcf, Some(ctx))
                    {
                        if is_none {
                            ctx.outlines_explicit_none.insert(id);
                        }
                        ctx.outlines.insert(id, outl);
                    }
                }
                b"stlt" => {
                    if let Ok((id, entry)) = parse_stlt_chunk(data) {
                        ctx.styles.insert(id, entry);
                    }
                }
                b"bmp " => {
                    register_bmp_chunk_payload(data, None, doc, ctx)?;
                }
                b"iccp" | b"PROF" => {
                    let _ = doc.set_custom_icc_profile(data.clone());
                }
                b"symb" => {
                    if let Ok(sp) = serde_json::from_slice::<SymbPayload>(data) {
                        for sym in sp.symbols {
                            doc.insert_symbol(sym);
                        }
                    }
                }
                b"stly" => {
                    if let Ok(st) = serde_json::from_slice::<StlyPayload>(data) {
                        for cs in st.color_styles {
                            doc.upsert_color_style(cs);
                        }
                        for os in st.object_styles {
                            doc.upsert_object_style(os);
                        }
                    }
                }
                b"guid" => {
                    if let Ok(gp) = serde_json::from_slice::<GuidPayload>(data) {
                        doc.replace_guides(gp.guides);
                        doc.set_grid(gp.grid);
                        doc.set_perspective_grid(gp.perspective_grid);
                    }
                }
                b"pref" => parse_pref_chunk(data, doc),
                b"vrsn" | b"fver" | b"DISP" | b"tmpt" | b"ptrt" | b"flgs" | b"spid" | b"bbox"
                | b"obbx" | b"usdn" => {}
                _ => {
                    doc.push_unknown_doc_chunk(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(ctx.version),
                        raw_bytes: data.clone(),
                    });
                }
            },
            RiffNode::List {
                list_type,
                children: list_ch,
                ..
            } => match list_type {
                b"fill" | b"filt" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if fourcc == b"fild" {
                                if let Ok((id, fill, is_norm)) =
                                    parse_fild_chunk_with_ctx(data, ctx.is_zcf, Some(ctx))
                                {
                                    if is_norm {
                                        ctx.normalized_fills.insert(id);
                                    }
                                    ctx.fills.insert(id, fill);
                                }
                            }
                        }
                    }
                }
                b"outl" | b"otlt" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if fourcc == b"outl" {
                                if let Ok((id, outl, is_none)) =
                                    parse_outl_chunk_with_ctx(data, ctx.is_zcf, Some(ctx))
                                {
                                    if is_none {
                                        ctx.outlines_explicit_none.insert(id);
                                    }
                                    ctx.outlines.insert(id, outl);
                                }
                            }
                        }
                    }
                }
                b"stlt" | b"styd" | b"font" => {
                    parse_font_list(list_ch, &mut ctx.styles, &mut ctx.fonts);
                }
                b"bmps" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if fourcc == b"bmp " {
                                if let Err(e) = register_bmp_chunk_payload(data, None, doc, ctx) {
                                    if matches!(
                                        e,
                                        CdrError::InvalidBitmapDimensions { .. }
                                            | CdrError::UncompressedSizeLimitExceeded { .. }
                                    ) {
                                        return Err(e);
                                    }
                                    ctx.warnings.push(format!("skipped document bitmap: {e}"));
                                }
                            }
                        }
                    }
                }
                b"bmpt" => {
                    parse_bmpt_list(list_ch, doc, ctx)?;
                }
                b"txsm" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if (fourcc == b"txsm" || fourcc == b"txst") && data.len() >= 8 {
                                let tid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                                ctx.text_streams.insert(tid, data.clone());
                            }
                        }
                    }
                }
                b"arrw" => parse_arrw_list(list_ch, ctx),
                b"bmpf" => parse_bmpf_list(list_ch, doc, ctx)?,
                b"vpat" => parse_vpat_list(list_ch, doc, ctx),
                b"clrs" => parse_clrs_list(list_ch, doc, ctx),
                b"page" => {}
                _ => {
                    let raw = serialize_list_body(list_type, list_ch, ctx.endian)?;
                    doc.push_unknown_doc_chunk(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            },
        }
    }

    // Pass 2: parse ALL pages inside `LIST doc `
    for &node in &flat_nodes {
        if let RiffNode::List {
            list_type,
            children: page_ch,
            ..
        } = node
        {
            if list_type == b"page" {
                parse_page_list(page_ch, doc, ctx)?;
            }
        }
    }

    Ok(())
}

fn parse_bmpt_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<()> {
    let mut base_bmp_id: Option<ImageAssetId> = None;
    let mut alpha_chunks: Vec<&[u8]> = Vec::new();
    for ch in children {
        if let RiffNode::Chunk { fourcc, data, .. } = ch {
            if fourcc == b"bmp " {
                match register_bmp_chunk_payload(data, None, doc, ctx) {
                    Ok(aid) => {
                        if base_bmp_id.is_none() {
                            base_bmp_id = Some(aid);
                        } else {
                            alpha_chunks.push(data.as_slice());
                        }
                    }
                    Err(e) => {
                        if matches!(
                            e,
                            CdrError::InvalidBitmapDimensions { .. }
                                | CdrError::UncompressedSizeLimitExceeded { .. }
                        ) {
                            return Err(e);
                        }
                        ctx.warnings.push(format!("skipped bmpt bitmap: {e}"));
                    }
                }
            } else if fourcc == b"alph" || fourcc == b"mask" {
                alpha_chunks.push(data.as_slice());
            }
        }
    }
    if let Some(aid) = base_bmp_id {
        if let Some(mask_raw) = alpha_chunks.first() {
            if let Some(asset) = doc.asset(aid).cloned() {
                if let Ok(alpha) = decode_bmpt_alpha_mask(
                    mask_raw,
                    asset.width,
                    asset.height,
                    ctx.limits,
                    ctx.budget_remaining,
                ) {
                    let mut new_rgba = asset.to_rgba8_vec();
                    for (px, &a) in new_rgba.as_chunks_mut::<4>().0.iter_mut().zip(alpha.iter()) {
                        px[3] = a;
                    }
                    if let Ok(mut updated) = RasterAsset::new(
                        asset.width,
                        asset.height,
                        new_rgba,
                        asset.source_dpi,
                        asset.color_mode,
                    ) {
                        updated.id = aid;
                        doc.replace_asset(updated);
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_pref_chunk(data: &[u8], doc: &mut Document) {
    if data.len() < 4 {
        return;
    }
    let unit = match data[0] {
        0 => Unit::Millimeter,
        1 => Unit::Centimeter,
        2 => Unit::Inch,
        3 => Unit::Point,
        4 => Unit::Pixel,
        _ => return,
    };
    doc.set_default_unit(unit);
    if data.len() >= 12 {
        let sx = cdr32_to_mm(i32::from_le_bytes([data[4], data[5], data[6], data[7]]));
        let sy = cdr32_to_mm(i32::from_le_bytes([data[8], data[9], data[10], data[11]]));
        if sx > 0.01 && sy > 0.01 && sx < 5000.0 && sy < 5000.0 {
            let mut g = doc.grid();
            g.spacing_x_mm = sx;
            g.spacing_y_mm = sy;
            doc.set_grid(g);
        }
    }
}

fn parse_doc_mcfg(data: &[u8], doc: &mut Document, ctx: &mut ParseContext<'_>) {
    if ctx.is_v3_v5 {
        if let Some((w, h, mode, dpi, title)) = parse_v3_v5_doc_mcfg(data) {
            ctx.doc_page_width_mm = w;
            ctx.doc_page_height_mm = h;
            doc.set_color_mode(mode);
            doc.set_dpi(dpi);
            if !title.is_empty() {
                doc.set_title(title);
            }
            return;
        }
    }

    if data.len() >= 64 {
        let mut r = ByteReader::new(data);
        let first = r.read_u32_le("mcfg hdr0").unwrap_or(0);
        if (100..100_000).contains(&first) && data.len() >= 20 {
            let w_cdr = i32::from_le_bytes([data[12], data[13], data[14], data[15]]);
            let h_cdr = i32::from_le_bytes([data[16], data[17], data[18], data[19]]);
            let wm = cdr32_to_mm(w_cdr);
            let hm = cdr32_to_mm(h_cdr);
            if wm > 1.0 && hm > 1.0 && wm < 50_000.0 && hm < 50_000.0 {
                ctx.doc_page_width_mm = wm;
                ctx.doc_page_height_mm = hm;
                return;
            }
        }
        let w_cdr = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let h_cdr = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let wm = cdr32_to_mm(w_cdr);
        let hm = cdr32_to_mm(h_cdr);
        if wm > 1.0 && hm > 1.0 && wm < 50_000.0 && hm < 50_000.0 {
            ctx.doc_page_width_mm = wm;
            ctx.doc_page_height_mm = hm;
            return;
        }
    }

    let mut r = ByteReader::new(data);
    let Ok(w_cdr) = r.read_i32_le("doc mcfg w") else {
        ctx.warnings.push("truncated doc mcfg chunk".into());
        return;
    };
    let Ok(h_cdr) = r.read_i32_le("doc mcfg h") else {
        return;
    };
    let wm = cdr32_to_mm(w_cdr);
    let hm = cdr32_to_mm(h_cdr);
    if wm > 1.0 && hm > 1.0 && wm < 50_000.0 && hm < 50_000.0 {
        ctx.doc_page_width_mm = wm;
        ctx.doc_page_height_mm = hm;
    }
    if let Ok(cm) = r.read_u8("doc mcfg color_mode") {
        doc.set_color_mode(if cm == 1 {
            ColorMode::Rgb
        } else {
            ColorMode::Cmyk
        });
    }
    if let Ok(dpi) = r.read_f32_le("doc mcfg dpi") {
        doc.set_dpi(sanitize_f32(dpi, 300.0).max(1.0));
    }
    if let Ok(title_len) = r.read_u16_le("doc mcfg title_len") {
        if let Ok(tb) = r.read_bytes(usize::from(title_len), "doc mcfg title") {
            let t = decode_cdr_text(tb, Some(CODEPAGE_UTF8), None);
            if !t.is_empty() {
                doc.set_title(t);
            }
        }
    }
}

fn parse_plte_chunk(data: &[u8], doc: &mut Document, ctx: &mut ParseContext<'_>) {
    let mut r = ByteReader::new(data);
    let Ok(count) = r.read_u16_le("plte count") else {
        ctx.warnings.push("truncated plte chunk".into());
        return;
    };
    doc.document_palette_mut().swatches.clear();
    for _ in 0..count {
        let Ok(name_len) = r.read_u16_le("plte swatch name len") else {
            break;
        };
        let Ok(nb) = r.read_bytes(usize::from(name_len), "plte swatch name") else {
            break;
        };
        let name = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
        let Ok(color) = read_tagged_color(&mut r, "plte swatch color") else {
            break;
        };
        doc.document_palette_mut()
            .add_swatch(ColorSwatch::new(name, color));
    }
}

pub(crate) fn register_bmp_chunk_payload(
    data: &[u8],
    override_id: Option<u32>,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<ImageAssetId> {
    let (bmp_id, dpi, color_mode, dib_bytes) = if data.len() >= 13 && &data[0..4] == CBMP_MAGIC {
        let mut r = ByteReader::new(data);
        r.skip(4, "CBMP magic")?;
        let id = override_id.unwrap_or(r.read_u32_le("CBMP id")?);
        let dpi_x100 = r.read_u32_le("CBMP dpi")?;
        let cm_byte = r.read_u8("CBMP color_mode")?;
        let dpi = if dpi_x100 > 0 {
            (dpi_x100 as f32) / 100.0
        } else {
            300.0
        };
        let cm = if cm_byte == 1 {
            ColorMode::Rgb
        } else {
            ColorMode::Cmyk
        };
        (id, dpi, cm, r.read_rest())
    } else {
        let id = override_id.unwrap_or_else(|| (ctx.bitmaps.len() as u32).saturating_add(1));
        (id, 300.0, ColorMode::Rgb, data)
    };

    let (w, h, rgba) = decode_dib_to_rgba_with_budget(dib_bytes, ctx.limits, ctx.budget_remaining)?;
    let asset = RasterAsset::new(w, h, rgba, dpi, color_mode)?;
    let asset_id = doc.register_asset(asset);
    ctx.bitmaps.insert(bmp_id, asset_id);
    ctx.asset_id_remap.insert(
        ImageAssetId(uuid::Uuid::from_u128(
            0xC0EE_0000_0000_0000_0000_0000_0000_0000u128 | u128::from(bmp_id),
        )),
        asset_id,
    );
    Ok(asset_id)
}

fn parse_page_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<()> {
    let page_num = doc.pages().len() + 1;
    let mut page_name = format!("Page {page_num}");
    let mut width_mm = ctx.doc_page_width_mm;
    let mut height_mm = ctx.doc_page_height_mm;
    let mut bleed_mm = 3.0;
    let mut facing_pages = false;
    let mut is_master_page = false;
    let mut page_unknown_chunks: Vec<UnknownCdrChunk> = Vec::new();

    for node in children {
        if let RiffNode::Chunk { fourcc, data, .. } = node {
            match fourcc {
                b"mcfg" => {
                    if ctx.is_v3_v5 {
                        if let Some((w, h, b, nm)) = parse_v3_v5_page_mcfg(data) {
                            width_mm = w;
                            height_mm = h;
                            bleed_mm = b;
                            if !nm.is_empty() {
                                page_name = nm;
                            }
                            continue;
                        }
                    }
                    if data.len() >= 64 {
                        if let Some((wm, hm)) = parse_real_page_mcfg(data) {
                            width_mm = wm;
                            height_mm = hm;
                        }
                    } else {
                        let mut r = ByteReader::new(data);
                        if let (Ok(w), Ok(h)) = (r.read_i32_le("page w"), r.read_i32_le("page h")) {
                            let wm = cdr32_to_mm(w);
                            let hm = cdr32_to_mm(h);
                            if wm > 0.1 && hm > 0.1 {
                                width_mm = wm;
                                height_mm = hm;
                            }
                        }
                        if let Ok(b) = r.read_i32_le("page bleed") {
                            bleed_mm = cdr32_to_mm(b).max(0.0);
                        }
                        if let Ok(fp) = r.read_u8("page facing") {
                            facing_pages = fp != 0;
                        }
                        if let Ok(nlen) = r.read_u16_le("page name len") {
                            if let Ok(nb) = r.read_bytes(usize::from(nlen), "page name") {
                                let s = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
                                if !s.is_empty() {
                                    page_name = s;
                                }
                            }
                        }
                    }
                }
                b"flgs" => {
                    if data.len() >= 4 {
                        let flgs = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        if (flgs & 0x0001_0000) != 0 {
                            is_master_page = true;
                        }
                    }
                }
                b"spid" | b"bbox" | b"obbx" | b"usdn" => {}
                _ => {
                    page_unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(ctx.version),
                        raw_bytes: data.clone(),
                    });
                }
            }
        }
    }

    ctx.current_page_w_mm = width_mm;
    ctx.current_page_h_mm = height_mm;

    let mut layer_lists: Vec<&[RiffNode]> = Vec::new();
    for node in children {
        if let RiffNode::List {
            list_type,
            children: sub_ch,
            ..
        } = node
        {
            match list_type {
                b"layr" => layer_lists.push(sub_ch.as_slice()),
                b"gobj" => {
                    for gnode in sub_ch {
                        if let RiffNode::List {
                            list_type: glt,
                            children: gch,
                            ..
                        } = gnode
                        {
                            if glt == b"layr" {
                                layer_lists.push(gch.as_slice());
                            }
                        }
                    }
                }
                b"lgob" => {
                    for lnode in sub_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = lnode {
                            if fourcc == b"loda" {
                                if let Some(name) = extract_real_loda_name(data) {
                                    page_name = name;
                                }
                            }
                        }
                    }
                }
                _ => {
                    let raw = serialize_list_body(list_type, sub_ch, ctx.endian)?;
                    page_unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            }
        }
    }

    if is_master_page {
        for layr_ch in layer_lists {
            if let Some(mut layer) = parse_layer_to_struct(layr_ch, true, 0, doc, ctx)? {
                if !layer.objects.is_empty() {
                    layer.master_scope = MasterLayerScope::AllPages;
                    ctx.master_layers.push(layer);
                }
            }
        }
        doc.extend_unknown_doc_chunks(page_unknown_chunks);
        return Ok(());
    }

    let page_id = PageId(doc.alloc_id());
    let page = Page {
        id: page_id,
        name: page_name,
        width_mm,
        height_mm,
        bleed_mm,
        facing_pages,
        background_fill: coree_core::Fill::None,
        printable_background: false,
        layers: Vec::new(),
        unknown_chunks: page_unknown_chunks,
    };
    doc.push_loaded_page(page);
    let page_idx = doc.pages().len() - 1;

    for layr_ch in layer_lists {
        let next_num = doc.pages()[page_idx].layers.len() + 1;
        if let Some(layer) = parse_layer_to_struct(layr_ch, false, next_num, doc, ctx)? {
            if let Some(p) = doc.page_mut(page_idx) {
                p.layers.push(layer);
            }
        }
    }

    if doc.pages()[page_idx].layers.is_empty() {
        let fallback_name = ctx
            .zcf
            .textinfo
            .layer_names
            .first()
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Layer 1".to_owned());
        let lid = LayerId(doc.alloc_id());
        if let Some(p) = doc.page_mut(page_idx) {
            p.layers.push(Layer::new(lid, fallback_name));
        }
    }

    Ok(())
}

fn parse_real_page_mcfg(data: &[u8]) -> Option<(f64, f64)> {
    if data.len() >= 20 {
        let first = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let (off_w, off_h) = if (100..100_000).contains(&first) {
            (12, 16)
        } else {
            (4, 8)
        };
        let w = cdr32_to_mm(i32::from_le_bytes([
            data[off_w],
            data[off_w + 1],
            data[off_w + 2],
            data[off_w + 3],
        ]));
        let h = cdr32_to_mm(i32::from_le_bytes([
            data[off_h],
            data[off_h + 1],
            data[off_h + 2],
            data[off_h + 3],
        ]));
        if w > 1.0 && h > 1.0 && w < 50_000.0 && h < 50_000.0 {
            return Some((w, h));
        }
    }
    None
}

fn parse_layer_to_struct(
    children: &[RiffNode],
    is_master_page: bool,
    layer_num: usize,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<Option<Layer>> {
    let layer_id = LayerId(doc.alloc_id());
    let mut layer = Layer::new(layer_id, format!("Layer {}", layer_num.max(1)));
    let mut is_system_layer = false;
    let mut has_explicit_name = false;

    for node in children {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"layr" | b"loda" => {
                    if is_cdr_native_loda(data) {
                        if let Some((ctype, name, props)) = parse_real_layer_loda(data) {
                            if matches!(
                                ctype,
                                CDR_CTYPE_LAYER_GRID
                                    | CDR_CTYPE_LAYER_GUIDES
                                    | CDR_CTYPE_LAYER_DESKTOP
                            ) {
                                is_system_layer = true;
                                if ctype == CDR_CTYPE_LAYER_DESKTOP {
                                    layer.is_desktop_layer = true;
                                    layer.master_scope = MasterLayerScope::AllPages;
                                }
                                if ctype == CDR_CTYPE_LAYER_GRID || ctype == CDR_CTYPE_LAYER_GUIDES
                                {
                                    layer.printable = false;
                                }
                            }
                            if let Some(p) = props {
                                layer.visible = p.visible;
                                layer.locked = p.locked;
                                layer.printable = p.printable;
                                layer.is_cut_layer = p.is_cut_layer;
                                layer.master_scope = p.master_scope;
                            }
                            if let Some(n) = name {
                                layer.name = n;
                                has_explicit_name = true;
                            }
                        }
                    } else {
                        let mut r = ByteReader::new(data);
                        if let Ok(flags) = r.read_u8("layer flags") {
                            layer.visible = (flags & 0x01) != 0;
                            layer.locked = (flags & 0x02) != 0;
                            layer.printable = (flags & 0x04) != 0;
                            layer.is_cut_layer = (flags & 0x08) != 0;
                        }
                        if let Ok(ms) = r.read_u8("layer master_scope") {
                            layer.master_scope = match ms {
                                1 => MasterLayerScope::AllPages,
                                2 => MasterLayerScope::OddPages,
                                3 => MasterLayerScope::EvenPages,
                                _ => MasterLayerScope::None,
                            };
                        }
                        if let Ok(nlen) = r.read_u16_le("layer name len") {
                            if let Ok(nb) = r.read_bytes(usize::from(nlen), "layer name") {
                                let s = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
                                if !s.is_empty() {
                                    layer.name = s;
                                    has_explicit_name = true;
                                }
                            }
                        }
                    }
                }
                b"flgs" => {
                    if data.len() >= 4 {
                        let flgs = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        layer.visible = (flgs & 0x0010) == 0;
                        layer.printable = (flgs & 0x0002) == 0;
                        layer.locked = (flgs & 0x0004) != 0;
                        if (flgs & 0x0001_0000) != 0 {
                            layer.master_scope = MasterLayerScope::AllPages;
                        }
                    }
                }
                b"spid" | b"bbox" | b"obbx" | b"usdn" => {}
                _ => {
                    layer.unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(ctx.version),
                        raw_bytes: data.clone(),
                    });
                }
            },
            RiffNode::List {
                list_type: [b'l', b'g', b'o', b'b'],
                children: lgob_ch,
                ..
            } => {
                for gc in lgob_ch {
                    if let RiffNode::Chunk { fourcc, data, .. } = gc {
                        if fourcc == b"loda" {
                            if let Some((ctype, name, props)) = parse_real_layer_loda(data) {
                                if matches!(
                                    ctype,
                                    CDR_CTYPE_LAYER_GRID
                                        | CDR_CTYPE_LAYER_GUIDES
                                        | CDR_CTYPE_LAYER_DESKTOP
                                ) {
                                    is_system_layer = true;
                                    if ctype == CDR_CTYPE_LAYER_DESKTOP {
                                        layer.is_desktop_layer = true;
                                        layer.master_scope = MasterLayerScope::AllPages;
                                    }
                                    if ctype == CDR_CTYPE_LAYER_GRID
                                        || ctype == CDR_CTYPE_LAYER_GUIDES
                                    {
                                        layer.printable = false;
                                    }
                                }
                                if let Some(p) = props {
                                    layer.visible = p.visible;
                                    layer.locked = p.locked;
                                    layer.printable = p.printable;
                                    layer.is_cut_layer = p.is_cut_layer;
                                    layer.master_scope = p.master_scope;
                                }
                                if let Some(n) = name {
                                    layer.name = n;
                                    has_explicit_name = true;
                                }
                            }
                        } else if !matches!(fourcc, b"flgs" | b"spid" | b"bbox" | b"obbx" | b"usdn")
                        {
                            layer.unknown_chunks.push(UnknownCdrChunk {
                                fourcc: *fourcc,
                                version_hint: u32::from(ctx.version),
                                raw_bytes: data.clone(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !has_explicit_name && !is_system_layer && layer_num > 0 {
        if let Some(ti_name) = ctx.zcf.textinfo.layer_names.get(layer_num - 1) {
            if !ti_name.is_empty() {
                layer.name = ti_name.clone();
            }
        }
    }

    for node in children {
        if let RiffNode::List {
            list_type,
            children: sub_ch,
            ..
        } = node
        {
            match list_type {
                b"lgob" => {}
                b"obj " => {
                    if let Some(oid) = parse_object_list(sub_ch, 1, doc, ctx)? {
                        layer.objects.push(oid);
                    }
                }
                b"grp " => {
                    if let Some(gid) = parse_group_list(sub_ch, 1, doc, ctx)? {
                        layer.objects.push(gid);
                    }
                }
                b"cmpr" => {
                    for inner in sub_ch {
                        if let RiffNode::List {
                            list_type: ilt,
                            children: ich,
                            ..
                        } = inner
                        {
                            match ilt {
                                b"obj " => {
                                    if let Some(oid) = parse_object_list(ich, 1, doc, ctx)? {
                                        layer.objects.push(oid);
                                    }
                                }
                                b"grp " => {
                                    if let Some(gid) = parse_group_list(ich, 1, doc, ctx)? {
                                        layer.objects.push(gid);
                                    }
                                }
                                _ => {
                                    let raw = serialize_list_body(ilt, ich, ctx.endian)?;
                                    layer.unknown_chunks.push(UnknownCdrChunk {
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
                    layer.unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            }
        }
    }

    if (is_system_layer || is_master_page)
        && layer.objects.is_empty()
        && layer.unknown_chunks.is_empty()
    {
        return Ok(None);
    }

    Ok(Some(layer))
}

pub(super) fn parse_group_list(
    children: &[RiffNode],
    depth: usize,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<Option<ObjectId>> {
    ctx.limits.check_depth(depth)?;
    let mut child_ids = Vec::new();
    let mut name = "Group".to_owned();
    let mut transform = Affine2D::IDENTITY;
    let mut is_real_cdr_group = false;
    let mut visible = true;
    let mut locked = false;
    let mut printable = true;
    let mut opacity = 1.0_f32;
    let mut blend_mode = BlendMode::Normal;
    let mut lens: Option<LensConfig> = None;
    let mut effects: Vec<Effect> = Vec::new();
    let mut style_id = None;
    let mut perspective = None;
    let mut color_style_link: Option<(Option<coree_core::ColorStyleId>, Option<usize>)> = None;
    let mut source_oid = None;
    let mut unknown_chunks = Vec::new();

    for node in children {
        match node {
            RiffNode::List {
                list_type,
                children: sub_ch,
                ..
            } => match list_type {
                b"lgob" => {
                    for gc in sub_ch {
                        match gc {
                            RiffNode::Chunk {
                                fourcc: [b'l', b'o', b'd', b'a'],
                                data,
                                ..
                            } => {
                                if let Ok(mut parsed) = parse_loda_chunk(data) {
                                    is_real_cdr_group = parsed.is_real_cdr;
                                    if let Some(p) = parsed.props {
                                        name = p.name;
                                        visible = p.visible;
                                        locked = p.locked;
                                        printable = p.printable;
                                        opacity = p.opacity;
                                        blend_mode = p.blend_mode;
                                    } else if let Some(n) = parsed.real_name {
                                        name = n;
                                    }
                                    effects.extend(parsed.effects);
                                    if parsed.object_style_id.is_some() {
                                        style_id = parsed.object_style_id;
                                    }
                                    if parsed.perspective.is_some() {
                                        perspective = parsed.perspective;
                                    }
                                    if parsed.color_style_link.is_some() {
                                        color_style_link = parsed.color_style_link;
                                    }
                                    if parsed.source_oid.is_some() {
                                        source_oid = parsed.source_oid;
                                    }
                                    for (arg_typ, arg_bytes) in
                                        std::mem::take(&mut parsed.unknown_args)
                                    {
                                        let mut raw = Vec::with_capacity(4 + arg_bytes.len());
                                        raw.extend_from_slice(&arg_typ.to_le_bytes());
                                        raw.extend_from_slice(&arg_bytes);
                                        unknown_chunks.push(UnknownCdrChunk {
                                            fourcc: *b"LODA",
                                            version_hint: u32::from(ctx.version),
                                            raw_bytes: raw,
                                        });
                                    }
                                }
                            }
                            RiffNode::List {
                                list_type: [b't', b'r', b'f', b'l'],
                                ..
                            } => {}
                            RiffNode::Chunk { fourcc, data, .. } => {
                                if !matches!(
                                    fourcc,
                                    b"flgs" | b"spid" | b"bbox" | b"obbx" | b"ftil" | b"usdn"
                                ) {
                                    unknown_chunks.push(UnknownCdrChunk {
                                        fourcc: *fourcc,
                                        version_hint: u32::from(ctx.version),
                                        raw_bytes: data.clone(),
                                    });
                                }
                            }
                            RiffNode::List {
                                list_type: slt,
                                children: sch,
                                ..
                            } => {
                                let raw = serialize_list_body(slt, sch, ctx.endian)?;
                                unknown_chunks.push(UnknownCdrChunk {
                                    fourcc: *b"LIST",
                                    version_hint: u32::from(ctx.version),
                                    raw_bytes: raw,
                                });
                            }
                        }
                    }
                }
                b"trfl" => {}
                b"obj " => {
                    if let Some(oid) = parse_object_list(sub_ch, depth + 1, doc, ctx)? {
                        child_ids.push(oid);
                    }
                }
                b"grp " => {
                    if let Some(gid) = parse_group_list(sub_ch, depth + 1, doc, ctx)? {
                        child_ids.push(gid);
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
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"trfd" => {
                    if is_cdr_native_trfd(data) {
                        is_real_cdr_group = true;
                    } else if let Ok(xf) = parse_synthetic_trfd_chunk(data) {
                        transform = xf;
                    }
                }
                b"lens" => {
                    if let Ok(l) = parse_lens_chunk(data) {
                        lens = Some(l);
                    }
                }
                b"loda" => {
                    if let Ok(mut parsed) = parse_loda_chunk(data) {
                        is_real_cdr_group = parsed.is_real_cdr;
                        if let Some(p) = parsed.props {
                            name = p.name;
                            visible = p.visible;
                            locked = p.locked;
                            printable = p.printable;
                            opacity = p.opacity;
                            blend_mode = p.blend_mode;
                        } else if let Some(n) = parsed.real_name {
                            name = n;
                        }
                        effects.extend(parsed.effects);
                        if parsed.object_style_id.is_some() {
                            style_id = parsed.object_style_id;
                        }
                        if parsed.perspective.is_some() {
                            perspective = parsed.perspective;
                        }
                        if parsed.color_style_link.is_some() {
                            color_style_link = parsed.color_style_link;
                        }
                        if parsed.source_oid.is_some() {
                            source_oid = parsed.source_oid;
                        }
                        for (arg_typ, arg_bytes) in std::mem::take(&mut parsed.unknown_args) {
                            let mut raw = Vec::with_capacity(4 + arg_bytes.len());
                            raw.extend_from_slice(&arg_typ.to_le_bytes());
                            raw.extend_from_slice(&arg_bytes);
                            unknown_chunks.push(UnknownCdrChunk {
                                fourcc: *b"LODA",
                                version_hint: u32::from(ctx.version),
                                raw_bytes: raw,
                            });
                        }
                    }
                }
                b"flgs" | b"spid" | b"bbox" | b"obbx" | b"ftil" | b"usdn" => {}
                _ => {
                    unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(ctx.version),
                        raw_bytes: data.clone(),
                    });
                }
            },
        }
    }

    if is_real_cdr_group {
        transform = Affine2D::IDENTITY;
    }

    let gid = doc.alloc_object_id();
    if let Some(src_id) = source_oid {
        ctx.oid_remap.insert(src_id, gid);
    }
    let mut grp = SceneObject::new(gid, name, NodePayload::Group(child_ids));
    grp.transform = transform;
    grp.fill = Fill::None;
    grp.outline = OutlineStyle::none();
    grp.visible = visible;
    grp.locked = locked;
    grp.printable = printable;
    grp.opacity = opacity;
    grp.blend_mode = blend_mode;
    grp.lens = lens;
    grp.effects = effects;
    grp.style_id = style_id;
    grp.perspective = perspective;
    if let Some((cs_id, cs_slot)) = color_style_link {
        grp.color_style_id = cs_id;
        grp.color_style_slot = cs_slot;
    }
    grp.unknown_chunks = unknown_chunks;
    if doc.object_count() >= crate::limits::MAX_CDR_OBJECTS {
        return Err(CdrError::InvalidChunk {
            fourcc: "grp ".to_owned(),
            reason: format!(
                "CDR object count exceeds limit {}",
                crate::limits::MAX_CDR_OBJECTS
            ),
        });
    }
    let _ = doc.insert_loaded_object(grp);
    Ok(Some(gid))
}
