// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! TOML parser for multi-mass torsional shaft models.
//!
//! Parses a `.toml` file containing one or more `[[shaft]]` entries, each with
//! segments, couplings, and torque source assignments. Returns a list of
//! [`ShaftDyn`] records ready to append to a `DynamicModel`.
//!
//! # Example TOML
//!
//! ```toml
//! [[shaft]]
//! bus = 1
//! machine_id = "1"
//!
//! [[shaft.segments]]
//! name = "HP"
//! h_pu = 0.092595
//! d_self_pu = 0.0
//!
//! [[shaft.segments]]
//! name = "GEN"
//! h_pu = 0.868495
//! d_self_pu = 0.0
//!
//! [[shaft.couplings]]
//! k_pu = 19.652
//! d_mutual_pu = 0.0
//!
//! [[shaft.torque_sources]]
//! segment = "HP"
//! source = "fraction"
//! value = 0.30
//!
//! [[shaft.torque_sources]]
//! segment = "GEN"
//! source = "electrical"
//! ```

use serde::Deserialize;
use surge_network::dynamics::ShaftDyn;
use surge_network::dynamics::{SegmentTorqueSource, ShaftCoupling, ShaftModel, ShaftSegment};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShaftError {
    #[error("failed to parse shaft TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("I/O error reading '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },

    #[error("shaft[{index}] (bus={bus}, id={machine_id}): {message}")]
    Validation {
        index: usize,
        bus: u32,
        machine_id: String,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Serde-friendly TOML schema
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ShaftFile {
    shaft: Vec<ShaftEntry>,
}

#[derive(Deserialize)]
struct ShaftEntry {
    bus: u32,
    machine_id: String,
    segments: Vec<SegmentEntry>,
    #[serde(default)]
    couplings: Vec<CouplingEntry>,
    #[serde(default)]
    torque_sources: Vec<TorqueSourceEntry>,
}

#[derive(Deserialize)]
struct SegmentEntry {
    name: String,
    h_pu: f64,
    #[serde(default)]
    d_self_pu: f64,
}

#[derive(Deserialize)]
struct CouplingEntry {
    k_pu: f64,
    #[serde(default)]
    d_mutual_pu: f64,
}

#[derive(Deserialize)]
struct TorqueSourceEntry {
    segment: String,
    source: String,
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    stage: Option<usize>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a TOML string containing shaft model definitions.
///
/// Returns a vector of [`ShaftDyn`] records (one per `[[shaft]]` entry).
pub fn parse_shaft_toml(toml_str: &str) -> Result<Vec<ShaftDyn>, ShaftError> {
    let file: ShaftFile = toml::from_str(toml_str)?;

    let mut result = Vec::with_capacity(file.shaft.len());

    for (idx, entry) in file.shaft.into_iter().enumerate() {
        let n_seg = entry.segments.len();
        if n_seg == 0 {
            return Err(ShaftError::Validation {
                index: idx,
                bus: entry.bus,
                machine_id: entry.machine_id,
                message: "no segments".to_string(),
            });
        }
        if entry.couplings.len() != n_seg - 1 {
            return Err(ShaftError::Validation {
                index: idx,
                bus: entry.bus,
                machine_id: entry.machine_id,
                message: format!(
                    "expected {} couplings for {} segments, got {}",
                    n_seg - 1,
                    n_seg,
                    entry.couplings.len()
                ),
            });
        }

        let segments: Vec<ShaftSegment> = entry
            .segments
            .iter()
            .map(|s| ShaftSegment {
                name: s.name.clone(),
                h_pu: s.h_pu,
                d_self_pu: s.d_self_pu,
            })
            .collect();

        let couplings: Vec<ShaftCoupling> = entry
            .couplings
            .iter()
            .map(|c| ShaftCoupling {
                k_pu: c.k_pu,
                d_mutual_pu: c.d_mutual_pu,
            })
            .collect();

        // Build torque sources.
        // Default: last segment = Electrical, all others = None.
        let mut torque_sources = vec![SegmentTorqueSource::None; n_seg];
        torque_sources[n_seg - 1] = SegmentTorqueSource::Electrical;

        // If segment named "GEN" exists and isn't the last, move Electrical to it.
        if let Some(gen_pos) = segments.iter().position(|s| s.name == "GEN")
            && gen_pos != n_seg - 1
        {
            torque_sources[n_seg - 1] = SegmentTorqueSource::None;
            torque_sources[gen_pos] = SegmentTorqueSource::Electrical;
        }

        // Apply explicit torque source assignments.
        for ts in &entry.torque_sources {
            let seg_idx = segments
                .iter()
                .position(|s| s.name == ts.segment)
                .ok_or_else(|| ShaftError::Validation {
                    index: idx,
                    bus: entry.bus,
                    machine_id: entry.machine_id.clone(),
                    message: format!("torque_source references unknown segment '{}'", ts.segment),
                })?;

            torque_sources[seg_idx] = match ts.source.as_str() {
                "electrical" => SegmentTorqueSource::Electrical,
                "fraction" => {
                    let v = ts.value.unwrap_or(0.0);
                    SegmentTorqueSource::Fraction(v)
                }
                "governor_stage" => {
                    let k = ts.stage.unwrap_or(0);
                    SegmentTorqueSource::GovernorStage(k)
                }
                "none" => SegmentTorqueSource::None,
                other => {
                    return Err(ShaftError::Validation {
                        index: idx,
                        bus: entry.bus,
                        machine_id: entry.machine_id.clone(),
                        message: format!(
                            "unknown torque source type '{other}' \
                             (expected: electrical, fraction, governor_stage, none)"
                        ),
                    });
                }
            };
        }

        let model = ShaftModel {
            segments,
            couplings,
            torque_sources,
        };
        // Validate shaft topology (Electrical count, fraction sum, K>0, H>0).
        if let Err(e) = model.validate() {
            return Err(ShaftError::Validation {
                index: idx,
                bus: entry.bus,
                machine_id: entry.machine_id,
                message: e.to_string(),
            });
        }
        result.push(ShaftDyn {
            bus: entry.bus,
            machine_id: entry.machine_id,
            model,
        });
    }

    Ok(result)
}

/// Parse a TOML file containing shaft model definitions.
pub fn parse_shaft_toml_file(path: &std::path::Path) -> Result<Vec<ShaftDyn>, ShaftError> {
    let content = std::fs::read_to_string(path).map_err(|e| ShaftError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    parse_shaft_toml(&content)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_shaft() {
        let toml = r#"
[[shaft]]
bus = 1
machine_id = "1"

[[shaft.segments]]
name = "HP"
h_pu = 0.1
d_self_pu = 0.0

[[shaft.segments]]
name = "GEN"
h_pu = 0.8
d_self_pu = 0.0

[[shaft.couplings]]
k_pu = 20.0
d_mutual_pu = 0.0
"#;

        let shafts = parse_shaft_toml(toml).unwrap();
        assert_eq!(shafts.len(), 1);
        assert_eq!(shafts[0].bus, 1);
        assert_eq!(shafts[0].machine_id, "1");
        assert_eq!(shafts[0].model.segments.len(), 2);
        assert_eq!(shafts[0].model.couplings.len(), 1);
        // GEN should get Electrical torque source by default.
        assert!(matches!(
            shafts[0].model.torque_sources[1],
            SegmentTorqueSource::Electrical
        ));
    }

    #[test]
    fn test_parse_ieee_fbm_toml() {
        let toml = r#"
[[shaft]]
bus = 1
machine_id = "1"

[[shaft.segments]]
name = "HP"
h_pu = 0.092595

[[shaft.segments]]
name = "IP"
h_pu = 0.155589

[[shaft.segments]]
name = "LPA"
h_pu = 0.858670

[[shaft.segments]]
name = "LPB"
h_pu = 0.884215

[[shaft.segments]]
name = "GEN"
h_pu = 0.868495

[[shaft.segments]]
name = "EXC"
h_pu = 0.034216

[[shaft.couplings]]
k_pu = 19.652

[[shaft.couplings]]
k_pu = 34.929

[[shaft.couplings]]
k_pu = 52.038

[[shaft.couplings]]
k_pu = 70.858

[[shaft.couplings]]
k_pu = 2.822

[[shaft.torque_sources]]
segment = "HP"
source = "fraction"
value = 0.30

[[shaft.torque_sources]]
segment = "IP"
source = "fraction"
value = 0.26

[[shaft.torque_sources]]
segment = "LPA"
source = "fraction"
value = 0.22

[[shaft.torque_sources]]
segment = "LPB"
source = "fraction"
value = 0.22

[[shaft.torque_sources]]
segment = "GEN"
source = "electrical"

[[shaft.torque_sources]]
segment = "EXC"
source = "none"
"#;

        let shafts = parse_shaft_toml(toml).unwrap();
        assert_eq!(shafts.len(), 1);
        let m = &shafts[0].model;
        assert_eq!(m.segments.len(), 6);
        assert_eq!(m.couplings.len(), 5);
        assert!(
            matches!(m.torque_sources[0], SegmentTorqueSource::Fraction(f) if (f - 0.30).abs() < 1e-10)
        );
        assert!(matches!(
            m.torque_sources[4],
            SegmentTorqueSource::Electrical
        ));
        assert!(matches!(m.torque_sources[5], SegmentTorqueSource::None));
    }

    #[test]
    fn test_parse_coupling_count_mismatch() {
        let toml = r#"
[[shaft]]
bus = 1
machine_id = "1"

[[shaft.segments]]
name = "HP"
h_pu = 0.1

[[shaft.segments]]
name = "GEN"
h_pu = 0.8
"#;
        // 2 segments but 0 couplings → error.
        let result = parse_shaft_toml(toml);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("coupling"),
            "error should mention couplings: {msg}"
        );
    }

    #[test]
    fn test_parse_governor_stage_source() {
        let toml = r#"
[[shaft]]
bus = 1
machine_id = "1"

[[shaft.segments]]
name = "HP"
h_pu = 0.1

[[shaft.segments]]
name = "GEN"
h_pu = 0.8

[[shaft.couplings]]
k_pu = 20.0

[[shaft.torque_sources]]
segment = "HP"
source = "governor_stage"
stage = 0

[[shaft.torque_sources]]
segment = "GEN"
source = "electrical"
"#;

        let shafts = parse_shaft_toml(toml).unwrap();
        assert!(matches!(
            shafts[0].model.torque_sources[0],
            SegmentTorqueSource::GovernorStage(0)
        ));
    }
}
