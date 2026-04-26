// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Generator representation.

use num_complex::Complex64;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::market::{CostCurve, EmissionRates, EnergyOffer};

// ── StorageDispatchMode ───────────────────────────────────────────────────

/// How the optimizer dispatches a storage generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StorageDispatchMode {
    /// Optimizer minimizes total system cost — storage charges/discharges optimally.
    /// Efficiency losses plus `variable_cost_per_mwh` and `degradation_cost_per_mwh`
    /// are baked into the LP/NLP objective alongside generator costs.  The optimizer
    /// places storage where it is most valuable given its costs; no external price
    /// signal is needed (marginal prices emerge endogenously from the dispatch LP).
    #[default]
    CostMinimization,
    /// BESS submits offer curves like a generator.  `discharge_offer` defines the
    /// cumulative `(MW, $/hr)` PWL breakpoints for selling power; `charge_bid`
    /// defines the cumulative `(MW, $/hr)` breakpoints for buying power. Curves
    /// must start with an explicit `(0.0, 0.0)` origin and at least one
    /// additional breakpoint. The optimizer clears the BESS against these curves
    /// at the endogenous bus LMP.
    OfferCurve,
    /// Operator pre-commits a fixed net injection for every period.  `self_schedule_mw`
    /// (positive = discharge, negative = charge) is injected as-is (clamped by SoC
    /// limits).  The schedule is **not** optimized — the BESS is dispatched exactly
    /// at the committed level regardless of prices.
    SelfSchedule,
}

// ── StorageParams ─────────────────────────────────────────────────────────

/// Storage-specific parameters for generators with energy storage capability.
///
/// Present on generators that are batteries (BESS), pumped hydro acting as
/// storage, or any other energy-limited bidirectional resource.
/// When `Some`, `Generator.pmin` is negative (= -charge_mw_max) and
/// `Generator.pmax` is the discharge power limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "StorageParamsWire")]
pub struct StorageParams {
    /// Charge-side efficiency (0 < eta <= 1): fraction of metered charge MW
    /// that reaches the SoC reservoir. Typical lithium-ion batteries lose
    /// most of their round-trip on this leg (~90%).
    pub charge_efficiency: f64,
    /// Discharge-side efficiency (0 < eta <= 1): fraction of SoC draw that
    /// reaches the grid as metered discharge MW. Typically higher than the
    /// charge-side (~98% for modern inverters).
    pub discharge_efficiency: f64,
    /// Usable energy capacity (MWh).
    pub energy_capacity_mwh: f64,
    /// Initial state of charge (MWh). Must be in [soc_min_mwh, soc_max_mwh].
    pub soc_initial_mwh: f64,
    /// Minimum allowable SoC (MWh).
    pub soc_min_mwh: f64,
    /// Maximum allowable SoC (MWh). Typically equal to energy_capacity_mwh.
    pub soc_max_mwh: f64,
    /// Variable cost per MWh discharged ($/MWh). Used in CostMinimization mode.
    #[serde(default)]
    pub variable_cost_per_mwh: f64,
    /// Degradation cost per MWh throughput ($/MWh), applied to both charge
    /// and discharge.
    #[serde(default)]
    pub degradation_cost_per_mwh: f64,
    /// How the optimizer dispatches this unit.
    #[serde(default)]
    pub dispatch_mode: StorageDispatchMode,
    /// Pre-committed net dispatch (MW). SelfSchedule mode only.
    #[serde(default)]
    pub self_schedule_mw: f64,
    /// Discharge offer curve: cumulative `(MW, $/hr)` breakpoints with an
    /// explicit `(0.0, 0.0)` origin. OfferCurve mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discharge_offer: Option<Vec<(f64, f64)>>,
    /// Charge bid curve: cumulative `(MW, $/hr)` breakpoints with an explicit
    /// `(0.0, 0.0)` origin. OfferCurve mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_bid: Option<Vec<(f64, f64)>>,
    /// Max charge C-rate (e.g. 0.25 for 4-hr battery). None = inverter-limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_c_rate_charge: Option<f64>,
    /// Max discharge C-rate. None = inverter-limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_c_rate_discharge: Option<f64>,
    /// Battery chemistry (informational): "LFP", "NMC", "flow", "sodium".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chemistry: Option<String>,
    /// Discharge-side foldback threshold (MWh of SoC). Above this, the
    /// battery can reach its full discharge MW cap; below, the cap
    /// derates linearly to 0 MW at ``soc_min_mwh``. ``None`` disables
    /// the foldback cut entirely. Typical lithium-ion value: a few
    /// percent of energy capacity above ``soc_min_mwh``.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discharge_foldback_soc_mwh: Option<f64>,
    /// Charge-side foldback threshold (MWh of SoC). Below this, the
    /// battery can reach its full charge MW cap; above, the cap
    /// derates linearly to 0 MW at ``soc_max_mwh``. ``None`` disables
    /// the foldback cut. Typical lithium-ion value: a few percent of
    /// energy capacity below ``soc_max_mwh``.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_foldback_soc_mwh: Option<f64>,
    /// Maximum full equivalent cycles per 24-hour window. One FEC =
    /// one full charge + one full discharge, i.e. throughput of
    /// `2 × energy_capacity_mwh`. The dispatch enforces it as a
    /// linear cap on `Σ_t (charge_mw[t] + discharge_mw[t]) · dt`
    /// inside each 24-hour bucket of the horizon (partial days are
    /// pro-rated). Only the time-coupled SCUC build honours this —
    /// in period-by-period SCED there is no inter-period coupling
    /// to enforce against. ``None`` disables the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_cycle_limit: Option<f64>,
}

/// Wire representation of [`StorageParams`] that accepts either the new
/// split (`charge_efficiency` / `discharge_efficiency`) or the legacy
/// single round-trip `efficiency` field. Used via `#[serde(from = ...)]`
/// so deserialization of older JSON cases keeps working.
#[derive(Deserialize)]
struct StorageParamsWire {
    #[serde(default)]
    charge_efficiency: Option<f64>,
    #[serde(default)]
    discharge_efficiency: Option<f64>,
    /// Legacy single-field round-trip efficiency — split sqrt-per-leg
    /// when the two new fields are absent.
    #[serde(default)]
    efficiency: Option<f64>,
    energy_capacity_mwh: f64,
    soc_initial_mwh: f64,
    soc_min_mwh: f64,
    soc_max_mwh: f64,
    #[serde(default)]
    variable_cost_per_mwh: f64,
    #[serde(default)]
    degradation_cost_per_mwh: f64,
    #[serde(default)]
    dispatch_mode: StorageDispatchMode,
    #[serde(default)]
    self_schedule_mw: f64,
    #[serde(default)]
    discharge_offer: Option<Vec<(f64, f64)>>,
    #[serde(default)]
    charge_bid: Option<Vec<(f64, f64)>>,
    #[serde(default)]
    max_c_rate_charge: Option<f64>,
    #[serde(default)]
    max_c_rate_discharge: Option<f64>,
    #[serde(default)]
    chemistry: Option<String>,
    #[serde(default)]
    discharge_foldback_soc_mwh: Option<f64>,
    #[serde(default)]
    charge_foldback_soc_mwh: Option<f64>,
    #[serde(default)]
    daily_cycle_limit: Option<f64>,
}

