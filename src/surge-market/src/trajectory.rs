// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical startup/shutdown trajectory derivation and online-status
//! inference.
//!
//! Power markets that model startup and shutdown as a physical power
//! ramp (rather than as an instantaneous step) need to reconstruct the
//! unit's power profile during the ramp window when they export a
//! solution. The same market also often needs to *infer* an on/off
//! schedule from a solved MW profile and a commitment decision —
//! especially for zero-Pmin units and across the horizon boundary,
//! where the solver's commitment bit is the only reliable source of
//! truth.
//!
//! Both are format-agnostic market concerns. This module provides the
//! canonical routines; GO C3 and other format adapters call them from
//! their solution exporters.

/// Given per-period `(p_startup_ramp_ub, p_shutdown_ramp_ub)` MW caps
/// and the integer interval durations in hours, compute the canonical
/// startup/shutdown trajectory power that should be attributed to a
/// unit's commitment transitions but is **not** paid through
/// `p_on`.
///
/// Each period `t` gets `trajectory_mw[t]`, the MW credited to the
/// physical ramp. The calling exporter typically subtracts this from
/// the solver's MW output when reporting `p_on`.
///
/// This is intentionally a pure helper: it assumes the caller has
/// already aligned the ramp cap vectors to the final commitment
/// schedule, and it treats zero-length ramp windows as "no
/// trajectory contribution."
pub fn derive_startup_shutdown_trajectory_power(
    u_on: &[bool],
    p_startup_ramp_mw: &[f64],
    p_shutdown_ramp_mw: &[f64],
    interval_hours: &[f64],
) -> Vec<f64> {
    let periods = u_on.len();
    let mut trajectory = vec![0.0; periods];
    for t in 0..periods {
        // Startup trajectory: period immediately preceding a commitment
        // transition from off → on contributes `p_startup_ramp_ub[t]`
        // times a fractional interval.
        let is_startup_edge = t + 1 < periods && !u_on[t] && u_on[t + 1];
        if is_startup_edge {
            let cap = p_startup_ramp_mw.get(t).copied().unwrap_or(0.0);
            let dt = interval_hours.get(t).copied().unwrap_or(1.0);
            trajectory[t] += cap * dt;
        }
        // Shutdown trajectory: period immediately following a
        // commitment transition from on → off contributes
        // `p_shutdown_ramp_ub[t]` times the interval.
        let is_shutdown_edge = t > 0 && u_on[t - 1] && !u_on[t];
        if is_shutdown_edge {
            let cap = p_shutdown_ramp_mw.get(t).copied().unwrap_or(0.0);
            let dt = interval_hours.get(t).copied().unwrap_or(1.0);
            trajectory[t] += cap * dt;
        }
    }
    trajectory
}

/// Infer a per-period `on_status` schedule from a solved MW profile
/// and the commitment decision from the solver.
///
/// The solver's `commitment` bit is authoritative for zero-floor (i.e.
/// Pmin = 0) units where MW alone does not distinguish "committed but
/// throttled to zero" from "uncommitted." For units with a nonzero
/// Pmin, a very small positive MW output with `commitment = false`
/// would indicate slack in the LP relaxation — in that case we defer
/// to the commitment bit as well, since the downstream schema
/// requires an integer 0/1 answer.
///
/// `p_mw` is the solved MW output; `commitment` is the solver's
/// commitment decision (`None` means "undefined" — the function
/// returns 0 for that period).
pub fn infer_online_status_from_dispatch(p_mw: &[f64], commitment: &[Option<bool>]) -> Vec<i32> {
    let periods = p_mw.len().max(commitment.len());
    let mut on_status = vec![0_i32; periods];
    for (t, status) in on_status.iter_mut().enumerate() {
        let c = commitment.get(t).copied().flatten();
        *status = match c {
            Some(true) => 1,
            Some(false) => 0,
            None => 0,
        };
    }
    on_status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trajectory_fires_at_startup_edge() {
        // 4-period horizon: off, off, on, on. Startup edge between t=1 and t=2.
        let u_on = vec![false, false, true, true];
        let p_startup = vec![10.0, 20.0, 30.0, 40.0]; // indexed by t (period before ON)
        let p_shutdown = vec![0.0; 4];
        let dt = vec![1.0, 1.0, 1.0, 1.0];
        let traj = derive_startup_shutdown_trajectory_power(&u_on, &p_startup, &p_shutdown, &dt);
        assert_eq!(traj, vec![0.0, 20.0, 0.0, 0.0]);
    }

    #[test]
    fn trajectory_fires_at_shutdown_edge() {
        // 4-period horizon: on, on, off, off. Shutdown edge entering t=2.
        let u_on = vec![true, true, false, false];
        let p_startup = vec![0.0; 4];
        let p_shutdown = vec![5.0, 15.0, 25.0, 35.0];
        let dt = vec![0.5, 0.5, 0.5, 0.5];
        let traj = derive_startup_shutdown_trajectory_power(&u_on, &p_startup, &p_shutdown, &dt);
        assert_eq!(traj, vec![0.0, 0.0, 12.5, 0.0]);
    }

    #[test]
    fn on_status_follows_commitment_bit() {
        let p = vec![0.0, 50.0, 100.0, 0.0];
        let commit = vec![Some(false), Some(true), Some(true), Some(false)];
        let status = infer_online_status_from_dispatch(&p, &commit);
        assert_eq!(status, vec![0, 1, 1, 0]);
    }

    #[test]
    fn on_status_defaults_to_zero_when_commitment_unknown() {
        let p = vec![0.0; 3];
        let commit = vec![None, None, None];
        let status = infer_online_status_from_dispatch(&p, &commit);
        assert_eq!(status, vec![0, 0, 0]);
    }
}
