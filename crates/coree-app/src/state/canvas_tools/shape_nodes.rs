// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Marquee-selects all Bézier nodes of selected paths (or selects objects) inside `rect`.
    pub fn select_nodes_in_rect(&mut self, rect: Rect2D, additive: bool) -> usize {
        let Some(tab) = self.active_tab_mut() else {
            return 0;
        };
        if tab.selection.is_empty() {
            let hits = coree_render::hit_test_rect(
                &tab.document,
                tab.document.active_page_index(),
                rect,
                false,
            );
            tab.selection.select_many(&tab.document, &hits);
            return hits.len();
        }
        if !additive {
            tab.selection.node_selection.clear();
        }
        let mut count = 0usize;
        for &oid in &tab.selection.objects {
            let Some(obj) = tab.document.object(oid) else {
                continue;
            };
            let NodePayload::Path(ref path) = obj.payload else {
                continue;
            };
            for (sp_idx, sp) in path.subpaths.iter().enumerate() {
                for (n_idx, node) in sp.nodes.iter().enumerate() {
                    let wp = obj.transform.transform_point(node.pos);
                    if rect.contains_point(wp) {
                        let nref = NodeRef {
                            object_id: oid,
                            subpath_index: sp_idx,
                            node_index: n_idx,
                        };
                        if !tab.selection.node_selection.contains(nref) {
                            tab.selection.node_selection.nodes.push(nref);
                        }
                        count += 1;
                    }
                }
            }
        }
        count
    }

    /// Commits a live `ParametricShape` handle edit cleanly through `CommandHistory` + WAL
    /// while preserving the object's exact `ObjectId`, Z-order, and parent container (`Layer`,
    /// `Group`, or `PowerClip`) without ever duplicating nested objects onto top-level layers.
    pub fn commit_parametric_shape_edit(
        &mut self,
        object_id: ObjectId,
        orig_shape: ParametricShape,
        new_shape: ParametricShape,
    ) -> bool {
        if orig_shape == new_shape {
            return false;
        }
        let ready = self.active_tab().is_some_and(|tab| {
            is_object_editable_in_doc(&tab.document, object_id)
                && (tab.document.layer_of_object(object_id).is_some()
                    || tab.document.parent_container_of(object_id).is_some())
                && tab
                    .document
                    .object(object_id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Shape(_)))
        });
        if !ready {
            return false;
        }
        let committed = self
            .execute_command(CommandOp::EditShape {
                id: object_id,
                shape: new_shape,
            })
            .is_ok();
        if committed {
            self.checkpoint_wal_snapshot_active();
        }
        committed
    }

    fn build_edit_path_op(doc: &Document, id: ObjectId, path: BezierPath) -> CommandOp {
        if doc
            .object(id)
            .is_some_and(|o| matches!(o.payload, NodePayload::Path(_)))
        {
            CommandOp::EditPath { id, path }
        } else {
            CommandOp::Batch(vec![
                CommandOp::ConvertToCurves { id },
                CommandOp::EditPath { id, path },
            ])
        }
    }

    /// Helper: collects editable `BezierPath`s for selected objects (converting live `ParametricShape`s if needed).
    fn selected_editable_paths(&self) -> Vec<(ObjectId, BezierPath)> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        tab.selection
            .objects
            .iter()
            .copied()
            .filter_map(|oid| {
                if !is_object_editable_in_doc(&tab.document, oid) {
                    return None;
                }
                let obj = tab.document.object(oid)?;
                match &obj.payload {
                    NodePayload::Path(p) => Some((oid, p.clone())),
                    NodePayload::Shape(s) => Some((oid, s.to_bezier_path())),
                    _ => None,
                }
            })
            .collect()
    }

    /// Shape Property Bar: inserts a new node on each selected segment (or segment 0 of selected path).
    pub fn shape_add_node(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        let mut newly_selected = Vec::new();

        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, mut refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    // Sort descending so inserting nodes doesn't invalidate earlier indices
                    refs.sort_unstable();
                    refs.dedup();
                    refs.reverse();
                    let mut changed = false;
                    for (sp_idx, n_idx) in refs {
                        let seg_count = path.subpaths.get(sp_idx).map_or(0, Subpath::segment_count);
                        if seg_count == 0 {
                            continue;
                        }
                        let seg_idx = n_idx.min(seg_count - 1);
                        if let Ok(ins) = path.add_node_at(sp_idx, seg_idx, 0.5) {
                            changed = true;
                            newly_selected.push(NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: ins,
                            });
                        }
                    }
                    if changed {
                        ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                    }
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    if path
                        .subpaths
                        .first()
                        .is_some_and(|sp| sp.segment_count() > 0)
                    {
                        if let Ok(ins) = path.add_node_at(0, 0, 0.5) {
                            newly_selected.push(NodeRef {
                                object_id: oid,
                                subpath_index: 0,
                                node_index: ins,
                            });
                            ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                        }
                    }
                }
            }
        }

        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.nodes = newly_selected;
            }
            self.push_toast("Added Bézier node", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: deletes all currently selected Bézier nodes cleanly.
    pub fn shape_delete_selected_nodes(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        if selected_nodes.is_empty() {
            return false;
        }
        let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
        for nr in selected_nodes {
            by_obj
                .entry(nr.object_id)
                .or_default()
                .push((nr.subpath_index, nr.node_index));
        }
        let mut ops = Vec::new();
        if let Some(tab) = self.active_tab() {
            for (oid, mut refs) in by_obj {
                let Some(obj) = tab.document.object(oid) else {
                    continue;
                };
                if obj.locked {
                    continue;
                }
                let NodePayload::Path(ref orig) = obj.payload else {
                    continue;
                };
                let mut path = orig.clone();
                refs.sort_unstable();
                refs.dedup();
                refs.reverse();
                let mut changed = false;
                for (sp_idx, n_idx) in refs {
                    if path.delete_node(sp_idx, n_idx).is_ok() {
                        changed = true;
                    }
                }
                if changed {
                    ops.push(CommandOp::EditPath { id: oid, path });
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.clear();
            }
            self.push_toast("Deleted selected node(s)", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: joins two selected endpoint nodes (or closes an open subpath).
    pub fn shape_join_nodes(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();

        // 1. Two nodes on the same path selected -> call BezierPath::join_nodes directly
        if selected_nodes.len() >= 2 && selected_nodes[0].object_id == selected_nodes[1].object_id {
            let oid = selected_nodes[0].object_id;
            let edited = self.active_tab().and_then(|tab| {
                let obj = tab.document.object(oid)?;
                if obj.locked {
                    return None;
                }
                let NodePayload::Path(ref p) = obj.payload else {
                    return None;
                };
                let mut path = p.clone();
                path.join_nodes(
                    selected_nodes[0].subpath_index,
                    selected_nodes[0].node_index,
                    selected_nodes[1].subpath_index,
                    selected_nodes[1].node_index,
                )
                .ok()?;
                Some(path)
            });
            if let Some(path) = edited {
                if self
                    .execute_command(CommandOp::EditPath { id: oid, path })
                    .is_ok()
                {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.selection.node_selection.clear();
                    }
                    self.push_toast("Joined nodes", false);
                    return true;
                }
            }
        }

        // 2. Fallback: join endpoints of any open subpath on the primary selected path
        let fallback_op = self.active_tab().and_then(|tab| {
            for (oid, mut path) in self.selected_editable_paths() {
                let mut joined = false;
                for sp_idx in 0..path.subpaths.len() {
                    if !path.subpaths[sp_idx].closed && path.subpaths[sp_idx].nodes.len() >= 2 {
                        let last = path.subpaths[sp_idx].nodes.len() - 1;
                        if path.join_nodes(sp_idx, 0, sp_idx, last).is_ok() {
                            joined = true;
                        }
                    }
                }
                if joined {
                    return Some(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
            None
        });
        if let Some(cmd) = fallback_op {
            if self.execute_command(cmd).is_ok() {
                self.push_toast("Joined open curve endpoints", false);
                return true;
            }
        }
        false
    }

    /// Shape Property Bar: breaks curve open at selected node(s) (or node 0 of selected path).
    pub fn shape_break_curve(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, mut refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    refs.sort_unstable();
                    refs.dedup();
                    refs.reverse();
                    let mut changed = false;
                    for (sp_idx, n_idx) in refs {
                        if path.break_at_node(sp_idx, n_idx).is_ok() {
                            changed = true;
                        }
                    }
                    if changed {
                        ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                    }
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    if !path.subpaths.is_empty() && path.break_at_node(0, 0).is_ok() {
                        ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                    }
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.clear();
            }
            self.push_toast("Broke curve at node", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: converts selected segment(s) (or all segments of selected path) to straight lines.
    pub fn shape_convert_to_line(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    for (sp_idx, n_idx) in refs {
                        let sc = path.subpaths.get(sp_idx).map_or(0, Subpath::segment_count);
                        if sc > 0 {
                            let _ = path.convert_segment_to_line(sp_idx, n_idx.min(sc - 1));
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    for sp_idx in 0..path.subpaths.len() {
                        let sc = path.subpaths[sp_idx].segment_count();
                        for seg_idx in 0..sc {
                            let _ = path.convert_segment_to_line(sp_idx, seg_idx);
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Converted segment(s) to line", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: converts selected segment(s) (or all segments of selected path) to cubic curves.
    pub fn shape_convert_to_curve(&mut self) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    for (sp_idx, n_idx) in refs {
                        let sc = path.subpaths.get(sp_idx).map_or(0, Subpath::segment_count);
                        if sc > 0 {
                            let _ = path.convert_segment_to_curve(sp_idx, n_idx.min(sc - 1));
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    for sp_idx in 0..path.subpaths.len() {
                        let sc = path.subpaths[sp_idx].segment_count();
                        for seg_idx in 0..sc {
                            let _ = path.convert_segment_to_curve(sp_idx, seg_idx);
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Converted segment(s) to curve", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: sets continuity (`Cusp`, `Smooth`, `Symmetrical`) on selected nodes.
    pub fn shape_set_node_smoothness(&mut self, kind: NodeSmoothness) -> bool {
        let selected_nodes = self
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let mut ops = Vec::new();
        if !selected_nodes.is_empty() {
            let mut by_obj: BTreeMap<ObjectId, Vec<(usize, usize)>> = BTreeMap::new();
            for nr in selected_nodes {
                by_obj
                    .entry(nr.object_id)
                    .or_default()
                    .push((nr.subpath_index, nr.node_index));
            }
            if let Some(tab) = self.active_tab() {
                for (oid, refs) in by_obj {
                    let Some(obj) = tab.document.object(oid) else {
                        continue;
                    };
                    if obj.locked {
                        continue;
                    }
                    let mut path = match &obj.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        _ => continue,
                    };
                    for (sp_idx, n_idx) in refs {
                        if kind != NodeKind::Cusp {
                            if let Some(sp) = path.subpaths.get_mut(sp_idx) {
                                let n = sp.nodes.len();
                                if n_idx < n && n >= 2 {
                                    let needs_h = sp.nodes[n_idx].handle_in.is_none()
                                        && sp.nodes[n_idx].handle_out.is_none();
                                    if needs_h {
                                        let prev = sp.nodes[(n_idx + n - 1) % n].pos;
                                        let next = sp.nodes[(n_idx + 1) % n].pos;
                                        let tang = (next - prev) * 0.2;
                                        if tang.length() > 1e-6 {
                                            let pos = sp.nodes[n_idx].pos;
                                            sp.nodes[n_idx].handle_in = Some(pos - tang);
                                            sp.nodes[n_idx].handle_out = Some(pos + tang);
                                        }
                                    }
                                }
                            }
                        }
                        let _ = path.set_node_kind(sp_idx, n_idx, kind);
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        } else {
            let paths = self.selected_editable_paths();
            if let Some(tab) = self.active_tab() {
                for (oid, mut path) in paths {
                    for sp_idx in 0..path.subpaths.len() {
                        let len = path.subpaths[sp_idx].nodes.len();
                        for n_idx in 0..len {
                            let _ = path.set_node_kind(sp_idx, n_idx, kind);
                        }
                    }
                    ops.push(Self::build_edit_path_op(&tab.document, oid, path));
                }
            }
        }
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast(format!("Set node smoothness: {kind:?}"), false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: reverses traversal direction of all subpaths on selected path(s).
    pub fn shape_reverse_subpaths(&mut self) -> bool {
        let paths = self.selected_editable_paths();
        let ops: Vec<CommandOp> = if let Some(tab) = self.active_tab() {
            paths
                .into_iter()
                .map(|(oid, mut path)| {
                    for sp_idx in 0..path.subpaths.len() {
                        let _ = path.reverse_subpath(sp_idx);
                    }
                    Self::build_edit_path_op(&tab.document, oid, path)
                })
                .collect()
        } else {
            Vec::new()
        };
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Reversed subpath direction", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: closes all open subpaths on selected path(s).
    pub fn shape_close_curve(&mut self) -> bool {
        let paths = self.selected_editable_paths();
        let ops: Vec<CommandOp> = if let Some(tab) = self.active_tab() {
            paths
                .into_iter()
                .map(|(oid, mut path)| {
                    for sp_idx in 0..path.subpaths.len() {
                        let _ = path.auto_close(sp_idx);
                    }
                    Self::build_edit_path_op(&tab.document, oid, path)
                })
                .collect()
        } else {
            Vec::new()
        };
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast("Closed curve", false);
            true
        } else {
            false
        }
    }

    /// Shape Property Bar: selects all Bézier nodes across currently selected path objects.
    pub fn shape_select_all_nodes(&mut self) -> usize {
        let Some(tab) = self.active_tab_mut() else {
            return 0;
        };
        tab.selection.node_selection.clear();
        for &oid in &tab.selection.objects {
            let Some(obj) = tab.document.object(oid) else {
                continue;
            };
            let NodePayload::Path(ref path) = obj.payload else {
                continue;
            };
            for (sp_idx, sp) in path.subpaths.iter().enumerate() {
                for n_idx in 0..sp.nodes.len() {
                    tab.selection.node_selection.nodes.push(NodeRef {
                        object_id: oid,
                        subpath_index: sp_idx,
                        node_index: n_idx,
                    });
                }
            }
        }
        tab.selection.node_selection.nodes.len()
    }

    /// Shape Property Bar: simplifies / reduces redundant nodes within `tolerance_mm`.
    pub fn shape_reduce_nodes(&mut self, tolerance_mm: f64) -> bool {
        let tol = if tolerance_mm.is_finite() {
            tolerance_mm.clamp(0.01, 50.0)
        } else {
            0.5
        };
        let paths = self.selected_editable_paths();
        let ops: Vec<CommandOp> = if let Some(tab) = self.active_tab() {
            paths
                .into_iter()
                .map(|(oid, mut path)| {
                    path.reduce_nodes(tol);
                    Self::build_edit_path_op(&tab.document, oid, path)
                })
                .collect()
        } else {
            Vec::new()
        };
        if ops.is_empty() {
            return false;
        }
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().unwrap()
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.node_selection.clear();
            }
            self.push_toast(format!("Reduced nodes (tol {tol:.2} mm)"), false);
            true
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // 5 Vector Sculpting Tools (Smooth, Smear, Twirl, AttractRepel, Roughen)
    // -------------------------------------------------------------------------

    /// Applies any of the 5 vector sculpting tools (`Smooth`, `Smear`, `Twirl`, `AttractRepel`,
    /// `Roughen`) along `points` in world mm, auto-converting `ParametricShape` to `BezierPath`
    /// and committing undoably via `CommandOp::EditPath`.
    pub fn apply_sculpt_stroke(
        &mut self,
        tool: ToolId,
        points: &[Point2D],
        shift_invert: bool,
    ) -> bool {
        if points.is_empty() {
            return false;
        }
        let radius_mm = self.tool_settings.sculpt_radius_mm.clamp(1.0, 200.0);
        let strength = self.tool_settings.sculpt_strength.clamp(0.05, 1.0);

        // Locate target vector object: selected first, else hovered within brush radius
        let target_id = self.active_tab().and_then(|tab| {
            let sel = tab.selection.objects.iter().copied().find(|&id| {
                is_object_editable_in_doc(&tab.document, id)
                    && tab.document.object(id).is_some_and(|o| {
                        matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                    })
            });
            if sel.is_some() {
                return sel;
            }
            for &pt in points {
                if let Some(hit) = hit_test_point(
                    &tab.document,
                    tab.document.active_page_index(),
                    pt,
                    radius_mm,
                ) {
                    if is_object_editable_in_doc(&tab.document, hit)
                        && tab.document.object(hit).is_some_and(|o| {
                            matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                        })
                    {
                        return Some(hit);
                    }
                }
            }
            None
        });
        let Some(oid) = target_id else {
            return false;
        };

        let edited_op = self.active_tab().and_then(|tab| {
            if !is_object_editable_in_doc(&tab.document, oid) {
                return None;
            }
            let obj = tab.document.object(oid)?;
            let mut path = match &obj.payload {
                NodePayload::Path(p) => p.clone(),
                NodePayload::Shape(s) => s.to_bezier_path(),
                _ => return None,
            };
            let inv = world_inverse_transform_of(&tab.document, oid);
            let scale_est = ((inv.m[0].hypot(inv.m[1]) + inv.m[2].hypot(inv.m[3])) * 0.5).max(1e-3);
            let local_radius = radius_mm * scale_est;

            for (idx, &pt) in points.iter().enumerate() {
                let lp = inv.transform_point(pt);
                let prev_lp = if idx > 0 {
                    inv.transform_point(points[idx - 1])
                } else {
                    lp
                };
                match tool {
                    ToolId::Smooth => {
                        path.smooth_near(lp, local_radius, strength);
                    }
                    ToolId::Smear => {
                        let delta = if idx > 0 {
                            lp - prev_lp
                        } else if points.len() >= 2 {
                            (inv.transform_point(points[1]) - lp) * 0.5
                        } else {
                            Vec2D::new(local_radius * 0.25, 0.0)
                        };
                        path.smear_near(lp, delta, local_radius);
                    }
                    ToolId::Twirl => {
                        let sign = if shift_invert { -1.0 } else { 1.0 };
                        let angle = sign * (0.45 * strength + 0.15);
                        path.twirl_near(lp, local_radius, angle);
                    }
                    ToolId::AttractRepel => {
                        let attract = self.tool_settings.sculpt_sub_mode.is_attract(shift_invert);
                        path.attract_repel_near(lp, local_radius, strength, attract);
                    }
                    ToolId::Roughen => {
                        let amp = (local_radius * 0.18 * strength).max(0.4);
                        path.roughen_near(
                            lp,
                            local_radius,
                            amp,
                            0x51A7_u64.wrapping_add(idx as u64),
                        );
                    }
                    _ => return None,
                }
            }
            Some(Self::build_edit_path_op(&tab.document, oid, path))
        });

        let Some(cmd) = edited_op else {
            return false;
        };
        if self.execute_command(cmd).is_ok() {
            if let Some(tab) = self.active_tab_mut() {
                tab.selection.select_single(&tab.document, oid);
            }
            true
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // Real Crop, Knife, Virtual Segment Delete & Vector Eraser
    // -------------------------------------------------------------------------

    /// Performs a REAL undoable crop on the active page: removes objects completely outside
    /// `crop`, translates all remaining active-page objects by `(-crop.min_x, -crop.min_y)`,
    /// and resizes the active page to `(crop.width(), crop.height())`.
    pub fn apply_crop_rect(&mut self, crop: Rect2D) -> bool {
        if !crop.is_finite() || crop.width() < 1.0 || crop.height() < 1.0 {
            return false;
        }
        let plan = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return false;
            };
            let page_idx = tab.document.active_page_index();
            let Some(page) = tab.document.page(page_idx) else {
                return false;
            };
            let page_id = page.id;
            let mut outside_ids = Vec::new();
            let mut inside_ids = Vec::new();
            let mut seen = HashSet::new();
            for layer in &page.layers {
                if layer.locked {
                    continue;
                }
                for &oid in &layer.objects {
                    if !seen.insert(oid) {
                        continue;
                    }
                    if let Some(obj) = tab.document.object(oid) {
                        if obj.locked {
                            continue;
                        }
                    }
                    let intersects = tab
                        .document
                        .world_object_bounds(oid)
                        .is_none_or(|bb| bb.intersects(crop));
                    if intersects {
                        inside_ids.push(oid);
                    } else {
                        outside_ids.push(oid);
                    }
                }
            }

            let mut ops = Vec::new();
            if !outside_ids.is_empty() {
                ops.push(CommandOp::DeleteObjects {
                    ids: outside_ids.clone(),
                });
            }
            if !inside_ids.is_empty() && (crop.min_x.abs() > 1e-6 || crop.min_y.abs() > 1e-6) {
                ops.push(CommandOp::TransformObjects {
                    ids: inside_ids,
                    affine: Affine2D::translate(-crop.min_x, -crop.min_y),
                });
            }
            let new_w = crop.width().clamp(1.0, 50_000.0);
            let new_h = crop.height().clamp(1.0, 50_000.0);
            ops.push(CommandOp::SetPageProperties {
                page_id,
                name: None,
                width_mm: Some(new_w),
                height_mm: Some(new_h),
                bleed_mm: None,
                facing_pages: None,
                background_fill: None,
                printable_background: None,
            });
            (ops, new_w, new_h, outside_ids.len())
        };

        let (ops, w, h, removed) = plan;
        let cmd = if ops.len() == 1 {
            ops.into_iter().next().expect("len == 1")
        } else {
            CommandOp::Batch(ops)
        };
        if self.execute_command(cmd).is_err() {
            return false;
        }
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.pending_crop_rect = None;
            tab.tool_core.pending_crop_rect = None;
            tab.transient_overlay = TransientOverlay::default();
            tab.fit_active_page();
        }
        self.checkpoint_wal_snapshot_active();
        self.push_toast(
            format!("Cropped page to {w:.1} × {h:.1} mm ({removed} outside object(s) removed)"),
            false,
        );
        true
    }

    /// Slices the selected (or line-intersected) vector shape along segment `a -> b`.
    pub fn apply_knife_cut(&mut self, a: Point2D, b: Point2D) -> usize {
        #[allow(clippy::too_many_arguments)]
        fn collect_cuttable_targets(
            doc: &Document,
            id: ObjectId,
            a: Point2D,
            b: Point2D,
            cut_bb: Rect2D,
            depth: usize,
            visiting: &mut HashSet<ObjectId>,
            out: &mut Vec<ObjectId>,
        ) {
            if depth > 64 || !visiting.insert(id) {
                return;
            }
            if !is_object_editable_in_doc(doc, id) {
                return;
            }
            let Some(obj) = doc.object(id) else {
                return;
            };
            match &obj.payload {
                NodePayload::Path(_) | NodePayload::Shape(_) => {
                    if doc
                        .world_object_bounds(id)
                        .is_some_and(|wb| wb.intersects(cut_bb))
                    {
                        out.push(id);
                    }
                }
                NodePayload::Group(children) => {
                    for &cid in children.iter().rev() {
                        collect_cuttable_targets(doc, cid, a, b, cut_bb, depth + 1, visiting, out);
                    }
                }
                NodePayload::PowerClip { contents, .. } => {
                    let Some(pc_wp) = doc.world_path_of(id) else {
                        return;
                    };
                    if !pc_wp.bounding_box().is_some_and(|bb| bb.intersects(cut_bb)) {
                        return;
                    }
                    let enters_pc = pc_wp.contains_point(a)
                        || pc_wp.contains_point(b)
                        || coree_core::knife_cut(&pc_wp, a, b, true).len() > 1
                        || coree_core::knife_cut(&pc_wp, a, b, false).len() > 1;
                    if enters_pc {
                        for &cid in contents.iter().rev() {
                            collect_cuttable_targets(
                                doc,
                                cid,
                                a,
                                b,
                                cut_bb,
                                depth + 1,
                                visiting,
                                out,
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        let cut_bb = Rect2D::from_points(a, b).expand(1.0);
        let knife_plan = self.active_tab().and_then(|tab| {
            let mut visiting = HashSet::new();
            let mut candidates = Vec::new();
            if tab.selection.objects.is_empty() {
                let hits = coree_render::hit_test_rect(
                    &tab.document,
                    tab.document.active_page_index(),
                    cut_bb,
                    false,
                );
                for hit_id in hits {
                    collect_cuttable_targets(
                        &tab.document,
                        hit_id,
                        a,
                        b,
                        cut_bb,
                        0,
                        &mut visiting,
                        &mut candidates,
                    );
                }
            } else {
                for &id in &tab.selection.objects {
                    collect_cuttable_targets(
                        &tab.document,
                        id,
                        a,
                        b,
                        cut_bb,
                        0,
                        &mut visiting,
                        &mut candidates,
                    );
                }
            }

            let mut chosen: Option<(ObjectId, &SceneObject, Vec<BezierPath>)> = None;
            for oid in candidates {
                if !is_object_editable_in_doc(&tab.document, oid) {
                    continue;
                }
                let target_layer_id =
                    enclosing_layer_of_object(&tab.document, oid).or(tab.active_layer_id);
                if target_layer_id
                    .is_some_and(|lid| tab.document.find_layer(lid).is_some_and(|l| l.locked))
                {
                    continue;
                }
                let Some(wp) = tab.document.world_path_of(oid) else {
                    continue;
                };
                let Some(obj) = tab.document.object(oid) else {
                    continue;
                };
                let pieces = coree_core::knife_cut(&wp, a, b, true);
                if pieces.len() > 1 {
                    chosen = Some((oid, obj, pieces));
                    break;
                }
            }

            let (oid, obj, pieces) = chosen?;
            let obj_world = world_transform_of_or_local(&tab.document, oid);
            let (inv_piece_xf, preserved_transform) = if let Some(inv_world) = obj_world.inverse() {
                (inv_world, obj.transform)
            } else {
                let inv_parent = tab
                    .document
                    .parent_container_of(oid)
                    .and_then(|pid| tab.document.world_transform_of(pid))
                    .and_then(|xf| xf.inverse())
                    .unwrap_or(Affine2D::IDENTITY);
                (inv_parent, Affine2D::IDENTITY)
            };

            let mut next_id = tab.document.next_id().max(1);
            let mut new_ids = Vec::with_capacity(pieces.len());
            let mut replacements = Vec::with_capacity(pieces.len());
            for (i, piece) in pieces.into_iter().enumerate() {
                let nid = ObjectId(next_id);
                next_id = next_id.saturating_add(1);
                new_ids.push(nid);
                let local_piece = piece.transformed(&inv_piece_xf);
                let mut new_obj = SceneObject::new(
                    nid,
                    format!("{} Slice {}", obj.name, i + 1),
                    NodePayload::Path(local_piece),
                );
                new_obj.transform = preserved_transform;
                new_obj.fill = obj.fill.clone();
                new_obj.outline = obj.outline.clone();
                new_obj.effects = obj.effects.clone();
                new_obj.lens = obj.lens.clone();
                new_obj.style_id = obj.style_id;
                new_obj.opacity = obj.opacity;
                new_obj.blend_mode = obj.blend_mode;
                new_obj.visible = obj.visible;
                new_obj.printable = obj.printable;
                new_obj.unknown_chunks = obj.unknown_chunks.clone();
                replacements.push(new_obj);
            }
            Some((
                CommandOp::ReplaceObjectInParent {
                    id: oid,
                    replacements,
                },
                new_ids,
            ))
        });

        if let Some((op, new_ids)) = knife_plan {
            let count = new_ids.len();
            if self.execute_command(op).is_ok() {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.select_many(&tab.document, &new_ids);
                    tab.metrics_dirty = true;
                }
                self.push_toast(format!("Knife sliced shape into {count} piece(s)"), false);
                return count;
            }
        }
        0
    }

    /// Deletes the curve segment between intersections nearest to `pt`.
    pub fn apply_virtual_segment_delete_at(&mut self, pt: Point2D) -> bool {
        let edit_op = self.active_tab().and_then(|tab| {
            let tol = (8.0 / tab.viewport.px_per_mm()).max(2.0);
            let oid = hit_test_point(&tab.document, tab.document.active_page_index(), pt, tol)?;
            if !is_object_editable_in_doc(&tab.document, oid) {
                return None;
            }
            if !tab
                .document
                .object(oid)
                .is_some_and(|o| matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_)))
            {
                return None;
            }
            let target_path = tab.document.world_path_of(oid)?;
            let cutters: Vec<BezierPath> = tab
                .document
                .objects()
                .keys()
                .filter(|&&id| id != oid)
                .filter_map(|&id| tab.document.world_path_of(id))
                .collect();
            let updated_world = coree_core::virtual_segment_delete(&target_path, &cutters, pt, tol);
            let inv = world_inverse_transform_of(&tab.document, oid);
            Some(Self::build_edit_path_op(
                &tab.document,
                oid,
                updated_world.transformed(&inv),
            ))
        });
        if let Some(op) = edit_op {
            if self.execute_command(op).is_ok() {
                self.push_toast("Deleted virtual curve segment", false);
                return true;
            }
        }
        false
    }

    /// Vector Eraser: erases a circular swath along `points` from the selected (or touched) vector shape.
    pub fn apply_vector_eraser_stroke(&mut self, points: &[Point2D]) -> bool {
        if points.is_empty() {
            return false;
        }
        let radius = self.tool_settings.eraser_radius_mm.max(0.25);
        let step = (radius * 0.4).max(0.25);
        let edit = self.active_tab().and_then(|tab| {
            let is_editable_vector = |id: ObjectId| {
                tab.document.object(id).is_some_and(|o| {
                    matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                }) && tab.document.world_path_of(id).is_some()
            };
            let oid = tab
                .selection
                .objects
                .iter()
                .copied()
                .find(|&id| is_editable_vector(id))
                .or_else(|| {
                    points.iter().find_map(|&p| {
                        hit_test_point(&tab.document, tab.document.active_page_index(), p, radius)
                            .filter(|&id| is_editable_vector(id))
                    })
                })?;
            if !is_object_editable_in_doc(&tab.document, oid) {
                return None;
            }
            let wp = tab.document.world_path_of(oid)?;
            let mut centers = Vec::with_capacity(points.len());
            centers.push(points[0]);
            for seg in points.windows(2) {
                let dist = seg[0].distance_to(seg[1]);
                let steps = ((dist / step).ceil() as usize).clamp(1, 256);
                for k in 1..=steps {
                    let t = k as f64 / steps as f64;
                    centers.push(seg[0].lerp(seg[1], t));
                }
            }
            let mut pieces = vec![wp];
            for center in centers {
                let mut next_pieces = Vec::new();
                for piece in &pieces {
                    if !piece.is_empty() {
                        next_pieces.extend(coree_core::erase_circle(piece, center, radius));
                    }
                }
                pieces = next_pieces;
                if pieces.is_empty() {
                    break;
                }
            }
            let combined = BezierPath {
                subpaths: pieces.into_iter().flat_map(|p| p.subpaths).collect(),
            };
            let inv = world_inverse_transform_of(&tab.document, oid);
            Some(Self::build_edit_path_op(
                &tab.document,
                oid,
                combined.transformed(&inv),
            ))
        });
        if let Some(op) = edit {
            self.execute_command(op).is_ok()
        } else {
            false
        }
    }

    // -------------------------------------------------------------------------
    // Bezier / Pen, B-Spline, 3-Point Curve, 3-Point Rect/Ellipse & Enter Confirm
    // -------------------------------------------------------------------------
}