impl From<StorageParamsWire> for StorageParams {
    fn from(w: StorageParamsWire) -> Self {
        let (charge_efficiency, discharge_efficiency) =
            match (w.charge_efficiency, w.discharge_efficiency, w.efficiency) {
                (Some(c), Some(d), _) => (c, d),
                (Some(c), None, _) => (c, 0.98),
                (None, Some(d), _) => (0.90, d),
                (None, None, Some(rt)) => {
                    let leg = rt.max(0.0).sqrt();
                    (leg, leg)
                }
                (None, None, None) => (0.90, 0.98),
            };
        Self {
            charge_efficiency,
            discharge_efficiency,
            energy_capacity_mwh: w.energy_capacity_mwh,
            soc_initial_mwh: w.soc_initial_mwh,
            soc_min_mwh: w.soc_min_mwh,
            soc_max_mwh: w.soc_max_mwh,
            variable_cost_per_mwh: w.variable_cost_per_mwh,
            degradation_cost_per_mwh: w.degradation_cost_per_mwh,
            dispatch_mode: w.dispatch_mode,
            self_schedule_mw: w.self_schedule_mw,
            discharge_offer: w.discharge_offer,
            charge_bid: w.charge_bid,
            max_c_rate_charge: w.max_c_rate_charge,
            max_c_rate_discharge: w.max_c_rate_discharge,
            chemistry: w.chemistry,
            discharge_foldback_soc_mwh: w.discharge_foldback_soc_mwh,
            charge_foldback_soc_mwh: w.charge_foldback_soc_mwh,
            daily_cycle_limit: w.daily_cycle_limit,
        }
    }
}

/// Validation error for [`StorageParams`].
#[derive(Debug, Clone, Error, PartialEq)]
pub enum StorageValidationError {
    #[error("charge_efficiency must be in (0, 1], got {0}")]
    InvalidChargeEfficiency(f64),
    #[error("discharge_efficiency must be in (0, 1], got {0}")]
    InvalidDischargeEfficiency(f64),
    #[error("energy_capacity_mwh must be > 0, got {0}")]
    InvalidEnergyCapacity(f64),
    #[error("soc_min_mwh ({soc_min_mwh}) exceeds soc_max_mwh ({soc_max_mwh})")]
    InvalidSocRange { soc_min_mwh: f64, soc_max_mwh: f64 },
    #[error(
        "soc_initial_mwh ({soc_initial_mwh}) must lie within [soc_min_mwh ({soc_min_mwh}), soc_max_mwh ({soc_max_mwh})]"
    )]
    InvalidInitialSoc {
        soc_initial_mwh: f64,
        soc_min_mwh: f64,
        soc_max_mwh: f64,
    },
    #[error(
        "discharge_foldback_soc_mwh ({threshold}) must lie in (soc_min_mwh ({soc_min}), soc_max_mwh ({soc_max})]; outside this range the foldback cut is either empty or covers the whole range"
    )]
    InvalidDischargeFoldback {
        threshold: f64,
        soc_min: f64,
        soc_max: f64,
    },
    #[error(
        "charge_foldback_soc_mwh ({threshold}) must lie in [soc_min_mwh ({soc_min}), soc_max_mwh ({soc_max})); outside this range the foldback cut is either empty or covers the whole range"
    )]
    InvalidChargeFoldback {
        threshold: f64,
        soc_min: f64,
        soc_max: f64,
    },
    #[error("daily_cycle_limit must be > 0, got {0}")]
    InvalidDailyCycleLimit(f64),
}

impl StorageParams {
    /// Construct with sensible defaults for a storage resource with the given
    /// energy capacity.
    ///
    /// Power limits live on the parent [`Generator`]:
    /// `Generator::pmin = -charge_mw_max` and
    /// `Generator::pmax = discharge_mw_max`.
    pub fn with_energy_capacity_mwh(energy_capacity_mwh: f64) -> Self {
        Self {
            charge_efficiency: 0.90,
            discharge_efficiency: 0.98,
            energy_capacity_mwh,
            soc_initial_mwh: 0.5 * energy_capacity_mwh,
            soc_min_mwh: 0.0,
            soc_max_mwh: energy_capacity_mwh,
            variable_cost_per_mwh: 0.0,
            degradation_cost_per_mwh: 0.0,
            dispatch_mode: StorageDispatchMode::default(),
            self_schedule_mw: 0.0,
            discharge_offer: None,
            charge_bid: None,
            max_c_rate_charge: None,
            max_c_rate_discharge: None,
            chemistry: None,
            discharge_foldback_soc_mwh: None,
            charge_foldback_soc_mwh: None,
            daily_cycle_limit: None,
        }
    }

    /// Round-trip efficiency implied by the charge/discharge pair. Useful for
    /// reporting and tests where a single figure is expected.
    pub fn round_trip_efficiency(&self) -> f64 {
        self.charge_efficiency * self.discharge_efficiency
    }

    /// Construct a symmetric split from a single round-trip figure. Convenience
    /// for callers that only have a nameplate round-trip number and want the
    /// legacy sqrt-per-leg behaviour.
    pub fn from_round_trip(energy_capacity_mwh: f64, round_trip: f64) -> Self {
        let leg = round_trip.max(0.0).sqrt();
        Self {
            charge_efficiency: leg,
            discharge_efficiency: leg,
            ..Self::with_energy_capacity_mwh(energy_capacity_mwh)
        }
    }

    /// Validate storage parameters.
    pub fn validate(&self) -> Result<(), StorageValidationError> {
        if self.charge_efficiency <= 0.0 || self.charge_efficiency > 1.0 {
            return Err(StorageValidationError::InvalidChargeEfficiency(
                self.charge_efficiency,
            ));
        }
        if self.discharge_efficiency <= 0.0 || self.discharge_efficiency > 1.0 {
            return Err(StorageValidationError::InvalidDischargeEfficiency(
                self.discharge_efficiency,
            ));
        }
        if self.energy_capacity_mwh <= 0.0 {
            return Err(StorageValidationError::InvalidEnergyCapacity(
                self.energy_capacity_mwh,
            ));
        }
        if self.soc_min_mwh > self.soc_max_mwh {
            return Err(StorageValidationError::InvalidSocRange {
                soc_min_mwh: self.soc_min_mwh,
                soc_max_mwh: self.soc_max_mwh,
            });
        }
        if self.soc_initial_mwh < self.soc_min_mwh || self.soc_initial_mwh > self.soc_max_mwh {
            return Err(StorageValidationError::InvalidInitialSoc {
                soc_initial_mwh: self.soc_initial_mwh,
                soc_min_mwh: self.soc_min_mwh,
                soc_max_mwh: self.soc_max_mwh,
            });
        }
        if let Some(t) = self.discharge_foldback_soc_mwh {
            if t <= self.soc_min_mwh || t > self.soc_max_mwh {
                return Err(StorageValidationError::InvalidDischargeFoldback {
                    threshold: t,
                    soc_min: self.soc_min_mwh,
                    soc_max: self.soc_max_mwh,
                });
            }
        }
        if let Some(t) = self.charge_foldback_soc_mwh {
            if t < self.soc_min_mwh || t >= self.soc_max_mwh {
                return Err(StorageValidationError::InvalidChargeFoldback {
                    threshold: t,
                    soc_min: self.soc_min_mwh,
                    soc_max: self.soc_max_mwh,
                });
            }
        }
        if let Some(limit) = self.daily_cycle_limit {
            if !limit.is_finite() || limit <= 0.0 {
                return Err(StorageValidationError::InvalidDailyCycleLimit(limit));
            }
        }
        Ok(())
    }

