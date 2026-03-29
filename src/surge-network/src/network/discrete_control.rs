// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Discrete voltage control devices: OLTC transformers and switched shunts.
//!
//! These models represent equipment that regulates bus voltage by taking
//! discrete steps (tap changes or capacitor/reactor bank switching) rather
//! than continuously varying a setpoint.  Because discrete steps cannot be
//! expressed as differentiable constraints in the Newton-Raphson Jacobian,
//! they are handled in an outer control loop that re-solves NR after each
//! round of tap/shunt adjustments.

use serde::{Deserialize, Serialize};
use tracing::debug;

/// On-Load Tap Changer (OLTC) control data for a transformer branch.
///
/// An OLTC regulates the voltage at a remote (or local) bus by stepping its
/// tap ratio in discrete increments.  After Newton-Raphson converges, the
/// solver checks each OLTC transformer:
///
/// 1. If `|vm[bus_regulated] - v_target| > v_band / 2`, tap is stepped toward
///    the target and NR is re-solved.
/// 2. Steps are bounded by `[tap_min, tap_max]`.
/// 3. The loop terminates when all regulated bus voltages are within band or
///    `oltc_max_iter` outer iterations are exhausted.
///
/// The `branch_index` field refers to the 0-based index into
/// `PowerNetwork::branches`.  The tap adjustment modifies
/// `branches[branch_index].tap` in place before each NR re-solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OltcControl {
    /// 0-based index into `Network::branches` for the transformer being controlled.
    pub branch_index: usize,
    /// 0-based index into `Network::buses` for the bus whose voltage is regulated.
    ///
    /// May differ from the transformer's from/to bus for remote voltage control.
    pub bus_regulated: usize,
    /// Voltage target in per-unit.
    pub v_target: f64,
    /// Dead-band half-width in per-unit (control activates when |V - v_target| > v_band / 2).
    pub v_band: f64,
    /// Minimum allowable tap ratio in per-unit (e.g. 0.9).
    pub tap_min: f64,
    /// Maximum allowable tap ratio in per-unit (e.g. 1.1).
    pub tap_max: f64,
    /// Discrete tap step size in per-unit (e.g. 0.00625 = 1/160, typical 16-step OLTC).
    pub tap_step: f64,
}

impl OltcControl {
    /// Create a standard OLTC with symmetric ±10 % range and 16 tap steps per side.
    ///
    /// `branch_index` — 0-based index of the transformer in `Network::branches`.
    /// `bus_regulated` — 0-based bus index to regulate.
    pub fn standard(branch_index: usize, bus_regulated: usize, v_target: f64) -> Self {
        debug!(
            branch_index,
            bus_regulated, v_target, "creating standard OLTC control"
        );
        Self {
            branch_index,
            bus_regulated,
            v_target,
            v_band: 0.01, // ±0.005 p.u. dead-band
            tap_min: 0.9,
            tap_max: 1.1,
            tap_step: 0.00625, // 1/160 — 16 steps over ±10 %
        }
    }
}

