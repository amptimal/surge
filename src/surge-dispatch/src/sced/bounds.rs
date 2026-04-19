// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCED variable bounds assembly.

use surge_network::Network;
use surge_network::market::{DispatchableLoad, ReserveDirection};
use surge_network::network::{CommitmentStatus, StorageDispatchMode};
use surge_opf::advanced::{IslandRefs, fix_island_theta_bounds};

use crate::common::blocks::{DispatchBlock, decompose_into_blocks};
use crate::common::reserves::{ReserveLpCtx, ReserveLpLayout};
use crate::common::runtime::DispatchPeriodContext;
use crate::common::spec::DispatchProblemSpec;
use crate::sced::layout::ScedLayout;

pub(super) struct ScedBoundsInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub context: DispatchPeriodContext<'a>,
    pub island_refs: &'a IslandRefs,
    pub reserve_layout: &'a ReserveLpLayout,
    pub reserve_ctx: &'a ReserveLpCtx<'a>,
    pub gen_indices: &'a [usize],
    pub gen_blocks: &'a [Vec<DispatchBlock>],
    pub gen_block_start: &'a [usize],
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub hvdc_band_offsets_abs: &'a [usize],
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub active_vbids: &'a [usize],
    pub n_var: usize,
    pub n_bus: usize,
    pub n_pwl_gen: usize,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_block_vars: usize,
    pub n_blk_res_vars: usize,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub n_iface_rows: usize,
    pub is_block_mode: bool,
    pub has_per_block_reserves: bool,
    pub layout: &'a ScedLayout,
    pub theta_offset: usize,
    pub pg_offset: usize,
    pub sto_ch_offset: usize,
    pub sto_dis_offset: usize,
    pub sto_soc_offset: usize,
    pub sto_epi_dis_offset: usize,
    pub sto_epi_ch_offset: usize,
    pub e_g_offset: usize,
    pub dl_offset: usize,
    pub vbid_offset: usize,
    pub block_offset: usize,
    pub blk_res_offset: usize,
    pub base: f64,
    pub col_cost: &'a mut [f64],
}

pub(super) struct ScedBoundsState {
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
}

