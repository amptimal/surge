// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC LP re-pricing helpers.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use surge_network::Network;
use surge_opf::backends::{LpOptions, LpResult, LpSolveStatus, SparseProblem};
use tracing::info;

use super::layout::{ScucFozGenInfo, ScucLayout, ScucPhModeInfo};
use super::losses::{ScucLossIterationInput, iterate_loss_factors};
use super::plan::ScucCcPlantInfo;
use super::problem::ScucProblemState;
use crate::common::dc::{
    DcNomogramTighteningInput, DcSolveSession, DcSparseProblemInput, build_sparse_problem,
    solve_sparse_problem, tighten_nomograms,
};
use crate::common::reserves::{ReserveLpLayout, ReserveResults};
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;

fn log_scuc_pricing_trace(message: impl AsRef<str>) {
    info!("scuc_pricing: {}", message.as_ref());
}

fn cap_logic_row_span(
    row_labels: &[String],
    reserve_row_base: usize,
    hour: usize,
) -> Option<std::ops::Range<usize>> {
    let cap_logic_prefix = format!("h{hour}:cap_logic_");
    let mut cap_logic_start = reserve_row_base;
    while cap_logic_start > 0
        && row_labels
            .get(cap_logic_start - 1)
            .is_some_and(|label| label.starts_with(&cap_logic_prefix))
    {
        cap_logic_start -= 1;
    }

    let mut cap_logic_end = reserve_row_base;
    while row_labels
        .get(cap_logic_end)
        .is_some_and(|label| label.starts_with(&cap_logic_prefix))
    {
        cap_logic_end += 1;
    }

    (cap_logic_start < cap_logic_end).then_some(cap_logic_start..cap_logic_end)
}

pub(super) struct PricingSummary {
    pub pricing_converged: bool,
    pub lmp_out: Vec<Vec<f64>>,
    pub dloss_dp_out: Vec<Vec<f64>>,
    pub hourly_reserve_results: Vec<Option<ReserveResults>>,
    pub branch_shadow_prices: Vec<Vec<f64>>,
    pub fg_shadow_prices: Vec<Vec<f64>>,
    pub iface_shadow_prices: Vec<Vec<f64>>,
}

pub(super) struct PricingLpState {
    pub lp_prob: SparseProblem,
}

pub(super) struct PricingRunState<'a> {
    pub primary_state: ScucProblemState<'a>,
    /// Post-repricing LP solution. `None` in the skip-pricing path —
    /// callers that index into `x` / `row_dual` must gate on
    /// `summary.pricing_converged` (always false when `lp_sol` is
    /// None). This avoids a ~few-MB pointless clone of the primary
    /// MIP solution for large cases.
    pub lp_sol: Option<LpResult>,
    pub summary: PricingSummary,
}

pub(super) struct FozBinaryGroup {
    pub delta_offset: usize,
    pub n_segments: usize,
    pub phi_offset: usize,
    pub n_zones: usize,
    pub rho_offset: usize,
}

pub(super) struct PhModeBinaryOffsets {
    pub m_gen_offset: usize,
    pub m_pump_offset: usize,
}

pub(super) struct CcBinaryBlock {
    pub z_block_off: usize,
    pub n_configs: usize,
}

pub(super) struct PricingBinaryMetadata {
    pub foz_groups: Vec<FozBinaryGroup>,
    pub ph_mode_offsets: Vec<PhModeBinaryOffsets>,
    pub cc_blocks: Vec<CcBinaryBlock>,
}

fn fixed_binary_value(x: &[f64], idx: usize) -> f64 {
    if x.get(idx).copied().unwrap_or(0.0) > 0.5 {
        1.0
    } else {
        0.0
    }
}

fn effective_startup_output_cap_pu(
    generator: &surge_network::network::Generator,
    dt_hours: f64,
    hour: usize,
    base: f64,
) -> f64 {
    let startup_cap_pu = generator.startup_ramp_mw_per_period(dt_hours) / base;
    let pmin_pu = generator.pmin / base;
    if hour > 0 && pmin_pu > startup_cap_pu + 1e-12 {
        pmin_pu
    } else {
        startup_cap_pu
    }
}

fn fixed_commitment_trajectory_offset_pu(
    generator: &surge_network::network::Generator,
    spec: &DispatchProblemSpec<'_>,
    hour: usize,
    n_hours: usize,
    startup_active: &[bool],
    shutdown_active: &[bool],
    initial_dispatch_mw: Option<f64>,
    base: f64,
) -> f64 {
    let mut trajectory_pu = 0.0;

    let startup_rate_mw_per_hour = generator.startup_ramp_mw_per_period(1.0);
    if startup_rate_mw_per_hour.is_finite() && startup_rate_mw_per_hour < 1e10 {
        for startup_hour in hour + 1..n_hours {
            if !startup_active.get(startup_hour).copied().unwrap_or(false) {
                continue;
            }
            let trajectory_mw = generator.pmin
                - startup_rate_mw_per_hour
                    * (spec.period_end_hours(startup_hour) - spec.period_end_hours(hour));
            if trajectory_mw > 1e-9 {
                trajectory_pu += trajectory_mw / base;
            }
        }
    }

    let shutdown_rate_mw_per_hour = generator.shutdown_ramp_mw_per_period(1.0);
    if shutdown_rate_mw_per_hour.is_finite() && shutdown_rate_mw_per_hour < 1e10 {
        let initial_shutdown_anchor_mw = initial_dispatch_mw.unwrap_or(generator.pmin);
        for shutdown_hour in 0..=hour {
            if !shutdown_active.get(shutdown_hour).copied().unwrap_or(false) {
                continue;
            }
            let shutdown_anchor_mw = if shutdown_hour == 0 {
                initial_shutdown_anchor_mw
            } else {
                generator.pmin
            };
            let trajectory_mw = shutdown_anchor_mw
                - shutdown_rate_mw_per_hour
                    * (spec.period_end_hours(hour) - spec.period_start_hours(shutdown_hour));
            if trajectory_mw > 1e-9 {
                trajectory_pu += trajectory_mw / base;
            }
        }
    }

    trajectory_pu
}

