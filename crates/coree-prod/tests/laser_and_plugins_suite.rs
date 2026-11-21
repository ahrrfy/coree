// SPDX-License-Identifier: MIT OR Apache-2.0

//! Comprehensive integration and regression test suite for:
//! - Laser Cutting, Engraving & Plotter Toolpath Engine (`laser_plugin`)
//! - Extensible External Bridge & Sandboxed Rhai Plugin Architecture (`plugins`)
//! - Deep Audit edge-case hardening across all `coree-prod` modules.

use coree_core::{
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, CommandHistory, ConnectorObject,
    ConnectorStyle, CornerStyle, Document, EllipseMode, Fill, MasterLayerScope, NodePayload,
    ObjectId, OutlineStyle, ParametricShape, Point2D, RasterAsset, RasterLayerData, SceneObject,
    TextStyle,
};
use coree_prod::{
    add_barcode_to_document, add_qr_code_to_document, apply_kerf_offset, apply_print_merge,
    collect_for_output, compute_production_metrics, compute_separations, default_laser_layer_rules,
    detect_executable, discover_plugins, export_for_plugin, export_laser_dxf_r12, export_laser_eps,
    export_laser_gcode, export_laser_plt, export_laser_svg, find_and_replace, generate_barcode,
    generate_cut_contour, generate_dieline, generate_hatch_fill, generate_print_marks,
    impose_document, install_sample_plugin_template, join_open_curves, launch_external_bridge,
    plan_laser_toolpath, remove_duplicate_segments, run_macro, run_preflight, run_recorded_macro,
    run_rhai_plugin, simulate_overprint_rgba, substitute_bridge_args, BarcodeOptions,
    BarcodeSymbology, CollectOptions, ColorMatch, DielineKind, FindQuery, FindScope, GcodeDialect,
    ImpositionMode, LaserJobConfig, LaserLayerRule, LaserOperationKind, LaserOrigin,
    LaserToolpathPlan, MacroSandboxConfig, MacroValue, PluginCategory, PluginExportFormat,
    PluginKind, PluginManifest, PluginRegistry, PreflightIssueKind, PreflightRules,
    PrintMarksOptions, PrintMergeOptions, PrintMergeTable, QrCodeOptions, ReplaceAction,
    SeparationOptions, SerialNumberConfig, ToolpathSegment,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn make_rect(x: f64, y: f64, w: f64, h: f64, fill: Fill, outline: OutlineStyle) -> SceneObject {
    SceneObject::new(
        ObjectId(0),
        "Rect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: w,
            height: h,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(fill)
    .with_outline(outline)
    .with_transform(Affine2D::translate(x, y))
}

#[test]
fn laser_dedupe_grid_of_tiled_squares_removes_all_shared_internal_edges() {
    // 2x2 grid of 10x10 squares sharing 4 internal edges (each of length 10mm)
    let mut squares = Vec::new();
    for r in 0..2 {
        for c in 0..2 {
            let x = (c as f64) * 10.0;
            let y = (r as f64) * 10.0;
            squares.push(vec![
                Point2D::new(x, y),
                Point2D::new(x + 10.0, y),
                Point2D::new(x + 10.0, y + 10.0),
                Point2D::new(x, y + 10.0),
                Point2D::new(x, y),
            ]);
        }
    }

    let (deduped, removed) = remove_duplicate_segments(&squares, 0.05);
    assert_eq!(removed, 4, "2x2 tiled grid has exactly 4 shared edges");
    // 4*4 = 16 raw edges - 4 duplicates = 12 unique edges * 10mm = 120mm
    let total_len: f64 = deduped
        .iter()
        .map(|p| p.windows(2).map(|w| w[0].distance_to(w[1])).sum::<f64>())
        .sum();
    assert!((total_len - 120.0).abs() < 1e-6);

    // Zero / NaN tolerance disables dedupe without panic
    let (unchanged, zero_rem) = remove_duplicate_segments(&squares, 0.0);
    assert_eq!(zero_rem, 0);
    assert_eq!(unchanged.len(), 4);
    let (_, nan_rem) = remove_duplicate_segments(
        &[vec![Point2D::new(f64::NAN, 0.0), Point2D::new(1.0, 1.0)]],
        f64::NAN,
    );
    assert_eq!(nan_rem, 0);
}

#[test]
fn laser_join_open_curves_respects_tolerance_and_reverses_segments() {
    // 3 segments forming an open U-shape with middle segment reversed, plus 1 disconnected segment
    let segs = vec![
        vec![Point2D::new(0.0, 0.0), Point2D::new(10.0, 0.0)],
        vec![Point2D::new(10.0, 10.0), Point2D::new(10.0, 0.02)],
        vec![Point2D::new(10.0, 10.0), Point2D::new(0.0, 10.0)],
        vec![Point2D::new(50.0, 50.0), Point2D::new(60.0, 50.0)],
    ];
    let (joined, count) = join_open_curves(&segs, 0.05);
    assert_eq!(count, 2);
    assert_eq!(joined.len(), 2);

    // Tight tolerance (0.005mm < 0.02mm gap) must NOT bridge the 0.02mm gap
    let (tight, tight_count) = join_open_curves(&segs, 0.005);
    assert_eq!(tight_count, 1); // only seg1-seg2 share exact (10,10)
    assert_eq!(tight.len(), 3);
}

#[test]
fn laser_kerf_offset_handles_open_curves_and_degenerate_inputs() {
    let open = vec![
        Point2D::new(0.0, 0.0),
        Point2D::new(20.0, 0.0),
        Point2D::new(20.0, 20.0),
    ];
    let shifted = apply_kerf_offset(&open, false, 0.6); // half = 0.3mm
    assert_eq!(shifted.len(), 3);
    // Horizontal edge (0,0)->(20,0) has left normal (0, -1), vertical (20,0)->(20,20) has (+1, 0)
    assert!((shifted[0].x - 0.0).abs() < 1e-6);
    assert!((shifted[0].y - (-0.3)).abs() < 1e-6);
    assert!((shifted[1].x - 20.3).abs() < 1e-6);
    assert!((shifted[1].y - (-0.3)).abs() < 1e-6);
    assert!((shifted[2].x - 20.3).abs() < 1e-6);
    assert!((shifted[2].y - 20.0).abs() < 1e-6);

    // Zero or NaN kerf is a clean no-op
    assert_eq!(apply_kerf_offset(&open, false, 0.0), open);
    assert_eq!(apply_kerf_offset(&open, false, f64::NAN), open);
}

#[test]
fn laser_hatch_fill_handles_3_level_island_nesting_and_adversarial_spacing() {
    let sq = |x: f64, y: f64, s: f64| {
        vec![
            Point2D::new(x, y),
            Point2D::new(x + s, y),
            Point2D::new(x + s, y + s),
            Point2D::new(x, y + s),
            Point2D::new(x, y),
        ]
    };
    // Outer [0..30], Hole [5..25], Inner Island [10..20] -> at y=15, filled spans are [0,5], [10,20], [25,30] (3 spans!)
    let outer = sq(0.0, 0.0, 30.0);
    let hole = sq(5.0, 5.0, 20.0);
    let island = sq(10.0, 10.0, 10.0);
    let hatch = generate_hatch_fill(&[outer.clone(), hole, island], 5.0, 0.0, false, 0.0);
    // Scanlines at y = 2.5 (1 seg), 7.5 (2 segs), 12.5 (3 segs), 17.5 (3 segs), 22.5 (2 segs), 27.5 (1 seg) = 12 segs
    assert_eq!(hatch.len(), 12);

    // Adversarial tiny spacing must stay bounded (no OOM) and invalid spacing returns empty
    let bounded = generate_hatch_fill(&[outer], 1e-9, 0.0, false, 0.0);
    assert!(!bounded.is_empty());
    assert!(bounded.len() <= 20_000);
    assert!(generate_hatch_fill(&[], 1.0, 0.0, false, 0.0).is_empty());
    assert!(generate_hatch_fill(&[sq(0.0, 0.0, 10.0)], f64::NAN, 0.0, false, 0.0).is_empty());
    assert!(generate_hatch_fill(&[sq(0.0, 0.0, 10.0)], -1.0, 0.0, false, 0.0).is_empty());
}

#[test]
fn laser_toolpath_three_level_nesting_travel_optimization_group_transforms_and_origins() {
    let mut doc = Document::new("Laser Panel", 200.0, 150.0, ColorMode::Rgb, 300.0);
    let layer = doc.pages[0].layers[0].id;

    // 3-level nested concentric red squares (added outer-first so planner MUST sort inner-first!)
    let outer_id = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                100.0,
                100.0,
                Fill::None,
                OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)),
            ),
            Some(layer),
        )
        .unwrap();
    let mid_id = doc
        .add_object(
            make_rect(
                20.0,
                20.0,
                80.0,
                80.0,
                Fill::None,
                OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)),
            ),
            Some(layer),
        )
        .unwrap();
    let inner_id = doc
        .add_object(
            make_rect(
                35.0,
                35.0,
                50.0,
                50.0,
                Fill::None,
                OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)),
            ),
            Some(layer),
        )
        .unwrap();

    // Group two far-apart blue open lines and apply a group translation transform
    let l1 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Line Far",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(100.0, 0.0),
                    Point2D::new(120.0, 0.0),
                )),
            )
            .with_outline(OutlineStyle::solid(Color::rgba8(0, 0, 255, 255), 0.2)),
            Some(layer),
        )
        .unwrap();
    // Line 2 is reversed: starts at x=80, ends at x=20 — travel optimizer should reverse it when starting near x=0!
    let l2 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Line Near Reversed",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(80.0, 0.0),
                    Point2D::new(20.0, 0.0),
                )),
            )
            .with_outline(OutlineStyle::solid(Color::rgba8(0, 0, 255, 255), 0.2)),
            Some(layer),
        )
        .unwrap();
    let grp_id = doc.group_objects(&[l1, l2], "Score Group").unwrap();
    doc.objects.get_mut(&grp_id).unwrap().transform = Affine2D::translate(5.0, 125.0);

    // Curved circle (radius 10mm) to verify adaptive flattening <= 0.05mm chord accuracy
    let circle_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Circle Cut",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 10.0,
                    ry: 10.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::cut_contour())
            .with_transform(Affine2D::translate(160.0, 50.0)),
            Some(layer),
        )
        .unwrap();

    // Ignored yellow guide rectangle
    let mut cfg = LaserJobConfig::default();
    cfg.rules.push(LaserLayerRule {
        name: "Yellow Guide (Ignore)".to_owned(),
        match_rgb: [255, 255, 0],
        color_tolerance: 30,
        match_cut_layer: false,
        operation: LaserOperationKind::Ignore,
        speed_mm_s: 10.0,
        min_power_pct: 0.0,
        max_power_pct: 0.0,
        passes: 1,
        air_assist: false,
        kerf_mm: 0.0,
        z_step_mm: 0.0,
        frequency_khz: 20.0,
        hatch_spacing_mm: 0.1,
        hatch_angle_deg: 0.0,
        cross_hatch: false,
        enabled: true,
    });
    doc.add_object(
        make_rect(
            140.0,
            100.0,
            30.0,
            20.0,
            Fill::None,
            OutlineStyle::solid(Color::rgba8(255, 255, 0, 255), 0.2),
        ),
        Some(layer),
    )
    .unwrap();

    // Compare travel optimized vs unoptimized
    let mut unopt_cfg = cfg.clone();
    unopt_cfg.optimize_travel = false;
    unopt_cfg.origin = LaserOrigin::TopLeft;
    cfg.origin = LaserOrigin::TopLeft;

    let plan_opt = plan_laser_toolpath(&doc, 0, None, &cfg).unwrap();
    let plan_unopt = plan_laser_toolpath(&doc, 0, None, &unopt_cfg).unwrap();

    // Yellow guide must be completely excluded
    assert!(!plan_opt
        .segments
        .iter()
        .any(|s| s.operation == LaserOperationKind::Ignore));

    // Verify travel optimizer strictly reduced rapid jump distance
    assert!(
        plan_opt.total_rapid_mm < plan_unopt.total_rapid_mm,
        "expected optimized rapid {} < unoptimized {}",
        plan_opt.total_rapid_mm,
        plan_unopt.total_rapid_mm
    );

    // Verify Group transform was applied to blue lines (y == 125.0) and nearer line was reversed to start at x=25
    let first_blue = plan_opt
        .segments
        .iter()
        .find(|s| s.operation == LaserOperationKind::VectorEngrave)
        .unwrap();
    assert!((first_blue.points[0].y - 125.0).abs() < 1e-4);
    assert!(
        (first_blue.points[0].x - 25.0).abs() < 1e-4,
        "expected reversed open line starting at x=25, got {}",
        first_blue.points[0].x
    );

    // Verify 3-level inner-first cut ordering: depth 2 -> depth 1 -> depth 0
    let cut_depths: Vec<usize> = plan_opt
        .segments
        .iter()
        .filter(|s| s.operation == LaserOperationKind::Cut)
        .map(|s| s.containment_depth)
        .collect();
    assert_eq!(cut_depths[0], 2);
    assert_eq!(cut_depths[1], 1);
    assert!(cut_depths[2..].iter().all(|&d| d == 0));

    // Verify circle flattening precision: circumference = 2 * pi * 10 = 62.83185mm within 0.1mm
    let sel_circle = plan_laser_toolpath(&doc, 0, Some(&[circle_id]), &cfg).unwrap();
    assert_eq!(sel_circle.segments.len(), 1);
    assert!(
        (sel_circle.total_cut_mm - 2.0 * std::f64::consts::PI * 10.0).abs() < 0.15,
        "circle length was {}",
        sel_circle.total_cut_mm
    );

    // Verify selection_filter on a Group includes its children
    let sel_grp = plan_laser_toolpath(&doc, 0, Some(&[grp_id]), &cfg).unwrap();
    assert_eq!(sel_grp.segments.len(), 2);

    // Verify selection_filter on subset of concentric squares
    let sel_two = plan_laser_toolpath(&doc, 0, Some(&[outer_id, mid_id, inner_id]), &cfg).unwrap();
    assert_eq!(sel_two.segments.len(), 3);

    // Verify all 3 LaserOrigin modes and RuidaGcode dialect produce valid G-Code
    for origin in [
        LaserOrigin::TopLeft,
        LaserOrigin::BottomLeft,
        LaserOrigin::Center,
    ] {
        let mut ocfg = cfg.clone();
        ocfg.origin = origin;
        ocfg.dialect = GcodeDialect::RuidaGcode;
        let gc = export_laser_gcode(&sel_circle, &ocfg);
        assert!(gc.contains("G21"));
        assert!(gc.contains("Ruida MinPower="));
        assert!(gc.contains("M5"));
    }

    // Verify exact HPGL 40 units/mm conversion on a known 10mm line
    let mut line_doc = Document::new("HPGL Check", 100.0, 100.0, ColorMode::Rgb, 300.0);
    line_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "10mm Cut",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(10.0, 20.0),
                    Point2D::new(25.0, 20.0),
                )),
            )
            .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255))),
            None,
        )
        .unwrap();
    let plt_cfg = LaserJobConfig {
        optimize_travel: false,
        ..LaserJobConfig::default()
    };
    let line_plan = plan_laser_toolpath(&line_doc, 0, None, &plt_cfg).unwrap();
    let plt = export_laser_plt(&line_plan);
    // x0=10*40=400, y0=(100-20)*40=3200; x1=25*40=1000, y1=3200
    assert!(plt.contains("PU400,3200;PD1000,3200;"), "got PLT:\n{plt}");

    // Out-of-range page index returns PageOutOfRange error
    assert!(plan_laser_toolpath(&doc, 9, None, &cfg).is_err());
}

