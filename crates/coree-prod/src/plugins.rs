// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extensible Laser, Engraving, Plotter & Production Plugin Architecture.
//!
//! Enables zero-friction integration with external laser cutting, fiber engraving,
//! diode laser, and vinyl plotter software — plus sandboxed Rhai community plugins:
//! - Built-in configurable bridge presets: LightBurn, RDWorks/Ruida CO2, EZCad Fiber,
//!   LaserDRW/M2 Nano (K40), LaserGRBL, InkCut/VisiCut, and Vinyl Plotter (HPGL)
//! - Clean-room `plugin.json` and `plugin.toml` manifest parsing with strict path-traversal
//!   and file-size validation
//! - Cross-platform executable auto-detection (user override, OS default install locations, `PATH`)
//! - Direct argv-substituted bridge launcher via `std::process::Command` (zero shell invocation,
//!   zero command injection)
//! - Sample custom plugin template installer for users and community plugin authors.

use crate::error::{ProdError, ProdResult};
use crate::laser_plugin::{
    export_laser_dxf_r12, export_laser_eps, export_laser_gcode, export_laser_plt, export_laser_svg,
    plan_laser_toolpath, GcodeDialect, LaserJobConfig, LaserOperationKind, LaserToolpathPlan,
};
use crate::macros::{
    plan_macro_ops, run_macro_with_ops, MacroExecutionResult, MacroSandboxConfig, MacroValue,
};
use coree_core::{Document, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Maximum allowed plugin manifest file size in bytes (256 KiB).
pub const MAX_PLUGIN_MANIFEST_BYTES: u64 = 256 * 1024;

/// Maximum allowed Rhai plugin script file size in bytes (1 MiB).
pub const MAX_PLUGIN_SCRIPT_BYTES: u64 = 1024 * 1024;

/// Maximum plugins loaded from any single discovery directory.
pub const MAX_PLUGINS_PER_DIR: usize = 256;

/// Maximum number of argv template entries in an external bridge plugin.
pub const MAX_BRIDGE_ARGS: usize = 64;

/// Functional category for organizing plugins in menus and dockers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PluginCategory {
    #[default]
    Laser,
    Plotter,
    Engraving,
    Prepress,
    Automation,
    Custom,
}

impl PluginCategory {
    /// Case-insensitive parser used by the clean-room `plugin.toml` loader.
    pub fn parse_str(raw: &str) -> ProdResult<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "laser" => Ok(Self::Laser),
            "plotter" | "vinyl" | "cutter" => Ok(Self::Plotter),
            "engraving" | "fiber" => Ok(Self::Engraving),
            "prepress" | "print" => Ok(Self::Prepress),
            "automation" | "macro" => Ok(Self::Automation),
            "custom" => Ok(Self::Custom),
            other => Err(ProdError::InvalidParameter(format!(
                "unknown plugin category '{other}'"
            ))),
        }
    }
}

/// Vector / CNC interchange format exported before handing off to an external bridge app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PluginExportFormat {
    #[default]
    DxfR12,
    Svg,
    PltHpgl,
    GcodeGrbl,
    GcodeMarlin,
    Eps,
}

impl PluginExportFormat {
    /// Canonical lowercase file extension (without leading dot).
    #[must_use]
    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::DxfR12 => "dxf",
            Self::Svg => "svg",
            Self::PltHpgl => "plt",
            Self::GcodeGrbl | Self::GcodeMarlin => "gcode",
            Self::Eps => "eps",
        }
    }

    /// Case-insensitive parser used by the clean-room `plugin.toml` loader.
    pub fn parse_str(raw: &str) -> ProdResult<Self> {
        match raw
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_', ' '], "")
            .as_str()
        {
            "dxfr12" | "dxf" => Ok(Self::DxfR12),
            "svg" => Ok(Self::Svg),
            "plthpgl" | "plt" | "hpgl" => Ok(Self::PltHpgl),
            "gcodegrbl" | "grbl" | "gcode" => Ok(Self::GcodeGrbl),
            "gcodemarlin" | "marlin" => Ok(Self::GcodeMarlin),
            "eps" => Ok(Self::Eps),
            other => Err(ProdError::InvalidParameter(format!(
                "unknown plugin export_format '{other}'"
            ))),
        }
    }
}

/// Execution model of a `coree` production plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PluginKind {
    /// Exports a prepared laser/plotter vector/G-Code file and launches an external application.
    ExternalBridge {
        target_app: String,
        default_paths_linux: Vec<String>,
        default_paths_windows: Vec<String>,
        args_template: Vec<String>,
        export_format: PluginExportFormat,
        selection_only: bool,
        auto_dedupe: bool,
        auto_join_mm: f64,
        auto_hatch: bool,
    },
    /// Runs a local sandboxed Rhai automation script located inside the plugin directory.
    RhaiScript {
        script_file: String,
        entry_label: String,
    },
}

/// Serializable manifest describing a built-in or user-installed production plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub category: PluginCategory,
    pub kind: PluginKind,
    /// Directory containing this plugin on disk (`None` for built-in presets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<PathBuf>,
    /// `true` for built-in presets shipped with `coree-prod`.
    #[serde(default)]
    pub builtin: bool,
}

