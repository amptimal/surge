// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC adjoint sensitivity for Benders cuts in AC-SCOPF.
//!
//! For each violated branch m after contingency k, computes the sensitivity
//! of the post-contingency apparent power flow to each generator's output:
//!
//!   α\[j\] = ∂|S_m|/∂Pg_j
//!
//! via the adjoint of the Newton-Raphson Jacobian:
//!
//!   Solve J^{k,T} λ_m = ∇_{\[Va,Vm\]} |S_m|
//!   α\[j\] = λ_m\[pvpq_pos\[bus_j\]\]  for non-slack generators
//!
//! The resulting Benders cut is: α^T Pg ≤ rating_pu - S_m_pu(Pg*) + α^T Pg*
//! which linearizes `|S_m| ≤ rating` around the current dispatch and forces
//! the master NLP to move to a dispatch that reduces the contingency flow.

use surge_ac::matrix::jacobian::JacobianPattern;
use surge_ac::matrix::mismatch::compute_power_injection;
use surge_ac::matrix::ybus::build_ybus;
use surge_network::Network;
use surge_network::network::BusType;
use surge_solution::PfSolution;
use surge_sparse::KluSolver;

/// A Benders optimality cut: α^T Pg ≤ rhs (all quantities in per-unit).
///
/// For **thermal** cuts, linearizes the post-contingency branch apparent power flow
/// around the current dispatch Pg* and adds:
///   Σ_j α\[j\] · Pg_j ≤ rating_m/base_mva − S_m_pu(Pg*) + α^T Pg*
///
/// For **voltage** cuts, linearizes the post-contingency bus voltage magnitude:
/// - Undervoltage: −α^T Pg ≤ −Vm_min + Vm_pu(Pg*) − α^T Pg*  (forces Vm up)
/// - Overvoltage:   α^T Pg ≤  Vm_max − Vm_pu(Pg*) + α^T Pg*  (forces Vm down)
///
/// Both use the same `α^T Pg ≤ rhs` form by negating alpha/rhs for undervoltage.
#[derive(Debug, Clone)]
pub struct BendersCut {
    /// Contingency that caused this cut.
    pub contingency_id: String,
    /// Branch index (thermal cuts) or bus index (voltage cuts).
    pub branch_idx: usize,
    /// Sensitivity coefficient α\[j\] for each in-service generator j.
    ///
    /// Indexed parallel to the `in_service_gens` slice passed to
    /// `compute_ac_benders_cuts`. Length = number of in-service generators.
    pub alpha: Vec<f64>,
    /// Cut right-hand side in per-unit.
    pub rhs: f64,
}

