// SPDX-License-Identifier: MPL-2.0

//! Pure-Rust Local Font Manager scanner & SQLite `font_index` repository.
//!
//! Scans standard OS font folders (Linux/Windows/macOS) and custom user folders,
//! inspects `.ttf` / `.otf` / `.ttc` SFNT binary tables (`name`, `OS/2`, `cmap`, `fvar`)
//! without any C dependencies to detect **Arabic/RTL** and **Kashida (`U+0640`)** support,
//! indexes faces into SQLite `font_index`, and serves fast filtered queries.

use crate::db::StorageDb;
use crate::error::{StorageError, StorageResult};
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use uuid::Uuid;

const MAX_FONT_SCAN_DEPTH: usize = 8;
const MAX_FONT_FILE_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB guard

/// Row stored in `font_index`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontRecord {
    pub id: String,
    pub file_path: String,
    pub face_index: u32,
    pub family_name: String,
    pub subfamily_name: String,
    pub postscript_name: Option<String>,
    pub weight: u16,
    pub italic: bool,
    pub is_variable: bool,
    pub supports_arabic_rtl: bool,
    pub supports_kashida: bool,
    pub collection_tag: String,
    pub favorite: bool,
    pub file_mtime_epoch: i64,
}

/// Metadata extracted from a single SFNT face inside a `.ttf`, `.otf`, or `.ttc` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFontFace {
    pub face_index: u32,
    pub family_name: String,
    pub subfamily_name: String,
    pub postscript_name: Option<String>,
    pub weight: u16,
    pub italic: bool,
    pub is_variable: bool,
    pub supports_arabic_rtl: bool,
    pub supports_kashida: bool,
}

/// Filter parameters for [`StorageDb::search_fonts`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontQuery {
    pub family_query: Option<String>,
    pub arabic_only: bool,
    pub kashida_only: bool,
    pub favorites_only: bool,
    pub collection_tag: Option<String>,
    pub limit: Option<usize>,
}

/// Summary statistics returned by [`StorageDb::scan_font_directories`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FontScanReport {
    pub files_seen: usize,
    pub faces_indexed: usize,
    pub files_skipped_unchanged: usize,
    pub files_failed: usize,
}

