// SPDX-License-Identifier: MIT OR Apache-2.0

//! Production command-line interface (`clap`) for headless batch conversion,
//! high-DPI rendering, OS thumbnail generation, laser/CNC toolpath planning &
//! plugin bridges, structural inspection, preflight, print merge,
//! collect-for-output, bitmap vectorization, and sandboxed Rhai macros.

use crate::cdr_bridge::{
    extract_file_thumbnail_png, format_inspection_hex_dumps, inspect_any_file, read_png_dimensions,
    MAX_THUMBNAIL_SIZE_PX,
};
use crate::io_bridge::{
    export_any_document, open_any_document, open_any_document_with_codepage,
    read_text_file_bounded, UniversalExportOptions, MAX_TEXT_READ_BYTES,
};
use clap::{Parser, Subcommand, ValueEnum};
use coree_cdr::{generate_sample_document, CdrWriteVersion};
use coree_core::brand::BrandInfo;
use coree_core::math::MM_PER_INCH;
use coree_core::{ColorMode, Document, Fill, NodePayload, ObjectId, OutlineStyle, SceneObject};
use coree_io::{atomic_write_bytes, import_raster};
use coree_prod::{
    apply_print_merge, collect_for_output, default_plugin_dirs, detect_executable,
    evaluate_cql_query, export_for_plugin, export_laser_dxf_r12, export_laser_eps,
    export_laser_gcode, export_laser_plt, export_laser_svg, find_builtin_macro,
    launch_external_bridge, plan_laser_toolpath, run_macro, run_preflight,
    stream_print_merge_csv_to_pdf, CollectOptions, FindScope, GcodeDialect, LaserJobConfig,
    LaserOperationKind, LaserToolpathPlan, MacroSandboxConfig, PluginKind, PluginRegistry,
    PreflightRules, PrintMergeOptions, PrintMergeTable,
};
use coree_render::{
    compute_page_layouts, render_page_to_rgba, render_page_to_rgba_with_backend,
    trace_raster_to_mm, CanvasBackend, OutlinePreset, RenderBackend, RenderBackendKind,
    RenderOptions, SceneRenderer, TraceConfig, TraceMode, ViewMode, Viewport,
};
use coree_storage::StorageDb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Command-line arguments for the `coree` desktop studio and headless CLI.
#[derive(Debug, Parser)]
#[command(
    name = "coree",
    version,
    args_conflicts_with_subcommands = true,
    about = "Clean-room vector graphics, multipage layout, laser & print production studio (MIT OR Apache-2.0)"
)]
pub struct Cli {
    /// Print build version, license, supported formats, built-in plugins, and local font count, then exit.
    #[arg(long)]
    pub diagnostics: bool,

    /// Launch an independent studio window instead of forwarding files to the running instance.
    #[arg(long)]
    pub new_window: bool,

    /// Optional document file(s) to open directly as tabs in the desktop GUI (supports `Exec=coree %F` and Windows file associations).
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
    #[value(name = "simple-wireframe")]
    SimpleWireframe,
    Wireframe,
    Normal,
    Enhanced,
    Pixels,
}

impl From<ViewModeArg> for ViewMode {
    fn from(value: ViewModeArg) -> Self {
        match value {
            ViewModeArg::SimpleWireframe => Self::SimpleWireframe,
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

/// Outline vectorization preset for `coree trace --preset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutlinePresetArg {
    Logo,
    LineArt,
    DetailedLogo,
    Clipart,
    LowQuality,
    HighQuality,
}

impl From<OutlinePresetArg> for OutlinePreset {
    fn from(value: OutlinePresetArg) -> Self {
        match value {
            OutlinePresetArg::Logo => Self::Logo,
            OutlinePresetArg::LineArt => Self::LineArt,
            OutlinePresetArg::DetailedLogo => Self::DetailedLogo,
            OutlinePresetArg::Clipart => Self::Clipart,
            OutlinePresetArg::LowQuality => Self::LowQualityImage,
            OutlinePresetArg::HighQuality => Self::HighQualityImage,
        }
    }
}

/// Output format for `coree laser --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LaserFormatArg {
    Gcode,
    Dxf,
    Plt,
    Svg,
    Eps,
}

impl LaserFormatArg {
    fn infer_from_path(path: &Path) -> Result<Self, String> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match ext.as_str() {
            "gcode" | "nc" => Ok(Self::Gcode),
            "dxf" => Ok(Self::Dxf),
            "plt" | "hpgl" => Ok(Self::Plt),
            "svg" => Ok(Self::Svg),
            "eps" | "ps" => Ok(Self::Eps),
            "" => Ok(Self::Gcode),
            other => Err(format!(
                "unrecognized laser output extension '.{other}' (use --format gcode|dxf|plt|svg|eps)"
            )),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Gcode => "G-Code",
            Self::Dxf => "DXF R12",
            Self::Plt => "HPGL PLT",
            Self::Svg => "Laser SVG",
            Self::Eps => "Laser EPS",
        }
    }
}

/// Target CNC/laser G-Code firmware dialect for `coree laser --dialect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GcodeDialectArg {
    Grbl,
    Marlin,
    Ruida,
}

