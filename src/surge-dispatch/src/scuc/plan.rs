// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC pre-solve model planning.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::market::{CostCurve, DispatchableLoad};
use surge_opf::advanced::IslandRefs;
use tracing::warn;

use super::bounds::ScucBoundsInput;
use super::cuts::ScucCommitmentCut;
use super::layout::{
    ScucDrActivationInfo, ScucLayout, ScucLayoutPlan, ScucLayoutPlanInput, build_layout_plan,
};
use super::metadata::{
    ScucBoundMetadataInput, ScucRowMetadata, ScucRowMetadataInput, build_bound_metadata,
    build_objective_cc_plants, build_row_metadata,
};
use super::objective::{ScucObjectiveInput, build_objective};
use super::rows::{ScucDrActivationLoad, ScucDrReboundLoad, ScucStartupTierInfo};
use crate::common::costs::{
    resolve_generator_economics_for_period, resolve_pwl_gen_segments_for_period,
};
use crate::common::dc::{DcModelContext, DcSolveSession};
use crate::common::network::DcNetworkPlan;
use crate::common::spec::DispatchProblemSpec;
use crate::dispatch::CommitmentMode;
use crate::error::ScedError;

type PwlSegments = Vec<(f64, f64)>;
type HourlyPwlSegments = Vec<Vec<Option<PwlSegments>>>;

pub(super) struct ScucCommitmentPolicyPlan<'a> {
    pub is_must_run_ext: Vec<bool>,
    pub da_commitment: Option<&'a [Vec<bool>]>,
}

pub(super) struct ScucModelPlan<'a> {
    pub hourly_networks: Vec<Network>,
    pub pwl: ScucPwlPlan,
    pub startup: ScucStartupPlan,
    pub layout: ScucLayoutPlan<'a>,
    pub variable: ScucVariablePlan<'a>,
    pub commitment_policy: ScucCommitmentPolicyPlan<'a>,
    pub network_plan: DcNetworkPlan,
    pub use_plc: bool,
    pub n_bp: usize,
    pub n_sbp: usize,
}

pub(super) struct ScucModelPlanInput<'a> {
    pub network: &'a Network,
    pub solve: &'a DcSolveSession<'a>,
    /// Optional pre-built per-hour network snapshots. When `Some`,
    /// `build_model_plan` takes ownership of the supplied Vec instead
    /// of cloning the base network `n_hours` times. Callers that
    /// already have hourly networks in scope (e.g. the explicit-
    /// security SCUC, which builds them for PTDF/LODF) can pass them
    /// through to avoid duplicate build + duplicate drop.
    pub hourly_networks: Option<Vec<Network>>,
}

pub(super) struct ScucPwlPlan {
    pub gen_j: Vec<usize>,
    pub segments_by_hour: HourlyPwlSegments,
    pub n_rows_total: usize,
}

pub(super) struct ScucStartupPlan {
    pub gen_tier_info_by_hour: Vec<Vec<Vec<ScucStartupTierInfo>>>,
    pub startup_tier_capacity: Vec<usize>,
    pub delta_gen_off: Vec<usize>,
    pub n_delta_per_hour: usize,
    pub pre_horizon_offline_hours: Vec<Option<f64>>,
}

pub(super) struct ScucCcPlantInfo {
    pub n_configs: usize,
    pub z_block_off: usize,
    pub member_gen_j: HashSet<usize>,
    pub allowed_transitions: HashMap<(usize, usize), (f64, f64)>,
    pub pgcc_entries: Vec<(usize, usize)>,
    pub pgcc_block_off: usize,
    pub transition_pairs: Vec<(usize, usize)>,
    pub ytrans_block_off: usize,
}

pub(super) struct ScucVariablePlan<'a> {
    pub commitment_cuts: Vec<ScucCommitmentCut<'a>>,
    pub n_penalty_slacks: usize,
    pub penalty_slack_base: usize,
    pub cc_infos: Vec<ScucCcPlantInfo>,
    pub cc_var_base: usize,
    pub cc_block_size: usize,
    pub dl_act_var_base: usize,
    pub dl_rebound_infos: Vec<usize>,
    pub n_dl_rebound: usize,
    pub dl_rebound_var_base: usize,
    /// Base column index for the multi-interval energy window slack
    /// columns. Energy windows are enforced softly: each (window,
    /// direction) pair gets one non-negative slack column priced from
    /// `spec.energy_window_violation_per_puh`. The mapping
    /// `(limit_idx, dir)` → column index is encoded order-stably in
    /// `energy_window_slack_kinds`.
    pub energy_window_slack_base: usize,
    pub n_energy_window_slacks: usize,
    pub energy_window_slack_kinds: Vec<EnergyWindowSlackKind>,
    pub explicit_contingency: Option<ExplicitContingencyObjectivePlan>,
    /// Base column index for the post-hourly **contingency-cut** slack
    /// columns (Option C path). Two parallel blocks of width `n_cut_rows`:
    /// `cut_slack_lower_base..cut_slack_lower_base + n_cut_rows` for the
    /// lower-direction slack `σ⁻_k` on cut `k`, and
    /// `cut_slack_upper_base..cut_slack_upper_base + n_cut_rows` for the
    /// upper-direction slack `σ⁺_k`. Populated from
    /// `spec.contingency_cuts`. Zero when the legacy Flowgate path is in
    /// use (SCED, non-security paths, or the iterative-refinement
    /// security loop that stays on Flowgates).
    pub cut_slack_lower_base: usize,
    pub cut_slack_upper_base: usize,
    pub n_cut_rows: usize,
    pub n_var: usize,
    pub dr_activation_loads: Vec<ScucDrActivationLoad>,
    pub dr_rebound_loads: Vec<ScucDrReboundLoad>,
}

pub(super) use crate::common::contingency::{
    ExplicitContingencyCasePlan, ExplicitContingencyObjectivePlan, ExplicitContingencyPeriodPlan,
};

/// Identifies which row a given energy window slack column belongs to.
/// `limit_idx` is the index into `spec.energy_window_limits`; `direction`
/// distinguishes the min-energy and max-energy rows when both are present
/// on the same window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EnergyWindowSlackKind {
    pub limit_idx: usize,
    pub direction: EnergyWindowSlackDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnergyWindowSlackDirection {
    /// Slack on `Σ dt × pg + e^+ ≥ emin` (eq 76).
    Min,
    /// Slack on `Σ dt × pg − e^+ ≤ emax` (eq 75).
    Max,
}

