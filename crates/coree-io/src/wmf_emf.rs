// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust Windows Metafile (`.wmf`, Aldus Placeable + Standard WMF) and Enhanced Metafile
//! (`.emf`, `[MS-EMF]`) readers and writers.

use crate::atomic::atomic_write_bytes;
use crate::clipboard_codec::{decode_clipboard_emf, encode_clipboard_emf};
use crate::display_list::{DisplayListOptions, DisplayOp, ExportDisplayList};
use crate::error::{IoError, IoResult, DEFAULT_MAX_SVG_BYTES};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, Document, EllipseMode, Fill, NodePayload,
    ObjectId, OutlineStyle, ParametricShape, Point2D, Rect2D, SceneObject, TextStyle,
};
use coree_render::TextEngine;
use std::io::Read;
use std::path::Path;

const MAX_METAFILE_BYTES: usize = DEFAULT_MAX_SVG_BYTES * 2; // 128 MiB
const MAX_WMF_RECORDS: usize = 500_000;
const ALDUS_KEY: u32 = 0x9AC6_CDD7;
const TWIPS_PER_INCH: f64 = 1440.0;
const MM_PER_INCH: f64 = 25.4;
const MM_TO_TWIPS: f64 = TWIPS_PER_INCH / MM_PER_INCH;

// Standard WMF record function codes
const META_EOF: u16 = 0x0000;
const META_SETMAPMODE: u16 = 0x0103;
const META_SETWINDOWORG: u16 = 0x020B;
const META_SETWINDOWEXT: u16 = 0x020C;
const META_SELECTOBJECT: u16 = 0x012D;
const META_DELETEOBJECT: u16 = 0x01F0;
const META_LINETO: u16 = 0x0213;
const META_MOVETO: u16 = 0x0214;
const META_CREATEPENINDIRECT: u16 = 0x02FA;
const META_CREATEBRUSHINDIRECT: u16 = 0x02FC;
const META_POLYGON: u16 = 0x0324;
const META_POLYLINE: u16 = 0x0325;
const META_ELLIPSE: u16 = 0x0418;
const META_RECTANGLE: u16 = 0x041B;
const META_TEXTOUT: u16 = 0x0521;
const META_POLYPOLYGON: u16 = 0x0538;
const META_EXTTEXTOUT: u16 = 0x0A32;
const META_STRETCHDIB: u16 = 0x0F43;

// =============================================================================
// Enhanced Metafile (.emf) — [MS-EMF]
// =============================================================================

/// Exports `page_index` of `doc` to `[MS-EMF]` binary `.emf` bytes (`n_records >= 3`).
pub fn export_emf_to_bytes(doc: &Document, page_index: usize) -> IoResult<Vec<u8>> {
    let bytes = encode_clipboard_emf(doc, page_index)?;
    validate_emf_header(&bytes)?;
    Ok(bytes)
}

/// Atomically exports `page_index` of `doc` to a binary `.emf` file at `path`.
pub fn export_emf(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    let bytes = export_emf_to_bytes(doc, page_index)?;
    atomic_write_bytes(path, &bytes)
}

/// Imports an `[MS-EMF]` `.emf` file from `path` into a [`Document`].
pub fn import_emf(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_METAFILE_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_METAFILE_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_emf_bytes={MAX_METAFILE_BYTES}"),
            actual: format!("file exceeds {MAX_METAFILE_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported EMF");
    import_emf_bytes(&bytes, title)
}

/// Imports `[MS-EMF]` `.emf` bytes into a [`Document`].
pub fn import_emf_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    validate_emf_header(bytes)?;
    let mut doc = decode_clipboard_emf(bytes)?;
    if !title.trim().is_empty() {
        doc.set_title(title.to_owned());
    }
    doc.metadata_mut().source_format = Some("emf".to_owned());
    Ok(doc)
}

fn validate_emf_header(bytes: &[u8]) -> IoResult<()> {
    if bytes.len() < 88 {
        return Err(IoError::InvalidFormat {
            format: "emf".into(),
            message: "EMF stream shorter than 88-byte EMR_HEADER".into(),
        });
    }
    let rec_type = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let rec_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let sig = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);
    let n_records = u32::from_le_bytes([bytes[52], bytes[53], bytes[54], bytes[55]]);
    if rec_type != 1 || rec_size < 88 || sig != 0x464D_4520 || n_records < 2 {
        return Err(IoError::InvalidFormat {
            format: "emf".into(),
            message: format!(
                "invalid EMR_HEADER (type={rec_type}, size={rec_size}, sig={sig:#010x}, records={n_records})"
            ),
        });
    }
    Ok(())
}

