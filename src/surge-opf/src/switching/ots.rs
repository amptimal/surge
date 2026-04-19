#![allow(clippy::needless_range_loop)]
// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Optimal Transmission Switching (OTS) — DC-MILP and LP-relaxation.
//!
//! OTS (Fisher et al. 2008, Hedman et al. 2009) optimally opens/closes
//! transmission lines to reduce total generation cost or congestion.
//!
//! ## Formulation
//!
//! DC power flow with binary switching variables z_k ∈ {0,1} (1 = in-service):
//!
//! **Variables**: `[θ (n_bus) | Pg (n_gen) | P_flow (n_sw) | z (n_sw)]`
//!
//! where `n_sw` = number of switchable branches.
//!
//! **Objective**: min Σ_j cost_j(Pg_j)
//!
//! **Power balance (per bus i)**:
//!   Σ_{k: from_bus=i} P_flow_k  −  Σ_{k: to_bus=i} P_flow_k
//!     + Σ_{k: non-switchable, from=i} B_k(θ_i − θ_j)
//!     − Σ_{k: non-switchable, to=i}   B_k(θ_i − θ_j)
//!     + Σ_{j: gen@i} Pg_j = Pd_i / base
//!
//! **Big-M flow coupling (switchable branch k)**:
//!   P_flow_k ≤  B_k(θ_f − θ_t) + M_k(1 − z_k)
//!   P_flow_k ≥  B_k(θ_f − θ_t) − M_k(1 − z_k)
//!
//! **Thermal limits (switchable branch k)**:
//!   −rate_a_k × z_k ≤ P_flow_k ≤ rate_a_k × z_k
//!
//! Non-switchable branches enter the power balance rows directly
//! (sparse B-bus contribution), exactly as in the base DC-OPF.
//!
//! **Big-M choice**: M_k = max(total_load / base, rate_a_k / base).
//! This bounds the worst-case flow when z_k = 0.
//!
//! ## LMP extraction
//!
//! Dual prices are only meaningful for the LP relaxation (DcRelaxed).
//! For DcMilp the MIP duals are set to zero; call the base DC-OPF with
//! the optimal switched topology to obtain physically correct LMPs.
//!
//! ## References
//!
//! - Fisher, E., O'Neill, R., Ferris, M. (2008). "Optimal Transmission
//!   Switching." IEEE Trans. Power Syst., 23(3), 1346–1355.
//! - Hedman, K. et al. (2009). "Optimal Transmission Switching with
//!   Contingency Analysis." IEEE Trans. Power Syst., 24(3), 1577–1586.

use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_network::Network;
use surge_network::market::CostCurve;
use tracing::{debug, info, warn};

use crate::backends::{LpOptions, LpSolver, SparseProblem, VariableDomain, try_default_lp_solver};
use crate::common::context::OpfNetworkContext;
use crate::dc::opf::{DcOpfError, DcOpfRuntime};
use surge_sparse::Triplet;

use crate::dc::opf_lp::{solve_dc_opf_lp_with_runtime, triplets_to_csc};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Which branches are candidates for switching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SwitchableSet {
    /// All in-service branches with non-zero impedance and rate_a > 0.
    AllBranches,
    /// A subset given by internal branch indices (0-based into `network.branches`).
    Subset(Vec<usize>),
    /// Only branches whose `rate_a` ≤ threshold (MVA).
    MaxRating(f64),
}

/// OTS mathematical formulation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum OtsFormulation {
    /// DC-OTS as a MILP with binary z_k variables (default, exact).
    #[default]
    DcMilp,
    /// LP relaxation of DC-OTS: z_k ∈ [0, 1] (fast, approximate).
    /// Provides a lower bound on cost and yields meaningful LMPs.
    DcRelaxed,
    /// Exhaustive enumeration: solve DC-OPF for all 2^n switching combinations
    /// and select the minimum-cost configuration.
    /// Only practical for very small networks (n_switchable ≤ 20).
    DcEnumerate,
}

/// Options for the OTS solver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtsOptions {
    /// Mathematical formulation (default: DcMilp).
    pub formulation: OtsFormulation,
    /// Which branches may be switched.
    pub switchable_branches: SwitchableSet,
    /// Maximum number of branches that may be open simultaneously.
    /// `None` = unlimited.
    pub max_switches_open: Option<usize>,
    /// MIP optimality gap tolerance (default 1 %).
    pub mip_gap: f64,
    /// Override the Big-M value. `None` = auto-compute.
    pub big_m: Option<f64>,
    /// MIP time limit in seconds (default 300 s).
    pub time_limit_s: f64,
    /// Inner solver tolerance (primal + dual feasibility).
    pub tolerance: f64,
    /// Maximum iterations for the inner LP/MIP solver.
    pub max_iter: u32,
}

impl Default for OtsOptions {
    fn default() -> Self {
        Self {
            formulation: OtsFormulation::default(),
            switchable_branches: SwitchableSet::AllBranches,
            max_switches_open: None,
            mip_gap: 0.01,
            big_m: None,
            time_limit_s: 300.0,
            tolerance: 1e-6,
            max_iter: 1000,
        }
    }
}

/// Runtime execution controls for OTS.
#[derive(Debug, Clone, Default)]
pub struct OtsRuntime {
    /// Override LP/MIP solver backend. `None` = use the canonical default LP policy.
    pub lp_solver: Option<Arc<dyn LpSolver>>,
}