    /// Validate a market bid/offer curve used by storage dispatch.
    ///
    /// Curves must include an explicit `(0.0, 0.0)` origin followed by at
    /// least one additional strictly increasing MW breakpoint.
    pub fn validate_market_curve_points(
        points: &[(f64, f64)],
        curve_label: &str,
    ) -> Result<(), String> {
        if points.len() < 2 {
            return Err(format!(
                "{curve_label} must include an explicit origin (0.0, 0.0) and at least one additional breakpoint"
            ));
        }

        let (mw0, cost0) = points[0];
        if !mw0.is_finite() || !cost0.is_finite() {
            return Err(format!(
                "{curve_label} origin breakpoint must be finite, got ({mw0}, {cost0})"
            ));
        }
        if mw0.abs() > 1e-9 || cost0.abs() > 1e-9 {
            return Err(format!(
                "{curve_label} must start at an explicit origin breakpoint (0.0, 0.0), got ({mw0}, {cost0})"
            ));
        }

        let mut prev_mw = mw0;
        for (point_idx, &(mw, cost)) in points.iter().enumerate().skip(1) {
            if !mw.is_finite() || !cost.is_finite() {
                return Err(format!(
                    "{curve_label} breakpoint {point_idx} must be finite, got ({mw}, {cost})"
                ));
            }
            if mw <= prev_mw + 1e-9 {
                return Err(format!(
                    "{curve_label} MW breakpoints must be strictly increasing after the origin; breakpoint {point_idx} has MW {mw} after {prev_mw}"
                ));
            }
            prev_mw = mw;
        }

        Ok(())
    }

    /// Evaluate a storage market bid/offer curve at a given MW level.
    pub fn market_curve_value(points: &[(f64, f64)], mw: f64) -> f64 {
        CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: points.to_vec(),
        }
        .evaluate(mw.max(0.0))
    }

    /// Evaluate the marginal value of a storage market bid/offer curve.
    pub fn market_curve_marginal_value(points: &[(f64, f64)], mw: f64) -> f64 {
        CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: points.to_vec(),
        }
        .marginal_cost(mw.max(0.0))
    }
}

/// Generator electrical class.
///
/// This captures the machine's electrical interface to the network rather than
/// the plant/resource technology or fuel. Keep technology and fuel in their
/// dedicated fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GenType {
    /// Conventional synchronous machine with electromechanical swing dynamics.
    Synchronous,
    /// Asynchronous / induction machine directly coupled to the grid.
    Asynchronous,
    /// Power-electronics-interfaced inverter-based resource.
    #[serde(alias = "Wind", alias = "Solar", alias = "InverterOther")]
    InverterBased,
    /// Hybrid resource combining multiple electrical interfaces.
    Hybrid,
    /// Electrical class is not known from the source data.
    #[default]
    Unknown,
}

/// Generator plant/resource technology classification.
///
/// This complements [`GenType`] by describing what the unit is, while
/// `gen_type` describes how it connects electrically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneratorTechnology {
    Thermal,
    SteamTurbine,
    CombustionTurbine,
    CombinedCycle,
    InternalCombustion,
    Hydro,
    PumpedStorage,
    Hydrokinetic,
    Nuclear,
    Geothermal,
    Wind,
    Solar,
    SolarPv,
    SolarThermal,
    Wave,
    Storage,
    BatteryStorage,
    CompressedAirStorage,
    FlywheelStorage,
    FuelCell,
    SynchronousCondenser,
    StaticVarCompensator,
    Motor,
    DispatchableLoad,
    DcTie,
    Other,
}

/// Unit commitment status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CommitmentStatus {
    /// ISO decides on/off based on economics.
    #[default]
    Market,
    /// Operator chose to be online, ISO dispatches energy.
    SelfCommitted,
    /// Required online for reliability.
    MustRun,
    /// On outage, not offered.
    Unavailable,
    /// Available only if ISO declares emergency.
    EmergencyOnly,
}

/// Fuel supply characteristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuelSupply {
    /// Fuel name: "natural_gas", "oil", "diesel", "coal", "uranium".
    pub fuel: String,
    /// Fuel price ($/MMBtu).
    pub price_per_mmbtu: f64,
    /// Heat rate curve: (MW, BTU/MWh) segments. Empty = use flat heat_rate.
    pub heat_rate_curve: Vec<(f64, f64)>,
    /// Daily fuel limit (MMBtu/day). None = unlimited.
    pub daily_limit_mmbtu: Option<f64>,
    /// Minimum fuel take (MMBtu/day). Take-or-pay. None = 0.
    pub min_take_mmbtu: Option<f64>,
}

// ── Sub-structs ───────────────────────────────────────────────────────────

/// Unit commitment parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitmentParams {
    /// Unit commitment status.
    #[serde(default)]
    pub status: CommitmentStatus,
    /// Economic minimum (MW). May be > pmin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_ecomin: Option<f64>,
    /// Economic maximum (MW). May be < pmax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_ecomax: Option<f64>,
    /// Emergency minimum (MW). Below economic min.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_emergency_min: Option<f64>,
    /// Emergency maximum (MW). Above economic max.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_emergency_max: Option<f64>,
    /// Regulation-mode minimum (MW). Active when unit is providing regulation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_reg_min: Option<f64>,
    /// Regulation-mode maximum (MW). Active when unit is providing regulation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_reg_max: Option<f64>,
    /// Minimum up time in hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_up_time_hr: Option<f64>,
    /// Minimum down time in hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_down_time_hr: Option<f64>,
    /// Max continuous run time (hours). None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_up_time_hr: Option<f64>,
    /// Min soak time at pmin after sync (hours).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_run_at_pmin_hr: Option<f64>,
    /// Maximum startup events in any rolling 24-hour window. None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_starts_per_day: Option<u32>,
    /// Maximum startup events in any rolling 7-day (168h) window. None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_starts_per_week: Option<u32>,
    /// Maximum energy output in any rolling 24-hour window (MWh). None = unlimited.
    /// For hydro, demand response, and energy-limited resources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_energy_mwh_per_day: Option<f64>,
    /// Shutdown ramp rate (MW/min). Used for de-loading constraints when
    /// `enforce_shutdown_deloading` is enabled. Falls back to `ramp_down_mw_per_min()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_ramp_mw_per_min: Option<f64>,
    /// Startup ramp rate (MW/min). Maximum rate at which the unit can increase
    /// output from zero during startup. Used for de-loading constraints when
    /// `enforce_shutdown_deloading` is enabled. Falls back to `ramp_up_mw_per_min()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_ramp_mw_per_min: Option<f64>,
    /// Forbidden operating zones: (low_mw, high_mw) ranges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_zones: Vec<(f64, f64)>,
    /// Hours online so far (SCUC warm-start).
    #[serde(default)]
    pub hours_online: f64,
    /// Hours offline so far (startup tier selection).
    #[serde(default)]
    pub hours_offline: f64,
}

impl Default for CommitmentParams {
    fn default() -> Self {
        Self {
            status: CommitmentStatus::Market,
            p_ecomin: None,
            p_ecomax: None,
            p_emergency_min: None,
            p_emergency_max: None,
            p_reg_min: None,
            p_reg_max: None,
            min_up_time_hr: None,
            min_down_time_hr: None,
            max_up_time_hr: None,
            min_run_at_pmin_hr: None,
            max_starts_per_day: None,
            max_starts_per_week: None,
            max_energy_mwh_per_day: None,
            shutdown_ramp_mw_per_min: None,
            startup_ramp_mw_per_min: None,
            forbidden_zones: Vec::new(),
            hours_online: 0.0,
            hours_offline: 0.0,
        }
    }
}

