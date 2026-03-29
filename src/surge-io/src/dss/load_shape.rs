// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Load shape definitions — corresponds to OpenDSS LoadShape elements.

use tracing::debug;

/// A load shape — per-unit multiplier time series.
#[derive(Debug, Clone)]
pub struct LoadShape {
    /// Shape name (case-insensitive cross-reference key).
    pub name: String,
    /// Number of data points.
    pub n_pts: usize,
    /// Uniform time step in hours (0 = use `hours` vector for irregular steps).
    pub interval_h: f64,
    /// Per-unit real power multipliers (length == n_pts).
    pub mult: Vec<f64>,
    /// Per-unit reactive power multipliers (length == n_pts or empty).
    pub q_mult: Vec<f64>,
    /// Non-uniform time points in hours (used only when interval_h == 0).
    pub hours: Vec<f64>,
    /// If true, normalise the mult array so its peak is 1.0 pu.
    pub normalise: bool,
    /// Mean of the mult array (used when normalise = true).
    pub mean: Option<f64>,
    /// Standard deviation of the mult array.
    pub std_dev: Option<f64>,
}

impl Default for LoadShape {
    fn default() -> Self {
        Self {
            name: String::new(),
            n_pts: 0,
            interval_h: 1.0,
            mult: Vec::new(),
            q_mult: Vec::new(),
            hours: Vec::new(),
            normalise: false,
            mean: None,
            std_dev: None,
        }
    }
}

impl LoadShape {
    /// Create a flat (constant) load shape with value 1.0 for `n` hourly steps.
    pub fn flat(name: &str, n: usize) -> Self {
        debug!(
            name = name,
            n_pts = n,
            "LoadShape::flat: creating constant shape"
        );
        Self {
            name: name.to_string(),
            n_pts: n,
            interval_h: 1.0,
            mult: vec![1.0; n],
            ..Default::default()
        }
    }

    /// Get the multiplier at position `idx` (wraps around for cyclic shapes).
    pub fn get(&self, idx: usize) -> f64 {
        if self.mult.is_empty() {
            return 1.0;
        }
        let i = idx % self.mult.len();
        self.mult[i]
    }

    /// Get the Q multiplier at position `idx` (falls back to real mult if empty).
    pub fn get_q(&self, idx: usize) -> f64 {
        if self.q_mult.is_empty() {
            return self.get(idx);
        }
        let i = idx % self.q_mult.len();
        self.q_mult[i]
    }

    /// Normalise the mult array in-place so its peak absolute value is 1.0.
    pub fn normalise_peak(&mut self) {
        let peak = self.mult.iter().cloned().fold(0.0_f64, f64::max);
        debug!(name = %self.name, peak = peak, "LoadShape::normalise_peak: normalising");
        if peak > 0.0 {
            self.mult.iter_mut().for_each(|v| *v /= peak);
        }
    }
}
