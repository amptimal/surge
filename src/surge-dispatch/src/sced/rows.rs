// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCED row planning and assembly helpers.

use surge_network::Network;
use surge_sparse::Triplet;

use crate::common::builders;
use crate::common::ramp::{ramp_dn_for_mode, ramp_up_for_mode};
use crate::common::reserves::{ReserveLpCtx, ReserveLpLayout};
use crate::common::runtime::{DispatchPeriodContext, effective_storage_soc_mwh};
use crate::common::setup::DispatchSetup;
use crate::common::spec::DispatchProblemSpec;
use crate::sced::layout::ScedLayout;

pub(super) struct ScedRowPlan {
    pub n_row: usize,
    pb_aggregation_base_row: usize,
    pub system_policy_base_row: usize,
    pub ramp_base_row: usize,
    pub reserve_row_base: usize,
    plc_base_row: usize,
    pub sto_base_row: usize,
    freq_base_row: usize,
    block_link_base_row: usize,
    /// Base row index for angle difference constraint rows.
    pub angle_diff_base_row: usize,
    /// Number of angle difference constraint rows (2 per angle-constrained branch).
    #[allow(dead_code)]
    pub n_angle_diff_rows: usize,
    /// Base row index for SCED-AC Benders cut constraints (one row per cut),
    /// or 0 when no Benders cuts are present in this period.
    pub benders_cut_base_row: usize,
    /// Number of Benders cut rows allocated in this period.
    pub n_benders_cut_rows: usize,
}

pub(super) fn plan_rows(
    n_flow: usize,
    n_bus: usize,
    n_system_policy_rows: usize,
    setup: &DispatchSetup,
    reserve_layout: &ReserveLpLayout,
    spec: &DispatchProblemSpec<'_>,
    has_prev_dispatch: bool,
    period: usize,
    n_angle_diff_branches: usize,
) -> ScedRowPlan {
    let n_storage = setup.n_storage;
    let n_gen = setup.n_gen;
    let n_active_products = reserve_layout.products.len();
    let mut n_sto_rows = 4 * n_storage + setup.n_sto_dis_offer_rows + setup.n_sto_ch_bid_rows;
    // Foldback rows (section 7 of builders::build_storage_rows) — one
    // per storage unit per direction where the threshold is set.
    n_sto_rows += setup
        .storage_foldback_discharge_mwh
        .iter()
        .filter(|o| o.is_some())
        .count()
        + setup
            .storage_foldback_charge_mwh
            .iter()
            .filter(|o| o.is_some())
            .count();
    if !spec.storage_reserve_soc_impact.is_empty() {
        n_sto_rows += 2 * n_storage;
    }
    let has_freq_inertia = spec.frequency_security.effective_min_inertia_mws() > 0.0;
    let has_freq_pfr = spec.frequency_security.min_pfr_mw.is_some_and(|v| v > 0.0);
    let n_freq_rows = if has_freq_inertia { 1 } else { 0 } + if has_freq_pfr { 1 } else { 0 };
    let n_block_link_rows = if setup.is_block_mode { n_gen } else { 0 };
    let n_blk_res_rows = if setup.has_per_block_reserves {
        n_gen * n_active_products + setup.n_block_vars * n_active_products
    } else {
        0
    };
    let n_pb_aggregation_rows = 2usize;
    let n_ramp_rows = if has_prev_dispatch { 2 * n_gen } else { 0 };
    // SCED-AC Benders: one inequality row per cut targeted at this period.
    // The row form is `eta − Σ_g λ_g · Pg[g] ≥ rhs`. Cuts that don't apply
    // to this period are silently filtered out by `cuts_for_period`.
    let n_benders_cut_rows = spec.sced_ac_benders.cuts_for_period(period).count();
    let pb_aggregation_base_row = n_flow + n_bus;
    let ramp_base_row = pb_aggregation_base_row + n_pb_aggregation_rows;
    let system_policy_base_row = ramp_base_row + n_ramp_rows;
    let plc_base_row = system_policy_base_row + n_system_policy_rows;
    let sto_base_row = plc_base_row + setup.n_pwl_rows;
    let freq_base_row = sto_base_row + n_sto_rows;
    let block_link_base_row = freq_base_row + n_freq_rows;
    let reserve_row_base = block_link_base_row + n_block_link_rows;
    // Each angle-constrained branch gets 2 constraint rows (upper + lower).
    let n_angle_diff_rows = 2 * n_angle_diff_branches;
    let angle_diff_base_row = reserve_row_base + reserve_layout.n_reserve_rows + n_blk_res_rows;
    let benders_cut_base_row = angle_diff_base_row + n_angle_diff_rows;
    let n_row = benders_cut_base_row + n_benders_cut_rows;

    ScedRowPlan {
        n_row,
        pb_aggregation_base_row,
        system_policy_base_row,
        ramp_base_row,
        reserve_row_base,
        plc_base_row,
        sto_base_row,
        freq_base_row,
        block_link_base_row,
        angle_diff_base_row,
        n_angle_diff_rows,
        benders_cut_base_row,
        n_benders_cut_rows,
    }
}

