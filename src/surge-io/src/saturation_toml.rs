// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! TOML parser for transformer saturation data.
//!
//! Reads saturation curves, core loss models, and converter commutation models
//! from a `.toml` file and attaches them to the corresponding branches in the
//! network by matching `(bus_from, bus_to, circuit)` keys.
//!
//! # Example TOML
//!
//! ```toml
//! [[transformer]]
//! from_bus = 1
//! to_bus = 2
//! circuit = "1"
//!
//! # Piecewise-linear saturation curve (Phi-I_m pairs).
//! [[transformer.saturation_points]]
//! phi_pu = 0.0
//! i_m_pu = 0.0
//!
//! [[transformer.saturation_points]]
//! phi_pu = 1.0
//! i_m_pu = 0.01
//!
//! [[transformer.saturation_points]]
//! phi_pu = 1.2
//! i_m_pu = 0.10
//!
//! [[transformer.saturation_points]]
//! phi_pu = 1.4
//! i_m_pu = 1.00
//!
//! # Optional: core type for GIC K-factor
//! core_type = "5-limb"
//!
//! # Optional: frequency-dependent core loss model
//! [transformer.core_loss]
//! f_eddy = 0.5
//! f_hyst = 0.5
//! f_excess = 0.0
//!
//! # Converter commutation models
//! [[converter]]
//! bus = 3
//! pulse_number = 6
//! firing_angle_deg = 15.0
//! x_commutation_pu = 0.15
//! i_dc_pu = 1.0
//! transformer_ratio = 1.0
//! rated_mva = 100.0
//! ```