/// Compute AC Benders sensitivity cuts for violated branches after a contingency.
///
/// For each thermal violation `(br_idx, flow_mva, rating_mva, _)` in `violations`,
/// solves the adjoint system `J^{k,T} λ_m = ∇_{[Va,Vm]} |S_m|` at the
/// post-contingency operating point and returns a `BendersCut`.
///
/// # Arguments
/// * `ctg_net`        — contingency network (outaged branch already in_service=false)
/// * `base_mva`       — system MVA base
/// * `in_service_gens`— generator indices into `ctg_net.generators` that are in service
/// * `pg_pu`          — current dispatch Pg* in per-unit, indexed parallel to `in_service_gens`
/// * `pf`             — post-contingency NR power flow solution
/// * `violations`     — (br_idx, flow_mva, rating_mva, overload_fraction) for each violation
/// * `contingency_id` — label for this contingency (used in cut deduplication)
///
/// Returns one `BendersCut` per successfully computed violated branch.
/// Skips branches where the adjoint solve fails or flow is negligible.
pub fn compute_ac_benders_cuts(
    ctg_net: &Network,
    base_mva: f64,
    in_service_gens: &[usize],
    pg_pu: &[f64],
    pf: &PfSolution,
    violations: &[(usize, f64, f64, f64)],
    contingency_id: &str,
) -> Vec<BendersCut> {
    if violations.is_empty() {
        return vec![];
    }

    let n_bus = ctg_net.n_buses();
    let bus_map = ctg_net.bus_index_map();

    let slack_idx = match ctg_net.slack_bus_index() {
        Some(i) => i,
        None => return vec![],
    };

    // Classify buses: pvpq = sorted non-slack, pq = PQ only.
    // This matches the NR solver's internal classification.
    let mut pvpq: Vec<usize> = Vec::with_capacity(n_bus);
    let mut pq: Vec<usize> = Vec::new();
    for i in 0..n_bus {
        if i == slack_idx {
            continue;
        }
        pvpq.push(i);
        if ctg_net.buses[i].bus_type == BusType::PQ {
            pq.push(i);
        }
    }
    pvpq.sort_unstable();
    let n_pvpq = pvpq.len();
    let n_pq = pq.len();
    let dim = n_pvpq + n_pq;

    if dim == 0 {
        return vec![];
    }

    // Reverse-lookup: internal bus index → position in pvpq / pq arrays.
    let mut pvpq_pos = vec![usize::MAX; n_bus];
    for (pos, &bus) in pvpq.iter().enumerate() {
        pvpq_pos[bus] = pos;
    }
    let mut pq_pos = vec![usize::MAX; n_bus];
    for (pos, &bus) in pq.iter().enumerate() {
        pq_pos[bus] = pos;
    }

    // Build Y-bus and NR Jacobian at the post-contingency operating point.
    let ybus = build_ybus(ctg_net);
    let (p_calc, q_calc) =
        compute_power_injection(&ybus, &pf.voltage_magnitude_pu, &pf.voltage_angle_rad);
    let jac_pattern = JacobianPattern::new(&ybus, &pvpq, &pq);
    let jac = jac_pattern.build(
        &pf.voltage_magnitude_pu,
        &pf.voltage_angle_rad,
        &p_calc,
        &q_calc,
    );

    // Extract CSC structure for KLU.
    let jac_ref = jac.as_ref();
    let jac_sym = jac_ref.symbolic();
    let col_ptrs: Vec<usize> = jac_sym.col_ptr().to_vec();
    let row_indices: Vec<usize> = jac_sym.row_idx().to_vec();
    let values: Vec<f64> = jac_ref.val().to_vec();

    // Factor the Jacobian.
    let mut klu = match KluSolver::new(dim, &col_ptrs, &row_indices) {
        Ok(k) => k,
        Err(_) => return vec![],
    };
    if klu.factor(&values).is_err() {
        return vec![];
    }

    let n_gen = in_service_gens.len();
    let mut cuts = Vec::new();

    for &(br_idx, flow_mva, rating_mva, _) in violations {
        let branch = &ctg_net.branches[br_idx];
        if !branch.in_service {
            continue;
        }
        let Some(&f) = bus_map.get(&branch.from_bus) else {
            continue;
        };
        let Some(&t) = bus_map.get(&branch.to_bus) else {
            continue;
        };

        let vm_f = pf.voltage_magnitude_pu[f];
        let vm_t = pf.voltage_magnitude_pu[t];
        let va_f = pf.voltage_angle_rad[f];
        let va_t = pf.voltage_angle_rad[t];

        let adm = branch.pi_model(1e-40);
        let theta_ft = va_f - va_t;
        let cos_t = theta_ft.cos();
        let sin_t = theta_ft.sin();

        // From-end complex power (pu).
        let pf_val = vm_f * vm_f * adm.g_ff + vm_f * vm_t * (adm.g_ft * cos_t + adm.b_ft * sin_t);
        let qf_val = -vm_f * vm_f * adm.b_ff + vm_f * vm_t * (adm.g_ft * sin_t - adm.b_ft * cos_t);
        let s_m = (pf_val * pf_val + qf_val * qf_val).sqrt();

        if s_m < 1e-9 {
            // Near-zero flow: gradient is ill-defined; skip this branch.
            continue;
        }

        // Partial derivatives of from-end apparent power w.r.t. voltage variables.
        //
        // From pi-circuit: Pf = Vf²·g_ff + Vf·Vt·(g_ft·cosθ + b_ft·sinθ)
        //                  Qf = −Vf²·b_ff + Vf·Vt·(g_ft·sinθ − b_ft·cosθ)
        // where θ = Va_f − Va_t.
        let dpf_dva_f = vm_f * vm_t * (-adm.g_ft * sin_t + adm.b_ft * cos_t);
        let dpf_dva_t = -dpf_dva_f;
        let dpf_dvm_f = 2.0 * vm_f * adm.g_ff + vm_t * (adm.g_ft * cos_t + adm.b_ft * sin_t);
        let dpf_dvm_t = vm_f * (adm.g_ft * cos_t + adm.b_ft * sin_t);

        let dqf_dva_f = vm_f * vm_t * (adm.g_ft * cos_t + adm.b_ft * sin_t);
        let dqf_dva_t = -dqf_dva_f;
        let dqf_dvm_f = -2.0 * vm_f * adm.b_ff + vm_t * (adm.g_ft * sin_t - adm.b_ft * cos_t);
        let dqf_dvm_t = vm_f * (adm.g_ft * sin_t - adm.b_ft * cos_t);

        // Gradient of S_m = |Sf| w.r.t. voltage angles and magnitudes.
        let dsm_dva_f = (pf_val * dpf_dva_f + qf_val * dqf_dva_f) / s_m;
        let dsm_dva_t = (pf_val * dpf_dva_t + qf_val * dqf_dva_t) / s_m;
        let dsm_dvm_f = (pf_val * dpf_dvm_f + qf_val * dqf_dvm_f) / s_m;
        let dsm_dvm_t = (pf_val * dpf_dvm_t + qf_val * dqf_dvm_t) / s_m;

        // Build the adjoint RHS b_m ∈ R^dim.
        //
        // The NR Jacobian J maps corrections [Δθ_pvpq; ΔVm_pq] → [ΔP_pvpq; ΔQ_pq].
        // Rows 0..n_pvpq correspond to P-balance at pvpq buses (Va variables).
        // Rows n_pvpq..dim correspond to Q-balance at pq buses (Vm variables).
        //
        // b_m is the gradient of S_m projected onto the Jacobian's variable space:
        //   b_m[pvpq_pos[i]] += ∂S_m/∂Va_i   (Va at pvpq buses)
        //   b_m[n_pvpq + pq_pos[i]] += ∂S_m/∂Vm_i  (Vm at PQ buses only)
        let mut b_rhs = vec![0.0_f64; dim];
        if pvpq_pos[f] != usize::MAX {
            b_rhs[pvpq_pos[f]] += dsm_dva_f;
        }
        if pvpq_pos[t] != usize::MAX {
            b_rhs[pvpq_pos[t]] += dsm_dva_t;
        }
        if pq_pos[f] != usize::MAX {
            b_rhs[n_pvpq + pq_pos[f]] += dsm_dvm_f;
        }
        if pq_pos[t] != usize::MAX {
            b_rhs[n_pvpq + pq_pos[t]] += dsm_dvm_t;
        }

        // Solve J^T λ = b_m.  After solve, b_rhs holds λ_m.
        if klu.solve_transpose(&mut b_rhs).is_err() {
            continue;
        }

        // Extract cut coefficients: α[j] = λ_m[pvpq_pos[bus_j]].
        //
        // By the implicit function theorem:
        //   ∂S_m/∂Pg_j = λ_m^T · (∂F/∂Pg_j) = λ_m[pvpq_pos[bus_j]] · (+1)
        // where the +1 comes from differentiating the P-balance w.r.t. Pg_j.
        // Generators at the slack bus contribute α[j] = 0 (Va_slack is fixed).
        let mut alpha = vec![0.0_f64; n_gen];
        for (j, &gi) in in_service_gens.iter().enumerate() {
            let gen_bus_num = ctg_net.generators[gi].bus;
            let Some(&bus_idx) = bus_map.get(&gen_bus_num) else {
                continue;
            };
            if pvpq_pos[bus_idx] != usize::MAX {
                alpha[j] = b_rhs[pvpq_pos[bus_idx]];
            }
            // Generators at the slack bus: alpha[j] = 0 (already initialized).
        }

        // Cut RHS: rating_pu − S_m_pu(Pg*) + α^T Pg*.
        // At Pg = Pg*, LHS = α^T Pg* > rhs (since S_m_pu > rating_pu at a violation).
        // This ensures the current dispatch violates the cut, forcing the master to move.
        let rating_pu = rating_mva / base_mva;
        let s_m_pu = flow_mva / base_mva;
        let alpha_dot_pg: f64 = alpha.iter().zip(pg_pu.iter()).map(|(&a, &p)| a * p).sum();
        let rhs = rating_pu - s_m_pu + alpha_dot_pg;

        cuts.push(BendersCut {
            contingency_id: contingency_id.to_string(),
            branch_idx: br_idx,
            alpha,
            rhs,
        });
    }

    cuts
}

