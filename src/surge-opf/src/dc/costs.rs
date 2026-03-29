// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared DC formulation helpers for generator cost modeling.

use std::collections::HashSet;

use surge_network::Network;
use surge_network::market::CostCurve;

use crate::dc::opf::DcOpfError;

#[derive(Debug, Clone)]
pub(crate) struct PwlGenInfoEntry {
    pub local_gen_index: usize,
    pub segments: Vec<(f64, f64)>,
}

pub(crate) fn build_pwl_gen_info(
    network: &Network,
    gen_indices: &[usize],
    base_mva: f64,
    use_pwl_costs: bool,
    pwl_cost_breakpoints: usize,
) -> Vec<PwlGenInfoEntry> {
    let mut pwl_gen_info = build_piecewise_linear_gen_info(network, gen_indices, base_mva);
    if use_pwl_costs {
        pwl_gen_info.extend(build_quadratic_pwl_gen_info(
            network,
            gen_indices,
            base_mva,
            pwl_cost_breakpoints,
        ));
    }
    pwl_gen_info
}

pub(crate) fn quadratic_pwl_local_indices(
    network: &Network,
    gen_indices: &[usize],
    use_pwl_costs: bool,
) -> HashSet<usize> {
    if !use_pwl_costs {
        return HashSet::new();
    }
    gen_indices
        .iter()
        .enumerate()
        .filter_map(|(local_idx, &gen_idx)| {
            let g = &network.generators[gen_idx];
            match &g.cost {
                Some(CostCurve::Polynomial { coeffs, .. })
                    if coeffs.len() >= 3 && coeffs[0].abs() > 1e-20 =>
                {
                    Some(local_idx)
                }
                _ => None,
            }
        })
        .collect()
}

pub(crate) fn has_mixed_quadratic_polynomial_costs(
    network: &Network,
    gen_indices: &[usize],
) -> bool {
    let mut saw_quadratic = false;
    let mut saw_non_quadratic = false;

    for &gen_idx in gen_indices {
        let g = &network.generators[gen_idx];
        let Some(CostCurve::Polynomial { coeffs, .. }) = &g.cost else {
            continue;
        };
        let has_quadratic = coeffs.len() >= 3 && coeffs[0].abs() > 1e-20;
        if has_quadratic {
            saw_quadratic = true;
        } else {
            saw_non_quadratic = true;
        }
        if saw_quadratic && saw_non_quadratic {
            return true;
        }
    }

    false
}

pub(crate) struct GeneratorCostBuffers<'a> {
    pub col_cost: &'a mut [f64],
    pub q_diag: &'a mut [f64],
    pub c0_total: &'a mut f64,
}

pub(crate) fn apply_generator_costs(
    network: &Network,
    gen_indices: &[usize],
    base_mva: f64,
    pg_offset: usize,
    buffers: GeneratorCostBuffers<'_>,
    poly_quad_local_indices: &HashSet<usize>,
) -> Result<(), DcOpfError> {
    for (local_idx, &gen_idx) in gen_indices.iter().enumerate() {
        let g = &network.generators[gen_idx];
        match g.cost.as_ref().ok_or(DcOpfError::MissingCost {
            gen_idx,
            bus: g.bus,
        })? {
            CostCurve::Polynomial { coeffs, .. } => {
                if poly_quad_local_indices.contains(&local_idx) {
                    continue;
                }
                match coeffs.len() {
                    0 => {}
                    1 => *buffers.c0_total += coeffs[0],
                    2 => {
                        buffers.col_cost[pg_offset + local_idx] = coeffs[0] * base_mva;
                        *buffers.c0_total += coeffs[1];
                    }
                    _ => {
                        if coeffs[0].abs() > 1e-20 {
                            buffers.q_diag[local_idx] = 2.0 * coeffs[0] * base_mva * base_mva;
                        }
                        buffers.col_cost[pg_offset + local_idx] = coeffs[1] * base_mva;
                        *buffers.c0_total += coeffs[2];
                    }
                }
            }
            CostCurve::PiecewiseLinear { .. } => {}
        }
    }
    Ok(())
}