#[test]
fn plugin_security_discovery_overrides_exporters_and_safe_bridge_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();

    // 1. Install sample template plugin
    let sample_dir = install_sample_plugin_template(&plugins_dir).unwrap();
    assert!(sample_dir.join("plugin.json").is_file());
    assert!(sample_dir.join("macro.rhai").is_file());

    // 2. Reject absolute script path, Windows backslash traversal, and non-.rhai extension
    for (dir_name, script_file) in [
        ("bad-abs", "/etc/passwd.rhai"),
        ("bad-win", r"..\escape.rhai"),
        ("bad-ext", "evil.sh"),
        ("bad-dotdot", "sub/../../escape.rhai"),
    ] {
        let d = plugins_dir.join(dir_name);
        fs::create_dir_all(&d).unwrap();
        let json = format!(
            r#"{{
                "id": "{dir_name}",
                "name": "Bad",
                "version": "1.0.0",
                "author": "x",
                "description": "bad",
                "category": "Custom",
                "kind": {{ "RhaiScript": {{ "script_file": "{script_file}", "entry_label": "Run" }} }}
            }}"#
        );
        fs::write(d.join("plugin.json"), json).unwrap();
    }

    // 3. On Unix, also verify symlinked plugin folder & symlinked manifest are rejected!
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = tmp.path().join("outside_secret");
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join("plugin.json"),
            r#"{"id":"sym-escape","name":"Sym","version":"1.0","author":"x","description":"x","category":"Custom","kind":{"RhaiScript":{"script_file":"a.rhai","entry_label":"Run"}}}"#,
        )
        .unwrap();
        fs::write(outside.join("a.rhai"), "log(\"escaped\");").unwrap();
        let _ = symlink(&outside, plugins_dir.join("sym-escape"));
    }

    // 4. Add a custom user TOML override for built-in `lightburn-bridge` switching it to EPS!
    let lb_override = plugins_dir.join("lightburn-override");
    fs::create_dir_all(&lb_override).unwrap();
    fs::write(
        lb_override.join("plugin.toml"),
        r#"
[plugin]
id = "lightburn-bridge"
name = "LightBurn (Custom EPS)"
version = "1.1.0"
author = "Shop"
description = "Custom override exporting EPS"
category = "Laser"
kind = "ExternalBridge"

[bridge]
target_app = "LightBurn"
export_format = "Eps"
args_template = ["{file}"]
auto_dedupe = true
auto_join_mm = 0.05
auto_hatch = false
"#,
    )
    .unwrap();

    let discovered = discover_plugins(std::slice::from_ref(&plugins_dir));
    assert_eq!(discovered.len(), 2, "only sample + valid override allowed");

    let mut registry = PluginRegistry::with_builtins();
    assert_eq!(registry.by_category(PluginCategory::Plotter).len(), 1);
    let loaded = registry.load_discovered(&[plugins_dir]);
    assert_eq!(loaded, 2);
    // Built-in `lightburn-bridge` was overridden cleanly in place (7 presets + 1 new sample = 8 total)
    assert_eq!(registry.plugins().len(), 8);
    let lb = registry.find("lightburn-bridge").unwrap();
    assert!(!lb.builtin);
    assert!(matches!(
        lb.kind,
        PluginKind::ExternalBridge {
            export_format: PluginExportFormat::Eps,
            ..
        }
    ));

    // Export EPS via overridden LightBurn bridge
    let mut doc = Document::new_a4("Arabic Shield درع");
    doc.add_object(
        make_rect(
            10.0,
            10.0,
            50.0,
            30.0,
            Fill::None,
            OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)),
        ),
        None,
    )
    .unwrap();
    let eps_out = tmp.path().join("exports/shield.eps");
    let outcome =
        export_for_plugin(&doc, 0, None, lb, &LaserJobConfig::default(), &eps_out).unwrap();
    assert_eq!(outcome.format, PluginExportFormat::Eps);
    let eps_txt = fs::read_to_string(&eps_out).unwrap();
    assert!(eps_txt.starts_with("%!PS-Adobe-3.0 EPSF-3.0"));

    // Verify `vinyl-plotter-hpgl` with weeding box exports CutContour + weeding box to PLT
    let plotter = registry.find("vinyl-plotter-hpgl").unwrap();
    let plot_cfg = LaserJobConfig {
        add_weeding_box: true,
        ..LaserJobConfig::default()
    };
    let plt_out = tmp.path().join("exports/vinyl.plt");
    let plot_res = export_for_plugin(&doc, 0, None, plotter, &plot_cfg, &plt_out).unwrap();
    assert_eq!(plot_res.plan.segments.len(), 2); // box + weeding box
    assert!(plot_res.plan.segments.last().unwrap().is_weeding_box);

    // Verify `launch_external_bridge` rejects missing executables and directories
    assert!(launch_external_bridge(
        Path::new("/nonexistent/laser/binary"),
        &["{file}".to_owned()],
        &eps_out
    )
    .is_err());
    assert!(launch_external_bridge(tmp.path(), &["{file}".to_owned()], &eps_out).is_err());

    // On Unix, verify `launch_external_bridge` executes directly with zero shell interpretation!
    #[cfg(unix)]
    {
        let marker = tmp.path().join("pwned_if_shell_ran");
        let hostile_name = tmp
            .path()
            .join("design; touch pwned_if_shell_ran $(whoami) `id`.dxf");
        fs::write(&hostile_name, b"0\nEOF\n").unwrap();
        let true_bin = Path::new("/bin/true");
        if true_bin.is_file() {
            let pid =
                launch_external_bridge(true_bin, &["{file}".to_owned()], &hostile_name).unwrap();
            assert!(pid > 0);
            assert!(
                !marker.exists(),
                "shell metacharacters in filename must NEVER execute!"
            );
        }
    }

    // Verify `run_rhai_plugin` works cleanly and rejects non-Rhai bridge plugins
    let sample_plugin = registry.find("custom-laser-example").unwrap().clone();
    let macro_res = run_rhai_plugin(
        &mut doc,
        &sample_plugin,
        &BTreeMap::new(),
        &MacroSandboxConfig::default(),
    )
    .unwrap();
    assert!(macro_res.commands_executed >= 1);
    assert!(run_rhai_plugin(
        &mut doc,
        plotter,
        &BTreeMap::new(),
        &MacroSandboxConfig::default()
    )
    .is_err());

    // Verify `detect_executable` returns None for RhaiScript and respects custom override
    assert_eq!(detect_executable(&sample_plugin, None), None);
    let argv = substitute_bridge_args(&[], Path::new("/tmp/a.svg")).unwrap();
    assert_eq!(argv, vec!["/tmp/a.svg"]);

    // Verify unregister & custom registration
    assert!(registry.unregister("custom-laser-example"));
    assert!(registry.find("custom-laser-example").is_none());
    let custom_manifest = PluginManifest {
        id: "marlin-cnc".to_owned(),
        name: "Marlin CNC".to_owned(),
        version: "1.0.0".to_owned(),
        author: "User".to_owned(),
        description: "Marlin export".to_owned(),
        category: PluginCategory::Laser,
        kind: PluginKind::ExternalBridge {
            target_app: "pronterface".to_owned(),
            default_paths_linux: vec![],
            default_paths_windows: vec![],
            args_template: vec!["{file}".to_owned()],
            export_format: PluginExportFormat::GcodeMarlin,
            selection_only: true,
            auto_dedupe: true,
            auto_join_mm: 0.05,
            auto_hatch: false,
        },
        source_dir: None,
        builtin: false,
    };
    registry.register(custom_manifest.clone()).unwrap();
    // `selection_only: true` with explicit empty slice errors cleanly
    assert!(export_for_plugin(
        &doc,
        0,
        Some(&[]),
        &custom_manifest,
        &LaserJobConfig::default(),
        &tmp.path().join("m.gcode")
    )
    .is_err());
    // Default laser rules sanity
    assert_eq!(default_laser_layer_rules().len(), 4);
}

