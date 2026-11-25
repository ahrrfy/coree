// SPDX-License-Identifier: MIT OR Apache-2.0

//! Technical dimensioning objects and dynamic snap-anchored connectors.

use crate::geometry::{BezierPath, NodeKind, PathNode, Subpath};
use crate::ids::ObjectId;
use crate::math::{sanitize_f64, Point2D, Rect2D, Unit, DEFAULT_DPI};
use crate::text::TextStyle;
use serde::{Deserialize, Serialize};

/// Measurement geometry mode for [`DimensionObject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DimensionKind {
    #[default]
    Parallel,
    Horizontal,
    Vertical,
    Angular,
    Segment,
    Callout3Pt,
}

/// Live technical dimension or 3-point callout object.
///
/// For [`DimensionKind::Angular`], `p1` is the angle vertex, `p2` lies on the first ray,
/// and `p3` lies on the second ray (defaults to a horizontal `+X` ray from `p1` if `None`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionObject {
    pub kind: DimensionKind,
    pub p1: Point2D,
    pub p2: Point2D,
    pub p3: Option<Point2D>,
    pub offset_mm: f64,
    pub precision: usize,
    pub prefix: String,
    pub suffix: String,
    pub label_override: Option<String>,
    pub text_style: TextStyle,
    pub unit_override: Option<Unit>,
}

impl DimensionObject {
    /// Creates a linear dimension (`Parallel`, `Horizontal`, `Vertical`, or `Segment`).
    #[must_use]
    pub fn linear(kind: DimensionKind, p1: Point2D, p2: Point2D, offset_mm: f64) -> Self {
        Self {
            kind,
            p1,
            p2,
            p3: None,
            offset_mm: sanitize_f64(offset_mm, 8.0),
            precision: 2,
            prefix: String::new(),
            suffix: String::new(),
            label_override: None,
            text_style: TextStyle {
                font_size_pt: 10.0,
                ..TextStyle::default()
            },
            unit_override: None,
        }
    }

    /// Creates an angular dimension centered at `vertex` measuring angle `ray_a` to `ray_b`.
    #[must_use]
    pub fn angular(vertex: Point2D, ray_a: Point2D, ray_b: Point2D, offset_mm: f64) -> Self {
        Self {
            kind: DimensionKind::Angular,
            p1: vertex,
            p2: ray_a,
            p3: Some(ray_b),
            offset_mm: sanitize_f64(offset_mm, 12.0),
            precision: 1,
            prefix: String::new(),
            suffix: String::new(),
            label_override: None,
            text_style: TextStyle {
                font_size_pt: 10.0,
                ..TextStyle::default()
            },
            unit_override: None,
        }
    }

    /// Computes the numeric measurement value in `unit` (or degrees for `Angular`).
    #[must_use]
    pub fn compute_value(&self, unit: Unit, dpi: f64) -> f64 {
        self.compute_value_with_scale(unit, dpi, crate::document::DrawingScale::default())
    }

    /// Computes the numeric measurement value in `unit` (or degrees for `Angular`),
    /// scaling linear dimensions by `drawing_scale`.
    #[must_use]
    pub fn compute_value_with_scale(
        &self,
        unit: Unit,
        dpi: f64,
        drawing_scale: crate::document::DrawingScale,
    ) -> f64 {
        let effective_unit = self.unit_override.unwrap_or(unit);
        let scale_factor = drawing_scale.scale_factor();
        match self.kind {
            DimensionKind::Parallel | DimensionKind::Segment => {
                let mm = sanitize_f64(self.p1.distance_to(self.p2), 0.0) * scale_factor;
                effective_unit.from_mm(mm, dpi)
            }
            DimensionKind::Horizontal => {
                let mm = sanitize_f64((self.p2.x - self.p1.x).abs(), 0.0) * scale_factor;
                effective_unit.from_mm(mm, dpi)
            }
            DimensionKind::Vertical => {
                let mm = sanitize_f64((self.p2.y - self.p1.y).abs(), 0.0) * scale_factor;
                effective_unit.from_mm(mm, dpi)
            }
            DimensionKind::Angular => {
                if !self.p1.is_finite()
                    || !self.p2.is_finite()
                    || self.p3.is_some_and(|p| !p.is_finite())
                {
                    return 0.0;
                }
                let v1 = self.p2 - self.p1;
                let v2 = self.p3.map_or_else(
                    || Point2D::new(self.p1.x + 1.0, self.p1.y) - self.p1,
                    |p| p - self.p1,
                );
                let l1 = v1.length();
                let l2 = v2.length();
                if !l1.is_finite() || !l2.is_finite() || l1 <= 1e-9 || l2 <= 1e-9 {
                    return 0.0;
                }
                let cos_a = v1.dot(v2) / (l1 * l2);
                if !cos_a.is_finite() {
                    return 0.0;
                }
                cos_a.clamp(-1.0, 1.0).acos().to_degrees()
            }
            DimensionKind::Callout3Pt => 0.0,
        }
    }

