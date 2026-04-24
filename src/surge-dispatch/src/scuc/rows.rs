// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared SCUC row-family builders.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::market::{
    DispatchableLoad, SystemReserveRequirement, ZonalReserveRequirement, qualifications_can_overlap,
};
use surge_network::network::Generator;
use surge_sparse::Triplet;

use super::layout::ScucLayout;
use crate::common::builders;
use crate::common::costs::resolve_dl_for_period_from_spec;
use crate::common::layout::LpBlock;
use crate::common::network::study_area_for_bus_index;
use crate::common::reserves::{
    ReserveLpLayout, dispatchable_load_reserve_offer_for_period, generator_reserve_offer_for_period,
};
use crate::common::setup::DispatchSetup;
use crate::common::spec::DispatchProblemSpec;

const BIG_M: f64 = 1e30;

#[inline]
fn dl_energy_coupling(
    product: &surge_network::market::ReserveProduct,
) -> surge_network::market::EnergyCoupling {
    product
        .dispatchable_load_energy_coupling
        .unwrap_or(product.energy_coupling)
}

/// Whether any active reserve product uses `OfflineQuickStart` qualification.
fn has_offline_reserve_products(layout: &ReserveLpLayout) -> bool {
    layout.products.iter().any(|ap| {
        matches!(
            ap.product.qualification,
            surge_network::market::QualificationRule::OfflineQuickStart
        )
    })
}

pub(super) fn capacity_logic_reserve_rows_per_hour(
    n_gen: usize,
    reserve_layout: &ReserveLpLayout,
) -> usize {
    // One offline headroom row per generator when any
    // OfflineQuickStart reserve product is active.
    let n_offline_headroom = if has_offline_reserve_products(reserve_layout) {
        n_gen
    } else {
        0
    };
    4 * n_gen + reserve_layout.n_reserve_rows + n_offline_headroom
}

pub(super) struct ScucCapacityLogicReserveRowsInput<'a> {
    pub network: &'a Network,
    pub hourly_network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub reserve_layout: &'a ReserveLpLayout,
    pub r_sys_reqs: &'a [SystemReserveRequirement],
    pub r_zonal_reqs: &'a [ZonalReserveRequirement],
    pub gen_indices: &'a [usize],
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub layout: &'a ScucLayout,
    pub hour: usize,
    pub n_hours: usize,
    pub row_base: usize,
    pub base: f64,
}

fn dispatchable_load_pmax_pu(
    input: &ScucCapacityLogicReserveRowsInput<'_>,
    k: usize,
    dl: &DispatchableLoad,
) -> f64 {
    let dl_idx = input.dl_orig_idx.get(k).copied().unwrap_or(k);
    let (_, p_max_pu, _, _, _, _) =
        resolve_dl_for_period_from_spec(dl_idx, input.hour, dl, input.spec);
    p_max_pu
}

fn push_triplet(triplets: &mut Vec<Triplet<f64>>, row: usize, col: usize, val: f64) {
    triplets.push(Triplet { row, col, val });
}

struct CommitmentCouplingCols {
    row: usize,
    pg_col: usize,
    reserve_col: Option<usize>,
    commitment_col: usize,
    startup_col: usize,
    shutdown_next_col: Option<usize>,
    headroom_slack_col: Option<usize>,
    footroom_slack_col: Option<usize>,
}

struct CommitmentCouplingInput<'a> {
    cols: CommitmentCouplingCols,
    layout: &'a ScucLayout,
    generator: &'a Generator,
    spec: &'a DispatchProblemSpec<'a>,
    hour: usize,
    n_hours: usize,
    gen_idx: usize,
    startup_dt_hours: f64,
    shutdown_dt_hours: f64,
    initial_dispatch_mw: Option<f64>,
    base: f64,
    limit_pu: f64,
    enforce_shutdown_deloading: bool,
    offline_commitment_trajectories: bool,
}

fn finite_rate_mw_per_hour(value: f64) -> Option<f64> {
    (value.is_finite() && value < 1e10).then_some(value)
}

fn effective_startup_output_cap_pu(input: &CommitmentCouplingInput<'_>) -> f64 {
    let startup_cap_pu = input
        .generator
        .startup_ramp_mw_per_period(input.startup_dt_hours)
        / input.base;
    let pmin_pu = input.generator.pmin / input.base;
    if input.hour > 0 && pmin_pu > startup_cap_pu + 1e-12 {
        pmin_pu
    } else {
        startup_cap_pu
    }
}

fn add_commitment_trajectory_terms(
    triplets: &mut Vec<Triplet<f64>>,
    input: &CommitmentCouplingInput<'_>,
    coeff_sign: f64,
) {
    if let Some(startup_rate_mw_per_hour) =
        finite_rate_mw_per_hour(input.generator.startup_ramp_mw_per_period(1.0))
    {
        for startup_hour in input.hour + 1..input.n_hours {
            let trajectory_mw = input.generator.pmin
                - startup_rate_mw_per_hour
                    * (input.spec.period_end_hours(startup_hour)
                        - input.spec.period_end_hours(input.hour));
            if trajectory_mw > 1e-9 {
                push_triplet(
                    triplets,
                    input.cols.row,
                    input.layout.startup_col(startup_hour, input.gen_idx),
                    coeff_sign * (trajectory_mw / input.base),
                );
            }
        }
    }

    if let Some(shutdown_rate_mw_per_hour) =
        finite_rate_mw_per_hour(input.generator.shutdown_ramp_mw_per_period(1.0))
    {
        let initial_shutdown_anchor_mw = input.initial_dispatch_mw.unwrap_or(input.generator.pmin);
        for shutdown_hour in 0..=input.hour {
            let shutdown_anchor_mw = if shutdown_hour == 0 {
                initial_shutdown_anchor_mw
            } else {
                input.generator.pmin
            };
            let trajectory_mw = shutdown_anchor_mw
                - shutdown_rate_mw_per_hour
                    * (input.spec.period_end_hours(input.hour)
                        - input.spec.period_start_hours(shutdown_hour));
            if trajectory_mw > 1e-9 {
                push_triplet(
                    triplets,
                    input.cols.row,
                    input.layout.shutdown_col(shutdown_hour, input.gen_idx),
                    coeff_sign * (trajectory_mw / input.base),
                );
            }
        }
    }
}

fn add_headroom_terms(triplets: &mut Vec<Triplet<f64>>, input: CommitmentCouplingInput<'_>) {
    push_triplet(triplets, input.cols.row, input.cols.pg_col, 1.0);
    if let Some(reserve_col) = input.cols.reserve_col {
        push_triplet(triplets, input.cols.row, reserve_col, 1.0);
    }
    push_triplet(
        triplets,
        input.cols.row,
        input.cols.commitment_col,
        -input.limit_pu,
    );

    if let Some(slack_col) = input.cols.headroom_slack_col {
        push_triplet(triplets, input.cols.row, slack_col, -1.0);
    }

    // GO trajectory power counts toward physical output but not toward online
    // p_on headroom, so subtract future startup and past shutdown trajectories.
    add_commitment_trajectory_terms(triplets, &input, -1.0);

    if !input.enforce_shutdown_deloading
        || input.generator.is_storage()
        || input.offline_commitment_trajectories
    {
        return;
    }

    let su_cap_pu = effective_startup_output_cap_pu(&input).min(input.limit_pu);
    let su_delta = input.limit_pu - su_cap_pu;
    if su_delta > 1e-12 {
        push_triplet(triplets, input.cols.row, input.cols.startup_col, su_delta);
    }

    if let Some(shutdown_next_col) = input.cols.shutdown_next_col {
        let sd_cap_pu = (input
            .generator
            .shutdown_ramp_mw_per_period(input.shutdown_dt_hours)
            / input.base)
            .min(input.limit_pu);
        let sd_delta = input.limit_pu - sd_cap_pu;
        if sd_delta > 1e-12 {
            push_triplet(triplets, input.cols.row, shutdown_next_col, sd_delta);
        }
    }
}

fn add_footroom_terms(triplets: &mut Vec<Triplet<f64>>, input: CommitmentCouplingInput<'_>) {
    if let Some(reserve_col) = input.cols.reserve_col {
        push_triplet(triplets, input.cols.row, reserve_col, 1.0);
    }
    push_triplet(triplets, input.cols.row, input.cols.pg_col, -1.0);
    push_triplet(
        triplets,
        input.cols.row,
        input.cols.commitment_col,
        input.limit_pu,
    );

    if let Some(slack_col) = input.cols.footroom_slack_col {
        push_triplet(triplets, input.cols.row, slack_col, -1.0);
    }

    // Online minimum output applies to p_on, not total physical output.
    add_commitment_trajectory_terms(triplets, &input, 1.0);

    if !input.enforce_shutdown_deloading
        || input.generator.is_storage()
        || input.offline_commitment_trajectories
    {
        return;
    }

    let su_cap_pu = effective_startup_output_cap_pu(&input);
    let su_delta = input.limit_pu - su_cap_pu.max(0.0);
    if su_delta > 1e-12 {
        push_triplet(triplets, input.cols.row, input.cols.startup_col, -su_delta);
    }

    if let Some(shutdown_next_col) = input.cols.shutdown_next_col {
        let sd_cap_pu = input
            .generator
            .shutdown_ramp_mw_per_period(input.shutdown_dt_hours)
            / input.base;
        let sd_delta = input.limit_pu - sd_cap_pu.max(0.0);
        if sd_delta > 1e-12 {
            push_triplet(triplets, input.cols.row, shutdown_next_col, -sd_delta);
        }
    }
}

