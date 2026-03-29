// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Multi-mass torsional shaft model for time-domain SSR simulation.
//!
//! These types define the N-mass shaft topology (segments, couplings, torque
//! sources) used by `surge-dyn` for torsional time-domain integration and by
//! `surge-ssr` for eigenvalue-based frequency screening.

use serde::{Deserialize, Serialize};

/// A single mass segment of a turbine-generator shaft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShaftSegment {
    /// Segment label (e.g., "HP", "IP", "LPA", "LPB", "GEN", "EXC").
    pub name: String,
    /// Inertia constant H (seconds, pu on machine MVA base).
    pub h_pu: f64,
    /// Self-damping coefficient D_self (pu torque per pu speed deviation).
    /// Represents bearing friction, windage losses for this segment.
    pub d_self_pu: f64,
}

/// Torsional spring + viscous coupling between two adjacent shaft segments.
///
/// `couplings[i]` connects `segments[i]` to `segments[i+1]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShaftCoupling {
    /// Spring stiffness K (pu torque / rad).
    pub k_pu: f64,
    /// Mutual (inter-segment) viscous damping D_mutual (pu torque per pu speed difference).
    /// Represents oil film damping in the coupling between adjacent masses.
    /// Set to 0.0 when unknown (standard for most SSR studies including IEEE FBM).
    pub d_mutual_pu: f64,
}

/// Source of mechanical torque for a shaft segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SegmentTorqueSource {
    /// This segment receives electrical torque Te from the network solution.
    /// Exactly one segment per shaft must be Electrical (the GEN segment).
    Electrical,

    /// This segment receives mechanical torque from a specific governor stage output.
    /// For IEEEG1: stage 0 = K2·x_gate, 1 = (K1+K4)·x1, 2 = (K3+K6)·x2, 3 = (K5+K7+K8)·x3.
    GovernorStage(usize),

    /// This segment receives a fixed fraction of the governor's total Pm output.
    /// Used for single-stage governors (TGOV1, GAST, HYGOV) that don't model
    /// per-stage turbine dynamics internally.
    Fraction(f64),

    /// No torque input (e.g., exciter segment).
    None,
}

/// N-mass torsional shaft model for time-domain SSR simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShaftModel {
    /// Ordered HP → EXC along the shaft.
    pub segments: Vec<ShaftSegment>,
    /// Spring + viscous couplings; couplings\[i\] connects segments\[i\] to segments\[i+1\].
    pub couplings: Vec<ShaftCoupling>,
    /// Torque source for each segment. Length must equal segments.len().
    /// Exactly one entry must be SegmentTorqueSource::Electrical (the GEN segment).
    pub torque_sources: Vec<SegmentTorqueSource>,
}

impl ShaftModel {
    /// Index of the GEN (electrical torque) segment.
    ///
    /// Panics if no segment has `SegmentTorqueSource::Electrical` — this is
    /// prevented by `validate()`.
    pub fn gen_segment_idx(&self) -> usize {
        self.torque_sources
            .iter()
            .position(|s| matches!(s, SegmentTorqueSource::Electrical))
            .expect("ShaftModel has no Electrical segment — call validate() first")
    }

    /// Validate the shaft model topology and torque source assignments.
    pub fn validate(&self) -> Result<(), String> {
        let n = self.segments.len();
        if n == 0 {
            return Err("shaft must have at least one segment".into());
        }
        if self.couplings.len() != n - 1 {
            return Err(format!(
                "expected {} couplings for {} segments, got {}",
                n - 1,
                n,
                self.couplings.len()
            ));
        }
        if self.torque_sources.len() != n {
            return Err(format!(
                "torque_sources length {} != segments length {}",
                self.torque_sources.len(),
                n
            ));
        }

        // Exactly one Electrical segment
        let n_elec = self
            .torque_sources
            .iter()
            .filter(|s| matches!(s, SegmentTorqueSource::Electrical))
            .count();
        if n_elec != 1 {
            return Err(format!(
                "exactly one Electrical torque source required, found {}",
                n_elec
            ));
        }

        // All H > 0
        for (i, seg) in self.segments.iter().enumerate() {
            if seg.h_pu <= 0.0 {
                return Err(format!(
                    "segment {} ({}) has H={} <= 0",
                    i, seg.name, seg.h_pu
                ));
            }
        }

        // All K > 0
        for (i, c) in self.couplings.iter().enumerate() {
            if c.k_pu <= 0.0 {
                return Err(format!("coupling {} has K={} <= 0", i, c.k_pu));
            }
        }

        // Sum of Fraction values for non-Electrical segments = 1.0
        let mut frac_sum = 0.0_f64;
        let mut has_fractions = false;
        for ts in &self.torque_sources {
            if let SegmentTorqueSource::Fraction(f) = ts {
                frac_sum += f;
                has_fractions = true;
            }
        }
        if has_fractions && (frac_sum - 1.0).abs() > 1e-6 {
            return Err(format!(
                "torque fractions sum to {}, expected 1.0",
                frac_sum
            ));
        }

        Ok(())
    }
}

