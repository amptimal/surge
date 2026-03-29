// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Core contingency analysis engine — public API entry points.

pub(crate) mod islands;
pub(crate) mod parallel;
pub(crate) mod power_flow;
pub(crate) mod solvers;

use std::collections::HashMap;
use std::time::Instant;

use crate::scrd::{ScrdOptions, ScrdSensitivityModel, ScrdViolation, solve_scrd};
use surge_ac::AcPfOptions;
use surge_ac::matrix::ybus::YBus;
use surge_network::Network;
use surge_network::network::discrete_control::{OltcControl, ParControl};
use surge_network::network::{Contingency, TplCategory};
use tracing::{info, warn};

use self::parallel::solve_contingencies_parallel;
use self::power_flow::{network_has_hvdc_assets, solve_network_pf_with_fallback};
use crate::ranking::contingency_severity_score;
use crate::screening::{screen_and_solve_with_fdpf, screen_with_lodf};
use crate::types::{
    AnalysisSummary, ContingencyAnalysis, ContingencyError, ContingencyOptions, ContingencyStatus,
    ScreeningMode, Violation, VsmCategory,
};
use crate::violations::{detect_violations, violation_key, violation_severity};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Analyze full N-1 branch contingency behavior.
///
/// Generates one contingency per in-service branch, optionally screens with
/// LODF, then solves remaining contingencies in parallel with rayon.
pub fn analyze_n1_branch(
    network: &Network,
    options: &ContingencyOptions,
) -> Result<ContingencyAnalysis, ContingencyError> {
    let mut contingencies = surge_network::network::generate_n1_branch_contingencies(network);
    if options.include_breaker_contingencies
        && let Some(sm) = network.topology.as_ref()
    {
        contingencies.extend(surge_network::network::generate_breaker_contingencies(sm));
    }
    analyze_contingencies(network, &contingencies, options)
}

/// Generate one N-1 contingency per in-service branch.
///
/// Delegates to [`surge_network::network::generate_n1_branch_contingencies`].
pub fn generate_n1_branch_contingencies(network: &Network) -> Vec<Contingency> {
    surge_network::network::generate_n1_branch_contingencies(network)
}

/// Generate one N-1 contingency per in-service generator (CTG-01).
///
/// Returns one [`Contingency`] per generator with `in_service == true`.
/// Each contingency has `branch_indices: []` and `generator_indices: [gen_idx]`.
pub fn generate_n1_generator_contingencies(network: &Network) -> Vec<Contingency> {
    network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, g)| Contingency {
            id: format!("gen_{i}"),
            label: format!("Generator outage bus {} gen {}", g.bus, i),
            generator_indices: vec![i],
            tpl_category: TplCategory::P3GeneratorTrip,
            ..Default::default()
        })
        .collect()
}

/// Run N-1 generator contingency analysis using the fast injection-vector path (CTG-01).
///
/// Generator outages do **not** change the Y-bus admittance matrix — only the
/// real and reactive power injection at the affected bus changes (Pg = Qg = 0),
/// and the bus may transition from PV to PQ if no other in-service generator
/// remains connected.  This avoids the full network clone + Y-bus recomputation
/// used by the legacy path, giving a significant speedup for large studies.
///
/// Generates one contingency per in-service generator and evaluates each with
/// the fast path. The result structure matches [`analyze_n1_branch`].
pub fn analyze_n1_generator(
    network: &Network,
    options: &ContingencyOptions,
) -> Result<ContingencyAnalysis, ContingencyError> {
    let contingencies = generate_n1_generator_contingencies(network);
    analyze_contingencies(network, &contingencies, options)
}

