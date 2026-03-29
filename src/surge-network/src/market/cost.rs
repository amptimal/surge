// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Generator cost curves for OPF formulations.
//!
//! Supports MATPOWER cost types:
//! - Type 2: Polynomial cost (quadratic, linear, or higher order)
//! - Type 1: Piecewise-linear cost

use serde::{Deserialize, Serialize};
use tracing::debug;

/// A generator cost curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CostCurve {
    /// Polynomial cost: f(P) = c_{n-1} * P^{n-1} + ... + c_1 * P + c_0
    ///
    /// MATPOWER type 2. Coefficients stored highest-order first: `[c_{n-1}, ..., c_1, c_0]`.
    Polynomial {
        startup: f64,
        shutdown: f64,
        coeffs: Vec<f64>,
    },
    /// Piecewise-linear cost: interpolation between (MW, $/hr) breakpoints.
    ///
    /// MATPOWER type 1. Points sorted by MW ascending.
    PiecewiseLinear {
        startup: f64,
        shutdown: f64,
        points: Vec<(f64, f64)>,
    },
}

impl CostCurve {
    /// Evaluate cost at a given power output (MW).
    ///
    /// For polynomial: f(p) = c_{n-1} * p^{n-1} + ... + c_1 * p + c_0
    /// For piecewise-linear: linear interpolation between breakpoints.
    pub fn evaluate(&self, p_mw: f64) -> f64 {
        debug!(p_mw, "evaluating cost curve");
        match self {
            CostCurve::Polynomial { coeffs, .. } => {
                // Horner's method: coeffs = [c_{n-1}, c_{n-2}, ..., c_1, c_0]
                let mut result = 0.0;
                for &c in coeffs {
                    result = result * p_mw + c;
                }
                result
            }
            CostCurve::PiecewiseLinear { points, .. } => {
                if points.is_empty() {
                    return 0.0;
                }
                if points.len() == 1 {
                    return points[0].1;
                }
                // Clamp to first/last segment
                if p_mw <= points[0].0 {
                    return points[0].1;
                }
                if p_mw >= points[points.len() - 1].0 {
                    return points[points.len() - 1].1;
                }
                // Find segment and interpolate
                for i in 1..points.len() {
                    if p_mw <= points[i].0 {
                        let (x0, y0) = points[i - 1];
                        let (x1, y1) = points[i];
                        let dx = x1 - x0;
                        if dx.abs() < 1e-20 {
                            return y0;
                        }
                        return y0 + (y1 - y0) * (p_mw - x0) / dx;
                    }
                }
                points[points.len() - 1].1
            }
        }
    }

    /// Compute marginal cost (first derivative) at a given power output (MW).
    ///
    /// For polynomial: f'(p) = (n-1)*c_{n-1}*p^{n-2} + ... + c_1
    /// For piecewise-linear: slope of the active segment.
    pub fn marginal_cost(&self, p_mw: f64) -> f64 {
        debug!(p_mw, "computing marginal cost");
        match self {
            CostCurve::Polynomial { coeffs, .. } => {
                if coeffs.len() <= 1 {
                    return 0.0;
                }
                // Derivative coefficients: [(n-1)*c_{n-1}, (n-2)*c_{n-2}, ..., 1*c_1]
                // Using Horner's method on derivative
                let n = coeffs.len();
                let mut result = 0.0;
                for (i, &c) in coeffs[..n - 1].iter().enumerate() {
                    let power = (n - 1 - i) as f64;
                    result = result * p_mw + power * c;
                }
                result
            }
            CostCurve::PiecewiseLinear { points, .. } => {
                if points.len() < 2 {
                    return 0.0;
                }
                // Clamp to first/last segment slope
                if p_mw <= points[0].0 {
                    let (x0, y0) = points[0];
                    let (x1, y1) = points[1];
                    let dx = x1 - x0;
                    return if dx.abs() < 1e-20 {
                        0.0
                    } else {
                        (y1 - y0) / dx
                    };
                }
                if p_mw >= points[points.len() - 1].0 {
                    let n = points.len();
                    let (x0, y0) = points[n - 2];
                    let (x1, y1) = points[n - 1];
                    let dx = x1 - x0;
                    return if dx.abs() < 1e-20 {
                        0.0
                    } else {
                        (y1 - y0) / dx
                    };
                }
                for i in 1..points.len() {
                    if p_mw <= points[i].0 {
                        let (x0, y0) = points[i - 1];
                        let (x1, y1) = points[i];
                        let dx = x1 - x0;
                        return if dx.abs() < 1e-20 {
                            0.0
                        } else {
                            (y1 - y0) / dx
                        };
                    }
                }
                0.0
            }
        }
    }

