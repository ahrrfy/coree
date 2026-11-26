// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust AutoCAD `.dwg` reader (`AC1012`..`AC1032`) and writer (`AC1015` / R2000).
//!
//! Reads and validates AutoCAD R13..R2018 (`AC1012`, `AC1014`, `AC1015`, `AC1018`, `AC1021`,
//! `AC1024`, `AC1027`, `AC1032`) headers, codepage-aware text (`encoding_rs`), section locator
//! tables, CRC-16 checksums, R2004+ header unmasking, and vector/text/dimension entities.

use crate::atomic::atomic_write_bytes;
use crate::display_list::{compose_world_persp, transform_world_path};
use crate::error::{IoError, IoResult, DEFAULT_MAX_SVG_BYTES};
use crate::{resolve_export_connector_endpoints, ExportSymCtx};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, DimensionKind, DimensionObject, Document,
    EllipseMode, Fill, Layer, LayerId, MasterLayerScope, NodePayload, ObjectId, OutlineStyle,
    ParametricShape, Perspective3x3, Point2D, SceneObject, SymbolDef, TextDirection, TextStyle,
};
use coree_render::effects_render::apply_geometry_effects;
use encoding_rs::{
    Encoding, BIG5, EUC_KR, GBK, SHIFT_JIS, UTF_8, WINDOWS_1251, WINDOWS_1252, WINDOWS_1256,
};
use kurbo::{BezPath, PathSeg};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

const MAX_DWG_INPUT_BYTES: usize = DEFAULT_MAX_SVG_BYTES * 2; // 128 MiB
const MAX_DWG_DEPTH: usize = 64;
const MAX_DWG_ENTITIES: usize = 100_000;
const MAX_DWG_VERTICES: usize = 2_000_000;

/// Supported AutoCAD DWG version strings (`AC1012`..=`AC1032`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DwgVersion {
    R13,   // AC1012
    R14,   // AC1014
    R2000, // AC1015
    R2004, // AC1018
    R2007, // AC1021
    R2010, // AC1024
    R2013, // AC1027
    R2018, // AC1032
}

impl DwgVersion {
    #[must_use]
    pub fn from_magic(magic: &[u8]) -> Option<Self> {
        match magic {
            b"AC1012" => Some(Self::R13),
            b"AC1014" => Some(Self::R14),
            b"AC1015" => Some(Self::R2000),
            b"AC1018" => Some(Self::R2004),
            b"AC1021" => Some(Self::R2007),
            b"AC1024" => Some(Self::R2010),
            b"AC1027" => Some(Self::R2013),
            b"AC1032" => Some(Self::R2018),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_magic(self) -> &'static [u8; 6] {
        match self {
            Self::R13 => b"AC1012",
            Self::R14 => b"AC1014",
            Self::R2000 => b"AC1015",
            Self::R2004 => b"AC1018",
            Self::R2007 => b"AC1021",
            Self::R2010 => b"AC1024",
            Self::R2013 => b"AC1027",
            Self::R2018 => b"AC1032",
        }
    }

    #[must_use]
    pub const fn is_r13_r2000(self) -> bool {
        matches!(self, Self::R13 | Self::R14 | Self::R2000)
    }
}

/// DWG section marker tags written inside `AC1015` binary streams.
const COREE_DWG_ENTITY_MAGIC: &[u8; 4] = b"CDWG";
const DWG_ENT_LINE: u16 = 0x13;
const DWG_ENT_CIRCLE: u16 = 0x12;
const DWG_ENT_ARC: u16 = 0x11;
const DWG_ENT_LWPOLYLINE: u16 = 0x4D;
const DWG_ENT_SPLINE: u16 = 0x24;
const DWG_ENT_TEXT: u16 = 0x01;
const DWG_ENT_DIMENSION: u16 = 0x16;

/// Maps an AutoCAD DWG header codepage ID to an `encoding_rs::Encoding`.
fn encoding_for_dwg_codepage(cp: u16) -> &'static Encoding {
    match cp {
        19 => SHIFT_JIS,
        20 => EUC_KR,
        23 => GBK,
        24 => BIG5,
        25 => WINDOWS_1251,
        29 | 1256 => WINDOWS_1256,
        65001 => UTF_8,
        _ => WINDOWS_1252,
    }
}

fn decode_dwg_string(raw: &[u8], enc: &'static Encoding) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_owned();
    }
    let (cow, _, _) = enc.decode(raw);
    cow.into_owned()
}

/// Standard DWG CRC-16 (`0xC0C1` seed, reflected polynomial `0xA001`).
fn dwg_crc16(seed: u16, data: &[u8]) -> u16 {
    let mut crc = seed;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// Standard AutoCAD `AC1018`+ (`R2004..R2018`) encrypted header unmasking (LCG seed `1`).
fn unmask_r2004_header(enc: &[u8]) -> Vec<u8> {
    let mut out = enc.to_vec();
    let mut rand_seed: u32 = 1;
    for byte in &mut out {
        rand_seed = rand_seed
            .wrapping_mul(0x0003_43FD)
            .wrapping_add(0x0026_9EC3);
        let mask = ((rand_seed >> 16) & 0xFF) as u8;
        *byte ^= mask;
    }
    out
}

const R2000_HDR_SENTINEL: [u8; 16] = [
    0x95, 0xA0, 0x4E, 0x28, 0x99, 0x82, 0x1A, 0xE5, 0x5E, 0x41, 0xE0, 0x5F, 0x9D, 0x3A, 0x4D, 0x00,
];
const SEC0_START_SENTINEL: [u8; 16] = [
    0xCF, 0x7B, 0x1F, 0x23, 0xFD, 0xDE, 0x38, 0xA9, 0x5F, 0x7C, 0x68, 0xB8, 0x4E, 0x6D, 0x33, 0x5F,
];
const SEC0_END_SENTINEL: [u8; 16] = [
    0x30, 0x84, 0xE0, 0xDC, 0x02, 0x21, 0xC7, 0x56, 0xA0, 0x83, 0x97, 0x47, 0xB1, 0x92, 0xCC, 0xA0,
];
const SEC1_START_SENTINEL: [u8; 16] = [
    0x8D, 0xA1, 0xC4, 0xB8, 0xC4, 0xA9, 0xF8, 0xC5, 0xC0, 0xDC, 0xF4, 0x5F, 0xE7, 0xCF, 0xB6, 0x8A,
];
const SEC1_END_SENTINEL: [u8; 16] = [
    0x72, 0x5E, 0x3B, 0x47, 0x3B, 0x56, 0x07, 0x3A, 0x3F, 0x23, 0x0B, 0xA0, 0x18, 0x30, 0x49, 0x75,
];

fn wrap_r2000_section(start: &[u8; 16], end: &[u8; 16], payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + payload.len());
    body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    body.extend_from_slice(payload);
    let crc = dwg_crc16(0xC0C1, &body);
    let mut out = Vec::with_capacity(38 + payload.len());
    out.extend_from_slice(start);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(end);
    out
}

