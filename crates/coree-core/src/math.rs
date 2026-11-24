// SPDX-License-Identifier: MIT OR Apache-2.0

//! Units, canonical millimeter 2D primitives, bounding boxes, and affine transforms.
//!
//! All internal coordinates in `coree-core` are represented in **millimeters (`f64`)**
//! using a right-handed Y-down canvas space (origin at top-left of the active page),
//! with helpers provided for converting to and from Y-up / page-centered formats.

use kurbo::ParamCurve;
use serde::{Deserialize, Serialize};
use std::ops::{Add, Div, Mul, Neg, Sub};

/// Millimeters per inch (exact international definition).
pub const MM_PER_INCH: f64 = 25.4;

/// Typographic points per inch (exact PostScript / DTP definition).
pub const POINTS_PER_INCH: f64 = 72.0;

/// Picas per inch (exact: 6 picas = 1 inch, 12 points = 1 pica).
pub const PICAS_PER_INCH: f64 = 6.0;

/// Number of 0.1-micrometer (tenth-micron) CDR internal units per millimeter.
pub const CDR_UNITS_PER_MM: f64 = 10_000.0;

/// Default screen DPI used when a caller provides an invalid/non-positive DPI.
pub const DEFAULT_DPI: f64 = 96.0;

/// Replaces non-finite `f64` values (`NaN`, `±Inf`) with `fallback`.
#[inline]
#[must_use]
pub fn sanitize_f64(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// Replaces non-finite `f32` values (`NaN`, `±Inf`) with `fallback`.
#[inline]
#[must_use]
pub fn sanitize_f32(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// Measurement units supported throughout the document and UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Unit {
    #[default]
    Millimeter,
    Centimeter,
    Meter,
    Inch,
    Point,
    Pica,
    Pixel,
    /// Legacy `.cdr` internal coordinate unit (`1 mm = 10_000.0` units = `0.1 µm`).
    CdrTenthMicron,
}

impl Unit {
    /// Converts a scalar value in `self` units into canonical millimeters (`mm`).
    ///
    /// For [`Unit::Pixel`], `dpi` controls pixels per inch; non-positive or non-finite
    /// DPI values safely fall back to [`DEFAULT_DPI`] (`96.0`).
    #[must_use]
    pub fn to_mm(self, value: f64, dpi: f64) -> f64 {
        if !value.is_finite() {
            return 0.0;
        }
        let safe_dpi = if dpi.is_finite() && dpi > 0.0 {
            dpi
        } else {
            DEFAULT_DPI
        };
        match self {
            Self::Millimeter => value,
            Self::Centimeter => value * 10.0,
            Self::Meter => value * 1_000.0,
            Self::Inch => value * MM_PER_INCH,
            Self::Point => value * (MM_PER_INCH / POINTS_PER_INCH),
            Self::Pica => value * (MM_PER_INCH / PICAS_PER_INCH),
            Self::Pixel => value * (MM_PER_INCH / safe_dpi),
            Self::CdrTenthMicron => value / CDR_UNITS_PER_MM,
        }
    }

    /// Converts a canonical millimeter (`mm`) scalar value into `self` units.
    #[must_use]
    pub fn from_mm(self, mm: f64, dpi: f64) -> f64 {
        if !mm.is_finite() {
            return 0.0;
        }
        let safe_dpi = if dpi.is_finite() && dpi > 0.0 {
            dpi
        } else {
            DEFAULT_DPI
        };
        match self {
            Self::Millimeter => mm,
            Self::Centimeter => mm / 10.0,
            Self::Meter => mm / 1_000.0,
            Self::Inch => mm / MM_PER_INCH,
            Self::Point => mm * (POINTS_PER_INCH / MM_PER_INCH),
            Self::Pica => mm * (PICAS_PER_INCH / MM_PER_INCH),
            Self::Pixel => mm * (safe_dpi / MM_PER_INCH),
            Self::CdrTenthMicron => mm * CDR_UNITS_PER_MM,
        }
    }

    /// Converts a scalar directly between two units at the given `dpi`.
    #[must_use]
    pub fn convert(value: f64, from: Self, to: Self, dpi: f64) -> f64 {
        if from == to {
            return sanitize_f64(value, 0.0);
        }
        to.from_mm(from.to_mm(value, dpi), dpi)
    }

    /// Short display suffix for this unit.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Millimeter => "mm",
            Self::Centimeter => "cm",
            Self::Meter => "m",
            Self::Inch => "in",
            Self::Point => "pt",
            Self::Pica => "pc",
            Self::Pixel => "px",
            Self::CdrTenthMicron => "0.1µm",
        }
    }

    /// Formats a millimeter value in this unit with the requested decimal places.
    #[must_use]
    pub fn format_mm(self, mm: f64, dpi: f64, decimals: usize) -> String {
        let converted = self.from_mm(mm, dpi);
        format!("{converted:.decimals$} {}", self.suffix())
    }
}

/// A 2D point in canonical millimeter coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point2D {
    pub x: f64,
    pub y: f64,
}