/// Piecewise ramp curve parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RampingParams {
    /// Normal ramp-up: (MW operating point, MW/min). Empty = unlimited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ramp_up_curve: Vec<(f64, f64)>,
    /// Normal ramp-down. Empty = unlimited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ramp_down_curve: Vec<(f64, f64)>,
    /// Emergency ramp-up. Empty = use ramp_up_curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emergency_ramp_up_curve: Vec<(f64, f64)>,
    /// Emergency ramp-down. Empty = use ramp_down_curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emergency_ramp_down_curve: Vec<(f64, f64)>,
    /// Regulation ramp-up (AGC following). Empty = use ramp_up_curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reg_ramp_up_curve: Vec<(f64, f64)>,
    /// Regulation ramp-down. Empty = use ramp_down_curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reg_ramp_down_curve: Vec<(f64, f64)>,
}

impl RampingParams {
    /// Scalar ramp-up rate (MW/min) — first segment of ramp_up_curve.
    /// Returns None if curve is empty.
    #[inline]
    pub fn ramp_up_mw_per_min(&self) -> Option<f64> {
        self.ramp_up_curve.first().map(|&(_, rate)| rate)
    }

    /// Scalar ramp-down rate (MW/min) — first segment, falls back to ramp_up.
    #[inline]
    pub fn ramp_down_mw_per_min(&self) -> Option<f64> {
        self.ramp_down_curve
            .first()
            .or(self.ramp_up_curve.first())
            .map(|&(_, rate)| rate)
    }

    /// AGC/regulation ramp rate (MW/min) — first segment, falls back to ramp_up.
    #[inline]
    pub fn ramp_agc_mw_per_min(&self) -> Option<f64> {
        self.reg_ramp_up_curve
            .first()
            .or(self.ramp_up_curve.first())
            .map(|&(_, rate)| rate)
    }

    /// Interpolate ramp-up rate at a given MW operating point.
    pub fn ramp_up_at_mw(&self, p_mw: f64) -> Option<f64> {
        ramp_rate_at_mw(&self.ramp_up_curve, p_mw)
    }

    /// Interpolate ramp-down rate at a given MW operating point.
    /// Falls back to ramp_up_curve if ramp_down_curve is empty.
    pub fn ramp_down_at_mw(&self, p_mw: f64) -> Option<f64> {
        let curve = if self.ramp_down_curve.is_empty() {
            &self.ramp_up_curve
        } else {
            &self.ramp_down_curve
        };
        ramp_rate_at_mw(curve, p_mw)
    }

    /// Interpolate regulation ramp-up rate at a given MW operating point.
    /// Falls back to ramp_up_curve if reg curve is empty.
    pub fn reg_ramp_up_at_mw(&self, p_mw: f64) -> Option<f64> {
        let curve = if self.reg_ramp_up_curve.is_empty() {
            &self.ramp_up_curve
        } else {
            &self.reg_ramp_up_curve
        };
        ramp_rate_at_mw(curve, p_mw)
    }

    /// Interpolate regulation ramp-down rate at a given MW operating point.
    /// Falls back to ramp_down_curve, then ramp_up_curve.
    pub fn reg_ramp_down_at_mw(&self, p_mw: f64) -> Option<f64> {
        let curve = if !self.reg_ramp_down_curve.is_empty() {
            &self.reg_ramp_down_curve
        } else if !self.ramp_down_curve.is_empty() {
            &self.ramp_down_curve
        } else {
            &self.ramp_up_curve
        };
        ramp_rate_at_mw(curve, p_mw)
    }

    /// Weighted-average ramp-up rate over \[lo_mw, hi_mw\].
    pub fn ramp_up_avg(&self, lo_mw: f64, hi_mw: f64) -> Option<f64> {
        ramp_rate_avg(&self.ramp_up_curve, lo_mw, hi_mw)
    }

    /// Weighted-average ramp-down rate over \[lo_mw, hi_mw\].
    /// Falls back to ramp_up_curve if ramp_down_curve is empty.
    pub fn ramp_down_avg(&self, lo_mw: f64, hi_mw: f64) -> Option<f64> {
        let curve = if self.ramp_down_curve.is_empty() {
            &self.ramp_up_curve
        } else {
            &self.ramp_down_curve
        };
        ramp_rate_avg(curve, lo_mw, hi_mw)
    }
}

/// Inverter-specific parameters (ignored for synchronous machines).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InverterParams {
    /// Inverter apparent power rating (MVA). Defines P-Q capability circle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s_rated_mva: Option<f64>,
    /// Current weather-limited available power (MW). None = use pmax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_available_mw: Option<f64>,
    /// Can be curtailed below p_available. Default false.
    #[serde(default)]
    pub curtailable: bool,
    /// Can form voltage/frequency reference. Default false.
    #[serde(default)]
    pub grid_forming: bool,
    /// Inverter no-load loss (MW). Default 0.
    #[serde(default)]
    pub inverter_loss_a_mw: f64,
    /// Inverter proportional loss coefficient. Default 0.
    #[serde(default, alias = "inverter_loss_b")]
    pub inverter_loss_b_pu: f64,
}

impl Default for InverterParams {
    fn default() -> Self {
        Self {
            s_rated_mva: None,
            p_available_mw: None,
            curtailable: false,
            grid_forming: false,
            inverter_loss_a_mw: 0.0,
            inverter_loss_b_pu: 0.0,
        }
    }
}

/// Generator fault/sequence data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenFaultData {
    /// Machine leakage reactance (pu, machine base). Used as Xd' for GENCLS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xs: Option<f64>,
    /// Negative-sequence subtransient reactance X2 in per-unit (machine base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x2_pu: Option<f64>,
    /// Negative-sequence resistance R2 in per-unit (machine base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r2_pu: Option<f64>,
    /// Zero-sequence reactance X0 in per-unit (machine base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x0_pu: Option<f64>,
    /// Zero-sequence resistance R0 in per-unit (machine base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r0_pu: Option<f64>,
    /// Neutral grounding impedance Zn in per-unit (system base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zn: Option<Complex64>,
}

/// Reactive capability curve data (MATPOWER cols 10-15 + D-curve).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReactiveCapability {
    /// Sorted list of (p_pu, qmax_pu, qmin_pu) operating points.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pq_curve: Vec<(f64, f64, f64)>,
    /// Lower real power output point for Q capability curve (MW).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc1: Option<f64>,
    /// Upper real power output point for Q capability curve (MW).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc2: Option<f64>,
    /// Minimum reactive power at Pc1 (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc1min: Option<f64>,
    /// Maximum reactive power at Pc1 (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc1max: Option<f64>,
    /// Minimum reactive power at Pc2 (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc2min: Option<f64>,
    /// Maximum reactive power at Pc2 (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc2max: Option<f64>,
    /// GO Competition Challenge 3 §4.6 eq (116): linear EQUALITY linking
    /// `q = q_at_p_zero_pu + beta * p`. Devices in `J^pqe`. The PDF
    /// further constrains q-reserves to zero on these devices (eqs
    /// 117-118), enforced by the reserves layer when present. None of the
    /// public 73/617/2000-bus scenarios I inspected exercise this case
    /// (`q_linear_cap = 1` in the GO JSON), but the larger 4000+ bus
    /// problems may.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pq_linear_equality: Option<PqLinearLink>,
    /// GO Competition Challenge 3 §4.6 eq (114): linear UPPER bound
    /// `q + q^qru ≤ q_at_p_zero_pu + beta * p` on devices in `J^pqmax`.
    /// In GO C3 inputs this is signaled by `q_bound_cap = 1` together
    /// with `q_0_ub` and `beta_ub`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pq_linear_upper: Option<PqLinearLink>,
    /// GO Competition Challenge 3 §4.6 eq (115): linear LOWER bound
    /// `q − q^qrd ≥ q_at_p_zero_pu + beta * p` on devices in `J^pqmin`.
    /// In GO C3 inputs this is signaled by `q_bound_cap = 1` together
    /// with `q_0_lb` and `beta_lb`. The GO data property eq (229)
    /// guarantees `J^pqmax = J^pqmin`, so a device with `q_bound_cap = 1`
    /// always carries both `pq_linear_upper` and `pq_linear_lower`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pq_linear_lower: Option<PqLinearLink>,
}