impl PluginManifest {
    /// Parses a `plugin.json` string and validates all security & parameter invariants.
    ///
    /// Security: resets untrusted `source_dir` and `builtin` fields from JSON input to
    /// `None` and `false` so external manifests cannot spoof built-in status or arbitrary
    /// script root directories.
    pub fn from_json_str(raw: &str) -> ProdResult<Self> {
        let mut manifest: Self = serde_json::from_str(raw)?;
        manifest.source_dir = None;
        manifest.builtin = false;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parses a clean-room `plugin.toml` key/value manifest (zero external TOML crates)
    /// and validates all security & parameter invariants.
    pub fn from_toml_str(raw: &str) -> ProdResult<Self> {
        let manifest = parse_plugin_toml(raw)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validates identifier syntax, numeric finiteness, and strict path-traversal safety.
    pub fn validate(&self) -> ProdResult<()> {
        validate_plugin_id(&self.id)?;
        if self.name.trim().is_empty() || self.name.len() > 256 {
            return Err(ProdError::InvalidParameter(
                "plugin name must be non-empty and <= 256 bytes".into(),
            ));
        }
        if self.version.trim().is_empty() || self.version.len() > 64 {
            return Err(ProdError::InvalidParameter(
                "plugin version must be non-empty and <= 64 bytes".into(),
            ));
        }

        match &self.kind {
            PluginKind::ExternalBridge {
                target_app,
                default_paths_linux,
                default_paths_windows,
                args_template,
                auto_join_mm,
                ..
            } => {
                validate_safe_target_app(target_app)?;
                if !auto_join_mm.is_finite() || *auto_join_mm < 0.0 || *auto_join_mm > 100.0 {
                    return Err(ProdError::InvalidParameter(format!(
                        "bridge plugin auto_join_mm must be finite in [0, 100] (got {auto_join_mm})"
                    )));
                }
                if args_template.len() > MAX_BRIDGE_ARGS {
                    return Err(ProdError::InvalidParameter(format!(
                        "bridge plugin args_template exceeds cap of {MAX_BRIDGE_ARGS} arguments"
                    )));
                }
                for arg in args_template {
                    if arg.contains('\0') {
                        return Err(ProdError::InvalidParameter(
                            "bridge args_template must not contain null bytes".into(),
                        ));
                    }
                }
                for p in default_paths_linux
                    .iter()
                    .chain(default_paths_windows.iter())
                {
                    if p.contains('\0') {
                        return Err(ProdError::InvalidParameter(
                            "bridge default executable paths must not contain null bytes".into(),
                        ));
                    }
                }
            }
            PluginKind::RhaiScript {
                script_file,
                entry_label,
            } => {
                validate_safe_relative_script_path(script_file)?;
                if entry_label.trim().is_empty() || entry_label.len() > 128 {
                    return Err(ProdError::InvalidParameter(
                        "RhaiScript entry_label must be non-empty and <= 128 bytes".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Validates that a plugin `id` contains only safe identifier characters and zero path traversal.
fn validate_plugin_id(id: &str) -> ProdResult<()> {
    if id.is_empty() || id.len() > 128 {
        return Err(ProdError::InvalidParameter(
            "plugin id must be 1..=128 ASCII characters".into(),
        ));
    }
    if id.contains("..") || id.starts_with('.') || id.ends_with('.') {
        return Err(ProdError::InvalidParameter(format!(
            "plugin id '{id}' contains illegal dot sequence"
        )));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ProdError::InvalidParameter(format!(
            "plugin id '{id}' must contain only ASCII letters, digits, '-', '_', or '.'"
        )));
    }
    Ok(())
}

/// Validates that `target_app` is strictly a bare executable name (no `/`, `\`, `..`, or null bytes)
/// so `PATH` lookups (`dir.join(target_app)`) can never traverse out of `PATH` entries.
fn validate_safe_target_app(target_app: &str) -> ProdResult<()> {
    let trimmed = target_app.trim();
    if trimmed.is_empty() || trimmed.len() > 128 || trimmed.contains('\0') {
        return Err(ProdError::InvalidParameter(
            "bridge plugin target_app must be 1..=128 chars without null bytes".into(),
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(ProdError::InvalidParameter(format!(
            "bridge plugin target_app '{trimmed}' must be a bare executable name without path separators or '..'"
        )));
    }
    let mut comps = Path::new(trimmed).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(ProdError::InvalidParameter(format!(
            "bridge plugin target_app '{trimmed}' must be a single path component"
        ))),
    }
}

/// Validates that `script_file` is strictly a relative `.rhai` path with no `..` or root traversal.
fn validate_safe_relative_script_path(script_file: &str) -> ProdResult<()> {
    let trimmed = script_file.trim();
    if trimmed.is_empty() || trimmed.len() > 256 || trimmed.contains('\0') {
        return Err(ProdError::InvalidParameter(
            "script_file must be a non-empty relative path without null bytes".into(),
        ));
    }
    // Reject Windows backslashes and explicit parent references across all platforms
    if trimmed.contains('\\') || trimmed.contains("..") {
        return Err(ProdError::InvalidParameter(format!(
            "script_file '{trimmed}' must not contain '..' or backslashes"
        )));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(ProdError::InvalidParameter(format!(
            "script_file '{trimmed}' must be relative to the plugin directory"
        )));
    }
    let mut normal_count = 0usize;
    for comp in path.components() {
        match comp {
            Component::Normal(_) => normal_count += 1,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ProdError::InvalidParameter(format!(
                    "script_file '{trimmed}' contains forbidden path traversal component"
                )));
            }
        }
    }
    if normal_count == 0 {
        return Err(ProdError::InvalidParameter(
            "script_file must name a file inside the plugin directory".into(),
        ));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if ext.as_deref() != Some("rhai") {
        return Err(ProdError::InvalidParameter(format!(
            "script_file '{trimmed}' must have a '.rhai' extension"
        )));
    }
    Ok(())
}

/// Summary returned by [`export_for_plugin`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginExportOutcome {
    pub plugin_id: String,
    pub format: PluginExportFormat,
    pub output_path: PathBuf,
    pub bytes_written: usize,
    pub plan: LaserToolpathPlan,
}

/// Registry holding built-in laser/plotter bridge presets and discovered user plugins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginRegistry {
    plugins: Vec<PluginManifest>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl PluginRegistry {
    /// Creates an empty registry with no presets loaded.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Creates a registry pre-populated with all 7 built-in laser, fiber, and plotter bridge presets.
    #[must_use]
    pub fn with_builtins() -> Self {
        Self {
            plugins: Self::builtin_presets(),
        }
    }

    /// Returns the 7 ready-to-use built-in laser & plotter bridge presets.
    #[must_use]
    pub fn builtin_presets() -> Vec<PluginManifest> {
        vec![
            PluginManifest {
                id: "lightburn-bridge".to_owned(),
                name: "LightBurn".to_owned(),
                version: "1.0.0".to_owned(),
                author: "coree".to_owned(),
                description:
                    "Send clean mm-scaled SVG (or DXF R12) layers directly to LightBurn CO2/Diode/Galvo laser software."
                        .to_owned(),
                category: PluginCategory::Laser,
                kind: PluginKind::ExternalBridge {
                    target_app: "LightBurn".to_owned(),
                    default_paths_linux: vec![
                        "/usr/bin/LightBurn".to_owned(),
                        "/usr/local/bin/LightBurn".to_owned(),
                        "/opt/LightBurn/LightBurn".to_owned(),
                        "~/.local/bin/LightBurn".to_owned(),
                    ],
                    default_paths_windows: vec![
                        r"C:\Program Files\LightBurn\LightBurn.exe".to_owned(),
                        r"C:\Program Files (x86)\LightBurn\LightBurn.exe".to_owned(),
                    ],
                    args_template: vec!["{file}".to_owned()],
                    export_format: PluginExportFormat::Svg,
                    selection_only: false,
                    auto_dedupe: true,
                    auto_join_mm: 0.05,
                    auto_hatch: false,
                },
                source_dir: None,
                builtin: true,
            },
            PluginManifest {
                id: "rdworks-ruida".to_owned(),
                name: "RDWorks / Ruida CO2 Laser".to_owned(),
                version: "1.0.0".to_owned(),
                author: "coree".to_owned(),
                description:
                    "Export clean AutoCAD R12 DXF with color-coded CUT/ENGRAVE/HATCH layers for RDWorksV8 & Ruida CO2 controllers."
                        .to_owned(),
                category: PluginCategory::Laser,
                kind: PluginKind::ExternalBridge {
                    target_app: "RDWorksV8".to_owned(),
                    default_paths_linux: vec![
                        "/usr/local/bin/rdworks".to_owned(),
                        "/opt/rdworks/RDWorksV8".to_owned(),
                    ],
                    default_paths_windows: vec![
                        r"C:\RDWorksV8\RDWorksV8.exe".to_owned(),
                        r"C:\Program Files\RDWorksV8\RDWorksV8.exe".to_owned(),
                        r"C:\Program Files (x86)\RDWorksV8\RDWorksV8.exe".to_owned(),
                    ],
                    args_template: vec!["{file}".to_owned()],
                    export_format: PluginExportFormat::DxfR12,
                    selection_only: false,
                    auto_dedupe: true,
                    auto_join_mm: 0.05,
                    auto_hatch: false,
                },
                source_dir: None,
                builtin: true,
            },
            PluginManifest {
                id: "ezcad-fiber".to_owned(),
                name: "EZCad Fiber Laser".to_owned(),
                version: "1.0.0".to_owned(),
                author: "coree".to_owned(),
                description:
                    "Export AutoCAD R12 DXF with automatic scanline hatch fill for EZCad2/3 JCZ Galvo fiber marking lasers."
                        .to_owned(),
                category: PluginCategory::Engraving,
                kind: PluginKind::ExternalBridge {
                    target_app: "EzCad2".to_owned(),
                    default_paths_linux: vec![
                        "/usr/local/bin/ezcad".to_owned(),
                        "/opt/ezcad/EzCad2".to_owned(),
                    ],
                    default_paths_windows: vec![
                        r"C:\EzCad2\EzCad2.exe".to_owned(),
                        r"C:\EZCAD\EzCad2.exe".to_owned(),
                        r"C:\Program Files\EzCad2\EzCad2.exe".to_owned(),
                        r"C:\Program Files (x86)\EzCad2\EzCad2.exe".to_owned(),
                    ],
                    args_template: vec!["{file}".to_owned()],
                    export_format: PluginExportFormat::DxfR12,
                    selection_only: false,
                    auto_dedupe: true,
                    auto_join_mm: 0.05,
                    auto_hatch: true,
                },
                source_dir: None,
                builtin: true,
            },
            PluginManifest {
                id: "laserdrw-m2nano".to_owned(),
                name: "LaserDRW / M2 Nano (K40)".to_owned(),
                version: "1.0.0".to_owned(),
                author: "coree".to_owned(),
                description:
                    "Export HPGL PLT (or DXF R12) vector cut/engrave paths for LaserDRW, K40 Whisperer, MeerK40t, and M2 Nano boards."
                        .to_owned(),
                category: PluginCategory::Laser,
                kind: PluginKind::ExternalBridge {
                    target_app: "LaserDRW".to_owned(),
                    default_paths_linux: vec![
                        "/usr/bin/k40_whisperer".to_owned(),
                        "/usr/local/bin/k40_whisperer".to_owned(),
                        "/usr/bin/meerk40t".to_owned(),
                        "/usr/local/bin/meerk40t".to_owned(),
                    ],
                    default_paths_windows: vec![
                        r"C:\Program Files (x86)\LaserDRW\LaserDRW.exe".to_owned(),
                        r"C:\Program Files\LaserDRW\LaserDRW.exe".to_owned(),
                        r"C:\Program Files\K40 Whisperer\k40_whisperer.exe".to_owned(),
                        r"C:\Program Files\MeerK40t\MeerK40t.exe".to_owned(),
                    ],
                    args_template: vec!["{file}".to_owned()],
                    export_format: PluginExportFormat::PltHpgl,
                    selection_only: false,
                    auto_dedupe: true,
                    auto_join_mm: 0.05,
                    auto_hatch: false,
                },
                source_dir: None,
                builtin: true,
            },
            PluginManifest {
                id: "lasergrbl-bridge".to_owned(),
                name: "LaserGRBL".to_owned(),
                version: "1.0.0".to_owned(),
                author: "coree".to_owned(),
                description:
                    "Export optimized GRBL 1.1 dynamic laser G-Code (.gcode) directly to LaserGRBL diode & CNC controllers."
                        .to_owned(),
                category: PluginCategory::Laser,
                kind: PluginKind::ExternalBridge {
                    target_app: "LaserGRBL".to_owned(),
                    default_paths_linux: vec![
                        "/usr/bin/lasergrbl".to_owned(),
                        "/usr/local/bin/lasergrbl".to_owned(),
                        "/usr/bin/ugsplatform".to_owned(),
                    ],
                    default_paths_windows: vec![
                        r"C:\Program Files (x86)\LaserGRBL\LaserGRBL.exe".to_owned(),
                        r"C:\Program Files\LaserGRBL\LaserGRBL.exe".to_owned(),
                    ],
                    args_template: vec!["{file}".to_owned()],
                    export_format: PluginExportFormat::GcodeGrbl,
                    selection_only: false,
                    auto_dedupe: true,
                    auto_join_mm: 0.05,
                    auto_hatch: false,
                },
                source_dir: None,
                builtin: true,
            },
            PluginManifest {
                id: "inkcut-visicut".to_owned(),
                name: "InkCut / VisiCut".to_owned(),
                version: "1.0.0".to_owned(),
                author: "coree".to_owned(),
                description:
                    "Export clean mm-scaled SVG (or HPGL PLT) directly to InkCut or VisiCut on Linux and Windows."
                        .to_owned(),
                category: PluginCategory::Laser,
                kind: PluginKind::ExternalBridge {
                    target_app: "inkcut".to_owned(),
                    default_paths_linux: vec![
                        "/usr/bin/inkcut".to_owned(),
                        "/usr/local/bin/inkcut".to_owned(),
                        "~/.local/bin/inkcut".to_owned(),
                        "/usr/bin/visicut".to_owned(),
                        "/opt/visicut/visicut".to_owned(),
                    ],
                    default_paths_windows: vec![
                        r"C:\Program Files\VisiCut\VisiCut.exe".to_owned(),
                        r"C:\Program Files (x86)\VisiCut\VisiCut.exe".to_owned(),
                    ],
                    args_template: vec!["open".to_owned(), "{file}".to_owned()],
                    export_format: PluginExportFormat::Svg,
                    selection_only: false,
                    auto_dedupe: true,
                    auto_join_mm: 0.05,
                    auto_hatch: false,
                },
                source_dir: None,
                builtin: true,
            },
            PluginManifest {
                id: "vinyl-plotter-hpgl".to_owned(),
                name: "Vinyl Cutter / Plotter (HPGL)".to_owned(),
                version: "1.0.0".to_owned(),
                author: "coree".to_owned(),
                description:
                    "Export 40-units/mm HPGL (.plt) CutContour paths with optional weeding box for Roland, Graphtec, and Summa vinyl cutters."
                        .to_owned(),
                category: PluginCategory::Plotter,
                kind: PluginKind::ExternalBridge {
                    target_app: "inkcut".to_owned(),
                    default_paths_linux: vec![
                        "/usr/bin/inkcut".to_owned(),
                        "/usr/local/bin/inkcut".to_owned(),
                        "~/.local/bin/inkcut".to_owned(),
                    ],
                    default_paths_windows: vec![
                        r"C:\Program Files\CutStudio\CutStudio.exe".to_owned(),
                        r"C:\Program Files (x86)\Roland\CutStudio\CutStudio.exe".to_owned(),
                        r"C:\Program Files\Graphtec\Cutting Master\CuttingMaster.exe".to_owned(),
                    ],
                    args_template: vec!["open".to_owned(), "{file}".to_owned()],
                    export_format: PluginExportFormat::PltHpgl,
                    selection_only: false,
                    auto_dedupe: true,
                    auto_join_mm: 0.05,
                    auto_hatch: false,
                },
                source_dir: None,
                builtin: true,
            },
        ]
    }

    /// Returns all plugins currently registered.
    #[must_use]
    pub fn plugins(&self) -> &[PluginManifest] {
        &self.plugins
    }

    /// Looks up a plugin by its `id` (case-insensitive).
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&PluginManifest> {
        let target = id.trim();
        self.plugins
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(target))
    }

    /// Looks up a mutable reference to a plugin by `id` so users can customize presets.
    #[must_use]
    pub fn find_mut(&mut self, id: &str) -> Option<&mut PluginManifest> {
        let target = id.trim();
        self.plugins
            .iter_mut()
            .find(|p| p.id.eq_ignore_ascii_case(target))
    }

    /// Lists all plugins matching `category`.
    #[must_use]
    pub fn by_category(&self, category: PluginCategory) -> Vec<&PluginManifest> {
        self.plugins
            .iter()
            .filter(|p| p.category == category)
            .collect()
    }

    /// Validates and inserts (or replaces by matching `id`) a plugin manifest.
    pub fn register(&mut self, manifest: PluginManifest) -> ProdResult<()> {
        manifest.validate()?;
        if let Some(slot) = self
            .plugins
            .iter_mut()
            .find(|p| p.id.eq_ignore_ascii_case(&manifest.id))
        {
            *slot = manifest;
        } else {
            self.plugins.push(manifest);
        }
        Ok(())
    }

    /// Removes a plugin by `id`. Returns `true` if a plugin was removed.
    pub fn unregister(&mut self, id: &str) -> bool {
        let before = self.plugins.len();
        let target = id.trim();
        self.plugins.retain(|p| !p.id.eq_ignore_ascii_case(target));
        self.plugins.len() < before
    }

    /// Discovers user plugins in `plugin_dirs` and merges them into this registry
    /// (user manifests with the same `id` override built-in defaults). Returns count loaded.
    pub fn load_discovered(&mut self, plugin_dirs: &[PathBuf]) -> usize {
        let found = discover_plugins(plugin_dirs);
        let count = found.len();
        for manifest in found {
            let _ = self.register(manifest);
        }
        count
    }
}

/// Returns standard OS plugin discovery directories:
/// - Linux/macOS: `$XDG_CONFIG_HOME/coree/plugins` or `$HOME/.config/coree/plugins`
/// - Windows: `%APPDATA%\coree\plugins`
#[must_use]
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                dirs.push(PathBuf::from(appdata).join("coree").join("plugins"));
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.trim().is_empty() {
                dirs.push(PathBuf::from(xdg).join("coree").join("plugins"));
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.trim().is_empty() {
                let candidate = PathBuf::from(home)
                    .join(".config")
                    .join("coree")
                    .join("plugins");
                if !dirs.contains(&candidate) {
                    dirs.push(candidate);
                }
            }
        }
    }
    dirs
}

/// Scans `plugin_dirs` for immediate subdirectories containing `plugin.json` or `plugin.toml`,
/// enforcing strict manifest size caps, path-traversal checks, and symlink containment.
#[must_use]
pub fn discover_plugins(plugin_dirs: &[PathBuf]) -> Vec<PluginManifest> {
    let mut discovered = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for root in plugin_dirs {
        let Ok(root_canon) = fs::canonicalize(root) else {
            continue;
        };
        if !root_canon.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&root_canon) else {
            continue;
        };

        let mut subdirs: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || name.contains("..") {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            // Reject symlinked plugin folders that escape the plugin root
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                subdirs.push(path);
            }
        }

        subdirs.sort();

        let mut loaded_from_dir = 0usize;
        for subdir in subdirs {
            if loaded_from_dir >= MAX_PLUGINS_PER_DIR {
                break;
            }
            let Ok(subdir_canon) = fs::canonicalize(&subdir) else {
                continue;
            };
            if !subdir_canon.starts_with(&root_canon) {
                continue;
            }

            let json_path = subdir_canon.join("plugin.json");
            let toml_path = subdir_canon.join("plugin.toml");

            let parsed = if let Some(m) = try_load_manifest_file(&json_path, &subdir_canon, true) {
                Some(m)
            } else {
                try_load_manifest_file(&toml_path, &subdir_canon, false)
            };

            if let Some(mut manifest) = parsed {
                // Verify RhaiScript file stays strictly inside `subdir_canon` if it exists
                if let PluginKind::RhaiScript {
                    ref script_file, ..
                } = manifest.kind
                {
                    if resolve_contained_script(&subdir_canon, script_file).is_err() {
                        continue;
                    }
                }
                manifest.source_dir = Some(subdir_canon);
                manifest.builtin = false;
                let id_key = manifest.id.to_ascii_lowercase();
                if seen_ids.insert(id_key) {
                    discovered.push(manifest);
                    loaded_from_dir += 1;
                }
            }
        }
    }

    discovered
}

fn try_load_manifest_file(
    manifest_path: &Path,
    subdir_canon: &Path,
    is_json: bool,
) -> Option<PluginManifest> {
    let meta = fs::symlink_metadata(manifest_path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_PLUGIN_MANIFEST_BYTES {
        return None;
    }
    let canon = fs::canonicalize(manifest_path).ok()?;
    if !canon.starts_with(subdir_canon) {
        return None;
    }
    let raw = fs::read_to_string(&canon).ok()?;
    if is_json {
        PluginManifest::from_json_str(&raw).ok()
    } else {
        PluginManifest::from_toml_str(&raw).ok()
    }
}

fn resolve_contained_script(plugin_dir: &Path, script_file: &str) -> ProdResult<PathBuf> {
    validate_safe_relative_script_path(script_file)?;
    let dir_canon = fs::canonicalize(plugin_dir)?;
    let candidate = dir_canon.join(script_file);
    let meta = fs::symlink_metadata(&candidate)?;
    if meta.file_type().is_symlink() {
        return Err(ProdError::InvalidParameter(format!(
            "script_file '{script_file}' must not be a symlink"
        )));
    }
    if !meta.is_file() {
        return Err(ProdError::InvalidParameter(format!(
            "script_file '{script_file}' is not a regular file"
        )));
    }
    if meta.len() > MAX_PLUGIN_SCRIPT_BYTES {
        return Err(ProdError::InvalidParameter(format!(
            "script_file '{script_file}' exceeds maximum size of {MAX_PLUGIN_SCRIPT_BYTES} bytes"
        )));
    }
    let script_canon = fs::canonicalize(&candidate)?;
    if !script_canon.starts_with(&dir_canon) {
        return Err(ProdError::InvalidParameter(format!(
            "script_file '{script_file}' resolves outside plugin directory"
        )));
    }
    Ok(script_canon)
}

/// Auto-detects the external executable path for a bridge plugin.
///
/// Search priority:
/// 1. Explicit user `override_path` (if provided and points to an existing regular file)
/// 2. OS-specific `default_paths_linux` / `default_paths_windows` (expanding leading `~/`)
/// 3. `PATH` environment variable lookup for `target_app` (plus `.exe` on Windows)
#[must_use]
pub fn detect_executable(plugin: &PluginManifest, override_path: Option<&Path>) -> Option<PathBuf> {
    let PluginKind::ExternalBridge {
        ref target_app,
        ref default_paths_linux,
        ref default_paths_windows,
        ..
    } = plugin.kind
    else {
        return None;
    };

    if let Some(ovr) = override_path {
        if is_usable_executable_file(ovr) {
            return Some(ovr.to_path_buf());
        }
    }

    let os_defaults: &[String] = if cfg!(windows) {
        default_paths_windows
    } else {
        default_paths_linux
    };

    for raw in os_defaults {
        let candidate = expand_tilde_path(raw);
        if is_usable_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    // Fallback: scan `PATH` environment variable for `target_app` (strict bare-name guard)
    if validate_safe_target_app(target_app).is_ok() {
        if let Some(path_var) = std::env::var_os("PATH") {
            let mut names = vec![target_app.clone()];
            let lower = target_app.to_ascii_lowercase();
            if lower != *target_app {
                names.push(lower.clone());
            }
            #[cfg(windows)]
            {
                if !lower.ends_with(".exe") {
                    names.push(format!("{target_app}.exe"));
                    if lower != *target_app {
                        names.push(format!("{lower}.exe"));
                    }
                }
            }
            for dir in std::env::split_paths(&path_var) {
                for name in &names {
                    let candidate = dir.join(name);
                    if is_usable_executable_file(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    None
}

fn expand_tilde_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return PathBuf::from(home).join(rest);
            }
        }
    }
    PathBuf::from(raw)
}

fn is_usable_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Exports `doc.pages[page_idx]` into `out_path` using the format and laser-prep defaults
/// configured on `plugin` (combining `plugin` flags with `laser_cfg`).
pub fn export_for_plugin(
    doc: &Document,
    page_idx: usize,
    selection_ids: Option<&[ObjectId]>,
    plugin: &PluginManifest,
    laser_cfg: &LaserJobConfig,
    out_path: &Path,
) -> ProdResult<PluginExportOutcome> {
    plugin.validate()?;
    let PluginKind::ExternalBridge {
        export_format,
        selection_only,
        auto_dedupe,
        auto_join_mm,
        auto_hatch,
        ..
    } = &plugin.kind
    else {
        return Err(ProdError::InvalidParameter(format!(
            "plugin '{}' is a RhaiScript plugin, not an ExternalBridge exporter",
            plugin.id
        )));
    };

    if *selection_only && selection_ids.is_none_or(<[ObjectId]>::is_empty) {
        return Err(ProdError::InvalidParameter(format!(
            "plugin '{}' requires at least one selected object (selection_only = true)",
            plugin.id
        )));
    }

    let mut effective_cfg = laser_cfg.clone();
    if *auto_dedupe && effective_cfg.dedupe_tolerance_mm <= 0.0 {
        effective_cfg.dedupe_tolerance_mm = 0.05;
    }
    if *auto_join_mm > 0.0 && effective_cfg.join_tolerance_mm <= 0.0 {
        effective_cfg.join_tolerance_mm = *auto_join_mm;
    }
    if *auto_hatch {
        for rule in &mut effective_cfg.rules {
            if rule.operation == LaserOperationKind::HatchEngrave {
                rule.enabled = true;
            }
        }
    }
    match export_format {
        PluginExportFormat::GcodeGrbl => effective_cfg.dialect = GcodeDialect::Grbl11,
        PluginExportFormat::GcodeMarlin => effective_cfg.dialect = GcodeDialect::Marlin,
        _ => {}
    }

    let plan = plan_laser_toolpath(doc, page_idx, selection_ids, &effective_cfg)?;

    let content = match export_format {
        PluginExportFormat::DxfR12 => export_laser_dxf_r12(&plan),
        PluginExportFormat::Svg => export_laser_svg(&plan),
        PluginExportFormat::PltHpgl => export_laser_plt(&plan),
        PluginExportFormat::GcodeGrbl | PluginExportFormat::GcodeMarlin => {
            export_laser_gcode(&plan, &effective_cfg)
        }
        PluginExportFormat::Eps => export_laser_eps(&plan),
    };

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(out_path, content.as_bytes())?;

    Ok(PluginExportOutcome {
        plugin_id: plugin.id.clone(),
        format: *export_format,
        output_path: out_path.to_path_buf(),
        bytes_written: content.len(),
        plan,
    })
}

/// Performs safe, shell-free argv template substitution (`{file}`, `{dir}`, `{stem}`, `{name}`).
///
/// Arguments are returned as discrete OS argv strings for direct `Command::args()` passing —
/// NEVER concatenated into a shell command string (100% immune to shell command injection).
pub fn substitute_bridge_args(
    args_template: &[String],
    exported_file: &Path,
) -> ProdResult<Vec<String>> {
    if args_template.len() > MAX_BRIDGE_ARGS {
        return Err(ProdError::InvalidParameter(format!(
            "args_template exceeds cap of {MAX_BRIDGE_ARGS} arguments"
        )));
    }
    let file_str = exported_file.to_string_lossy();
    if file_str.is_empty() || file_str.contains('\0') {
        return Err(ProdError::InvalidParameter(
            "exported_file path must be non-empty and contain no null bytes".into(),
        ));
    }
    let dir_str = exported_file
        .parent()
        .map(|p| p.to_string_lossy())
        .unwrap_or_default();
    let stem_str = exported_file
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let name_str = exported_file
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();

    if args_template.is_empty() {
        return Ok(vec![file_str.into_owned()]);
    }

    let mut argv = Vec::with_capacity(args_template.len());
    for tpl in args_template {
        if tpl.contains('\0') {
            return Err(ProdError::InvalidParameter(
                "args_template entry must not contain null bytes".into(),
            ));
        }
        let expanded =
            expand_bridge_arg_single_pass(tpl, &file_str, &dir_str, &stem_str, &name_str);
        argv.push(expanded);
    }
    Ok(argv)
}

/// Single-pass left-to-right token substitution preventing double-expansion when substituted
/// file paths themselves contain literal `{file}`, `{dir}`, `{stem}`, or `{name}` substrings.
fn expand_bridge_arg_single_pass(
    tpl: &str,
    file: &str,
    dir: &str,
    stem: &str,
    name: &str,
) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open..];
        if let Some(tail) = after_open.strip_prefix("{file}") {
            out.push_str(file);
            rest = tail;
        } else if let Some(tail) = after_open.strip_prefix("{dir}") {
            out.push_str(dir);
            rest = tail;
        } else if let Some(tail) = after_open.strip_prefix("{stem}") {
            out.push_str(stem);
            rest = tail;
        } else if let Some(tail) = after_open.strip_prefix("{name}") {
            out.push_str(name);
            rest = tail;
        } else {
            out.push('{');
            rest = &after_open[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Safely launches an external laser / plotter bridge executable via `std::process::Command`
/// with direct argv substitution (`{file}`, `{dir}`, `{stem}`, `{name}`) — NEVER invoking
/// `sh -c` or `cmd /c` (zero command injection).
///
/// Returns the spawned OS child process ID (`u32`).
pub fn launch_external_bridge(
    executable_path: &Path,
    args_template: &[String],
    exported_file: &Path,
) -> ProdResult<u32> {
    let exe_str = executable_path.to_string_lossy();
    if exe_str.is_empty() || exe_str.contains('\0') {
        return Err(ProdError::InvalidParameter(
            "executable_path must be non-empty without null bytes".into(),
        ));
    }
    if !executable_path.is_file() {
        return Err(ProdError::InvalidParameter(format!(
            "bridge executable does not exist or is not a file: {}",
            executable_path.display()
        )));
    }
    let argv = substitute_bridge_args(args_template, exported_file)?;
    let child = Command::new(executable_path).args(&argv).spawn()?;
    Ok(child.id())
}

fn read_rhai_plugin_script(plugin: &PluginManifest) -> ProdResult<String> {
    plugin.validate()?;
    let PluginKind::RhaiScript {
        ref script_file, ..
    } = plugin.kind
    else {
        return Err(ProdError::InvalidParameter(format!(
            "plugin '{}' is not a RhaiScript plugin",
            plugin.id
        )));
    };
    let plugin_dir = plugin.source_dir.as_deref().ok_or_else(|| {
        ProdError::InvalidParameter(format!(
            "RhaiScript plugin '{}' has no source_dir on disk",
            plugin.id
        ))
    })?;
    let script_path = resolve_contained_script(plugin_dir, script_file)?;
    Ok(fs::read_to_string(&script_path)?)
}

/// Plans a `PluginKind::RhaiScript` plugin against `doc` inside the strict Rhai sandbox,
/// returning the execution summary and exact replayable [`coree_core::CommandOp`] sequence
/// without mutating `doc`.
pub fn plan_rhai_plugin_ops(
    doc: &Document,
    plugin: &PluginManifest,
    vars: &BTreeMap<String, MacroValue>,
    sandbox: &MacroSandboxConfig,
) -> ProdResult<(MacroExecutionResult, Vec<coree_core::CommandOp>)> {
    let script = read_rhai_plugin_script(plugin)?;
    plan_macro_ops(doc, &script, vars, sandbox)
}

/// Loads and executes a `PluginKind::RhaiScript` plugin against `doc` inside the strict Rhai sandbox,
/// returning both the execution summary and the exact replayable [`coree_core::CommandOp`] sequence.
pub fn run_rhai_plugin_with_ops(
    doc: &mut Document,
    plugin: &PluginManifest,
    vars: &BTreeMap<String, MacroValue>,
    sandbox: &MacroSandboxConfig,
) -> ProdResult<(MacroExecutionResult, Vec<coree_core::CommandOp>)> {
    let script = read_rhai_plugin_script(plugin)?;
    run_macro_with_ops(doc, &script, vars, sandbox)
}

/// Loads and executes a `PluginKind::RhaiScript` plugin against `doc` inside the strict Rhai sandbox.
pub fn run_rhai_plugin(
    doc: &mut Document,
    plugin: &PluginManifest,
    vars: &BTreeMap<String, MacroValue>,
    sandbox: &MacroSandboxConfig,
) -> ProdResult<MacroExecutionResult> {
    let (res, _ops) = run_rhai_plugin_with_ops(doc, plugin, vars, sandbox)?;
    Ok(res)
}

/// Installs a well-documented sample custom plugin (`custom-laser-example/plugin.json` +
/// `macro.rhai`) inside `plugins_dir` so users and community developers can author their
/// own laser automation and external-bridge plugins easily.
///
/// Returns the created `custom-laser-example` directory path.
pub fn install_sample_plugin_template(plugins_dir: &Path) -> ProdResult<PathBuf> {
    let example_dir = plugins_dir.join("custom-laser-example");
    fs::create_dir_all(&example_dir)?;

    let sample_manifest = PluginManifest {
        id: "custom-laser-example".to_owned(),
        name: "Custom Laser Prep Example".to_owned(),
        version: "1.0.0".to_owned(),
        author: "coree Community".to_owned(),
        description:
            "Sample sandboxed Rhai laser plugin: generates a 1.5 mm CutContour halo around all artwork and logs job dimensions."
                .to_owned(),
        category: PluginCategory::Laser,
        kind: PluginKind::RhaiScript {
            script_file: "macro.rhai".to_owned(),
            entry_label: "Prep CutContour Halo (1.5 mm)".to_owned(),
        },
        source_dir: None,
        builtin: false,
    };

    let manifest_json = serde_json::to_string_pretty(&sample_manifest)?;
    fs::write(
        example_dir.join("plugin.json"),
        format!("{manifest_json}\n"),
    )?;

    let sample_rhai = r#"// coree Custom Laser Plugin Example (Sandboxed Rhai)
// Available variables: override `offset_mm` or `spot_name` from the plugin runner UI.
// Safe host functions: all_object_ids(), non_cut_object_ids(), page_width_mm(),
// page_height_mm(), generate_cut_contour(ids, offset_mm, spot_name), align_grid(...),
// resize_objects(...), translate_objects(...), set_outline_mm(...), recolor_all_to_cmyk(), log(msg).

let offset = if is_def_var("offset_mm") { offset_mm } else { 1.5 };
let spot = if is_def_var("spot_name") { spot_name } else { "CutContour" };
let ids = non_cut_object_ids();

if ids.len() == 0 {
    log("No artwork objects found on active page.");
} else {
    let cid = generate_cut_contour(ids, offset, spot);
    log("Prepared " + spot + " cut path #" + cid + " (offset " + offset + " mm) on " + page_width_mm() + "x" + page_height_mm() + " mm page.");
}
"#;
    fs::write(example_dir.join("macro.rhai"), sample_rhai)?;

    // Also provide a non-loaded `.sample` reference showing how to author a `plugin.toml` external bridge
    let toml_reference = r#"# Reference example: rename to `plugin.toml` in its own subfolder under
# ~/.config/coree/plugins/<your-plugin-id>/plugin.toml (Linux) or
# %APPDATA%\coree\plugins\<your-plugin-id>\plugin.toml (Windows).
#
# [plugin]
# id = "my-custom-laser"
# name = "My Custom Laser Bridge"
# version = "1.0.0"
# author = "Shop Engineer"
# description = "Export DXF R12 and open in custom laser controller"
# category = "Laser"
# kind = "ExternalBridge"
#
# [bridge]
# target_app = "MyLaserApp"
# default_paths_linux = ["/usr/local/bin/mylaser"]
# default_paths_windows = ['C:\MyLaser\MyLaserApp.exe']
# args_template = ["--import", "{file}"]
# export_format = "DxfR12"
# selection_only = false
# auto_dedupe = true
# auto_join_mm = 0.05
# auto_hatch = false
"#;
    fs::write(example_dir.join("bridge.toml.sample"), toml_reference)?;

    Ok(example_dir)
}

// =============================================================================
// Clean-Room `plugin.toml` Key/Value Parser (Zero External TOML Crates)
// =============================================================================

#[derive(Debug, Clone)]
enum TomlVal {
    Str(String),
    Bool(bool),
    Float(f64),
    Array(Vec<String>),
}

fn parse_plugin_toml(raw: &str) -> ProdResult<PluginManifest> {
    let text = raw.strip_prefix('\u{FEFF}').unwrap_or(raw);
    let mut table: BTreeMap<String, TomlVal> = BTreeMap::new();
    let mut section = String::new();

    let lines: Vec<&str> = text.lines().collect();
    let mut idx = 0usize;

    while idx < lines.len() {
        let line_no = idx + 1;
        let stripped = strip_toml_comment(lines[idx]).trim();
        idx += 1;

        if stripped.is_empty() {
            continue;
        }
        if stripped.starts_with('[') && stripped.ends_with(']') {
            section = stripped[1..stripped.len() - 1].trim().to_ascii_lowercase();
            continue;
        }

        let Some((raw_key, raw_val_start)) = stripped.split_once('=') else {
            return Err(ProdError::InvalidParameter(format!(
                "plugin.toml line {line_no}: expected 'key = value'"
            )));
        };
        let key = raw_key.trim().to_ascii_lowercase();
        if key.is_empty() {
            return Err(ProdError::InvalidParameter(format!(
                "plugin.toml line {line_no}: empty key"
            )));
        }

        let mut val_buf = raw_val_start.trim().to_owned();
        // Support multi-line string arrays `[ ... \n ... ]`
        if val_buf.starts_with('[') && !has_closing_bracket_outside_quotes(&val_buf) {
            while idx < lines.len() {
                let next_part = strip_toml_comment(lines[idx]).trim();
                idx += 1;
                val_buf.push(' ');
                val_buf.push_str(next_part);
                if has_closing_bracket_outside_quotes(&val_buf) {
                    break;
                }
            }
        }

        let parsed_val = parse_toml_value(&val_buf, line_no)?;
        // Store both section-qualified and bare key (bare keys allow flat or sectioned layouts)
        if !section.is_empty() {
            table.insert(format!("{section}.{key}"), parsed_val.clone());
        }
        table.insert(key, parsed_val);
    }

    let get_str = |k: &str| -> Option<String> {
        match table.get(k) {
            Some(TomlVal::Str(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let req_str = |k: &str| -> ProdResult<String> {
        get_str(k).ok_or_else(|| {
            ProdError::InvalidParameter(format!("plugin.toml missing required string key '{k}'"))
        })
    };
    let get_bool = |k: &str, default: bool| -> bool {
        match table.get(k) {
            Some(TomlVal::Bool(b)) => *b,
            _ => default,
        }
    };
    let get_float = |k: &str, default: f64| -> f64 {
        match table.get(k) {
            Some(TomlVal::Float(f)) => *f,
            _ => default,
        }
    };
    let get_arr = |k: &str| -> Vec<String> {
        match table.get(k) {
            Some(TomlVal::Array(a)) => a.clone(),
            _ => Vec::new(),
        }
    };

    let id = req_str("id")?;
    let name = req_str("name")?;
    let version = get_str("version").unwrap_or_else(|| "1.0.0".to_owned());
    let author = get_str("author").unwrap_or_else(|| "Unknown".to_owned());
    let description = get_str("description").unwrap_or_default();
    let category = match get_str("category") {
        Some(c) => PluginCategory::parse_str(&c)?,
        None => PluginCategory::Custom,
    };

    let kind_str = get_str("kind").unwrap_or_else(|| {
        if table.contains_key("script_file") {
            "RhaiScript".to_owned()
        } else {
            "ExternalBridge".to_owned()
        }
    });

    let kind = match kind_str
        .trim()
        .to_ascii_lowercase()
        .replace(['_', '-'], "")
        .as_str()
    {
        "externalbridge" | "bridge" => {
            let target_app = req_str("target_app")?;
            let export_format = match get_str("export_format") {
                Some(f) => PluginExportFormat::parse_str(&f)?,
                None => PluginExportFormat::DxfR12,
            };
            let args_template = {
                let a = get_arr("args_template");
                if a.is_empty() {
                    vec!["{file}".to_owned()]
                } else {
                    a
                }
            };
            PluginKind::ExternalBridge {
                target_app,
                default_paths_linux: get_arr("default_paths_linux"),
                default_paths_windows: get_arr("default_paths_windows"),
                args_template,
                export_format,
                selection_only: get_bool("selection_only", false),
                auto_dedupe: get_bool("auto_dedupe", true),
                auto_join_mm: get_float("auto_join_mm", 0.05),
                auto_hatch: get_bool("auto_hatch", false),
            }
        }
        "rhaiscript" | "rhai" | "script" => PluginKind::RhaiScript {
            script_file: req_str("script_file")?,
            entry_label: get_str("entry_label").unwrap_or_else(|| name.clone()),
        },
        other => {
            return Err(ProdError::InvalidParameter(format!(
                "plugin.toml unknown kind '{other}' (expected ExternalBridge or RhaiScript)"
            )));
        }
    };

    Ok(PluginManifest {
        id,
        name,
        version,
        author,
        description,
        category,
        kind,
        source_dir: None,
        builtin: false,
    })
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for (byte_idx, ch) in line.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
        } else if in_single {
            if ch == '\'' {
                in_single = false;
            }
        } else {
            match ch {
                '"' => in_double = true,
                '\'' => in_single = true,
                '#' => return &line[..byte_idx],
                _ => {}
            }
        }
    }
    line
}

fn has_closing_bracket_outside_quotes(s: &str) -> bool {
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for ch in s.chars() {
        if in_double {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
        } else if in_single {
            if ch == '\'' {
                in_single = false;
            }
        } else {
            match ch {
                '"' => in_double = true,
                '\'' => in_single = true,
                ']' => return true,
                _ => {}
            }
        }
    }
    false
}

fn parse_toml_value(raw: &str, line_no: usize) -> ProdResult<TomlVal> {
    let v = raw.trim();
    if v.starts_with('"') || v.starts_with('\'') {
        let (s, rest) = parse_toml_quoted(v, line_no)?;
        if !rest.trim().is_empty() {
            return Err(ProdError::InvalidParameter(format!(
                "plugin.toml line {line_no}: unexpected trailing tokens after string"
            )));
        }
        return Ok(TomlVal::Str(s));
    }
    if v.starts_with('[') {
        if !v.ends_with(']') {
            return Err(ProdError::InvalidParameter(format!(
                "plugin.toml line {line_no}: unterminated array"
            )));
        }
        let inner = v[1..v.len() - 1].trim();
        let mut items = Vec::new();
        let mut cur = inner;
        while !cur.is_empty() {
            cur = cur.trim_start();
            if cur.is_empty() {
                break;
            }
            let (s, rest) = parse_toml_quoted(cur, line_no)?;
            items.push(s);
            let rest = rest.trim_start();
            if let Some(after_comma) = rest.strip_prefix(',') {
                cur = after_comma;
            } else if rest.is_empty() {
                break;
            } else {
                return Err(ProdError::InvalidParameter(format!(
                    "plugin.toml line {line_no}: expected ',' or ']' in array"
                )));
            }
        }
        return Ok(TomlVal::Array(items));
    }
    if v.eq_ignore_ascii_case("true") {
        return Ok(TomlVal::Bool(true));
    }
    if v.eq_ignore_ascii_case("false") {
        return Ok(TomlVal::Bool(false));
    }
    if let Ok(num) = v.replace('_', "").parse::<f64>() {
        if num.is_finite() {
            return Ok(TomlVal::Float(num));
        }
    }
    Err(ProdError::InvalidParameter(format!(
        "plugin.toml line {line_no}: invalid value '{v}'"
    )))
}

fn parse_toml_quoted(s: &str, line_no: usize) -> ProdResult<(String, &str)> {
    if let Some(rest) = s.strip_prefix('\'') {
        // Literal string (no escape processing — ideal for Windows paths like 'C:\RDWorksV8\RDWorksV8.exe')
        let Some(end) = rest.find('\'') else {
            return Err(ProdError::InvalidParameter(format!(
                "plugin.toml line {line_no}: unterminated single-quoted string"
            )));
        };
        return Ok((rest[..end].to_owned(), &rest[end + 1..]));
    }
    let Some(rest) = s.strip_prefix('"') else {
        return Err(ProdError::InvalidParameter(format!(
            "plugin.toml line {line_no}: expected quoted string"
        )));
    };
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '"' => return Ok((out, &rest[i + 1..])),
            '\\' => {
                let Some((_, esc)) = chars.next() else {
                    return Err(ProdError::InvalidParameter(format!(
                        "plugin.toml line {line_no}: incomplete escape sequence"
                    )));
                };
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
    }
    Err(ProdError::InvalidParameter(format!(
        "plugin.toml line {line_no}: unterminated double-quoted string"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        Affine2D, Color, CornerStyle, Fill, NodePayload, OutlineStyle, ParametricShape, SceneObject,
    };

    fn sample_laser_doc() -> Document {
        let mut doc = Document::new_a4("Laser Plugin Test");
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Red Cut Box",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 40.0,
                    height: 30.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)))
            .with_transform(Affine2D::translate(15.0, 20.0)),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Black Engrave Logo",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 12.0,
                    height: 8.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::rgba8(0, 0, 0, 255)))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(25.0, 30.0)),
            None,
        )
        .unwrap();
        doc
    }

    #[test]
    fn builtin_registry_contains_all_seven_laser_and_plotter_presets() {
        let reg = PluginRegistry::with_builtins();
        assert_eq!(reg.plugins().len(), 7);
        for id in [
            "lightburn-bridge",
            "rdworks-ruida",
            "ezcad-fiber",
            "laserdrw-m2nano",
            "lasergrbl-bridge",
            "inkcut-visicut",
            "vinyl-plotter-hpgl",
        ] {
            let p = reg
                .find(id)
                .unwrap_or_else(|| panic!("missing preset {id}"));
            assert!(p.builtin);
            p.validate().unwrap();
        }
        // EZCad fiber must enable auto_hatch
        let ezcad = reg.find("ezcad-fiber").unwrap();
        assert!(matches!(
            ezcad.kind,
            PluginKind::ExternalBridge {
                auto_hatch: true,
                export_format: PluginExportFormat::DxfR12,
                ..
            }
        ));
    }

    #[test]
    fn plugin_toml_parser_handles_windows_paths_arrays_and_comments() {
        let toml = r#"
# Custom Ruida bridge manifest
[plugin]
id = "shop-ruida-bridge"
name = "Shop Ruida Bridge"
version = "2.1.0"
author = "أحمد"
description = "Exports DXF R12 #FF0000 cuts to Ruida"
category = "Laser"
kind = "ExternalBridge"

[bridge]
target_app = "RDWorksV8"
default_paths_linux = [
    "/usr/local/bin/rdworks",
    "~/.local/bin/rdworks",
]
default_paths_windows = ['C:\RDWorksV8\RDWorksV8.exe']
args_template = ["--open", "{file}", "--dir={dir}"]
export_format = "DxfR12"
selection_only = true
auto_dedupe = true
auto_join_mm = 0.08
auto_hatch = false
"#;
        let m = PluginManifest::from_toml_str(toml).unwrap();
        assert_eq!(m.id, "shop-ruida-bridge");
        assert_eq!(m.author, "أحمد");
        assert!(m.description.contains("#FF0000"));
        assert_eq!(m.category, PluginCategory::Laser);
        match m.kind {
            PluginKind::ExternalBridge {
                target_app,
                default_paths_linux,
                default_paths_windows,
                args_template,
                export_format,
                selection_only,
                auto_dedupe,
                auto_join_mm,
                auto_hatch,
            } => {
                assert_eq!(target_app, "RDWorksV8");
                assert_eq!(default_paths_linux.len(), 2);
                assert_eq!(default_paths_windows, vec![r"C:\RDWorksV8\RDWorksV8.exe"]);
                assert_eq!(args_template, vec!["--open", "{file}", "--dir={dir}"]);
                assert_eq!(export_format, PluginExportFormat::DxfR12);
                assert!(selection_only);
                assert!(auto_dedupe);
                assert!((auto_join_mm - 0.08).abs() < 1e-9);
                assert!(!auto_hatch);
            }
            PluginKind::RhaiScript { .. } => panic!("expected ExternalBridge"),
        }
    }

    #[test]
    fn discovery_rejects_path_traversal_and_oversized_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // 1. Malicious Rhai script escaping plugin dir via `../`
        let bad_dir = root.join("evil-traversal");
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(
            bad_dir.join("plugin.json"),
            r#"{
              "id": "evil-traversal",
              "name": "Evil",
              "version": "1.0.0",
              "author": "x",
              "description": "bad",
              "category": "Custom",
              "kind": { "RhaiScript": { "script_file": "../../etc/passwd.rhai", "entry_label": "Run" } }
            }"#,
        )
        .unwrap();

        // 2. Malicious plugin ID containing `..`
        let bad_id_dir = root.join("bad-id");
        fs::create_dir_all(&bad_id_dir).unwrap();
        fs::write(
            bad_id_dir.join("plugin.toml"),
            "id = \"../escape\"\nname = \"Bad\"\nkind = \"RhaiScript\"\nscript_file = \"ok.rhai\"\n",
        )
        .unwrap();

        // 3. Oversized manifest (> 256 KiB)
        let huge_dir = root.join("huge-manifest");
        fs::create_dir_all(&huge_dir).unwrap();
        fs::write(huge_dir.join("plugin.json"), vec![b' '; 300 * 1024]).unwrap();

        // 4. Valid TOML plugin
        let good_dir = root.join("valid-plotter");
        fs::create_dir_all(&good_dir).unwrap();
        fs::write(
            good_dir.join("plugin.toml"),
            "id = \"valid-plotter\"\nname = \"Valid Plotter\"\ncategory = \"Plotter\"\nkind = \"ExternalBridge\"\ntarget_app = \"inkcut\"\nexport_format = \"PltHpgl\"\n",
        )
        .unwrap();

        let found = discover_plugins(&[root.to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "valid-plotter");
    }

    #[test]
    fn sample_plugin_template_installs_discovers_and_executes_safely() {
        let tmp = tempfile::tempdir().unwrap();
        let installed_dir = install_sample_plugin_template(tmp.path()).unwrap();
        assert!(installed_dir.join("plugin.json").is_file());
        assert!(installed_dir.join("macro.rhai").is_file());

        let found = discover_plugins(&[tmp.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "custom-laser-example");

        let mut doc = sample_laser_doc();
        let res = run_rhai_plugin(
            &mut doc,
            &found[0],
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
        .unwrap();
        assert!(res.commands_executed >= 1);
        assert!(doc.pages()[0]
            .layers
            .iter()
            .any(|l| l.is_cut_layer && !l.objects.is_empty()));
    }

    #[test]
    fn bridge_arg_substitution_is_shell_injection_safe_and_export_works_for_all_presets() {
        // Filenames with shell metacharacters & Arabic UTF-8 remain literal single argv elements!
        let tricky = Path::new("/tmp/درع الشكر; rm -rf ~ $(whoami) `id`.dxf");
        let argv = substitute_bridge_args(
            &[
                "--open".to_owned(),
                "{file}".to_owned(),
                "--dir={dir}".to_owned(),
                "{stem}".to_owned(),
            ],
            tricky,
        )
        .unwrap();
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[0], "--open");
        assert_eq!(argv[1], "/tmp/درع الشكر; rm -rf ~ $(whoami) `id`.dxf");
        assert_eq!(argv[2], "--dir=/tmp");
        assert_eq!(argv[3], "درع الشكر; rm -rf ~ $(whoami) `id`");

        // Null bytes must be rejected
        assert!(
            substitute_bridge_args(&["{file}".to_owned()], Path::new("/tmp/a\0b.dxf")).is_err()
        );

        // Export for every built-in preset
        let tmp = tempfile::tempdir().unwrap();
        let doc = sample_laser_doc();
        let mut cfg = LaserJobConfig::default();
        cfg.rules[2].hatch_spacing_mm = 1.0;

        let reg = PluginRegistry::with_builtins();
        for preset in reg.plugins() {
            let ext = match preset.kind {
                PluginKind::ExternalBridge { export_format, .. } => export_format.file_extension(),
                PluginKind::RhaiScript { .. } => continue,
            };
            let out_file = tmp.path().join(format!("{}.{ext}", preset.id));
            let outcome = export_for_plugin(&doc, 0, None, preset, &cfg, &out_file).unwrap();
            assert!(outcome.output_path.is_file());
            assert!(outcome.bytes_written > 20);
        }

        // Executable detection respects override path
        let fake_exe = tmp.path().join("mock_lightburn");
        fs::write(&fake_exe, b"#!/bin/sh\n").unwrap();
        let lb = reg.find("lightburn-bridge").unwrap();
        assert_eq!(detect_executable(lb, Some(&fake_exe)), Some(fake_exe));
    }
}