impl Point2D {
    /// Origin `(0.0, 0.0)`.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// Creates a new point.
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Returns `true` if both coordinates are finite.
    #[inline]
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    /// Replaces non-finite coordinates with `0.0`.
    #[inline]
    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            x: sanitize_f64(self.x, 0.0),
            y: sanitize_f64(self.y, 0.0),
        }
    }

    /// Euclidean distance to `other`.
    #[inline]
    #[must_use]
    pub fn distance_to(self, other: Self) -> f64 {
        (other.x - self.x).hypot(other.y - self.y)
    }

    /// Squared Euclidean distance to `other`.
    #[inline]
    #[must_use]
    pub fn distance_sq(self, other: Self) -> f64 {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        dx * dx + dy * dy
    }

    /// Linearly interpolates between `self` (`t = 0`) and `other` (`t = 1`).
    #[inline]
    #[must_use]
    pub fn lerp(self, other: Self, t: f64) -> Self {
        Self {
            x: self.x + (other.x - self.x) * t,
            y: self.y + (other.y - self.y) * t,
        }
    }

    /// Midpoint between `self` and `other`.
    #[inline]
    #[must_use]
    pub fn midpoint(self, other: Self) -> Self {
        self.lerp(other, 0.5)
    }

    /// Translates the point by `(dx, dy)`.
    #[inline]
    #[must_use]
    pub fn offset(self, dx: f64, dy: f64) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }

    /// Converts this point into a position vector relative to the origin.
    #[inline]
    #[must_use]
    pub const fn to_vec(self) -> Vec2D {
        Vec2D {
            x: self.x,
            y: self.y,
        }
    }

    /// Converts to a `kurbo::Point`.
    #[inline]
    #[must_use]
    pub const fn to_kurbo(self) -> kurbo::Point {
        kurbo::Point::new(self.x, self.y)
    }

    /// Creates from a `kurbo::Point`.
    #[inline]
    #[must_use]
    pub const fn from_kurbo(p: kurbo::Point) -> Self {
        Self { x: p.x, y: p.y }
    }

    /// Converts page-local coordinates to canvas coordinates given a page origin.
    #[inline]
    #[must_use]
    pub fn page_to_canvas(self, page_origin: Self) -> Self {
        Self {
            x: self.x + page_origin.x,
            y: self.y + page_origin.y,
        }
    }

    /// Converts canvas coordinates to page-local coordinates given a page origin.
    #[inline]
    #[must_use]
    pub fn canvas_to_page(self, page_origin: Self) -> Self {
        Self {
            x: self.x - page_origin.x,
            y: self.y - page_origin.y,
        }
    }

    /// Converts between Y-up (e.g. PDF / legacy CDR) and Y-down page coordinates.
    #[inline]
    #[must_use]
    pub fn flip_y_in_page(self, page_height_mm: f64) -> Self {
        Self {
            x: self.x,
            y: page_height_mm - self.y,
        }
    }

    /// Converts page-centered Y-up coordinates (origin at page center) into
    /// top-left Y-down page coordinates.
    #[inline]
    #[must_use]
    pub fn from_centered_y_up(self, page_width_mm: f64, page_height_mm: f64) -> Self {
        Self {
            x: self.x + page_width_mm * 0.5,
            y: page_height_mm * 0.5 - self.y,
        }
    }
}

impl From<(f64, f64)> for Point2D {
    fn from((x, y): (f64, f64)) -> Self {
        Self { x, y }
    }
}

impl From<[f64; 2]> for Point2D {
    fn from([x, y]: [f64; 2]) -> Self {
        Self { x, y }
    }
}

impl Add<Vec2D> for Point2D {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Vec2D) -> Self {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl Sub<Self> for Point2D {
    type Output = Vec2D;