// =============================================================================
// Windows Metafile (.wmf) — Aldus Placeable + Standard WMF
// =============================================================================

/// Exports `page_index` of `doc` to an Aldus Placeable `.wmf` byte buffer.
pub fn export_wmf_to_bytes(doc: &Document, page_index: usize) -> IoResult<Vec<u8>> {
    let mut text_engine = TextEngine::new();
    let dl = ExportDisplayList::build(
        doc,
        page_index,
        &DisplayListOptions::eps(),
        &mut text_engine,
    )?;
    let page_w = if dl.width_mm.is_finite() && dl.width_mm > 0.0 {
        dl.width_mm
    } else {
        210.0
    };
    let page_h = if dl.height_mm.is_finite() && dl.height_mm > 0.0 {
        dl.height_mm
    } else {
        297.0
    };

    let w_twips = (page_w * MM_TO_TWIPS).round().clamp(1.0, i16::MAX as f64) as i16;
    let h_twips = (page_h * MM_TO_TWIPS).round().clamp(1.0, i16::MAX as f64) as i16;

    let mut recs: Vec<u8> = Vec::with_capacity(2048);
    let mut max_record_words: u32 = 3;
    let mut n_objects: u16 = 0;

    // META_SETMAPMODE (MM_ANISOTROPIC = 8)
    push_wmf_record(&mut recs, META_SETMAPMODE, &[8], &mut max_record_words);
    // META_SETWINDOWORG (0, 0)
    push_wmf_record(&mut recs, META_SETWINDOWORG, &[0, 0], &mut max_record_words);
    // META_SETWINDOWEXT (h, w) — WMF args are pushed in reverse order (y, x)
    push_wmf_record(
        &mut recs,
        META_SETWINDOWEXT,
        &[h_twips as u16, w_twips as u16],
        &mut max_record_words,
    );

    for op in &dl.ops {
        match op {
            DisplayOp::DrawVector {
                path,
                fill,
                outline,
                visible,
                ..
            } if *visible => {
                emit_wmf_path(
                    path,
                    fill,
                    outline,
                    &mut recs,
                    &mut max_record_words,
                    &mut n_objects,
                );
            }
            DisplayOp::DrawTextRuns {
                fallback_curves,
                outline,
                visible,
                ..
            } if *visible => {
                for piece in fallback_curves {
                    emit_wmf_path(
                        &piece.path,
                        &Fill::Solid(piece.fill.clone()),
                        outline,
                        &mut recs,
                        &mut max_record_words,
                        &mut n_objects,
                    );
                }
            }
            DisplayOp::DrawRaster {
                asset_id,
                inline_rgba,
                rect_mm,
                bl,
                br,
                tl,
                opacity,
                visible,
                ..
            } if *visible => {
                let src_rgba = if let Some(arc) = inline_rgba {
                    Some((arc.0, arc.1, arc.2.clone()))
                } else if let Some(aid) = asset_id {
                    doc.asset(*aid)
                        .map(|a| (a.width, a.height, a.to_rgba8_vec()))
                } else {
                    None
                };
                if let Some((rw, rh, rgba)) = src_rgba {
                    let tr = Point2D::new(br.x + (tl.x - bl.x), br.y + (tl.y - bl.y));
                    let rect = rect_mm.unwrap_or_else(|| {
                        let min_x = bl.x.min(br.x).min(tl.x).min(tr.x);
                        let min_y = bl.y.min(br.y).min(tl.y).min(tr.y);
                        let max_x = bl.x.max(br.x).max(tl.x).max(tr.x);
                        let max_y = bl.y.max(br.y).max(tl.y).max(tr.y);
                        Rect2D::from_xywh(min_x, min_y, max_x - min_x, max_y - min_y)
                    });
                    emit_wmf_raster_dib(
                        rw,
                        rh,
                        &rgba,
                        *opacity,
                        rect,
                        &mut recs,
                        &mut max_record_words,
                    );
                }
            }
            _ => {}
        }
    }

    // META_EOF (size = 3 words, fn = 0x0000)
    push_wmf_record(&mut recs, META_EOF, &[], &mut max_record_words);

    // 22-byte Aldus Placeable Header + 18-byte Standard WMF Header
    let mut out = Vec::with_capacity(40 + recs.len());
    let mut aldus_words: [u16; 10] = [
        (ALDUS_KEY & 0xFFFF) as u16,
        ((ALDUS_KEY >> 16) & 0xFFFF) as u16,
        0,              // hmf
        0,              // left
        0,              // top
        w_twips as u16, // right
        h_twips as u16, // bottom
        TWIPS_PER_INCH as u16,
        0, // reserved low
        0, // reserved high
    ];
    let checksum = aldus_words.iter().fold(0u16, |acc, &w| acc ^ w);
    for w in aldus_words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(&checksum.to_le_bytes());

    let mt_size_words = (18 + recs.len()) / 2;
    out.extend_from_slice(&1u16.to_le_bytes()); // mtType = 1 (MEMORYMETAFILE)
    out.extend_from_slice(&9u16.to_le_bytes()); // mtHeaderSize = 9 words
    out.extend_from_slice(&0x0300u16.to_le_bytes()); // mtVersion = 0x0300
    out.extend_from_slice(&(mt_size_words as u32).to_le_bytes());
    out.extend_from_slice(&n_objects.max(2).to_le_bytes());
    out.extend_from_slice(&max_record_words.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // mtNoParameters
    out.extend_from_slice(&recs);
    let _ = &mut aldus_words;
    Ok(out)
}

/// Atomically exports `page_index` of `doc` to an Aldus Placeable `.wmf` file at `path`.
pub fn export_wmf(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    let bytes = export_wmf_to_bytes(doc, page_index)?;
    atomic_write_bytes(path, &bytes)
}

/// Imports a `.wmf` (Aldus Placeable or standard WMF) file from `path` into a [`Document`].
pub fn import_wmf(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_METAFILE_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_METAFILE_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_wmf_bytes={MAX_METAFILE_BYTES}"),
            actual: format!("file exceeds {MAX_METAFILE_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported WMF");
    import_wmf_bytes(&bytes, title)
}

#[derive(Debug, Clone)]
enum WmfGdiObj {
    Pen(OutlineStyle),
    Brush(Fill),
}

/// Imports a `.wmf` byte buffer (Aldus Placeable or standard WMF) into a [`Document`].
pub fn import_wmf_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    let mut off = 0usize;
    let mut inch = TWIPS_PER_INCH;
    let mut aldus_bounds: Option<(i16, i16, i16, i16)> = None;

    if bytes.len() >= 22 {
        let key = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if key == ALDUS_KEY {
            let left = i16::from_le_bytes([bytes[6], bytes[7]]);
            let top = i16::from_le_bytes([bytes[8], bytes[9]]);
            let right = i16::from_le_bytes([bytes[10], bytes[11]]);
            let bottom = i16::from_le_bytes([bytes[12], bytes[13]]);
            let raw_inch = u16::from_le_bytes([bytes[14], bytes[15]]);
            if raw_inch > 0 {
                inch = f64::from(raw_inch);
            }
            let mut sum = 0u16;
            for c in bytes[0..20].as_chunks::<2>().0 {
                sum ^= u16::from_le_bytes(*c);
            }
            let expected = u16::from_le_bytes([bytes[20], bytes[21]]);
            if sum != expected {
                return Err(IoError::InvalidFormat {
                    format: "wmf".into(),
                    message: format!(
                        "Aldus Placeable WMF checksum mismatch (expected {expected:#06x}, got {sum:#06x})"
                    ),
                });
            }
            aldus_bounds = Some((left, top, right, bottom));
            off = 22;
        }
    }

    if bytes.len() < off + 18 {
        return Err(IoError::InvalidFormat {
            format: "wmf".into(),
            message: "WMF stream shorter than 18-byte METAHEADER".into(),
        });
    }
    let mt_type = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
    let mt_header_words = u16::from_le_bytes([bytes[off + 2], bytes[off + 3]]) as usize;
    if !(mt_type == 1 || mt_type == 2) || mt_header_words < 9 {
        return Err(IoError::InvalidFormat {
            format: "wmf".into(),
            message: format!(
                "invalid WMF header (mtType={mt_type}, mtHeaderSize={mt_header_words})"
            ),
        });
    }
    off += mt_header_words * 2;
    if off > bytes.len() {
        return Err(IoError::InvalidFormat {
            format: "wmf".into(),
            message: "truncated WMF header".into(),
        });
    }

    let unit_to_mm = MM_PER_INCH / inch.max(1.0);
    let (mut win_ox, mut win_oy) =
        aldus_bounds.map_or((0.0, 0.0), |(l, t, _, _)| (f64::from(l), f64::from(t)));
    let (mut win_ex, mut win_ey) =
        aldus_bounds.map_or((210.0 / unit_to_mm, 297.0 / unit_to_mm), |(l, t, r, b)| {
            (
                (f64::from(r) - f64::from(l)).max(1.0),
                (f64::from(b) - f64::from(t)).max(1.0),
            )
        });
    let page_w = (win_ex * unit_to_mm).clamp(10.0, 20_000.0);
    let page_h = (win_ey * unit_to_mm).clamp(10.0, 20_000.0);

    let mut gdi_table: Vec<Option<WmfGdiObj>> = Vec::new();
    let mut cur_outline = OutlineStyle::solid(Color::BLACK, 0.25);
    let mut cur_fill = Fill::None;
    let mut cur_pos = Point2D::ZERO;

    let mut emitted_objs: Vec<SceneObject> = Vec::new();
    let mut pending_rasters: Vec<(coree_core::RasterAsset, Rect2D)> = Vec::new();
    let mut rec_count = 0usize;

    let map_pt = |lx: i16, ly: i16, ox: f64, oy: f64, ex: f64, ey: f64| -> Point2D {
        let sx = page_w / ex.abs().max(1.0);
        let sy = page_h / ey.abs().max(1.0);
        Point2D::new((f64::from(lx) - ox) * sx, (f64::from(ly) - oy) * sy)
    };

    while off + 6 <= bytes.len() {
        rec_count += 1;
        if rec_count > MAX_WMF_RECORDS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_wmf_records={MAX_WMF_RECORDS}"),
                actual: "WMF record count exceeded".into(),
            });
        }
        let words = u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
            as usize;
        let func = u16::from_le_bytes([bytes[off + 4], bytes[off + 5]]);
        if words < 3 {
            return Err(IoError::InvalidFormat {
                format: "wmf".into(),
                message: format!("invalid WMF record size {words} words"),
            });
        }
        let byte_len = words.saturating_mul(2);
        let next_off = off
            .checked_add(byte_len)
            .ok_or_else(|| IoError::InvalidFormat {
                format: "wmf".into(),
                message: "WMF record offset overflow".into(),
            })?;
        if next_off > bytes.len() {
            break;
        }
        let params = &bytes[off + 6..next_off];
        off = next_off;

        if func == META_EOF {
            break;
        }
        match func {
            META_SETWINDOWORG if params.len() >= 4 => {
                win_oy = f64::from(read_i16_le(params, 0));
                win_ox = f64::from(read_i16_le(params, 2));
            }
            META_SETWINDOWEXT if params.len() >= 4 => {
                win_ey = f64::from(read_i16_le(params, 0));
                win_ex = f64::from(read_i16_le(params, 2));
            }
            META_CREATEPENINDIRECT if params.len() >= 10 => {
                let style = read_u16_le(params, 0);
                let width_logical = f64::from(read_i16_le(params, 2).abs()).max(1.0);
                let col = Color::rgba8(params[6], params[7], params[8], 255);
                let outline = if style == 5 {
                    OutlineStyle::none()
                } else {
                    OutlineStyle::solid(col, (width_logical * unit_to_mm).clamp(0.1, 25.0))
                };
                insert_wmf_gdi(&mut gdi_table, WmfGdiObj::Pen(outline));
            }
            META_CREATEBRUSHINDIRECT if params.len() >= 8 => {
                let style = read_u16_le(params, 0);
                let col = Color::rgba8(params[2], params[3], params[4], 255);
                let fill = if style == 1 {
                    Fill::None
                } else {
                    Fill::Solid(col)
                };
                insert_wmf_gdi(&mut gdi_table, WmfGdiObj::Brush(fill));
            }
            META_SELECTOBJECT if params.len() >= 2 => {
                let idx = usize::from(read_u16_le(params, 0));
                if let Some(Some(obj)) = gdi_table.get(idx) {
                    match obj {
                        WmfGdiObj::Pen(o) => cur_outline = o.clone(),
                        WmfGdiObj::Brush(f) => cur_fill = f.clone(),
                    }
                }
            }
            META_DELETEOBJECT if params.len() >= 2 => {
                let idx = usize::from(read_u16_le(params, 0));
                if let Some(slot) = gdi_table.get_mut(idx) {
                    *slot = None;
                }
            }
            META_MOVETO if params.len() >= 4 => {
                let y = read_i16_le(params, 0);
                let x = read_i16_le(params, 2);
                cur_pos = map_pt(x, y, win_ox, win_oy, win_ex, win_ey);
            }
            META_LINETO if params.len() >= 4 => {
                let y = read_i16_le(params, 0);
                let x = read_i16_le(params, 2);
                let next = map_pt(x, y, win_ox, win_oy, win_ex, win_ey);
                let obj = SceneObject::new(
                    ObjectId(0),
                    "WMF Line",
                    NodePayload::Path(BezierPath::from_line(cur_pos, next)),
                )
                .with_fill(Fill::None)
                .with_outline(cur_outline.clone());
                emitted_objs.push(obj);
                cur_pos = next;
            }
            META_POLYGON | META_POLYLINE if params.len() >= 2 => {
                let count = usize::from(read_u16_le(params, 0)).min(100_000);
                let mut pts = Vec::with_capacity(count);
                for i in 0..count {
                    let p_off = 2 + i * 4;
                    if p_off + 4 > params.len() {
                        break;
                    }
                    let px = read_i16_le(params, p_off);
                    let py = read_i16_le(params, p_off + 2);
                    pts.push(map_pt(px, py, win_ox, win_oy, win_ex, win_ey));
                }
                if pts.len() >= 2 {
                    let closed = func == META_POLYGON;
                    let fill = if closed { cur_fill.clone() } else { Fill::None };
                    let obj = SceneObject::new(
                        ObjectId(0),
                        if closed {
                            "WMF Polygon"
                        } else {
                            "WMF Polyline"
                        },
                        NodePayload::Path(BezierPath::from_polygon(pts, closed)),
                    )
                    .with_fill(fill)
                    .with_outline(cur_outline.clone());
                    emitted_objs.push(obj);
                }
            }
            META_POLYPOLYGON if params.len() >= 2 => {
                let n_polys = usize::from(read_u16_le(params, 0)).min(10_000);
                let counts_end = 2 + n_polys * 2;
                if counts_end <= params.len() {
                    let mut p_off = counts_end;
                    let mut bp = BezierPath::new();
                    for pi in 0..n_polys {
                        let cnt = usize::from(read_u16_le(params, 2 + pi * 2)).min(50_000);
                        let mut pts = Vec::with_capacity(cnt);
                        for _ in 0..cnt {
                            if p_off + 4 > params.len() {
                                break;
                            }
                            let px = read_i16_le(params, p_off);
                            let py = read_i16_le(params, p_off + 2);
                            p_off += 4;
                            pts.push(map_pt(px, py, win_ox, win_oy, win_ex, win_ey));
                        }
                        if pts.len() >= 2 {
                            bp.subpaths
                                .push(coree_core::Subpath::from_points(pts, true));
                        }
                    }
                    if !bp.is_empty() {
                        emitted_objs.push(
                            SceneObject::new(ObjectId(0), "WMF PolyPolygon", NodePayload::Path(bp))
                                .with_fill(cur_fill.clone())
                                .with_outline(cur_outline.clone()),
                        );
                    }
                }
            }
            META_RECTANGLE if params.len() >= 8 => {
                let b = read_i16_le(params, 0);
                let r = read_i16_le(params, 2);
                let t = read_i16_le(params, 4);
                let l = read_i16_le(params, 6);
                let p0 = map_pt(l, t, win_ox, win_oy, win_ex, win_ey);
                let p1 = map_pt(r, b, win_ox, win_oy, win_ex, win_ey);
                let rect = Rect2D::from_points(p0, p1);
                emitted_objs.push(
                    SceneObject::new(
                        ObjectId(0),
                        "WMF Rect",
                        NodePayload::Path(BezierPath::from_rect(rect)),
                    )
                    .with_fill(cur_fill.clone())
                    .with_outline(cur_outline.clone()),
                );
            }
            META_ELLIPSE if params.len() >= 8 => {
                let b = read_i16_le(params, 0);
                let r = read_i16_le(params, 2);
                let t = read_i16_le(params, 4);
                let l = read_i16_le(params, 6);
                let p0 = map_pt(l, t, win_ox, win_oy, win_ex, win_ey);
                let p1 = map_pt(r, b, win_ox, win_oy, win_ex, win_ey);
                let rect = Rect2D::from_points(p0, p1);
                let shape = ParametricShape::Ellipse {
                    rx: (rect.width() * 0.5).max(0.05),
                    ry: (rect.height() * 0.5).max(0.05),
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                };
                emitted_objs.push(
                    SceneObject::new(ObjectId(0), "WMF Ellipse", NodePayload::Shape(shape))
                        .with_fill(cur_fill.clone())
                        .with_outline(cur_outline.clone())
                        .with_transform(Affine2D::translate(
                            rect.min_x + rect.width() * 0.5,
                            rect.min_y + rect.height() * 0.5,
                        )),
                );
            }
            META_TEXTOUT | META_EXTTEXTOUT if func == META_TEXTOUT && params.len() >= 2 => {
                let len = usize::from(read_u16_le(params, 0));
                if 2 + len <= params.len() {
                    let txt = String::from_utf8_lossy(&params[2..2 + len]).into_owned();
                    if !txt.trim().is_empty() && 2 + len + (len & 1) + 4 <= params.len() {
                        let y_off = 2 + len + (len & 1);
                        let y = read_i16_le(params, y_off);
                        let x = read_i16_le(params, y_off + 2);
                        let pt = map_pt(x, y, win_ox, win_oy, win_ex, win_ey);
                        emitted_objs.push(
                            SceneObject::new(
                                ObjectId(0),
                                "WMF Text",
                                NodePayload::ArtisticText(ArtisticText::from_plain(
                                    txt,
                                    TextStyle::default(),
                                )),
                            )
                            .with_transform(Affine2D::translate(pt.x, pt.y)),
                        );
                    }
                }
            }
            META_STRETCHDIB if params.len() >= 22 + 40 => {
                // params[0..2]=colorUsage, [2..6]=rop, [6..14]=src(h,w,y,x), [14..22]=dst(h,w,y,x), [22..]=BITMAPINFO+bits
                let dh = read_i16_le(params, 14);
                let dw = read_i16_le(params, 16);
                let dy = read_i16_le(params, 18);
                let dx = read_i16_le(params, 20);
                let p0 = map_pt(dx, dy, win_ox, win_oy, win_ex, win_ey);
                let p1 = map_pt(
                    dx.saturating_add(dw),
                    dy.saturating_add(dh),
                    win_ox,
                    win_oy,
                    win_ex,
                    win_ey,
                );
                let rect = Rect2D::from_points(p0, p1);
                if let Some(asset) = parse_dib_24bpp_to_asset(&params[22..]) {
                    pending_rasters.push((asset, rect));
                }
            }
            _ => {}
        }
    }

    let mut doc = Document::new(title, page_w, page_h, ColorMode::Rgb, 96.0);
    doc.metadata_mut().source_format = Some("wmf".to_owned());
    for obj in emitted_objs {
        doc.add_object(obj, None)?;
    }
    for (idx, (asset, rect)) in pending_rasters.into_iter().enumerate() {
        let asset_id = doc.register_asset(asset);
        let obj = SceneObject::new(
            ObjectId(0),
            format!("WMF Raster {}", idx + 1),
            NodePayload::Raster(coree_core::RasterLayerData {
                asset_id,
                width_mm: rect.width().max(0.1),
                height_mm: rect.height().max(0.1),
                filters: Vec::new(),
                filter_enabled: Vec::new(),
                crop_rect: None,
            }),
        )
        .with_transform(Affine2D::translate(rect.min_x, rect.min_y));
        doc.add_object(obj, None)?;
    }
    doc.refresh_document_palette();
    Ok(doc)
}

