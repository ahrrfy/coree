// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust vector Barcode and QR Code generator.
//!
//! Outputs crisp vector [`BezierPath`] geometry (never blurry bitmaps) at exact physical
//! millimeter dimensions:
//! - **QR Code** (backed by `qrcode`, with error-correction levels `L`/`M`/`Q`/`H`, quiet zone,
//!   and polygon-welded vector contours)
//! - **1D Barcodes** (pure-Rust ISO/GS1 encoders): `EAN-13` (with automatic checksum calculation
//!   & validation), `EAN-8`, `UPC-A`, `Code128` (Subset B), and `Code39`.

use crate::error::{ProdError, ProdResult};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, Color, Document, Fill, LayerId, NodePayload, ObjectId,
    OutlineStyle, Point2D, Rect2D, SceneObject, Subpath, TextAlign, TextStyle,
};
use qrcode::{Color as QrModuleColor, EcLevel, QrCode};
use serde::{Deserialize, Serialize};

// =============================================================================
// QR Code Vector Generator
// =============================================================================

/// QR code Reed-Solomon error correction capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum QrErrorCorrection {
    /// ~7% codeword recovery.
    L,
    /// ~15% codeword recovery (standard default).
    #[default]
    M,
    /// ~25% codeword recovery.
    Q,
    /// ~30% codeword recovery (recommended when placing a center logo).
    H,
}

impl QrErrorCorrection {
    fn to_qrcode_ec(self) -> EcLevel {
        match self {
            Self::L => EcLevel::L,
            Self::M => EcLevel::M,
            Self::Q => EcLevel::Q,
            Self::H => EcLevel::H,
        }
    }
}

/// Configuration for vector QR code generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QrCodeOptions {
    /// Total physical width & height in millimeters (including quiet zone).
    pub size_mm: f64,
    /// Quiet-zone border width in module units (ISO standard default: `4`).
    pub quiet_zone_modules: u32,
    /// Reed-Solomon error correction level.
    pub ec_level: QrErrorCorrection,
    /// Foreground (dark module) color.
    pub foreground: Color,
    /// Optional opaque background plate color (recommended white for print contrast).
    pub background: Option<Color>,
    /// When `true`, welds adjacent dark modules into clean merged vector polygons via `i_overlay`.
    pub weld_modules: bool,
}

impl Default for QrCodeOptions {
    fn default() -> Self {
        Self {
            size_mm: 25.0,
            quiet_zone_modules: 4,
            ec_level: QrErrorCorrection::M,
            foreground: Color::KEY_BLACK,
            background: Some(Color::cmyk(0.0, 0.0, 0.0, 0.0)),
            weld_modules: true,
        }
    }
}

/// Generated crisp vector QR code geometry and matrix metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QrCodeOutput {
    pub data: String,
    pub module_count: usize,
    pub total_modules_with_quiet_zone: usize,
    pub module_size_mm: f64,
    pub size_mm: f64,
    pub ec_level: QrErrorCorrection,
    pub dark_modules_path: BezierPath,
    pub bounds: Rect2D,
}

/// Maximum allowed quiet-zone width in modules (guards against degenerate/OOM layouts).
pub const MAX_QUIET_ZONE_MODULES: u32 = 64;

/// Maximum physical QR code dimension in millimeters (100 meters).
pub const MAX_QR_SIZE_MM: f64 = 100_000.0;

/// Encodes `data` into a crisp vector QR code [`BezierPath`] at exact physical `options.size_mm`.
pub fn generate_qr_code(data: &str, options: &QrCodeOptions) -> ProdResult<QrCodeOutput> {
    if !options.size_mm.is_finite() || options.size_mm <= 0.0 || options.size_mm > MAX_QR_SIZE_MM {
        return Err(ProdError::BarcodeError(format!(
            "QR code size_mm must be in (0, {MAX_QR_SIZE_MM}] (got {})",
            options.size_mm
        )));
    }
    if options.quiet_zone_modules > MAX_QUIET_ZONE_MODULES {
        return Err(ProdError::BarcodeError(format!(
            "QR quiet_zone_modules {} exceeds maximum {MAX_QUIET_ZONE_MODULES}",
            options.quiet_zone_modules
        )));
    }
    let qr = QrCode::with_error_correction_level(data.as_bytes(), options.ec_level.to_qrcode_ec())
        .map_err(|e| ProdError::BarcodeError(format!("QR encoding failed: {e}")))?;

    let grid_n = qr.width();
    let qz = options.quiet_zone_modules as usize;
    let total_modules = grid_n + 2 * qz;
    let module_mm = options.size_mm / (total_modules as f64);
    let colors = qr.to_colors();

    let dark_modules_path = if options.weld_modules {
        weld_qr_grid_contours(&colors, grid_n, qz, module_mm)
    } else {
        let mut rects: Vec<Rect2D> = Vec::new();
        for y in 0..grid_n {
            let mut x = 0usize;
            while x < grid_n {
                if colors[y * grid_n + x] == QrModuleColor::Dark {
                    let start_x = x;
                    while x < grid_n && colors[y * grid_n + x] == QrModuleColor::Dark {
                        x += 1;
                    }
                    let rx = ((qz + start_x) as f64) * module_mm;
                    let ry = ((qz + y) as f64) * module_mm;
                    let rw = ((x - start_x) as f64) * module_mm;
                    rects.push(Rect2D::from_xywh(rx, ry, rw, module_mm));
                } else {
                    x += 1;
                }
            }
        }
        let subpaths = rects
            .iter()
            .map(|r| Subpath::from_points(r.corners(), true))
            .collect();
        BezierPath { subpaths }
    };

    Ok(QrCodeOutput {
        data: data.to_owned(),
        module_count: grid_n,
        total_modules_with_quiet_zone: total_modules,
        module_size_mm: module_mm,
        size_mm: options.size_mm,
        ec_level: options.ec_level,
        dark_modules_path,
        bounds: Rect2D::from_xywh(0.0, 0.0, options.size_mm, options.size_mm),
    })
}

/// Exact `O(N^2)` integer-lattice boundary contour welder for QR dark modules.
/// Traces closed orthogonal loops (exterior + holes) with right-turn saddle disambiguation
/// and collinear vertex pruning, avoiding general polygon clipping overhead.
fn weld_qr_grid_contours(
    colors: &[QrModuleColor],
    grid_n: usize,
    qz: usize,
    module_mm: f64,
) -> BezierPath {
    const DIR_E: u8 = 1; // (+1,  0)
    const DIR_S: u8 = 2; // ( 0, +1)
    const DIR_W: u8 = 4; // (-1,  0)
    const DIR_N: u8 = 8; // ( 0, -1)

    if grid_n == 0 {
        return BezierPath::new();
    }
    let stride = grid_n + 1;
    let mut out_edges = vec![0u8; stride * stride];
    let is_dark = |x: isize, y: isize| -> bool {
        if x < 0 || y < 0 || (x as usize) >= grid_n || (y as usize) >= grid_n {
            false
        } else {
            colors[(y as usize) * grid_n + (x as usize)] == QrModuleColor::Dark
        }
    };

    for y in 0..grid_n {
        for x in 0..grid_n {
            if !is_dark(x as isize, y as isize) {
                continue;
            }
            if !is_dark(x as isize, y as isize - 1) {
                out_edges[y * stride + x] |= DIR_E;
            }
            if !is_dark(x as isize + 1, y as isize) {
                out_edges[y * stride + (x + 1)] |= DIR_S;
            }
            if !is_dark(x as isize, y as isize + 1) {
                out_edges[(y + 1) * stride + (x + 1)] |= DIR_W;
            }
            if !is_dark(x as isize - 1, y as isize) {
                out_edges[(y + 1) * stride + x] |= DIR_N;
            }
        }
    }

    let step = |d: u8| -> (isize, isize) {
        match d {
            DIR_E => (1, 0),
            DIR_S => (0, 1),
            DIR_W => (-1, 0),
            _ => (0, -1),
        }
    };
    let right_turn = |d: u8| -> u8 {
        match d {
            DIR_E => DIR_S,
            DIR_S => DIR_W,
            DIR_W => DIR_N,
            _ => DIR_E,
        }
    };

    let mut subpaths = Vec::new();
    for start_y in 0..stride {
        for start_x in 0..stride {
            loop {
                let start_mask = out_edges[start_y * stride + start_x];
                if start_mask == 0 {
                    break;
                }
                let first_dir = 1u8 << start_mask.trailing_zeros();
                let mut vx = start_x as isize;
                let mut vy = start_y as isize;
                let mut dir = first_dir;
                let mut pts: Vec<Point2D> = Vec::with_capacity(16);
                let mut prev_dir: u8 = 0;

                loop {
                    let idx = (vy as usize) * stride + (vx as usize);
                    if (out_edges[idx] & dir) == 0 {
                        break;
                    }
                    out_edges[idx] &= !dir;
                    if dir != prev_dir {
                        pts.push(Point2D::new(
                            ((qz as isize + vx) as f64) * module_mm,
                            ((qz as isize + vy) as f64) * module_mm,
                        ));
                    }
                    let (dx, dy) = step(dir);
                    vx += dx;
                    vy += dy;
                    prev_dir = dir;
                    if vx == start_x as isize && vy == start_y as isize {
                        break;
                    }
                    let next_idx = (vy as usize) * stride + (vx as usize);
                    let avail = out_edges[next_idx];
                    if avail == 0 {
                        break;
                    }
                    let pref = right_turn(dir);
                    dir = if (avail & pref) != 0 {
                        pref
                    } else if (avail & dir) != 0 {
                        dir
                    } else {
                        1u8 << avail.trailing_zeros()
                    };
                }

                if pts.len() >= 3 {
                    subpaths.push(Subpath::from_points(pts, true));
                }
            }
        }
    }
    BezierPath { subpaths }
}

/// Adds a grouped vector QR code to `doc` at `origin` and returns its top-level [`ObjectId`].
pub fn add_qr_code_to_document(
    doc: &mut Document,
    data: &str,
    origin: Point2D,
    options: &QrCodeOptions,
    layer_id: Option<LayerId>,
) -> ProdResult<ObjectId> {
    if !origin.is_finite() {
        return Err(ProdError::BarcodeError(
            "QR code origin must be finite".into(),
        ));
    }
    let qr = generate_qr_code(data, options)?;
    let mut children = Vec::new();

    if let Some(bg) = &options.background {
        let bg_obj = SceneObject::new(
            ObjectId(0),
            "QR Quiet Zone",
            NodePayload::Path(BezierPath::from_rect(qr.bounds)),
        )
        .with_fill(Fill::Solid(bg.clone()))
        .with_outline(OutlineStyle::none());
        children.push(doc.add_object(bg_obj, layer_id)?);
    }

    let modules_obj = SceneObject::new(
        ObjectId(0),
        "QR Modules",
        NodePayload::Path(qr.dark_modules_path),
    )
    .with_fill(Fill::Solid(options.foreground.clone()))
    .with_outline(OutlineStyle::none());
    children.push(doc.add_object(modules_obj, layer_id)?);

    let group_id = doc.group_objects(&children, format!("Barcode:QR:{data}"))?;
    let _ = doc.set_object_transform(group_id, Affine2D::translate(origin.x, origin.y));
    Ok(group_id)
}

