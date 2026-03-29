// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Base-case and contingency voltage-stability helpers.
//!
//! The canonical public base-case entry point is [`compute_voltage_stress`],
//! which runs an AC power flow and returns the same exact/proxy result shape
//! used by contingency analysis.
//!
//! ## References
//! - Kessel, P. & Glavitsch, H. (1986). *IEEE Transactions on Power Delivery*, 1(3), 346-354.

pub(crate) mod stress_proxy;

use surge_ac::matrix::ybus::build_ybus;
use surge_ac::solve_ac_pf_kernel;
use surge_network::Network;
use surge_solution::{PfSolution, SolveStatus};

use crate::{ContingencyError, VoltageStressMode, VoltageStressOptions, VoltageStressResult};

use self::stress_proxy::compute_voltage_stress_summary;

/// Compute base-case voltage stress from an AC power flow solution.
///
/// The default [`VoltageStressOptions`] use the exact Kessel-Glavitsch
/// L-index path and return the same [`VoltageStressResult`] shape used for
/// post-contingency reporting.
pub fn compute_voltage_stress(
    network: &Network,
    options: &VoltageStressOptions,
) -> Result<VoltageStressResult, ContingencyError> {
    let solution = solve_ac_pf_kernel(network, &options.acpf_options)
        .map_err(|error| ContingencyError::BaseCaseFailed(error.to_string()))?;
    if solution.status != SolveStatus::Converged {
        return Err(ContingencyError::BaseCaseFailed(format!(
            "AC power flow did not converge: {} iterations, max_mismatch={:.2e}",
            solution.iterations, solution.max_mismatch
        )));
    }

    Ok(compute_voltage_stress_from_solution(
        network,
        &solution,
        &options.mode,
    ))
}

/// Compute voltage-stress metrics from a solved base case.
pub fn compute_voltage_stress_from_solution(
    network: &Network,
    solution: &PfSolution,
    mode: &VoltageStressMode,
) -> VoltageStressResult {
    let ybus = build_ybus(network);
    compute_voltage_stress_summary(
        network,
        &ybus,
        &solution.voltage_magnitude_pu,
        &solution.voltage_angle_rad,
        &solution.reactive_power_injection_pu,
        mode,
    )
    .into_option()
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_ac::{AcPfOptions, solve_ac_pf};
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn acpf_opts() -> AcPfOptions {
        AcPfOptions {
            tolerance: 1e-8,
            max_iterations: 50,
            flat_start: false,
            vm_min: 0.5,
            vm_max: 2.0,
            enforce_q_limits: false,
            ..Default::default()
        }
    }

    fn build_2bus_network(load_mw: f64, load_mvar: f64) -> Network {
        let mut bus1 = Bus::new(1, BusType::Slack, 100.0);
        bus1.voltage_magnitude_pu = 1.05;
        let bus2 = Bus::new(2, BusType::PQ, 100.0);

        let mut g1 = Generator::new(1, 300.0, 1.05);
        g1.qmax = 200.0;
        g1.qmin = -100.0;
        g1.pmax = 500.0;

        let br12 = Branch::new_line(1, 2, 0.02, 0.06, 0.02);

        let mut loads = vec![];
        if load_mw != 0.0 || load_mvar != 0.0 {
            loads.push(Load::new(2, load_mw, load_mvar));
        }

        Network {
            name: "test_2bus".to_string(),
            base_mva: 100.0,
            freq_hz: 60.0,
            buses: vec![bus1, bus2],
            branches: vec![br12],
            generators: vec![g1],
            loads,
            controls: Default::default(),
            market_data: Default::default(),
            ..Default::default()
        }
    }

    #[test]
    fn test_compute_voltage_stress_defaults_to_exact_l_index() {
        let net = build_2bus_network(80.0, 30.0);
        let result = compute_voltage_stress(&net, &VoltageStressOptions::default())
            .expect("base-case voltage-stress solve should converge");

        assert!(
            result.max_l_index.is_some(),
            "default base-case voltage-stress API should return exact L-index"
        );
        assert!(
            result.max_qv_stress_proxy.is_some(),
            "exact mode should still include the local proxy as a secondary metric"
        );
        assert_eq!(
            result.critical_l_index_bus,
            Some(2),
            "only PQ bus should be the critical L-index bus"
        );
        assert!(
            result.category.is_some(),
            "default base-case voltage-stress API should classify the result"
        );
    }

    #[test]
    fn test_compute_voltage_stress_exact_metric_increases_when_stressed() {
        let light = build_2bus_network(20.0, 5.0);
        let heavy = build_2bus_network(120.0, 50.0);

        let light_result = compute_voltage_stress(&light, &VoltageStressOptions::default())
            .expect("light case should converge");
        let heavy_result = compute_voltage_stress(&heavy, &VoltageStressOptions::default())
            .expect("heavy case should converge");

        assert!(
            heavy_result.max_l_index.unwrap_or_default()
                > light_result.max_l_index.unwrap_or_default(),
            "heavier loading should increase the exact L-index"
        );
    }

    #[test]
    fn test_compute_voltage_stress_proxy_mode_returns_proxy_only() {
        let net = build_2bus_network(80.0, 30.0);
        let solution = solve_ac_pf(&net, &acpf_opts()).expect("proxy case should converge");
        let result =
            compute_voltage_stress_from_solution(&net, &solution, &VoltageStressMode::Proxy);

        assert!(
            result.max_qv_stress_proxy.is_some(),
            "proxy mode should populate the local Q-V proxy summary"
        );
        assert!(
            result.max_l_index.is_none(),
            "proxy mode must not expose an exact L-index"
        );
        assert!(
            result.category.is_none(),
            "proxy mode must not classify exact L-index stability categories"
        );
    }
}