fn insert_wmf_gdi(table: &mut Vec<Option<WmfGdiObj>>, obj: WmfGdiObj) -> u16 {
    if let Some(pos) = table.iter().position(Option::is_none) {
        table[pos] = Some(obj);
        pos as u16
    } else {
        table.push(Some(obj));
        (table.len() - 1) as u16
    }
}

fn push_wmf_record(out: &mut Vec<u8>, func: u16, words: &[u16], max_words: &mut u32) {
    let total_words = (3 + words.len()) as u32;
    *max_words = (*max_words).max(total_words);
    out.extend_from_slice(&total_words.to_le_bytes());
    out.extend_from_slice(&func.to_le_bytes());
    for &w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
}

fn emit_wmf_path(
    path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    recs: &mut Vec<u8>,
    max_words: &mut u32,
    n_objects: &mut u16,
) {
    if path.is_empty() {
        return;
    }
    let fill_col = match fill {
        Fill::Solid(c) if c.alpha() > 1e-4 => Some(c.to_rgba8()),
        Fill::LinearGradient { stops, .. } | Fill::RadialGradient { stops, .. } => {
            stops.first().map(|s| s.color.to_rgba8())
        }
        _ => None,
    };
    let pen_style: u16 = if outline.is_visible() { 0 } else { 5 };
    let pen_w = ((outline.effective_width_mm() * MM_TO_TWIPS).round() as i16).max(1) as u16;
    let pen_col = outline.color.to_rgba8();

    // Pen slot 0
    push_wmf_record(
        recs,
        META_CREATEPENINDIRECT,
        &[
            pen_style,
            pen_w,
            0,
            u16::from(pen_col[0]) | (u16::from(pen_col[1]) << 8),
            u16::from(pen_col[2]),
        ],
        max_words,
    );
    push_wmf_record(recs, META_SELECTOBJECT, &[0], max_words);

    // Brush slot 1
    let (bs_style, b_r, b_g, b_b) = match fill_col {
        Some([r, g, b, _]) => (0u16, r, g, b),
        None => (1u16, 0, 0, 0),
    };
    push_wmf_record(
        recs,
        META_CREATEBRUSHINDIRECT,
        &[
            bs_style,
            u16::from(b_r) | (u16::from(b_g) << 8),
            u16::from(b_b),
            0,
        ],
        max_words,
    );
    push_wmf_record(recs, META_SELECTOBJECT, &[1], max_words);
    *n_objects = (*n_objects).max(2);

    for sp in &path.subpaths {
        let polys = BezierPath {
            subpaths: vec![sp.clone()],
        }
        .flatten(0.15);
        for pts in polys {
            if pts.len() < 2 {
                continue;
            }
            let capped = &pts[..pts.len().min(10_000)];
            let mut words = Vec::with_capacity(1 + capped.len() * 2);
            words.push(capped.len() as u16);
            for pt in capped {
                let lx = (pt.x * MM_TO_TWIPS)
                    .round()
                    .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                let ly = (pt.y * MM_TO_TWIPS)
                    .round()
                    .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                words.push(lx as u16);
                words.push(ly as u16);
            }
            let func = if sp.closed && fill_col.is_some() {
                META_POLYGON
            } else {
                META_POLYLINE
            };
            push_wmf_record(recs, func, &words, max_words);
        }
    }

    push_wmf_record(recs, META_DELETEOBJECT, &[1], max_words);
    push_wmf_record(recs, META_DELETEOBJECT, &[0], max_words);
}