pub(super) struct ScedRowsInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub context: DispatchPeriodContext<'a>,
    pub setup: &'a DispatchSetup,
    pub reserve_layout: &'a ReserveLpLayout,
    pub reserve_ctx: &'a ReserveLpCtx<'a>,
    pub plan: &'a ScedRowPlan,
    pub layout: &'a ScedLayout,
    pub n_pb_curt_segs: usize,
    pub n_pb_excess_segs: usize,
    pub pg_offset: usize,
    pub sto_ch_offset: usize,
    pub sto_dis_offset: usize,
    pub sto_soc_offset: usize,
    pub sto_epi_dis_offset: usize,
    pub sto_epi_ch_offset: usize,
    pub e_g_offset: usize,
    pub block_offset: usize,
    pub blk_res_offset: usize,
    pub base: f64,
}

pub(super) fn build_rows(
    input: ScedRowsInput<'_>,
    triplets: &mut Vec<Triplet<f64>>,
    row_lower: &mut Vec<f64>,
    row_upper: &mut Vec<f64>,
) {
    let period_spec = input.spec.period(input.context.period);
    let period_hours = period_spec.interval_hours();
    for bus_idx in 0..input.network.n_buses() {
        triplets.push(Triplet {
            row: input.plan.pb_aggregation_base_row,
            col: input.layout.pb_curtailment_bus_col(bus_idx),
            val: 1.0,
        });
        triplets.push(Triplet {
            row: input.plan.pb_aggregation_base_row + 1,
            col: input.layout.pb_excess_bus_col(bus_idx),
            val: 1.0,
        });
    }
    for seg_idx in 0..input.n_pb_curt_segs {
        triplets.push(Triplet {
            row: input.plan.pb_aggregation_base_row,
            col: input.layout.pb_curtailment_seg_col(seg_idx),
            val: -1.0,
        });
    }
    for seg_idx in 0..input.n_pb_excess_segs {
        triplets.push(Triplet {
            row: input.plan.pb_aggregation_base_row + 1,
            col: input.layout.pb_excess_seg_col(seg_idx),
            val: -1.0,
        });
    }
    row_lower.push(0.0);
    row_upper.push(0.0);
    row_lower.push(0.0);
    row_upper.push(0.0);

    if input.context.has_prev_dispatch() {
        for (j, &gi) in input.setup.gen_indices.iter().enumerate() {
            let up_row = input.plan.ramp_base_row + 2 * j;
            let down_row = input.plan.ramp_base_row + 2 * j + 1;
            if let Some(prev_mw) = input.context.prev_dispatch_at(j) {
                let generator = &input.network.generators[gi];
                let ramp_up_mw = ramp_up_for_mode(generator, prev_mw, input.spec.ramp_mode)
                    * 60.0
                    * period_hours;
                let ramp_down_mw = ramp_dn_for_mode(generator, prev_mw, input.spec.ramp_mode)
                    * 60.0
                    * period_hours;

                triplets.push(Triplet {
                    row: up_row,
                    col: input.pg_offset + j,
                    val: 1.0,
                });
                triplets.push(Triplet {
                    row: up_row,
                    col: input.layout.ramp_up_slack_col(j),
                    val: -1.0,
                });
                row_lower.push(f64::NEG_INFINITY);
                row_upper.push((prev_mw + ramp_up_mw) / input.base);

                triplets.push(Triplet {
                    row: down_row,
                    col: input.pg_offset + j,
                    val: 1.0,
                });
                triplets.push(Triplet {
                    row: down_row,
                    col: input.layout.ramp_down_slack_col(j),
                    val: 1.0,
                });
                row_lower.push((prev_mw - ramp_down_mw) / input.base);
                row_upper.push(f64::INFINITY);
            } else {
                row_lower.push(f64::NEG_INFINITY);
                row_upper.push(f64::INFINITY);
                row_lower.push(f64::NEG_INFINITY);
                row_upper.push(f64::INFINITY);
            }
        }
    }

    let system_policy_hour_bases = [0usize];
    builders::build_system_policy_rows(builders::DcSystemPolicyRowsInput {
        spec: input.spec,
        hourly_networks: std::slice::from_ref(input.network),
        effective_co2_rate: &input.setup.effective_co2_rate,
        tie_line_pairs: &input.setup.tie_line_pairs,
        hour_col_bases: &system_policy_hour_bases,
        theta_off: input.layout.dispatch.theta,
        pg_off: input.pg_offset,
        hvdc_off: input.layout.dispatch.hvdc,
        hvdc_band_offsets: &input.setup.hvdc_band_offsets_rel,
        row_base: input.plan.system_policy_base_row,
        base: input.base,
        step_h: period_hours,
    })
    .extend_into(triplets, row_lower, row_upper);

    builders::build_gen_epiograph_rows(
        input.setup,
        0,
        input.plan.plc_base_row,
        input.pg_offset,
        input.e_g_offset,
    )
    .extend_into(triplets, row_lower, row_upper);

    let soc_prev_mwh: Vec<f64> = input
        .setup
        .storage_gen_local
        .iter()
        .map(|&(_, _, gi)| {
            effective_storage_soc_mwh(
                input.context.storage_soc_override,
                gi,
                &input.network.generators[gi],
            )
        })
        .collect();
    builders::build_storage_rows(
        input.network,
        input.setup,
        input.sto_ch_offset,
        input.sto_dis_offset,
        input.sto_soc_offset,
        input.sto_epi_dis_offset,
        input.sto_epi_ch_offset,
        input.pg_offset,
        0,
        input.plan.sto_base_row,
        &soc_prev_mwh,
        None,
        period_hours,
        input.reserve_layout,
        true,
        input.base,
    )
    .extend_into(triplets, row_lower, row_upper);

    if input.setup.n_storage > 0 && !input.spec.storage_reserve_soc_impact.is_empty() {
        // The reserve SoC-impact rows sit AFTER the foldback rows so we
        // have to offset past them too.
        let n_foldback = input
            .setup
            .storage_foldback_discharge_mwh
            .iter()
            .filter(|o| o.is_some())
            .count()
            + input
                .setup
                .storage_foldback_charge_mwh
                .iter()
                .filter(|o| o.is_some())
                .count();
        let mut local_row = 4 * input.setup.n_storage
            + input.setup.n_sto_dis_offer_rows
            + input.setup.n_sto_ch_bid_rows
            + n_foldback;

        for &(s, j, gi) in &input.setup.storage_gen_local {
            let storage = input.network.generators[gi]
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            let row = input.plan.sto_base_row + local_row;
            triplets.push(Triplet {
                row,
                col: input.sto_soc_offset + s,
                val: 1.0,
            });
            for ap in &input.reserve_layout.products {
                let impact = period_spec.storage_reserve_soc_impact(gi, ap.product.id.as_str());
                if impact > 0.0 {
                    triplets.push(Triplet {
                        row,
                        col: ap.gen_var_offset + j,
                        val: -impact * period_hours * input.base,
                    });
                }
            }
            row_lower.push(storage.soc_min_mwh);
            row_upper.push(f64::INFINITY);
            local_row += 1;
        }

        for &(s, j, gi) in &input.setup.storage_gen_local {
            let storage = input.network.generators[gi]
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            let row = input.plan.sto_base_row + local_row;
            triplets.push(Triplet {
                row,
                col: input.sto_soc_offset + s,
                val: 1.0,
            });
            for ap in &input.reserve_layout.products {
                let impact = period_spec.storage_reserve_soc_impact(gi, ap.product.id.as_str());
                if impact < 0.0 {
                    triplets.push(Triplet {
                        row,
                        col: ap.gen_var_offset + j,
                        val: -impact * period_hours * input.base,
                    });
                }
            }
            row_lower.push(f64::NEG_INFINITY);
            row_upper.push(storage.soc_max_mwh);
            local_row += 1;
        }
    }

    builders::build_frequency_rows(
        input.network,
        &input.setup.gen_indices,
        input.spec,
        0,
        input.plan.freq_base_row,
        input.pg_offset,
        input.base,
        false,
        0,
    )
    .extend_into(triplets, row_lower, row_upper);

    if input.setup.is_block_mode {
        builders::build_block_linking_rows(
            input.setup,
            input.spec,
            &input.setup.gen_indices,
            input.network,
            input.context.period,
            0,
            input.plan.block_link_base_row,
            input.pg_offset,
            input.block_offset,
            None,
            input.base,
        )
        .extend_into(triplets, row_lower, row_upper);
    }

    let (reserve_triplets_vec, reserve_row_lower, reserve_row_upper) =
        crate::common::reserves::build_constraints(
            input.reserve_layout,
            input.plan.reserve_row_base,
            input.pg_offset,
            input.layout.dispatch.dl,
            input.reserve_ctx,
        );
    triplets.extend(reserve_triplets_vec);
    row_lower.extend(reserve_row_lower);
    row_upper.extend(reserve_row_upper);

    if input.setup.has_per_block_reserves {
        let blk_res_row_base = input.plan.reserve_row_base + input.reserve_layout.n_reserve_rows;
        builders::build_per_block_reserve_rows(
            input.setup,
            input.reserve_layout,
            0,
            blk_res_row_base,
            input.block_offset,
            input.blk_res_offset,
            input.base,
        )
        .extend_into(triplets, row_lower, row_upper);
    }

    build_benders_cut_rows(&input, triplets, row_lower, row_upper);
}

