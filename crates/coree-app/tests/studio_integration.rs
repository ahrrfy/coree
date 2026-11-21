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
    assert_eq!(merged_doc.pages.len(), 3);

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

    assert_eq!(app.active_tab().unwrap().document.objects.len(), 4);

    // Select all -> Group -> Ungroup -> Boolean Weld
    app.select_all();
    assert_eq!(app.active_tab().unwrap().selection.len(), 4);
    app.group_selection();
    assert_eq!(app.active_tab().unwrap().document.objects.len(), 5);
    let grp_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects
        .keys()
        .last()
        .unwrap();
    let tab = app.active_tab_mut().unwrap();
    tab.selection.select_single(&tab.document, grp_id);
    app.ungroup_selection();
    assert_eq!(app.active_tab().unwrap().document.objects.len(), 4);
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
        .objects
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

        let applied = app.active_tab().unwrap().document.objects[&host_id]
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
        expected_doc_id = app.active_tab().unwrap().document.id.to_string();

        // Execute 2 undoable commands (each appends to WAL command_journal)
        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 50.0, 30.0));
        app.commit_drawn_box(Rect2D::from_xywh(70.0, 10.0, 30.0, 30.0));
        assert_eq!(app.active_tab().unwrap().document.objects.len(), 2);

        // Undo & Redo work deterministically
        app.undo().unwrap();
        assert_eq!(app.active_tab().unwrap().document.objects.len(), 1);
        app.redo().unwrap();
        assert_eq!(app.active_tab().unwrap().document.objects.len(), 2);

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
        recovered_app.active_tab().unwrap().document.objects.len(),
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
        .objects
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
        app.active_tab().unwrap().document.objects[&rect_id]
            .effects
            .len(),
        10
    );
    app.remove_effect_on(rect_id, 0);
    assert_eq!(
        app.active_tab().unwrap().document.objects[&rect_id]
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
        app.active_tab().unwrap().document.objects[&raster_id].payload,
        NodePayload::Raster(_)
    ));

    // Exercise all 19 Bitmap Studio filters!
    assert_eq!(BitmapDockerState::FILTER_NAMES.len(), 19);
    for filter_idx in 0..19 {
        app.bitmap_state.filter_idx = filter_idx;
        let filter = app.bitmap_state.build_filter();
        app.apply_raster_filter_to_selection(filter);
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
        doc_id_a = app.active_tab().unwrap().document.id.to_string();

        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
        app.save_active_as(&saved_file).expect("save active");
        assert_eq!(app.active_tab().unwrap().journal_seq, 0);
        assert!(!app.active_tab().unwrap().wal_session_open);

        // Post-save edit must open a fresh WAL session with a baseline snapshot
        app.commit_drawn_box(Rect2D::from_xywh(50.0, 10.0, 30.0, 20.0));
        assert_eq!(app.active_tab().unwrap().document.objects.len(), 2);
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
            recovered.active_tab().unwrap().document.objects.len(),
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
        doc_id_b = app.active_tab().unwrap().document.id.to_string();

        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
        app.commit_drawn_box(Rect2D::from_xywh(50.0, 10.0, 30.0, 20.0));
        assert_eq!(app.active_tab().unwrap().document.objects.len(), 2);

        app.undo().expect("undo op2");
        assert_eq!(app.active_tab().unwrap().document.objects.len(), 1);
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
            recovered.active_tab().unwrap().document.objects.len(),
            1,
            "undone edit must NOT resurrect after crash recovery"
        );

        // Verify duplicate_selection routes through CommandHistory (undoable) and zero-offset duplicate works
        let first_id = *recovered
            .active_tab()
            .unwrap()
            .document
            .objects
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
        assert_eq!(recovered.active_tab().unwrap().document.objects.len(), 1);
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
        .objects
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

    // 4. convert_selection_to_bitmap accounts for 4.0 mm stroke/shadow padding on every side
    app.convert_selection_to_bitmap();
    let raster_id = app.active_tab().unwrap().selection.objects[0];
    let raster_obj = &app.active_tab().unwrap().document.objects[&raster_id];
    match &raster_obj.payload {
        NodePayload::Raster(r) => {
            assert!((r.width_mm - 48.0).abs() < 1e-6, "expected 40 + 8 mm");
            assert!((r.height_mm - 28.0).abs() < 1e-6, "expected 20 + 8 mm");
        }
        other => panic!("expected Raster payload, got {other:?}"),
    }
    let origin = raster_obj.transform.transform_point(Point2D::ZERO);
    assert!((origin.x - 96.0).abs() < 1e-6, "expected 100 - 4 mm");
    assert!((origin.y - 146.0).abs() < 1e-6, "expected 150 - 4 mm");
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
    src.symbols.insert(sym_id, sym);

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
    if let Some(a_obj) = src.objects.get_mut(&shape_a) {
        a_obj.effects.push(Effect::Blend {
            target_id: shape_b,
            steps: 5,
            rotation_deg: 0.0,
            along_path: Some(host_id),
        });
    }
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

    // 3. Inject an explicit cyclic Group pair (g1 <-> g2) into src to test cycle guard
    let g1 = src.alloc_object_id();
    let g2 = src.alloc_object_id();
    src.objects.insert(
        g1,
        SceneObject::new(g1, "CycleG1", NodePayload::Group(vec![g2])),
    );
    src.objects.insert(
        g2,
        SceneObject::new(g2, "CycleG2", NodePayload::Group(vec![g1])),
    );
    src.pages[0].layers[0].objects.push(g1);

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
        target.symbols.contains_key(&sym_id),
        "symbols must be copied into target"
    );

    // Verify TextOnPath, Connector, and Blend references were remapped to NEW target IDs!
    let new_host = target
        .objects
        .values()
        .find(|o| o.name == "HostPath")
        .map(|o| o.id)
        .unwrap();
    let new_a = target
        .objects
        .values()
        .find(|o| o.name == "ShapeA")
        .map(|o| o.id)
        .unwrap();
    let new_b = target
        .objects
        .values()
        .find(|o| o.name == "ShapeB")
        .map(|o| o.id)
        .unwrap();
    assert_ne!(new_host, host_id);
    assert_ne!(new_a, shape_a);
    assert_ne!(new_b, shape_b);

    let top_obj = target
        .objects
        .values()
        .find(|o| o.name == "TextOnHost")
        .unwrap();
    if let NodePayload::TextOnPath(ref top) = top_obj.payload {
        assert_eq!(top.path_id, new_host, "TextOnPath.path_id must be remapped");
    } else {
        panic!("expected TextOnPath");
    }

    let conn_obj = target.objects.values().find(|o| o.name == "Link").unwrap();
    if let NodePayload::Connector(ref c) = conn_obj.payload {
        assert_eq!(c.from_obj, Some(new_a));
        assert_eq!(c.to_obj, Some(new_b));
    } else {
        panic!("expected Connector");
    }

    let a_obj = &target.objects[&new_a];
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
    assert_eq!(app.active_tab().unwrap().document.pages.len(), 1);
    assert_eq!(app.active_tab().unwrap().document.objects.len(), 1);

    // Duplicate Page 1 -> Page 2 must contain a cloned object, and active_layer_id must point to Page 2
    app.duplicate_page(0);
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.pages.len(), 2);
        assert_eq!(tab.document.active_page_index, 1);
        assert_eq!(
            tab.document.objects.len(),
            2,
            "duplicate_page must clone source page objects onto new page"
        );
        let p2_layers: Vec<_> = tab.document.pages[1].layers.iter().map(|l| l.id).collect();
        assert!(p2_layers.contains(&tab.active_layer_id.unwrap()));
    }

    // Switch back via PrevPage shortcut -> active_layer_id must sync to Page 1
    app.handle_shortcut(ShortcutAction::PrevPage);
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.active_page_index, 0);
        let p1_layers: Vec<_> = tab.document.pages[0].layers.iter().map(|l| l.id).collect();
        assert!(p1_layers.contains(&tab.active_layer_id.unwrap()));
    }

    // Switch to Page 2 via NextPage and delete Page 2 -> active_layer_id must NOT be stale!
    app.handle_shortcut(ShortcutAction::NextPage);
    let p2_id = app.active_tab().unwrap().document.pages[1].id;
    app.execute_command(CommandOp::DeletePage { page_id: p2_id })
        .expect("delete page 2");
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.document.pages.len(), 1);
        let p1_layers: Vec<_> = tab.document.pages[0].layers.iter().map(|l| l.id).collect();
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
        app.active_tab().unwrap().document.objects[&text_id].payload,
        NodePayload::ArtisticText(_)
    ));

    app.convert_selection_to_curves();
    match &app.active_tab().unwrap().document.objects[&text_id].payload {
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
    assert!(!loaded.objects.is_empty());

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
            .objects
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
            app.active_tab().unwrap().document.objects[&oid].fill,
            Fill::ConicalGradient { .. }
        ));
        app.undo().expect("undo fill");
        assert!(!matches!(
            app.active_tab().unwrap().document.objects[&oid].fill,
            Fill::ConicalGradient { .. }
        ));
        app.redo().expect("redo fill");
        assert!(matches!(
            app.active_tab().unwrap().document.objects[&oid].fill,
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
        let applied_outline = &app.active_tab().unwrap().document.objects[&oid].outline;
        assert!((applied_outline.width_mm - 1.25).abs() < 1e-6);
        assert!((applied_outline.calligraphy_stretch - 0.55).abs() < 1e-6);
        assert_eq!(applied_outline.end_arrow, ArrowheadStyle::Triangle);
        assert!(applied_outline.overprint);
        app.undo().expect("undo outline");
        assert!(
            (app.active_tab().unwrap().document.objects[&oid]
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
        let gid = app.active_tab().unwrap().document.guides.last().unwrap().id;
        app.execute_command(CommandOp::MoveGuide {
            id: gid,
            orientation: GuideOrientation::Vertical(85.0),
        })
        .expect("move guide");
        assert_eq!(
            app.active_tab()
                .unwrap()
                .document
                .guides
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
                .guides
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
            .guides
            .iter()
            .any(|g| g.id == gid));
        app.undo().expect("undo remove guide");
        assert!(app
            .active_tab()
            .unwrap()
            .document
            .guides
            .iter()
            .any(|g| g.id == gid));
        app.undo().expect("undo toggle lock");
        app.undo().expect("undo move guide");
        assert_eq!(
            app.active_tab()
                .unwrap()
                .document
                .guides
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
