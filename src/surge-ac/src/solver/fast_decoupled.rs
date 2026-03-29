// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Fast Decoupled Power Flow (FDPF) solver — XB and BX variants.
//!
//! # Decoupled Jacobian approximation (AC-01)
//!
//! FDPF intentionally uses a **decoupled Jacobian** for speed.  The cross-coupling
//! terms dP/dV and dQ/dtheta are dropped from the full Newton-Raphson Jacobian,
//! yielding two smaller independent sub-problems (P-theta and Q-V) that can be
//! solved with constant pre-factored matrices B' and B''.  This is the Stott &
//! Alsac (1974) formulation used by every major power system tool (MATPOWER,
//! PSS/E, PowerWorld).  For precise power flow with quadratic convergence and
//! full Jacobian coupling, use the Newton-Raphson solver in [`newton_raphson`].
//!
//! # B' matrix: resistance ignored (AC-02)
//!
//! In the XB variant (default), B' is built using `1/x` per branch, ignoring
//! resistance entirely.  This is the standard Stott & Alsac XB formulation and
//! is accurate for high-voltage transmission networks where R/X << 1 (typically
//! 0.05-0.15 for 230+ kV lines).  For **distribution networks** with high R/X
//! ratios (R/X > 0.5, common at 12.47 kV and below), the BX variant provides
//! better convergence, or use the full Newton-Raphson solver which retains R in
//! the Jacobian.
//!
//! # Voltage clamping (AC-03)
//!
//! During iteration, voltage magnitudes are clamped to [0.5, 2.0] pu as a
//! **numerical safeguard**.  This does not affect converged solutions — at
//! convergence, all voltages are within normal operating range (typically
//! 0.9-1.1 pu) and the clamp is never active.  The clamp prevents numerical
//! divergence during early iterations when voltages may temporarily swing to
//! extreme values, especially on ill-conditioned networks or from cold starts.
//!
//! # When to use FDPF vs. Newton-Raphson
//!
//! - **FDPF**: Ideal for contingency screening, real-time monitoring, and any
//!   application needing thousands of fast approximate solutions.  Each iteration
//!   is ~10x cheaper than NR (triangular solve vs. Jacobian assembly + LU).
//! - **Newton-Raphson**: Required when quadratic convergence is needed, for
//!   ill-conditioned networks, or when the full coupled Jacobian matters (high
//!   R/X systems, heavy reactive compensation, voltage-critical studies).
//!
//! Exploits the weak coupling between P-theta and Q-V in high-voltage
//! transmission networks. B' and B'' are factored once, then each FDPF
//! "half-iteration" is just a triangular solve (~10x cheaper than NR
//! iteration which requires Jacobian assembly + LU factorization).
//!
//! FDPF-XB (MATPOWER default):
//! - **B'**: Built from branch reactances only (ignoring resistance, taps, shifts).
//!   Rows/columns for slack bus removed. Used for P-theta sub-problem.
//! - **B''**: Built from full branch admittance (including taps, charging).
//!   Rows/columns for slack + PV buses removed. Used for Q-V sub-problem.
//!
//! Algorithm per iteration:
//! 1. Compute P mismatches: dP/V for non-slack buses
//! 2. Solve B' * d_theta = dP/V -> update theta
//! 3. Compute Q mismatches: dQ/V for PQ buses only
//! 4. Solve B'' * dV = dQ/V -> update V
//!
//! Convergence: Linear (5-15 iterations typical), but each iteration is
//! ~10x cheaper than NR. Ideal for contingency screening where we need
//! approximate voltages to check violations, not machine-precision solutions.
//!
//! # References
//!
//! - B. Stott & O. Alsac, "Fast Decoupled Load Flow," IEEE Trans. PAS, 1974.
//! - MATPOWER manual, Section 4.3 (FDPF formulation).

use std::collections::HashMap;

use tracing::{debug, trace, warn};

use crate::matrix::ybus::YBus;
use crate::topology::islands::detect_islands;
use surge_network::Network;
use surge_network::network::BusType;
use surge_sparse::KluSolver;

/// FDPF variant controlling how B' and B'' are built.
///
/// - **XB** (default, MATPOWER default): B' uses only reactance (ignoring R and taps);
///   B'' uses full branch admittance (taps + charging).
///   Best for high-voltage transmission networks with low R/X.
///
/// - **BX**: B' uses full imaginary part of Y-bus (same as XB for B');
///   B'' uses only reactance (no R, no charging): B''_ij = -1/X_ij.
///   Converges better on distribution feeders with high R/X ratios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FdpfVariant {
    /// XB variant — MATPOWER default, best for high-voltage transmission.
    #[default]
    Xb,
    /// BX variant — better convergence for high R/X distribution networks.
    Bx,
}

/// Result of a single FDPF solve.
#[derive(Debug, Clone)]
pub struct FdpfResult {
    /// Converged voltage magnitudes (p.u.).
    pub vm: Vec<f64>,
    /// Converged voltage angles (radians).
    pub va: Vec<f64>,
    /// Number of half-iterations performed.
    pub iterations: u32,
    /// Maximum power mismatch at convergence (p.u.).
    pub max_mismatch: f64,
}

/// Options for FDPF solve (both low-level `FdpfFactors` and top-level `solve_fdpf`).
#[derive(Debug, Clone)]
pub struct FdpfOptions {
    /// Which FDPF variant to use for matrix construction.
    pub variant: FdpfVariant,
    /// Convergence tolerance (p.u. mismatch).
    /// Default: 1e-6 (relaxed vs NR — FDPF converges linearly, tighter tolerances
    /// cost disproportionate iterations).
    pub tolerance: f64,
    /// Maximum number of half-iterations. Default: 100.
    pub max_iterations: u32,
    /// Use flat start (Vm=1.0, Va=0.0). If false (default), use case data
    /// voltages with generator setpoints applied to PV/Slack buses.
    pub flat_start: bool,
    /// Enforce generator reactive power limits (PV→PQ bus switching).
    ///
    /// When `true` (default), after FDPF converges the solver checks all PV
    /// buses against their generator qmin/qmax. The worst violator is switched
    /// to PQ, B'' is refactored, and FDPF re-runs from the current voltages.
    /// Repeats until no violations remain or the switch budget is exhausted.
    pub enforce_q_limits: bool,
    /// Automatically reduce node-breaker topology before solving.
    ///
    /// When `true` and `network.topology` is `Some`, calls
    /// `surge_topology::rebuild_topology()` before the solve. No-op otherwise.
    pub auto_reduce_topology: bool,
}

impl Default for FdpfOptions {
    fn default() -> Self {
        Self {
            variant: FdpfVariant::Xb,
            tolerance: 1e-6,
            max_iterations: 100,
            flat_start: false,
            enforce_q_limits: true,
            auto_reduce_topology: false,
        }
    }
}

/// Pre-factored B' and B'' matrices for FDPF.
///
/// Build once from the base case network, then call `solve()` or
/// `solve_from_ybus()` per contingency. The factorization is cheap to
/// share across threads (each thread needs its own `KluSolver` due to
/// mutable solve state, but the CSC structure and factored values are
/// produced once).
pub struct FdpfFactors {
    /// KLU solver for B' (P-θ, dimension = n_pvpq)
    b_prime_klu: KluSolver,
    /// KLU solver for B'' (Q-V, dimension = n_pq)
    b_double_prime_klu: KluSolver,