/// Returns standard OS font directories across Linux, Windows, and macOS.
#[must_use]
pub fn standard_font_directories() -> Vec<PathBuf> {
    let mut dirs_list = Vec::new();

    #[cfg(target_os = "linux")]
    {
        dirs_list.push(PathBuf::from("/usr/share/fonts"));
        dirs_list.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(data_local) = dirs::data_local_dir() {
            dirs_list.push(data_local.join("fonts"));
        }
        if let Some(home) = dirs::home_dir() {
            dirs_list.push(home.join(".fonts"));
        }
    }

    #[cfg(target_os = "windows")]
    {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_owned());
        dirs_list.push(PathBuf::from(windir).join("Fonts"));
        if let Some(local) = dirs::data_local_dir() {
            dirs_list.push(local.join("Microsoft").join("Windows").join("Fonts"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        dirs_list.push(PathBuf::from("/System/Library/Fonts"));
        dirs_list.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = dirs::home_dir() {
            dirs_list.push(home.join("Library").join("Fonts"));
        }
    }

    dirs_list
}

impl StorageDb {
    /// Scans all existing standard OS font directories and indexes discovered faces.
    pub fn scan_system_fonts(&self) -> StorageResult<FontScanReport> {
        let existing: Vec<PathBuf> = standard_font_directories()
            .into_iter()
            .filter(|d| d.exists())
            .collect();
        self.scan_font_directories(&existing, "system")
    }

    /// Recursively scans `directories` for `.ttf`, `.otf`, and `.ttc` files and indexes them
    /// into SQLite `font_index` under `collection_tag`.
    pub fn scan_font_directories(
        &self,
        directories: &[PathBuf],
        collection_tag: &str,
    ) -> StorageResult<FontScanReport> {
        let mut report = FontScanReport::default();
        let mut font_files = Vec::new();
        let mut visited_dirs = HashSet::new();
        for dir in directories {
            collect_font_files_recursive(dir, 0, &mut visited_dirs, &mut font_files);
        }

        for path in font_files {
            report.files_seen += 1;
            let mtime = file_mtime_epoch(&path);
            let path_str = path.to_string_lossy().into_owned();

            let cached_mtime: Option<i64> = self
                .conn
                .query_row(
                    "SELECT file_mtime_epoch FROM font_index WHERE file_path = ?1 LIMIT 1",
                    params![path_str],
                    |row| row.get(0),
                )
                .optional()?;
            if cached_mtime == Some(mtime) && mtime != 0 {
                report.files_skipped_unchanged += 1;
                continue;
            }

            match self.index_font_file(&path, collection_tag) {
                Ok(n) => report.faces_indexed += n,
                Err(_) => report.files_failed += 1,
            }
        }
        Ok(report)
    }

    /// Parses and indexes a single `.ttf`, `.otf`, or `.ttc` file into `font_index`
    /// atomically (pruning any stale faces when a font collection shrinks).
    pub fn index_font_file(&self, path: &Path, collection_tag: &str) -> StorageResult<usize> {
        let mtime = file_mtime_epoch(path);
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(MAX_FONT_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if (bytes.len() as u64) > MAX_FONT_FILE_BYTES {
            return Err(StorageError::InvalidArgument(format!(
                "font file exceeds {} byte limit",
                MAX_FONT_FILE_BYTES
            )));
        }

        let faces = inspect_font_bytes(&bytes, path);
        if faces.is_empty() {
            return Err(StorageError::InvalidArgument(format!(
                "no valid SFNT font faces in {}",
                path.display()
            )));
        }

        let path_str = path.to_string_lossy().into_owned();
        self.conn.execute_batch("SAVEPOINT font_index_file")?;
        let tx_res: StorageResult<usize> = (|| {
            for face in &faces {
                let id = Uuid::new_v4().to_string();
                let safe_weight = i64::from(face.weight.clamp(100, 1000));
                self.conn.execute(
                    "INSERT INTO font_index (
                         id, file_path, face_index, family_name, subfamily_name, postscript_name,
                         weight, italic, is_variable, supports_arabic_rtl, supports_kashida,
                         collection_tag, favorite, file_mtime_epoch, indexed_at
                     ) VALUES (
                         ?1, ?2, ?3, ?4, ?5, ?6,
                         ?7, ?8, ?9, ?10, ?11,
                         ?12, 0, ?13, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                     )
                     ON CONFLICT(file_path, face_index) DO UPDATE SET
                         family_name         = excluded.family_name,
                         subfamily_name      = excluded.subfamily_name,
                         postscript_name     = excluded.postscript_name,
                         weight              = excluded.weight,
                         italic              = excluded.italic,
                         is_variable         = excluded.is_variable,
                         supports_arabic_rtl = excluded.supports_arabic_rtl,
                         supports_kashida    = excluded.supports_kashida,
                         collection_tag      = excluded.collection_tag,
                         file_mtime_epoch    = excluded.file_mtime_epoch,
                         indexed_at          = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
                    params![
                        id,
                        path_str,
                        i64::from(face.face_index),
                        face.family_name,
                        face.subfamily_name,
                        face.postscript_name,
                        safe_weight,
                        i64::from(face.italic),
                        i64::from(face.is_variable),
                        i64::from(face.supports_arabic_rtl),
                        i64::from(face.supports_kashida),
                        collection_tag,
                        mtime,
                    ],
                )?;
            }
            self.conn.execute(
                "DELETE FROM font_index WHERE file_path = ?1 AND face_index >= ?2",
                params![path_str, faces.len() as i64],
            )?;
            Ok(faces.len())
        })();

        match tx_res {
            Ok(count) => {
                self.conn
                    .execute_batch("RELEASE SAVEPOINT font_index_file")?;
                Ok(count)
            }
            Err(err) => {
                let _ = self.conn.execute_batch(
                    "ROLLBACK TO SAVEPOINT font_index_file; RELEASE SAVEPOINT font_index_file;",
                );
                Err(err)
            }
        }
    }

    /// Searches indexed fonts by family substring, Arabic RTL support, Kashida support,
    /// favorites, and collection tag.
    pub fn search_fonts(&self, query: &FontQuery) -> StorageResult<Vec<FontRecord>> {
        let mut sql = String::from(
            "SELECT id, file_path, face_index, family_name, subfamily_name, postscript_name,
                    weight, italic, is_variable, supports_arabic_rtl, supports_kashida,
                    collection_tag, favorite, file_mtime_epoch
             FROM font_index WHERE 1=1",
        );

        let family_pattern = query
            .family_query
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                let escaped = s
                    .to_lowercase()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                format!("%{escaped}%")
            });

        if family_pattern.is_some() {
            sql.push_str(
                " AND (lower(family_name) LIKE ?1 ESCAPE '\\' OR lower(COALESCE(postscript_name,'')) LIKE ?1 ESCAPE '\\')",
            );
        }
        if query.arabic_only {
            sql.push_str(" AND supports_arabic_rtl = 1");
        }
        if query.kashida_only {
            sql.push_str(" AND supports_kashida = 1");
        }
        if query.favorites_only {
            sql.push_str(" AND favorite = 1");
        }
        if query.collection_tag.is_some() {
            sql.push_str(" AND collection_tag = ?2");
        }
        sql.push_str(" ORDER BY favorite DESC, family_name COLLATE NOCASE ASC, weight ASC");
        if let Some(limit) = query.limit {
            use std::fmt::Write as _;
            let safe_limit = limit.min(i64::MAX as usize);
            let _ = write!(sql, " LIMIT {safe_limit}");
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match (&family_pattern, &query.collection_tag) {
            (Some(fam), Some(tag)) => stmt.query_map(params![fam, tag], row_to_font)?,
            (Some(fam), None) => stmt.query_map(params![fam], row_to_font)?,
            (None, Some(tag)) => {
                // Rebind placeholder ?2 -> ?1 cleanly
                drop(stmt);
                let sql2 = sql.replace("?2", "?1");
                let mut stmt2 = self.conn.prepare(&sql2)?;
                let collected = stmt2
                    .query_map(params![tag], row_to_font)?
                    .collect::<Result<Vec<_>, _>>()?;
                return Ok(collected);
            }
            (None, None) => stmt.query_map([], row_to_font)?,
        };
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Marks or unmarks an indexed font face as a favorite.
    pub fn set_font_favorite(&self, font_id: &str, favorite: bool) -> StorageResult<bool> {
        let count = self.conn.execute(
            "UPDATE font_index SET favorite = ?2 WHERE id = ?1",
            params![font_id, i64::from(favorite)],
        )?;
        Ok(count > 0)
    }
}

fn row_to_font(row: &rusqlite::Row<'_>) -> rusqlite::Result<FontRecord> {
    Ok(FontRecord {
        id: row.get(0)?,
        file_path: row.get(1)?,
        face_index: row.get::<_, i64>(2)? as u32,
        family_name: row.get(3)?,
        subfamily_name: row.get(4)?,
        postscript_name: row.get(5)?,
        weight: row.get::<_, i64>(6)? as u16,
        italic: row.get::<_, i64>(7)? != 0,
        is_variable: row.get::<_, i64>(8)? != 0,
        supports_arabic_rtl: row.get::<_, i64>(9)? != 0,
        supports_kashida: row.get::<_, i64>(10)? != 0,
        collection_tag: row.get(11)?,
        favorite: row.get::<_, i64>(12)? != 0,
        file_mtime_epoch: row.get(13)?,
    })
}

fn collect_font_files_recursive(
    dir: &Path,
    depth: usize,
    visited_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    if depth > MAX_FONT_SCAN_DEPTH {
        return;
    }
    let canonical = match std::fs::canonicalize(dir) {
        Ok(c) => c,
        Err(_) => return,
    };
    if !visited_dirs.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Use `std::fs::metadata` (follows symlinks) so symlinked Linux system fonts are indexed!
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            collect_font_files_recursive(&path, depth + 1, visited_dirs, out);
        } else if meta.is_file() && is_supported_font_extension(&path) {
            out.push(path);
        }
    }
}