/// 2D canvas rendering backend for `coree-cli render --backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum RenderBackendArg {
    #[default]
    Auto,
    #[value(alias = "cpu")]
    Tinyskia,
    #[value(alias = "gpu")]
    Wgpu,
}

impl From<RenderBackendArg> for RenderBackendKind {
    fn from(arg: RenderBackendArg) -> Self {
        match arg {
            RenderBackendArg::Auto => Self::from_env(),
            RenderBackendArg::Tinyskia => Self::TinySkia,
            RenderBackendArg::Wgpu => Self::Wgpu,
        }
    }
}

impl From<GcodeDialectArg> for GcodeDialect {
    fn from(value: GcodeDialectArg) -> Self {
        match value {
            GcodeDialectArg::Grbl => Self::Grbl11,
            GcodeDialectArg::Marlin => Self::Marlin,
            GcodeDialectArg::Ruida => Self::RuidaGcode,
        }
    }
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
    /// Structurally inspect any `.cdr`, `.cdr.zip`, `.cdt`, `.cmx`, `.cpt`, `.coree`, `.svg`, `.pdf`, `.ai`, `.eps`, `.dxf`, `.dwg`, `.wmf`, `.emf`, `.plt`, `.psd`, or `.acb` file.
    Inspect {
        /// Input file path.
        input_path: PathBuf,
        /// Emit structured JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
        /// Include bounded Hex/ASCII dumps for RIFF chunks (`S16`).
        #[arg(long)]
        hex: bool,
    },
    /// Universal offline converter across `.coree`, `.cdr`, `.cdr.zip`, `.cdt`, `.cmx`, `.cpt`, `.svg`, `.pdf`, `.ai`, `.eps`, `.dxf`, `.dwg`, `.wmf`, `.emf`, `.plt`, `.psd`, `.gcode`, `.png`, `.jpg`, `.webp`, `.tiff`, `.bmp` (note: `.acb` swatch palettes use `inspect` / Palette Import).
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
        /// Fallback legacy `.cdr` codepage (`auto`, `cp1256`, `cp1251`, `utf8`, `cp1252`).
        #[arg(long)]
        codepage: Option<String>,
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
        /// 2D rasterization backend (`auto`, `tinyskia`/`cpu`, or `wgpu`/`gpu`).
        #[arg(long, value_enum, default_value_t = RenderBackendArg::Auto)]
        backend: RenderBackendArg,
        /// Optional monitor / display RGB ICC profile path (`.icc` / `.icm`).
        #[arg(long)]
        display_icc: Option<PathBuf>,
    },
    /// Fast, bounded OS thumbnail generator (used by desktop file managers for `.cdr`, `.coree`, `.svg`, `.pdf`, etc.).
    Thumbnail {
        /// Input document or image path.
        input_path: PathBuf,
        /// Output PNG thumbnail path (written atomically).
        output_png: PathBuf,
        /// Maximum width/height in pixels (`1..=4096`).
        #[arg(long, default_value_t = 256)]
        size: u32,
    },
    /// Laser cutting, engraving & plotter toolpath planner + machine exporter (`G-Code`, `DXF R12`, `PLT`, `SVG`, `EPS`) and external plugin bridge.
    Laser {
        /// Input vector document path (`.coree`, `.cdr`, `.cdr.zip`, `.svg`, `.pdf`, `.ai`, `.eps`, `.dxf`, `.plt`).
        input_path: PathBuf,
        /// Output laser/CNC file path.
        output_path: PathBuf,
        /// Target laser export format (inferred from `output_path` extension when omitted).
        #[arg(long, value_enum)]
        format: Option<LaserFormatArg>,
        /// Target G-Code controller firmware dialect.
        #[arg(long, value_enum, default_value_t = GcodeDialectArg::Grbl)]
        dialect: GcodeDialectArg,
        /// Zero-based page index to plan.
        #[arg(long, default_value_t = 0)]
        page: usize,
        /// Duplicate overlapping segment removal tolerance in mm (`0.0` disables).
        #[arg(long, default_value_t = 0.05)]
        dedupe: f64,
        /// Open-curve endpoint stitching tolerance in mm (`0.0` disables).
        #[arg(long, default_value_t = 0.05)]
        join: f64,
        /// Geometric kerf compensation offset in mm (positive = outward, negative = inward).
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        kerf: f64,
        /// Override hatch engraving line spacing in mm (`0.0` keeps rule default).
        #[arg(long, default_value_t = 0.0)]
        hatch_spacing: f64,
        /// Hatch engraving scanline angle in degrees.
        #[arg(long, default_value_t = 45.0, allow_hyphen_values = true)]
        hatch_angle: f64,
        /// Enable 90-degree cross-hatch engraving pass.
        #[arg(long)]
        cross_hatch: bool,
        /// Add an outer vinyl/laser weeding box around the planned job bounds.
        #[arg(long)]
        weeding_box: bool,
        /// Export via a built-in or user-installed bridge plugin ID (e.g., `lightburn-bridge`, `rdworks-ruida`, `ezcad-fiber`, `lasergrbl-bridge`).
        #[arg(long)]
        plugin: Option<String>,
        /// After exporting with `--plugin`, launch the detected external bridge application safely (no shell invocation).
        #[arg(long)]
        launch: bool,
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
        /// Outline preset (`logo`, `line-art`, `detailed-logo`, `clipart`, `low-quality`, `high-quality`).
        #[arg(long, value_enum, default_value_t = OutlinePresetArg::Logo)]
        preset: OutlinePresetArg,
        /// Maximum quantized color regions (2..=64).
        #[arg(long, default_value_t = 8)]
        colors: usize,
        /// Detail preservation factor (0..=100).
        #[arg(long, default_value_t = 75.0)]
        detail: f32,
        /// Curve smoothing strength (0..=100).
        #[arg(long, default_value_t = 25.0)]
        smoothing: f32,
        /// Corner sharpness threshold (0..=100).
        #[arg(long, default_value_t = 20.0)]
        corner_threshold: f32,
        /// Remove solid border background color during trace.
        #[arg(long)]
        remove_background: bool,
        /// Minimum contiguous region pixel area before merging speckles into neighbors.
        #[arg(long, default_value_t = 4)]
        speckle_size: usize,
    },
    /// Query document objects using `coree` CQL (`@type == 'curve' and @color == 'CutContour'`).
    Cql {
        /// Input document path.
        input_path: PathBuf,
        /// CQL query string via `--query`.
        #[arg(long, conflicts_with = "query_pos")]
        query: Option<String>,
        /// Optional positional CQL query string.
        #[arg(value_name = "QUERY")]
        query_pos: Option<String>,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
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
    /// Convert all live shapes, text, B-Splines, artistic media, and clones in a document to vector curves.
    ConvertToCurves {
        /// Input document path (`.coree`, `.cdr`, `.svg`, etc.).
        input_path: PathBuf,
        /// Output document path (format inferred from extension).
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
        let normalized = cli
            .files
            .iter()
            .map(|p| normalize_desktop_file_arg(p))
            .collect();
        return Ok(CliOutcome::LaunchGui(normalized));
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
        Commands::Inspect {
            input_path,
            json,
            hex,
        } => {
            let report = inspect_any_file(input_path)?;
            if *json {
                let s = serde_json::to_string_pretty(&report)
                    .map_err(|e| format!("JSON serialization failed: {e}"))?;
                Ok(CliOutcome::Printed(s))
            } else {
                let mut out = report.format_human();
                if *hex {
                    let dump = format_inspection_hex_dumps(&report);
                    if !dump.is_empty() {
                        out.push_str("\n--- Chunk Hex / ASCII Dumps ---\n");
                        out.push_str(&dump);
                    }
                }
                Ok(CliOutcome::Printed(out))
            }
        }
        Commands::Convert {
            input_path,
            output_path,
            cdr_version,
            dpi,
            cmyk,
            crop_marks,
            codepage,
        } => {
            if !dpi.is_finite() || *dpi <= 0.0 {
                return Err(format!("invalid DPI value: {dpi} (must be finite and > 0)"));
            }
            let cp = parse_cli_codepage(codepage.as_deref())?;
            let opened = open_any_document_with_codepage(input_path, cp)?;
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
            backend,
            display_icc,
        } => {
            if !dpi.is_finite() || *dpi <= 0.0 {
                return Err(format!("invalid DPI value: {dpi} (must be finite and > 0)"));
            }
            let opened = open_any_document(input_path)?;
            let view_mode: ViewMode = (*view).into();
            let backend_kind: RenderBackendKind = (*backend).into();
            let display_icc_bytes = if let Some(ref icc_path) = display_icc {
                let info = crate::display_icc::load_display_icc_from_path(icc_path, "cli")
                    .ok_or_else(|| {
                        format!("invalid display ICC profile: {}", icc_path.display())
                    })?;
                Some(info.bytes)
            } else {
                None
            };
            let mut opts = RenderOptions::export();
            opts.display_icc_profile = display_icc_bytes;

            if *multipage {
                let mut doc = opened.document;
                doc.set_multipage_view(true);
                let safe_dpi = dpi.clamp(1.0, 2400.0);
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
                let mut surface = RenderBackend::new_for_kind(backend_kind, width, height)
                    .map_err(|e| format!("multipage surface failed: {e}"))?;
                let mut vp = Viewport::for_export(width, height, effective_dpi);
                vp.multipage_view = true;
                let mut renderer = SceneRenderer::new();
                renderer.render_document(&doc, &vp, view_mode, &opts, &mut surface);
                let rgba = surface.into_rgba();
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
                let rendered = render_page_to_rgba_with_backend(
                    &opened.document,
                    *page,
                    *dpi,
                    view_mode,
                    &opts,
                    backend_kind,
                )
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
        Commands::Thumbnail {
            input_path,
            output_png,
            size,
        } => {
            if *size == 0 || *size > MAX_THUMBNAIL_SIZE_PX {
                return Err(format!(
                    "invalid thumbnail --size {size} (must be between 1 and {MAX_THUMBNAIL_SIZE_PX})"
                ));
            }
            let normalized_input = normalize_desktop_file_arg(input_path);
            let png_bytes = extract_file_thumbnail_png(&normalized_input, *size)?;
            atomic_write_bytes(output_png, &png_bytes)
                .map_err(|e| format!("failed to write thumbnail PNG: {e}"))?;
            let (w, h) = read_png_dimensions(&png_bytes).unwrap_or((*size, *size));
            Ok(CliOutcome::Printed(format!(
                "Generated thumbnail ({w}x{h}, {} bytes) at {}",
                png_bytes.len(),
                output_png.display()
            )))
        }
        Commands::Laser {
            input_path,
            output_path,
            format,
            dialect,
            page,
            dedupe,
            join,
            kerf,
            hatch_spacing,
            hatch_angle,
            cross_hatch,
            weeding_box,
            plugin,
            launch,
        } => execute_laser_command(
            input_path,
            output_path,
            *format,
            *dialect,
            *page,
            *dedupe,
            *join,
            *kerf,
            *hatch_spacing,
            *hatch_angle,
            *cross_hatch,
            *weeding_box,
            plugin.as_deref(),
            *launch,
        ),
        Commands::Preflight {
            input_path,
            min_dpi,
            json,
        } => {
            if !min_dpi.is_finite() || *min_dpi <= 0.0 {
                return Err(format!(
                    "invalid --min-dpi value: {min_dpi} (must be finite and > 0)"
                ));
            }
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
            let is_pdf = output_path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
            if is_pdf {
                let row_count = stream_print_merge_csv_to_pdf(
                    &opened.document,
                    csv_path,
                    output_path,
                    &PrintMergeOptions::default(),
                )
                .map_err(|e| format!("streaming print merge to PDF failed: {e}"))?;
                Ok(CliOutcome::Printed(format!(
                    "Streamed {row_count} record(s) into {} page(s) at {}",
                    row_count,
                    output_path.display()
                )))
            } else {
                let csv_text = read_text_file_bounded(csv_path, MAX_TEXT_READ_BYTES)
                    .map_err(|e| format!("failed to read CSV: {e}"))?;
                let table = PrintMergeTable::from_csv(&csv_text)
                    .map_err(|e| format!("CSV parse failed: {e}"))?;
                let merged =
                    apply_print_merge(&opened.document, &table, &PrintMergeOptions::default())
                        .map_err(|e| format!("print merge failed: {e}"))?;
                let msg =
                    export_any_document(&merged, output_path, &UniversalExportOptions::default())?;
                Ok(CliOutcome::Printed(format!(
                    "Merged {} record(s) into {} page(s). {msg}",
                    table.rows.len(),
                    merged.pages().len()
                )))
            }
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
            preset,
            colors,
            detail,
            smoothing,
            corner_threshold,
            remove_background,
            speckle_size,
        } => {
            let asset_raw = import_raster(input_image)
                .map_err(|e| format!("failed to load raster '{}': {e}", input_image.display()))?;
            let dpi = f64::from(asset_raw.source_dpi.max(1.0));
            let w_mm = (f64::from(asset_raw.width) / dpi * MM_PER_INCH).max(10.0);
            let h_mm = (f64::from(asset_raw.height) / dpi * MM_PER_INCH).max(10.0);
            // Guard against >16MP memory spikes during trace while preserving physical document size
            let px_count = u64::from(asset_raw.width) * u64::from(asset_raw.height);
            let asset = if px_count > 4_000_000 {
                let max_dim = 2048.0_f64;
                let scale = max_dim / f64::from(asset_raw.width.max(asset_raw.height).max(1));
                let nw = (f64::from(asset_raw.width) * scale).round().max(1.0) as u32;
                let nh = (f64::from(asset_raw.height) * scale).round().max(1.0) as u32;
                coree_render::resample_with_algorithm(
                    &asset_raw,
                    nw,
                    nh,
                    coree_render::ResampleAlgorithm::Bilinear,
                )
                .map_err(|e| format!("trace pre-resample failed: {e}"))?
            } else {
                asset_raw
            };
            let trace_mode = match mode {
                TraceModeArg::Quick => TraceMode::QuickTrace,
                TraceModeArg::Outline => TraceMode::OutlineTrace((*preset).into()),
                TraceModeArg::Centerline => TraceMode::CenterlineTrace,
            };
            let cfg = TraceConfig {
                mode: trace_mode,
                max_colors: (*colors).clamp(2, 64),
                detail: if *detail > 1.0 {
                    f64::from(*detail) / 100.0
                } else {
                    f64::from(*detail)
                }
                .clamp(0.0, 1.0),
                smoothing: if *smoothing > 1.0 {
                    f64::from(*smoothing) / 100.0
                } else {
                    f64::from(*smoothing)
                }
                .clamp(0.0, 1.0),
                corner_threshold: f64::from(*corner_threshold).clamp(0.0, 180.0),
                remove_background: *remove_background,
                speckle_size: (*speckle_size).min(4096),
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
        Commands::Cql {
            input_path,
            query,
            query_pos,
            json,
        } => {
            let q = query
                .as_deref()
                .or(query_pos.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "missing CQL query (pass `--query '...'` or positional QUERY)".to_owned()
                })?;
            let opened = open_any_document(input_path)?;
            let matched = evaluate_cql_query(&opened.document, FindScope::WholeDocument, q)
                .map_err(|e| format!("CQL query error: {e}"))?;
            if *json {
                let items: Vec<serde_json::Value> = matched
                    .iter()
                    .map(|oid| {
                        let name = opened
                            .document
                            .object(*oid)
                            .map(|o| o.name.as_str())
                            .unwrap_or("");
                        serde_json::json!({ "id": oid.0, "name": name })
                    })
                    .collect();
                let payload = serde_json::json!({
                    "query": q,
                    "count": items.len(),
                    "matches": items,
                });
                let s = serde_json::to_string_pretty(&payload)
                    .map_err(|e| format!("CQL JSON failed: {e}"))?;
                Ok(CliOutcome::Printed(s))
            } else if matched.is_empty() {
                Ok(CliOutcome::Printed("0 objects matched.".to_owned()))
            } else {
                let mut lines = Vec::with_capacity(matched.len());
                for oid in matched {
                    let name = opened
                        .document
                        .object(oid)
                        .map(|o| o.name.as_str())
                        .unwrap_or("Object");
                    lines.push(format!("{}: {name}", oid.0));
                }
                Ok(CliOutcome::Printed(lines.join("\n")))
            }
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
        Commands::ConvertToCurves {
            input_path,
            output_path,
        } => {
            let mut doc = open_any_document(input_path)?.document;
            let mut ids: Vec<ObjectId> = doc.objects().keys().copied().collect();
            ids.sort_by_key(|id| {
                let obj = doc.object(*id);
                let is_clone = obj.is_some_and(|o| matches!(o.payload, NodePayload::Clone { .. }));
                let is_top = obj.is_some_and(|o| matches!(o.payload, NodePayload::TextOnPath(_)));
                (is_clone, !is_top, id.0)
            });
            let mut text_engine = coree_render::TextEngine::new();
            let mut converted = 0usize;
            for id in ids {
                let mut outliner = |obj: &SceneObject, d: &Document| {
                    crate::state::outline_text_object_with_engine(&mut text_engine, obj, d)
                };
                if doc
                    .convert_to_curves_with_outliner(id, &mut outliner)
                    .is_ok()
                {
                    converted += 1;
                }
            }
            let msg = export_any_document(&doc, output_path, &UniversalExportOptions::default())?;
            Ok(CliOutcome::Printed(format!(
                "Converted {converted} object(s) to curves. {msg}"
            )))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_laser_command(
    input_path: &Path,
    output_path: &Path,
    format: Option<LaserFormatArg>,
    dialect: GcodeDialectArg,
    page: usize,
    dedupe: f64,
    join: f64,
    kerf: f64,
    hatch_spacing: f64,
    hatch_angle: f64,
    cross_hatch: bool,
    weeding_box: bool,
    plugin_id: Option<&str>,
    launch: bool,
) -> Result<CliOutcome, String> {
    if !dedupe.is_finite() || dedupe < 0.0 {
        return Err(format!(
            "invalid --dedupe tolerance: {dedupe} (must be finite and >= 0)"
        ));
    }
    if !join.is_finite() || join < 0.0 {
        return Err(format!(
            "invalid --join tolerance: {join} (must be finite and >= 0)"
        ));
    }
    if !kerf.is_finite() || kerf.abs() > 50.0 {
        return Err(format!(
            "invalid --kerf offset: {kerf} (must be finite and within -50..=50 mm)"
        ));
    }
    if !hatch_spacing.is_finite() || hatch_spacing < 0.0 {
        return Err(format!(
            "invalid --hatch-spacing: {hatch_spacing} (must be finite and >= 0)"
        ));
    }
    if !hatch_angle.is_finite() {
        return Err(format!(
            "invalid --hatch-angle: {hatch_angle} (must be finite)"
        ));
    }
    if launch && plugin_id.is_none() {
        return Err("--launch requires --plugin <plugin_id>".to_owned());
    }

    let opened = open_any_document(input_path)?;

    let mut cfg = LaserJobConfig {
        dialect: dialect.into(),
        dedupe_tolerance_mm: dedupe,
        join_tolerance_mm: join,
        add_weeding_box: weeding_box,
        ..LaserJobConfig::default()
    };
    for rule in &mut cfg.rules {
        if kerf != 0.0 && rule.operation != LaserOperationKind::Ignore {
            rule.kerf_mm = kerf;
        }
        if rule.operation == LaserOperationKind::HatchEngrave {
            if hatch_spacing > 0.0 {
                rule.hatch_spacing_mm = hatch_spacing;
                rule.enabled = true;
            }
            rule.hatch_angle_deg = hatch_angle;
            if cross_hatch {
                rule.cross_hatch = true;
            }
        }
    }

    if let Some(pid) = plugin_id {
        let mut registry = PluginRegistry::with_builtins();
        registry.load_discovered(&default_plugin_dirs());
        let manifest = registry.find(pid).ok_or_else(|| {
            let ids: Vec<&str> = registry.plugins().iter().map(|p| p.id.as_str()).collect();
            format!("unknown plugin '{pid}' (available: {})", ids.join(", "))
        })?;
        let outcome = export_for_plugin(&opened.document, page, None, manifest, &cfg, output_path)
            .map_err(|e| format!("plugin export '{pid}' failed: {e}"))?;

        let mut summary = format_laser_metrics(
            &format!("Plugin '{}' ({:?})", manifest.name, outcome.format),
            &outcome.plan,
            outcome.bytes_written,
            &outcome.output_path,
        );

        if launch {
            let PluginKind::ExternalBridge {
                ref args_template,
                ref target_app,
                ..
            } = manifest.kind
            else {
                return Err(format!(
                    "plugin '{}' is not an external bridge application",
                    manifest.id
                ));
            };
            let exe = detect_executable(manifest, None).ok_or_else(|| {
                format!(
                    "exported to {}, but external bridge executable '{}' was not found on this system",
                    outcome.output_path.display(),
                    target_app
                )
            })?;
            let child_pid = launch_external_bridge(&exe, args_template, &outcome.output_path)
                .map_err(|e| format!("failed to launch '{}': {e}", exe.display()))?;
            summary.push_str(&format!(
                "\nLaunched bridge '{}' (PID {child_pid})",
                exe.display()
            ));
        }

        return Ok(CliOutcome::Printed(summary));
    }

    let target_fmt = match format {
        Some(f) => f,
        None => LaserFormatArg::infer_from_path(output_path)?,
    };

    let plan = plan_laser_toolpath(&opened.document, page, None, &cfg)
        .map_err(|e| format!("laser toolpath planning failed: {e}"))?;

    let content = match target_fmt {
        LaserFormatArg::Gcode => export_laser_gcode(&plan, &cfg),
        LaserFormatArg::Dxf => export_laser_dxf_r12(&plan),
        LaserFormatArg::Plt => export_laser_plt(&plan),
        LaserFormatArg::Svg => export_laser_svg(&plan),
        LaserFormatArg::Eps => export_laser_eps(&plan),
    };

    atomic_write_bytes(output_path, content.as_bytes()).map_err(|e| {
        format!(
            "failed to write laser output '{}': {e}",
            output_path.display()
        )
    })?;

    let header = match target_fmt {
        LaserFormatArg::Gcode => format!("{} ({:?})", target_fmt.label(), cfg.dialect),
        _ => target_fmt.label().to_owned(),
    };

    Ok(CliOutcome::Printed(format_laser_metrics(
        &header,
        &plan,
        content.len(),
        output_path,
    )))
}

fn format_laser_metrics(
    target_label: &str,
    plan: &LaserToolpathPlan,
    bytes_written: usize,
    output_path: &Path,
) -> String {
    format!(
        "Laser job exported [{target_label}] -> {} ({bytes_written} bytes)\n\
         Cuts: {} | Segments: {} | Duplicates Removed: {} | Curves Joined: {}\n\
         Cut: {:.2} mm | Engrave: {:.2} mm | Rapid: {:.2} mm | Est. Time: {:.1} s",
        output_path.display(),
        plan.cut_count,
        plan.segments.len(),
        plan.duplicate_segments_removed,
        plan.curves_joined,
        plan.total_cut_mm,
        plan.total_engrave_mm,
        plan.total_rapid_mm,
        plan.estimated_seconds
    )
}

/// Normalizes desktop `file://` URIs passed by OS launchers while preserving regular paths verbatim.
fn normalize_desktop_file_arg(path: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };
    let uri_path = raw
        .strip_prefix("file://localhost/")
        .or_else(|| raw.strip_prefix("file:///"))
        .map(|rest| format!("/{rest}"));
    let Some(encoded) = uri_path else {
        return path.to_path_buf();
    };
    if path.exists() {
        return path.to_path_buf();
    }
    let decoded = percent_decode_utf8(&encoded);
    #[cfg(windows)]
    {
        // Strip leading slash on Windows drive paths like `/C:/path/file.cdr`
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return PathBuf::from(&decoded[1..]);
        }
    }
    PathBuf::from(decoded)
}

fn percent_decode_utf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_owned())
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_cli_codepage(raw: Option<&str>) -> Result<Option<u16>, String> {
    let Some(val) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let lower = val.to_ascii_lowercase();
    match lower.as_str() {
        "auto" => Ok(None),
        "cp1256" | "1256" | "arabic" => Ok(Some(1256)),
        "cp1251" | "1251" | "cyrillic" => Ok(Some(1251)),
        "cp1252" | "1252" | "western" | "latin1" => Ok(Some(1252)),
        "utf8" | "utf-8" | "65001" => Ok(Some(65001)),
        _ => val.parse::<u16>().map(Some).map_err(|_| {
            format!("unsupported codepage '{val}' (expected auto|cp1256|cp1251|cp1252|utf8)")
        }),
    }
}

/// Builds the `--diagnostics` summary string with version, license, formats, crates,
/// built-in laser/plotter plugin count, and local font count.
#[must_use]
pub fn build_diagnostics_report() -> String {
    let brand = BrandInfo::current();
    let font_count = discover_local_font_count();
    let builtin_plugins = PluginRegistry::builtin_presets();
    let plugin_ids: Vec<&str> = builtin_plugins.iter().map(|p| p.id.as_str()).collect();
    format!(
        "{} v{} ({})\n\
         Repository: {}\n\
         Workspace Crates: {}, {}, {}, {}, {}, {}, {}, {}\n\
         Supported Import Formats: .coree, .cdr (RIFF v4-v13 / X4-X5 / X6-2025 / .cdr.zip), .cdt (.cdt.zip), .cmx (16/32-bit), .cpt, .svg, .svgz, .pdf, .ai, .eps, .dxf, .dwg, .wmf, .emf, .plt (HPGL), .psd, .png, .jpg, .webp, .tiff, .bmp, .gpl, .ase, .pal, .acb\n\
         Supported Export Formats: .coree, .cdr (RIFF/X4/X6), .cdt, .cmx, .cpt, .svg, .pdf (CMYK/RGB + print marks), .eps, .dxf, .dwg, .wmf, .emf, .plt (HPGL), .psd, .gcode (GRBL/Marlin/Ruida), .png, .jpg, .webp, .tiff, .bmp\n\
         Built-in Laser & Plotter Plugins: {} ({})\n\
         Local System Fonts Indexed: {}\n\
         Mode: 100% Offline Pure-Rust (#![forbid(unsafe_code)])",
        brand.display_name,
        brand.version,
        brand.license,
        brand.repo_url,
        coree_core::CRATE_NAME,
        coree_tools::CRATE_NAME,
        coree_cdr::CRATE_NAME,
        coree_io::CRATE_NAME,
        coree_storage::CRATE_NAME,
        coree_render::CRATE_NAME,
        coree_prod::CRATE_NAME,
        crate::CRATE_NAME,
        builtin_plugins.len(),
        plugin_ids.join(", "),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_positional_files_including_arabic_and_multiple_args() {
        let single = Cli::try_parse_from(["coree", "/tmp/درع الشكر/sample.cdr.zip"]).unwrap();
        assert_eq!(
            execute_cli(&single).unwrap(),
            CliOutcome::LaunchGui(vec![PathBuf::from("/tmp/درع الشكر/sample.cdr.zip")])
        );

        let multi = Cli::try_parse_from(["coree", "a.cdr", "b.pdf", "c.dxf"]).unwrap();
        assert_eq!(
            execute_cli(&multi).unwrap(),
            CliOutcome::LaunchGui(vec![
                PathBuf::from("a.cdr"),
                PathBuf::from("b.pdf"),
                PathBuf::from("c.dxf"),
            ])
        );

        let uri = Cli::try_parse_from(["coree", "file:///tmp/my%20shield.cdr"]).unwrap();
        assert_eq!(
            execute_cli(&uri).unwrap(),
            CliOutcome::LaunchGui(vec![PathBuf::from("/tmp/my shield.cdr")])
        );
    }

    #[test]
    fn cli_diagnostics_lists_all_v02_formats_and_plugins() {
        let diag = build_diagnostics_report();
        for token in [
            ".coree",
            ".cdr",
            ".cdt",
            ".cmx",
            ".cpt",
            ".svg",
            ".svgz",
            ".pdf",
            ".ai",
            ".eps",
            ".dxf",
            ".plt",
            ".psd",
            ".png",
            ".jpg",
            ".webp",
            ".tiff",
            ".bmp",
            ".gcode",
            "Built-in Laser & Plotter Plugins: 7",
            "lightburn-bridge",
        ] {
            assert!(
                diag.contains(token),
                "diagnostics missing '{token}':\n{diag}"
            );
        }
    }

    #[test]
    fn cli_laser_and_thumbnail_validate_bad_arguments() {
        let bad_thumb =
            Cli::try_parse_from(["coree", "thumbnail", "in.cdr", "out.png", "--size", "0"])
                .unwrap();
        assert!(execute_cli(&bad_thumb).is_err());

        let launch_no_plugin =
            Cli::try_parse_from(["coree", "laser", "in.cdr", "out.gcode", "--launch"]).unwrap();
        assert!(execute_cli(&launch_no_plugin).is_err());

        let bad_dedupe =
            Cli::try_parse_from(["coree", "laser", "in.cdr", "out.gcode", "--dedupe=NaN"]).unwrap();
        assert!(execute_cli(&bad_dedupe).is_err());

        for bad_dpi in ["NaN", "inf", "0", "-150"] {
            let flag = format!("--min-dpi={bad_dpi}");
            let bad_preflight =
                Cli::try_parse_from(["coree", "preflight", "in.cdr", &flag]).unwrap();
            assert!(
                execute_cli(&bad_preflight).is_err(),
                "preflight with {flag} must return Err"
            );
        }

        let neg_kerf = Cli::try_parse_from([
            "coree",
            "laser",
            "in.cdr",
            "out.gcode",
            "--kerf",
            "-0.15",
            "--hatch-angle",
            "-45.0",
        ])
        .unwrap();
        assert!(matches!(
            neg_kerf.command,
            Some(Commands::Laser {
                kerf,
                hatch_angle,
                ..
            }) if (kerf + 0.15).abs() < 1e-9 && (hatch_angle + 45.0).abs() < 1e-9
        ));
    }

    #[test]
    fn cli_convert_to_curves_outlines_text_bspline_artistic_and_empty_doc() {
        let dir = tempfile::tempdir().unwrap();
        let empty_in = dir.path().join("empty.coree");
        let empty_out = dir.path().join("empty_curves.coree");
        let empty_doc = Document::new_a4("Empty");
        coree_io::save_coree_with_preview(&empty_doc, None, &empty_in).unwrap();
        let parsed_empty = Cli::try_parse_from([
            "coree",
            "convert-to-curves",
            empty_in.to_str().unwrap(),
            empty_out.to_str().unwrap(),
        ])
        .unwrap();
        assert!(execute_cli(&parsed_empty).is_ok());

        let rich_in = dir.path().join("rich.coree");
        let rich_out = dir.path().join("rich_curves.coree");
        let mut doc = Document::new_a4("Rich");
        let txt = coree_core::ArtisticText::from_plain("Hi", Default::default());
        doc.add_object(
            SceneObject::new(ObjectId(0), "Txt", NodePayload::ArtisticText(txt))
                .with_fill(Fill::None),
            None,
        )
        .unwrap();
        let bs = coree_core::BSplineCurve::from_control_points(
            vec![
                coree_core::Point2D::new(10.0, 10.0),
                coree_core::Point2D::new(30.0, 40.0),
                coree_core::Point2D::new(60.0, 10.0),
            ],
            2,
            false,
        );
        doc.add_object(
            SceneObject::new(ObjectId(0), "BS", NodePayload::BSpline(bs)),
            None,
        )
        .unwrap();
        coree_io::save_coree_with_preview(&doc, None, &rich_in).unwrap();

        let parsed_rich = Cli::try_parse_from([
            "coree",
            "convert-to-curves",
            rich_in.to_str().unwrap(),
            rich_out.to_str().unwrap(),
        ])
        .unwrap();
        assert!(execute_cli(&parsed_rich).is_ok());
        let loaded = open_any_document(&rich_out).unwrap().document;
        assert!(loaded
            .objects()
            .values()
            .all(|o| matches!(o.payload, NodePayload::Path(_))));
        assert!(loaded
            .objects()
            .values()
            .any(|o| o.name == "Txt" && o.fill.is_visible()));

        // REM-02: Clone added BEFORE text masters (ObjectId(clone) < ObjectId(master)) must still convert
        let rev_in = dir.path().join("rev_clone.coree");
        let rev_out = dir.path().join("rev_clone_curves.coree");
        let mut rev_doc = Document::new_a4("RevCloneText");
        let master_id = rev_doc
            .add_object(
                SceneObject::new(
                    ObjectId(10),
                    "MasterLate",
                    NodePayload::ArtisticText(coree_core::ArtisticText::from_plain(
                        "HI",
                        Default::default(),
                    )),
                )
                .with_fill(Fill::None),
                None,
            )
            .unwrap();
        let clone_id = rev_doc
            .add_object(
                SceneObject::new(
                    ObjectId(5),
                    "CloneEarly",
                    NodePayload::Clone {
                        master_id,
                        link_fill: true,
                        link_outline: true,
                        link_shape: true,
                    },
                )
                .with_fill(Fill::None)
                .with_transform(coree_core::Affine2D::translate(10.0, 40.0)),
                None,
            )
            .unwrap();
        assert!(clone_id.0 < master_id.0);
        coree_io::save_coree_with_preview(&rev_doc, None, &rev_in).unwrap();

        let parsed_rev = Cli::try_parse_from([
            "coree",
            "convert-to-curves",
            rev_in.to_str().unwrap(),
            rev_out.to_str().unwrap(),
        ])
        .unwrap();
        assert!(execute_cli(&parsed_rev).is_ok());
        let rev_loaded = open_any_document(&rev_out).unwrap().document;
        assert!(rev_loaded
            .objects()
            .values()
            .all(|o| matches!(o.payload, NodePayload::Path(_))));
        assert!(rev_loaded.objects().values().all(|o| o.fill.is_visible()));
    }

    #[test]
    fn cli_help_lists_dwg_wmf_emf_and_acb_for_inspect_and_convert() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        for sub in ["inspect", "convert"] {
            let subcmd = cmd.find_subcommand_mut(sub).expect("subcommand exists");
            let help = subcmd.render_long_help().to_string();
            for ext in [".dwg", ".wmf", ".emf", ".acb"] {
                assert!(
                    help.contains(ext),
                    "`{sub} --help` must list `{ext}`:\n{help}"
                );
            }
        }
    }
}
