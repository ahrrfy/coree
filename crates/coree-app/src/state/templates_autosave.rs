// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Audits the active tab for missing font families (`CRLFOM` modal trigger on open/import)
    /// and prepares `PANOSE` perceptual fallback suggestions (`REQ-07`).
    pub fn check_missing_fonts_on_active_tab(&mut self, doc_title: &str) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let rules = PreflightRules {
            check_rgb_in_cmyk: false,
            warn_live_text: false,
            check_missing_fonts: true,
            check_bleed: false,
            check_layer_states: false,
            ..PreflightRules::default()
        };
        let report = run_preflight(&tab.document, &rules);
        let mut missing_families: Vec<String> = Vec::new();
        for issue in report.issues {
            if issue.kind == coree_prod::PreflightIssueKind::MissingFont {
                if let Some(rest) = issue.message.split("references font family '").nth(1) {
                    if let Some((fam, _)) = rest.split_once('\'') {
                        let fam = fam.trim();
                        if !fam.is_empty()
                            && !missing_families
                                .iter()
                                .any(|existing| existing.eq_ignore_ascii_case(fam))
                        {
                            missing_families.push(fam.to_owned());
                        }
                    }
                }
            }
        }
        if missing_families.is_empty() {
            return;
        }
        if self.font_manager_state.indexed_fonts.is_empty() && self.storage.is_some() {
            self.refresh_font_manager(true);
        }
        let mut entries = Vec::with_capacity(missing_families.len());
        for fam in missing_families {
            let stored = self
                .storage
                .as_ref()
                .and_then(|db| db.get_font_substitution(&fam).ok().flatten());
            let is_arabic = fam.to_ascii_lowercase().contains("arab")
                || fam.to_ascii_lowercase().contains("naskh")
                || fam.to_ascii_lowercase().contains("kufi");
            let panose_match = self.find_best_panose_substitute(
                &PanoseClassification::default(),
                400,
                false,
                is_arabic,
            );
            let fallback = if is_arabic {
                "Noto Sans Arabic"
            } else {
                "Noto Sans"
            };
            let suggested = panose_match
                .as_ref()
                .map(|(rec, _)| rec.family_name.clone())
                .unwrap_or_else(|| fallback.to_owned());
            let selected = stored
                .map(|s| s.substitute_family)
                .unwrap_or_else(|| suggested.clone());
            entries.push(MissingFontPromptEntry {
                missing_family: fam,
                suggested_family: suggested,
                selected_family: selected,
                panose_distance: panose_match.map(|(_, d)| d),
            });
        }
        self.missing_font_dialog = MissingFontPromptState {
            open: true,
            document_title: doc_title.to_owned(),
            entries,
            remember_rules: true,
        };
    }

    /// Applies the user's `CRLFOM` modal font substitutions to the active document via `CommandOp`
    /// and optionally persists each rule into SQLite `font_substitutions`.
    pub fn apply_missing_font_dialog_substitutions(&mut self) -> usize {
        let entries = self.missing_font_dialog.entries.clone();
        let remember = self.missing_font_dialog.remember_rules;
        let mut replaced_total = 0usize;
        for entry in &entries {
            let target = entry.selected_family.trim();
            if target.is_empty() {
                continue;
            }
            if remember && self.storage.is_some() {
                let _ = self.set_font_substitution(&entry.missing_family, target, None);
            }
            let query = FindQuery {
                scope: FindScope::WholeDocument,
                object_kind: None,
                fill_color: None,
                outline_color: None,
                outline_width_min_mm: None,
                outline_width_max_mm: None,
                is_hairline: None,
                font_family: Some(entry.missing_family.clone()),
                text_pattern: None,
                lens_kind: None,
                layer_id: None,
                layer_name: None,
            };
            let cmd = CommandOp::FindAndReplace {
                query,
                action: ReplaceAction::ReplaceFontFamily {
                    font_family: target.to_owned(),
                },
            };
            if self.execute_command(cmd).is_ok() {
                replaced_total += 1;
            }
        }
        self.missing_font_dialog.open = false;
        if replaced_total > 0 {
            self.push_toast(
                format!("Substituted fonts in {replaced_total} missing family rule(s)"),
                false,
            );
        }
        replaced_total
    }

    /// Creates a fresh document from a SQLite/built-in template (opening `.cdt` if present on disk,
    /// otherwise creating a configured `Document` with matching dimensions, bleed, color mode & pages).
    pub fn create_document_from_template(&mut self, tpl: &TemplateRecord) {
        if !tpl.template_path.trim().is_empty() && !tpl.template_path.starts_with("builtin://") {
            let path = Path::new(&tpl.template_path);
            if path.is_file() && self.open_file_path(path).is_ok() {
                return;
            }
        }
        let color_mode = if tpl.color_mode.eq_ignore_ascii_case("rgb") {
            ColorMode::Rgb
        } else {
            ColorMode::Cmyk
        };
        let mut doc = Document::new(
            &tpl.name,
            tpl.width_mm.max(10.0),
            tpl.height_mm.max(10.0),
            color_mode,
            300.0,
        );
        if let Some(p0) = doc.page_mut(0) {
            p0.bleed_mm = tpl.bleed_mm.max(0.0);
        }
        for p_idx in 1..tpl.page_count.max(1) {
            doc.add_page(
                format!("Page {}", p_idx + 1),
                tpl.width_mm.max(10.0),
                tpl.height_mm.max(10.0),
                tpl.bleed_mm.max(0.0),
            );
        }
        self.new_document(doc);
    }

    /// Saves the active document as a `.cdt` template file and registers it in SQLite `templates`.
    pub fn save_active_as_cdt_template(
        &mut self,
        path: &Path,
        category: &str,
        description: &str,
    ) -> Result<(), String> {
        let tpl_rec = {
            let tab = self
                .tabs
                .get(self.active_tab)
                .ok_or_else(|| "no active document".to_owned())?;
            let page0 = tab.document.pages().first();
            coree_io::save_coree(&tab.document, path).map_err(|e| e.to_string())?;
            TemplateRecord {
                id: format!("tpl-{}", Uuid::new_v4()),
                name: tab.document.title().to_owned(),
                category: if category.trim().is_empty() {
                    "custom".to_owned()
                } else {
                    category.trim().to_owned()
                },
                description: description.trim().to_owned(),
                template_path: path.to_string_lossy().into_owned(),
                width_mm: page0.map_or(210.0, |p| p.width_mm),
                height_mm: page0.map_or(297.0, |p| p.height_mm),
                bleed_mm: page0.map_or(3.0, |p| p.bleed_mm),
                page_count: tab.document.pages().len() as u32,
                color_mode: match tab.document.color_mode() {
                    ColorMode::Cmyk => "cmyk",
                    ColorMode::Rgb => "rgb",
                }
                .to_owned(),
                thumbnail_png: None,
                is_builtin: false,
                favorite: true,
            }
        };
        if let Some(ref db) = self.storage {
            db.upsert_template(&tpl_rec).map_err(|e| e.to_string())?;
            if let Ok(list) = db.list_templates(None) {
                self.templates = list;
            }
        } else {
            self.templates.push(tpl_rec);
        }
        self.push_toast(format!("Saved template '{}'", path.display()), false);
        Ok(())
    }
}