pub(super) struct ScucNetworkRowsPlan {
    pub fg_limits: Vec<f64>,
    pub fg_shift_offsets: Vec<f64>,
}

pub(super) struct ScucColumnBuildInput<'spec, 'input> {
    pub network: &'input Network,
    pub hourly_networks: &'input [Network],
    pub spec: &'spec DispatchProblemSpec<'spec>,
    pub island_refs: &'input IslandRefs,
    pub layout: &'input ScucLayout,
    pub n_var: usize,
    pub n_hours: usize,
    pub n_bus: usize,
    pub n_gen: usize,
    pub n_bp: usize,
    pub n_sbp: usize,
    pub n_branch_flow: usize,
    /// Map from LP thermal-row index to `network.branches` index.
    /// Carried so the bounds layer can cap `col_upper` on per-branch
    /// thermal slack columns at a finite multiple of branch rating
    /// (prevents LP-relaxation hallucination — see
    /// [`crate::scuc::bounds::ScucBoundsInput::constrained_branches`]).
    pub constrained_branches: &'input [usize],
    pub n_fg_rows: usize,
    /// Map from LP flowgate row index to `network.flowgates` index.
    /// Carried so the bounds layer can look up per-row breach_sides +
    /// active_period and pin inactive slack columns to zero.
    pub fg_rows: &'input [usize],
    pub n_iface_rows: usize,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_block_vars_per_hour: usize,
    pub is_block_mode: bool,
    pub use_plc: bool,
    pub has_reg_products: bool,
    pub has_per_block_reserves: bool,
    pub gen_indices: &'input [usize],
    pub gen_blocks: &'input [Vec<crate::common::blocks::DispatchBlock>],
    pub gen_block_start: &'input [usize],
    pub gen_tier_info_by_hour: &'input [Vec<Vec<ScucStartupTierInfo>>],
    pub startup_tier_capacity: &'input [usize],
    pub delta_gen_off: &'input [usize],
    pub reserve_layout: &'input crate::common::reserves::ReserveLpLayout,
    pub system_reserve_requirements: &'input [surge_network::market::SystemReserveRequirement],
    pub storage_gen_local: &'input [(usize, usize, usize)],
    pub hvdc_band_offsets: &'input [usize],
    pub pwl_gen_j: &'input [usize],
    pub pwl_gen_segments_by_hour: &'input [Vec<Option<PwlSegments>>],
    pub dl_list: &'input [&'spec DispatchableLoad],
    pub dl_orig_idx: &'input [usize],
    pub active_vbids: &'input [usize],
    pub effective_co2_price: f64,
    pub effective_co2_rate: &'input [f64],
    pub cc_member_gen_set: &'input HashSet<usize>,
    pub foz_gens: &'input [super::layout::ScucFozGenInfo],
    pub ph_mode_infos: &'input [super::layout::ScucPhModeInfo],
    pub cc_infos: &'input [ScucCcPlantInfo],
    pub dl_activation_infos: &'input [ScucDrActivationInfo],
    pub cc_var_base: usize,
    pub dl_act_var_base: usize,
    pub dl_rebound_var_base: usize,
    pub n_dl_rebound: usize,
    pub energy_window_slack_base: usize,
    pub energy_window_slack_kinds: &'input [EnergyWindowSlackKind],
    pub explicit_contingency: Option<&'input ExplicitContingencyObjectivePlan>,
    /// Post-hourly contingency-cut slack column bases + count. All
    /// zero when the legacy Flowgate path is in use.
    pub cut_slack_lower_base: usize,
    pub cut_slack_upper_base: usize,
    pub n_cut_rows: usize,
    pub base: f64,
}

pub(super) struct ScucColumnBuildState {
    pub col_cost: Vec<f64>,
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
    pub integrality: Vec<surge_opf::backends::VariableDomain>,
}

pub(super) struct ScucRowBuildInput<'spec, 'input> {
    pub network: &'input Network,
    pub spec: &'spec DispatchProblemSpec<'spec>,
    pub gen_indices: &'input [usize],
    pub gen_tier_info_by_hour: &'input [Vec<Vec<ScucStartupTierInfo>>],
    pub delta_gen_off: &'input [usize],
    pub pre_horizon_offline_hours: &'input [Option<f64>],
    pub prev_dispatch_mw: Option<&'spec [f64]>,
    pub prev_dispatch_mask: Option<&'spec [bool]>,
    pub foz_gens: &'input [super::layout::ScucFozGenInfo],
    pub ph_mode_infos: &'input [super::layout::ScucPhModeInfo],
    pub ph_head_infos: &'input [super::layout::ScucPhHeadInfo],
    pub cc_infos: &'input [ScucCcPlantInfo],
    pub cc_var_base: usize,
    pub hvdc_off: usize,
    pub hvdc_band_offsets: &'input [usize],
    pub n_hours: usize,
    pub has_hvdc: bool,
    pub base: f64,
    pub step_h: f64,
}

pub(super) struct ScucRowBuildState<'a> {
    pub cc_member_gen_set: HashSet<usize>,
    pub row_metadata: ScucRowMetadata<'a>,
}

pub(super) struct ScucProblemPlanInput<'spec, 'input> {
    pub network: &'input Network,
    pub solve: &'input DcSolveSession<'spec>,
    pub model_plan: &'input ScucModelPlan<'spec>,
}

pub(super) struct ScucProblemPlan<'a> {
    pub model_plan: &'a ScucModelPlan<'a>,
    pub row_state: ScucRowBuildState<'a>,
    pub columns: ScucColumnBuildState,
    pub network_rows: ScucNetworkRowsPlan,
}

