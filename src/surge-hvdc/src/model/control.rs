// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! VSC station control modes and droop control for MTDC systems.
//!
//! Implements PLAN-098/FPQ-55 (VSC station control modes) and
//! PLAN-099/FPQ-56 (VSC droop control for MTDC).
//!
//! # Control modes
//!
//! Each VSC station can operate in one of four modes:
//!
//! - `ConstantPQ`: fixed P and Q setpoints (default).
//! - `ConstantPVac`: fixed P setpoint, regulate AC bus voltage to a target.
//!   Q is adjusted within `[q_min, q_max]` to hold V_ac ≈ v_target;
//!   outside the capability band the station behaves as `ConstantPQ`.
//! - `ConstantVdc`: DC voltage slack — the DC bus voltage is fixed and P is
//!   determined by the DC power balance (used for the single DC slack station
//!   in an MTDC system).
//! - `PVdcDroop`: P/V_dc droop — the active power varies with DC bus voltage:
//!   `P = p_set + k_droop * (v_dc - v_dc_set)`, clamped to `[p_min, p_max]`.
//!   Used in MTDC systems to distribute DC voltage regulation across stations.

use serde::{Deserialize, Serialize};

// ─── LCC control modes ───────────────────────────────────────────────────────

/// Control mode for a Line-Commutated Converter (LCC) HVDC link.
///
/// In real HVDC systems the rectifier and inverter each run one of these modes.
/// The mode determines how the DC operating point is computed given the current
/// AC bus voltages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum LccHvdcControlMode {
    /// Constant DC power control (default).
    ///
    /// The rectifier firing angle α is adjusted to deliver `p_dc_mw` at the
    /// inverter end, subject to the minimum firing angle guard (`alpha_min_deg`
    /// in `LccHvdcLink`).  This is the standard mode for scheduled DC tie flows.
    #[default]
    ConstantPower,

    /// Constant DC current control.
    ///
    /// The DC current is held at `i_d_pu` (system-base per-unit).  The firing
    /// angle adjusts automatically; power varies with AC bus voltage.  Typically
    /// used as the primary rectifier control mode in multi-terminal DC systems.
    ConstantCurrent {
        /// DC current setpoint in system-base per-unit.
        /// `i_d_pu ≈ p_dc_mw / base_mva` at rated DC voltage.
        i_d_pu: f64,
    },

    /// Fixed firing angle (open-loop / manual control).
    ///
    /// The firing angle α is held at `alpha_deg`, bounded below by
    /// `LccHvdcLink::alpha_min_deg`.  Both DC current and power float with
    /// the AC bus voltages.  Used to model blocked / de-energising converters
    /// or bypass thyristors in fault studies.
    ConstantAlpha {
        /// Firing angle setpoint in degrees.
        alpha_deg: f64,
    },

    /// Voltage-Dependent Current Order Limiter (VDCOL).
    ///
    /// Reduces the DC current order as the rectifier-side DC voltage drops,
    /// preventing commutation failures during AC faults and aiding post-fault
    /// recovery.  The current order follows a piecewise-linear characteristic:
    ///
    /// ```text
    ///   Vd_R ≥ v_high  →  I_d = i_order_pu   (full order)
    ///   Vd_R ≤ v_low   →  I_d = i_min_pu     (minimum order)
    ///   otherwise      →  linear interpolation
    /// ```
    ///
    /// The solver iterates between the VDCOL lookup and the DC voltage
    /// equations until the current order converges.
    Vdcol {
        /// Full DC current order in system-base pu (applied above `v_high_pu`).
        i_order_pu: f64,
        /// DC voltage threshold above which the full current order is applied (pu).
        v_high_pu: f64,
        /// DC voltage threshold below which the minimum current order is applied (pu).
        v_low_pu: f64,
        /// Minimum DC current order in system-base pu (applied below `v_low_pu`).
        i_min_pu: f64,
    },
}

// ─── VSC control modes ───────────────────────────────────────────────────────

