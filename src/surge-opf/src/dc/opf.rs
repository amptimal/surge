// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC Optimal Power Flow (DC-OPF) solver.
//!
//! # DC Power Flow Linearization
//!
//! DC-OPF is based on the **DC power flow** linearization of the full AC power
//! flow equations. This linearization makes three standard assumptions:
//!
//! 1. **Lossless branches**: Branch resistances are neglected (`r = 0`), so all
//!    power injected into the network is delivered. There are no I^2*R losses.
//! 2. **Flat voltage profile**: All bus voltage magnitudes are assumed to be
//!    `|V| = 1.0` p.u. Voltage magnitude constraints and reactive power are
//!    not modeled.
//! 3. **Small angle differences**: `sin(theta_i - theta_j) ≈ theta_i - theta_j`
//!    and `cos(theta_i - theta_j) ≈ 1`. This linearizes the power flow
//!    equations into `P_ij = B_ij * (theta_i - theta_j)`.
//!
//! These assumptions yield a linear (or convex quadratic, for quadratic cost
//! curves) optimization problem that can be solved extremely fast using LP/QP
//! solvers.
//!
//! **When to use DC-OPF:**
//! - Real-time market clearing (DC-OPF is the standard engine used by ISOs
//!   including PJM, ERCOT, CAISO, MISO, SPP, and NYISO for day-ahead and
//!   real-time energy markets).
//! - Congestion analysis and LMP computation.
//! - Security-constrained economic dispatch (SCED) and SCOPF.
//! - Any application where solve speed matters more than voltage/reactive accuracy.
//!
//! **When NOT to use DC-OPF:**
//! - When losses are important (use lossy DC-OPF or AC-OPF).
//! - When voltage limits or reactive power dispatch are needed (use AC-OPF:
//!   [`crate::ac::solve::solve_ac_opf`]).
//! - For distribution networks with high R/X ratios (DC approximation is poor).
//! - When precise branch flow limits in MVA (not just MW) are needed.
//!
//! # Formulation
//!
//! Uses the sparse B-theta formulation (`dc_opf_lp::solve_dc_opf_lp`):
//!
//! ```text
//! min  sum_g (c2_g * Pg^2 + c1_g * Pg + c0_g)
//! s.t. B * theta = Pg_bus - Pd_bus                           (power balance)
//!      -f_max_l <= b_l * (theta_i - theta_j) <= f_max_l      (thermal limits)
//!      Pmin_g <= Pg <= Pmax_g                                 (generator limits)
//! ```
//!
//! Variables are `[theta | Pg | e_g]` where `e_g` are epiograph variables for
//! piecewise-linear cost curves. The constraint matrix is sparse CSC, O(nnz).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use surge_network::Network;
use surge_network::market::PenaltyConfig;
use surge_solution::{OpfSolution, ParSetpoint};

use crate::backends::LpSolver;

// ---------------------------------------------------------------------------
// HVDC link description (moved from hvdc_opf.rs for unified options)
// ---------------------------------------------------------------------------

/// Description of a single HVDC link for the DC-OPF.
///
/// Contains the minimum set of parameters needed to model HVDC in the LP.
/// When passed via [`DcOpfOptions::hvdc_links`], each variable link adds one
/// LP decision variable for P_dc (MW transferred from rectifier to inverter).
/// Both terminal buses must exist in the network; malformed links are rejected
/// during model build instead of being partially applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvdcOpfLink {
    /// AC bus number at the rectifier (power source) end.
    pub from_bus: u32,
    /// AC bus number at the inverter (power sink) end.
    pub to_bus: u32,
    /// Minimum DC power transfer (MW). Must be >= 0.
    pub p_dc_min_mw: f64,
    /// Maximum DC power transfer (MW). When > p_dc_min_mw, P_dc is a variable.
    pub p_dc_max_mw: f64,
    /// Constant loss coefficient `a` (MW): P_loss = a + b × P_dc.
    pub loss_a_mw: f64,
    /// Linear loss coefficient `b` (fraction): P_loss = a + b × P_dc.
    pub loss_b_frac: f64,
    /// Optional name for reporting.
    pub name: String,
}

