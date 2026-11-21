// SPDX-License-Identifier: MPL-2.0

//! Production command-line interface (`clap`) for headless batch conversion,
//! high-DPI rendering, structural inspection, preflight, print merge,
//! collect-for-output, bitmap vectorization, and sandboxed Rhai macros.

use crate::cdr_bridge::inspect_any_file;
use crate::io_bridge::{
    export_any_document, open_any_document, read_text_file_bounded, UniversalExportOptions,
    MAX_TEXT_READ_BYTES,
};
use clap::{Parser, Subcommand, ValueEnum};
use coree_cdr::{generate_sample_document, CdrWriteVersion};
use coree_core::brand::BrandInfo;
use coree_core::math::MM_PER_INCH;
use coree_core::{ColorMode, Document, Fill, NodePayload, ObjectId, OutlineStyle, SceneObject};
use coree_io::import_raster;
use coree_prod::{
    apply_print_merge, collect_for_output, find_builtin_macro, run_macro, run_preflight,
    CollectOptions, MacroSandboxConfig, PreflightRules, PrintMergeOptions, PrintMergeTable,
};
use coree_render::{
    compute_page_layouts, render_page_to_rgba, trace_raster_to_mm, CanvasBackend, OutlinePreset,
    RenderOptions, SceneRenderer, TinySkiaBackend, TraceConfig, TraceMode, ViewMode, Viewport,
};
use coree_storage::StorageDb;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Command-line arguments for the `coree` desktop studio and headless CLI.
#[derive(Debug, Parser)]
#[command(
    name = "coree",
    version,
    about = "Clean-room vector graphics, multipage layout & print production studio (MPL-2.0)"
)]
pub struct Cli {
    /// Print build version, license, supported formats, and local font count, then exit.
    #[arg(long)]
    pub diagnostics: bool,

    /// Optional document file(s) to open directly as tabs in the desktop GUI.
    #[arg(value_name = "FILES")]
    pub files: Vec<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Supported formats for `coree generate-sample`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SampleFormatArg {
    Coree,
    #[value(name = "cdr-riff")]
    CdrRiff,
    #[value(name = "cdr-x4")]
    CdrX4,
    #[value(name = "cdr-x6")]
    CdrX6,
    Cdt,
    Cmx,
    Svg,
}

/// Target `.cdr` container version for `coree convert --cdr-version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CdrVersionArg {
    Riff,
    X4,
    X6,
}

impl From<CdrVersionArg> for CdrWriteVersion {
    fn from(value: CdrVersionArg) -> Self {
        match value {
            CdrVersionArg::Riff => Self::RiffV12,
            CdrVersionArg::X4 => Self::ZipX4,
            CdrVersionArg::X6 => Self::ZipX6,
        }
    }
}

/// Rendering fidelity mode for `coree render --view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ViewModeArg {
    Wireframe,
    Normal,
    Enhanced,
    Pixels,
}

impl From<ViewModeArg> for ViewMode {
    fn from(value: ViewModeArg) -> Self {
        match value {
            ViewModeArg::Wireframe => Self::Wireframe,
            ViewModeArg::Normal => Self::Normal,
            ViewModeArg::Enhanced => Self::Enhanced,
            ViewModeArg::Pixels => Self::Pixels,
        }
    }
}

/// Tracing mode for `coree trace --mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TraceModeArg {
    Quick,
    Outline,
    Centerline,
}

