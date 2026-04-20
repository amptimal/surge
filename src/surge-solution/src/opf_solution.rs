// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Optimal Power Flow solution types.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use surge_network::market::VirtualBidResult;

use crate::{
    AuditableSolution, ObjectiveLedgerMismatch, ObjectiveLedgerScopeKind, ObjectiveTerm, ParResult,
    PfSolution, SolutionAuditReport,
};

fn serialize_branch_loading_pct<S>(values: &[f64], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::SerializeSeq;

    let mut seq = serializer.serialize_seq(Some(values.len()))?;
    for value in values {
        if value.is_finite() {
            seq.serialize_element(value)?;
        } else {
            seq.serialize_element(&Option::<f64>::None)?;
        }
    }
    seq.end()
}

fn deserialize_branch_loading_pct<'de, D>(deserializer: D) -> Result<Vec<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<Option<f64>>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .map(|value| value.unwrap_or(f64::NAN))
        .collect())
}

/// OPF formulation type — identifies which solver produced this solution.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpfType {
    /// DC-OPF via sparse B-θ formulation (lossless, linear).
    #[default]
    DcOpf,
    /// AC-OPF via Ipopt NLP (nonlinear, exact losses).
    AcOpf,
    /// DC Security-Constrained OPF (preventive or corrective N-1).
    DcScopf,
    /// AC Security-Constrained OPF (Benders N-1, NLP master).
    AcScopf,
    /// DC-OPF with embedded HVDC links.
    HvdcOpf,
}

// ---------------------------------------------------------------------------
// Sub-structs
// ---------------------------------------------------------------------------

/// Generator dispatch results and identity mapping.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfGeneratorResults {
    /// Optimal real power dispatch per generator (MW).
    ///
    /// Indexed by **in-service** generators in `network.generators` order.
    pub gen_p_mw: Vec<f64>,
    /// Optimal reactive power dispatch per generator (MVAr).
    ///
    /// Same ordering as `gen_p_mw`. Empty for DC-OPF; populated for AC-OPF.
    pub gen_q_mvar: Vec<f64>,
    /// External bus number for each entry in `gen_p_mw` / `gen_q_mvar`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_bus_numbers: Vec<u32>,
    /// Canonical generator identifier for each entry in `gen_p_mw` / `gen_q_mvar`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_ids: Vec<String>,
    /// Machine identifier for each entry in `gen_p_mw` / `gen_q_mvar`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_machine_ids: Vec<String>,
    /// Lower active-power bound duals ($/MWh), one per in-service generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_pg_min: Vec<f64>,
    /// Upper active-power bound duals ($/MWh), one per in-service generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_pg_max: Vec<f64>,
    /// Reactive power lower-bound duals ($/MWh per pu), one per in-service generator.
    /// AC-OPF only; empty for DC.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_qg_min: Vec<f64>,
    /// Reactive power upper-bound duals ($/MWh per pu), one per in-service generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_qg_max: Vec<f64>,
}

/// LMP decomposition per bus.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfPricing {
    /// Locational marginal prices per bus ($/MWh).
    pub lmp: Vec<f64>,
    /// Energy component of LMP per bus ($/MWh).
    pub lmp_energy: Vec<f64>,
    /// Congestion component of LMP per bus ($/MWh).
    pub lmp_congestion: Vec<f64>,
    /// Loss component of LMP per bus ($/MWh). Zero for DC-OPF.
    pub lmp_loss: Vec<f64>,
    /// Reactive LMP per bus ($/MVAr-h). AC-OPF only; empty for DC.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lmp_reactive: Vec<f64>,
}