/// Operating mode for a VSC converter station.
///
/// The default mode is `ConstantPQ`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VscHvdcControlMode {
    /// Fixed active and reactive power setpoints.
    ///
    /// The converter injects exactly `p_set` MW and `q_set` MVAR into the AC
    /// network (subject to the converter Q limits stored in `VscHvdcLink`).
    ConstantPQ {
        /// Active power setpoint in MW (positive = injection into AC at inverter;
        /// for the rectifier this is the magnitude drawn from AC).
        p_set: f64,
        /// Reactive power setpoint in MVAR (positive = injection into AC).
        q_set: f64,
    },

    /// Fixed active power, regulate AC bus voltage to a target.
    ///
    /// The converter maintains `p_set` MW while adjusting Q within
    /// `[q_min, q_max]` to keep the AC terminal voltage close to `v_target`.
    /// When Q hits a limit the station switches to constant-Q (PQ) behaviour
    /// at that limit (analogous to PV→PQ switching in Newton-Raphson).
    ConstantPVac {
        /// Active power setpoint in MW.
        p_set: f64,
        /// AC voltage magnitude target in per-unit.
        v_target: f64,
        /// Voltage dead-band half-width in per-unit.
        ///
        /// No corrective Q action is taken when `|v_ac - v_target| <= v_band`.
        v_band: f64,
        /// Minimum reactive injection in MVAR (negative = absorption).
        q_min: f64,
        /// Maximum reactive injection in MVAR (positive = injection).
        q_max: f64,
    },

    /// DC voltage slack — the DC bus voltage is fixed.
    ///
    /// The converter absorbs or injects whatever active power is required to
    /// hold the DC bus voltage at `v_dc_target`.  In a sequential AC-DC
    /// iteration P is back-calculated from the DC power balance once the DC
    /// voltages of the other stations are known.  Q is fixed at `q_set`.
    ConstantVdc {
        /// DC bus voltage target in per-unit.
        v_dc_target: f64,
        /// Reactive power setpoint in MVAR.
        q_set: f64,
    },

    /// P/V_dc droop control for MTDC systems.
    ///
    /// The active power injection is a linear function of the DC bus voltage:
    ///
    /// ```text
    /// P = p_set + k_droop * (v_dc - v_dc_set)
    /// ```
    ///
    /// clamped to `[p_min, p_max]`.  The reactive power setpoint is inherited
    /// from `VscHvdcLink::q_from_mvar` / `q_to_mvar`.
    PVdcDroop {
        /// Base active power setpoint in MW (power at the nominal DC voltage).
        p_set: f64,
        /// Nominal DC voltage in per-unit (the voltage at which P = p_set).
        voltage_dc_setpoint_pu: f64,
        /// Droop gain in MW per per-unit voltage deviation (MW/pu).
        ///
        /// A positive `k_droop` means P increases when V_dc rises above
        /// `v_dc_set` (the converter absorbs more power from the DC network to
        /// resist over-voltage).
        k_droop: f64,
        /// Minimum active power in MW (clamp lower bound).
        p_min: f64,
        /// Maximum active power in MW (clamp upper bound).
        p_max: f64,
    },
}

impl VscHvdcControlMode {
    /// Return the base active power setpoint in MW.
    ///
    /// For `ConstantVdc` the setpoint is not fixed in advance; returns 0.0 as a
    /// placeholder (the actual P is determined by DC power balance).
    pub fn p_set_mw(&self) -> f64 {
        match self {
            VscHvdcControlMode::ConstantPQ { p_set, .. } => *p_set,
            VscHvdcControlMode::ConstantPVac { p_set, .. } => *p_set,
            VscHvdcControlMode::ConstantVdc { .. } => 0.0,
            VscHvdcControlMode::PVdcDroop { p_set, .. } => *p_set,
        }
    }

    /// Compute the effective active power injection in MW given DC bus voltage.
    ///
    /// For `PVdcDroop`, applies the droop equation and clamps to limits.
    /// For all other modes the DC voltage is not used and `p_set_mw` is returned.
    pub fn effective_p_mw(&self, v_dc_pu: f64) -> f64 {
        match self {
            VscHvdcControlMode::PVdcDroop {
                p_set,
                voltage_dc_setpoint_pu,
                k_droop,
                p_min,
                p_max,
            } => {
                let p = p_set + k_droop * (v_dc_pu - voltage_dc_setpoint_pu);
                p.clamp(*p_min, *p_max)
            }
            _ => self.p_set_mw(),
        }
    }

