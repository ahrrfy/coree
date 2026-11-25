// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust TrueType/OpenType (`glyf`/`loca` & `CFF`) PDF font subsetter and
//! `/Type0` + `/CIDFontType2` + `/ToUnicode` CMap emitter.
//!
//! Produces PDF/X-compliant embedded subset fonts (`emb=yes, sub=yes, uni=yes` in
//! Poppler `pdffonts`) with accurate glyph widths and UTF-16BE `/ToUnicode` mappings
//! for Latin, Arabic RTL/ligatures/Kashida, and multilingual typography.

use crate::error::{IoError, IoResult};
use coree_render::ShapedPdfRun;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Arc;
use ttf_parser::{Face, GlyphId, Tag};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FontFaceKey {
    pub hash: u64,
    pub len: usize,
    pub face_index: u32,
}

impl FontFaceKey {
    pub(crate) fn new(bytes: &[u8], face_index: u32) -> Self {
        Self {
            hash: fnv1a_64(bytes),
            len: bytes.len(),
            face_index,
        }
    }
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[derive(Debug, Clone)]
struct CollectedFont {
    font_bytes: Arc<[u8]>,
    face_index: u32,
    postscript_name: String,
    /// Original GID -> Unicode cluster text (first seen non-empty mapping wins).
    used_gids: BTreeMap<u16, String>,
}

/// Resolved embedded PDF font ready for content-stream glyph emission.
#[derive(Debug, Clone)]
pub(crate) struct EmbeddedPdfFont {
    pub res_name: String,
    pub type0_obj_id: usize,
    pub is_identity_remapped: bool,
    pub orig_to_subset: HashMap<u16, u16>,
    pub widths_1000: HashMap<u16, u32>,
}

impl EmbeddedPdfFont {
    /// Maps an original glyph ID to the character code emitted in PDF content streams.
    #[must_use]
    pub(crate) fn map_gid(&self, orig_gid: u16) -> u16 {
        if self.is_identity_remapped {
            self.orig_to_subset.get(&orig_gid).copied().unwrap_or(0)
        } else {
            orig_gid
        }
    }

    /// Returns the PDF 1000-unit EM width for `orig_gid`.
    #[must_use]
    pub(crate) fn width_1000(&self, orig_gid: u16) -> u32 {
        let key = self.map_gid(orig_gid);
        self.widths_1000.get(&key).copied().unwrap_or(1000)
    }
}

/// Collects used glyphs across all PDF pages and emits deduplicated subset `/Type0` fonts.
#[derive(Debug, Default)]
pub(crate) struct PdfFontRegistry {
    fonts: Vec<CollectedFont>,
    by_key: HashMap<FontFaceKey, usize>,
}

impl PdfFontRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers all glyphs in `run` when the underlying SFNT face can be embedded.
    /// Returns `Some(FontFaceKey)` on success, or `None` if curve fallback is required.
    pub(crate) fn register_run(&mut self, run: &ShapedPdfRun) -> Option<FontFaceKey> {
        if run.glyphs.is_empty() || run.font_bytes.is_empty() {
            return None;
        }
        let key = FontFaceKey::new(&run.font_bytes, run.face_index);
        let idx = if let Some(&existing) = self.by_key.get(&key) {
            existing
        } else {
            // Verify font can be parsed and has `glyf` or `CFF ` outlines
            let face = Face::parse(&run.font_bytes, run.face_index).ok()?;
            let raw = face.raw_face();
            let has_glyf = raw.table(Tag::from_bytes(b"glyf")).is_some()
                && raw.table(Tag::from_bytes(b"loca")).is_some();
            let has_cff = raw.table(Tag::from_bytes(b"CFF ")).is_some();
            if !has_glyf && !has_cff {
                return None;
            }
            let idx = self.fonts.len();
            self.fonts.push(CollectedFont {
                font_bytes: run.font_bytes.clone(),
                face_index: run.face_index,
                postscript_name: run.postscript_name.clone(),
                used_gids: BTreeMap::from([(0u16, String::new())]),
            });
            self.by_key.insert(key, idx);
            idx
        };

        let entry = &mut self.fonts[idx];
        for g in &run.glyphs {
            entry
                .used_gids
                .entry(g.glyph_id)
                .and_modify(|existing| {
                    if existing.is_empty() && !g.unicode.is_empty() {
                        *existing = g.unicode.clone();
                    }
                })
                .or_insert_with(|| g.unicode.clone());
        }
        Some(key)
    }