impl HvdcOpfLink {
    /// Create a lossless HVDC link with variable P_dc bounds.
    pub fn new(from_bus: u32, to_bus: u32, p_dc_min_mw: f64, p_dc_max_mw: f64) -> Self {
        Self {
            from_bus,
            to_bus,
            p_dc_min_mw,
            p_dc_max_mw,
            loss_a_mw: 0.0,
            loss_b_frac: 0.0,
            name: String::new(),
        }
    }

    /// Returns `true` when P_dc is a free variable (p_dc_min < p_dc_max).
    pub fn is_variable(&self) -> bool {
        self.p_dc_min_mw < self.p_dc_max_mw
    }

    /// Compute inverter injection for a given P_dc (after linear losses).
    pub fn p_inv_mw(&self, p_dc_mw: f64) -> f64 {
        let p_loss = self.loss_a_mw + self.loss_b_frac * p_dc_mw;
        p_dc_mw - p_loss
    }
}

/// DC-OPF solver options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DcOpfOptions {
    /// Convergence tolerance.
    /// Default: 1e-8 (LP solver tolerance).
    pub tolerance: f64,
    /// Maximum solver iterations.
    pub max_iterations: u32,
    /// Whether to enforce branch thermal limits.
    pub enforce_thermal_limits: bool,
    /// Minimum rate_a (MVA) to consider a branch as having a thermal limit.
    /// Branches with rate_a below this are unconstrained.
    pub min_rate_a: f64,
    /// Penalty configuration for soft thermal limit violations.
    ///
    /// In the LP/QP formulation, thermal slack variables are added with objective
    /// coefficient `penalty_config.thermal.marginal_cost_at(0.0)` (linear
    /// approximation at the zero-violation point, which equals the first-segment
    /// slope for PiecewiseLinear curves). This converts hard thermal constraints
    /// into soft constraints, allowing the LP to remain feasible when thermal
    /// limits cannot be simultaneously satisfied.
    pub penalty_config: PenaltyConfig,
    /// Convert polynomial generators with quadratic cost (c2 > 0) to a
    /// piecewise-linear (PWL) epigraph formulation, eliminating the QP
    /// Hessian and making the problem a pure LP.
    ///
    /// When `false` (default): exact quadratic objective via QP Hessian.
    /// When `true`: tangent-line outer linearisation with `pwl_cost_breakpoints`
    ///   tangents per generator; both HiGHS and COPT solve the identical LP,
    ///   avoiding HiGHS ASM artefacts on degenerate networks.
    ///
    /// CLI: `--pwl-costs` / `--no-pwl-costs`.
    pub use_pwl_costs: bool,
    /// Number of tangent-line breakpoints per quadratic generator when
    /// `use_pwl_costs = true`.  More breakpoints → smaller approximation error.
    /// Default: 20 (max PWL error ≈ c2*(Pmax-Pmin)²/(4*(N-1)²) ≈ 0.14% for
    /// typical MATPOWER cases).
    pub pwl_cost_breakpoints: usize,
    /// Whether to enforce interface and base-case flowgate constraints.
    ///
    /// When `true` (default): in-service interfaces and base-case flowgates
    /// (those with `contingency_branch = None`) are added as linear constraints
    /// on bus angles in the DC-OPF LP.  Contingency flowgates are always
    /// skipped — they belong in SCOPF.
    pub enforce_flowgates: bool,
    /// Phase-shifting transformers (PARs) operating in flow-setpoint mode.
    ///
    /// Each entry removes the matching branch from the passive B matrix and
    /// replaces it with fixed scheduled injections at its terminal buses.
    /// The optimizer then dispatches the remaining network with the PAR flow
    /// treated as a fixed scheduled interchange.  Post-solve, the implied shift
    /// angle is computed and returned in `OpfSolution::par_results`.
    ///
    /// If a PAR branch is not found in `network.branches`, it is silently ignored.
    pub par_setpoints: Vec<ParSetpoint>,

    /// HVDC links to co-optimize (P_dc as decision variables).
    ///
    /// When `Some(links)`, each link with `p_dc_min < p_dc_max` adds one LP
    /// variable for P_dc. Fixed links (`p_dc_min == p_dc_max`) are baked into
    /// the bus power balance as constant injections. All endpoints must map to
    /// real buses in the network. `None` = no HVDC.
    ///
    /// Variable layout extends the base solver:
    ///   `x = [θ | Pg | P_hvdc | s_upper | s_lower | sg_upper | sg_lower | e_g]`
    pub hvdc_links: Option<Vec<HvdcOpfLink>>,

    /// When set, adds Pmin/Pmax slack variables with this penalty cost ($/MW).
    ///
    /// Enables generator-limit softening for feasibility analysis. Each
    /// in-service generator gets two non-negative slack variables (upper/lower)
    /// penalised at this cost in the objective. `None` = hard gen limits.
    pub gen_limit_penalty: Option<f64>,

    /// Day-ahead virtual energy bids (inc/dec convergence bids).
    ///
    /// Each in-service bid adds one LP variable `v_k ∈ [0, mw_limit/base]` and
    /// a power-balance injection at the specified bus.  When empty (the default),
    /// the solve is identical to a no-virtual-bid run — zero overhead.
    ///
    /// **Day-ahead only** — virtual bids are a DA market instrument.
    /// Do not populate this field for real-time dispatch runs.
    pub virtual_bids: Vec<surge_network::market::VirtualBid>,

    /// Iteratively adjust generator power balance coefficients using marginal
    /// loss sensitivity factors. When `true`, the solver computes penalty
    /// factors `pf[i] = 1/(1 - ∂Loss/∂P_i)` from DC branch flows and modifies
    /// generator coefficients to `-(1 - ∂Loss/∂P_i)` in the power balance rows.
    /// The LP is re-solved with warm start until penalty factors converge.
    /// Loss LMPs then fall out naturally from the dual values.
    pub use_loss_factors: bool,
    /// Maximum iterations for loss factor convergence. Default: 3.
    pub max_loss_iter: usize,
    /// Convergence tolerance for penalty factor change. Default: 1e-3.
    pub loss_tol: f64,
}