    /// Compute second derivative at a given power output (MW).
    ///
    /// For polynomial: f''(p) = (n-1)*(n-2)*c_{n-1}*p^{n-3} + ...
    /// For piecewise-linear: always 0 (linear segments).
    pub fn second_derivative(&self, p_mw: f64) -> f64 {
        match self {
            CostCurve::Polynomial { coeffs, .. } => {
                if coeffs.len() <= 2 {
                    return 0.0;
                }
                let n = coeffs.len();
                let mut result = 0.0;
                for (i, &c) in coeffs[..n - 2].iter().enumerate() {
                    let power = (n - 1 - i) as f64;
                    let power_m1 = power - 1.0;
                    result = result * p_mw + power * power_m1 * c;
                }
                result
            }
            CostCurve::PiecewiseLinear { .. } => 0.0,
        }
    }

    /// Return the linear (marginal) coefficient in $/MWh evaluated at zero output.
    ///
    /// - `Polynomial { coeffs, .. }`: returns the second-to-last coefficient (c1 in
    ///   `c_{n-1}*P^{n-1} + ... + c_1*P + c_0`), i.e. `coeffs[coeffs.len()-2]`.
    ///   For a pure-constant curve (1 coeff) returns 0.0.
    /// - `PiecewiseLinear { points, .. }`: returns the slope of the first segment
    ///   (cheapest marginal cost), or 0.0 if fewer than two points.
    pub fn linear_coeff(&self) -> f64 {
        match self {
            CostCurve::Polynomial { coeffs, .. } => {
                // coeffs = [c_{n-1}, ..., c_1, c_0] (highest-order first)
                // c_1 is at index len-2
                if coeffs.len() < 2 {
                    return 0.0;
                }
                coeffs[coeffs.len() - 2]
            }
            CostCurve::PiecewiseLinear { points, .. } => {
                if points.len() < 2 {
                    return 0.0;
                }
                let (x0, y0) = points[0];
                let (x1, y1) = points[1];
                let dx = x1 - x0;
                if dx.abs() < 1e-20 {
                    0.0
                } else {
                    (y1 - y0) / dx
                }
            }
        }
    }