/// Switched shunt (capacitor/reactor bank) discrete voltage control.
///
/// A switched shunt injects reactive power in discrete MVAr steps to regulate
/// bus voltage.  Capacitor banks raise voltage (positive susceptance); reactor
/// banks lower voltage (negative susceptance).
///
/// The total shunt susceptance injected is:
///   `B_total = b_step × n_active_steps`
///
/// where `n_active_steps` is in `[-n_steps_react, n_steps_cap]`.  The shunt
/// susceptance is added to `buses[bus].shunt_susceptance_mvar` before each NR re-solve
/// after converting from p.u. to MVAr using the network base MVA.
///
/// After NR converges, if `vm[bus_regulated]` is outside the voltage band, the
/// solver increments or decrements `n_active_steps` by 1 and re-solves until
/// the voltage is within band or the step limit is reached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchedShunt {
    /// Stable switched-shunt identifier.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// External bus number for the bus that hosts this shunt (where the
    /// susceptance is injected).
    pub bus: u32,
    /// External bus number whose voltage is regulated.
    ///
    /// Equal to `bus` for local regulation (the common case). May differ for
    /// remote voltage regulation (PSS/E `SWREM` field points to another bus).
    pub bus_regulated: u32,
    /// Susceptance per step in per-unit (must be positive; reactors use n_steps_react).
    pub b_step: f64,
    /// Maximum number of capacitor steps (positive susceptance, raises voltage).
    pub n_steps_cap: i32,
    /// Maximum number of reactor steps (negative susceptance, lowers voltage).
    pub n_steps_react: i32,
    /// Voltage target in per-unit.
    pub v_target: f64,
    /// Dead-band half-width in per-unit (control activates when |V - v_target| > v_band / 2).
    pub v_band: f64,
    /// Current number of active steps (positive = capacitor, negative = reactor).
    /// Initialised to 0 (all banks open).
    pub n_active_steps: i32,
}

impl SwitchedShunt {
    /// Create a capacitor-only switched shunt with `n_steps` equal-sized banks.
    ///
    /// `bus` — external bus number.
    /// `b_total_cap_pu` — total capacitive susceptance in per-unit at full switching.
    /// `n_steps` — number of discrete steps (banks).
    pub fn capacitor_only(bus: u32, b_total_cap_pu: f64, n_steps: i32, v_target: f64) -> Self {
        let b_step = if n_steps > 0 {
            b_total_cap_pu / n_steps as f64
        } else {
            b_total_cap_pu
        };
        Self {
            id: String::new(),
            bus,
            bus_regulated: bus,
            b_step,
            n_steps_cap: n_steps,
            n_steps_react: 0,
            v_target,
            v_band: 0.02,
            n_active_steps: 0,
        }
    }

    /// Total susceptance currently injected in per-unit.
    #[inline]
    pub fn b_injected(&self) -> f64 {
        let b = self.b_step * self.n_active_steps as f64;
        debug!(
            bus = self.bus,
            n_active_steps = self.n_active_steps,
            b_injected = b,
            "switched shunt reactive injection"
        );
        b
    }
}

/// OLTC specification stored in the network model, using external bus numbers.
///
/// Populated by the PSS/E parser from transformer control fields (COD1 = 1 or 2).
/// At solve time the NR wrapper converts each `OltcSpec` to an [`OltcControl`]
/// (0-indexed) so the discrete-control outer loop can act on it directly.
///
/// The regulated bus is the external bus whose voltage is held within
/// `[v_target − v_band/2, v_target + v_band/2]`. A `regulated_bus` of zero
/// means *local* regulation (the transformer's to-bus).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OltcSpec {
    /// External bus number of the transformer from-bus.
    pub from_bus: u32,
    /// External bus number of the transformer to-bus.
    pub to_bus: u32,
    /// Circuit identifier string.
    pub circuit: String,
    /// External bus number whose voltage is regulated (0 → to-bus).
    pub regulated_bus: u32,
    /// Voltage target in per-unit.
    pub v_target: f64,
    /// Dead-band full-width in per-unit (control activates when |V − v_target| > v_band/2).
    pub v_band: f64,
    /// Minimum allowable tap ratio in per-unit.
    pub tap_min: f64,
    /// Maximum allowable tap ratio in per-unit.
    pub tap_max: f64,
    /// Discrete tap step size in per-unit.
    pub tap_step: f64,
}