#[test]
fn audit_edge_cases_empty_docs_nan_inf_arabic_utf8_and_locked_layers() {
    // 1. Empty document (0 pages) handled safely across all engines
    let mut empty_doc = Document::new_a4("فارغ");
    empty_doc.pages.clear();
    assert!(plan_laser_toolpath(&empty_doc, 0, None, &LaserJobConfig::default()).is_err());
    assert!(compute_separations(&empty_doc, 0, &SeparationOptions::default()).is_err());
    assert!(impose_document(
        &empty_doc,
        &ImpositionMode::BookletSaddleStitch {
            sheet_w_mm: 297.0,
            sheet_h_mm: 210.0,
        }
    )
    .is_err());
    assert!(generate_print_marks(&mut empty_doc, 0, &PrintMarksOptions::default()).is_err());
    let empty_preflight = run_preflight(&empty_doc, &PreflightRules::default());
    assert!(empty_preflight.has_issue_kind(PreflightIssueKind::EmptyDocument));
    let empty_metrics = compute_production_metrics(&empty_doc, None);
    assert_eq!(empty_metrics.object_count, 0);

    // 2. Barcode & QR reject NaN/Inf origins and absurd dimensions without mutating doc
    let mut doc = Document::new_a4("درع الشكر والتقدير");
    let before_count = doc.objects.len();
    assert!(add_qr_code_to_document(
        &mut doc,
        "مرحبا",
        Point2D::new(f64::NAN, 10.0),
        &QrCodeOptions::default(),
        None
    )
    .is_err());
    assert!(add_barcode_to_document(
        &mut doc,
        BarcodeSymbology::Code128,
        "ABC-123",
        Point2D::new(f64::INFINITY, 10.0),
        &BarcodeOptions::default(),
        None
    )
    .is_err());
    assert_eq!(doc.objects.len(), before_count);
    assert!(generate_barcode(
        BarcodeSymbology::Code128,
        "ABC",
        &BarcodeOptions {
            module_width_mm: f64::MAX,
            ..BarcodeOptions::default()
        }
    )
    .is_err());

    // 3. Dieline & CutContour reject extreme overflow dimensions cleanly
    assert!(generate_dieline(
        &mut doc,
        0,
        &DielineKind::TuckEndBox {
            length_mm: 1e300,
            width_mm: 10.0,
            height_mm: 10.0,
            glue_flap_mm: 5.0,
            tuck_flap_mm: 5.0,
        },
        Point2D::ZERO
    )
    .is_err());
    let r_id = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                20.0,
                20.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    assert!(generate_cut_contour(&mut doc, &[r_id], 50_000.0, "CutContour").is_err());

    // 4. Preflight sanitizes NaN rules & non-finite raster transforms without panicking or NaN output
    let asset = RasterAsset::new(8, 8, vec![128; 8 * 8 * 4], 300.0, ColorMode::Cmyk).unwrap();
    let aid = doc.register_asset(asset);
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Bad Matrix Raster",
            NodePayload::Raster(RasterLayerData {
                asset_id: aid,
                width_mm: f64::NAN,
                height_mm: 10.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        )
        .with_transform(Affine2D {
            m: [f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0],
        }),
        None,
    )
    .unwrap();
    let nan_rules = PreflightRules {
        min_dpi: f64::NAN,
        min_outline_width_mm: f64::INFINITY,
        bleed_edge_tolerance_mm: f64::NAN,
        max_tac_pct: f64::NAN,
        ..PreflightRules::default()
    };
    let pf = run_preflight(&doc, &nan_rules);
    assert!(pf.page_count == 1);

    // 5. Find & Replace respects LOCKED LAYERS + NaN ColorMatch tolerance safely
    let locked_layer = doc
        .add_layer(
            doc.pages[0].id,
            "Locked Brand",
            false,
            MasterLayerScope::None,
        )
        .unwrap();
    let locked_layer_obj = doc
        .add_object(
            make_rect(
                50.0,
                50.0,
                20.0,
                20.0,
                Fill::Solid(Color::MAGENTA),
                OutlineStyle::none(),
            ),
            Some(locked_layer),
        )
        .unwrap();
    doc.set_layer_flags(locked_layer, None, Some(true), None, None)
        .unwrap();
    let q = FindQuery {
        scope: FindScope::WholeDocument,
        fill_color: Some(ColorMatch::within_delta_e(Color::MAGENTA, f32::NAN)),
        ..FindQuery::default()
    };
    let rep = find_and_replace(
        &mut doc,
        &q,
        &ReplaceAction::ReplaceFillColor {
            color: Color::YELLOW,
        },
    );
    assert_eq!(rep.matched_ids, vec![locked_layer_obj]);
    assert!(
        rep.modified_ids.is_empty(),
        "objects on locked layers must not be mutated"
    );
    assert_eq!(
        doc.objects[&locked_layer_obj].fill,
        Fill::Solid(Color::MAGENTA)
    );

    // 6. Print merge caps absurd serial pad_width (no OOM) and handles Arabic headers
    let huge_serial = SerialNumberConfig {
        column_name: "الرقم".to_owned(),
        start: 1,
        step: 1,
        pad_width: 100_000_000,
        prefix: "AR-".to_owned(),
        suffix: String::new(),
    };
    let formatted = huge_serial.format_row(0);
    assert!(formatted.len() <= 70, "pad_width must be safely capped");
    let ar_csv = "الاسم,المدينة\nأحمد,الرياض\nسارة,القاهرة\n";
    let ar_tbl = PrintMergeTable::from_csv(ar_csv).unwrap();
    let mut tpl = Document::new_a4("شهادة");
    tpl.add_object(
        SceneObject::new(
            ObjectId(0),
            "Arabic Placeholder",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "مرحباً <<الاسم>> من <<المدينة>>",
                TextStyle::default(),
            )),
        ),
        None,
    )
    .unwrap();
    let merged = apply_print_merge(&tpl, &ar_tbl, &PrintMergeOptions::default()).unwrap();
    assert_eq!(merged.pages.len(), 2);
    assert!(PrintMergeTable::from_delimited("A\"B\n1\"2", '"').is_err());

    // 7. Sandboxed macros reject NaN/Inf transforms & page sizes atomically
    let mut macro_doc = Document::new_a4("Macro Safety");
    let obj_id = macro_doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                20.0,
                20.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    let snap = macro_doc.clone();
    let mut nan_vars = BTreeMap::new();
    nan_vars.insert("bad".to_owned(), MacroValue::Float(f64::NAN));
    assert!(run_macro(
        &mut macro_doc,
        "translate_objects(all_object_ids(), bad, 0.0);",
        &nan_vars,
        &MacroSandboxConfig::default()
    )
    .is_err());
    assert!(run_macro(
        &mut macro_doc,
        "transform_objects(all_object_ids(), [bad, 0.0, 0.0, 1.0, 0.0, 0.0]);",
        &nan_vars,
        &MacroSandboxConfig::default()
    )
    .is_err());
    assert!(run_macro(
        &mut macro_doc,
        "add_page(\"Bad\", -10.0, 100.0, 0.0);",
        &BTreeMap::new(),
        &MacroSandboxConfig::default()
    )
    .is_err());
    assert_eq!(macro_doc, snap);
    let mut hist = CommandHistory::default();
    assert!(run_recorded_macro(
        &mut macro_doc,
        &mut hist,
        &format!(
            "transform_objects([{}], [1.0/0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);",
            obj_id.0
        ),
        &MacroSandboxConfig::default()
    )
    .is_err());

    // 8. Collect for output handles pure Arabic doc title cleanly and rejects zero-dim assets
    let tmp = tempfile::tempdir().unwrap();
    let ar_doc = Document::new_a4("درع الشكر والتقدير");
    let col = collect_for_output(
        &ar_doc,
        &tmp.path().join("ar_bundle"),
        &CollectOptions::default(),
    )
    .unwrap();
    assert!(col.manifest_path.is_file());
    assert!(col.document_json_path.unwrap().is_file());

    // 9. Prepress separations reject invalid page dimensions and handle Inf dash patterns safely
    let mut bad_dash_doc = Document::new_a4("Dash Guard");
    let mut inf_dash = OutlineStyle::solid(Color::KEY_BLACK, 0.5);
    inf_dash.dash_pattern = vec![f64::INFINITY, f64::NAN, 2.0];
    bad_dash_doc
        .add_object(
            make_rect(10.0, 10.0, 40.0, 40.0, Fill::None, inf_dash),
            None,
        )
        .unwrap();
    let sep = compute_separations(
        &bad_dash_doc,
        0,
        &SeparationOptions {
            dpi: 25.4,
            max_dimension_px: 128,
            include_cut_layers: true,
        },
    )
    .unwrap();
    assert!(!simulate_overprint_rgba(&sep).is_empty());

    // Verify zero-size path produces no panic in laser toolpath planner or metrics
    let mut zero_path_doc = Document::new_a4("Zero Path");
    zero_path_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Empty Path",
                NodePayload::Path(BezierPath::new()),
            )
            .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255))),
            None,
        )
        .unwrap();
    let zp_plan = plan_laser_toolpath(&zero_path_doc, 0, None, &LaserJobConfig::default()).unwrap();
    assert!(zp_plan.segments.is_empty());
    assert!(export_laser_dxf_r12(&zp_plan).ends_with("EOF\n"));
    assert!(export_laser_svg(&zp_plan).contains("</svg>"));
    assert!(export_laser_eps(&zp_plan).contains("%%EOF"));
}

