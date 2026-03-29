// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-aware ATC surface and implementation.

use surge_network::Network;
use tracing::info;

use crate::atc::compute_nerc_atc;
use crate::dfax::validate_transfer_path;
use crate::error::TransferError;
use crate::types::{AcAtcRequest, AtcMargins, AtcOptions, NercAtcRequest};

pub use crate::types::{AcAtcLimitingConstraint, AcAtcResult};

use surge_ac::{AcPfError, AcPfOptions, solve_ac_pf_kernel};

fn validate_voltage_band(request: &AcAtcRequest) -> Result<(), TransferError> {
    if !request.v_min_pu.is_finite() || !request.v_max_pu.is_finite() {
        return Err(TransferError::InvalidRequest(format!(
            "AC ATC voltage limits for path '{}' must be finite",
            request.path.name
        )));
    }
    if request.v_min_pu <= 0.0 {
        return Err(TransferError::InvalidRequest(format!(
            "AC ATC v_min_pu for path '{}' must be > 0.0, got {}",
            request.path.name, request.v_min_pu
        )));
    }
    if request.v_min_pu >= request.v_max_pu {
        return Err(TransferError::InvalidRequest(format!(
            "AC ATC voltage band for path '{}' must satisfy v_min_pu < v_max_pu (got {} >= {})",
            request.path.name, request.v_min_pu, request.v_max_pu
        )));
    }
    Ok(())
}

fn perturbed_voltage_magnitudes_or_error(
    path_name: &str,
    vm_perturbed: Option<Vec<f64>>,
) -> Result<Vec<f64>, TransferError> {
    vm_perturbed.ok_or_else(|| {
        TransferError::AcPowerFlow(format!(
            "perturbed FDPF voltage sensitivity solve failed for transfer path '{path_name}'"
        ))
    })
}

/// Compute AC-aware ATC that enforces both thermal branch limits and voltage margins.
pub fn compute_ac_atc(
    network: &Network,
    request: &AcAtcRequest,
) -> Result<AcAtcResult, TransferError> {
    compute_ac_atc_with_options(network, request, &AcPfOptions::default())
}