#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
fn apply_fixed_commitment_pricing_transform(
    lp_prob: &mut SparseProblem,
    primary_state: &ScucProblemState<'_>,
    spec: &DispatchProblemSpec<'_>,
    layout: &ScucLayout,
    n_bus: usize,
    n_pb_curt_segs: usize,
    n_pb_excess_segs: usize,
    hourly_networks: &[Network],
    gen_indices: &[usize],
    n_hours: usize,
    _n_gen: usize,
    dt_hours: f64,
    base: f64,
    enforce_shutdown_deloading: bool,
    offline_commitment_trajectories: bool,
) {
    let skip_bus_pb = spec.scuc_disable_bus_power_balance;
    for t in 0..n_hours {
        if !skip_bus_pb {
            for bus_idx in 0..n_bus {
                for col in [
                    layout.pb_curtailment_bus_col(t, bus_idx),
                    layout.pb_excess_bus_col(t, bus_idx),
                ] {
                    if primary_state
                        .solution
                        .x
                        .get(col)
                        .copied()
                        .unwrap_or(0.0)
                        .abs()
                        <= 1e-9
                    {
                        lp_prob.col_lower[col] = 0.0;
                        lp_prob.col_upper[col] = 0.0;
                    }
                }
            }
            for seg_idx in 0..n_pb_curt_segs {
                let col = layout.pb_curtailment_seg_col(t, seg_idx);
                if primary_state
                    .solution
                    .x
                    .get(col)
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    <= 1e-9
                {
                    lp_prob.col_lower[col] = 0.0;
                    lp_prob.col_upper[col] = 0.0;
                }
            }
            for seg_idx in 0..n_pb_excess_segs {
                let col = layout.pb_excess_seg_col(t, seg_idx);
                if primary_state
                    .solution
                    .x
                    .get(col)
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    <= 1e-9
                {
                    lp_prob.col_lower[col] = 0.0;
                    lp_prob.col_upper[col] = 0.0;
                }
            }
        }

        for (j, &gi) in gen_indices.iter().enumerate() {
            let generator = &hourly_networks[t].generators[gi];
            let u = fixed_binary_value(&primary_state.solution.x, layout.commitment_col(t, j));
            let v = fixed_binary_value(&primary_state.solution.x, layout.startup_col(t, j));
            let shutdown_next = if t + 1 < n_hours {
                fixed_binary_value(&primary_state.solution.x, layout.shutdown_col(t + 1, j))
            } else {
                0.0
            };
            let startup_schedule: Vec<bool> = (0..n_hours)
                .map(|hour| {
                    fixed_binary_value(&primary_state.solution.x, layout.startup_col(hour, j)) > 0.5
                })
                .collect();
            let shutdown_schedule: Vec<bool> = (0..n_hours)
                .map(|hour| {
                    fixed_binary_value(&primary_state.solution.x, layout.shutdown_col(hour, j))
                        > 0.5
                })
                .collect();

            let mut effective_upper = generator.pmax / base * u;
            let mut effective_lower = generator.pmin / base * u;
            if offline_commitment_trajectories {
                let trajectory_offset_pu = fixed_commitment_trajectory_offset_pu(
                    generator,
                    spec,
                    t,
                    n_hours,
                    &startup_schedule,
                    &shutdown_schedule,
                    spec.prev_dispatch_mw_at(j),
                    base,
                );
                effective_upper += trajectory_offset_pu;
                effective_lower += trajectory_offset_pu;
            }

            if enforce_shutdown_deloading
                && !generator.is_storage()
                && !offline_commitment_trajectories
            {
                let pmax_pu = generator.pmax / base;
                let startup_cap_pu =
                    effective_startup_output_cap_pu(generator, dt_hours, t, base).min(pmax_pu);
                let startup_upper_delta = pmax_pu - startup_cap_pu;
                if startup_upper_delta > 1e-12 {
                    effective_upper -= startup_upper_delta * v;
                }

                let startup_lower_delta = generator.pmin / base - startup_cap_pu.max(0.0);
                if startup_lower_delta > 1e-12 {
                    effective_lower -= startup_lower_delta * v;
                }

                if t + 1 < n_hours {
                    let shutdown_cap_pu =
                        (generator.shutdown_ramp_mw_per_period(dt_hours) / base).min(pmax_pu);
                    let shutdown_upper_delta = pmax_pu - shutdown_cap_pu;
                    if shutdown_upper_delta > 1e-12 {
                        effective_upper -= shutdown_upper_delta * shutdown_next;
                    }

                    let shutdown_lower_delta = generator.pmin / base - shutdown_cap_pu.max(0.0);
                    if shutdown_lower_delta > 1e-12 {
                        effective_lower -= shutdown_lower_delta * shutdown_next;
                    }
                }
            }

            let pg_col = layout.pg_col(t, j);
            if !generator.is_storage() && effective_lower.abs() > 1e-9 {
                let col_start = lp_prob.a_start[pg_col] as usize;
                let col_end = lp_prob.a_start[pg_col + 1] as usize;
                for nz in col_start..col_end {
                    let row = lp_prob.a_index[nz] as usize;
                    let coeff = lp_prob.a_value[nz];
                    if lp_prob.row_lower[row].is_finite() {
                        lp_prob.row_lower[row] -= coeff * effective_lower;
                    }
                    if lp_prob.row_upper[row].is_finite() {
                        lp_prob.row_upper[row] -= coeff * effective_lower;
                    }
                }
                lp_prob.col_lower[pg_col] = 0.0;
                lp_prob.col_upper[pg_col] = (effective_upper - effective_lower).max(0.0);
            } else {
                lp_prob.col_lower[pg_col] = effective_lower.min(effective_upper);
                lp_prob.col_upper[pg_col] = effective_upper.max(effective_lower);
            }

            for col in [
                layout.headroom_slack_col(t, j),
                layout.footroom_slack_col(t, j),
            ] {
                if primary_state
                    .solution
                    .x
                    .get(col)
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    <= 1e-9
                {
                    lp_prob.col_lower[col] = 0.0;
                    lp_prob.col_upper[col] = 0.0;
                }
            }
        }

        let reserve_row_base = primary_state.problem.hour_reserve_row_bases[t];
        let Some(cap_logic_rows) =
            cap_logic_row_span(&primary_state.problem.row_labels, reserve_row_base, t)
        else {
            continue;
        };

        for row in cap_logic_rows {
            lp_prob.row_lower[row] = f64::NEG_INFINITY;
            lp_prob.row_upper[row] = f64::INFINITY;
        }
    }
}

