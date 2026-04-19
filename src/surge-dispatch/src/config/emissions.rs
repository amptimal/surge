// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Emission-aware dispatch types: DISP-05, DISP-06, DISP-09.
//!
//! This module provides:
//! - [`EmissionProfile`]   — per-generator tCO2/MWh rates (alternative to inline co2_rate_t_per_mwh)
//! - [`CarbonPrice`]       — $/tCO2 carbon price (alternative to inline co2_price_per_t)
//! - [`TieLineLimits`]     — DISP-06 area-pair MW transfer limits
//! - [`MustRunUnits`]      — DISP-09 reliability must-run (RMR) generator floor list

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ─── DISP-05: Emission profile and carbon price ─────────────────────────────

/// Per-unit emission intensity (tCO2/MWh) for each generator.
///
/// When provided alongside [`CarbonPrice`] in a canonical dispatch request,
/// the carbon cost is added to the LP objective:
///
/// ```text
/// obj += Σ_i Pg_i_pu * base_mva * rate_i * carbon_price_per_tonne
/// ```
///
/// This mirrors the inline `co2_price_per_t` × `co2_rate_t_per_mwh` path but
/// allows externally supplied emission rates that override the generator field.
/// If both are provided, `EmissionProfile` rates take precedence.
///
/// The length of `rates_tonnes_per_mwh` must equal the number of in-service
/// generators in network order (i.e. matches dispatch/result generator ordering).
/// Extra entries are ignored; missing entries default to 0.0.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EmissionProfile {
    /// Generator index → tCO2/MWh emission rate.
    ///
    /// Index is the position within the in-service generator list, consistent
    /// with the order used by SCED/SCUC dispatch result vectors.
    pub rates_tonnes_per_mwh: Vec<f64>,
}

impl EmissionProfile {
    /// Return the emission rate for in-service generator at position `j`.
    /// Falls back to `0.0` when index is out of bounds.
    pub fn rate_for(&self, j: usize) -> f64 {
        self.rates_tonnes_per_mwh.get(j).copied().unwrap_or(0.0)
    }
}

/// Carbon price in $/tCO2.
///
/// Combined with [`EmissionProfile`] (or the inline `co2_rate_t_per_mwh`
/// generator field), this adds an emission surcharge to each generator's
/// dispatch cost:
///
/// ```text
/// carbon_cost_g = pg_mw_g * hours * rate_g * price_per_tonne
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CarbonPrice {
    /// Carbon price in USD per tonne of CO2.
    pub price_per_tonne: f64,
}

impl CarbonPrice {
    /// Create a new `CarbonPrice`.
    pub fn new(price_per_tonne: f64) -> Self {
        Self { price_per_tonne }
    }

    /// Effective cost in $/MWh for a generator with the given emission rate.
    ///
    /// ```text
    /// shadow_price = price_per_tonne × rate_t_per_mwh   [$/MWh]
    /// ```
    pub fn co2_shadow_price_per_mwh(&self, rate_t_per_mwh: f64) -> f64 {
        self.price_per_tonne * rate_t_per_mwh
    }
}

// ─── DISP-06: Multi-area tie-line limits ────────────────────────────────────

/// Inter-area transfer limits for multi-area dispatch.
///
/// When provided in a canonical dispatch request, an additional LP constraint is
/// added per `(from_area, to_area)` pair per hour on the physical interchange
/// across AC branches and dispatchable HVDC links whose terminal buses lie in
/// those two areas:
///
/// ```text
/// net_transfer[from → to]
///   = Σ AC branch flow crossing from_area to to_area
///   + Σ dispatchable HVDC transfer from_area to to_area
/// net_transfer[from → to] ≤ limit_mw[(from_area, to_area)]
/// ```
///
/// Area assignment uses `load_area[b]` (index into bus list). Generator-area
/// tags are not used for these interface rows.
///
/// `limits_mw` is directional: a positive entry `limits_mw[(0,1)] = 200.0`
/// means area 0 may export at most 200 MW to area 1.  The reverse flow
/// `limits_mw[(1,0)]` is a separate entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TieLineLimits {
    /// Area pair (from_area, to_area) → max export MW (positive = from→to).
    /// Tuple-keyed map; schema renders the JSON shape used by serde_json
    /// (an array of [[from_area, to_area], limit_mw] pairs).
    #[schemars(with = "Vec<((usize, usize), f64)>")]
    pub limits_mw: HashMap<(usize, usize), f64>,
}

impl TieLineLimits {
    /// Returns `true` if there are no tie-line limits defined.
    pub fn is_empty(&self) -> bool {
        self.limits_mw.is_empty()
    }
}

// ─── DISP-09: Must-run / Reliability Must-Run (RMR) floors ─────────────────

/// Generator indices that must produce at least `pmin_mw` at all times.
///
/// In an LP (SCED), the lower bound of `p_i` is raised to `pmin[i]` for
/// each must-run unit.  In a MILP (SCUC), the binary commitment variable
/// `u[g,t]` is forced to 1 (identical to the inline `must_run` generator
/// field).
///
/// This struct allows injecting must-run floors externally — e.g. from an
/// RMR contract list — without modifying the network model.
///
/// Units are specified as indices into the **in-service generator list**,
/// i.e. the same ordering used in `DispatchPeriodResult::pg_mw`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MustRunUnits {
    /// In-service generator indices that must produce at least pmin_mw.
    pub unit_indices: Vec<usize>,
}

impl MustRunUnits {
    /// Returns `true` if generator at in-service index `j` is must-run.
    pub fn contains(&self, j: usize) -> bool {
        self.unit_indices.contains(&j)
    }
}
