// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC marginal loss sensitivity computation for loss-aware dispatch.
//!
//! Computes `∂P_loss_total / ∂P_inject_i` at each bus from branch flows
//! using the DC power flow approximation: `P_loss_branch ≈ r × flow²`.

use std::collections::HashMap;

use surge_dc::PtdfRows;
use surge_network::Network;

/// Compute DC marginal loss sensitivities at each bus from branch flows.
///
/// For each in-service branch with nonzero impedance:
///   `flow = b_dc × (θ_from - θ_to)`
///   `∂P_loss/∂P_inject_i = Σ_l 2 × r_l × flow_l × PTDF[l, i]`
///
/// Returns `dloss_dp[bus_idx]` for all buses (dimensionless, per-unit).
pub fn compute_dc_loss_sensitivities(
    network: &Network,
    theta: &[f64],
    bus_map: &HashMap<u32, usize>,
    ptdf: &PtdfRows,
) -> Vec<f64> {
    let n_bus = theta.len();
    let mut dloss_dp = vec![0.0; n_bus];

    for (branch_idx, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        if br.x.abs() < 1e-20 {
            continue;
        }
        let Some(&from) = bus_map.get(&br.from_bus) else {
            continue;
        };
        let Some(&to) = bus_map.get(&br.to_bus) else {
            continue;
        };

        let b_val = br.b_dc();
        let flow_pu = b_val * (theta[from] - theta[to]);
        let coeff = 2.0 * br.r * flow_pu;
        if coeff.abs() < 1e-20 {
            continue;
        }
        if let Some(row) = ptdf.row(branch_idx) {
            for (col_pos, &bus_idx) in ptdf.bus_indices().iter().enumerate() {
                dloss_dp[bus_idx] += coeff * row[col_pos];
            }
        }
    }

    dloss_dp
}

/// Compute total DC losses from branch flows: Σ r × flow².
pub fn compute_total_dc_losses(
    network: &Network,
    theta: &[f64],
    bus_map: &HashMap<u32, usize>,
) -> f64 {
    let mut total = 0.0;

    for br in &network.branches {
        if !br.in_service {
            continue;
        }
        if br.x.abs() < 1e-20 {
            continue;
        }
        let Some(&from) = bus_map.get(&br.from_bus) else {
            continue;
        };
        let Some(&to) = bus_map.get(&br.to_bus) else {
            continue;
        };

        let b_val = br.b_dc();
        let flow_pu = b_val * (theta[from] - theta[to]);
        total += br.r * flow_pu * flow_pu;
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_dc::{PtdfRequest, compute_ptdf, solve_dc};
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn make_3bus_loss_case() -> Network {
        let mut net = Network::new("dc-loss-factors");
        net.buses.extend([
            Bus::new(1, BusType::Slack, 230.0),
            Bus::new(2, BusType::PQ, 230.0),
            Bus::new(3, BusType::PQ, 230.0),
        ]);
        net.generators.push(Generator::new(1, 120.0, 1.0));
        net.loads.push(Load::new(2, 70.0, 0.0));
        net.loads.push(Load::new(3, 50.0, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.02, 0.10, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.03, 0.12, 0.0));
        net
    }

    #[test]
    fn dc_loss_sensitivities_match_injection_finite_difference() {
        let net = make_3bus_loss_case();
        let bus_map = net.bus_index_map();
        let theta = solve_dc(&net).expect("base DC PF").theta;
        let monitored_branches: Vec<usize> = (0..net.n_branches()).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&monitored_branches))
            .expect("PTDF for loss sensitivities");
        let dloss_dp = compute_dc_loss_sensitivities(&net, &theta, &bus_map, &ptdf);

        let eps_mw = 1e-3;
        let eps_pu = eps_mw / net.base_mva;
        let mut plus = net.clone();
        plus.loads[0].active_power_demand_mw -= eps_mw;
        let theta_plus = solve_dc(&plus).expect("plus DC PF").theta;
        let loss_plus = compute_total_dc_losses(&plus, &theta_plus, &plus.bus_index_map());

        let mut minus = net.clone();
        minus.loads[0].active_power_demand_mw += eps_mw;
        let theta_minus = solve_dc(&minus).expect("minus DC PF").theta;
        let loss_minus = compute_total_dc_losses(&minus, &theta_minus, &minus.bus_index_map());

        let fd = (loss_plus - loss_minus) / (2.0 * eps_pu);
        assert!(
            (dloss_dp[1] - fd).abs() < 1e-4,
            "analytic dloss/dpinj for bus 2 ({}) should match finite difference ({})",
            dloss_dp[1],
            fd
        );
    }
}