fn is_supported_font_extension(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "ttf" | "otf" | "ttc" | "otc"
        )
    })
}

fn file_mtime_epoch(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

// =============================================================================
// Pure-Rust SFNT (.ttf / .otf / .ttc) Inspector
// =============================================================================

/// Pure-Rust SFNT binary parser that inspects `name`, `OS/2`, `cmap`, and `fvar` tables
/// across single fonts and TrueType Collections (`.ttc`).
#[must_use]
pub fn inspect_font_bytes(bytes: &[u8], file_path: &Path) -> Vec<ParsedFontFace> {
    if bytes.len() < 12 {
        return Vec::new();
    }
    let fallback_family = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown Font")
        .to_owned();

    if &bytes[0..4] == b"ttcf" {
        let Some(num_fonts) = read_u32_be(bytes, 8) else {
            return Vec::new();
        };
        let count = (num_fonts as usize).min(64);
        let mut faces = Vec::new();
        for i in 0..count {
            let Some(offset) = read_u32_be(bytes, 12 + i * 4) else {
                break;
            };
            if let Some(face) =
                inspect_sfnt_face(bytes, offset as usize, i as u32, &fallback_family)
            {
                faces.push(face);
            }
        }
        faces
    } else {
        inspect_sfnt_face(bytes, 0, 0, &fallback_family)
            .into_iter()
            .collect()
    }
}

fn inspect_sfnt_face(
    bytes: &[u8],
    face_offset: usize,
    face_index: u32,
    fallback_family: &str,
) -> Option<ParsedFontFace> {
    if face_offset.checked_add(12)? > bytes.len() {
        return None;
    }
    let sfnt_ver = &bytes[face_offset..face_offset + 4];
    let valid_sig = sfnt_ver == [0x00, 0x01, 0x00, 0x00]
        || sfnt_ver == b"OTTO"
        || sfnt_ver == b"true"
        || sfnt_ver == b"typ1";
    if !valid_sig {
        return None;
    }

    let num_tables = usize::from(read_u16_be(bytes, face_offset + 4)?);
    let mut name_table: Option<&[u8]> = None;
    let mut os2_table: Option<&[u8]> = None;
    let mut cmap_table: Option<&[u8]> = None;
    let mut is_variable = false;

    for i in 0..num_tables.min(256) {
        let rec_off = face_offset.checked_add(12)?.checked_add(i * 16)?;
        if rec_off + 16 > bytes.len() {
            break;
        }
        let tag = &bytes[rec_off..rec_off + 4];
        let tbl_off = read_u32_be(bytes, rec_off + 8)? as usize;
        let tbl_len = read_u32_be(bytes, rec_off + 12)? as usize;
        let Some(end) = tbl_off.checked_add(tbl_len) else {
            continue;
        };
        if end > bytes.len() {
            continue;
        }
        let slice = &bytes[tbl_off..end];
        match tag {
            b"name" => name_table = Some(slice),
            b"OS/2" => os2_table = Some(slice),
            b"cmap" => cmap_table = Some(slice),
            b"fvar" => is_variable = true,
            _ => {}
        }
    }

    let names = name_table.map(parse_name_table).unwrap_or_default();
    let family_name = names
        .typographic_family
        .or(names.family)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback_family.to_owned());
    let subfamily_name = names
        .typographic_subfamily
        .or(names.subfamily)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Regular".to_owned());

    let (weight, os2_italic, os2_arabic) = os2_table
        .map(parse_os2_table)
        .unwrap_or((400, false, false));

    let italic = os2_italic
        || subfamily_name.to_ascii_lowercase().contains("italic")
        || subfamily_name.to_ascii_lowercase().contains("oblique");

    let (cmap_arabic, cmap_kashida) = cmap_table
        .map(inspect_cmap_arabic)
        .unwrap_or((false, false));

    let supports_arabic_rtl = os2_arabic || cmap_arabic;
    let supports_kashida = cmap_kashida;

    Some(ParsedFontFace {
        face_index,
        family_name,
        subfamily_name,
        postscript_name: names.postscript,
        weight: weight.clamp(100, 1000),
        italic,
        is_variable,
        supports_arabic_rtl,
        supports_kashida,
    })
}