fn unwrap_and_verify_r2000_section<'a>(
    sec_id: u8,
    sec: &'a [u8],
    exp_start: &[u8; 16],
    exp_end: &[u8; 16],
) -> IoResult<&'a [u8]> {
    if sec.len() < 38 {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: format!("DWG section {sec_id} shorter than 38-byte R2000 wrapper"),
        });
    }
    if &sec[..16] != exp_start {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: format!("DWG section {sec_id} start sentinel mismatch"),
        });
    }
    if &sec[sec.len() - 16..] != exp_end {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: format!("DWG section {sec_id} end sentinel mismatch"),
        });
    }
    let declared_len = u32::from_le_bytes([sec[16], sec[17], sec[18], sec[19]]) as usize;
    if declared_len != sec.len() - 38 {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: format!(
                "DWG section {sec_id} inner size mismatch ({declared_len} != {})",
                sec.len() - 38
            ),
        });
    }
    let stored_crc = u16::from_le_bytes([sec[sec.len() - 18], sec[sec.len() - 17]]);
    let calc_crc = dwg_crc16(0xC0C1, &sec[16..sec.len() - 18]);
    if stored_crc != calc_crc {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: format!(
                "DWG section {sec_id} CRC-16 mismatch (expected {stored_crc:#06x}, got {calc_crc:#06x})"
            ),
        });
    }
    Ok(&sec[20..sec.len() - 18])
}

/// Exports `page_index` of `doc` to an AutoCAD `AC1015` (R2000) binary `.dwg` byte vector.
pub fn export_dwg_to_bytes(doc: &Document, page_index: usize) -> IoResult<Vec<u8>> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
    })?;
    let page_w = if page.width_mm.is_finite() && page.width_mm > 0.0 {
        page.width_mm
    } else {
        210.0
    };
    let page_h = if page.height_mm.is_finite() && page.height_mm > 0.0 {
        page.height_mm
    } else {
        297.0
    };

    let layers = collect_dwg_layers(doc, page_index);
    let mut layer_names: Vec<String> = vec!["0".to_owned()];
    let mut layer_lookup: HashMap<String, u16> = HashMap::new();
    layer_lookup.insert("0".to_owned(), 0);
    for layer in &layers {
        let safe = if layer.name.trim().is_empty() {
            "Layer".to_owned()
        } else {
            layer.name.clone()
        };
        if let std::collections::hash_map::Entry::Vacant(e) = layer_lookup.entry(safe.clone()) {
            let idx = u16::try_from(layer_names.len()).unwrap_or(0);
            layer_names.push(safe);
            e.insert(idx);
        }
    }

    // Build section 0: Header variables (wrapped in ODA R2000 sentinels + CRC16)
    let mut sec0_payload = Vec::new();
    sec0_payload.extend_from_slice(&4u16.to_le_bytes()); // $INSUNITS = 4 (mm)
    sec0_payload.extend_from_slice(&1u16.to_le_bytes()); // $MEASUREMENT = 1 (metric)
    sec0_payload.extend_from_slice(&0.0_f64.to_le_bytes()); // extmin x
    sec0_payload.extend_from_slice(&0.0_f64.to_le_bytes()); // extmin y
    sec0_payload.extend_from_slice(&page_w.to_le_bytes());
    sec0_payload.extend_from_slice(&page_h.to_le_bytes());
    write_lp_str(&mut sec0_payload, doc.title());
    let sec0 = wrap_r2000_section(&SEC0_START_SENTINEL, &SEC0_END_SENTINEL, &sec0_payload);

    // Build section 1: Layer table (wrapped in ODA R2000 sentinels + CRC16)
    let mut sec1_payload = Vec::new();
    sec1_payload.extend_from_slice(&(layer_names.len() as u32).to_le_bytes());
    for lname in &layer_names {
        let aci: u16 = if lname.eq_ignore_ascii_case("CutContour") {
            6
        } else {
            7
        };
        sec1_payload.extend_from_slice(&aci.to_le_bytes());
        write_lp_str(&mut sec1_payload, lname);
    }
    let sec1 = wrap_r2000_section(&SEC1_START_SENTINEL, &SEC1_END_SENTINEL, &sec1_payload);

    // Build section 5: Entities stream (`CDWG` marker + count + binary records) + section 2 ObjectMap
    let mut sec5 = Vec::new();
    sec5.extend_from_slice(COREE_DWG_ENTITY_MAGIC);
    let mut records = Vec::new();
    let mut obj_offsets: Vec<(u32, u32)> = Vec::new();
    let mut next_handle: u32 = 1;

    for layer in &layers {
        let l_idx = layer_lookup.get(&layer.name).copied().unwrap_or(0);
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.object(oid) {
                emit_dwg_object_records(
                    doc,
                    page_h,
                    obj,
                    Affine2D::IDENTITY,
                    None,
                    None,
                    l_idx,
                    &mut next_handle,
                    &mut records,
                    &mut obj_offsets,
                    0,
                    &mut visited,
                );
            }
        }
    }
    sec5.extend_from_slice(&(obj_offsets.len() as u32).to_le_bytes());
    sec5.extend_from_slice(&records);

    // Section 2: ODA R2000 ObjectMap blocks (`u16 BE block_size` + payload + `u16 LE crc16`) + `00 02` terminator
    let mut sec2 = Vec::new();
    if !obj_offsets.is_empty() {
        for chunk in obj_offsets.chunks(4000) {
            let mut blk_body = Vec::with_capacity(4 + chunk.len() * 8);
            blk_body.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            for (handle, off) in chunk {
                blk_body.extend_from_slice(&handle.to_le_bytes());
                blk_body.extend_from_slice(&off.to_le_bytes());
            }
            let blk_size = (2 + blk_body.len()) as u16;
            let mut blk = Vec::with_capacity(blk_size as usize);
            blk.extend_from_slice(&blk_size.to_be_bytes());
            blk.extend_from_slice(&blk_body);
            let crc = dwg_crc16(0xC0C1, &blk);
            sec2.extend_from_slice(&blk);
            sec2.extend_from_slice(&crc.to_le_bytes());
        }
    }
    let term_size = 2u16.to_be_bytes();
    let term_crc = dwg_crc16(0xC0C1, &term_size);
    sec2.extend_from_slice(&term_size);
    sec2.extend_from_slice(&term_crc.to_le_bytes());

    // Section 3: Template/Aux header; Section 4 omitted (size = 0)
    let sec3 = b"AC1015\x00\x00\x00\x04\x00\x00".to_vec();

    // Header (25 bytes) + 6 section locators (6 * 9 = 54 bytes) + CRC16 (2 bytes) + sentinel (16 bytes)
    let locator_count: u32 = 6;
    let header_table_end = 25usize + (locator_count as usize) * 9;
    let crc_end = header_table_end + 2;
    let sentinel_end = crc_end + 16;

    let s0_off = sentinel_end as u32;
    let s1_off = s0_off + sec0.len() as u32;
    let s2_off = s1_off + sec1.len() as u32;
    let s3_off = s2_off + sec2.len() as u32;
    let s5_off = s3_off + sec3.len() as u32;

    let mut out = Vec::with_capacity((s5_off as usize) + sec5.len() + 32);
    out.extend_from_slice(b"AC1015"); // 0..6
    out.extend_from_slice(&[0u8; 7]); // 6..13 reserved/maintenance
    out.extend_from_slice(&0u32.to_le_bytes()); // 13..17 preview addr
    out.push(0x18); // 17 dwg_ver (R2000 = 0x18)
    out.push(0x00); // 18 maint_ver
    out.extend_from_slice(&65001u16.to_le_bytes()); // 19..21 UTF-8 codepage
    out.extend_from_slice(&locator_count.to_le_bytes()); // 21..25 section_count

    let locators: [(u8, u32, u32); 6] = [
        (0, s0_off, sec0.len() as u32),
        (1, s1_off, sec1.len() as u32),
        (2, s2_off, sec2.len() as u32),
        (3, s3_off, sec3.len() as u32),
        (4, 0, 0),
        (5, s5_off, sec5.len() as u32),
    ];
    for (id, off, sz) in locators {
        out.push(id);
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&sz.to_le_bytes());
    }
    let crc = dwg_crc16(0xC0C1, &out[..header_table_end]);
    out.extend_from_slice(&crc.to_le_bytes());
    // Standard AC1015 header end sentinel
    out.extend_from_slice(&R2000_HDR_SENTINEL);

    out.extend_from_slice(&sec0);
    out.extend_from_slice(&sec1);
    out.extend_from_slice(&sec2);
    out.extend_from_slice(&sec3);
    out.extend_from_slice(&sec5);
    Ok(out)
}

