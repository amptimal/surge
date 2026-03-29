// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Post-NR discrete voltage control: OLTC tap stepping and switched shunt switching.
//!
//! Both OLTC transformers and switched shunts regulate bus voltage through
//! discrete steps that cannot be expressed as smooth constraints in the NR
//! Jacobian.  The strategy is an outer control loop:
//!
//! 1. Run NR to convergence.
//! 2. Check each OLTC/shunt: if regulated bus voltage is outside the dead-band,
//!    step the device toward the target and mark the network as modified.
//! 3. If any device stepped, rebuild the Y-bus and re-run NR from the current
//!    voltage state (warm start).
//! 4. Repeat until all devices are within band or `max_iter` is exhausted.
//!
//! This module provides:
//! - `OltcState`: per-OLTC working state (current tap).
//! - `SwitchedShuntState`: per-shunt working state (current step count).
//! - [`apply_oltc_steps`]: check OLTCs and step taps; returns number of changes.
//! - `apply_shunt_steps`: check shunts and step banks; returns number of changes.
//! - [`apply_par_steps`]: check PARs and step phase angles; returns number of changes.

use surge_network::network::discrete_control::{OltcControl, ParControl};
use surge_solution::PfSolution;

/// Internal switched-shunt control resolved onto solver bus indices.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedSwitchedShunt {
    pub(crate) id: String,
    pub(crate) bus_idx: usize,
    pub(crate) bus_regulated_idx: usize,
    pub(crate) b_step: f64,
    pub(crate) n_steps_cap: i32,
    pub(crate) n_steps_react: i32,
    pub(crate) v_target: f64,
    pub(crate) v_band: f64,
    pub(crate) n_active_steps: i32,
}

/// Apply one round of OLTC tap adjustments.
///
/// For each OLTC:
/// - If `vm[oltc.bus_regulated]` is below the dead-band (v_target - v_band/2),
///   raise the tap (lower turns ratio on LV side → higher secondary voltage).
/// - If above the dead-band, lower the tap.
/// - Clamp the resulting tap to `[tap_min, tap_max]`.
///
/// Modifies `taps[branch_index]` in the caller's working tap array.
///
/// Returns the number of OLTC transformers that changed tap position this round.
pub fn apply_oltc_steps(oltcs: &[OltcControl], vm: &[f64], taps: &mut [f64]) -> usize {
    let mut n_changed = 0usize;

    for oltc in oltcs {
        if oltc.bus_regulated >= vm.len() || oltc.branch_index >= taps.len() {
            continue;
        }
        let v = vm[oltc.bus_regulated];
        let half_band = oltc.v_band * 0.5;
        let current_tap = taps[oltc.branch_index];
        let new_tap;

        if v < oltc.v_target - half_band {
            // Voltage too low → decrease tap ratio (increase secondary voltage).
            // PSS/E convention: a lower tap ratio raises the regulated-bus voltage.
            new_tap = (current_tap - oltc.tap_step).max(oltc.tap_min);
        } else if v > oltc.v_target + half_band {
            // Voltage too high → increase tap ratio.
            new_tap = (current_tap + oltc.tap_step).min(oltc.tap_max);
        } else {
            // Within dead-band — no change.
            continue;
        }

        if (new_tap - current_tap).abs() > 1e-12 {
            taps[oltc.branch_index] = new_tap;
            n_changed += 1;
        }
    }

    n_changed
}

/// Apply one round of switched shunt bank switching.
///
/// For each shunt:
/// - If `vm[shunt.bus]` is below the dead-band, switch in one more capacitor
///   step (increment `n_active_steps`), capped at `n_steps_cap`.
/// - If above the dead-band, switch in one more reactor step (decrement
///   `n_active_steps`), capped at `-n_steps_react`.
///
/// Modifies `active_steps[shunt_index]` and `bs_delta[shunt.bus]` in the
/// caller's working arrays.
///
/// Returns the number of shunts that changed step count this round.
pub(crate) fn apply_shunt_steps(
    shunts: &[ResolvedSwitchedShunt],
    vm: &[f64],
    active_steps: &mut [i32],
) -> usize {
    let mut n_changed = 0usize;

    for (idx, shunt) in shunts.iter().enumerate() {
        if shunt.bus_regulated_idx >= vm.len() || idx >= active_steps.len() {
            continue;
        }
        let v = vm[shunt.bus_regulated_idx];
        let half_band = shunt.v_band * 0.5;
        let current_steps = active_steps[idx];
        let new_steps;

        if v < shunt.v_target - half_band {
            // Voltage too low → switch in one capacitor step (raise voltage).
            new_steps = (current_steps + 1).min(shunt.n_steps_cap);
        } else if v > shunt.v_target + half_band {
            // Voltage too high → switch in one reactor step (lower voltage).
            new_steps = (current_steps - 1).max(-shunt.n_steps_react);
        } else {
            // Within dead-band — no change.
            continue;
        }

        if new_steps != current_steps {
            active_steps[idx] = new_steps;
            n_changed += 1;
        }
    }

    n_changed
}