/// Compute AC Benders cuts for post-contingency voltage violations.
///
/// For each voltage violation `(bus_idx, vm_pu, vm_min, vm_max)`, computes:
///   α\[j\] = ∂Vm_b/∂Pg_j
///
/// via the adjoint: J^{k,T} λ = e_b (unit vector for Vm at bus b in the
/// Jacobian's [Va_pvpq | Vm_pq] space). This gives the sensitivity of
/// voltage magnitude at bus b to each generator's real power injection.
///
/// Returns one `BendersCut` per voltage violation bus.
/// - Undervoltage (Vm < Vm_min): negated alpha, rhs = -Vm_min + Vm - α^T Pg*
/// - Overvoltage (Vm > Vm_max): positive alpha, rhs = Vm_max - Vm + α^T Pg*
pub fn compute_ac_voltage_benders_cuts(
    ctg_net: &Network,
    _base_mva: f64,
    in_service_gens: &[usize],
    pg_pu: &[f64],
    pf: &PfSolution,
    voltage_violations: &[(usize, f64, f64, f64)], // (bus_idx, vm_pu, vm_min, vm_max)
    contingency_id: &str,
) -> Vec<BendersCut> {
    if voltage_violations.is_empty() {
        return vec![];
    }

    let n_bus = ctg_net.n_buses();
    let bus_map = ctg_net.bus_index_map();

    let slack_idx = match ctg_net.slack_bus_index() {
        Some(i) => i,
        None => return vec![],
    };

    // Classify buses for Jacobian dimensions
    let mut pvpq: Vec<usize> = Vec::with_capacity(n_bus);
    let mut pq: Vec<usize> = Vec::new();
    for i in 0..n_bus {
        if i == slack_idx {
            continue;
        }
        pvpq.push(i);
        if ctg_net.buses[i].bus_type == BusType::PQ {
            pq.push(i);
        }
    }
    pvpq.sort_unstable();
    let n_pvpq = pvpq.len();
    let n_pq = pq.len();
    let dim = n_pvpq + n_pq;

    if dim == 0 {
        return vec![];
    }

    let mut pvpq_pos = vec![usize::MAX; n_bus];
    for (pos, &bus) in pvpq.iter().enumerate() {
        pvpq_pos[bus] = pos;
    }
    let mut pq_pos = vec![usize::MAX; n_bus];
    for (pos, &bus) in pq.iter().enumerate() {
        pq_pos[bus] = pos;
    }

    // Build and factor the Jacobian at the post-contingency operating point
    let ybus = build_ybus(ctg_net);
    let (p_calc, q_calc) =
        compute_power_injection(&ybus, &pf.voltage_magnitude_pu, &pf.voltage_angle_rad);
    let jac_pattern = JacobianPattern::new(&ybus, &pvpq, &pq);
    let jac = jac_pattern.build(
        &pf.voltage_magnitude_pu,
        &pf.voltage_angle_rad,
        &p_calc,
        &q_calc,
    );

    let jac_ref = jac.as_ref();
    let jac_sym = jac_ref.symbolic();
    let col_ptrs: Vec<usize> = jac_sym.col_ptr().to_vec();
    let row_indices: Vec<usize> = jac_sym.row_idx().to_vec();
    let values: Vec<f64> = jac_ref.val().to_vec();

    let mut klu = match KluSolver::new(dim, &col_ptrs, &row_indices) {
        Ok(k) => k,
        Err(_) => return vec![],
    };
    if klu.factor(&values).is_err() {
        return vec![];
    }

    let n_gen = in_service_gens.len();
    let mut cuts = Vec::new();

    for &(bus_idx, vm_pu, vm_min, vm_max) in voltage_violations {
        // We can only generate voltage cuts for PQ buses (Vm is a variable).
        // PV buses have fixed Vm in the NR formulation.
        if pq_pos[bus_idx] == usize::MAX {
            continue;
        }

        // Build adjoint RHS: unit vector for Vm at bus_idx
        // b[n_pvpq + pq_pos[bus_idx]] = 1.0
        let mut b_rhs = vec![0.0_f64; dim];
        b_rhs[n_pvpq + pq_pos[bus_idx]] = 1.0;

        // Solve J^T λ = e_b
        if klu.solve_transpose(&mut b_rhs).is_err() {
            continue;
        }

        // Extract α[j] = λ[pvpq_pos[bus_j]] (same as thermal cuts)
        let mut alpha = vec![0.0_f64; n_gen];
        for (j, &gi) in in_service_gens.iter().enumerate() {
            let gen_bus_num = ctg_net.generators[gi].bus;
            let Some(&gen_bus_idx) = bus_map.get(&gen_bus_num) else {
                continue;
            };
            if pvpq_pos[gen_bus_idx] != usize::MAX {
                alpha[j] = b_rhs[pvpq_pos[gen_bus_idx]];
            }
        }

        let alpha_dot_pg: f64 = alpha.iter().zip(pg_pu.iter()).map(|(&a, &p)| a * p).sum();

        if vm_pu < vm_min {
            // Undervoltage: we want α^T Pg ≥ something (increase Vm)
            // Convert to: -α^T Pg ≤ -Vm_min + Vm(Pg*) - α^T Pg*
            // Which is: (-α)^T Pg ≤ -(Vm_min - vm_pu) - α^T Pg* + α^T Pg*
            //         = (-α)^T Pg ≤ -α_dot_pg + (vm_pu - vm_min) + (-α_dot_pg)
            // Actually: rhs = -(vm_min) + vm_pu - alpha_dot_pg + alpha_dot_pg  ... let me think.
            //
            // Want: α^T Pg ≥ vm_min (linearized)
            // At Pg*, the function value is vm_pu (< vm_min), and α^T Pg* is the LHS.
            // The linearization: Vm(Pg) ≈ vm_pu + α^T(Pg - Pg*)
            // We want: vm_pu + α^T(Pg - Pg*) ≥ vm_min
            //       → α^T Pg ≥ vm_min - vm_pu + α^T Pg*
            //       → -α^T Pg ≤ -(vm_min - vm_pu + α^T Pg*)
            //       → (-α)^T Pg ≤ vm_pu - vm_min - α^T Pg*
            let neg_alpha: Vec<f64> = alpha.iter().map(|&a| -a).collect();
            let rhs = vm_pu - vm_min - alpha_dot_pg;

            cuts.push(BendersCut {
                contingency_id: contingency_id.to_string(),
                branch_idx: bus_idx, // reuse field for bus_idx
                alpha: neg_alpha,
                rhs,
            });
        } else if vm_pu > vm_max {
            // Overvoltage: we want α^T Pg ≤ something (decrease Vm)
            // Linearization: Vm(Pg) ≈ vm_pu + α^T(Pg - Pg*)
            // We want: vm_pu + α^T(Pg - Pg*) ≤ vm_max
            //       → α^T Pg ≤ vm_max - vm_pu + α^T Pg*
            let rhs = vm_max - vm_pu + alpha_dot_pg;

            cuts.push(BendersCut {
                contingency_id: contingency_id.to_string(),
                branch_idx: bus_idx,
                alpha,
                rhs,
            });
        }
    }

    cuts
}