#[test]
fn regression_1_hatch_large_coords_no_overflow_or_panic() {
    // Overflowing span_y = 1e308 - (-1e308) = Inf must return cleanly without panic
    let huge_overflow = vec![
        Point2D::new(-1e308, -1e308),
        Point2D::new(1e308, -1e308),
        Point2D::new(1e308, 1e308),
        Point2D::new(-1e308, 1e308),
        Point2D::new(-1e308, -1e308),
    ];
    let res_inf = generate_hatch_fill(&[huge_overflow], 0.1, 0.0, false, 0.0);
    assert!(res_inf.len() <= 20_000);

    // span_y = 1e20, spacing = 0.1 -> span_y / spacing = 1e21 > usize::MAX (must not panic or wrap)
    let huge_finite = vec![
        Point2D::new(0.0, 0.0),
        Point2D::new(1e20, 0.0),
        Point2D::new(1e20, 1e20),
        Point2D::new(0.0, 1e20),
        Point2D::new(0.0, 0.0),
    ];
    let res_big = generate_hatch_fill(&[huge_finite], 0.1, 0.0, false, 0.0);
    assert!(
        !res_big.is_empty() && res_big.len() <= 20_000,
        "expected bounded hatch count, got {}",
        res_big.len()
    );
}

#[test]
fn regression_2_perforate_dash_huge_and_inf_segments_no_hang_or_oom() {
    let mut doc = Document::new_a4("Perforate Adversarial");
    let green = OutlineStyle::solid(Color::rgba8(0, 170, 0, 255), 0.2);

    // 1. Segment whose Euclidean distance overflows to Inf
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Inf Len Perf",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 0.0),
                Point2D::new(1e308, 1e308),
            )),
        )
        .with_outline(green.clone()),
        None,
    )
    .unwrap();

    // 2. Segment at ~1e16 coords where f64 ULP >= 2.0 (would stall naive split loop)
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "ULP Stall Perf",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(1e16, 0.0),
                Point2D::new(1e16 + 100.0, 0.0),
            )),
        )
        .with_outline(green.clone()),
        None,
    )
    .unwrap();

    // 3. Very long 1e9 mm segment (must stay strictly capped without OOM)
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Long Perf",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 5.0),
                Point2D::new(1e9, 5.0),
            )),
        )
        .with_outline(green),
        None,
    )
    .unwrap();

    let cfg = LaserJobConfig {
        optimize_travel: false,
        ..LaserJobConfig::default()
    };
    let plan = plan_laser_toolpath(&doc, 0, None, &cfg).unwrap();
    assert!(plan.segments.len() <= 250_000);
}