/// Apply one round of Phase Angle Regulator (PAR) phase-shift adjustments.
///
/// For each PAR, computes the active power flow on the monitored branch from
/// the converged NR solution.  If the flow is outside the target band:
///
/// - Too little flow → increase phase-angle shift (push more P through).
/// - Too much flow → decrease phase-angle shift.
///
/// The resulting shift is clamped to `[angle_min_deg, angle_max_deg]`.
///
/// Modifies `shifts_deg[branch_index]` in the caller's working phase-angle array.
///
/// Returns the number of PARs that changed phase position this round.
pub fn apply_par_steps(pars: &[ParControl], sol: &PfSolution, shifts_deg: &mut [f64]) -> usize {
    let flows = sol.branch_pq_flows();
    let mut n_changed = 0usize;

    for par in pars {
        // Guard both indices before touching arrays.
        if par.branch_index >= shifts_deg.len() || par.monitored_branch_index >= flows.len() {
            continue;
        }
        let p_flow = flows[par.monitored_branch_index].0; // MW

        let half_band = par.p_band_mw * 0.5;
        let current_ang = shifts_deg[par.branch_index];
        let new_ang;

        if p_flow < par.p_target_mw - half_band {
            // Too little flow → increase phase angle (drive more P through PAR).
            new_ang = (current_ang + par.ang_step_deg).min(par.angle_max_deg);
        } else if p_flow > par.p_target_mw + half_band {
            // Too much flow → decrease phase angle.
            new_ang = (current_ang - par.ang_step_deg).max(par.angle_min_deg);
        } else {
            continue; // within dead-band
        }

        if (new_ang - current_ang).abs() > 1e-10 {
            shifts_deg[par.branch_index] = new_ang;
            n_changed += 1;
        }
    }

    n_changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::discrete_control::OltcControl;

    // -----------------------------------------------------------------------
    // OLTC unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn oltc_steps_down_when_voltage_high() {
        let oltc = OltcControl {
            branch_index: 0,
            bus_regulated: 1,
            v_target: 1.0,
            v_band: 0.02,
            tap_min: 0.9,
            tap_max: 1.1,
            tap_step: 0.00625,
        };
        let vm = vec![1.05, 1.06]; // bus 1 voltage = 1.06 > 1.01 (upper edge)
        let mut taps = vec![1.0f64];

        let changed = apply_oltc_steps(&[oltc], &vm, &mut taps);
        assert_eq!(changed, 1);
        assert!(
            (taps[0] - 1.00625).abs() < 1e-10,
            "tap should increase by one step"
        );
    }

    #[test]
    fn oltc_steps_up_when_voltage_low() {
        let oltc = OltcControl {
            branch_index: 0,
            bus_regulated: 0,
            v_target: 1.0,
            v_band: 0.02,
            tap_min: 0.9,
            tap_max: 1.1,
            tap_step: 0.00625,
        };
        let vm = vec![0.93]; // voltage = 0.93 < 0.99 (lower edge)
        let mut taps = vec![1.0f64];

        let changed = apply_oltc_steps(&[oltc], &vm, &mut taps);
        assert_eq!(changed, 1);
        assert!(
            (taps[0] - 0.99375).abs() < 1e-10,
            "tap should decrease by one step"
        );
    }

    #[test]
    fn oltc_no_change_within_band() {
        let oltc = OltcControl {
            branch_index: 0,
            bus_regulated: 0,
            v_target: 1.0,
            v_band: 0.04,
            tap_min: 0.9,
            tap_max: 1.1,
            tap_step: 0.00625,
        };
        let vm = vec![1.01]; // within ±0.02 band
        let mut taps = vec![1.0f64];

        let changed = apply_oltc_steps(&[oltc], &vm, &mut taps);
        assert_eq!(changed, 0);
        assert!((taps[0] - 1.0).abs() < 1e-10, "tap should not change");
    }

    #[test]
    fn oltc_clamped_at_tap_max() {
        let oltc = OltcControl {
            branch_index: 0,
            bus_regulated: 0,
            v_target: 1.0,
            v_band: 0.02,
            tap_min: 0.9,
            tap_max: 1.0, // already at max
            tap_step: 0.00625,
        };
        let vm = vec![1.05]; // voltage too high
        let mut taps = vec![1.0f64]; // already at tap_max

        let changed = apply_oltc_steps(&[oltc], &vm, &mut taps);
        // tap can't increase beyond tap_max so no effective change
        assert_eq!(changed, 0);
    }

    #[test]
    fn oltc_clamped_at_tap_min() {
        let oltc = OltcControl {
            branch_index: 0,
            bus_regulated: 0,
            v_target: 1.0,
            v_band: 0.02,
            tap_min: 1.0, // already at min
            tap_max: 1.1,
            tap_step: 0.00625,
        };
        let vm = vec![0.93]; // voltage too low
        let mut taps = vec![1.0f64]; // already at tap_min

        let changed = apply_oltc_steps(&[oltc], &vm, &mut taps);
        // tap can't decrease beyond tap_min so no effective change
        assert_eq!(changed, 0);
    }

    // -----------------------------------------------------------------------
    // Switched shunt unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn shunt_switches_cap_step_when_voltage_low() {
        let shunt = ResolvedSwitchedShunt {
            id: String::new(),
            bus_idx: 0,
            bus_regulated_idx: 0,
            b_step: 0.1,
            n_steps_cap: 5,
            n_steps_react: 0,
            v_target: 1.0,
            v_band: 0.02,
            n_active_steps: 0,
        };
        let vm = vec![0.97]; // voltage < 0.99 — need capacitor
        let mut steps = vec![0i32]; // current active steps

        let changed = apply_shunt_steps(&[shunt], &vm, &mut steps);
        assert_eq!(changed, 1);
        assert_eq!(steps[0], 1, "one capacitor step should be switched in");
    }

    #[test]
    fn shunt_switches_reactor_step_when_voltage_high() {
        let shunt = ResolvedSwitchedShunt {
            id: String::new(),
            bus_idx: 0,
            bus_regulated_idx: 0,
            b_step: 0.1,
            n_steps_cap: 3,
            n_steps_react: 3,
            v_target: 1.0,
            v_band: 0.02,
            n_active_steps: 0,
        };
        let vm = vec![1.05]; // voltage > 1.01 — need reactor
        let mut steps = vec![0i32];

        let changed = apply_shunt_steps(&[shunt], &vm, &mut steps);
        assert_eq!(changed, 1);
        assert_eq!(steps[0], -1, "one reactor step should be switched in");
    }

    #[test]
    fn shunt_no_change_within_band() {
        let shunt = ResolvedSwitchedShunt {
            id: String::new(),
            bus_idx: 0,
            bus_regulated_idx: 0,
            b_step: 0.1,
            n_steps_cap: 5,
            n_steps_react: 5,
            v_target: 1.0,
            v_band: 0.04,
            n_active_steps: 2,
        };
        let vm = vec![1.01]; // within ±0.02 band
        let mut steps = vec![2i32];

        let changed = apply_shunt_steps(&[shunt], &vm, &mut steps);
        assert_eq!(changed, 0);
        assert_eq!(steps[0], 2, "steps should not change");
    }

    #[test]
    fn shunt_capped_at_max_cap_steps() {
        let shunt = ResolvedSwitchedShunt {
            id: String::new(),
            bus_idx: 0,
            bus_regulated_idx: 0,
            b_step: 0.1,
            n_steps_cap: 3,
            n_steps_react: 0,
            v_target: 1.0,
            v_band: 0.02,
            n_active_steps: 3, // already at max
        };
        let vm = vec![0.90]; // very low voltage
        let mut steps = vec![3i32]; // already maxed out

        let changed = apply_shunt_steps(&[shunt], &vm, &mut steps);
        assert_eq!(changed, 0, "at max cap steps, no further change possible");
    }

    #[test]
    fn shunt_capped_at_max_react_steps() {
        let shunt = ResolvedSwitchedShunt {
            id: String::new(),
            bus_idx: 0,
            bus_regulated_idx: 0,
            b_step: 0.1,
            n_steps_cap: 0,
            n_steps_react: 3,
            v_target: 1.0,
            v_band: 0.02,
            n_active_steps: -3, // already at max reactor
        };
        let vm = vec![1.10]; // very high voltage
        let mut steps = vec![-3i32]; // already maxed out

        let changed = apply_shunt_steps(&[shunt], &vm, &mut steps);
        assert_eq!(
            changed, 0,
            "at max reactor steps, no further change possible"
        );
    }

    #[test]
    fn shunt_b_injected() {
        let mut shunt = ResolvedSwitchedShunt {
            id: String::new(),
            bus_idx: 0,
            bus_regulated_idx: 0,
            b_step: 0.1,
            n_steps_cap: 5,
            n_steps_react: 0,
            v_target: 1.0,
            v_band: 0.02,
            n_active_steps: 0,
        };
        assert!((shunt.b_step * shunt.n_active_steps as f64 - 0.0).abs() < 1e-12);
        shunt.n_active_steps = 3;
        assert!(
            (shunt.b_step * shunt.n_active_steps as f64 - 0.3).abs() < 1e-10,
            "3 steps × 0.1 = 0.3 p.u."
        );
    }
}