/// Branch-level results: loading, shadow prices, and constraint duals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfBranchResults {
    /// Branch loading percentage: `max(|Sf|, |St|) / rate_a * 100`.
    ///
    /// `NaN` for branches with no positive Rate A limit; serialized as `null`
    /// in JSON.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_branch_loading_pct",
        deserialize_with = "deserialize_branch_loading_pct"
    )]
    pub branch_loading_pct: Vec<f64>,
    /// Shadow prices on branch thermal flow limits ($/MWh per MW).
    pub branch_shadow_prices: Vec<f64>,
    /// Shadow prices on branch angmin constraints ($/MWh per rad).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_angmin: Vec<f64>,
    /// Shadow prices on branch angmax constraints ($/MWh per rad).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_angmax: Vec<f64>,
    /// From-side thermal overflow slack (MVA), one entry per network branch.
    ///
    /// Non-zero values mean AC-OPF was allowed to exceed the branch's apparent
    /// power rating on the from side by this amount.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thermal_limit_slack_from_mva: Vec<f64>,
    /// To-side thermal overflow slack (MVA), one entry per network branch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thermal_limit_slack_to_mva: Vec<f64>,
    /// Shadow prices for flowgate constraints ($/MWh).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flowgate_shadow_prices: Vec<f64>,
    /// Shadow prices for interface constraints ($/MWh).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_shadow_prices: Vec<f64>,
    /// Voltage magnitude lower-bound duals ($/MWh per pu), one per bus.
    /// AC-OPF only; empty for DC.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_vm_min: Vec<f64>,
    /// Voltage magnitude upper-bound duals ($/MWh per pu), one per bus.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_vm_max: Vec<f64>,
}

impl OpfBranchResults {
    /// Indices of binding branches (|branch_shadow_prices\[i\]| > 1e-6).
    pub fn binding_branch_indices(&self) -> Vec<usize> {
        self.branch_shadow_prices
            .iter()
            .enumerate()
            .filter(|(_, price)| price.abs() > 1e-6)
            .map(|(i, _)| i)
            .collect()
    }
}

/// FACTS device, storage, and transformer dispatch results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfDeviceDispatch {
    /// Switched shunt dispatch: `(bus_idx, continuous_b_pu, rounded_b_pu)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switched_shunt_dispatch: Vec<(usize, f64, f64)>,
    /// Transformer tap dispatch: `(branch_idx, continuous_tap, rounded_tap)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tap_dispatch: Vec<(usize, f64, f64)>,
    /// Phase-shifter dispatch: `(branch_idx, continuous_rad, rounded_rad)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_dispatch: Vec<(usize, f64, f64)>,
    /// SVC dispatch: `(device_index, optimal_b_svc_pu, mu_lower, mu_upper)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub svc_dispatch: Vec<(usize, f64, f64, f64)>,
    /// TCSC dispatch: `(device_index, optimal_x_comp_pu, mu_lower, mu_upper)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tcsc_dispatch: Vec<(usize, f64, f64, f64)>,
    /// Net storage dispatch (MW). Positive = discharging, negative = charging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_net_mw: Vec<f64>,
    /// Dispatchable-load real power served (MW), in active resource order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatchable_load_served_mw: Vec<f64>,
    /// Dispatchable-load reactive power served (MVAr), in active resource order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatchable_load_served_q_mvar: Vec<f64>,
    /// Cleared producer reactive up-reserve award per in-service
    /// generator (MVAr), in `gen_indices` order. AC-OPF only; empty
    /// otherwise or when the network has no reactive reserve products.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub producer_q_reserve_up_mvar: Vec<f64>,
    /// Cleared producer reactive down-reserve award per in-service
    /// generator (MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub producer_q_reserve_down_mvar: Vec<f64>,
    /// Cleared consumer reactive up-reserve award per in-service
    /// dispatchable load (MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumer_q_reserve_up_mvar: Vec<f64>,
    /// Cleared consumer reactive down-reserve award per in-service
    /// dispatchable load (MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumer_q_reserve_down_mvar: Vec<f64>,
    /// Zonal reactive up-reserve shortfall per (zone, up product) pair
    /// (MVAr). Parallel to the zonal requirement order the AC-OPF walked
    /// when building its reactive reserve plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zone_q_reserve_up_shortfall_mvar: Vec<f64>,
    /// Zonal reactive down-reserve shortfall per (zone, down product)
    /// pair (MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zone_q_reserve_down_shortfall_mvar: Vec<f64>,
    /// Point-to-point HVDC link P dispatch (MW), in the order the
    /// joint AC-DC NLP exposes them. Positive = flow from the link's
    /// from-bus (rectifier) to its to-bus (inverter). Non-empty only
    /// when at least one in-service `LccHvdcLink`/`VscHvdcLink` has a
    /// non-degenerate `[p_dc_min_mw, p_dc_max_mw]` range AND the AC
    /// OPF ran through the joint-NLP path (rather than the legacy
    /// sequential AC-DC iteration, which reports the same quantity
    /// via `AcOpfHvdcResult.hvdc_p_dc_mw`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_p2p_dispatch_mw: Vec<f64>,
    /// Whether the discrete round-and-check verification passed.
    ///
    /// `None` = continuous mode. `Some(true)` = passed. `Some(false)` = violations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discrete_feasible: Option<bool>,
    /// Human-readable descriptions of violations introduced by discrete rounding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discrete_violations: Vec<String>,
}

