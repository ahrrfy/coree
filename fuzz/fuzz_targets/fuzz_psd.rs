// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use coree_fuzz::fuzz_target;
use coree_io::{decode_raster_bytes_with_limits, import_psd_bytes, IoLimits};

fuzz_target!(|data: &[u8]| {
    let limits = IoLimits::default();
    let _ = import_psd_bytes(data, "fuzz_psd");
    let _ = decode_raster_bytes_with_limits(data, &limits);
});