pub(super) fn build_row_state<'a>(input: ScucRowBuildInput<'a, 'a>) -> ScucRowBuildState<'a> {
    let gen_j_lookup: HashMap<usize, usize> = input
        .gen_indices
        .iter()
        .enumerate()
        .map(|(gen_idx, &network_gen_idx)| (network_gen_idx, gen_idx))
        .collect();
    let cc_member_gen_set: HashSet<usize> = {
        let mut set = HashSet::new();
        for plant in &input.network.market_data.combined_cycle_plants {
            for config in &plant.configs {
                for &network_gen_idx in &config.gen_indices {
                    if let Some(&gen_idx) = gen_j_lookup.get(&network_gen_idx) {
                        set.insert(gen_idx);
                    }
                }
            }
        }
        set
    };

    let row_metadata = build_row_metadata(ScucRowMetadataInput {
        network: input.network,
        spec: input.spec,
        gen_indices: input.gen_indices,
        delta_gen_off: input.delta_gen_off,
        gen_tier_info_by_hour: input.gen_tier_info_by_hour,
        pre_horizon_offline_hours: input.pre_horizon_offline_hours,
        prev_dispatch_mw: input.prev_dispatch_mw,
        prev_dispatch_mask: input.prev_dispatch_mask,
        cc_member_gen_set: &cc_member_gen_set,
        foz_gens: input.foz_gens,
        ph_mode_infos: input.ph_mode_infos,
        ph_head_infos: input.ph_head_infos,
        cc_infos: input.cc_infos,
        cc_var_base: input.cc_var_base,
        hvdc_off: input.hvdc_off,
        hvdc_band_offsets: input.hvdc_band_offsets,
        n_hours: input.n_hours,
        has_hvdc: input.has_hvdc,
        base: input.base,
        step_h: input.step_h,
    });

    ScucRowBuildState {
        cc_member_gen_set,
        row_metadata,
    }
}

/// Build the horizon-level SCUC plan from immutable inputs and derived setup.
///
/// This stage owns the exact startup-tier, PWL, variable-layout, commitment
/// policy, and network-row planning that feed the sparse problem build.
pub(super) fn build_problem_plan<'a>(input: ScucProblemPlanInput<'a, 'a>) -> ScucProblemPlan<'a> {
    use std::time::Instant;
    let spec = &input.solve.spec;
    let setup = &input.solve.setup;
    let island_refs = &input.solve.island_refs;
    let base = input.solve.base_mva;
    let layout_plan = &input.model_plan.layout;
    let layout = &layout_plan.layout;
    let active_inputs = &layout_plan.active;
    let startup = &input.model_plan.startup;
    let pwl = &input.model_plan.pwl;
    let variable = &input.model_plan.variable;
    let network_plan = &input.model_plan.network_plan;
    let n_hours = spec.n_periods;
    let n_gen = setup.n_gen;
    let n_sto_dis_epi = setup.n_sto_dis_epi;
    let n_sto_ch_epi = setup.n_sto_ch_epi;
    let n_block_vars_per_hour = setup.n_block_vars;
    let is_block_mode = setup.is_block_mode;
    let has_per_block_reserves = setup.has_per_block_reserves;
    let has_hvdc = setup.n_hvdc_links > 0;
    let step_h = spec.dt_hours;

    let _t0 = Instant::now();
    let row_state = build_row_state(ScucRowBuildInput {
        network: input.network,
        spec,
        gen_indices: &setup.gen_indices,
        gen_tier_info_by_hour: &startup.gen_tier_info_by_hour,
        delta_gen_off: &startup.delta_gen_off,
        pre_horizon_offline_hours: &startup.pre_horizon_offline_hours,
        prev_dispatch_mw: spec.prev_dispatch_mw(),
        prev_dispatch_mask: spec.initial_state.prev_dispatch_mask.as_deref(),
        foz_gens: &layout_plan.foz_gens,
        ph_mode_infos: &layout_plan.ph_mode_infos,
        ph_head_infos: &layout_plan.ph_head_infos,
        cc_infos: &variable.cc_infos,
        cc_var_base: variable.cc_var_base,
        hvdc_off: layout.dispatch.hvdc,
        hvdc_band_offsets: &setup.hvdc_band_offsets_rel,
        n_hours,
        has_hvdc,
        base,
        step_h,
    });
    tracing::info!(
        stage = "build_problem_plan.build_row_state",
        secs = _t0.elapsed().as_secs_f64(),
        "SCUC plan build timing"
    );
    let _t0 = Instant::now();
    let mut columns = build_column_state(ScucColumnBuildInput {
        network: input.network,
        hourly_networks: &input.model_plan.hourly_networks,
        spec,
        island_refs,
        layout,
        n_var: variable.n_var,
        n_hours,
        n_bus: input.network.n_buses(),
        n_gen,
        n_bp: input.model_plan.n_bp,
        n_sbp: input.model_plan.n_sbp,
        n_branch_flow: input.model_plan.network_plan.constrained_branches.len(),
        constrained_branches: &input.model_plan.network_plan.constrained_branches,
        n_fg_rows: input.model_plan.network_plan.fg_rows.len(),
        fg_rows: &input.model_plan.network_plan.fg_rows,
        n_iface_rows: input.model_plan.network_plan.iface_rows.len(),
        n_sto_dis_epi,
        n_sto_ch_epi,
        n_block_vars_per_hour,
        is_block_mode,
        use_plc: input.model_plan.use_plc,
        has_reg_products: active_inputs.has_reg_products,
        has_per_block_reserves,
        gen_indices: &setup.gen_indices,
        gen_blocks: &setup.gen_blocks,
        gen_block_start: &setup.gen_block_start,
        gen_tier_info_by_hour: &startup.gen_tier_info_by_hour,
        startup_tier_capacity: &startup.startup_tier_capacity,
        delta_gen_off: &startup.delta_gen_off,
        reserve_layout: &active_inputs.reserve_layout,
        system_reserve_requirements: &setup.r_sys_reqs,
        storage_gen_local: &setup.storage_gen_local,
        hvdc_band_offsets: &setup.hvdc_band_offsets_rel,
        pwl_gen_j: &pwl.gen_j,
        pwl_gen_segments_by_hour: &pwl.segments_by_hour,
        dl_list: &active_inputs.dl_list,
        dl_orig_idx: &active_inputs.dl_orig_idx,
        active_vbids: &active_inputs.active_vbids,
        effective_co2_price: setup.effective_co2_price,
        effective_co2_rate: &setup.effective_co2_rate,
        cc_member_gen_set: &row_state.cc_member_gen_set,
        foz_gens: &layout_plan.foz_gens,
        ph_mode_infos: &layout_plan.ph_mode_infos,
        cc_infos: &variable.cc_infos,
        dl_activation_infos: &layout_plan.dl_activation_infos,
        cc_var_base: variable.cc_var_base,
        dl_act_var_base: variable.dl_act_var_base,
        dl_rebound_var_base: variable.dl_rebound_var_base,
        n_dl_rebound: variable.n_dl_rebound,
        energy_window_slack_base: variable.energy_window_slack_base,
        energy_window_slack_kinds: &variable.energy_window_slack_kinds,
        explicit_contingency: variable.explicit_contingency.as_ref(),
        cut_slack_lower_base: variable.cut_slack_lower_base,
        cut_slack_upper_base: variable.cut_slack_upper_base,
        n_cut_rows: variable.n_cut_rows,
        base,
    });
    tracing::info!(
        stage = "build_problem_plan.build_column_state",
        secs = _t0.elapsed().as_secs_f64(),
        "SCUC plan build timing"
    );
    let _t0 = Instant::now();
    super::cuts::apply_penalty_slack_columns(
        &variable.commitment_cuts,
        variable.penalty_slack_base,
        &mut columns.col_cost,
        &mut columns.col_lower,
        &mut columns.col_upper,
    );
    tracing::info!(
        stage = "build_problem_plan.apply_penalty_slack_columns",
        secs = _t0.elapsed().as_secs_f64(),
        "SCUC plan build timing"
    );

    let _t0 = Instant::now();
    let (fg_limits, fg_shift_offsets) = crate::common::builders::init_flowgate_nomogram_data(
        input.network,
        &network_plan.fg_rows,
        &setup.resolved_flowgates,
    );
    tracing::info!(
        stage = "build_problem_plan.init_flowgate_nomogram_data",
        secs = _t0.elapsed().as_secs_f64(),
        n_fg_rows = network_plan.fg_rows.len(),
        "SCUC plan build timing"
    );

    ScucProblemPlan {
        model_plan: input.model_plan,
        row_state,
        columns,
        network_rows: ScucNetworkRowsPlan {
            fg_limits,
            fg_shift_offsets,
        },
    }
}

