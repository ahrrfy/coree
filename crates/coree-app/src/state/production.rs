// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    // -------------------------------------------------------------------------
    // Production, Prepress, Trace, Bitmap Studio, Find & Replace, Macros
    // -------------------------------------------------------------------------

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

    /// Traces the selected raster object via `coree_render::trace_raster_to_mm` and adds grouped vector curves.
    pub fn trace_selected_raster(&mut self) -> Result<usize, String> {
        let layer_id = self.active_creation_layer_or_err()?;
        let cfg = self.trace_state.to_config();
        let is_centerline = matches!(cfg.mode, TraceMode::CenterlineTrace);
        let (ops, created_ids, count) = {
            let tab = self
                .tabs
                .get(self.active_tab)
                .ok_or_else(|| "no active document".to_owned())?;

            // Find selected raster object (or Clone of raster, or first raster on active page)
            let is_raster_or_clone = |id: ObjectId| -> bool {
                let Some(obj) = tab.document.object(id) else {
                    return false;
                };
                if matches!(obj.payload, NodePayload::Raster(_)) {
                    return true;
                }
                tab.document
                    .resolve_clone_payload_and_style_with_sym_scope(obj, None)
                    .is_some_and(|(p, _, _, _)| matches!(p, NodePayload::Raster(_)))
            };
            let target_id = tab
                .selection
                .objects
                .iter()
                .copied()
                .find(|id| is_raster_or_clone(*id))
                .or_else(|| {
                    let active_page_idx = tab.document.active_page_index();
                    tab.document
                        .objects()
                        .iter()
                        .find(|(&id, _)| {
                            is_raster_or_clone(id)
                                && is_object_editable_in_doc(&tab.document, id)
                                && enclosing_layer_of_object(&tab.document, id).is_some_and(|lid| {
                                    tab.document.pages().iter().enumerate().any(|(p_idx, p)| {
                                        p.layers.iter().any(|l| {
                                            l.id == lid
                                                && Document::layer_applies_to_page(
                                                    l,
                                                    p_idx,
                                                    active_page_idx,
                                                )
                                        })
                                    })
                                })
                        })
                        .map(|(&id, _)| id)
                })
                .ok_or_else(|| "select a raster bitmap object to run Vector Trace".to_owned())?;

            let (asset, filters, crop_rect, w_mm, h_mm, xf, persp) = {
                let obj = tab
                    .document
                    .object(target_id)
                    .ok_or_else(|| "raster object missing".to_owned())?;
                let resolved_payload = if let NodePayload::Raster(ref r) = obj.payload {
                    Some(r.clone())
                } else {
                    tab.document
                        .resolve_clone_payload_and_style_with_sym_scope(obj, None)
                        .and_then(|(p, _, _, _)| match p {
                            NodePayload::Raster(r) => Some(r),
                            _ => None,
                        })
                };
                let Some(r) = resolved_payload else {
                    return Err("object is not a raster".to_owned());
                };
                let asset = tab
                    .document
                    .asset(r.asset_id)
                    .cloned()
                    .ok_or_else(|| "embedded raster asset missing".to_owned())?;
                let xf = tab
                    .document
                    .world_transform_of(target_id)
                    .unwrap_or(obj.transform);
                (
                    asset,
                    r.active_filters(),
                    r.crop_rect,
                    r.width_mm,
                    r.height_mm,
                    xf,
                    obj.perspective,
                )
            };

            if !w_mm.is_finite() || w_mm <= 0.0 || !h_mm.is_finite() || h_mm <= 0.0 {
                return Err("raster has invalid dimensions".to_owned());
            }

            let filtered_asset = if filters.is_empty() {
                asset
            } else {
                coree_render::apply_raster_filters(&asset, &filters)
            };

            let (trace_asset, offset_x_mm, offset_y_mm, trace_w_mm, trace_h_mm) = if let Some(cr) =
                crop_rect.filter(|c| c.is_finite())
            {
                let vis_min_x = cr.min_x.max(0.0);
                let vis_min_y = cr.min_y.max(0.0);
                let vis_max_x = cr.max_x.min(w_mm);
                let vis_max_y = cr.max_y.min(h_mm);
                if vis_max_x - vis_min_x <= 1e-6 || vis_max_y - vis_min_y <= 1e-6 {
                    return Err("cropped raster region is empty (outside image bounds)".to_owned());
                }
                let aw = filtered_asset.width;
                let ah = filtered_asset.height;
                if aw < 2 || ah < 2 {
                    return Err("raster resolution too small to trace".to_owned());
                }
                let px_per_mm_x = f64::from(aw) / w_mm;
                let px_per_mm_y = f64::from(ah) / h_mm;
                let mut x0 = (vis_min_x * px_per_mm_x)
                    .floor()
                    .clamp(0.0, (aw - 1) as f64) as u32;
                let mut y0 = (vis_min_y * px_per_mm_y)
                    .floor()
                    .clamp(0.0, (ah - 1) as f64) as u32;
                let mut x1 = (vis_max_x * px_per_mm_x)
                    .ceil()
                    .clamp((x0 + 1) as f64, aw as f64) as u32;
                let mut y1 = (vis_max_y * px_per_mm_y)
                    .ceil()
                    .clamp((y0 + 1) as f64, ah as f64) as u32;
                if x1 - x0 < 2 {
                    if x1 < aw {
                        x1 += 1;
                    } else {
                        x0 = x0.saturating_sub(1);
                    }
                }
                if y1 - y0 < 2 {
                    if y1 < ah {
                        y1 += 1;
                    } else {
                        y0 = y0.saturating_sub(1);
                    }
                }
                let cw = x1 - x0;
                let ch = y1 - y0;
                let filtered_rgba = filtered_asset.to_rgba8_vec();
                let mut cropped_bytes = Vec::with_capacity((cw * ch * 4) as usize);
                for row in y0..y1 {
                    let start = ((row * aw + x0) * 4) as usize;
                    let end = ((row * aw + x1) * 4) as usize;
                    cropped_bytes.extend_from_slice(&filtered_rgba[start..end]);
                }
                let cropped_asset = RasterAsset::new(
                    cw,
                    ch,
                    cropped_bytes,
                    filtered_asset.source_dpi,
                    filtered_asset.color_mode,
                )
                .map_err(|e| e.to_string())?;
                (
                    cropped_asset,
                    f64::from(x0) / px_per_mm_x,
                    f64::from(y0) / px_per_mm_y,
                    f64::from(cw) / px_per_mm_x,
                    f64::from(ch) / px_per_mm_y,
                )
            } else {
                (filtered_asset, 0.0, 0.0, w_mm, h_mm)
            };

            let traced = trace_raster_to_mm(&trace_asset, trace_w_mm, trace_h_mm, &cfg);
            let count = traced.len();
            if count == 0 {
                return Err("trace produced 0 contours (adjust detail or colors)".to_owned());
            }

            let mut next_id = tab.document.next_id().max(1);
            let mut ops = Vec::with_capacity(count + 1);
            let mut created_ids = Vec::with_capacity(count);
            for (idx, (mut path, color)) in traced.into_iter().enumerate() {
                if offset_x_mm.abs() > 1e-9 || offset_y_mm.abs() > 1e-9 {
                    path.transform(&Affine2D::translate(offset_x_mm, offset_y_mm));
                }
                if let Some(p) = persp {
                    path = p.transform_bezier_path(&path);
                }
                let oid = ObjectId(next_id);
                next_id = next_id.saturating_add(1);
                let mut obj = SceneObject::new(
                    oid,
                    format!("Traced Curve {}", idx + 1),
                    NodePayload::Path(path),
                )
                .with_transform(xf);
                if is_centerline {
                    obj.fill = Fill::None;
                    obj.outline = OutlineStyle::solid(color, 0.25);
                } else {
                    obj.fill = Fill::Solid(color);
                    obj.outline = OutlineStyle::none();
                }
                created_ids.push(oid);
                ops.push(CommandOp::create_object(obj, layer_id));
            }
            if created_ids.len() > 1 {
                ops.push(CommandOp::Group {
                    ids: created_ids.clone(),
                    name: "Vector Trace Result".to_owned(),
                });
            }
            (ops, created_ids, count)
        };

        let cmd = if ops.len() == 1 {
            ops.into_iter().next().expect("len == 1")
        } else {
            CommandOp::Batch(ops)
        };
        self.execute_command(cmd)?;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if created_ids.len() > 1 {
                if let Some(gid) = tab.document.parent_container_of(created_ids[0]) {
                    tab.selection.select_single(&tab.document, gid);
                }
            } else if let Some(&single) = created_ids.first() {
                tab.selection.select_single(&tab.document, single);
            }
            tab.needs_rerender = true;
        }
        self.push_toast(format!("Vector Trace created {count} curve(s)"), false);
        Ok(count)
    }

    /// Converts the first selected vector object into an embedded raster bitmap at `dpi`.
    /// Uses `coree_render::convert_object_to_bitmap_with_bounds` so the resulting `RasterLayerData`
    /// shares the exact padded world rect (`4.0 + spatial_bounds_padding_mm + effect_bounds_padding_mm`
    /// plus `Blend`/`Extrude25D` extents) and aligns 1:1 in world space with zero shrink or shift.
    pub fn convert_selection_to_bitmap(&mut self) {
        let Some(layer_id) = self.ensure_active_creation_layer() else {
            return;
        };
        let dpi = self.bitmap_state.convert_dpi.clamp(36.0, 1200.0);
        let plan = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return;
            };
            let Some(&target_id) = tab.selection.objects.first() else {
                self.push_toast("Select an object to Convert to Bitmap", false);
                return;
            };
            match convert_object_to_bitmap_with_bounds(&tab.document, target_id, dpi) {
                Ok((asset, padded)) => {
                    let expected_id = ObjectId(tab.document.next_id().max(1));
                    let aid = asset.id;
                    let raster_obj = SceneObject::new(
                        ObjectId(0),
                        "Rasterized Bitmap",
                        NodePayload::Raster(coree_core::RasterLayerData {
                            asset_id: aid,
                            width_mm: padded.width().max(1.0),
                            height_mm: padded.height().max(1.0),
                            filters: Vec::new(),
                            filter_enabled: Vec::new(),
                            crop_rect: None,
                        }),
                    )
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::none())
                    .with_transform(Affine2D::translate(padded.min_x, padded.min_y));
                    Ok((
                        expected_id,
                        CommandOp::Batch(vec![
                            CommandOp::RegisterAsset { asset },
                            CommandOp::create_object(raster_obj, layer_id),
                        ]),
                    ))
                }
                Err(e) => Err(format!("Convert to Bitmap failed: {e}")),
            }
        };
        match plan {
            Ok((expected_id, cmd)) => match self.execute_command(cmd) {
                Ok(()) => {
                    if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                        if tab.document.has_object(expected_id) {
                            tab.selection.select_single(&tab.document, expected_id);
                        }
                    }
                    self.checkpoint_wal_snapshot_active();
                    self.push_toast(format!("Converted to bitmap at {dpi:.0} DPI"), false);
                }
                Err(e) => self.push_toast(format!("Convert to Bitmap failed: {e}"), true),
            },
            Err(err) => self.push_toast(err, true),
        }
    }

    /// Appends a non-destructive [`RasterFilter`] to the selected raster object.
    pub fn apply_raster_filter_to_selection(&mut self, filter: RasterFilter) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let target = tab.selection.objects.iter().copied().find(|id| {
            tab.document
                .object(*id)
                .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
        });
        let Some(id) = target else {
            self.push_toast("Select a raster bitmap first", false);
            return;
        };
        let mut data = match &tab.document.objects()[&id].payload {
            NodePayload::Raster(r) => r.clone(),
            _ => return,
        };
        data.filters.push(filter);
        if !data.filter_enabled.is_empty() {
            data.filter_enabled.push(true);
        }
        match self.execute_command(CommandOp::UpdateRaster { id, data }) {
            Ok(()) => self.push_toast("Applied non-destructive bitmap filter", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Resamples the selected raster object's underlying bitmap to `(new_w, new_h)`.
    pub fn resample_selected_bitmap(&mut self, new_w: u32, new_h: u32) {
        let algo = self.bitmap_state.resample_algo;
        self.resample_selected_bitmap_with_algorithm(new_w, new_h, algo);
    }

    /// Resamples the selected raster object's underlying bitmap using `algorithm`.
    pub fn resample_selected_bitmap_with_algorithm(
        &mut self,
        new_w: u32,
        new_h: u32,
        algorithm: coree_render::ResampleAlgorithm,
    ) {
        let plan: Result<CommandOp, String> = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return;
            };
            let target = tab.selection.objects.iter().copied().find(|id| {
                tab.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
            });
            let Some(id) = target else {
                self.push_toast("Select a raster bitmap to Resample", false);
                return;
            };
            let mut data = match &tab.document.objects()[&id].payload {
                NodePayload::Raster(r) => r.clone(),
                _ => return,
            };
            let Some(orig) = tab.document.asset(data.asset_id) else {
                return;
            };
            match coree_render::resample_with_algorithm(
                orig,
                new_w.clamp(4, 8192),
                new_h.clamp(4, 8192),
                algorithm,
            ) {
                Ok(mut resized) => {
                    let new_asset_id = coree_core::ImageAssetId::new();
                    resized.id = new_asset_id;
                    data.asset_id = new_asset_id;
                    Ok(CommandOp::Batch(vec![
                        CommandOp::RegisterAsset { asset: resized },
                        CommandOp::UpdateRaster { id, data },
                    ]))
                }
                Err(e) => Err(format!("Resample failed: {e}")),
            }
        };
        match plan {
            Ok(cmd) => match self.execute_command(cmd) {
                Ok(()) => {
                    self.checkpoint_wal_snapshot_active();
                    self.push_toast(format!("Resampled bitmap to {new_w}x{new_h} px"), false);
                }
                Err(e) => self.push_toast(format!("Resample failed: {e}"), true),
            },
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Converts the selected raster bitmap between 8-bit (`Bit8`) and 16-bit (`Bit16`) channels.
    pub fn convert_selected_bitmap_depth(&mut self, depth: coree_core::PixelDepth) {
        let plan = self.active_tab().and_then(|tab| {
            let id = tab.selection.objects.iter().copied().find(|id| {
                tab.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
            })?;
            let NodePayload::Raster(mut data) = tab.document.object(id)?.payload.clone() else {
                return None;
            };
            let orig = tab.document.asset(data.asset_id)?;
            let mut converted = coree_render::convert_raster_depth(orig, depth);
            let new_id = coree_core::ImageAssetId::new();
            converted.id = new_id;
            data.asset_id = new_id;
            Some(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset: converted },
                CommandOp::UpdateRaster { id, data },
            ]))
        });
        let Some(cmd) = plan else {
            self.push_toast("Select a raster bitmap to convert bit depth", false);
            return;
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast(format!("Converted bitmap to {:?}", depth), false);
        }
    }

    /// Converts the selected raster bitmap's color mode (`Rgb` / `Cmyk`) through the active ICC pipeline.
    pub fn convert_selected_bitmap_color_mode(&mut self, mode: ColorMode) {
        let plan = self.active_tab().and_then(|tab| {
            let id = tab.selection.objects.iter().copied().find(|id| {
                tab.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
            })?;
            let NodePayload::Raster(mut data) = tab.document.object(id)?.payload.clone() else {
                return None;
            };
            let orig = tab.document.asset(data.asset_id)?;
            let icc = tab.document.icc_pipeline();
            let mut converted = coree_render::convert_raster_color_mode(orig, mode, Some(&icc));
            let new_id = coree_core::ImageAssetId::new();
            converted.id = new_id;
            data.asset_id = new_id;
            Some(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset: converted },
                CommandOp::UpdateRaster { id, data },
            ]))
        });
        let Some(cmd) = plan else {
            self.push_toast("Select a raster bitmap to convert color mode", false);
            return;
        };
        if self.execute_command(cmd).is_ok() {
            self.push_toast(format!("Converted bitmap to {mode:?}"), false);
        }
    }

    /// Computes a live histogram for the currently selected raster bitmap (resolving `Clone`,
    /// applying active `filters` and `crop_rect`, and caching by content signature).
    #[must_use]
    pub fn selected_bitmap_histogram(&self, bins: usize) -> Option<coree_render::RasterHistogram> {
        let tab = self.active_tab()?;
        let (id, data) = tab.selection.objects.iter().copied().find_map(|oid| {
            let obj = tab.document.object(oid)?;
            if let NodePayload::Raster(ref r) = obj.payload {
                return Some((oid, r.clone()));
            }
            tab.document
                .resolve_clone_payload_and_style_with_sym_scope(obj, None)
                .and_then(|(p, _, _, _)| match p {
                    NodePayload::Raster(r) => Some((oid, r)),
                    _ => None,
                })
        })?;
        let asset = tab.document.asset(data.asset_id)?;

        // Compute FNV-1a cache key covering object, asset, bins, filters, and crop rect
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(0x0100_0000_01b3);
        };
        mix(id.0);
        mix(data.asset_id.0.as_u128() as u64);
        mix((data.asset_id.0.as_u128() >> 64) as u64);
        mix(bins as u64);
        mix(data.filters.len() as u64);
        for b in format!("{:?}{:?}", data.filters, data.filter_enabled).as_bytes() {
            mix(u64::from(*b));
        }
        if let Some(cr) = data.crop_rect {
            mix(1);
            mix(cr.min_x.to_bits());
            mix(cr.min_y.to_bits());
            mix(cr.max_x.to_bits());
            mix(cr.max_y.to_bits());
        } else {
            mix(0);
        }
        if let Some((cached_key, ref cached_hist)) = *self.bitmap_state.histogram_cache.borrow() {
            if cached_key == h {
                return Some(cached_hist.clone());
            }
        }

        let filtered;
        let active_f = data.active_filters();
        let filtered_ref = if active_f.is_empty() {
            asset
        } else {
            filtered = coree_render::apply_raster_filters(asset, &active_f);
            &filtered
        };

        let hist = if let Some(cr) = data.crop_rect.filter(|c| c.is_finite()) {
            let w_mm = data.width_mm.max(1e-6);
            let h_mm = data.height_mm.max(1e-6);
            let aw = filtered_ref.width;
            let ah = filtered_ref.height;
            if aw > 0 && ah > 0 {
                let px_per_mm_x = f64::from(aw) / w_mm;
                let px_per_mm_y = f64::from(ah) / h_mm;
                let x0 = (cr.min_x.max(0.0) * px_per_mm_x)
                    .floor()
                    .clamp(0.0, (aw - 1) as f64) as u32;
                let y0 = (cr.min_y.max(0.0) * px_per_mm_y)
                    .floor()
                    .clamp(0.0, (ah - 1) as f64) as u32;
                let x1 = (cr.max_x.min(w_mm) * px_per_mm_x)
                    .ceil()
                    .clamp((x0 + 1) as f64, aw as f64) as u32;
                let y1 = (cr.max_y.min(h_mm) * px_per_mm_y)
                    .ceil()
                    .clamp((y0 + 1) as f64, ah as f64) as u32;
                let cw = (x1 - x0).max(1);
                let ch = (y1 - y0).max(1);
                let rgba = filtered_ref.to_rgba8_vec();
                let mut cropped_bytes = Vec::with_capacity((cw * ch * 4) as usize);
                for row in y0..y1 {
                    let s = ((row * aw + x0) * 4) as usize;
                    let e = ((row * aw + x1) * 4) as usize;
                    cropped_bytes.extend_from_slice(&rgba[s..e]);
                }
                if let Ok(cropped) = RasterAsset::new(
                    cw,
                    ch,
                    cropped_bytes,
                    filtered_ref.source_dpi,
                    filtered_ref.color_mode,
                ) {
                    coree_render::compute_histogram(&cropped, bins)
                } else {
                    coree_render::compute_histogram(filtered_ref, bins)
                }
            } else {
                coree_render::compute_histogram(filtered_ref, bins)
            }
        } else {
            coree_render::compute_histogram(filtered_ref, bins)
        };
        *self.bitmap_state.histogram_cache.borrow_mut() = Some((h, hist.clone()));
        Some(hist)
    }

    /// Runs Cutout Lab background removal on the selected raster bitmap using the active
    /// selection mask (or automatic corner-seeded foreground extraction when no mask is active).
    pub fn run_cutout_lab_active(&mut self) {
        let feather_px = self.bitmap_state.cutout_feather_px.max(0.0);
        let bg_tol = self.bitmap_state.cutout_bg_threshold.max(1.0);
        let default_feather = self.tool_settings.mask_feather_px;
        let default_inverted = self.tool_settings.mask_inverted;
        let plan: Result<CommandOp, String> = (|| {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let oid = tab
                .selection
                .objects
                .iter()
                .copied()
                .find(|id| {
                    tab.document
                        .object(*id)
                        .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
                })
                .ok_or_else(|| "Select a raster bitmap for Cutout Lab".to_owned())?;
            let obj = tab
                .document
                .object(oid)
                .ok_or_else(|| "raster object missing".to_owned())?;
            let NodePayload::Raster(ref r) = obj.payload else {
                return Err("object is not a raster".to_owned());
            };
            let orig = tab
                .document
                .asset(r.asset_id)
                .ok_or_else(|| "raster asset missing".to_owned())?;
            if orig.width == 0 || orig.height == 0 {
                return Err("raster asset is empty".to_owned());
            }
            let mut new_asset = orig.clone();
            new_asset.id = ImageAssetId::new();
            let mut updated_data = r.clone();
            updated_data.asset_id = new_asset.id;

            let aw = f64::from(new_asset.width);
            let ah = f64::from(new_asset.height);
            let w_mm = r.width_mm.max(1e-3);
            let h_mm = r.height_mm.max(1e-3);
            let inv_xf = world_inverse_transform_of(&tab.document, oid);
            let inv_persp = obj.perspective.and_then(|p| p.inverse());
            let to_px = |p: Point2D| {
                let mut lp = inv_xf.transform_point(p);
                if let Some(ref ih) = inv_persp {
                    lp = ih.transform_point_or(lp);
                }
                Point2D::new((lp.x / w_mm) * aw, (lp.y / h_mm) * ah)
            };

            let fg_mask = Self::build_raster_mask_for_asset(
                &new_asset,
                tab.raster_mask.as_ref(),
                tab.raster_mask_rect_mm,
                &to_px,
                default_feather,
                default_inverted,
            )
            .unwrap_or_else(|| {
                let mut bg = RasterMask::from_magic_wand(&new_asset, 0, 0, bg_tol, true);
                bg.invert();
                bg
            });
            cutout_lab(&mut new_asset, &fg_mask, feather_px);
            Ok(CommandOp::Batch(vec![
                CommandOp::RegisterAsset { asset: new_asset },
                CommandOp::UpdateRaster {
                    id: oid,
                    data: updated_data,
                },
            ]))
        })();

        match plan {
            Ok(cmd) => match self.execute_command(cmd) {
                Ok(()) => {
                    self.checkpoint_wal_snapshot_active();
                    self.push_toast("Applied Cutout Lab alpha matting", false);
                }
                Err(e) => self.push_toast(format!("Cutout Lab failed: {e}"), true),
            },
            Err(err) => self.push_toast(err, true),
        }
    }

    /// Builds a [`FindQuery`] from [`FindReplaceState`].
    #[must_use]
    pub fn build_find_query(&self) -> FindQuery {
        let fr = &self.find_replace_state;
        let (min_w, max_w) = if fr.use_outline_width {
            let lo = fr
                .outline_width_min_mm
                .min(fr.outline_width_max_mm)
                .max(0.0);
            let hi = fr.outline_width_min_mm.max(fr.outline_width_max_mm).max(lo);
            (Some(lo), Some(hi))
        } else {
            (None, None)
        };
        let layer_trim = fr.layer_filter.trim();
        let parsed_layer_id = layer_trim.parse::<u64>().ok().map(LayerId);
        let layer_name =
            (!layer_trim.is_empty() && parsed_layer_id.is_none()).then(|| layer_trim.to_owned());
        FindQuery {
            scope: fr.scope,
            object_kind: fr.object_kind,
            fill_color: fr
                .use_fill_color
                .then(|| coree_core::ColorMatch::exact(fr.find_fill_color.clone())),
            outline_color: fr
                .use_outline_color
                .then(|| coree_core::ColorMatch::exact(fr.find_outline_color.clone())),
            outline_width_min_mm: min_w,
            outline_width_max_mm: max_w,
            is_hairline: fr.filter_hairline,
            font_family: (!fr.font_query.trim().is_empty())
                .then(|| fr.font_query.trim().to_owned()),
            text_pattern: (!fr.text_query.is_empty()).then(|| {
                (
                    fr.text_query.clone(),
                    TextMatchMode::Contains {
                        case_sensitive: fr.case_sensitive,
                    },
                )
            }),
            lens_kind: fr.lens_filter,
            layer_id: parsed_layer_id,
            layer_name,
        }
    }

    /// Finds matching objects and selects them on the active document.
    pub fn run_find_select(&mut self) -> Vec<ObjectId> {
        let query = self.build_find_query();
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let ids = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Vec::new();
            };
            let ids = find_objects(&tab.document, &query);
            tab.selection.select_many(&tab.document, &ids);
            tab.metrics_dirty = true;
            ids
        };
        self.find_replace_state.last_report = Some(FindReplaceReport {
            matched_ids: ids.clone(),
            modified_ids: Vec::new(),
            replacements_applied: 0,
        });
        self.push_toast(format!("Found {} matching object(s)", ids.len()), false);
        self.maybe_fire_selection_change(&before_sel);
        ids
    }

    /// Evaluates the clean-room CQL query in `find_replace_state.cql_query` and selects matches.
    pub fn run_cql_select(&mut self) -> Result<Vec<ObjectId>, String> {
        let scope = self.find_replace_state.scope;
        let query = self.find_replace_state.cql_query.clone();
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let res = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("No active document".to_owned());
            };
            let matched =
                evaluate_cql_query(&tab.document, scope, &query).map_err(|e| e.to_string())?;
            tab.selection.select_many(&tab.document, &matched);
            tab.metrics_dirty = true;
            matched
        };
        self.find_replace_state.last_cql_error = None;
        self.find_replace_state.last_report = Some(FindReplaceReport {
            matched_ids: res.clone(),
            modified_ids: Vec::new(),
            replacements_applied: 0,
        });
        self.push_toast(format!("CQL matched {} object(s)", res.len()), false);
        self.maybe_fire_selection_change(&before_sel);
        Ok(res)
    }

    /// Runs `coree_prod::find_and_replace` via [`CommandOp::FindAndReplace`] with explicit `action`.
    pub fn run_find_and_replace_action(&mut self, action: ReplaceAction) -> FindReplaceReport {
        let query = self.build_find_query();
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let rep = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return FindReplaceReport::default();
            };
            if matches!(action, ReplaceAction::SelectMatches) {
                let matched_ids = find_objects(&tab.document, &query);
                FindReplaceReport {
                    matched_ids,
                    modified_ids: Vec::new(),
                    replacements_applied: 0,
                }
            } else {
                coree_core::preview_find_and_replace(&tab.document, &query, &action)
            }
        };
        if matches!(action, ReplaceAction::SelectMatches) || rep.modified_ids.is_empty() {
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if !rep.matched_ids.is_empty() {
                    tab.selection.select_many(&tab.document, &rep.matched_ids);
                    tab.metrics_dirty = true;
                }
            }
        } else {
            if let Err(err) = self.execute_command(CommandOp::FindAndReplace { query, action }) {
                self.push_toast(format!("Find & Replace failed: {err}"), true);
                return FindReplaceReport::default();
            }
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if !rep.matched_ids.is_empty() {
                    tab.selection.select_many(&tab.document, &rep.matched_ids);
                    tab.metrics_dirty = true;
                }
            }
        }
        self.find_replace_state.last_report = Some(rep.clone());
        self.push_toast(
            format!(
                "Find & Replace: {} matched, {} updated",
                rep.matched_ids.len(),
                rep.replacements_applied
            ),
            false,
        );
        self.maybe_fire_selection_change(&before_sel);
        rep
    }

    /// Starts recording subsequent commands into a Rhai macro.
    pub fn start_macro_recording(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.history.start_macro_recording();
            self.push_toast("Recording Rhai Macro...", false);
        }
    }

    /// Stops macro recording and populates the Macro Docker editor with the generated Rhai script.
    pub fn stop_macro_recording(&mut self) -> String {
        let script = self
            .active_tab_mut()
            .map(|tab| tab.history.stop_macro_to_rhai())
            .unwrap_or_default();
        if !script.is_empty() {
            self.macro_state.script_source = script.clone();
            self.focus_docker(DockerTab::Macros);
            self.push_toast("Macro recorded to Rhai script", false);
        }
        script
    }

    /// Executes `script` inside `coree_prod::macros` strict sandbox against the active document.
    pub fn run_rhai_script(&mut self, script: &str) -> Result<usize, String> {
        let planned = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let cfg = MacroSandboxConfig::default();
            if script.contains("// coree recorded macro") {
                plan_recorded_macro_ops(script, &cfg)
            } else {
                plan_macro_ops_with_selection(
                    &tab.document,
                    script,
                    &BTreeMap::new(),
                    &cfg,
                    &tab.selection.objects,
                )
            }
        };
        match planned {
            Ok((exec, ops)) => {
                if !ops.is_empty() {
                    let cmd = if ops.len() == 1 {
                        ops.into_iter().next().expect("len == 1")
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(err_str) = self.execute_command(cmd) {
                        self.macro_state.last_error = Some(err_str.clone());
                        self.push_toast(format!("Macro error: {err_str}"), true);
                        return Err(err_str);
                    }
                    if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                        tab.sync_active_layer();
                        tab.needs_rerender = true;
                    }
                    self.checkpoint_wal_snapshot_active();
                }
                self.macro_state.output_logs = exec.logs;
                self.macro_state.last_error = None;
                self.push_toast(
                    format!("Macro ran ({} command(s))", exec.commands_executed),
                    false,
                );
                Ok(exec.commands_executed)
            }
            Err(e) => {
                let err_str = e.to_string();
                self.macro_state.last_error = Some(err_str.clone());
                self.push_toast(format!("Macro error: {err_str}"), true);
                Err(err_str)
            }
        }
    }

    /// Scans system fonts into SQLite `font_index` and refreshes the Font Manager list.
    pub fn refresh_font_manager(&mut self, rescan: bool) {
        let Some(ref db) = self.storage else {
            return;
        };
        if rescan || !self.font_manager_state.scanned_once {
            if let Ok(rep) = db.scan_system_fonts() {
                self.font_manager_state.quarantined_files = rep.quarantined_files;
            }
            self.font_manager_state.scanned_once = true;
        }
        let query = FontQuery {
            family_query: (!self.font_manager_state.search_query.trim().is_empty())
                .then(|| self.font_manager_state.search_query.trim().to_owned()),
            arabic_only: self.font_manager_state.arabic_only,
            kashida_only: self.font_manager_state.kashida_only,
            favorites_only: self.font_manager_state.favorites_only,
            collection_tag: self.font_manager_state.active_collection_tag.clone(),
            limit: Some(200),
        };
        if let Ok(fonts) = db.search_fonts(&query) {
            self.font_manager_state.indexed_fonts = fonts;
        }
        if let Ok(subs) = db.list_font_substitutions() {
            self.font_manager_state.substitutions = subs;
        }
    }

    /// Toggles the favorite flag on an indexed font face in SQLite and refreshes the list.
    pub fn toggle_font_favorite(&mut self, font_id: &str, favorite: bool) -> bool {
        let updated = self
            .storage
            .as_ref()
            .and_then(|db| db.set_font_favorite(font_id, favorite).ok())
            .unwrap_or(false);
        if updated {
            self.refresh_font_manager(false);
        }
        updated
    }

    /// Finds the closest PANOSE perceptual substitute in `font_index` and caches it in `FontManagerState`.
    pub fn find_best_panose_substitute(
        &mut self,
        target_panose: &PanoseClassification,
        target_weight: u16,
        target_italic: bool,
        require_arabic: bool,
    ) -> Option<(FontRecord, u32)> {
        let best = self
            .storage
            .as_ref()
            .and_then(|db| {
                db.find_best_panose_substitute(
                    target_panose,
                    target_weight,
                    target_italic,
                    require_arabic,
                )
                .ok()
            })
            .flatten();
        if let Some((ref rec, _)) = best {
            self.font_manager_state.substitute_family_input = rec.family_name.clone();
        }
        self.font_manager_state.panose_suggestion = best.clone();
        best
    }

    /// Persists a missing-font substitution rule in SQLite `font_substitutions` and refreshes state.
    pub fn set_font_substitution(
        &mut self,
        missing_family: &str,
        substitute_family: &str,
        panose: Option<&PanoseClassification>,
    ) -> Result<(), String> {
        let Some(ref db) = self.storage else {
            return Err("No SQLite storage database open".into());
        };
        db.set_font_substitution(missing_family, substitute_family, panose)
            .map_err(|e| e.to_string())?;
        self.refresh_font_manager(false);
        self.push_toast(
            format!(
                "Saved font substitution '{}' -> '{}'",
                missing_family.trim(),
                substitute_family.trim()
            ),
            false,
        );
        Ok(())
    }

    /// Lists all persisted font substitution rules from SQLite `font_substitutions`.
    #[must_use]
    pub fn list_font_substitutions(&self) -> Vec<FontSubstitutionRecord> {
        self.storage
            .as_ref()
            .and_then(|db| db.list_font_substitutions().ok())
            .unwrap_or_default()
    }

    /// Deletes a persisted font substitution rule and refreshes `FontManagerState`.
    pub fn remove_font_substitution(&mut self, missing_family: &str) -> bool {
        let removed = self
            .storage
            .as_ref()
            .and_then(|db| db.remove_font_substitution(missing_family).ok())
            .unwrap_or(false);
        if removed {
            self.refresh_font_manager(false);
        }
        removed
    }

    /// Replaces `missing_family` (case-insensitive) with `substitute_family` across text objects
    /// in the active document via undoable, WAL-journaled `CommandOp::EditText` operations.
    pub fn apply_font_substitution_to_active_document(
        &mut self,
        missing_family: &str,
        substitute_family: &str,
    ) -> usize {
        let missing = missing_family.trim();
        let substitute = substitute_family.trim();
        if missing.is_empty() || substitute.is_empty() {
            return 0;
        }
        let ops: Vec<CommandOp> = self
            .active_tab()
            .map(|tab| {
                let mut collected = Vec::new();
                for (&oid, obj) in tab.document.objects() {
                    let mut payload = obj.payload.clone();
                    let mut changed = false;
                    match &mut payload {
                        NodePayload::ArtisticText(t) => {
                            for run in &mut t.runs {
                                if run.style.font_family.trim().eq_ignore_ascii_case(missing) {
                                    run.style.font_family = substitute.to_owned();
                                    changed = true;
                                }
                            }
                        }
                        NodePayload::ParagraphText(p) => {
                            for run in &mut p.runs {
                                if run.style.font_family.trim().eq_ignore_ascii_case(missing) {
                                    run.style.font_family = substitute.to_owned();
                                    changed = true;
                                }
                            }
                        }
                        NodePayload::TextOnPath(top) => {
                            for run in &mut top.text.runs {
                                if run.style.font_family.trim().eq_ignore_ascii_case(missing) {
                                    run.style.font_family = substitute.to_owned();
                                    changed = true;
                                }
                            }
                        }
                        _ => {}
                    }
                    if changed {
                        collected.push(CommandOp::EditText { id: oid, payload });
                    }
                }
                collected
            })
            .unwrap_or_default();
        let count = ops.len();
        if count == 1 {
            if let Some(op) = ops.into_iter().next() {
                let _ = self.execute_command(op);
            }
        } else if count > 1 {
            let _ = self.execute_command(CommandOp::Batch(ops));
        }
        if count > 0 {
            self.push_toast(
                format!("Substituted '{missing}' -> '{substitute}' in {count} text object(s)"),
                false,
            );
        }
        count
    }

    /// Imports a `.gpl`, `.ase`, or `.pal` palette file into `custom_palettes`.
    pub fn import_palette_from_path(&mut self, path: &Path) -> Result<String, String> {
        let pal = import_palette_file(path).map_err(|e| format!("palette import failed: {e}"))?;
        let name = pal.name.clone();
        self.custom_palettes.push(pal);
        self.active_palette_index = 3 + self.custom_palettes.len();
        self.push_toast(format!("Imported palette '{name}'"), false);
        Ok(name)
    }

    // -------------------------------------------------------------------------
    // Tool Switching, Dockers, Theme, Shortcuts & Command Palette
    // -------------------------------------------------------------------------

    /// Activates `tool` and updates its parent flyout group's remembered primary button.
    pub fn select_tool(&mut self, tool: ToolId) {
        if (tool != ToolId::Pick && tool != ToolId::Zoom)
            || (tool == ToolId::Zoom && self.one_shot_zoom_return_tool.is_none())
        {
            self.previous_non_pick_tool = tool;
        }
        if tool != ToolId::Zoom {
            self.one_shot_zoom_return_tool = None;
        }
        // Sync raster sub-mode cleanly when switching between raster toolbox buttons
        match tool {
            ToolId::BitmapEraser => self.tool_settings.raster_op = RasterBrushOp::Eraser,
            ToolId::CloneStamp => self.tool_settings.raster_op = RasterBrushOp::CloneStamp,
            ToolId::Smudge => self.tool_settings.raster_op = RasterBrushOp::Smudge,
            ToolId::BitmapBrush
                if matches!(
                    self.tool_settings.raster_op,
                    RasterBrushOp::Eraser | RasterBrushOp::CloneStamp | RasterBrushOp::Smudge
                ) =>
            {
                self.tool_settings.raster_op = RasterBrushOp::Paint;
            }
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                // Start fresh in Sample mode whenever the user picks an Eyedropper tool
                self.tool_settings.eyedropper_apply_mode = false;
            }
            _ => {}
        }
        let g = tool.group_index();
        self.group_active_tools[g] = tool;
        self.active_tool = tool;
        self.open_flyout_group = None;
        if let Some(tab) = self.active_tab_mut() {
            tab.tool_core.set_active_tool(tool);
            tab.transient_overlay = TransientOverlay::default();
            if tool != ToolId::Crop {
                tab.pending_crop_rect = None;
            }
        }
    }

    /// Selects a specific [`RasterBrushOp`] (Paint, Eraser, CloneStamp, Smudge, Healing, Dodge, Burn, FloodFill)
    /// and activates the corresponding toolbox tool.
    pub fn set_raster_brush_op(&mut self, op: RasterBrushOp) {
        let target_tool = match op {
            RasterBrushOp::Eraser => ToolId::BitmapEraser,
            RasterBrushOp::CloneStamp => ToolId::CloneStamp,
            RasterBrushOp::Smudge => ToolId::Smudge,
            RasterBrushOp::Paint
            | RasterBrushOp::Healing
            | RasterBrushOp::Dodge
            | RasterBrushOp::Burn
            | RasterBrushOp::FloodFill => ToolId::BitmapBrush,
        };
        self.select_tool(target_tool);
        self.tool_settings.raster_op = op;
    }

    /// Activates one-shot Zoom (`F2`): performs a single zoom click/marquee and returns to the previous tool.
    pub fn activate_one_shot_zoom(&mut self) {
        if self.active_tool != ToolId::Zoom {
            self.one_shot_zoom_return_tool = Some(self.active_tool);
        }
        self.active_tool = ToolId::Zoom;
        self.group_active_tools[ToolId::Zoom.group_index()] = ToolId::Zoom;
        self.open_flyout_group = None;
        if let Some(tab) = self.active_tab_mut() {
            tab.tool_core.set_active_tool(ToolId::Zoom);
            tab.transient_overlay = TransientOverlay::default();
        }
    }

    /// Completes a one-shot `F2` zoom operation by restoring the previous tool if armed.
    pub fn complete_one_shot_zoom_if_active(&mut self) {
        if let Some(return_tool) = self.one_shot_zoom_return_tool.take() {
            self.select_tool(return_tool);
        }
    }

    /// Zooms viewport tightly to the currently selected object(s) (`Shift+F2`), falling back to Fit All.
    pub fn zoom_to_selected(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if let Some(bb) = tab.selection.bounding_box(&tab.document) {
                tab.viewport.fit_rect(bb.expand(4.0), 40.0);
                tab.needs_rerender = true;
            } else {
                tab.fit_all_objects();
            }
        }
    }

    /// Applies a built-in [`WorkspacePreset`] layout, configuring dockers and toolbox mode.
    pub fn apply_workspace_preset(&mut self, preset: WorkspacePreset) {
        self.active_workspace_preset = preset;
        match preset {
            WorkspacePreset::DefaultClassic => {
                self.dock_state = default_dock_state();
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::Lite => {
                self.dock_state = DockState::new(vec![DockerTab::Properties, DockerTab::Objects]);
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::Illustration => {
                let mut state = DockState::new(vec![
                    DockerTab::Properties,
                    DockerTab::Effects,
                    DockerTab::Lens,
                    DockerTab::SymbolsStyles,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.58,
                    vec![
                        DockerTab::Objects,
                        DockerTab::History,
                        DockerTab::BitmapStudio,
                    ],
                );
                self.dock_state = state;
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::Illustrator => {
                let mut state = DockState::new(vec![
                    DockerTab::Properties,
                    DockerTab::Objects,
                    DockerTab::Effects,
                    DockerTab::SymbolsStyles,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.56,
                    vec![DockerTab::TransformAlignShape, DockerTab::History],
                );
                self.dock_state = state;
                self.toolbox_two_column = true;
                self.show_dockers = true;
                self.set_shortcut_profile(ShortcutProfile::StandardVector);
            }
            WorkspacePreset::PageLayout => {
                let mut state = DockState::new(vec![
                    DockerTab::Objects,
                    DockerTab::Properties,
                    DockerTab::FontManager,
                    DockerTab::FindReplace,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.55,
                    vec![DockerTab::Production, DockerTab::History],
                );
                self.dock_state = state;
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::LaserSignStudio => {
                let mut state = DockState::new(vec![
                    DockerTab::Production,
                    DockerTab::Objects,
                    DockerTab::Properties,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.56,
                    vec![
                        DockerTab::TransformAlignShape,
                        DockerTab::CdrInspector,
                        DockerTab::Macros,
                    ],
                );
                self.dock_state = state;
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::TouchCompact => {
                self.dock_state = DockState::new(vec![
                    DockerTab::Properties,
                    DockerTab::Objects,
                    DockerTab::Production,
                ]);
                self.toolbox_two_column = true;
                self.show_dockers = true;
            }
        }
        self.push_toast(format!("Workspace: {}", preset.label()), false);
    }

    /// Exports the active workspace configuration to `.coreews` JSON (v2 includes full
    /// dock split tree, language, active flyout group tools, and custom shortcuts).
    #[must_use]
    pub fn export_workspace_json(&self) -> String {
        let mut open_dockers = Vec::new();
        for (_, &tab) in self.dock_state.iter_all_tabs() {
            if !open_dockers.contains(&tab) {
                open_dockers.push(tab);
            }
        }
        let bundle = WorkspaceBundle {
            version: 2,
            preset: self.active_workspace_preset,
            theme: self.theme,
            shortcut_profile: self.shortcut_profile,
            toolbox_two_column: self.toolbox_two_column,
            show_dockers: self.show_dockers,
            palette_show_names: self.palette_show_names,
            treat_all_objects_as_filled: self.treat_all_objects_as_filled,
            object_origin_anchor: self.object_origin_anchor,
            open_dockers,
            ui_language: self.ui_language,
            dock_state: Some(sanitize_dock_state_for_serde(&self.dock_state)),
            group_active_tools: Some(self.group_active_tools),
            custom_shortcuts: self.custom_shortcuts.clone(),
            toolbar_config: self.toolbar_config.clone(),
        };
        serde_json::to_string_pretty(&bundle).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Imports and applies a `.coreews` workspace JSON configuration (both v1 and v2).
    pub fn import_workspace_json(&mut self, json: &str) -> Result<(), String> {
        if json.len() > 2 * 1024 * 1024 {
            return Err("workspace file exceeds 2 MiB safety limit".to_owned());
        }
        let bundle: WorkspaceBundle =
            serde_json::from_str(json).map_err(|e| format!("invalid workspace JSON: {e}"))?;
        if bundle.version == 0 || bundle.version > 2 {
            return Err(format!("unsupported workspace version {}", bundle.version));
        }
        self.apply_workspace_preset(bundle.preset);
        if let Some(dock_state) = bundle.dock_state {
            self.dock_state = dock_state;
        } else {
            for &docker in &bundle.open_dockers {
                if self.dock_state.find_tab(&docker).is_none() {
                    self.dock_state.push_to_first_leaf(docker);
                }
            }
        }
        self.toolbox_two_column = bundle.toolbox_two_column;
        self.show_dockers = bundle.show_dockers;
        self.palette_show_names = bundle.palette_show_names;
        self.treat_all_objects_as_filled = bundle.treat_all_objects_as_filled;
        self.object_origin_anchor = (
            bundle.object_origin_anchor.0.min(2),
            bundle.object_origin_anchor.1.min(2),
        );
        if let Some(tools) = bundle.group_active_tools {
            self.group_active_tools = tools;
        }
        self.toolbar_config = bundle.toolbar_config;
        self.custom_shortcuts = bundle.custom_shortcuts;
        self.set_theme(bundle.theme);
        self.set_ui_language(bundle.ui_language);
        self.set_shortcut_profile(bundle.shortcut_profile);
        self.push_toast(
            format!(
                "Imported workspace '{}'",
                self.active_workspace_preset.label()
            ),
            false,
        );
        Ok(())
    }

    fn apply_printer_scan_result(&mut self, res: Result<Vec<PrinterInfo>, String>) {
        match res {
            Ok(printers) => {
                if self.print_job_config.printer_name.is_none() {
                    if let Some(def) = printers.iter().find(|p| p.is_default).or(printers.first()) {
                        self.print_job_config.printer_name = Some(def.name.clone());
                    }
                }
                let count = printers.len();
                self.available_printers = printers;
                self.printers_status = Some(if count == 0 {
                    "No OS printers detected — install CUPS / system printer or use PDF export."
                        .to_owned()
                } else {
                    format!("Detected {count} OS print queue(s)")
                });
            }
            Err(err) => {
                self.available_printers.clear();
                self.printers_status = Some(err);
            }
        }
    }

    /// Refreshes available OS print queues synchronously (headless tests / CLI callers).
    pub fn refresh_system_printers(&mut self) {
        self.apply_printer_scan_result(list_system_printers());
    }

    /// Triggers background OS print queue discovery so opening Print Preview never blocks the UI thread.
    pub fn refresh_system_printers_async(&mut self) {
        if self.scanning_printers {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.scanning_printers = true;
        self.printer_scan_rx = Some(rx);
        self.printers_status = Some("Scanning OS print queues...".to_owned());
        if let Err(err) = std::thread::Builder::new()
            .name("coree-printer-scan".into())
            .spawn(move || {
                let _ = tx.send(list_system_printers());
            })
        {
            self.scanning_printers = false;
            self.printer_scan_rx = None;
            self.printers_status = Some(format!("Failed to start printer scan: {err}"));
        }
    }

    /// Polls any in-flight background OS printer discovery without blocking.
    pub fn poll_system_printers_async(&mut self) {
        let Some(ref rx) = self.printer_scan_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(res) => {
                self.printer_scan_rx = None;
                self.scanning_printers = false;
                self.apply_printer_scan_result(res);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.printer_scan_rx = None;
                self.scanning_printers = false;
                self.printers_status =
                    Some("Printer discovery thread stopped unexpectedly.".into());
            }
        }
    }

    /// Submits the active document to the configured native OS printer spooler (`Ctrl+P`).
    pub fn print_active_document_os(&mut self) {
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            self.push_toast("No active document to print", true);
            return;
        };
        let cfg = self.print_job_config.clone();
        match print_document_via_os_spooler(&doc, &cfg) {
            Ok(msg) => {
                self.printers_status = Some(msg.clone());
                self.push_toast(msg, false);
            }
            Err(err) => {
                self.printers_status = Some(err.clone());
                self.push_toast(format!("Print failed: {err}"), true);
            }
        }
    }

    /// Syncs soft-proofing and gamut-warning flags into every open tab's `RenderOptions`.
    pub fn sync_proof_options_to_tabs(&mut self) {
        for tab in &mut self.tabs {
            if tab.render_options.soft_proof_cmyk != self.soft_proof_cmyk
                || tab.render_options.gamut_warning != self.gamut_warning
                || tab.render_options.proof_profile != self.proof_profile
                || tab.render_options.proof_intent != self.proof_intent
            {
                tab.render_options.soft_proof_cmyk = self.soft_proof_cmyk;
                tab.render_options.gamut_warning = self.gamut_warning;
                tab.render_options.proof_profile = self.proof_profile;
                tab.render_options.proof_intent = self.proof_intent;
                tab.tile_cache.mark_all_dirty();
                tab.needs_rerender = true;
            }
        }
    }

    /// Resizes the active page tightly around its visible content with `margin_mm` padding
    /// via real `TransformObjects` + `SetPageProperties` `CommandOp`s (100% undoable & WAL-journaled).
    pub fn autofit_active_page(&mut self, margin_mm: f64) {
        let plan: Result<(Vec<CommandOp>, f64, f64), String> = (|| {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let doc = &tab.document;
            let page_idx = doc.active_page_index();
            let page = doc
                .page(page_idx)
                .ok_or_else(|| "active page out of range".to_owned())?;
            let page_id = page.id;
            let margin = if margin_mm.is_finite() {
                margin_mm.clamp(0.0, 5_000.0)
            } else {
                0.0
            };

            let mut visible_ids = Vec::new();
            let mut all_page_obj_ids = Vec::new();
            let mut seen = HashSet::new();
            for layer in &page.layers {
                for &oid in &layer.objects {
                    if seen.insert(oid) {
                        all_page_obj_ids.push(oid);
                        if layer.visible {
                            if let Some(obj) = doc.object(oid) {
                                if obj.visible {
                                    visible_ids.push(oid);
                                }
                            }
                        }
                    }
                }
            }

            let bounds = doc
                .combined_bounds(&visible_ids)
                .ok_or_else(|| "page has no visible content to autofit".to_owned())?;
            if !bounds.is_finite() {
                return Err("visible content bounds are non-finite".to_owned());
            }

            let dx = margin - bounds.min_x;
            let dy = margin - bounds.min_y;
            let new_w = (bounds.width() + 2.0 * margin).clamp(1.0, 50_000.0);
            let new_h = (bounds.height() + 2.0 * margin).clamp(1.0, 50_000.0);

            let mut ops = Vec::new();
            if !all_page_obj_ids.is_empty() && (dx.abs() > 1e-9 || dy.abs() > 1e-9) {
                ops.push(CommandOp::TransformObjects {
                    ids: all_page_obj_ids,
                    affine: Affine2D::translate(dx, dy),
                });
            }
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
            Ok((ops, new_w, new_h))
        })();

        match plan {
            Ok((ops, w, h)) => {
                let cmd = if ops.len() == 1 {
                    ops.into_iter().next().expect("len == 1")
                } else {
                    CommandOp::Batch(ops)
                };
                if let Err(err) = self.execute_command(cmd) {
                    self.push_toast(format!("Autofit Page failed: {err}"), true);
                    return;
                }
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.fit_active_page();
                }
                self.checkpoint_wal_snapshot_active();
                self.push_toast(
                    format!("Autofit page to {w:.1} × {h:.1} mm (margin {margin_mm:.1} mm)"),
                    false,
                );
            }
            Err(err) => {
                if err != "no active document" {
                    self.push_toast(format!("Autofit Page failed: {err}"), true);
                }
            }
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

    /// Inserts `glyph` into the active inline text editor, the selected text object, or a new Artistic Text object.
    pub fn insert_glyph_at_selection(&mut self, glyph: &str) {
        if glyph.is_empty() {
            return;
        }
        // 1. Active inline on-canvas text edit buffer
        if let Some(tab) = self.active_tab_mut() {
            if let Some(ref mut edit) = tab.inline_text_edit {
                edit.buffer.push_str(glyph);
                return;
            }
        }
        // 2. Append into selected ArtisticText / ParagraphText / TextOnPath
        let edit_op = self.active_tab().and_then(|tab| {
            let oid = tab.selection.objects.iter().copied().find(|id| {
                tab.document.object(*id).is_some_and(|o| {
                    matches!(
                        o.payload,
                        NodePayload::ArtisticText(_)
                            | NodePayload::ParagraphText(_)
                            | NodePayload::TextOnPath(_)
                    )
                })
            })?;
            let obj = tab.document.object(oid)?;
            let mut payload = obj.payload.clone();
            match &mut payload {
                NodePayload::ArtisticText(art) => {
                    if let Some(last) = art.runs.last_mut() {
                        last.text.push_str(glyph);
                    } else {
                        art.runs.push(coree_core::TextRun::new(
                            glyph.to_owned(),
                            self.default_text_style.clone(),
                        ));
                    }
                }
                NodePayload::ParagraphText(para) => {
                    if let Some(last) = para.runs.last_mut() {
                        last.text.push_str(glyph);
                    } else {
                        para.runs.push(coree_core::TextRun::new(
                            glyph.to_owned(),
                            self.default_text_style.clone(),
                        ));
                    }
                }
                NodePayload::TextOnPath(top) => {
                    if let Some(last) = top.text.runs.last_mut() {
                        last.text.push_str(glyph);
                    } else {
                        top.text.runs.push(coree_core::TextRun::new(
                            glyph.to_owned(),
                            self.default_text_style.clone(),
                        ));
                    }
                }
                _ => return None,
            }
            Some(CommandOp::EditText { id: oid, payload })
        });
        if let Some(op) = edit_op {
            match self.execute_command(op) {
                Ok(()) => self.push_toast(format!("Inserted glyph '{glyph}'"), false),
                Err(e) => self.push_toast(e, true),
            }
            return;
        }
        // 3. Otherwise create a new ArtisticText object at page center
        let Some(_layer_id) = self.ensure_active_creation_layer() else {
            return;
        };
        let pos = self
            .active_tab()
            .and_then(|t| t.document.active_page())
            .map_or(Point2D::new(80.0, 80.0), |p| p.trim_rect().center());
        let art = ArtisticText::from_plain(glyph.to_owned(), self.default_text_style.clone());
        let obj = SceneObject::new(ObjectId(0), "Glyph", NodePayload::ArtisticText(art))
            .with_fill(Fill::Solid(self.default_text_style.fill.clone()))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(pos.x, pos.y));
        if self.create_object_on_active_layer(obj).is_some() {
            self.push_toast(format!("Inserted glyph '{glyph}'"), false);
        }
    }

    /// Classic Studio `Space` behavior: toggles cleanly between `Pick` and the previous active tool.
    pub fn toggle_pick_space(&mut self) {
        if self.active_tool == ToolId::Pick {
            let prev = self.previous_non_pick_tool;
            self.select_tool(prev);
        } else {
            self.select_tool(ToolId::Pick);
        }
    }

    /// Focuses `docker` in `egui_dock` (adding it to the main surface if it was closed).
    pub fn focus_docker(&mut self, docker: DockerTab) {
        self.show_dockers = true;
        if let Some(tab_path) = self.dock_state.find_tab(&docker) {
            let _ = self.dock_state.set_active_tab(tab_path);
        } else {
            self.dock_state.push_to_first_leaf(docker);
        }
    }

    /// Toggles `docker` open/closed inside `egui_dock`.
    pub fn toggle_docker(&mut self, docker: DockerTab) {
        if let Some(tab_path) = self.dock_state.find_tab(&docker) {
            self.dock_state.remove_tab(tab_path);
        } else {
            self.show_dockers = true;
            if self.dock_state.main_surface().is_empty() {
                self.dock_state = DockState::new(vec![docker]);
            } else {
                self.dock_state.set_focused_node_and_surface(NodePath {
                    surface: SurfaceIndex::main(),
                    node: NodeIndex::root(),
                });
                self.dock_state.push_to_focused_leaf(docker);
            }
        }
    }

    /// Switches the active studio theme and updates every open tab's pasteboard background.
    pub fn set_theme(&mut self, theme: StudioTheme) {
        self.theme = theme;
        self.theme_dirty = true;
        let bg = theme.tokens().canvas_well_core_color();
        for tab in &mut self.tabs {
            tab.render_options.canvas_bg = bg.clone();
            tab.needs_rerender = true;
            tab.tile_cache.mark_all_dirty();
        }
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.theme", "ui", &theme);
        }
    }

    /// Switches the active keyboard shortcut profile (`ClassicStudio` / `StandardVector`).
    pub fn set_shortcut_profile(&mut self, profile: ShortcutProfile) {
        self.shortcut_profile = profile;
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.shortcut_profile", "ui", &profile);
        }
        self.push_toast(format!("Shortcut profile: {}", profile.label()), false);
    }

    /// Switches UI studio language (`English` / `العربية`) and persists locally.
    pub fn set_ui_language(&mut self, language: StudioLanguage) {
        if self.ui_language != language {
            self.fonts_configured = false;
        }
        self.ui_language = language;
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.language", "ui", &language);
        }
        self.push_toast(
            match language {
                StudioLanguage::English => "UI Language: English",
                StudioLanguage::Arabic => "لغة الواجهة: العربية",
            },
            false,
        );
    }

    /// Opens the classic modal Edit Fill dialog (`F11`) preloaded from the active selection.
    pub fn open_edit_fill_dialog(&mut self) {
        let initial = self
            .active_tab()
            .and_then(|t| {
                let id = t.selection.objects.first()?;
                t.document.object(*id).map(|o| o.fill.clone())
            })
            .unwrap_or_else(|| self.default_fill.clone());
        self.edit_fill_draft = initial;
        self.edit_fill_selected_stop = 0;
        self.show_edit_fill_dialog = true;
    }

    /// Applies the edited fill draft from `Edit Fill (F11)` onto selected objects or default fill.
    pub fn apply_edit_fill_draft(&mut self) {
        let fill = self.edit_fill_draft.clone();
        self.apply_fill(fill);
    }

    /// Opens the classic modal Outline Pen dialog (`F12`) preloaded from the active selection.
    pub fn open_outline_pen_dialog(&mut self) {
        let initial = self
            .active_tab()
            .and_then(|t| {
                let id = t.selection.objects.first()?;
                t.document.object(*id).map(|o| o.outline.clone())
            })
            .unwrap_or_else(|| self.default_outline.clone());
        self.outline_pen_draft = initial;
        self.show_outline_pen_dialog = true;
    }

    /// Applies the edited outline draft from `Outline Pen (F12)` onto selected objects or default outline.
    pub fn apply_outline_pen_draft(&mut self) {
        let outline = self.outline_pen_draft.clone();
        self.apply_outline(outline);
    }

    /// Executes a resolved [`ShortcutAction`] cleanly (used both by live egui input and headless tests).
    pub fn handle_shortcut(&mut self, action: ShortcutAction) {
        match action {
            ShortcutAction::TogglePickSpace => self.toggle_pick_space(),
            ShortcutAction::SelectPick => self.select_tool(ToolId::Pick),
            ShortcutAction::SelectShapeNode => self.select_tool(ToolId::ShapeNode),
            ShortcutAction::SelectZoom => self.select_tool(ToolId::Zoom),
            ShortcutAction::SelectOneShotZoom => self.activate_one_shot_zoom(),
            ShortcutAction::SelectPan => self.select_tool(ToolId::Pan),
            ShortcutAction::SelectFreehand => self.select_tool(ToolId::Freehand),
            ShortcutAction::SelectPen => self.select_tool(ToolId::Pen),
            ShortcutAction::SelectRectangle => self.select_tool(ToolId::Rectangle),
            ShortcutAction::SelectEllipse => self.select_tool(ToolId::Ellipse),
            ShortcutAction::SelectPolygon => self.select_tool(ToolId::Polygon),
            ShortcutAction::SelectSpiral => self.select_tool(ToolId::Spiral),
            ShortcutAction::SelectGraphPaper => self.select_tool(ToolId::GraphPaper),
            ShortcutAction::SelectArtisticText => self.select_tool(ToolId::ArtisticText),
            ShortcutAction::SelectInteractiveFill => self.select_tool(ToolId::InteractiveFill),
            ShortcutAction::SelectMeshFill => self.select_tool(ToolId::MeshFill),
            ShortcutAction::SelectEyedropper => self.select_tool(ToolId::ColorEyedropper),
            ShortcutAction::SelectTool(tool) => self.select_tool(tool),
            ShortcutAction::OpenEditFill => self.open_edit_fill_dialog(),
            ShortcutAction::FocusOutlinePen => self.open_outline_pen_dialog(),
            ShortcutAction::ZoomIn => self.zoom_step(1.25),
            ShortcutAction::ZoomOut => self.zoom_step(0.8),
            ShortcutAction::ZoomToAll => {
                if let Some(t) = self.active_tab_mut() {
                    t.fit_all_objects();
                }
            }
            ShortcutAction::ZoomToPage => {
                if let Some(t) = self.active_tab_mut() {
                    t.fit_active_page();
                }
            }
            ShortcutAction::ZoomToSelected => self.zoom_to_selected(),
            ShortcutAction::ZoomActual => {
                if let Some(t) = self.active_tab_mut() {
                    t.viewport.zoom = 1.0;
                    t.needs_rerender = true;
                }
            }
            ShortcutAction::ToggleFullScreenPreview => {
                self.full_screen_preview = !self.full_screen_preview;
                if self.full_screen_preview {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.cancel_active_drag();
                    }
                }
                self.push_toast(
                    if self.full_screen_preview {
                        "Full-Screen Preview (press Esc or F9 to exit)"
                    } else {
                        "Exited Full-Screen Preview"
                    },
                    false,
                );
            }
            ShortcutAction::PrevPage => {
                if let Some(t) = self.active_tab_mut() {
                    let cur = t.document.active_page_index();
                    if cur > 0 {
                        let _ = t.document.set_active_page_index(cur - 1);
                        t.selection.clear();
                        t.sync_active_layer();
                        t.needs_rerender = true;
                    }
                }
            }
            ShortcutAction::NextPage => {
                if let Some(t) = self.active_tab_mut() {
                    let cur = t.document.active_page_index();
                    if cur + 1 < t.document.pages().len() {
                        let _ = t.document.set_active_page_index(cur + 1);
                        t.selection.clear();
                        t.sync_active_layer();
                        t.needs_rerender = true;
                    }
                }
            }
            ShortcutAction::NewDocument => self.new_doc_dialog.open = true,
            ShortcutAction::OpenDocument => self.open_file_dialog(),
            ShortcutAction::SaveDocument => {
                if self
                    .active_tab()
                    .and_then(|t| t.source_path.as_ref())
                    .is_some()
                {
                    if let Err(e) = self.save_active(None) {
                        self.push_toast(e, true);
                    }
                } else {
                    self.save_as_file_dialog();
                }
            }
            ShortcutAction::SaveDocumentAs => self.save_as_file_dialog(),
            ShortcutAction::ImportFile => self.import_file_dialog(),
            ShortcutAction::ExportFile => self.export_dialog.open = true,
            ShortcutAction::PrintDocument => {
                self.show_print_preview_dialog = true;
                self.refresh_system_printers_async();
            }
            ShortcutAction::ToggleCommandPalette => {
                self.command_palette.open = !self.command_palette.open;
                if self.command_palette.open {
                    self.command_palette.query.clear();
                    self.command_palette.selected_index = 0;
                    self.command_palette.just_opened = true;
                }
            }
            ShortcutAction::OpenFindReplace => self.focus_docker(DockerTab::FindReplace),
            ShortcutAction::OpenOptions => {
                self.show_options_dialog = true;
                self.show_preferences_dialog = true;
            }
            ShortcutAction::ToggleDockerObjects => self.toggle_docker(DockerTab::Objects),
            ShortcutAction::ToggleDockerProperties => self.toggle_docker(DockerTab::Properties),
            ShortcutAction::ToggleDockerLens => self.toggle_docker(DockerTab::Lens),
            ShortcutAction::Undo => {
                let _ = self.undo();
            }
            ShortcutAction::Redo => {
                let _ = self.redo();
            }
            ShortcutAction::Cut => {
                self.cut_selection();
            }
            ShortcutAction::Copy => {
                self.copy_selection();
            }
            ShortcutAction::Paste => {
                self.paste_clipboard();
            }
            ShortcutAction::Duplicate => {
                let _ = self.duplicate_selection();
            }
            ShortcutAction::DuplicateInPlace => {
                let _ = self.duplicate_selection_with_offset(Vec2D::ZERO);
            }
            ShortcutAction::RepeatLastCommand => self.repeat_last_command(),
            ShortcutAction::DeleteSelection => {
                // If actively drafting a Bezier/Pen/BSpline/Polyline path, pop the last node
                let popped_draft = self
                    .active_tab_mut()
                    .is_some_and(|tab| tab.tool_core.pop_last_draft_point());
                if popped_draft {
                    return;
                }
                // In ShapeNode mode with selected Bezier nodes, Delete removes those nodes!
                if self.active_tool == ToolId::ShapeNode
                    && self
                        .active_tab()
                        .is_some_and(|t| !t.selection.node_selection.is_empty())
                {
                    self.shape_delete_selected_nodes();
                } else {
                    self.delete_selection();
                }
            }
            ShortcutAction::SelectAll => {
                if self.active_tool == ToolId::ShapeNode
                    && self.active_tab().is_some_and(|t| !t.selection.is_empty())
                {
                    let n = self.shape_select_all_nodes();
                    if n > 0 {
                        self.push_toast(format!("Selected all {n} nodes"), false);
                        return;
                    }
                }
                self.select_all();
            }
            ShortcutAction::EscapeCancel => {
                self.open_flyout_group = None;
                if self.full_screen_preview {
                    self.full_screen_preview = false;
                } else if self.show_edit_fill_dialog {
                    self.show_edit_fill_dialog = false;
                } else if self.show_outline_pen_dialog {
                    self.show_outline_pen_dialog = false;
                } else if self.command_palette.open {
                    self.command_palette.open = false;
                } else if self.show_options_dialog {
                    self.show_options_dialog = false;
                    self.show_preferences_dialog = false;
                } else if self.new_doc_dialog.open {
                    self.new_doc_dialog.open = false;
                } else if self.export_dialog.open {
                    self.export_dialog.open = false;
                } else if self
                    .active_tab()
                    .is_some_and(|t| t.inline_text_edit.is_some())
                {
                    self.cancel_inline_text_edit();
                } else if let Some(tab) = self.active_tab_mut() {
                    let cleared_draft = tab.tool_core.has_active_draft_or_drag()
                        || tab.pending_crop_rect.is_some()
                        || tab.raster_mask_rect_mm.is_some()
                        || tab.raster_mask.is_some();
                    tab.cancel_active_drag();
                    let _ = tab.tool_core.cancel_all();
                    tab.pending_crop_rect = None;
                    tab.raster_mask_rect_mm = None;
                    tab.raster_mask = None;
                    if !cleared_draft {
                        if !tab.selection.is_empty() {
                            tab.selection.clear();
                            tab.metrics_dirty = true;
                        } else if tab.powerclip_edit_id.is_some() {
                            tab.powerclip_edit_id = None;
                            tab.needs_rerender = true;
                        }
                    }
                }
            }
            ShortcutAction::Group => self.group_selection(),
            ShortcutAction::Ungroup => self.ungroup_selection(),
            ShortcutAction::ConvertToCurves => self.convert_selection_to_curves(),
            ShortcutAction::ConvertOutlineToObject => self.convert_outline_to_object_selection(),
            ShortcutAction::BreakApart => self.break_apart_selection(),
            ShortcutAction::Combine => self.apply_boolean_op(BooleanOp::Combine),
            ShortcutAction::TogglePowerClipLock => self.set_powerclip_lock_selected(None),
            ShortcutAction::MoveToDesktop => self.move_selection_to_desktop_layer(),
            ShortcutAction::ZOrder(op) => self.apply_z_order(op),
            ShortcutAction::Align(op) => self.apply_align(op),
            ShortcutAction::Nudge {
                dx_steps,
                dy_steps,
                mult,
            } => self.nudge_selection(dx_steps, dy_steps, mult),
            ShortcutAction::MicroNudge { dx_steps, dy_steps } => {
                self.micro_nudge_selection(dx_steps, dy_steps);
            }
        }
    }

    /// Zooms the active viewport around its center by `factor`.
    pub fn zoom_step(&mut self, factor: f64) {
        if let Some(tab) = self.active_tab_mut() {
            let center = Point2D::new(
                f64::from(tab.viewport.width_px) * 0.5,
                f64::from(tab.viewport.height_px) * 0.5,
            );
            tab.viewport.zoom_at_screen_point(center, factor);
            tab.needs_rerender = true;
        }
    }

    // -------------------------------------------------------------------------
    // Native `rfd` xdg-portal / Win32 File Dialog Triggers
    // -------------------------------------------------------------------------

    /// Opens a native OS file picker and opens the chosen document.
    /// Includes both lowercase and uppercase extensions for Linux portal & Windows safety,
    /// plus an `"All Files"` fallback filter so valid files are never hidden.
    pub fn open_file_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Open Document")
            .add_filter(
                "All Supported Studio Formats",
                &[
                    "coree", "COREE", "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT",
                    "pdf", "PDF", "ai", "AI", "eps", "EPS", "ps", "PS", "dxf", "DXF", "dwg", "DWG",
                    "wmf", "WMF", "emf", "EMF", "plt", "PLT", "psd", "PSD", "psb", "PSB", "svg",
                    "SVG", "svgz", "SVGZ", "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp",
                    "WEBP", "tiff", "TIFF", "tif", "TIF", "bmp", "BMP", "zip", "ZIP",
                ],
            )
            .add_filter("Native coree (.coree)", &["coree", "COREE"])
            .add_filter(
                "CDR / CDT / CMX / CPT (.cdr, .cdr.zip)",
                &[
                    "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT", "zip", "ZIP",
                ],
            )
            .add_filter("PDF & Vector AI (.pdf, .ai)", &["pdf", "PDF", "ai", "AI"])
            .add_filter("PostScript / EPS (.eps, .ps)", &["eps", "EPS", "ps", "PS"])
            .add_filter(
                "CAD / Plotter (.dxf, .dwg, .plt)",
                &["dxf", "DXF", "dwg", "DWG", "plt", "PLT"],
            )
            .add_filter(
                "Windows Metafiles (.wmf, .emf)",
                &["wmf", "WMF", "emf", "EMF"],
            )
            .add_filter("SVG Vector (.svg, .svgz)", &["svg", "SVG", "svgz", "SVGZ"])
            .add_filter(
                "Layered Raster / PSD (.psd, .psb)",
                &["psd", "PSD", "psb", "PSB"],
            )
            .add_filter(
                "Raster Images (.png, .jpg, .webp, .tif, .bmp)",
                &[
                    "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp", "WEBP", "tiff", "TIFF",
                    "tif", "TIF", "bmp", "BMP",
                ],
            )
            .add_filter("All Files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            if let Err(err) = self.open_file_path(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Opens a native OS file picker and imports the chosen file into the active page.
    /// Includes all supported vector, CAD, PDF/EPS, PSD, CDR, and raster extensions (lower + upper)
    /// plus `"All Files"` fallback.
    pub fn import_file_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Import into Active Document")
            .add_filter(
                "All Importable Formats",
                &[
                    "coree", "COREE", "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT",
                    "pdf", "PDF", "ai", "AI", "eps", "EPS", "ps", "PS", "dxf", "DXF", "dwg", "DWG",
                    "wmf", "WMF", "emf", "EMF", "plt", "PLT", "psd", "PSD", "psb", "PSB", "svg",
                    "SVG", "svgz", "SVGZ", "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp",
                    "WEBP", "tiff", "TIFF", "tif", "TIF", "bmp", "BMP", "zip", "ZIP",
                ],
            )
            .add_filter(
                "CDR / CDT / CMX / CPT (.cdr, .cdr.zip)",
                &[
                    "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT", "zip", "ZIP",
                ],
            )
            .add_filter("PDF & Vector AI (.pdf, .ai)", &["pdf", "PDF", "ai", "AI"])
            .add_filter("PostScript / EPS (.eps, .ps)", &["eps", "EPS", "ps", "PS"])
            .add_filter(
                "CAD / Plotter (.dxf, .dwg, .plt)",
                &["dxf", "DXF", "dwg", "DWG", "plt", "PLT"],
            )
            .add_filter(
                "Windows Metafiles (.wmf, .emf)",
                &["wmf", "WMF", "emf", "EMF"],
            )
            .add_filter("SVG Vector (.svg, .svgz)", &["svg", "SVG", "svgz", "SVGZ"])
            .add_filter(
                "Layered Raster / PSD (.psd, .psb)",
                &["psd", "PSD", "psb", "PSB"],
            )
            .add_filter(
                "Raster Images (.png, .jpg, .webp, .tif, .bmp)",
                &[
                    "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp", "WEBP", "tiff", "TIFF",
                    "tif", "TIF", "bmp", "BMP",
                ],
            )
            .add_filter("Native coree (.coree)", &["coree", "COREE"])
            .add_filter("All Files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            if let Err(err) = self.import_file_path(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Opens a native OS Save As dialog for `.coree` / `.cdr` / `.cdt` / `.svg`.
    pub fn save_as_file_dialog(&mut self) {
        let default_name = self
            .active_tab()
            .map(|t| format!("{}.coree", t.document.title()))
            .unwrap_or_else(|| "Untitled.coree".to_owned());
        let picked = rfd::FileDialog::new()
            .set_title("Save Document As")
            .set_file_name(&default_name)
            .add_filter("Native coree", &["coree", "COREE"])
            .add_filter("CDR Archive", &["cdr", "CDR"])
            .add_filter("CDT Template", &["cdt", "CDT"])
            .add_filter("SVG Vector", &["svg", "SVG"])
            .add_filter("All Files", &["*"])
            .save_file();
        if let Some(path) = picked {
            if let Err(err) = self.save_active_as(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Saves a non-destructive numbered backup copy (`_v1`, `_v2`, ...) beside the active file.
    pub fn save_incremental_copy(&mut self) -> Result<PathBuf, String> {
        let src = {
            let tab = self
                .active_tab()
                .ok_or_else(|| "no active document".to_owned())?;
            tab.source_path
                .clone()
                .ok_or_else(|| "document has not been saved yet; use Save As first".to_owned())?
        };
        let target = next_incremental_path(&src);
        self.save_active_as(&target)?;
        Ok(target)
    }

    // -------------------------------------------------------------------------
    // Command Palette Registry & Fuzzy Search
    // -------------------------------------------------------------------------

    /// Builds the complete list of searchable Command Palette actions.
    #[must_use]
    pub fn all_palette_commands(&self) -> Vec<PaletteCommand> {
        let p = self.shortcut_profile;
        let mut list = vec![
            PaletteCommand {
                id: "file.new".into(),
                title: "New Document...".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::NewDocument).into(),
            },
            PaletteCommand {
                id: "file.sample".into(),
                title: "Open Showcase Sample Document".into(),
                category: "File".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "file.open".into(),
                title: "Open File...".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::OpenDocument).into(),
            },
            PaletteCommand {
                id: "file.save".into(),
                title: "Save Document".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::SaveDocument).into(),
            },
            PaletteCommand {
                id: "file.save_incremental".into(),
                title: "Save Incremental Copy (_vN)".into(),
                category: "File".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "file.export".into(),
                title: "Export Document...".into(),
                category: "File".into(),
                shortcut: shortcut_label_for(p, &self.custom_shortcuts, ShortcutAction::ExportFile),
            },
            PaletteCommand {
                id: "file.print".into(),
                title: "Print Document (Native OS Spooler)...".into(),
                category: "File".into(),
                shortcut: shortcut_label_for(
                    p,
                    &self.custom_shortcuts,
                    ShortcutAction::PrintDocument,
                ),
            },
            PaletteCommand {
                id: "edit.undo".into(),
                title: "Undo".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Undo).into(),
            },
            PaletteCommand {
                id: "edit.redo".into(),
                title: "Redo".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Redo).into(),
            },
            PaletteCommand {
                id: "edit.duplicate".into(),
                title: "Duplicate Selection".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Duplicate).into(),
            },
            PaletteCommand {
                id: "object.group".into(),
                title: "Group Selection".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::Group).into(),
            },
            PaletteCommand {
                id: "object.ungroup".into(),
                title: "Ungroup Selection".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::Ungroup).into(),
            },
            PaletteCommand {
                id: "object.curves".into(),
                title: "Convert to Curves".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::ConvertToCurves).into(),
            },
            PaletteCommand {
                id: "object.convert_outline_to_object".into(),
                title: "Convert Outline to Object".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::ConvertOutlineToObject).into(),
            },
            PaletteCommand {
                id: "object.powerclip.create".into(),
                title: "PowerClip: Place Inside Frame".into(),
                category: "Object".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "object.powerclip.extract".into(),
                title: "PowerClip: Extract Contents".into(),
                category: "Object".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "object.powerclip.lock".into(),
                title: "PowerClip: Toggle Lock Contents".into(),
                category: "Object".into(),
                shortcut: shortcut_label_for(
                    p,
                    &self.custom_shortcuts,
                    ShortcutAction::TogglePowerClipLock,
                ),
            },
            PaletteCommand {
                id: "object.desktop_layer".into(),
                title: "Move Selection to Cross-Page Desktop Layer".into(),
                category: "Object".into(),
                shortcut: shortcut_label_for(
                    p,
                    &self.custom_shortcuts,
                    ShortcutAction::MoveToDesktop,
                ),
            },
            PaletteCommand {
                id: "prod.cutcontour".into(),
                title: "One-Click CutContour Spot Hairline".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.preflight".into(),
                title: "Run Print Preflight Audit".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.separations".into(),
                title: "Compute CMYK & Spot Separations".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.printmarks".into(),
                title: "Generate Crop & Registration Marks".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.multipage".into(),
                title: "Toggle Multipage View".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.wireframe".into(),
                title: "View Mode: Wireframe".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.enhanced".into(),
                title: "View Mode: Enhanced".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.zoom_all".into(),
                title: "Zoom to All Objects".into(),
                category: "View".into(),
                shortcut: shortcut_label(p, ShortcutAction::ZoomToAll).into(),
            },
            PaletteCommand {
                id: "view.zoom_page".into(),
                title: "Zoom to Active Page".into(),
                category: "View".into(),
                shortcut: shortcut_label(p, ShortcutAction::ZoomToPage).into(),
            },
            PaletteCommand {
                id: "text.fit_path".into(),
                title: "Fit Text to Path".into(),
                category: "Text".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "table.insert".into(),
                title: "Insert Vector Table (3x3)".into(),
                category: "Table".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "bitmap.trace".into(),
                title: "Vector Trace Selected Bitmap".into(),
                category: "Bitmaps".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "bitmap.convert".into(),
                title: "Convert Selection to Bitmap".into(),
                category: "Bitmaps".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.dark".into(),
                title: "Theme: Dark Studio".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.light".into(),
                title: "Theme: Classic Light".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.contrast".into(),
                title: "Theme: High Contrast".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "profile.classic".into(),
                title: "Shortcuts: Classic Studio".into(),
                category: "Shortcuts".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "profile.standard".into(),
                title: "Shortcuts: Standard Vector".into(),
                category: "Shortcuts".into(),
                shortcut: String::new(),
            },
        ];

        // All 9 Boolean Shaping operations
        for (id_suffix, title) in [
            ("weld", "Shaping: Weld"),
            ("trim", "Shaping: Trim"),
            ("intersect", "Shaping: Intersect"),
            ("simplify", "Shaping: Simplify"),
            ("front_minus_back", "Shaping: Front Minus Back"),
            ("back_minus_front", "Shaping: Back Minus Front"),
            ("boundary", "Shaping: Create Boundary"),
            ("combine", "Shaping: Combine"),
            ("break_apart", "Shaping: Break Apart"),
        ] {
            list.push(PaletteCommand {
                id: format!("bool.{id_suffix}"),
                title: title.into(),
                category: "Shaping".into(),
                shortcut: String::new(),
            });
        }

        // All 11 Lenses
        for (idx, name) in LensDockerState::NAMES.iter().enumerate() {
            list.push(PaletteCommand {
                id: format!("lens.{idx}"),
                title: format!("Apply Lens: {name}"),
                category: "Lens".into(),
                shortcut: String::new(),
            });
        }

        // All 14 Dockers
        for docker in DockerTab::ALL {
            list.push(PaletteCommand {
                id: format!("docker.{}", docker.title()),
                title: format!("Show Docker: {}", docker.title()),
                category: "Window".into(),
                shortcut: String::new(),
            });
        }

        // All Tools across all 15 Toolbox Flyout groups
        for group in TOOL_GROUPS {
            for &tool in group {
                let sc = crate::state::tool_shortcut_action(tool)
                    .map(|a| shortcut_label(p, a))
                    .unwrap_or("");
                list.push(PaletteCommand {
                    id: format!("tool.{tool:?}"),
                    title: format!("Tool: {}", tool.label()),
                    category: "Toolbox".into(),
                    shortcut: sc.into(),
                });
            }
        }

        // Built-in Rhai Macros
        for m in builtin_macros() {
            list.push(PaletteCommand {
                id: format!("macro.{}", m.id),
                title: format!("Run Macro: {}", m.name),
                category: "Macros".into(),
                shortcut: String::new(),
            });
        }

        list
    }

    /// Fuzzy-filters Command Palette commands ordered by match quality.
    #[must_use]
    pub fn search_command_palette(&self, query: &str) -> Vec<PaletteCommand> {
        let q = query.trim().to_ascii_lowercase();
        let all = self.all_palette_commands();
        if q.is_empty() {
            return all;
        }
        let mut scored: Vec<(i32, PaletteCommand)> = all
            .into_iter()
            .filter_map(|cmd| {
                let hay = format!("{} {}", cmd.title, cmd.category).to_ascii_lowercase();
                fuzzy_score(&hay, &q).map(|s| (s, cmd))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    /// Executes a Command Palette action by its `id`.
    pub fn execute_palette_command(&mut self, id: &str) -> bool {
        match id {
            "file.new" => self.new_doc_dialog.open = true,
            "file.sample" => self.new_sample_document(),
            "file.open" => self.open_file_dialog(),
            "file.save" => self.handle_shortcut(ShortcutAction::SaveDocument),
            "file.save_incremental" => {
                if let Err(e) = self.save_incremental_copy() {
                    self.push_toast(e, true);
                }
            }
            "file.export" => self.export_dialog.open = true,
            "file.print" => self.handle_shortcut(ShortcutAction::PrintDocument),
            "edit.undo" => {
                let _ = self.undo();
            }
            "edit.redo" => {
                let _ = self.redo();
            }
            "edit.duplicate" => {
                let _ = self.duplicate_selection();
            }
            "object.group" => self.group_selection(),
            "object.ungroup" => self.ungroup_selection(),
            "object.curves" => self.convert_selection_to_curves(),
            "object.convert_outline_to_object" => self.convert_outline_to_object_selection(),
            "object.powerclip.create" => self.create_powerclip_from_selection(),
            "object.powerclip.extract" => self.extract_selected_powerclip(),
            "object.powerclip.lock" => self.set_powerclip_lock_selected(None),
            "object.desktop_layer" => self.move_selection_to_desktop_layer(),
            "prod.cutcontour" => self.generate_cut_contour_active(),
            "prod.preflight" => {
                self.run_preflight_active();
                self.production_state.sub_section = 0;
                self.focus_docker(DockerTab::Production);
            }
            "prod.separations" => {
                self.compute_separations_active();
                self.production_state.sub_section = 1;
                self.focus_docker(DockerTab::Production);
            }
            "prod.printmarks" => self.add_print_marks_active(),
            "view.multipage" => {
                let next = self.active_tab().map(|t| !t.document.multipage_view());
                if let Some(next) = next {
                    let _ = self.execute_command(CommandOp::SetDocumentProperties {
                        title: None,
                        color_mode: None,
                        default_unit: None,
                        dpi: None,
                        multipage_view: Some(next),
                        active_page_index: None,
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
                    if let Some(t) = self.active_tab_mut() {
                        t.viewport.multipage_view = next;
                        t.needs_rerender = true;
                    }
                }
            }
            "view.wireframe" => {
                if let Some(t) = self.active_tab_mut() {
                    t.view_mode = ViewMode::Wireframe;
                    t.needs_rerender = true;
                }
            }
            "view.enhanced" => {
                if let Some(t) = self.active_tab_mut() {
                    t.view_mode = ViewMode::Enhanced;
                    t.needs_rerender = true;
                }
            }
            "view.zoom_all" => self.handle_shortcut(ShortcutAction::ZoomToAll),
            "view.zoom_page" => self.handle_shortcut(ShortcutAction::ZoomToPage),
            "text.fit_path" => self.fit_selected_text_to_path(),
            "table.insert" => self.insert_table(3, 3),
            "bitmap.trace" => {
                if let Err(e) = self.trace_selected_raster() {
                    self.push_toast(e, true);
                }
            }
            "bitmap.convert" => self.convert_selection_to_bitmap(),
            "theme.dark" => self.set_theme(StudioTheme::DarkStudio),
            "theme.light" => self.set_theme(StudioTheme::ClassicLight),
            "theme.contrast" => self.set_theme(StudioTheme::HighContrast),
            "profile.classic" => self.set_shortcut_profile(ShortcutProfile::ClassicStudio),
            "profile.standard" => self.set_shortcut_profile(ShortcutProfile::StandardVector),
            "bool.weld" => self.apply_boolean_op(BooleanOp::Weld),
            "bool.trim" => self.apply_boolean_op(BooleanOp::Trim),
            "bool.intersect" => self.apply_boolean_op(BooleanOp::Intersect),
            "bool.simplify" => self.apply_boolean_op(BooleanOp::Simplify),
            "bool.front_minus_back" => self.apply_boolean_op(BooleanOp::FrontMinusBack),
            "bool.back_minus_front" => self.apply_boolean_op(BooleanOp::BackMinusFront),
            "bool.boundary" => self.apply_boolean_op(BooleanOp::CreateBoundary),
            "bool.combine" => self.apply_boolean_op(BooleanOp::Combine),
            "bool.break_apart" => self.apply_boolean_op(BooleanOp::BreakApart),
            other => {
                if let Some(idx_str) = other.strip_prefix("lens.") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if idx < 11 {
                            self.lens_state.selected_index = idx;
                            let cfg = self.lens_state.build_config();
                            self.apply_lens_to_selection(Some(cfg));
                            self.focus_docker(DockerTab::Lens);
                            return true;
                        }
                    }
                }
                if let Some(title) = other.strip_prefix("docker.") {
                    if let Some(d) = DockerTab::ALL.into_iter().find(|d| d.title() == title) {
                        self.focus_docker(d);
                        return true;
                    }
                }
                if let Some(tool_dbg) = other.strip_prefix("tool.") {
                    for group in TOOL_GROUPS {
                        for &tool in group {
                            if format!("{tool:?}") == tool_dbg {
                                self.select_tool(tool);
                                return true;
                            }
                        }
                    }
                }
                if let Some(macro_id) = other.strip_prefix("macro.") {
                    if let Some(m) = coree_prod::find_builtin_macro(macro_id) {
                        let _ = self.run_rhai_script(m.script);
                        return true;
                    }
                }
                return false;
            }
        }
        true
    }
}

fn next_incremental_path(src: &Path) -> PathBuf {
    let dir = src
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let raw_stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Untitled");
    let raw_ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "coree".to_owned());
    let ext = if matches!(raw_ext.as_str(), "coree" | "cdr" | "cdt" | "svg") {
        raw_ext
    } else {
        "coree".to_owned()
    };

    let (base, start_num) = if let Some((prefix, num_str)) = raw_stem.rsplit_once("_v") {
        if !prefix.is_empty() {
            if let Ok(n) = num_str.parse::<u32>() {
                (prefix.to_owned(), n.saturating_add(1).max(1))
            } else {
                (raw_stem.to_owned(), 1)
            }
        } else {
            (raw_stem.to_owned(), 1)
        }
    } else {
        (raw_stem.to_owned(), 1)
    };

    for v in start_num..start_num.saturating_add(10_000) {
        let candidate = dir.join(format!("{base}_v{v}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{base}_v{start_num}.{ext}"))
}