impl Default for DcOpfOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-8,
            max_iterations: 200,
            enforce_thermal_limits: true,
            min_rate_a: 1.0,
            penalty_config: PenaltyConfig::default(),
            use_pwl_costs: false,
            pwl_cost_breakpoints: 20,
            enforce_flowgates: true,
            par_setpoints: Vec::new(),
            hvdc_links: None,
            gen_limit_penalty: None,
            virtual_bids: Vec::new(),
            use_loss_factors: false,
            max_loss_iter: 3,
            loss_tol: 1e-3,
        }
    }
}

/// Runtime execution controls for DC-OPF.
///
/// These settings affect how the solve is executed, not the mathematical
/// problem definition itself.
#[derive(Debug, Clone, Default)]
pub struct DcOpfRuntime {
    /// Override LP solver backend. `None` = use the canonical default LP policy.
    pub lp_solver: Option<Arc<dyn LpSolver>>,
    /// Optional starting point for DC-OPF (bus voltage angles, radians).
    ///
    /// HiGHS (LP) has built-in hot-start and does not require an explicit
    /// starting point, so this field is currently stored for API completeness
    /// and future LP-warmstart support. Passing `Some(zeros)` is equivalent
    /// to the default flat-start and produces identical results.
    pub warm_start_theta: Option<Vec<f64>>,
}

impl DcOpfRuntime {
    /// Set the LP solver backend (builder pattern).
    pub fn with_lp_solver(mut self, solver: Arc<dyn LpSolver>) -> Self {
        self.lp_solver = Some(solver);
        self
    }

    /// Set the warm-start bus voltage angles in radians (builder pattern).
    pub fn with_warm_start_theta(mut self, theta: Vec<f64>) -> Self {
        self.warm_start_theta = Some(theta);
        self
    }
}

