// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Comprehensive headless CLI, studio state machine, 11-Lens, shortcut profile,
//! Find & Replace, crash-recovery autosave, Command Palette, theme tokens, and
//! full `egui 0.37` + `egui_dock 0.22` UI frame integration tests for `coree-app`.

use clap::Parser;
use coree_app::cli::{execute_cli, Cli, CliOutcome};
use coree_app::io_bridge::{import_document_into, read_text_file_bounded, UniversalExportOptions};
use coree_app::shortcuts::{resolve_shortcut, ShortcutAction, ShortcutProfile};
use coree_app::state::{
    BitmapDockerState, DockerTab, LensDockerState, StudioApp, ToolId, TOOL_GROUPS,
};
use coree_app::theme::{apply_theme, StudioLanguage, StudioTheme};
use coree_core::{
    Affine2D, AlignOp, AnchorPoint, ArrowheadStyle, ArtisticText, BezierPath, BooleanOp, Color,
    ColorMode, CommandOp, ConnectorObject, ConnectorStyle, CornerStyle, Document, Effect, Fill,
    GradientStop, GuideOrientation, LensKind, NodePayload, ObjectId, OutlineStyle, ParametricShape,
    Point2D, Rect2D, SceneObject, SymbolDef, TextOnPath, TextStyle, Vec2D, ZOrderOp,
};
use coree_prod::ReplaceAction;
use coree_storage::StorageDb;
use egui::{Key, Modifiers, RawInput, Rect};
use std::path::{Path, PathBuf};
use uuid::Uuid;

struct TempTestDir(PathBuf);

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("coree-app-{prefix}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_cli_args(args: &[&str]) -> String {
    let cli = Cli::try_parse_from(args).expect("CLI parse should succeed");
    match execute_cli(&cli).expect("CLI execution should succeed") {
        CliOutcome::Printed(out) => out,
        CliOutcome::LaunchGui(_) => panic!("expected printed CLI output, got LaunchGui"),
    }
}

// =============================================================================
// Part A: Production CLI End-to-End Tests
// =============================================================================

#[test]
fn cli_diagnostics_and_gui_launch_parsing() {
    let diag = run_cli_args(&["coree", "--diagnostics"]);
    assert!(diag.contains("coree"));
    assert!(diag.contains("MIT OR Apache-2.0"));
    assert!(diag.contains("coree-core"));
    assert!(diag.contains("coree-cdr"));
    assert!(diag.contains("coree-prod"));
    assert!(diag.contains(".cdr"));
    assert!(diag.contains("Local System Fonts Indexed:"));

    let gui_empty = Cli::try_parse_from(["coree"]).unwrap();
    assert_eq!(
        execute_cli(&gui_empty).unwrap(),
        CliOutcome::LaunchGui(vec![])
    );

    let gui_files = Cli::try_parse_from(["coree", "sample.cdr", "layout.coree"]).unwrap();
    assert_eq!(
        execute_cli(&gui_files).unwrap(),
        CliOutcome::LaunchGui(vec![
            PathBuf::from("sample.cdr"),
            PathBuf::from("layout.coree")
        ])
    );
}

#[test]
fn cli_generate_sample_inspect_and_universal_convert() {
    let tmp = TempTestDir::new("cli-convert");

    // 1. generate-sample across all supported formats
    let sample_coree = tmp.path().join("sample.coree");
    let sample_riff = tmp.path().join("sample_riff.cdr");
    let sample_x4 = tmp.path().join("sample_x4.cdr");
    let sample_x6 = tmp.path().join("sample_x6.cdr");
    let sample_cdt = tmp.path().join("sample.cdt");
    let sample_cmx = tmp.path().join("sample.cmx");
    let sample_svg = tmp.path().join("sample.svg");

    for (path, fmt) in [
        (&sample_coree, "coree"),
        (&sample_riff, "cdr-riff"),
        (&sample_x4, "cdr-x4"),
        (&sample_x6, "cdr-x6"),
        (&sample_cdt, "cdt"),
        (&sample_cmx, "cmx"),
        (&sample_svg, "svg"),
    ] {
        let p = path.to_string_lossy();
        run_cli_args(&["coree", "generate-sample", &p, "--format", fmt]);
        assert!(
            path.is_file(),
            "expected generated sample at {}",
            path.display()
        );
        assert!(std::fs::metadata(path).unwrap().len() > 64);
    }

    // 2. inspect on .cdr (text & --json), .coree, and .svg
    let insp_txt = run_cli_args(&["coree", "inspect", &sample_x6.to_string_lossy()]);
    assert!(insp_txt.contains("CDR ZIP/OPC"));
    assert!(insp_txt.contains("RIFF Chunk Tree"));

    let insp_json = run_cli_args(&["coree", "inspect", &sample_riff.to_string_lossy(), "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&insp_json).expect("valid inspect JSON");
    assert!(parsed["object_count"].as_u64().unwrap() > 0);
    assert!(parsed["page_count"].as_u64().unwrap() >= 1);

    let insp_coree = run_cli_args(&["coree", "inspect", &sample_coree.to_string_lossy()]);
    assert!(insp_coree.contains("Native .coree"));

    let insp_svg = run_cli_args(&["coree", "inspect", &sample_svg.to_string_lossy()]);
    assert!(insp_svg.contains("SVG"));

    // 3. convert to .cdr, .svg, .pdf (CMYK + crop marks), .eps, .dxf, .plt, .png, .jpg
    let out_cdr = tmp.path().join("conv.cdr");
    let out_svg = tmp.path().join("conv.svg");
    let out_pdf = tmp.path().join("conv.pdf");
    let out_eps = tmp.path().join("conv.eps");
    let out_dxf = tmp.path().join("conv.dxf");
    let out_plt = tmp.path().join("conv.plt");
    let out_png = tmp.path().join("conv.png");
    let out_jpg = tmp.path().join("conv.jpg");

    let src_str = sample_x6.to_string_lossy().into_owned();
    run_cli_args(&[
        "coree",
        "convert",
        &src_str,
        &out_cdr.to_string_lossy(),
        "--cdr-version",
        "riff",
    ]);
    run_cli_args(&["coree", "convert", &src_str, &out_svg.to_string_lossy()]);
    run_cli_args(&[
        "coree",
        "convert",
        &src_str,
        &out_pdf.to_string_lossy(),
        "--cmyk",
        "--crop-marks",
    ]);
    run_cli_args(&["coree", "convert", &src_str, &out_eps.to_string_lossy()]);
    run_cli_args(&["coree", "convert", &src_str, &out_dxf.to_string_lossy()]);
    run_cli_args(&["coree", "convert", &src_str, &out_plt.to_string_lossy()]);
    run_cli_args(&[
        "coree",
        "convert",
        &src_str,
        &out_png.to_string_lossy(),
        "--dpi",
        "72",
    ]);
    run_cli_args(&[
        "coree",
        "convert",
        &src_str,
        &out_jpg.to_string_lossy(),
        "--dpi",
        "72",
    ]);

    for p in [
        &out_cdr, &out_svg, &out_pdf, &out_eps, &out_dxf, &out_plt, &out_png, &out_jpg,
    ] {
        assert!(p.is_file(), "converted file missing: {}", p.display());
        assert!(std::fs::metadata(p).unwrap().len() > 32);
    }
    let pdf_bytes = std::fs::read(&out_pdf).unwrap();
    assert!(pdf_bytes.starts_with(b"%PDF-1.7"));
    let dxf_text = std::fs::read_to_string(&out_dxf).unwrap();
    assert!(dxf_text.contains("SECTION") && dxf_text.contains("EOF"));
    let plt_text = std::fs::read_to_string(&out_plt).unwrap();
    assert!(plt_text.starts_with("IN;"));
    let png_bytes = std::fs::read(&out_png).unwrap();
    assert!(png_bytes.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[test]
fn cli_render_preflight_print_merge_collect_trace_and_macro() {
    let tmp = TempTestDir::new("cli-prod");
    let sample = tmp.path().join("doc.coree");
    run_cli_args(&[
        "coree",
        "generate-sample",
        &sample.to_string_lossy(),
        "--format",
        "coree",
    ]);

    // 1. render single page (enhanced & wireframe) + multipage
    let png_enh = tmp.path().join("page0.png");
    let png_wire = tmp.path().join("page0_wire.png");
    let png_multi = tmp.path().join("multipage.png");
    run_cli_args(&[
        "coree",
        "render",
        &sample.to_string_lossy(),
        &png_enh.to_string_lossy(),
        "--page",
        "0",
        "--dpi",
        "72",
        "--view",
        "enhanced",
    ]);
    run_cli_args(&[
        "coree",
        "render",
        &sample.to_string_lossy(),
        &png_wire.to_string_lossy(),
        "--dpi",
        "72",
        "--view",
        "wireframe",
    ]);
    run_cli_args(&[
        "coree",
        "render",
        &sample.to_string_lossy(),
        &png_multi.to_string_lossy(),
        "--dpi",
        "48",
        "--multipage",
    ]);
    for p in [&png_enh, &png_wire, &png_multi] {
        let bytes = std::fs::read(p).unwrap();
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    // 2. preflight (plain text & JSON)
    let pf_txt = run_cli_args(&[
        "coree",
        "preflight",
        &sample.to_string_lossy(),
        "--min-dpi",
        "300",
    ]);
    assert!(pf_txt.contains("PREFLIGHT REPORT"));
    let pf_json = run_cli_args(&["coree", "preflight", &sample.to_string_lossy(), "--json"]);
    let pf_val: serde_json::Value = serde_json::from_str(&pf_json).unwrap();
    assert!(pf_val["page_count"].as_u64().unwrap() >= 1);

    // 3. print-merge with CSV placeholders
    let tpl_path = tmp.path().join("badge_tpl.coree");
    let mut tpl = Document::new("Badge", 90.0, 55.0, ColorMode::Cmyk, 300.0);
    let _ = tpl.add_object(
        SceneObject::new(
            ObjectId(0),
            "Name Field",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "<<Name>> — <<Role>>",
                TextStyle::default(),
            )),
        ),
        None,
    );
    coree_io::save_coree(&tpl, &tpl_path).unwrap();

    let csv_path = tmp.path().join("guests.csv");
    std::fs::write(
        &csv_path,
        "Name,Role\nAhmed,Designer\nسارة,مهندسة\nOmar,Printer\n",
    )
    .unwrap();

    let merged_out = tmp.path().join("merged.coree");
    let merge_msg = run_cli_args(&[
        "coree",
        "print-merge",
        &tpl_path.to_string_lossy(),
        &csv_path.to_string_lossy(),
        &merged_out.to_string_lossy(),
    ]);
    assert!(merge_msg.contains("Merged 3 record(s)"));
    let merged_doc = coree_io::load_coree(&merged_out).unwrap();
    assert_eq!(merged_doc.pages().len(), 3);

    // 4. collect
    let bundle_dir = tmp.path().join("handoff_bundle");
    let collect_msg = run_cli_args(&[
        "coree",
        "collect",
        &sample.to_string_lossy(),
        &bundle_dir.to_string_lossy(),
    ]);
    assert!(collect_msg.contains("Collected"));
    assert!(bundle_dir.join("manifest.json").is_file());
    assert!(bundle_dir.join("preflight.txt").is_file());

    // 5. trace (outline & centerline on rendered PNG)
    let traced_svg = tmp.path().join("traced.svg");
    let trace_msg = run_cli_args(&[
        "coree",
        "trace",
        &png_enh.to_string_lossy(),
        &traced_svg.to_string_lossy(),
        "--mode",
        "outline",
        "--colors",
        "6",
    ]);
    assert!(trace_msg.contains("Traced"));
    assert!(traced_svg.is_file());

    let traced_dxf = tmp.path().join("traced_center.dxf");
    run_cli_args(&[
        "coree",
        "trace",
        &png_enh.to_string_lossy(),
        &traced_dxf.to_string_lossy(),
        "--mode",
        "centerline",
        "--colors",
        "2",
    ]);
    assert!(traced_dxf.is_file());

    // 6. run-macro (built-in ID + custom .rhai file)
    let macro_out = tmp.path().join("recolored.coree");
    let macro_msg = run_cli_args(&[
        "coree",
        "run-macro",
        &sample.to_string_lossy(),
        "recolor-cmyk",
        &macro_out.to_string_lossy(),
    ]);
    assert!(macro_msg.contains("Macro executed"));
    assert!(macro_out.is_file());

    let custom_rhai = tmp.path().join("custom.rhai");
    std::fs::write(
        &custom_rhai,
        r#"let ids = all_object_ids(); log("Count=" + ids.len());"#,
    )
    .unwrap();
    let custom_out = tmp.path().join("custom_out.coree");
    let custom_msg = run_cli_args(&[
        "coree",
        "run-macro",
        &sample.to_string_lossy(),
        &custom_rhai.to_string_lossy(),
        &custom_out.to_string_lossy(),
    ]);
    assert!(custom_msg.contains("Count="));
    assert!(custom_out.is_file());
}

// =============================================================================
// Part B: Headless StudioApp State Machine, Tools, Shortcuts, 11 Lenses & Recovery
// =============================================================================

#[test]
fn studio_app_tools_shortcuts_both_profiles_and_15_groups() {
    assert_eq!(
        TOOL_GROUPS.len(),
        15,
        "must expose all 15 Toolbox Flyout groups"
    );

    let mut app = StudioApp::new_headless();
    assert_eq!(app.shortcut_profile, ShortcutProfile::ClassicStudio);

    // Verify Classic Studio shortcuts
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::F6,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::SelectRectangle)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::F7,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::SelectEllipse)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::F8,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::SelectArtisticText)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::F10,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::SelectShapeNode)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::Space,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::TogglePickSpace)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::P,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::Align(AlignOp::CenterPage))
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::C,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::Align(AlignOp::CenterH))
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::Space,
            Modifiers::CTRL,
            false
        ),
        Some(ShortcutAction::ToggleCommandPalette)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::PageUp,
            Modifiers::SHIFT,
            false
        ),
        Some(ShortcutAction::ZOrder(ZOrderOp::BringToFront))
    );

    // Spacebar toggles Pick <-> previous drawing tool
    app.handle_shortcut(ShortcutAction::SelectRectangle);
    assert_eq!(app.active_tool, ToolId::Rectangle);
    app.handle_shortcut(ShortcutAction::TogglePickSpace);
    assert_eq!(app.active_tool, ToolId::Pick);
    app.handle_shortcut(ShortcutAction::TogglePickSpace);
    assert_eq!(app.active_tool, ToolId::Rectangle);

    // Switch to StandardVector profile and verify Inkscape/Illustrator-style bindings
    app.set_shortcut_profile(ShortcutProfile::StandardVector);
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::V,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::SelectPick)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::N,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::SelectShapeNode)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::R,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::SelectRectangle)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::K,
            Modifiers::CTRL,
            false
        ),
        Some(ShortcutAction::ToggleCommandPalette)
    );
    // Single-letter shortcuts suppressed while text input is focused
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::R,
            Modifiers::NONE,
            true
        ),
        None
    );

    // Exercise interactive drawing commit across multiple tool types
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 40.0, 30.0));
    app.select_tool(ToolId::Ellipse);
    app.commit_drawn_box(Rect2D::from_xywh(25.0, 15.0, 35.0, 35.0));
    app.select_tool(ToolId::Polygon);
    app.commit_drawn_box(Rect2D::from_xywh(70.0, 20.0, 30.0, 30.0));
    app.insert_artistic_text_at(Point2D::new(20.0, 80.0));

    assert_eq!(app.active_tab().unwrap().document.objects().len(), 4);

    // Select all -> Group -> Ungroup -> Boolean Weld
    app.select_all();
    assert_eq!(app.active_tab().unwrap().selection.len(), 4);
    app.group_selection();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 5);
    let grp_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    let tab = app.active_tab_mut().unwrap();
    tab.selection.select_single(&tab.document, grp_id);
    app.ungroup_selection();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 4);
}

#[test]
fn studio_app_all_eleven_lenses_find_replace_and_command_palette() {
    let mut app = StudioApp::new_headless();

    // Add a closed rectangle and a text object
    let rect = SceneObject::new(
        ObjectId(0),
        "Lens Host",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 60.0,
            height: 40.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.5));
    app.execute_command(CommandOp::create_object(rect, None))
        .unwrap();
    let host_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();
    let tab = app.active_tab_mut().unwrap();
    tab.selection.select_single(&tab.document, host_id);

    // Apply ALL 11 Lenses via LensDockerState with Frozen + Viewpoint + RemoveFace flags!
    assert_eq!(LensDockerState::NAMES.len(), LensKind::VARIANT_COUNT);
    for idx in 0..11 {
        app.lens_state.selected_index = idx;
        app.lens_state.frozen = idx % 2 == 0;
        app.lens_state.use_viewpoint = idx % 3 == 0;
        app.lens_state.viewpoint_x = 25.0 + idx as f64;
        app.lens_state.viewpoint_y = 15.0;
        app.lens_state.remove_face = idx == 10;
        let cfg = app.lens_state.build_config();
        assert_eq!(cfg.kind.name(), LensDockerState::NAMES[idx]);
        app.apply_lens_to_selection(Some(cfg.clone()));

        let applied = app
            .active_tab()
            .unwrap()
            .document
            .object(host_id)
            .unwrap()
            .lens
            .as_ref()
            .expect("lens must be applied");
        assert_eq!(applied.kind.name(), LensDockerState::NAMES[idx]);
        assert_eq!(applied.frozen, cfg.frozen);
        assert_eq!(applied.remove_face, cfg.remove_face);
    }

    // Add Arabic + English text and verify Find & Replace
    app.tool_settings.text_insert_draft = "Hello Studio مرحبا".to_owned();
    app.insert_artistic_text_at(Point2D::new(15.0, 60.0));

    app.find_replace_state.text_query = "Hello".to_owned();
    let matches = app.run_find_select();
    assert_eq!(matches.len(), 1);

    let rep = app.run_find_and_replace_action(ReplaceAction::ReplaceText {
        find: "Hello".to_owned(),
        replace_with: "Welcome".to_owned(),
        case_sensitive: true,
    });
    assert_eq!(rep.replacements_applied, 1);

    // Verify Command Palette fuzzy search & execution
    let lens_hits = app.search_command_palette("fish eye");
    assert!(lens_hits.iter().any(|c| c.id == "lens.4"));
    let weld_hits = app.search_command_palette("weld");
    assert!(weld_hits.iter().any(|c| c.id == "bool.weld"));
    assert!(app.execute_palette_command("theme.contrast"));
    assert_eq!(app.theme, StudioTheme::HighContrast);
    assert!(app.execute_palette_command("docker.Macros"));
    assert!(app.dock_state.find_tab(&DockerTab::Macros).is_some());
}

#[test]
fn studio_app_undo_redo_and_sqlite_crash_recovery() {
    let tmp = TempTestDir::new("crash-recovery");
    let db_path = tmp.path().join("studio.db");

    let expected_doc_id: String;
    {
        let db = StorageDb::open(&db_path).expect("open sqlite db");
        let mut app = StudioApp::new_with_storage(db);
        expected_doc_id = app.active_tab().unwrap().document.id().to_string();

        // Execute 2 undoable commands (each appends to WAL command_journal)
        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 50.0, 30.0));
        app.commit_drawn_box(Rect2D::from_xywh(70.0, 10.0, 30.0, 30.0));
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);

        // Undo & Redo work deterministically
        app.undo().unwrap();
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 1);
        app.redo().unwrap();
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);

        // Drop app WITHOUT calling `shutdown_cleanly()` -> simulates sudden crash!
    }

    // Reopen fresh StudioApp on the same DB -> crash recovery detects unclean session!
    let db2 = StorageDb::open(&db_path).expect("reopen sqlite db");
    let mut recovered_app = StudioApp::new_with_storage(db2);
    assert!(
        !recovered_app.unclean_sessions.is_empty(),
        "must detect unclean session on startup"
    );
    let sess = recovered_app
        .unclean_sessions
        .iter()
        .find(|s| s.document_id == expected_doc_id)
        .cloned()
        .expect("matching unclean session");

    recovered_app
        .recover_unclean_session(&sess.document_id, &sess.session_id)
        .expect("recovery replay succeeds");
    assert_eq!(
        recovered_app.active_tab().unwrap().document.objects().len(),
        2
    );
}

#[test]
fn theme_tokens_match_design_spec_and_headless_egui_frames_render_cleanly() {
    // Verify exact WCAG AA Dark Studio tokens from replica/design/tokens.json
    let dark = StudioTheme::DarkStudio.tokens();
    assert_eq!(dark.bg, egui::Color32::from_rgb(0x11, 0x13, 0x18));
    assert_eq!(dark.canvas_well, egui::Color32::from_rgb(0x0c, 0x0e, 0x12));
    assert_eq!(dark.surface, egui::Color32::from_rgb(0x19, 0x1c, 0x24));
    assert_eq!(
        dark.surface_elevated,
        egui::Color32::from_rgb(0x23, 0x27, 0x34)
    );
    assert_eq!(dark.border, egui::Color32::from_rgb(0x2e, 0x34, 0x45));
    assert_eq!(dark.text, egui::Color32::from_rgb(0xf1, 0xf4, 0xf9));
    assert_eq!(dark.text_muted, egui::Color32::from_rgb(0xad, 0xb6, 0xca));
    assert_eq!(dark.accent, egui::Color32::from_rgb(0x3e, 0xa6, 0xff));
    assert_eq!(
        dark.accent_secondary,
        egui::Color32::from_rgb(0xf6, 0xad, 0x3c)
    );
    assert_eq!(dark.danger, egui::Color32::from_rgb(0xff, 0x70, 0x78));
    assert_eq!(dark.success, egui::Color32::from_rgb(0x38, 0xd9, 0xa9));
    assert_eq!(dark.warning, egui::Color32::from_rgb(0xfb, 0xbf, 0x24));
    assert_eq!(dark.cutcontour, egui::Color32::from_rgb(0xff, 0x66, 0xc4));

    // Apply all 3 themes & render full StudioApp UI headlessly through egui 0.37!
    let ctx = egui::Context::default();
    for theme in StudioTheme::ALL {
        apply_theme(&ctx, theme);
    }

    let mut app = StudioApp::new_headless();
    app.new_sample_document();
    app.fonts_configured = true; // avoid scanning OS fonts repeatedly in test loop

    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        ..RawInput::default()
    };

    // Frame 1: Welcome Hub (`S01`)
    app.show_welcome = true;
    let mut out1 = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
    assert!(!out1.shapes.is_empty());
    out1.textures_delta.clear();

    // Frame 2: Main Studio Canvas (`S02`) with Toolbox, Property Bar, Palette Bar, Rulers, Status Bar & Dockers
    app.show_welcome = false;
    let mut out2 = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
    assert!(!out2.shapes.is_empty());
    out2.textures_delta.clear();

    // Frame 3: Command Palette + New Document + Export + About + Preferences modals + every Docker tab!
    app.command_palette.open = true;
    app.new_doc_dialog.open = true;
    app.export_dialog.open = true;
    app.show_about_dialog = true;
    app.show_preferences_dialog = true;
    app.show_print_preview_dialog = true;
    for docker in DockerTab::ALL {
        app.focus_docker(docker);
        let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();
    }

    // Exercise Production helpers on showcase document
    app.generate_cut_contour_active();
    app.generate_dieline_active();
    app.insert_barcode_active();
    app.insert_qr_active();
    app.run_preflight_active();
    app.compute_separations_active();
    app.apply_boolean_op(BooleanOp::CreateBoundary);
    app.shutdown_cleanly();
}

#[test]
fn studio_app_vector_effects_bitmap_studio_trace_macros_and_async_jobs() {
    let tmp = TempTestDir::new("studio-workflows");
    let db = StorageDb::open(&tmp.path().join("studio.db")).expect("open sqlite db");
    let mut app = StudioApp::new_with_storage(db);

    // 1. Draw a shape and apply all 10 live Vector Effects
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(15.0, 15.0, 50.0, 35.0));
    let rect_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();
    let tab = app.active_tab_mut().unwrap();
    tab.selection.select_single(&tab.document, rect_id);

    for kind_idx in 0..10 {
        app.effects_state.selected_effect_kind = kind_idx;
        let eff = app.build_selected_effect();
        app.add_effect_to_selection(eff);
    }
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .effects
            .len(),
        10
    );
    app.remove_effect_on(rect_id, 0);
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .effects
            .len(),
        9
    );

    // 2. Convert selection to Bitmap -> apply Bitmap Studio filters -> Resample -> Vector Trace!
    app.convert_selection_to_bitmap();
    let raster_id = app
        .active_tab()
        .unwrap()
        .selection
        .objects
        .first()
        .copied()
        .expect("converted bitmap should be selected");
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(raster_id)
            .unwrap()
            .payload,
        NodePayload::Raster(_)
    ));

    // Exercise all 19 Bitmap Studio filters!
    assert_eq!(BitmapDockerState::FILTER_NAMES.len(), 19);
    for filter_idx in 0..19 {
        app.bitmap_state.filter_idx = filter_idx;
        let filter = app.bitmap_state.build_filter();
        app.apply_raster_filter_to_selection(filter);
    }
    // Undo the trailing destructive edge-collapse filters (Emboss/EdgeDetect/Threshold/etc.)
    // so the filtered bitmap retains traceable foreground contours, then resample & trace.
    for _ in 0..10 {
        app.undo().expect("undo raster filter");
    }
    app.resample_selected_bitmap(64, 48);
    let traced_count = app
        .trace_selected_raster()
        .expect("vector trace on selected bitmap must succeed");
    assert!(traced_count > 0);

    // 3. Macro Recorder -> record undoable ops -> replay Rhai script
    app.start_macro_recording();
    app.select_tool(ToolId::Ellipse);
    app.commit_drawn_box(Rect2D::from_xywh(80.0, 20.0, 25.0, 25.0));
    let recorded = app.stop_macro_recording();
    assert!(!recorded.trim().is_empty());
    let executed = app
        .run_rhai_script(r#"let ids = all_object_ids(); log("Macro saw " + ids.len());"#)
        .expect("run sandboxed rhai script");
    assert_eq!(executed, 0);

    // 4. Save As (.coree) -> Reopen in new tab -> Async JobPool Export (.svg) -> Close tab
    let saved_path = tmp.path().join("project_saved.coree");
    app.save_active_as(&saved_path)
        .expect("save active as .coree");
    assert!(saved_path.is_file());
    assert!(!app.active_tab().unwrap().dirty);

    let tabs_before = app.tabs.len();
    app.open_file_path(&saved_path)
        .expect("reopen saved .coree");
    assert_eq!(app.tabs.len(), tabs_before + 1);

    let async_svg = tmp.path().join("async_export.svg");
    app.export_active_async(async_svg.clone(), UniversalExportOptions::default());
    for _ in 0..500 {
        app.tick_background_jobs();
        if app.active_jobs.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        app.active_jobs.is_empty(),
        "background export job must finish"
    );
    assert!(async_svg.is_file(), "async exported SVG must exist on disk");

    app.close_tab(app.active_tab);
    app.shutdown_cleanly();
}

// =============================================================================
// Targeted Audit Regression Tests (Items 1–8)
// =============================================================================

#[test]
fn regression_wal_post_save_and_post_undo_crash_recovery() {
    let tmp = TempTestDir::new("wal-regressions");
    let db_path = tmp.path().join("studio.db");
    let saved_file = tmp.path().join("saved.coree");

    // Case A: Op1 -> Save -> Op2 -> Crash (post-save edit MUST survive crash recovery!)
    let doc_id_a: String;
    {
        let db = StorageDb::open(&db_path).expect("open db");
        let mut app = StudioApp::new_with_storage(db);
        doc_id_a = app.active_tab().unwrap().document.id().to_string();

        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
        app.save_active_as(&saved_file).expect("save active");
        assert_eq!(app.active_tab().unwrap().journal_seq, 0);
        assert!(!app.active_tab().unwrap().wal_session_open);

        // Post-save edit must open a fresh WAL session with a baseline snapshot
        app.commit_drawn_box(Rect2D::from_xywh(50.0, 10.0, 30.0, 20.0));
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);
        // Crash without shutdown_cleanly()
    }
    {
        let db = StorageDb::open(&db_path).expect("reopen db");
        let mut recovered = StudioApp::new_with_storage(db);
        let sess = recovered
            .unclean_sessions
            .iter()
            .find(|s| s.document_id == doc_id_a)
            .cloned()
            .expect("post-save crash must produce an unclean recovery session");
        recovered
            .recover_unclean_session(&sess.document_id, &sess.session_id)
            .expect("post-save recovery succeeds");
        assert_eq!(
            recovered.active_tab().unwrap().document.objects().len(),
            2,
            "post-save edit must be preserved on crash recovery"
        );
        recovered.shutdown_cleanly();
    }

    // Case B: Op1 -> Op2 -> Undo(Op2) -> Crash (undone Op2 MUST NOT resurrect!)
    let doc_id_b: String;
    {
        let db = StorageDb::open(&db_path).expect("open db");
        let mut app = StudioApp::new_with_storage(db);
        doc_id_b = app.active_tab().unwrap().document.id().to_string();

        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
        app.commit_drawn_box(Rect2D::from_xywh(50.0, 10.0, 30.0, 20.0));
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);

        app.undo().expect("undo op2");
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 1);
        // Crash right after undo!
    }
    {
        let db = StorageDb::open(&db_path).expect("reopen db");
        let mut recovered = StudioApp::new_with_storage(db);
        let sess = recovered
            .unclean_sessions
            .iter()
            .find(|s| s.document_id == doc_id_b)
            .cloned()
            .expect("unclean session after undo must exist");
        recovered
            .recover_unclean_session(&sess.document_id, &sess.session_id)
            .expect("post-undo recovery succeeds");
        assert_eq!(
            recovered.active_tab().unwrap().document.objects().len(),
            1,
            "undone edit must NOT resurrect after crash recovery"
        );

        // Verify duplicate_selection routes through CommandHistory (undoable) and zero-offset duplicate works
        let first_id = *recovered
            .active_tab()
            .unwrap()
            .document
            .objects()
            .keys()
            .next()
            .unwrap();
        let tab = recovered.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, first_id);
        let orig_bb = tab.document.object_bounds(first_id).unwrap();

        let dup_zero = recovered
            .duplicate_selection_with_offset(Vec2D::ZERO)
            .expect("zero-offset duplicate");
        assert_eq!(dup_zero.len(), 1);
        let dup_bb = recovered
            .active_tab()
            .unwrap()
            .document
            .object_bounds(dup_zero[0])
            .unwrap();
        assert!((dup_bb.min_x - orig_bb.min_x).abs() < 1e-6);
        assert!((dup_bb.min_y - orig_bb.min_y).abs() < 1e-6);

        recovered.undo().expect("undo duplicate_selection");
        assert_eq!(recovered.active_tab().unwrap().document.objects().len(), 1);
        recovered.shutdown_cleanly();
    }
}

