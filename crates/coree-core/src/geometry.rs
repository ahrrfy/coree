// SPDX-License-Identifier: MIT OR Apache-2.0

//! Vector Bézier geometry, node editing, sculpting, knife/virtual-segment/eraser
//! operations, smart stroke recognition, artistic media expansion, and live
//! parametric shapes.
//!
//! All coordinates are in canonical **millimeters (`f64`)**. Control handles
//! [`PathNode::handle_in`] and [`PathNode::handle_out`] are stored as **absolute
//! canvas/local coordinates** in millimeters.

use crate::error::{CoreError, CoreResult};
use crate::math::{sanitize_f64, Affine2D, Point2D, Rect2D, Vec2D};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::float::slice::FloatSlice;
use kurbo::{
    BezPath, CubicBez, Line, ParamCurve, ParamCurveArclen, ParamCurveDeriv, PathEl, PathSeg, Shape,
};
use serde::{Deserialize, Serialize};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

/// Cubic Bézier circular-arc approximation constant `4 * (sqrt(2) - 1) / 3`.
const CIRCLE_KAPPA: f64 = 0.552_284_749_830_793_6;

/// Smoothness continuity constraint of a [`PathNode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum NodeKind {
    /// Independent incoming and outgoing handles (sharp corner possible).
    #[default]
    Cusp,
    /// Incoming and outgoing handles remain collinear (C1 tangent continuity).
    Smooth,
    /// Incoming and outgoing handles are collinear and equal length (symmetric).
    Symmetrical,
}

/// A single vertex on a cubic Bézier [`Subpath`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PathNode {
    /// Anchor position in millimeters.
    pub pos: Point2D,
    /// Optional incoming cubic control point (absolute mm coordinates).
    pub handle_in: Option<Point2D>,
    /// Optional outgoing cubic control point (absolute mm coordinates).
    pub handle_out: Option<Point2D>,
    /// Handle continuity rule.
    pub kind: NodeKind,
}

impl PathNode {
    /// Creates a corner node with no Bézier handles.
    #[inline]
    #[must_use]
    pub const fn corner(pos: Point2D) -> Self {
        Self {
            pos,
            handle_in: None,
            handle_out: None,
            kind: NodeKind::Cusp,
        }
    }

    /// Creates a node with explicit handles and continuity kind.
    #[inline]
    #[must_use]
    pub const fn new(
        pos: Point2D,
        handle_in: Option<Point2D>,
        handle_out: Option<Point2D>,
        kind: NodeKind,
    ) -> Self {
        Self {
            pos,
            handle_in,
            handle_out,
            kind,
        }
    }

    /// Relative vector from `pos` to `handle_in`.
    #[inline]
    #[must_use]
    pub fn in_vec(&self) -> Option<Vec2D> {
        self.handle_in.map(|h| h - self.pos)
    }

    /// Relative vector from `pos` to `handle_out`.
    #[inline]
    #[must_use]
    pub fn out_vec(&self) -> Option<Vec2D> {
        self.handle_out.map(|h| h - self.pos)
    }

    /// Returns `true` if the anchor and all present handles are finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.pos.is_finite()
            && self.handle_in.is_none_or(Point2D::is_finite)
            && self.handle_out.is_none_or(Point2D::is_finite)
    }

    /// Transforms anchor and handles by `affine`.
    pub fn transform(&mut self, affine: &Affine2D) {
        self.pos = affine.transform_point(self.pos);
        if let Some(h) = self.handle_in.as_mut() {
            *h = affine.transform_point(*h);
        }
        if let Some(h) = self.handle_out.as_mut() {
            *h = affine.transform_point(*h);
        }
    }

    /// Translates anchor and handles together by `delta`.
    pub fn translate(&mut self, delta: Vec2D) {
        self.pos = self.pos + delta;
        if let Some(h) = self.handle_in.as_mut() {
            *h = *h + delta;
        }
        if let Some(h) = self.handle_out.as_mut() {
            *h = *h + delta;
        }
    }

    /// Adjusts `handle_out` (and optionally `handle_in`) to satisfy `self.kind`.
    pub fn enforce_continuity(&mut self) {
        match self.kind {
            NodeKind::Cusp => {}
            NodeKind::Smooth => {
                if let (Some(h_in), Some(h_out)) = (self.handle_in, self.handle_out) {
                    let v_in = h_in - self.pos;
                    let v_out = h_out - self.pos;
                    let dir = (v_out - v_in)
                        .normalized()
                        .or_else(|| v_in.normalized().map(|d| -d))
                        .or_else(|| v_out.normalized());
                    if let Some(dir) = dir {
                        let len_in = v_in.length();
                        let len_out = v_out.length();
                        self.handle_in = Some(self.pos - dir * len_in);
                        self.handle_out = Some(self.pos + dir * len_out);
                    }
                }
            }
            NodeKind::Symmetrical => {
                if let (Some(h_in), Some(h_out)) = (self.handle_in, self.handle_out) {
                    let v_in = h_in - self.pos;
                    let v_out = h_out - self.pos;
                    let avg_len = 0.5 * (v_in.length() + v_out.length());
                    let dir = (v_out - v_in)
                        .normalized()
                        .or_else(|| v_in.normalized().map(|d| -d))
                        .or_else(|| v_out.normalized());
                    if let Some(dir) = dir {
                        self.handle_in = Some(self.pos - dir * avg_len);
                        self.handle_out = Some(self.pos + dir * avg_len);
                    }
                } else if let Some(h_in) = self.handle_in {
                    let v = h_in - self.pos;
                    self.handle_out = Some(self.pos - v);
                } else if let Some(h_out) = self.handle_out {
                    let v = h_out - self.pos;
                    self.handle_in = Some(self.pos - v);
                }
            }
        }
    }

    /// Enforces `self.kind` continuity while keeping the actively dragged handle
    /// (`handle_in` when `active_is_in` is `true`, `handle_out` otherwise) fixed in place.
    pub fn enforce_continuity_for_handle(&mut self, active_is_in: bool) {
        match self.kind {
            NodeKind::Cusp => {}
            NodeKind::Smooth => {
                if active_is_in {
                    if let (Some(h_in), Some(h_out)) = (self.handle_in, self.handle_out) {
                        if let Some(dir) = (h_in - self.pos).normalized() {
                            let len_out = (h_out - self.pos).length();
                            self.handle_out = Some(self.pos - dir * len_out);
                        }
                    }
                } else if let (Some(h_out), Some(h_in)) = (self.handle_out, self.handle_in) {
                    if let Some(dir) = (h_out - self.pos).normalized() {
                        let len_in = (h_in - self.pos).length();
                        self.handle_in = Some(self.pos - dir * len_in);
                    }
                }
            }
            NodeKind::Symmetrical => {
                if active_is_in {
                    if let Some(h_in) = self.handle_in {
                        let v = h_in - self.pos;
                        if v.length() > 1e-12 {
                            self.handle_out = Some(self.pos - v);
                        }
                    }
                } else if let Some(h_out) = self.handle_out {
                    let v = h_out - self.pos;
                    if v.length() > 1e-12 {
                        self.handle_in = Some(self.pos - v);
                    }
                }
            }
        }
    }
}

/// A single connected contour (either open polyline/curve or closed loop).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Subpath {
    pub nodes: Vec<PathNode>,
    pub closed: bool,
}

impl Subpath {
    /// Creates an empty subpath.
    #[must_use]
    pub fn new(closed: bool) -> Self {
        Self {
            nodes: Vec::new(),
            closed,
        }
    }

    /// Creates a polygonal subpath from vertices.
    #[must_use]
    pub fn from_points(points: impl IntoIterator<Item = Point2D>, closed: bool) -> Self {
        let nodes = points
            .into_iter()
            .filter(|p| p.is_finite())
            .map(PathNode::corner)
            .collect();
        Self { nodes, closed }
    }

    /// Number of segments in this subpath.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        if self.nodes.len() < 2 {
            0
        } else if self.closed {
            self.nodes.len()
        } else {
            self.nodes.len() - 1
        }
    }

    /// Returns the `(start_node_idx, end_node_idx)` pair for `seg_idx`.
    #[must_use]
    pub fn segment_node_indices(&self, seg_idx: usize) -> Option<(usize, usize)> {
        let count = self.segment_count();
        if seg_idx >= count {
            return None;
        }
        let next = (seg_idx + 1) % self.nodes.len();
        Some((seg_idx, next))
    }

    /// Returns the `kurbo::PathSeg` for segment `seg_idx`.
    #[must_use]
    pub fn segment_to_kurbo(&self, seg_idx: usize) -> Option<PathSeg> {
        let (i0, i1) = self.segment_node_indices(seg_idx)?;
        let n0 = self.nodes[i0];
        let n1 = self.nodes[i1];
        if !n0.pos.is_finite() || !n1.pos.is_finite() {
            return None;
        }
        match (n0.handle_out, n1.handle_in) {
            (None, None) => Some(PathSeg::Line(Line::new(
                n0.pos.to_kurbo(),
                n1.pos.to_kurbo(),
            ))),
            (h0, h1) => {
                let c0 = h0.filter(|p| p.is_finite()).unwrap_or(n0.pos);
                let c1 = h1.filter(|p| p.is_finite()).unwrap_or(n1.pos);
                Some(PathSeg::Cubic(CubicBez::new(
                    n0.pos.to_kurbo(),
                    c0.to_kurbo(),
                    c1.to_kurbo(),
                    n1.pos.to_kurbo(),
                )))
            }
        }
    }

    /// Reverses the traversal order of this subpath in-place.
    pub fn reverse(&mut self) {
        self.nodes.reverse();
        for node in &mut self.nodes {
            std::mem::swap(&mut node.handle_in, &mut node.handle_out);
        }
    }

    /// Transforms every node and handle by `affine`.
    pub fn transform(&mut self, affine: &Affine2D) {
        for node in &mut self.nodes {
            node.transform(affine);
        }
    }
}

/// Multi-subpath cubic Bézier path (compound vector geometry).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BezierPath {
    pub subpaths: Vec<Subpath>,
}

impl BezierPath {
    /// Creates an empty path.
    #[must_use]
    pub fn new() -> Self {
        Self {
            subpaths: Vec::new(),
        }
    }

    /// Creates a path containing a single subpath.
    #[must_use]
    pub fn from_subpath(subpath: Subpath) -> Self {
        if subpath.nodes.is_empty() {
            Self::new()
        } else {
            Self {
                subpaths: vec![subpath],
            }
        }
    }

    /// Creates a single polygonal subpath from points.
    #[must_use]
    pub fn from_polygon(points: impl IntoIterator<Item = Point2D>, closed: bool) -> Self {
        Self::from_subpath(Subpath::from_points(points, closed))
    }

    /// Creates a closed axis-aligned rectangle path.
    #[must_use]
    pub fn from_rect(rect: Rect2D) -> Self {
        if !rect.is_finite() {
            return Self::new();
        }
        Self::from_polygon(rect.corners(), true)
    }

    /// Creates a single open line segment from `a` to `b`.
    #[must_use]
    pub fn from_line(a: Point2D, b: Point2D) -> Self {
        if !a.is_finite() || !b.is_finite() {
            return Self::new();
        }
        Self::from_polygon([a, b], false)
    }