crate::common::opf_common_errors!(DcOpfError {
    /// Total generation capacity is less than total load.
    #[error("insufficient generation capacity: need {load_mw:.1} MW, max {capacity_mw:.1} MW")]
    InsufficientCapacity { load_mw: f64, capacity_mw: f64 },

    /// The solver did not converge within the iteration limit.
    #[error("solver did not converge in {iterations} iterations")]
    NotConverged { iterations: u32 },

    /// The solver returned a feasible but provably suboptimal solution.
    #[error("solver returned suboptimal solution — result may be far from true optimum")]
    SubOptimalSolution,

    /// The solver reported an infeasible LP/QP.
    #[error("solver reported infeasible problem")]
    InfeasibleProblem,

    /// The solver reported an unbounded LP/QP.
    #[error("solver reported unbounded problem")]
    UnboundedProblem,

    /// A configured HVDC link references a bus that does not exist in the network.
    #[error(
        "invalid HVDC link {index} ({from_bus} -> {to_bus}): {reason}"
    )]
    InvalidHvdcLink {
        index: usize,
        from_bus: u32,
        to_bus: u32,
        reason: String,
    },
});

/// Extended DC-OPF result including HVDC dispatch and gen-limit violations.
///
/// Returned by `solve_dc_opf_full` when HVDC links or gen-limit slacks are active.
/// The base [`OpfSolution`] is always available via the `opf` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DcOpfResult {
    /// Base OPF solution (generator dispatch, LMPs, etc.)
    pub opf: OpfSolution,
    /// Optimal P_dc setpoints for each variable HVDC link (MW).
    /// Same order as variable links in `hvdc_links` (those where `is_variable()` is true).
    /// Empty if no HVDC links were specified.
    pub hvdc_dispatch_mw: Vec<f64>,
    /// Shadow prices for HVDC link capacity constraints ($/MWh).
    ///
    /// Same order as `hvdc_dispatch_mw` (one per variable HVDC link).
    /// Positive when the upper bound (p_dc_max) is binding — the HVDC link is at capacity
    /// and the shadow price equals the inter-area energy price spread.
    /// Negative when the lower bound (p_dc_min) is binding.
    /// Zero when the link is not at either bound.
    pub hvdc_shadow_prices: Vec<f64>,
    /// Generator limit violations: `(gen_index, violation_mw)`.
    /// Non-empty only when `gen_limit_penalty` is set and violations occur.
    pub gen_limit_violations: Vec<(usize, f64)>,
    /// Whether the solution is feasible in the hard-constrained sense
    /// (no gen-limit violations, no thermal violations beyond penalty tolerance).
    pub is_feasible: bool,
}

/// Solve DC-OPF for a network using the sparse B-theta formulation.
///
/// Returns [`DcOpfResult`] containing the base [`OpfSolution`] (dispatch, LMPs,
/// branch shadow prices) plus HVDC dispatch and gen-limit violation data when
/// those features are enabled via options.
///
/// # Example
///
/// ```no_run
/// use surge_io::load;
/// use surge_opf::{DcOpfOptions, solve_dc_opf};
///
/// let net = load("examples/cases/ieee118/case118.surge.json.zst").unwrap();
/// let result = solve_dc_opf(&net, &DcOpfOptions::default()).unwrap();
/// println!("cost=${:.2}/hr, LMP range=[{:.1}, {:.1}]",
///     result.opf.total_cost,
///     result.opf.pricing.lmp.iter().copied().fold(f64::INFINITY, f64::min),
///     result.opf.pricing.lmp.iter().copied().fold(f64::NEG_INFINITY, f64::max),
/// );
/// ```
pub fn solve_dc_opf(network: &Network, options: &DcOpfOptions) -> Result<DcOpfResult, DcOpfError> {
    solve_dc_opf_with_runtime(network, options, &DcOpfRuntime::default())
}

/// Solve DC-OPF with explicit runtime controls (solver backend, warm-start).
pub fn solve_dc_opf_with_runtime(
    network: &Network,
    options: &DcOpfOptions,
    runtime: &DcOpfRuntime,
) -> Result<DcOpfResult, DcOpfError> {
    let mut network = network.clone();
    network.canonicalize_runtime_identities();
    network
        .validate_for_dc_solve()
        .map_err(|e| DcOpfError::InvalidNetwork(e.to_string()))?;
    crate::dc::opf_lp::solve_dc_opf_lp_with_runtime(&network, options, runtime)
}

