// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn resolve_paste_target_layer(doc: &Document, active_layer_id: Option<LayerId>) -> Option<LayerId> {
    let active_page = doc.active_page()?;
    if let Some(lid) = active_layer_id {
        let on_active_page = active_page.layers.iter().find(|l| l.id == lid);
        let desktop_layer = doc.find_layer(lid).filter(|l| l.is_desktop_layer);
        if let Some(layer) = on_active_page.or(desktop_layer) {
            if layer.visible && !layer.locked {
                return Some(layer.id);
            }
        }
    }
    active_page
        .layers
        .iter()
        .rev()
        .find(|l| l.visible && !l.locked)
        .map(|l| l.id)
}

impl StudioApp {
    // -------------------------------------------------------------------------
    // Core Scene Operations (Selection, Transform, Shaping, Fill, Outline, Lens, Effects)
    // -------------------------------------------------------------------------

    /// Duplicates all currently selected objects via undoable, WAL-journaled [`CommandOp::Batch`].
    ///
    /// Uses smart step-and-repeat: if the current selection matches the immediately preceding
    /// duplicate batch, repeats the cumulative delta transform (including any interactive Move,
    /// Scale, Rotate, or Skew applied right after duplicating); otherwise uses `tab.duplicate_offset_mm`.
    pub fn duplicate_selection(&mut self) -> Result<Vec<ObjectId>, String> {
        enum DupPlan {
            Affine(Affine2D),
            Offset(Vec2D),
        }
        let plan = self
            .active_tab()
            .map(|t| {
                if let Some((ref batch_ids, delta_xf)) = t.last_duplicate_batch {
                    if !batch_ids.is_empty() && *batch_ids == t.selection.objects {
                        return DupPlan::Affine(delta_xf);
                    }
                }
                DupPlan::Offset(t.duplicate_offset_mm)
            })
            .unwrap_or(DupPlan::Offset(Vec2D::new(5.0, 5.0)));
        match plan {
            DupPlan::Affine(delta) => self.duplicate_selection_with_affine(delta),
            DupPlan::Offset(offset) => self.duplicate_selection_with_offset(offset),
        }
    }

    /// Duplicates all currently selected objects with an explicit `offset` (used by
    /// both `Ctrl+D` and Transform Docker "Apply to Duplicate" with zero offset).
    pub fn duplicate_selection_with_offset(
        &mut self,
        offset: Vec2D,
    ) -> Result<Vec<ObjectId>, String> {
        self.duplicate_selection_with_affine(Affine2D::translate(offset.x, offset.y))
    }

