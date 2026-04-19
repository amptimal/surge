// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared DC network planning helpers used by SCED and SCUC.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::market::ZonalReserveRequirement;

use crate::common::spec::DispatchProblemSpec;

/// An angle-constrained branch entry for the DC dispatch formulation.
///
/// Stores the branch network index, internal bus indices for the from/to
/// endpoints, and the finite angle limits in radians.
#[derive(Clone, Debug)]
pub(crate) struct AngleConstrainedBranch {
    pub branch_idx: usize,
    pub from_bus_idx: usize,
    pub to_bus_idx: usize,
    pub angmin_rad: f64,
    pub angmax_rad: f64,
}

pub(crate) struct DcNetworkPlan {
    pub hvdc_from_idx: Vec<Option<usize>>,
    pub hvdc_to_idx: Vec<Option<usize>>,
    pub constrained_branches: Vec<usize>,
    pub fg_rows: Vec<usize>,
    pub iface_rows: Vec<usize>,
    /// Branches with finite angle-difference limits (angmin/angmax).
    /// Only populated when the angle penalty cost is non-zero.
    pub angle_constrained_branches: Vec<AngleConstrainedBranch>,
}

pub(crate) struct DcNetworkPlanInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub bus_map: &'a HashMap<u32, usize>,
    pub excluded_branches: Option<&'a HashSet<usize>>,
}

pub(crate) fn build_dc_network_plan(input: DcNetworkPlanInput<'_>) -> DcNetworkPlan {
    let hvdc_from_idx = input
        .spec
        .hvdc_links
        .iter()
        .map(|link| input.bus_map.get(&link.from_bus).copied())
        .collect();
    let hvdc_to_idx = input
        .spec
        .hvdc_links
        .iter()
        .map(|link| input.bus_map.get(&link.to_bus).copied())
        .collect();

    let constrained_branches = if input.spec.enforce_thermal_limits {
        input
            .network
            .branches
            .iter()
            .enumerate()
            .filter_map(|(branch_idx, branch)| {
                let excluded = input
                    .excluded_branches
                    .is_some_and(|set| set.contains(&branch_idx));
                (branch.in_service && branch.rating_a_mva >= input.spec.min_rate_a && !excluded)
                    .then_some(branch_idx)
            })
            .collect()
    } else {
        Vec::new()
    };

    let (fg_rows, iface_rows) = if input.spec.enforce_flowgates {
        let fg_rows = input
            .network
            .flowgates
            .iter()
            .enumerate()
            .filter_map(|(idx, fg)| fg.in_service.then_some(idx))
            .collect();
        let iface_rows = input
            .network
            .interfaces
            .iter()
            .enumerate()
            .filter_map(|(idx, iface)| {
                (iface.in_service && iface.limit_forward_mw > 0.0).then_some(idx)
            })
            .collect();
        (fg_rows, iface_rows)
    } else {
        (Vec::new(), Vec::new())
    };

    // Angle-constrained branches: collect branches with at least one
    // finite angle limit, but only when the angle penalty is non-zero.
    let angle_penalty_nonzero = input.spec.angle_penalty_curve.marginal_cost_at(0.0).abs() > 1e-12;
    let angle_constrained_branches = if angle_penalty_nonzero {
        input
            .network
            .branches
            .iter()
            .enumerate()
            .filter_map(|(branch_idx, branch)| {
                if !branch.in_service {
                    return None;
                }
                let angmin = branch
                    .angle_diff_min_rad
                    .filter(|v| v.is_finite())
                    .unwrap_or(f64::NEG_INFINITY);
                let angmax = branch
                    .angle_diff_max_rad
                    .filter(|v| v.is_finite())
                    .unwrap_or(f64::INFINITY);
                if !angmin.is_finite() && !angmax.is_finite() {
                    return None;
                }
                let from_bus_idx = input.bus_map.get(&branch.from_bus).copied()?;
                let to_bus_idx = input.bus_map.get(&branch.to_bus).copied()?;
                Some(AngleConstrainedBranch {
                    branch_idx,
                    from_bus_idx,
                    to_bus_idx,
                    angmin_rad: angmin,
                    angmax_rad: angmax,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    DcNetworkPlan {
        hvdc_from_idx,
        hvdc_to_idx,
        constrained_branches,
        fg_rows,
        iface_rows,
        angle_constrained_branches,
    }
}

pub(crate) fn study_area_for_bus_index(
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
    bus_idx: usize,
) -> Option<usize> {
    spec.load_area
        .get(bus_idx)
        .copied()
        .or_else(|| network.buses.get(bus_idx).map(|bus| bus.area as usize))
}

pub(crate) fn study_area_for_bus(
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
    bus_number: u32,
) -> Option<usize> {
    let bus_idx = network.bus_index_map().get(&bus_number).copied()?;
    study_area_for_bus_index(network, spec, bus_idx)
}

pub(crate) fn zonal_participant_bus_matches(
    zone_id: usize,
    participant_bus_numbers: Option<&[u32]>,
    bus_number: u32,
    fallback_area: Option<usize>,
) -> bool {
    if let Some(participants) = participant_bus_numbers {
        participants.contains(&bus_number)
    } else {
        fallback_area.unwrap_or(0) == zone_id
    }
}

pub(crate) fn zonal_requirement_matches_bus(
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
    requirement: &ZonalReserveRequirement,
    bus_number: u32,
) -> bool {
    zonal_participant_bus_matches(
        requirement.zone_id,
        requirement.participant_bus_numbers.as_deref(),
        bus_number,
        study_area_for_bus(network, spec, bus_number),
    )
}
