// SPDX-License-Identifier: MPL-2.0

//! End-to-end integration tests for `coree-storage`.

use coree_core::{
    apply_command, Affine2D, BezierPath, Color, ColorMode, ColorPalette, ColorSwatch, CommandOp,
    Document, Fill, NodePayload, ObjectId, OutlineStyle, Rect2D, SceneObject,
};
use coree_storage::{
    standard_font_directories, BrushPresetRecord, FontQuery, JobOutcome, JobPool, JobState,
    RecentDocumentRecord, StorageDb, StorageError, TemplateRecord, WorkspaceLayoutRecord,
    SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn sqlite_schema_matches_all_tables_columns_constraints_and_cascades() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("coree.db");
    let db = StorageDb::open(&db_path).unwrap();

    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(db.journal_mode().unwrap().to_ascii_lowercase(), "wal");
    assert!(db.foreign_keys_enabled().unwrap());

    // Verify critical columns across all 10 tables from replica/schema.sql
    let expected_columns: &[(&str, &[&str])] = &[
        (
            "recent_documents",
            &[
                "id",
                "file_path",
                "display_name",
                "file_format",
                "cdr_version",
                "page_count",
                "color_mode",
                "thumbnail_png",
                "file_size_bytes",
                "pinned",
                "last_opened_at",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "autosave_snapshots",
            &[
                "id",
                "document_id",
                "session_id",
                "original_file_path",
                "display_name",
                "snapshot_blob_path",
                "snapshot_sha256",
                "last_command_seq",
                "byte_size",
                "clean_shutdown",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "command_journal",
            &[
                "id",
                "document_id",
                "session_id",
                "snapshot_id",
                "seq_no",
                "command_type",
                "payload_json",
                "inverse_json",
                "rhai_snippet",
                "applied",
                "committed_at",
            ],
        ),
        (
            "custom_palettes",
            &[
                "id",
                "name",
                "description",
                "palette_kind",
                "is_builtin",
                "is_docked",
                "show_color_names",
                "sort_order",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "palette_swatches",
            &[
                "id",
                "palette_id",
                "name",
                "color_space",
                "c",
                "m",
                "y",
                "k",
                "r",
                "g",
                "b",
                "lab_l",
                "lab_a",
                "lab_b",
                "spot_channel_name",
                "overprint_default",
                "sort_order",
                "created_at",
            ],
        ),
        (
            "templates",
            &[
                "id",
                "name",
                "category",
                "description",
                "template_path",
                "width_mm",
                "height_mm",
                "bleed_mm",
                "page_count",
                "color_mode",
                "thumbnail_png",
                "is_builtin",
                "favorite",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "workspace_layouts",
            &[
                "id",
                "name",
                "description",
                "dock_tree_json",
                "toolbox_config_json",
                "shortcuts_json",
                "is_builtin",
                "is_active",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "font_index",
            &[
                "id",
                "file_path",
                "face_index",
                "family_name",
                "subfamily_name",
                "postscript_name",
                "weight",
                "italic",
                "is_variable",
                "supports_arabic_rtl",
                "supports_kashida",
                "collection_tag",
                "favorite",
                "file_mtime_epoch",
                "indexed_at",
            ],
        ),
        (
            "brush_presets",
            &[
                "id",
                "name",
                "engine_kind",
                "category",
                "size_px",
                "hardness",
                "spacing",
                "opacity",
                "pressure_curve_json",
                "params_json",
                "is_builtin",
                "sort_order",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "app_preferences",
            &["key", "value_json", "category", "updated_at"],
        ),
    ];

    for (table, cols) in expected_columns {
        let mut stmt = db
            .connection()
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let actual_cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let expected_vec: Vec<String> = cols.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(
            actual_cols, expected_vec,
            "column mismatch on table {table}"
        );
    }

    // CHECK constraint enforcement: invalid file_format rejected
    let bad_recent = RecentDocumentRecord {
        id: "bad-1".into(),
        file_path: "/tmp/bad.xyz".into(),
        display_name: "Bad".into(),
        file_format: "xyz".into(),
        cdr_version: None,
        page_count: 1,
        color_mode: "cmyk".into(),
        thumbnail_png: None,
        file_size_bytes: 10,
        pinned: false,
        last_opened_at: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    assert!(matches!(
        db.upsert_recent_document(&bad_recent),
        Err(StorageError::Sqlite(_))
    ));
}

#[test]
fn multi_snapshot_crash_recovery_replays_only_post_snapshot_commands_and_purges_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("recovery.db");
    let db = StorageDb::open(&db_path).unwrap();

    let mut doc = Document::new_a4("Packaging Box");
    let doc_id = doc.id.to_string();

    // Snapshot 1 (empty document, seq 0)
    let snap1 = db
        .save_snapshot(
            &doc_id,
            &doc.title,
            Some(Path::new("/work/box.coree")),
            &doc,
        )
        .unwrap();
    assert!(Path::new(&snap1.snapshot_blob_path).exists());

    // Apply commands 1 and 2
    let op1 = CommandOp::create_object(
        SceneObject::new(
            ObjectId(0),
            "Front Panel",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 10.0, 100.0, 60.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::CYAN)),
        None,
    );
    let obj1_id = ObjectId(doc.next_id);
    apply_command(&mut doc, &op1).unwrap();
    db.append_command(&doc_id, 1, &op1).unwrap();

    let op2 = CommandOp::SetOutline {
        ids: vec![obj1_id],
        outline: OutlineStyle::cut_contour(),
    };
    apply_command(&mut doc, &op2).unwrap();
    db.append_command(&doc_id, 2, &op2).unwrap();

    // Snapshot 2 (captures seq_no == 2!)
    let snap2 = db
        .save_snapshot(
            &doc_id,
            &doc.title,
            Some(Path::new("/work/box.coree")),
            &doc,
        )
        .unwrap();
    assert_eq!(snap2.last_command_seq, 2);

    // Apply commands 3 and 4 AFTER snapshot 2, then simulate crash by dropping DB handle
    let op3 = CommandOp::TransformObjects {
        ids: vec![obj1_id],
        affine: Affine2D::translate(15.0, 20.0),
    };
    apply_command(&mut doc, &op3).unwrap();
    db.append_command(&doc_id, 3, &op3).unwrap();

    let op4 = CommandOp::AddPage {
        name: "Back".into(),
        width_mm: 210.0,
        height_mm: 297.0,
        bleed_mm: 3.0,
    };
    apply_command(&mut doc, &op4).unwrap();
    db.append_command(&doc_id, 4, &op4).unwrap();
    drop(db);

    // Reopen DB after crash
    let db2 = StorageDb::open(&db_path).unwrap();
    let unclean = db2.list_unclean_sessions().unwrap();
    assert_eq!(unclean.len(), 1);
    assert_eq!(unclean[0].latest_snapshot_id, snap2.id);
    assert_eq!(unclean[0].pending_journal_ops, 2);

    let recovered = db2.recover_document(&doc_id).unwrap();
    assert_eq!(recovered.pages.len(), 2);
    assert_eq!(recovered.pages[1].name, "Back");
    assert_eq!(recovered.objects.len(), 1);
    assert!(recovered.objects[&obj1_id].outline.is_hairline);
    let bounds = recovered.object_bounds(obj1_id).unwrap();
    assert!((bounds.min_x - 25.0).abs() < 1e-6);
    assert!((bounds.min_y - 30.0).abs() < 1e-6);

    db2.mark_clean_close(&doc_id).unwrap();
    assert!(db2.list_unclean_sessions().unwrap().is_empty());

    db2.purge_document_autosaves(&doc_id).unwrap();
    assert!(!Path::new(&snap1.snapshot_blob_path).exists());
    assert!(!Path::new(&snap2.snapshot_blob_path).exists());
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ColorMgmtPref {
    cmyk_profile: String,
    rendering_intent: String,
    simulate_overprint: bool,
}

#[test]
fn assets_preferences_and_real_font_directory_smoke() {
    let mut db = StorageDb::open_in_memory().unwrap();

    // Complex typed preference round-trip
    let pref = ColorMgmtPref {
        cmyk_profile: "FOGRA39".into(),
        rendering_intent: "RelativeColorimetric".into(),
        simulate_overprint: true,
    };
    db.set_preference("color_mgmt", "color_management", &pref)
        .unwrap();
    assert_eq!(
        db.get_preference::<ColorMgmtPref>("color_mgmt").unwrap(),
        Some(pref)
    );

    // Palette roundtrip + workspace layout switch + template + brush
    let mut pal = ColorPalette::new("Spot Cutters", ColorMode::Cmyk);
    pal.add_swatch(ColorSwatch::new("CutContour", Color::cut_contour_spot()));
    pal.add_swatch(ColorSwatch::new(
        "Crease",
        Color::spot("Crease", 1.0, [1.0, 0.0, 0.0, 0.0]),
    ));
    let pid = db
        .save_core_palette(&pal, "cutcontour", false, true)
        .unwrap();
    let loaded_pal = db.load_core_palette(&pid).unwrap();
    assert_eq!(loaded_pal.swatches.len(), 2);

    db.upsert_workspace_layout(&WorkspaceLayoutRecord {
        id: "ws-1".into(),
        name: "Default".into(),
        description: String::new(),
        dock_tree_json: "{}".into(),
        toolbox_config_json: "{}".into(),
        shortcuts_json: "{}".into(),
        is_builtin: true,
        is_active: true,
    })
    .unwrap();
    db.upsert_workspace_layout(&WorkspaceLayoutRecord {
        id: "ws-2".into(),
        name: "Illustration".into(),
        description: String::new(),
        dock_tree_json: "{}".into(),
        toolbox_config_json: "{}".into(),
        shortcuts_json: "{}".into(),
        is_builtin: true,
        is_active: false,
    })
    .unwrap();
    db.set_active_workspace_layout("ws-2").unwrap();
    assert_eq!(
        db.get_active_workspace_layout().unwrap().unwrap().id,
        "ws-2"
    );

    db.upsert_template(&TemplateRecord {
        id: "tpl-arabic".into(),
        name: "Arabic Brochure".into(),
        category: "arabic-rtl".into(),
        description: "Tri-fold RTL brochure".into(),
        template_path: "/templates/arabic_brochure.coree".into(),
        width_mm: 297.0,
        height_mm: 210.0,
        bleed_mm: 3.0,
        page_count: 2,
        color_mode: "cmyk".into(),
        thumbnail_png: None,
        is_builtin: true,
        favorite: false,
    })
    .unwrap();
    assert_eq!(db.list_templates(Some("arabic-rtl")).unwrap().len(), 1);

    db.upsert_brush_preset(&BrushPresetRecord {
        id: "br-1".into(),
        name: "Soft Airbrush".into(),
        engine_kind: "raster_paint".into(),
        category: "photo".into(),
        size_px: 48.0,
        hardness: 0.2,
        spacing: 0.1,
        opacity: 0.65,
        pressure_curve_json: "[[0.0,0.0],[1.0,1.0]]".into(),
        params_json: "{}".into(),
        is_builtin: true,
        sort_order: 0,
    })
    .unwrap();
    assert_eq!(
        db.list_brush_presets(Some("raster_paint")).unwrap().len(),
        1
    );

    // OS standard font directories exist and real font scan completes safely
    let dirs = standard_font_directories();
    assert!(!dirs.is_empty());
    let report = db.scan_system_fonts().unwrap();
    if report.faces_indexed > 0 {
        let all = db
            .search_fonts(&FontQuery {
                limit: Some(10),
                ..FontQuery::default()
            })
            .unwrap();
        assert!(!all.is_empty());
    }
}

#[test]
fn job_pool_runs_concurrent_jobs_non_blocking_and_cancels_cleanly() {
    let pool = JobPool::default_pool().unwrap();
    assert!(pool.worker_count() >= 2);

    // Submit 4 parallel jobs
    let mut handles = Vec::new();
    for idx in 0..4usize {
        handles.push(pool.submit(format!("Job {idx}"), move |ctx| {
            ctx.set_progress(50);
            let sum: usize = (0..10_000).map(|v| v + idx).sum();
            ctx.set_progress(100);
            Ok(sum)
        }));
    }
    for (idx, h) in handles.into_iter().enumerate() {
        let expected: usize = (0..10_000).map(|v| v + idx).sum();
        assert_eq!(h.wait(), JobOutcome::Completed(expected));
    }

    // Cancel a spinning job cooperatively and observe JobState::Cancelled
    let entered = Arc::new(AtomicBool::new(false));
    let entered_worker = Arc::clone(&entered);
    let cancel_handle = pool.submit("Heavy Export", move |ctx| {
        entered_worker.store(true, Ordering::Release);
        while !ctx.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    });

    while !entered.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(cancel_handle.try_recv().is_none());
    cancel_handle.cancel();
    let outcome = cancel_handle.wait_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(outcome, JobOutcome::Cancelled);

    // Error propagation job
    let err_handle = pool.submit::<(), _>("Broken File", |_ctx| Err("corrupt header".into()));
    let res = err_handle.wait();
    assert_eq!(res, JobOutcome::Failed("corrupt header".into()));

    // Verify JobState enum values
    assert_ne!(JobState::Queued, JobState::Completed);
}

#[test]
fn regression_5_reopen_after_clean_close_session_apis_and_bounds() {
    let db = StorageDb::open_in_memory().unwrap();
    let mut doc = Document::new_a4("Reopen Test");
    let doc_id = doc.id.to_string();

    // Session 1: snapshot + op seq 1 + clean close
    let snap1 = db.save_snapshot(&doc_id, &doc.title, None, &doc).unwrap();
    let op1 = CommandOp::AddPage {
        name: "Session 1 Page".into(),
        width_mm: 210.0,
        height_mm: 297.0,
        bleed_mm: 3.0,
    };
    apply_command(&mut doc, &op1).unwrap();
    db.append_command(&doc_id, 1, &op1).unwrap();
    db.mark_clean_close(&doc_id).unwrap();
    assert!(db.list_unclean_sessions().unwrap().is_empty());

    // Session 2: reopen the SAME doc_id and start seq=1 again (must NOT hit UNIQUE collision!)
    let snap2 = db.save_snapshot(&doc_id, &doc.title, None, &doc).unwrap();
    assert_ne!(snap1.session_id, snap2.session_id);
    assert_eq!(snap2.last_command_seq, 0);

    let op2 = CommandOp::AddPage {
        name: "Session 2 Page".into(),
        width_mm: 100.0,
        height_mm: 100.0,
        bleed_mm: 0.0,
    };
    db.append_command(&doc_id, 1, &op2).unwrap();

    let recovered = db.recover_document(&doc_id).unwrap();
    assert_eq!(recovered.pages.len(), 3);
    assert_eq!(recovered.pages[2].name, "Session 2 Page");

    // Session-scoped APIs: mark_session_clean_close & recover_session
    let recovered_s1 = db.recover_session(&doc_id, &snap1.session_id).unwrap();
    assert_eq!(recovered_s1.pages.len(), 2);
    assert_eq!(
        db.mark_session_clean_close(&doc_id, &snap2.session_id)
            .unwrap(),
        1
    );
    assert!(db.list_unclean_sessions().unwrap().is_empty());

    // Session 3: reopen calling append_command BEFORE save_snapshot (must also rotate cleanly)
    let op3 = CommandOp::AddPage {
        name: "Session 3 Pre-Snap".into(),
        width_mm: 50.0,
        height_mm: 50.0,
        bleed_mm: 0.0,
    };
    db.append_command(&doc_id, 1, &op3).unwrap();
    let snap3 = db.save_snapshot(&doc_id, &doc.title, None, &doc).unwrap();
    assert_ne!(snap3.session_id, snap2.session_id);
    assert_eq!(snap3.last_command_seq, 1);

    // Bounds validation on u64 -> i64 parameters
    assert!(matches!(
        db.save_snapshot_for_session(&doc_id, "s3", "T", None, &doc, u64::MAX),
        Err(StorageError::InvalidArgument(_))
    ));
    assert!(matches!(
        db.list_journal_entries(&doc_id, None, u64::MAX),
        Err(StorageError::InvalidArgument(_))
    ));
}

#[test]
fn regression_6_palette_nan_sanitization_and_workspace_active_invariant() {
    let mut db = StorageDb::open_in_memory().unwrap();

    // NaN / Inf color channels must be sanitized to finite values, never bound as SQL NULL
    let mut pal = ColorPalette::new("Sanitized Palette", ColorMode::Cmyk);
    pal.add_swatch(ColorSwatch::new(
        "Broken CMYK",
        Color::Cmyk {
            c: f32::NAN,
            m: f32::INFINITY,
            y: f32::NEG_INFINITY,
            k: 0.5,
            a: 1.0,
        },
    ));
    pal.add_swatch(ColorSwatch::new(
        "Broken Lab",
        Color::Lab {
            l: f32::NAN,
            a_ch: f32::INFINITY,
            b_ch: f32::NEG_INFINITY,
            alpha: 1.0,
        },
    ));
    let pid = db.save_core_palette(&pal, "cmyk", false, false).unwrap();
    let rows = db.list_palette_swatches(&pid).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].c, Some(0.0));
    assert_eq!(rows[0].m, Some(0.0));
    assert_eq!(rows[0].y, Some(0.0));
    assert_eq!(rows[0].k, Some(50.0));
    assert_eq!(rows[1].lab_l, Some(0.0));
    assert_eq!(rows[1].lab_a, Some(0.0));
    assert_eq!(rows[1].lab_b, Some(0.0));

    // Workspace layout active invariant
    db.upsert_workspace_layout(&WorkspaceLayoutRecord {
        id: "ws-a".into(),
        name: "Layout A".into(),
        description: String::new(),
        dock_tree_json: "{}".into(),
        toolbox_config_json: "{}".into(),
        shortcuts_json: "{}".into(),
        is_builtin: false,
        is_active: true,
    })
    .unwrap();
    // Setting nonexistent ID must return Ok(false) AND preserve Layout A as active!
    assert!(!db.set_active_workspace_layout("ws-does-not-exist").unwrap());
    assert_eq!(
        db.get_active_workspace_layout().unwrap().unwrap().id,
        "ws-a"
    );

    // Upserting Layout B with is_active = true must clear Layout A atomically
    db.upsert_workspace_layout(&WorkspaceLayoutRecord {
        id: "ws-b".into(),
        name: "Layout B".into(),
        description: String::new(),
        dock_tree_json: "{}".into(),
        toolbox_config_json: "{}".into(),
        shortcuts_json: "{}".into(),
        is_builtin: false,
        is_active: true,
    })
    .unwrap();
    let all = db.list_workspace_layouts().unwrap();
    let active_count = all.iter().filter(|l| l.is_active).count();
    assert_eq!(active_count, 1);
    assert_eq!(
        db.get_active_workspace_layout().unwrap().unwrap().id,
        "ws-b"
    );
}

#[test]
fn regression_7_symlinked_fonts_cycle_guard_and_large_limit() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real");
    let scan_dir = dir.path().join("scan");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::create_dir_all(&scan_dir).unwrap();

    // Minimal valid SFNT font with `name` table ("LinkFont")
    let mut name_tbl = Vec::new();
    let fam_u16: Vec<u8> = "LinkFont"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    name_tbl.extend_from_slice(&0u16.to_be_bytes());
    name_tbl.extend_from_slice(&1u16.to_be_bytes());
    name_tbl.extend_from_slice(&(6u16 + 12).to_be_bytes());
    for v in [3u16, 1, 0x0409, 1, fam_u16.len() as u16, 0] {
        name_tbl.extend_from_slice(&v.to_be_bytes());
    }
    name_tbl.extend_from_slice(&fam_u16);

    let mut ttf = Vec::new();
    ttf.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    ttf.extend_from_slice(&1u16.to_be_bytes());
    ttf.extend_from_slice(&16u16.to_be_bytes());
    ttf.extend_from_slice(&0u16.to_be_bytes());
    ttf.extend_from_slice(&0u16.to_be_bytes());
    ttf.extend_from_slice(b"name");
    ttf.extend_from_slice(&0u32.to_be_bytes());
    ttf.extend_from_slice(&28u32.to_be_bytes());
    ttf.extend_from_slice(&(name_tbl.len() as u32).to_be_bytes());
    ttf.extend_from_slice(&name_tbl);

    let real_font = real_dir.join("LinkFont.ttf");
    std::fs::write(&real_font, &ttf).unwrap();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real_font, scan_dir.join("Linked.ttf")).unwrap();
        // Also create a symlink loop back to scan_dir to verify cycle termination
        std::os::unix::fs::symlink(&scan_dir, scan_dir.join("loop_dir")).unwrap();
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(&real_font, scan_dir.join("Linked.ttf")).unwrap();
    }

    let db = StorageDb::open_in_memory().unwrap();
    let report = db
        .scan_font_directories(&[scan_dir], "symlink-test")
        .unwrap();
    assert_eq!(report.faces_indexed, 1);

    // Huge usize::MAX limit must clamp cleanly without SQLite overflow error
    let results = db
        .search_fonts(&FontQuery {
            family_query: Some("LinkFont".into()),
            limit: Some(usize::MAX),
            ..FontQuery::default()
        })
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].family_name, "LinkFont");

    // Stale face pruning: index a 2-face `.ttc`, then shrink the same file to a 1-face `.ttf`
    let ttc_path = dir.path().join("Shrinking.ttc");
    let mut ttc = Vec::new();
    ttc.extend_from_slice(b"ttcf"); // TTC tag
    ttc.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // major 1, minor 0
    ttc.extend_from_slice(&2u32.to_be_bytes()); // numFonts = 2
    let face0_off = 20u32;
    let face1_off = face0_off + (ttf.len() as u32);
    ttc.extend_from_slice(&face0_off.to_be_bytes());
    ttc.extend_from_slice(&face1_off.to_be_bytes());
    let mut face0 = ttf.clone();
    face0[20..24].copy_from_slice(&(face0_off + 28).to_be_bytes());
    let mut face1 = ttf.clone();
    face1[20..24].copy_from_slice(&(face1_off + 28).to_be_bytes());
    ttc.extend_from_slice(&face0);
    ttc.extend_from_slice(&face1);
    std::fs::write(&ttc_path, &ttc).unwrap();
    assert_eq!(db.index_font_file(&ttc_path, "shrink-test").unwrap(), 2);
    assert_eq!(
        db.search_fonts(&FontQuery {
            collection_tag: Some("shrink-test".into()),
            ..FontQuery::default()
        })
        .unwrap()
        .len(),
        2
    );
    std::fs::write(&ttc_path, &ttf).unwrap();
    assert_eq!(db.index_font_file(&ttc_path, "shrink-test").unwrap(), 1);
    let after_prune = db
        .search_fonts(&FontQuery {
            collection_tag: Some("shrink-test".into()),
            ..FontQuery::default()
        })
        .unwrap();
    assert_eq!(
        after_prune.len(),
        1,
        "stale face_index >= faces.len() must be pruned"
    );

    // cmap fmt4 idRangeOffset != 0 where raw_gid + idDelta == 0 (mod 65536) must NOT report Arabic!
    let mut cmap_tbl = Vec::new();
    // cmap header: version=0, numTables=1
    cmap_tbl.extend_from_slice(&0u16.to_be_bytes());
    cmap_tbl.extend_from_slice(&1u16.to_be_bytes());
    // Encoding record: platform=3 (Windows), encoding=1 (Unicode BMP), offset=12
    cmap_tbl.extend_from_slice(&3u16.to_be_bytes());
    cmap_tbl.extend_from_slice(&1u16.to_be_bytes());
    cmap_tbl.extend_from_slice(&12u32.to_be_bytes());
    // Format 4 subtable: 2 segments (0x0627..=0x0627 with range_val=4, delta=65531 (-5), raw_gid=5 -> 5 + (-5) == 0; plus sentinel 0xFFFF)
    let seg_count_x2 = 4u16;
    let fmt4_len = 14 + 2 + (4 * 2) + 2; // header(14) + reservedPad(2) + 4 arrays(2*2 each) + 1 glyphIdArray(2)
    cmap_tbl.extend_from_slice(&4u16.to_be_bytes()); // format
    cmap_tbl.extend_from_slice(&(fmt4_len as u16).to_be_bytes()); // length
    cmap_tbl.extend_from_slice(&0u16.to_be_bytes()); // language
    cmap_tbl.extend_from_slice(&seg_count_x2.to_be_bytes()); // segCountX2
    cmap_tbl.extend_from_slice(&4u16.to_be_bytes()); // searchRange
    cmap_tbl.extend_from_slice(&1u16.to_be_bytes()); // entrySelector
    cmap_tbl.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
                                                     // endCodes: 0x0627, 0xFFFF
    cmap_tbl.extend_from_slice(&0x0627u16.to_be_bytes());
    cmap_tbl.extend_from_slice(&0xFFFFu16.to_be_bytes());
    cmap_tbl.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
                                                     // startCodes: 0x0627, 0xFFFF
    cmap_tbl.extend_from_slice(&0x0627u16.to_be_bytes());
    cmap_tbl.extend_from_slice(&0xFFFFu16.to_be_bytes());
    // idDelta: (-5i16 as u16 = 65531), 1
    cmap_tbl.extend_from_slice(&((-5i16) as u16).to_be_bytes());
    cmap_tbl.extend_from_slice(&1u16.to_be_bytes());
    // idRangeOffset: 4 (points 4 bytes ahead past sentinel to glyphIdArray[0]), 0
    cmap_tbl.extend_from_slice(&4u16.to_be_bytes());
    cmap_tbl.extend_from_slice(&0u16.to_be_bytes());
    // glyphIdArray[0] = 5 -> (5 + (-5)) mod 65536 == 0 (missing glyph!)
    cmap_tbl.extend_from_slice(&5u16.to_be_bytes());

    let mut cmap_ttf = Vec::new();
    cmap_ttf.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    cmap_ttf.extend_from_slice(&1u16.to_be_bytes());
    cmap_ttf.extend_from_slice(&16u16.to_be_bytes());
    cmap_ttf.extend_from_slice(&0u16.to_be_bytes());
    cmap_ttf.extend_from_slice(&0u16.to_be_bytes());
    cmap_ttf.extend_from_slice(b"cmap");
    cmap_ttf.extend_from_slice(&0u32.to_be_bytes());
    cmap_ttf.extend_from_slice(&28u32.to_be_bytes());
    cmap_ttf.extend_from_slice(&(cmap_tbl.len() as u32).to_be_bytes());
    cmap_ttf.extend_from_slice(&cmap_tbl);

    let parsed_faces = coree_storage::inspect_font_bytes(&cmap_ttf, Path::new("ZeroDelta.ttf"));
    assert_eq!(parsed_faces.len(), 1);
    assert!(
        !parsed_faces[0].supports_arabic_rtl,
        "cmap fmt4 raw_gid + idDelta == 0 (mod 65536) must be treated as missing glyph 0"
    );
}
