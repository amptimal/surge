// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC metadata translation for objective, bounds, and row builders.

use std::collections::{HashMap, HashSet};

use surge_network::Network;

use super::bounds::{
    ScucBoundsCcPlant, ScucBoundsDrActivation, ScucBoundsFozGroup, ScucBoundsPhMode,
};
use super::layout::{ScucDrActivationInfo, ScucFozGenInfo, ScucPhHeadInfo, ScucPhModeInfo};
use super::objective::ScucObjectiveCcPlant;
use super::plan::ScucCcPlantInfo;
use super::rows::{
    ScucCcConfig, ScucCcMemberGen, ScucCcPgccEntry, ScucCcPlant, ScucCcTransitionDelay,
    ScucFozGroup, ScucHvdcRampVar, ScucPhHeadUnit, ScucPhModeUnit, ScucStartupTierInfo,
    ScucUnitIntertemporalGen,
};
use crate::common::spec::DispatchProblemSpec;
use crate::request::RampMode;

fn avg_ramp_from_curve(curve: &[(f64, f64)]) -> Option<f64> {
    if curve.is_empty() {
        return None;
    }
    Some(curve.iter().map(|&(_, rate)| rate).sum::<f64>() / curve.len() as f64)
}

pub(super) struct ScucRowMetadata<'a> {
    pub foz_groups: Vec<ScucFozGroup<'a>>,
    pub ph_mode_units: Vec<ScucPhModeUnit>,
    pub ph_head_units: Vec<ScucPhHeadUnit<'a>>,
    pub unit_intertemporal_gens: Vec<ScucUnitIntertemporalGen<'a>>,
    pub hvdc_ramp_vars: Vec<ScucHvdcRampVar>,
    pub cc_row_plants: Vec<ScucCcPlant>,
}

pub(super) struct ScucRowMetadataInput<'spec, 'input, 'helper> {
    pub network: &'input Network,
    pub spec: &'spec DispatchProblemSpec<'spec>,
    pub gen_indices: &'input [usize],
    pub delta_gen_off: &'input [usize],
    pub gen_tier_info_by_hour: &'input [Vec<Vec<ScucStartupTierInfo>>],
    pub pre_horizon_offline_hours: &'input [Option<f64>],
    pub prev_dispatch_mw: Option<&'spec [f64]>,
    pub prev_dispatch_mask: Option<&'spec [bool]>,
    pub cc_member_gen_set: &'helper HashSet<usize>,
    pub foz_gens: &'input [ScucFozGenInfo],
    pub ph_mode_infos: &'input [ScucPhModeInfo],
    pub ph_head_infos: &'input [ScucPhHeadInfo],
    pub cc_infos: &'input [ScucCcPlantInfo],
    pub cc_var_base: usize,
    pub hvdc_off: usize,
    pub hvdc_band_offsets: &'input [usize],
    pub n_hours: usize,
    pub has_hvdc: bool,
    pub base: f64,
    pub step_h: f64,
}