/// Build the IEEE First Benchmark Model shaft.
///
/// 6-mass shaft (HP/IP/LPA/LPB/GEN/EXC) with published inertias, stiffnesses,
/// and torque fractions. Zero damping (standard for FBM validation).
///
/// Reference: IEEE Trans. PAS-96, No.5, Sep/Oct 1977.
/// Data matches `surge-ssr/src/ieee_fbm.rs` canonical values.
pub fn ieee_fbm_shaft_model() -> ShaftModel {
    let names = ["HP", "IP", "LPA", "LPB", "GEN", "EXC"];
    let h_vals = [0.092595, 0.155589, 0.858670, 0.884215, 0.868495, 0.034216];
    let k_vals = [19.652, 34.929, 52.038, 70.858, 2.822];

    let segments = names
        .iter()
        .zip(h_vals.iter())
        .map(|(&name, &h)| ShaftSegment {
            name: name.to_string(),
            h_pu: h,
            d_self_pu: 0.0,
        })
        .collect();

    let couplings = k_vals
        .iter()
        .map(|&k| ShaftCoupling {
            k_pu: k,
            d_mutual_pu: 0.0,
        })
        .collect();

    let torque_sources = vec![
        SegmentTorqueSource::Fraction(0.30), // HP
        SegmentTorqueSource::Fraction(0.26), // IP
        SegmentTorqueSource::Fraction(0.22), // LPA
        SegmentTorqueSource::Fraction(0.22), // LPB
        SegmentTorqueSource::Electrical,     // GEN
        SegmentTorqueSource::None,           // EXC
    ];

    ShaftModel {
        segments,
        couplings,
        torque_sources,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ieee_fbm_validates() {
        let model = ieee_fbm_shaft_model();
        model.validate().unwrap();
        assert_eq!(model.gen_segment_idx(), 4);
    }

    #[test]
    fn test_validate_no_electrical() {
        let model = ShaftModel {
            segments: vec![
                ShaftSegment {
                    name: "A".into(),
                    h_pu: 1.0,
                    d_self_pu: 0.0,
                },
                ShaftSegment {
                    name: "B".into(),
                    h_pu: 1.0,
                    d_self_pu: 0.0,
                },
            ],
            couplings: vec![ShaftCoupling {
                k_pu: 10.0,
                d_mutual_pu: 0.0,
            }],
            torque_sources: vec![
                SegmentTorqueSource::Fraction(1.0),
                SegmentTorqueSource::None,
            ],
        };
        assert!(model.validate().unwrap_err().contains("Electrical"));
    }

    #[test]
    fn test_validate_bad_fraction_sum() {
        let model = ShaftModel {
            segments: vec![
                ShaftSegment {
                    name: "HP".into(),
                    h_pu: 1.0,
                    d_self_pu: 0.0,
                },
                ShaftSegment {
                    name: "GEN".into(),
                    h_pu: 1.0,
                    d_self_pu: 0.0,
                },
            ],
            couplings: vec![ShaftCoupling {
                k_pu: 10.0,
                d_mutual_pu: 0.0,
            }],
            torque_sources: vec![
                SegmentTorqueSource::Fraction(0.5),
                SegmentTorqueSource::Electrical,
            ],
        };
        assert!(model.validate().unwrap_err().contains("fractions sum"));
    }

    #[test]
    fn test_validate_coupling_count() {
        let model = ShaftModel {
            segments: vec![
                ShaftSegment {
                    name: "A".into(),
                    h_pu: 1.0,
                    d_self_pu: 0.0,
                },
                ShaftSegment {
                    name: "B".into(),
                    h_pu: 1.0,
                    d_self_pu: 0.0,
                },
            ],
            couplings: vec![], // should be 1
            torque_sources: vec![
                SegmentTorqueSource::Fraction(1.0),
                SegmentTorqueSource::Electrical,
            ],
        };
        assert!(model.validate().unwrap_err().contains("couplings"));
    }

    #[test]
    fn test_validate_zero_inertia() {
        let model = ShaftModel {
            segments: vec![
                ShaftSegment {
                    name: "A".into(),
                    h_pu: 0.0,
                    d_self_pu: 0.0,
                },
                ShaftSegment {
                    name: "B".into(),
                    h_pu: 1.0,
                    d_self_pu: 0.0,
                },
            ],
            couplings: vec![ShaftCoupling {
                k_pu: 10.0,
                d_mutual_pu: 0.0,
            }],
            torque_sources: vec![
                SegmentTorqueSource::Fraction(1.0),
                SegmentTorqueSource::Electrical,
            ],
        };
        assert!(model.validate().unwrap_err().contains("H=0"));
    }
}
