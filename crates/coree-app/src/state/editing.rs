// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    // -------------------------------------------------------------------------
    // Core Scene Operations (Selection, Transform, Shaping, Fill, Outline, Lens, Effects)
    // -------------------------------------------------------------------------

    /// Duplicates all currently selected objects by `tab.duplicate_offset_mm` via
    /// undoable, WAL-journaled [`CommandOp::Batch`].
    pub fn duplicate_selection(&mut self) -> Result<Vec<ObjectId>, String> {
        let offset = self
            .active_tab()
            .map(|t| t.duplicate_offset_mm)
            .unwrap_or(Vec2D::new(5.0, 5.0));
        self.duplicate_selection_with_offset(offset)
    }

    /// Duplicates all currently selected objects with an explicit `offset` (used by
    /// both `Ctrl+D` and Transform Docker "Apply to Duplicate" with zero offset).
    pub fn duplicate_selection_with_offset(
        &mut self,
        offset: Vec2D,
    ) -> Result<Vec<ObjectId>, String> {
        self.duplicate_selection_with_affine(Affine2D::translate(offset.x, offset.y))
    }

    /// Duplicates all currently selected objects with an arbitrary `delta_xf` affine
    /// transform (supporting Move, Scale, Rotate, Skew, and FreeTransform stamping).
    pub fn duplicate_selection_with_affine(
        &mut self,
        delta_xf: Affine2D,
    ) -> Result<Vec<ObjectId>, String> {
        let (ops, created) = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            if tab.selection.is_empty() {
                return Ok(Vec::new());
            }
            let ids = tab.selection.objects.clone();
            let mut next_id = tab.document.next_id.max(
                tab.document
                    .objects
                    .keys()
                    .next_back()
                    .map_or(1, |id| id.0.saturating_add(1)),
            );
            let mut id_map = HashMap::new();
            let mut ops = Vec::new();
            let mut created = Vec::new();
            for src_id in ids {
                let layer_id = tab.document.layer_of_object(src_id).or(tab.active_layer_id);
                let mut visiting = HashSet::new();
                if let Some(new_id) = build_duplicate_tree_ops(
                    &tab.document,
                    src_id,
                    layer_id,
                    delta_xf,
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
            (ops, created)
        };
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        self.execute_command(CommandOp::Batch(ops))?;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.selection.select_many(&tab.document, &created);
            tab.metrics_dirty = true;
        }
        self.push_toast(format!("Duplicated {} object(s)", created.len()), false);
        Ok(created)
    }

    /// Duplicates page `src_idx`, cloning its size, bleed, layers, and every object subtree
    /// via a single undoable [`CommandOp::Batch`].
    pub fn duplicate_page(&mut self, src_idx: usize) {
        let plan = self.active_tab().and_then(|tab| {
            let src_page = tab.document.pages.get(src_idx)?;
            let mut next_id = tab.document.next_id.max(1);
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

            // Map each source layer onto a corresponding target layer on the new page
            let mut layer_targets: Vec<(LayerId, Vec<ObjectId>)> = Vec::new();
            for (l_idx, src_layer) in src_page.layers.iter().enumerate() {
                let dst_lid = if l_idx == 0 {
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

            let first_layer_name = src_page.layers.first().map(|l| l.name.clone());
            Some((ops, first_layer_id, first_layer_name))
        });

        if let Some((ops, first_layer_id, first_layer_name)) = plan {
            if let Err(err) = self.execute_command(CommandOp::Batch(ops)) {
                self.push_toast(format!("Duplicate page failed: {err}"), true);
                return;
            }
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if let Some(name) = first_layer_name {
                    if let Some(layer) = tab.document.find_layer_mut(first_layer_id) {
                        layer.name = name;
                    }
                }
                if !tab.document.pages.is_empty() {
                    tab.document.active_page_index = tab.document.pages.len() - 1;
                }
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
                            .objects
                            .get(oid)
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
        let ids = tab.selection.objects.clone();
        let affine = Affine2D::translate(f64::from(dx_steps) * step, f64::from(dy_steps) * step);
        let _ = self.execute_command(CommandOp::TransformObjects { ids, affine });
    }

    /// Micro-nudges selected objects by `(dx_steps, dy_steps) * (nudge_mm / micro_nudge_divisor)` (`Ctrl+Arrow`).
    pub fn micro_nudge_selection(&mut self, dx_steps: i32, dy_steps: i32) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if tab.selection.is_empty() {
            return;
        }
        let divisor = tab.micro_nudge_divisor.max(1.0);
        let step = tab.nudge_mm / divisor;
        let ids = tab.selection.objects.clone();
        let affine = Affine2D::translate(f64::from(dx_steps) * step, f64::from(dy_steps) * step);
        let _ = self.execute_command(CommandOp::TransformObjects { ids, affine });
    }

    /// Copies the selected objects (including full `Group` / `PowerClip` descendant subtrees
    /// and referenced raster assets) into the studio's internal clipboard.
    pub fn copy_selection(&mut self) -> usize {
        let captured = self.active_tab().map(|t| {
            let mut roots = Vec::new();
            let mut copied = Vec::new();
            let mut snap_doc = Document::new_a4("Clipboard");
            let mut seen_roots = HashSet::new();
            for &id in &t.selection.objects {
                if !seen_roots.insert(id) {
                    continue;
                }
                if let Some(obj) = t.document.objects.get(&id) {
                    roots.push(id);
                    copied.push(obj.clone());
                    let mut visiting = HashSet::new();
                    collect_subtree_into_snapshot(&t.document, &mut snap_doc, id, 0, &mut visiting);
                }
            }
            (copied, roots, snap_doc)
        });
        let Some((copied, roots, snap_doc)) = captured else {
            return 0;
        };
        let count = copied.len();
        if count > 0 {
            self.clipboard_objects = copied;
            self.clipboard_snapshot = Some(ClipboardSnapshot {
                roots,
                document: snap_doc,
            });
            self.push_toast(format!("Copied {count} object(s)"), false);
        }
        count
    }

    /// Cuts the selected objects into the internal clipboard and removes them undoably.
    pub fn cut_selection(&mut self) -> usize {
        let count = self.copy_selection();
        if count > 0 {
            self.delete_selection();
        }
        count
    }

    /// Pastes objects (with deep-cloned `Group`/`PowerClip` subtrees and remapped child/link IDs)
    /// from the internal clipboard onto the active unlocked layer via undoable `Batch`.
    pub fn paste_clipboard(&mut self) -> Vec<ObjectId> {
        if self.clipboard_objects.is_empty() && self.clipboard_snapshot.is_none() {
            return Vec::new();
        }
        if let Some(snapshot) = self.clipboard_snapshot.clone() {
            if !snapshot.roots.is_empty() {
                // Ensure any referenced raster assets exist in the destination document
                let plan = self.active_tab_mut().map(|tab| {
                    for (&aid, asset) in &snapshot.document.assets {
                        tab.document
                            .assets
                            .entry(aid)
                            .or_insert_with(|| asset.clone());
                    }
                    let target_layer_id = tab
                        .active_layer_id
                        .filter(|lid| {
                            tab.document
                                .find_layer(*lid)
                                .is_some_and(|l| l.visible && !l.locked)
                        })
                        .or_else(|| {
                            tab.document.active_page().and_then(|p| {
                                p.layers
                                    .iter()
                                    .rev()
                                    .find(|l| l.visible && !l.locked)
                                    .map(|l| l.id)
                            })
                        });
                    let mut next_id = tab.document.next_id.max(
                        tab.document
                            .objects
                            .keys()
                            .next_back()
                            .map_or(1, |id| id.0.saturating_add(1)),
                    );
                    let shift =
                        Affine2D::translate(tab.duplicate_offset_mm.x, tab.duplicate_offset_mm.y);
                    let mut id_map = HashMap::new();
                    let mut ops = Vec::new();
                    let mut created = Vec::new();
                    for &src_id in &snapshot.roots {
                        let mut visiting = HashSet::new();
                        if let Some(new_id) = build_duplicate_tree_ops(
                            &snapshot.document,
                            src_id,
                            target_layer_id,
                            shift,
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
                    (ops, created)
                });
                let Some((ops, created)) = plan else {
                    return Vec::new();
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
        let Some((layer_id, mut next_id, offset)) = self.active_tab().map(|t| {
            (
                t.active_layer_id,
                t.document.next_id.max(
                    t.document
                        .objects
                        .keys()
                        .next_back()
                        .map_or(1, |id| id.0.saturating_add(1)),
                ),
                t.duplicate_offset_mm,
            )
        }) else {
            return Vec::new();
        };
        let mut ops = Vec::with_capacity(self.clipboard_objects.len());
        let mut created = Vec::with_capacity(self.clipboard_objects.len());
        let shift = Affine2D::translate(offset.x, offset.y);
        for src in &self.clipboard_objects {
            let mut obj = src.clone();
            let nid = ObjectId(next_id);
            next_id = next_id.saturating_add(1);
            obj.id = nid;
            obj.transform = shift.compose(&obj.transform);
            created.push(nid);
            ops.push(CommandOp::create_object(obj, layer_id));
        }
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
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if tab.selection.is_empty() {
            return;
        }
        let any_unlocked = tab
            .selection
            .objects
            .iter()
            .any(|id| tab.document.objects.get(id).is_some_and(|o| !o.locked));
        let target_lock = any_unlocked;
        for &id in &tab.selection.objects {
            if let Some(obj) = tab.document.objects.get_mut(&id) {
                obj.locked = target_lock;
            }
        }
        tab.mark_modified();
        self.checkpoint_wal_snapshot_active();
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
        let adapted = match last {
            CommandOp::TransformObjects { affine, .. } if !sel_ids.is_empty() => {
                CommandOp::TransformObjects {
                    ids: sel_ids,
                    affine,
                }
            }
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
            let mut next_id = tab.document.next_id.max(
                tab.document
                    .objects
                    .keys()
                    .next_back()
                    .map_or(1, |k| k.0.saturating_add(1)),
            );
            for &id in &ids {
                if let Some(obj) = tab.document.objects.get(&id) {
                    match &obj.payload {
                        NodePayload::Group(_) => ops.push(CommandOp::Ungroup { group_id: id }),
                        NodePayload::PowerClip { .. } => {
                            ops.push(CommandOp::ExtractPowerClip { powerclip_id: id });
                        }
                        _ if !obj.effects.is_empty() => {
                            let world_xf =
                                tab.document.world_transform_of(id).unwrap_or(obj.transform);
                            let layer_id = tab.document.layer_of_object(id).or(tab.active_layer_id);
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
                            .objects
                            .get(id)
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
                let bb = tab.document.object_bounds(ids[0])?;
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
                Some(Affine2D::translate(dx, dy))
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
            tab.selection
                .objects
                .iter()
                .copied()
                .map(|id| {
                    if let Some(obj) = tab.document.objects.get(&id) {
                        let glyph_path = match &obj.payload {
                            NodePayload::ArtisticText(art) => Some(text_to_curves(art)),
                            NodePayload::ParagraphText(para) => Some(paragraph_to_curves(para)),
                            NodePayload::TextOnPath(top) => {
                                tab.document.world_path_of(top.path_id).map(|wp| {
                                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                                    text_on_path_to_curves(top, &wp.transformed(&inv))
                                })
                            }
                            _ => None,
                        };
                        if let Some(path) = glyph_path {
                            if !path.is_empty() && path.is_finite() {
                                return CommandOp::EditPath { id, path };
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
                    .objects
                    .get(id)
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
                if let Some(obj) = tab.document.objects.get(&id) {
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
        // Record solid color into Document Palette automatically
        if let Fill::Solid(ref col) = fill {
            if let Some(tab) = self.active_tab_mut() {
                if !tab
                    .document
                    .document_palette
                    .swatches
                    .iter()
                    .any(|s| s.color == *col)
                {
                    tab.document
                        .document_palette
                        .swatches
                        .push(ColorSwatch::new(col.to_hex(), col.clone()));
                }
            }
        }
        let _ = self.execute_command(CommandOp::SetFill { ids, fill });
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
                    .objects
                    .get(&id)
                    .map(|o| o.outline.clone())
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
        let _ = self.execute_command(CommandOp::Batch(ops));
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
        let _ = self.execute_command(CommandOp::SetOutline { ids, outline });
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
                if let Some(obj) = tab.document.objects.get(&id) {
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
        let _ = self.execute_command(CommandOp::create_object(obj, tab.active_layer_id));
        self.push_toast(format!("Inserted {rows}x{cols} table"), false);
    }
}