pub(super) fn build_column_state(input: ScucColumnBuildInput<'_, '_>) -> ScucColumnBuildState {
    use std::time::Instant;
    let _t0 = Instant::now();
    let cc_objective_plants = build_objective_cc_plants(input.cc_infos);
    let mut col_cost = build_objective(ScucObjectiveInput {
        network: input.network,
        hourly_networks: input.hourly_networks,
        spec: input.spec,
        layout: input.layout,
        n_var: input.n_var,
        n_hours: input.n_hours,
        n_gen: input.n_gen,
        n_bp: input.n_bp,
        n_branch_flow: input.n_branch_flow,
        n_fg_rows: input.n_fg_rows,
        n_iface_rows: input.n_iface_rows,
        is_block_mode: input.is_block_mode,
        use_plc: input.use_plc,
        gen_indices: input.gen_indices,
        gen_blocks: input.gen_blocks,
        gen_block_start: input.gen_block_start,
        gen_tier_info_by_hour: input.gen_tier_info_by_hour,
        startup_tier_capacity: input.startup_tier_capacity,
        delta_gen_off: input.delta_gen_off,
        reserve_layout: input.reserve_layout,
        storage_gen_local: input.storage_gen_local,
        n_sto_dis_epi: input.n_sto_dis_epi,
        n_sto_ch_epi: input.n_sto_ch_epi,
        hvdc_band_offsets: input.hvdc_band_offsets,
        pwl_gen_j: input.pwl_gen_j,
        pwl_gen_segments_by_hour: input.pwl_gen_segments_by_hour,
        dl_list: input.dl_list,
        dl_orig_idx: input.dl_orig_idx,
        active_vbids: input.active_vbids,
        effective_co2_price: input.effective_co2_price,
        effective_co2_rate: input.effective_co2_rate,
        cc_var_base: input.cc_var_base,
        cc_plants: &cc_objective_plants,
        explicit_contingency: input.explicit_contingency,
        base: input.base,
    });
    tracing::info!(
        stage = "build_column_state.build_objective",
        secs = _t0.elapsed().as_secs_f64(),
        "SCUC plan build timing"
    );
    let _t0 = Instant::now();

    let bound_metadata = build_bound_metadata(ScucBoundMetadataInput {
        network: input.network,
        spec: input.spec,
        n_gen: input.n_gen,
        cc_member_gen_set: input.cc_member_gen_set,
        foz_gens: input.foz_gens,
        ph_mode_infos: input.ph_mode_infos,
        cc_infos: input.cc_infos,
        dl_activation_infos: input.dl_activation_infos,
    });
    let bounds = super::bounds::build_variable_bounds(ScucBoundsInput {
        network: input.network,
        hourly_networks: input.hourly_networks,
        spec: input.spec,
        layout: input.layout,
        island_refs: input.island_refs,
        n_var: input.n_var,
        n_hours: input.n_hours,
        n_bus: input.n_bus,
        n_bp: input.n_bp,
        n_sbp: input.n_sbp,
        n_branch_flow: input.n_branch_flow,
        constrained_branches: input.constrained_branches,
        n_fg_rows: input.n_fg_rows,
        fg_rows: input.fg_rows,
        n_iface_rows: input.n_iface_rows,
        n_sto_dis_epi: input.n_sto_dis_epi,
        n_sto_ch_epi: input.n_sto_ch_epi,
        n_block_vars_per_hour: input.n_block_vars_per_hour,
        is_block_mode: input.is_block_mode,
        use_plc: input.use_plc,
        has_reg_products: input.has_reg_products,
        has_per_block_reserves: input.has_per_block_reserves,
        gen_indices: input.gen_indices,
        gen_blocks: input.gen_blocks,
        gen_block_start: input.gen_block_start,
        gen_tier_info_by_hour: input.gen_tier_info_by_hour,
        startup_tier_capacity: input.startup_tier_capacity,
        delta_gen_off: input.delta_gen_off,
        reserve_layout: input.reserve_layout,
        r_sys_reqs: input.system_reserve_requirements,
        r_zonal_reqs: input.spec.zonal_reserve_requirements,
        storage_gen_local: input.storage_gen_local,
        hvdc_band_offsets: input.hvdc_band_offsets,
        pwl_gen_segments_by_hour: input.pwl_gen_segments_by_hour,
        dl_list: input.dl_list,
        dl_orig_idx: input.dl_orig_idx,
        active_vbids: input.active_vbids,
        foz_groups: &bound_metadata.foz_bound_groups,
        ph_mode_infos: &bound_metadata.ph_mode_bound_infos,
        cc_var_base: input.cc_var_base,
        cc_plants: &bound_metadata.cc_bound_plants,
        cc_member_gen: &bound_metadata.cc_member_gen_mask,
        dl_act_var_base: input.dl_act_var_base,
        dl_activation_infos: &bound_metadata.dl_activation_bound_infos,
        dl_rebound_var_base: input.dl_rebound_var_base,
        n_dl_rebound: input.n_dl_rebound,
        energy_window_slack_base: input.energy_window_slack_base,
        energy_window_slack_kinds: input.energy_window_slack_kinds,
        explicit_contingency: input.explicit_contingency,
        cut_slack_lower_base: input.cut_slack_lower_base,
        cut_slack_upper_base: input.cut_slack_upper_base,
        n_cut_rows: input.n_cut_rows,
        base: input.base,
        col_cost: &mut col_cost,
    });
    tracing::info!(
        stage = "build_column_state.build_bounds",
        secs = _t0.elapsed().as_secs_f64(),
        n_col = input.n_var,
        "SCUC plan build timing"
    );

    ScucColumnBuildState {
        col_cost,
        col_lower: bounds.col_lower,
        col_upper: bounds.col_upper,
        integrality: bounds.integrality,
    }
}

