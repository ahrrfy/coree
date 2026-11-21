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
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::Overlay;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::i_shape::int::shape::IntContour;
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

/// Maximum safe integer coordinate magnitude for `i_overlay`'s `i64` sweep engine.
const MAX_SAFE_INT_COORD: f64 = 1_000_000_000_000_000.0; // 1e15 << i64::MAX

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

    // Merge contiguous dark modules horizontally per row first
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

    let dark_modules_path = if options.weld_modules && !rects.is_empty() {
        weld_rects_single_pass(&rects)
    } else {
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

/// Welds many axis-aligned rectangles into minimal clean vector contours in a single
/// `i_overlay` integer sweep with dynamic scale clamping so `i64` conversion can never overflow.
fn weld_rects_single_pass(rects: &[Rect2D]) -> BezierPath {
    const DEFAULT_SCALE: f64 = 10_000.0;
    let max_abs = rects
        .iter()
        .flat_map(|r| [r.min_x.abs(), r.min_y.abs(), r.max_x.abs(), r.max_y.abs()])
        .filter(|v| v.is_finite())
        .fold(1.0_f64, f64::max);
    let scale = if max_abs * DEFAULT_SCALE <= MAX_SAFE_INT_COORD {
        DEFAULT_SCALE
    } else {
        (MAX_SAFE_INT_COORD / max_abs).max(1e-6)
    };
    let inv = 1.0 / scale;
    let contours: Vec<IntContour<i64>> = rects
        .iter()
        .filter(|r| r.is_finite())
        .map(|r| {
            let x0 = (r.min_x * scale)
                .round()
                .clamp(-MAX_SAFE_INT_COORD, MAX_SAFE_INT_COORD) as i64;
            let y0 = (r.min_y * scale)
                .round()
                .clamp(-MAX_SAFE_INT_COORD, MAX_SAFE_INT_COORD) as i64;
            let x1 = (r.max_x * scale)
                .round()
                .clamp(-MAX_SAFE_INT_COORD, MAX_SAFE_INT_COORD) as i64;
            let y1 = (r.max_y * scale)
                .round()
                .clamp(-MAX_SAFE_INT_COORD, MAX_SAFE_INT_COORD) as i64;
            vec![
                IntPoint::new(x0, y0),
                IntPoint::new(x1, y0),
                IntPoint::new(x1, y1),
                IntPoint::new(x0, y1),
            ]
        })
        .collect();

    let empty_clip: &[IntContour<i64>] = &[];
    let shapes = Overlay::<i64>::from_subj_and_clip(&contours, empty_clip)
        .overlay(OverlayRule::Subject, FillRule::NonZero);

    let mut subpaths = Vec::new();
    for shape in shapes {
        for contour in shape {
            if contour.len() >= 3 {
                let pts = contour
                    .into_iter()
                    .map(|p| Point2D::new((p.x as f64) * inv, (p.y as f64) * inv));
                subpaths.push(Subpath::from_points(pts, true));
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
    if let Some(grp) = doc.objects.get_mut(&group_id) {
        grp.transform = Affine2D::translate(origin.x, origin.y);
    }
    Ok(group_id)
}

// =============================================================================
// 1D Vector Barcode Encoders (EAN-13, EAN-8, UPC-A, Code128-B, Code39)
// =============================================================================

/// Supported 1D linear barcode symbologies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BarcodeSymbology {
    Ean13,
    Ean8,
    UpcA,
    Code128,
    Code39,
}

impl BarcodeSymbology {
    /// Parses a symbology token (case-insensitive, e.g. `"EAN13"`, `"EAN-13"`, `"Code128"`).
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
            _ => None,
        }
    }
}

/// Configuration for 1D vector barcode generation.
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
        }
    }
}

/// Generated crisp vector 1D barcode output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarcodeOutput {
    pub symbology: BarcodeSymbology,
    /// Canonical encoded payload (including computed check digit for EAN/UPC).
    pub encoded_text: String,
    /// Encoded binary module sequence (`true` = dark bar, `false` = light space), excluding quiet zones.
    pub modules: Vec<bool>,
    /// Vector compound path of all dark bar rectangles in millimeters.
    pub bars_path: BezierPath,
    /// Total physical bounding box including quiet zones and optional human-readable text space.
    pub bounds: Rect2D,
    /// Optional human-readable text label.
    pub human_readable_text: Option<String>,
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

    let (encoded_text, modules, guard_indices) = match symbology {
        BarcodeSymbology::Ean13 => encode_ean13(data)?,
        BarcodeSymbology::Ean8 => encode_ean8(data)?,
        BarcodeSymbology::UpcA => encode_upca(data)?,
        BarcodeSymbology::Code128 => encode_code128_b(data)?,
        BarcodeSymbology::Code39 => encode_code39(data)?,
    };

    let qz_mm = f64::from(options.quiet_zone_modules) * options.module_width_mm;
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
            let x = qz_mm + (start as f64) * options.module_width_mm;
            let w = ((idx - start) as f64) * options.module_width_mm;
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
        let font_pt = ((out.bounds.height() - options.bar_height_mm) * 2.4).clamp(6.0, 12.0);
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
        .with_transform(Affine2D::translate(
            out.bounds.width() * 0.15,
            options.bar_height_mm + 0.5,
        ))
        .with_outline(OutlineStyle::none());
        children.push(doc.add_object(text_obj, layer_id)?);
    }

    let group_id = doc.group_objects(
        &children,
        format!("Barcode:{symbology:?}:{}", out.encoded_text),
    )?;
    if let Some(grp) = doc.objects.get_mut(&group_id) {
        grp.transform = Affine2D::translate(origin.x, origin.y);
    }
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

fn encode_code128_b(data: &str) -> ProdResult<(String, Vec<bool>, Vec<usize>)> {
    if data.is_empty() {
        return Err(ProdError::BarcodeError(
            "Code128 payload cannot be empty".into(),
        ));
    }
    let mut values = Vec::with_capacity(data.len());
    for ch in data.chars() {
        let cp = ch as u32;
        if !(32..=126).contains(&cp) {
            return Err(ProdError::BarcodeError(format!(
                "Code128 Subset B only supports printable ASCII (32..=126), invalid char '{ch}'"
            )));
        }
        values.push((cp - 32) as usize);
    }

    const START_B: usize = 104;
    const STOP: usize = 106;

    let mut checksum = START_B;
    for (i, &val) in values.iter().enumerate() {
        checksum += (i + 1) * val;
    }
    let check_val = checksum % 103;

    let mut modules = Vec::with_capacity((values.len() + 2) * 11 + 13);
    append_code128_symbol(&mut modules, START_B);
    for val in values {
        append_code128_symbol(&mut modules, val);
    }
    append_code128_symbol(&mut modules, check_val);
    append_code128_symbol(&mut modules, STOP);

    Ok((data.to_owned(), modules, Vec::new()))
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
        assert!(doc.objects[&id].name.contains("4006381333931"));
        assert!(doc.object_bounds(id).unwrap().width() > 30.0);
    }
}