#[test]
fn regression_3_dedupe_never_shreds_short_consecutive_curve_chords() {
    // Open curve composed of 40 consecutive 0.03mm chords (< default dedupe_tolerance_mm = 0.05!)
    let mut short_curve = Vec::new();
    for i in 0..=40 {
        let x = (i as f64) * 0.03;
        let y = (x * 2.0).sin() * 0.1;
        short_curve.push(Point2D::new(x, y));
    }
    let orig_len: f64 = short_curve.windows(2).map(|w| w[0].distance_to(w[1])).sum();
    let (deduped, removed) = remove_duplicate_segments(&[short_curve], 0.05);
    assert_eq!(
        removed, 0,
        "consecutive short chords must NEVER self-dedupe"
    );
    assert_eq!(deduped.len(), 1);
    let kept_len: f64 = deduped[0].windows(2).map(|w| w[0].distance_to(w[1])).sum();
    assert!((kept_len - orig_len).abs() < 1e-9);

    // Small 1.0mm-radius circle under default LaserJobConfig must preserve its full ~6.28mm circumference!
    let mut doc = Document::new_a4("Small Hole");
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "1mm Bolt Hole",
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 1.0,
                ry: 1.0,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)))
        .with_transform(Affine2D::translate(20.0, 20.0)),
        None,
    )
    .unwrap();
    let plan = plan_laser_toolpath(&doc, 0, None, &LaserJobConfig::default()).unwrap();
    assert_eq!(plan.duplicate_segments_removed, 0);
    assert_eq!(plan.segments.len(), 1);
    assert!(
        (plan.total_cut_mm - 2.0 * std::f64::consts::PI).abs() < 0.15,
        "small hole circumference shredded: got {}",
        plan.total_cut_mm
    );
}