#[allow(clippy::needless_late_init)]
pub(super) fn build_capacity_logic_reserve_rows(
    input: ScucCapacityLogicReserveRowsInput<'_>,
) -> LpBlock {
    let n_gen = input.gen_indices.len();
    let n_rows = capacity_logic_reserve_rows_per_hour(n_gen, input.reserve_layout);
    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };

    // Precompute study_area-per-bus ONCE for all DLs and gens in this
    // period so the inner zonal_participant_bus_matches calls don't
    // rebuild `network.bus_index_map()` per iteration. With 16,000+
    // DLs × multiple zonal requirements this was the dominant cost in
    // capacity_logic row building (~7s wall per period on 4224-bus).
    let bus_index_map: HashMap<u32, usize> = input
        .network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();
    let fallback_area_by_dl: Vec<Option<usize>> = input
        .dl_list
        .iter()
        .map(|dl| {
            bus_index_map
                .get(&dl.bus)
                .copied()
                .and_then(|bus_idx| study_area_for_bus_index(input.network, input.spec, bus_idx))
        })
        .collect();
    let fallback_area_by_gen: Vec<Option<usize>> = input
        .gen_indices
        .iter()
        .enumerate()
        .map(|(j, _gi)| input.spec.generator_area.get(j).copied())
        .collect();
    let fallback_area_by_group: Vec<Option<usize>> = input
        .reserve_layout
        .dl_consumer_groups
        .iter()
        .map(|group| {
            let canonical_k = group.member_dl_indices.first().copied();
            canonical_k
                .and_then(|k| input.dl_list.get(k))
                .and_then(|dl| bus_index_map.get(&dl.bus).copied())
                .and_then(|bus_idx| study_area_for_bus_index(input.network, input.spec, bus_idx))
        })
        .collect();

    // Diagnostic sub-phase timers; only emit for hour 0 to avoid
    // flooding logs. The per-hour work is similar enough that hour 0
    // gives a representative breakdown.
    let trace_timings = input.hour == 0;
    let t_gen_coupling;
    let t_commitment_state;
    let t_cross_headroom;
    let t_cross_footroom;
    let t_dl_cross_headroom;
    let t_dl_cross_footroom;
    let t_shared_gen;
    let t_shared_dl;
    let t_per_product;
    let _t_gc_t0 = std::time::Instant::now();

    let mut local_row = 0usize;

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.hourly_network.generators[gi];
        let row = input.row_base + local_row + j;
        let pmax_pu = generator.pmax / input.base;
        let startup_dt_hours = input.spec.period_hours(input.hour);
        let shutdown_dt_hours = if input.hour + 1 < input.n_hours {
            input.spec.period_hours(input.hour + 1)
        } else {
            startup_dt_hours
        };
        add_headroom_terms(
            &mut block.triplets,
            CommitmentCouplingInput {
                cols: CommitmentCouplingCols {
                    row,
                    pg_col: input.layout.pg_col(input.hour, j),
                    reserve_col: None,
                    commitment_col: input.layout.commitment_col(input.hour, j),
                    startup_col: input.layout.startup_col(input.hour, j),
                    shutdown_next_col: (input.hour + 1 < input.n_hours)
                        .then(|| input.layout.shutdown_col(input.hour + 1, j)),
                    headroom_slack_col: Some(input.layout.headroom_slack_col(input.hour, j)),
                    footroom_slack_col: None,
                },
                layout: input.layout,
                generator,
                spec: input.spec,
                hour: input.hour,
                n_hours: input.n_hours,
                gen_idx: j,
                startup_dt_hours,
                shutdown_dt_hours,
                initial_dispatch_mw: input.spec.prev_dispatch_mw_at(j),
                base: input.base,
                limit_pu: pmax_pu,
                enforce_shutdown_deloading: input.spec.enforce_shutdown_deloading,
                offline_commitment_trajectories: input.spec.offline_commitment_trajectories,
            },
        );
        block.row_lower[local_row + j] = -BIG_M;
        block.row_upper[local_row + j] = 0.0;
    }
    local_row += n_gen;

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.hourly_network.generators[gi];
        let row = input.row_base + local_row + j;
        let pmin_pu = generator.pmin / input.base;
        let startup_dt_hours = input.spec.period_hours(input.hour);
        let shutdown_dt_hours = if input.hour + 1 < input.n_hours {
            input.spec.period_hours(input.hour + 1)
        } else {
            startup_dt_hours
        };
        add_footroom_terms(
            &mut block.triplets,
            CommitmentCouplingInput {
                cols: CommitmentCouplingCols {
                    row,
                    pg_col: input.layout.pg_col(input.hour, j),
                    reserve_col: None,
                    commitment_col: input.layout.commitment_col(input.hour, j),
                    startup_col: input.layout.startup_col(input.hour, j),
                    shutdown_next_col: (input.hour + 1 < input.n_hours)
                        .then(|| input.layout.shutdown_col(input.hour + 1, j)),
                    headroom_slack_col: None,
                    footroom_slack_col: Some(input.layout.footroom_slack_col(input.hour, j)),
                },
                layout: input.layout,
                generator,
                spec: input.spec,
                hour: input.hour,
                n_hours: input.n_hours,
                gen_idx: j,
                startup_dt_hours,
                shutdown_dt_hours,
                initial_dispatch_mw: input.spec.prev_dispatch_mw_at(j),
                base: input.base,
                limit_pu: pmin_pu,
                enforce_shutdown_deloading: input.spec.enforce_shutdown_deloading,
                offline_commitment_trajectories: input.spec.offline_commitment_trajectories,
            },
        );
        block.row_lower[local_row + j] = -BIG_M;
        block.row_upper[local_row + j] = 0.0;
    }
    local_row += n_gen;

    t_gen_coupling = _t_gc_t0.elapsed().as_secs_f64();
    let _t_cs_t0 = std::time::Instant::now();
    for j in 0..n_gen {
        let row = input.row_base + local_row + j;
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.commitment_col(input.hour, j),
            1.0,
        );
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.startup_col(input.hour, j),
            -1.0,
        );
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.shutdown_col(input.hour, j),
            1.0,
        );

        if input.hour > 0 {
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.commitment_col(input.hour - 1, j),
                -1.0,
            );
            block.row_lower[local_row + j] = 0.0;
            block.row_upper[local_row + j] = 0.0;
        } else {
            let u0 = input
                .spec
                .initial_commitment_at(j)
                .map(|initial| if initial { 1.0 } else { 0.0 })
                .unwrap_or(1.0);
            block.row_lower[local_row + j] = u0;
            block.row_upper[local_row + j] = u0;
        }
    }
    local_row += n_gen;

    for j in 0..n_gen {
        let row = input.row_base + local_row + j;
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.startup_col(input.hour, j),
            1.0,
        );
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.shutdown_col(input.hour, j),
            1.0,
        );
        block.row_lower[local_row + j] = -BIG_M;
        block.row_upper[local_row + j] = 1.0;
    }
    local_row += n_gen;

    t_commitment_state = _t_cs_t0.elapsed().as_secs_f64();
    let _t_ch_t0 = std::time::Instant::now();
    if input.reserve_layout.n_cross_headroom_rows > 0 {
        // Phase 4: emit cross-headroom rows only for gens that
        // participate in at least one Headroom product. A
        // non-participant's cross row would collapse to the same
        // commitment-coupling shape already emitted by
        // `build_capacity_logic_reserve_rows` (same terms, no reserve
        // addend), and HiGHS presolve would collapse the duplicates.
        let mut emitted = 0;
        for (j, &gi) in input.gen_indices.iter().enumerate() {
            let participates = input.reserve_layout.products.iter().any(|ap| {
                ap.product.energy_coupling == surge_network::market::EnergyCoupling::Headroom
                    && ap.gen_reserve_col(j).is_some()
            });
            if !participates {
                continue;
            }
            let generator = &input.network.generators[gi];
            let pmax_pu = input.hourly_network.generators[gi].pmax / input.base;
            let row = input.row_base + local_row + emitted;
            let startup_dt_hours = input.spec.period_hours(input.hour);
            let shutdown_dt_hours = if input.hour + 1 < input.n_hours {
                input.spec.period_hours(input.hour + 1)
            } else {
                startup_dt_hours
            };
            add_headroom_terms(
                &mut block.triplets,
                CommitmentCouplingInput {
                    cols: CommitmentCouplingCols {
                        row,
                        pg_col: input.layout.pg_col(input.hour, j),
                        reserve_col: None,
                        commitment_col: input.layout.commitment_col(input.hour, j),
                        startup_col: input.layout.startup_col(input.hour, j),
                        shutdown_next_col: (input.hour + 1 < input.n_hours)
                            .then(|| input.layout.shutdown_col(input.hour + 1, j)),
                        headroom_slack_col: Some(input.layout.headroom_slack_col(input.hour, j)),
                        footroom_slack_col: None,
                    },
                    layout: input.layout,
                    generator,
                    spec: input.spec,
                    hour: input.hour,
                    n_hours: input.n_hours,
                    gen_idx: j,
                    startup_dt_hours,
                    shutdown_dt_hours,
                    initial_dispatch_mw: input.spec.prev_dispatch_mw_at(j),
                    base: input.base,
                    limit_pu: pmax_pu,
                    enforce_shutdown_deloading: input.spec.enforce_shutdown_deloading,
                    offline_commitment_trajectories: input.spec.offline_commitment_trajectories,
                },
            );
            for ap in &input.reserve_layout.products {
                if ap.product.energy_coupling == surge_network::market::EnergyCoupling::Headroom {
                    if let Some(intra) = ap.gen_reserve_col(j) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                }
            }
            block.row_lower[local_row + emitted] = -BIG_M;
            block.row_upper[local_row + emitted] = 0.0;
            emitted += 1;
        }
        debug_assert_eq!(emitted, input.reserve_layout.n_cross_headroom_rows);
        local_row += input.reserve_layout.n_cross_headroom_rows;
    }

    t_cross_headroom = _t_ch_t0.elapsed().as_secs_f64();
    let _t_cf_t0 = std::time::Instant::now();
    if input.reserve_layout.n_cross_footroom_rows > 0 {
        let mut emitted = 0;
        for (j, &gi) in input.gen_indices.iter().enumerate() {
            let participates = input.reserve_layout.products.iter().any(|ap| {
                ap.product.energy_coupling == surge_network::market::EnergyCoupling::Footroom
                    && ap.gen_reserve_col(j).is_some()
            });
            if !participates {
                continue;
            }
            let generator = &input.network.generators[gi];
            let pmin_pu = input.hourly_network.generators[gi].pmin / input.base;
            let row = input.row_base + local_row + emitted;
            let startup_dt_hours = input.spec.period_hours(input.hour);
            let shutdown_dt_hours = if input.hour + 1 < input.n_hours {
                input.spec.period_hours(input.hour + 1)
            } else {
                startup_dt_hours
            };
            add_footroom_terms(
                &mut block.triplets,
                CommitmentCouplingInput {
                    cols: CommitmentCouplingCols {
                        row,
                        pg_col: input.layout.pg_col(input.hour, j),
                        reserve_col: None,
                        commitment_col: input.layout.commitment_col(input.hour, j),
                        startup_col: input.layout.startup_col(input.hour, j),
                        shutdown_next_col: (input.hour + 1 < input.n_hours)
                            .then(|| input.layout.shutdown_col(input.hour + 1, j)),
                        headroom_slack_col: None,
                        footroom_slack_col: Some(input.layout.footroom_slack_col(input.hour, j)),
                    },
                    layout: input.layout,
                    generator,
                    spec: input.spec,
                    hour: input.hour,
                    n_hours: input.n_hours,
                    gen_idx: j,
                    startup_dt_hours,
                    shutdown_dt_hours,
                    initial_dispatch_mw: input.spec.prev_dispatch_mw_at(j),
                    base: input.base,
                    limit_pu: pmin_pu,
                    enforce_shutdown_deloading: input.spec.enforce_shutdown_deloading,
                    offline_commitment_trajectories: input.spec.offline_commitment_trajectories,
                },
            );
            for ap in &input.reserve_layout.products {
                if ap.product.energy_coupling == surge_network::market::EnergyCoupling::Footroom {
                    if let Some(intra) = ap.gen_reserve_col(j) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                }
            }
            block.row_lower[local_row + emitted] = -BIG_M;
            block.row_upper[local_row + emitted] = 0.0;
            emitted += 1;
        }
        debug_assert_eq!(emitted, input.reserve_layout.n_cross_footroom_rows);
        local_row += input.reserve_layout.n_cross_footroom_rows;
    }

    t_cross_footroom = _t_cf_t0.elapsed().as_secs_f64();
    let _t_dch_t0 = std::time::Instant::now();
    let n_dl_groups = input.reserve_layout.dl_consumer_groups.len();
    if input.reserve_layout.n_dl_cross_headroom_rows > 0 {
        let mut emitted = 0;
        for (gi, group) in input.reserve_layout.dl_consumer_groups.iter().enumerate() {
            let participates = input.reserve_layout.products.iter().any(|ap| {
                dl_energy_coupling(&ap.product) == surge_network::market::EnergyCoupling::Headroom
                    && ap.dl_group_reserve_col(gi).is_some()
            });
            if !participates {
                continue;
            }
            let row = input.row_base + local_row + emitted;
            let mut pmax_sum_pu = 0.0;
            for &k in &group.member_dl_indices {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.col(input.hour, input.layout.dispatch.dl + k),
                    1.0,
                );
                pmax_sum_pu += dispatchable_load_pmax_pu(&input, k, input.dl_list[k]);
            }
            for ap in &input.reserve_layout.products {
                if dl_energy_coupling(&ap.product)
                    == surge_network::market::EnergyCoupling::Headroom
                {
                    if let Some(intra) = ap.dl_group_reserve_col(gi) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                }
            }
            block.row_lower[local_row + emitted] = -BIG_M;
            block.row_upper[local_row + emitted] = pmax_sum_pu;
            emitted += 1;
        }
        debug_assert_eq!(emitted, input.reserve_layout.n_dl_cross_headroom_rows);
        local_row += input.reserve_layout.n_dl_cross_headroom_rows;
    }

    t_dl_cross_headroom = _t_dch_t0.elapsed().as_secs_f64();
    let _t_dcf_t0 = std::time::Instant::now();
    if input.reserve_layout.n_dl_cross_footroom_rows > 0 {
        let mut emitted = 0;
        for (gi, group) in input.reserve_layout.dl_consumer_groups.iter().enumerate() {
            let participates = input.reserve_layout.products.iter().any(|ap| {
                dl_energy_coupling(&ap.product) == surge_network::market::EnergyCoupling::Footroom
                    && ap.dl_group_reserve_col(gi).is_some()
            });
            if !participates {
                continue;
            }
            let row = input.row_base + local_row + emitted;
            let mut pmin_sum_pu = 0.0;
            for &k in &group.member_dl_indices {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.col(input.hour, input.layout.dispatch.dl + k),
                    -1.0,
                );
                pmin_sum_pu += input.dl_list[k].p_min_pu;
            }
            for ap in &input.reserve_layout.products {
                if dl_energy_coupling(&ap.product)
                    == surge_network::market::EnergyCoupling::Footroom
                {
                    if let Some(intra) = ap.dl_group_reserve_col(gi) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                }
            }
            block.row_lower[local_row + emitted] = -BIG_M;
            block.row_upper[local_row + emitted] = -pmin_sum_pu;
            emitted += 1;
        }
        debug_assert_eq!(emitted, input.reserve_layout.n_dl_cross_footroom_rows);
        local_row += input.reserve_layout.n_dl_cross_footroom_rows;
    }

    t_dl_cross_footroom = _t_dcf_t0.elapsed().as_secs_f64();
    let _t_sg_t0 = std::time::Instant::now();
    for ap in &input.reserve_layout.products {
        if ap.product.shared_limit_products.is_empty() {
            continue;
        }
        // Precompute the overlapping shared-limit products for `ap`.
        let shared_products_for_ap: Vec<_> = ap
            .product
            .shared_limit_products
            .iter()
            .filter_map(|shared_id| {
                input.reserve_layout.products.iter().find(|candidate| {
                    candidate.product.id == *shared_id
                        && qualifications_can_overlap(
                            &ap.product.qualification,
                            &candidate.product.qualification,
                        )
                })
            })
            .collect();
        let mut emitted = 0;
        for (j, _) in input.gen_indices.iter().enumerate() {
            let contributes = ap.gen_reserve_col(j).is_some()
                || shared_products_for_ap
                    .iter()
                    .any(|sp: &&_| sp.gen_reserve_col(j).is_some());
            if !contributes {
                continue;
            }
            let row = input.row_base + local_row + emitted;
            if let Some(intra) = ap.gen_reserve_col(j) {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.col(input.hour, intra),
                    1.0,
                );
            }
            for sp in &shared_products_for_ap {
                if let Some(intra) = sp.gen_reserve_col(j) {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.col(input.hour, intra),
                        1.0,
                    );
                }
            }
            let offer_cap = generator_reserve_offer_for_period(
                input.spec,
                input.gen_indices[j],
                &input.network.generators[input.gen_indices[j]],
                &ap.product.id,
                input.hour,
            )
            .map(|offer| offer.capacity_mw)
            .unwrap_or(0.0)
                / input.base;
            match ap.product.qualification {
                surge_network::market::QualificationRule::OfflineQuickStart => {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.commitment_col(input.hour, j),
                        offer_cap,
                    );
                    block.row_lower[local_row + emitted] = -BIG_M;
                    block.row_upper[local_row + emitted] = offer_cap;
                }
                surge_network::market::QualificationRule::QuickStart => {
                    block.row_lower[local_row + emitted] = -BIG_M;
                    block.row_upper[local_row + emitted] = offer_cap;
                }
                _ => {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.commitment_col(input.hour, j),
                        -offer_cap,
                    );
                    block.row_lower[local_row + emitted] = -BIG_M;
                    block.row_upper[local_row + emitted] = 0.0;
                }
            }
            emitted += 1;
        }
        local_row += emitted;
    }
    t_shared_gen = _t_sg_t0.elapsed().as_secs_f64();

    let _t_sd_t0 = std::time::Instant::now();
    for ap in &input.reserve_layout.products {
        if ap.product.shared_limit_products.is_empty() {
            continue;
        }
        let shared_products_for_ap: Vec<_> = ap
            .product
            .shared_limit_products
            .iter()
            .filter_map(|shared_id| {
                input.reserve_layout.products.iter().find(|candidate| {
                    candidate.product.id == *shared_id
                        && qualifications_can_overlap(
                            &ap.product.qualification,
                            &candidate.product.qualification,
                        )
                })
            })
            .collect();
        let mut emitted = 0;
        for (gi, group) in input.reserve_layout.dl_consumer_groups.iter().enumerate() {
            let contributes = ap.dl_group_reserve_col(gi).is_some()
                || shared_products_for_ap
                    .iter()
                    .any(|sp: &&_| sp.dl_group_reserve_col(gi).is_some());
            if !contributes {
                continue;
            }
            let row = input.row_base + local_row + emitted;
            if let Some(intra) = ap.dl_group_reserve_col(gi) {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.col(input.hour, intra),
                    1.0,
                );
            }
            for sp in &shared_products_for_ap {
                if let Some(intra) = sp.dl_group_reserve_col(gi) {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.col(input.hour, intra),
                        1.0,
                    );
                }
            }
            let offer_cap: f64 = group
                .member_dl_indices
                .iter()
                .map(|&k| {
                    dispatchable_load_reserve_offer_for_period(
                        input.spec,
                        input.dl_orig_idx.get(k).copied().unwrap_or(k),
                        input.dl_list[k],
                        &ap.product.id,
                        input.hour,
                    )
                    .map(|offer| offer.capacity_mw)
                    .unwrap_or(0.0)
                })
                .sum::<f64>()
                / input.base;
            block.row_lower[local_row + emitted] = -BIG_M;
            block.row_upper[local_row + emitted] = offer_cap;
            emitted += 1;
        }
        local_row += emitted;
    }
    t_shared_dl = _t_sd_t0.elapsed().as_secs_f64();

    let _t_pp_t0 = std::time::Instant::now();
    for ap in &input.reserve_layout.products {
        match ap.product.energy_coupling {
            surge_network::market::EnergyCoupling::Headroom => {
                // Phase 4: only participating gens need a row.
                // Non-participants collapse to commitment coupling
                // already provided by `build_capacity_logic_reserve_rows`.
                for (offset, &j) in ap.gen_participation.iter().enumerate() {
                    let gi = input.gen_indices[j];
                    let generator = &input.network.generators[gi];
                    let pmax_pu = input.hourly_network.generators[gi].pmax / input.base;
                    let row = input.row_base + local_row + offset;
                    if matches!(
                        ap.product.qualification,
                        surge_network::market::QualificationRule::QuickStart
                    ) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.pg_col(input.hour, j),
                            1.0,
                        );
                        if let Some(intra) = ap.gen_reserve_col(j) {
                            push_triplet(
                                &mut block.triplets,
                                row,
                                input.layout.col(input.hour, intra),
                                1.0,
                            );
                        }
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.headroom_slack_col(input.hour, j),
                            -1.0,
                        );
                        block.row_lower[local_row + offset] = -BIG_M;
                        block.row_upper[local_row + offset] = pmax_pu;
                        continue;
                    }
                    let startup_dt_hours = input.spec.period_hours(input.hour);
                    let shutdown_dt_hours = if input.hour + 1 < input.n_hours {
                        input.spec.period_hours(input.hour + 1)
                    } else {
                        startup_dt_hours
                    };
                    add_headroom_terms(
                        &mut block.triplets,
                        CommitmentCouplingInput {
                            cols: CommitmentCouplingCols {
                                row,
                                pg_col: input.layout.pg_col(input.hour, j),
                                reserve_col: ap
                                    .gen_reserve_col(j)
                                    .map(|intra| input.layout.col(input.hour, intra)),
                                commitment_col: input.layout.commitment_col(input.hour, j),
                                startup_col: input.layout.startup_col(input.hour, j),
                                shutdown_next_col: (input.hour + 1 < input.n_hours)
                                    .then(|| input.layout.shutdown_col(input.hour + 1, j)),
                                headroom_slack_col: Some(
                                    input.layout.headroom_slack_col(input.hour, j),
                                ),
                                footroom_slack_col: None,
                            },
                            layout: input.layout,
                            generator,
                            spec: input.spec,
                            hour: input.hour,
                            n_hours: input.n_hours,
                            gen_idx: j,
                            startup_dt_hours,
                            shutdown_dt_hours,
                            initial_dispatch_mw: input.spec.prev_dispatch_mw_at(j),
                            base: input.base,
                            limit_pu: pmax_pu,
                            enforce_shutdown_deloading: input.spec.enforce_shutdown_deloading,
                            offline_commitment_trajectories: input
                                .spec
                                .offline_commitment_trajectories,
                        },
                    );
                    block.row_lower[local_row + offset] = -BIG_M;
                    block.row_upper[local_row + offset] = 0.0;
                }
                local_row += ap.gen_participation.len();
            }
            surge_network::market::EnergyCoupling::Footroom => {
                for (offset, &j) in ap.gen_participation.iter().enumerate() {
                    let gi = input.gen_indices[j];
                    let generator = &input.network.generators[gi];
                    let pmin_pu = input.hourly_network.generators[gi].pmin / input.base;
                    let row = input.row_base + local_row + offset;
                    let startup_dt_hours = input.spec.period_hours(input.hour);
                    let shutdown_dt_hours = if input.hour + 1 < input.n_hours {
                        input.spec.period_hours(input.hour + 1)
                    } else {
                        startup_dt_hours
                    };
                    add_footroom_terms(
                        &mut block.triplets,
                        CommitmentCouplingInput {
                            cols: CommitmentCouplingCols {
                                row,
                                pg_col: input.layout.pg_col(input.hour, j),
                                reserve_col: ap
                                    .gen_reserve_col(j)
                                    .map(|intra| input.layout.col(input.hour, intra)),
                                commitment_col: input.layout.commitment_col(input.hour, j),
                                startup_col: input.layout.startup_col(input.hour, j),
                                shutdown_next_col: (input.hour + 1 < input.n_hours)
                                    .then(|| input.layout.shutdown_col(input.hour + 1, j)),
                                headroom_slack_col: None,
                                footroom_slack_col: Some(
                                    input.layout.footroom_slack_col(input.hour, j),
                                ),
                            },
                            layout: input.layout,
                            generator,
                            spec: input.spec,
                            hour: input.hour,
                            n_hours: input.n_hours,
                            gen_idx: j,
                            startup_dt_hours,
                            shutdown_dt_hours,
                            initial_dispatch_mw: input.spec.prev_dispatch_mw_at(j),
                            base: input.base,
                            limit_pu: pmin_pu,
                            enforce_shutdown_deloading: input.spec.enforce_shutdown_deloading,
                            offline_commitment_trajectories: input
                                .spec
                                .offline_commitment_trajectories,
                        },
                    );
                    block.row_lower[local_row + offset] = -BIG_M;
                    block.row_upper[local_row + offset] = 0.0;
                }
                local_row += ap.gen_participation.len();
            }
            surge_network::market::EnergyCoupling::None => {}
        }

        match dl_energy_coupling(&ap.product) {
            surge_network::market::EnergyCoupling::Headroom => {
                // Group-level headroom, emitted only for participating
                // groups. Non-participants collapse to `Σ p_served ≤
                // Σ pmax` — redundant with per-block p_served col
                // bounds.
                //   r_group + Σ_{m ∈ group} p_served[m] ≤ Σ pmax[m]
                for (offset, &gi) in ap.dl_group_participation.iter().enumerate() {
                    let group = &input.reserve_layout.dl_consumer_groups[gi];
                    let row = input.row_base + local_row + offset;
                    if let Some(intra) = ap.dl_group_reserve_col(gi) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                    let mut pmax_sum_pu = 0.0;
                    for &k in &group.member_dl_indices {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, input.layout.dispatch.dl + k),
                            1.0,
                        );
                        pmax_sum_pu += dispatchable_load_pmax_pu(&input, k, input.dl_list[k]);
                    }
                    block.row_lower[local_row + offset] = -BIG_M;
                    block.row_upper[local_row + offset] = pmax_sum_pu;
                }
                local_row += ap.dl_group_participation.len();
            }
            surge_network::market::EnergyCoupling::Footroom => {
                for (offset, &gi) in ap.dl_group_participation.iter().enumerate() {
                    let group = &input.reserve_layout.dl_consumer_groups[gi];
                    let row = input.row_base + local_row + offset;
                    if let Some(intra) = ap.dl_group_reserve_col(gi) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                    let mut pmin_sum_pu = 0.0;
                    for &k in &group.member_dl_indices {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, input.layout.dispatch.dl + k),
                            -1.0,
                        );
                        pmin_sum_pu += input.dl_list[k].p_min_pu;
                    }
                    block.row_lower[local_row + offset] = -BIG_M;
                    block.row_upper[local_row + offset] = -pmin_sum_pu;
                }
                local_row += ap.dl_group_participation.len();
            }
            surge_network::market::EnergyCoupling::None => {}
        }

        if ap.system_balance_cap_mw > 0.0 {
            let row = input.row_base + local_row;
            for balance_idx in &ap.balance_product_indices {
                let Some(balance_product) = input.reserve_layout.products.get(*balance_idx) else {
                    continue;
                };
                for j in 0..n_gen {
                    if let Some(intra) = balance_product.gen_reserve_col(j) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                }
                for gi in 0..input.reserve_layout.dl_consumer_groups.len() {
                    if let Some(intra) = balance_product.dl_group_reserve_col(gi) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                }
            }
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.col(input.hour, ap.slack_offset),
                1.0,
            );

            let period_req_mw = ap
                .system_balance_req_indices
                .iter()
                .filter_map(|&idx| input.r_sys_reqs.get(idx))
                .map(|req: &SystemReserveRequirement| req.requirement_mw_for_period(input.hour))
                .sum::<f64>();
            block.row_lower[local_row] = period_req_mw / input.base;
            block.row_upper[local_row] = BIG_M;
            local_row += 1;
        }

        for (zi, req) in ap.zonal_reqs.iter().enumerate() {
            let req_mw = req
                .balance_req_indices
                .iter()
                .filter_map(|&idx| input.r_zonal_reqs.get(idx))
                .map(|item| item.requirement_mw_for_period(input.hour))
                .sum::<f64>();
            // Zone membership lookups use the pre-computed
            // `fallback_area_by_*` slices so `study_area_for_bus` isn't
            // re-invoked per DL (which would rebuild the network's bus
            // index map inside every filter call). With the HashSet on
            // `req.participant_bus_set`, the common case is an O(1)
            // lookup per iteration.
            let bus_matches_gen = |j: usize| -> bool {
                let bus_number = input.network.generators[input.gen_indices[j]].bus;
                match &req.participant_bus_set {
                    Some(set) => set.contains(&bus_number),
                    None => {
                        fallback_area_by_gen.get(j).copied().flatten().unwrap_or(0) == req.zone_id
                    }
                }
            };
            let bus_matches_dl = |k: usize, dl_bus: u32| -> bool {
                match &req.participant_bus_set {
                    Some(set) => set.contains(&dl_bus),
                    None => {
                        fallback_area_by_dl.get(k).copied().flatten().unwrap_or(0) == req.zone_id
                    }
                }
            };
            let bus_matches_group = |gi: usize, canonical_bus: u32| -> bool {
                match &req.participant_bus_set {
                    Some(set) => set.contains(&canonical_bus),
                    None => {
                        fallback_area_by_group
                            .get(gi)
                            .copied()
                            .flatten()
                            .unwrap_or(0)
                            == req.zone_id
                    }
                }
            };
            let zone_gen_indices: Vec<usize> = input
                .gen_indices
                .iter()
                .enumerate()
                .filter_map(|(j, _)| bus_matches_gen(j).then_some(j))
                .collect();
            let zone_dl_indices: Vec<usize> = input
                .dl_list
                .iter()
                .enumerate()
                .filter_map(|(k, dl)| bus_matches_dl(k, dl.bus).then_some(k))
                .collect();
            let zone_group_indices: Vec<usize> = input
                .reserve_layout
                .dl_consumer_groups
                .iter()
                .enumerate()
                .filter_map(|(gi, group)| bus_matches_group(gi, group.canonical_bus).then_some(gi))
                .collect();
            // NOTE: see `common/reserves.rs::build_constraints`. These are
            // dimensionless fractions (e.g. 0.03 for "3% of served consumer
            // MW"); they couple directly to per-unit decision variables and
            // must stay unscaled. Dividing by base collapses the enforced
            // requirement to 1/base of its intended value.
            let largest_coeff = req
                .balance_largest_generator_dispatch_coefficient
                .unwrap_or(0.0);
            let served_dl_coeff = req
                .balance_served_dispatchable_load_coefficient
                .unwrap_or(0.0);
            let has_peak_rows = largest_coeff > 0.0 && !zone_gen_indices.is_empty();

            let mut emit_row = |peak_gen_local: Option<usize>, local_row_idx: usize| {
                let row = input.row_base + local_row_idx;
                for balance_idx in &ap.balance_product_indices {
                    let Some(balance_product) = input.reserve_layout.products.get(*balance_idx)
                    else {
                        continue;
                    };
                    for &j in &zone_gen_indices {
                        if let Some(intra) = balance_product.gen_reserve_col(j) {
                            push_triplet(
                                &mut block.triplets,
                                row,
                                input.layout.col(input.hour, intra),
                                1.0,
                            );
                        }
                    }
                    for &gi in &zone_group_indices {
                        if let Some(intra) = balance_product.dl_group_reserve_col(gi) {
                            push_triplet(
                                &mut block.triplets,
                                row,
                                input.layout.col(input.hour, intra),
                                1.0,
                            );
                        }
                    }
                }
                for &k in &zone_dl_indices {
                    if served_dl_coeff > 0.0 {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, input.layout.dispatch.dl + k),
                            -served_dl_coeff,
                        );
                    }
                }
                if let Some(j) = peak_gen_local {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.pg_col(input.hour, j),
                        -largest_coeff,
                    );
                }
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.col(input.hour, ap.zonal_slack_offset + zi),
                    1.0,
                );
                block.row_lower[local_row_idx] = req_mw / input.base;
                block.row_upper[local_row_idx] = BIG_M;
            };

            if has_peak_rows {
                for &peak_gen_local in &zone_gen_indices {
                    emit_row(Some(peak_gen_local), local_row);
                    local_row += 1;
                }
            } else {
                emit_row(None, local_row);
                local_row += 1;
            }
        }
    }

    // Offline headroom rows for producers and consumers:
    //   p^su_jt + p^sd_jt + Σ(offline reserves) ≤ pmax × (1 − u^on_jt)
    //
    // Startup/shutdown trajectory power and offline reserve awards must
    // together not exceed the physical capacity envelope when a unit is
    // offline.  Rewritten for the LP as:
    //   trajectory_terms + Σ(offline_reserve_vars) + pmax·u_commitment ≤ pmax
    if has_offline_reserve_products(input.reserve_layout) {
        for (j, &gi) in input.gen_indices.iter().enumerate() {
            let generator = &input.network.generators[gi];
            let pmax_pu = input.hourly_network.generators[gi].pmax / input.base;
            let row = input.row_base + local_row + j;
            let startup_dt_hours = input.spec.period_hours(input.hour);
            let shutdown_dt_hours = if input.hour + 1 < input.n_hours {
                input.spec.period_hours(input.hour + 1)
            } else {
                startup_dt_hours
            };

            // Trajectory power (positive coefficient = consumes offline headroom).
            add_commitment_trajectory_terms(
                &mut block.triplets,
                &CommitmentCouplingInput {
                    cols: CommitmentCouplingCols {
                        row,
                        pg_col: input.layout.pg_col(input.hour, j),
                        reserve_col: None,
                        commitment_col: input.layout.commitment_col(input.hour, j),
                        startup_col: input.layout.startup_col(input.hour, j),
                        shutdown_next_col: None,
                        headroom_slack_col: None,
                        footroom_slack_col: None,
                    },
                    layout: input.layout,
                    generator,
                    spec: input.spec,
                    hour: input.hour,
                    n_hours: input.n_hours,
                    gen_idx: j,
                    startup_dt_hours,
                    shutdown_dt_hours,
                    initial_dispatch_mw: input.spec.prev_dispatch_mw_at(j),
                    base: input.base,
                    limit_pu: pmax_pu,
                    enforce_shutdown_deloading: false,
                    offline_commitment_trajectories: false,
                },
                1.0, // positive: trajectory consumes offline headroom
            );

            // Offline reserve variables.
            for ap in &input.reserve_layout.products {
                if matches!(
                    ap.product.qualification,
                    surge_network::market::QualificationRule::OfflineQuickStart
                ) {
                    if let Some(intra) = ap.gen_reserve_col(j) {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.col(input.hour, intra),
                            1.0,
                        );
                    }
                }
            }

            // pmax × u_commitment (so RHS − LHS = pmax·(1 − u) − offline_usage).
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.commitment_col(input.hour, j),
                pmax_pu,
            );

            block.row_lower[local_row + j] = -BIG_M;
            block.row_upper[local_row + j] = pmax_pu;
        }
        local_row += n_gen;
    }

    t_per_product = _t_pp_t0.elapsed().as_secs_f64();
    if trace_timings {
        tracing::info!(
            stage = "build_capacity_logic_reserve_rows.hour0",
            total_secs = t_gen_coupling
                + t_commitment_state
                + t_cross_headroom
                + t_cross_footroom
                + t_dl_cross_headroom
                + t_dl_cross_footroom
                + t_shared_gen
                + t_shared_dl
                + t_per_product,
            gen_coupling_secs = t_gen_coupling,
            commitment_state_secs = t_commitment_state,
            cross_headroom_secs = t_cross_headroom,
            cross_footroom_secs = t_cross_footroom,
            dl_cross_headroom_secs = t_dl_cross_headroom,
            dl_cross_footroom_secs = t_dl_cross_footroom,
            shared_gen_secs = t_shared_gen,
            shared_dl_secs = t_shared_dl,
            per_product_secs = t_per_product,
            n_cross_headroom_rows = input.reserve_layout.n_cross_headroom_rows,
            n_cross_footroom_rows = input.reserve_layout.n_cross_footroom_rows,
            n_dl_cross_headroom_rows = input.reserve_layout.n_dl_cross_headroom_rows,
            n_dl_cross_footroom_rows = input.reserve_layout.n_dl_cross_footroom_rows,
            n_dl_groups = n_dl_groups,
            "SCUC capacity_logic sub-phase (hour 0)"
        );
    }
    debug_assert!(local_row <= n_rows);
    block
}

