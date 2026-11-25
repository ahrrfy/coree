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

pub(crate) type ExportSymCtx<'a> = (
    &'a std::collections::HashMap<coree_core::ObjectId, &'a coree_core::SceneObject>,
    &'a [coree_core::SceneObject],
    coree_core::Affine2D,
);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ExportSymParentRef {
    Id(coree_core::ObjectId),
    RootIndex(usize),
}

fn sym_object_world_xf(
    id: coree_core::ObjectId,
    sym_root_xf: &coree_core::Affine2D,
    sym_map: &std::collections::HashMap<coree_core::ObjectId, &coree_core::SceneObject>,
    sym_objects: &[coree_core::SceneObject],
) -> Option<coree_core::Affine2D> {
    let mut chain = Vec::new();
    let mut cur = ExportSymParentRef::Id(id);
    let mut seen = std::collections::HashSet::new();
    while seen.insert(cur) {
        let obj = match cur {
            ExportSymParentRef::Id(oid) => sym_map.get(&oid).copied()?,
            ExportSymParentRef::RootIndex(idx) => sym_objects.get(idx)?,
        };
        chain.push(obj.transform);
        match cur {
            ExportSymParentRef::Id(oid) => {
                let parent = sym_objects.iter().enumerate().find_map(|(idx, po)| {
                    let contains = match &po.payload {
                        coree_core::NodePayload::Group(c) => c.contains(&oid),
                        coree_core::NodePayload::PowerClip { contents, .. } => {
                            contents.contains(&oid)
                        }
                        _ => false,
                    };
                    contains.then_some(if po.id.0 != 0 {
                        ExportSymParentRef::Id(po.id)
                    } else {
                        ExportSymParentRef::RootIndex(idx)
                    })
                });
                match parent {
                    Some(pid) => cur = pid,
                    None => break,
                }
            }
            ExportSymParentRef::RootIndex(_) => break,
        }
    }
    let mut xf = *sym_root_xf;
    for local in chain.into_iter().rev() {
        xf = xf.compose(&local);
    }
    xf.is_finite().then_some(xf)
}

pub(crate) fn resolve_export_world_path(
    doc: &coree_core::Document,
    path_id: coree_core::ObjectId,
    sym_ctx: Option<ExportSymCtx<'_>>,
) -> Option<coree_core::BezierPath> {
    if let Some((sym_map, sym_objects, sym_root_xf)) = sym_ctx {
        if let Some(sub) = sym_map.get(&path_id).copied() {
            let p_xf = sym_object_world_xf(path_id, &sym_root_xf, sym_map, sym_objects)?;
            let local = match &sub.payload {
                coree_core::NodePayload::Path(p) => Some(p.clone()),
                coree_core::NodePayload::Shape(s) => Some(s.to_bezier_path()),
                coree_core::NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                coree_core::NodePayload::ArtisticMedia { spine, style } => {
                    Some(coree_core::ArtisticMedia::expand_stroke(spine, style))
                }
                coree_core::NodePayload::Clone { .. } => doc
                    .resolve_clone_payload_and_style(sub)
                    .and_then(|(eff_p, _, _)| match eff_p {
                        coree_core::NodePayload::Path(p) => Some(p),
                        coree_core::NodePayload::Shape(s) => Some(s.to_bezier_path()),
                        coree_core::NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                        coree_core::NodePayload::ArtisticMedia { spine, style } => {
                            Some(coree_core::ArtisticMedia::expand_stroke(&spine, &style))
                        }
                        coree_core::NodePayload::PowerClip { container, .. } => Some(*container),
                        _ => None,
                    }),
                coree_core::NodePayload::PowerClip { container, .. } => Some((**container).clone()),
                _ => None,
            }?;
            let mut warped = if let Some(persp) = sub.perspective {
                persp.transform_bezier_path(&local)
            } else {
                local
            };
            for eff in &sub.effects {
                if let coree_core::Effect::Perspective { quad } = eff {
                    if let Some(bb) = warped.bounding_box() {
                        if let Some(h) = coree_core::Perspective3x3::from_rect_to_quad(bb, quad) {
                            warped = h.transform_bezier_path(&warped);
                        }
                    }
                }
            }
            return Some(warped.transformed(&p_xf));
        }
    }
    doc.world_path_of(path_id)
}

pub(crate) fn resolve_export_connector_endpoints(
    doc: &coree_core::Document,
    conn: &coree_core::ConnectorObject,
    base_world_xf: &coree_core::Affine2D,
    sym_ctx: Option<ExportSymCtx<'_>>,
) -> (coree_core::Point2D, coree_core::Point2D) {
    let Some((sym_map, _, sym_root_xf)) = sym_ctx else {
        return doc.resolve_connector_endpoints(conn);
    };
    let mut visiting = std::collections::HashMap::new();
    let mut visited_syms = std::collections::HashSet::new();
    let mut cache = std::collections::HashMap::new();
    coree_core::hit_test::resolve_connector_endpoints_with_sym(
        doc,
        conn,
        base_world_xf,
        Some((sym_map, sym_root_xf)),
        &mut visiting,
        &mut visited_syms,
        &mut cache,
        0,
    )
    .0
}

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
