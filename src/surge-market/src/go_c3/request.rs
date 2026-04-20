// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 dispatch-request orchestrator.
//!
//! This module is the top-level builder that wires together the
//! GO-C3-specific sub-modules
//! (reserves / dispatchable_loads / commitment / hvdc / zones /
//! penalty / bus_profiles) and the canonical market helpers
//! (offers / profiles / commitment / trajectory) to produce a fully-
//! typed [`DispatchRequest`].
//!
//! The high-level flow is:
//!
//! 1. Pre-aggregate consumer bus profiles (`bus_profiles`).
//! 2. Build reserve products and zonal requirements (`reserves`).
//! 3. Walk the device list, emitting for each producer: offer
//!    schedule, dispatch bounds, reserve offers (+ per-period
//!    `q_headroom` when present), derate profile, commitment metadata
//!    (startup/energy windows, min-commit, initial conditions,
//!    previous dispatch). Zero-MW producers get a stubbed schedule.
//! 4. For each consumer: dispatchable-load block decomposition
//!    (`dispatchable_loads`).
//! 5. HVDC links + previous dispatch + DC-line reactive synthetic
//!    generators (AC formulation only).
//! 6. Assemble the [`DispatchRequest`] through the canonical builder.

use std::collections::{HashMap, HashSet};

use surge_dispatch::request::{
    GeneratorDerateProfile, GeneratorDerateProfiles, GeneratorDispatchBoundsProfile,
};
use surge_dispatch::{
    AcBusLoadProfile, AcBusLoadProfiles, BranchRef, CommitmentOptions, CommitmentPolicy,
    CommitmentSchedule, CommitmentTrajectoryMode, ConstraintEnforcement, DispatchInitialState,
    DispatchMarket, DispatchNetwork, DispatchProfiles, DispatchRequest, DispatchRuntime,
    DispatchState, DispatchTimeline, Formulation, GeneratorOfferSchedule,
    GeneratorReserveOfferSchedule, HvdcLinkRef, IntervalCoupling, ResourceCommitmentSchedule,
    ResourceDispatchPoint, ResourcePeriodCommitment, SecurityEmbedding, SecurityPolicy,
    TopologyControlMode,
};
use surge_io::go_c3::types::{GoC3Bus, GoC3DeviceType};
use surge_io::go_c3::{
    BranchRef as GoC3BranchRef, GoC3CommitmentMode, GoC3ConsumerMode, GoC3Context, GoC3Device,
    GoC3DeviceTimeSeries, GoC3Formulation, GoC3Policy, GoC3Problem,
};
use surge_network::market::OfferSchedule;
use surge_opf::AcOpfOptions;

use crate::offers::build_offer_curve;
use crate::profiles::build_generator_dispatch_bounds_profiles;

use super::GoC3DispatchError;
use super::bus_profiles::aggregate_consumer_bus_profiles;
use super::commitment::{
    energy_window_limits, generator_derate_factor, initial_condition_for_device,
    is_zero_mw_producer, minimum_commitment, startup_window_limits,
};
use super::dispatchable_loads::build_consumer_pieces;
use super::hvdc::build_hvdc_request_pieces;
use super::presets::goc3_penalty_config;
use super::reserves::{build_generator_reserve_offer_schedules, build_reserves};
use super::zones::build_zone_assignments;

const GEN_BLOCK_TIEBREAK_EPSILON: f64 = 0.0;

