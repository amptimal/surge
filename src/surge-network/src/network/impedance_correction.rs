// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Impedance correction tables.
//!
//! Defines tap-position-dependent impedance scaling factors for transformers.
//! Large autotransformers have non-linear impedance-vs-tap characteristics;
//! correction tables allow the solver to adjust R and X at each tap position.
//! PSS/E RAW section: "IMPEDANCE CORRECTION DATA".

use serde::{Deserialize, Serialize};

/// An impedance correction table (PSS/E TAB field on transformers).
///
/// Each entry is a `(T, F)` pair where:
/// - `T` is the tap ratio (or phase angle in degrees for phase shifters)
/// - `F` is the impedance scaling factor (1.0 = nominal)
///
/// The transformer's R and X are multiplied by the interpolated F value
/// at the current tap position. Referenced by `Branch.tab` (the TAB1/TAB2
/// field in PSS/E transformer records).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpedanceCorrectionTable {
    /// Table number (I in PSS/E). Referenced by transformer `tab` field.
    pub number: u32,
    /// Up to 11 (tap_or_angle, scaling_factor) pairs.
    /// Must be sorted by the first element (tap ratio or angle).
    pub entries: Vec<(f64, f64)>,
}

impl ImpedanceCorrectionTable {
    /// Interpolate the scaling factor for a given tap ratio or phase angle.
    ///
    /// Uses linear interpolation between the two nearest entries.
    /// Clamps to the boundary factor if `t` is outside the table range.
    pub fn interpolate(&self, t: f64) -> f64 {
        if self.entries.is_empty() {
            return 1.0;
        }
        if self.entries.len() == 1 {
            return self.entries[0].1;
        }
        // Below first entry
        if t <= self.entries[0].0 {
            return self.entries[0].1;
        }
        // Above last entry
        let last = self.entries.len() - 1;
        if t >= self.entries[last].0 {
            return self.entries[last].1;
        }
        // Linear interpolation
        for i in 0..last {
            let (t0, f0) = self.entries[i];
            let (t1, f1) = self.entries[i + 1];
            if t >= t0 && t <= t1 {
                let frac = (t - t0) / (t1 - t0);
                return f0 + frac * (f1 - f0);
            }
        }
        1.0
    }
}

impl Default for ImpedanceCorrectionTable {
    fn default() -> Self {
        Self {
            number: 1,
            entries: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_within_range() {
        let table = ImpedanceCorrectionTable {
            number: 1,
            entries: vec![(0.9, 1.1), (1.0, 1.0), (1.1, 0.95)],
        };
        assert!((table.interpolate(0.95) - 1.05).abs() < 1e-10);
        assert!((table.interpolate(1.0) - 1.0).abs() < 1e-10);
        assert!((table.interpolate(1.05) - 0.975).abs() < 1e-10);
    }

    #[test]
    fn interpolate_clamps_at_boundaries() {
        let table = ImpedanceCorrectionTable {
            number: 1,
            entries: vec![(0.9, 1.1), (1.1, 0.9)],
        };
        assert!((table.interpolate(0.8) - 1.1).abs() < 1e-10);
        assert!((table.interpolate(1.2) - 0.9).abs() < 1e-10);
    }

    #[test]
    fn interpolate_empty_returns_one() {
        let table = ImpedanceCorrectionTable {
            number: 1,
            entries: vec![],
        };
        assert!((table.interpolate(1.0) - 1.0).abs() < 1e-10);
    }
}
