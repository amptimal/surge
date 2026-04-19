// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Per-hour network snapshot builder for SCUC.

use surge_hvdc::interop::{apply_dc_grid_injections, dc_grid_injections};
use surge_network::Network;

use crate::common::spec::DispatchProblemSpec;

/// Build a per-hour snapshot of the network from immutable dispatch problem data.
///
/// Application order:
/// 1. Load profiles (bus MW demand)
/// 2. Generator derate factors (equipment availability) — pmax scaled first
/// 3. Renewable capacity factors (fuel/resource availability) — applied to derated pmax
/// 4. Branch derate factors (line/transformer availability)
pub(crate) fn network_at_hour_with_spec(
    base: &Network,
    spec: &DispatchProblemSpec<'_>,
    hour: usize,
) -> Network {
    let mut net = base.clone();
    crate::common::profiles::apply_dc_time_series_profiles(&mut net, spec, hour);

    // Apply explicit DC-grid injections as fixed bus demand adjustments.
    // Uses flat-start AC voltages — appropriate for DC-only dispatch formulation.
    if let Ok(dc_grid) = dc_grid_injections(&net) {
        apply_dc_grid_injections(&mut net, &dc_grid.injections, true);
    }

    net
}
