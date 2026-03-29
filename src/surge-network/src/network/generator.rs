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
    /// (MW, $/hr) PWL breakpoints for selling power; `charge_bid` defines the
    /// (MW, $/hr) PWL breakpoints for buying power.  The optimizer clears the BESS
    /// against these curves at the endogenous bus LMP.
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
pub struct StorageParams {
    /// Round-trip efficiency (0 < eta <= 1). Applied symmetrically: sqrt(eta)
    /// per direction.
    pub efficiency: f64,
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
    /// Discharge offer curve: (MW, $/hr) breakpoints. OfferCurve mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discharge_offer: Option<Vec<(f64, f64)>>,
    /// Charge bid curve: (MW, $/hr) breakpoints. OfferCurve mode only.
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
}

/// Validation error for [`StorageParams`].
#[derive(Debug, Clone, Error, PartialEq)]
pub enum StorageValidationError {
    #[error("efficiency must be in (0, 1], got {0}")]
    InvalidEfficiency(f64),
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
            efficiency: 0.90,
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
        }
    }

    /// Validate storage parameters.
    pub fn validate(&self) -> Result<(), StorageValidationError> {
        if self.efficiency <= 0.0 || self.efficiency > 1.0 {
            return Err(StorageValidationError::InvalidEfficiency(self.efficiency));
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
        Ok(())
    }
}

/// Generator type classification.
///
/// Determines whether the machine is modeled as a synchronous generator
/// (with inertia and rotating mass) or an inverter-based resource (IBR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GenType {
    /// Conventional synchronous machine (steam, hydro, gas turbine, nuclear).
    #[default]
    Synchronous,
    /// Wind turbine generator (Type 3/4 IBR).
    Wind,
    /// Solar photovoltaic inverter.
    Solar,
    /// Other inverter-based resource (e.g. fuel cell, flywheel).
    InverterOther,
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
    /// Generator type classification.
    #[serde(default)]
    pub gen_type: GenType,
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
            gen_type: GenType::Synchronous,
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
