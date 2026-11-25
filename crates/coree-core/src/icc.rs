// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust ICC v2/v4 Color Management Engine.
//!
//! Provides:
//! - Strictly bounded ICC v2/v4 binary profile parsing ([`IccProfile::from_bytes`])
//!   supporting 128-byte headers, tag tables, `desc`/`mluc` names, `wtpt` white point,
//!   `rXYZ`/`gXYZ`/`bXYZ` primaries, `curv` & `para` (types 0–4) tone response curves,
//!   and `mft1` (8-bit) & `mft2` (16-bit) 3D/4D LUTs with tetrahedral/quadrilinear interpolation.
//! - Clean-room mathematical built-in profiles ([`BuiltinIccProfile::Srgb`],
//!   [`BuiltinIccProfile::DisplayP3`], [`BuiltinIccProfile::IsoCoatedV2Cmyk`],
//!   [`BuiltinIccProfile::UsWebCoatedSwopCmyk`]) using CIE D50 PCS, Bradford chromatic
//!   adaptation, and a 16-primary Yule-Nielsen Neugebauer offset-press model with GCR/TAC.
//! - ICC color conversions, live CMYK soft-proofing, out-of-gamut detection, and a
//!   cached 3D tetrahedral LUT accelerator for real-time framebuffer soft-proofing.

use crate::error::{CoreError, CoreResult};
use crate::math::sanitize_f32;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Maximum accepted binary ICC profile size (16 MiB) to guard against DoS/OOM.
pub const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum number of tags permitted in an ICC profile tag table.
const MAX_ICC_TAG_COUNT: usize = 1024;

/// Maximum number of 1D LUT entries in `curv` or `mft2` tables.
const MAX_ICC_CURVE_ENTRIES: usize = 65_536;

/// Maximum total scalar entries permitted inside an ICC `mft1`/`mft2` CLUT.
const MAX_ICC_CLUT_ENTRIES: usize = 2_000_000;

/// Maximum grid points per axis in an ICC CLUT (`mft1`/`mft2`).
const MAX_ICC_GRID_POINTS: usize = 65;

/// Maximum byte length decoded from any single ICC `desc`/`mluc` string record (64 KiB).
const MAX_ICC_DESC_BYTES: usize = 65_536;

/// Maximum cumulative bytes scanned across all records in a single `mluc` tag (256 KiB).
const MAX_ICC_MLUC_TOTAL_SCAN_BYTES: usize = 262_144;

/// Standard ICC PCS D50 reference white (CIE 1931 2°).
pub const D50_WHITE: [f64; 3] = [0.964_22, 1.000_00, 0.825_21];

/// Standard CIE D65 reference white (sRGB / Display P3).
pub const D65_WHITE: [f64; 3] = [0.950_47, 1.000_00, 1.088_83];

/// Standard Bradford cone-response matrix (`CIE XYZ -> LMS`).
pub const BRADFORD_M: [[f64; 3]; 3] = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// Inverse Bradford cone-response matrix (`LMS -> CIE XYZ`).
pub const BRADFORD_M_INV: [[f64; 3]; 3] = [
    [0.986_992_905_5, -0.147_054_256_4, 0.159_962_651_7],
    [0.432_305_269_7, 0.518_360_271_5, 0.049_291_228_2],
    [-0.008_528_664_6, 0.040_042_821_7, 0.968_486_695_8],
];

/// Computes the 3x3 Bradford chromatic adaptation matrix adapting `src_wp` to `dst_wp`.
#[must_use]
pub fn bradford_adaptation_matrix(src_wp: [f64; 3], dst_wp: [f64; 3]) -> [[f64; 3]; 3] {
    let src_lms = mat3_mul_vec(&BRADFORD_M, src_wp.map(sanitize_finite));
    let dst_lms = mat3_mul_vec(&BRADFORD_M, dst_wp.map(sanitize_finite));
    let scale = [
        if src_lms[0].abs() > 1e-9 {
            dst_lms[0] / src_lms[0]
        } else {
            1.0
        },
        if src_lms[1].abs() > 1e-9 {
            dst_lms[1] / src_lms[1]
        } else {
            1.0
        },
        if src_lms[2].abs() > 1e-9 {
            dst_lms[2] / src_lms[2]
        } else {
            1.0
        },
    ];
    let scaled_m = [
        [
            scale[0] * BRADFORD_M[0][0],
            scale[0] * BRADFORD_M[0][1],
            scale[0] * BRADFORD_M[0][2],
        ],
        [
            scale[1] * BRADFORD_M[1][0],
            scale[1] * BRADFORD_M[1][1],
            scale[1] * BRADFORD_M[1][2],
        ],
        [
            scale[2] * BRADFORD_M[2][0],
            scale[2] * BRADFORD_M[2][1],
            scale[2] * BRADFORD_M[2][2],
        ],
    ];
    mat3_mul_mat3(&BRADFORD_M_INV, &scaled_m)
}

/// Adapts a CIE XYZ tristimulus value from `src_wp` to `dst_wp` via Bradford chromatic adaptation.
#[must_use]
pub fn adapt_xyz_bradford(xyz: [f64; 3], src_wp: [f64; 3], dst_wp: [f64; 3]) -> [f64; 3] {
    let mat = bradford_adaptation_matrix(src_wp, dst_wp);
    mat3_mul_vec(&mat, xyz.map(sanitize_finite))
}

/// CIE Lab constants (`(6/29)^3` and `(29/3)^3`).
const LAB_EPSILON: f64 = 216.0 / 24_389.0;
const LAB_KAPPA: f64 = 24_389.0 / 27.0;

/// Delta-E76 threshold above which an sRGB color is flagged as out of CMYK press gamut.
const CMYK_GAMUT_DELTA_E_THRESHOLD: f64 = 4.5;

/// Built-in 3D soft-proof LUT resolution (`17^3 = 4913` nodes, < 2ms one-time init).
const SOFT_PROOF_LUT_GRID: usize = 17;

/// ICC rendering intent controlling out-of-gamut compression and white-point adaptation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RenderingIntent {
    #[default]
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

impl RenderingIntent {
    /// Decodes the 32-bit ICC header rendering intent field.
    #[must_use]
    pub const fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::Perceptual),
            1 => Some(Self::RelativeColorimetric),
            2 => Some(Self::Saturation),
            3 => Some(Self::AbsoluteColorimetric),
            _ => None,
        }
    }

    /// Encodes as the standard ICC 32-bit integer value (`0..=3`).
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Perceptual => 0,
            Self::RelativeColorimetric => 1,
            Self::Saturation => 2,
            Self::AbsoluteColorimetric => 3,
        }
    }
}

/// Built-in clean-room mathematical ICC color profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BuiltinIccProfile {
    /// Standard sRGB (IEC 61966-2-1, D65 Bradford-adapted to D50 PCS).
    Srgb,
    /// Wide-gamut Display P3 (D65 Bradford-adapted to D50 PCS).
    DisplayP3,
    /// Sheetfed glossy coated CMYK press model (ISO 12647-2 coated class, TAC 330%).
    #[default]
    IsoCoatedV2Cmyk,
    /// Web offset coated CMYK press model (SWOP coated class, TAC 300%).
    UsWebCoatedSwopCmyk,
}

impl BuiltinIccProfile {
    /// All built-in clean-room ICC profiles.
    pub const ALL: [Self; 4] = [
        Self::Srgb,
        Self::DisplayP3,
        Self::IsoCoatedV2Cmyk,
        Self::UsWebCoatedSwopCmyk,
    ];

    /// Human-readable display name for UI dropdowns and preflight reports.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB IEC61966-2.1 (Clean-Room)",
            Self::DisplayP3 => "Display P3 (Clean-Room)",
            Self::IsoCoatedV2Cmyk => "ISO Coated v2 CMYK (Clean-Room)",
            Self::UsWebCoatedSwopCmyk => "US Web Coated SWOP CMYK (Clean-Room)",
        }
    }

    /// Returns `true` if this built-in profile models a 4-channel CMYK print press.
    #[must_use]
    pub const fn is_cmyk(self) -> bool {
        matches!(self, Self::IsoCoatedV2Cmyk | Self::UsWebCoatedSwopCmyk)
    }

    /// Resolves any profile to a CMYK press profile (falling back to `IsoCoatedV2Cmyk` for RGB profiles).
    #[must_use]
    const fn as_cmyk_press(self) -> CmykPressKind {
        match self {
            Self::UsWebCoatedSwopCmyk => CmykPressKind::UsWebCoatedSwop,
            Self::IsoCoatedV2Cmyk | Self::Srgb | Self::DisplayP3 => CmykPressKind::IsoCoatedV2,
        }
    }
}

/// ICC device color space signature (`bytes 16..20` of header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IccColorSpace {
    Rgb,
    Cmyk,
    Gray,
    Lab,
    Xyz,
    Unknown([u8; 4]),
}

impl IccColorSpace {
    /// Parses a 4-byte ICC color space signature.
    #[must_use]
    pub const fn from_fourcc(sig: [u8; 4]) -> Self {
        match &sig {
            b"RGB " => Self::Rgb,
            b"CMYK" => Self::Cmyk,
            b"GRAY" => Self::Gray,
            b"Lab " => Self::Lab,
            b"XYZ " => Self::Xyz,
            _ => Self::Unknown(sig),
        }
    }

    /// Number of device channels for known color spaces.
    #[must_use]
    pub const fn channel_count(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb | Self::Lab | Self::Xyz => 3,
            Self::Cmyk => 4,
            Self::Unknown(_) => 0,
        }
    }
}

/// ICC Profile Connection Space (`bytes 20..24` of header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IccPcs {
    Xyz,
    Lab,
    Unknown([u8; 4]),
}

impl IccPcs {
    /// Parses a 4-byte ICC PCS signature.
    #[must_use]
    pub const fn from_fourcc(sig: [u8; 4]) -> Self {
        match &sig {
            b"XYZ " => Self::Xyz,
            b"Lab " => Self::Lab,
            _ => Self::Unknown(sig),
        }
    }
}

/// ICC Profile/Device class (`bytes 12..16` of header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IccProfileClass {
    Input,
    Display,
    Output,
    DeviceLink,
    ColorSpace,
    Abstract,
    NamedColor,
    Unknown([u8; 4]),
}

impl IccProfileClass {
    /// Parses a 4-byte ICC profile class signature.
    #[must_use]
    pub const fn from_fourcc(sig: [u8; 4]) -> Self {
        match &sig {
            b"scnr" => Self::Input,
            b"mntr" => Self::Display,
            b"prtr" => Self::Output,
            b"link" => Self::DeviceLink,
            b"spac" => Self::ColorSpace,
            b"abst" => Self::Abstract,
            b"nmcl" => Self::NamedColor,
            _ => Self::Unknown(sig),
        }
    }
}

/// Parsed 128-byte ICC v2/v4 profile header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IccProfileHeader {
    pub profile_size: u32,
    pub cmm_type: [u8; 4],
    pub version_major: u8,
    pub version_minor: u8,
    pub version_bugfix: u8,
    pub profile_class: IccProfileClass,
    pub color_space: IccColorSpace,
    pub pcs: IccPcs,
    pub rendering_intent: RenderingIntent,
    pub pcs_illuminant: [f64; 3],
}

/// Raw tag directory entry in an ICC binary profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IccTagEntry {
    pub signature: [u8; 4],
    pub offset: u32,
    pub size: u32,
}

/// Parsed ICC tone response curve (`curv` gamma/table or `para` parametric types 0..=4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IccToneCurve {
    /// Linear identity (`y = x`).
    Identity,
    /// Pure power curve (`y = x^gamma`).
    Gamma(f64),
    /// 1D piecewise-linear lookup table over `[0.0, 1.0]`.
    Table(Vec<f64>),
    /// ICC v4 parametric curve (`para`) supporting function types `0..=4`.
    Parametric { func_type: u16, params: [f64; 7] },
}

impl IccToneCurve {
    /// Standard IEC 61966-2-1 sRGB electro-optical transfer curve (`para` type 3).
    #[must_use]
    pub const fn srgb() -> Self {
        Self::Parametric {
            func_type: 3,
            params: [
                2.4,
                1.0 / 1.055,
                0.055 / 1.055,
                1.0 / 12.92,
                0.04045,
                0.0,
                0.0,
            ],
        }
    }

