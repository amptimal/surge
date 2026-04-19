// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical profile-assembly helpers.
//!
//! Markets that model consumers as fixed per-bus demand need to sum
//! individual load time-series onto their bus assignments. Markets
//! that expose per-period generator dispatch bounds likewise need to
//! collect `(p_min, p_max, q_min, q_max)` series and wrap them in the
//! typed [`GeneratorDispatchBoundsProfile`] container.
//!
//! Both are generic market-layer concerns with straightforward
//! implementations that several adapters would otherwise re-invent.
//! They live here so adapters focus on data-source-specific field
//! extraction.

use std::collections::HashMap;

use surge_dispatch::request::{GeneratorDispatchBoundsProfile, GeneratorDispatchBoundsProfiles};
use surge_dispatch::{BusLoadProfile, BusLoadProfiles};

/// Sum per-consumer load series onto their assigned bus numbers.
///
/// Each `(bus_number, values_mw)` pair is added (element-wise) to the
/// bus's running total. Buses with an all-zero total are dropped. The
/// resulting list is sorted by bus number for deterministic output.
///
/// `periods` is the canonical horizon length; series shorter than
/// `periods` are padded with zeros at the tail.
pub fn aggregate_consumer_profiles_by_bus(
    entries: impl IntoIterator<Item = (u32, Vec<f64>)>,
    periods: usize,
) -> BusLoadProfiles {
    let mut bus_load: HashMap<u32, Vec<f64>> = HashMap::new();
    for (bus_number, values) in entries {
        let series = bus_load
            .entry(bus_number)
            .or_insert_with(|| vec![0.0; periods]);
        for (i, &v) in values.iter().enumerate().take(periods) {
            series[i] += v;
        }
    }
    let mut as_vec: Vec<_> = bus_load.into_iter().collect();
    as_vec.sort_by_key(|&(bus, _)| bus);
    let profiles: Vec<BusLoadProfile> = as_vec
        .into_iter()
        .filter(|(_, v)| v.iter().any(|x| x.abs() > 1e-12))
        .map(|(bus_number, values_mw)| BusLoadProfile {
            bus_number,
            values_mw,
        })
        .collect();
    BusLoadProfiles { profiles }
}

/// Aggregate generator dispatch bound profiles into the typed
/// [`GeneratorDispatchBoundsProfiles`] wrapper.
///
/// This is a thin wrapper so adapters can hand a `Vec` to the request
/// builder without referencing the wrapper struct layout.
pub fn build_generator_dispatch_bounds_profiles(
    profiles: Vec<GeneratorDispatchBoundsProfile>,
) -> GeneratorDispatchBoundsProfiles {
    GeneratorDispatchBoundsProfiles { profiles }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_profiles_and_drops_zero_buses() {
        let profiles = aggregate_consumer_profiles_by_bus(
            [
                (7, vec![1.0, 2.0, 3.0]),
                (3, vec![0.5, 0.5, 0.5]),
                (7, vec![0.1, 0.2, 0.3]),
                (11, vec![0.0, 0.0, 0.0]),
            ],
            3,
        );
        assert_eq!(profiles.profiles.len(), 2);
        // Sorted by bus number: 3, then 7.
        assert_eq!(profiles.profiles[0].bus_number, 3);
        assert_eq!(profiles.profiles[1].bus_number, 7);
        // Bus 7 sum: 1.1, 2.2, 3.3
        assert!((profiles.profiles[1].values_mw[0] - 1.1).abs() < 1e-9);
        assert!((profiles.profiles[1].values_mw[2] - 3.3).abs() < 1e-9);
    }

    #[test]
    fn short_series_is_padded_with_zeros() {
        let profiles = aggregate_consumer_profiles_by_bus([(5, vec![1.0]), (5, vec![2.0, 3.0])], 3);
        assert_eq!(profiles.profiles.len(), 1);
        assert_eq!(profiles.profiles[0].values_mw, vec![3.0, 3.0, 0.0]);
    }
}