// =============================================================================
// 1D Vector Barcode Encoders (EAN-13, EAN-8, UPC-A, Code128-B, Code39)
// =============================================================================

/// Code128 character subset selection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Code128Mode {
    /// Automatic optimal switching between Subsets A, B, and C.
    Auto,
    /// Subset A (uppercase ASCII + control codes 0..=31).
    SubsetA,
    /// Subset B (printable ASCII 32..=126; default for legacy backward compatibility).
    #[default]
    SubsetB,
    /// Subset C (high-density numeric pairs `00..=99`).
    SubsetC,
}

/// Supported 1D & 2D vector barcode symbologies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BarcodeSymbology {
    Ean13,
    Ean8,
    UpcA,
    Code128,
    Code39,
    Gs1128,
    DataMatrix,
    Pdf417,
    Aztec,
}

impl BarcodeSymbology {
    /// Parses a symbology token (case-insensitive, e.g. `"EAN13"`, `"EAN-13"`, `"Code128"`, `"DataMatrix"`).
    #[must_use]
    pub fn parse_name(name: &str) -> Option<Self> {
        match name
            .trim()
            .to_ascii_uppercase()
            .replace(['-', '_', ' '], "")
            .as_str()
        {
            "EAN13" => Some(Self::Ean13),
            "EAN8" => Some(Self::Ean8),
            "UPCA" | "UPC" => Some(Self::UpcA),
            "CODE128" | "C128" | "128" => Some(Self::Code128),
            "CODE39" | "C39" | "39" => Some(Self::Code39),
            "GS1128" | "UCC128" | "EAN128" => Some(Self::Gs1128),
            "DATAMATRIX" | "DM" | "ECC200" => Some(Self::DataMatrix),
            "PDF417" | "PDF" => Some(Self::Pdf417),
            "AZTEC" => Some(Self::Aztec),
            _ => None,
        }
    }

    /// Returns `true` for 2D matrix/stacked symbologies (`DataMatrix`, `Pdf417`, `Aztec`).
    #[must_use]
    pub fn is_2d(self) -> bool {
        matches!(self, Self::DataMatrix | Self::Pdf417 | Self::Aztec)
    }
}

/// Configuration for 1D & 2D vector barcode generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarcodeOptions {
    /// Width of the narrowest single bar/space module (X-dimension) in millimeters (default: `0.33 mm`).
    pub module_width_mm: f64,
    /// Height of standard data bars in millimeters (default: `20.0 mm`).
    pub bar_height_mm: f64,
    /// Extra downward extension of EAN/UPC guard bars in millimeters (default: `2.0 mm`).
    pub guard_extension_mm: f64,
    /// Quiet-zone padding in modules on left and right ends (default: `10`).
    pub quiet_zone_modules: u32,
    /// Include human-readable caption below the bars.
    pub include_human_readable: bool,
    /// Bar fill color (default: 100% process black `K=100%`).
    pub foreground: Color,
    /// Optional opaque white background plate.
    pub background: Option<Color>,
    /// Press dot-gain Bar Width Reduction (BWR) in millimeters (default: `0.0`).
    #[serde(default)]
    pub bar_width_reduction_mm: f64,
    /// Code128 subset mode (`Auto`, `SubsetA`, `SubsetB`, `SubsetC`).
    #[serde(default)]
    pub code128_mode: Code128Mode,
}

impl Default for BarcodeOptions {
    fn default() -> Self {
        Self {
            module_width_mm: 0.33,
            bar_height_mm: 20.0,
            guard_extension_mm: 2.0,
            quiet_zone_modules: 10,
            include_human_readable: true,
            foreground: Color::KEY_BLACK,
            background: Some(Color::cmyk(0.0, 0.0, 0.0, 0.0)),
            bar_width_reduction_mm: 0.0,
            code128_mode: Code128Mode::SubsetB,
        }
    }
}

/// Generated crisp vector 1D or 2D barcode output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarcodeOutput {
    pub symbology: BarcodeSymbology,
    /// Canonical encoded payload (including computed check digit for EAN/UPC).
    pub encoded_text: String,
    /// Encoded binary module sequence (`true` = dark bar/cell, `false` = light space), row-major for 2D.
    pub modules: Vec<bool>,
    /// Vector compound path of all dark bar/cell rectangles in millimeters.
    pub bars_path: BezierPath,
    /// Total physical bounding box including quiet zones and optional human-readable text space.
    pub bounds: Rect2D,
    /// Optional human-readable text label.
    pub human_readable_text: Option<String>,
    /// For 2D symbologies (`DataMatrix`, `Pdf417`, `Aztec`), number of matrix columns; `0` for 1D.
    #[serde(default)]
    pub matrix_cols: usize,
    /// For 2D symbologies (`DataMatrix`, `Pdf417`, `Aztec`), number of matrix rows; `0` for 1D.
    #[serde(default)]
    pub matrix_rows: usize,
}