/// Entry point: build a [`DispatchRequest`] from a GO C3 problem +
/// adapter context.
pub fn build_dispatch_request(
    problem: &GoC3Problem,
    context: &mut GoC3Context,
    policy: &GoC3Policy,
) -> Result<DispatchRequest, GoC3DispatchError> {
    let base_mva = problem.network.general.base_norm_mva;
    let periods = problem.time_series_input.general.time_periods;
    let formulation = match policy.formulation {
        GoC3Formulation::Dc => Formulation::Dc,
        GoC3Formulation::Ac => Formulation::Ac,
    };

    let device_ts_by_uid: HashMap<&str, &GoC3DeviceTimeSeries> = problem
        .time_series_input
        .simple_dispatchable_device
        .iter()
        .map(|ts| (ts.uid.as_str(), ts))
        .collect();
    let _devices_by_uid: HashMap<&str, &GoC3Device> = problem
        .network
        .simple_dispatchable_device
        .iter()
        .map(|d| (d.uid.as_str(), d))
        .collect();

    // Consumer fixed-bus aggregation for DC-path load profiles and
    // AC-path `ac_bus_load` profiles. Persist the per-device fixed
    // series on the context so the exporter can recover consumer
    // baselines when no dispatchable-load served power is available.
    let bus_profile_data = aggregate_consumer_bus_profiles(problem, context, policy);
    for (uid, series) in &bus_profile_data.fixed_p_series_pu_by_uid {
        context
            .device_fixed_p_series_pu
            .insert(uid.clone(), series.clone());
    }

    // Reserves.
    let (reserve_products, zonal_reserve_requirements) = build_reserves(
        problem,
        context,
        base_mva,
        periods,
        policy.scuc_reserve_penalty_multiplier,
    );
    let q_headroom_active = context
        .reserve_product_ids
        .iter()
        .any(|id| id == "q_headroom");

    // Timeline.
    let timeline = build_timeline(problem);

    // Producer device loop.
    let mut generator_offer_schedules: Vec<GeneratorOfferSchedule> = Vec::new();
    let mut generator_reserve_offer_schedules: Vec<GeneratorReserveOfferSchedule> = Vec::new();
    let mut generator_dispatch_bounds: Vec<GeneratorDispatchBoundsProfile> = Vec::new();
    let mut generator_derate_profiles: Vec<GeneratorDerateProfile> = Vec::new();
    let mut startup_window_limits_all = Vec::new();
    let mut energy_window_limits_all = Vec::new();
    let mut initial_conditions = Vec::new();
    let mut previous_resource_dispatch: Vec<ResourceDispatchPoint> = Vec::new();
    let mut minimum_commitment_list: Vec<ResourcePeriodCommitment> = Vec::new();
    let mut commitment_resources: Vec<ResourceCommitmentSchedule> = Vec::new();

    // Precompute the base reserve offer table for producers (needed
    // when the builder emits per-period schedules). The canonical
    // catalog in `super::reserves` handles this; we rely on it for the
    // standard offer schedule and augment per-period with `q_headroom`
    // below.
    let producer_reserve_offer_schedules = build_generator_reserve_offer_schedules(
        problem,
        context,
        &device_ts_by_uid,
        base_mva,
        periods,
    );
    let producer_reserve_schedule_by_uid: HashMap<String, GeneratorReserveOfferSchedule> =
        producer_reserve_offer_schedules
            .into_iter()
            .map(|s| (s.resource_id.clone(), s))
            .collect();

    let commitment_mode = policy.commitment_mode;
    for device in &problem.network.simple_dispatchable_device {
        if device.device_type != GoC3DeviceType::Producer {
            continue;
        }
        let Some(ts) = device_ts_by_uid.get(device.uid.as_str()).copied() else {
            continue;
        };

        if is_zero_mw_producer(ts) {
            emit_zero_mw_producer(
                device,
                ts,
                periods,
                base_mva,
                &mut generator_offer_schedules,
                &mut generator_dispatch_bounds,
            );
            continue;
        }

        let (offer_sched, dispatch_bounds, derate_profile, reserve_sched) = build_producer_device(
            device,
            ts,
            problem,
            periods,
            base_mva,
            &producer_reserve_schedule_by_uid,
            q_headroom_active,
        );
        generator_offer_schedules.push(offer_sched);
        generator_dispatch_bounds.push(dispatch_bounds);
        if let Some(d) = derate_profile {
            generator_derate_profiles.push(d);
        }
        if let Some(rs) = reserve_sched {
            generator_reserve_offer_schedules.push(rs);
        }

        startup_window_limits_all.extend(startup_window_limits(problem, device));
        energy_window_limits_all.extend(energy_window_limits(problem, ts, device));
        initial_conditions.push(initial_condition_for_device(problem, device, ts));
        previous_resource_dispatch.push(ResourceDispatchPoint {
            resource_id: device.uid.clone(),
            mw: device.initial_status.p * base_mva,
        });
        if let Some(m) = minimum_commitment(device, ts) {
            minimum_commitment_list.push(m);
        }
        if matches!(commitment_mode, GoC3CommitmentMode::FixedInitial) {
            let initial_on = device.initial_status.on_status != 0;
            commitment_resources.push(ResourceCommitmentSchedule {
                resource_id: device.uid.clone(),
                initial: initial_on,
                periods: Some(vec![initial_on; periods]),
            });
        }
    }

    // Consumer device loop: dispatchable-load block decomposition.
    // This mutates `context.consumer_dispatchable_resource_ids_by_uid`
    // so the export path can later route block-level outputs back to
    // the parent consumer UID.
    let consumer_pieces = if matches!(policy.consumer_mode, GoC3ConsumerMode::Dispatchable) {
        build_consumer_pieces(problem, context, &device_ts_by_uid)
    } else {
        super::dispatchable_loads::ConsumerPieces {
            loads: Vec::new(),
            offer_schedules: Vec::new(),
            reserve_offer_schedules: Vec::new(),
        }
    };

    // HVDC pieces + DC reactive synthetic gens.
    let hvdc_pieces = build_hvdc_request_pieces(problem, context, policy);

    // Fold HVDC synthetic resources into the producer arrays.
    for sch in hvdc_pieces.synthetic_offer_schedules {
        generator_offer_schedules.push(sch);
    }
    for db in hvdc_pieces.synthetic_dispatch_bounds {
        generator_dispatch_bounds.push(db);
    }
    for prev in hvdc_pieces.synthetic_previous_dispatch {
        previous_resource_dispatch.push(prev);
    }
    for commit in hvdc_pieces.synthetic_commitment_schedules {
        commitment_resources.push(commit);
    }

    // Reserve zone assignments (resource_area / bus_area).
    let zone_assignments = build_zone_assignments(problem);
    let resource_area_assignments: Vec<surge_dispatch::request::ResourceAreaAssignment> = problem
        .network
        .simple_dispatchable_device
        .iter()
        .filter(|d| d.device_type == GoC3DeviceType::Producer)
        .filter_map(|device| {
            zone_assignments
                .primary
                .device_zone_uids
                .get(&device.uid)
                .and_then(|zones| {
                    if zones.len() == 1 {
                        zone_assignments
                            .primary
                            .zone_uid_to_area_id
                            .get(&zones[0])
                            .copied()
                    } else {
                        None
                    }
                })
                .map(|area_id| surge_dispatch::request::ResourceAreaAssignment {
                    resource_id: device.uid.clone(),
                    area_id: surge_dispatch::AreaId(area_id as u32),
                })
        })
        .collect();
    let mut bus_entries: Vec<(u32, &String)> = zone_assignments
        .primary
        .bus_zone_uids
        .iter()
        .filter_map(|(bus_uid, zone_uids)| {
            if zone_uids.len() == 1 {
                context
                    .bus_uid_to_number
                    .get(bus_uid)
                    .copied()
                    .map(|num| (num, &zone_uids[0]))
            } else {
                None
            }
        })
        .collect();
    bus_entries.sort_by_key(|&(num, _)| num);
    let bus_area_assignments: Vec<surge_dispatch::request::BusAreaAssignment> = bus_entries
        .into_iter()
        .filter_map(|(bus_number, zone_uid)| {
            zone_assignments
                .primary
                .zone_uid_to_area_id
                .get(zone_uid)
                .copied()
                .map(|area_id| surge_dispatch::request::BusAreaAssignment {
                    bus_number,
                    area_id: surge_dispatch::AreaId(area_id as u32),
                })
        })
        .collect();

    // ── Profiles ─────────────────────────────────────────────────────────
    let mut profiles = DispatchProfiles::default();
    let mut load_profiles: Vec<surge_dispatch::BusLoadProfile> = Vec::new();
    let mut ac_bus_profiles: Vec<AcBusLoadProfile> = Vec::new();
    if matches!(formulation, Formulation::Dc) {
        let mut entries: Vec<(u32, Vec<f64>)> = bus_profile_data.p_by_bus.into_iter().collect();
        entries.sort_by_key(|&(b, _)| b);
        for (bus_number, values_mw) in entries {
            if values_mw.iter().any(|v| v.abs() > 1e-12) {
                load_profiles.push(surge_dispatch::BusLoadProfile {
                    bus_number,
                    values_mw,
                });
            }
        }
    } else {
        let mut entries: Vec<u32> = bus_profile_data.p_by_bus.keys().copied().collect();
        entries.sort();
        for bus_number in entries {
            let p = bus_profile_data
                .p_by_bus
                .get(&bus_number)
                .cloned()
                .unwrap_or_default();
            let q = bus_profile_data
                .q_by_bus
                .get(&bus_number)
                .cloned()
                .unwrap_or_default();
            let has_p = p.iter().any(|v| v.abs() > 1e-12);
            let has_q = q.iter().any(|v| v.abs() > 1e-12);
            if has_p || has_q {
                ac_bus_profiles.push(AcBusLoadProfile {
                    bus_number,
                    p_mw: if has_p { Some(p) } else { None },
                    q_mvar: if has_q { Some(q) } else { None },
                });
            }
        }
    }
    profiles.load = surge_dispatch::BusLoadProfiles {
        profiles: load_profiles,
    };
    profiles.ac_bus_load = AcBusLoadProfiles {
        profiles: ac_bus_profiles,
    };
    profiles.generator_dispatch_bounds =
        build_generator_dispatch_bounds_profiles(generator_dispatch_bounds);
    if !generator_derate_profiles.is_empty() {
        profiles.generator_derates = GeneratorDerateProfiles {
            profiles: generator_derate_profiles,
        };
    }

    // ── Market ───────────────────────────────────────────────────────────
    let market = DispatchMarket {
        reserve_products,
        zonal_reserve_requirements,
        generator_offer_schedules,
        generator_reserve_offer_schedules,
        dispatchable_loads: consumer_pieces.loads,
        dispatchable_load_offer_schedules: consumer_pieces.offer_schedules,
        dispatchable_load_reserve_offer_schedules: consumer_pieces.reserve_offer_schedules,
        resource_area_assignments,
        bus_area_assignments,
        startup_window_limits: startup_window_limits_all,
        energy_window_limits: energy_window_limits_all,
        penalty_config: goc3_penalty_config(problem, policy.scuc_thermal_penalty_multiplier),
        ..DispatchMarket::default()
    };

    // ── Network ──────────────────────────────────────────────────────────
    let mut network = DispatchNetwork::default();
    // Security screening is DC-only — the canonical AC SCED stage
    // runs without contingency constraints and lets the outer
    // workflow re-invoke SCUC if security feedback is needed.
    if matches!(formulation, Formulation::Dc) {
        if let Some(security) = build_security_screening(problem, context, policy) {
            network.security = Some(security);
        }
    }
    network.thermal_limits.enforce = true;
    network.ramping.enforcement = ConstraintEnforcement::Hard;
    if let Some(vio) = &problem.network.violation_cost {
        network.energy_windows.penalty_per_puh = vio.e_vio_cost / base_mva.max(1.0);
    }
    network.commitment_transitions.trajectory_mode = CommitmentTrajectoryMode::OfflineTrajectory;
    network.commitment_transitions.shutdown_deloading = true;
    network.topology_control.mode = if policy.allow_branch_switching {
        TopologyControlMode::Switchable
    } else {
        TopologyControlMode::Fixed
    };
    network.hvdc_links = hvdc_pieces.links;
    // Enable LP-level flowgate enforcement by default. When the
    // scenario provides contingencies via `reliability.contingency`,
    // the SCUC explicit-security path converts them into compact
    // `ContingencyCut` entries that the LP then enforces as
    // post-contingency flow constraints. SCUC detects an empty
    // contingency list + `enforce_flowgates=true` and short-circuits
    // the flowgate row family so scenarios without contingencies pay
    // no extra LP weight. The policy's `disable_flowgates` diagnostic
    // flag turns enforcement off entirely — drops both normal flowgates
    // and the explicit N-1 cuts so the MIP runs without the security
    // overhead. For production GO C3 solves, leave `disable_flowgates`
    // at its default (false).
    network.flowgates.enabled = !policy.disable_flowgates;
    // GO C3 ramp-feasible SCUC requires marginal loss iteration so
    // the DC dispatch accounts for line losses rather than forcing
    // the AC reconcile to absorb them as slack. See `markets/go_c3/
    // adapter.py:4217` for the $10M-penalty calibration motivation.
    network.loss_factors.enabled = true;
    network.loss_factors.max_iterations = policy.scuc_loss_factor_max_iterations.unwrap_or(1);
    network.loss_factors.tolerance = 1.0e-3;
    // Cold-start loss-factor warm-start mode. Off by default; caller
    // opts in via `GoC3Policy::scuc_loss_factor_warm_start`.
    //
    // * `Some(("uniform", rate))` → `LossFactorWarmStartMode::Uniform { rate }`
    // * `Some(("load_pattern", rate))` → `LossFactorWarmStartMode::LoadPattern { rate }`
    // * `Some(("dc_pf", _))` → `LossFactorWarmStartMode::DcPf`
    // * `None` or unrecognised mode → falls through to `Disabled`.
    network.loss_factors.warm_start_mode = match policy.scuc_loss_factor_warm_start.as_ref() {
        Some((mode, rate)) => match mode.as_str() {
            "uniform" => {
                surge_dispatch::request::network::LossFactorWarmStartMode::Uniform { rate: *rate }
            }
            "load_pattern" => {
                surge_dispatch::request::network::LossFactorWarmStartMode::LoadPattern {
                    rate: *rate,
                }
            }
            "dc_pf" => surge_dispatch::request::network::LossFactorWarmStartMode::DcPf,
            _ => surge_dispatch::request::network::LossFactorWarmStartMode::Disabled,
        },
        None => surge_dispatch::request::network::LossFactorWarmStartMode::Disabled,
    };

    // ── Commitment ───────────────────────────────────────────────────────
    let commitment = match commitment_mode {
        GoC3CommitmentMode::FixedInitial => CommitmentPolicy::Fixed(CommitmentSchedule {
            resources: commitment_resources,
        }),
        GoC3CommitmentMode::AllCommitted => CommitmentPolicy::AllCommitted,
        GoC3CommitmentMode::Optimize => {
            let mut ics = initial_conditions.clone();
            ics.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
            let options = CommitmentOptions {
                initial_conditions: ics,
                warm_start_commitment: Vec::new(),
                time_limit_secs: policy.commitment_time_limit_secs,
                mip_rel_gap: policy.commitment_mip_rel_gap,
                mip_gap_schedule: policy.commitment_mip_gap_schedule.clone(),
                disable_warm_start: policy.disable_scuc_warm_start,
            };
            if !minimum_commitment_list.is_empty() {
                CommitmentPolicy::Additional {
                    minimum_commitment: minimum_commitment_list,
                    options,
                }
            } else {
                CommitmentPolicy::Optimize(options)
            }
        }
    };

    // ── Runtime ──────────────────────────────────────────────────────────
    // LP repricing re-solve to recover LMP duals. Defaults off in
    // GO C3 scoring runs since LMPs aren't consumed downstream and the
    // re-solve adds ~15-25s/617-bus SCUC.
    let mut runtime = DispatchRuntime {
        run_pricing: policy.run_pricing,
        ..DispatchRuntime::default()
    };
    if matches!(formulation, Formulation::Ac) {
        if let Some(vio) = &problem.network.violation_cost {
            let base = base_mva.max(1.0);
            let ac_opf = AcOpfOptions {
                bus_active_power_balance_slack_penalty_per_mw: vio.p_bus_vio_cost / base,
                bus_reactive_power_balance_slack_penalty_per_mvar: vio.q_bus_vio_cost / base,
                thermal_limit_slack_penalty_per_mva: (vio.s_vio_cost / base)
                    * policy.sced_thermal_penalty_multiplier,
                ..AcOpfOptions::default()
            };
            runtime.ac_opf = Some(ac_opf);
        }
    }

    // ── State ────────────────────────────────────────────────────────────
    let state = DispatchState {
        initial: DispatchInitialState {
            previous_resource_dispatch,
            previous_hvdc_dispatch: hvdc_pieces.previous_dispatch,
            storage_soc_overrides: Vec::new(),
        },
    };

    // AC dispatch requires period-by-period coupling (the AC kernel
    // validates this invariant). DC defaults to time-coupled.
    let coupling = match formulation {
        Formulation::Ac => IntervalCoupling::PeriodByPeriod,
        Formulation::Dc => IntervalCoupling::TimeCoupled,
    };

    Ok(DispatchRequest::builder()
        .formulation(formulation)
        .coupling(coupling)
        .timeline(timeline)
        .market(market)
        .profiles(profiles)
        .state(state)
        .network(network)
        .commitment(commitment)
        .runtime(runtime)
        .build())
}

