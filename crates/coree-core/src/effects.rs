// SPDX-License-Identifier: MIT OR Apache-2.0

//! Non-destructive live vector effects and raster adjustment/filter pipelines.

use crate::color::Color;
use crate::geometry::BezierPath;
use crate::ids::ObjectId;
use crate::math::{sanitize_f64, Affine2D, Point2D};
use crate::style::BlendMode;
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;

/// Drop shadow feather edge profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum FeatherDirection {
    Inside,
    #[default]
    Middle,
    Outside,
    Average,
}

/// Direction of concentric stepped contours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ContourMode {
    ToCenter,
    Inside,
    #[default]
    Outside,
}

/// Interactive vector distortion algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DistortKind {
    PushPull,
    Zipper,
    Twister,
}

/// Envelope mesh deformation constraint mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum EnvelopeMode {
    Straight,
    SingleArc,
    DoubleArc,
    #[default]
    Unconstrained,
}

/// Transparency alpha distribution across an object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransparencyMode {
    Uniform(f32),
    FountainLinear {
        start: Point2D,
        end: Point2D,
        start_alpha: f32,
        end_alpha: f32,
    },
    FountainRadial {
        center: Point2D,
        radius: f64,
        inner_alpha: f32,
        outer_alpha: f32,
    },
}

/// Non-destructive live vector effect attached to a [`crate::document::SceneObject`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    DropShadow {
        dx_mm: f64,
        dy_mm: f64,
        blur_mm: f64,
        opacity: f32,
        color: Color,
        feather_direction: FeatherDirection,
    },
    BlockShadow {
        dx_mm: f64,
        dy_mm: f64,
        color: Color,
        simplify: bool,
        overprint: bool,
    },
    Contour {
        mode: ContourMode,
        steps: u32,
        offset_mm: f64,
        fill_color: Color,
        outline_color: Color,
    },
    Blend {
        target_id: ObjectId,
        steps: u32,
        rotation_deg: f64,
        along_path: Option<ObjectId>,
    },
    Distort {
        kind: DistortKind,
        amplitude: f64,
        frequency: f64,
        center: Point2D,
    },
    Envelope {
        mode: EnvelopeMode,
        cage: Vec<Point2D>,
    },
    Extrude25D {
        vanishing_point: Point2D,
        depth: f64,
        bevel_depth: f64,
        shade_color: Color,
        light_dir: [f32; 3],
    },
    Transparency {
        mode: TransparencyMode,
        blend_mode: BlendMode,
    },
    Perspective {
        /// Destination quadrilateral corners `[top_left, top_right, bottom_right, bottom_left]`.
        quad: [Point2D; 4],
    },
    Symmetry {
        axes: u32,
        mirror: bool,
        origin: Point2D,
    },
}

impl Effect {
    /// Generates the full list of symmetry transformation matrices for [`Effect::Symmetry`].
    #[must_use]
    pub fn symmetry_transforms(axes: u32, mirror: bool, origin: Point2D) -> Vec<Affine2D> {
        let n = axes.clamp(1, 64);
        let mut out = Vec::with_capacity((n as usize) * if mirror { 2 } else { 1 });
        for i in 0..n {
            let angle = TAU * f64::from(i) / f64::from(n);
            let rot = Affine2D::rotate_about(angle, origin);
            out.push(rot);
            if mirror {
                let flip = Affine2D::translate(origin.x, origin.y)
                    .compose(&Affine2D::rotate(angle))
                    .compose(&Affine2D::scale(-1.0, 1.0))
                    .compose(&Affine2D::translate(-origin.x, -origin.y));
                out.push(flip);
            }
        }
        out
    }

    /// Evaluates a live [`Effect::Distort`] transformation onto a copy of `path`.
    #[must_use]
    pub fn apply_distort_to_path(
        path: &BezierPath,
        kind: DistortKind,
        amplitude: f64,
        frequency: f64,
        center: Point2D,
    ) -> BezierPath {
        let amp = sanitize_f64(amplitude, 0.0);
        let freq = sanitize_f64(frequency, 1.0).max(0.1);
        if !center.is_finite() || amp.abs() <= 1e-9 {
            return path.clone();
        }
        let mut out = path.clone();
        for sp in &mut out.subpaths {
            let n = sp.nodes.len();
            for (idx, node) in sp.nodes.iter_mut().enumerate() {
                let warp = |p: Point2D, phase_idx: usize| -> Point2D {
                    let rel = p - center;
                    let r = rel.length();
                    if r <= 1e-9 {
                        return p;
                    }
                    match kind {
                        DistortKind::PushPull => {
                            let dir = rel / r;
                            let sign = if phase_idx.is_multiple_of(2) {
                                1.0
                            } else {
                                -1.0
                            };
                            p + dir * (amp * sign)
                        }
                        DistortKind::Zipper => {
                            let dir = rel / r;
                            let wave = ((phase_idx as f64) * freq * std::f64::consts::PI).sin();
                            p + dir * (amp * wave)
                        }
                        DistortKind::Twister => {
                            let theta = (amp.to_radians()) * (r / (50.0 / freq).max(1.0));
                            center + rel.rotate(theta)
                        }
                    }
                };
                node.pos = warp(node.pos, idx);
                node.handle_in = node.handle_in.map(|h| warp(h, idx));
                node.handle_out = node.handle_out.map(|h| warp(h, (idx + 1) % n.max(1)));
            }
        }
        out
    }