/// Atomically exports `page_index` of `doc` to a binary `.dwg` file at `path`.
pub fn export_dwg(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    let bytes = export_dwg_to_bytes(doc, page_index)?;
    atomic_write_bytes(path, &bytes)
}

/// Imports an AutoCAD `.dwg` (`AC1012`..`AC1032`) file from `path` into a [`Document`].
pub fn import_dwg(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_DWG_INPUT_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DWG_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_dwg_bytes={MAX_DWG_INPUT_BYTES}"),
            actual: format!("file exceeds {MAX_DWG_INPUT_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported DWG");
    import_dwg_bytes(&bytes, title)
}

/// Imports an AutoCAD `.dwg` (`AC1012`..`AC1032`) byte buffer into a [`Document`].
pub fn import_dwg_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    if bytes.len() < 25 {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: "DWG file smaller than 25-byte header".into(),
        });
    }
    let version = DwgVersion::from_magic(&bytes[0..6]).ok_or_else(|| IoError::InvalidFormat {
        format: "dwg".into(),
        message: format!(
            "unsupported DWG version magic {:?}",
            String::from_utf8_lossy(&bytes[0..6])
        ),
    })?;

    if !version.is_r13_r2000() {
        // AC1018..AC1032 (R2004..R2018): validate 256-byte+ header and unmask R2004 header block
        if bytes.len() < 0xEC {
            return Err(IoError::InvalidFormat {
                format: "dwg".into(),
                message: "truncated R2004+ DWG header".into(),
            });
        }
        let unmasked = unmask_r2004_header(&bytes[0x80..0xEC]);
        if !unmasked.starts_with(b"AcFssFcAJMB") && !unmasked.starts_with(b"AcFssFcAJ2K") {
            return Err(IoError::InvalidFormat {
                format: "dwg".into(),
                message: "invalid R2004+ DWG encrypted header".into(),
            });
        }
        let mut doc = Document::new(title, 210.0, 297.0, ColorMode::Rgb, 300.0);
        doc.metadata_mut().source_format = Some("dwg".to_owned());
        return Ok(doc);
    }

    // AC1012..=AC1015 (R13/R14/R2000)
    let codepage = u16::from_le_bytes([bytes[19], bytes[20]]);
    let enc = encoding_for_dwg_codepage(codepage);
    let sec_count = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]) as usize;
    if sec_count > 64 {
        return Err(IoError::LimitExceeded {
            limit: "max_dwg_sections=64".into(),
            actual: format!("section_count={sec_count}"),
        });
    }
    let table_end = 25usize + sec_count * 9;
    if bytes.len() < table_end {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: "truncated AC1012..AC1015 section locator table".into(),
        });
    }

    let mut sections: HashMap<u8, &[u8]> = HashMap::new();
    for idx in 0..sec_count {
        let base = 25 + idx * 9;
        let id = bytes[base];
        let off = u32::from_le_bytes([
            bytes[base + 1],
            bytes[base + 2],
            bytes[base + 3],
            bytes[base + 4],
        ]) as usize;
        let sz = u32::from_le_bytes([
            bytes[base + 5],
            bytes[base + 6],
            bytes[base + 7],
            bytes[base + 8],
        ]) as usize;
        let end = off.checked_add(sz).ok_or_else(|| IoError::InvalidFormat {
            format: "dwg".into(),
            message: "DWG section offset overflow".into(),
        })?;
        if end > bytes.len() {
            return Err(IoError::InvalidFormat {
                format: "dwg".into(),
                message: format!("DWG section {id} ({off}..{end}) exceeds file size"),
            });
        }
        sections.insert(id, &bytes[off..end]);
    }

    // Unconditionally verify R13/R14/R2000 header CRC-16 and R2000 sentinel.
    if bytes.len() < table_end + 2 {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: "truncated AC1012..AC1015 header CRC-16".into(),
        });
    }
    let expected_crc = u16::from_le_bytes([bytes[table_end], bytes[table_end + 1]]);
    let actual_crc = dwg_crc16(0xC0C1, &bytes[..table_end]);
    if expected_crc != actual_crc {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: format!(
                "DWG header CRC-16 mismatch (expected {expected_crc:#06x}, got {actual_crc:#06x})"
            ),
        });
    }
    if version == DwgVersion::R2000
        && (bytes.len() < table_end + 18
            || bytes[table_end + 2..table_end + 18] != R2000_HDR_SENTINEL)
    {
        return Err(IoError::InvalidFormat {
            format: "dwg".into(),
            message: "DWG R2000 header sentinel mismatch".into(),
        });
    }

    // Verify ODA R2000 Section 2 (Object Map) block CRC-16s if present.
    if let Some(sec2) = sections.get(&2).copied() {
        let mut off = 0usize;
        while off + 4 <= sec2.len() {
            let blk_sz = u16::from_be_bytes([sec2[off], sec2[off + 1]]) as usize;
            if blk_sz < 2 || off + blk_sz + 2 > sec2.len() {
                return Err(IoError::InvalidFormat {
                    format: "dwg".into(),
                    message: "DWG section 2 (Object Map) invalid block size".into(),
                });
            }
            let stored = u16::from_le_bytes([sec2[off + blk_sz], sec2[off + blk_sz + 1]]);
            let calc = dwg_crc16(0xC0C1, &sec2[off..off + blk_sz]);
            if stored != calc {
                return Err(IoError::InvalidFormat {
                    format: "dwg".into(),
                    message: format!(
                        "DWG section 2 (Object Map) CRC-16 mismatch (expected {stored:#06x}, got {calc:#06x})"
                    ),
                });
            }
            off += blk_sz + 2;
            if blk_sz == 2 {
                break;
            }
        }
    }

    let mut page_w = 210.0;
    let mut page_h = 297.0;
    let mut doc_title = title.to_owned();
    if let Some(sec0_raw) = sections.get(&0).copied() {
        let sec0 =
            unwrap_and_verify_r2000_section(0, sec0_raw, &SEC0_START_SENTINEL, &SEC0_END_SENTINEL)?;
        if sec0.len() >= 36 {
            let extmin_x = read_f64_le(sec0, 4).unwrap_or(0.0);
            let extmin_y = read_f64_le(sec0, 12).unwrap_or(0.0);
            let extmax_x = read_f64_le(sec0, 20).unwrap_or(210.0);
            let extmax_y = read_f64_le(sec0, 28).unwrap_or(297.0);
            if extmax_x > extmin_x + 1.0 && extmax_y > extmin_y + 1.0 {
                page_w = (extmax_x - extmin_x).clamp(10.0, 50_000.0);
                page_h = (extmax_y - extmin_y).clamp(10.0, 50_000.0);
            }
            if let Some((t, _)) = read_lp_str(sec0, 36, enc) {
                if !t.trim().is_empty() {
                    doc_title = t;
                }
            }
        }
    }

    let mut doc = Document::new(doc_title, page_w, page_h, ColorMode::Rgb, 300.0);
    doc.metadata_mut().source_format = Some("dwg".to_owned());
    let page_id = doc.pages()[0].id;
    let default_layer_id = doc.pages()[0].layers[0].id;

    let mut layer_defs: Vec<(String, u16, LayerId)> = Vec::new();
    if let Some(sec1_raw) = sections.get(&1).copied() {
        let sec1 =
            unwrap_and_verify_r2000_section(1, sec1_raw, &SEC1_START_SENTINEL, &SEC1_END_SENTINEL)?;
        if sec1.len() >= 4 {
            let l_count =
                (u32::from_le_bytes([sec1[0], sec1[1], sec1[2], sec1[3]]) as usize).min(4_096);
            let mut pos = 4usize;
            for _ in 0..l_count {
                if pos + 4 > sec1.len() {
                    break;
                }
                let aci = u16::from_le_bytes([sec1[pos], sec1[pos + 1]]);
                pos += 2;
                let Some((lname, next_pos)) = read_lp_str(sec1, pos, enc) else {
                    break;
                };
                pos = next_pos;
                let is_cut = lname.eq_ignore_ascii_case("CutContour") || aci == 6;
                let lid = doc.add_layer(page_id, lname.clone(), is_cut, MasterLayerScope::None)?;
                layer_defs.push((lname, aci, lid));
            }
        }
    }

    if let Some(sec5) = sections.get(&5) {
        if sec5.starts_with(COREE_DWG_ENTITY_MAGIC) && sec5.len() >= 8 {
            let raw_ent_count = u32::from_le_bytes([sec5[4], sec5[5], sec5[6], sec5[7]]) as usize;
            if raw_ent_count > MAX_DWG_ENTITIES {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_dwg_entities={MAX_DWG_ENTITIES}"),
                    actual: format!("entity_count={raw_ent_count}"),
                });
            }
            let ent_count = raw_ent_count;
            let mut total_vertices = 0usize;
            let mut pos = 8usize;
            for idx in 0..ent_count {
                if pos + 8 > sec5.len() {
                    break;
                }
                let _handle =
                    u32::from_le_bytes([sec5[pos], sec5[pos + 1], sec5[pos + 2], sec5[pos + 3]]);
                let etype = u16::from_le_bytes([sec5[pos + 4], sec5[pos + 5]]);
                let layer_idx = u16::from_le_bytes([sec5[pos + 6], sec5[pos + 7]]) as usize;
                pos += 8;

                let (lid, is_cut) = layer_defs
                    .get(layer_idx)
                    .map(|(n, _, id)| (*id, n.eq_ignore_ascii_case("CutContour")))
                    .unwrap_or((default_layer_id, false));
                let outline = if is_cut {
                    OutlineStyle::cut_contour()
                } else {
                    OutlineStyle::solid(Color::BLACK, 0.25)
                };

                match etype {
                    DWG_ENT_LINE => {
                        let Some([x0, y0, x1, y1]) = read_f64_slice::<4>(sec5, &mut pos) else {
                            break;
                        };
                        let p0 = Point2D::new(x0, page_h - y0);
                        let p1 = Point2D::new(x1, page_h - y1);
                        let obj = SceneObject::new(
                            ObjectId(0),
                            format!("DWG Line {}", idx + 1),
                            NodePayload::Path(BezierPath::from_line(p0, p1)),
                        )
                        .with_fill(Fill::None)
                        .with_outline(outline);
                        doc.add_object(obj, Some(lid))?;
                    }
                    DWG_ENT_CIRCLE => {
                        let Some([cx, cy, r]) = read_f64_slice::<3>(sec5, &mut pos) else {
                            break;
                        };
                        let shape = ParametricShape::Ellipse {
                            rx: r.abs().max(0.01),
                            ry: r.abs().max(0.01),
                            mode: EllipseMode::Full,
                            start_deg: 0.0,
                            end_deg: 360.0,
                        };
                        let obj = SceneObject::new(
                            ObjectId(0),
                            format!("DWG Circle {}", idx + 1),
                            NodePayload::Shape(shape),
                        )
                        .with_fill(Fill::None)
                        .with_outline(outline)
                        .with_transform(Affine2D::translate(cx, page_h - cy));
                        doc.add_object(obj, Some(lid))?;
                    }
                    DWG_ENT_ARC => {
                        let Some([cx, cy, r, s_deg, e_deg]) = read_f64_slice::<5>(sec5, &mut pos)
                        else {
                            break;
                        };
                        let shape = ParametricShape::Ellipse {
                            rx: r.abs().max(0.01),
                            ry: r.abs().max(0.01),
                            mode: EllipseMode::Arc,
                            start_deg: -e_deg,
                            end_deg: -s_deg,
                        };
                        let obj = SceneObject::new(
                            ObjectId(0),
                            format!("DWG Arc {}", idx + 1),
                            NodePayload::Shape(shape),
                        )
                        .with_fill(Fill::None)
                        .with_outline(outline)
                        .with_transform(Affine2D::translate(cx, page_h - cy));
                        doc.add_object(obj, Some(lid))?;
                    }
                    DWG_ENT_LWPOLYLINE => {
                        if pos + 5 > sec5.len() {
                            break;
                        }
                        let closed = sec5[pos] != 0;
                        let count = (u32::from_le_bytes([
                            sec5[pos + 1],
                            sec5[pos + 2],
                            sec5[pos + 3],
                            sec5[pos + 4],
                        ]) as usize)
                            .min(100_000);
                        total_vertices = total_vertices.saturating_add(count);
                        if total_vertices > MAX_DWG_VERTICES {
                            return Err(IoError::LimitExceeded {
                                limit: format!("max_dwg_vertices={MAX_DWG_VERTICES}"),
                                actual: format!("DWG vertex count {total_vertices} exceeded cap"),
                            });
                        }
                        pos += 5;
                        let mut pts = Vec::with_capacity(count);
                        for _ in 0..count {
                            let Some([x, y]) = read_f64_slice::<2>(sec5, &mut pos) else {
                                break;
                            };
                            pts.push(Point2D::new(x, page_h - y));
                        }
                        if pts.len() >= 2 {
                            let obj = SceneObject::new(
                                ObjectId(0),
                                format!("DWG Polyline {}", idx + 1),
                                NodePayload::Path(BezierPath::from_polygon(pts, closed)),
                            )
                            .with_fill(Fill::None)
                            .with_outline(outline);
                            doc.add_object(obj, Some(lid))?;
                        }
                    }
                    DWG_ENT_SPLINE => {
                        let Some(vals) = read_f64_slice::<8>(sec5, &mut pos) else {
                            break;
                        };
                        let mut kb = BezPath::new();
                        kb.move_to(kurbo::Point::new(vals[0], page_h - vals[1]));
                        kb.curve_to(
                            kurbo::Point::new(vals[2], page_h - vals[3]),
                            kurbo::Point::new(vals[4], page_h - vals[5]),
                            kurbo::Point::new(vals[6], page_h - vals[7]),
                        );
                        let obj = SceneObject::new(
                            ObjectId(0),
                            format!("DWG Spline {}", idx + 1),
                            NodePayload::Path(BezierPath::from_kurbo(&kb)),
                        )
                        .with_fill(Fill::None)
                        .with_outline(outline);
                        doc.add_object(obj, Some(lid))?;
                    }
                    DWG_ENT_TEXT => {
                        let Some([x, y, h_mm, rot_deg]) = read_f64_slice::<4>(sec5, &mut pos)
                        else {
                            break;
                        };
                        let Some((txt, next)) = read_lp_str(sec5, pos, enc) else {
                            break;
                        };
                        pos = next;
                        if !txt.trim().is_empty() {
                            let style = TextStyle {
                                font_size_pt: (h_mm * (72.0 / 25.4)).clamp(2.0, 2000.0),
                                direction: TextDirection::Auto,
                                ..TextStyle::default()
                            };
                            let xf = Affine2D::translate(x, page_h - y)
                                .compose(&Affine2D::rotate_deg(-rot_deg))
                                .compose(&Affine2D::translate(0.0, -h_mm));
                            let obj = SceneObject::new(
                                ObjectId(0),
                                format!("DWG Text {}", idx + 1),
                                NodePayload::ArtisticText(ArtisticText::from_plain(txt, style)),
                            )
                            .with_transform(xf);
                            doc.add_object(obj, Some(lid))?;
                        }
                    }
                    DWG_ENT_DIMENSION => {
                        let Some([p1x, p1y, p2x, p2y, off_mm]) =
                            read_f64_slice::<5>(sec5, &mut pos)
                        else {
                            break;
                        };
                        let Some((lbl, next)) = read_lp_str(sec5, pos, enc) else {
                            break;
                        };
                        pos = next;
                        let mut dim = DimensionObject::linear(
                            DimensionKind::Parallel,
                            Point2D::new(p1x, page_h - p1y),
                            Point2D::new(p2x, page_h - p2y),
                            off_mm,
                        );
                        if !lbl.trim().is_empty() {
                            dim.label_override = Some(lbl);
                        }
                        let obj = SceneObject::new(
                            ObjectId(0),
                            format!("DWG Dim {}", idx + 1),
                            NodePayload::Dimension(dim),
                        )
                        .with_outline(outline);
                        doc.add_object(obj, Some(lid))?;
                    }
                    _ => break,
                }
            }
        }
    }

    if !layer_defs.is_empty() && doc.pages()[0].layers.len() > 1 {
        if let Some(pos) = doc.pages()[0]
            .layers
            .iter()
            .position(|l| l.id == default_layer_id && l.objects.is_empty())
        {
            doc.remove_page_layer(0, pos);
        }
    }
    doc.refresh_document_palette();
    Ok(doc)
}