fn emit_wmf_raster_dib(
    src_w: u32,
    src_h: u32,
    rgba: &[u8],
    opacity: f32,
    rect_mm: Rect2D,
    recs: &mut Vec<u8>,
    max_words: &mut u32,
) {
    if src_w == 0 || src_h == 0 || rgba.len() < (src_w as usize) * (src_h as usize) * 4 {
        return;
    }
    let max_dim = 128u32;
    let scale = (src_w.max(src_h) as f64 / f64::from(max_dim)).max(1.0);
    let dst_w = ((src_w as f64 / scale).round() as u32).clamp(1, max_dim);
    let dst_h = ((src_h as f64 / scale).round() as u32).clamp(1, max_dim);
    let stride = (dst_w as usize * 3 + 3) & !3;
    let img_bytes = stride * dst_h as usize;

    let x_tw = (rect_mm.min_x * MM_TO_TWIPS)
        .round()
        .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
    let y_tw = (rect_mm.min_y * MM_TO_TWIPS)
        .round()
        .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
    let w_tw = (rect_mm.width() * MM_TO_TWIPS)
        .round()
        .clamp(1.0, i16::MAX as f64) as i16;
    let h_tw = (rect_mm.height() * MM_TO_TWIPS)
        .round()
        .clamp(1.0, i16::MAX as f64) as i16;

    let mut words: Vec<u16> = Vec::with_capacity(11 + 20 + img_bytes / 2);
    // ColorUsage = 0 (DIB_RGB_COLORS), RasterOperation = 0x00CC0020 (SRCCOPY, LE u32)
    words.push(0x0000);
    words.push(0x0020);
    words.push(0x00CC);
    words.push((dst_h as i16) as u16);
    words.push((dst_w as i16) as u16);
    words.push(0); // YSrc
    words.push(0); // XSrc
    words.push(h_tw as u16);
    words.push(w_tw as u16);
    words.push(y_tw as u16);
    words.push(x_tw as u16);

    let mut dib = Vec::with_capacity(40 + img_bytes);
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&(dst_w as i32).to_le_bytes());
    dib.extend_from_slice(&(dst_h as i32).to_le_bytes()); // bottom-up
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&(img_bytes as u32).to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());

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
            row[out_off] = blend(rgba[idx + 2]); // B
            row[out_off + 1] = blend(rgba[idx + 1]); // G
            row[out_off + 2] = blend(rgba[idx]); // R
        }
        dib.extend_from_slice(&row);
    }
    for &pair in dib.as_chunks::<2>().0 {
        words.push(u16::from_le_bytes(pair));
    }
    push_wmf_record(recs, META_STRETCHDIB, &words, max_words);
}