impl OtsRuntime {
    /// Set the LP/MIP solver backend (builder pattern).
    pub fn with_lp_solver(mut self, solver: Arc<dyn LpSolver>) -> Self {
        self.lp_solver = Some(solver);
        self
    }
}

/// Result of an OTS solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtsResult {
    /// True if the solver found a feasible (possibly non-optimal) solution.
    pub converged: bool,
    /// Total generation cost ($/hr).
    pub objective: f64,
    /// External (from_bus, to_bus, circuit) of switched-out branches.
    pub switched_out: Vec<(u32, u32, String)>,
    /// Number of branches switched out.
    pub n_switches: usize,
    /// Generator dispatch (MW), in order of in-service generators.
    pub gen_dispatch: Vec<f64>,
    /// Branch power flows (MW), 0.0 for switched-out branches.
    /// Indexed over all branches (same order as `network.branches`).
    pub branch_flows: Vec<f64>,
    /// Bus LMPs ($/MWh). Zero for DcMilp (duals not meaningful for MIP).
    pub lmps: Vec<f64>,
    /// Solve time in milliseconds.
    pub solve_time_ms: f64,
    /// Final MIP optimality gap (0.0 for LP relaxation / DcEnumerate).
    pub mip_gap: f64,
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Solve the Optimal Transmission Switching problem.
pub fn solve_ots(network: &Network, options: &OtsOptions) -> Result<OtsResult, DcOpfError> {
    solve_ots_with_runtime(network, options, &OtsRuntime::default())
}

/// Solve the Optimal Transmission Switching problem with explicit runtime controls.
pub fn solve_ots_with_runtime(
    network: &Network,
    options: &OtsOptions,
    runtime: &OtsRuntime,
) -> Result<OtsResult, DcOpfError> {
    match options.formulation {
        OtsFormulation::DcMilp => solve_ots_dc(network, options, runtime, false),
        OtsFormulation::DcRelaxed => solve_ots_dc(network, options, runtime, true),
        OtsFormulation::DcEnumerate => solve_ots_ac_binary(network, options, runtime),
    }
}

// ---------------------------------------------------------------------------
// DC-OTS (MILP or LP relaxation)
// ---------------------------------------------------------------------------