    /// Number of buses
    n: usize,
    /// PVPQ bus indices (sorted) — have θ as unknowns
    pvpq_indices: Vec<usize>,
    /// PQ bus indices (sorted) — have V as unknowns
    pq_indices: Vec<usize>,
}

use crate::matrix::SENTINEL;

/// Apply remote voltage regulation bus-type switching for FDPF.
///
/// Demotes terminal PV buses to PQ when all their in-service generators
/// regulate a remote bus, and promotes the remote regulated PQ buses to PV.
/// This ensures FDPF's B'' matrix has the correct dimension and that
/// voltage-regulated remote buses are treated as PV unknowns.
fn apply_fdpf_remote_reg(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    bus_types: &mut [BusType],
) {
    let mut terminal_all_remote: HashMap<usize, bool> = HashMap::new();
    let mut remote_promote: HashMap<usize, f64> = HashMap::new();

    for g in &network.generators {
        if !g.in_service {
            continue;
        }
        let Some(&gen_idx) = bus_map.get(&g.bus) else {
            continue;
        };
        if !matches!(bus_types[gen_idx], BusType::PV | BusType::Slack) {
            continue;
        }
        let reg = g.reg_bus.unwrap_or(g.bus);
        let Some(&reg_idx) = bus_map.get(&reg) else {
            continue;
        };
        if reg == g.bus || reg_idx == gen_idx {
            terminal_all_remote.insert(gen_idx, false);
        } else {
            terminal_all_remote.entry(gen_idx).or_insert(true);
            let vs_entry = remote_promote
                .entry(reg_idx)
                .or_insert(g.voltage_setpoint_pu);
            if g.voltage_setpoint_pu > *vs_entry {
                *vs_entry = g.voltage_setpoint_pu;
            }
        }
    }
    for (idx, all_remote) in &terminal_all_remote {
        if *all_remote && bus_types[*idx] == BusType::PV {
            bus_types[*idx] = BusType::PQ;
        }
    }
    for &idx in remote_promote.keys() {
        if bus_types[idx] == BusType::PQ {
            bus_types[idx] = BusType::PV;
        }
    }
}

/// Build B' triplets: `1/x` per branch, tap=1 for all, no R, no charging.
///
/// Per Stott & Alsac (1974) and MATPOWER `makeB.m`, B' sets `TAP=1` for all
/// branches in both XB and BX variants. Sign is preserved so series-compensated
/// branches (`x < 0`) correctly produce negative susceptance.
///
/// Rows/cols correspond to `pvpq_pos` positions (slack bus removed).
fn build_b_prime_triplets(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    pvpq_pos: &[usize],
    n_pvpq: usize,
) -> HashMap<(usize, usize), f64> {
    let mut triplets: HashMap<(usize, usize), f64> = HashMap::new();

    for branch in &network.branches {
        if !branch.in_service || branch.x.abs() < 1e-20 {
            continue;
        }

        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        let b_series = 1.0 / branch.x;

        if let (Some(&row_i), Some(&row_j)) = (
            pvpq_pos.get(from_idx).filter(|&&p| p != SENTINEL),
            pvpq_pos.get(to_idx).filter(|&&p| p != SENTINEL),
        ) {
            *triplets.entry((row_i, row_j)).or_default() -= b_series;
            *triplets.entry((row_j, row_i)).or_default() -= b_series;
            *triplets.entry((row_i, row_i)).or_default() += b_series;
            *triplets.entry((row_j, row_j)).or_default() += b_series;
        } else if let Some(&row_i) = pvpq_pos.get(from_idx).filter(|&&p| p != SENTINEL) {
            *triplets.entry((row_i, row_i)).or_default() += b_series;
        } else if let Some(&row_j) = pvpq_pos.get(to_idx).filter(|&&p| p != SENTINEL) {
            *triplets.entry((row_j, row_j)).or_default() += b_series;
        }
    }

    // Ensure diagonals exist
    for i in 0..n_pvpq {
        triplets.entry((i, i)).or_default();
    }

    triplets
}

/// Build B'' triplets (XB variant): full branch susceptance with taps and charging.
///
/// Rows/cols correspond to `pq_pos` positions (slack + PV buses removed).
/// Includes bus shunt susceptance diagonal contributions.
fn build_b_double_prime_xb_triplets(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    pq_pos: &[usize],
    n_pq: usize,
) -> HashMap<(usize, usize), f64> {
    let mut triplets: HashMap<(usize, usize), f64> = HashMap::new();

    for branch in &network.branches {
        if !branch.in_service {
            continue;
        }

        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];

        let z_sq = branch.r * branch.r + branch.x * branch.x;
        let bs = if z_sq > 1e-20 { -branch.x / z_sq } else { 0.0 };
        let tap = branch.effective_tap();
        let tap_sq = tap * tap;

        if let Some(&row_i) = pq_pos.get(from_idx).filter(|&&p| p != SENTINEL) {
            *triplets.entry((row_i, row_i)).or_default() -= (bs + branch.b / 2.0) / tap_sq;
        }

        if let Some(&row_j) = pq_pos.get(to_idx).filter(|&&p| p != SENTINEL) {
            *triplets.entry((row_j, row_j)).or_default() -= bs + branch.b / 2.0;
        }

        let shift_rad = branch.phase_shift_rad;
        let cos_s = shift_rad.cos();
        let sin_s = shift_rad.sin();
        let gs = if z_sq > 1e-20 { branch.r / z_sq } else { 0.0 };
        let b_ft = (gs * sin_s + bs * cos_s) / tap;
        let b_tf = (-gs * sin_s + bs * cos_s) / tap;
        let b_off = (b_ft + b_tf) * 0.5;
        if let (Some(&row_i), Some(&row_j)) = (
            pq_pos.get(from_idx).filter(|&&p| p != SENTINEL),
            pq_pos.get(to_idx).filter(|&&p| p != SENTINEL),
        ) {
            *triplets.entry((row_i, row_j)).or_default() += b_off;
            *triplets.entry((row_j, row_i)).or_default() += b_off;
        }
    }

    // Bus shunt susceptance diagonal: -bs/baseMVA
    for (i, bus) in network.buses.iter().enumerate() {
        if bus.shunt_susceptance_mvar != 0.0
            && let Some(&row_i) = pq_pos.get(i).filter(|&&p| p != SENTINEL)
        {
            *triplets.entry((row_i, row_i)).or_default() -=
                bus.shunt_susceptance_mvar / network.base_mva;
        }
    }

    // Ensure diagonals exist
    for i in 0..n_pq {
        triplets.entry((i, i)).or_default();
    }

    triplets
}