fn dump_pricing_duals(
    path: &Path,
    row_labels: &[String],
    row_lower: &[f64],
    row_upper: &[f64],
    row_dual: &[f64],
) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "row_index\tlabel\trow_lower\trow_upper\trow_dual")?;
    for (row_idx, &dual) in row_dual.iter().enumerate() {
        let label = row_labels.get(row_idx).map(String::as_str).unwrap_or("");
        let lower = row_lower.get(row_idx).copied().unwrap_or(f64::NAN);
        let upper = row_upper.get(row_idx).copied().unwrap_or(f64::NAN);
        writeln!(
            writer,
            "{row_idx}\t{label}\t{lower:.12}\t{upper:.12}\t{dual:.12}",
        )?;
    }
    writer.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn dump_pricing_columns(
    path: &Path,
    layout: &ScucLayout,
    n_gen: usize,
    n_hours: usize,
    col_cost: &[f64],
    col_lower: &[f64],
    col_upper: &[f64],
    col_value: &[f64],
    col_dual: &[f64],
) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(
        writer,
        "col_index\tlabel\tcol_cost\tcol_lower\tcol_upper\tcol_value\tcol_dual",
    )?;
    let mut emit = |idx: usize, label: String| -> std::io::Result<()> {
        let cost = col_cost.get(idx).copied().unwrap_or(f64::NAN);
        let lower = col_lower.get(idx).copied().unwrap_or(f64::NAN);
        let upper = col_upper.get(idx).copied().unwrap_or(f64::NAN);
        let value = col_value.get(idx).copied().unwrap_or(f64::NAN);
        let dual = col_dual.get(idx).copied().unwrap_or(f64::NAN);
        writeln!(
            writer,
            "{idx}\t{label}\t{cost:.12e}\t{lower:.12e}\t{upper:.12e}\t{value:.12e}\t{dual:.12e}",
        )
    };
    // Only dump for hour 0 to keep the file small.
    let n_e_g = layout.dispatch.dl - layout.dispatch.e_g;
    let n_dl = layout.dispatch.vbid - layout.dispatch.dl;
    for j in 0..n_gen {
        emit(layout.pg_col(0, j), format!("h0:pg_{j}"))?;
    }
    for k in 0..n_e_g {
        emit(
            layout.col(0, layout.dispatch.e_g + k),
            format!("h0:e_g_{k}"),
        )?;
    }
    for k in 0..n_dl {
        emit(layout.col(0, layout.dispatch.dl + k), format!("h0:dl_{k}"))?;
    }
    let _ = n_hours; // for future expansion
    writer.flush()?;
    Ok(())
}

pub(super) fn build_binary_metadata(
    layout: &ScucLayout,
    foz_gens: &[ScucFozGenInfo],
    ph_mode_infos: &[ScucPhModeInfo],
    cc_infos: &[ScucCcPlantInfo],
) -> PricingBinaryMetadata {
    let foz_groups = foz_gens
        .iter()
        .map(|group| FozBinaryGroup {
            delta_offset: layout.foz_delta + group.delta_local_off,
            n_segments: group.segments.len(),
            phi_offset: layout.foz_phi + group.phi_local_off,
            n_zones: group.zones.len(),
            rho_offset: layout.foz_rho + group.rho_local_off,
        })
        .collect();
    let ph_mode_offsets = ph_mode_infos
        .iter()
        .map(|info| PhModeBinaryOffsets {
            m_gen_offset: layout.ph_mode + info.m_gen_local_off,
            m_pump_offset: layout.ph_mode + info.m_pump_local_off,
        })
        .collect();
    let cc_blocks = cc_infos
        .iter()
        .map(|info| CcBinaryBlock {
            z_block_off: info.z_block_off,
            n_configs: info.n_configs,
        })
        .collect();

    PricingBinaryMetadata {
        foz_groups,
        ph_mode_offsets,
        cc_blocks,
    }
}

pub(super) struct PricingLpInitInput<'a> {
    pub mip_solution: &'a LpResult,
    pub is_fixed_commitment: bool,
    pub layout: &'a ScucLayout,
    pub n_var: usize,
    pub n_row: usize,
    pub n_hours: usize,
    pub n_gen: usize,
    pub cc_var_base: usize,
    pub foz_groups: &'a [FozBinaryGroup],
    pub ph_mode_offsets: &'a [PhModeBinaryOffsets],
    pub cc_blocks: &'a [CcBinaryBlock],
    pub col_cost: Vec<f64>,
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
    pub row_lower: Vec<f64>,
    pub row_upper: Vec<f64>,
    pub a_start: Vec<i32>,
    pub a_index: Vec<i32>,
    pub a_value: Vec<f64>,
}

