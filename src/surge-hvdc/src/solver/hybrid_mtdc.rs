// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! LCC/VSC Hybrid Multi-Terminal DC (MTDC) Power Flow.
//!
//! Implements FPQ-57 / PLAN-100 / P5-066: joint power flow for heterogeneous
//! multi-terminal DC networks where LCC (Line-Commutated Converter) rectifiers
//! and VSC (Voltage-Sourced Converter) inverters share the same DC bus network.
//!
//! # Background
//!
//! Real-world HVDC corridors mix LCC and VSC technology:
//! - **NorNed** (Norway–Netherlands): LCC 700 MW
//! - **BritNed** (UK–Netherlands): VSC 1000 MW
//! - Proposed **ERCOT HVDC overlays**: LCC backbone + VSC taps
//!
//! This module enables joint power flow across such heterogeneous systems.
//!
//! # LCC Converter Model (thyristor-based, current-sourced)
//!
//! DC voltage equation (6-pulse bridge):
//! ```text
//! V_dc = (3√2/π) × V_ac × cos(α) − I_dc × (3/π) × X_c
//! ```
//! where:
//! - `V_ac` = AC line-to-line voltage magnitude (pu)
//! - `α` = firing angle (rectifier) or extinction angle `γ` (inverter)
//! - `X_c` = commutation reactance (pu on system base)
//! - `I_dc` = DC current (pu)
//!
//! The firing angle is solved analytically given V_dc and I_dc:
//! ```text
//! cos(α) = (V_dc + K_COMM × X_c × I_dc) / (K_BRIDGE × V_ac)
//! ```
//!
//! Active and reactive power:
//! ```text
//! P_lcc = V_dc × I_dc
//! cos(φ) ≈ V_dc / (K_BRIDGE × V_ac)
//! Q_lcc = P_lcc × tan(φ)     [always absorbed: Q_lcc < 0 injection into AC]
//! ```
//!
//! # VSC Converter Model (voltage-sourced, bidirectional)
//!
//! Independent P and Q control within the converter capability curve.
//! One VSC may be designated as the DC voltage slack.
//!
//! # Newton-Raphson Formulation
//!
//! The NR unknowns are the DC bus voltages at all non-slack DC buses.
//! For each free DC bus k, the KCL mismatch is:
//!
//! ```text
//! f_k = Σ_j G_kj × V_dc_j − I_net_k(V_dc_k) = 0
//! ```
//!
//! where `I_net_k` is the net current injection at bus k from all converters:
//! - LCC: `I_lcc_k = P_setpoint / (V_dc_k × base_mva)` (constant-power source)
//! - VSC: `I_vsc_k = P_setpoint / (V_dc_k × base_mva)` (constant-power source)
//!
//! The LCC firing angle α is **derived analytically** at each iteration from
//! the current DC bus voltage (not an independent NR unknown), which makes the
//! system well-conditioned.
//!
//! Convergence criterion: max|f_k| < `tol` (default 1e-6 pu).

use num_complex::Complex64;
use tracing::{debug, info, warn};

use crate::dc_network::topology::{DcNetwork, solve_system};
use crate::error::HvdcError;

// Physical constant: (3√2/π) for 6-pulse bridge converter
const K_BRIDGE: f64 = 3.0 * std::f64::consts::SQRT_2 / std::f64::consts::PI;
// (3/π) for commutation voltage drop
const K_COMM: f64 = 3.0 / std::f64::consts::PI;

// ─── Data types ──────────────────────────────────────────────────────────────

/// An LCC (Line-Commutated Converter, thyristor-based) converter station.
///
/// LCC converters are current-sourced. The DC current is the primary control
/// variable. Firing angle α (rectifier) or extinction angle γ (inverter)
/// is derived analytically from the DC bus voltage via the bridge equation.
#[derive(Debug, Clone)]
pub struct LccConverter {
    /// AC bus number (external, matches network bus numbering).
    pub bus_ac: u32,
    /// DC bus index (0-indexed internal index in `HybridMtdcNetwork`).
    pub bus_dc: usize,
    /// Rated apparent power in MW.
    pub rated_power_mw: f64,
    /// Rated AC voltage in kV (informational; power flow uses pu quantities).
    pub rated_voltage_kv: f64,
    /// Commutation reactance in per-unit on the system base (MVA base = 100 MVA).
    ///
    /// Typical range: 0.10–0.20 pu. Models the transformer leakage reactance
    /// that causes commutation voltage overlap.
    pub x_commutation_pu: f64,
    /// Minimum firing angle α_min in degrees (default 5°).
    ///
    /// Below α_min the converter cannot fire reliably.
    pub alpha_min_deg: f64,
    /// Maximum firing angle α_max in degrees (default 150°).
    ///
    /// Above this the converter would cause commutation failure.
    pub alpha_max_deg: f64,
    /// Minimum extinction angle γ_min in degrees (default 15°).
    ///
    /// In inverter mode the extinction angle must exceed γ_min.
    pub gamma_min_deg: f64,
    /// Active power setpoint in MW (positive = rectifier into DC, negative = inverter from DC).
    pub p_setpoint_mw: f64,
    /// True if this converter is in service.
    pub in_service: bool,
}

impl LccConverter {
    /// Create an LCC converter with default angle limits.
    pub fn new(bus_ac: u32, bus_dc: usize, rated_power_mw: f64) -> Self {
        Self {
            bus_ac,
            bus_dc,
            rated_power_mw,
            rated_voltage_kv: 500.0,
            x_commutation_pu: 0.15,
            alpha_min_deg: 5.0,
            alpha_max_deg: 150.0,
            gamma_min_deg: 15.0,
            p_setpoint_mw: rated_power_mw,
            in_service: true,
        }
    }

