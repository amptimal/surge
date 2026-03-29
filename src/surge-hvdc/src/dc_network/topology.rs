// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! True multi-terminal DC (MTDC) network power flow.
//!
//! Implements FPQ-42 / P5-023: DC bus voltages as explicit unknowns with
//! Kirchhoff's laws on the DC cable network.
//!
//! # DC Power Flow Equations
//!
//! For each non-slack DC bus k:
//!
//! ```text
//! f_k(V_dc) = sum_j( G_kj * (V_dc_k - V_dc_j) )
//!           + G_shunt_k * V_dc_k + G_ground_k * V_dc_k
//!           - P_dc_k / V_dc_k = 0
//! ```
//!
//! where:
//! - `G_kj = 1 / R_dc_kj` is the cable conductance between buses k and j
//! - `G_shunt_k` is the DC shunt conductance at bus k (filter bank ESR losses)
//! - `G_ground_k = 1/R_ground` is the ground return conductance (monopole/asymmetric bipole)
//! - `P_dc_k` is the VSC power injection at DC bus k (positive = inverter injecting power into DC network)
//! - `V_dc_k` is the DC bus voltage in per-unit
//!
//! The slack DC bus is held fixed at `V_dc = v_dc_slack` (typically 1.0 pu).
//!
//! # Newton-Raphson Formulation
//!
//! The Jacobian element is:
//! ```text
//! ∂f_k/∂V_dc_k = sum_j(G_kj) + P_dc_k / V_dc_k²
//! ∂f_k/∂V_dc_j = -G_kj   (j ≠ k)
//! ```

use tracing::{debug, info, warn};

use crate::error::HvdcError;

/// A DC branch (cable or overhead line) connecting two DC buses.
#[derive(Debug, Clone)]
pub struct DcBranch {
    /// DC bus index at the sending end (0-indexed internal DC bus).
    pub from_dc_bus: usize,
    /// DC bus index at the receiving end (0-indexed internal DC bus).
    pub to_dc_bus: usize,
    /// DC branch resistance in per-unit on system base (must be > 0).
    pub r_dc_pu: f64,
    /// DC branch current rating in per-unit (0.0 = unlimited).
    pub i_max_pu: f64,
}

/// Backward-compatible alias.
pub type DcCable = DcBranch;

impl DcBranch {
    /// Branch conductance in per-unit (1/R_dc).
    #[inline]
    pub fn conductance(&self) -> f64 {
        if self.r_dc_pu > 1e-12 {
            1.0 / self.r_dc_pu
        } else {
            f64::MAX / 2.0
        }
    }
}

/// Multi-terminal DC network with explicit DC bus voltages.
///
/// The DC network topology is defined by DC buses (indexed 0..n_dc_buses) and
/// branches connecting them.  One bus is designated as the DC slack: its voltage
/// is held fixed at `v_dc_slack` and it absorbs power imbalances.
#[derive(Debug, Clone)]
pub struct DcNetwork {
    /// DC bus voltages in per-unit.  Updated in-place by `solve_dc_pf`.
    pub v_dc: Vec<f64>,
    /// DC branches between buses.
    pub branches: Vec<DcBranch>,
    /// Index of the DC slack bus (0-indexed).  The slack voltage is fixed.
    pub slack_dc_bus: usize,
    /// DC slack bus voltage setpoint in per-unit (default 1.0).
    pub v_dc_slack: f64,
    /// Per-bus shunt conductance in per-unit (DC filter bank ESR losses).
    ///
    /// At DC steady-state only the resistive component matters (wC = 0).
    /// Shunt current: I_shunt = G_shunt x V_dc.  Loss = G_shunt x V_dc^2.
    pub g_shunt_pu: Vec<f64>,
    /// Per-bus ground return conductance in per-unit (1/R_ground).
    ///
    /// Models the earth electrode resistance for monopole HVDC or asymmetric
    /// bipole operation.  0.0 = no ground return path.
    pub g_ground_pu: Vec<f64>,
}