#[cfg(test)]
mod tests {
    use crate::test_util::case_path;

    use super::*;

    fn format_optional_iterations(iterations: Option<u32>) -> String {
        iterations
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    #[test]
    fn test_dc_opf_case9() {
        let net = surge_io::load(case_path("case9")).unwrap();

        let opts = DcOpfOptions::default();
        let sol = solve_dc_opf(&net, &opts).expect("DC-OPF should solve").opf;

        // Power balance: sum(Pg) = sum(Pd)
        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.1,
            "power balance: gen={total_gen:.2}, load={total_load:.2}"
        );

        // All generators within limits
        let gen_indices: Vec<usize> = net
            .generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.in_service)
            .map(|(i, _)| i)
            .collect();
        for (j, &gi) in gen_indices.iter().enumerate() {
            let g = &net.generators[gi];
            assert!(
                sol.generators.gen_p_mw[j] >= g.pmin - 0.1,
                "gen {} below pmin: {:.2} < {:.2}",
                gi,
                sol.generators.gen_p_mw[j],
                g.pmin
            );
            assert!(
                sol.generators.gen_p_mw[j] <= g.pmax + 0.1,
                "gen {} above pmax: {:.2} > {:.2}",
                gi,
                sol.generators.gen_p_mw[j],
                g.pmax
            );
        }

        // Cost should be positive and reasonable
        assert!(sol.total_cost > 0.0, "cost should be positive");
        assert!(
            sol.total_cost < 100000.0,
            "cost unreasonably high: {}",
            sol.total_cost
        );

        // LMPs should be positive (generators have positive costs)
        for (i, &lmp) in sol.pricing.lmp.iter().enumerate() {
            assert!(
                lmp > 0.0,
                "LMP at bus {} should be positive, got {:.4}",
                net.buses[i].number,
                lmp
            );
        }

        println!(
            "case9 DC-OPF: cost={:.2} $/hr, time={:.2} ms, iters={}",
            sol.total_cost,
            sol.solve_time_secs * 1000.0,
            format_optional_iterations(sol.iterations)
        );
        println!(
            "  Pg: {:?}",
            sol.generators
                .gen_p_mw
                .iter()
                .map(|p| format!("{:.1}", p))
                .collect::<Vec<_>>()
        );
        println!(
            "  LMPs: {:?}",
            sol.pricing
                .lmp
                .iter()
                .map(|l| format!("{:.2}", l))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dc_opf_case14() {
        let net = surge_io::load(case_path("case14")).unwrap();

        let opts = DcOpfOptions::default();
        let sol = solve_dc_opf(&net, &opts).expect("DC-OPF should solve").opf;

        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.1,
            "power balance violated"
        );
        assert!(sol.total_cost > 0.0);

        println!(
            "case14 DC-OPF: cost={:.2} $/hr, time={:.2} ms",
            sol.total_cost,
            sol.solve_time_secs * 1000.0
        );
    }

    #[test]
    fn test_dc_opf_case30() {
        let net = surge_io::load(case_path("case30")).unwrap();

        let opts = DcOpfOptions::default();
        let sol = solve_dc_opf(&net, &opts).expect("DC-OPF should solve").opf;

        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.1,
            "power balance violated"
        );
        assert!(sol.total_cost > 0.0);