    /// True if operating as a rectifier (P_setpoint ≥ 0, power flows from AC into DC).
    pub fn is_rectifier(&self) -> bool {
        self.p_setpoint_mw >= 0.0
    }

    /// Compute the DC current injection at the DC bus (pu), given DC bus voltage.
    ///
    /// Sign convention: positive = power injected into the DC network (rectifier).
    ///
    /// I_dc = P_setpoint / V_dc
    #[inline]
    pub fn i_dc_pu(&self, v_dc: f64, base_mva: f64) -> f64 {
        if v_dc.abs() > 1e-9 {
            (self.p_setpoint_mw / base_mva) / v_dc
        } else {
            0.0
        }
    }

    /// Derive the firing angle α from V_dc, V_ac, and I_dc (radians).
    ///
    /// From the bridge equation: V_dc = K_BRIDGE × V_ac × cos(α) − K_COMM × X_c × I_dc
    /// Rearranging: cos(α) = (V_dc + K_COMM × X_c × |I_dc|) / (K_BRIDGE × V_ac)
    ///
    /// For the rectifier I_dc > 0; for inverter the sign of I_dc reverses the
    /// commutation drop term (inverter adds voltage, so K_COMM term is subtracted).
    ///
    /// Returns the angle clamped to [α_min, α_max].
    pub fn firing_angle_rad(&self, v_dc: f64, v_ac: f64, i_dc_abs: f64) -> f64 {
        let denominator = K_BRIDGE * v_ac;
        if denominator < 1e-9 {
            return self.alpha_min_deg.to_radians();
        }
        // For rectifier: V_dc = K_BRIDGE*V_ac*cos(α) - K_COMM*Xc*I_dc
        //   → cos(α) = (V_dc + K_COMM*Xc*I_dc) / (K_BRIDGE*V_ac)
        // For inverter: V_dc = K_BRIDGE*V_ac*cos(γ) + K_COMM*Xc*I_dc
        // (voltage rises due to commutation in inverter mode)
        // We store γ as the "firing angle" for inverters in this implementation.
        let cos_angle = if self.is_rectifier() {
            (v_dc + K_COMM * self.x_commutation_pu * i_dc_abs) / denominator
        } else {
            (v_dc - K_COMM * self.x_commutation_pu * i_dc_abs) / denominator
        };
        let cos_clamped = cos_angle.clamp(-1.0 + 1e-9, 1.0 - 1e-9);
        let angle = cos_clamped.acos();
        let angle_min = self.alpha_min_deg.to_radians();
        let angle_max = self.alpha_max_deg.to_radians();
        angle.clamp(angle_min, angle_max)
    }

    /// Compute extinction angle γ for an inverter (radians).
    ///
    /// γ is the actual turn-off angle after commutation:
    /// For inverter: V_dc = K_BRIDGE*V_ac*cos(γ) + K_COMM*Xc*I_dc
    /// → cos(γ) = (V_dc - K_COMM*Xc*I_dc) / (K_BRIDGE*V_ac)
    ///
    /// A healthy commutation requires γ ≥ γ_min (default 15°).
    pub fn extinction_angle_rad(&self, v_dc: f64, v_ac: f64, i_dc_abs: f64) -> f64 {
        let denominator = K_BRIDGE * v_ac;
        if denominator < 1e-9 {
            return self.gamma_min_deg.to_radians();
        }
        let cos_gamma = (v_dc - K_COMM * self.x_commutation_pu * i_dc_abs) / denominator;
        let cos_clamped = cos_gamma.clamp(-1.0 + 1e-9, 1.0 - 1e-9);
        cos_clamped.acos().max(self.gamma_min_deg.to_radians())
    }
}

/// A VSC (Voltage-Sourced Converter, IGBT-based) converter station for hybrid MTDC.
///
/// VSC converters are voltage-sourced and bidirectional, providing independent
/// P and Q control. One VSC may be designated as the DC voltage slack.
#[derive(Debug, Clone)]
pub struct HybridVscConverter {
    /// AC bus number (external, matches network bus numbering).
    pub bus_ac: u32,
    /// DC bus index (0-indexed internal index in `HybridMtdcNetwork`).
    pub bus_dc: usize,
    /// Active power setpoint in MW.
    ///
    /// Sign convention: positive = rectifier (draws from AC, injects into DC);
    /// negative = inverter (draws from DC, injects into AC).
    /// For DC slack: determined by power balance (set to 0.0, ignored in solve).
    pub p_setpoint_mw: f64,
    /// Reactive power setpoint in MVAR (positive = injection into AC).
    pub q_setpoint_mvar: f64,
    /// Maximum reactive injection in MVAR.
    pub q_max_mvar: f64,
    /// Minimum reactive injection in MVAR.
    pub q_min_mvar: f64,
    /// If true, this VSC controls the DC bus voltage (acts as DC slack).
    pub is_dc_slack: bool,
    /// DC voltage setpoint for DC slack mode (pu).
    pub v_dc_setpoint: f64,
    /// Constant loss coefficient (pu, applied to system base MVA).
    pub loss_constant_mw: f64,
    /// Linear loss coefficient (pu/pu of AC current).
    pub loss_linear: f64,
    /// Quadratic loss coefficient for rectifier operation (pu/pu^2 of AC current).
    pub loss_quadratic_rectifier: f64,
    /// Quadratic loss coefficient for inverter operation (pu/pu^2 of AC current).
    pub loss_quadratic_inverter: f64,
    /// True if this converter is in service.
    pub in_service: bool,
}