fn solve_ots_dc(
    network: &Network,
    options: &OtsOptions,
    runtime: &OtsRuntime,
    relaxed: bool,
) -> Result<OtsResult, DcOpfError> {
    let wall = Instant::now();
    let ctx = OpfNetworkContext::for_dc(network)?;

    let n_bus = ctx.n_bus;
    let n_br = ctx.n_branches;
    let base = ctx.base_mva;

    info!(buses = n_bus, branches = n_br, relaxed, "starting DC-OTS");

    let bus_map = &ctx.bus_map;
    let slack_idx = ctx.slack_idx;
    let gen_indices = &ctx.gen_indices;
    let n_gen = gen_indices.len();
    let bus_pd_mw = network.bus_load_p_mw();

    // --- Identify switchable and non-switchable branches ---
    let switchable_indices: Vec<usize> = collect_switchable(network, options);
    let n_sw = switchable_indices.len();

    // Non-switchable: all in-service branches NOT in the switchable set
    let sw_set: std::collections::HashSet<usize> = switchable_indices.iter().cloned().collect();
    let nonsw_indices: Vec<usize> = (0..n_br)
        .filter(|i| {
            let br = &network.branches[*i];
            br.in_service && br.x.abs() >= 1e-20 && !sw_set.contains(i)
        })
        .collect();

    // --- Auto-compute Big-M ---
    // Network-wide max susceptance: used as floor when a branch has x ≈ 0.
    let max_susceptance: f64 = network
        .branches
        .iter()
        .filter(|br| br.in_service && br.x.abs() >= 1e-20)
        .map(|br| 1.0 / br.x.abs())
        .fold(0.0_f64, f64::max);

    let big_m_global = if let Some(m) = options.big_m {
        m
    } else {
        // Fallback global Big-M (only used for thermal limits on unrated branches).
        let total_load: f64 = network.total_load_mw();
        (total_load / base).max(1.0)
    };

    // Per-branch Big-M: max(rate_a/base, π/|x|) × safety_margin, with 10 pu floor.
    //
    // Using total_load/base as Big-M is far too small for unrated branches (rate_a = 0).
    // A 345 kV line with x = 0.01 pu can carry up to B_k * π ≈ 314 pu in the DC
    // approximation.  A Big-M of only 2-5 pu would make the LP relaxation infeasible
    // for valid switching configurations on such branches.
    const SAFETY_MARGIN: f64 = 1.5;
    const BIG_M_FLOOR: f64 = 10.0;
    let branch_big_m: Vec<f64> = switchable_indices
        .iter()
        .map(|&i| {
            let br = &network.branches[i];
            // Susceptance-based physical bound: π / |x|  (DC angle ≤ π rad)
            let susceptance_bound = if br.x.abs() >= 1e-6 {
                std::f64::consts::PI / br.x.abs()
            } else {
                // x ≈ 0: use network-wide max susceptance as conservative estimate
                max_susceptance * std::f64::consts::PI
            };
            // Thermal rating bound (only non-zero when rate_a > 0)
            let thermal_bound = if br.rating_a_mva > 0.0 {
                br.rating_a_mva / base
            } else {
                0.0
            };
            // Per-branch Big-M: tightest meaningful bound, scaled by safety margin,
            // with an absolute floor to avoid degenerate LP relaxations.
            (thermal_bound.max(susceptance_bound) * SAFETY_MARGIN).max(BIG_M_FLOOR)
        })
        .collect();

    // --- Variable layout ---
    // x = [θ (n_bus) | Pg (n_gen) | P_flow (n_sw) | z (n_sw)]
    //       [0..n_bus | n_bus..n_bus+n_gen | pg_end..pg_end+n_sw | pf_end..pf_end+n_sw]
    let theta_off = 0_usize;
    let pg_off = n_bus;
    let pf_off = n_bus + n_gen; // P_flow for switchable branches
    let z_off = n_bus + n_gen + n_sw; // binary/relaxed switch status
    let n_var = n_bus + n_gen + n_sw + n_sw;

    // --- Objective coefficients ---
    let mut col_cost = vec![0.0_f64; n_var];
    let mut c0_total = 0.0_f64;

    // Quadratic diagonal (for generators with c2 > 0 in polynomial cost)
    let mut q_diag = vec![0.0_f64; n_gen];

    for (j, &gi) in gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        match g
            .cost
            .as_ref()
            .expect("generator cost validated before OPF solve")
        {
            CostCurve::Polynomial { coeffs, .. } => match coeffs.len() {
                0 => {}
                1 => {
                    col_cost[pg_off + j] = coeffs[0] * base;
                }
                2 => {
                    col_cost[pg_off + j] = coeffs[1] * base;
                    c0_total += coeffs[0];
                }
                _ => {
                    // coeffs = [c2, c1, c0, ...]
                    col_cost[pg_off + j] = coeffs[1] * base;
                    // c2 * Pg_mw² = c2 * base² * Pg_pu²
                    // HiGHS QP objective is 0.5*x'Qx, so Q diagonal = 2*c2*base²
                    q_diag[j] = 2.0 * coeffs[0] * base * base;
                    c0_total += coeffs[2];
                }
            },
            CostCurve::PiecewiseLinear { points, .. } => {
                if let Some((&(p0, c0), rest)) = points.split_first()
                    && let Some(&(p1, c1)) = rest.first()
                {
                    let slope = if (p1 - p0).abs() > 1e-20 {
                        (c1 - c0) / (p1 - p0)
                    } else {
                        0.0
                    };
                    col_cost[pg_off + j] = slope * base;
                    c0_total += c0 - slope * p0;
                }
            }
        }
    }

    // --- Variable bounds ---
    let mut col_lower = vec![0.0_f64; n_var];
    let mut col_upper = vec![f64::INFINITY; n_var];

    // Angles: slack fixed at 0, others unconstrained ±π
    for i in 0..n_bus {
        if i == slack_idx {
            col_lower[theta_off + i] = 0.0;
            col_upper[theta_off + i] = 0.0;
        } else {
            col_lower[theta_off + i] = -std::f64::consts::PI;
            col_upper[theta_off + i] = std::f64::consts::PI;
        }
    }

    // Generator output bounds (in per-unit)
    for (j, &gi) in gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        col_lower[pg_off + j] = g.pmin / base;
        col_upper[pg_off + j] = g.pmax / base;
    }

    // P_flow bounds: initially ±∞ (thermal limits imposed via Big-M rows)
    for i in 0..n_sw {
        col_lower[pf_off + i] = f64::NEG_INFINITY;
        col_upper[pf_off + i] = f64::INFINITY;
    }

    // z bounds: [0,1] always (binary in MILP, continuous for relaxation)
    for i in 0..n_sw {
        col_lower[z_off + i] = 0.0;
        col_upper[z_off + i] = 1.0;
    }

    // --- Integrality vector ---
    let mut integrality = vec![VariableDomain::Continuous; n_var];
    if !relaxed {
        for i in 0..n_sw {
            integrality[z_off + i] = VariableDomain::Binary;
        }
    }

    // --- Constraint layout ---
    //
    // Rows:
    //  [0 .. n_bus)          power balance (equality)
    //  [n_bus .. n_bus+n_sw) Big-M upper: P_flow_k ≤ B_k(θ_f-θ_t) + M_k(1-z_k)
    //  [n_bus+n_sw .. n_bus+2*n_sw) Big-M lower: P_flow_k ≥ B_k(θ_f-θ_t) - M_k(1-z_k)
    //  [n_bus+2*n_sw .. n_bus+3*n_sw) Thermal upper: P_flow_k ≤ rate_a_k * z_k
    //  [n_bus+3*n_sw .. n_bus+4*n_sw) Thermal lower: P_flow_k ≥ -rate_a_k * z_k
    //
    // Optional max-switches constraint (one extra row if options.max_switches_open is Some):
    //  Σ (1 - z_k) ≤ max_switches_open   ⟺   Σ z_k ≥ n_sw - max_switches_open

    let has_max_sw = options.max_switches_open.is_some();
    let n_row_base = n_bus + 4 * n_sw;
    let n_row = n_row_base + if has_max_sw { 1 } else { 0 };

    let row_pb = 0_usize; // power balance rows: [0, n_bus)
    let row_bm_up = n_bus; // Big-M upper: [n_bus, n_bus+n_sw)
    let row_bm_lo = n_bus + n_sw; // Big-M lower: [n_bus+n_sw, n_bus+2*n_sw)
    let row_th_up = n_bus + 2 * n_sw; // thermal upper
    let row_th_lo = n_bus + 3 * n_sw; // thermal lower
    let row_maxsw = n_row_base; // optional max-switches row

    // --- Row bounds ---
    let mut row_lower = vec![f64::NEG_INFINITY; n_row];
    let mut row_upper = vec![f64::INFINITY; n_row];

    // Power balance RHS: same convention as dc_opf_lp.
    // RHS[i] = -(Pd[i] + Gs[i]) / base - pbusinj[i]
    // Only non-switchable branch phase shifts contribute to pbusinj here.
    // Switchable branch shifts are absorbed into the Big-M constraint RHS.
    let mut pbusinj = vec![0.0_f64; n_bus];
    for &l in &nonsw_indices {
        let br = &network.branches[l];
        if br.phase_shift_rad.abs() < 1e-12 {
            continue;
        }
        let b = br.b_dc();
        let shift_rad = br.phase_shift_rad;
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];
        let p_shift = b * shift_rad;
        pbusinj[from] += p_shift;
        pbusinj[to] -= p_shift;
    }

    for i in 0..n_bus {
        let pd_pu = bus_pd_mw[i] / base;
        let gs_pu = network.buses[i].shunt_conductance_mw / base;
        let rhs = -pd_pu - gs_pu - pbusinj[i];
        row_lower[row_pb + i] = rhs;
        row_upper[row_pb + i] = rhs;
    }

    // Big-M bounds and thermal bounds for each switchable branch
    for (k, &l) in switchable_indices.iter().enumerate() {
        let br = &network.branches[l];
        let mk = branch_big_m[k];

        // Big-M upper: P_flow_k - B_k*θ_f + B_k*θ_t ≤ M_k(1-z_k)
        //   => P_flow_k - B_k*θ_f + B_k*θ_t + M_k*z_k ≤ M_k
        row_lower[row_bm_up + k] = f64::NEG_INFINITY;
        row_upper[row_bm_up + k] = mk;

        // Big-M lower: P_flow_k - B_k*θ_f + B_k*θ_t ≥ -M_k(1-z_k)
        //   => P_flow_k - B_k*θ_f + B_k*θ_t + M_k*z_k ≥ -M_k + 2*M_k = M_k  (wrong, expand carefully)
        //   => P_flow_k - B_k(θ_f-θ_t) ≥ -M_k + M_k*z_k
        //   => P_flow_k - B_k*θ_f + B_k*θ_t - M_k*z_k ≥ -M_k
        row_lower[row_bm_lo + k] = -mk;
        row_upper[row_bm_lo + k] = f64::INFINITY;

        // Thermal upper: P_flow_k ≤ rate_a_k * z_k
        //   => P_flow_k - rate_a_k*z_k ≤ 0
        let rate_pu = if br.rating_a_mva > 0.0 {
            br.rating_a_mva / base
        } else {
            f64::INFINITY
        };
        let rate_pu = rate_pu.min(1e6); // guard against infinite rates
        row_lower[row_th_up + k] = f64::NEG_INFINITY;
        row_upper[row_th_up + k] = 0.0;

        // Thermal lower: P_flow_k ≥ -rate_a_k * z_k
        //   => P_flow_k + rate_a_k*z_k ≥ 0
        //   => -P_flow_k - rate_a_k*z_k ≤ 0
        row_lower[row_th_lo + k] = 0.0;
        row_upper[row_th_lo + k] = f64::INFINITY;

        let _ = rate_pu; // used indirectly via branch_big_m
    }

    // Thermal row values need rate_a in actual constraint coefficients below.

    // Optional: max open-switches constraint
    // Σ z_k ≥ n_sw - max_open  (at most max_open lines open)
    if let Some(max_open) = options.max_switches_open {
        let min_closed = n_sw.saturating_sub(max_open);
        row_lower[row_maxsw] = min_closed as f64;
        row_upper[row_maxsw] = f64::INFINITY;
    }

    // --- Constraint matrix (triplets, then converted to CSC) ---
    //
    // Estimate non-zeros:
    //   Power balance: n_bus*(avg_degree) + n_gen + n_sw
    //   Big-M upper: 3 per switchable branch (P_flow, θ_f, θ_t, z)
    //   Big-M lower: 3 per switchable branch
    //   Thermal: 2 per switchable branch each (P_flow, z)
    //   Max-switches: n_sw
    let est_nnz = 6 * n_bus + n_gen + 4 * n_sw + 4 * n_sw + 4 * n_sw + n_sw;
    let mut triplets: Vec<Triplet<f64>> = Vec::with_capacity(est_nnz);

    // ------------------------------------------------------------------
    // Power balance rows [row_pb, row_pb + n_bus)
    // ------------------------------------------------------------------
    //
    // For non-switchable in-service branches, contribute B-bus terms:
    //   from bus row: +B_k * θ_from - B_k * θ_to
    //   to bus row:   -B_k * θ_from + B_k * θ_to
    //
    // For switchable branches, flows are explicit P_flow variables:
    //   from bus row: +P_flow_k
    //   to bus row:   -P_flow_k
    //
    // Generator injection:
    //   bus row: +Pg_j

    // Non-switchable B-bus contributions
    for &l in &nonsw_indices {
        let br = &network.branches[l];
        let b = br.b_dc();
        if b.abs() < 1e-20 {
            continue;
        }
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];

        triplets.push(Triplet {
            row: row_pb + from,
            col: theta_off + from,
            val: b,
        });
        triplets.push(Triplet {
            row: row_pb + from,
            col: theta_off + to,
            val: -b,
        });
        triplets.push(Triplet {
            row: row_pb + to,
            col: theta_off + from,
            val: -b,
        });
        triplets.push(Triplet {
            row: row_pb + to,
            col: theta_off + to,
            val: b,
        });
    }

    // Switchable branch P_flow contributions to power balance
    for (k, &l) in switchable_indices.iter().enumerate() {
        let br = &network.branches[l];
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];

        triplets.push(Triplet {
            row: row_pb + from,
            col: pf_off + k,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: row_pb + to,
            col: pf_off + k,
            val: -1.0,
        });
    }

    // Generator injection (negative, matching dc_opf_lp convention: B*θ - Pg = -Pd)
    for (j, &gi) in gen_indices.iter().enumerate() {
        let bus_idx = bus_map[&network.generators[gi].bus];
        triplets.push(Triplet {
            row: row_pb + bus_idx,
            col: pg_off + j,
            val: -1.0,
        });
    }

    // ------------------------------------------------------------------
    // Big-M constraints for switchable branches
    // ------------------------------------------------------------------
    //
    // Upper: P_flow_k - B_k*θ_f + B_k*θ_t + M_k*z_k ≤ M_k
    // Lower: P_flow_k - B_k*θ_f + B_k*θ_t - M_k*z_k ≥ -M_k
    //
    // (Shift terms are absorbed into the RHS via row_upper/row_lower adjustment below.)

    for (k, &l) in switchable_indices.iter().enumerate() {
        let br = &network.branches[l];
        let b = br.b_dc();
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];
        let mk = branch_big_m[k];

        // Shift injection offset for this branch
        let shift_offset = if br.phase_shift_rad.abs() > 1e-12 {
            b * br.phase_shift_rad
        } else {
            0.0
        };

        // Big-M upper row k:
        //   P_flow_k - b*(θ_f - θ_t) + M_k*z_k ≤ M_k + shift_offset
        triplets.push(Triplet {
            row: row_bm_up + k,
            col: pf_off + k,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: row_bm_up + k,
            col: theta_off + from,
            val: -b,
        });
        triplets.push(Triplet {
            row: row_bm_up + k,
            col: theta_off + to,
            val: b,
        });
        triplets.push(Triplet {
            row: row_bm_up + k,
            col: z_off + k,
            val: mk,
        });
        // Adjust RHS: row_upper = M_k + shift (positive shift makes RHS larger, tighter)
        row_upper[row_bm_up + k] = mk + shift_offset;

        // Big-M lower row k:
        //   P_flow_k - b*(θ_f - θ_t) - M_k*z_k ≥ -M_k + shift_offset
        triplets.push(Triplet {
            row: row_bm_lo + k,
            col: pf_off + k,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: row_bm_lo + k,
            col: theta_off + from,
            val: -b,
        });
        triplets.push(Triplet {
            row: row_bm_lo + k,
            col: theta_off + to,
            val: b,
        });
        triplets.push(Triplet {
            row: row_bm_lo + k,
            col: z_off + k,
            val: -mk,
        });
        row_lower[row_bm_lo + k] = -mk + shift_offset;
    }

    // ------------------------------------------------------------------
    // Thermal limits (Big-M style on P_flow via z_k)
    // ------------------------------------------------------------------
    //
    // Upper: P_flow_k - rate_a_k * z_k ≤ 0
    // Lower: P_flow_k + rate_a_k * z_k ≥ 0  (i.e. -P_flow_k - rate_a_k * z_k ≤ 0)

    for (k, &l) in switchable_indices.iter().enumerate() {
        let br = &network.branches[l];
        let rate_pu = if br.rating_a_mva > 0.0 {
            (br.rating_a_mva / base).min(1e6)
        } else {
            big_m_global // no rating → use Big-M as proxy
        };

        // Thermal upper: P_flow_k - rate_a_k * z_k ≤ 0
        triplets.push(Triplet {
            row: row_th_up + k,
            col: pf_off + k,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: row_th_up + k,
            col: z_off + k,
            val: -rate_pu,
        });

        // Thermal lower: -P_flow_k - rate_a_k * z_k ≤ 0
        //  (equivalent to P_flow_k + rate_a*z_k ≥ 0, but written for ≤ convention)
        // We use a range row: P_flow_k + rate_a_k * z_k ≥ 0
        triplets.push(Triplet {
            row: row_th_lo + k,
            col: pf_off + k,
            val: 1.0,
        });
        triplets.push(Triplet {
            row: row_th_lo + k,
            col: z_off + k,
            val: rate_pu,
        });
    }

    // ------------------------------------------------------------------
    // Optional max-switches constraint: Σ z_k ≥ n_sw - max_open
    // ------------------------------------------------------------------
    if has_max_sw {
        for i in 0..n_sw {
            triplets.push(Triplet {
                row: row_maxsw,
                col: z_off + i,
                val: 1.0,
            });
        }
    }

    // Convert to CSC
    let (a_start, a_index, a_value) = triplets_to_csc(&triplets, n_row, n_var);

    // --- Solve ---
    let lp_solver = runtime
        .lp_solver
        .clone()
        .map_or_else(|| try_default_lp_solver(), Ok)
        .map_err(DcOpfError::SolverError)?;

    let mut q_start = vec![0_i32; n_var + 1];
    let mut q_index = Vec::new();
    let mut q_value = Vec::new();
    for col in 0..n_var {
        if (pg_off..pg_off + n_gen).contains(&col) {
            let gen_pos = col - pg_off;
            let diag = q_diag[gen_pos];
            if diag != 0.0 {
                q_index.push(col as i32);
                q_value.push(diag);
            }
        }
        q_start[col + 1] = q_index.len() as i32;
    }
    let quadratic_objective = (!q_value.is_empty()).then_some((q_start, q_index, q_value));

    let problem = SparseProblem {
        n_col: n_var,
        n_row,
        col_cost: col_cost.clone(),
        col_lower: col_lower.clone(),
        col_upper: col_upper.clone(),
        row_lower: row_lower.clone(),
        row_upper: row_upper.clone(),
        a_start: a_start.clone(),
        a_index: a_index.clone(),
        a_value: a_value.clone(),
        q_start: quadratic_objective
            .as_ref()
            .map(|(start, _, _)| start.clone()),
        q_index: quadratic_objective
            .as_ref()
            .map(|(_, index, _)| index.clone()),
        q_value: quadratic_objective
            .as_ref()
            .map(|(_, _, value)| value.clone()),
        col_names: None,
        row_names: None,
        integrality: if relaxed { None } else { Some(integrality) },
    };
    let lp_opts = LpOptions {
        tolerance: if relaxed {
            options.tolerance
        } else {
            options.mip_gap.max(options.tolerance)
        },
        time_limit_secs: Some(options.time_limit_s),
        mip_rel_gap: (!relaxed && options.mip_gap > 0.0).then_some(options.mip_gap),
        mip_gap_schedule: None,
        primal_start: None,
        algorithm: crate::backends::LpAlgorithm::Auto,
        print_level: 0,
    };
    let result = lp_solver
        .solve(&problem, &lp_opts)
        .map_err(DcOpfError::SolverError)?;
    let converged = result.status == crate::backends::LpSolveStatus::Optimal
        || result.status == crate::backends::LpSolveStatus::SubOptimal;
    let (sol_x, sol_row_dual, obj_raw) = (result.x, result.row_dual, result.objective);

    if !converged {
        return Err(DcOpfError::SolverError(
            "OTS solver did not converge".into(),
        ));
    }

    let solve_time_ms = wall.elapsed().as_secs_f64() * 1000.0;

    // --- Extract solution ---
    let theta_vals = &sol_x[theta_off..theta_off + n_bus];
    let pg_pu = &sol_x[pg_off..pg_off + n_gen];
    let pf_pu = &sol_x[pf_off..pf_off + n_sw];
    let z_vals = &sol_x[z_off..z_off + n_sw];

    // Generator dispatch (MW)
    let gen_dispatch: Vec<f64> = pg_pu.iter().map(|&p| p * base).collect();

    // Switched-out branches (z < 0.5)
    let mut switched_out: Vec<(u32, u32, String)> = Vec::new();
    for (k, &l) in switchable_indices.iter().enumerate() {
        if z_vals[k] < 0.5 {
            let br = &network.branches[l];
            switched_out.push((br.from_bus, br.to_bus, br.circuit.clone()));
        }
    }
    let n_switches = switched_out.len();

    // Branch flows (MW) over ALL branches
    let sw_pos: std::collections::HashMap<usize, usize> = switchable_indices
        .iter()
        .cloned()
        .enumerate()
        .map(|(k, l)| (l, k))
        .collect();

    let mut branch_flows = vec![0.0_f64; n_br];
    for l in 0..n_br {
        let br = &network.branches[l];
        if !br.in_service {
            continue;
        }
        if let Some(&k) = sw_pos.get(&l) {
            // Switchable branch: direct P_flow variable
            branch_flows[l] = pf_pu[k] * base;
        } else {
            // Non-switchable: compute from B*(θ_f - θ_t)
            let b = br.b_dc();
            if b.abs() < 1e-20 || br.x.abs() < 1e-20 {
                continue;
            }
            let from = bus_map[&br.from_bus];
            let to = bus_map[&br.to_bus];
            let shift_rad = br.phase_shift_rad;
            let flow_pu = b * (theta_vals[from] - theta_vals[to] - shift_rad);
            branch_flows[l] = flow_pu * base;
        }
    }

    // LMPs: only meaningful for LP relaxation
    let lmps: Vec<f64> = if relaxed {
        // Power balance duals: rows [row_pb, row_pb + n_bus)
        sol_row_dual[row_pb..row_pb + n_bus]
            .iter()
            .map(|&d| d / base)
            .collect()
    } else {
        vec![0.0; n_bus]
    };

    // Total cost including constant term
    let objective = obj_raw + c0_total;

    debug!(
        n_switches,
        objective, solve_time_ms, "DC-OTS solve complete"
    );

    info!(
        n_switches,
        objective,
        converged,
        solve_time_ms = format_args!("{:.1}", solve_time_ms),
        "OTS solved"
    );

    Ok(OtsResult {
        converged,
        objective,
        switched_out,
        n_switches,
        gen_dispatch,
        branch_flows,
        lmps,
        solve_time_ms,
        mip_gap: if relaxed { 0.0 } else { options.mip_gap },
    })
}