#[test]
fn regression_local_coords_mesh_envelope_and_bitmap_padding() {
    let mut app = StudioApp::new_headless();

    // Create a 40x20 rectangle translated to world (100, 150)
    let rect = SceneObject::new(
        ObjectId(0),
        "Translated Rect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 40.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.5))
    .with_transform(Affine2D::translate(100.0, 150.0));
    app.execute_command(CommandOp::create_object(rect, None))
        .unwrap();
    let id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();
    let tab = app.active_tab_mut().unwrap();
    tab.selection.select_single(&tab.document, id);

    // 1. object_local_bounds returns local (0,0,40,20), NOT world (100,150,140,170)
    let local_bb = StudioApp::object_local_bounds(&app.active_tab().unwrap().document, id)
        .expect("local bounds");
    assert!((local_bb.min_x - 0.0).abs() < 1e-6);
    assert!((local_bb.min_y - 0.0).abs() < 1e-6);
    assert!((local_bb.width() - 40.0).abs() < 1e-6);
    assert!((local_bb.height() - 20.0).abs() < 1e-6);

    // 2. MeshFill emits exact rows * cols nodes in local space (clamped >= 2)
    let mesh = StudioApp::build_mesh_fill_for_bounds(local_bb, 3, 4);
    match mesh {
        Fill::MeshFill { rows, cols, nodes } => {
            assert_eq!(rows, 3);
            assert_eq!(cols, 4);
            assert_eq!(nodes.len(), 12);
            assert_eq!(nodes.last().unwrap().row, 2);
            assert_eq!(nodes.last().unwrap().col, 3);
            assert!((nodes.last().unwrap().pos.x - 40.0).abs() < 1e-6);
            assert!((nodes.last().unwrap().pos.y - 20.0).abs() < 1e-6);
        }
        other => panic!("expected MeshFill, got {other:?}"),
    }
    let clamped_mesh = StudioApp::build_mesh_fill_for_bounds(local_bb, 1, 1);
    match clamped_mesh {
        Fill::MeshFill { rows, cols, nodes } => {
            assert_eq!((rows, cols, nodes.len()), (2, 2, 4));
        }
        other => panic!("expected MeshFill, got {other:?}"),
    }

    // 3. Distort, Envelope (8-pt cage!), and Perspective use local coordinates
    app.effects_state.selected_effect_kind = 4; // Distort
    match app.build_selected_effect() {
        Effect::Distort { center, .. } => {
            assert!((center.x - 20.0).abs() < 1e-6);
            assert!((center.y - 10.0).abs() < 1e-6);
        }
        other => panic!("expected Distort, got {other:?}"),
    }

    app.effects_state.selected_effect_kind = 5; // Envelope
    match app.build_selected_effect() {
        Effect::Envelope { cage, .. } => {
            assert_eq!(
                cage.len(),
                8,
                "Envelope must emit 8-point [TL,T_mid,TR,R_mid,BR,B_mid,BL,L_mid] Coons cage"
            );
            assert!((cage[0].x - 0.0).abs() < 1e-6 && (cage[0].y - 0.0).abs() < 1e-6); // TL
            assert!((cage[2].x - 40.0).abs() < 1e-6 && (cage[2].y - 0.0).abs() < 1e-6); // TR
            assert!((cage[4].x - 40.0).abs() < 1e-6 && (cage[4].y - 20.0).abs() < 1e-6); // BR
            assert!((cage[6].x - 0.0).abs() < 1e-6 && (cage[6].y - 20.0).abs() < 1e-6);
            // BL
        }
        other => panic!("expected Envelope, got {other:?}"),
    }

    app.effects_state.selected_effect_kind = 8; // Perspective
    match app.build_selected_effect() {
        Effect::Perspective { quad } => {
            assert!(quad.iter().all(|p| p.x >= -1e-6 && p.x <= 40.0 + 1e-6));
            assert!(quad.iter().all(|p| p.y >= -1e-6 && p.y <= 20.0 + 1e-6));
        }
        other => panic!("expected Perspective, got {other:?}"),
    }

    // 4. convert_selection_to_bitmap accounts for 4.0 mm + spatial_bounds_padding_mm (+0.25 mm for 0.5 mm stroke)
    app.convert_selection_to_bitmap();
    let raster_id = app.active_tab().unwrap().selection.objects[0];
    let raster_obj = app
        .active_tab()
        .unwrap()
        .document
        .object(raster_id)
        .unwrap();
    match &raster_obj.payload {
        NodePayload::Raster(r) => {
            assert!((r.width_mm - 48.5).abs() < 1e-6, "expected 40 + 8.5 mm");
            assert!((r.height_mm - 28.5).abs() < 1e-6, "expected 20 + 8.5 mm");
        }
        other => panic!("expected Raster payload, got {other:?}"),
    }
    let origin = raster_obj.transform.transform_point(Point2D::ZERO);
    assert!((origin.x - 95.75).abs() < 1e-6, "expected 100 - 4.25 mm");
    assert!((origin.y - 145.75).abs() < 1e-6, "expected 150 - 4.25 mm");
}

#[test]
fn regression_import_cycle_guard_symbols_and_id_remapping() {
    let mut src = Document::new("Source", 100.0, 100.0, ColorMode::Cmyk, 300.0);

    // 1. Add a reusable Symbol definition
    let sym = SymbolDef::new(
        "StarBadge",
        vec![SceneObject::new(
            ObjectId(0),
            "SymRect",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 10.0, 10.0,
            ))),
        )],
    );
    let sym_id = sym.id;
    let mut src_hist = coree_core::CommandHistory::default();
    src_hist
        .execute(&mut src, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();
    let _sym_inst = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "StarBadgeInst",
                NodePayload::SymbolInstance(sym_id),
            ),
            None,
        )
        .unwrap();

    // 2. Add host path + TextOnPath + two shapes + Connector + Blend referencing them
    let host_id = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "HostPath",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(50.0, 0.0),
                )),
            ),
            None,
        )
        .unwrap();
    let shape_a = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ShapeA",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 10.0, 10.0,
                ))),
            ),
            None,
        )
        .unwrap();
    let shape_b = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ShapeB",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    20.0, 0.0, 10.0, 10.0,
                ))),
            ),
            None,
        )
        .unwrap();
    src_hist
        .execute(
            &mut src,
            CommandOp::AddEffect {
                id: shape_a,
                effect: Effect::Blend {
                    target_id: shape_b,
                    steps: 5,
                    rotation_deg: 0.0,
                    along_path: Some(host_id),
                },
            },
        )
        .unwrap();
    let _top_id = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TextOnHost",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("Curved", TextStyle::default()),
                    path_id: host_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            None,
        )
        .unwrap();
    let _conn_id = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Link",
                NodePayload::Connector(ConnectorObject::between_objects(
                    shape_a,
                    AnchorPoint::Right,
                    shape_b,
                    AnchorPoint::Left,
                    ConnectorStyle::Straight,
                )),
            ),
            None,
        )
        .unwrap();

    // 3. Inject an explicit cyclic Group pair (g1 <-> g2) into src via serde JSON to test cycle guard
    let g1 = src.alloc_id();
    let g2 = src.alloc_id();
    let p0_id = src.pages()[0].id;
    let mut doc_json = serde_json::to_value(&src).unwrap();
    let objs_map = doc_json["objects"].as_object_mut().unwrap();
    objs_map.insert(
        g1.to_string(),
        serde_json::to_value(SceneObject::new(
            ObjectId(g1),
            "CycleG1",
            NodePayload::Group(vec![ObjectId(g2)]),
        ))
        .unwrap(),
    );
    objs_map.insert(
        g2.to_string(),
        serde_json::to_value(SceneObject::new(
            ObjectId(g2),
            "CycleG2",
            NodePayload::Group(vec![ObjectId(g1)]),
        ))
        .unwrap(),
    );
    for page in doc_json["pages"].as_array_mut().unwrap() {
        if page["id"] == serde_json::to_value(p0_id).unwrap() {
            page["layers"][0]["objects"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(g1));
        }
    }
    let src: Document = serde_json::from_value(doc_json).unwrap();

    // Pre-seed target with 5 objects so every imported ID differs from src IDs
    let mut target = Document::new("Target", 210.0, 297.0, ColorMode::Cmyk, 300.0);
    for i in 0..5 {
        target
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("Seed {i}"),
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
                ),
                None,
            )
            .unwrap();
    }

    let imported = import_document_into(&mut target, &src);
    assert!(!imported.is_empty());
    assert!(
        target.symbols().contains_key(&sym_id),
        "symbols must be copied into target"
    );

    // Verify TextOnPath, Connector, and Blend references were remapped to NEW target IDs!
    let new_host = target
        .objects()
        .values()
        .find(|o| o.name == "HostPath")
        .map(|o| o.id)
        .unwrap();
    let new_a = target
        .objects()
        .values()
        .find(|o| o.name == "ShapeA")
        .map(|o| o.id)
        .unwrap();
    let new_b = target
        .objects()
        .values()
        .find(|o| o.name == "ShapeB")
        .map(|o| o.id)
        .unwrap();
    assert_ne!(new_host, host_id);
    assert_ne!(new_a, shape_a);
    assert_ne!(new_b, shape_b);

    let top_obj = target
        .objects()
        .values()
        .find(|o| o.name == "TextOnHost")
        .unwrap();
    if let NodePayload::TextOnPath(ref top) = top_obj.payload {
        assert_eq!(top.path_id, new_host, "TextOnPath.path_id must be remapped");
    } else {
        panic!("expected TextOnPath");
    }

    let conn_obj = target
        .objects()
        .values()
        .find(|o| o.name == "Link")
        .unwrap();
    if let NodePayload::Connector(ref c) = conn_obj.payload {
        assert_eq!(c.from_obj, Some(new_a));
        assert_eq!(c.to_obj, Some(new_b));
    } else {
        panic!("expected Connector");
    }

    let a_obj = target.object(new_a).unwrap();
    match &a_obj.effects[0] {
        Effect::Blend {
            target_id,
            along_path,
            ..
        } => {
            assert_eq!(*target_id, new_b);
            assert_eq!(*along_path, Some(new_host));
        }
        other => panic!("expected Blend effect, got {other:?}"),
    }
}

#[test]
fn regression_page_switch_delete_layer_sync_and_duplicate_page() {
    let mut app = StudioApp::new_headless();
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 40.0, 30.0));
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 1);
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 1);

    // Duplicate Page 1 -> Page 2 must contain a cloned object, and active_layer_id must point to Page 2
    app.duplicate_page(0);
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.pages().len(), 2);
        assert_eq!(tab.document.active_page_index(), 1);
        assert_eq!(
            tab.document.objects().len(),
            2,
            "duplicate_page must clone source page objects onto new page"
        );
        let p2_layers: Vec<_> = tab.document.pages()[1]
            .layers
            .iter()
            .map(|l| l.id)
            .collect();
        assert!(p2_layers.contains(&tab.active_layer_id.unwrap()));
    }

    // Switch back via PrevPage shortcut -> active_layer_id must sync to Page 1
    app.handle_shortcut(ShortcutAction::PrevPage);
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.active_page_index(), 0);
        let p1_layers: Vec<_> = tab.document.pages()[0]
            .layers
            .iter()
            .map(|l| l.id)
            .collect();
        assert!(p1_layers.contains(&tab.active_layer_id.unwrap()));
    }

    // Switch to Page 2 via NextPage and delete Page 2 -> active_layer_id must NOT be stale!
    app.handle_shortcut(ShortcutAction::NextPage);
    let p2_id = app.active_tab().unwrap().document.pages()[1].id;
    app.execute_command(CommandOp::DeletePage { page_id: p2_id })
        .expect("delete page 2");
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.pages().len(), 1);
        let p1_layers: Vec<_> = tab.document.pages()[0]
            .layers
            .iter()
            .map(|l| l.id)
            .collect();
        assert!(
            p1_layers.contains(&tab.active_layer_id.unwrap()),
            "active_layer_id must sync to surviving page after DeletePage"
        );
    }

    // Creating a new object on active_layer_id MUST succeed (no LayerNotFound!)
    let layer_id = app.active_tab().unwrap().active_layer_id;
    let obj = SceneObject::new(
        ObjectId(0),
        "Post-Delete Rect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            5.0, 5.0, 20.0, 20.0,
        ))),
    );
    app.execute_command(CommandOp::create_object(obj, layer_id))
        .expect("create_object after DeletePage must succeed");
}

#[test]
fn regression_ctrl_q_text_converts_to_real_glyph_curves() {
    let mut app = StudioApp::new_headless();
    app.tool_settings.text_insert_draft = "Coree Studio".to_owned();
    app.insert_artistic_text_at(Point2D::new(20.0, 40.0));
    let text_id = app.active_tab().unwrap().selection.objects[0];
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(text_id)
            .unwrap()
            .payload,
        NodePayload::ArtisticText(_)
    ));

    app.convert_selection_to_curves();
    match &app
        .active_tab()
        .unwrap()
        .document
        .object(text_id)
        .unwrap()
        .payload
    {
        NodePayload::Path(p) => {
            assert!(
                p.subpaths.len() > 1,
                "Ctrl+Q on ArtisticText must produce real multi-glyph curves (got {} subpath(s))",
                p.subpaths.len()
            );
        }
        other => panic!("expected Path after Ctrl+Q, got {other:?}"),
    }
}

#[test]
fn regression_cli_format_override_dpi_validation_and_bounded_read() {
    let tmp = TempTestDir::new("cli-regressions");

    // 1. `--format coree` writes an actual `.coree` archive even if filename ends with `.svg`
    let coree_with_svg_ext = tmp.path().join("explicit_coree.svg");
    run_cli_args(&[
        "coree",
        "generate-sample",
        &coree_with_svg_ext.to_string_lossy(),
        "--format",
        "coree",
    ]);
    let loaded = coree_io::load_coree(&coree_with_svg_ext)
        .expect("--format coree must write a valid .coree ZIP container regardless of extension");
    assert!(!loaded.objects().is_empty());

    // 2. Reject NaN / inf / zero / negative DPI on `render` (both single-page and multipage)
    for bad_dpi in ["NaN", "inf", "0", "-72"] {
        let out_png = tmp.path().join(format!("bad_{bad_dpi}.png"));
        let dpi_flag = format!("--dpi={bad_dpi}");
        let cli = Cli::try_parse_from([
            "coree",
            "render",
            &coree_with_svg_ext.to_string_lossy(),
            &out_png.to_string_lossy(),
            &dpi_flag,
            "--multipage",
        ])
        .expect("clap parses numeric string");
        assert!(
            execute_cli(&cli).is_err(),
            "render with {dpi_flag} must return Err"
        );
    }

    // 3. Oversized multipage render scales effective_dpi down cleanly to <= 16384 px
    let mut big_doc = Document::new("WidePages", 2000.0, 50.0, ColorMode::Cmyk, 300.0);
    for i in 2..=10 {
        big_doc.add_page(format!("Page {i}"), 2000.0, 50.0, 0.0);
    }
    let big_path = tmp.path().join("wide.coree");
    coree_io::save_coree(&big_doc, &big_path).unwrap();
    let wide_png = tmp.path().join("wide_multi.png");
    let msg = run_cli_args(&[
        "coree",
        "render",
        &big_path.to_string_lossy(),
        &wide_png.to_string_lossy(),
        "--multipage",
        "--dpi",
        "2400",
    ]);
    assert!(msg.contains("Rendered multipage"));
    assert!(
        !msg.contains("@ 2400.0 DPI"),
        "oversized multipage layout must scale effective DPI down instead of cropping: {msg}"
    );
    assert!(wide_png.is_file());

    // 4. Bounded text reader rejects files exceeding limit
    let small_file = tmp.path().join("bounded.txt");
    std::fs::write(&small_file, "0123456789abcdef").unwrap();
    assert!(read_text_file_bounded(&small_file, 16).is_ok());
    assert!(read_text_file_bounded(&small_file, 8).is_err());
}

#[test]
fn regression_shortcuts_gated_when_text_focused() {
    let gated_cases = [
        (ShortcutProfile::ClassicStudio, Key::Z, Modifiers::CTRL),
        (
            ShortcutProfile::ClassicStudio,
            Key::Z,
            Modifiers::CTRL | Modifiers::SHIFT,
        ),
        (ShortcutProfile::ClassicStudio, Key::Y, Modifiers::CTRL),
        (ShortcutProfile::ClassicStudio, Key::D, Modifiers::CTRL),
        (ShortcutProfile::ClassicStudio, Key::G, Modifiers::CTRL),
        (ShortcutProfile::ClassicStudio, Key::U, Modifiers::CTRL),
        (ShortcutProfile::ClassicStudio, Key::Q, Modifiers::CTRL),
        (ShortcutProfile::ClassicStudio, Key::K, Modifiers::CTRL),
        (ShortcutProfile::ClassicStudio, Key::L, Modifiers::CTRL),
        (ShortcutProfile::ClassicStudio, Key::A, Modifiers::CTRL),
        (
            ShortcutProfile::StandardVector,
            Key::G,
            Modifiers::CTRL | Modifiers::SHIFT,
        ),
    ];

    for (prof, key, mods) in gated_cases {
        assert_eq!(
            resolve_shortcut(prof, key, mods, true),
            None,
            "shortcut ({prof:?}, {key:?}, {mods:?}) must be suppressed while text_focus=true"
        );
        assert!(
            resolve_shortcut(prof, key, mods, false).is_some(),
            "shortcut ({prof:?}, {key:?}, {mods:?}) must resolve when text_focus=false"
        );
    }

    // Command Palette shortcuts remain accessible even from text inputs
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::K,
            Modifiers::CTRL,
            true
        ),
        Some(ShortcutAction::ToggleCommandPalette)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::Space,
            Modifiers::CTRL,
            true
        ),
        Some(ShortcutAction::ToggleCommandPalette)
    );
}

#[test]
fn regression_edit_fill_f11_outline_pen_f12_guide_ops_and_arabic_ui() {
    let tmp = TempTestDir::new("f11-f12-guides-ar");
    let db_path = tmp.path().join("prefs.db");

    // 1. Both shortcut profiles resolve F11 -> OpenEditFill and F12 -> FocusOutlinePen
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::F11,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::OpenEditFill)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::F11,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::OpenEditFill)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::StandardVector,
            Key::F12,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::FocusOutlinePen)
    );

    // 2. F11 Edit Fill + F12 Outline Pen apply onto selected object with clean undo/redo
    {
        let db = StorageDb::open(&db_path).expect("open sqlite db");
        let mut app = StudioApp::new_with_storage(db);
        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 40.0, 30.0));
        let oid = *app
            .active_tab()
            .unwrap()
            .document
            .objects()
            .keys()
            .next()
            .unwrap();
        {
            let tab = app.active_tab_mut().unwrap();
            tab.selection.select_single(&tab.document, oid);
        }

        // Trigger F11 -> modify draft to ConicalGradient -> Apply -> Undo -> Redo
        app.handle_shortcut(ShortcutAction::OpenEditFill);
        assert!(app.show_edit_fill_dialog);
        app.edit_fill_draft = Fill::ConicalGradient {
            center: Point2D::new(20.0, 15.0),
            angle_deg: 45.0,
            stops: vec![
                GradientStop::new(0.0, Color::rgba8(255, 0, 80, 255)),
                GradientStop::new(1.0, Color::rgba8(0, 120, 255, 255)),
            ],
        };
        app.apply_edit_fill_draft();
        app.show_edit_fill_dialog = false;
        assert!(matches!(
            app.active_tab().unwrap().document.object(oid).unwrap().fill,
            Fill::ConicalGradient { .. }
        ));
        app.undo().expect("undo fill");
        assert!(!matches!(
            app.active_tab().unwrap().document.object(oid).unwrap().fill,
            Fill::ConicalGradient { .. }
        ));
        app.redo().expect("redo fill");
        assert!(matches!(
            app.active_tab().unwrap().document.object(oid).unwrap().fill,
            Fill::ConicalGradient { .. }
        ));

        // Trigger F12 -> modify draft with Calligraphy Nib + End Arrow + Overprint -> Apply -> Undo
        app.handle_shortcut(ShortcutAction::FocusOutlinePen);
        assert!(app.show_outline_pen_dialog);
        app.outline_pen_draft.width_mm = 1.25;
        app.outline_pen_draft.calligraphy_stretch = 0.55;
        app.outline_pen_draft.calligraphy_angle_deg = 35.0;
        app.outline_pen_draft.end_arrow = ArrowheadStyle::Triangle;
        app.outline_pen_draft.overprint = true;
        app.apply_outline_pen_draft();
        app.show_outline_pen_dialog = false;
        let applied_outline = &app
            .active_tab()
            .unwrap()
            .document
            .object(oid)
            .unwrap()
            .outline;
        assert!((applied_outline.width_mm - 1.25).abs() < 1e-6);
        assert!((applied_outline.calligraphy_stretch - 0.55).abs() < 1e-6);
        assert_eq!(applied_outline.end_arrow, ArrowheadStyle::Triangle);
        assert!(applied_outline.overprint);
        app.undo().expect("undo outline");
        assert!(
            (app.active_tab()
                .unwrap()
                .document
                .object(oid)
                .unwrap()
                .outline
                .width_mm
                - 1.25)
                .abs()
                > 0.1
        );

        // 3. Guide Move / Lock / Remove command ops roundtrip undo/redo cleanly
        app.execute_command(CommandOp::AddGuide {
            orientation: GuideOrientation::Vertical(50.0),
            locked: false,
        })
        .expect("add guide");
        let gid = app
            .active_tab()
            .unwrap()
            .document
            .guides()
            .last()
            .unwrap()
            .id;
        app.execute_command(CommandOp::MoveGuide {
            id: gid,
            orientation: GuideOrientation::Vertical(85.0),
        })
        .expect("move guide");
        assert_eq!(
            app.active_tab()
                .unwrap()
                .document
                .guides()
                .last()
                .unwrap()
                .orientation,
            GuideOrientation::Vertical(85.0)
        );
        app.execute_command(CommandOp::ToggleGuideLock { id: gid })
            .expect("lock guide");
        assert!(
            app.active_tab()
                .unwrap()
                .document
                .guides()
                .last()
                .unwrap()
                .locked
        );
        app.execute_command(CommandOp::RemoveGuide { id: gid })
            .expect("remove guide");
        assert!(!app
            .active_tab()
            .unwrap()
            .document
            .guides()
            .iter()
            .any(|g| g.id == gid));
        app.undo().expect("undo remove guide");
        assert!(app
            .active_tab()
            .unwrap()
            .document
            .guides()
            .iter()
            .any(|g| g.id == gid));
        app.undo().expect("undo toggle lock");
        app.undo().expect("undo move guide");
        assert_eq!(
            app.active_tab()
                .unwrap()
                .document
                .guides()
                .last()
                .unwrap()
                .orientation,
            GuideOrientation::Vertical(50.0)
        );

        // 4. Switch to Bilingual Arabic UI + ClassicLight theme + StandardVector shortcuts
        // and verify headless UI frame renders F11/F12 modals cleanly and persists in SQLite
        app.set_theme(StudioTheme::ClassicLight);
        app.set_shortcut_profile(ShortcutProfile::StandardVector);
        app.set_ui_language(StudioLanguage::Arabic);
        assert_eq!(app.ui_language, StudioLanguage::Arabic);
        app.show_welcome = false;
        app.show_edit_fill_dialog = true;
        app.show_outline_pen_dialog = true;
        app.fonts_configured = true;
        let ctx = egui::Context::default();
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1440.0, 900.0),
            )),
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();
        app.shutdown_cleanly();
    }

    // 5. Reopen SQLite DB -> theme, shortcut profile, and Arabic UI preference persist cleanly
    let db2 = StorageDb::open(&db_path).expect("reopen sqlite db");
    let app2 = StudioApp::new_with_storage(db2);
    assert_eq!(app2.theme, StudioTheme::ClassicLight);
    assert_eq!(app2.shortcut_profile, ShortcutProfile::StandardVector);
    assert_eq!(app2.ui_language, StudioLanguage::Arabic);

    // 6. HiDPI (pixels_per_point = 2.0) scales canvas texture & DPI cleanly to native monitor resolution
    //    AND `physical_key` resolves shortcuts even if OS layout emits non-Latin `key`.
    let hidpi_ctx = egui::Context::default();
    let mut hidpi_app = StudioApp::new_headless();
    hidpi_app.show_welcome = false;
    hidpi_app.fonts_configured = true;
    let mut hidpi_in = RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        events: vec![egui::Event::Key {
            key: Key::Questionmark,
            physical_key: Some(Key::Z),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        }],
        ..RawInput::default()
    };
    hidpi_in
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(2.0);
    let mut hidpi_out = hidpi_ctx.run_ui(hidpi_in.clone(), |ui| {
        hidpi_app.render_ui(ui);
    });
    hidpi_out.textures_delta.clear();
    let actions =
        coree_app::shortcuts::poll_egui_shortcuts(&hidpi_ctx, ShortcutProfile::ClassicStudio);
    assert_eq!(actions, vec![ShortcutAction::Undo]);
    let tab = hidpi_app.active_tab().unwrap();
    let content_r = hidpi_ctx.content_rect();
    assert!(
        tab.viewport.dpi > 180.0,
        "2x HiDPI viewport must render at ~192 DPI instead of 96 DPI: dpi={}, w={}, h={}, ctx_ppp={}, reported_max={}, content_rect={:?}",
        tab.viewport.dpi,
        tab.viewport.width_px,
        tab.viewport.height_px,
        hidpi_ctx.pixels_per_point(),
        hidpi_ctx.input(|i| i.max_texture_side),
        content_r,
    );
    assert!(
        tab.viewport.width_px > 1280,
        "2x HiDPI backing texture must exceed logical canvas width: {}",
        tab.viewport.width_px
    );
    let _ = &mut hidpi_in;
}

#[test]
fn wave_d_v030_break_effect_powerclip_desktop_icc_and_workspace_bundle_suite() {
    use coree_app::shortcuts::{shortcut_label_for, ShortcutAction, ShortcutProfile};
    use coree_app::state::StudioApp;
    use coree_app::theme::StudioLanguage;
    use coree_core::{
        generate_minimal_cmyk_icc_v2, Affine2D, BuiltinIccProfile, Color, CommandOp, ContourMode,
        CornerStyle, Effect, Layer, NodePayload, ObjectId, ParametricShape, Point2D, SceneObject,
    };
    use coree_storage::StorageDb;

    let mut app = StudioApp::new_headless();

    // 1. Break Effect Apart (`Ctrl+K`) lowers attached effects into independent editable shapes + Undo!
    let mut obj = SceneObject::new(
        ObjectId(0),
        "FX Emblem",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    obj.effects.push(Effect::Contour {
        mode: ContourMode::Outside,
        steps: 2,
        offset_mm: 2.0,
        fill_color: Color::CYAN,
        outline_color: Color::BLACK,
    });
    let lid = app.active_tab().unwrap().active_layer_id;
    app.execute_command(CommandOp::create_object(obj, lid))
        .unwrap();
    let fx_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next_back()
        .unwrap();
    let doc_snap1 = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_single(&doc_snap1, fx_id);
    app.break_apart_selection();
    // Contour(2) + Base shape = 3 independent vector objects, 0 effects remaining
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 3);
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .values()
        .all(|o| o.effects.is_empty()));
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 1);
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(fx_id)
            .unwrap()
            .effects
            .len(),
        1
    );

    // 2. PowerClip Lock Contents toggle + Desktop Layer across multi-page + Undo!
    let child = SceneObject::new(
        ObjectId(0),
        "Child",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 10.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_transform(Affine2D::translate(15.0, 15.0));
    let frame = SceneObject::new(
        ObjectId(0),
        "Frame",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 40.0,
            height: 40.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    app.execute_command(CommandOp::create_object(child, lid))
        .unwrap();
    let cid = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next_back()
        .unwrap();
    app.execute_command(CommandOp::create_object(frame, lid))
        .unwrap();
    let pid = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next_back()
        .unwrap();
    app.execute_command(CommandOp::CreatePowerClip {
        container_id: pid,
        content_ids: vec![cid],
    })
    .unwrap();
    let doc_snap2 = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_single(&doc_snap2, pid);
    app.set_powerclip_lock_selected(Some(false));
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(pid)
            .unwrap()
            .payload,
        NodePayload::PowerClip {
            lock_contents: false,
            ..
        }
    ));

    // Move FX object to cross-page Desktop layer
    let doc_snap3 = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_single(&doc_snap3, fx_id);
    app.move_selection_to_desktop_layer();
    let desk_lid = app
        .active_tab()
        .unwrap()
        .document
        .desktop_layer_id()
        .expect("Desktop layer created");
    assert_eq!(
        app.active_tab().unwrap().document.layer_of_object(fx_id),
        Some(desk_lid)
    );
    let _ = Layer::desktop;

    // 3. Attach custom ICC v2 profile and soft-proof render a frame
    let icc_bytes =
        generate_minimal_cmyk_icc_v2("Studio Proof CMYK", BuiltinIccProfile::IsoCoatedV2Cmyk);
    app.active_tab_mut()
        .unwrap()
        .document
        .set_custom_icc_profile(icc_bytes)
        .unwrap();
    app.soft_proof_cmyk = true;
    app.sync_proof_options_to_tabs();
    let proof_ctx = egui::Context::default();
    let mut out = proof_ctx.run_ui(RawInput::default(), |ui| app.render_ui(ui));
    out.textures_delta.clear();

    // 4. Full v2 WorkspaceBundle export/import + SQLite shutdown -> reload persistence
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("studio_persist.db");
    let data_dir = dir.path().join("data");
    {
        let db = StorageDb::open_with_data_dir(&db_path, data_dir.clone()).expect("open sqlite db");
        let mut app1 = StudioApp::new_with_storage(db);
        app1.set_ui_language(StudioLanguage::Arabic);
        app1.toolbox_two_column = true;
        app1.custom_shortcuts
            .set_binding(ShortcutAction::PrintDocument, "Ctrl+Alt+P")
            .unwrap();
        let ws_json = app1.export_workspace_json();

        let mut app2 = StudioApp::new_headless();
        app2.import_workspace_json(&ws_json)
            .expect("import v2 workspace");
        assert_eq!(app2.ui_language, StudioLanguage::Arabic);
        assert!(app2.toolbox_two_column);
        assert_eq!(
            shortcut_label_for(
                ShortcutProfile::ClassicStudio,
                &app2.custom_shortcuts,
                ShortcutAction::PrintDocument
            ),
            "Ctrl+Alt+P"
        );

        app1.shutdown_cleanly();
    }

    let db_reopened = StorageDb::open_with_data_dir(&db_path, data_dir).expect("reopen sqlite db");
    let app_reloaded = StudioApp::new_with_storage(db_reopened);
    assert_eq!(app_reloaded.ui_language, StudioLanguage::Arabic);
    assert!(app_reloaded.toolbox_two_column);
    assert_eq!(
        app_reloaded
            .custom_shortcuts
            .combo_for(ShortcutAction::PrintDocument),
        Some("Ctrl+Alt+P")
    );
    let _ = Point2D::ZERO;
}