/// Computes the GS1 Mod-10 check digit for a 12-digit EAN-13 payload.
pub fn ean13_check_digit(digits12: &str) -> ProdResult<u8> {
    let digits = parse_exact_digits(digits12, 12, "EAN-13")?;
    let sum: u32 = digits
        .iter()
        .enumerate()
        .map(|(i, &d)| u32::from(d) * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    Ok(((10 - (sum % 10)) % 10) as u8)
}

/// Computes the GS1 Mod-10 check digit for a 7-digit EAN-8 payload.
pub fn ean8_check_digit(digits7: &str) -> ProdResult<u8> {
    let digits = parse_exact_digits(digits7, 7, "EAN-8")?;
    let sum: u32 = digits
        .iter()
        .enumerate()
        .map(|(i, &d)| u32::from(d) * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    Ok(((10 - (sum % 10)) % 10) as u8)
}

/// Computes the GS1 Mod-10 check digit for an 11-digit UPC-A payload.
pub fn upca_check_digit(digits11: &str) -> ProdResult<u8> {
    let digits = parse_exact_digits(digits11, 11, "UPC-A")?;
    let sum: u32 = digits
        .iter()
        .enumerate()
        .map(|(i, &d)| u32::from(d) * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    Ok(((10 - (sum % 10)) % 10) as u8)
}

/// Generates a crisp vector 1D barcode for `symbology` and `data`.
pub fn generate_barcode(
    symbology: BarcodeSymbology,
    data: &str,
    options: &BarcodeOptions,
) -> ProdResult<BarcodeOutput> {
    if !options.module_width_mm.is_finite()
        || options.module_width_mm <= 0.0
        || options.module_width_mm > 1_000.0
    {
        return Err(ProdError::BarcodeError(
            "module_width_mm must be finite in (0, 1000]".into(),
        ));
    }
    if !options.bar_height_mm.is_finite()
        || options.bar_height_mm <= 0.0
        || options.bar_height_mm > 10_000.0
    {
        return Err(ProdError::BarcodeError(
            "bar_height_mm must be finite in (0, 10000]".into(),
        ));
    }
    if !options.guard_extension_mm.is_finite()
        || options.guard_extension_mm < 0.0
        || options.guard_extension_mm > 1_000.0
    {
        return Err(ProdError::BarcodeError(
            "guard_extension_mm must be finite in [0, 1000]".into(),
        ));
    }
    if options.quiet_zone_modules > MAX_QUIET_ZONE_MODULES {
        return Err(ProdError::BarcodeError(format!(
            "barcode quiet_zone_modules {} exceeds maximum {MAX_QUIET_ZONE_MODULES}",
            options.quiet_zone_modules
        )));
    }
    if !options.bar_width_reduction_mm.is_finite()
        || options.bar_width_reduction_mm < 0.0
        || options.bar_width_reduction_mm >= options.module_width_mm * 0.9
    {
        return Err(ProdError::BarcodeError(
            "bar_width_reduction_mm must be finite in [0, 0.9 * module_width_mm)".into(),
        ));
    }

    let bwr = options.bar_width_reduction_mm;
    let qz_mm = f64::from(options.quiet_zone_modules) * options.module_width_mm;

    if symbology.is_2d() {
        let (encoded_text, modules, cols, rows, row_height_mult) = match symbology {
            BarcodeSymbology::DataMatrix => encode_data_matrix(data)?,
            BarcodeSymbology::Pdf417 => encode_pdf417(data)?,
            BarcodeSymbology::Aztec => encode_aztec(data)?,
            _ => unreachable!(),
        };
        let cell_w = options.module_width_mm;
        let cell_h = options.module_width_mm * row_height_mult;
        let total_w = (cols as f64) * cell_w + 2.0 * qz_mm;
        let matrix_h = (rows as f64) * cell_h;
        let text_zone_h = if options.include_human_readable {
            (matrix_h * 0.18).clamp(3.0, 5.0)
        } else {
            0.0
        };
        let total_h = matrix_h + 2.0 * qz_mm + text_zone_h;
        if !total_w.is_finite() || !total_h.is_finite() || total_w > MAX_QR_SIZE_MM {
            return Err(ProdError::BarcodeError(
                "computed 2D barcode dimensions overflowed safe physical bounds".into(),
            ));
        }

        let inset_x = (bwr * 0.5).min(cell_w * 0.45);
        let inset_y = if row_height_mult == 1.0 {
            (bwr * 0.5).min(cell_h * 0.45)
        } else {
            0.0
        };
        let mut subpaths = Vec::new();
        for r in 0..rows {
            let mut c = 0usize;
            while c < cols {
                if modules[r * cols + c] {
                    let start_c = c;
                    while c < cols && modules[r * cols + c] {
                        c += 1;
                    }
                    let x = qz_mm + (start_c as f64) * cell_w + inset_x;
                    let w = (((c - start_c) as f64) * cell_w - 2.0 * inset_x).max(0.01);
                    let y = qz_mm + (r as f64) * cell_h + inset_y;
                    let h = (cell_h - 2.0 * inset_y).max(0.01);
                    subpaths.push(Subpath::from_points(
                        Rect2D::from_xywh(x, y, w, h).corners(),
                        true,
                    ));
                } else {
                    c += 1;
                }
            }
        }
        let human_readable_text = options.include_human_readable.then(|| encoded_text.clone());
        return Ok(BarcodeOutput {
            symbology,
            encoded_text,
            modules,
            bars_path: BezierPath { subpaths },
            bounds: Rect2D::from_xywh(0.0, 0.0, total_w, total_h),
            human_readable_text,
            matrix_cols: cols,
            matrix_rows: rows,
        });
    }

    let (encoded_text, modules, guard_indices) = match symbology {
        BarcodeSymbology::Ean13 => encode_ean13(data)?,
        BarcodeSymbology::Ean8 => encode_ean8(data)?,
        BarcodeSymbology::UpcA => encode_upca(data)?,
        BarcodeSymbology::Code128 => encode_code128_with_mode(data, options.code128_mode)?,
        BarcodeSymbology::Code39 => encode_code39(data)?,
        BarcodeSymbology::Gs1128 => encode_gs1_128(data)?,
        BarcodeSymbology::DataMatrix | BarcodeSymbology::Pdf417 | BarcodeSymbology::Aztec => {
            unreachable!()
        }
    };

    let total_w = (modules.len() as f64) * options.module_width_mm + 2.0 * qz_mm;
    let has_guards = !guard_indices.is_empty();
    let text_zone_h = if options.include_human_readable {
        (options.bar_height_mm * 0.22).clamp(3.0, 6.0)
    } else {
        0.0
    };
    let guard_ext = if has_guards {
        options.guard_extension_mm
    } else {
        0.0
    };
    let total_h = options.bar_height_mm + guard_ext.max(text_zone_h);
    if !total_w.is_finite() || !total_h.is_finite() || total_w > MAX_QR_SIZE_MM {
        return Err(ProdError::BarcodeError(
            "computed barcode dimensions overflowed safe physical bounds".into(),
        ));
    }

    // Emit merged vector bar rectangles (splitting only if bar height changes at guard indices)
    // and applying Bar Width Reduction (BWR) symmetrically on both edges of every dark bar run.
    let mut subpaths = Vec::new();
    let mut idx = 0usize;
    while idx < modules.len() {
        if modules[idx] {
            let is_guard = guard_indices.contains(&idx);
            let start = idx;
            while idx < modules.len() && modules[idx] && (guard_indices.contains(&idx) == is_guard)
            {
                idx += 1;
            }
            let raw_x = qz_mm + (start as f64) * options.module_width_mm;
            let raw_w = ((idx - start) as f64) * options.module_width_mm;
            let bwr_shift = (bwr * 0.5).min(raw_w * 0.45);
            let x = raw_x + bwr_shift;
            let w = (raw_w - 2.0 * bwr_shift).max(0.01);
            let h = if is_guard {
                options.bar_height_mm + guard_ext
            } else {
                options.bar_height_mm
            };
            subpaths.push(Subpath::from_points(
                Rect2D::from_xywh(x, 0.0, w, h).corners(),
                true,
            ));
        } else {
            idx += 1;
        }
    }

    let human_readable_text = options.include_human_readable.then(|| encoded_text.clone());

    Ok(BarcodeOutput {
        symbology,
        encoded_text,
        modules,
        bars_path: BezierPath { subpaths },
        bounds: Rect2D::from_xywh(0.0, 0.0, total_w, total_h),
        human_readable_text,
        matrix_cols: 0,
        matrix_rows: 0,
    })
}

/// Adds a grouped vector 1D barcode (optional background + vector bars + optional human-readable text)
/// to `doc` at `origin`, returning the group [`ObjectId`].
pub fn add_barcode_to_document(
    doc: &mut Document,
    symbology: BarcodeSymbology,
    data: &str,
    origin: Point2D,
    options: &BarcodeOptions,
    layer_id: Option<LayerId>,
) -> ProdResult<ObjectId> {
    if !origin.is_finite() {
        return Err(ProdError::BarcodeError(
            "barcode origin must be finite".into(),
        ));
    }
    let out = generate_barcode(symbology, data, options)?;
    let mut children = Vec::new();

    if let Some(bg) = &options.background {
        let bg_obj = SceneObject::new(
            ObjectId(0),
            "Barcode Background",
            NodePayload::Path(BezierPath::from_rect(out.bounds)),
        )
        .with_fill(Fill::Solid(bg.clone()))
        .with_outline(OutlineStyle::none());
        children.push(doc.add_object(bg_obj, layer_id)?);
    }

    let bars_obj = SceneObject::new(
        ObjectId(0),
        "Barcode Bars",
        NodePayload::Path(out.bars_path),
    )
    .with_fill(Fill::Solid(options.foreground.clone()))
    .with_outline(OutlineStyle::none());
    children.push(doc.add_object(bars_obj, layer_id)?);

    if let Some(caption) = out.human_readable_text {
        let caption_y = if symbology.is_2d() {
            (out.bounds.height() - 4.0).max(0.5)
        } else {
            options.bar_height_mm + 0.5
        };
        let font_pt = if symbology.is_2d() {
            7.5
        } else {
            ((out.bounds.height() - options.bar_height_mm) * 2.4).clamp(6.0, 12.0)
        };
        let text_obj = SceneObject::new(
            ObjectId(0),
            "Barcode Caption",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                caption,
                TextStyle {
                    font_family: "Noto Sans Mono".to_owned(),
                    font_size_pt: font_pt,
                    fill: options.foreground.clone(),
                    align: TextAlign::Center,
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(out.bounds.width() * 0.15, caption_y))
        .with_outline(OutlineStyle::none());
        children.push(doc.add_object(text_obj, layer_id)?);
    }

    let group_id = doc.group_objects(
        &children,
        format!("Barcode:{symbology:?}:{}", out.encoded_text),
    )?;
    let _ = doc.set_object_transform(group_id, Affine2D::translate(origin.x, origin.y));
    Ok(group_id)
}

// --- EAN-13 / EAN-8 / UPC-A tables ---

const EAN_L_CODES: [&str; 10] = [
    "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011",
    "0110111", "0001011",
];

const EAN_G_CODES: [&str; 10] = [
    "0100111", "0110011", "0011011", "0100001", "0011101", "0111001", "0000101", "0010001",
    "0001001", "0010111",
];

const EAN_R_CODES: [&str; 10] = [
    "1110010", "1100110", "1101100", "1000010", "1011100", "1001110", "1010000", "1000100",
    "1001000", "1110100",
];

const EAN13_PARITY: [&str; 10] = [
    "LLLLLL", "LLGLGG", "LLGGLG", "LLGGGL", "LGLLGG", "LGGLLG", "LGGGLL", "LGLGLG", "LGLGGL",
    "LGGLGL",
];

fn parse_exact_digits(input: &str, expected_len: usize, name: &str) -> ProdResult<Vec<u8>> {
    let trimmed = input.trim();
    if trimmed.len() != expected_len || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProdError::BarcodeError(format!(
            "{name} requires {expected_len} ASCII digits (got '{input}')"
        )));
    }
    Ok(trimmed.bytes().map(|b| b - b'0').collect())
}

fn push_bits(out: &mut Vec<bool>, bits: &str) {
    for b in bits.bytes() {
        out.push(b == b'1');
    }
}

fn encode_ean13(data: &str) -> ProdResult<(String, Vec<bool>, Vec<usize>)> {
    let trimmed = data.trim();
    if !trimmed.is_ascii() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProdError::BarcodeError(format!(
            "EAN-13 requires ASCII digits only (got '{trimmed}')"
        )));
    }
    let full = match trimmed.len() {
        12 => {
            let check = ean13_check_digit(trimmed)?;
            format!("{trimmed}{check}")
        }
        13 => {
            let expected = ean13_check_digit(&trimmed[..12])?;
            let actual = trimmed.as_bytes()[12] - b'0';
            if actual != expected {
                return Err(ProdError::BarcodeError(format!(
                    "EAN-13 invalid check digit in '{trimmed}': expected {expected}"
                )));
            }
            trimmed.to_owned()
        }
        _ => {
            return Err(ProdError::BarcodeError(format!(
                "EAN-13 requires 12 or 13 digits (got '{trimmed}')"
            )));
        }
    };

    let digits: Vec<usize> = full.bytes().map(|b| (b - b'0') as usize).collect();
    let parity = EAN13_PARITY[digits[0]].as_bytes();
    let mut modules = Vec::with_capacity(95);
    let mut guards = Vec::with_capacity(11);

    // Start guard (indices 0..3)
    guards.extend(0..3);
    push_bits(&mut modules, "101");

    // Left 6 digits (digits[1..=6])
    for i in 0..6 {
        let d = digits[1 + i];
        let pat = if parity[i] == b'L' {
            EAN_L_CODES[d]
        } else {
            EAN_G_CODES[d]
        };
        push_bits(&mut modules, pat);
    }

    // Center guard (indices 45..50)
    let center_start = modules.len();
    guards.extend(center_start..(center_start + 5));
    push_bits(&mut modules, "01010");

    // Right 6 digits (digits[7..=12])
    for &d in &digits[7..13] {
        push_bits(&mut modules, EAN_R_CODES[d]);
    }

    // End guard (indices 92..95)
    let end_start = modules.len();
    guards.extend(end_start..(end_start + 3));
    push_bits(&mut modules, "101");

    Ok((full, modules, guards))
}

fn encode_ean8(data: &str) -> ProdResult<(String, Vec<bool>, Vec<usize>)> {
    let trimmed = data.trim();
    if !trimmed.is_ascii() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProdError::BarcodeError(format!(
            "EAN-8 requires ASCII digits only (got '{trimmed}')"
        )));
    }
    let full = match trimmed.len() {
        7 => {
            let check = ean8_check_digit(trimmed)?;
            format!("{trimmed}{check}")
        }
        8 => {
            let expected = ean8_check_digit(&trimmed[..7])?;
            let actual = trimmed.as_bytes()[7] - b'0';
            if actual != expected {
                return Err(ProdError::BarcodeError(format!(
                    "EAN-8 invalid check digit in '{trimmed}': expected {expected}"
                )));
            }
            trimmed.to_owned()
        }
        _ => {
            return Err(ProdError::BarcodeError(format!(
                "EAN-8 requires 7 or 8 digits (got '{trimmed}')"
            )));
        }
    };

    let digits: Vec<usize> = full.bytes().map(|b| (b - b'0') as usize).collect();
    let mut modules = Vec::with_capacity(67);
    let mut guards = Vec::with_capacity(11);

    guards.extend(0..3);
    push_bits(&mut modules, "101");

    for &d in &digits[0..4] {
        push_bits(&mut modules, EAN_L_CODES[d]);
    }

    let center_start = modules.len();
    guards.extend(center_start..(center_start + 5));
    push_bits(&mut modules, "01010");

    for &d in &digits[4..8] {
        push_bits(&mut modules, EAN_R_CODES[d]);
    }

    let end_start = modules.len();
    guards.extend(end_start..(end_start + 3));
    push_bits(&mut modules, "101");

    Ok((full, modules, guards))
}

