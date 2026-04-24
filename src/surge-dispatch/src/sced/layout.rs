// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCED column layout and active-input planning.

use surge_network::Network;
use surge_network::market::DispatchableLoad;
use surge_network::market::{ReserveProduct, SystemReserveRequirement, ZonalReserveRequirement};

use crate::common::layout::DispatchOffsets;
use crate::common::reserves::{ReserveLpCtx, ReserveLpLayout};
use crate::common::runtime::DispatchPeriodContext;
use crate::common::spec::DispatchProblemSpec;

pub(super) struct ScedLayout {
    pub dispatch: DispatchOffsets,
    pub pb_curtailment_bus: usize,
    pub pb_excess_bus: usize,
    pub pb_curtailment_seg: usize,
    pub pb_excess_seg: usize,
    pub branch_lower_slack: usize,
    pub branch_upper_slack: usize,
    pub flowgate_lower_slack: usize,
    pub flowgate_upper_slack: usize,
    pub interface_lower_slack: usize,
    pub interface_upper_slack: usize,
    pub ramp_up_slack: usize,
    pub ramp_down_slack: usize,
    pub angle_diff_lower_slack: usize,
    pub angle_diff_upper_slack: usize,
    pub n_angle_diff_rows: usize,
    pub hvdc_band_offsets_abs: Vec<usize>,
    /// Column index of the SCED-AC Benders eta epigraph variable for this
    /// period, or `None` if eta is disabled. When `Some(col)`, the LP has
    /// one extra column at `col` with `lb = 0`, `ub = +inf`, and cost `+1.0`
    /// — a free epigraph variable that the cut rows in
    /// [`super::rows::build_benders_cut_rows`] bound from below.
    pub benders_eta_col: Option<usize>,
    /// Base column index of explicit contingency variables, or `None` when
    /// explicit contingency mode is inactive for this period. The block
    /// layout is `[case_penalty × n_cases | worst_case | avg_case]`.
    pub explicit_ctg_base: Option<usize>,
}

