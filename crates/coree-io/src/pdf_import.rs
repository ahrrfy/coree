// SPDX-License-Identifier: MPL-2.0

//! Pure-Rust bounded PDF 1.0–2.0 (and PDF-compatible Adobe Illustrator `.ai`) importer.
//!
//! Features:
//! - Parses both traditional `xref` tables (including chained `/Prev` incremental updates
//!   and hybrid `/XRefStm`) AND PDF 1.5+ cross-reference streams (`/Type /XRef`) with
//!   compressed object streams (`/Type /ObjStm`).
//! - Bounded stream decompression: `/FlateDecode` (with cumulative 128 MiB cap & Zip-Bomb
//!   ratio guard), `/ASCII85Decode`, `/ASCIIHexDecode`, `/RunLengthDecode`, and `/Predictor`
//!   (TIFF horizontal differencing Predictor 2 & PNG None/Sub/Up/Average/Paeth 10–15).
//! - Full `/Pages` tree traversal with inherited `/MediaBox`, `/CropBox`, `/Rotate`, and
//!   `/Resources`, converting PDF points (`1 pt = 25.4 / 72 mm`) into exact millimeter pages.
//! - Content stream interpreter supporting graphics state stack (`q`/`Q`), transforms (`cm`),
//!   `ExtGState` (`gs`), RGB/CMYK/Gray/Named/ICCBased/Separation colors, strokes (`w`/`J`/`j`/`M`/`d`),
//!   paths (`m`/`l`/`c`/`v`/`y`/`re`/`h` + `f`/`F`/`f*`/`S`/`s`/`B`/`B*`/`b`/`b*`/`n`/`W`/`W*`),
//!   Form XObjects (`/Subtype /Form`), Image XObjects (`/Subtype /Image` with `/DCTDecode` JPEG,
//!   `/FlateDecode` Gray/RGB/CMYK/Indexed, `/SMask` alpha, and bounded RAM/downscaling), and
//!   Text (`BT`/`ET`/`Tf`/`Tm`/`Td`/`TD`/`T*`/`TL`/`Tj`/`TJ`/`'`/`"` with `/ToUnicode` CMap,
//!   UTF-16BE BOM, and WinAnsi/PDFDocEncoding).

use crate::error::{IoError, IoResult};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, ColorMode, Document, Fill, LayerId,
    LineCap, LineJoin, NodePayload, ObjectId, OutlineStyle, Point2D, RasterAsset, RasterLayerData,
    SceneObject, TextDirection, TextStyle, Vec2D,
};
use flate2::read::{DeflateDecoder, ZlibDecoder};
use image::{ImageFormat, ImageReader, Limits};
use kurbo::BezPath;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::Path;

const PT_TO_MM: f64 = 25.4 / 72.0;
const MM_TO_PT: f64 = 72.0 / 25.4;

/// Maximum input PDF file size read from disk (`256 MiB`).
const MAX_PDF_INPUT_BYTES: usize = 256 * 1024 * 1024;
/// Maximum cumulative decompressed bytes across all Flate/filter streams (`128 MiB`).
const MAX_CUMULATIVE_DECOMPRESSED: usize = 128 * 1024 * 1024;
/// Maximum single decompressed stream size (`64 MiB`).
const MAX_SINGLE_STREAM_DECOMPRESSED: usize = 64 * 1024 * 1024;
/// Minimum decoded bytes before enforcing compression-ratio Zip-Bomb rejection (`64 KiB`).
const MIN_RATIO_CHECK_BYTES: usize = 64 * 1024;
/// Maximum allowed decompression ratio above `MIN_RATIO_CHECK_BYTES`.
const MAX_DECOMPRESSION_RATIO: f64 = 250.0;
/// Maximum cumulative decoded RGBA image memory across all pages (`128 MiB`).
const MAX_PDF_RGBA_BUDGET: usize = 128 * 1024 * 1024;
/// Maximum raster dimension accepted per PDF Image XObject before downscaling/rejection.
const MAX_PDF_IMAGE_DIM: u32 = 16_384;
/// Maximum object / dict / array parser recursion depth.
const MAX_PARSE_DEPTH: usize = 64;
/// Maximum indirect reference resolution depth.
const MAX_REF_DEPTH: usize = 32;
/// Maximum Form XObject recursion depth.
const MAX_FORM_DEPTH: usize = 32;
/// Maximum graphics state stack depth (`q`/`Q`).
const MAX_GSTATE_DEPTH: usize = 128;
/// Maximum number of pages imported from a single PDF.
const MAX_PDF_PAGES: usize = 4_096;
/// Maximum number of content-stream operations per page.
const MAX_OPS_PER_PAGE: usize = 2_000_000;
/// Maximum chained `/Filter` entries per stream.
const MAX_FILTER_CHAIN_LEN: usize = 16;
/// Maximum `/Contents` array entries per page.
const MAX_PAGE_CONTENT_ITEMS: usize = 2_048;
/// Maximum concatenated content stream bytes per page (`32 MiB`).
const MAX_PAGE_CONTENT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum items in a single PDF dictionary or array.
const MAX_COLLECTION_ITEMS: usize = 250_000;
/// Maximum tokens parsed from a single content stream.
const MAX_CONTENT_TOKENS: usize = 4_000_000;

/// Imports a PDF (or PDF-compatible `.ai`) file from `path` into a multipage millimeter [`Document`].
pub fn import_pdf(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_PDF_INPUT_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PDF_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_pdf_bytes={MAX_PDF_INPUT_BYTES}"),
            actual: format!("file exceeds {MAX_PDF_INPUT_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported PDF");
    import_pdf_bytes(&bytes, title)
}

/// Imports a PDF 1.0–2.0 (or PDF-compatible `.ai`) byte slice into a multipage [`Document`].
pub fn import_pdf_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    if bytes.len() < 8 {
        return Err(pdf_err("input too short to be a valid PDF"));
    }
    if bytes.len() > MAX_PDF_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_pdf_bytes={MAX_PDF_INPUT_BYTES}"),
            actual: format!("{} bytes", bytes.len()),
        });
    }

    let header_offset = find_subsequence(&bytes[..bytes.len().min(1024)], b"%PDF-")
        .ok_or_else(|| pdf_err("missing '%PDF-' header signature"))?;

    let mut parser = PdfParser::new(bytes, header_offset);
    parser.load_xref()?;

    if parser.has_encrypt {
        return Err(pdf_err("encrypted PDF documents are not supported"));
    }

    let root_ref = parser
        .root_ref
        .ok_or_else(|| pdf_err("PDF trailer missing /Root catalog reference"))?;
    let catalog = parser.resolve_dict(&PdfObj::Ref(root_ref.0, root_ref.1), 0)?;

    // Extract optional /Info metadata (/Title, /Author, /Subject)
    let mut info_title: Option<String> = None;
    let mut info_author = String::new();
    let mut info_subject = String::new();
    if let Some(info_ref) = parser.info_ref {
        if let Ok(info) = parser.resolve_dict(&PdfObj::Ref(info_ref.0, info_ref.1), 0) {
            if let Some(PdfObj::Str(b)) = info.get("Title") {
                let s = decode_pdf_text_string(b);
                if !s.trim().is_empty() {
                    info_title = Some(s);
                }
            }
            if let Some(PdfObj::Str(b)) = info.get("Author") {
                info_author = decode_pdf_text_string(b);
            }
            if let Some(PdfObj::Str(b)) = info.get("Subject") {
                info_subject = decode_pdf_text_string(b);
            }
        }
    }

    // Traverse /Pages tree collecting all leaf pages with inherited attributes.
    let pages_obj = catalog
        .get("Pages")
        .cloned()
        .ok_or_else(|| pdf_err("PDF catalog missing /Pages tree"))?;
    let mut pages_spec = Vec::new();
    let mut visited_pages = HashSet::new();
    let inherited = InheritedPageProps::default();
    parser.collect_pages(
        &pages_obj,
        &inherited,
        0,
        &mut visited_pages,
        &mut pages_spec,
    )?;

    if pages_spec.is_empty() {
        return Err(pdf_err("PDF contains zero pages"));
    }

    let doc_title = info_title.unwrap_or_else(|| {
        if title.trim().is_empty() {
            "Imported PDF".to_owned()
        } else {
            title.to_owned()
        }
    });

    let first_spec = &pages_spec[0];
    let (first_w_mm, first_h_mm, _) = page_geometry(first_spec);
    let mut doc = Document::new(doc_title, first_w_mm, first_h_mm, ColorMode::Rgb, 300.0);
    doc.metadata.source_format = Some("pdf".to_owned());
    doc.metadata.author = info_author;
    doc.metadata.subject = info_subject;

    let total_pages = pages_spec.len();
    let per_image_budget = if total_pages <= 4 {
        16 * 1024 * 1024
    } else {
        (MAX_PDF_RGBA_BUDGET / total_pages).clamp(512 * 1024, 16 * 1024 * 1024)
    };
    let mut ctx = ImportContext {
        doc: &mut doc,
        saw_cmyk: false,
        saw_rgb: false,
        rgba_budget_remaining: MAX_PDF_RGBA_BUDGET,
        per_image_budget,
    };

    for (page_idx, spec) in pages_spec.iter().enumerate() {
        let (w_mm, h_mm, page_to_mm) = page_geometry(spec);
        let layer_id = if page_idx == 0 {
            ctx.doc.pages[0].width_mm = w_mm;
            ctx.doc.pages[0].height_mm = h_mm;
            ctx.doc.pages[0].layers[0].id
        } else {
            let pid = ctx
                .doc
                .add_page(format!("Page {}", page_idx + 1), w_mm, h_mm, 0.0);
            ctx.doc
                .pages
                .iter()
                .find(|p| p.id == pid)
                .map(|p| p.layers[0].id)
                .ok_or_else(|| pdf_err("failed to locate created page"))?
        };

        let content_bytes = parser.read_page_contents(&spec.dict)?;
        if !content_bytes.is_empty() {
            let mut interp = PdfContentInterpreter::new(
                &mut parser,
                &mut ctx,
                layer_id,
                page_to_mm,
                spec.resources.clone(),
            );
            interp.run(&content_bytes, 0)?;
        }
    }

    if ctx.saw_cmyk && !ctx.saw_rgb {
        ctx.doc.color_mode = ColorMode::Cmyk;
    }
    ctx.doc.refresh_document_palette();
    Ok(doc)
}

// =============================================================================
// PDF Object Model & Lexer/Parser
// =============================================================================

#[derive(Debug, Clone)]
enum PdfObj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(String),
    Array(Vec<PdfObj>),
    Dict(BTreeMap<String, PdfObj>),
    Stream {
        dict: BTreeMap<String, PdfObj>,
        start: usize,
        len: usize,
    },
    Ref(u32, u16),
}