#[test]
fn community_workflows_outline_to_object_smart_dup_powerclip_page_frame_incremental_and_async_printers(
) {
    let dir = TempTestDir::new("community-workflows");
    let mut app = StudioApp::new_headless();
    app.show_welcome = false;
    let (pw, ph, layer_id) = {
        let tab = app.active_tab().unwrap();
        let p = tab.document.active_page().unwrap();
        (p.width_mm, p.height_mm, tab.active_layer_id)
    };

    // 1. Ctrl+Shift+Q Convert Outline to Object (preserving scale_with_object + z-order + undo/redo)
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::Q,
            Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
            false,
        ),
        Some(ShortcutAction::ConvertOutlineToObject)
    );
    let stroked_rect = SceneObject::new(
        ObjectId(0),
        "StrokedBox",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(20, 120, 220, 255)))
    .with_outline(OutlineStyle {
        color: Color::rgba8(0, 0, 0, 255),
        width_mm: 2.0,
        scale_with_object: true,
        ..OutlineStyle::default()
    })
    .with_transform(Affine2D::scale(2.0, 2.0));
    app.execute_command(CommandOp::create_object(stroked_rect, layer_id))
        .unwrap();
    let orig_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    let doc_snap = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_single(&doc_snap, orig_id);

    app.handle_shortcut(ShortcutAction::ConvertOutlineToObject);
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);
    assert!(!app
        .active_tab()
        .unwrap()
        .document
        .object(orig_id)
        .unwrap()
        .outline
        .is_visible());
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 1);
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(orig_id)
        .unwrap()
        .outline
        .is_visible());
    app.redo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);

    // 2. Smart Step-and-Repeat Ctrl+D (tracks interactive transform delta after Duplicate)
    let doc_snap = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_single(&doc_snap, orig_id);
    let b0 = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(orig_id)
        .unwrap();
    let first_dups = app.duplicate_selection().unwrap();
    assert_eq!(first_dups.len(), 1);
    let dup1 = first_dups[0];
    // Move dup1 by (+15, +5) relative to its initial (+5, +5) offset -> total delta (+20, +10)
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![dup1],
        affine: Affine2D::translate(15.0, 5.0),
    })
    .unwrap();
    let second_dups = app.duplicate_selection().unwrap();
    assert_eq!(second_dups.len(), 1);
    let b2 = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(second_dups[0])
        .unwrap();
    assert!((b2.min_x - (b0.min_x + 40.0)).abs() < 1e-4);
    assert!((b2.min_y - (b0.min_y + 20.0)).abs() < 1e-4);

    // 3. PowerClip Edit Mode: enter, hit-test internal child, and exit via Esc
    let child_circle = SceneObject::new(
        ObjectId(0),
        "ClipChild",
        NodePayload::Shape(ParametricShape::Ellipse {
            rx: 8.0,
            ry: 8.0,
            mode: coree_core::EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(220, 50, 50, 255)))
    .with_transform(Affine2D::translate(120.0, 120.0));
    let frame_rect = SceneObject::new(
        ObjectId(0),
        "ClipFrame",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 50.0,
            height: 50.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(240, 240, 240, 255)))
    .with_transform(Affine2D::translate(100.0, 100.0));
    app.execute_command(CommandOp::create_object(child_circle, layer_id))
        .unwrap();
    let child_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    app.execute_command(CommandOp::create_object(frame_rect, layer_id))
        .unwrap();
    let frame_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    let doc_snap = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_many(&doc_snap, &[child_id, frame_id]);
    app.create_powerclip_from_selection();
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(frame_id)
            .unwrap()
            .payload,
        NodePayload::PowerClip { .. }
    ));
    assert_eq!(
        app.hit_test_canvas_point(Point2D::new(120.0, 120.0), 1.0),
        Some(frame_id)
    );
    assert!(app.enter_powerclip_edit(frame_id));
    assert_eq!(app.active_tab().unwrap().powerclip_edit_id, Some(frame_id));
    assert_eq!(
        app.hit_test_canvas_point(Point2D::new(120.0, 120.0), 1.0),
        Some(child_id)
    );
    // 1st Esc clears selection inside PowerClip; 2nd Esc exits PowerClip Edit Mode
    app.handle_shortcut(ShortcutAction::EscapeCancel);
    assert_eq!(app.active_tab().unwrap().powerclip_edit_id, Some(frame_id));
    app.handle_shortcut(ShortcutAction::EscapeCancel);
    assert_eq!(app.active_tab().unwrap().powerclip_edit_id, None);

    // 4. Page Frame & Shift+Selection Frame helper (Toolbox Double-Click Rectangle workflow)
    let doc_snap = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_single(&doc_snap, frame_id);
    let sel_frame_id = app.create_page_frame_rectangle(true).unwrap();
    let sel_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(sel_frame_id)
        .unwrap();
    assert!((sel_bb.min_x - 100.0).abs() < 1e-4 && (sel_bb.width() - 50.0).abs() < 1e-4);

    let page_frame_id = app.create_page_frame_rectangle(false).unwrap();
    let pg_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(page_frame_id)
        .unwrap();
    assert!((pg_bb.width() - pw).abs() < 1e-4 && (pg_bb.height() - ph).abs() < 1e-4);

    // 5. Save Incremental Copy (_v1 -> _v2) & Native DXF CIRCLE export
    let base_file = dir.path().join("poster.coree");
    app.save_active_as(&base_file).unwrap();
    let v1 = app.save_incremental_copy().unwrap();
    let v2 = app.save_incremental_copy().unwrap();
    assert_eq!(
        v1.file_name().and_then(|s| s.to_str()),
        Some("poster_v1.coree")
    );
    assert_eq!(
        v2.file_name().and_then(|s| s.to_str()),
        Some("poster_v2.coree")
    );
    assert!(v1.exists() && v2.exists());

    let top_circle = SceneObject::new(
        ObjectId(0),
        "CadCircle",
        NodePayload::Shape(ParametricShape::Ellipse {
            rx: 12.0,
            ry: 12.0,
            mode: coree_core::EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default())
    .with_transform(Affine2D::translate(80.0, 80.0));
    app.execute_command(CommandOp::create_object(top_circle, layer_id))
        .unwrap();

    let dxf_path = dir.path().join("circle_check.dxf");
    app.export_active(&dxf_path, &UniversalExportOptions::default())
        .unwrap();
    let dxf_text = std::fs::read_to_string(&dxf_path).unwrap();
    assert!(
        dxf_text.contains("\nCIRCLE\n"),
        "DXF export should emit native CIRCLE entity for circular objects"
    );

    // 6. Async printer discovery returns immediately without blocking UI thread & completes cleanly
    let t0 = std::time::Instant::now();
    app.refresh_system_printers_async();
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(250),
        "async printer discovery must not block caller thread"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while app.scanning_printers && std::time::Instant::now() < deadline {
        app.poll_system_printers_async();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!app.scanning_printers);
    assert!(app.printers_status.is_some());

    // 7. Full UI render parity across DarkStudio, ClassicLight & HighContrast (including PowerClip pill + Status Bar)
    assert!(app.enter_powerclip_edit(frame_id));
    let ctx = egui::Context::default();
    for theme in [
        StudioTheme::DarkStudio,
        StudioTheme::ClassicLight,
        StudioTheme::HighContrast,
    ] {
        app.set_theme(theme);
        let mut out = ctx.run_ui(RawInput::default(), |ui| app.render_ui(ui));
        out.textures_delta.clear();
    }
}

#[test]
fn v031_nested_group_transforms_cql_separations_and_arabic_print_preview() {
    let mut app = StudioApp::new_headless();
    app.show_welcome = false;
    let layer_id = app.active_tab().unwrap().active_layer_id;

    // 1. Create two vector curves and group them via CommandOp::Group + translate Group (+50, +30 mm)
    let child_a = SceneObject::new(
        ObjectId(0),
        "Grouped Curve A",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 40.0, 20.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::cut_contour());
    let child_b = SceneObject::new(
        ObjectId(0),
        "Grouped Curve B",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            5.0, 5.0, 10.0, 10.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::MAGENTA))
    .with_outline(OutlineStyle::none());

    app.execute_command(CommandOp::create_object(child_a, layer_id))
        .unwrap();
    app.execute_command(CommandOp::create_object(child_b, layer_id))
        .unwrap();
    let (child_id, child_b_id) = {
        let objs = app.active_tab().unwrap().document.objects();
        let a = objs
            .iter()
            .find(|(_, o)| o.name == "Grouped Curve A")
            .map(|(&id, _)| id)
            .unwrap();
        let b = objs
            .iter()
            .find(|(_, o)| o.name == "Grouped Curve B")
            .map(|(&id, _)| id)
            .unwrap();
        (a, b)
    };
    app.execute_command(CommandOp::Group {
        ids: vec![child_id, child_b_id],
        name: "Translated Group".to_owned(),
    })
    .unwrap();
    let group_id = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .iter()
        .find(|(_, o)| o.name == "Translated Group")
        .map(|(&id, _)| id)
        .unwrap();
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![group_id],
        affine: Affine2D::translate(50.0, 30.0),
    })
    .unwrap();

    // Lock group and select child -> parent Group lock must block vector eraser!
    app.execute_command(CommandOp::SetObjectLocked {
        ids: vec![group_id],
        locked: true,
    })
    .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, child_id);
    }

    // Locked parent Group must block sculpting / erasing even if child itself is unlocked
    assert!(
        !app.apply_vector_eraser_stroke(&[Point2D::new(70.0, 30.0)]),
        "locked ancestor Group must block vector eraser"
    );

    // Unlock Group -> eraser stroke at world (70, 30) maps to child-local (20, 0) via composed world inverse!
    app.execute_command(CommandOp::SetObjectLocked {
        ids: vec![group_id],
        locked: false,
    })
    .unwrap();
    assert!(
        app.apply_vector_eraser_stroke(&[Point2D::new(70.0, 30.0)]),
        "unlocked nested child must erase accurately at world coords (70, 30)"
    );
    app.apply_tool_mesh_fill_at_world(child_id, Point2D::new(60.0, 40.0), Point2D::new(62.0, 42.0));

    // Verify MeshFill nodes live in child-local coords (0..40, NOT 50..90)!
    if let Fill::MeshFill { ref nodes, .. } = app
        .active_tab()
        .unwrap()
        .document
        .object(child_id)
        .unwrap()
        .fill
    {
        assert!(
            nodes.iter().all(|n| n.pos.x <= 45.0 && n.pos.y <= 25.0),
            "MeshFill initialized inside translated Group must stay in object-local coordinates"
        );
    } else {
        panic!("expected MeshFill on child object");
    }

    // 2. Clean-room CQL search selects the hairline CutContour child curve
    app.find_replace_state.cql_query = "@type == 'curve' and @outline.hairline".to_owned();
    let cql_hits = app.run_cql_select().unwrap();
    assert!(cql_hits.contains(&child_id));

    // 3. Outliner-backed CMYK+Spot separations execute cleanly with active ArtisticText
    let txt = SceneObject::new(
        ObjectId(0),
        "Label",
        NodePayload::ArtisticText(coree_core::ArtisticText::from_plain(
            "O",
            coree_core::TextStyle {
                font_size_pt: 24.0,
                fill: Color::KEY_BLACK,
                ..coree_core::TextStyle::default()
            },
        )),
    )
    .with_transform(Affine2D::translate(10.0, 10.0));
    app.execute_command(CommandOp::create_object(txt, layer_id))
        .unwrap();
    app.compute_separations_active();
    assert!(app.active_tab().unwrap().separations_preview.is_some());

    // 4. Arabic UI + Print Preview live thumbnail + Find&Replace CQL UI render cleanly without panics
    app.set_ui_language(coree_app::theme::StudioLanguage::Arabic);
    app.show_print_preview_dialog = true;
    app.focus_docker(coree_app::state::DockerTab::FindReplace);
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(RawInput::default(), |ui| app.render_ui(ui));
    out.textures_delta.clear();
}

#[test]
fn w1b_app_undo_redo_find_replace_shape_edit_path_lock_and_docker_ops() {
    use coree_core::{BlendMode, ObjectStyle, SymbolDef};

    let mut app = StudioApp::new_headless();
    let layer_id = app.active_tab().unwrap().active_layer_id;

    // 1. Seed shape + text
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 40.0, 30.0));
    let rect_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    app.tool_settings.text_insert_draft = "Studio Token".to_owned();
    app.insert_artistic_text_at(Point2D::new(10.0, 60.0));
    let text_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();

    // 2. Mutating Find & Replace executes through CommandHistory (Undo/Redo!)
    app.find_replace_state.text_query = "Token".to_owned();
    let rep = app.run_find_and_replace_action(ReplaceAction::ReplaceText {
        find: "Token".to_owned(),
        replace_with: "Badge".to_owned(),
        case_sensitive: true,
    });
    assert_eq!(rep.replacements_applied, 1);
    if let NodePayload::ArtisticText(ref t) = app
        .active_tab()
        .unwrap()
        .document
        .object(text_id)
        .unwrap()
        .payload
    {
        assert_eq!(t.plain_text(), "Studio Badge");
    } else {
        panic!("expected ArtisticText");
    }
    app.undo().expect("undo FindAndReplace");
    if let NodePayload::ArtisticText(ref t) = app
        .active_tab()
        .unwrap()
        .document
        .object(text_id)
        .unwrap()
        .payload
    {
        assert_eq!(t.plain_text(), "Studio Token");
    }
    app.redo().expect("redo FindAndReplace");
    if let NodePayload::ArtisticText(ref t) = app
        .active_tab()
        .unwrap()
        .document
        .object(text_id)
        .unwrap()
        .payload
    {
        assert_eq!(t.plain_text(), "Studio Badge");
    }

    // 3. Shape Add Node on ParametricShape emits ConvertToCurves + EditPath and Undo restores Shape!
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, rect_id);
    }
    assert!(app.shape_add_node());
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .payload,
        NodePayload::Path(_)
    ));
    app.undo().expect("undo shape_add_node on ParametricShape");
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .payload,
        NodePayload::Shape(ParametricShape::Rectangle { .. })
    ));

    // 4. toggle_lock_selection uses CommandOp::SetObjectLocked and is Undoable
    app.toggle_lock_selection();
    assert!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .locked
    );
    app.undo().expect("undo toggle_lock_selection");
    assert!(
        !app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .locked
    );

    // 5. Docker CommandOps (SetOpacity, SetBlendMode, UpsertSymbol, UpsertObjectStyle) Undo/Redo cleanly
    app.execute_command(CommandOp::SetOpacity {
        ids: vec![rect_id],
        opacity: 0.35,
    })
    .unwrap();
    app.execute_command(CommandOp::SetBlendMode {
        ids: vec![rect_id],
        blend_mode: BlendMode::Overlay,
    })
    .unwrap();
    let sym = SymbolDef::new(
        "DockerSym",
        vec![app
            .active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .clone()],
    );
    let sym_id = sym.id;
    app.execute_command(CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();
    let mut style = ObjectStyle::new("DockerStyle");
    style.opacity = Some(0.35);
    app.execute_command(CommandOp::UpsertObjectStyle { style })
        .unwrap();

    assert!(app
        .active_tab()
        .unwrap()
        .document
        .symbols()
        .contains_key(&sym_id));
    assert_eq!(app.active_tab().unwrap().document.object_styles().len(), 1);
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.object_styles().len(), 0);
    app.undo().unwrap();
    assert!(!app
        .active_tab()
        .unwrap()
        .document
        .symbols()
        .contains_key(&sym_id));
    app.undo().unwrap();
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .blend_mode,
        BlendMode::Normal
    );
    app.undo().unwrap();
    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .opacity
            - 1.0)
            .abs()
            < 1e-6
    );
    let _ = layer_id;
}

#[test]
fn w1b_r2_app_wiring_shape_close_align_print_marks_trace_bitmap_and_resample() {
    use coree_core::{
        BlendMode, GridConfig, ObjectStyle, PerspectiveGridConfig, RasterAsset, Unit,
    };

    let mut app = StudioApp::new_headless();
    let layer_id = app.active_tab().unwrap().active_layer_id;

    // 1. shape_close_curve on open ParametricShape::Spiral succeeds and undoes back to Shape
    let spiral = SceneObject::new(
        ObjectId(0),
        "OpenSpiral",
        NodePayload::Shape(ParametricShape::Spiral {
            radius: 15.0,
            revolutions: 2.0,
            expansion: 1.0,
            logarithmic: false,
        }),
    );
    app.execute_command(CommandOp::create_object(spiral, layer_id))
        .unwrap();
    let spiral_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, spiral_id);
    }
    assert!(
        app.shape_close_curve(),
        "shape_close_curve must convert ParametricShape to curves and close it"
    );
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(spiral_id)
        .unwrap()
        .payload
    {
        assert!(p.subpaths.iter().all(|sp| sp.closed));
    } else {
        panic!("expected Path after shape_close_curve");
    }
    app.undo().expect("undo shape_close_curve");
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(spiral_id)
            .unwrap()
            .payload,
        NodePayload::Shape(ParametricShape::Spiral { .. })
    ));

    // 2. SetObjectStyleLink applies fill, outline, blend_mode, opacity, and links style_id
    let mut style = ObjectStyle::new("LinkedStyle");
    style.fill = Some(Fill::Solid(Color::MAGENTA));
    style.blend_mode = Some(BlendMode::Screen);
    style.opacity = Some(0.55);
    let style_id = style.id;
    app.execute_command(CommandOp::UpsertObjectStyle { style })
        .unwrap();
    app.execute_command(CommandOp::SetObjectStyleLink {
        ids: vec![spiral_id],
        style_id: Some(style_id),
    })
    .unwrap();
    {
        let obj = app
            .active_tab()
            .unwrap()
            .document
            .object(spiral_id)
            .unwrap();
        assert_eq!(obj.style_id, Some(style_id));
        assert_eq!(obj.blend_mode, BlendMode::Screen);
        assert!((obj.opacity - 0.55).abs() < 1e-6);
    }
    app.undo().unwrap();
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(spiral_id)
            .unwrap()
            .style_id,
        None
    );

    // 3. SetPageProperties, SetDocumentProperties (unit, multipage, custom ICC), SetGridConfig, SetPerspectiveGrid, duplicate_page RenameLayer
    let page_id = app.active_tab().unwrap().document.active_page().unwrap().id;
    app.execute_command(CommandOp::SetPageProperties {
        page_id,
        name: None,
        width_mm: Some(150.0),
        height_mm: Some(100.0),
        bleed_mm: None,
        facing_pages: None,
    })
    .unwrap();
    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .active_page()
            .unwrap()
            .width_mm
            - 150.0)
            .abs()
            < 1e-6
    );
    app.undo().unwrap();
    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .active_page()
            .unwrap()
            .width_mm
            - 210.0)
            .abs()
            < 1e-6
    );

    let icc = coree_core::icc::generate_minimal_srgb_icc_v2("Studio Test ICC");
    app.execute_command(CommandOp::SetDocumentProperties {
        title: None,
        color_mode: None,
        default_unit: Some(Unit::Inch),
        dpi: None,
        multipage_view: Some(true),
        active_page_index: None,
        metadata: None,
        custom_icc_profile: Some(icc),
        clear_custom_icc: false,
    })
    .unwrap();
    assert_eq!(
        app.active_tab().unwrap().document.default_unit(),
        Unit::Inch
    );
    assert!(app.active_tab().unwrap().document.multipage_view());
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .custom_icc_profile()
        .is_some());
    app.undo().unwrap();
    assert_eq!(
        app.active_tab().unwrap().document.default_unit(),
        Unit::Millimeter
    );
    assert!(!app.active_tab().unwrap().document.multipage_view());
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .custom_icc_profile()
        .is_none());

    let grid = GridConfig {
        visible: true,
        snap: true,
        ..Default::default()
    };
    app.execute_command(CommandOp::SetGridConfig { config: grid })
        .unwrap();
    assert!(app.active_tab().unwrap().document.grid().visible);
    app.undo().unwrap();
    assert!(!app.active_tab().unwrap().document.grid().visible);

    let pgrid = PerspectiveGridConfig {
        enabled: true,
        ..Default::default()
    };
    app.execute_command(CommandOp::SetPerspectiveGrid { config: pgrid })
        .unwrap();
    assert!(
        app.active_tab()
            .unwrap()
            .document
            .perspective_grid()
            .enabled
    );
    app.undo().unwrap();
    assert!(
        !app.active_tab()
            .unwrap()
            .document
            .perspective_grid()
            .enabled
    );

    // duplicate_page must preserve first layer's custom name across Undo/Redo!
    let p0_layer0 = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    app.execute_command(CommandOp::RenameLayer {
        layer_id: p0_layer0,
        new_name: "Artwork Layer".to_owned(),
    })
    .unwrap();
    app.duplicate_page(0);
    assert_eq!(
        app.active_tab().unwrap().document.pages()[1].layers[0].name,
        "Artwork Layer"
    );
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 1);
    app.redo().unwrap();
    assert_eq!(
        app.active_tab().unwrap().document.pages()[1].layers[0].name,
        "Artwork Layer"
    );
    app.undo().unwrap();

    // 4. Single-object apply_align inside a rotated + scaled Group aligns world bounds to page edge
    let child_box = SceneObject::new(
        ObjectId(0),
        "GroupedBox",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 10.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_transform(Affine2D::translate(30.0, 40.0));
    app.execute_command(CommandOp::create_object(child_box, Some(p0_layer0)))
        .unwrap();
    let child_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    app.execute_command(CommandOp::Group {
        ids: vec![child_id],
        name: "RotGrp".to_owned(),
    })
    .unwrap();
    let grp_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![grp_id],
        affine: Affine2D::translate(80.0, 50.0)
            .compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_2))
            .compose(&Affine2D::scale(2.0, 2.0)),
    })
    .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, child_id);
    }
    app.apply_align(AlignOp::Left);
    let aligned_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!(
        aligned_bb.min_x.abs() < 1e-5,
        "single-object Left align inside rotated/scaled Group must place world min_x at 0.0, got {aligned_bb:?}"
    );
    app.undo().unwrap();

    // 5. add_print_marks_active, convert_selection_to_bitmap, resample_selected_bitmap, and trace_selected_raster are Undoable!
    let count_before_marks = app.active_tab().unwrap().document.objects().len();
    app.add_print_marks_active();
    assert!(app.active_tab().unwrap().document.objects().len() > count_before_marks);
    app.undo().expect("undo add_print_marks_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_marks
    );

    // Select spiral_id and convert to bitmap -> Undo -> Redo
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, spiral_id);
    }
    let count_before_bmp = app.active_tab().unwrap().document.objects().len();
    app.convert_selection_to_bitmap();
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_bmp + 1
    );
    let bmp_id = app.active_tab().unwrap().selection.objects[0];
    app.undo().expect("undo convert_selection_to_bitmap");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_bmp
    );
    app.redo().expect("redo convert_selection_to_bitmap");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_bmp + 1
    );

    // Resample bitmap -> preserves old asset_id for Undo!
    let orig_asset_id = match &app
        .active_tab()
        .unwrap()
        .document
        .object(bmp_id)
        .unwrap()
        .payload
    {
        NodePayload::Raster(r) => r.asset_id,
        other => panic!("expected Raster, got {other:?}"),
    };
    assert!(
        app.active_tab()
            .unwrap()
            .document
            .asset(orig_asset_id)
            .is_some(),
        "orig asset must be present after redo"
    );
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, bmp_id);
    }
    app.resample_selected_bitmap(32, 24);
    let new_asset_id = match &app
        .active_tab()
        .unwrap()
        .document
        .object(bmp_id)
        .unwrap()
        .payload
    {
        NodePayload::Raster(r) => r.asset_id,
        other => panic!("expected Raster, got {other:?}"),
    };
    assert_ne!(
        new_asset_id,
        orig_asset_id,
        "resample must allocate a fresh asset_id; toasts={:?}, sel={:?}",
        app.toasts,
        app.active_tab().unwrap().selection.objects
    );
    let resampled_asset = app
        .active_tab()
        .unwrap()
        .document
        .asset(new_asset_id)
        .unwrap();
    assert_eq!((resampled_asset.width, resampled_asset.height), (32, 24));
    app.undo().expect("undo resample_selected_bitmap");
    match &app
        .active_tab()
        .unwrap()
        .document
        .object(bmp_id)
        .unwrap()
        .payload
    {
        NodePayload::Raster(r) => assert_eq!(r.asset_id, orig_asset_id),
        other => panic!("expected Raster, got {other:?}"),
    }
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .asset(orig_asset_id)
        .is_some());

    // Create a crisp high-contrast synthetic raster and run trace_selected_raster -> Undo -> Redo
    let mut px = vec![255u8; 20 * 20 * 4];
    for y in 4..16 {
        for x in 4..16 {
            let idx = (y * 20 + x) * 4;
            px[idx] = 0;
            px[idx + 1] = 0;
            px[idx + 2] = 0;
            px[idx + 3] = 255;
        }
    }
    let trace_asset = RasterAsset::new(20, 20, px, 72.0, ColorMode::Rgb).unwrap();
    let trace_aid = trace_asset.id;
    app.active_tab_mut()
        .unwrap()
        .document
        .insert_asset_if_absent(trace_asset);
    let trace_raster_obj = SceneObject::new(
        ObjectId(0),
        "TraceSrc",
        NodePayload::Raster(coree_core::RasterLayerData {
            asset_id: trace_aid,
            width_mm: 20.0,
            height_mm: 20.0,
            filters: Vec::new(),
            crop_rect: None,
        }),
    );
    app.execute_command(CommandOp::create_object(trace_raster_obj, Some(p0_layer0)))
        .unwrap();
    let trace_src_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, trace_src_id);
    }
    let count_before_trace = app.active_tab().unwrap().document.objects().len();
    let traced_count = app
        .trace_selected_raster()
        .expect("trace_selected_raster must succeed");
    assert!(traced_count >= 1);
    assert!(app.active_tab().unwrap().document.objects().len() > count_before_trace);
    app.undo().expect("undo trace_selected_raster");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_trace
    );
}

trait MeshTestExt {
    fn apply_tool_mesh_fill_at_world(&mut self, id: ObjectId, a: Point2D, b: Point2D);
}

impl MeshTestExt for StudioApp {
    fn apply_tool_mesh_fill_at_world(&mut self, id: ObjectId, a: Point2D, b: Point2D) {
        if let Some(tab) = self.active_tab_mut() {
            tab.selection.select_single(&tab.document, id);
        }
        assert!(self.mesh_fill_interact(a, b, None));
    }
}

#[test]
fn w1b_r3_production_macros_import_and_autofit_undo_redo_and_replay() {
    let tmp = TempTestDir::new("w1b-r3-undo-replay");
    let db = StorageDb::open(&tmp.path().join("studio.db")).expect("open sqlite db");
    let mut app = StudioApp::new_with_storage(db);

    // Baseline rectangle on page 0 at (30, 40), size 50x25
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(30.0, 40.0, 50.0, 25.0));
    let base_rect_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, base_rect_id);
    }

    // 1. CutContour: Undo removes generated cut object + layer, Redo restores both
    let count_before_cut = app.active_tab().unwrap().document.objects().len();
    let layers_before_cut = app.active_tab().unwrap().document.pages()[0].layers.len();
    app.generate_cut_contour_active();
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_cut + 1
    );
    assert_eq!(
        app.active_tab().unwrap().document.pages()[0].layers.len(),
        layers_before_cut + 1
    );
    app.undo().expect("undo generate_cut_contour_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_cut
    );
    assert_eq!(
        app.active_tab().unwrap().document.pages()[0].layers.len(),
        layers_before_cut
    );
    app.redo().expect("redo generate_cut_contour_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_cut + 1
    );

    // 2. Parametric Dieline: Undo removes all dieline paths + Cut/Crease layers, Redo restores
    let count_before_die = app.active_tab().unwrap().document.objects().len();
    app.generate_dieline_active();
    let count_after_die = app.active_tab().unwrap().document.objects().len();
    assert!(count_after_die > count_before_die);
    app.undo().expect("undo generate_dieline_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_die
    );
    app.redo().expect("redo generate_dieline_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_after_die
    );

    // 3. Vector Barcode: Undo removes grouped barcode + children, Redo restores
    let count_before_bc = app.active_tab().unwrap().document.objects().len();
    app.insert_barcode_active();
    let count_after_bc = app.active_tab().unwrap().document.objects().len();
    assert!(count_after_bc > count_before_bc);
    app.undo().expect("undo insert_barcode_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_bc
    );
    app.redo().expect("redo insert_barcode_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_after_bc
    );

    // 4. Vector QR Code: Undo removes grouped QR + children, Redo restores
    let count_before_qr = app.active_tab().unwrap().document.objects().len();
    app.insert_qr_active();
    let count_after_qr = app.active_tab().unwrap().document.objects().len();
    assert!(count_after_qr > count_before_qr);
    app.undo().expect("undo insert_qr_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_qr
    );
    app.redo().expect("redo insert_qr_active");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_after_qr
    );

    // 5. Non-recorded Rhai macro: Undo/Redo + error rollback leaves doc & history untouched
    let before_macro_doc = app.active_tab().unwrap().document.clone();
    let ran = app
        .run_rhai_script("translate_objects(all_object_ids(), 15.0, -5.0);")
        .expect("non-recorded Rhai macro must succeed");
    assert_eq!(ran, 1);
    assert_ne!(app.active_tab().unwrap().document, before_macro_doc);
    let log_after_macro = app.active_tab().unwrap().history.command_log();
    assert_no_dummy_transform_ops(&log_after_macro, "run_rhai_script");
    app.undo().expect("undo non-recorded Rhai macro");
    assert_eq!(app.active_tab().unwrap().document, before_macro_doc);
    app.redo().expect("redo non-recorded Rhai macro");
    assert_ne!(app.active_tab().unwrap().document, before_macro_doc);

    let before_err_macro = app.active_tab().unwrap().document.clone();
    let undo_len_before_err = app.active_tab().unwrap().history.undo_labels().len();
    let err_res =
        app.run_rhai_script("translate_objects(all_object_ids(), 40.0, 40.0); throw \"boom\";");
    assert!(err_res.is_err());
    assert_eq!(
        app.active_tab().unwrap().document,
        before_err_macro,
        "failed Rhai macro must roll back partial mutations"
    );
    assert_eq!(
        app.active_tab().unwrap().history.undo_labels().len(),
        undo_len_before_err,
        "failed Rhai macro must not push an undo entry"
    );

    // 6. import_file_path: importing SVG is cleanly undoable/redoable
    let svg_path = tmp.path().join("import_part.svg");
    std::fs::write(
        &svg_path,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="20mm" viewBox="0 0 40 20"><rect x="2" y="3" width="12" height="8" fill="#ff0000"/></svg>"##,
    )
    .unwrap();
    let count_before_import = app.active_tab().unwrap().document.objects().len();
    let imported = app
        .import_file_path(&svg_path)
        .expect("import_file_path must succeed");
    assert!(imported >= 1);
    assert_no_dummy_transform_ops(
        &app.active_tab().unwrap().history.command_log(),
        "import_file_path",
    );
    let count_after_import = app.active_tab().unwrap().document.objects().len();
    assert!(count_after_import > count_before_import);
    app.undo().expect("undo import_file_path");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_import
    );
    app.redo().expect("redo import_file_path");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_after_import
    );

    // 7. autofit_active_page: real TransformObjects + SetPageProperties + command_log replay parity
    let mut autofit_app = StudioApp::new_headless();
    let init_doc = autofit_app.active_tab().unwrap().document.clone();
    autofit_app.select_tool(ToolId::Rectangle);
    autofit_app.commit_drawn_box(Rect2D::from_xywh(40.0, 60.0, 50.0, 30.0));
    let obj_id = *autofit_app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();

    autofit_app.autofit_active_page(5.0);
    let tab = autofit_app.active_tab().unwrap();
    let page = &tab.document.pages()[0];
    assert!((page.width_mm - 60.0).abs() < 1e-6);
    assert!((page.height_mm - 40.0).abs() < 1e-6);
    let bb = tab.document.object_bounds(obj_id).unwrap();
    assert!((bb.min_x - 5.0).abs() < 1e-6);
    assert!((bb.min_y - 5.0).abs() < 1e-6);

    // Verify ZERO dummy no-op TransformObjects { ids: [] } and deterministic replay_log parity!
    let log = tab.history.command_log();
    let has_dummy_noop = log.iter().any(|op| match op {
        CommandOp::TransformObjects { ids, .. } => ids.is_empty(),
        CommandOp::Batch(ops) => ops.iter().any(
            |inner| matches!(inner, CommandOp::TransformObjects { ids, .. } if ids.is_empty()),
        ),
        _ => false,
    });
    assert!(
        !has_dummy_noop,
        "autofit_active_page must never emit dummy empty TransformObjects"
    );

    let mut replayed = init_doc;
    coree_core::CommandHistory::replay_log(&mut replayed, &log)
        .expect("command_log replay must reproduce autofit_active_page");
    assert!((replayed.pages()[0].width_mm - 60.0).abs() < 1e-6);
    assert!((replayed.pages()[0].height_mm - 40.0).abs() < 1e-6);
    let rbb = replayed.object_bounds(obj_id).unwrap();
    assert!((rbb.min_x - 5.0).abs() < 1e-6);
    assert!((rbb.min_y - 5.0).abs() < 1e-6);

    autofit_app.undo().expect("undo autofit_active_page");
    let page_undone = &autofit_app.active_tab().unwrap().document.pages()[0];
    assert!((page_undone.width_mm - 210.0).abs() < 1e-6);
    assert!((page_undone.height_mm - 297.0).abs() < 1e-6);
    let bb_undone = autofit_app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(obj_id)
        .unwrap();
    assert!((bb_undone.min_x - 40.0).abs() < 1e-6);
    assert!((bb_undone.min_y - 60.0).abs() < 1e-6);

    autofit_app.redo().expect("redo autofit_active_page");
    let page_redone = &autofit_app.active_tab().unwrap().document.pages()[0];
    assert!((page_redone.width_mm - 60.0).abs() < 1e-6);
    assert!((page_redone.height_mm - 40.0).abs() < 1e-6);
}

