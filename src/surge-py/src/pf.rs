// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Power flow solver entry points: DC, AC (Newton-Raphson), and HVDC.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::exceptions::{NetworkError, SurgeError, extract_panic_msg, to_pyerr};
use crate::network::Network;
use crate::solutions::{AcPfResult, DcPfResult, HvdcSolution, dc_pf_result_from_result};

/// Solve DC power flow. Returns (theta, branch_flows, slack_injection).
///
/// When ``headroom_slack=True``, the power imbalance is redistributed across
/// in-service generator buses according to available generator headroom
/// instead of being absorbed entirely by the single slack bus. ``angle_reference``
/// controls only the reported output angle convention.
#[pyfunction]
#[pyo3(signature = (network, headroom_slack = false, headroom_slack_buses = None, participation_factors = None, angle_reference = "preserve_initial"))]
pub fn solve_dc_pf(
    network: &Network,
    headroom_slack: bool,
    headroom_slack_buses: Option<Vec<u32>>,
    participation_factors: Option<std::collections::HashMap<u32, f64>>,
    angle_reference: &str,
) -> PyResult<DcPfResult> {
    use surge_dc::solve_dc_opts;
    // Fix 4: validate before releasing the GIL (panics inside allow_threads → abort).
    network.validate()?;
    let net = Arc::clone(&network.inner);
    let opts = build_dc_pf_options(
        &net,
        headroom_slack,
        headroom_slack_buses.as_deref(),
        participation_factors.as_ref(),
        angle_reference,
    )?;

    let result = Python::attach(|py| {
        py.detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| solve_dc_opts(&net, &opts)))
                .map_err(|e| format!("solve_dc_pf failed: {}", extract_panic_msg(e)))
                .and_then(|r| r.map_err(|e| e.to_string()))
        })
    })
    .map_err(to_pyerr)?;
    Ok(dc_pf_result_from_result(Arc::clone(&net), result))
}

pub(crate) fn build_dc_pf_options(
    net: &surge_network::Network,
    headroom_slack: bool,
    headroom_slack_buses: Option<&[u32]>,
    participation_factors: Option<&std::collections::HashMap<u32, f64>>,
    angle_reference: &str,
) -> PyResult<surge_dc::DcPfOptions> {
    use surge_dc::DcPfOptions;

    let angle_reference = parse_angle_reference(angle_reference)?;

    // Participation factors take precedence.
    if let Some(pf_map) = participation_factors {
        let bus_map = net.bus_index_map();
        let mut weights = Vec::with_capacity(pf_map.len());
        for (&bus_num, &factor) in pf_map {
            let idx = bus_map.get(&bus_num).ok_or_else(|| {
                NetworkError::new_err(format!(
                    "participation_factors: bus {bus_num} not found in network"
                ))
            })?;
            weights.push((*idx, factor));
        }
        return Ok(
            DcPfOptions::with_participation_factors(&weights).with_angle_reference(angle_reference)
        );
    }

    if let Some(buses) = headroom_slack_buses {
        let bus_map = net.bus_index_map();
        let mut indices = Vec::with_capacity(buses.len());
        for &bus_num in buses {
            let idx = bus_map.get(&bus_num).ok_or_else(|| {
                NetworkError::new_err(format!(
                    "headroom_slack_buses: bus {bus_num} not found in network"
                ))
            })?;
            indices.push(*idx);
        }
        return Ok(DcPfOptions::with_headroom_slack(&indices).with_angle_reference(angle_reference));
    }

    if headroom_slack {
        let gen_bus_indices: Vec<usize> = net
            .generators
            .iter()
            .filter(|g| g.in_service)
            .filter_map(|g| net.buses.iter().position(|b| b.number == g.bus))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        return Ok(DcPfOptions::with_headroom_slack(&gen_bus_indices)
            .with_angle_reference(angle_reference));
    }

    Ok(DcPfOptions::default().with_angle_reference(angle_reference))
}

fn parse_angle_reference(angle_reference: &str) -> PyResult<surge_network::AngleReference> {
    Ok(match angle_reference {
        "preserve_initial" => surge_network::AngleReference::PreserveInitial,
        "zero" => surge_network::AngleReference::Zero,
        "distributed" | "distributed_load" => surge_network::AngleReference::Distributed(
            surge_network::DistributedAngleWeight::LoadWeighted,
        ),
        "distributed_generation" => surge_network::AngleReference::Distributed(
            surge_network::DistributedAngleWeight::GenerationWeighted,
        ),
        "distributed_inertia" => surge_network::AngleReference::Distributed(
            surge_network::DistributedAngleWeight::InertiaWeighted,
        ),
        other => {
            return Err(PyValueError::new_err(format!(
                "unsupported angle_reference '{other}'; expected 'preserve_initial', 'zero', \
             'distributed', 'distributed_load', 'distributed_generation', or 'distributed_inertia'"
            )));
        }
    })
}