pub(super) fn build_variable_bounds(input: ScedBoundsInput<'_>) -> ScedBoundsState {
    let mut col_lower = vec![0.0; input.n_var];
    let mut col_upper = vec![0.0; input.n_var];
    let n_storage = input.storage_gen_local.len();
    let has_hvdc = !input.spec.hvdc_links.is_empty();
    let has_dl = !input.dl_list.is_empty();
    let period_spec = input.spec.period(input.context.period);
    let period_hours = period_spec.interval_hours();
    fix_island_theta_bounds(
        &mut col_lower,
        &mut col_upper,
        input.theta_offset,
        input.n_bus,
        input.island_refs,
    );

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        let g = &input.network.generators[gi];
        let mut lb = g.pmin / input.base;
        let mut ub = g.pmax / input.base;

        let is_ext_must_run = input.spec.must_run_units.is_some_and(|mu| mu.contains(j));
        let commitment_status = g.commitment.as_ref().map(|c| c.status).unwrap_or_default();
        if (commitment_status == CommitmentStatus::MustRun || is_ext_must_run)
            && lb < g.pmin / input.base
        {
            lb = g.pmin / input.base;
        }

        let is_committed = period_spec.is_committed(j);
        if !is_committed {
            lb = 0.0;
            ub = 0.0;
        }

        if input.spec.enforce_shutdown_deloading
            && is_committed
            && let Some(next_commit) = input.context.next_period_commitment
            && !next_commit.get(j).copied().unwrap_or(true)
        {
            let sd_ub = g.shutdown_ramp_mw_per_period(period_hours) / input.base;
            ub = ub.min(sd_ub);
        }

        col_lower[input.pg_offset + j] = lb;
        col_upper[input.pg_offset + j] = ub;
    }

    crate::common::reserves::set_bounds(
        input.reserve_layout,
        &mut col_lower,
        &mut col_upper,
        input.reserve_ctx,
    );

    if let Some(reg_mode) = input.spec.regulation_eligible {
        for ap in &input.reserve_layout.products {
            if ap.product.id.starts_with("reg") {
                for (j, _) in input.gen_indices.iter().enumerate() {
                    if !reg_mode.get(j).copied().unwrap_or(true) {
                        col_upper[ap.gen_var_offset + j] = 0.0;
                    }
                }
            }
        }
    }

    if input.has_per_block_reserves {
        let reg_mode = input.spec.regulation_eligible;
        let mut br_flat = 0;
        for (pi, ap) in input.reserve_layout.products.iter().enumerate() {
            let is_reg = ap.product.id.starts_with("reg");
            let deploy_min = ap.product.deploy_secs / 60.0;
            for (j, blocks) in input.gen_blocks.iter().enumerate() {
                let reg_blocked =
                    is_reg && reg_mode.is_some_and(|rm| rm.get(j).copied() == Some(false));
                for (i, block) in blocks.iter().enumerate() {
                    let col = input.blk_res_offset
                        + pi * input.n_block_vars
                        + input.gen_block_start[j]
                        + i;
                    col_lower[col] = 0.0;
                    if reg_blocked {
                        col_upper[col] = 0.0;
                    } else {
                        let width_mw = block.width_mw();
                        let ramp_mw = if ap.product.apply_deploy_ramp_limit {
                            (match ap.product.direction {
                                ReserveDirection::Up => {
                                    if is_reg {
                                        block.reg_ramp_up_mw_per_min
                                    } else {
                                        block.ramp_up_mw_per_min
                                    }
                                }
                                ReserveDirection::Down => {
                                    if is_reg {
                                        block.reg_ramp_dn_mw_per_min
                                    } else {
                                        block.ramp_dn_mw_per_min
                                    }
                                }
                            }) * deploy_min
                        } else {
                            f64::INFINITY
                        };
                        col_upper[col] = width_mw.min(ramp_mw) / input.base;
                    }
                    br_flat += 1;
                }
            }
        }
        debug_assert_eq!(br_flat, input.n_blk_res_vars);
    }

    if has_hvdc {
        for (k, hvdc) in input.spec.hvdc_links.iter().enumerate() {
            if hvdc.is_banded() {
                for (b, band) in hvdc.bands.iter().enumerate() {
                    let col = input.hvdc_band_offsets_abs[k] + b;
                    let mut lb = band.p_min_mw / input.base;
                    let mut ub = band.p_max_mw / input.base;
                    let ramp = if band.ramp_mw_per_min > 0.0 {
                        band.ramp_mw_per_min
                    } else if hvdc.ramp_mw_per_min > 0.0 {
                        hvdc.ramp_mw_per_min
                    } else {
                        0.0
                    };
                    if ramp > 0.0
                        && let Some(prev_band) = input.context.prev_hvdc_dispatch_at(k)
                    {
                        let ramp_mw = ramp * 60.0 * period_hours;
                        let prev_band = if b == 0 { prev_band } else { 0.0 };
                        lb = lb.max((prev_band - ramp_mw) / input.base);
                        ub = ub.min((prev_band + ramp_mw) / input.base);
                    }
                    col_lower[col] = lb;
                    col_upper[col] = ub;
                }
            } else {
                let mut lb = hvdc.p_dc_min_mw / input.base;
                let mut ub = hvdc.p_dc_max_mw / input.base;

                if let Some(fixed_mw) = input
                    .spec
                    .fixed_hvdc_dispatch_mw_at(input.context.period, k)
                {
                    let fixed_pu = fixed_mw / input.base;
                    lb = fixed_pu;
                    ub = fixed_pu;
                }

                if (ub - lb).abs() > 1e-9
                    && let Some(prev_mw) = input.context.prev_hvdc_dispatch_at(k)
                    && hvdc.ramp_mw_per_min > 0.0
                {
                    let ramp_mw = hvdc.ramp_mw_per_min * 60.0 * period_hours;
                    lb = lb.max((prev_mw - ramp_mw) / input.base);
                    ub = ub.min((prev_mw + ramp_mw) / input.base);
                }

                col_lower[input.hvdc_band_offsets_abs[k]] = lb;
                col_upper[input.hvdc_band_offsets_abs[k]] = ub;
            }
        }
    }

    for k in 0..input.n_pwl_gen {
        col_lower[input.e_g_offset + k] = f64::NEG_INFINITY;
        col_upper[input.e_g_offset + k] = f64::INFINITY;
    }

    for &(s, j, gi) in input.storage_gen_local {
        let g = &input.network.generators[gi];
        let sto = g
            .storage
            .as_ref()
            .expect("storage_gen_local only contains generators with storage");
        col_lower[input.sto_soc_offset + s] = sto.soc_min_mwh;
        col_upper[input.sto_soc_offset + s] = sto.soc_max_mwh;
        if sto.dispatch_mode == StorageDispatchMode::SelfSchedule {
            let net = period_spec
                .storage_self_schedule_mw(gi)
                .unwrap_or(sto.self_schedule_mw);
            let dis_val = net.max(0.0).min(g.discharge_mw_max()) / input.base;
            let ch_val = (-net).max(0.0).min(g.charge_mw_max()) / input.base;
            col_lower[input.sto_dis_offset + s] = dis_val;
            col_upper[input.sto_dis_offset + s] = dis_val;
            col_lower[input.sto_ch_offset + s] = ch_val;
            col_upper[input.sto_ch_offset + s] = ch_val;
            let pg_val = dis_val - ch_val;
            col_lower[input.pg_offset + j] = pg_val;
            col_upper[input.pg_offset + j] = pg_val;
        } else {
            col_lower[input.sto_ch_offset + s] = 0.0;
            col_upper[input.sto_ch_offset + s] = g.charge_mw_max() / input.base;
            col_lower[input.sto_dis_offset + s] = 0.0;
            col_upper[input.sto_dis_offset + s] = g.discharge_mw_max() / input.base;
        }
    }
    for k in 0..input.n_sto_dis_epi {
        col_lower[input.sto_epi_dis_offset + k] = f64::NEG_INFINITY;
        col_upper[input.sto_epi_dis_offset + k] = f64::INFINITY;
    }
    for k in 0..input.n_sto_ch_epi {
        col_lower[input.sto_epi_ch_offset + k] = f64::NEG_INFINITY;
        col_upper[input.sto_epi_ch_offset + k] = f64::INFINITY;
    }

    if has_dl {
        for (k, dl) in input.dl_list.iter().enumerate() {
            let (_, p_max, _, _, _, _) = crate::common::costs::resolve_dl_for_period_from_spec(
                input.dl_orig_idx[k],
                input.context.period,
                dl,
                input.spec,
            );
            col_lower[input.dl_offset + k] = dl.p_min_pu;
            col_upper[input.dl_offset + k] = p_max;
        }
    }

    for (k, &bi) in input.active_vbids.iter().enumerate() {
        let vb = &input.spec.virtual_bids[bi];
        col_lower[input.vbid_offset + k] = 0.0;
        col_upper[input.vbid_offset + k] = vb.mw_limit / input.base;
    }

    for bus_idx in 0..input.n_bus {
        col_lower[input.layout.pb_curtailment_bus_col(bus_idx)] = 0.0;
        col_upper[input.layout.pb_curtailment_bus_col(bus_idx)] = f64::INFINITY;
        col_lower[input.layout.pb_excess_bus_col(bus_idx)] = 0.0;
        col_upper[input.layout.pb_excess_bus_col(bus_idx)] = f64::INFINITY;
    }
    for (s, &(mw_cap, penalty)) in input
        .spec
        .power_balance_penalty
        .curtailment
        .iter()
        .enumerate()
    {
        col_lower[input.layout.pb_curtailment_seg_col(s)] = 0.0;
        col_upper[input.layout.pb_curtailment_seg_col(s)] = mw_cap / input.base;
        input.col_cost[input.layout.pb_curtailment_seg_col(s)] =
            penalty * input.base * period_spec.interval_hours();
    }
    for (s, &(mw_cap, penalty)) in input.spec.power_balance_penalty.excess.iter().enumerate() {
        col_lower[input.layout.pb_excess_seg_col(s)] = 0.0;
        col_upper[input.layout.pb_excess_seg_col(s)] = mw_cap / input.base;
        input.col_cost[input.layout.pb_excess_seg_col(s)] =
            penalty * input.base * period_spec.interval_hours();
    }
    for row_idx in 0..input.n_branch_flow {
        col_lower[input.layout.branch_lower_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.branch_lower_slack_col(row_idx)] = f64::INFINITY;
        col_lower[input.layout.branch_upper_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.branch_upper_slack_col(row_idx)] = f64::INFINITY;
    }
    for row_idx in 0..input.n_fg_rows {
        col_lower[input.layout.flowgate_lower_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.flowgate_lower_slack_col(row_idx)] = f64::INFINITY;
        col_lower[input.layout.flowgate_upper_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.flowgate_upper_slack_col(row_idx)] = f64::INFINITY;
    }
    for row_idx in 0..input.n_iface_rows {
        col_lower[input.layout.interface_lower_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.interface_lower_slack_col(row_idx)] = f64::INFINITY;
        col_lower[input.layout.interface_upper_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.interface_upper_slack_col(row_idx)] = f64::INFINITY;
    }
    for j in 0..input.gen_indices.len() {
        col_lower[input.layout.ramp_up_slack_col(j)] = 0.0;
        col_upper[input.layout.ramp_up_slack_col(j)] = f64::INFINITY;
        col_lower[input.layout.ramp_down_slack_col(j)] = 0.0;
        col_upper[input.layout.ramp_down_slack_col(j)] = f64::INFINITY;
    }
    for row_idx in 0..input.layout.n_angle_diff_rows {
        col_lower[input.layout.angle_diff_lower_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.angle_diff_lower_slack_col(row_idx)] = f64::INFINITY;
        col_lower[input.layout.angle_diff_upper_slack_col(row_idx)] = 0.0;
        col_upper[input.layout.angle_diff_upper_slack_col(row_idx)] = f64::INFINITY;
    }

    if input.is_block_mode {
        let mut flat_idx = 0;
        for (j, blocks) in input.gen_blocks.iter().enumerate() {
            let gi = input.gen_indices[j];
            let g = &input.network.generators[gi];
            let is_committed = period_spec.is_committed(j);

            let prev_fills = if is_committed {
                input
                    .context
                    .prev_dispatch_at(j)
                    .map(|prev| decompose_into_blocks(prev, g.pmin, blocks))
            } else {
                None
            };

            for (i, block) in blocks.iter().enumerate() {
                let width_pu = block.width_mw() / input.base;
                if !is_committed {
                    col_lower[input.block_offset + flat_idx] = 0.0;
                    col_upper[input.block_offset + flat_idx] = 0.0;
                } else {
                    let mut lb: f64 = 0.0;
                    let mut ub = width_pu;

                    if let Some(ref fills) = prev_fills {
                        let prev_fill_pu = fills[i] / input.base;
                        let dt_min = 60.0 * period_hours;
                        let ramp_up_pu = block.ramp_up_mw_per_min * dt_min / input.base;
                        let ramp_dn_pu = block.ramp_dn_mw_per_min * dt_min / input.base;

                        if ramp_up_pu < f64::MAX / 2.0 {
                            ub = ub.min(prev_fill_pu + ramp_up_pu);
                        }
                        if ramp_dn_pu < f64::MAX / 2.0 {
                            lb = lb.max(prev_fill_pu - ramp_dn_pu);
                        }
                    }

                    col_lower[input.block_offset + flat_idx] = lb;
                    col_upper[input.block_offset + flat_idx] = ub;
                }
                flat_idx += 1;
            }
        }
    }

    // SCED-AC Benders epigraph variable. The variable is allocated only when
    // the runtime opted into Benders for this period. We bound it from below
    // at 0 because the AC slack penalty cost it tracks is non-negative; the
    // upper bound is +inf and the per-cut LP rows are responsible for forcing
    // it up. Cost is `+1.0` (in dollars per per-unit-MW-hr converted to
    // dollars by the LP solver's natural scaling — the eta variable carries
    // dollar units already, no `* base` multiplication).
    if let Some(eta_col) = input.layout.benders_eta_col() {
        col_lower[eta_col] = 0.0;
        col_upper[eta_col] = f64::INFINITY;
        input.col_cost[eta_col] = 1.0;
    }

    debug_assert_eq!(n_storage, input.storage_gen_local.len());

    ScedBoundsState {
        col_lower,
        col_upper,
    }
}