fn assert_no_dummy_transform_ops(ops: &[CommandOp], ctx: &str) {
    fn contains_dummy(op: &CommandOp) -> bool {
        match op {
            CommandOp::TransformObjects { ids, .. } => ids.is_empty(),
            CommandOp::Batch(inner) => inner.iter().any(contains_dummy),
            _ => false,
        }
    }
    assert!(
        !ops.iter().any(contains_dummy),
        "{ctx}: command_log must never contain dummy empty TransformObjects"
    );
}

#[test]
fn w1b_r4_replay_parity_nested_shape_rhai_import_bitmap_paste_crop_and_strokes() {
    let tmp = TempTestDir::new("w1b-r4-full-replay");
    let db_path = tmp.path().join("studio.db");
    let expected_doc_id: String;
    let init_doc: Document;
    let live_doc: Document;
    let full_log: Vec<CommandOp>;

    {
        let db = StorageDb::open(&db_path).expect("open sqlite db");
        let mut app = StudioApp::new_with_storage(db);
        expected_doc_id = app.active_tab().unwrap().document.id().to_string();
        init_doc = app.active_tab().unwrap().document.clone();

        // 0. Empty-page 0-op Rhai scripts must push ZERO undo entries and emit ZERO dummy ops
        let undo_before_noop = app.active_tab().unwrap().history.undo_labels().len();
        assert_eq!(
            app.run_rhai_script("translate_objects(all_object_ids(), 10.0, 10.0); log(\"empty\");")
                .unwrap(),
            0
        );
        assert_eq!(
            app.active_tab().unwrap().history.undo_labels().len(),
            undo_before_noop
        );

        // 1. Nested ParametricShape edit inside a Group via commit_parametric_shape_edit
        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
        app.commit_drawn_box(Rect2D::from_xywh(50.0, 10.0, 25.0, 15.0));
        let created_ids: Vec<ObjectId> = app
            .active_tab()
            .unwrap()
            .document
            .objects()
            .keys()
            .copied()
            .collect();
        let rect1 = created_ids[0];
        let rect2 = created_ids[1];
        app.execute_command(CommandOp::Group {
            ids: vec![rect1, rect2],
            name: "ShapeGroup".to_owned(),
        })
        .unwrap();
        let orig_shape = match &app
            .active_tab()
            .unwrap()
            .document
            .object(rect1)
            .unwrap()
            .payload
        {
            NodePayload::Shape(s) => s.clone(),
            _ => panic!("expected parametric shape"),
        };
        let edited_shape = ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [5.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        };
        assert!(app.commit_parametric_shape_edit(rect1, orig_shape.clone(), edited_shape.clone()));
        assert_eq!(
            app.active_tab().unwrap().document.layer_of_object(rect1),
            None
        );
        app.undo().expect("undo nested EditShape");
        assert_eq!(
            app.active_tab()
                .unwrap()
                .document
                .object(rect1)
                .unwrap()
                .payload,
            NodePayload::Shape(orig_shape)
        );
        app.redo().expect("redo nested EditShape");
        assert_eq!(
            app.active_tab()
                .unwrap()
                .document
                .object(rect1)
                .unwrap()
                .payload,
            NodePayload::Shape(edited_shape)
        );

        // 2. All 5 built-in Rhai macros + custom + recorded macro + failing script rollback
        let page_id = app.active_tab().unwrap().document.pages()[0].id;
        app.execute_command(CommandOp::AddLayer {
            page_id,
            name: "EmptyToClean".to_owned(),
            is_cut_layer: false,
            master_scope: coree_core::MasterLayerScope::None,
        })
        .unwrap();
        let rhai_scripts = [
            "resize_objects(all_object_ids(), 35.0, 22.0);",
            "align_grid(all_object_ids(), 2, 8.0, 8.0, 12.0, 12.0);",
            "recolor_all_to_cmyk();",
            "clean_empty_layers();",
            "generate_cut_contour(non_cut_object_ids(), 1.5, \"CutContour\");",
            "translate_objects(all_object_ids(), 4.0, 3.0);",
            "// coree recorded macro\ntranslate_objects(all_object_ids(), 2.0, 1.0);",
        ];
        for script in rhai_scripts {
            // Note: recorded macro uses `all_object_ids()` only in normal mode; for recorded mode pass concrete ID
            if script.starts_with("// coree recorded macro") {
                let first_id = *app
                    .active_tab()
                    .unwrap()
                    .document
                    .objects()
                    .keys()
                    .next()
                    .unwrap();
                let rec = format!(
                    "// coree recorded macro\ntransform_objects([{}], [1.0, 0.0, 0.0, 1.0, 2.0, 1.0]);",
                    first_id.0
                );
                app.run_rhai_script(&rec).expect("recorded macro");
            } else {
                app.run_rhai_script(script).expect("rhai script");
            }
        }
        let err_undo_len = app.active_tab().unwrap().history.undo_labels().len();
        assert!(app
            .run_rhai_script("translate_objects(all_object_ids(), 5.0, 5.0); throw \"boom\";")
            .is_err());
        assert_eq!(
            app.active_tab().unwrap().history.undo_labels().len(),
            err_undo_len
        );

        // 3. import_file_path: grouped SVG + PNG raster
        let svg_path = tmp.path().join("grouped.svg");
        std::fs::write(
            &svg_path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="30mm" height="20mm" viewBox="0 0 30 20"><g id="grp"><rect x="1" y="1" width="10" height="8" fill="#0088ff"/><circle cx="18" cy="10" r="5" fill="#ff5500"/></g></svg>"##,
        )
        .unwrap();
        assert!(app.import_file_path(&svg_path).unwrap() >= 1);

        let png_path = tmp.path().join("sample.png");
        coree_io::export_raster(
            4,
            4,
            &[200u8; 4 * 4 * 4],
            &coree_io::RasterExportOptions::default(),
            &png_path,
        )
        .unwrap();
        assert!(app.import_file_path(&png_path).unwrap() >= 1);

        // 4. convert_selection_to_bitmap + resample_selected_bitmap
        {
            let tab = app.active_tab_mut().unwrap();
            tab.selection.select_single(&tab.document, rect1);
        }
        app.convert_selection_to_bitmap();
        app.resample_selected_bitmap(16, 16);

        // 5. copy_selection on raster -> delete original -> paste_clipboard
        let raster_id = *app
            .active_tab()
            .unwrap()
            .document
            .objects()
            .iter()
            .find(|(_, o)| matches!(o.payload, NodePayload::Raster(_)))
            .unwrap()
            .0;
        {
            let tab = app.active_tab_mut().unwrap();
            tab.selection.select_single(&tab.document, raster_id);
        }
        assert_eq!(app.copy_selection(), 1);
        app.delete_selection();
        let pasted = app.paste_clipboard();
        assert_eq!(pasted.len(), 1);

        // 6. apply_raster_tool_stroke (auto-creates raster layer via ensure_raster_target_at + paints stroke)
        {
            let tab = app.active_tab_mut().unwrap();
            tab.selection.clear();
        }
        assert!(app.apply_raster_tool_stroke(
            ToolId::BitmapBrush,
            &[Point2D::new(15.0, 15.0), Point2D::new(25.0, 25.0)],
        ));

        // 7. apply_crop_rect (plus undo check verifying cropped raster + asset restore)
        let before_crop = app.active_tab().unwrap().document.clone();
        assert!(app.apply_crop_rect(Rect2D::from_xywh(5.0, 5.0, 120.0, 100.0)));
        app.undo().expect("undo apply_crop_rect");
        assert_eq!(
            app.active_tab().unwrap().document.objects(),
            before_crop.objects()
        );
        app.redo().expect("redo apply_crop_rect");

        // 8. Execute one more EditShape after the last WAL snapshot checkpoint so SQLite
        // `command_journal` replay (without snapshot overwrite) is ALSO exercised on crash recovery.
        let edited_again = ParametricShape::Rectangle {
            width: 32.0,
            height: 24.0,
            corner_radii: [3.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        };
        app.execute_command(CommandOp::EditShape {
            id: rect1,
            shape: edited_again,
        })
        .expect("post-checkpoint EditShape");

        let tab = app.active_tab().unwrap();
        full_log = tab.history.command_log();
        assert_no_dummy_transform_ops(&full_log, "w1b_r4_full_session");
        live_doc = tab.document.clone();
    }

    // Verify CommandHistory::replay_log reproduces live objects, pages, symbols, and ALL raster assets
    let mut replayed = init_doc;
    coree_core::CommandHistory::replay_log(&mut replayed, &full_log)
        .expect("replay_log must succeed for full W1B-R4 session");
    assert_eq!(replayed.objects(), live_doc.objects());
    assert_eq!(replayed.pages(), live_doc.pages());
    assert_eq!(replayed.symbols(), live_doc.symbols());
    for (oid, obj) in replayed.objects() {
        if let NodePayload::Raster(r) = &obj.payload {
            assert_eq!(
                replayed.asset(r.asset_id),
                live_doc.asset(r.asset_id),
                "replayed raster {oid} must have valid non-dangling asset {}",
                r.asset_id
            );
        }
    }

    // Verify SQLite WAL crash recovery (snapshot + command_journal replay) reproduces live document
    let db2 = StorageDb::open(&db_path).expect("reopen sqlite db");
    let mut recovered_app = StudioApp::new_with_storage(db2);
    let sess = recovered_app
        .unclean_sessions
        .iter()
        .find(|s| s.document_id == expected_doc_id)
        .cloned()
        .expect("unclean WAL session must exist");
    recovered_app
        .recover_unclean_session(&sess.document_id, &sess.session_id)
        .expect("WAL recovery must succeed");
    let rec_doc = &recovered_app.active_tab().unwrap().document;
    let live_json_roundtrip: Document =
        serde_json::from_slice(&serde_json::to_vec(&live_doc).expect("serialize live_doc"))
            .expect("roundtrip live_doc");
    assert_eq!(rec_doc.objects(), live_json_roundtrip.objects());
    assert_eq!(rec_doc.pages(), live_json_roundtrip.pages());
    assert_eq!(rec_doc.symbols(), live_json_roundtrip.symbols());
    for (oid, obj) in rec_doc.objects() {
        if let NodePayload::Raster(r) = &obj.payload {
            assert_eq!(
                rec_doc.asset(r.asset_id),
                live_doc.asset(r.asset_id),
                "WAL-recovered raster {oid} must have valid asset"
            );
        }
    }
}

#[test]
fn w1b_r5_copy_paste_bitmap_pattern_and_table_cell_pattern_cross_tab() {
    use coree_core::{RasterAsset, VectorTable};

    let mut app = StudioApp::new_headless();
    let layer1 = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    let pat_asset = RasterAsset::new(2, 2, vec![0xAAu8; 16], 150.0, ColorMode::Rgb).unwrap();
    let pat_aid = pat_asset.id;
    let tbl_asset = RasterAsset::new(2, 2, vec![0x55u8; 16], 150.0, ColorMode::Rgb).unwrap();
    let tbl_aid = tbl_asset.id;

    let pat_rect = SceneObject::new(
        ObjectId(0),
        "BitmapPatternRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 25.0,
            height: 15.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::BitmapPattern {
        image_id: pat_aid,
        tile_w: 10.0,
        tile_h: 10.0,
    });

    let mut tbl = VectorTable::new(1, 2, 15.0, 10.0);
    tbl.cells[0].fill = Fill::BitmapPattern {
        image_id: tbl_aid,
        tile_w: 8.0,
        tile_h: 8.0,
    };
    let tbl_obj = SceneObject::new(ObjectId(0), "PatternTable", NodePayload::Table(tbl));

    app.execute_command(CommandOp::Batch(vec![
        CommandOp::RegisterAsset {
            asset: pat_asset.clone(),
        },
        CommandOp::RegisterAsset {
            asset: tbl_asset.clone(),
        },
        CommandOp::create_object(pat_rect, Some(layer1)),
        CommandOp::create_object(tbl_obj, Some(layer1)),
    ]))
    .unwrap();

    let created_ids: Vec<ObjectId> = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .copied()
        .collect();
    assert_eq!(created_ids.len(), 2);

    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_many(&tab.document, &created_ids);
    }
    assert_eq!(app.copy_selection(), 2);
    app.delete_selection();

    // Paste into a fresh second tab
    app.new_document(Document::new_a4("Tab2"));
    let tab2_init = app.active_tab().unwrap().document.clone();
    let pasted = app.paste_clipboard();
    assert_eq!(pasted.len(), 2);

    let tab2 = app.active_tab().unwrap();
    assert_eq!(tab2.document.asset(pat_aid), Some(&pat_asset));
    assert_eq!(tab2.document.asset(tbl_aid), Some(&tbl_asset));

    let mut replayed = tab2_init;
    coree_core::CommandHistory::replay_log(&mut replayed, &tab2.history.command_log()).unwrap();
    assert_eq!(replayed.objects(), tab2.document.objects());
    assert_eq!(replayed.asset(pat_aid), Some(&pat_asset));
    assert_eq!(replayed.asset(tbl_aid), Some(&tbl_asset));

    // Delete one pasted object and verify post-delete GC keeps surviving pattern asset
    // and undo still restores the deleted object + its asset cleanly
    let first_pasted = pasted[0];
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, first_pasted);
    }
    app.delete_selection();
    {
        let tab = app.active_tab_mut().unwrap();
        let _ = tab.history.gc_document_assets(&mut tab.document);
        assert!(tab.document.asset(pat_aid).is_some());
        assert!(tab.document.asset(tbl_aid).is_some());
    }
    app.undo().unwrap();
    let tab2_after_undo = app.active_tab().unwrap();
    assert!(tab2_after_undo.document.object(first_pasted).is_some());
    assert_eq!(tab2_after_undo.document.asset(pat_aid), Some(&pat_asset));
    assert_eq!(tab2_after_undo.document.asset(tbl_aid), Some(&tbl_asset));
}

#[test]
fn w1b_r6_studio_execute_command_syncs_max_history_asset_eviction_to_wal() {
    use coree_core::{CommandHistory, RasterAsset};
    use coree_storage::StorageDb;

    let dir = tempfile::tempdir().unwrap();
    let db = StorageDb::open(&dir.path().join("studio_max_history_wal.db")).unwrap();
    let mut app = StudioApp::new_with_storage(db);
    app.new_document(Document::new_a4("StudioMaxHistory"));
    let doc_id = app.active_tab().unwrap().document.id().to_string();
    let layer = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    app.active_tab_mut().unwrap().history = CommandHistory::new(2);

    let box_obj = SceneObject::new(
        ObjectId(0),
        "Box",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    app.execute_command(CommandOp::create_object(box_obj, Some(layer)))
        .unwrap();
    let box_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();

    let asset = RasterAsset::new(2, 2, vec![0x77u8; 16], 150.0, ColorMode::Rgb).unwrap();
    app.execute_command(CommandOp::RegisterAsset { asset })
        .unwrap();
    assert_eq!(app.active_tab().unwrap().document.assets().len(), 1);

    app.execute_command(CommandOp::RenameObject {
        id: box_id,
        new_name: "B1".into(),
    })
    .unwrap();
    app.execute_command(CommandOp::RenameObject {
        id: box_id,
        new_name: "B2".into(),
    })
    .unwrap();

    let live_assets = app.active_tab().unwrap().document.assets().clone();
    assert!(live_assets.is_empty());
    let recovered = app
        .storage
        .as_ref()
        .unwrap()
        .recover_document(&doc_id)
        .unwrap();
    assert_eq!(recovered.assets(), &live_assets);
}

#[test]
fn w1b_r8_clipboard_and_duplicate_preserve_group_properties_and_bitmap_pattern() {
    use coree_core::{
        BlendMode, CommandHistory, Effect, FeatherDirection, LensConfig, LensKind, ObjectStyle,
        RasterAsset,
    };
    use coree_storage::StorageDb;

    let dir = tempfile::tempdir().unwrap();
    let db = StorageDb::open(&dir.path().join("r8_group_clipboard.db")).unwrap();
    let mut app = StudioApp::new_with_storage(db);
    app.new_document(Document::new_a4("R8GroupProps"));
    let doc_id = app.active_tab().unwrap().document.id().to_string();
    let layer_id = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    let mut group_style = ObjectStyle::new("GroupStyle");
    group_style.fill = Some(Fill::Solid(Color::YELLOW));
    group_style.outline = Some(OutlineStyle::solid(Color::KEY_BLACK, 0.5));
    group_style.blend_mode = Some(BlendMode::Screen);
    group_style.opacity = Some(0.8);
    let style_id = group_style.id;
    let pat_asset = RasterAsset::new(2, 2, vec![0xCCu8; 16], 150.0, ColorMode::Rgb).unwrap();
    let pat_aid = pat_asset.id;

    let r1 = SceneObject::new(
        ObjectId(0),
        "Child1",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 15.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    let r2 = SceneObject::new(
        ObjectId(0),
        "Child2",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 12.0,
            height: 8.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::MAGENTA))
    .with_transform(Affine2D::translate(20.0, 0.0));

    app.execute_command(CommandOp::Batch(vec![
        CommandOp::UpsertObjectStyle { style: group_style },
        CommandOp::RegisterAsset {
            asset: pat_asset.clone(),
        },
        CommandOp::create_object(r1, Some(layer_id)),
        CommandOp::create_object(r2, Some(layer_id)),
    ]))
    .unwrap();

    let child_ids: Vec<ObjectId> = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .copied()
        .collect();
    assert_eq!(child_ids.len(), 2);
    let locked_child_id = child_ids[0];

    app.execute_command(CommandOp::Group {
        ids: child_ids.clone(),
        name: "StyledGroup".into(),
    })
    .unwrap();
    let group_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .iter()
        .find(|(_, o)| matches!(o.payload, NodePayload::Group(_)))
        .unwrap()
        .0;

    let grp_fill = Fill::BitmapPattern {
        image_id: pat_aid,
        tile_w: 6.0,
        tile_h: 6.0,
    };
    let grp_outline = OutlineStyle::solid(Color::MAGENTA, 1.25);
    let grp_lens = Some(LensConfig::new(LensKind::Invert));
    let grp_eff = Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 3.0,
        opacity: 0.6,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    };

    app.execute_command(CommandOp::Batch(vec![
        CommandOp::SetObjectStyleLink {
            ids: vec![group_id],
            style_id: Some(style_id),
        },
        CommandOp::SetFill {
            ids: vec![group_id],
            fill: grp_fill.clone(),
        },
        CommandOp::SetOutline {
            ids: vec![group_id],
            outline: grp_outline.clone(),
        },
        CommandOp::SetOpacity {
            ids: vec![group_id],
            opacity: 0.45,
        },
        CommandOp::SetBlendMode {
            ids: vec![group_id],
            blend_mode: BlendMode::Multiply,
        },
        CommandOp::SetLens {
            ids: vec![group_id],
            lens: grp_lens.clone(),
        },
        CommandOp::AddEffect {
            id: group_id,
            effect: grp_eff.clone(),
        },
        CommandOp::SetObjectVisibility {
            ids: vec![group_id],
            visible: false,
        },
        CommandOp::SetObjectPrintable {
            ids: vec![group_id],
            printable: false,
        },
        CommandOp::SetObjectLocked {
            ids: vec![locked_child_id, group_id],
            locked: true,
        },
    ]))
    .unwrap();

    // 1. duplicate_selection preserves ALL Group properties (including locked child + locked Group)
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, group_id);
    }
    let dups = app.duplicate_selection().expect("duplicate_selection");
    assert_eq!(dups.len(), 1);
    let dup_gid = dups[0];
    {
        let tab = app.active_tab().unwrap();
        let dup_grp = tab.document.object(dup_gid).unwrap();
        assert_eq!(dup_grp.style_id, Some(style_id));
        assert_eq!(dup_grp.fill, grp_fill);
        assert_eq!(dup_grp.outline, grp_outline);
        assert!((dup_grp.opacity - 0.45).abs() < 1e-6);
        assert_eq!(dup_grp.blend_mode, BlendMode::Multiply);
        assert_eq!(dup_grp.lens, grp_lens);
        assert_eq!(dup_grp.effects, vec![grp_eff.clone()]);
        assert!(!dup_grp.visible);
        assert!(!dup_grp.printable);
        assert!(dup_grp.locked);
        if let NodePayload::Group(ref children) = dup_grp.payload {
            assert_eq!(children.len(), 2);
            assert!(tab.document.object(children[0]).unwrap().locked);
        } else {
            panic!("expected Group");
        }
    }

    // Undo / redo duplicate_selection
    let after_dup = app.active_tab().unwrap().document.clone();
    app.undo().expect("undo duplicate_selection");
    assert!(!app.active_tab().unwrap().document.has_object(dup_gid));
    app.redo().expect("redo duplicate_selection");
    assert_eq!(
        app.active_tab().unwrap().document.objects(),
        after_dup.objects()
    );

    // 2. copy_selection -> delete both groups (after unlocking) -> evict asset via max_history -> paste_clipboard in SAME tab
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, group_id);
    }
    assert_eq!(app.copy_selection(), 1);

    app.execute_command(CommandOp::SetObjectLocked {
        ids: vec![group_id, locked_child_id, dup_gid],
        locked: false,
    })
    .unwrap();
    // Unlock all children of dup_gid too
    let dup_children = match &app
        .active_tab()
        .unwrap()
        .document
        .object(dup_gid)
        .unwrap()
        .payload
    {
        NodePayload::Group(c) => c.clone(),
        _ => Vec::new(),
    };
    app.execute_command(CommandOp::SetObjectLocked {
        ids: dup_children,
        locked: false,
    })
    .unwrap();
    app.execute_command(CommandOp::DeleteObjects {
        ids: vec![group_id, dup_gid],
    })
    .unwrap();
    {
        // Force asset eviction by clearing history + gc_document_assets
        let tab = app.active_tab_mut().unwrap();
        let init_doc_snap = tab.document.clone();
        tab.history = CommandHistory::new(10);
        let pruned = tab.history.gc_document_assets(&mut tab.document);
        assert_eq!(pruned, 1);
        assert!(tab.document.assets().is_empty());
        let _ = init_doc_snap;
    }

    let pasted = app.paste_clipboard();
    assert_eq!(pasted.len(), 1);
    let pasted_gid = pasted[0];
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.asset(pat_aid), Some(&pat_asset));
        let p_grp = tab.document.object(pasted_gid).unwrap();
        assert_eq!(p_grp.style_id, Some(style_id));
        assert_eq!(p_grp.fill, grp_fill);
        assert_eq!(p_grp.outline, grp_outline);
        assert!((p_grp.opacity - 0.45).abs() < 1e-6);
        assert_eq!(p_grp.blend_mode, BlendMode::Multiply);
        assert_eq!(p_grp.lens, grp_lens);
        assert_eq!(p_grp.effects, vec![grp_eff.clone()]);
        assert!(!p_grp.visible);
        assert!(!p_grp.printable);
        assert!(p_grp.locked);
    }

    // Undo / Redo & WAL recovery parity on same-tab paste
    let post_paste = app.active_tab().unwrap().document.clone();
    app.undo().expect("undo paste_clipboard");
    assert!(!app.active_tab().unwrap().document.has_object(pasted_gid));
    app.redo().expect("redo paste_clipboard");
    assert_eq!(
        app.active_tab().unwrap().document.objects(),
        post_paste.objects()
    );
    let recovered = app
        .storage
        .as_ref()
        .unwrap()
        .recover_document(&doc_id)
        .unwrap();
    assert_eq!(recovered.asset(pat_aid), Some(&pat_asset));
    assert_eq!(recovered.object(pasted_gid), post_paste.object(pasted_gid));

    // 3. Cross-tab paste into a fresh tab (no ObjectStyleId(77) in target tab) succeeds cleanly
    app.new_document(Document::new_a4("Tab2Clean"));
    let tab2_init = app.active_tab().unwrap().document.clone();
    let cross_pasted = app.paste_clipboard();
    assert_eq!(cross_pasted.len(), 1);
    let cross_gid = cross_pasted[0];
    {
        let tab2 = app.active_tab().unwrap();
        assert_eq!(tab2.document.asset(pat_aid), Some(&pat_asset));
        let cg = tab2.document.object(cross_gid).unwrap();
        assert_eq!(cg.fill, grp_fill);
        assert_eq!(cg.outline, grp_outline);
        assert!((cg.opacity - 0.45).abs() < 1e-6);
        assert_eq!(cg.blend_mode, BlendMode::Multiply);
        assert!(!cg.visible);
        assert!(!cg.printable);
        assert!(cg.locked);

        let mut replayed = tab2_init;
        CommandHistory::replay_log(&mut replayed, &tab2.history.command_log()).unwrap();
        assert_eq!(replayed.objects(), tab2.document.objects());
        assert_eq!(replayed.asset(pat_aid), Some(&pat_asset));
    }
}