/// Built-in offline production templates seeded into SQLite `templates` on first run (`REQ-12`).
pub(super) fn builtin_default_templates() -> Vec<TemplateRecord> {
    vec![
        TemplateRecord {
            id: "builtin-a4-flyer".to_owned(),
            name: "A4 Print Flyer (CMYK 3mm Bleed)".to_owned(),
            category: "print".to_owned(),
            description: "ISO A4 210x297 mm CMYK press-ready layout with 3 mm bleed.".to_owned(),
            template_path: "builtin://builtin-a4-flyer".to_owned(),
            width_mm: 210.0,
            height_mm: 297.0,
            bleed_mm: 3.0,
            page_count: 1,
            color_mode: "cmyk".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: true,
        },
        TemplateRecord {
            id: "builtin-bizcard".to_owned(),
            name: "Business Card (90x50 mm)".to_owned(),
            category: "print".to_owned(),
            description: "Standard 90x50 mm front & back double-sided business card.".to_owned(),
            template_path: "builtin://builtin-bizcard".to_owned(),
            width_mm: 90.0,
            height_mm: 50.0,
            bleed_mm: 3.0,
            page_count: 2,
            color_mode: "cmyk".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: false,
        },
        TemplateRecord {
            id: "builtin-fefco-pkg".to_owned(),
            name: "Packaging Dieline Sheet (450x320 mm)".to_owned(),
            category: "packaging".to_owned(),
            description: "SRA3 450x320 mm packaging & CutContour dieline sheet.".to_owned(),
            template_path: "builtin://builtin-fefco-pkg".to_owned(),
            width_mm: 450.0,
            height_mm: 320.0,
            bleed_mm: 5.0,
            page_count: 1,
            color_mode: "cmyk".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: false,
        },
        TemplateRecord {
            id: "builtin-social-rgb".to_owned(),
            name: "Social Square Post (1080px / RGB)".to_owned(),
            category: "web-ui".to_owned(),
            description: "Square sRGB social media graphic preset.".to_owned(),
            template_path: "builtin://builtin-social-rgb".to_owned(),
            width_mm: 91.44,
            height_mm: 91.44,
            bleed_mm: 0.0,
            page_count: 1,
            color_mode: "rgb".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: false,
        },
    ]
}

