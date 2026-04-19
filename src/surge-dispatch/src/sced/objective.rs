// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCED objective and Hessian assembly helpers.

use std::collections::HashSet;

use surge_network::Network;
use surge_network::market::{CostCurve, DispatchableLoad, VirtualBidDirection};
use surge_network::network::StorageDispatchMode;

use crate::common::blocks::DispatchBlock;
use crate::common::reserves::{ReserveLpCtx, ReserveLpLayout};
use crate::common::spec::DispatchProblemSpec;
use crate::sced::layout::ScedLayout;

pub(super) struct ScedObjectiveInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub reserve_ctx: &'a ReserveLpCtx<'a>,
    pub reserve_layout: &'a ReserveLpLayout,
    pub gen_indices: &'a [usize],
    pub gen_blocks: &'a [Vec<DispatchBlock>],
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub hvdc_band_offsets_abs: &'a [usize],
    pub pwl_gen_info: &'a [(usize, Vec<(f64, f64)>)],
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub active_vbids: &'a [usize],
    pub effective_co2_price: f64,
    pub effective_co2_rate: &'a [f64],
    pub period: usize,
    pub layout: &'a ScedLayout,
    pub n_var: usize,
    pub n_bus: usize,
    pub n_gen: usize,
    pub n_storage: usize,
    pub n_hvdc_vars: usize,
    pub n_pwl_gen: usize,
    pub n_vbid: usize,
    pub n_block_vars: usize,
    pub n_blk_res_vars: usize,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_pb_curt_segs: usize,
    pub n_pb_excess_segs: usize,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub n_iface_rows: usize,
    pub is_block_mode: bool,
    pub pg_offset: usize,
    pub sto_ch_offset: usize,
    pub sto_dis_offset: usize,
    pub sto_epi_dis_offset: usize,
    pub sto_epi_ch_offset: usize,
    pub e_g_offset: usize,
    pub dl_offset: usize,
    pub vbid_offset: usize,
    pub block_offset: usize,
    pub base: f64,
}

pub(super) struct ScedObjectiveState {
    pub col_cost: Vec<f64>,
    pub c0_total: f64,
    pub q_start: Option<Vec<i32>>,
    pub q_index: Option<Vec<i32>>,
    pub q_value: Option<Vec<f64>>,
}

/// See `scuc/objective.rs::pu_h_cost`. SCED objective is single-period so the
/// `dt_h` factor is hoisted once at the top of `build_objective`.
#[inline]
fn pu_h_cost(rate_per_mwh: f64, base_mva: f64, dt_h: f64) -> f64 {
    rate_per_mwh * base_mva * dt_h
}

#[inline]
fn h_cost(rate_per_hour: f64, dt_h: f64) -> f64 {
    rate_per_hour * dt_h
}

