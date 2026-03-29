// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC Marginal Loss Factors via one J^T solve.
//!
//! MLF\[i\] = ∂P_loss_total / ∂P_inject_i, computed exactly at the AC operating
//! point (Va*, Vm*). Requires one Jacobian build, one KLU factorization, and
//! one J^T solve — approximately the cost of one Newton-Raphson iteration.
//!
//! The loss gradient `s_loss = ∂P_loss/∂(θ,|V|)` is derived from the full
//! pi-model with transformer tap ratio, phase shift, line charging conductance
//! (`g_pi`), and magnetizing conductance (`g_mag`), matching the Y-bus
//! assembled in `ybus.rs`.  Plain lines (a=1, φ=0, g_pi=0, g_mag=0) reduce
//! to the standard textbook formula.
//!
//! # Usage in LMP decomposition
//! ```text
//! lmp_loss[i]       = λ_energy * MLF[i]          // AC-exact
//! lmp_congestion[i] = lmp[i] - energy[i] - loss[i]  // exact by subtraction
//! ```

use surge_ac::matrix::jacobian::JacobianPattern;
use surge_ac::matrix::mismatch::compute_power_injection;
use surge_ac::matrix::ybus::build_ybus;
use surge_network::Network;
use surge_sparse::KluSolver;

/// Compute AC marginal loss factors at an AC operating point.
///
/// Returns a `Vec<f64>` of length `n_bus` where entry `i` is `MLF[i]`.
/// `MLF[slack_idx] = 0.0` by definition (reference bus).
///
/// Returns `Err` if the Jacobian is singular (e.g. multi-island HVDC network).
pub fn compute_ac_marginal_loss_factors(
    network: &Network,
    va: &[f64],
    vm: &[f64],
    slack_idx: usize,
) -> Result<Vec<f64>, String> {
    let ybus = build_ybus(network);
    let n = network.n_buses();

    // Treat all non-slack buses as PQ for full sensitivity.
    // At the OPF optimum, Vm is free at all non-slack buses so the full
    // 2(n-1) × 2(n-1) Jacobian captures all sensitivity correctly.
    let pvpq: Vec<usize> = (0..n).filter(|&i| i != slack_idx).collect();
    let pq: Vec<usize> = pvpq.clone();

    let n_pvpq = pvpq.len();

    // Power injections needed for the Jacobian diagonal terms.
    let (p_calc, q_calc) = compute_power_injection(&ybus, vm, va);

    // Build Jacobian pattern and fill numerical values.
    let pattern = JacobianPattern::new(&ybus, &pvpq, &pq);
    let jac = pattern.build(vm, va, &p_calc, &q_calc);
    let jac_ref = jac.as_ref();
    let sym = jac_ref.symbolic();
    let col_ptrs: Vec<usize> = sym.col_ptr().to_vec();
    let row_indices: Vec<usize> = sym.row_idx().to_vec();
    let values: Vec<f64> = jac_ref.val().to_vec();

    // KLU symbolic + numeric factorization.
    let dim = pattern.dim();
    let mut klu = KluSolver::new(dim, &col_ptrs, &row_indices)
        .map_err(|e| format!("AC MLF: KLU symbolic failed: {e}"))?;
    if klu.factor(&values).is_err() {
        return Err("AC MLF: KLU factorization failed (singular Jacobian)".to_string());
    }

    // Build s_loss = ∂P_loss_total / ∂(θ, |V|).
    //
    // Full pi-model with transformer tap a = |tap_c| and phase shift φ (rad):
    //
    //   P_loss_k = g_s · (Vm_f²/a² + Vm_t² − 2·Vm_f·Vm_t·cos(δ−φ)/a)
    //            + (g_pi/2) · (Vm_f²/a² + Vm_t²)
    //            + g_mag · Vm_f²
    //
    // where δ = θ_f − θ_t and g_s = r/(r²+x²).  For a plain line (a=1, φ=0,
    // g_pi=0, g_mag=0) this collapses to the standard g_s·(Vm_f²+Vm_t²−2·Vm_f·Vm_t·cos δ).
    //
    // Partial derivatives (δ̃ ≡ δ − φ):
    //   ∂P_loss_k/∂θ_f  =  2·g_s·Vm_f·Vm_t·sin(δ̃)/a
    //   ∂P_loss_k/∂θ_t  = −2·g_s·Vm_f·Vm_t·sin(δ̃)/a
    //   ∂P_loss_k/∂Vm_f =  2·(g_s+g_pi/2)·Vm_f/a² − 2·g_s·Vm_t·cos(δ̃)/a + 2·g_mag·Vm_f
    //   ∂P_loss_k/∂Vm_t =  2·(g_s+g_pi/2)·Vm_t    − 2·g_s·Vm_f·cos(δ̃)/a
    //
    // Jacobian row layout: rows 0..n_pvpq → ∂P/∂θ (pvpq buses, in pvpq order)
    //                      rows n_pvpq..dim → ∂Q/∂|V| (pq buses, same order)
    // Since pvpq == pq == all non-slack, pvpq_pos[i] = row position for bus i.

    // Pre-compute position lookup: bus internal index → pvpq position.
    // Slack bus has no entry (position = usize::MAX sentinel).
    let mut pvpq_pos = vec![usize::MAX; n];
    for (pos, &bus_idx) in pvpq.iter().enumerate() {
        pvpq_pos[bus_idx] = pos;
    }

    let bus_map = network.bus_index_map();

    let mut s_loss = vec![0.0_f64; dim];

    for branch in &network.branches {
        if !branch.in_service {
            continue;
        }

        let f = match bus_map.get(&branch.from_bus) {
            Some(&idx) => idx,
            None => continue,
        };
        let t = match bus_map.get(&branch.to_bus) {
            Some(&idx) => idx,
            None => continue,
        };

        // Series conductance g_s = r / (r² + x²), matching Y-bus assembly.
        let z_sq = branch.r * branch.r + branch.x * branch.x;
        if z_sq < 1e-20 {
            // Zero-impedance (lossless) branch — skip.
            continue;
        }
        let g_s = branch.r / z_sq;

        // Tap magnitude and phase shift — consistent with ybus.rs effective_tap().
        let a = branch.effective_tap(); // 1.0 for plain lines
        let phi = branch.phase_shift_rad; // 0.0 for plain lines
        let a_sq = a * a;

        // δ̃ = (θ_f − θ_t) − φ  (effective angle seen by the series branch)
        let delta_eff = (va[f] - va[t]) - phi;
        let cos_d = delta_eff.cos();
        let sin_d = delta_eff.sin();
        let vm_f = vm[f];
        let vm_t = vm[t];

        // g_pi/2 and g_mag from branch model (zero for plain lines).
        let g_pi_half = branch.g_pi / 2.0;
        let g_mag = branch.g_mag;

        // ∂P_loss_k/∂θ_f =  2·g_s·Vm_f·Vm_t·sin(δ̃)/a
        let d_loss_dthf = 2.0 * g_s * vm_f * vm_t * sin_d / a;
        // ∂P_loss_k/∂θ_t = −2·g_s·Vm_f·Vm_t·sin(δ̃)/a
        let d_loss_dtht = -d_loss_dthf;
        // ∂P_loss_k/∂Vm_f = 2·(g_s+g_pi/2)·Vm_f/a² − 2·g_s·Vm_t·cos(δ̃)/a + 2·g_mag·Vm_f
        let d_loss_dvmf = 2.0 * (g_s + g_pi_half) * vm_f / a_sq - 2.0 * g_s * vm_t * cos_d / a
            + 2.0 * g_mag * vm_f;
        // ∂P_loss_k/∂Vm_t = 2·(g_s+g_pi/2)·Vm_t − 2·g_s·Vm_f·cos(δ̃)/a
        let d_loss_dvmt = 2.0 * (g_s + g_pi_half) * vm_t - 2.0 * g_s * vm_f * cos_d / a;

        // Accumulate into s_loss using Jacobian row ordering.
        if pvpq_pos[f] != usize::MAX {
            s_loss[pvpq_pos[f]] += d_loss_dthf;
            s_loss[n_pvpq + pvpq_pos[f]] += d_loss_dvmf;
        }
        if pvpq_pos[t] != usize::MAX {
            s_loss[pvpq_pos[t]] += d_loss_dtht;
            s_loss[n_pvpq + pvpq_pos[t]] += d_loss_dvmt;
        }
    }

    // Solve J^T · α = s_loss (in-place; s_loss becomes α).
    if klu.solve_transpose(&mut s_loss).is_err() {
        return Err("AC MLF: J^T solve failed".to_string());
    }
    // s_loss now holds α.

    // Map α back to full bus vector. The P-block (rows 0..n_pvpq) gives
    // ∂P_loss/∂P_inject at each non-slack bus — that is the MLF.
    let mut mlf = vec![0.0_f64; n];
    for (pos, &bus_idx) in pvpq.iter().enumerate() {
        mlf[bus_idx] = s_loss[pos];
    }
    // slack_idx entry stays 0.0.

    Ok(mlf)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use surge_ac::AcPfOptions;
    use surge_ac::solve_ac_pf_kernel;
    use surge_network::Network;
    use surge_network::network::BusType;
    use surge_network::network::{Branch, Bus, Generator, Load};

    use super::*;

    /// Build a small 3-bus network with only PQ + Slack buses.
    ///
    /// Bus 1 (Slack) ──[transformer: tap=1.05, shift=5°, g_pi=0.005, g_mag=0.001]──
    ///   Bus 2 (PQ, load) ──[line: r=0.02, x=0.15, b=0.03, g_pi=0.002]── Bus 3 (PQ, load)
    ///
    /// All non-slack buses are PQ so the NR Jacobian matches the full 2(n-1)×2(n-1)
    /// Jacobian used in acmlf (which treats all buses as PQ — correct for OPF context).
    ///
    /// This exercises ALL four previously-missing loss gradient terms:
    ///   - tap ratio (a = 1.05 ≠ 1)
    ///   - phase shift (φ = 5° ≠ 0)
    ///   - g_pi (line charging conductance)
    ///   - g_mag (magnetizing conductance)
    fn make_transformer_network() -> Network {
        let mut net = Network::new("acmlf_test");
        net.base_mva = 100.0;

        net.buses = vec![
            {
                let mut b = Bus::new(1, BusType::Slack, 100.0);
                b.voltage_magnitude_pu = 1.05;
                b.voltage_angle_rad = 0.0;
                b
            },
            {
                let mut b = Bus::new(2, BusType::PQ, 100.0);
                b.voltage_magnitude_pu = 1.0;
                b.voltage_angle_rad = 0.0;
                b
            },
            {
                let mut b = Bus::new(3, BusType::PQ, 100.0);
                b.voltage_magnitude_pu = 1.0;
                b.voltage_angle_rad = 0.0;
                b
            },
        ];

        // Branch 1→2: transformer with tap, phase shift, g_pi, g_mag — the four
        // terms that were missing from the old loss gradient.
        let mut xfmr = Branch::new_line(1, 2, 0.005, 0.08, 0.0);
        xfmr.tap = 1.05;
        xfmr.phase_shift_rad = 5.0_f64.to_radians();
        xfmr.g_pi = 0.005;
        xfmr.g_mag = 0.001;

        // Branch 2→3: plain line with g_pi.
        let mut line = Branch::new_line(2, 3, 0.02, 0.15, 0.03);
        line.g_pi = 0.002;

        net.branches = vec![xfmr, line];

        // Single slack generator, loads at both PQ buses.
        let mut g1 = Generator::new(1, 200.0, 1.05);
        g1.qmin = -200.0;
        g1.qmax = 200.0;
        g1.pmax = 400.0;
        net.generators.push(g1);

        net.loads.push(Load::new(2, 80.0, 30.0));
        net.loads.push(Load::new(3, 120.0, 40.0));

        net
    }

    /// Compute total real power losses from branch flows using the full pi-model
    /// loss formula (independent implementation for FD validation).
    fn branch_losses(network: &Network, vm: &[f64], va: &[f64]) -> f64 {
        let bus_map = network.bus_index_map();
        let mut total = 0.0;
        for branch in &network.branches {
            if !branch.in_service {
                continue;
            }
            let f = bus_map[&branch.from_bus];
            let t = bus_map[&branch.to_bus];
            let z_sq = branch.r * branch.r + branch.x * branch.x;
            if z_sq < 1e-20 {
                continue;
            }
            let g_s = branch.r / z_sq;
            let a = branch.effective_tap();
            let phi = branch.phase_shift_rad;
            let delta_eff = (va[f] - va[t]) - phi;
            let vmf = vm[f];
            let vmt = vm[t];
            // P_loss = g_s·(Vmf²/a² + Vmt² − 2·Vmf·Vmt·cos(δ−φ)/a)
            //        + (g_pi/2)·(Vmf²/a² + Vmt²)
            //        + g_mag·Vmf²
            total += g_s
                * (vmf * vmf / (a * a) + vmt * vmt - 2.0 * vmf * vmt * delta_eff.cos() / a)
                + (branch.g_pi / 2.0) * (vmf * vmf / (a * a) + vmt * vmt)
                + branch.g_mag * vmf * vmf;
        }
        total
    }

    /// Finite-difference validation of MLF gradient against branch-loss FD on
    /// a network with transformer tap, phase shift, g_pi, and g_mag — the four
    /// terms previously missing from the loss gradient.
    ///
    /// For each non-slack bus, perturbs load by ±ε, re-solves power flow,
    /// measures total branch losses, and checks FD ≈ analytic MLF to 1e-3.
    #[test]
    fn test_acmlf_fd_transformer_network() {
        let net = make_transformer_network();
        let acpf_opts = AcPfOptions {
            enforce_q_limits: false,
            ..AcPfOptions::default()
        };
        let sol_base = solve_ac_pf_kernel(&net, &acpf_opts).expect("base NR must converge");

        let bus_map = net.bus_index_map();
        let slack_idx = net
            .buses
            .iter()
            .position(|b| b.bus_type == BusType::Slack)
            .unwrap();

        let mlf = compute_ac_marginal_loss_factors(
            &net,
            &sol_base.voltage_angle_rad,
            &sol_base.voltage_magnitude_pu,
            slack_idx,
        )
        .expect("MLF must succeed");

        // eps in MW (loads stored in MW, base_mva=100 → 0.01 pu perturbation).
        // Large enough to dominate NR tolerance (1e-8 pu) but small enough for
        // linearity. FD sign: perturbing load +eps reduces injection by eps/base_mva,
        // so FD_loss = -MLF * (eps/base_mva), i.e. FD = -MLF after normalising.
        let eps = 1.0_f64; // 1 MW = 0.01 pu on 100 MVA base

        // Check all non-slack PQ buses.
        for (idx, bus) in net.buses.iter().enumerate() {
            if idx == slack_idx || bus.bus_type != BusType::PQ {
                continue;
            }

            // Perturb load Pd at this bus by ±eps MW.
            // All PQ buses in this test have loads, but guard anyway.
            let mut net_plus = net.clone();
            let mut net_minus = net.clone();
            for l in &mut net_plus.loads {
                if bus_map[&l.bus] == idx {
                    l.active_power_demand_mw += eps;
                }
            }
            for l in &mut net_minus.loads {
                if bus_map[&l.bus] == idx {
                    l.active_power_demand_mw -= eps;
                }
            }

            let sol_plus =
                solve_ac_pf_kernel(&net_plus, &acpf_opts).expect("perturbed+ NR must converge");
            let sol_minus =
                solve_ac_pf_kernel(&net_minus, &acpf_opts).expect("perturbed− NR must converge");

            let loss_plus = branch_losses(
                &net,
                &sol_plus.voltage_magnitude_pu,
                &sol_plus.voltage_angle_rad,
            );
            let loss_minus = branch_losses(
                &net,
                &sol_minus.voltage_magnitude_pu,
                &sol_minus.voltage_angle_rad,
            );

            // FD of losses w.r.t. load increase = -FD w.r.t. injection increase.
            // Normalise eps to per-unit: eps_pu = eps / base_mva.
            let eps_pu = eps / net.base_mva;
            let fd_mlf = -(loss_plus - loss_minus) / (2.0 * eps_pu);

            let err = (fd_mlf - mlf[idx]).abs();
            assert!(
                err < 1e-3,
                "bus {} (idx {}): FD MLF = {fd_mlf:.6}, analytic MLF = {:.6}, err = {err:.2e}",
                bus.number,
                idx,
                mlf[idx],
            );
        }
    }
}