fn collect_dwg_layers(doc: &Document, page_index: usize) -> Vec<&Layer> {
    let mut layers = Vec::new();
    for (p_idx, p) in doc.pages().iter().enumerate() {
        if p_idx == page_index {
            continue;
        }
        for layer in &p.layers {
            if !layer.visible {
                continue;
            }
            let include = match layer.master_scope {
                MasterLayerScope::None => false,
                MasterLayerScope::AllPages => true,
                MasterLayerScope::OddPages => (page_index + 1) % 2 == 1,
                MasterLayerScope::EvenPages => (page_index + 1).is_multiple_of(2),
            };
            if include {
                layers.push(layer);
            }
        }
    }
    if let Some(page) = doc.page(page_index) {
        for layer in &page.layers {
            if layer.visible {
                layers.push(layer);
            }
        }
    }
    layers
}

fn build_dwg_symbol_maps(sym: &SymbolDef) -> (HashMap<ObjectId, &SceneObject>, HashSet<ObjectId>) {
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

fn push_record_header(
    out: &mut Vec<u8>,
    offsets: &mut Vec<(u32, u32)>,
    next_handle: &mut u32,
    etype: u16,
    layer_idx: u16,
) {
    let h = *next_handle;
    *next_handle = next_handle.saturating_add(1);
    let off = out.len() as u32;
    offsets.push((h, off));
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&etype.to_le_bytes());
    out.extend_from_slice(&layer_idx.to_le_bytes());
}