fn encode_upca(data: &str) -> ProdResult<(String, Vec<bool>, Vec<usize>)> {
    let trimmed = data.trim();
    if !trimmed.is_ascii() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProdError::BarcodeError(format!(
            "UPC-A requires ASCII digits only (got '{trimmed}')"
        )));
    }
    let full = match trimmed.len() {
        11 => {
            let check = upca_check_digit(trimmed)?;
            format!("{trimmed}{check}")
        }
        12 => {
            let expected = upca_check_digit(&trimmed[..11])?;
            let actual = trimmed.as_bytes()[11] - b'0';
            if actual != expected {
                return Err(ProdError::BarcodeError(format!(
                    "UPC-A invalid check digit in '{trimmed}': expected {expected}"
                )));
            }
            trimmed.to_owned()
        }
        _ => {
            return Err(ProdError::BarcodeError(format!(
                "UPC-A requires 11 or 12 digits (got '{trimmed}')"
            )));
        }
    };

    let digits: Vec<usize> = full.bytes().map(|b| (b - b'0') as usize).collect();
    let mut modules = Vec::with_capacity(95);
    let mut guards = Vec::with_capacity(25);

    // Start guard + first digit (extended in UPC-A)
    guards.extend(0..10);
    push_bits(&mut modules, "101");
    for &d in &digits[0..6] {
        push_bits(&mut modules, EAN_L_CODES[d]);
    }

    let center_start = modules.len();
    guards.extend(center_start..(center_start + 5));
    push_bits(&mut modules, "01010");

    for &d in &digits[6..12] {
        push_bits(&mut modules, EAN_R_CODES[d]);
    }

    // Last digit + end guard extended
    let last_digit_start = modules.len() - 7;
    guards.extend(last_digit_start..(modules.len() + 3));
    push_bits(&mut modules, "101");

    Ok((full, modules, guards))
}

// --- Code 128 (ISO/IEC 15417) ---

const CODE128_PATTERNS: [&str; 107] = [
    "212222", "222122", "222221", "121223", "121322", "131222", "122213", "122312", "132212",
    "221213", "221312", "231212", "112232", "122132", "122231", "113222", "123122", "123221",
    "223211", "221132", "221231", "213212", "223112", "312131", "311222", "321122", "321221",
    "312212", "322112", "322211", "212123", "212321", "232121", "111323", "131123", "131321",
    "112313", "132113", "132311", "211313", "231113", "231311", "112133", "112331", "132131",
    "113123", "113321", "133121", "313121", "211331", "231131", "213113", "213311", "213131",
    "311123", "311321", "331121", "312113", "312311", "332111", "314111", "221411", "431111",
    "111224", "111422", "121124", "121421", "141122", "141221", "112214", "112412", "122114",
    "122411", "142112", "142211", "241211", "221114", "413111", "241112", "134111", "111242",
    "121142", "121241", "114212", "124112", "124211", "411212", "421112", "421211", "212141",
    "214121", "412121", "111143", "111341", "131141", "114113", "114311", "411113", "411311",
    "113141", "114131", "311141", "411131", "211412", "211214", "211232", "2331112",
];

fn append_code128_symbol(modules: &mut Vec<bool>, code_value: usize) {
    let pat = CODE128_PATTERNS[code_value];
    for (idx, b) in pat.bytes().enumerate() {
        let count = (b - b'0') as usize;
        let is_bar = idx % 2 == 0;
        for _ in 0..count {
            modules.push(is_bar);
        }
    }
}

fn finalize_code128_values(start_code: usize, values: &[usize]) -> Vec<bool> {
    const STOP: usize = 106;
    let mut checksum = start_code;
    for (i, &val) in values.iter().enumerate() {
        checksum += (i + 1) * val;
    }
    let check_val = checksum % 103;
    let mut modules = Vec::with_capacity((values.len() + 2) * 11 + 13);
    append_code128_symbol(&mut modules, start_code);
    for &val in values {
        append_code128_symbol(&mut modules, val);
    }
    append_code128_symbol(&mut modules, check_val);
    append_code128_symbol(&mut modules, STOP);
    modules
}

fn code128_a_val(b: u8) -> Option<usize> {
    if b < 32 {
        Some(usize::from(b) + 64)
    } else if b <= 95 {
        Some(usize::from(b - 32))
    } else {
        None
    }
}

fn code128_b_val(b: u8) -> Option<usize> {
    if (32..=126).contains(&b) {
        Some(usize::from(b - 32))
    } else {
        None
    }
}

fn count_leading_digits(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|b| b.is_ascii_digit()).count()
}

fn encode_code128_bytes_auto(bytes: &[u8], prepend_fnc1: bool) -> ProdResult<(usize, Vec<usize>)> {
    const FNC1: usize = 102;
    const CODE_C: usize = 99;
    const CODE_B: usize = 100;
    const CODE_A: usize = 101;
    const START_A: usize = 103;
    const START_B: usize = 104;
    const START_C: usize = 105;

    for &b in bytes {
        if b > 126 && b != 0x1D {
            return Err(ProdError::BarcodeError(format!(
                "Code128 only supports ASCII 0..=126 (got byte 0x{b:02X})"
            )));
        }
    }

    let leading_digits = count_leading_digits(bytes);
    let mut subset =
        if leading_digits >= 4 || (leading_digits >= 2 && leading_digits == bytes.len()) {
            'C'
        } else if bytes.first().is_some_and(|&b| b < 32) {
            'A'
        } else {
            'B'
        };

    let start_code = match subset {
        'A' => START_A,
        'B' => START_B,
        _ => START_C,
    };

    let mut values = Vec::with_capacity(bytes.len() + 4);
    if prepend_fnc1 {
        values.push(FNC1);
    }

    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1D {
            values.push(FNC1);
            i += 1;
            continue;
        }
        let rem = &bytes[i..];
        let digits = count_leading_digits(rem);
        match subset {
            'C' => {
                if digits >= 2 {
                    let pair = usize::from(rem[0] - b'0') * 10 + usize::from(rem[1] - b'0');
                    values.push(pair);
                    i += 2;
                } else if rem[0] < 32 {
                    values.push(CODE_A);
                    subset = 'A';
                    values.push(code128_a_val(rem[0]).expect("validated"));
                    i += 1;
                } else {
                    values.push(CODE_B);
                    subset = 'B';
                    values.push(code128_b_val(rem[0]).expect("validated"));
                    i += 1;
                }
            }
            'A' => {
                if digits >= 4 {
                    values.push(CODE_C);
                    subset = 'C';
                } else if let Some(v) = code128_a_val(rem[0]) {
                    values.push(v);
                    i += 1;
                } else {
                    values.push(CODE_B);
                    subset = 'B';
                    values.push(code128_b_val(rem[0]).expect("validated"));
                    i += 1;
                }
            }
            _ => {
                if digits >= 4 {
                    values.push(CODE_C);
                    subset = 'C';
                } else if let Some(v) = code128_b_val(rem[0]) {
                    values.push(v);
                    i += 1;
                } else {
                    values.push(CODE_A);
                    subset = 'A';
                    values.push(code128_a_val(rem[0]).expect("validated"));
                    i += 1;
                }
            }
        }
    }

    Ok((start_code, values))
}

fn encode_code128_with_mode(
    data: &str,
    mode: Code128Mode,
) -> ProdResult<(String, Vec<bool>, Vec<usize>)> {
    if data.is_empty() {
        return Err(ProdError::BarcodeError(
            "Code128 payload cannot be empty".into(),
        ));
    }
    const START_A: usize = 103;
    const START_B: usize = 104;
    const START_C: usize = 105;

    let (start_code, values) = match mode {
        Code128Mode::SubsetA => {
            let mut vals = Vec::with_capacity(data.len());
            for b in data.bytes() {
                vals.push(code128_a_val(b).ok_or_else(|| {
                    ProdError::BarcodeError(format!(
                        "Code128 Subset A only supports ASCII 0..=95, invalid byte 0x{b:02X}"
                    ))
                })?);
            }
            (START_A, vals)
        }
        Code128Mode::SubsetB => {
            let mut vals = Vec::with_capacity(data.len());
            for ch in data.chars() {
                let cp = ch as u32;
                if !(32..=126).contains(&cp) {
                    return Err(ProdError::BarcodeError(format!(
                        "Code128 Subset B only supports printable ASCII (32..=126), invalid char '{ch}'"
                    )));
                }
                vals.push((cp - 32) as usize);
            }
            (START_B, vals)
        }
        Code128Mode::SubsetC => {
            let bytes = data.as_bytes();
            if !bytes.len().is_multiple_of(2) || !bytes.iter().all(u8::is_ascii_digit) {
                return Err(ProdError::BarcodeError(
                    "Code128 Subset C requires an even number of ASCII digits".into(),
                ));
            }
            let mut vals = Vec::with_capacity(bytes.len() / 2);
            for pair in bytes.as_chunks::<2>().0 {
                vals.push(usize::from(pair[0] - b'0') * 10 + usize::from(pair[1] - b'0'));
            }
            (START_C, vals)
        }
        Code128Mode::Auto => encode_code128_bytes_auto(data.as_bytes(), false)?,
    };

    Ok((
        data.to_owned(),
        finalize_code128_values(start_code, &values),
        Vec::new(),
    ))
}

fn gs1_ai_is_fixed_length(ai: &str) -> bool {
    matches!(
        ai,
        "00" | "01"
            | "02"
            | "03"
            | "04"
            | "11"
            | "12"
            | "13"
            | "14"
            | "15"
            | "16"
            | "17"
            | "18"
            | "19"
            | "20"
    ) || (ai.len() == 4
        && (ai.starts_with("31")
            || ai.starts_with("32")
            || ai.starts_with("33")
            || ai.starts_with("34")
            || ai.starts_with("35")
            || ai.starts_with("36")
            || ai.starts_with("41")))
}

fn encode_gs1_128(data: &str) -> ProdResult<(String, Vec<bool>, Vec<usize>)> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Err(ProdError::BarcodeError(
            "GS1-128 payload cannot be empty".into(),
        ));
    }
    let normalized = trimmed.replace("<FNC1>", "\x1D");
    let mut raw_bytes = Vec::with_capacity(normalized.len() + 4);
    let mut display_text = String::with_capacity(normalized.len());

    if normalized.starts_with('(') {
        let mut rest = normalized.as_str();
        let mut prev_needs_fnc1 = false;
        while !rest.is_empty() {
            if !rest.starts_with('(') {
                return Err(ProdError::BarcodeError(format!(
                    "GS1-128 malformed AI syntax near '{rest}'"
                )));
            }
            let close = rest.find(')').ok_or_else(|| {
                ProdError::BarcodeError("GS1-128 missing closing ')' in AI".into())
            })?;
            let ai = &rest[1..close];
            if ai.len() < 2 || !ai.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ProdError::BarcodeError(format!(
                    "GS1-128 invalid Application Identifier '({ai})'"
                )));
            }
            let after_ai = &rest[close + 1..];
            let next_paren = after_ai.find('(').unwrap_or(after_ai.len());
            let value = &after_ai[..next_paren];
            if value.is_empty() {
                return Err(ProdError::BarcodeError(format!(
                    "GS1-128 AI '({ai})' has empty data value"
                )));
            }
            if prev_needs_fnc1 {
                raw_bytes.push(0x1D);
            }
            raw_bytes.extend_from_slice(ai.as_bytes());
            raw_bytes.extend_from_slice(value.as_bytes());
            display_text.push('(');
            display_text.push_str(ai);
            display_text.push(')');
            display_text.push_str(value);
            prev_needs_fnc1 = !gs1_ai_is_fixed_length(ai);
            rest = &after_ai[next_paren..];
        }
    } else {
        raw_bytes.extend_from_slice(normalized.as_bytes());
        display_text.push_str(trimmed);
    }

    let (start_code, values) = encode_code128_bytes_auto(&raw_bytes, true)?;
    Ok((
        display_text,
        finalize_code128_values(start_code, &values),
        Vec::new(),
    ))
}