pub(super) struct ScucVariablePlanInput<'spec, 'input> {
    pub network: &'input Network,
    pub spec: &'spec DispatchProblemSpec<'spec>,
    pub layout: &'input ScucLayout,
    pub n_hours: usize,
    pub n_gen: usize,
    pub fg_rows: &'input [usize],
    pub gen_indices: &'input [usize],
    pub dl_list: &'input [&'spec DispatchableLoad],
    pub dl_orig_idx: &'input [usize],
    pub dl_activation_infos: &'input [ScucDrActivationInfo],
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_model_plan<'a>(
    input: ScucModelPlanInput<'a>,
) -> Result<ScucModelPlan<'a>, ScedError> {
    let ScucModelPlanInput {
        network,
        solve,
        hourly_networks: prebuilt_hourly_networks,
    } = input;
    let spec = &solve.spec;
    let setup = &solve.setup;
    let bus_map = &solve.bus_map;
    let base = solve.base_mva;
    let reserve_products = &setup.r_products;
    let system_reserve_requirements = &setup.r_sys_reqs;
    let zonal_reserve_requirements = &setup.r_zonal_reqs;
    let gen_indices = &setup.gen_indices;
    let storage_gen_local = &setup.storage_gen_local;
    let n_gen = setup.n_gen;
    let n_storage = setup.n_storage;
    let n_sto_dis_epi = setup.n_sto_dis_epi;
    let n_sto_ch_epi = setup.n_sto_ch_epi;
    let n_hvdc_vars = setup.n_hvdc_vars;
    let n_block_vars_per_hour = setup.n_block_vars;
    let is_block_mode = setup.is_block_mode;
    let has_per_block_reserves = setup.has_per_block_reserves;

    let n_hours = spec.n_periods;
    let hourly_networks: Vec<Network> = match prebuilt_hourly_networks {
        Some(v) if v.len() == n_hours => v,
        // Either the caller didn't pass them or the length doesn't
        // match (defensive — a mismatch means we'd silently use the
        // wrong snapshots). Fall back to building.
        _ => (0..n_hours)
            .map(|hour| super::snapshot::network_at_hour_with_spec(network, spec, hour))
            .collect(),
    };
    let pwl = build_pwl_plan(&hourly_networks, spec, gen_indices, base);

    let use_plc = if is_block_mode {
        false
    } else {
        spec.use_pwl_generator_costs()
    };

    if !use_plc {
        let n_quad = gen_indices
            .iter()
            .filter(|&&network_gen_idx| {
                let generator = &network.generators[network_gen_idx];
                matches!(
                    generator.cost.as_ref(),
                    Some(CostCurve::Polynomial { coeffs, .. })
                        if coeffs.len() >= 3 && coeffs[0].abs() > 1e-10
                )
            })
            .count();
        if n_quad > 0 {
            warn!(
                n_quad,
                "SCUC: use_plc=false but {} generators have quadratic cost terms that will be \
                 ignored. Enable request.market.generator_cost_modeling.use_pwl_costs (or legacy \
                 n_cost_segments > 0) for accurate cost modeling.",
                n_quad
            );
        }
    }

    let commitment_policy = build_commitment_policy_plan(spec, gen_indices);
    let startup = build_startup_plan(network, spec, gen_indices, n_hours);
    let n_bp = if use_plc {
        spec.generator_pwl_cost_breakpoints().unwrap_or(0)
    } else {
        0
    };
    let n_sbp = if use_plc && n_bp > 2 { n_bp - 2 } else { 0 };
    let network_plan = DcModelContext::build_network_plan(network, spec, bus_map, None);

    let layout = build_layout_plan(ScucLayoutPlanInput {
        network,
        spec,
        has_prev_dispatch: spec.has_prev_dispatch(),
        reserve_products,
        system_reserve_requirements,
        zonal_reserve_requirements,
        gen_indices,
        storage_gen_local,
        n_bus: network.n_buses(),
        n_gen,
        n_delta_per_hour: startup.n_delta_per_hour,
        use_plc,
        n_bp,
        n_storage,
        n_sto_dis_epi,
        n_sto_ch_epi,
        n_hvdc_vars,
        n_pwl_gen: pwl.gen_j.len(),
        n_block_vars_per_hour,
        is_block_mode,
        has_per_block_reserves,
        n_branch_flow: network_plan.constrained_branches.len(),
        n_fg_rows: network_plan.fg_rows.len(),
        n_iface_rows: network_plan.iface_rows.len(),
        n_angle_diff_rows: network_plan.angle_constrained_branches.len(),
    });
    let variable = build_variable_plan(ScucVariablePlanInput {
        network,
        spec,
        layout: &layout.layout,
        n_hours,
        n_gen,
        fg_rows: &network_plan.fg_rows,
        gen_indices,
        dl_list: &layout.active.dl_list,
        dl_orig_idx: &layout.active.dl_orig_idx,
        dl_activation_infos: &layout.dl_activation_infos,
    })?;
    Ok(ScucModelPlan {
        hourly_networks,
        pwl,
        startup,
        layout,
        variable,
        commitment_policy,
        network_plan,
        use_plc,
        n_bp,
        n_sbp,
    })
}

