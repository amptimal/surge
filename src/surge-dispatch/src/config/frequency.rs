// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Optional frequency security constraints for SCED/SCUC dispatch LP.
//!
//! When all options are `None` (the default), zero LP rows are added and there
//! is no performance impact. Enable individual constraints by setting the
//! corresponding `Option<f64>` field.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Frequency security constraints to embed in the SCED/SCUC LP.
///
/// Each field is optional — `None` means "do not add this constraint."
/// All `None` (the default) produces zero additional LP rows.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FrequencySecurityOptions {
    /// Minimum system inertia in MW·s.
    ///
    /// SCED (continuous LP): enforces
    ///   Σ_i (H_i · Sg_i / Pmax_i) · Pg_i ≥ min_inertia_mws / base_mva
    /// using dispatch level as a proxy for online fraction. Conservative
    /// (underestimates inertia at partial dispatch), safe for security.
    ///
    /// SCUC (MIP): enforces
    ///   Σ_i H_i · Sg_i · u_i ≥ min_inertia_mws
    /// exactly, since the binary commitment variable u_i correctly represents
    /// whether the unit is online.
    pub min_inertia_mws: Option<f64>,

    /// Maximum rate-of-change-of-frequency (Hz/s) after the largest credible
    /// contingency. Converted to a minimum inertia requirement:
    ///   H_min_from_rocof = largest_contingency_mw / (2 · base_frequency_hz · max_rocof)
    /// The effective inertia floor is max(min_inertia_mws, H_min_from_rocof).
    pub max_rocof_hz_per_s: Option<f64>,

    /// Minimum primary frequency response (MW) deployable within 30 seconds.
    ///
    /// Enforces: Σ_i pfr_eligible_i · (Pmax_i - Pg_i) ≥ min_pfr_mw / base_mva
    /// Generators opt in via `Generator::pfr_eligible` (default: true).
    pub min_pfr_mw: Option<f64>,

    /// Per-generator inertia constant H (seconds), indexed by position in the
    /// in-service generator list. Required for inertia and RoCoF post-solve
    /// metrics. When empty, frequency metrics are not computed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generator_h_values: Vec<f64>,

    /// Generation loss event size (MW) for RoCoF and nadir calculations.
    /// When `0.0` (the default), the solver uses the largest generator Pmax
    /// as the event size.
    #[serde(default)]
    pub freq_event_mw: f64,

    /// Minimum frequency nadir (Hz). `0.0` = disabled.
    ///
    /// Enforces a linearized headroom constraint:
    ///   Σ (headroom_g / R_g) ≥ P_event / (f0 × Δf_max)
    /// where Δf_max = f0 − min_nadir_hz and R_g is per-unit droop (default 0.05).
    #[serde(default)]
    pub min_nadir_hz: f64,

    /// Largest credible contingency in MW. Required when `max_rocof_hz_per_s`
    /// is `Some`. Ignored otherwise.
    #[serde(default = "default_contingency")]
    pub largest_contingency_mw: f64,

    /// Nominal system frequency in Hz. Default 60.0 (ERCOT / North America).
    #[serde(default = "default_frequency")]
    pub base_frequency_hz: f64,
}

fn default_contingency() -> f64 {
    0.0
}
fn default_frequency() -> f64 {
    60.0
}

impl Default for FrequencySecurityOptions {
    fn default() -> Self {
        Self {
            min_inertia_mws: None,
            max_rocof_hz_per_s: None,
            min_pfr_mw: None,
            generator_h_values: Vec::new(),
            freq_event_mw: 0.0,
            min_nadir_hz: 0.0,
            largest_contingency_mw: 0.0,
            base_frequency_hz: 60.0,
        }
    }
}

impl FrequencySecurityOptions {
    /// Returns true if any constraint is enabled (i.e., would add LP rows).
    pub fn is_active(&self) -> bool {
        self.min_inertia_mws.is_some()
            || self.max_rocof_hz_per_s.is_some()
            || self.min_pfr_mw.is_some()
    }

    /// Compute the effective minimum inertia floor in MW·s, combining
    /// min_inertia_mws and ROCOF-derived requirement.
    pub fn effective_min_inertia_mws(&self) -> f64 {
        let from_inertia = self.min_inertia_mws.unwrap_or(0.0);
        let from_rocof = self
            .max_rocof_hz_per_s
            .map(|rocof| {
                if rocof > 0.0 && self.largest_contingency_mw > 0.0 {
                    self.largest_contingency_mw / (2.0 * self.base_frequency_hz * rocof)
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);
        from_inertia.max(from_rocof)
    }
}