impl ScedLayout {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        n_bus: usize,
        n_gen: usize,
        n_storage: usize,
        n_sto_dis_epi: usize,
        n_sto_ch_epi: usize,
        n_hvdc_vars: usize,
        hvdc_band_offsets_rel: &[usize],
        n_pwl_gen: usize,
        n_dl: usize,
        n_vbid: usize,
        n_block_vars: usize,
        reserve_var_count: usize,
        n_blk_res_vars: usize,
        n_branch_flow: usize,
        n_fg_rows: usize,
        n_iface_rows: usize,
        n_pb_curt_segs: usize,
        n_pb_excess_segs: usize,
        n_angle_diff_rows: usize,
        benders_eta_active: bool,
        n_explicit_ctg_vars: usize,
    ) -> Self {
        let theta = 0;
        let pg = theta + n_bus;
        let sto_ch = pg + n_gen;
        let sto_dis = sto_ch + n_storage;
        let sto_soc = sto_dis + n_storage;
        let sto_epi_dis = sto_soc + n_storage;
        let sto_epi_ch = sto_epi_dis + n_sto_dis_epi;
        let hvdc = sto_epi_ch + n_sto_ch_epi;
        let e_g = hvdc + n_hvdc_vars;
        let dl = e_g + n_pwl_gen;
        let vbid = dl + n_dl;
        let block = vbid + n_vbid;
        let reserve = block + n_block_vars;
        let block_reserve = reserve + reserve_var_count;
        let pb_curtailment_bus = block_reserve + n_blk_res_vars;
        let pb_excess_bus = pb_curtailment_bus + n_bus;
        let pb_curtailment_seg = pb_excess_bus + n_bus;
        let pb_excess_seg = pb_curtailment_seg + n_pb_curt_segs;
        let branch_lower_slack = pb_excess_seg + n_pb_excess_segs;
        let branch_upper_slack = branch_lower_slack + n_branch_flow;
        let flowgate_lower_slack = branch_upper_slack + n_branch_flow;
        let flowgate_upper_slack = flowgate_lower_slack + n_fg_rows;
        let interface_lower_slack = flowgate_upper_slack + n_fg_rows;
        let interface_upper_slack = interface_lower_slack + n_iface_rows;
        let ramp_up_slack = interface_upper_slack + n_iface_rows;
        let ramp_down_slack = ramp_up_slack + n_gen;
        let angle_diff_lower_slack = ramp_down_slack + n_gen;
        let angle_diff_upper_slack = angle_diff_lower_slack + n_angle_diff_rows;
        // Optional SCED-AC Benders eta epigraph variable; placed at the very
        // end of the column layout so the existing offsets above stay intact
        // for callers that do not opt in to Benders.
        let mut tail = angle_diff_upper_slack + n_angle_diff_rows;
        let benders_eta_col = if benders_eta_active {
            let col = tail;
            tail += 1;
            Some(col)
        } else {
            None
        };
        // Optional explicit contingency variables: case penalty columns
        // followed by one worst-case and one avg-case column per period
        // (SCED is single-period so at most n_cases + 2 extra columns).
        let explicit_ctg_base = if n_explicit_ctg_vars > 0 {
            let base = tail;
            tail += n_explicit_ctg_vars;
            Some(base)
        } else {
            None
        };
        let n_vars = tail;

        Self {
            dispatch: DispatchOffsets {
                theta,
                pg,
                sto_ch,
                sto_dis,
                sto_soc,
                sto_epi_dis,
                sto_epi_ch,
                hvdc,
                e_g,
                dl,
                vbid,
                block,
                reserve,
                block_reserve,
                n_vars,
            },
            pb_curtailment_bus,
            pb_excess_bus,
            pb_curtailment_seg,
            pb_excess_seg,
            branch_lower_slack,
            branch_upper_slack,
            flowgate_lower_slack,
            flowgate_upper_slack,
            interface_lower_slack,
            interface_upper_slack,
            ramp_up_slack,
            ramp_down_slack,
            angle_diff_lower_slack,
            angle_diff_upper_slack,
            n_angle_diff_rows,
            hvdc_band_offsets_abs: hvdc_band_offsets_rel
                .iter()
                .map(|&rel| hvdc + rel)
                .collect(),
            benders_eta_col,
            explicit_ctg_base,
        }
    }

    pub fn pb_curtailment_bus_col(&self, bus_idx: usize) -> usize {
        self.pb_curtailment_bus + bus_idx
    }

    pub fn pb_excess_bus_col(&self, bus_idx: usize) -> usize {
        self.pb_excess_bus + bus_idx
    }

    pub fn pb_curtailment_seg_col(&self, seg_idx: usize) -> usize {
        self.pb_curtailment_seg + seg_idx
    }

    pub fn pb_excess_seg_col(&self, seg_idx: usize) -> usize {
        self.pb_excess_seg + seg_idx
    }

    pub fn branch_lower_slack_col(&self, row_idx: usize) -> usize {
        self.branch_lower_slack + row_idx
    }

    pub fn branch_upper_slack_col(&self, row_idx: usize) -> usize {
        self.branch_upper_slack + row_idx
    }

    pub fn flowgate_lower_slack_col(&self, row_idx: usize) -> usize {
        self.flowgate_lower_slack + row_idx
    }

    pub fn flowgate_upper_slack_col(&self, row_idx: usize) -> usize {
        self.flowgate_upper_slack + row_idx
    }

    pub fn interface_lower_slack_col(&self, row_idx: usize) -> usize {
        self.interface_lower_slack + row_idx
    }

    pub fn interface_upper_slack_col(&self, row_idx: usize) -> usize {
        self.interface_upper_slack + row_idx
    }

    pub fn ramp_up_slack_col(&self, gen_idx: usize) -> usize {
        self.ramp_up_slack + gen_idx
    }

    pub fn ramp_down_slack_col(&self, gen_idx: usize) -> usize {
        self.ramp_down_slack + gen_idx
    }

    pub fn angle_diff_lower_slack_col(&self, row_idx: usize) -> usize {
        self.angle_diff_lower_slack + row_idx
    }

    pub fn angle_diff_upper_slack_col(&self, row_idx: usize) -> usize {
        self.angle_diff_upper_slack + row_idx
    }

    /// Column index of the SCED-AC Benders epigraph variable, when active.
    pub fn benders_eta_col(&self) -> Option<usize> {
        self.benders_eta_col
    }
}

pub(super) struct ScedActiveInputs<'a> {
    pub dl_list: Vec<&'a DispatchableLoad>,
    pub dl_orig_idx: Vec<usize>,
    pub active_vbids: Vec<usize>,
    pub reserve_layout: ReserveLpLayout,
    pub reserve_ctx: ReserveLpCtx<'a>,
}

pub(super) struct ScedLayoutPlan<'a> {
    pub layout: ScedLayout,
    pub active: ScedActiveInputs<'a>,
    pub n_blk_res_vars: usize,
    pub n_pb_curt_segs: usize,
    pub n_pb_excess_segs: usize,
}

pub(super) struct ScedLayoutPlanInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub context: DispatchPeriodContext<'a>,
    pub gen_indices: &'a [usize],
    pub reserve_products: &'a [ReserveProduct],
    pub system_reserve_requirements: &'a [SystemReserveRequirement],
    pub zonal_reserve_requirements: &'a [ZonalReserveRequirement],
    pub hvdc_band_offsets_rel: &'a [usize],
    pub n_bus: usize,
    pub n_gen: usize,
    pub n_storage: usize,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_hvdc_vars: usize,
    pub n_pwl_gen: usize,
    pub n_block_vars: usize,
    pub has_per_block_reserves: bool,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub n_iface_rows: usize,
    pub n_angle_diff_rows: usize,
    /// Total number of explicit contingency variables to allocate
    /// at the tail of the column vector. Zero when inactive.
    pub n_explicit_ctg_vars: usize,
}