#[derive(Default)]
struct NameRecords {
    family: Option<String>,
    subfamily: Option<String>,
    postscript: Option<String>,
    typographic_family: Option<String>,
    typographic_subfamily: Option<String>,
}

fn parse_name_table(table: &[u8]) -> NameRecords {
    let mut out = NameRecords::default();
    let (Some(count), Some(string_offset)) = (read_u16_be(table, 2), read_u16_be(table, 4)) else {
        return out;
    };
    let storage_base = usize::from(string_offset);

    for i in 0..usize::from(count).min(512) {
        let rec = 6 + i * 12;
        if rec + 12 > table.len() {
            break;
        }
        let Some(platform_id) = read_u16_be(table, rec) else {
            continue;
        };
        let Some(encoding_id) = read_u16_be(table, rec + 2) else {
            continue;
        };
        let Some(name_id) = read_u16_be(table, rec + 6) else {
            continue;
        };
        let Some(length) = read_u16_be(table, rec + 8) else {
            continue;
        };
        let Some(offset) = read_u16_be(table, rec + 10) else {
            continue;
        };

        if !matches!(name_id, 1 | 2 | 6 | 16 | 17) {
            continue;
        }
        let start = storage_base.saturating_add(usize::from(offset));
        let end = start.saturating_add(usize::from(length));
        let Some(raw) = table.get(start..end) else {
            continue;
        };

        let decoded = match (platform_id, encoding_id) {
            // Unicode (0) or Windows BMP/Full (3, 1 | 10) -> UTF-16BE
            (0, _) | (3, 1) | (3, 10) => decode_utf16be(raw),
            // Macintosh Roman (1, 0) -> ASCII / Latin-1
            (1, 0) => Some(raw.iter().map(|&b| char::from(b)).collect::<String>()),
            _ => None,
        };
        let Some(val) = decoded
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        // Prefer Windows/Unicode UTF-16 over Mac fallback
        let prefer_overwrite = platform_id == 3 || platform_id == 0;
        let slot = match name_id {
            1 => &mut out.family,
            2 => &mut out.subfamily,
            6 => &mut out.postscript,
            16 => &mut out.typographic_family,
            17 => &mut out.typographic_subfamily,
            _ => continue,
        };
        if slot.is_none() || prefer_overwrite {
            *slot = Some(val);
        }
    }
    out
}