pub(super) fn build_differential_patch_from_tab(
    tab: &DocumentTab,
    doc_id: &str,
    session_id: String,
    base_snapshot_id: String,
) -> DifferentialSnapshotPatch {
    let mut upserted_objects = Vec::with_capacity(tab.autosave_dirty_objects.len());
    let mut deleted_object_ids = Vec::new();
    for &oid in &tab.autosave_dirty_objects {
        if let Some(obj) = tab.document.object(oid) {
            upserted_objects.push((oid, obj.clone()));
        } else {
            deleted_object_ids.push(oid);
        }
    }
    let struct_dirty = tab.autosave_dirty_structure;
    let pal_dirty = struct_dirty || tab.autosave_dirty_palette;
    let mut upserted_assets = Vec::new();
    let mut deleted_asset_ids = Vec::new();
    if !struct_dirty {
        for &aid in &tab.autosave_upserted_assets {
            if let Some(asset) = tab.document.asset(aid) {
                upserted_assets.push((aid, asset.clone()));
            }
        }
        deleted_asset_ids.extend(tab.autosave_deleted_assets.iter().copied());
    }
    DifferentialSnapshotPatch {
        doc_id: doc_id.to_owned(),
        session_id,
        title: tab.document.title().to_owned(),
        source_path: tab
            .source_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        base_snapshot_id,
        last_command_seq: tab.journal_seq,
        upserted_objects,
        deleted_object_ids,
        upserted_assets,
        deleted_asset_ids,
        full_assets: struct_dirty.then(|| tab.document.assets().clone()),
        pages: struct_dirty.then(|| tab.document.pages().to_vec()),
        active_page_index: struct_dirty.then(|| tab.document.active_page_index()),
        next_id: Some(tab.document.next_id()),
        color_mode: struct_dirty.then(|| tab.document.color_mode()),
        default_unit: struct_dirty.then(|| tab.document.default_unit()),
        dpi: struct_dirty.then(|| tab.document.dpi()),
        multipage_view: struct_dirty.then(|| tab.document.multipage_view()),
        palettes: pal_dirty.then(|| tab.document.palettes().to_vec()),
        document_palette: pal_dirty.then(|| tab.document.document_palette().clone()),
        guides: struct_dirty.then(|| tab.document.guides().to_vec()),
        grid: struct_dirty.then(|| tab.document.grid()),
        perspective_grid: struct_dirty.then(|| tab.document.perspective_grid().clone()),
        color_styles: struct_dirty.then(|| tab.document.color_styles().to_vec()),
        object_styles: struct_dirty.then(|| tab.document.object_styles().to_vec()),
        symbols: struct_dirty.then(|| tab.document.symbols().clone()),
        metadata: struct_dirty.then(|| tab.document.metadata().clone()),
        custom_icc_profile: struct_dirty
            .then(|| tab.document.custom_icc_profile().map(<[u8]>::to_vec)),
        custom_icc_name: struct_dirty.then(|| tab.document.custom_icc_name().map(str::to_owned)),
        rgb_icc_profile: struct_dirty.then(|| tab.document.rgb_icc_profile().map(<[u8]>::to_vec)),
        rgb_icc_name: struct_dirty.then(|| tab.document.rgb_icc_name().map(str::to_owned)),
        cmyk_icc_profile: struct_dirty.then(|| tab.document.cmyk_icc_profile().map(<[u8]>::to_vec)),
        cmyk_icc_name: struct_dirty.then(|| tab.document.cmyk_icc_name().map(str::to_owned)),
        gray_icc_profile: struct_dirty.then(|| tab.document.gray_icc_profile().map(<[u8]>::to_vec)),
        gray_icc_name: struct_dirty.then(|| tab.document.gray_icc_name().map(str::to_owned)),
        rendering_intent: struct_dirty.then(|| tab.document.rendering_intent()),
        black_point_compensation: struct_dirty.then(|| tab.document.black_point_compensation()),
        drawing_scale: struct_dirty.then(|| tab.document.drawing_scale()),
        unknown_doc_chunks: struct_dirty.then(|| tab.document.unknown_doc_chunks().to_vec()),
        unknown_data_files: struct_dirty.then(|| tab.document.unknown_data_files().clone()),
    }
}