#[test]
fn w1b_r10_cross_tab_paste_clipboard_sanitizes_styles_symbols_and_object_refs() {
    use coree_core::{CommandHistory, ObjectStyle, ParagraphText, RasterAsset};

    let dir = TempTestDir::new("w1b-r10-cross-tab-paste");
    let db = StorageDb::open(&dir.path().join("r10_cross_tab.db")).unwrap();
    let mut app = StudioApp::new_with_storage(db);
    app.new_document(Document::new_a4("Tab1Source"));
    let layer1 = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    // 1. Setup ObjectStyle, nested Symbols with RasterAsset, and target objects in Tab 1
    let mut obj_style = ObjectStyle::new("LeafStyle");
    obj_style.fill = Some(Fill::Solid(Color::CYAN));
    let style_id = obj_style.id;

    let sym_raster = RasterAsset::new(2, 2, vec![0x42u8; 16], 150.0, ColorMode::Rgb).unwrap();
    let sym_raster_id = sym_raster.id;
    let inner_sym = SymbolDef::new(
        "InnerSym",
        vec![SceneObject::new(
            ObjectId(1),
            "InnerImg",
            NodePayload::Raster(coree_core::RasterLayerData {
                asset_id: sym_raster_id,
                width_mm: 10.0,
                height_mm: 10.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        )],
    );
    let inner_sym_id = inner_sym.id;
    let outer_sym = SymbolDef::new(
        "OuterSym",
        vec![
            SceneObject::new(
                ObjectId(1),
                "InnerInst",
                NodePayload::SymbolInstance(inner_sym_id),
            ),
            SceneObject::new(
                ObjectId(2),
                "SymShape",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 8.0,
                    height: 8.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            ),
        ],
    );
    let outer_sym_id = outer_sym.id;

    // Uncopied anchor objects (will occupy ObjectId(1) and ObjectId(2) in Tab 1)
    let anchor_a = SceneObject::new(
        ObjectId(0),
        "AnchorA",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_transform(Affine2D::translate(10.0, 10.0));
    let anchor_b = SceneObject::new(
        ObjectId(0),
        "AnchorB",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_transform(Affine2D::translate(80.0, 20.0));
    let host_curve = SceneObject::new(
        ObjectId(0),
        "HostCurve",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(20.0, 50.0),
            Point2D::new(120.0, 50.0),
        )),
    );
    let uncopied_para = SceneObject::new(
        ObjectId(0),
        "NextPara",
        NodePayload::ParagraphText(ParagraphText::new(
            "Overflow",
            Vec2D::new(40.0, 20.0),
            TextStyle::default(),
        )),
    );

    app.execute_command(CommandOp::Batch(vec![
        CommandOp::UpsertObjectStyle { style: obj_style },
        CommandOp::RegisterAsset {
            asset: sym_raster.clone(),
        },
        CommandOp::UpsertSymbol { symbol: inner_sym },
        CommandOp::UpsertSymbol { symbol: outer_sym },
        CommandOp::create_object(anchor_a, Some(layer1)),
        CommandOp::create_object(anchor_b, Some(layer1)),
        CommandOp::create_object(host_curve, Some(layer1)),
        CommandOp::create_object(uncopied_para, Some(layer1)),
    ]))
    .unwrap();

    let tab1_ids: Vec<ObjectId> = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .copied()
        .collect();
    let (anchor_a_id, anchor_b_id, host_curve_id, next_para_id) =
        (tab1_ids[0], tab1_ids[1], tab1_ids[2], tab1_ids[3]);

    // Objects to copy:
    // - styled Shape
    // - styled PowerClip (container + styled child)
    // - SymbolInstance(outer_sym_id)
    // - Shape with Fill::VectorPattern(outer_sym_id)
    // - TextOnPath -> uncopied host_curve_id
    // - Connector -> uncopied anchor_a_id .. anchor_b_id
    // - ParagraphText -> uncopied next_para_id
    // - Blend -> uncopied anchor_b_id along uncopied host_curve_id
    // - Blend -> copied styled_shape_id along uncopied host_curve_id
    let mut styled_shape = SceneObject::new(
        ObjectId(0),
        "StyledShape",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 15.0,
            height: 12.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    styled_shape.style_id = Some(style_id);

    let mut pc_child = SceneObject::new(
        ObjectId(0),
        "PcChild",
        NodePayload::Shape(ParametricShape::Ellipse {
            rx: 5.0,
            ry: 5.0,
            mode: coree_core::EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }),
    );
    pc_child.style_id = Some(style_id);
    let mut pc_container = SceneObject::new(
        ObjectId(0),
        "StyledPowerClip",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 30.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    pc_container.style_id = Some(style_id);

    let sym_inst = SceneObject::new(
        ObjectId(0),
        "SymInst",
        NodePayload::SymbolInstance(outer_sym_id),
    );
    let vpat_rect = SceneObject::new(
        ObjectId(0),
        "VPatRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 25.0,
            height: 25.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::VectorPattern {
        symbol_id: outer_sym_id,
        tile_w: 10.0,
        tile_h: 10.0,
    });
    let text_on_path = SceneObject::new(
        ObjectId(0),
        "CurvedText",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("AlongCurve", TextStyle::default()),
            path_id: host_curve_id,
            offset_mm: 15.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    let connector = SceneObject::new(
        ObjectId(0),
        "BridgeConn",
        NodePayload::Connector(ConnectorObject::between_objects(
            anchor_a_id,
            AnchorPoint::Center,
            anchor_b_id,
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        )),
    );
    let mut linked_para = ParagraphText::new("Body", Vec2D::new(40.0, 20.0), TextStyle::default());
    linked_para.linked_next_frame = Some(next_para_id);
    let para_obj = SceneObject::new(
        ObjectId(0),
        "LinkedPara",
        NodePayload::ParagraphText(linked_para),
    );
    let mut blend_uncopied_target = SceneObject::new(
        ObjectId(0),
        "BlendDrop",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    blend_uncopied_target.effects.push(Effect::Blend {
        target_id: anchor_b_id,
        steps: 3,
        rotation_deg: 0.0,
        along_path: Some(host_curve_id),
    });

    app.execute_command(CommandOp::Batch(vec![
        CommandOp::create_object(styled_shape, Some(layer1)),
        CommandOp::create_object(pc_child, Some(layer1)),
        CommandOp::create_object(pc_container, Some(layer1)),
        CommandOp::create_object(sym_inst, Some(layer1)),
        CommandOp::create_object(vpat_rect, Some(layer1)),
        CommandOp::create_object(text_on_path, Some(layer1)),
        CommandOp::create_object(connector, Some(layer1)),
        CommandOp::create_object(para_obj, Some(layer1)),
        CommandOp::create_object(blend_uncopied_target, Some(layer1)),
    ]))
    .unwrap();

    let find_by_name = |doc: &Document, name: &str| -> ObjectId {
        doc.objects()
            .iter()
            .find(|(_, o)| o.name == name)
            .map(|(&id, _)| id)
            .unwrap()
    };

    let (styled_id, pc_cid, pc_id, sym_inst_id, vpat_id, top_id, conn_id, para_id, blend_drop_id) = {
        let d = &app.active_tab().unwrap().document;
        (
            find_by_name(d, "StyledShape"),
            find_by_name(d, "PcChild"),
            find_by_name(d, "StyledPowerClip"),
            find_by_name(d, "SymInst"),
            find_by_name(d, "VPatRect"),
            find_by_name(d, "CurvedText"),
            find_by_name(d, "BridgeConn"),
            find_by_name(d, "LinkedPara"),
            find_by_name(d, "BlendDrop"),
        )
    };

    app.execute_command(CommandOp::CreatePowerClip {
        container_id: pc_id,
        content_ids: vec![pc_cid],
    })
    .unwrap();

    // Also add a Blend whose target IS copied (styled_id), but along_path is uncopied (host_curve_id)
    let mut blend_partial = SceneObject::new(
        ObjectId(0),
        "BlendKeepTarget",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            5.0, 5.0, 10.0, 10.0,
        ))),
    );
    blend_partial.effects.push(Effect::Blend {
        target_id: styled_id,
        steps: 4,
        rotation_deg: 0.0,
        along_path: Some(host_curve_id),
    });
    app.execute_command(CommandOp::create_object(blend_partial, Some(layer1)))
        .unwrap();
    let blend_keep_id = find_by_name(&app.active_tab().unwrap().document, "BlendKeepTarget");

    // Capture expected world connector endpoints and TextOnPath anchor in Tab 1 before copying
    let (orig_conn_from, orig_conn_to) = {
        let d = &app.active_tab().unwrap().document;
        let co = d.object(conn_id).unwrap();
        let NodePayload::Connector(ref c) = co.payload else {
            panic!("expected Connector");
        };
        d.resolve_connector_endpoints_with_transform(c, &co.transform)
    };

    // Select and copy all 9 objects (deliberately omitting anchor_a, anchor_b, host_curve, next_para)
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_many(
            &tab.document,
            &[
                styled_id,
                pc_id,
                sym_inst_id,
                vpat_id,
                top_id,
                conn_id,
                para_id,
                blend_drop_id,
                blend_keep_id,
            ],
        );
    }
    assert_eq!(app.copy_selection(), 9);

    // 2. Open Tab 2 and populate unrelated objects at ObjectId(1..=4) to verify NO hijacking!
    app.new_document(Document::new_a4("Tab2Target"));
    let tab2_doc_id = app.active_tab().unwrap().document.id().to_string();
    let layer2 = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let mut trap_ops = Vec::new();
    for i in 1..=4 {
        trap_ops.push(CommandOp::create_object(
            SceneObject::new(
                ObjectId(0),
                format!("UnrelatedTrap{i}"),
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    200.0 + f64::from(i) * 10.0,
                    200.0,
                    5.0,
                    5.0,
                ))),
            ),
            Some(layer2),
        ));
    }
    app.execute_command(CommandOp::Batch(trap_ops)).unwrap();
    let tab2_before_paste = app.active_tab().unwrap().document.clone();
    let paste_offset = app.active_tab().unwrap().duplicate_offset_mm;

    let pasted_ids = app.paste_clipboard();
    assert_eq!(pasted_ids.len(), 9);

    {
        let tab2 = app.active_tab().unwrap();
        let d2 = &tab2.document;

        // No dangling ObjectStyleId on Shape, PowerClip, or PowerClip child
        let p_styled = d2.object(find_by_name(d2, "StyledShape")).unwrap();
        assert_eq!(p_styled.style_id, None);
        let p_pc = d2.object(find_by_name(d2, "StyledPowerClip")).unwrap();
        assert_eq!(p_pc.style_id, None);
        if let NodePayload::PowerClip { ref contents, .. } = p_pc.payload {
            assert_eq!(contents.len(), 1);
            assert_eq!(d2.object(contents[0]).unwrap().style_id, None);
        } else {
            panic!("expected PowerClip");
        }

        // Nested SymbolDefs and their RasterAsset exist in Tab 2
        assert!(d2.symbol(outer_sym_id).is_some());
        assert!(d2.symbol(inner_sym_id).is_some());
        assert_eq!(d2.asset(sym_raster_id), Some(&sym_raster));

        // TextOnPath with uncopied path_id was demoted to ArtisticText at shifted path anchor
        let p_top = d2.object(find_by_name(d2, "CurvedText")).unwrap();
        match &p_top.payload {
            NodePayload::ArtisticText(art) => assert_eq!(art.plain_text(), "AlongCurve"),
            other => panic!("expected demoted ArtisticText, got {other:?}"),
        }
        let expected_top_pt = Point2D::new(35.0 + paste_offset.x, 50.0 + paste_offset.y);
        let actual_top_pt = p_top.transform.transform_point(Point2D::ZERO);
        assert!(
            (actual_top_pt.x - expected_top_pt.x).abs() < 1e-4
                && (actual_top_pt.y - expected_top_pt.y).abs() < 1e-4,
            "demoted TextOnPath position mismatch: got {actual_top_pt:?}, expected {expected_top_pt:?}"
        );

        // Connector uncopied endpoints cleared to None and fallback geometry matches shifted coords
        let p_conn = d2.object(find_by_name(d2, "BridgeConn")).unwrap();
        let NodePayload::Connector(ref c2) = p_conn.payload else {
            panic!("expected Connector");
        };
        assert_eq!(c2.from_obj, None);
        assert_eq!(c2.to_obj, None);
        let (got_from, got_to) =
            d2.resolve_connector_endpoints_with_transform(c2, &p_conn.transform);
        assert!((got_from.x - (orig_conn_from.x + paste_offset.x)).abs() < 1e-4);
        assert!((got_from.y - (orig_conn_from.y + paste_offset.y)).abs() < 1e-4);
        assert!((got_to.x - (orig_conn_to.x + paste_offset.x)).abs() < 1e-4);
        assert!((got_to.y - (orig_conn_to.y + paste_offset.y)).abs() < 1e-4);

        // ParagraphText linked_next_frame cleared to None
        let p_para = d2.object(find_by_name(d2, "LinkedPara")).unwrap();
        let NodePayload::ParagraphText(ref para2) = p_para.payload else {
            panic!("expected ParagraphText");
        };
        assert_eq!(para2.linked_next_frame, None);

        // Blend with uncopied target dropped; Blend with copied target remapped & along_path cleared
        let p_blend_drop = d2.object(find_by_name(d2, "BlendDrop")).unwrap();
        assert!(p_blend_drop.effects.is_empty());
        let p_blend_keep = d2.object(find_by_name(d2, "BlendKeepTarget")).unwrap();
        assert_eq!(p_blend_keep.effects.len(), 1);
        match p_blend_keep.effects[0] {
            Effect::Blend {
                target_id,
                along_path,
                ..
            } => {
                assert_eq!(target_id, p_styled.id);
                assert_eq!(along_path, None);
            }
            ref other => panic!("expected Effect::Blend, got {other:?}"),
        }
    }

    // UnlinkSymbolInstance succeeds cleanly on cross-tab pasted SymbolInstance
    let pasted_sym_inst_id = find_by_name(&app.active_tab().unwrap().document, "SymInst");
    app.execute_command(CommandOp::UnlinkSymbolInstance {
        id: pasted_sym_inst_id,
    })
    .expect("UnlinkSymbolInstance should succeed on cross-tab pasted SymbolInstance");
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(pasted_sym_inst_id)
            .unwrap()
            .payload,
        NodePayload::Group(_)
    ));

    // Undo / Redo & WAL parity on Tab 2
    let post_unlink_doc = app.active_tab().unwrap().document.clone();
    app.undo().expect("undo unlink");
    app.undo().expect("undo cross-tab paste");
    assert_eq!(
        app.active_tab().unwrap().document.objects(),
        tab2_before_paste.objects()
    );
    assert!(app.active_tab().unwrap().document.symbols().is_empty());
    app.redo().expect("redo cross-tab paste");
    app.redo().expect("redo unlink");
    assert_eq!(
        app.active_tab().unwrap().document.objects(),
        post_unlink_doc.objects()
    );
    assert_eq!(
        app.active_tab().unwrap().document.symbols(),
        post_unlink_doc.symbols()
    );

    let recovered = app
        .storage
        .as_ref()
        .unwrap()
        .recover_document(&tab2_doc_id)
        .unwrap();
    assert_eq!(recovered.objects(), post_unlink_doc.objects());
    assert_eq!(recovered.symbols(), post_unlink_doc.symbols());
    assert_eq!(recovered.assets(), post_unlink_doc.assets());
    let _ = CommandHistory::replay_log;
}

#[test]
fn w1b_r10_import_coree_handles_object_style_links_without_dangling_ids() {
    use coree_core::{ObjectStyle, ObjectStyleId, RasterAsset};
    use coree_io::save_coree;

    let dir = TempTestDir::new("w1b-r10-import-styles");
    let coree_path = dir.path().join("styled_source.coree");

    // Build source .coree document with an ObjectStyle (referencing a BitmapPattern asset) + a dangling style_id
    let mut src_doc = Document::new_a4("StyledSource");
    let pat_asset = RasterAsset::new(2, 2, vec![0x99u8; 16], 150.0, ColorMode::Rgb).unwrap();
    let pat_aid = pat_asset.id;
    src_doc.register_asset(pat_asset.clone());

    let mut style = ObjectStyle::new("PatternStyle");
    style.fill = Some(Fill::BitmapPattern {
        image_id: pat_aid,
        tile_w: 10.0,
        tile_h: 10.0,
    });
    let valid_style_id = style.id;
    src_doc.upsert_object_style(style);

    let mut obj_valid = SceneObject::new(
        ObjectId(0),
        "ValidStyled",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    obj_valid.style_id = Some(valid_style_id);

    let mut obj_broken = SceneObject::new(
        ObjectId(0),
        "BrokenStyled",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 15.0,
            height: 15.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    obj_broken.style_id = Some(ObjectStyleId::new());

    src_doc.add_object(obj_valid, None).unwrap();
    src_doc.add_object(obj_broken, None).unwrap();
    save_coree(&src_doc, &coree_path).unwrap();

    let db = StorageDb::open(&dir.path().join("r10_import.db")).unwrap();
    let mut app = StudioApp::new_with_storage(db);
    app.new_document(Document::new_a4("TargetImport"));
    let doc_id = app.active_tab().unwrap().document.id().to_string();
    let initial_doc = app.active_tab().unwrap().document.clone();

    let imported_count = app.import_file_path(&coree_path).expect("import .coree");
    assert_eq!(imported_count, 2);

    let post_import = app.active_tab().unwrap().document.clone();
    // Every Some(style_id) in the document must resolve to an existing ObjectStyle
    for obj in post_import.objects().values() {
        if let Some(sid) = obj.style_id {
            assert!(
                post_import.object_style(sid).is_some(),
                "dangling style_id {sid:?} on imported object '{}'",
                obj.name
            );
        }
    }
    let imported_valid = post_import
        .objects()
        .values()
        .find(|o| o.name == "ValidStyled")
        .unwrap();
    assert_eq!(imported_valid.style_id, Some(valid_style_id));
    assert_eq!(post_import.asset(pat_aid), Some(&pat_asset));
    let imported_broken = post_import
        .objects()
        .values()
        .find(|o| o.name == "BrokenStyled")
        .unwrap();
    assert_eq!(imported_broken.style_id, None);

    // Undo / Redo & WAL recovery parity
    app.undo().expect("undo import");
    assert!(app.active_tab().unwrap().document.objects().is_empty());
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object_styles()
        .is_empty());
    assert_eq!(
        app.active_tab().unwrap().document.objects(),
        initial_doc.objects()
    );

    app.redo().expect("redo import");
    assert_eq!(
        app.active_tab().unwrap().document.objects(),
        post_import.objects()
    );
    assert_eq!(
        app.active_tab().unwrap().document.object_styles(),
        post_import.object_styles()
    );

    let recovered = app
        .storage
        .as_ref()
        .unwrap()
        .recover_document(&doc_id)
        .unwrap();
    assert_eq!(recovered.objects().len(), post_import.objects().len());
    for (&oid, post_obj) in post_import.objects() {
        let rec_obj = recovered.object(oid).expect("recovered object exists");
        assert_eq!(rec_obj.name, post_obj.name);
        assert_eq!(rec_obj.payload, post_obj.payload);
        assert_eq!(rec_obj.style_id, post_obj.style_id);
        for (a, b) in rec_obj.transform.m.iter().zip(post_obj.transform.m.iter()) {
            assert!((a - b).abs() < 1e-12, "transform mismatch: {a} vs {b}");
        }
    }
    assert_eq!(recovered.object_styles(), post_import.object_styles());
    assert_eq!(recovered.assets(), post_import.assets());
}

#[test]
fn w1b_r10_duplicate_page_active_page_index_undo_redo_wal_parity() {
    let dir = TempTestDir::new("w1b-r10-dup-page");
    let db = StorageDb::open(&dir.path().join("r10_dup_page.db")).unwrap();
    let mut app = StudioApp::new_with_storage(db);
    app.new_document(Document::new_a4("DupPageParity"));
    let doc_id = app.active_tab().unwrap().document.id().to_string();
    let layer_id = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    let rect = SceneObject::new(
        ObjectId(0),
        "Page1Rect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    app.execute_command(CommandOp::create_object(rect, Some(layer_id)))
        .unwrap();
    assert_eq!(app.active_tab().unwrap().document.active_page_index(), 0);
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 1);

    app.duplicate_page(0);
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 2);
    assert_eq!(app.active_tab().unwrap().document.active_page_index(), 1);
    let post_dup = app.active_tab().unwrap().document.clone();

    app.undo().expect("undo duplicate_page");
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 1);
    assert_eq!(app.active_tab().unwrap().document.active_page_index(), 0);

    app.redo().expect("redo duplicate_page");
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 2);
    assert_eq!(app.active_tab().unwrap().document.active_page_index(), 1);
    assert_eq!(app.active_tab().unwrap().document.pages(), post_dup.pages());
    assert_eq!(
        app.active_tab().unwrap().document.objects(),
        post_dup.objects()
    );

    let recovered = app
        .storage
        .as_ref()
        .unwrap()
        .recover_document(&doc_id)
        .unwrap();
    assert_eq!(recovered.pages().len(), 2);
    assert_eq!(recovered.active_page_index(), 1);
    assert_eq!(recovered.pages(), post_dup.pages());
    assert_eq!(recovered.objects(), post_dup.objects());
}

#[test]
fn w1b_r11_paste_clipboard_respects_locked_hidden_layers_and_active_page() {
    let mut app = StudioApp::new_headless();
    app.new_document(Document::new_a4("PasteLayerGuards"));
    let p1_layer = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    // Create a Shape and a Group on Page 1 and copy them
    let s1 = SceneObject::new(
        ObjectId(0),
        "CopyLeaf",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    let g_child = SceneObject::new(
        ObjectId(0),
        "GrpChild",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            30.0, 0.0, 10.0, 10.0,
        ))),
    );
    app.execute_command(CommandOp::Batch(vec![
        CommandOp::create_object(s1, Some(p1_layer)),
        CommandOp::create_object(g_child, Some(p1_layer)),
    ]))
    .unwrap();
    let p1_ids: Vec<ObjectId> = app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .clone();
    app.execute_command(CommandOp::Group {
        ids: vec![p1_ids[1]],
        name: "CopyGrp".into(),
    })
    .unwrap();
    let grp_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[1];

    // 1. Copy Shape & paste into an all-locked Page 2 -> must refuse & mutate nothing!
    let doc_snap = app.active_tab().unwrap().document.clone();
    app.active_tab_mut()
        .unwrap()
        .selection
        .select_many(&doc_snap, &[p1_ids[0], grp_id]);
    assert_eq!(app.copy_selection(), 2);

    app.execute_command(CommandOp::AddPage {
        name: "Page 2".into(),
        width_mm: 210.0,
        height_mm: 297.0,
        bleed_mm: 3.0,
    })
    .unwrap();
    let p2_l1 = app.active_tab().unwrap().document.pages()[1].layers[0].id;
    app.execute_command(CommandOp::SetDocumentProperties {
        title: None,
        color_mode: None,
        default_unit: None,
        dpi: None,
        multipage_view: None,
        active_page_index: Some(1),
        metadata: None,
        custom_icc_profile: None,
        clear_custom_icc: false,
    })
    .unwrap();

    // Case A: stale active_layer_id still points to Page 1 while Page 2 is active -> must paste onto Page 2!
    app.active_tab_mut().unwrap().active_layer_id = Some(p1_layer);
    let pasted_p2 = app.paste_clipboard();
    assert_eq!(pasted_p2.len(), 2);
    for pid in &pasted_p2 {
        assert_eq!(
            app.active_tab().unwrap().document.layer_of_object(*pid),
            Some(p2_l1),
            "paste on Page 2 with stale Page 1 active_layer_id must target Page 2"
        );
    }

    // Case B: all layers on Page 2 locked -> snapshot paste returns [] without mutating
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: p2_l1,
        visible: None,
        locked: Some(true),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    let count_before_locked = app.active_tab().unwrap().document.objects().len();
    assert!(app.paste_clipboard().is_empty());
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_locked
    );

    // Case C: all layers on Page 2 hidden -> snapshot paste returns [] without mutating
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: p2_l1,
        visible: Some(false),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    assert!(app.paste_clipboard().is_empty());
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_locked
    );

    // Case D: clipboard_objects fallback respects all-locked / hidden AND redirects from locked active_layer_id to unlocked layer
    app.clipboard_snapshot = None;
    app.clipboard_objects = vec![SceneObject::new(
        ObjectId(99),
        "FallbackRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    )];
    assert!(app.paste_clipboard().is_empty());
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_before_locked
    );

    // Unlock Page 2 layer 1 and add a locked Page 2 layer 2 set as active_layer_id -> fallback must use layer 1!
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: p2_l1,
        visible: Some(true),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    let p2_id = app.active_tab().unwrap().document.pages()[1].id;
    app.execute_command(CommandOp::AddLayer {
        page_id: p2_id,
        name: "P2Locked".into(),
        is_cut_layer: false,
        master_scope: coree_core::MasterLayerScope::None,
    })
    .unwrap();
    let p2_l2 = app.active_tab().unwrap().document.pages()[1].layers[1].id;
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: p2_l2,
        visible: None,
        locked: Some(true),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    app.active_tab_mut().unwrap().active_layer_id = Some(p2_l2);
    let fallback_pasted = app.paste_clipboard();
    assert_eq!(fallback_pasted.len(), 1);
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .layer_of_object(fallback_pasted[0]),
        Some(p2_l1)
    );
}

#[test]
fn w1b_r11_import_document_into_sanitizes_cross_page_refs_and_symbol_styles() {
    use coree_app::io_bridge::import_into_document;
    use coree_core::{
        AnchorPoint, ArtisticText, ConnectorObject, ConnectorStyle, ObjectStyleId, ParagraphText,
        TextOnPath, TextStyle, Vec2D,
    };
    use coree_io::save_coree;

    // Build multi-page src where Page 1 objects reference Page 2 objects + SymbolDef with missing ObjectStyleId
    let mut src = Document::new_a4("MultiPageSrc");
    let p1_l = src.pages()[0].layers[0].id;
    let _p2 = src.add_page("Page 2", 210.0, 297.0, 3.0);
    let p2_l = src.pages()[1].layers[0].id;

    // Allocate dummy ids so Page 2 objects sit at ObjectId(5), ObjectId(6)
    let p1_box_a = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "P1BoxA",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 10.0, 10.0,
                ))),
            ),
            Some(p1_l),
        )
        .unwrap();
    let mut sym_sub = SceneObject::new(
        ObjectId(0),
        "SymSub",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
    );
    sym_sub.style_id = Some(ObjectStyleId::new());
    let sym = SymbolDef::new("DanglingSym", vec![sym_sub]);
    let sym_id = sym.id;
    src.insert_symbol(sym);
    src.add_object(
        SceneObject::new(
            ObjectId(0),
            "P1SymInst",
            NodePayload::SymbolInstance(sym_id),
        ),
        Some(p1_l),
    )
    .unwrap();

    let p2_curve = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "P2Curve",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(40.0, 0.0),
                )),
            )
            .with_transform(Affine2D::translate(20.0, 30.0)),
            Some(p2_l),
        )
        .unwrap();
    let p2_box = src
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "P2Box",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    80.0, 40.0, 10.0, 10.0,
                ))),
            ),
            Some(p2_l),
        )
        .unwrap();

    // Page 1 objects referencing Page 2 objects
    src.add_object(
        SceneObject::new(
            ObjectId(0),
            "CrossTop",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("CrossTop", TextStyle::default()),
                path_id: p2_curve,
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        ),
        Some(p1_l),
    )
    .unwrap();
    src.add_object(
        SceneObject::new(
            ObjectId(0),
            "CrossConn",
            NodePayload::Connector(ConnectorObject::between_objects(
                p1_box_a,
                AnchorPoint::Center,
                p2_box,
                AnchorPoint::Center,
                ConnectorStyle::Straight,
            )),
        ),
        Some(p1_l),
    )
    .unwrap();
    let mut para = ParagraphText::new("Frame1", Vec2D::new(30.0, 20.0), TextStyle::default());
    para.linked_next_frame = Some(p2_box);
    src.add_object(
        SceneObject::new(ObjectId(0), "CrossPara", NodePayload::ParagraphText(para)),
        Some(p1_l),
    )
    .unwrap();
    let mut blend_obj = SceneObject::new(
        ObjectId(0),
        "CrossBlend",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    blend_obj.effects.push(Effect::Blend {
        target_id: p2_box,
        steps: 3,
        rotation_deg: 0.0,
        along_path: Some(p2_curve),
    });
    src.add_object(blend_obj, Some(p1_l)).unwrap();

    // Seed target with colliding IDs so newly allocated IDs collide with p2_curve / p2_box numeric IDs
    let mut target = Document::new_a4("CollisionTarget");
    for i in 1..=6 {
        target
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("Seed{i}"),
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 2.0, 2.0))),
                ),
                None,
            )
            .unwrap();
    }

    let imported = import_document_into(&mut target, &src);
    assert_eq!(imported.len(), 6);

    fn find_name<'a>(doc: &'a Document, name: &str) -> &'a SceneObject {
        doc.objects().values().find(|o| o.name == name).unwrap()
    }

    // 1. CrossTop demoted to ArtisticText (zero dangling/self-hijacked path_id)
    assert!(matches!(
        find_name(&target, "CrossTop").payload,
        NodePayload::ArtisticText(_)
    ));
    // 2. CrossConn cleared unmapped to_obj = None while remapping from_obj to imported P1BoxA
    let imp_a_id = find_name(&target, "P1BoxA").id;
    let NodePayload::Connector(ref c) = find_name(&target, "CrossConn").payload else {
        panic!("expected Connector");
    };
    assert_eq!(c.from_obj, Some(imp_a_id));
    assert_eq!(c.to_obj, None);
    // 3. CrossPara cleared unmapped linked_next_frame = None
    let NodePayload::ParagraphText(ref p) = find_name(&target, "CrossPara").payload else {
        panic!("expected ParagraphText");
    };
    assert_eq!(p.linked_next_frame, None);
    // 4. CrossBlend dropped unmapped Effect::Blend
    assert!(find_name(&target, "CrossBlend").effects.is_empty());
    // 5. SymbolDef sub-object dangling style_id cleared to None
    let imp_sym = target.symbol(sym_id).expect("symbol imported");
    assert_eq!(imp_sym.objects[0].style_id, None);

    // Also verify file-based import_into_document produces identical sanitization
    let dir = TempTestDir::new("w1b-r11-import-into");
    let coree_path = dir.path().join("multi.coree");
    save_coree(&src, &coree_path).unwrap();
    let mut target2 = Document::new_a4("FileTarget");
    let imported2 = import_into_document(&mut target2, &coree_path).unwrap();
    assert_eq!(imported2.len(), 6);
    assert!(matches!(
        find_name(&target2, "CrossTop").payload,
        NodePayload::ArtisticText(_)
    ));
    assert_eq!(target2.symbol(sym_id).unwrap().objects[0].style_id, None);
}

#[test]
fn w1b_r12_apply_fill_color_atomic_on_locked_and_undo_redo_wal_palette_parity() {
    use coree_storage::StorageDb;

    let dir = TempTestDir::new("w1b-r12-apply-fill-wal");
    let db = StorageDb::open(&dir.path().join("palette_wal.db")).unwrap();
    let mut app = StudioApp::new_with_storage(db);
    app.new_document(Document::new_a4("FillPaletteWAL"));

    let layer_id = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let mut locked_rect = SceneObject::new(
        ObjectId(0),
        "LockedBox",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 20.0, 20.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::WHITE));
    locked_rect.locked = true;
    let free_rect = SceneObject::new(
        ObjectId(0),
        "FreeBox",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            40.0, 10.0, 20.0, 20.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::WHITE));
    app.execute_command(CommandOp::Batch(vec![
        CommandOp::create_object(locked_rect, Some(layer_id)),
        CommandOp::create_object(free_rect, Some(layer_id)),
    ]))
    .unwrap();
    let objs = app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .clone();
    let locked_id = objs[0];
    let free_id = objs[1];

    // 1. Applying fill to a locked object must NOT mutate document_palette or object fill
    {
        let doc_snap = app.active_tab().unwrap().document.clone();
        app.active_tab_mut()
            .unwrap()
            .selection
            .select_many(&doc_snap, &[locked_id]);
    }
    app.apply_fill_color(Color::CYAN);
    {
        let tab = app.active_tab().unwrap();
        assert!(
            tab.document.document_palette().swatches.is_empty(),
            "rejected SetFill on locked object must not mutate document_palette"
        );
        assert_eq!(
            tab.document.object(locked_id).unwrap().fill,
            Fill::Solid(Color::WHITE)
        );
    }

    // 2. Applying fill to an unlocked object updates document_palette deterministically across undo/redo/WAL
    {
        let doc_snap = app.active_tab().unwrap().document.clone();
        app.active_tab_mut()
            .unwrap()
            .selection
            .select_many(&doc_snap, &[free_id]);
    }
    app.apply_fill_color(Color::MAGENTA);
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .document_palette()
            .swatches
            .len(),
        1
    );

    let doc_id = app.active_tab().unwrap().document.id().to_string();
    let recovered = app
        .storage
        .as_ref()
        .unwrap()
        .recover_document(&doc_id)
        .unwrap();
    assert_eq!(
        recovered.document_palette(),
        app.active_tab().unwrap().document.document_palette()
    );
    assert_eq!(
        recovered.objects(),
        app.active_tab().unwrap().document.objects()
    );

    app.undo().expect("undo apply_fill_color");
    assert!(
        app.active_tab()
            .unwrap()
            .document
            .document_palette()
            .swatches
            .is_empty(),
        "undo must remove added swatch from document_palette"
    );
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(free_id)
            .unwrap()
            .fill,
        Fill::Solid(Color::WHITE)
    );

    app.redo().expect("redo apply_fill_color");
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .document_palette()
            .swatches
            .len(),
        1
    );
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(free_id)
            .unwrap()
            .fill,
        Fill::Solid(Color::MAGENTA)
    );
}