pub(super) fn initialize_pricing_lp(
    input: PricingLpInitInput<'_>,
) -> Result<PricingLpState, ScedError> {
    let mut lp_prob = build_sparse_problem(DcSparseProblemInput {
        n_col: input.n_var,
        n_row: input.n_row,
        col_cost: input.col_cost,
        col_lower: input.col_lower,
        col_upper: input.col_upper,
        row_lower: input.row_lower,
        row_upper: input.row_upper,
        a_start: input.a_start,
        a_index: input.a_index,
        a_value: input.a_value,
        q_start: None,
        q_index: None,
        q_value: None,
        col_names: None,
        row_names: None,
        integrality: None,
    });

    if input.is_fixed_commitment {
        return Ok(PricingLpState { lp_prob });
    }

    for t in 0..input.n_hours {
        for j in 0..input.n_gen {
            for col in [
                input.layout.commitment_col(t, j),
                input.layout.startup_col(t, j),
                input.layout.shutdown_col(t, j),
            ] {
                let fixed = if input.mip_solution.x[col] > 0.5 {
                    1.0
                } else {
                    0.0
                };
                lp_prob.col_lower[col] = fixed;
                lp_prob.col_upper[col] = fixed;
            }
        }

        for fg in input.foz_groups {
            for k in 0..fg.n_segments {
                let idx = input.layout.col(t, fg.delta_offset + k);
                let fixed = if input.mip_solution.x[idx] > 0.5 {
                    1.0
                } else {
                    0.0
                };
                lp_prob.col_lower[idx] = fixed;
                lp_prob.col_upper[idx] = fixed;
            }
            for z in 0..fg.n_zones {
                let phi_idx = input.layout.col(t, fg.phi_offset + z);
                let phi_fixed = if input.mip_solution.x[phi_idx] > 0.5 {
                    1.0
                } else {
                    0.0
                };
                lp_prob.col_lower[phi_idx] = phi_fixed;
                lp_prob.col_upper[phi_idx] = phi_fixed;

                let rho_idx = input.layout.col(t, fg.rho_offset + z);
                let rho_fixed = if input.mip_solution.x[rho_idx] > 0.5 {
                    1.0
                } else {
                    0.0
                };
                lp_prob.col_lower[rho_idx] = rho_fixed;
                lp_prob.col_upper[rho_idx] = rho_fixed;
            }
        }

        for ph in input.ph_mode_offsets {
            for idx in [
                input.layout.col(t, ph.m_gen_offset),
                input.layout.col(t, ph.m_pump_offset),
            ] {
                let fixed = if input.mip_solution.x[idx] > 0.5 {
                    1.0
                } else {
                    0.0
                };
                lp_prob.col_lower[idx] = fixed;
                lp_prob.col_upper[idx] = fixed;
            }
        }
    }

    for block in input.cc_blocks {
        for c in 0..block.n_configs {
            let z_base = input.cc_var_base + block.z_block_off + c * input.n_hours;
            let yup_base = input.cc_var_base
                + block.z_block_off
                + block.n_configs * input.n_hours
                + c * input.n_hours;
            let ydn_base = input.cc_var_base
                + block.z_block_off
                + 2 * block.n_configs * input.n_hours
                + c * input.n_hours;
            for t in 0..input.n_hours {
                for idx in [z_base + t, yup_base + t, ydn_base + t] {
                    let fixed = if input.mip_solution.x[idx] > 0.5 {
                        1.0
                    } else {
                        0.0
                    };
                    lp_prob.col_lower[idx] = fixed;
                    lp_prob.col_upper[idx] = fixed;
                }
            }
        }
    }

    Ok(PricingLpState { lp_prob })
}

pub(super) struct PricingSummaryInput<'a> {
    pub spec: &'a DispatchProblemSpec<'a>,
    pub network: &'a Network,
    pub lp_sol: &'a LpResult,
    pub reserve_layout: &'a ReserveLpLayout,
    pub constrained_branches: &'a [usize],
    pub fg_rows: &'a [usize],
    pub iface_rows: &'a [usize],
    pub n_flow: usize,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub hour_row_bases: &'a [usize],
    pub hour_reserve_row_bases: &'a [usize],
    pub n_hours: usize,
    pub n_row: usize,
    pub n_bus: usize,
    pub n_gen: usize,
    pub n_storage: usize,
    pub n_dl: usize,
    pub vars_per_hour: usize,
    pub base: f64,
}

pub(super) struct PricingRunInput<'a> {
    pub network: &'a Network,
    pub solve: &'a DcSolveSession<'a>,
    pub primary_state: ScucProblemState<'a>,
}