/// Generate all C(n,2) simultaneous double branch outage contingencies (N-2).
///
/// For a network with `n` in-service branches, this produces n*(n-1)/2 contingencies,
/// each tripping two branches simultaneously. Use [`analyze_n2_branch`] for the
/// full analysis pipeline, or call this function directly to inspect or filter
/// the contingency list before passing it to [`analyze_contingencies`].
pub fn generate_n2_branch_contingencies(network: &Network) -> Vec<Contingency> {
    let in_service: Vec<(usize, &surge_network::network::Branch)> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service)
        .collect();

    let n = in_service.len();
    let mut contingencies = Vec::with_capacity(n * (n.saturating_sub(1)) / 2);

    for i in 0..n {
        for j in (i + 1)..n {
            let (idx_i, br_i) = in_service[i];
            let (idx_j, br_j) = in_service[j];
            contingencies.push(Contingency {
                id: format!("n2_branch_{idx_i}_{idx_j}"),
                label: format!(
                    "N-2: Line {}->{}(ckt {}) & Line {}->{}(ckt {})",
                    br_i.from_bus,
                    br_i.to_bus,
                    br_i.circuit,
                    br_j.from_bus,
                    br_j.to_bus,
                    br_j.circuit
                ),
                branch_indices: vec![idx_i, idx_j],
                ..Default::default()
            });
        }
    }

    contingencies
}

/// Analyze full N-2 simultaneous double branch contingency behavior.
///
/// Generates all C(n,2) branch pairs from the set of in-service branches, then
/// evaluates each pair using AC Newton-Raphson (with optional LODF/FDPF screening).
///
/// **Scale note**: The number of contingencies grows as O(n²).  For a 100-branch
/// network this is ~4,950 contingencies; for 500 branches ~124,750.  Use LODF
/// screening ([`ScreeningMode::Lodf`]) to reduce the AC solve count on large
/// networks, or set [`ContingencyOptions::top_k`] to limit returned results.
pub fn analyze_n2_branch(
    network: &Network,
    options: &ContingencyOptions,
) -> Result<ContingencyAnalysis, ContingencyError> {
    let contingencies = generate_n2_branch_contingencies(network);
    let n_branches = network.branches.iter().filter(|b| b.in_service).count();
    info!(
        "N-2 branch analysis: {} in-service branches → {} contingency pairs",
        n_branches,
        contingencies.len()
    );
    analyze_contingencies(network, &contingencies, options)
}

// ---------------------------------------------------------------------------
// Branch delta computation (shared by parallel solver and screening)
// ---------------------------------------------------------------------------