fn build_timeline(problem: &GoC3Problem) -> DispatchTimeline {
    let general = &problem.time_series_input.general;
    let intervals = &general.interval_duration;
    if intervals.is_empty() {
        return DispatchTimeline {
            periods: general.time_periods,
            interval_hours: 1.0,
            interval_hours_by_period: Vec::new(),
        };
    }
    let uniform = intervals.iter().all(|d| (d - intervals[0]).abs() <= 1e-9);
    if uniform {
        DispatchTimeline {
            periods: general.time_periods,
            interval_hours: intervals[0],
            interval_hours_by_period: Vec::new(),
        }
    } else {
        let sum: f64 = intervals.iter().sum();
        DispatchTimeline {
            periods: general.time_periods,
            interval_hours: sum / intervals.len() as f64,
            interval_hours_by_period: intervals.clone(),
        }
    }
}

fn emit_zero_mw_producer(
    device: &GoC3Device,
    device_ts: &GoC3DeviceTimeSeries,
    periods: usize,
    base_mva: f64,
    offer_schedules: &mut Vec<GeneratorOfferSchedule>,
    dispatch_bounds: &mut Vec<GeneratorDispatchBoundsProfile>,
) {
    let q_lb: Vec<f64> = device_ts
        .q_lb
        .iter()
        .take(periods)
        .map(|v| v * base_mva)
        .chain(std::iter::repeat(0.0))
        .take(periods)
        .collect();
    let q_ub: Vec<f64> = device_ts
        .q_ub
        .iter()
        .take(periods)
        .map(|v| v * base_mva)
        .chain(std::iter::repeat(0.0))
        .take(periods)
        .collect();
    let periods_iter: Vec<Option<surge_network::market::OfferCurve>> = (0..periods)
        .map(|_| {
            Some(surge_network::market::OfferCurve {
                segments: vec![(0.0, 0.0)],
                no_load_cost: 0.0,
                startup_tiers: Vec::new(),
            })
        })
        .collect();
    offer_schedules.push(GeneratorOfferSchedule {
        resource_id: device.uid.clone(),
        schedule: OfferSchedule {
            periods: periods_iter,
        },
    });
    let q_min: Vec<f64> = q_lb
        .iter()
        .zip(q_ub.iter())
        .map(|(a, b)| a.min(*b))
        .collect();
    let q_max: Vec<f64> = q_lb
        .iter()
        .zip(q_ub.iter())
        .map(|(a, b)| a.max(*b))
        .collect();
    dispatch_bounds.push(GeneratorDispatchBoundsProfile {
        resource_id: device.uid.clone(),
        p_min_mw: vec![0.0; periods],
        p_max_mw: vec![0.0; periods],
        q_min_mvar: Some(q_min),
        q_max_mvar: Some(q_max),
    });
}

