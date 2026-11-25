// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Print-production, prepress, cut/laser/packaging, variable-data print merge,
//! barcode/QR, advanced find & replace, collect-for-output, and sandboxed Rhai
//! automation engine for `coree`.

pub mod barcode;
pub mod collect;
pub mod cut_packaging;
pub mod error;
pub mod find_replace;
pub mod laser_plugin;
pub mod macros;
pub mod plugins;
pub mod preflight;
pub mod prepress;
pub mod print_merge;
mod scene_helpers;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-prod";

// Ergonomic top-level re-exports for downstream crates (`coree-app`, CLI, integrations)
pub use barcode::{
    add_barcode_to_document, add_qr_code_to_document, ean13_check_digit, ean8_check_digit,
    generate_barcode, generate_qr_code, upca_check_digit, BarcodeOptions, BarcodeOutput,
    BarcodeSymbology, Code128Mode, QrCodeOptions, QrCodeOutput, QrErrorCorrection,
};
pub use collect::{
    collect_for_output, fs_type_allows_embedding, fs_type_label, read_sfnt_os2_fs_type,
    CollectOptions, CollectReport, CollectedFontInfo, CollectedImageInfo, PageSummary,
    ProductionManifest,
};
pub use cut_packaging::{
    compute_production_metrics, generate_cut_contour, generate_dieline, generate_fefco_dieline,
    CutContourResult, DielineKind, DielineResult, FefcoCode, FluteType, ProductionMetrics,
    DEFAULT_CREASE_SPOT_NAME, DEFAULT_CUT_SPOT_NAME,
};
pub use error::{ProdError, ProdResult};
pub use find_replace::{
    cql_matches_object, evaluate_cql_query, find_and_replace, find_objects, find_objects_cql,
    parse_cql_query, ColorMatch, CqlCompareOp, CqlExpr, CqlLiteral, FindQuery, FindReplaceReport,
    FindScope, LensKindFilter, ObjectKindFilter, ReplaceAction, TextMatchMode,
};
pub use laser_plugin::{
    apply_kerf_offset, apply_kerf_offset_multi, default_laser_layer_rules, export_laser_dxf_r12,
    export_laser_eps, export_laser_gcode, export_laser_plt, export_laser_svg,
    frame_open_laser_protocol, generate_hatch_fill, join_open_curves, plan_laser_toolpath,
    remove_duplicate_segments, GcodeDialect, LaserControllerFrame, LaserJobConfig, LaserLayerRule,
    LaserOperationKind, LaserOrigin, LaserToolpathPlan, OpenLaserProtocol, ToolpathSegment,
};
pub use macros::{
    builtin_macros, find_builtin_macro, plan_macro_ops, plan_macro_ops_with_selection,
    plan_recorded_macro_ops, run_macro, run_macro_event_hook, run_macro_event_hook_with_selection,
    run_macro_with_ops, run_macro_with_ops_and_selection, run_recorded_macro, BuiltinMacro,
    MacroEventHook, MacroExecutionResult, MacroSandboxConfig, MacroValue,
};
pub use plugins::{
    default_plugin_dirs, detect_executable, discover_plugins, dispatch_plugin_jsonrpc,
    export_for_plugin, handle_plugin_jsonrpc_line, install_sample_plugin_template,
    launch_external_bridge, plan_rhai_plugin_ops, run_rhai_plugin, run_rhai_plugin_with_ops,
    serve_plugin_jsonrpc_stream, substitute_bridge_args, PluginCategory, PluginExportFormat,
    PluginExportOutcome, PluginJsonRpcError, PluginJsonRpcRequest, PluginJsonRpcResponse,
    PluginKind, PluginManifest, PluginRegistry, MAX_BRIDGE_ARGS, MAX_PLUGINS_PER_DIR,
    MAX_PLUGIN_MANIFEST_BYTES, MAX_PLUGIN_SCRIPT_BYTES,
};
pub use preflight::{
    run_preflight, PreflightCqlRule, PreflightIssue, PreflightIssueKind, PreflightReport,
    PreflightRules, PreflightSeverity,
};
pub use prepress::{
    apply_halftone_screen, apply_trapping, booklet_signature_order, compute_separations,
    compute_separations_with_text_outliner, compute_trapping, compute_vector_separations,
    export_separated_plates_pdf, export_separated_plates_pdf_bytes, generate_halftone_preview,
    generate_print_marks, impose_document, simulate_overprint_rgba, HalftoneDotShape,
    HalftonePlate, HalftoneScreen, HalftoneSettings, ImpositionMode, PrintMarksOptions,
    PrintMarksResult, RasterPlatePrimitive, SeparationChannel, SeparationOptions, SeparationPlate,
    SeparationResult, TrapPlacement, TrapZone, TrappingResult, TrappingSettings,
    VectorPlatePrimitive, PRINT_MARKS_LAYER_NAME, TILE_MARKS_LAYER_NAME,
};
pub use print_merge::{
    apply_print_merge, stream_print_merge_csv_to_pdf, stream_print_merge_table_to_pdf,
    substitute_placeholders, PrintMergeOptions, PrintMergeTable, SerialNumberConfig,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_metadata_is_valid() {
        assert_eq!(CRATE_NAME, "coree-prod");
    }
}