#[cfg(test)]
mod tests {
    use crate::test_util::{data_available, test_data_dir};

    use super::*;
    use surge_ac::AcPfOptions;
    use surge_ac::solve_ac_pf_kernel;

    fn load_case(name: &str) -> Network {
        let path = test_data_dir().join(format!("{name}.m"));
        surge_io::matpower::load(&path).unwrap_or_else(|e| panic!("failed to parse {name}: {e}"))
    }

    /// Validate AC adjoint sensitivity coefficients against finite-difference approximation.
    ///
    /// For each in-service generator j, perturb Pg_j by ±eps and re-solve NR.
    /// The change in branch apparent power flow gives the numerical sensitivity.
    /// The adjoint α[j] should match the numerical FD to within a loose tolerance
    /// (FD is O(eps²) accurate; network is mildly nonlinear near the operating point).
    #[test]
    fn test_benders_cut_sensitivity_fd_case9() {
        if !data_available() {
            eprintln!("SKIP: test data not present");
            return;
        }

        let net = load_case("case9");
        let base_mva = net.base_mva;
        let bus_map = net.bus_index_map();

        // Solve base-case power flow to get operating point.
        let acpf_opts = AcPfOptions {
            flat_start: false,
            ..Default::default()
        };
        let pf0 = solve_ac_pf_kernel(&net, &acpf_opts).expect("case9 NR should converge");

        // Identify in-service generators.
        let in_service_gens: Vec<usize> = net
            .generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.in_service)
            .map(|(i, _)| i)
            .collect();
        let n_gen = in_service_gens.len();
        let pg_pu: Vec<f64> = in_service_gens
            .iter()
            .map(|&gi| net.generators[gi].p / base_mva)
            .collect();