fn build_producer_device(
    device: &GoC3Device,
    ts: &GoC3DeviceTimeSeries,
    problem: &GoC3Problem,
    periods: usize,
    base_mva: f64,
    producer_reserve_schedule_by_uid: &HashMap<String, GeneratorReserveOfferSchedule>,
    q_headroom_active: bool,
) -> (
    GeneratorOfferSchedule,
    GeneratorDispatchBoundsProfile,
    Option<GeneratorDerateProfile>,
    Option<GeneratorReserveOfferSchedule>,
) {
    // Startup tiers: GO layout is [(cost_adjustment, max_offline_hours)] pairs.
    let startup_pairs: Vec<(f64, f64)> = device
        .startup_states
        .iter()
        .filter_map(|pair| {
            if pair.len() < 2 {
                None
            } else {
                Some((pair[1], pair[0]))
            }
        })
        .collect();
    let startup_tiers =
        crate::commitment::startup_tiers_from_piecewise(device.startup_cost, &startup_pairs);

    let mut p_min = Vec::with_capacity(periods);
    let mut p_max = Vec::with_capacity(periods);
    let mut q_min = Vec::with_capacity(periods);
    let mut q_max = Vec::with_capacity(periods);
    let mut period_offers: Vec<Option<surge_network::market::OfferCurve>> =
        Vec::with_capacity(periods);
    let mut derate_factors: Vec<f64> = Vec::with_capacity(periods);
    let on_status_ub_raw: &[f64] = ts.on_status_ub.as_slice();
    let on_status_ub = |idx: usize| -> i32 {
        if on_status_ub_raw.is_empty() {
            1
        } else if idx < on_status_ub_raw.len() {
            on_status_ub_raw[idx] as i32
        } else {
            1
        }
    };
    let pmax_pu = ts.p_ub.iter().copied().fold(0.0_f64, f64::max);

    for i in 0..periods {
        let p_lb_pu = ts.p_lb.get(i).copied().unwrap_or(0.0);
        let p_ub_pu = ts.p_ub.get(i).copied().unwrap_or(0.0);
        let lower_mw = p_lb_pu * base_mva;
        let upper_mw = (p_ub_pu * base_mva).max(lower_mw);
        p_min.push(lower_mw);
        p_max.push(upper_mw);
        let q_lb_mvar = ts.q_lb.get(i).copied().unwrap_or(0.0) * base_mva;
        let q_ub_mvar = ts.q_ub.get(i).copied().unwrap_or(0.0) * base_mva;
        let q_upper = q_lb_mvar.max(q_ub_mvar);
        q_min.push(q_lb_mvar.min(q_upper));
        q_max.push(q_upper);

        let blocks = ts.cost.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut offer = build_offer_curve(blocks, device.on_cost, startup_tiers.clone(), base_mva);
        if GEN_BLOCK_TIEBREAK_EPSILON > 0.0 {
            // Tiebreak epsilon is dormant (0.0) on the canonical GO
            // C3 path but preserved here for future perturbation work.
            for (idx, seg) in offer.segments.iter_mut().enumerate() {
                seg.1 += GEN_BLOCK_TIEBREAK_EPSILON * idx as f64;
            }
        }
        period_offers.push(Some(offer));
        let ub = on_status_ub(i);
        derate_factors.push(generator_derate_factor(p_ub_pu, pmax_pu, ub));
    }
    let offer_sched = GeneratorOfferSchedule {
        resource_id: device.uid.clone(),
        schedule: OfferSchedule {
            periods: period_offers,
        },
    };
    let dispatch_bounds = GeneratorDispatchBoundsProfile {
        resource_id: device.uid.clone(),
        p_min_mw: p_min,
        p_max_mw: p_max,
        q_min_mvar: Some(q_min.clone()),
        q_max_mvar: Some(q_max.clone()),
    };
    let derate_profile = if derate_factors.iter().any(|&f| f < 1.0 - 1e-9) {
        Some(GeneratorDerateProfile {
            resource_id: device.uid.clone(),
            derate_factors,
        })
    } else {
        None
    };

    // Reserve offer schedule — augment with per-period q_headroom when
    // reactive zonal requirements are active.
    let mut reserve_sched = producer_reserve_schedule_by_uid.get(&device.uid).cloned();
    if q_headroom_active {
        let headroom_periods: Vec<(f64, usize)> = (0..periods)
            .map(|i| {
                let q_range = (q_max[i] - q_min[i]).max(0.0);
                (q_range, i)
            })
            .collect();
        let has_any = headroom_periods.iter().any(|(v, _)| *v > 1e-9);
        if has_any {
            let mut target = reserve_sched.unwrap_or_else(|| GeneratorReserveOfferSchedule {
                resource_id: device.uid.clone(),
                schedule: surge_dispatch::ReserveOfferSchedule {
                    periods: vec![Vec::new(); periods],
                },
            });
            if target.schedule.periods.len() < periods {
                target.schedule.periods.resize(periods, Vec::new());
            }
            for (i, (q_range, _)) in headroom_periods.iter().enumerate() {
                if *q_range > 1e-9 {
                    target.schedule.periods[i].push(surge_network::market::ReserveOffer {
                        product_id: "q_headroom".to_string(),
                        capacity_mw: *q_range,
                        cost_per_mwh: 0.0,
                    });
                }
            }
            reserve_sched = Some(target);
        }
    }

    let _ = problem; // kept for signature-stability; future metadata moves here
    (offer_sched, dispatch_bounds, derate_profile, reserve_sched)
}