    /// Compute the effective reactive power injection in MVAR given the current
    /// AC bus voltage.
    ///
    /// For `ConstantPVac`, performs the simple proportional voltage regulation:
    ///
    /// ```text
    /// Q = Q_prev + k_v * (v_target - v_ac)
    /// ```
    ///
    /// where the proportional gain `k_v` is derived from the Q range divided by
    /// the allowed voltage excursion (2 × v_band), giving a droop-like response.
    /// Q is then clamped to `[q_min, q_max]`.  If `|v_ac - v_target| <= v_band`
    /// Q is returned unchanged (dead-band).
    ///
    /// For `ConstantPQ` and `ConstantVdc`, the reactive setpoint is returned
    /// directly.  For `PVdcDroop` the Q setpoint is taken from `q_fixed`.
    ///
    /// # Arguments
    /// * `v_ac`    — Current AC terminal voltage magnitude in pu
    /// * `q_prev`  — Previous Q injection in MVAR (used for incremental update)
    /// * `q_fixed` — Fallback Q setpoint from `VscHvdcLink` (used by droop/Vdc modes)
    pub fn effective_q_mvar(&self, v_ac: f64, q_prev: f64, q_fixed: f64) -> f64 {
        match self {
            VscHvdcControlMode::ConstantPQ { q_set, .. } => *q_set,
            VscHvdcControlMode::ConstantVdc { q_set, .. } => *q_set,
            VscHvdcControlMode::PVdcDroop { .. } => q_fixed,
            VscHvdcControlMode::ConstantPVac {
                v_target,
                v_band,
                q_min,
                q_max,
                ..
            } => {
                let dv = v_target - v_ac;
                if dv.abs() <= *v_band {
                    // Inside dead-band: hold current Q.
                    q_prev.clamp(*q_min, *q_max)
                } else {
                    // Proportional response: k_v = (q_max - q_min) / (2 * v_band + 0.02)
                    // The extra 0.02 prevents k_v from going to infinity when v_band → 0.
                    let q_range = q_max - q_min;
                    let k_v = q_range / (2.0 * v_band + 0.02).max(0.02);
                    let q_new = q_prev + k_v * dv;
                    q_new.clamp(*q_min, *q_max)
                }
            }
        }
    }

    /// Return `true` if this mode regulates AC bus voltage (ConstantPVac).
    pub fn is_voltage_regulating(&self) -> bool {
        matches!(self, VscHvdcControlMode::ConstantPVac { .. })
    }

    /// Return `true` if this mode is a DC voltage slack (ConstantVdc).
    pub fn is_dc_slack(&self) -> bool {
        matches!(self, VscHvdcControlMode::ConstantVdc { .. })
    }

    /// Return `true` if this mode uses P/V_dc droop (PVdcDroop).
    pub fn is_droop(&self) -> bool {
        matches!(self, VscHvdcControlMode::PVdcDroop { .. })
    }
}

impl Default for VscHvdcControlMode {
    /// Default is `ConstantPQ` with zero setpoints, preserving existing behaviour.
    fn default() -> Self {
        VscHvdcControlMode::ConstantPQ {
            p_set: 0.0,
            q_set: 0.0,
        }
    }
}

/// Per-station state used in the sequential AC-DC MTDC iteration.
///
/// Carried between outer iterations so that voltage-regulating and droop
/// stations can update their Q and P setpoints incrementally.
#[derive(Debug, Clone)]
pub struct VscStationState {
    /// Current active power injection at this station in MW.
    pub p_mw: f64,
    /// Current reactive power injection at this station in MVAR.
    pub q_mvar: f64,
    /// Current DC bus voltage in per-unit (updated during MTDC iteration).
    pub v_dc_pu: f64,
}