/// Coefficients for a linear p-q linking constraint of the form
/// `q ⨀ q_at_p_zero_pu + beta * p` where `⨀` is `=`, `≤`, or `≥` depending
/// on the variant of `ReactiveCapability::pq_linear_*` that holds it.
///
/// All values are in per-unit on the device's machine base. The full GO
/// formulation also references the `active_indicator` (`u^on + Σ u^su +
/// Σ u^sd`) on the constant term, but in single-period AC reconcile the
/// commitment is fixed and the indicator collapses to a constant `0` or
/// `1` that the AC OPF can fold into the row's constant offset.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PqLinearLink {
    /// Reactive power intercept at `p = 0` in per-unit.
    pub q_at_p_zero_pu: f64,
    /// Slope `dq/dp` in per-unit (pu/pu).
    pub beta: f64,
}

/// Fuel-related parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuelParams {
    /// Fuel type (e.g. "gas", "coal", "nuclear", "wind", "solar").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel_type: Option<String>,
    /// Heat rate in BTU/MWh (flat fallback when no fuel heat_rate_curve).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heat_rate_btu_mwh: Option<f64>,
    /// Primary fuel supply. None = use cost curve directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_fuel: Option<FuelSupply>,
    /// Backup fuel (dual-fuel units). None = single fuel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_fuel: Option<FuelSupply>,
    /// Currently on backup fuel.
    #[serde(default)]
    pub on_backup_fuel: bool,
    /// Fuel switching time (minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel_switch_time_min: Option<f64>,
    /// Multi-pollutant emission rates.
    #[serde(default)]
    pub emission_rates: EmissionRates,
}

/// Market offer/qualification parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketParams {
    /// Energy offer for market clearing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_offer: Option<EnergyOffer>,
    /// Reserve offers keyed by product ID (generic reserve model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserve_offers: Vec<crate::market::reserve::ReserveOffer>,
    /// Custom qualification flags for reserve products.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub qualifications: crate::market::reserve::QualificationMap,
}

// ── Generator ─────────────────────────────────────────────────────────────

/// A generation unit connected to a bus in the transmission network.
///
/// Represents thermal, hydro, renewable, and storage resources. For storage
/// units (BESS, pumped hydro), set `storage = Some(StorageParams)` with
/// `pmin = -charge_mw_max` (negative) and `pmax = discharge_mw_max`.
///
/// All power quantities are in MW/MVAr (system base = 100 MVA for per-unit
/// conversion). Voltage setpoint is in per-unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generator {
    /// Canonical generator identifier.
    ///
    /// This is the stable, crate-native identity for the generator and is used
    /// for replaying detached solutions, scenario application, and any other
    /// workflow that must survive generator reordering.
    ///
    /// Auto-assigned by [`crate::network::Network::canonicalize_generator_ids`]
    /// for generators that lack an explicit ID (format: `gen_{bus}_{ordinal}`).
    #[serde(default = "default_empty_string")]
    pub id: String,
    /// Bus number where the generator is connected.
    pub bus: u32,
    /// PSS/E machine ID string (e.g. `"1"`, `"G1"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// Real power output in MW.
    #[serde(alias = "pg")]
    pub p: f64,
    /// Reactive power output in MVAr.
    #[serde(alias = "qg")]
    pub q: f64,
    /// Maximum reactive power in MVAr.
    pub qmax: f64,
    /// Minimum reactive power in MVAr.
    pub qmin: f64,
    /// Voltage setpoint in per-unit.
    pub voltage_setpoint_pu: f64,
    /// Whether this generator participates in voltage regulation (PV bus).
    /// When false, the generator injects P/Q but does not control bus voltage.
    /// Default true — most generators regulate voltage.
    #[serde(default = "default_true")]
    pub voltage_regulated: bool,
    /// Remote voltage regulated bus number (PSS/E IREG field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reg_bus: Option<u32>,
    /// Machine base MVA.
    pub machine_base_mva: f64,
    /// Maximum real power in MW.
    pub pmax: f64,
    /// Minimum real power in MW.
    pub pmin: f64,
    /// Generator status (true = in service).
    pub in_service: bool,
    /// Cost curve for OPF (physical cost for planning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostCurve>,
    /// Generator electrical class.
    #[serde(default)]
    pub gen_type: GenType,
    /// Generator technology / prime mover classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub technology: Option<GeneratorTechnology>,
    /// Source-native technology code when available (e.g. MATPOWER `gentype`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_technology_code: Option<String>,
    /// AGC participation factor (dimensionless).
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "apf")]
    pub agc_participation_factor: Option<f64>,
    /// Inertia constant H in seconds (MVA·s / MVA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h_inertia_s: Option<f64>,
    /// Eligible to provide primary frequency response.
    #[serde(default = "default_true")]
    pub pfr_eligible: bool,
    /// Quick-start flag: can reach full output in <=10 minutes.
    #[serde(default)]
    pub quick_start: bool,
    /// Forced outage rate [0, 1].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_outage_rate: Option<f64>,
    /// Storage parameters. Present iff this generator has energy storage
    /// capability (BESS, pumped hydro in storage mode, etc.).
    /// When Some: pmin = -charge_mw_max (negative), pmax = discharge_mw_max.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageParams>,
    /// Ownership entries (PSS/E O1,F1..O4,F4). Up to 4 co-owners.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<super::owner::OwnershipEntry>,

    // ── Optional sub-structs ──────────────────────────────────────────
    /// Unit commitment parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment: Option<CommitmentParams>,
    /// Piecewise ramp curve parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramping: Option<RampingParams>,
    /// Inverter-specific parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverter: Option<InverterParams>,
    /// Generator fault/sequence data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_data: Option<GenFaultData>,
    /// Reactive capability curve data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactive_capability: Option<ReactiveCapability>,
    /// Fuel-related parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel: Option<FuelParams>,
    /// Market offer/qualification parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<MarketParams>,
}

use crate::network::serde_defaults::{default_empty_string, default_true};

impl Default for Generator {
    fn default() -> Self {
        Self {
            id: default_empty_string(),
            bus: 0,
            machine_id: None,
            p: 0.0,
            q: 0.0,
            qmax: 9999.0,
            qmin: -9999.0,
            voltage_setpoint_pu: 1.0,
            voltage_regulated: true,
            reg_bus: None,
            machine_base_mva: 100.0,
            pmax: 9999.0,
            pmin: 0.0,
            in_service: true,
            cost: None,
            gen_type: GenType::Unknown,
            technology: None,
            source_technology_code: None,
            agc_participation_factor: None,
            h_inertia_s: None,
            pfr_eligible: true,
            quick_start: false,
            forced_outage_rate: None,
            storage: None,
            owners: Vec::new(),
            commitment: None,
            ramping: None,
            inverter: None,
            fault_data: None,
            reactive_capability: None,
            fuel: None,
            market: None,
        }
    }
}