impl DcNetwork {
    /// Create a new DC network with `n_buses` DC buses.
    ///
    /// All DC bus voltages are initialised to `v_dc_slack` (flat start).
    /// The slack bus index is `slack_dc_bus`.
    pub fn new(n_buses: usize, slack_dc_bus: usize) -> Self {
        Self {
            v_dc: vec![1.0; n_buses],
            branches: Vec::new(),
            slack_dc_bus,
            v_dc_slack: 1.0,
            g_shunt_pu: vec![0.0; n_buses],
            g_ground_pu: vec![0.0; n_buses],
        }
    }

    /// Number of DC buses.
    pub fn n_buses(&self) -> usize {
        self.v_dc.len()
    }

    /// Number of non-slack DC buses (the unknowns in the NR system).
    pub fn n_free(&self) -> usize {
        self.n_buses().saturating_sub(1)
    }

    /// Add a branch to the network.
    pub fn add_branch(&mut self, branch: DcBranch) {
        self.branches.push(branch);
    }

    /// Add a branch to the network (backward-compatible alias).
    pub fn add_cable(&mut self, cable: DcCable) {
        self.branches.push(cable);
    }

    /// Solve the DC power flow by Newton-Raphson.
    ///
    /// `p_dc_injections[k]` is the active power injected by the VSC at DC bus k
    /// in per-unit (positive = inverter injecting power into DC network from AC side;
    /// negative = rectifier drawing power from DC network).
    ///
    /// After convergence `self.v_dc` holds the solved DC bus voltages.
    ///
    /// # Errors
    /// Returns `HvdcError::NotConverged` if the maximum iterations are reached
    /// without meeting the tolerance.
    pub fn solve_dc_pf(
        &mut self,
        p_dc_injections: &[f64],
        tol: f64,
        max_iter: usize,
    ) -> Result<DcPfResult, HvdcError> {
        let n = self.n_buses();
        if p_dc_injections.len() != n {
            return Err(HvdcError::InvalidLink(format!(
                "p_dc_injections length {} must equal n_buses {}",
                p_dc_injections.len(),
                n
            )));
        }

        if n == 0 {
            info!(
                n_buses = 0,
                "DC power flow: empty network, returning trivial solution"
            );
            return Ok(DcPfResult {
                v_dc: Vec::new(),
                branch_flows: Vec::new(),
                branch_losses: Vec::new(),
                shunt_losses: 0.0,
                ground_losses: 0.0,
                converged: true,
                iterations: 0,
            });
        }

        if self.slack_dc_bus >= n {
            return Err(HvdcError::InvalidLink(format!(
                "slack_dc_bus {} is out of range for {} DC buses",
                self.slack_dc_bus, n
            )));
        }

        info!(
            n_buses = n,
            n_cables = self.branches.len(),
            slack_dc_bus = self.slack_dc_bus,
            v_dc_slack = self.v_dc_slack,
            tol = tol,
            max_iter = max_iter,
            "DC power flow (Newton-Raphson) starting"
        );

        // Ensure slack bus voltage is set.
        self.v_dc[self.slack_dc_bus] = self.v_dc_slack;

        // Build mapping: free bus index (in NR system) → global DC bus index.
        // The slack bus is excluded.
        let free_buses: Vec<usize> = (0..n).filter(|&i| i != self.slack_dc_bus).collect();
        let n_free = free_buses.len();

        if n_free == 0 {
            // Only a slack bus — trivially converged.
            let (branch_flows, branch_losses) = self.compute_branch_flows();
            let (shunt_losses, ground_losses) = self.compute_shunt_ground_losses();
            info!(
                n_buses = n,
                "DC power flow: only slack bus present, trivially converged"
            );
            return Ok(DcPfResult {
                v_dc: self.v_dc.clone(),
                branch_flows,
                branch_losses,
                shunt_losses,
                ground_losses,
                converged: true,
                iterations: 0,
            });
        }

        // Build the DC conductance matrix (n×n, symmetric).
        // G_dc[k][j] = sum of cable conductances between k and j.
        // G_dc[k][k] = sum of all cable conductances incident to bus k.
        let g_dc = self.build_conductance_matrix(n);

        let mut converged = false;
        let mut iterations = 0;

        for _iter in 0..max_iter {
            iterations += 1;

            // Compute mismatch vector f[i] for each free bus.
            // f_k = sum_j(G_kj * (V_k - V_j)) - P_k / V_k
            //      = sum_j(G_kj) * V_k - sum_j(G_kj * V_j) - P_k / V_k
            //      = G_dc[k][k] * V_k - sum_{j≠k}(G_dc[k][j] * V_j) - P_k / V_k
            let mut f = vec![0.0; n_free];
            for (fi, &k) in free_buses.iter().enumerate() {
                let v_k = self.v_dc[k];
                let current_injection: f64 = g_dc[k]
                    .iter()
                    .zip(self.v_dc.iter())
                    .map(|(g, v)| g * v)
                    .sum();
                // current_injection = sum_j(G_kj*(V_k - V_j)) for j≠k  + 0 for j=k
                // which equals: G_kk*V_k - sum_{j≠k}(G_kj*V_j)
                // But our G_dc already handles diagonal = sum of off-diagonal conductances:
                // G_dc[k][k]*V_k + sum_{j≠k}(G_dc[k][j]*V_j)
                // where G_dc[k][j] = -g_cable  (off-diagonal is negative)
                // so current_injection = G_dc[k][k]*V_k - sum(g_cable*V_j) = net DC current leaving bus k
                let power_calc = current_injection * v_k;
                f[fi] = power_calc - p_dc_injections[k];
            }

            // Check convergence.
            let max_f = f.iter().copied().fold(0.0_f64, |a, b| a.max(b.abs()));
            debug!(
                iteration = iterations,
                max_mismatch_pu = max_f,
                tol = tol,
                "DC power flow NR iteration"
            );
            if max_f < tol {
                converged = true;
                break;
            }

            // Build Jacobian (n_free × n_free).
            // J[fi][fj] = ∂f_k/∂V_dc_m
            //
            // For diagonal (k == m):
            //   ∂f_k/∂V_dc_k = d/dV_k [ (sum_j G_kj*(V_k - V_j)) * V_k ]
            //                 = (sum_j G_kj) * V_k + sum_j G_kj * (V_k - V_j)
            //                 = G_dc[k][k] * V_k + current_injection
            //                 = 2 * G_dc[k][k] * V_k - sum_{j≠k}(G_dc[k][j]*V_j)
            //                 Simplification: = G_dc[k][k]*V_k + I_net
            //                 where I_net = G_dc[k][k]*V_k + sum_{j≠k}(G_dc[k][j]*V_j)
            //
            // Let I_k = sum_j G_dc[k][j]*V_j (net conductance-weighted sum):
            //   ∂f_k/∂V_dc_k = G_dc[k][k] * V_k + I_k
            //   (since P_k = I_k * V_k → dP/dV_k = I_k + G_dc[k][k]*V_k)
            //
            // For off-diagonal (m ≠ k):
            //   ∂f_k/∂V_dc_m = G_dc[k][m] * V_k   (= -g_cable * V_k ≤ 0)
            let mut j_mat = vec![vec![0.0; n_free]; n_free];
            for (fi, &k) in free_buses.iter().enumerate() {
                let v_k = self.v_dc[k];
                // Compute I_k = sum_j G_dc[k][j] * V_j
                let i_k: f64 = (0..n).map(|j| g_dc[k][j] * self.v_dc[j]).sum();

                // Diagonal
                j_mat[fi][fi] = g_dc[k][k] * v_k + i_k;

                // Off-diagonal
                for (fj, &m) in free_buses.iter().enumerate() {
                    if fj != fi {
                        j_mat[fi][fj] = g_dc[k][m] * v_k;
                    }
                }
            }

            // Solve J * Δv = -f using Gaussian elimination with partial pivoting.
            let delta_v = solve_system(&j_mat, &f.iter().map(|&x| -x).collect::<Vec<_>>());

            // Update free bus voltages.
            for (fi, &k) in free_buses.iter().enumerate() {
                self.v_dc[k] += delta_v[fi];
                // Clamp to avoid negative voltages.
                if self.v_dc[k] < 0.01 {
                    self.v_dc[k] = 0.01;
                }
            }
        }

        let (branch_flows, branch_losses) = self.compute_branch_flows();
        let (shunt_losses, ground_losses) = self.compute_shunt_ground_losses();

        if !converged {
            // Recompute max mismatch for error reporting and warn.
            let g_dc_err = self.build_conductance_matrix(n);
            let max_delta = free_buses
                .iter()
                .map(|&k| {
                    let v_k = self.v_dc[k];
                    let i_k: f64 = g_dc_err[k]
                        .iter()
                        .zip(self.v_dc.iter())
                        .map(|(g, v)| g * v)
                        .sum();
                    (i_k * v_k - p_dc_injections[k]).abs()
                })
                .fold(0.0_f64, f64::max);
            warn!(
                iterations = iterations,
                max_iter = max_iter,
                max_delta = max_delta,
                "DC power flow did not converge"
            );
            return Err(HvdcError::NotConverged {
                iterations: iterations as u32,
                max_delta,
            });
        }

        let total_loss_pu: f64 = branch_losses.iter().sum::<f64>() + shunt_losses + ground_losses;
        info!(
            iterations = iterations,
            n_free = n_free,
            total_dc_loss_pu = total_loss_pu,
            cable_loss_pu = branch_losses.iter().sum::<f64>(),
            shunt_loss_pu = shunt_losses,
            ground_loss_pu = ground_losses,
            converged = converged,
            "DC power flow converged"
        );

        Ok(DcPfResult {
            v_dc: self.v_dc.clone(),
            branch_flows,
            branch_losses,
            shunt_losses,
            ground_losses,
            converged,
            iterations,
        })
    }