    #[inline]
    fn sub(self, rhs: Self) -> Vec2D {
        Vec2D {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl Sub<Vec2D> for Point2D {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Vec2D) -> Self {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

/// A 2D displacement or direction vector in millimeters.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Vec2D {
    pub x: f64,
    pub y: f64,
}

impl Vec2D {
    /// Zero vector `(0.0, 0.0)`.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// Creates a new vector.
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Creates a unit vector pointing at `angle_rad` radians from `+X`.
    #[inline]
    #[must_use]
    pub fn from_angle(angle_rad: f64) -> Self {
        Self {
            x: angle_rad.cos(),
            y: angle_rad.sin(),
        }
    }

    /// Returns `true` if both components are finite.
    #[inline]
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    /// Euclidean norm (length).
    #[inline]
    #[must_use]
    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    /// Squared Euclidean norm.
    #[inline]
    #[must_use]
    pub fn length_sq(self) -> f64 {
        self.x * self.x + self.y * self.y
    }

    /// Returns a unit vector in the same direction, or `None` if zero/non-finite.
    #[inline]
    #[must_use]
    pub fn normalized(self) -> Option<Self> {
        let len = self.length();
        if len.is_finite() && len > 1e-12 {
            Some(Self {
                x: self.x / len,
                y: self.y / len,
            })
        } else {
            None
        }
    }

    /// Returns a unit vector, or [`Vec2D::ZERO`] if degenerate.
    #[inline]
    #[must_use]
    pub fn normalize_or_zero(self) -> Self {
        self.normalized().unwrap_or(Self::ZERO)
    }

    /// Dot product `self · other`.
    #[inline]
    #[must_use]
    pub fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y
    }

    /// 2D scalar cross product (`self.x * other.y - self.y * other.x`).
    #[inline]
    #[must_use]
    pub fn cross(self, other: Self) -> f64 {
        self.x * other.y - self.y * other.x
    }

    /// Returns a perpendicular vector rotated +90° (`(-y, x)`).
    #[inline]
    #[must_use]
    pub const fn perp(self) -> Self {
        Self {
            x: -self.y,
            y: self.x,
        }
    }

    /// Angle in radians (`atan2(y, x)`).
    #[inline]
    #[must_use]
    pub fn angle_rad(self) -> f64 {
        self.y.atan2(self.x)
    }

    /// Rotates the vector counter-clockwise by `angle_rad`.
    #[inline]
    #[must_use]
    pub fn rotate(self, angle_rad: f64) -> Self {
        let (s, c) = angle_rad.sin_cos();
        Self {
            x: self.x * c - self.y * s,
            y: self.x * s + self.y * c,
        }
    }

    /// Converts to a `Point2D`.
    #[inline]
    #[must_use]
    pub const fn to_point(self) -> Point2D {
        Point2D {
            x: self.x,
            y: self.y,
        }
    }

    /// Converts to a `kurbo::Vec2`.
    #[inline]
    #[must_use]
    pub const fn to_kurbo(self) -> kurbo::Vec2 {
        kurbo::Vec2::new(self.x, self.y)
    }

    /// Creates from a `kurbo::Vec2`.
    #[inline]
    #[must_use]
    pub const fn from_kurbo(v: kurbo::Vec2) -> Self {
        Self { x: v.x, y: v.y }
    }
}

impl Add for Vec2D {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl Sub for Vec2D {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl Mul<f64> for Vec2D {
    type Output = Self;

    #[inline]
    fn mul(self, rhs: f64) -> Self {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

impl Mul<Vec2D> for f64 {
    type Output = Vec2D;

    #[inline]
    fn mul(self, rhs: Vec2D) -> Vec2D {
        Vec2D {
            x: self * rhs.x,
            y: self * rhs.y,
        }
    }
}

impl Div<f64> for Vec2D {
    type Output = Self;

    #[inline]
    fn div(self, rhs: f64) -> Self {
        Self {
            x: self.x / rhs,
            y: self.y / rhs,
        }
    }
}

impl Neg for Vec2D {
    type Output = Self;

    #[inline]
    fn neg(self) -> Self {
        Self {
            x: -self.x,
            y: -self.y,
        }
    }
}

/// Axis-aligned 2D bounding rectangle in millimeters.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect2D {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Rect2D {
    /// Creates a rectangle from explicit `min` and `max` bounds, normalizing inverted axes.
    #[must_use]
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            min_x: x0.min(x1),
            min_y: y0.min(y1),
            max_x: x0.max(x1),
            max_y: y0.max(y1),
        }
    }

    /// Creates a rectangle from origin `(x, y)` and `(width, height)`.
    #[must_use]
    pub fn from_xywh(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self::new(x, y, x + width, y + height)
    }

    /// Creates a rectangle spanning two arbitrary points.
    #[must_use]
    pub fn from_points(a: Point2D, b: Point2D) -> Self {
        Self::new(a.x, a.y, b.x, b.y)
    }

    /// Creates a rectangle centered at `center` with dimensions `width` × `height`.
    #[must_use]
    pub fn from_center_size(center: Point2D, width: f64, height: f64) -> Self {
        let hw = width.abs() * 0.5;
        let hh = height.abs() * 0.5;
        Self {
            min_x: center.x - hw,
            min_y: center.y - hh,
            max_x: center.x + hw,
            max_y: center.y + hh,
        }
    }

    /// Computes the bounding box of a point cloud; returns `None` if empty or all non-finite.
    #[must_use]
    pub fn from_iter_points<I: IntoIterator<Item = Point2D>>(iter: I) -> Option<Self> {
        let mut rect: Option<Self> = None;
        for pt in iter {
            if !pt.is_finite() {
                continue;
            }
            rect = Some(match rect {
                None => Self {
                    min_x: pt.x,
                    min_y: pt.y,
                    max_x: pt.x,
                    max_y: pt.y,
                },
                Some(r) => r.union_point(pt),
            });
        }
        rect
    }

    /// Returns `true` if all four bounds are finite.
    #[inline]
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.min_x.is_finite()
            && self.min_y.is_finite()
            && self.max_x.is_finite()
            && self.max_y.is_finite()
    }

    /// Returns `true` if the rectangle has negative width or height, or non-finite bounds.
    #[inline]
    #[must_use]
    pub fn is_empty(self) -> bool {
        !self.is_finite() || self.max_x < self.min_x || self.max_y < self.min_y
    }

    /// Width (`max_x - min_x`, clamped to `>= 0.0`).
    #[inline]
    #[must_use]
    pub fn width(self) -> f64 {
        (self.max_x - self.min_x).max(0.0)
    }

    /// Height (`max_y - min_y`, clamped to `>= 0.0`).
    #[inline]
    #[must_use]
    pub fn height(self) -> f64 {
        (self.max_y - self.min_y).max(0.0)
    }

    /// Geometric center point.
    #[inline]
    #[must_use]
    pub fn center(self) -> Point2D {
        Point2D::new(
            (self.min_x + self.max_x) * 0.5,
            (self.min_y + self.max_y) * 0.5,
        )
    }

    /// Top-left corner `(min_x, min_y)`.
    #[inline]
    #[must_use]
    pub const fn min(self) -> Point2D {
        Point2D::new(self.min_x, self.min_y)
    }

    /// Bottom-right corner `(max_x, max_y)`.
    #[inline]
    #[must_use]
    pub const fn max(self) -> Point2D {
        Point2D::new(self.max_x, self.max_y)
    }

    /// All four corners in clockwise order starting at top-left.
    #[inline]
    #[must_use]
    pub const fn corners(self) -> [Point2D; 4] {
        [
            Point2D::new(self.min_x, self.min_y),
            Point2D::new(self.max_x, self.min_y),
            Point2D::new(self.max_x, self.max_y),
            Point2D::new(self.min_x, self.max_y),
        ]
    }

    /// Smallest rectangle containing both `self` and `other`.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    /// Smallest rectangle containing `self` and `pt`.
    #[must_use]
    pub fn union_point(self, pt: Point2D) -> Self {
        if !pt.is_finite() {
            return self;
        }
        if self.is_empty() {
            return Self {
                min_x: pt.x,
                min_y: pt.y,
                max_x: pt.x,
                max_y: pt.y,
            };
        }
        Self {
            min_x: self.min_x.min(pt.x),
            min_y: self.min_y.min(pt.y),
            max_x: self.max_x.max(pt.x),
            max_y: self.max_y.max(pt.y),
        }
    }

    /// Returns `true` if `self` and `other` overlap (including touching edges).
    #[inline]
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        self.min_x <= other.max_x
            && self.max_x >= other.min_x
            && self.min_y <= other.max_y
            && self.max_y >= other.min_y
    }

    /// Overlapping sub-rectangle, or `None` if disjoint.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        if !self.intersects(other) {
            return None;
        }
        Some(Self {
            min_x: self.min_x.max(other.min_x),
            min_y: self.min_y.max(other.min_y),
            max_x: self.max_x.min(other.max_x),
            max_y: self.max_y.min(other.max_y),
        })
    }