/// Phase Angle Regulator (PAR) specification stored in the network model.
///
/// Populated by the PSS/E parser from transformer control fields (COD1 = 3).
/// At solve time the NR wrapper converts each `ParSpec` to a [`ParControl`]
/// (0-indexed) so the discrete-control outer loop can act on it.
///
/// The PAR adjusts its phase-shift angle (ANG, degrees) in discrete steps to
/// drive active power flow on the monitored branch toward a target band
/// `[p_target_mw − p_band_mw/2, p_target_mw + p_band_mw/2]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParSpec {
    /// External bus number of the PAR transformer from-bus.
    pub from_bus: u32,
    /// External bus number of the PAR transformer to-bus.
    pub to_bus: u32,
    /// Circuit identifier string.
    pub circuit: String,
    /// External bus number of the monitored branch from-bus
    /// (0 = monitor the PAR branch itself).
    pub monitored_from_bus: u32,
    /// External bus number of the monitored branch to-bus
    /// (0 = monitor the PAR branch itself).
    pub monitored_to_bus: u32,
    /// Circuit of the monitored branch (used when monitored branch ≠ PAR branch).
    pub monitored_circuit: String,
    /// Target active power flow in MW (midpoint of control band).
    pub p_target_mw: f64,
    /// Dead-band full-width in MW.
    pub p_band_mw: f64,
    /// Minimum phase-angle shift in degrees.
    #[serde(alias = "ang_min_deg")]
    pub angle_min_deg: f64,
    /// Maximum phase-angle shift in degrees.
    #[serde(alias = "ang_max_deg")]
    pub angle_max_deg: f64,
    /// Discrete step size in degrees.
    pub ang_step_deg: f64,
}

/// Phase Angle Regulator control data (0-indexed), consumed by the NR outer loop.
///
/// Created from a [`ParSpec`] by resolving external bus numbers to 0-based
/// branch indices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParControl {
    /// 0-based index into `Network::branches` for the PAR transformer.
    pub branch_index: usize,
    /// 0-based index into `Network::branches` for the monitored branch.
    ///
    /// Equals `branch_index` when the PAR branch itself is the monitored element.
    pub monitored_branch_index: usize,
    /// Target active power flow in MW.
    pub p_target_mw: f64,
    /// Dead-band full-width in MW.
    pub p_band_mw: f64,
    /// Minimum phase-angle shift in degrees.
    #[serde(alias = "ang_min_deg")]
    pub angle_min_deg: f64,
    /// Maximum phase-angle shift in degrees.
    #[serde(alias = "ang_max_deg")]
    pub angle_max_deg: f64,
    /// Discrete step size in degrees.
    pub ang_step_deg: f64,
}

/// Switched shunt for continuous OPF relaxation (AC-OPF co-optimization).
///
/// Rather than a discrete stepped model, this represents the same physical
/// capacitor/reactor bank as a continuous susceptance variable for the NLP.
/// After the NLP converges, the optimal `b_val` is rounded to the nearest
/// realizable discrete step via [`SwitchedShuntOpf::round_to_steps`].
///
/// This struct is used exclusively in the AC-OPF pipeline; the discrete-step
/// version ([`SwitchedShunt`]) drives the outer NR-based control loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchedShuntOpf {
    /// Stable switched-shunt identifier.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// External bus number hosting this shunt.
    pub bus: u32,
    /// Minimum susceptance (pu) — most inductive (negative for reactors).
    pub b_min_pu: f64,
    /// Maximum susceptance (pu) — most capacitive.
    pub b_max_pu: f64,
    /// Initial/current susceptance (pu) — used as NLP warm-start.
    pub b_init_pu: f64,
    /// Discrete step size (pu). Used for post-solve rounding.
    /// Set to 0 to skip rounding (continuous shunt).
    pub b_step_pu: f64,
}

impl SwitchedShuntOpf {
    /// Round the optimal continuous `b_val` to the nearest realizable step
    /// within `[b_min_pu, b_max_pu]`.
    ///
    /// If `b_step_pu <= 0`, returns `b_val` clamped to `[b_min_pu, b_max_pu]`
    /// without rounding (continuous shunt).
    pub fn round_to_steps(&self, b_val: f64) -> f64 {
        if self.b_step_pu <= 0.0 {
            return b_val.clamp(self.b_min_pu, self.b_max_pu);
        }
        let clamped = b_val.clamp(self.b_min_pu, self.b_max_pu);
        let n_steps = ((clamped - self.b_min_pu) / self.b_step_pu).round() as i64;
        (self.b_min_pu + n_steps as f64 * self.b_step_pu).clamp(self.b_min_pu, self.b_max_pu)
    }
}