impl Generator {
    /// Create a generator with the given bus, real power output, and voltage setpoint.
    pub fn new(bus: u32, p: f64, voltage_setpoint_pu: f64) -> Self {
        Self {
            bus,
            p,
            voltage_setpoint_pu,
            ..Default::default()
        }
    }

    /// Create a generator with an explicit canonical ID.
    pub fn with_id(id: impl Into<String>, bus: u32, p: f64, voltage_setpoint_pu: f64) -> Self {
        let mut generator = Self::new(bus, p, voltage_setpoint_pu);
        generator.id = id.into();
        generator
    }

    /// Returns true when the generator can move reactive output enough to support
    /// a voltage target.
    #[inline]
    pub fn has_reactive_power_range(&self, tolerance_mvar: f64) -> bool {
        if self.qmax.is_nan() || self.qmin.is_nan() {
            return false;
        }
        if !self.qmax.is_finite() || !self.qmin.is_finite() {
            return true;
        }
        self.qmax > self.qmin + tolerance_mvar
    }

    /// Returns true when this generator is explicitly excluded from acting as
    /// a voltage-regulating reference resource.
    #[inline]
    pub fn is_excluded_from_voltage_regulation(&self) -> bool {
        self.market
            .as_ref()
            .and_then(|market| market.qualifications.get("ac_voltage_regulation_excluded"))
            .copied()
            .unwrap_or(false)
    }

    /// Returns true when the generator should participate in AC voltage control.
    ///
    /// Q range is intentionally NOT required here: a generator with
    /// `qmin == qmax` (e.g. Q pinned to a reference value for
    /// diagnostic roundtrips or fixed-Q dispatch replays) is still a
    /// legitimate voltage reference — V is the free variable at its bus,
    /// Q just happens to be fixed rather than free. Requiring
    /// `has_reactive_power_range` here breaks `validate_for_solve`
    /// whenever the per-period dispatch profile collapses Q bounds to
    /// a point (the bus then has no regulator count, slack-placement
    /// check fails, AC-OPF preflight rejects the network).
    #[inline]
    pub fn can_voltage_regulate(&self) -> bool {
        self.in_service && !self.is_excluded_from_voltage_regulation() && self.voltage_regulated
    }

    // ── Ramp forwarding methods ───────────────────────────────────────

    /// Interpolate ramp-up rate at a given MW operating point.
    /// Below first breakpoint -> first segment's rate.
    /// Above last breakpoint -> last segment's rate.
    /// Returns None if curve is empty (unlimited ramp).
    pub fn ramp_up_at_mw(&self, p_mw: f64) -> Option<f64> {
        self.ramping.as_ref().and_then(|r| r.ramp_up_at_mw(p_mw))
    }

    /// Interpolate ramp-down rate at a given MW operating point.
    /// Falls back to ramp_up_curve if ramp_down_curve is empty.
    pub fn ramp_down_at_mw(&self, p_mw: f64) -> Option<f64> {
        self.ramping.as_ref().and_then(|r| r.ramp_down_at_mw(p_mw))
    }

    /// Interpolate regulation ramp-up rate at a given MW operating point.
    /// Falls back to ramp_up_curve if reg curve is empty.
    pub fn reg_ramp_up_at_mw(&self, p_mw: f64) -> Option<f64> {
        self.ramping
            .as_ref()
            .and_then(|r| r.reg_ramp_up_at_mw(p_mw))
    }

    /// Interpolate regulation ramp-down rate at a given MW operating point.
    /// Falls back to ramp_down_curve, then ramp_up_curve.
    pub fn reg_ramp_down_at_mw(&self, p_mw: f64) -> Option<f64> {
        self.ramping
            .as_ref()
            .and_then(|r| r.reg_ramp_down_at_mw(p_mw))
    }

    /// Weighted-average ramp-up rate over \[Pmin, Pmax\].
    /// Returns None if curve is empty (unlimited ramp).
    pub fn ramp_up_avg_mw_per_min(&self) -> Option<f64> {
        self.ramping
            .as_ref()
            .and_then(|r| r.ramp_up_avg(self.pmin, self.pmax))
    }

    /// Weighted-average ramp-down rate over \[Pmin, Pmax\].
    /// Falls back to ramp_up_curve if ramp_down_curve is empty.
    pub fn ramp_down_avg_mw_per_min(&self) -> Option<f64> {
        self.ramping
            .as_ref()
            .and_then(|r| r.ramp_down_avg(self.pmin, self.pmax))
    }

    /// Scalar ramp-up rate (MW/min) — first segment of ramp_up_curve.
    /// Equivalent to `ramp_up_at_mw(pmin)`. Returns None if curve is empty.
    #[inline]
    pub fn ramp_up_mw_per_min(&self) -> Option<f64> {
        self.ramping.as_ref().and_then(|r| r.ramp_up_mw_per_min())
    }

    /// Scalar ramp-down rate (MW/min) — first segment, falls back to ramp_up.
    #[inline]
    pub fn ramp_down_mw_per_min(&self) -> Option<f64> {
        self.ramping.as_ref().and_then(|r| r.ramp_down_mw_per_min())
    }

    /// AGC/regulation ramp rate (MW/min) — first segment, falls back to ramp_up.
    #[inline]
    pub fn ramp_agc_mw_per_min(&self) -> Option<f64> {
        self.ramping.as_ref().and_then(|r| r.ramp_agc_mw_per_min())
    }

    // ── Cross-cutting methods ─────────────────────────────────────────

    /// Returns true if this generator has energy storage capability.
    #[inline]
    pub fn is_storage(&self) -> bool {
        self.storage.is_some()
    }

    /// Maximum charge power (MW). Returns 0 for non-storage generators.
    #[inline]
    pub fn charge_mw_max(&self) -> f64 {
        if self.storage.is_some() {
            (-self.pmin).max(0.0)
        } else {
            0.0
        }
    }

    /// Maximum discharge power (MW). Alias for pmax for storage generators.
    #[inline]
    pub fn discharge_mw_max(&self) -> f64 {
        self.pmax
    }

    /// True if this generator is must-run.
    #[inline]
    pub fn is_must_run(&self) -> bool {
        self.commitment
            .as_ref()
            .is_some_and(|c| c.status == CommitmentStatus::MustRun)
    }

    /// Get the reserve offer for a specific product, if any.
    pub fn reserve_offer(&self, product_id: &str) -> Option<&crate::market::reserve::ReserveOffer> {
        self.market
            .as_ref()
            .and_then(|m| m.reserve_offers.iter().find(|o| o.product_id == product_id))
    }

    /// Maximum MW deliverable within a product's deployment window, limited by ramp rate.
    ///
    /// Selects the appropriate ramp curve (reg/normal/emergency) based on product context.
    pub fn ramp_limited_mw(&self, product: &crate::market::reserve::ReserveProduct) -> f64 {
        use crate::market::reserve::ReserveDirection;
        let deploy_min = product.deploy_secs / 60.0;
        let rate = match product.direction {
            ReserveDirection::Up => {
                // Regulation products use reg ramp curve
                if product.id.starts_with("reg") {
                    self.ramp_agc_mw_per_min()
                } else {
                    self.ramp_up_mw_per_min()
                }
            }
            ReserveDirection::Down => {
                if product.id.starts_with("reg") {
                    // Reg down — use reg ramp down, fall back to ramp down
                    self.ramping.as_ref().and_then(|r| {
                        r.reg_ramp_down_curve
                            .first()
                            .or(r.ramp_down_curve.first())
                            .or(r.ramp_up_curve.first())
                            .map(|&(_, rate)| rate)
                    })
                } else {
                    self.ramp_down_mw_per_min()
                }
            }
        };
        rate.map(|r| r * deploy_min).unwrap_or(f64::INFINITY)
    }