// --- DataMatrix ECC200 (ISO/IEC 16022) ---

fn gf256_mul_dm(a: u8, b: u8) -> u8 {
    let mut a = u16::from(a);
    let mut b = u16::from(b);
    let mut p = 0u16;
    for _ in 0..8 {
        if (b & 1) != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x12D;
        }
        b >>= 1;
    }
    p as u8
}

fn gf256_pow2_dm(exp: usize) -> u8 {
    let mut v = 1u8;
    for _ in 0..exp {
        v = gf256_mul_dm(v, 2);
    }
    v
}

fn dm_rs_block(data: &[u8], ecc_len: usize) -> Vec<u8> {
    let mut gen = vec![0u8; ecc_len + 1];
    gen[0] = 1;
    for i in 1..=ecc_len {
        let root = gf256_pow2_dm(i);
        for j in (1..=i).rev() {
            gen[j] ^= gf256_mul_dm(gen[j - 1], root);
        }
    }
    let mut ecc = vec![0u8; ecc_len];
    for &d in data {
        let factor = d ^ ecc[ecc_len - 1];
        for j in (1..ecc_len).rev() {
            ecc[j] = ecc[j - 1] ^ gf256_mul_dm(gen[ecc_len - j], factor);
        }
        ecc[0] = gf256_mul_dm(gen[ecc_len], factor);
    }
    // Reverse into highest-degree-first order expected by ECC200 placement.
    ecc.reverse();
    ecc
}

fn dm_place_bit(
    arr: &mut [i16],
    nrow: isize,
    ncol: isize,
    mut row: isize,
    mut col: isize,
    cw_idx: usize,
    bit: u8,
) {
    if row < 0 {
        row += nrow;
        col += 4 - ((nrow + 4) % 8);
    }
    if col < 0 {
        col += ncol;
        row += 4 - ((ncol + 4) % 8);
    }
    let r = (row as usize) % (nrow as usize);
    let c = (col as usize) % (ncol as usize);
    arr[r * (ncol as usize) + c] = ((cw_idx << 3) | usize::from(bit)) as i16;
}

fn dm_place_utah(arr: &mut [i16], nrow: isize, ncol: isize, r: isize, c: isize, cw_idx: usize) {
    dm_place_bit(arr, nrow, ncol, r - 2, c - 2, cw_idx, 7);
    dm_place_bit(arr, nrow, ncol, r - 2, c - 1, cw_idx, 6);
    dm_place_bit(arr, nrow, ncol, r - 1, c - 2, cw_idx, 5);
    dm_place_bit(arr, nrow, ncol, r - 1, c - 1, cw_idx, 4);
    dm_place_bit(arr, nrow, ncol, r - 1, c, cw_idx, 3);
    dm_place_bit(arr, nrow, ncol, r, c - 2, cw_idx, 2);
    dm_place_bit(arr, nrow, ncol, r, c - 1, cw_idx, 1);
    dm_place_bit(arr, nrow, ncol, r, c, cw_idx, 0);
}

fn dm_ecc200_placement(nrow: usize, ncol: usize) -> Vec<i16> {
    let mut arr = vec![-1i16; nrow * ncol];
    let nr = nrow as isize;
    let nc = ncol as isize;
    let mut cw = 0usize;
    let mut r = 4isize;
    let mut c = 0isize;

    loop {
        if r == nr && c == 0 {
            dm_place_bit(&mut arr, nr, nc, nr - 1, 0, cw, 7);
            dm_place_bit(&mut arr, nr, nc, nr - 1, 1, cw, 6);
            dm_place_bit(&mut arr, nr, nc, nr - 1, 2, cw, 5);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 2, cw, 4);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 1, cw, 3);
            dm_place_bit(&mut arr, nr, nc, 1, nc - 1, cw, 2);
            dm_place_bit(&mut arr, nr, nc, 2, nc - 1, cw, 1);
            dm_place_bit(&mut arr, nr, nc, 3, nc - 1, cw, 0);
            cw += 1;
        }
        if r == nr - 2 && c == 0 && (nc % 4) != 0 {
            dm_place_bit(&mut arr, nr, nc, nr - 3, 0, cw, 7);
            dm_place_bit(&mut arr, nr, nc, nr - 2, 0, cw, 6);
            dm_place_bit(&mut arr, nr, nc, nr - 1, 0, cw, 5);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 4, cw, 4);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 3, cw, 3);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 2, cw, 2);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 1, cw, 1);
            dm_place_bit(&mut arr, nr, nc, 1, nc - 1, cw, 0);
            cw += 1;
        }
        if r == nr - 2 && c == 0 && (nc % 8) == 4 {
            dm_place_bit(&mut arr, nr, nc, nr - 3, 0, cw, 7);
            dm_place_bit(&mut arr, nr, nc, nr - 2, 0, cw, 6);
            dm_place_bit(&mut arr, nr, nc, nr - 1, 0, cw, 5);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 2, cw, 4);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 1, cw, 3);
            dm_place_bit(&mut arr, nr, nc, 1, nc - 1, cw, 2);
            dm_place_bit(&mut arr, nr, nc, 2, nc - 1, cw, 1);
            dm_place_bit(&mut arr, nr, nc, 3, nc - 1, cw, 0);
            cw += 1;
        }
        if r == nr + 4 && c == 2 && (nc % 8) == 0 {
            dm_place_bit(&mut arr, nr, nc, nr - 1, 0, cw, 7);
            dm_place_bit(&mut arr, nr, nc, nr - 1, nc - 1, cw, 6);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 3, cw, 5);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 2, cw, 4);
            dm_place_bit(&mut arr, nr, nc, 0, nc - 1, cw, 3);
            dm_place_bit(&mut arr, nr, nc, 1, nc - 3, cw, 2);
            dm_place_bit(&mut arr, nr, nc, 1, nc - 2, cw, 1);
            dm_place_bit(&mut arr, nr, nc, 1, nc - 1, cw, 0);
            cw += 1;
        }

        loop {
            if r < nr && c >= 0 && arr[(r as usize) * ncol + (c as usize)] == -1 {
                dm_place_utah(&mut arr, nr, nc, r, c, cw);
                cw += 1;
            }
            r -= 2;
            c += 2;
            if r < 0 || c >= nc {
                break;
            }
        }
        r += 1;
        c += 3;

        loop {
            if r >= 0 && c < nc && arr[(r as usize) * ncol + (c as usize)] == -1 {
                dm_place_utah(&mut arr, nr, nc, r, c, cw);
                cw += 1;
            }
            r += 2;
            c -= 2;
            if r >= nr || c < 0 {
                break;
            }
        }
        r += 3;
        c += 1;

        if r >= nr && c >= nc {
            break;
        }
    }

    if arr[(nrow - 1) * ncol + (ncol - 1)] == -1 {
        arr[(nrow - 1) * ncol + (ncol - 1)] = -2;
        arr[(nrow - 2) * ncol + (ncol - 2)] = -2;
    }
    arr
}

fn encode_data_matrix(data: &str) -> ProdResult<(String, Vec<bool>, usize, usize, f64)> {
    if data.is_empty() {
        return Err(ProdError::BarcodeError(
            "DataMatrix payload cannot be empty".into(),
        ));
    }
    let bytes = data.as_bytes();
    let mut cw = Vec::with_capacity(bytes.len() + 8);
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i].is_ascii_digit() && bytes[i + 1].is_ascii_digit() {
            let pair = (bytes[i] - b'0') * 10 + (bytes[i + 1] - b'0');
            cw.push(130 + pair);
            i += 2;
        } else if bytes[i] >= 128 {
            cw.push(235);
            cw.push((bytes[i] - 128) + 1);
            i += 1;
        } else {
            cw.push(bytes[i] + 1);
            i += 1;
        }
    }

    // (symbol_side, regions_per_side, data_cw, ecc_cw, blocks)
    const DM_SIZES: [(usize, usize, usize, usize, usize); 18] = [
        (10, 1, 3, 5, 1),
        (12, 1, 5, 7, 1),
        (14, 1, 8, 10, 1),
        (16, 1, 12, 12, 1),
        (18, 1, 18, 14, 1),
        (20, 1, 22, 18, 1),
        (22, 1, 30, 20, 1),
        (24, 1, 36, 24, 1),
        (26, 1, 44, 28, 1),
        (32, 2, 62, 36, 1),
        (36, 2, 86, 42, 1),
        (40, 2, 114, 48, 1),
        (44, 2, 144, 56, 1),
        (48, 2, 174, 68, 1),
        (52, 2, 204, 84, 2),
        (64, 4, 280, 112, 2),
        (72, 4, 368, 144, 4),
        (80, 4, 456, 192, 4),
    ];

    let &(side, regions, data_cap, ecc_total, blocks) = DM_SIZES
        .iter()
        .find(|&&(_, _, cap, _, _)| cap >= cw.len())
        .ok_or_else(|| {
            ProdError::BarcodeError(format!(
                "DataMatrix payload requires {} codewords, exceeding max ECC200 80x80 capacity",
                cw.len()
            ))
        })?;

    if cw.len() < data_cap {
        cw.push(129);
        while cw.len() < data_cap {
            let pos = cw.len() + 1;
            let rand_pad = (129 + ((149 * pos) % 253) + 1) % 254;
            cw.push(rand_pad as u8);
        }
    }

    let ecc_per_block = ecc_total / blocks;
    let mut full_cw = vec![0u8; data_cap + ecc_total];
    full_cw[..data_cap].copy_from_slice(&cw);
    for b in 0..blocks {
        let mut block_data = Vec::with_capacity(data_cap.div_ceil(blocks));
        let mut idx = b;
        while idx < data_cap {
            block_data.push(cw[idx]);
            idx += blocks;
        }
        let block_ecc = dm_rs_block(&block_data, ecc_per_block);
        for (k, &ev) in block_ecc.iter().enumerate() {
            full_cw[data_cap + k * blocks + b] = ev;
        }
    }

    let map_side = side - 2 * regions;
    let placement = dm_ecc200_placement(map_side, map_side);
    let region_full = side / regions;
    let region_data = region_full - 2;
    let mut grid = vec![false; side * side];

    // Draw L-finder and alternating clock patterns per region
    for ry in 0..regions {
        for rx in 0..regions {
            let y0 = ry * region_full;
            let x0 = rx * region_full;
            for k in 0..region_full {
                grid[(y0 + region_full - 1) * side + (x0 + k)] = true; // bottom solid
                grid[(y0 + k) * side + x0] = true; // left solid
                grid[y0 * side + (x0 + k)] = k % 2 == 0; // top clock
                grid[(y0 + k) * side + (x0 + region_full - 1)] = k % 2 == 1; // right clock
            }
        }
    }

    for mr in 0..map_side {
        for mc in 0..map_side {
            let code = placement[mr * map_side + mc];
            let dark = if code == -2 {
                true
            } else if code >= 0 {
                let cw_idx = (code as usize) >> 3;
                let bit = (code as usize) & 7;
                cw_idx < full_cw.len() && ((full_cw[cw_idx] >> bit) & 1) != 0
            } else {
                false
            };
            let ry = mr / region_data;
            let rx = mc / region_data;
            let sr = ry * region_full + 1 + (mr % region_data);
            let sc = rx * region_full + 1 + (mc % region_data);
            grid[sr * side + sc] = dark;
        }
    }

    Ok((data.to_owned(), grid, side, side, 1.0))
}