#[test]
fn regression_4_join_open_curves_no_premature_loop_collapse_on_short_chords() {
    // 10 tiny 0.02mm open segments in sequence (first 2 chords have dist(0, 2) = 0.04 <= tol 0.05!)
    let mut chords = Vec::new();
    for i in 0..10 {
        chords.push(vec![
            Point2D::new((i as f64) * 0.02, 0.0),
            Point2D::new(((i + 1) as f64) * 0.02, 0.0),
        ]);
    }
    let (joined, count) = join_open_curves(&chords, 0.05);
    assert_eq!(count, 9, "all 10 short chords must stitch into 1 open line");
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].len(), 11);
    assert!(
        (joined[0].last().unwrap().x - 0.20).abs() < 1e-9,
        "end of open line must not collapse back to start"
    );

    // 2 segments forming a tiny V-shape [A, B, C] with dist(A, C) <= tol must NOT snap into zero-area spike [A, B, A]
    let v_chords = vec![
        vec![Point2D::new(0.0, 0.0), Point2D::new(0.02, 0.0)],
        vec![Point2D::new(0.02, 0.0), Point2D::new(0.04, 0.0)],
    ];
    let (v_joined, v_count) = join_open_curves(&v_chords, 0.05);
    assert_eq!(v_count, 1);
    assert_eq!(v_joined.len(), 1);
    assert!((v_joined[0].last().unwrap().x - 0.04).abs() < 1e-9);
}