/// Solve AC power flow using Newton-Raphson (KLU sparse solver).
///
/// If the network has OLTC controls (added via ``network.add_oltc_control()``)
/// or switched shunts (via ``network.add_switched_shunt()``), an outer loop
/// iterates until all controls are within their dead-bands or ``oltc_max_iter``
/// outer iterations are exhausted.
///
/// Args:
///   network:              The network to solve.
///   tolerance:            Convergence tolerance (max power mismatch in p.u.). Default 1e-8.
///   max_iterations:       Maximum Newton-Raphson iterations. Default 100.
///   flat_start:           If True, initialise from Vm=1, Va=0 instead of case data.
///   oltc:                 Enable OLTC tap-changer outer loop. Default True.
///   switched_shunts:      Enable switched-shunt voltage control outer loop. Default True.
///   oltc_max_iter:        Maximum outer-loop iterations for OLTC/shunt control. Default 20.
///   distributed_slack:    If True, distribute slack equally across all in-service generators.
///   slack_participation:  Dict mapping external bus number → participation factor (must sum to 1).
///   enforce_interchange:  Enforce area interchange targets from network.area_schedules.
///   interchange_max_iter: Maximum area-interchange correction iterations. Default 10.
///   startup_policy:      Startup policy for non-flat solves without an explicit warm start.
///                         ``"single"`` (default) runs one attempt. ``"adaptive"`` escalates
///                         through fallbacks on failure. ``"parallel_warm_and_flat"`` races
///                         warm and flat starts.
///
/// Returns:
///   AcPfResult with vm, va, active_power_injection_pu_mw, reactive_power_injection_pu_mvar arrays.
///   If the solver does not converge, the result has ``converged=False`` and
///   contains the partial voltage profile from the last NR iterate.
///
/// Raises:
///   NetworkError:     If the network is structurally invalid (no slack bus, etc.).
///   ValueError:       If tolerance or max_iterations are out of range.
#[pyfunction]
#[pyo3(signature = (
    network,
    tolerance = 1e-8,
    max_iterations = 100,
    flat_start = false,
    oltc = true,
    switched_shunts = true,
    oltc_max_iter = 20,
    distributed_slack = true,
    slack_participation = None,
    enforce_interchange = false,
    interchange_max_iter = 10,
    enforce_q_limits = true,
    enforce_gen_p_limits = true,
    merge_zero_impedance = false,
    dc_warm_start = true,
    startup_policy = "adaptive",
    q_sharing = "capability",
    warm_start = None,
    line_search = true,
    detect_islands = true,
    dc_line_model = "fixed_schedule",
    record_convergence_history = false,
    vm_min = 0.5,
    vm_max = 1.5,
    angle_reference = "preserve_initial",
))]
pub fn solve_ac_pf(
    py: Python<'_>,
    network: &Network,
    tolerance: f64,
    max_iterations: u32,
    flat_start: bool,
    oltc: bool,
    switched_shunts: bool,
    oltc_max_iter: usize,
    distributed_slack: bool,
    slack_participation: Option<std::collections::HashMap<u32, f64>>,
    enforce_interchange: bool,
    interchange_max_iter: usize,
    enforce_q_limits: bool,
    enforce_gen_p_limits: bool,
    merge_zero_impedance: bool,
    dc_warm_start: bool,
    startup_policy: &str,
    q_sharing: &str,
    warm_start: Option<&AcPfResult>,
    line_search: bool,
    detect_islands: bool,
    dc_line_model: &str,
    record_convergence_history: bool,
    vm_min: f64,
    vm_max: f64,
    angle_reference: &str,
) -> PyResult<AcPfResult> {
    // Validate inputs before releasing the GIL.
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Err(PyValueError::new_err(format!(
            "tolerance must be a finite positive number, got {tolerance}"
        )));
    }
    if max_iterations > 10_000 {
        return Err(PyValueError::new_err(format!(
            "max_iterations={max_iterations} exceeds limit of 10,000"
        )));
    }
    network.validate()?;

    // Convert external bus number → internal index for slack participation.
    let internal_participation = if let Some(ref ext_map) = slack_participation {
        let bus_map = network.inner.bus_index_map();
        let mut internal = std::collections::HashMap::with_capacity(ext_map.len());
        for (&bus_num, &factor) in ext_map {
            let idx = bus_map.get(&bus_num).ok_or_else(|| {
                NetworkError::new_err(format!(
                    "slack_participation: bus {bus_num} not found in network"
                ))
            })?;
            internal.insert(*idx, factor);
        }
        Some(internal)
    } else {
        None
    };

    // Auto-convert OltcSpec (external bus numbers, from PSS/E) → OltcControl (0-based indices).
    // These are merged with any manually registered oltc_controls, deduplicating by branch_index
    // so a PSS/E spec and a manual add_oltc_control() call don't conflict.
    let mut merged_oltc = network.oltc_controls.clone();
    if oltc {
        let bus_map = network.inner.bus_index_map();
        for spec in &network.inner.controls.oltc_specs {
            // Find the transformer branch matching from_bus/to_bus/circuit.
            let branch_index = network.inner.branches.iter().position(|b| {
                b.from_bus == spec.from_bus && b.to_bus == spec.to_bus && b.circuit == spec.circuit
            });
            let Some(branch_index) = branch_index else {
                continue; // skip silently if branch not found (e.g. star-bus split)
            };
            // Skip if a manually registered control already targets this branch.
            if merged_oltc.iter().any(|c| c.branch_index == branch_index) {
                continue;
            }
            let reg_bus_ext = if spec.regulated_bus == 0 {
                spec.to_bus
            } else {
                spec.regulated_bus
            };
            let Some(&bus_regulated) = bus_map.get(&reg_bus_ext) else {
                continue;
            };
            merged_oltc.push(surge_network::network::discrete_control::OltcControl {
                branch_index,
                bus_regulated,
                v_target: spec.v_target,
                v_band: spec.v_band,
                tap_min: spec.tap_min,
                tap_max: spec.tap_max,
                tap_step: spec.tap_step,
            });
        }
    }

    // Auto-convert ParSpec (external bus numbers, from PSS/E) → ParControl (0-based branch indices).
    // Only built when oltc=True (PAR is a discrete-control outer-loop feature like OLTC).
    let mut par_controls: Vec<surge_network::network::discrete_control::ParControl> = Vec::new();
    if oltc {
        for spec in &network.inner.controls.par_specs {
            let branch_index = network.inner.branches.iter().position(|b| {
                b.from_bus == spec.from_bus && b.to_bus == spec.to_bus && b.circuit == spec.circuit
            });
            let Some(branch_index) = branch_index else {
                continue;
            };
            // Resolve the monitored branch.
            // PSS/E CONT1 for COD1=3 gives only the from-bus of the monitored branch
            // (monitored_to_bus == 0). When only one endpoint is known, find the first
            // branch that touches that bus. Fall back to the PAR branch itself if none found.
            let monitored_branch_index = if spec.monitored_from_bus == 0 {
                // CONT1=0: monitor the PAR branch itself.
                branch_index
            } else if spec.monitored_to_bus == 0 {
                // CONT1 specifies from-bus only (PSS/E 2-W case): find first in-service branch
                // touching that bus, excluding the PAR branch itself (it also touches that bus
                // and would otherwise be the first hit when CONT1 names a different line).
                network
                    .inner
                    .branches
                    .iter()
                    .enumerate()
                    .find(|(i, b)| {
                        *i != branch_index
                            && b.in_service
                            && (b.from_bus == spec.monitored_from_bus
                                || b.to_bus == spec.monitored_from_bus)
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(branch_index) // fall back to monitoring PAR branch
            } else {
                // Both endpoints known: exact lookup by from_bus/to_bus/circuit.
                let found = network.inner.branches.iter().position(|b| {
                    b.from_bus == spec.monitored_from_bus
                        && b.to_bus == spec.monitored_to_bus
                        && b.circuit == spec.monitored_circuit
                });
                let Some(idx) = found else { continue };
                idx
            };
            par_controls.push(surge_network::network::discrete_control::ParControl {
                branch_index,
                monitored_branch_index,
                p_target_mw: spec.p_target_mw,
                p_band_mw: spec.p_band_mw,
                angle_min_deg: spec.angle_min_deg,
                angle_max_deg: spec.angle_max_deg,
                ang_step_deg: spec.ang_step_deg,
            });
        }
    }

    let opts = surge_ac::AcPfOptions {
        tolerance,
        max_iterations,
        flat_start,
        oltc_enabled: oltc,
        oltc_max_iter,
        oltc_controls: merged_oltc,
        par_enabled: oltc && !par_controls.is_empty(),
        par_controls,
        shunt_enabled: switched_shunts,
        shunt_max_iter: oltc_max_iter,
        switched_shunts: network.switched_shunts.clone(),
        distributed_slack,
        slack_participation: internal_participation,
        enforce_interchange,
        interchange_max_iter,
        enforce_q_limits,
        enforce_gen_p_limits,
        dc_warm_start,
        startup_policy: match startup_policy {
            "single" => surge_ac::StartupPolicy::Single,
            "adaptive" => surge_ac::StartupPolicy::Adaptive,
            "parallel_warm_and_flat" => surge_ac::StartupPolicy::ParallelWarmAndFlat,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unsupported startup_policy '{other}'; expected 'single', 'adaptive', or 'parallel_warm_and_flat'"
                )));
            }
        },
        q_sharing: match q_sharing {
            "capability" => surge_ac::QSharingMode::Capability,
            "mbase" => surge_ac::QSharingMode::Mbase,
            "equal" => surge_ac::QSharingMode::Equal,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unsupported q_sharing '{other}'; expected 'capability', 'mbase', or 'equal'"
                )));
            }
        },
        warm_start: warm_start.map(|ws| surge_ac::WarmStart::from_solution(&ws.inner)),
        line_search,
        detect_islands,
        dc_line_model: match dc_line_model {
            "fixed_schedule" => surge_ac::DcLineModel::FixedSchedule,
            "sequential_ac_dc" => surge_ac::DcLineModel::SequentialAcDc,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unsupported dc_line_model '{other}'; expected 'fixed_schedule' or 'sequential_ac_dc'"
                )));
            }
        },
        record_convergence_history,
        vm_min,
        vm_max,
        angle_reference: parse_angle_reference(angle_reference)?,
        ..Default::default()
    };

    // For zero-impedance merging, we solve on the contracted network and expand.
    if merge_zero_impedance {
        // OLTC / PAR / shunt controls reference branch/bus indices in the ORIGINAL network.
        // After zero-impedance merging the branch array changes (zero-Z branches removed,
        // terminals remapped), so those indices are no longer valid.  Clear them and warn.
        let mut zi_opts = opts.clone();
        if !zi_opts.oltc_controls.is_empty()
            || !zi_opts.par_controls.is_empty()
            || !zi_opts.switched_shunts.is_empty()
        {
            let warnings = py.import("warnings").map_err(to_pyerr)?;
            warnings
                .call_method1(
                    "warn",
                    (
                        "merge_zero_impedance=True: OLTC, PAR, and switched-shunt discrete \
                         controls are disabled for this solve because branch/bus indices \
                         become invalid after bus merging.  Use merge_zero_impedance=False \
                         if you need these controls.",
                        py.get_type::<pyo3::exceptions::PyUserWarning>(),
                        2i32,
                    ),
                )
                .map_err(to_pyerr)?;
            zi_opts.oltc_controls.clear();
            zi_opts.oltc_enabled = false;
            zi_opts.par_controls.clear();
            zi_opts.par_enabled = false;
            zi_opts.switched_shunts.clear();
            zi_opts.shunt_enabled = false;
        }

        let net = Arc::clone(&network.inner);
        let result = py.detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let merged = surge_ac::merge_zero_impedance(&net, 1e-6);
                let sol = surge_ac::solve_ac_pf_kernel(&merged.network, &zi_opts)?;
                Ok::<_, surge_ac::AcPfError>(surge_ac::expand_pf_solution(&sol, &merged, &net))
            }))
            .map_err(|panic| {
                surge_ac::AcPfError::InvalidNetwork(format!(
                    "internal panic: {}",
                    extract_panic_msg(panic)
                ))
            })
            .and_then(|r| r)
        });
        let orig_net = Arc::clone(&network.inner);
        return match result {
            Ok(mut inner) => {
                // expand_pf_solution leaves bus_numbers empty; fill from the original network.
                inner.bus_numbers = orig_net.buses.iter().map(|b| b.number).collect();
                Ok(AcPfResult {
                    inner,
                    net: Some(orig_net),
                })
            }
            Err(surge_ac::AcPfError::NotConverged {
                iterations: iters,
                max_mismatch: mismatch,
                worst_bus,
                partial_vm,
                partial_va,
            }) => {
                let inner = build_non_converged_solution(
                    &orig_net, iters, mismatch, worst_bus, partial_vm, partial_va,
                );
                Ok(AcPfResult {
                    inner,
                    net: Some(orig_net),
                })
            }
            Err(e) => Err(SurgeError::new_err(e.to_string())),
        };
    }

    let net = Arc::clone(&network.inner);

    // Release GIL for the Rust solve; catch panics so they become Python exceptions.
    let result: Result<surge_solution::PfSolution, surge_ac::AcPfError> = py.detach(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            surge_ac::solve_ac_pf_kernel(&net, &opts)
        }))
        .map_err(|panic| {
            surge_ac::AcPfError::InvalidNetwork(format!(
                "internal panic: {}",
                extract_panic_msg(panic)
            ))
        })
        .and_then(|r| r)
    });

    match result {
        Ok(inner) => Ok(AcPfResult {
            inner,
            net: Some(net),
        }),
        Err(surge_ac::AcPfError::NotConverged {
            iterations: iters,
            max_mismatch: mismatch,
            worst_bus,
            partial_vm,
            partial_va,
        }) => {
            let inner = build_non_converged_solution(
                &net, iters, mismatch, worst_bus, partial_vm, partial_va,
            );
            Ok(AcPfResult {
                inner,
                net: Some(net),
            })
        }
        Err(e) => Err(SurgeError::new_err(e.to_string())),
    }
}