pub(crate) fn compute_ac_atc_with_options(
    network: &Network,
    request: &AcAtcRequest,
    acpf_opts: &AcPfOptions,
) -> Result<AcAtcResult, TransferError> {
    validate_transfer_path(network, &request.path)?;
    validate_voltage_band(request)?;

    let source_buses = &request.path.source_buses;
    let sink_buses = &request.path.sink_buses;
    let v_min_pu = request.v_min_pu;
    let v_max_pu = request.v_max_pu;
    info!(
        path = %request.path.name,
        sources = source_buses.len(),
        sinks = sink_buses.len(),
        v_min = v_min_pu,
        v_max = v_max_pu,
        "computing AC-aware ATC"
    );

    use surge_ac::FdpfFactors;
    use surge_ac::matrix::ybus::build_ybus;

    let bus_map = network.bus_index_map();
    let n = network.n_buses();
    let base_mva = network.base_mva;

    for &bus in source_buses.iter().chain(sink_buses.iter()) {
        if !bus_map.contains_key(&bus) {
            return Err(TransferError::InvalidRequest(format!(
                "bus {bus} not found in network"
            )));
        }
    }

    let base_sol = match solve_ac_pf_kernel(network, acpf_opts) {
        Ok(solution) => solution,
        Err(AcPfError::NotConverged {
            iterations,
            max_mismatch,
            ..
        }) => {
            return Err(TransferError::AcPowerFlow(format!(
                "base-case AC PF did not converge: status=NotConverged, max_mismatch={max_mismatch:.3e} after {iterations} iterations"
            )));
        }
        Err(error) => {
            return Err(TransferError::AcPowerFlow(format!(
                "base-case AC PF failed: {error}"
            )));
        }
    };
    if base_sol.status != surge_solution::SolveStatus::Converged {
        return Err(TransferError::AcPowerFlow(format!(
            "base-case AC PF did not converge: status={:?}, max_mismatch={:.3e} after {} iterations",
            base_sol.status, base_sol.max_mismatch, base_sol.iterations
        )));
    }
    let vm0 = &base_sol.voltage_magnitude_pu;

    let delta_p_pu = 0.01_f64;
    let p_spec_base = network.bus_p_injection_pu();
    let q_spec_base = network.bus_q_injection_pu();
    let mut p_spec_perturbed = p_spec_base.clone();

    let n_src = source_buses.len() as f64;
    let n_snk = sink_buses.len() as f64;

    for &bus in source_buses {
        let idx = bus_map[&bus];
        p_spec_perturbed[idx] += delta_p_pu / n_src;
    }
    for &bus in sink_buses {
        let idx = bus_map[&bus];
        p_spec_perturbed[idx] -= delta_p_pu / n_snk;
    }

    let ybus = build_ybus(network);
    let mut fdpf = FdpfFactors::new(network)
        .map_err(|_| TransferError::AcPowerFlow("FDPF matrix factorization failed".to_string()))?;

    let vm_init = vm0.clone();
    let va_init = base_sol.voltage_angle_rad.clone();

    let vm_perturbed = perturbed_voltage_magnitudes_or_error(
        &request.path.name,
        fdpf.solve_from_ybus(
            &ybus,
            &p_spec_perturbed,
            &q_spec_base,
            &vm_init,
            &va_init,
            1e-6,
            50,
        )
        .map(|r| r.vm),
    )?;

    let dv_dp: Vec<f64> = (0..n)
        .map(|i| (vm_perturbed[i] - vm0[i]) / delta_p_pu)
        .collect();

    let mut voltage_limit_pu = f64::INFINITY;
    let mut limiting_bus: Option<usize> = None;

    for i in 0..n {
        let v0 = vm0[i];
        let sens = dv_dp[i];

        if sens.abs() < 1e-12 {
            continue;
        }

        let headroom = if sens > 0.0 {
            (v_max_pu - v0) / sens
        } else {
            (v0 - v_min_pu) / (-sens)
        };

        let clamped = headroom.max(0.0);
        if clamped < voltage_limit_pu {
            voltage_limit_pu = clamped;
            limiting_bus = Some(i);
        }
    }

    let voltage_limit_mw = if voltage_limit_pu.is_infinite() {
        f64::INFINITY
    } else {
        voltage_limit_pu * base_mva
    };

    let monitored_all: Vec<usize> = (0..network.n_branches()).collect();
    let (thermal_limit_mw, thermal_binding_branch) =
        if source_buses.is_empty() || sink_buses.is_empty() {
            (f64::INFINITY, None)
        } else {
            let nerc = compute_nerc_atc(
                network,
                &NercAtcRequest {
                    path: request.path.clone(),
                    options: AtcOptions {
                        monitored_branches: Some(monitored_all),
                        contingency_branches: None,
                        margins: AtcMargins {
                            trm_fraction: 0.0,
                            cbm_mw: 0.0,
                            etc_mw: 0.0,
                        },
                    },
                },
            )?;
            (nerc.ttc_mw, nerc.limit_cause.monitored_branch())
        };

    let (atc_mw, limiting_constraint) = if thermal_limit_mw <= voltage_limit_mw {
        (thermal_limit_mw, AcAtcLimitingConstraint::Thermal)
    } else {
        (voltage_limit_mw, AcAtcLimitingConstraint::Voltage)
    };

    let final_limiting_bus = if limiting_constraint == AcAtcLimitingConstraint::Voltage {
        limiting_bus
    } else {
        None
    };

    let final_binding_branch = if limiting_constraint == AcAtcLimitingConstraint::Thermal {
        thermal_binding_branch
    } else {
        None
    };

    info!(
        atc_mw = atc_mw,
        thermal_limit_mw = thermal_limit_mw,
        voltage_limit_mw = voltage_limit_mw,
        limiting_constraint = %limiting_constraint,
        "AC-aware ATC computed"
    );

    Ok(AcAtcResult {
        atc_mw,
        thermal_limit_mw,
        voltage_limit_mw,
        limiting_bus: final_limiting_bus,
        binding_branch: final_binding_branch,
        limiting_constraint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::case_path;
    use crate::types::{AcAtcRequest, AtcOptions, NercAtcRequest, TransferPath};
    use surge_ac::AcPfOptions;
    use surge_network::Network;
    use surge_network::network::{Bus, BusType};

    fn load_case14() -> surge_network::Network {
        surge_io::load(case_path("case14")).expect("parse case14")
    }

    fn validation_network() -> Network {
        let mut net = Network::new("ac_atc_validation");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net
    }

    fn ac_request(
        source_buses: Vec<u32>,
        sink_buses: Vec<u32>,
        v_min_pu: f64,
        v_max_pu: f64,
    ) -> AcAtcRequest {
        AcAtcRequest::new(
            TransferPath::new("ac_test_path", source_buses, sink_buses),
            v_min_pu,
            v_max_pu,
        )
    }

    fn thermal_request(
        source_bus: u32,
        sink_bus: u32,
        monitored_branches: Vec<usize>,
        margins: AtcMargins,
    ) -> NercAtcRequest {
        NercAtcRequest {
            path: TransferPath::new("thermal_test_path", vec![source_bus], vec![sink_bus]),
            options: AtcOptions {
                monitored_branches: Some(monitored_branches),
                contingency_branches: None,
                margins,
            },
        }
    }

    #[test]
    fn test_nerc_ac_atc_is_leq_thermal_atc() {
        let net = load_case14();

        let monitored: Vec<usize> = (0..net.n_branches()).collect();
        let thermal_result = compute_nerc_atc(
            &net,
            &thermal_request(
                1,
                14,
                monitored,
                AtcMargins {
                    trm_fraction: 0.0,
                    cbm_mw: 0.0,
                    etc_mw: 0.0,
                },
            ),
        )
        .expect("thermal ATC should succeed");

        let ac_result = compute_ac_atc(&net, &ac_request(vec![1], vec![14], 0.95, 1.05))
            .expect("AC-aware ATC should succeed");

        assert!(
            ac_result.atc_mw <= thermal_result.atc_mw + 1e-6,
            "AC-aware ATC ({:.4} MW) must be ≤ thermal ATC ({:.4} MW)",
            ac_result.atc_mw,
            thermal_result.atc_mw
        );
    }

    #[test]
    fn test_nerc_ac_atc_limiting_bus_valid() {
        let net = load_case14();

        let result = compute_ac_atc(&net, &ac_request(vec![1], vec![14], 0.95, 1.05))
            .expect("AC-aware ATC should succeed");

        if let Some(bus_idx) = result.limiting_bus {
            assert!(
                bus_idx < net.n_buses(),
                "limiting_bus index {bus_idx} out of range (n_buses = {})",
                net.n_buses()
            );
        }

        assert!(
            matches!(
                result.limiting_constraint,
                AcAtcLimitingConstraint::Thermal | AcAtcLimitingConstraint::Voltage
            ),
            "unexpected limiting constraint: {:?}",
            result.limiting_constraint
        );
    }

    #[test]
    fn test_nerc_ac_atc_wide_voltage_band_equals_thermal() {
        let net = surge_io::load(case_path("case9")).expect("parse case9");

        let monitored: Vec<usize> = (0..net.n_branches()).collect();
        let thermal_result = compute_nerc_atc(
            &net,
            &thermal_request(
                1,
                9,
                monitored,
                AtcMargins {
                    trm_fraction: 0.0,
                    cbm_mw: 0.0,
                    etc_mw: 0.0,
                },
            ),
        )
        .expect("thermal ATC should succeed");

        let ac_result = compute_ac_atc(&net, &ac_request(vec![1], vec![9], 0.5, 1.5))
            .expect("AC-aware ATC should succeed");

        assert_eq!(
            ac_result.limiting_constraint,
            AcAtcLimitingConstraint::Thermal,
            "wide voltage band should result in thermal-limited ATC on case9, got: {:?}",
            ac_result.limiting_constraint
        );

        assert!(
            (ac_result.atc_mw - thermal_result.atc_mw).abs() < 1.0,
            "AC-aware ATC ({:.4} MW) should equal thermal ATC ({:.4} MW) with wide voltage band",
            ac_result.atc_mw,
            thermal_result.atc_mw
        );
    }

    #[test]
    fn test_nerc_ac_atc_invalid_bus_returns_err() {
        let net = load_case14();
        let result = compute_ac_atc(&net, &ac_request(vec![999], vec![14], 0.95, 1.05));
        assert!(
            result.is_err(),
            "expected Err for non-existent source_bus 999"
        );
    }

    #[test]
    fn test_nerc_ac_atc_empty_buses_rejected() {
        let net = load_case14();
        let result = compute_ac_atc(&net, &ac_request(vec![], vec![], 0.95, 1.05));
        assert!(
            result.is_err(),
            "empty source/sink lists should be rejected"
        );
    }

    #[test]
    fn test_validate_voltage_band_rejects_invalid_ranges() {
        let request = ac_request(vec![1], vec![2], 1.05, 0.95);
        let err = validate_voltage_band(&request).expect_err("invalid voltage range must fail");
        assert!(
            err.to_string().contains("v_min_pu < v_max_pu"),
            "unexpected error: {err}"
        );

        let nan_request = AcAtcRequest::new(
            TransferPath::new("nan_band", vec![1], vec![2]),
            f64::NAN,
            1.05,
        );
        let err = validate_voltage_band(&nan_request).expect_err("NaN voltage bound must fail");
        assert!(err.to_string().contains("must be finite"));
    }

    #[test]
    fn test_perturbed_voltage_magnitudes_missing_solution_is_error() {
        let err = perturbed_voltage_magnitudes_or_error("ac_test_path", None)
            .expect_err("missing perturbation solution must fail");
        assert!(
            err.to_string()
                .contains("perturbed FDPF voltage sensitivity solve failed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_compute_ac_atc_rejects_invalid_voltage_band_before_pf() {
        let net = validation_network();
        let result = compute_ac_atc(&net, &ac_request(vec![1], vec![2], 1.05, 0.95));
        let err = result.expect_err("invalid voltage band must be rejected");
        assert!(
            err.to_string().contains("v_min_pu < v_max_pu"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_compute_ac_atc_rejects_nonconverged_base_case() {
        let net = load_case14();
        let opts = AcPfOptions {
            max_iterations: 0,
            ..Default::default()
        };

        let err =
            compute_ac_atc_with_options(&net, &ac_request(vec![1], vec![14], 0.95, 1.05), &opts)
                .expect_err("nonconverged base AC PF must fail AC-ATC");
        assert!(
            err.to_string().contains("base-case AC PF did not converge"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_compute_ac_atc_rejects_duplicate_source_bus() {
        let net = validation_network();
        let result = compute_ac_atc(&net, &ac_request(vec![1, 1], vec![2], 0.95, 1.05));
        let err = result.expect_err("duplicate source bus must be rejected");
        assert!(
            err.to_string().contains("duplicate source bus 1"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_compute_ac_atc_rejects_overlapping_source_and_sink_bus() {
        let net = validation_network();
        let result = compute_ac_atc(&net, &ac_request(vec![1], vec![1], 0.95, 1.05));
        let err = result.expect_err("overlapping source/sink bus must be rejected");
        assert!(
            err.to_string().contains("cannot be both source and sink"),
            "unexpected error: {err}"
        );
    }
}