impl PdfObj {
    fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Int(n) => Some(n as f64),
            Self::Real(v) if v.is_finite() => Some(v),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match *self {
            Self::Int(n) => Some(n),
            Self::Real(v) if v.is_finite() => Some(v.round() as i64),
            _ => None,
        }
    }

    fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_ref(&self) -> Option<(u32, u16)> {
        match *self {
            Self::Ref(num, gen_num) => Some((num, gen_num)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum XRefEntry {
    Free,
    InUse { offset: usize },
    Compressed { obj_stm_num: u32, index: usize },
}

struct ObjStmCache {
    decoded: Vec<u8>,
    first: usize,
    offsets: Vec<(u32, usize)>,
}

struct PdfParser<'a> {
    bytes: &'a [u8],
    header_offset: usize,
    xref: HashMap<u32, XRefEntry>,
    root_ref: Option<(u32, u16)>,
    info_ref: Option<(u32, u16)>,
    has_encrypt: bool,
    obj_stm_cache: HashMap<u32, ObjStmCache>,
    resolving_objs: HashSet<u32>,
    cumulative_decompressed: usize,
}

impl<'a> PdfParser<'a> {
    fn new(bytes: &'a [u8], header_offset: usize) -> Self {
        Self {
            bytes,
            header_offset,
            xref: HashMap::new(),
            root_ref: None,
            info_ref: None,
            has_encrypt: false,
            obj_stm_cache: HashMap::new(),
            resolving_objs: HashSet::new(),
            cumulative_decompressed: 0,
        }
    }

    fn charge_cumulative_bytes(&mut self, added: usize, label: &str) -> IoResult<()> {
        let next = self.cumulative_decompressed.saturating_add(added);
        if next > MAX_CUMULATIVE_DECOMPRESSED {
            return Err(IoError::ZipBomb {
                reason: format!(
                    "cumulative PDF {label} output exceeds {MAX_CUMULATIVE_DECOMPRESSED} bytes"
                ),
            });
        }
        self.cumulative_decompressed = next;
        Ok(())
    }

    fn load_xref(&mut self) -> IoResult<()> {
        let startxref_offset = find_last_startxref(self.bytes);
        let mut parsed_any = false;

        if let Some(mut xref_off) = startxref_offset {
            let mut visited = HashSet::new();
            for _ in 0..64 {
                let abs_off = self.header_offset.saturating_add(xref_off);
                if abs_off >= self.bytes.len() || !visited.insert(abs_off) {
                    break;
                }
                match self.parse_xref_at(abs_off)? {
                    Some(prev) => {
                        parsed_any = true;
                        xref_off = prev;
                    }
                    None => {
                        parsed_any = true;
                        break;
                    }
                }
            }
        }

        if !parsed_any || self.root_ref.is_none() {
            self.fallback_scan_objects()?;
        }

        Ok(())
    }

    /// Parses either a traditional `xref` table + `trailer` OR a PDF 1.5+ `/Type /XRef` stream.
    /// Returns `Ok(Some(prev_offset))` when `/Prev` chains to an older xref section.
    fn parse_xref_at(&mut self, offset: usize) -> IoResult<Option<usize>> {
        let mut pos = offset;
        skip_pdf_ws(self.bytes, &mut pos);
        if pos >= self.bytes.len() {
            return Ok(None);
        }

        if self.bytes[pos..].starts_with(b"xref") {
            pos += 4;
            skip_pdf_ws(self.bytes, &mut pos);

            // Parse traditional xref subsections until `trailer`
            while pos < self.bytes.len() {
                skip_pdf_ws(self.bytes, &mut pos);
                if self.bytes[pos..].starts_with(b"trailer") {
                    pos += 7;
                    break;
                }
                let Some(first_obj) = read_ascii_uint(self.bytes, &mut pos) else {
                    break;
                };
                skip_pdf_ws(self.bytes, &mut pos);
                let Some(count) = read_ascii_uint(self.bytes, &mut pos) else {
                    break;
                };
                let count = (count as usize).min(1_000_000);

                for idx in 0..count {
                    skip_pdf_ws(self.bytes, &mut pos);
                    let Some(ent_off) = read_ascii_uint(self.bytes, &mut pos) else {
                        break;
                    };
                    skip_pdf_ws(self.bytes, &mut pos);
                    let Some(_ent_gen) = read_ascii_uint(self.bytes, &mut pos) else {
                        break;
                    };
                    skip_pdf_ws(self.bytes, &mut pos);
                    if pos >= self.bytes.len() {
                        break;
                    }
                    let flag = self.bytes[pos];
                    pos += 1;
                    let obj_num = (first_obj as u32).saturating_add(idx as u32);
                    if flag == b'n' {
                        let abs = self.header_offset.saturating_add(ent_off as usize);
                        self.xref
                            .entry(obj_num)
                            .or_insert(XRefEntry::InUse { offset: abs });
                    } else if flag == b'f' {
                        self.xref.entry(obj_num).or_insert(XRefEntry::Free);
                    }
                }
            }

            skip_pdf_ws(self.bytes, &mut pos);
            let trailer_obj = self.parse_obj_at(self.bytes, &mut pos, 0)?;
            let PdfObj::Dict(trailer) = trailer_obj else {
                return Ok(None);
            };
            self.record_trailer_refs(&trailer);

            // Hybrid-reference PDF 1.5+ files may point to an XRef stream via /XRefStm
            if let Some(stm_off) = trailer.get("XRefStm").and_then(PdfObj::as_i64) {
                if stm_off > 0 {
                    let abs_stm = self.header_offset.saturating_add(stm_off as usize);
                    if abs_stm < self.bytes.len() {
                        let _ = self.parse_xref_stream_at(abs_stm);
                    }
                }
            }

            let prev = trailer
                .get("Prev")
                .and_then(PdfObj::as_i64)
                .filter(|&p| p >= 0)
                .map(|p| p as usize);
            return Ok(prev);
        }

        self.parse_xref_stream_at(offset)
    }

    fn parse_xref_stream_at(&mut self, offset: usize) -> IoResult<Option<usize>> {
        let mut pos = offset;
        let Some((_obj_num, obj)) = self.parse_indirect_obj_at(&mut pos, 0)? else {
            return Ok(None);
        };
        let PdfObj::Stream { dict, start, len } = obj else {
            return Ok(None);
        };
        self.record_trailer_refs(&dict);

        let w_arr = match dict.get("W") {
            Some(PdfObj::Array(a)) if a.len() >= 3 => a,
            _ => return Ok(None),
        };
        let w0 = w_arr[0].as_i64().unwrap_or(0).clamp(0, 8) as usize;
        let w1 = w_arr[1].as_i64().unwrap_or(0).clamp(0, 8) as usize;
        let w2 = w_arr[2].as_i64().unwrap_or(0).clamp(0, 8) as usize;
        let row_size = w0 + w1 + w2;
        if row_size == 0 {
            return Ok(None);
        }

        let size = dict
            .get("Size")
            .and_then(PdfObj::as_i64)
            .unwrap_or(0)
            .max(0) as u32;
        let mut ranges: Vec<(u32, usize)> = Vec::new();
        if let Some(PdfObj::Array(index_arr)) = dict.get("Index") {
            for pair in index_arr.as_chunks::<2>().0 {
                let st = pair[0].as_i64().unwrap_or(0).max(0) as u32;
                let cnt = pair[1].as_i64().unwrap_or(0).max(0) as usize;
                ranges.push((st, cnt));
            }
        } else {
            ranges.push((0, size as usize));
        }

        let raw = &self.bytes[start..start.saturating_add(len).min(self.bytes.len())];
        let decoded = self.decode_stream_bytes(&dict, raw)?;

        let mut cursor = 0usize;
        for (start_obj, count) in ranges {
            for i in 0..count {
                if cursor + row_size > decoded.len() {
                    break;
                }
                let f0 = if w0 == 0 {
                    1u64
                } else {
                    read_be_uint(&decoded[cursor..cursor + w0])
                };
                let f1 = read_be_uint(&decoded[cursor + w0..cursor + w0 + w1]);
                let f2 = read_be_uint(&decoded[cursor + w0 + w1..cursor + row_size]);
                cursor += row_size;

                let obj_num = start_obj.saturating_add(i as u32);
                let entry = match f0 {
                    0 => XRefEntry::Free,
                    1 => XRefEntry::InUse {
                        offset: self.header_offset.saturating_add(f1 as usize),
                    },
                    2 => XRefEntry::Compressed {
                        obj_stm_num: f1 as u32,
                        index: f2 as usize,
                    },
                    _ => continue,
                };
                self.xref.entry(obj_num).or_insert(entry);
            }
        }

        let prev = dict
            .get("Prev")
            .and_then(PdfObj::as_i64)
            .filter(|&p| p >= 0)
            .map(|p| p as usize);
        Ok(prev)
    }

    fn record_trailer_refs(&mut self, dict: &BTreeMap<String, PdfObj>) {
        if self.root_ref.is_none() {
            if let Some(r) = dict.get("Root").and_then(PdfObj::as_ref) {
                self.root_ref = Some(r);
            }
        }
        if self.info_ref.is_none() {
            if let Some(r) = dict.get("Info").and_then(PdfObj::as_ref) {
                self.info_ref = Some(r);
            }
        }
        if dict.contains_key("Encrypt") {
            self.has_encrypt = true;
        }
    }

    /// Fallback scan for PDFs with damaged or missing xref tables.
    fn fallback_scan_objects(&mut self) -> IoResult<()> {
        let len = self.bytes.len();
        let mut i = self.header_offset;
        while i + 6 < len {
            if self.bytes[i].is_ascii_digit()
                && (i == 0
                    || matches!(
                        self.bytes[i - 1],
                        b'\n' | b'\r' | b' ' | b'\t' | b'>' | b']'
                    ))
            {
                let start = i;
                let mut p = i;
                if let Some(obj_num) = read_ascii_uint(self.bytes, &mut p) {
                    let s1 = p;
                    skip_inline_spaces(self.bytes, &mut p);
                    if p > s1 {
                        if let Some(_gen_num) = read_ascii_uint(self.bytes, &mut p) {
                            let s2 = p;
                            skip_inline_spaces(self.bytes, &mut p);
                            if p > s2
                                && self.bytes[p..].starts_with(b"obj")
                                && self
                                    .bytes
                                    .get(p + 3)
                                    .is_none_or(|&b| is_pdf_ws(b) || is_pdf_delim(b))
                            {
                                self.xref
                                    .entry(obj_num as u32)
                                    .or_insert(XRefEntry::InUse { offset: start });
                                // Detect catalog if /Root wasn't found in trailer
                                if self.root_ref.is_none() {
                                    let probe_end = (p + 256).min(len);
                                    if find_subsequence(&self.bytes[p..probe_end], b"/Catalog")
                                        .is_some()
                                    {
                                        self.root_ref = Some((obj_num as u32, 0));
                                    }
                                }
                                i = p + 3;
                                continue;
                            }
                        }
                    }
                }
            }
            i += 1;
        }
        Ok(())
    }

    fn resolve(&mut self, obj: &PdfObj, depth: usize) -> IoResult<PdfObj> {
        if depth > MAX_REF_DEPTH {
            return Err(IoError::LimitExceeded {
                limit: format!("max_pdf_ref_depth={MAX_REF_DEPTH}"),
                actual: format!("depth={depth}"),
            });
        }
        let PdfObj::Ref(obj_num, _gen_num) = *obj else {
            return Ok(obj.clone());
        };
        if !self.resolving_objs.insert(obj_num) {
            // Cyclic indirect object reference (e.g. self-referential /Length 1 0 R)
            return Ok(PdfObj::Null);
        }
        let result = (|| {
            let Some(entry) = self.xref.get(&obj_num).copied() else {
                return Ok(PdfObj::Null);
            };
            match entry {
                XRefEntry::Free => Ok(PdfObj::Null),
                XRefEntry::InUse { offset } => {
                    if offset >= self.bytes.len() {
                        return Ok(PdfObj::Null);
                    }
                    let mut pos = offset;
                    match self.parse_indirect_obj_at(&mut pos, depth + 1)? {
                        Some((_, val)) => {
                            if matches!(val, PdfObj::Ref(..)) {
                                self.resolve(&val, depth + 1)
                            } else {
                                Ok(val)
                            }
                        }
                        None => Ok(PdfObj::Null),
                    }
                }
                XRefEntry::Compressed { obj_stm_num, index } => {
                    self.resolve_compressed_obj(obj_stm_num, obj_num, index, depth + 1)
                }
            }
        })();
        self.resolving_objs.remove(&obj_num);
        result
    }

    fn resolve_dict(&mut self, obj: &PdfObj, depth: usize) -> IoResult<BTreeMap<String, PdfObj>> {
        match self.resolve(obj, depth)? {
            PdfObj::Dict(d) | PdfObj::Stream { dict: d, .. } => Ok(d),
            _ => Err(pdf_err("expected PDF dictionary object")),
        }
    }

    fn resolve_compressed_obj(
        &mut self,
        obj_stm_num: u32,
        target_obj_num: u32,
        index: usize,
        depth: usize,
    ) -> IoResult<PdfObj> {
        if depth > MAX_REF_DEPTH {
            return Ok(PdfObj::Null);
        }
        if !self.obj_stm_cache.contains_key(&obj_stm_num) {
            let stm_obj = self.resolve(&PdfObj::Ref(obj_stm_num, 0), depth + 1)?;
            let PdfObj::Stream { dict, start, len } = stm_obj else {
                return Ok(PdfObj::Null);
            };
            let first = dict
                .get("First")
                .and_then(PdfObj::as_i64)
                .unwrap_or(0)
                .max(0) as usize;
            let n = dict
                .get("N")
                .and_then(PdfObj::as_i64)
                .unwrap_or(0)
                .clamp(0, 100_000) as usize;
            let raw = &self.bytes[start..start.saturating_add(len).min(self.bytes.len())];
            let decoded = self.decode_stream_bytes(&dict, raw)?;

            let mut hpos = 0usize;
            let mut offsets = Vec::with_capacity(n);
            for _ in 0..n {
                skip_pdf_ws(&decoded, &mut hpos);
                let Some(num) = read_ascii_uint(&decoded, &mut hpos) else {
                    break;
                };
                skip_pdf_ws(&decoded, &mut hpos);
                let Some(rel) = read_ascii_uint(&decoded, &mut hpos) else {
                    break;
                };
                offsets.push((num as u32, rel as usize));
            }
            self.obj_stm_cache.insert(
                obj_stm_num,
                ObjStmCache {
                    decoded,
                    first,
                    offsets,
                },
            );
        }

        let Some(cache) = self.obj_stm_cache.get(&obj_stm_num) else {
            return Ok(PdfObj::Null);
        };
        let rel_offset = cache
            .offsets
            .get(index)
            .filter(|&&(num, _)| num == target_obj_num)
            .or_else(|| {
                cache
                    .offsets
                    .iter()
                    .find(|&&(num, _)| num == target_obj_num)
            })
            .map(|&(_, rel)| rel);
        let Some(rel) = rel_offset else {
            return Ok(PdfObj::Null);
        };
        let mut p = cache.first.saturating_add(rel);
        if p >= cache.decoded.len() {
            return Ok(PdfObj::Null);
        }
        // Parse directly from the cached decoded buffer
        parse_obj_from_slice(&cache.decoded, &mut p, 0)
    }

    fn parse_indirect_obj_at(
        &mut self,
        pos: &mut usize,
        depth: usize,
    ) -> IoResult<Option<(u32, PdfObj)>> {
        if depth > MAX_REF_DEPTH {
            return Err(IoError::LimitExceeded {
                limit: format!("max_pdf_ref_depth={MAX_REF_DEPTH}"),
                actual: format!("depth={depth}"),
            });
        }
        skip_pdf_ws(self.bytes, pos);
        let Some(obj_num) = read_ascii_uint(self.bytes, pos) else {
            return Ok(None);
        };
        skip_pdf_ws(self.bytes, pos);
        let Some(_gen_num) = read_ascii_uint(self.bytes, pos) else {
            return Ok(None);
        };
        skip_pdf_ws(self.bytes, pos);
        if !self.bytes[*pos..].starts_with(b"obj") {
            return Ok(None);
        }
        *pos += 3;
        skip_pdf_ws(self.bytes, pos);

        let val = self.parse_obj_at(self.bytes, pos, 0)?;
        if let PdfObj::Dict(dict) = val {
            let save = *pos;
            skip_pdf_ws(self.bytes, pos);
            if self.bytes[*pos..].starts_with(b"stream") {
                *pos += 6;
                // Per PDF spec §7.3.8.1: keyword `stream` is followed by CRLF or LF (not CR alone).
                if self.bytes.get(*pos) == Some(&b'\r') && self.bytes.get(*pos + 1) == Some(&b'\n')
                {
                    *pos += 2;
                } else if self.bytes.get(*pos) == Some(&b'\n')
                    || self.bytes.get(*pos) == Some(&b'\r')
                {
                    *pos += 1;
                }
                let stream_start = *pos;
                let resolved_len = match dict.get("Length") {
                    Some(PdfObj::Int(n)) if *n >= 0 => Some(*n as usize),
                    Some(r @ PdfObj::Ref(..)) => self
                        .resolve(r, depth + 1)
                        .ok()
                        .and_then(|o| o.as_i64())
                        .filter(|&n| n >= 0)
                        .map(|n| n as usize),
                    _ => None,
                };

                let stream_len = if let Some(l) = resolved_len {
                    if stream_start.saturating_add(l) <= self.bytes.len() {
                        l
                    } else {
                        find_subsequence(&self.bytes[stream_start..], b"endstream").unwrap_or(0)
                    }
                } else {
                    find_subsequence(&self.bytes[stream_start..], b"endstream").unwrap_or(0)
                };

                *pos = stream_start
                    .saturating_add(stream_len)
                    .min(self.bytes.len());
                return Ok(Some((
                    obj_num as u32,
                    PdfObj::Stream {
                        dict,
                        start: stream_start,
                        len: stream_len,
                    },
                )));
            }
            *pos = save;
            return Ok(Some((obj_num as u32, PdfObj::Dict(dict))));
        }

        Ok(Some((obj_num as u32, val)))
    }

    fn parse_obj_at(&self, data: &[u8], pos: &mut usize, depth: usize) -> IoResult<PdfObj> {
        parse_obj_from_slice(data, pos, depth)
    }

    fn decode_stream_obj(&mut self, obj: &PdfObj) -> IoResult<Vec<u8>> {
        let resolved = self.resolve(obj, 0)?;
        let PdfObj::Stream { dict, start, len } = resolved else {
            return Ok(Vec::new());
        };
        let raw = &self.bytes[start..start.saturating_add(len).min(self.bytes.len())];
        self.decode_stream_bytes(&dict, raw)
    }

    fn decode_stream_bytes(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        raw: &[u8],
    ) -> IoResult<Vec<u8>> {
        let filters = self.extract_filter_chain(dict, 0)?;
        if filters.len() > MAX_FILTER_CHAIN_LEN {
            return Err(IoError::LimitExceeded {
                limit: format!("max_pdf_filter_chain={MAX_FILTER_CHAIN_LEN}"),
                actual: format!("{} filters", filters.len()),
            });
        }
        let parms = self.extract_decode_parms(dict, 0)?;

        let mut data = raw.to_vec();
        for (idx, filter) in filters.iter().enumerate() {
            let dp = parms.get(idx).and_then(|p| p.as_ref());
            let rem = MAX_CUMULATIVE_DECOMPRESSED.saturating_sub(self.cumulative_decompressed);
            let step_cap = MAX_SINGLE_STREAM_DECOMPRESSED.min(rem);
            match filter.as_str() {
                "FlateDecode" | "Fl" => {
                    data = self.flate_decode_bounded(&data)?;
                    if let Some(dp_dict) = dp {
                        data = apply_pdf_predictor(&data, dp_dict)?;
                    }
                }
                "ASCIIHexDecode" | "AHx" => {
                    data = decode_ascii_hex(&data, step_cap)?;
                    self.charge_cumulative_bytes(data.len(), "ASCIIHexDecode")?;
                }
                "ASCII85Decode" | "A85" => {
                    data = decode_ascii85(&data, step_cap)?;
                    self.charge_cumulative_bytes(data.len(), "ASCII85Decode")?;
                }
                "RunLengthDecode" | "RL" => {
                    data = decode_run_length(&data, step_cap)?;
                    self.charge_cumulative_bytes(data.len(), "RunLengthDecode")?;
                }
                "DCTDecode" | "DCT" => {
                    // DCTDecode is handled directly by the JPEG image decoder; stop filter chain here.
                    break;
                }
                _ => {
                    // Unsupported filter (e.g. JBIG2/CCITT/JPX): leave bytes as-is so callers skip gracefully.
                    break;
                }
            }
        }
        Ok(data)
    }

    fn extract_filter_chain(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        depth: usize,
    ) -> IoResult<Vec<String>> {
        let Some(f_obj) = dict.get("Filter").cloned() else {
            return Ok(Vec::new());
        };
        match self.resolve(&f_obj, depth + 1)? {
            PdfObj::Name(n) => Ok(vec![n]),
            PdfObj::Array(arr) => {
                if arr.len() > MAX_FILTER_CHAIN_LEN {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_pdf_filter_chain={MAX_FILTER_CHAIN_LEN}"),
                        actual: format!("{} filters", arr.len()),
                    });
                }
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    if let PdfObj::Name(n) = self.resolve(&item, depth + 1)? {
                        out.push(n);
                    }
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn extract_decode_parms(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        depth: usize,
    ) -> IoResult<Vec<Option<BTreeMap<String, PdfObj>>>> {
        let Some(dp_obj) = dict.get("DecodeParms").cloned() else {
            return Ok(Vec::new());
        };
        match self.resolve(&dp_obj, depth + 1)? {
            PdfObj::Dict(d) => Ok(vec![Some(d)]),
            PdfObj::Array(arr) => {
                let mut out = Vec::with_capacity(arr.len().min(MAX_FILTER_CHAIN_LEN));
                for item in arr.into_iter().take(MAX_FILTER_CHAIN_LEN) {
                    match self.resolve(&item, depth + 1)? {
                        PdfObj::Dict(d) => out.push(Some(d)),
                        _ => out.push(None),
                    }
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn flate_decode_bounded(&mut self, compressed: &[u8]) -> IoResult<Vec<u8>> {
        // Trim leading whitespace that some PDFs place before zlib header
        let start = compressed
            .iter()
            .position(|b| !matches!(b, b'\r' | b'\n'))
            .unwrap_or(0);
        let input = &compressed[start..];

        match self.flate_read_loop(ZlibDecoder::new(input), input.len()) {
            Ok(data) => Ok(data),
            Err(IoError::ZipBomb { reason }) => Err(IoError::ZipBomb { reason }),
            Err(IoError::LimitExceeded { limit, actual }) => {
                Err(IoError::LimitExceeded { limit, actual })
            }
            Err(_) => {
                // Fallback to raw deflate when zlib wrapper is omitted/non-standard
                self.flate_read_loop(DeflateDecoder::new(input), input.len())
            }
        }
    }

    fn flate_read_loop<R: Read>(
        &mut self,
        mut decoder: R,
        compressed_len: usize,
    ) -> IoResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let denom = compressed_len.max(1) as f64;
        loop {
            let n = decoder.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            let next_len = out.len().saturating_add(n);
            if next_len > MAX_SINGLE_STREAM_DECOMPRESSED {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_pdf_stream_bytes={MAX_SINGLE_STREAM_DECOMPRESSED}"),
                    actual: format!("stream exceeds {MAX_SINGLE_STREAM_DECOMPRESSED} bytes"),
                });
            }
            let next_cum = self.cumulative_decompressed.saturating_add(n);
            if next_cum > MAX_CUMULATIVE_DECOMPRESSED {
                return Err(IoError::ZipBomb {
                    reason: format!(
                        "cumulative PDF FlateDecode output exceeds {MAX_CUMULATIVE_DECOMPRESSED} bytes"
                    ),
                });
            }
            if next_len >= MIN_RATIO_CHECK_BYTES {
                let ratio = (next_len as f64) / denom;
                if ratio > MAX_DECOMPRESSION_RATIO {
                    return Err(IoError::ZipBomb {
                        reason: format!(
                            "PDF FlateDecode ratio {ratio:.1}x exceeds limit {MAX_DECOMPRESSION_RATIO:.1}x ({next_len}B / {compressed_len}B)"
                        ),
                    });
                }
            }
            self.cumulative_decompressed = next_cum;
            out.extend_from_slice(&chunk[..n]);
        }
        Ok(out)
    }

    fn collect_pages(
        &mut self,
        node_obj: &PdfObj,
        inherited: &InheritedPageProps,
        depth: usize,
        visited: &mut HashSet<u32>,
        out: &mut Vec<PdfPageSpec>,
    ) -> IoResult<()> {
        if depth > MAX_PARSE_DEPTH || out.len() >= MAX_PDF_PAGES {
            return Ok(());
        }
        if let Some((num, _)) = node_obj.as_ref() {
            if !visited.insert(num) {
                return Ok(());
            }
        }
        let dict = match self.resolve_dict(node_obj, 0) {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };

        let mut props = inherited.clone();
        if let Some(mb) = dict.get("MediaBox").cloned() {
            if let Some(rect) = self.resolve_rect(&mb) {
                props.media_box = Some(rect);
            }
        }
        if let Some(cb) = dict.get("CropBox").cloned() {
            if let Some(rect) = self.resolve_rect(&cb) {
                props.crop_box = Some(rect);
            }
        }
        if let Some(rot) = dict.get("Rotate").and_then(PdfObj::as_i64) {
            props.rotate = rot.rem_euclid(360) as i32;
        }
        if let Some(res_obj) = dict.get("Resources").cloned() {
            if let Ok(res_dict) = self.resolve_dict(&res_obj, 0) {
                props.resources = res_dict;
            }
        }

        let node_type = dict.get("Type").and_then(PdfObj::as_name).unwrap_or("");
        if node_type == "Pages" || dict.contains_key("Kids") {
            if let Some(kids_obj) = dict.get("Kids").cloned() {
                if let PdfObj::Array(kids) = self.resolve(&kids_obj, 0)? {
                    for kid in kids {
                        self.collect_pages(&kid, &props, depth + 1, visited, out)?;
                        if out.len() >= MAX_PDF_PAGES {
                            break;
                        }
                    }
                }
            }
        } else {
            out.push(PdfPageSpec {
                dict,
                media_box: props.media_box.unwrap_or([0.0, 0.0, 595.2756, 841.8898]),
                crop_box: props.crop_box,
                rotate: props.rotate,
                resources: props.resources,
            });
        }

        Ok(())
    }

    fn resolve_rect(&mut self, obj: &PdfObj) -> Option<[f64; 4]> {
        let resolved = self.resolve(obj, 0).ok()?;
        let PdfObj::Array(arr) = resolved else {
            return None;
        };
        if arr.len() < 4 {
            return None;
        }
        let a = arr[0].as_f64()?;
        let b = arr[1].as_f64()?;
        let c = arr[2].as_f64()?;
        let d = arr[3].as_f64()?;
        Some([a.min(c), b.min(d), a.max(c), b.max(d)])
    }

    fn read_page_contents(&mut self, page_dict: &BTreeMap<String, PdfObj>) -> IoResult<Vec<u8>> {
        let Some(contents_obj) = page_dict.get("Contents").cloned() else {
            return Ok(Vec::new());
        };
        match self.resolve(&contents_obj, 0)? {
            PdfObj::Stream { dict, start, len } => {
                let raw = &self.bytes[start..start.saturating_add(len).min(self.bytes.len())];
                let out = self.decode_stream_bytes(&dict, raw)?;
                if out.len() > MAX_PAGE_CONTENT_BYTES {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_page_content_bytes={MAX_PAGE_CONTENT_BYTES}"),
                        actual: format!("{} bytes", out.len()),
                    });
                }
                Ok(out)
            }
            PdfObj::Array(items) => {
                if items.len() > MAX_PAGE_CONTENT_ITEMS {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_page_content_items={MAX_PAGE_CONTENT_ITEMS}"),
                        actual: format!("{} content stream entries", items.len()),
                    });
                }
                let mut combined = Vec::new();
                for item in items {
                    let part = self.decode_stream_obj(&item)?;
                    if !part.is_empty() {
                        let next_len = combined.len().saturating_add(part.len()).saturating_add(1);
                        if next_len > MAX_PAGE_CONTENT_BYTES {
                            return Err(IoError::ZipBomb {
                                reason: format!(
                                    "concatenated page /Contents exceeds {MAX_PAGE_CONTENT_BYTES} bytes"
                                ),
                            });
                        }
                        self.charge_cumulative_bytes(part.len(), "Contents")?;
                        if !combined.is_empty() {
                            combined.push(b'\n');
                        }
                        combined.extend_from_slice(&part);
                    }
                }
                Ok(combined)
            }
            _ => Ok(Vec::new()),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct InheritedPageProps {
    media_box: Option<[f64; 4]>,
    crop_box: Option<[f64; 4]>,
    rotate: i32,
    resources: BTreeMap<String, PdfObj>,
}

struct PdfPageSpec {
    dict: BTreeMap<String, PdfObj>,
    media_box: [f64; 4],
    crop_box: Option<[f64; 4]>,
    rotate: i32,
    resources: BTreeMap<String, PdfObj>,
}

/// Computes `(width_mm, height_mm, page_to_mm_affine)` from a page's MediaBox/CropBox and /Rotate.
fn page_geometry(spec: &PdfPageSpec) -> (f64, f64, Affine2D) {
    let [llx, lly, urx, ury] = spec.crop_box.unwrap_or(spec.media_box);
    let bw = (urx - llx).abs().max(1.0);
    let bh = (ury - lly).abs().max(1.0);
    let rot = spec.rotate.rem_euclid(360);
    match rot {
        90 => (
            (bh * PT_TO_MM).max(1.0),
            (bw * PT_TO_MM).max(1.0),
            Affine2D::new(
                0.0,
                PT_TO_MM,
                PT_TO_MM,
                0.0,
                -lly * PT_TO_MM,
                -llx * PT_TO_MM,
            ),
        ),
        180 => (
            (bw * PT_TO_MM).max(1.0),
            (bh * PT_TO_MM).max(1.0),
            Affine2D::new(
                -PT_TO_MM,
                0.0,
                0.0,
                PT_TO_MM,
                urx * PT_TO_MM,
                -lly * PT_TO_MM,
            ),
        ),
        270 => (
            (bh * PT_TO_MM).max(1.0),
            (bw * PT_TO_MM).max(1.0),
            Affine2D::new(
                0.0,
                -PT_TO_MM,
                -PT_TO_MM,
                0.0,
                ury * PT_TO_MM,
                urx * PT_TO_MM,
            ),
        ),
        _ => (
            (bw * PT_TO_MM).max(1.0),
            (bh * PT_TO_MM).max(1.0),
            Affine2D::new(
                PT_TO_MM,
                0.0,
                0.0,
                -PT_TO_MM,
                -llx * PT_TO_MM,
                ury * PT_TO_MM,
            ),
        ),
    }
}

// =============================================================================
// PDF Object Slice Parser & Stream Filter Decoders
// =============================================================================

fn parse_obj_from_slice(data: &[u8], pos: &mut usize, depth: usize) -> IoResult<PdfObj> {
    if depth > MAX_PARSE_DEPTH {
        return Err(IoError::LimitExceeded {
            limit: format!("max_pdf_parse_depth={MAX_PARSE_DEPTH}"),
            actual: format!("depth={depth}"),
        });
    }
    skip_pdf_ws(data, pos);
    if *pos >= data.len() {
        return Ok(PdfObj::Null);
    }

    let b = data[*pos];
    match b {
        b'(' => Ok(PdfObj::Str(read_pdf_literal_string(data, pos))),
        b'<' => {
            if data.get(*pos + 1) == Some(&b'<') {
                *pos += 2;
                let mut dict = BTreeMap::new();
                loop {
                    skip_pdf_ws(data, pos);
                    if *pos >= data.len() {
                        break;
                    }
                    if data[*pos] == b'>' && data.get(*pos + 1) == Some(&b'>') {
                        *pos += 2;
                        break;
                    }
                    if dict.len() >= MAX_COLLECTION_ITEMS {
                        return Err(IoError::LimitExceeded {
                            limit: format!("max_pdf_collection_items={MAX_COLLECTION_ITEMS}"),
                            actual: "dictionary entry limit exceeded".into(),
                        });
                    }
                    let before = *pos;
                    let key_obj = parse_obj_from_slice(data, pos, depth + 1)?;
                    let PdfObj::Name(key) = key_obj else {
                        // Guarantee forward progress on any unexpected token inside dict
                        if *pos == before && *pos < data.len() {
                            *pos += 1;
                        }
                        continue;
                    };
                    let val = parse_obj_from_slice(data, pos, depth + 1)?;
                    dict.insert(key, val);
                }
                Ok(PdfObj::Dict(dict))
            } else {
                Ok(PdfObj::Str(read_pdf_hex_string(data, pos)))
            }
        }
        b'[' => {
            *pos += 1;
            let mut arr = Vec::new();
            loop {
                skip_pdf_ws(data, pos);
                if *pos >= data.len() {
                    break;
                }
                if data[*pos] == b']' {
                    *pos += 1;
                    break;
                }
                if arr.len() >= MAX_COLLECTION_ITEMS {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_pdf_collection_items={MAX_COLLECTION_ITEMS}"),
                        actual: "array element limit exceeded".into(),
                    });
                }
                let before = *pos;
                let elem = parse_obj_from_slice(data, pos, depth + 1)?;
                if *pos == before && *pos < data.len() {
                    *pos += 1;
                }
                arr.push(elem);
            }
            Ok(PdfObj::Array(arr))
        }
        b'/' => Ok(PdfObj::Name(read_pdf_name(data, pos))),
        _ => {
            let word = read_pdf_word(data, pos);
            if word.is_empty() {
                // Unexpected delimiter byte (e.g. `]`, `)`, `}`, `>`) — consume 1 byte to guarantee progress.
                if *pos < data.len() {
                    *pos += 1;
                }
                return Ok(PdfObj::Null);
            }
            match word.as_str() {
                "null" => Ok(PdfObj::Null),
                "true" => Ok(PdfObj::Bool(true)),
                "false" => Ok(PdfObj::Bool(false)),
                _ => {
                    if let Ok(iv) = word.parse::<i64>() {
                        // Look ahead for `<gen> R` indirect reference!
                        if iv >= 0 {
                            let save = *pos;
                            skip_pdf_ws(data, pos);
                            if let Some(gen_val) = read_ascii_uint(data, pos) {
                                skip_pdf_ws(data, pos);
                                if data.get(*pos) == Some(&b'R')
                                    && data
                                        .get(*pos + 1)
                                        .is_none_or(|&c| is_pdf_ws(c) || is_pdf_delim(c))
                                {
                                    *pos += 1;
                                    return Ok(PdfObj::Ref(iv as u32, gen_val as u16));
                                }
                            }
                            *pos = save;
                        }
                        Ok(PdfObj::Int(iv))
                    } else if let Ok(rv) = word.parse::<f64>() {
                        Ok(PdfObj::Real(rv))
                    } else {
                        Ok(PdfObj::Null)
                    }
                }
            }
        }
    }
}

fn read_pdf_name(data: &[u8], pos: &mut usize) -> String {
    *pos += 1; // skip '/'
    let mut raw = Vec::new();
    while *pos < data.len() && !is_pdf_ws(data[*pos]) && !is_pdf_delim(data[*pos]) {
        if data[*pos] == b'#' && *pos + 2 < data.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(data[*pos + 1]), hex_nibble(data[*pos + 2])) {
                raw.push((hi << 4) | lo);
                *pos += 3;
                continue;
            }
        }
        raw.push(data[*pos]);
        *pos += 1;
    }
    String::from_utf8_lossy(&raw).into_owned()
}