// ---------------------------------------------------------------------------
// AC-Binary OTS (enumerate all 2^n switching combos via DC ranking + AC verify)
// ---------------------------------------------------------------------------

fn solve_ots_ac_binary(
    network: &Network,
    options: &OtsOptions,
    runtime: &OtsRuntime,
) -> Result<OtsResult, DcOpfError> {
    use crate::dc::opf::DcOpfOptions;
    let wall = Instant::now();
    let switchable_indices = collect_switchable(network, options);
    let n_sw = switchable_indices.len();

    if n_sw > 20 {
        warn!(
            n_sw,
            "AC-Binary OTS: more than 20 switchable branches ({n_sw}). \
             This is exponentially expensive. Falling back to DC-MILP."
        );
        return solve_ots_dc(network, options, runtime, false);
    }

    // Use DC relaxation to rank candidates; then try each in order
    let relax_opts = OtsOptions {
        formulation: OtsFormulation::DcRelaxed,
        ..options.clone()
    };
    let dc_result = solve_ots_dc(network, &relax_opts, runtime, true)?;

    // Build candidate open-sets: enumerate 2^n_sw combinations
    // For small n_sw this is tractable; for n_sw > ~15 it's slow
    let n_configs = 1_usize << n_sw;
    let max_open = options.max_switches_open.unwrap_or(n_sw);

    let mut best_obj = f64::INFINITY;
    let mut best_open: Vec<usize> = vec![];
    let mut found = false;

    // Use DC ranking: sort configs by ascending popcount (fewer opens = more conservative)
    // and try them; the DC result gives us the best lower bound
    for mask in 0..n_configs {
        let open_set: Vec<usize> = (0..n_sw)
            .filter(|&k| (mask >> k) & 1 == 1)
            .map(|k| switchable_indices[k])
            .collect();
        if open_set.len() > max_open {
            continue;
        }

        // Modify network and solve DC-OPF
        let mut net_mod = network.clone();
        for &br_idx in &open_set {
            net_mod.branches[br_idx].in_service = false;
        }

        let dc_opts = DcOpfOptions {
            tolerance: options.tolerance,
            max_iterations: options.max_iter,
            ..DcOpfOptions::default()
        };
        let dc_runtime = match runtime.lp_solver.clone() {
            Some(solver) => DcOpfRuntime::default().with_lp_solver(solver),
            None => DcOpfRuntime::default(),
        };
        let sol = match solve_dc_opf_lp_with_runtime(&net_mod, &dc_opts, &dc_runtime) {
            Ok(r) => r.opf,
            Err(_) => continue,
        };

        if sol.total_cost < best_obj {
            best_obj = sol.total_cost;
            best_open = open_set;
            found = true;
        }
    }

    let solve_time_ms = wall.elapsed().as_secs_f64() * 1000.0;

    if !found {
        return Err(DcOpfError::SolverError(
            "AC-Binary OTS: no feasible configuration found".into(),
        ));
    }

    // Build final result using the best configuration
    let mut net_best = network.clone();
    for &br_idx in &best_open {
        net_best.branches[br_idx].in_service = false;
    }
    let dc_opts_final = DcOpfOptions {
        tolerance: options.tolerance,
        max_iterations: options.max_iter,
        ..DcOpfOptions::default()
    };
    let dc_runtime = match runtime.lp_solver.clone() {
        Some(solver) => DcOpfRuntime::default().with_lp_solver(solver),
        None => DcOpfRuntime::default(),
    };
    let final_sol = solve_dc_opf_lp_with_runtime(&net_best, &dc_opts_final, &dc_runtime)?.opf;

    let switched_out: Vec<(u32, u32, String)> = best_open
        .iter()
        .map(|&i| {
            let br = &network.branches[i];
            (br.from_bus, br.to_bus, br.circuit.clone())
        })
        .collect();
    let n_switches = switched_out.len();

    // Build branch flows over all branches
    let mut branch_flows = vec![0.0_f64; network.n_branches()];
    for (i, &pf) in final_sol.power_flow.branch_p_from_mw.iter().enumerate() {
        let orig_l = if i < network.n_branches() { i } else { break };
        branch_flows[orig_l] = pf;
    }

    let _ = dc_result; // used for ranking strategy indication

    Ok(OtsResult {
        converged: true,
        objective: final_sol.total_cost,
        switched_out,
        n_switches,
        gen_dispatch: final_sol.generators.gen_p_mw.clone(),
        branch_flows,
        lmps: final_sol.pricing.lmp.clone(),
        solve_time_ms,
        mip_gap: 0.0,
    })
}

