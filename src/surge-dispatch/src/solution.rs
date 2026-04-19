// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Dispatch solution types shared by SCED, SCUC, and multi-period dispatch.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use surge_network::market::{DemandResponseResults, PowerBalanceViolation, VirtualBidResult};
use surge_solution::{ObjectiveTerm, ParResult};

use crate::result::{CommitmentSource, ConstraintKind, ConstraintScope, ReserveScope};

/// Per-resource market and physical outcome for one period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawResourcePeriodResult {
    /// Stable resource id from `DispatchSolution.resources`.
    pub resource_id: String,
    /// Signed power at the grid connection in MW. Positive = injection,
    /// negative = withdrawal.
    pub power_mw: f64,
    /// Commitment state when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment: Option<bool>,
    /// How the commitment state for this period was determined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment_source: Option<CommitmentSource>,
    /// Startup event when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<bool>,
    /// Shutdown event when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<bool>,
    /// Energy cost contribution for this resource in the period ($/hr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_cost: Option<f64>,
    /// No-load cost contribution for this resource in the period ($/hr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_load_cost: Option<f64>,
    /// Startup cost contribution for this resource in the period ($/hr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_cost: Option<f64>,
    /// Total reserve awards by product (MW).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reserve_awards: HashMap<String, f64>,
    /// Reserve offer cost contribution by product ($/hr).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reserve_costs: HashMap<String, f64>,
    /// Regulation participation state when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regulation: Option<bool>,
    /// Storage state of charge at end of period (MWh) when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_soc_mwh: Option<f64>,
    /// Resource CO2 emissions in tonnes for the period when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co2_t: Option<f64>,
    /// Resource reactive-power output when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_mvar: Option<f64>,
    /// Storage charging power in MW when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_mw: Option<f64>,
    /// Storage discharging power in MW when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discharge_mw: Option<f64>,
    /// Dispatchable-load served reactive power in MVAr when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_q_mvar: Option<f64>,
    /// Dispatchable-load curtailed power in MW when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curtailed_mw: Option<f64>,
    /// Dispatchable-load curtailment percentage when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curtailment_pct: Option<f64>,
    /// Dispatchable-load nodal price at the serving bus when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lmp_at_bus: Option<f64>,
    /// Net economic benefit of dispatchable-load curtailment when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_curtailment_benefit: Option<f64>,
}

/// Per-bus market and physical outcome for one period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawBusPeriodResult {
    /// External bus number.
    pub bus_number: u32,
    /// Full nodal price ($/MWh).
    pub lmp: f64,
    /// Marginal energy component ($/MWh).
    pub mec: f64,
    /// Marginal congestion component ($/MWh).
    pub mcc: f64,
    /// Marginal loss component ($/MWh).
    pub mlc: f64,
    /// Bus angle in radians when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_rad: Option<f64>,
    /// Bus voltage magnitude in per-unit when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage_pu: Option<f64>,
    /// Net injection at the bus in MW after withdrawals.
    pub net_injection_mw: f64,
    /// Total withdrawals at the bus in MW (consumer demand only, excludes losses).
    pub withdrawals_mw: f64,
    /// DC transmission loss allocation at this bus in MW.
    /// Losses allocated proportional to bus load. Zero when loss factors disabled.
    #[serde(default, skip_serializing_if = "crate::is_zero_f64")]
    pub loss_allocation_mw: f64,
    /// Net reactive injection at the bus in MVAr when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_reactive_injection_mvar: Option<f64>,
    /// Total withdrawals at the bus in MVAr when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawals_mvar: Option<f64>,
    /// Positive reactive-power balance slack at this bus (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_slack_pos_mvar: Option<f64>,
    /// Negative reactive-power balance slack at this bus (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_slack_neg_mvar: Option<f64>,
    /// Positive active-power balance slack at this bus (MW).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_slack_pos_mw: Option<f64>,
    /// Negative active-power balance slack at this bus (MW).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_slack_neg_mw: Option<f64>,
}

/// Cleared reserve-market result for one requirement bucket in one period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawReservePeriodResult {
    /// Reserve product id.
    pub product_id: String,
    /// Requirement scope: `system` or `zone`.
    pub scope: ReserveScope,
    /// Zone id when scope is `zone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<usize>,
    /// Requirement in MW.
    pub requirement_mw: f64,
    /// Cleared/provided MW.
    pub provided_mw: f64,
    /// Unmet requirement in MW.
    pub shortfall_mw: f64,
    /// Clearing price ($/MWh).
    pub clearing_price: f64,
    /// Penalty cost for shortfall (dollars for this period).
    #[serde(default)]
    pub shortfall_cost: f64,
}