fn build_security_screening(
    problem: &GoC3Problem,
    context: &GoC3Context,
    policy: &GoC3Policy,
) -> Option<SecurityPolicy> {
    if problem.reliability.contingency.is_empty() {
        return None;
    }
    let dc_line_uids: HashSet<&str> = problem
        .network
        .dc_line
        .iter()
        .map(|d| d.uid.as_str())
        .collect();

    let mut branch_contingencies = Vec::new();
    let mut hvdc_contingencies = Vec::new();

    for contingency in &problem.reliability.contingency {
        for component in &contingency.components {
            if let Some(ref_) = context.branch_uid_to_ref.get(component.as_str()) {
                branch_contingencies.push(go_c3_branch_ref_to_dispatch(ref_));
            } else if dc_line_uids.contains(component.as_str()) {
                hvdc_contingencies.push(HvdcLinkRef {
                    link_id: component.clone(),
                });
            }
        }
    }
    if branch_contingencies.is_empty() && hvdc_contingencies.is_empty() {
        return None;
    }
    Some(SecurityPolicy {
        // Iterative (on-demand) cut addition. The SCUC is solved
        // without any contingency cuts on the first pass, then the
        // solution is screened against every (contingency ×
        // monitored-branch × period) triple. Only pairs whose
        // post-contingency flow exceeds `violation_tolerance_pu`
        // become cuts; the SCUC is re-solved with the augmented
        // flowgate set. Converges in 2–6 iterations on typical
        // N-1 scenarios, with total cut count bounded by the
        // actually-binding constraints (tens to low thousands) rather
        // than the full (contingencies × monitored × periods)
        // expansion. This replaces the ExplicitContingencies path
        // whose upfront LP growth scaled to millions of rows on
        // 562-contingency workloads (617-bus D1).
        embedding: SecurityEmbedding::IterativeScreening,
        // Policy-controlled outer iteration cap. Default 5 is enough for
        // typical N-1 convergence on GO-C3 cases (73-bus D1/345
        // converges in 3, 617-bus D1/002 in 3); headroom above ~5
        // usually means the cut sequence is cycling rather than
        // converging, which is worth warning on rather than silently
        // absorbing. Bump up (or down) via policy if a case
        // legitimately needs it.
        max_iterations: policy.scuc_security_max_iterations,
        violation_tolerance_pu: 0.01,
        // Policy-controlled per-iteration cut cap. Default 10_000 is
        // matched to the preseed budget so post-solve screening can
        // absorb a full structural wave of binding pairs in a single
        // iteration. At this scale we trade iterative parsimony for
        // wall-time convergence: the MIP carries a larger constant-row
        // budget but iterates fewer times, which on 73-bus D1 cuts
        // total SCUC wall by ~50% on contingency-active scenarios.
        max_cuts_per_iteration: policy.scuc_security_max_cuts_per_iteration,
        branch_contingencies,
        hvdc_contingencies,
        preseed_count_per_period: policy.scuc_security_preseed_count_per_period,
        preseed_method: if policy.scuc_security_preseed_count_per_period > 0 {
            surge_dispatch::SecurityPreseedMethod::MaxLodfTopology
        } else {
            surge_dispatch::SecurityPreseedMethod::None
        },
    })
}

fn go_c3_branch_ref_to_dispatch(r: &GoC3BranchRef) -> BranchRef {
    BranchRef {
        from_bus: r.from_bus,
        to_bus: r.to_bus,
        circuit: r.circuit.clone(),
    }
}

// Unused helpers to satisfy imports (kept under `allow(dead_code)` for
// future expansion).
#[allow(dead_code)]
fn unused_bus_marker(_buses: &[GoC3Bus]) {}
