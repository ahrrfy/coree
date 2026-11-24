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

    /// Toggles `locked` state on currently selected objects (or unlocks locked objects on active page if none selected).
    pub fn toggle_lock_selection(&mut self) {
        let (ids, target_lock) = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return;
            };
            if tab.selection.is_empty() {
                return;
            }
            let any_unlocked = tab
                .selection
                .objects
                .iter()
                .any(|id| tab.document.object(*id).is_some_and(|o| !o.locked));
            (tab.selection.objects.clone(), any_unlocked)
        };
        if let Err(err) = self.execute_command(CommandOp::SetObjectLocked {
            ids,
            locked: target_lock,
        }) {
            self.push_toast(format!("Lock failed: {err}"), true);
            return;
        }
        self.push_toast(
            if target_lock {
                "Locked selected object(s)"
            } else {
                "Unlocked selected object(s)"
            },
            false,
        );
    }

    /// Repeats the last executed command (`Ctrl+R`), targeting current selection for transform/style ops.
    pub fn repeat_last_command(&mut self) {
        let Some(last) = self.last_executed_op.clone() else {
            self.push_toast("Nothing to repeat yet", false);
            return;
        };
        let sel_ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if let Some(world_affine) = self
            .last_world_transform
            .filter(|_| !sel_ids.is_empty())
            .or(match &last {
                CommandOp::TransformObjects { affine, .. } if !sel_ids.is_empty() => Some(*affine),
                _ => None,
            })
        {
            if let Some(cmd) = self
                .active_tab()
                .and_then(|t| build_world_transform_command(&t.document, &sel_ids, world_affine))
            {
                match self.execute_command(cmd) {
                    Ok(()) => self.push_toast("Repeated last command", false),
                    Err(e) => self.push_toast(format!("Repeat failed: {e}"), true),
                }
            }
            return;
        }
        let adapted = match last {
            CommandOp::SetFill { fill, .. } if !sel_ids.is_empty() => {
                CommandOp::SetFill { ids: sel_ids, fill }
            }
            CommandOp::SetOutline { outline, .. } if !sel_ids.is_empty() => CommandOp::SetOutline {
                ids: sel_ids,
                outline,
            },
            CommandOp::SetLens { lens, .. } if !sel_ids.is_empty() => {
                CommandOp::SetLens { ids: sel_ids, lens }
            }
            CommandOp::AddEffect { effect, .. } if !sel_ids.is_empty() => {
                let ops = sel_ids
                    .into_iter()
                    .map(|id| CommandOp::AddEffect {
                        id,
                        effect: effect.clone(),
                    })
                    .collect();
                CommandOp::Batch(ops)
            }
            CommandOp::CreateObject {
                mut object,
                layer_id,
            } => {
                let offset = self
                    .active_tab()
                    .map(|t| t.duplicate_offset_mm)
                    .unwrap_or(Vec2D::new(5.0, 5.0));
                object.id = ObjectId(0);
                object.transform =
                    Affine2D::translate(offset.x, offset.y).compose(&object.transform);
                CommandOp::CreateObject { object, layer_id }
            }
            other => other,
        };
        match self.execute_command(adapted) {
            Ok(()) => self.push_toast("Repeated last command", false),
            Err(e) => self.push_toast(format!("Repeat failed: {e}"), true),
        }
    }

    /// Breaks apart Groups, PowerClips, attached Effects (lowering to independent vector
    /// primitives), or compound Paths (`Ctrl+K`).
    pub fn break_apart_selection(&mut self) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.push_toast("Select an object to Break Apart", false);
            return;
        }
        let mut ops = Vec::new();
        let mut path_ids = Vec::new();
        let mut new_selection = Vec::new();
        if let Some(tab) = self.active_tab() {
            let mut next_id = tab.document.next_id().max(
                tab.document
                    .objects()
                    .keys()
                    .next_back()
                    .map_or(1, |k| k.0.saturating_add(1)),
            );
            for &id in &ids {
                if let Some(obj) = tab.document.object(id) {
                    match &obj.payload {
                        NodePayload::Group(_) => ops.push(CommandOp::Ungroup { group_id: id }),
                        NodePayload::PowerClip { .. } => {
                            ops.push(CommandOp::ExtractPowerClip { powerclip_id: id });
                        }
                        _ if !obj.effects.is_empty() => {
                            let world_xf =
                                tab.document.world_transform_of(id).unwrap_or(obj.transform);
                            let layer_id = enclosing_layer_of_object(&tab.document, id)
                                .or(tab.active_layer_id);
                            let lowered = lower_effects_to_primitives(
                                &tab.document,
                                obj,
                                world_xf,
                                &mut || {
                                    let allocated = ObjectId(next_id);
                                    next_id = next_id.saturating_add(1);
                                    allocated
                                },
                            );
                            if lowered.is_empty() {
                                for idx in (0..obj.effects.len()).rev() {
                                    ops.push(CommandOp::RemoveEffect {
                                        id,
                                        effect_index: idx,
                                    });
                                }
                            } else {
                                ops.push(CommandOp::DeleteObjects { ids: vec![id] });
                                for piece in lowered {
                                    new_selection.push(piece.id);
                                    ops.push(CommandOp::create_object(piece, layer_id));
                                }
                            }
                        }
                        _ => path_ids.push(id),
                    }
                }
            }
        }
        if !path_ids.is_empty() {
            ops.push(CommandOp::BooleanOp {
                op: BooleanOp::BreakApart,
                ids: path_ids,
            });
        }
        if ops.is_empty() {
            return;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        match self.execute_command(cmd) {
            Ok(()) => {
                if !new_selection.is_empty() {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.selection.objects = new_selection;
                        tab.needs_rerender = true;
                    }
                }
                self.push_toast("Broke apart selection", false);
            }
            Err(e) => self.push_toast(format!("Break Apart failed: {e}"), true),
        }
    }

    /// Groups the current multi-object selection.
    pub fn group_selection(&mut self) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.len() < 2 {
            self.push_toast("Select at least 2 objects to Group", false);
            return;
        }
        if let Err(e) = self.execute_command(CommandOp::Group {
            ids,
            name: "Group".to_owned(),
        }) {
            self.push_toast(e, true);
        } else {
            self.push_toast("Grouped objects", false);
        }
    }

    /// Ungroups the selected Group object(s).
    pub fn ungroup_selection(&mut self) {
        let group_ids: Vec<ObjectId> = self
            .active_tab()
            .map(|t| {
                t.selection
                    .objects
                    .iter()
                    .copied()
                    .filter(|id| {
                        t.document
                            .object(*id)
                            .is_some_and(|o| matches!(o.payload, NodePayload::Group(_)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if group_ids.is_empty() {
            self.push_toast("Select a Group to Ungroup", false);
            return;
        }
        let ops = group_ids
            .into_iter()
            .map(|group_id| CommandOp::Ungroup { group_id })
            .collect();
        if let Err(e) = self.execute_command(CommandOp::Batch(ops)) {
            self.push_toast(e, true);
        } else {
            self.push_toast("Ungrouped selection", false);
        }
    }

    /// Applies any of the 9 vector Boolean/Shaping ops to the selection.
    pub fn apply_boolean_op(&mut self, op: BooleanOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.push_toast("Select vector objects first", false);
            return;
        }
        match self.execute_command(CommandOp::BooleanOp { op, ids }) {
            Ok(()) => self.push_toast(format!("Applied {op:?}"), false),
            Err(e) => self.push_toast(format!("{op:?} failed: {e}"), true),
        }
    }

    /// Aligns selected objects (`AlignOp`). For a single selected object, aligns it to the active page!
    pub fn apply_align(&mut self, op: AlignOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        if ids.len() == 1 && op != AlignOp::CenterPage {
            let shift = self.active_tab().and_then(|tab| {
                let page_rect = tab
                    .document
                    .active_page()
                    .map_or(Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0), |p| p.trim_rect());
                let bb = tab.document.world_object_bounds(ids[0])?;
                let (dx, dy) = match op {
                    AlignOp::Left => (page_rect.min_x - bb.min_x, 0.0),
                    AlignOp::CenterH => (page_rect.center().x - bb.center().x, 0.0),
                    AlignOp::Right => (page_rect.max_x - bb.max_x, 0.0),
                    AlignOp::Top => (0.0, page_rect.min_y - bb.min_y),
                    AlignOp::CenterV => (0.0, page_rect.center().y - bb.center().y),
                    AlignOp::Bottom => (0.0, page_rect.max_y - bb.max_y),
                    AlignOp::CenterPage => (
                        page_rect.center().x - bb.center().x,
                        page_rect.center().y - bb.center().y,
                    ),
                };
                let world_delta = Affine2D::translate(dx, dy);
                let local_delta = if let Some(pid) = tab.document.parent_container_of(ids[0]) {
                    if let Some(pw) = tab.document.world_transform_of(pid) {
                        if let Some(inv_pw) = pw.inverse() {
                            inv_pw.compose(&world_delta).compose(&pw)
                        } else {
                            world_delta
                        }
                    } else {
                        world_delta
                    }
                } else {
                    world_delta
                };
                Some(local_delta)
            });
            if let Some(affine) = shift {
                let _ = self.execute_command(CommandOp::TransformObjects { ids, affine });
            }
            return;
        }
        let _ = self.execute_command(CommandOp::AlignObjects { ids, op });
    }

    /// Distributes 3+ selected objects (`DistributeOp`).
    pub fn apply_distribute(&mut self, op: DistributeOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.len() < 3 {
            self.push_toast("Select at least 3 objects to Distribute", false);
            return;
        }
        let _ = self.execute_command(CommandOp::DistributeObjects { ids, op });
    }

    /// Changes Z-order of selected objects.
    pub fn apply_z_order(&mut self, op: ZOrderOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        let ops = ids
            .into_iter()
            .map(|id| CommandOp::SetZOrder { id, op })
            .collect();
        let _ = self.execute_command(CommandOp::Batch(ops));
    }

    /// Converts all selected shapes/text/tables to editable [`NodePayload::Path`] curves (`Ctrl+Q`).
    /// For text objects, extracts real glyph vector outlines via `coree_render` (falling back to
    /// `CommandOp::ConvertToCurves` if glyph outlines are empty).
    pub fn convert_selection_to_curves(&mut self) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            if tab.selection.objects.is_empty() {
                return;
            }
            let mut text_engine = TextEngine::new();
            tab.selection
                .objects
                .iter()
                .copied()
                .map(|id| {
                    if let Some(obj) = tab.document.object(id) {
                        if let Some(path) =
                            outline_text_object_with_engine(&mut text_engine, obj, &tab.document)
                        {
                            if !path.is_empty() && path.is_finite() {
                                return CommandOp::Batch(vec![
                                    CommandOp::ConvertToCurves { id },
                                    CommandOp::EditPath { id, path },
                                ]);
                            }
                        }
                    }
                    CommandOp::ConvertToCurves { id }
                })
                .collect()
        };
        match self.execute_command(CommandOp::Batch(ops)) {
            Ok(()) => self.push_toast("Converted selection to curves", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Converts visible outlines on selected objects into closed fillable vector objects (`Ctrl+Shift+Q`).
    pub fn convert_outline_to_object_selection(&mut self) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            if tab.selection.objects.is_empty() {
                return;
            }
            tab.selection
                .objects
                .iter()
                .copied()
                .filter(|id| {
                    is_object_editable_in_doc(&tab.document, *id)
                        && tab
                            .document
                            .object(*id)
                            .is_some_and(|obj| obj.outline.is_visible())
                })
                .map(|id| CommandOp::ConvertOutlineToObject { id })
                .collect()
        };
        if ops.is_empty() {
            self.push_toast("No unlocked objects with a visible outline selected", false);
            return;
        }
        let batch = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        match self.execute_command(batch) {
            Ok(()) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.metrics_dirty = true;
                    tab.needs_rerender = true;
                }
                self.push_toast("Converted outline to closed object", false);
            }
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Creates a PowerClip using the last-selected object as container and earlier ones as contents.
    pub fn create_powerclip_from_selection(&mut self) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.len() < 2 {
            self.push_toast(
                "Select content object(s) + container shape last for PowerClip",
                false,
            );
            return;
        }
        let container_id = *ids.last().unwrap();
        let content_ids = ids[..ids.len() - 1].to_vec();
        match self.execute_command(CommandOp::CreatePowerClip {
            container_id,
            content_ids,
        }) {
            Ok(()) => self.push_toast("Created PowerClip container", false),
            Err(e) => self.push_toast(format!("PowerClip error: {e}"), true),
        }
    }

    /// Extracts clipped contents from the selected PowerClip container.
    pub fn extract_selected_powerclip(&mut self) {
        let pc_id = self.active_tab().and_then(|t| {
            t.selection.objects.iter().copied().find(|id| {
                t.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::PowerClip { .. }))
            })
        });
        let Some(powerclip_id) = pc_id else {
            self.push_toast("Select a PowerClip container to extract", false);
            return;
        };
        match self.execute_command(CommandOp::ExtractPowerClip { powerclip_id }) {
            Ok(()) => {
                if let Some(tab) = self.active_tab_mut() {
                    if tab.powerclip_edit_id == Some(powerclip_id) {
                        tab.powerclip_edit_id = None;
                    }
                }
                self.push_toast("Extracted PowerClip contents", false);
            }
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Sets or toggles `Lock Contents to PowerClip` on selected PowerClip container(s).
    pub fn set_powerclip_lock_selected(&mut self, lock_contents: Option<bool>) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            let mut pc_states = Vec::new();
            for &id in &tab.selection.objects {
                if let Some(obj) = tab.document.object(id) {
                    if let NodePayload::PowerClip {
                        lock_contents: cur, ..
                    } = &obj.payload
                    {
                        pc_states.push((id, *cur));
                    }
                }
            }
            if pc_states.is_empty() {
                Vec::new()
            } else {
                let target = lock_contents.unwrap_or(!pc_states[0].1);
                pc_states
                    .into_iter()
                    .map(|(id, _)| CommandOp::SetPowerClipLock {
                        id,
                        lock_contents: target,
                    })
                    .collect()
            }
        };
        if ops.is_empty() {
            self.push_toast("Select a PowerClip container first", false);
            return;
        }
        let locked = matches!(
            ops[0],
            CommandOp::SetPowerClipLock {
                lock_contents: true,
                ..
            }
        );
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        match self.execute_command(cmd) {
            Ok(()) => self.push_toast(
                if locked {
                    "Locked contents to PowerClip"
                } else {
                    "Unlocked contents from PowerClip"
                },
                false,
            ),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Moves selected objects onto the studio's persistent cross-page `Desktop` layer.
    pub fn move_selection_to_desktop_layer(&mut self) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            tab.selection
                .objects
                .iter()
                .copied()
                .map(|id| CommandOp::MoveToDesktopLayer { id })
                .collect()
        };
        if ops.is_empty() {
            self.push_toast("Select object(s) to move to Desktop layer", false);
            return;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        match self.execute_command(cmd) {
            Ok(()) => self.push_toast("Moved selection to Desktop layer", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Applies `fill` to selected objects (or updates `default_fill` if nothing is selected).
    pub fn apply_fill(&mut self, fill: Fill) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.default_fill = fill;
            self.push_toast("Updated default Fill", false);
            return;
        }
        if let Err(e) = self.execute_command(CommandOp::SetFill { ids, fill }) {
            self.push_toast(e, true);
        }
    }

    /// Applies a solid color fill (`Left-click` on Palette Bar swatch).
    pub fn apply_fill_color(&mut self, color: Color) {
        self.apply_fill(Fill::Solid(color));
    }

    /// Applies an outline stroke color (`Right-click` on Palette Bar swatch), enabling 0.3mm width if previously none.
    pub fn apply_outline_color(&mut self, color: Color) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.default_outline.color = color;
            if !self.default_outline.is_visible() {
                self.default_outline.width_mm = 0.3;
            }
            self.push_toast("Updated default Outline color", false);
            return;
        }
        let mut ops = Vec::with_capacity(ids.len());
        if let Some(tab) = self.active_tab() {
            for &id in &ids {
                let mut outline = tab
                    .document
                    .object(id)
                    .map(|o| match &o.payload {
                        NodePayload::Table(tbl) => tbl.border_style.clone(),
                        _ => o.outline.clone(),
                    })
                    .unwrap_or_default();
                outline.color = color.clone();
                if !outline.is_visible() {
                    outline.width_mm = 0.3;
                }
                ops.push(CommandOp::SetOutline {
                    ids: vec![id],
                    outline,
                });
            }
        }
        if let Err(e) = self.execute_command(CommandOp::Batch(ops)) {
            self.push_toast(e, true);
        }
    }

    /// Removes fill (`Fill::None`) from selected objects (Left-click top `X` swatch).
    pub fn remove_fill(&mut self) {
        self.apply_fill(Fill::None);
    }

    /// Removes outline (`OutlineStyle::none()`) from selected objects (Right-click top `X` swatch).
    pub fn remove_outline(&mut self) {
        self.apply_outline(OutlineStyle::none());
    }

    /// Applies a complete [`OutlineStyle`] to selected objects (or sets default outline).
    pub fn apply_outline(&mut self, outline: OutlineStyle) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.default_outline = outline;
            return;
        }
        if let Err(e) = self.execute_command(CommandOp::SetOutline { ids, outline }) {
            self.push_toast(e, true);
        }
    }

    /// Applies a [`LensConfig`] (any of the 11 Lenses) to the selected object(s).
    pub fn apply_lens_to_selection(&mut self, lens: Option<LensConfig>) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.push_toast("Select a closed vector shape to apply a Lens", false);
            return;
        }
        let name = lens.as_ref().map_or("None", |l| l.kind.name());
        match self.execute_command(CommandOp::SetLens { ids, lens }) {
            Ok(()) => self.push_toast(format!("Applied Lens: {name}"), false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Adds a non-destructive [`Effect`] to all selected objects.
    pub fn add_effect_to_selection(&mut self, effect: Effect) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.push_toast("Select an object to add an Effect", false);
            return;
        }
        let ops = ids
            .into_iter()
            .map(|id| CommandOp::AddEffect {
                id,
                effect: effect.clone(),
            })
            .collect();
        match self.execute_command(CommandOp::Batch(ops)) {
            Ok(()) => self.push_toast("Added live vector effect", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Removes effect `effect_index` from `id`.
    pub fn remove_effect_on(&mut self, id: ObjectId, effect_index: usize) {
        let _ = self.execute_command(CommandOp::RemoveEffect { id, effect_index });
    }

    /// Fits a selected ArtisticText object onto a selected vector path/shape (`Text > Fit Text to Path`).
    pub fn fit_selected_text_to_path(&mut self) {
        let pair = self.active_tab().and_then(|tab| {
            let mut text_obj: Option<(ObjectId, ArtisticText)> = None;
            let mut path_id: Option<ObjectId> = None;
            for &id in &tab.selection.objects {
                if let Some(obj) = tab.document.object(id) {
                    match &obj.payload {
                        NodePayload::ArtisticText(t) if text_obj.is_none() => {
                            text_obj = Some((id, t.clone()));
                        }
                        NodePayload::Path(_) | NodePayload::Shape(_) if path_id.is_none() => {
                            path_id = Some(id);
                        }
                        _ => {}
                    }
                }
            }
            text_obj.zip(path_id)
        });

        let Some(((text_id, text), host_path_id)) = pair else {
            self.push_toast("Select one Artistic Text object and one curve/shape", false);
            return;
        };

        let payload = NodePayload::TextOnPath(TextOnPath {
            text,
            path_id: host_path_id,
            offset_mm: 0.0,
            distance_mm: 1.5,
            flip_vertical: false,
            flip_horizontal: false,
        });
        match self.execute_command(CommandOp::EditText {
            id: text_id,
            payload,
        }) {
            Ok(()) => self.push_toast("Fitted text to path", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Inserts a fresh [`VectorTable`] on the active page.
    pub fn insert_table(&mut self, rows: usize, cols: usize) {
        let Some(layer_id) = self.ensure_active_creation_layer() else {
            return;
        };
        let Some(tab) = self.active_tab() else {
            return;
        };
        let center = tab
            .document
            .active_page()
            .map_or(Point2D::new(40.0, 40.0), |p| {
                Point2D::new(p.width_mm * 0.2, p.height_mm * 0.2)
            });
        let table = VectorTable::new(rows.max(1), cols.max(1), 24.0, 10.0);
        let obj = SceneObject::new(
            ObjectId(0),
            format!("Table {rows}x{cols}"),
            NodePayload::Table(table),
        )
        .with_transform(Affine2D::translate(center.x, center.y));
        match self.execute_command(CommandOp::create_object(obj, layer_id)) {
            Ok(()) => self.push_toast(format!("Inserted {rows}x{cols} table"), false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Double-click Rectangle tool -> Page Frame; Shift+Double-click -> tight frame around selection.
    pub fn create_page_frame_rectangle(&mut self, around_selection: bool) -> Option<ObjectId> {
        let layer_id = self.ensure_active_creation_layer()?;
        let (target_rect, outline, is_sel_frame) = {
            let tab = self.active_tab()?;
            let page = tab.document.active_page()?;
            let page_rect = Rect2D::from_xywh(0.0, 0.0, page.width_mm, page.height_mm);
            let (rect, used_sel) = if around_selection && !tab.selection.is_empty() {
                tab.selection
                    .bounding_box(&tab.document)
                    .map_or((page_rect, false), |bb| (bb, true))
            } else {
                (page_rect, false)
            };
            let outline = if self.default_outline.is_visible() {
                self.default_outline.clone()
            } else {
                OutlineStyle {
                    color: Color::rgba8(0, 0, 0, 255),
                    width_mm: 0.25,
                    ..self.default_outline.clone()
                }
            };
            (rect, outline, used_sel)
        };
        let label = if is_sel_frame {
            "Selection Frame"
        } else {
            "Page Frame"
        };
        let obj = SceneObject::new(
            ObjectId(0),
            label.to_owned(),
            NodePayload::Shape(ParametricShape::Rectangle {
                width: target_rect.width().max(0.5),
                height: target_rect.height().max(0.5),
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(outline)
        .with_transform(Affine2D::translate(target_rect.min_x, target_rect.min_y));
        if let Err(e) = self.execute_command(CommandOp::create_object(obj, layer_id)) {
            self.push_toast(e, true);
            return None;
        }
        let created_id = if let Some(tab) = self.active_tab_mut() {
            let id = tab.document.objects().keys().last().copied();
            if let Some(oid) = id {
                tab.selection.select_single(&tab.document, oid);
            }
            tab.metrics_dirty = true;
            tab.needs_rerender = true;
            id
        } else {
            None
        };
        self.push_toast(
            if is_sel_frame {
                "Created frame around selection"
            } else {
                "Created Page Frame rectangle"
            },
            false,
        );
        created_id
    }

    /// Enters PowerClip internal content-editing mode (`Ctrl+Click` container).
    pub fn enter_powerclip_edit(&mut self, pc_id: ObjectId) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        tab.cancel_active_drag();
        let _ = tab.tool_core.cancel_all();
        let first_child = match tab.document.object(pc_id).map(|o| &o.payload) {
            Some(NodePayload::PowerClip { contents, .. }) => contents
                .iter()
                .rev()
                .copied()
                .find(|cid| is_object_editable_in_doc(&tab.document, *cid)),
            _ => return false,
        };
        tab.powerclip_edit_id = Some(pc_id);
        if let Some(cid) = first_child {
            tab.selection.select_single(&tab.document, cid);
        } else {
            tab.selection.clear();
        }
        tab.metrics_dirty = true;
        tab.needs_rerender = true;
        self.push_toast(
            "Editing PowerClip contents — Ctrl+Click outside or Esc to finish",
            false,
        );
        true
    }

    /// Exits PowerClip content-editing mode and restores selection to the parent container.
    pub fn exit_powerclip_edit(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        tab.cancel_active_drag();
        let _ = tab.tool_core.cancel_all();
        if let Some(pc_id) = tab.powerclip_edit_id.take() {
            if tab.document.has_object(pc_id) {
                tab.selection.select_single(&tab.document, pc_id);
            } else {
                tab.selection.clear();
            }
            tab.metrics_dirty = true;
            tab.needs_rerender = true;
        }
    }
}