impl VscStationState {
    /// Construct initial state from nominal setpoints.
    pub fn new(p_mw: f64, q_mvar: f64) -> Self {
        Self {
            p_mw,
            q_mvar,
            v_dc_pu: 1.0,
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ConstantPQ: effective_p and effective_q return the setpoints ──────────

    #[test]
    fn constant_pq_returns_setpoints() {
        let mode = VscHvdcControlMode::ConstantPQ {
            p_set: 100.0,
            q_set: 30.0,
        };
        assert_eq!(mode.effective_p_mw(1.0), 100.0);
        assert_eq!(mode.effective_q_mvar(1.0, 0.0, 0.0), 30.0);
        assert!(!mode.is_voltage_regulating());
        assert!(!mode.is_dc_slack());
        assert!(!mode.is_droop());
    }

    // ── ConstantPVac: Q adjusts to regulate voltage, clamps at limits ─────────

    #[test]
    fn constant_pvac_q_increases_when_voltage_low() {
        let mode = VscHvdcControlMode::ConstantPVac {
            p_set: 100.0,
            v_target: 1.0,
            v_band: 0.01,
            q_min: -50.0,
            q_max: 50.0,
        };
        // v_ac = 0.95 (low) → Q should increase from 0 to help support voltage.
        let q = mode.effective_q_mvar(0.95, 0.0, 0.0);
        assert!(
            q > 0.0,
            "Q must be positive to support low voltage, got {q}"
        );
        assert!(q <= 50.0, "Q must not exceed q_max=50.0, got {q}");
        assert!(mode.is_voltage_regulating());
    }

    #[test]
    fn constant_pvac_no_action_within_deadband() {
        let mode = VscHvdcControlMode::ConstantPVac {
            p_set: 100.0,
            v_target: 1.0,
            v_band: 0.02,
            q_min: -50.0,
            q_max: 50.0,
        };
        // v_ac = 1.01 (within ±0.02 band) → Q unchanged from q_prev=10.0.
        let q = mode.effective_q_mvar(1.01, 10.0, 0.0);
        assert_eq!(q, 10.0, "Q must not change inside dead-band");
    }

    #[test]
    fn constant_pvac_q_clamps_at_limit() {
        let mode = VscHvdcControlMode::ConstantPVac {
            p_set: 100.0,
            v_target: 1.0,
            v_band: 0.001,
            q_min: -20.0,
            q_max: 20.0,
        };
        // Very large voltage deviation; Q should clamp at q_max.
        let q_high = mode.effective_q_mvar(0.50, 0.0, 0.0);
        assert_eq!(q_high, 20.0, "Q must clamp at q_max on large low-voltage");

        // Q should clamp at q_min for high voltage.
        let q_low = mode.effective_q_mvar(1.50, 0.0, 0.0);
        assert_eq!(q_low, -20.0, "Q must clamp at q_min on large high-voltage");
    }

    // ── ConstantVdc: P determined externally; Q is fixed ─────────────────────

    #[test]
    fn constant_vdc_flags() {
        let mode = VscHvdcControlMode::ConstantVdc {
            v_dc_target: 1.0,
            q_set: -10.0,
        };
        assert!(mode.is_dc_slack());
        assert_eq!(mode.effective_q_mvar(1.0, 0.0, 0.0), -10.0);
        assert_eq!(mode.p_set_mw(), 0.0); // placeholder
    }

    // ── PVdcDroop: P responds linearly to V_dc deviation, clamps ─────────────

    #[test]
    fn droop_p_increases_with_v_dc() {
        let mode = VscHvdcControlMode::PVdcDroop {
            p_set: 100.0,
            voltage_dc_setpoint_pu: 1.0,
            k_droop: 50.0, // 50 MW per pu voltage deviation
            p_min: 50.0,
            p_max: 150.0,
        };
        assert!(mode.is_droop());

        // At nominal V_dc: P = p_set.
        assert!((mode.effective_p_mw(1.0) - 100.0).abs() < 1e-10);

        // V_dc = 1.02 (above nominal): P increases.
        let p_high = mode.effective_p_mw(1.02);
        assert!(
            (p_high - 101.0).abs() < 1e-10,
            "expected 101.0, got {p_high}"
        );

        // V_dc = 0.98 (below nominal): P decreases.
        let p_low = mode.effective_p_mw(0.98);
        assert!((p_low - 99.0).abs() < 1e-10, "expected 99.0, got {p_low}");
    }

    #[test]
    fn droop_p_clamps_to_limits() {
        let mode = VscHvdcControlMode::PVdcDroop {
            p_set: 100.0,
            voltage_dc_setpoint_pu: 1.0,
            k_droop: 1000.0, // very high droop — should saturate quickly
            p_min: 80.0,
            p_max: 120.0,
        };
        // Large positive deviation → clamp at p_max.
        assert_eq!(mode.effective_p_mw(1.10), 120.0);
        // Large negative deviation → clamp at p_min.
        assert_eq!(mode.effective_p_mw(0.90), 80.0);
    }

    // ── Default mode is ConstantPQ with zero setpoints ────────────────────────

    #[test]
    fn default_mode_is_constant_pq_zero() {
        let mode = VscHvdcControlMode::default();
        assert_eq!(
            mode,
            VscHvdcControlMode::ConstantPQ {
                p_set: 0.0,
                q_set: 0.0
            }
        );
    }
}
