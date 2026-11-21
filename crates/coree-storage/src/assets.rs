// SPDX-License-Identifier: MPL-2.0

//! Typed offline CRUD for `recent_documents`, `custom_palettes`, `palette_swatches`,
//! `templates`, `workspace_layouts`, `brush_presets`, and `app_preferences`.

use crate::db::StorageDb;
use crate::error::{StorageError, StorageResult};
use coree_core::{Color, ColorMode, ColorPalette, ColorSwatch};
use rusqlite::{params, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

// =============================================================================
// 1. Recent Documents
// =============================================================================

/// Row in `recent_documents`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentDocumentRecord {
    pub id: String,
    pub file_path: String,
    pub display_name: String,
    pub file_format: String,
    pub cdr_version: Option<String>,
    pub page_count: u32,
    pub color_mode: String,
    pub thumbnail_png: Option<Vec<u8>>,
    pub file_size_bytes: u64,
    pub pinned: bool,
    pub last_opened_at: String,
    pub created_at: String,
    pub updated_at: String,
}

// =============================================================================
// 2. Custom Palettes & Swatches
// =============================================================================

/// Row in `custom_palettes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomPaletteRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub palette_kind: String,
    pub is_builtin: bool,
    pub is_docked: bool,
    pub show_color_names: bool,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
}

/// Row in `palette_swatches`.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteSwatchRecord {
    pub id: String,
    pub palette_id: String,
    pub name: String,
    pub color_space: String,
    pub c: Option<f64>,
    pub m: Option<f64>,
    pub y: Option<f64>,
    pub k: Option<f64>,
    pub r: Option<u8>,
    pub g: Option<u8>,
    pub b: Option<u8>,
    pub lab_l: Option<f64>,
    pub lab_a: Option<f64>,
    pub lab_b: Option<f64>,
    pub spot_channel_name: Option<String>,
    pub overprint_default: bool,
    pub sort_order: i32,
}

impl PaletteSwatchRecord {
    /// Converts a `coree_core::ColorSwatch` into a SQLite swatch record honoring schema constraints.
    #[must_use]
    pub fn from_core_swatch(
        palette_id: impl Into<String>,
        swatch: &ColorSwatch,
        sort_order: i32,
    ) -> Self {
        let id = Uuid::new_v4().to_string();
        let palette_id = palette_id.into();
        let mut rec = Self {
            id,
            palette_id,
            name: swatch.name.clone(),
            color_space: "rgb".to_owned(),
            c: None,
            m: None,
            y: None,
            k: None,
            r: None,
            g: None,
            b: None,
            lab_l: None,
            lab_a: None,
            lab_b: None,
            spot_channel_name: None,
            overprint_default: false,
            sort_order,
        };

        let sanitize_clamp = |v: f32, scale: f64, lo: f64, hi: f64| -> f64 {
            let scaled = f64::from(v) * scale;
            if scaled.is_finite() {
                scaled.clamp(lo, hi)
            } else {
                0.0_f64.clamp(lo, hi)
            }
        };

        match &swatch.color {
            Color::Cmyk { c, m, y, k, .. } => {
                rec.color_space = "cmyk".to_owned();
                rec.c = Some(sanitize_clamp(*c, 100.0, 0.0, 100.0));
                rec.m = Some(sanitize_clamp(*m, 100.0, 0.0, 100.0));
                rec.y = Some(sanitize_clamp(*y, 100.0, 0.0, 100.0));
                rec.k = Some(sanitize_clamp(*k, 100.0, 0.0, 100.0));
            }
            Color::Spot {
                name,
                fallback_cmyk,
                ..
            } => {
                rec.color_space = "spot".to_owned();
                rec.c = Some(sanitize_clamp(fallback_cmyk[0], 100.0, 0.0, 100.0));
                rec.m = Some(sanitize_clamp(fallback_cmyk[1], 100.0, 0.0, 100.0));
                rec.y = Some(sanitize_clamp(fallback_cmyk[2], 100.0, 0.0, 100.0));
                rec.k = Some(sanitize_clamp(fallback_cmyk[3], 100.0, 0.0, 100.0));
                let channel = if name.trim().is_empty() {
                    if swatch.name.trim().is_empty() {
                        "Spot".to_owned()
                    } else {
                        swatch.name.clone()
                    }
                } else {
                    name.clone()
                };
                rec.overprint_default = channel.eq_ignore_ascii_case("CutContour");
                rec.spot_channel_name = Some(channel);
            }
            Color::Lab { l, a_ch, b_ch, .. } => {
                rec.color_space = "lab".to_owned();
                rec.lab_l = Some(sanitize_clamp(*l, 1.0, 0.0, 100.0));
                rec.lab_a = Some(sanitize_clamp(*a_ch, 1.0, -128.0, 127.0));
                rec.lab_b = Some(sanitize_clamp(*b_ch, 1.0, -128.0, 127.0));
            }
            Color::Rgba { .. } | Color::Hsl { .. } | Color::Gray { .. } => {
                let [r, g, b, _] = swatch.color.to_rgba8();
                rec.color_space = "rgb".to_owned();
                rec.r = Some(r);
                rec.g = Some(g);
                rec.b = Some(b);
            }
        }
        rec
    }