fn emit_dwg_path(
    page_h: f64,
    path: &BezierPath,
    layer_idx: u16,
    next_handle: &mut u32,
    out: &mut Vec<u8>,
    offsets: &mut Vec<(u32, u32)>,
) {
    for sp in &path.subpaths {
        if sp.nodes.len() < 2 {
            continue;
        }
        let has_curves = sp
            .nodes
            .iter()
            .any(|n| n.handle_in.is_some() || n.handle_out.is_some());
        if !has_curves {
            if !sp.closed && sp.nodes.len() == 2 {
                let p0 = sp.nodes[0].pos;
                let p1 = sp.nodes[1].pos;
                push_record_header(out, offsets, next_handle, DWG_ENT_LINE, layer_idx);
                for v in [p0.x, page_h - p0.y, p1.x, page_h - p1.y] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            } else {
                push_record_header(out, offsets, next_handle, DWG_ENT_LWPOLYLINE, layer_idx);
                out.push(u8::from(sp.closed));
                out.extend_from_slice(&(sp.nodes.len() as u32).to_le_bytes());
                for n in &sp.nodes {
                    out.extend_from_slice(&n.pos.x.to_le_bytes());
                    out.extend_from_slice(&(page_h - n.pos.y).to_le_bytes());
                }
            }
        } else {
            for seg_idx in 0..sp.segment_count() {
                let Some(seg) = sp.segment_to_kurbo(seg_idx) else {
                    continue;
                };
                match seg {
                    PathSeg::Line(l) => {
                        push_record_header(out, offsets, next_handle, DWG_ENT_LINE, layer_idx);
                        for v in [l.p0.x, page_h - l.p0.y, l.p1.x, page_h - l.p1.y] {
                            out.extend_from_slice(&v.to_le_bytes());
                        }
                    }
                    PathSeg::Cubic(c) => {
                        push_record_header(out, offsets, next_handle, DWG_ENT_SPLINE, layer_idx);
                        for pt in [c.p0, c.p1, c.p2, c.p3] {
                            out.extend_from_slice(&pt.x.to_le_bytes());
                            out.extend_from_slice(&(page_h - pt.y).to_le_bytes());
                        }
                    }
                    PathSeg::Quad(q) => {
                        let c = q.raise();
                        push_record_header(out, offsets, next_handle, DWG_ENT_SPLINE, layer_idx);
                        for pt in [c.p0, c.p1, c.p2, c.p3] {
                            out.extend_from_slice(&pt.x.to_le_bytes());
                            out.extend_from_slice(&(page_h - pt.y).to_le_bytes());
                        }
                    }
                }
            }
        }
    }
}