impl FdpfFactors {
    /// Build and factor B' and B'' matrices from a network.
    ///
    /// B' (XB variant): 1/x for each branch, slack bus removed.
    /// B'': Full branch susceptance including taps and charging,
    ///      slack + PV buses removed.
    pub fn new(network: &Network) -> Result<Self, String> {
        let n = network.n_buses();
        let bus_map = network.bus_index_map();

        // Pre-check: FDPF requires a connected network. A disconnected network
        // produces a singular B' or B'' matrix with an opaque factorization error.
        // Detect islands early and return an informative message. For multi-island
        // networks, use solve_ac_pf with detect_islands=true instead.
        let islands = detect_islands(network, &bus_map);
        if islands.n_islands > 1 {
            return Err(format!(
                "FDPF requires a connected network ({} islands detected); \
                 use solve_ac_pf with detect_islands=true to solve multi-island networks",
                islands.n_islands
            ));
        }

        // Build mutable bus type array — allows remote voltage regulation
        // type-switching (PSS/E IREG) before classifying PV/PQ indices.
        let mut bus_types: Vec<BusType> = network.buses.iter().map(|b| b.bus_type).collect();

        // Remote voltage regulation: demote terminal PV→PQ, promote remote PQ→PV.
        apply_fdpf_remote_reg(network, &bus_map, &mut bus_types);

        let mut pv_indices = Vec::new();
        let mut pq_indices = Vec::new();
        for (i, &bt) in bus_types.iter().enumerate() {
            match bt {
                BusType::PV => pv_indices.push(i),
                BusType::PQ => pq_indices.push(i),
                _ => {}
            }
        }
        let mut pvpq_indices = Vec::new();
        pvpq_indices.extend(&pv_indices);
        pvpq_indices.extend(&pq_indices);
        pvpq_indices.sort();

        let n_pvpq = pvpq_indices.len();
        let n_pq = pq_indices.len();

        // Reverse lookup maps
        let mut pvpq_pos = vec![SENTINEL; n];
        for (pos, &bus) in pvpq_indices.iter().enumerate() {
            pvpq_pos[bus] = pos;
        }
        let mut pq_pos = vec![SENTINEL; n];
        for (pos, &bus) in pq_indices.iter().enumerate() {
            pq_pos[bus] = pos;
        }

        let b_prime_triplets = build_b_prime_triplets(network, &bus_map, &pvpq_pos, n_pvpq);
        let b_prime_klu = triplets_to_klu(&b_prime_triplets, n_pvpq)?;

        let b_double_prime_triplets =
            build_b_double_prime_xb_triplets(network, &bus_map, &pq_pos, n_pq);
        let b_double_prime_klu = triplets_to_klu(&b_double_prime_triplets, n_pq)?;

        Ok(Self {
            b_prime_klu,
            b_double_prime_klu,
            n,
            pvpq_indices,
            pq_indices,
        })
    }

    /// Rebuild B'' with an updated bus-type assignment (for Q-limit outer loop).
    ///
    /// Reuses B' from `self` (unchanged) and recomputes B'' from scratch using
    /// `bus_types` as the PQ/PV classification. Called by the Q-limit outer loop
    /// in `solve_fdpf` after switching a PV bus to PQ.
    pub(crate) fn rebuild_b_double_prime(
        &mut self,
        network: &Network,
        bus_types: &[BusType],
    ) -> Result<(), String> {
        let bus_map = network.bus_index_map();
        let n = network.n_buses();

        let mut pq_indices = Vec::new();
        for (i, &bt) in bus_types.iter().enumerate() {
            if bt == BusType::PQ {
                pq_indices.push(i);
            }
        }
        let n_pq = pq_indices.len();

        let mut pq_pos = vec![SENTINEL; n];
        for (pos, &bus) in pq_indices.iter().enumerate() {
            pq_pos[bus] = pos;
        }

        // Rebuild pvpq_indices to keep pvpq and pq in sync.
        let mut pvpq_indices = Vec::new();
        for (i, &bt) in bus_types.iter().enumerate() {
            if bt == BusType::PV || bt == BusType::PQ {
                pvpq_indices.push(i);
            }
        }
        pvpq_indices.sort();

        let b_double_prime_triplets =
            build_b_double_prime_xb_triplets(network, &bus_map, &pq_pos, n_pq);
        self.b_double_prime_klu = triplets_to_klu(&b_double_prime_triplets, n_pq)?;
        self.pq_indices = pq_indices;
        self.pvpq_indices = pvpq_indices;
        Ok(())
    }

    /// Build and factor B' and B'' matrices using the specified FDPF variant.
    ///
    /// - `FdpfVariant::Xb` (default): delegates to [`FdpfFactors::new`].
    /// - `FdpfVariant::Bx`: B' is the same as XB (imaginary part of Y-bus,
    ///   reactance only); B'' uses B\''_ij = -1/X_ij (reactance only, no R,
    ///   no charging, no taps). Better convergence on high R/X feeders.
    pub fn new_with_variant(network: &Network, variant: FdpfVariant) -> Result<Self, String> {
        // Island check is done inside new() for XB; do it here for BX so both paths are covered.
        if variant == FdpfVariant::Bx {
            let bus_map = network.bus_index_map();
            let islands = detect_islands(network, &bus_map);
            if islands.n_islands > 1 {
                return Err(format!(
                    "FDPF requires a connected network ({} islands detected); \
                     use solve_ac_pf with detect_islands=true to solve multi-island networks",
                    islands.n_islands
                ));
            }
        }
        match variant {
            FdpfVariant::Xb => Self::new(network),
            FdpfVariant::Bx => Self::new_bx(network),
        }
    }

    /// Build FDPF using the BX variant.
    ///
    /// BX differences from XB:
    /// - B': same as XB — 1/x per branch (reactance only), slack removed.
    /// - B'': 1/x per branch (reactance only, no R, no charging, no taps),
    ///   slack + PV buses removed.
    fn new_bx(network: &Network) -> Result<Self, String> {
        let n = network.n_buses();
        let bus_map = network.bus_index_map();

        // Build mutable bus type array with remote voltage regulation,
        // matching the XB variant's treatment of PSS/E IREG fields.
        let mut bus_types: Vec<BusType> = network.buses.iter().map(|b| b.bus_type).collect();
        apply_fdpf_remote_reg(network, &bus_map, &mut bus_types);

        let mut pv_indices = Vec::new();
        let mut pq_indices = Vec::new();
        for (i, &bt) in bus_types.iter().enumerate() {
            match bt {
                BusType::PV => pv_indices.push(i),
                BusType::PQ => pq_indices.push(i),
                _ => {}
            }
        }
        let mut pvpq_indices = Vec::new();
        pvpq_indices.extend(&pv_indices);
        pvpq_indices.extend(&pq_indices);
        pvpq_indices.sort();

        let n_pvpq = pvpq_indices.len();
        let n_pq = pq_indices.len();

        let mut pvpq_pos = vec![SENTINEL; n];
        for (pos, &bus) in pvpq_indices.iter().enumerate() {
            pvpq_pos[bus] = pos;
        }
        let mut pq_pos = vec![SENTINEL; n];
        for (pos, &bus) in pq_indices.iter().enumerate() {
            pq_pos[bus] = pos;
        }

        // BX B': same as XB — 1/x for each branch, tap=1 for all, slack removed.
        let b_prime_triplets = build_b_prime_triplets(network, &bus_map, &pvpq_pos, n_pvpq);
        let b_prime_klu = triplets_to_klu(&b_prime_triplets, n_pvpq)?;

        // BX B'': purely reactance-based: B''_ij = -1/X_ij (no R, no charging, no tap).
        // Rows/cols: PQ buses only.
        let mut b_double_prime_triplets: HashMap<(usize, usize), f64> = HashMap::new();

        for branch in &network.branches {
            if !branch.in_service || branch.x.abs() < 1e-20 {
                continue;
            }

            let from_idx = bus_map[&branch.from_bus];
            let to_idx = bus_map[&branch.to_bus];
            // BX B'' uses 1/X only (no resistance, no shunt charging, no tap).
            let b_series = 1.0 / branch.x;

            if let (Some(&row_i), Some(&row_j)) = (
                pq_pos.get(from_idx).filter(|&&p| p != SENTINEL),
                pq_pos.get(to_idx).filter(|&&p| p != SENTINEL),
            ) {
                *b_double_prime_triplets.entry((row_i, row_j)).or_default() -= b_series;
                *b_double_prime_triplets.entry((row_j, row_i)).or_default() -= b_series;
                *b_double_prime_triplets.entry((row_i, row_i)).or_default() += b_series;
                *b_double_prime_triplets.entry((row_j, row_j)).or_default() += b_series;
            } else if let Some(&row_i) = pq_pos.get(from_idx).filter(|&&p| p != SENTINEL) {
                *b_double_prime_triplets.entry((row_i, row_i)).or_default() += b_series;
            } else if let Some(&row_j) = pq_pos.get(to_idx).filter(|&&p| p != SENTINEL) {
                *b_double_prime_triplets.entry((row_j, row_j)).or_default() += b_series;
            }
        }
        // Bus shunt susceptance: -bs/baseMVA (same normalization as XB).
        // bus.shunt_susceptance_mvar is in MVAr physical; /baseMVA converts to pu once.
        for (i, bus) in network.buses.iter().enumerate() {
            if bus.shunt_susceptance_mvar != 0.0
                && let Some(&row_i) = pq_pos.get(i).filter(|&&p| p != SENTINEL)
            {
                *b_double_prime_triplets.entry((row_i, row_i)).or_default() -=
                    bus.shunt_susceptance_mvar / network.base_mva;
            }
        }
        for i in 0..n_pq {
            b_double_prime_triplets.entry((i, i)).or_default();
        }
        let b_double_prime_klu = triplets_to_klu(&b_double_prime_triplets, n_pq)?;

        Ok(Self {
            b_prime_klu,
            b_double_prime_klu,
            n,
            pvpq_indices,
            pq_indices,
        })
    }