pub(super) fn build_commitment_policy_plan<'a>(
    spec: &'a DispatchProblemSpec<'a>,
    gen_indices: &[usize],
) -> ScucCommitmentPolicyPlan<'a> {
    let is_must_run_ext = gen_indices
        .iter()
        .enumerate()
        .map(|(local_gen_idx, _)| {
            spec.must_run_units
                .as_ref()
                .is_some_and(|must_run| must_run.contains(local_gen_idx))
        })
        .collect();
    let da_commitment = match spec.commitment {
        CommitmentMode::Additional { da_commitment, .. } => Some(da_commitment.as_slice()),
        _ => None,
    };
    ScucCommitmentPolicyPlan {
        is_must_run_ext,
        da_commitment,
    }
}

pub(super) fn build_pwl_plan(
    hourly_networks: &[Network],
    spec: &DispatchProblemSpec<'_>,
    gen_indices: &[usize],
    base: f64,
) -> ScucPwlPlan {
    let mut pwl_gen_j = Vec::new();
    let mut pwl_gen_k_by_j = HashMap::new();
    let mut sparse_pwl_info_by_hour = Vec::with_capacity(hourly_networks.len());
    for (hour, network) in hourly_networks.iter().enumerate() {
        let info = resolve_pwl_gen_segments_for_period(
            network,
            gen_indices,
            spec.offer_schedules,
            hour,
            base,
            None,
        );
        for (gen_idx, _) in &info {
            if !pwl_gen_k_by_j.contains_key(gen_idx) {
                let pwl_idx = pwl_gen_j.len();
                pwl_gen_j.push(*gen_idx);
                pwl_gen_k_by_j.insert(*gen_idx, pwl_idx);
            }
        }
        sparse_pwl_info_by_hour.push(info);
    }

    let n_pwl_gen = pwl_gen_j.len();
    let mut segments_by_hour = vec![vec![None; n_pwl_gen]; hourly_networks.len()];
    let mut n_pwl_rows_total = 0usize;
    for (hour, info) in sparse_pwl_info_by_hour.into_iter().enumerate() {
        for (gen_idx, segments) in info {
            if let Some(&pwl_idx) = pwl_gen_k_by_j.get(&gen_idx) {
                n_pwl_rows_total += segments.len();
                segments_by_hour[hour][pwl_idx] = Some(segments);
            }
        }
    }

    ScucPwlPlan {
        gen_j: pwl_gen_j,
        segments_by_hour,
        n_rows_total: n_pwl_rows_total,
    }
}

pub(super) fn build_startup_plan(
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
    gen_indices: &[usize],
    n_hours: usize,
) -> ScucStartupPlan {
    let gen_tier_info_by_hour: Vec<Vec<Vec<ScucStartupTierInfo>>> = gen_indices
        .iter()
        .map(|&network_gen_idx| {
            let generator = &network.generators[network_gen_idx];
            if generator.is_storage() {
                return vec![Vec::new(); n_hours];
            }
            (0..n_hours)
                .map(|hour| {
                    let economics = resolve_generator_economics_for_period(
                        network_gen_idx,
                        hour,
                        generator,
                        spec.offer_schedules,
                        Some(generator.pmax),
                    )
                    .expect("validated generators should always have period economics");
                    if economics.startup_tiers.is_empty() {
                        vec![ScucStartupTierInfo {
                            lookback_periods: n_hours + 1,
                            max_offline_hours: f64::INFINITY,
                            cost: economics.startup_cost_for_offline_hours(0.0),
                        }]
                    } else {
                        economics
                            .startup_tiers
                            .iter()
                            .map(|tier| {
                                let lookback_periods = if tier.max_offline_hours.is_infinite() {
                                    n_hours + 1
                                } else if hour == 0 {
                                    0
                                } else {
                                    spec.lookback_periods_covering(hour - 1, tier.max_offline_hours)
                                };
                                ScucStartupTierInfo {
                                    lookback_periods,
                                    max_offline_hours: tier.max_offline_hours,
                                    cost: tier.cost,
                                }
                            })
                            .collect()
                    }
                })
                .collect()
        })
        .collect();

    let startup_tier_capacity: Vec<usize> = gen_tier_info_by_hour
        .iter()
        .map(|hourly_tiers| hourly_tiers.iter().map(Vec::len).max().unwrap_or(0))
        .collect();
    let mut delta_gen_off = Vec::with_capacity(startup_tier_capacity.len());
    let mut n_delta_per_hour = 0usize;
    for &tier_capacity in &startup_tier_capacity {
        delta_gen_off.push(n_delta_per_hour);
        n_delta_per_hour += tier_capacity;
    }

    let pre_horizon_offline_hours = (0..gen_indices.len())
        .map(|gen_idx| {
            let initially_on = spec.initial_commitment_at(gen_idx).unwrap_or(true);
            if initially_on {
                None
            } else {
                Some(
                    spec.initial_offline_hours_at(gen_idx)
                        .unwrap_or(f64::INFINITY),
                )
            }
        })
        .collect();

    ScucStartupPlan {
        gen_tier_info_by_hour,
        startup_tier_capacity,
        delta_gen_off,
        n_delta_per_hour,
        pre_horizon_offline_hours,
    }
}