    /// Returns `true` if there are no non-empty subpaths.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subpaths.iter().all(|sp| sp.nodes.is_empty())
    }

    /// Total number of anchor nodes across all subpaths.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.subpaths.iter().map(|sp| sp.nodes.len()).sum()
    }

    /// Returns `true` if every node and handle in the path is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.subpaths
            .iter()
            .all(|sp| sp.nodes.iter().all(PathNode::is_finite))
    }

    /// Converts to a `kurbo::BezPath`, skipping non-finite subpaths safely.
    #[must_use]
    pub fn to_kurbo(&self) -> BezPath {
        let mut out = BezPath::new();
        for sp in &self.subpaths {
            push_subpath_to_kurbo(sp, &mut out);
        }
        out
    }

    /// Reconstructs a [`BezierPath`] from a `kurbo::BezPath`, elevating quadratics to cubics.
    #[must_use]
    pub fn from_kurbo(path: &BezPath) -> Self {
        let mut subpaths = Vec::new();
        let mut current: Option<Subpath> = None;

        for el in path.elements() {
            match *el {
                PathEl::MoveTo(p) => {
                    if let Some(sp) = current.take() {
                        if !sp.nodes.is_empty() {
                            subpaths.push(sp);
                        }
                    }
                    let pt = Point2D::from_kurbo(p);
                    if pt.is_finite() {
                        let mut sp = Subpath::new(false);
                        sp.nodes.push(PathNode::corner(pt));
                        current = Some(sp);
                    }
                }
                PathEl::LineTo(p) => {
                    let pt = Point2D::from_kurbo(p);
                    if !pt.is_finite() {
                        continue;
                    }
                    let sp = current.get_or_insert_with(|| Subpath::new(false));
                    sp.nodes.push(PathNode::corner(pt));
                }
                PathEl::QuadTo(p1, p2) => {
                    let sp = current.get_or_insert_with(|| Subpath::new(false));
                    let p0 = sp.nodes.last().map_or(Point2D::from_kurbo(p1), |n| n.pos);
                    let q1 = Point2D::from_kurbo(p1);
                    let q2 = Point2D::from_kurbo(p2);
                    if !p0.is_finite() || !q1.is_finite() || !q2.is_finite() {
                        continue;
                    }
                    // Exact quadratic -> cubic degree elevation
                    let c1 = Point2D::new(
                        p0.x + (2.0 / 3.0) * (q1.x - p0.x),
                        p0.y + (2.0 / 3.0) * (q1.y - p0.y),
                    );
                    let c2 = Point2D::new(
                        q2.x + (2.0 / 3.0) * (q1.x - q2.x),
                        q2.y + (2.0 / 3.0) * (q1.y - q2.y),
                    );
                    if let Some(prev) = sp.nodes.last_mut() {
                        prev.handle_out = Some(c1);
                    }
                    sp.nodes
                        .push(PathNode::new(q2, Some(c2), None, NodeKind::Cusp));
                }
                PathEl::CurveTo(p1, p2, p3) => {
                    let sp = current.get_or_insert_with(|| Subpath::new(false));
                    let c1 = Point2D::from_kurbo(p1);
                    let c2 = Point2D::from_kurbo(p2);
                    let end = Point2D::from_kurbo(p3);
                    if !c1.is_finite() || !c2.is_finite() || !end.is_finite() {
                        continue;
                    }
                    if let Some(prev) = sp.nodes.last_mut() {
                        prev.handle_out = Some(c1);
                    }
                    sp.nodes
                        .push(PathNode::new(end, Some(c2), None, NodeKind::Cusp));
                }
                PathEl::ClosePath => {
                    if let Some(mut sp) = current.take() {
                        sp.closed = true;
                        // If last node coincides with first node, fold its incoming handle into first node
                        if sp.nodes.len() > 1 {
                            let first_pos = sp.nodes[0].pos;
                            let last = sp.nodes[sp.nodes.len() - 1];
                            if first_pos.distance_to(last.pos) < 1e-6 {
                                sp.nodes.pop();
                                if sp.nodes[0].handle_in.is_none() {
                                    sp.nodes[0].handle_in = last.handle_in;
                                }
                            }
                        }
                        if !sp.nodes.is_empty() {
                            subpaths.push(sp);
                        }
                    }
                }
            }
        }

        if let Some(sp) = current {
            if !sp.nodes.is_empty() {
                subpaths.push(sp);
            }
        }
        Self { subpaths }
    }

    /// Tight axis-aligned bounding box in millimeters, or `None` if empty/non-finite.
    #[must_use]
    pub fn bounding_box(&self) -> Option<Rect2D> {
        if self.is_empty() {
            return None;
        }
        let kb = self.to_kurbo();
        if kb.is_empty() {
            return None;
        }
        let r = kb.bounding_box();
        let rect = Rect2D::from_kurbo(r);
        rect.is_finite().then_some(rect)
    }

    /// Transforms every subpath in place by `affine`.
    pub fn transform(&mut self, affine: &Affine2D) {
        if !affine.is_finite() {
            return;
        }
        for sp in &mut self.subpaths {
            sp.transform(affine);
        }
    }

    /// Returns a transformed clone of this path.
    #[must_use]
    pub fn transformed(&self, affine: &Affine2D) -> Self {
        let mut cloned = self.clone();
        cloned.transform(affine);
        cloned
    }

    /// Flattens every subpath into polylines with maximum chord error `tolerance_mm`.
    #[must_use]
    pub fn flatten(&self, tolerance_mm: f64) -> Vec<Vec<Point2D>> {
        const MAX_FLATTEN_VERTICES: usize = 1_000_000;
        if self.subpaths.iter().any(|sp| {
            sp.nodes.iter().any(|n| {
                !n.pos.is_finite()
                    || n.handle_in.is_some_and(|h| !h.is_finite())
                    || n.handle_out.is_some_and(|h| !h.is_finite())
            })
        }) {
            return Vec::new();
        }
        let Some(bb) = self.bounding_box() else {
            return Vec::new();
        };
        if !bb.is_finite() {
            return Vec::new();
        }
        let max_span = bb.width().max(bb.height());
        if !max_span.is_finite() {
            return Vec::new();
        }
        let tol = sanitize_f64(tolerance_mm, 0.05)
            .max(1e-4)
            .max(max_span * 1e-5);
        let kb = self.to_kurbo();
        let mut polylines: Vec<Vec<Point2D>> = Vec::new();
        let mut current: Vec<Point2D> = Vec::new();
        let mut start_pt: Option<Point2D> = None;
        let mut total_verts = 0usize;

        kurbo::flatten(kb.iter(), tol, |el| {
            if total_verts >= MAX_FLATTEN_VERTICES {
                return;
            }
            match el {
                PathEl::MoveTo(p) => {
                    if !current.is_empty() {
                        polylines.push(std::mem::take(&mut current));
                    }
                    let pt = Point2D::from_kurbo(p);
                    if pt.is_finite() {
                        start_pt = Some(pt);
                        current.push(pt);
                        total_verts += 1;
                    } else {
                        start_pt = None;
                    }
                }
                PathEl::LineTo(p) => {
                    let pt = Point2D::from_kurbo(p);
                    if pt.is_finite()
                        && current
                            .last()
                            .is_none_or(|prev| prev.distance_to(pt) > 1e-9)
                    {
                        current.push(pt);
                        total_verts += 1;
                    }
                }
                PathEl::ClosePath => {
                    if let Some(s) = start_pt {
                        if current.last().is_none_or(|prev| prev.distance_to(s) > 1e-9)
                            && total_verts < MAX_FLATTEN_VERTICES
                        {
                            current.push(s);
                            total_verts += 1;
                        }
                    }
                    if !current.is_empty() {
                        polylines.push(std::mem::take(&mut current));
                    }
                    start_pt = None;
                }
                PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
            }
        });

        if !current.is_empty() {
            polylines.push(current);
        }
        polylines
    }

    /// Total arc length of all segments in millimeters (powers the Path Length Meter).
    #[must_use]
    pub fn path_length_mm(&self) -> f64 {
        let kb = self.to_kurbo();
        let mut total = 0.0;
        for seg in kb.segments() {
            let bb = kurbo::Shape::bounding_box(&seg);
            let span = bb.width().max(bb.height());
            let acc = if span.is_finite() {
                (1e-4 * span).max(1e-4)
            } else {
                1e-4
            };
            let len = seg.arclen(acc);
            if len.is_finite() {
                total += len;
            }
        }
        total
    }

    /// Exact signed area enclosed by closed subpaths in square millimeters (`mm²`).
    #[must_use]
    pub fn signed_area_mm2(&self) -> f64 {
        let mut total = 0.0;
        for sp in &self.subpaths {
            if !sp.closed || sp.nodes.len() < 2 {
                continue;
            }
            let single = Self::from_subpath(sp.clone()).to_kurbo();
            let a = single.area();
            if a.is_finite() {
                total += a;
            }
        }
        total
    }

    /// Unsigned enclosed area (`signed_area_mm2().abs()`).
    #[must_use]
    pub fn area_mm2(&self) -> f64 {
        self.signed_area_mm2().abs()
    }

    /// Evaluates the point and unit tangent vector at arc-length distance `dist_mm`
    /// along the path (clamped to `[0, path_length_mm()]`). Powers Fit Text to Path.
    #[must_use]
    pub fn point_and_tangent_at(&self, dist_mm: f64) -> Option<(Point2D, Vec2D)> {
        if !dist_mm.is_finite() {
            return None;
        }
        let kb = self.to_kurbo();
        let segments: Vec<PathSeg> = kb.segments().collect();
        if segments.is_empty() {
            return None;
        }
        let mut remaining = dist_mm.max(0.0);
        let seg_acc = |seg: &PathSeg| -> f64 {
            let bb = kurbo::Shape::bounding_box(seg);
            let span = bb.width().max(bb.height());
            if span.is_finite() {
                (1e-4 * span).max(1e-4)
            } else {
                1e-4
            }
        };
        for seg in &segments {
            let acc = seg_acc(seg);
            let len = seg.arclen(acc);
            if !len.is_finite() || len <= 1e-12 {
                continue;
            }
            if remaining <= len {
                let t = seg.inv_arclen(remaining, acc).clamp(0.0, 1.0);
                return Some(eval_seg_point_tangent(*seg, t));
            }
            remaining -= len;
        }
        // Clamp to the end of the last non-degenerate segment
        segments
            .iter()
            .rev()
            .find(|seg| seg.arclen(seg_acc(seg)) > 1e-12)
            .map(|seg| eval_seg_point_tangent(*seg, 1.0))
    }

    /// Evaluates the point and unit tangent at normalized fraction `t` in `0.0..=1.0`.
    #[must_use]
    pub fn point_and_tangent_at_t(&self, t: f64) -> Option<(Point2D, Vec2D)> {
        if !t.is_finite() {
            return None;
        }
        let total = self.path_length_mm();
        self.point_and_tangent_at(total * t.clamp(0.0, 1.0))
    }

    /// Non-zero winding hit-test for closed contours.
    #[must_use]
    pub fn contains_point(&self, pt: Point2D) -> bool {
        if !pt.is_finite() || self.is_empty() || !self.subpaths.iter().any(|sp| sp.closed) {
            return false;
        }
        let mut kb = BezPath::new();
        for sp in self.subpaths.iter().filter(|sp| sp.closed) {
            push_subpath_to_kurbo(sp, &mut kb);
        }
        if kb.is_empty() {
            return false;
        }
        kb.winding(pt.to_kurbo()) != 0
    }

    // -------------------------------------------------------------------------
    // Node / Shape Editing Operations
    // -------------------------------------------------------------------------

    /// Inserts a new node on `(subpath_idx, seg_idx)` at curve parameter `t` (`0 < t < 1`),
    /// using exact de Casteljau subdivision so the path geometry does not change.
    /// Returns the index of the newly inserted node.
    pub fn add_node_at(&mut self, subpath_idx: usize, seg_idx: usize, t: f64) -> CoreResult<usize> {
        let t = sanitize_f64(t, 0.5).clamp(1e-4, 1.0 - 1e-4);
        let sp = self
            .subpaths
            .get_mut(subpath_idx)
            .ok_or_else(|| CoreError::InvalidParameter("subpath index out of range".into()))?;
        let (i0, i1) = sp
            .segment_node_indices(seg_idx)
            .ok_or_else(|| CoreError::InvalidParameter("segment index out of range".into()))?;

        let n0 = sp.nodes[i0];
        let n1 = sp.nodes[i1];
        let is_curve = n0.handle_out.is_some() || n1.handle_in.is_some();

        let (new_n0_out, new_node, new_n1_in) = if !is_curve {
            let mid = n0.pos.lerp(n1.pos, t);
            (None, PathNode::corner(mid), None)
        } else {
            let p0 = n0.pos;
            let p1 = n0.handle_out.unwrap_or(n0.pos);
            let p2 = n1.handle_in.unwrap_or(n1.pos);
            let p3 = n1.pos;
            // Exact de Casteljau cubic split
            let q0 = p0.lerp(p1, t);
            let q1 = p1.lerp(p2, t);
            let q2 = p2.lerp(p3, t);
            let r0 = q0.lerp(q1, t);
            let r1 = q1.lerp(q2, t);
            let s = r0.lerp(r1, t);
            (
                Some(q0),
                PathNode::new(s, Some(r0), Some(r1), NodeKind::Smooth),
                Some(q2),
            )
        };

        sp.nodes[i0].handle_out = new_n0_out;
        sp.nodes[i1].handle_in = new_n1_in;

        let insert_idx = if i1 == 0 { sp.nodes.len() } else { i1 };
        sp.nodes.insert(insert_idx, new_node);
        Ok(insert_idx)
    }

    /// Removes node `node_idx` from `subpath_idx`. Removes the subpath if it becomes empty.
    pub fn delete_node(&mut self, subpath_idx: usize, node_idx: usize) -> CoreResult<()> {
        let sp = self
            .subpaths
            .get_mut(subpath_idx)
            .ok_or_else(|| CoreError::InvalidParameter("subpath index out of range".into()))?;
        if node_idx >= sp.nodes.len() {
            return Err(CoreError::InvalidParameter(
                "node index out of range".into(),
            ));
        }
        sp.nodes.remove(node_idx);
        if sp.nodes.len() < 2 {
            sp.closed = false;
        }
        if sp.nodes.is_empty() {
            self.subpaths.remove(subpath_idx);
        }
        Ok(())
    }

    /// Joins two open endpoints:
    /// - If both nodes belong to the same open subpath (its start and end), closes the subpath.
    /// - If they belong to two different open subpaths, merges them into a single subpath.
    pub fn join_nodes(
        &mut self,
        sp_a: usize,
        node_a: usize,
        sp_b: usize,
        node_b: usize,
    ) -> CoreResult<()> {
        if sp_a >= self.subpaths.len() || sp_b >= self.subpaths.len() {
            return Err(CoreError::InvalidParameter(
                "subpath index out of range".into(),
            ));
        }
        if sp_a == sp_b {
            let sp = &mut self.subpaths[sp_a];
            if sp.closed || sp.nodes.len() < 2 {
                return Err(CoreError::InvalidGeometry(
                    "cannot join nodes on a closed or single-node subpath".into(),
                ));
            }
            let last = sp.nodes.len() - 1;
            if !((node_a == 0 && node_b == last) || (node_b == 0 && node_a == last)) {
                return Err(CoreError::InvalidGeometry(
                    "only opposite endpoints of an open subpath can be joined".into(),
                ));
            }
            let mid = sp.nodes[0].pos.midpoint(sp.nodes[last].pos);
            let last_node = sp.nodes.pop().expect("len >= 2");
            sp.nodes[0].pos = mid;
            if sp.nodes[0].handle_in.is_none() {
                sp.nodes[0].handle_in = last_node.handle_in;
            }
            sp.closed = true;
            return Ok(());
        }

        if self.subpaths[sp_a].closed || self.subpaths[sp_b].closed {
            return Err(CoreError::InvalidGeometry(
                "cannot join closed subpaths".into(),
            ));
        }

        let len_a = self.subpaths[sp_a].nodes.len();
        let len_b = self.subpaths[sp_b].nodes.len();
        if len_a == 0 || len_b == 0 {
            return Err(CoreError::InvalidGeometry("empty subpath".into()));
        }
        let a_is_start = node_a == 0;
        let a_is_end = node_a == len_a - 1;
        let b_is_start = node_b == 0;
        let b_is_end = node_b == len_b - 1;
        if !(a_is_start || a_is_end) || !(b_is_start || b_is_end) {
            return Err(CoreError::InvalidGeometry(
                "can only join endpoint nodes".into(),
            ));
        }

        // Remove the higher index first so lower index stays valid
        let (first_idx, second_idx, flip_first, flip_second) = if sp_a < sp_b {
            (sp_a, sp_b, a_is_start && !a_is_end, b_is_end && !b_is_start)
        } else {
            (sp_b, sp_a, b_is_start && !b_is_end, a_is_end && !a_is_start)
        };

        let mut second = self.subpaths.remove(second_idx);
        let first = &mut self.subpaths[first_idx];

        if flip_first {
            first.reverse();
        }
        if flip_second {
            second.reverse();
        }

        // Merge first.last() with second.first() at their midpoint
        let a_end = first.nodes.last().copied().expect("non-empty");
        let b_start = second.nodes[0];
        let joined_pos = a_end.pos.midpoint(b_start.pos);
        if let Some(last_mut) = first.nodes.last_mut() {
            last_mut.pos = joined_pos;
            last_mut.handle_out = b_start.handle_out;
        }
        first.nodes.extend(second.nodes.into_iter().skip(1));
        Ok(())
    }

    /// Breaks a subpath at `node_idx`:
    /// - Closed subpath -> opens into an open subpath starting and ending at `node_idx`.
    /// - Open interior node -> splits into two independent open subpaths sharing the split point.
    pub fn break_at_node(&mut self, subpath_idx: usize, node_idx: usize) -> CoreResult<()> {
        let sp = self
            .subpaths
            .get_mut(subpath_idx)
            .ok_or_else(|| CoreError::InvalidParameter("subpath index out of range".into()))?;
        if node_idx >= sp.nodes.len() {
            return Err(CoreError::InvalidParameter(
                "node index out of range".into(),
            ));
        }
        if sp.closed {
            sp.nodes.rotate_left(node_idx);
            let mut end_copy = sp.nodes[0];
            end_copy.handle_out = None;
            sp.nodes[0].handle_in = None;
            sp.nodes.push(end_copy);
            sp.closed = false;
            return Ok(());
        }
        if node_idx == 0 || node_idx == sp.nodes.len() - 1 {
            return Ok(());
        }
        let mut tail_nodes = sp.nodes.split_off(node_idx);
        let mut head_end = tail_nodes[0];
        head_end.handle_out = None;
        sp.nodes.push(head_end);
        tail_nodes[0].handle_in = None;
        let tail = Subpath {
            nodes: tail_nodes,
            closed: false,
        };
        self.subpaths.insert(subpath_idx + 1, tail);
        Ok(())
    }

    /// Sets the continuity kind of a node and updates its handles to satisfy it.
    pub fn set_node_kind(
        &mut self,
        subpath_idx: usize,
        node_idx: usize,
        kind: NodeKind,
    ) -> CoreResult<()> {
        let node = self
            .subpaths
            .get_mut(subpath_idx)
            .and_then(|sp| sp.nodes.get_mut(node_idx))
            .ok_or_else(|| CoreError::InvalidParameter("node index out of range".into()))?;
        node.kind = kind;
        node.enforce_continuity();
        Ok(())
    }

    /// Converts segment `(subpath_idx, seg_idx)` into a straight line by clearing its handles.
    pub fn convert_segment_to_line(
        &mut self,
        subpath_idx: usize,
        seg_idx: usize,
    ) -> CoreResult<()> {
        let sp = self
            .subpaths
            .get_mut(subpath_idx)
            .ok_or_else(|| CoreError::InvalidParameter("subpath index out of range".into()))?;
        let (i0, i1) = sp
            .segment_node_indices(seg_idx)
            .ok_or_else(|| CoreError::InvalidParameter("segment index out of range".into()))?;
        sp.nodes[i0].handle_out = None;
        sp.nodes[i1].handle_in = None;
        Ok(())
    }

    /// Converts straight segment `(subpath_idx, seg_idx)` into a cubic Bézier curve
    /// with handles placed at `1/3` and `2/3` along the chord.
    pub fn convert_segment_to_curve(
        &mut self,
        subpath_idx: usize,
        seg_idx: usize,
    ) -> CoreResult<()> {
        let sp = self
            .subpaths
            .get_mut(subpath_idx)
            .ok_or_else(|| CoreError::InvalidParameter("subpath index out of range".into()))?;
        let (i0, i1) = sp
            .segment_node_indices(seg_idx)
            .ok_or_else(|| CoreError::InvalidParameter("segment index out of range".into()))?;
        let p0 = sp.nodes[i0].pos;
        let p1 = sp.nodes[i1].pos;
        if sp.nodes[i0].handle_out.is_none() {
            sp.nodes[i0].handle_out = Some(p0.lerp(p1, 1.0 / 3.0));
        }
        if sp.nodes[i1].handle_in.is_none() {
            sp.nodes[i1].handle_in = Some(p0.lerp(p1, 2.0 / 3.0));
        }
        Ok(())
    }

    /// Closes an open subpath.
    pub fn auto_close(&mut self, subpath_idx: usize) -> CoreResult<()> {
        let sp = self
            .subpaths
            .get_mut(subpath_idx)
            .ok_or_else(|| CoreError::InvalidParameter("subpath index out of range".into()))?;
        if sp.nodes.len() >= 2 {
            sp.closed = true;
        }
        Ok(())
    }

    /// Reduces redundant nodes across all subpaths within `tolerance_mm` using
    /// Ramer–Douglas–Peucker simplification on linear runs and chord-deviation pruning.
    pub fn reduce_nodes(&mut self, tolerance_mm: f64) {
        let tol = sanitize_f64(tolerance_mm, 0.1).max(1e-4);
        for sp in &mut self.subpaths {
            if sp.nodes.len() <= 2 {
                continue;
            }
            let has_curves = sp
                .nodes
                .iter()
                .any(|n| n.handle_in.is_some() || n.handle_out.is_some());
            if !has_curves {
                let pts: Vec<Point2D> = sp.nodes.iter().map(|n| n.pos).collect();
                let mut simplified = rdp_simplify(&pts, tol);
                if sp.closed && simplified.len() > 3 {
                    // Also check wrap-around redundancy of first/last vertex
                    let first = simplified[0];
                    let last = simplified[simplified.len() - 1];
                    let second = simplified[1];
                    if point_to_segment_distance(first, last, second) < tol {
                        simplified.remove(0);
                    }
                }
                if simplified.len() >= 2 {
                    sp.nodes = simplified.into_iter().map(PathNode::corner).collect();
                }
            } else {
                // Remove collinear / near-redundant interior nodes whose anchor and handles
                // all lie within `tol` of the chord between its neighbors.
                let mut i = 1;
                while sp.nodes.len() > 3 && i + 1 < sp.nodes.len() {
                    let prev = sp.nodes[i - 1].pos;
                    let curr = sp.nodes[i];
                    let next = sp.nodes[i + 1].pos;
                    let anchor_ok = point_to_segment_distance(curr.pos, prev, next) <= tol;
                    let in_ok = curr
                        .handle_in
                        .is_none_or(|h| point_to_segment_distance(h, prev, next) <= tol);
                    let out_ok = curr
                        .handle_out
                        .is_none_or(|h| point_to_segment_distance(h, prev, next) <= tol);
                    if anchor_ok && in_ok && out_ok {
                        sp.nodes.remove(i);
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }

    /// Reverses the traversal direction of `subpath_idx`.
    pub fn reverse_subpath(&mut self, subpath_idx: usize) -> CoreResult<()> {
        let sp = self
            .subpaths
            .get_mut(subpath_idx)
            .ok_or_else(|| CoreError::InvalidParameter("subpath index out of range".into()))?;
        sp.reverse();
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Direct Sculpting Tools
    // -------------------------------------------------------------------------

    /// Smooths nodes within `radius` of `center` using smoothstep Laplacian relaxation.
    pub fn smooth_near(&mut self, center: Point2D, radius: f64, strength: f64) {
        if !center.is_finite() || !radius.is_finite() || radius <= 1e-6 {
            return;
        }
        let strength = sanitize_f64(strength, 0.5).clamp(0.0, 1.0);
        for sp in &mut self.subpaths {
            let n = sp.nodes.len();
            if n < 3 {
                continue;
            }
            let orig: Vec<PathNode> = sp.nodes.clone();
            for i in 0..n {
                if !sp.closed && (i == 0 || i == n - 1) {
                    continue;
                }
                let d = orig[i].pos.distance_to(center);
                if d >= radius {
                    continue;
                }
                let w = brush_falloff(d / radius) * strength;
                let prev = orig[(i + n - 1) % n].pos;
                let next = orig[(i + 1) % n].pos;
                let avg = prev.midpoint(next);
                let delta = (avg - orig[i].pos) * w;
                sp.nodes[i].translate(delta);
                // Recompute gentle smooth handles along neighbor chord
                let tangent = (next - prev) * (0.18 * w);
                if tangent.length() > 1e-6 {
                    sp.nodes[i].handle_in = Some(sp.nodes[i].pos - tangent);
                    sp.nodes[i].handle_out = Some(sp.nodes[i].pos + tangent);
                    sp.nodes[i].kind = NodeKind::Smooth;
                }
            }
        }
    }

    /// Smears (pushes/pulls) path nodes near `center` along `delta` with smooth falloff.
    pub fn smear_near(&mut self, center: Point2D, delta: Vec2D, radius: f64) {
        if !center.is_finite() || !delta.is_finite() || !radius.is_finite() || radius <= 1e-6 {
            return;
        }
        self.subdivide_near(center, radius, radius * 0.35);
        for sp in &mut self.subpaths {
            for node in &mut sp.nodes {
                let d = node.pos.distance_to(center);
                if d < radius {
                    let w = brush_falloff(d / radius);
                    node.translate(delta * w);
                }
            }
        }
    }

    /// Twirls nodes and handles around `center` by up to `angle_rad` radians.
    pub fn twirl_near(&mut self, center: Point2D, radius: f64, angle_rad: f64) {
        if !center.is_finite() || !radius.is_finite() || !angle_rad.is_finite() || radius <= 1e-6 {
            return;
        }
        self.subdivide_near(center, radius, radius * 0.3);
        let twirl_pt = |p: Point2D| {
            let d = p.distance_to(center);
            if d < radius {
                let theta = angle_rad * brush_falloff(d / radius);
                let rel = p - center;
                center + rel.rotate(theta)
            } else {
                p
            }
        };
        for sp in &mut self.subpaths {
            for node in &mut sp.nodes {
                node.pos = twirl_pt(node.pos);
                node.handle_in = node.handle_in.map(twirl_pt);
                node.handle_out = node.handle_out.map(twirl_pt);
            }
        }
    }

    /// Attracts (`attract = true`) or repels (`attract = false`) nodes near `center`.
    pub fn attract_repel_near(
        &mut self,
        center: Point2D,
        radius: f64,
        strength: f64,
        attract: bool,
    ) {
        if !center.is_finite() || !radius.is_finite() || radius <= 1e-6 {
            return;
        }
        let strength = sanitize_f64(strength, 0.3).clamp(0.0, 1.0);
        self.subdivide_near(center, radius, radius * 0.35);
        for sp in &mut self.subpaths {
            for node in &mut sp.nodes {
                let offset = center - node.pos;
                let d = offset.length();
                if d > 1e-9 && d < radius {
                    let w = brush_falloff(d / radius) * strength;
                    let delta = if attract {
                        offset * w
                    } else {
                        (-offset / d) * ((radius - d) * w)
                    };
                    node.translate(delta);
                }
            }
        }
    }

    /// Applies deterministic jagged/sawtooth displacement to segments within `radius` of `center`.
    pub fn roughen_near(&mut self, center: Point2D, radius: f64, amplitude: f64, seed: u64) {
        if !center.is_finite()
            || !radius.is_finite()
            || !amplitude.is_finite()
            || radius <= 1e-6
            || amplitude.abs() <= 1e-9
        {
            return;
        }
        self.subdivide_near(center, radius, (radius * 0.2).max(0.5));
        for (sp_idx, sp) in self.subpaths.iter_mut().enumerate() {
            let n = sp.nodes.len();
            if n < 2 {
                continue;
            }
            let positions: Vec<Point2D> = sp.nodes.iter().map(|node| node.pos).collect();
            for (i, node) in sp.nodes.iter_mut().enumerate() {
                let d = positions[i].distance_to(center);
                if d >= radius {
                    continue;
                }
                let prev = positions[if i == 0 {
                    if sp.closed {
                        n - 1
                    } else {
                        0
                    }
                } else {
                    i - 1
                }];
                let next = positions[if i + 1 == n {
                    if sp.closed {
                        0
                    } else {
                        n - 1
                    }
                } else {
                    i + 1
                }];
                let normal = (next - prev)
                    .perp()
                    .normalized()
                    .unwrap_or(Vec2D::new(0.0, 1.0));
                let hash = splitmix64(
                    seed ^ ((sp_idx as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
                        ^ (i as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9),
                );
                let signed_unit = ((hash as f64) / (u64::MAX as f64)) * 2.0 - 1.0;
                let w = brush_falloff(d / radius);
                node.translate(normal * (amplitude * w * signed_unit));
            }
        }
    }

    /// Subdivides segments near `center` whose chord exceeds `max_seg_len` so sculpting brushes
    /// have enough vertices to deform smoothly.
    fn subdivide_near(&mut self, center: Point2D, radius: f64, max_seg_len: f64) {
        let max_len = max_seg_len.max(0.25);
        for sp_idx in 0..self.subpaths.len() {
            let mut safety = 0usize;
            while safety < 64 {
                safety += 1;
                let candidate = {
                    let sp = &self.subpaths[sp_idx];
                    let mut found = None;
                    for s in 0..sp.segment_count() {
                        if let Some((i0, i1)) = sp.segment_node_indices(s) {
                            let p0 = sp.nodes[i0].pos;
                            let p1 = sp.nodes[i1].pos;
                            if p0.distance_to(p1) > max_len
                                && point_to_segment_distance(center, p0, p1) < radius
                            {
                                found = Some(s);
                                break;
                            }
                        }
                    }
                    found
                };
                if let Some(seg) = candidate {
                    let _ = self.add_node_at(sp_idx, seg, 0.5);
                } else {
                    break;
                }
            }
        }
    }
}

#[inline]
fn brush_falloff(norm_dist: f64) -> f64 {
    let u = (1.0 - norm_dist.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

#[inline]
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn push_subpath_to_kurbo(sp: &Subpath, out: &mut BezPath) {
    if sp.nodes.is_empty() {
        return;
    }
    let first = sp.nodes[0];
    if !first.pos.is_finite() {
        return;
    }
    out.move_to(first.pos.to_kurbo());
    let seg_count = sp.segment_count();
    for s in 0..seg_count {
        if let Some(seg) = sp.segment_to_kurbo(s) {
            match seg {
                PathSeg::Line(l) => out.line_to(l.p1),
                PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
                PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
            }
        }
    }
    if sp.closed {
        out.close_path();
    }
}

fn eval_seg_point_tangent(seg: PathSeg, t: f64) -> (Point2D, Vec2D) {
    let pt = Point2D::from_kurbo(seg.eval(t));
    let deriv = match seg {
        PathSeg::Line(l) => Vec2D::from_kurbo(l.p1 - l.p0),
        PathSeg::Quad(q) => Vec2D::from_kurbo(q.deriv().eval(t).to_vec2()),
        PathSeg::Cubic(c) => {
            let d = Vec2D::from_kurbo(c.deriv().eval(t).to_vec2());
            if d.length() > 1e-9 {
                d
            } else {
                // Fallback for zero-velocity cusp endpoint
                let eps = if t < 0.5 { 1e-3 } else { -1e-3 };
                Point2D::from_kurbo(c.eval((t + eps).clamp(0.0, 1.0))) - pt
            }
        }
    };
    let tangent = deriv.normalized().unwrap_or(Vec2D::new(1.0, 0.0));
    (pt, tangent)
}

/// Shortest Euclidean distance from point `p` to line segment `[a, b]`.
#[must_use]
pub fn point_to_segment_distance(p: Point2D, a: Point2D, b: Point2D) -> f64 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= 1e-12 {
        return p.distance_to(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let proj = a + ab * t;
    p.distance_to(proj)
}

fn rdp_simplify(pts: &[Point2D], tol: f64) -> Vec<Point2D> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let first = pts[0];
    let last = pts[pts.len() - 1];
    let mut max_dist = 0.0;
    let mut split_idx = 0;
    for (i, &p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let d = point_to_segment_distance(p, first, last);
        if d > max_dist {
            max_dist = d;
            split_idx = i;
        }
    }
    if max_dist > tol && split_idx > 0 {
        let mut left = rdp_simplify(&pts[..=split_idx], tol);
        let right = rdp_simplify(&pts[split_idx..], tol);
        left.pop();
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

// =============================================================================
// Knife, Virtual Segment Delete, and Eraser Geometry Operations
// =============================================================================

/// Maximum coordinate magnitude passed to `i_overlay`'s float-based polygon engines.
const MAX_OVERLAY_FLOAT_COORD: f64 = 1e12;

fn point_in_overlay_range(p: Point2D) -> bool {
    p.is_finite() && p.x.abs() <= MAX_OVERLAY_FLOAT_COORD && p.y.abs() <= MAX_OVERLAY_FLOAT_COORD
}

/// Slices `path` along the line segment `[line_a, line_b]`.
///
/// - When `auto_close` is `true` and subpaths are closed, slices polygons via
///   `i_overlay` into independent closed regions.
/// - For open subpaths (or when `auto_close` is `false`), splits curves cleanly
///   at exact line-curve intersections.
#[must_use]
pub fn knife_cut(
    path: &BezierPath,
    line_a: Point2D,
    line_b: Point2D,
    auto_close: bool,
) -> Vec<BezierPath> {
    if path.is_empty()
        || !point_in_overlay_range(line_a)
        || !point_in_overlay_range(line_b)
        || line_a.distance_to(line_b) < 1e-6
    {
        return if path.is_empty() {
            Vec::new()
        } else {
            vec![path.clone()]
        };
    }

    let all_closed = path.subpaths.iter().all(|sp| sp.closed);
    if auto_close && all_closed {
        let contours: Vec<Vec<[f64; 2]>> = path
            .flatten(0.02)
            .into_iter()
            .filter_map(|poly| {
                if !poly.iter().all(|&p| point_in_overlay_range(p)) {
                    return None;
                }
                let mut ring: Vec<[f64; 2]> = poly.into_iter().map(|p| [p.x, p.y]).collect();
                if ring.len() > 1 && ring.first() == ring.last() {
                    ring.pop();
                }
                (ring.len() >= 3).then_some(ring)
            })
            .collect();
        if !contours.is_empty() {
            let cut_line = vec![[line_a.x, line_a.y], [line_b.x, line_b.y]];
            let sliced = contours.slice_by_as::<i64>(&cut_line, FillRule::EvenOdd);
            if !sliced.is_empty() {
                return sliced
                    .into_iter()
                    .map(|shape| {
                        let subpaths = shape
                            .into_iter()
                            .filter(|c| c.len() >= 3)
                            .map(|c| {
                                let pts: Vec<Point2D> =
                                    c.into_iter().map(|[x, y]| Point2D::new(x, y)).collect();
                                fit_cubic_bezier_subpath(&pts, true, 0.025)
                            })
                            .collect();
                        BezierPath { subpaths }
                    })
                    .filter(|p| !p.is_empty())
                    .collect();
            }
        }
        return vec![path.clone()];
    }

    // Open-path or non-auto-close curve splitting via exact kurbo line-segment intersections
    let cut_kurbo = Line::new(line_a.to_kurbo(), line_b.to_kurbo());
    let mut results = Vec::new();

    for sp in &path.subpaths {
        let seg_count = sp.segment_count();
        if seg_count == 0 {
            continue;
        }
        let mut pieces: Vec<Vec<PathSeg>> = Vec::new();
        let mut current: Vec<PathSeg> = Vec::new();
        let mut had_cut = false;
        let mut cut_at_seam = false;

        for s in 0..seg_count {
            let Some(seg) = sp.segment_to_kurbo(s) else {
                continue;
            };
            let mut hits: Vec<f64> = seg
                .intersect_line(cut_kurbo)
                .into_iter()
                .filter(|ix| {
                    ix.line_t >= -1e-6
                        && ix.line_t <= 1.0 + 1e-6
                        && ix.segment_t >= -1e-5
                        && ix.segment_t <= 1.0 + 1e-5
                })
                .map(|ix| ix.segment_t.clamp(0.0, 1.0))
                .collect();
            hits.sort_by(|a, b| a.total_cmp(b));
            hits.dedup_by(|a, b| (*a - *b).abs() < 1e-5);

            if hits.is_empty() {
                current.push(seg);
            } else {
                let mut prev_t = 0.0;
                for t in hits {
                    if t <= 1e-5 {
                        // Cut right through the start vertex of segment `s`
                        if !current.is_empty() {
                            pieces.push(std::mem::take(&mut current));
                            had_cut = true;
                        } else if sp.closed && s == 0 {
                            cut_at_seam = true;
                            had_cut = true;
                        }
                    } else if t >= 1.0 - 1e-5 {
                        // Cut right through the end vertex of segment `s`
                        if 1.0 - prev_t > 1e-5 {
                            current.push(seg.subsegment(prev_t..1.0));
                        }
                        if sp.closed || s + 1 < seg_count {
                            if !current.is_empty() {
                                pieces.push(std::mem::take(&mut current));
                                had_cut = true;
                            }
                            if sp.closed && s + 1 == seg_count {
                                cut_at_seam = true;
                            }
                        }
                        prev_t = 1.0;
                    } else {
                        had_cut = true;
                        current.push(seg.subsegment(prev_t..t));
                        pieces.push(std::mem::take(&mut current));
                        prev_t = t;
                    }
                }
                if 1.0 - prev_t > 1e-5 {
                    current.push(seg.subsegment(prev_t..1.0));
                }
            }
        }

        if !current.is_empty() {
            // If closed subpath was cut (and NOT cut at the start/end seam), stitch final tail onto piece 0
            if sp.closed && had_cut && !cut_at_seam && !pieces.is_empty() {
                let mut combined = current;
                combined.extend(pieces.remove(0));
                pieces.insert(0, combined);
            } else {
                pieces.push(current);
            }
        }

        if !had_cut {
            results.push(BezierPath::from_subpath(sp.clone()));
        } else {
            for segs in pieces {
                let mut kb = BezPath::new();
                for (idx, seg) in segs.iter().enumerate() {
                    if idx == 0 {
                        kb.move_to(seg.start());
                    }
                    kb.push(seg.as_path_el());
                }
                if auto_close && segs.len() >= 2 {
                    kb.close_path();
                }
                let bp = BezierPath::from_kurbo(&kb);
                if !bp.is_empty() {
                    results.push(bp);
                }
            }
        }
    }

    if results.is_empty() {
        vec![path.clone()]
    } else {
        results
    }
}

/// Deletes the single virtual segment of `target` closest to `click_pt` (within `tolerance`)
/// bounded by intersections with `cutters` (or `target`'s own vertices/self-intersections).
#[must_use]
pub fn virtual_segment_delete(
    target: &BezierPath,
    cutters: &[BezierPath],
    click_pt: Point2D,
    tolerance: f64,
) -> BezierPath {
    if target.is_empty() || !click_pt.is_finite() || !tolerance.is_finite() || tolerance <= 0.0 {
        return target.clone();
    }

    // Collect cutter polylines as linear segments
    let mut cutter_edges: Vec<(Point2D, Point2D)> = Vec::new();
    for cutter in cutters {
        for poly in cutter.flatten(0.02) {
            for win in poly.windows(2) {
                if win[0].distance_to(win[1]) > 1e-6 {
                    cutter_edges.push((win[0], win[1]));
                }
            }
        }
    }

    // Flatten target subpaths while preserving original subpath indices
    let target_polys: Vec<(usize, bool, Vec<Point2D>)> = target
        .subpaths
        .iter()
        .enumerate()
        .filter_map(|(orig_idx, sp)| {
            let mut poly = BezierPath::from_subpath(sp.clone())
                .flatten(0.02)
                .into_iter()
                .next()?;
            if poly.len() < 2 {
                return None;
            }
            if sp.closed
                && poly
                    .first()
                    .zip(poly.last())
                    .is_some_and(|(a, b)| a.distance_to(*b) > 1e-6)
            {
                poly.push(poly[0]);
            }
            Some((orig_idx, sp.closed, poly))
        })
        .collect();

    // Locate which original subpath & linear arc-length position was clicked
    let mut best_hit: Option<(usize, f64, f64)> = None; // (orig_sp_idx, dist_to_click, arc_pos)
    for (orig_idx, _, poly) in &target_polys {
        let mut accum = 0.0;
        for win in poly.windows(2) {
            let a = win[0];
            let b = win[1];
            let seg_len = a.distance_to(b);
            if seg_len <= 1e-9 {
                continue;
            }
            let ab = b - a;
            let t = ((click_pt - a).dot(ab) / (seg_len * seg_len)).clamp(0.0, 1.0);
            let proj = a + ab * t;
            let d = click_pt.distance_to(proj);
            if d <= tolerance && best_hit.is_none_or(|(_, bd, _)| d < bd) {
                best_hit = Some((*orig_idx, d, accum + t * seg_len));
            }
            accum += seg_len;
        }
    }

    let Some((hit_sp_idx, _, click_arc)) = best_hit else {
        return target.clone();
    };

    let mut out_subpaths = Vec::new();
    let hit_poly = target_polys
        .into_iter()
        .find(|(idx, _, _)| *idx == hit_sp_idx)
        .map(|(_, closed, poly)| (closed, poly));

    for (orig_idx, orig_sp) in target.subpaths.iter().enumerate() {
        if orig_idx != hit_sp_idx {
            out_subpaths.push(orig_sp.clone());
            continue;
        }
        let Some((closed, poly)) = hit_poly.as_ref() else {
            out_subpaths.push(orig_sp.clone());
            continue;
        };
        let closed = *closed;

        // Compute total perimeter and all intersection arc positions on this subpath
        let num_edges = poly.len().saturating_sub(1);
        let mut total_len = 0.0;
        let mut cut_arcs: Vec<f64> = Vec::new();
        for (edge_idx, win) in poly.windows(2).enumerate() {
            let a = win[0];
            let b = win[1];
            let seg_len = a.distance_to(b);
            if seg_len <= 1e-9 {
                continue;
            }
            for &(c0, c1) in &cutter_edges {
                if let Some((ta, _)) = segment_segment_intersection(a, b, c0, c1) {
                    cut_arcs.push(total_len + ta * seg_len);
                }
            }
            // Self-intersections with non-adjacent edges of the same subpath (including symmetric wrap-around)
            for (other_idx, owin) in poly.windows(2).enumerate() {
                if other_idx.abs_diff(edge_idx) <= 1 {
                    continue;
                }
                if closed
                    && ((edge_idx == 0 && other_idx + 1 == num_edges)
                        || (other_idx == 0 && edge_idx + 1 == num_edges))
                {
                    continue;
                }
                if let Some((ta, _)) = segment_segment_intersection(a, b, owin[0], owin[1]) {
                    cut_arcs.push(total_len + ta * seg_len);
                }
            }
            total_len += seg_len;
        }

        cut_arcs.sort_by(|a, b| a.total_cmp(b));
        cut_arcs.dedup_by(|a, b| (*a - *b).abs() < 1e-4);

        if !closed {
            // Find bounding cuts around click_arc (defaulting to 0..total_len)
            let prev_cut = cut_arcs
                .iter()
                .copied()
                .rev()
                .find(|&c| c < click_arc - 1e-4)
                .unwrap_or(0.0);
            let next_cut = cut_arcs
                .iter()
                .copied()
                .find(|&c| c > click_arc + 1e-4)
                .unwrap_or(total_len);

            if prev_cut > 1e-4 {
                let left = extract_polyline_range(poly, 0.0, prev_cut);
                if left.len() >= 2 {
                    out_subpaths.push(fit_cubic_bezier_subpath(&left, false, 0.025));
                }
            }
            if total_len - next_cut > 1e-4 {
                let right = extract_polyline_range(poly, next_cut, total_len);
                if right.len() >= 2 {
                    out_subpaths.push(fit_cubic_bezier_subpath(&right, false, 0.025));
                }
            }
        } else if cut_arcs.len() >= 2 {
            // Find the interval [prev_cut, next_cut] wrapping around the loop that contains click_arc
            let next_idx = cut_arcs
                .iter()
                .position(|&c| c > click_arc + 1e-4)
                .unwrap_or(0);
            let prev_idx = if next_idx == 0 {
                cut_arcs.len() - 1
            } else {
                next_idx - 1
            };
            let prev_cut = cut_arcs[prev_idx];
            let next_cut = cut_arcs[next_idx];

            // Keep the complement: from next_cut around to prev_cut
            let mut kept = Vec::new();
            if next_cut <= prev_cut {
                kept = extract_polyline_range(poly, next_cut, prev_cut);
            } else {
                let part1 = extract_polyline_range(poly, next_cut, total_len);
                let part2 = extract_polyline_range(poly, 0.0, prev_cut);
                kept.extend(part1);
                for pt in part2 {
                    if kept.last().is_none_or(|last| last.distance_to(pt) > 1e-5) {
                        kept.push(pt);
                    }
                }
            }
            if kept.len() >= 2 {
                out_subpaths.push(fit_cubic_bezier_subpath(&kept, false, 0.025));
            }
        }
        // If closed with < 2 intersections, deleting the clicked virtual segment removes the loop
    }

    BezierPath {
        subpaths: out_subpaths,
    }
}

/// Exact Euclidean minimum distance from `pt` to a `kurbo::PathSeg` (line, quad, or cubic Bézier).
#[must_use]
pub fn segment_min_dist_to_point(seg: PathSeg, pt: Point2D) -> f64 {
    if !pt.is_finite() {
        return f64::INFINITY;
    }
    match seg {
        PathSeg::Line(l) => {
            point_to_segment_distance(pt, Point2D::from_kurbo(l.p0), Point2D::from_kurbo(l.p1))
        }
        PathSeg::Quad(_) | PathSeg::Cubic(_) => {
            const SAMPLES: usize = 32;
            let mut best_t = 0.0_f64;
            let mut best_d2 = f64::INFINITY;
            for i in 0..=SAMPLES {
                let t = (i as f64) / (SAMPLES as f64);
                let p = Point2D::from_kurbo(seg.eval(t));
                let d2 = p.distance_sq(pt);
                if d2 < best_d2 {
                    best_d2 = d2;
                    best_t = t;
                }
            }
            // Golden-section / ternary refinement around best sample
            let step = 1.0 / (SAMPLES as f64);
            let mut lo = (best_t - step).max(0.0);
            let mut hi = (best_t + step).min(1.0);
            for _ in 0..24 {
                let m1 = lo + (hi - lo) / 3.0;
                let m2 = hi - (hi - lo) / 3.0;
                let d1 = Point2D::from_kurbo(seg.eval(m1)).distance_sq(pt);
                let d2 = Point2D::from_kurbo(seg.eval(m2)).distance_sq(pt);
                if d1 < d2 {
                    hi = m2;
                } else {
                    lo = m1;
                }
            }
            let mid = 0.5 * (lo + hi);
            let refined = Point2D::from_kurbo(seg.eval(mid)).distance_sq(pt);
            best_d2.min(refined).sqrt()
        }
    }
}

/// Returns `true` if `pt` lies inside the compound region defined by `closed_subpaths`
/// under the `EvenOdd` fill rule (evaluated analytically over exact Bézier segments).
#[must_use]
pub fn closed_subpaths_contain_even_odd(closed_subpaths: &[Subpath], pt: Point2D) -> bool {
    if !pt.is_finite() {
        return false;
    }
    let mut kb = BezPath::new();
    let mut any = false;
    for sp in closed_subpaths {
        if sp.closed && sp.nodes.len() >= 2 {
            push_subpath_to_kurbo(sp, &mut kb);
            any = true;
        }
    }
    if !any || kb.is_empty() {
        return false;
    }
    kb.winding(pt.to_kurbo()).rem_euclid(2) != 0
}

fn segment_kurbo_circle_intersections(seg: PathSeg, center: Point2D, radius: f64) -> Vec<f64> {
    match seg {
        PathSeg::Line(l) => segment_circle_intersections(
            Point2D::from_kurbo(l.p0),
            Point2D::from_kurbo(l.p1),
            center,
            radius,
        ),
        PathSeg::Quad(_) | PathSeg::Cubic(_) => {
            const SAMPLES: usize = 32;
            let r2 = radius * radius;
            let f = |t: f64| -> f64 {
                let p = Point2D::from_kurbo(seg.eval(t));
                p.distance_sq(center) - r2
            };
            let mut roots: Vec<f64> = Vec::new();
            let mut ts = [0.0_f64; SAMPLES + 1];
            let mut fs = [0.0_f64; SAMPLES + 1];
            for i in 0..=SAMPLES {
                let t = (i as f64) / (SAMPLES as f64);
                ts[i] = t;
                fs[i] = f(t);
            }
            let refine_bisect = |mut a: f64, mut b: f64, mut fa: f64, mut fb: f64| -> f64 {
                for _ in 0..42 {
                    let mid = 0.5 * (a + b);
                    let fm = f(mid);
                    if fm.abs() <= 1e-12 || (b - a) <= 1e-11 {
                        return mid;
                    }
                    if (fa <= 0.0 && fm <= 0.0) || (fa >= 0.0 && fm >= 0.0) {
                        a = mid;
                        fa = fm;
                    } else {
                        b = mid;
                        fb = fm;
                    }
                    let _ = fb;
                }
                0.5 * (a + b)
            };
            for i in 0..SAMPLES {
                let a = ts[i];
                let b = ts[i + 1];
                let fa = fs[i];
                let fb = fs[i + 1];
                if fa.abs() <= 1e-10 {
                    roots.push(a);
                } else if (fa < 0.0 && fb > 0.0) || (fa > 0.0 && fb < 0.0) {
                    roots.push(refine_bisect(a, b, fa, fb));
                } else if fa > 0.0 && fb > 0.0 {
                    // Check for interior grazing tangency where f dips <= 0 inside [a, b]
                    let mut lo = a;
                    let mut hi = b;
                    for _ in 0..24 {
                        let m1 = lo + (hi - lo) / 3.0;
                        let m2 = hi - (hi - lo) / 3.0;
                        if f(m1) < f(m2) {
                            hi = m2;
                        } else {
                            lo = m1;
                        }
                    }
                    let t_min = 0.5 * (lo + hi);
                    let f_min = f(t_min);
                    if f_min <= 1e-7 {
                        if f_min >= -1e-7 {
                            roots.push(t_min);
                        } else {
                            roots.push(refine_bisect(a, t_min, fa, f_min));
                            roots.push(refine_bisect(t_min, b, f_min, fb));
                        }
                    }
                }
            }
            if fs[SAMPLES].abs() <= 1e-10 {
                roots.push(1.0);
            }
            roots.retain(|t| *t > 1e-6 && *t < 1.0 - 1e-6);
            roots.sort_by(|a, b| a.total_cmp(b));
            roots.dedup_by(|a, b| (*a - *b).abs() < 1e-5);
            roots
        }
    }
}

fn seg_reverse(seg: PathSeg) -> PathSeg {
    match seg {
        PathSeg::Line(l) => PathSeg::Line(Line::new(l.p1, l.p0)),
        PathSeg::Quad(q) => PathSeg::Quad(kurbo::QuadBez::new(q.p2, q.p1, q.p0)),
        PathSeg::Cubic(c) => PathSeg::Cubic(CubicBez::new(c.p3, c.p2, c.p1, c.p0)),
    }
}

fn circle_arc_to_cubic_segs(
    center: Point2D,
    radius: f64,
    start_angle: f64,
    end_angle: f64,
) -> Vec<PathSeg> {
    let sweep = end_angle - start_angle;
    if sweep.abs() <= 1e-8 {
        return Vec::new();
    }
    let count = ((sweep.abs() / FRAC_PI_2).ceil() as usize).clamp(1, 8);
    let step = sweep / (count as f64);
    let alpha = (4.0 / 3.0) * (step * 0.25).tan();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let a0 = start_angle + (i as f64) * step;
        let a1 = a0 + step;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let p0 = kurbo::Point::new(center.x + radius * c0, center.y + radius * s0);
        let p3 = kurbo::Point::new(center.x + radius * c1, center.y + radius * s1);
        let p1 = kurbo::Point::new(p0.x - alpha * radius * s0, p0.y + alpha * radius * c0);
        let p2 = kurbo::Point::new(p3.x + alpha * radius * s1, p3.y - alpha * radius * c1);
        out.push(PathSeg::Cubic(CubicBez::new(p0, p1, p2, p3)));
    }
    out
}

fn segs_to_bezier_subpath(segs: &[PathSeg], closed: bool) -> Option<Subpath> {
    if segs.is_empty() {
        return None;
    }
    let mut kb = BezPath::new();
    kb.move_to(segs[0].start());
    for seg in segs {
        kb.push(seg.as_path_el());
    }
    if closed {
        kb.close_path();
    }
    let mut bp = BezierPath::from_kurbo(&kb);
    bp.subpaths.pop().filter(|sp| sp.nodes.len() >= 2)
}

fn exact_circle_subpath(center: Point2D, radius: f64, reverse_winding: bool) -> Subpath {
    let k = radius * CIRCLE_KAPPA;
    let nodes = vec![
        PathNode::new(
            Point2D::new(center.x + radius, center.y),
            Some(Point2D::new(center.x + radius, center.y - k)),
            Some(Point2D::new(center.x + radius, center.y + k)),
            NodeKind::Symmetrical,
        ),
        PathNode::new(
            Point2D::new(center.x, center.y + radius),
            Some(Point2D::new(center.x + k, center.y + radius)),
            Some(Point2D::new(center.x - k, center.y + radius)),
            NodeKind::Symmetrical,
        ),
        PathNode::new(
            Point2D::new(center.x - radius, center.y),
            Some(Point2D::new(center.x - radius, center.y + k)),
            Some(Point2D::new(center.x - radius, center.y - k)),
            NodeKind::Symmetrical,
        ),
        PathNode::new(
            Point2D::new(center.x, center.y - radius),
            Some(Point2D::new(center.x - k, center.y - radius)),
            Some(Point2D::new(center.x + k, center.y - radius)),
            NodeKind::Symmetrical,
        ),
    ];
    let mut sp = Subpath {
        nodes,
        closed: true,
    };
    if reverse_winding {
        sp.reverse();
    }
    sp
}

/// Erases all geometry of `path` falling inside the disk `(center, radius)`.
///
/// Uses analytical `PathSeg` vs. circle intersections and exact circular cubic Bézier arcs
/// (`CIRCLE_KAPPA` / `4/3 * tan(dtheta/4)`) with zero polygon approximation.
#[must_use]
pub fn erase_circle(path: &BezierPath, center: Point2D, radius: f64) -> Vec<BezierPath> {
    if path.is_empty()
        || !point_in_overlay_range(center)
        || !radius.is_finite()
        || radius <= 1e-6
        || radius > MAX_OVERLAY_FLOAT_COORD * 0.5
    {
        return if path.is_empty() {
            Vec::new()
        } else {
            vec![path.clone()]
        };
    }

    let mut results = Vec::new();
    let mut closed_subpaths: Vec<Subpath> = Vec::new();

    for sp in &path.subpaths {
        if sp.closed {
            if sp.nodes.len() >= 3 && sp.nodes.iter().all(|n| point_in_overlay_range(n.pos)) {
                closed_subpaths.push(sp.clone());
            }
        } else {
            // Exact open-curve trimming against disk `(center, radius)`
            let seg_count = sp.segment_count();
            let mut active_segs: Vec<PathSeg> = Vec::new();
            let flush_open = |segs: &mut Vec<PathSeg>, out: &mut Vec<BezierPath>| {
                if let Some(sub) = segs_to_bezier_subpath(segs, false) {
                    out.push(BezierPath::from_subpath(sub));
                }
                segs.clear();
            };

            for s in 0..seg_count {
                let Some(seg) = sp.segment_to_kurbo(s) else {
                    continue;
                };
                let mut ts = vec![0.0, 1.0];
                ts.extend(segment_kurbo_circle_intersections(seg, center, radius));
                ts.sort_by(|x, y| x.total_cmp(y));
                ts.dedup_by(|x, y| (*x - *y).abs() < 1e-6);

                for pair in ts.windows(2) {
                    let t0 = pair[0];
                    let t1 = pair[1];
                    if t1 - t0 < 1e-7 {
                        continue;
                    }
                    let piece = seg.subsegment(t0..t1);
                    let mid = Point2D::from_kurbo(piece.eval(0.5));
                    if mid.distance_to(center) > radius + 1e-6 {
                        let piece_start = Point2D::from_kurbo(piece.start());
                        if let Some(prev) = active_segs.last() {
                            let prev_end = Point2D::from_kurbo(prev.end());
                            if prev_end.distance_to(piece_start) > 1e-4 {
                                flush_open(&mut active_segs, &mut results);
                            }
                        }
                        active_segs.push(piece);
                    } else {
                        flush_open(&mut active_segs, &mut results);
                    }
                }
            }
            flush_open(&mut active_segs, &mut results);
        }
    }

    if !closed_subpaths.is_empty() {
        let mut untouched_outside: Vec<Subpath> = Vec::new();
        let mut boundary_segs: Vec<PathSeg> = Vec::new();
        let mut hit_angles: Vec<f64> = Vec::new();

        for sp in &closed_subpaths {
            let seg_count = sp.segment_count();
            let mut sp_has_hit = false;
            let mut sp_all_inside = true;
            let mut sp_kept: Vec<PathSeg> = Vec::new();

            for s in 0..seg_count {
                let Some(seg) = sp.segment_to_kurbo(s) else {
                    continue;
                };
                let min_d = segment_min_dist_to_point(seg, center);
                let mut ts = vec![0.0, 1.0];
                let hits = segment_kurbo_circle_intersections(seg, center, radius);
                if !hits.is_empty() || min_d <= radius + 1e-5 {
                    sp_has_hit = true;
                }
                for &t in &hits {
                    let hp = Point2D::from_kurbo(seg.eval(t));
                    hit_angles.push((hp.y - center.y).atan2(hp.x - center.x));
                }
                ts.extend(hits);
                ts.sort_by(|x, y| x.total_cmp(y));
                ts.dedup_by(|x, y| (*x - *y).abs() < 1e-6);

                for pair in ts.windows(2) {
                    let t0 = pair[0];
                    let t1 = pair[1];
                    if t1 - t0 < 1e-7 {
                        continue;
                    }
                    let piece = seg.subsegment(t0..t1);
                    let mid = Point2D::from_kurbo(piece.eval(0.5));
                    if mid.distance_to(center) > radius + 1e-6 {
                        sp_all_inside = false;
                        let ps = Point2D::from_kurbo(piece.start());
                        let pe = Point2D::from_kurbo(piece.end());
                        if (ps.distance_to(center) - radius).abs() <= 1e-4 {
                            hit_angles.push((ps.y - center.y).atan2(ps.x - center.x));
                        }
                        if (pe.distance_to(center) - radius).abs() <= 1e-4 {
                            hit_angles.push((pe.y - center.y).atan2(pe.x - center.x));
                        }
                        sp_kept.push(piece);
                    }
                }
            }

            if !sp_has_hit {
                if !sp_all_inside {
                    untouched_outside.push(sp.clone());
                }
            } else if !sp_all_inside {
                boundary_segs.extend(sp_kept);
            }
        }

        // Normalize and deduplicate angular intersection points on circle
        for a in &mut hit_angles {
            *a = a.rem_euclid(TAU);
        }
        hit_angles.sort_by(|a, b| a.total_cmp(b));
        hit_angles.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        if hit_angles.len() >= 2
            && (hit_angles[0] + TAU - hit_angles[hit_angles.len() - 1]).abs() < 1e-4
        {
            hit_angles.pop();
        }

        let mut arc_segs: Vec<PathSeg> = Vec::new();
        if hit_angles.len() >= 2 {
            let k = hit_angles.len();
            for i in 0..k {
                let a0 = hit_angles[i];
                let a1 = if i + 1 < k {
                    hit_angles[i + 1]
                } else {
                    hit_angles[0] + TAU
                };
                if a1 - a0 <= 1e-5 {
                    continue;
                }
                let mid_a = 0.5 * (a0 + a1);
                let mid_pt = Point2D::new(
                    center.x + radius * mid_a.cos(),
                    center.y + radius * mid_a.sin(),
                );
                if closed_subpaths_contain_even_odd(&closed_subpaths, mid_pt) {
                    arc_segs.extend(circle_arc_to_cubic_segs(center, radius, a0, a1));
                }
            }
        }

        let mut out_closed: Vec<Subpath> = untouched_outside;

        // Stitch kept boundary segments and circular arcs into exact closed subpaths
        let mut pool: Vec<PathSeg> = Vec::with_capacity(boundary_segs.len() + arc_segs.len());
        pool.extend(boundary_segs);
        pool.extend(arc_segs);
        let mut used = vec![false; pool.len()];
        let stitch_tol = 0.05;

        for start_idx in 0..pool.len() {
            if used[start_idx] {
                continue;
            }
            used[start_idx] = true;
            let first = pool[start_idx];
            let loop_start = Point2D::from_kurbo(first.start());
            let mut cur_end = Point2D::from_kurbo(first.end());
            let mut chain = vec![first];

            loop {
                if chain.len() >= 2 && cur_end.distance_to(loop_start) <= 1e-3 {
                    break;
                }
                let mut best: Option<(usize, bool, f64)> = None;
                for (j, seg) in pool.iter().enumerate() {
                    if used[j] {
                        continue;
                    }
                    let d_fwd = cur_end.distance_to(Point2D::from_kurbo(seg.start()));
                    if d_fwd <= stitch_tol && best.is_none_or(|(_, _, bd)| d_fwd < bd) {
                        best = Some((j, false, d_fwd));
                    }
                    let d_rev = cur_end.distance_to(Point2D::from_kurbo(seg.end()));
                    if d_rev <= stitch_tol && best.is_none_or(|(_, _, bd)| d_rev < bd) {
                        best = Some((j, true, d_rev));
                    }
                }
                if let Some((j, rev, d_next)) = best {
                    let d_close = cur_end.distance_to(loop_start);
                    if chain.len() >= 2 && d_close < d_next && d_close <= stitch_tol {
                        break;
                    }
                    used[j] = true;
                    let next_seg = if rev { seg_reverse(pool[j]) } else { pool[j] };
                    cur_end = Point2D::from_kurbo(next_seg.end());
                    chain.push(next_seg);
                } else {
                    break;
                }
            }

            if cur_end.distance_to(loop_start) <= stitch_tol {
                if let Some(sub) = segs_to_bezier_subpath(&chain, true) {
                    if BezierPath::from_subpath(sub.clone()).area_mm2() > 1e-6 {
                        out_closed.push(sub);
                    }
                }
            }
        }

        // If the circle punched strictly inside the remaining shape without cutting boundaries
        if hit_angles.len() < 2
            && !out_closed.is_empty()
            && closed_subpaths_contain_even_odd(&out_closed, center)
        {
            let outer_sign: f64 = out_closed
                .iter()
                .map(|sp| BezierPath::from_subpath(sp.clone()).signed_area_mm2())
                .sum();
            let mut hole = exact_circle_subpath(center, radius, false);
            let hole_sign = BezierPath::from_subpath(hole.clone()).signed_area_mm2();
            if (outer_sign >= 0.0) == (hole_sign >= 0.0) {
                hole.reverse();
            }
            out_closed.push(hole);
        }

        if !out_closed.is_empty() {
            results.push(BezierPath {
                subpaths: out_closed,
            });
        }
    }

    results
}

/// Re-fits a dense polyline contour back into compact cubic Bézier segments using Philip
/// Schneider's chord-length least-squares + Newton-Raphson curve fitter with sharp-corner
/// preservation (preventing 100-node polygon bloat after boolean, offset, or eraser ops).
#[must_use]
pub fn fit_cubic_bezier_subpath(points: &[Point2D], closed: bool, max_error_mm: f64) -> Subpath {
    let mut pts: Vec<Point2D> = Vec::with_capacity(points.len());
    for &p in points {
        if !p.is_finite() {
            continue;
        }
        if pts.last().is_none_or(|prev| prev.distance_to(p) > 1e-6) {
            pts.push(p);
        }
    }
    if closed && pts.len() > 1 && pts[0].distance_to(pts[pts.len() - 1]) <= 1e-6 {
        pts.pop();
    }
    if pts.len() < 2 || (closed && pts.len() < 3) {
        return Subpath::from_points(pts, closed);
    }

    // Drop strictly collinear micro-vertices on flat edges (within 1e-4 mm) so rectangles and
    // chamfered shapes keep minimal sharp corners without unnecessary nodes.
    let pts = simplify_collinear_vertices(&pts, closed, 1e-4);
    let n = pts.len();
    if n < 2 || (closed && n < 3) {
        return Subpath::from_points(pts, closed);
    }

    let err_tol = sanitize_f64(max_error_mm, 0.02).clamp(0.002, 2.0);
    let mut is_corner = vec![false; n];
    for i in 0..n {
        if !closed && (i == 0 || i == n - 1) {
            is_corner[i] = true;
            continue;
        }
        let prev = pts[(i + n - 1) % n];
        let curr = pts[i];
        let next = pts[(i + 1) % n];
        let d0 = curr - prev;
        let d1 = next - curr;
        let l0 = d0.length();
        let l1 = d1.length();
        if l0 <= 1e-9 || l1 <= 1e-9 {
            is_corner[i] = true;
            continue;
        }
        let cos_a = (d0.dot(d1) / (l0 * l1)).clamp(-1.0, 1.0);
        // Treat crisp geometric turns (> ~26 deg) or flat-sided polygon corners (longer chords) as sharp cusps.
        if cos_a < 0.90 || (cos_a < 0.996 && l0.min(l1) > 4.0) {
            is_corner[i] = true;
        }
    }

    // Closed smooth loop with zero corners (e.g., circle/ellipse): seed 4 cardinal anchors
    let mut corner_indices: Vec<usize> = (0..n).filter(|&i| is_corner[i]).collect();
    let all_smooth_loop = closed && corner_indices.is_empty();
    if all_smooth_loop && n >= 8 {
        corner_indices = vec![0, n / 4, n / 2, (3 * n) / 4];
        corner_indices.sort_unstable();
        corner_indices.dedup();
    } else if !closed && corner_indices.is_empty() {
        corner_indices.push(0);
        corner_indices.push(n - 1);
    }

    if corner_indices.len() < 2 {
        return Subpath::from_points(pts, closed);
    }

    // Build contiguous spans between consecutive corners and collect cubic segments.
    #[derive(Clone, Copy)]
    struct FittedSeg {
        p0: Point2D,
        c1: Option<Point2D>,
        c2: Option<Point2D>,
        p1: Point2D,
        c1_smooth_anchor: bool,
        c2_smooth_anchor: bool,
    }

    let mut segs: Vec<FittedSeg> = Vec::new();
    let span_count = if closed {
        corner_indices.len()
    } else {
        corner_indices.len() - 1
    };

    for s_idx in 0..span_count {
        let i_start = corner_indices[s_idx];
        let i_end = corner_indices[(s_idx + 1) % corner_indices.len()];
        let mut span: Vec<Point2D> = Vec::new();
        let mut k = i_start;
        loop {
            span.push(pts[k]);
            if k == i_end {
                break;
            }
            k = (k + 1) % n;
            if span.len() > n + 1 {
                break;
            }
        }
        if span.len() < 2 {
            continue;
        }
        if span.len() == 2 || span_max_deviation(&span) <= err_tol * 0.3 {
            segs.push(FittedSeg {
                p0: span[0],
                c1: None,
                c2: None,
                p1: *span.last().expect("span >= 2"),
                c1_smooth_anchor: !is_corner[i_start],
                c2_smooth_anchor: !is_corner[i_end],
            });
            continue;
        }

        let t_hat1 = if is_corner[i_start] {
            (span[1] - span[0])
                .normalized()
                .unwrap_or(Vec2D::new(1.0, 0.0))
        } else {
            let prev = pts[(i_start + n - 1) % n];
            (span[1] - prev)
                .normalized()
                .unwrap_or(Vec2D::new(1.0, 0.0))
        };
        let t_hat2 = if is_corner[i_end] {
            (span[span.len() - 2] - span[span.len() - 1])
                .normalized()
                .unwrap_or(Vec2D::new(-1.0, 0.0))
        } else {
            let next = pts[(i_end + 1) % n];
            (span[span.len() - 2] - next)
                .normalized()
                .unwrap_or(Vec2D::new(-1.0, 0.0))
        };

        let cubics = fit_schneider_cubic(&span, t_hat1, t_hat2, err_tol * err_tol, 0);
        let m = cubics.len();
        for (idx, (p0, c1, c2, p1)) in cubics.into_iter().enumerate() {
            segs.push(FittedSeg {
                p0,
                c1: Some(c1),
                c2: Some(c2),
                p1,
                c1_smooth_anchor: idx > 0 || !is_corner[i_start],
                c2_smooth_anchor: idx + 1 < m || !is_corner[i_end],
            });
        }
    }

    if segs.is_empty() {
        return Subpath::from_points(pts, closed);
    }

    let mut nodes: Vec<PathNode> = Vec::with_capacity(segs.len() + usize::from(!closed));
    if closed {
        let m = segs.len();
        for i in 0..m {
            let prev = &segs[(i + m - 1) % m];
            let curr = &segs[i];
            let kind = if curr.c1_smooth_anchor
                && prev.c2_smooth_anchor
                && prev.c2.is_some()
                && curr.c1.is_some()
            {
                NodeKind::Smooth
            } else {
                NodeKind::Cusp
            };
            nodes.push(PathNode::new(curr.p0, prev.c2, curr.c1, kind));
        }
    } else {
        let m = segs.len();
        nodes.push(PathNode::new(segs[0].p0, None, segs[0].c1, NodeKind::Cusp));
        for i in 1..m {
            let prev = &segs[i - 1];
            let curr = &segs[i];
            let kind = if curr.c1_smooth_anchor
                && prev.c2_smooth_anchor
                && prev.c2.is_some()
                && curr.c1.is_some()
            {
                NodeKind::Smooth
            } else {
                NodeKind::Cusp
            };
            nodes.push(PathNode::new(curr.p0, prev.c2, curr.c1, kind));
        }
        let last = &segs[m - 1];
        nodes.push(PathNode::new(last.p1, last.c2, None, NodeKind::Cusp));
    }
    Subpath { nodes, closed }
}

fn simplify_collinear_vertices(pts: &[Point2D], closed: bool, eps: f64) -> Vec<Point2D> {
    if pts.len() <= 3 {
        return pts.to_vec();
    }
    let n = pts.len();
    let mut keep = Vec::with_capacity(n);
    for i in 0..n {
        if !closed && (i == 0 || i == n - 1) {
            keep.push(pts[i]);
            continue;
        }
        let prev = keep.last().copied().unwrap_or(pts[(i + n - 1) % n]);
        let next = pts[(i + 1) % n];
        if point_to_segment_dist(pts[i], prev, next) > eps {
            keep.push(pts[i]);
        }
    }
    if keep.len() < if closed { 3 } else { 2 } {
        pts.to_vec()
    } else {
        keep
    }
}

fn point_to_segment_dist(p: Point2D, a: Point2D, b: Point2D) -> f64 {
    let ab = b - a;
    let len_sq = ab.dot(ab);
    if len_sq <= 1e-12 {
        return p.distance_to(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.distance_to(a + ab * t)
}

fn span_max_deviation(pts: &[Point2D]) -> f64 {
    if pts.len() <= 2 {
        return 0.0;
    }
    let a = pts[0];
    let b = pts[pts.len() - 1];
    pts[1..pts.len() - 1]
        .iter()
        .map(|&p| point_to_segment_dist(p, a, b))
        .fold(0.0_f64, f64::max)
}

#[inline]
fn eval_cubic_pts(p0: Point2D, c1: Point2D, c2: Point2D, p1: Point2D, t: f64) -> Point2D {
    let mt = 1.0 - t;
    let mt2 = mt * mt;
    let t2 = t * t;
    let sum = p0.to_vec() * (mt2 * mt)
        + c1.to_vec() * (3.0 * mt2 * t)
        + c2.to_vec() * (3.0 * mt * t2)
        + p1.to_vec() * (t2 * t);
    Point2D::new(sum.x, sum.y)
}

fn fit_schneider_cubic(
    pts: &[Point2D],
    t_hat1: Vec2D,
    t_hat2: Vec2D,
    err_sq_tol: f64,
    depth: usize,
) -> Vec<(Point2D, Point2D, Point2D, Point2D)> {
    let n = pts.len();
    if n <= 2 {
        let p0 = pts[0];
        let p1 = pts[n - 1];
        let d = p0.distance_to(p1) / 3.0;
        return vec![(p0, p0 + t_hat1 * d, p1 + t_hat2 * d, p1)];
    }

    // Chord-length parameterization
    let mut u = Vec::with_capacity(n);
    u.push(0.0);
    for i in 1..n {
        u.push(u[i - 1] + pts[i].distance_to(pts[i - 1]));
    }
    let total_len = *u.last().unwrap_or(&1.0);
    if total_len <= 1e-9 {
        let p0 = pts[0];
        return vec![(p0, p0, p0, pts[n - 1])];
    }
    for val in &mut u {
        *val /= total_len;
    }

    let mut bez = generate_bezier(pts, &u, t_hat1, t_hat2);
    let (mut max_err_sq, mut split_idx) = compute_max_fit_error(pts, &u, bez);
    if max_err_sq <= err_sq_tol {
        return vec![bez];
    }

    if max_err_sq < err_sq_tol * 16.0 {
        for _ in 0..4 {
            reparameterize(pts, &mut u, bez);
            bez = generate_bezier(pts, &u, t_hat1, t_hat2);
            let (err_sq, s) = compute_max_fit_error(pts, &u, bez);
            max_err_sq = err_sq;
            split_idx = s;
            if max_err_sq <= err_sq_tol {
                return vec![bez];
            }
        }
    }

    if depth >= 10 || n <= 3 {
        return vec![bez];
    }

    let split = split_idx.clamp(1, n - 2);
    let t_center = (pts[split - 1] - pts[split + 1])
        .normalized()
        .unwrap_or_else(|| (pts[split] - pts[split + 1]).normalized().unwrap_or(t_hat2));
    let mut left = fit_schneider_cubic(&pts[..=split], t_hat1, t_center, err_sq_tol, depth + 1);
    let right = fit_schneider_cubic(&pts[split..], -t_center, t_hat2, err_sq_tol, depth + 1);
    left.extend(right);
    left
}

fn generate_bezier(
    pts: &[Point2D],
    u: &[f64],
    t_hat1: Vec2D,
    t_hat2: Vec2D,
) -> (Point2D, Point2D, Point2D, Point2D) {
    let n = pts.len();
    let p0 = pts[0];
    let p3 = pts[n - 1];
    let mut c = [[0.0_f64; 2]; 2];
    let mut x = [0.0_f64; 2];

    for i in 0..n {
        let t = u[i];
        let mt = 1.0 - t;
        let b0 = mt * mt * mt;
        let b1 = 3.0 * mt * mt * t;
        let b2 = 3.0 * mt * t * t;
        let b3 = t * t * t;
        let a1 = t_hat1 * b1;
        let a2 = t_hat2 * b2;

        c[0][0] += a1.dot(a1);
        c[0][1] += a1.dot(a2);
        c[1][0] = c[0][1];
        c[1][1] += a2.dot(a2);

        let base_vec = p0.to_vec() * (b0 + b1) + p3.to_vec() * (b2 + b3);
        let tmp = pts[i] - Point2D::new(base_vec.x, base_vec.y);
        x[0] += a1.dot(tmp);
        x[1] += a2.dot(tmp);
    }

    let det = c[0][0] * c[1][1] - c[0][1] * c[1][0];
    let seg_len = p0.distance_to(p3);
    let default_alpha = (seg_len / 3.0).max(1e-6);
    let (mut alpha_l, mut alpha_r) = if det.abs() > 1e-12 {
        (
            (x[0] * c[1][1] - x[1] * c[0][1]) / det,
            (c[0][0] * x[1] - c[1][0] * x[0]) / det,
        )
    } else {
        (default_alpha, default_alpha)
    };

    let eps = 1e-6 * seg_len;
    if !alpha_l.is_finite()
        || !alpha_r.is_finite()
        || alpha_l < eps
        || alpha_r < eps
        || alpha_l > seg_len * 3.0 + 1.0
        || alpha_r > seg_len * 3.0 + 1.0
    {
        alpha_l = default_alpha;
        alpha_r = default_alpha;
    }

    (p0, p0 + t_hat1 * alpha_l, p3 + t_hat2 * alpha_r, p3)
}

fn compute_max_fit_error(
    pts: &[Point2D],
    u: &[f64],
    (p0, c1, c2, p3): (Point2D, Point2D, Point2D, Point2D),
) -> (f64, usize) {
    let mut max_dist_sq = 0.0_f64;
    let mut split_idx = pts.len() / 2;
    for i in 1..pts.len().saturating_sub(1) {
        let p = eval_cubic_pts(p0, c1, c2, p3, u[i]);
        let d = p - pts[i];
        let d_sq = d.dot(d);
        if d_sq >= max_dist_sq {
            max_dist_sq = d_sq;
            split_idx = i;
        }
    }
    (max_dist_sq, split_idx)
}

fn reparameterize(
    pts: &[Point2D],
    u: &mut [f64],
    (p0, c1, c2, p3): (Point2D, Point2D, Point2D, Point2D),
) {
    let q1 = (c1 - p0) * 3.0;
    let q2 = (c2 - c1) * 3.0;
    let q3 = (p3 - c2) * 3.0;
    let r1 = (q2 - q1) * 2.0;
    let r2 = (q3 - q2) * 2.0;

    for i in 1..pts.len().saturating_sub(1) {
        let t = u[i];
        let mt = 1.0 - t;
        let qt = eval_cubic_pts(p0, c1, c2, p3, t);
        let q1_t = q1 * (mt * mt) + q2 * (2.0 * mt * t) + q3 * (t * t);
        let q2_t = r1 * mt + r2 * t;
        let diff = qt - pts[i];
        let num = diff.dot(q1_t);
        let den = q1_t.dot(q1_t) + diff.dot(q2_t);
        if den.abs() > 1e-12 {
            u[i] = (t - num / den).clamp(0.0, 1.0);
        }
    }
}

fn segment_segment_intersection(
    a0: Point2D,
    a1: Point2D,
    b0: Point2D,
    b1: Point2D,
) -> Option<(f64, f64)> {
    let r = a1 - a0;
    let s = b1 - b0;
    let denom = r.cross(s);
    if denom.abs() < 1e-12 {
        return None;
    }
    let qp = b0 - a0;
    let t = qp.cross(s) / denom;
    let u = qp.cross(r) / denom;
    if (-1e-7..=1.0 + 1e-7).contains(&t) && (-1e-7..=1.0 + 1e-7).contains(&u) {
        Some((t.clamp(0.0, 1.0), u.clamp(0.0, 1.0)))
    } else {
        None
    }
}

fn segment_circle_intersections(a: Point2D, b: Point2D, center: Point2D, radius: f64) -> Vec<f64> {
    let d = b - a;
    let f = a - center;
    let aa = d.dot(d);
    if aa <= 1e-12 {
        return Vec::new();
    }
    let bb = 2.0 * f.dot(d);
    let cc = f.dot(f) - radius * radius;
    let disc = bb * bb - 4.0 * aa * cc;
    if disc < 0.0 {
        return Vec::new();
    }
    let sqrt_disc = disc.sqrt();
    let mut out = Vec::new();
    for t in [
        (-bb - sqrt_disc) / (2.0 * aa),
        (-bb + sqrt_disc) / (2.0 * aa),
    ] {
        if t > 1e-6 && t < 1.0 - 1e-6 {
            out.push(t);
        }
    }
    out
}

fn extract_polyline_range(poly: &[Point2D], start_arc: f64, end_arc: f64) -> Vec<Point2D> {
    if end_arc <= start_arc + 1e-6 || poly.len() < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut accum = 0.0;
    for win in poly.windows(2) {
        let a = win[0];
        let b = win[1];
        let len = a.distance_to(b);
        if len <= 1e-9 {
            continue;
        }
        let seg_start = accum;
        let seg_end = accum + len;
        if seg_end >= start_arc && seg_start <= end_arc {
            let t0 = ((start_arc - seg_start) / len).clamp(0.0, 1.0);
            let t1 = ((end_arc - seg_start) / len).clamp(0.0, 1.0);
            let p0 = a.lerp(b, t0);
            let p1 = a.lerp(b, t1);
            if out
                .last()
                .is_none_or(|last: &Point2D| last.distance_to(p0) > 1e-6)
            {
                out.push(p0);
            }
            if out
                .last()
                .is_none_or(|last: &Point2D| last.distance_to(p1) > 1e-6)
            {
                out.push(p1);
            }
        }
        accum = seg_end;
    }
    out
}

// =============================================================================
// SmartDrawing, LiveSketch, and ArtisticMedia
// =============================================================================

/// Geometric primitive recognized from a rough freehand stroke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecognizedShape {
    Line {
        start: Point2D,
        end: Point2D,
    },
    Rectangle {
        rect: Rect2D,
        angle_rad: f64,
    },
    Ellipse {
        center: Point2D,
        rx: f64,
        ry: f64,
        angle_rad: f64,
    },
    Triangle {
        vertices: [Point2D; 3],
    },
    Freehand(BezierPath),
}

impl RecognizedShape {
    /// Converts the recognized shape into an editable [`BezierPath`].
    #[must_use]
    pub fn to_bezier_path(&self) -> BezierPath {
        match self {
            Self::Line { start, end } => BezierPath::from_line(*start, *end),
            Self::Rectangle { rect, angle_rad } => {
                let mut bp = BezierPath::from_rect(*rect);
                if angle_rad.abs() > 1e-6 {
                    bp.transform(&Affine2D::rotate_about(*angle_rad, rect.center()));
                }
                bp
            }
            Self::Ellipse {
                center,
                rx,
                ry,
                angle_rad,
            } => {
                let mut bp = ParametricShape::Ellipse {
                    rx: *rx,
                    ry: *ry,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }
                .to_bezier_path();
                let xf =
                    Affine2D::translate(center.x, center.y).compose(&Affine2D::rotate(*angle_rad));
                bp.transform(&xf);
                bp
            }
            Self::Triangle { vertices } => BezierPath::from_polygon(*vertices, true),
            Self::Freehand(p) => p.clone(),
        }
    }
}

/// Freehand stroke recognizer (line, rectangle, ellipse, triangle, or smooth freehand).
pub struct SmartDrawing;

impl SmartDrawing {
    /// Classifies a sequence of raw pointer/stylus points into a clean [`RecognizedShape`].
    #[must_use]
    pub fn recognize_rough_stroke(pts: &[Point2D]) -> RecognizedShape {
        let clean: Vec<Point2D> = pts.iter().copied().filter(|p| p.is_finite()).collect();
        if clean.len() < 2 {
            return RecognizedShape::Freehand(BezierPath::from_polygon(clean, false));
        }

        let bbox = Rect2D::from_iter_points(clean.iter().copied()).unwrap_or_default();
        let diag = bbox.width().hypot(bbox.height());
        if diag < 1e-4 {
            return RecognizedShape::Freehand(BezierPath::from_polygon(clean, false));
        }

        let first = clean[0];
        let last = clean[clean.len() - 1];
        let chord = first.distance_to(last);

        // 1. Check straight line
        let max_line_dev = clean
            .iter()
            .map(|&p| point_to_segment_distance(p, first, last))
            .fold(0.0_f64, f64::max);
        if chord > diag * 0.7 && max_line_dev <= chord * 0.08 {
            return RecognizedShape::Line {
                start: first,
                end: last,
            };
        }

        // 2. Check closed shape candidates
        let is_closed = chord <= diag * 0.35;
        if is_closed && clean.len() >= 4 {
            // Strip closing duplicate vertex for corner analysis
            let mut ring = clean.clone();
            if ring
                .first()
                .zip(ring.last())
                .is_some_and(|(a, b)| a.distance_to(*b) < diag * 0.2)
            {
                ring.pop();
            }

            // Simplify aggressively to find dominant corners
            let mut simplified = rdp_simplify(&clean, diag * 0.08);
            if simplified.len() > 1
                && simplified[0].distance_to(simplified[simplified.len() - 1]) < diag * 0.25
            {
                simplified.pop();
            }

            // Test Ellipse fit: check normalized radial deviation from bbox center
            let center = bbox.center();
            let rx = (bbox.width() * 0.5).max(1e-3);
            let ry = (bbox.height() * 0.5).max(1e-3);
            let radial_err: f64 = ring
                .iter()
                .map(|p| {
                    let nx = (p.x - center.x) / rx;
                    let ny = (p.y - center.y) / ry;
                    (nx.hypot(ny) - 1.0).abs()
                })
                .sum::<f64>()
                / (ring.len().max(1) as f64);

            if simplified.len() == 3 {
                return RecognizedShape::Triangle {
                    vertices: [simplified[0], simplified[1], simplified[2]],
                };
            }

            if simplified.len() == 4 {
                // Verify all four interior angles are reasonably close to 90°
                let mut right_angle_ok = true;
                for i in 0..4 {
                    let prev = simplified[(i + 3) % 4];
                    let curr = simplified[i];
                    let next = simplified[(i + 1) % 4];
                    let v1 = (prev - curr).normalize_or_zero();
                    let v2 = (next - curr).normalize_or_zero();
                    if v1.dot(v2).abs() > 0.35 {
                        right_angle_ok = false;
                        break;
                    }
                }
                if right_angle_ok {
                    return RecognizedShape::Rectangle {
                        rect: bbox,
                        angle_rad: 0.0,
                    };
                }
            }

            if radial_err < 0.16 {
                return RecognizedShape::Ellipse {
                    center,
                    rx,
                    ry,
                    angle_rad: 0.0,
                };
            }
        }

        // 3. Default to smoothed freehand Bézier curve
        let simplified = rdp_simplify(&clean, (diag * 0.015).max(0.15));
        RecognizedShape::Freehand(fit_catmull_rom_bezier(&simplified, is_closed))
    }
}

/// Multi-stroke iterative sketching engine that averages overlapping freehand strokes
/// into a single clean Bézier curve.
pub struct LiveSketch;

impl LiveSketch {
    /// Resamples and averages multiple strokes into a single smooth [`BezierPath`].
    #[must_use]
    pub fn average_strokes(strokes: &[Vec<Point2D>], samples: usize) -> BezierPath {
        let samples = samples.clamp(2, 256);
        let valid: Vec<Vec<Point2D>> = strokes
            .iter()
            .map(|s| {
                s.iter()
                    .copied()
                    .filter(|p| p.is_finite())
                    .collect::<Vec<_>>()
            })
            .filter(|s| s.len() >= 2)
            .collect();
        if valid.is_empty() {
            return BezierPath::new();
        }

        let base = resample_polyline(&valid[0], samples);
        let mut accum = base.clone();

        for stroke in valid.iter().skip(1) {
            let mut resampled = resample_polyline(stroke, samples);
            // Align stroke direction with base so back-and-forth sketching doesn't cancel
            let fwd_dist: f64 = base
                .iter()
                .zip(&resampled)
                .map(|(a, b)| a.distance_sq(*b))
                .sum();
            let rev_dist: f64 = base
                .iter()
                .zip(resampled.iter().rev())
                .map(|(a, b)| a.distance_sq(*b))
                .sum();
            if rev_dist < fwd_dist {
                resampled.reverse();
            }
            for (acc, pt) in accum.iter_mut().zip(&resampled) {
                acc.x += pt.x;
                acc.y += pt.y;
            }
        }

        let inv_n = 1.0 / (valid.len() as f64);
        for pt in &mut accum {
            pt.x *= inv_n;
            pt.y *= inv_n;
        }

        fit_catmull_rom_bezier(&accum, false)
    }
}

fn resample_polyline(pts: &[Point2D], count: usize) -> Vec<Point2D> {
    if pts.is_empty() || count == 0 {
        return Vec::new();
    }
    if pts.len() == 1 || count == 1 {
        return vec![pts[0]; count];
    }
    let mut cum = vec![0.0; pts.len()];
    for i in 1..pts.len() {
        cum[i] = cum[i - 1] + pts[i - 1].distance_to(pts[i]);
    }
    let total = *cum.last().unwrap_or(&0.0);
    if total <= 1e-9 {
        return vec![pts[0]; count];
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let target = total * (i as f64) / ((count - 1) as f64);
        let idx = cum
            .partition_point(|&d| d < target)
            .saturating_sub(1)
            .min(pts.len() - 2);
        let seg_len = (cum[idx + 1] - cum[idx]).max(1e-9);
        let t = ((target - cum[idx]) / seg_len).clamp(0.0, 1.0);
        out.push(pts[idx].lerp(pts[idx + 1], t));
    }
    out
}

fn fit_catmull_rom_bezier(pts: &[Point2D], closed: bool) -> BezierPath {
    if pts.len() < 2 {
        return BezierPath::from_polygon(pts.iter().copied(), closed);
    }
    let n = pts.len();
    let mut nodes = Vec::with_capacity(n);
    for i in 0..n {
        let p_prev = if i == 0 {
            if closed {
                pts[n - 1]
            } else {
                pts[0]
            }
        } else {
            pts[i - 1]
        };
        let p_curr = pts[i];
        let p_next = if i + 1 == n {
            if closed {
                pts[0]
            } else {
                pts[n - 1]
            }
        } else {
            pts[i + 1]
        };
        let tangent = (p_next - p_prev) * (1.0 / 6.0);
        let h_in = if !closed && i == 0 {
            None
        } else {
            Some(p_curr - tangent)
        };
        let h_out = if !closed && i + 1 == n {
            None
        } else {
            Some(p_curr + tangent)
        };
        let kind = if h_in.is_some() && h_out.is_some() {
            NodeKind::Smooth
        } else {
            NodeKind::Cusp
        };
        nodes.push(PathNode::new(p_curr, h_in, h_out, kind));
    }
    BezierPath::from_subpath(Subpath { nodes, closed })
}

/// Width profile curve for [`ArtisticMediaStyle::Preset`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ArtisticWidthProfile {
    TaperBoth,
    TaperStart,
    TaperEnd,
    Wave,
    Uniform,
    Custom(Vec<(f64, f64)>),
}

/// Artistic media stroke expansion modes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ArtisticMediaStyle {
    Preset {
        max_width_mm: f64,
        profile: ArtisticWidthProfile,
    },
    Brush {
        width_mm: f64,
        smoothness: f64,
    },
    Sprayer {
        spacing_mm: f64,
        size_mm: f64,
        scatter_mm: f64,
        seed: u64,
    },
    Calligraphic {
        width_mm: f64,
        angle_deg: f64,
    },
    Expression {
        min_width_mm: f64,
        max_width_mm: f64,
        pressures: Vec<f64>,
    },
}

/// Expands centerline curves into closed variable-width vector outlines.
pub struct ArtisticMedia;

impl ArtisticMedia {
    /// Expands `centerline` into a closed outline [`BezierPath`] according to `style`.
    #[must_use]
    pub fn expand_stroke(centerline: &BezierPath, style: &ArtisticMediaStyle) -> BezierPath {
        if centerline.is_empty() {
            return BezierPath::new();
        }
        let total_len = centerline.path_length_mm();
        if total_len <= 1e-6 {
            return BezierPath::new();
        }

        if let ArtisticMediaStyle::Sprayer {
            spacing_mm,
            size_mm,
            scatter_mm,
            seed,
        } = style
        {
            let step = sanitize_f64(*spacing_mm, 5.0).max(0.5);
            let r = (sanitize_f64(*size_mm, 4.0) * 0.5).max(0.25);
            let scatter = sanitize_f64(*scatter_mm, 0.0).max(0.0);
            let count = ((total_len / step).floor() as usize).clamp(1, 200);
            let mut subpaths = Vec::with_capacity(count);
            for i in 0..=count {
                let d = (i as f64) * step;
                if d > total_len {
                    break;
                }
                if let Some((pt, tangent)) = centerline.point_and_tangent_at(d) {
                    let h = splitmix64(seed.wrapping_add(i as u64));
                    let jitter = ((h as f64) / (u64::MAX as f64) * 2.0 - 1.0) * scatter;
                    let center = pt + tangent.perp() * jitter;
                    let mut dab = ParametricShape::Ellipse {
                        rx: r,
                        ry: r,
                        mode: EllipseMode::Full,
                        start_deg: 0.0,
                        end_deg: 360.0,
                    }
                    .to_bezier_path();
                    dab.transform(&Affine2D::translate(center.x, center.y));
                    subpaths.extend(dab.subpaths);
                }
            }
            return BezierPath { subpaths };
        }

        let steps = ((total_len / 0.75).ceil() as usize).clamp(12, 240);
        let mut left_side = Vec::with_capacity(steps + 1);
        let mut right_side = Vec::with_capacity(steps + 1);

        for i in 0..=steps {
            let t = (i as f64) / (steps as f64);
            let Some((pt, tangent)) = centerline.point_and_tangent_at(t * total_len) else {
                continue;
            };
            let normal = tangent.perp();
            let offset_vec = match style {
                ArtisticMediaStyle::Preset {
                    max_width_mm,
                    profile,
                } => {
                    let w = sanitize_f64(*max_width_mm, 4.0).max(0.1);
                    let factor = eval_width_profile(profile, t);
                    normal * (0.5 * w * factor)
                }
                ArtisticMediaStyle::Brush {
                    width_mm,
                    smoothness,
                } => {
                    let w = sanitize_f64(*width_mm, 4.0).max(0.1);
                    let s = sanitize_f64(*smoothness, 0.5).clamp(0.0, 1.0);
                    let edge_taper = (t * PI).sin().powf(0.25 + 0.5 * s);
                    normal * (0.5 * w * edge_taper.max(0.15))
                }
                ArtisticMediaStyle::Calligraphic {
                    width_mm,
                    angle_deg,
                } => {
                    let w = sanitize_f64(*width_mm, 4.0).max(0.1);
                    let nib = Vec2D::from_angle(sanitize_f64(*angle_deg, 45.0).to_radians());
                    // Orient nib consistently to the left side of the tangent
                    let oriented = if nib.dot(normal) < 0.0 { -nib } else { nib };
                    oriented * (0.5 * w)
                }
                ArtisticMediaStyle::Expression {
                    min_width_mm,
                    max_width_mm,
                    pressures,
                } => {
                    let lo = sanitize_f64(*min_width_mm, 0.5).max(0.05);
                    let hi = sanitize_f64(*max_width_mm, 5.0).max(lo);
                    let p = sample_pressure(pressures, t);
                    normal * (0.5 * (lo + (hi - lo) * p))
                }
                ArtisticMediaStyle::Sprayer { .. } => Vec2D::ZERO,
            };
            left_side.push(pt + offset_vec);
            right_side.push(pt - offset_vec);
        }

        if left_side.len() < 2 {
            return BezierPath::new();
        }
        right_side.reverse();
        left_side.extend(right_side);
        BezierPath::from_polygon(left_side, true)
    }
}

fn eval_width_profile(profile: &ArtisticWidthProfile, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match profile {
        ArtisticWidthProfile::TaperBoth => (t * PI).sin().max(0.05),
        ArtisticWidthProfile::TaperStart => t.max(0.05),
        ArtisticWidthProfile::TaperEnd => (1.0 - t).max(0.05),
        ArtisticWidthProfile::Wave => (0.55 + 0.45 * (t * TAU * 2.0).sin()).max(0.05),
        ArtisticWidthProfile::Uniform => 1.0,
        ArtisticWidthProfile::Custom(stops) => {
            if stops.is_empty() {
                return 1.0;
            }
            for win in stops.windows(2) {
                let (t0, w0) = win[0];
                let (t1, w1) = win[1];
                if t >= t0 && t <= t1 && (t1 - t0).abs() > 1e-9 {
                    let f = (t - t0) / (t1 - t0);
                    return (w0 + (w1 - w0) * f).clamp(0.02, 2.0);
                }
            }
            stops.last().map_or(1.0, |&(_, w)| w.clamp(0.02, 2.0))
        }
    }
}

fn sample_pressure(pressures: &[f64], t: f64) -> f64 {
    if pressures.is_empty() {
        return (t * PI).sin().max(0.15);
    }
    if pressures.len() == 1 {
        return sanitize_f64(pressures[0], 0.5).clamp(0.0, 1.0);
    }
    let scaled = t.clamp(0.0, 1.0) * ((pressures.len() - 1) as f64);
    let idx = (scaled.floor() as usize).min(pressures.len() - 2);
    let frac = scaled - (idx as f64);
    let p0 = sanitize_f64(pressures[idx], 0.5).clamp(0.0, 1.0);
    let p1 = sanitize_f64(pressures[idx + 1], 0.5).clamp(0.0, 1.0);
    p0 + (p1 - p0) * frac
}

// =============================================================================
// Live Editable Parametric Shapes
// =============================================================================

/// Corner treatment style for [`ParametricShape::Rectangle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CornerStyle {
    #[default]
    Rounded,
    Scalloped,
    Chamfered,
}

/// Arc closure mode for [`ParametricShape::Ellipse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum EllipseMode {
    #[default]
    Full,
    Pie,
    Arc,
}

/// Ray layout style for [`ParametricShape::Impact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ImpactStyle {
    #[default]
    Radial,
    Parallel,
}

/// Built-in library of parametric basic shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BasicShapeKind {
    ArrowRight,
    ArrowLeft,
    DoubleArrow,
    Banner,
    Callout,
    Heart,
    Cross,
    Diamond,
    Trapezoid,
}

/// Live editable geometric primitives convertible to exact [`BezierPath`] curves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParametricShape {
    Rectangle {
        width: f64,
        height: f64,
        /// Corner radii `[top_left, top_right, bottom_right, bottom_left]` in mm.
        corner_radii: [f64; 4],
        corner_style: CornerStyle,
        lock_corners: bool,
    },
    Ellipse {
        rx: f64,
        ry: f64,
        mode: EllipseMode,
        start_deg: f64,
        end_deg: f64,
    },
    Polygon {
        sides: u32,
        radius_x: f64,
        radius_y: f64,
    },
    Star {
        points: u32,
        outer_radius: f64,
        inner_ratio: f64,
        complex: bool,
    },
    Spiral {
        revolutions: f64,
        radius: f64,
        logarithmic: bool,
        expansion: f64,
    },
    GraphPaper {
        cols: u32,
        rows: u32,
        width: f64,
        height: f64,
    },
    Impact {
        rays: u32,
        inner_radius: f64,
        outer_radius: f64,
        style: ImpactStyle,
    },
    BasicShape {
        kind: BasicShapeKind,
        width: f64,
        height: f64,
    },
}

impl ParametricShape {
    /// Converts this live parametric primitive into an editable [`BezierPath`]
    /// ("Convert to Curves").
    #[must_use]
    pub fn to_bezier_path(&self) -> BezierPath {
        match *self {
            Self::Rectangle {
                width,
                height,
                corner_radii,
                corner_style,
                ..
            } => build_rectangle_path(width, height, corner_radii, corner_style),
            Self::Ellipse {
                rx,
                ry,
                mode,
                start_deg,
                end_deg,
            } => build_ellipse_path(rx, ry, mode, start_deg, end_deg),
            Self::Polygon {
                sides,
                radius_x,
                radius_y,
            } => build_polygon_path(sides, radius_x, radius_y),
            Self::Star {
                points,
                outer_radius,
                inner_ratio,
                complex,
            } => build_star_path(points, outer_radius, inner_ratio, complex),
            Self::Spiral {
                revolutions,
                radius,
                logarithmic,
                expansion,
            } => build_spiral_path(revolutions, radius, logarithmic, expansion),
            Self::GraphPaper {
                cols,
                rows,
                width,
                height,
            } => build_graph_paper_path(cols, rows, width, height),
            Self::Impact {
                rays,
                inner_radius,
                outer_radius,
                style,
            } => build_impact_path(rays, inner_radius, outer_radius, style),
            Self::BasicShape {
                kind,
                width,
                height,
            } => build_basic_shape_path(kind, width, height),
        }
    }

    /// Local-space bounding box of this shape.
    #[must_use]
    pub fn bounding_box(&self) -> Option<Rect2D> {
        self.to_bezier_path().bounding_box()
    }

    /// Returns `true` if every floating-point field on this shape is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        match *self {
            Self::Rectangle {
                width,
                height,
                corner_radii,
                ..
            } => {
                width.is_finite()
                    && height.is_finite()
                    && corner_radii.iter().all(|r| r.is_finite())
            }
            Self::Ellipse {
                rx,
                ry,
                start_deg,
                end_deg,
                ..
            } => rx.is_finite() && ry.is_finite() && start_deg.is_finite() && end_deg.is_finite(),
            Self::Polygon {
                radius_x, radius_y, ..
            } => radius_x.is_finite() && radius_y.is_finite(),
            Self::Star {
                outer_radius,
                inner_ratio,
                ..
            } => outer_radius.is_finite() && inner_ratio.is_finite(),
            Self::Spiral {
                revolutions,
                radius,
                expansion,
                ..
            } => revolutions.is_finite() && radius.is_finite() && expansion.is_finite(),
            Self::GraphPaper { width, height, .. } => width.is_finite() && height.is_finite(),
            Self::Impact {
                inner_radius,
                outer_radius,
                ..
            } => inner_radius.is_finite() && outer_radius.is_finite(),
            Self::BasicShape { width, height, .. } => width.is_finite() && height.is_finite(),
        }
    }
}

fn build_rectangle_path(
    width: f64,
    height: f64,
    radii: [f64; 4],
    style: CornerStyle,
) -> BezierPath {
    let w = sanitize_f64(width, 0.0).max(0.0);
    let h = sanitize_f64(height, 0.0).max(0.0);
    if w <= 1e-9 || h <= 1e-9 {
        return BezierPath::new();
    }
    let max_r = 0.5 * w.min(h);
    let [r_tl, r_tr, r_br, r_bl] = radii.map(|r| sanitize_f64(r, 0.0).clamp(0.0, max_r));

    if r_tl <= 1e-9 && r_tr <= 1e-9 && r_br <= 1e-9 && r_bl <= 1e-9 {
        return BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, w, h));
    }

    let mut kb = BezPath::new();
    kb.move_to((r_tl, 0.0));
    kb.line_to((w - r_tr, 0.0));

    // Top-right corner: from (w - r_tr, 0) to (w, r_tr)
    emit_rect_corner(
        &mut kb,
        style,
        (w - r_tr, 0.0),
        (w, 0.0),
        (w, r_tr),
        (w - r_tr, r_tr),
        r_tr,
    );
    kb.line_to((w, h - r_br));

    // Bottom-right corner: from (w, h - r_br) to (w - r_br, h)
    emit_rect_corner(
        &mut kb,
        style,
        (w, h - r_br),
        (w, h),
        (w - r_br, h),
        (w - r_br, h - r_br),
        r_br,
    );
    kb.line_to((r_bl, h));

    // Bottom-left corner: from (r_bl, h) to (0, h - r_bl)
    emit_rect_corner(
        &mut kb,
        style,
        (r_bl, h),
        (0.0, h),
        (0.0, h - r_bl),
        (r_bl, h - r_bl),
        r_bl,
    );
    kb.line_to((0.0, r_tl));

    // Top-left corner: from (0, r_tl) to (r_tl, 0)
    emit_rect_corner(
        &mut kb,
        style,
        (0.0, r_tl),
        (0.0, 0.0),
        (r_tl, 0.0),
        (r_tl, r_tl),
        r_tl,
    );
    kb.close_path();
    BezierPath::from_kurbo(&kb)
}

