// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use coree_cdr::cmx::import_cmx_bytes;
use coree_cdr::cpt::import_cpt_bytes;
use coree_cdr::{
    detect_format_with_limits, extract_cdr_preview_png, import_cdr_bytes_with_limits,
    import_cdt_bytes, inspect_cdr_bytes_with_limits, CodecLimits,
};
use coree_fuzz::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = CodecLimits::default();
    let _ = detect_format_with_limits(data, &limits);
    let _ = extract_cdr_preview_png(data, &limits);
    let _ = inspect_cdr_bytes_with_limits(data, &limits);
    let _ = import_cdr_bytes_with_limits(data, &limits);
    let _ = import_cdt_bytes(data, &limits);
    let _ = import_cmx_bytes(data, &limits);
    let _ = import_cpt_bytes(data, &limits);
});