    /// Build the DC nodal conductance matrix (n×n).
    ///
    /// Off-diagonal: G\[k\]\[j\] = -g_cable (negative, sum over parallel cables)
    /// Diagonal:     G\[k\]\[k\] = sum of cable conductances + g_shunt + g_ground
    pub fn build_conductance_matrix(&self, n: usize) -> Vec<Vec<f64>> {
        let mut g = vec![vec![0.0; n]; n];
        for cable in &self.branches {
            let k = cable.from_dc_bus;
            let j = cable.to_dc_bus;
            let g_val = cable.conductance();
            g[k][k] += g_val;
            g[j][j] += g_val;
            g[k][j] -= g_val;
            g[j][k] -= g_val;
        }
        // Add per-bus shunt and ground return conductance to diagonal.
        for (k, g_row) in g.iter_mut().enumerate() {
            if k < self.g_shunt_pu.len() {
                g_row[k] += self.g_shunt_pu[k];
            }
            if k < self.g_ground_pu.len() {
                g_row[k] += self.g_ground_pu[k];
            }
        }
        g
    }

    /// Compute DC cable power flows and I²R losses after solve.
    ///
    /// Returns `(branch_flows, branch_losses)` where each element corresponds to
    /// `self.branches` in order.
    ///
    /// `branch_flows[i]` = power flowing from `from_dc_bus` to `to_dc_bus` in pu.
    /// `branch_losses[i]` = I²R losses in pu (always non-negative).
    pub fn compute_branch_flows(&self) -> (Vec<f64>, Vec<f64>) {
        let flows: Vec<f64> = self
            .branches
            .iter()
            .map(|c| {
                let v_from = self.v_dc[c.from_dc_bus];
                let v_to = self.v_dc[c.to_dc_bus];
                let g = c.conductance();
                // Power at from-end: P_from = I * V_from = G*(V_from - V_to)*V_from
                g * (v_from - v_to) * v_from
            })
            .collect();

        let losses: Vec<f64> = self
            .branches
            .iter()
            .map(|c| {
                let v_from = self.v_dc[c.from_dc_bus];
                let v_to = self.v_dc[c.to_dc_bus];
                let g = c.conductance();
                // I²R = G*(V_from - V_to)² (since R = 1/G, I = G*(V_from-V_to))
                let i_dc = g * (v_from - v_to);
                i_dc * i_dc / g // = (V_from - V_to)² * G = I² * R
            })
            .collect();

        (flows, losses)
    }