fn emit_rect_corner(
    kb: &mut BezPath,
    style: CornerStyle,
    start: (f64, f64),
    corner: (f64, f64),
    end: (f64, f64),
    inward_center: (f64, f64),
    r: f64,
) {
    if r <= 1e-9 {
        return;
    }
    match style {
        CornerStyle::Chamfered => {
            kb.line_to(end);
        }
        CornerStyle::Rounded => {
            let c1 = (
                start.0 + (corner.0 - start.0) * CIRCLE_KAPPA,
                start.1 + (corner.1 - start.1) * CIRCLE_KAPPA,
            );
            let c2 = (
                end.0 + (corner.0 - end.0) * CIRCLE_KAPPA,
                end.1 + (corner.1 - end.1) * CIRCLE_KAPPA,
            );
            kb.curve_to(c1, c2, end);
        }
        CornerStyle::Scalloped => {
            // Concave circular arc curving inward toward the interior corner center
            let c1 = (
                start.0 + (inward_center.0 - start.0) * CIRCLE_KAPPA,
                start.1 + (inward_center.1 - start.1) * CIRCLE_KAPPA,
            );
            let c2 = (
                end.0 + (inward_center.0 - end.0) * CIRCLE_KAPPA,
                end.1 + (inward_center.1 - end.1) * CIRCLE_KAPPA,
            );
            kb.curve_to(c1, c2, end);
        }
    }
}

