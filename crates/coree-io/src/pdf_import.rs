// SPDX-License-Identifier: MIT OR Apache-2.0

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
use crate::pdf_crypto::{parse_cipher_map, EncryptDictParams, PdfCipher, PdfCryptoCtx};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, ColorMode, Document, Fill, GradientStop,
    LayerId, LineCap, LineJoin, NodePayload, ObjectId, OutlineStyle, Point2D, RasterAsset,
    RasterLayerData, Rect2D, SceneObject, TextDirection, TextStyle, Vec2D,
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
    import_pdf_with_password(path, "")
}

/// Imports a PDF (or PDF-compatible `.ai`) file from `path`, decrypting encrypted documents using `password`.
pub fn import_pdf_with_password(path: &Path, password: &str) -> IoResult<Document> {
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
    import_pdf_bytes_with_password(&bytes, title, password)
}

/// Imports a PDF 1.0–2.0 (or PDF-compatible `.ai`) byte slice into a multipage [`Document`].
pub fn import_pdf_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    import_pdf_bytes_with_password(bytes, title, "")
}

/// Imports a PDF 1.0–2.0 (or PDF-compatible `.ai`) byte slice, decrypting encrypted documents using `password`.
pub fn import_pdf_bytes_with_password(
    bytes: &[u8],
    title: &str,
    password: &str,
) -> IoResult<Document> {
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
        parser.init_encryption(password)?;
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
    doc.metadata_mut().source_format = Some("pdf".to_owned());
    doc.metadata_mut().author = info_author;
    doc.metadata_mut().subject = info_subject;

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
            ctx.doc.set_page_dimensions(0, w_mm, h_mm)?;
            ctx.doc.pages()[0].layers[0].id
        } else {
            let pid = ctx
                .doc
                .add_page(format!("Page {}", page_idx + 1), w_mm, h_mm, 0.0);
            ctx.doc
                .page_by_id(pid)
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
                w_mm,
                h_mm,
                spec.resources.clone(),
            );
            interp.run(&content_bytes, 0)?;
        }
    }

    if ctx.saw_cmyk && !ctx.saw_rgb {
        ctx.doc.set_color_mode(ColorMode::Cmyk);
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
    encrypt_obj: Option<PdfObj>,
    trailer_id0: Vec<u8>,
    crypto: Option<PdfCryptoCtx>,
    stream_obj_refs: HashMap<usize, (u32, u16)>,
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
            encrypt_obj: None,
            trailer_id0: Vec::new(),
            crypto: None,
            stream_obj_refs: HashMap::new(),
            obj_stm_cache: HashMap::new(),
            resolving_objs: HashSet::new(),
            cumulative_decompressed: 0,
        }
    }

    fn init_encryption(&mut self, password: &str) -> IoResult<()> {
        let Some(enc_obj) = self.encrypt_obj.clone() else {
            return Err(pdf_err(
                "PDF marked /Encrypt but missing /Encrypt dictionary",
            ));
        };
        let enc_obj_num = enc_obj.as_ref().map(|(n, _)| n);
        let dict = self.resolve_dict(&enc_obj, 0)?;
        let filter = dict
            .get("Filter")
            .and_then(PdfObj::as_name)
            .unwrap_or("Standard");
        let v = dict.get("V").and_then(PdfObj::as_i64).unwrap_or(0);
        let r = dict.get("R").and_then(PdfObj::as_i64).unwrap_or(2);
        let length_bits = dict
            .get("Length")
            .and_then(PdfObj::as_i64)
            .unwrap_or(if r == 2 { 40 } else { 128 })
            .clamp(40, 256) as usize;
        let p = dict.get("P").and_then(PdfObj::as_i64).unwrap_or(0) as i32;
        let o = match dict.get("O") {
            Some(PdfObj::Str(b)) => b.as_slice(),
            _ => &[],
        };
        let u = match dict.get("U") {
            Some(PdfObj::Str(b)) => b.as_slice(),
            _ => &[],
        };
        let oe = match dict.get("OE") {
            Some(PdfObj::Str(b)) => b.as_slice(),
            _ => &[],
        };
        let ue = match dict.get("UE") {
            Some(PdfObj::Str(b)) => b.as_slice(),
            _ => &[],
        };
        let encrypt_metadata = !matches!(dict.get("EncryptMetadata"), Some(PdfObj::Bool(false)));

        let mut cf_cfms: BTreeMap<String, PdfCipher> = BTreeMap::new();
        if let Some(cf_obj) = dict.get("CF").cloned() {
            if let Ok(cf_dict) = self.resolve_dict(&cf_obj, 0) {
                for (cf_name, cf_val) in cf_dict {
                    if let Ok(sub) = self.resolve_dict(&cf_val, 1) {
                        let cfm = match sub.get("CFM").and_then(PdfObj::as_name) {
                            Some("AESV2") => PdfCipher::Aes128Cbc,
                            Some("AESV3") => PdfCipher::Aes256Cbc,
                            Some("None") => PdfCipher::None,
                            _ => PdfCipher::Rc4,
                        };
                        cf_cfms.insert(cf_name, cfm);
                    }
                }
            }
        }
        let strf = dict.get("StrF").and_then(PdfObj::as_name);
        let stmf = dict.get("StmF").and_then(PdfObj::as_name);
        let (str_cipher, stm_cipher) = parse_cipher_map(&cf_cfms, strf, stmf, v);

        let params = EncryptDictParams {
            filter,
            v,
            r,
            length_bits,
            p,
            o,
            u,
            oe,
            ue,
            id0: &self.trailer_id0,
            encrypt_metadata,
            str_cipher,
            stm_cipher,
            encrypt_obj_num: enc_obj_num,
        };
        let ctx = PdfCryptoCtx::from_params(&params, password)?;
        self.crypto = Some(ctx);
        Ok(())
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
        if let Some(enc) = dict.get("Encrypt") {
            if !matches!(enc, PdfObj::Null) {
                self.has_encrypt = true;
                if self.encrypt_obj.is_none() {
                    self.encrypt_obj = Some(enc.clone());
                }
            }
        }
        if self.trailer_id0.is_empty() {
            if let Some(PdfObj::Array(ids)) = dict.get("ID") {
                if let Some(PdfObj::Str(id0)) = ids.first() {
                    self.trailer_id0 = id0.clone();
                }
            }
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
            let decoded = self.decode_stream_slice(&dict, start, len)?;

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
        let Some(gen_num) = read_ascii_uint(self.bytes, pos) else {
            return Ok(None);
        };
        let gen_u16 = gen_num.min(u64::from(u16::MAX)) as u16;
        skip_pdf_ws(self.bytes, pos);
        if !self.bytes[*pos..].starts_with(b"obj") {
            return Ok(None);
        }
        *pos += 3;
        skip_pdf_ws(self.bytes, pos);

        let mut val = self.parse_obj_at(self.bytes, pos, 0)?;
        if let Some(ref crypto) = self.crypto {
            decrypt_pdf_obj_strings(crypto, &mut val, obj_num as u32, gen_u16);
        }
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
                self.stream_obj_refs
                    .insert(stream_start, (obj_num as u32, gen_u16));
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
        self.decode_stream_slice(&dict, start, len)
    }

    fn decode_stream_slice(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        start: usize,
        len: usize,
    ) -> IoResult<Vec<u8>> {
        let raw = &self.bytes[start..start.saturating_add(len).min(self.bytes.len())];
        let obj_ref = self.stream_obj_refs.get(&start).copied();
        self.decode_stream_bytes_for_obj(dict, raw, obj_ref)
    }

    fn decode_stream_bytes(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        raw: &[u8],
    ) -> IoResult<Vec<u8>> {
        self.decode_stream_bytes_for_obj(dict, raw, None)
    }

    fn decode_stream_bytes_for_obj(
        &mut self,
        dict: &BTreeMap<String, PdfObj>,
        raw: &[u8],
        obj_ref: Option<(u32, u16)>,
    ) -> IoResult<Vec<u8>> {
        let filters = self.extract_filter_chain(dict, 0)?;
        if filters.len() > MAX_FILTER_CHAIN_LEN {
            return Err(IoError::LimitExceeded {
                limit: format!("max_pdf_filter_chain={MAX_FILTER_CHAIN_LEN}"),
                actual: format!("{} filters", filters.len()),
            });
        }
        let parms = self.extract_decode_parms(dict, 0)?;

        let mut data = if let (Some(ref crypto), Some((obj_num, gen_num))) = (&self.crypto, obj_ref)
        {
            let stream_type = dict.get("Type").and_then(PdfObj::as_name);
            let crypt_name = filters
                .iter()
                .position(|f| f == "Crypt")
                .and_then(|idx| parms.get(idx).and_then(|p| p.as_ref()))
                .and_then(|dp| dp.get("Name"))
                .and_then(PdfObj::as_name);
            crypto.decrypt_stream(raw, obj_num, gen_num, stream_type, &filters, crypt_name)
        } else {
            raw.to_vec()
        };

        for (idx, filter) in filters.iter().enumerate() {
            let dp = parms.get(idx).and_then(|p| p.as_ref());
            let rem = MAX_CUMULATIVE_DECOMPRESSED.saturating_sub(self.cumulative_decompressed);
            let step_cap = MAX_SINGLE_STREAM_DECOMPRESSED.min(rem);
            match filter.as_str() {
                "Crypt" => {}
                "FlateDecode" | "Fl" => {
                    data = self.flate_decode_bounded(&data)?;
                    if let Some(dp_dict) = dp {
                        data = apply_pdf_predictor(&data, dp_dict)?;
                    }
                }
                "LZWDecode" | "LZW" => {
                    let early_change = dp
                        .and_then(|d| d.get("EarlyChange"))
                        .and_then(PdfObj::as_i64)
                        .unwrap_or(1)
                        != 0;
                    data = self.lzw_decode_bounded(&data, early_change)?;
                    if let Some(dp_dict) = dp {
                        data = apply_pdf_predictor(&data, dp_dict)?;
                    }
                }
                "CCITTFaxDecode" | "CCF" => {
                    data = self.ccitt_decode_bounded(&data, dict, dp)?;
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
                "JBIG2Decode" | "JPXDecode" => {
                    // Bounded pass-through for JBIG2/JPX payloads (enforces stream size cap).
                    if data.len() > MAX_SINGLE_STREAM_DECOMPRESSED {
                        return Err(IoError::LimitExceeded {
                            limit: format!("max_pdf_stream_bytes={MAX_SINGLE_STREAM_DECOMPRESSED}"),
                            actual: format!("{} bytes", data.len()),
                        });
                    }
                    break;
                }
                _ => {
                    break;
                }
            }
        }
        Ok(data)
    }

    fn lzw_decode_bounded(&mut self, compressed: &[u8], early_change: bool) -> IoResult<Vec<u8>> {
        let mut decoder = if early_change {
            weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
        } else {
            weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8)
        };
        let mut out = Vec::new();
        let mut in_off = 0usize;
        let mut buf = [0u8; 16 * 1024];
        let denom = compressed.len().max(1) as f64;
        loop {
            let res = decoder.decode_bytes(&compressed[in_off..], &mut buf);
            in_off = in_off.saturating_add(res.consumed_in).min(compressed.len());
            if res.consumed_out > 0 {
                let n = res.consumed_out;
                let next_len = out.len().saturating_add(n);
                if next_len > MAX_SINGLE_STREAM_DECOMPRESSED {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_pdf_stream_bytes={MAX_SINGLE_STREAM_DECOMPRESSED}"),
                        actual: format!("stream exceeds {MAX_SINGLE_STREAM_DECOMPRESSED} bytes"),
                    });
                }
                self.charge_cumulative_bytes(n, "LZWDecode")?;
                if next_len >= MIN_RATIO_CHECK_BYTES
                    && (next_len as f64) / denom > MAX_DECOMPRESSION_RATIO
                {
                    return Err(IoError::ZipBomb {
                        reason: "PDF LZWDecode ratio exceeds limit".into(),
                    });
                }
                out.extend_from_slice(&buf[..n]);
            }
            match res.status {
                Ok(weezl::LzwStatus::Done) => break,
                Ok(weezl::LzwStatus::Ok) => {
                    if res.consumed_in == 0 && res.consumed_out == 0 {
                        break;
                    }
                }
                Ok(weezl::LzwStatus::NoProgress) | Err(_) => break,
            }
        }
        Ok(out)
    }

    fn ccitt_decode_bounded(
        &mut self,
        compressed: &[u8],
        img_dict: &BTreeMap<String, PdfObj>,
        dp: Option<&BTreeMap<String, PdfObj>>,
    ) -> IoResult<Vec<u8>> {
        let cols = dp
            .and_then(|d| d.get("Columns"))
            .or_else(|| img_dict.get("Width"))
            .and_then(PdfObj::as_i64)
            .unwrap_or(1728)
            .clamp(1, i64::from(MAX_PDF_IMAGE_DIM)) as u16;
        let rows = dp
            .and_then(|d| d.get("Rows"))
            .or_else(|| img_dict.get("Height"))
            .and_then(PdfObj::as_i64)
            .unwrap_or(0)
            .clamp(0, i64::from(MAX_PDF_IMAGE_DIM)) as u16;
        let k = dp
            .and_then(|d| d.get("K"))
            .and_then(PdfObj::as_i64)
            .unwrap_or(0);
        let black_is_1 = matches!(dp.and_then(|d| d.get("BlackIs1")), Some(PdfObj::Bool(true)));
        let row_bytes = usize::from(cols).div_ceil(8);
        let expected_rows = if rows > 0 {
            usize::from(rows)
        } else {
            MAX_PDF_IMAGE_DIM as usize
        };
        let max_bytes = expected_rows
            .saturating_mul(row_bytes)
            .min(MAX_SINGLE_STREAM_DECOMPRESSED);

        let mut packed = Vec::new();
        let mut emit_line = |transitions: &[u16]| {
            if packed.len().saturating_add(row_bytes) > max_bytes {
                return;
            }
            let row_start = packed.len();
            let white_byte = if black_is_1 { 0x00u8 } else { 0xFFu8 };
            packed.resize(row_start + row_bytes, white_byte);
            for (x, pel) in fax::decoder::pels(transitions, cols).enumerate() {
                let bit_val = match (pel, black_is_1) {
                    (fax::Color::Black, true) => 1u8,
                    (fax::Color::Black, false) => 0u8,
                    (fax::Color::White, true) => 0u8,
                    (fax::Color::White, false) => 1u8,
                };
                if bit_val == 1 {
                    packed[row_start + x / 8] |= 1 << (7 - (x % 8));
                } else {
                    packed[row_start + x / 8] &= !(1 << (7 - (x % 8)));
                }
            }
        };

        if k < 0 {
            let h_opt = (rows > 0).then_some(rows);
            fax::decoder::decode_g4(compressed.iter().copied(), cols, h_opt, |t| emit_line(t));
        } else {
            fax::decoder::decode_g3(compressed.iter().copied(), |t| emit_line(t));
        }

        if rows > 0 {
            let fill_byte = if black_is_1 { 0x00u8 } else { 0xFFu8 };
            packed.resize(usize::from(rows) * row_bytes, fill_byte);
        }
        self.charge_cumulative_bytes(packed.len(), "CCITTFaxDecode")?;
        Ok(packed)
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
                let out = self.decode_stream_slice(&dict, start, len)?;
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
        if val > u64::from(u32::MAX) {
            return Err(IoError::InvalidFormat {
                format: "pdf".into(),
                message: "PDF ASCII85Decode partial 5-tuple overflows u32".into(),
            });
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
enum PdfFunction {
    Exponential {
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
        domain: (f64, f64),
    },
    Stitching {
        domain: (f64, f64),
        bounds: Vec<f64>,
        encode: Vec<(f64, f64)>,
        funcs: Vec<PdfFunction>,
    },
    Sampled {
        domain: (f64, f64),
        range: Vec<(f64, f64)>,
        size: usize,
        bps: u32,
        samples: Vec<u8>,
    },
}

impl PdfFunction {
    fn eval(&self, t_in: f64) -> Vec<f64> {
        match self {
            Self::Exponential { c0, c1, n, domain } => {
                let x = t_in.clamp(domain.0, domain.1);
                let len = (domain.1 - domain.0).abs();
                let u = if len > 1e-12 {
                    ((x - domain.0) / (domain.1 - domain.0)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let p = u.powf(*n);
                let out_len = c0.len().max(c1.len()).max(1);
                (0..out_len)
                    .map(|i| {
                        let a = c0.get(i).copied().unwrap_or(0.0);
                        let b = c1.get(i).copied().unwrap_or(1.0);
                        (a + p * (b - a)).clamp(0.0, 1.0)
                    })
                    .collect()
            }
            Self::Stitching {
                domain,
                bounds,
                encode,
                funcs,
            } => {
                if funcs.is_empty() {
                    return vec![t_in.clamp(0.0, 1.0)];
                }
                let x = t_in.clamp(domain.0, domain.1);
                let mut idx = 0usize;
                for (i, &b) in bounds.iter().enumerate() {
                    if x >= b {
                        idx = i + 1;
                    }
                }
                idx = idx.min(funcs.len() - 1);
                let sub_lo = if idx == 0 { domain.0 } else { bounds[idx - 1] };
                let sub_hi = if idx < bounds.len() {
                    bounds[idx]
                } else {
                    domain.1
                };
                let enc = encode.get(idx).copied().unwrap_or((0.0, 1.0));
                let span = sub_hi - sub_lo;
                let mapped = if span.abs() > 1e-12 {
                    enc.0 + ((x - sub_lo) / span) * (enc.1 - enc.0)
                } else {
                    enc.0
                };
                funcs[idx].eval(mapped)
            }
            Self::Sampled {
                domain,
                range,
                size,
                bps,
                samples,
            } => {
                if *size == 0 || samples.is_empty() {
                    return vec![t_in.clamp(0.0, 1.0)];
                }
                let x = t_in.clamp(domain.0, domain.1);
                let span = (domain.1 - domain.0).abs();
                let u = if span > 1e-12 && *size > 1 {
                    ((x - domain.0) / (domain.1 - domain.0)).clamp(0.0, 1.0) * ((*size - 1) as f64)
                } else {
                    0.0
                };
                let i0 = (u.floor() as usize).min(size.saturating_sub(1));
                let i1 = (i0 + 1).min(size.saturating_sub(1));
                let frac = u - (i0 as f64);
                let n_out = range.len().max(1);
                let max_val = ((1u64 << (*bps).min(32)) - 1).max(1) as f64;
                let read_sample = |si: usize, ci: usize| -> f64 {
                    let elem = si * n_out + ci;
                    let raw = match bps {
                        16 => samples
                            .get(elem * 2..elem * 2 + 2)
                            .map_or(0.0, |b| f64::from(u16::from_be_bytes([b[0], b[1]]))),
                        _ => samples.get(elem).copied().map_or(0.0, f64::from),
                    };
                    let (r0, r1) = range.get(ci).copied().unwrap_or((0.0, 1.0));
                    (r0 + (raw / max_val) * (r1 - r0)).clamp(0.0, 1.0)
                };
                (0..n_out)
                    .map(|ci| {
                        let v0 = read_sample(i0, ci);
                        let v1 = read_sample(i1, ci);
                        (v0 + frac * (v1 - v0)).clamp(0.0, 1.0)
                    })
                    .collect()
            }
        }
    }
}

#[derive(Debug, Clone)]
enum PdfColorSpaceKind {
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
    Separation {
        name: String,
        alternate_is_cmyk: bool,
        alternate_comps: usize,
        tint_fn: Option<PdfFunction>,
    },
}

#[derive(Debug, Clone)]
struct PdfFontInfo {
    family: String,
    bold: bool,
    italic: bool,
    to_unicode: HashMap<u16, String>,
    is_two_byte: bool,
    first_char: u16,
    widths: Vec<f64>,
    cid_widths: HashMap<u16, f64>,
    default_cid_width: f64,
    differences: HashMap<u8, char>,
}

impl Default for PdfFontInfo {
    fn default() -> Self {
        Self {
            family: "Helvetica".to_owned(),
            bold: false,
            italic: false,
            to_unicode: HashMap::new(),
            is_two_byte: false,
            first_char: 0,
            widths: Vec::new(),
            cid_widths: HashMap::new(),
            default_cid_width: 1000.0,
            differences: HashMap::new(),
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
    InlineImage {
        dict: BTreeMap<String, PdfObj>,
        raw: Vec<u8>,
    },
}

struct PdfContentInterpreter<'p, 'a, 'd> {
    parser: &'p mut PdfParser<'a>,
    ctx: &'p mut ImportContext<'d>,
    layer_id: LayerId,
    page_to_mm: Affine2D,
    page_w_mm: f64,
    page_h_mm: f64,
    resources: BTreeMap<String, PdfObj>,
    gstate: PdfGState,
    gstack: Vec<PdfGState>,
    path: BezPath,
    cur_pt: Option<Point2D>,
    subpath_start: Option<Point2D>,
    pending_clip: Option<BezierPath>,
    clip_stack: Vec<(usize, BezierPath, Vec<ObjectId>)>,
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
        page_w_mm: f64,
        page_h_mm: f64,
        resources: BTreeMap<String, PdfObj>,
    ) -> Self {
        Self {
            parser,
            ctx,
            layer_id,
            page_to_mm,
            page_w_mm,
            page_h_mm,
            resources,
            gstate: PdfGState::default(),
            gstack: Vec::new(),
            path: BezPath::new(),
            cur_pt: None,
            subpath_start: None,
            pending_clip: None,
            clip_stack: Vec::new(),
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
                ContentToken::InlineImage { dict, raw } => {
                    self.op_count += 1;
                    if self.op_count > MAX_OPS_PER_PAGE {
                        break;
                    }
                    self.exec_inline_image(&dict, &raw)?;
                    operands.clear();
                }
            }
        }
        if form_depth == 0 {
            self.flush_clips_to_depth(0)?;
        }
        Ok(())
    }

    fn record_emitted_id(&mut self, id: ObjectId) {
        if let Some(top) = self.clip_stack.last_mut() {
            top.2.push(id);
        }
    }

    fn flush_clips_to_depth(&mut self, target_gstack_len: usize) -> IoResult<()> {
        while let Some((depth, _, _)) = self.clip_stack.last() {
            if *depth <= target_gstack_len {
                break;
            }
            let (_, clip_path, children) = self.clip_stack.pop().unwrap();
            if children.is_empty() {
                continue;
            }
            self.emitted_count += 1;
            let container = SceneObject::new(
                ObjectId(0),
                format!("PDF Clip {}", self.emitted_count),
                NodePayload::Path(clip_path),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::none());
            let pc_id = self.ctx.doc.add_object(container, Some(self.layer_id))?;
            if let Ok(created_id) = self.ctx.doc.create_powerclip(pc_id, &children) {
                self.record_emitted_id(created_id);
            } else {
                self.record_emitted_id(pc_id);
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
                    let target = self.gstack.len();
                    self.flush_clips_to_depth(target)?;
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
            "f" | "F" => {
                self.emit_path(true, false, false, false)?;
                self.activate_pending_clip();
            }
            "f*" => {
                self.emit_path(true, false, false, true)?;
                self.activate_pending_clip();
            }
            "S" => {
                self.emit_path(false, true, false, false)?;
                self.activate_pending_clip();
            }
            "s" => {
                self.emit_path(false, true, true, false)?;
                self.activate_pending_clip();
            }
            "B" => {
                self.emit_path(true, true, false, false)?;
                self.activate_pending_clip();
            }
            "B*" => {
                self.emit_path(true, true, false, true)?;
                self.activate_pending_clip();
            }
            "b" => {
                self.emit_path(true, true, true, false)?;
                self.activate_pending_clip();
            }
            "b*" => {
                self.emit_path(true, true, true, true)?;
                self.activate_pending_clip();
            }
            "n" => {
                self.path = BezPath::new();
                self.cur_pt = None;
                self.subpath_start = None;
                self.activate_pending_clip();
            }
            "W" => {
                if !self.path.is_empty() {
                    let clip_bez = BezierPath::from_kurbo(&self.path);
                    if !clip_bez.is_empty() {
                        self.pending_clip = Some(clip_bez);
                    }
                }
            }
            "W*" => {
                if !self.path.is_empty() {
                    let mut clip_bez = BezierPath::from_kurbo(&self.path);
                    if !clip_bez.is_empty() {
                        if path_needs_even_odd_normalization(&clip_bez) {
                            if let Some(norm) = normalize_even_odd_path(&clip_bez) {
                                clip_bez = if norm.is_empty() {
                                    BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 0.0, 0.0))
                                } else {
                                    norm
                                };
                            }
                        }
                        self.pending_clip = Some(clip_bez);
                    }
                }
            }
            "sh" if !args.is_empty() => {
                if let Some(sh_name) = args[0].as_name() {
                    self.exec_shading(sh_name)?;
                }
            }

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
                    let (s, adv_1000) = self.decode_font_string_and_width(raw);
                    self.emit_text(&s, Some(adv_1000))?;
                }
            }
            "TJ" if !args.is_empty() => {
                if let PdfObj::Array(ref items) = args[0] {
                    let mut combined = String::new();
                    let mut total_w_1000 = 0.0_f64;
                    for item in items {
                        match item {
                            PdfObj::Str(raw) => {
                                let (s, adv) = self.decode_font_string_and_width(raw);
                                combined.push_str(&s);
                                total_w_1000 += adv;
                            }
                            PdfObj::Int(n) => {
                                total_w_1000 -= *n as f64;
                                if *n < -180 {
                                    combined.push(' ');
                                }
                            }
                            PdfObj::Real(v) => {
                                total_w_1000 -= *v;
                                if *v < -180.0 {
                                    combined.push(' ');
                                }
                            }
                            _ => {}
                        }
                    }
                    self.emit_text(&combined, Some(total_w_1000))?;
                }
            }
            "'" if !args.is_empty() => {
                let dy = -self.gstate.text_leading;
                self.text_line_matrix =
                    self.text_line_matrix.compose(&Affine2D::translate(0.0, dy));
                self.text_matrix = self.text_line_matrix;
                if let PdfObj::Str(ref raw) = args[0] {
                    let (s, adv_1000) = self.decode_font_string_and_width(raw);
                    self.emit_text(&s, Some(adv_1000))?;
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
                    let (s, adv_1000) = self.decode_font_string_and_width(raw);
                    self.emit_text(&s, Some(adv_1000))?;
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
                                    Some(4) => {
                                        self.ctx.saw_cmyk = true;
                                        PdfColorSpaceKind::DeviceCmyk
                                    }
                                    Some(3) => {
                                        self.ctx.saw_rgb = true;
                                        PdfColorSpaceKind::DeviceRgb
                                    }
                                    _ => {
                                        if let Some(alt) = d.get("Alternate").cloned() {
                                            if let Ok(resolved) = self.parser.resolve(&alt, 1) {
                                                return self.classify_colorspace(&resolved);
                                            }
                                        }
                                        PdfColorSpaceKind::DeviceRgb
                                    }
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
                        let (alternate_is_cmyk, alternate_comps) = arr
                            .get(2)
                            .and_then(|o| self.parser.resolve(o, 1).ok())
                            .map(|alt| match self.classify_colorspace(&alt) {
                                PdfColorSpaceKind::DeviceCmyk => (true, 4usize),
                                PdfColorSpaceKind::DeviceGray => (false, 1usize),
                                _ => (false, 3usize),
                            })
                            .unwrap_or((true, 4usize));
                        let tint_fn = arr.get(3).and_then(|o| self.parse_pdf_function(o, 0));
                        PdfColorSpaceKind::Separation {
                            name: spot_name,
                            alternate_is_cmyk,
                            alternate_comps,
                            tint_fn,
                        }
                    }
                    _ => PdfColorSpaceKind::DeviceRgb,
                }
            }
            _ => PdfColorSpaceKind::DeviceRgb,
        }
    }

    fn parse_pdf_function(&mut self, obj: &PdfObj, depth: usize) -> Option<PdfFunction> {
        if depth > 8 {
            return None;
        }
        let resolved = self.parser.resolve(obj, 0).ok()?;
        let (dict, stream_bytes) = match resolved {
            PdfObj::Dict(d) => (d, None),
            PdfObj::Stream { dict, start, len } => {
                let bytes = self.parser.decode_stream_slice(&dict, start, len).ok()?;
                (dict, Some(bytes))
            }
            _ => return None,
        };
        let ftype = dict.get("FunctionType").and_then(PdfObj::as_i64)?;
        let domain = match dict.get("Domain") {
            Some(PdfObj::Array(a)) if a.len() >= 2 => {
                (a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(1.0))
            }
            _ => (0.0, 1.0),
        };
        match ftype {
            2 => {
                let c0 = match dict.get("C0") {
                    Some(PdfObj::Array(a)) => a.iter().filter_map(PdfObj::as_f64).collect(),
                    _ => vec![0.0],
                };
                let c1 = match dict.get("C1") {
                    Some(PdfObj::Array(a)) => a.iter().filter_map(PdfObj::as_f64).collect(),
                    _ => vec![1.0],
                };
                let n = dict.get("N").and_then(PdfObj::as_f64).unwrap_or(1.0);
                Some(PdfFunction::Exponential { c0, c1, n, domain })
            }
            3 => {
                let bounds: Vec<f64> = match dict.get("Bounds") {
                    Some(PdfObj::Array(a)) => a.iter().filter_map(PdfObj::as_f64).collect(),
                    _ => Vec::new(),
                };
                let encode: Vec<(f64, f64)> = match dict.get("Encode") {
                    Some(PdfObj::Array(a)) => a
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|p| (p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(1.0)))
                        .collect(),
                    _ => Vec::new(),
                };
                let mut funcs = Vec::new();
                if let Some(PdfObj::Array(arr)) = dict.get("Functions") {
                    for sub in arr {
                        if let Some(f) = self.parse_pdf_function(sub, depth + 1) {
                            funcs.push(f);
                        }
                    }
                }
                Some(PdfFunction::Stitching {
                    domain,
                    bounds,
                    encode,
                    funcs,
                })
            }
            0 => {
                let samples = stream_bytes?;
                let size = match dict.get("Size") {
                    Some(PdfObj::Array(a)) => {
                        a.first().and_then(PdfObj::as_i64).unwrap_or(0) as usize
                    }
                    _ => 0,
                };
                let bps = dict
                    .get("BitsPerSample")
                    .and_then(PdfObj::as_i64)
                    .unwrap_or(8) as u32;
                let range: Vec<(f64, f64)> = match dict.get("Range") {
                    Some(PdfObj::Array(a)) => a
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|p| (p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(1.0)))
                        .collect(),
                    _ => vec![(0.0, 1.0)],
                };
                Some(PdfFunction::Sampled {
                    domain,
                    range,
                    size,
                    bps,
                    samples,
                })
            }
            _ => None,
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
            PdfColorSpaceKind::Separation {
                name,
                alternate_is_cmyk,
                alternate_comps,
                tint_fn,
            } if !nums.is_empty() => {
                let tint = (nums[0] as f32).clamp(0.0, 1.0);
                let cmyk_fallback = if let Some(tf) = tint_fn {
                    let full = tf.eval(1.0);
                    if *alternate_is_cmyk && full.len() >= 4 {
                        [
                            full[0] as f32,
                            full[1] as f32,
                            full[2] as f32,
                            full[3] as f32,
                        ]
                    } else if *alternate_comps == 3 && full.len() >= 3 {
                        Color::rgb(full[0] as f32, full[1] as f32, full[2] as f32).to_cmyk()
                    } else if *alternate_comps == 1 && !full.is_empty() {
                        Color::gray(full[0] as f32).to_cmyk()
                    } else if name.eq_ignore_ascii_case("CutContour") {
                        [0.0, 1.0, 0.0, 0.0]
                    } else {
                        [0.0, 0.0, 0.0, 1.0]
                    }
                } else if name.eq_ignore_ascii_case("CutContour") {
                    [0.0, 1.0, 0.0, 0.0]
                } else {
                    [0.0, 0.0, 0.0, 1.0]
                };
                Some(Color::spot(name.clone(), tint, cmyk_fallback))
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

    fn emit_path(
        &mut self,
        do_fill: bool,
        do_stroke: bool,
        close: bool,
        even_odd: bool,
    ) -> IoResult<()> {
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

        if even_odd && do_fill && path_needs_even_odd_normalization(&bez) {
            if let Some(norm_bez) = normalize_even_odd_path(&bez) {
                if do_stroke {
                    if !norm_bez.is_empty() {
                        self.emitted_count += 1;
                        let mut fill_obj = SceneObject::new(
                            ObjectId(0),
                            format!("PDF Path {}", self.emitted_count),
                            NodePayload::Path(norm_bez),
                        )
                        .with_fill(fill)
                        .with_outline(OutlineStyle::none());
                        fill_obj.blend_mode = self.gstate.blend_mode;
                        let oid = self.ctx.doc.add_object(fill_obj, Some(self.layer_id))?;
                        self.record_emitted_id(oid);
                    }
                    self.emitted_count += 1;
                    let mut stroke_obj = SceneObject::new(
                        ObjectId(0),
                        format!("PDF Path {}", self.emitted_count),
                        NodePayload::Path(bez),
                    )
                    .with_fill(Fill::None)
                    .with_outline(outline);
                    stroke_obj.blend_mode = self.gstate.blend_mode;
                    let oid = self.ctx.doc.add_object(stroke_obj, Some(self.layer_id))?;
                    self.record_emitted_id(oid);
                    return Ok(());
                }
                if norm_bez.is_empty() {
                    return Ok(());
                }
                self.emitted_count += 1;
                let mut obj = SceneObject::new(
                    ObjectId(0),
                    format!("PDF Path {}", self.emitted_count),
                    NodePayload::Path(norm_bez),
                )
                .with_fill(fill)
                .with_outline(OutlineStyle::none());
                obj.blend_mode = self.gstate.blend_mode;
                let oid = self.ctx.doc.add_object(obj, Some(self.layer_id))?;
                self.record_emitted_id(oid);
                return Ok(());
            }
        }

        self.emitted_count += 1;
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("PDF Path {}", self.emitted_count),
            NodePayload::Path(bez),
        )
        .with_fill(fill)
        .with_outline(outline);
        obj.blend_mode = self.gstate.blend_mode;
        let oid = self.ctx.doc.add_object(obj, Some(self.layer_id))?;
        self.record_emitted_id(oid);
        Ok(())
    }

    fn activate_pending_clip(&mut self) {
        if let Some(clip_path) = self.pending_clip.take() {
            let is_full_page_rect = self.clip_stack.is_empty()
                && clip_path.subpaths.len() == 1
                && clip_path.subpaths[0].nodes.len() <= 5
                && clip_path.bounding_box().is_some_and(|bb| {
                    bb.min_x <= 0.25
                        && bb.min_y <= 0.25
                        && bb.max_x >= self.page_w_mm - 0.25
                        && bb.max_y >= self.page_h_mm - 0.25
                });
            if !is_full_page_rect {
                self.clip_stack
                    .push((self.gstack.len(), clip_path, Vec::new()));
            }
        }
    }

    fn exec_shading(&mut self, name: &str) -> IoResult<()> {
        let Some(sh_table_obj) = self.resources.get("Shading").cloned() else {
            return Ok(());
        };
        let Ok(sh_table) = self.parser.resolve_dict(&sh_table_obj, 0) else {
            return Ok(());
        };
        let Some(sh_ref) = sh_table.get(name).cloned() else {
            return Ok(());
        };
        let Ok(sh_dict) = self.parser.resolve_dict(&sh_ref, 0) else {
            return Ok(());
        };
        let sh_type = sh_dict
            .get("ShadingType")
            .and_then(PdfObj::as_i64)
            .unwrap_or(0);
        if sh_type != 2 && sh_type != 3 {
            return Ok(());
        }
        let cs = match sh_dict.get("ColorSpace").cloned() {
            Some(o) => match self.parser.resolve(&o, 0) {
                Ok(res) => self.classify_colorspace(&res),
                Err(_) => PdfColorSpaceKind::DeviceRgb,
            },
            None => PdfColorSpaceKind::DeviceRgb,
        };
        let Some(fn_obj) = sh_dict.get("Function").cloned() else {
            return Ok(());
        };
        let Some(func) = self.parse_pdf_function(&fn_obj, 0) else {
            return Ok(());
        };
        let (dom_lo, dom_hi) = match sh_dict.get("Domain") {
            Some(PdfObj::Array(a)) if a.len() >= 2 => {
                (a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(1.0))
            }
            _ => (0.0, 1.0),
        };
        let coords: Vec<f64> = match sh_dict.get("Coords") {
            Some(PdfObj::Array(a)) => a.iter().filter_map(PdfObj::as_f64).collect(),
            _ => return Ok(()),
        };
        let n_samples = match &func {
            PdfFunction::Exponential { n, .. } if (*n - 1.0).abs() < 1e-4 => 2usize,
            PdfFunction::Stitching { bounds, .. } => (bounds.len() + 2).clamp(3, 17),
            _ => 9usize,
        };
        let mut stops = Vec::with_capacity(n_samples);
        for i in 0..n_samples {
            let u = (i as f64) / ((n_samples - 1).max(1) as f64);
            let t = dom_lo + u * (dom_hi - dom_lo);
            let comps = func.eval(t);
            let col = self
                .color_from_cs(&cs, &comps)
                .unwrap_or(Color::BLACK)
                .with_alpha(self.gstate.fill_alpha);
            stops.push(GradientStop::new(u as f32, col));
        }

        let xf = self.world_xf();
        let fill = if sh_type == 2 && coords.len() >= 4 {
            let start = xf.transform_point(Point2D::new(coords[0], coords[1]));
            let end = xf.transform_point(Point2D::new(coords[2], coords[3]));
            Fill::LinearGradient { start, end, stops }
        } else if sh_type == 3 && coords.len() >= 6 {
            let focal = xf.transform_point(Point2D::new(coords[0], coords[1]));
            let center = xf.transform_point(Point2D::new(coords[3], coords[4]));
            let scale = effective_affine_scale(&xf);
            let radius = (coords[5].abs() * scale).max(0.1);
            Fill::RadialGradient {
                center,
                radius,
                focal,
                stops,
            }
        } else {
            return Ok(());
        };

        let rect = match sh_dict.get("BBox") {
            Some(PdfObj::Array(a)) if a.len() >= 4 => {
                let x0 = a[0].as_f64().unwrap_or(0.0);
                let y0 = a[1].as_f64().unwrap_or(0.0);
                let x1 = a[2].as_f64().unwrap_or(100.0);
                let y1 = a[3].as_f64().unwrap_or(100.0);
                let p0 = xf.transform_point(Point2D::new(x0, y0));
                let p1 = xf.transform_point(Point2D::new(x1, y1));
                Rect2D::from_xywh(
                    p0.x.min(p1.x),
                    p0.y.min(p1.y),
                    (p1.x - p0.x).abs().max(0.1),
                    (p1.y - p0.y).abs().max(0.1),
                )
            }
            _ => self
                .clip_stack
                .last()
                .and_then(|(_, bp, _)| bp.bounding_box())
                .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, self.page_w_mm, self.page_h_mm)),
        };

        self.emitted_count += 1;
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("PDF Shading {}", self.emitted_count),
            NodePayload::Path(BezierPath::from_rect(rect)),
        )
        .with_fill(fill)
        .with_outline(OutlineStyle::none());
        obj.blend_mode = self.gstate.blend_mode;
        let oid = self.ctx.doc.add_object(obj, Some(self.layer_id))?;
        self.record_emitted_id(oid);
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
        let stream_bytes = self.parser.decode_stream_slice(dict, start, len)?;
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

    fn exec_inline_image(&mut self, dict: &BTreeMap<String, PdfObj>, raw: &[u8]) -> IoResult<()> {
        self.exec_image_dict_bytes("Inline", dict, None, raw)
    }

    fn exec_image_xobject(
        &mut self,
        name: &str,
        dict: &BTreeMap<String, PdfObj>,
        start: usize,
        len: usize,
    ) -> IoResult<()> {
        self.exec_image_dict_bytes(name, dict, Some((start, len)), &[])
    }

    fn exec_image_dict_bytes(
        &mut self,
        name: &str,
        dict: &BTreeMap<String, PdfObj>,
        stream_span: Option<(usize, usize)>,
        inline_raw: &[u8],
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
        let raw_owned: Vec<u8>;
        let raw_slice = if let Some((start, len)) = stream_span {
            let s =
                &self.parser.bytes[start..start.saturating_add(len).min(self.parser.bytes.len())];
            if let Some(obj_ref) = self.parser.stream_obj_refs.get(&start).copied() {
                if let Some(ref crypto) = self.parser.crypto {
                    raw_owned = crypto.decrypt_stream(
                        s,
                        obj_ref.0,
                        obj_ref.1,
                        dict.get("Type").and_then(PdfObj::as_name),
                        &filters,
                        None,
                    );
                    &raw_owned[..]
                } else {
                    s
                }
            } else {
                s
            }
        } else {
            inline_raw
        };

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
            // Uncompressed, FlateDecode, LZWDecode, CCITTFaxDecode, ASCII85, ASCIIHex, RunLength
            let supported = filters.iter().all(|f| {
                matches!(
                    f.as_str(),
                    "Crypt"
                        | "FlateDecode"
                        | "Fl"
                        | "LZWDecode"
                        | "LZW"
                        | "CCITTFaxDecode"
                        | "CCF"
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
                    if let Ok(sm_bytes) =
                        self.parser.decode_stream_slice(&sm_dict, sm_start, sm_len)
                    {
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
                filter_enabled: Vec::new(),
                crop_rect: None,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none())
        .with_transform(obj_xf);
        obj.opacity = self.gstate.fill_alpha;
        obj.blend_mode = self.gstate.blend_mode;
        let oid = self.ctx.doc.add_object(obj, Some(self.layer_id))?;
        self.record_emitted_id(oid);
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
                    PdfColorSpaceKind::DeviceGray | PdfColorSpaceKind::Separation { .. } => 1usize,
                    PdfColorSpaceKind::DeviceCmyk => 4usize,
                    PdfColorSpaceKind::DeviceRgb => 3usize,
                };
                let lookup_bytes = match self.parser.resolve(&arr[3], 0)? {
                    PdfObj::Str(b) => b,
                    PdfObj::Stream {
                        dict: ld,
                        start,
                        len,
                    } => self.parser.decode_stream_slice(&ld, start, len)?,
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
            PdfColorSpaceKind::DeviceGray | PdfColorSpaceKind::Separation { .. } => 1usize,
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
                        if let Some(fc) = fdict.get("FirstChar").and_then(PdfObj::as_i64) {
                            info.first_char = fc.clamp(0, 255) as u16;
                        }
                        if let Some(widths_obj) = fdict.get("Widths").cloned() {
                            if let Ok(PdfObj::Array(arr)) = self.parser.resolve(&widths_obj, 0) {
                                info.widths = arr
                                    .iter()
                                    .take(512)
                                    .map(|o| o.as_f64().unwrap_or(500.0).clamp(0.0, 4000.0))
                                    .collect();
                            }
                        }
                        if let Some(desc_obj) = fdict.get("DescendantFonts").cloned() {
                            if let Ok(PdfObj::Array(desc_arr)) = self.parser.resolve(&desc_obj, 0) {
                                if let Some(first_desc) = desc_arr.first() {
                                    if let Ok(cid_dict) = self.parser.resolve_dict(first_desc, 0) {
                                        if let Some(dw) =
                                            cid_dict.get("DW").and_then(PdfObj::as_f64)
                                        {
                                            info.default_cid_width = dw.clamp(0.0, 4000.0);
                                        }
                                        if let Some(w_obj) = cid_dict.get("W").cloned() {
                                            if let Ok(PdfObj::Array(w_arr)) =
                                                self.parser.resolve(&w_obj, 0)
                                            {
                                                parse_cid_widths_array(
                                                    &w_arr,
                                                    &mut info.cid_widths,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(enc_obj) = fdict.get("Encoding").cloned() {
                            if let Ok(enc_dict) = self.parser.resolve_dict(&enc_obj, 0) {
                                if let Some(PdfObj::Array(diff_arr)) = enc_dict.get("Differences") {
                                    let mut code: u16 = 0;
                                    for item in diff_arr {
                                        match item {
                                            PdfObj::Int(n) => {
                                                code = (*n).clamp(0, 255) as u16;
                                            }
                                            PdfObj::Name(gname) => {
                                                if code <= 255 {
                                                    if let Some(ch) = agl_name_to_char(gname) {
                                                        info.differences.insert(code as u8, ch);
                                                    }
                                                }
                                                code = code.saturating_add(1);
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
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

    fn decode_font_string_and_width(&self, raw: &[u8]) -> (String, f64) {
        if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
            let s = decode_utf16_be(&raw[2..]);
            let adv = (s.chars().count() as f64) * 550.0;
            return (s, adv);
        }
        if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
            let s = String::from_utf8_lossy(&raw[3..]).into_owned();
            let adv = (s.chars().count() as f64) * 550.0;
            return (s, adv);
        }
        if let Some(font) = self.font_cache.get(&self.gstate.font_key) {
            let mut out = String::new();
            let mut width_sum = 0.0_f64;
            if font.is_two_byte && raw.len().is_multiple_of(2) {
                for pair in raw.as_chunks::<2>().0 {
                    let code = u16::from_be_bytes([pair[0], pair[1]]);
                    width_sum += font
                        .cid_widths
                        .get(&code)
                        .copied()
                        .unwrap_or(font.default_cid_width);
                    if let Some(mapped) = font.to_unicode.get(&code) {
                        out.push_str(mapped);
                    } else if let Some(ch) = char::from_u32(u32::from(code)) {
                        out.push(ch);
                    }
                }
                return (out, width_sum);
            }
            let has_tu = !font.to_unicode.is_empty();
            for &b in raw {
                let b_u16 = u16::from(b);
                let w = b_u16
                    .checked_sub(font.first_char)
                    .and_then(|idx| font.widths.get(usize::from(idx)).copied())
                    .unwrap_or(550.0);
                width_sum += w;
                if has_tu {
                    if let Some(mapped) = font.to_unicode.get(&b_u16) {
                        out.push_str(mapped);
                    } else if let Some(&ch) = font.differences.get(&b) {
                        out.push(ch);
                    } else {
                        out.push(winansi_byte_to_char(b));
                    }
                } else if let Some(&ch) = font.differences.get(&b) {
                    out.push(ch);
                } else {
                    out.push(winansi_byte_to_char(b));
                }
            }
            return (out, width_sum);
        }
        let s = decode_pdf_text_string(raw);
        let adv = (s.chars().count() as f64) * 550.0;
        (s, adv)
    }

    fn emit_text(&mut self, text: &str, total_w_1000: Option<f64>) -> IoResult<()> {
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
        let oid = self.ctx.doc.add_object(obj, Some(self.layer_id))?;
        self.record_emitted_id(oid);

        let w_1000 = total_w_1000.unwrap_or_else(|| (text.chars().count() as f64) * 550.0);
        let adv_pt = (w_1000 * font_pt / 1000.0) * (self.gstate.horiz_scale / 100.0);
        self.text_matrix = self.text_matrix.compose(&Affine2D::translate(adv_pt, 0.0));
        Ok(())
    }
}

fn decrypt_pdf_obj_strings(crypto: &PdfCryptoCtx, obj: &mut PdfObj, obj_num: u32, gen_num: u16) {
    if crypto.encrypt_obj_num == Some(obj_num) {
        return;
    }
    match obj {
        PdfObj::Str(bytes) => {
            *bytes = crypto.decrypt_string(bytes, obj_num, gen_num);
        }
        PdfObj::Array(items) => {
            for item in items {
                decrypt_pdf_obj_strings(crypto, item, obj_num, gen_num);
            }
        }
        PdfObj::Dict(dict) | PdfObj::Stream { dict, .. } => {
            for val in dict.values_mut() {
                decrypt_pdf_obj_strings(crypto, val, obj_num, gen_num);
            }
        }
        _ => {}
    }
}

fn parse_cid_widths_array(arr: &[PdfObj], out: &mut HashMap<u16, f64>) {
    let mut i = 0usize;
    while i < arr.len() {
        let Some(c_first) = arr[i].as_i64() else {
            i += 1;
            continue;
        };
        if i + 1 >= arr.len() {
            break;
        }
        match &arr[i + 1] {
            PdfObj::Array(widths) => {
                for (off, w_obj) in widths.iter().enumerate() {
                    if let Some(w) = w_obj.as_f64() {
                        let cid = c_first.saturating_add(off as i64);
                        if (0..=i64::from(u16::MAX)).contains(&cid) {
                            out.insert(cid as u16, w.clamp(0.0, 4000.0));
                        }
                    }
                }
                i += 2;
            }
            other => {
                if i + 2 < arr.len() {
                    if let (Some(c_last), Some(w)) = (other.as_i64(), arr[i + 2].as_f64()) {
                        let lo = c_first.clamp(0, i64::from(u16::MAX)) as u16;
                        let hi = c_last.clamp(0, i64::from(u16::MAX)) as u16;
                        let count = hi.saturating_sub(lo).min(4096);
                        for off in 0..=count {
                            out.insert(lo + off, w.clamp(0.0, 4000.0));
                        }
                        i += 3;
                        continue;
                    }
                }
                i += 1;
            }
        }
    }
}

fn agl_name_to_char(name: &str) -> Option<char> {
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() == 4 {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp);
            }
        }
    }
    if name.len() == 1 {
        return name.chars().next();
    }
    Some(match name {
        "space" => ' ',
        "exclam" => '!',
        "quotedbl" => '"',
        "numbersign" => '#',
        "dollar" => '$',
        "percent" => '%',
        "ampersand" => '&',
        "quotesingle" => '\'',
        "parenleft" => '(',
        "parenright" => ')',
        "asterisk" => '*',
        "plus" => '+',
        "comma" => ',',
        "hyphen" => '-',
        "period" => '.',
        "slash" => '/',
        "zero" => '0',
        "one" => '1',
        "two" => '2',
        "three" => '3',
        "four" => '4',
        "five" => '5',
        "six" => '6',
        "seven" => '7',
        "eight" => '8',
        "nine" => '9',
        "colon" => ':',
        "semicolon" => ';',
        "less" => '<',
        "equal" => '=',
        "greater" => '>',
        "question" => '?',
        "at" => '@',
        "bullet" => '\u{2022}',
        "endash" => '\u{2013}',
        "emdash" => '\u{2014}',
        "ellipsis" => '\u{2026}',
        "Euro" => '\u{20AC}',
        "trademark" => '\u{2122}',
        "copyright" => '\u{00A9}',
        "registered" => '\u{00AE}',
        "degree" => '\u{00B0}',
        "plusminus" => '\u{00B1}',
        "multiply" => '\u{00D7}',
        "divide" => '\u{00F7}',
        "fi" => '\u{FB01}',
        "fl" => '\u{FB02}',
        "quoteleft" => '\u{2018}',
        "quoteright" => '\u{2019}',
        "quotedblleft" => '\u{201C}',
        "quotedblright" => '\u{201D}',
        _ => return None,
    })
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
                // Parse inline image payloads (`BI ... ID <binary> EI`)
                if word == "BI" {
                    if let Some(tok) = parse_inline_image(data, &mut pos) {
                        tokens.push(tok);
                    }
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

fn parse_inline_image(data: &[u8], pos: &mut usize) -> Option<ContentToken> {
    let len = data.len();
    let mut dict: BTreeMap<String, PdfObj> = BTreeMap::new();
    let mut pending_key: Option<String> = None;

    while *pos < len {
        skip_pdf_ws(data, pos);
        if *pos >= len {
            return None;
        }
        if *pos + 2 <= len
            && &data[*pos..*pos + 2] == b"ID"
            && data.get(*pos + 2).is_none_or(|&b| is_pdf_ws(b))
        {
            *pos += 2;
            if *pos < len && is_pdf_ws(data[*pos]) {
                // Single whitespace byte separates `ID` from image bytes (or CRLF -> advance 2 if CR+LF)
                if data[*pos] == b'\r' && data.get(*pos + 1) == Some(&b'\n') {
                    *pos += 2;
                } else {
                    *pos += 1;
                }
            }
            break;
        }
        let val = match data[*pos] {
            b'/' | b'[' | b'<' | b'(' => {
                let before = *pos;
                let parsed = parse_obj_from_slice(data, pos, 0).ok()?;
                if *pos == before {
                    *pos += 1;
                }
                parsed
            }
            _ => {
                let w = read_pdf_word(data, pos);
                if w.is_empty() {
                    *pos += 1;
                    continue;
                }
                if w == "true" {
                    PdfObj::Bool(true)
                } else if w == "false" {
                    PdfObj::Bool(false)
                } else if let Ok(iv) = w.parse::<i64>() {
                    PdfObj::Int(iv)
                } else if let Ok(rv) = w.parse::<f64>() {
                    PdfObj::Real(rv)
                } else {
                    PdfObj::Name(w)
                }
            }
        };
        if let Some(key_raw) = pending_key.take() {
            let full_key = expand_inline_key(&key_raw);
            let full_val = expand_inline_val(&full_key, val);
            dict.insert(full_key, full_val);
        } else if let PdfObj::Name(k) = val {
            pending_key = Some(k);
        }
    }

    let img_start = *pos;
    // Scan for EI delimited by whitespace
    let mut img_end = len;
    while *pos + 2 <= len {
        if &data[*pos..*pos + 2] == b"EI"
            && (*pos == img_start || is_pdf_ws(data[*pos - 1]))
            && data
                .get(*pos + 2)
                .is_none_or(|&b| is_pdf_ws(b) || is_pdf_delim(b))
        {
            img_end = if *pos > img_start && is_pdf_ws(data[*pos - 1]) {
                *pos - 1
            } else {
                *pos
            };
            *pos += 2;
            break;
        }
        *pos += 1;
    }
    let raw = data[img_start..img_end].to_vec();
    Some(ContentToken::InlineImage { dict, raw })
}

fn expand_inline_key(k: &str) -> String {
    match k {
        "BPC" => "BitsPerComponent",
        "CS" => "ColorSpace",
        "D" => "Decode",
        "DP" => "DecodeParms",
        "F" => "Filter",
        "H" => "Height",
        "IM" => "ImageMask",
        "I" => "Interpolate",
        "W" => "Width",
        other => other,
    }
    .to_owned()
}

fn expand_inline_val(key: &str, val: PdfObj) -> PdfObj {
    match (key, val) {
        ("ColorSpace", PdfObj::Name(n)) => PdfObj::Name(
            match n.as_str() {
                "G" => "DeviceGray",
                "RGB" => "DeviceRGB",
                "CMYK" => "DeviceCMYK",
                "I" => "Indexed",
                other => other,
            }
            .to_owned(),
        ),
        ("Filter", PdfObj::Name(n)) => PdfObj::Name(expand_inline_filter_name(&n)),
        ("Filter", PdfObj::Array(arr)) => PdfObj::Array(
            arr.into_iter()
                .map(|item| match item {
                    PdfObj::Name(n) => PdfObj::Name(expand_inline_filter_name(&n)),
                    other => other,
                })
                .collect(),
        ),
        (_, other) => other,
    }
}

fn expand_inline_filter_name(n: &str) -> String {
    match n {
        "AHx" => "ASCIIHexDecode",
        "A85" => "ASCII85Decode",
        "LZW" => "LZWDecode",
        "Fl" => "FlateDecode",
        "RL" => "RunLengthDecode",
        "DCT" => "DCTDecode",
        "CCF" => "CCITTFaxDecode",
        other => other,
    }
    .to_owned()
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

fn segments_properly_intersect(a1: Point2D, a2: Point2D, b1: Point2D, b2: Point2D) -> bool {
    if a1.x.max(a2.x) < b1.x.min(b2.x)
        || b1.x.max(b2.x) < a1.x.min(a2.x)
        || a1.y.max(a2.y) < b1.y.min(b2.y)
        || b1.y.max(b2.y) < a1.y.min(a2.y)
    {
        return false;
    }
    let cross =
        |o: Point2D, p: Point2D, q: Point2D| (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x);
    let d1 = cross(a1, a2, b1);
    let d2 = cross(a1, a2, b2);
    let d3 = cross(b1, b2, a1);
    let d4 = cross(b1, b2, a2);
    ((d1 > 1e-9 && d2 < -1e-9) || (d1 < -1e-9 && d2 > 1e-9))
        && ((d3 > 1e-9 && d4 < -1e-9) || (d3 < -1e-9 && d4 > 1e-9))
}

fn path_needs_even_odd_normalization(bez: &BezierPath) -> bool {
    if bez.subpaths.len() > 1 {
        return true;
    }
    if bez.subpaths.len() == 1 {
        let polys = bez.flatten(0.1);
        if let Some(pts) = polys.first() {
            let mut ring: Vec<Point2D> = pts.clone();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            let n = ring.len();
            if (4..=512).contains(&n) {
                for i in 0..n {
                    let a1 = ring[i];
                    let a2 = ring[(i + 1) % n];
                    for j in (i + 2)..n {
                        if i == 0 && j == n - 1 {
                            continue;
                        }
                        let b1 = ring[j];
                        let b2 = ring[(j + 1) % n];
                        if segments_properly_intersect(a1, a2, b1, b2) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

fn normalize_even_odd_path(bez: &BezierPath) -> Option<BezierPath> {
    let welded =
        coree_core::apply_boolean_many(coree_core::BooleanOp::Weld, std::slice::from_ref(bez))
            .ok()?;
    let mut subpaths = Vec::new();
    for p in welded {
        subpaths.extend(p.subpaths);
    }
    Some(BezierPath { subpaths })
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
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(60.0, 10.0)),
            None,
        )
        .unwrap();

        // Second page with curved shape
        let p2 = doc.add_page("Page 2", 150.0, 100.0, 0.0);
        let p2_layer = doc.page_by_id(p2).unwrap().layers[0].id;
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

        assert_eq!(imported.pages().len(), 2);
        assert!((imported.pages()[0].width_mm - 100.0).abs() < 0.1);
        assert!((imported.pages()[0].height_mm - 80.0).abs() < 0.1);
        assert!((imported.pages()[1].width_mm - 150.0).abs() < 0.1);
        assert!((imported.pages()[1].height_mm - 100.0).abs() < 0.1);

        // Verify page 1 recovered vector path, text, and raster image
        let p1_objs: Vec<&SceneObject> = imported.pages()[0].layers[0]
            .objects
            .iter()
            .filter_map(|id| imported.object(*id))
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
        assert!(!imported.assets().is_empty());

        // Verify page 2 has the curved badge
        assert!(!imported.pages()[1].layers[0].objects.is_empty());
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
        assert_eq!(doc.pages().len(), 1);
        // MediaBox was 200x100 pt, Rotate 90 -> swapped to 100x200 pt!
        assert!((doc.pages()[0].width_mm - 100.0 * PT_TO_MM).abs() < 0.1);
        assert!((doc.pages()[0].height_mm - 200.0 * PT_TO_MM).abs() < 0.1);
        assert_eq!(doc.object_count(), 2); // Form XObject rect + UTF-16BE "OK" text
        assert!(doc.objects().values().any(|o| matches!(
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
        assert_eq!(doc.pages().len(), 1);
        assert!(doc.assets().is_empty());

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

    #[test]
    fn pdf_import_clipping_shadings_spot_tint_font_widths_lzw_ccitt_and_inline_image() {
        // Compress an RGB 2x2 image via LZW (EarlyChange=1, MSB)
        let mut lzw_enc = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
        let lzw_rgb = lzw_enc
            .encode(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0])
            .unwrap();

        // Compress a 2x2 monochrome CCITT Group 4 image (row0: Black,White; row1: White,Black)
        let mut g4_enc = fax::encoder::Encoder::new(fax::VecWriter::new());
        g4_enc
            .encode_line([fax::Color::Black, fax::Color::White].into_iter(), 2)
            .unwrap();
        g4_enc
            .encode_line([fax::Color::White, fax::Color::Black].into_iter(), 2)
            .unwrap();
        let g4_bytes = g4_enc.finish().unwrap().finish();

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offsets = Vec::new();
        let push_obj = |buf: &mut Vec<u8>, offs: &mut Vec<usize>, id: usize, body: &[u8]| {
            offs.push(buf.len());
            buf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            buf.extend_from_slice(body);
            buf.extend_from_slice(b"\nendobj\n");
        };

        push_obj(
            &mut pdf,
            &mut offsets,
            1,
            b"<< /Type /Catalog /Pages 2 0 R >>",
        );
        push_obj(
            &mut pdf,
            &mut offsets,
            2,
            b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        );
        push_obj(
            &mut pdf,
            &mut offsets,
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
               /Resources << \
                 /ColorSpace << /CS1 [/Separation /PantoneRed /DeviceCMYK 6 0 R] >> \
                 /Shading << /Sh1 << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Function 7 0 R >> >> \
                 /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 /Widths [600 650] /Encoding << /Differences [65 /Euro] >> >> >> \
                 /XObject << /ImLzw 4 0 R /ImFax 5 0 R >> \
               >> /Contents 8 0 R >>",
        );
        let mut im4 = format!(
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /LZWDecode /Length {} >>\nstream\n",
            lzw_rgb.len()
        )
        .into_bytes();
        im4.extend_from_slice(&lzw_rgb);
        im4.extend_from_slice(b"\nendstream");
        push_obj(&mut pdf, &mut offsets, 4, &im4);

        let mut im5 = format!(
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 2 /Rows 2 /BlackIs1 false >> /Length {} >>\nstream\n",
            g4_bytes.len()
        )
        .into_bytes();
        im5.extend_from_slice(&g4_bytes);
        im5.extend_from_slice(b"\nendstream");
        push_obj(&mut pdf, &mut offsets, 5, &im5);

        push_obj(
            &mut pdf,
            &mut offsets,
            6,
            b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0.0 0.9 0.85 0.05] /N 1 >>",
        );
        push_obj(
            &mut pdf,
            &mut offsets,
            7,
            b"<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>",
        );

        let content = b"\
            q 10 10 80 80 re W n /Sh1 sh Q\n\
            /CS1 cs 1 scn 10 100 30 30 re f\n\
            q 20 0 0 20 100 100 cm /ImLzw Do Q\n\
            q 20 0 0 20 130 100 cm /ImFax Do Q\n\
            q 10 0 0 10 100 140 cm BI /W 1 /H 1 /CS /RGB /BPC 8 ID \x11\x22\x33 EI Q\n\
            BT /F1 12 Tf 10 160 Td (AB) Tj ET\n";
        let mut s8 = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        s8.extend_from_slice(content);
        s8.extend_from_slice(b"\nendstream");
        push_obj(&mut pdf, &mut offsets, 8, &s8);

        let xref_off = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
        );
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n",
                offsets.len() + 1
            )
            .as_bytes(),
        );

        let doc = import_pdf_bytes(&pdf, "Req04 Features").unwrap();
        assert_eq!(doc.pages().len(), 1);
        assert!(
            doc.assets().len() >= 3,
            "expected LZW, CCITT G4, and inline BI images, got {}",
            doc.assets().len()
        );
        let has_powerclip = doc
            .objects()
            .values()
            .any(|o| matches!(o.payload, NodePayload::PowerClip { .. }));
        assert!(has_powerclip, "expected W/W* clipping PowerClip node");
        let has_gradient = doc
            .objects()
            .values()
            .any(|o| matches!(o.fill, Fill::LinearGradient { .. }));
        assert!(has_gradient, "expected Type 2 sh LinearGradient");
        let has_spot = doc.objects().values().any(|o| matches!(&o.fill, Fill::Solid(Color::Spot { name, fallback_cmyk, .. }) if name == "PantoneRed" && (fallback_cmyk[1] - 0.9).abs() < 0.02));
        assert!(
            has_spot,
            "expected /Separation /PantoneRed with TintTransform CMYK fallback"
        );
        let text_str: String = doc
            .objects()
            .values()
            .filter_map(|o| match &o.payload {
                NodePayload::ArtisticText(at) => Some(at.plain_text()),
                _ => None,
            })
            .collect();
        assert!(
            text_str.contains('\u{20AC}') && text_str.contains('B'),
            "expected /Differences /Euro + B, got {text_str:?}"
        );
    }

    #[test]
    fn pdf_import_decrypts_rc4_and_aes128_and_rejects_wrong_password() {
        use crate::pdf_crypto::{aes_128_cbc_encrypt_no_pad, md5, rc4_crypt};

        // Build an encrypted PDF 1.6 (R=4, V=4, AESV2 128-bit) with user password "secret"
        const PAD: [u8; 32] = [
            0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA,
            0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE,
            0x64, 0x53, 0x69, 0x7A,
        ];
        let id0 = b"0123456789ABCDEF";
        let p_val: i32 = -4;
        let pw = b"secret";
        let mut pw32 = PAD;
        pw32[..pw.len()].copy_from_slice(pw);
        pw32[pw.len()..].copy_from_slice(&PAD[..32 - pw.len()]);

        // Owner O = padded owner hashed 50 times, encrypting pw32
        let mut ok = md5(&pw32);
        for _ in 0..50 {
            ok = md5(&ok);
        }
        let mut o_entry = pw32.to_vec();
        for i in 0u8..=19 {
            let mut ik = ok;
            for b in &mut ik {
                *b ^= i;
            }
            o_entry = rc4_crypt(&ik, &o_entry);
        }

        // File key
        let mut fk_in = Vec::new();
        fk_in.extend_from_slice(&pw32);
        fk_in.extend_from_slice(&o_entry);
        fk_in.extend_from_slice(&p_val.to_le_bytes());
        fk_in.extend_from_slice(id0);
        let mut fk = md5(&fk_in);
        for _ in 0..50 {
            fk = md5(&fk);
        }

        // User U entry
        let mut h_in = Vec::from(PAD);
        h_in.extend_from_slice(id0);
        let h = md5(&h_in);
        let mut u_entry = rc4_crypt(&fk, &h);
        for i in 1u8..=19 {
            let mut ik = fk;
            for b in &mut ik {
                *b ^= i;
            }
            u_entry = rc4_crypt(&ik, &u_entry);
        }
        u_entry.resize(32, 0);

        let enc_obj = |plain: &[u8], obj_num: u32, gen_num: u16| -> Vec<u8> {
            let mut k_in = Vec::new();
            k_in.extend_from_slice(&fk);
            k_in.extend_from_slice(&obj_num.to_le_bytes()[..3]);
            k_in.extend_from_slice(&gen_num.to_le_bytes()[..2]);
            k_in.extend_from_slice(b"sAlT");
            let obj_key = md5(&k_in);
            let iv = [0xA5u8; 16];
            let pad_len = 16 - (plain.len() % 16);
            let mut padded = plain.to_vec();
            padded.resize(plain.len() + pad_len, pad_len as u8);
            let mut out = iv.to_vec();
            out.extend_from_slice(&aes_128_cbc_encrypt_no_pad(&obj_key, &iv, &padded));
            out
        };

        let to_hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02X}")).collect() };

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.6\n");
        let mut offsets = Vec::new();
        let push_obj = |buf: &mut Vec<u8>, offs: &mut Vec<usize>, id: usize, body: &[u8]| {
            offs.push(buf.len());
            buf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            buf.extend_from_slice(body);
            buf.extend_from_slice(b"\nendobj\n");
        };

        push_obj(
            &mut pdf,
            &mut offsets,
            1,
            b"<< /Type /Catalog /Pages 2 0 R >>",
        );
        push_obj(
            &mut pdf,
            &mut offsets,
            2,
            b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        );
        push_obj(
            &mut pdf,
            &mut offsets,
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
        );
        let enc_stm = enc_obj(b"0 1 0 rg 10 10 40 40 re f\n", 4, 0);
        let mut s4 = format!("<< /Length {} >>\nstream\n", enc_stm.len()).into_bytes();
        s4.extend_from_slice(&enc_stm);
        s4.extend_from_slice(b"\nendstream");
        push_obj(&mut pdf, &mut offsets, 4, &s4);

        let enc_title = enc_obj(b"Secret Title", 5, 0);
        push_obj(
            &mut pdf,
            &mut offsets,
            5,
            format!("<< /Title <{}> >>", to_hex(&enc_title)).as_bytes(),
        );
        push_obj(
            &mut pdf,
            &mut offsets,
            6,
            format!(
                "<< /Filter /Standard /V 4 /R 4 /Length 128 /P {p_val} /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StrF /StdCF /StmF /StdCF /O <{}> /U <{}> >>",
                to_hex(&o_entry),
                to_hex(&u_entry)
            )
            .as_bytes(),
        );

        let xref_off = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
        );
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R /Encrypt 6 0 R /ID [<{}> <{}>] >>\nstartxref\n{xref_off}\n%%EOF\n",
                offsets.len() + 1,
                to_hex(id0),
                to_hex(id0)
            )
            .as_bytes(),
        );

        assert!(import_pdf_bytes_with_password(&pdf, "Fail", "wrong").is_err());
        let doc = import_pdf_bytes_with_password(&pdf, "Fallback", "secret").unwrap();
        assert_eq!(doc.title(), "Secret Title");
        assert_eq!(doc.objects().len(), 1);
    }

    fn build_minimal_pdf(
        media_box: [i32; 4],
        content: &str,
        extra_resources: Option<&str>,
        extra_objs: &[&str],
    ) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();
        let push_obj = |buf: &mut Vec<u8>, offs: &mut Vec<usize>, id: usize, body: &[u8]| {
            offs.push(buf.len());
            buf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            buf.extend_from_slice(body);
            buf.extend_from_slice(b"\nendobj\n");
        };
        push_obj(&mut pdf, &mut offs, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push_obj(
            &mut pdf,
            &mut offs,
            2,
            b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        );
        let res_str = extra_resources.unwrap_or("<< >>");
        push_obj(
            &mut pdf,
            &mut offs,
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [{} {} {} {}] /Resources {res_str} /Contents 4 0 R >>",
                media_box[0], media_box[1], media_box[2], media_box[3]
            )
            .as_bytes(),
        );
        let stm = format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        );
        push_obj(&mut pdf, &mut offs, 4, stm.as_bytes());
        for (i, body) in extra_objs.iter().enumerate() {
            push_obj(&mut pdf, &mut offs, 5 + i, body.as_bytes());
        }
        let xref_off = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offs.len() + 1).as_bytes(),
        );
        for off in &offs {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n",
                offs.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    fn pixel_rgba(buf: &coree_render::RenderedImage, x: u32, y: u32) -> [u8; 4] {
        let idx = ((y * buf.width + x) * 4) as usize;
        [
            buf.rgba[idx],
            buf.rgba[idx + 1],
            buf.rgba[idx + 2],
            buf.rgba[idx + 3],
        ]
    }

    #[test]
    fn pdf_import_even_odd_clip_w_star_punches_center_hole_on_shading() {
        let sh = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [10 50 90 50] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >>";
        let pdf = build_minimal_pdf(
            [0, 0, 100, 100],
            "q 10 10 80 80 re 30 30 40 40 re W* n /Sh1 sh Q",
            Some("<< /Shading << /Sh1 5 0 R >> >>"),
            &[sh],
        );
        let doc = import_pdf_bytes(&pdf, "WStarClip").unwrap();
        let rendered =
            coree_render::render_page_to_rgba(&doc, 0, 72.0, coree_core::ViewMode::Enhanced)
                .unwrap();
        assert_eq!(rendered.width, 100);
        assert_eq!(rendered.height, 100);

        // Center (50, 50) lies inside the inner 30..70 hole -> must be white
        assert_eq!(pixel_rgba(&rendered, 50, 50), [255, 255, 255, 255]);
        // Outer ring (20, 50) lies inside 10..90 outside 30..70 -> must be gradient colored (red-ish)
        let ring = pixel_rgba(&rendered, 20, 50);
        assert!(
            ring[0] > 150 && ring[1] < 20,
            "expected red-dominant gradient in outer ring, got {ring:?}"
        );
    }

    #[test]
    fn pdf_import_even_odd_fill_f_star_and_b_star_punch_center_hole_and_preserve_strokes() {
        let pdf_f = build_minimal_pdf(
            [0, 0, 100, 100],
            "1 0 0 rg 10 10 80 80 re 30 30 40 40 re f*",
            None,
            &[],
        );
        let doc_f = import_pdf_bytes(&pdf_f, "FStarFill").unwrap();
        let ren_f =
            coree_render::render_page_to_rgba(&doc_f, 0, 72.0, coree_core::ViewMode::Enhanced)
                .unwrap();
        assert_eq!(pixel_rgba(&ren_f, 50, 50), [255, 255, 255, 255]);
        assert_eq!(pixel_rgba(&ren_f, 20, 50), [255, 0, 0, 255]);

        // B* must punch center hole in fill AND stroke both outer and inner rectangle edges
        let pdf_b = build_minimal_pdf(
            [0, 0, 100, 100],
            "1 0 0 rg 0 0 1 RG 2 w 10 10 80 80 re 30 30 40 40 re B*",
            None,
            &[],
        );
        let doc_b = import_pdf_bytes(&pdf_b, "BStarFillStroke").unwrap();
        let ren_b =
            coree_render::render_page_to_rgba(&doc_b, 0, 72.0, coree_core::ViewMode::Enhanced)
                .unwrap();
        assert_eq!(pixel_rgba(&ren_b, 50, 50), [255, 255, 255, 255]);
        assert_eq!(pixel_rgba(&ren_b, 20, 50), [255, 0, 0, 255]);
        // Inner hole boundary (x=30, y=50) must be blue stroked
        let inner_edge = pixel_rgba(&ren_b, 30, 50);
        assert!(
            inner_edge[2] > 200 && inner_edge[0] < 30,
            "expected blue inner stroke at (30,50), got {inner_edge:?}"
        );

        // Winding `f` (non-*) on same-winding concentric rects must keep center filled
        let pdf_w = build_minimal_pdf(
            [0, 0, 100, 100],
            "1 0 0 rg 10 10 80 80 re 30 30 40 40 re f",
            None,
            &[],
        );
        let doc_w = import_pdf_bytes(&pdf_w, "WindingFill").unwrap();
        let ren_w =
            coree_render::render_page_to_rgba(&doc_w, 0, 72.0, coree_core::ViewMode::Enhanced)
                .unwrap();
        assert_eq!(pixel_rgba(&ren_w, 50, 50), [255, 0, 0, 255]);
    }

    #[test]
    fn w5a_ascii85_partial_tuple_overflow_returns_error() {
        // `uuuu` padded with `'u'` (84) -> `84 * (85^4 + ... + 1)` > u32::MAX (0xFFFF_FFFF)
        let err = decode_ascii85(b"uuuu~>", 1024).unwrap_err();
        assert!(
            matches!(err, IoError::InvalidFormat { .. }),
            "expected InvalidFormat on partial ASCII85 overflow, got {err:?}"
        );
    }
}