// --- PDF417 (ISO/IEC 15438) ---

fn pdf417_cluster_tables() -> [Vec<u32>; 3] {
    let mut tables = [Vec::new(), Vec::new(), Vec::new()];
    let mut widths = [1u8; 8];
    fn gen(idx: usize, rem: u8, widths: &mut [u8; 8], tables: &mut [Vec<u32>; 3]) {
        if idx == 7 {
            if (1..=6).contains(&rem) {
                widths[7] = rem;
                let k = (i32::from(widths[0]) - i32::from(widths[2]) + i32::from(widths[4])
                    - i32::from(widths[6])
                    + 18)
                    % 9;
                if let Some(c) = match k {
                    0 => Some(0usize),
                    3 => Some(1usize),
                    6 => Some(2usize),
                    _ => None,
                } {
                    if tables[c].len() < 929 {
                        let mut pat = 0u32;
                        for (pos, &w) in widths.iter().enumerate() {
                            let is_bar = pos % 2 == 0;
                            for _ in 0..w {
                                pat = (pat << 1) | u32::from(is_bar);
                            }
                        }
                        tables[c].push(pat);
                    }
                }
            }
            return;
        }
        let min_left = (7 - idx) as u8;
        let max_w = rem.saturating_sub(min_left).min(6);
        for w in 1..=max_w {
            widths[idx] = w;
            gen(idx + 1, rem - w, widths, tables);
        }
    }
    gen(0, 17, &mut widths, &mut tables);
    tables
}

fn pdf417_compact_codewords(bytes: &[u8]) -> Vec<u16> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut cw = Vec::with_capacity(bytes.len() + 2);
    // Use Byte Compaction mode (`901` for non-multiple-of-6, `924` when multiple of 6)
    cw.push(if bytes.len().is_multiple_of(6) {
        924
    } else {
        901
    });
    for chunk in bytes.chunks(6) {
        if chunk.len() == 6 {
            let mut val: u64 = 0;
            for &b in chunk {
                val = (val << 8) | u64::from(b);
            }
            let mut base900 = [0u16; 5];
            for slot in base900.iter_mut().rev() {
                *slot = (val % 900) as u16;
                val /= 900;
            }
            cw.extend_from_slice(&base900);
        } else {
            for &b in chunk {
                cw.push(u16::from(b));
            }
        }
    }
    cw
}

fn pdf417_rs_ecc(data: &[u16], ec_level: u8) -> Vec<u16> {
    let ecc_len = 1usize << (ec_level + 1);
    let mut gen = vec![0u32; ecc_len + 1];
    gen[0] = 1;
    let mut root = 3u32;
    for i in 1..=ecc_len {
        for j in (1..=i).rev() {
            let term = (gen[j - 1] * root) % 929;
            gen[j] = (gen[j] + 929 - term) % 929;
        }
        root = (root * 3) % 929;
    }
    // Reverse so gen[0]..gen[ecc_len-1] align with polynomial remainder division
    let mut ecc = vec![0u32; ecc_len];
    for &d in data {
        let t = (u32::from(d) + ecc[0]) % 929;
        for j in 0..ecc_len - 1 {
            ecc[j] = (ecc[j + 1] + 929 - (t * gen[j + 1]) % 929) % 929;
        }
        ecc[ecc_len - 1] = (929 - (t * gen[ecc_len]) % 929) % 929;
    }
    ecc.into_iter().map(|v| ((929 - v) % 929) as u16).collect()
}

fn push_pdf417_bits(row: &mut Vec<bool>, pat: u32, width: u32) {
    for shift in (0..width).rev() {
        row.push(((pat >> shift) & 1) != 0);
    }
}

fn encode_pdf417(data: &str) -> ProdResult<(String, Vec<bool>, usize, usize, f64)> {
    if data.is_empty() {
        return Err(ProdError::BarcodeError(
            "PDF417 payload cannot be empty".into(),
        ));
    }
    let body = pdf417_compact_codewords(data.as_bytes());
    let ec_level: u8 = if body.len() <= 40 { 2 } else { 3 };
    let ecc_len = 1usize << (ec_level + 1);
    let total_raw = 1 + body.len() + ecc_len;
    if total_raw > 90 * 12 {
        return Err(ProdError::BarcodeError(format!(
            "PDF417 payload exceeds maximum 1080-codeword matrix capacity ({total_raw} > 1080)"
        )));
    }
    let cols = (total_raw.div_ceil(12)).clamp(2, 12);
    let rows = total_raw.div_ceil(cols).clamp(3, 90);
    let total_cells = rows * cols;
    if total_cells < total_raw {
        return Err(ProdError::BarcodeError(format!(
            "PDF417 matrix capacity {total_cells} insufficient for {total_raw} codewords"
        )));
    }
    let pad_len = total_cells - ecc_len - 1 - body.len();

    let mut data_cw = Vec::with_capacity(total_cells - ecc_len);
    data_cw.push((1 + body.len() + pad_len) as u16);
    data_cw.extend_from_slice(&body);
    data_cw.resize(total_cells - ecc_len, 900);
    let ecc = pdf417_rs_ecc(&data_cw, ec_level);
    data_cw.extend_from_slice(&ecc);

    let clusters = pdf417_cluster_tables();
    const START_PATTERN: u32 = 0x1FEA8; // 17 modules
    const STOP_PATTERN: u32 = 0x3FA29; // 18 modules

    let total_cols = 17 + 17 * (cols + 2) + 18;
    let mut modules = Vec::with_capacity(rows * total_cols);

    for r in 0..rows {
        let cluster_idx = r % 3;
        let row_group = (r / 3) as u16;
        let left_val = 30 * row_group
            + match cluster_idx {
                0 => ((rows - 1) / 3) as u16,
                1 => u16::from(ec_level) * 3 + ((rows - 1) % 3) as u16,
                _ => (cols - 1) as u16,
            };
        let right_val = 30 * row_group
            + match cluster_idx {
                0 => (cols - 1) as u16,
                1 => ((rows - 1) / 3) as u16,
                _ => u16::from(ec_level) * 3 + ((rows - 1) % 3) as u16,
            };
        let table = &clusters[cluster_idx];
        push_pdf417_bits(&mut modules, START_PATTERN, 17);
        push_pdf417_bits(&mut modules, table[usize::from(left_val) % 929], 17);
        for c in 0..cols {
            let cw = usize::from(data_cw[r * cols + c]) % 929;
            push_pdf417_bits(&mut modules, table[cw], 17);
        }
        push_pdf417_bits(&mut modules, table[usize::from(right_val) % 929], 17);
        push_pdf417_bits(&mut modules, STOP_PATTERN, 18);
    }

    Ok((data.to_owned(), modules, total_cols, rows, 3.0))
}

// --- Aztec Code (ISO/IEC 24778) ---

fn gf_mul_aztec(mut a: u16, mut b: u16, poly: u16, mask: u16) -> u16 {
    let hi_bit = (mask >> 1) + 1;
    let mut p = 0u16;
    while b > 0 {
        if (b & 1) != 0 {
            p ^= a;
        }
        let carry = (a & hi_bit) != 0;
        a = (a << 1) & mask;
        if carry {
            a ^= poly & mask;
        }
        b >>= 1;
    }
    p
}

fn aztec_rs_words(data: &[u16], ecc_count: usize, bits: usize) -> Vec<u16> {
    let (poly, mask) = match bits {
        4 => (0x13u16, 0x0Fu16),
        6 => (0x43u16, 0x3Fu16),
        8 => (0x12Du16, 0xFFu16),
        10 => (0x409u16, 0x3FFu16),
        _ => (0x1069u16, 0xFFFu16),
    };
    let mut gen = vec![0u16; ecc_count + 1];
    gen[0] = 1;
    let mut alpha_pow = 2u16;
    for i in 1..=ecc_count {
        for j in (1..=i).rev() {
            gen[j] ^= gf_mul_aztec(gen[j - 1], alpha_pow, poly, mask);
        }
        alpha_pow = gf_mul_aztec(alpha_pow, 2, poly, mask);
    }
    let mut ecc = vec![0u16; ecc_count];
    for &d in data {
        let factor = d ^ ecc[0];
        for j in 0..ecc_count - 1 {
            ecc[j] = ecc[j + 1] ^ gf_mul_aztec(factor, gen[j + 1], poly, mask);
        }
        ecc[ecc_count - 1] = gf_mul_aztec(factor, gen[ecc_count], poly, mask);
    }
    ecc
}

fn aztec_stuff_bits(bits: &[bool], word_size: usize) -> Vec<u16> {
    let mut words = Vec::new();
    let mask = (1u16 << word_size) - 1;
    let mut i = 0usize;
    while i < bits.len() {
        let mut word = 0u16;
        let mut count = 0usize;
        while count < word_size - 1 && i < bits.len() {
            word = (word << 1) | u16::from(bits[i]);
            i += 1;
            count += 1;
        }
        if count < word_size - 1 {
            word <<= (word_size - 1) - count;
        }
        if word == 0 {
            words.push(1);
        } else if word == (mask >> 1) {
            words.push(mask - 1);
        } else {
            let next_bit = if i < bits.len() {
                let b = u16::from(bits[i]);
                i += 1;
                b
            } else {
                1
            };
            words.push((word << 1) | next_bit);
        }
    }
    words
}