impl HybridVscConverter {
    /// Create a VSC converter with zero losses (lossless model).
    pub fn new(bus_ac: u32, bus_dc: usize, p_setpoint_mw: f64) -> Self {
        Self {
            bus_ac,
            bus_dc,
            p_setpoint_mw,
            q_setpoint_mvar: 0.0,
            q_max_mvar: 100.0,
            q_min_mvar: -100.0,
            is_dc_slack: false,
            v_dc_setpoint: 1.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_quadratic_rectifier: 0.0,
            loss_quadratic_inverter: 0.0,
            in_service: true,
        }
    }

    #[inline]
    pub fn quadratic_loss_coefficient(&self, p_mw: f64) -> f64 {
        if p_mw >= 0.0 {
            self.loss_quadratic_rectifier
        } else {
            self.loss_quadratic_inverter
        }
    }

    /// Compute DC current injection at the DC bus (pu), given DC bus voltage.
    ///
    /// For DC slack VSC: the current is determined by power balance (return 0.0;
    /// the slack bus absorbs the residual automatically via KCL).
    #[inline]
    pub fn i_dc_pu(&self, v_dc: f64, base_mva: f64) -> f64 {
        if self.is_dc_slack {
            return 0.0;
        }
        if v_dc.abs() > 1e-9 {
            (self.p_setpoint_mw / base_mva) / v_dc
        } else {
            0.0
        }
    }

    /// Compute converter losses in MW given AC apparent power and voltage.
    pub fn losses_mw(&self, p_mw: f64, q_mvar: f64, v_ac_pu: f64, base_mva: f64) -> f64 {
        crate::model::vsc::compute_vsc_losses_mw(
            self.loss_constant_mw,
            self.loss_linear,
            self.quadratic_loss_coefficient(p_mw),
            p_mw,
            q_mvar,
            v_ac_pu,
            base_mva,
        )
    }
}

/// The hybrid MTDC network: LCC and VSC converters sharing the same DC bus network.
///
/// The DC topology (buses, branches, shunt/ground conductances) is stored in
/// the embedded [`DcNetwork`]. The converter stations are stored separately.
#[derive(Debug, Clone)]
pub struct HybridMtdcNetwork {
    /// DC network topology (buses, branches, voltages, shunt/ground conductances).
    pub dc_network: DcNetwork,
    /// LCC converter stations.
    pub lcc_converters: Vec<LccConverter>,
    /// VSC converter stations.
    pub vsc_converters: Vec<HybridVscConverter>,
    /// System MVA base.
    pub base_mva: f64,
}

impl HybridMtdcNetwork {
    /// Create a new hybrid MTDC network with `n_dc_buses` DC buses.
    pub fn new(base_mva: f64, n_dc_buses: usize, slack_dc_bus: usize) -> Self {
        Self {
            dc_network: DcNetwork::new(n_dc_buses, slack_dc_bus),
            lcc_converters: Vec::new(),
            vsc_converters: Vec::new(),
            base_mva,
        }
    }

    /// Number of DC buses.
    pub fn n_dc_buses(&self) -> usize {
        self.dc_network.n_buses()
    }

    /// Build the DC nodal conductance matrix.
    pub fn build_g_dc(&self) -> Vec<Vec<f64>> {
        self.dc_network
            .build_conductance_matrix(self.dc_network.n_buses())
    }

    /// Find the index of the DC slack bus.
    fn slack_dc_bus_idx(&self) -> Option<usize> {
        Some(self.dc_network.slack_dc_bus)
    }
}

// ─── Result types ─────────────────────────────────────────────────────────────

/// Result for a single LCC converter after the hybrid MTDC solve.
#[derive(Debug, Clone)]
pub struct LccConverterResult {
    /// AC bus number.
    pub bus_ac: u32,
    /// AC real power injection in MW.
    ///
    /// Negative = draws from AC (rectifier); positive = injects into AC (inverter).
    pub p_ac_mw: f64,
    /// AC reactive power injection in MVAR.
    ///
    /// LCC converters always absorb reactive power → q_ac_mvar ≤ 0.
    pub q_ac_mvar: f64,
    /// DC power in MW with sign convention: positive = power into the DC network.
    pub p_dc_mw: f64,
    /// DC bus voltage in per-unit.
    pub v_dc_pu: f64,
    /// DC current in per-unit (magnitude; sign is implicit in rectifier/inverter mode).
    pub i_dc_pu: f64,
    /// Firing angle α in degrees (rectifier) or extinction angle γ in degrees (inverter).
    pub alpha_deg: f64,
    /// Extinction angle γ in degrees (inverter mode only; 0.0 for rectifier).
    pub gamma_deg: f64,
    /// Commutation power factor cos(φ).
    pub power_factor: f64,
}

/// Result for a single VSC converter after the hybrid MTDC solve.
#[derive(Debug, Clone)]
pub struct VscConverterResult {
    /// AC bus number.
    pub bus_ac: u32,
    /// AC real power injection in MW (positive = injection into AC).
    pub p_ac_mw: f64,
    /// AC reactive power injection in MVAR.
    pub q_ac_mvar: f64,
    /// DC power in MW (positive = power flowing from AC to DC side).
    pub p_dc_mw: f64,
    /// DC bus voltage in per-unit.
    pub v_dc_pu: f64,
    /// DC current in per-unit (signed: positive into DC network).
    pub i_dc_pu: f64,
    /// Converter losses in MW.
    pub losses_mw: f64,
}