#[test]
fn w1b_r12_import_respects_locked_hidden_layers_and_filters_referenced_symbols() {
    use coree_app::io_bridge::{build_import_document_ops, import_into_document};
    use coree_io::save_coree;

    // Build source document with:
    // - Page 1 rect
    // - An existing-in-target SymbolDef (same SymbolId) referenced on Page 1 (must NOT overwrite target's symbol)
    // - An unused SymbolDef with non-finite NaN transform (must NOT abort import!)
    let mut src = Document::new_a4("ImportSourceR12");
    let p1_l = src.pages()[0].layers[0].id;
    let _p2 = src.add_page("Page 2", 210.0, 297.0, 3.0);

    let shared_sym_id = coree_core::SymbolId::new();
    let src_shared_sym = SymbolDef {
        id: shared_sym_id,
        name: "SrcOverwriteAttempt".into(),
        objects: vec![SceneObject::new(
            ObjectId(0),
            "SrcSymSub",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 50.0, 50.0,
            ))),
        )],
    };
    src.insert_symbol(src_shared_sym);

    let unused_p2_sym = SymbolDef::new(
        "UnusedP2Sym",
        vec![SceneObject::new(
            ObjectId(0),
            "UnusedP2Sub",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
        )],
    );
    let unused_p2_sym_id = unused_p2_sym.id;
    src.insert_symbol(unused_p2_sym);

    let mut bad_unused_sub = SceneObject::new(
        ObjectId(0),
        "BadUnusedSub",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
    );
    bad_unused_sub.transform = Affine2D::translate(f64::NAN, 0.0);
    let unused_bad_sym = SymbolDef::new("UnusedBadSym", vec![bad_unused_sub]);
    let unused_bad_sym_id = unused_bad_sym.id;

    src.add_object(
        SceneObject::new(
            ObjectId(0),
            "ImportedRect",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 10.0, 25.0, 15.0,
            ))),
        ),
        Some(p1_l),
    )
    .unwrap();
    src.add_object(
        SceneObject::new(
            ObjectId(0),
            "SharedSymInst",
            NodePayload::SymbolInstance(shared_sym_id),
        ),
        Some(p1_l),
    )
    .unwrap();

    let mut src_with_bad_sym = src.clone();
    src_with_bad_sym.insert_symbol(unused_bad_sym);

    let dir = TempTestDir::new("w1b-r12-import-guards");
    let src_path = dir.path().join("src_r12.coree");
    save_coree(&src, &src_path).unwrap();

    // 1. All layers locked -> import_file_path, import_into_document, import_document_into, build_import_document_ops all refuse
    let mut app = StudioApp::new_headless();
    app.new_document(Document::new_a4("ImportTargetR12"));
    let tgt_p1 = app.active_tab().unwrap().document.pages()[0].id;
    let tgt_l1 = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: tgt_l1,
        visible: None,
        locked: Some(true),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();

    assert!(app.import_file_path(&src_path).is_err());
    assert!(app.active_tab().unwrap().document.objects().is_empty());

    let mut locked_doc = app.active_tab().unwrap().document.clone();
    assert!(import_into_document(&mut locked_doc, &src_path).is_err());
    assert!(import_document_into(&mut locked_doc, &src).is_empty());
    let (ops, created) = build_import_document_ops(&locked_doc, &src);
    assert!(ops.is_empty() && created.is_empty());
    assert!(locked_doc.objects().is_empty());

    // 2. All layers hidden -> all entry points refuse without mutating
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: tgt_l1,
        visible: Some(false),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    assert!(app.import_file_path(&src_path).is_err());
    assert!(app.active_tab().unwrap().document.objects().is_empty());

    let mut hidden_doc = app.active_tab().unwrap().document.clone();
    assert!(import_into_document(&mut hidden_doc, &src_path).is_err());
    assert!(import_document_into(&mut hidden_doc, &src).is_empty());
    assert!(hidden_doc.objects().is_empty());

    // 3. Layer 1 visible+unlocked (active) + Layer 2 hidden+unlocked on top -> must land on Layer 1!
    //    Also pre-populate shared_sym_id on target to verify existing target SymbolDef is preserved,
    //    and unused NaN symbol does NOT abort import!
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: tgt_l1,
        visible: Some(true),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    app.execute_command(CommandOp::AddLayer {
        page_id: tgt_p1,
        name: "TopHiddenLayer".into(),
        is_cut_layer: false,
        master_scope: coree_core::MasterLayerScope::None,
    })
    .unwrap();
    let tgt_l2 = app.active_tab().unwrap().document.pages()[0].layers[1].id;
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: tgt_l2,
        visible: Some(false),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    app.active_tab_mut().unwrap().active_layer_id = Some(tgt_l1);
    let tgt_existing_sym = SymbolDef {
        id: shared_sym_id,
        name: "TargetPreservedSymbol".into(),
        objects: vec![SceneObject::new(
            ObjectId(0),
            "TgtSymSub",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
        )],
    };
    app.execute_command(CommandOp::UpsertSymbol {
        symbol: tgt_existing_sym,
    })
    .unwrap();

    let count = app.import_file_path(&src_path).unwrap();
    assert_eq!(count, 2);
    let doc = &app.active_tab().unwrap().document;
    assert_eq!(doc.pages()[0].layers[0].objects.len(), 2);
    assert!(doc.pages()[0].layers[1].objects.is_empty());
    assert_eq!(
        doc.symbol(shared_sym_id).unwrap().name,
        "TargetPreservedSymbol"
    );
    assert!(doc.symbol(unused_p2_sym_id).is_none());

    // Direct import_document_into also lands on Layer 1 (not hidden Layer 2) and skips unused NaN symbol
    let mut direct_tgt = Document::new_a4("DirectTgt");
    let dl1 = direct_tgt.pages()[0].layers[0].id;
    let dl2 = direct_tgt
        .add_layer(
            direct_tgt.pages()[0].id,
            "HiddenTop",
            false,
            coree_core::MasterLayerScope::None,
        )
        .unwrap();
    direct_tgt
        .set_layer_flags(dl2, Some(false), Some(false), None, None)
        .unwrap();
    let direct_ids = import_document_into(&mut direct_tgt, &src_with_bad_sym);
    assert_eq!(direct_ids.len(), 2);
    for id in direct_ids {
        assert_eq!(direct_tgt.layer_of_object(id), Some(dl1));
    }
    assert!(direct_tgt.symbol(unused_bad_sym_id).is_none());
    assert!(direct_tgt.symbol(unused_p2_sym_id).is_none());
}

#[test]
fn w1b_r13_studio_creation_refuses_locked_and_hidden_active_layers_and_propagates_outline_toasts() {
    let mut app = StudioApp::new_headless();
    let l1 = app.active_tab().unwrap().active_layer_id.unwrap();

    // Seed an unlocked raster + locked vector on a second unlocked layer before locking l1
    let p1 = app.active_tab().unwrap().document.pages()[0].id;
    app.execute_command(CommandOp::AddLayer {
        page_id: p1,
        name: "Layer 2".to_owned(),
        is_cut_layer: false,
        master_scope: coree_core::MasterLayerScope::None,
    })
    .unwrap();
    let l2 = app.active_tab().unwrap().document.pages()[0].layers[1].id;

    let asset =
        coree_core::RasterAsset::new(8, 8, vec![255u8; 8 * 8 * 4], 72.0, ColorMode::Rgb).unwrap();
    let aid = asset.id;
    app.execute_command(CommandOp::RegisterAsset { asset })
        .unwrap();
    app.execute_command(CommandOp::create_object(
        SceneObject::new(
            ObjectId(0),
            "SeedRaster",
            NodePayload::Raster(coree_core::RasterLayerData {
                asset_id: aid,
                width_mm: 20.0,
                height_mm: 20.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        ),
        Some(l2),
    ))
    .unwrap();
    let raster_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();

    let mut locked_vec = SceneObject::new(
        ObjectId(0),
        "LockedVec",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_outline(OutlineStyle::solid(Color::BLACK, 1.0));
    locked_vec.locked = true;
    app.execute_command(CommandOp::create_object(locked_vec, Some(l2)))
        .unwrap();
    let locked_vec_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();

    // Lock l1 while keeping tab.active_layer_id == Some(l1)
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: l1,
        visible: None,
        locked: Some(true),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.active_layer_id = Some(l1);
        tab.selection.select_single(&tab.document, raster_id);
    }

    let obj_count_before = app.active_tab().unwrap().document.objects().len();

    // 1. Drawing rectangle, inserting table, page frame, glyph, raster->bitmap, and vector trace must refuse
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );
    assert!(app.toasts.last().is_some_and(|t| t.is_error));

    app.toasts.clear();
    app.insert_table(2, 2);
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );
    assert!(app
        .toasts
        .last()
        .is_some_and(|t| t.is_error && !t.message.contains("Inserted")));

    app.toasts.clear();
    assert!(app.create_page_frame_rectangle(false).is_none());
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );
    assert!(app.toasts.last().is_some_and(|t| t.is_error));

    app.toasts.clear();
    app.insert_glyph_at_selection("A");
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );
    assert!(app.toasts.last().is_some_and(|t| t.is_error));

    app.toasts.clear();
    app.convert_selection_to_bitmap();
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );
    assert!(app.toasts.last().is_some_and(|t| t.is_error));

    assert!(app.trace_selected_raster().is_err());
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );

    // 2. Hidden unlocked active_layer_id must also refuse interactive UI creation (desktop studio parity)
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: l1,
        visible: Some(false),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    app.toasts.clear();
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );
    assert!(app.toasts.last().is_some_and(|t| t.is_error));
    assert!(app.create_page_frame_rectangle(false).is_none());
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        obj_count_before
    );

    // 3. apply_outline_color & apply_outline propagate error toasts when target object is locked
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, locked_vec_id);
    }
    app.toasts.clear();
    app.apply_outline_color(Color::MAGENTA);
    assert!(app.toasts.last().is_some_and(|t| t.is_error));
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(locked_vec_id)
            .unwrap()
            .outline
            .color,
        Color::BLACK
    );
    app.toasts.clear();
    app.apply_outline(OutlineStyle::solid(Color::CYAN, 2.0));
    assert!(app.toasts.last().is_some_and(|t| t.is_error));
    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .object(locked_vec_id)
            .unwrap()
            .outline
            .width_mm
            - 1.0)
            .abs()
            < 1e-6
    );
}

#[test]
fn w1b_r13_import_invalid_object_style_with_bad_vector_pattern_symbol_is_sanitized() {
    let bad_sym_id = coree_core::SymbolId::new();
    let mut bad_style = coree_core::ObjectStyle::new("BrokenStyle");
    bad_style.fill = Some(Fill::VectorPattern {
        symbol_id: bad_sym_id,
        tile_w: 10.0,
        tile_h: 10.0,
    });
    let mut bad_outline = OutlineStyle::solid(Color::BLACK, 1.0);
    bad_outline.width_mm = -3.0;
    bad_style.outline = Some(bad_outline);
    let bad_style_id = bad_style.id;

    let mut src = Document::new_a4("SrcInvalidStyleVectorPattern");
    src.insert_symbol(SymbolDef {
        id: bad_sym_id,
        name: "MalformedSym".into(),
        objects: vec![SceneObject::new(
            ObjectId(1),
            "BadSymShape",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 10.0,
                height: 10.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_transform(Affine2D::translate(f64::NAN, 0.0))],
    });
    src.upsert_object_style(bad_style);

    let mut styled_rect = SceneObject::new(
        ObjectId(0),
        "StyledRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    styled_rect.style_id = Some(bad_style_id);
    src.add_object(styled_rect, None).unwrap();
    src.add_object(
        SceneObject::new(
            ObjectId(0),
            "PlainRect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 15.0,
                height: 15.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        ),
        None,
    )
    .unwrap();

    let mut target = Document::new_a4("Target");
    let (ops, created_ids) = coree_app::io_bridge::build_import_document_ops(&target, &src);
    assert_eq!(created_ids.len(), 2);
    assert!(
        !ops.iter()
            .any(|op| matches!(op, CommandOp::UpsertSymbol { symbol } if symbol.id == bad_sym_id)),
        "discarded invalid ObjectStyle must never emit UpsertSymbol for its bad VectorPattern symbol"
    );
    assert!(
        !ops.iter().any(
            |op| matches!(op, CommandOp::UpsertObjectStyle { style } if style.id == bad_style_id)
        ),
        "invalid ObjectStyle must be discarded before emitting ops"
    );

    let imported = import_document_into(&mut target, &src);
    assert_eq!(imported.len(), 2);
    assert!(target.symbol(bad_sym_id).is_none());
    assert!(target.object_style(bad_style_id).is_none());
    for oid in &imported {
        assert!(target.object(*oid).unwrap().style_id.is_none());
    }

    // Verify full StudioApp::import_file_path + undo/redo parity with a JSON-serializable bad style
    let mut disk_src = src.clone();
    if let Some(sym) = disk_src.symbols().get(&bad_sym_id).cloned() {
        let mut clean_sym = sym;
        clean_sym.objects[0].transform = Affine2D::IDENTITY;
        disk_src.insert_symbol(clean_sym);
    }
    let dir = TempTestDir::new("r13-bad-style-sym");
    let src_path = dir.path().join("src_bad_style_sym.coree");
    coree_io::save_coree(&disk_src, &src_path).unwrap();
    let mut app = StudioApp::new_headless();
    let count = app.import_file_path(&src_path).unwrap();
    assert_eq!(count, 2);
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .symbol(bad_sym_id)
        .is_none());
    app.undo().unwrap();
    assert!(app.active_tab().unwrap().document.objects().is_empty());
    app.redo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 2);
}

#[test]
fn w1b_r14_apply_knife_cut_enclosing_layer_and_preserves_metadata() {
    use coree_core::{BlendMode, ObjectStyle};

    let mut app = StudioApp::new_headless();
    let p1 = app.active_tab().unwrap().document.pages()[0].id;
    let l1 = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    app.execute_command(CommandOp::AddLayer {
        page_id: p1,
        name: "Layer 2".into(),
        is_cut_layer: false,
        master_scope: coree_core::MasterLayerScope::None,
    })
    .unwrap();
    let l2 = app.active_tab().unwrap().document.pages()[0].layers[1].id;

    let style = ObjectStyle::new("KnifeStyle");
    let style_id = style.id;
    app.execute_command(CommandOp::UpsertObjectStyle { style })
        .unwrap();

    let mut target = SceneObject::new(
        ObjectId(0),
        "SliceMe",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 40.0, 40.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgb(1.0, 0.0, 0.0)));
    target.effects.push(Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 3.0,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: coree_core::FeatherDirection::Middle,
    });
    target.lens = Some(coree_core::LensConfig::new(LensKind::Brighten {
        rate_pct: 25.0,
    }));
    target.style_id = Some(style_id);
    target.opacity = 0.75;
    target.blend_mode = BlendMode::Multiply;
    target.printable = false;
    app.execute_command(CommandOp::create_object(target, Some(l1)))
        .unwrap();
    let oid = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();

    // Active layer is locked Layer 2, while target sits on unlocked visible Layer 1
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: l2,
        visible: Some(true),
        locked: Some(true),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.active_layer_id = Some(l2);
        tab.selection.select_single(&tab.document, oid);
    }

    let count = app.apply_knife_cut(Point2D::new(30.0, 0.0), Point2D::new(30.0, 60.0));
    assert_eq!(count, 2);
    let doc = &app.active_tab().unwrap().document;
    assert!(!doc.has_object(oid));
    assert!(doc.find_layer(l2).unwrap().objects.is_empty());
    let l1_objs = &doc.find_layer(l1).unwrap().objects;
    assert_eq!(l1_objs.len(), 2);
    for &sid in l1_objs {
        let s = doc.object(sid).unwrap();
        assert_eq!(s.effects.len(), 1);
        assert!(s.lens.is_some());
        assert_eq!(s.style_id, Some(style_id));
        assert!((s.opacity - 0.75).abs() < 1e-6);
        assert_eq!(s.blend_mode, BlendMode::Multiply);
        assert!(!s.printable);
    }

    // Undo and re-test when active layer is hidden Layer 2
    app.undo().unwrap();
    assert!(app.active_tab().unwrap().document.has_object(oid));
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: l2,
        visible: Some(false),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.active_layer_id = Some(l2);
        tab.selection.select_single(&tab.document, oid);
    }
    let count2 = app.apply_knife_cut(Point2D::new(30.0, 0.0), Point2D::new(30.0, 60.0));
    assert_eq!(count2, 2);
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .find_layer(l1)
            .unwrap()
            .objects
            .len(),
        2
    );
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l2)
        .unwrap()
        .objects
        .is_empty());
}

#[test]
fn w1b_r14_break_apart_group_child_with_effects_resolves_enclosing_layer() {
    let mut app = StudioApp::new_headless();
    let p1 = app.active_tab().unwrap().document.pages()[0].id;
    let l1 = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    app.execute_command(CommandOp::AddLayer {
        page_id: p1,
        name: "Layer 2".into(),
        is_cut_layer: false,
        master_scope: coree_core::MasterLayerScope::None,
    })
    .unwrap();
    let l2 = app.active_tab().unwrap().document.pages()[0].layers[1].id;

    let mut child = SceneObject::new(
        ObjectId(0),
        "ShadowBox",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 20.0, 20.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgb(0.0, 0.0, 1.0)));
    child.effects.push(Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 3.0,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: coree_core::FeatherDirection::Middle,
    });
    app.execute_command(CommandOp::create_object(child, Some(l1)))
        .unwrap();
    let child_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    let sib = SceneObject::new(
        ObjectId(0),
        "Sib",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            50.0, 10.0, 10.0, 10.0,
        ))),
    );
    app.execute_command(CommandOp::create_object(sib, Some(l1)))
        .unwrap();
    let sib_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    app.execute_command(CommandOp::Group {
        ids: vec![child_id, sib_id],
        name: "G".into(),
    })
    .unwrap();

    // Locked active Layer 2 must not block Break Apart on Layer 1's Group child
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: l2,
        visible: Some(true),
        locked: Some(true),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.active_layer_id = Some(l2);
        tab.selection.select_single(&tab.document, child_id);
    }
    app.break_apart_selection();
    assert!(!app.active_tab().unwrap().document.has_object(child_id));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l2)
        .unwrap()
        .objects
        .is_empty());
    assert!(
        app.active_tab()
            .unwrap()
            .document
            .find_layer(l1)
            .unwrap()
            .objects
            .len()
            >= 2
    );
    assert!(!app.toasts.last().is_some_and(|t| t.is_error));

    // Undo and re-test with hidden active Layer 2
    app.undo().unwrap();
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: l2,
        visible: Some(false),
        locked: Some(false),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.active_layer_id = Some(l2);
        tab.selection.select_single(&tab.document, child_id);
    }
    app.break_apart_selection();
    assert!(!app.active_tab().unwrap().document.has_object(child_id));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l2)
        .unwrap()
        .objects
        .is_empty());
    assert!(
        app.active_tab()
            .unwrap()
            .document
            .find_layer(l1)
            .unwrap()
            .objects
            .len()
            >= 2
    );
}

#[test]
fn w1b_r14_duplicate_selection_deduplicates_ancestor_and_descendant() {
    let mut app = StudioApp::new_headless();
    let l1 = app.active_tab().unwrap().active_layer_id.unwrap();
    for (name, x) in [("A", 10.0), ("B", 30.0)] {
        app.execute_command(CommandOp::create_object(
            SceneObject::new(
                ObjectId(0),
                name,
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    x, 10.0, 10.0, 10.0,
                ))),
            ),
            Some(l1),
        ))
        .unwrap();
    }
    let ids: Vec<ObjectId> = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .copied()
        .collect();
    let child_id = ids[0];
    app.execute_command(CommandOp::Group {
        ids: ids.clone(),
        name: "Grp".into(),
    })
    .unwrap();
    let group_id = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 3);

    // Order 1: [child_id, group_id]
    app.active_tab_mut().unwrap().selection.objects = vec![child_id, group_id];
    let created1 = app.duplicate_selection().unwrap();
    assert_eq!(created1.len(), 1);
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 6);
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 3);

    // Order 2: [group_id, child_id]
    app.active_tab_mut().unwrap().selection.objects = vec![group_id, child_id];
    let created2 = app.duplicate_selection().unwrap();
    assert_eq!(created2.len(), 1);
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 6);
}

#[test]
fn w1b_r14_apply_raster_filter_locked_emits_error_toast() {
    use coree_core::{RasterAsset, RasterFilter, RasterLayerData};

    let mut app = StudioApp::new_headless();
    let l1 = app.active_tab().unwrap().active_layer_id.unwrap();
    let asset = RasterAsset::new(2, 2, vec![255; 16], 72.0, ColorMode::Rgb).unwrap();
    let asset_id = asset.id;
    app.execute_command(CommandOp::RegisterAsset { asset })
        .unwrap();
    let mut raster = SceneObject::new(
        ObjectId(0),
        "LockedImg",
        NodePayload::Raster(RasterLayerData {
            asset_id,
            width_mm: 20.0,
            height_mm: 20.0,
            filters: Vec::new(),
            crop_rect: None,
        }),
    );
    raster.locked = true;
    app.execute_command(CommandOp::create_object(raster, Some(l1)))
        .unwrap();
    let rid = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    app.active_tab_mut().unwrap().selection.objects = vec![rid];
    app.toasts.clear();

    app.apply_raster_filter_to_selection(RasterFilter::BrightnessContrast {
        brightness: 0.2,
        contrast: 0.1,
    });
    assert!(app.toasts.iter().any(|t| t.is_error));
    assert!(!app
        .toasts
        .iter()
        .any(|t| !t.is_error && t.message.contains("Applied non-destructive bitmap filter")));
    let NodePayload::Raster(ref r) = app
        .active_tab()
        .unwrap()
        .document
        .object(rid)
        .unwrap()
        .payload
    else {
        panic!("expected Raster");
    };
    assert!(r.filters.is_empty());
}

#[test]
fn w1b_r14_import_sanitizes_invalid_symbols_and_assets_on_valid_styles_and_objects() {
    use coree_core::{ObjectStyle, RasterAsset};

    let mut src = Document::new_a4("BadSymAssetSrc");

    let mut bad_sub = SceneObject::new(
        ObjectId(0),
        "NaNSub",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
    );
    bad_sub.transform = Affine2D::translate(f64::NAN, 0.0);
    let bad_sym = SymbolDef::new("BadSym", vec![bad_sub]);
    let bad_sym_id = bad_sym.id;
    src.insert_symbol(bad_sym);

    let mut bad_asset = RasterAsset::new(2, 2, vec![255; 16], 72.0, ColorMode::Rgb).unwrap();
    bad_asset.rgba_bytes.pop(); // invalid length
    let bad_asset_id = bad_asset.id;
    src.insert_asset_if_absent(bad_asset);

    let mut valid_style = ObjectStyle::new("ValidStyleWithBadSym");
    valid_style.fill = Some(Fill::VectorPattern {
        symbol_id: bad_sym_id,
        tile_w: 10.0,
        tile_h: 10.0,
    });
    let valid_style_id = valid_style.id;
    src.upsert_object_style(valid_style);

    let mut styled_rect = SceneObject::new(
        ObjectId(0),
        "StyledRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 20.0, 20.0,
        ))),
    );
    styled_rect.style_id = Some(valid_style_id);
    src.add_object(styled_rect, None).unwrap();

    let pat_rect = SceneObject::new(
        ObjectId(0),
        "BadAssetPatRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            40.0, 10.0, 20.0, 20.0,
        ))),
    )
    .with_fill(Fill::BitmapPattern {
        image_id: bad_asset_id,
        tile_w: 10.0,
        tile_h: 10.0,
    });
    src.add_object(pat_rect, None).unwrap();

    src.add_object(
        SceneObject::new(
            ObjectId(0),
            "BrokenSymInst",
            NodePayload::SymbolInstance(bad_sym_id),
        ),
        None,
    )
    .unwrap();

    let mut target = Document::new_a4("Target");
    let (ops, created_ids) = coree_app::io_bridge::build_import_document_ops(&target, &src);
    assert_eq!(created_ids.len(), 2);
    assert!(!ops
        .iter()
        .any(|op| matches!(op, CommandOp::UpsertSymbol { symbol } if symbol.id == bad_sym_id)));
    assert!(!ops
        .iter()
        .any(|op| matches!(op, CommandOp::RegisterAsset { asset } if asset.id == bad_asset_id)));

    let imported = import_document_into(&mut target, &src);
    assert_eq!(imported.len(), 2);
    assert!(target.symbol(bad_sym_id).is_none());
    assert!(target.asset(bad_asset_id).is_none());
    assert_eq!(
        target.object_style(valid_style_id).unwrap().fill,
        Some(Fill::None)
    );
    for oid in &imported {
        let o = target.object(*oid).unwrap();
        assert!(!matches!(o.payload, NodePayload::SymbolInstance(_)));
        if o.name == "BadAssetPatRect" {
            assert_eq!(o.fill, Fill::None);
        }
    }
}

#[test]
fn w1b_r14_duplicate_page_preserves_facing_pages() {
    let mut app = StudioApp::new_headless();
    let p1_id = app.active_tab().unwrap().document.pages()[0].id;
    app.execute_command(CommandOp::SetPageProperties {
        page_id: p1_id,
        name: None,
        width_mm: None,
        height_mm: None,
        bleed_mm: None,
        facing_pages: Some(true),
    })
    .unwrap();
    app.duplicate_page(0);
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 2);
    assert!(app.active_tab().unwrap().document.pages()[1].facing_pages);
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.pages().len(), 1);
    app.redo().unwrap();
    assert!(app.active_tab().unwrap().document.pages()[1].facing_pages);
}

#[test]
fn w1b_r15_apply_knife_cut_preserves_group_and_group_in_powerclip_hierarchy() {
    let mut app = StudioApp::new_headless();
    let l1 = app.active_tab().unwrap().active_layer_id.unwrap();

    // 1. Group with [before, target, after] translated by (100, 50)
    for (name, rect) in [
        ("Before", Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0)),
        ("Target", Rect2D::from_xywh(10.0, 10.0, 40.0, 40.0)),
        ("After", Rect2D::from_xywh(60.0, 0.0, 5.0, 5.0)),
    ] {
        app.execute_command(CommandOp::create_object(
            SceneObject::new(
                ObjectId(0),
                name,
                NodePayload::Path(BezierPath::from_rect(rect)),
            ),
            Some(l1),
        ))
        .unwrap();
    }
    let layer_objs = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects
        .clone();
    let (before_id, target_id, after_id) = (layer_objs[0], layer_objs[1], layer_objs[2]);
    app.execute_command(CommandOp::Group {
        ids: vec![before_id, target_id, after_id],
        name: "TranslatedGrp".into(),
    })
    .unwrap();
    let grp_id = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![grp_id],
        affine: Affine2D::translate(100.0, 50.0),
    })
    .unwrap();

    // Slice Target along vertical line at world x = 130 (passes through [110..150, 60..100])
    app.active_tab_mut().unwrap().selection.objects = vec![target_id];
    let slices = app.apply_knife_cut(Point2D::new(130.0, 50.0), Point2D::new(130.0, 110.0));
    assert_eq!(slices, 2);

    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.find_layer(l1).unwrap().objects, vec![grp_id]);
        let NodePayload::Group(ref children) = tab.document.object(grp_id).unwrap().payload else {
            panic!("expected Group");
        };
        assert_eq!(children.len(), 4);
        assert_eq!(children[0], before_id);
        assert_eq!(children[3], after_id);
        let b1 = tab.document.world_object_bounds(children[1]).unwrap();
        let b2 = tab.document.world_object_bounds(children[2]).unwrap();
        let combined = b1.union(b2);
        assert!((combined.min_x - 110.0).abs() < 1e-4, "got {combined:?}");
        assert!((combined.min_y - 60.0).abs() < 1e-4, "got {combined:?}");
        assert!((combined.max_x - 150.0).abs() < 1e-4, "got {combined:?}");
        assert!((combined.max_y - 100.0).abs() < 1e-4, "got {combined:?}");
    }

    app.undo().unwrap();
    {
        let tab = app.active_tab().unwrap();
        let NodePayload::Group(ref children) = tab.document.object(grp_id).unwrap().payload else {
            panic!("expected Group");
        };
        assert_eq!(children, &[before_id, target_id, after_id]);
    }
    app.redo().unwrap();
    {
        let tab = app.active_tab().unwrap();
        let NodePayload::Group(ref children) = tab.document.object(grp_id).unwrap().payload else {
            panic!("expected Group");
        };
        assert_eq!(children.len(), 4);
    }

    // 2. Group nested inside a translated PowerClip: slices must remain in Group AND PowerClip!
    let mut app2 = StudioApp::new_headless();
    let l2 = app2.active_tab().unwrap().active_layer_id.unwrap();
    app2.execute_command(CommandOp::create_object(
        SceneObject::new(
            ObjectId(0),
            "PCClip",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 100.0, 100.0,
            ))),
        )
        .with_transform(Affine2D::translate(20.0, 30.0)),
        Some(l2),
    ))
    .unwrap();
    app2.execute_command(CommandOp::create_object(
        SceneObject::new(
            ObjectId(0),
            "InnerTarget",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                30.0, 40.0, 40.0, 40.0,
            ))),
        ),
        Some(l2),
    ))
    .unwrap();
    let objs2 = app2
        .active_tab()
        .unwrap()
        .document
        .find_layer(l2)
        .unwrap()
        .objects
        .clone();
    let (pc_id, inner_id) = (objs2[0], objs2[1]);
    app2.execute_command(CommandOp::Group {
        ids: vec![inner_id],
        name: "InnerGrp".into(),
    })
    .unwrap();
    let inner_grp = app2
        .active_tab()
        .unwrap()
        .document
        .find_layer(l2)
        .unwrap()
        .objects[1];
    app2.execute_command(CommandOp::CreatePowerClip {
        container_id: pc_id,
        content_ids: vec![inner_grp],
    })
    .unwrap();

    // World bounds of inner_id are [30..70, 40..80]; slice vertically at x = 50
    app2.active_tab_mut().unwrap().selection.objects = vec![inner_id];
    let slices2 = app2.apply_knife_cut(Point2D::new(50.0, 35.0), Point2D::new(50.0, 85.0));
    assert_eq!(slices2, 2);
    {
        let tab = app2.active_tab().unwrap();
        assert_eq!(tab.document.find_layer(l2).unwrap().objects, vec![pc_id]);
        let NodePayload::PowerClip { ref contents, .. } =
            tab.document.object(pc_id).unwrap().payload
        else {
            panic!("expected PowerClip");
        };
        assert_eq!(contents, &[inner_grp]);
        let NodePayload::Group(ref children) = tab.document.object(inner_grp).unwrap().payload
        else {
            panic!("expected Group");
        };
        assert_eq!(children.len(), 2);
        let b1 = tab.document.world_object_bounds(children[0]).unwrap();
        let b2 = tab.document.world_object_bounds(children[1]).unwrap();
        let combined = b1.union(b2);
        assert!((combined.min_x - 30.0).abs() < 1e-4, "got {combined:?}");
        assert!((combined.max_x - 70.0).abs() < 1e-4, "got {combined:?}");
    }
    app2.undo().unwrap();
    {
        let tab = app2.active_tab().unwrap();
        let NodePayload::Group(ref children) = tab.document.object(inner_grp).unwrap().payload
        else {
            panic!("expected Group");
        };
        assert_eq!(children, &[inner_id]);
    }
}

#[test]
fn w1b_r15_copy_selection_deduplicates_ancestor_and_descendant() {
    let mut app = StudioApp::new_headless();
    let l1 = app.active_tab().unwrap().active_layer_id.unwrap();
    for (name, x) in [("A", 10.0), ("B", 30.0)] {
        app.execute_command(CommandOp::create_object(
            SceneObject::new(
                ObjectId(0),
                name,
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    x, 10.0, 10.0, 10.0,
                ))),
            ),
            Some(l1),
        ))
        .unwrap();
    }
    let ids: Vec<ObjectId> = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .copied()
        .collect();
    let child_id = ids[0];
    app.execute_command(CommandOp::Group {
        ids,
        name: "Grp".into(),
    })
    .unwrap();
    let group_id = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 3);

    // Order 1: [child_id, group_id]
    app.active_tab_mut().unwrap().selection.objects = vec![child_id, group_id];
    assert_eq!(app.copy_selection(), 1);
    assert_eq!(app.paste_clipboard().len(), 1);
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 6);
    app.undo().unwrap();
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 3);

    // Order 2: [group_id, child_id]
    app.active_tab_mut().unwrap().selection.objects = vec![group_id, child_id];
    assert_eq!(app.copy_selection(), 1);
    assert_eq!(app.paste_clipboard().len(), 1);
    assert_eq!(app.active_tab().unwrap().document.objects().len(), 6);
}

