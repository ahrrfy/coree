// SPDX-License-Identifier: MPL-2.0
#![forbid(unsafe_code)]

//! Print-production, prepress, cut/laser/packaging, variable-data print merge,
//! barcode/QR, advanced find & replace, collect-for-output, and sandboxed Rhai
//! automation engine for `coree`.

pub mod barcode;
pub mod collect;
pub mod cut_packaging;
pub mod error;
pub mod find_replace;
pub mod macros;
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
    BarcodeSymbology, QrCodeOptions, QrCodeOutput, QrErrorCorrection,
};
pub use collect::{
    collect_for_output, CollectOptions, CollectReport, CollectedFontInfo, CollectedImageInfo,
    PageSummary, ProductionManifest,
};
pub use cut_packaging::{
    compute_production_metrics, generate_cut_contour, generate_dieline, CutContourResult,
    DielineKind, DielineResult, ProductionMetrics, DEFAULT_CREASE_SPOT_NAME, DEFAULT_CUT_SPOT_NAME,
};
pub use error::{ProdError, ProdResult};
pub use find_replace::{
    find_and_replace, find_objects, ColorMatch, FindQuery, FindReplaceReport, FindScope,
    LensKindFilter, ObjectKindFilter, ReplaceAction, TextMatchMode,
};
pub use macros::{
    builtin_macros, find_builtin_macro, run_macro, run_recorded_macro, BuiltinMacro,
    MacroExecutionResult, MacroSandboxConfig, MacroValue,
};
pub use preflight::{
    run_preflight, PreflightIssue, PreflightIssueKind, PreflightReport, PreflightRules,
    PreflightSeverity,
};
pub use prepress::{
    booklet_signature_order, compute_separations, generate_print_marks, impose_document,
    simulate_overprint_rgba, ImpositionMode, PrintMarksOptions, PrintMarksResult,
    SeparationChannel, SeparationOptions, SeparationPlate, SeparationResult,
    PRINT_MARKS_LAYER_NAME, TILE_MARKS_LAYER_NAME,
};
pub use print_merge::{
    apply_print_merge, substitute_placeholders, PrintMergeOptions, PrintMergeTable,
    SerialNumberConfig,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_metadata_is_valid() {
        assert_eq!(CRATE_NAME, "coree-prod");
    }
}