fn read_pdf_word(data: &[u8], pos: &mut usize) -> String {
    let start = *pos;
    while *pos < data.len() && !is_pdf_ws(data[*pos]) && !is_pdf_delim(data[*pos]) {
        *pos += 1;
    }
    String::from_utf8_lossy(&data[start..*pos]).into_owned()
}

fn read_pdf_literal_string(data: &[u8], pos: &mut usize) -> Vec<u8> {
    let mut out = Vec::new();
    let len = data.len();
    *pos += 1; // skip '('
    let mut depth = 1u32;
    while *pos < len && depth > 0 {
        let b = data[*pos];
        *pos += 1;
        match b {
            b'(' => {
                depth += 1;
                out.push(b'(');
            }
            b')' => {
                depth -= 1;
                if depth > 0 {
                    out.push(b')');
                }
            }
            b'\\' => {
                if *pos >= len {
                    break;
                }
                let esc = data[*pos];
                *pos += 1;
                match esc {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'\n' => {}
                    b'\r' => {
                        if data.get(*pos) == Some(&b'\n') {
                            *pos += 1;
                        }
                    }
                    b'0'..=b'7' => {
                        let mut val = u32::from(esc - b'0');
                        for _ in 0..2 {
                            if *pos < len && matches!(data[*pos], b'0'..=b'7') {
                                val = val * 8 + u32::from(data[*pos] - b'0');
                                *pos += 1;
                            } else {
                                break;
                            }
                        }
                        out.push((val & 0xFF) as u8);
                    }
                    other => out.push(other),
                }
            }
            b'\r' => {
                if data.get(*pos) == Some(&b'\n') {
                    *pos += 1;
                }
                out.push(b'\n');
            }
            other => out.push(other),
        }
    }
    out
}