    /// Solve FDPF using the Y-bus for mismatch computation.
    ///
    /// This is the contingency-friendly variant: the Y-bus can be the
    /// base case Y-bus with a delta applied for the outaged branch.
    /// B'/B'' factorizations are reused from the base case (they are
    /// approximate anyway — FDPF is inherently an approximation).
    ///
    /// Returns `Some(FdpfResult)` on convergence, `None` on failure.
    #[allow(clippy::too_many_arguments)]
    pub fn solve_from_ybus(
        &mut self,
        ybus: &YBus,
        p_spec: &[f64],
        q_spec: &[f64],
        vm_init: &[f64],
        va_init: &[f64],
        tolerance: f64,
        max_iterations: u32,
    ) -> Option<FdpfResult> {
        let mut vm = vm_init.to_vec();
        let mut va = va_init.to_vec();

        let n_pvpq = self.pvpq_indices.len();
        let n_pq = self.pq_indices.len();

        let mut rhs_p = vec![0.0f64; n_pvpq];
        let mut rhs_q = vec![0.0f64; n_pq];
        let mut p_calc = vec![0.0f64; self.n];
        let mut q_calc = vec![0.0f64; self.n];

        debug!(
            buses = self.n,
            pv_buses = self.pvpq_indices.len() - self.pq_indices.len(),
            pq_buses = self.pq_indices.len(),
            tolerance,
            max_iterations,
            "starting FDPF solve"
        );

        for iteration in 0..max_iterations {
            // Compute P and Q injections from Y-bus and current voltages
            compute_pq_from_ybus(ybus, &vm, &va, &mut p_calc, &mut q_calc);

            // P mismatch: ΔP/V for pvpq buses
            let mut max_p_mismatch = 0.0f64;
            for (k, &i) in self.pvpq_indices.iter().enumerate() {
                let dp = (p_spec[i] - p_calc[i]) / vm[i];
                max_p_mismatch = max_p_mismatch.max(dp.abs() * vm[i]); // actual mismatch
                rhs_p[k] = dp;
            }

            // Q mismatch: ΔQ/V for pq buses
            let mut max_q_mismatch = 0.0f64;
            for (k, &i) in self.pq_indices.iter().enumerate() {
                let dq = (q_spec[i] - q_calc[i]) / vm[i];
                max_q_mismatch = max_q_mismatch.max(dq.abs() * vm[i]);
                rhs_q[k] = dq;
            }

            let max_mismatch = max_p_mismatch.max(max_q_mismatch);
            if max_mismatch < tolerance {
                debug!(iteration, max_mismatch, "FDPF converged");
                return Some(FdpfResult {
                    vm,
                    va,
                    iterations: iteration,
                    max_mismatch,
                });
            }

            trace!(iteration, max_mismatch, "FDPF iteration");

            // P-θ step: B'·Δθ = ΔP/V
            if self.b_prime_klu.solve(&mut rhs_p).is_err() {
                warn!(iteration, "FDPF B' solve failed");
                return None;
            }
            for (k, &i) in self.pvpq_indices.iter().enumerate() {
                va[i] += rhs_p[k];
            }

            // Recompute Q after θ update (half-iteration interleaving)
            compute_pq_from_ybus(ybus, &vm, &va, &mut p_calc, &mut q_calc);

            // Q-V step: B''·ΔV = ΔQ/V
            for (k, &i) in self.pq_indices.iter().enumerate() {
                rhs_q[k] = (q_spec[i] - q_calc[i]) / vm[i];
            }
            if self.b_double_prime_klu.solve(&mut rhs_q).is_err() {
                warn!(iteration, "FDPF B'' solve failed");
                return None;
            }
            for (k, &i) in self.pq_indices.iter().enumerate() {
                vm[i] += rhs_q[k];
                vm[i] = vm[i].clamp(0.5, 2.0);
            }
        }

        warn!(
            max_iterations,
            "FDPF did not converge within iteration budget"
        );
        None // max iterations reached
    }
}

/// Build a KLU solver from triplets, factor it, and return the solver.
fn triplets_to_klu(triplets: &HashMap<(usize, usize), f64>, n: usize) -> Result<KluSolver, String> {
    // Convert to CSC format (sorted by column, then row)
    let mut entries: Vec<(usize, usize, f64)> =
        triplets.iter().map(|(&(r, c), &v)| (r, c, v)).collect();
    entries.sort_by_key(|&(r, c, _)| (c, r));

    let nnz = entries.len();
    let mut col_ptrs = vec![0usize; n + 1];
    let mut row_indices = Vec::with_capacity(nnz);
    let mut values = Vec::with_capacity(nnz);

    for &(r, c, v) in &entries {
        col_ptrs[c + 1] += 1;
        row_indices.push(r);
        values.push(v);
    }
    for i in 1..=n {
        col_ptrs[i] += col_ptrs[i - 1];
    }

    let mut klu = KluSolver::new(n, &col_ptrs, &row_indices)
        .map_err(|error| format!("FDPF matrix symbolic analysis failed: {error}"))?;
    if klu.factor(&values).is_err() {
        return Err(format!(
            "FDPF matrix factorization failed (dim={n}); \
             network may be disconnected or have zero-reactance branches"
        ));
    }
    Ok(klu)
}

