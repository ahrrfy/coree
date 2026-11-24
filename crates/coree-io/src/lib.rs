// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Hardened vector, print, CAD/plotter, raster, palette, and native `.coree` I/O
//! pipelines with atomic writes and Zip-Bomb / Zip-Slip / depth / dimension safeguards.

pub mod atomic;
pub mod cad_plot;
pub mod clipboard_codec;
pub mod eps;
pub mod error;
pub mod native;
pub mod palette_io;
pub mod pdf;
pub mod pdf_import;
pub mod psd;
pub mod raster;
pub mod svg;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-io";

// Ergonomic top-level re-exports
pub use atomic::{atomic_write_bytes, atomic_write_with};
pub use cad_plot::{
    export_dxf, export_dxf_to_string, export_dxf_to_string_with_options, export_dxf_with_options,
    export_plt, export_plt_to_string, export_plt_to_string_with_options, export_plt_with_options,
    import_dxf, import_dxf_bytes, import_plt, import_plt_bytes, DxfExportOptions, PltExportOptions,
    HPGL_UNITS_PER_MM,
};
pub use clipboard_codec::{
    build_clipboard_document, decode_clipboard_auto, decode_clipboard_emf, decode_clipboard_pdf,
    decode_clipboard_png, decode_clipboard_rgba, decode_clipboard_svg, encode_clipboard_emf,
    encode_clipboard_payload, encode_clipboard_pdf, encode_clipboard_png_rgba,
    encode_clipboard_svg, ClipboardDecodedFormat, ClipboardPayload, MIME_EMF, MIME_PDF, MIME_PNG,
    MIME_SVG, MIME_TEXT_PLAIN,
};
pub use eps::{export_eps, export_eps_to_string, import_eps, import_eps_bytes};
pub use error::{sanitize_zip_path, IoError, IoLimits, IoResult};
pub use native::{
    load_coree, load_coree_from_reader, load_coree_with_limits, read_coree_manifest,
    read_coree_preview, save_coree, save_coree_with_preview, CoreeManifest, COREE_FORMAT_VERSION,
};
pub use palette_io::{
    export_ase, export_gpl, export_pal, export_palette_file, import_ase, import_gpl, import_pal,
    import_palette_file,
};
pub use pdf::{export_pdf, export_pdf_to_bytes, PdfColorSpace, PdfExportOptions};
pub use pdf_import::{import_pdf, import_pdf_bytes};
pub use psd::{import_psd, import_psd_bytes};
pub use raster::{
    decode_raster_bytes_with_limits, export_raster, export_raster_to_bytes, import_raster,
    import_raster_with_limits, inject_png_phys_chunk, parse_png_phys_dpi,
    validate_raster_dimensions, RasterExportOptions, RasterFormat,
};
pub use svg::{
    export_svg, export_svg_to_string, import_svg, import_svg_str, import_svg_str_with_limits,
    import_svg_with_limits,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_metadata_is_valid() {
        assert_eq!(CRATE_NAME, "coree-io");
    }
}