fn build_ellipse_path(
    rx: f64,
    ry: f64,
    mode: EllipseMode,
    start_deg: f64,
    end_deg: f64,
) -> BezierPath {
    let rx = sanitize_f64(rx, 0.0).abs();
    let ry = sanitize_f64(ry, 0.0).abs();
    if rx <= 1e-9 || ry <= 1e-9 {
        return BezierPath::new();
    }

    if mode == EllipseMode::Full {
        let kx = rx * CIRCLE_KAPPA;
        let ky = ry * CIRCLE_KAPPA;
        let nodes = vec![
            PathNode::new(
                Point2D::new(rx, 0.0),
                Some(Point2D::new(rx, -ky)),
                Some(Point2D::new(rx, ky)),
                NodeKind::Symmetrical,
            ),
            PathNode::new(
                Point2D::new(0.0, ry),
                Some(Point2D::new(kx, ry)),
                Some(Point2D::new(-kx, ry)),
                NodeKind::Symmetrical,
            ),
            PathNode::new(
                Point2D::new(-rx, 0.0),
                Some(Point2D::new(-rx, ky)),
                Some(Point2D::new(-rx, -ky)),
                NodeKind::Symmetrical,
            ),
            PathNode::new(
                Point2D::new(0.0, -ry),
                Some(Point2D::new(-kx, -ry)),
                Some(Point2D::new(kx, -ry)),
                NodeKind::Symmetrical,
            ),
        ];
        return BezierPath::from_subpath(Subpath {
            nodes,
            closed: true,
        });
    }

    let start = sanitize_f64(start_deg, 0.0).to_radians();
    let mut sweep = (sanitize_f64(end_deg, 270.0) - sanitize_f64(start_deg, 0.0)).to_radians();
    if sweep.abs() < 1e-6 {
        sweep = TAU;
    }
    sweep = sweep.clamp(-TAU, TAU);
    let segments = ((sweep.abs() / FRAC_PI_2).ceil() as usize).clamp(1, 8);
    let step = sweep / (segments as f64);

    let mut kb = BezPath::new();
    let p_start = (rx * start.cos(), ry * start.sin());
    if mode == EllipseMode::Pie {
        kb.move_to((0.0, 0.0));
        kb.line_to(p_start);
    } else {
        kb.move_to(p_start);
    }

    let alpha = (4.0 / 3.0) * (step * 0.25).tan();
    for i in 0..segments {
        let a0 = start + (i as f64) * step;
        let a1 = a0 + step;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let p0 = (rx * c0, ry * s0);
        let p3 = (rx * c1, ry * s1);
        let p1 = (p0.0 - alpha * rx * s0, p0.1 + alpha * ry * c0);
        let p2 = (p3.0 + alpha * rx * s1, p3.1 - alpha * ry * c1);
        kb.curve_to(p1, p2, p3);
    }

    if mode == EllipseMode::Pie {
        kb.close_path();
    }
    BezierPath::from_kurbo(&kb)
}