    /// Converts this database row back into a `coree_core::ColorSwatch`.
    #[must_use]
    pub fn to_core_swatch(&self) -> ColorSwatch {
        let color = match self.color_space.as_str() {
            "cmyk" => Color::cmyk(
                (self.c.unwrap_or(0.0) / 100.0) as f32,
                (self.m.unwrap_or(0.0) / 100.0) as f32,
                (self.y.unwrap_or(0.0) / 100.0) as f32,
                (self.k.unwrap_or(0.0) / 100.0) as f32,
            ),
            "spot" => Color::spot(
                self.spot_channel_name
                    .clone()
                    .unwrap_or_else(|| self.name.clone()),
                1.0,
                [
                    (self.c.unwrap_or(0.0) / 100.0) as f32,
                    (self.m.unwrap_or(100.0) / 100.0) as f32,
                    (self.y.unwrap_or(0.0) / 100.0) as f32,
                    (self.k.unwrap_or(0.0) / 100.0) as f32,
                ],
            ),
            "lab" => Color::lab(
                self.lab_l.unwrap_or(0.0) as f32,
                self.lab_a.unwrap_or(0.0) as f32,
                self.lab_b.unwrap_or(0.0) as f32,
            ),
            _ => Color::rgba8(
                self.r.unwrap_or(0),
                self.g.unwrap_or(0),
                self.b.unwrap_or(0),
                255,
            ),
        };
        ColorSwatch::new(self.name.clone(), color)
    }
}

// =============================================================================
// 3. Templates
// =============================================================================

/// Row in `templates`.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateRecord {
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub template_path: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub bleed_mm: f64,
    pub page_count: u32,
    pub color_mode: String,
    pub thumbnail_png: Option<Vec<u8>>,
    pub is_builtin: bool,
    pub favorite: bool,
}

// =============================================================================
// 4. Workspace Layouts
// =============================================================================

/// Row in `workspace_layouts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLayoutRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub dock_tree_json: String,
    pub toolbox_config_json: String,
    pub shortcuts_json: String,
    pub is_builtin: bool,
    pub is_active: bool,
}

// =============================================================================
// 5. Brush Presets
// =============================================================================

/// Row in `brush_presets`.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushPresetRecord {
    pub id: String,
    pub name: String,
    pub engine_kind: String,
    pub category: String,
    pub size_px: f64,
    pub hardness: f64,
    pub spacing: f64,
    pub opacity: f64,
    pub pressure_curve_json: String,
    pub params_json: String,
    pub is_builtin: bool,
    pub sort_order: i32,
}

// =============================================================================
// 6. App Preferences
// =============================================================================

/// Row in `app_preferences`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPreferenceRecord {
    pub key: String,
    pub value_json: String,
    pub category: String,
    pub updated_at: String,
}

// =============================================================================
// StorageDb CRUD Implementations
// =============================================================================

impl StorageDb {
    // -------------------------------------------------------------------------
    // recent_documents
    // -------------------------------------------------------------------------