/// Subcommands available in headless CLI mode.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Generate a rich multi-element showcase document (shapes, gradients, Arabic+English text, lenses, CutContour).
    GenerateSample {
        /// Output file path.
        out_path: PathBuf,
        /// Target format (inferred from extension if omitted).
        #[arg(long, value_enum)]
        format: Option<SampleFormatArg>,
    },
    /// Structurally inspect a `.cdr`, `.cdt`, `.cmx`, `.cpt`, `.coree`, or `.svg` file.
    Inspect {
        /// Input file path.
        input_path: PathBuf,
        /// Emit structured JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Universal offline converter across `.coree`, `.cdr`, `.cdt`, `.cmx`, `.cpt`, `.svg`, `.pdf`, `.eps`, `.dxf`, `.plt`, `.png`, `.jpg`, `.webp`, `.tiff`, `.bmp`.
    Convert {
        /// Input document or image path.
        input_path: PathBuf,
        /// Output path (format inferred from extension).
        output_path: PathBuf,
        /// Target `.cdr`/`.cdt` container era when exporting binary CDR/CDT.
        #[arg(long, value_enum, default_value_t = CdrVersionArg::X6)]
        cdr_version: CdrVersionArg,
        /// Target DPI for raster outputs.
        #[arg(long, default_value_t = 300.0)]
        dpi: f32,
        /// Force DeviceCMYK color space on PDF output.
        #[arg(long)]
        cmyk: bool,
        /// Add trim crop marks on PDF output.
        #[arg(long)]
        crop_marks: bool,
    },
    /// Headless high-DPI page renderer outputting straight-RGBA PNG.
    Render {
        /// Input document path.
        input_path: PathBuf,
        /// Output PNG path.
        output_png: PathBuf,
        /// Zero-based page index to render (ignored when `--multipage` is set).
        #[arg(long, default_value_t = 0)]
        page: usize,
        /// Rendering resolution in DPI.
        #[arg(long, default_value_t = 150.0)]
        dpi: f64,
        /// Rendering view mode.
        #[arg(long, value_enum, default_value_t = ViewModeArg::Enhanced)]
        view: ViewModeArg,
        /// Render all pages side-by-side in Multipage View.
        #[arg(long)]
        multipage: bool,
    },
    /// Run print-production Preflight audit and report errors/warnings.
    Preflight {
        /// Input document path.
        input_path: PathBuf,
        /// Minimum acceptable effective raster resolution (DPI).
        #[arg(long, default_value_t = 300.0)]
        min_dpi: f64,
        /// Emit structured JSON instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// Variable-data CSV Print Merge producing a multi-page document (`.coree`, `.pdf`, `.cdr`, etc.).
    PrintMerge {
        /// Template document containing `<<Field>>` placeholders.
        template_path: PathBuf,
        /// RFC-4180 CSV data source.
        csv_path: PathBuf,
        /// Output merged document path.
        output_path: PathBuf,
    },
    /// Collect for Output: bundle document JSON, extracted raster images, font manifest, and preflight report.
    Collect {
        /// Input document path.
        input_path: PathBuf,
        /// Target output directory.
        output_dir: PathBuf,
    },
    /// Vector Trace: convert a raster image into editable Bézier vector curves.
    Trace {
        /// Input raster image (`.png`, `.jpg`, `.webp`, `.tiff`, `.bmp`).
        input_image: PathBuf,
        /// Output vector path (`.coree`, `.svg`, `.cdr`, `.dxf`, `.plt`, `.pdf`).
        output_path: PathBuf,
        /// Vectorization mode.
        #[arg(long, value_enum, default_value_t = TraceModeArg::Outline)]
        mode: TraceModeArg,
        /// Maximum quantized color regions (2..=64).
        #[arg(long, default_value_t = 8)]
        colors: usize,
    },
    /// Execute a sandboxed Rhai automation macro (file path or built-in ID such as `recolor-cmyk`).
    RunMacro {
        /// Input document path.
        input_path: PathBuf,
        /// Path to a `.rhai` file or built-in macro ID (`batch-resize`, `auto-cut-contour`, `align-grid`, `recolor-cmyk`, `clean-empty-layers`).
        script_path_or_builtin: String,
        /// Output document path after running the macro.
        output_path: PathBuf,
    },
}

/// Outcome returned by [`execute_cli`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliOutcome {
    /// Print text to stdout and exit 0.
    Printed(String),
    /// Launch the interactive desktop studio GUI with these initial files.
    LaunchGui(Vec<PathBuf>),
}