    /// Formats the measurement string using `unit` (at default 96 DPI).
    #[must_use]
    pub fn compute_measurement(&self, unit: Unit) -> String {
        self.compute_measurement_with_dpi(unit, DEFAULT_DPI)
    }

    /// Formats the measurement string using `unit` and explicit `dpi`.
    #[must_use]
    pub fn compute_measurement_with_dpi(&self, unit: Unit, dpi: f64) -> String {
        self.compute_measurement_with_scale(unit, dpi, crate::document::DrawingScale::default())
    }

    /// Formats the measurement string using `unit`, explicit `dpi`, and engineering `drawing_scale`.
    #[must_use]
    pub fn compute_measurement_with_scale(
        &self,
        unit: Unit,
        dpi: f64,
        drawing_scale: crate::document::DrawingScale,
    ) -> String {
        if let Some(label) = &self.label_override {
            return label.clone();
        }
        if self.kind == DimensionKind::Callout3Pt {
            return format!("{}{}", self.prefix, self.suffix);
        }
        let prec = self.precision.min(6);
        let value = self.compute_value_with_scale(unit, dpi, drawing_scale);
        let effective_unit = self.unit_override.unwrap_or(unit);
        if self.kind == DimensionKind::Angular {
            format!("{}{value:.prec$}°{}", self.prefix, self.suffix)
        } else {
            format!(
                "{}{value:.prec$} {}{}",
                self.prefix,
                effective_unit.suffix(),
                self.suffix
            )
        }
    }

    /// Local-space bounding box enclosing the dimension anchor points and offset line.
    #[must_use]
    pub fn bounding_box(&self) -> Rect2D {
        let mut pts = vec![self.p1, self.p2];
        if let Some(p3) = self.p3 {
            pts.push(p3);
        }
        let base = Rect2D::from_iter_points(pts).unwrap_or_default();
        base.expand(sanitize_f64(self.offset_mm, 0.0).abs())
    }
}

/// Named anchor attachment slot on a connected object's bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum AnchorPoint {
    #[default]
    Center,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Custom(Point2D),
}

impl AnchorPoint {
    /// Resolves this anchor onto a world-space point given an object's bounding box.
    #[must_use]
    pub fn resolve(self, bounds: Rect2D) -> Point2D {
        let c = bounds.center();
        match self {
            Self::Center => c,
            Self::Top => Point2D::new(c.x, bounds.min_y),
            Self::Bottom => Point2D::new(c.x, bounds.max_y),
            Self::Left => Point2D::new(bounds.min_x, c.y),
            Self::Right => Point2D::new(bounds.max_x, c.y),
            Self::TopLeft => Point2D::new(bounds.min_x, bounds.min_y),
            Self::TopRight => Point2D::new(bounds.max_x, bounds.min_y),
            Self::BottomLeft => Point2D::new(bounds.min_x, bounds.max_y),
            Self::BottomRight => Point2D::new(bounds.max_x, bounds.max_y),
            Self::Custom(p) => p,
        }
    }
}

/// Connector routing line geometry style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ConnectorStyle {
    #[default]
    Straight,
    RightAngle,
    RoundedRightAngle,
}

/// Dynamic flowchart / diagram connector linked between two scene objects (or free endpoints).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorObject {
    pub from_obj: Option<ObjectId>,
    pub from_anchor: AnchorPoint,
    pub from_fallback: Point2D,
    pub to_obj: Option<ObjectId>,
    pub to_anchor: AnchorPoint,
    pub to_fallback: Point2D,
    pub style: ConnectorStyle,
    pub corner_radius_mm: f64,
}

impl ConnectorObject {
    /// Creates a connector linking two scene objects.
    #[must_use]
    pub fn between_objects(
        from_obj: ObjectId,
        from_anchor: AnchorPoint,
        to_obj: ObjectId,
        to_anchor: AnchorPoint,
        style: ConnectorStyle,
    ) -> Self {
        Self {
            from_obj: Some(from_obj),
            from_anchor,
            from_fallback: Point2D::ZERO,
            to_obj: Some(to_obj),
            to_anchor,
            to_fallback: Point2D::new(20.0, 0.0),
            style,
            corner_radius_mm: 3.0,
        }
    }