fn build_polygon_path(sides: u32, radius_x: f64, radius_y: f64) -> BezierPath {
    let n = sides.clamp(3, 512) as usize;
    let rx = sanitize_f64(radius_x, 0.0).abs();
    let ry = sanitize_f64(radius_y, 0.0).abs();
    if rx <= 1e-9 || ry <= 1e-9 {
        return BezierPath::new();
    }
    let pts: Vec<Point2D> = (0..n)
        .map(|i| {
            let theta = -FRAC_PI_2 + TAU * (i as f64) / (n as f64);
            Point2D::new(rx * theta.cos(), ry * theta.sin())
        })
        .collect();
    BezierPath::from_polygon(pts, true)
}

fn build_star_path(points: u32, outer_radius: f64, inner_ratio: f64, complex: bool) -> BezierPath {
    let n = points.clamp(3, 256) as usize;
    let r_out = sanitize_f64(outer_radius, 0.0).abs();
    if r_out <= 1e-9 {
        return BezierPath::new();
    }
    let ratio = sanitize_f64(inner_ratio, 0.5).clamp(0.05, 0.99);

    if complex && n >= 5 {
        // Complex intersecting star polygon {n / k} with step <= (n - 1) / 2 so every cycle has >= 3 vertices
        let step = ((n - 1) / 2).max(2);
        let mut visited = vec![false; n];
        let mut subpaths = Vec::new();
        for start in 0..n {
            if visited[start] {
                continue;
            }
            let mut idx = start;
            let mut pts = Vec::new();
            while !visited[idx] {
                visited[idx] = true;
                let theta = -FRAC_PI_2 + TAU * (idx as f64) / (n as f64);
                pts.push(Point2D::new(r_out * theta.cos(), r_out * theta.sin()));
                idx = (idx + step) % n;
            }
            if pts.len() >= 3 {
                subpaths.push(Subpath::from_points(pts, true));
            }
        }
        return BezierPath { subpaths };
    }

    let r_in = r_out * ratio;
    let total_verts = n * 2;
    let pts: Vec<Point2D> = (0..total_verts)
        .map(|i| {
            let r = if i % 2 == 0 { r_out } else { r_in };
            let theta = -FRAC_PI_2 + TAU * (i as f64) / (total_verts as f64);
            Point2D::new(r * theta.cos(), r * theta.sin())
        })
        .collect();
    BezierPath::from_polygon(pts, true)
}