    /// Compute total shunt and ground return losses: G × V_dc² summed over all buses.
    pub fn compute_shunt_ground_losses(&self) -> (f64, f64) {
        let mut shunt_loss = 0.0;
        let mut ground_loss = 0.0;
        for (k, &v) in self.v_dc.iter().enumerate() {
            if k < self.g_shunt_pu.len() {
                shunt_loss += self.g_shunt_pu[k] * v * v;
            }
            if k < self.g_ground_pu.len() {
                ground_loss += self.g_ground_pu[k] * v * v;
            }
        }
        (shunt_loss, ground_loss)
    }
}

/// Result of a DC power flow solve.
#[derive(Debug, Clone)]
pub struct DcPfResult {
    /// DC bus voltages in per-unit (same order as `DcNetwork::v_dc`).
    pub v_dc: Vec<f64>,
    /// Power flowing through each branch in per-unit (from `from_dc_bus` perspective).
    pub branch_flows: Vec<f64>,
    /// I²R losses in each branch in per-unit.
    pub branch_losses: Vec<f64>,
    /// Total shunt losses (G_shunt × V_dc²) summed over all buses, in per-unit.
    pub shunt_losses: f64,
    /// Total ground return losses (G_ground × V_dc²) summed over all buses, in per-unit.
    pub ground_losses: f64,
    /// True if the Newton-Raphson iteration converged.
    pub converged: bool,
    /// Number of NR iterations taken.
    pub iterations: usize,
}