    /// Returns `true` if `pt` lies inside or on the boundary of `self`.
    #[inline]
    #[must_use]
    pub fn contains_point(self, pt: Point2D) -> bool {
        if !pt.is_finite() || self.is_empty() {
            return false;
        }
        pt.x >= self.min_x && pt.x <= self.max_x && pt.y >= self.min_y && pt.y <= self.max_y
    }

    /// Returns `true` if `other` is completely contained within `self`.
    #[inline]
    #[must_use]
    pub fn contains_rect(self, other: Self) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        other.min_x >= self.min_x
            && other.max_x <= self.max_x
            && other.min_y >= self.min_y
            && other.max_y <= self.max_y
    }

    /// Uniformly expands (or shrinks if negative) all four sides by `margin`.
    #[must_use]
    pub fn expand(self, margin: f64) -> Self {
        let m = sanitize_f64(margin, 0.0);
        let min_x = self.min_x - m;
        let min_y = self.min_y - m;
        let max_x = self.max_x + m;
        let max_y = self.max_y + m;
        if min_x > max_x || min_y > max_y {
            let c = self.center();
            Self {
                min_x: c.x,
                min_y: c.y,
                max_x: c.x,
                max_y: c.y,
            }
        } else {
            Self {
                min_x,
                min_y,
                max_x,
                max_y,
            }
        }
    }

    /// Translates the rectangle by `delta`.
    #[must_use]
    pub fn translate(self, delta: Vec2D) -> Self {
        Self {
            min_x: self.min_x + delta.x,
            min_y: self.min_y + delta.y,
            max_x: self.max_x + delta.x,
            max_y: self.max_y + delta.y,
        }
    }

    /// Converts to a `kurbo::Rect`.
    #[inline]
    #[must_use]
    pub const fn to_kurbo(self) -> kurbo::Rect {
        kurbo::Rect::new(self.min_x, self.min_y, self.max_x, self.max_y)
    }

    /// Creates from a `kurbo::Rect`.
    #[inline]
    #[must_use]
    pub fn from_kurbo(r: kurbo::Rect) -> Self {
        Self::new(r.x0, r.y0, r.x1, r.y1)
    }
}

/// Decomposed components of a 2D affine matrix (`translate * rotate * skew_x * scale`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AffineDecomposed {
    pub translation: Vec2D,
    pub rotation_rad: f64,
    pub scale: Vec2D,
    pub skew_x_rad: f64,
}

/// A 2D affine transformation matrix stored as `[a, b, c, d, tx, ty]`:
///
/// ```text
/// | a  c  tx |   | x |   | a*x + c*y + tx |
/// | b  d  ty | * | y | = | b*x + d*y + ty |
/// | 0  0   1 |   | 1 |   |       1        |
/// ```
///
/// This memory layout matches `kurbo::Affine::as_coeffs()` and standard SVG/PDF matrices.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Affine2D {
    pub m: [f64; 6],
}

impl Default for Affine2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine2D {
    /// Identity transformation.
    pub const IDENTITY: Self = Self {
        m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };

    /// Returns the identity transformation.
    #[inline]
    #[must_use]
    pub const fn identity() -> Self {
        Self::IDENTITY
    }

    /// Constructs a matrix directly from `[a, b, c, d, tx, ty]`.
    #[inline]
    #[must_use]
    pub const fn new(a: f64, b: f64, c: f64, d: f64, tx: f64, ty: f64) -> Self {
        Self {
            m: [a, b, c, d, tx, ty],
        }
    }