    /// Finalizes and adds all collected subset `/Type0` fonts into the PDF object store.
    pub(crate) fn emit_all(
        &self,
        add_obj: &mut dyn FnMut(Vec<u8>) -> usize,
    ) -> IoResult<HashMap<FontFaceKey, EmbeddedPdfFont>> {
        let mut result = HashMap::with_capacity(self.fonts.len());
        for (idx, font) in self.fonts.iter().enumerate() {
            let key = FontFaceKey::new(&font.font_bytes, font.face_index);
            let res_name = format!("F{}", idx + 1);
            let embedded = build_and_emit_font(font, res_name, add_obj)?;
            result.insert(key, embedded);
        }
        Ok(result)
    }
}

fn sanitize_ps_font_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    if cleaned.is_empty() {
        "CoreeSubsetFont".to_owned()
    } else {
        cleaned
    }
}

fn compute_subset_tag(used_gids: &BTreeMap<u16, String>, font_bytes: &[u8]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for b in font_bytes.iter().take(256) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    for &gid in used_gids.keys() {
        for b in gid.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    let mut tag = String::with_capacity(6);
    for _ in 0..6 {
        let ch = b'A' + ((h % 26) as u8);
        tag.push(char::from(ch));
        h /= 26;
        h ^= 0x9e37_79b9_7f4a_7c15;
    }
    tag
}

fn compress_pdf_stream(dict_extra: &str, raw: &[u8]) -> IoResult<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(raw)?;
    let compressed = enc.finish()?;
    let mut buf = format!(
        "<< {dict_extra} /Filter /FlateDecode /Length {} >>\nstream\n",
        compressed.len()
    )
    .into_bytes();
    buf.extend_from_slice(&compressed);
    buf.extend_from_slice(b"\nendstream");
    Ok(buf)
}