impl DcPfResult {
    /// Total DC-side losses: cables + shunts + ground returns.
    pub fn total_losses(&self) -> f64 {
        let cable: f64 = self.branch_losses.iter().sum();
        cable + self.shunt_losses + self.ground_losses
    }
}

// ─── Per-unit base impedance helpers ─────────────────────────────────────────

/// Compute the DC impedance base for a single DC bus: Z_base = kV² / MVA.
pub(crate) fn dc_bus_z_base(base_mva: f64, base_kv_dc: f64) -> Result<f64, HvdcError> {
    if base_kv_dc <= 0.0 {
        return Err(HvdcError::UnsupportedConfiguration(
            "explicit DC topology requires every dc_bus.base_kv_dc to be positive".to_string(),
        ));
    }
    Ok(base_kv_dc * base_kv_dc / base_mva)
}

/// Compute the DC impedance base for a branch spanning two DC buses.
///
/// When both ends share the same base kV, uses that value directly.
/// Otherwise averages the two (typical for inter-voltage-level cables).
pub(crate) fn dc_branch_z_base(
    base_mva: f64,
    from_base_kv: f64,
    to_base_kv: f64,
) -> Result<f64, HvdcError> {
    let base_kv_dc = if (from_base_kv - to_base_kv).abs() < 1e-6 {
        from_base_kv
    } else {
        (from_base_kv + to_base_kv) / 2.0
    };
    dc_bus_z_base(base_mva, base_kv_dc)
}

// ─── Linear system solvers ───────────────────────────────────────────────────

/// Solve `A * x = b`, dispatching to KLU sparse solver for large systems.
///
/// For n ≤ 32, uses dense Gaussian elimination (lower overhead).
/// For n > 32, converts to CSC and uses KLU (O(n) vs O(n³)).
pub(crate) fn solve_system(a: &[Vec<f64>], b: &[f64]) -> Vec<f64> {
    let n = b.len();
    if n > 32 {
        solve_sparse_system(a, b)
    } else {
        solve_dense_system(a, b)
    }
}