pub(crate) fn parse_dib_24bpp_to_asset(dib: &[u8]) -> Option<coree_core::RasterAsset> {
    if dib.len() < 40 {
        return None;
    }
    let bi_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) as usize;
    if bi_size < 40 || bi_size > dib.len() {
        return None;
    }
    let bw = i32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]);
    let bh = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]);
    let bpp = u16::from_le_bytes([dib[14], dib[15]]);
    let comp = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    if bw <= 0 || bh == 0 || bpp != 24 || comp != 0 {
        return None;
    }
    let w = (bw as u32).min(4096);
    let h = bh.unsigned_abs().min(4096);
    let bottom_up = bh > 0;
    let stride = (w as usize * 3 + 3) & !3;
    let bits = &dib[bi_size..];
    if bits.len() < stride * h as usize {
        return None;
    }
    let mut rgba = vec![255u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        let src_y = if bottom_up { h as usize - 1 - y } else { y };
        let row = &bits[src_y * stride..src_y * stride + w as usize * 3];
        for x in 0..w as usize {
            let b = row[x * 3];
            let g = row[x * 3 + 1];
            let r = row[x * 3 + 2];
            let dst = (y * w as usize + x) * 4;
            rgba[dst] = r;
            rgba[dst + 1] = g;
            rgba[dst + 2] = b;
            rgba[dst + 3] = 255;
        }
    }
    coree_core::RasterAsset::new(w, h, rgba, 96.0, ColorMode::Rgb).ok()
}