    /// Check if the cost curve is convex.
    ///
    /// Polynomial: convex if second derivative >= 0 everywhere in [pmin, pmax].
    /// For quadratic (3 coeffs), convex iff leading coefficient >= 0.
    /// Piecewise-linear: convex if slopes are non-decreasing.
    pub fn is_convex(&self) -> bool {
        match self {
            CostCurve::Polynomial { coeffs, .. } => {
                match coeffs.len() {
                    0 | 1 => true,         // constant or empty
                    2 => true,             // linear is convex
                    3 => coeffs[0] >= 0.0, // quadratic: convex iff a >= 0
                    _ => {
                        // For higher order, sample second derivative
                        // A rigorous check would analyze the polynomial,
                        // but in practice MATPOWER uses quadratic costs
                        coeffs[0] >= 0.0
                    }
                }
            }
            CostCurve::PiecewiseLinear { points, .. } => {
                if points.len() < 3 {
                    return true;
                }
                // Check slopes are non-decreasing
                for i in 2..points.len() {
                    let dx0 = points[i - 1].0 - points[i - 2].0;
                    let dy0 = points[i - 1].1 - points[i - 2].1;
                    let dx1 = points[i].0 - points[i - 1].0;
                    let dy1 = points[i].1 - points[i - 1].1;
                    if dx0.abs() < 1e-20 || dx1.abs() < 1e-20 {
                        continue;
                    }
                    let slope0 = dy0 / dx0;
                    let slope1 = dy1 / dx1;
                    if slope1 < slope0 - 1e-10 {
                        return false;
                    }
                }
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_polynomial_evaluate() {
        // case9 gen 1: 0.11*P^2 + 5*P + 150
        let cost = CostCurve::Polynomial {
            startup: 1500.0,
            shutdown: 0.0,
            coeffs: vec![0.11, 5.0, 150.0],
        };
        // f(0) = 150
        assert!((cost.evaluate(0.0) - 150.0).abs() < 1e-10);
        // f(100) = 0.11*10000 + 5*100 + 150 = 1100 + 500 + 150 = 1750
        assert!((cost.evaluate(100.0) - 1750.0).abs() < 1e-10);
        // f(72.3) = 0.11*(72.3^2) + 5*72.3 + 150
        let expected = 0.11 * 72.3 * 72.3 + 5.0 * 72.3 + 150.0;
        assert!((cost.evaluate(72.3) - expected).abs() < 1e-10);
    }

    #[test]
    fn test_polynomial_marginal_cost() {
        // f(P) = 0.11*P^2 + 5*P + 150
        // f'(P) = 0.22*P + 5
        let cost = CostCurve::Polynomial {
            startup: 1500.0,
            shutdown: 0.0,
            coeffs: vec![0.11, 5.0, 150.0],
        };
        assert!((cost.marginal_cost(0.0) - 5.0).abs() < 1e-10);
        assert!((cost.marginal_cost(100.0) - 27.0).abs() < 1e-10);
        assert!((cost.marginal_cost(50.0) - 16.0).abs() < 1e-10);
    }

    #[test]
    fn test_polynomial_second_derivative() {
        // f(P) = 0.11*P^2 + 5*P + 150
        // f''(P) = 0.22
        let cost = CostCurve::Polynomial {
            startup: 1500.0,
            shutdown: 0.0,
            coeffs: vec![0.11, 5.0, 150.0],
        };
        assert!((cost.second_derivative(0.0) - 0.22).abs() < 1e-10);
        assert!((cost.second_derivative(100.0) - 0.22).abs() < 1e-10);

        // Linear: f(P) = 5*P + 150 → f'' = 0
        let linear = CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![5.0, 150.0],
        };
        assert!((linear.second_derivative(50.0)).abs() < 1e-10);
    }

    #[test]
    fn test_polynomial_is_convex() {
        // Positive leading coefficient → convex
        let convex = CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.11, 5.0, 150.0],
        };
        assert!(convex.is_convex());

        // Negative leading coefficient → not convex
        let concave = CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![-0.11, 5.0, 150.0],
        };
        assert!(!concave.is_convex());

        // Linear → always convex
        let linear = CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![5.0, 150.0],
        };
        assert!(linear.is_convex());
    }

    #[test]
    fn test_piecewise_linear_evaluate() {
        // 3 breakpoints: (0, 0), (100, 1000), (200, 3000)
        let cost = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 0.0), (100.0, 1000.0), (200.0, 3000.0)],
        };
        // At breakpoints
        assert!((cost.evaluate(0.0)).abs() < 1e-10);
        assert!((cost.evaluate(100.0) - 1000.0).abs() < 1e-10);
        assert!((cost.evaluate(200.0) - 3000.0).abs() < 1e-10);
        // Midpoint of first segment: (50, 500)
        assert!((cost.evaluate(50.0) - 500.0).abs() < 1e-10);
        // Midpoint of second segment: (150, 2000)
        assert!((cost.evaluate(150.0) - 2000.0).abs() < 1e-10);
    }

    #[test]
    fn test_piecewise_linear_marginal_cost() {
        let cost = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 0.0), (100.0, 1000.0), (200.0, 3000.0)],
        };
        // First segment slope = 1000/100 = 10
        assert!((cost.marginal_cost(50.0) - 10.0).abs() < 1e-10);
        // Second segment slope = 2000/100 = 20
        assert!((cost.marginal_cost(150.0) - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_piecewise_linear_is_convex() {
        // Increasing slopes → convex
        let convex = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 0.0), (100.0, 1000.0), (200.0, 3000.0)],
        };
        assert!(convex.is_convex());

        // Decreasing slopes → not convex
        let not_convex = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 0.0), (100.0, 3000.0), (200.0, 4000.0)],
        };
        assert!(!not_convex.is_convex());
    }
}