fn extend_block(dst: &mut LpBlock, src: LpBlock) {
    dst.triplets.extend(src.triplets);
    dst.row_lower.extend(src.row_lower);
    dst.row_upper.extend(src.row_upper);
}

pub(super) fn storage_rows_per_hour(
    setup: &DispatchSetup,
    spec: &DispatchProblemSpec<'_>,
) -> usize {
    if setup.n_storage == 0 {
        return 0;
    }
    let base_rows = 4 * setup.n_storage + setup.n_sto_dis_offer_rows + setup.n_sto_ch_bid_rows;
    // SoC-dependent power foldback adds one row per storage unit per
    // direction where the threshold is set (see
    // ``builders::build_storage_rows`` section 7).
    let foldback_rows = setup
        .storage_foldback_discharge_mwh
        .iter()
        .filter(|o| o.is_some())
        .count()
        + setup
            .storage_foldback_charge_mwh
            .iter()
            .filter(|o| o.is_some())
            .count();
    let total = base_rows + foldback_rows;
    if spec.storage_reserve_soc_impact.is_empty() {
        total
    } else {
        total + 2 * setup.n_storage
    }
}

pub(super) struct ScucStorageRowsInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub setup: &'a DispatchSetup,
    pub reserve_layout: &'a ReserveLpLayout,
    pub storage_initial_soc_mwh: &'a [f64],
    pub layout: &'a ScucLayout,
    pub hour: usize,
    pub row_base: usize,
    pub base: f64,
}

pub(super) fn build_storage_rows(input: ScucStorageRowsInput<'_>) -> LpBlock {
    let col_base = input.layout.hour_col_base(input.hour);
    let period_hours = input.spec.period_hours(input.hour);
    let mut block = builders::build_storage_rows(
        input.network,
        input.setup,
        input.layout.dispatch.sto_ch,
        input.layout.dispatch.sto_dis,
        input.layout.dispatch.sto_soc,
        input.layout.dispatch.sto_epi_dis,
        input.layout.dispatch.sto_epi_ch,
        input.layout.dispatch.pg,
        col_base,
        input.row_base,
        input.storage_initial_soc_mwh,
        (input.hour > 0).then(|| input.layout.hour_col_base(input.hour - 1)),
        period_hours,
        input.reserve_layout,
        false,
        input.base,
    );

    if input.setup.n_storage == 0 || input.spec.storage_reserve_soc_impact.is_empty() {
        return block;
    }

    let mut local_row = block.row_lower.len();

    for &(s, j, gi) in &input.setup.storage_gen_local {
        let generator = &input.network.generators[gi];
        let storage = generator
            .storage
            .as_ref()
            .expect("storage_gen_local only contains generators with storage");
        let row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.storage_soc_col(input.hour, s),
            1.0,
        );
        if let Some(products) = input.spec.storage_reserve_soc_impact.get(&gi) {
            for ap in &input.reserve_layout.products {
                if let Some(factors) = products.get(&ap.product.id) {
                    let impact = factors.get(input.hour).copied().unwrap_or(0.0);
                    if impact > 0.0 {
                        if let Some(intra) = ap.gen_reserve_col(j) {
                            push_triplet(
                                &mut block.triplets,
                                row,
                                input.layout.col(input.hour, intra),
                                -impact * period_hours * input.base,
                            );
                        }
                    }
                }
            }
        }
        block.row_lower.push(storage.soc_min_mwh);
        block.row_upper.push(f64::INFINITY);
        local_row += 1;
    }

    for &(s, j, gi) in &input.setup.storage_gen_local {
        let generator = &input.network.generators[gi];
        let storage = generator
            .storage
            .as_ref()
            .expect("storage_gen_local only contains generators with storage");
        let row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.storage_soc_col(input.hour, s),
            1.0,
        );
        if let Some(products) = input.spec.storage_reserve_soc_impact.get(&gi) {
            for ap in &input.reserve_layout.products {
                if let Some(factors) = products.get(&ap.product.id) {
                    let impact = factors.get(input.hour).copied().unwrap_or(0.0);
                    if impact < 0.0 {
                        if let Some(intra) = ap.gen_reserve_col(j) {
                            push_triplet(
                                &mut block.triplets,
                                row,
                                input.layout.col(input.hour, intra),
                                -impact * period_hours * input.base,
                            );
                        }
                    }
                }
            }
        }
        block.row_lower.push(f64::NEG_INFINITY);
        block.row_upper.push(storage.soc_max_mwh);
        local_row += 1;
    }

    block
}

fn frequency_rows_per_hour(spec: &DispatchProblemSpec<'_>) -> usize {
    usize::from(spec.frequency_security.effective_min_inertia_mws() > 0.0)
        + usize::from(spec.frequency_security.min_pfr_mw.is_some_and(|v| v > 0.0))
}

fn regulation_capacity_rows(network: &Network, gen_indices: &[usize]) -> usize {
    gen_indices
        .iter()
        .map(|&gi| {
            let generator = &network.generators[gi];
            usize::from(
                generator
                    .commitment
                    .as_ref()
                    .and_then(|c| c.p_reg_max)
                    .is_some(),
            ) + usize::from(
                generator
                    .commitment
                    .as_ref()
                    .and_then(|c| c.p_reg_min)
                    .is_some(),
            )
        })
        .sum()
}

fn active_reg_product_count(reserve_layout: &ReserveLpLayout) -> usize {
    reserve_layout
        .products
        .iter()
        .filter(|ap| ap.product.id.starts_with("reg"))
        .count()
}

