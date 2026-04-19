// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Study timeline types for dispatch requests.

use schemars::JsonSchema;

use crate::error::ScedError;

/// Study timeline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct DispatchTimeline {
    /// Number of dispatch intervals.
    pub periods: usize,
    /// Interval width in hours.
    pub interval_hours: f64,
    /// Optional per-period interval widths in hours.
    ///
    /// When provided, this overrides `interval_hours` for period-specific
    /// calculations while preserving the scalar field for backwards-compatible
    /// callers and serialization.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interval_hours_by_period: Vec<f64>,
}

impl Default for DispatchTimeline {
    fn default() -> Self {
        Self {
            periods: 1,
            interval_hours: 1.0,
            interval_hours_by_period: Vec::new(),
        }
    }
}

impl DispatchTimeline {
    /// Create a timeline with hourly intervals.
    pub fn hourly(periods: usize) -> Self {
        Self {
            periods,
            interval_hours: 1.0,
            interval_hours_by_period: Vec::new(),
        }
    }

    /// Create a timeline with explicit interval widths for each period.
    pub fn variable(interval_hours_by_period: Vec<f64>) -> Self {
        let periods = interval_hours_by_period.len();
        let interval_hours = if periods == 0 {
            1.0
        } else {
            interval_hours_by_period.iter().sum::<f64>() / periods as f64
        };
        Self {
            periods,
            interval_hours,
            interval_hours_by_period,
        }
    }

    /// Return the effective per-period interval widths in hours.
    pub fn resolved_interval_hours(&self) -> Vec<f64> {
        if self.interval_hours_by_period.is_empty() {
            vec![self.interval_hours; self.periods]
        } else {
            self.interval_hours_by_period.clone()
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ScedError> {
        if self.periods == 0 {
            return Err(ScedError::InvalidInput(
                "dispatch request requires periods >= 1".to_string(),
            ));
        }
        if !self.interval_hours_by_period.is_empty()
            && self.interval_hours_by_period.len() != self.periods
        {
            return Err(ScedError::InvalidInput(format!(
                "dispatch request interval_hours_by_period length {} must match periods {}",
                self.interval_hours_by_period.len(),
                self.periods
            )));
        }
        if self.interval_hours_by_period.is_empty()
            && !(self.interval_hours.is_finite() && self.interval_hours > 0.0)
        {
            return Err(ScedError::InvalidInput(format!(
                "dispatch request requires interval_hours > 0, got {}",
                self.interval_hours
            )));
        }
        for (period, hours) in self.interval_hours_by_period.iter().enumerate() {
            if !(hours.is_finite() && *hours > 0.0) {
                return Err(ScedError::InvalidInput(format!(
                    "dispatch request interval_hours_by_period[{period}] must be > 0, got {hours}",
                )));
            }
        }
        Ok(())
    }
}