    /// Returns `true` if every coefficient is finite.
    #[inline]
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.m.iter().all(|v| v.is_finite())
    }

    /// Pure translation by `(tx, ty)`.
    #[inline]
    #[must_use]
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// Non-uniform scaling about the origin.
    #[inline]
    #[must_use]
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Uniform scaling about the origin.
    #[inline]
    #[must_use]
    pub const fn scale_uniform(s: f64) -> Self {
        Self::scale(s, s)
    }

    /// Scaling about an arbitrary `pivot` point.
    #[must_use]
    pub fn scale_about(sx: f64, sy: f64, pivot: Point2D) -> Self {
        Self::translate(pivot.x, pivot.y)
            .compose(&Self::scale(sx, sy))
            .compose(&Self::translate(-pivot.x, -pivot.y))
    }

    /// Rotation by `angle_rad` radians around the origin.
    #[inline]
    #[must_use]
    pub fn rotate(angle_rad: f64) -> Self {
        let (s, c) = angle_rad.sin_cos();
        Self::new(c, s, -s, c, 0.0, 0.0)
    }

    /// Rotation by `angle_deg` degrees around the origin.
    #[inline]
    #[must_use]
    pub fn rotate_deg(angle_deg: f64) -> Self {
        Self::rotate(angle_deg.to_radians())
    }

    /// Rotation by `angle_rad` radians around `pivot`.
    #[must_use]
    pub fn rotate_about(angle_rad: f64, pivot: Point2D) -> Self {
        Self::translate(pivot.x, pivot.y)
            .compose(&Self::rotate(angle_rad))
            .compose(&Self::translate(-pivot.x, -pivot.y))
    }

    /// Shear / skew by `(skew_x_rad, skew_y_rad)` radians.
    #[inline]
    #[must_use]
    pub fn skew(skew_x_rad: f64, skew_y_rad: f64) -> Self {
        Self::new(1.0, skew_y_rad.tan(), skew_x_rad.tan(), 1.0, 0.0, 0.0)
    }

    /// Shear / skew about an arbitrary `pivot` point.
    #[must_use]
    pub fn skew_about(skew_x_rad: f64, skew_y_rad: f64, pivot: Point2D) -> Self {
        Self::translate(pivot.x, pivot.y)
            .compose(&Self::skew(skew_x_rad, skew_y_rad))
            .compose(&Self::translate(-pivot.x, -pivot.y))
    }

    /// Horizontal mirror across the vertical line `x = pivot_x`.
    #[must_use]
    pub fn flip_x(pivot_x: f64) -> Self {
        Self::scale_about(-1.0, 1.0, Point2D::new(pivot_x, 0.0))
    }

    /// Vertical mirror across the horizontal line `y = pivot_y`.
    #[must_use]
    pub fn flip_y(pivot_y: f64) -> Self {
        Self::scale_about(1.0, -1.0, Point2D::new(0.0, pivot_y))
    }

    /// Matrix multiplication `self * other` (applies `other` first, then `self`).
    #[must_use]
    pub fn compose(&self, other: &Self) -> Self {
        let [a1, b1, c1, d1, tx1, ty1] = self.m;
        let [a2, b2, c2, d2, tx2, ty2] = other.m;
        Self::new(
            a1 * a2 + c1 * b2,
            b1 * a2 + d1 * b2,
            a1 * c2 + c1 * d2,
            b1 * c2 + d1 * d2,
            a1 * tx2 + c1 * ty2 + tx1,
            b1 * tx2 + d1 * ty2 + ty1,
        )
    }

    /// Sequential composition: applies `self` first, then `next` (`next * self`).
    #[inline]
    #[must_use]
    pub fn then(&self, next: &Self) -> Self {
        next.compose(self)
    }

    /// Determinant `a*d - b*c` of the linear part.
    #[inline]
    #[must_use]
    pub fn determinant(&self) -> f64 {
        self.m[0] * self.m[3] - self.m[1] * self.m[2]
    }

    /// Computes the exact inverse matrix, or `None` if singular or non-finite.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        if !self.is_finite() {
            return None;
        }
        let det = self.determinant();
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let [a, b, c, d, tx, ty] = self.m;
        let inv = Self::new(
            d * inv_det,
            -b * inv_det,
            -c * inv_det,
            a * inv_det,
            (c * ty - d * tx) * inv_det,
            (b * tx - a * ty) * inv_det,
        );
        inv.is_finite().then_some(inv)
    }

    /// Transforms a 2D point by this affine matrix.
    #[inline]
    #[must_use]
    pub fn transform_point(&self, p: Point2D) -> Point2D {
        let [a, b, c, d, tx, ty] = self.m;
        Point2D::new(a * p.x + c * p.y + tx, b * p.x + d * p.y + ty)
    }

    /// Transforms a direction vector (ignores translation).
    #[inline]
    #[must_use]
    pub fn transform_vec(&self, v: Vec2D) -> Vec2D {
        let [a, b, c, d, _, _] = self.m;
        Vec2D::new(a * v.x + c * v.y, b * v.x + d * v.y)
    }

    /// Returns the axis-aligned bounding box enclosing the transformed rectangle,
    /// or `None` if either the transform or rectangle is non-finite.
    #[must_use]
    pub fn try_transform_rect(&self, rect: Rect2D) -> Option<Rect2D> {
        if !self.is_finite() || !rect.is_finite() {
            return None;
        }
        let corners = rect.corners().map(|c| self.transform_point(c));
        Rect2D::from_iter_points(corners).filter(|r| r.is_finite())
    }

    /// Returns the axis-aligned bounding box enclosing the transformed rectangle
    /// (falls back to an empty zero rect if inputs are non-finite).
    #[must_use]
    pub fn transform_rect(&self, rect: Rect2D) -> Rect2D {
        self.try_transform_rect(rect).unwrap_or_default()
    }

    /// QR-style decomposition into translation, rotation, skew-X, and scale so that
    /// `translate * rotate * skew(skew_x, 0) * scale` reconstructs `self`.
    #[must_use]
    pub fn decompose(&self) -> AffineDecomposed {
        let [a, b, c, d, tx, ty] = self.m;
        let translation = Vec2D::new(tx, ty);
        let det = a * d - b * c;
        let sx_mag = a.hypot(b);
        if sx_mag > 1e-12 {
            let rotation_rad = b.atan2(a);
            let sign_y = if det < 0.0 { -1.0 } else { 1.0 };
            let sy = sign_y * (det.abs() / sx_mag);
            let dot = a * c + b * d;
            let tan_skew = if det.abs() > 1e-12 {
                (dot / (sx_mag * sx_mag)) * (sx_mag / sy)
            } else {
                0.0
            };
            let skew_x_rad = tan_skew.atan();
            AffineDecomposed {
                translation,
                rotation_rad,
                scale: Vec2D::new(sx_mag, sy),
                skew_x_rad,
            }
        } else {
            let sy_mag = c.hypot(d);
            let rotation_rad = if sy_mag > 1e-12 { (-c).atan2(d) } else { 0.0 };
            AffineDecomposed {
                translation,
                rotation_rad,
                scale: Vec2D::new(0.0, sy_mag),
                skew_x_rad: 0.0,
            }
        }
    }

    /// Converts to a `kurbo::Affine`.
    #[inline]
    #[must_use]
    pub const fn to_kurbo(self) -> kurbo::Affine {
        kurbo::Affine::new(self.m)
    }

    /// Creates from a `kurbo::Affine`.
    #[inline]
    #[must_use]
    pub fn from_kurbo(affine: kurbo::Affine) -> Self {
        Self {
            m: affine.as_coeffs(),
        }
    }
}