/// Round a continuous tap ratio to the nearest discrete step within bounds.
///
/// If `tap_step <= 0`, returns `tap` clamped to `[tap_min, tap_max]` (continuous).
pub fn round_tap(tap: f64, tap_min: f64, tap_max: f64, tap_step: f64) -> f64 {
    if tap_step <= 0.0 {
        return tap.clamp(tap_min, tap_max);
    }
    let clamped = tap.clamp(tap_min, tap_max);
    let n = ((clamped - tap_min) / tap_step).round() as i64;
    (tap_min + n as f64 * tap_step).clamp(tap_min, tap_max)
}

/// Round a continuous phase shift (radians) to the nearest discrete step within bounds.
///
/// `step_deg` is the step size in degrees. If `step_deg <= 0`, returns `shift_rad`
/// clamped to `[min_rad, max_rad]` (continuous).
pub fn round_phase(shift_rad: f64, min_rad: f64, max_rad: f64, step_deg: f64) -> f64 {
    if step_deg <= 0.0 {
        return shift_rad.clamp(min_rad, max_rad);
    }
    let step_rad = step_deg.to_radians();
    let clamped = shift_rad.clamp(min_rad, max_rad);
    let n = ((clamped - min_rad) / step_rad).round() as i64;
    (min_rad + n as f64 * step_rad).clamp(min_rad, max_rad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_tap_exact_step() {
        // 16-step OLTC: step = 0.00625, range [0.9, 1.1]
        let step = 0.00625;
        // 0.953 should round to step 8.48 → nearest 8 → 0.9 + 8*0.00625 = 0.95
        assert!((round_tap(0.953, 0.9, 1.1, step) - 0.95).abs() < 1e-12);
        // Exact step value should be preserved
        assert!((round_tap(0.95, 0.9, 1.1, step) - 0.95).abs() < 1e-12);
        // Just above midpoint: 0.9535 → step index = (0.0535/0.00625) = 8.56 → round to 9 → 0.95625
        assert!((round_tap(0.9535, 0.9, 1.1, step) - 0.95625).abs() < 1e-12);
    }

    #[test]
    fn test_round_tap_continuous() {
        // step=0 means continuous: no rounding, just clamp
        assert!((round_tap(0.953, 0.9, 1.1, 0.0) - 0.953).abs() < 1e-12);
        // Out of bounds clamp
        assert!((round_tap(0.85, 0.9, 1.1, 0.0) - 0.9).abs() < 1e-12);
        assert!((round_tap(1.15, 0.9, 1.1, 0.0) - 1.1).abs() < 1e-12);
    }

    #[test]
    fn test_round_phase_exact_step() {
        // 1° step, range [-30°, 30°] in radians
        let min_rad = (-30.0_f64).to_radians();
        let max_rad = (30.0_f64).to_radians();
        let step_deg = 1.0;
        // 5.5° in radians → should round to 6° (nearest step from min)
        let val = (5.5_f64).to_radians();
        let rounded = round_phase(val, min_rad, max_rad, step_deg);
        // -30° + n*1° → n = round((5.5+30)/1) = 36 → -30+36 = 6°
        let expected = (6.0_f64).to_radians();
        assert!((rounded - expected).abs() < 1e-10);
    }

    #[test]
    fn test_round_phase_continuous() {
        let min_rad = (-30.0_f64).to_radians();
        let max_rad = (30.0_f64).to_radians();
        let val = (5.5_f64).to_radians();
        assert!((round_phase(val, min_rad, max_rad, 0.0) - val).abs() < 1e-12);
    }
}
