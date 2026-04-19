// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical commitment helpers.
//!
//! Power-market data sources report a unit's pre-horizon state in a
//! variety of conventions: "hours since last start", "hours since last
//! shutdown", "24-hour start count", etc. This module provides the
//! canonical translations into the [`CommitmentInitialCondition`] type
//! that [`surge_dispatch`] consumes.
//!
//! Startup-tier construction from a base cost and a list of
//! `(max_offline_hours, extra_cost)` pairs is likewise a standard
//! pattern shared across markets; we expose it here so adapters do not
//! each re-implement the same loop.

use surge_dispatch::CommitmentInitialCondition;
use surge_network::market::StartupTier;

/// Build a [`CommitmentInitialCondition`] from a unit's pre-horizon
/// state expressed as "hours since last start" (when currently
/// committed) or "hours since last shutdown" (when currently offline).
///
/// This is the canonical accumulator-based initial condition:
///
/// * When `committed` is `true`, `accumulated_up_hours` is floored and
///   cast to `hours_on`; `offline_hours` is left unset (the unit has
///   been on for at least `hours_on` hours).
/// * When `committed` is `false`, `accumulated_down_hours` is clamped
///   to `>= 0` and stored as `offline_hours`; `hours_on` is left unset.
///
/// Start counts (`starts_24h`, `starts_168h`) and recent energy
/// (`energy_mwh_24h`) are left `None`. Adapters that track those
/// histories can override the returned struct in-place before passing
/// it into [`CommitmentOptions`].
pub fn initial_condition_from_accumulated_times(
    resource_id: String,
    committed: bool,
    accumulated_up_hours: f64,
    accumulated_down_hours: f64,
) -> CommitmentInitialCondition {
    CommitmentInitialCondition {
        resource_id,
        committed: Some(committed),
        hours_on: if committed {
            Some(accumulated_up_hours.max(0.0).floor() as i32)
        } else {
            None
        },
        offline_hours: if committed {
            None
        } else {
            Some(accumulated_down_hours.max(0.0))
        },
        starts_24h: None,
        starts_168h: None,
        energy_mwh_24h: None,
    }
}

/// Assemble a list of [`StartupTier`] structures from a base startup
/// cost and a list of `(max_offline_hours, extra_cost)` pairs.
///
/// `base_cost` is the minimum cost to bring the unit online (often a
/// "hot start"); each tier's `cost` field is `base_cost + extra_cost`.
/// `sync_time_min` is set to zero because the canonical startup-tier
/// surface does not include synchronization time yet; adapters that
/// need it can override the returned vector.
pub fn startup_tiers_from_piecewise(base_cost: f64, tiers: &[(f64, f64)]) -> Vec<StartupTier> {
    tiers
        .iter()
        .map(|&(max_offline_hours, extra_cost)| StartupTier {
            max_offline_hours,
            cost: base_cost + extra_cost,
            sync_time_min: 0.0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_unit_gets_hours_on_from_accumulator() {
        let ic = initial_condition_from_accumulated_times("sd_001".to_string(), true, 12.7, 0.0);
        assert_eq!(ic.resource_id, "sd_001");
        assert_eq!(ic.committed, Some(true));
        assert_eq!(ic.hours_on, Some(12));
        assert_eq!(ic.offline_hours, None);
    }

    #[test]
    fn offline_unit_gets_offline_hours() {
        let ic = initial_condition_from_accumulated_times("sd_001".to_string(), false, 0.0, 48.25);
        assert_eq!(ic.committed, Some(false));
        assert_eq!(ic.hours_on, None);
        assert_eq!(ic.offline_hours, Some(48.25));
    }

    #[test]
    fn negative_accumulators_clamp_to_zero() {
        let ic = initial_condition_from_accumulated_times("sd_001".to_string(), false, 0.0, -5.0);
        assert_eq!(ic.offline_hours, Some(0.0));
    }

    #[test]
    fn startup_tiers_from_piecewise_composes_cost() {
        let tiers =
            startup_tiers_from_piecewise(500.0, &[(8.0, 0.0), (24.0, 300.0), (72.0, 1500.0)]);
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0].cost, 500.0);
        assert_eq!(tiers[1].cost, 800.0);
        assert_eq!(tiers[2].cost, 2000.0);
        assert_eq!(tiers[1].max_offline_hours, 24.0);
    }
}
