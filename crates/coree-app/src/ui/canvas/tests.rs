// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use coree_core::{Fill, GradientStop};
use coree_tools::{gradient_ray_local, gradient_stops_mut, interpolate_gradient_stop_color};

/// Inserts an interpolated color stop at normalized ray fraction `t` across any of the 4
/// interactive fountain fill gradient types.
fn insert_stop_on_fill_ray(fill: &Fill, t: f32) -> Option<Fill> {
    let clamped = t.clamp(0.02, 0.98);
    let mut updated = fill.clone();
    let (_, _, stops) = gradient_stops_mut(&mut updated)?;
    let color = interpolate_gradient_stop_color(stops, clamped);
    stops.push(GradientStop::new(clamped, color));
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Some(updated)
}

#[test]
fn interactive_fill_inserts_interpolated_stops_across_all_four_gradient_types() {
    let base_stops = vec![
        GradientStop::new(0.0, Color::rgba8(255, 0, 0, 255)),
        GradientStop::new(1.0, Color::rgba8(0, 0, 255, 255)),
    ];
    let variants = [
        Fill::LinearGradient {
            start: Point2D::ZERO,
            end: Point2D::new(40.0, 0.0),
            stops: base_stops.clone(),
        },
        Fill::RadialGradient {
            center: Point2D::ZERO,
            radius: 25.0,
            focal: Point2D::ZERO,
            stops: base_stops.clone(),
        },
        Fill::ConicalGradient {
            center: Point2D::ZERO,
            angle_deg: 0.0,
            stops: base_stops.clone(),
        },
        Fill::SquareGradient {
            center: Point2D::ZERO,
            size: 30.0,
            angle_deg: 45.0,
            stops: base_stops,
        },
    ];
    for fill in variants {
        let next = insert_stop_on_fill_ray(&fill, 0.5).expect("gradient fill accepts mid stop");
        let (_, _, stops) = gradient_ray_local(&next).unwrap();
        assert_eq!(stops.len(), 3);
        assert!((stops[1].offset - 0.5).abs() < 1e-6);
        let [r, _g, b, _] = stops[1].color.to_rgba8();
        assert!(
            r > 100 && b > 100,
            "midpoint color interpolated: r={r}, b={b}"
        );
    }
}
