// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use coree_fuzz::fuzz_target;
use coree_io::import_psd_bytes;

fuzz_target!(|data: &[u8]| {
    let _ = import_psd_bytes(data, "fuzz_psd");
});