/// Generic promoted constraint result for one period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawConstraintPeriodResult {
    /// Stable public constraint id.
    pub constraint_id: String,
    /// Constraint family/kind.
    pub kind: ConstraintKind,
    /// Scope descriptor such as `system`, `flowgate`, `interface`.
    pub scope: ConstraintScope,
    /// Shadow price when meaningful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_price: Option<f64>,
    /// Slack or violation magnitude when meaningful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack_mw: Option<f64>,
    /// Penalty rate when the constraint is softened ($/MW or $/MVA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub penalty_cost: Option<f64>,
    /// Actual penalty dollars for this period (slack × rate × dt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub penalty_dollars: Option<f64>,
}

/// Per-link HVDC outcome for one period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawHvdcBandPeriodResult {
    /// Stable public HVDC band id.
    pub band_id: String,
    /// Final dispatch setpoint (MW) for this band.
    pub mw: f64,
}

/// Per-link HVDC outcome for one period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawHvdcPeriodResult {
    /// Stable public HVDC link id.
    pub link_id: String,
    /// Human-readable link name.
    pub name: String,
    /// Final dispatch setpoint (MW).
    pub mw: f64,
    /// Delivered MW after losses.
    pub delivered_mw: f64,
    /// Per-band dispatch outcomes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub band_results: Vec<RawHvdcBandPeriodResult>,
}

/// Period emissions rollup and per-resource breakdown.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawEmissionsPeriodResult {
    /// Total CO2 emissions in tonnes for the period.
    pub total_co2_t: f64,
    /// Per-resource CO2 emissions in tonnes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub by_resource_t: HashMap<String, f64>,
}

/// Frequency-security metrics for one period when configured.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawFrequencyPeriodResult {
    /// System inertia at the dispatch point (seconds).
    pub system_inertia_s: f64,
    /// Estimated initial RoCoF for the configured event (Hz/s).
    pub estimated_rocof_hz_per_s: f64,
    /// Whether the configured frequency-security constraints are satisfied.
    pub frequency_secure: bool,
}

// ---------------------------------------------------------------------------
// RawDispatchPeriodResult — shared per-period result
// ---------------------------------------------------------------------------