/// Executes parsed CLI arguments (either running a headless subcommand or requesting GUI launch).
pub fn execute_cli(cli: &Cli) -> Result<CliOutcome, String> {
    if cli.diagnostics {
        return Ok(CliOutcome::Printed(build_diagnostics_report()));
    }

    let Some(cmd) = &cli.command else {
        return Ok(CliOutcome::LaunchGui(cli.files.clone()));
    };

    match cmd {
        Commands::GenerateSample { out_path, format } => {
            let doc = generate_sample_document();
            let opts = UniversalExportOptions::default();
            let target_path = out_path.clone();
            if let Some(fmt) = format {
                match fmt {
                    SampleFormatArg::Coree => {
                        let preview_png = render_page_to_rgba(&doc, 0, 48.0, ViewMode::Enhanced)
                            .ok()
                            .and_then(|img| img.encode_png().ok());
                        coree_io::save_coree_with_preview(
                            &doc,
                            preview_png.as_deref(),
                            &target_path,
                        )
                        .map_err(|e| format!("failed to write .coree sample: {e}"))?;
                        return Ok(CliOutcome::Printed(format!(
                            "Generated native .coree sample at {}",
                            target_path.display()
                        )));
                    }
                    SampleFormatArg::CdrRiff => {
                        coree_cdr::export_cdr(&doc, &target_path, CdrWriteVersion::RiffV12)
                            .map_err(|e| format!("failed to write RIFF CDR sample: {e}"))?;
                        return Ok(CliOutcome::Printed(format!(
                            "Generated RIFF CDR sample at {}",
                            target_path.display()
                        )));
                    }
                    SampleFormatArg::CdrX4 => {
                        coree_cdr::export_cdr(&doc, &target_path, CdrWriteVersion::ZipX4)
                            .map_err(|e| format!("failed to write CDR X4 sample: {e}"))?;
                        return Ok(CliOutcome::Printed(format!(
                            "Generated CDR X4 ZIP sample at {}",
                            target_path.display()
                        )));
                    }
                    SampleFormatArg::CdrX6 => {
                        coree_cdr::export_cdr(&doc, &target_path, CdrWriteVersion::ZipX6)
                            .map_err(|e| format!("failed to write CDR X6 sample: {e}"))?;
                        return Ok(CliOutcome::Printed(format!(
                            "Generated CDR X6 ZIP sample at {}",
                            target_path.display()
                        )));
                    }
                    SampleFormatArg::Cdt => {
                        coree_cdr::export_cdt(&doc, &target_path, CdrWriteVersion::ZipX6)
                            .map_err(|e| format!("failed to write CDT sample: {e}"))?;
                        return Ok(CliOutcome::Printed(format!(
                            "Generated CDT template sample at {}",
                            target_path.display()
                        )));
                    }
                    SampleFormatArg::Cmx => {
                        coree_cdr::export_cmx(&doc, &target_path)
                            .map_err(|e| format!("failed to write CMX sample: {e}"))?;
                        return Ok(CliOutcome::Printed(format!(
                            "Generated CMX sample at {}",
                            target_path.display()
                        )));
                    }
                    SampleFormatArg::Svg => {
                        coree_io::export_svg(&doc, 0, &target_path)
                            .map_err(|e| format!("failed to write SVG sample: {e}"))?;
                        return Ok(CliOutcome::Printed(format!(
                            "Generated SVG sample at {}",
                            target_path.display()
                        )));
                    }
                }
            }
            let msg = export_any_document(&doc, &target_path, &opts)?;
            Ok(CliOutcome::Printed(msg))
        }
        Commands::Inspect { input_path, json } => {
            let report = inspect_any_file(input_path)?;
            if *json {
                let s = serde_json::to_string_pretty(&report)
                    .map_err(|e| format!("JSON serialization failed: {e}"))?;
                Ok(CliOutcome::Printed(s))
            } else {
                Ok(CliOutcome::Printed(report.format_human()))
            }
        }
        Commands::Convert {
            input_path,
            output_path,
            cdr_version,
            dpi,
            cmyk,
            crop_marks,
        } => {
            if !dpi.is_finite() || *dpi <= 0.0 {
                return Err(format!("invalid DPI value: {dpi} (must be finite and > 0)"));
            }
            let opened = open_any_document(input_path)?;
            let opts = UniversalExportOptions {
                cdr_version: (*cdr_version).into(),
                dpi: *dpi,
                force_cmyk: *cmyk,
                crop_marks: *crop_marks,
                ..UniversalExportOptions::default()
            };
            let msg = export_any_document(&opened.document, output_path, &opts)?;
            Ok(CliOutcome::Printed(msg))
        }
        Commands::Render {
            input_path,
            output_png,
            page,
            dpi,
            view,
            multipage,
        } => {
            if !dpi.is_finite() || *dpi <= 0.0 {
                return Err(format!("invalid DPI value: {dpi} (must be finite and > 0)"));
            }
            let opened = open_any_document(input_path)?;
            let view_mode: ViewMode = (*view).into();
            if *multipage {
                let mut doc = opened.document;
                doc.multipage_view = true;
                let safe_dpi = dpi.clamp(1.0, 2400.0);
                let opts = RenderOptions::export();
                let layouts = compute_page_layouts(&doc, true, opts.multipage_gap_mm);
                let total_w_mm = layouts
                    .iter()
                    .map(|l| l.origin_mm.x + l.width_mm)
                    .fold(10.0_f64, f64::max);
                let total_h_mm = layouts
                    .iter()
                    .map(|l| l.origin_mm.y + l.height_mm)
                    .fold(10.0_f64, f64::max);
                // Scale effective DPI down cleanly if total canvas exceeds 16384px so trailing pages are never cropped
                let max_dim_px = 16_384.0_f64;
                let max_dpi_w = max_dim_px * MM_PER_INCH / total_w_mm.max(1.0);
                let max_dpi_h = max_dim_px * MM_PER_INCH / total_h_mm.max(1.0);
                let effective_dpi = safe_dpi.min(max_dpi_w).min(max_dpi_h).max(1.0);
                let px_per_mm = effective_dpi / MM_PER_INCH;
                let width = (total_w_mm * px_per_mm).ceil().clamp(1.0, max_dim_px) as u32;
                let height = (total_h_mm * px_per_mm).ceil().clamp(1.0, max_dim_px) as u32;
                let mut backend = TinySkiaBackend::new(width, height)
                    .map_err(|e| format!("multipage surface failed: {e}"))?;
                let mut vp = Viewport::for_export(width, height, effective_dpi);
                vp.multipage_view = true;
                let mut renderer = SceneRenderer::new();
                renderer.render_document(&doc, &vp, view_mode, &opts, &mut backend);
                let rgba = backend.into_rgba();
                coree_io::export_raster(
                    rgba.width,
                    rgba.height,
                    &rgba.rgba,
                    &coree_io::RasterExportOptions::new(
                        coree_io::RasterFormat::Png,
                        effective_dpi as f32,
                    ),
                    output_png,
                )
                .map_err(|e| format!("failed to write multipage PNG: {e}"))?;
                Ok(CliOutcome::Printed(format!(
                    "Rendered multipage ({}x{} @ {effective_dpi:.1} DPI) to {}",
                    rgba.width,
                    rgba.height,
                    output_png.display()
                )))
            } else {
                let rendered = render_page_to_rgba(&opened.document, *page, *dpi, view_mode)
                    .map_err(|e| format!("render failed: {e}"))?;
                coree_io::export_raster(
                    rendered.width,
                    rendered.height,
                    &rendered.rgba,
                    &coree_io::RasterExportOptions::new(
                        coree_io::RasterFormat::Png,
                        rendered.dpi as f32,
                    ),
                    output_png,
                )
                .map_err(|e| format!("failed to write PNG: {e}"))?;
                Ok(CliOutcome::Printed(format!(
                    "Rendered page {} ({}x{} @ {} DPI, {:?}) to {}",
                    page,
                    rendered.width,
                    rendered.height,
                    rendered.dpi,
                    view_mode,
                    output_png.display()
                )))
            }
        }
        Commands::Preflight {
            input_path,
            min_dpi,
            json,
        } => {
            let opened = open_any_document(input_path)?;
            let rules = PreflightRules {
                min_dpi: *min_dpi,
                ..PreflightRules::default()
            };
            let report = run_preflight(&opened.document, &rules);
            if *json {
                let s = serde_json::to_string_pretty(&report)
                    .map_err(|e| format!("preflight JSON failed: {e}"))?;
                Ok(CliOutcome::Printed(s))
            } else {
                Ok(CliOutcome::Printed(report.to_plain_text()))
            }
        }
        Commands::PrintMerge {
            template_path,
            csv_path,
            output_path,
        } => {
            let opened = open_any_document(template_path)?;
            let csv_text = read_text_file_bounded(csv_path, MAX_TEXT_READ_BYTES)
                .map_err(|e| format!("failed to read CSV: {e}"))?;
            let table = PrintMergeTable::from_csv(&csv_text)
                .map_err(|e| format!("CSV parse failed: {e}"))?;
            let merged = apply_print_merge(&opened.document, &table, &PrintMergeOptions::default())
                .map_err(|e| format!("print merge failed: {e}"))?;
            let msg =
                export_any_document(&merged, output_path, &UniversalExportOptions::default())?;
            Ok(CliOutcome::Printed(format!(
                "Merged {} record(s) into {} page(s). {msg}",
                table.rows.len(),
                merged.pages.len()
            )))
        }
        Commands::Collect {
            input_path,
            output_dir,
        } => {
            let opened = open_any_document(input_path)?;
            let report =
                collect_for_output(&opened.document, output_dir, &CollectOptions::default())
                    .map_err(|e| format!("collect for output failed: {e}"))?;
            Ok(CliOutcome::Printed(format!(
                "Collected {} file(s) ({} image(s), {} font(s)) into {}",
                report.total_files_written,
                report.extracted_images.len(),
                report.used_fonts.len(),
                report.output_dir.display()
            )))
        }
        Commands::Trace {
            input_image,
            output_path,
            mode,
            colors,
        } => {
            let asset = import_raster(input_image)
                .map_err(|e| format!("failed to load raster '{}': {e}", input_image.display()))?;
            let dpi = f64::from(asset.source_dpi.max(1.0));
            let w_mm = (f64::from(asset.width) / dpi * MM_PER_INCH).max(10.0);
            let h_mm = (f64::from(asset.height) / dpi * MM_PER_INCH).max(10.0);
            let trace_mode = match mode {
                TraceModeArg::Quick => TraceMode::QuickTrace,
                TraceModeArg::Outline => TraceMode::OutlineTrace(OutlinePreset::Logo),
                TraceModeArg::Centerline => TraceMode::CenterlineTrace,
            };
            let cfg = TraceConfig {
                mode: trace_mode,
                max_colors: (*colors).clamp(2, 64),
                ..TraceConfig::default()
            };
            let traced = trace_raster_to_mm(&asset, w_mm, h_mm, &cfg);
            let stem = input_image
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Traced");
            let mut doc = Document::new(stem, w_mm, h_mm, ColorMode::Cmyk, 300.0);
            let is_centerline = matches!(trace_mode, TraceMode::CenterlineTrace);
            let count = traced.len();
            for (idx, (path, color)) in traced.into_iter().enumerate() {
                let mut obj = SceneObject::new(
                    ObjectId(0),
                    format!("Traced Curve {}", idx + 1),
                    NodePayload::Path(path),
                );
                if is_centerline {
                    obj.fill = Fill::None;
                    obj.outline = OutlineStyle::solid(color, 0.25);
                } else {
                    obj.fill = Fill::Solid(color);
                    obj.outline = OutlineStyle::none();
                }
                let _ = doc.add_object(obj, None);
            }
            let msg = export_any_document(&doc, output_path, &UniversalExportOptions::default())?;
            Ok(CliOutcome::Printed(format!(
                "Traced {count} vector curve(s) ({mode:?}). {msg}"
            )))
        }
        Commands::RunMacro {
            input_path,
            script_path_or_builtin,
            output_path,
        } => {
            let mut opened = open_any_document(input_path)?;
            let script = if let Some(builtin) = find_builtin_macro(script_path_or_builtin) {
                builtin.script.to_owned()
            } else {
                read_text_file_bounded(
                    std::path::Path::new(script_path_or_builtin),
                    MAX_TEXT_READ_BYTES,
                )
                .map_err(|e| {
                    format!("macro '{script_path_or_builtin}' is neither a built-in macro ID nor a readable file: {e}")
                })?
            };
            let exec = run_macro(
                &mut opened.document,
                &script,
                &BTreeMap::new(),
                &MacroSandboxConfig::default(),
            )
            .map_err(|e| format!("macro execution failed: {e}"))?;
            let msg = export_any_document(
                &opened.document,
                output_path,
                &UniversalExportOptions::default(),
            )?;
            let mut out = format!(
                "Macro executed ({} command(s)). {msg}",
                exec.commands_executed
            );
            for log in exec.logs {
                out.push('\n');
                out.push_str(&format!("  [macro] {log}"));
            }
            Ok(CliOutcome::Printed(out))
        }
    }
}