/// Compute P and Q injections from Y-bus and voltages (inline, no allocation).
fn compute_pq_from_ybus(
    ybus: &YBus,
    vm: &[f64],
    va: &[f64],
    p_calc: &mut [f64],
    q_calc: &mut [f64],
) {
    let n = ybus.n;
    p_calc[..n].fill(0.0);
    q_calc[..n].fill(0.0);

    for i in 0..n {
        let vi = vm[i];
        let mut pi = 0.0;
        let mut qi = 0.0;

        let row = ybus.row(i);
        for (k, &j) in row.col_idx.iter().enumerate() {
            let vj = vm[j];
            let theta_ij = va[i] - va[j];
            let (sin_t, cos_t) = theta_ij.sin_cos();
            let g = row.g[k];
            let b = row.b[k];

            pi += vj * (g * cos_t + b * sin_t);
            qi += vj * (g * sin_t - b * cos_t);
        }

        p_calc[i] = vi * pi;
        q_calc[i] = vi * qi;
    }
}

/// Solve AC power flow using the Fast Decoupled method.
///
/// This is a convenience wrapper that handles the full FDPF pipeline:
/// 1. Build Y-bus
/// 2. Create and factor B'/B'' matrices
/// 3. Initialize voltages (generator setpoints or flat start)
/// 4. Run the FDPF iteration
/// 5. Return a `PfSolution`
///
/// For contingency screening or other advanced uses where you need to reuse
/// B'/B'' factors across multiple solves, use `FdpfFactors` directly.
pub fn solve_fdpf(
    network: &Network,
    options: &FdpfOptions,
) -> Result<surge_solution::PfSolution, String> {
    network
        .validate()
        .map_err(|e| format!("network validation failed: {e}"))?;
    solve_fdpf_validated(network, options)
}