    /// Inserts or updates a recent document entry keyed by `file_path`.
    pub fn upsert_recent_document(&self, rec: &RecentDocumentRecord) -> StorageResult<()> {
        self.conn.execute(
            "INSERT INTO recent_documents (
                 id, file_path, display_name, file_format, cdr_version,
                 page_count, color_mode, thumbnail_png, file_size_bytes, pinned,
                 last_opened_at, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5,
                 ?6, ?7, ?8, ?9, ?10,
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             )
             ON CONFLICT(file_path) DO UPDATE SET
                 display_name    = excluded.display_name,
                 file_format     = excluded.file_format,
                 cdr_version     = excluded.cdr_version,
                 page_count      = excluded.page_count,
                 color_mode      = excluded.color_mode,
                 thumbnail_png   = COALESCE(excluded.thumbnail_png, recent_documents.thumbnail_png),
                 file_size_bytes = excluded.file_size_bytes,
                 pinned          = excluded.pinned,
                 last_opened_at  = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                 updated_at      = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![
                rec.id,
                rec.file_path,
                rec.display_name,
                rec.file_format,
                rec.cdr_version,
                i64::from(rec.page_count.max(1)),
                rec.color_mode,
                rec.thumbnail_png,
                rec.file_size_bytes as i64,
                i64::from(rec.pinned),
            ],
        )?;
        Ok(())
    }

    /// Lists recent documents ordered by pinned first, then most recently opened.
    pub fn list_recent_documents(&self, limit: usize) -> StorageResult<Vec<RecentDocumentRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, display_name, file_format, cdr_version,
                    page_count, color_mode, thumbnail_png, file_size_bytes, pinned,
                    last_opened_at, created_at, updated_at
             FROM recent_documents
             ORDER BY pinned DESC, last_opened_at DESC, rowid DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(RecentDocumentRecord {
                id: row.get(0)?,
                file_path: row.get(1)?,
                display_name: row.get(2)?,
                file_format: row.get(3)?,
                cdr_version: row.get(4)?,
                page_count: row.get::<_, i64>(5)? as u32,
                color_mode: row.get(6)?,
                thumbnail_png: row.get(7)?,
                file_size_bytes: row.get::<_, i64>(8)? as u64,
                pinned: row.get::<_, i64>(9)? != 0,
                last_opened_at: row.get(10)?,
                created_at: row.get(11)?,
                updated_at: row.get(12)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Toggles the `pinned` flag on a recent document by `id`.
    pub fn set_recent_pinned(&self, id: &str, pinned: bool) -> StorageResult<bool> {
        let count = self.conn.execute(
            "UPDATE recent_documents
             SET pinned = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = ?1",
            params![id, i64::from(pinned)],
        )?;
        Ok(count > 0)
    }

    /// Removes a recent document entry by `id`.
    pub fn delete_recent_document(&self, id: &str) -> StorageResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM recent_documents WHERE id = ?1", params![id])?;
        Ok(count > 0)
    }

    /// Clears all non-pinned recent documents.
    pub fn clear_unpinned_recent_documents(&self) -> StorageResult<usize> {
        let count = self
            .conn
            .execute("DELETE FROM recent_documents WHERE pinned = 0", [])?;
        Ok(count)
    }

    // -------------------------------------------------------------------------
    // custom_palettes & palette_swatches
    // -------------------------------------------------------------------------

    /// Saves (inserts or updates) a palette and atomically replaces its swatches in one transaction.
    pub fn save_palette(
        &mut self,
        palette: &CustomPaletteRecord,
        swatches: &[PaletteSwatchRecord],
    ) -> StorageResult<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO custom_palettes (
                 id, name, description, palette_kind, is_builtin,
                 is_docked, show_color_names, sort_order, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             )
             ON CONFLICT(id) DO UPDATE SET
                 name             = excluded.name,
                 description      = excluded.description,
                 palette_kind     = excluded.palette_kind,
                 is_builtin       = excluded.is_builtin,
                 is_docked        = excluded.is_docked,
                 show_color_names = excluded.show_color_names,
                 sort_order       = excluded.sort_order,
                 updated_at       = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![
                palette.id,
                palette.name,
                palette.description,
                palette.palette_kind,
                i64::from(palette.is_builtin),
                i64::from(palette.is_docked),
                i64::from(palette.show_color_names),
                palette.sort_order,
            ],
        )?;

        tx.execute(
            "DELETE FROM palette_swatches WHERE palette_id = ?1",
            params![palette.id],
        )?;

        for sw in swatches {
            tx.execute(
                "INSERT INTO palette_swatches (
                     id, palette_id, name, color_space,
                     c, m, y, k, r, g, b, lab_l, lab_a, lab_b,
                     spot_channel_name, overprint_default, sort_order
                 ) VALUES (
                     ?1, ?2, ?3, ?4,
                     ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17
                 )",
                params![
                    sw.id,
                    palette.id,
                    sw.name,
                    sw.color_space,
                    sw.c,
                    sw.m,
                    sw.y,
                    sw.k,
                    sw.r.map(i64::from),
                    sw.g.map(i64::from),
                    sw.b.map(i64::from),
                    sw.lab_l,
                    sw.lab_a,
                    sw.lab_b,
                    sw.spot_channel_name,
                    i64::from(sw.overprint_default),
                    sw.sort_order,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Saves a `coree_core::ColorPalette` directly into `custom_palettes` + `palette_swatches`.
    pub fn save_core_palette(
        &mut self,
        palette: &ColorPalette,
        palette_kind: &str,
        is_builtin: bool,
        is_docked: bool,
    ) -> StorageResult<String> {
        let id = palette.id.to_string();
        let rec = CustomPaletteRecord {
            id: id.clone(),
            name: palette.name.clone(),
            description: String::new(),
            palette_kind: palette_kind.to_owned(),
            is_builtin,
            is_docked,
            show_color_names: false,
            sort_order: 0,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let swatches: Vec<PaletteSwatchRecord> = palette
            .swatches
            .iter()
            .enumerate()
            .map(|(i, sw)| PaletteSwatchRecord::from_core_swatch(&id, sw, i as i32))
            .collect();
        self.save_palette(&rec, &swatches)?;
        Ok(id)
    }

    /// Loads a palette and its swatches back into a `coree_core::ColorPalette`.
    pub fn load_core_palette(&self, palette_id: &str) -> StorageResult<ColorPalette> {
        let rec: CustomPaletteRecord = self
            .conn
            .query_row(
                "SELECT id, name, description, palette_kind, is_builtin,
                        is_docked, show_color_names, sort_order, created_at, updated_at
                 FROM custom_palettes WHERE id = ?1",
                params![palette_id],
                row_to_palette,
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("palette {palette_id}")))?;

        let swatches = self.list_palette_swatches(palette_id)?;
        let mode = match rec.palette_kind.as_str() {
            "rgb" => ColorMode::Rgb,
            _ => ColorMode::Cmyk,
        };
        let mut out = ColorPalette::new(rec.name, mode);
        if let Ok(uuid) = Uuid::parse_str(&rec.id) {
            out.id = uuid;
        }
        out.is_read_only = rec.is_builtin;
        out.swatches = swatches
            .iter()
            .map(PaletteSwatchRecord::to_core_swatch)
            .collect();
        Ok(out)
    }

    /// Lists all saved palettes ordered by docked status and sort order.
    pub fn list_palettes(&self) -> StorageResult<Vec<CustomPaletteRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, palette_kind, is_builtin,
                    is_docked, show_color_names, sort_order, created_at, updated_at
             FROM custom_palettes
             ORDER BY is_docked DESC, sort_order ASC, name ASC",
        )?;
        let rows = stmt.query_map([], row_to_palette)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Lists all swatches belonging to `palette_id` in sort order.
    pub fn list_palette_swatches(
        &self,
        palette_id: &str,
    ) -> StorageResult<Vec<PaletteSwatchRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, palette_id, name, color_space,
                    c, m, y, k, r, g, b, lab_l, lab_a, lab_b,
                    spot_channel_name, overprint_default, sort_order
             FROM palette_swatches
             WHERE palette_id = ?1
             ORDER BY sort_order ASC",
        )?;
        let rows = stmt.query_map(params![palette_id], |row| {
            Ok(PaletteSwatchRecord {
                id: row.get(0)?,
                palette_id: row.get(1)?,
                name: row.get(2)?,
                color_space: row.get(3)?,
                c: row.get(4)?,
                m: row.get(5)?,
                y: row.get(6)?,
                k: row.get(7)?,
                r: row.get::<_, Option<i64>>(8)?.map(|v| v as u8),
                g: row.get::<_, Option<i64>>(9)?.map(|v| v as u8),
                b: row.get::<_, Option<i64>>(10)?.map(|v| v as u8),
                lab_l: row.get(11)?,
                lab_a: row.get(12)?,
                lab_b: row.get(13)?,
                spot_channel_name: row.get(14)?,
                overprint_default: row.get::<_, i64>(15)? != 0,
                sort_order: row.get(16)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Deletes a palette by `palette_id` (cascades to delete all its `palette_swatches`).
    pub fn delete_palette(&self, palette_id: &str) -> StorageResult<bool> {
        let count = self.conn.execute(
            "DELETE FROM custom_palettes WHERE id = ?1",
            params![palette_id],
        )?;
        Ok(count > 0)
    }

    // -------------------------------------------------------------------------
    // templates
    // -------------------------------------------------------------------------

    /// Inserts or updates a document/packaging template record.
    pub fn upsert_template(&self, tpl: &TemplateRecord) -> StorageResult<()> {
        self.conn.execute(
            "INSERT INTO templates (
                 id, name, category, description, template_path,
                 width_mm, height_mm, bleed_mm, page_count, color_mode,
                 thumbnail_png, is_builtin, favorite, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5,
                 ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             )
             ON CONFLICT(id) DO UPDATE SET
                 name          = excluded.name,
                 category      = excluded.category,
                 description   = excluded.description,
                 template_path = excluded.template_path,
                 width_mm      = excluded.width_mm,
                 height_mm     = excluded.height_mm,
                 bleed_mm      = excluded.bleed_mm,
                 page_count    = excluded.page_count,
                 color_mode    = excluded.color_mode,
                 thumbnail_png = COALESCE(excluded.thumbnail_png, templates.thumbnail_png),
                 is_builtin    = excluded.is_builtin,
                 favorite      = excluded.favorite,
                 updated_at    = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![
                tpl.id,
                tpl.name,
                tpl.category,
                tpl.description,
                tpl.template_path,
                tpl.width_mm,
                tpl.height_mm,
                tpl.bleed_mm,
                i64::from(tpl.page_count.max(1)),
                tpl.color_mode,
                tpl.thumbnail_png,
                i64::from(tpl.is_builtin),
                i64::from(tpl.favorite),
            ],
        )?;
        Ok(())
    }

    /// Lists templates optionally filtered by `category`.
    pub fn list_templates(&self, category: Option<&str>) -> StorageResult<Vec<TemplateRecord>> {
        let mut out = Vec::new();
        if let Some(cat) = category {
            let mut stmt = self.conn.prepare(
                "SELECT id, name, category, description, template_path,
                        width_mm, height_mm, bleed_mm, page_count, color_mode,
                        thumbnail_png, is_builtin, favorite
                 FROM templates
                 WHERE category = ?1
                 ORDER BY favorite DESC, name ASC",
            )?;
            let rows = stmt.query_map(params![cat], row_to_template)?;
            for r in rows {
                out.push(r?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, name, category, description, template_path,
                        width_mm, height_mm, bleed_mm, page_count, color_mode,
                        thumbnail_png, is_builtin, favorite
                 FROM templates
                 ORDER BY category ASC, favorite DESC, name ASC",
            )?;
            let rows = stmt.query_map([], row_to_template)?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    }

    /// Deletes a template by `id`.
    pub fn delete_template(&self, id: &str) -> StorageResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM templates WHERE id = ?1", params![id])?;
        Ok(count > 0)
    }

    // -------------------------------------------------------------------------
    // workspace_layouts
    // -------------------------------------------------------------------------

    /// Inserts or updates a docked workspace layout profile.
    ///
    /// When `layout.is_active` is `true`, atomically clears `is_active` on all other rows
    /// so at most one workspace layout is active at any time.
    pub fn upsert_workspace_layout(&self, layout: &WorkspaceLayoutRecord) -> StorageResult<()> {
        self.conn.execute_batch("SAVEPOINT ws_upsert")?;
        let res: StorageResult<()> = (|| {
            if layout.is_active {
                self.conn.execute(
                    "UPDATE workspace_layouts SET is_active = 0 WHERE id != ?1",
                    params![layout.id],
                )?;
            }
            self.conn.execute(
                "INSERT INTO workspace_layouts (
                     id, name, description, dock_tree_json, toolbox_config_json,
                     shortcuts_json, is_builtin, is_active, updated_at
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                 )
                 ON CONFLICT(id) DO UPDATE SET
                     name                = excluded.name,
                     description         = excluded.description,
                     dock_tree_json      = excluded.dock_tree_json,
                     toolbox_config_json = excluded.toolbox_config_json,
                     shortcuts_json      = excluded.shortcuts_json,
                     is_builtin          = excluded.is_builtin,
                     is_active           = excluded.is_active,
                     updated_at          = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
                params![
                    layout.id,
                    layout.name,
                    layout.description,
                    layout.dock_tree_json,
                    layout.toolbox_config_json,
                    layout.shortcuts_json,
                    i64::from(layout.is_builtin),
                    i64::from(layout.is_active),
                ],
            )?;
            Ok(())
        })();
        match res {
            Ok(()) => {
                self.conn.execute_batch("RELEASE SAVEPOINT ws_upsert")?;
                Ok(())
            }
            Err(err) => {
                let _ = self
                    .conn
                    .execute_batch("ROLLBACK TO SAVEPOINT ws_upsert; RELEASE SAVEPOINT ws_upsert;");
                Err(err)
            }
        }
    }

    /// Marks `layout_id` as the single active workspace layout.
    ///
    /// Returns `Ok(false)` without modifying existing active layouts if `layout_id` does not exist.
    pub fn set_active_workspace_layout(&mut self, layout_id: &str) -> StorageResult<bool> {
        let tx = self.conn.transaction()?;
        let updated = tx.execute(
            "UPDATE workspace_layouts
             SET is_active = 1, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = ?1",
            params![layout_id],
        )?;
        if updated == 0 {
            // Roll back automatically on drop; never clear existing active layout on missing ID.
            return Ok(false);
        }
        tx.execute(
            "UPDATE workspace_layouts SET is_active = 0 WHERE id != ?1",
            params![layout_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Returns the currently active workspace layout, if any.
    pub fn get_active_workspace_layout(&self) -> StorageResult<Option<WorkspaceLayoutRecord>> {
        self.conn
            .query_row(
                "SELECT id, name, description, dock_tree_json, toolbox_config_json,
                        shortcuts_json, is_builtin, is_active
                 FROM workspace_layouts
                 WHERE is_active = 1
                 LIMIT 1",
                [],
                row_to_workspace_layout,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Lists all workspace layouts.
    pub fn list_workspace_layouts(&self) -> StorageResult<Vec<WorkspaceLayoutRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, dock_tree_json, toolbox_config_json,
                    shortcuts_json, is_builtin, is_active
             FROM workspace_layouts
             ORDER BY is_active DESC, name ASC",
        )?;
        let rows = stmt.query_map([], row_to_workspace_layout)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// Deletes a workspace layout by `id`.
    pub fn delete_workspace_layout(&self, id: &str) -> StorageResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM workspace_layouts WHERE id = ?1", params![id])?;
        Ok(count > 0)
    }

    // -------------------------------------------------------------------------
    // brush_presets
    // -------------------------------------------------------------------------

    /// Inserts or updates a vector Artistic Media or raster Photo Studio brush preset.
    pub fn upsert_brush_preset(&self, brush: &BrushPresetRecord) -> StorageResult<()> {
        self.conn.execute(
            "INSERT INTO brush_presets (
                 id, name, engine_kind, category, size_px, hardness, spacing, opacity,
                 pressure_curve_json, params_json, is_builtin, sort_order, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                 ?9, ?10, ?11, ?12, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             )
             ON CONFLICT(id) DO UPDATE SET
                 name                = excluded.name,
                 engine_kind         = excluded.engine_kind,
                 category            = excluded.category,
                 size_px             = excluded.size_px,
                 hardness            = excluded.hardness,
                 spacing             = excluded.spacing,
                 opacity             = excluded.opacity,
                 pressure_curve_json = excluded.pressure_curve_json,
                 params_json         = excluded.params_json,
                 is_builtin          = excluded.is_builtin,
                 sort_order          = excluded.sort_order,
                 updated_at          = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![
                brush.id,
                brush.name,
                brush.engine_kind,
                brush.category,
                brush.size_px,
                brush.hardness,
                brush.spacing,
                brush.opacity,
                brush.pressure_curve_json,
                brush.params_json,
                i64::from(brush.is_builtin),
                brush.sort_order,
            ],
        )?;
        Ok(())
    }

    /// Lists brush presets optionally filtered by `engine_kind`.
    pub fn list_brush_presets(
        &self,
        engine_kind: Option<&str>,
    ) -> StorageResult<Vec<BrushPresetRecord>> {
        let mut out = Vec::new();
        if let Some(kind) = engine_kind {
            let mut stmt = self.conn.prepare(
                "SELECT id, name, engine_kind, category, size_px, hardness, spacing, opacity,
                        pressure_curve_json, params_json, is_builtin, sort_order
                 FROM brush_presets
                 WHERE engine_kind = ?1
                 ORDER BY category ASC, sort_order ASC, name ASC",
            )?;
            let rows = stmt.query_map(params![kind], row_to_brush)?;
            for r in rows {
                out.push(r?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, name, engine_kind, category, size_px, hardness, spacing, opacity,
                        pressure_curve_json, params_json, is_builtin, sort_order
                 FROM brush_presets
                 ORDER BY engine_kind ASC, category ASC, sort_order ASC, name ASC",
            )?;
            let rows = stmt.query_map([], row_to_brush)?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    }

    /// Deletes a brush preset by `id`.
    pub fn delete_brush_preset(&self, id: &str) -> StorageResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM brush_presets WHERE id = ?1", params![id])?;
        Ok(count > 0)
    }

    // -------------------------------------------------------------------------
    // app_preferences (typed JSON key/value store)
    // -------------------------------------------------------------------------

    /// Stores a typed preference value serialized as JSON under `(key, category)`.
    pub fn set_preference<T: Serialize>(
        &self,
        key: &str,
        category: &str,
        value: &T,
    ) -> StorageResult<()> {
        let json = serde_json::to_string(value)?;
        self.conn.execute(
            "INSERT INTO app_preferences (key, value_json, category, updated_at)
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             ON CONFLICT(key) DO UPDATE SET
                 value_json = excluded.value_json,
                 category   = excluded.category,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            params![key, json, category],
        )?;
        Ok(())
    }

    /// Reads and deserializes a typed preference value by `key`.
    pub fn get_preference<T: DeserializeOwned>(&self, key: &str) -> StorageResult<Option<T>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM app_preferences WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    /// Lists raw preference records optionally filtered by `category`.
    pub fn list_preferences(
        &self,
        category: Option<&str>,
    ) -> StorageResult<Vec<AppPreferenceRecord>> {
        let mut out = Vec::new();
        if let Some(cat) = category {
            let mut stmt = self.conn.prepare(
                "SELECT key, value_json, category, updated_at
                 FROM app_preferences
                 WHERE category = ?1
                 ORDER BY key ASC",
            )?;
            let rows = stmt.query_map(params![cat], row_to_pref)?;
            for r in rows {
                out.push(r?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT key, value_json, category, updated_at
                 FROM app_preferences
                 ORDER BY category ASC, key ASC",
            )?;
            let rows = stmt.query_map([], row_to_pref)?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    }

    /// Deletes a preference by `key`.
    pub fn delete_preference(&self, key: &str) -> StorageResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM app_preferences WHERE key = ?1", params![key])?;
        Ok(count > 0)
    }
}

fn row_to_palette(row: &rusqlite::Row<'_>) -> rusqlite::Result<CustomPaletteRecord> {
    Ok(CustomPaletteRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        palette_kind: row.get(3)?,
        is_builtin: row.get::<_, i64>(4)? != 0,
        is_docked: row.get::<_, i64>(5)? != 0,
        show_color_names: row.get::<_, i64>(6)? != 0,
        sort_order: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn row_to_template(row: &rusqlite::Row<'_>) -> rusqlite::Result<TemplateRecord> {
    Ok(TemplateRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        category: row.get(2)?,
        description: row.get(3)?,
        template_path: row.get(4)?,
        width_mm: row.get(5)?,
        height_mm: row.get(6)?,
        bleed_mm: row.get(7)?,
        page_count: row.get::<_, i64>(8)? as u32,
        color_mode: row.get(9)?,
        thumbnail_png: row.get(10)?,
        is_builtin: row.get::<_, i64>(11)? != 0,
        favorite: row.get::<_, i64>(12)? != 0,
    })
}

fn row_to_workspace_layout(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceLayoutRecord> {
    Ok(WorkspaceLayoutRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        dock_tree_json: row.get(3)?,
        toolbox_config_json: row.get(4)?,
        shortcuts_json: row.get(5)?,
        is_builtin: row.get::<_, i64>(6)? != 0,
        is_active: row.get::<_, i64>(7)? != 0,
    })
}

fn row_to_brush(row: &rusqlite::Row<'_>) -> rusqlite::Result<BrushPresetRecord> {
    Ok(BrushPresetRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        engine_kind: row.get(2)?,
        category: row.get(3)?,
        size_px: row.get(4)?,
        hardness: row.get(5)?,
        spacing: row.get(6)?,
        opacity: row.get(7)?,
        pressure_curve_json: row.get(8)?,
        params_json: row.get(9)?,
        is_builtin: row.get::<_, i64>(10)? != 0,
        sort_order: row.get(11)?,
    })
}

fn row_to_pref(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppPreferenceRecord> {
    Ok(AppPreferenceRecord {
        key: row.get(0)?,
        value_json: row.get(1)?,
        category: row.get(2)?,
        updated_at: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_crud_across_recent_palettes_templates_layouts_brushes_and_preferences() {
        let mut db = StorageDb::open_in_memory().unwrap();

        // 1. recent_documents
        let r1 = RecentDocumentRecord {
            id: "rec-1".into(),
            file_path: "/projects/flyer.cdr".into(),
            display_name: "Flyer".into(),
            file_format: "cdr".into(),
            cdr_version: Some("X8".into()),
            page_count: 2,
            color_mode: "cmyk".into(),
            thumbnail_png: Some(vec![137, 80, 78, 71]),
            file_size_bytes: 120_000,
            pinned: false,
            last_opened_at: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let r2 = RecentDocumentRecord {
            id: "rec-2".into(),
            file_path: "/projects/logo.coree".into(),
            display_name: "Logo".into(),
            file_format: "coree".into(),
            cdr_version: None,
            page_count: 1,
            color_mode: "rgb".into(),
            thumbnail_png: None,
            file_size_bytes: 42_000,
            pinned: true,
            last_opened_at: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        db.upsert_recent_document(&r1).unwrap();
        db.upsert_recent_document(&r2).unwrap();
        let recents = db.list_recent_documents(10).unwrap();
        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].id, "rec-2"); // pinned first!

        // 2. custom_palettes + palette_swatches + foreign key cascade
        let mut pal = ColorPalette::new("Print Kit", ColorMode::Cmyk);
        pal.add_swatch(ColorSwatch::new("Cyan", Color::CYAN));
        pal.add_swatch(ColorSwatch::new("CutContour", Color::cut_contour_spot()));
        pal.add_swatch(ColorSwatch::new("Warm", Color::lab(70.0, 15.0, 20.0)));
        pal.add_swatch(ColorSwatch::new("Sky", Color::rgba8(50, 150, 250, 255)));
        let pid = db.save_core_palette(&pal, "cmyk", false, true).unwrap();
        let loaded = db.load_core_palette(&pid).unwrap();
        assert_eq!(loaded.name, "Print Kit");
        assert_eq!(loaded.swatches.len(), 4);
        assert!(db.delete_palette(&pid).unwrap());
        assert!(db.list_palette_swatches(&pid).unwrap().is_empty());

        // 3. templates
        db.upsert_template(&TemplateRecord {
            id: "tpl-box".into(),
            name: "Tuck End Carton".into(),
            category: "packaging".into(),
            description: "FEFCO carton box".into(),
            template_path: "/templates/tuck.cdt".into(),
            width_mm: 240.0,
            height_mm: 180.0,
            bleed_mm: 3.0,
            page_count: 1,
            color_mode: "cmyk".into(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: true,
        })
        .unwrap();
        assert_eq!(db.list_templates(Some("packaging")).unwrap().len(), 1);

        // 4. workspace_layouts
        db.upsert_workspace_layout(&WorkspaceLayoutRecord {
            id: "ws-default".into(),
            name: "Default Studio".into(),
            description: "Standard docking layout".into(),
            dock_tree_json: "{\"tabs\":[\"Properties\",\"Objects\"]}".into(),
            toolbox_config_json: "{\"columns\":1}".into(),
            shortcuts_json: "{\"F6\":\"Rectangle\"}".into(),
            is_builtin: true,
            is_active: false,
        })
        .unwrap();
        assert!(db.set_active_workspace_layout("ws-default").unwrap());
        assert_eq!(
            db.get_active_workspace_layout().unwrap().unwrap().name,
            "Default Studio"
        );

        // 5. brush_presets
        db.upsert_brush_preset(&BrushPresetRecord {
            id: "br-callig".into(),
            name: "Arabic Qalam".into(),
            engine_kind: "artistic_media".into(),
            category: "calligraphy".into(),
            size_px: 18.0,
            hardness: 1.0,
            spacing: 0.1,
            opacity: 1.0,
            pressure_curve_json: "[[0.0,0.2],[1.0,1.0]]".into(),
            params_json: "{\"angle_deg\":45}".into(),
            is_builtin: true,
            sort_order: 1,
        })
        .unwrap();
        assert_eq!(
            db.list_brush_presets(Some("artistic_media")).unwrap().len(),
            1
        );

        // 6. app_preferences (typed)
        db.set_preference("nudge_step_mm", "units", &0.5_f64)
            .unwrap();
        db.set_preference("autosave_interval_sec", "autosave", &30_u32)
            .unwrap();
        assert_eq!(
            db.get_preference::<f64>("nudge_step_mm").unwrap(),
            Some(0.5)
        );
        assert_eq!(
            db.get_preference::<u32>("autosave_interval_sec").unwrap(),
            Some(30)
        );
    }
}