fn build_spiral_path(
    revolutions: f64,
    radius: f64,
    logarithmic: bool,
    expansion: f64,
) -> BezierPath {
    let revs = sanitize_f64(revolutions, 3.0).clamp(0.25, 50.0);
    let max_r = sanitize_f64(radius, 0.0).abs();
    if max_r <= 1e-9 {
        return BezierPath::new();
    }
    let k = sanitize_f64(expansion, 2.0).clamp(0.1, 10.0);
    let steps = ((revs * 16.0).ceil() as usize).clamp(8, 1024);
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = (i as f64) / (steps as f64);
        let theta = revs * TAU * t;
        let r = if logarithmic {
            let denom = k.exp() - 1.0;
            if denom.abs() > 1e-9 {
                max_r * ((k * t).exp() - 1.0) / denom
            } else {
                max_r * t
            }
        } else {
            max_r * t
        };
        pts.push(Point2D::new(r * theta.cos(), r * theta.sin()));
    }
    fit_catmull_rom_bezier(&pts, false)
}

fn build_graph_paper_path(cols: u32, rows: u32, width: f64, height: f64) -> BezierPath {
    let cols = cols.clamp(1, 200) as usize;
    let rows = rows.clamp(1, 200) as usize;
    let w = sanitize_f64(width, 0.0).max(0.0);
    let h = sanitize_f64(height, 0.0).max(0.0);
    if w <= 1e-9 || h <= 1e-9 {
        return BezierPath::new();
    }
    let cw = w / (cols as f64);
    let ch = h / (rows as f64);
    let mut subpaths = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            let rect = Rect2D::from_xywh((c as f64) * cw, (r as f64) * ch, cw, ch);
            subpaths.push(Subpath::from_points(rect.corners(), true));
        }
    }
    BezierPath { subpaths }
}

