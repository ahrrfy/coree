// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Runs `coree_prod::run_preflight` on the active document.
    pub fn run_preflight_active(&mut self) -> Option<PreflightReport> {
        let min_dpi = self.production_state.preflight_min_dpi;
        let mut rules = self.production_state.preflight_rules.clone();
        rules.min_dpi = min_dpi;
        let tab = self.tabs.get_mut(self.active_tab)?;
        let report = run_preflight(&tab.document, &rules);
        let summary = format!(
            "Preflight: {} error(s), {} warning(s), {} info",
            report.error_count(),
            report.warning_count(),
            report.info_count()
        );
        tab.preflight_report = Some(report.clone());
        self.push_toast(summary, report.has_errors());
        Some(report)
    }

    /// Computes CMYK + Spot color separations on the active page.
    pub fn compute_separations_active(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let page_idx = tab.document.active_page_index();
        let mut text_engine = TextEngine::new();
        match compute_separations_with_text_outliner(
            &tab.document,
            page_idx,
            &SeparationOptions::default(),
            |obj, doc| outline_text_object_with_engine(&mut text_engine, obj, doc),
        ) {
            Ok(sep) => {
                let plate_count = sep.plates.len();
                let tac = sep.max_tac_pct;
                tab.separations_preview = Some(sep);
                self.push_toast(
                    format!("Computed {plate_count} separation plate(s) · Max TAC {tac:.1}%"),
                    false,
                );
            }
            Err(e) => self.push_toast(format!("Separations failed: {e}"), true),
        }
    }

    /// Exports vector CMYK + Spot color separation plates as a multi-page PDF.
    pub fn export_separated_plates_pdf_active(&mut self, out_path: &Path) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let page_idx = tab.document.active_page_index();
        match export_separated_plates_pdf(
            &tab.document,
            page_idx,
            &SeparationOptions::default(),
            out_path,
        ) {
            Ok(plates) => {
                self.push_toast(
                    format!(
                        "Exported {} separated PDF plate(s) to {}",
                        plates.len(),
                        out_path.display()
                    ),
                    false,
                );
            }
            Err(e) => self.push_toast(format!("Separated PDF failed: {e}"), true),
        }
    }

    /// Applies prepress color trapping (`apply_trapping`) on the active page via `CommandHistory`.
    pub fn apply_trapping_active(&mut self) {
        let ps = &self.production_state;
        let settings = TrappingSettings {
            trap_width_mm: ps.trap_width_mm.clamp(0.01, 5.0),
            placement: ps.trap_placement,
            ..TrappingSettings::default()
        };
        let plan: Result<(Vec<CommandOp>, usize), String> = (|| {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let page_idx = tab.document.active_page_index();
            let page_id = tab
                .document
                .page(page_idx)
                .map(|p| p.id)
                .ok_or_else(|| "active page out of range".to_owned())?;
            let existing_layers: HashSet<LayerId> = tab
                .document
                .pages()
                .iter()
                .flat_map(|p| p.layers.iter().map(|l| l.id))
                .collect();
            let existing_obj_ids: HashSet<ObjectId> =
                tab.document.objects().keys().copied().collect();

            tab.history
                .begin_transaction(&tab.document, "Prepress Trapping")
                .map_err(|e| e.to_string())?;
            let res = match apply_trapping(&mut tab.document, page_idx, &settings) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tab.history.cancel_transaction(&mut tab.document);
                    return Err(e.to_string());
                }
            };

            let mut ops = Vec::new();
            if !res.traps.is_empty() {
                if let Some(trap_layer) = tab.document.page(page_idx).and_then(|p| {
                    p.layers
                        .iter()
                        .find(|l| l.name.eq_ignore_ascii_case("Prepress Traps"))
                }) {
                    let trap_layer_id = trap_layer.id;
                    let trap_layer_name = trap_layer.name.clone();
                    let trap_layer_cut = trap_layer.is_cut_layer;
                    let trap_layer_scope = trap_layer.master_scope;
                    let trap_obj_ids: Vec<ObjectId> = trap_layer
                        .objects
                        .iter()
                        .copied()
                        .filter(|oid| !existing_obj_ids.contains(oid))
                        .collect();
                    if !existing_layers.contains(&trap_layer_id) {
                        ops.push(CommandOp::AddLayer {
                            page_id,
                            name: trap_layer_name,
                            is_cut_layer: trap_layer_cut,
                            master_scope: trap_layer_scope,
                        });
                    }
                    for oid in trap_obj_ids {
                        if let Some(obj) = tab.document.object(oid) {
                            ops.push(CommandOp::create_object(obj.clone(), Some(trap_layer_id)));
                        }
                    }
                }
            }
            let n = res.traps.len();
            let _ = tab.history.cancel_transaction(&mut tab.document);
            Ok((ops, n))
        })();

        match plan {
            Ok((ops, n)) => {
                if !ops.is_empty() {
                    if let Err(e) = self.execute_command(CommandOp::Batch(ops)) {
                        self.push_toast(format!("Trapping failed: {e}"), true);
                        return;
                    }
                    self.checkpoint_wal_snapshot_active();
                }
                self.push_toast(format!("Generated {n} prepress trap zone(s)"), false);
            }
            Err(e) => {
                if e != "no active document" {
                    self.push_toast(format!("Trapping failed: {e}"), true);
                }
            }
        }
    }

    /// Generates halftone screen preview across active separation plates.
    pub fn apply_halftone_preview_active(&mut self) {
        let ps = &self.production_state;
        let lpi = ps.halftone_lpi.clamp(10.0, 400.0);
        let shape = ps.halftone_shape;
        let mut hs = HalftoneSettings::default();
        hs.cyan.lpi = lpi;
        hs.cyan.dot_shape = shape;
        hs.magenta.lpi = lpi;
        hs.magenta.dot_shape = shape;
        hs.yellow.lpi = lpi;
        hs.yellow.dot_shape = shape;
        hs.black.lpi = lpi;
        hs.black.dot_shape = shape;

        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let page_idx = tab.document.active_page_index();
        let sep = match tab.separations_preview.clone() {
            Some(s) => s,
            None => {
                let mut te = TextEngine::new();
                match compute_separations_with_text_outliner(
                    &tab.document,
                    page_idx,
                    &SeparationOptions::default(),
                    |obj, doc| outline_text_object_with_engine(&mut te, obj, doc),
                ) {
                    Ok(s) => {
                        tab.separations_preview = Some(s.clone());
                        s
                    }
                    Err(e) => {
                        self.push_toast(format!("Halftone separations failed: {e}"), true);
                        return;
                    }
                }
            }
        };
        match generate_halftone_preview(&sep, &hs) {
            Ok(plates) => {
                self.push_toast(
                    format!(
                        "Halftone screened {} plate(s) at {:.0} LPI ({shape:?})",
                        plates.len(),
                        lpi
                    ),
                    false,
                );
            }
            Err(e) => self.push_toast(format!("Halftone screening failed: {e}"), true),
        }
    }

    /// Plans laser toolpaths and exports framed controller stream (`Grbl`/`Marlin`/`Ruida`/`LightBurn`/`EGV`).
    pub fn export_laser_protocol_active(&mut self, out_path: &Path) {
        let protocol = self.production_state.laser_protocol;
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let page_idx = tab.document.active_page_index();
        let cfg = LaserJobConfig::default();
        match plan_laser_toolpath(&tab.document, page_idx, None, &cfg) {
            Ok(plan) => match frame_open_laser_protocol(&plan, &cfg, protocol) {
                Ok(frame) => match std::fs::write(out_path, &frame.payload) {
                    Ok(()) => self.push_toast(
                        format!(
                            "Exported {:?} ({} bytes) to {}",
                            protocol,
                            frame.payload.len(),
                            out_path.display()
                        ),
                        false,
                    ),
                    Err(e) => self.push_toast(format!("Write failed: {e}"), true),
                },
                Err(e) => self.push_toast(format!("Laser framing failed: {e}"), true),
            },
            Err(e) => self.push_toast(format!("Laser toolpath failed: {e}"), true),
        }
    }

    /// One-click welded `CutContour` spot hairline around selected (or all) objects.
    pub fn generate_cut_contour_active(&mut self) {
        let offset = self.production_state.cut_offset_mm;
        let spot = self.production_state.cut_spot_name.clone();
        let plan: Result<(Vec<CommandOp>, ObjectId, String, f64), String> = (|| {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let targets: Vec<ObjectId> = if !tab.selection.is_empty() {
                tab.selection.objects.clone()
            } else {
                tab.document
                    .active_page()
                    .map(|p| {
                        p.layers
                            .iter()
                            .filter(|l| !l.is_cut_layer && l.visible)
                            .flat_map(|l| l.objects.iter().copied())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let existing_layers: HashSet<LayerId> = tab
                .document
                .pages()
                .iter()
                .flat_map(|p| p.layers.iter().map(|l| l.id))
                .collect();

            tab.history
                .begin_transaction(&tab.document, "Generate CutContour")
                .map_err(|e| e.to_string())?;
            let res = match generate_cut_contour(&mut tab.document, &targets, offset, &spot) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tab.history.cancel_transaction(&mut tab.document);
                    return Err(e.to_string());
                }
            };

            let mut ops = Vec::new();
            if !existing_layers.contains(&res.layer_id) {
                let page_id = tab
                    .document
                    .pages()
                    .iter()
                    .find(|p| p.layers.iter().any(|l| l.id == res.layer_id))
                    .map(|p| p.id)
                    .ok_or_else(|| {
                        let _ = tab.history.cancel_transaction(&mut tab.document);
                        "cut contour layer missing from page".to_owned()
                    })?;
                if let Some(layer) = tab.document.find_layer(res.layer_id) {
                    ops.push(CommandOp::AddLayer {
                        page_id,
                        name: layer.name.clone(),
                        is_cut_layer: layer.is_cut_layer,
                        master_scope: layer.master_scope,
                    });
                }
            } else {
                ops.push(CommandOp::SetLayerFlags {
                    layer_id: res.layer_id,
                    visible: Some(true),
                    locked: None,
                    printable: Some(true),
                    is_cut_layer: Some(true),
                });
            }
            if let Some(obj) = tab.document.object(res.object_id) {
                ops.push(CommandOp::create_object(obj.clone(), Some(res.layer_id)));
            }
            let _ = tab.history.cancel_transaction(&mut tab.document);
            Ok((ops, res.object_id, res.spot_name, res.cut_length_mm))
        })();

        match plan {
            Ok((ops, object_id, spot_name, cut_length_mm)) => {
                if !ops.is_empty() {
                    let cmd = if ops.len() == 1 {
                        ops.into_iter().next().expect("len == 1")
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(e) = self.execute_command(cmd) {
                        self.push_toast(format!("CutContour failed: {e}"), true);
                        return;
                    }
                }
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.selection.select_single(&tab.document, object_id);
                }
                self.push_toast(
                    format!("Created {spot_name} ({cut_length_mm:.1} mm cut path)"),
                    false,
                );
            }
            Err(e) => {
                if e != "no active document" {
                    self.push_toast(format!("CutContour failed: {e}"), true);
                }
            }
        }
    }

    /// Generates a parametric packaging dieline (`TuckEndBox`, `Envelope`, `ShoppingBag`, `StickerSheet`).
    pub fn generate_dieline_active(&mut self) {
        let ps = &self.production_state;
        let kind = match ps.dieline_type {
            1 => DielineKind::Envelope {
                width_mm: ps.box_l_mm.max(40.0),
                height_mm: ps.box_h_mm.max(40.0),
                flap_mm: (ps.box_w_mm * 0.6).max(15.0),
            },
            2 => DielineKind::ShoppingBag {
                width_mm: ps.box_l_mm.max(50.0),
                depth_mm: ps.box_w_mm.max(25.0),
                height_mm: ps.box_h_mm.max(60.0),
                fold_mm: 20.0,
            },
            3 => DielineKind::StickerSheet {
                label_w_mm: ps.box_l_mm.clamp(15.0, 120.0),
                label_h_mm: ps.box_w_mm.clamp(15.0, 120.0),
                corner_r_mm: 3.0,
                cols: ps.sticker_cols.clamp(1, 20),
                rows: ps.sticker_rows.clamp(1, 20),
                gap_mm: 4.0,
            },
            4 => DielineKind::Fefco {
                code: ps.fefco_code,
                length_mm: ps.box_l_mm.max(20.0),
                width_mm: ps.box_w_mm.max(15.0),
                height_mm: ps.box_h_mm.max(20.0),
                flute: ps.flute_type,
                custom_caliper_mm: None,
            },
            _ => DielineKind::TuckEndBox {
                length_mm: ps.box_l_mm.max(20.0),
                width_mm: ps.box_w_mm.max(15.0),
                height_mm: ps.box_h_mm.max(20.0),
                glue_flap_mm: 12.0,
                tuck_flap_mm: 14.0,
            },
        };
        let plan: Result<(Vec<CommandOp>, f64, f64), String> = (|| {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let page_idx = tab.document.active_page_index();
            let page_id = tab
                .document
                .page(page_idx)
                .map(|p| p.id)
                .ok_or_else(|| "active page out of range".to_owned())?;
            let existing_layers: HashSet<LayerId> = tab
                .document
                .pages()
                .iter()
                .flat_map(|p| p.layers.iter().map(|l| l.id))
                .collect();

            tab.history
                .begin_transaction(&tab.document, "Generate Dieline")
                .map_err(|e| e.to_string())?;
            let res = match generate_dieline(
                &mut tab.document,
                page_idx,
                &kind,
                Point2D::new(10.0, 10.0),
            ) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tab.history.cancel_transaction(&mut tab.document);
                    return Err(e.to_string());
                }
            };

            let mut ops = Vec::new();
            for lid in [res.cut_layer_id, res.crease_layer_id] {
                if !existing_layers.contains(&lid) {
                    if let Some(layer) = tab.document.find_layer(lid) {
                        ops.push(CommandOp::AddLayer {
                            page_id,
                            name: layer.name.clone(),
                            is_cut_layer: layer.is_cut_layer,
                            master_scope: layer.master_scope,
                        });
                    }
                } else {
                    ops.push(CommandOp::SetLayerFlags {
                        layer_id: lid,
                        visible: Some(true),
                        locked: None,
                        printable: Some(true),
                        is_cut_layer: Some(true),
                    });
                }
            }
            for &oid in &res.cut_object_ids {
                if let Some(obj) = tab.document.object(oid) {
                    ops.push(CommandOp::create_object(
                        obj.clone(),
                        Some(res.cut_layer_id),
                    ));
                }
            }
            for &oid in &res.crease_object_ids {
                if let Some(obj) = tab.document.object(oid) {
                    ops.push(CommandOp::create_object(
                        obj.clone(),
                        Some(res.crease_layer_id),
                    ));
                }
            }
            let _ = tab.history.cancel_transaction(&mut tab.document);
            Ok((ops, res.cut_length_mm, res.crease_length_mm))
        })();

        match plan {
            Ok((ops, cut_length_mm, crease_length_mm)) => {
                if !ops.is_empty() {
                    let cmd = if ops.len() == 1 {
                        ops.into_iter().next().expect("len == 1")
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(e) = self.execute_command(cmd) {
                        self.push_toast(format!("Dieline error: {e}"), true);
                        return;
                    }
                }
                self.push_toast(
                    format!("Dieline generated: Cut {cut_length_mm:.0} mm | Crease {crease_length_mm:.0} mm"),
                    false,
                );
            }
            Err(e) => {
                if e != "no active document" {
                    self.push_toast(format!("Dieline error: {e}"), true);
                }
            }
        }
    }

    /// Inserts a crisp 1D vector barcode onto the active page.
    pub fn insert_barcode_active(&mut self) {
        let Some(layer_id) = self.ensure_active_creation_layer() else {
            return;
        };
        let sym = self.production_state.barcode_symbology;
        let payload = self.production_state.barcode_payload.clone();
        let bc_opts = BarcodeOptions {
            bar_width_reduction_mm: self.production_state.barcode_bwr_mm.max(0.0),
            code128_mode: self.production_state.code128_mode,
            ..BarcodeOptions::default()
        };
        let plan: Result<(Vec<CommandOp>, ObjectId), String> = (|| {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            tab.history
                .begin_transaction(&tab.document, "Insert Barcode")
                .map_err(|e| e.to_string())?;
            let group_id = match add_barcode_to_document(
                &mut tab.document,
                sym,
                &payload,
                Point2D::new(20.0, 20.0),
                &bc_opts,
                layer_id,
            ) {
                Ok(oid) => oid,
                Err(e) => {
                    let _ = tab.history.cancel_transaction(&mut tab.document);
                    return Err(e.to_string());
                }
            };

            let mut ops = Vec::new();
            if let Some(group_obj) = tab.document.object(group_id).cloned() {
                if let NodePayload::Group(ref children) = group_obj.payload {
                    for &cid in children {
                        if let Some(child_obj) = tab.document.object(cid) {
                            ops.push(CommandOp::create_object(child_obj.clone(), layer_id));
                        }
                    }
                    ops.push(CommandOp::Group {
                        ids: children.clone(),
                        name: group_obj.name.clone(),
                    });
                    if group_obj.transform != Affine2D::IDENTITY {
                        ops.push(CommandOp::TransformObjects {
                            ids: vec![group_id],
                            affine: group_obj.transform,
                        });
                    }
                }
            }
            let _ = tab.history.cancel_transaction(&mut tab.document);
            Ok((ops, group_id))
        })();

        match plan {
            Ok((ops, oid)) => {
                if !ops.is_empty() {
                    let cmd = if ops.len() == 1 {
                        ops.into_iter().next().expect("len == 1")
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(e) = self.execute_command(cmd) {
                        self.push_toast(format!("Barcode error: {e}"), true);
                        return;
                    }
                }
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.selection.select_single(&tab.document, oid);
                }
                self.push_toast(format!("Inserted {sym:?} barcode"), false);
            }
            Err(e) => {
                if e != "no active document" {
                    self.push_toast(format!("Barcode error: {e}"), true);
                }
            }
        }
    }

    /// Inserts a crisp vector QR code onto the active page.
    pub fn insert_qr_active(&mut self) {
        let Some(layer_id) = self.ensure_active_creation_layer() else {
            return;
        };
        let payload = self.production_state.qr_payload.clone();
        let opts = QrCodeOptions {
            size_mm: self.production_state.qr_size_mm.clamp(8.0, 500.0),
            ec_level: self.production_state.qr_ec,
            ..QrCodeOptions::default()
        };
        let plan: Result<(Vec<CommandOp>, ObjectId), String> = (|| {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            tab.history
                .begin_transaction(&tab.document, "Insert QR Code")
                .map_err(|e| e.to_string())?;
            let group_id = match add_qr_code_to_document(
                &mut tab.document,
                &payload,
                Point2D::new(25.0, 25.0),
                &opts,
                layer_id,
            ) {
                Ok(oid) => oid,
                Err(e) => {
                    let _ = tab.history.cancel_transaction(&mut tab.document);
                    return Err(e.to_string());
                }
            };

            let mut ops = Vec::new();
            if let Some(group_obj) = tab.document.object(group_id).cloned() {
                if let NodePayload::Group(ref children) = group_obj.payload {
                    for &cid in children {
                        if let Some(child_obj) = tab.document.object(cid) {
                            ops.push(CommandOp::create_object(child_obj.clone(), layer_id));
                        }
                    }
                    ops.push(CommandOp::Group {
                        ids: children.clone(),
                        name: group_obj.name.clone(),
                    });
                    if group_obj.transform != Affine2D::IDENTITY {
                        ops.push(CommandOp::TransformObjects {
                            ids: vec![group_id],
                            affine: group_obj.transform,
                        });
                    }
                }
            }
            let _ = tab.history.cancel_transaction(&mut tab.document);
            Ok((ops, group_id))
        })();

        match plan {
            Ok((ops, oid)) => {
                if !ops.is_empty() {
                    let cmd = if ops.len() == 1 {
                        ops.into_iter().next().expect("len == 1")
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(e) = self.execute_command(cmd) {
                        self.push_toast(format!("QR error: {e}"), true);
                        return;
                    }
                }
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.selection.select_single(&tab.document, oid);
                }
                self.push_toast("Inserted vector QR Code", false);
            }
            Err(e) => {
                if e != "no active document" {
                    self.push_toast(format!("QR error: {e}"), true);
                }
            }
        }
    }

    /// Generates vector print marks (crop, bleed, registration, color bars, page slug) on the active page.
    pub fn add_print_marks_active(&mut self) {
        let plan: Result<(Vec<CommandOp>, usize), String> = (|| {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let page_idx = tab.document.active_page_index();
            let page_id = tab
                .document
                .page(page_idx)
                .map(|p| p.id)
                .ok_or_else(|| "active page out of range".to_owned())?;
            let existing_layers: HashSet<LayerId> = tab
                .document
                .pages()
                .iter()
                .flat_map(|p| p.layers.iter().map(|l| l.id))
                .collect();

            tab.history
                .begin_transaction(&tab.document, "Add Print Marks")
                .map_err(|e| e.to_string())?;
            let res = match generate_print_marks(
                &mut tab.document,
                page_idx,
                &PrintMarksOptions::default(),
            ) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tab.history.cancel_transaction(&mut tab.document);
                    return Err(e.to_string());
                }
            };

            let mut ops = Vec::new();
            if !existing_layers.contains(&res.layer_id) {
                if let Some(layer) = tab.document.find_layer(res.layer_id) {
                    ops.push(CommandOp::AddLayer {
                        page_id,
                        name: layer.name.clone(),
                        is_cut_layer: layer.is_cut_layer,
                        master_scope: layer.master_scope,
                    });
                }
            }
            let mut new_obj_ids = Vec::with_capacity(res.total_objects());
            new_obj_ids.extend(res.crop_mark_ids.iter().copied());
            new_obj_ids.extend(res.bleed_mark_ids.iter().copied());
            new_obj_ids.extend(res.registration_target_ids.iter().copied());
            new_obj_ids.extend(res.color_bar_ids.iter().copied());
            if let Some(slug) = res.page_slug_id {
                new_obj_ids.push(slug);
            }
            for oid in new_obj_ids {
                if let Some(obj) = tab.document.object(oid) {
                    ops.push(CommandOp::create_object(obj.clone(), Some(res.layer_id)));
                }
            }
            let total = res.total_objects();
            let _ = tab.history.cancel_transaction(&mut tab.document);
            Ok((ops, total))
        })();

        match plan {
            Ok((ops, total)) => {
                if !ops.is_empty() {
                    if let Err(e) = self.execute_command(CommandOp::Batch(ops)) {
                        self.push_toast(format!("Print marks failed: {e}"), true);
                        return;
                    }
                }
                self.push_toast(format!("Added {total} print mark object(s)"), false);
            }
            Err(e) => {
                if e != "no active document" {
                    self.push_toast(format!("Print marks failed: {e}"), true);
                }
            }
        }
    }

    /// Runs Imposition (`N-Up`, `Booklet`, `Tiling`) and opens the imposed result in a new document tab.
    pub fn run_imposition_active(&mut self) {
        let ps = &self.production_state;
        let mode = match ps.imposition_kind {
            1 => ImpositionMode::BookletSaddleStitch {
                sheet_w_mm: ps.sheet_w_mm.max(50.0),
                sheet_h_mm: ps.sheet_h_mm.max(50.0),
            },
            2 => ImpositionMode::Tiling {
                tile_w_mm: ps.tile_w_mm.max(30.0),
                tile_h_mm: ps.tile_h_mm.max(30.0),
                overlap_mm: ps.tile_overlap_mm.max(0.0),
            },
            3 => ImpositionMode::PerfectBound {
                sheet_w_mm: ps.sheet_w_mm.max(50.0),
                sheet_h_mm: ps.sheet_h_mm.max(50.0),
                signature_pages: ps.imposition_signature_pages.max(4) as usize,
                spine_thickness_mm: ps.spine_thickness_mm.max(0.0),
                creep_mm: ps.creep_mm.max(0.0),
                milling_allowance_mm: ps.milling_allowance_mm.max(0.0),
            },
            _ => ImpositionMode::NUp {
                cols: ps.nup_cols.clamp(1, 20),
                rows: ps.nup_rows.clamp(1, 20),
                gap_x_mm: 4.0,
                gap_y_mm: 4.0,
                sheet_w_mm: ps.sheet_w_mm.max(50.0),
                sheet_h_mm: ps.sheet_h_mm.max(50.0),
            },
        };
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            return;
        };
        match impose_document(&doc, &mode) {
            Ok(imposed) => {
                let n = imposed.pages().len();
                self.new_document(imposed);
                self.push_toast(format!("Imposed into {n} press sheet(s)"), false);
            }
            Err(e) => self.push_toast(format!("Imposition error: {e}"), true),
        }
    }

    /// Runs CSV Print Merge on the active document and opens the merged document in a new tab.
    pub fn run_print_merge_active(&mut self) {
        let csv_text = self.production_state.merge_csv_text.clone();
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            return;
        };
        let table = match PrintMergeTable::from_csv(&csv_text) {
            Ok(t) => t,
            Err(e) => {
                self.push_toast(format!("CSV error: {e}"), true);
                return;
            }
        };
        match apply_print_merge(&doc, &table, &PrintMergeOptions::default()) {
            Ok(merged) => {
                let pages = merged.pages().len();
                self.new_document(merged);
                self.push_toast(format!("Print Merge generated {pages} page(s)"), false);
            }
            Err(e) => self.push_toast(format!("Print Merge failed: {e}"), true),
        }
    }

    /// Runs `Collect for Output` packaging into `output_dir`.
    pub fn run_collect_for_output(&mut self, output_dir: &Path) {
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            return;
        };
        match collect_for_output(&doc, output_dir, &CollectOptions::default()) {
            Ok(rep) => {
                self.production_state.last_production_note = format!(
                    "Bundled {} file(s) to {}",
                    rep.total_files_written,
                    rep.output_dir.display()
                );
                self.push_toast(self.production_state.last_production_note.clone(), false);
            }
            Err(e) => self.push_toast(format!("Collect failed: {e}"), true),
        }
    }

    /// Recomputes `laser_plan_preview` from the active page (and active selection if non-empty).
    pub fn refresh_laser_plan_preview(&mut self) {
        let plan_res = self.active_tab().map(|tab| {
            let page_idx = tab.document.active_page_index();
            let sel = if tab.selection.is_empty() {
                None
            } else {
                Some(tab.selection.objects.as_slice())
            };
            plan_laser_toolpath(&tab.document, page_idx, sel, &self.laser_config)
        });
        match plan_res {
            Some(Ok(plan)) => {
                let msg = format!(
                    "Laser Plan: {} seg(s) · Cut {:.1} mm · Engrave {:.1} mm · ~{:.1}s",
                    plan.segments.len(),
                    plan.total_cut_mm,
                    plan.total_engrave_mm,
                    plan.estimated_seconds
                );
                self.laser_plan_preview = Some(plan);
                self.show_laser_toolpath_overlay = true;
                self.production_state.last_production_note = msg.clone();
                self.push_toast(msg, false);
            }
            Some(Err(err)) => {
                self.laser_plan_preview = None;
                self.push_toast(format!("Laser plan failed: {err}"), true);
            }
            None => {}
        }
    }

    /// Exports active page/selection for laser/plotter plugin `plugin_id` into an explicit path,
    /// optionally launching the detected external bridge executable.
    pub fn send_to_laser_plugin_to_path(
        &mut self,
        plugin_id: &str,
        out_path: &Path,
        launch: bool,
    ) -> Result<PluginExportOutcome, String> {
        let plugin = self
            .plugin_registry
            .find(plugin_id)
            .cloned()
            .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
        let Some(tab) = self.active_tab() else {
            return Err("no active document".to_owned());
        };
        let page_idx = tab.document.active_page_index();
        let sel = if tab.selection.is_empty() {
            None
        } else {
            Some(tab.selection.objects.as_slice())
        };
        let outcome = export_for_plugin(
            &tab.document,
            page_idx,
            sel,
            &plugin,
            &self.laser_config,
            out_path,
        )
        .map_err(|e| format!("plugin export failed: {e}"))?;

        self.last_plugin_export_path = Some(outcome.output_path.clone());
        let mut note = format!(
            "Exported {} bytes for '{}' -> {}",
            outcome.bytes_written,
            plugin.name,
            outcome.output_path.display()
        );

        if launch {
            if let PluginKind::ExternalBridge {
                ref args_template, ..
            } = plugin.kind
            {
                let override_path = self
                    .plugin_exec_overrides
                    .get(plugin_id)
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from);
                if let Some(exe) = detect_executable(&plugin, override_path.as_deref()) {
                    match launch_external_bridge(&exe, args_template, &outcome.output_path) {
                        Ok(pid) => {
                            note = format!(
                                "Launched '{}' (PID {pid}) with {}",
                                plugin.name,
                                outcome.output_path.display()
                            );
                        }
                        Err(e) => {
                            note = format!("{note} (launch skipped: {e})");
                        }
                    }
                } else {
                    note = format!("{note} (app not detected on PATH; file ready to open)");
                }
            }
        }
        self.production_state.last_production_note = note.clone();
        self.push_toast(note, false);
        Ok(outcome)
    }

    /// Exports active artwork for laser/plotter plugin `plugin_id` to a deterministic temp/export file
    /// and optionally launches the bridge executable.
    pub fn send_to_laser_plugin(&mut self, plugin_id: &str, launch: bool) {
        let ext = self
            .plugin_registry
            .find(plugin_id)
            .and_then(|p| match p.kind {
                PluginKind::ExternalBridge { export_format, .. } => {
                    Some(export_format.file_extension())
                }
                PluginKind::RhaiScript { .. } => None,
            })
            .unwrap_or("svg");
        let safe_id: String = plugin_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let out_dir = std::env::temp_dir().join("coree_plugin_exports");
        let _ = std::fs::create_dir_all(&out_dir);
        let out_path = out_dir.join(format!("coree_{safe_id}.{ext}"));
        if let Err(err) = self.send_to_laser_plugin_to_path(plugin_id, &out_path, launch) {
            self.push_toast(err, true);
        }
    }
}