fn solve_fdpf_validated(
    network: &Network,
    options: &FdpfOptions,
) -> Result<surge_solution::PfSolution, String> {
    // Auto-reduce node-breaker topology before solving.
    if options.auto_reduce_topology && network.topology.is_some() {
        let reduced = surge_topology::rebuild_topology(network)
            .map_err(|e| format!("topology reduction failed: {e}"))?;
        let mut opts = options.clone();
        opts.auto_reduce_topology = false;
        return solve_fdpf_validated(&reduced, &opts);
    }

    let start = std::time::Instant::now();
    let ybus = crate::matrix::ybus::build_ybus(network);
    let mut fdpf = FdpfFactors::new_with_variant(network, options.variant)?;

    let p_spec = network.bus_p_injection_pu();
    let mut q_spec = network.bus_q_injection_pu();

    let mut vm: Vec<f64> = network
        .buses
        .iter()
        .map(|b| b.voltage_magnitude_pu)
        .collect();
    let va: Vec<f64> = if options.flat_start {
        vec![0.0; network.n_buses()]
    } else {
        network.buses.iter().map(|b| b.voltage_angle_rad).collect()
    };

    let bus_map = network.bus_index_map();
    if options.flat_start {
        vm.fill(1.0);
    } else {
        for g in &network.generators {
            if g.in_service
                && let Some(&idx) = bus_map.get(&g.bus)
                && (network.buses[idx].bus_type == BusType::PV
                    || network.buses[idx].bus_type == BusType::Slack)
            {
                let reg = g.reg_bus.unwrap_or(g.bus);
                if let Some(&reg_idx) = bus_map.get(&reg) {
                    vm[reg_idx] = g.voltage_setpoint_pu;
                }
            }
        }
    }

    // Initial FDPF solve.
    let result = fdpf.solve_from_ybus(
        &ybus,
        &p_spec,
        &q_spec,
        &vm,
        &va,
        options.tolerance,
        options.max_iterations,
    );

    let (mut cur_vm, mut cur_va, mut total_iter, mut final_mismatch) = match result {
        Some(r) => (r.vm, r.va, r.iterations, r.max_mismatch),
        None => {
            let elapsed = start.elapsed().as_secs_f64();
            let bus_numbers: Vec<u32> = network.buses.iter().map(|b| b.number).collect();
            let (branch_pf, branch_pt, branch_qf, branch_qt) =
                surge_solution::compute_branch_power_flows(network, &vm, &va, network.base_mva);
            return Ok(surge_solution::PfSolution {
                pf_model: surge_solution::PfModel::Ac,
                status: surge_solution::SolveStatus::MaxIterations,
                iterations: options.max_iterations,
                max_mismatch: f64::INFINITY,
                solve_time_secs: elapsed,
                voltage_magnitude_pu: network
                    .buses
                    .iter()
                    .map(|b| b.voltage_magnitude_pu)
                    .collect(),
                voltage_angle_rad: network.buses.iter().map(|b| b.voltage_angle_rad).collect(),
                active_power_injection_pu: p_spec,
                reactive_power_injection_pu: q_spec,
                branch_p_from_mw: branch_pf,
                branch_p_to_mw: branch_pt,
                branch_q_from_mvar: branch_qf,
                branch_q_to_mvar: branch_qt,
                bus_numbers,
                island_ids: Vec::new(),
                q_limited_buses: Vec::new(),
                n_q_limit_switches: 0,
                gen_slack_contribution_mw: Vec::new(),
                convergence_history: Vec::new(),
                worst_mismatch_bus: None,
                area_interchange: None,
            });
        }
    };

    // Q-limit enforcement outer loop (MATPOWER one-at-a-time convention).
    let mut q_limited_bus_numbers: Vec<u32> = Vec::new();
    let mut q_limit_failed = false;
    let n_q_switches = if options.enforce_q_limits {
        let q_limits = crate::solver::newton_raphson::collect_q_limits(network);
        let n = network.n_buses();
        let mut bus_types: Vec<BusType> = network.buses.iter().map(|b| b.bus_type).collect();
        let mut switched_to_pq = vec![false; n];
        let max_q_switches = bus_types.iter().filter(|&&bt| bt == BusType::PV).count();
        let mut n_switched = 0usize;

        for _ in 0..max_q_switches {
            // Compute Q at all buses from current solution.
            let mut p_calc = vec![0.0f64; n];
            let mut q_calc = vec![0.0f64; n];
            compute_pq_from_ybus(&ybus, &cur_vm, &cur_va, &mut p_calc, &mut q_calc);

            // Find worst Q-limit violator among non-switched PV buses.
            let mut worst_idx: Option<usize> = None;
            let mut worst_viol = 0.0f64;
            for (i, &bt) in bus_types.iter().enumerate() {
                if bt != BusType::PV || switched_to_pq[i] {
                    continue;
                }
                let Some(&(q_at_min, q_at_max)) = q_limits.get(&i) else {
                    continue;
                };
                let qi = q_calc[i];
                let viol = if qi > q_at_max {
                    qi - q_at_max
                } else if qi < q_at_min {
                    q_at_min - qi
                } else {
                    0.0
                };
                if viol > worst_viol {
                    worst_viol = viol;
                    worst_idx = Some(i);
                }
            }

            let Some(widx) = worst_idx else { break }; // no violations — done

            // Switch worst violator PV → PQ; fix q_spec at the limit.
            let (q_at_min, q_at_max) = q_limits[&widx];
            if q_calc[widx] > q_at_max {
                q_spec[widx] = q_at_max;
            } else {
                q_spec[widx] = q_at_min;
            }
            bus_types[widx] = BusType::PQ;
            switched_to_pq[widx] = true;
            q_limited_bus_numbers.push(network.buses[widx].number);
            n_switched += 1;

            // Rebuild B'' for the new PQ set, re-run FDPF from current voltages.
            if let Err(e) = fdpf.rebuild_b_double_prime(network, &bus_types) {
                warn!("FDPF Q-limit: B'' rebuild failed after switch: {e}");
                q_limit_failed = true;
                break;
            }

            match fdpf.solve_from_ybus(
                &ybus,
                &p_spec,
                &q_spec,
                &cur_vm,
                &cur_va,
                options.tolerance,
                options.max_iterations,
            ) {
                Some(r) => {
                    cur_vm = r.vm;
                    cur_va = r.va;
                    total_iter += r.iterations;
                    final_mismatch = r.max_mismatch;
                }
                None => {
                    warn!("FDPF Q-limit: re-solve failed after bus switch");
                    q_limit_failed = true;
                    break;
                }
            }
        }
        n_switched as u32
    } else {
        0
    };

    let elapsed = start.elapsed().as_secs_f64();
    let bus_numbers: Vec<u32> = network.buses.iter().map(|b| b.number).collect();
    let status = if q_limit_failed {
        surge_solution::SolveStatus::MaxIterations
    } else {
        surge_solution::SolveStatus::Converged
    };
    let (branch_pf, branch_pt, branch_qf, branch_qt) =
        surge_solution::compute_branch_power_flows(network, &cur_vm, &cur_va, network.base_mva);

    Ok(surge_solution::PfSolution {
        pf_model: surge_solution::PfModel::Ac,
        status,
        iterations: total_iter,
        max_mismatch: final_mismatch,
        solve_time_secs: elapsed,
        voltage_magnitude_pu: cur_vm,
        voltage_angle_rad: cur_va,
        active_power_injection_pu: p_spec,
        reactive_power_injection_pu: q_spec,
        branch_p_from_mw: branch_pf,
        branch_p_to_mw: branch_pt,
        branch_q_from_mvar: branch_qf,
        branch_q_to_mvar: branch_qt,
        bus_numbers,
        n_q_limit_switches: n_q_switches,
        q_limited_buses: q_limited_bus_numbers,
        island_ids: Vec::new(),
        gen_slack_contribution_mw: Vec::new(),
        convergence_history: Vec::new(),
        worst_mismatch_bus: None,
        area_interchange: None,
    })
}

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn data_available() -> bool {
        crate::test_cases::case_available("case9")
    }
    #[allow(dead_code)]
    fn test_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::new()
    }

    use super::*;
    use crate::matrix::ybus::build_ybus;
    use crate::solver::newton_raphson::{AcPfOptions, solve_ac_pf_kernel};

    fn load_case(name: &str) -> Network {
        crate::test_cases::load_case(name)
            .unwrap_or_else(|err| panic!("failed to load {name} fixture: {err}"))
    }

    /// Initialize voltages with generator setpoints (same as NR solver).
    fn init_voltages(net: &Network) -> (Vec<f64>, Vec<f64>) {
        let mut vm: Vec<f64> = net.buses.iter().map(|b| b.voltage_magnitude_pu).collect();
        let va: Vec<f64> = net.buses.iter().map(|b| b.voltage_angle_rad).collect();
        let bus_map = net.bus_index_map();
        for g in &net.generators {
            if g.in_service
                && let Some(&idx) = bus_map.get(&g.bus)
                && (net.buses[idx].bus_type == BusType::PV
                    || net.buses[idx].bus_type == BusType::Slack)
            {
                vm[idx] = g.voltage_setpoint_pu;
            }
        }
        (vm, va)
    }

    #[test]
    fn test_fdpf_case9() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let ybus = build_ybus(&net);
        let mut fdpf = FdpfFactors::new(&net).unwrap();

        let p_spec = net.bus_p_injection_pu();
        let q_spec = net.bus_q_injection_pu();
        let (vm, va) = init_voltages(&net);

        let result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-6, 50);
        assert!(result.is_some(), "FDPF should converge on case9");

        let r = result.unwrap();
        let (fdpf_vm, iters) = (r.vm, r.iterations);
        assert!(
            iters <= 30,
            "FDPF should converge in reasonable iterations: {iters}"
        );

        // Compare PQ bus voltages with NR solution
        let nr_sol = solve_ac_pf_kernel(&net, &AcPfOptions::default()).unwrap();
        for (i, bus) in net.buses.iter().enumerate() {
            if bus.bus_type == BusType::PQ {
                let diff: f64 = fdpf_vm[i] - nr_sol.voltage_magnitude_pu[i];
                assert!(
                    diff.abs() < 0.01,
                    "FDPF vs NR Vm mismatch at PQ bus {i}: fdpf={}, nr={}",
                    fdpf_vm[i],
                    nr_sol.voltage_magnitude_pu[i]
                );
            }
        }
    }

    #[test]
    fn test_fdpf_case14() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case14");
        let ybus = build_ybus(&net);
        let mut fdpf = FdpfFactors::new(&net).unwrap();

        let p_spec = net.bus_p_injection_pu();
        let q_spec = net.bus_q_injection_pu();
        let vm: Vec<f64> = net.buses.iter().map(|b| b.voltage_magnitude_pu).collect();
        let va: Vec<f64> = net.buses.iter().map(|b| b.voltage_angle_rad).collect();

        let result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-6, 50);
        assert!(result.is_some(), "FDPF should converge on case14");

        let r = result.unwrap();
        let (fdpf_vm, iters) = (r.vm, r.iterations);
        assert!(iters <= 30, "FDPF iterations: {iters}");

        // Compare PQ bus voltages against NR reference.
        // case14 is a good regression case for:
        //   - B'' shunt normalization (bus 9, Bs=19 MVAr): verifies that the
        //     B'' shunt diagonal is correctly computed as -Bs/baseMVA = -0.19 pu,
        //     not double-divided by baseMVA.
        //   - B' tap correction (M-01): case14 has 5 transformers with
        //     off-nominal tap ratios; using 1/|x| (XB) vs 1/(x*tap) (b_dc) changes
        //     the P-theta sub-problem for these branches.
        //
        // Tolerance: 0.01 pu Vm is the standard FDPF acceptance criterion for
        // transmission networks.
        let nr_sol = solve_ac_pf_kernel(
            &net,
            &AcPfOptions {
                enforce_q_limits: false,
                ..AcPfOptions::default()
            },
        )
        .unwrap();
        let max_vm_diff: f64 = fdpf_vm
            .iter()
            .zip(nr_sol.voltage_magnitude_pu.iter())
            .enumerate()
            .filter(|(i, _)| net.buses[*i].bus_type == BusType::PQ)
            .map(|(_, (&f, &n))| (f - n).abs())
            .fold(0.0, f64::max);
        assert!(
            max_vm_diff < 0.01,
            "case14: FDPF vs NR max PQ Vm diff {max_vm_diff:.4e} exceeds 0.01 pu — \
             check B'' shunt normalization (bus 9, Bs=19 MVAr) and B' tap correction"
        );

        // Specifically check bus 9 (index 8, Bs=19 MVAr): this bus is the
        // primary test vector for C-02 (shunt normalization).
        let bus9_diff = (fdpf_vm[8] - nr_sol.voltage_magnitude_pu[8]).abs();
        assert!(
            bus9_diff < 0.01,
            "case14 bus 9 (Bs=19 MVAr): FDPF Vm={:.4} vs NR Vm={:.4}, diff={:.4e}",
            fdpf_vm[8],
            nr_sol.voltage_magnitude_pu[8],
            bus9_diff
        );
    }

    #[test]
    fn test_fdpf_case118() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case118");
        let ybus = build_ybus(&net);
        let mut fdpf = FdpfFactors::new(&net).unwrap();

        let p_spec = net.bus_p_injection_pu();
        let q_spec = net.bus_q_injection_pu();
        let vm: Vec<f64> = net.buses.iter().map(|b| b.voltage_magnitude_pu).collect();
        let va: Vec<f64> = net.buses.iter().map(|b| b.voltage_angle_rad).collect();

        let result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-6, 50);
        assert!(result.is_some(), "FDPF should converge on case118");

        let r = result.unwrap();
        let (fdpf_vm, iters) = (r.vm, r.iterations);
        assert!(iters <= 30, "FDPF iterations: {iters}");

        // Compare with NR
        let nr_sol = solve_ac_pf_kernel(&net, &AcPfOptions::default()).unwrap();
        let max_vm_diff: f64 = fdpf_vm
            .iter()
            .zip(nr_sol.voltage_magnitude_pu.iter())
            .map(|(&f, &n)| (f - n).abs())
            .fold(0.0, f64::max);
        assert!(
            max_vm_diff < 0.01,
            "FDPF vs NR max Vm diff: {max_vm_diff:.4e}"
        );
    }

    #[test]
    fn test_fdpf_warm_start() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        // FDPF from warm start (NR solution) should converge in 0-1 iterations
        let net = load_case("case9");
        let ybus = build_ybus(&net);
        let mut fdpf = FdpfFactors::new(&net).unwrap();

        let p_spec = net.bus_p_injection_pu();
        let q_spec = net.bus_q_injection_pu();

        let nr_sol = solve_ac_pf_kernel(&net, &AcPfOptions::default()).unwrap();
        let result = fdpf.solve_from_ybus(
            &ybus,
            &p_spec,
            &q_spec,
            &nr_sol.voltage_magnitude_pu,
            &nr_sol.voltage_angle_rad,
            1e-6,
            50,
        );
        assert!(result.is_some(), "FDPF from warm start should converge");
        let iters = result.unwrap().iterations;
        assert!(
            iters <= 2,
            "FDPF from warm start should be near-instant: {iters} iters"
        );
    }

    /// FDPF must converge from cold start on all standard test cases.
    /// In production, base case problems are an operational reality — we cannot
    /// assume a prior NR solution is always available as warm start.
    #[test]
    fn test_fdpf_cold_start_all_cases() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let cases = [
            ("case9", 50, 0.01),
            ("case14", 50, 0.01),
            ("case30", 50, 0.01),
            ("case118", 50, 0.01),
            ("case2383wp", 100, 0.05),
            ("case_ACTIVSg2000", 100, 0.05),
        ];

        for (name, max_iters, vm_tol) in &cases {
            let net = load_case(name);
            let ybus = build_ybus(&net);
            let mut fdpf = FdpfFactors::new(&net).unwrap();

            let p_spec = net.bus_p_injection_pu();
            let q_spec = net.bus_q_injection_pu();
            let (vm, va) = init_voltages(&net);

            let result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-6, *max_iters);
            assert!(
                result.is_some(),
                "FDPF cold start should converge on {name}"
            );

            let r = result.unwrap();
            let (fdpf_vm, iters) = (r.vm, r.iterations);
            eprintln!("{name}: FDPF converged in {iters} iterations");

            // Verify against NR solution (no Q-limit enforcement so both solve the same system).
            let nr_sol = solve_ac_pf_kernel(
                &net,
                &AcPfOptions {
                    enforce_q_limits: false,
                    ..AcPfOptions::default()
                },
            )
            .unwrap();
            let max_vm_diff: f64 = fdpf_vm
                .iter()
                .zip(nr_sol.voltage_magnitude_pu.iter())
                .enumerate()
                .filter(|(i, _)| net.buses[*i].bus_type == BusType::PQ)
                .map(|(_, (&f, &n))| (f - n).abs())
                .fold(0.0, f64::max);
            assert!(
                max_vm_diff < *vm_tol,
                "{name}: FDPF vs NR max PQ Vm diff {max_vm_diff:.4e} exceeds tolerance {vm_tol}"
            );
        }
    }

    /// Verify FDPF converges on large cases from cold start.
    /// These are slower so kept in a separate test.
    #[test]
    fn test_fdpf_cold_start_large_cases() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let cases = [
            "case6515rte",
            "case9241pegase",
            "case_ACTIVSg10k",
            "case13659pegase",
        ];

        for name in &cases {
            let net = load_case(name);
            let ybus = build_ybus(&net);
            let mut fdpf = FdpfFactors::new(&net).unwrap();

            let p_spec = net.bus_p_injection_pu();
            let q_spec = net.bus_q_injection_pu();
            let (vm, va) = init_voltages(&net);

            let result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-4, 200);
            assert!(
                result.is_some(),
                "FDPF cold start should converge on {name} (even to 1e-4)"
            );

            let iters = result.unwrap().iterations;
            eprintln!("{name}: FDPF converged in {iters} iterations (tol=1e-4)");
        }
    }

    /// AC-06: BX variant must converge to the same solution as XB on case30.
    ///
    /// case30 has some high R/X branches, making it a useful regression case.
    /// Both variants should agree with the NR solution within 0.01 p.u. Vm.
    #[test]
    fn test_fdpf_bx_variant_case30() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case30");
        let ybus = build_ybus(&net);

        let p_spec = net.bus_p_injection_pu();
        let q_spec = net.bus_q_injection_pu();
        let (vm_init, va_init) = init_voltages(&net);

        // XB solve.
        let mut fdpf_xb = FdpfFactors::new_with_variant(&net, super::FdpfVariant::Xb).unwrap();
        let xb_result =
            fdpf_xb.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm_init, &va_init, 1e-6, 100);
        assert!(xb_result.is_some(), "XB variant must converge on case30");
        let xb_r = xb_result.unwrap();
        let (xb_vm, xb_iters) = (xb_r.vm, xb_r.iterations);
        eprintln!("case30 XB converged in {xb_iters} iterations");

        // BX solve.
        let mut fdpf_bx = FdpfFactors::new_with_variant(&net, super::FdpfVariant::Bx).unwrap();
        let bx_result =
            fdpf_bx.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm_init, &va_init, 1e-6, 100);
        assert!(bx_result.is_some(), "BX variant must converge on case30");
        let bx_r = bx_result.unwrap();
        let (bx_vm, bx_iters) = (bx_r.vm, bx_r.iterations);
        eprintln!("case30 BX converged in {bx_iters} iterations");

        // NR reference solution.
        let nr_sol = solve_ac_pf_kernel(&net, &AcPfOptions::default()).unwrap();

        // Both variants must agree with NR within 0.01 p.u. on PQ buses.
        let n = net.n_buses();
        for i in 0..n {
            if net.buses[i].bus_type != BusType::PQ {
                continue;
            }
            let xb_diff = (xb_vm[i] - nr_sol.voltage_magnitude_pu[i]).abs();
            let bx_diff = (bx_vm[i] - nr_sol.voltage_magnitude_pu[i]).abs();
            assert!(
                xb_diff < 0.01,
                "XB vs NR Vm mismatch at bus {i}: xb={:.4}, nr={:.4}",
                xb_vm[i],
                nr_sol.voltage_magnitude_pu[i]
            );
            assert!(
                bx_diff < 0.01,
                "BX vs NR Vm mismatch at bus {i}: bx={:.4}, nr={:.4}",
                bx_vm[i],
                nr_sol.voltage_magnitude_pu[i]
            );
        }

        // XB and BX must also agree with each other within 0.01 p.u.
        let max_xb_bx_diff: f64 = (0..n)
            .filter(|&i| net.buses[i].bus_type == BusType::PQ)
            .map(|i| (xb_vm[i] - bx_vm[i]).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_xb_bx_diff < 0.01,
            "XB vs BX max Vm diff = {max_xb_bx_diff:.4e} — variants should converge to same solution"
        );
    }

    /// GAP 1: FDPF accuracy vs NR on case57.
    ///
    /// case57 is a 57-bus IEEE test case with mixed line/transformer topology
    /// and is a standard FDPF regression target.  This test:
    ///   - Verifies FDPF converges on case57 within 30 iterations.
    ///   - Checks max PQ-bus Vm deviation from NR < 1 mpu (1e-3 pu).
    ///   - Checks max PQ-bus Va deviation from NR < 0.05°.
    ///
    /// These tolerances are the standard FDPF acceptance criterion for
    /// well-conditioned transmission networks (X/R ≥ 3 on most branches).
    #[test]
    fn test_fdpf_agrees_with_nr_case57() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case57");
        let ybus = build_ybus(&net);
        let mut fdpf = FdpfFactors::new(&net).unwrap();

        let p_spec = net.bus_p_injection_pu();
        let q_spec = net.bus_q_injection_pu();
        let (vm, va) = init_voltages(&net);

        let result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-6, 50);
        assert!(result.is_some(), "FDPF should converge on case57");

        let r = result.unwrap();
        let (fdpf_vm, fdpf_va, iters) = (r.vm, r.va, r.iterations);
        assert!(
            iters <= 30,
            "FDPF case57 should converge within 30 iterations, got {iters}"
        );

        let nr_sol = solve_ac_pf_kernel(
            &net,
            &AcPfOptions {
                enforce_q_limits: false,
                ..AcPfOptions::default()
            },
        )
        .unwrap();

        // Check Vm agreement on PQ buses.
        let max_vm_diff: f64 = fdpf_vm
            .iter()
            .zip(nr_sol.voltage_magnitude_pu.iter())
            .enumerate()
            .filter(|(i, _)| net.buses[*i].bus_type == BusType::PQ)
            .map(|(_, (&f, &n))| (f - n).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_vm_diff < 1e-3,
            "case57: FDPF vs NR max PQ Vm diff {max_vm_diff:.4e} pu exceeds 1e-3 pu"
        );

        // Check Va agreement on PQ buses (converted to degrees).
        let max_va_diff_deg: f64 = fdpf_va
            .iter()
            .zip(nr_sol.voltage_angle_rad.iter())
            .enumerate()
            .filter(|(i, _)| net.buses[*i].bus_type == BusType::PQ)
            .map(|(_, (&f, &n))| (f - n).abs().to_degrees())
            .fold(0.0_f64, f64::max);
        assert!(
            max_va_diff_deg < 0.05,
            "case57: FDPF vs NR max PQ Va diff {max_va_diff_deg:.4e} deg exceeds 0.05°"
        );
    }

    // -----------------------------------------------------------------------
    // Issue 19A: Island detection — FDPF rejects disconnected networks.
    // -----------------------------------------------------------------------
    /// `solve_fdpf` on a network with two disconnected islands must return Err
    /// containing "islands detected" rather than silently producing incorrect results.
    #[test]
    fn test_fdpf_rejects_multi_island_network() {
        use surge_network::Network;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        // Island A: buses 1–2 (connected by a line).
        // Island B: buses 3–4 (connected by a line).
        // No cross-island branch → two islands.
        let mut net = Network::new("two_island_fdpf_test");
        net.base_mva = 100.0;
        net.buses = vec![
            Bus::new(1, BusType::Slack, 138.0),
            Bus::new(2, BusType::PQ, 138.0),
            Bus::new(3, BusType::Slack, 138.0),
            Bus::new(4, BusType::PQ, 138.0),
        ];
        net.branches = vec![
            Branch::new_line(1, 2, 0.01, 0.1, 0.02),
            Branch::new_line(3, 4, 0.01, 0.1, 0.02),
        ];
        net.generators = vec![Generator::new(1, 50.0, 1.0), Generator::new(3, 30.0, 1.0)];
        net.loads = vec![Load::new(2, 20.0, 5.0), Load::new(4, 15.0, 3.0)];

        let opts = FdpfOptions::default();
        let result = solve_fdpf(&net, &opts);
        assert!(
            result.is_err(),
            "solve_fdpf must return Err on a multi-island network"
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("island") || msg.contains("connected"),
            "error message should mention islands; got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Issue 19B: Q-limit enforcement in standalone FDPF.
    // -----------------------------------------------------------------------
    /// On a 3-bus network with a tight generator Q-max, FDPF with
    /// `enforce_q_limits = true` must switch the PV bus to PQ without
    /// failing.  A PQ bus must exist from the start so B'' is non-empty.
    #[test]
    fn test_fdpf_q_limit_enforcement() {
        use surge_network::Network;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        // 3-bus network:
        //   Bus 1 (Slack, source)
        //   Bus 2 (PV) — generator with very tight Q limits
        //   Bus 3 (PQ) — load bus (B'' is non-empty from the start)
        let mut net = Network::new("fdpf_qlimit_test");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 138.0);
        b1.voltage_magnitude_pu = 1.05;
        net.buses.push(b1);

        let mut b2 = Bus::new(2, BusType::PV, 138.0);
        b2.voltage_magnitude_pu = 1.04;
        net.buses.push(b2);

        let b3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses.push(b3);

        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.02, 0.2, 0.04));

        // Generator at bus 2 with very tight Q limits → hits Q_max quickly
        let mut g = Generator::new(2, 20.0, 1.04);
        g.qmax = 1.0 / 100.0; // 1 MVAR in pu (very tight)
        g.qmin = -1.0 / 100.0;
        net.generators.push(g);
        net.generators.push(Generator::new(1, 200.0, 1.05));

        // Heavy Q load at bus 3 forces Q demand through bus 2
        net.loads.push(Load::new(3, 60.0, 50.0));

        let opts = FdpfOptions {
            enforce_q_limits: true,
            ..FdpfOptions::default()
        };
        let result = solve_fdpf(&net, &opts);
        assert!(
            result.is_ok(),
            "FDPF with Q-limit enforcement must return Ok: {:?}",
            result.err()
        );
    }
}