fn build_impact_path(
    rays: u32,
    inner_radius: f64,
    outer_radius: f64,
    style: ImpactStyle,
) -> BezierPath {
    let n = rays.clamp(3, 360) as usize;
    let r_in = sanitize_f64(inner_radius, 0.0).abs();
    let r_out_raw = sanitize_f64(outer_radius, 0.0).abs();
    if r_out_raw <= 1e-9 && r_in <= 1e-9 {
        return BezierPath::new();
    }
    let r_out = r_out_raw.max(r_in + 0.5);
    let mut subpaths = Vec::with_capacity(n);

    match style {
        ImpactStyle::Radial => {
            let half_angle = (TAU / (n as f64)) * 0.22;
            for i in 0..n {
                let theta = TAU * (i as f64) / (n as f64);
                let tip = Point2D::new(r_in * theta.cos(), r_in * theta.sin());
                let a = Point2D::new(
                    r_out * (theta - half_angle).cos(),
                    r_out * (theta - half_angle).sin(),
                );
                let b = Point2D::new(
                    r_out * (theta + half_angle).cos(),
                    r_out * (theta + half_angle).sin(),
                );
                subpaths.push(Subpath::from_points([tip, a, b], true));
            }
        }
        ImpactStyle::Parallel => {
            let span = r_out * 2.0;
            let pitch = span / (n as f64);
            let half_thick = (pitch * 0.18).max(0.1);
            for i in 0..n {
                let y = -r_out + pitch * ((i as f64) + 0.5);
                let rect = Rect2D::from_xywh(-r_out, y - half_thick, span, half_thick * 2.0);
                subpaths.push(Subpath::from_points(rect.corners(), true));
            }
        }
    }

    BezierPath { subpaths }
}

fn build_basic_shape_path(kind: BasicShapeKind, width: f64, height: f64) -> BezierPath {
    let w = sanitize_f64(width, 0.0).max(0.0);
    let h = sanitize_f64(height, 0.0).max(0.0);
    if w <= 1e-9 || h <= 1e-9 {
        return BezierPath::new();
    }

    match kind {
        BasicShapeKind::ArrowRight => BezierPath::from_polygon(
            [
                Point2D::new(0.0, h * 0.28),
                Point2D::new(w * 0.62, h * 0.28),
                Point2D::new(w * 0.62, 0.0),
                Point2D::new(w, h * 0.5),
                Point2D::new(w * 0.62, h),
                Point2D::new(w * 0.62, h * 0.72),
                Point2D::new(0.0, h * 0.72),
            ],
            true,
        ),
        BasicShapeKind::ArrowLeft => BezierPath::from_polygon(
            [
                Point2D::new(w, h * 0.28),
                Point2D::new(w * 0.38, h * 0.28),
                Point2D::new(w * 0.38, 0.0),
                Point2D::new(0.0, h * 0.5),
                Point2D::new(w * 0.38, h),
                Point2D::new(w * 0.38, h * 0.72),
                Point2D::new(w, h * 0.72),
            ],
            true,
        ),
        BasicShapeKind::DoubleArrow => BezierPath::from_polygon(
            [
                Point2D::new(0.0, h * 0.5),
                Point2D::new(w * 0.28, 0.0),
                Point2D::new(w * 0.28, h * 0.28),
                Point2D::new(w * 0.72, h * 0.28),
                Point2D::new(w * 0.72, 0.0),
                Point2D::new(w, h * 0.5),
                Point2D::new(w * 0.72, h),
                Point2D::new(w * 0.72, h * 0.72),
                Point2D::new(w * 0.28, h * 0.72),
                Point2D::new(w * 0.28, h),
            ],
            true,
        ),
        BasicShapeKind::Banner => BezierPath::from_polygon(
            [
                Point2D::new(0.0, 0.0),
                Point2D::new(w, 0.0),
                Point2D::new(w * 0.88, h * 0.5),
                Point2D::new(w, h),
                Point2D::new(0.0, h),
                Point2D::new(w * 0.12, h * 0.5),
            ],
            true,
        ),
        BasicShapeKind::Callout => BezierPath::from_polygon(
            [
                Point2D::new(0.0, 0.0),
                Point2D::new(w, 0.0),
                Point2D::new(w, h * 0.72),
                Point2D::new(w * 0.38, h * 0.72),
                Point2D::new(w * 0.18, h),
                Point2D::new(w * 0.22, h * 0.72),
                Point2D::new(0.0, h * 0.72),
            ],
            true,
        ),
        BasicShapeKind::Heart => {
            let mut kb = BezPath::new();
            kb.move_to((w * 0.5, h));
            kb.curve_to((w * 0.12, h * 0.72), (0.0, h * 0.44), (0.0, h * 0.26));
            kb.curve_to((0.0, h * 0.06), (w * 0.18, 0.0), (w * 0.34, 0.0));
            kb.curve_to((w * 0.44, 0.0), (w * 0.5, h * 0.10), (w * 0.5, h * 0.20));
            kb.curve_to((w * 0.5, h * 0.10), (w * 0.56, 0.0), (w * 0.66, 0.0));
            kb.curve_to((w * 0.82, 0.0), (w, h * 0.06), (w, h * 0.26));
            kb.curve_to((w, h * 0.44), (w * 0.88, h * 0.72), (w * 0.5, h));
            kb.close_path();
            BezierPath::from_kurbo(&kb)
        }
        BasicShapeKind::Cross => {
            let t = 1.0 / 3.0;
            BezierPath::from_polygon(
                [
                    Point2D::new(w * t, 0.0),
                    Point2D::new(w * (1.0 - t), 0.0),
                    Point2D::new(w * (1.0 - t), h * t),
                    Point2D::new(w, h * t),
                    Point2D::new(w, h * (1.0 - t)),
                    Point2D::new(w * (1.0 - t), h * (1.0 - t)),
                    Point2D::new(w * (1.0 - t), h),
                    Point2D::new(w * t, h),
                    Point2D::new(w * t, h * (1.0 - t)),
                    Point2D::new(0.0, h * (1.0 - t)),
                    Point2D::new(0.0, h * t),
                    Point2D::new(w * t, h * t),
                ],
                true,
            )
        }
        BasicShapeKind::Diamond => BezierPath::from_polygon(
            [
                Point2D::new(w * 0.5, 0.0),
                Point2D::new(w, h * 0.5),
                Point2D::new(w * 0.5, h),
                Point2D::new(0.0, h * 0.5),
            ],
            true,
        ),
        BasicShapeKind::Trapezoid => BezierPath::from_polygon(
            [
                Point2D::new(w * 0.2, 0.0),
                Point2D::new(w * 0.8, 0.0),
                Point2D::new(w, h),
                Point2D::new(0.0, h),
            ],
            true,
        ),
    }
}

/// Native non-uniform B-spline curve evaluated via de Boor's algorithm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BSplineCurve {
    pub control_points: Vec<Point2D>,
    pub degree: u32,
    pub knots: Vec<f64>,
    pub closed: bool,
}

impl BSplineCurve {
    /// Builds a clamped uniform B-spline from `control_points`.
    #[must_use]
    pub fn from_control_points(control_points: Vec<Point2D>, degree: u32, closed: bool) -> Self {
        let n = control_points.len();
        let p = if n <= 1 {
            0
        } else {
            (degree as usize).clamp(1, (n - 1).min(7))
        };
        let knots = Self::default_clamped_knots(n, p);
        Self {
            control_points,
            degree: p as u32,
            knots,
            closed,
        }
    }

    /// Generates a clamped uniform knot vector of length `n + p + 1` in `[0.0, 1.0]`.
    #[must_use]
    pub fn default_clamped_knots(n: usize, p: usize) -> Vec<f64> {
        if n == 0 {
            return Vec::new();
        }
        let m = n + p + 1;
        let mut knots = Vec::with_capacity(m);
        let interior = n.saturating_sub(p);
        for i in 0..m {
            if i <= p {
                knots.push(0.0);
            } else if i >= n {
                knots.push(1.0);
            } else if interior > 0 {
                knots.push((i - p) as f64 / (interior as f64));
            } else {
                knots.push(1.0);
            }
        }
        knots
    }