pub(super) fn build_row_metadata<'spec, 'input, 'helper>(
    input: ScucRowMetadataInput<'spec, 'input, 'helper>,
) -> ScucRowMetadata<'input> {
    let foz_groups = input
        .foz_gens
        .iter()
        .map(|group| ScucFozGroup {
            gen_idx: group.gen_idx,
            segments: &group.segments,
            zones: &group.zones,
            max_transit: &group.max_transit,
            delta_local_off: group.delta_local_off,
            phi_local_off: group.phi_local_off,
            rho_local_off: group.rho_local_off,
            pmax_pu: input.network.generators[input.gen_indices[group.gen_idx]].pmax / input.base,
        })
        .collect();
    let ph_mode_units = input
        .ph_mode_infos
        .iter()
        .map(|info| ScucPhModeUnit {
            storage_idx: info.storage_idx,
            dis_max_mw: info.dis_max_mw,
            ch_max_mw: info.ch_max_mw,
            min_gen_run: info.min_gen_run,
            min_pump_run: info.min_pump_run,
            p2g_delay: info.p2g_delay,
            g2p_delay: info.g2p_delay,
            max_pump_starts: info.max_pump_starts,
            m_gen_local_off: info.m_gen_local_off,
            m_pump_local_off: info.m_pump_local_off,
        })
        .collect();
    let ph_head_units = input
        .ph_head_infos
        .iter()
        .map(|info| ScucPhHeadUnit {
            storage_idx: info.storage_idx,
            breakpoints: &info.breakpoints,
        })
        .collect();
    let unit_intertemporal_gens = input
        .gen_indices
        .iter()
        .enumerate()
        .map(|(gen_idx, &network_gen_idx)| {
            let generator = &input.network.generators[network_gen_idx];
            let mid = (generator.pmin + generator.pmax) / 2.0;
            let ramp_up_limit_pu_by_hour = (0..input.n_hours)
                .map(|hour| {
                    let ramp_up_mw = match input.spec.ramp_mode {
                        RampMode::Averaged | RampMode::Block { .. } => {
                            generator.ramp_up_avg_mw_per_min().unwrap_or(f64::MAX)
                        }
                        RampMode::Interpolated => generator.ramp_up_at_mw(mid).unwrap_or(f64::MAX),
                    } * 60.0
                        * input.spec.period_hours(hour);
                    let limit_pu = ramp_up_mw / input.base;
                    (limit_pu < 1e10).then_some(limit_pu)
                })
                .collect();
            let ramp_down_limit_pu_by_hour = (0..input.n_hours)
                .map(|hour| {
                    let ramp_down_mw = match input.spec.ramp_mode {
                        RampMode::Averaged | RampMode::Block { .. } => {
                            generator.ramp_down_avg_mw_per_min().unwrap_or(f64::MAX)
                        }
                        RampMode::Interpolated => {
                            generator.ramp_down_at_mw(mid).unwrap_or(f64::MAX)
                        }
                    } * 60.0
                        * input.spec.period_hours(hour);
                    let limit_pu = ramp_down_mw / input.base;
                    (limit_pu < 1e10).then_some(limit_pu)
                })
                .collect();
            let startup_ramp_limit_pu_by_hour = (0..input.n_hours)
                .map(|hour| {
                    let limit_pu = generator
                        .startup_ramp_mw_per_period(input.spec.period_hours(hour))
                        / input.base;
                    (limit_pu < 1e10).then_some(limit_pu)
                })
                .collect();
            let shutdown_ramp_limit_pu_by_hour = (0..input.n_hours)
                .map(|hour| {
                    let limit_pu = generator
                        .shutdown_ramp_mw_per_period(input.spec.period_hours(hour))
                        / input.base;
                    (limit_pu < 1e10).then_some(limit_pu)
                })
                .collect();
            let initial_commitment = input.spec.initial_commitment_at(gen_idx);

            let min_up_periods_by_hour = if input.cc_member_gen_set.contains(&gen_idx) {
                vec![0; input.n_hours]
            } else {
                let min_up_time_hr = generator
                    .commitment
                    .as_ref()
                    .and_then(|c| c.min_up_time_hr)
                    .unwrap_or(1.0);
                (0..input.n_hours)
                    .map(|hour| {
                        input
                            .spec
                            .hours_to_periods_ceil_from_uncapped(hour, min_up_time_hr)
                    })
                    .collect()
            };
            let min_down_periods_by_hour = if input.cc_member_gen_set.contains(&gen_idx) {
                vec![0; input.n_hours]
            } else {
                let min_down_time_hr = generator
                    .commitment
                    .as_ref()
                    .and_then(|c| c.min_down_time_hr)
                    .unwrap_or(1.0);
                (0..input.n_hours)
                    .map(|hour| {
                        input
                            .spec
                            .hours_to_periods_ceil_from_uncapped(hour, min_down_time_hr)
                    })
                    .collect()
            };
            let forced_offline_hours = (0..input.n_hours)
                .map(|hour| {
                    input
                        .spec
                        .gen_derate_profiles
                        .profiles
                        .iter()
                        .any(|profile| {
                            profile.generator_id == generator.id
                                && hour < profile.derate_factors.len()
                                && profile.derate_factors[hour] == 0.0
                        })
                })
                .collect();

            ScucUnitIntertemporalGen {
                gen_idx,
                min_up_periods_by_hour,
                min_down_periods_by_hour,
                forced_offline_hours,
                startup_delta_local_off: input.delta_gen_off[gen_idx],
                use_deloading_limits: input.spec.enforce_shutdown_deloading
                    && !generator.is_storage(),
                startup_tiers_by_hour: &input.gen_tier_info_by_hour[gen_idx],
                pre_horizon_offline_hours: input.pre_horizon_offline_hours[gen_idx],
                elapsed_horizon_hours_before_by_hour: (0..input.n_hours)
                    .map(|hour| input.spec.hours_between(0, hour))
                    .collect(),
                ramp_up_limit_pu_by_hour,
                ramp_down_limit_pu_by_hour,
                startup_ramp_limit_pu_by_hour,
                shutdown_ramp_limit_pu_by_hour,
                pmax_pu: generator.pmax / input.base,
                pmin_pu: generator.pmin / input.base,
                initial_commitment,
                initial_dispatch_pu: input
                    .prev_dispatch_mw
                    .and_then(|prev| {
                        if let Some(mask) = input.prev_dispatch_mask
                            && !mask.get(gen_idx).copied().unwrap_or(false)
                        {
                            return None;
                        }
                        prev.get(gen_idx).copied()
                    })
                    .map(|prev| prev / input.base)
                    .or_else(|| (initial_commitment == Some(false)).then_some(0.0)),
            }
        })
        .collect();
    let hvdc_ramp_vars = if input.n_hours > 1 && input.has_hvdc {
        input
            .spec
            .hvdc_links
            .iter()
            .enumerate()
            .flat_map(|(link_idx, hvdc)| {
                if hvdc.is_banded() {
                    hvdc.bands
                        .iter()
                        .enumerate()
                        .filter_map(move |(band_idx, band)| {
                            let ramp_mw_per_min = if band.ramp_mw_per_min > 0.0 {
                                band.ramp_mw_per_min
                            } else {
                                hvdc.ramp_mw_per_min
                            };
                            (ramp_mw_per_min > 0.0).then_some(ScucHvdcRampVar {
                                col_local: input.hvdc_off
                                    + input.hvdc_band_offsets[link_idx]
                                    + band_idx,
                                ramp_limit_pu: ramp_mw_per_min * 60.0 * input.step_h / input.base,
                            })
                        })
                        .collect::<Vec<_>>()
                } else if hvdc.ramp_mw_per_min > 0.0 {
                    vec![ScucHvdcRampVar {
                        col_local: input.hvdc_off + input.hvdc_band_offsets[link_idx],
                        ramp_limit_pu: hvdc.ramp_mw_per_min * 60.0 * input.step_h / input.base,
                    }]
                } else {
                    Vec::new()
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    let gen_j_lookup: HashMap<usize, usize> = input
        .gen_indices
        .iter()
        .enumerate()
        .map(|(gen_idx, &network_gen_idx)| (network_gen_idx, gen_idx))
        .collect();
    let cc_row_plants = input
        .cc_infos
        .iter()
        .enumerate()
        .map(|(plant_idx, info)| {
            let plant = &input.network.market_data.combined_cycle_plants[plant_idx];
            let initial_active_config = plant.active_config.as_deref().and_then(|active| {
                plant
                    .configs
                    .iter()
                    .position(|config| config.name == active)
            });

            let mut member_gens: Vec<ScucCcMemberGen> = info
                .member_gen_j
                .iter()
                .copied()
                .map(|gen_idx| ScucCcMemberGen {
                    gen_idx,
                    config_indices: plant
                        .configs
                        .iter()
                        .enumerate()
                        .filter_map(|(config_idx, config)| {
                            config
                                .gen_indices
                                .contains(&input.gen_indices[gen_idx])
                                .then_some(config_idx)
                        })
                        .collect(),
                    pgcc_entry_indices: info
                        .pgcc_entries
                        .iter()
                        .enumerate()
                        .filter_map(|(entry_idx, &(entry_gen_idx, _))| {
                            (entry_gen_idx == gen_idx).then_some(entry_idx)
                        })
                        .collect(),
                })
                .collect();
            member_gens.sort_unstable_by_key(|member| member.gen_idx);

            let configs = plant
                .configs
                .iter()
                .map(|config| {
                    let member_gen_j: Vec<usize> = config
                        .gen_indices
                        .iter()
                        .filter_map(|gi| gen_j_lookup.get(gi).copied())
                        .collect();
                    let big_m_pu = member_gen_j
                        .iter()
                        .map(|&gen_idx| {
                            input.network.generators[input.gen_indices[gen_idx]].pmax / input.base
                        })
                        .sum();
                    ScucCcConfig {
                        member_gen_j,
                        min_up_periods: (config.min_up_time_hr / input.spec.dt_hours).ceil()
                            as usize,
                        min_down_periods: (config.min_down_time_hr / input.spec.dt_hours).ceil()
                            as usize,
                        p_min_pu: config.p_min_mw / input.base,
                        p_max_pu: config.p_max_mw / input.base,
                        big_m_pu,
                        ramp_up_pu: avg_ramp_from_curve(&config.ramp_up_curve)
                            .map(|rate| rate * 60.0 * input.spec.dt_hours / input.base),
                        ramp_down_pu: avg_ramp_from_curve(&config.ramp_down_curve)
                            .map(|rate| rate * 60.0 * input.spec.dt_hours / input.base),
                    }
                })
                .collect();

            let mut disallowed_transitions = Vec::new();
            for from_config in 0..info.n_configs {
                for to_config in 0..info.n_configs {
                    if !info
                        .allowed_transitions
                        .contains_key(&(from_config, to_config))
                    {
                        disallowed_transitions.push((from_config, to_config));
                    }
                }
            }

            let delayed_transitions = info
                .transition_pairs
                .iter()
                .filter_map(|&(from_config, to_config)| {
                    let delay_periods = info
                        .allowed_transitions
                        .get(&(from_config, to_config))
                        .map(|&(_cost, time_min)| {
                            (time_min / (60.0 * input.spec.dt_hours)).ceil() as usize
                        })
                        .unwrap_or(0);
                    (delay_periods > 0).then_some(ScucCcTransitionDelay {
                        from_config,
                        to_config,
                        delay_periods,
                    })
                })
                .collect();

            let pgcc_entries = info
                .pgcc_entries
                .iter()
                .map(|&(gen_idx, config_idx)| ScucCcPgccEntry {
                    config_idx,
                    pmax_pu: input.network.generators[input.gen_indices[gen_idx]].pmax / input.base,
                })
                .collect();

            ScucCcPlant {
                n_configs: info.n_configs,
                z_block_base: input.cc_var_base + info.z_block_off,
                ytrans_block_base: input.cc_var_base + info.ytrans_block_off,
                pgcc_block_base: input.cc_var_base + info.pgcc_block_off,
                initial_active_config,
                member_gens,
                configs,
                disallowed_transitions,
                delayed_transitions,
                transition_pairs: info.transition_pairs.clone(),
                pgcc_entries,
            }
        })
        .collect();

    ScucRowMetadata {
        foz_groups,
        ph_mode_units,
        ph_head_units,
        unit_intertemporal_gens,
        hvdc_ramp_vars,
        cc_row_plants,
    }
}

pub(super) fn build_objective_cc_plants(cc_infos: &[ScucCcPlantInfo]) -> Vec<ScucObjectiveCcPlant> {
    cc_infos
        .iter()
        .enumerate()
        .map(|(plant_index, info)| ScucObjectiveCcPlant {
            plant_index,
            z_block_off: info.z_block_off,
            ytrans_block_off: info.ytrans_block_off,
            pgcc_block_off: info.pgcc_block_off,
            member_gen_j: info.member_gen_j.clone(),
            transition_costs: info
                .transition_pairs
                .iter()
                .map(|pair| {
                    info.allowed_transitions
                        .get(pair)
                        .map(|(cost, _)| *cost)
                        .unwrap_or(0.0)
                })
                .collect(),
            pgcc_entries: info.pgcc_entries.clone(),
        })
        .collect()
}

pub(super) struct ScucBoundMetadata {
    pub foz_bound_groups: Vec<ScucBoundsFozGroup>,
    pub ph_mode_bound_infos: Vec<ScucBoundsPhMode>,
    pub cc_bound_plants: Vec<ScucBoundsCcPlant>,
    pub cc_member_gen_mask: Vec<bool>,
    pub dl_activation_bound_infos: Vec<ScucBoundsDrActivation>,
}

pub(super) struct ScucBoundMetadataInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub n_gen: usize,
    pub cc_member_gen_set: &'a HashSet<usize>,
    pub foz_gens: &'a [ScucFozGenInfo],
    pub ph_mode_infos: &'a [ScucPhModeInfo],
    pub cc_infos: &'a [ScucCcPlantInfo],
    pub dl_activation_infos: &'a [ScucDrActivationInfo],
}

pub(super) fn build_bound_metadata(input: ScucBoundMetadataInput<'_>) -> ScucBoundMetadata {
    let foz_bound_groups = input
        .foz_gens
        .iter()
        .map(|group| ScucBoundsFozGroup {
            delta_local_off: group.delta_local_off,
            phi_local_off: group.phi_local_off,
            rho_local_off: group.rho_local_off,
            n_segments: group.segments.len(),
            max_transit: group.max_transit.clone(),
        })
        .collect();
    let ph_mode_bound_infos = input
        .ph_mode_infos
        .iter()
        .map(|info| ScucBoundsPhMode {
            m_gen_local_off: info.m_gen_local_off,
            m_pump_local_off: info.m_pump_local_off,
        })
        .collect();
    let cc_bound_plants = input
        .cc_infos
        .iter()
        .enumerate()
        .map(|(plant_index, info)| {
            let plant = &input.network.market_data.combined_cycle_plants[plant_index];
            let initial_active_config = plant.active_config.as_ref().and_then(|active_name| {
                plant
                    .configs
                    .iter()
                    .position(|cfg| cfg.name == *active_name)
            });
            let initial_config_force_periods = initial_active_config
                .map(|config_idx| {
                    let remaining_hours =
                        (plant.configs[config_idx].min_up_time_hr - plant.hours_in_config).max(0.0);
                    (remaining_hours / input.spec.dt_hours).ceil() as usize
                })
                .unwrap_or(0);

            ScucBoundsCcPlant {
                n_configs: info.n_configs,
                z_block_off: info.z_block_off,
                ytrans_block_off: info.ytrans_block_off,
                pgcc_block_off: info.pgcc_block_off,
                n_transition_pairs: info.transition_pairs.len(),
                pgcc_gen_j: info
                    .pgcc_entries
                    .iter()
                    .map(|&(gen_idx, _)| gen_idx)
                    .collect(),
                initial_active_config,
                initial_config_force_periods,
            }
        })
        .collect();
    let cc_member_gen_mask = (0..input.n_gen)
        .map(|gen_idx| input.cc_member_gen_set.contains(&gen_idx))
        .collect();
    let dl_activation_bound_infos = input
        .dl_activation_infos
        .iter()
        .map(|info| ScucBoundsDrActivation {
            n_notify: info.notification_periods,
        })
        .collect();

    ScucBoundMetadata {
        foz_bound_groups,
        ph_mode_bound_infos,
        cc_bound_plants,
        cc_member_gen_mask,
        dl_activation_bound_infos,
    }
}