/// Solve HVDC power flow using the canonical `surge_hvdc` interface.
/// Build a `PfSolution` from a non-converged NR result, using partial
/// voltage iterates when available and falling back to the network's
/// initial voltages otherwise.
fn build_non_converged_solution(
    net: &surge_network::Network,
    iterations: u32,
    max_mismatch: f64,
    worst_bus: Option<u32>,
    partial_vm: Option<Vec<f64>>,
    partial_va: Option<Vec<f64>>,
) -> surge_solution::PfSolution {
    let n = net.n_buses();
    let vm =
        partial_vm.unwrap_or_else(|| net.buses.iter().map(|b| b.voltage_magnitude_pu).collect());
    let va = partial_va.unwrap_or_else(|| net.buses.iter().map(|b| b.voltage_angle_rad).collect());
    let (branch_pf, branch_pt, branch_qf, branch_qt) =
        surge_solution::compute_branch_power_flows(net, &vm, &va, net.base_mva);
    surge_solution::PfSolution {
        pf_model: surge_solution::PfModel::Ac,
        status: surge_solution::SolveStatus::MaxIterations,
        iterations,
        max_mismatch,
        solve_time_secs: 0.0,
        voltage_magnitude_pu: vm,
        voltage_angle_rad: va,
        active_power_injection_pu: vec![0.0; n],
        reactive_power_injection_pu: vec![0.0; n],
        branch_p_from_mw: branch_pf,
        branch_p_to_mw: branch_pt,
        branch_q_from_mvar: branch_qf,
        branch_q_to_mvar: branch_qt,
        bus_numbers: net.buses.iter().map(|b| b.number).collect(),
        island_ids: Vec::new(),
        q_limited_buses: Vec::new(),
        n_q_limit_switches: 0,
        gen_slack_contribution_mw: Vec::new(),
        convergence_history: Vec::new(),
        worst_mismatch_bus: worst_bus,
        area_interchange: None,
    }
}