// ---------------------------------------------------------------------------
// OpfSolution
// ---------------------------------------------------------------------------

/// Result of an Optimal Power Flow computation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfSolution {
    /// OPF formulation type; required for correct interpretation of all other fields.
    pub opf_type: OpfType,
    /// System MVA base used to convert MW/MVAr values to per-unit.
    pub base_mva: f64,
    /// Underlying power flow solution (voltages, injections, branch flows).
    pub power_flow: PfSolution,

    // System totals
    /// Total objective cost for the solved interval (dollars).
    pub total_cost: f64,
    /// Total system load (MW).
    pub total_load_mw: f64,
    /// Total generation (MW).
    pub total_generation_mw: f64,
    /// Total system losses (MW). Zero for DC-OPF.
    pub total_losses_mw: f64,

    // Grouped results
    /// Generator dispatch and dual values.
    #[serde(flatten)]
    pub generators: OpfGeneratorResults,
    /// LMP decomposition.
    #[serde(flatten)]
    pub pricing: OpfPricing,
    /// Branch-level results and constraint duals.
    #[serde(flatten)]
    pub branches: OpfBranchResults,
    /// FACTS device, storage, and transformer dispatch.
    #[serde(flatten)]
    pub devices: OpfDeviceDispatch,

    // Supplemental results
    /// PAR implied shift angles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub par_results: Vec<ParResult>,
    /// Virtual bid clearing results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub virtual_bid_results: Vec<VirtualBidResult>,
    /// Benders cut dual values from AC-SCOPF.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benders_cut_duals: Vec<f64>,
    /// Exact objective decomposition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objective_terms: Vec<ObjectiveTerm>,
    /// Persisted exact-audit status for this solution payload.
    #[serde(default)]
    pub audit: SolutionAuditReport,

    /// Per-bus reactive-power balance slack (positive direction, MVAr).
    /// Entry `i` corresponds to bus index `i` in the network.  Non-empty
    /// only when `bus_reactive_power_balance_slack_penalty_per_mvar > 0`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_q_slack_pos_mvar: Vec<f64>,
    /// Per-bus reactive-power balance slack (negative direction, MVAr).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_q_slack_neg_mvar: Vec<f64>,
    /// Per-bus active-power balance slack (positive direction, MW).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_p_slack_pos_mw: Vec<f64>,
    /// Per-bus active-power balance slack (negative direction, MW).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_p_slack_neg_mw: Vec<f64>,
    /// Per-bus voltage-magnitude high slack (pu), one per bus.
    /// `σ_high[i] = max(0, vm[i] - vm_max[i])`. Non-empty only when
    /// `voltage_magnitude_slack_penalty_per_pu > 0`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vm_slack_high_pu: Vec<f64>,
    /// Per-bus voltage-magnitude low slack (pu), one per bus.
    /// `σ_low[i] = max(0, vm_min[i] - vm[i])`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vm_slack_low_pu: Vec<f64>,
    /// Per-branch angle-difference high slack (radians), indexed by network branch order.
    /// `sigma_high[i]` allows `Va_from - Va_to` to exceed `angmax` by this amount.
    /// Non-empty only when `angle_difference_slack_penalty_per_rad > 0`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_diff_slack_high_rad: Vec<f64>,
    /// Per-branch angle-difference low slack (radians), indexed by network branch order.
    /// `sigma_low[i]` allows `Va_from - Va_to` to go below `angmin` by this amount.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_diff_slack_low_rad: Vec<f64>,

    // Solver metadata
    /// Total solve time in seconds.
    pub solve_time_secs: f64,
    /// Number of solver iterations, when the backend exposes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u32>,
    /// Name of the LP/NLP solver (e.g. `"HiGHS"`, `"Gurobi"`, `"Ipopt"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_name: Option<String>,
    /// Version string of the solver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_version: Option<String>,
    /// Per-phase timing breakdown within the OPF solve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_opf_timings: Option<AcOpfTimings>,
    /// NLP solver stats and final-iterate diagnostics. Populated by the
    /// NLP backend (Ipopt today). Captures problem size, termination
    /// status, and residuals/barrier at the final iterate so post-mortem
    /// analysis can distinguish convergence/infeasibility/iteration-limit
    /// cases without parsing log strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nlp_trace: Option<NlpTrace>,
}

