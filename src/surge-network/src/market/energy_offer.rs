// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Market energy offer representation.

use serde::{Deserialize, Serialize};

/// A single startup cost tier submitted in a generator's energy offer.
///
/// Each tier represents the cost and synchronization time for a generator
/// that has been offline for up to `max_offline_hours`.  Tiers are ordered
/// ascending by `max_offline_hours`; the last tier (cold start) should use
/// `f64::INFINITY`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupTier {
    /// Maximum offline hours for this tier.  The first tier whose
    /// `max_offline_hours >= actual_offline_hours` is selected.
    pub max_offline_hours: f64,
    /// Startup cost ($) for this tier.
    pub cost: f64,
    /// Synchronization time from start command to Pmin (minutes).
    pub sync_time_min: f64,
}

/// A single offer curve — price-quantity segments + no-load + startup tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferCurve {
    /// Price-quantity segments (MW, $/MWh), sorted ascending by MW.
    /// First segment starts at pmin. Each price is the marginal offer
    /// for that incremental block.
    pub segments: Vec<(f64, f64)>,
    /// No-load cost ($/hr) — cost incurred when committed at zero output.
    pub no_load_cost: f64,
    /// Startup cost tiers, ordered by ascending `max_offline_hours`.
    /// Last tier is the cold-start catch-all (max_offline_hours = ∞).
    pub startup_tiers: Vec<StartupTier>,
}

/// Full energy offer with submitted and mitigated versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyOffer {
    /// Submitted offer — what the resource wants to be paid.
    pub submitted: OfferCurve,
    /// Mitigated offer — ISO reference level, used when mitigation triggered.
    pub mitigated: Option<OfferCurve>,
    /// True = ISO has imposed the mitigated offer for this period.
    pub mitigation_active: bool,
}

/// Time-varying offer schedule for per-period offer variation.
///
/// Each entry overrides the generator's default `EnergyOffer` for that period.
/// This is the primary mechanism for market simulation: generators submit
/// different price-quantity curves for each hour of the operating day.
///
/// # Example
/// ```
/// # use surge_network::market::energy_offer::{OfferSchedule, OfferCurve};
/// let schedule = OfferSchedule {
///     periods: vec![
///         // Period 0: use default offer (None)
///         None,
///         // Period 1: override with $50/MWh flat
///         Some(OfferCurve {
///             segments: vec![(200.0, 50.0)],
///             no_load_cost: 1000.0,
///             startup_tiers: vec![],
///         }),
///     ],
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferSchedule {
    /// Per-period offer curves. Index = period (hour for DA, interval for RT).
    /// `None` = use the generator's default `energy_offer.submitted` for that period.
    pub periods: Vec<Option<OfferCurve>>,
}

impl OfferSchedule {
    /// Get the effective offer curve for a given period.
    ///
    /// Returns the period-specific override if present, otherwise falls back
    /// to the provided default curve.
    pub fn offer_for_period<'a>(
        &'a self,
        period: usize,
        default: &'a OfferCurve,
    ) -> &'a OfferCurve {
        self.periods
            .get(period)
            .and_then(|o| o.as_ref())
            .unwrap_or(default)
    }

    /// Create a schedule where every period uses the same offer curve.
    pub fn constant(curve: OfferCurve, n_periods: usize) -> Self {
        Self {
            periods: vec![Some(curve); n_periods],
        }
    }

    /// Create a schedule with no overrides (all periods use the default).
    pub fn empty(n_periods: usize) -> Self {
        Self {
            periods: vec![None; n_periods],
        }
    }

    /// Number of periods in this schedule.
    pub fn len(&self) -> usize {
        self.periods.len()
    }

    /// Whether this schedule has zero periods.
    pub fn is_empty(&self) -> bool {
        self.periods.is_empty()
    }
}