pub(super) fn frequency_block_reg_rows_per_hour(
    network: &Network,
    setup: &DispatchSetup,
    reserve_layout: &ReserveLpLayout,
    gen_indices: &[usize],
    spec: &DispatchProblemSpec<'_>,
    has_reg_products: bool,
) -> usize {
    let mut rows = frequency_rows_per_hour(spec);
    if setup.is_block_mode {
        rows += gen_indices.len();
    }
    if setup.has_per_block_reserves {
        let n_active_products = reserve_layout.products.len();
        rows += gen_indices.len() * n_active_products + setup.n_block_vars * n_active_products;
    }
    if has_reg_products {
        rows += gen_indices.len();
        rows += regulation_capacity_rows(network, gen_indices);
        if setup.has_per_block_reserves {
            rows += setup.n_block_vars * active_reg_product_count(reserve_layout);
        }
    }
    rows
}

pub(super) struct ScucFrequencyBlockRegRowsInput<'a> {
    pub network: &'a Network,
    pub hourly_network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub setup: &'a DispatchSetup,
    pub reserve_layout: &'a ReserveLpLayout,
    pub gen_indices: &'a [usize],
    pub layout: &'a ScucLayout,
    pub hour: usize,
    pub row_base: usize,
    pub base: f64,
    pub has_reg_products: bool,
}

pub(super) fn build_frequency_block_reg_rows(input: ScucFrequencyBlockRegRowsInput<'_>) -> LpBlock {
    let col_base = input.layout.hour_col_base(input.hour);
    let mut row_base = input.row_base;
    let mut block = LpBlock::empty();

    let freq_block = builders::build_frequency_rows(
        input.hourly_network,
        input.gen_indices,
        input.spec,
        col_base,
        row_base,
        input.layout.dispatch.pg,
        input.base,
        true,
        input.layout.commitment,
    );
    row_base += freq_block.n_rows();
    extend_block(&mut block, freq_block);

    if input.setup.is_block_mode {
        let block_link = builders::build_block_linking_rows(
            input.setup,
            input.spec,
            input.gen_indices,
            input.network,
            input.hour,
            col_base,
            row_base,
            input.layout.dispatch.pg,
            input.layout.dispatch.block,
            Some(input.layout.commitment),
            input.base,
        );
        row_base += block_link.n_rows();
        extend_block(&mut block, block_link);
    }

    if input.setup.has_per_block_reserves {
        let block_reserve = builders::build_per_block_reserve_rows(
            input.setup,
            input.reserve_layout,
            col_base,
            row_base,
            input.layout.dispatch.block,
            input.layout.dispatch.block_reserve,
            input.base,
        );
        row_base += block_reserve.n_rows();
        extend_block(&mut block, block_reserve);
    }

    if !input.has_reg_products {
        return block;
    }

    for j in 0..input.gen_indices.len() {
        let row = row_base;
        push_triplet(
            &mut block.triplets,
            row,
            input
                .layout
                .col(input.hour, input.layout.regulation_mode + j),
            1.0,
        );
        push_triplet(
            &mut block.triplets,
            row,
            input.layout.commitment_col(input.hour, j),
            -1.0,
        );
        block.row_lower.push(f64::NEG_INFINITY);
        block.row_upper.push(0.0);
        row_base += 1;
    }

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];
        let r_col = input
            .layout
            .col(input.hour, input.layout.regulation_mode + j);
        let u_col = input.layout.commitment_col(input.hour, j);
        let pg_col = input.layout.pg_col(input.hour, j);

        if let Some(prmax) = generator.commitment.as_ref().and_then(|c| c.p_reg_max) {
            let row = row_base;
            push_triplet(&mut block.triplets, row, pg_col, 1.0);
            push_triplet(
                &mut block.triplets,
                row,
                r_col,
                (generator.pmax - prmax) / input.base,
            );
            push_triplet(
                &mut block.triplets,
                row,
                u_col,
                -(generator.pmax / input.base),
            );
            block.row_lower.push(f64::NEG_INFINITY);
            block.row_upper.push(0.0);
            row_base += 1;
        }

        if let Some(prmin) = generator.commitment.as_ref().and_then(|c| c.p_reg_min) {
            let row = row_base;
            push_triplet(&mut block.triplets, row, pg_col, -1.0);
            push_triplet(
                &mut block.triplets,
                row,
                r_col,
                (prmin - generator.pmin) / input.base,
            );
            push_triplet(&mut block.triplets, row, u_col, generator.pmin / input.base);
            block.row_lower.push(f64::NEG_INFINITY);
            block.row_upper.push(0.0);
            row_base += 1;
        }
    }

    if input.setup.has_per_block_reserves {
        for (pi, ap) in input.reserve_layout.products.iter().enumerate() {
            if !ap.product.id.starts_with("reg") {
                continue;
            }
            let deploy_min = ap.product.deploy_secs / 60.0;
            for (j, blocks) in input.setup.gen_blocks.iter().enumerate() {
                for (i, gen_block) in blocks.iter().enumerate() {
                    let row = row_base;
                    let reg_ramp = gen_block.reg_ramp_up_mw_per_min * deploy_min;
                    let reg_bound = gen_block.width_mw().min(reg_ramp) / input.base;
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.col(
                            input.hour,
                            input.layout.dispatch.block_reserve
                                + pi * input.setup.n_block_vars
                                + input.setup.gen_block_start[j]
                                + i,
                        ),
                        1.0,
                    );
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(input.hour, input.layout.regulation_mode + j),
                        -reg_bound,
                    );
                    block.row_lower.push(f64::NEG_INFINITY);
                    block.row_upper.push(0.0);
                    row_base += 1;
                }
            }
        }
    }

    block
}

pub(super) struct ScucFozGroup<'a> {
    pub gen_idx: usize,
    pub segments: &'a [(f64, f64)],
    pub zones: &'a [(f64, f64)],
    pub max_transit: &'a [usize],
    pub delta_local_off: usize,
    pub phi_local_off: usize,
    pub rho_local_off: usize,
    pub pmax_pu: f64,
}

pub(super) fn foz_rows_per_hour(foz_groups: &[ScucFozGroup<'_>]) -> usize {
    3 * foz_groups.len()
}

pub(super) struct ScucFozHourlyRowsInput<'a> {
    pub foz_groups: &'a [ScucFozGroup<'a>],
    pub layout: &'a ScucLayout,
    pub hour: usize,
    pub row_base: usize,
    pub base: f64,
}

pub(super) fn build_foz_rows(input: ScucFozHourlyRowsInput<'_>) -> LpBlock {
    let n_rows = foz_rows_per_hour(input.foz_groups);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for group in input.foz_groups {
        let selection_row = input.row_base + local_row;
        for (k, _) in group.segments.iter().enumerate() {
            push_triplet(
                &mut block.triplets,
                selection_row,
                input.layout.col(
                    input.hour,
                    input.layout.foz_delta + group.delta_local_off + k,
                ),
                1.0,
            );
        }
        for (z, _) in group.zones.iter().enumerate() {
            push_triplet(
                &mut block.triplets,
                selection_row,
                input
                    .layout
                    .col(input.hour, input.layout.foz_phi + group.phi_local_off + z),
                1.0,
            );
        }
        push_triplet(
            &mut block.triplets,
            selection_row,
            input.layout.commitment_col(input.hour, group.gen_idx),
            -1.0,
        );
        block.row_lower[local_row] = 0.0;
        block.row_upper[local_row] = 0.0;
        local_row += 1;

        let upper_row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            upper_row,
            input.layout.pg_col(input.hour, group.gen_idx),
            1.0,
        );
        for (k, &(_lo, hi)) in group.segments.iter().enumerate() {
            push_triplet(
                &mut block.triplets,
                upper_row,
                input.layout.col(
                    input.hour,
                    input.layout.foz_delta + group.delta_local_off + k,
                ),
                -(hi / input.base),
            );
        }
        for (z, &(_lo, hi)) in group.zones.iter().enumerate() {
            push_triplet(
                &mut block.triplets,
                upper_row,
                input
                    .layout
                    .col(input.hour, input.layout.foz_phi + group.phi_local_off + z),
                -(hi / input.base),
            );
        }
        block.row_lower[local_row] = -BIG_M;
        block.row_upper[local_row] = 0.0;
        local_row += 1;

        let lower_row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            lower_row,
            input.layout.pg_col(input.hour, group.gen_idx),
            -1.0,
        );
        for (k, &(lo, _hi)) in group.segments.iter().enumerate() {
            push_triplet(
                &mut block.triplets,
                lower_row,
                input.layout.col(
                    input.hour,
                    input.layout.foz_delta + group.delta_local_off + k,
                ),
                lo / input.base,
            );
        }
        for (z, &(lo, _hi)) in group.zones.iter().enumerate() {
            push_triplet(
                &mut block.triplets,
                lower_row,
                input
                    .layout
                    .col(input.hour, input.layout.foz_phi + group.phi_local_off + z),
                lo / input.base,
            );
        }
        block.row_lower[local_row] = -BIG_M;
        block.row_upper[local_row] = 0.0;
        local_row += 1;
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) fn foz_cross_rows(foz_groups: &[ScucFozGroup<'_>], n_hours: usize) -> usize {
    let rolling_rows: usize = foz_groups
        .iter()
        .flat_map(|group| {
            group.max_transit.iter().map(|&max_transit| {
                let window = max_transit + 1;
                if window <= n_hours {
                    n_hours - window + 1
                } else {
                    0
                }
            })
        })
        .sum();
    let monotonic_rows = if n_hours > 1 {
        2 * foz_groups
            .iter()
            .map(|group| group.zones.len())
            .sum::<usize>()
            * (n_hours - 1)
    } else {
        0
    };
    rolling_rows + monotonic_rows
}

pub(super) struct ScucFozCrossRowsInput<'a> {
    pub foz_groups: &'a [ScucFozGroup<'a>],
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_foz_cross_rows(input: ScucFozCrossRowsInput<'_>) -> LpBlock {
    let n_rows = foz_cross_rows(input.foz_groups, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for group in input.foz_groups {
        for (z, &max_transit) in group.max_transit.iter().enumerate() {
            let window = max_transit + 1;
            if window > input.n_hours {
                continue;
            }
            for hour in 0..=(input.n_hours - window) {
                let row = input.row_base + local_row;
                for offset in 0..window {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.col(
                            hour + offset,
                            input.layout.foz_phi + group.phi_local_off + z,
                        ),
                        1.0,
                    );
                }
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = (window - 1) as f64;
                local_row += 1;
            }
        }
    }

    if input.n_hours > 1 {
        for group in input.foz_groups {
            for z in 0..group.zones.len() {
                for hour in 0..(input.n_hours - 1) {
                    let phi_t = input
                        .layout
                        .col(hour, input.layout.foz_phi + group.phi_local_off + z);
                    let phi_t1 = input
                        .layout
                        .col(hour + 1, input.layout.foz_phi + group.phi_local_off + z);
                    let rho_col = input
                        .layout
                        .col(hour, input.layout.foz_rho + group.rho_local_off + z);

                    let lower_row = input.row_base + local_row;
                    push_triplet(
                        &mut block.triplets,
                        lower_row,
                        input.layout.pg_col(hour + 1, group.gen_idx),
                        1.0,
                    );
                    push_triplet(
                        &mut block.triplets,
                        lower_row,
                        input.layout.pg_col(hour, group.gen_idx),
                        -1.0,
                    );
                    push_triplet(&mut block.triplets, lower_row, rho_col, -group.pmax_pu);
                    push_triplet(&mut block.triplets, lower_row, phi_t, -group.pmax_pu);
                    push_triplet(&mut block.triplets, lower_row, phi_t1, -group.pmax_pu);
                    block.row_lower[local_row] = -3.0 * group.pmax_pu;
                    block.row_upper[local_row] = BIG_M;
                    local_row += 1;

                    let upper_row = input.row_base + local_row;
                    push_triplet(
                        &mut block.triplets,
                        upper_row,
                        input.layout.pg_col(hour + 1, group.gen_idx),
                        1.0,
                    );
                    push_triplet(
                        &mut block.triplets,
                        upper_row,
                        input.layout.pg_col(hour, group.gen_idx),
                        -1.0,
                    );
                    push_triplet(&mut block.triplets, upper_row, rho_col, -group.pmax_pu);
                    push_triplet(&mut block.triplets, upper_row, phi_t, group.pmax_pu);
                    push_triplet(&mut block.triplets, upper_row, phi_t1, group.pmax_pu);
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = 2.0 * group.pmax_pu;
                    local_row += 1;
                }
            }
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) struct ScucPhModeUnit {
    pub storage_idx: usize,
    pub dis_max_mw: f64,
    pub ch_max_mw: f64,
    pub min_gen_run: usize,
    pub min_pump_run: usize,
    pub p2g_delay: usize,
    pub g2p_delay: usize,
    pub max_pump_starts: Option<u32>,
    pub m_gen_local_off: usize,
    pub m_pump_local_off: usize,
}

pub(super) struct ScucPhHeadUnit<'a> {
    pub storage_idx: usize,
    pub breakpoints: &'a [(f64, f64)],
}

fn pumped_hydro_head_rows_per_hour(ph_head_units: &[ScucPhHeadUnit<'_>]) -> usize {
    ph_head_units
        .iter()
        .map(|unit| {
            unit.breakpoints
                .windows(2)
                .filter(|pair| (pair[1].0 - pair[0].0).abs() >= 1e-12)
                .count()
        })
        .sum()
}

pub(super) fn pumped_hydro_rows_per_hour(
    ph_mode_units: &[ScucPhModeUnit],
    ph_head_units: &[ScucPhHeadUnit<'_>],
) -> usize {
    3 * ph_mode_units.len() + pumped_hydro_head_rows_per_hour(ph_head_units)
}

pub(super) struct ScucPumpedHydroRowsInput<'a> {
    pub ph_mode_units: &'a [ScucPhModeUnit],
    pub ph_head_units: &'a [ScucPhHeadUnit<'a>],
    pub layout: &'a ScucLayout,
    pub hour: usize,
    pub row_base: usize,
}

pub(super) fn build_pumped_hydro_rows(input: ScucPumpedHydroRowsInput<'_>) -> LpBlock {
    let n_rows = pumped_hydro_rows_per_hour(input.ph_mode_units, input.ph_head_units);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for unit in input.ph_mode_units {
        let m_gen_col = input
            .layout
            .col(input.hour, input.layout.ph_mode + unit.m_gen_local_off);
        let m_pump_col = input
            .layout
            .col(input.hour, input.layout.ph_mode + unit.m_pump_local_off);

        let dis_row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            dis_row,
            input
                .layout
                .storage_discharge_col(input.hour, unit.storage_idx),
            1.0,
        );
        push_triplet(&mut block.triplets, dis_row, m_gen_col, -unit.dis_max_mw);
        block.row_lower[local_row] = -BIG_M;
        block.row_upper[local_row] = 0.0;
        local_row += 1;

        let ch_row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            ch_row,
            input
                .layout
                .storage_charge_col(input.hour, unit.storage_idx),
            1.0,
        );
        push_triplet(&mut block.triplets, ch_row, m_pump_col, -unit.ch_max_mw);
        block.row_lower[local_row] = -BIG_M;
        block.row_upper[local_row] = 0.0;
        local_row += 1;

        let exclusivity_row = input.row_base + local_row;
        push_triplet(&mut block.triplets, exclusivity_row, m_gen_col, 1.0);
        push_triplet(&mut block.triplets, exclusivity_row, m_pump_col, 1.0);
        block.row_lower[local_row] = -BIG_M;
        block.row_upper[local_row] = 1.0;
        local_row += 1;
    }

    for unit in input.ph_head_units {
        for pair in unit.breakpoints.windows(2) {
            let (soc_k, pmax_k) = pair[0];
            let (soc_k1, pmax_k1) = pair[1];
            let dsoc = soc_k1 - soc_k;
            if dsoc.abs() < 1e-12 {
                continue;
            }
            let slope = (pmax_k1 - pmax_k) / dsoc;
            let rhs = pmax_k - slope * soc_k;

            let row = input.row_base + local_row;
            push_triplet(
                &mut block.triplets,
                row,
                input
                    .layout
                    .storage_discharge_col(input.hour, unit.storage_idx),
                1.0,
            );
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.storage_soc_col(input.hour, unit.storage_idx),
                -slope,
            );
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = rhs;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) fn pumped_hydro_transition_rows(
    ph_mode_units: &[ScucPhModeUnit],
    n_hours: usize,
) -> usize {
    ph_mode_units
        .iter()
        .map(|unit| {
            let mut rows = 0usize;
            if unit.min_gen_run > 1 {
                rows += n_hours;
            }
            if unit.min_pump_run > 1 {
                rows += n_hours;
            }
            if unit.p2g_delay > 0 {
                for hour in 0..n_hours {
                    let start = (hour + 1).saturating_sub(unit.p2g_delay);
                    rows += hour.saturating_sub(start);
                }
            }
            if unit.g2p_delay > 0 {
                for hour in 0..n_hours {
                    let start = (hour + 1).saturating_sub(unit.g2p_delay);
                    rows += hour.saturating_sub(start);
                }
            }
            if unit.max_pump_starts.is_some() {
                rows += 1;
            }
            rows
        })
        .sum()
}

