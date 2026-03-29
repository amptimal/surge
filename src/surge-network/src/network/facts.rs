// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! FACTS (Flexible AC Transmission System) device data structures.
//!
//! Covers SVC, STATCOM (shunt-only), TCSC (series-only), and UPFC
//! (series + shunt) devices. PSS/E RAW section: "FACTS DEVICE DATA".

use serde::{Deserialize, Serialize};

/// Operating mode of a FACTS device (PSS/E MODE field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FactsMode {
    /// Device is out of service.
    #[default]
    OutOfService = 0,
    /// Series element only (TCSC in impedance/power mode).
    SeriesOnly = 1,
    /// Shunt element only (SVC or STATCOM at bus_from).
    ShuntOnly = 2,
    /// Shunt and series combined (UPFC).
    ShuntSeries = 3,
    /// Series element with active power control.
    SeriesPowerControl = 4,
    /// Direct impedance modulation.
    ImpedanceModulation = 5,
}

impl FactsMode {
    /// Convert a PSS/E MODE integer to a `FactsMode`. Out-of-range values map to `OutOfService`.
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::SeriesOnly,
            2 => Self::ShuntOnly,
            3 => Self::ShuntSeries,
            4 => Self::SeriesPowerControl,
            5 => Self::ImpedanceModulation,
            _ => Self::OutOfService,
        }
    }

    /// Returns `true` if this mode includes a shunt element at `bus_from`.
    pub fn has_shunt(&self) -> bool {
        matches!(
            self,
            Self::ShuntOnly | Self::ShuntSeries | Self::SeriesPowerControl
        )
    }

    /// Returns `true` if this mode includes a series element between `bus_from` and `bus_to`.
    pub fn has_series(&self) -> bool {
        matches!(
            self,
            Self::SeriesOnly
                | Self::ShuntSeries
                | Self::SeriesPowerControl
                | Self::ImpedanceModulation
        )
    }

    /// Returns `true` if the device is in service.
    pub fn in_service(&self) -> bool {
        !matches!(self, Self::OutOfService)
    }
}

/// FACTS device type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FactsType {
    /// Static VAR Compensator (thyristor-switched capacitor/reactor).
    #[default]
    Svc,
    /// Static Synchronous Compensator (VSC-based shunt).
    Statcom,
    /// Thyristor-Controlled Series Capacitor.
    Tcsc,
    /// Static Synchronous Series Compensator.
    Sssc,
    /// Unified Power Flow Controller (shunt + series VSC).
    Upfc,
    /// Unclassified FACTS device.
    Other,
}

/// A FACTS control device (SVC, STATCOM, TCSC, UPFC).
///
/// The device is electrically represented differently depending on `mode`:
///
/// | Mode | Device | AC effect |
/// |------|--------|-----------|
/// | 0 | Out of service | No effect |
/// | 1 | TCSC (series) | Subtract `linx` from the branch reactance between `bus_from` and `bus_to` |
/// | 2 | SVC / STATCOM | Add a PV generator at `bus_from` with Q range `[−q_max, q_max]` |
/// | 3 | UPFC | Both series reactance mod and shunt reactive compensation |
/// | 4 | Series power control | Same as mode 1 with active power targeting (soft constraint) |
/// | 5 | Impedance modulation | Direct `branch.x` modification by `linx` |
///
/// The `expand_facts` function in `surge-ac` converts these records into
/// concrete Generator additions and Branch modifications before the NR solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactsDevice {
    /// Device name (NAME).
    pub name: String,
    /// Shunt connection bus number (I). SVCs/STATCOMs connect here.
    #[serde(alias = "bus_i")]
    pub bus_from: u32,
    /// Remote/series bus number (J). Zero for shunt-only devices.
    #[serde(alias = "bus_j")]
    pub bus_to: u32,
    /// Operating mode.
    pub mode: FactsMode,
    /// Desired active power flow through series element in MW (PDES).
    pub p_setpoint_mw: f64,
    /// Desired reactive power from shunt element in MVAr (QDES).
    pub q_setpoint_mvar: f64,
    /// Voltage setpoint in per-unit at `bus_from` (VSET). Used when `mode` includes a shunt.
    pub voltage_setpoint_pu: f64,
    /// Maximum shunt reactive injection in MVAr (SHMX). Minimum is `−q_max`.
    pub q_max: f64,
    /// Series reactance in per-unit on system base (LINX). Applied to the branch between
    /// `bus_from` and `bus_to`. Negative values reduce impedance (TCSC boost).
    pub series_reactance_pu: f64,
    /// Device is in service (derived from `mode ≠ 0`).
    pub in_service: bool,

    // --- expanded fields ---
    /// FACTS device type classification.
    #[serde(default)]
    pub facts_type: FactsType,
    /// Rated apparent power (MVA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s_rated_mva: Option<f64>,
    /// Minimum reactive power (MVAr). Allows asymmetric range.
    #[serde(default)]
    pub q_min: f64,
    /// Voltage droop (pu V / pu Q). 0 = flat (STATCOM).
    #[serde(default)]
    pub v_droop: f64,
    /// Max current (pu on s_rated). STATCOM low-V behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub i_max_pu: Option<f64>,
    /// Short-term overload (e.g. 1.2 = 120%).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overload_pct: Option<f64>,
    /// Overload duration (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overload_duration_s: Option<f64>,
    /// No-load loss (MW).
    #[serde(default)]
    pub loss_a_mw: f64,
    /// Proportional loss coefficient (per-unit).
    #[serde(default, alias = "loss_b")]
    pub loss_b_pu: f64,
    /// SVC: number of TSC banks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tsc_steps: Option<u32>,
    /// SVC: MVAr per TSC step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tsc_mvar_per_step: Option<f64>,
    /// TCSC/SSSC: min series reactance (pu, capacitive limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_min: Option<f64>,
    /// TCSC/SSSC: max series reactance (pu, inductive limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_max: Option<f64>,
}

impl Default for FactsDevice {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus_from: 0,
            bus_to: 0,
            mode: FactsMode::OutOfService,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.0,
            q_max: 9999.0,
            series_reactance_pu: 0.0,
            in_service: false,
            facts_type: FactsType::Svc,
            s_rated_mva: None,
            q_min: 0.0,
            v_droop: 0.0,
            i_max_pu: None,
            overload_pct: None,
            overload_duration_s: None,
            loss_a_mw: 0.0,
            loss_b_pu: 0.0,
            tsc_steps: None,
            tsc_mvar_per_step: None,
            x_min: None,
            x_max: None,
        }
    }
}