    /// Maps normalized `(u, v)` in `[0, 1]²` into a 4-corner [`Effect::Perspective`] quad
    /// using bilinear interpolation.
    #[must_use]
    pub fn map_perspective_uv(quad: &[Point2D; 4], u: f64, v: f64) -> Point2D {
        let u = sanitize_f64(u, 0.0);
        let v = sanitize_f64(v, 0.0);
        let top = quad[0].lerp(quad[1], u);
        let bottom = quad[3].lerp(quad[2], u);
        top.lerp(bottom, v)
    }
}

/// Non-destructive raster/photo studio adjustments and convolution filters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RasterFilter {
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
    },
    ToneCurve {
        points: Vec<(f32, f32)>,
    },
    Levels {
        in_black: f32,
        in_gamma: f32,
        in_white: f32,
        out_black: f32,
        out_white: f32,
    },
    Hsl {
        hue_deg: f32,
        saturation: f32,
        lightness: f32,
    },
    ColorBalance {
        cyan_red: f32,
        magenta_green: f32,
        yellow_blue: f32,
    },
    ReplaceColor {
        target: Color,
        replacement: Color,
        tolerance: f32,
    },
    Gamma {
        gamma: f32,
    },
    Desaturate,
    Invert,
    Posterize {
        levels: u8,
    },
    Threshold {
        cutoff: u8,
    },
    GaussianBlur {
        radius_px: f32,
    },
    MotionBlur {
        distance_px: f32,
        angle_deg: f32,
    },
    UnsharpMask {
        amount: f32,
        radius_px: f32,
        threshold: u8,
    },
    Sharpen {
        amount: f32,
    },
    Noise {
        amount: f32,
        monochrome: bool,
        seed: u64,
    },
    Vignette {
        strength: f32,
        radius: f32,
        feather: f32,
    },
    Emboss {
        angle_deg: f32,
        depth: f32,
    },
    EdgeDetect {
        sensitivity: f32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Rect2D;

    #[test]
    fn effects_and_raster_filters_serialize_roundtrip() {
        let effects = vec![
            Effect::DropShadow {
                dx_mm: 2.0,
                dy_mm: 3.0,
                blur_mm: 4.0,
                opacity: 0.6,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Middle,
            },
            Effect::BlockShadow {
                dx_mm: 5.0,
                dy_mm: 5.0,
                color: Color::BLACK,
                simplify: true,
                overprint: false,
            },
            Effect::Contour {
                mode: ContourMode::Outside,
                steps: 3,
                offset_mm: 1.5,
                fill_color: Color::CYAN,
                outline_color: Color::BLACK,
            },
            Effect::Blend {
                target_id: ObjectId(2),
                steps: 10,
                rotation_deg: 0.0,
                along_path: None,
            },
            Effect::Distort {
                kind: DistortKind::Twister,
                amplitude: 30.0,
                frequency: 2.0,
                center: Point2D::ZERO,
            },
            Effect::Envelope {
                mode: EnvelopeMode::SingleArc,
                cage: vec![Point2D::ZERO, Point2D::new(10.0, 0.0)],
            },
            Effect::Extrude25D {
                vanishing_point: Point2D::new(50.0, -20.0),
                depth: 12.0,
                bevel_depth: 1.0,
                shade_color: Color::gray(0.3),
                light_dir: [0.5, -0.5, 0.7],
            },
            Effect::Transparency {
                mode: TransparencyMode::Uniform(0.5),
                blend_mode: BlendMode::Multiply,
            },
            Effect::Perspective {
                quad: [
                    Point2D::new(0.0, 0.0),
                    Point2D::new(10.0, 1.0),
                    Point2D::new(10.0, 9.0),
                    Point2D::new(0.0, 10.0),
                ],
            },
            Effect::Symmetry {
                axes: 4,
                mirror: true,
                origin: Point2D::new(50.0, 50.0),
            },
        ];

        let json = serde_json::to_string(&effects).unwrap();
        let back: Vec<Effect> = serde_json::from_str(&json).unwrap();
        assert_eq!(effects, back);

        assert_eq!(Effect::symmetry_transforms(4, true, Point2D::ZERO).len(), 8);

        let sq = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));
        let warped = Effect::apply_distort_to_path(
            &sq,
            DistortKind::Twister,
            45.0,
            1.0,
            Point2D::new(10.0, 10.0),
        );
        assert!(warped.is_finite());

        let filters = vec![
            RasterFilter::BrightnessContrast {
                brightness: 0.1,
                contrast: 0.2,
            },
            RasterFilter::GaussianBlur { radius_px: 3.5 },
            RasterFilter::Desaturate,
            RasterFilter::Invert,
            RasterFilter::Posterize { levels: 4 },
            RasterFilter::Threshold { cutoff: 128 },
            RasterFilter::EdgeDetect { sensitivity: 1.0 },
        ];
        let fj = serde_json::to_string(&filters).unwrap();
        let fb: Vec<RasterFilter> = serde_json::from_str(&fj).unwrap();
        assert_eq!(filters, fb);
    }
}