    /// Evaluates the forward TRC mapping device value `x` in `[0.0, 1.0]` to linear light.
    #[must_use]
    pub fn eval(&self, x: f64) -> f64 {
        let x = if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match self {
            Self::Identity => x,
            Self::Gamma(g) => {
                let g = if g.is_finite() && *g > 1e-6 { *g } else { 1.0 };
                x.powf(g).clamp(0.0, 1.0)
            }
            Self::Table(tbl) => eval_1d_table(tbl, x),
            Self::Parametric { func_type, params } => {
                let [g, a, b, c, d, e, f] = *params;
                let y = match *func_type {
                    0 => x.powf(g),
                    1 => {
                        if a.abs() > 1e-12 && x >= -b / a {
                            (a * x + b).max(0.0).powf(g)
                        } else {
                            0.0
                        }
                    }
                    2 => {
                        if a.abs() > 1e-12 && x >= -b / a {
                            (a * x + b).max(0.0).powf(g) + c
                        } else {
                            c
                        }
                    }
                    3 => {
                        if x >= d {
                            (a * x + b).max(0.0).powf(g)
                        } else {
                            c * x
                        }
                    }
                    4 => {
                        if x >= d {
                            (a * x + b).max(0.0).powf(g) + e
                        } else {
                            c * x + f
                        }
                    }
                    _ => x,
                };
                if y.is_finite() {
                    y.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
        }
    }

    /// Evaluates the inverse TRC mapping linear light `y` in `[0.0, 1.0]` back to device space.
    #[must_use]
    pub fn eval_inverse(&self, y: f64) -> f64 {
        let y = if y.is_finite() {
            y.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match self {
            Self::Identity => y,
            Self::Gamma(g) => {
                let g = if g.is_finite() && *g > 1e-6 { *g } else { 1.0 };
                y.powf(1.0 / g).clamp(0.0, 1.0)
            }
            Self::Table(tbl) => {
                if tbl.len() < 2 {
                    return y;
                }
                // Handle monotonic increasing tables cleanly with binary search, fallback to scan
                let n = tbl.len();
                if y <= tbl[0] {
                    return 0.0;
                }
                if y >= tbl[n - 1] {
                    return 1.0;
                }
                for i in 0..(n - 1) {
                    let y0 = tbl[i];
                    let y1 = tbl[i + 1];
                    if (y >= y0 && y <= y1) || (y >= y1 && y <= y0) {
                        let span = y1 - y0;
                        let frac = if span.abs() > 1e-12 {
                            ((y - y0) / span).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        return ((i as f64) + frac) / ((n - 1) as f64);
                    }
                }
                y
            }
            Self::Parametric { .. } => {
                // Monotonic bisection on [0.0, 1.0] (40 iterations => < 1e-12 precision)
                let mut lo = 0.0_f64;
                let mut hi = 1.0_f64;
                for _ in 0..40 {
                    let mid = 0.5 * (lo + hi);
                    if self.eval(mid) < y {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                0.5 * (lo + hi)
            }
        }
    }
}

/// Parsed ICC multi-dimensional lookup table (`mft1` 8-bit or `mft2` 16-bit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IccLut {
    pub in_channels: usize,
    pub out_channels: usize,
    pub grid_points: usize,
    pub is_16bit: bool,
    /// Optional 3×3 matrix (applied only when input is 3-channel XYZ PCS per ICC spec).
    pub matrix: [[f64; 3]; 3],
    /// Per-input-channel 1D pre-shaper tables (normalized `0.0..=1.0`).
    pub input_tables: Vec<Vec<f64>>,
    /// Flattened `grid_points^in_channels * out_channels` CLUT (normalized `0.0..=1.0`).
    pub clut: Vec<f64>,
    /// Per-output-channel 1D post-shaper tables (normalized `0.0..=1.0`).
    pub output_tables: Vec<Vec<f64>>,
}

impl IccLut {
    /// Evaluates this `mft1`/`mft2` LUT for `input` channels in `0.0..=1.0`.
    ///
    /// Uses 3D **tetrahedral interpolation** for 3-channel inputs and 4D **quadrilinear
    /// interpolation** for 4-channel inputs (plus 1D/2D interpolation for 1–2 channels).
    #[must_use]
    pub fn evaluate(&self, input: &[f64], apply_matrix: bool) -> Vec<f64> {
        if self.in_channels == 0
            || self.out_channels == 0
            || self.grid_points < 2
            || input.len() < self.in_channels
        {
            return vec![0.0; self.out_channels];
        }

        let mut x = [0.0_f64; 4];
        for i in 0..self.in_channels.min(4) {
            x[i] = if input[i].is_finite() {
                input[i].clamp(0.0, 1.0)
            } else {
                0.0
            };
        }

        // Per ICC spec, the 3x3 matrix applies only to 3-channel XYZ inputs
        if apply_matrix && self.in_channels == 3 {
            let m = &self.matrix;
            let nx = [
                clamp_unit_finite(m[0][0] * x[0] + m[0][1] * x[1] + m[0][2] * x[2]),
                clamp_unit_finite(m[1][0] * x[0] + m[1][1] * x[1] + m[1][2] * x[2]),
                clamp_unit_finite(m[2][0] * x[0] + m[2][1] * x[1] + m[2][2] * x[2]),
            ];
            x[0] = nx[0];
            x[1] = nx[1];
            x[2] = nx[2];
        }

        // 1. Input 1D shaper tables
        for (i, slot) in x.iter_mut().enumerate().take(self.in_channels.min(4)) {
            if let Some(tbl) = self.input_tables.get(i) {
                *slot = eval_1d_table(tbl, *slot);
            }
        }

        // 2. Multi-dimensional CLUT interpolation
        let mut mid = match self.in_channels {
            1 => self.interp_1d(x[0]),
            2 => self.interp_2d(x[0], x[1]),
            3 => self.interp_3d_tetrahedral(x[0], x[1], x[2]),
            4 => self.interp_4d_quadrilinear(x[0], x[1], x[2], x[3]),
            _ => vec![0.0; self.out_channels],
        };

        // 3. Output 1D shaper tables
        for (i, val) in mid.iter_mut().enumerate() {
            if let Some(tbl) = self.output_tables.get(i) {
                *val = eval_1d_table(tbl, *val);
            }
        }

        mid
    }

    #[inline]
    fn clut_slice(&self, flat_node: usize) -> &[f64] {
        let off = flat_node * self.out_channels;
        if off + self.out_channels <= self.clut.len() {
            &self.clut[off..off + self.out_channels]
        } else {
            &[]
        }
    }

    fn interp_1d(&self, x0: f64) -> Vec<f64> {
        let g = self.grid_points;
        let s = x0.clamp(0.0, 1.0) * ((g - 1) as f64);
        let i0 = (s.floor() as usize).min(g - 1);
        let i1 = (i0 + 1).min(g - 1);
        let t = s - (i0 as f64);
        let a = self.clut_slice(i0);
        let b = self.clut_slice(i1);
        (0..self.out_channels)
            .map(|c| {
                let va = a.get(c).copied().unwrap_or(0.0);
                let vb = b.get(c).copied().unwrap_or(0.0);
                (va + (vb - va) * t).clamp(0.0, 1.0)
            })
            .collect()
    }

    fn interp_2d(&self, x0: f64, x1: f64) -> Vec<f64> {
        let g = self.grid_points;
        let s0 = x0.clamp(0.0, 1.0) * ((g - 1) as f64);
        let s1 = x1.clamp(0.0, 1.0) * ((g - 1) as f64);
        let i0 = (s0.floor() as usize).min(g - 1);
        let j0 = (s1.floor() as usize).min(g - 1);
        let i1 = (i0 + 1).min(g - 1);
        let j1 = (j0 + 1).min(g - 1);
        let f0 = s0 - (i0 as f64);
        let f1 = s1 - (j0 as f64);

        let c00 = self.clut_slice(i0 * g + j0);
        let c10 = self.clut_slice(i1 * g + j0);
        let c01 = self.clut_slice(i0 * g + j1);
        let c11 = self.clut_slice(i1 * g + j1);

        let w00 = (1.0 - f0) * (1.0 - f1);
        let w10 = f0 * (1.0 - f1);
        let w01 = (1.0 - f0) * f1;
        let w11 = f0 * f1;

        (0..self.out_channels)
            .map(|c| {
                (c00.get(c).copied().unwrap_or(0.0) * w00
                    + c10.get(c).copied().unwrap_or(0.0) * w10
                    + c01.get(c).copied().unwrap_or(0.0) * w01
                    + c11.get(c).copied().unwrap_or(0.0) * w11)
                    .clamp(0.0, 1.0)
            })
            .collect()
    }

    /// Exact Sakamoto 6-simplex 3D tetrahedral interpolation (preserves neutral gray axis!).
    fn interp_3d_tetrahedral(&self, x: f64, y: f64, z: f64) -> Vec<f64> {
        let g = self.grid_points;
        let sx = x.clamp(0.0, 1.0) * ((g - 1) as f64);
        let sy = y.clamp(0.0, 1.0) * ((g - 1) as f64);
        let sz = z.clamp(0.0, 1.0) * ((g - 1) as f64);
        let ix0 = (sx.floor() as usize).min(g - 1);
        let iy0 = (sy.floor() as usize).min(g - 1);
        let iz0 = (sz.floor() as usize).min(g - 1);
        let ix1 = (ix0 + 1).min(g - 1);
        let iy1 = (iy0 + 1).min(g - 1);
        let iz1 = (iz0 + 1).min(g - 1);
        let fx = sx - (ix0 as f64);
        let fy = sy - (iy0 as f64);
        let fz = sz - (iz0 as f64);

        let idx = |ix: usize, iy: usize, iz: usize| (ix * g + iy) * g + iz;
        let c000 = self.clut_slice(idx(ix0, iy0, iz0));
        let c111 = self.clut_slice(idx(ix1, iy1, iz1));

        let (wa, ca, wb, cb, wc, cc, wd, cd) = if fx >= fy {
            if fy >= fz {
                // fx >= fy >= fz
                (
                    1.0 - fx,
                    c000,
                    fx - fy,
                    self.clut_slice(idx(ix1, iy0, iz0)),
                    fy - fz,
                    self.clut_slice(idx(ix1, iy1, iz0)),
                    fz,
                    c111,
                )
            } else if fx >= fz {
                // fx >= fz >= fy
                (
                    1.0 - fx,
                    c000,
                    fx - fz,
                    self.clut_slice(idx(ix1, iy0, iz0)),
                    fz - fy,
                    self.clut_slice(idx(ix1, iy0, iz1)),
                    fy,
                    c111,
                )
            } else {
                // fz >= fx >= fy
                (
                    1.0 - fz,
                    c000,
                    fz - fx,
                    self.clut_slice(idx(ix0, iy0, iz1)),
                    fx - fy,
                    self.clut_slice(idx(ix1, iy0, iz1)),
                    fy,
                    c111,
                )
            }
        } else if fz >= fy {
            // fz >= fy >= fx
            (
                1.0 - fz,
                c000,
                fz - fy,
                self.clut_slice(idx(ix0, iy0, iz1)),
                fy - fx,
                self.clut_slice(idx(ix0, iy1, iz1)),
                fx,
                c111,
            )
        } else if fz >= fx {
            // fy >= fz >= fx
            (
                1.0 - fy,
                c000,
                fy - fz,
                self.clut_slice(idx(ix0, iy1, iz0)),
                fz - fx,
                self.clut_slice(idx(ix0, iy1, iz1)),
                fx,
                c111,
            )
        } else {
            // fy >= fx >= fz
            (
                1.0 - fy,
                c000,
                fy - fx,
                self.clut_slice(idx(ix0, iy1, iz0)),
                fx - fz,
                self.clut_slice(idx(ix1, iy1, iz0)),
                fz,
                c111,
            )
        };

        (0..self.out_channels)
            .map(|ch| {
                (wa * ca.get(ch).copied().unwrap_or(0.0)
                    + wb * cb.get(ch).copied().unwrap_or(0.0)
                    + wc * cc.get(ch).copied().unwrap_or(0.0)
                    + wd * cd.get(ch).copied().unwrap_or(0.0))
                .clamp(0.0, 1.0)
            })
            .collect()
    }

    /// 4D quadrilinear hypercube interpolation (16-corner weighted sum for CMYK -> PCS CLUTs).
    fn interp_4d_quadrilinear(&self, c: f64, m: f64, y: f64, k: f64) -> Vec<f64> {
        let g = self.grid_points;
        let coords = [c, m, y, k];
        let mut i0 = [0usize; 4];
        let mut i1 = [0usize; 4];
        let mut f = [0.0_f64; 4];
        for axis in 0..4 {
            let s = coords[axis].clamp(0.0, 1.0) * ((g - 1) as f64);
            i0[axis] = (s.floor() as usize).min(g - 1);
            i1[axis] = (i0[axis] + 1).min(g - 1);
            f[axis] = s - (i0[axis] as f64);
        }

        let mut out = vec![0.0_f64; self.out_channels];
        for corner in 0..16usize {
            let ic = if (corner & 1) == 0 { i0[0] } else { i1[0] };
            let im = if (corner & 2) == 0 { i0[1] } else { i1[1] };
            let iy = if (corner & 4) == 0 { i0[2] } else { i1[2] };
            let ik = if (corner & 8) == 0 { i0[3] } else { i1[3] };
            let wc = if (corner & 1) == 0 { 1.0 - f[0] } else { f[0] };
            let wm = if (corner & 2) == 0 { 1.0 - f[1] } else { f[1] };
            let wy = if (corner & 4) == 0 { 1.0 - f[2] } else { f[2] };
            let wk = if (corner & 8) == 0 { 1.0 - f[3] } else { f[3] };
            let w = wc * wm * wy * wk;
            if w <= 1e-12 {
                continue;
            }
            let flat = (((ic * g + im) * g + iy) * g) + ik;
            let node = self.clut_slice(flat);
            for (dst, &val) in out.iter_mut().zip(node.iter()) {
                *dst += w * val;
            }
        }
        for v in &mut out {
            *v = v.clamp(0.0, 1.0);
        }
        out
    }
}

/// Parsed ICC v4 `mAB ` (`lutAToBType`) or `mBA ` (`lutBToAType`) multi-stage LUT pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IccV4Lut {
    /// `false` for `mAB ` (Device -> PCS), `true` for `mBA ` (PCS -> Device).
    pub is_b_to_a: bool,
    pub in_channels: usize,
    pub out_channels: usize,
    pub b_curves: Vec<IccToneCurve>,
    pub m_curves: Vec<IccToneCurve>,
    /// Optional `3 × 3` matrix (cols `0..3`) plus additive bias vector (col `3`).
    pub matrix: Option<[[f64; 4]; 3]>,
    pub a_curves: Vec<IccToneCurve>,
    pub clut_grid: [usize; 4],
    pub clut_is_16bit: bool,
    pub clut: Vec<f64>,
}

impl IccV4Lut {
    /// Evaluates the full ICC v4 `mAB`/`mBA` pipeline in normalized `0.0..=1.0` coordinates.
    #[must_use]
    pub fn evaluate(&self, input: &[f64]) -> Vec<f64> {
        if self.in_channels == 0 || self.out_channels == 0 || input.len() < self.in_channels {
            return vec![0.0; self.out_channels.max(1)];
        }
        let mut cur: Vec<f64> = input[..self.in_channels]
            .iter()
            .map(|&v| clamp_unit_finite(v))
            .collect();

        if !self.is_b_to_a {
            // mAB: A-curves -> CLUT -> M-curves -> Matrix -> B-curves
            for (i, v) in cur.iter_mut().enumerate() {
                if let Some(c) = self.a_curves.get(i) {
                    *v = c.eval(*v);
                }
            }
            if !self.clut.is_empty() {
                cur = self.interp_clut(&cur);
            }
            for (i, v) in cur.iter_mut().enumerate() {
                if let Some(c) = self.m_curves.get(i) {
                    *v = c.eval(*v);
                }
            }
            if let Some(ref mat) = self.matrix {
                if cur.len() >= 3 {
                    let x = [cur[0], cur[1], cur[2]];
                    cur[0] = clamp_unit_finite(
                        mat[0][0] * x[0] + mat[0][1] * x[1] + mat[0][2] * x[2] + mat[0][3],
                    );
                    cur[1] = clamp_unit_finite(
                        mat[1][0] * x[0] + mat[1][1] * x[1] + mat[1][2] * x[2] + mat[1][3],
                    );
                    cur[2] = clamp_unit_finite(
                        mat[2][0] * x[0] + mat[2][1] * x[1] + mat[2][2] * x[2] + mat[2][3],
                    );
                }
            }
            for (i, v) in cur.iter_mut().enumerate() {
                if let Some(c) = self.b_curves.get(i) {
                    *v = c.eval(*v);
                }
            }
        } else {
            // mBA: B-curves -> Matrix -> M-curves -> CLUT -> A-curves
            for (i, v) in cur.iter_mut().enumerate() {
                if let Some(c) = self.b_curves.get(i) {
                    *v = c.eval(*v);
                }
            }
            if let Some(ref mat) = self.matrix {
                if cur.len() >= 3 {
                    let x = [cur[0], cur[1], cur[2]];
                    cur[0] = clamp_unit_finite(
                        mat[0][0] * x[0] + mat[0][1] * x[1] + mat[0][2] * x[2] + mat[0][3],
                    );
                    cur[1] = clamp_unit_finite(
                        mat[1][0] * x[0] + mat[1][1] * x[1] + mat[1][2] * x[2] + mat[1][3],
                    );
                    cur[2] = clamp_unit_finite(
                        mat[2][0] * x[0] + mat[2][1] * x[1] + mat[2][2] * x[2] + mat[2][3],
                    );
                }
            }
            for (i, v) in cur.iter_mut().enumerate() {
                if let Some(c) = self.m_curves.get(i) {
                    *v = c.eval(*v);
                }
            }
            if !self.clut.is_empty() {
                cur = self.interp_clut(&cur);
            }
            for (i, v) in cur.iter_mut().enumerate() {
                if let Some(c) = self.a_curves.get(i) {
                    *v = c.eval(*v);
                }
            }
        }

        cur.resize(self.out_channels, 0.0);
        for v in &mut cur {
            *v = clamp_unit_finite(*v);
        }
        cur
    }

    fn interp_clut(&self, x: &[f64]) -> Vec<f64> {
        let dims = x.len().min(4);
        if dims == 0 || self.clut.is_empty() {
            return vec![0.0; self.out_channels];
        }
        let mut strides = [1usize; 4];
        for axis in (0..dims).rev() {
            if axis + 1 < dims {
                strides[axis] = strides[axis + 1].saturating_mul(self.clut_grid[axis + 1].max(2));
            }
        }
        let mut i0 = [0usize; 4];
        let mut i1 = [0usize; 4];
        let mut f = [0.0_f64; 4];
        for axis in 0..dims {
            let g = self.clut_grid[axis].max(2);
            let s = clamp_unit_finite(x[axis]) * ((g - 1) as f64);
            i0[axis] = (s.floor() as usize).min(g - 1);
            i1[axis] = (i0[axis] + 1).min(g - 1);
            f[axis] = s - (i0[axis] as f64);
        }

        let corners = 1usize << dims;
        let mut out = vec![0.0_f64; self.out_channels];
        for mask in 0..corners {
            let mut w = 1.0_f64;
            let mut flat = 0usize;
            for axis in 0..dims {
                if (mask & (1 << axis)) == 0 {
                    w *= 1.0 - f[axis];
                    flat = flat.saturating_add(i0[axis].saturating_mul(strides[axis]));
                } else {
                    w *= f[axis];
                    flat = flat.saturating_add(i1[axis].saturating_mul(strides[axis]));
                }
            }
            if w <= 1e-12 {
                continue;
            }
            let off = flat.saturating_mul(self.out_channels);
            if off + self.out_channels <= self.clut.len() {
                for ch in 0..self.out_channels {
                    out[ch] += w * self.clut[off + ch];
                }
            }
        }
        for v in &mut out {
            *v = clamp_unit_finite(*v);
        }
        out
    }
}

/// Complete parsed or built-in ICC v2/v4 color profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IccProfile {
    pub header: IccProfileHeader,
    pub description: String,
    pub tags: Vec<IccTagEntry>,
    pub white_point: Option<[f64; 3]>,
    /// Optional ICC `bkpt` media black point in D50 PCS XYZ.
    #[serde(default)]
    pub black_point: Option<[f64; 3]>,
    /// Optional ICC `chad` (`sf32` 3x3) chromatic adaptation matrix (`media_wp -> D50`).
    #[serde(default)]
    pub chad: Option<[[f64; 3]; 3]>,
    /// Matrix columns `[rXYZ, gXYZ, bXYZ]` mapping linear RGB to D50 PCS XYZ.
    pub rgb_to_xyz_matrix: Option<[[f64; 3]; 3]>,
    pub r_trc: Option<IccToneCurve>,
    pub g_trc: Option<IccToneCurve>,
    pub b_trc: Option<IccToneCurve>,
    pub k_trc: Option<IccToneCurve>,
    /// Device -> PCS lookup table (`A2B0`, or fallback `A2B1`).
    pub a2b0: Option<IccLut>,
    /// PCS -> Device lookup table (`B2A0`, or fallback `B2A1`).
    pub b2a0: Option<IccLut>,
    /// Intent-indexed `mft1`/`mft2` Device -> PCS tables (`[Perceptual A2B0, Colorimetric A2B1, Saturation A2B2]`).
    #[serde(default)]
    pub a2b_intents: [Option<IccLut>; 3],
    /// Intent-indexed `mft1`/`mft2` PCS -> Device tables (`[Perceptual B2A0, Colorimetric B2A1, Saturation B2A2]`).
    #[serde(default)]
    pub b2a_intents: [Option<IccLut>; 3],
    /// Intent-indexed ICC v4 `mAB ` tables (`[Perceptual A2B0, Colorimetric A2B1, Saturation A2B2]`).
    #[serde(default)]
    pub v4_a2b: [Option<IccV4Lut>; 3],
    /// Intent-indexed ICC v4 `mBA ` tables (`[Perceptual B2A0, Colorimetric B2A1, Saturation B2A2]`).
    #[serde(default)]
    pub v4_b2a: [Option<IccV4Lut>; 3],
    /// Optional built-in analytical press tag when constructed via [`IccProfile::builtin`].
    pub builtin_kind: Option<BuiltinIccProfile>,
}

impl IccProfile {
    /// Parses a real ICC v2/v4 binary profile from `bytes` with strict bounds checks.
    pub fn from_bytes(bytes: &[u8]) -> CoreResult<Self> {
        if bytes.len() < 132 {
            return Err(CoreError::InvalidColor(format!(
                "ICC profile too short ({} bytes, minimum 132)",
                bytes.len()
            )));
        }
        if bytes.len() > MAX_ICC_PROFILE_BYTES {
            return Err(CoreError::InvalidColor(format!(
                "ICC profile exceeds maximum safe size ({} > {MAX_ICC_PROFILE_BYTES} bytes)",
                bytes.len()
            )));
        }
        if &bytes[36..40] != b"acsp" {
            return Err(CoreError::InvalidColor(
                "invalid ICC profile signature (expected 'acsp' at offset 36)".into(),
            ));
        }

        let profile_size = read_be_u32(bytes, 0)?;
        if (profile_size as usize) < 132 || (profile_size as usize) > bytes.len() {
            return Err(CoreError::InvalidColor(format!(
                "ICC profile size {profile_size} out of bounds for buffer of {} bytes",
                bytes.len()
            )));
        }
        let bytes = &bytes[..profile_size as usize];

        let cmm_type = read_fourcc(bytes, 4)?;
        let version_major = bytes[8];
        let version_minor = bytes[9] >> 4;
        let version_bugfix = bytes[9] & 0x0F;
        let profile_class = IccProfileClass::from_fourcc(read_fourcc(bytes, 12)?);
        let color_space = IccColorSpace::from_fourcc(read_fourcc(bytes, 16)?);
        let pcs = IccPcs::from_fourcc(read_fourcc(bytes, 20)?);

        let intent_raw = read_be_u32(bytes, 64)? & 0xFFFF;
        let rendering_intent = RenderingIntent::from_u32(intent_raw).ok_or_else(|| {
            CoreError::InvalidColor(format!("invalid ICC rendering intent {intent_raw}"))
        })?;

        let pcs_illuminant = [
            read_s15_fixed16(bytes, 68)?,
            read_s15_fixed16(bytes, 72)?,
            read_s15_fixed16(bytes, 76)?,
        ];

        let header = IccProfileHeader {
            profile_size,
            cmm_type,
            version_major,
            version_minor,
            version_bugfix,
            profile_class,
            color_space,
            pcs,
            rendering_intent,
            pcs_illuminant,
        };

        // Parse tag table
        let tag_count = read_be_u32(bytes, 128)? as usize;
        if tag_count > MAX_ICC_TAG_COUNT {
            return Err(CoreError::InvalidColor(format!(
                "ICC tag count {tag_count} exceeds limit {MAX_ICC_TAG_COUNT}"
            )));
        }
        let tag_table_end = 132usize
            .checked_add(
                tag_count
                    .checked_mul(12)
                    .ok_or_else(|| CoreError::InvalidColor("ICC tag table size overflow".into()))?,
            )
            .ok_or_else(|| CoreError::InvalidColor("ICC tag table offset overflow".into()))?;
        if tag_table_end > bytes.len() {
            return Err(CoreError::InvalidColor(
                "ICC tag table extends beyond end of profile".into(),
            ));
        }

        let mut tags = Vec::with_capacity(tag_count);
        let mut tag_map: BTreeMap<[u8; 4], (usize, usize)> = BTreeMap::new();
        for i in 0..tag_count {
            let entry_off = 132 + i * 12;
            let sig = read_fourcc(bytes, entry_off)?;
            let offset = read_be_u32(bytes, entry_off + 4)?;
            let size = read_be_u32(bytes, entry_off + 8)?;
            let start = offset as usize;
            let len = size as usize;
            let end = start
                .checked_add(len)
                .ok_or_else(|| CoreError::InvalidColor("ICC tag offset + size overflow".into()))?;
            if start < 132 || end > bytes.len() {
                return Err(CoreError::InvalidColor(format!(
                    "ICC tag '{}' slice {start}..{end} out of bounds (profile len {})",
                    String::from_utf8_lossy(&sig),
                    bytes.len()
                )));
            }
            tags.push(IccTagEntry {
                signature: sig,
                offset,
                size,
            });
            tag_map.entry(sig).or_insert((start, len));
        }

        let tag_slice = |sig: &[u8; 4]| -> Option<&'_ [u8]> {
            let &(start, len) = tag_map.get(sig)?;
            bytes.get(start..start + len)
        };

        let description = tag_slice(b"desc")
            .and_then(|s| parse_desc_or_mluc_tag(s).ok())
            .unwrap_or_default();

        let white_point = tag_slice(b"wtpt").and_then(|s| parse_xyz_tag(s).ok());
        let black_point = tag_slice(b"bkpt").and_then(|s| parse_xyz_tag(s).ok());
        let chad = tag_slice(b"chad").and_then(|s| parse_sf32_chad_tag(s).ok());

        let r_xyz = tag_slice(b"rXYZ").and_then(|s| parse_xyz_tag(s).ok());
        let g_xyz = tag_slice(b"gXYZ").and_then(|s| parse_xyz_tag(s).ok());
        let b_xyz = tag_slice(b"bXYZ").and_then(|s| parse_xyz_tag(s).ok());
        let rgb_to_xyz_matrix = match (r_xyz, g_xyz, b_xyz) {
            (Some(r), Some(g), Some(b)) => {
                Some([[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]])
            }
            _ => None,
        };

        let r_trc = match tag_slice(b"rTRC") {
            Some(s) => Some(parse_trc_tag(s)?),
            None => None,
        };
        let g_trc = match tag_slice(b"gTRC") {
            Some(s) => Some(parse_trc_tag(s)?),
            None => None,
        };
        let b_trc = match tag_slice(b"bTRC") {
            Some(s) => Some(parse_trc_tag(s)?),
            None => None,
        };
        let k_trc = match tag_slice(b"kTRC") {
            Some(s) => Some(parse_trc_tag(s)?),
            None => None,
        };

        let parse_stage = |sig: &[u8; 4]| -> CoreResult<(Option<IccLut>, Option<IccV4Lut>)> {
            match tag_slice(sig) {
                Some(s) => parse_any_lut_tag(s),
                None => Ok((None, None)),
            }
        };
        let (a2b_p, v4_a2b_p) = parse_stage(b"A2B0")?;
        let (a2b_c, v4_a2b_c) = parse_stage(b"A2B1")?;
        let (a2b_s, v4_a2b_s) = parse_stage(b"A2B2")?;
        let (b2a_p, v4_b2a_p) = parse_stage(b"B2A0")?;
        let (b2a_c, v4_b2a_c) = parse_stage(b"B2A1")?;
        let (b2a_s, v4_b2a_s) = parse_stage(b"B2A2")?;

        let a2b0 = a2b_p.clone().or_else(|| a2b_c.clone());
        let b2a0 = b2a_p.clone().or_else(|| b2a_c.clone());

        Ok(Self {
            header,
            description,
            tags,
            white_point,
            black_point,
            chad,
            rgb_to_xyz_matrix,
            r_trc,
            g_trc,
            b_trc,
            k_trc,
            a2b0,
            b2a0,
            a2b_intents: [a2b_p, a2b_c, a2b_s],
            b2a_intents: [b2a_p, b2a_c, b2a_s],
            v4_a2b: [v4_a2b_p, v4_a2b_c, v4_a2b_s],
            v4_b2a: [v4_b2a_p, v4_b2a_c, v4_b2a_s],
            builtin_kind: None,
        })
    }

    /// Constructs one of the built-in clean-room mathematical ICC profiles.
    #[must_use]
    pub fn builtin(kind: BuiltinIccProfile) -> Self {
        match kind {
            BuiltinIccProfile::Srgb => Self {
                header: IccProfileHeader {
                    profile_size: 128,
                    cmm_type: *b"core",
                    version_major: 4,
                    version_minor: 3,
                    version_bugfix: 0,
                    profile_class: IccProfileClass::Display,
                    color_space: IccColorSpace::Rgb,
                    pcs: IccPcs::Xyz,
                    rendering_intent: RenderingIntent::Perceptual,
                    pcs_illuminant: D50_WHITE,
                },
                description: kind.display_name().to_owned(),
                tags: Vec::new(),
                white_point: Some(D65_WHITE),
                black_point: Some([0.0, 0.0, 0.0]),
                chad: Some(bradford_adaptation_matrix(D65_WHITE, D50_WHITE)),
                rgb_to_xyz_matrix: Some(SRGB_TO_XYZ_D50),
                r_trc: Some(IccToneCurve::srgb()),
                g_trc: Some(IccToneCurve::srgb()),
                b_trc: Some(IccToneCurve::srgb()),
                k_trc: None,
                a2b0: None,
                b2a0: None,
                a2b_intents: [None, None, None],
                b2a_intents: [None, None, None],
                v4_a2b: [None, None, None],
                v4_b2a: [None, None, None],
                builtin_kind: Some(kind),
            },
            BuiltinIccProfile::DisplayP3 => Self {
                header: IccProfileHeader {
                    profile_size: 128,
                    cmm_type: *b"core",
                    version_major: 4,
                    version_minor: 3,
                    version_bugfix: 0,
                    profile_class: IccProfileClass::Display,
                    color_space: IccColorSpace::Rgb,
                    pcs: IccPcs::Xyz,
                    rendering_intent: RenderingIntent::Perceptual,
                    pcs_illuminant: D50_WHITE,
                },
                description: kind.display_name().to_owned(),
                tags: Vec::new(),
                white_point: Some(D65_WHITE),
                black_point: Some([0.0, 0.0, 0.0]),
                chad: Some(bradford_adaptation_matrix(D65_WHITE, D50_WHITE)),
                rgb_to_xyz_matrix: Some(DISPLAY_P3_TO_XYZ_D50),
                r_trc: Some(IccToneCurve::srgb()),
                g_trc: Some(IccToneCurve::srgb()),
                b_trc: Some(IccToneCurve::srgb()),
                k_trc: None,
                a2b0: None,
                b2a0: None,
                a2b_intents: [None, None, None],
                b2a_intents: [None, None, None],
                v4_a2b: [None, None, None],
                v4_b2a: [None, None, None],
                builtin_kind: Some(kind),
            },
            BuiltinIccProfile::IsoCoatedV2Cmyk | BuiltinIccProfile::UsWebCoatedSwopCmyk => {
                let press = kind.as_cmyk_press();
                Self {
                    header: IccProfileHeader {
                        profile_size: 128,
                        cmm_type: *b"core",
                        version_major: 4,
                        version_minor: 3,
                        version_bugfix: 0,
                        profile_class: IccProfileClass::Output,
                        color_space: IccColorSpace::Cmyk,
                        pcs: IccPcs::Lab,
                        rendering_intent: RenderingIntent::RelativeColorimetric,
                        pcs_illuminant: D50_WHITE,
                    },
                    description: kind.display_name().to_owned(),
                    tags: Vec::new(),
                    white_point: Some(press.paper_white_xyz()),
                    black_point: None,
                    chad: None,
                    rgb_to_xyz_matrix: None,
                    r_trc: None,
                    g_trc: None,
                    b_trc: None,
                    k_trc: None,
                    a2b0: None,
                    b2a0: None,
                    a2b_intents: [None, None, None],
                    b2a_intents: [None, None, None],
                    v4_a2b: [None, None, None],
                    v4_b2a: [None, None, None],
                    builtin_kind: Some(kind),
                }
            }
        }
    }

    fn intent_slot(intent: RenderingIntent) -> usize {
        match intent {
            RenderingIntent::Perceptual => 0,
            RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => 1,
            RenderingIntent::Saturation => 2,
        }
    }

    fn select_a2b(&self, intent: RenderingIntent) -> Option<&IccLut> {
        let s = Self::intent_slot(intent);
        self.a2b_intents[s]
            .as_ref()
            .or(self.a2b_intents[0].as_ref())
            .or(self.a2b0.as_ref())
    }

    fn select_b2a(&self, intent: RenderingIntent) -> Option<&IccLut> {
        let s = Self::intent_slot(intent);
        self.b2a_intents[s]
            .as_ref()
            .or(self.b2a_intents[0].as_ref())
            .or(self.b2a0.as_ref())
    }

    fn select_v4_a2b(&self, intent: RenderingIntent) -> Option<&IccV4Lut> {
        let s = Self::intent_slot(intent);
        self.v4_a2b[s]
            .as_ref()
            .or(self.v4_a2b[0].as_ref())
            .or_else(|| self.v4_a2b.iter().flatten().next())
    }

    fn select_v4_b2a(&self, intent: RenderingIntent) -> Option<&IccV4Lut> {
        let s = Self::intent_slot(intent);
        self.v4_b2a[s]
            .as_ref()
            .or(self.v4_b2a[0].as_ref())
            .or_else(|| self.v4_b2a.iter().flatten().next())
    }

    #[inline]
    fn adapt_rel_xyz_for_intent(&self, xyz: [f64; 3], intent: RenderingIntent) -> [f64; 3] {
        if intent != RenderingIntent::AbsoluteColorimetric {
            return xyz;
        }
        if let Some(chad) = self.chad {
            if let Some(inv_chad) = mat3_invert(&chad) {
                return mat3_mul_vec(&inv_chad, xyz);
            }
        }
        let Some(wp) = self.white_point else {
            return xyz;
        };
        if wp[0] <= 1e-4 || wp[1] <= 1e-4 || wp[2] <= 1e-4 {
            return xyz;
        }
        [
            xyz[0] * (wp[0] / D50_WHITE[0]),
            xyz[1] * (wp[1] / D50_WHITE[1]),
            xyz[2] * (wp[2] / D50_WHITE[2]),
        ]
    }

    #[inline]
    fn adapt_abs_xyz_to_rel(&self, xyz: [f64; 3], intent: RenderingIntent) -> [f64; 3] {
        if intent != RenderingIntent::AbsoluteColorimetric {
            return xyz;
        }
        if let Some(chad) = self.chad {
            return mat3_mul_vec(&chad, xyz);
        }
        let Some(wp) = self.white_point else {
            return xyz;
        };
        if wp[0] <= 1e-4 || wp[1] <= 1e-4 || wp[2] <= 1e-4 {
            return xyz;
        }
        [
            xyz[0] * (D50_WHITE[0] / wp[0]),
            xyz[1] * (D50_WHITE[1] / wp[1]),
            xyz[2] * (D50_WHITE[2] / wp[2]),
        ]
    }

    /// Observer-adapted (`AdaptationState = 1.0`, ICC v4 / LittleCMS `-d1` default) media
    /// white point in D50 PCS.
    ///
    /// - Built-in `Srgb`/`DisplayP3` store unadapted `D65_WHITE` in `white_point` alongside
    ///   `chad = D65 -> D50`; their D50-adapted white point is `D50_WHITE`.
    /// - Profiles with a `chad` tag (ICC v4): per ICC.1:2010, `wtpt` is already chromatically
    ///   adapted into D50 PCS.
    /// - Display RGB and Gray profiles without `chad` (ICC v2 `mntr` `RGB ` / `GRAY`): `wtpt`
    ///   is the unadapted monitor illuminant, so under full observer adaptation its D50-adapted
    ///   white is `D50_WHITE`.
    /// - Output profiles without `chad` (e.g. `FOGRA39`, `SWOP`): `wtpt` is the D50-relative
    ///   paper white point.
    #[inline]
    fn adapted_white_point(&self) -> Option<[f64; 3]> {
        if matches!(
            self.builtin_kind,
            Some(BuiltinIccProfile::Srgb | BuiltinIccProfile::DisplayP3)
        ) {
            return Some(D50_WHITE);
        }
        if self.chad.is_some() {
            return self.white_point;
        }
        if self.header.profile_class == IccProfileClass::Display
            && matches!(
                self.header.color_space,
                IccColorSpace::Rgb | IccColorSpace::Gray
            )
        {
            return Some(D50_WHITE);
        }
        self.white_point
    }

    #[inline]
    fn adapt_rel_xyz_for_intent_adapted(&self, xyz: [f64; 3], intent: RenderingIntent) -> [f64; 3] {
        if intent != RenderingIntent::AbsoluteColorimetric {
            return xyz;
        }
        let Some(wp) = self.adapted_white_point() else {
            return xyz;
        };
        if wp[0] <= 1e-4 || wp[1] <= 1e-4 || wp[2] <= 1e-4 {
            return xyz;
        }
        [
            xyz[0] * (wp[0] / D50_WHITE[0]),
            xyz[1] * (wp[1] / D50_WHITE[1]),
            xyz[2] * (wp[2] / D50_WHITE[2]),
        ]
    }

    #[inline]
    fn adapt_abs_xyz_to_rel_adapted(&self, xyz: [f64; 3], intent: RenderingIntent) -> [f64; 3] {
        if intent != RenderingIntent::AbsoluteColorimetric {
            return xyz;
        }
        let Some(wp) = self.adapted_white_point() else {
            return xyz;
        };
        if wp[0] <= 1e-4 || wp[1] <= 1e-4 || wp[2] <= 1e-4 {
            return xyz;
        }
        [
            xyz[0] * (D50_WHITE[0] / wp[0]),
            xyz[1] * (D50_WHITE[1] / wp[1]),
            xyz[2] * (D50_WHITE[2] / wp[2]),
        ]
    }

    /// Detects the source black point in D50 PCS XYZ per ISO 18619 / Adobe BPC.
    #[must_use]
    pub fn detect_black_point(&self, intent: RenderingIntent) -> Option<[f64; 3]> {
        match self.header.color_space {
            IccColorSpace::Cmyk => {
                if let Some(v4) = self.select_v4_b2a(intent) {
                    if v4.in_channels == 3 && v4.out_channels == 4 {
                        let zero_ab = 128.0 / 255.0;
                        let bp_pcs = if self.header.pcs == IccPcs::Xyz {
                            [0.0, 0.0, 0.0]
                        } else {
                            [0.0, zero_ab, zero_ab]
                        };
                        let cmyk_bp = v4.evaluate(&bp_pcs);
                        if cmyk_bp.len() >= 4 {
                            if let Some(xyz) = self.cmyk_to_pcs_xyz_with_intent_bpc(
                                [cmyk_bp[0], cmyk_bp[1], cmyk_bp[2], cmyk_bp[3]],
                                intent,
                                false,
                            ) {
                                return Some(xyz);
                            }
                        }
                    }
                }
                if let Some(lut) = self.select_b2a(intent) {
                    if lut.in_channels == 3 && lut.out_channels == 4 {
                        let zero_ab = if lut.is_16bit && self.header.version_major < 4 {
                            32_768.0 / 65_535.0
                        } else {
                            128.0 / 255.0
                        };
                        let bp_pcs = if self.header.pcs == IccPcs::Xyz {
                            [0.0, 0.0, 0.0]
                        } else {
                            [0.0, zero_ab, zero_ab]
                        };
                        let cmyk_bp = lut.evaluate(&bp_pcs, self.header.pcs == IccPcs::Xyz);
                        if cmyk_bp.len() >= 4 {
                            if let Some(xyz) = self.cmyk_to_pcs_xyz_with_intent_bpc(
                                [cmyk_bp[0], cmyk_bp[1], cmyk_bp[2], cmyk_bp[3]],
                                intent,
                                false,
                            ) {
                                return Some(xyz);
                            }
                        }
                    }
                }
                if let Some(bp) = self.black_point {
                    return Some(bp);
                }
                self.cmyk_to_pcs_xyz_with_intent_bpc([1.0, 1.0, 1.0, 1.0], intent, false)
            }
            IccColorSpace::Rgb => {
                if let Some(bp) = self.black_point {
                    return Some(bp);
                }
                self.rgb_to_pcs_xyz_with_intent_bpc([0.0, 0.0, 0.0], intent, false)
            }
            IccColorSpace::Gray => {
                if let Some(bp) = self.black_point {
                    return Some(bp);
                }
                let g0 = self.gray_to_pcs_xyz_with_intent_bpc(0.0, intent, false)?;
                let g1 = self.gray_to_pcs_xyz_with_intent_bpc(1.0, intent, false)?;
                Some(if g0[1] <= g1[1] { g0 } else { g1 })
            }
            _ => self.black_point,
        }
    }

    /// Applies ISO 18619 linear XYZ Black Point Compensation relative to D50 white point.
    #[must_use]
    pub fn apply_bpc(
        xyz: [f64; 3],
        src_bp: [f64; 3],
        dst_bp_y: f64,
        intent: RenderingIntent,
        bpc: bool,
    ) -> [f64; 3] {
        if !bpc || intent == RenderingIntent::AbsoluteColorimetric {
            return xyz;
        }
        let src_y = sanitize_finite(src_bp[1]).clamp(0.0, 0.99);
        let dst_y = sanitize_finite(dst_bp_y).clamp(0.0, 0.99);
        if (src_y - dst_y).abs() < 1e-9 {
            return xyz;
        }
        let scale = (1.0 - dst_y) / (1.0 - src_y);
        let offset = 1.0 - scale;
        [
            xyz[0] * scale + D50_WHITE[0] * offset,
            xyz[1] * scale + D50_WHITE[1] * offset,
            xyz[2] * scale + D50_WHITE[2] * offset,
        ]
    }

    /// Converts device RGB (`0.0..=1.0`) to D50 PCS CIE XYZ using this profile's `A2B*` LUT or TRC+Matrix.
    #[must_use]
    pub fn rgb_to_pcs_xyz(&self, rgb: [f64; 3]) -> Option<[f64; 3]> {
        self.rgb_to_pcs_xyz_with_intent(rgb, self.header.rendering_intent)
    }

    /// Converts device RGB (`0.0..=1.0`) to D50 PCS CIE XYZ honoring explicit [`RenderingIntent`].
    #[must_use]
    pub fn rgb_to_pcs_xyz_with_intent(
        &self,
        rgb: [f64; 3],
        intent: RenderingIntent,
    ) -> Option<[f64; 3]> {
        self.rgb_to_pcs_xyz_with_intent_bpc(rgb, intent, false)
    }

    /// Converts device RGB (`0.0..=1.0`) to D50 PCS CIE XYZ honoring [`RenderingIntent`] and BPC.
    #[must_use]
    pub fn rgb_to_pcs_xyz_with_intent_bpc(
        &self,
        rgb: [f64; 3],
        intent: RenderingIntent,
        bpc: bool,
    ) -> Option<[f64; 3]> {
        self.rgb_to_pcs_xyz_impl(rgb, intent, bpc, false)
    }

    fn rgb_to_pcs_xyz_impl(
        &self,
        rgb: [f64; 3],
        intent: RenderingIntent,
        bpc: bool,
        adapted_abs: bool,
    ) -> Option<[f64; 3]> {
        if self.header.color_space != IccColorSpace::Rgb {
            return None;
        }
        let r = clamp_unit_finite(rgb[0]);
        let g = clamp_unit_finite(rgb[1]);
        let b = clamp_unit_finite(rgb[2]);
        let rel = if let Some(v4) = self
            .select_v4_a2b(intent)
            .filter(|v4| v4.in_channels == 3 && v4.out_channels == 3)
        {
            let raw = v4.evaluate(&[r, g, b]);
            decode_lut_pcs_to_xyz(
                [raw[0], raw[1], raw[2]],
                self.header.pcs,
                v4.clut_is_16bit,
                4,
            )
            .map(sanitize_finite)
        } else if let Some(lut) = self
            .select_a2b(intent)
            .filter(|lut| lut.in_channels == 3 && lut.out_channels == 3)
        {
            let raw = lut.evaluate(&[r, g, b], false);
            decode_lut_pcs_to_xyz(
                [raw[0], raw[1], raw[2]],
                self.header.pcs,
                lut.is_16bit,
                self.header.version_major,
            )
            .map(sanitize_finite)
        } else {
            let mat = self.rgb_to_xyz_matrix?;
            let rl = self.r_trc.as_ref().map_or(r, |t| t.eval(r));
            let gl = self.g_trc.as_ref().map_or(g, |t| t.eval(g));
            let bl = self.b_trc.as_ref().map_or(b, |t| t.eval(b));
            mat3_mul_vec(&mat, [rl, gl, bl]).map(sanitize_finite)
        };
        let bpc_adj = if bpc && intent != RenderingIntent::AbsoluteColorimetric {
            if let Some(src_bp) = self.detect_black_point(intent) {
                Self::apply_bpc(rel, src_bp, 0.0, intent, true)
            } else {
                rel
            }
        } else {
            rel
        };
        Some(if adapted_abs {
            self.adapt_rel_xyz_for_intent_adapted(bpc_adj, intent)
        } else {
            self.adapt_rel_xyz_for_intent(bpc_adj, intent)
        })
    }

    /// Converts D50 PCS CIE XYZ to device RGB (`0.0..=1.0`) using this profile's `B2A*` LUT or inverse Matrix+TRC.
    #[must_use]
    pub fn pcs_xyz_to_rgb(&self, xyz: [f64; 3]) -> Option<[f64; 3]> {
        self.pcs_xyz_to_rgb_with_intent(xyz, self.header.rendering_intent)
    }

    /// Converts D50 PCS CIE XYZ to device RGB (`0.0..=1.0`) honoring explicit [`RenderingIntent`].
    #[must_use]
    pub fn pcs_xyz_to_rgb_with_intent(
        &self,
        xyz: [f64; 3],
        intent: RenderingIntent,
    ) -> Option<[f64; 3]> {
        self.pcs_xyz_to_rgb_impl(xyz, intent, false)
    }

    fn pcs_xyz_to_rgb_impl(
        &self,
        xyz: [f64; 3],
        intent: RenderingIntent,
        adapted_abs: bool,
    ) -> Option<[f64; 3]> {
        if self.header.color_space != IccColorSpace::Rgb {
            return None;
        }
        let xyz_clean = xyz.map(sanitize_finite);
        let rel_xyz = if adapted_abs {
            self.adapt_abs_xyz_to_rel_adapted(xyz_clean, intent)
        } else {
            self.adapt_abs_xyz_to_rel(xyz_clean, intent)
        };
        if let Some(v4) = self.select_v4_b2a(intent) {
            if v4.in_channels == 3 && v4.out_channels == 3 {
                let encoded = encode_xyz_to_lut_pcs(rel_xyz, self.header.pcs, v4.clut_is_16bit, 4);
                let out = v4.evaluate(&encoded);
                return Some([
                    clamp_unit_finite(out[0]),
                    clamp_unit_finite(out[1]),
                    clamp_unit_finite(out[2]),
                ]);
            }
        }
        if let Some(lut) = self.select_b2a(intent) {
            if lut.in_channels == 3 && lut.out_channels == 3 {
                let encoded = encode_xyz_to_lut_pcs(
                    rel_xyz,
                    self.header.pcs,
                    lut.is_16bit,
                    self.header.version_major,
                );
                let out = lut.evaluate(&encoded, self.header.pcs == IccPcs::Xyz);
                return Some([
                    clamp_unit_finite(out[0]),
                    clamp_unit_finite(out[1]),
                    clamp_unit_finite(out[2]),
                ]);
            }
        }
        let mat = self.rgb_to_xyz_matrix?;
        let inv = mat3_invert(&mat)?;
        let lin = mat3_mul_vec(&inv, rel_xyz);
        let r = self
            .r_trc
            .as_ref()
            .map_or_else(|| clamp_unit_finite(lin[0]), |t| t.eval_inverse(lin[0]));
        let g = self
            .g_trc
            .as_ref()
            .map_or_else(|| clamp_unit_finite(lin[1]), |t| t.eval_inverse(lin[1]));
        let b = self
            .b_trc
            .as_ref()
            .map_or_else(|| clamp_unit_finite(lin[2]), |t| t.eval_inverse(lin[2]));
        Some([
            clamp_unit_finite(r),
            clamp_unit_finite(g),
            clamp_unit_finite(b),
        ])
    }

    /// Converts device CMYK (`0.0..=1.0`) to D50 PCS CIE XYZ using `A2B*` LUT or built-in press model.
    #[must_use]
    pub fn cmyk_to_pcs_xyz(&self, cmyk: [f64; 4]) -> Option<[f64; 3]> {
        self.cmyk_to_pcs_xyz_with_intent(cmyk, self.header.rendering_intent)
    }

    /// Converts device CMYK (`0.0..=1.0`) to D50 PCS CIE XYZ honoring explicit [`RenderingIntent`].
    #[must_use]
    pub fn cmyk_to_pcs_xyz_with_intent(
        &self,
        cmyk: [f64; 4],
        intent: RenderingIntent,
    ) -> Option<[f64; 3]> {
        self.cmyk_to_pcs_xyz_with_intent_bpc(cmyk, intent, false)
    }

    /// Converts device CMYK (`0.0..=1.0`) to D50 PCS CIE XYZ honoring [`RenderingIntent`] and BPC.
    #[must_use]
    pub fn cmyk_to_pcs_xyz_with_intent_bpc(
        &self,
        cmyk: [f64; 4],
        intent: RenderingIntent,
        bpc: bool,
    ) -> Option<[f64; 3]> {
        self.cmyk_to_pcs_xyz_impl(cmyk, intent, bpc, false)
    }

    fn cmyk_to_pcs_xyz_impl(
        &self,
        cmyk: [f64; 4],
        intent: RenderingIntent,
        bpc: bool,
        adapted_abs: bool,
    ) -> Option<[f64; 3]> {
        if self.header.color_space != IccColorSpace::Cmyk {
            return None;
        }
        let clamped = cmyk.map(clamp_unit_finite);
        let rel = if let Some(v4) = self
            .select_v4_a2b(intent)
            .filter(|v4| v4.in_channels == 4 && v4.out_channels == 3)
        {
            let raw = v4.evaluate(&clamped);
            decode_lut_pcs_to_xyz(
                [raw[0], raw[1], raw[2]],
                self.header.pcs,
                v4.clut_is_16bit,
                4,
            )
            .map(sanitize_finite)
        } else if let Some(lut) = self
            .select_a2b(intent)
            .filter(|lut| lut.in_channels == 4 && lut.out_channels == 3)
        {
            let raw = lut.evaluate(&clamped, false);
            decode_lut_pcs_to_xyz(
                [raw[0], raw[1], raw[2]],
                self.header.pcs,
                lut.is_16bit,
                self.header.version_major,
            )
            .map(sanitize_finite)
        } else {
            let builtin = self.builtin_kind.filter(|b| b.is_cmyk())?;
            let press = builtin.as_cmyk_press();
            // Compute media-relative PCS first so BPC can operate before Absolute scaling.
            let rel_press = press
                .cmyk_to_xyz_d50(clamped, RenderingIntent::RelativeColorimetric)
                .map(sanitize_finite);
            let bpc_adj = if bpc && intent != RenderingIntent::AbsoluteColorimetric {
                if let Some(src_bp) = self.detect_black_point(intent) {
                    Self::apply_bpc(rel_press, src_bp, 0.0, intent, true)
                } else {
                    rel_press
                }
            } else {
                rel_press
            };
            return Some(if intent == RenderingIntent::AbsoluteColorimetric {
                press.cmyk_to_xyz_d50(clamped, intent).map(sanitize_finite)
            } else {
                bpc_adj
            });
        };
        let bpc_adj = if bpc && intent != RenderingIntent::AbsoluteColorimetric {
            if let Some(src_bp) = self.detect_black_point(intent) {
                Self::apply_bpc(rel, src_bp, 0.0, intent, true)
            } else {
                rel
            }
        } else {
            rel
        };
        Some(if adapted_abs {
            self.adapt_rel_xyz_for_intent_adapted(bpc_adj, intent)
        } else {
            self.adapt_rel_xyz_for_intent(bpc_adj, intent)
        })
    }

    /// Converts device Gray (`0.0..=1.0`) to D50 PCS CIE XYZ using `kTRC` or `A2B*`.
    #[must_use]
    pub fn gray_to_pcs_xyz(&self, gray: f64) -> Option<[f64; 3]> {
        self.gray_to_pcs_xyz_with_intent(gray, self.header.rendering_intent)
    }

    /// Converts device Gray (`0.0..=1.0`) to D50 PCS CIE XYZ honoring [`RenderingIntent`].
    #[must_use]
    pub fn gray_to_pcs_xyz_with_intent(
        &self,
        gray: f64,
        intent: RenderingIntent,
    ) -> Option<[f64; 3]> {
        self.gray_to_pcs_xyz_with_intent_bpc(gray, intent, false)
    }

    /// Converts device Gray (`0.0..=1.0`) to D50 PCS CIE XYZ honoring [`RenderingIntent`] and BPC.
    #[must_use]
    pub fn gray_to_pcs_xyz_with_intent_bpc(
        &self,
        gray: f64,
        intent: RenderingIntent,
        bpc: bool,
    ) -> Option<[f64; 3]> {
        self.gray_to_pcs_xyz_impl(gray, intent, bpc, false)
    }

    fn gray_to_pcs_xyz_impl(
        &self,
        gray: f64,
        intent: RenderingIntent,
        bpc: bool,
        adapted_abs: bool,
    ) -> Option<[f64; 3]> {
        if self.header.color_space != IccColorSpace::Gray {
            return None;
        }
        let g = clamp_unit_finite(gray);
        let rel = if let Some(trc) = self.k_trc.as_ref() {
            let y = trc.eval(g);
            [y * D50_WHITE[0], y * D50_WHITE[1], y * D50_WHITE[2]]
        } else if let Some(v4) = self
            .select_v4_a2b(intent)
            .filter(|v4| v4.in_channels == 1 && v4.out_channels == 3)
        {
            let raw = v4.evaluate(&[g]);
            decode_lut_pcs_to_xyz(
                [raw[0], raw[1], raw[2]],
                self.header.pcs,
                v4.clut_is_16bit,
                4,
            )
            .map(sanitize_finite)
        } else {
            let lut = self
                .select_a2b(intent)
                .filter(|lut| lut.in_channels == 1 && lut.out_channels == 3)?;
            let raw = lut.evaluate(&[g], false);
            decode_lut_pcs_to_xyz(
                [raw[0], raw[1], raw[2]],
                self.header.pcs,
                lut.is_16bit,
                self.header.version_major,
            )
            .map(sanitize_finite)
        };
        let bpc_adj = if bpc && intent != RenderingIntent::AbsoluteColorimetric {
            if let Some(src_bp) = self.detect_black_point(intent) {
                Self::apply_bpc(rel, src_bp, 0.0, intent, true)
            } else {
                rel
            }
        } else {
            rel
        };
        Some(if adapted_abs {
            self.adapt_rel_xyz_for_intent_adapted(bpc_adj, intent)
        } else {
            self.adapt_rel_xyz_for_intent(bpc_adj, intent)
        })
    }

    /// Converts D50 PCS CIE XYZ to device Gray (`0.0..=1.0`) honoring [`RenderingIntent`].
    #[must_use]
    pub fn pcs_xyz_to_gray_with_intent(
        &self,
        xyz: [f64; 3],
        intent: RenderingIntent,
    ) -> Option<f64> {
        if self.header.color_space != IccColorSpace::Gray {
            return None;
        }
        let rel_xyz = self.adapt_abs_xyz_to_rel(xyz.map(sanitize_finite), intent);
        if let Some(trc) = self.k_trc.as_ref() {
            let y_norm = (rel_xyz[1] / D50_WHITE[1]).clamp(0.0, 1.0);
            return Some(clamp_unit_finite(trc.eval_inverse(y_norm)));
        }
        if let Some(v4) = self
            .select_v4_b2a(intent)
            .filter(|v4| v4.in_channels == 3 && v4.out_channels == 1)
        {
            let encoded = encode_xyz_to_lut_pcs(rel_xyz, self.header.pcs, v4.clut_is_16bit, 4);
            let out = v4.evaluate(&encoded);
            return Some(clamp_unit_finite(out[0]));
        }
        if let Some(lut) = self
            .select_b2a(intent)
            .filter(|lut| lut.in_channels == 3 && lut.out_channels == 1)
        {
            let encoded = encode_xyz_to_lut_pcs(
                rel_xyz,
                self.header.pcs,
                lut.is_16bit,
                self.header.version_major,
            );
            let out = lut.evaluate(&encoded, self.header.pcs == IccPcs::Xyz);
            return Some(clamp_unit_finite(out[0]));
        }
        None
    }

    /// Converts device CMYK (`0.0..=1.0`) directly to sRGB (`0.0..=1.0`) using this profile,
    /// honoring [`RenderingIntent`] and ISO 18619 Black Point Compensation.
    #[must_use]
    pub fn cmyk_to_srgb_with_intent_bpc(
        &self,
        cmyk: [f64; 4],
        intent: RenderingIntent,
        bpc: bool,
    ) -> Option<[f64; 3]> {
        let xyz = self.cmyk_to_pcs_xyz_impl(cmyk, intent, bpc, true)?;
        Some(xyz_d50_to_srgb(xyz))
    }

    /// Converts D50 PCS CIE XYZ to device CMYK (`0.0..=1.0`) using `B2A*` LUT or built-in press model.
    #[must_use]
    pub fn pcs_xyz_to_cmyk(&self, xyz: [f64; 3]) -> Option<[f64; 4]> {
        self.pcs_xyz_to_cmyk_with_intent(xyz, self.header.rendering_intent)
    }

    /// Converts D50 PCS CIE XYZ to device CMYK (`0.0..=1.0`) honoring explicit [`RenderingIntent`].
    #[must_use]
    pub fn pcs_xyz_to_cmyk_with_intent(
        &self,
        xyz: [f64; 3],
        intent: RenderingIntent,
    ) -> Option<[f64; 4]> {
        self.pcs_xyz_to_cmyk_impl(xyz, intent, false)
    }

    fn pcs_xyz_to_cmyk_impl(
        &self,
        xyz: [f64; 3],
        intent: RenderingIntent,
        adapted_abs: bool,
    ) -> Option<[f64; 4]> {
        if self.header.color_space != IccColorSpace::Cmyk {
            return None;
        }
        let xyz_clean = xyz.map(sanitize_finite);
        let rel_xyz = if adapted_abs {
            self.adapt_abs_xyz_to_rel_adapted(xyz_clean, intent)
        } else {
            self.adapt_abs_xyz_to_rel(xyz_clean, intent)
        };
        if let Some(v4) = self.select_v4_b2a(intent) {
            if v4.in_channels == 3 && v4.out_channels == 4 {
                let encoded = encode_xyz_to_lut_pcs(rel_xyz, self.header.pcs, v4.clut_is_16bit, 4);
                let out = v4.evaluate(&encoded);
                return Some([
                    clamp_unit_finite(out[0]),
                    clamp_unit_finite(out[1]),
                    clamp_unit_finite(out[2]),
                    clamp_unit_finite(out[3]),
                ]);
            }
        }
        if let Some(lut) = self.select_b2a(intent) {
            if lut.in_channels == 3 && lut.out_channels == 4 {
                let encoded = encode_xyz_to_lut_pcs(
                    rel_xyz,
                    self.header.pcs,
                    lut.is_16bit,
                    self.header.version_major,
                );
                let out = lut.evaluate(&encoded, self.header.pcs == IccPcs::Xyz);
                return Some([
                    clamp_unit_finite(out[0]),
                    clamp_unit_finite(out[1]),
                    clamp_unit_finite(out[2]),
                    clamp_unit_finite(out[3]),
                ]);
            }
        }
        if let Some(builtin) = self.builtin_kind {
            if builtin.is_cmyk() {
                let press = builtin.as_cmyk_press();
                return Some(
                    press
                        .xyz_d50_to_cmyk(xyz_clean, intent)
                        .map(clamp_unit_finite),
                );
            }
        }
        None
    }
}

// =============================================================================
// Public High-Level Color Management Pipeline APIs
// =============================================================================

/// Converts sRGB `[r, g, b]` (`0.0..=1.0`) to CMYK `[c, m, y, k]` (`0.0..=1.0`) through
/// the specified built-in ICC press `profile` and `intent` (with GCR & TAC clamping).
#[must_use]
pub fn convert_rgb_to_cmyk_icc(
    rgb: [f32; 3],
    profile: BuiltinIccProfile,
    intent: RenderingIntent,
) -> [f32; 4] {
    let xyz = srgb_to_xyz_d50([
        f64::from(sanitize_f32(rgb[0], 0.0).clamp(0.0, 1.0)),
        f64::from(sanitize_f32(rgb[1], 0.0).clamp(0.0, 1.0)),
        f64::from(sanitize_f32(rgb[2], 0.0).clamp(0.0, 1.0)),
    ]);
    let press = profile.as_cmyk_press();
    let cmyk = press.xyz_d50_to_cmyk(xyz, intent);
    [
        cmyk[0] as f32,
        cmyk[1] as f32,
        cmyk[2] as f32,
        cmyk[3] as f32,
    ]
}

/// Converts CMYK `[c, m, y, k]` (`0.0..=1.0`) to sRGB `[r, g, b]` (`0.0..=1.0`) through
/// the specified built-in ICC press `profile` and `intent`.
#[must_use]
pub fn convert_cmyk_to_rgb_icc(
    cmyk: [f32; 4],
    profile: BuiltinIccProfile,
    intent: RenderingIntent,
) -> [f32; 3] {
    let press = profile.as_cmyk_press();
    let xyz = press.cmyk_to_xyz_d50(
        [
            f64::from(sanitize_f32(cmyk[0], 0.0).clamp(0.0, 1.0)),
            f64::from(sanitize_f32(cmyk[1], 0.0).clamp(0.0, 1.0)),
            f64::from(sanitize_f32(cmyk[2], 0.0).clamp(0.0, 1.0)),
            f64::from(sanitize_f32(cmyk[3], 0.0).clamp(0.0, 1.0)),
        ],
        intent,
    );
    let rgb = xyz_d50_to_srgb(xyz);
    [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32]
}

/// Soft-proofs an sRGB color `[r, g, b]` (`0.0..=1.0`) by round-tripping through the
/// CMYK press `profile` (`sRGB -> D50 PCS -> CMYK press separation -> D50 PCS -> sRGB`).
#[must_use]
pub fn soft_proof_srgb_through_cmyk(
    rgb: [f32; 3],
    profile: BuiltinIccProfile,
    intent: RenderingIntent,
) -> [f32; 3] {
    let cmyk = convert_rgb_to_cmyk_icc(rgb, profile, intent);
    convert_cmyk_to_rgb_icc(cmyk, profile, intent)
}

/// Returns `true` if sRGB color `[r, g, b]` falls outside the reproducible color gamut
/// of the CMYK press `profile` (perceptual CIE76 ΔE > 4.5 under Relative Colorimetric).
#[must_use]
pub fn is_out_of_cmyk_gamut(rgb: [f32; 3], profile: BuiltinIccProfile) -> bool {
    let clean = [
        f64::from(sanitize_f32(rgb[0], 0.0).clamp(0.0, 1.0)),
        f64::from(sanitize_f32(rgb[1], 0.0).clamp(0.0, 1.0)),
        f64::from(sanitize_f32(rgb[2], 0.0).clamp(0.0, 1.0)),
    ];
    let xyz_in = srgb_to_xyz_d50(clean);
    let lab_in = xyz_d50_to_lab(xyz_in);

    let press = profile.as_cmyk_press();
    let cmyk = press.xyz_d50_to_cmyk(xyz_in, RenderingIntent::RelativeColorimetric);
    let xyz_out = press.cmyk_to_xyz_d50(cmyk, RenderingIntent::RelativeColorimetric);
    let lab_out = xyz_d50_to_lab(xyz_out);

    let dl = lab_in[0] - lab_out[0];
    let da = lab_in[1] - lab_out[1];
    let db = lab_in[2] - lab_out[2];
    let delta_e = (dl * dl + da * da + db * db).sqrt();
    delta_e > CMYK_GAMUT_DELTA_E_THRESHOLD
}

/// Converts between sRGB and Display P3 (`0.0..=1.0`) via D50 ICC PCS.
#[must_use]
pub fn convert_rgb_between_builtin_profiles(
    rgb: [f32; 3],
    src: BuiltinIccProfile,
    dst: BuiltinIccProfile,
) -> [f32; 3] {
    let clean = [
        f64::from(sanitize_f32(rgb[0], 0.0).clamp(0.0, 1.0)),
        f64::from(sanitize_f32(rgb[1], 0.0).clamp(0.0, 1.0)),
        f64::from(sanitize_f32(rgb[2], 0.0).clamp(0.0, 1.0)),
    ];
    let src_prof = IccProfile::builtin(src);
    let dst_prof = IccProfile::builtin(dst);
    let xyz = src_prof
        .rgb_to_pcs_xyz(clean)
        .unwrap_or_else(|| srgb_to_xyz_d50(clean));
    let out = dst_prof
        .pcs_xyz_to_rgb(xyz)
        .unwrap_or_else(|| xyz_d50_to_srgb(xyz));
    [out[0] as f32, out[1] as f32, out[2] as f32]
}

/// Unified 3-slot (RGB / CMYK / Gray) document ICC color management pipeline honoring
/// explicit [`RenderingIntent`] and ISO 18619 Black Point Compensation.
#[derive(Debug, Clone)]
pub struct IccColorPipeline {
    pub rgb_profile: IccProfile,
    pub cmyk_profile: Option<IccProfile>,
    pub gray_profile: Option<IccProfile>,
    pub intent: RenderingIntent,
    pub bpc: bool,
    rgb_is_srgb: bool,
}

impl Default for IccColorPipeline {
    fn default() -> Self {
        Self {
            rgb_profile: IccProfile::builtin(BuiltinIccProfile::Srgb),
            cmyk_profile: None,
            gray_profile: None,
            intent: RenderingIntent::RelativeColorimetric,
            bpc: true,
            rgb_is_srgb: true,
        }
    }
}

impl IccColorPipeline {
    /// Constructs a pipeline from optional raw binary ICC profiles for RGB, CMYK, and Gray
    /// slots, plus an optional legacy `custom_icc_profile` fallback routed by `color_space`.
    #[must_use]
    pub fn from_document_profiles(
        rgb_bytes: Option<&[u8]>,
        cmyk_bytes: Option<&[u8]>,
        gray_bytes: Option<&[u8]>,
        legacy_custom: Option<&[u8]>,
        intent: RenderingIntent,
        bpc: bool,
    ) -> Self {
        let mut rgb_prof = rgb_bytes
            .and_then(|b| IccProfile::from_bytes(b).ok())
            .filter(|p| p.header.color_space == IccColorSpace::Rgb);
        let mut cmyk_prof = cmyk_bytes
            .and_then(|b| IccProfile::from_bytes(b).ok())
            .filter(|p| p.header.color_space == IccColorSpace::Cmyk);
        let mut gray_prof = gray_bytes
            .and_then(|b| IccProfile::from_bytes(b).ok())
            .filter(|p| p.header.color_space == IccColorSpace::Gray);

        if let Some(custom) = legacy_custom.and_then(|b| IccProfile::from_bytes(b).ok()) {
            match custom.header.color_space {
                IccColorSpace::Rgb if rgb_prof.is_none() => rgb_prof = Some(custom),
                IccColorSpace::Cmyk if cmyk_prof.is_none() => cmyk_prof = Some(custom),
                IccColorSpace::Gray if gray_prof.is_none() => gray_prof = Some(custom),
                _ => {}
            }
        }

        let rgb_is_srgb = rgb_prof.is_none();
        Self {
            rgb_profile: rgb_prof.unwrap_or_else(|| IccProfile::builtin(BuiltinIccProfile::Srgb)),
            cmyk_profile: cmyk_prof,
            gray_profile: gray_prof,
            intent,
            bpc,
            rgb_is_srgb,
        }
    }

    /// Converts device CMYK (`0.0..=1.0`) to sRGB (`0.0..=1.0`) using `self.cmyk_profile`,
    /// `self.intent`, and `self.bpc`. When no CMYK ICC profile is bound, uses the deterministic
    /// baseline CMYK fallback matching `Color::to_rgba_f32`.
    #[must_use]
    pub fn cmyk_to_srgb(&self, cmyk: [f32; 4]) -> [f32; 3] {
        let c = sanitize_f32(cmyk[0], 0.0).clamp(0.0, 1.0);
        let m = sanitize_f32(cmyk[1], 0.0).clamp(0.0, 1.0);
        let y = sanitize_f32(cmyk[2], 0.0).clamp(0.0, 1.0);
        let k = sanitize_f32(cmyk[3], 0.0).clamp(0.0, 1.0);
        if let Some(cp) = &self.cmyk_profile {
            let cmyk64 = [f64::from(c), f64::from(m), f64::from(y), f64::from(k)];
            if let Some(xyz) = cp.cmyk_to_pcs_xyz_impl(cmyk64, self.intent, self.bpc, true) {
                let rgb = xyz_d50_to_srgb(xyz);
                return [
                    rgb[0].clamp(0.0, 1.0) as f32,
                    rgb[1].clamp(0.0, 1.0) as f32,
                    rgb[2].clamp(0.0, 1.0) as f32,
                ];
            }
            return convert_cmyk_to_rgb_icc(
                [c, m, y, k],
                BuiltinIccProfile::IsoCoatedV2Cmyk,
                self.intent,
            );
        }
        let inv_k = 1.0 - k;
        [(1.0 - c) * inv_k, (1.0 - m) * inv_k, (1.0 - y) * inv_k]
    }

    /// Converts device Gray (`0.0` black .. `1.0` white) to sRGB (`0.0..=1.0`).
    #[must_use]
    pub fn gray_to_srgb(&self, v: f32) -> [f32; 3] {
        let vf = sanitize_f32(v, 0.0).clamp(0.0, 1.0);
        if let Some(gp) = &self.gray_profile {
            if let Some(xyz) = gp.gray_to_pcs_xyz_impl(f64::from(vf), self.intent, self.bpc, true) {
                let rgb = xyz_d50_to_srgb(xyz);
                return [
                    rgb[0].clamp(0.0, 1.0) as f32,
                    rgb[1].clamp(0.0, 1.0) as f32,
                    rgb[2].clamp(0.0, 1.0) as f32,
                ];
            }
        }
        [vf, vf, vf]
    }

    /// Converts sRGB (`0.0..=1.0`) to device CMYK (`0.0..=1.0`) using `self.cmyk_profile`.
    #[must_use]
    pub fn srgb_to_cmyk(&self, rgb: [f32; 3]) -> [f32; 4] {
        let rgb64 = [
            f64::from(sanitize_f32(rgb[0], 0.0).clamp(0.0, 1.0)),
            f64::from(sanitize_f32(rgb[1], 0.0).clamp(0.0, 1.0)),
            f64::from(sanitize_f32(rgb[2], 0.0).clamp(0.0, 1.0)),
        ];
        let xyz = if self.rgb_is_srgb {
            srgb_to_xyz_d50(rgb64)
        } else {
            self.rgb_profile
                .rgb_to_pcs_xyz_impl(rgb64, self.intent, self.bpc, true)
                .unwrap_or_else(|| srgb_to_xyz_d50(rgb64))
        };
        if let Some(cp) = &self.cmyk_profile {
            if let Some(cmyk) = cp.pcs_xyz_to_cmyk_impl(xyz, self.intent, true) {
                return [
                    cmyk[0].clamp(0.0, 1.0) as f32,
                    cmyk[1].clamp(0.0, 1.0) as f32,
                    cmyk[2].clamp(0.0, 1.0) as f32,
                    cmyk[3].clamp(0.0, 1.0) as f32,
                ];
            }
        }
        convert_rgb_to_cmyk_icc(rgb, BuiltinIccProfile::IsoCoatedV2Cmyk, self.intent)
    }

    /// Converts any [`crate::color::Color`] into display sRGB `[r, g, b, a]` through this pipeline.
    #[must_use]
    pub fn color_to_rgba_f32(&self, color: &crate::color::Color) -> [f32; 4] {
        use crate::color::Color;
        match *color {
            Color::Rgba { r, g, b, a } => {
                let a = sanitize_f32(a, 0.0).clamp(0.0, 1.0);
                if self.rgb_is_srgb {
                    return [
                        sanitize_f32(r, 0.0).clamp(0.0, 1.0),
                        sanitize_f32(g, 0.0).clamp(0.0, 1.0),
                        sanitize_f32(b, 0.0).clamp(0.0, 1.0),
                        a,
                    ];
                }
                let rgb64 = [
                    f64::from(sanitize_f32(r, 0.0).clamp(0.0, 1.0)),
                    f64::from(sanitize_f32(g, 0.0).clamp(0.0, 1.0)),
                    f64::from(sanitize_f32(b, 0.0).clamp(0.0, 1.0)),
                ];
                let out = self
                    .rgb_profile
                    .rgb_to_pcs_xyz_impl(rgb64, self.intent, self.bpc, true)
                    .map(xyz_d50_to_srgb)
                    .unwrap_or(rgb64);
                [
                    out[0].clamp(0.0, 1.0) as f32,
                    out[1].clamp(0.0, 1.0) as f32,
                    out[2].clamp(0.0, 1.0) as f32,
                    a,
                ]
            }
            Color::Cmyk { c, m, y, k, a } => {
                let [r, g, b] = self.cmyk_to_srgb([c, m, y, k]);
                [r, g, b, sanitize_f32(a, 0.0).clamp(0.0, 1.0)]
            }
            Color::Spot {
                tint,
                fallback_cmyk,
                alpha,
                ..
            } => {
                let t = sanitize_f32(tint, 0.0).clamp(0.0, 1.0);
                let [r, g, b] = self.cmyk_to_srgb([
                    fallback_cmyk[0] * t,
                    fallback_cmyk[1] * t,
                    fallback_cmyk[2] * t,
                    fallback_cmyk[3] * t,
                ]);
                [r, g, b, sanitize_f32(alpha, 0.0).clamp(0.0, 1.0)]
            }
            Color::Gray { v, a } => {
                let [r, g, b] = self.gray_to_srgb(v);
                [r, g, b, sanitize_f32(a, 0.0).clamp(0.0, 1.0)]
            }
            Color::Hsl { .. } | Color::Lab { .. } => color.to_rgba_f32(),
        }
    }

    /// Converts any [`crate::color::Color`] into 8-bit `[r, g, b, a]` through this pipeline.
    #[must_use]
    pub fn color_to_rgba8(&self, color: &crate::color::Color) -> [u8; 4] {
        let [r, g, b, a] = self.color_to_rgba_f32(color);
        [
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
            (a * 255.0).round() as u8,
        ]
    }

    /// ICC-aware color interpolation between `a` (`t = 0.0`) and `b` (`t = 1.0`).
    #[must_use]
    pub fn lerp_color(
        &self,
        a: &crate::color::Color,
        b: &crate::color::Color,
        t: f32,
    ) -> crate::color::Color {
        use crate::color::Color;
        let t = sanitize_f32(t, 0.0).clamp(0.0, 1.0);
        if let (
            Color::Cmyk {
                c: c0,
                m: m0,
                y: y0,
                k: k0,
                a: a0,
            },
            Color::Cmyk {
                c: c1,
                m: m1,
                y: y1,
                k: k1,
                a: a1,
            },
        ) = (a, b)
        {
            return Color::Cmyk {
                c: c0 + (c1 - c0) * t,
                m: m0 + (m1 - m0) * t,
                y: y0 + (y1 - y0) * t,
                k: k0 + (k1 - k0) * t,
                a: a0 + (a1 - a0) * t,
            };
        }
        let ca = self.color_to_rgba_f32(a);
        let cb = self.color_to_rgba_f32(b);
        Color::rgba(
            ca[0] + (cb[0] - ca[0]) * t,
            ca[1] + (cb[1] - ca[1]) * t,
            ca[2] + (cb[2] - ca[2]) * t,
            ca[3] + (cb[3] - ca[3]) * t,
        )
    }
}

/// Fast real-time framebuffer CMYK soft-proofing and optional out-of-gamut warning pass
/// over straight-RGBA8 pixel bytes using a cached `17 × 17 × 17` 3D tetrahedral LUT.
///
/// Out-of-gamut pixels (when `gamut_warning == true`) are overlaid with high-visibility
/// cyan-green gamut warning tint (`#00FF99`, 75% blend) while preserving pixel alpha.
pub fn apply_soft_proof_rgba(
    rgba: &mut [u8],
    profile: BuiltinIccProfile,
    intent: RenderingIntent,
    soft_proof: bool,
    gamut_warning: bool,
) {
    apply_soft_proof_rgba_with_profile(rgba, None, profile, intent, soft_proof, gamut_warning);
}

/// Real-time soft-proofing and out-of-gamut overlay supporting arbitrary loaded custom
/// [`IccProfile`] profiles (falling back to `fallback` built-in press when `custom_profile`
/// lacks forward/inverse CMYK or RGB transforms).
pub fn apply_soft_proof_rgba_with_profile(
    rgba: &mut [u8],
    custom_profile: Option<&IccProfile>,
    fallback: BuiltinIccProfile,
    intent: RenderingIntent,
    soft_proof: bool,
    gamut_warning: bool,
) {
    if !soft_proof && !gamut_warning {
        return;
    }
    let custom_lut = custom_profile.and_then(|p| SoftProofLut3D::build_from_profile(p, intent));
    let lut = custom_lut
        .as_ref()
        .unwrap_or_else(|| cached_soft_proof_lut(fallback.as_cmyk_press(), intent));
    lut.apply_rgba(rgba, soft_proof, gamut_warning);
}

// =============================================================================
// Cached 17×17×17 3D Tetrahedral Soft-Proof LUT
// =============================================================================

/// Cached `17 × 17 × 17` 3D tetrahedral transform LUT for fast soft-proofing & gamut checks.
#[derive(Debug, Clone)]
pub struct SoftProofLut3D {
    grid: usize,
    /// `[proof_r, proof_g, proof_b, rel_delta_e / 100.0]` per `(r, g, b)` lattice point.
    data: Vec<[f32; 4]>,
}

impl SoftProofLut3D {
    /// Builds a LUT for `custom_profile` if capable of full soft-proofing roundtrips, otherwise
    /// returns the static cached LUT for `fallback` press and `intent`.
    #[must_use]
    pub fn build_for_profile_or_builtin(
        custom_profile: Option<&IccProfile>,
        fallback: BuiltinIccProfile,
        intent: RenderingIntent,
    ) -> Self {
        custom_profile
            .and_then(|p| Self::build_from_profile(p, intent))
            .unwrap_or_else(|| cached_soft_proof_lut(fallback.as_cmyk_press(), intent).clone())
    }

    /// Applies precomputed soft-proofing / gamut-warning transformation to straight RGBA8 pixels.
    pub fn apply_rgba(&self, rgba: &mut [u8], soft_proof: bool, gamut_warning: bool) {
        if !soft_proof && !gamut_warning {
            return;
        }
        for px in rgba.as_chunks_mut::<4>().0 {
            if px[3] == 0 {
                continue;
            }
            let rf = f32::from(px[0]) / 255.0;
            let gf = f32::from(px[1]) / 255.0;
            let bf = f32::from(px[2]) / 255.0;
            let [pr, pg, pb, delta_e_norm] = self.sample_tetrahedral(rf, gf, bf);

            let (mut r, mut g, mut b) = if soft_proof {
                (pr, pg, pb)
            } else {
                (rf, gf, bf)
            };

            if gamut_warning && (f64::from(delta_e_norm) * 100.0) > CMYK_GAMUT_DELTA_E_THRESHOLD {
                const WARN_R: f32 = 0.0;
                const WARN_G: f32 = 1.0;
                const WARN_B: f32 = 0.6;
                r = r * 0.25 + WARN_R * 0.75;
                g = g * 0.25 + WARN_G * 0.75;
                b = b * 0.25 + WARN_B * 0.75;
            }

            px[0] = (r * 255.0).round().clamp(0.0, 255.0) as u8;
            px[1] = (g * 255.0).round().clamp(0.0, 255.0) as u8;
            px[2] = (b * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    fn build(press: CmykPressKind, intent: RenderingIntent) -> Self {
        let g = SOFT_PROOF_LUT_GRID;
        let mut data = Vec::with_capacity(g * g * g);
        let denom = (g - 1) as f64;
        for ir in 0..g {
            let r = (ir as f64) / denom;
            for ig in 0..g {
                let gv = (ig as f64) / denom;
                for ib in 0..g {
                    let b = (ib as f64) / denom;
                    let xyz_in = srgb_to_xyz_d50([r, gv, b]);
                    let lab_in = xyz_d50_to_lab(xyz_in);

                    // Proofed color under selected rendering intent
                    let cmyk_intent = press.xyz_d50_to_cmyk(xyz_in, intent);
                    let xyz_proof = press.cmyk_to_xyz_d50(cmyk_intent, intent);
                    let proof_rgb = xyz_d50_to_srgb(xyz_proof);

                    // Out-of-gamut metric always measured under RelativeColorimetric
                    let xyz_rel_out = if intent == RenderingIntent::RelativeColorimetric {
                        xyz_proof
                    } else {
                        let cmyk_rel =
                            press.xyz_d50_to_cmyk(xyz_in, RenderingIntent::RelativeColorimetric);
                        press.cmyk_to_xyz_d50(cmyk_rel, RenderingIntent::RelativeColorimetric)
                    };
                    let lab_rel_out = xyz_d50_to_lab(xyz_rel_out);
                    let dl = lab_in[0] - lab_rel_out[0];
                    let da = lab_in[1] - lab_rel_out[1];
                    let db = lab_in[2] - lab_rel_out[2];
                    let de_norm = ((dl * dl + da * da + db * db).sqrt() / 100.0).clamp(0.0, 2.0);

                    data.push([
                        proof_rgb[0] as f32,
                        proof_rgb[1] as f32,
                        proof_rgb[2] as f32,
                        de_norm as f32,
                    ]);
                }
            }
        }
        Self { grid: g, data }
    }

    #[must_use]
    pub fn build_from_profile(profile: &IccProfile, intent: RenderingIntent) -> Option<Self> {
        let has_fwd_cmyk = profile.select_a2b(intent).is_some()
            || profile.select_v4_a2b(intent).is_some()
            || profile.builtin_kind.is_some_and(BuiltinIccProfile::is_cmyk);
        let has_bwd_cmyk = profile.select_b2a(intent).is_some()
            || profile.select_v4_b2a(intent).is_some()
            || profile.builtin_kind.is_some_and(BuiltinIccProfile::is_cmyk);
        let can_cmyk =
            profile.header.color_space == IccColorSpace::Cmyk && has_fwd_cmyk && has_bwd_cmyk;

        let has_fwd_rgb = profile.select_a2b(intent).is_some()
            || profile.select_v4_a2b(intent).is_some()
            || profile.rgb_to_xyz_matrix.is_some();
        let has_bwd_rgb = profile.select_b2a(intent).is_some()
            || profile.select_v4_b2a(intent).is_some()
            || (profile.rgb_to_xyz_matrix.is_some() && profile.b2a0.is_none());
        let can_rgb =
            profile.header.color_space == IccColorSpace::Rgb && has_fwd_rgb && has_bwd_rgb;
        if !can_cmyk && !can_rgb {
            return None;
        }
        let g = SOFT_PROOF_LUT_GRID;
        let mut data = Vec::with_capacity(g * g * g);
        let denom = (g - 1) as f64;
        for ir in 0..g {
            let r = (ir as f64) / denom;
            for ig in 0..g {
                let gv = (ig as f64) / denom;
                for ib in 0..g {
                    let b = (ib as f64) / denom;
                    let xyz_in = srgb_to_xyz_d50([r, gv, b]);
                    let lab_in = xyz_d50_to_lab(xyz_in);
                    let xyz_proof = if can_cmyk {
                        let cmyk = profile.pcs_xyz_to_cmyk_impl(xyz_in, intent, true)?;
                        profile.cmyk_to_pcs_xyz_impl(cmyk, intent, false, true)?
                    } else {
                        let rgb_dev = profile.pcs_xyz_to_rgb_impl(xyz_in, intent, true)?;
                        profile.rgb_to_pcs_xyz_impl(rgb_dev, intent, false, true)?
                    };
                    let proof_rgb = xyz_d50_to_srgb(xyz_proof);
                    let xyz_rel = if intent == RenderingIntent::RelativeColorimetric {
                        xyz_proof
                    } else if can_cmyk {
                        let cmyk_r = profile.pcs_xyz_to_cmyk_impl(
                            xyz_in,
                            RenderingIntent::RelativeColorimetric,
                            true,
                        )?;
                        profile.cmyk_to_pcs_xyz_impl(
                            cmyk_r,
                            RenderingIntent::RelativeColorimetric,
                            false,
                            true,
                        )?
                    } else {
                        let rgb_r = profile.pcs_xyz_to_rgb_impl(
                            xyz_in,
                            RenderingIntent::RelativeColorimetric,
                            true,
                        )?;
                        profile.rgb_to_pcs_xyz_impl(
                            rgb_r,
                            RenderingIntent::RelativeColorimetric,
                            false,
                            true,
                        )?
                    };
                    let lab_out = xyz_d50_to_lab(xyz_rel);
                    let dl = lab_in[0] - lab_out[0];
                    let da = lab_in[1] - lab_out[1];
                    let db = lab_in[2] - lab_out[2];
                    let de_norm = ((dl * dl + da * da + db * db).sqrt() / 100.0).clamp(0.0, 2.0);
                    data.push([
                        proof_rgb[0] as f32,
                        proof_rgb[1] as f32,
                        proof_rgb[2] as f32,
                        de_norm as f32,
                    ]);
                }
            }
        }
        Some(Self { grid: g, data })
    }

    #[inline]
    fn sample_tetrahedral(&self, r: f32, g_ch: f32, b: f32) -> [f32; 4] {
        let g = self.grid;
        let scale = (g - 1) as f32;
        let sr = r.clamp(0.0, 1.0) * scale;
        let sg = g_ch.clamp(0.0, 1.0) * scale;
        let sb = b.clamp(0.0, 1.0) * scale;
        let ir0 = (sr.floor() as usize).min(g - 1);
        let ig0 = (sg.floor() as usize).min(g - 1);
        let ib0 = (sb.floor() as usize).min(g - 1);
        let ir1 = (ir0 + 1).min(g - 1);
        let ig1 = (ig0 + 1).min(g - 1);
        let ib1 = (ib0 + 1).min(g - 1);
        let fx = sr - ir0 as f32;
        let fy = sg - ig0 as f32;
        let fz = sb - ib0 as f32;

        let at = |ir: usize, ig: usize, ib: usize| self.data[(ir * g + ig) * g + ib];
        let c000 = at(ir0, ig0, ib0);
        let c111 = at(ir1, ig1, ib1);

        let (wa, ca, wb, cb, wc, cc, wd, cd) = if fx >= fy {
            if fy >= fz {
                (
                    1.0 - fx,
                    c000,
                    fx - fy,
                    at(ir1, ig0, ib0),
                    fy - fz,
                    at(ir1, ig1, ib0),
                    fz,
                    c111,
                )
            } else if fx >= fz {
                (
                    1.0 - fx,
                    c000,
                    fx - fz,
                    at(ir1, ig0, ib0),
                    fz - fy,
                    at(ir1, ig0, ib1),
                    fy,
                    c111,
                )
            } else {
                (
                    1.0 - fz,
                    c000,
                    fz - fx,
                    at(ir0, ig0, ib1),
                    fx - fy,
                    at(ir1, ig0, ib1),
                    fy,
                    c111,
                )
            }
        } else if fz >= fy {
            (
                1.0 - fz,
                c000,
                fz - fy,
                at(ir0, ig0, ib1),
                fy - fx,
                at(ir0, ig1, ib1),
                fx,
                c111,
            )
        } else if fz >= fx {
            (
                1.0 - fy,
                c000,
                fy - fz,
                at(ir0, ig1, ib0),
                fz - fx,
                at(ir0, ig1, ib1),
                fx,
                c111,
            )
        } else {
            (
                1.0 - fy,
                c000,
                fy - fx,
                at(ir0, ig1, ib0),
                fx - fz,
                at(ir1, ig1, ib0),
                fz,
                c111,
            )
        };

        [
            wa * ca[0] + wb * cb[0] + wc * cc[0] + wd * cd[0],
            wa * ca[1] + wb * cb[1] + wc * cc[1] + wd * cd[1],
            wa * ca[2] + wb * cb[2] + wc * cc[2] + wd * cd[2],
            wa * ca[3] + wb * cb[3] + wc * cc[3] + wd * cd[3],
        ]
    }
}

fn cached_soft_proof_lut(press: CmykPressKind, intent: RenderingIntent) -> &'static SoftProofLut3D {
    static LUTS: [OnceLock<SoftProofLut3D>; 8] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    let press_idx = match press {
        CmykPressKind::IsoCoatedV2 => 0,
        CmykPressKind::UsWebCoatedSwop => 1,
    };
    let intent_idx = intent.to_u32() as usize;
    let slot = press_idx * 4 + intent_idx;
    LUTS[slot].get_or_init(|| SoftProofLut3D::build(press, intent))
}

// =============================================================================
// Clean-Room Physical CMYK Press Model (Yule-Nielsen 16-Primary Neugebauer)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmykPressKind {
    IsoCoatedV2,
    UsWebCoatedSwop,
}

struct CmykPressSpec {
    /// Midtone dot gain (TVI) for `[C, M, Y, K]`.
    dot_gain: [f64; 4],
    /// Yule-Nielsen optical trapping factor (`1.7`).
    yn_n: f64,
    /// Maximum Total Area Coverage (`3.30` = 330% for ISO Coated v2, `3.00` = 300% for SWOP).
    tac_limit: f64,
    /// GCR black start & strength.
    gcr_start: f64,
    gcr_strength: f64,
    /// Pre-raised `XYZ^(1/n)` values for the 16 Neugebauer primaries (bitmask: 1=C, 2=M, 4=Y, 8=K).
    primaries_yn: [[f64; 3]; 16],
    /// Absolute D50 paper white XYZ (primary 0).
    paper_white_xyz: [f64; 3],
    /// Relative-colorimetric dark point lightness L* (for Perceptual BPC).
    black_point_l: f64,
}

impl CmykPressKind {
    fn spec(self) -> &'static CmykPressSpec {
        static ISO_COATED_V2: OnceLock<CmykPressSpec> = OnceLock::new();
        static US_WEB_SWOP: OnceLock<CmykPressSpec> = OnceLock::new();
        match self {
            Self::IsoCoatedV2 => ISO_COATED_V2.get_or_init(|| {
                // ISO 12647-2 glossy coated sheetfed colorimetric aims (clean-room mathematical model)
                let labs: [[f64; 3]; 16] = [
                    [95.0, 0.0, -2.0],    // 0:  Paper White (W)
                    [55.0, -37.0, -50.0], // 1:  C
                    [48.0, 74.0, -3.0],   // 2:  M
                    [24.0, 17.0, -46.0],  // 3:  C+M (Blue)
                    [89.0, -5.0, 93.0],   // 4:  Y
                    [50.0, -68.0, 25.0],  // 5:  C+Y (Green)
                    [47.0, 68.0, 48.0],   // 6:  M+Y (Red)
                    [23.0, 0.0, 0.0],     // 7:  C+M+Y
                    [16.0, 0.0, 0.0],     // 8:  K
                    [14.0, -10.0, -14.0], // 9:  C+K
                    [13.0, 18.0, -1.0],   // 10: M+K
                    [10.0, 3.0, -11.0],   // 11: C+M+K
                    [15.5, -1.0, 16.0],   // 12: Y+K
                    [12.0, -15.0, 6.0],   // 13: C+Y+K
                    [12.0, 16.0, 10.0],   // 14: M+Y+K
                    [9.0, 0.0, 0.0],      // 15: C+M+Y+K
                ];
                build_press_spec([0.14, 0.14, 0.13, 0.17], 1.7, 3.30, 0.12, 0.80, labs)
            }),
            Self::UsWebCoatedSwop => US_WEB_SWOP.get_or_init(|| {
                // US Web Coated SWOP v2 web-offset colorimetric aims (clean-room mathematical model)
                let labs: [[f64; 3]; 16] = [
                    [93.0, 0.5, 2.0],     // 0:  Paper White (W)
                    [56.0, -35.0, -45.0], // 1:  C
                    [47.0, 69.0, -2.0],   // 2:  M
                    [26.0, 15.0, -40.0],  // 3:  C+M (Blue)
                    [86.0, -4.0, 86.0],   // 4:  Y
                    [50.0, -61.0, 22.0],  // 5:  C+Y (Green)
                    [46.0, 63.0, 44.0],   // 6:  M+Y (Red)
                    [26.0, 1.0, 2.0],     // 7:  C+M+Y
                    [19.0, 0.5, 1.0],     // 8:  K
                    [16.5, -9.0, -12.0],  // 9:  C+K
                    [15.5, 16.0, 0.0],    // 10: M+K
                    [13.0, 2.5, -9.0],    // 11: C+M+K
                    [18.0, -0.5, 14.0],   // 12: Y+K
                    [14.5, -13.0, 5.0],   // 13: C+Y+K
                    [14.5, 14.0, 9.0],    // 14: M+Y+K
                    [12.0, 0.5, 1.0],     // 15: C+M+Y+K
                ];
                build_press_spec([0.18, 0.19, 0.17, 0.21], 1.7, 3.00, 0.15, 0.78, labs)
            }),
        }
    }

    fn paper_white_xyz(self) -> [f64; 3] {
        self.spec().paper_white_xyz
    }

    /// Forward physical press model: `CMYK -> D50 PCS XYZ`.
    fn cmyk_to_xyz_d50(self, cmyk: [f64; 4], intent: RenderingIntent) -> [f64; 3] {
        let spec = self.spec();
        let abs_xyz = eval_neugebauer_abs_xyz(spec, cmyk);
        if intent == RenderingIntent::AbsoluteColorimetric {
            abs_xyz
        } else {
            // Media-relative normalization: paper white maps cleanly to D50 PCS white
            [
                abs_xyz[0] * (D50_WHITE[0] / spec.paper_white_xyz[0]),
                abs_xyz[1] * (D50_WHITE[1] / spec.paper_white_xyz[1]),
                abs_xyz[2] * (D50_WHITE[2] / spec.paper_white_xyz[2]),
            ]
        }
    }

    /// Inverse press separation: `D50 PCS XYZ -> CMYK` using GCR + damped Gauss-Newton + TAC limit.
    fn xyz_d50_to_cmyk(self, xyz: [f64; 3], intent: RenderingIntent) -> [f64; 4] {
        let spec = self.spec();

        // Prepare target Lab in media-relative (or absolute) space according to rendering intent
        let mut target_lab = xyz_d50_to_lab(xyz);
        match intent {
            RenderingIntent::Perceptual => {
                // Smooth black-point compensation + gentle chroma compression for print gamut
                let bp = spec.black_point_l;
                target_lab[0] = bp + (target_lab[0] / 100.0) * (100.0 - bp);
                let chroma = target_lab[1].hypot(target_lab[2]);
                if chroma > 38.0 {
                    let excess = chroma - 38.0;
                    let compressed = 38.0 + excess / (1.0 + excess / 55.0);
                    let scale = compressed / chroma;
                    target_lab[1] *= scale;
                    target_lab[2] *= scale;
                }
            }
            RenderingIntent::Saturation => {
                // Slight chroma boost before press boundary clamping
                target_lab[1] *= 1.08;
                target_lab[2] *= 1.08;
            }
            RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => {}
        }

        let target_xyz_rel = lab_to_xyz_d50(target_lab);
        let target_abs_xyz = if intent == RenderingIntent::AbsoluteColorimetric {
            xyz
        } else {
            [
                target_xyz_rel[0] * (spec.paper_white_xyz[0] / D50_WHITE[0]),
                target_xyz_rel[1] * (spec.paper_white_xyz[1] / D50_WHITE[1]),
                target_xyz_rel[2] * (spec.paper_white_xyz[2] / D50_WHITE[2]),
            ]
        };
        let target_abs_lab = xyz_d50_to_lab(target_abs_xyz);

        // Quick exact paper-white check
        let paper_lab = xyz_d50_to_lab(spec.paper_white_xyz);
        if lab_dist_sq(target_abs_lab, paper_lab) < 1e-4 {
            return [0.0, 0.0, 0.0, 0.0];
        }

        // Initial seed from naive sRGB -> CMYK with GCR black generation
        let seed_rgb = xyz_d50_to_srgb(target_xyz_rel);
        let raw_c = (1.0 - seed_rgb[0]).clamp(0.0, 1.0);
        let raw_m = (1.0 - seed_rgb[1]).clamp(0.0, 1.0);
        let raw_y = (1.0 - seed_rgb[2]).clamp(0.0, 1.0);
        let min_cmy = raw_c.min(raw_m).min(raw_y);
        let k_gcr = if min_cmy > spec.gcr_start {
            let norm = (min_cmy - spec.gcr_start) / (1.0 - spec.gcr_start);
            (norm.powf(1.35) * spec.gcr_strength).clamp(0.0, 0.98)
        } else {
            0.0
        };
        let mut cmyk = [
            (raw_c - k_gcr * 0.82).clamp(0.0, 1.0),
            (raw_m - k_gcr * 0.82).clamp(0.0, 1.0),
            (raw_y - k_gcr * 0.82).clamp(0.0, 1.0),
            k_gcr,
        ];
        enforce_tac(&mut cmyk, spec.tac_limit);

        let eval_err = |x: [f64; 4]| -> ([f64; 3], f64) {
            let cur_lab = xyz_d50_to_lab(eval_neugebauer_abs_xyz(spec, x));
            let r = [
                cur_lab[0] - target_abs_lab[0],
                cur_lab[1] - target_abs_lab[1],
                cur_lab[2] - target_abs_lab[2],
            ];
            (r, r[0] * r[0] + r[1] * r[1] + r[2] * r[2])
        };

        // Stage 1: Damped Gauss-Newton solving (c, m, y) at GCR-seeded K (8 iterations)
        let (mut res, mut err) = eval_err(cmyk);
        let mut lambda = 0.08_f64;
        for _ in 0..8 {
            if err < 1e-4 {
                break;
            }
            let eps = 1e-3;
            let mut jac = [[0.0_f64; 3]; 3];
            for j in 0..3 {
                let mut probe = cmyk;
                let step = if probe[j] + eps <= 1.0 { eps } else { -eps };
                probe[j] = (probe[j] + step).clamp(0.0, 1.0);
                let (p_res, _) = eval_err(probe);
                let inv_step = 1.0 / step;
                jac[0][j] = (p_res[0] - res[0]) * inv_step;
                jac[1][j] = (p_res[1] - res[1]) * inv_step;
                jac[2][j] = (p_res[2] - res[2]) * inv_step;
            }
            // Solve (J^T J + lambda I) delta = -J^T res
            let mut jtj = [[0.0_f64; 3]; 3];
            let mut rhs = [0.0_f64; 3];
            for r_i in 0..3 {
                for c_i in 0..3 {
                    jtj[r_i][c_i] = jac[0][r_i] * jac[0][c_i]
                        + jac[1][r_i] * jac[1][c_i]
                        + jac[2][r_i] * jac[2][c_i];
                }
                jtj[r_i][r_i] += lambda;
                rhs[r_i] = -(jac[0][r_i] * res[0] + jac[1][r_i] * res[1] + jac[2][r_i] * res[2]);
            }
            let Some(inv_jtj) = mat3_invert(&jtj) else {
                lambda *= 4.0;
                continue;
            };
            let delta = mat3_mul_vec(&inv_jtj, rhs);

            let mut accepted = false;
            let mut alpha = 1.0;
            for _ in 0..4 {
                let mut cand = [
                    (cmyk[0] + delta[0] * alpha).clamp(0.0, 1.0),
                    (cmyk[1] + delta[1] * alpha).clamp(0.0, 1.0),
                    (cmyk[2] + delta[2] * alpha).clamp(0.0, 1.0),
                    cmyk[3],
                ];
                enforce_tac(&mut cand, spec.tac_limit);
                let (c_res, c_err) = eval_err(cand);
                if c_err < err {
                    cmyk = cand;
                    res = c_res;
                    err = c_err;
                    lambda = (lambda * 0.5).max(1e-4);
                    accepted = true;
                    break;
                }
                alpha *= 0.5;
            }
            if !accepted {
                lambda *= 4.0;
            }
        }

        // Stage 2: If significant residual remains (deep shadows or out-of-gamut dark colors),
        // allow K to adjust along with CMY via 1D golden/bisection K sweep + 3-step CMY polish
        if err > 0.25 && target_abs_lab[0] < 45.0 {
            let mut best = cmyk;
            let mut best_err = err;
            for step in [0.15_f64, 0.35, 0.55, 0.75, 0.92, 1.0] {
                let mut cand = [cmyk[0], cmyk[1], cmyk[2], step];
                enforce_tac(&mut cand, spec.tac_limit);
                let (_, c_err) = eval_err(cand);
                if c_err < best_err {
                    best = cand;
                    best_err = c_err;
                }
            }
            if best_err < err {
                cmyk = best;
                let (r0, e0) = eval_err(cmyk);
                res = r0;
                err = e0;
                for _ in 0..4 {
                    if err < 1e-4 {
                        break;
                    }
                    let eps = 1e-3;
                    let mut jac = [[0.0_f64; 3]; 3];
                    for j in 0..3 {
                        let mut probe = cmyk;
                        let step = if probe[j] + eps <= 1.0 { eps } else { -eps };
                        probe[j] = (probe[j] + step).clamp(0.0, 1.0);
                        let (p_res, _) = eval_err(probe);
                        let inv_step = 1.0 / step;
                        jac[0][j] = (p_res[0] - res[0]) * inv_step;
                        jac[1][j] = (p_res[1] - res[1]) * inv_step;
                        jac[2][j] = (p_res[2] - res[2]) * inv_step;
                    }
                    let mut jtj = [[0.0_f64; 3]; 3];
                    let mut rhs = [0.0_f64; 3];
                    for r_i in 0..3 {
                        for c_i in 0..3 {
                            jtj[r_i][c_i] = jac[0][r_i] * jac[0][c_i]
                                + jac[1][r_i] * jac[1][c_i]
                                + jac[2][r_i] * jac[2][c_i];
                        }
                        jtj[r_i][r_i] += 0.1;
                        rhs[r_i] =
                            -(jac[0][r_i] * res[0] + jac[1][r_i] * res[1] + jac[2][r_i] * res[2]);
                    }
                    if let Some(inv_jtj) = mat3_invert(&jtj) {
                        let delta = mat3_mul_vec(&inv_jtj, rhs);
                        let mut cand = [
                            (cmyk[0] + delta[0]).clamp(0.0, 1.0),
                            (cmyk[1] + delta[1]).clamp(0.0, 1.0),
                            (cmyk[2] + delta[2]).clamp(0.0, 1.0),
                            cmyk[3],
                        ];
                        enforce_tac(&mut cand, spec.tac_limit);
                        let (c_res, c_err) = eval_err(cand);
                        if c_err < err {
                            cmyk = cand;
                            res = c_res;
                            err = c_err;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        enforce_tac(&mut cmyk, spec.tac_limit);
        cmyk
    }
}

fn build_press_spec(
    dot_gain: [f64; 4],
    yn_n: f64,
    tac_limit: f64,
    gcr_start: f64,
    gcr_strength: f64,
    labs: [[f64; 3]; 16],
) -> CmykPressSpec {
    let inv_n = 1.0 / yn_n;
    let mut primaries_yn = [[0.0_f64; 3]; 16];
    let mut paper_white_xyz = D50_WHITE;
    for (i, lab) in labs.iter().enumerate() {
        let xyz = lab_to_xyz_d50(*lab);
        if i == 0 {
            paper_white_xyz = xyz;
        }
        primaries_yn[i] = [
            xyz[0].max(0.0).powf(inv_n),
            xyz[1].max(0.0).powf(inv_n),
            xyz[2].max(0.0).powf(inv_n),
        ];
    }
    // Compute media-relative L* of richest reachable black (C=0.75, M=0.65, Y=0.65, K=0.95 clamped to TAC)
    let mut rich_black = [0.75, 0.65, 0.65, 0.95];
    enforce_tac(&mut rich_black, tac_limit);
    let mut temp = CmykPressSpec {
        dot_gain,
        yn_n,
        tac_limit,
        gcr_start,
        gcr_strength,
        primaries_yn,
        paper_white_xyz,
        black_point_l: 10.0,
    };
    let dark_abs = eval_neugebauer_abs_xyz(&temp, rich_black);
    let dark_rel = [
        dark_abs[0] * (D50_WHITE[0] / paper_white_xyz[0]),
        dark_abs[1] * (D50_WHITE[1] / paper_white_xyz[1]),
        dark_abs[2] * (D50_WHITE[2] / paper_white_xyz[2]),
    ];
    temp.black_point_l = xyz_d50_to_lab(dark_rel)[0].clamp(5.0, 25.0);
    temp
}

#[inline]
fn apply_dot_gain(v: f64, gain: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    (v + gain * 4.0 * v * (1.0 - v)).clamp(0.0, 1.0)
}

fn eval_neugebauer_abs_xyz(spec: &CmykPressSpec, cmyk: [f64; 4]) -> [f64; 3] {
    let c = apply_dot_gain(cmyk[0], spec.dot_gain[0]);
    let m = apply_dot_gain(cmyk[1], spec.dot_gain[1]);
    let y = apply_dot_gain(cmyk[2], spec.dot_gain[2]);
    let k = apply_dot_gain(cmyk[3], spec.dot_gain[3]);
    let ic = 1.0 - c;
    let im = 1.0 - m;
    let iy = 1.0 - y;
    let ik = 1.0 - k;

    let weights = [
        ic * im * iy * ik, // 0: W
        c * im * iy * ik,  // 1: C
        ic * m * iy * ik,  // 2: M
        c * m * iy * ik,   // 3: CM
        ic * im * y * ik,  // 4: Y
        c * im * y * ik,   // 5: CY
        ic * m * y * ik,   // 6: MY
        c * m * y * ik,    // 7: CMY
        ic * im * iy * k,  // 8: K
        c * im * iy * k,   // 9: CK
        ic * m * iy * k,   // 10: MK
        c * m * iy * k,    // 11: CMK
        ic * im * y * k,   // 12: YK
        c * im * y * k,    // 13: CYK
        ic * m * y * k,    // 14: MYK
        c * m * y * k,     // 15: CMYK
    ];

    let mut acc = [0.0_f64; 3];
    for (w, p) in weights.iter().zip(spec.primaries_yn.iter()) {
        acc[0] += w * p[0];
        acc[1] += w * p[1];
        acc[2] += w * p[2];
    }
    let n = spec.yn_n;
    [
        acc[0].max(0.0).powf(n),
        acc[1].max(0.0).powf(n),
        acc[2].max(0.0).powf(n),
    ]
}

/// Scales CMY proportionally if total ink coverage `C + M + Y + K` exceeds `tac_limit`.
fn enforce_tac(cmyk: &mut [f64; 4], tac_limit: f64) {
    for ch in cmyk.iter_mut() {
        *ch = if ch.is_finite() {
            ch.clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    let sum = cmyk[0] + cmyk[1] + cmyk[2] + cmyk[3];
    if sum > tac_limit {
        let cmy_sum = cmyk[0] + cmyk[1] + cmyk[2];
        let allowed_cmy = (tac_limit - cmyk[3]).max(0.0);
        if cmy_sum > 1e-9 {
            let scale = (allowed_cmy / cmy_sum).clamp(0.0, 1.0);
            cmyk[0] *= scale;
            cmyk[1] *= scale;
            cmyk[2] *= scale;
        }
    }
}

// =============================================================================
// Standard Matrices & D50 PCS Color Space Conversions
// =============================================================================

/// Bradford-adapted sRGB linear -> CIE D50 PCS XYZ matrix (exact ICC v4 standard).
const SRGB_TO_XYZ_D50: [[f64; 3]; 3] = [
    [0.436_074_7, 0.385_064_9, 0.143_080_4],
    [0.222_504_5, 0.716_878_6, 0.060_616_9],
    [0.013_932_2, 0.097_104_5, 0.714_173_3],
];

/// Inverse CIE D50 PCS XYZ -> sRGB linear matrix.
const XYZ_D50_TO_SRGB: [[f64; 3]; 3] = [
    [3.133_856_1, -1.616_866_7, -0.490_614_6],
    [-0.978_768_4, 1.916_141_5, 0.033_454_0],
    [0.071_945_3, -0.228_991_4, 1.405_242_7],
];

/// Bradford-adapted Display P3 linear -> CIE D50 PCS XYZ matrix.
const DISPLAY_P3_TO_XYZ_D50: [[f64; 3]; 3] = [
    [0.515_121_0, 0.291_978_0, 0.157_121_0],
    [0.241_182_0, 0.692_236_0, 0.066_582_0],
    [-0.001_049_0, 0.041_882_0, 0.784_377_0],
];

#[inline]
fn clamp_unit_finite(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[inline]
fn sanitize_finite(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

#[inline]
fn srgb_to_linear(c: f64) -> f64 {
    let c = clamp_unit_finite(c);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(c: f64) -> f64 {
    let c = clamp_unit_finite(c);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        (1.055 * c.powf(1.0 / 2.4) - 0.055).clamp(0.0, 1.0)
    }
}

fn srgb_to_xyz_d50(rgb: [f64; 3]) -> [f64; 3] {
    mat3_mul_vec(
        &SRGB_TO_XYZ_D50,
        [
            srgb_to_linear(rgb[0]),
            srgb_to_linear(rgb[1]),
            srgb_to_linear(rgb[2]),
        ],
    )
}

fn xyz_d50_to_srgb(xyz: [f64; 3]) -> [f64; 3] {
    let lin = mat3_mul_vec(&XYZ_D50_TO_SRGB, xyz);
    [
        linear_to_srgb(lin[0]),
        linear_to_srgb(lin[1]),
        linear_to_srgb(lin[2]),
    ]
}

#[inline]
fn lab_f(t: f64) -> f64 {
    if t > LAB_EPSILON {
        t.cbrt()
    } else {
        (LAB_KAPPA * t + 16.0) / 116.0
    }
}

#[inline]
fn lab_f_inv(t: f64) -> f64 {
    let t3 = t * t * t;
    if t3 > LAB_EPSILON {
        t3
    } else {
        (116.0 * t - 16.0) / LAB_KAPPA
    }
}

fn xyz_d50_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let fx = lab_f((sanitize_finite(xyz[0]) / D50_WHITE[0]).max(0.0));
    let fy = lab_f((sanitize_finite(xyz[1]) / D50_WHITE[1]).max(0.0));
    let fz = lab_f((sanitize_finite(xyz[2]) / D50_WHITE[2]).max(0.0));
    [
        (116.0 * fy - 16.0).clamp(0.0, 100.0),
        500.0 * (fx - fy),
        200.0 * (fy - fz),
    ]
}

fn lab_to_xyz_d50(lab: [f64; 3]) -> [f64; 3] {
    let l = sanitize_finite(lab[0]).clamp(0.0, 100.0);
    let a = sanitize_finite(lab[1]);
    let b = sanitize_finite(lab[2]);
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let x = lab_f_inv(fx) * D50_WHITE[0];
    let y = if l > LAB_KAPPA * LAB_EPSILON {
        fy * fy * fy * D50_WHITE[1]
    } else {
        (l / LAB_KAPPA) * D50_WHITE[1]
    };
    let z = lab_f_inv(fz) * D50_WHITE[2];
    [x.max(0.0), y.max(0.0), z.max(0.0)]
}

#[inline]
fn lab_dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dl = a[0] - b[0];
    let da = a[1] - b[1];
    let db = a[2] - b[2];
    dl * dl + da * da + db * db
}

#[inline]
fn mat3_mul_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn mat3_mul_mat3(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0_f64; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    out
}

fn mat3_invert(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
    let c01 = -(m[1][0] * m[2][2] - m[1][2] * m[2][0]);
    let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];

    let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;

    let c10 = -(m[0][1] * m[2][2] - m[0][2] * m[2][1]);
    let c11 = m[0][0] * m[2][2] - m[0][2] * m[2][0];
    let c12 = -(m[0][0] * m[2][1] - m[0][1] * m[2][0]);

    let c20 = m[0][1] * m[1][2] - m[0][2] * m[1][1];
    let c21 = -(m[0][0] * m[1][2] - m[0][2] * m[1][0]);
    let c22 = m[0][0] * m[1][1] - m[0][1] * m[1][0];

    let inv = [
        [c00 * inv_det, c10 * inv_det, c20 * inv_det],
        [c01 * inv_det, c11 * inv_det, c21 * inv_det],
        [c02 * inv_det, c12 * inv_det, c22 * inv_det],
    ];
    if inv.iter().flatten().all(|v| v.is_finite()) {
        Some(inv)
    } else {
        None
    }
}

/// Decodes normalized `[0..1]` LUT output values into D50 CIE XYZ according to ICC PCS & version.
fn decode_lut_pcs_to_xyz(
    vals: [f64; 3],
    pcs: IccPcs,
    is_16bit: bool,
    version_major: u8,
) -> [f64; 3] {
    match pcs {
        IccPcs::Xyz => {
            // ICC u1Fixed15Number encodes [0.0 .. 1.99997] across [0..65535]
            let scale = 65_535.0 / 32_768.0;
            [vals[0] * scale, vals[1] * scale, vals[2] * scale]
        }
        IccPcs::Lab | IccPcs::Unknown(_) => {
            let (l, a, b) = if is_16bit && version_major < 4 {
                // Legacy ICC v2 16-bit Lab encoding: L = u16 / 65280 * 100, a/b = u16 / 256 - 128
                let l = (vals[0] * (65_535.0 / 65_280.0) * 100.0).clamp(0.0, 100.0);
                let a = vals[1] * (65_535.0 / 256.0) - 128.0;
                let b = vals[2] * (65_535.0 / 256.0) - 128.0;
                (l, a, b)
            } else {
                // ICC v4 (and 8-bit mft1) Lab encoding: L = v * 100, a/b = v * 255 - 128
                let l = (vals[0] * 100.0).clamp(0.0, 100.0);
                let a = vals[1] * 255.0 - 128.0;
                let b = vals[2] * 255.0 - 128.0;
                (l, a, b)
            };
            lab_to_xyz_d50([l, a, b])
        }
    }
}

/// Encodes D50 CIE XYZ into normalized `[0..1]` LUT input channels according to ICC PCS & version.
fn encode_xyz_to_lut_pcs(
    xyz: [f64; 3],
    pcs: IccPcs,
    is_16bit: bool,
    version_major: u8,
) -> [f64; 3] {
    match pcs {
        IccPcs::Xyz => {
            let inv = 32_768.0 / 65_535.0;
            [
                (xyz[0] * inv).clamp(0.0, 1.0),
                (xyz[1] * inv).clamp(0.0, 1.0),
                (xyz[2] * inv).clamp(0.0, 1.0),
            ]
        }
        IccPcs::Lab | IccPcs::Unknown(_) => {
            let lab = xyz_d50_to_lab(xyz);
            if is_16bit && version_major < 4 {
                [
                    ((lab[0] / 100.0) * (65_280.0 / 65_535.0)).clamp(0.0, 1.0),
                    ((lab[1] + 128.0) * (256.0 / 65_535.0)).clamp(0.0, 1.0),
                    ((lab[2] + 128.0) * (256.0 / 65_535.0)).clamp(0.0, 1.0),
                ]
            } else {
                [
                    (lab[0] / 100.0).clamp(0.0, 1.0),
                    ((lab[1] + 128.0) / 255.0).clamp(0.0, 1.0),
                    ((lab[2] + 128.0) / 255.0).clamp(0.0, 1.0),
                ]
            }
        }
    }
}

#[inline]
fn eval_1d_table(table: &[f64], x: f64) -> f64 {
    let x = clamp_unit_finite(x);
    let n = table.len();
    if n == 0 {
        return x;
    }
    if n == 1 {
        return clamp_unit_finite(table[0]);
    }
    let s = x * ((n - 1) as f64);
    let i0 = (s.floor() as usize).min(n - 1);
    let i1 = (i0 + 1).min(n - 1);
    let t = s - (i0 as f64);
    clamp_unit_finite(table[i0] + (table[i1] - table[i0]) * t)
}

// =============================================================================
// Strictly Bounded Binary Tag Parsers
// =============================================================================

fn read_fourcc(data: &[u8], off: usize) -> CoreResult<[u8; 4]> {
    let slice = data.get(off..off + 4).ok_or_else(|| {
        CoreError::InvalidColor(format!("unexpected EOF reading ICC fourcc at {off}"))
    })?;
    Ok([slice[0], slice[1], slice[2], slice[3]])
}

fn read_be_u16(data: &[u8], off: usize) -> CoreResult<u16> {
    let slice = data
        .get(off..off + 2)
        .ok_or_else(|| CoreError::InvalidColor(format!("unexpected EOF reading u16 at {off}")))?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_be_u32(data: &[u8], off: usize) -> CoreResult<u32> {
    let slice = data
        .get(off..off + 4)
        .ok_or_else(|| CoreError::InvalidColor(format!("unexpected EOF reading u32 at {off}")))?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_be_i32(data: &[u8], off: usize) -> CoreResult<i32> {
    let slice = data
        .get(off..off + 4)
        .ok_or_else(|| CoreError::InvalidColor(format!("unexpected EOF reading i32 at {off}")))?;
    Ok(i32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_s15_fixed16(data: &[u8], off: usize) -> CoreResult<f64> {
    Ok(f64::from(read_be_i32(data, off)?) / 65_536.0)
}

fn parse_xyz_tag(tag: &[u8]) -> CoreResult<[f64; 3]> {
    if tag.len() < 20 || &tag[0..4] != b"XYZ " {
        return Err(CoreError::InvalidColor("malformed ICC XYZ tag".into()));
    }
    Ok([
        read_s15_fixed16(tag, 8)?,
        read_s15_fixed16(tag, 12)?,
        read_s15_fixed16(tag, 16)?,
    ])
}

fn parse_sf32_chad_tag(tag: &[u8]) -> CoreResult<[[f64; 3]; 3]> {
    if tag.len() < 44 || &tag[0..4] != b"sf32" {
        return Err(CoreError::InvalidColor(
            "malformed ICC chad sf32 tag".into(),
        ));
    }
    let mut m = [[0.0_f64; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            let v = read_s15_fixed16(tag, 8 + (r * 3 + c) * 4)?;
            if !v.is_finite() {
                return Err(CoreError::InvalidColor("non-finite ICC chad entry".into()));
            }
            *cell = v;
        }
    }
    Ok(m)
}

fn parse_desc_or_mluc_tag(tag: &[u8]) -> CoreResult<String> {
    if tag.len() < 12 {
        return Err(CoreError::InvalidColor(
            "truncated ICC description tag".into(),
        ));
    }
    match &tag[0..4] {
        b"desc" => {
            let ascii_len = (read_be_u32(tag, 8)? as usize).min(MAX_ICC_DESC_BYTES);
            let end = 12usize.saturating_add(ascii_len).min(tag.len());
            let raw = &tag[12..end];
            let trimmed = raw
                .iter()
                .position(|&b| b == 0)
                .map_or(raw, |nul| &raw[..nul]);
            Ok(String::from_utf8_lossy(trimmed).into_owned())
        }
        b"mluc" => {
            if tag.len() < 16 {
                return Err(CoreError::InvalidColor("truncated ICC mluc tag".into()));
            }
            let record_count = (read_be_u32(tag, 8)? as usize).min(256);
            let record_size = read_be_u32(tag, 12)? as usize;
            if record_size < 12 || record_count == 0 {
                return Ok(String::new());
            }
            // Prefer first valid UTF-16BE localized string record within strict per-record and total scan caps
            let mut scanned_bytes = 0usize;
            for i in 0..record_count {
                let Some(rec_off) = i
                    .checked_mul(record_size)
                    .and_then(|off| 16usize.checked_add(off))
                else {
                    break;
                };
                let Some(rec_end) = rec_off.checked_add(12) else {
                    break;
                };
                if rec_end > tag.len() {
                    break;
                }
                let str_len = read_be_u32(tag, rec_off + 4)? as usize;
                let str_off = read_be_u32(tag, rec_off + 8)? as usize;
                if !(2..=MAX_ICC_DESC_BYTES).contains(&str_len) {
                    continue;
                }
                let Some(str_end) = str_off.checked_add(str_len) else {
                    continue;
                };
                if str_end > tag.len() {
                    continue;
                }
                if scanned_bytes.saturating_add(str_len) > MAX_ICC_MLUC_TOTAL_SCAN_BYTES {
                    break;
                }
                scanned_bytes += str_len;
                let units: Vec<u16> = tag[str_off..str_end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes(*pair))
                    .take_while(|&u| u != 0)
                    .collect();
                if let Ok(decoded) = String::from_utf16(&units) {
                    if !decoded.is_empty() {
                        return Ok(decoded);
                    }
                }
            }
            Ok(String::new())
        }
        _ => Err(CoreError::InvalidColor(
            "unsupported ICC description tag type".into(),
        )),
    }
}

fn parse_trc_tag(tag: &[u8]) -> CoreResult<IccToneCurve> {
    if tag.len() < 12 {
        return Err(CoreError::InvalidColor("truncated ICC TRC tag".into()));
    }
    match &tag[0..4] {
        b"curv" => {
            let count = read_be_u32(tag, 8)? as usize;
            if count == 0 {
                Ok(IccToneCurve::Identity)
            } else if count == 1 {
                let raw = read_be_u16(tag, 12)?;
                Ok(IccToneCurve::Gamma(f64::from(raw) / 256.0))
            } else {
                if count > MAX_ICC_CURVE_ENTRIES {
                    return Err(CoreError::InvalidColor(format!(
                        "ICC curv table length {count} exceeds limit {MAX_ICC_CURVE_ENTRIES}"
                    )));
                }
                let needed = 12usize
                    .checked_add(count.checked_mul(2).ok_or_else(|| {
                        CoreError::InvalidColor("ICC curv byte length overflow".into())
                    })?)
                    .ok_or_else(|| CoreError::InvalidColor("ICC curv offset overflow".into()))?;
                if needed > tag.len() {
                    return Err(CoreError::InvalidColor("ICC curv table truncated".into()));
                }
                let mut table = Vec::with_capacity(count);
                for i in 0..count {
                    table.push(f64::from(read_be_u16(tag, 12 + i * 2)?) / 65_535.0);
                }
                Ok(IccToneCurve::Table(table))
            }
        }
        b"para" => {
            let func_type = read_be_u16(tag, 8)?;
            let param_count: usize = match func_type {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => {
                    return Err(CoreError::InvalidColor(format!(
                        "unsupported ICC para function type {func_type}"
                    )));
                }
            };
            if 12 + param_count * 4 > tag.len() {
                return Err(CoreError::InvalidColor("ICC para curve truncated".into()));
            }
            let mut params = [0.0_f64; 7];
            for (i, slot) in params.iter_mut().enumerate().take(param_count) {
                *slot = read_s15_fixed16(tag, 12 + i * 4)?;
            }
            Ok(IccToneCurve::Parametric { func_type, params })
        }
        _ => Err(CoreError::InvalidColor(format!(
            "unsupported ICC TRC tag type '{}'",
            String::from_utf8_lossy(&tag[0..4])
        ))),
    }
}

/// Parses an arbitrary `mft1`, `mft2`, `mAB `, or `mBA ` LUT tag into either `IccLut` or `IccV4Lut`.
fn parse_any_lut_tag(tag: &[u8]) -> CoreResult<(Option<IccLut>, Option<IccV4Lut>)> {
    if tag.len() < 4 {
        return Err(CoreError::InvalidColor("truncated ICC LUT tag".into()));
    }
    match &tag[0..4] {
        b"mft1" | b"mft2" => parse_lut_tag(tag).map(|lut| (Some(lut), None)),
        b"mAB " | b"mBA " => parse_mab_mba_tag(tag).map(|v4| (None, v4)),
        sig => Err(CoreError::InvalidColor(format!(
            "unsupported ICC LUT tag type '{}'",
            String::from_utf8_lossy(sig)
        ))),
    }
}

/// Parses an ICC v4 `mAB ` (`lutAToBType`) or `mBA ` (`lutBToAType`) multi-stage LUT tag with
/// strict offset/clut bounds checks; returns `Ok(None)` if all stage offsets are zero (no-op stub).
fn parse_mab_mba_tag(tag: &[u8]) -> CoreResult<Option<IccV4Lut>> {
    if tag.len() < 32 {
        return Err(CoreError::InvalidColor(
            "truncated ICC v4 mAB/mBA LUT tag".into(),
        ));
    }
    let is_b_to_a = &tag[0..4] == b"mBA ";
    let in_channels = usize::from(tag[8]);
    let out_channels = usize::from(tag[9]);
    if !(1..=4).contains(&in_channels) || !(1..=4).contains(&out_channels) {
        return Err(CoreError::InvalidColor(format!(
            "ICC v4 LUT invalid channel counts ({in_channels} -> {out_channels})"
        )));
    }

    let b_off = read_be_u32(tag, 12)? as usize;
    let mat_off = read_be_u32(tag, 16)? as usize;
    let m_off = read_be_u32(tag, 20)? as usize;
    let clut_off = read_be_u32(tag, 24)? as usize;
    let a_off = read_be_u32(tag, 28)? as usize;

    // Empty stub mAB/mBA headers (all stage offsets zero) delegate cleanly to matrix+TRC.
    if b_off == 0 && mat_off == 0 && m_off == 0 && clut_off == 0 && a_off == 0 {
        return Ok(None);
    }

    let b_count = if is_b_to_a { in_channels } else { out_channels };
    let a_count = if is_b_to_a { out_channels } else { in_channels };

    let b_curves = if b_off != 0 {
        parse_curve_sequence(tag, b_off, b_count)?
    } else {
        Vec::new()
    };
    let m_curves = if m_off != 0 {
        parse_curve_sequence(tag, m_off, 3.min(b_count))?
    } else {
        Vec::new()
    };
    let matrix = if mat_off != 0 {
        let end = mat_off
            .checked_add(48)
            .ok_or_else(|| CoreError::InvalidColor("ICC v4 matrix offset overflow".into()))?;
        if end > tag.len() {
            return Err(CoreError::InvalidColor(
                "ICC v4 matrix tag truncated".into(),
            ));
        }
        let mut m = [[0.0_f64; 4]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().take(3).enumerate() {
                *cell = read_s15_fixed16(tag, mat_off + (r * 3 + c) * 4)?;
            }
            row[3] = read_s15_fixed16(tag, mat_off + 36 + r * 4)?;
        }
        Some(m)
    } else {
        None
    };

    let (clut_grid, clut_is_16bit, clut) = if clut_off != 0 {
        let hdr_end = clut_off
            .checked_add(20)
            .ok_or_else(|| CoreError::InvalidColor("ICC v4 CLUT header overflow".into()))?;
        if hdr_end > tag.len() {
            return Err(CoreError::InvalidColor(
                "ICC v4 CLUT header truncated".into(),
            ));
        }
        let mut grid = [1usize; 4];
        let mut total_nodes = 1usize;
        for (axis, g_slot) in grid.iter_mut().enumerate().take(in_channels) {
            let g = usize::from(tag[clut_off + axis]);
            if !(2..=MAX_ICC_GRID_POINTS).contains(&g) {
                return Err(CoreError::InvalidColor(format!(
                    "ICC v4 CLUT grid axis {axis}={g} outside 2..={MAX_ICC_GRID_POINTS}"
                )));
            }
            *g_slot = g;
            total_nodes = total_nodes
                .checked_mul(g)
                .ok_or_else(|| CoreError::InvalidColor("ICC v4 CLUT node count overflow".into()))?;
        }
        let precision = tag[clut_off + 16];
        let is_16 = match precision {
            1 => false,
            2 => true,
            _ => {
                return Err(CoreError::InvalidColor(format!(
                    "ICC v4 CLUT invalid precision {precision}"
                )));
            }
        };
        let scalars = total_nodes
            .checked_mul(out_channels)
            .ok_or_else(|| CoreError::InvalidColor("ICC v4 CLUT scalar count overflow".into()))?;
        if scalars > MAX_ICC_CLUT_ENTRIES {
            return Err(CoreError::InvalidColor(format!(
                "ICC v4 CLUT scalars {scalars} exceeds limit {MAX_ICC_CLUT_ENTRIES}"
            )));
        }
        let elem_bytes = if is_16 { 2usize } else { 1usize };
        let data_bytes = scalars
            .checked_mul(elem_bytes)
            .ok_or_else(|| CoreError::InvalidColor("ICC v4 CLUT byte size overflow".into()))?;
        let data_end = hdr_end
            .checked_add(data_bytes)
            .ok_or_else(|| CoreError::InvalidColor("ICC v4 CLUT data end overflow".into()))?;
        if data_end > tag.len() {
            return Err(CoreError::InvalidColor(
                "ICC v4 CLUT payload truncated".into(),
            ));
        }
        let mut vals = Vec::with_capacity(scalars);
        let mut off = hdr_end;
        if is_16 {
            for _ in 0..scalars {
                vals.push(f64::from(read_be_u16(tag, off)?) / 65_535.0);
                off += 2;
            }
        } else {
            for _ in 0..scalars {
                vals.push(f64::from(tag[off]) / 255.0);
                off += 1;
            }
        }
        (grid, is_16, vals)
    } else {
        ([0usize; 4], true, Vec::new())
    };

    let a_curves = if a_off != 0 {
        parse_curve_sequence(tag, a_off, a_count)?
    } else {
        Vec::new()
    };

    Ok(Some(IccV4Lut {
        is_b_to_a,
        in_channels,
        out_channels,
        b_curves,
        m_curves,
        matrix,
        a_curves,
        clut_grid,
        clut_is_16bit,
        clut,
    }))
}

fn parse_curve_sequence(
    tag: &[u8],
    start_off: usize,
    count: usize,
) -> CoreResult<Vec<IccToneCurve>> {
    let mut curves = Vec::with_capacity(count);
    let mut pos = start_off;
    for _ in 0..count {
        if pos + 12 > tag.len() {
            return Err(CoreError::InvalidColor(
                "ICC v4 curve entry truncated".into(),
            ));
        }
        let curve = parse_trc_tag(&tag[pos..])?;
        let raw_len = match &tag[pos..pos + 4] {
            b"curv" => {
                let n = read_be_u32(tag, pos + 8)? as usize;
                let entry_bytes = n.saturating_mul(2);
                12usize.saturating_add(entry_bytes)
            }
            b"para" => {
                let ft = read_be_u16(tag, pos + 8)?;
                let params = match ft {
                    0 => 1usize,
                    1 => 3,
                    2 => 4,
                    3 => 5,
                    4 => 7,
                    _ => 0,
                };
                12usize + params * 4
            }
            _ => 12usize,
        };
        let aligned = (raw_len + 3) & !3;
        pos = pos
            .checked_add(aligned)
            .ok_or_else(|| CoreError::InvalidColor("ICC v4 curve sequence overflow".into()))?;
        curves.push(curve);
    }
    Ok(curves)
}

fn parse_lut_tag(tag: &[u8]) -> CoreResult<IccLut> {
    if tag.len() < 48 {
        return Err(CoreError::InvalidColor("truncated ICC LUT tag".into()));
    }
    let sig = &tag[0..4];
    let is_16bit = match sig {
        b"mft1" => false,
        b"mft2" => true,
        _ => {
            return Err(CoreError::InvalidColor(format!(
                "unsupported ICC LUT tag type '{}'",
                String::from_utf8_lossy(sig)
            )));
        }
    };

    let in_channels = usize::from(tag[8]);
    let out_channels = usize::from(tag[9]);
    let grid_points = usize::from(tag[10]);

    if !(1..=4).contains(&in_channels) || !(1..=4).contains(&out_channels) {
        return Err(CoreError::InvalidColor(format!(
            "ICC LUT invalid channel counts ({in_channels} -> {out_channels})"
        )));
    }
    if !(2..=MAX_ICC_GRID_POINTS).contains(&grid_points) {
        return Err(CoreError::InvalidColor(format!(
            "ICC LUT grid points {grid_points} outside safe range 2..={MAX_ICC_GRID_POINTS}"
        )));
    }

    let mut matrix = [[0.0_f64; 3]; 3];
    for (r, row) in matrix.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = read_s15_fixed16(tag, 12 + (r * 3 + c) * 4)?;
        }
    }

    let (in_entries, out_entries, mut cursor) = if is_16bit {
        if tag.len() < 52 {
            return Err(CoreError::InvalidColor("truncated ICC mft2 header".into()));
        }
        let ie = usize::from(read_be_u16(tag, 48)?);
        let oe = usize::from(read_be_u16(tag, 50)?);
        if !(2..=4096).contains(&ie) || !(2..=4096).contains(&oe) {
            return Err(CoreError::InvalidColor(format!(
                "ICC mft2 1D table sizes ({ie}, {oe}) outside safe range 2..=4096"
            )));
        }
        (ie, oe, 52usize)
    } else {
        (256usize, 256usize, 48usize)
    };

    let mut clut_nodes = 1usize;
    for _ in 0..in_channels {
        clut_nodes = clut_nodes
            .checked_mul(grid_points)
            .ok_or_else(|| CoreError::InvalidColor("ICC CLUT size overflow".into()))?;
    }
    let clut_scalars = clut_nodes
        .checked_mul(out_channels)
        .ok_or_else(|| CoreError::InvalidColor("ICC CLUT scalar count overflow".into()))?;
    if clut_scalars > MAX_ICC_CLUT_ENTRIES {
        return Err(CoreError::InvalidColor(format!(
            "ICC CLUT scalar count {clut_scalars} exceeds limit {MAX_ICC_CLUT_ENTRIES}"
        )));
    }

    let elem_bytes = if is_16bit { 2usize } else { 1usize };
    let in_bytes = in_channels
        .checked_mul(in_entries)
        .and_then(|n| n.checked_mul(elem_bytes))
        .ok_or_else(|| CoreError::InvalidColor("ICC LUT input table size overflow".into()))?;
    let clut_bytes = clut_scalars
        .checked_mul(elem_bytes)
        .ok_or_else(|| CoreError::InvalidColor("ICC CLUT byte size overflow".into()))?;
    let out_bytes = out_channels
        .checked_mul(out_entries)
        .and_then(|n| n.checked_mul(elem_bytes))
        .ok_or_else(|| CoreError::InvalidColor("ICC LUT output table size overflow".into()))?;
    let total_needed = cursor
        .checked_add(in_bytes)
        .and_then(|n| n.checked_add(clut_bytes))
        .and_then(|n| n.checked_add(out_bytes))
        .ok_or_else(|| CoreError::InvalidColor("ICC LUT total size overflow".into()))?;
    if total_needed > tag.len() {
        return Err(CoreError::InvalidColor(format!(
            "ICC LUT tag truncated: need {total_needed} bytes, got {}",
            tag.len()
        )));
    }

    let read_norm_vec = |data: &[u8], off: &mut usize, count: usize| -> CoreResult<Vec<f64>> {
        let mut out = Vec::with_capacity(count);
        if is_16bit {
            for _ in 0..count {
                out.push(f64::from(read_be_u16(data, *off)?) / 65_535.0);
                *off += 2;
            }
        } else {
            for _ in 0..count {
                out.push(f64::from(data[*off]) / 255.0);
                *off += 1;
            }
        }
        Ok(out)
    };

    let mut input_tables = Vec::with_capacity(in_channels);
    for _ in 0..in_channels {
        input_tables.push(read_norm_vec(tag, &mut cursor, in_entries)?);
    }
    let clut = read_norm_vec(tag, &mut cursor, clut_scalars)?;
    let mut output_tables = Vec::with_capacity(out_channels);
    for _ in 0..out_channels {
        output_tables.push(read_norm_vec(tag, &mut cursor, out_entries)?);
    }

    Ok(IccLut {
        in_channels,
        out_channels,
        grid_points,
        is_16bit,
        matrix,
        input_tables,
        clut,
        output_tables,
    })
}

fn push_s15_fixed16(buf: &mut Vec<u8>, v: f64) {
    let fixed = (v * 65_536.0).round() as i32;
    buf.extend_from_slice(&fixed.to_be_bytes());
}

fn encode_icc_profile_bytes(
    version_major: u8,
    profile_class: &[u8; 4],
    color_space: &[u8; 4],
    pcs: &[u8; 4],
    intent: u32,
    tags: &[(&[u8; 4], Vec<u8>)],
) -> Vec<u8> {
    let mut header = vec![0u8; 128];
    header[4..8].copy_from_slice(b"core");
    header[8] = version_major;
    header[9] = 0x20;
    header[12..16].copy_from_slice(profile_class);
    header[16..20].copy_from_slice(color_space);
    header[20..24].copy_from_slice(pcs);
    header[36..40].copy_from_slice(b"acsp");
    header[64..68].copy_from_slice(&intent.to_be_bytes());
    let mut ill = Vec::new();
    push_s15_fixed16(&mut ill, D50_WHITE[0]);
    push_s15_fixed16(&mut ill, D50_WHITE[1]);
    push_s15_fixed16(&mut ill, D50_WHITE[2]);
    header[68..80].copy_from_slice(&ill);

    let tag_table_len = 4 + tags.len() * 12;
    let mut offset = 128 + tag_table_len;
    let mut table = Vec::with_capacity(tag_table_len);
    table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    let mut body = Vec::new();

    for (sig, data) in tags {
        table.extend_from_slice(*sig);
        table.extend_from_slice(&(offset as u32).to_be_bytes());
        table.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        offset = 128 + tag_table_len + body.len();
    }

    let total = 128 + table.len() + body.len();
    header[0..4].copy_from_slice(&(total as u32).to_be_bytes());

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&header);
    out.extend_from_slice(&table);
    out.extend_from_slice(&body);
    out
}

fn encode_xyz_tag(xyz: [f64; 3]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"XYZ ");
    v.extend_from_slice(&0u32.to_be_bytes());
    push_s15_fixed16(&mut v, xyz[0]);
    push_s15_fixed16(&mut v, xyz[1]);
    push_s15_fixed16(&mut v, xyz[2]);
    v
}

fn encode_desc_tag(name: &str) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"desc");
    v.extend_from_slice(&0u32.to_be_bytes());
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    v.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    v.extend_from_slice(&bytes);
    v
}

fn encode_curv_gamma_tag(gamma: f64) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"curv");
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    let u8f8 = (gamma * 256.0).round() as u16;
    v.extend_from_slice(&u8f8.to_be_bytes());
    v
}

/// Generates a compact standards-compliant ICC v2 Matrix+TRC RGB profile (e.g., for PDF `/OutputIntents`).
#[must_use]
pub fn generate_minimal_srgb_icc_v2(description: &str) -> Vec<u8> {
    encode_icc_profile_bytes(
        2,
        b"mntr",
        b"RGB ",
        b"XYZ ",
        0,
        &[
            (b"desc", encode_desc_tag(description)),
            (b"wtpt", encode_xyz_tag(D65_WHITE)),
            (
                b"rXYZ",
                encode_xyz_tag([
                    SRGB_TO_XYZ_D50[0][0],
                    SRGB_TO_XYZ_D50[1][0],
                    SRGB_TO_XYZ_D50[2][0],
                ]),
            ),
            (
                b"gXYZ",
                encode_xyz_tag([
                    SRGB_TO_XYZ_D50[0][1],
                    SRGB_TO_XYZ_D50[1][1],
                    SRGB_TO_XYZ_D50[2][1],
                ]),
            ),
            (
                b"bXYZ",
                encode_xyz_tag([
                    SRGB_TO_XYZ_D50[0][2],
                    SRGB_TO_XYZ_D50[1][2],
                    SRGB_TO_XYZ_D50[2][2],
                ]),
            ),
            (b"rTRC", encode_curv_gamma_tag(2.2)),
            (b"gTRC", encode_curv_gamma_tag(2.2)),
            (b"bTRC", encode_curv_gamma_tag(2.2)),
        ],
    )
}

/// Generates a compact standards-compliant ICC v2 Output CMYK profile with 4D->3D `A2B0`
/// and 3D->4D `B2A0` `mft1` lookup tables derived from `builtin` (suitable for PDF `/OutputIntents`).
#[must_use]
pub fn generate_minimal_cmyk_icc_v2(description: &str, builtin: BuiltinIccProfile) -> Vec<u8> {
    let press = builtin.as_cmyk_press();

    let encode_mft1 = |in_ch: u8, out_ch: u8, grid: u8, clut: &[u8]| -> Vec<u8> {
        let mut tag =
            Vec::with_capacity(48 + (in_ch as usize) * 256 + clut.len() + (out_ch as usize) * 256);
        tag.extend_from_slice(b"mft1");
        tag.extend_from_slice(&0u32.to_be_bytes());
        tag.push(in_ch);
        tag.push(out_ch);
        tag.push(grid);
        tag.push(0);
        // 3x3 identity matrix in s15Fixed16
        for row in 0..3 {
            for col in 0..3 {
                push_s15_fixed16(&mut tag, if row == col { 1.0 } else { 0.0 });
            }
        }
        for _ in 0..in_ch {
            for v in 0..=255u8 {
                tag.push(v);
            }
        }
        tag.extend_from_slice(clut);
        for _ in 0..out_ch {
            for v in 0..=255u8 {
                tag.push(v);
            }
        }
        tag
    };

    // 5x5x5x5 CMYK -> Lab (v2 mft1 Lab encoding: L* = byte/255 * 100, a* = byte - 128, b* = byte - 128)
    let mut a2b_clut = Vec::with_capacity(625 * 3);
    for ic in 0..5 {
        let c = f64::from(ic) / 4.0;
        for im in 0..5 {
            let m = f64::from(im) / 4.0;
            for iy in 0..5 {
                let y = f64::from(iy) / 4.0;
                for ik in 0..5 {
                    let k = f64::from(ik) / 4.0;
                    let xyz =
                        press.cmyk_to_xyz_d50([c, m, y, k], RenderingIntent::RelativeColorimetric);
                    let lab = xyz_d50_to_lab(xyz);
                    a2b_clut.push((lab[0] * 2.55).round().clamp(0.0, 255.0) as u8);
                    a2b_clut.push((lab[1] + 128.0).round().clamp(0.0, 255.0) as u8);
                    a2b_clut.push((lab[2] + 128.0).round().clamp(0.0, 255.0) as u8);
                }
            }
        }
    }

    // 7x7x7 Lab -> CMYK
    let mut b2a_clut = Vec::with_capacity(343 * 4);
    for il in 0..7 {
        let l = (f64::from(il) / 6.0) * 100.0;
        for ia in 0..7 {
            let a = (f64::from(ia) / 6.0) * 255.0 - 128.0;
            for ib in 0..7 {
                let b = (f64::from(ib) / 6.0) * 255.0 - 128.0;
                let fy = (l + 16.0) / 116.0;
                let fx = a / 500.0 + fy;
                let fz = fy - b / 200.0;
                let lab_f_inv = |t: f64| -> f64 {
                    if t * t * t > LAB_EPSILON {
                        t * t * t
                    } else {
                        (116.0 * t - 16.0) / LAB_KAPPA
                    }
                };
                let xyz = [
                    D50_WHITE[0] * lab_f_inv(fx),
                    D50_WHITE[1] * lab_f_inv(fy),
                    D50_WHITE[2] * lab_f_inv(fz),
                ];
                let cmyk = press.xyz_d50_to_cmyk(xyz, RenderingIntent::RelativeColorimetric);
                for ch in cmyk {
                    b2a_clut.push((ch * 255.0).round().clamp(0.0, 255.0) as u8);
                }
            }
        }
    }

    encode_icc_profile_bytes(
        2,
        b"prtr",
        b"CMYK",
        b"Lab ",
        1,
        &[
            (b"desc", encode_desc_tag(description)),
            (b"wtpt", encode_xyz_tag(press.paper_white_xyz())),
            (b"A2B0", encode_mft1(4, 3, 5, &a2b_clut)),
            (b"B2A0", encode_mft1(3, 4, 7, &b2a_clut)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to assemble a valid binary ICC v2 or v4 profile from tags for parser tests.
    fn build_synthetic_icc_bytes(
        version_major: u8,
        profile_class: &[u8; 4],
        color_space: &[u8; 4],
        pcs: &[u8; 4],
        intent: u32,
        tags: &[(&[u8; 4], Vec<u8>)],
    ) -> Vec<u8> {
        encode_icc_profile_bytes(version_major, profile_class, color_space, pcs, intent, tags)
    }

    fn make_xyz_tag(xyz: [f64; 3]) -> Vec<u8> {
        encode_xyz_tag(xyz)
    }

    fn make_desc_tag(name: &str) -> Vec<u8> {
        encode_desc_tag(name)
    }

    fn make_curv_gamma_tag(gamma: f64) -> Vec<u8> {
        encode_curv_gamma_tag(gamma)
    }

    fn make_mluc_tag(name: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"mluc");
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes()); // 1 record
        v.extend_from_slice(&12u32.to_be_bytes()); // record size 12
        v.extend_from_slice(b"enUS");
        let utf16: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        v.extend_from_slice(&(utf16.len() as u32).to_be_bytes());
        v.extend_from_slice(&28u32.to_be_bytes()); // offset 16 + 12 = 28
        v.extend_from_slice(&utf16);
        v
    }

    fn make_para_srgb_tag() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"para");
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&3u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        for p in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            push_s15_fixed16(&mut v, p);
        }
        v
    }

    #[test]
    fn parses_real_icc_v2_and_v4_matrix_trc_profiles() {
        // ICC v4 RGB Display profile with mluc + para type 3 + rXYZ/gXYZ/bXYZ
        let bytes_v4 = build_synthetic_icc_bytes(
            4,
            b"mntr",
            b"RGB ",
            b"XYZ ",
            1,
            &[
                (b"desc", make_mluc_tag("CleanRoom sRGB v4")),
                (b"wtpt", make_xyz_tag(D65_WHITE)),
                (
                    b"rXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][0],
                        SRGB_TO_XYZ_D50[1][0],
                        SRGB_TO_XYZ_D50[2][0],
                    ]),
                ),
                (
                    b"gXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][1],
                        SRGB_TO_XYZ_D50[1][1],
                        SRGB_TO_XYZ_D50[2][1],
                    ]),
                ),
                (
                    b"bXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][2],
                        SRGB_TO_XYZ_D50[1][2],
                        SRGB_TO_XYZ_D50[2][2],
                    ]),
                ),
                (b"rTRC", make_para_srgb_tag()),
                (b"gTRC", make_para_srgb_tag()),
                (b"bTRC", make_para_srgb_tag()),
            ],
        );

        let prof = IccProfile::from_bytes(&bytes_v4).expect("valid v4 ICC");
        assert_eq!(prof.header.version_major, 4);
        assert_eq!(prof.header.color_space, IccColorSpace::Rgb);
        assert_eq!(prof.header.pcs, IccPcs::Xyz);
        assert_eq!(
            prof.header.rendering_intent,
            RenderingIntent::RelativeColorimetric
        );
        assert_eq!(prof.description, "CleanRoom sRGB v4");

        // Roundtrip RGB -> PCS XYZ -> RGB
        let rgb_in = [0.25, 0.65, 0.85];
        let xyz = prof.rgb_to_pcs_xyz(rgb_in).expect("rgb->xyz");
        let rgb_back = prof.pcs_xyz_to_rgb(xyz).expect("xyz->rgb");
        for i in 0..3 {
            assert!(
                (rgb_in[i] - rgb_back[i]).abs() < 1e-3,
                "channel {i}: {} vs {}",
                rgb_in[i],
                rgb_back[i]
            );
        }

        // ICC v2 profile with desc + curv gamma 2.2
        let bytes_v2 = build_synthetic_icc_bytes(
            2,
            b"mntr",
            b"RGB ",
            b"XYZ ",
            0,
            &[
                (b"desc", make_desc_tag("Legacy Gamma 2.2")),
                (b"rTRC", make_curv_gamma_tag(2.2)),
                (b"gTRC", make_curv_gamma_tag(2.2)),
                (b"bTRC", make_curv_gamma_tag(2.2)),
            ],
        );
        let prof_v2 = IccProfile::from_bytes(&bytes_v2).expect("valid v2 ICC");
        assert_eq!(prof_v2.description, "Legacy Gamma 2.2");
        let y = prof_v2.r_trc.as_ref().unwrap().eval(0.5);
        assert!((y - 0.5_f64.powf(2.2)).abs() < 1e-2);
    }

    #[test]
    fn parses_and_interpolates_mft1_3d_tetrahedral_and_mft2_4d_luts() {
        // 1. Build an mft1 (8-bit) 3D -> 4D LUT (Lab -> CMYK, 3x3x3 grid)
        let mut mft1 = Vec::new();
        mft1.extend_from_slice(b"mft1");
        mft1.extend_from_slice(&0u32.to_be_bytes());
        mft1.push(3); // in_channels
        mft1.push(4); // out_channels
        mft1.push(3); // grid_points
        mft1.push(0); // pad
        for r in 0..3 {
            for c in 0..3 {
                push_s15_fixed16(&mut mft1, if r == c { 1.0 } else { 0.0 });
            }
        }
        // 3 identity 256-entry 8-bit input tables
        for _ in 0..3 {
            for i in 0..=255u8 {
                mft1.push(i);
            }
        }
        // 3^3 * 4 CLUT: linear ramp encoding (c = 1-x, m = 1-y, y = 1-z, k = 0.5*(1-x))
        for ix in 0..3 {
            for iy in 0..3 {
                for iz in 0..3 {
                    let fx = ix as f64 / 2.0;
                    let fy = iy as f64 / 2.0;
                    let fz = iz as f64 / 2.0;
                    mft1.push(((1.0 - fx) * 255.0).round() as u8);
                    mft1.push(((1.0 - fy) * 255.0).round() as u8);
                    mft1.push(((1.0 - fz) * 255.0).round() as u8);
                    mft1.push((0.5 * (1.0 - fx) * 255.0).round() as u8);
                }
            }
        }
        // 4 identity 256-entry output tables
        for _ in 0..4 {
            for i in 0..=255u8 {
                mft1.push(i);
            }
        }

        // 2. Build an mft2 (16-bit) 4D -> 3D LUT (CMYK -> Lab, 2x2x2x2 grid)
        let mut mft2 = Vec::new();
        mft2.extend_from_slice(b"mft2");
        mft2.extend_from_slice(&0u32.to_be_bytes());
        mft2.push(4); // in_channels
        mft2.push(3); // out_channels
        mft2.push(2); // grid_points
        mft2.push(0);
        for r in 0..3 {
            for c in 0..3 {
                push_s15_fixed16(&mut mft2, if r == c { 1.0 } else { 0.0 });
            }
        }
        mft2.extend_from_slice(&2u16.to_be_bytes()); // in entries
        mft2.extend_from_slice(&2u16.to_be_bytes()); // out entries
        for _ in 0..4 {
            mft2.extend_from_slice(&0u16.to_be_bytes());
            mft2.extend_from_slice(&65535u16.to_be_bytes());
        }
        for ic in 0..2 {
            for im in 0..2 {
                for iy in 0..2 {
                    for ik in 0..2 {
                        let l_norm = (1.0 - 0.25 * (ic + im + iy + ik) as f64).clamp(0.0, 1.0);
                        let l_u16 = (l_norm * 65_535.0).round() as u16;
                        mft2.extend_from_slice(&l_u16.to_be_bytes());
                        mft2.extend_from_slice(&32_768u16.to_be_bytes());
                        mft2.extend_from_slice(&32_768u16.to_be_bytes());
                    }
                }
            }
        }
        for _ in 0..3 {
            mft2.extend_from_slice(&0u16.to_be_bytes());
            mft2.extend_from_slice(&65535u16.to_be_bytes());
        }

        let icc_bytes = build_synthetic_icc_bytes(
            4,
            b"prtr",
            b"CMYK",
            b"Lab ",
            0,
            &[
                (b"desc", make_mluc_tag("Test CMYK LUT")),
                (b"A2B0", mft2),
                (b"B2A0", mft1),
            ],
        );
        let prof = IccProfile::from_bytes(&icc_bytes).expect("valid CMYK LUT ICC");
        assert!(prof.a2b0.is_some());
        assert!(prof.b2a0.is_some());

        // Test 3D tetrahedral interpolation on linear field (exact!)
        let b2a = prof.b2a0.as_ref().unwrap();
        let out = b2a.evaluate(&[0.3, 0.7, 0.2], false);
        assert!((out[0] - 0.7).abs() < 0.02);
        assert!((out[1] - 0.3).abs() < 0.02);
        assert!((out[2] - 0.8).abs() < 0.02);
        assert!((out[3] - 0.35).abs() < 0.02);

        // Test 4D quadrilinear interpolation on linear field (exact!)
        let a2b = prof.a2b0.as_ref().unwrap();
        let out4 = a2b.evaluate(&[0.5, 0.5, 0.0, 0.0], false);
        assert!((out4[0] - 0.75).abs() < 0.01);
        assert!(prof.cmyk_to_pcs_xyz([0.2, 0.3, 0.4, 0.1]).is_some());
        assert!(prof.pcs_xyz_to_cmyk(D50_WHITE).is_some());
    }

    #[test]
    fn rejects_malformed_and_oversized_icc_inputs_safely() {
        assert!(IccProfile::from_bytes(&[]).is_err());
        assert!(IccProfile::from_bytes(&[0u8; 132]).is_err()); // bad 'acsp' magic

        // Valid magic, bad tag count exceeding bounds
        let mut bad_tags = vec![0u8; 140];
        bad_tags[0..4].copy_from_slice(&140u32.to_be_bytes());
        bad_tags[36..40].copy_from_slice(b"acsp");
        bad_tags[128..132].copy_from_slice(&9999u32.to_be_bytes());
        assert!(IccProfile::from_bytes(&bad_tags).is_err());

        // Tag pointing out of bounds
        let mut oob_tag = vec![0u8; 144];
        oob_tag[0..4].copy_from_slice(&144u32.to_be_bytes());
        oob_tag[36..40].copy_from_slice(b"acsp");
        oob_tag[128..132].copy_from_slice(&1u32.to_be_bytes());
        oob_tag[132..136].copy_from_slice(b"wtpt");
        oob_tag[136..140].copy_from_slice(&500u32.to_be_bytes());
        oob_tag[140..144].copy_from_slice(&20u32.to_be_bytes());
        assert!(IccProfile::from_bytes(&oob_tag).is_err());
    }

    #[test]
    fn builtin_cmyk_profiles_soft_proof_and_gamut_warning_are_colorimetrically_accurate() {
        // 1. Neutral white/gray stay strictly in CMYK gamut
        assert!(!is_out_of_cmyk_gamut(
            [1.0, 1.0, 1.0],
            BuiltinIccProfile::IsoCoatedV2Cmyk
        ));
        assert!(!is_out_of_cmyk_gamut(
            [0.5, 0.5, 0.5],
            BuiltinIccProfile::IsoCoatedV2Cmyk
        ));
        assert!(!is_out_of_cmyk_gamut(
            [0.55, 0.42, 0.34],
            BuiltinIccProfile::IsoCoatedV2Cmyk
        ));

        // 2. Saturated screen neon green & electric blue are genuinely OUT of CMYK press gamut!
        assert!(
            is_out_of_cmyk_gamut([0.0, 1.0, 0.0], BuiltinIccProfile::IsoCoatedV2Cmyk),
            "Pure sRGB neon green must be out of ISO Coated v2 CMYK gamut"
        );
        assert!(
            is_out_of_cmyk_gamut([0.0, 0.0, 1.0], BuiltinIccProfile::IsoCoatedV2Cmyk),
            "Pure sRGB electric blue must be out of ISO Coated v2 CMYK gamut"
        );
        assert!(
            is_out_of_cmyk_gamut([0.0, 1.0, 0.0], BuiltinIccProfile::UsWebCoatedSwopCmyk),
            "Pure sRGB neon green must be out of US Web Coated SWOP gamut"
        );

        // 3. Soft-proofing neon green noticeably desaturates/darkens into press green
        let proofed_green = soft_proof_srgb_through_cmyk(
            [0.0, 1.0, 0.0],
            BuiltinIccProfile::IsoCoatedV2Cmyk,
            RenderingIntent::RelativeColorimetric,
        );
        assert!(
            proofed_green[1] < 0.85,
            "Soft-proofed neon green should dull significantly: {proofed_green:?}"
        );

        // 4. ISO Coated v2 and US Web Coated SWOP produce distinct press renditions
        let cmyk_iso = convert_rgb_to_cmyk_icc(
            [0.2, 0.5, 0.7],
            BuiltinIccProfile::IsoCoatedV2Cmyk,
            RenderingIntent::RelativeColorimetric,
        );
        let cmyk_swop = convert_rgb_to_cmyk_icc(
            [0.2, 0.5, 0.7],
            BuiltinIccProfile::UsWebCoatedSwopCmyk,
            RenderingIntent::RelativeColorimetric,
        );
        let diff: f32 = (0..4).map(|i| (cmyk_iso[i] - cmyk_swop[i]).abs()).sum();
        assert!(
            diff > 0.02,
            "ISO and SWOP should yield distinct CMYK separations"
        );

        // 5. TAC limit strictly enforced even on pure black
        let black_swop = convert_rgb_to_cmyk_icc(
            [0.0, 0.0, 0.0],
            BuiltinIccProfile::UsWebCoatedSwopCmyk,
            RenderingIntent::RelativeColorimetric,
        );
        let tac: f32 = black_swop.iter().sum();
        assert!(tac <= 3.01, "SWOP TAC must not exceed 300%, got {tac}");

        // 6. In-gamut roundtrip CMYK -> RGB -> CMYK maintains low colorimetric error
        let orig_rgb = [0.62_f32, 0.44, 0.35];
        let proof_rgb = soft_proof_srgb_through_cmyk(
            orig_rgb,
            BuiltinIccProfile::IsoCoatedV2Cmyk,
            RenderingIntent::RelativeColorimetric,
        );
        for i in 0..3 {
            assert!(
                (orig_rgb[i] - proof_rgb[i]).abs() < 0.04,
                "In-gamut soft proof drifted on channel {i}: {} vs {}",
                orig_rgb[i],
                proof_rgb[i]
            );
        }

        // 7. Fast framebuffer pass matches expected behavior & preserves transparency
        let mut rgba = vec![
            0, 255, 0, 255, // neon green (out of gamut)
            128, 128, 128, 255, // mid gray (in gamut)
            255, 0, 0, 0, // fully transparent (untouched)
        ];
        apply_soft_proof_rgba(
            &mut rgba,
            BuiltinIccProfile::IsoCoatedV2Cmyk,
            RenderingIntent::RelativeColorimetric,
            true,
            true,
        );
        assert!(
            rgba[2] > 100,
            "Out-of-gamut neon green should receive cyan-green warning tint"
        );
        assert!(
            (i32::from(rgba[4]) - 128).abs() < 12,
            "In-gamut mid gray stays neutral"
        );
        assert_eq!(
            &rgba[8..12],
            &[255, 0, 0, 0],
            "Transparent pixel stays untouched"
        );
    }

    #[test]
    fn mluc_dos_oversized_and_multi_record_allocation_churn_is_bounded() {
        // Case A: 256 records pointing to an oversized (> 64 KiB) unpaired UTF-16 slice + one small valid fallback record
        let mut mluc = Vec::new();
        mluc.extend_from_slice(b"mluc");
        mluc.extend_from_slice(&0u32.to_be_bytes());
        let record_count = 256u32;
        mluc.extend_from_slice(&record_count.to_be_bytes());
        mluc.extend_from_slice(&12u32.to_be_bytes());

        let header_and_records = 16usize + (record_count as usize) * 12;
        let big_len = 200_000usize; // > MAX_ICC_DESC_BYTES (65_536)
        let valid_utf16: Vec<u8> = "Bounded OK"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        let valid_off = header_and_records + big_len;

        for i in 0..record_count {
            mluc.extend_from_slice(b"enUS");
            if i + 1 < record_count {
                mluc.extend_from_slice(&(big_len as u32).to_be_bytes());
                mluc.extend_from_slice(&(header_and_records as u32).to_be_bytes());
            } else {
                mluc.extend_from_slice(&(valid_utf16.len() as u32).to_be_bytes());
                mluc.extend_from_slice(&(valid_off as u32).to_be_bytes());
            }
        }
        // Invalid lone high surrogates (0xD800) so String::from_utf16 would fail if scanned
        for _ in 0..(big_len / 2) {
            mluc.extend_from_slice(&0xD800u16.to_be_bytes());
        }
        mluc.extend_from_slice(&valid_utf16);

        let bytes = build_synthetic_icc_bytes(4, b"mntr", b"RGB ", b"XYZ ", 0, &[(b"desc", mluc)]);
        let prof = IccProfile::from_bytes(&bytes).expect("oversized mluc records skipped safely");
        assert_eq!(prof.description, "Bounded OK");

        // Case B: 256 records each at MAX_ICC_DESC_BYTES pointing to invalid UTF-16 must trip total scan budget
        let mut mluc_churn = Vec::new();
        mluc_churn.extend_from_slice(b"mluc");
        mluc_churn.extend_from_slice(&0u32.to_be_bytes());
        mluc_churn.extend_from_slice(&256u32.to_be_bytes());
        mluc_churn.extend_from_slice(&12u32.to_be_bytes());
        let payload_off = 16usize + 256 * 12;
        for _ in 0..256 {
            mluc_churn.extend_from_slice(b"enUS");
            mluc_churn.extend_from_slice(&(MAX_ICC_DESC_BYTES as u32).to_be_bytes());
            mluc_churn.extend_from_slice(&(payload_off as u32).to_be_bytes());
        }
        for _ in 0..(MAX_ICC_DESC_BYTES / 2) {
            mluc_churn.extend_from_slice(&0xD800u16.to_be_bytes());
        }
        let churn_bytes =
            build_synthetic_icc_bytes(4, b"mntr", b"RGB ", b"XYZ ", 0, &[(b"desc", mluc_churn)]);
        let prof_churn =
            IccProfile::from_bytes(&churn_bytes).expect("mluc total scan budget stops churn");
        assert!(prof_churn.description.is_empty());

        // Case C: Extreme record_size causing arithmetic overflow breaks safely without panic
        let mut mluc_overflow = Vec::new();
        mluc_overflow.extend_from_slice(b"mluc");
        mluc_overflow.extend_from_slice(&0u32.to_be_bytes());
        mluc_overflow.extend_from_slice(&4u32.to_be_bytes());
        mluc_overflow.extend_from_slice(&u32::MAX.to_be_bytes());
        mluc_overflow.extend_from_slice(&[0u8; 32]);
        let ov_bytes =
            build_synthetic_icc_bytes(4, b"mntr", b"RGB ", b"XYZ ", 0, &[(b"desc", mluc_overflow)]);
        let prof_ov = IccProfile::from_bytes(&ov_bytes).expect("overflowing record_size safe");
        assert!(prof_ov.description.is_empty());
    }

    #[test]
    fn profile_size_strictly_bounds_tag_table_and_tag_payloads() {
        // 1. profile_size == 128 (< 132) inside a 200-byte buffer MUST be rejected
        let mut p128 = vec![0u8; 200];
        p128[0..4].copy_from_slice(&128u32.to_be_bytes());
        p128[36..40].copy_from_slice(b"acsp");
        assert!(
            IccProfile::from_bytes(&p128).is_err(),
            "profile_size 128 (< 132) must be rejected"
        );

        // 2. Tag payload located past profile_size (even though inside outer buffer) MUST be rejected
        let mut past_psize = vec![0u8; 200];
        past_psize[0..4].copy_from_slice(&148u32.to_be_bytes()); // declared profile ends at 148
        past_psize[36..40].copy_from_slice(b"acsp");
        past_psize[128..132].copy_from_slice(&1u32.to_be_bytes()); // 1 tag -> table ends at 144
        past_psize[132..136].copy_from_slice(b"wtpt");
        past_psize[136..140].copy_from_slice(&160u32.to_be_bytes()); // past declared 148!
        past_psize[140..144].copy_from_slice(&20u32.to_be_bytes());
        assert!(
            IccProfile::from_bytes(&past_psize).is_err(),
            "tag referencing bytes past profile_size must be rejected"
        );

        // 3. Tag starting at offset 128 (inside tag_count) MUST be rejected
        let mut overlap_tag = vec![0u8; 200];
        overlap_tag[0..4].copy_from_slice(&200u32.to_be_bytes());
        overlap_tag[36..40].copy_from_slice(b"acsp");
        overlap_tag[128..132].copy_from_slice(&1u32.to_be_bytes());
        overlap_tag[132..136].copy_from_slice(b"wtpt");
        overlap_tag[136..140].copy_from_slice(&128u32.to_be_bytes());
        overlap_tag[140..144].copy_from_slice(&20u32.to_be_bytes());
        assert!(IccProfile::from_bytes(&overlap_tag).is_err());

        // 4. Valid profile with harmless trailing padding bytes beyond profile_size still parses cleanly
        let mut valid_with_trailing = build_synthetic_icc_bytes(
            4,
            b"mntr",
            b"RGB ",
            b"XYZ ",
            0,
            &[(b"desc", make_mluc_tag("Trailing Padding OK"))],
        );
        valid_with_trailing.extend_from_slice(&[0xAAu8; 64]);
        let parsed = IccProfile::from_bytes(&valid_with_trailing)
            .expect("valid profile with trailing padding after profile_size");
        assert_eq!(parsed.description, "Trailing Padding OK");
    }

    #[test]
    fn nan_and_inf_inputs_never_leak_nan_from_profile_conversions() {
        // Matrix-only RGB profile with rTRC/gTRC/bTRC absent
        let bytes = build_synthetic_icc_bytes(
            4,
            b"mntr",
            b"RGB ",
            b"XYZ ",
            1,
            &[
                (
                    b"rXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][0],
                        SRGB_TO_XYZ_D50[1][0],
                        SRGB_TO_XYZ_D50[2][0],
                    ]),
                ),
                (
                    b"gXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][1],
                        SRGB_TO_XYZ_D50[1][1],
                        SRGB_TO_XYZ_D50[2][1],
                    ]),
                ),
                (
                    b"bXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][2],
                        SRGB_TO_XYZ_D50[1][2],
                        SRGB_TO_XYZ_D50[2][2],
                    ]),
                ),
            ],
        );
        let no_trc = IccProfile::from_bytes(&bytes).expect("matrix-only RGB profile");
        assert!(no_trc.r_trc.is_none());

        for bad_rgb in [
            [f64::NAN, 0.5, 0.5],
            [f64::INFINITY, f64::NEG_INFINITY, f64::NAN],
        ] {
            let xyz = no_trc
                .rgb_to_pcs_xyz(bad_rgb)
                .expect("rgb_to_pcs_xyz returns Some");
            assert!(
                xyz.iter().all(|v| v.is_finite()),
                "rgb_to_pcs_xyz leaked non-finite value: {xyz:?}"
            );
        }

        for bad_xyz in [
            [f64::NAN, 0.5, 0.5],
            [f64::INFINITY, f64::NEG_INFINITY, f64::NAN],
        ] {
            let rgb = no_trc
                .pcs_xyz_to_rgb(bad_xyz)
                .expect("pcs_xyz_to_rgb returns Some");
            assert!(
                rgb.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                "pcs_xyz_to_rgb leaked non-finite or out-of-range value: {rgb:?}"
            );

            let cmyk_prof = IccProfile::builtin(BuiltinIccProfile::IsoCoatedV2Cmyk);
            let cmyk = cmyk_prof
                .pcs_xyz_to_cmyk(bad_xyz)
                .expect("pcs_xyz_to_cmyk returns Some");
            assert!(
                cmyk.iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                "pcs_xyz_to_cmyk leaked non-finite or out-of-range value: {cmyk:?}"
            );
        }
    }

    #[test]
    fn parses_icc_v4_mab_mba_profiles_with_matrix_fallback_and_rejects_corrupt_mft() {
        let mut mab = vec![0u8; 64];
        mab[0..4].copy_from_slice(b"mAB ");
        mab[8] = 3;
        mab[9] = 3;

        let mut mba = vec![0u8; 64];
        mba[0..4].copy_from_slice(b"mBA ");
        mba[8] = 3;
        mba[9] = 3;

        // 1. ICC v4 RGB profile carrying both mAB/mBA LUTs and matrix+TRC parses cleanly and falls back to matrix+TRC
        let bytes = build_synthetic_icc_bytes(
            4,
            b"mntr",
            b"RGB ",
            b"XYZ ",
            1,
            &[
                (b"desc", make_mluc_tag("v4 Hybrid mAB+Matrix")),
                (
                    b"rXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][0],
                        SRGB_TO_XYZ_D50[1][0],
                        SRGB_TO_XYZ_D50[2][0],
                    ]),
                ),
                (
                    b"gXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][1],
                        SRGB_TO_XYZ_D50[1][1],
                        SRGB_TO_XYZ_D50[2][1],
                    ]),
                ),
                (
                    b"bXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][2],
                        SRGB_TO_XYZ_D50[1][2],
                        SRGB_TO_XYZ_D50[2][2],
                    ]),
                ),
                (b"rTRC", make_para_srgb_tag()),
                (b"gTRC", make_para_srgb_tag()),
                (b"bTRC", make_para_srgb_tag()),
                (b"A2B0", mab.clone()),
                (b"B2A0", mba.clone()),
            ],
        );
        let prof = IccProfile::from_bytes(&bytes).expect("v4 profile with mAB/mBA parses cleanly");
        assert!(prof.a2b0.is_none());
        assert!(prof.b2a0.is_none());
        let xyz = prof
            .rgb_to_pcs_xyz([0.3, 0.6, 0.8])
            .expect("matrix fallback rgb->xyz");
        let back = prof.pcs_xyz_to_rgb(xyz).expect("matrix fallback xyz->rgb");
        assert!((back[0] - 0.3).abs() < 1e-3);
        assert!((back[1] - 0.6).abs() < 1e-3);
        assert!((back[2] - 0.8).abs() < 1e-3);

        // 2. Named-color ICC v4 profile (like colord Crayons.icc) with mBA B2A0 parses cleanly
        let nmcl_bytes = build_synthetic_icc_bytes(
            4,
            b"nmcl",
            b"Lab ",
            b"Lab ",
            0,
            &[(b"desc", make_mluc_tag("Named Palette v4")), (b"B2A0", mba)],
        );
        let nmcl = IccProfile::from_bytes(&nmcl_bytes).expect("nmcl v4 profile with mBA");
        assert_eq!(nmcl.header.profile_class, IccProfileClass::NamedColor);
        assert!(nmcl.b2a0.is_none());

        // 3. Truncated mAB (< 32 bytes), truncated mft1/mft2, and unknown LUT types are STILL rejected!
        let mut trunc_mab = vec![0u8; 20];
        trunc_mab[0..4].copy_from_slice(b"mAB ");
        let bad_mab =
            build_synthetic_icc_bytes(4, b"mntr", b"RGB ", b"XYZ ", 0, &[(b"A2B0", trunc_mab)]);
        assert!(IccProfile::from_bytes(&bad_mab).is_err());

        let mut trunc_mft1 = vec![0u8; 48];
        trunc_mft1[0..4].copy_from_slice(b"mft1");
        trunc_mft1[8] = 3;
        trunc_mft1[9] = 3;
        trunc_mft1[10] = 2;
        let bad_mft1 =
            build_synthetic_icc_bytes(2, b"prtr", b"CMYK", b"Lab ", 0, &[(b"A2B0", trunc_mft1)]);
        assert!(IccProfile::from_bytes(&bad_mft1).is_err());

        let mut trunc_mft2 = vec![0u8; 52];
        trunc_mft2[0..4].copy_from_slice(b"mft2");
        trunc_mft2[8] = 4;
        trunc_mft2[9] = 3;
        trunc_mft2[10] = 2;
        trunc_mft2[48..50].copy_from_slice(&2u16.to_be_bytes());
        trunc_mft2[50..52].copy_from_slice(&2u16.to_be_bytes());
        let bad_mft2 =
            build_synthetic_icc_bytes(2, b"prtr", b"CMYK", b"Lab ", 0, &[(b"A2B0", trunc_mft2)]);
        assert!(IccProfile::from_bytes(&bad_mft2).is_err());

        let mut bad_sig = vec![0u8; 64];
        bad_sig[0..4].copy_from_slice(b"xxxx");
        let bad_unknown =
            build_synthetic_icc_bytes(4, b"mntr", b"RGB ", b"XYZ ", 0, &[(b"A2B0", bad_sig)]);
        assert!(IccProfile::from_bytes(&bad_unknown).is_err());
    }

    #[test]
    fn generated_minimal_icc_profiles_parse_and_soft_proof_cleanly() {
        let srgb_bytes = generate_minimal_srgb_icc_v2("Test sRGB");
        let srgb = IccProfile::from_bytes(&srgb_bytes).expect("minimal sRGB ICC v2 parses");
        assert_eq!(srgb.header.color_space, IccColorSpace::Rgb);

        let cmyk_bytes =
            generate_minimal_cmyk_icc_v2("Test ISO CMYK", BuiltinIccProfile::IsoCoatedV2Cmyk);
        let cmyk = IccProfile::from_bytes(&cmyk_bytes).expect("minimal CMYK ICC v2 parses");
        assert_eq!(cmyk.header.color_space, IccColorSpace::Cmyk);
        assert!(cmyk.a2b0.is_some());
        assert!(cmyk.b2a0.is_some());

        assert!(
            SoftProofLut3D::build_from_profile(&cmyk, RenderingIntent::RelativeColorimetric)
                .is_some()
        );
        let mut px = vec![0u8, 174u8, 239u8, 255u8];
        apply_soft_proof_rgba_with_profile(
            &mut px,
            Some(&cmyk),
            BuiltinIccProfile::IsoCoatedV2Cmyk,
            RenderingIntent::RelativeColorimetric,
            true,
            false,
        );
        assert_eq!(px[3], 255);
    }

    #[test]
    fn parses_real_v4_mab_mba_clut_and_honors_rendering_intents() {
        // Build a synthetic ICC v4 A2B0 (`mAB `) with a 2x2x2 CLUT + curve sequence (no matrix)
        let mut mab = vec![0u8; 32];
        mab[0..4].copy_from_slice(b"mAB ");
        mab[8] = 3;
        mab[9] = 3;
        // b_curves at 32 (3 identity `curv` tags, each 12 bytes -> 36 bytes)
        mab[12..16].copy_from_slice(&32u32.to_be_bytes());
        for _ in 0..3 {
            mab.extend_from_slice(b"curv\0\0\0\0\0\0\0\0");
        }
        let clut_off = mab.len() as u32;
        mab[24..28].copy_from_slice(&clut_off.to_be_bytes());
        let mut hdr = [0u8; 20];
        hdr[0] = 2;
        hdr[1] = 2;
        hdr[2] = 2;
        hdr[16] = 1; // 8-bit CLUT
        mab.extend_from_slice(&hdr);
        // 8 grid corners * 3 output bytes = 24 bytes (identity XYZ-ish cube scaled to [0..255])
        for ix in 0..=1u8 {
            for iy in 0..=1u8 {
                for iz in 0..=1u8 {
                    mab.extend_from_slice(&[ix * 200, iy * 200, iz * 200]);
                }
            }
        }

        let bytes = build_synthetic_icc_bytes(
            4,
            b"mntr",
            b"RGB ",
            b"Lab ",
            0,
            &[
                (b"desc", make_mluc_tag("v4 Full mAB CLUT")),
                (b"wtpt", make_xyz_tag([0.90, 0.95, 0.80])),
                (b"A2B0", mab),
            ],
        );
        let prof = IccProfile::from_bytes(&bytes).expect("real v4 mAB CLUT parses cleanly");
        assert!(prof.v4_a2b[0].is_some());
        let xyz_rel = prof
            .rgb_to_pcs_xyz_with_intent([0.5, 0.5, 0.5], RenderingIntent::Perceptual)
            .expect("v4 mAB evaluation");
        let xyz_abs = prof
            .rgb_to_pcs_xyz_with_intent([0.5, 0.5, 0.5], RenderingIntent::AbsoluteColorimetric)
            .expect("v4 absolute colorimetric evaluation");
        assert!(xyz_rel.iter().all(|v| v.is_finite()));
        assert!(
            (xyz_abs[2] - xyz_rel[2]).abs() > 1e-4,
            "AbsoluteColorimetric should apply media white-point scaling"
        );
    }

    #[test]
    fn parses_chad_and_bkpt_tags_and_applies_bradford_and_bpc() {
        let mut chad = Vec::new();
        chad.extend_from_slice(b"sf32");
        chad.extend_from_slice(&0u32.to_be_bytes());
        let bmat = bradford_adaptation_matrix(D65_WHITE, D50_WHITE);
        for row in &bmat {
            for &val in row {
                let fixed = (val * 65_536.0).round() as i32;
                chad.extend_from_slice(&fixed.to_be_bytes());
            }
        }
        let bkpt = make_xyz_tag([0.02, 0.02, 0.015]);
        let bytes = build_synthetic_icc_bytes(
            4,
            b"mntr",
            b"RGB ",
            b"XYZ ",
            1,
            &[
                (b"desc", make_mluc_tag("Bradford Test")),
                (b"wtpt", make_xyz_tag(D65_WHITE)),
                (b"bkpt", bkpt),
                (b"chad", chad),
                (
                    b"rXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][0],
                        SRGB_TO_XYZ_D50[1][0],
                        SRGB_TO_XYZ_D50[2][0],
                    ]),
                ),
                (
                    b"gXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][1],
                        SRGB_TO_XYZ_D50[1][1],
                        SRGB_TO_XYZ_D50[2][1],
                    ]),
                ),
                (
                    b"bXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][2],
                        SRGB_TO_XYZ_D50[1][2],
                        SRGB_TO_XYZ_D50[2][2],
                    ]),
                ),
                (b"rTRC", make_para_srgb_tag()),
                (b"gTRC", make_para_srgb_tag()),
                (b"bTRC", make_para_srgb_tag()),
            ],
        );
        let prof = IccProfile::from_bytes(&bytes).expect("profile with chad + bkpt parses");
        assert!(prof.chad.is_some());
        assert!(prof.black_point.is_some());

        let adapted = adapt_xyz_bradford(D65_WHITE, D65_WHITE, D50_WHITE);
        for i in 0..3 {
            assert!((adapted[i] - D50_WHITE[i]).abs() < 1e-3);
        }

        // ISO 18619 BPC maps source black point Y -> 0.0 while keeping D50 white fixed
        let xyz_no_bpc = prof
            .rgb_to_pcs_xyz_with_intent_bpc(
                [0.0, 0.0, 0.0],
                RenderingIntent::RelativeColorimetric,
                false,
            )
            .unwrap();
        let xyz_bpc = prof
            .rgb_to_pcs_xyz_with_intent_bpc(
                [0.0, 0.0, 0.0],
                RenderingIntent::RelativeColorimetric,
                true,
            )
            .unwrap();
        assert!((xyz_no_bpc[1] - 0.0).abs() < 1e-4);
        assert!(xyz_bpc[1] <= xyz_no_bpc[1] + 1e-6);

        let scaled = IccProfile::apply_bpc(
            [0.02, 0.02, 0.015],
            [0.02, 0.02, 0.015],
            0.0,
            RenderingIntent::RelativeColorimetric,
            true,
        );
        assert!(scaled[1].abs() < 1e-6);
        let white_fixed = IccProfile::apply_bpc(
            D50_WHITE,
            [0.02, 0.02, 0.015],
            0.0,
            RenderingIntent::RelativeColorimetric,
            true,
        );
        for i in 0..3 {
            assert!((white_fixed[i] - D50_WHITE[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn gray_pcs_roundtrip_and_cmyk_bpc_pipeline() {
        let mut ktrc = Vec::new();
        ktrc.extend_from_slice(b"curv\0\0\0\0");
        ktrc.extend_from_slice(&1u32.to_be_bytes());
        ktrc.extend_from_slice(&((2.2 * 256.0) as u16).to_be_bytes());
        let gray_bytes = build_synthetic_icc_bytes(
            2,
            b"mntr",
            b"GRAY",
            b"XYZ ",
            1,
            &[(b"desc", make_mluc_tag("Gray Gamma 2.2")), (b"kTRC", ktrc)],
        );
        let gray_prof = IccProfile::from_bytes(&gray_bytes).expect("gray ICC profile parses");
        assert_eq!(gray_prof.header.color_space, IccColorSpace::Gray);
        let xyz = gray_prof
            .gray_to_pcs_xyz_with_intent_bpc(0.5, RenderingIntent::RelativeColorimetric, false)
            .expect("gray->xyz");
        let back = gray_prof
            .pcs_xyz_to_gray_with_intent(xyz, RenderingIntent::RelativeColorimetric)
            .expect("xyz->gray");
        assert!((back - 0.5).abs() < 1e-3, "gray roundtrip back={back}");

        let cmyk_prof = IccProfile::builtin(BuiltinIccProfile::IsoCoatedV2Cmyk);
        let no_bpc = cmyk_prof
            .cmyk_to_srgb_with_intent_bpc(
                [1.0, 1.0, 1.0, 1.0],
                RenderingIntent::RelativeColorimetric,
                false,
            )
            .unwrap();
        let with_bpc = cmyk_prof
            .cmyk_to_srgb_with_intent_bpc(
                [1.0, 1.0, 1.0, 1.0],
                RenderingIntent::RelativeColorimetric,
                true,
            )
            .unwrap();
        assert!(with_bpc[1] < no_bpc[1], "BPC must deepen CMYK black point");

        let pipeline = IccColorPipeline::from_document_profiles(
            None,
            None,
            Some(&gray_bytes),
            None,
            RenderingIntent::RelativeColorimetric,
            true,
        );
        let gray_srgb = pipeline.color_to_rgba_f32(&crate::color::Color::gray(0.5));
        assert!(
            (gray_srgb[0] - 0.503).abs() < 0.02,
            "gamma 2.2 gray 0.5 -> sRGB ~0.503, got {:?}",
            gray_srgb
        );
    }

    #[test]
    fn absolute_colorimetric_applies_chad_matrix_when_present() {
        let mut prof = IccProfile::builtin(BuiltinIccProfile::Srgb);
        // Set white_point to D50 so any white-point difference in AbsoluteColorimetric
        // MUST come from `self.chad` (Bradford D65 -> D50), not diagonal white_point scaling.
        prof.white_point = Some(D50_WHITE);
        let bradford_d65_to_d50 = bradford_adaptation_matrix(D65_WHITE, D50_WHITE);
        prof.chad = Some(bradford_d65_to_d50);

        let abs_white = prof
            .rgb_to_pcs_xyz_with_intent_bpc(
                [1.0, 1.0, 1.0],
                RenderingIntent::AbsoluteColorimetric,
                false,
            )
            .expect("rgb->pcs_xyz");
        for i in 0..3 {
            assert!(
                (abs_white[i] - D65_WHITE[i]).abs() < 1e-3,
                "chad^-1 must recover D65 white at channel {i}: got {}, expected {}",
                abs_white[i],
                D65_WHITE[i]
            );
        }

        let roundtrip_rgb = prof
            .pcs_xyz_to_rgb_with_intent(abs_white, RenderingIntent::AbsoluteColorimetric)
            .expect("pcs_xyz->rgb");
        for (i, &val) in roundtrip_rgb.iter().enumerate() {
            assert!(
                (val - 1.0).abs() < 1e-3,
                "chad roundtrip must recover white RGB=1.0 at channel {i}: got {val}",
            );
        }
    }

    fn build_synthetic_srgb_v4_with_chad_bytes() -> Vec<u8> {
        let mut chad = Vec::new();
        chad.extend_from_slice(b"sf32");
        chad.extend_from_slice(&0u32.to_be_bytes());
        let bmat = bradford_adaptation_matrix(D65_WHITE, D50_WHITE);
        for row in &bmat {
            for &val in row {
                let fixed = (val * 65_536.0).round() as i32;
                chad.extend_from_slice(&fixed.to_be_bytes());
            }
        }
        build_synthetic_icc_bytes(
            4,
            b"mntr",
            b"RGB ",
            b"XYZ ",
            1,
            &[
                (b"desc", make_mluc_tag("Synthetic sRGB v4 with chad")),
                (b"wtpt", make_xyz_tag(D50_WHITE)),
                (b"chad", chad),
                (
                    b"rXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][0],
                        SRGB_TO_XYZ_D50[1][0],
                        SRGB_TO_XYZ_D50[2][0],
                    ]),
                ),
                (
                    b"gXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][1],
                        SRGB_TO_XYZ_D50[1][1],
                        SRGB_TO_XYZ_D50[2][1],
                    ]),
                ),
                (
                    b"bXYZ",
                    make_xyz_tag([
                        SRGB_TO_XYZ_D50[0][2],
                        SRGB_TO_XYZ_D50[1][2],
                        SRGB_TO_XYZ_D50[2][2],
                    ]),
                ),
                (b"rTRC", make_para_srgb_tag()),
                (b"gTRC", make_para_srgb_tag()),
                (b"bTRC", make_para_srgb_tag()),
            ],
        )
    }

    #[test]
    fn absolute_colorimetric_pipeline_chad_rgb_profiles_preserve_white_without_cyan_shift() {
        use crate::color::Color;

        // 1. Synthetic ICC v4 D65 profile carrying a `chad` matrix and D50-adapted `wtpt`.
        let srgb_v4_bytes = build_synthetic_srgb_v4_with_chad_bytes();
        let srgb_v4 = IccProfile::from_bytes(&srgb_v4_bytes).expect("synthetic sRGB v4 parses");
        assert!(srgb_v4.chad.is_some());

        let pipeline = IccColorPipeline::from_document_profiles(
            Some(&srgb_v4_bytes),
            None,
            None,
            None,
            RenderingIntent::AbsoluteColorimetric,
            false,
        );
        let white_f32 = pipeline.color_to_rgba_f32(&Color::rgba(1.0, 1.0, 1.0, 1.0));
        for (ch, &val) in white_f32.iter().enumerate() {
            assert!(
                (val - 1.0).abs() < 1e-3,
                "synthetic chad sRGB v4 AbsoluteColorimetric white channel {ch} drifted: {white_f32:?}"
            );
        }
        assert_eq!(
            pipeline.color_to_rgba8(&Color::rgba(1.0, 1.0, 1.0, 1.0)),
            [255, 255, 255, 255]
        );

        let mid_f32 = pipeline.color_to_rgba_f32(&Color::rgba(0.5, 0.5, 0.5, 1.0));
        for (ch, &val) in mid_f32[..3].iter().enumerate() {
            assert!(
                (val - 0.5).abs() < 5e-3,
                "synthetic chad sRGB v4 neutral gray channel {ch} drifted: {mid_f32:?}"
            );
        }

        // SoftProofLut3D::build_from_profile must also keep white neutral under AbsoluteColorimetric.
        let mut proof_px = [255u8, 255, 255, 255];
        apply_soft_proof_rgba_with_profile(
            &mut proof_px,
            Some(&srgb_v4),
            BuiltinIccProfile::IsoCoatedV2Cmyk,
            RenderingIntent::AbsoluteColorimetric,
            true,
            false,
        );
        assert_eq!(
            proof_px,
            [255, 255, 255, 255],
            "SoftProofLut3D turned white to non-white under AbsoluteColorimetric: {proof_px:?}"
        );

        // 2. Real colord v4 profiles (`sRGB.icc`, `AdobeRGB1998.icc`, `Bluish.icc`) when installed.
        for name in ["sRGB.icc", "AdobeRGB1998.icc", "Bluish.icc"] {
            let path = std::path::Path::new("/usr/share/color/icc/colord").join(name);
            if !path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read colord profile");
            let parsed = IccProfile::from_bytes(&bytes).expect("parse colord profile");
            assert!(
                parsed.chad.is_some(),
                "{name} should carry a chad tag for regression coverage"
            );
            let pipe = IccColorPipeline::from_document_profiles(
                Some(&bytes),
                None,
                None,
                None,
                RenderingIntent::AbsoluteColorimetric,
                false,
            );
            let out = pipe.color_to_rgba_f32(&Color::rgba(1.0, 1.0, 1.0, 1.0));
            for (ch, &val) in out.iter().enumerate() {
                assert!(
                    (val - 1.0).abs() < 1e-3,
                    "{name} AbsoluteColorimetric white channel {ch} shifted (cyan bug?): {out:?}"
                );
            }
            assert_eq!(
                pipe.color_to_rgba8(&Color::rgba(1.0, 1.0, 1.0, 1.0)),
                [255, 255, 255, 255],
                "{name} AbsoluteColorimetric white must render (255,255,255)"
            );

            let mut px = [255u8, 255, 255, 255];
            apply_soft_proof_rgba_with_profile(
                &mut px,
                Some(&parsed),
                BuiltinIccProfile::IsoCoatedV2Cmyk,
                RenderingIntent::AbsoluteColorimetric,
                true,
                false,
            );
            assert_eq!(
                px,
                [255, 255, 255, 255],
                "{name} SoftProofLut3D white drifted under AbsoluteColorimetric: {px:?}"
            );
        }
    }

    #[test]
    fn absolute_colorimetric_pipeline_cmyk_agrees_between_default_and_explicit_srgb_profile() {
        use crate::color::Color;

        let srgb_v4_bytes = build_synthetic_srgb_v4_with_chad_bytes();
        let cmyk_v2_bytes =
            generate_minimal_cmyk_icc_v2("Synth ISO Coated v2", BuiltinIccProfile::IsoCoatedV2Cmyk);

        let pipe_default = IccColorPipeline::from_document_profiles(
            None,
            Some(&cmyk_v2_bytes),
            None,
            None,
            RenderingIntent::AbsoluteColorimetric,
            false,
        );
        let pipe_explicit_srgb = IccColorPipeline::from_document_profiles(
            Some(&srgb_v4_bytes),
            Some(&cmyk_v2_bytes),
            None,
            None,
            RenderingIntent::AbsoluteColorimetric,
            false,
        );
        for cmyk in [
            [0.0, 0.0, 0.0, 0.0],
            [0.65, 0.15, 0.0, 0.0],
            [0.0, 0.85, 0.05, 0.0],
            [0.4, 0.7, 0.2, 0.1],
        ] {
            let col = Color::cmyk(cmyk[0], cmyk[1], cmyk[2], cmyk[3]);
            let a = pipe_default.color_to_rgba_f32(&col);
            let b = pipe_explicit_srgb.color_to_rgba_f32(&col);
            for ch in 0..4 {
                assert!(
                    (a[ch] - b[ch]).abs() < 1e-3,
                    "CMYK {cmyk:?} default vs explicit sRGB diverged on ch {ch}: {a:?} vs {b:?}"
                );
            }
        }

        // Real FOGRA39L_coated.icc + sRGB.icc when installed on system.
        let fogra_path = std::path::Path::new("/usr/share/color/icc/colord/FOGRA39L_coated.icc");
        let srgb_path = std::path::Path::new("/usr/share/color/icc/colord/sRGB.icc");
        if fogra_path.is_file() && srgb_path.is_file() {
            let fogra_bytes = std::fs::read(fogra_path).expect("read FOGRA39");
            let srgb_bytes = std::fs::read(srgb_path).expect("read sRGB.icc");
            let p_none = IccColorPipeline::from_document_profiles(
                None,
                Some(&fogra_bytes),
                None,
                None,
                RenderingIntent::AbsoluteColorimetric,
                false,
            );
            let p_srgb = IccColorPipeline::from_document_profiles(
                Some(&srgb_bytes),
                Some(&fogra_bytes),
                None,
                None,
                RenderingIntent::AbsoluteColorimetric,
                false,
            );
            for cmyk in [
                [0.0, 0.0, 0.0, 0.0],
                [0.65, 0.15, 0.0, 0.0],
                [0.0, 0.85, 0.05, 0.0],
                [0.4, 0.7, 0.2, 0.1],
            ] {
                let col = Color::cmyk(cmyk[0], cmyk[1], cmyk[2], cmyk[3]);
                let a = p_none.color_to_rgba_f32(&col);
                let b = p_srgb.color_to_rgba_f32(&col);
                for ch in 0..4 {
                    assert!(
                        (a[ch] - b[ch]).abs() < 1e-3,
                        "FOGRA39 {cmyk:?} rgb=None vs rgb=sRGB.icc diverged on ch {ch}: {a:?} vs {b:?}"
                    );
                }
            }
            // Paper white (0,0,0,0) under FOGRA39 AbsoluteColorimetric matches transicc (~239, 241, 244).
            let paper = p_srgb.color_to_rgba8(&Color::cmyk(0.0, 0.0, 0.0, 0.0));
            assert!(
                (i32::from(paper[0]) - 239).abs() <= 2
                    && (i32::from(paper[1]) - 241).abs() <= 2
                    && (i32::from(paper[2]) - 245).abs() <= 2,
                "FOGRA39 paper white unexpected: {paper:?}"
            );
        }
    }

    #[test]
    fn absolute_colorimetric_display_gray_v2_without_chad_preserves_white_without_cyan_shift() {
        use crate::color::Color;

        let mut ktrc = Vec::new();
        ktrc.extend_from_slice(b"curv\0\0\0\0");
        ktrc.extend_from_slice(&1u32.to_be_bytes());
        ktrc.extend_from_slice(&((2.2 * 256.0) as u16).to_be_bytes());
        let gray_v2_bytes = build_synthetic_icc_bytes(
            2,
            b"mntr",
            b"GRAY",
            b"XYZ ",
            1,
            &[
                (b"desc", make_mluc_tag("Display Gray v2 D65")),
                (b"wtpt", make_xyz_tag(D65_WHITE)),
                (b"kTRC", ktrc),
            ],
        );
        let parsed = IccProfile::from_bytes(&gray_v2_bytes).expect("synthetic display gray parses");
        assert!(parsed.chad.is_none());
        assert_eq!(parsed.header.profile_class, IccProfileClass::Display);
        assert_eq!(parsed.header.color_space, IccColorSpace::Gray);

        let pipe = IccColorPipeline::from_document_profiles(
            None,
            None,
            Some(&gray_v2_bytes),
            None,
            RenderingIntent::AbsoluteColorimetric,
            false,
        );
        assert_eq!(
            pipe.color_to_rgba8(&Color::gray(1.0)),
            [255, 255, 255, 255],
            "Display Gray ICC v2 without chad must map white Gray(1.0) to (255,255,255) under AbsCol"
        );
        let mid = pipe.color_to_rgba8(&Color::gray(0.5));
        assert!(
            mid[0].abs_diff(mid[1]) <= 1 && mid[1].abs_diff(mid[2]) <= 1,
            "Display Gray ICC v2 without chad must keep Gray(0.5) neutral without cyan shift: {mid:?}"
        );
        assert_eq!(pipe.color_to_rgba8(&Color::gray(0.0)), [0, 0, 0, 255]);

        for name in ["sgray.icc", "default_gray.icc"] {
            let path = std::path::Path::new("/usr/share/color/icc/ghostscript").join(name);
            if !path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read ghostscript gray icc");
            let p = IccColorPipeline::from_document_profiles(
                None,
                None,
                Some(&bytes),
                None,
                RenderingIntent::AbsoluteColorimetric,
                false,
            );
            assert_eq!(
                p.color_to_rgba8(&Color::gray(1.0)),
                [255, 255, 255, 255],
                "{name} AbsoluteColorimetric Gray(1.0) must render (255,255,255)"
            );
            let m = p.color_to_rgba8(&Color::gray(0.5));
            assert!(
                m[0].abs_diff(m[1]) <= 1 && m[1].abs_diff(m[2]) <= 1,
                "{name} AbsoluteColorimetric Gray(0.5) must remain neutral: {m:?}"
            );
            assert_eq!(p.color_to_rgba8(&Color::gray(0.0)), [0, 0, 0, 255]);
        }
    }
}