use serde::Deserialize;
use surge_network::Network;
use surge_network::dynamics::{
    ConverterCommutationModel, CoreLossModel, CoreType, SaturationCurve, SaturationPoint,
    TransformerSaturation,
};
use surge_network::network::HarmonicData;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SaturationError {
    #[error("failed to parse saturation TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("I/O error reading '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },

    #[error("validation error for transformer ({from_bus}, {to_bus}, '{circuit}'): {message}")]
    Validation {
        from_bus: u32,
        to_bus: u32,
        circuit: String,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Serde-friendly TOML schema
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SaturationFile {
    #[serde(default)]
    transformer: Vec<TransformerEntry>,
    #[serde(default)]
    converter: Vec<ConverterEntry>,
}

#[derive(Deserialize)]
struct TransformerEntry {
    from_bus: u32,
    to_bus: u32,
    #[serde(default = "default_circuit")]
    circuit: String,
    #[serde(default)]
    saturation_points: Vec<SatPointEntry>,
    #[serde(default)]
    core_type: Option<String>,
    #[serde(default)]
    core_loss: Option<CoreLossEntry>,
}

fn default_circuit() -> String {
    "1".to_string()
}

#[derive(Deserialize)]
struct SatPointEntry {
    phi_pu: f64,
    i_m_pu: f64,
}

#[derive(Deserialize)]
struct CoreLossEntry {
    f_eddy: f64,
    f_hyst: f64,
    #[serde(default)]
    f_excess: f64,
}

#[derive(Deserialize)]
struct ConverterEntry {
    bus: u32,
    #[serde(default = "default_pulse")]
    pulse_number: u32,
    #[serde(default = "default_firing_angle")]
    firing_angle_deg: f64,
    x_commutation_pu: f64,
    i_dc_pu: f64,
    #[serde(default = "default_ratio")]
    transformer_ratio: f64,
    rated_mva: f64,
}

fn default_pulse() -> u32 {
    6
}
fn default_firing_angle() -> f64 {
    15.0
}
fn default_ratio() -> f64 {
    1.0
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Result of parsing a saturation TOML file.
pub struct SaturationData {
    /// Number of transformers matched and updated.
    pub transformers_matched: usize,
    /// Number of transformers in the file that could not be matched.
    pub transformers_unmatched: usize,
    /// Converter commutation models parsed from the file.
    pub converters: Vec<ConverterCommutationModel>,
}

/// Parse a TOML string and attach saturation data to the network.
///
/// Matches transformers by `(from_bus, to_bus, circuit)` and sets the
/// `saturation`, `core_type`, and `core_loss_model` fields on the branch.
///
/// Returns converter models separately (not attached to branches).
pub fn apply_saturation_toml(
    network: &mut Network,
    toml_str: &str,
) -> Result<SaturationData, SaturationError> {
    let file: SaturationFile = toml::from_str(toml_str)?;

    let mut matched = 0;
    let mut unmatched = 0;

    for entry in &file.transformer {
        // Find matching branch.
        let branch_idx = network.branches.iter().position(|br| {
            br.from_bus == entry.from_bus
                && br.to_bus == entry.to_bus
                && br.circuit == entry.circuit
        });

        let Some(idx) = branch_idx else {
            unmatched += 1;
            tracing::warn!(
                "saturation TOML: no branch found for ({}, {}, '{}')",
                entry.from_bus,
                entry.to_bus,
                entry.circuit
            );
            continue;
        };

        let branch = &mut network.branches[idx];

        // Set saturation curve.
        if !entry.saturation_points.is_empty() {
            let points: Vec<SaturationPoint> = entry
                .saturation_points
                .iter()
                .map(|p| SaturationPoint {
                    phi_pu: p.phi_pu,
                    i_m_pu: p.i_m_pu,
                })
                .collect();

            let curve = SaturationCurve { points };
            if let Err(e) = curve.validate() {
                return Err(SaturationError::Validation {
                    from_bus: entry.from_bus,
                    to_bus: entry.to_bus,
                    circuit: entry.circuit.clone(),
                    message: e.to_string(),
                });
            }
            branch
                .harmonic
                .get_or_insert_with(HarmonicData::default)
                .saturation = Some(TransformerSaturation::PiecewiseLinear(curve));
        }

        // Set core type.
        if let Some(ref ct_str) = entry.core_type {
            let ct: CoreType = ct_str
                .parse()
                .map_err(|e: String| SaturationError::Validation {
                    from_bus: entry.from_bus,
                    to_bus: entry.to_bus,
                    circuit: entry.circuit.clone(),
                    message: e,
                })?;
            branch
                .harmonic
                .get_or_insert_with(HarmonicData::default)
                .core_type = Some(ct);
        }

        // Set core loss model.
        if let Some(ref cl) = entry.core_loss {
            let model = CoreLossModel {
                f_eddy: cl.f_eddy,
                f_hyst: cl.f_hyst,
                f_excess: cl.f_excess,
            };
            if let Err(e) = model.validate() {
                return Err(SaturationError::Validation {
                    from_bus: entry.from_bus,
                    to_bus: entry.to_bus,
                    circuit: entry.circuit.clone(),
                    message: e.to_string(),
                });
            }
            branch
                .harmonic
                .get_or_insert_with(HarmonicData::default)
                .core_loss_model = Some(model);
        }

        matched += 1;
    }

    // Parse converters.
    let converters: Vec<ConverterCommutationModel> = file
        .converter
        .iter()
        .map(|c| ConverterCommutationModel {
            bus: c.bus,
            pulse_number: c.pulse_number,
            firing_angle_deg: c.firing_angle_deg,
            x_commutation_pu: c.x_commutation_pu,
            i_dc_pu: c.i_dc_pu,
            transformer_ratio: c.transformer_ratio,
            rated_mva: c.rated_mva,
        })
        .collect();

    Ok(SaturationData {
        transformers_matched: matched,
        transformers_unmatched: unmatched,
        converters,
    })
}

/// Parse a TOML file and attach saturation data to the network.
pub fn apply_saturation_toml_file(
    network: &mut Network,
    path: &std::path::Path,
) -> Result<SaturationData, SaturationError> {
    let content = std::fs::read_to_string(path).map_err(|e| SaturationError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    apply_saturation_toml(network, &content)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::{Branch, Bus, BusType};

    fn test_network() -> Network {
        let mut net = Network::new("test");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 345.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 13.8));

        let mut br = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        br.tap = 345.0 / 138.0;
        br.circuit = "1".to_string();
        net.branches.push(br);

        let br2 = Branch::new_line(2, 3, 0.02, 0.2, 0.01);
        net.branches.push(br2);

        net
    }

    #[test]
    fn test_parse_saturation_curve() {
        let toml = r#"
[[transformer]]
from_bus = 1
to_bus = 2
circuit = "1"
core_type = "5-limb"
saturation_points = [
    { phi_pu = 0.0, i_m_pu = 0.0 },
    { phi_pu = 1.0, i_m_pu = 0.01 },
    { phi_pu = 1.2, i_m_pu = 0.10 },
    { phi_pu = 1.4, i_m_pu = 1.00 },
]

[transformer.core_loss]
f_eddy = 0.5
f_hyst = 0.5
f_excess = 0.0
"#;

        let mut net = test_network();
        let result = apply_saturation_toml(&mut net, toml).unwrap();

        assert_eq!(result.transformers_matched, 1);
        assert_eq!(result.transformers_unmatched, 0);
        assert!(
            net.branches[0]
                .harmonic
                .as_ref()
                .unwrap()
                .saturation
                .is_some()
        );
        assert_eq!(
            net.branches[0]
                .harmonic
                .as_ref()
                .unwrap()
                .core_type
                .as_ref()
                .unwrap()
                .k_factor(),
            1.18
        );
        assert!(
            net.branches[0]
                .harmonic
                .as_ref()
                .unwrap()
                .core_loss_model
                .is_some()
        );
    }

    #[test]
    fn test_parse_converter() {
        let toml = r#"
[[converter]]
bus = 3
pulse_number = 12
firing_angle_deg = 15.0
x_commutation_pu = 0.15
i_dc_pu = 1.0
rated_mva = 50.0
"#;

        let mut net = test_network();
        let result = apply_saturation_toml(&mut net, toml).unwrap();

        assert_eq!(result.converters.len(), 1);
        assert_eq!(result.converters[0].pulse_number, 12);
        assert_eq!(result.converters[0].bus, 3);
    }

    #[test]
    fn test_unmatched_transformer() {
        let toml = r#"
[[transformer]]
from_bus = 99
to_bus = 100
circuit = "1"

[[transformer.saturation_points]]
phi_pu = 0.0
i_m_pu = 0.0

[[transformer.saturation_points]]
phi_pu = 1.0
i_m_pu = 0.01
"#;

        let mut net = test_network();
        let result = apply_saturation_toml(&mut net, toml).unwrap();
        assert_eq!(result.transformers_matched, 0);
        assert_eq!(result.transformers_unmatched, 1);
    }

    #[test]
    fn test_invalid_core_loss_sum() {
        let toml = r#"
[[transformer]]
from_bus = 1
to_bus = 2
circuit = "1"

[[transformer.saturation_points]]
phi_pu = 0.0
i_m_pu = 0.0

[[transformer.saturation_points]]
phi_pu = 1.0
i_m_pu = 0.01

[transformer.core_loss]
f_eddy = 0.3
f_hyst = 0.3
f_excess = 0.1
"#;

        let mut net = test_network();
        let result = apply_saturation_toml(&mut net, toml);
        assert!(
            result.is_err(),
            "core loss fractions summing to 0.7 should fail validation"
        );
    }

    #[test]
    fn test_nonmonotonic_curve_rejected() {
        let toml = r#"
[[transformer]]
from_bus = 1
to_bus = 2
circuit = "1"

[[transformer.saturation_points]]
phi_pu = 0.0
i_m_pu = 0.0

[[transformer.saturation_points]]
phi_pu = 1.0
i_m_pu = 0.10

[[transformer.saturation_points]]
phi_pu = 0.5
i_m_pu = 0.05
"#;

        let mut net = test_network();
        let result = apply_saturation_toml(&mut net, toml);
        assert!(
            result.is_err(),
            "non-monotonic curve should fail validation"
        );
    }
}
