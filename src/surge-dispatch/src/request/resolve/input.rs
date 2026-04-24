// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Dispatch-input normalization helpers.

use std::collections::{HashMap, HashSet};

use surge_network::Network;

use crate::config::emissions::{
    EmissionProfile as IndexedEmissionProfile, MustRunUnits as IndexedMustRunUnits,
};
use crate::dispatch::{
    IndexedCommitmentConstraint, IndexedCommitmentTerm, IndexedDispatchInitialState,
    IndexedEnergyWindowLimit, IndexedPhHeadCurve, IndexedPhModeConstraint,
    IndexedStartupWindowLimit,
};
use crate::error::ScedError;
use crate::request::{DispatchInput, DispatchRequest, DispatchSolveOptions};

use super::registry::{ResolveCatalog, resolve_combined_cycle_config};

pub(crate) fn build_input(
    request: &DispatchRequest,
    network: Option<&Network>,
    catalog: Option<&ResolveCatalog>,
    solve_options: &DispatchSolveOptions,
) -> Result<DispatchInput, ScedError> {
    let period_hours = request.timeline.resolved_interval_hours();
    let mut period_hour_prefix = Vec::with_capacity(period_hours.len() + 1);
    period_hour_prefix.push(0.0);
    for hours in &period_hours {
        let next = period_hour_prefix.last().copied().unwrap_or(0.0) + *hours;
        period_hour_prefix.push(next);
    }
    let representative_dt_hours = if period_hours.is_empty() {
        request.timeline.interval_hours
    } else {
        period_hours.iter().sum::<f64>() / period_hours.len() as f64
    };
    let mut input = DispatchInput {
        n_periods: request.timeline.periods,
        dt_hours: representative_dt_hours,
        period_hours,
        period_hour_prefix,
        load_profiles: request.profiles.load.to_indexed(request.timeline.periods),
        ac_bus_load_profiles: request.profiles.ac_bus_load.clone(),
        renewable_profiles: request
            .profiles
            .renewable
            .to_indexed(request.timeline.periods),
        gen_derate_profiles: request
            .profiles
            .generator_derates
            .to_indexed(request.timeline.periods),
        generator_dispatch_bounds: request.profiles.generator_dispatch_bounds.clone(),
        branch_derate_profiles: request
            .profiles
            .branch_derates
            .to_indexed(request.timeline.periods),
        hvdc_derate_profiles: request
            .profiles
            .hvdc_derates
            .to_indexed(request.timeline.periods),
        initial_state: IndexedDispatchInitialState::default(),
        tolerance: request.runtime.tolerance,
        enforce_thermal_limits: request.network.thermal_limits.enforce,
        min_rate_a: request.network.thermal_limits.min_rate_a,
        enforce_flowgates: request.network.flowgates.enabled,
        max_nomogram_iter: request.network.flowgates.max_nomogram_iterations,
        par_setpoints: request.network.par_setpoints.clone(),
        reserve_products: request.market.reserve_products.clone(),
        system_reserve_requirements: request.market.system_reserve_requirements.clone(),
        zonal_reserve_requirements: request.market.zonal_reserve_requirements.clone(),
        ramp_sharing: request.market.ramp_sharing.clone(),
        co2_cap_t: request.market.co2_cap_t,
        co2_price_per_t: request.market.co2_price_per_t,
        emission_profile: None,
        carbon_price: request.market.carbon_price,
        storage_self_schedules: None,
        storage_reserve_soc_impact: HashMap::new(),
        offer_schedules: HashMap::new(),
        dl_offer_schedules: HashMap::new(),
        gen_reserve_offer_schedules: HashMap::new(),
        dl_reserve_offer_schedules: HashMap::new(),
        cc_config_offers: Vec::new(),
        hvdc_links: request.network.hvdc_links.clone(),
        tie_line_limits: request.market.tie_line_limits.clone(),
        generator_area: Vec::new(),
        load_area: Vec::new(),
        must_run_units: None,
        frequency_security: request.market.frequency_security.clone(),
        dispatchable_loads: request.market.dispatchable_loads.clone(),
        virtual_bids: request.market.virtual_bids.clone(),
        power_balance_penalty: request.market.power_balance_penalty.clone(),
        penalty_config: request.market.penalty_config.clone(),
        generator_cost_modeling: request.market.generator_cost_modeling.clone(),
        use_loss_factors: request.network.loss_factors.enabled,
        max_loss_factor_iters: request.network.loss_factors.max_iterations,
        loss_factor_tol: request.network.loss_factors.tolerance,
        loss_factor_warm_start_mode: request.network.loss_factors.warm_start_mode,
        enforce_forbidden_zones: request.network.forbidden_zones.enabled,
        foz_max_transit_periods: request.network.forbidden_zones.max_transit_periods,
        enforce_shutdown_deloading: request.network.commitment_transitions.shutdown_deloading,
        offline_commitment_trajectories: matches!(
            request.network.commitment_transitions.trajectory_mode,
            crate::request::CommitmentTrajectoryMode::OfflineTrajectory
        ),
        ramp_mode: request.network.ramping.mode.clone(),
        ramp_constraints_hard: request.network.ramping.enforcement.is_hard(),
        energy_window_constraints_hard: request.network.energy_windows.enforcement.is_hard(),
        energy_window_violation_per_puh: request.network.energy_windows.penalty_per_puh,
        allow_branch_switching: matches!(
            request.network.topology_control.mode,
            crate::request::TopologyControlMode::Switchable
        ),
        branch_switching_big_m_factor: request
            .network
            .topology_control
            .branch_switching_big_m_factor,
        regulation_eligible: None,
        startup_window_limits: Vec::new(),
        energy_window_limits: Vec::new(),
        commitment_constraints: Vec::new(),
        ph_head_curves: Vec::new(),
        ph_mode_constraints: Vec::new(),
        ac_generator_warm_start_p_mw: HashMap::new(),
        ac_generator_warm_start_q_mvar: HashMap::new(),
        ac_bus_warm_start_vm_pu: HashMap::new(),
        ac_bus_warm_start_va_rad: HashMap::new(),
        ac_dispatchable_load_warm_start_p_mw: HashMap::new(),
        ac_dispatchable_load_warm_start_q_mvar: HashMap::new(),
        fixed_hvdc_dispatch_mw: HashMap::new(),
        fixed_hvdc_dispatch_q_fr_mvar: HashMap::new(),
        fixed_hvdc_dispatch_q_to_mvar: HashMap::new(),
        ac_hvdc_warm_start_p_mw: HashMap::new(),
        ac_hvdc_warm_start_q_fr_mvar: HashMap::new(),
        ac_hvdc_warm_start_q_to_mvar: HashMap::new(),
        ac_target_tracking: request.runtime.ac_target_tracking.clone(),
        sced_ac_benders: request.runtime.sced_ac_benders.clone(),
        run_pricing: request.runtime.run_pricing,
        ac_relax_committed_pmin_to_zero: request.runtime.ac_relax_committed_pmin_to_zero,
        lp_solver: solve_options.lp_solver.clone(),
        capture_model_diagnostics: request.runtime.capture_model_diagnostics,
        scuc_firm_bus_balance_slacks: request.runtime.scuc_firm_bus_balance_slacks,
        scuc_firm_branch_thermal_slacks: request.runtime.scuc_firm_branch_thermal_slacks,
        scuc_disable_bus_power_balance: request.runtime.scuc_disable_bus_power_balance,
        ac_sced_period_concurrency: request.runtime.ac_sced_period_concurrency,
    };

    if !request.profiles.hvdc_derates.profiles.is_empty() {
        let hvdc_names_by_id: HashMap<String, String> = request
            .network
            .hvdc_links
            .iter()
            .enumerate()
            .map(|(source_index, link)| {
                let link_id = if !link.id.is_empty() {
                    link.id.clone()
                } else if !link.name.is_empty() {
                    link.name.clone()
                } else {
                    format!("hvdc:{source_index}")
                };
                (link_id, link.name.clone())
            })
            .collect();
        input.hvdc_derate_profiles = surge_network::market::HvdcDerateProfiles {
            profiles: request
                .profiles
                .hvdc_derates
                .profiles
                .iter()
                .map(|profile| surge_network::market::HvdcDerateProfile {
                    name: hvdc_names_by_id
                        .get(profile.link_id.as_str())
                        .cloned()
                        .unwrap_or_else(|| profile.link_id.clone()),
                    derate_factors: profile.derate_factors.clone(),
                })
                .collect(),
            n_timesteps: request.timeline.periods,
        };
    }

    let requires_network = !request.state.initial.previous_resource_dispatch.is_empty()
        || !request.state.initial.previous_hvdc_dispatch.is_empty()
        || !request.state.initial.storage_soc_overrides.is_empty()
        || !request.runtime.fixed_hvdc_dispatch.is_empty()
        || !request.runtime.ac_dispatch_warm_start.is_empty()
        || request
            .market
            .emission_profile
            .as_ref()
            .is_some_and(|profile| !profile.resources.is_empty())
        || !request.market.storage_self_schedules.is_empty()
        || !request.market.storage_reserve_soc_impacts.is_empty()
        || !request.market.generator_offer_schedules.is_empty()
        || !request.market.dispatchable_load_offer_schedules.is_empty()
        || !request.market.generator_reserve_offer_schedules.is_empty()
        || !request
            .market
            .dispatchable_load_reserve_offer_schedules
            .is_empty()
        || !request.market.combined_cycle_offer_schedules.is_empty()
        || !request.market.resource_area_assignments.is_empty()
        || !request.market.bus_area_assignments.is_empty()
        || request
            .market
            .must_run_units
            .as_ref()
            .is_some_and(|units| !units.resource_ids.is_empty())
        || !request.market.regulation_eligibility.is_empty()
        || !request.market.commitment_constraints.is_empty()
        || !request.network.ph_head_curves.is_empty()
        || !request.network.ph_mode_constraints.is_empty();

    let Some(network) = network else {
        if requires_network {
            return Err(ScedError::InvalidInput(
                "request normalization without a network is only available when keyed selectors are unused".to_string(),
            ));
        }
        return Ok(input);
    };

    let catalog = catalog.expect("resolve catalog required when network is provided");

    if !request.state.initial.previous_resource_dispatch.is_empty() {
        let mut prev_dispatch_mw = vec![0.0; catalog.n_in_service_generators()];
        let mut prev_dispatch_mask = vec![false; catalog.n_in_service_generators()];
        for point in &request.state.initial.previous_resource_dispatch {
            let Some(local_idx) = catalog.resolve_local_gen(point.resource_id.as_str()) else {
                return Err(ScedError::InvalidInput(format!(
                    "previous_resource_dispatch references unknown dispatch resource {}",
                    point.resource_id
                )));
            };
            if !point.mw.is_finite() {
                return Err(ScedError::InvalidInput(format!(
                    "previous_resource_dispatch for {} must be finite",
                    point.resource_id
                )));
            }
            if prev_dispatch_mask[local_idx] {
                return Err(ScedError::InvalidInput(format!(
                    "previous_resource_dispatch contains duplicate resource {}",
                    point.resource_id
                )));
            }
            prev_dispatch_mw[local_idx] = point.mw;
            prev_dispatch_mask[local_idx] = true;
        }
        input.initial_state.prev_dispatch_mw = Some(prev_dispatch_mw);
        input.initial_state.prev_dispatch_mask = Some(prev_dispatch_mask);
    }

    if !request.state.initial.previous_hvdc_dispatch.is_empty() {
        let mut prev_hvdc_dispatch_mw = vec![0.0; request.network.hvdc_links.len()];
        let mut prev_hvdc_dispatch_mask = vec![false; request.network.hvdc_links.len()];
        for point in &request.state.initial.previous_hvdc_dispatch {
            let Some(link_idx) =
                catalog.resolve_hvdc(point.link_id.as_str(), request.network.hvdc_links.len())
            else {
                return Err(ScedError::InvalidInput(format!(
                    "previous_hvdc_dispatch references unknown link_id {}",
                    point.link_id
                )));
            };
            if !point.mw.is_finite() {
                return Err(ScedError::InvalidInput(format!(
                    "previous_hvdc_dispatch for {} must be finite",
                    point.link_id
                )));
            }
            if prev_hvdc_dispatch_mask[link_idx] {
                return Err(ScedError::InvalidInput(format!(
                    "previous_hvdc_dispatch contains duplicate link_id {}",
                    point.link_id
                )));
            }
            prev_hvdc_dispatch_mw[link_idx] = point.mw;
            prev_hvdc_dispatch_mask[link_idx] = true;
        }
        input.initial_state.prev_hvdc_dispatch_mw = Some(prev_hvdc_dispatch_mw);
        input.initial_state.prev_hvdc_dispatch_mask = Some(prev_hvdc_dispatch_mask);
    }

    if !request.state.initial.storage_soc_overrides.is_empty() {
        let mut storage_soc_override = HashMap::new();
        for override_row in &request.state.initial.storage_soc_overrides {
            let Some(global_idx) = catalog.resolve_global_gen(override_row.resource_id.as_str())
            else {
                return Err(ScedError::InvalidInput(format!(
                    "storage_soc_overrides references unknown resource {}",
                    override_row.resource_id
                )));
            };
            let generator = &network.generators[global_idx];
            if !generator.is_storage() {
                return Err(ScedError::InvalidInput(format!(
                    "storage_soc_overrides resource {} is not a storage unit",
                    override_row.resource_id
                )));
            }
            if !override_row.soc_mwh.is_finite() {
                return Err(ScedError::InvalidInput(format!(
                    "storage_soc_overrides for {} must be finite",
                    override_row.resource_id
                )));
            }
            if storage_soc_override
                .insert(global_idx, override_row.soc_mwh)
                .is_some()
            {
                return Err(ScedError::InvalidInput(format!(
                    "storage_soc_overrides contains duplicate resource {}",
                    override_row.resource_id
                )));
            }
        }
        input.initial_state.storage_soc_override = Some(storage_soc_override);
    }

    if !request.runtime.ac_dispatch_warm_start.generators.is_empty() {
        let mut seen_resources = HashSet::new();
        for schedule in &request.runtime.ac_dispatch_warm_start.generators {
            let Some(local_idx) = catalog.resolve_local_gen(schedule.resource_id.as_str()) else {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.generators references unknown resource {}",
                    schedule.resource_id
                )));
            };
            if !seen_resources.insert(local_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.generators contains duplicate resource {}",
                    schedule.resource_id
                )));
            }
            if schedule.p_mw.len() != request.timeline.periods {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.generators schedule for {} has {} periods but request timeline expects {}",
                    schedule.resource_id,
                    schedule.p_mw.len(),
                    request.timeline.periods
                )));
            }
            if schedule.p_mw.iter().any(|value| !value.is_finite()) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.generators for {} must be finite",
                    schedule.resource_id
                )));
            }
            if !schedule.q_mvar.is_empty() {
                if schedule.q_mvar.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.generators q_mvar schedule for {} has {} periods but request timeline expects {}",
                        schedule.resource_id,
                        schedule.q_mvar.len(),
                        request.timeline.periods
                    )));
                }
                if schedule.q_mvar.iter().any(|value| !value.is_finite()) {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.generators q_mvar for {} must be finite",
                        schedule.resource_id
                    )));
                }
                input
                    .ac_generator_warm_start_q_mvar
                    .insert(local_idx, schedule.q_mvar.clone());
            }
            input
                .ac_generator_warm_start_p_mw
                .insert(local_idx, schedule.p_mw.clone());
        }
    }

    if !request.runtime.ac_dispatch_warm_start.buses.is_empty() {
        let mut seen_buses = HashSet::new();
        for schedule in &request.runtime.ac_dispatch_warm_start.buses {
            let Some(bus_idx) = catalog.resolve_bus(schedule.bus_number, network.buses.len())
            else {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.buses references unknown bus_number {}",
                    schedule.bus_number
                )));
            };
            if !seen_buses.insert(bus_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.buses contains duplicate bus_number {}",
                    schedule.bus_number
                )));
            }
            if schedule.vm_pu.len() != request.timeline.periods {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.buses vm_pu schedule for bus {} has {} periods but request timeline expects {}",
                    schedule.bus_number,
                    schedule.vm_pu.len(),
                    request.timeline.periods
                )));
            }
            if schedule.va_rad.len() != request.timeline.periods {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.buses va_rad schedule for bus {} has {} periods but request timeline expects {}",
                    schedule.bus_number,
                    schedule.va_rad.len(),
                    request.timeline.periods
                )));
            }
            if schedule.vm_pu.iter().any(|value| !value.is_finite()) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.buses vm_pu for bus {} must be finite",
                    schedule.bus_number
                )));
            }
            if schedule.va_rad.iter().any(|value| !value.is_finite()) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.buses va_rad for bus {} must be finite",
                    schedule.bus_number
                )));
            }
            input
                .ac_bus_warm_start_vm_pu
                .insert(bus_idx, schedule.vm_pu.clone());
            input
                .ac_bus_warm_start_va_rad
                .insert(bus_idx, schedule.va_rad.clone());
        }
    }

    if !request
        .runtime
        .ac_dispatch_warm_start
        .dispatchable_loads
        .is_empty()
    {
        let mut seen_resources = HashSet::new();
        for schedule in &request.runtime.ac_dispatch_warm_start.dispatchable_loads {
            let Some(dl_idx) = catalog.resolve_dispatchable_load(
                schedule.resource_id.as_str(),
                request.market.dispatchable_loads.len(),
            ) else {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.dispatchable_loads references unknown resource {}",
                    schedule.resource_id
                )));
            };
            if !seen_resources.insert(dl_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.dispatchable_loads contains duplicate resource {}",
                    schedule.resource_id
                )));
            }
            if schedule.p_mw.len() != request.timeline.periods {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.dispatchable_loads schedule for {} has {} periods but request timeline expects {}",
                    schedule.resource_id,
                    schedule.p_mw.len(),
                    request.timeline.periods
                )));
            }
            if schedule.p_mw.iter().any(|value| !value.is_finite()) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.dispatchable_loads p_mw for {} must be finite",
                    schedule.resource_id
                )));
            }
            if !schedule.q_mvar.is_empty() {
                if schedule.q_mvar.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.dispatchable_loads q_mvar schedule for {} has {} periods but request timeline expects {}",
                        schedule.resource_id,
                        schedule.q_mvar.len(),
                        request.timeline.periods
                    )));
                }
                if schedule.q_mvar.iter().any(|value| !value.is_finite()) {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.dispatchable_loads q_mvar for {} must be finite",
                        schedule.resource_id
                    )));
                }
                input
                    .ac_dispatchable_load_warm_start_q_mvar
                    .insert(dl_idx, schedule.q_mvar.clone());
            }
            input
                .ac_dispatchable_load_warm_start_p_mw
                .insert(dl_idx, schedule.p_mw.clone());
        }
    }

    if !request.runtime.ac_dispatch_warm_start.hvdc_links.is_empty() {
        let mut seen_links = HashSet::new();
        for schedule in &request.runtime.ac_dispatch_warm_start.hvdc_links {
            let Some(link_idx) =
                catalog.resolve_hvdc(schedule.link_id.as_str(), request.network.hvdc_links.len())
            else {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.hvdc_links references unknown link_id {}",
                    schedule.link_id
                )));
            };
            if !seen_links.insert(link_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.hvdc_links contains duplicate link_id {}",
                    schedule.link_id
                )));
            }
            if schedule.p_mw.len() != request.timeline.periods {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.hvdc_links schedule for {} has {} periods but request timeline expects {}",
                    schedule.link_id,
                    schedule.p_mw.len(),
                    request.timeline.periods
                )));
            }
            if schedule.p_mw.iter().any(|value| !value.is_finite()) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.ac_dispatch_warm_start.hvdc_links p_mw for {} must be finite",
                    schedule.link_id
                )));
            }
            input
                .ac_hvdc_warm_start_p_mw
                .insert(link_idx, schedule.p_mw.clone());
            if !schedule.q_fr_mvar.is_empty() {
                if schedule.q_fr_mvar.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.hvdc_links q_fr_mvar schedule for {} has {} periods but request timeline expects {}",
                        schedule.link_id,
                        schedule.q_fr_mvar.len(),
                        request.timeline.periods
                    )));
                }
                if schedule.q_fr_mvar.iter().any(|value| !value.is_finite()) {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.hvdc_links q_fr_mvar for {} must be finite",
                        schedule.link_id
                    )));
                }
                input
                    .ac_hvdc_warm_start_q_fr_mvar
                    .insert(link_idx, schedule.q_fr_mvar.clone());
            }
            if !schedule.q_to_mvar.is_empty() {
                if schedule.q_to_mvar.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.hvdc_links q_to_mvar schedule for {} has {} periods but request timeline expects {}",
                        schedule.link_id,
                        schedule.q_to_mvar.len(),
                        request.timeline.periods
                    )));
                }
                if schedule.q_to_mvar.iter().any(|value| !value.is_finite()) {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.ac_dispatch_warm_start.hvdc_links q_to_mvar for {} must be finite",
                        schedule.link_id
                    )));
                }
                input
                    .ac_hvdc_warm_start_q_to_mvar
                    .insert(link_idx, schedule.q_to_mvar.clone());
            }
        }
    }

    if !request.runtime.fixed_hvdc_dispatch.is_empty() {
        let mut seen_links = HashSet::new();
        for schedule in &request.runtime.fixed_hvdc_dispatch {
            let Some(link_idx) =
                catalog.resolve_hvdc(schedule.link_id.as_str(), request.network.hvdc_links.len())
            else {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.fixed_hvdc_dispatch references unknown link_id {}",
                    schedule.link_id
                )));
            };
            if !seen_links.insert(link_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.fixed_hvdc_dispatch contains duplicate link_id {}",
                    schedule.link_id
                )));
            }
            if schedule.p_mw.len() != request.timeline.periods {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.fixed_hvdc_dispatch schedule for {} has {} periods but request timeline expects {}",
                    schedule.link_id,
                    schedule.p_mw.len(),
                    request.timeline.periods
                )));
            }
            if schedule.p_mw.iter().any(|value| !value.is_finite()) {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.fixed_hvdc_dispatch p_mw for {} must be finite",
                    schedule.link_id
                )));
            }
            if request.network.hvdc_links[link_idx].is_banded() {
                return Err(ScedError::InvalidInput(format!(
                    "runtime.fixed_hvdc_dispatch does not support banded HVDC link {}",
                    schedule.link_id
                )));
            }
            input
                .fixed_hvdc_dispatch_mw
                .insert(link_idx, schedule.p_mw.clone());
            if !schedule.q_fr_mvar.is_empty() {
                if schedule.q_fr_mvar.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.fixed_hvdc_dispatch q_fr_mvar schedule for {} has {} periods but request timeline expects {}",
                        schedule.link_id,
                        schedule.q_fr_mvar.len(),
                        request.timeline.periods
                    )));
                }
                if schedule.q_fr_mvar.iter().any(|value| !value.is_finite()) {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.fixed_hvdc_dispatch q_fr_mvar for {} must be finite",
                        schedule.link_id
                    )));
                }
                input
                    .fixed_hvdc_dispatch_q_fr_mvar
                    .insert(link_idx, schedule.q_fr_mvar.clone());
            }
            if !schedule.q_to_mvar.is_empty() {
                if schedule.q_to_mvar.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.fixed_hvdc_dispatch q_to_mvar schedule for {} has {} periods but request timeline expects {}",
                        schedule.link_id,
                        schedule.q_to_mvar.len(),
                        request.timeline.periods
                    )));
                }
                if schedule.q_to_mvar.iter().any(|value| !value.is_finite()) {
                    return Err(ScedError::InvalidInput(format!(
                        "runtime.fixed_hvdc_dispatch q_to_mvar for {} must be finite",
                        schedule.link_id
                    )));
                }
                input
                    .fixed_hvdc_dispatch_q_to_mvar
                    .insert(link_idx, schedule.q_to_mvar.clone());
            }
        }
    }

    if let Some(profile) = &request.market.emission_profile
        && !profile.resources.is_empty()
    {
        let mut rates_tonnes_per_mwh = vec![0.0; catalog.n_in_service_generators()];
        let mut seen_resources = HashSet::new();
        for entry in &profile.resources {
            let Some(local_idx) = catalog.resolve_local_gen(entry.resource_id.as_str()) else {
                return Err(ScedError::InvalidInput(format!(
                    "emission_profile references unknown resource {}",
                    entry.resource_id
                )));
            };
            if !entry.rate_tonnes_per_mwh.is_finite() {
                return Err(ScedError::InvalidInput(format!(
                    "emission_profile rate for {} must be finite",
                    entry.resource_id
                )));
            }
            if !seen_resources.insert(local_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "emission_profile contains duplicate resource {}",
                    entry.resource_id
                )));
            }
            rates_tonnes_per_mwh[local_idx] = entry.rate_tonnes_per_mwh;
        }
        input.emission_profile = Some(IndexedEmissionProfile {
            rates_tonnes_per_mwh,
        });
    }

    if !request.market.storage_self_schedules.is_empty() {
        let mut storage_self_schedules = HashMap::new();
        for schedule in &request.market.storage_self_schedules {
            let Some(global_idx) = catalog.resolve_global_gen(schedule.resource_id.as_str()) else {
                return Err(ScedError::InvalidInput(format!(
                    "storage_self_schedules references unknown resource {}",
                    schedule.resource_id
                )));
            };
            let generator = &network.generators[global_idx];
            if !generator.is_storage() {
                return Err(ScedError::InvalidInput(format!(
                    "storage_self_schedules resource {} is not a storage unit",
                    schedule.resource_id
                )));
            }
            if storage_self_schedules
                .insert(global_idx, schedule.values_mw.clone())
                .is_some()
            {
                return Err(ScedError::InvalidInput(format!(
                    "storage_self_schedules contains duplicate resource {}",
                    schedule.resource_id
                )));
            }
        }
        input.storage_self_schedules = Some(storage_self_schedules);
    }

    for impact in &request.market.storage_reserve_soc_impacts {
        let Some(global_idx) = catalog.resolve_global_gen(impact.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "storage_reserve_soc_impacts references unknown resource {}",
                impact.resource_id
            )));
        };
        let generator = &network.generators[global_idx];
        if !generator.is_storage() {
            return Err(ScedError::InvalidInput(format!(
                "storage_reserve_soc_impacts resource {} is not a storage unit",
                impact.resource_id
            )));
        }
        let by_product = input
            .storage_reserve_soc_impact
            .entry(global_idx)
            .or_default();
        if by_product
            .insert(impact.product_id.clone(), impact.values_mwh_per_mw.clone())
            .is_some()
        {
            return Err(ScedError::InvalidInput(format!(
                "storage_reserve_soc_impacts contains duplicate ({}, {})",
                impact.resource_id, impact.product_id
            )));
        }
    }

    for schedule in &request.market.generator_offer_schedules {
        let Some(global_idx) = catalog.resolve_global_gen(schedule.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "generator_offer_schedules references unknown resource {}",
                schedule.resource_id
            )));
        };
        if input
            .offer_schedules
            .insert(global_idx, schedule.schedule.clone())
            .is_some()
        {
            return Err(ScedError::InvalidInput(format!(
                "generator_offer_schedules contains duplicate resource {}",
                schedule.resource_id
            )));
        }
    }

    for schedule in &request.market.dispatchable_load_offer_schedules {
        let Some(dl_idx) = catalog.resolve_dispatchable_load(
            schedule.resource_id.as_str(),
            request.market.dispatchable_loads.len(),
        ) else {
            return Err(ScedError::InvalidInput(format!(
                "dispatchable_load_offer_schedules references unknown resource {}",
                schedule.resource_id
            )));
        };
        if input
            .dl_offer_schedules
            .insert(dl_idx, schedule.schedule.clone())
            .is_some()
        {
            return Err(ScedError::InvalidInput(format!(
                "dispatchable_load_offer_schedules contains duplicate resource {}",
                schedule.resource_id
            )));
        }
    }

    for schedule in &request.market.generator_reserve_offer_schedules {
        let Some(global_idx) = catalog.resolve_global_gen(schedule.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "generator_reserve_offer_schedules references unknown resource {}",
                schedule.resource_id
            )));
        };
        if input
            .gen_reserve_offer_schedules
            .insert(global_idx, schedule.schedule.clone())
            .is_some()
        {
            return Err(ScedError::InvalidInput(format!(
                "generator_reserve_offer_schedules contains duplicate resource {}",
                schedule.resource_id
            )));
        }
    }

    for schedule in &request.market.dispatchable_load_reserve_offer_schedules {
        let Some(dl_idx) = catalog.resolve_dispatchable_load(
            schedule.resource_id.as_str(),
            request.market.dispatchable_loads.len(),
        ) else {
            return Err(ScedError::InvalidInput(format!(
                "dispatchable_load_reserve_offer_schedules references unknown resource {}",
                schedule.resource_id
            )));
        };
        if input
            .dl_reserve_offer_schedules
            .insert(dl_idx, schedule.schedule.clone())
            .is_some()
        {
            return Err(ScedError::InvalidInput(format!(
                "dispatchable_load_reserve_offer_schedules contains duplicate resource {}",
                schedule.resource_id
            )));
        }
    }

    if !request.market.combined_cycle_offer_schedules.is_empty() {
        let mut cc_config_offers = network
            .market_data
            .combined_cycle_plants
            .iter()
            .map(|plant| {
                plant
                    .configs
                    .iter()
                    .map(|_| surge_network::market::OfferSchedule::empty(request.timeline.periods))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut seen_schedule_targets = HashSet::new();
        for schedule in &request.market.combined_cycle_offer_schedules {
            let Some(plant_idx) = catalog.resolve_combined_cycle_plant(
                schedule.plant_id.as_str(),
                network.market_data.combined_cycle_plants.len(),
            ) else {
                return Err(ScedError::InvalidInput(format!(
                    "combined_cycle_offer_schedules references unknown plant_id {}",
                    schedule.plant_id
                )));
            };
            let plant = &network.market_data.combined_cycle_plants[plant_idx];
            let Some(config_idx) = plant
                .configs
                .iter()
                .position(|config| config.name == schedule.config_name)
                .or_else(|| {
                    resolve_combined_cycle_config(
                        schedule.config_name.as_str(),
                        plant.configs.len(),
                    )
                })
            else {
                return Err(ScedError::InvalidInput(format!(
                    "combined_cycle_offer_schedules references unknown configuration {} for plant {}",
                    schedule.config_name, schedule.plant_id
                )));
            };
            if !seen_schedule_targets.insert((plant_idx, config_idx)) {
                return Err(ScedError::InvalidInput(format!(
                    "combined_cycle_offer_schedules contains duplicate ({}, {})",
                    schedule.plant_id, schedule.config_name
                )));
            }
            cc_config_offers[plant_idx][config_idx] = schedule.schedule.clone();
        }
        input.cc_config_offers = cc_config_offers;
    }

    let mut generator_area = catalog
        .in_service_gen_indices
        .iter()
        .map(|&global_idx| {
            let bus_number = network.generators[global_idx].bus;
            let bus_idx = catalog.bus_index_map.get(&bus_number).copied().unwrap_or(0);
            network
                .buses
                .get(bus_idx)
                .map(|bus| bus.area as usize)
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    let mut seen_resource_areas = HashSet::new();
    for assignment in &request.market.resource_area_assignments {
        let Some(local_idx) = catalog.resolve_local_gen(assignment.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "resource_area_assignments references unknown supply resource {}",
                assignment.resource_id
            )));
        };
        if !seen_resource_areas.insert(local_idx) {
            return Err(ScedError::InvalidInput(format!(
                "resource_area_assignments contains duplicate resource {}",
                assignment.resource_id
            )));
        }
        generator_area[local_idx] = assignment.area_id.as_usize();
    }
    input.generator_area = generator_area;

    let mut load_area = network
        .buses
        .iter()
        .map(|bus| bus.area as usize)
        .collect::<Vec<_>>();
    let mut seen_bus_areas = HashSet::new();
    for assignment in &request.market.bus_area_assignments {
        let Some(bus_idx) = catalog.resolve_bus(assignment.bus_number, network.buses.len()) else {
            return Err(ScedError::InvalidInput(format!(
                "bus_area_assignments references unknown bus {}",
                assignment.bus_number
            )));
        };
        if !seen_bus_areas.insert(bus_idx) {
            return Err(ScedError::InvalidInput(format!(
                "bus_area_assignments contains duplicate bus {}",
                assignment.bus_number
            )));
        }
        load_area[bus_idx] = assignment.area_id.as_usize();
    }
    input.load_area = load_area;

    if let Some(units) = &request.market.must_run_units
        && !units.resource_ids.is_empty()
    {
        let mut unit_indices = Vec::with_capacity(units.resource_ids.len());
        let mut seen_unit_indices = HashSet::new();
        for resource_id in &units.resource_ids {
            let Some(local_idx) = catalog.resolve_local_gen(resource_id.as_str()) else {
                return Err(ScedError::InvalidInput(format!(
                    "must_run_units references unknown resource {}",
                    resource_id
                )));
            };
            if !seen_unit_indices.insert(local_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "must_run_units contains duplicate resource {}",
                    resource_id
                )));
            }
            unit_indices.push(local_idx);
        }
        input.must_run_units = Some(IndexedMustRunUnits { unit_indices });
    }

    if !request.market.regulation_eligibility.is_empty() {
        let mut regulation_eligible = vec![true; catalog.n_in_service_generators()];
        let mut seen_eligibility = HashSet::new();
        for eligibility in &request.market.regulation_eligibility {
            let Some(local_idx) = catalog.resolve_local_gen(eligibility.resource_id.as_str())
            else {
                return Err(ScedError::InvalidInput(format!(
                    "regulation_eligibility references unknown resource {}",
                    eligibility.resource_id
                )));
            };
            if !seen_eligibility.insert(local_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "regulation_eligibility contains duplicate resource {}",
                    eligibility.resource_id
                )));
            }
            regulation_eligible[local_idx] = eligibility.eligible;
        }
        input.regulation_eligible = Some(regulation_eligible);
    }

    for constraint in &request.market.commitment_constraints {
        let mut terms = Vec::with_capacity(constraint.terms.len());
        let mut seen_terms = HashSet::new();
        for term in &constraint.terms {
            let Some(local_idx) = catalog.resolve_local_gen(term.resource_id.as_str()) else {
                return Err(ScedError::InvalidInput(format!(
                    "commitment constraint {} references unknown resource {}",
                    constraint.name, term.resource_id
                )));
            };
            if !seen_terms.insert(local_idx) {
                return Err(ScedError::InvalidInput(format!(
                    "commitment constraint {} contains duplicate resource {}",
                    constraint.name, term.resource_id
                )));
            }
            terms.push(IndexedCommitmentTerm {
                gen_index: local_idx,
                coeff: term.coeff,
            });
        }
        input
            .commitment_constraints
            .push(IndexedCommitmentConstraint {
                name: constraint.name.clone(),
                period_idx: constraint.period_idx,
                terms,
                lower_bound: constraint.lower_bound,
                penalty_cost: constraint.penalty_cost,
            });
    }

    for limit in &request.market.startup_window_limits {
        let Some(local_idx) = catalog.resolve_local_gen(limit.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "startup_window_limits references unknown resource {}",
                limit.resource_id
            )));
        };
        input.startup_window_limits.push(IndexedStartupWindowLimit {
            gen_index: local_idx,
            start_period_idx: limit.start_period_idx,
            end_period_idx: limit.end_period_idx,
            max_startups: limit.max_startups,
        });
    }

    for limit in &request.market.energy_window_limits {
        let Some(local_idx) = catalog.resolve_local_gen(limit.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "energy_window_limits references unknown resource {}",
                limit.resource_id
            )));
        };
        input.energy_window_limits.push(IndexedEnergyWindowLimit {
            gen_index: local_idx,
            start_period_idx: limit.start_period_idx,
            end_period_idx: limit.end_period_idx,
            min_energy_mwh: limit.min_energy_mwh,
            max_energy_mwh: limit.max_energy_mwh,
        });
    }

    let mut seen_ph_head_curves = HashSet::new();
    for curve in &request.network.ph_head_curves {
        let Some(global_idx) = catalog.resolve_global_gen(curve.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "ph_head_curves references unknown resource {}",
                curve.resource_id
            )));
        };
        if !seen_ph_head_curves.insert(global_idx) {
            return Err(ScedError::InvalidInput(format!(
                "ph_head_curves contains duplicate resource {}",
                curve.resource_id
            )));
        }
        input.ph_head_curves.push(IndexedPhHeadCurve {
            gen_index: global_idx,
            breakpoints: curve.breakpoints.clone(),
        });
    }

    let mut seen_ph_mode_constraints = HashSet::new();
    for constraint in &request.network.ph_mode_constraints {
        let Some(global_idx) = catalog.resolve_global_gen(constraint.resource_id.as_str()) else {
            return Err(ScedError::InvalidInput(format!(
                "ph_mode_constraints references unknown resource {}",
                constraint.resource_id
            )));
        };
        if !seen_ph_mode_constraints.insert(global_idx) {
            return Err(ScedError::InvalidInput(format!(
                "ph_mode_constraints contains duplicate resource {}",
                constraint.resource_id
            )));
        }
        input.ph_mode_constraints.push(IndexedPhModeConstraint {
            gen_index: global_idx,
            min_gen_run_periods: constraint.min_gen_run_periods,
            min_pump_run_periods: constraint.min_pump_run_periods,
            pump_to_gen_periods: constraint.pump_to_gen_periods,
            gen_to_pump_periods: constraint.gen_to_pump_periods,
            max_pump_starts: constraint.max_pump_starts,
        });
    }

    Ok(input)
}