fn parse_os2_table(table: &[u8]) -> (u16, bool, bool) {
    let weight = read_u16_be(table, 4).unwrap_or(400);
    // ulUnicodeRange1 at offset 42 (bit 13 = Arabic U+0600..U+06FF)
    // ulUnicodeRange2 at offset 46 (bit 31 -> bit 63 = Arabic Presentation Forms-A)
    // ulUnicodeRange3 at offset 50 (bit 3 -> bit 67 = Arabic Presentation Forms-B)
    let ur1 = read_u32_be(table, 42).unwrap_or(0);
    let ur2 = read_u32_be(table, 46).unwrap_or(0);
    let ur3 = read_u32_be(table, 50).unwrap_or(0);
    let has_arabic_range =
        (ur1 & (1 << 13)) != 0 || (ur2 & (1 << 31)) != 0 || (ur3 & (1 << 3)) != 0;

    // fsSelection at offset 62: bit 0 = ITALIC, bit 9 = OBLIQUE
    let fs_sel = read_u16_be(table, 62).unwrap_or(0);
    let is_italic = (fs_sel & 0x0001) != 0 || (fs_sel & 0x0200) != 0;
    (weight, is_italic, has_arabic_range)
}

/// Checks `cmap` Format 4 and Format 12 subtables for Arabic letters (`U+0627` Alef / `U+0628` Beh)
/// and Arabic Tatweel/Kashida (`U+0640`).
fn inspect_cmap_arabic(table: &[u8]) -> (bool, bool) {
    let Some(num_tables) = read_u16_be(table, 2) else {
        return (false, false);
    };
    let mut has_arabic = false;
    let mut has_kashida = false;

    for i in 0..usize::from(num_tables).min(32) {
        let rec = 4 + i * 8;
        if rec + 8 > table.len() {
            break;
        }
        let Some(platform) = read_u16_be(table, rec) else {
            continue;
        };
        let Some(encoding) = read_u16_be(table, rec + 2) else {
            continue;
        };
        let Some(sub_off) = read_u32_be(table, rec + 4).map(|v| v as usize) else {
            continue;
        };

        // Only inspect Unicode cmap subtables
        if !(platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10))) {
            continue;
        }
        let Some(sub) = table.get(sub_off..) else {
            continue;
        };
        let Some(format) = read_u16_be(sub, 0) else {
            continue;
        };

        match format {
            4 => {
                if cmap_fmt4_contains(sub, 0x0627) || cmap_fmt4_contains(sub, 0x0628) {
                    has_arabic = true;
                }
                if cmap_fmt4_contains(sub, 0x0640) {
                    has_kashida = true;
                }
            }
            12 => {
                if cmap_fmt12_contains(sub, 0x0627) || cmap_fmt12_contains(sub, 0x0628) {
                    has_arabic = true;
                }
                if cmap_fmt12_contains(sub, 0x0640) {
                    has_kashida = true;
                }
            }
            _ => {}
        }
    }

    (has_arabic, has_kashida)
}