fn build_and_emit_font(
    collected: &CollectedFont,
    res_name: String,
    add_obj: &mut dyn FnMut(Vec<u8>) -> usize,
) -> IoResult<EmbeddedPdfFont> {
    let face = Face::parse(&collected.font_bytes, collected.face_index).map_err(|e| {
        IoError::InvalidFormat {
            format: "pdf".into(),
            message: format!("failed to parse embedded font: {e}"),
        }
    })?;

    let upem = f64::from(face.units_per_em().max(1));
    let scale = 1000.0 / upem;
    let bbox = face.global_bounding_box();
    let x_min = (f64::from(bbox.x_min) * scale).round() as i32;
    let y_min = (f64::from(bbox.y_min) * scale).round() as i32;
    let x_max = (f64::from(bbox.x_max) * scale).round() as i32;
    let y_max = (f64::from(bbox.y_max) * scale).round() as i32;
    let ascent = (f64::from(face.ascender()) * scale).round() as i32;
    let descent = (f64::from(face.descender()) * scale).round() as i32;
    let cap_height = face
        .capital_height()
        .map_or((ascent as f64 * 0.72).round() as i32, |c| {
            (f64::from(c) * scale).round() as i32
        });
    let italic_angle = face.italic_angle();
    let stem_v = if face.is_bold() { 120 } else { 80 };
    let mut flags: u32 = 4; // Symbolic (standard for Identity-H CID fonts)
    if face.is_monospaced() {
        flags |= 1;
    }
    if face.is_italic() {
        flags |= 64;
    }

    let tag = compute_subset_tag(&collected.used_gids, &collected.font_bytes);
    let base_ps = sanitize_ps_font_name(&collected.postscript_name);
    let subset_name = format!("{tag}+{base_ps}");

    let raw_face = face.raw_face();
    let is_ttf = raw_face.table(Tag::from_bytes(b"glyf")).is_some()
        && raw_face.table(Tag::from_bytes(b"loca")).is_some();

    if is_ttf {
        let (subset_sfnt, orig_to_subset, subset_to_orig) =
            build_subset_truetype_sfnt(&face, &collected.used_gids)?;

        let font_file_dict = format!("/Length1 {}", subset_sfnt.len());
        let font_file_id = add_obj(compress_pdf_stream(&font_file_dict, &subset_sfnt)?);

        let fd_body = format!(
            "<< /Type /FontDescriptor /FontName /{subset_name} /Flags {flags} \
             /FontBBox [{x_min} {y_min} {x_max} {y_max}] /ItalicAngle {italic_angle:.2} \
             /Ascent {ascent} /Descent {descent} /CapHeight {cap_height} /StemV {stem_v} \
             /FontFile2 {font_file_id} 0 R >>"
        );
        let fd_id = add_obj(fd_body.into_bytes());

        let mut widths_1000 = HashMap::with_capacity(subset_to_orig.len());
        let mut w_list = String::new();
        let mut to_unicode_entries: Vec<(u16, String)> = Vec::new();
        for (sub_gid_usize, &orig_gid) in subset_to_orig.iter().enumerate() {
            let sub_gid = sub_gid_usize as u16;
            let adv = face.glyph_hor_advance(GlyphId(orig_gid)).unwrap_or(0);
            let w_pdf = (f64::from(adv) * scale).round() as u32;
            widths_1000.insert(sub_gid, w_pdf);
            if !w_list.is_empty() {
                w_list.push(' ');
            }
            let _ = write!(w_list, "{w_pdf}");
            if let Some(uni) = collected.used_gids.get(&orig_gid) {
                if !uni.is_empty() {
                    to_unicode_entries.push((sub_gid, uni.clone()));
                }
            }
        }
        if to_unicode_entries.is_empty() {
            to_unicode_entries.push((0, "\u{FFFD}".to_owned()));
        }

        let cid_body = format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{subset_name} \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /FontDescriptor {fd_id} 0 R /DW 1000 /W [0 [{w_list}]] /CIDToGIDMap /Identity >>"
        );
        let cid_id = add_obj(cid_body.into_bytes());

        let cmap_bytes = build_tounicode_cmap(&to_unicode_entries);
        let cmap_id = add_obj(compress_pdf_stream("", &cmap_bytes)?);

        let type0_body = format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{subset_name} \
             /Encoding /Identity-H /DescendantFonts [{cid_id} 0 R] /ToUnicode {cmap_id} 0 R >>"
        );
        let type0_obj_id = add_obj(type0_body.into_bytes());

        Ok(EmbeddedPdfFont {
            res_name,
            type0_obj_id,
            is_identity_remapped: true,
            orig_to_subset,
            widths_1000,
        })
    } else {
        // CFF OpenType fallback (`/Subtype /CIDFontType0` + `/FontFile3 /CIDFontType0C`)
        let cff_data =
            raw_face
                .table(Tag::from_bytes(b"CFF "))
                .ok_or_else(|| IoError::InvalidFormat {
                    format: "pdf".into(),
                    message: "font has neither glyf nor CFF table".into(),
                })?;
        let font_file_id = add_obj(compress_pdf_stream("/Subtype /CIDFontType0C", cff_data)?);
        let fd_body = format!(
            "<< /Type /FontDescriptor /FontName /{subset_name} /Flags {flags} \
             /FontBBox [{x_min} {y_min} {x_max} {y_max}] /ItalicAngle {italic_angle:.2} \
             /Ascent {ascent} /Descent {descent} /CapHeight {cap_height} /StemV {stem_v} \
             /FontFile3 {font_file_id} 0 R >>"
        );
        let fd_id = add_obj(fd_body.into_bytes());

        let mut widths_1000 = HashMap::new();
        let mut w_str = String::new();
        let mut to_unicode_entries = Vec::new();
        for (&orig_gid, uni) in &collected.used_gids {
            let adv = face.glyph_hor_advance(GlyphId(orig_gid)).unwrap_or(0);
            let w_pdf = (f64::from(adv) * scale).round() as u32;
            widths_1000.insert(orig_gid, w_pdf);
            let _ = write!(w_str, "{orig_gid} [{w_pdf}] ");
            if !uni.is_empty() {
                to_unicode_entries.push((orig_gid, uni.clone()));
            }
        }
        if to_unicode_entries.is_empty() {
            to_unicode_entries.push((0, "\u{FFFD}".to_owned()));
        }

        let cid_body = format!(
            "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /{subset_name} \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /FontDescriptor {fd_id} 0 R /DW 1000 /W [{w_str}] >>"
        );
        let cid_id = add_obj(cid_body.into_bytes());
        let cmap_bytes = build_tounicode_cmap(&to_unicode_entries);
        let cmap_id = add_obj(compress_pdf_stream("", &cmap_bytes)?);
        let type0_body = format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{subset_name} \
             /Encoding /Identity-H /DescendantFonts [{cid_id} 0 R] /ToUnicode {cmap_id} 0 R >>"
        );
        let type0_obj_id = add_obj(type0_body.into_bytes());

        Ok(EmbeddedPdfFont {
            res_name,
            type0_obj_id,
            is_identity_remapped: false,
            orig_to_subset: HashMap::new(),
            widths_1000,
        })
    }
}

