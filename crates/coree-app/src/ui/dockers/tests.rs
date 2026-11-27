// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::ui::dialogs::{cached_detect_executable, invalidate_plugin_detect_cache};
use crate::ui::dockers::find_symbols_fonts::ui_font_manager_docker;
use crate::ui::dockers::inspector_laser::run_rhai_plugin_undoable;
use coree_prod::{install_sample_plugin_template, PluginCategory, PluginRegistry};

#[test]
fn cached_plugin_detection_avoids_repeated_fs_probes_and_invalidates_cleanly() {
    let ctx = egui::Context::default();
    let tmp_dir =
        std::env::temp_dir().join(format!("coree_plugin_cache_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    let fake_exe = tmp_dir.join("fake_laser_bridge");
    std::fs::write(&fake_exe, b"#!/bin/sh\nexit 0\n").expect("write fake exe");

    let plugin = PluginRegistry::builtin_presets()
        .into_iter()
        .next()
        .expect("builtin preset");
    let override_str = fake_exe.to_string_lossy().into_owned();

    // 1. First probe finds the fake executable and caches it
    let first = cached_detect_executable(&ctx, &plugin, &override_str);
    assert_eq!(first.as_deref(), Some(fake_exe.as_path()));

    // 2. Delete file on disk: cached probe must STILL return cached Some without hitting fs
    std::fs::remove_file(&fake_exe).expect("remove fake exe");
    let second = cached_detect_executable(&ctx, &plugin, &override_str);
    assert_eq!(second.as_deref(), Some(fake_exe.as_path()));

    // 3. Changing override path immediately re-probes the new key
    let missing_override = tmp_dir
        .join("nonexistent_bin")
        .to_string_lossy()
        .into_owned();
    assert!(cached_detect_executable(&ctx, &plugin, &missing_override).is_none());

    // 4. Invalidating the cache causes the original key to re-probe and see removal
    invalidate_plugin_detect_cache();
    assert!(cached_detect_executable(&ctx, &plugin, &override_str).is_none());

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn rhai_plugin_execution_is_undoable_redoable_and_rolls_back_on_error() {
    let tmp_dir =
        std::env::temp_dir().join(format!("coree_rhai_undo_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    let plugin_dir = install_sample_plugin_template(&tmp_dir).expect("install sample plugin");
    let mut reg = PluginRegistry::empty();
    let loaded = reg.load_discovered(std::slice::from_ref(&tmp_dir));
    assert_eq!(loaded, 1);
    let sample_plugin = reg
        .find("custom-laser-example")
        .expect("custom-laser-example")
        .clone();

    let mut app = StudioApp::new_headless();
    app.new_sample_document();
    let doc_before_plugin = app.active_tab().unwrap().document.clone();
    let count_before = doc_before_plugin.objects().len();
    assert!(count_before > 0);

    // Run custom Rhai plugin -> adds a CutContour halo object via real CommandOps
    run_rhai_plugin_undoable(&mut app, &sample_plugin);
    let count_after = app.active_tab().unwrap().document.objects().len();
    assert!(
        count_after > count_before,
        "sample Rhai plugin should add CutContour object"
    );
    assert!(app.active_tab().unwrap().history.can_undo());

    let log = app.active_tab().unwrap().history.command_log();
    fn has_empty_transform(op: &CommandOp) -> bool {
        match op {
            CommandOp::TransformObjects { ids, .. } => ids.is_empty(),
            CommandOp::Batch(ops) => ops.iter().any(has_empty_transform),
            _ => false,
        }
    }
    assert!(
        !log.iter().any(has_empty_transform),
        "run_rhai_plugin_undoable must never emit dummy empty TransformObjects"
    );
    let mut replayed = doc_before_plugin;
    coree_core::CommandHistory::replay_log(&mut replayed, &log)
        .expect("replay_log must reproduce plugin execution");
    assert_eq!(
        replayed.objects(),
        app.active_tab().unwrap().document.objects()
    );
    assert_eq!(replayed.pages(), app.active_tab().unwrap().document.pages());

    // 0-command no-op Rhai plugin must NOT push a spurious undo entry
    let noop_dir = tmp_dir.join("noop-plugin");
    std::fs::create_dir_all(&noop_dir).expect("create noop dir");
    std::fs::write(noop_dir.join("noop.rhai"), "log(\"noop plugin\");").expect("write noop rhai");
    let noop_plugin = PluginManifest {
        id: "noop-plugin".to_owned(),
        name: "Noop Plugin".to_owned(),
        version: "1.0.0".to_owned(),
        author: "test".to_owned(),
        description: "logs without mutating".to_owned(),
        category: PluginCategory::Laser,
        kind: PluginKind::RhaiScript {
            script_file: "noop.rhai".to_owned(),
            entry_label: "Noop".to_owned(),
        },
        source_dir: Some(noop_dir),
        builtin: false,
    };
    let undo_len_before_noop = app.active_tab().unwrap().history.undo_labels().len();
    run_rhai_plugin_undoable(&mut app, &noop_plugin);
    assert_eq!(
        app.active_tab().unwrap().history.undo_labels().len(),
        undo_len_before_noop,
        "0-op Rhai plugin must not push a spurious undo entry"
    );

    // Undo cleanly restores exact pre-plugin object count
    app.undo().expect("undo plugin transaction");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before
    );

    // Redo cleanly restores exact post-plugin object count
    app.redo().expect("redo plugin transaction");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_after
    );

    // Failing Rhai plugin after a mutation rolls back cleanly via cancel_transaction
    let bad_dir = tmp_dir.join("bad-plugin");
    std::fs::create_dir_all(&bad_dir).expect("create bad dir");
    std::fs::write(
            bad_dir.join("bad.rhai"),
            "let ids = non_cut_object_ids(); generate_cut_contour(ids, 1.0, \"CutContour\"); throw \"boom\";",
        )
        .expect("write bad rhai");
    let bad_plugin = PluginManifest {
        id: "bad-plugin".to_owned(),
        name: "Bad Plugin".to_owned(),
        version: "1.0.0".to_owned(),
        author: "test".to_owned(),
        description: "fails after mutating".to_owned(),
        category: PluginCategory::Laser,
        kind: PluginKind::RhaiScript {
            script_file: "bad.rhai".to_owned(),
            entry_label: "Fail".to_owned(),
        },
        source_dir: Some(bad_dir),
        builtin: false,
    };
    run_rhai_plugin_undoable(&mut app, &bad_plugin);
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_after,
        "failed Rhai plugin must roll back partial mutations"
    );

    let _ = std::fs::remove_dir_all(&plugin_dir);
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn font_manager_ui_renders_favorites_panose_and_glyphs_headlessly() {
    let db = coree_storage::StorageDb::open_in_memory().expect("open in-memory storage");
    let mut app = StudioApp::new_with_storage(db);
    app.font_manager_state.scanned_once = true;
    app.font_manager_state.missing_family_input = "MissingFont".into();
    app.font_manager_state.substitute_family_input = "Noto Sans".into();

    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(360.0, 720.0),
        )),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        ui_font_manager_docker(ui, &mut app);
    });
    out.textures_delta.clear();
    assert!(!out.shapes.is_empty());
}