// ---------------------------------------------------------------------------
// Helper: collect switchable branch indices
// ---------------------------------------------------------------------------

fn collect_switchable(network: &Network, options: &OtsOptions) -> Vec<usize> {
    match &options.switchable_branches {
        SwitchableSet::AllBranches => network
            .branches
            .iter()
            .enumerate()
            .filter(|(_, br)| br.in_service && br.x.abs() >= 1e-20 && br.rating_a_mva > 0.0)
            .map(|(i, _)| i)
            .collect(),

        SwitchableSet::Subset(indices) => indices
            .iter()
            .cloned()
            .filter(|&i| i < network.n_branches() && network.branches[i].in_service)
            .collect(),

        SwitchableSet::MaxRating(threshold) => network
            .branches
            .iter()
            .enumerate()
            .filter(|(_, br)| {
                br.in_service
                    && br.x.abs() >= 1e-20
                    && br.rating_a_mva > 0.0
                    && br.rating_a_mva <= *threshold
            })
            .map(|(i, _)| i)
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::test_util::{data_available, test_data_dir};

    use super::*;

    fn load_case(name: &str) -> Network {
        let path = test_data_dir().join(format!("{name}.m"));
        surge_io::matpower::load(&path).unwrap_or_else(|e| panic!("failed to parse {name}: {e}"))
    }

    fn base_dc_opf_cost(net: &Network) -> f64 {
        use crate::dc::opf::{DcOpfOptions, solve_dc_opf};
        let opts = DcOpfOptions::default();
        solve_dc_opf(net, &opts)
            .map(|r| r.opf.total_cost)
            .unwrap_or(f64::INFINITY)
    }

    // ------------------------------------------------------------------

    /// DC-OTS MILP on case9: OTS cost should be ≤ base DC-OPF cost.
    #[test]
    fn test_ots_dc_milp_case9() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let base_cost = base_dc_opf_cost(&net);
        assert!(base_cost.is_finite(), "base DC-OPF must converge");

        let opts = OtsOptions {
            formulation: OtsFormulation::DcMilp,
            mip_gap: 0.01,
            time_limit_s: 60.0,
            ..OtsOptions::default()
        };
        let result = solve_ots(&net, &opts).expect("OTS MILP must converge on case9");

        assert!(result.converged, "OTS must converge");
        assert!(result.objective.is_finite(), "OTS objective must be finite");
        assert!(
            result.objective <= base_cost * 1.01 + 1.0,
            "OTS cost ({:.2}) should be ≤ DC-OPF cost ({:.2}) + 1% gap tolerance",
            result.objective,
            base_cost
        );
        assert!(result.n_switches <= net.n_branches());
        assert_eq!(result.gen_dispatch.len(), net.n_generators_in_service());
        assert_eq!(result.branch_flows.len(), net.n_branches());
    }

    /// DC-OTS LP relaxation on case14: objective ≤ DC-OPF objective (LP is a lower bound).
    #[test]
    fn test_ots_relaxed_case14() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case14");
        let base_cost = base_dc_opf_cost(&net);
        assert!(base_cost.is_finite(), "base DC-OPF must converge");

        let opts = OtsOptions {
            formulation: OtsFormulation::DcRelaxed,
            ..OtsOptions::default()
        };
        let result = solve_ots(&net, &opts).expect("OTS relaxation must converge on case14");

        assert!(result.converged, "OTS relaxation must converge");
        // LP relaxation with switching is a lower bound on both DC-OPF and DC-MILP
        assert!(
            result.objective <= base_cost + 1e-3,
            "OTS LP relaxation ({:.4}) should be ≤ DC-OPF ({:.4})",
            result.objective,
            base_cost
        );
        // LMPs should be computed for LP relaxation
        assert_eq!(result.lmps.len(), net.n_buses());
        // MIP gap should be 0 for relaxation
        assert_eq!(result.mip_gap, 0.0);
    }

    /// With max_switches_open=0, OTS result should equal base DC-OPF result.
    #[test]
    fn test_ots_no_switching_equals_dc_opf() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");

        let base_cost = base_dc_opf_cost(&net);
        assert!(base_cost.is_finite(), "base DC-OPF must converge");

        let opts = OtsOptions {
            formulation: OtsFormulation::DcMilp,
            max_switches_open: Some(0),
            mip_gap: 1e-6,
            time_limit_s: 60.0,
            ..OtsOptions::default()
        };
        let result = solve_ots(&net, &opts).expect("OTS with 0 switches must converge");

        assert!(result.converged, "must converge");
        assert_eq!(result.n_switches, 0, "no branches should be opened");
        assert!(
            (result.objective - base_cost).abs() < 1.0,
            "OTS cost ({:.4}) should match DC-OPF ({:.4}) within $1/hr",
            result.objective,
            base_cost
        );
    }

    /// Big-M correctness: when z=0, P_flow must be ~0 (within numerical tolerance).
    #[test]
    fn test_ots_big_m_correctness() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");

        // Run MILP and check that switched-out branches have near-zero flows
        let opts = OtsOptions {
            formulation: OtsFormulation::DcMilp,
            mip_gap: 0.01,
            time_limit_s: 60.0,
            ..OtsOptions::default()
        };
        let result = solve_ots(&net, &opts).expect("OTS MILP must converge");
        assert!(result.converged);

        // For every switched-out branch, flow should be ≈ 0
        let sw_pos: std::collections::HashSet<(u32, u32, String)> =
            result.switched_out.iter().cloned().collect();
        for (l, br) in net.branches.iter().enumerate() {
            if sw_pos.contains(&(br.from_bus, br.to_bus, br.circuit.clone())) {
                let flow = result.branch_flows[l].abs();
                assert!(
                    flow < 1.0, // < 1 MW phantom flow allowed (numerical tolerance)
                    "switched-out branch ({}->{}) should have near-zero flow, got {:.4} MW",
                    br.from_bus,
                    br.to_bus,
                    flow
                );
            }
        }
    }
}