fn cmap_fmt4_contains(sub: &[u8], cp: u16) -> bool {
    let Some(seg_count_x2) = read_u16_be(sub, 6).map(usize::from) else {
        return false;
    };
    let seg_count = seg_count_x2 / 2;
    let end_codes_off = 14usize;
    let start_codes_off = end_codes_off + seg_count_x2 + 2;
    let id_delta_off = start_codes_off + seg_count_x2;
    let id_range_off = id_delta_off + seg_count_x2;
    if id_range_off + seg_count_x2 > sub.len() {
        return false;
    }

    for i in 0..seg_count {
        let Some(end_c) = read_u16_be(sub, end_codes_off + i * 2) else {
            return false;
        };
        let Some(start_c) = read_u16_be(sub, start_codes_off + i * 2) else {
            return false;
        };
        if cp >= start_c && cp <= end_c {
            let delta = read_u16_be(sub, id_delta_off + i * 2).unwrap_or(0);
            let range_val = read_u16_be(sub, id_range_off + i * 2).unwrap_or(0);
            let glyph_id = if range_val == 0 {
                cp.wrapping_add(delta)
            } else {
                let Some(ptr) = id_range_off
                    .checked_add(i * 2)
                    .and_then(|v| v.checked_add(usize::from(range_val)))
                    .and_then(|v| v.checked_add(usize::from(cp - start_c) * 2))
                else {
                    return false;
                };
                let raw_gid = read_u16_be(sub, ptr).unwrap_or(0);
                if raw_gid == 0 {
                    0
                } else {
                    raw_gid.wrapping_add(delta)
                }
            };
            return glyph_id != 0;
        }
    }
    false
}

fn cmap_fmt12_contains(sub: &[u8], cp: u32) -> bool {
    let Some(num_groups) = read_u32_be(sub, 12).map(|v| v as usize) else {
        return false;
    };
    for i in 0..num_groups.min(65_536) {
        let off = 16 + i * 12;
        if off + 12 > sub.len() {
            break;
        }
        let Some(start) = read_u32_be(sub, off) else {
            break;
        };
        let Some(end) = read_u32_be(sub, off + 4) else {
            break;
        };
        let Some(start_gid) = read_u32_be(sub, off + 8) else {
            break;
        };
        if cp >= start && cp <= end {
            return start_gid.saturating_add(cp - start) != 0;
        }
    }
    false
}

fn decode_utf16be(raw: &[u8]) -> Option<String> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    String::from_utf16(&units).ok()
}