pub(super) struct ScucPumpedHydroTransitionRowsInput<'a> {
    pub ph_mode_units: &'a [ScucPhModeUnit],
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_pumped_hydro_transition_rows(
    input: ScucPumpedHydroTransitionRowsInput<'_>,
) -> LpBlock {
    let n_rows = pumped_hydro_transition_rows(input.ph_mode_units, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for unit in input.ph_mode_units {
        if unit.min_gen_run > 1 {
            for hour in 0..input.n_hours {
                let end = (hour + unit.min_gen_run).min(input.n_hours);
                let span = end - hour;
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    input
                        .layout
                        .col(hour, input.layout.ph_mode + unit.m_gen_local_off),
                    1.0 - span as f64,
                );
                for next_hour in (hour + 1)..end {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(next_hour, input.layout.ph_mode + unit.m_gen_local_off),
                        1.0,
                    );
                }
                if hour > 0 {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(hour - 1, input.layout.ph_mode + unit.m_gen_local_off),
                        span as f64,
                    );
                }
                block.row_lower[local_row] = 0.0;
                block.row_upper[local_row] = BIG_M;
                local_row += 1;
            }
        }

        if unit.min_pump_run > 1 {
            for hour in 0..input.n_hours {
                let end = (hour + unit.min_pump_run).min(input.n_hours);
                let span = end - hour;
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    input
                        .layout
                        .col(hour, input.layout.ph_mode + unit.m_pump_local_off),
                    1.0 - span as f64,
                );
                for next_hour in (hour + 1)..end {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(next_hour, input.layout.ph_mode + unit.m_pump_local_off),
                        1.0,
                    );
                }
                if hour > 0 {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(hour - 1, input.layout.ph_mode + unit.m_pump_local_off),
                        span as f64,
                    );
                }
                block.row_lower[local_row] = 0.0;
                block.row_upper[local_row] = BIG_M;
                local_row += 1;
            }
        }

        if unit.p2g_delay > 0 {
            for hour in 0..input.n_hours {
                let start = (hour + 1).saturating_sub(unit.p2g_delay);
                for lookback in start..hour {
                    let row = input.row_base + local_row;
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(hour, input.layout.ph_mode + unit.m_gen_local_off),
                        1.0,
                    );
                    if hour > 0 {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input
                                .layout
                                .col(hour - 1, input.layout.ph_mode + unit.m_gen_local_off),
                            -1.0,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(lookback, input.layout.ph_mode + unit.m_pump_local_off),
                        1.0,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = 1.0;
                    local_row += 1;
                }
            }
        }

        if unit.g2p_delay > 0 {
            for hour in 0..input.n_hours {
                let start = (hour + 1).saturating_sub(unit.g2p_delay);
                for lookback in start..hour {
                    let row = input.row_base + local_row;
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(hour, input.layout.ph_mode + unit.m_pump_local_off),
                        1.0,
                    );
                    if hour > 0 {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input
                                .layout
                                .col(hour - 1, input.layout.ph_mode + unit.m_pump_local_off),
                            -1.0,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input
                            .layout
                            .col(lookback, input.layout.ph_mode + unit.m_gen_local_off),
                        1.0,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = 1.0;
                    local_row += 1;
                }
            }
        }

        if let Some(max_pump_starts) = unit.max_pump_starts {
            let row = input.row_base + local_row;
            for hour in 0..input.n_hours {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input
                        .layout
                        .col(hour, input.layout.ph_mode + unit.m_pump_local_off),
                    1.0,
                );
            }
            let effective_limit = max_pump_starts as f64 * unit.min_pump_run.max(1) as f64;
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = effective_limit;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) struct ScucCcMemberGen {
    pub gen_idx: usize,
    pub config_indices: Vec<usize>,
    pub pgcc_entry_indices: Vec<usize>,
}

pub(super) struct ScucCcConfig {
    pub member_gen_j: Vec<usize>,
    pub min_up_periods: usize,
    pub min_down_periods: usize,
    pub p_min_pu: f64,
    pub p_max_pu: f64,
    pub big_m_pu: f64,
    pub ramp_up_pu: Option<f64>,
    pub ramp_down_pu: Option<f64>,
}

pub(super) struct ScucCcPgccEntry {
    pub config_idx: usize,
    pub pmax_pu: f64,
}

pub(super) struct ScucCcTransitionDelay {
    pub from_config: usize,
    pub to_config: usize,
    pub delay_periods: usize,
}

pub(super) struct ScucCcPlant {
    pub n_configs: usize,
    pub z_block_base: usize,
    pub ytrans_block_base: usize,
    pub pgcc_block_base: usize,
    pub initial_active_config: Option<usize>,
    pub member_gens: Vec<ScucCcMemberGen>,
    pub configs: Vec<ScucCcConfig>,
    pub disallowed_transitions: Vec<(usize, usize)>,
    pub delayed_transitions: Vec<ScucCcTransitionDelay>,
    pub transition_pairs: Vec<(usize, usize)>,
    pub pgcc_entries: Vec<ScucCcPgccEntry>,
}

fn cc_z_col(plant: &ScucCcPlant, config_idx: usize, hour: usize, n_hours: usize) -> usize {
    plant.z_block_base + config_idx * n_hours + hour
}

fn cc_yup_col(plant: &ScucCcPlant, config_idx: usize, hour: usize, n_hours: usize) -> usize {
    plant.z_block_base + plant.n_configs * n_hours + config_idx * n_hours + hour
}

fn cc_ydn_col(plant: &ScucCcPlant, config_idx: usize, hour: usize, n_hours: usize) -> usize {
    plant.z_block_base + 2 * plant.n_configs * n_hours + config_idx * n_hours + hour
}

fn cc_ytrans_col(plant: &ScucCcPlant, transition_idx: usize, hour: usize, n_hours: usize) -> usize {
    plant.ytrans_block_base + transition_idx * n_hours + hour
}

fn cc_pgcc_col(plant: &ScucCcPlant, entry_idx: usize, hour: usize, n_hours: usize) -> usize {
    plant.pgcc_block_base + entry_idx * n_hours + hour
}

pub(super) fn cc_rows(cc_plants: &[ScucCcPlant], n_hours: usize) -> usize {
    cc_plants
        .iter()
        .map(|plant| {
            let mut rows = n_hours;
            rows += plant.member_gens.len() * n_hours;
            rows += plant.n_configs * n_hours;
            rows += plant.disallowed_transitions.len() * n_hours.saturating_sub(1);

            for transition in &plant.delayed_transitions {
                for hour in 1..n_hours {
                    let window_start = (hour + 1).saturating_sub(transition.delay_periods);
                    rows += hour.saturating_sub(window_start);
                }
            }

            rows += plant
                .configs
                .iter()
                .filter(|config| config.min_up_periods > 1)
                .count()
                * n_hours;
            rows += plant
                .configs
                .iter()
                .filter(|config| config.min_down_periods > 1)
                .count()
                * n_hours;
            rows += 2 * plant.n_configs * n_hours;
            rows += plant
                .configs
                .iter()
                .map(|config| {
                    usize::from(config.ramp_up_pu.is_some()) * n_hours.saturating_sub(1)
                        + usize::from(config.ramp_down_pu.is_some()) * n_hours.saturating_sub(1)
                })
                .sum::<usize>();
            rows += plant.member_gens.len() * n_hours;
            rows += plant.pgcc_entries.len() * n_hours;
            rows += plant.transition_pairs.len() * n_hours * 3;
            rows
        })
        .sum()
}

pub(super) struct ScucCcRowsInput<'a> {
    pub cc_plants: &'a [ScucCcPlant],
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_cc_rows(input: ScucCcRowsInput<'_>) -> LpBlock {
    let n_rows = cc_rows(input.cc_plants, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for plant in input.cc_plants {
        for hour in 0..input.n_hours {
            let row = input.row_base + local_row;
            for config_idx in 0..plant.n_configs {
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_z_col(plant, config_idx, hour, input.n_hours),
                    1.0,
                );
            }
            block.row_lower[local_row] = 0.0;
            block.row_upper[local_row] = 1.0;
            local_row += 1;
        }

        for member in &plant.member_gens {
            for hour in 0..input.n_hours {
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.commitment_col(hour, member.gen_idx),
                    1.0,
                );
                for &config_idx in &member.config_indices {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_z_col(plant, config_idx, hour, input.n_hours),
                        -1.0,
                    );
                }
                block.row_lower[local_row] = 0.0;
                block.row_upper[local_row] = 0.0;
                local_row += 1;
            }
        }

        for config_idx in 0..plant.n_configs {
            for hour in 0..input.n_hours {
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_z_col(plant, config_idx, hour, input.n_hours),
                    1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_yup_col(plant, config_idx, hour, input.n_hours),
                    -1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_ydn_col(plant, config_idx, hour, input.n_hours),
                    1.0,
                );

                if hour > 0 {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_z_col(plant, config_idx, hour - 1, input.n_hours),
                        -1.0,
                    );
                    block.row_lower[local_row] = 0.0;
                    block.row_upper[local_row] = 0.0;
                } else {
                    let z0 = usize::from(plant.initial_active_config == Some(config_idx)) as f64;
                    block.row_lower[local_row] = z0;
                    block.row_upper[local_row] = z0;
                }
                local_row += 1;
            }
        }

        for &(from_config, to_config) in &plant.disallowed_transitions {
            for hour in 1..input.n_hours {
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_ydn_col(plant, from_config, hour, input.n_hours),
                    1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_yup_col(plant, to_config, hour, input.n_hours),
                    1.0,
                );
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = 1.0;
                local_row += 1;
            }
        }

        for transition in &plant.delayed_transitions {
            for hour in 1..input.n_hours {
                let window_start = (hour + 1).saturating_sub(transition.delay_periods);
                for lookback in window_start..hour {
                    let row = input.row_base + local_row;
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_yup_col(plant, transition.to_config, hour, input.n_hours),
                        1.0,
                    );
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_ydn_col(plant, transition.from_config, lookback, input.n_hours),
                        1.0,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = 1.0;
                    local_row += 1;
                }
            }
        }

        for (config_idx, config) in plant.configs.iter().enumerate() {
            if config.min_up_periods > 1 {
                for hour in 0..input.n_hours {
                    let end = (hour + config.min_up_periods).min(input.n_hours);
                    let span = end - hour;
                    let row = input.row_base + local_row;
                    for next_hour in hour..end {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            cc_z_col(plant, config_idx, next_hour, input.n_hours),
                            1.0,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_yup_col(plant, config_idx, hour, input.n_hours),
                        -(span as f64),
                    );
                    block.row_lower[local_row] = 0.0;
                    block.row_upper[local_row] = BIG_M;
                    local_row += 1;
                }
            }

            if config.min_down_periods > 1 {
                for hour in 0..input.n_hours {
                    let end = (hour + config.min_down_periods).min(input.n_hours);
                    let span = end - hour;
                    let row = input.row_base + local_row;
                    for next_hour in hour..end {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            cc_z_col(plant, config_idx, next_hour, input.n_hours),
                            1.0,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_ydn_col(plant, config_idx, hour, input.n_hours),
                        span as f64,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = span as f64;
                    local_row += 1;
                }
            }

            for hour in 0..input.n_hours {
                let upper_row = input.row_base + local_row;
                for &gen_idx in &config.member_gen_j {
                    push_triplet(
                        &mut block.triplets,
                        upper_row,
                        input.layout.pg_col(hour, gen_idx),
                        1.0,
                    );
                }
                push_triplet(
                    &mut block.triplets,
                    upper_row,
                    cc_z_col(plant, config_idx, hour, input.n_hours),
                    config.big_m_pu - config.p_max_pu,
                );
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = config.big_m_pu;
                local_row += 1;

                let lower_row = input.row_base + local_row;
                for &gen_idx in &config.member_gen_j {
                    push_triplet(
                        &mut block.triplets,
                        lower_row,
                        input.layout.pg_col(hour, gen_idx),
                        -1.0,
                    );
                }
                push_triplet(
                    &mut block.triplets,
                    lower_row,
                    cc_z_col(plant, config_idx, hour, input.n_hours),
                    config.p_min_pu,
                );
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = 0.0;
                local_row += 1;
            }

            if let Some(ramp_up_pu) = config.ramp_up_pu {
                for hour in 1..input.n_hours {
                    let row = input.row_base + local_row;
                    for &gen_idx in &config.member_gen_j {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.pg_col(hour, gen_idx),
                            1.0,
                        );
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.pg_col(hour - 1, gen_idx),
                            -1.0,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_z_col(plant, config_idx, hour, input.n_hours),
                        config.big_m_pu,
                    );
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_z_col(plant, config_idx, hour - 1, input.n_hours),
                        config.big_m_pu,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = ramp_up_pu + 2.0 * config.big_m_pu;
                    local_row += 1;
                }
            }

            if let Some(ramp_down_pu) = config.ramp_down_pu {
                for hour in 1..input.n_hours {
                    let row = input.row_base + local_row;
                    for &gen_idx in &config.member_gen_j {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.pg_col(hour - 1, gen_idx),
                            1.0,
                        );
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.pg_col(hour, gen_idx),
                            -1.0,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_z_col(plant, config_idx, hour, input.n_hours),
                        config.big_m_pu,
                    );
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_z_col(plant, config_idx, hour - 1, input.n_hours),
                        config.big_m_pu,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = ramp_down_pu + 2.0 * config.big_m_pu;
                    local_row += 1;
                }
            }
        }

        for member in &plant.member_gens {
            for hour in 0..input.n_hours {
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.pg_col(hour, member.gen_idx),
                    1.0,
                );
                for &entry_idx in &member.pgcc_entry_indices {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        cc_pgcc_col(plant, entry_idx, hour, input.n_hours),
                        -1.0,
                    );
                }
                block.row_lower[local_row] = 0.0;
                block.row_upper[local_row] = 0.0;
                local_row += 1;
            }
        }

        for (entry_idx, entry) in plant.pgcc_entries.iter().enumerate() {
            for hour in 0..input.n_hours {
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_pgcc_col(plant, entry_idx, hour, input.n_hours),
                    1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    row,
                    cc_z_col(plant, entry.config_idx, hour, input.n_hours),
                    -entry.pmax_pu,
                );
                block.row_lower[local_row] = f64::NEG_INFINITY;
                block.row_upper[local_row] = 0.0;
                local_row += 1;
            }
        }

        for (transition_idx, &(from_config, to_config)) in plant.transition_pairs.iter().enumerate()
        {
            for hour in 0..input.n_hours {
                let ytrans_col = cc_ytrans_col(plant, transition_idx, hour, input.n_hours);

                let ydn_row = input.row_base + local_row;
                push_triplet(&mut block.triplets, ydn_row, ytrans_col, 1.0);
                push_triplet(
                    &mut block.triplets,
                    ydn_row,
                    cc_ydn_col(plant, from_config, hour, input.n_hours),
                    -1.0,
                );
                block.row_lower[local_row] = f64::NEG_INFINITY;
                block.row_upper[local_row] = 0.0;
                local_row += 1;

                let yup_row = input.row_base + local_row;
                push_triplet(&mut block.triplets, yup_row, ytrans_col, 1.0);
                push_triplet(
                    &mut block.triplets,
                    yup_row,
                    cc_yup_col(plant, to_config, hour, input.n_hours),
                    -1.0,
                );
                block.row_lower[local_row] = f64::NEG_INFINITY;
                block.row_upper[local_row] = 0.0;
                local_row += 1;

                let lower_row = input.row_base + local_row;
                push_triplet(&mut block.triplets, lower_row, ytrans_col, 1.0);
                push_triplet(
                    &mut block.triplets,
                    lower_row,
                    cc_ydn_col(plant, from_config, hour, input.n_hours),
                    -1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    lower_row,
                    cc_yup_col(plant, to_config, hour, input.n_hours),
                    -1.0,
                );
                block.row_lower[local_row] = -1.0;
                block.row_upper[local_row] = f64::INFINITY;
                local_row += 1;
            }
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) struct ScucDrActivationLoad {
    pub load_idx: usize,
    pub activation_block_base: usize,
    pub min_duration_periods: usize,
    pub p_sched_pu: f64,
    pub curtailment_range_pu: f64,
}

pub(super) fn dr_activation_rows(
    activation_loads: &[ScucDrActivationLoad],
    n_hours: usize,
) -> usize {
    activation_loads.len() * n_hours
        + activation_loads
            .iter()
            .filter(|load| load.min_duration_periods > 1)
            .count()
            * n_hours
}