///
/// This is the Python entrypoint for point-to-point and explicit DC-network
/// HVDC solves. Method selection defaults to ``"auto"`` and follows the same
/// routing rules as the Rust API.
#[pyfunction]
#[pyo3(signature = (
    network,
    method = "auto",
    tol = 1e-6,
    max_iter = 50,
    ac_tol = 1e-8,
    max_ac_iter = 100,
    dc_tol = 1e-8,
    max_dc_iter = 50,
    flat_start = true,
    coupling_sensitivities = true,
    coordinated_droop = true,
))]
pub fn solve_hvdc(
    py: Python<'_>,
    network: &Network,
    method: &str,
    tol: f64,
    max_iter: u32,
    ac_tol: f64,
    max_ac_iter: u32,
    dc_tol: f64,
    max_dc_iter: u32,
    flat_start: bool,
    coupling_sensitivities: bool,
    coordinated_droop: bool,
) -> PyResult<HvdcSolution> {
    network.validate()?;
    let method = match method {
        "auto" => surge_hvdc::HvdcMethod::Auto,
        "sequential" => surge_hvdc::HvdcMethod::Sequential,
        "block_coupled" => surge_hvdc::HvdcMethod::BlockCoupled,
        "hybrid" => surge_hvdc::HvdcMethod::Hybrid,
        other => {
            return Err(PyValueError::new_err(format!(
                "unsupported HVDC method '{other}'; expected one of: auto, sequential, block_coupled, hybrid"
            )));
        }
    };
    let net = Arc::clone(&network.inner);
    let opts = surge_hvdc::HvdcOptions {
        method,
        tol,
        max_iter,
        ac_tol,
        max_ac_iter,
        dc_tol,
        max_dc_iter,
        flat_start,
        coupling_sensitivities,
        coordinated_droop,
    };
    let inner = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_hvdc::solve_hvdc(&net, &opts).map_err(|e| e.to_string())
            }))
            .map_err(|e| format!("solve_hvdc failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;
    Ok(HvdcSolution::from_inner(inner))
}

// ---------------------------------------------------------------------------
// Loss sensitivity factors
// ---------------------------------------------------------------------------

use numpy::{IntoPyArray, PyArray1};
use pyo3::types::PyDict;

use crate::utils::dict_to_dataframe_with_index;

#[pyclass(name = "_LsfResult")]
pub struct LsfResult {
    bus_numbers_vec: Vec<u32>,
    lsf_vec: Vec<f64>,
    base_losses: f64,
}

#[pymethods]
impl LsfResult {
    #[getter]
    fn bus_numbers(&self) -> Vec<u32> {
        self.bus_numbers_vec.clone()
    }

    #[getter]
    fn lsf<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        self.lsf_vec.clone().into_pyarray(py)
    }

    #[getter]
    fn base_losses_mw(&self) -> f64 {
        self.base_losses
    }

    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("bus_id", self.bus_numbers_vec.clone())?;
        dict.set_item("lsf", self.lsf_vec.clone())?;
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    fn __repr__(&self) -> String {
        format!(
            "LsfResult(buses={}, base_losses={:.2} MW)",
            self.bus_numbers_vec.len(),
            self.base_losses
        )
    }
}