fn read_pdf_hex_string(data: &[u8], pos: &mut usize) -> Vec<u8> {
    let mut out = Vec::new();
    let len = data.len();
    *pos += 1; // skip '<'
    let mut hi: Option<u8> = None;
    while *pos < len {
        let b = data[*pos];
        *pos += 1;
        if b == b'>' {
            break;
        }
        if let Some(nib) = hex_nibble(b) {
            if let Some(h) = hi.take() {
                out.push((h << 4) | nib);
            } else {
                hi = Some(nib);
            }
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn decode_ascii_hex(input: &[u8], max_out: usize) -> IoResult<Vec<u8>> {
    let mut out = Vec::with_capacity((input.len() / 2).min(max_out).min(1024 * 1024));
    let mut hi: Option<u8> = None;
    for &b in input {
        if b == b'>' {
            break;
        }
        if let Some(nib) = hex_nibble(b) {
            if let Some(h) = hi.take() {
                if out.len() >= max_out {
                    return Err(IoError::ZipBomb {
                        reason: "PDF ASCIIHexDecode exceeds stream limit".into(),
                    });
                }
                out.push((h << 4) | nib);
            } else {
                hi = Some(nib);
            }
        }
    }
    if let Some(h) = hi {
        if out.len() >= max_out {
            return Err(IoError::ZipBomb {
                reason: "PDF ASCIIHexDecode exceeds stream limit".into(),
            });
        }
        out.push(h << 4);
    }
    Ok(out)
}

fn decode_ascii85(input: &[u8], max_out: usize) -> IoResult<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len().min(max_out).min(1024 * 1024));
    let mut group = [0u32; 5];
    let mut count = 0usize;
    let mut i = 0usize;
    while i < input.len() {
        let b = input[i];
        i += 1;
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'~' {
            break;
        }
        if b == b'z' && count == 0 {
            if out.len().saturating_add(4) > max_out {
                return Err(IoError::ZipBomb {
                    reason: "PDF ASCII85Decode exceeds stream limit".into(),
                });
            }
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            continue;
        }
        group[count] = u32::from(b - b'!');
        count += 1;
        if count == 5 {
            let mut val: u64 = 0;
            for &g in &group {
                val = val * 85 + u64::from(g);
            }
            if val > u64::from(u32::MAX) {
                return Err(pdf_err("ASCII85 5-tuple overflow"));
            }
            if out.len().saturating_add(4) > max_out {
                return Err(IoError::ZipBomb {
                    reason: "PDF ASCII85Decode exceeds stream limit".into(),
                });
            }
            out.extend_from_slice(&(val as u32).to_be_bytes());
            count = 0;
        }
    }
    if count >= 2 {
        for slot in &mut group[count..5] {
            *slot = 84;
        }
        let mut val: u64 = 0;
        for &g in &group {
            val = val * 85 + u64::from(g);
        }
        let bytes = (val as u32).to_be_bytes();
        let rem = count - 1;
        if out.len().saturating_add(rem) > max_out {
            return Err(IoError::ZipBomb {
                reason: "PDF ASCII85Decode exceeds stream limit".into(),
            });
        }
        out.extend_from_slice(&bytes[..rem]);
    }
    Ok(out)
}

fn decode_run_length(input: &[u8], max_out: usize) -> IoResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < input.len() {
        let len = input[i];
        i += 1;
        if len == 128 {
            break;
        } else if len < 128 {
            let count = usize::from(len) + 1;
            if out.len().saturating_add(count) > max_out {
                return Err(IoError::ZipBomb {
                    reason: "PDF RunLengthDecode exceeds stream limit".into(),
                });
            }
            let avail = (input.len() - i).min(count);
            out.extend_from_slice(&input[i..i + avail]);
            i += avail;
        } else {
            let count = 257usize - usize::from(len);
            if i >= input.len() {
                break;
            }
            if out.len().saturating_add(count) > max_out {
                return Err(IoError::ZipBomb {
                    reason: "PDF RunLengthDecode exceeds stream limit".into(),
                });
            }
            let val = input[i];
            i += 1;
            out.resize(out.len() + count, val);
        }
    }
    Ok(out)
}

/// Applies PDF `/DecodeParms` `/Predictor` (TIFF Predictor 2 & PNG Predictors 10–15).
fn apply_pdf_predictor(data: &[u8], dp: &BTreeMap<String, PdfObj>) -> IoResult<Vec<u8>> {
    let predictor = dp.get("Predictor").and_then(PdfObj::as_i64).unwrap_or(1);
    if predictor <= 1 {
        return Ok(data.to_vec());
    }
    let columns = dp
        .get("Columns")
        .and_then(PdfObj::as_i64)
        .unwrap_or(1)
        .clamp(1, 100_000) as usize;
    let colors = dp
        .get("Colors")
        .and_then(PdfObj::as_i64)
        .unwrap_or(1)
        .clamp(1, 16) as usize;
    let bpc = dp
        .get("BitsPerComponent")
        .and_then(PdfObj::as_i64)
        .unwrap_or(8)
        .clamp(1, 16) as usize;

    let row_bytes = (columns * colors * bpc).div_ceil(8).max(1);
    let bpp = (colors * bpc).div_ceil(8).max(1);

    if predictor == 2 {
        // TIFF Predictor 2: horizontal differencing per sample component
        let mut out = data.to_vec();
        for row in out.chunks_mut(row_bytes) {
            if bpc == 8 {
                for i in colors..row.len() {
                    row[i] = row[i].wrapping_add(row[i - colors]);
                }
            } else if bpc == 16 {
                let comp_bytes = colors * 2;
                let mut i = comp_bytes;
                while i + 1 < row.len() {
                    let prev = u16::from_be_bytes([row[i - comp_bytes], row[i - comp_bytes + 1]]);
                    let cur = u16::from_be_bytes([row[i], row[i + 1]]);
                    let sum = cur.wrapping_add(prev).to_be_bytes();
                    row[i] = sum[0];
                    row[i + 1] = sum[1];
                    i += 2;
                }
            }
        }
        return Ok(out);
    }

    if (10..=15).contains(&predictor) {
        // PNG row predictors: 1 filter byte + row_bytes per scanline
        let stride = row_bytes + 1;
        let num_rows = data.len() / stride;
        let mut out = vec![0u8; num_rows * row_bytes];
        let mut prev_row = vec![0u8; row_bytes];

        for r in 0..num_rows {
            let src = &data[r * stride..(r + 1) * stride];
            let filter = src[0];
            let payload = &src[1..];
            let dst = &mut out[r * row_bytes..(r + 1) * row_bytes];

            match filter {
                0 => dst.copy_from_slice(payload),
                1 => {
                    // Sub
                    for i in 0..row_bytes {
                        let left = if i >= bpp { dst[i - bpp] } else { 0 };
                        dst[i] = payload[i].wrapping_add(left);
                    }
                }
                2 => {
                    // Up
                    for i in 0..row_bytes {
                        dst[i] = payload[i].wrapping_add(prev_row[i]);
                    }
                }
                3 => {
                    // Average
                    for i in 0..row_bytes {
                        let left = if i >= bpp { u16::from(dst[i - bpp]) } else { 0 };
                        let up = u16::from(prev_row[i]);
                        dst[i] = payload[i].wrapping_add(((left + up) / 2) as u8);
                    }
                }
                4 => {
                    // Paeth
                    for i in 0..row_bytes {
                        let a = if i >= bpp { dst[i - bpp] } else { 0 };
                        let b = prev_row[i];
                        let c = if i >= bpp { prev_row[i - bpp] } else { 0 };
                        dst[i] = payload[i].wrapping_add(paeth_predictor(a, b, c));
                    }
                }
                _ => dst.copy_from_slice(payload),
            }
            prev_row.copy_from_slice(dst);
        }
        return Ok(out);
    }

    Ok(data.to_vec())
}

fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let ai = i32::from(a);
    let bi = i32::from(b);
    let ci = i32::from(c);
    let p = ai + bi - ci;
    let pa = (p - ai).abs();
    let pb = (p - bi).abs();
    let pc = (p - ci).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// =============================================================================
// PDF Content Stream Interpreter
// =============================================================================

struct ImportContext<'d> {
    doc: &'d mut Document,
    saw_cmyk: bool,
    saw_rgb: bool,
    rgba_budget_remaining: usize,
    per_image_budget: usize,
}

#[derive(Debug, Clone)]
enum PdfColorSpaceKind {
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
    Separation(String),
}

#[derive(Debug, Clone)]
struct PdfFontInfo {
    family: String,
    bold: bool,
    italic: bool,
    to_unicode: HashMap<u16, String>,
    is_two_byte: bool,
}

impl Default for PdfFontInfo {
    fn default() -> Self {
        Self {
            family: "Helvetica".to_owned(),
            bold: false,
            italic: false,
            to_unicode: HashMap::new(),
            is_two_byte: false,
        }
    }
}

#[derive(Debug, Clone)]
struct PdfGState {
    ctm: Affine2D,
    fill_color: Color,
    stroke_color: Color,
    fill_cs: PdfColorSpaceKind,
    stroke_cs: PdfColorSpaceKind,
    fill_alpha: f32,
    stroke_alpha: f32,
    blend_mode: BlendMode,
    line_width_pt: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash_pattern_pt: Vec<f64>,
    overprint_stroke: bool,
    font_key: String,
    font_size_pt: f64,
    text_leading: f64,
    char_space: f64,
    word_space: f64,
    horiz_scale: f64,
    text_rise: f64,
}