pub(super) struct ScucDrActivationRowsInput<'a> {
    pub activation_loads: &'a [ScucDrActivationLoad],
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_dr_activation_rows(input: ScucDrActivationRowsInput<'_>) -> LpBlock {
    let n_rows = dr_activation_rows(input.activation_loads, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for load in input.activation_loads {
        for hour in 0..input.n_hours {
            let row = input.row_base + local_row;
            push_triplet(
                &mut block.triplets,
                row,
                input
                    .layout
                    .col(hour, input.layout.dispatch.dl + load.load_idx),
                1.0,
            );
            push_triplet(
                &mut block.triplets,
                row,
                load.activation_block_base + hour,
                load.curtailment_range_pu,
            );
            block.row_lower[local_row] = load.p_sched_pu;
            block.row_upper[local_row] = BIG_M;
            local_row += 1;
        }
    }

    for load in input.activation_loads {
        if load.min_duration_periods <= 1 {
            continue;
        }
        for hour in 0..input.n_hours {
            let end = (hour + load.min_duration_periods).min(input.n_hours);
            let span = end - hour;
            let row = input.row_base + local_row;
            for next_hour in hour..end {
                push_triplet(
                    &mut block.triplets,
                    row,
                    load.activation_block_base + next_hour,
                    1.0,
                );
            }
            push_triplet(
                &mut block.triplets,
                row,
                load.activation_block_base + hour,
                -(span as f64),
            );
            if hour > 0 {
                push_triplet(
                    &mut block.triplets,
                    row,
                    load.activation_block_base + hour - 1,
                    span as f64,
                );
            }
            block.row_lower[local_row] = 0.0;
            block.row_upper[local_row] = BIG_M;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) struct ScucDrReboundLoad {
    pub load_idx: usize,
    pub original_load_idx: usize,
    pub rebound_block_base: usize,
    pub rebound_fraction: f64,
    pub rebound_periods: usize,
}

pub(super) fn dr_rebound_rows(rebound_loads: &[ScucDrReboundLoad], n_hours: usize) -> usize {
    rebound_loads.len() * n_hours
}

pub(super) struct ScucDrReboundRowsInput<'a> {
    pub rebound_loads: &'a [ScucDrReboundLoad],
    pub dl_list: &'a [&'a DispatchableLoad],
    pub spec: &'a DispatchProblemSpec<'a>,
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_dr_rebound_rows(input: ScucDrReboundRowsInput<'_>) -> LpBlock {
    let n_rows = dr_rebound_rows(input.rebound_loads, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for load in input.rebound_loads {
        let dl = input.dl_list[load.load_idx];
        let coeff = load.rebound_fraction / load.rebound_periods as f64;
        for hour in 0..input.n_hours {
            let row = input.row_base + local_row;
            let window_start = hour.saturating_sub(load.rebound_periods);
            let window_end = hour;

            push_triplet(
                &mut block.triplets,
                row,
                load.rebound_block_base + hour,
                1.0,
            );
            for lookback in window_start..window_end {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input
                        .layout
                        .col(lookback, input.layout.dispatch.dl + load.load_idx),
                    coeff,
                );
            }

            let mut rhs = 0.0;
            for lookback in window_start..window_end {
                let (p_sched_pu, _, _, _, _, _) =
                    crate::common::costs::resolve_dl_for_period_from_spec(
                        load.original_load_idx,
                        lookback,
                        dl,
                        input.spec,
                    );
                rhs += coeff * p_sched_pu;
            }
            block.row_lower[local_row] = rhs;
            block.row_upper[local_row] = rhs;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) fn commitment_policy_rows(
    network: &Network,
    gen_indices: &[usize],
    spec: &DispatchProblemSpec<'_>,
    is_must_run_ext: &[bool],
    da_commitment: Option<&[Vec<bool>]>,
    n_hours: usize,
) -> usize {
    let n_static_must_run = gen_indices
        .iter()
        .enumerate()
        .filter(|&(gen_idx, &gi)| {
            network.generators[gi].is_must_run()
                || is_must_run_ext[gen_idx]
                || network.generators[gi].is_storage()
        })
        .count()
        * n_hours;
    let n_da_commit_rows = da_commitment
        .map(|commitment| {
            let mut count = 0usize;
            for (hour, period) in commitment
                .iter()
                .enumerate()
                .take(n_hours.min(commitment.len()))
            {
                for (gen_idx, &gi) in gen_indices.iter().enumerate() {
                    let generator = &network.generators[gi];
                    let already_must_run = generator.is_must_run()
                        || is_must_run_ext[gen_idx]
                        || generator.is_storage();
                    let implied_by_initial_state =
                        spec.additional_commitment_prefix_through(hour, gen_idx);
                    if !already_must_run
                        && period.get(gen_idx).copied().unwrap_or(false)
                        && !implied_by_initial_state
                    {
                        count += 1;
                    }
                }
            }
            count
        })
        .unwrap_or(0);

    let mut n_max_start_rows = 0usize;
    let mut n_soak_rows = 0usize;
    let mut n_max_up_rows = 0usize;
    let mut n_max_energy_rows = 0usize;
    for (gen_idx, &gi) in gen_indices.iter().enumerate() {
        let generator = &network.generators[gi];
        let commit = generator.commitment.as_ref();
        if commit.and_then(|c| c.max_starts_per_day).is_some() {
            n_max_start_rows += n_hours;
        }
        if commit.and_then(|c| c.max_starts_per_week).is_some() {
            n_max_start_rows += n_hours;
        }

        let soak_hours = commit.and_then(|c| c.min_run_at_pmin_hr).unwrap_or(0.0);
        if soak_hours > 0.0 {
            for startup_hour in 0..n_hours {
                n_soak_rows += spec
                    .hours_to_periods_ceil_from(startup_hour, soak_hours)
                    .min(n_hours.saturating_sub(startup_hour));
            }

            let initially_on = spec.initial_commitment_at(gen_idx).unwrap_or(true);
            if initially_on {
                let h0_hours = spec.initial_online_hours_at(gen_idx).unwrap_or(0.0);
                if h0_hours > 0.0 && h0_hours < soak_hours {
                    n_soak_rows += spec
                        .hours_to_periods_ceil_from(0, soak_hours - h0_hours)
                        .min(n_hours);
                }
            }
        }

        if commit.and_then(|c| c.max_up_time_hr).is_some() {
            n_max_up_rows += n_hours;
        }
        if commit.and_then(|c| c.max_energy_mwh_per_day).is_some() {
            n_max_energy_rows += n_hours;
        }
    }

    let n_startup_window_rows = spec.startup_window_limits.len();
    let n_energy_window_rows = spec
        .energy_window_limits
        .iter()
        .map(|limit| {
            usize::from(limit.min_energy_mwh.is_some())
                + usize::from(limit.max_energy_mwh.is_some())
        })
        .sum::<usize>();

    n_static_must_run
        + n_da_commit_rows
        + n_max_start_rows
        + n_soak_rows
        + n_max_up_rows
        + n_max_energy_rows
        + n_startup_window_rows
        + n_energy_window_rows
}

pub(super) struct ScucCommitmentPolicyRowsInput<'a> {
    pub network: &'a Network,
    pub hourly_networks: &'a [Network],
    pub spec: &'a DispatchProblemSpec<'a>,
    pub gen_indices: &'a [usize],
    pub is_must_run_ext: &'a [bool],
    pub da_commitment: Option<&'a [Vec<bool>]>,
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
    pub base: f64,
    /// Base column index of the multi-interval energy window slack
    /// columns. The energy window row builder consumes this together
    /// with `energy_window_slack_kinds` to attach the slack triplet to
    /// each (window, direction) row. Set to 0 with empty kinds when no
    /// energy windows exist.
    pub energy_window_slack_base: usize,
    pub energy_window_slack_kinds: &'a [super::plan::EnergyWindowSlackKind],
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ScucStartupTierInfo {
    pub lookback_periods: usize,
    pub max_offline_hours: f64,
    pub cost: f64,
}

fn is_forced_offline_hour(
    spec: &DispatchProblemSpec<'_>,
    network: &Network,
    hourly_networks: &[Network],
    base: f64,
    gi: usize,
    hour: usize,
) -> bool {
    let generator = &network.generators[gi];
    let derate_zero = spec.gen_derate_profiles.profiles.iter().any(|profile| {
        profile.generator_id == generator.id
            && hour < profile.derate_factors.len()
            && profile.derate_factors[hour] == 0.0
    });
    if derate_zero {
        return true;
    }
    // A zero hourly pmax (e.g. solar at night, outage expressed via
    // capacity profile rather than derate factor) also makes the unit
    // physically unable to be committed. Honouring this here keeps
    // must-run / DA-commitment enforcement consistent with the
    // physical-pmax-zero pin applied in `scuc::bounds`.
    if generator.is_storage() {
        return false;
    }
    hourly_networks
        .get(hour)
        .map(|net| net.generators[gi].pmax / base <= 1e-9)
        .unwrap_or(false)
}

pub(super) fn build_commitment_policy_rows(input: ScucCommitmentPolicyRowsInput<'_>) -> LpBlock {
    let n_rows = commitment_policy_rows(
        input.network,
        input.gen_indices,
        input.spec,
        input.is_must_run_ext,
        input.da_commitment,
        input.n_hours,
    );
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for (gen_idx, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];
        if !generator.is_must_run() && !input.is_must_run_ext[gen_idx] && !generator.is_storage() {
            continue;
        }
        for hour in 0..input.n_hours {
            if is_forced_offline_hour(
                input.spec,
                input.network,
                input.hourly_networks,
                input.base,
                gi,
                hour,
            ) {
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = BIG_M;
                local_row += 1;
                continue;
            }
            let row = input.row_base + local_row;
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.commitment_col(hour, gen_idx),
                1.0,
            );
            block.row_lower[local_row] = 1.0;
            block.row_upper[local_row] = 1.0;
            local_row += 1;
        }
    }

    if let Some(commitment) = input.da_commitment {
        for (hour, period) in commitment
            .iter()
            .enumerate()
            .take(input.n_hours.min(commitment.len()))
        {
            for (gen_idx, &gi) in input.gen_indices.iter().enumerate() {
                let generator = &input.network.generators[gi];
                let already_must_run = generator.is_must_run()
                    || input.is_must_run_ext[gen_idx]
                    || generator.is_storage();
                if already_must_run {
                    continue;
                }
                if period.get(gen_idx).copied().unwrap_or(false) {
                    let implied_by_initial_state = input
                        .spec
                        .additional_commitment_prefix_through(hour, gen_idx);
                    if implied_by_initial_state {
                        continue;
                    }
                    if is_forced_offline_hour(
                        input.spec,
                        input.network,
                        input.hourly_networks,
                        input.base,
                        gi,
                        hour,
                    ) {
                        block.row_lower[local_row] = -BIG_M;
                        block.row_upper[local_row] = BIG_M;
                        local_row += 1;
                        continue;
                    }
                    let row = input.row_base + local_row;
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.commitment_col(hour, gen_idx),
                        1.0,
                    );
                    block.row_lower[local_row] = 1.0;
                    block.row_upper[local_row] = 1.0;
                    local_row += 1;
                }
            }
        }
    }

    for (gen_idx, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];

        if let Some(max_starts_day) = generator
            .commitment
            .as_ref()
            .and_then(|c| c.max_starts_per_day)
        {
            let pre_starts = input.spec.initial_starts_24h_at(gen_idx).unwrap_or(0);
            for hour in 0..input.n_hours {
                let window = input.spec.lookback_periods_covering(hour, 24.0).max(1);
                let window_start = (hour + 1).saturating_sub(window);
                let rhs = if input.spec.hours_between(0, hour + 1) + 1e-9 < 24.0 {
                    (max_starts_day as i64 - pre_starts as i64).max(0) as f64
                } else {
                    max_starts_day as f64
                };
                let row = input.row_base + local_row;
                for lookback in window_start..=hour {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.startup_col(lookback, gen_idx),
                        1.0,
                    );
                }
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = rhs;
                local_row += 1;
            }
        }

        if let Some(max_starts_week) = generator
            .commitment
            .as_ref()
            .and_then(|c| c.max_starts_per_week)
        {
            let pre_starts = input.spec.initial_starts_168h_at(gen_idx).unwrap_or(0);
            for hour in 0..input.n_hours {
                let window = input.spec.lookback_periods_covering(hour, 168.0).max(1);
                let window_start = (hour + 1).saturating_sub(window);
                let rhs = if input.spec.hours_between(0, hour + 1) + 1e-9 < 168.0 {
                    (max_starts_week as i64 - pre_starts as i64).max(0) as f64
                } else {
                    max_starts_week as f64
                };
                let row = input.row_base + local_row;
                for lookback in window_start..=hour {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.startup_col(lookback, gen_idx),
                        1.0,
                    );
                }
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = rhs;
                local_row += 1;
            }
        }
    }

    for (gen_idx, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];
        let soak_hours = generator
            .commitment
            .as_ref()
            .and_then(|c| c.min_run_at_pmin_hr)
            .unwrap_or(0.0);
        if soak_hours <= 0.0 {
            continue;
        }

        for startup_hour in 0..input.n_hours {
            let end = (startup_hour
                + input
                    .spec
                    .hours_to_periods_ceil_from(startup_hour, soak_hours))
            .min(input.n_hours);
            for hour in startup_hour..end {
                if is_forced_offline_hour(
                    input.spec,
                    input.network,
                    input.hourly_networks,
                    input.base,
                    gi,
                    hour,
                ) {
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = BIG_M;
                    local_row += 1;
                    continue;
                }
                let pmax_pu = input.hourly_networks[hour].generators[gi].pmax / input.base;
                let pmin_pu = input.hourly_networks[hour].generators[gi].pmin / input.base;
                let delta_pu = pmax_pu - pmin_pu;
                let row = input.row_base + local_row;
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.pg_col(hour, gen_idx),
                    1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.commitment_col(hour, gen_idx),
                    -pmax_pu,
                );
                if delta_pu > 1e-12 {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.startup_col(startup_hour, gen_idx),
                        delta_pu,
                    );
                }
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = 0.0;
                local_row += 1;
            }
        }

        let initially_on = input.spec.initial_commitment_at(gen_idx).unwrap_or(true);
        if initially_on {
            let h0_hours = input.spec.initial_online_hours_at(gen_idx).unwrap_or(0.0);
            if h0_hours > 0.0 && h0_hours < soak_hours {
                let remaining = input
                    .spec
                    .hours_to_periods_ceil_from(0, soak_hours - h0_hours);
                for hour in 0..remaining.min(input.n_hours) {
                    if is_forced_offline_hour(
                        input.spec,
                        input.network,
                        input.hourly_networks,
                        input.base,
                        gi,
                        hour,
                    ) {
                        block.row_lower[local_row] = -BIG_M;
                        block.row_upper[local_row] = BIG_M;
                        local_row += 1;
                        continue;
                    }
                    let pmin_pu = input.hourly_networks[hour].generators[gi].pmin / input.base;
                    let row = input.row_base + local_row;
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.pg_col(hour, gen_idx),
                        1.0,
                    );
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.commitment_col(hour, gen_idx),
                        -pmin_pu,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = 0.0;
                    local_row += 1;
                }
            }
        }
    }

    for (gen_idx, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];
        if let Some(max_up_hours) = generator.commitment.as_ref().and_then(|c| c.max_up_time_hr) {
            let max_up_period_budget = input.spec.hours_to_periods_ceil(max_up_hours);
            let initially_on = input.spec.initial_commitment_at(gen_idx).unwrap_or(true);
            let initial_on_hours = if initially_on {
                input.spec.initial_online_hours_at(gen_idx).unwrap_or(0.0)
            } else {
                0.0
            };

            for hour in 0..input.n_hours {
                let history_window = if hour == 0 {
                    0
                } else {
                    input.spec.lookback_periods_covering(hour - 1, max_up_hours)
                };
                let window_start = hour.saturating_sub(history_window);
                let remaining_pre_hours =
                    (max_up_hours - input.spec.hours_between(0, hour)).max(0.0);
                let pre_on_periods = if initially_on && remaining_pre_hours > 0.0 {
                    input
                        .spec
                        .hours_to_periods_ceil(remaining_pre_hours.min(initial_on_hours))
                } else {
                    0
                };
                let rhs = (max_up_period_budget as i64 - pre_on_periods as i64).max(0) as f64;
                let row = input.row_base + local_row;
                for lookback in window_start..=hour {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.commitment_col(lookback, gen_idx),
                        1.0,
                    );
                }
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = rhs;
                local_row += 1;
            }
        }
    }

    for (gen_idx, &gi) in input.gen_indices.iter().enumerate() {
        let generator = &input.network.generators[gi];
        let Some(max_energy_mwh) = generator
            .commitment
            .as_ref()
            .and_then(|c| c.max_energy_mwh_per_day)
        else {
            continue;
        };
        let pre_energy = input.spec.initial_energy_mwh_24h_at(gen_idx).unwrap_or(0.0);
        for hour in 0..input.n_hours {
            let window = input.spec.lookback_periods_covering(hour, 24.0).max(1);
            let window_start = (hour + 1).saturating_sub(window);
            let rhs = if input.spec.hours_between(0, hour + 1) + 1e-9 < 24.0 {
                ((max_energy_mwh - pre_energy).max(0.0)) / input.base
            } else {
                max_energy_mwh / input.base
            };
            let row = input.row_base + local_row;
            for lookback in window_start..=hour {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.pg_col(lookback, gen_idx),
                    input.spec.period_hours(lookback),
                );
            }
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = rhs;
            local_row += 1;
        }
    }

    for limit in input.spec.startup_window_limits {
        let row = input.row_base + local_row;
        for hour in limit.start_period_idx..=limit.end_period_idx {
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.startup_col(hour, limit.gen_index),
                1.0,
            );
        }
        block.row_lower[local_row] = -BIG_M;
        block.row_upper[local_row] = limit.max_startups as f64;
        local_row += 1;
    }

    // Build a fast lookup from (limit_idx, direction) → slack column.
    // The kinds vector is order-stable with the iteration order below
    // (one min row first if present, then one max row), so a forward
    // index walk would also work, but the explicit lookup makes the row
    // builder robust to any future reordering of `energy_window_slack_kinds`.
    use super::plan::{EnergyWindowSlackDirection, EnergyWindowSlackKind};
    let slack_col_for = |limit_idx: usize, dir: EnergyWindowSlackDirection| -> Option<usize> {
        input
            .energy_window_slack_kinds
            .iter()
            .position(|k: &EnergyWindowSlackKind| k.limit_idx == limit_idx && k.direction == dir)
            .map(|pos| input.energy_window_slack_base + pos)
    };

    for (limit_idx, limit) in input.spec.energy_window_limits.iter().enumerate() {
        let build_energy_row = |block: &mut LpBlock,
                                local_row: &mut usize,
                                lower: f64,
                                upper: f64,
                                slack_col: Option<usize>,
                                slack_coeff: f64| {
            let row = input.row_base + *local_row;
            for hour in limit.start_period_idx..=limit.end_period_idx {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.pg_col(hour, limit.gen_index),
                    input.spec.period_hours(hour),
                );
            }
            if let Some(col) = slack_col {
                push_triplet(&mut block.triplets, row, col, slack_coeff);
            }
            block.row_lower[*local_row] = lower;
            block.row_upper[*local_row] = upper;
            *local_row += 1;
        };

        if let Some(min_energy_mwh) = limit.min_energy_mwh {
            // Minimum-energy window: Σ dt × pg + e^+ ≥ emin / base.
            // The slack is added to the LHS with coefficient +1, so the
            // LP can pay the penalty by raising e^+ instead of forcing
            // infeasibility on a tight energy floor.
            let slack_col = slack_col_for(limit_idx, EnergyWindowSlackDirection::Min);
            build_energy_row(
                &mut block,
                &mut local_row,
                min_energy_mwh / input.base,
                BIG_M,
                slack_col,
                1.0,
            );
        }
        if let Some(max_energy_mwh) = limit.max_energy_mwh {
            // Maximum-energy window: Σ dt × pg − e^+ ≤ emax / base.
            // The slack is added to the LHS with coefficient −1.
            let slack_col = slack_col_for(limit_idx, EnergyWindowSlackDirection::Max);
            build_energy_row(
                &mut block,
                &mut local_row,
                -BIG_M,
                max_energy_mwh / input.base,
                slack_col,
                -1.0,
            );
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

// ----- AC branch on/off state evolution + simul start/stop ban -----
//
// Per AC branch (members of `J^pr,cs,ac`):
//
//     u^on_jt − u^on_{j,t-1} = u^su_jt − u^sd_jt           (interior periods)
//     u^on_j0 − u^on,0_j     = u^su_j0 − u^sd_j0           (first period)
//     u^su_jt + u^sd_jt ≤ 1                                (no simultaneous transition)
//
// Emitted only when `allow_branch_switching = true`. In SW0 the
// branch-state binary columns (`u^on`, `u^su`, `u^sd`) are not
// allocated by the layout, so these rows have nothing to reference —
// they'd collapse to `0 = 0` / `0 ≤ 1` anyway. Skipping them up
// front saves `~2 × n_ac_branches × n_hours` rows plus ~5× that in
// triplets on every SCUC build.

/// Number of branch state-evolution rows the SCUC LP needs.
///
/// `2 × n_ac_branches × n_hours` when branch switching is allowed
/// (one state-evolution equality and one simultaneous-transition
/// inequality per branch per period), zero otherwise.
pub(super) fn branch_state_rows_count(
    network: &Network,
    n_hours: usize,
    allow_branch_switching: bool,
) -> usize {
    if !allow_branch_switching {
        return 0;
    }
    2 * network.branches.len() * n_hours
}

pub(super) struct ScucBranchStateRowsInput<'a> {
    pub network: &'a Network,
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
    pub allow_branch_switching: bool,
}