/// Materialise SCED-AC Benders optimality cuts into LP rows.
///
/// Each cut for the current period contributes one inequality of the form
///
///   `eta − Σ_g λ_g · Pg[g] ≥ rhs`
///
/// where `eta` is the SCED epigraph variable allocated by [`super::layout::ScedLayout`]
/// and `λ_g` is the Lagrangian multiplier on the corresponding generator's
/// fixed-Pg bound from the AC OPF subproblem (see
/// `surge_opf::solve_ac_opf_subproblem`). The cut is silently skipped when:
///
///   - the layout has no eta column allocated for this period (Benders is
///     disabled);
///   - the cut's `coefficients` map is empty (degenerate cut);
///   - none of the cut's resource ids match an in-service generator at this
///     period (the cut still installs an `eta ≥ rhs` constant lower bound,
///     because the constant term is part of the AC adder we want to track).
///
/// Coefficient units: `coefficients_dollars_per_mw_per_hour[g] · base_mva`
/// gives the LP coefficient on Pg in per-unit. The eta variable carries
/// dollars per hour directly (no scaling), so the row constant is consumed
/// as-is.
fn build_benders_cut_rows(
    input: &ScedRowsInput<'_>,
    triplets: &mut Vec<Triplet<f64>>,
    row_lower: &mut Vec<f64>,
    row_upper: &mut Vec<f64>,
) {
    let Some(eta_col) = input.layout.benders_eta_col() else {
        return;
    };
    if input.plan.n_benders_cut_rows == 0 {
        return;
    }

    // Build a quick lookup from in-service-generator local index `j` →
    // resource id, so we can match cut coefficients (which are keyed by
    // resource id) to LP columns. The local index `j` is the SCED Pg layout
    // index; the column is `pg_offset + j`.
    let n_gen = input.setup.gen_indices.len();
    let mut local_id_to_j: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::with_capacity(n_gen);
    for (j, &gi) in input.setup.gen_indices.iter().enumerate() {
        let id = input.network.generators[gi].id.as_str();
        local_id_to_j.insert(id, j);
    }

    let cuts = input
        .spec
        .sced_ac_benders
        .cuts_for_period(input.context.period);
    for (cut_idx, cut) in cuts.enumerate() {
        let row = input.plan.benders_cut_base_row + cut_idx;
        // η contributes with coefficient +1.
        triplets.push(Triplet {
            row,
            col: eta_col,
            val: 1.0,
        });
        // − Σ_g λ_g · Pg[g] in per-unit. Multiply each `λ_g` (in $/MW-hr)
        // by `base` to convert from MW to per-unit, since the Pg LP variable
        // is in per-unit.
        for (resource_id, &lambda_dollars_per_mw_per_hr) in
            &cut.coefficients_dollars_per_mw_per_hour
        {
            if let Some(&j) = local_id_to_j.get(resource_id.as_str()) {
                if lambda_dollars_per_mw_per_hr.abs() > 1e-12 {
                    triplets.push(Triplet {
                        row,
                        col: input.pg_offset + j,
                        val: -lambda_dollars_per_mw_per_hr * input.base,
                    });
                }
            }
            // Cut coefficients keyed to resource ids that are not in-service
            // (or not in this network) are silently dropped — they cannot
            // contribute because the corresponding Pg variable does not
            // exist. The constant term still applies via `rhs`.
        }
        // Row form: `eta − Σ … ≥ rhs`. The lower bound is `rhs`, upper is
        // unbounded.
        row_lower.push(cut.rhs_dollars_per_hour);
        row_upper.push(f64::INFINITY);
    }
}