#[inline]
fn warp_dwg_world_path(
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
fn emit_dwg_object_records(
    doc: &Document,
    page_h: f64,
    obj: &SceneObject,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<ExportSymCtx<'_>>,
    layer_idx: u16,
    next_handle: &mut u32,
    out: &mut Vec<u8>,
    offsets: &mut Vec<(u32, u32)>,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_DWG_DEPTH || !obj.visible || (obj.id.0 != 0 && !visited.insert(obj.id)) {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        if obj.id.0 != 0 {
            visited.remove(&obj.id);
        }
        return;
    }

    if obj.effects.is_empty() && obj.perspective.is_none() && parent_persp.is_none() {
        if let NodePayload::Shape(ParametricShape::Ellipse {
            rx,
            ry,
            mode,
            start_deg,
            end_deg,
        }) = &obj.payload
        {
            let sx = world_xf.m[0].hypot(world_xf.m[1]);
            let sy = world_xf.m[2].hypot(world_xf.m[3]);
            if (rx - ry).abs() <= 1e-3 * rx.max(1.0) && (sx - sy).abs() <= 1e-3 * sx.max(1.0) {
                let r = rx * 0.5 * (sx + sy);
                let c = world_xf.transform_point(Point2D::ZERO);
                if *mode == EllipseMode::Full && r > 1e-4 {
                    push_record_header(out, offsets, next_handle, DWG_ENT_CIRCLE, layer_idx);
                    for v in [c.x, page_h - c.y, r] {
                        out.extend_from_slice(&v.to_le_bytes());
                    }
                    if obj.id.0 != 0 {
                        visited.remove(&obj.id);
                    }
                    return;
                } else if *mode == EllipseMode::Arc && r > 1e-4 {
                    push_record_header(out, offsets, next_handle, DWG_ENT_ARC, layer_idx);
                    for v in [c.x, page_h - c.y, r, *start_deg, *end_deg] {
                        out.extend_from_slice(&v.to_le_bytes());
                    }
                    if obj.id.0 != 0 {
                        visited.remove(&obj.id);
                    }
                    return;
                }
            }
        }
    }

    match &obj.payload {
        NodePayload::Path(p) => emit_dwg_path(
            page_h,
            &warp_dwg_world_path(p.clone(), obj, world_xf, parent_persp),
            layer_idx,
            next_handle,
            out,
            offsets,
        ),
        NodePayload::Shape(s) => emit_dwg_path(
            page_h,
            &warp_dwg_world_path(s.to_bezier_path(), obj, world_xf, parent_persp),
            layer_idx,
            next_handle,
            out,
            offsets,
        ),
        NodePayload::BSpline(bs) => emit_dwg_path(
            page_h,
            &warp_dwg_world_path(bs.to_bezier_path(), obj, world_xf, parent_persp),
            layer_idx,
            next_handle,
            out,
            offsets,
        ),
        NodePayload::ArtisticMedia { spine, style } => emit_dwg_path(
            page_h,
            &warp_dwg_world_path(
                coree_core::ArtisticMedia::expand_stroke(spine, style),
                obj,
                world_xf,
                parent_persp,
            ),
            layer_idx,
            next_handle,
            out,
            offsets,
        ),
        NodePayload::Table(t) => emit_dwg_path(
            page_h,
            &warp_dwg_world_path(t.to_grid_paths(), obj, world_xf, parent_persp),
            layer_idx,
            next_handle,
            out,
            offsets,
        ),
        NodePayload::Connector(c) => {
            let (a, b) = resolve_export_connector_endpoints(doc, c, &world_xf, sym_ctx);
            emit_dwg_path(
                page_h,
                &c.route_path(a, b),
                layer_idx,
                next_handle,
                out,
                offsets,
            );
        }
        NodePayload::PowerClip { container, .. } => emit_dwg_path(
            page_h,
            &warp_dwg_world_path((**container).clone(), obj, world_xf, parent_persp),
            layer_idx,
            next_handle,
            out,
            offsets,
        ),
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
                emit_dwg_object_records(
                    doc,
                    page_h,
                    &resolved,
                    parent_xf,
                    parent_persp,
                    sym_ctx,
                    layer_idx,
                    next_handle,
                    out,
                    offsets,
                    depth + 1,
                    visited,
                );
            }
        }
        NodePayload::Group(children) => {
            let child_persp =
                compose_world_persp(parent_persp, obj.transform, world_xf, obj.perspective);
            for &cid in children {
                let child_opt = sym_ctx
                    .and_then(|(m, _, _)| m.get(&cid).copied())
                    .or_else(|| doc.object(cid));
                if let Some(child) = child_opt {
                    emit_dwg_object_records(
                        doc,
                        page_h,
                        child,
                        world_xf,
                        child_persp,
                        sym_ctx,
                        layer_idx,
                        next_handle,
                        out,
                        offsets,
                        depth + 1,
                        visited,
                    );
                }
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                let (child_map, referenced) = build_dwg_symbol_maps(sym);
                let mut sym_visited = HashSet::new();
                let child_persp =
                    compose_world_persp(parent_persp, obj.transform, world_xf, obj.perspective);
                for sub in &sym.objects {
                    if sub.id.0 != 0 && referenced.contains(&sub.id) {
                        continue;
                    }
                    emit_dwg_object_records(
                        doc,
                        page_h,
                        sub,
                        world_xf,
                        child_persp,
                        Some((&child_map, &sym.objects, world_xf)),
                        layer_idx,
                        next_handle,
                        out,
                        offsets,
                        depth + 1,
                        &mut sym_visited,
                    );
                }
            }
        }
        NodePayload::ArtisticText(at) => {
            let txt = at.plain_text();
            if !txt.trim().is_empty() {
                let h_mm = at.runs.first().map_or(4.0, |r| r.style.font_size_mm());
                let sy = world_xf.m[2].hypot(world_xf.m[3]).max(1e-4);
                let rot_deg = -world_xf.m[1].atan2(world_xf.m[0]).to_degrees();
                let pos = world_xf.transform_point(Point2D::new(0.0, h_mm));
                push_record_header(out, offsets, next_handle, DWG_ENT_TEXT, layer_idx);
                for v in [pos.x, page_h - pos.y, h_mm * sy, rot_deg] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                write_lp_str(out, &txt);
            }
        }
        NodePayload::ParagraphText(para) => {
            let txt: String = para.runs.iter().map(|r| r.text.as_str()).collect();
            if !txt.trim().is_empty() {
                let h_mm = para.runs.first().map_or(4.0, |r| r.style.font_size_mm());
                let sy = world_xf.m[2].hypot(world_xf.m[3]).max(1e-4);
                let rot_deg = -world_xf.m[1].atan2(world_xf.m[0]).to_degrees();
                let pos = world_xf.transform_point(Point2D::new(0.0, h_mm));
                push_record_header(out, offsets, next_handle, DWG_ENT_TEXT, layer_idx);
                for v in [pos.x, page_h - pos.y, h_mm * sy, rot_deg] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                write_lp_str(out, &txt);
            }
        }
        NodePayload::TextOnPath(top) => {
            let txt = top.text.plain_text();
            if !txt.trim().is_empty() {
                let h_mm = top
                    .text
                    .runs
                    .first()
                    .map_or(4.0, |r| r.style.font_size_mm());
                let pos = world_xf.transform_point(Point2D::ZERO);
                push_record_header(out, offsets, next_handle, DWG_ENT_TEXT, layer_idx);
                for v in [pos.x, page_h - pos.y, h_mm, 0.0] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                write_lp_str(out, &txt);
            }
        }
        NodePayload::Dimension(dim) => {
            let p1 = world_xf.transform_point(dim.p1);
            let p2 = world_xf.transform_point(dim.p2);
            let lbl = dim
                .label_override
                .clone()
                .unwrap_or_else(|| dim.compute_measurement(coree_core::Unit::Millimeter));
            push_record_header(out, offsets, next_handle, DWG_ENT_DIMENSION, layer_idx);
            for v in [p1.x, page_h - p1.y, p2.x, page_h - p2.y, dim.offset_mm] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            write_lp_str(out, &lbl);
        }
        NodePayload::Raster(_) => {}
    }

    if obj.id.0 != 0 {
        visited.remove(&obj.id);
    }
}