/// NLP solver stats for a single `NlpSolver::solve` call.
///
/// Populated by the backend on the returned `NlpSolution`. Carries both
/// problem structure (size, sparsity) and termination state (status code
/// and mnemonic, iteration count, final primal/dual residuals, barrier
/// parameter). Enables SCUC-equivalent debugging on the AC SCED side —
/// e.g. distinguishing a proven-infeasible solve from one that hit
/// `max_iter` with small residuals.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NlpTrace {
    /// Number of decision variables.
    pub n_vars: u32,
    /// Number of constraints (rows of `g(x)`).
    pub n_constraints: u32,
    /// Nonzeros in the constraint Jacobian sparsity pattern.
    pub jac_nnz: u32,
    /// Nonzeros in the Hessian sparsity pattern (0 when Hessian is
    /// approximated, e.g. Ipopt L-BFGS mode).
    pub hess_nnz: u32,
    /// Raw solver status code (backend-specific). For Ipopt: 0 =
    /// Solve_Succeeded, 1 = Solved_To_Acceptable_Level, 2 =
    /// Infeasible_Problem_Detected, -1 = Maximum_Iterations_Exceeded,
    /// etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<i32>,
    /// Human-readable status label (backend mnemonic), e.g.
    /// `"Solve_Succeeded"`, `"Restoration_Failed"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_label: Option<String>,
    /// Iteration count at termination.
    pub iterations: u32,
    /// Final objective value.
    pub objective: f64,
    /// Primal infeasibility at the final iterate (max constraint
    /// violation in the solver's scaled internal representation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_primal_inf: Option<f64>,
    /// Dual infeasibility at the final iterate (KKT stationarity residual).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_dual_inf: Option<f64>,
    /// Final barrier parameter `μ` (Ipopt). Large `μ` at termination
    /// typically indicates the solver stalled before reaching the
    /// interior-point endgame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_mu: Option<f64>,
    /// Whether the solver considered the problem converged.
    pub converged: bool,
}

/// Per-phase timing breakdown for a single AC-OPF solve call.
///
/// Populated inside `solve_ac_opf_with_context_once`. Each `nlp_build`
/// + `nlp_solve` pair corresponds to one NLP construction and Ipopt
///   call; the constraint-screening fallback path may produce a second
///   pair, reflected in `nlp_attempts`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AcOpfTimings {
    /// FACTS expansion, network canonicalize, validation, missing-cost
    /// check — everything before solver lookup and warm-start decisions.
    pub network_prep_secs: f64,
    /// NLP solver lookup, DC-OPF warm-start decision, constraint
    /// screening setup — between network prep and NLP construction.
    pub solve_setup_secs: f64,
    /// Time spent constructing `AcOpfProblem` (Y-bus, branch admittances,
    /// Jacobian sparsity enumeration). Cumulative across all attempts.
    pub nlp_build_secs: f64,
    /// Time spent inside `NlpSolver::solve()` (the actual interior-point
    /// iterations). Cumulative across all attempts.
    pub nlp_solve_secs: f64,
    /// Time spent extracting the solution from NLP variables into
    /// `OpfSolution` fields (voltages, dispatch, slacks, LMPs).
    pub extract_secs: f64,
    /// Wall-clock total for the entire `solve_ac_opf_with_context_once`
    /// call (equals `solve_time_secs` on the parent `OpfSolution`).
    pub total_secs: f64,
    /// Number of `AcOpfProblem::new()` + `nlp.solve()` pairs executed.
    /// 1 normally; 2 when constraint-screening fallback activates.
    pub nlp_attempts: u32,
}