/// Builds the `--diagnostics` summary string with version, license, formats, crates, and local font count.
#[must_use]
pub fn build_diagnostics_report() -> String {
    let brand = BrandInfo::current();
    let font_count = discover_local_font_count();
    format!(
        "{} v{} ({})\n\
         Repository: {}\n\
         Workspace Crates: {}, {}, {}, {}, {}, {}\n\
         Supported Import Formats: .coree, .cdr (RIFF v4-v13 / X4-X5 / X6-2025), .cdt, .cmx (16/32-bit), .cpt, .svg, .svgz, .png, .jpg, .webp, .tiff, .bmp, .gpl, .ase, .pal\n\
         Supported Export Formats: .coree, .cdr (RIFF/X4/X6), .cdt, .cmx, .svg, .pdf (CMYK/RGB + print marks), .eps, .dxf, .plt (HPGL), .png, .jpg, .webp, .tiff, .bmp\n\
         Local System Fonts Indexed: {}\n\
         Mode: 100% Offline Pure-Rust (#![forbid(unsafe_code)])",
        brand.display_name,
        brand.version,
        brand.license,
        brand.repo_url,
        coree_core::CRATE_NAME,
        coree_cdr::CRATE_NAME,
        coree_io::CRATE_NAME,
        coree_storage::CRATE_NAME,
        coree_render::CRATE_NAME,
        coree_prod::CRATE_NAME,
        font_count
    )
}

fn discover_local_font_count() -> usize {
    if let Ok(db) = StorageDb::open_in_memory() {
        if let Ok(report) = db.scan_system_fonts() {
            return report.faces_indexed;
        }
    }
    0
}