    /// Returns `true` if every control point and knot value is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.control_points.iter().all(|p| p.is_finite())
            && self.knots.iter().all(|k| k.is_finite())
    }

    /// Transforms all control points in place by `affine`.
    pub fn transform(&mut self, affine: &Affine2D) {
        if !affine.is_finite() {
            return;
        }
        for pt in &mut self.control_points {
            *pt = affine.transform_point(*pt);
        }
    }

    /// Returns a transformed copy of this B-spline.
    #[must_use]
    pub fn transformed(&self, affine: &Affine2D) -> Self {
        let mut out = self.clone();
        out.transform(affine);
        out
    }

    /// Resolves a valid non-decreasing knot vector and effective degree without panicking on
    /// degenerate or malformed inputs (`degree == 0`, 1 point, short/non-monotonic knots).
    fn resolved_knots_and_degree(&self) -> Option<(usize, Vec<f64>)> {
        let n = self.control_points.len();
        if n < 2 || self.degree == 0 || !self.is_finite() {
            return None;
        }
        let p = (self.degree as usize).min(n - 1).clamp(1, 7);
        let expected_len = n + p + 1;
        let valid_knots = self.knots.len() == expected_len
            && self.knots.windows(2).all(|w| w[0] <= w[1])
            && self.knots[n] > self.knots[p];
        let knots = if valid_knots {
            self.knots.clone()
        } else {
            Self::default_clamped_knots(n, p)
        };
        if knots.len() != expected_len || knots[n] <= knots[p] {
            return None;
        }
        Some((p, knots))
    }

    /// Evaluates the B-spline curve at parameter `u` (clamped to `[knots[p], knots[n]]`)
    /// using de Boor's algorithm.
    #[must_use]
    pub fn evaluate(&self, u: f64) -> Option<Point2D> {
        let valid_pts: Vec<Point2D> = self
            .control_points
            .iter()
            .copied()
            .filter(|p| p.is_finite())
            .collect();
        if valid_pts.is_empty() || !u.is_finite() {
            return None;
        }
        if valid_pts.len() == 1 || self.degree == 0 {
            return Some(valid_pts[0]);
        }
        let n = valid_pts.len();
        let (p, knots) = self.resolved_knots_and_degree()?;
        let u_min = knots[p];
        let u_max = knots[n];
        let t = u.clamp(u_min, u_max);

        // Locate knot span `k` such that knots[k] <= t < knots[k + 1] (or last non-empty span at u_max)
        let mut k = p;
        if (t - u_max).abs() <= 1e-12 {
            k = n - 1;
            while k > p && (knots[k + 1] - knots[k]).abs() <= 1e-12 {
                k -= 1;
            }
        } else {
            for idx in p..n {
                if t >= knots[idx] && t < knots[idx + 1] {
                    k = idx;
                    break;
                }
            }
        }

        let mut d: Vec<Point2D> = (0..=p)
            .map(|j| valid_pts[(k + j).saturating_sub(p).min(n - 1)])
            .collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let i = k + j - p;
                let denom = knots[i + p + 1 - r] - knots[i];
                let alpha = if denom.abs() > 1e-12 {
                    ((t - knots[i]) / denom).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                d[j] = d[j - 1].lerp(d[j], alpha);
            }
        }
        Some(d[p])
    }

    /// Evaluates this B-spline into a smooth editable cubic [`BezierPath`].
    #[must_use]
    pub fn to_bezier_path(&self) -> BezierPath {
        let clean: Vec<Point2D> = self
            .control_points
            .iter()
            .copied()
            .filter(|p| p.is_finite())
            .collect();
        if clean.len() < 2 {
            return BezierPath::new();
        }
        if self.degree == 0 {
            return BezierPath::from_polygon(clean, self.closed);
        }
        let Some((p, knots)) = self.resolved_knots_and_degree() else {
            return BezierPath::from_polygon(clean, self.closed);
        };
        let n = clean.len();
        let u_min = knots[p];
        let u_max = knots[n];
        let samples = (n * 16).clamp(24, 256);
        let mut pts = Vec::with_capacity(samples + 1);
        for i in 0..=samples {
            let frac = (i as f64) / (samples as f64);
            let u = u_min + (u_max - u_min) * frac;
            if let Some(pt) = self.evaluate(u) {
                pts.push(pt);
            }
        }
        if pts.len() < 2 {
            return BezierPath::new();
        }
        BezierPath::from_subpath(fit_cubic_bezier_subpath(&pts, self.closed, 0.015))
    }

    /// Tight bounding box of the evaluated B-spline curve.
    #[must_use]
    pub fn bounding_box(&self) -> Option<Rect2D> {
        self.to_bezier_path().bounding_box()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_length_area_and_tangent() {
        let rect = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 10.0));
        assert!((rect.path_length_mm() - 60.0).abs() < 1e-3);
        assert!((rect.area_mm2() - 200.0).abs() < 1e-3);

        let (pt, tan) = rect.point_and_tangent_at(10.0).expect("midpoint of top");
        assert!((pt.x - 10.0).abs() < 1e-3);
        assert!(pt.y.abs() < 1e-3);
        assert!((tan.x - 1.0).abs() < 1e-3);
    }

    #[test]
    fn node_editing_operations_preserve_geometry() {
        let mut path = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 10.0));
        path.convert_segment_to_curve(0, 0).unwrap();
        let len_before = path.path_length_mm();
        let inserted = path.add_node_at(0, 0, 0.5).unwrap();
        assert_eq!(inserted, 1);
        assert!((path.path_length_mm() - len_before).abs() < 1e-3);

        path.set_node_kind(0, inserted, NodeKind::Symmetrical)
            .unwrap();
        assert_eq!(path.subpaths[0].nodes[inserted].kind, NodeKind::Symmetrical);

        path.convert_segment_to_line(0, 0).unwrap();
        assert!(path.subpaths[0].nodes[0].handle_out.is_none());

        // Break closed path into open path, then join endpoints back
        path.break_at_node(0, 0).unwrap();
        assert!(!path.subpaths[0].closed);
        let last = path.subpaths[0].nodes.len() - 1;
        path.join_nodes(0, 0, 0, last).unwrap();
        assert!(path.subpaths[0].closed);

        // Reverse subpath flips area sign
        let area_before = path.signed_area_mm2();
        path.reverse_subpath(0).unwrap();
        assert!((path.signed_area_mm2() + area_before).abs() < 1e-2);
    }

    #[test]
    fn sculpting_tools_modify_nodes_safely() {
        let mut path = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
        path.smear_near(Point2D::new(20.0, 0.0), Vec2D::new(0.0, 5.0), 15.0);
        assert!(path.node_count() > 4);
        path.smooth_near(Point2D::new(20.0, 2.0), 15.0, 0.5);
        path.twirl_near(Point2D::new(20.0, 20.0), 30.0, 0.4);
        path.attract_repel_near(Point2D::new(20.0, 20.0), 30.0, 0.25, true);
        path.roughen_near(Point2D::new(20.0, 20.0), 30.0, 1.5, 42);
        assert!(path.is_finite());

        // NaN brush inputs must not corrupt the path
        path.smear_near(Point2D::new(f64::NAN, 0.0), Vec2D::new(1.0, 1.0), 10.0);
        assert!(path.is_finite());
    }

    #[test]
    fn sculpting_brushes_zero_radius_repel_smooth_and_determinism() {
        let base = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));

        // 1. Zero/negative/non-finite radius or amplitude must be an exact no-op
        let mut noop = base.clone();
        noop.smear_near(Point2D::new(20.0, 0.0), Vec2D::new(5.0, 5.0), 0.0);
        noop.twirl_near(Point2D::new(20.0, 20.0), -2.0, 1.0);
        noop.attract_repel_near(Point2D::new(20.0, 20.0), f64::NAN, 0.8, false);
        noop.roughen_near(Point2D::new(20.0, 20.0), 15.0, 0.0, 7);
        noop.smooth_near(Point2D::new(20.0, 20.0), 0.0, 0.5);
        assert_eq!(noop, base);

        // 2. Repel moves affected edge nodes farther from center; attract moves them closer
        let center = Point2D::new(20.0, 5.0);
        let mut repelled = base.clone();
        repelled.attract_repel_near(center, 15.0, 0.6, false);
        let mut attracted = base.clone();
        attracted.attract_repel_near(center, 15.0, 0.6, true);
        let min_y_repelled = repelled.subpaths[0]
            .nodes
            .iter()
            .map(|n| n.pos.y)
            .fold(f64::INFINITY, f64::min);
        let min_y_attracted = attracted.subpaths[0]
            .nodes
            .iter()
            .map(|n| n.pos.y)
            .fold(f64::INFINITY, f64::min);
        assert!(
            min_y_repelled < 0.0,
            "repel from (20,5) must push top edge outward (y < 0), got {min_y_repelled}"
        );
        assert!(
            min_y_attracted >= 0.0
                && attracted.subpaths[0]
                    .nodes
                    .iter()
                    .any(|n| n.pos.y > 0.5 && n.pos.y < 10.0),
            "attract toward (20,5) must pull top edge inward"
        );

        // 3. Roughen determinism with same seed vs different seed + smooth_near flattening
        let mut r_a1 = base.clone();
        let mut r_a2 = base.clone();
        let mut r_b = base.clone();
        r_a1.roughen_near(Point2D::new(20.0, 0.0), 16.0, 2.5, 101);
        r_a2.roughen_near(Point2D::new(20.0, 0.0), 16.0, 2.5, 101);
        r_b.roughen_near(Point2D::new(20.0, 0.0), 16.0, 2.5, 202);
        assert_eq!(r_a1, r_a2);
        assert_ne!(r_a1, r_b);

        let before_smooth_len = r_a1.path_length_mm();
        r_a1.smooth_near(Point2D::new(20.0, 0.0), 16.0, 0.8);
        assert!(r_a1.is_finite());
        assert!(
            r_a1.path_length_mm() <= before_smooth_len + 1e-6,
            "smooth_near should reduce or preserve roughened perimeter length"
        );
    }

    #[test]
    fn knife_virtual_segment_delete_and_erase_circle() {
        let square = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));

        // Knife cut right down the middle -> 2 closed halves of 200 mm² each
        let halves = knife_cut(
            &square,
            Point2D::new(10.0, -5.0),
            Point2D::new(10.0, 25.0),
            true,
        );
        assert_eq!(halves.len(), 2);
        let total_area: f64 = halves.iter().map(BezierPath::area_mm2).sum();
        assert!((total_area - 400.0).abs() < 1.0);

        // Virtual segment delete on an open line crossed by a vertical cutter
        let horiz = BezierPath::from_line(Point2D::new(0.0, 10.0), Point2D::new(30.0, 10.0));
        let cut_l = BezierPath::from_line(Point2D::new(10.0, 0.0), Point2D::new(10.0, 20.0));
        let cut_r = BezierPath::from_line(Point2D::new(20.0, 0.0), Point2D::new(20.0, 20.0));
        let trimmed =
            virtual_segment_delete(&horiz, &[cut_l, cut_r], Point2D::new(15.0, 10.0), 2.0);
        assert_eq!(trimmed.subpaths.len(), 2);
        assert!((trimmed.path_length_mm() - 20.0).abs() < 0.2);

        // Erase circle out of the center of a square reduces area
        let erased = erase_circle(&square, Point2D::new(10.0, 10.0), 5.0);
        assert!(!erased.is_empty());
        let erased_area: f64 = erased.iter().map(BezierPath::area_mm2).sum();
        let expected = 400.0 - PI * 25.0;
        assert!((erased_area - expected).abs() < 3.0);
    }

    #[test]
    fn smart_drawing_live_sketch_and_artistic_media() {
        let line_pts: Vec<Point2D> = (0..=10)
            .map(|i| Point2D::new(f64::from(i) * 5.0, f64::from(i) * 2.0))
            .collect();
        assert!(matches!(
            SmartDrawing::recognize_rough_stroke(&line_pts),
            RecognizedShape::Line { .. }
        ));

        let circle_pts: Vec<Point2D> = (0..=36)
            .map(|i| {
                let a = TAU * f64::from(i) / 36.0;
                Point2D::new(50.0 + 20.0 * a.cos(), 50.0 + 20.0 * a.sin())
            })
            .collect();
        assert!(matches!(
            SmartDrawing::recognize_rough_stroke(&circle_pts),
            RecognizedShape::Ellipse { .. }
        ));

        let avg = LiveSketch::average_strokes(
            &[
                vec![Point2D::new(0.0, 0.0), Point2D::new(20.0, 0.0)],
                vec![Point2D::new(20.0, 4.0), Point2D::new(0.0, 4.0)],
            ],
            8,
        );
        let bb = avg.bounding_box().unwrap();
        assert!((bb.center().y - 2.0).abs() < 1e-2);

        let center = BezierPath::from_line(Point2D::new(0.0, 0.0), Point2D::new(50.0, 0.0));
        let expanded = ArtisticMedia::expand_stroke(
            &center,
            &ArtisticMediaStyle::Calligraphic {
                width_mm: 6.0,
                angle_deg: 90.0,
            },
        );
        assert!(expanded.area_mm2() > 100.0);
    }

    #[test]
    fn all_parametric_shapes_convert_to_valid_curves() {
        let shapes = vec![
            ParametricShape::Rectangle {
                width: 40.0,
                height: 20.0,
                corner_radii: [4.0, 4.0, 4.0, 4.0],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            },
            ParametricShape::Rectangle {
                width: 40.0,
                height: 20.0,
                corner_radii: [4.0, 2.0, 6.0, 0.0],
                corner_style: CornerStyle::Scalloped,
                lock_corners: false,
            },
            ParametricShape::Rectangle {
                width: 40.0,
                height: 20.0,
                corner_radii: [3.0; 4],
                corner_style: CornerStyle::Chamfered,
                lock_corners: true,
            },
            ParametricShape::Ellipse {
                rx: 15.0,
                ry: 10.0,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            },
            ParametricShape::Ellipse {
                rx: 15.0,
                ry: 10.0,
                mode: EllipseMode::Pie,
                start_deg: 0.0,
                end_deg: 90.0,
            },
            ParametricShape::Ellipse {
                rx: 15.0,
                ry: 10.0,
                mode: EllipseMode::Arc,
                start_deg: 30.0,
                end_deg: 210.0,
            },
            ParametricShape::Polygon {
                sides: 6,
                radius_x: 10.0,
                radius_y: 10.0,
            },
            ParametricShape::Star {
                points: 5,
                outer_radius: 15.0,
                inner_ratio: 0.4,
                complex: false,
            },
            ParametricShape::Star {
                points: 7,
                outer_radius: 15.0,
                inner_ratio: 0.5,
                complex: true,
            },
            ParametricShape::Spiral {
                revolutions: 3.0,
                radius: 20.0,
                logarithmic: false,
                expansion: 1.0,
            },
            ParametricShape::Spiral {
                revolutions: 3.0,
                radius: 20.0,
                logarithmic: true,
                expansion: 2.5,
            },
            ParametricShape::GraphPaper {
                cols: 4,
                rows: 3,
                width: 40.0,
                height: 30.0,
            },
            ParametricShape::Impact {
                rays: 12,
                inner_radius: 5.0,
                outer_radius: 25.0,
                style: ImpactStyle::Radial,
            },
            ParametricShape::Impact {
                rays: 8,
                inner_radius: 5.0,
                outer_radius: 25.0,
                style: ImpactStyle::Parallel,
            },
        ];

        for shape in shapes {
            let path = shape.to_bezier_path();
            assert!(!path.is_empty(), "empty curve for {shape:?}");
            assert!(path.is_finite(), "non-finite curve for {shape:?}");
            assert!(path.path_length_mm() > 0.0, "zero length for {shape:?}");
        }

        let kinds = [
            BasicShapeKind::ArrowRight,
            BasicShapeKind::ArrowLeft,
            BasicShapeKind::DoubleArrow,
            BasicShapeKind::Banner,
            BasicShapeKind::Callout,
            BasicShapeKind::Heart,
            BasicShapeKind::Cross,
            BasicShapeKind::Diamond,
            BasicShapeKind::Trapezoid,
        ];
        for kind in kinds {
            let p = ParametricShape::BasicShape {
                kind,
                width: 30.0,
                height: 20.0,
            }
            .to_bezier_path();
            assert!(!p.is_empty() && p.area_mm2() > 10.0, "failed for {kind:?}");
        }

        // Degenerate / NaN parametric shape safety
        let deg = ParametricShape::Rectangle {
            width: f64::NAN,
            height: -5.0,
            corner_radii: [f64::INFINITY; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: false,
        };
        assert!(deg.to_bezier_path().is_empty());
    }

    #[test]
    fn geometry_analytical_erase_circle_grazing_and_open_curve() {
        // Grazing disk at distance 0.9997 * r from square edge must still intersect analytically
        let square = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));
        let radius = 10.0;
        let grazing_center = Point2D::new(10.0, -radius * 0.9997);
        let grazed = erase_circle(&square, grazing_center, radius);
        assert_eq!(grazed.len(), 1);
        assert!(
            grazed[0].area_mm2() < 400.0 - 1e-4,
            "analytical erase_circle must detect 0.9997*r grazing (area={})",
            grazed[0].area_mm2()
        );

        // Open cubic curve trimmed by disk preserves curved handles on surviving pieces
        let curve = BezierPath::from_subpath(Subpath {
            nodes: vec![
                PathNode::new(
                    Point2D::new(0.0, 10.0),
                    None,
                    Some(Point2D::new(10.0, 0.0)),
                    NodeKind::Smooth,
                ),
                PathNode::new(
                    Point2D::new(40.0, 10.0),
                    Some(Point2D::new(30.0, 20.0)),
                    None,
                    NodeKind::Smooth,
                ),
            ],
            closed: false,
        });
        let trimmed = erase_circle(&curve, Point2D::new(20.0, 10.0), 6.0);
        assert_eq!(trimmed.len(), 2);
        assert!(trimmed.iter().all(|p| p.subpaths[0]
            .nodes
            .iter()
            .any(|n| n.handle_in.is_some() || n.handle_out.is_some())));
    }

    #[test]
    fn geometry_knife_and_vsd_refit_cubic_curves() {
        let circle = ParametricShape::Ellipse {
            rx: 20.0,
            ry: 20.0,
            mode: EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }
        .to_bezier_path()
        .transformed(&Affine2D::translate(30.0, 30.0));
        let halves = knife_cut(
            &circle,
            Point2D::new(30.0, 0.0),
            Point2D::new(30.0, 60.0),
            true,
        );
        assert_eq!(halves.len(), 2);
        for h in &halves {
            assert!(
                h.node_count() <= 12,
                "knife refit bloated to {} nodes",
                h.node_count()
            );
            assert!(h.subpaths[0]
                .nodes
                .iter()
                .any(|n| n.handle_in.is_some() || n.handle_out.is_some()));
        }

        // Virtual segment delete on an open cubic curve refits curved geometry
        let arc = ParametricShape::Ellipse {
            rx: 20.0,
            ry: 20.0,
            mode: EllipseMode::Arc,
            start_deg: 0.0,
            end_deg: 180.0,
        }
        .to_bezier_path();
        let cutter = BezierPath::from_line(Point2D::new(0.0, -5.0), Point2D::new(0.0, 25.0));
        let vsd = virtual_segment_delete(&arc, &[cutter], Point2D::new(-14.14, 14.14), 3.0);
        assert!(!vsd.is_empty());
        assert!(vsd.node_count() <= 10);
        assert!(vsd.subpaths[0]
            .nodes
            .iter()
            .any(|n| n.handle_in.is_some() || n.handle_out.is_some()));
    }

    #[test]
    fn geometry_bspline_evaluation_and_bva_safety() {
        let bs = BSplineCurve {
            control_points: vec![
                Point2D::new(30.0, 140.0),
                Point2D::new(60.0, 110.0),
                Point2D::new(100.0, 115.0),
                Point2D::new(125.0, 145.0),
                Point2D::new(80.0, 165.0),
            ],
            degree: 3,
            knots: vec![0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0],
            closed: true,
        };
        let path = bs.to_bezier_path();
        assert!(!path.is_empty());
        assert!(path.area_mm2() > 500.0);

        // Degenerate inputs must not panic
        let deg = BSplineCurve {
            control_points: vec![Point2D::new(10.0, 10.0)],
            degree: 0,
            knots: vec![0.0],
            closed: false,
        };
        assert!(deg.to_bezier_path().is_empty());
        assert_eq!(deg.evaluate(0.5), Some(Point2D::new(10.0, 10.0)));
    }

    #[test]
    fn continuity_enforce_preserves_dragged_handle_and_handles_cusp_and_folded() {
        let mut smooth = PathNode::new(
            Point2D::new(50.0, 50.0),
            Some(Point2D::new(30.0, 50.0)),
            Some(Point2D::new(60.0, 40.0)),
            NodeKind::Smooth,
        );
        smooth.enforce_continuity_for_handle(true);
        assert_eq!(smooth.handle_in, Some(Point2D::new(30.0, 50.0)));
        let out_vec = smooth.handle_out.unwrap() - smooth.pos;
        assert!((out_vec.y).abs() < 1e-9 && out_vec.x > 0.0);
        assert!((out_vec.length() - 10.0_f64.hypot(10.0)).abs() < 1e-6);

        let mut sym = PathNode::new(
            Point2D::new(10.0, 10.0),
            Some(Point2D::new(0.0, 10.0)),
            Some(Point2D::new(10.0, 25.0)),
            NodeKind::Symmetrical,
        );
        sym.enforce_continuity_for_handle(false);
        assert_eq!(sym.handle_out, Some(Point2D::new(10.0, 25.0)));
        assert_eq!(sym.handle_in, Some(Point2D::new(10.0, -5.0)));

        // Folded handles (identical side) still align via enforce_continuity
        let mut folded = PathNode::new(
            Point2D::new(0.0, 0.0),
            Some(Point2D::new(10.0, 0.0)),
            Some(Point2D::new(10.0, 0.0)),
            NodeKind::Symmetrical,
        );
        folded.enforce_continuity();
        assert_eq!(folded.handle_in, Some(Point2D::new(10.0, 0.0)));
        assert_eq!(folded.handle_out, Some(Point2D::new(-10.0, 0.0)));

        // Cusp remains independent
        let mut cusp = PathNode::new(
            Point2D::new(0.0, 0.0),
            Some(Point2D::new(-5.0, 0.0)),
            Some(Point2D::new(0.0, 8.0)),
            NodeKind::Cusp,
        );
        cusp.enforce_continuity_for_handle(true);
        assert_eq!(cusp.handle_out, Some(Point2D::new(0.0, 8.0)));
    }

    #[test]
    fn smart_drawing_triangle_rectangle_and_freehand_modes() {
        let rect_pts = vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(40.0, 0.0),
            Point2D::new(40.0, 20.0),
            Point2D::new(0.0, 20.0),
            Point2D::new(0.0, 0.0),
        ];
        assert!(matches!(
            SmartDrawing::recognize_rough_stroke(&rect_pts),
            RecognizedShape::Rectangle { .. }
        ));

        let tri_pts = vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(30.0, 0.0),
            Point2D::new(15.0, 25.0),
            Point2D::new(0.0, 0.0),
        ];
        assert!(matches!(
            SmartDrawing::recognize_rough_stroke(&tri_pts),
            RecognizedShape::Triangle { .. }
        ));
    }

    #[test]
    fn smart_drawing_freehand_handles_degenerate_input() {
        let single = [Point2D::new(5.0, 5.0)];
        let res = SmartDrawing::recognize_rough_stroke(&single);
        assert!(matches!(res, RecognizedShape::Freehand(_)));
        let nan_pts = [
            Point2D::new(f64::NAN, 0.0),
            Point2D::new(0.0, f64::INFINITY),
        ];
        assert!(SmartDrawing::recognize_rough_stroke(&nan_pts)
            .to_bezier_path()
            .is_empty());
    }

    #[test]
    fn artistic_media_preset_brush_expression_and_sprayer() {
        let spine = BezierPath::from_line(Point2D::new(0.0, 0.0), Point2D::new(60.0, 0.0));
        let preset = ArtisticMedia::expand_stroke(
            &spine,
            &ArtisticMediaStyle::Preset {
                max_width_mm: 8.0,
                profile: ArtisticWidthProfile::TaperBoth,
            },
        );
        assert!(preset.area_mm2() > 150.0);

        let sprayer = ArtisticMedia::expand_stroke(
            &spine,
            &ArtisticMediaStyle::Sprayer {
                spacing_mm: 10.0,
                size_mm: 4.0,
                scatter_mm: 1.0,
                seed: 7,
            },
        );
        assert!(sprayer.subpaths.len() >= 5);
    }

    #[test]
    fn artistic_media_handles_degenerate_zero_length_spine() {
        let single = BezierPath::from_polygon([Point2D::new(10.0, 10.0)], false);
        let out = ArtisticMedia::expand_stroke(
            &single,
            &ArtisticMediaStyle::Calligraphic {
                width_mm: 0.0,
                angle_deg: 45.0,
            },
        );
        assert!(out.is_empty());
    }

    #[test]
    fn erase_circle_engulfing_compound_donut_hole_preserves_eraser_hole() {
        let outer = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));
        let mut hole = BezierPath::from_rect(Rect2D::from_xywh(40.0, 40.0, 20.0, 20.0));
        if (outer.signed_area_mm2() >= 0.0) == (hole.signed_area_mm2() >= 0.0) {
            hole.subpaths[0].reverse();
        }
        let donut = BezierPath {
            subpaths: vec![outer.subpaths[0].clone(), hole.subpaths[0].clone()],
        };
        assert!((donut.area_mm2() - 9600.0).abs() < 1.0);

        let erased = erase_circle(&donut, Point2D::new(50.0, 50.0), 15.0);
        assert_eq!(erased.len(), 1);
        assert_eq!(erased[0].subpaths.len(), 2);
        let expected_area = 10000.0 - std::f64::consts::PI * 225.0;
        assert!(
            (erased[0].area_mm2() - expected_area).abs() < 1.0,
            "expected ~{expected_area:.2}, got {:.2}",
            erased[0].area_mm2()
        );
    }
}