fn encode_aztec(data: &str) -> ProdResult<(String, Vec<bool>, usize, usize, f64)> {
    if data.is_empty() {
        return Err(ProdError::BarcodeError(
            "Aztec payload cannot be empty".into(),
        ));
    }
    let bytes = data.as_bytes();
    let mut bits = Vec::with_capacity(bytes.len() * 8 + 20);
    // Byte Shift latch (31 in 5-bit Upper) + byte length
    for s in (0..5).rev() {
        bits.push(((31 >> s) & 1) != 0);
    }
    if bytes.len() <= 31 {
        for s in (0..5).rev() {
            bits.push((((bytes.len() as u32) >> s) & 1) != 0);
        }
    } else {
        bits.extend(std::iter::repeat_n(false, 5));
        let extra = (bytes.len() - 31) as u32;
        for s in (0..11).rev() {
            bits.push(((extra >> s) & 1) != 0);
        }
    }
    for &b in bytes {
        for s in (0..8).rev() {
            bits.push(((b >> s) & 1) != 0);
        }
    }

    // Choose Compact (1..=4 layers) or Full (5..=16 layers)
    let mut chosen: Option<(bool, usize, usize, usize, Vec<u16>)> = None;
    for &(compact, layers, word_size, total_bits) in &[
        (true, 1usize, 6usize, 104usize),
        (true, 2, 6, 240),
        (true, 3, 8, 408),
        (true, 4, 8, 608),
        (false, 5, 8, 960),
        (false, 6, 8, 1248),
        (false, 7, 8, 1568),
        (false, 8, 8, 1920),
        (false, 9, 10, 2304),
        (false, 10, 10, 2720),
    ] {
        let total_words = total_bits / word_size;
        let max_data_words = (total_words * 75) / 100;
        let stuffed = aztec_stuff_bits(&bits, word_size);
        if stuffed.len() <= max_data_words.max(1) {
            chosen = Some((compact, layers, word_size, total_words, stuffed));
            break;
        }
    }
    let (compact, layers, word_size, total_words, data_words) = chosen.ok_or_else(|| {
        ProdError::BarcodeError("Aztec payload exceeds maximum 10-layer capacity".into())
    })?;

    let ecc_words = total_words - data_words.len();
    let ecc = aztec_rs_words(&data_words, ecc_words, word_size);
    let mut all_words = data_words.clone();
    all_words.extend_from_slice(&ecc);

    let mut total_bitstream = Vec::with_capacity(all_words.len() * word_size);
    for w in all_words {
        for s in (0..word_size).rev() {
            total_bitstream.push(((w >> s) & 1) != 0);
        }
    }

    let core_half = if compact { 5isize } else { 7isize };
    let base_size = if compact {
        11 + 4 * layers
    } else {
        15 + 4 * layers + 2 * (layers / 4)
    };
    let center = (base_size / 2) as isize;
    let mut grid = vec![false; base_size * base_size];

    let set_pt = |grid: &mut [bool], x: isize, y: isize, v: bool| {
        if x >= 0 && y >= 0 && (x as usize) < base_size && (y as usize) < base_size {
            grid[(y as usize) * base_size + (x as usize)] = v;
        }
    };

    // Bullseye alternating square rings
    for r in 0..=core_half {
        let dark = r % 2 == 0;
        for d in -r..=r {
            set_pt(&mut grid, center + d, center - r, dark);
            set_pt(&mut grid, center + d, center + r, dark);
            set_pt(&mut grid, center - r, center + d, dark);
            set_pt(&mut grid, center + r, center + d, dark);
        }
    }
    // Orientation corner marks around bullseye
    let c = core_half + 1;
    set_pt(&mut grid, center - c, center - c, true);
    set_pt(&mut grid, center - c + 1, center - c, true);
    set_pt(&mut grid, center - c, center - c + 1, true);
    set_pt(&mut grid, center + c, center - c, true);
    set_pt(&mut grid, center + c, center - c + 1, true);
    set_pt(&mut grid, center + c, center + c - 1, true);

    // Reference grid lines for full-range Aztec
    if !compact {
        let mut offset = 16isize;
        while center + offset < base_size as isize {
            for i in 0..base_size as isize {
                let dark = (i + center) % 2 == 0;
                set_pt(&mut grid, center + offset, i, dark);
                set_pt(&mut grid, center - offset, i, dark);
                set_pt(&mut grid, i, center + offset, dark);
                set_pt(&mut grid, i, center - offset, dark);
            }
            offset += 16;
        }
    }

    // Mode message encoded with GF(16) Reed-Solomon
    let mode_words = if compact {
        let mut mw = vec![
            ((layers - 1) as u16) << 2 | (((data_words.len() - 1) as u16) >> 4),
            ((data_words.len() - 1) as u16) & 0xF,
        ];
        mw.extend(aztec_rs_words(&mw, 5, 4));
        mw
    } else {
        let l = (layers - 1) as u16;
        let d = (data_words.len() - 1) as u16;
        let mut mw = vec![l >> 1, ((l & 1) << 3) | (d >> 8), (d >> 4) & 0xF, d & 0xF];
        mw.extend(aztec_rs_words(&mw, 6, 4));
        mw
    };
    let mut mode_bits = Vec::with_capacity(mode_words.len() * 4);
    for w in mode_words {
        for s in (0..4).rev() {
            mode_bits.push(((w >> s) & 1) != 0);
        }
    }
    let m_span = if compact { 7isize } else { 11isize };
    let half_span = m_span / 2;
    let m_ring = core_half + 1;
    let mut mb_idx = 0usize;
    for side_idx in 0..4 {
        for k in 0..m_span {
            if !compact && (k - half_span) == 0 {
                continue;
            }
            let b = mode_bits.get(mb_idx).copied().unwrap_or(false);
            mb_idx += 1;
            let (x, y) = match side_idx {
                0 => (center - half_span + k, center - m_ring),
                1 => (center + m_ring, center - half_span + k),
                2 => (center + half_span - k, center + m_ring),
                _ => (center - m_ring, center + half_span - k),
            };
            set_pt(&mut grid, x, y, b);
        }
    }

    // Spiral placement of data + ECC bits outward across layers
    let mut bit_cursor = 0usize;
    for layer in 0..layers {
        let base_r = core_half + 2 + (layer as isize) * 2;
        let side_len = base_r * 2 + 1;
        for step in 0..4 * side_len {
            let side_id = step / side_len;
            let k = step % side_len;
            for band in 0..2isize {
                let (x, y) = match side_id {
                    0 => (center - base_r + k, center - base_r - band),
                    1 => (center + base_r + band, center - base_r + k),
                    2 => (center + base_r - k, center + base_r + band),
                    _ => (center - base_r - band, center + base_r - k),
                };
                if !compact && ((x - center) % 16 == 0 || (y - center) % 16 == 0) {
                    continue;
                }
                if bit_cursor < total_bitstream.len() {
                    set_pt(&mut grid, x, y, total_bitstream[bit_cursor]);
                    bit_cursor += 1;
                }
            }
        }
    }

    Ok((data.to_owned(), grid, base_size, base_size, 1.0))
}

// --- Code 39 (ISO/IEC 16388) ---

const CODE39_TABLE: [(char, &str); 44] = [
    ('0', "nnnwwnwnn"),
    ('1', "wnnwnnnnw"),
    ('2', "nnwwnnnnw"),
    ('3', "wnwwnnnnn"),
    ('4', "nnnwwnnnw"),
    ('5', "wnnwwnnnn"),
    ('6', "nnwwwnnnn"),
    ('7', "nnnwnnwnw"),
    ('8', "wnnwnnwnn"),
    ('9', "nnwwnnwnn"),
    ('A', "wnnnnwnnw"),
    ('B', "nnwnnwnnw"),
    ('C', "wnwnnwnnn"),
    ('D', "nnnnwwnnw"),
    ('E', "wnnnwwnnn"),
    ('F', "nnwnwwnnn"),
    ('G', "nnnnnwwnw"),
    ('H', "wnnnnwwnn"),
    ('I', "nnwnnwwnn"),
    ('J', "nnnnwwwnn"),
    ('K', "wnnnnnnww"),
    ('L', "nnwnnnnww"),
    ('M', "wnwnnnnwn"),
    ('N', "nnnnwnnww"),
    ('O', "wnnnwnnwn"),
    ('P', "nnwnwnnwn"),
    ('Q', "nnnnnnwww"),
    ('R', "wnnnnnwwn"),
    ('S', "nnwnnnwwn"),
    ('T', "nnnnwnwwn"),
    ('U', "wwnnnnnnw"),
    ('V', "nwwnnnnnw"),
    ('W', "wwwnnnnnn"),
    ('X', "nwnnwnnnw"),
    ('Y', "wwnnwnnnn"),
    ('Z', "nwwnwnnnn"),
    ('-', "nwnnnnwnw"),
    ('.', "wwnnnnwnn"),
    (' ', "nwwnnnwnn"),
    ('*', "nwnnwnwnn"),
    ('$', "nwnwnwnnn"),
    ('/', "nwnwnnnwn"),
    ('+', "nwnnnwnwn"),
    ('%', "nnnwnwnwn"),
];

fn lookup_code39(ch: char) -> Option<&'static str> {
    CODE39_TABLE
        .iter()
        .find_map(|&(c, pat)| (c == ch).then_some(pat))
}

