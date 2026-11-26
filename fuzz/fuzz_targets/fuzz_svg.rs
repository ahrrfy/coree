// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use coree_fuzz::fuzz_target;
use coree_io::{
    decode_clipboard_emf, import_dwg_bytes, import_dxf_bytes, import_emf_bytes, import_plt_bytes,
    import_svg_str_with_limits, import_wmf_bytes, load_coree_from_reader, IoLimits,
};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let limits = IoLimits::default();
    if let Ok(svg_utf8) = std::str::from_utf8(data) {
        let _ = import_svg_str_with_limits(svg_utf8, &limits);
    }
    let lossy = String::from_utf8_lossy(data);
    let _ = import_svg_str_with_limits(&lossy, &limits);
    let _ = load_coree_from_reader(Cursor::new(data), &limits);
    let _ = import_dxf_bytes(data, "fuzz_svg");
    let _ = import_plt_bytes(data, "fuzz_svg");
    let _ = import_dwg_bytes(data, "fuzz_svg");
    let _ = import_wmf_bytes(data, "fuzz_svg");
    let _ = import_emf_bytes(data, "fuzz_svg");
    let _ = decode_clipboard_emf(data);
});