/// Result of a hybrid MTDC power flow solve.
#[derive(Debug, Clone)]
pub struct HybridMtdcResult {
    /// DC bus voltages in per-unit (indexed 0..n_dc_buses).
    pub dc_voltages_pu: Vec<f64>,
    /// Per-LCC converter operating results.
    pub lcc_results: Vec<LccConverterResult>,
    /// Per-VSC converter operating results.
    pub vsc_results: Vec<VscConverterResult>,
    /// Total DC network losses in MW (cables + shunts + ground returns).
    pub total_dc_loss_mw: f64,
    /// True if the Newton-Raphson iteration converged.
    pub converged: bool,
    /// Number of NR iterations taken.
    pub iterations: u32,
}

// ─── Solver ───────────────────────────────────────────────────────────────────

/// Solve the hybrid LCC/VSC MTDC power flow.
///
/// Uses a Newton-Raphson iteration on the DC KCL equations. LCC converters
/// are modelled as constant-power current sources; VSC converters are
/// also constant-power sources. The LCC firing angle is derived analytically
/// from the converged DC bus voltage (not an independent NR unknown).
///
/// # Arguments
///
/// * `network`     — Hybrid MTDC network (LCC + VSC converters on shared DC buses).
/// * `ac_voltages` — Pre-solved AC bus complex voltages in pu, indexed by AC
///   bus number with index `0` unused. Used to look up `|V_ac|` for each
///   converter by `ac_voltages[bus_ac as usize]`.
/// * `max_iter`    — Maximum NR iterations (default 50).
/// * `tol`         — Convergence tolerance on KCL mismatch in pu (default 1e-6).
///
/// # Returns
///
/// `HybridMtdcResult` with converged DC voltages and per-converter results.
///
/// # Errors
///
/// Returns `HvdcError::NotConverged` if iteration fails to converge.
/// Returns `HvdcError::InvalidLink` for degenerate topology.
pub fn solve_hybrid_mtdc(
    network: &HybridMtdcNetwork,
    ac_voltages: &[Complex64],
    max_iter: u32,
    tol: f64,
) -> Result<HybridMtdcResult, HvdcError> {
    let n_dc = network.n_dc_buses();
    if n_dc == 0 {
        return Err(HvdcError::InvalidLink(
            "Hybrid MTDC network has no DC buses".to_string(),
        ));
    }

    let slack_idx = network.slack_dc_bus_idx().ok_or_else(|| {
        HvdcError::InvalidLink("Hybrid MTDC network has no DC slack bus".to_string())
    })?;

    info!(
        n_dc_buses = n_dc,
        n_dc_branches = network.dc_network.branches.len(),
        n_lcc = network.lcc_converters.len(),
        n_vsc = network.vsc_converters.len(),
        slack_dc_bus = slack_idx,
        max_iter = max_iter,
        tol = tol,
        "Hybrid MTDC power flow starting"
    );

    let base_mva = network.base_mva;

    // Helper: extract |V_ac| at converter AC bus.
    // Convention: bus_ac is the external bus number, used directly as the
    // index in `ac_voltages`, with entry 0 intentionally unused.
    let v_ac_pu = |bus_ac: u32| -> f64 {
        let idx = bus_ac as usize;
        if idx < ac_voltages.len() {
            ac_voltages[idx].norm()
        } else {
            1.0 // flat-start fallback
        }
    };

    // Initialise DC bus voltages.
    let mut v_dc: Vec<f64> = network.dc_network.v_dc.clone();
    v_dc[slack_idx] = network.dc_network.v_dc_slack;

    // Build DC conductance matrix (constant throughout iteration).
    let g_dc = network.build_g_dc();

    // Free buses: all non-slack DC buses (the NR unknowns).
    let free_buses: Vec<usize> = (0..n_dc).filter(|&i| i != slack_idx).collect();
    let n_free = free_buses.len();

    // Collect in-service converters.
    let lcc_active: Vec<&LccConverter> = network
        .lcc_converters
        .iter()
        .filter(|c| c.in_service)
        .collect();
    let vsc_active: Vec<&HybridVscConverter> = network
        .vsc_converters
        .iter()
        .filter(|c| c.in_service)
        .collect();

    // ── Newton-Raphson iteration ──────────────────────────────────────────────
    //
    // Unknowns: V_dc[k] for each free bus k.
    //
    // Mismatch: f_k = Σ_j G_kj * V_dc_j − I_net_k(V_dc_k)
    //
    // where I_net_k = Σ_converters_at_k  P_set / (V_dc_k * base_mva)
    //
    // Jacobian:
    //   ∂f_k/∂V_dc_k = G_kk − ∂I_net_k/∂V_dc_k
    //                 = G_kk + Σ_conv P_set / (V_dc_k² * base_mva)
    //   ∂f_k/∂V_dc_m = G_km  (m ≠ k)

    let mut converged = false;
    let mut iterations = 0u32;
    let mut max_f_final = 0.0_f64;

    for _iter in 0..max_iter {
        iterations += 1;

        // Compute net current injection at each DC bus (pu).
        // Sign: positive = power injected into DC network.
        let i_net: Vec<f64> = (0..n_dc)
            .map(|k| {
                let v_k = v_dc[k];
                let i_lcc: f64 = lcc_active
                    .iter()
                    .filter(|lcc| lcc.bus_dc == k)
                    .map(|lcc| lcc.i_dc_pu(v_k, base_mva))
                    .sum();
                let i_vsc: f64 = vsc_active
                    .iter()
                    .filter(|vsc| vsc.bus_dc == k)
                    .map(|vsc| vsc.i_dc_pu(v_k, base_mva))
                    .sum();
                i_lcc + i_vsc
            })
            .collect();

        // KCL mismatch at each free bus: f_k = (G*V)_k − I_net_k
        let mut f = vec![0.0f64; n_free];
        for (fi, &k) in free_buses.iter().enumerate() {
            let gv_k: f64 = (0..n_dc).map(|j| g_dc[k][j] * v_dc[j]).sum();
            f[fi] = gv_k - i_net[k];
        }

        // Convergence check.
        max_f_final = f.iter().copied().fold(0.0_f64, |a, b| a.max(b.abs()));
        debug!(
            iteration = iterations,
            max_kcl_mismatch_pu = max_f_final,
            tol = tol,
            "Hybrid MTDC NR iteration"
        );
        if max_f_final < tol {
            converged = true;
            break;
        }

        // Build Jacobian (n_free × n_free).
        let mut jac = vec![vec![0.0f64; n_free]; n_free];
        for (fi, &k) in free_buses.iter().enumerate() {
            let v_k = v_dc[k];
            // Total power injection at bus k (pu) for Jacobian correction.
            let p_inj_pu: f64 = lcc_active
                .iter()
                .filter(|lcc| lcc.bus_dc == k)
                .map(|lcc| lcc.p_setpoint_mw / base_mva)
                .sum::<f64>()
                + vsc_active
                    .iter()
                    .filter(|vsc| !vsc.is_dc_slack && vsc.bus_dc == k)
                    .map(|vsc| vsc.p_setpoint_mw / base_mva)
                    .sum::<f64>();

            // ∂f_k/∂V_dc_k = G_kk − ∂I_net_k/∂V_dc_k
            // ∂I_net_k/∂V_dc_k = −P_inj_pu / V_dc_k²  (since I = P / V)
            jac[fi][fi] = g_dc[k][k] + p_inj_pu / (v_k * v_k + 1e-15);

            // Off-diagonal: ∂f_k/∂V_dc_m = G_km.
            for (fj, &m) in free_buses.iter().enumerate() {
                if fj != fi {
                    jac[fi][fj] = g_dc[k][m];
                }
            }
        }

        // Solve J * Δv = −f.
        let rhs: Vec<f64> = f.iter().map(|&x| -x).collect();
        let delta = solve_system(&jac, &rhs);

        // Apply voltage updates with clamping.
        for (fi, &k) in free_buses.iter().enumerate() {
            v_dc[k] += delta[fi];
            if v_dc[k] < 0.01 {
                v_dc[k] = 0.01;
            }
        }
    }

    if !converged {
        warn!(
            iterations = iterations,
            max_iter = max_iter,
            max_kcl_mismatch_pu = max_f_final,
            max_delta = max_f_final * base_mva,
            "Hybrid MTDC power flow did not converge"
        );
        return Err(HvdcError::NotConverged {
            iterations,
            max_delta: max_f_final * base_mva,
        });
    }

    // ── Build per-converter results ───────────────────────────────────────────

    let lcc_results: Vec<LccConverterResult> = network
        .lcc_converters
        .iter()
        .map(|lcc| {
            if !lcc.in_service {
                return LccConverterResult {
                    bus_ac: lcc.bus_ac,
                    p_ac_mw: 0.0,
                    q_ac_mvar: 0.0,
                    p_dc_mw: 0.0,
                    v_dc_pu: 0.0,
                    i_dc_pu: 0.0,
                    alpha_deg: 0.0,
                    gamma_deg: 0.0,
                    power_factor: 0.0,
                };
            }

            let v_k = v_dc[lcc.bus_dc];
            let v_ac = v_ac_pu(lcc.bus_ac);
            let i_dc = lcc.i_dc_pu(v_k, base_mva).abs();

            // Derive firing/extinction angle from bridge equation.
            let alpha_rad = lcc.firing_angle_rad(v_k, v_ac, i_dc);
            let alpha_deg = alpha_rad.to_degrees();

            // Extinction angle (inverter mode only).
            let gamma_deg = if !lcc.is_rectifier() {
                lcc.extinction_angle_rad(v_k, v_ac, i_dc).to_degrees()
            } else {
                0.0
            };

            // Commutation power factor.
            let v_dc_ideal = K_BRIDGE * v_ac;
            let cos_phi = if v_dc_ideal > 1e-9 {
                (v_k / v_dc_ideal).clamp(0.0, 1.0)
            } else {
                0.9
            };
            let phi = cos_phi.acos();
            let tan_phi = phi.tan();

            // DC power (signed: positive = into DC network).
            let p_dc_mw = lcc.p_setpoint_mw;

            // AC real power: opposite sign to DC.
            let p_ac_mw = -p_dc_mw;

            // Q: LCC always absorbs reactive power (negative injection into AC).
            let q_ac_mvar = -p_dc_mw.abs() * tan_phi;

            LccConverterResult {
                bus_ac: lcc.bus_ac,
                p_ac_mw,
                q_ac_mvar,
                p_dc_mw,
                v_dc_pu: v_k,
                i_dc_pu: i_dc,
                alpha_deg,
                gamma_deg,
                power_factor: cos_phi,
            }
        })
        .collect();

    let vsc_results: Vec<VscConverterResult> = network
        .vsc_converters
        .iter()
        .map(|vsc| {
            if !vsc.in_service {
                return VscConverterResult {
                    bus_ac: vsc.bus_ac,
                    p_ac_mw: 0.0,
                    q_ac_mvar: 0.0,
                    p_dc_mw: 0.0,
                    v_dc_pu: 0.0,
                    i_dc_pu: 0.0,
                    losses_mw: 0.0,
                };
            }

            let v_k = v_dc[vsc.bus_dc];
            let v_ac = v_ac_pu(vsc.bus_ac);

            let p_dc_mw = if vsc.is_dc_slack {
                // Back-calculate from DC KCL at the slack bus.
                let i_net_slack: f64 = (0..n_dc).map(|j| g_dc[slack_idx][j] * v_dc[j]).sum();
                // i_net_slack is the net current flowing away from slack into the network.
                // P_dc at slack = V_slack * I_net (power absorbed by the network from slack).
                -i_net_slack * v_dc[slack_idx] * base_mva
            } else {
                vsc.p_setpoint_mw
            };

            let i_dc_pu = if v_k.abs() > 1e-9 {
                p_dc_mw / (v_k * base_mva)
            } else {
                0.0
            };

            let losses = vsc.losses_mw(p_dc_mw, vsc.q_setpoint_mvar.abs(), v_ac, base_mva);
            // AC injection: inverter (p_dc < 0) → positive AC injection.
            let p_ac_mw = -p_dc_mw - losses;
            let q_ac_mvar = vsc.q_setpoint_mvar.clamp(vsc.q_min_mvar, vsc.q_max_mvar);

            VscConverterResult {
                bus_ac: vsc.bus_ac,
                p_ac_mw,
                q_ac_mvar,
                p_dc_mw,
                v_dc_pu: v_k,
                i_dc_pu,
                losses_mw: losses,
            }
        })
        .collect();

    // DC branch and bus conductance losses.
    let branch_loss_mw: f64 = network
        .dc_network
        .branches
        .iter()
        .map(|br| {
            let v_from = v_dc[br.from_dc_bus];
            let v_to = v_dc[br.to_dc_bus];
            let g = br.conductance();
            let i_branch = g * (v_from - v_to);
            i_branch * i_branch / g * base_mva
        })
        .sum();
    let shunt_and_ground_loss_mw: f64 = network
        .dc_network
        .g_shunt_pu
        .iter()
        .zip(network.dc_network.g_ground_pu.iter())
        .zip(v_dc.iter())
        .map(|((&g_sh, &g_gr), &v)| (g_sh + g_gr) * v * v * base_mva)
        .sum();
    let total_dc_loss_mw = branch_loss_mw + shunt_and_ground_loss_mw;

    info!(
        iterations = iterations,
        converged = converged,
        n_lcc_results = lcc_results.len(),
        n_vsc_results = vsc_results.len(),
        total_dc_loss_mw = total_dc_loss_mw,
        "Hybrid MTDC power flow complete"
    );

    Ok(HybridMtdcResult {
        dc_voltages_pu: v_dc,
        lcc_results,
        vsc_results,
        total_dc_loss_mw,
        converged,
        iterations,
    })
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dc_network::topology::DcBranch;
    use num_complex::Complex64;

    /// Helper: flat AC voltage array of n buses at 1.0 pu / 0° phase.
    fn flat_ac(n: usize) -> Vec<Complex64> {
        vec![Complex64::new(1.0, 0.0); n]
    }

    #[test]
    fn hybrid_vsc_losses_use_direction_specific_quadratic_coefficients() {
        let mut converter = HybridVscConverter::new(1, 0, 100.0);
        converter.loss_linear = 0.01;
        converter.loss_quadratic_rectifier = 0.02;
        converter.loss_quadratic_inverter = 0.05;

        let rectifier_loss = converter.losses_mw(100.0, 0.0, 1.0, 100.0);
        let inverter_loss = converter.losses_mw(-100.0, 0.0, 1.0, 100.0);

        assert!(inverter_loss > rectifier_loss);
        assert!((rectifier_loss - 3.0).abs() < 1e-12);
        assert!((inverter_loss - 6.0).abs() < 1e-12);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3-terminal hybrid: 1 LCC rectifier + 2 VSC inverters
    //
    // To enforce correct power balance, the DC slack is at bus 0 (no converter).
    // All converters sit at non-slack buses so their setpoints are enforced.
    //
    // Topology:
    //   DC Bus 0 (slack, V=1.0)  — no converter (voltage reference only)
    //   DC Bus 1 (free)          — LCC rectifier   +380 MW
    //   DC Bus 2 (free)          — VSC inverter 1  −200 MW
    //   DC Bus 3 (free)          — VSC inverter 2  −180 MW
    //   Star cables:  0→1, 0→2, 0→3  R = 1e-4 pu (near-lossless)
    //
    // Power balance: +380 − 200 − 180 = 0 MW in, losses ≈ 0. ✓
    // ─────────────────────────────────────────────────────────────────────────

    fn build_3terminal_hybrid() -> (HybridMtdcNetwork, Vec<Complex64>) {
        let mut net = HybridMtdcNetwork::new(100.0, 4, 0);
        net.dc_network.v_dc_slack = 1.0;

        // Star cables from slack hub.
        for to in [1, 2, 3] {
            net.dc_network.add_branch(DcBranch {
                from_dc_bus: 0,
                to_dc_bus: to,
                r_dc_pu: 1e-4,
                i_max_pu: 0.0,
            });
        }

        // LCC rectifier: +380 MW into DC network.
        let mut lcc = LccConverter::new(1, 1, 400.0);
        lcc.p_setpoint_mw = 380.0;
        lcc.x_commutation_pu = 0.15;
        net.lcc_converters.push(lcc);

        // VSC inverter 1: −200 MW (draws from DC, injects into AC).
        let vsc1 = HybridVscConverter::new(2, 2, -200.0);
        net.vsc_converters.push(vsc1);

        // VSC inverter 2: −180 MW.
        let vsc2 = HybridVscConverter::new(3, 3, -180.0);
        net.vsc_converters.push(vsc2);

        (net, flat_ac(3))
    }

    #[test]
    fn hybrid_3terminal_converges() {
        let (net, ac_v) = build_3terminal_hybrid();
        let result = solve_hybrid_mtdc(&net, &ac_v, 50, 1e-6)
            .expect("3-terminal hybrid MTDC should converge");

        assert!(
            result.converged,
            "Hybrid MTDC must converge; iterations = {}",
            result.iterations
        );
        assert!(
            result.iterations <= 50,
            "Should converge in ≤50 iterations, took {}",
            result.iterations
        );
    }

    #[test]
    fn hybrid_3terminal_dc_voltages_realistic() {
        let (net, ac_v) = build_3terminal_hybrid();
        let result = solve_hybrid_mtdc(&net, &ac_v, 50, 1e-6).unwrap();

        for (i, &v) in result.dc_voltages_pu.iter().enumerate() {
            assert!(
                v > 0.9 && v < 1.1,
                "DC bus {i} voltage {v:.4} pu is outside expected [0.9, 1.1] range"
            );
        }
    }

    #[test]
    fn hybrid_3terminal_power_balance() {
        let (net, ac_v) = build_3terminal_hybrid();
        let result = solve_hybrid_mtdc(&net, &ac_v, 50, 1e-6).unwrap();

        // All converter setpoints are at non-slack buses (enforced by KCL).
        // P_lcc (+380) + P_vsc1 (-200) + P_vsc2 (-180) + losses ≈ 0.
        let p_lcc_dc: f64 = result.lcc_results.iter().map(|r| r.p_dc_mw).sum();
        let p_vsc_dc: f64 = result.vsc_results.iter().map(|r| r.p_dc_mw).sum();
        let balance = p_lcc_dc + p_vsc_dc + result.total_dc_loss_mw;

        assert!(
            balance.abs() < 5.0,
            "Power balance error {balance:.2} MW (> 5 MW). \
             P_lcc_dc={p_lcc_dc:.2}, P_vsc_dc={p_vsc_dc:.2}, losses={:.2}",
            result.total_dc_loss_mw
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // LCC extinction angle test: 1 VSC rectifier + 1 LCC inverter
    //
    // Slack is at bus 0 (no converter).
    // VSC rectifier at bus 1, LCC inverter at bus 2.
    // ─────────────────────────────────────────────────────────────────────────

    fn build_lcc_inverter_test() -> (HybridMtdcNetwork, Vec<Complex64>) {
        let mut net = HybridMtdcNetwork::new(100.0, 3, 0);
        net.dc_network.v_dc_slack = 1.0;

        for (f, t) in [(0, 1), (0, 2), (1, 2)] {
            net.dc_network.add_branch(DcBranch {
                from_dc_bus: f,
                to_dc_bus: t,
                r_dc_pu: 0.01,
                i_max_pu: 0.0,
            });
        }

        // VSC rectifier at AC bus 1, DC bus 1. +280 MW into DC.
        let vsc = HybridVscConverter::new(1, 1, 280.0);
        net.vsc_converters.push(vsc);

        // LCC inverter at AC bus 2, DC bus 2. −280 MW from DC into AC.
        let mut lcc = LccConverter::new(2, 2, 280.0);
        lcc.p_setpoint_mw = -280.0;
        lcc.gamma_min_deg = 15.0;
        lcc.alpha_min_deg = 5.0;
        lcc.alpha_max_deg = 150.0;
        lcc.x_commutation_pu = 0.12;
        net.lcc_converters.push(lcc);

        (net, flat_ac(2))
    }

    #[test]
    fn lcc_inverter_extinction_angle_satisfied() {
        let (net, ac_v) = build_lcc_inverter_test();
        let result = solve_hybrid_mtdc(&net, &ac_v, 50, 1e-6).unwrap();

        assert!(result.converged, "LCC inverter test must converge");

        let lcc_res = &result.lcc_results[0];
        let gamma_min = net.lcc_converters[0].gamma_min_deg;

        assert!(
            lcc_res.gamma_deg >= gamma_min - 0.1,
            "Extinction angle γ={:.2}° must be ≥ γ_min={:.2}°",
            lcc_res.gamma_deg,
            gamma_min
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4-terminal mixed: 2 LCC rectifiers + 2 VSC inverters
    //
    // Slack is at bus 0 (no converter). Balanced: +500 in, −500 out.
    //   DC Bus 0 (slack)  — no converter
    //   DC Bus 1 (free)   — LCC rectifier 1  +250 MW
    //   DC Bus 2 (free)   — LCC rectifier 2  +250 MW
    //   DC Bus 3 (free)   — VSC inverter 1   −250 MW
    //   DC Bus 4 (free)   — VSC inverter 2   −250 MW
    //   Ring cables: 0→1, 1→2, 2→3, 3→4, 4→0   R = 1e-3 pu
    // ─────────────────────────────────────────────────────────────────────────

    fn build_4terminal_mixed() -> (HybridMtdcNetwork, Vec<Complex64>) {
        let mut net = HybridMtdcNetwork::new(100.0, 5, 0);
        net.dc_network.v_dc_slack = 1.0;

        // Ring topology.
        for (from, to) in [(0usize, 1), (1, 2), (2, 3), (3, 4), (4, 0)] {
            net.dc_network.add_branch(DcBranch {
                from_dc_bus: from,
                to_dc_bus: to,
                r_dc_pu: 1e-3,
                i_max_pu: 0.0,
            });
        }

        let mut lcc1 = LccConverter::new(1, 1, 250.0);
        lcc1.p_setpoint_mw = 250.0;
        lcc1.x_commutation_pu = 0.15;
        net.lcc_converters.push(lcc1);

        let mut lcc2 = LccConverter::new(2, 2, 250.0);
        lcc2.p_setpoint_mw = 250.0;
        lcc2.x_commutation_pu = 0.15;
        net.lcc_converters.push(lcc2);

        let vsc1 = HybridVscConverter::new(3, 3, -250.0);
        net.vsc_converters.push(vsc1);

        let vsc2 = HybridVscConverter::new(4, 4, -250.0);
        net.vsc_converters.push(vsc2);

        (net, flat_ac(4))
    }

    #[test]
    fn hybrid_4terminal_converges() {
        let (net, ac_v) = build_4terminal_mixed();
        let result = solve_hybrid_mtdc(&net, &ac_v, 100, 1e-6)
            .expect("4-terminal hybrid MTDC should converge");

        assert!(
            result.converged,
            "4-terminal MTDC must converge; iterations = {}",
            result.iterations
        );
    }

    #[test]
    fn hybrid_4terminal_dc_voltages_realistic() {
        let (net, ac_v) = build_4terminal_mixed();
        let result = solve_hybrid_mtdc(&net, &ac_v, 100, 1e-6).unwrap();

        for (i, &v) in result.dc_voltages_pu.iter().enumerate() {
            assert!(
                v > 0.85 && v < 1.15,
                "DC bus {i} voltage {v:.4} pu outside expected [0.85, 1.15]"
            );
        }
    }

    #[test]
    fn hybrid_4terminal_power_balance() {
        let (net, ac_v) = build_4terminal_mixed();
        let result = solve_hybrid_mtdc(&net, &ac_v, 100, 1e-6).unwrap();

        // All converters at non-slack buses: setpoints are enforced.
        // Balanced: +250+250−250−250 = 0 MW, losses ≈ small.
        let p_lcc_dc: f64 = result.lcc_results.iter().map(|r| r.p_dc_mw).sum();
        let p_vsc_dc: f64 = result.vsc_results.iter().map(|r| r.p_dc_mw).sum();
        let balance = p_lcc_dc + p_vsc_dc + result.total_dc_loss_mw;

        assert!(
            balance.abs() < 10.0,
            "4-terminal power balance error {balance:.2} MW. \
             P_lcc_dc={p_lcc_dc:.2}, P_vsc_dc={p_vsc_dc:.2}, losses={:.2}",
            result.total_dc_loss_mw
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // DC slack bus voltage is held at setpoint
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn dc_slack_bus_voltage_is_held() {
        let (net, ac_v) = build_3terminal_hybrid();
        let result = solve_hybrid_mtdc(&net, &ac_v, 50, 1e-6).unwrap();

        let v_slack = result.dc_voltages_pu[0];
        assert!(
            (v_slack - 1.0).abs() < 1e-9,
            "DC slack bus must be held at 1.0 pu, got {v_slack:.8}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // LCC reactive power is always absorbed (Q injection into AC ≤ 0)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn lcc_always_absorbs_reactive_power() {
        let (net, ac_v) = build_3terminal_hybrid();
        let result = solve_hybrid_mtdc(&net, &ac_v, 50, 1e-6).unwrap();

        for (i, lcc_res) in result.lcc_results.iter().enumerate() {
            assert!(
                lcc_res.q_ac_mvar <= 0.0,
                "LCC converter {i} must absorb reactive power; \
                 got Q_ac = {:.2} MVAR (should be ≤ 0)",
                lcc_res.q_ac_mvar
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // LCC firing angle within physical limits
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn lcc_firing_angle_within_limits() {
        let (net, ac_v) = build_4terminal_mixed();
        let result = solve_hybrid_mtdc(&net, &ac_v, 100, 1e-6).unwrap();

        for (i, (lcc_res, lcc_conv)) in result
            .lcc_results
            .iter()
            .zip(net.lcc_converters.iter())
            .enumerate()
        {
            assert!(
                lcc_res.alpha_deg >= lcc_conv.alpha_min_deg - 0.1,
                "LCC {i} firing angle {:.2}° < α_min={:.2}°",
                lcc_res.alpha_deg,
                lcc_conv.alpha_min_deg
            );
            assert!(
                lcc_res.alpha_deg <= lcc_conv.alpha_max_deg + 0.1,
                "LCC {i} firing angle {:.2}° > α_max={:.2}°",
                lcc_res.alpha_deg,
                lcc_conv.alpha_max_deg
            );
        }
    }
}
