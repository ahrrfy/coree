// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use coree_core::{DisplayTransformLut3D, IccProfile, RenderingIntent, SoftProofLut3D};
use coree_fuzz::fuzz_target;
use coree_io::palette_io::{import_acb, import_ase, import_gpl, import_pal};

fuzz_target!(|data: &[u8]| {
    if let Ok(prof) = IccProfile::from_bytes(data) {
        let _ = SoftProofLut3D::build_from_profile(&prof, RenderingIntent::RelativeColorimetric);
        let _ = DisplayTransformLut3D::build_from_profile(&prof, RenderingIntent::Perceptual);
    }
    let _ = import_ase(data);
    let _ = import_acb(data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = import_gpl(text);
        let _ = import_pal(text);
    }
});