pub(crate) fn build_hessian_csc(
    n_bus: usize,
    q_diag: &[f64],
    trailing_zero_cols: usize,
) -> Option<(Vec<i32>, Vec<i32>, Vec<f64>)> {
    if !q_diag.iter().any(|&v| v.abs() > 1e-20) {
        return None;
    }

    let total_cols = n_bus + q_diag.len() + trailing_zero_cols;
    let mut q_start = Vec::with_capacity(total_cols + 1);
    let mut q_index = Vec::new();
    let mut q_value = Vec::new();

    for _ in 0..n_bus {
        q_start.push(q_index.len() as i32);
    }
    for (local_idx, &qd) in q_diag.iter().enumerate() {
        q_start.push(q_index.len() as i32);
        if qd.abs() > 1e-20 {
            q_index.push((n_bus + local_idx) as i32);
            q_value.push(qd);
        }
    }
    for _ in 0..trailing_zero_cols {
        q_start.push(q_index.len() as i32);
    }
    q_start.push(q_index.len() as i32);

    Some((q_start, q_index, q_value))
}

fn build_piecewise_linear_gen_info(
    network: &Network,
    gen_indices: &[usize],
    base_mva: f64,
) -> Vec<PwlGenInfoEntry> {
    let mut pwl_gen_info = Vec::new();
    for (local_idx, &gen_idx) in gen_indices.iter().enumerate() {
        let g = &network.generators[gen_idx];
        if let Some(CostCurve::PiecewiseLinear { points, .. }) = &g.cost
            && points.len() >= 2
        {
            let mut segments = Vec::with_capacity(points.len() - 1);
            for segment_idx in 0..points.len() - 1 {
                let (p0, c0) = points[segment_idx];
                let (p1, c1) = points[segment_idx + 1];
                let dp = p1 - p0;
                if dp.abs() < 1e-20 {
                    continue;
                }
                let slope = (c1 - c0) / dp;
                let intercept = c0 - slope * p0;
                segments.push((slope * base_mva, intercept));
            }
            if !segments.is_empty() {
                pwl_gen_info.push(PwlGenInfoEntry {
                    local_gen_index: local_idx,
                    segments,
                });
            }
        }
    }
    pwl_gen_info
}

fn build_quadratic_pwl_gen_info(
    network: &Network,
    gen_indices: &[usize],
    base_mva: f64,
    pwl_cost_breakpoints: usize,
) -> Vec<PwlGenInfoEntry> {
    let n_breakpoints = pwl_cost_breakpoints.max(2);
    let mut pwl_gen_info = Vec::new();

    for (local_idx, &gen_idx) in gen_indices.iter().enumerate() {
        let g = &network.generators[gen_idx];
        if let Some(CostCurve::Polynomial { coeffs, .. }) = &g.cost
            && coeffs.len() >= 3
            && coeffs[0].abs() > 1e-20
        {
            let pmin_mw = g.pmin;
            let pmax_mw = g.pmax;
            if pmax_mw <= pmin_mw + 1e-6 {
                continue;
            }
            let c2 = coeffs[0];
            let c1 = coeffs[1];
            let c0 = coeffs[2];
            let mut segments = Vec::with_capacity(n_breakpoints);
            for breakpoint_idx in 0..n_breakpoints {
                let t = breakpoint_idx as f64 / (n_breakpoints - 1) as f64;
                let p_k_mw = pmin_mw + t * (pmax_mw - pmin_mw);
                let slope_mwh = 2.0 * c2 * p_k_mw + c1;
                let intercept = c0 - c2 * p_k_mw * p_k_mw;
                segments.push((slope_mwh * base_mva, intercept));
            }
            pwl_gen_info.push(PwlGenInfoEntry {
                local_gen_index: local_idx,
                segments,
            });
        }
    }

    pwl_gen_info
}