fn zero_pricing_summary(n_hours: usize, n_bus: usize) -> PricingSummary {
    PricingSummary {
        pricing_converged: false,
        lmp_out: vec![vec![0.0; n_bus]; n_hours],
        dloss_dp_out: vec![vec![0.0; n_bus]; n_hours],
        hourly_reserve_results: (0..n_hours).map(|_| None).collect(),
        branch_shadow_prices: vec![],
        fg_shadow_prices: vec![],
        iface_shadow_prices: vec![],
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_hourly_reserve_results(
    reserve_layout: &ReserveLpLayout,
    solution: &LpResult,
    row_dual: Option<&[f64]>,
    hour_reserve_row_bases: &[usize],
    n_hours: usize,
    n_gen: usize,
    n_storage: usize,
    n_dl: usize,
    vars_per_hour: usize,
    base: f64,
) -> Vec<Option<ReserveResults>> {
    if reserve_layout.products.is_empty() {
        return (0..n_hours).map(|_| None).collect();
    }

    (0..n_hours)
        .map(|t| {
            let col_base = t * vars_per_hour;
            Some(crate::common::reserves::extract_results(
                reserve_layout,
                &solution.x[col_base..],
                row_dual,
                hour_reserve_row_bases[t],
                n_gen,
                n_storage,
                n_dl,
                base,
            ))
        })
        .collect()
}

fn extract_reserve_awards_without_repricing(
    input: &PricingRunInput<'_>,
) -> Vec<Option<ReserveResults>> {
    let n_hours = input.solve.spec.n_periods;
    let layout_plan = &input.primary_state.problem_plan.model_plan.layout;
    let reserve_layout = &layout_plan.active.reserve_layout;
    let n_gen = input.network.generators.len();
    let n_storage = input
        .network
        .generators
        .iter()
        .filter(|generator| generator.is_storage())
        .count();
    let n_dl = layout_plan.active.dl_list.len();
    let base = input.network.base_mva.max(1.0);

    // Skip path: no duals available and none needed. Passing `None`
    // avoids both the O(n_row) zero-vec allocation and the dual-reading
    // branches inside `extract_results`.
    extract_hourly_reserve_results(
        reserve_layout,
        &input.primary_state.solution,
        None,
        &input.primary_state.problem.hour_reserve_row_bases,
        n_hours,
        n_gen,
        n_storage,
        n_dl,
        layout_plan.vars_per_hour,
        base,
    )
}

fn preserve_primary_reserve_clearing(
    repriced_results: &mut [Option<ReserveResults>],
    primary_results: &[Option<ReserveResults>],
) {
    for (repriced, primary) in repriced_results.iter_mut().zip(primary_results.iter()) {
        match (repriced.as_mut(), primary.as_ref()) {
            (Some(repriced), Some(primary)) => {
                repriced.awards = primary.awards.clone();
                repriced.dl_awards = primary.dl_awards.clone();
                repriced.provided = primary.provided.clone();
                repriced.shortfall = primary.shortfall.clone();
                repriced.zonal_shortfall = primary.zonal_shortfall.clone();
            }
            (None, Some(primary)) => *repriced = Some(primary.clone()),
            _ => {}
        }
    }
}

pub(super) fn skip_pricing(input: PricingRunInput<'_>) -> PricingRunState<'_> {
    let n_hours = input.solve.spec.n_periods;
    let n_bus = input.network.n_buses();
    info!(
        hours = n_hours,
        buses = n_bus,
        "SCUC: skipping LP repricing and emitting zero-valued pricing outputs"
    );
    let hourly_reserve_results = extract_reserve_awards_without_repricing(&input);
    let primary_state = input.primary_state;
    let mut summary = zero_pricing_summary(n_hours, n_bus);
    summary.hourly_reserve_results = hourly_reserve_results;
    PricingRunState {
        // No pricing ran — no LP solution to carry. `pricing_converged`
        // stays false, so downstream readers won't reach in.
        lp_sol: None,
        summary,
        primary_state,
    }
}

/// Re-solve SCUC as an LP with fixed commitment binaries to obtain pricing duals.
pub(super) fn run_pricing(input: PricingRunInput<'_>) -> Result<PricingRunState<'_>, ScedError> {
    let PricingRunInput {
        network,
        solve,
        mut primary_state,
    } = input;
    let spec = &solve.spec;
    let solver = solve.solver.as_ref();
    let setup = &solve.setup;
    let base = solve.base_mva;
    if !primary_state.is_fixed_commitment {
        info!("SCUC: re-solving LP with fixed binaries for LMP extraction");
    }
    let model_plan = primary_state.problem_plan.model_plan;
    let layout_plan = &model_plan.layout;
    let layout = &layout_plan.layout;
    let active_inputs = &layout_plan.active;
    let network_plan = &model_plan.network_plan;
    let variable_plan = &model_plan.variable;
    let n_hours = spec.n_periods;
    let n_bus = network.n_buses();
    let n_gen = setup.n_gen;
    let n_storage = setup.n_storage;
    // Primary (MIP) solution has no duals; we only need award
    // quantities here. Passing None avoids allocating a zero-filled
    // Vec<f64> of `n_row` length for nothing.
    let primary_hourly_reserve_results = extract_hourly_reserve_results(
        &active_inputs.reserve_layout,
        &primary_state.solution,
        None,
        &primary_state.problem.hour_reserve_row_bases,
        n_hours,
        n_gen,
        n_storage,
        active_inputs.dl_list.len(),
        layout_plan.vars_per_hour,
        base,
    );
    let binary_metadata = build_binary_metadata(
        layout,
        &layout_plan.foz_gens,
        &layout_plan.ph_mode_infos,
        &variable_plan.cc_infos,
    );
    let pricing_lp = initialize_pricing_lp(PricingLpInitInput {
        mip_solution: &primary_state.solution,
        is_fixed_commitment: primary_state.is_fixed_commitment,
        layout,
        n_var: variable_plan.n_var,
        n_row: primary_state.problem.n_row,
        n_hours,
        n_gen,
        cc_var_base: variable_plan.cc_var_base,
        foz_groups: &binary_metadata.foz_groups,
        ph_mode_offsets: &binary_metadata.ph_mode_offsets,
        cc_blocks: &binary_metadata.cc_blocks,
        col_cost: primary_state.problem_plan.columns.col_cost.clone(),
        col_lower: primary_state.problem_plan.columns.col_lower.clone(),
        col_upper: primary_state.problem_plan.columns.col_upper.clone(),
        row_lower: primary_state.problem.row_lower.clone(),
        row_upper: primary_state.problem.row_upper.clone(),
        a_start: primary_state.problem.a_start.clone(),
        a_index: primary_state.problem.a_index.clone(),
        a_value: primary_state.problem.a_value.clone(),
    })?;
    let mut lp_prob = pricing_lp.lp_prob;

    apply_fixed_commitment_pricing_transform(
        &mut lp_prob,
        &primary_state,
        spec,
        layout,
        n_bus,
        layout_plan.n_pb_curt_segs,
        layout_plan.n_pb_excess_segs,
        &model_plan.hourly_networks,
        &setup.gen_indices,
        n_hours,
        n_gen,
        spec.dt_hours,
        base,
        input.solve.spec.enforce_shutdown_deloading,
        input.solve.spec.offline_commitment_trajectories,
    );

    let mut lp_sol = solve_sparse_problem(solver, &lp_prob, spec.tolerance, None)?;

    let lp_opts_fast = LpOptions {
        tolerance: spec.tolerance,
        time_limit_secs: None,
        ..Default::default()
    };
    tighten_nomograms(DcNomogramTighteningInput {
        network,
        spec,
        solver,
        lp_opts: &lp_opts_fast,
        lp_sol: &mut lp_sol,
        lp_prob: &mut lp_prob,
        fg_rows: &network_plan.fg_rows,
        fg_limits: &mut primary_state.problem_plan.network_rows.fg_limits,
        compute_flow_mw: |_, fgi, lp_sol| {
            let resolved = &setup.resolved_flowgates[fgi];
            let mut worst = 0.0_f64;
            for t in 0..n_hours {
                let mut flow_pu = 0.0;
                for term in &resolved.terms {
                    flow_pu += term.theta_coeff
                        * (lp_sol.x[layout.theta_col(t, term.from_bus_idx)]
                            - lp_sol.x[layout.theta_col(t, term.to_bus_idx)]);
                }
                let flow_mw = flow_pu * base;
                if flow_mw.abs() > worst.abs() {
                    worst = flow_mw;
                }
            }
            worst
        },
        apply_limit: |lp_prob, ri, new_limit| {
            for t in 0..n_hours {
                let row = primary_state.problem.hour_row_bases[t]
                    + primary_state.problem.n_branch_flow
                    + ri;
                lp_prob.row_lower[row] = -new_limit / base
                    - primary_state.problem_plan.network_rows.fg_shift_offsets[ri];
                lp_prob.row_upper[row] =
                    new_limit / base - primary_state.problem_plan.network_rows.fg_shift_offsets[ri];
            }
        },
    })?;

    // Pricing LP runs its own (fresh) loss-factor iteration against
    // the pricing problem matrix — build a dedicated prep struct for
    // it rather than reusing the SCUC primary-solve prep (different
    // SparseProblem, different `a_value` positions). No warm start is
    // supplied here: pricing is a one-shot LP re-solve that already
    // inherits the SCUC commitment, so the initial coefficients are
    // already close to the converged loss-adjusted state via the
    // prior SCUC iteration's outputs.
    //
    // Skipped entirely when per-bus balance is disabled — loss
    // factors require the per-bus KCL rows to read and adjust.
    let n_flow_pricing = primary_state.problem.n_branch_flow
        + primary_state.problem.n_fg_rows
        + network_plan.iface_rows.len();
    let dloss_dp_out: Vec<Vec<f64>>;
    if spec.scuc_disable_bus_power_balance {
        // Per-bus balance is disabled, so loss-factor refinement
        // (which rewrites per-bus pg coefficients and bus-balance
        // RHS) is not applicable. Solve the pricing LP once with the
        // system-level expected loss already baked into the RHS by
        // `build_system_power_balance_row`.
        lp_sol = crate::common::dc::solve_sparse_problem(solver, &lp_prob, spec.tolerance, None)?;
        dloss_dp_out = vec![vec![0.0_f64; n_bus]; n_hours];
    } else {
        let pricing_prep = crate::scuc::losses::build_loss_factor_prep(
            &lp_prob,
            &model_plan.hourly_networks,
            &solve.bus_map,
            layout,
            &setup.gen_bus_idx,
            &primary_state.problem.hour_row_bases,
            n_flow_pricing,
            n_bus,
        )?;
        let loss_result = iterate_loss_factors(
            ScucLossIterationInput {
                solver,
                spec,
                hourly_networks: &model_plan.hourly_networks,
                bus_map: &solve.bus_map,
                layout,
                gen_bus_idx: &setup.gen_bus_idx,
                hour_row_bases: &primary_state.problem.hour_row_bases,
                n_flow: n_flow_pricing,
                n_bus,
                time_limit_secs: None,
                problem: &mut lp_prob,
                solution: &mut lp_sol,
            },
            &pricing_prep,
            None,
        )?;
        dloss_dp_out = loss_result.dloss_dp;
    }

    if let Some(path) = std::env::var_os("SURGE_DEBUG_DUMP_SCUC_PRICING_DUALS") {
        let path = Path::new(&path);
        dump_pricing_duals(
            path,
            &primary_state.problem.row_labels,
            &lp_prob.row_lower,
            &lp_prob.row_upper,
            &lp_sol.row_dual,
        )
        .map_err(|err| {
            ScedError::SolverError(format!(
                "failed to dump SCUC pricing duals to {}: {err}",
                path.display()
            ))
        })?;
        info!(path = %path.display(), "SCUC pricing: dumped pricing duals");
    }
    if let Some(path) = std::env::var_os("SURGE_DEBUG_DUMP_SCUC_PRICING_COLS") {
        let path = Path::new(&path);
        dump_pricing_columns(
            path,
            layout,
            n_gen,
            n_hours,
            &lp_prob.col_cost,
            &lp_prob.col_lower,
            &lp_prob.col_upper,
            &lp_sol.x,
            &lp_sol.col_dual,
        )
        .map_err(|err| {
            ScedError::SolverError(format!(
                "failed to dump SCUC pricing columns to {}: {err}",
                path.display()
            ))
        })?;
        info!(path = %path.display(), "SCUC pricing: dumped pricing columns");
    }
    if std::env::var_os("SURGE_DEBUG_SCUC_PRICING_LP_STATS").is_some() {
        // Quick stats on the pricing LP to verify scale.
        let max_cost = lp_prob
            .col_cost
            .iter()
            .fold(0.0_f64, |a, &b| a.max(b.abs()));
        let nz_costs = lp_prob.col_cost.iter().filter(|c| c.abs() > 1e-12).count();
        let max_dual = lp_sol.row_dual.iter().fold(0.0_f64, |a, &b| a.max(b.abs()));
        let max_neg_dual = lp_sol
            .row_dual
            .iter()
            .fold(0.0_f64, |a, &b| if b < a { b } else { a });
        let min_pos_dual = lp_sol
            .row_dual
            .iter()
            .filter(|&&b| b > 0.0)
            .fold(f64::INFINITY, |a, &b| if b < a { b } else { a });
        log_scuc_pricing_trace(format!(
            "lp_stats objective={:.6e} n_var={} n_row={} max|col_cost|={:.6e} nonzero_costs={} max|row_dual|={:.6e} max_neg_dual={:.6e} min_pos_dual={:.6e}",
            lp_sol.objective,
            lp_prob.n_col,
            lp_prob.n_row,
            max_cost,
            nz_costs,
            max_dual,
            max_neg_dual,
            min_pos_dual,
        ));

        // Sample bus balance row dual at hour 0 and the e_g col cost at first PWL gen
        let h0_balance_base = primary_state.problem.hour_row_bases[0]
            + primary_state.problem.n_branch_flow
            + primary_state.problem.n_fg_rows
            + primary_state.problem.n_branch_flow.saturating_sub(0);
        let _ = h0_balance_base; // computed via labels below
        // Find row labeled h0:bus_0 to be safe across different layouts
        if let Some(bus0_row) = primary_state
            .problem
            .row_labels
            .iter()
            .position(|s| s == "h0:bus_0")
        {
            log_scuc_pricing_trace(format!(
                "h0_bus_0 row_dual={:.6e} row_lower={:.6e} row_upper={:.6e}",
                lp_sol.row_dual[bus0_row], lp_prob.row_lower[bus0_row], lp_prob.row_upper[bus0_row]
            ));
        }
        if let Some(eg0_col) = (0..n_gen).find_map(|j| {
            let pg = layout.pg_col(0, j);
            if lp_prob.col_lower[pg].is_finite() && lp_prob.col_upper[pg] > lp_prob.col_lower[pg] {
                Some(pg)
            } else {
                None
            }
        }) {
            log_scuc_pricing_trace(format!(
                "sample_pg col={eg0_col} cost={:.6e} lower={:.6e} upper={:.6e} value={:.6e}",
                lp_prob.col_cost[eg0_col],
                lp_prob.col_lower[eg0_col],
                lp_prob.col_upper[eg0_col],
                lp_sol.x[eg0_col],
            ));
        }
        // First few e_g vars (epigraph variable per PWL gen)
        let n_e_g = layout.dispatch.dl - layout.dispatch.e_g;
        for k in 0..n_e_g.min(3) {
            let col = layout.col(0, layout.dispatch.e_g + k);
            log_scuc_pricing_trace(format!(
                "sample_e_g col={col} k={k} cost={:.6e} lower={:.6e} upper={:.6e} value={:.6e}",
                lp_prob.col_cost[col],
                lp_prob.col_lower[col],
                lp_prob.col_upper[col],
                lp_sol.x[col],
            ));
        }
    }

    let mut summary = extract_pricing_summary(PricingSummaryInput {
        spec,
        network,
        lp_sol: &lp_sol,
        reserve_layout: &active_inputs.reserve_layout,
        constrained_branches: &network_plan.constrained_branches,
        fg_rows: &network_plan.fg_rows,
        iface_rows: &network_plan.iface_rows,
        n_flow: primary_state.problem.n_branch_flow
            + primary_state.problem.n_fg_rows
            + network_plan.iface_rows.len(),
        n_branch_flow: primary_state.problem.n_branch_flow,
        n_fg_rows: primary_state.problem.n_fg_rows,
        hour_row_bases: &primary_state.problem.hour_row_bases,
        hour_reserve_row_bases: &primary_state.problem.hour_reserve_row_bases,
        n_hours,
        n_row: primary_state.problem.n_row,
        n_bus,
        n_gen,
        n_storage,
        n_dl: active_inputs.dl_list.len(),
        vars_per_hour: layout_plan.vars_per_hour,
        base,
    });
    preserve_primary_reserve_clearing(
        &mut summary.hourly_reserve_results,
        &primary_hourly_reserve_results,
    );
    summary.dloss_dp_out = dloss_dp_out;

    Ok(PricingRunState {
        primary_state,
        lp_sol: Some(lp_sol),
        summary,
    })
}

pub(super) fn extract_pricing_summary(input: PricingSummaryInput<'_>) -> PricingSummary {
    let pricing_converged = matches!(
        input.lp_sol.status,
        LpSolveStatus::Optimal | LpSolveStatus::SubOptimal
    ) && input.lp_sol.row_dual.len() >= input.n_row;

    let mut lmp_out = Vec::with_capacity(input.n_hours);
    if pricing_converged {
        for &row_base in input.hour_row_bases {
            let balance_base = row_base + input.n_flow;
            // Sign convention: match the documented standard in
            // `LpResult.row_dual` ("LMP extraction: lmp[i] =
            // row_dual[balance_row_i] / base_mva"). Surge's LP backends
            // already negate the raw solver dual into the standard
            // Lagrange convention, so the bus balance row dual is
            // already in the form `d(obj)/d(rhs in pu)`. Dividing by
            // base converts to $/MWh. An additional negation here is
            // incorrect — it produced negative LMPs even when committed
            // generators were dispatched at pmax (where LMP must
            // exceed the marginal cost of the upper cost block, which
            // is positive).
            let lmp: Vec<f64> = (0..input.n_bus)
                .map(|i| input.lp_sol.row_dual[balance_base + i] / input.base)
                .collect();
            lmp_out.push(lmp);
        }
    } else {
        tracing::warn!(
            status = ?input.lp_sol.status,
            hours = input.n_hours,
            buses = input.n_bus,
            "SCUC: LP re-solve for LMP extraction did not converge — LMPs will be zero"
        );
        for _ in 0..input.n_hours {
            lmp_out.push(vec![0.0; input.n_bus]);
        }
    }

    let hourly_reserve_results = if pricing_converged {
        extract_hourly_reserve_results(
            input.reserve_layout,
            input.lp_sol,
            Some(&input.lp_sol.row_dual),
            input.hour_reserve_row_bases,
            input.n_hours,
            input.n_gen,
            input.n_storage,
            input.n_dl,
            input.vars_per_hour,
            input.base,
        )
    } else {
        (0..input.n_hours).map(|_| None).collect::<Vec<_>>()
    };

    let branch_shadow_prices = if input.spec.enforce_thermal_limits
        && !input.constrained_branches.is_empty()
        && pricing_converged
    {
        crate::common::extraction::extract_branch_shadow_prices_multi(
            &input.lp_sol.row_dual,
            input.n_branch_flow,
            input.hour_row_bases,
            input.base,
        )
    } else {
        vec![]
    };

    let fg_shadow_prices =
        if input.spec.enforce_flowgates && !input.fg_rows.is_empty() && pricing_converged {
            crate::common::extraction::extract_flowgate_shadow_prices_multi(
                &input.lp_sol.row_dual,
                input.fg_rows,
                input.network.flowgates.len(),
                input.n_branch_flow,
                input.hour_row_bases,
                input.base,
            )
        } else {
            vec![]
        };

    let iface_shadow_prices =
        if input.spec.enforce_flowgates && !input.iface_rows.is_empty() && pricing_converged {
            crate::common::extraction::extract_interface_shadow_prices_multi(
                &input.lp_sol.row_dual,
                input.iface_rows,
                input.network.interfaces.len(),
                input.n_branch_flow,
                input.n_fg_rows,
                input.hour_row_bases,
                input.base,
            )
        } else {
            vec![]
        };

    PricingSummary {
        pricing_converged,
        lmp_out,
        dloss_dp_out: vec![vec![0.0; input.n_bus]; input.n_hours],
        hourly_reserve_results,
        branch_shadow_prices,
        fg_shadow_prices,
        iface_shadow_prices,
    }
}

#[cfg(test)]
mod tests {
    use super::{cap_logic_row_span, fixed_commitment_trajectory_offset_pu};
    use crate::dispatch::{
        CommitmentMode, Horizon, IndexedCommitmentOptions, IndexedDispatchInitialState,
    };
    use crate::legacy::DispatchOptions;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Bus, BusType, CommitmentParams, Generator, Load};

    #[test]
    fn cap_logic_row_span_finds_full_hour_block_before_reserve_rows() {
        let labels = vec![
            "h0:other_0".to_string(),
            "h0:cap_logic_0".to_string(),
            "h0:cap_logic_1".to_string(),
            "h0:cap_logic_2".to_string(),
            "h0:cap_logic_3".to_string(),
            "h0:cap_logic_4".to_string(),
            "h0:reserve_0".to_string(),
            "h1:cap_logic_0".to_string(),
        ];

        let span = cap_logic_row_span(&labels, 6, 0).expect("expected cap-logic rows");
        assert_eq!(span, 1..6);
    }

    #[test]
    fn cap_logic_row_span_ignores_other_hours_and_noncontiguous_rows() {
        let labels = vec![
            "h0:other_0".to_string(),
            "h0:cap_logic_0".to_string(),
            "h0:other_1".to_string(),
            "h0:cap_logic_1".to_string(),
            "h0:cap_logic_2".to_string(),
            "h0:reserve_0".to_string(),
        ];

        let span = cap_logic_row_span(&labels, 5, 0).expect("expected contiguous tail block");
        assert_eq!(span, 3..5);
        assert_eq!(cap_logic_row_span(&labels, 1, 0), Some(1..2));
    }

    #[test]
    fn cap_logic_row_span_expands_forward_from_reserve_row_base() {
        let labels = vec![
            "h0:other_0".to_string(),
            "h0:cap_logic_0".to_string(),
            "h0:cap_logic_1".to_string(),
            "h0:cap_logic_2".to_string(),
            "h0:cap_logic_3".to_string(),
            "h0:cap_logic_4".to_string(),
            "h0:cap_logic_5".to_string(),
            "h0:reserve_0".to_string(),
        ];

        let span = cap_logic_row_span(&labels, 3, 0).expect("expected full cap-logic block");
        assert_eq!(span, 1..7);
    }

    #[test]
    fn fixed_commitment_trajectory_offset_includes_future_startup_power() {
        let mut net = Network::new("pricing_trajectory_offset");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 0.0, 0.0));

        let mut generator = Generator::new(1, 0.0, 1.0);
        generator.pmin = 170.0;
        generator.pmax = 355.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        generator.commitment = Some(CommitmentParams {
            startup_ramp_mw_per_min: Some(88.75 / 60.0),
            shutdown_ramp_mw_per_min: Some(88.75 / 60.0),
            ..Default::default()
        });
        net.generators.push(generator.clone());

        let opts = DispatchOptions {
            n_periods: 2,
            horizon: Horizon::TimeCoupled,
            commitment: CommitmentMode::Optimize(IndexedCommitmentOptions::default()),
            initial_state: IndexedDispatchInitialState::default(),
            ..DispatchOptions::default()
        };
        let spec = crate::common::spec::DispatchProblemSpec::from_options(&opts);
        let startup_active = vec![false, true];
        let shutdown_active = vec![false, false];

        let offset = fixed_commitment_trajectory_offset_pu(
            &generator,
            &spec,
            0,
            2,
            &startup_active,
            &shutdown_active,
            None,
            net.base_mva,
        );
        assert!(
            (offset - 0.8125).abs() < 1e-9,
            "future startup trajectory should contribute 81.25 MW / 0.8125 pu, got {offset:.9}",
        );
    }
}
