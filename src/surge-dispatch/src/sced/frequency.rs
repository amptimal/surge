// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Frequency security post-solve metrics for SCED.

use surge_network::Network;
use tracing::info;

use crate::common::spec::DispatchProblemSpec;

/// Compute system inertia H_sys at the given dispatch point.
pub(crate) fn compute_system_inertia(
    _pg_mw: &[f64],
    gen_indices: &[usize],
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
) -> f64 {
    if spec.frequency_security.generator_h_values.is_empty() {
        return 0.0;
    }
    let mut sum_h_mbase = 0.0_f64;
    let mut sum_mbase = 0.0_f64;
    for (j, &gi) in gen_indices.iter().enumerate() {
        let h = spec
            .frequency_security
            .generator_h_values
            .get(j)
            .copied()
            .unwrap_or(0.0);
        let mbase = network.generators[gi]
            .machine_base_mva
            .max(network.base_mva);
        sum_h_mbase += h * mbase;
        sum_mbase += mbase;
    }
    if sum_mbase > 0.0 {
        sum_h_mbase / sum_mbase
    } else {
        0.0
    }
}

/// Compute estimated initial RoCoF for the largest credible event.
pub(crate) fn compute_estimated_rocof(
    _pg_mw: &[f64],
    gen_indices: &[usize],
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
) -> f64 {
    if spec.frequency_security.generator_h_values.is_empty() {
        return 0.0;
    }
    // Total stored kinetic energy = Σ(H_g × S_g) in MWs.
    let mut sum_h_mbase = 0.0_f64;
    for (j, &gi) in gen_indices.iter().enumerate() {
        let h = spec
            .frequency_security
            .generator_h_values
            .get(j)
            .copied()
            .unwrap_or(0.0);
        let mbase = network.generators[gi]
            .machine_base_mva
            .max(network.base_mva);
        sum_h_mbase += h * mbase;
    }
    if sum_h_mbase <= 0.0 {
        return 0.0;
    }
    let f0 = 60.0; // Hz
    let event_mw = if spec.frequency_security.freq_event_mw > 0.0 {
        spec.frequency_security.freq_event_mw
    } else {
        // Auto: largest unit Pmax.
        gen_indices
            .iter()
            .map(|&gi| network.generators[gi].pmax)
            .fold(0.0_f64, f64::max)
    };
    // RoCoF = P_event_MW × f0 / (2 × Σ(H_g × S_g_MVA)) — in Hz/s
    event_mw * f0 / (2.0 * sum_h_mbase)
}

/// Check if frequency constraints are satisfied at the dispatch point.
pub(crate) fn check_frequency_security(
    pg_mw: &[f64],
    gen_indices: &[usize],
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
) -> bool {
    let h_sys = compute_system_inertia(pg_mw, gen_indices, network, spec);

    // Inertia constraint.
    let min_inertia_s = spec.frequency_security.min_inertia_mws.unwrap_or(0.0);
    if min_inertia_s > 0.0 && h_sys < min_inertia_s {
        info!(
            "Frequency constraint VIOLATED: H_sys={h_sys:.2}s < min={:.2}s",
            min_inertia_s
        );
        return false;
    }

    // RoCoF constraint.
    let max_rocof = spec.frequency_security.max_rocof_hz_per_s.unwrap_or(0.0);
    if max_rocof > 0.0 {
        let rocof = compute_estimated_rocof(pg_mw, gen_indices, network, spec);
        if rocof > max_rocof {
            info!(
                "Frequency constraint VIOLATED: RoCoF={rocof:.3} Hz/s > max={:.3} Hz/s",
                max_rocof
            );
            return false;
        }
    }

    // Nadir constraint (linearized headroom-based approximation).
    if spec.frequency_security.min_nadir_hz > 0.0 {
        let f0 = 60.0;
        let delta_f_max = f0 - spec.frequency_security.min_nadir_hz;
        if delta_f_max <= 0.0 {
            return true; // no meaningful constraint
        }
        let event_mw = if spec.frequency_security.freq_event_mw > 0.0 {
            spec.frequency_security.freq_event_mw
        } else {
            gen_indices
                .iter()
                .map(|&gi| network.generators[gi].pmax)
                .fold(0.0_f64, f64::max)
        };
        // Required: Σ(headroom_g / R_g) >= P_event / (f0 × Δf_max)
        let required = event_mw / (f0 * delta_f_max);
        let mut available = 0.0_f64;
        for (j, &gi) in gen_indices.iter().enumerate() {
            let headroom = network.generators[gi].pmax - pg_mw[j];
            // Default 5% droop (R=0.05 pu). Generator-specific droop is not
            // available in the MATPOWER/PSS/E data model; use governor models
            // for accurate per-unit droop characterization.
            let droop = 0.05;
            if headroom > 0.0 && droop > 0.0 {
                available += headroom / droop;
            }
        }
        let available_pu = available / network.base_mva;
        if available_pu < required / network.base_mva {
            info!(
                "Frequency constraint VIOLATED: nadir headroom={available:.1} MW < required for {event_mw:.1} MW event"
            );
            return false;
        }
    }

    true
}