/// A 3×3 planar projective homography matrix stored in row-major order:
///
/// ```text
/// | m[0]  m[1]  m[2] |   | x |   | x' |
/// | m[3]  m[4]  m[5] | * | y | = | y' |   =>  (x'/w', y'/w')
/// | m[6]  m[7]  m[8] |   | 1 |   | w' |
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Perspective3x3 {
    pub m: [f64; 9],
}

impl Default for Perspective3x3 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Perspective3x3 {
    /// Identity projective transform.
    pub const IDENTITY: Self = Self {
        m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };

    /// Constructs a homography directly from 9 row-major coefficients.
    #[inline]
    #[must_use]
    pub const fn new(m: [f64; 9]) -> Self {
        Self { m }
    }

    /// Returns `true` if all 9 entries are finite.
    #[inline]
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.m.iter().all(|v| v.is_finite())
    }

    /// Promotes an [`Affine2D`] into a [`Perspective3x3`] with `[0, 0, 1]` bottom row.
    #[inline]
    #[must_use]
    pub const fn from_affine(a: Affine2D) -> Self {
        Self {
            m: [
                a.m[0], a.m[2], a.m[4], a.m[1], a.m[3], a.m[5], 0.0, 0.0, 1.0,
            ],
        }
    }

    /// Solves the exact 3×3 projective homography mapping `[0, 1]²` corners
    /// `[(0,0), (1,0), (1,1), (0,1)]` onto `quad` `[TL, TR, BR, BL]`.
    #[must_use]
    pub fn from_unit_square_to_quad(q: &[Point2D; 4]) -> Option<Self> {
        if !q.iter().all(|p| p.is_finite()) {
            return None;
        }
        let src = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let mut a = [[0.0_f64; 9]; 8];
        for i in 0..4 {
            let (u, v) = src[i];
            let (x, y) = (q[i].x, q[i].y);
            a[2 * i] = [u, v, 1.0, 0.0, 0.0, 0.0, -x * u, -x * v, x];
            a[2 * i + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -y * u, -y * v, y];
        }
        for col in 0..8 {
            let mut pivot_row = col;
            let mut max_val = a[col][col].abs();
            for (r, row) in a.iter().enumerate().skip(col + 1) {
                if row[col].abs() > max_val {
                    max_val = row[col].abs();
                    pivot_row = r;
                }
            }
            if !max_val.is_finite() || max_val < 1e-10 {
                return None;
            }
            a.swap(col, pivot_row);
            let div = a[col][col];
            for cell in a[col].iter_mut().skip(col) {
                *cell /= div;
            }
            for r in 0..8 {
                if r == col {
                    continue;
                }
                let factor = a[r][col];
                for c in col..9 {
                    a[r][c] -= factor * a[col][c];
                }
            }
        }
        let res = Self {
            m: [
                a[0][8], a[1][8], a[2][8], a[3][8], a[4][8], a[5][8], a[6][8], a[7][8], 1.0,
            ],
        };
        res.is_finite().then_some(res)
    }

    /// Solves the projective homography mapping `rect` `[TL, TR, BR, BL]` onto `quad`.
    #[must_use]
    pub fn from_rect_to_quad(rect: Rect2D, quad: &[Point2D; 4]) -> Option<Self> {
        if !rect.is_finite() || rect.width() <= 1e-9 || rect.height() <= 1e-9 {
            return None;
        }
        let unit_to_quad = Self::from_unit_square_to_quad(quad)?;
        let inv_w = 1.0 / rect.width();
        let inv_h = 1.0 / rect.height();
        let rect_to_unit = Self::new([
            inv_w,
            0.0,
            -rect.min_x * inv_w,
            0.0,
            inv_h,
            -rect.min_y * inv_h,
            0.0,
            0.0,
            1.0,
        ]);
        let res = unit_to_quad.compose(&rect_to_unit);
        res.is_finite().then_some(res)
    }

    /// Solves the projective homography mapping `src` quad onto `dst` quad.
    #[must_use]
    pub fn from_quad_to_quad(src: &[Point2D; 4], dst: &[Point2D; 4]) -> Option<Self> {
        let u2s = Self::from_unit_square_to_quad(src)?;
        let s2u = u2s.inverse()?;
        let u2d = Self::from_unit_square_to_quad(dst)?;
        let res = u2d.compose(&s2u);
        res.is_finite().then_some(res)
    }

    /// Matrix multiplication `self * other` (applies `other` first, then `self`).
    #[must_use]
    pub fn compose(&self, other: &Self) -> Self {
        let a = &self.m;
        let b = &other.m;
        Self {
            m: [
                a[0] * b[0] + a[1] * b[3] + a[2] * b[6],
                a[0] * b[1] + a[1] * b[4] + a[2] * b[7],
                a[0] * b[2] + a[1] * b[5] + a[2] * b[8],
                a[3] * b[0] + a[4] * b[3] + a[5] * b[6],
                a[3] * b[1] + a[4] * b[4] + a[5] * b[7],
                a[3] * b[2] + a[4] * b[5] + a[5] * b[8],
                a[6] * b[0] + a[7] * b[3] + a[8] * b[6],
                a[6] * b[1] + a[7] * b[4] + a[8] * b[7],
                a[6] * b[2] + a[7] * b[5] + a[8] * b[8],
            ],
        }
    }

    /// Exact 3×3 matrix inverse, normalized so `m[8] = 1.0` when non-zero, or `None` if singular.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        if !self.is_finite() {
            return None;
        }
        let [a, b, c, d, e, f, g, h, i] = self.m;
        let c00 = e * i - f * h;
        let c01 = -(d * i - f * g);
        let c02 = d * h - e * g;
        let c10 = -(b * i - c * h);
        let c11 = a * i - c * g;
        let c12 = -(a * h - b * g);
        let c20 = b * f - c * e;
        let c21 = -(a * f - c * d);
        let c22 = a * e - b * d;
        let det = a * c00 + b * c01 + c * c02;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let mut m = [
            c00 * inv_det,
            c10 * inv_det,
            c20 * inv_det,
            c01 * inv_det,
            c11 * inv_det,
            c21 * inv_det,
            c02 * inv_det,
            c12 * inv_det,
            c22 * inv_det,
        ];
        if m[8].abs() > 1e-12 {
            let norm = 1.0 / m[8];
            for v in &mut m {
                *v *= norm;
            }
        }
        let res = Self { m };
        res.is_finite().then_some(res)
    }

    /// Projects a 2D point through the homogeneous 3×3 matrix (`None` if at horizon `w ≈ 0`).
    #[inline]
    #[must_use]
    pub fn transform_point(&self, p: Point2D) -> Option<Point2D> {
        if !p.is_finite() || !self.is_finite() {
            return None;
        }
        let m = &self.m;
        let w = m[6] * p.x + m[7] * p.y + m[8];
        if !w.is_finite() || w.abs() <= 1e-6 {
            return None;
        }
        let x = (m[0] * p.x + m[1] * p.y + m[2]) / w;
        let y = (m[3] * p.x + m[4] * p.y + m[5]) / w;
        (x.is_finite() && y.is_finite()).then_some(Point2D::new(x, y))
    }

    /// Projects a 2D point, falling back to `p` if degenerate.
    #[inline]
    #[must_use]
    pub fn transform_point_or(&self, p: Point2D) -> Point2D {
        self.transform_point(p).unwrap_or(p)
    }

    /// Computes an axis-aligned bounding box enclosing the projected corners of `rect`.
    #[must_use]
    pub fn try_transform_rect(&self, rect: Rect2D) -> Option<Rect2D> {
        if !self.is_finite() || !rect.is_finite() {
            return None;
        }
        let mut pts = Vec::with_capacity(4);
        for c in rect.corners() {
            if let Some(tp) = self.transform_point(c) {
                pts.push(tp);
            }
        }
        Rect2D::from_iter_points(pts).filter(|r| r.is_finite())
    }

    /// Subdivides and warps a [`crate::geometry::BezierPath`] through this projective homography.
    #[must_use]
    pub fn transform_bezier_path(
        &self,
        path: &crate::geometry::BezierPath,
    ) -> crate::geometry::BezierPath {
        if path.is_empty() || !self.is_finite() {
            return path.clone();
        }
        let mut out_subpaths = Vec::with_capacity(path.subpaths.len());
        for sp in &path.subpaths {
            let seg_count = sp.segment_count();
            if seg_count == 0 {
                let nodes = sp
                    .nodes
                    .iter()
                    .map(|n| crate::geometry::PathNode {
                        pos: self.transform_point_or(n.pos),
                        handle_in: n.handle_in.map(|h| self.transform_point_or(h)),
                        handle_out: n.handle_out.map(|h| self.transform_point_or(h)),
                        kind: n.kind,
                    })
                    .collect();
                out_subpaths.push(crate::geometry::Subpath {
                    nodes,
                    closed: sp.closed,
                });
                continue;
            }
            let mut kb = kurbo::BezPath::new();
            let mut moved = false;
            for s in 0..seg_count {
                let Some(seg) = sp.segment_to_kurbo(s) else {
                    continue;
                };
                let is_cubic = matches!(seg, kurbo::PathSeg::Cubic(_));
                let chord = seg.start().distance(seg.end());
                let subdiv = if is_cubic {
                    ((chord / 2.5).ceil() as usize).clamp(2, 16)
                } else {
                    ((chord / 10.0).ceil() as usize).clamp(1, 8)
                };
                for k in 0..subdiv {
                    let t0 = k as f64 / subdiv as f64;
                    let t1 = (k + 1) as f64 / subdiv as f64;
                    let piece = kurbo::ParamCurve::subsegment(&seg, t0..t1);
                    if !moved {
                        let p0 = self.transform_point_or(Point2D::from_kurbo(piece.start()));
                        kb.move_to(p0.to_kurbo());
                        moved = true;
                    }
                    match piece {
                        kurbo::PathSeg::Line(l) => {
                            let p1 = self.transform_point_or(Point2D::from_kurbo(l.p1));
                            kb.line_to(p1.to_kurbo());
                        }
                        kurbo::PathSeg::Quad(q) => {
                            let c1 = self.transform_point_or(Point2D::from_kurbo(q.p1));
                            let p2 = self.transform_point_or(Point2D::from_kurbo(q.p2));
                            kb.quad_to(c1.to_kurbo(), p2.to_kurbo());
                        }
                        kurbo::PathSeg::Cubic(c) => {
                            let c1 = self.transform_point_or(Point2D::from_kurbo(c.p1));
                            let c2 = self.transform_point_or(Point2D::from_kurbo(c.p2));
                            let p3 = self.transform_point_or(Point2D::from_kurbo(c.p3));
                            kb.curve_to(c1.to_kurbo(), c2.to_kurbo(), p3.to_kurbo());
                        }
                    }
                }
            }
            if sp.closed && moved {
                kb.close_path();
            }
            let bp = crate::geometry::BezierPath::from_kurbo(&kb);
            out_subpaths.extend(bp.subpaths);
        }
        crate::geometry::BezierPath {
            subpaths: out_subpaths,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversions_are_exact() {
        let dpi = 300.0;
        assert!((Unit::Inch.to_mm(1.0, dpi) - 25.4).abs() < 1e-12);
        assert!((Unit::Centimeter.to_mm(2.54, dpi) - 25.4).abs() < 1e-12);
        assert!((Unit::Meter.to_mm(1.0, dpi) - 1000.0).abs() < 1e-12);
        assert!((Unit::Point.to_mm(72.0, dpi) - 25.4).abs() < 1e-12);
        assert!((Unit::Pica.to_mm(6.0, dpi) - 25.4).abs() < 1e-12);
        assert!((Unit::Pixel.to_mm(300.0, dpi) - 25.4).abs() < 1e-12);
        assert!((Unit::CdrTenthMicron.to_mm(10_000.0, dpi) - 1.0).abs() < 1e-12);
        assert!((Unit::CdrTenthMicron.from_mm(1.0, dpi) - 10_000.0).abs() < 1e-12);

        // Round-trip all units
        let all = [
            Unit::Millimeter,
            Unit::Centimeter,
            Unit::Meter,
            Unit::Inch,
            Unit::Point,
            Unit::Pica,
            Unit::Pixel,
            Unit::CdrTenthMicron,
        ];
        for u in all {
            let mm = u.to_mm(42.5, 96.0);
            let back = u.from_mm(mm, 96.0);
            assert!((back - 42.5).abs() < 1e-9, "roundtrip failed for {u:?}");
        }
        // NaN / zero DPI safety
        assert_eq!(Unit::Pixel.to_mm(f64::NAN, 96.0), 0.0);
        assert!((Unit::Pixel.to_mm(96.0, 0.0) - 25.4).abs() < 1e-12);
    }

    #[test]
    fn rect_operations() {
        let a = Rect2D::from_xywh(0.0, 0.0, 10.0, 20.0);
        let b = Rect2D::from_xywh(5.0, 10.0, 15.0, 15.0);
        assert!(a.intersects(b));
        assert!(a.contains_point(Point2D::new(5.0, 10.0)));
        assert!(!a.contains_point(Point2D::new(11.0, 10.0)));
        assert!(!a.contains_point(Point2D::new(f64::NAN, 0.0)));

        let u = a.union(b);
        assert_eq!(u, Rect2D::new(0.0, 0.0, 20.0, 25.0));
        let exp = a.expand(2.0);
        assert_eq!(exp, Rect2D::new(-2.0, -2.0, 12.0, 22.0));
    }

    #[test]
    fn affine_compose_inverse_and_decompose() {
        let t = Affine2D::translate(15.0, -7.5)
            .compose(&Affine2D::rotate_deg(35.0))
            .compose(&Affine2D::skew(0.15, 0.0))
            .compose(&Affine2D::scale(2.0, 1.5));

        let inv = t.inverse().expect("invertible");
        let id = t.compose(&inv);
        for (i, (&got, &expected)) in id.m.iter().zip(Affine2D::IDENTITY.m.iter()).enumerate() {
            assert!((got - expected).abs() < 1e-10, "entry {i} mismatch: {got}");
        }

        let dec = t.decompose();
        let rebuilt = Affine2D::translate(dec.translation.x, dec.translation.y)
            .compose(&Affine2D::rotate(dec.rotation_rad))
            .compose(&Affine2D::skew(dec.skew_x_rad, 0.0))
            .compose(&Affine2D::scale(dec.scale.x, dec.scale.y));
        for (i, (&got, &expected)) in rebuilt.m.iter().zip(t.m.iter()).enumerate() {
            assert!(
                (got - expected).abs() < 1e-9,
                "decomposed entry {i} mismatch: {got} vs {expected}"
            );
        }

        // Singular matrix safely returns None
        assert!(Affine2D::scale(0.0, 1.0).inverse().is_none());
        assert!(Affine2D::new(f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0)
            .inverse()
            .is_none());
    }

    #[test]
    fn perspective_3x3_homography_projects_midpoint_and_inverts() {
        let rect = Rect2D::from_xywh(0.0, 0.0, 60.0, 60.0);
        let quad = [
            Point2D::new(20.0, 0.0),
            Point2D::new(40.0, 0.0),
            Point2D::new(60.0, 60.0),
            Point2D::new(0.0, 60.0),
        ];
        let h = Perspective3x3::from_rect_to_quad(rect, &quad).expect("valid homography");
        let corners = rect.corners();
        for (c, q) in corners.iter().zip(quad.iter()) {
            let mapped = h.transform_point(*c).expect("corner mapped");
            assert!(
                mapped.distance_to(*q) < 1e-8,
                "corner {c:?} mapped to {mapped:?} instead of {q:?}"
            );
        }
        // Projective center (30, 30) maps strictly above bilinear midpoint (30, 30) because top edge is narrower!
        let proj_center = h.transform_point(Point2D::new(30.0, 30.0)).expect("center");
        assert!(
            proj_center.y < 25.0 && (proj_center.x - 30.0).abs() < 1e-8,
            "expected projective midpoint y < 25, got {proj_center:?}"
        );
        let inv = h.inverse().expect("invertible homography");
        let back = inv.transform_point(proj_center).expect("inverse point");
        assert!(back.distance_to(Point2D::new(30.0, 30.0)) < 1e-7);

        // Degenerate collinear quad safely returns None
        let colinear = [Point2D::ZERO; 4];
        assert!(Perspective3x3::from_unit_square_to_quad(&colinear).is_none());
    }
}
