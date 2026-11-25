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