impl Default for PdfGState {
    fn default() -> Self {
        Self {
            ctm: Affine2D::IDENTITY,
            fill_color: Color::BLACK,
            stroke_color: Color::BLACK,
            fill_cs: PdfColorSpaceKind::DeviceGray,
            stroke_cs: PdfColorSpaceKind::DeviceGray,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend_mode: BlendMode::Normal,
            line_width_pt: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern_pt: Vec::new(),
            overprint_stroke: false,
            font_key: String::new(),
            font_size_pt: 12.0,
            text_leading: 0.0,
            char_space: 0.0,
            word_space: 0.0,
            horiz_scale: 100.0,
            text_rise: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
enum ContentToken {
    Obj(PdfObj),
    Op(String),
}

struct PdfContentInterpreter<'p, 'a, 'd> {
    parser: &'p mut PdfParser<'a>,
    ctx: &'p mut ImportContext<'d>,
    layer_id: LayerId,
    page_to_mm: Affine2D,
    resources: BTreeMap<String, PdfObj>,
    gstate: PdfGState,
    gstack: Vec<PdfGState>,
    path: BezPath,
    cur_pt: Option<Point2D>,
    subpath_start: Option<Point2D>,
    in_text: bool,
    text_matrix: Affine2D,
    text_line_matrix: Affine2D,
    font_cache: HashMap<String, PdfFontInfo>,
    op_count: usize,
    emitted_count: usize,
}

impl<'p, 'a, 'd> PdfContentInterpreter<'p, 'a, 'd> {
    fn new(
        parser: &'p mut PdfParser<'a>,
        ctx: &'p mut ImportContext<'d>,
        layer_id: LayerId,
        page_to_mm: Affine2D,
        resources: BTreeMap<String, PdfObj>,
    ) -> Self {
        Self {
            parser,
            ctx,
            layer_id,
            page_to_mm,
            resources,
            gstate: PdfGState::default(),
            gstack: Vec::new(),
            path: BezPath::new(),
            cur_pt: None,
            subpath_start: None,
            in_text: false,
            text_matrix: Affine2D::IDENTITY,
            text_line_matrix: Affine2D::IDENTITY,
            font_cache: HashMap::new(),
            op_count: 0,
            emitted_count: 0,
        }
    }

    fn run(&mut self, stream: &[u8], form_depth: usize) -> IoResult<()> {
        if form_depth > MAX_FORM_DEPTH {
            return Ok(());
        }
        let tokens = tokenize_content_stream(stream);
        let mut operands: Vec<PdfObj> = Vec::with_capacity(16);

        for tok in tokens {
            match tok {
                ContentToken::Obj(o) => {
                    if operands.len() < 256 {
                        operands.push(o);
                    }
                }
                ContentToken::Op(op) => {
                    self.op_count += 1;
                    if self.op_count > MAX_OPS_PER_PAGE {
                        break;
                    }
                    self.exec_op(&op, &operands, form_depth)?;
                    operands.clear();
                }
            }
        }
        Ok(())
    }

    fn world_xf(&self) -> Affine2D {
        self.page_to_mm.compose(&self.gstate.ctm)
    }

    fn exec_op(&mut self, op: &str, args: &[PdfObj], form_depth: usize) -> IoResult<()> {
        match op {
            // Graphics state stack & CTM
            "q" => {
                if self.gstack.len() < MAX_GSTATE_DEPTH {
                    self.gstack.push(self.gstate.clone());
                }
            }
            "Q" => {
                if let Some(prev) = self.gstack.pop() {
                    self.gstate = prev;
                }
            }
            "cm" if args.len() >= 6 => {
                if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                    args[4].as_f64(),
                    args[5].as_f64(),
                ) {
                    let m = Affine2D::new(a, b, c, d, e, f);
                    if m.is_finite() {
                        self.gstate.ctm = self.gstate.ctm.compose(&m);
                    }
                }
            }
            "gs" if !args.is_empty() => {
                if let Some(gs_name) = args[0].as_name() {
                    self.apply_ext_gstate(gs_name);
                }
            }

            // Stroke styling
            "w" if !args.is_empty() => {
                if let Some(w) = args[0].as_f64() {
                    self.gstate.line_width_pt = w.max(0.0);
                }
            }
            "J" if !args.is_empty() => {
                if let Some(c) = args[0].as_i64() {
                    self.gstate.line_cap = match c {
                        1 => LineCap::Round,
                        2 => LineCap::Square,
                        _ => LineCap::Butt,
                    };
                }
            }
            "j" if !args.is_empty() => {
                if let Some(j) = args[0].as_i64() {
                    self.gstate.line_join = match j {
                        1 => LineJoin::Round,
                        2 => LineJoin::Bevel,
                        _ => LineJoin::Miter,
                    };
                }
            }
            "M" if !args.is_empty() => {
                if let Some(m) = args[0].as_f64() {
                    self.gstate.miter_limit = m.max(1.0);
                }
            }
            "d" if args.len() >= 2 => {
                if let PdfObj::Array(ref arr) = args[0] {
                    self.gstate.dash_pattern_pt = arr
                        .iter()
                        .filter_map(PdfObj::as_f64)
                        .filter(|&v| v > 0.0)
                        .collect();
                }
            }

            // Colors
            "g" if !args.is_empty() => {
                if let Some(g) = args[0].as_f64() {
                    self.gstate.fill_cs = PdfColorSpaceKind::DeviceGray;
                    self.gstate.fill_color = Color::gray(g as f32);
                }
            }
            "G" if !args.is_empty() => {
                if let Some(g) = args[0].as_f64() {
                    self.gstate.stroke_cs = PdfColorSpaceKind::DeviceGray;
                    self.gstate.stroke_color = Color::gray(g as f32);
                }
            }
            "rg" if args.len() >= 3 => {
                if let (Some(r), Some(g), Some(b)) =
                    (args[0].as_f64(), args[1].as_f64(), args[2].as_f64())
                {
                    self.gstate.fill_cs = PdfColorSpaceKind::DeviceRgb;
                    self.gstate.fill_color = Color::rgb(r as f32, g as f32, b as f32);
                    self.ctx.saw_rgb = true;
                }
            }
            "RG" if args.len() >= 3 => {
                if let (Some(r), Some(g), Some(b)) =
                    (args[0].as_f64(), args[1].as_f64(), args[2].as_f64())
                {
                    self.gstate.stroke_cs = PdfColorSpaceKind::DeviceRgb;
                    self.gstate.stroke_color = Color::rgb(r as f32, g as f32, b as f32);
                    self.ctx.saw_rgb = true;
                }
            }
            "k" if args.len() >= 4 => {
                if let (Some(c), Some(m), Some(y), Some(k)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                ) {
                    self.gstate.fill_cs = PdfColorSpaceKind::DeviceCmyk;
                    self.gstate.fill_color = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.ctx.saw_cmyk = true;
                }
            }
            "K" if args.len() >= 4 => {
                if let (Some(c), Some(m), Some(y), Some(k)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                ) {
                    self.gstate.stroke_cs = PdfColorSpaceKind::DeviceCmyk;
                    self.gstate.stroke_color = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.ctx.saw_cmyk = true;
                }
            }
            "cs" if !args.is_empty() => {
                if let Some(name) = args[0].as_name() {
                    self.gstate.fill_cs = self.resolve_named_colorspace(name);
                }
            }
            "CS" if !args.is_empty() => {
                if let Some(name) = args[0].as_name() {
                    self.gstate.stroke_cs = self.resolve_named_colorspace(name);
                }
            }
            "sc" | "scn" => {
                let nums: Vec<f64> = args.iter().filter_map(PdfObj::as_f64).collect();
                if let Some(c) = self.color_from_cs(&self.gstate.fill_cs.clone(), &nums) {
                    self.gstate.fill_color = c;
                }
            }
            "SC" | "SCN" => {
                let nums: Vec<f64> = args.iter().filter_map(PdfObj::as_f64).collect();
                if let Some(c) = self.color_from_cs(&self.gstate.stroke_cs.clone(), &nums) {
                    self.gstate.stroke_color = c;
                }
            }

            // Path construction
            "m" if args.len() >= 2 => {
                if let (Some(x), Some(y)) = (args[0].as_f64(), args[1].as_f64()) {
                    let p = self.world_xf().transform_point(Point2D::new(x, y));
                    self.path.move_to(p.to_kurbo());
                    self.cur_pt = Some(p);
                    self.subpath_start = Some(p);
                }
            }
            "l" if args.len() >= 2 => {
                if let (Some(x), Some(y)) = (args[0].as_f64(), args[1].as_f64()) {
                    let p = self.world_xf().transform_point(Point2D::new(x, y));
                    if self.cur_pt.is_none() {
                        self.path.move_to(p.to_kurbo());
                        self.subpath_start = Some(p);
                    } else {
                        self.path.line_to(p.to_kurbo());
                    }
                    self.cur_pt = Some(p);
                }
            }
            "c" if args.len() >= 6 => {
                if let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x3), Some(y3)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                    args[4].as_f64(),
                    args[5].as_f64(),
                ) {
                    let xf = self.world_xf();
                    let p1 = xf.transform_point(Point2D::new(x1, y1));
                    let p2 = xf.transform_point(Point2D::new(x2, y2));
                    let p3 = xf.transform_point(Point2D::new(x3, y3));
                    if self.cur_pt.is_none() {
                        self.path.move_to(p1.to_kurbo());
                        self.subpath_start = Some(p1);
                    }
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "v" if args.len() >= 4 => {
                if let (Some(x2), Some(y2), Some(x3), Some(y3)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                ) {
                    let xf = self.world_xf();
                    let p1 = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let p2 = xf.transform_point(Point2D::new(x2, y2));
                    let p3 = xf.transform_point(Point2D::new(x3, y3));
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "y" if args.len() >= 4 => {
                if let (Some(x1), Some(y1), Some(x3), Some(y3)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                ) {
                    let xf = self.world_xf();
                    let p1 = xf.transform_point(Point2D::new(x1, y1));
                    let p3 = xf.transform_point(Point2D::new(x3, y3));
                    self.path
                        .curve_to(p1.to_kurbo(), p3.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "re" if args.len() >= 4 => {
                if let (Some(x), Some(y), Some(w), Some(h)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                ) {
                    let xf = self.world_xf();
                    let p0 = xf.transform_point(Point2D::new(x, y));
                    let p1 = xf.transform_point(Point2D::new(x + w, y));
                    let p2 = xf.transform_point(Point2D::new(x + w, y + h));
                    let p3 = xf.transform_point(Point2D::new(x, y + h));
                    self.path.move_to(p0.to_kurbo());
                    self.path.line_to(p1.to_kurbo());
                    self.path.line_to(p2.to_kurbo());
                    self.path.line_to(p3.to_kurbo());
                    self.path.close_path();
                    self.cur_pt = Some(p0);
                    self.subpath_start = Some(p0);
                }
            }
            "h" => {
                if self.cur_pt.is_some() {
                    self.path.close_path();
                    self.cur_pt = self.subpath_start;
                }
            }

            // Path painting & clipping
            "f" | "F" | "f*" => self.emit_path(true, false, false)?,
            "S" => self.emit_path(false, true, false)?,
            "s" => self.emit_path(false, true, true)?,
            "B" | "B*" => self.emit_path(true, true, false)?,
            "b" | "b*" => self.emit_path(true, true, true)?,
            "n" => {
                self.path = BezPath::new();
                self.cur_pt = None;
                self.subpath_start = None;
            }
            "W" | "W*" => {}

            // XObjects (Form & Image)
            "Do" if !args.is_empty() => {
                if let Some(xname) = args[0].as_name() {
                    self.exec_xobject(xname, form_depth)?;
                }
            }

            // Text state & painting
            "BT" => {
                self.in_text = true;
                self.text_matrix = Affine2D::IDENTITY;
                self.text_line_matrix = Affine2D::IDENTITY;
            }
            "ET" => {
                self.in_text = false;
            }
            "Tf" if args.len() >= 2 => {
                if let (Some(fname), Some(size)) = (args[0].as_name(), args[1].as_f64()) {
                    self.gstate.font_key = fname.to_owned();
                    self.gstate.font_size_pt = size.abs().max(0.5);
                    self.ensure_font_loaded(fname);
                }
            }
            "TL" if !args.is_empty() => {
                if let Some(tl) = args[0].as_f64() {
                    self.gstate.text_leading = tl;
                }
            }
            "Tc" if !args.is_empty() => {
                if let Some(tc) = args[0].as_f64() {
                    self.gstate.char_space = tc;
                }
            }
            "Tw" if !args.is_empty() => {
                if let Some(tw) = args[0].as_f64() {
                    self.gstate.word_space = tw;
                }
            }
            "Tz" if !args.is_empty() => {
                if let Some(tz) = args[0].as_f64() {
                    self.gstate.horiz_scale = tz.clamp(1.0, 1000.0);
                }
            }
            "Ts" if !args.is_empty() => {
                if let Some(ts) = args[0].as_f64() {
                    self.gstate.text_rise = ts;
                }
            }
            "Tm" if args.len() >= 6 => {
                if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                    args[0].as_f64(),
                    args[1].as_f64(),
                    args[2].as_f64(),
                    args[3].as_f64(),
                    args[4].as_f64(),
                    args[5].as_f64(),
                ) {
                    let m = Affine2D::new(a, b, c, d, e, f);
                    if m.is_finite() {
                        self.text_line_matrix = m;
                        self.text_matrix = m;
                    }
                }
            }
            "Td" if args.len() >= 2 => {
                if let (Some(tx), Some(ty)) = (args[0].as_f64(), args[1].as_f64()) {
                    self.text_line_matrix =
                        self.text_line_matrix.compose(&Affine2D::translate(tx, ty));
                    self.text_matrix = self.text_line_matrix;
                }
            }
            "TD" if args.len() >= 2 => {
                if let (Some(tx), Some(ty)) = (args[0].as_f64(), args[1].as_f64()) {
                    self.gstate.text_leading = -ty;
                    self.text_line_matrix =
                        self.text_line_matrix.compose(&Affine2D::translate(tx, ty));
                    self.text_matrix = self.text_line_matrix;
                }
            }
            "T*" => {
                let dy = -self.gstate.text_leading;
                self.text_line_matrix =
                    self.text_line_matrix.compose(&Affine2D::translate(0.0, dy));
                self.text_matrix = self.text_line_matrix;
            }
            "Tj" if !args.is_empty() => {
                if let PdfObj::Str(ref raw) = args[0] {
                    let s = self.decode_font_string(raw);
                    self.emit_text(&s)?;
                }
            }
            "TJ" if !args.is_empty() => {
                if let PdfObj::Array(ref items) = args[0] {
                    let mut combined = String::new();
                    for item in items {
                        match item {
                            PdfObj::Str(raw) => combined.push_str(&self.decode_font_string(raw)),
                            PdfObj::Int(n) if *n < -180 => combined.push(' '),
                            PdfObj::Real(v) if *v < -180.0 => combined.push(' '),
                            _ => {}
                        }
                    }
                    self.emit_text(&combined)?;
                }
            }
            "'" if !args.is_empty() => {
                let dy = -self.gstate.text_leading;
                self.text_line_matrix =
                    self.text_line_matrix.compose(&Affine2D::translate(0.0, dy));
                self.text_matrix = self.text_line_matrix;
                if let PdfObj::Str(ref raw) = args[0] {
                    let s = self.decode_font_string(raw);
                    self.emit_text(&s)?;
                }
            }
            "\"" if args.len() >= 3 => {
                if let (Some(aw), Some(ac)) = (args[0].as_f64(), args[1].as_f64()) {
                    self.gstate.word_space = aw;
                    self.gstate.char_space = ac;
                }
                let dy = -self.gstate.text_leading;
                self.text_line_matrix =
                    self.text_line_matrix.compose(&Affine2D::translate(0.0, dy));
                self.text_matrix = self.text_line_matrix;
                if let PdfObj::Str(ref raw) = args[2] {
                    let s = self.decode_font_string(raw);
                    self.emit_text(&s)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_ext_gstate(&mut self, name: &str) {
        let Some(gs_table_obj) = self.resources.get("ExtGState").cloned() else {
            return;
        };
        let Ok(gs_table) = self.parser.resolve_dict(&gs_table_obj, 0) else {
            return;
        };
        let Some(entry_obj) = gs_table.get(name).cloned() else {
            return;
        };
        let Ok(gs) = self.parser.resolve_dict(&entry_obj, 0) else {
            return;
        };

        if let Some(ca) = gs.get("ca").and_then(PdfObj::as_f64) {
            self.gstate.fill_alpha = (ca as f32).clamp(0.0, 1.0);
        }
        if let Some(ca) = gs.get("CA").and_then(PdfObj::as_f64) {
            self.gstate.stroke_alpha = (ca as f32).clamp(0.0, 1.0);
        }
        if let Some(lw) = gs.get("LW").and_then(PdfObj::as_f64) {
            self.gstate.line_width_pt = lw.max(0.0);
        }
        if let Some(lc) = gs.get("LC").and_then(PdfObj::as_i64) {
            self.gstate.line_cap = match lc {
                1 => LineCap::Round,
                2 => LineCap::Square,
                _ => LineCap::Butt,
            };
        }
        if let Some(lj) = gs.get("LJ").and_then(PdfObj::as_i64) {
            self.gstate.line_join = match lj {
                1 => LineJoin::Round,
                2 => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
        }
        if let Some(ml) = gs.get("ML").and_then(PdfObj::as_f64) {
            self.gstate.miter_limit = ml.max(1.0);
        }
        if let Some(PdfObj::Bool(op)) = gs.get("OP") {
            self.gstate.overprint_stroke = *op;
        }
        if let Some(bm_obj) = gs.get("BM") {
            let bm_name = match bm_obj {
                PdfObj::Name(n) => Some(n.as_str()),
                PdfObj::Array(arr) => arr.first().and_then(PdfObj::as_name),
                _ => None,
            };
            if let Some(bm) = bm_name {
                self.gstate.blend_mode = map_pdf_blend_mode(bm);
            }
        }
    }

    fn resolve_named_colorspace(&mut self, name: &str) -> PdfColorSpaceKind {
        match name {
            "DeviceGray" | "G" => return PdfColorSpaceKind::DeviceGray,
            "DeviceRGB" | "RGB" => return PdfColorSpaceKind::DeviceRgb,
            "DeviceCMYK" | "CMYK" => return PdfColorSpaceKind::DeviceCmyk,
            _ => {}
        }
        if let Some(cs_table_obj) = self.resources.get("ColorSpace").cloned() {
            if let Ok(cs_table) = self.parser.resolve_dict(&cs_table_obj, 0) {
                if let Some(cs_def_obj) = cs_table.get(name).cloned() {
                    if let Ok(resolved) = self.parser.resolve(&cs_def_obj, 0) {
                        return self.classify_colorspace(&resolved);
                    }
                }
            }
        }
        PdfColorSpaceKind::DeviceRgb
    }

    fn classify_colorspace(&mut self, obj: &PdfObj) -> PdfColorSpaceKind {
        match obj {
            PdfObj::Name(n) => match n.as_str() {
                "DeviceGray" | "G" | "CalGray" => PdfColorSpaceKind::DeviceGray,
                "DeviceCMYK" | "CMYK" => PdfColorSpaceKind::DeviceCmyk,
                _ => PdfColorSpaceKind::DeviceRgb,
            },
            PdfObj::Array(arr) => {
                let tag = arr.first().and_then(PdfObj::as_name).unwrap_or("");
                match tag {
                    "DeviceGray" | "CalGray" => PdfColorSpaceKind::DeviceGray,
                    "DeviceRGB" | "CalRGB" | "Lab" => PdfColorSpaceKind::DeviceRgb,
                    "DeviceCMYK" => PdfColorSpaceKind::DeviceCmyk,
                    "ICCBased" => {
                        if let Some(stm) = arr.get(1) {
                            if let Ok(d) = self.parser.resolve_dict(stm, 0) {
                                return match d.get("N").and_then(PdfObj::as_i64) {
                                    Some(1) => PdfColorSpaceKind::DeviceGray,
                                    Some(4) => PdfColorSpaceKind::DeviceCmyk,
                                    _ => PdfColorSpaceKind::DeviceRgb,
                                };
                            }
                        }
                        PdfColorSpaceKind::DeviceRgb
                    }
                    "Separation" => {
                        let spot_name = arr
                            .get(1)
                            .and_then(PdfObj::as_name)
                            .unwrap_or("Spot")
                            .to_owned();
                        PdfColorSpaceKind::Separation(spot_name)
                    }
                    _ => PdfColorSpaceKind::DeviceRgb,
                }
            }
            _ => PdfColorSpaceKind::DeviceRgb,
        }
    }

    fn color_from_cs(&mut self, cs: &PdfColorSpaceKind, nums: &[f64]) -> Option<Color> {
        match cs {
            PdfColorSpaceKind::DeviceGray if !nums.is_empty() => Some(Color::gray(nums[0] as f32)),
            PdfColorSpaceKind::DeviceRgb if nums.len() >= 3 => {
                self.ctx.saw_rgb = true;
                Some(Color::rgb(nums[0] as f32, nums[1] as f32, nums[2] as f32))
            }
            PdfColorSpaceKind::DeviceCmyk if nums.len() >= 4 => {
                self.ctx.saw_cmyk = true;
                Some(Color::cmyk(
                    nums[0] as f32,
                    nums[1] as f32,
                    nums[2] as f32,
                    nums[3] as f32,
                ))
            }
            PdfColorSpaceKind::Separation(name) if !nums.is_empty() => {
                let tint = (nums[0] as f32).clamp(0.0, 1.0);
                if name.eq_ignore_ascii_case("CutContour") {
                    Some(Color::spot(name.clone(), tint, [0.0, 1.0, 0.0, 0.0]))
                } else {
                    Some(Color::spot(name.clone(), tint, [0.0, 0.0, 0.0, 1.0]))
                }
            }
            _ => match nums.len() {
                1 => Some(Color::gray(nums[0] as f32)),
                3 => {
                    self.ctx.saw_rgb = true;
                    Some(Color::rgb(nums[0] as f32, nums[1] as f32, nums[2] as f32))
                }
                4 => {
                    self.ctx.saw_cmyk = true;
                    Some(Color::cmyk(
                        nums[0] as f32,
                        nums[1] as f32,
                        nums[2] as f32,
                        nums[3] as f32,
                    ))
                }
                _ => None,
            },
        }
    }

    fn emit_path(&mut self, do_fill: bool, do_stroke: bool, close: bool) -> IoResult<()> {
        if close && self.cur_pt.is_some() {
            self.path.close_path();
        }
        let kb = std::mem::take(&mut self.path);
        self.cur_pt = None;
        self.subpath_start = None;
        if kb.is_empty() {
            return Ok(());
        }
        let bez = BezierPath::from_kurbo(&kb);
        if bez.is_empty() {
            return Ok(());
        }

        let fill = if do_fill {
            Fill::Solid(self.gstate.fill_color.with_alpha(self.gstate.fill_alpha))
        } else {
            Fill::None
        };
        let outline = if do_stroke {
            let scale = effective_affine_scale(&self.gstate.ctm);
            let width_mm = (self.gstate.line_width_pt * scale * PT_TO_MM).max(0.05);
            let mut o = OutlineStyle::solid(
                self.gstate
                    .stroke_color
                    .with_alpha(self.gstate.stroke_alpha),
                width_mm,
            );
            o.cap = self.gstate.line_cap;
            o.join = self.gstate.line_join;
            o.miter_limit = self.gstate.miter_limit;
            o.overprint = self.gstate.overprint_stroke;
            o.dash_pattern = self
                .gstate
                .dash_pattern_pt
                .iter()
                .map(|d| d * scale * PT_TO_MM)
                .filter(|d| d.is_finite() && *d > 0.0)
                .collect();
            o
        } else {
            OutlineStyle::none()
        };

        self.emitted_count += 1;
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("PDF Path {}", self.emitted_count),
            NodePayload::Path(bez),
        )
        .with_fill(fill)
        .with_outline(outline);
        obj.blend_mode = self.gstate.blend_mode;
        self.ctx.doc.add_object(obj, Some(self.layer_id))?;
        Ok(())
    }

    fn exec_xobject(&mut self, name: &str, form_depth: usize) -> IoResult<()> {
        let Some(xo_table_obj) = self.resources.get("XObject").cloned() else {
            return Ok(());
        };
        let Ok(xo_table) = self.parser.resolve_dict(&xo_table_obj, 0) else {
            return Ok(());
        };
        let Some(xobj_ref) = xo_table.get(name).cloned() else {
            return Ok(());
        };
        let resolved = self.parser.resolve(&xobj_ref, 0)?;
        let PdfObj::Stream { dict, start, len } = resolved else {
            return Ok(());
        };
        let subtype = dict.get("Subtype").and_then(PdfObj::as_name).unwrap_or("");
        match subtype {
            "Form" => self.exec_form_xobject(&dict, start, len, form_depth),
            "Image" => self.exec_image_xobject(name, &dict, start, len),
            _ => Ok(()),
        }
    }

    fn exec_form_xobject(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        start: usize,
        len: usize,
        form_depth: usize,
    ) -> IoResult<()> {
        if form_depth >= MAX_FORM_DEPTH {
            return Ok(());
        }
        let raw = &self.parser.bytes[start..start.saturating_add(len).min(self.parser.bytes.len())];
        let stream_bytes = self.parser.decode_stream_bytes(dict, raw)?;
        if stream_bytes.is_empty() {
            return Ok(());
        }

        let form_matrix = match dict.get("Matrix") {
            Some(PdfObj::Array(arr)) if arr.len() >= 6 => {
                let nums: Vec<f64> = arr.iter().take(6).filter_map(PdfObj::as_f64).collect();
                if nums.len() == 6 {
                    Affine2D::new(nums[0], nums[1], nums[2], nums[3], nums[4], nums[5])
                } else {
                    Affine2D::IDENTITY
                }
            }
            _ => Affine2D::IDENTITY,
        };

        let form_resources = match dict.get("Resources").cloned() {
            Some(res_obj) => self
                .parser
                .resolve_dict(&res_obj, 0)
                .unwrap_or_else(|_| self.resources.clone()),
            None => self.resources.clone(),
        };

        // Save graphics state, resources, and path state cleanly
        let saved_gstate = self.gstate.clone();
        let saved_res = std::mem::replace(&mut self.resources, form_resources);
        let saved_fonts = std::mem::take(&mut self.font_cache);
        if form_matrix.is_finite() {
            self.gstate.ctm = self.gstate.ctm.compose(&form_matrix);
        }

        let res = self.run(&stream_bytes, form_depth + 1);

        self.gstate = saved_gstate;
        self.resources = saved_res;
        self.font_cache = saved_fonts;
        res
    }

    fn exec_image_xobject(
        &mut self,
        name: &str,
        dict: &BTreeMap<String, PdfObj>,
        start: usize,
        len: usize,
    ) -> IoResult<()> {
        let width = dict.get("Width").and_then(PdfObj::as_i64).unwrap_or(0);
        let height = dict.get("Height").and_then(PdfObj::as_i64).unwrap_or(0);
        if width <= 0 || height <= 0 {
            return Ok(());
        }
        let width = width as u32;
        let height = height as u32;
        if width > MAX_PDF_IMAGE_DIM || height > MAX_PDF_IMAGE_DIM {
            return Ok(());
        }
        if self.ctx.rgba_budget_remaining < 4 {
            return Ok(());
        }

        let filters = self.parser.extract_filter_chain(dict, 0)?;
        let raw_slice =
            &self.parser.bytes[start..start.saturating_add(len).min(self.parser.bytes.len())];

        let is_jpeg = filters
            .last()
            .is_some_and(|f| f == "DCTDecode" || f == "DCT");

        let mut decoded_rgba = if is_jpeg {
            // Decode any outer filters (like FlateDecode/ASCII85) before JPEG
            let jpeg_bytes = if filters.len() > 1 {
                self.parser.decode_stream_bytes(dict, raw_slice)?
            } else {
                raw_slice.to_vec()
            };
            let mut reader = ImageReader::with_format(Cursor::new(&jpeg_bytes), ImageFormat::Jpeg);
            let mut img_limits = Limits::default();
            img_limits.max_image_width = Some(MAX_PDF_IMAGE_DIM);
            img_limits.max_image_height = Some(MAX_PDF_IMAGE_DIM);
            img_limits.max_alloc = Some(MAX_PDF_RGBA_BUDGET as u64);
            reader.limits(img_limits);
            match reader.decode() {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    (rgba.width(), rgba.height(), rgba.into_raw())
                }
                Err(_) => return Ok(()),
            }
        } else {
            // Uncompressed, FlateDecode, ASCII85, ASCIIHex, RunLength
            let supported = filters.iter().all(|f| {
                matches!(
                    f.as_str(),
                    "FlateDecode"
                        | "Fl"
                        | "ASCII85Decode"
                        | "A85"
                        | "ASCIIHexDecode"
                        | "AHx"
                        | "RunLengthDecode"
                        | "RL"
                )
            });
            if !supported {
                return Ok(());
            }
            let raw_pixels = self.parser.decode_stream_bytes(dict, raw_slice)?;
            let Some(rgba) = self.decode_raw_pdf_image(dict, width, height, &raw_pixels)? else {
                return Ok(());
            };
            (width, height, rgba)
        };

        // Apply optional /SMask grayscale alpha channel
        if let Some(smask_obj) = dict.get("SMask").cloned() {
            if let Ok(PdfObj::Stream {
                dict: sm_dict,
                start: sm_start,
                len: sm_len,
            }) = self.parser.resolve(&smask_obj, 0)
            {
                let sm_w = sm_dict.get("Width").and_then(PdfObj::as_i64).unwrap_or(0) as u32;
                let sm_h = sm_dict.get("Height").and_then(PdfObj::as_i64).unwrap_or(0) as u32;
                if sm_w == decoded_rgba.0 && sm_h == decoded_rgba.1 {
                    let sm_raw = &self.parser.bytes
                        [sm_start..sm_start.saturating_add(sm_len).min(self.parser.bytes.len())];
                    if let Ok(sm_bytes) = self.parser.decode_stream_bytes(&sm_dict, sm_raw) {
                        let px_cnt = (sm_w as usize) * (sm_h as usize);
                        if sm_bytes.len() >= px_cnt {
                            for (i, &a) in sm_bytes.iter().take(px_cnt).enumerate() {
                                decoded_rgba.2[i * 4 + 3] = a;
                            }
                        }
                    }
                }
            }
        }

        // Downscale cleanly if needed to fit both per-image budget and cumulative RGBA budget
        let target_budget = self
            .ctx
            .rgba_budget_remaining
            .min(self.ctx.per_image_budget);
        let (final_w, final_h, final_rgba) = downscale_rgba_if_needed(
            decoded_rgba.0,
            decoded_rgba.1,
            decoded_rgba.2,
            2048,
            target_budget,
        );
        self.ctx.rgba_budget_remaining = self
            .ctx
            .rgba_budget_remaining
            .saturating_sub(final_rgba.len());

        // Compute exact physical millimeter placement from CTM (PDF image lies in [0,1]x[0,1], top-left = (0,1))
        let world_xf = self.world_xf();
        let w_mm = world_xf
            .transform_vec(Vec2D::new(1.0, 0.0))
            .length()
            .max(0.1);
        let h_mm = world_xf
            .transform_vec(Vec2D::new(0.0, 1.0))
            .length()
            .max(0.1);
        let local_to_unit = Affine2D::new(1.0 / w_mm, 0.0, 0.0, -1.0 / h_mm, 0.0, 1.0);
        let obj_xf = world_xf.compose(&local_to_unit);
        if !obj_xf.is_finite() {
            return Ok(());
        }

        let est_dpi = ((f64::from(width) / (w_mm / 25.4)) as f32).clamp(36.0, 2400.0);
        let asset = RasterAsset::new(final_w, final_h, final_rgba, est_dpi, ColorMode::Rgb)?;
        let asset_id = self.ctx.doc.register_asset(asset);

        self.emitted_count += 1;
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("PDF Image {name}"),
            NodePayload::Raster(RasterLayerData {
                asset_id,
                width_mm: w_mm,
                height_mm: h_mm,
                filters: Vec::new(),
                crop_rect: None,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none())
        .with_transform(obj_xf);
        obj.opacity = self.gstate.fill_alpha;
        obj.blend_mode = self.gstate.blend_mode;
        self.ctx.doc.add_object(obj, Some(self.layer_id))?;
        Ok(())
    }

    fn decode_raw_pdf_image(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> IoResult<Option<Vec<u8>>> {
        let bpc_raw = dict
            .get("BitsPerComponent")
            .and_then(PdfObj::as_i64)
            .unwrap_or(8);
        // ISO 32000-1 §8.9.5: valid BitsPerComponent values are strictly 1, 2, 4, 8, or 16.
        if !matches!(bpc_raw, 1 | 2 | 4 | 8 | 16) {
            return Ok(None);
        }
        let bpc = bpc_raw as u32;
        let is_image_mask = matches!(dict.get("ImageMask"), Some(PdfObj::Bool(true)));
        let px_count = (width as usize).saturating_mul(height as usize);
        if px_count == 0 || px_count > (MAX_PDF_RGBA_BUDGET / 4) {
            return Ok(None);
        }

        if is_image_mask && bpc == 1 {
            let [fr, fg, fb, fa] = self.gstate.fill_color.to_rgba_f32();
            let r8 = (fr * 255.0).round() as u8;
            let g8 = (fg * 255.0).round() as u8;
            let b8 = (fb * 255.0).round() as u8;
            let a8 = (fa * self.gstate.fill_alpha * 255.0).round() as u8;
            let row_bytes = (width as usize).div_ceil(8);
            let mut rgba = vec![0u8; px_count * 4];
            for y in 0..(height as usize) {
                for x in 0..(width as usize) {
                    let byte = pixels.get(y * row_bytes + x / 8).copied().unwrap_or(0xFF);
                    let bit = (byte >> (7 - (x % 8))) & 1;
                    // Default PDF ImageMask: 0 = painted with current fill color, 1 = transparent
                    if bit == 0 {
                        let o = (y * (width as usize) + x) * 4;
                        rgba[o] = r8;
                        rgba[o + 1] = g8;
                        rgba[o + 2] = b8;
                        rgba[o + 3] = a8;
                    }
                }
            }
            return Ok(Some(rgba));
        }

        // Determine color space (including Indexed palette)
        let cs_obj = match dict.get("ColorSpace").cloned() {
            Some(o) => self.parser.resolve(&o, 0)?,
            None => PdfObj::Name("DeviceRGB".to_owned()),
        };

        // Check for Indexed colorspace `[/Indexed base hival lookup]`
        if let PdfObj::Array(ref arr) = cs_obj {
            if arr.first().and_then(PdfObj::as_name) == Some("Indexed") && arr.len() >= 4 {
                let base_obj = self.parser.resolve(&arr[1], 0)?;
                let base_cs = self.classify_colorspace(&base_obj);
                let base_comps = match base_cs {
                    PdfColorSpaceKind::DeviceGray | PdfColorSpaceKind::Separation(_) => 1usize,
                    PdfColorSpaceKind::DeviceCmyk => 4usize,
                    PdfColorSpaceKind::DeviceRgb => 3usize,
                };
                let lookup_bytes = match self.parser.resolve(&arr[3], 0)? {
                    PdfObj::Str(b) => b,
                    PdfObj::Stream {
                        dict: ld,
                        start,
                        len,
                    } => {
                        let r = &self.parser.bytes
                            [start..start.saturating_add(len).min(self.parser.bytes.len())];
                        self.parser.decode_stream_bytes(&ld, r)?
                    }
                    _ => Vec::new(),
                };
                let indices = unpack_bpc_samples(pixels, width, height, 1, bpc);
                let mut rgba = vec![255u8; px_count * 4];
                for (i, &idx) in indices.iter().take(px_count).enumerate() {
                    let off = usize::from(idx) * base_comps;
                    let o = i * 4;
                    match base_comps {
                        1 => {
                            let g = lookup_bytes.get(off).copied().unwrap_or(0);
                            rgba[o] = g;
                            rgba[o + 1] = g;
                            rgba[o + 2] = g;
                        }
                        3 => {
                            rgba[o] = lookup_bytes.get(off).copied().unwrap_or(0);
                            rgba[o + 1] = lookup_bytes.get(off + 1).copied().unwrap_or(0);
                            rgba[o + 2] = lookup_bytes.get(off + 2).copied().unwrap_or(0);
                        }
                        4 => {
                            let c = f32::from(lookup_bytes.get(off).copied().unwrap_or(0)) / 255.0;
                            let m =
                                f32::from(lookup_bytes.get(off + 1).copied().unwrap_or(0)) / 255.0;
                            let y =
                                f32::from(lookup_bytes.get(off + 2).copied().unwrap_or(0)) / 255.0;
                            let k =
                                f32::from(lookup_bytes.get(off + 3).copied().unwrap_or(0)) / 255.0;
                            let inv_k = 1.0 - k;
                            rgba[o] = ((1.0 - c) * inv_k * 255.0).round() as u8;
                            rgba[o + 1] = ((1.0 - m) * inv_k * 255.0).round() as u8;
                            rgba[o + 2] = ((1.0 - y) * inv_k * 255.0).round() as u8;
                        }
                        _ => {}
                    }
                    rgba[o + 3] = 255;
                }
                return Ok(Some(rgba));
            }
        }

        let cs_kind = self.classify_colorspace(&cs_obj);
        let comps = match cs_kind {
            PdfColorSpaceKind::DeviceGray | PdfColorSpaceKind::Separation(_) => 1usize,
            PdfColorSpaceKind::DeviceRgb => 3usize,
            PdfColorSpaceKind::DeviceCmyk => 4usize,
        };
        let samples = unpack_bpc_samples(pixels, width, height, comps, bpc);
        let mut rgba = vec![255u8; px_count * 4];
        for i in 0..px_count {
            let s_off = i * comps;
            let o = i * 4;
            match comps {
                1 => {
                    let g = samples.get(s_off).copied().unwrap_or(0);
                    rgba[o] = g;
                    rgba[o + 1] = g;
                    rgba[o + 2] = g;
                }
                3 => {
                    rgba[o] = samples.get(s_off).copied().unwrap_or(0);
                    rgba[o + 1] = samples.get(s_off + 1).copied().unwrap_or(0);
                    rgba[o + 2] = samples.get(s_off + 2).copied().unwrap_or(0);
                }
                4 => {
                    let c = f32::from(samples.get(s_off).copied().unwrap_or(0)) / 255.0;
                    let m = f32::from(samples.get(s_off + 1).copied().unwrap_or(0)) / 255.0;
                    let y = f32::from(samples.get(s_off + 2).copied().unwrap_or(0)) / 255.0;
                    let k = f32::from(samples.get(s_off + 3).copied().unwrap_or(0)) / 255.0;
                    let inv_k = 1.0 - k;
                    rgba[o] = ((1.0 - c) * inv_k * 255.0).round() as u8;
                    rgba[o + 1] = ((1.0 - m) * inv_k * 255.0).round() as u8;
                    rgba[o + 2] = ((1.0 - y) * inv_k * 255.0).round() as u8;
                }
                _ => {}
            }
            rgba[o + 3] = 255;
        }
        Ok(Some(rgba))
    }

    fn ensure_font_loaded(&mut self, key: &str) {
        if self.font_cache.contains_key(key) {
            return;
        }
        let mut info = PdfFontInfo::default();
        if let Some(font_table_obj) = self.resources.get("Font").cloned() {
            if let Ok(font_table) = self.parser.resolve_dict(&font_table_obj, 0) {
                if let Some(font_obj) = font_table.get(key).cloned() {
                    if let Ok(fdict) = self.parser.resolve_dict(&font_obj, 0) {
                        if let Some(base) = fdict.get("BaseFont").and_then(PdfObj::as_name) {
                            let clean = base.split_once('+').map_or(base, |(_, rest)| rest);
                            let lower = clean.to_ascii_lowercase();
                            info.bold = lower.contains("bold") || lower.contains("black");
                            info.italic = lower.contains("italic") || lower.contains("oblique");
                            info.family = clean.split('-').next().unwrap_or(clean).to_owned();
                        }
                        if fdict.get("Subtype").and_then(PdfObj::as_name) == Some("Type0") {
                            info.is_two_byte = true;
                        }
                        if let Some(tu_obj) = fdict.get("ToUnicode").cloned() {
                            if let Ok(cmap_bytes) = self.parser.decode_stream_obj(&tu_obj) {
                                info.to_unicode = parse_to_unicode_cmap(&cmap_bytes);
                            }
                        }
                    }
                }
            }
        }
        self.font_cache.insert(key.to_owned(), info);
    }

    fn decode_font_string(&self, raw: &[u8]) -> String {
        if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
            return decode_utf16_be(&raw[2..]);
        }
        if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
            return String::from_utf8_lossy(&raw[3..]).into_owned();
        }
        if let Some(font) = self.font_cache.get(&self.gstate.font_key) {
            if !font.to_unicode.is_empty() {
                let mut out = String::new();
                if font.is_two_byte && raw.len().is_multiple_of(2) {
                    for pair in raw.as_chunks::<2>().0 {
                        let code = u16::from_be_bytes([pair[0], pair[1]]);
                        if let Some(mapped) = font.to_unicode.get(&code) {
                            out.push_str(mapped);
                        } else if let Some(ch) = char::from_u32(u32::from(code)) {
                            out.push(ch);
                        }
                    }
                    return out;
                }
                for &b in raw {
                    if let Some(mapped) = font.to_unicode.get(&u16::from(b)) {
                        out.push_str(mapped);
                    } else {
                        out.push(winansi_byte_to_char(b));
                    }
                }
                return out;
            }
        }
        decode_pdf_text_string(raw)
    }

    fn emit_text(&mut self, text: &str) -> IoResult<()> {
        if text.is_empty() {
            return Ok(());
        }
        let font = self
            .font_cache
            .get(&self.gstate.font_key)
            .cloned()
            .unwrap_or_default();

        // Combine CTM and Text Matrix; convert baseline Y-up pt into coree Y-down mm
        let combined_pt = self.gstate.ctm.compose(&self.text_matrix);
        let world_xf = self.page_to_mm.compose(&combined_pt);
        let font_pt = self.gstate.font_size_pt.max(1.0);
        let text_local_to_pdf = Affine2D::new(
            MM_TO_PT * (self.gstate.horiz_scale / 100.0),
            0.0,
            0.0,
            -MM_TO_PT,
            0.0,
            font_pt + self.gstate.text_rise,
        );
        let text_xf = world_xf.compose(&text_local_to_pdf);
        if !text_xf.is_finite() {
            return Ok(());
        }

        let style = TextStyle {
            font_family: font.family,
            font_size_pt: font_pt,
            bold: font.bold,
            italic: font.italic,
            fill: self.gstate.fill_color.with_alpha(self.gstate.fill_alpha),
            direction: TextDirection::Auto,
            ..TextStyle::default()
        };

        self.emitted_count += 1;
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("PDF Text {}", self.emitted_count),
            NodePayload::ArtisticText(ArtisticText::from_plain(text.to_owned(), style)),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none())
        .with_transform(text_xf);
        obj.blend_mode = self.gstate.blend_mode;
        self.ctx.doc.add_object(obj, Some(self.layer_id))?;

        // Advance text matrix horizontally by approximate glyph advance so consecutive Tj calls don't overlap
        let approx_adv =
            (text.chars().count() as f64) * font_pt * 0.55 * (self.gstate.horiz_scale / 100.0);
        self.text_matrix = self
            .text_matrix
            .compose(&Affine2D::translate(approx_adv, 0.0));
        Ok(())
    }
}

fn tokenize_content_stream(data: &[u8]) -> Vec<ContentToken> {
    let mut tokens = Vec::new();
    let mut pos = 0usize;
    let len = data.len();

    while pos < len && tokens.len() < MAX_CONTENT_TOKENS {
        skip_pdf_ws(data, &mut pos);
        if pos >= len {
            break;
        }
        let b = data[pos];
        match b {
            b'(' => tokens.push(ContentToken::Obj(PdfObj::Str(read_pdf_literal_string(
                data, &mut pos,
            )))),
            b'<' | b'[' | b'/' => {
                let before = pos;
                match parse_obj_from_slice(data, &mut pos, 0) {
                    Ok(obj) => tokens.push(ContentToken::Obj(obj)),
                    Err(_) => {
                        if pos == before {
                            pos += 1;
                        }
                    }
                }
                if pos == before && pos < len {
                    pos += 1;
                }
            }
            b']' | b'>' => {
                pos += 1;
            }
            _ => {
                let word = read_pdf_word(data, &mut pos);
                if word.is_empty() {
                    pos += 1;
                    continue;
                }
                // Skip inline image payloads (`BI ... ID <binary> EI`) safely!
                if word == "BI" {
                    skip_inline_image(data, &mut pos);
                    continue;
                }
                if word == "true" {
                    tokens.push(ContentToken::Obj(PdfObj::Bool(true)));
                } else if word == "false" {
                    tokens.push(ContentToken::Obj(PdfObj::Bool(false)));
                } else if word == "null" {
                    tokens.push(ContentToken::Obj(PdfObj::Null));
                } else if let Ok(iv) = word.parse::<i64>() {
                    tokens.push(ContentToken::Obj(PdfObj::Int(iv)));
                } else if let Ok(rv) = word.parse::<f64>() {
                    tokens.push(ContentToken::Obj(PdfObj::Real(rv)));
                } else {
                    tokens.push(ContentToken::Op(word));
                }
            }
        }
    }
    tokens
}

fn skip_inline_image(data: &[u8], pos: &mut usize) {
    let len = data.len();
    // Advance past ID marker first
    while *pos + 2 <= len {
        if &data[*pos..*pos + 2] == b"ID"
            && (*pos == 0 || is_pdf_ws(data[*pos - 1]))
            && data.get(*pos + 2).is_none_or(|&b| is_pdf_ws(b))
        {
            *pos += 3.min(len - *pos);
            break;
        }
        *pos += 1;
    }
    // Scan for EI delimited by whitespace
    while *pos + 2 <= len {
        if &data[*pos..*pos + 2] == b"EI"
            && (*pos == 0 || is_pdf_ws(data[*pos - 1]))
            && data
                .get(*pos + 2)
                .is_none_or(|&b| is_pdf_ws(b) || is_pdf_delim(b))
        {
            *pos += 2;
            return;
        }
        *pos += 1;
    }
    *pos = len;
}

/// Unpacks 1/2/4/8/16-bit packed PDF image samples into 8-bit values (`width * height * comps`).
fn unpack_bpc_samples(pixels: &[u8], width: u32, height: u32, comps: usize, bpc: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let total = w * h * comps;
    if bpc == 8 {
        let mut out = vec![0u8; total];
        let n = pixels.len().min(total);
        out[..n].copy_from_slice(&pixels[..n]);
        return out;
    }
    if bpc == 16 {
        let mut out = Vec::with_capacity(total);
        for pair in pixels.as_chunks::<2>().0.iter().take(total) {
            out.push(pair[0]);
        }
        out.resize(total, 0);
        return out;
    }
    let bpc_usize = match bpc {
        1 => 1usize,
        2 => 2usize,
        4 => 4usize,
        _ => return vec![0u8; total],
    };
    let max_val = ((1u32 << bpc_usize) - 1).max(1);
    let row_bytes = (w * comps * bpc_usize).div_ceil(8);
    let mut out = Vec::with_capacity(total);
    for y in 0..h {
        let row_start = y * row_bytes;
        let mut bit_off = 0usize;
        for _ in 0..(w * comps) {
            let byte_idx = row_start + bit_off / 8;
            let shift = 8usize.saturating_sub(bpc_usize + (bit_off % 8));
            let raw_byte = pixels.get(byte_idx).copied().unwrap_or(0);
            let sample = (u32::from(raw_byte) >> shift) & max_val;
            out.push(((sample * 255 + max_val / 2) / max_val) as u8);
            bit_off += bpc_usize;
        }
    }
    out
}

/// Box-downscales an RGBA buffer when its dimensions exceed `max_dim` or its byte size exceeds `max_bytes`.
fn downscale_rgba_if_needed(
    src_w: u32,
    src_h: u32,
    rgba: Vec<u8>,
    max_dim: u32,
    max_bytes: usize,
) -> (u32, u32, Vec<u8>) {
    let mut scale = 1.0_f64;
    let longest = src_w.max(src_h);
    if longest > max_dim {
        scale = f64::from(max_dim) / f64::from(longest);
    }
    let mut dst_w = ((f64::from(src_w) * scale).round() as u32).clamp(1, max_dim);
    let mut dst_h = ((f64::from(src_h) * scale).round() as u32).clamp(1, max_dim);

    let max_px = (max_bytes / 4).max(1);
    let cur_px = (dst_w as usize).saturating_mul(dst_h as usize);
    if cur_px > max_px {
        let factor = ((max_px as f64) / (cur_px as f64)).sqrt();
        dst_w = ((f64::from(dst_w) * factor).floor() as u32).max(1);
        dst_h = ((f64::from(dst_h) * factor).floor() as u32).max(1);
    }

    if dst_w == src_w && dst_h == src_h {
        return (src_w, src_h, rgba);
    }

    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;
    let mut sums = vec![[0u64; 4]; dw * dh];
    let mut counts = vec![0u32; dw * dh];

    for sy in 0..sh {
        let dy = ((sy as u64) * (dh as u64) / (sh as u64)) as usize;
        let dy = dy.min(dh - 1);
        for sx in 0..sw {
            let dx = ((sx as u64) * (dw as u64) / (sw as u64)) as usize;
            let dx = dx.min(dw - 1);
            let si = (sy * sw + sx) * 4;
            let di = dy * dw + dx;
            sums[di][0] += u64::from(rgba[si]);
            sums[di][1] += u64::from(rgba[si + 1]);
            sums[di][2] += u64::from(rgba[si + 2]);
            sums[di][3] += u64::from(rgba[si + 3]);
            counts[di] += 1;
        }
    }

    let mut out = Vec::with_capacity(dw * dh * 4);
    for (s, c) in sums.into_iter().zip(counts) {
        if c == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let cu = u64::from(c);
            out.push(((s[0] + cu / 2) / cu).min(255) as u8);
            out.push(((s[1] + cu / 2) / cu).min(255) as u8);
            out.push(((s[2] + cu / 2) / cu).min(255) as u8);
            out.push(((s[3] + cu / 2) / cu).min(255) as u8);
        }
    }
    (dst_w, dst_h, out)
}

/// Lightweight `/ToUnicode` CMap parser (`beginbfchar`/`beginbfrange`).
fn parse_to_unicode_cmap(bytes: &[u8]) -> HashMap<u16, String> {
    let mut map = HashMap::new();
    let text = String::from_utf8_lossy(bytes);
    let mut mode = 0u8; // 0 = none, 1 = bfchar, 2 = bfrange

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with("beginbfchar") {
            mode = 1;
            continue;
        }
        if trimmed.ends_with("beginbfrange") {
            mode = 2;
            continue;
        }
        if trimmed.starts_with("endbfchar") || trimmed.starts_with("endbfrange") {
            mode = 0;
            continue;
        }
        if mode == 1 {
            let hexes = extract_hex_tokens(trimmed);
            if hexes.len() >= 2 {
                if let Some(code) = bytes_to_u16(&hexes[0]) {
                    map.insert(code, decode_utf16_be(&hexes[1]));
                }
            }
        } else if mode == 2 {
            let hexes = extract_hex_tokens(trimmed);
            if hexes.len() >= 3 {
                if let (Some(lo), Some(hi)) = (bytes_to_u16(&hexes[0]), bytes_to_u16(&hexes[1])) {
                    if hi >= lo && (hi - lo) <= 512 {
                        let base_str = decode_utf16_be(&hexes[2]);
                        if let Some(first_ch) = base_str.chars().next() {
                            let base_cp = first_ch as u32;
                            for c in lo..=hi {
                                if let Some(ch) = char::from_u32(base_cp + u32::from(c - lo)) {
                                    map.insert(c, ch.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    map
}

fn extract_hex_tokens(line: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) != Some(&b'<') {
            out.push(read_pdf_hex_string(bytes, &mut i));
        } else {
            i += 1;
        }
    }
    out
}

fn bytes_to_u16(b: &[u8]) -> Option<u16> {
    match b.len() {
        1 => Some(u16::from(b[0])),
        2 => Some(u16::from_be_bytes([b[0], b[1]])),
        _ => None,
    }
}

fn decode_pdf_text_string(raw: &[u8]) -> String {
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        return decode_utf16_be(&raw[2..]);
    }
    if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    raw.iter().map(|&b| winansi_byte_to_char(b)).collect()
}

fn decode_utf16_be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Maps WinAnsi / PDFDocEncoding single bytes into Unicode characters.
fn winansi_byte_to_char(b: u8) -> char {
    match b {
        0x80 => '\u{20AC}', // €
        0x82 => '\u{201A}', // ‚
        0x83 => '\u{0192}', // ƒ
        0x84 => '\u{201E}', // „
        0x85 => '\u{2026}', // …
        0x86 => '\u{2020}', // †
        0x87 => '\u{2021}', // ‡
        0x88 => '\u{02C6}', // ˆ
        0x89 => '\u{2030}', // ‰
        0x8A => '\u{0160}', // Š
        0x8B => '\u{2039}', // ‹
        0x8C => '\u{0152}', // Œ
        0x8E => '\u{017D}', // Ž
        0x91 => '\u{2018}', // ‘
        0x92 => '\u{2019}', // ’
        0x93 => '\u{201C}', // “
        0x94 => '\u{201D}', // ”
        0x95 => '\u{2022}', // •
        0x96 => '\u{2013}', // –
        0x97 => '\u{2014}', // —
        0x98 => '\u{02DC}', // ˜
        0x99 => '\u{2122}', // ™
        0x9A => '\u{0161}', // š
        0x9B => '\u{203A}', // ›
        0x9C => '\u{0153}', // œ
        0x9E => '\u{017E}', // ž
        0x9F => '\u{0178}', // Ÿ
        other => char::from(other),
    }
}

fn map_pdf_blend_mode(bm: &str) -> BlendMode {
    match bm {
        "Multiply" => BlendMode::Multiply,
        "Screen" => BlendMode::Screen,
        "Overlay" => BlendMode::Overlay,
        "Darken" => BlendMode::Darken,
        "Lighten" => BlendMode::Lighten,
        "Difference" => BlendMode::Difference,
        "Exclusion" => BlendMode::Exclusion,
        "SoftLight" => BlendMode::SoftLight,
        "HardLight" => BlendMode::HardLight,
        "ColorDodge" => BlendMode::ColorDodge,
        "ColorBurn" => BlendMode::ColorBurn,
        "Hue" => BlendMode::Hue,
        "Saturation" => BlendMode::Saturation,
        "Color" => BlendMode::Color,
        "Luminosity" => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

// =============================================================================
// Low-level Byte Helpers
// =============================================================================

fn pdf_err(msg: impl Into<String>) -> IoError {
    IoError::InvalidFormat {
        format: "pdf".into(),
        message: msg.into(),
    }
}

fn is_pdf_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

fn is_pdf_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn skip_pdf_ws(data: &[u8], pos: &mut usize) {
    let len = data.len();
    while *pos < len {
        let b = data[*pos];
        if is_pdf_ws(b) {
            *pos += 1;
        } else if b == b'%' {
            while *pos < len && data[*pos] != b'\n' && data[*pos] != b'\r' {
                *pos += 1;
            }
        } else {
            break;
        }
    }
}

fn skip_inline_spaces(data: &[u8], pos: &mut usize) {
    while *pos < data.len() && matches!(data[*pos], b' ' | b'\t') {
        *pos += 1;
    }
}

fn read_ascii_uint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let start = *pos;
    let mut val = 0u64;
    while *pos < data.len() && data[*pos].is_ascii_digit() {
        val = val
            .checked_mul(10)?
            .checked_add(u64::from(data[*pos] - b'0'))?;
        *pos += 1;
    }
    (*pos > start).then_some(val)
}

fn read_be_uint(slice: &[u8]) -> u64 {
    let mut acc = 0u64;
    for &b in slice {
        acc = (acc << 8) | u64::from(b);
    }
    acc
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_last_startxref(bytes: &[u8]) -> Option<usize> {
    let tail_start = bytes.len().saturating_sub(8192);
    let needle = b"startxref";
    let rel = bytes[tail_start..]
        .windows(needle.len())
        .rposition(|w| w == needle)
        .map(|p| tail_start + p)
        .or_else(|| bytes.windows(needle.len()).rposition(|w| w == needle))?;
    let mut pos = rel + needle.len();
    skip_pdf_ws(bytes, &mut pos);
    read_ascii_uint(bytes, &mut pos).map(|v| v as usize)
}

fn effective_affine_scale(xf: &Affine2D) -> f64 {
    let sx = xf.m[0].hypot(xf.m[1]);
    let sy = xf.m[2].hypot(xf.m[3]);
    let avg = 0.5 * (sx + sy);
    if avg.is_finite() && avg > 1e-6 {
        avg
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::{export_pdf_to_bytes, PdfColorSpace, PdfExportOptions};
    use coree_core::{CornerStyle, ParametricShape, Rect2D};
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write as _;

    #[test]
    fn pdf_roundtrip_multipage_cmyk_rgb_text_raster_and_compressed_streams() {
        let mut doc = Document::new("Brochure", 100.0, 80.0, ColorMode::Rgb, 300.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Hero Rect",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 12.0, 40.0, 25.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgb(0.2, 0.6, 0.9)))
            .with_outline(OutlineStyle::solid(Color::rgb(0.1, 0.1, 0.1), 1.0)),
            None,
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Title",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Hello PDF 2026",
                    TextStyle {
                        font_family: "Helvetica".to_owned(),
                        font_size_pt: 16.0,
                        bold: true,
                        fill: Color::rgb(0.05, 0.05, 0.05),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(10.0, 50.0)),
            None,
        )
        .unwrap();

        // Embedded 4x4 RGBA raster image
        let rgba = [240u8, 80, 40, 255].repeat(16);
        let asset = RasterAsset::new(4, 4, rgba, 150.0, ColorMode::Rgb).unwrap();
        let asset_id = doc.register_asset(asset);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Photo",
                NodePayload::Raster(RasterLayerData {
                    asset_id,
                    width_mm: 20.0,
                    height_mm: 20.0,
                    filters: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(60.0, 10.0)),
            None,
        )
        .unwrap();

        // Second page with curved shape
        let p2 = doc.add_page("Page 2", 150.0, 100.0, 0.0);
        let p2_layer = doc.pages.iter().find(|p| p.id == p2).unwrap().layers[0].id;
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Badge",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 50.0,
                    height: 30.0,
                    corner_radii: [6.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::rgb(0.9, 0.1, 0.4))),
            Some(p2_layer),
        )
        .unwrap();

        let opts = PdfExportOptions {
            color_space: PdfColorSpace::DeviceRgb,
            include_bleed: false,
            compress_streams: true,
            ..PdfExportOptions::default()
        };
        let pdf_bytes = export_pdf_to_bytes(&doc, &opts).unwrap();
        let imported = import_pdf_bytes(&pdf_bytes, "Fallback").unwrap();

        assert_eq!(imported.pages.len(), 2);
        assert!((imported.pages[0].width_mm - 100.0).abs() < 0.1);
        assert!((imported.pages[0].height_mm - 80.0).abs() < 0.1);
        assert!((imported.pages[1].width_mm - 150.0).abs() < 0.1);
        assert!((imported.pages[1].height_mm - 100.0).abs() < 0.1);

        // Verify page 1 recovered vector path, text, and raster image
        let p1_objs: Vec<&SceneObject> = imported.pages[0].layers[0]
            .objects
            .iter()
            .filter_map(|id| imported.objects.get(id))
            .collect();
        assert!(p1_objs
            .iter()
            .any(|o| matches!(&o.payload, NodePayload::Path(_))));
        assert!(p1_objs.iter().any(|o| matches!(
            &o.payload,
            NodePayload::ArtisticText(t) if t.plain_text().contains("Hello PDF 2026")
        )));
        assert!(p1_objs
            .iter()
            .any(|o| matches!(&o.payload, NodePayload::Raster(_))));
        assert!(!imported.assets.is_empty());

        // Verify page 2 has the curved badge
        assert!(!imported.pages[1].layers[0].objects.is_empty());
    }

    #[test]
    fn pdf_1_5_xref_stream_objstm_png_predictor_form_xobject_and_rotate() {
        // Synthesize a PDF 1.5 with:
        // - Object 1 (Catalog) & Object 2 (Pages) stored INSIDE compressed ObjStm (Obj 5)!
        // - Object 3 (Page with /Rotate 90 and Form XObject /Fm1)
        // - Object 4 (Form XObject stream using ASCII85Decode!)
        // - Object 6 (Page content stream invoking /Fm1 Do and UTF-16BE text!)
        // - Object 7 (/Type /XRef stream compressed with FlateDecode + PNG Up Predictor 12!)
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n");

        // Obj 3: Page (uncompressed)
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate 90 /Resources << /XObject << /Fm1 4 0 R >> >> /Contents 6 0 R >>\nendobj\n",
        );

        // Obj 4: Form XObject with ASCII85Decode (`0 0 50 50 re f`)
        let off4 = pdf.len();
        let form_a85 = b"0`V1R0`V1R3#O_@3#O_@F`M~>"; // we'll use ASCIIHexDecode for exact simplicity
        let _ = form_a85;
        let form_hex = b"3020302035302035302072652066>"; // "0 0 50 50 re f"
        let header4 = format!(
            "4 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [1 0 0 1 10 10] /Filter /ASCIIHexDecode /Length {} >>\nstream\n",
            form_hex.len()
        );
        pdf.extend_from_slice(header4.as_bytes());
        pdf.extend_from_slice(form_hex);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Obj 5: ObjStm containing Obj 1 (Catalog) and Obj 2 (Pages)
        let off5 = pdf.len();
        let obj1_body = b"<< /Type /Catalog /Pages 2 0 R >>";
        let obj2_body = b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>";
        let header_str = format!("1 0 2 {} ", obj1_body.len() + 1);
        let first = header_str.len();
        let mut stm_plain = Vec::new();
        stm_plain.extend_from_slice(header_str.as_bytes());
        stm_plain.extend_from_slice(obj1_body);
        stm_plain.push(b' ');
        stm_plain.extend_from_slice(obj2_body);
        let mut enc5 = ZlibEncoder::new(Vec::new(), Compression::default());
        enc5.write_all(&stm_plain).unwrap();
        let stm_comp = enc5.finish().unwrap();
        let h5 = format!(
            "5 0 obj\n<< /Type /ObjStm /N 2 /First {first} /Filter /FlateDecode /Length {} >>\nstream\n",
            stm_comp.len()
        );
        pdf.extend_from_slice(h5.as_bytes());
        pdf.extend_from_slice(&stm_comp);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Obj 6: Page content stream calling /Fm1 Do + UTF-16BE text (<FEFF004F004B> = "OK")
        let off6 = pdf.len();
        let content6 = b"q /Fm1 Do Q BT /F1 12 Tf 1 0 0 1 20 20 Tm <FEFF004F004B> Tj ET";
        let h6 = format!("6 0 obj\n<< /Length {} >>\nstream\n", content6.len());
        pdf.extend_from_slice(h6.as_bytes());
        pdf.extend_from_slice(content6);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Obj 7: /Type /XRef stream with /W [1 4 1] and PNG Up Predictor 12!
        let off7 = pdf.len();
        let entries: Vec<[u8; 6]> = vec![
            [0, 0, 0, 0, 0, 255], // 0: free
            [2, 0, 0, 0, 5, 0],   // 1: compressed in ObjStm 5, idx 0
            [2, 0, 0, 0, 5, 1],   // 2: compressed in ObjStm 5, idx 1
            [
                1,
                (off3 >> 24) as u8,
                (off3 >> 16) as u8,
                (off3 >> 8) as u8,
                off3 as u8,
                0,
            ],
            [
                1,
                (off4 >> 24) as u8,
                (off4 >> 16) as u8,
                (off4 >> 8) as u8,
                off4 as u8,
                0,
            ],
            [
                1,
                (off5 >> 24) as u8,
                (off5 >> 16) as u8,
                (off5 >> 8) as u8,
                off5 as u8,
                0,
            ],
            [
                1,
                (off6 >> 24) as u8,
                (off6 >> 16) as u8,
                (off6 >> 8) as u8,
                off6 as u8,
                0,
            ],
            [
                1,
                (off7 >> 24) as u8,
                (off7 >> 16) as u8,
                (off7 >> 8) as u8,
                off7 as u8,
                0,
            ],
        ];
        // Apply PNG Up predictor (filter byte = 2)
        let mut pred_buf = Vec::new();
        let mut prev = [0u8; 6];
        for row in &entries {
            pred_buf.push(2); // PNG Up
            for i in 0..6 {
                pred_buf.push(row[i].wrapping_sub(prev[i]));
            }
            prev = *row;
        }
        let mut enc7 = ZlibEncoder::new(Vec::new(), Compression::default());
        enc7.write_all(&pred_buf).unwrap();
        let xref_comp = enc7.finish().unwrap();

        let h7 = format!(
            "7 0 obj\n<< /Type /XRef /Size 8 /Root 1 0 R /W [1 4 1] /Filter /FlateDecode /DecodeParms << /Columns 6 /Predictor 12 >> /Length {} >>\nstream\n",
            xref_comp.len()
        );
        pdf.extend_from_slice(h7.as_bytes());
        pdf.extend_from_slice(&xref_comp);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        let tail = format!("startxref\n{off7}\n%%EOF\n");
        pdf.extend_from_slice(tail.as_bytes());

        let doc = import_pdf_bytes(&pdf, "PDF 1.5 XRefStream").unwrap();
        assert_eq!(doc.pages.len(), 1);
        // MediaBox was 200x100 pt, Rotate 90 -> swapped to 100x200 pt!
        assert!((doc.pages[0].width_mm - 100.0 * PT_TO_MM).abs() < 0.1);
        assert!((doc.pages[0].height_mm - 200.0 * PT_TO_MM).abs() < 0.1);
        assert_eq!(doc.objects.len(), 2); // Form XObject rect + UTF-16BE "OK" text
        assert!(doc.objects.values().any(|o| matches!(
            &o.payload,
            NodePayload::ArtisticText(t) if t.plain_text() == "OK"
        )));
    }

    #[test]
    fn pdf_flate_zip_bomb_is_rejected() {
        // Compress 1 MiB of zeros (~1 KB compressed => ~1000:1 ratio > 250x limit!)
        let zeros = vec![0u8; 1024 * 1024];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&zeros).unwrap();
        let bomb = enc.finish().unwrap();

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>\nendobj\n",
        );
        let off4 = pdf.len();
        let h4 = format!(
            "4 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
            bomb.len()
        );
        pdf.extend_from_slice(h4.as_bytes());
        pdf.extend_from_slice(&bomb);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        let xref_off = pdf.len();
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n"
        );
        pdf.extend_from_slice(xref.as_bytes());

        let err = import_pdf_bytes(&pdf, "Bomb").unwrap_err();
        assert!(matches!(err, IoError::ZipBomb { .. }));
    }

    #[test]
    fn pdf_ascii85_and_tiff_predictor_decode_cleanly() {
        let decoded = decode_ascii85(b"9jqo^BlbD-BleB1DJ+*+F(f,q/0JhKF<GL>Cj@.4Gp$d7F!,L7@<6@)/0JDEF<G%<+EV:2F!,O<DJ+*.@<*K0@<6L(Df-\\0Ec5e;DffZ(EZee.Bl.9pF\"AGXBPCsi+DGm>@3BB/F*&OCAfu2/AKYi(DIb:@FD,*)+C]U=@3BN#EcYf8ATD3s@q?d$AftVqCh[NqF<G:8+EV:.+Cf>-FD5W8ARlolDIal(DId<j@<?3r@:F%a+D58'ATD4$Bl@l3De:,-DJs`8ARoFb/0JMK@qB4^F!,R<AKZ&-DfTqBG%G>uD.RTpAKYo'+CT/5+Cei#DII?(E,9)oF*2M7/c~>", 1024 * 1024).unwrap();
        assert!(String::from_utf8_lossy(&decoded).starts_with("Man is distinguished"));

        let mut dp = BTreeMap::new();
        dp.insert("Predictor".to_owned(), PdfObj::Int(2));
        dp.insert("Columns".to_owned(), PdfObj::Int(4));
        dp.insert("Colors".to_owned(), PdfObj::Int(1));
        dp.insert("BitsPerComponent".to_owned(), PdfObj::Int(8));
        // Differences [10, 5, 5, 5] -> [10, 15, 20, 25]
        let unpred = apply_pdf_predictor(&[10, 5, 5, 5], &dp).unwrap();
        assert_eq!(unpred, vec![10, 15, 20, 25]);
    }

    #[test]
    fn pdf_adversarial_malformed_delims_length_cycle_bad_bpc_and_amplification() {
        // 1. Unexpected delimiter inside dict/array/content stream must NOT infinite-loop or OOM
        let mut p1 = 0usize;
        assert!(parse_obj_from_slice(b"<< ] >>", &mut p1, 0).is_ok());
        assert_eq!(p1, 7);
        let mut p2 = 0usize;
        assert!(parse_obj_from_slice(b"[ ) ]", &mut p2, 0).is_ok());
        assert_eq!(p2, 5);
        let toks = tokenize_content_stream(b"[ ) ] << ] >> 0 0 m");
        assert!(!toks.is_empty());

        // 2. Self-referential /Length 1 0 R must NOT stack-overflow
        let cyclic_len =
            b"%PDF-1.4\n1 0 obj\n<< /Length 1 0 R >>\nstream\nabc\nendstream\nendobj\n";
        let mut parser = PdfParser::new(cyclic_len, 0);
        parser.xref.insert(1, XRefEntry::InUse { offset: 9 });
        let resolved = parser.resolve(&PdfObj::Ref(1, 0), 0).unwrap();
        assert!(matches!(resolved, PdfObj::Stream { .. }));

        // 3. Invalid BitsPerComponent (3, 5, 6, 7) must NOT panic with usize underflow
        let bad_bpc_pdf = b"%PDF-1.4\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n\
3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n\
4 0 obj\n<< /Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceGray /BitsPerComponent 3 /Length 8 >>\nstream\n12345678\nendstream\nendobj\n\
5 0 obj\n<< /Length 7 >>\nstream\n/Im1 Do\nendstream\nendobj\n%%EOF\n";
        let doc = import_pdf_bytes(bad_bpc_pdf, "BadBpc").unwrap();
        assert_eq!(doc.pages.len(), 1);
        assert!(doc.assets.is_empty());

        // 4. ASCII85 `z` amplification & repeated /Contents array cap must reject cleanly
        let z_bomb = vec![b'z'; 1000];
        assert!(matches!(
            decode_ascii85(&z_bomb, 512),
            Err(IoError::ZipBomb { .. })
        ));

        let mut repeated_contents = String::from(
            "%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
             2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n\
             3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents [",
        );
        for _ in 0..2_100 {
            repeated_contents.push_str("4 0 R ");
        }
        repeated_contents.push_str(
            "] >>\nendobj\n4 0 obj\n<< /Length 2 >>\nstream\n  \nendstream\nendobj\n%%EOF\n",
        );
        let err = import_pdf_bytes(repeated_contents.as_bytes(), "Amp").unwrap_err();
        assert!(matches!(err, IoError::LimitExceeded { .. }));
    }
}