const OBJECTIVE_LEDGER_TOLERANCE: f64 = 1e-6;

fn objective_term_total(terms: &[ObjectiveTerm]) -> f64 {
    terms.iter().map(|term| term.dollars).sum()
}

fn residual_term_total(terms: &[ObjectiveTerm]) -> f64 {
    terms
        .iter()
        .filter(|term| {
            term.kind == crate::ObjectiveTermKind::Other && term.component_id == "residual"
        })
        .map(|term| term.dollars)
        .sum()
}

fn maybe_push_objective_ledger_mismatch(
    mismatches: &mut Vec<ObjectiveLedgerMismatch>,
    scope_kind: ObjectiveLedgerScopeKind,
    scope_id: impl Into<String>,
    field: &str,
    expected_dollars: f64,
    actual_dollars: f64,
) {
    let difference = actual_dollars - expected_dollars;
    if difference.abs() > OBJECTIVE_LEDGER_TOLERANCE {
        mismatches.push(ObjectiveLedgerMismatch {
            scope_kind,
            scope_id: scope_id.into(),
            field: field.to_string(),
            expected_dollars,
            actual_dollars,
            difference,
        });
    }
}

impl OpfSolution {
    /// Persisted exact-audit status stored on the serialized solution payload.
    pub fn audit(&self) -> &SolutionAuditReport {
        &self.audit
    }

    /// Whether this solution carries an exact objective ledger that can be audited.
    pub fn has_objective_ledger(&self) -> bool {
        !self.objective_terms.is_empty()
    }

    /// Recompute and store the persisted audit block from the exact
    /// objective ledger. Gated by the `SURGE_OBJECTIVE_AUDIT` env var —
    /// see [`crate::objective_audit_enabled`]. When the gate is off
    /// (the default), this is a no-op and the `audit` field stays at
    /// its serde default.
    pub fn refresh_audit(&mut self) {
        if !crate::objective_audit_enabled() {
            return;
        }
        self.audit = <Self as AuditableSolution>::computed_solution_audit(self);
    }

    /// Return every objective-ledger mismatch found on this OPF solution.
    pub fn objective_ledger_mismatches(&self) -> Vec<ObjectiveLedgerMismatch> {
        let mut mismatches = Vec::new();
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::OpfSolution,
            "opf",
            "total_cost",
            objective_term_total(&self.objective_terms),
            self.total_cost,
        );
        maybe_push_objective_ledger_mismatch(
            &mut mismatches,
            ObjectiveLedgerScopeKind::OpfSolution,
            "opf",
            "residual",
            0.0,
            residual_term_total(&self.objective_terms),
        );
        mismatches
    }

    /// Whether the OPF solution's exact objective ledger reconciles cleanly.
    pub fn objective_ledger_is_consistent(&self) -> bool {
        self.objective_ledger_mismatches().is_empty()
    }
}

impl AuditableSolution for OpfSolution {
    fn computed_solution_audit(&self) -> SolutionAuditReport {
        SolutionAuditReport::from_mismatches(self.objective_ledger_mismatches())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectiveBucket, ObjectiveSubjectKind, ObjectiveTermKind};

