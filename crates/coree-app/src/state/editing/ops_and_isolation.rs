// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
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

    /// Clears the active object/node selection and notifies `on_selection_change` hooks.
    pub fn deselect_all(&mut self) {
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if let Some(tab) = self.active_tab_mut() {
            tab.selection.clear();
            tab.needs_rerender = true;
        }
        if !before_sel.is_empty() {
            self.maybe_fire_selection_change(&before_sel);
        }
    }

    /// Unlocks currently selected objects, or all locked objects on the active page if none selected.
    pub fn unlock_selection_or_page(&mut self) {
        let ids: Vec<ObjectId> = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return;
            };
            if !tab.selection.is_empty() {
                tab.selection
                    .objects
                    .iter()
                    .copied()
                    .filter(|id| tab.document.object(*id).is_some_and(|o| o.locked))
                    .collect()
            } else {
                tab.document
                    .active_page()
                    .map(|p| {
                        p.layers
                            .iter()
                            .flat_map(|l| l.objects.iter().copied())
                            .filter(|id| tab.document.object(*id).is_some_and(|o| o.locked))
                            .collect()
                    })
                    .unwrap_or_default()
            }
        };
        if ids.is_empty() {
            return;
        }
        if let Err(err) = self.execute_command(CommandOp::SetObjectLocked { ids, locked: false }) {
            self.push_toast(format!("Unlock failed: {err}"), true);
        } else {
            self.push_toast("Unlocked object(s)", false);
        }
    }

    /// Removes all non-destructive effects from currently selected objects.
    pub fn clear_effects_selection(&mut self) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            let mut list = Vec::new();
            for &id in &tab.selection.objects {
                if let Some(obj) = tab.document.object(id) {
                    for idx in (0..obj.effects.len()).rev() {
                        list.push(CommandOp::RemoveEffect {
                            id,
                            effect_index: idx,
                        });
                    }
                }
            }
            list
        };
        if ops.is_empty() {
            return;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if let Err(err) = self.execute_command(cmd) {
            self.push_toast(format!("Clear effects failed: {err}"), true);
        } else {
            self.push_toast("Cleared all effects", false);
        }
    }

    /// Straightens selected `TextOnPath` back to `ArtisticText`.
    pub fn straighten_selected_text(&mut self) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            let mut list = Vec::new();
            for &id in &tab.selection.objects {
                if let Some(obj) = tab.document.object(id) {
                    if let NodePayload::TextOnPath(ref top) = obj.payload {
                        list.push(CommandOp::EditText {
                            id,
                            payload: NodePayload::ArtisticText(top.text.clone()),
                        });
                    }
                }
            }
            list
        };
        if ops.is_empty() {
            return;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        let _ = self.execute_command(cmd);
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
                        NodePayload::Clone { .. } if obj.effects.is_empty() => {
                            if let Some((payload, fill, outline, _linked)) = tab
                                .document
                                .resolve_clone_payload_and_style_with_sym_scope(obj, None)
                            {
                                let world_xf =
                                    tab.document.world_transform_of(id).unwrap_or(obj.transform);
                                let layer_id = enclosing_layer_of_object(&tab.document, id)
                                    .or(tab.active_layer_id);
                                let mut detached =
                                    SceneObject::new(ObjectId(next_id), obj.name.clone(), payload)
                                        .with_transform(world_xf)
                                        .with_fill(fill)
                                        .with_outline(outline);
                                detached.opacity = obj.opacity;
                                detached.blend_mode = obj.blend_mode;
                                detached.visible = obj.visible;
                                detached.locked = obj.locked;
                                detached.printable = obj.printable;
                                next_id = next_id.saturating_add(1);
                                new_selection.push(detached.id);
                                ops.push(CommandOp::DeleteObjects { ids: vec![id] });
                                ops.push(CommandOp::create_object(detached, layer_id));
                            } else {
                                path_ids.push(id);
                            }
                        }
                        _ if !obj.effects.is_empty() => {
                            let world_xf =
                                tab.document.world_transform_of(id).unwrap_or(obj.transform);
                            let layer_id = enclosing_layer_of_object(&tab.document, id)
                                .or(tab.active_layer_id);
                            let resolved_storage;
                            let target_obj = if matches!(obj.payload, NodePayload::Clone { .. }) {
                                if let Some((payload, fill, outline, _linked)) = tab
                                    .document
                                    .resolve_clone_payload_and_style_with_sym_scope(obj, None)
                                {
                                    let mut resolved =
                                        SceneObject::new(obj.id, obj.name.clone(), payload)
                                            .with_transform(obj.transform)
                                            .with_fill(fill)
                                            .with_outline(outline);
                                    resolved.effects = obj.effects.clone();
                                    resolved.opacity = obj.opacity;
                                    resolved.blend_mode = obj.blend_mode;
                                    resolved.perspective = obj.perspective;
                                    resolved_storage = resolved;
                                    &resolved_storage
                                } else {
                                    obj
                                }
                            } else {
                                obj
                            };
                            let lowered = lower_effects_to_primitives(
                                &tab.document,
                                target_obj,
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
                        let (resolved_tmp, eff_fill) =
                            if matches!(obj.payload, NodePayload::Clone { .. }) {
                                if let Some((eff_payload, eff_fill, _)) =
                                    tab.document.resolve_clone_payload_and_style(obj)
                                {
                                    let mut tmp = obj.clone();
                                    tmp.payload = eff_payload;
                                    (Some(tmp), Some(eff_fill))
                                } else {
                                    (None, None)
                                }
                            } else {
                                (None, None)
                            };
                        let target_obj = resolved_tmp.as_ref().unwrap_or(obj);
                        if let Some(path) = outline_text_object_with_engine(
                            &mut text_engine,
                            target_obj,
                            &tab.document,
                        ) {
                            if !path.is_empty() && path.is_finite() {
                                let mut batch = Vec::new();
                                let has_visible_fill = obj.fill.is_visible()
                                    || eff_fill.as_ref().is_some_and(Fill::is_visible);
                                if !has_visible_fill {
                                    let run_fill = match &target_obj.payload {
                                        NodePayload::ArtisticText(t) => t
                                            .runs
                                            .iter()
                                            .find(|r| r.style.fill.alpha() > 1e-6)
                                            .map(|r| r.style.fill.clone()),
                                        NodePayload::ParagraphText(p) => p
                                            .runs
                                            .iter()
                                            .find(|r| r.style.fill.alpha() > 1e-6)
                                            .map(|r| r.style.fill.clone()),
                                        NodePayload::TextOnPath(top) => top
                                            .text
                                            .runs
                                            .iter()
                                            .find(|r| r.style.fill.alpha() > 1e-6)
                                            .map(|r| r.style.fill.clone()),
                                        _ => None,
                                    };
                                    if let Some(c) = run_fill {
                                        batch.push(CommandOp::SetFill {
                                            ids: vec![id],
                                            fill: Fill::Solid(c),
                                        });
                                    }
                                }
                                batch.push(CommandOp::ConvertToCurves { id });
                                batch.push(CommandOp::EditPath { id, path });
                                return CommandOp::Batch(batch);
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

    /// Toggles `clip_even_odd` on the first selected `PowerClip` container cleanly via undoable commands.
    pub fn toggle_powerclip_even_odd(&mut self) {
        let plan = self.active_tab().and_then(|tab| {
            let oid = tab.selection.objects.iter().copied().find(|id| {
                tab.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::PowerClip { .. }))
            })?;
            let obj = tab.document.object(oid)?;
            let layer_id = enclosing_layer_of_object(&tab.document, oid)?;
            let layer = tab.document.find_layer(layer_id)?;
            let mut updated = obj.clone();
            updated.clip_even_odd = !updated.clip_even_odd;
            let next_even_odd = updated.clip_even_odd;
            let new_id = ObjectId(
                tab.document.next_id().max(
                    tab.document
                        .objects()
                        .keys()
                        .next_back()
                        .map_or(1, |k| k.0 + 1),
                ),
            );
            updated.id = new_id;
            let layer_order: Vec<ObjectId> = layer
                .objects
                .iter()
                .map(|&id| if id == oid { new_id } else { id })
                .collect();
            Some((oid, new_id, layer_id, updated, layer_order, next_even_odd))
        });
        let Some((old_id, new_id, layer_id, updated, layer_order, next_even_odd)) = plan else {
            return;
        };
        let ops = vec![
            CommandOp::DeleteObjects { ids: vec![old_id] },
            CommandOp::create_object(updated, Some(layer_id)),
            CommandOp::ReorderLayerObjects {
                layer_id,
                object_ids: layer_order,
            },
        ];
        if self.execute_command(CommandOp::Batch(ops)).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.select_single(&tab.document, new_id);
                tab.needs_rerender = true;
            }
            self.push_toast(
                if next_even_odd {
                    "PowerClip clip rule: Even-Odd"
                } else {
                    "PowerClip clip rule: Non-Zero Winding"
                },
                false,
            );
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
    /// When `frozen` is enabled without a pre-baked snapshot, captures and persists the PNG
    /// `frozen_snapshot` into the document immediately (`REM-04`).
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
        let needs_snap = lens
            .as_ref()
            .is_some_and(|l| l.frozen && l.frozen_snapshot.is_none());
        match self.execute_command(CommandOp::SetLens {
            ids: ids.clone(),
            lens: lens.clone(),
        }) {
            Ok(()) => {
                if needs_snap {
                    if let Some(mut cfg) = lens {
                        let snaps: Vec<(ObjectId, Option<Vec<u8>>)> = self
                            .active_tab()
                            .map(|tab| {
                                let dpi = f64::from(tab.document.dpi()).clamp(72.0, 300.0);
                                ids.iter()
                                    .map(|&id| {
                                        (
                                            id,
                                            coree_render::capture_frozen_lens_snapshot(
                                                &tab.document,
                                                id,
                                                dpi,
                                            ),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let ops: Vec<CommandOp> = snaps
                            .into_iter()
                            .filter_map(|(id, snap)| {
                                snap.map(|bytes| {
                                    let mut c = cfg.clone();
                                    c.frozen_snapshot = Some(bytes);
                                    CommandOp::SetLens {
                                        ids: vec![id],
                                        lens: Some(c),
                                    }
                                })
                            })
                            .collect();
                        if !ops.is_empty() {
                            cfg.frozen_snapshot = None;
                            let _ = self.execute_command(CommandOp::Batch(ops));
                        }
                    }
                }
                self.push_toast(format!("Applied Lens: {name}"), false);
            }
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

    /// Enters isolated symbol-editing mode for `sym_id`, opening the symbol definition
    /// inside an isolated working canvas until [`Self::finish_edit_symbol`] is called.
    pub fn begin_edit_symbol(&mut self, sym_id: SymbolId) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        if tab.editing_symbol.is_some() {
            return false;
        }
        let Some(sym) = tab.document.symbol(sym_id).cloned() else {
            return false;
        };
        tab.cancel_active_drag();
        let _ = tab.tool_core.cancel_all();
        tab.powerclip_edit_id = None;

        let backup_document = tab.document.clone();
        let backup_history = std::mem::take(&mut tab.history);
        let backup_selection = tab.selection.objects.clone();
        let backup_active_layer_id = tab.active_layer_id;

        let mut sym_doc = Document::new_a4(format!("Symbol: {}", sym.name));
        for asset in backup_document.assets().values() {
            sym_doc.insert_asset_if_absent(asset.clone());
        }
        for cs in backup_document.color_styles() {
            sym_doc.upsert_color_style(cs.clone());
        }
        for os in backup_document.object_styles() {
            sym_doc.upsert_object_style(os.clone());
        }
        for (sid, sdef) in backup_document.symbols() {
            if *sid != sym_id {
                sym_doc.upsert_symbol(sdef.clone());
            }
        }
        let mut referenced_children: HashSet<ObjectId> = HashSet::new();
        for sub in &sym.objects {
            match &sub.payload {
                NodePayload::Group(children) => {
                    referenced_children.extend(children.iter().copied());
                }
                NodePayload::PowerClip { contents, .. } => {
                    referenced_children.extend(contents.iter().copied());
                }
                _ => {}
            }
        }
        let mut old_to_new: HashMap<ObjectId, ObjectId> = HashMap::new();
        let mut root_id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
        let mut staged: Vec<(bool, SceneObject)> = Vec::with_capacity(sym.objects.len());
        for mut sub in sym.objects {
            let orig_id = sub.id;
            let new_id = sym_doc.alloc_object_id();
            if orig_id.0 != 0 {
                old_to_new.insert(orig_id, new_id);
            }
            root_id_map.insert(new_id, orig_id);
            let is_top = orig_id.0 == 0 || !referenced_children.contains(&orig_id);
            sub.id = new_id;
            staged.push((is_top, sub));
        }
        let mut inserted_roots = Vec::new();
        for (is_top, mut sub) in staged {
            Document::remap_object_refs_with_map(&mut sub, &old_to_new);
            let new_id = sub.id;
            if is_top {
                if sym_doc.add_object(sub, None).is_ok() {
                    inserted_roots.push(new_id);
                }
            } else {
                let _ = sym_doc.insert_loaded_object(sub);
            }
        }
        let new_layer = sym_doc
            .pages()
            .first()
            .and_then(|p| p.layers.first())
            .map(|l| l.id);

        tab.editing_symbol = Some(SymbolEditSession {
            symbol_id: sym_id,
            symbol_name: sym.name.clone(),
            backup_document,
            backup_history,
            backup_selection,
            backup_active_layer_id,
            root_id_map,
        });
        tab.document = sym_doc;
        tab.history = CommandHistory::default();
        tab.active_layer_id = new_layer;
        tab.selection.select_many(&tab.document, &inserted_roots);
        tab.tile_cache.mark_all_dirty();
        tab.metrics_dirty = true;
        tab.needs_rerender = true;
        self.push_toast(
            format!(
                "Editing Symbol '{}' — click Finish Editing to apply",
                sym.name
            ),
            false,
        );
        true
    }

    /// Exits isolated symbol-editing mode; when `commit` is `true`, updates the symbol
    /// definition via [`CommandOp::UpsertSymbol`] with full Undo/Redo support.
    pub fn finish_edit_symbol(&mut self, commit: bool) -> bool {
        let Some(session) = self.active_tab_mut().and_then(|t| t.editing_symbol.take()) else {
            return false;
        };
        let edited_doc = {
            let tab = self.active_tab_mut().expect("active tab exists");
            tab.cancel_active_drag();
            let _ = tab.tool_core.cancel_all();
            let edited_doc = std::mem::replace(&mut tab.document, session.backup_document);
            tab.history = session.backup_history;
            tab.active_layer_id = session.backup_active_layer_id;
            tab.selection
                .select_many(&tab.document, &session.backup_selection);
            tab.tile_cache.mark_all_dirty();
            tab.metrics_dirty = true;
            tab.needs_rerender = true;
            edited_doc
        };

        if commit {
            let mut root_ids: Vec<ObjectId> = Vec::new();
            for page in edited_doc.pages() {
                for layer in &page.layers {
                    root_ids.extend(layer.objects.iter().copied());
                }
            }
            let mut objects = edited_doc.collect_subtree_objects(&root_ids);
            let mut used_ids: HashSet<ObjectId> = HashSet::new();
            let mut final_id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
            for obj in &objects {
                if let Some(&orig) = session.root_id_map.get(&obj.id) {
                    if orig.0 != 0 && used_ids.insert(orig) {
                        final_id_map.insert(obj.id, orig);
                    }
                }
            }
            let mut next_free = used_ids
                .iter()
                .chain(objects.iter().map(|o| &o.id))
                .map(|id| id.0)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            for obj in &objects {
                if final_id_map.contains_key(&obj.id) {
                    continue;
                }
                if obj.id.0 != 0 && used_ids.insert(obj.id) {
                    final_id_map.insert(obj.id, obj.id);
                } else {
                    while !used_ids.insert(ObjectId(next_free)) {
                        next_free = next_free.saturating_add(1);
                    }
                    final_id_map.insert(obj.id, ObjectId(next_free));
                    next_free = next_free.saturating_add(1);
                }
            }
            for obj in &mut objects {
                if let Some(&mapped) = final_id_map.get(&obj.id) {
                    obj.id = mapped;
                }
                Document::remap_object_refs_with_map(obj, &final_id_map);
            }
            let updated_sym = coree_core::SymbolDef {
                id: session.symbol_id,
                name: session.symbol_name.clone(),
                objects,
            };
            match self.execute_command(CommandOp::UpsertSymbol {
                symbol: updated_sym,
            }) {
                Ok(()) => {
                    self.push_toast(
                        format!("Saved edits to Symbol '{}'", session.symbol_name),
                        false,
                    );
                }
                Err(e) => {
                    self.push_toast(e, true);
                    return false;
                }
            }
        } else {
            self.push_toast("Cancelled Symbol editing", false);
        }
        true
    }

    /// Fits, centers, or stretches contents inside all selected PowerClip containers.
    pub fn fit_powerclip_selection(&mut self, mode: coree_core::PowerClipFitMode) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            tab.selection
                .objects
                .iter()
                .copied()
                .filter(|id| {
                    tab.document
                        .object(*id)
                        .is_some_and(|o| matches!(o.payload, NodePayload::PowerClip { .. }))
                })
                .map(|id| CommandOp::FitPowerClip { id, mode })
                .collect()
        };
        if ops.is_empty() {
            self.push_toast("Select a PowerClip container first", true);
            return;
        }
        match self.execute_command(CommandOp::Batch(ops)) {
            Ok(()) => self.push_toast(format!("PowerClip {mode:?} applied"), false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Creates live linked [`NodePayload::Clone`] objects from the currently selected
    /// objects on the active unlocked layer via undoable [`CommandOp`].
    pub fn clone_selection(&mut self) -> Vec<ObjectId> {
        let Some(layer_id) = self.ensure_active_creation_layer() else {
            return Vec::new();
        };
        let (ops, created) = {
            let Some(tab) = self.active_tab() else {
                return Vec::new();
            };
            if tab.selection.is_empty() {
                return Vec::new();
            }
            let ids = tab
                .document
                .filter_topmost_selection(&tab.selection.objects);
            let offset = tab.duplicate_offset_mm;
            let mut next_id = tab.document.next_id().max(
                tab.document
                    .objects()
                    .keys()
                    .next_back()
                    .map_or(1, |id| id.0.saturating_add(1)),
            );
            let mut ops = Vec::new();
            let mut created = Vec::new();
            for src_id in ids {
                let Some(src_obj) = tab.document.object(src_id) else {
                    continue;
                };
                let parent_world = tab
                    .document
                    .parent_container_of(src_id)
                    .and_then(|pid| tab.document.world_transform_of(pid))
                    .unwrap_or(Affine2D::IDENTITY);
                let master_world_xf = parent_world.compose(&src_obj.transform);
                let new_id = ObjectId(next_id);
                next_id = next_id.saturating_add(1);
                let mut clone_obj = SceneObject::new(
                    new_id,
                    format!("Clone of {}", src_obj.name),
                    NodePayload::Clone {
                        master_id: src_id,
                        link_fill: true,
                        link_outline: true,
                        link_shape: true,
                    },
                );
                clone_obj.transform =
                    Affine2D::translate(offset.x, offset.y).compose(&master_world_xf);
                clone_obj.fill = src_obj.fill.clone();
                clone_obj.outline = src_obj.outline.clone();
                clone_obj.opacity = src_obj.opacity;
                clone_obj.blend_mode = src_obj.blend_mode;
                clone_obj.perspective = src_obj.perspective;
                ops.push(CommandOp::create_object(clone_obj, layer_id));
                created.push(new_id);
            }
            (ops, created)
        };
        if ops.is_empty() {
            return Vec::new();
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().expect("single op")
        } else {
            CommandOp::Batch(ops)
        };
        match self.execute_command(cmd) {
            Ok(()) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.select_many(&tab.document, &created);
                    tab.metrics_dirty = true;
                }
                self.push_toast(format!("Cloned {} object(s)", created.len()), false);
                created
            }
            Err(e) => {
                self.push_toast(e, true);
                Vec::new()
            }
        }
    }

    /// Updates width/profile on all selected [`NodePayload::ArtisticMedia`] objects via
    /// undoable [`CommandOp::EditArtisticMedia`].
    pub fn edit_selected_artistic_media(
        &mut self,
        width_mm: Option<f64>,
        profile: Option<coree_core::ArtisticWidthProfile>,
    ) -> bool {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return false;
            };
            let mut out = Vec::new();
            for &id in &tab.selection.objects {
                let Some(obj) = tab.document.object(id) else {
                    continue;
                };
                if let NodePayload::ArtisticMedia { spine, style } = &obj.payload {
                    let mut next_style = style.clone();
                    match &mut next_style {
                        coree_core::ArtisticMediaStyle::Preset {
                            max_width_mm,
                            profile: prof,
                        } => {
                            if let Some(w) = width_mm {
                                *max_width_mm = w.max(0.1);
                            }
                            if let Some(ref p) = profile {
                                *prof = p.clone();
                            }
                        }
                        coree_core::ArtisticMediaStyle::Brush { width_mm: bw, .. }
                        | coree_core::ArtisticMediaStyle::Calligraphic { width_mm: bw, .. } => {
                            if let Some(w) = width_mm {
                                *bw = w.max(0.1);
                            }
                        }
                        coree_core::ArtisticMediaStyle::Sprayer { size_mm, .. } => {
                            if let Some(w) = width_mm {
                                *size_mm = w.max(0.1);
                            }
                        }
                        coree_core::ArtisticMediaStyle::Expression { max_width_mm, .. } => {
                            if let Some(w) = width_mm {
                                *max_width_mm = w.max(0.1);
                            }
                        }
                    }
                    if next_style != *style {
                        out.push(CommandOp::EditArtisticMedia {
                            id,
                            spine: spine.clone(),
                            style: next_style,
                        });
                    }
                }
            }
            out
        };
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().expect("single op")
        } else {
            CommandOp::Batch(ops)
        };
        match self.execute_command(cmd) {
            Ok(()) => true,
            Err(e) => {
                self.push_toast(e, true);
                false
            }
        }
    }

    /// Unlinks selected live Clone objects into independent editable shapes/curves.
    pub fn unlink_clone_selection(&mut self) {
        let ids: Vec<ObjectId> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            tab.selection
                .objects
                .iter()
                .copied()
                .filter(|id| {
                    tab.document
                        .object(*id)
                        .is_some_and(|o| matches!(o.payload, NodePayload::Clone { .. }))
                })
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        match self.execute_command(CommandOp::UnlinkClone { ids }) {
            Ok(()) => self.push_toast("Unlinked Clone object(s)", false),
            Err(e) => self.push_toast(e, true),
        }
    }
}