pub(super) fn build_variable_plan<'spec>(
    input: ScucVariablePlanInput<'spec, '_>,
) -> Result<ScucVariablePlan<'spec>, ScedError> {
    let ScucVariablePlanInput {
        network,
        spec,
        layout,
        n_hours,
        n_gen,
        fg_rows,
        gen_indices,
        dl_list,
        dl_orig_idx,
        dl_activation_infos,
    } = input;

    let commitment_cuts = super::cuts::normalize_commitment_cuts(spec, n_hours, n_gen)?;
    let n_penalty_slacks = super::cuts::penalty_slack_count(&commitment_cuts);
    let penalty_slack_base = layout.penalty_slack_base(n_hours);

    let gen_j_lookup: HashMap<usize, usize> = gen_indices
        .iter()
        .enumerate()
        .map(|(gen_idx, &network_gen_idx)| (network_gen_idx, gen_idx))
        .collect();

    let mut cc_infos = Vec::with_capacity(network.market_data.combined_cycle_plants.len());
    let mut cc_block_size = 0usize;
    for plant in &network.market_data.combined_cycle_plants {
        let n_configs = plant.configs.len();
        let z_block_off = cc_block_size;
        cc_block_size += 3 * n_configs * n_hours;

        let mut member_gen_j = HashSet::new();
        for config in &plant.configs {
            for &network_gen_idx in &config.gen_indices {
                if let Some(&gen_idx) = gen_j_lookup.get(&network_gen_idx) {
                    member_gen_j.insert(gen_idx);
                }
            }
        }

        let config_name_to_idx: HashMap<&str, usize> = plant
            .configs
            .iter()
            .enumerate()
            .map(|(config_idx, config)| (config.name.as_str(), config_idx))
            .collect();
        let mut allowed_transitions = HashMap::new();
        for transition in &plant.transitions {
            if let (Some(&from_idx), Some(&to_idx)) = (
                config_name_to_idx.get(transition.from_config.as_str()),
                config_name_to_idx.get(transition.to_config.as_str()),
            ) {
                allowed_transitions.insert(
                    (from_idx, to_idx),
                    (transition.transition_cost, transition.transition_time_min),
                );
            }
        }

        let mut pgcc_entries = Vec::new();
        for (config_idx, config) in plant.configs.iter().enumerate() {
            for &network_gen_idx in &config.gen_indices {
                if let Some(&gen_idx) = gen_j_lookup.get(&network_gen_idx) {
                    pgcc_entries.push((gen_idx, config_idx));
                }
            }
        }

        let transition_pairs: Vec<(usize, usize)> = allowed_transitions.keys().copied().collect();
        let ytrans_block_off = cc_block_size;
        cc_block_size += transition_pairs.len() * n_hours;
        let pgcc_block_off = cc_block_size;
        cc_block_size += pgcc_entries.len() * n_hours;

        cc_infos.push(ScucCcPlantInfo {
            n_configs,
            z_block_off,
            member_gen_j,
            allowed_transitions,
            pgcc_entries,
            pgcc_block_off,
            transition_pairs,
            ytrans_block_off,
        });
    }

    let cc_var_base = penalty_slack_base + n_penalty_slacks;
    let dl_act_var_base = cc_var_base + cc_block_size;
    let n_dl_act_vars = dl_activation_infos.len() * n_hours;
    let dl_rebound_infos: Vec<usize> = dl_list
        .iter()
        .enumerate()
        .filter_map(|(load_idx, dl)| {
            (dl.rebound_fraction > 0.0 && dl.rebound_periods > 0).then_some(load_idx)
        })
        .collect();
    let n_dl_rebound = dl_rebound_infos.len();
    let dl_rebound_var_base = dl_act_var_base + n_dl_act_vars;

    // Multi-interval energy window slack columns. One slack column
    // per (window, direction) pair. The column ordering is recorded in
    // `energy_window_slack_kinds` so the row builder, the bounds
    // setter, the objective setter, and the extractor all use the
    // same canonical mapping.
    let energy_window_slack_base = dl_rebound_var_base + n_dl_rebound * n_hours;
    let mut energy_window_slack_kinds: Vec<EnergyWindowSlackKind> = Vec::new();
    for (limit_idx, limit) in spec.energy_window_limits.iter().enumerate() {
        if limit.min_energy_mwh.is_some() {
            energy_window_slack_kinds.push(EnergyWindowSlackKind {
                limit_idx,
                direction: EnergyWindowSlackDirection::Min,
            });
        }
        if limit.max_energy_mwh.is_some() {
            energy_window_slack_kinds.push(EnergyWindowSlackKind {
                limit_idx,
                direction: EnergyWindowSlackDirection::Max,
            });
        }
    }
    let n_energy_window_slacks = energy_window_slack_kinds.len();
    let explicit_ctg_case_penalty_base = energy_window_slack_base + n_energy_window_slacks;

    // Pre-compute column-base offsets for explicit-contingency +
    // contingency-cut blocks before building the plan itself, so the
    // cut-slack columns can be referenced from `ExplicitContingencyCasePlan.
    // flowgate_slack_cols` when the Option C path is active.
    let n_cases = spec.explicit_contingency_cases.len();
    let (worst_case_base, avg_case_base, post_explicit_ctg_end) = if n_cases == 0 {
        (
            explicit_ctg_case_penalty_base,
            explicit_ctg_case_penalty_base,
            explicit_ctg_case_penalty_base,
        )
    } else {
        let wcb = explicit_ctg_case_penalty_base + n_cases;
        let acb = wcb + n_hours;
        (wcb, acb, acb + n_hours)
    };
    let n_cut_rows = spec.contingency_cuts.len();
    let cut_slack_lower_base = post_explicit_ctg_end;
    let cut_slack_upper_base = cut_slack_lower_base + n_cut_rows;
    let n_var = cut_slack_upper_base + n_cut_rows;

    let explicit_contingency = if n_cases == 0 {
        None
    } else if !spec.contingency_cuts.is_empty() {
        // Option C path: group cuts by case_index. Each case's
        // `flowgate_slack_cols` becomes the list of post-hourly
        // cut-slack column pairs — one entry per cut belonging to
        // the case. `flowgate_row_cases` (indexed by fg_rows) stays
        // empty because the cut path doesn't route through the
        // `network.flowgates` row family at all.
        let mut case_cut_indices: Vec<Vec<usize>> = vec![Vec::new(); n_cases];
        for (cut_idx, cut) in spec.contingency_cuts.iter().enumerate() {
            let case_idx = cut.case_index as usize;
            if let Some(slot) = case_cut_indices.get_mut(case_idx) {
                slot.push(cut_idx);
            }
        }

        let cases = spec
            .explicit_contingency_cases
            .iter()
            .enumerate()
            .map(|(case_index, case)| {
                let slack_cols = case_cut_indices[case_index]
                    .iter()
                    .map(|&cut_idx| {
                        (
                            cut_slack_lower_base + cut_idx,
                            cut_slack_upper_base + cut_idx,
                        )
                    })
                    .collect();
                ExplicitContingencyCasePlan {
                    case_index,
                    period: case.period,
                    penalty_col: explicit_ctg_case_penalty_base + case_index,
                    flowgate_slack_cols: slack_cols,
                }
            })
            .collect::<Vec<_>>();

        let mut period_case_indices = vec![Vec::new(); n_hours];
        for case_plan in &cases {
            if let Some(period_cases) = period_case_indices.get_mut(case_plan.period) {
                period_cases.push(case_plan.case_index);
            }
        }
        let periods = period_case_indices
            .into_iter()
            .enumerate()
            .map(|(period, case_indices)| ExplicitContingencyPeriodPlan {
                case_indices,
                worst_case_col: worst_case_base + period,
                avg_case_col: avg_case_base + period,
            })
            .collect::<Vec<_>>();

        Some(ExplicitContingencyObjectivePlan {
            case_penalty_base: explicit_ctg_case_penalty_base,
            worst_case_base,
            avg_case_base,
            cases,
            periods,
            // Cut path does not populate fg_rows, so the
            // per-fg-row case map stays empty. `bounds.rs` /
            // `extract.rs` loops that consult it are naturally no-ops
            // because `n_fg_rows == 0` on this path.
            flowgate_row_cases: Vec::new(),
        })
    } else {
        // Legacy Flowgate path (SCED and iterative security stay here).
        let flowgate_row_by_index: HashMap<usize, usize> = fg_rows
            .iter()
            .enumerate()
            .map(|(row_idx, &fg_idx)| (fg_idx, row_idx))
            .collect();
        let mut flowgate_row_cases = vec![None; fg_rows.len()];
        let mut case_flowgate_rows = vec![Vec::new(); n_cases];
        for mapping in spec.explicit_contingency_flowgates {
            let Some(&flowgate_row) = flowgate_row_by_index.get(&mapping.flowgate_idx) else {
                continue;
            };
            if let Some(slot) = flowgate_row_cases.get_mut(flowgate_row) {
                *slot = Some(mapping.case_index);
            }
            if let Some(rows) = case_flowgate_rows.get_mut(mapping.case_index) {
                rows.push(flowgate_row);
            }
        }
        for rows in &mut case_flowgate_rows {
            rows.sort_unstable();
        }

        let cases = spec
            .explicit_contingency_cases
            .iter()
            .enumerate()
            .map(|(case_index, case)| {
                let rows = std::mem::take(&mut case_flowgate_rows[case_index]);
                let slack_cols = rows
                    .iter()
                    .map(|&fg_row| {
                        (
                            layout.flowgate_lower_slack_col(case.period, fg_row),
                            layout.flowgate_upper_slack_col(case.period, fg_row),
                        )
                    })
                    .collect();
                ExplicitContingencyCasePlan {
                    case_index,
                    period: case.period,
                    penalty_col: explicit_ctg_case_penalty_base + case_index,
                    flowgate_slack_cols: slack_cols,
                }
            })
            .collect::<Vec<_>>();

        let mut period_case_indices = vec![Vec::new(); n_hours];
        for case_plan in &cases {
            if let Some(period_cases) = period_case_indices.get_mut(case_plan.period) {
                period_cases.push(case_plan.case_index);
            }
        }
        let periods = period_case_indices
            .into_iter()
            .enumerate()
            .map(|(period, case_indices)| ExplicitContingencyPeriodPlan {
                case_indices,
                worst_case_col: worst_case_base + period,
                avg_case_col: avg_case_base + period,
            })
            .collect::<Vec<_>>();

        Some(ExplicitContingencyObjectivePlan {
            case_penalty_base: explicit_ctg_case_penalty_base,
            worst_case_base,
            avg_case_base,
            cases,
            periods,
            flowgate_row_cases,
        })
    };

    let dr_activation_loads: Vec<ScucDrActivationLoad> = dl_activation_infos
        .iter()
        .enumerate()
        .map(|(activation_idx, info)| {
            let dl = dl_list[info.load_idx];
            ScucDrActivationLoad {
                load_idx: info.load_idx,
                activation_block_base: dl_act_var_base + activation_idx * n_hours,
                min_duration_periods: info.min_duration_periods,
                p_sched_pu: dl.p_sched_pu,
                curtailment_range_pu: dl.p_sched_pu - dl.p_min_pu,
            }
        })
        .collect();
    let dr_rebound_loads: Vec<ScucDrReboundLoad> = dl_rebound_infos
        .iter()
        .enumerate()
        .map(|(rebound_idx, &load_idx)| {
            let dl = dl_list[load_idx];
            ScucDrReboundLoad {
                load_idx,
                original_load_idx: dl_orig_idx[load_idx],
                rebound_block_base: dl_rebound_var_base + rebound_idx * n_hours,
                rebound_fraction: dl.rebound_fraction,
                rebound_periods: dl.rebound_periods,
            }
        })
        .collect();

    Ok(ScucVariablePlan {
        commitment_cuts,
        n_penalty_slacks,
        penalty_slack_base,
        cc_infos,
        cc_var_base,
        cc_block_size,
        dl_act_var_base,
        dl_rebound_infos,
        n_dl_rebound,
        dl_rebound_var_base,
        energy_window_slack_base,
        n_energy_window_slacks,
        energy_window_slack_kinds,
        explicit_contingency,
        cut_slack_lower_base,
        cut_slack_upper_base,
        n_cut_rows,
        n_var,
        dr_activation_loads,
        dr_rebound_loads,
    })
}