    fn make_opf(
        opf_type: OpfType,
        gen_p_mw: Vec<f64>,
        gen_q_mvar: Vec<f64>,
        lmp: Vec<f64>,
    ) -> OpfSolution {
        let n_buses = lmp.len();
        let pf = PfSolution {
            voltage_magnitude_pu: vec![1.0; n_buses],
            voltage_angle_rad: vec![0.0; n_buses],
            ..Default::default()
        };
        OpfSolution {
            opf_type,
            base_mva: 100.0,
            power_flow: pf,
            generators: OpfGeneratorResults {
                gen_p_mw,
                gen_q_mvar,
                ..Default::default()
            },
            pricing: OpfPricing {
                lmp,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_opf_solution_dc_construction_and_field_access() {
        let sol = make_opf(
            OpfType::DcOpf,
            vec![50.0, 100.0],
            vec![],
            vec![25.0, 30.0, 28.0],
        );
        assert_eq!(sol.opf_type, OpfType::DcOpf);
        assert_eq!(sol.base_mva, 100.0);
        assert_eq!(sol.generators.gen_p_mw, vec![50.0, 100.0]);
        assert!(
            sol.generators.gen_q_mvar.is_empty(),
            "DC-OPF should have empty gen_q_mvar"
        );
        assert_eq!(sol.pricing.lmp, vec![25.0, 30.0, 28.0]);
        assert_eq!(sol.power_flow.voltage_magnitude_pu.len(), 3);
    }

    #[test]
    fn test_opf_solution_ac_construction_with_reactive() {
        let sol = make_opf(OpfType::AcOpf, vec![75.0], vec![20.0], vec![35.0, 40.0]);
        assert_eq!(sol.opf_type, OpfType::AcOpf);
        assert_eq!(sol.generators.gen_p_mw, vec![75.0]);
        assert_eq!(sol.generators.gen_q_mvar, vec![20.0]);
    }

    #[test]
    fn test_opf_type_variants() {
        assert_eq!(OpfType::DcOpf, OpfType::DcOpf);
        assert_eq!(OpfType::AcOpf, OpfType::AcOpf);
        assert_eq!(OpfType::DcScopf, OpfType::DcScopf);
        assert_eq!(OpfType::AcScopf, OpfType::AcScopf);
        assert_eq!(OpfType::HvdcOpf, OpfType::HvdcOpf);
        assert_ne!(OpfType::DcOpf, OpfType::AcOpf);
    }

    #[test]
    fn test_opf_solution_cost_and_load_fields() {
        let mut sol = make_opf(OpfType::DcOpf, vec![150.0, 200.0], vec![], vec![30.0]);
        sol.total_cost = 5000.0;
        sol.total_load_mw = 340.0;
        sol.total_generation_mw = 350.0;
        sol.total_losses_mw = 10.0;
        assert_eq!(sol.total_cost, 5000.0);
        assert_eq!(sol.total_load_mw, 340.0);
        assert_eq!(sol.total_generation_mw, 350.0);
        assert_eq!(sol.total_losses_mw, 10.0);
    }

    #[test]
    fn test_opf_solution_solver_metadata() {
        let mut sol = make_opf(OpfType::DcOpf, vec![], vec![], vec![]);
        sol.solver_name = Some("HiGHS".to_string());
        sol.solver_version = Some("1.7.0".to_string());
        sol.iterations = Some(42);
        sol.solve_time_secs = 1.23;
        assert_eq!(sol.solver_name.as_deref(), Some("HiGHS"));
        assert_eq!(sol.solver_version.as_deref(), Some("1.7.0"));
        assert_eq!(sol.iterations, Some(42));
        assert_eq!(sol.solve_time_secs, 1.23);
    }

    #[test]
    fn test_opf_solution_binding_branches() {
        let mut sol = make_opf(OpfType::DcOpf, vec![], vec![], vec![]);
        sol.branches.branch_shadow_prices = vec![0.0, 5.2, 0.0, -3.1];
        let binding = sol.branches.binding_branch_indices();
        assert_eq!(binding, vec![1, 3]);
        assert_eq!(sol.branches.branch_shadow_prices[1], 5.2);
        assert_eq!(sol.branches.branch_shadow_prices[3], -3.1);
    }

    #[test]
    fn test_opf_solution_objective_ledger_validation() {
        let mut sol = make_opf(OpfType::DcOpf, vec![50.0], vec![], vec![30.0]);
        sol.total_cost = 500.0;
        sol.objective_terms = vec![ObjectiveTerm {
            component_id: "energy".to_string(),
            bucket: ObjectiveBucket::Energy,
            kind: ObjectiveTermKind::GeneratorEnergy,
            subject_kind: ObjectiveSubjectKind::Resource,
            subject_id: "gen_1_0".to_string(),
            dollars: 500.0,
            quantity: Some(50.0),
            quantity_unit: Some(crate::ObjectiveQuantityUnit::Mwh),
            unit_rate: Some(10.0),
        }];
        assert!(sol.objective_ledger_is_consistent());

        sol.total_cost = 400.0;
        let mismatches = sol.objective_ledger_mismatches();
        assert_eq!(mismatches.len(), 1);
        assert_eq!(
            mismatches[0].scope_kind,
            ObjectiveLedgerScopeKind::OpfSolution
        );
        assert_eq!(mismatches[0].field, "total_cost");
    }
}