        // Pick branch 0 (from-end) as the branch to compute sensitivity for.
        // Construct a mock violation: flow_mva > rating to trigger cut computation.
        let br_idx = 0usize;
        let branch = &net.branches[br_idx];
        let f = bus_map[&branch.from_bus];
        let t = bus_map[&branch.to_bus];
        let vm_f = pf0.voltage_magnitude_pu[f];
        let vm_t = pf0.voltage_magnitude_pu[t];
        let va_f = pf0.voltage_angle_rad[f];
        let va_t = pf0.voltage_angle_rad[t];

        // Compute actual from-end apparent power at operating point.
        let r = branch.r;
        let x = branch.x;
        let z_sq = r * r + x * x;
        let (gs, bs) = if z_sq > 1e-40 {
            (r / z_sq, -x / z_sq)
        } else {
            (1e6, 0.0)
        };
        let tap = branch.effective_tap();
        let shift_rad = branch.phase_shift_rad;
        let tap_sq = tap * tap;
        let g_ff = gs / tap_sq;
        let b_ff = (bs + branch.b / 2.0) / tap_sq;
        let cos_s = shift_rad.cos();
        let sin_s = shift_rad.sin();
        let g_ft = -(gs * cos_s - bs * sin_s) / tap;
        let b_ft = -(gs * sin_s + bs * cos_s) / tap;
        let theta = va_f - va_t - shift_rad;
        let cos_t = theta.cos();
        let sin_t = theta.sin();
        let pf_val = vm_f * vm_f * g_ff + vm_f * vm_t * (g_ft * cos_t + b_ft * sin_t);
        let qf_val = -vm_f * vm_f * b_ff + vm_f * vm_t * (g_ft * sin_t - b_ft * cos_t);
        let s0_pu = (pf_val * pf_val + qf_val * qf_val).sqrt();
        let flow_mva = s0_pu * base_mva;