    /// Generates the routed [`BezierPath`] connecting resolved world points `from_pt` and `to_pt`.
    #[must_use]
    pub fn route_path(&self, from_pt: Point2D, to_pt: Point2D) -> BezierPath {
        if !from_pt.is_finite() || !to_pt.is_finite() {
            return BezierPath::new();
        }
        match self.style {
            ConnectorStyle::Straight => BezierPath::from_line(from_pt, to_pt),
            ConnectorStyle::RightAngle => {
                let mid_x = 0.5 * (from_pt.x + to_pt.x);
                BezierPath::from_polygon(
                    [
                        from_pt,
                        Point2D::new(mid_x, from_pt.y),
                        Point2D::new(mid_x, to_pt.y),
                        to_pt,
                    ],
                    false,
                )
            }
            ConnectorStyle::RoundedRightAngle => {
                let mid_x = 0.5 * (from_pt.x + to_pt.x);
                let dx = (to_pt.x - from_pt.x).abs();
                let dy = (to_pt.y - from_pt.y).abs();
                let r =
                    sanitize_f64(self.corner_radius_mm, 3.0).clamp(0.0, (dx * 0.25).min(dy * 0.5));
                if r <= 1e-4 {
                    return BezierPath::from_polygon(
                        [
                            from_pt,
                            Point2D::new(mid_x, from_pt.y),
                            Point2D::new(mid_x, to_pt.y),
                            to_pt,
                        ],
                        false,
                    );
                }
                let sx = if to_pt.x >= from_pt.x { 1.0 } else { -1.0 };
                let sy = if to_pt.y >= from_pt.y { 1.0 } else { -1.0 };
                let k = 0.552_284_75 * r;

                let c1 = Point2D::new(mid_x, from_pt.y);
                let c1_in = Point2D::new(mid_x - sx * r, from_pt.y);
                let c1_out = Point2D::new(mid_x, from_pt.y + sy * r);

                let c2 = Point2D::new(mid_x, to_pt.y);
                let c2_in = Point2D::new(mid_x, to_pt.y - sy * r);
                let c2_out = Point2D::new(mid_x + sx * r, to_pt.y);

                let nodes = vec![
                    PathNode::corner(from_pt),
                    PathNode::new(
                        c1_in,
                        None,
                        Some(Point2D::new(c1.x - sx * (r - k), c1.y)),
                        NodeKind::Cusp,
                    ),
                    PathNode::new(
                        c1_out,
                        Some(Point2D::new(c1.x, c1.y + sy * (r - k))),
                        None,
                        NodeKind::Cusp,
                    ),
                    PathNode::new(
                        c2_in,
                        None,
                        Some(Point2D::new(c2.x, c2.y - sy * (r - k))),
                        NodeKind::Cusp,
                    ),
                    PathNode::new(
                        c2_out,
                        Some(Point2D::new(c2.x + sx * (r - k), c2.y)),
                        None,
                        NodeKind::Cusp,
                    ),
                    PathNode::corner(to_pt),
                ];
                BezierPath::from_subpath(Subpath {
                    nodes,
                    closed: false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_measurements_and_connectors() {
        let diag = DimensionObject::linear(
            DimensionKind::Parallel,
            Point2D::new(0.0, 0.0),
            Point2D::new(30.0, 40.0),
            5.0,
        );
        assert_eq!(diag.compute_measurement(Unit::Millimeter), "50.00 mm");

        let horiz = DimensionObject::linear(
            DimensionKind::Horizontal,
            Point2D::new(0.0, 0.0),
            Point2D::new(25.4, 99.0),
            5.0,
        );
        assert_eq!(horiz.compute_measurement(Unit::Inch), "1.00 in");

        let ang = DimensionObject::angular(
            Point2D::ZERO,
            Point2D::new(10.0, 0.0),
            Point2D::new(0.0, 10.0),
            10.0,
        );
        assert_eq!(ang.compute_measurement(Unit::Millimeter), "90.0°");

        let conn = ConnectorObject::between_objects(
            ObjectId(1),
            AnchorPoint::Right,
            ObjectId(2),
            AnchorPoint::Left,
            ConnectorStyle::RoundedRightAngle,
        );
        let route = conn.route_path(Point2D::new(0.0, 0.0), Point2D::new(40.0, 20.0));
        assert!(route.path_length_mm() > 40.0);
        assert!(route.is_finite());
    }
}