/// Internal package helper for loss sensitivity factors.
///
/// Computes AC marginal loss factors analytically via one Jacobian build and
/// one J^T solve — approximately the cost of a single Newton-Raphson iteration.
/// An optional ``solution`` (AcPfResult) may be provided to skip the base AC
/// power flow solve; otherwise one is run internally.
#[pyfunction(name = "_losses_compute_factors")]
#[pyo3(signature = (network, solution = None))]
pub fn compute_loss_factors<'py>(
    py: Python<'py>,
    network: &Network,
    solution: Option<&AcPfResult>,
) -> PyResult<LsfResult> {
    use surge_ac::AcPfOptions;
    use surge_ac::solve_ac_pf_kernel;
    use surge_network::network::BusType;

    let net = Arc::clone(&network.inner);
    let base_mva = net.base_mva;

    // Obtain an AC operating point: use the provided solution or solve one.
    let (va, vm, base_losses) = if let Some(sol) = solution {
        let total = sol.inner.active_power_injection_pu.iter().sum::<f64>() * base_mva;
        (
            sol.inner.voltage_angle_rad.clone(),
            sol.inner.voltage_magnitude_pu.clone(),
            total,
        )
    } else {
        let base_sol = py
            .detach(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    solve_ac_pf_kernel(&net, &AcPfOptions::default()).map_err(|e| e.to_string())
                }))
                .map_err(|e| format!("compute_loss_factors failed: {}", extract_panic_msg(e)))
                .and_then(|r| r)
            })
            .map_err(to_pyerr)?;
        let total = base_sol.active_power_injection_pu.iter().sum::<f64>() * base_mva;
        (
            base_sol.voltage_angle_rad,
            base_sol.voltage_magnitude_pu,
            total,
        )
    };

    // Find the slack bus index.
    let slack_idx = net
        .buses
        .iter()
        .position(|b| b.bus_type == BusType::Slack)
        .ok_or_else(|| PyValueError::new_err("network has no slack bus"))?;

    // Analytical MLF via one J^T solve (surge-opf).
    let lsf = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_opf::compute_ac_marginal_loss_factors(&net, &va, &vm, slack_idx)
            }))
            .map_err(|e| format!("compute_loss_factors failed: {}", extract_panic_msg(e)))
            .and_then(|r| r)
        })
        .map_err(to_pyerr)?;

    Ok(LsfResult {
        bus_numbers_vec: net.buses.iter().map(|bus| bus.number).collect(),
        lsf_vec: lsf,
        base_losses,
    })
}
