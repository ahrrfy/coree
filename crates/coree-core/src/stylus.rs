// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hardware stylus / pen tablet sample data and configurable pressure response curves.

use serde::{Deserialize, Serialize};

/// Per-event hardware stylus / pen input sample (`pressure`, `tilt`, `twist`, `is_eraser`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StylusSample {
    /// Normalized pen pressure in `[0.0, 1.0]` (`1.0` when `has_pressure` is `false`).
    pub pressure: f32,
    /// `true` when `pressure` comes from a pressure-sensitive stylus or touch force event.
    pub has_pressure: bool,
    /// `[tilt_x_deg, tilt_y_deg]` pen tilt angles in degrees (`[-90.0, 90.0]`).
    pub tilt_deg: [f32; 2],
    /// Pen barrel rotation / twist angle in degrees (`[0.0, 360.0)`).
    pub twist_deg: f32,
    /// `true` when the active stylus tip is the inverted eraser end (`BTN_TOOL_RUBBER` / eraser).
    pub is_eraser: bool,
}

impl Default for StylusSample {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl StylusSample {
    /// Neutral default sample for mouse / non-pressure pointer events.
    pub const DEFAULT: Self = Self {
        pressure: 1.0,
        has_pressure: false,
        tilt_deg: [0.0, 0.0],
        twist_deg: 0.0,
        is_eraser: false,
    };

    /// Constructs a pressure-bearing sample with clamped `[0.0, 1.0]` pressure.
    #[must_use]
    pub fn with_pressure(pressure: f32) -> Self {
        let p = if pressure.is_finite() {
            pressure.clamp(0.0, 1.0)
        } else {
            1.0
        };
        Self {
            pressure: p,
            has_pressure: true,
            tilt_deg: [0.0, 0.0],
            twist_deg: 0.0,
            is_eraser: false,
        }
    }

    /// Returns sanitized pressure clamped to `[0.0, 1.0]`, or `1.0` if `!has_pressure`.
    #[must_use]
    pub fn normalized_pressure(self) -> f64 {
        if !self.has_pressure || !self.pressure.is_finite() {
            return 1.0;
        }
        f64::from(self.pressure).clamp(0.0, 1.0)
    }

    /// Maps this sample's pressure through `curve`, returning `[0.0, 1.0]` (or `1.0` for mouse).
    #[must_use]
    pub fn mapped_pressure(self, curve: &PressureCurve) -> f64 {
        if !self.has_pressure {
            return 1.0;
        }
        curve.evaluate(self.normalized_pressure())
    }
}

/// Piecewise-linear stylus pressure response curve (`[[in_x, out_y], ...]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PressureCurve {
    stops: Vec<[f64; 2]>,
}

impl Default for PressureCurve {
    fn default() -> Self {
        Self::linear()
    }
}

impl PressureCurve {
    /// Default identity linear curve `[[0.0, 0.0], [1.0, 1.0]]`.
    #[must_use]
    pub fn linear() -> Self {
        Self {
            stops: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }

    /// Parses a `pressure_curve_json` string (e.g., `"[[0.0,0.2],[1.0,1.0]]"`).
    ///
    /// Clamps coordinates to `[0.0, 1.0]`, drops non-finite pairs, sorts by `x`,
    /// deduplicates identical `x` stops (keeping the later value), and falls back
    /// to [`PressureCurve::linear`] if invalid or empty.
    #[must_use]
    pub fn from_json(json: &str) -> Self {
        let Ok(raw) = serde_json::from_str::<Vec<[f64; 2]>>(json.trim()) else {
            return Self::linear();
        };
        if raw.is_empty() {
            return Self::linear();
        }
        let mut clean: Vec<[f64; 2]> = raw
            .into_iter()
            .filter(|p| p[0].is_finite() && p[1].is_finite())
            .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
            .collect();
        if clean.is_empty() {
            return Self::linear();
        }
        clean.sort_by(|a, b| a[0].total_cmp(&b[0]));

        let mut deduped: Vec<[f64; 2]> = Vec::with_capacity(clean.len());
        for pt in clean {
            if let Some(last) = deduped.last_mut() {
                if (pt[0] - last[0]).abs() <= 1e-9 {
                    last[1] = pt[1];
                    continue;
                }
            }
            deduped.push(pt);
        }

        if deduped.len() == 1 {
            let y = deduped[0][1];
            return Self {
                stops: vec![[0.0, y], [1.0, y]],
            };
        }
        Self { stops: deduped }
    }

    /// Evaluates the mapped output pressure in `[0.0, 1.0]` for `raw_pressure`.
    #[must_use]
    pub fn evaluate(&self, raw_pressure: f64) -> f64 {
        if self.stops.is_empty() {
            return if raw_pressure.is_finite() {
                raw_pressure.clamp(0.0, 1.0)
            } else {
                1.0
            };
        }
        let x = if raw_pressure.is_finite() {
            raw_pressure.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let first = self.stops[0];
        if x <= first[0] {
            return first[1].clamp(0.0, 1.0);
        }
        let last = self.stops[self.stops.len() - 1];
        if x >= last[0] {
            return last[1].clamp(0.0, 1.0);
        }
        for win in self.stops.windows(2) {
            let p0 = win[0];
            let p1 = win[1];
            if x <= p1[0] {
                let span = p1[0] - p0[0];
                if span <= 1e-12 {
                    return p1[1].clamp(0.0, 1.0);
                }
                let t = ((x - p0[0]) / span).clamp(0.0, 1.0);
                return (p0[1] + (p1[1] - p0[1]) * t).clamp(0.0, 1.0);
            }
        }
        last[1].clamp(0.0, 1.0)
    }

    /// Returns the sanitized control stops `[[x, y], ...]`.
    #[must_use]
    pub fn stops(&self) -> &[[f64; 2]] {
        &self.stops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_curve_linear_and_custom_json() {
        let lin = PressureCurve::from_json("[[0.0,0.0],[1.0,1.0]]");
        assert!((lin.evaluate(0.0) - 0.0).abs() < 1e-9);
        assert!((lin.evaluate(0.5) - 0.5).abs() < 1e-9);
        assert!((lin.evaluate(1.0) - 1.0).abs() < 1e-9);

        let boosted = PressureCurve::from_json("[[0.0,0.2],[0.5,0.7],[1.0,1.0]]");
        assert!((boosted.evaluate(0.0) - 0.2).abs() < 1e-9);
        assert!((boosted.evaluate(0.25) - 0.45).abs() < 1e-9);
        assert!((boosted.evaluate(0.5) - 0.7).abs() < 1e-9);
        assert!((boosted.evaluate(0.75) - 0.85).abs() < 1e-9);
        assert!((boosted.evaluate(1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pressure_curve_malformed_and_clamping_fallback() {
        let bad = PressureCurve::from_json("not valid json");
        assert!((bad.evaluate(0.35) - 0.35).abs() < 1e-9);

        let empty = PressureCurve::from_json("[]");
        assert!((empty.evaluate(0.6) - 0.6).abs() < 1e-9);

        let unsorted = PressureCurve::from_json("[[1.5,2.0],[-0.2,-0.5],[0.5,0.25]]");
        assert!((unsorted.evaluate(-1.0) - 0.0).abs() < 1e-9);
        assert!((unsorted.evaluate(0.5) - 0.25).abs() < 1e-9);
        assert!((unsorted.evaluate(2.0) - 1.0).abs() < 1e-9);
        assert!((unsorted.evaluate(f64::NAN) - 1.0).abs() < 1e-9);
    }
}