pub(super) fn build_objective(input: ScedObjectiveInput<'_>) -> ScedObjectiveState {
    let mut col_cost = vec![0.0; input.n_var];
    let mut q_diag = vec![0.0; input.n_gen];
    let mut c0_total = 0.0;
    let period_spec = input.spec.period(input.period);
    let pwl_gen_set: HashSet<usize> = input.pwl_gen_info.iter().map(|(j, _)| *j).collect();
    // Every per-MWh / per-hour cost in the SCED objective must scale
    // by the period duration so the optimum is invariant to sub-hourly
    // horizons.
    let dt_h = input.spec.period_hours(input.period);

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        let g = &input.network.generators[gi];
        if g.is_storage() {
            continue;
        }
        let mut offer_cost_buf: Option<CostCurve> = None;
        let cost = crate::common::costs::resolve_cost_for_period_from_spec(
            gi,
            input.period,
            g,
            input.spec,
            &mut offer_cost_buf,
            Some(g.pmax),
        );

        if input.is_block_mode {
            let is_committed = period_spec.is_committed(j);

            if is_committed {
                match cost {
                    CostCurve::Polynomial { coeffs, .. } => {
                        let mut val = 0.0;
                        for c in coeffs {
                            val = val * g.pmin + c;
                        }
                        // `val` is f(pmin) in $/h. Eq (61): commitment cost is
                        // dt × f(pmin) for committed devices.
                        c0_total += h_cost(val, dt_h);
                    }
                    CostCurve::PiecewiseLinear { points, .. } => {
                        if let Some(&(_, c)) = points.first() {
                            c0_total += h_cost(c, dt_h);
                        }
                    }
                }
            } else if let CostCurve::Polynomial { coeffs, .. } = cost {
                // For uncommitted devices the constant term still contributes
                // to the period objective when the gen happens to dispatch
                // (offer/no-load splitting); preserve the existing convention.
                match coeffs.len() {
                    1 => c0_total += h_cost(coeffs[0], dt_h),
                    2 => c0_total += h_cost(coeffs[1], dt_h),
                    n if n >= 3 => c0_total += h_cost(coeffs[2], dt_h),
                    _ => {}
                }
            }
            continue;
        }

        if pwl_gen_set.contains(&j) {
            continue;
        }

        match cost {
            CostCurve::Polynomial { coeffs, .. } => match coeffs.len() {
                0 => {}
                1 => c0_total += h_cost(coeffs[0], dt_h),
                2 => {
                    col_cost[input.pg_offset + j] = pu_h_cost(coeffs[0], input.base, dt_h);
                    c0_total += h_cost(coeffs[1], dt_h);
                }
                _ => {
                    // Quadratic cost f(p) = a·p² + b·p + c, p in MW. The LP
                    // pg column is in pu, so the Hessian diagonal entry is
                    // 2a · base² · dt_h (the factor of 2 absorbs the ½ x'Hx
                    // convention HiGHS expects). Without dt_h the quadratic
                    // term would be wrong on non-uniform horizons.
                    q_diag[j] = 2.0 * coeffs[0] * input.base * input.base * dt_h;
                    col_cost[input.pg_offset + j] = pu_h_cost(coeffs[1], input.base, dt_h);
                    c0_total += h_cost(coeffs[2], dt_h);
                }
            },
            CostCurve::PiecewiseLinear { .. } => {}
        }
    }

    for k in 0..input.n_pwl_gen {
        // Epigraph rows constrain `e_g` in $/h (slope_pu and intercept carry
        // $/h units; see `common/costs::pwl_curve_segments`). Scale by `dt_h`
        // so the LP objective contribution is $ for the period, matching
        // `pu_h_cost` convention elsewhere.
        col_cost[input.e_g_offset + k] = dt_h;
    }

    if input.is_block_mode {
        let mut flat_idx = 0;
        for blocks in input.gen_blocks {
            for block in blocks {
                // Eq (131): z^en = dt × Σ c^en × p_jtm.
                col_cost[input.block_offset + flat_idx] =
                    pu_h_cost(block.marginal_cost, input.base, dt_h);
                flat_idx += 1;
            }
        }
    }

    // common/reserves::set_objective writes its own reserve cost coefficients
    // using the shared `ReserveLpCtx`. The context now carries `dt_h` so the
    // helper applies the correct period scaling.
    crate::common::reserves::set_objective(input.reserve_layout, &mut col_cost, input.reserve_ctx);

    for (k, hvdc) in input.spec.hvdc_links.iter().enumerate() {
        if hvdc.is_banded() {
            for (b, band) in hvdc.bands.iter().enumerate() {
                let col = input.hvdc_band_offsets_abs[k] + b;
                if band.cost_per_mwh.abs() > 1e-12 {
                    col_cost[col] = pu_h_cost(band.cost_per_mwh, input.base, dt_h);
                }
            }
        } else if hvdc.cost_per_mwh > 0.0 {
            col_cost[input.hvdc_band_offsets_abs[k]] =
                pu_h_cost(hvdc.cost_per_mwh, input.base, dt_h);
        }
    }

    for &(s, _, gi) in input.storage_gen_local {
        let g = &input.network.generators[gi];
        let sto = g
            .storage
            .as_ref()
            .expect("storage_gen_local only contains generators with storage");
        match sto.dispatch_mode {
            StorageDispatchMode::CostMinimization => {
                // Storage is a first-class device on real grids. Its variable
                // and degradation costs are $/MWh and the LP columns are pu,
                // so the dt × base scaling is mandatory for non-1h periods.
                col_cost[input.sto_dis_offset + s] = pu_h_cost(
                    sto.variable_cost_per_mwh + sto.degradation_cost_per_mwh,
                    input.base,
                    dt_h,
                );
                col_cost[input.sto_ch_offset + s] =
                    pu_h_cost(sto.degradation_cost_per_mwh, input.base, dt_h);
            }
            StorageDispatchMode::OfferCurve | StorageDispatchMode::SelfSchedule => {}
        }
    }
    for k in 0..input.n_sto_dis_epi {
        col_cost[input.sto_epi_dis_offset + k] = 1.0;
    }
    for k in 0..input.n_sto_ch_epi {
        col_cost[input.sto_epi_ch_offset + k] = 1.0;
    }

    for (k, dl) in input.dl_list.iter().enumerate() {
        let (_, _, _, _, _, cost_model) = crate::common::costs::resolve_dl_for_period_from_spec(
            input.dl_orig_idx[k],
            input.period,
            dl,
            input.spec,
        );
        col_cost[input.dl_offset + k] = cost_model.dc_linear_obj_coeff(input.base) * dt_h;
    }

    for (k, &bi) in input.active_vbids.iter().enumerate() {
        let vb = &input.spec.virtual_bids[bi];
        col_cost[input.vbid_offset + k] = match vb.direction {
            VirtualBidDirection::Inc => pu_h_cost(vb.price_per_mwh, input.base, dt_h),
            VirtualBidDirection::Dec => -pu_h_cost(vb.price_per_mwh, input.base, dt_h),
        };
    }

    if input.effective_co2_price > 0.0 {
        for j in 0..input.n_gen {
            col_cost[input.pg_offset + j] += pu_h_cost(
                input.effective_co2_price * input.effective_co2_rate[j],
                input.base,
                dt_h,
            );
        }
    }

    let has_dl_quadratic = input.dl_list.iter().any(|dl| {
        dl.cost_model
            .dc_quadratic_obj_coeff(input.network.base_mva)
            .abs()
            > 1e-20
    });
    let has_quadratic = q_diag.iter().any(|&v| v.abs() > 1e-20) || has_dl_quadratic;
    let (q_start, q_index, q_value) = if has_quadratic {
        let mut q_start = Vec::with_capacity(input.n_var + 1);
        let mut q_index = Vec::new();
        let mut q_value = Vec::new();

        for _ in 0..input.n_bus {
            q_start.push(q_index.len() as i32);
        }
        for (j, &qd) in q_diag.iter().enumerate().take(input.n_gen) {
            q_start.push(q_index.len() as i32);
            if qd.abs() > 1e-20 {
                q_index.push((input.pg_offset + j) as i32);
                q_value.push(qd);
            }
        }
        let n_sto_vars = 3 * input.n_storage + input.n_sto_dis_epi + input.n_sto_ch_epi;
        for _ in 0..n_sto_vars {
            q_start.push(q_index.len() as i32);
        }
        for _ in 0..input.n_hvdc_vars {
            q_start.push(q_index.len() as i32);
        }
        for _ in 0..input.n_pwl_gen {
            q_start.push(q_index.len() as i32);
        }
        for (k, dl) in input.dl_list.iter().enumerate() {
            q_start.push(q_index.len() as i32);
            // dc_quadratic_obj_coeff returns b · base² (the full quadratic
            // coefficient on the pu LP variable). Multiply by dt_h to land
            // in $/pu² for the period — same reasoning as the linear branch.
            let q_dl = dl.cost_model.dc_quadratic_obj_coeff(input.network.base_mva) * dt_h;
            if q_dl.abs() > 1e-20 {
                q_index.push((input.dl_offset + k) as i32);
                q_value.push(q_dl);
            }
        }
        for _ in 0..input.n_vbid {
            q_start.push(q_index.len() as i32);
        }
        for _ in 0..input.n_block_vars {
            q_start.push(q_index.len() as i32);
        }
        for _ in 0..input.reserve_layout.n_reserve_vars {
            q_start.push(q_index.len() as i32);
        }
        for _ in 0..input.n_blk_res_vars {
            q_start.push(q_index.len() as i32);
        }
        for _ in 0..(2 * input.n_bus
            + input.n_pb_curt_segs
            + input.n_pb_excess_segs
            + 2 * input.n_branch_flow
            + 2 * input.n_fg_rows
            + 2 * input.n_iface_rows
            + 2 * input.n_gen
            + 2 * input.layout.n_angle_diff_rows)
        {
            q_start.push(q_index.len() as i32);
        }
        // SCED-AC Benders eta epigraph column (when active) has no quadratic
        // cost; it contributes one empty column to the Hessian. Without this
        // push, q_start is one entry short of `n_var + 1` and the downstream
        // Gurobi/HiGHS backends read past the end of the slice in release
        // builds (debug builds would hit the debug_assert below).
        if input.layout.benders_eta_col().is_some() {
            q_start.push(q_index.len() as i32);
        }
        // Explicit contingency columns (penalty, worst, avg) have no
        // quadratic cost; push empty Hessian entries for each.
        if let Some(ctg_base) = input.layout.explicit_ctg_base {
            let n_ctg_cols = input.n_var - ctg_base;
            for _ in 0..n_ctg_cols {
                q_start.push(q_index.len() as i32);
            }
        }
        q_start.push(q_index.len() as i32);
        debug_assert_eq!(q_start.len(), input.n_var + 1);

        (Some(q_start), Some(q_index), Some(q_value))
    } else {
        (None, None, None)
    };

    // Eqs (15)-(16): bus power balance penalties are $/(pu·h).
    let pb_curt_penalty = input
        .spec
        .power_balance_penalty
        .curtailment
        .first()
        .map(|(_, price)| pu_h_cost(*price, input.base, dt_h));
    if let Some(penalty) = pb_curt_penalty {
        for s in 0..input.n_pb_curt_segs {
            col_cost[input.layout.pb_curtailment_seg_col(s)] = penalty;
        }
    }
    let pb_excess_penalty = input
        .spec
        .power_balance_penalty
        .excess
        .first()
        .map(|(_, price)| pu_h_cost(*price, input.base, dt_h));
    if let Some(penalty) = pb_excess_penalty {
        for s in 0..input.n_pb_excess_segs {
            col_cost[input.layout.pb_excess_seg_col(s)] = penalty;
        }
    }

    // Eqs (138)-(141), (158): branch overload penalty in $/(pu·h).
    let thermal_penalty = pu_h_cost(
        input.spec.thermal_penalty_curve.marginal_cost_at(0.0),
        input.base,
        dt_h,
    );
    for row_idx in 0..input.n_branch_flow {
        col_cost[input.layout.branch_lower_slack_col(row_idx)] = thermal_penalty;
        col_cost[input.layout.branch_upper_slack_col(row_idx)] = thermal_penalty;
    }
    for row_idx in 0..input.n_fg_rows {
        col_cost[input.layout.flowgate_lower_slack_col(row_idx)] = thermal_penalty;
        col_cost[input.layout.flowgate_upper_slack_col(row_idx)] = thermal_penalty;
    }
    for row_idx in 0..input.n_iface_rows {
        col_cost[input.layout.interface_lower_slack_col(row_idx)] = thermal_penalty;
        col_cost[input.layout.interface_upper_slack_col(row_idx)] = thermal_penalty;
    }

    // Ramp slack penalty (eqs 71-74 soft-constraint mode).
    let ramp_penalty = pu_h_cost(
        input.spec.ramp_penalty_curve.marginal_cost_at(0.0),
        input.base,
        dt_h,
    );
    for j in 0..input.n_gen {
        col_cost[input.layout.ramp_up_slack_col(j)] = ramp_penalty;
        col_cost[input.layout.ramp_down_slack_col(j)] = ramp_penalty;
    }

    // Angle difference slack penalty. The penalty cost is $/rad, so no
    // base_mva scaling is needed — the slack variables are in radians.
    let angle_penalty_per_rad = input.spec.angle_penalty_curve.marginal_cost_at(0.0) * dt_h;
    for row_idx in 0..input.layout.n_angle_diff_rows {
        col_cost[input.layout.angle_diff_lower_slack_col(row_idx)] = angle_penalty_per_rad;
        col_cost[input.layout.angle_diff_upper_slack_col(row_idx)] = angle_penalty_per_rad;
    }

    ScedObjectiveState {
        col_cost,
        c0_total,
        q_start,
        q_index,
        q_value,
    }
}