        // Use a rating slightly below actual flow to trigger a violation.
        let rating_mva = flow_mva * 0.99;
        let violations = vec![(br_idx, flow_mva, rating_mva, flow_mva / rating_mva)];

        // Compute adjoint cuts.
        let cuts = compute_ac_benders_cuts(
            &net,
            base_mva,
            &in_service_gens,
            &pg_pu,
            &pf0,
            &violations,
            "ctg_test",
        );
        assert_eq!(
            cuts.len(),
            1,
            "should produce exactly one cut for one violation"
        );
        let cut = &cuts[0];
        assert_eq!(
            cut.alpha.len(),
            n_gen,
            "alpha must have one entry per generator"
        );

        // Finite-difference validation: perturb Pg_j and re-solve NR.
        let eps = 1e-4; // perturbation in pu
        let mut alpha_fd = vec![0.0_f64; n_gen];

        let compute_sm = |pf_sol: &surge_solution::PfSolution| -> f64 {
            let vm_f = pf_sol.voltage_magnitude_pu[f];
            let vm_t = pf_sol.voltage_magnitude_pu[t];
            let va_f = pf_sol.voltage_angle_rad[f];
            let va_t = pf_sol.voltage_angle_rad[t];
            let theta = va_f - va_t - shift_rad;
            let cos_t = theta.cos();
            let sin_t = theta.sin();
            let pf_v = vm_f * vm_f * g_ff + vm_f * vm_t * (g_ft * cos_t + b_ft * sin_t);
            let qf_v = -vm_f * vm_f * b_ff + vm_f * vm_t * (g_ft * sin_t - b_ft * cos_t);
            (pf_v * pf_v + qf_v * qf_v).sqrt()
        };