/// Per-period dispatch result shared by SCED, SCUC, and multi-period dispatch.
///
/// Contains generator dispatch, LMPs (with energy/congestion decomposition),
/// generic per-product reserve awards, clearing prices, and emissions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawDispatchPeriodResult {
    /// Generator dispatch in MW (one per in-service generator).
    pub pg_mw: Vec<f64>,
    /// Locational marginal price at each bus ($/MWh).
    pub lmp: Vec<f64>,
    /// LMP energy component at each bus ($/MWh). Equal to the reference bus LMP.
    pub lmp_energy: Vec<f64>,
    /// LMP congestion component at each bus ($/MWh). `lmp - lmp_energy`.
    pub lmp_congestion: Vec<f64>,
    /// Total production cost for this period ($/hr).
    pub total_cost: f64,
    /// CO2 emissions in tonnes for this period.
    pub co2_t: f64,
    /// HVDC dispatch in MW (one per HVDC link). Empty if no HVDC links.
    pub hvdc_dispatch_mw: Vec<f64>,
    /// Per-band HVDC dispatch (MW). Outer vec indexed by HVDC link; inner vec
    /// indexed by band. Empty inner vec for legacy (non-banded) links.
    pub hvdc_band_dispatch_mw: Vec<Vec<f64>>,
    /// Storage charge power per unit (MW). Empty if no storage.
    pub storage_charge_mw: Vec<f64>,
    /// Storage discharge power per unit (MW). Empty if no storage.
    pub storage_discharge_mw: Vec<f64>,
    /// End-of-period SoC per storage unit (MWh). Empty if no storage.
    pub storage_soc_mwh: Vec<f64>,
    /// LMP loss component at each bus ($/MWh).
    /// Computed from DC marginal loss factors when enabled.
    pub lmp_loss: Vec<f64>,
    /// Per-product per-generator reserve awards (MW).
    /// Outer key: product_id. Inner vec: one per in-service generator.
    pub reserve_awards: HashMap<String, Vec<f64>>,
    /// Per-product clearing price ($/MWh).
    pub reserve_prices: HashMap<String, f64>,
    /// Per-product total provided (MW).
    pub reserve_provided: HashMap<String, f64>,
    /// Per-product unmet system reserve requirement (MW).
    pub reserve_shortfall: HashMap<String, f64>,
    /// Per-(zone_id, product_id) clearing prices ($/MWh).
    pub zonal_reserve_prices: HashMap<String, f64>,
    /// Per-(zone_id, product_id) unmet zonal reserve requirement (MW).
    pub zonal_reserve_shortfall: HashMap<String, f64>,
    /// Per-product per-DL reserve awards (MW).
    /// Outer key: product_id. Inner vec: one per in-service dispatchable load.
    pub dr_reserve_awards: HashMap<String, Vec<f64>>,
    /// PAR implied shift angles, one per configured PAR setpoint.
    pub par_results: Vec<ParResult>,
    /// Shadow prices for branch thermal constraints ($/MWh), ordered by constrained branch rows.
    ///
    /// Positive = forward direction binding; negative = reverse binding; zero = slack.
    pub branch_shadow_prices: Vec<f64>,
    /// Shadow prices for flowgate constraints ($/MWh), indexed by `Network::flowgates`.
    ///
    /// Positive = forward direction binding; negative = reverse binding; zero = slack.
    pub flowgate_shadow_prices: Vec<f64>,
    /// Shadow prices for interface constraints ($/MWh), indexed by `Network::interfaces`.
    pub interface_shadow_prices: Vec<f64>,
    /// Demand-response dispatch outcomes.
    pub dr_results: DemandResponseResults,
    /// Virtual bid clearing results. Empty when no virtual bids were submitted.
    pub virtual_bid_results: Vec<VirtualBidResult>,
    /// Power balance violation for this period.
    /// Non-zero when penalty slacks absorbed a generation–load imbalance.
    #[serde(default)]
    pub power_balance_violation: PowerBalanceViolation,
    /// Exact period objective decomposition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objective_terms: Vec<ObjectiveTerm>,
    /// Keyed resource-native period outcomes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_results: Vec<RawResourcePeriodResult>,
    /// Keyed bus-native period outcomes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_results: Vec<RawBusPeriodResult>,
    /// Reserve market clearing results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserve_results: Vec<RawReservePeriodResult>,
    /// Promoted constraint results and pricing signals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraint_results: Vec<RawConstraintPeriodResult>,
    /// Keyed HVDC link results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hvdc_results: Vec<RawHvdcPeriodResult>,
    /// Rounded transformer tap dispatch `(branch_idx, continuous_tap, rounded_tap)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tap_dispatch: Vec<(usize, f64, f64)>,
    /// Rounded phase-shifter dispatch `(branch_idx, continuous_rad, rounded_rad)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_dispatch: Vec<(usize, f64, f64)>,
    /// Rounded switched-shunt dispatch
    /// `(control_id, bus_number, continuous_b_pu, rounded_b_pu)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switched_shunt_dispatch: Vec<(String, u32, f64, f64)>,
    /// Emissions breakdown for the period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissions_results: Option<RawEmissionsPeriodResult>,
    /// Frequency-security metrics for the period when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_results: Option<RawFrequencyPeriodResult>,
    /// SCED-AC Benders epigraph variable value (`$/hr`) for this period.
    /// `None` when the period did not allocate an `eta` variable (i.e. the
    /// request did not opt in to SCED-AC Benders for this period). When
    /// `Some(value)`, the value is the LP-optimal lower bound on the AC
    /// physics adder consistent with all currently-applied Benders cuts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sced_ac_benders_eta_dollars_per_hour: Option<f64>,
}

// ---------------------------------------------------------------------------
// RawScedSolution
// ---------------------------------------------------------------------------

/// Test-only single-period SCED compatibility result.
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawScedSolution {
    /// Per-period dispatch, LMP, reserves, prices, cost.
    #[serde(flatten)]
    pub dispatch: RawDispatchPeriodResult,

    /// Solve time in seconds.
    pub solve_time_secs: f64,
    /// Solver iterations.
    pub iterations: u32,

    /// System inertia H (seconds) at the dispatch point. 0 if not computed.
    pub system_inertia_s: f64,
    /// Estimated initial RoCoF (Hz/s) for the configured event. 0 if not computed.
    pub estimated_rocof_hz_per_s: f64,
    /// Whether frequency constraints are satisfied. True if no constraints configured.
    pub frequency_secure: bool,
}