    fn build_duplicate_ops_for_selection(
        &self,
        delta_xf: Affine2D,
    ) -> Result<(Vec<CommandOp>, Vec<ObjectId>), String> {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return Err("no active document".to_owned());
        };
        if tab.selection.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let ids = tab
            .document
            .filter_topmost_selection(&tab.selection.objects);
        let mut next_id = tab.document.next_id().max(
            tab.document
                .objects()
                .keys()
                .next_back()
                .map_or(1, |id| id.0.saturating_add(1)),
        );
        let mut id_map = HashMap::new();
        let mut ops = Vec::new();
        let mut created = Vec::new();
        for src_id in ids {
            let layer_id = enclosing_layer_of_object(&tab.document, src_id).or(tab.active_layer_id);
            let parent_world = tab
                .document
                .parent_container_of(src_id)
                .and_then(|pid| tab.document.world_transform_of(pid))
                .unwrap_or(Affine2D::IDENTITY);
            let root_delta = delta_xf.compose(&parent_world);
            let mut visiting = HashSet::new();
            if let Some(new_id) = build_duplicate_tree_ops(
                &tab.document,
                src_id,
                layer_id,
                root_delta,
                0,
                &mut next_id,
                &mut visiting,
                &mut id_map,
                &mut ops,
            ) {
                created.push(new_id);
            }
        }
        remap_duplicate_ops(&mut ops, &id_map);
        ops.retain(|op| {
            !matches!(
                op,
                CommandOp::SetObjectStyleLink {
                    style_id: Some(sid),
                    ..
                } if tab.document.object_style(*sid).is_none()
            )
        });
        Ok((ops, created))
    }

    /// Duplicates all currently selected objects with an arbitrary `delta_xf` affine
    /// transform (supporting Move, Scale, Rotate, Skew, and FreeTransform stamping).
    pub fn duplicate_selection_with_affine(
        &mut self,
        delta_xf: Affine2D,
    ) -> Result<Vec<ObjectId>, String> {
        let (ops, created) = self.build_duplicate_ops_for_selection(delta_xf)?;
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        self.execute_command(CommandOp::Batch(ops))?;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.selection.select_many(&tab.document, &created);
            tab.last_duplicate_batch = Some((created.clone(), delta_xf));
            tab.metrics_dirty = true;
        }
        self.push_toast(format!("Duplicated {} object(s)", created.len()), false);
        Ok(created)
    }

    /// Duplicates page `src_idx`, cloning its size, bleed, layers, and every object subtree
    /// via a single undoable [`CommandOp::Batch`].
    pub fn duplicate_page(&mut self, src_idx: usize) {
        let plan = self.active_tab().and_then(|tab| {
            let src_page = tab.document.page(src_idx)?;
            let mut next_id = tab.document.next_id().max(1);
            let new_page_id = PageId(next_id);
            next_id = next_id.saturating_add(1);
            let first_layer_id = LayerId(next_id);
            next_id = next_id.saturating_add(1);

            let mut ops = vec![CommandOp::AddPage {
                name: format!("{} Copy", src_page.name),
                width_mm: src_page.width_mm,
                height_mm: src_page.height_mm,
                bleed_mm: src_page.bleed_mm,
            }];
            if src_page.facing_pages {
                ops.push(CommandOp::SetPageProperties {
                    page_id: new_page_id,
                    name: None,
                    width_mm: None,
                    height_mm: None,
                    bleed_mm: None,
                    facing_pages: Some(true),
                    background_fill: None,
                    printable_background: None,
                });
            }

            // Map each source layer onto a corresponding target layer on the new page
            let mut layer_targets: Vec<(LayerId, Vec<ObjectId>)> = Vec::new();
            for (l_idx, src_layer) in src_page.layers.iter().enumerate() {
                let dst_lid = if l_idx == 0 {
                    ops.push(CommandOp::RenameLayer {
                        layer_id: first_layer_id,
                        new_name: src_layer.name.clone(),
                    });
                    ops.push(CommandOp::SetLayerFlags {
                        layer_id: first_layer_id,
                        visible: Some(src_layer.visible),
                        locked: Some(false), // keep unlocked while inserting cloned objects
                        printable: Some(src_layer.printable),
                        is_cut_layer: Some(src_layer.is_cut_layer),
                    });
                    first_layer_id
                } else {
                    let lid = LayerId(next_id);
                    next_id = next_id.saturating_add(1);
                    ops.push(CommandOp::AddLayer {
                        page_id: new_page_id,
                        name: src_layer.name.clone(),
                        is_cut_layer: src_layer.is_cut_layer,
                        master_scope: src_layer.master_scope,
                    });
                    ops.push(CommandOp::SetLayerFlags {
                        layer_id: lid,
                        visible: Some(src_layer.visible),
                        locked: Some(false),
                        printable: Some(src_layer.printable),
                        is_cut_layer: Some(src_layer.is_cut_layer),
                    });
                    lid
                };
                layer_targets.push((dst_lid, src_layer.objects.clone()));
            }

            // Clone every object subtree on every layer
            let mut id_map = HashMap::new();
            for (dst_lid, objs) in &layer_targets {
                for &src_oid in objs {
                    let mut visiting = HashSet::new();
                    let _ = build_duplicate_tree_ops(
                        &tab.document,
                        src_oid,
                        Some(*dst_lid),
                        Affine2D::IDENTITY,
                        0,
                        &mut next_id,
                        &mut visiting,
                        &mut id_map,
                        &mut ops,
                    );
                }
            }
            remap_duplicate_ops(&mut ops, &id_map);

            // Restore locked flags for any locked source layers after inserting objects
            for (l_idx, src_layer) in src_page.layers.iter().enumerate() {
                if src_layer.locked {
                    let dst_lid = layer_targets[l_idx].0;
                    ops.push(CommandOp::SetLayerFlags {
                        layer_id: dst_lid,
                        visible: None,
                        locked: Some(true),
                        printable: None,
                        is_cut_layer: None,
                    });
                }
            }

            ops.push(CommandOp::SetDocumentProperties {
                title: None,
                color_mode: None,
                default_unit: None,
                dpi: None,
                multipage_view: None,
                active_page_index: Some(tab.document.pages().len()),
                metadata: None,
                custom_icc_profile: None,
                clear_custom_icc: false,
                rgb_icc_profile: None,
                clear_rgb_icc: false,
                cmyk_icc_profile: None,
                clear_cmyk_icc: false,
                gray_icc_profile: None,
                clear_gray_icc: false,
                rendering_intent: None,
                black_point_compensation: None,
                drawing_scale: None,
            });

            Some(ops)
        });

        if let Some(ops) = plan {
            if let Err(err) = self.execute_command(CommandOp::Batch(ops)) {
                self.push_toast(format!("Duplicate page failed: {err}"), true);
                return;
            }
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.selection.clear();
                tab.sync_active_layer();
                tab.needs_rerender = true;
            }
            self.push_toast("Duplicated page with layers & objects", false);
        }
    }

    /// Deletes currently selected objects via undoable [`CommandOp::DeleteObjects`].
    pub fn delete_selection(&mut self) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            let gid = self.active_tab().and_then(|t| t.selected_guide_id);
            if let Some(guide_id) = gid {
                if self
                    .execute_command(CommandOp::RemoveGuide { id: guide_id })
                    .is_ok()
                {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.selected_guide_id = None;
                    }
                    self.push_toast("Removed selected guide line", false);
                }
            }
            return;
        }
        match self.execute_command(CommandOp::DeleteObjects { ids }) {
            Ok(()) => self.push_toast("Deleted selection", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Selects all visible, unlocked top-level objects on the active page.
    pub fn select_all(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let ids: Vec<ObjectId> = tab
            .document
            .active_page()
            .map(|p| {
                p.layers
                    .iter()
                    .filter(|l| l.visible && !l.locked)
                    .flat_map(|l| l.objects.iter().copied())
                    .filter(|oid| {
                        tab.document
                            .object(*oid)
                            .is_some_and(|o| o.visible && !o.locked)
                    })
                    .collect()
            })
            .unwrap_or_default();
        tab.selection.select_many(&tab.document, &ids);
        tab.metrics_dirty = true;
    }

    /// Nudges selected objects by `(dx_steps, dy_steps) * nudge_mm * effective_mult`.
    pub fn nudge_selection(&mut self, dx_steps: i32, dy_steps: i32, mult: i32) {
        let cmd = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            if tab.selection.is_empty() {
                return;
            }
            let scale = if mult > 1 {
                tab.super_nudge_factor.max(1.0)
            } else {
                f64::from(mult.max(1))
            };
            let step = tab.nudge_mm * scale;
            let affine =
                Affine2D::translate(f64::from(dx_steps) * step, f64::from(dy_steps) * step);
            build_world_transform_command(&tab.document, &tab.selection.objects, affine)
        };
        if let Some(cmd) = cmd {
            let _ = self.execute_command(cmd);
        }
    }

    /// Micro-nudges selected objects by `(dx_steps, dy_steps) * (nudge_mm / micro_nudge_divisor)` (`Ctrl+Arrow`).
    pub fn micro_nudge_selection(&mut self, dx_steps: i32, dy_steps: i32) {
        let cmd = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            if tab.selection.is_empty() {
                return;
            }
            let divisor = tab.micro_nudge_divisor.max(1.0);
            let step = tab.nudge_mm / divisor;
            let affine =
                Affine2D::translate(f64::from(dx_steps) * step, f64::from(dy_steps) * step);
            build_world_transform_command(&tab.document, &tab.selection.objects, affine)
        };
        if let Some(cmd) = cmd {
            let _ = self.execute_command(cmd);
        }
    }

    /// Copies the selected objects (including full `Group` / `PowerClip` descendant subtrees,
    /// referenced symbol definitions, raster assets, and fallback geometry) into the studio's internal
    /// clipboard AND publishes live `SVG`, `PDF`, `[MS-EMF]`, and `PNG/RGBA8` representations to the OS clipboard.
    pub fn copy_selection(&mut self) -> usize {
        let captured = self.active_tab().map(|t| {
            let source_doc_id = t.document.id();
            let mut roots = Vec::new();
            let mut copied = Vec::new();
            let mut snap_doc = Document::new_a4("Clipboard");
            let mut seen_roots = HashSet::new();
            let mut visiting_syms = HashSet::new();
            let mut text_on_path_fallbacks = HashMap::new();
            let topmost = t.document.filter_topmost_selection(&t.selection.objects);
            for id in topmost {
                if !seen_roots.insert(id) {
                    continue;
                }
                if let Some(obj) = t.document.object(id) {
                    let parent_world = t
                        .document
                        .parent_container_of(id)
                        .and_then(|pid| t.document.world_transform_of(pid))
                        .unwrap_or(Affine2D::IDENTITY);
                    roots.push(id);
                    let mut root_copy = obj.clone();
                    root_copy.transform = parent_world.compose(&root_copy.transform);
                    copied.push(root_copy);
                    let mut visiting = HashSet::new();
                    collect_subtree_into_snapshot(
                        &t.document,
                        &mut snap_doc,
                        id,
                        0,
                        parent_world,
                        &mut visiting,
                        &mut visiting_syms,
                        &mut text_on_path_fallbacks,
                    );
                }
            }
            let clipboard_payload = if !roots.is_empty() {
                coree_io::build_clipboard_document(&t.document, &roots, Some(&snap_doc))
                    .and_then(|export_doc| coree_io::encode_clipboard_payload(&export_doc, 0))
                    .ok()
            } else {
                None
            };
            (
                copied,
                source_doc_id,
                roots,
                snap_doc,
                text_on_path_fallbacks,
                clipboard_payload,
            )
        });
        let Some((
            copied,
            source_doc_id,
            roots,
            snap_doc,
            text_on_path_fallbacks,
            clipboard_payload,
        )) = captured
        else {
            return 0;
        };
        let count = copied.len();
        if count > 0 {
            self.clipboard_objects = copied;
            self.clipboard_snapshot = Some(ClipboardSnapshot {
                source_doc_id,
                roots,
                document: snap_doc,
                text_on_path_fallbacks,
            });
            if let Some(payload) = clipboard_payload {
                self.last_clipboard_hash = Some(payload.content_hash);
                let _ = self.clipboard_backend.write_payload(&payload);
            }
            self.push_toast(format!("Copied {count} object(s)"), false);
        }
        count
    }

    /// Cuts the selected objects into the studio & OS clipboard, cancels active drags, and removes them undoably.
    pub fn cut_selection(&mut self) -> usize {
        let count = self.copy_selection();
        if count > 0 {
            if let Some(tab) = self.active_tab_mut() {
                tab.cancel_active_drag();
            }
            self.delete_selection();
        }
        count
    }

    /// Pastes objects (with deep-cloned `Group`/`PowerClip` subtrees, referenced symbols/assets,
    /// and sanitized cross-document references) from the studio or OS vector/raster clipboard
    /// (`SVG > PDF > PNG > EMF > RGBA8`) onto the active unlocked layer via undoable `Batch`.
    pub fn paste_clipboard(&mut self) -> Vec<ObjectId> {
        let external_opt = self.clipboard_backend.read_payload().ok().flatten();
        let has_external = external_opt.as_ref().is_some_and(|e| e.has_any());
        let has_internal = !self.clipboard_objects.is_empty() || self.clipboard_snapshot.is_some();
        if !has_internal && !has_external {
            return Vec::new();
        }

        // Guard visible & unlocked target layer FIRST (never mutate or decode onto locked/hidden layers).
        let target_layer_check = self
            .active_tab()
            .map(|tab| resolve_paste_target_layer(&tab.document, tab.active_layer_id).is_some());
        match target_layer_check {
            None => return Vec::new(),
            Some(false) => {
                self.push_toast(
                    "Cannot paste: active page has no visible unlocked layer",
                    true,
                );
                return Vec::new();
            }
            Some(true) => {}
        }

        // Prefer external OS clipboard when internal clipboard is empty OR external content hash changed.
        let use_external = has_external
            && (!has_internal
                || external_opt.as_ref().is_some_and(|e| {
                    e.content_hash != 0 && Some(e.content_hash) != self.last_clipboard_hash
                }));
        if use_external {
            if let Some(ext) = external_opt {
                let decoded_res = if ext.svg.is_some()
                    || ext.pdf.is_some()
                    || ext.emf.is_some()
                    || ext.png.is_some()
                {
                    coree_io::decode_clipboard_auto(
                        ext.svg.as_deref(),
                        ext.pdf.as_deref(),
                        ext.emf.as_deref(),
                        ext.png.as_deref(),
                    )
                    .map(|(doc, fmt)| (doc, fmt.label().to_owned()))
                } else if let Some((w, h, ref rgba)) = ext.rgba {
                    coree_io::decode_clipboard_rgba(w, h, rgba, 96.0)
                        .map(|doc| (doc, "RGBA".to_owned()))
                } else {
                    Err(coree_io::IoError::InvalidFormat {
                        format: "clipboard".into(),
                        message: "empty external clipboard".into(),
                    })
                };

                if let Ok((decoded_doc, fmt_label)) = decoded_res {
                    let (ops, created) = match self.active_tab() {
                        Some(tab) => build_import_document_ops_on_layer(
                            &tab.document,
                            &decoded_doc,
                            tab.active_layer_id,
                        ),
                        None => (Vec::new(), Vec::new()),
                    };
                    if !ops.is_empty() && !created.is_empty() {
                        let cmd = if ops.len() == 1 {
                            ops.into_iter().next().expect("len == 1")
                        } else {
                            CommandOp::Batch(ops)
                        };
                        if self.execute_command(cmd).is_ok() {
                            let count = created.len();
                            if let Some(tab) = self.active_tab_mut() {
                                tab.selection.select_many(&tab.document, &created);
                                tab.metrics_dirty = true;
                                tab.needs_rerender = true;
                            }
                            self.push_toast(
                                format!("Pasted {count} object(s) from OS clipboard ({fmt_label})"),
                                false,
                            );
                            return created;
                        }
                    }
                }
            }
        }

        if let Some(snapshot) = self.clipboard_snapshot.clone() {
            if !snapshot.roots.is_empty() {
                enum PasteSnapshotPlan {
                    NoTab,
                    NoUnlockedLayer,
                    Ready(Vec<CommandOp>, Vec<ObjectId>),
                }
                let plan = match self.active_tab() {
                    None => PasteSnapshotPlan::NoTab,
                    Some(tab) => {
                        if let Some(target_layer_id) =
                            resolve_paste_target_layer(&tab.document, tab.active_layer_id)
                        {
                            let mut next_id = tab.document.next_id().max(
                                tab.document
                                    .objects()
                                    .keys()
                                    .next_back()
                                    .map_or(1, |id| id.0.saturating_add(1)),
                            );
                            let shift = Affine2D::translate(
                                tab.duplicate_offset_mm.x,
                                tab.duplicate_offset_mm.y,
                            );
                            let mut id_map = HashMap::new();
                            let mut obj_ops = Vec::new();
                            let mut created = Vec::new();
                            for &src_id in &snapshot.roots {
                                let mut visiting = HashSet::new();
                                if let Some(new_id) = build_duplicate_tree_ops(
                                    &snapshot.document,
                                    src_id,
                                    Some(target_layer_id),
                                    shift,
                                    0,
                                    &mut next_id,
                                    &mut visiting,
                                    &mut id_map,
                                    &mut obj_ops,
                                ) {
                                    created.push(new_id);
                                }
                            }
                            let same_doc = tab.document.id() == snapshot.source_doc_id;
                            remap_and_sanitize_paste_ops(
                                &mut obj_ops,
                                &id_map,
                                &snapshot.document,
                                &snapshot.text_on_path_fallbacks,
                                shift,
                                &tab.document,
                                same_doc,
                            );
                            let mut ops = Vec::new();
                            if !obj_ops.is_empty() {
                                let mut needed_assets = HashSet::new();
                                let mut needed_symbols = HashSet::new();
                                for op in &obj_ops {
                                    match op {
                                        CommandOp::CreateObject { object, .. }
                                        | CommandOp::CreateDetachedObject { object } => {
                                            object.collect_asset_refs(&mut needed_assets);
                                            collect_symbol_refs_from_object(
                                                object,
                                                &mut needed_symbols,
                                            );
                                        }
                                        CommandOp::SetFill { fill, .. } => {
                                            if let Fill::BitmapPattern { image_id, .. } = fill {
                                                needed_assets.insert(*image_id);
                                            } else if let Fill::VectorPattern {
                                                symbol_id, ..
                                            } = fill
                                            {
                                                needed_symbols.insert(*symbol_id);
                                            }
                                        }
                                        CommandOp::EditTable { table, .. } => {
                                            for cell in &table.cells {
                                                if let Fill::BitmapPattern { image_id, .. } =
                                                    cell.fill
                                                {
                                                    needed_assets.insert(image_id);
                                                } else if let Fill::VectorPattern {
                                                    symbol_id,
                                                    ..
                                                } = cell.fill
                                                {
                                                    needed_symbols.insert(symbol_id);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                let mut sym_queue: Vec<SymbolId> =
                                    needed_symbols.iter().copied().collect();
                                while let Some(sid) = sym_queue.pop() {
                                    if let Some(sym) = snapshot.document.symbol(sid) {
                                        for sub in &sym.objects {
                                            sub.collect_asset_refs(&mut needed_assets);
                                            let mut nested = HashSet::new();
                                            collect_symbol_refs_from_object(sub, &mut nested);
                                            for nsid in nested {
                                                if needed_symbols.insert(nsid) {
                                                    sym_queue.push(nsid);
                                                }
                                            }
                                        }
                                    }
                                }
                                for asset in snapshot.document.assets().values() {
                                    if needed_assets.contains(&asset.id) {
                                        ops.push(CommandOp::RegisterAsset {
                                            asset: asset.clone(),
                                        });
                                    }
                                }
                                for sym in snapshot.document.symbols().values() {
                                    if needed_symbols.contains(&sym.id)
                                        && tab.document.symbol(sym.id).is_none()
                                    {
                                        let mut sanitized_sym = sym.clone();
                                        for sub in &mut sanitized_sym.objects {
                                            if sub.style_id.is_some_and(|sid| {
                                                tab.document.object_style(sid).is_none()
                                            }) {
                                                sub.style_id = None;
                                            }
                                        }
                                        ops.push(CommandOp::UpsertSymbol {
                                            symbol: sanitized_sym,
                                        });
                                    }
                                }
                                ops.extend(obj_ops);
                            }
                            PasteSnapshotPlan::Ready(ops, created)
                        } else {
                            PasteSnapshotPlan::NoUnlockedLayer
                        }
                    }
                };
                let (ops, created) = match plan {
                    PasteSnapshotPlan::NoTab => return Vec::new(),
                    PasteSnapshotPlan::NoUnlockedLayer => {
                        self.push_toast(
                            "Cannot paste: active page has no visible unlocked layer",
                            true,
                        );
                        return Vec::new();
                    }
                    PasteSnapshotPlan::Ready(ops, created) => (ops, created),
                };
                if ops.is_empty() {
                    return Vec::new();
                }
                return if self.execute_command(CommandOp::Batch(ops)).is_ok() {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.selection.select_many(&tab.document, &created);
                        tab.metrics_dirty = true;
                    }
                    self.push_toast(format!("Pasted {} object(s)", created.len()), false);
                    created
                } else {
                    Vec::new()
                };
            }
        }

        // Fallback if a caller populated `clipboard_objects` directly without `copy_selection`
        let (ops, created) = {
            let Some(t) = self.active_tab() else {
                return Vec::new();
            };
            let Some(target_layer_id) = resolve_paste_target_layer(&t.document, t.active_layer_id)
            else {
                self.push_toast(
                    "Cannot paste: active page has no visible unlocked layer",
                    true,
                );
                return Vec::new();
            };
            let mut next_id = t.document.next_id().max(
                t.document
                    .objects()
                    .keys()
                    .next_back()
                    .map_or(1, |id| id.0.saturating_add(1)),
            );
            let mut ops = Vec::with_capacity(self.clipboard_objects.len());
            let mut created = Vec::with_capacity(self.clipboard_objects.len());
            let shift = Affine2D::translate(t.duplicate_offset_mm.x, t.duplicate_offset_mm.y);
            for src in &self.clipboard_objects {
                let mut obj = src.clone();
                let nid = ObjectId(next_id);
                next_id = next_id.saturating_add(1);
                obj.id = nid;
                obj.transform = shift.compose(&obj.transform);
                if obj
                    .style_id
                    .is_some_and(|sid| t.document.object_style(sid).is_none())
                {
                    obj.style_id = None;
                }
                match &mut obj.payload {
                    NodePayload::TextOnPath(top) if !t.document.has_object(top.path_id) => {
                        obj.payload = NodePayload::ArtisticText(top.text.clone());
                    }
                    NodePayload::Connector(conn) => {
                        if conn.from_obj.is_some_and(|fid| !t.document.has_object(fid)) {
                            conn.from_obj = None;
                        }
                        if conn.to_obj.is_some_and(|tid| !t.document.has_object(tid)) {
                            conn.to_obj = None;
                        }
                    }
                    NodePayload::ParagraphText(para)
                        if para
                            .linked_next_frame
                            .is_some_and(|nid| !t.document.has_object(nid)) =>
                    {
                        para.linked_next_frame = None;
                    }
                    _ => {}
                }
                obj.effects.retain(|eff| match eff {
                    Effect::Blend { target_id, .. } => t.document.has_object(*target_id),
                    _ => true,
                });
                for eff in &mut obj.effects {
                    if let Effect::Blend { along_path, .. } = eff {
                        if along_path.is_some_and(|ap| !t.document.has_object(ap)) {
                            *along_path = None;
                        }
                    }
                }
                created.push(nid);
                ops.push(CommandOp::create_object(obj, Some(target_layer_id)));
            }
            (ops, created)
        };
        if self.execute_command(CommandOp::Batch(ops)).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.select_many(&tab.document, &created);
                tab.metrics_dirty = true;
            }
            self.push_toast(format!("Pasted {} object(s)", created.len()), false);
            created
        } else {
            Vec::new()
        }
    }
}

mod ops_and_isolation;