fn read_u16_be(buf: &[u8], off: usize) -> Option<u16> {
    let s = buf.get(off..off + 2)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

fn read_u32_be(buf: &[u8], off: usize) -> Option<u32> {
    let s = buf.get(off..off + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesizes a minimal valid SFNT TrueType font in-memory for deterministic testing.
    fn build_synthetic_ttf(
        family: &str,
        subfamily: &str,
        weight: u16,
        italic: bool,
        arabic: bool,
        kashida: bool,
    ) -> Vec<u8> {
        // 1. Build `name` table (2 Windows UTF-16BE records: family=1, subfamily=2)
        let fam_u16: Vec<u8> = family.encode_utf16().flat_map(u16::to_be_bytes).collect();
        let sub_u16: Vec<u8> = subfamily
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        let mut name_tbl = Vec::new();
        name_tbl.extend_from_slice(&0u16.to_be_bytes()); // format
        name_tbl.extend_from_slice(&2u16.to_be_bytes()); // count
        let str_off = 6u16 + 2 * 12;
        name_tbl.extend_from_slice(&str_off.to_be_bytes());
        // Record 1: family
        for v in [3u16, 1, 0x0409, 1, fam_u16.len() as u16, 0] {
            name_tbl.extend_from_slice(&v.to_be_bytes());
        }
        // Record 2: subfamily
        for v in [
            3u16,
            1,
            0x0409,
            2,
            sub_u16.len() as u16,
            fam_u16.len() as u16,
        ] {
            name_tbl.extend_from_slice(&v.to_be_bytes());
        }
        name_tbl.extend_from_slice(&fam_u16);
        name_tbl.extend_from_slice(&sub_u16);

        // 2. Build `OS/2` table (78 bytes)
        let mut os2_tbl = vec![0u8; 78];
        os2_tbl[4..6].copy_from_slice(&weight.to_be_bytes());
        let ur1: u32 = if arabic { 1 << 13 } else { 1 }; // bit 13 = Arabic, bit 0 = Basic Latin
        os2_tbl[42..46].copy_from_slice(&ur1.to_be_bytes());
        let fs_sel: u16 = if italic { 1 } else { 0x40 };
        os2_tbl[62..64].copy_from_slice(&fs_sel.to_be_bytes());

        // 3. Build `cmap` table with Format 12 subtable
        let mut groups: Vec<(u32, u32, u32)> = vec![(0x0041, 0x005A, 1)]; // A..Z
        if arabic {
            groups.push((0x0627, 0x063A, 30)); // Arabic letters
        }
        if kashida {
            groups.push((0x0640, 0x0640, 60)); // Arabic Tatweel / Kashida
        }
        let mut fmt12 = Vec::new();
        fmt12.extend_from_slice(&12u16.to_be_bytes());
        fmt12.extend_from_slice(&0u16.to_be_bytes());
        let fmt12_len = (16 + groups.len() * 12) as u32;
        fmt12.extend_from_slice(&fmt12_len.to_be_bytes());
        fmt12.extend_from_slice(&0u32.to_be_bytes()); // lang
        fmt12.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for (s, e, g) in groups {
            fmt12.extend_from_slice(&s.to_be_bytes());
            fmt12.extend_from_slice(&e.to_be_bytes());
            fmt12.extend_from_slice(&g.to_be_bytes());
        }
        let mut cmap_tbl = Vec::new();
        cmap_tbl.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap_tbl.extend_from_slice(&1u16.to_be_bytes()); // numTables
        cmap_tbl.extend_from_slice(&3u16.to_be_bytes()); // Windows
        cmap_tbl.extend_from_slice(&10u16.to_be_bytes()); // Unicode full
        cmap_tbl.extend_from_slice(&12u32.to_be_bytes()); // subtable offset
        cmap_tbl.extend_from_slice(&fmt12);

        // Assemble SFNT directory + tables
        let tables: [(&[u8; 4], Vec<u8>); 3] =
            [(b"OS/2", os2_tbl), (b"cmap", cmap_tbl), (b"name", name_tbl)];
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // TrueType
        out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        out.extend_from_slice(&32u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&16u16.to_be_bytes());

        let mut data_offset = 12 + tables.len() * 16;
        for (tag, data) in &tables {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&0u32.to_be_bytes()); // checksum
            out.extend_from_slice(&(data_offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            data_offset += data.len();
        }
        for (_, data) in tables {
            out.extend_from_slice(&data);
        }
        out
    }

    #[test]
    fn font_scanner_indexes_and_filters_arabic_kashida_and_latin_fonts() {
        let dir = tempfile::tempdir().unwrap();
        let arabic_path = dir.path().join("CairoArabic-Bold.ttf");
        let latin_path = dir.path().join("StudioSans-Italic.ttf");

        std::fs::write(
            &arabic_path,
            build_synthetic_ttf("Cairo Arabic", "Bold", 700, false, true, true),
        )
        .unwrap();
        std::fs::write(
            &latin_path,
            build_synthetic_ttf("Studio Sans", "Italic", 400, true, false, false),
        )
        .unwrap();

        let db = StorageDb::open_in_memory().unwrap();
        let report = db
            .scan_font_directories(&[dir.path().to_path_buf()], "custom")
            .unwrap();
        assert_eq!(report.files_seen, 2);
        assert_eq!(report.faces_indexed, 2);
        assert_eq!(report.files_failed, 0);

        // Arabic-only filter returns only Cairo Arabic with kashida support
        let arabic_fonts = db
            .search_fonts(&FontQuery {
                arabic_only: true,
                ..FontQuery::default()
            })
            .unwrap();
        assert_eq!(arabic_fonts.len(), 1);
        assert_eq!(arabic_fonts[0].family_name, "Cairo Arabic");
        assert_eq!(arabic_fonts[0].weight, 700);
        assert!(arabic_fonts[0].supports_arabic_rtl);
        assert!(arabic_fonts[0].supports_kashida);

        // Family substring search
        let sans = db
            .search_fonts(&FontQuery {
                family_query: Some("studio".into()),
                ..FontQuery::default()
            })
            .unwrap();
        assert_eq!(sans.len(), 1);
        assert!(sans[0].italic);
        assert!(!sans[0].supports_arabic_rtl);

        // Re-scanning unchanged files skips them via mtime cache
        let report2 = db
            .scan_font_directories(&[dir.path().to_path_buf()], "custom")
            .unwrap();
        assert_eq!(report2.files_skipped_unchanged, 2);
    }
}