#[test]
fn w1b_r15_convert_to_bitmap_and_trace_raster_preserve_nested_world_coords() {
    use coree_core::{RasterAsset, RasterLayerData};

    let mut app = StudioApp::new_headless();
    let l1 = app.active_tab().unwrap().active_layer_id.unwrap();

    // 1. Child rect [5..15, 5..15] inside a Group translated by (100, 100) and scaled 2x -> world [110..130, 110..130]
    app.execute_command(CommandOp::create_object(
        SceneObject::new(
            ObjectId(0),
            "NestedRect",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                5.0, 5.0, 10.0, 10.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
        Some(l1),
    ))
    .unwrap();
    let child_id = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    app.execute_command(CommandOp::Group {
        ids: vec![child_id],
        name: "ScaledGrp".into(),
    })
    .unwrap();
    let grp_id = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![grp_id],
        affine: Affine2D::translate(100.0, 100.0).compose(&Affine2D::scale(2.0, 2.0)),
    })
    .unwrap();
    let child_world = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!((child_world.min_x - 110.0).abs() < 1e-6);
    assert!((child_world.max_x - 130.0).abs() < 1e-6);

    app.active_tab_mut().unwrap().selection.objects = vec![child_id];
    app.bitmap_state.convert_dpi = 72.0;
    app.convert_selection_to_bitmap();
    let bmp_id = app.active_tab().unwrap().selection.objects[0];
    assert_ne!(bmp_id, child_id);
    let bmp_world = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(bmp_id)
        .unwrap();
    let expected_pad = 4.0
        + app
            .active_tab()
            .unwrap()
            .document
            .spatial_bounds_padding_mm(child_id);
    let expected_padded = child_world.expand(expected_pad);
    assert!(
        (bmp_world.min_x - expected_padded.min_x).abs() < 1e-6
            && (bmp_world.min_y - expected_padded.min_y).abs() < 1e-6
            && (bmp_world.max_x - expected_padded.max_x).abs() < 1e-6
            && (bmp_world.max_y - expected_padded.max_y).abs() < 1e-6,
        "converted bitmap must align 1:1 with padded world bounds {expected_padded:?}, got {bmp_world:?}"
    );
    if let NodePayload::Raster(ref r) = app
        .active_tab()
        .unwrap()
        .document
        .object(bmp_id)
        .unwrap()
        .payload
    {
        let asset = app
            .active_tab()
            .unwrap()
            .document
            .asset(r.asset_id)
            .unwrap();
        assert!(
            asset
                .rgba_bytes
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] > 0),
            "converted nested bitmap must contain rendered non-transparent pixels"
        );
    } else {
        panic!("expected Raster payload");
    }

    // 2. Trace a Raster inside a translated Group -> traced vectors must align in world coords
    let mut px = vec![255u8; 20 * 20 * 4];
    for y in 4..16 {
        for x in 4..16 {
            let idx = (y * 20 + x) * 4;
            px[idx] = 0;
            px[idx + 1] = 0;
            px[idx + 2] = 0;
            px[idx + 3] = 255;
        }
    }
    let asset = RasterAsset::new(20, 20, px, 72.0, ColorMode::Rgb).unwrap();
    let asset_id = asset.id;
    app.execute_command(CommandOp::RegisterAsset { asset })
        .unwrap();
    let r_obj = SceneObject::new(
        ObjectId(0),
        "NestedRaster",
        NodePayload::Raster(RasterLayerData {
            asset_id,
            width_mm: 20.0,
            height_mm: 20.0,
            filters: Vec::new(),
            crop_rect: None,
        }),
    );
    app.execute_command(CommandOp::create_object(r_obj, Some(l1)))
        .unwrap();
    let r_id = *app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects
        .last()
        .unwrap();
    app.execute_command(CommandOp::Group {
        ids: vec![r_id],
        name: "RasterGrp".into(),
    })
    .unwrap();
    let r_grp = *app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects
        .last()
        .unwrap();
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![r_grp],
        affine: Affine2D::translate(100.0, 100.0),
    })
    .unwrap();

    app.active_tab_mut().unwrap().selection.objects = vec![r_id];
    let traced_count = app.trace_selected_raster().unwrap();
    assert!(traced_count >= 1);
    let traced_id = app.active_tab().unwrap().selection.objects[0];
    let traced_world = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(traced_id)
        .unwrap();
    assert!(
        traced_world.min_x >= 100.0 && traced_world.max_x <= 120.0
            && traced_world.min_y >= 100.0 && traced_world.max_y <= 120.0,
        "traced vector curves must align with raster world bounds [100..120, 100..120], got {traced_world:?}"
    );
}

#[test]
fn w1b_r15_selected_local_bounds_nested_group_and_symbol_instance() {
    let mut app = StudioApp::new_headless();
    let l1 = app.active_tab().unwrap().active_layer_id.unwrap();

    // Nested Group inside an outer Group translated by (100, 50)
    app.execute_command(CommandOp::create_object(
        SceneObject::new(
            ObjectId(0),
            "Leaf",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 10.0, 30.0, 20.0,
            ))),
        ),
        Some(l1),
    ))
    .unwrap();
    let leaf_id = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    app.execute_command(CommandOp::Group {
        ids: vec![leaf_id],
        name: "InnerGrp".into(),
    })
    .unwrap();
    let inner_grp = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    app.execute_command(CommandOp::Group {
        ids: vec![inner_grp],
        name: "OuterGrp".into(),
    })
    .unwrap();
    let outer_grp = app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects[0];
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![outer_grp],
        affine: Affine2D::translate(100.0, 50.0),
    })
    .unwrap();

    app.active_tab_mut().unwrap().selection.objects = vec![inner_grp];
    let local_bb = app.selected_local_bounds().unwrap();
    assert!(
        (local_bb.min_x - 10.0).abs() < 1e-6
            && (local_bb.min_y - 10.0).abs() < 1e-6
            && (local_bb.max_x - 40.0).abs() < 1e-6
            && (local_bb.max_y - 30.0).abs() < 1e-6,
        "selected_local_bounds on nested Group must return [10..40, 10..30], got {local_bb:?}"
    );

    // Nested SymbolInstance inside OuterGrp
    let sym = SymbolDef::new(
        "BoxSym",
        vec![SceneObject::new(
            ObjectId(0),
            "SymBox",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                5.0, 8.0, 15.0, 12.0,
            ))),
        )],
    );
    let sym_id = sym.id;
    app.execute_command(CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();
    app.execute_command(CommandOp::InstantiateSymbol {
        symbol_id: sym_id,
        layer_id: Some(l1),
        transform: Affine2D::translate(200.0, 200.0),
        name: None,
    })
    .unwrap();
    let inst_id = *app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects
        .last()
        .unwrap();
    app.execute_command(CommandOp::Group {
        ids: vec![inst_id],
        name: "SymOuter".into(),
    })
    .unwrap();
    let sym_outer = *app
        .active_tab()
        .unwrap()
        .document
        .find_layer(l1)
        .unwrap()
        .objects
        .last()
        .unwrap();
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![sym_outer],
        affine: Affine2D::translate(50.0, 75.0),
    })
    .unwrap();

    app.active_tab_mut().unwrap().selection.objects = vec![inst_id];
    let sym_local = app.selected_local_bounds().unwrap();
    assert!(
        (sym_local.min_x - 5.0).abs() < 1e-6
            && (sym_local.min_y - 8.0).abs() < 1e-6
            && (sym_local.max_x - 20.0).abs() < 1e-6
            && (sym_local.max_y - 20.0).abs() < 1e-6,
        "selected_local_bounds on nested SymbolInstance must return [5..20, 8..20], got {sym_local:?}"
    );
}

#[test]
fn w1b_r16_object_local_bounds_rotated_group_symbol_connector() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let rot45 =
        Affine2D::translate(50.0, 40.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_4));

    let bar_id = {
        let tab = app.active_tab_mut().unwrap();
        let bar = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Bar",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 100.0, 10.0,
                    ))),
                ),
                Some(lid),
            )
            .unwrap();
        let grp = tab.document.group_objects(&[bar], "RotBarGrp").unwrap();
        tab.document.transform_object(grp, rot45).unwrap();
        let pc = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "OuterPC",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 200.0, 200.0,
                    ))),
                )
                .with_transform(Affine2D::rotate(0.52)),
                Some(lid),
            )
            .unwrap();
        tab.document.create_powerclip(pc, &[grp]).unwrap();
        let bb = StudioApp::object_local_bounds(&tab.document, grp).unwrap();
        assert!(
            (bb.min_x - 0.0).abs() < 1e-4
                && (bb.min_y - 0.0).abs() < 1e-4
                && (bb.width() - 100.0).abs() < 1e-4
                && (bb.height() - 10.0).abs() < 1e-4,
            "rotated Group inside rotated PowerClip must have [0..100, 0..10] local bounds, got {bb:?}"
        );
        tab.selection.select_single(&tab.document, grp);
        bar
    };

    app.ensure_envelope_on_object(app.active_tab().unwrap().selection.objects[0]);
    let grp_id = app.active_tab().unwrap().selection.objects[0];
    let has_valid_cage = app
        .active_tab()
        .unwrap()
        .document
        .object(grp_id)
        .unwrap()
        .effects
        .iter()
        .any(|e| match e {
            Effect::Envelope { cage, .. } => {
                let b = Rect2D::from_points(cage[0], cage[4]);
                (b.width() - 100.0).abs() < 1e-3 && (b.height() - 10.0).abs() < 1e-3
            }
            _ => false,
        });
    assert!(
        has_valid_cage,
        "Envelope cage must match [0..100, 0..10] without AABB-of-AABB bloat"
    );

    // Rotated SymbolInstance + rotated Connector
    let tab = app.active_tab_mut().unwrap();
    let sym = SymbolDef::new(
        "BarSym",
        vec![SceneObject::new(
            ObjectId(1),
            "Bar",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 100.0, 10.0,
            ))),
        )],
    );
    let sym_id = sym.id;
    tab.document.upsert_symbol(sym);
    let inst = tab
        .document
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(rot45),
            Some(lid),
        )
        .unwrap();
    let inst_bb = StudioApp::object_local_bounds(&tab.document, inst).unwrap();
    assert!((inst_bb.width() - 100.0).abs() < 1e-4 && (inst_bb.height() - 10.0).abs() < 1e-4);

    let mut conn = ConnectorObject::between_objects(
        bar_id,
        AnchorPoint::Center,
        bar_id,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn.from_obj = None;
    conn.to_obj = None;
    conn.from_fallback = Point2D::new(0.0, 5.0);
    conn.to_fallback = Point2D::new(100.0, 5.0);
    let conn_id = tab
        .document
        .add_object(
            SceneObject::new(ObjectId(0), "Conn", NodePayload::Connector(conn))
                .with_transform(rot45),
            Some(lid),
        )
        .unwrap();
    let conn_bb = StudioApp::object_local_bounds(&tab.document, conn_id).unwrap();
    assert!((conn_bb.width() - 100.0).abs() < 1e-4 && conn_bb.height() < 1e-4);
}

#[test]
fn w1b_r16_apply_knife_cut_group_powerclip_and_local_fill_effects() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    // 1. Cut inside a selected Group
    let (grp_id, child_id) = {
        let tab = app.active_tab_mut().unwrap();
        let rect = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Rect",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 40.0, 20.0,
                    ))),
                )
                .with_transform(Affine2D::translate(50.0, 50.0)),
                Some(lid),
            )
            .unwrap();
        let grp = tab.document.group_objects(&[rect], "CutGrp").unwrap();
        tab.selection.select_single(&tab.document, grp);
        (grp, rect)
    };
    let count = app.apply_knife_cut(Point2D::new(70.0, 40.0), Point2D::new(70.0, 80.0));
    assert_eq!(
        count, 2,
        "knife cut with Group selected must slice child into 2 pieces"
    );
    assert!(!app.active_tab().unwrap().document.has_object(child_id));
    if let NodePayload::Group(ref children) = app
        .active_tab()
        .unwrap()
        .document
        .object(grp_id)
        .unwrap()
        .payload
    {
        assert_eq!(children.len(), 2);
        for &cid in children {
            assert_eq!(
                app.active_tab()
                    .unwrap()
                    .document
                    .object(cid)
                    .unwrap()
                    .transform,
                Affine2D::translate(50.0, 50.0),
                "sliced child must preserve obj.transform"
            );
        }
    } else {
        panic!("expected Group");
    }

    // 2. PowerClip container clip bounds check + local LinearGradient & Distort alignment
    let mut app2 = StudioApp::new_headless();
    let lid2 = app2.active_tab().unwrap().document.pages()[0].layers[0].id;
    let pc_id = {
        let tab = app2.active_tab_mut().unwrap();
        let pc = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "PC",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 40.0, 40.0,
                    ))),
                ),
                Some(lid2),
            )
            .unwrap();
        let mut inner = SceneObject::new(
            ObjectId(0),
            "GradBar",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 60.0, 20.0,
            ))),
        )
        .with_transform(Affine2D::translate(10.0, 10.0));
        inner.fill = Fill::LinearGradient {
            start: Point2D::new(0.0, 0.0),
            end: Point2D::new(60.0, 0.0),
            stops: vec![
                GradientStop::new(0.0, Color::rgb(1.0, 0.0, 0.0)),
                GradientStop::new(1.0, Color::rgb(0.0, 0.0, 1.0)),
            ],
        };
        inner.effects.push(Effect::Distort {
            kind: coree_core::DistortKind::PushPull,
            amplitude: 2.0,
            frequency: 2.0,
            center: Point2D::new(30.0, 10.0),
        });
        let inner_id = tab.document.add_object(inner, Some(lid2)).unwrap();
        tab.document.create_powerclip(pc, &[inner_id]).unwrap();
        tab.selection.select_single(&tab.document, pc);
        pc
    };
    // Cut at x = 55 is outside PowerClip [0..40, 0..40] even though inner extends to x = 70!
    assert_eq!(
        app2.apply_knife_cut(Point2D::new(55.0, 0.0), Point2D::new(55.0, 40.0)),
        0,
        "knife cut outside PowerClip clip bounds must return 0"
    );
    // Cut at x = 25 is inside PowerClip -> slices inner into 2 pieces preserving local transform & fill
    assert_eq!(
        app2.apply_knife_cut(Point2D::new(25.0, -5.0), Point2D::new(25.0, 45.0)),
        2,
        "knife cut inside PowerClip clip bounds must slice content"
    );
    if let NodePayload::PowerClip { ref contents, .. } = app2
        .active_tab()
        .unwrap()
        .document
        .object(pc_id)
        .unwrap()
        .payload
    {
        assert_eq!(contents.len(), 2);
        let slice_obj = app2
            .active_tab()
            .unwrap()
            .document
            .object(contents[0])
            .unwrap();
        assert_eq!(slice_obj.transform, Affine2D::translate(10.0, 10.0));
        assert!(matches!(slice_obj.fill, Fill::LinearGradient { .. }));
        assert_eq!(slice_obj.effects.len(), 1);
    } else {
        panic!("expected PowerClip");
    }
}

#[test]
fn w1b_r16_copy_paste_lone_child_in_rotated_group_and_powerclip() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let grp_xf =
        Affine2D::translate(100.0, 80.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_4));

    let (leaf_id, conn_id, top_id, exp_leaf_bb, exp_conn_bb, exp_top_bb) = {
        let tab = app.active_tab_mut().unwrap();
        let leaf = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Leaf",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 20.0, 10.0,
                    ))),
                ),
                Some(lid),
            )
            .unwrap();
        let mut conn = ConnectorObject::between_objects(
            leaf,
            AnchorPoint::Center,
            leaf,
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        );
        conn.from_obj = None;
        conn.to_obj = None;
        conn.from_fallback = Point2D::new(5.0, 5.0);
        conn.to_fallback = Point2D::new(35.0, 15.0);
        let conn_id = tab
            .document
            .add_object(
                SceneObject::new(ObjectId(0), "ChildConn", NodePayload::Connector(conn)),
                Some(lid),
            )
            .unwrap();
        let top_id = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "ChildTOP",
                    NodePayload::TextOnPath(TextOnPath {
                        text: ArtisticText::from_plain("Child", TextStyle::default()),
                        path_id: leaf,
                        offset_mm: 0.0,
                        distance_mm: 0.0,
                        flip_vertical: false,
                        flip_horizontal: false,
                    }),
                ),
                Some(lid),
            )
            .unwrap();
        let grp = tab
            .document
            .group_objects(&[leaf, conn_id, top_id], "RotGrp")
            .unwrap();
        tab.document.transform_object(grp, grp_xf).unwrap();
        (
            leaf,
            conn_id,
            top_id,
            tab.document.world_object_bounds(leaf).unwrap(),
            tab.document.world_object_bounds(conn_id).unwrap(),
            tab.document.world_object_bounds(top_id).unwrap(),
        )
    };

    // Copy lone leaf child -> paste -> world bounds shifted by duplicate_offset_mm (5, 5)
    app.active_tab_mut().unwrap().selection.objects = vec![leaf_id];
    assert_eq!(app.copy_selection(), 1);
    let pasted_leaf = app.paste_clipboard();
    assert_eq!(pasted_leaf.len(), 1);
    let p_leaf_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(pasted_leaf[0])
        .unwrap();
    assert!(
        (p_leaf_bb.min_x - (exp_leaf_bb.min_x + 5.0)).abs() < 1e-4
            && (p_leaf_bb.min_y - (exp_leaf_bb.min_y + 5.0)).abs() < 1e-4
            && (p_leaf_bb.max_x - (exp_leaf_bb.max_x + 5.0)).abs() < 1e-4
            && (p_leaf_bb.max_y - (exp_leaf_bb.max_y + 5.0)).abs() < 1e-4,
        "pasted lone leaf child must shift from {exp_leaf_bb:?} by (+5,+5), got {p_leaf_bb:?}"
    );

    // Copy lone Connector child -> paste -> world bounds shifted by (+5, +5)
    app.active_tab_mut().unwrap().selection.objects = vec![conn_id];
    assert_eq!(app.copy_selection(), 1);
    let pasted_conn = app.paste_clipboard();
    assert_eq!(pasted_conn.len(), 1);
    let p_conn_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(pasted_conn[0])
        .unwrap();
    assert!(
        (p_conn_bb.min_x - (exp_conn_bb.min_x + 5.0)).abs() < 1e-4
            && (p_conn_bb.min_y - (exp_conn_bb.min_y + 5.0)).abs() < 1e-4,
        "pasted lone Connector child must shift from {exp_conn_bb:?} by (+5,+5), got {p_conn_bb:?}"
    );

    // Copy lone TextOnPath child -> paste (demoted to ArtisticText when host uncopied) -> lands near shifted anchor
    app.active_tab_mut().unwrap().selection.objects = vec![top_id];
    assert_eq!(app.copy_selection(), 1);
    let pasted_top = app.paste_clipboard();
    assert_eq!(pasted_top.len(), 1);
    let p_top_obj = app
        .active_tab()
        .unwrap()
        .document
        .object(pasted_top[0])
        .unwrap()
        .clone();
    let p_top_pos = p_top_obj.transform.transform_point(Point2D::ZERO);
    let exp_anchor = grp_xf.transform_point(Point2D::ZERO);
    assert!(
        (p_top_pos.x - (exp_anchor.x + 5.0)).abs() < 1e-4
            && (p_top_pos.y - (exp_anchor.y + 5.0)).abs() < 1e-4,
        "pasted lone TextOnPath child must land at shifted world anchor {:?}, got {p_top_pos:?} (orig_bb={exp_top_bb:?})",
        Point2D::new(exp_anchor.x + 5.0, exp_anchor.y + 5.0)
    );
}

#[test]
fn w1b_r16_trace_selected_raster_filters_and_crop_rect() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let rot_xf =
        Affine2D::translate(30.0, 40.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_4));

    // 40x40 px -> 40x40 mm raster: top-left quadrant red, bottom-right quadrant blue, white background
    let mut bytes = vec![255u8; 40 * 40 * 4];
    for y in 0..40u32 {
        for x in 0..40u32 {
            let idx = ((y * 40 + x) * 4) as usize;
            if (2..18).contains(&x) && (2..18).contains(&y) {
                bytes[idx] = 240;
                bytes[idx + 1] = 10;
                bytes[idx + 2] = 10;
                bytes[idx + 3] = 255;
            } else if (22..38).contains(&x) && (22..38).contains(&y) {
                bytes[idx] = 10;
                bytes[idx + 1] = 10;
                bytes[idx + 2] = 240;
                bytes[idx + 3] = 255;
            }
        }
    }
    let raster_id = {
        let tab = app.active_tab_mut().unwrap();
        let asset = coree_core::RasterAsset::new(40, 40, bytes, 96.0, ColorMode::Rgb).unwrap();
        let aid = tab.document.register_asset(asset);
        let rid = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "QuadRaster",
                    NodePayload::Raster(coree_core::RasterLayerData {
                        asset_id: aid,
                        width_mm: 40.0,
                        height_mm: 40.0,
                        crop_rect: Some(Rect2D::from_xywh(20.0, 20.0, 20.0, 20.0)),
                        filters: vec![coree_core::RasterFilter::Invert],
                    }),
                ),
                Some(lid),
            )
            .unwrap();
        let grp = tab.document.group_objects(&[rid], "RotRasterGrp").unwrap();
        tab.document.transform_object(grp, rot_xf).unwrap();
        tab.selection.select_single(&tab.document, rid);
        rid
    };

    let count = app
        .trace_selected_raster()
        .expect("trace cropped filtered raster");
    assert!(count >= 1);

    let tab = app.active_tab().unwrap();
    let inv_xf = rot_xf.inverse().unwrap();
    for &sel_id in &tab.selection.objects {
        let obj = tab.document.object(sel_id).unwrap();
        if let NodePayload::Path(ref p) = obj.payload {
            let local_bb = p
                .transformed(&obj.transform)
                .transformed(&inv_xf)
                .bounding_box()
                .unwrap();
            assert!(
                local_bb.min_x >= 19.5 && local_bb.min_y >= 19.5,
                "cropped raster trace must stay in [20..40, 20..40], got {local_bb:?}"
            );
            if let Fill::Solid(ref c) = obj.fill {
                let [r, g, b, _] = c.to_rgba8();
                // Inverted blue [10,10,240] -> yellow [245,245,15]!
                assert!(
                    r > 180 && g > 180 && b < 80,
                    "filtered (Invert) blue quadrant must trace as yellow, got ({r},{g},{b})"
                );
            }
        }
    }
    let _ = raster_id;
}

#[test]
fn w1b_r16_moving_selection_nested_rotated_group_and_parent_child_dedup() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let rot90 =
        Affine2D::translate(100.0, 50.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_2));

    let (grp_id, child_id) = {
        let tab = app.active_tab_mut().unwrap();
        let child = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Child",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 20.0, 10.0,
                    ))),
                ),
                Some(lid),
            )
            .unwrap();
        let grp = tab.document.group_objects(&[child], "Rot90Grp").unwrap();
        tab.document.transform_object(grp, rot90).unwrap();
        (grp, child)
    };

    // Case A: lone child inside 90-deg rotated Group -> stamp duplicate + cancel_active_drag + commit + undo
    let orig_child_world = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    let orig_child_local = app
        .active_tab()
        .unwrap()
        .document
        .object(child_id)
        .unwrap()
        .transform;

    // Simulate live drag (+10, 0) in world space via ToolCore pointer down/move (zero mid-drag Document mutation!)
    let prev_filled = app.treat_all_objects_as_filled;
    app.treat_all_objects_as_filled = true;
    let drag_start = orig_child_world.center();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.objects = vec![child_id];
        tab.viewport.zoom = 4.0;
        tab.snap_options.grid = false;
        tab.snap_options.guidelines = false;
        tab.snap_options.objects = false;
        tab.snap_options.page_edges = false;
        tab.snap_options.nodes = false;
        tab.snap_options.dynamic_guides = false;
    }
    app.run_tool_pointer_down(&coree_app::state::PointerEvent::primary(
        drag_start,
        [0.0, 0.0],
    ));
    app.run_tool_pointer_move(&coree_app::state::PointerEvent::primary(
        Point2D::new(drag_start.x + 10.0, drag_start.y),
        [10.0, 0.0],
    ));
    let doc_mid_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert_eq!(
        doc_mid_bb, orig_child_world,
        "ToolCore MUST NOT mutate Document mid-drag"
    );
    let live_xf = app
        .active_tab()
        .unwrap()
        .tool_core
        .active_live_world_transform()
        .unwrap();
    let live_min = live_xf.transform_point(orig_child_world.min());
    assert!(
        (live_min.x - (orig_child_world.min_x + 10.0)).abs() < 1e-4
            && (live_min.y - orig_child_world.min_y).abs() < 1e-4,
        "live MovingSelection on rotated child must translate (+10, 0) in world space"
    );
    assert!(
        !app.active_tab()
            .unwrap()
            .transient_overlay
            .preview_paths
            .is_empty(),
        "mid-drag drag must populate TransientOverlay preview_paths"
    );

    let stamped = app.stamp_duplicate_during_active_drag().unwrap();
    assert_eq!(stamped.len(), 1);
    let stamped_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(stamped[0])
        .unwrap();
    assert!(
        (stamped_bb.min_x - (orig_child_world.min_x + 10.0)).abs() < 1e-4
            && (stamped_bb.min_y - orig_child_world.min_y).abs() < 1e-4,
        "Spacebar stamped duplicate during MovingSelection must land at (+10, 0) world: {stamped_bb:?}"
    );

    app.active_tab_mut().unwrap().cancel_active_drag();
    app.treat_all_objects_as_filled = prev_filled;
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(child_id)
            .unwrap()
            .transform,
        orig_child_local,
        "cancel_active_drag must restore exact pre-drag local transform on rotated child"
    );

    // Case B: [grp_id, child_id] both selected -> nudge_selection (+5, 0) must NOT double-move child!
    let orig_grp_world = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(grp_id)
        .unwrap();
    app.active_tab_mut().unwrap().selection.objects = vec![grp_id, child_id];
    app.active_tab_mut().unwrap().nudge_mm = 5.0;
    app.nudge_selection(1, 0, 1);
    let nudged_child_world = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!(
        (nudged_child_world.min_x - (orig_grp_world.min_x + 5.0)).abs() < 1e-4
            && (nudged_child_world.min_y - orig_grp_world.min_y).abs() < 1e-4,
        "nudge_selection with [grp, child] must deduplicate and move once (+5, 0): {nudged_child_world:?}"
    );
    app.undo().expect("undo nudge_selection");
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .world_object_bounds(child_id)
            .unwrap(),
        orig_child_world
    );
}

#[test]
fn w1b_r16_convert_outline_mixed_locked_layer() {
    let mut app = StudioApp::new_headless();
    let page_id = app.active_tab().unwrap().document.pages()[0].id;
    let unlocked_lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let locked_lid = app
        .active_tab_mut()
        .unwrap()
        .document
        .add_layer(
            page_id,
            "LockedLayer",
            false,
            coree_core::MasterLayerScope::None,
        )
        .unwrap();

    let (editable_id, locked_id) = {
        let tab = app.active_tab_mut().unwrap();
        let e = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "EditableStroke",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 20.0, 20.0,
                    ))),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::solid(Color::BLACK, 2.0)),
                Some(unlocked_lid),
            )
            .unwrap();
        let l = tab
            .document
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "LockedLayerStroke",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        30.0, 0.0, 20.0, 20.0,
                    ))),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::solid(Color::BLACK, 2.0)),
                Some(locked_lid),
            )
            .unwrap();
        tab.document.find_layer_mut(locked_lid).unwrap().locked = true;
        tab.selection.objects = vec![e, l];
        (e, l)
    };

    app.convert_outline_to_object_selection();
    let tab = app.active_tab().unwrap();
    assert!(
        tab.document.object(editable_id).unwrap().fill.is_visible(),
        "editable object outline must be converted even when a locked-layer object is also selected"
    );
    assert!(
        !tab.document.object(locked_id).unwrap().fill.is_visible(),
        "locked-layer object must remain untouched"
    );
}

#[test]
fn w1b_r17_convert_selection_to_bitmap_pixel_alignment_for_wide_stroke_and_top() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    // 1. Wide-stroked (10 mm) rectangle at [40..60, 40..60]
    let wide_rect = SceneObject::new(
        ObjectId(0),
        "WideRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 120, 240, 255)))
    .with_outline(OutlineStyle::solid(Color::rgba8(240, 0, 0, 255), 10.0))
    .with_transform(Affine2D::translate(40.0, 40.0));
    app.execute_command(CommandOp::create_object(wide_rect, Some(lid)))
        .unwrap();
    let wide_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];

    let mut opts = coree_render::RenderOptions::export();
    opts.page_bg = Color::TRANSPARENT;
    let vec_img = coree_render::render_page_to_rgba_with_options(
        &app.active_tab().unwrap().document,
        0,
        25.4,
        coree_render::ViewMode::Enhanced,
        &opts,
    )
    .unwrap();

    app.active_tab_mut().unwrap().selection.objects = vec![wide_id];
    app.bitmap_state.convert_dpi = 96.0;
    app.convert_selection_to_bitmap();
    let bmp_id = app.active_tab().unwrap().selection.objects[0];
    assert_ne!(bmp_id, wide_id);
    // Remove original vector object so we render ONLY the converted bitmap
    app.execute_command(CommandOp::DeleteObjects { ids: vec![wide_id] })
        .unwrap();
    let bmp_img = coree_render::render_page_to_rgba_with_options(
        &app.active_tab().unwrap().document,
        0,
        25.4,
        coree_render::ViewMode::Enhanced,
        &opts,
    )
    .unwrap();

    let ink_bbox = |img: &coree_render::RenderedImage| -> (u32, u32, u32, u32) {
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0;
        let mut max_y = 0;
        for y in 0..img.height {
            for x in 0..img.width {
                let i = ((y as usize) * (img.width as usize) + (x as usize)) * 4;
                if img.rgba[i + 3] > 32
                    && (img.rgba[i] < 245 || img.rgba[i + 1] < 245 || img.rgba[i + 2] < 245)
                {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(
            min_x <= max_x && min_y <= max_y,
            "expected non-white ink pixels"
        );
        (min_x, min_y, max_x, max_y)
    };
    let vb = ink_bbox(&vec_img);
    let bb = ink_bbox(&bmp_img);
    assert!(
        vb.0.abs_diff(bb.0) <= 2
            && vb.1.abs_diff(bb.1) <= 2
            && vb.2.abs_diff(bb.2) <= 2
            && vb.3.abs_diff(bb.3) <= 2,
        "wide-stroke vector ink bbox {vb:?} and converted bitmap ink bbox {bb:?} must align within <= 2px"
    );

    // 2. TextOnPath (24pt, distance_mm = 5.0)
    app.execute_command(CommandOp::DeleteObjects { ids: vec![bmp_id] })
        .unwrap();
    let host = SceneObject::new(
        ObjectId(0),
        "HostLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(20.0, 100.0),
            Point2D::new(140.0, 100.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    app.execute_command(CommandOp::create_object(host, Some(lid)))
        .unwrap();
    let host_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();
    let style24 = TextStyle {
        font_size_pt: 24.0,
        ..Default::default()
    };
    let top = SceneObject::new(
        ObjectId(0),
        "TallTOP",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("ALIGN_CHECK", style24),
            path_id: host_id,
            offset_mm: 0.0,
            distance_mm: 5.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 0, 200, 255)))
    .with_outline(OutlineStyle::none());
    app.execute_command(CommandOp::create_object(top, Some(lid)))
        .unwrap();
    let top_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();

    let vec_top_img = coree_render::render_page_to_rgba_with_options(
        &app.active_tab().unwrap().document,
        0,
        25.4,
        coree_render::ViewMode::Enhanced,
        &opts,
    )
    .unwrap();
    app.active_tab_mut().unwrap().selection.objects = vec![top_id];
    app.convert_selection_to_bitmap();
    let top_bmp_id = app.active_tab().unwrap().selection.objects[0];
    app.execute_command(CommandOp::DeleteObjects { ids: vec![top_id] })
        .unwrap();
    let bmp_top_img = coree_render::render_page_to_rgba_with_options(
        &app.active_tab().unwrap().document,
        0,
        25.4,
        coree_render::ViewMode::Enhanced,
        &opts,
    )
    .unwrap();
    let vtb = ink_bbox(&vec_top_img);
    let btb = ink_bbox(&bmp_top_img);
    assert!(
        vtb.0.abs_diff(btb.0) <= 2
            && vtb.1.abs_diff(btb.1) <= 2
            && vtb.2.abs_diff(btb.2) <= 2
            && vtb.3.abs_diff(btb.3) <= 2,
        "TextOnPath vector ink bbox {vtb:?} and converted bitmap ink bbox {btb:?} must align within <= 2px (bmp_id={top_bmp_id})"
    );
}

#[test]
fn w1b_r17_copy_paste_lone_text_on_path_same_doc_inside_rotated_group() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let host = SceneObject::new(
        ObjectId(0),
        "HostPath",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, 0.0),
            Point2D::new(40.0, 0.0),
        )),
    );
    app.execute_command(CommandOp::create_object(host, Some(lid)))
        .unwrap();
    let host_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    let top = SceneObject::new(
        ObjectId(0),
        "LoneTOP",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("CURVE_COPY", TextStyle::default()),
            path_id: host_id,
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    app.execute_command(CommandOp::create_object(top, Some(lid)))
        .unwrap();
    let top_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[1];

    app.execute_command(CommandOp::Group {
        ids: vec![host_id, top_id],
        name: "RotGrp".into(),
    })
    .unwrap();
    let grp_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![grp_id],
        affine: Affine2D::translate(40.0, 40.0).compose(&Affine2D::rotate_deg(45.0)),
    })
    .unwrap();

    // Select ONLY the lone TextOnPath inside the rotated Group (NOT its host_id)
    app.active_tab_mut().unwrap().duplicate_offset_mm = Vec2D::new(25.0, 35.0);
    app.active_tab_mut().unwrap().selection.objects = vec![top_id];
    assert_eq!(app.copy_selection(), 1);
    let pasted = app.paste_clipboard();
    assert_eq!(pasted.len(), 1);
    let pasted_id = pasted[0];

    let tab = app.active_tab().unwrap();
    let pasted_obj = tab.document.object(pasted_id).unwrap();
    assert!(
        matches!(pasted_obj.payload, NodePayload::ArtisticText(_)),
        "lone same-doc pasted TextOnPath must demote to ArtisticText, got {:?}",
        pasted_obj.payload
    );
    // Its world origin must be shifted by duplicate_offset_mm (25, 35) from host's world start (40, 40)
    let pasted_origin = pasted_obj.transform.transform_point(Point2D::ZERO);
    assert!(
        (pasted_origin.x - 65.0).abs() < 1e-4 && (pasted_origin.y - 75.0).abs() < 1e-4,
        "pasted lone TextOnPath must shift by (25, 35) to (65, 75), got {pasted_origin:?}"
    );
}