/// Solve `A * x = b` using KLU sparse factorization.
///
/// Converts the dense row-major matrix to CSC format, then uses KLU.
/// Efficient for large DC networks (>32 buses) where most entries are zero.
fn solve_sparse_system(a: &[Vec<f64>], b: &[f64]) -> Vec<f64> {
    use surge_sparse::KluSolver;

    let n = b.len();
    if n == 0 {
        return Vec::new();
    }

    // Build CSC from dense matrix: collect (row, col, val) sorted by column.
    let mut col_ptrs = vec![0usize; n + 1];
    let mut row_indices = Vec::new();
    let mut values = Vec::new();

    for j in 0..n {
        for (i, row) in a.iter().enumerate() {
            let v = row[j];
            if v.abs() > 1e-20 {
                row_indices.push(i);
                values.push(v);
            }
        }
        col_ptrs[j + 1] = row_indices.len();
    }

    let mut klu = match KluSolver::new(n, &col_ptrs, &row_indices) {
        Ok(k) => k,
        Err(_) => return vec![0.0; n],
    };

    if klu.factor(&values).is_err() {
        return vec![0.0; n];
    }

    let mut x = b.to_vec();
    if klu.solve(&mut x).is_err() {
        return vec![0.0; n];
    }
    x
}

/// Solve `A * x = b` using Gaussian elimination with partial pivoting.
///
/// Used for the small dense DC power flow Jacobian system (n_free × n_free).
/// Sufficient for typical MTDC systems (< 32 DC buses).
pub(crate) fn solve_dense_system(a: &[Vec<f64>], b: &[f64]) -> Vec<f64> {
    let n = b.len();
    if n == 0 {
        return Vec::new();
    }

    // Augmented matrix [A | b]
    let mut aug: Vec<Vec<f64>> = a
        .iter()
        .zip(b.iter())
        .map(|(row, &bi)| {
            let mut r = row.clone();
            r.push(bi);
            r
        })
        .collect();

    // Forward elimination with partial pivoting.
    for col in 0..n {
        // Find pivot.
        let pivot_row = (col..n)
            .max_by(|&r1, &r2| {
                aug[r1][col]
                    .abs()
                    .partial_cmp(&aug[r2][col].abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(col);

        aug.swap(col, pivot_row);

        let pivot = aug[col][col];
        if pivot.abs() < 1e-14 {
            // Singular or near-singular — return zeros.
            return vec![0.0; n];
        }

        // Eliminate below.
        for row in (col + 1)..n {
            let factor = aug[row][col] / pivot;
            let col_row_vals: Vec<f64> = aug[col][col..=n].iter().map(|&v| v * factor).collect();
            for (a, val) in aug[row][col..=n].iter_mut().zip(col_row_vals.iter()) {
                *a -= val;
            }
        }
    }

    // Back substitution.
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = aug[i][n];
        for j in (i + 1)..n {
            sum -= aug[i][j] * x[j];
        }
        x[i] = sum / aug[i][i];
    }
    x
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a 2-bus DC network with one cable.
    ///
    /// Bus 0 = slack (V_dc = 1.0 pu, `v_dc_slack`).
    /// Bus 1 = free with power injection `p1_pu`.
    fn two_bus_dc_network(r_dc_pu: f64, p1_pu: f64) -> (DcNetwork, Vec<f64>) {
        let mut net = DcNetwork::new(2, 0);
        net.v_dc_slack = 1.0;
        net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu,
            i_max_pu: 10.0,
        });
        // p_dc_injections: bus 0 (slack) absorbs remainder, bus 1 injects p1_pu.
        let p_dc = vec![0.0, p1_pu];
        (net, p_dc)
    }

    // ── dc_pf_two_terminal ────────────────────────────────────────────────────

    /// 2 VSC stations, 1 cable.
    /// Bus 0: slack at 1.0 pu (rectifier, absorbs power from DC).
    /// Bus 1: inverter injecting -0.5 pu into DC (i.e. taking 0.5 pu from DC).
    ///
    /// With G = 1/R and f_1 = G*(V_1 - V_0)*V_1 - P_1 = 0
    /// → G*(V_1 - 1.0)*V_1 = P_1
    /// For R = 0.02 (G = 50), P_1 = -0.5:
    /// 50*(V_1 - 1)*V_1 = -0.5  → 50*V_1² - 50*V_1 + 0.5 = 0
    /// V_1 = (50 ± sqrt(2500 - 100)) / 100 ≈ (50 - 48.99) / 100 ≈ 0.9899
    #[test]
    fn dc_pf_two_terminal() {
        let r_dc = 0.02;
        let p1 = -0.5; // inverter draws 0.5 pu from DC network at bus 1
        let (mut net, p_dc) = two_bus_dc_network(r_dc, p1);

        let result = net
            .solve_dc_pf(&p_dc, 1e-8, 50)
            .expect("DC PF should converge");

        assert!(result.converged, "DC PF must converge");
        assert!(result.iterations > 0);

        // Verify KVL: voltage at free bus must satisfy the DC PF equation.
        let v0 = result.v_dc[0];
        let v1 = result.v_dc[1];
        let g = 1.0 / r_dc;

        // Check slack bus at specified voltage.
        assert!(
            (v0 - 1.0).abs() < 1e-9,
            "Slack bus voltage must be 1.0 pu, got {v0:.6}"
        );

        // Check DC power balance at free bus:
        // f_1 = G*(V_1 - V_0)*V_1 - P_1 ≈ 0
        let f1 = g * (v1 - v0) * v1 - p1;
        assert!(
            f1.abs() < 1e-6,
            "DC PF equation not satisfied at bus 1: residual = {f1:.3e}"
        );

        // V_1 should be slightly below 1.0 (voltage drop across cable).
        assert!(v1 < v0, "Bus 1 voltage must be below slack due to losses");
    }

    // ── dc_pf_three_terminal_mtdc ─────────────────────────────────────────────

    /// 3-terminal MTDC: 3 stations, 2 cables.
    /// Bus 0: slack (V_dc = 1.0, absorbs imbalance).
    /// Bus 1: injects +0.3 pu into DC (rectifier feeding DC network).
    /// Bus 2: draws -0.25 pu from DC (inverter feeding AC network).
    ///
    /// Power balance at slack should account for losses.
    #[test]
    fn dc_pf_three_terminal_mtdc() {
        let mut net = DcNetwork::new(3, 0);
        net.v_dc_slack = 1.0;

        // Cable 0–1: R = 0.01 pu
        net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.01,
            i_max_pu: 5.0,
        });
        // Cable 0–2: R = 0.02 pu
        net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 2,
            r_dc_pu: 0.02,
            i_max_pu: 5.0,
        });

        // Bus 0 (slack): injection determined by balance.
        // Bus 1: rectifier injects 0.3 pu into DC.
        // Bus 2: inverter draws 0.25 pu from DC.
        let p_dc = [0.0_f64, 0.3, -0.25];

        let result = net
            .solve_dc_pf(&p_dc, 1e-8, 100)
            .expect("3-terminal DC PF should converge");

        assert!(result.converged, "3-terminal DC PF must converge");

        let v_dc = &result.v_dc;

        // Verify DC PF equations at free buses (buses 1 and 2).
        let g_dc = net.build_conductance_matrix(3);
        for k in [1usize, 2usize] {
            let i_k: f64 = (0..3).map(|j| g_dc[k][j] * v_dc[j]).sum();
            let f_k = i_k * v_dc[k] - p_dc[k];
            assert!(
                f_k.abs() < 1e-5,
                "DC PF residual at bus {k} = {f_k:.3e}, expected < 1e-5"
            );
        }

        // Net power balance: sum(P_dc) + losses ≈ 0.
        // Total injected = 0.3 - 0.25 = 0.05 pu; losses should be small.
        let total_loss: f64 = result.branch_losses.iter().sum();
        assert!(
            total_loss >= 0.0,
            "Total losses must be non-negative, got {total_loss}"
        );
        assert!(
            total_loss < 0.01,
            "Losses unexpectedly large: {total_loss:.4e} pu"
        );
    }

    // ── dc_branch_losses_nonzero ───────────────────────────────────────────────

    /// Verify that I²R losses are computed correctly and are non-zero when
    /// current flows through a cable.
    #[test]
    fn dc_branch_losses_nonzero() {
        let r_dc = 0.05; // higher resistance to make losses visible
        let p1 = -0.4; // 0.4 pu drawn from DC network at bus 1
        let (mut net, p_dc) = two_bus_dc_network(r_dc, p1);

        let result = net
            .solve_dc_pf(&p_dc, 1e-8, 50)
            .expect("DC PF should converge");

        let v0 = result.v_dc[0];
        let v1 = result.v_dc[1];
        let g = 1.0 / r_dc;

        // Analytically: I_dc = G*(V_0 - V_1), P_loss = I²*R = (V_0-V_1)²*G
        let i_dc = g * (v0 - v1);
        let p_loss_analytical = i_dc * i_dc * r_dc;

        assert_eq!(result.branch_losses.len(), 1, "Exactly 1 cable");
        let p_loss_computed = result.branch_losses[0];

        assert!(
            p_loss_computed > 0.0,
            "Losses must be positive when current flows, got {p_loss_computed:.6e}"
        );
        assert!(
            (p_loss_computed - p_loss_analytical).abs() < 1e-8,
            "Computed loss {p_loss_computed:.6e} does not match analytical {p_loss_analytical:.6e}"
        );
    }

    // ── dc_pf_shunt_conductance ────────────────────────────────────────────

    /// Verify that shunt conductance at a DC bus adds to I²R losses and
    /// affects the voltage solution (bus with shunt draws current to ground).
    #[test]
    fn dc_pf_shunt_conductance() {
        let mut net = DcNetwork::new(2, 0);
        net.v_dc_slack = 1.0;
        net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.02,
            i_max_pu: 10.0,
        });
        // Add a shunt conductance at bus 1 (G_shunt = 0.5 pu).
        // This draws I_shunt = G × V ≈ 0.5 A pu from bus 1 to ground.
        net.g_shunt_pu[1] = 0.5;

        // Bus 1 has zero converter injection — only the shunt draws power.
        let p_dc = vec![0.0, 0.0];
        let result = net
            .solve_dc_pf(&p_dc, 1e-8, 50)
            .expect("DC PF with shunt should converge");

        assert!(result.converged);
        // Shunt draws current → V_1 < V_slack (voltage drop through cable).
        assert!(
            result.v_dc[1] < result.v_dc[0],
            "Bus with shunt should have lower voltage: V_1={:.6}, V_0={:.6}",
            result.v_dc[1],
            result.v_dc[0]
        );
        // Shunt losses must be positive.
        assert!(
            result.shunt_losses > 0.0,
            "Shunt losses must be positive, got {:.6e}",
            result.shunt_losses
        );
        // Total losses = cable + shunt.
        let total = result.total_losses();
        assert!(
            total > result.shunt_losses,
            "Total losses must include cable losses"
        );
    }

    // ── dc_pf_ground_return ────────────────────────────────────────────────

    /// Verify that ground return conductance affects voltage and creates losses.
    #[test]
    fn dc_pf_ground_return() {
        let mut net = DcNetwork::new(2, 0);
        net.v_dc_slack = 1.0;
        net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.02,
            i_max_pu: 10.0,
        });
        // Ground return at bus 1: G_ground = 1.0 pu (R_ground = 1.0 pu).
        net.g_ground_pu[1] = 1.0;

        let p_dc = vec![0.0, 0.0];
        let result = net
            .solve_dc_pf(&p_dc, 1e-8, 50)
            .expect("DC PF with ground return should converge");

        assert!(result.converged);
        assert!(
            result.ground_losses > 0.0,
            "Ground losses must be positive, got {:.6e}",
            result.ground_losses
        );
        assert!(
            result.v_dc[1] < 1.0,
            "Bus with ground return should have lower voltage"
        );
    }
}