fn read_u16_le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn read_i16_le(buf: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([buf[off], buf[off + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmf_aldus_and_emf_export_import_roundtrip() {
        let mut doc = Document::new("Metafile Test", 120.0, 90.0, ColorMode::Rgb, 96.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Box",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 15.0, 40.0, 25.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgba8(20, 120, 220, 255)))
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
            None,
        )
        .unwrap();

        let wmf = export_wmf_to_bytes(&doc, 0).unwrap();
        assert_eq!(
            u32::from_le_bytes([wmf[0], wmf[1], wmf[2], wmf[3]]),
            ALDUS_KEY
        );
        let wmf_doc = import_wmf_bytes(&wmf, "WMF").unwrap();
        assert!(!wmf_doc.objects().is_empty());

        let emf = export_emf_to_bytes(&doc, 0).unwrap();
        let n_records = u32::from_le_bytes([emf[52], emf[53], emf[54], emf[55]]);
        assert!(n_records >= 3);
        let emf_doc = import_emf_bytes(&emf, "EMF").unwrap();
        assert!(!emf_doc.objects().is_empty());
    }

    #[test]
    fn emf_minimal_two_record_stream_imports_cleanly() {
        let mut minimal = vec![0u8; 108];
        minimal[0..4].copy_from_slice(&1u32.to_le_bytes()); // EMR_HEADER
        minimal[4..8].copy_from_slice(&88u32.to_le_bytes());
        minimal[16..20].copy_from_slice(&1000i32.to_le_bytes());
        minimal[20..24].copy_from_slice(&1000i32.to_le_bytes());
        minimal[32..36].copy_from_slice(&10000i32.to_le_bytes());
        minimal[36..40].copy_from_slice(&10000i32.to_le_bytes());
        minimal[40..44].copy_from_slice(b" EMF");
        minimal[44..48].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        minimal[48..52].copy_from_slice(&108u32.to_le_bytes());
        minimal[52..56].copy_from_slice(&2u32.to_le_bytes()); // nRecords = 2 (HEADER + EOF)
        minimal[88..92].copy_from_slice(&14u32.to_le_bytes()); // EMR_EOF
        minimal[92..96].copy_from_slice(&20u32.to_le_bytes());
        minimal[100..104].copy_from_slice(&16u32.to_le_bytes());
        minimal[104..108].copy_from_slice(&20u32.to_le_bytes());

        let doc = import_emf_bytes(&minimal, "Minimal EMF").unwrap();
        assert_eq!(doc.pages().len(), 1);
    }
}