fn encode_code39(data: &str) -> ProdResult<(String, Vec<bool>, Vec<usize>)> {
    let core = data.trim().trim_matches('*').to_ascii_uppercase();
    if core.is_empty() {
        return Err(ProdError::BarcodeError(
            "Code39 payload cannot be empty".into(),
        ));
    }
    for ch in core.chars() {
        if ch == '*' || lookup_code39(ch).is_none() {
            return Err(ProdError::BarcodeError(format!(
                "Code39 unsupported character '{ch}'"
            )));
        }
    }

    let full_seq = format!("*{core}*");
    let mut modules = Vec::with_capacity(full_seq.len() * 16);
    for (char_idx, ch) in full_seq.chars().enumerate() {
        if char_idx > 0 {
            // Narrow inter-character gap (1 module light space)
            modules.push(false);
        }
        let pat = lookup_code39(ch).expect("validated above");
        for (elem_idx, b) in pat.bytes().enumerate() {
            let is_bar = elem_idx % 2 == 0;
            let width = if b == b'w' { 3 } else { 1 };
            for _ in 0..width {
                modules.push(is_bar);
            }
        }
    }

    Ok((full_seq, modules, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn code128_and_code39_tables_satisfy_iso_invariants() {
        let mut seen_128 = HashSet::new();
        for (idx, &pat) in CODE128_PATTERNS.iter().enumerate() {
            assert!(seen_128.insert(pat), "duplicate Code128 pattern at {idx}");
            let widths: Vec<u32> = pat.bytes().map(|b| u32::from(b - b'0')).collect();
            let total: u32 = widths.iter().sum();
            let bar_sum: u32 = widths.iter().step_by(2).sum();
            if idx < 106 {
                assert_eq!(widths.len(), 6, "Code128[{idx}] wrong len");
                assert_eq!(total, 11, "Code128[{idx}] must sum to 11");
                assert_eq!(bar_sum % 2, 0, "Code128[{idx}] must have even bar parity");
            } else {
                assert_eq!(widths.len(), 7);
                assert_eq!(total, 13);
            }
        }

        let mut seen_39 = HashSet::new();
        for &(ch, pat) in &CODE39_TABLE {
            assert_eq!(pat.len(), 9, "Code39 '{ch}' len");
            assert_eq!(
                pat.bytes().filter(|&b| b == b'w').count(),
                3,
                "Code39 '{ch}' must have 3 wide elements"
            );
            assert!(seen_39.insert(pat), "duplicate Code39 pattern for '{ch}'");
        }
    }

    #[test]
    fn qr_code_generates_crisp_welded_vector_geometry_at_exact_size() {
        let opts = QrCodeOptions {
            size_mm: 30.0,
            quiet_zone_modules: 4,
            ec_level: QrErrorCorrection::H,
            weld_modules: true,
            ..QrCodeOptions::default()
        };
        let qr = generate_qr_code("https://github.com/ahrrfy/coree", &opts).unwrap();
        assert!(qr.module_count >= 21);
        assert_eq!(qr.bounds.width(), 30.0);
        assert_eq!(qr.bounds.height(), 30.0);
        assert!(!qr.dark_modules_path.is_empty());

        let bb = qr.dark_modules_path.bounding_box().unwrap();
        let qz_mm = 4.0 * qr.module_size_mm;
        assert!((bb.min_x - qz_mm).abs() < 0.05);
        assert!((bb.min_y - qz_mm).abs() < 0.05);
        assert!((bb.max_x - (30.0 - qz_mm)).abs() < 0.05);

        let mut doc = Document::new_a4("QR Doc");
        let grp_id = add_qr_code_to_document(
            &mut doc,
            "مرحباً كوري",
            Point2D::new(20.0, 20.0),
            &opts,
            None,
        )
        .unwrap();
        let doc_bb = doc.object_bounds(grp_id).unwrap();
        assert!((doc_bb.width() - 30.0).abs() < 0.05);
        assert!((doc_bb.min_x - 20.0).abs() < 0.05);
    }

    #[test]
    fn all_1d_barcodes_encode_valid_vector_geometry_and_checksums() {
        let opts = BarcodeOptions::default();

        // EAN-13: 400638133393 -> known check digit is 1 ("4006381333931")
        assert_eq!(ean13_check_digit("400638133393").unwrap(), 1);
        let ean13 = generate_barcode(BarcodeSymbology::Ean13, "400638133393", &opts).unwrap();
        assert_eq!(ean13.encoded_text, "4006381333931");
        assert_eq!(ean13.modules.len(), 95);
        // Verify exact ISO/GS1 bit string for "4006381333931" (leading '4' => parity LGLLGG):
        // start(101) + 0L(0001101) + 0G(0100111) + 6L(0101111) + 3L(0111101) + 8G(0001001) + 1G(0110011)
        // + center(01010) + 3R(1000010)*3 + 9R(1110100) + 3R(1000010) + 1R(1100110) + end(101)
        let bit_str: String = ean13
            .modules
            .iter()
            .map(|&b| if b { '1' } else { '0' })
            .collect();
        assert_eq!(
            bit_str,
            "10100011010100111010111101111010001001011001101010100001010000101000010111010010000101100110101"
        );
        assert!(!ean13.bars_path.is_empty());
        // Invalid checksum must be rejected
        assert!(generate_barcode(BarcodeSymbology::Ean13, "4006381333939", &opts).is_err());

        // Test all 4 QR EC levels (L, M, Q, H) both welded and unwelded
        for ec in [
            QrErrorCorrection::L,
            QrErrorCorrection::M,
            QrErrorCorrection::Q,
            QrErrorCorrection::H,
        ] {
            for weld in [true, false] {
                let q = generate_qr_code(
                    "coree-prod",
                    &QrCodeOptions {
                        ec_level: ec,
                        weld_modules: weld,
                        ..QrCodeOptions::default()
                    },
                )
                .unwrap();
                assert!(!q.dark_modules_path.is_empty());
            }
        }

        // EAN-8: 9638507 -> known check digit is 4 ("96385074")
        assert_eq!(ean8_check_digit("9638507").unwrap(), 4);
        let ean8 = generate_barcode(BarcodeSymbology::Ean8, "9638507", &opts).unwrap();
        assert_eq!(ean8.encoded_text, "96385074");
        assert_eq!(ean8.modules.len(), 67);
        assert!(generate_barcode(BarcodeSymbology::Ean8, "96385070", &opts).is_err());

        // UPC-A: 03600029145 -> known check digit is 2 ("036000291452")
        assert_eq!(upca_check_digit("03600029145").unwrap(), 2);
        let upca = generate_barcode(BarcodeSymbology::UpcA, "03600029145", &opts).unwrap();
        assert_eq!(upca.encoded_text, "036000291452");
        assert_eq!(upca.modules.len(), 95);
        assert!(generate_barcode(BarcodeSymbology::UpcA, "036000291450", &opts).is_err());

        // Code128 Subset B
        let c128 = generate_barcode(BarcodeSymbology::Code128, "COREE-2026", &opts).unwrap();
        // Start(11) + 10*11 + Check(11) + Stop(13) = 145 modules
        assert_eq!(c128.modules.len(), 145);
        assert!(!c128.bars_path.is_empty());

        // Code39
        let c39 = generate_barcode(BarcodeSymbology::Code39, "AB-123", &opts).unwrap();
        assert_eq!(c39.encoded_text, "*AB-123*");
        // 8 chars * 15 modules (6*1 + 3*3 = 15) + 7 inter-char spaces = 127 modules
        assert_eq!(c39.modules.len(), 127);
        assert!(!c39.bars_path.is_empty());

        // Add barcode into document
        let mut doc = Document::new_a4("Label");
        let id = add_barcode_to_document(
            &mut doc,
            BarcodeSymbology::Ean13,
            "400638133393",
            Point2D::new(15.0, 40.0),
            &opts,
            None,
        )
        .unwrap();
        assert!(doc.objects()[&id].name.contains("4006381333931"));
        assert!(doc.object_bounds(id).unwrap().width() > 30.0);
    }

    #[test]
    fn data_matrix_ecc200_generates_valid_matrix_and_finder_pattern() {
        let opts = BarcodeOptions {
            module_width_mm: 0.5,
            quiet_zone_modules: 2,
            ..BarcodeOptions::default()
        };
        let dm = generate_barcode(BarcodeSymbology::DataMatrix, "COREE-DM-2026", &opts).unwrap();
        assert_eq!(dm.matrix_cols, dm.matrix_rows);
        assert!(dm.matrix_cols >= 14);
        assert_eq!(dm.modules.len(), dm.matrix_cols * dm.matrix_rows);
        // Bottom row and left column must be solid L-finder dark modules
        let s = dm.matrix_cols;
        for i in 0..s {
            assert!(dm.modules[(s - 1) * s + i], "bottom finder col {i}");
            assert!(dm.modules[i * s], "left finder row {i}");
        }
        // Top clock track alternates dark/light
        for (i, &mod_val) in dm.modules.iter().enumerate().take(s) {
            assert_eq!(mod_val, i % 2 == 0, "top clock col {i}");
        }
        assert!(!dm.bars_path.is_empty());
    }

    #[test]
    fn pdf417_generates_multi_row_stacked_matrix_with_start_stop() {
        let opts = BarcodeOptions {
            module_width_mm: 0.33,
            quiet_zone_modules: 2,
            ..BarcodeOptions::default()
        };
        let pdf =
            generate_barcode(BarcodeSymbology::Pdf417, "COREE-PDF417-PACK-01", &opts).unwrap();
        assert!(pdf.matrix_rows >= 3);
        assert!(pdf.matrix_cols >= 17 * 4 + 18);
        assert_eq!(pdf.modules.len(), pdf.matrix_rows * pdf.matrix_cols);
        // First 8 modules of row 0 must be dark (PDF417 start bar width 8)
        assert!(pdf.modules[0..8].iter().all(|&m| m));
        assert!(!pdf.modules[8]);
        assert!(!pdf.bars_path.is_empty());
    }

    #[test]
    fn aztec_generates_symmetric_bullseye_core_and_layers() {
        let opts = BarcodeOptions {
            module_width_mm: 0.5,
            quiet_zone_modules: 2,
            ..BarcodeOptions::default()
        };
        let az = generate_barcode(BarcodeSymbology::Aztec, "COREE-AZTEC-2026", &opts).unwrap();
        assert_eq!(az.matrix_cols, az.matrix_rows);
        assert!(az.matrix_cols >= 15);
        let center = az.matrix_cols / 2;
        // Bullseye center pixel must be dark and ring r=1 light
        assert!(az.modules[center * az.matrix_cols + center]);
        assert!(!az.modules[(center - 1) * az.matrix_cols + center]);
        assert!(az.modules[(center - 2) * az.matrix_cols + center]);
        assert!(!az.bars_path.is_empty());
    }

    #[test]
    fn gs1_128_and_code128_modes_and_bwr_work_end_to_end() {
        let opts = BarcodeOptions {
            module_width_mm: 0.4,
            bar_width_reduction_mm: 0.04,
            code128_mode: Code128Mode::SubsetC,
            ..BarcodeOptions::default()
        };
        let c128_c = generate_barcode(BarcodeSymbology::Code128, "1234567890", &opts).unwrap();
        // Subset C packs 10 digits into 5 symbols: Start(11) + 5*11 + Check(11) + Stop(13) = 90 modules
        assert_eq!(c128_c.modules.len(), 90);

        // Verify BWR narrows the narrowest bars by bar_width_reduction_mm
        let min_bar_w = c128_c
            .bars_path
            .subpaths
            .iter()
            .filter_map(|sp| {
                BezierPath::from_subpath(sp.clone())
                    .bounding_box()
                    .map(|bb| bb.width())
            })
            .fold(f64::INFINITY, f64::min);
        assert!((min_bar_w - (0.4 - 0.04)).abs() < 1e-6);

        let gs1 = generate_barcode(
            BarcodeSymbology::Gs1128,
            "(01)04006381333931(10)LOT42(21)SER99",
            &BarcodeOptions {
                code128_mode: Code128Mode::Auto,
                ..BarcodeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(gs1.encoded_text, "(01)04006381333931(10)LOT42(21)SER99");
        assert!(!gs1.bars_path.is_empty());
        // Starts with START_C (105 -> "211232") or START_B + FNC1 (102 -> "411131")
        let fnc1_offset = 11usize;
        let fnc1_bits: String = gs1.modules[fnc1_offset..fnc1_offset + 11]
            .iter()
            .map(|&b| if b { '1' } else { '0' })
            .collect();
        // FNC1 pattern "411131" => 11110101110
        assert_eq!(fnc1_bits, "11110101110");
    }
}