pub(super) fn clear_tab_autosave_dirty(tab: &mut DocumentTab) {
    tab.autosave_dirty_objects.clear();
    tab.autosave_dirty_structure = false;
    tab.autosave_dirty_palette = false;
    tab.autosave_upserted_assets.clear();
    tab.autosave_deleted_assets.clear();
    tab.autosave_dirty_all = false;
    tab.last_autosave_at = Instant::now();
}

pub(super) fn command_op_touches_structure(op: &CommandOp) -> bool {
    match op {
        CommandOp::TransformObjects { .. }
        | CommandOp::SetFill { .. }
        | CommandOp::SetOutline { .. }
        | CommandOp::EditPath { .. }
        | CommandOp::EditShape { .. }
        | CommandOp::EditBSpline { .. }
        | CommandOp::EditArtisticMedia { .. }
        | CommandOp::EditText { .. }
        | CommandOp::EditTable { .. }
        | CommandOp::EditDimension { .. }
        | CommandOp::SetOpacity { .. }
        | CommandOp::SetBlendMode { .. }
        | CommandOp::SetLens { .. }
        | CommandOp::SetObjectPerspective { .. }
        | CommandOp::SetObjectColorStyleLink { .. }
        | CommandOp::AddEffect { .. }
        | CommandOp::RemoveEffect { .. }
        | CommandOp::SetObjectVisibility { .. }
        | CommandOp::SetObjectLocked { .. }
        | CommandOp::SetObjectPrintable { .. }
        | CommandOp::SetPowerClipFillRule { .. }
        | CommandOp::RenameObject { .. }
        | CommandOp::UpdateRaster { .. } => false,
        CommandOp::Batch(ops) => ops.iter().any(command_op_touches_structure),
        _ => true,
    }
}