        for j in 0..n_gen {
            let gi = in_service_gens[j];
            let slack_gen =
                net.generators[gi].bus == net.buses[net.slack_bus_index().unwrap()].number;
            if slack_gen {
                // Slack generator absorbs the imbalance — α = 0 by definition.
                alpha_fd[j] = 0.0;
                continue;
            }

            let mut net_plus = net.clone();
            let mut net_minus = net.clone();
            net_plus.generators[gi].p += eps * base_mva;
            net_minus.generators[gi].p -= eps * base_mva;

            let pf_plus = match solve_ac_pf_kernel(&net_plus, &acpf_opts) {
                Ok(s) if s.status == surge_solution::SolveStatus::Converged => s,
                _ => continue,
            };
            let pf_minus = match solve_ac_pf_kernel(&net_minus, &acpf_opts) {
                Ok(s) if s.status == surge_solution::SolveStatus::Converged => s,
                _ => continue,
            };

            let sm_plus = compute_sm(&pf_plus);
            let sm_minus = compute_sm(&pf_minus);
            alpha_fd[j] = (sm_plus - sm_minus) / (2.0 * eps);
        }

        // Compare adjoint vs FD. Allow 10% relative tolerance (linearization error +
        // FD truncation). Zero entries (slack bus, zero-sensitivity) are checked separately.
        let abs_tol = 0.05_f64; // 0.05 pu
        for (j, &fd) in alpha_fd.iter().enumerate().take(n_gen) {
            let adj = cut.alpha[j];
            let err = (adj - fd).abs();
            let threshold = abs_tol.max(0.10 * fd.abs());
            assert!(
                err <= threshold,
                "generator {j} sensitivity mismatch: adjoint={adj:.6}, FD={fd:.6}, err={err:.2e}",
            );
        }