#[test]
fn regression_5_travel_optimizer_never_drops_segments_on_extreme_coords() {
    let mut doc = Document::new_a4("Extreme Coords");
    let red = OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255));
    // Two far-apart segments whose inter-segment distance overflows to Inf (1e308 - (-1e308))
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Far Left",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(-1e308, 0.0),
                Point2D::new(-1e308 + 1e300, 0.0),
            )),
        )
        .with_outline(red.clone()),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Far Right",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(1e308 - 1e300, 0.0),
                Point2D::new(1e308, 0.0),
            )),
        )
        .with_outline(red),
        None,
    )
    .unwrap();

    let cfg = LaserJobConfig {
        dedupe_tolerance_mm: 0.0,
        join_tolerance_mm: 0.0,
        optimize_travel: true,
        ..LaserJobConfig::default()
    };
    let plan = plan_laser_toolpath(&doc, 0, None, &cfg).unwrap();
    assert_eq!(
        plan.segments.len(),
        2,
        "travel optimizer must NEVER silently drop segments when distances overflow to Inf"
    );
}

#[test]
fn regression_6_gcode_comment_injection_nan_sanitization_and_moved_connector() {
    // 1. G-Code newline injection via `rule_name` + NaN float sanitization across all exporters
    let malicious_plan = LaserToolpathPlan {
        page_index: 0,
        page_width_mm: f64::NAN,
        page_height_mm: 100.0,
        bounds: None,
        segments: vec![ToolpathSegment {
            operation: LaserOperationKind::Cut,
            rule_name: "Cut\nG0 X999 Y999\r\nM3 S1000".to_owned(),
            color_rgb: [255, 0, 0],
            points: vec![Point2D::new(10.0, 10.0), Point2D::new(20.0, 10.0)],
            closed: false,
            speed_mm_s: f64::NAN,
            min_power_pct: f64::NAN,
            max_power_pct: f64::NAN,
            passes: 1,
            air_assist: false,
            z_step_mm: f64::NAN,
            frequency_khz: f64::NAN,
            containment_depth: 0,
            is_weeding_box: false,
        }],
        total_cut_mm: f64::NAN,
        total_engrave_mm: f64::INFINITY,
        total_rapid_mm: f64::NAN,
        estimated_seconds: f64::NAN,
        cut_count: 1,
        duplicate_segments_removed: 0,
        curves_joined: 0,
    };

    let ruida_cfg = LaserJobConfig {
        dialect: GcodeDialect::RuidaGcode,
        ..LaserJobConfig::default()
    };
    let gcode = export_laser_gcode(&malicious_plan, &ruida_cfg);
    // Neither injected command may ever start a non-comment line!
    for line in gcode.lines() {
        let trimmed = line.trim();
        assert!(
            !trimmed.starts_with("G0 X999"),
            "newline injection leaked raw G0 command: {line}"
        );
        assert!(
            !trimmed.starts_with("M3 S1000"),
            "newline injection leaked raw M3 command: {line}"
        );
    }
    assert!(!gcode.contains("NaN"), "G-Code contained NaN:\n{gcode}");
    assert!(!gcode.contains("inf"), "G-Code contained inf:\n{gcode}");

    let dxf = export_laser_dxf_r12(&malicious_plan);
    let plt = export_laser_plt(&malicious_plan);
    let svg = export_laser_svg(&malicious_plan);
    let eps = export_laser_eps(&malicious_plan);
    for (label, txt) in [("DXF", dxf), ("PLT", plt), ("SVG", svg), ("EPS", eps)] {
        assert!(!txt.contains("NaN"), "{label} contained NaN:\n{txt}");
        assert!(!txt.contains("inf"), "{label} contained inf:\n{txt}");
    }

    // 2. Moved/grouped standalone Connector exports at its world-transformed position
    let mut doc = Document::new_a4("Moved Connector");
    let conn = ConnectorObject {
        from_obj: None,
        from_anchor: coree_core::AnchorPoint::Center,
        to_obj: None,
        to_anchor: coree_core::AnchorPoint::Center,
        from_fallback: Point2D::new(0.0, 0.0),
        to_fallback: Point2D::new(20.0, 0.0),
        style: ConnectorStyle::Straight,
        corner_radius_mm: 0.0,
    };
    let cid = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Link", NodePayload::Connector(conn))
                .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)))
                .with_transform(Affine2D::translate(50.0, 40.0)),
            None,
        )
        .unwrap();
    let grp = doc.group_objects(&[cid], "Moved Group").unwrap();
    doc.objects.get_mut(&grp).unwrap().transform = Affine2D::translate(10.0, 5.0);

    let plan = plan_laser_toolpath(&doc, 0, None, &LaserJobConfig::default()).unwrap();
    assert_eq!(plan.segments.len(), 1);
    let p0 = plan.segments[0].points[0];
    let p1 = *plan.segments[0].points.last().unwrap();
    // World endpoints should be (60, 45) and (80, 45) (order may be reversed by travel optimizer)
    let min_x = p0.x.min(p1.x);
    let max_x = p0.x.max(p1.x);
    assert!(
        (min_x - 60.0).abs() < 1e-4,
        "expected min_x=60, got {min_x}"
    );
    assert!(
        (max_x - 80.0).abs() < 1e-4,
        "expected max_x=80, got {max_x}"
    );
    assert!((p0.y - 45.0).abs() < 1e-4, "expected y=45, got {}", p0.y);
}