    /// Effective shutdown ramp capacity (MW) for one dispatch period of `dt_hours`.
    ///
    /// Uses `shutdown_ramp_mw_per_min` when present; otherwise falls back to the
    /// economic ramp-down rate. Returns `f64::MAX` when no ramp curve is defined.
    #[inline]
    pub fn shutdown_ramp_mw_per_period(&self, dt_hours: f64) -> f64 {
        let rate = self
            .commitment
            .as_ref()
            .and_then(|c| c.shutdown_ramp_mw_per_min)
            .or_else(|| self.ramp_down_mw_per_min())
            .unwrap_or(f64::MAX);
        rate * 60.0 * dt_hours
    }

    /// Effective startup ramp capacity (MW) for one dispatch period of `dt_hours`.
    ///
    /// Uses `startup_ramp_mw_per_min` when present; otherwise falls back to the
    /// economic ramp-up rate. Returns `f64::MAX` when no ramp curve is defined.
    #[inline]
    pub fn startup_ramp_mw_per_period(&self, dt_hours: f64) -> f64 {
        let rate = self
            .commitment
            .as_ref()
            .and_then(|c| c.startup_ramp_mw_per_min)
            .or_else(|| self.ramp_up_mw_per_min())
            .unwrap_or(f64::MAX);
        rate * 60.0 * dt_hours
    }
}

// ── Piecewise ramp curve helpers ──────────────────────────────────────

/// Interpolate a ramp rate at a given MW operating point on a piecewise curve.
/// The curve is `[(MW_breakpoint, MW/min_rate), ...]`, sorted by MW ascending.
/// Returns the rate of the segment containing `p_mw`:
/// - Below first breakpoint -> first rate.
/// - Above last breakpoint -> last rate.
/// - Between breakpoints -> rate of the segment whose range contains `p_mw`.
fn ramp_rate_at_mw(curve: &[(f64, f64)], p_mw: f64) -> Option<f64> {
    if curve.is_empty() {
        return None;
    }
    if curve.len() == 1 {
        return Some(curve[0].1);
    }
    // Find the segment: last breakpoint whose MW <= p_mw
    for i in (0..curve.len()).rev() {
        if p_mw >= curve[i].0 {
            return Some(curve[i].1);
        }
    }
    // p_mw is below the first breakpoint — use first rate
    Some(curve[0].1)
}

/// Weighted-average ramp rate over [lo_mw, hi_mw].
/// Each segment's rate is weighted by the MW overlap with [lo_mw, hi_mw].
fn ramp_rate_avg(curve: &[(f64, f64)], lo_mw: f64, hi_mw: f64) -> Option<f64> {
    if curve.is_empty() {
        return None;
    }
    let span = hi_mw - lo_mw;
    if span <= 0.0 {
        // Degenerate range — return rate at lo_mw
        return ramp_rate_at_mw(curve, lo_mw);
    }
    if curve.len() == 1 {
        return Some(curve[0].1);
    }

    // Build segment ranges: segment i covers [curve[i].0, curve[i+1].0),
    // last segment extends to infinity.
    let mut weighted_sum = 0.0;
    for i in 0..curve.len() {
        let seg_lo = curve[i].0;
        let seg_hi = if i + 1 < curve.len() {
            curve[i + 1].0
        } else {
            f64::MAX
        };
        let rate = curve[i].1;

        // Overlap of [seg_lo, seg_hi) with [lo_mw, hi_mw]
        let overlap_lo = seg_lo.max(lo_mw);
        let overlap_hi = seg_hi.min(hi_mw);
        if overlap_hi > overlap_lo {
            weighted_sum += rate * (overlap_hi - overlap_lo);
        }
    }
    Some(weighted_sum / span)
}

