// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CIM-aligned measurement types for CGMES Measurement profile integration.
//!
//! These types store CIM mRID linkage and can be resolved to internal 0-based
//! bus/branch indices at runtime by downstream crates (e.g., surge-se).

use serde::{Deserialize, Serialize};

/// CIM measurement type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CimMeasurementType {
    /// Active power (MW) — injection or flow.
    #[default]
    ActivePower,
    /// Reactive power (MVAr) — injection or flow.
    ReactivePower,
    /// Voltage magnitude (kV or pu).
    VoltageMagnitude,
    /// Voltage angle (degrees or radians).
    VoltageAngle,
    /// Current magnitude (A or pu).
    CurrentMagnitude,
    /// Frequency (Hz).
    Frequency,
    /// Tap position (discrete integer).
    TapPosition,
    /// Switch/breaker status (open/closed).
    SwitchStatus,
    /// Energy accumulator (MWh).
    EnergyAccumulator,
    /// PMU voltage phasor real part.
    PmuVoltageReal,
    /// PMU voltage phasor imaginary part.
    PmuVoltageImaginary,
    /// PMU current phasor real part.
    PmuCurrentReal,
    /// PMU current phasor imaginary part.
    PmuCurrentImaginary,
}

/// Measurement value source type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MeasurementSource {
    #[default]
    Scada,
    Pmu,
    Manual,
    Calculated,
    Other,
}

/// Quality of a measurement value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MeasurementQuality {
    #[default]
    Good,
    Suspect,
    Bad,
    Missing,
}

/// A CIM-aligned measurement definition from the CGMES Measurement profile.
///
/// Links to network elements via bus number and optional branch circuit ID,
/// allowing resolution to internal 0-based indices at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CimMeasurement {
    /// CIM mRID of this measurement.
    pub mrid: String,
    /// Human-readable name.
    pub name: String,
    /// What physical quantity this measures.
    #[serde(alias = "meas_type")]
    pub measurement_type: CimMeasurementType,
    /// Bus number where this measurement is located.
    pub bus: u32,
    /// For flow/current measurements: the branch circuit identifier.
    pub branch_circuit: Option<String>,
    /// For flow measurements: which end of the branch (true = from-end).
    pub from_end: bool,
    /// Measured value (in engineering units: MW, MVAr, kV, A, Hz, etc.).
    pub value: f64,
    /// Standard deviation of measurement noise (same units as value).
    pub sigma: f64,
    /// Whether this measurement is enabled/active.
    pub enabled: bool,
    /// Data source.
    pub source: MeasurementSource,
    /// Data quality.
    pub quality: MeasurementQuality,
    /// CIM Terminal mRID (for traceability).
    pub terminal_mrid: Option<String>,
}

impl Default for CimMeasurement {
    fn default() -> Self {
        Self {
            mrid: String::new(),
            name: String::new(),
            measurement_type: CimMeasurementType::default(),
            bus: 0,
            branch_circuit: None,
            from_end: true,
            value: 0.0,
            sigma: 0.02,
            enabled: true,
            source: MeasurementSource::default(),
            quality: MeasurementQuality::default(),
            terminal_mrid: None,
        }
    }
}