#[test]
fn trace_split_preview_and_cdr_hex_view_render_headlessly() {
    let mut app = StudioApp::new_headless();
    app.trace_state.split_preview = true;
    let chunk = coree_cdr::ChunkSummary {
        fourcc: "loda".to_owned(),
        size: 8,
        depth: 2,
        is_list: false,
        compressed: false,
        preview_bytes: vec![0x4C, 0x4F, 0x44, 0x41, 0x01, 0x02, 0x03, 0x04],
    };

    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(360.0, 480.0),
        )),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        crate::ui::dockers::render_trace_split_preview(ui, &mut app);
        crate::ui::dockers::hex_view(ui, &chunk, crate::theme::StudioLanguage::English);
    });
    out.textures_delta.clear();
    assert!(!out.shapes.is_empty());
}

#[test]
fn trace_raster_via_to_curves_keep_original_side_by_side_and_split_preview() {
    use coree_core::{
        ColorMode, Fill, NodePayload, ObjectId, OutlineStyle, RasterAsset, RasterLayerData,
        SceneObject,
    };
    let mut app = StudioApp::new_headless();
    let w = 48u32;
    let h = 48u32;
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for y in 8..40 {
        for x in 8..40 {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[210, 30, 40, 255]);
        }
    }
    let asset = RasterAsset::new(w, h, rgba, 96.0, ColorMode::Rgb).unwrap();
    let aid = asset.id;
    let tab = app.active_tab_mut().unwrap();
    tab.document.insert_asset_if_absent(asset);
    let lid = tab.document.pages()[0].layers[0].id;
    let rid = tab
        .document
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LogoBmp",
                NodePayload::Raster(RasterLayerData {
                    asset_id: aid,
                    width_mm: 40.0,
                    height_mm: 40.0,
                    filters: Vec::new(),
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();
    tab.selection.select_single(&tab.document, rid);

    // 1. Live WYSIWYG Split Preview with an actual selected raster renders cleanly
    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(360.0, 480.0),
        )),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        crate::ui::dockers::render_trace_split_preview(ui, &mut app);
    });
    out.textures_delta.clear();
    assert!(app.trace_state.split_preview_cache.borrow().is_some());

    // 2. `convert_selection_to_curves` (`To Curves` / `Ctrl+Q`) on a selected Raster traces it
    // non-destructively (keeps original + places beside when enabled)
    app.trace_state.keep_original = true;
    app.trace_state.place_side_by_side = true;
    app.convert_selection_to_curves();
    {
        let tab = app.active_tab().unwrap();
        assert!(
            tab.document.object(rid).is_some(),
            "original raster must be preserved when keep_original=true"
        );
        assert!(
            tab.document.objects().len() >= 2,
            "expected traced vector curve created alongside original raster"
        );
    }
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 1);

    // 3. `keep_original = false` replaces original bitmap atomically and restores on 1 Undo
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, rid);
    }
    app.trace_state.keep_original = false;
    app.trace_state.place_side_by_side = false;
    let cnt = app.trace_selected_raster().unwrap();
    assert_eq!(cnt, 1);
    assert!(
        app.active_tab().unwrap().document.object(rid).is_none(),
        "original raster must be removed when keep_original=false"
    );
    app.undo().unwrap();
    assert!(
        app.active_tab().unwrap().document.object(rid).is_some(),
        "single undo must restore original raster"
    );

    // 4. Imported PDF `PowerClip` wrapping a `Raster` ("PDF Clip 4"):
    // `Convert To Curves` MUST vectorize the raster inside the PowerClip instead of wiping it!
    let pc_frame_id = {
        let tab = app.active_tab_mut().unwrap();
        let fid = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "PDF Clip 4",
                    NodePayload::Path(coree_core::BezierPath::from_rect(
                        coree_core::Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0),
                    )),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none()),
                Some(lid),
            )
            .unwrap();
        let pc_id = tab.document.create_powerclip(fid, &[rid]).unwrap();
        tab.selection.select_single(&tab.document, pc_id);
        pc_id
    };
    assert!(app.selection_has_traceable_raster());
    assert!(app.selection_is_exclusively_traceable_raster());
    app.trace_state.keep_original = true;
    app.convert_selection_to_curves();
    {
        let tab = app.active_tab().unwrap();
        assert!(
            tab.document.object(pc_frame_id).is_some(),
            "PDF Clip PowerClip must NOT be destroyed when keep_original=true"
        );
        let has_filled_traced_path = tab
            .document
            .objects()
            .values()
            .any(|o| matches!(o.payload, NodePayload::Path(_)) && o.fill.is_visible());
        assert!(
            has_filled_traced_path,
            "Convert To Curves on PDF PowerClip raster must create visible traced vector curve"
        );
    }
}