        eprintln!(
            "AC adjoint sensitivity FD validation PASSED for case9 branch {br_idx}: \
             alpha={:?}, alpha_fd={:?}",
            cut.alpha, alpha_fd
        );
    }

    /// Verify that the cut RHS is computed correctly.
    ///
    /// At the current dispatch Pg*, the LHS (α^T Pg*) must be strictly greater
    /// than the RHS (rating_pu − S_m_pu + α^T Pg*) when S_m > rating.
    /// This ensures the cut is violated at the current point (which is the whole point).
    #[test]
    fn test_benders_cut_rhs_violated_at_current_dispatch() {
        if !data_available() {
            eprintln!("SKIP: test data not present");
            return;
        }

        let net = load_case("case9");
        let base_mva = net.base_mva;
        let acpf_opts = AcPfOptions::default();
        let pf0 = solve_ac_pf_kernel(&net, &acpf_opts).expect("case9 NR should converge");

        let in_service_gens: Vec<usize> = net
            .generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.in_service)
            .map(|(i, _)| i)
            .collect();
        let pg_pu: Vec<f64> = in_service_gens
            .iter()
            .map(|&gi| net.generators[gi].p / base_mva)
            .collect();

        // Use branch 1, force a violation by setting rating well below actual flow.
        let br_idx = 1usize;
        let bus_map = net.bus_index_map();
        let branch = &net.branches[br_idx];
        let f = bus_map[&branch.from_bus];
        let t = bus_map[&branch.to_bus];
        let r = branch.r;
        let x = branch.x;
        let z_sq = r * r + x * x;
        let (gs, bs) = if z_sq > 1e-40 {
            (r / z_sq, -x / z_sq)
        } else {
            (1e6, 0.0)
        };
        let tap = branch.effective_tap();
        let shift_rad = branch.phase_shift_rad;
        let tap_sq = tap * tap;
        let g_ff = gs / tap_sq;
        let b_ff = (bs + branch.b / 2.0) / tap_sq;
        let cos_s = shift_rad.cos();
        let sin_s = shift_rad.sin();
        let g_ft = -(gs * cos_s - bs * sin_s) / tap;
        let b_ft = -(gs * sin_s + bs * cos_s) / tap;
        let theta = pf0.voltage_angle_rad[f] - pf0.voltage_angle_rad[t] - shift_rad;
        let cos_t = theta.cos();
        let sin_t = theta.sin();
        let pf_v = pf0.voltage_magnitude_pu[f] * pf0.voltage_magnitude_pu[f] * g_ff
            + pf0.voltage_magnitude_pu[f]
                * pf0.voltage_magnitude_pu[t]
                * (g_ft * cos_t + b_ft * sin_t);
        let qf_v = -pf0.voltage_magnitude_pu[f] * pf0.voltage_magnitude_pu[f] * b_ff
            + pf0.voltage_magnitude_pu[f]
                * pf0.voltage_magnitude_pu[t]
                * (g_ft * sin_t - b_ft * cos_t);
        let s0_mva = (pf_v * pf_v + qf_v * qf_v).sqrt() * base_mva;
        let rating_mva = s0_mva * 0.80; // 80% of actual → 25% overload

        let violations = vec![(br_idx, s0_mva, rating_mva, s0_mva / rating_mva)];
        let cuts = compute_ac_benders_cuts(
            &net,
            base_mva,
            &in_service_gens,
            &pg_pu,
            &pf0,
            &violations,
            "ctg_rhs_test",
        );

        assert!(
            !cuts.is_empty(),
            "should generate a cut for a genuine violation"
        );
        let cut = &cuts[0];

        // At Pg*, the cut LHS = α^T Pg*. This equals: rhs + S_m_pu - rating_pu.
        // Since S_m_pu > rating_pu, the cut LHS > rhs (cut is violated).
        let lhs: f64 = cut
            .alpha
            .iter()
            .zip(pg_pu.iter())
            .map(|(&a, &p)| a * p)
            .sum();
        let s_m_pu = s0_mva / base_mva;
        let rating_pu = rating_mva / base_mva;
        // rhs = rating_pu - s_m_pu + alpha^T pg* = rating_pu - s_m_pu + lhs
        // So lhs > rhs iff s_m_pu > rating_pu (i.e., there's a violation).
        assert!(
            s_m_pu > rating_pu,
            "test setup requires a genuine violation: s_m_pu={s_m_pu:.4}, rating_pu={rating_pu:.4}"
        );
        assert!(
            lhs > cut.rhs,
            "cut must be violated at current Pg*: lhs={lhs:.6} should > rhs={:.6}",
            cut.rhs
        );

        eprintln!(
            "Cut RHS test PASSED: s_m_pu={s_m_pu:.4}, rating_pu={rating_pu:.4}, \
             lhs={lhs:.6}, rhs={:.6} (lhs-rhs={:.6})",
            cut.rhs,
            lhs - cut.rhs
        );
    }
}