        println!(
            "case30 DC-OPF: cost={:.2} $/hr, time={:.2} ms",
            sol.total_cost,
            sol.solve_time_secs * 1000.0
        );
    }

    #[test]
    fn test_dc_opf_case118() {
        let net = surge_io::load(case_path("case118")).unwrap();

        let opts = DcOpfOptions::default();
        let sol = solve_dc_opf(&net, &opts).expect("DC-OPF should solve").opf;

        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.5,
            "power balance: gen={total_gen:.2}, load={total_load:.2}"
        );
        assert!(sol.total_cost > 0.0);

        println!(
            "case118 DC-OPF: cost={:.2} $/hr, time={:.2} ms, iters={}",
            sol.total_cost,
            sol.solve_time_secs * 1000.0,
            format_optional_iterations(sol.iterations)
        );
    }

    #[test]
    fn test_dc_opf_no_thermal_limits() {
        let net = surge_io::load(case_path("case9")).unwrap();

        // Without thermal limits, should give lower (or equal) cost
        let opts_constrained = DcOpfOptions::default();
        let opts_unconstrained = DcOpfOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };

        let sol_c = solve_dc_opf(&net, &opts_constrained).unwrap().opf;
        let sol_u = solve_dc_opf(&net, &opts_unconstrained).unwrap().opf;

        assert!(
            sol_u.total_cost <= sol_c.total_cost + 0.1,
            "unconstrained cost ({:.2}) should be <= constrained ({:.2})",
            sol_u.total_cost,
            sol_c.total_cost
        );
    }

    #[test]
    fn test_dc_opf_missing_cost() {
        let mut net = surge_io::load(case_path("case9")).unwrap();
        // Remove cost from one generator
        net.generators[0].cost = None;

        let result = solve_dc_opf(&net, &DcOpfOptions::default()).map(|r| r.opf);
        assert!(result.is_err());
        match result.unwrap_err() {
            DcOpfError::MissingCost { gen_idx: 0, .. } => {}
            other => panic!("expected MissingCost, got: {other}"),
        }
    }

    #[test]
    fn test_dc_opf_case2383wp() {
        let net = surge_io::load(case_path("case2383wp")).unwrap();

        let opts = DcOpfOptions::default();
        let sol = solve_dc_opf(&net, &opts).expect("DC-OPF should solve").opf;

        let total_gen: f64 = sol.generators.gen_p_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 1.0,
            "power balance: gen={total_gen:.2}, load={total_load:.2}"
        );
        assert!(sol.total_cost > 0.0);

        println!(
            "case2383wp DC-OPF: cost={:.2} $/hr, time={:.1} ms, iters={}",
            sol.total_cost,
            sol.solve_time_secs * 1000.0,
            format_optional_iterations(sol.iterations)
        );
    }
    /// OPF-08: DC-OPF with warm_start_theta=Some(zeros) produces same result as default.
    ///
    /// HiGHS LP has internal hot-start; explicit warm_start_theta has no effect on
    /// the solution but must not cause a panic or incorrect result.
    #[test]
    fn test_opf08_dc_opf_with_warm_start() {
        let net = surge_io::load(case_path("case9")).unwrap();

        // Cold solve (no warm start).
        let cold_opts = DcOpfOptions::default();
        let cold_sol = solve_dc_opf(&net, &cold_opts)
            .expect("cold DC-OPF should solve")
            .opf;

        // Warm solve: provide zero angle vector as starting point.
        let n_bus = net.n_buses();
        let warm_runtime = DcOpfRuntime::default().with_warm_start_theta(vec![0.0; n_bus]);
        let warm_sol = solve_dc_opf_with_runtime(&net, &cold_opts, &warm_runtime)
            .expect("warm DC-OPF should solve")
            .opf;

        // Results must be identical — LP solution is unique.
        let cost_gap =
            (cold_sol.total_cost - warm_sol.total_cost).abs() / cold_sol.total_cost.max(1.0);
        assert!(
            cost_gap < 1e-6,
            "OPF-08: cold cost {:.4} vs warm cost {:.4} (gap={:.2e})",
            cold_sol.total_cost,
            warm_sol.total_cost,
            cost_gap
        );

        // LMPs should match.
        for (i, (&lc, &lw)) in cold_sol
            .pricing
            .lmp
            .iter()
            .zip(warm_sol.pricing.lmp.iter())
            .enumerate()
        {
            assert!(
                (lc - lw).abs() < 1e-4,
                "OPF-08: LMP[{i}] cold={lc:.4} warm={lw:.4}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // MATPOWER reference cost pinning tests
    // -----------------------------------------------------------------------

    /// MATPOWER DC-OPF reference regression test — case9 optimal cost.
    ///
    /// Total cost validated against MATPOWER 7.1 `rundcopf('case9')`.
    /// Reference value: 5216.0266 $/hr.
    #[test]
    fn test_dc_opf_case9_cost_reference() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let result = solve_dc_opf(&net, &DcOpfOptions::default())
            .expect("DC-OPF should converge on case9")
            .opf;

        let ref_cost = 5216.03;
        assert!(
            (result.total_cost - ref_cost).abs() < 1.0,
            "case9 DC-OPF cost regression: got {:.2}, expected {:.2} (tolerance +-$1)",
            result.total_cost,
            ref_cost
        );
    }

    /// MATPOWER DC-OPF reference regression test — case14 optimal cost.
    ///
    /// Total cost validated against MATPOWER 7.1 `rundcopf('case14')`.
    /// Reference value: 7642.5918 $/hr.
    #[test]
    fn test_dc_opf_case14_cost_reference() {
        let net = surge_io::load(case_path("case14")).unwrap();
        let result = solve_dc_opf(&net, &DcOpfOptions::default())
            .expect("DC-OPF should converge on case14")
            .opf;

        let ref_cost = 7642.59;
        assert!(
            (result.total_cost - ref_cost).abs() < 1.0,
            "case14 DC-OPF cost regression: got {:.2}, expected {:.2} (tolerance +-$1)",
            result.total_cost,
            ref_cost
        );
    }

    /// MATPOWER DC-OPF reference regression test — case9 generator dispatch.
    ///
    /// Individual generator Pg values validated against MATPOWER 7.1 `rundcopf('case9')`.
    /// Generator order matches MATPOWER mpc.gen row order (bus 1, bus 2, bus 3).
    #[test]
    fn test_dc_opf_case9_dispatch_reference() {
        let net = surge_io::load(case_path("case9")).unwrap();
        let result = solve_dc_opf(&net, &DcOpfOptions::default())
            .expect("DC-OPF should converge on case9")
            .opf;

        // Reference dispatch (MW) — MATPOWER 7.1 rundcopf('case9').
        // case9 has quadratic costs (QP) and the LP/QP has degenerate primal
        // solutions — different solvers (MIPS, Gurobi, HiGHS) may find
        // different optimal vertices with identical objectives.  Use 1.0 MW
        // tolerance to accommodate solver-dependent vertex selection.
        let ref_pg = &[86.5645, 134.3776, 94.0579];

        assert_eq!(
            result.generators.gen_p_mw.len(),
            ref_pg.len(),
            "case9 should have {} generators, got {}",
            ref_pg.len(),
            result.generators.gen_p_mw.len()
        );

        for (i, &pg) in result.generators.gen_p_mw.iter().enumerate() {
            assert!(
                (pg - ref_pg[i]).abs() < 1.0,
                "case9 DC-OPF gen {} Pg: got {:.4} MW, expected {:.4} MW (tolerance +-1.0 MW)",
                i,
                pg,
                ref_pg[i]
            );
        }
    }

    /// MATPOWER DC-OPF reference regression test — case14 generator dispatch.
    ///
    /// Individual generator Pg values validated against MATPOWER 7.1 `rundcopf('case14')`.
    /// case14 has 5 generators; only the first two carry load under DC-OPF.
    #[test]
    fn test_dc_opf_case14_dispatch_reference() {
        let net = surge_io::load(case_path("case14")).unwrap();
        let result = solve_dc_opf(&net, &DcOpfOptions::default())
            .expect("DC-OPF should converge on case14")
            .opf;

        // Reference dispatch (MW) — MATPOWER 7.1 rundcopf('case14')
        let ref_pg = &[220.9677, 38.0323, 0.0, 0.0, 0.0];

        assert_eq!(
            result.generators.gen_p_mw.len(),
            ref_pg.len(),
            "case14 should have {} generators, got {}",
            ref_pg.len(),
            result.generators.gen_p_mw.len()
        );

        for (i, &pg) in result.generators.gen_p_mw.iter().enumerate() {
            assert!(
                (pg - ref_pg[i]).abs() < 0.1,
                "case14 DC-OPF gen {} Pg: got {:.4} MW, expected {:.4} MW (tolerance +-0.1 MW)",
                i,
                pg,
                ref_pg[i]
            );
        }
    }
}