/// Enforce monotonically non-decreasing rates on a ramp curve.
/// Segments with rate < previous are flattened to the previous rate.
pub fn enforce_monotonic_ramp(curve: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut result: Vec<(f64, f64)> = curve.to_vec();
    for i in 1..result.len() {
        if result[i].1 < result[i - 1].1 {
            tracing::warn!(
                segment = i,
                rate = result[i].1,
                previous = result[i - 1].1,
                "Ramp curve segment: rate < previous, flattened"
            );
            result[i].1 = result[i - 1].1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen_with_ramp(pmin: f64, pmax: f64, up: Vec<(f64, f64)>, dn: Vec<(f64, f64)>) -> Generator {
        Generator {
            pmin,
            pmax,
            ramping: Some(RampingParams {
                ramp_up_curve: up,
                ramp_down_curve: dn,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // ── ramp_rate_at_mw ──

    #[test]
    fn test_ramp_at_mw_empty_curve() {
        assert_eq!(ramp_rate_at_mw(&[], 100.0), None);
    }

    #[test]
    fn test_ramp_at_mw_single_segment() {
        let curve = vec![(0.0, 10.0)];
        assert_eq!(ramp_rate_at_mw(&curve, 0.0), Some(10.0));
        assert_eq!(ramp_rate_at_mw(&curve, 500.0), Some(10.0));
    }

    #[test]
    fn test_ramp_at_mw_multi_segment() {
        // Rate changes at MW breakpoints: 8 below 350, 12 from 350+
        let curve = vec![(200.0, 8.0), (350.0, 12.0), (500.0, 10.0)];
        assert_eq!(ramp_rate_at_mw(&curve, 100.0), Some(8.0)); // below first bp
        assert_eq!(ramp_rate_at_mw(&curve, 200.0), Some(8.0)); // at first bp
        assert_eq!(ramp_rate_at_mw(&curve, 300.0), Some(8.0)); // in first segment
        assert_eq!(ramp_rate_at_mw(&curve, 350.0), Some(12.0)); // at second bp
        assert_eq!(ramp_rate_at_mw(&curve, 400.0), Some(12.0)); // in second segment
        assert_eq!(ramp_rate_at_mw(&curve, 500.0), Some(10.0)); // at third bp
        assert_eq!(ramp_rate_at_mw(&curve, 600.0), Some(10.0)); // above last bp
    }

    // ── ramp_rate_avg ──

    #[test]
    fn test_ramp_avg_empty_curve() {
        assert_eq!(ramp_rate_avg(&[], 100.0, 500.0), None);
    }

    #[test]
    fn test_ramp_avg_single_segment() {
        let curve = vec![(0.0, 10.0)];
        assert_eq!(ramp_rate_avg(&curve, 100.0, 500.0), Some(10.0));
    }

    #[test]
    fn test_ramp_avg_multi_segment() {
        // 200-350: rate=8 (150 MW), 350-500: rate=12 (150 MW)
        let curve = vec![(200.0, 8.0), (350.0, 12.0)];
        let avg = ramp_rate_avg(&curve, 200.0, 500.0).unwrap();
        // (8 * 150 + 12 * 150) / 300 = (1200 + 1800) / 300 = 10.0
        assert!((avg - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_ramp_avg_partial_overlap() {
        // Curve starts at 100, but we integrate over [200, 400]
        // Segment 100-300: rate=5. Segment 300-500: rate=15.
        let curve = vec![(100.0, 5.0), (300.0, 15.0)];
        let avg = ramp_rate_avg(&curve, 200.0, 400.0).unwrap();
        // Overlap: [200,300] -> 100 MW at rate=5, [300,400] -> 100 MW at rate=15
        // avg = (5*100 + 15*100) / 200 = 10.0
        assert!((avg - 10.0).abs() < 1e-10);
    }

    // ── Generator methods ──

    #[test]
    fn test_gen_ramp_up_avg() {
        let g = gen_with_ramp(200.0, 500.0, vec![(200.0, 8.0), (350.0, 12.0)], vec![]);
        let avg = g.ramp_up_avg_mw_per_min().unwrap();
        assert!((avg - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_gen_ramp_dn_fallback_to_up() {
        let g = gen_with_ramp(200.0, 500.0, vec![(0.0, 10.0)], vec![]);
        assert_eq!(g.ramp_down_at_mw(300.0), Some(10.0));
        assert_eq!(g.ramp_down_avg_mw_per_min(), Some(10.0));
    }

    #[test]
    fn test_gen_ramp_up_at_mw() {
        let g = gen_with_ramp(200.0, 500.0, vec![(200.0, 8.0), (400.0, 12.0)], vec![]);
        assert_eq!(g.ramp_up_at_mw(300.0), Some(8.0));
        assert_eq!(g.ramp_up_at_mw(450.0), Some(12.0));
    }

    #[test]
    fn test_gen_single_segment_consistent() {
        // Single-segment: all methods should agree
        let g = gen_with_ramp(0.0, 100.0, vec![(0.0, 5.0)], vec![]);
        assert_eq!(g.ramp_up_mw_per_min(), Some(5.0));
        assert_eq!(g.ramp_up_at_mw(50.0), Some(5.0));
        assert_eq!(g.ramp_up_avg_mw_per_min(), Some(5.0));
    }

    #[test]
    #[ignore = "encoded spec behavior drifted from implementation; revisit voltage-regulation eligibility rules"]
    fn test_generator_can_voltage_regulate_requires_reactive_range() {
        let mut generator = Generator {
            qmin: 0.0,
            qmax: 0.0,
            ..Generator::default()
        };
        assert!(!generator.has_reactive_power_range(1e-9));
        assert!(!generator.can_voltage_regulate());

        generator.qmax = 10.0;
        assert!(generator.has_reactive_power_range(1e-9));
        assert!(generator.can_voltage_regulate());
    }

    #[test]
    fn test_generator_can_voltage_regulate_accepts_unbounded_reactive_range() {
        let generator = Generator {
            qmin: f64::NEG_INFINITY,
            qmax: f64::INFINITY,
            ..Generator::default()
        };
        assert!(generator.has_reactive_power_range(1e-9));
        assert!(generator.can_voltage_regulate());
    }

    #[test]
    fn test_generator_can_voltage_regulate_respects_exclusion_qualification() {
        let mut generator = Generator {
            qmin: -10.0,
            qmax: 10.0,
            market: Some(MarketParams::default()),
            ..Generator::default()
        };
        generator
            .market
            .as_mut()
            .expect("market params")
            .qualifications
            .insert("ac_voltage_regulation_excluded".to_string(), true);
        assert!(generator.has_reactive_power_range(1e-9));
        assert!(!generator.can_voltage_regulate());
        assert!(generator.is_excluded_from_voltage_regulation());
    }

    // ── enforce_monotonic_ramp ──

    #[test]
    fn test_enforce_monotonic_already_monotonic() {
        let curve = vec![(200.0, 8.0), (400.0, 12.0), (500.0, 15.0)];
        let result = enforce_monotonic_ramp(&curve);
        assert_eq!(result, curve);
    }

    #[test]
    fn test_enforce_monotonic_flattens_decrease() {
        let curve = vec![(200.0, 8.0), (400.0, 12.0), (500.0, 6.0)];
        let result = enforce_monotonic_ramp(&curve);
        assert_eq!(result, vec![(200.0, 8.0), (400.0, 12.0), (500.0, 12.0)]);
    }

    #[test]
    fn test_enforce_monotonic_empty() {
        let result = enforce_monotonic_ramp(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_enforce_monotonic_cascading() {
        // All decrease: should flatten everything to first rate
        let curve = vec![(0.0, 10.0), (100.0, 8.0), (200.0, 5.0)];
        let result = enforce_monotonic_ramp(&curve);
        assert_eq!(result, vec![(0.0, 10.0), (100.0, 10.0), (200.0, 10.0)]);
    }

    #[test]
    fn test_shutdown_ramp_mw_per_period_explicit() {
        let g = Generator {
            pmax: 300.0,
            commitment: Some(CommitmentParams {
                shutdown_ramp_mw_per_min: Some(2.0),
                ..Default::default()
            }),
            ramping: Some(RampingParams {
                ramp_down_curve: vec![(0.0, 5.0)],
                ..Default::default()
            }),
            ..Default::default()
        };
        // Uses explicit shutdown ramp: 2.0 * 60 * 1.0 = 120
        assert!((g.shutdown_ramp_mw_per_period(1.0) - 120.0).abs() < 1e-10);
    }

    #[test]
    fn test_shutdown_ramp_mw_per_period_fallback() {
        let g = Generator {
            pmax: 300.0,
            ramping: Some(RampingParams {
                ramp_down_curve: vec![(0.0, 5.0)],
                ..Default::default()
            }),
            ..Default::default()
        };
        // Falls back to ramp_down: 5.0 * 60 * 1.0 = 300
        assert!((g.shutdown_ramp_mw_per_period(1.0) - 300.0).abs() < 1e-10);
    }

    #[test]
    fn test_shutdown_ramp_mw_per_period_no_curve() {
        let g = Generator::default();
        // No ramp curve -> effectively unlimited (>= f64::MAX)
        assert!(g.shutdown_ramp_mw_per_period(1.0) >= f64::MAX);
    }

    #[test]
    fn test_startup_ramp_mw_per_period_explicit() {
        let g = Generator {
            pmax: 300.0,
            commitment: Some(CommitmentParams {
                startup_ramp_mw_per_min: Some(1.5),
                ..Default::default()
            }),
            ramping: Some(RampingParams {
                ramp_up_curve: vec![(0.0, 5.0)],
                ..Default::default()
            }),
            ..Default::default()
        };
        // Uses explicit startup ramp: 1.5 * 60 * 1.0 = 90
        assert!((g.startup_ramp_mw_per_period(1.0) - 90.0).abs() < 1e-10);
    }

    #[test]
    fn test_startup_ramp_mw_per_period_fallback() {
        let g = Generator {
            pmax: 300.0,
            ramping: Some(RampingParams {
                ramp_up_curve: vec![(0.0, 5.0)],
                ..Default::default()
            }),
            ..Default::default()
        };
        // Falls back to ramp_up: 5.0 * 60 * 1.0 = 300
        assert!((g.startup_ramp_mw_per_period(1.0) - 300.0).abs() < 1e-10);
    }

    #[test]
    fn test_startup_ramp_mw_per_period_half_hour() {
        let g = Generator {
            commitment: Some(CommitmentParams {
                startup_ramp_mw_per_min: Some(3.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        // 3.0 * 60 * 0.5 = 90
        assert!((g.startup_ramp_mw_per_period(0.5) - 90.0).abs() < 1e-10);
    }
}