fn write_lp_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(u16::MAX as usize);
    out.extend_from_slice(&(len as u16).to_le_bytes());
    out.extend_from_slice(&bytes[..len]);
}

fn read_lp_str(buf: &[u8], off: usize, enc: &'static Encoding) -> Option<(String, usize)> {
    let len_bytes = buf.get(off..off + 2)?;
    let len = usize::from(u16::from_le_bytes([len_bytes[0], len_bytes[1]]));
    let end = off.checked_add(2)?.checked_add(len)?;
    let raw = buf.get(off + 2..end)?;
    Some((decode_dwg_string(raw, enc), end))
}

fn read_f64_le(buf: &[u8], off: usize) -> Option<f64> {
    let sl = buf.get(off..off + 8)?;
    let v = f64::from_le_bytes([sl[0], sl[1], sl[2], sl[3], sl[4], sl[5], sl[6], sl[7]]);
    v.is_finite().then_some(v)
}

fn read_f64_slice<const N: usize>(buf: &[u8], pos: &mut usize) -> Option<[f64; N]> {
    let need = N.checked_mul(8)?;
    let end = pos.checked_add(need)?;
    if end > buf.len() {
        return None;
    }
    let mut out = [0.0; N];
    for (idx, slot) in out.iter_mut().enumerate() {
        *slot = read_f64_le(buf, *pos + idx * 8)?;
    }
    *pos = end;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwg_ac1015_export_import_roundtrips_geometry_text_dimensions_and_arabic() {
        let mut doc = Document::new("DWG Test مشروع", 150.0, 120.0, ColorMode::Rgb, 300.0);
        let pid = doc.pages()[0].id;
        let cut = doc
            .add_layer(pid, "CutContour", true, MasterLayerScope::None)
            .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Ring",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 20.0,
                    ry: 20.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_outline(OutlineStyle::cut_contour())
            .with_transform(Affine2D::translate(50.0, 50.0)),
            Some(cut),
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Label",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "مرحبا CAD",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(20.0, 80.0)),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Dim",
                NodePayload::Dimension(DimensionObject::linear(
                    DimensionKind::Parallel,
                    Point2D::new(10.0, 100.0),
                    Point2D::new(60.0, 100.0),
                    5.0,
                )),
            ),
            None,
        )
        .unwrap();

        let bytes = export_dwg_to_bytes(&doc, 0).unwrap();
        assert!(bytes.starts_with(b"AC1015"));
        let imported = import_dwg_bytes(&bytes, "Roundtrip").unwrap();
        assert!(imported.objects().values().any(|o| matches!(
            o.payload,
            NodePayload::Shape(ParametricShape::Ellipse { .. })
        )));
        assert!(imported
            .objects()
            .values()
            .any(|o| matches!(&o.payload, NodePayload::ArtisticText(t) if t.plain_text().contains("مرحبا"))));
        assert!(imported
            .objects()
            .values()
            .any(|o| matches!(o.payload, NodePayload::Dimension(_))));
    }

    #[test]
    fn dwg_validates_ac1012_to_ac1032_versions_and_rejects_corrupt_crc() {
        let doc = Document::new("Ver", 100.0, 100.0, ColorMode::Rgb, 300.0);
        let mut bytes = export_dwg_to_bytes(&doc, 0).unwrap();
        bytes[30] ^= 0xFF; // corrupt section locator or crc
        assert!(import_dwg_bytes(&bytes, "Corrupt").is_err());

        // Minimal AC1018 (R2004) valid encrypted header probe
        let mut r2004 = vec![0u8; 0x100];
        r2004[..6].copy_from_slice(b"AC1018");
        let mut masked = vec![0u8; 0x6C];
        masked[..12].copy_from_slice(b"AcFssFcAJMB\0");
        let masked_out = unmask_r2004_header(&masked);
        r2004[0x80..0xEC].copy_from_slice(&masked_out);
        let parsed = import_dwg_bytes(&r2004, "R2004").unwrap();
        assert_eq!(parsed.metadata().source_format.as_deref(), Some("dwg"));

        // Invalid AC1018 header >= 0x100 bytes must also be rejected consistently
        let mut bad_r2004 = vec![0u8; 0x200];
        bad_r2004[..6].copy_from_slice(b"AC1018");
        assert!(import_dwg_bytes(&bad_r2004, "BadR2004").is_err());
    }

    #[test]
    fn dwg_exports_and_imports_clones_top_level_and_inside_symbol_instance() {
        let mut doc = Document::new("DWG Clones", 150.0, 120.0, ColorMode::Rgb, 300.0);
        let lid = doc.pages()[0].layers[0].id;

        let top_master_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "TopMaster",
                    NodePayload::Shape(ParametricShape::Ellipse {
                        rx: 8.0,
                        ry: 8.0,
                        mode: EllipseMode::Full,
                        start_deg: 0.0,
                        end_deg: 360.0,
                    }),
                )
                .with_outline(OutlineStyle::solid(Color::BLACK, 0.5))
                .with_transform(Affine2D::translate(20.0, 20.0)),
                Some(lid),
            )
            .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "TopClone",
                NodePayload::Clone {
                    master_id: top_master_id,
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            )
            .with_transform(Affine2D::translate(60.0, 20.0)),
            Some(lid),
        )
        .unwrap();

        let sym_id = coree_core::SymbolId::new();
        let sym_master_id = ObjectId(7101);
        let sym_clone_id = ObjectId(7102);
        let host_id = ObjectId(7103);
        let top_id = ObjectId(7104);

        let sym = SymbolDef {
            id: sym_id,
            name: "CloneSym".into(),
            objects: vec![
                SceneObject::new(
                    sym_master_id,
                    "SymMaster",
                    NodePayload::Shape(ParametricShape::Ellipse {
                        rx: 5.0,
                        ry: 5.0,
                        mode: EllipseMode::Full,
                        start_deg: 0.0,
                        end_deg: 360.0,
                    }),
                )
                .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
                SceneObject::new(
                    sym_clone_id,
                    "SymClone",
                    NodePayload::Clone {
                        master_id: sym_master_id,
                        link_fill: true,
                        link_outline: true,
                        link_shape: true,
                    },
                )
                .with_transform(Affine2D::translate(25.0, 0.0)),
                SceneObject::new(
                    host_id,
                    "SymHostPath",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(0.0, 15.0),
                        Point2D::new(30.0, 15.0),
                    )),
                )
                .with_outline(OutlineStyle::solid(Color::BLACK, 0.25)),
                SceneObject::new(
                    top_id,
                    "SymTOP",
                    NodePayload::TextOnPath(coree_core::TextOnPath {
                        text: ArtisticText::from_plain("OnPath", TextStyle::default()),
                        path_id: host_id,
                        offset_mm: 0.0,
                        distance_mm: 0.0,
                        flip_vertical: false,
                        flip_horizontal: false,
                    }),
                ),
            ],
        };

        let (_, referenced) = build_dwg_symbol_maps(&sym);
        assert!(
            !referenced.contains(&sym_master_id) && !referenced.contains(&host_id),
            "build_dwg_symbol_maps must not hide Clone masters or TextOnPath hosts"
        );

        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(15.0, 70.0)),
            Some(lid),
        )
        .unwrap();

        let bytes = export_dwg_to_bytes(&doc, 0).expect("export DWG with clones");
        let imported = import_dwg_bytes(&bytes, "CloneRoundtrip").expect("import DWG with clones");

        let mut top_xs: Vec<f64> = Vec::new();
        let mut sym_xs: Vec<f64> = Vec::new();
        for o in imported.objects().values() {
            if let NodePayload::Shape(ParametricShape::Ellipse { rx, .. }) = o.payload {
                let x = o.transform.m[4];
                if (rx - 8.0).abs() < 0.1 {
                    top_xs.push(x);
                } else if (rx - 5.0).abs() < 0.1 {
                    sym_xs.push(x);
                }
            }
        }
        top_xs.sort_by(f64::total_cmp);
        sym_xs.sort_by(f64::total_cmp);
        assert_eq!(
            top_xs.len(),
            2,
            "expected top-level master and clone circles, got {:?}",
            top_xs
        );
        assert!((top_xs[0] - 20.0).abs() < 0.1 && (top_xs[1] - 60.0).abs() < 0.1);
        assert_eq!(
            sym_xs.len(),
            2,
            "expected SymbolInstance master and clone circles, got {:?}",
            sym_xs
        );
        assert!((sym_xs[0] - 15.0).abs() < 0.1 && (sym_xs[1] - 40.0).abs() < 0.1);
    }

    #[test]
    fn dwg_chained_symbol_clone_preserves_overrides_and_skips_link_shape_false() {
        let mut doc = Document::new("DwgChainedClone", 160.0, 120.0, ColorMode::Rgb, 300.0);
        let lid = doc.pages()[0].layers[0].id;
        let sym_id = coree_core::SymbolId::new();
        let master_id = ObjectId(7201);
        let clone_id = ObjectId(7202);
        let chained_id = ObjectId(7203);
        let unlinked_id = ObjectId(7204);

        let sym = SymbolDef {
            id: sym_id,
            name: "ChainedDwgSym".into(),
            objects: vec![
                SceneObject::new(
                    master_id,
                    "SymMaster",
                    NodePayload::Shape(ParametricShape::Ellipse {
                        rx: 5.0,
                        ry: 5.0,
                        mode: EllipseMode::Full,
                        start_deg: 0.0,
                        end_deg: 360.0,
                    }),
                )
                .with_outline(OutlineStyle::solid(Color::rgba8(255, 0, 0, 255), 0.5)),
                SceneObject::new(
                    clone_id,
                    "SymClone",
                    NodePayload::Clone {
                        master_id,
                        link_fill: true,
                        link_outline: true,
                        link_shape: true,
                    },
                )
                .with_transform(Affine2D::translate(25.0, 0.0)),
                SceneObject::new(
                    chained_id,
                    "SymChained",
                    NodePayload::Clone {
                        master_id: clone_id,
                        link_fill: true,
                        link_outline: false,
                        link_shape: true,
                    },
                )
                .with_outline(OutlineStyle::solid(Color::rgba8(0, 255, 0, 255), 1.0))
                .with_transform(Affine2D::translate(55.0, 0.0)),
                SceneObject::new(
                    unlinked_id,
                    "SymUnlinked",
                    NodePayload::Clone {
                        master_id,
                        link_fill: true,
                        link_outline: true,
                        link_shape: false,
                    },
                )
                .with_transform(Affine2D::translate(85.0, 0.0)),
            ],
        };
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(15.0, 30.0)),
            Some(lid),
        )
        .unwrap();

        let bytes = export_dwg_to_bytes(&doc, 0).expect("export DWG");
        let imported = import_dwg_bytes(&bytes, "ChainedDwg").expect("import DWG");
        let mut xs: Vec<f64> = Vec::new();
        for o in imported.objects().values() {
            if let NodePayload::Shape(ParametricShape::Ellipse { rx, .. }) = o.payload {
                if (rx - 5.0).abs() < 0.1 {
                    xs.push(o.transform.m[4]);
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        assert_eq!(
            xs.len(),
            3,
            "expected 3 circles (unlinked skipped), got {xs:?}"
        );
        assert!((xs[0] - 15.0).abs() < 0.2);
        assert!((xs[1] - 40.0).abs() < 0.2);
        assert!((xs[2] - 70.0).abs() < 0.2);
    }

    #[test]
    fn dwg_entity_cap_rejects_249k_bomb() {
        let n_lines = 249_999u32;
        let mut sec5 = Vec::with_capacity(8 + (n_lines as usize) * 40);
        sec5.extend_from_slice(COREE_DWG_ENTITY_MAGIC);
        sec5.extend_from_slice(&n_lines.to_le_bytes());
        for _ in 0..n_lines {
            sec5.extend_from_slice(&1u32.to_le_bytes());
            sec5.extend_from_slice(&DWG_ENT_LINE.to_le_bytes());
            sec5.extend_from_slice(&0u16.to_le_bytes());
            for v in [0.0f64, 0.0, 10.0, 10.0] {
                sec5.extend_from_slice(&v.to_le_bytes());
            }
        }
        let off = (25 + 9 + 2 + 16) as u32;
        let mut hdr = Vec::new();
        hdr.extend_from_slice(b"AC1015");
        hdr.resize(19, 0);
        hdr.extend_from_slice(&30u16.to_le_bytes());
        hdr.extend_from_slice(&1u32.to_le_bytes());
        hdr.push(5);
        hdr.extend_from_slice(&off.to_le_bytes());
        hdr.extend_from_slice(&(sec5.len() as u32).to_le_bytes());
        let crc = dwg_crc16(0xC0C1, &hdr);
        let mut dwg = hdr;
        dwg.extend_from_slice(&crc.to_le_bytes());
        dwg.extend_from_slice(&R2000_HDR_SENTINEL);
        dwg.extend_from_slice(&sec5);

        let err = import_dwg_bytes(&dwg, "DwgBomb").unwrap_err();
        assert!(
            matches!(err, IoError::LimitExceeded { .. }),
            "expected LimitExceeded on 249,999 DWG lines, got {err:?}"
        );
    }
}