/// Pre-compute Y-bus branch removal deltas for each contingency.
///
/// Returns `Some(deltas)` for pure branch contingencies (where `deltas` is a vec
/// of per-branch removal delta arrays). Returns `None` for contingencies that
/// involve generators, modifications, switches, or HVDC — these require the
/// full-clone solver path.
///
/// An all-OOS-branch contingency (where no branch produces a delta) also returns
/// `None` since the delta path has nothing to apply.
#[allow(clippy::type_complexity)]
pub(crate) fn compute_branch_deltas<'a>(
    contingencies: impl Iterator<Item = &'a Contingency>,
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    corr_map: &HashMap<
        u32,
        &surge_network::network::impedance_correction::ImpedanceCorrectionTable,
    >,
) -> Vec<Option<Vec<[(usize, usize, f64, f64); 4]>>> {
    if network_has_hvdc_assets(network) {
        return contingencies.map(|_| None).collect();
    }

    contingencies
        .map(|ctg| {
            if !ctg.generator_indices.is_empty()
                || !ctg.modifications.is_empty()
                || !ctg.switch_ids.is_empty()
                || !ctg.hvdc_converter_indices.is_empty()
                || !ctg.hvdc_cable_indices.is_empty()
            {
                return None;
            }
            let deltas: Vec<_> = ctg
                .branch_indices
                .iter()
                .filter_map(|&br_idx| {
                    network
                        .branches
                        .get(br_idx)
                        .filter(|b| b.in_service)
                        .map(|br| YBus::branch_removal_delta(br, bus_map, corr_map))
                })
                .collect();
            if deltas.is_empty() {
                None
            } else {
                Some(deltas)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Discrete control population
// ---------------------------------------------------------------------------

/// Populate `acpf_opts` discrete-control vecs from the network's spec fields.
///
/// Converts `network.controls.oltc_specs` → [`OltcControl`], `network.controls.par_specs` →
/// [`ParControl`], and copies `network.controls.switched_shunts` into the NR options.
/// Existing entries in the NR options are preserved; duplicates (same branch)
/// are skipped.
pub(crate) fn populate_discrete_controls(acpf_opts: &mut AcPfOptions, network: &Network) {
    let bus_map = network.bus_index_map();

    // --- OLTC: OltcSpec (external bus numbers) → OltcControl (0-based indices) ---
    for spec in &network.controls.oltc_specs {
        let branch_index = network.branches.iter().position(|b| {
            b.from_bus == spec.from_bus && b.to_bus == spec.to_bus && b.circuit == spec.circuit
        });
        let Some(branch_index) = branch_index else {
            continue;
        };
        if acpf_opts
            .oltc_controls
            .iter()
            .any(|c| c.branch_index == branch_index)
        {
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
        acpf_opts.oltc_controls.push(OltcControl {
            branch_index,
            bus_regulated,
            v_target: spec.v_target,
            v_band: spec.v_band,
            tap_min: spec.tap_min,
            tap_max: spec.tap_max,
            tap_step: spec.tap_step,
        });
    }

    // --- PAR: ParSpec (external bus numbers) → ParControl (0-based indices) ---
    for spec in &network.controls.par_specs {
        let branch_index = network.branches.iter().position(|b| {
            b.from_bus == spec.from_bus && b.to_bus == spec.to_bus && b.circuit == spec.circuit
        });
        let Some(branch_index) = branch_index else {
            continue;
        };
        if acpf_opts
            .par_controls
            .iter()
            .any(|c| c.branch_index == branch_index)
        {
            continue;
        }
        let monitored_branch_index = if spec.monitored_from_bus == 0 {
            branch_index
        } else if spec.monitored_to_bus == 0 {
            network
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
                .unwrap_or(branch_index)
        } else {
            let found = network.branches.iter().position(|b| {
                b.from_bus == spec.monitored_from_bus
                    && b.to_bus == spec.monitored_to_bus
                    && b.circuit == spec.monitored_circuit
            });
            let Some(idx) = found else { continue };
            idx
        };
        acpf_opts.par_controls.push(ParControl {
            branch_index,
            monitored_branch_index,
            p_target_mw: spec.p_target_mw,
            p_band_mw: spec.p_band_mw,
            angle_min_deg: spec.angle_min_deg,
            angle_max_deg: spec.angle_max_deg,
            ang_step_deg: spec.ang_step_deg,
        });
    }

    // --- Switched shunts: copy from network if NR options vec is empty ---
    if acpf_opts.switched_shunts.is_empty() && !network.controls.switched_shunts.is_empty() {
        acpf_opts.switched_shunts = network.controls.switched_shunts.clone();
    }
}

// ---------------------------------------------------------------------------
// Main engine: analyze_contingencies
// ---------------------------------------------------------------------------

/// Run contingency analysis for a custom list of contingencies.
pub fn analyze_contingencies(
    network: &Network,
    contingencies: &[Contingency],
    options: &ContingencyOptions,
) -> Result<ContingencyAnalysis, ContingencyError> {
    // Expand FACTS devices so every contingency solve sees the correct network:
    // - TCSC: modified branch reactance reflected in Y-bus and DC screening flows
    // - SVC/STATCOM: PV generators providing reactive support in post-contingency NR
    // Branch indices in Contingency::branch_indices remain valid — expansion only
    // modifies reactance in-place (TCSC) or appends generators (SVC).
    let mut network = surge_ac::expand_facts(network).into_owned();

    network = prepare_runtime_topology(network)?;
    network.canonicalize_runtime_identities();

    network.validate().map_err(|e| {
        ContingencyError::BaseCaseFailed(format!("Network validation failed: {}", e))
    })?;

    let network = &network;

    // When discrete_controls is enabled, populate NR options with controls
    // derived from the network's OLTC/PAR specs and switched shunts.
    let owned_options;
    let options: &ContingencyOptions = if options.discrete_controls {
        let mut o = options.clone();
        populate_discrete_controls(&mut o.acpf_options, network);
        owned_options = o;
        &owned_options
    } else {
        options
    };

    let wall_start = Instant::now();
    let has_hvdc = network_has_hvdc_assets(network);

    // P1-031: Filter out contingencies that reference only out-of-service
    // branches.  Tripping an already-OOS branch is a no-op that wastes
    // compute.  A contingency is kept if it trips at least one in-service
    // branch OR at least one generator.  Pure branch contingencies where
    // every referenced branch is OOS are dropped.
    let original_count = contingencies.len();
    let contingencies: Vec<Contingency> = contingencies
        .iter()
        .filter(|ctg| {
            if !ctg.generator_indices.is_empty() {
                return true;
            }
            if !ctg.switch_ids.is_empty() {
                return true;
            }
            if !ctg.modifications.is_empty() {
                return true;
            }
            if !ctg.hvdc_converter_indices.is_empty() || !ctg.hvdc_cable_indices.is_empty() {
                return true;
            }
            ctg.branch_indices
                .iter()
                .any(|&br_idx| network.branches.get(br_idx).is_some_and(|br| br.in_service))
        })
        .cloned()
        .collect();
    let oos_skipped = original_count - contingencies.len();
    if oos_skipped > 0 {
        info!(
            "P1-031: Skipped {} contingencies referencing only out-of-service branches",
            oos_skipped
        );
    }

    // Solve base case with fallback chain:
    //   1. NR with user-specified options (case data voltages or flat start)
    //   2. If that fails and not already flat-start, try flat start
    // Base case convergence problems happen in production (stressed systems,
    // bad data, unusual dispatch) — we must handle them robustly.
    let base_case = solve_network_pf_with_fallback(network, &options.acpf_options)
        .map_err(ContingencyError::BaseCaseFailed)?;

    // Compute base-case violations so we can suppress them from contingency results.
    // Operators need to see only NEW violations introduced by each contingency,
    // not pre-existing base-case violations that would appear in every single result.
    let base_case_violations = detect_violations(network, &base_case, options);

    if !base_case_violations.is_empty() {
        info!(
            "Base case has {} pre-existing violations — suppressing from contingency results",
            base_case_violations.len()
        );
    }

    // Screen contingencies and solve
    let screening = if has_hvdc {
        if !matches!(options.screening, ScreeningMode::Off) {
            info!(
                "HVDC network detected: bypassing AC-only screening and solving all contingencies exactly"
            );
        }
        ScreeningMode::Off
    } else {
        options.screening
    };

    let (mut results, screened_out) = match screening {
        ScreeningMode::Off => {
            let all: Vec<&Contingency> = contingencies.iter().collect();
            info!(
                "Contingency analysis: {} total, 0 screened out, {} to AC solve",
                contingencies.len(),
                contingencies.len()
            );
            (
                solve_contingencies_parallel(
                    network,
                    &all,
                    options,
                    &base_case.voltage_magnitude_pu,
                    &base_case.voltage_angle_rad,
                ),
                0,
            )
        }
        ScreeningMode::Lodf => {
            let (critical_indices, screened_out) =
                screen_with_lodf(network, &contingencies, options, &base_case)?;
            let ac_count = critical_indices.len();
            info!(
                "Contingency analysis: {} total, {} screened out (LODF), {} to AC solve",
                contingencies.len(),
                screened_out,
                ac_count
            );
            let critical: Vec<&Contingency> = critical_indices
                .iter()
                .map(|&i| &contingencies[i])
                .collect();
            (
                solve_contingencies_parallel(
                    network,
                    &critical,
                    options,
                    &base_case.voltage_magnitude_pu,
                    &base_case.voltage_angle_rad,
                ),
                screened_out,
            )
        }
        ScreeningMode::Fdpf => {
            let results = screen_and_solve_with_fdpf(network, &contingencies, options, &base_case)?;
            let screened_out = results
                .iter()
                .filter(|result| {
                    result.status == ContingencyStatus::Approximate && !result.fdpf_fallback
                })
                .count();
            (results, screened_out)
        }
    };

    // Filter out base-case violations from every contingency result so that only
    // violations that are *new* relative to the base case are reported.
    suppress_base_case_violations(&mut results, &base_case_violations);

    // CTG-02: Corrective redispatch (SCRD) for contingencies with thermal violations.
    if options.corrective_dispatch {
        run_corrective_dispatch(network, &mut results, options);
    }

    let converged = results.iter().filter(|r| r.converged).count();
    let with_violations = results.iter().filter(|r| !r.violations.is_empty()).count();
    let wall_time = wall_start.elapsed().as_secs_f64();
    let ac_solved = results
        .iter()
        .filter(|result| result.status != ContingencyStatus::Approximate)
        .count();
    let approximate_returned = results
        .iter()
        .filter(|result| result.status == ContingencyStatus::Approximate)
        .count();

    let n_voltage_critical = results
        .iter()
        .filter(|r| {
            r.voltage_stress.as_ref().is_some_and(|vs| {
                matches!(
                    vs.category,
                    Some(VsmCategory::Critical | VsmCategory::Unstable)
                )
            })
        })
        .count();
    // CTG-10: Top-K ranking — sort by worst severity then truncate.
    //
    // Severity metric (descending, so highest-severity contingency sorts first):
    //   - NonConvergent violations score f64::INFINITY.
    //   - ThermalOverload: max loading_pct across all thermal violations.
    //   - VoltageLow / VoltageHigh: max |vm - nominal| expressed as a percentage.
    //   - No violations: 0.0.
    if let Some(k) = options.top_k {
        results.sort_by(|a, b| {
            let score_a = contingency_severity_score(a);
            let score_b = contingency_severity_score(b);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(k);

        info!("Top-K={k}: retained {} worst contingencies", results.len());
    }

    info!(
        "Contingency analysis complete: {:.3}s, {} converged, {} with violations",
        wall_time, converged, with_violations
    );

    Ok(ContingencyAnalysis {
        base_case,
        summary: AnalysisSummary {
            total_contingencies: contingencies.len(),
            screened_out,
            ac_solved,
            approximate_returned,
            converged,
            with_violations,
            solve_time_secs: wall_time,
            n_voltage_critical,
        },
        results,
    })
}

fn suppress_base_case_violations(
    results: &mut [crate::types::ContingencyResult],
    base_case_violations: &[Violation],
) {
    if base_case_violations.is_empty() {
        return;
    }

    let mut base_severity_by_key: HashMap<String, f64> = HashMap::new();
    for violation in base_case_violations {
        let key = violation_key(violation);
        let severity = violation_severity(violation);
        base_severity_by_key
            .entry(key)
            .and_modify(|existing| {
                *existing = (*existing).max(severity);
            })
            .or_insert(severity);
    }

    for result in results {
        result.violations.retain(|violation| {
            match base_severity_by_key.get(&violation_key(violation)) {
                Some(base_severity) => violation_severity(violation) > *base_severity + 1e-9,
                None => true,
            }
        });
    }
}

/// Run corrective redispatch (SCRD) for contingencies with thermal violations.
///
/// Computes DC base flows + PTDF/LODF once (shared across all contingencies),
/// then calls solve_scrd for each result that has ThermalOverload violations.
/// Logs warnings on failure rather than propagating errors.
fn run_corrective_dispatch(
    network: &Network,
    results: &mut [crate::types::ContingencyResult],
    options: &ContingencyOptions,
) {
    let mut dc_model = match surge_dc::PreparedDcStudy::new(network) {
        Ok(model) => model,
        Err(e) => {
            warn!("Corrective dispatch skipped: DC preparation failed: {e}");
            return;
        }
    };
    let all_branches: Vec<usize> = (0..network.n_branches()).collect();
    let bus_map = network.bus_index_map();
    let mut ptdf_bus_indices: Vec<usize> = network
        .generators
        .iter()
        .filter(|generator| generator.in_service)
        .filter_map(|generator| bus_map.get(&generator.bus).copied())
        .collect();
    ptdf_bus_indices.sort_unstable();
    ptdf_bus_indices.dedup();
    let dc_workflow = match dc_model.run_analysis(
        &surge_dc::DcAnalysisRequest::with_monitored_branches(&all_branches)
            .with_ptdf_buses(&ptdf_bus_indices),
    ) {
        Ok(result) => result,
        Err(e) => {
            warn!("Corrective dispatch skipped: DC sensitivity workflow failed: {e}");
            return;
        }
    };
    let base_flows_mw: Vec<f64> = dc_workflow
        .power_flow
        .branch_p_flow
        .iter()
        .map(|&f| f * network.base_mva)
        .collect();
    let ptdf = dc_workflow.ptdf;
    let mut lodf_columns = dc_model.lodf_columns();

    for result in results {
        let ctg_branch = if result.branch_indices.len() == 1 {
            Some(result.branch_indices[0])
        } else {
            None
        };

        let thermal_viols: Vec<ScrdViolation> = result
            .violations
            .iter()
            .filter_map(|v| {
                if let Violation::ThermalOverload {
                    branch_idx,
                    flow_mw,
                    limit_mva,
                    ..
                } = v
                {
                    Some(ScrdViolation {
                        branch_index: *branch_idx,
                        contingency_branch: ctg_branch,
                        flow_mw: *flow_mw,
                        rating_mw: *limit_mva,
                    })
                } else {
                    None
                }
            })
            .collect();

        if thermal_viols.is_empty() {
            continue;
        }

        let mut lodf_pairs: HashMap<(usize, usize), f64> = HashMap::new();
        if let Some(k) = ctg_branch {
            let mut monitored_branches: Vec<usize> =
                thermal_viols.iter().map(|v| v.branch_index).collect();
            monitored_branches.sort_unstable();
            monitored_branches.dedup();

            match lodf_columns.compute_column(&monitored_branches, k) {
                Ok(column) => {
                    for (position, &monitored_branch) in monitored_branches.iter().enumerate() {
                        lodf_pairs.insert((monitored_branch, k), column[position]);
                    }
                }
                Err(e) => {
                    info!(
                        "SCRD for contingency {}: LODF subset solve failed: {}",
                        result.id, e
                    );
                    continue;
                }
            }
        }

        let scrd_opts = ScrdOptions {
            violations: thermal_viols,
            penalty_config: options.penalty_config.clone(),
            ..ScrdOptions::default()
        };
        match solve_scrd(
            network,
            &base_flows_mw,
            ScrdSensitivityModel {
                ptdf_rows: &ptdf,
                lodf_pairs: &lodf_pairs,
            },
            &scrd_opts,
        ) {
            Ok(sol) => {
                result.corrective_dispatch = Some(sol);
                if result.tpl_category == TplCategory::P1SingleElement {
                    result.tpl_category = TplCategory::P2SingleWithRAS;
                }
            }
            Err(e) => {
                info!("SCRD for contingency {}: {:?}", result.id, e);
            }
        }
    }
}

fn prepare_runtime_topology(mut network: Network) -> Result<Network, ContingencyError> {
    let topology_state = network.topology.as_ref().map(|topology| topology.status());
    match topology_state {
        Some(surge_network::network::TopologyMappingState::Missing) => {
            let projection = surge_topology::project_node_breaker_topology(
                network
                    .topology
                    .as_ref()
                    .expect("topology state checked above"),
            )
            .map_err(|error| {
                ContingencyError::BaseCaseFailed(format!("Topology projection failed: {error}"))
            })?;
            if let Some(topology) = network.topology.as_mut() {
                topology.install_mapping(projection.mapping);
            }
            Ok(network)
        }
        Some(surge_network::network::TopologyMappingState::Stale) => {
            surge_topology::rebuild_topology(&network).map_err(|error| {
                ContingencyError::BaseCaseFailed(format!("Topology rebuild failed: {error}"))
            })
        }
        _ => Ok(network),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppress_base_case_violations_keeps_worse_thermal_overloads() {
        let base_case_violations = vec![Violation::ThermalOverload {
            branch_idx: 7,
            from_bus: 1,
            to_bus: 2,
            loading_pct: 80.0,
            flow_mw: 80.0,
            flow_mva: 80.0,
            limit_mva: 100.0,
        }];
        let mut results = vec![crate::types::ContingencyResult {
            id: "ctg".into(),
            violations: vec![
                Violation::ThermalOverload {
                    branch_idx: 7,
                    from_bus: 1,
                    to_bus: 2,
                    loading_pct: 80.0,
                    flow_mw: 80.0,
                    flow_mva: 80.0,
                    limit_mva: 100.0,
                },
                Violation::ThermalOverload {
                    branch_idx: 7,
                    from_bus: 1,
                    to_bus: 2,
                    loading_pct: 92.5,
                    flow_mw: 92.5,
                    flow_mva: 92.5,
                    limit_mva: 100.0,
                },
            ],
            ..Default::default()
        }];

        suppress_base_case_violations(&mut results, &base_case_violations);

        assert_eq!(results[0].violations.len(), 1);
        match &results[0].violations[0] {
            Violation::ThermalOverload { loading_pct, .. } => {
                assert!((*loading_pct - 92.5).abs() < 1e-9);
            }
            other => panic!("unexpected violation retained after suppression: {other:?}"),
        }
    }
}