pub(super) fn build_branch_state_rows(input: ScucBranchStateRowsInput<'_>) -> LpBlock {
    let n_branches = input.network.branches.len();
    let n_rows =
        branch_state_rows_count(input.network, input.n_hours, input.allow_branch_switching);
    if n_rows == 0 {
        return LpBlock::empty();
    }
    let mut block = LpBlock {
        triplets: Vec::with_capacity(5 * n_rows),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for branch_local_idx in 0..n_branches {
        let initial_on = if input.network.branches[branch_local_idx].in_service {
            1.0
        } else {
            0.0
        };

        for hour in 0..input.n_hours {
            // ── State evolution row (eqs 53-54) ──────────────────────
            // u^on_t − u^on_{t-1} − u^su_t + u^sd_t = 0    (interior)
            // u^on_0 − u^on,0 − u^su_0 + u^sd_0 = 0        (first period)
            let row = input.row_base + local_row;
            let bc_t = input.layout.branch_commitment_col(hour, branch_local_idx);
            let bs_t = input.layout.branch_startup_col(hour, branch_local_idx);
            let bd_t = input.layout.branch_shutdown_col(hour, branch_local_idx);
            push_triplet(&mut block.triplets, row, bc_t, 1.0);
            push_triplet(&mut block.triplets, row, bs_t, -1.0);
            push_triplet(&mut block.triplets, row, bd_t, 1.0);
            if hour == 0 {
                // Equality `u^on_0 = u^on,0 + u^su_0 - u^sd_0`
                // rewritten as `u^on_0 - u^su_0 + u^sd_0 = u^on,0`.
                block.row_lower[local_row] = initial_on;
                block.row_upper[local_row] = initial_on;
            } else {
                let bc_prev = input
                    .layout
                    .branch_commitment_col(hour - 1, branch_local_idx);
                push_triplet(&mut block.triplets, row, bc_prev, -1.0);
                block.row_lower[local_row] = 0.0;
                block.row_upper[local_row] = 0.0;
            }
            local_row += 1;

            // ── Simultaneous start/stop ban (eq 55) ──────────────────
            // u^su_t + u^sd_t ≤ 1
            let row = input.row_base + local_row;
            push_triplet(&mut block.triplets, row, bs_t, 1.0);
            push_triplet(&mut block.triplets, row, bd_t, 1.0);
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = 1.0;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

/// Number of Big-M branch flow definition rows. Four rows per AC
/// branch per period when switching is enabled, zero otherwise.
pub(super) fn branch_flow_definition_rows_count(
    network: &Network,
    n_hours: usize,
    allow_branch_switching: bool,
) -> usize {
    if !allow_branch_switching {
        return 0;
    }
    4 * network.branches.len() * n_hours
}

pub(super) struct ScucBranchFlowDefinitionRowsInput<'a> {
    pub network: &'a Network,
    pub layout: &'a ScucLayout,
    pub bus_map: &'a HashMap<u32, usize>,
    pub n_hours: usize,
    pub row_base: usize,
    pub base_mva: f64,
    pub big_m_factor: f64,
}

/// Build the four-row Big-M switchable branch flow definition family.
///
/// For every AC branch `l` and period `t`, four rows are emitted:
///
/// ```text
/// (1)   pf_l_t − b·(θ_from_t − θ_to_t) + M·u^on_lt ≤  M
/// (2)   pf_l_t − b·(θ_from_t − θ_to_t) − M·u^on_lt ≥ −M
/// (3)   pf_l_t − fmax_l·u^on_lt ≤ 0
/// (4)   pf_l_t + fmax_l·u^on_lt ≥ 0
/// ```
///
/// where `M = big_m_factor × fmax_l` (in per-unit) per branch. Rows
/// (1)-(2) tie `pf_l` to the angle difference when the branch is on
/// (`u^on = 1`) and leave it unconstrained to a Big-M envelope when
/// off (`u^on = 0`). Rows (3)-(4) force `pf_l = 0` when off and
/// `|pf_l| ≤ fmax` when on, exactly matching the branch thermal
/// limit. Together they are the linearized equivalent of the
/// product `u^on · b · Δθ` that appears in eqs (148)-(149).
///
/// The row layout is period-outer, branch-inner:
///   `local_row = 4 * (hour * n_branches + branch_local_idx) + k`
/// where `k ∈ {0, 1, 2, 3}` selects the four rows above. Callers
/// pin `row_base` to the block's first row.
pub(super) fn build_branch_flow_definition_rows(
    input: ScucBranchFlowDefinitionRowsInput<'_>,
) -> LpBlock {
    let n_branches = input.network.branches.len();
    let n_rows = branch_flow_definition_rows_count(input.network, input.n_hours, true);
    if n_rows == 0 {
        return LpBlock::empty();
    }
    let mut block = LpBlock {
        triplets: Vec::with_capacity(6 * n_rows),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let big_m_factor = input.big_m_factor.max(1.0);
    let mut local_row = 0usize;

    for hour in 0..input.n_hours {
        for branch_local_idx in 0..n_branches {
            let branch = &input.network.branches[branch_local_idx];
            // Degenerate zero-impedance branches can't define a
            // meaningful flow variable — leave all four rows trivially
            // satisfied so the LP layout stays consistent.
            if branch.x.abs() < 1e-20 {
                for _k in 0..4 {
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = BIG_M;
                    local_row += 1;
                }
                continue;
            }
            let b_val = branch.b_dc();
            let fmax_pu = branch.rating_a_mva.max(0.0) / input.base_mva;
            let big_m = big_m_factor * fmax_pu;

            let from = input.bus_map[&branch.from_bus];
            let to = input.bus_map[&branch.to_bus];
            let pf_col = input.layout.branch_flow_col(hour, branch_local_idx);
            let theta_from_col = input.layout.theta_col(hour, from);
            let theta_to_col = input.layout.theta_col(hour, to);
            let uon_col = input.layout.branch_commitment_col(hour, branch_local_idx);

            // (1) pf_l − b·θ_from + b·θ_to + M·u^on ≤ M
            let row = input.row_base + local_row;
            push_triplet(&mut block.triplets, row, pf_col, 1.0);
            push_triplet(&mut block.triplets, row, theta_from_col, -b_val);
            push_triplet(&mut block.triplets, row, theta_to_col, b_val);
            push_triplet(&mut block.triplets, row, uon_col, big_m);
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = big_m;
            local_row += 1;

            // (2) pf_l − b·θ_from + b·θ_to − M·u^on ≥ −M
            let row = input.row_base + local_row;
            push_triplet(&mut block.triplets, row, pf_col, 1.0);
            push_triplet(&mut block.triplets, row, theta_from_col, -b_val);
            push_triplet(&mut block.triplets, row, theta_to_col, b_val);
            push_triplet(&mut block.triplets, row, uon_col, -big_m);
            block.row_lower[local_row] = -big_m;
            block.row_upper[local_row] = BIG_M;
            local_row += 1;

            // (3) pf_l − fmax·u^on ≤ 0
            let row = input.row_base + local_row;
            push_triplet(&mut block.triplets, row, pf_col, 1.0);
            push_triplet(&mut block.triplets, row, uon_col, -fmax_pu);
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = 0.0;
            local_row += 1;

            // (4) pf_l + fmax·u^on ≥ 0
            let row = input.row_base + local_row;
            push_triplet(&mut block.triplets, row, pf_col, 1.0);
            push_triplet(&mut block.triplets, row, uon_col, fmax_pu);
            block.row_lower[local_row] = 0.0;
            block.row_upper[local_row] = BIG_M;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) struct ScucUnitIntertemporalGen<'a> {
    pub gen_idx: usize,
    pub min_up_periods_by_hour: Vec<usize>,
    pub min_down_periods_by_hour: Vec<usize>,
    pub forced_offline_hours: Vec<bool>,
    pub startup_delta_local_off: usize,
    pub use_deloading_limits: bool,
    pub startup_tiers_by_hour: &'a [Vec<ScucStartupTierInfo>],
    pub pre_horizon_offline_hours: Option<f64>,
    pub elapsed_horizon_hours_before_by_hour: Vec<f64>,
    pub ramp_up_limit_pu_by_hour: Vec<Option<f64>>,
    pub ramp_down_limit_pu_by_hour: Vec<Option<f64>>,
    pub startup_ramp_limit_pu_by_hour: Vec<Option<f64>>,
    pub shutdown_ramp_limit_pu_by_hour: Vec<Option<f64>>,
    pub pmax_pu: f64,
    /// Static generator pmin (per-unit). Paired with `pmax_pu` so the
    /// ramp-row builder can pre-screen structurally-slack rows — a
    /// ramp limit at or above the full dispatch range `pmax - pmin`
    /// cannot bind, so the row is dropped from the MIP.
    pub pmin_pu: f64,
    pub initial_commitment: Option<bool>,
    pub initial_dispatch_pu: Option<f64>,
}

/// A ramp row is structurally slack when every reachable dispatch
/// delta fits inside the allowed ramp, so the row is never binding.
///
/// Steady-state delta (unit stays on): bounded by `pmax - pmin`, which
/// is `pmax` for units with `pmin <= 0` (storage charging, demand-side
/// resources, dispatchable renewables) or `pmax - pmin` for thermal
/// units with a strict pmin floor. The form `pmax - pmin` is valid
/// for both signs as long as `pmin <= pmax`, since `pmax - pmin` is
/// non-negative and equals the physical dispatch span.
///
/// Transition delta (unit sees a startup or shutdown): bounded by
/// `pmax - pmin` as well — when the unit goes off `pg` is driven to
/// 0, and `0 - pmax` = `-pmax` is what the row must accommodate on
/// shutdown, while `pmax - 0 = pmax` is what ramp-up must accommodate
/// on startup. For the non-startup-coupled row this is the
/// `startup_or_shutdown_limit_pu` comparand; we require it dominate
/// `pmax - pmin` too so both reachable deltas remain non-binding.
fn ramp_row_is_structurally_slack(
    ramp_limit_pu: f64,
    startup_or_shutdown_limit_pu: f64,
    pmax_pu: f64,
    pmin_pu: f64,
) -> bool {
    let range_pu = (pmax_pu - pmin_pu).max(pmax_pu).max(0.0);
    ramp_limit_pu + 1e-12 >= range_pu && startup_or_shutdown_limit_pu + 1e-12 >= range_pu
}

pub(super) fn unit_intertemporal_rows(
    units: &[ScucUnitIntertemporalGen<'_>],
    n_hours: usize,
) -> usize {
    units
        .iter()
        .map(|unit| {
            let mut rows = 0usize;
            rows += unit
                .min_up_periods_by_hour
                .iter()
                .take(n_hours)
                .filter(|&&periods| periods > 1)
                .count();
            rows += unit
                .min_down_periods_by_hour
                .iter()
                .take(n_hours)
                .filter(|&&periods| periods > 1)
                .count();
            rows += unit
                .startup_tiers_by_hour
                .iter()
                .map(Vec::len)
                .sum::<usize>();
            rows += 2 * n_hours.saturating_sub(1);
            let initial_on = unit
                .initial_commitment
                .unwrap_or(unit.initial_dispatch_pu.unwrap_or(0.0) > 1e-9);
            rows += usize::from(unit.initial_dispatch_pu.is_some());
            rows += usize::from(unit.initial_dispatch_pu.is_some() && initial_on);
            rows
        })
        .sum()
}

pub(super) struct ScucUnitIntertemporalRowsInput<'a> {
    pub units: &'a [ScucUnitIntertemporalGen<'a>],
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_unit_intertemporal_rows(input: ScucUnitIntertemporalRowsInput<'_>) -> LpBlock {
    let n_rows = unit_intertemporal_rows(input.units, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for unit in input.units {
        for hour in 0..input.n_hours {
            let min_up_periods = unit.min_up_periods_by_hour.get(hour).copied().unwrap_or(0);
            if min_up_periods <= 1 {
                continue;
            }
            let active_end = unit.forced_offline_hours[hour..]
                .iter()
                .position(|forced| *forced)
                .map(|offset| hour + offset)
                .unwrap_or(input.n_hours);
            let end = (hour + min_up_periods).min(active_end).min(input.n_hours);
            let span = end - hour;
            if span > 0 {
                let row = input.row_base + local_row;
                for next_hour in hour..end {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.commitment_col(next_hour, unit.gen_idx),
                        1.0,
                    );
                }
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.startup_col(hour, unit.gen_idx),
                    -(span as f64),
                );
                block.row_lower[local_row] = 0.0;
                block.row_upper[local_row] = BIG_M;
            } else {
                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = BIG_M;
            }
            local_row += 1;
        }
    }

    for unit in input.units {
        for hour in 0..input.n_hours {
            let min_down_periods = unit
                .min_down_periods_by_hour
                .get(hour)
                .copied()
                .unwrap_or(0);
            if min_down_periods <= 1 {
                continue;
            }
            let end = (hour + min_down_periods).min(input.n_hours);
            let span = end - hour;
            let row = input.row_base + local_row;
            for next_hour in hour..end {
                push_triplet(
                    &mut block.triplets,
                    row,
                    input.layout.commitment_col(next_hour, unit.gen_idx),
                    1.0,
                );
            }
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.shutdown_col(hour, unit.gen_idx),
                span as f64,
            );
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = span as f64;
            local_row += 1;
        }
    }

    for hour in 0..input.n_hours {
        for unit in input.units {
            let startup_tiers = &unit.startup_tiers_by_hour[hour];
            if startup_tiers.is_empty() {
                continue;
            }

            let partition_row = input.row_base + local_row;
            for tier_idx in 0..startup_tiers.len() {
                push_triplet(
                    &mut block.triplets,
                    partition_row,
                    input.layout.col(
                        hour,
                        input.layout.startup_delta + unit.startup_delta_local_off + tier_idx,
                    ),
                    1.0,
                );
            }
            push_triplet(
                &mut block.triplets,
                partition_row,
                input.layout.startup_col(hour, unit.gen_idx),
                -1.0,
            );
            block.row_lower[local_row] = 0.0;
            block.row_upper[local_row] = 0.0;
            local_row += 1;

            for (tier_idx, tier) in startup_tiers
                .iter()
                .take(startup_tiers.len().saturating_sub(1))
                .enumerate()
            {
                let lookback_periods = tier.lookback_periods;
                let row = input.row_base + local_row;
                for eligible_tier in 0..=tier_idx {
                    push_triplet(
                        &mut block.triplets,
                        row,
                        input.layout.col(
                            hour,
                            input.layout.startup_delta
                                + unit.startup_delta_local_off
                                + eligible_tier,
                        ),
                        1.0,
                    );
                }

                let mut rhs = 0.0;
                for lookback in 1..=lookback_periods {
                    let prior_hour = hour as i64 - lookback as i64;
                    if prior_hour >= 0 {
                        push_triplet(
                            &mut block.triplets,
                            row,
                            input.layout.shutdown_col(prior_hour as usize, unit.gen_idx),
                            -1.0,
                        );
                    }
                }

                if let Some(initial_offline_hours) = unit.pre_horizon_offline_hours {
                    let offline_hours_before_startup = initial_offline_hours
                        + unit
                            .elapsed_horizon_hours_before_by_hour
                            .get(hour)
                            .copied()
                            .unwrap_or(0.0);
                    if offline_hours_before_startup <= tier.max_offline_hours + 1e-9 {
                        rhs += 1.0;
                    }
                }

                block.row_lower[local_row] = -BIG_M;
                block.row_upper[local_row] = rhs;
                local_row += 1;
            }
        }
    }

    if input.n_hours > 1 {
        for unit in input.units {
            for hour in 1..input.n_hours {
                // Ramp-up row (hour > 0). Skip entirely when the unit's
                // per-period ramp-up limit can span its full dispatch
                // range — the constraint cannot bind and emitting it
                // only adds triplets and a row for presolve to peel
                // away. Deloading-mode startup trajectories are
                // unaffected: when `use_deloading_limits = true` and
                // the startup-ramp limit differs from the normal ramp,
                // the coupling-via-commitment term keeps the row live
                // on startup transitions, so we skip only when the
                // plain ramp limit itself is already wide enough.
                let ramp_up_limit_pu = unit.ramp_up_limit_pu_by_hour.get(hour).copied().flatten();
                let ramp_up_startup_pu = unit
                    .startup_ramp_limit_pu_by_hour
                    .get(hour)
                    .copied()
                    .flatten()
                    .or(ramp_up_limit_pu);
                let ramp_up_slack = ramp_up_limit_pu.is_some_and(|lim| {
                    ramp_up_startup_pu.is_some_and(|start_lim| {
                        ramp_row_is_structurally_slack(lim, start_lim, unit.pmax_pu, unit.pmin_pu)
                    })
                });
                let ramp_up_row = input.row_base + local_row;
                if !ramp_up_slack {
                    push_triplet(
                        &mut block.triplets,
                        ramp_up_row,
                        input.layout.pg_col(hour, unit.gen_idx),
                        1.0,
                    );
                    push_triplet(
                        &mut block.triplets,
                        ramp_up_row,
                        input.layout.pg_col(hour - 1, unit.gen_idx),
                        -1.0,
                    );
                }
                if let Some(ramp_up_limit_pu) = ramp_up_limit_pu
                    && !ramp_up_slack
                {
                    let startup_limit_pu = if unit.use_deloading_limits {
                        unit.startup_ramp_limit_pu_by_hour
                            .get(hour)
                            .copied()
                            .flatten()
                            .unwrap_or(ramp_up_limit_pu)
                    } else {
                        ramp_up_limit_pu
                    };
                    let online_delta_pu = ramp_up_limit_pu - startup_limit_pu;
                    if online_delta_pu.abs() > 1e-12 {
                        push_triplet(
                            &mut block.triplets,
                            ramp_up_row,
                            input.layout.commitment_col(hour, unit.gen_idx),
                            -online_delta_pu,
                        );
                        push_triplet(
                            &mut block.triplets,
                            ramp_up_row,
                            input.layout.startup_col(hour, unit.gen_idx),
                            online_delta_pu,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        ramp_up_row,
                        input.layout.ramp_up_slack_col(hour, unit.gen_idx),
                        -1.0,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = startup_limit_pu;
                } else {
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = BIG_M;
                }
                local_row += 1;

                let ramp_down_limit_pu =
                    unit.ramp_down_limit_pu_by_hour.get(hour).copied().flatten();
                let ramp_down_shutdown_pu = unit
                    .shutdown_ramp_limit_pu_by_hour
                    .get(hour)
                    .copied()
                    .flatten()
                    .or(ramp_down_limit_pu);
                let ramp_down_slack = ramp_down_limit_pu.is_some_and(|lim| {
                    ramp_down_shutdown_pu.is_some_and(|shut_lim| {
                        ramp_row_is_structurally_slack(lim, shut_lim, unit.pmax_pu, unit.pmin_pu)
                    })
                });
                let ramp_down_row = input.row_base + local_row;
                if !ramp_down_slack {
                    push_triplet(
                        &mut block.triplets,
                        ramp_down_row,
                        input.layout.pg_col(hour - 1, unit.gen_idx),
                        1.0,
                    );
                    push_triplet(
                        &mut block.triplets,
                        ramp_down_row,
                        input.layout.pg_col(hour, unit.gen_idx),
                        -1.0,
                    );
                }
                if let Some(ramp_down_limit_pu) = ramp_down_limit_pu
                    && !ramp_down_slack
                {
                    let shutdown_limit_pu = if unit.use_deloading_limits {
                        unit.shutdown_ramp_limit_pu_by_hour
                            .get(hour)
                            .copied()
                            .flatten()
                            .unwrap_or(ramp_down_limit_pu)
                    } else {
                        ramp_down_limit_pu
                    };
                    let online_delta_pu = ramp_down_limit_pu - shutdown_limit_pu;
                    if online_delta_pu.abs() > 1e-12 {
                        push_triplet(
                            &mut block.triplets,
                            ramp_down_row,
                            input.layout.commitment_col(hour, unit.gen_idx),
                            -online_delta_pu,
                        );
                    }
                    push_triplet(
                        &mut block.triplets,
                        ramp_down_row,
                        input.layout.ramp_down_slack_col(hour, unit.gen_idx),
                        -1.0,
                    );
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = shutdown_limit_pu;
                } else {
                    block.row_lower[local_row] = -BIG_M;
                    block.row_upper[local_row] = BIG_M;
                }
                local_row += 1;
            }
        }
    }

    for unit in input.units {
        let Some(initial_dispatch_pu) = unit.initial_dispatch_pu else {
            continue;
        };
        let initial_on = unit
            .initial_commitment
            .unwrap_or(initial_dispatch_pu > 1e-9);

        let ramp_up_row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            ramp_up_row,
            input.layout.pg_col(0, unit.gen_idx),
            1.0,
        );
        if let Some(ramp_up_limit_pu) = unit.ramp_up_limit_pu_by_hour.first().copied().flatten() {
            let startup_limit_pu = if unit.use_deloading_limits {
                unit.startup_ramp_limit_pu_by_hour
                    .first()
                    .copied()
                    .flatten()
                    .unwrap_or(ramp_up_limit_pu)
            } else {
                ramp_up_limit_pu
            };
            let online_delta_pu = ramp_up_limit_pu - startup_limit_pu;
            if online_delta_pu.abs() > 1e-12 {
                push_triplet(
                    &mut block.triplets,
                    ramp_up_row,
                    input.layout.commitment_col(0, unit.gen_idx),
                    -online_delta_pu,
                );
                push_triplet(
                    &mut block.triplets,
                    ramp_up_row,
                    input.layout.startup_col(0, unit.gen_idx),
                    online_delta_pu,
                );
            }
            push_triplet(
                &mut block.triplets,
                ramp_up_row,
                input.layout.ramp_up_slack_col(0, unit.gen_idx),
                -1.0,
            );
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = initial_dispatch_pu + startup_limit_pu;
        } else {
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = BIG_M;
        }
        local_row += 1;

        if !initial_on {
            continue;
        }

        let ramp_down_row = input.row_base + local_row;
        push_triplet(
            &mut block.triplets,
            ramp_down_row,
            input.layout.pg_col(0, unit.gen_idx),
            -1.0,
        );
        if let Some(ramp_down_limit_pu) = unit.ramp_down_limit_pu_by_hour.first().copied().flatten()
        {
            let shutdown_limit_pu = if unit.use_deloading_limits {
                unit.shutdown_ramp_limit_pu_by_hour
                    .first()
                    .copied()
                    .flatten()
                    .unwrap_or(ramp_down_limit_pu)
            } else {
                ramp_down_limit_pu
            };
            let online_delta_pu = ramp_down_limit_pu - shutdown_limit_pu;
            if online_delta_pu.abs() > 1e-12 {
                push_triplet(
                    &mut block.triplets,
                    ramp_down_row,
                    input.layout.commitment_col(0, unit.gen_idx),
                    -online_delta_pu,
                );
            }
            push_triplet(
                &mut block.triplets,
                ramp_down_row,
                input.layout.ramp_down_slack_col(0, unit.gen_idx),
                -1.0,
            );
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = shutdown_limit_pu - initial_dispatch_pu;
        } else {
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = BIG_M;
        }
        local_row += 1;
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

/// Intertemporal ramp constraint on an aggregate of dispatchable-load blocks.
///
/// Market models that split one physical consumer into multiple
/// price-block DLs carry the ramp-rate limit at the consumer level,
/// not per block. The SCUC/SCED LP therefore needs to constrain the
/// *sum* of served MW across all blocks in the group against the
/// shared ramp rate: the LP is free to move individual blocks however
/// it likes as long as the total stays within the ramp window.
pub(super) struct ScucDlRampGroup {
    /// Indices into `spec.dispatchable_loads` / `dl_list` for each block in
    /// this group. All blocks must belong to the same physical consumer.
    pub member_load_indices: Vec<usize>,
    /// Upward ramp rate of total served real power (pu per hour).
    pub ramp_up_pu_per_hr: f64,
    /// Downward ramp rate of total served real power (pu per hour).
    pub ramp_down_pu_per_hr: f64,
    /// Prior-horizon served real power for the whole consumer (pu). When
    /// `Some`, the LP adds a first-period anchor row. When `None`, only the
    /// period-to-period transitions are constrained.
    pub initial_p_pu: Option<f64>,
}

pub(super) fn dl_ramp_group_rows(groups: &[ScucDlRampGroup], n_hours: usize) -> usize {
    // Per group:
    //   * 2 transition rows (ramp up / ramp down) for every period t >= 1
    //   * 2 initial-anchor rows at t = 0 when `initial_p_pu` is set
    //
    // Empty groups (member_load_indices is empty) contribute zero rows.
    let mut total = 0usize;
    for group in groups {
        if group.member_load_indices.is_empty() {
            continue;
        }
        total += 2 * n_hours.saturating_sub(1);
        if group.initial_p_pu.is_some() {
            total += 2;
        }
    }
    total
}

pub(super) struct ScucDlRampGroupRowsInput<'a> {
    pub groups: &'a [ScucDlRampGroup],
    pub layout: &'a ScucLayout,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_dl_ramp_group_rows(input: ScucDlRampGroupRowsInput<'_>) -> LpBlock {
    let n_rows = dl_ramp_group_rows(input.groups, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for group in input.groups {
        if group.member_load_indices.is_empty() {
            continue;
        }

        // Initial-period anchor: Σ p_served[0] within [initial_p ± ramp * dt[0]].
        if let Some(initial_p_pu) = group.initial_p_pu {
            let dt0 = input.spec.period_hours(0);
            let up_limit = group.ramp_up_pu_per_hr * dt0;
            let down_limit = group.ramp_down_pu_per_hr * dt0;

            // Row 1: Σ p_served[0] ≤ initial_p + ramp_up * dt[0]
            let row_up = input.row_base + local_row;
            for &load_idx in &group.member_load_indices {
                push_triplet(
                    &mut block.triplets,
                    row_up,
                    input.layout.col(0, input.layout.dispatch.dl + load_idx),
                    1.0,
                );
            }
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = initial_p_pu + up_limit;
            local_row += 1;

            // Row 2: Σ p_served[0] ≥ max(0, initial_p − ramp_down * dt[0])
            let row_down = input.row_base + local_row;
            for &load_idx in &group.member_load_indices {
                push_triplet(
                    &mut block.triplets,
                    row_down,
                    input.layout.col(0, input.layout.dispatch.dl + load_idx),
                    1.0,
                );
            }
            block.row_lower[local_row] = (initial_p_pu - down_limit).max(0.0);
            block.row_upper[local_row] = BIG_M;
            local_row += 1;
        }

        // Period-to-period transitions: for t in 1..n_hours.
        for hour in 1..input.n_hours {
            let dt_t = input.spec.period_hours(hour);
            let up_limit = group.ramp_up_pu_per_hr * dt_t;
            let down_limit = group.ramp_down_pu_per_hr * dt_t;

            // Ramp up:  Σ p_served[t] − Σ p_served[t−1] ≤ ramp_up * dt[t]
            let row_up = input.row_base + local_row;
            for &load_idx in &group.member_load_indices {
                push_triplet(
                    &mut block.triplets,
                    row_up,
                    input.layout.col(hour, input.layout.dispatch.dl + load_idx),
                    1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    row_up,
                    input
                        .layout
                        .col(hour - 1, input.layout.dispatch.dl + load_idx),
                    -1.0,
                );
            }
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = up_limit;
            local_row += 1;

            // Ramp down:  Σ p_served[t−1] − Σ p_served[t] ≤ ramp_down * dt[t]
            let row_down = input.row_base + local_row;
            for &load_idx in &group.member_load_indices {
                push_triplet(
                    &mut block.triplets,
                    row_down,
                    input
                        .layout
                        .col(hour - 1, input.layout.dispatch.dl + load_idx),
                    1.0,
                );
                push_triplet(
                    &mut block.triplets,
                    row_down,
                    input.layout.col(hour, input.layout.dispatch.dl + load_idx),
                    -1.0,
                );
            }
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = down_limit;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) struct ScucHvdcRampVar {
    pub col_local: usize,
    pub ramp_limit_pu: f64,
}

pub(super) fn hvdc_ramp_rows(ramp_vars: &[ScucHvdcRampVar], n_hours: usize) -> usize {
    if n_hours > 1 {
        2 * ramp_vars.len() * (n_hours - 1)
    } else {
        0
    }
}

pub(super) struct ScucHvdcRampRowsInput<'a> {
    pub ramp_vars: &'a [ScucHvdcRampVar],
    pub layout: &'a ScucLayout,
    pub n_hours: usize,
    pub row_base: usize,
}

pub(super) fn build_hvdc_ramp_rows(input: ScucHvdcRampRowsInput<'_>) -> LpBlock {
    let n_rows = hvdc_ramp_rows(input.ramp_vars, input.n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for ramp_var in input.ramp_vars {
        for hour in 1..input.n_hours {
            let ramp_up_row = input.row_base + local_row;
            push_triplet(
                &mut block.triplets,
                ramp_up_row,
                input.layout.col(hour, ramp_var.col_local),
                1.0,
            );
            push_triplet(
                &mut block.triplets,
                ramp_up_row,
                input.layout.col(hour - 1, ramp_var.col_local),
                -1.0,
            );
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = ramp_var.ramp_limit_pu;
            local_row += 1;

            let ramp_down_row = input.row_base + local_row;
            push_triplet(
                &mut block.triplets,
                ramp_down_row,
                input.layout.col(hour - 1, ramp_var.col_local),
                1.0,
            );
            push_triplet(
                &mut block.triplets,
                ramp_down_row,
                input.layout.col(hour, ramp_var.col_local),
                -1.0,
            );
            block.row_lower[local_row] = -BIG_M;
            block.row_upper[local_row] = ramp_var.ramp_limit_pu;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

pub(super) fn explicit_contingency_objective_rows(
    plan: Option<&super::plan::ExplicitContingencyObjectivePlan>,
) -> usize {
    crate::common::contingency::explicit_contingency_objective_rows(plan)
}

pub(super) struct ScucExplicitContingencyObjectiveRowsInput<'a> {
    pub plan: &'a super::plan::ExplicitContingencyObjectivePlan,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub row_base: usize,
    pub base: f64,
}

pub(super) fn build_explicit_contingency_objective_rows(
    input: ScucExplicitContingencyObjectiveRowsInput<'_>,
) -> LpBlock {
    let spec = input.spec;
    crate::common::contingency::build_explicit_contingency_objective_rows(
        crate::common::contingency::ExplicitContingencyObjectiveRowsInput {
            plan: input.plan,
            thermal_penalty_curve: spec.thermal_penalty_curve,
            period_hours: &|period| spec.period_hours(period),
            row_base: input.row_base,
            base: input.base,
        },
    )
}