#[test]
fn regression_7_plugins_selection_only_single_pass_target_app_and_json_spoof() {
    let tmp = tempfile::tempdir().unwrap();
    let mut doc = Document::new_a4("Plugin Hardening");
    let obj_id = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                30.0,
                20.0,
                Fill::None,
                OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)),
            ),
            None,
        )
        .unwrap();

    // 7a. `selection_only: true` MUST reject `selection_ids: None` AND `Some(&[])`, succeed on `Some(&[obj_id])`
    let sel_only_plugin = PluginManifest {
        id: "sel-only-bridge".to_owned(),
        name: "Selection Only".to_owned(),
        version: "1.0.0".to_owned(),
        author: "User".to_owned(),
        description: "Requires explicit selection".to_owned(),
        category: PluginCategory::Laser,
        kind: PluginKind::ExternalBridge {
            target_app: "LightBurn".to_owned(),
            default_paths_linux: vec![],
            default_paths_windows: vec![],
            args_template: vec!["{file}".to_owned()],
            export_format: PluginExportFormat::Svg,
            selection_only: true,
            auto_dedupe: true,
            auto_join_mm: 0.05,
            auto_hatch: false,
        },
        source_dir: None,
        builtin: false,
    };
    assert!(export_for_plugin(
        &doc,
        0,
        None,
        &sel_only_plugin,
        &LaserJobConfig::default(),
        &tmp.path().join("fail_none.svg")
    )
    .is_err());
    assert!(export_for_plugin(
        &doc,
        0,
        Some(&[]),
        &sel_only_plugin,
        &LaserJobConfig::default(),
        &tmp.path().join("fail_empty.svg")
    )
    .is_err());
    assert!(export_for_plugin(
        &doc,
        0,
        Some(&[obj_id]),
        &sel_only_plugin,
        &LaserJobConfig::default(),
        &tmp.path().join("ok_selected.svg")
    )
    .is_ok());

    // 7b. Single-pass `substitute_bridge_args` never double-expands `{name}`/`{dir}`/`{stem}` in file paths
    let tricky_path = Path::new("/tmp/{name}/{stem}/part_{dir}.dxf");
    let argv = substitute_bridge_args(
        &[
            "{file}".to_owned(),
            "--dir={dir}".to_owned(),
            "--stem={stem}".to_owned(),
            "--name={name}".to_owned(),
        ],
        tricky_path,
    )
    .unwrap();
    assert_eq!(argv[0], "/tmp/{name}/{stem}/part_{dir}.dxf");
    assert_eq!(argv[1], "--dir=/tmp/{name}/{stem}");
    assert_eq!(argv[2], "--stem=part_{dir}");
    assert_eq!(argv[3], "--name=part_{dir}.dxf");

    // 7c. `target_app` with `/`, `\`, or `..` must fail validation AND never escape `PATH` in `detect_executable`
    for bad_target in ["/bin/sh", "../sh", r"C:\Windows\cmd.exe", "sub/bin"] {
        let bad_manifest = PluginManifest {
            id: "bad-target".to_owned(),
            name: "Bad Target".to_owned(),
            version: "1.0.0".to_owned(),
            author: "x".to_owned(),
            description: "x".to_owned(),
            category: PluginCategory::Laser,
            kind: PluginKind::ExternalBridge {
                target_app: bad_target.to_owned(),
                default_paths_linux: vec![],
                default_paths_windows: vec![],
                args_template: vec!["{file}".to_owned()],
                export_format: PluginExportFormat::DxfR12,
                selection_only: false,
                auto_dedupe: true,
                auto_join_mm: 0.05,
                auto_hatch: false,
            },
            source_dir: None,
            builtin: false,
        };
        assert!(
            bad_manifest.validate().is_err(),
            "target_app '{bad_target}' must be rejected"
        );
        assert_eq!(
            detect_executable(&bad_manifest, None),
            None,
            "detect_executable must never resolve path-separating target_app '{bad_target}'"
        );
    }

    // 7d. `PluginManifest::from_json_str` strips attacker-spoofed `source_dir` and `builtin: true`
    let spoofed_json = r#"{
        "id": "untrusted-plugin",
        "name": "Untrusted",
        "version": "1.0.0",
        "author": "attacker",
        "description": "tries to spoof builtin and source_dir",
        "category": "Custom",
        "source_dir": "/etc",
        "builtin": true,
        "kind": { "RhaiScript": { "script_file": "ok.rhai", "entry_label": "Run" } }
    }"#;
    let parsed = PluginManifest::from_json_str(spoofed_json).unwrap();
    assert_eq!(parsed.source_dir, None, "JSON must not set source_dir");
    assert!(!parsed.builtin, "JSON must not spoof builtin=true");
}