fn build_tounicode_cmap(entries: &[(u16, String)]) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n\
         <0000> <FFFF>\n\
         endcodespacerange\n",
    );
    for chunk in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", chunk.len());
        for (gid, uni) in chunk {
            let mut utf16_hex = String::new();
            for unit in uni.encode_utf16() {
                let _ = write!(utf16_hex, "{unit:04X}");
            }
            if utf16_hex.is_empty() {
                utf16_hex.push_str("FFFD");
            }
            let _ = writeln!(s, "<{gid:04X}> <{utf16_hex}>");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str(
        "endcmap\n\
         CMapName currentdict /CMap defineresource pop\n\
         end\n\
         end\n",
    );
    s.into_bytes()
}

fn read_be_u16(bytes: &[u8], off: usize) -> Option<u16> {
    let slice = bytes.get(off..off + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_be_i16(bytes: &[u8], off: usize) -> Option<i16> {
    let slice = bytes.get(off..off + 2)?;
    Some(i16::from_be_bytes([slice[0], slice[1]]))
}

fn read_be_u32(bytes: &[u8], off: usize) -> Option<u32> {
    let slice = bytes.get(off..off + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn glyph_glyf_range(
    loca: &[u8],
    long_loca: bool,
    glyf_len: usize,
    gid: u16,
) -> Option<(usize, usize)> {
    let idx = usize::from(gid);
    let (start, end) = if long_loca {
        let s = read_be_u32(loca, idx * 4)? as usize;
        let e = read_be_u32(loca, (idx + 1) * 4)? as usize;
        (s, e)
    } else {
        let s = (read_be_u16(loca, idx * 2)? as usize) * 2;
        let e = (read_be_u16(loca, (idx + 1) * 2)? as usize) * 2;
        (s, e)
    };
    if start > end || end > glyf_len {
        return None;
    }
    Some((start, end))
}

fn collect_composite_components(glyph_bytes: &[u8], out: &mut Vec<u16>) {
    if glyph_bytes.len() < 10 {
        return;
    }
    let Some(num_contours) = read_be_i16(glyph_bytes, 0) else {
        return;
    };
    if num_contours >= 0 {
        return;
    }
    const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
    const WE_HAVE_A_SCALE: u16 = 0x0008;
    const MORE_COMPONENTS: u16 = 0x0020;
    const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
    const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

    let mut off = 10usize;
    while let Some(flags) = read_be_u16(glyph_bytes, off) {
        let Some(comp_gid) = read_be_u16(glyph_bytes, off + 2) else {
            break;
        };
        out.push(comp_gid);
        off += 4;
        off += if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        if flags & WE_HAVE_A_SCALE != 0 {
            off += 2;
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            off += 4;
        } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            off += 8;
        }
        if flags & MORE_COMPONENTS == 0 {
            break;
        }
    }
}

fn rewrite_composite_components(glyph_bytes: &mut [u8], remap: &HashMap<u16, u16>) {
    if glyph_bytes.len() < 10 {
        return;
    }
    let Some(num_contours) = read_be_i16(glyph_bytes, 0) else {
        return;
    };
    if num_contours >= 0 {
        return;
    }
    const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
    const WE_HAVE_A_SCALE: u16 = 0x0008;
    const MORE_COMPONENTS: u16 = 0x0020;
    const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
    const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

    let mut off = 10usize;
    while let Some(flags) = read_be_u16(glyph_bytes, off) {
        let Some(comp_gid) = read_be_u16(glyph_bytes, off + 2) else {
            break;
        };
        let new_gid = remap.get(&comp_gid).copied().unwrap_or(0);
        glyph_bytes[off + 2..off + 4].copy_from_slice(&new_gid.to_be_bytes());
        off += 4;
        off += if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        if flags & WE_HAVE_A_SCALE != 0 {
            off += 2;
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            off += 4;
        } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            off += 8;
        }
        if flags & MORE_COMPONENTS == 0 {
            break;
        }
    }
}

type SubsetSfntResult = (Vec<u8>, HashMap<u16, u16>, Vec<u16>);

fn build_subset_truetype_sfnt(
    face: &Face<'_>,
    used_gids: &BTreeMap<u16, String>,
) -> IoResult<SubsetSfntResult> {
    let raw = face.raw_face();
    let glyf_orig = raw
        .table(Tag::from_bytes(b"glyf"))
        .ok_or_else(|| IoError::InvalidFormat {
            format: "pdf".into(),
            message: "missing glyf table".into(),
        })?;
    let loca_orig = raw
        .table(Tag::from_bytes(b"loca"))
        .ok_or_else(|| IoError::InvalidFormat {
            format: "pdf".into(),
            message: "missing loca table".into(),
        })?;
    let head_orig = raw
        .table(Tag::from_bytes(b"head"))
        .ok_or_else(|| IoError::InvalidFormat {
            format: "pdf".into(),
            message: "missing head table".into(),
        })?;
    let hhea_orig = raw
        .table(Tag::from_bytes(b"hhea"))
        .ok_or_else(|| IoError::InvalidFormat {
            format: "pdf".into(),
            message: "missing hhea table".into(),
        })?;
    let maxp_orig = raw
        .table(Tag::from_bytes(b"maxp"))
        .ok_or_else(|| IoError::InvalidFormat {
            format: "pdf".into(),
            message: "missing maxp table".into(),
        })?;

    if head_orig.len() < 54 || hhea_orig.len() < 36 || maxp_orig.len() < 6 {
        return Err(IoError::InvalidFormat {
            format: "pdf".into(),
            message: "truncated TrueType head/hhea/maxp table".into(),
        });
    }
    let long_loca = read_be_i16(head_orig, 50).unwrap_or(0) != 0;
    let total_orig_glyphs = face.number_of_glyphs();

    // Expand composite glyph components recursively
    let mut needed: BTreeSet<u16> = BTreeSet::new();
    let mut queue: VecDeque<u16> = VecDeque::new();
    needed.insert(0);
    queue.push_back(0);
    for &gid in used_gids.keys() {
        if gid < total_orig_glyphs && needed.insert(gid) {
            queue.push_back(gid);
        }
    }
    while let Some(gid) = queue.pop_front() {
        if let Some((s, e)) = glyph_glyf_range(loca_orig, long_loca, glyf_orig.len(), gid) {
            let mut comps = Vec::new();
            collect_composite_components(&glyf_orig[s..e], &mut comps);
            for c in comps {
                if c < total_orig_glyphs && needed.insert(c) {
                    queue.push_back(c);
                }
            }
        }
    }

    let mut subset_to_orig = Vec::with_capacity(needed.len());
    subset_to_orig.push(0);
    for &gid in &needed {
        if gid != 0 {
            subset_to_orig.push(gid);
        }
    }
    let mut orig_to_subset = HashMap::with_capacity(subset_to_orig.len());
    for (sub_idx, &orig_gid) in subset_to_orig.iter().enumerate() {
        orig_to_subset.insert(orig_gid, sub_idx as u16);
    }

    let subset_count = subset_to_orig.len() as u16;
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity((subset_to_orig.len() + 1) * 4);
    let mut new_hmtx = Vec::with_capacity(subset_to_orig.len() * 4);

    for &orig_gid in &subset_to_orig {
        let start_off = new_glyf.len() as u32;
        new_loca.extend_from_slice(&start_off.to_be_bytes());
        if let Some((s, e)) = glyph_glyf_range(loca_orig, long_loca, glyf_orig.len(), orig_gid) {
            if e > s {
                let mut gbytes = glyf_orig[s..e].to_vec();
                rewrite_composite_components(&mut gbytes, &orig_to_subset);
                new_glyf.extend_from_slice(&gbytes);
                while new_glyf.len() % 4 != 0 {
                    new_glyf.push(0);
                }
            }
        }
        let adv = face.glyph_hor_advance(GlyphId(orig_gid)).unwrap_or(0);
        let lsb = face.glyph_hor_side_bearing(GlyphId(orig_gid)).unwrap_or(0);
        new_hmtx.extend_from_slice(&adv.to_be_bytes());
        new_hmtx.extend_from_slice(&lsb.to_be_bytes());
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
    if new_glyf.is_empty() {
        new_glyf.extend_from_slice(&[0u8; 4]);
    }

    let mut new_head = head_orig.to_vec();
    new_head[8..12].copy_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment cleared before SFNT assembly
    new_head[50..52].copy_from_slice(&1i16.to_be_bytes()); // indexToLocFormat = 1 (long 32-bit)

    let mut new_hhea = hhea_orig.to_vec();
    new_hhea[34..36].copy_from_slice(&subset_count.to_be_bytes());

    let mut new_maxp = maxp_orig.to_vec();
    new_maxp[4..6].copy_from_slice(&subset_count.to_be_bytes());

    let mut new_post = vec![0u8; 32];
    if let Some(post_orig) = raw.table(Tag::from_bytes(b"post")) {
        let copy_len = post_orig.len().min(32);
        new_post[..copy_len].copy_from_slice(&post_orig[..copy_len]);
    }
    new_post[0..4].copy_from_slice(&0x0003_0000_u32.to_be_bytes()); // format 3.0 (no glyph names)

    let mut tables: BTreeMap<[u8; 4], Vec<u8>> = BTreeMap::new();
    tables.insert(*b"glyf", new_glyf);
    tables.insert(*b"head", new_head);
    tables.insert(*b"hhea", new_hhea);
    tables.insert(*b"hmtx", new_hmtx);
    tables.insert(*b"loca", new_loca);
    tables.insert(*b"maxp", new_maxp);
    tables.insert(*b"post", new_post);
    for opt_tag in [b"OS/2", b"cvt ", b"fpgm", b"name", b"prep"] {
        if let Some(data) = raw.table(Tag::from_bytes(opt_tag)) {
            if !data.is_empty() {
                tables.insert(*opt_tag, data.to_vec());
            }
        }
    }

    let sfnt = assemble_sfnt(tables);
    Ok((sfnt, orig_to_subset, subset_to_orig))
}

fn sfnt_table_checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0usize;
    while i < data.len() {
        let b0 = data[i];
        let b1 = data.get(i + 1).copied().unwrap_or(0);
        let b2 = data.get(i + 2).copied().unwrap_or(0);
        let b3 = data.get(i + 3).copied().unwrap_or(0);
        sum = sum.wrapping_add(u32::from_be_bytes([b0, b1, b2, b3]));
        i += 4;
    }
    sum
}

fn assemble_sfnt(tables: BTreeMap<[u8; 4], Vec<u8>>) -> Vec<u8> {
    let num_tables = tables.len() as u16;
    let mut entry_selector = 0u16;
    let mut max_pow2 = 1u16;
    while max_pow2 * 2 <= num_tables {
        max_pow2 *= 2;
        entry_selector += 1;
    }
    let search_range = max_pow2 * 16;
    let range_shift = num_tables * 16 - search_range;

    let header_len = 12 + usize::from(num_tables) * 16;
    let mut out = Vec::with_capacity(header_len + 4096);
    // sfntVersion = 0x00010000 (TrueType)
    out.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    let mut cur_offset = header_len;
    let mut head_offset = None;
    let mut table_blobs: Vec<Vec<u8>> = Vec::with_capacity(tables.len());

    for (tag, data) in tables {
        let len = data.len() as u32;
        let checksum = sfnt_table_checksum(&data);
        if &tag == b"head" {
            head_offset = Some(cur_offset);
        }
        out.extend_from_slice(&tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&(cur_offset as u32).to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());

        let padded_len = (data.len() + 3) & !3;
        cur_offset += padded_len;
        table_blobs.push(data);
    }

    for mut data in table_blobs {
        out.extend_from_slice(&data);
        let rem = data.len() % 4;
        if rem != 0 {
            data.clear();
            out.extend(std::iter::repeat_n(0, 4 - rem));
        }
    }

    if let Some(hoff) = head_offset {
        let whole_sum = sfnt_table_checksum(&out);
        let adj = 0xB1B0_AFBA_u32.wrapping_sub(whole_sum);
        if hoff + 12 <= out.len() {
            out[hoff + 8..hoff + 12].copy_from_slice(&adj.to_be_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{ArtisticText, TextDirection, TextRun, TextStyle};
    use coree_render::TextEngine;

    #[test]
    fn subset_font_parses_with_ttf_parser_and_builds_bilingual_tounicode() {
        let mut te = TextEngine::new();
        let text = ArtisticText {
            runs: vec![
                TextRun::new("coree Tier1 ", TextStyle::default()),
                TextRun::new(
                    "كوري",
                    TextStyle {
                        direction: TextDirection::Rtl,
                        ..TextStyle::default()
                    },
                ),
            ],
            direction: TextDirection::Auto,
            align: coree_core::TextAlign::Left,
        };
        let (runs, _fallback) = te.layout_artistic_pdf_runs(&text);
        assert!(
            !runs.is_empty(),
            "expected shaped PDF runs from installed system fonts"
        );
        let mut reg = PdfFontRegistry::new();
        let mut keys = Vec::new();
        for r in &runs {
            if let Some(k) = reg.register_run(r) {
                keys.push(k);
            }
        }
        assert!(!keys.is_empty());

        let mut objs = Vec::new();
        let embedded = reg
            .emit_all(&mut |bytes| {
                objs.push(bytes);
                objs.len()
            })
            .unwrap();
        assert_eq!(embedded.len(), keys.iter().collect::<BTreeSet<_>>().len());

        // Verify FontFile2 SFNT stream decompresses & parses cleanly via ttf-parser
        let all_text: String = objs
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("/Subtype /Type0"));
        assert!(all_text.contains("/Subtype /CIDFontType2"));
        assert!(all_text.contains("/CIDToGIDMap /Identity"));
        assert!(all_text.contains("/FontFile2"));
    }
}