#[allow(clippy::too_many_lines)]
pub(super) fn build_layout_plan<'a>(input: ScedLayoutPlanInput<'a>) -> ScedLayoutPlan<'a> {
    let reserve_var_base = input.n_bus
        + input.n_gen
        + 3 * input.n_storage
        + input.n_sto_dis_epi
        + input.n_sto_ch_epi
        + input.n_hvdc_vars
        + input.n_pwl_gen
        + input
            .spec
            .dispatchable_loads
            .iter()
            .filter(|dl| dl.in_service)
            .count()
        + input
            .spec
            .virtual_bids
            .iter()
            .filter(|vb| vb.in_service && vb.period == input.context.period)
            .count()
        + input.n_block_vars;
    let active = select_active_inputs(
        input.network,
        input.spec,
        input.context,
        input.gen_indices,
        input.reserve_products,
        input.system_reserve_requirements,
        input.zonal_reserve_requirements,
        reserve_var_base,
    );
    let n_active_products = active.reserve_layout.products.len();
    let n_blk_res_vars = if input.has_per_block_reserves {
        input.n_block_vars * n_active_products
    } else {
        0
    };
    let n_pb_curt_segs = input.spec.power_balance_penalty.curtailment.len();
    let n_pb_excess_segs = input.spec.power_balance_penalty.excess.len();
    let benders_eta_active = input
        .spec
        .sced_ac_benders
        .period_eta_active(input.context.period);
    let layout = ScedLayout::build(
        input.n_bus,
        input.n_gen,
        input.n_storage,
        input.n_sto_dis_epi,
        input.n_sto_ch_epi,
        input.n_hvdc_vars,
        input.hvdc_band_offsets_rel,
        input.n_pwl_gen,
        active.dl_list.len(),
        active.active_vbids.len(),
        input.n_block_vars,
        active.reserve_layout.n_reserve_vars,
        n_blk_res_vars,
        input.n_branch_flow,
        input.n_fg_rows,
        input.n_iface_rows,
        n_pb_curt_segs,
        n_pb_excess_segs,
        input.n_angle_diff_rows,
        benders_eta_active,
        input.n_explicit_ctg_vars,
    );

    ScedLayoutPlan {
        layout,
        active,
        n_blk_res_vars,
        n_pb_curt_segs,
        n_pb_excess_segs,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn select_active_inputs<'a>(
    network: &'a Network,
    spec: &'a DispatchProblemSpec<'a>,
    context: DispatchPeriodContext<'a>,
    gen_indices: &'a [usize],
    reserve_products: &'a [ReserveProduct],
    system_reserve_requirements: &'a [SystemReserveRequirement],
    zonal_reserve_requirements: &'a [ZonalReserveRequirement],
    reserve_var_base: usize,
) -> ScedActiveInputs<'a> {
    let dl_with_idx: Vec<(usize, &DispatchableLoad)> = spec
        .dispatchable_loads
        .iter()
        .enumerate()
        .filter(|(_, dl)| dl.in_service)
        .collect();
    let dl_list: Vec<&DispatchableLoad> = dl_with_idx.iter().map(|(_, dl)| *dl).collect();
    let dl_orig_idx: Vec<usize> = dl_with_idx.iter().map(|(i, _)| *i).collect();
    let generator_bus_numbers: Vec<u32> = gen_indices
        .iter()
        .map(|&gi| network.generators[gi].bus)
        .collect();
    let active_vbids: Vec<usize> = spec
        .virtual_bids
        .iter()
        .enumerate()
        .filter(|(_, vb)| vb.in_service && vb.period == context.period)
        .map(|(i, _)| i)
        .collect();
    // Sparse participation per product — same semantics as the SCUC
    // layout. DL-side uses the CONSUMER GROUP (Phase 3) granularity.
    // Participation is computed across the full horizon so the
    // column layout stays consistent across SCED periods.
    let gen_participation_by_product = crate::common::reserves::compute_gen_participation(
        reserve_products,
        spec,
        network,
        gen_indices,
        spec.n_periods,
    );
    let dl_consumer_groups = crate::common::reserves::compute_dl_consumer_groups(&dl_list);
    let dl_group_participation_by_product = crate::common::reserves::compute_dl_group_participation(
        reserve_products,
        spec,
        &dl_list,
        &dl_orig_idx,
        &dl_consumer_groups,
        spec.n_periods,
    );
    let reserve_layout = crate::common::reserves::build_layout_for_period(
        reserve_products,
        system_reserve_requirements,
        zonal_reserve_requirements,
        spec.ramp_sharing,
        spec.generator_area,
        &generator_bus_numbers,
        gen_indices.len(),
        0,
        dl_list.len(),
        reserve_var_base,
        context.has_prev_dispatch(),
        context.period,
        &gen_participation_by_product,
        &dl_consumer_groups,
        &dl_group_participation_by_product,
    );
    let reserve_ctx = ReserveLpCtx::from_problem(
        network,
        gen_indices,
        spec,
        context.period,
        context.prev_dispatch_mw,
        context.prev_dispatch_mask,
    );

    ScedActiveInputs {
        dl_list,
        dl_orig_idx,
        active_vbids,
        reserve_layout,
        reserve_ctx,
    }
}