#[test]
fn w1b_r17_world_transforms_free_transform_repeat_powerclip_dedup_and_locked() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    // 1. Child inside 90°-rotated Group -> apply_free_transform + repeat_last_command
    let rect = SceneObject::new(
        ObjectId(0),
        "NestedRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 20.0, 10.0,
        ))),
    );
    app.execute_command(CommandOp::create_object(rect, Some(lid)))
        .unwrap();
    let child_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    app.execute_command(CommandOp::Group {
        ids: vec![child_id],
        name: "Grp90".into(),
    })
    .unwrap();
    let grp_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![grp_id],
        affine: Affine2D::translate(100.0, 50.0).compose(&Affine2D::rotate_deg(90.0)),
    })
    .unwrap();

    // Child world bounds are [90..100, 50..70], center = (95, 60)
    let before = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    let world_center = before.center();
    assert!((world_center.x - 95.0).abs() < 1e-4 && (world_center.y - 60.0).abs() < 1e-4);

    // apply_free_transform: 2x scale around world center must preserve world center
    app.active_tab_mut().unwrap().selection.objects = vec![child_id];
    let start_pt = Point2D::new(world_center.x + 5.0, world_center.y);
    let end_pt = Point2D::new(world_center.x + 10.0, world_center.y);
    assert!(app.apply_free_transform(world_center, start_pt, end_pt, false, false));
    let after_ft = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!(
        after_ft.center().distance_to(world_center) < 1e-4,
        "apply_free_transform must preserve world center {world_center:?}, got {:?}",
        after_ft.center()
    );
    assert!(
        (after_ft.width() - before.width() * 2.0).abs() < 1e-4,
        "apply_free_transform 2x scale expected width {}, got {}",
        before.width() * 2.0,
        after_ft.width()
    );

    // [grp_id, child_id] dedup on apply_free_transform (another 2x -> total 4x, NOT 8x)
    app.active_tab_mut().unwrap().selection.objects = vec![grp_id, child_id];
    assert!(app.apply_free_transform(world_center, start_pt, end_pt, false, false));
    let after_dedup = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!(
        (after_dedup.width() - before.width() * 4.0).abs() < 1e-4,
        "[grp, child] dedup must scale 2x once (total 4x), got {}",
        after_dedup.width()
    );

    // repeat_last_command: nudge a top-level object (+10, 0), then repeat on nested child, then repeat AGAIN on nested child
    let top_rect = SceneObject::new(
        ObjectId(0),
        "TopRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 10.0, 10.0,
        ))),
    );
    app.execute_command(CommandOp::create_object(top_rect, Some(lid)))
        .unwrap();
    let top_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[1];
    app.active_tab_mut().unwrap().nudge_mm = 10.0;
    app.active_tab_mut().unwrap().selection.objects = vec![top_id];
    app.nudge_selection(1, 0, 1);

    let c_before_rep = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    app.active_tab_mut().unwrap().selection.objects = vec![child_id];
    app.repeat_last_command();
    let c_after_rep1 = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!(
        (c_after_rep1.min_x - (c_before_rep.min_x + 10.0)).abs() < 1e-4
            && (c_after_rep1.min_y - c_before_rep.min_y).abs() < 1e-4,
        "repeat_last_command must move rotated Group child by (+10, 0) in world space: {c_before_rep:?} -> {c_after_rep1:?}"
    );
    // Second repeat on the same nested child must NOT double-conjugate
    app.repeat_last_command();
    let c_after_rep2 = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!(
        (c_after_rep2.min_x - (c_after_rep1.min_x + 10.0)).abs() < 1e-4
            && (c_after_rep2.min_y - c_after_rep1.min_y).abs() < 1e-4,
        "second repeat_last_command on nested child must also move (+10, 0) in world space: {c_after_rep1:?} -> {c_after_rep2:?}"
    );

    // 2. PowerClip-edited child + [pc_id, pc_child] dedup + mixed locked selection via build_world_transform_command
    let pc_container = SceneObject::new(
        ObjectId(0),
        "PcCont",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 80.0, 80.0,
        ))),
    );
    app.execute_command(CommandOp::create_object(pc_container, Some(lid)))
        .unwrap();
    let pc_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();
    let pc_child = SceneObject::new(
        ObjectId(0),
        "PcChild",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 20.0, 10.0,
        ))),
    );
    app.execute_command(CommandOp::create_object(pc_child, Some(lid)))
        .unwrap();
    let pc_child_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();
    app.execute_command(CommandOp::CreatePowerClip {
        container_id: pc_id,
        content_ids: vec![pc_child_id],
    })
    .unwrap();
    app.execute_command(CommandOp::TransformObjects {
        ids: vec![pc_id],
        affine: Affine2D::translate(120.0, 100.0).compose(&Affine2D::rotate_deg(90.0)),
    })
    .unwrap();
    let locked_obj = SceneObject::new(
        ObjectId(0),
        "LockedOne",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    app.execute_command(CommandOp::create_object(locked_obj, Some(lid)))
        .unwrap();
    let locked_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();
    app.execute_command(CommandOp::SetObjectLocked {
        ids: vec![locked_id],
        locked: true,
    })
    .unwrap();

    // Enter PowerClip edit and test apply_free_transform + nudge_selection with [pc_child_id, locked_id] and [pc_id, pc_child_id]
    assert!(app.enter_powerclip_edit(pc_id));
    let pc_child_before = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(pc_child_id)
        .unwrap();
    let locked_before = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(locked_id)
        .unwrap();
    app.active_tab_mut().unwrap().selection.objects = vec![pc_child_id, locked_id];
    let pc_center = pc_child_before.center();
    assert!(app.apply_free_transform(
        pc_center,
        Point2D::new(pc_center.x + 5.0, pc_center.y),
        Point2D::new(pc_center.x, pc_center.y + 5.0),
        false,
        false,
    ));
    let pc_child_after = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(pc_child_id)
        .unwrap();
    let locked_after = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(locked_id)
        .unwrap();
    assert!(
        pc_child_after.center().distance_to(pc_child_before.center()) < 1e-4
            && (pc_child_after.width() - pc_child_before.height()).abs() < 1e-4,
        "PowerClip-edited child rotated 90° around world center: {pc_child_before:?} -> {pc_child_after:?}"
    );
    assert_eq!(
        locked_before, locked_after,
        "locked object must remain untouched"
    );

    // Dedup [pc_id, pc_child_id]: nudge (+15, 0) once, not twice
    let pc_before = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(pc_child_id)
        .unwrap();
    app.active_tab_mut().unwrap().nudge_mm = 15.0;
    app.active_tab_mut().unwrap().selection.objects = vec![pc_id, pc_child_id];
    app.nudge_selection(1, 0, 1);
    let pc_after = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(pc_child_id)
        .unwrap();
    assert!(
        (pc_after.min_x - (pc_before.min_x + 15.0)).abs() < 1e-4,
        "[pc_id, pc_child_id] dedup must translate +15 once, not +30: {pc_before:?} -> {pc_after:?}"
    );
}

#[test]
fn w1b_r17_trace_selected_raster_fallback_skips_locked_hidden_and_other_page() {
    let mut app = StudioApp::new_headless();
    let p1_id = app.active_tab().unwrap().document.pages()[0].id;
    let p1_editable_lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    let mut px = vec![255u8; 16 * 16 * 4];
    for y in 4..12 {
        for x in 4..12 {
            let idx = (y * 16 + x) * 4;
            px[idx] = 220;
            px[idx + 1] = 20;
            px[idx + 2] = 20;
            px[idx + 3] = 255;
        }
    }
    let asset = coree_core::RasterAsset::new(16, 16, px, 96.0, ColorMode::Rgb).unwrap();
    let aid = asset.id;
    app.execute_command(CommandOp::RegisterAsset { asset })
        .unwrap();

    let make_raster_obj = |name: &str| {
        SceneObject::new(
            ObjectId(0),
            name,
            NodePayload::Raster(coree_core::RasterLayerData {
                asset_id: aid,
                width_mm: 20.0,
                height_mm: 20.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        )
        .with_transform(Affine2D::translate(10.0, 10.0))
    };

    // 1. Locked layer raster on Page 1
    let locked_lid = app
        .active_tab_mut()
        .unwrap()
        .document
        .add_layer(p1_id, "Locked", false, coree_core::MasterLayerScope::None)
        .unwrap();
    app.execute_command(CommandOp::create_object(
        make_raster_obj("LockedRaster"),
        Some(locked_lid),
    ))
    .unwrap();
    app.active_tab_mut()
        .unwrap()
        .document
        .find_layer_mut(locked_lid)
        .unwrap()
        .locked = true;

    // 2. Hidden layer raster on Page 1
    let hidden_lid = app
        .active_tab_mut()
        .unwrap()
        .document
        .add_layer(p1_id, "Hidden", false, coree_core::MasterLayerScope::None)
        .unwrap();
    app.execute_command(CommandOp::create_object(
        make_raster_obj("HiddenRaster"),
        Some(hidden_lid),
    ))
    .unwrap();
    app.active_tab_mut()
        .unwrap()
        .document
        .find_layer_mut(hidden_lid)
        .unwrap()
        .visible = false;

    // 3. Editable raster on Page 2 while active page is Page 1
    let p2_id = app
        .active_tab_mut()
        .unwrap()
        .document
        .add_page("Page 2", 210.0, 297.0, 3.0);
    let p2_lid = app
        .active_tab()
        .unwrap()
        .document
        .pages()
        .iter()
        .find(|p| p.id == p2_id)
        .unwrap()
        .layers[0]
        .id;
    app.execute_command(CommandOp::create_object(
        make_raster_obj("Page2Raster"),
        Some(p2_lid),
    ))
    .unwrap();
    app.active_tab_mut()
        .unwrap()
        .document
        .set_active_page_index(0)
        .unwrap();
    app.active_tab_mut().unwrap().selection.objects.clear();

    let err = app.trace_selected_raster().unwrap_err();
    assert!(
        err.contains("select a raster bitmap object"),
        "fallback trace_selected_raster must skip rasters on locked/hidden layers and inactive pages, got: {err}"
    );

    // 4. Add an editable raster on active Page 1 -> non-selected fallback succeeds
    app.execute_command(CommandOp::create_object(
        make_raster_obj("ActivePageEditable"),
        Some(p1_editable_lid),
    ))
    .unwrap();
    app.active_tab_mut().unwrap().selection.objects.clear();
    let traced = app.trace_selected_raster().unwrap();
    assert!(
        traced >= 1,
        "fallback trace_selected_raster must trace editable raster on active page"
    );
}

#[test]
fn w1b_r19_convert_selection_to_bitmap_lens_and_group_child_symmetry() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;

    // 1. Non-frozen Lens::Invert sitting over opaque Red rect
    let red_rect = SceneObject::new(
        ObjectId(0),
        "RedBg",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 40.0,
            height: 40.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(255, 0, 0, 255)))
    .with_transform(Affine2D::translate(20.0, 20.0));
    app.execute_command(CommandOp::create_object(red_rect, Some(lid)))
        .unwrap();

    let mut lens_obj = SceneObject::new(
        ObjectId(0),
        "InvertLens",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::None)
    .with_transform(Affine2D::translate(30.0, 30.0));
    lens_obj.lens = Some(coree_core::LensConfig {
        kind: LensKind::Invert,
        frozen: false,
        viewpoint: None,
        remove_face: false,
    });
    app.execute_command(CommandOp::create_object(lens_obj, Some(lid)))
        .unwrap();
    let lens_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();

    app.active_tab_mut().unwrap().selection.objects = vec![lens_id];
    app.bitmap_state.convert_dpi = 96.0;
    app.convert_selection_to_bitmap();
    let bmp_id = app.active_tab().unwrap().selection.objects[0];
    assert_ne!(bmp_id, lens_id);
    let tab = app.active_tab().unwrap();
    let bmp_obj = tab.document.object(bmp_id).unwrap();
    let aid = match &bmp_obj.payload {
        NodePayload::Raster(r) => r.asset_id,
        other => panic!("expected Raster, got {other:?}"),
    };
    let asset = tab.document.assets().get(&aid).unwrap();
    let center_idx =
        (((asset.height / 2) as usize) * (asset.width as usize) + ((asset.width / 2) as usize)) * 4;
    let center_px = &asset.rgba_bytes[center_idx..center_idx + 4];
    assert!(
        center_px[3] > 240 && center_px[0] < 15 && center_px[1] > 240 && center_px[2] > 240,
        "convert_selection_to_bitmap on non-frozen Invert lens over Red must produce opaque Cyan, got {center_px:?}"
    );

    // 2. Group containing child with Effect::Symmetry -> convert_selection_to_bitmap covers full clone bounds
    let mut child = SceneObject::new(
        ObjectId(0),
        "SymChild",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 100, 220, 255)))
    .with_transform(Affine2D::translate(10.0, 10.0));
    child.effects.push(Effect::Symmetry {
        axes: 2,
        mirror: false,
        origin: Point2D::new(80.0, 80.0),
    });
    app.execute_command(CommandOp::create_object(child, Some(lid)))
        .unwrap();
    let child_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();
    app.execute_command(CommandOp::Group {
        ids: vec![child_id],
        name: "SymGrp".into(),
    })
    .unwrap();
    let grp_id = *app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();
    app.active_tab_mut().unwrap().selection.objects = vec![grp_id];
    app.convert_selection_to_bitmap();
    let sym_bmp_id = app.active_tab().unwrap().selection.objects[0];
    let sym_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(sym_bmp_id)
        .unwrap();
    assert!(
        sym_bb.max_x >= 150.0 && sym_bb.max_y >= 150.0,
        "convert_selection_to_bitmap on Group with child Symmetry must cover full clone bounds: {sym_bb:?}"
    );
}

#[test]
fn studio_clipboard_cross_instance_svg_pdf_emf_png_roundtrip_and_undo_redo() {
    use coree_app::clipboard::{ExternalClipboardData, MemoryClipboardBackend};
    use std::sync::Arc;

    let shared_mem = MemoryClipboardBackend::new();
    let slot = shared_mem.shared_slot();

    let mut app_a = StudioApp::new_headless();
    app_a.set_clipboard_backend(Box::new(MemoryClipboardBackend::with_shared_slot(
        Arc::clone(&slot),
    )));

    let mut app_b = StudioApp::new_headless();
    app_b.set_clipboard_backend(Box::new(MemoryClipboardBackend::with_shared_slot(
        Arc::clone(&slot),
    )));

    let lid_a = app_a.active_tab().unwrap().document.pages()[0].layers[0].id;
    let rect_obj = SceneObject::new(
        ObjectId(0),
        "VectorRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            12.0, 18.0, 44.0, 28.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(22, 140, 210, 255)))
    .with_outline(OutlineStyle::solid(Color::rgba8(15, 25, 35, 255), 0.5));
    app_a
        .execute_command(CommandOp::create_object(rect_obj, Some(lid_a)))
        .unwrap();
    let oid_a = *app_a.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .last()
        .unwrap();
    app_a.active_tab_mut().unwrap().selection.objects = vec![oid_a];

    // Copy in App A publishes SVG + PDF + EMF + PNG to shared clipboard backend
    assert_eq!(app_a.copy_selection(), 1);
    assert!(app_b.has_clipboard_content());

    // Paste in App B (fresh process/instance with empty internal clipboard) -> decodes SVG vector
    let pasted_svg = app_b.paste_clipboard();
    assert!(
        !pasted_svg.is_empty(),
        "App B must paste SVG from shared clipboard"
    );
    assert_eq!(
        app_b.active_tab().unwrap().document.objects().len(),
        pasted_svg.len()
    );
    assert!(app_b.undo().is_ok());
    assert_eq!(app_b.active_tab().unwrap().document.objects().len(), 0);
    assert!(app_b.redo().is_ok());
    assert_eq!(
        app_b.active_tab().unwrap().document.objects().len(),
        pasted_svg.len()
    );

    // Also verify PDF-only, EMF-only, and PNG-only external clipboard offers in App B
    let original = slot.lock().unwrap().clone().unwrap();
    for (label, data) in [
        (
            "pdf",
            ExternalClipboardData {
                pdf: original.pdf.clone(),
                content_hash: 101,
                ..Default::default()
            },
        ),
        (
            "emf",
            ExternalClipboardData {
                emf: original.emf.clone(),
                content_hash: 102,
                ..Default::default()
            },
        ),
        (
            "png",
            ExternalClipboardData {
                png: original.png.clone(),
                content_hash: 103,
                ..Default::default()
            },
        ),
    ] {
        *slot.lock().unwrap() = Some(data);
        let ids = app_b.paste_clipboard();
        assert!(
            !ids.is_empty(),
            "App B must paste external {label} clipboard payload"
        );
    }
}

#[test]
fn edit_symbol_isolated_session_updates_all_instances_and_supports_undo_redo() {
    let mut app = StudioApp::new_headless();
    let lid = app.active_tab().unwrap().document.pages()[0].layers[0].id;
    let rect = SceneObject::new(
        ObjectId(0),
        "SymPart",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    let sym = coree_core::SymbolDef::new("Badge", vec![rect]);
    let sym_id = sym.id;
    app.execute_command(CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();
    app.execute_command(CommandOp::InstantiateSymbol {
        symbol_id: sym_id,
        layer_id: Some(lid),
        transform: Affine2D::translate(10.0, 10.0),
        name: Some("Inst1".to_owned()),
    })
    .unwrap();
    app.execute_command(CommandOp::InstantiateSymbol {
        symbol_id: sym_id,
        layer_id: Some(lid),
        transform: Affine2D::translate(80.0, 10.0),
        name: Some("Inst2".to_owned()),
    })
    .unwrap();
    let inst_ids: Vec<ObjectId> = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .values()
        .filter(|o| matches!(o.payload, NodePayload::SymbolInstance(_)))
        .map(|o| o.id)
        .collect();
    assert_eq!(inst_ids.len(), 2);

    assert!(app.begin_edit_symbol(sym_id));
    assert!(app.active_tab().unwrap().editing_symbol.is_some());
    // Resize the isolated symbol part to 45x25 mm
    let iso_id = app.active_tab().unwrap().selection.objects[0];
    app.execute_command(CommandOp::EditShape {
        id: iso_id,
        shape: ParametricShape::Rectangle {
            width: 45.0,
            height: 25.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        },
    })
    .unwrap();
    assert!(app.finish_edit_symbol(true));
    assert!(app.active_tab().unwrap().editing_symbol.is_none());

    for &iid in &inst_ids {
        let bb = app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(iid)
            .unwrap();
        assert!((bb.width() - 45.0).abs() < 1e-4 && (bb.height() - 25.0).abs() < 1e-4);
    }

    // Undo restores original 20x20 symbol size across both instances
    app.undo().unwrap();
    for &iid in &inst_ids {
        let bb = app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(iid)
            .unwrap();
        assert!((bb.width() - 20.0).abs() < 1e-4);
    }
    app.redo().unwrap();
    for &iid in &inst_ids {
        let bb = app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(iid)
            .unwrap();
        assert!((bb.width() - 45.0).abs() < 1e-4);
    }

    // Test FitPowerClip & ADD-01 build_selected_effect indices 10..=12
    app.effects_state.selected_effect_kind = 10;
    assert!(matches!(
        app.build_selected_effect(),
        Effect::InnerShadow { .. }
    ));
    app.effects_state.selected_effect_kind = 11;
    assert!(matches!(app.build_selected_effect(), Effect::Bevel { .. }));
    app.effects_state.selected_effect_kind = 12;
    assert!(matches!(
        app.build_selected_effect(),
        Effect::Extrude3D { .. }
    ));
}

#[test]
fn w2b_r2_edit_symbol_preserves_group_and_powerclip_hierarchy() {
    use coree_render::{render_page_to_rgba, ViewMode};

    let mut app = StudioApp::new_headless();
    let lid = Some(app.active_tab().unwrap().document.pages()[0].layers[0].id);

    // 1. Create a Group on canvas and build a SymbolDef via collect_subtree_objects (matching + Create Symbol from Selection)
    let r1 = SceneObject::new(
        ObjectId(0),
        "SymR1",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 15.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(220, 30, 30, 255)));
    let mut r2 = SceneObject::new(
        ObjectId(0),
        "SymR2",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 15.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 64, 255, 255)));
    r2.transform = Affine2D::translate(25.0, 0.0);
    app.execute_command(CommandOp::create_object(r1, lid))
        .unwrap();
    app.execute_command(CommandOp::create_object(r2, lid))
        .unwrap();
    let layer_objs = app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .clone();
    assert_eq!(layer_objs.len(), 2);

    app.execute_command(CommandOp::Group {
        ids: layer_objs,
        name: "SrcGrp".to_owned(),
    })
    .unwrap();
    let grp_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    let collected = app
        .active_tab()
        .unwrap()
        .document
        .collect_subtree_objects(&[grp_id]);
    assert_eq!(
        collected.len(),
        3,
        "Group symbol creation must collect Group + both children"
    );

    let sym = SymbolDef::new("GroupSym", collected);
    let sym_id = sym.id;
    app.execute_command(CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();
    app.execute_command(CommandOp::InstantiateSymbol {
        symbol_id: sym_id,
        layer_id: lid,
        transform: Affine2D::translate(30.0, 30.0),
        name: Some("GroupSymInst".to_owned()),
    })
    .unwrap();
    let inst_id = app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .values()
        .find(|o| matches!(o.payload, NodePayload::SymbolInstance(_)))
        .unwrap()
        .id;

    // 2. Open existing SymbolDef containing a Group via begin_edit_symbol -> verify zero dangling IDs and top-level only on layer
    assert!(app.begin_edit_symbol(sym_id));
    {
        let tab = app.active_tab().unwrap();
        let sym_layer_objs = &tab.document.pages()[0].layers[0].objects;
        assert_eq!(
            sym_layer_objs.len(),
            1,
            "Only top-level Group must be attached to sym_doc layer"
        );
        let iso_grp = tab.document.object(sym_layer_objs[0]).unwrap();
        let NodePayload::Group(children) = &iso_grp.payload else {
            panic!("expected Group in sym_doc");
        };
        assert_eq!(children.len(), 2);
        for &cid in children {
            assert!(
                tab.document.has_object(cid),
                "Group child {cid:?} must exist in sym_doc (zero dangling IDs)"
            );
        }
    }

    // 3. Ungroup and re-group inside SymbolEditSession + add a third rect grouped together -> finish_edit_symbol(true)
    let iso_grp_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    app.execute_command(CommandOp::Ungroup {
        group_id: iso_grp_id,
    })
    .unwrap();
    let iso_rids = app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .clone();
    assert_eq!(iso_rids.len(), 2);
    let mut r3 = SceneObject::new(
        ObjectId(0),
        "SymR3",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 10.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(30, 180, 30, 255)));
    r3.transform = Affine2D::translate(50.0, 0.0);
    let iso_lid = Some(app.active_tab().unwrap().document.pages()[0].layers[0].id);
    app.execute_command(CommandOp::create_object(r3, iso_lid))
        .unwrap();
    let all_iso = app.active_tab().unwrap().document.pages()[0].layers[0]
        .objects
        .clone();
    app.execute_command(CommandOp::Group {
        ids: all_iso,
        name: "EditedGrp".to_owned(),
    })
    .unwrap();
    assert!(app.finish_edit_symbol(true));

    // 4. Verify saved SymbolDef has Group + 3 children, SymbolInstance has valid bounds, non-blank render, and Undo/Redo works
    let saved_sym = app
        .active_tab()
        .unwrap()
        .document
        .symbol(sym_id)
        .expect("GroupSym must exist");
    assert_eq!(saved_sym.objects.len(), 4);
    let post_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(inst_id)
        .expect("SymbolInstance bounds must be Some after grouping inside SymbolEditSession");
    assert!(
        (post_bb.width() - 60.0).abs() < 1e-4 && (post_bb.height() - 20.0).abs() < 1e-4,
        "unexpected post-edit bounds: {post_bb:?}"
    );
    let rendered = render_page_to_rgba(
        &app.active_tab().unwrap().document,
        0,
        72.0,
        ViewMode::Enhanced,
    )
    .unwrap();
    let non_white = rendered
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] < 250 || px[1] < 250 || px[2] < 250)
        .count();
    assert!(
        non_white > 100,
        "SymbolInstance with Group must render non-blank pixels (got {non_white})"
    );

    app.undo().unwrap();
    let undo_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(inst_id)
        .unwrap();
    assert!((undo_bb.width() - 40.0).abs() < 1e-4);
    app.redo().unwrap();
    let redo_bb = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(inst_id)
        .unwrap();
    assert!((redo_bb.width() - 60.0).abs() < 1e-4);
}

#[test]
fn w2b_r2_clone_selection_live_sync_and_unlink_with_undo_redo() {
    let mut app = StudioApp::new_headless();
    let lid = Some(app.active_tab().unwrap().document.pages()[0].layers[0].id);

    let mut master = SceneObject::new(
        ObjectId(0),
        "MasterRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(220, 30, 30, 255)));
    master.transform = Affine2D::translate(10.0, 10.0);
    app.execute_command(CommandOp::create_object(master, lid))
        .unwrap();
    let master_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, master_id);
    }

    let cloned_ids = app.clone_selection();
    assert_eq!(cloned_ids.len(), 1);
    let clone_id = cloned_ids[0];
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(clone_id)
            .unwrap()
            .payload,
        NodePayload::Clone {
            master_id: mid,
            link_fill: true,
            link_outline: true,
            link_shape: true,
        } if mid == master_id
    ));

    // Edit master fill & shape -> live sync on Clone
    app.execute_command(CommandOp::SetFill {
        ids: vec![master_id],
        fill: Fill::Solid(Color::rgba8(30, 180, 30, 255)),
    })
    .unwrap();
    app.execute_command(CommandOp::EditShape {
        id: master_id,
        shape: ParametricShape::Rectangle {
            width: 55.0,
            height: 25.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        },
    })
    .unwrap();
    {
        let doc = &app.active_tab().unwrap().document;
        let clone_obj = doc.object(clone_id).unwrap();
        let (_, eff_fill, _) = doc.resolve_clone_payload_and_style(clone_obj).unwrap();
        assert_eq!(eff_fill, Fill::Solid(Color::rgba8(30, 180, 30, 255)));
        let bb = doc.world_object_bounds(clone_id).unwrap();
        assert!((bb.width() - 55.0).abs() < 1e-4 && (bb.height() - 25.0).abs() < 1e-4);
    }

    // Unlink Clone -> independent Shape with Green 55x25
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, clone_id);
    }
    app.unlink_clone_selection();
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(clone_id)
            .unwrap()
            .payload,
        NodePayload::Shape(ParametricShape::Rectangle { width, .. }) if (width - 55.0).abs() < 1e-4
    ));

    // Undo unlink -> back to live Clone; Redo unlink -> back to independent Shape
    app.undo().unwrap();
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(clone_id)
            .unwrap()
            .payload,
        NodePayload::Clone { .. }
    ));
    app.redo().unwrap();
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(clone_id)
            .unwrap()
            .payload,
        NodePayload::Shape(_)
    ));
}

#[test]
fn w2b_r2_artistic_media_property_bar_edit_and_undo_redo() {
    use coree_core::{ArtisticMediaStyle, ArtisticWidthProfile, BezierPath};

    let mut app = StudioApp::new_headless();
    let lid = Some(app.active_tab().unwrap().document.pages()[0].layers[0].id);
    let spine = BezierPath::from_line(Point2D::new(10.0, 20.0), Point2D::new(90.0, 20.0));
    let orig_style = ArtisticMediaStyle::Preset {
        max_width_mm: 5.0,
        profile: ArtisticWidthProfile::TaperBoth,
    };
    let obj = SceneObject::new(
        ObjectId(0),
        "ArtMediaProp",
        NodePayload::ArtisticMedia {
            spine,
            style: orig_style,
        },
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 64, 255, 255)));
    app.execute_command(CommandOp::create_object(obj, lid))
        .unwrap();
    let am_id = app.active_tab().unwrap().document.pages()[0].layers[0].objects[0];
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, am_id);
    }

    assert!(app.edit_selected_artistic_media(Some(14.0), Some(ArtisticWidthProfile::Wave)));
    match &app
        .active_tab()
        .unwrap()
        .document
        .object(am_id)
        .unwrap()
        .payload
    {
        NodePayload::ArtisticMedia { style, .. } => {
            assert_eq!(
                style,
                &ArtisticMediaStyle::Preset {
                    max_width_mm: 14.0,
                    profile: ArtisticWidthProfile::Wave,
                }
            );
        }
        other => panic!("expected ArtisticMedia, got {other:?}"),
    }

    app.undo().unwrap();
    match &app
        .active_tab()
        .unwrap()
        .document
        .object(am_id)
        .unwrap()
        .payload
    {
        NodePayload::ArtisticMedia { style, .. } => {
            assert_eq!(
                style,
                &ArtisticMediaStyle::Preset {
                    max_width_mm: 5.0,
                    profile: ArtisticWidthProfile::TaperBoth,
                }
            );
        }
        other => panic!("expected ArtisticMedia after undo, got {other:?}"),
    }
    app.redo().unwrap();
    match &app
        .active_tab()
        .unwrap()
        .document
        .object(am_id)
        .unwrap()
        .payload
    {
        NodePayload::ArtisticMedia { style, .. } => {
            assert_eq!(
                style,
                &ArtisticMediaStyle::Preset {
                    max_width_mm: 14.0,
                    profile: ArtisticWidthProfile::Wave,
                }
            );
        }
        other => panic!("expected ArtisticMedia after redo, got {other:?}"),
    }
}
