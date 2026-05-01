// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC variable bounds and integrality assembly.

use rayon::prelude::*;
use surge_network::Network;
use surge_network::market::{
    DispatchableLoad, ReserveDirection, SystemReserveRequirement, ZonalReserveRequirement,
    qualifies_for,
};
use surge_network::network::StorageDispatchMode;
use surge_opf::advanced::{IslandRefs, fix_island_theta_bounds};
use surge_opf::backends::VariableDomain;
use tracing::info;

use super::layout::ScucLayout;
use super::rows::ScucStartupTierInfo;
use crate::common::blocks::DispatchBlock;
use crate::common::costs::{
    active_energy_offer_curve, resolve_dl_for_period_from_spec,
    resolve_generator_economics_for_period,
};
use crate::common::network::study_area_for_bus;
use crate::common::reserves::{
    ReserveLpLayout, dispatchable_load_reserve_offer_for_period, generator_reserve_offer_for_period,
};
use crate::common::spec::DispatchProblemSpec;
use crate::dispatch::CommitmentMode;

type PwlSegments = Vec<(f64, f64)>;
type HourlyPwlSegments = Vec<Option<PwlSegments>>;

fn log_scuc_bounds_trace(message: impl AsRef<str>) {
    info!("scuc_bounds: {}", message.as_ref());
}

fn reserve_generator_qualified_in_scuc(
    spec: &DispatchProblemSpec<'_>,
    period: usize,
    local_gen_idx: usize,
    qualification: &surge_network::market::QualificationRule,
    is_quick_start: bool,
    qualifications: &surge_network::market::QualificationMap,
) -> bool {
    // In optimize mode there is no fixed on/off schedule yet, so
    // `DispatchPeriodSpec::is_committed()` defaults to `true`. That is fine
    // for committed/synchronized products, whose SCUC rows already couple the
    // reserve variable to the commitment binary `u`, but it incorrectly zeros
    // OfflineQuickStart products (e.g. non-synchronous reserves). Only use an explicit
    // fixed commitment when one exists; otherwise leave offline quick-start
    // eligibility to the reserve rows themselves.
    let is_committed = spec
        .period(period)
        .fixed_commitment()
        .and_then(|commitment| commitment.get(local_gen_idx))
        .copied()
        .unwrap_or(!matches!(
            qualification,
            surge_network::market::QualificationRule::OfflineQuickStart
        ));
    qualifies_for(qualification, is_committed, is_quick_start, qualifications)
}

pub(super) struct ScucBoundsFozGroup {
    pub delta_local_off: usize,
    pub phi_local_off: usize,
    pub rho_local_off: usize,
    pub n_segments: usize,
    pub max_transit: Vec<usize>,
}

pub(super) struct ScucBoundsPhMode {
    pub m_gen_local_off: usize,
    pub m_pump_local_off: usize,
}

pub(super) struct ScucBoundsCcPlant {
    pub n_configs: usize,
    pub z_block_off: usize,
    pub ytrans_block_off: usize,
    pub pgcc_block_off: usize,
    pub n_transition_pairs: usize,
    pub pgcc_gen_j: Vec<usize>,
    pub initial_active_config: Option<usize>,
    pub initial_config_force_periods: usize,
}

pub(super) struct ScucBoundsDrActivation {
    pub n_notify: usize,
}

pub(super) struct ScucBoundsInput<'a> {
    pub network: &'a Network,
    pub hourly_networks: &'a [Network],
    pub spec: &'a DispatchProblemSpec<'a>,
    pub layout: &'a ScucLayout,
    pub island_refs: &'a IslandRefs,
    pub n_var: usize,
    pub n_hours: usize,
    pub n_bus: usize,
    pub n_bp: usize,
    pub n_sbp: usize,
    pub n_branch_flow: usize,
    /// Maps thermal-row index `row_idx` to the source branch index in
    /// `network.branches`. Used when bounding `col_upper` on the lower/
    /// upper thermal slack columns — we cap each at `slack_cap_factor ×
    /// branch_rating_a_mva / base_mva` so the LP relaxation can't
    /// hallucinate unbounded slack absorbing pathological dual bounds
    /// (which observably drove -$1.7e14 dual bounds on 1576-bus D1 with
    /// the `f64::INFINITY` cap). Physical violations remain accommodated
    /// as long as they stay within the factor × rating envelope.
    pub constrained_branches: &'a [usize],
    pub n_fg_rows: usize,
    /// Map from flowgate LP row index to index into `network.flowgates`.
    /// The bounds layer uses it to look up `limit_mw_active_period` +
    /// `breach_sides` when deciding whether to allocate each side's
    /// slack column or pin it to zero (lp-reduce drops the pinned ones).
    pub fg_rows: &'a [usize],
    pub n_iface_rows: usize,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_block_vars_per_hour: usize,
    pub is_block_mode: bool,
    pub use_plc: bool,
    pub has_reg_products: bool,
    pub has_per_block_reserves: bool,
    pub gen_indices: &'a [usize],
    pub gen_blocks: &'a [Vec<DispatchBlock>],
    pub gen_block_start: &'a [usize],
    pub gen_tier_info_by_hour: &'a [Vec<Vec<ScucStartupTierInfo>>],
    pub startup_tier_capacity: &'a [usize],
    pub delta_gen_off: &'a [usize],
    pub reserve_layout: &'a ReserveLpLayout,
    pub r_sys_reqs: &'a [SystemReserveRequirement],
    pub r_zonal_reqs: &'a [ZonalReserveRequirement],
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub hvdc_band_offsets: &'a [usize],
    pub pwl_gen_segments_by_hour: &'a [HourlyPwlSegments],
    pub dl_list: &'a [&'a DispatchableLoad],
    pub dl_orig_idx: &'a [usize],
    pub active_vbids: &'a [usize],
    pub foz_groups: &'a [ScucBoundsFozGroup],
    pub ph_mode_infos: &'a [ScucBoundsPhMode],
    pub cc_var_base: usize,
    pub cc_plants: &'a [ScucBoundsCcPlant],
    pub cc_member_gen: &'a [bool],
    pub dl_act_var_base: usize,
    pub dl_activation_infos: &'a [ScucBoundsDrActivation],
    pub dl_rebound_var_base: usize,
    pub n_dl_rebound: usize,
    /// Base column index for the multi-interval energy window slack
    /// columns. Set to 0 with empty kinds when no energy windows exist.
    pub energy_window_slack_base: usize,
    pub energy_window_slack_kinds: &'a [super::plan::EnergyWindowSlackKind],
    pub explicit_contingency: Option<&'a super::plan::ExplicitContingencyObjectivePlan>,
    /// Post-hourly slack-column bases for the Option C compact
    /// contingency-cut row family. `cut_slack_lower_base..+n_cut_rows`
    /// holds `σ⁻_k`; `cut_slack_upper_base..+n_cut_rows` holds `σ⁺_k`.
    /// All zero when the Flowgate path is in use.
    pub cut_slack_lower_base: usize,
    pub cut_slack_upper_base: usize,
    pub n_cut_rows: usize,
    pub base: f64,
    pub col_cost: &'a mut [f64],
}

pub(super) struct ScucBoundsState {
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
    pub integrality: Vec<VariableDomain>,
}

/// True when the (gen, period) pair cannot be committed for a physical
/// reason: derate profile says off, or hourly pmax is zero on a non-
/// storage unit. Mirrors the must-off conditions used elsewhere in the
/// SCUC. Used by the zero-no-load-cost must-run pin to determine when
/// to stop the contiguous u=1 run.
fn period_is_must_off(
    spec: &DispatchProblemSpec<'_>,
    network: &Network,
    hourly_networks: &[Network],
    base: f64,
    gi: usize,
    t: usize,
) -> bool {
    if is_forced_offline(spec, network, gi, t) {
        return true;
    }
    let generator = &network.generators[gi];
    if generator.is_storage() {
        return false;
    }
    hourly_networks
        .get(t)
        .map(|net| net.generators[gi].pmax / base <= 1e-9)
        .unwrap_or(false)
}

/// True when every period in `[0, n_hours)` has zero no-load cost and
/// `pmin <= 0`, respecting per-period offer-schedule overrides. The
/// no-load-cost cost curve ternary `cost.evaluate(0.0)` captures the
/// constant term that `offer_curve_to_cost_curve` embeds from
/// `no_load_cost`. `pmin <= 0` allows the unit to sit at zero output
/// while committed, so u=1 never forces a positive energy dispatch.
fn unit_has_zero_no_load_and_nonpositive_pmin(
    spec: &DispatchProblemSpec<'_>,
    network: &Network,
    hourly_networks: &[Network],
    base: f64,
    gi: usize,
    n_hours: usize,
) -> bool {
    let generator = &network.generators[gi];
    for t in 0..n_hours {
        let Some(econ) = resolve_generator_economics_for_period(
            gi,
            t,
            generator,
            spec.offer_schedules,
            Some(generator.pmax),
        ) else {
            return false;
        };
        if econ.cost.evaluate(0.0) > 1e-9 {
            return false;
        }
        let pmin_pu = hourly_networks
            .get(t)
            .map(|net| net.generators[gi].pmin / base)
            .unwrap_or(generator.pmin / base);
        if pmin_pu > 1e-9 {
            return false;
        }
    }
    true
}

/// True when every startup tier cost is zero (both from the generator's
/// submitted offer curve and from any period-specific offer-schedule
/// override) AND the static cost curve's fallback startup cost is zero.
/// A unit that satisfies this can be committed at t=0 without paying
/// any startup fee even when `initial_on` is false.
fn unit_has_zero_startup(
    spec: &DispatchProblemSpec<'_>,
    network: &Network,
    gi: usize,
    n_hours: usize,
) -> bool {
    let generator = &network.generators[gi];
    // Static cost curve fallback.
    if let Some(cost) = generator.cost.as_ref() {
        let static_startup = match cost {
            surge_network::market::CostCurve::Polynomial { startup, .. }
            | surge_network::market::CostCurve::PiecewiseLinear { startup, .. } => *startup,
        };
        if static_startup > 1e-9 {
            return false;
        }
    }
    // Generator-level submitted offer curve tiers.
    if let Some(curve) = active_energy_offer_curve(generator) {
        for tier in &curve.startup_tiers {
            if tier.cost > 1e-9 {
                return false;
            }
        }
    }
    // Per-period offer schedule overrides.
    for t in 0..n_hours {
        let Some(econ) = resolve_generator_economics_for_period(
            gi,
            t,
            generator,
            spec.offer_schedules,
            Some(generator.pmax),
        ) else {
            return false;
        };
        for tier in econ.startup_tiers.iter() {
            if tier.cost > 1e-9 {
                return false;
            }
        }
    }
    true
}

fn is_forced_offline(
    spec: &DispatchProblemSpec<'_>,
    network: &Network,
    gi: usize,
    t: usize,
) -> bool {
    let g = &network.generators[gi];
    spec.gen_derate_profiles.profiles.iter().any(|profile| {
        profile.generator_id == g.id
            && t < profile.derate_factors.len()
            && profile.derate_factors[t] == 0.0
    })
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "on" | "yes")
    )
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn fixed_binary_value(lower: f64, upper: f64) -> Option<bool> {
    if lower >= 0.5 && upper >= 0.5 {
        Some(true)
    } else if lower <= 0.5 && upper <= 0.5 {
        Some(false)
    } else {
        None
    }
}

fn zonal_slack_upper_bound_mw(
    input: &ScucBoundsInput<'_>,
    t: usize,
    req: &crate::common::reserves::ActiveZonalRequirement,
) -> f64 {
    let profiled_network = input.hourly_networks.get(t).unwrap_or(input.network);
    // Fast bus-match using the precomputed HashSet on `req`. Previously
    // this used `participants.contains(&bus)` on the raw Vec — O(P_zone)
    // per check — inside per-DL and per-gen loops. With 16,000+ DLs and
    // ~12 zonal requirements over 18 periods, the Vec path was the
    // dominant build-side cost on 6049-bus SCUC.
    //
    // Note: the closure takes a `fallback_area_fn` rather than a
    // pre-computed `Option<usize>` because the fallback path (used when
    // no explicit participant set) calls `study_area_for_bus`, which
    // internally rebuilds `network.bus_index_map()` — O(N_buses) per
    // call. Lazy evaluation means the fallback computation is skipped
    // on every iteration when the participant set is known, which is
    // the common case for GO C3 zones.
    let bus_matches = |bus_number: u32, fallback_area_fn: &dyn Fn() -> Option<usize>| -> bool {
        match &req.participant_bus_set {
            Some(set) => set.contains(&bus_number),
            None => fallback_area_fn().unwrap_or(0) == req.zone_id,
        }
    };

    let base_requirement_mw = req
        .balance_req_indices
        .iter()
        .filter_map(|&idx| input.r_zonal_reqs.get(idx))
        .map(|item| item.requirement_mw_for_period(t))
        .sum::<f64>();
    // Skip the inner DL iteration when the coefficient is zero /
    // absent — multiplying the sum by 0 is wasteful when the sum
    // requires walking 16,000+ DLs with per-DL HashMap work.
    let served_dispatchable_load_cap_mw = req
        .balance_served_dispatchable_load_coefficient
        .filter(|&c| c.abs() > 0.0)
        .map(|coeff| {
            coeff
                * input
                    .dl_list
                    .iter()
                    .enumerate()
                    .filter(|&(_, dl)| {
                        bus_matches(dl.bus, &|| {
                            study_area_for_bus(profiled_network, input.spec, dl.bus)
                        })
                    })
                    .map(|(k, dl)| {
                        let dl_idx = input.dl_orig_idx.get(k).copied().unwrap_or(k);
                        let (_, p_max_pu, _, _, _, _) =
                            resolve_dl_for_period_from_spec(dl_idx, t, dl, input.spec);
                        p_max_pu.max(0.0) * input.base
                    })
                    .sum::<f64>()
        })
        .unwrap_or(0.0);
    let largest_generator_cap_mw = req
        .balance_largest_generator_dispatch_coefficient
        .filter(|&c| c.abs() > 0.0)
        .map(|coeff| {
            coeff
                * input
                    .gen_indices
                    .iter()
                    .enumerate()
                    .filter_map(|(j, &gi)| {
                        bus_matches(profiled_network.generators[gi].bus, &|| {
                            input.spec.generator_area.get(j).copied()
                        })
                        .then_some(profiled_network.generators[gi].pmax.max(0.0))
                    })
                    .fold(0.0, f64::max)
        })
        .unwrap_or(0.0);

    (base_requirement_mw + served_dispatchable_load_cap_mw + largest_generator_cap_mw).max(0.0)
}

fn pin_binary_bounds(col_lower: &mut [f64], col_upper: &mut [f64], idx: usize, value: bool) {
    let bound = if value { 1.0 } else { 0.0 };
    col_lower[idx] = bound;
    col_upper[idx] = bound;
}

fn cc_z_idx(
    cc_var_base: usize,
    plant: &ScucBoundsCcPlant,
    c: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.z_block_off + c * n_hours + t
}

fn cc_yup_idx(
    cc_var_base: usize,
    plant: &ScucBoundsCcPlant,
    c: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.z_block_off + plant.n_configs * n_hours + c * n_hours + t
}

fn cc_ydn_idx(
    cc_var_base: usize,
    plant: &ScucBoundsCcPlant,
    c: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.z_block_off + 2 * plant.n_configs * n_hours + c * n_hours + t
}

fn cc_ytrans_idx(
    cc_var_base: usize,
    plant: &ScucBoundsCcPlant,
    tr_idx: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.ytrans_block_off + tr_idx * n_hours + t
}

fn cc_pgcc_idx(
    cc_var_base: usize,
    plant: &ScucBoundsCcPlant,
    entry_idx: usize,
    t: usize,
    n_hours: usize,
) -> usize {
    cc_var_base + plant.pgcc_block_off + entry_idx * n_hours + t
}

fn dl_act_idx(base: usize, info_idx: usize, t: usize, n_hours: usize) -> usize {
    base + info_idx * n_hours + t
}

fn dl_rebound_idx(base: usize, rb_idx: usize, t: usize, n_hours: usize) -> usize {
    base + rb_idx * n_hours + t
}

pub(super) fn build_variable_bounds(input: ScucBoundsInput<'_>) -> ScucBoundsState {
    use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
    use std::time::Instant;
    let _bounds_fn_t0 = Instant::now();
    // Per-section nanosecond accumulators for bounds build sub-phase
    // timings. Written atomically from rayon tasks (one task per
    // period), reported as a single `info!` after the parallel loop
    // exits.
    let t_gen_bounds = AtomicU64::new(0);
    let t_reserves = AtomicU64::new(0);
    let t_reserves_gen = AtomicU64::new(0);
    let t_reserves_dlgroup = AtomicU64::new(0);
    let t_reserves_zonal = AtomicU64::new(0);
    let t_dl_bounds = AtomicU64::new(0);
    let t_branch_slacks = AtomicU64::new(0);
    let t_flowgate_slacks = AtomicU64::new(0);
    let t_gen_commit = AtomicU64::new(0);
    let t_other = AtomicU64::new(0);
    let ignore_forced_offline_commitment =
        env_flag("SURGE_DEBUG_IGNORE_SCUC_FORCED_OFFLINE_COMMITMENT_BOUNDS");
    let relax_commitment_binaries = env_flag("SURGE_DEBUG_RELAX_SCUC_COMMITMENT_BINARIES");
    let relax_auxiliary_binaries = env_flag("SURGE_DEBUG_RELAX_SCUC_AUXILIARY_BINARIES");
    let trace_commitment_bounds = env_flag("SURGE_DEBUG_TRACE_SCUC_COMMITMENT_BOUNDS");
    let trace_commitment_unit = env_value("SURGE_DEBUG_TRACE_SCUC_UNIT");
    let mut col_lower = vec![0.0; input.n_var];
    let mut col_upper = vec![0.0; input.n_var];
    let mut integrality = vec![VariableDomain::Continuous; input.n_var];
    let n_gen = input.gen_indices.len();
    let has_hvdc = !input.spec.hvdc_links.is_empty();
    let has_dl = !input.dl_list.is_empty();
    let has_foz = !input.foz_groups.is_empty();
    let has_ph_mode = !input.ph_mode_infos.is_empty();
    let pb_penalty = input.spec.power_balance_penalty;

    // Parallelize the per-period bounds loop with rayon. Each period's
    // body writes strictly to the disjoint chunk
    // `[t*n_vars_per_hour, (t+1)*n_vars_per_hour)` of col_lower / col_upper
    // / integrality via `layout.col(t, offset)`, so per-period work is
    // independent. Pass raw pointers through AtomicPtr so each thread
    // can reconstruct a full `&mut [...]` view without falling afoul of
    // the borrow checker; SAFETY follows from the disjoint-write
    // invariant above.
    let total_n_var = input.n_var;
    let col_lower_ptr = AtomicPtr::new(col_lower.as_mut_ptr());
    let col_upper_ptr = AtomicPtr::new(col_upper.as_mut_ptr());
    let integrality_ptr = AtomicPtr::new(integrality.as_mut_ptr());
    // `col_cost` is the only &mut field on `ScucBoundsInput`; capture
    // it as a raw pointer so the parallel closure can reconstruct a
    // per-task &mut slice alongside the three bound arrays.
    let col_cost_len = input.col_cost.len();
    let col_cost_ptr = AtomicPtr::new(input.col_cost.as_mut_ptr());
    // Borrow the rest of `input` immutably for the parallel body. The
    // captured `&input` fields must be Sync; they are (all inner
    // references target `Sync` data).
    let input_ref: &ScucBoundsInput<'_> = &input;
    let trace_commitment_unit_ref = &trace_commitment_unit;

    let period_entries: Vec<_> = input
        .pwl_gen_segments_by_hour
        .iter()
        .enumerate()
        .take(input.n_hours)
        .collect();

    period_entries.into_par_iter().for_each(|(t, pwl_segments_t)| {
        // Shadow the outer `input` with a per-task immutable view so
        // the unchanged loop body still refers to `input.x` for the
        // read-only fields. The mutable `col_cost` field is accessed
        // through the local `col_cost` binding below; any body access
        // to `input.col_cost[...]` is rewritten accordingly.
        let input = input_ref;
        let trace_commitment_unit = trace_commitment_unit_ref;
        // SAFETY: each rayon task writes only to its own disjoint
        // per-period slice of these arrays; the Vec (and &mut slice)
        // backings are not reallocated while par_iter runs; total_n_var
        // / col_cost_len are the authoritative lengths.
        let col_lower: &mut [f64] = unsafe {
            std::slice::from_raw_parts_mut(
                col_lower_ptr.load(Ordering::Relaxed),
                total_n_var,
            )
        };
        let col_upper: &mut [f64] = unsafe {
            std::slice::from_raw_parts_mut(
                col_upper_ptr.load(Ordering::Relaxed),
                total_n_var,
            )
        };
        let integrality: &mut [VariableDomain] = unsafe {
            std::slice::from_raw_parts_mut(
                integrality_ptr.load(Ordering::Relaxed),
                total_n_var,
            )
        };
        let col_cost: &mut [f64] = unsafe {
            std::slice::from_raw_parts_mut(
                col_cost_ptr.load(Ordering::Relaxed),
                col_cost_len,
            )
        };
    {
        let hour_base = input.layout.hour_col_base(t);
        let dt_h = input.spec.period_hours(t);
        fix_island_theta_bounds(
            &mut col_lower[hour_base..],
            &mut col_upper[hour_base..],
            0,
            input.n_bus,
            input.island_refs,
        );

        let _t_gen_t0 = std::time::Instant::now();
        for (j, &gi) in input.gen_indices.iter().enumerate() {
            let g_hourly = &input.hourly_networks[t].generators[gi];
            let g_base = &input.network.generators[gi];
            let idx = input.layout.pg_col(t, j);
            col_lower[idx] = if g_base.is_storage() || g_hourly.pmin < 0.0 {
                g_hourly.pmin / input.base
            } else {
                0.0
            };
            col_upper[idx] = g_hourly.pmax / input.base;
        }

        for (j, &gi) in input.gen_indices.iter().enumerate() {
            let g_base = &input.network.generators[gi];
            let forced_offline = is_forced_offline(input.spec, input.network, gi, t);
            let enforce_forced_offline = forced_offline && !ignore_forced_offline_commitment;

            let startup_cap_pu =
                g_base.startup_ramp_mw_per_period(input.spec.period_hours(t)) / input.base;
            // Only the first in-horizon interval can be truly
            // startup-infeasible on this basis. For later intervals,
            // startup trajectories can inject positive pre-startup
            // power in earlier periods, so a unit can still transition
            // on even when this interval's standalone ramp volume is
            // below pmin.
            let startup_infeasible_at_horizon_start = t == 0
                && matches!(input.spec.initial_commitment_at(j), Some(false))
                && input.spec.enforce_shutdown_deloading
                && !g_base.is_storage()
                && startup_cap_pu + 1e-9 < (g_base.pmin / input.base);
            let u_idx = input.layout.commitment_col(t, j);
            let initial_commitment = input.spec.initial_commitment_at(j);
            let forbid_period_zero_commitment = t == 0
                && matches!(initial_commitment, Some(false))
                && startup_infeasible_at_horizon_start;
            col_lower[u_idx] = 0.0;
            col_upper[u_idx] = if enforce_forced_offline || forbid_period_zero_commitment {
                0.0
            } else {
                1.0
            };
            if !relax_commitment_binaries {
                integrality[u_idx] = VariableDomain::Binary;
            }

            let v_idx = input.layout.startup_col(t, j);
            col_lower[v_idx] = 0.0;
            let forbid_period_zero_startup = t == 0 && matches!(initial_commitment, Some(true));
            col_upper[v_idx] = if enforce_forced_offline
                || startup_infeasible_at_horizon_start
                || forbid_period_zero_startup
            {
                0.0
            } else {
                1.0
            };
            if !relax_commitment_binaries {
                integrality[v_idx] = VariableDomain::Binary;
            }

            let w_idx = input.layout.shutdown_col(t, j);
            col_lower[w_idx] = 0.0;
            let forbid_period_zero_shutdown = t == 0 && matches!(initial_commitment, Some(false));
            let preserve_additional_commitment_prefix =
                input.spec.additional_commitment_prefix_through(t, j);
            col_upper[w_idx] =
                if forbid_period_zero_shutdown || preserve_additional_commitment_prefix {
                    0.0
                } else {
                    1.0
                };
            if !relax_commitment_binaries {
                integrality[w_idx] = VariableDomain::Binary;
            }

            if trace_commitment_bounds
                && t == 0
                && trace_commitment_unit
                    .as_deref()
                    .is_none_or(|unit_id| unit_id == g_base.id)
            {
                log_scuc_bounds_trace(format!(
                    "scuc_bounds_trace unit={} t=0 idxs(u={},v={},w={},pg={}) initial_commitment={:?} startup_cap_pu={:.6} pmin_pu={:.6} forced_offline={} forbid_u0={} keep_additional_prefix={} u_bounds=[{:.1},{:.1}] v_bounds=[{:.1},{:.1}] w_bounds=[{:.1},{:.1}]",
                    g_base.id,
                    u_idx,
                    v_idx,
                    w_idx,
                    input.layout.pg_col(t, j),
                    initial_commitment,
                    startup_cap_pu,
                    g_base.pmin / input.base,
                    enforce_forced_offline,
                    forbid_period_zero_commitment,
                    preserve_additional_commitment_prefix,
                    col_lower[u_idx],
                    col_upper[u_idx],
                    col_lower[v_idx],
                    col_upper[v_idx],
                    col_lower[w_idx],
                    col_upper[w_idx],
                ));
            }

            let active_tiers = &input.gen_tier_info_by_hour[j][t];
            for k in 0..input.startup_tier_capacity[j] {
                let d_idx = input
                    .layout
                    .col(t, input.layout.startup_delta + input.delta_gen_off[j] + k);
                col_lower[d_idx] = 0.0;
                col_upper[d_idx] = if k < active_tiers.len() { 1.0 } else { 0.0 };
                if trace_commitment_bounds
                    && trace_commitment_unit
                        .as_deref()
                        .is_none_or(|unit_id| unit_id == g_base.id)
                {
                    log_scuc_bounds_trace(format!(
                        "startup_delta_init unit={} t={} k={} active_tiers={} d_idx={} bounds=[{:.1},{:.1}]",
                        g_base.id,
                        t,
                        k,
                        active_tiers.len(),
                        d_idx,
                        col_lower[d_idx],
                        col_upper[d_idx],
                    ));
                }
            }
        }

        t_gen_bounds.fetch_add(_t_gen_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let _t_reserves_t0 = std::time::Instant::now();
        for ap in &input.reserve_layout.products {
            let _t_rgen_t0 = std::time::Instant::now();
            for (j, &gi) in input.gen_indices.iter().enumerate() {
                // Non-participants have no reserve column — skip bounds entirely.
                let Some(intra_period_col) = ap.gen_reserve_col(j) else {
                    continue;
                };
                let g = &input.network.generators[gi];
                let col = input.layout.col(t, intra_period_col);
                col_lower[col] = 0.0;

                let empty_quals = Default::default();
                let quals = g
                    .market
                    .as_ref()
                    .map(|m| &m.qualifications)
                    .unwrap_or(&empty_quals);
                let qualified = reserve_generator_qualified_in_scuc(
                    input.spec,
                    t,
                    j,
                    &ap.product.qualification,
                    g.quick_start,
                    quals,
                );
                if !qualified {
                    col_upper[col] = 0.0;
                    continue;
                }
                let offer_cap =
                    generator_reserve_offer_for_period(input.spec, gi, g, &ap.product.id, t)
                        .map(|offer| offer.capacity_mw)
                        .unwrap_or(0.0);
                let g_h = &input.hourly_networks[t].generators[gi];
                let is_offline_quick_start = g.quick_start
                    && matches!(
                        ap.product.qualification,
                        surge_network::market::QualificationRule::OfflineQuickStart
                            | surge_network::market::QualificationRule::QuickStart
                    );
                let ramp_cap = if is_offline_quick_start {
                    // Offline quick-start reserve should be limited by its explicit
                    // reserve capability, not the online energy ramp surrogate.
                    offer_cap
                } else if !ap.product.apply_deploy_ramp_limit {
                    // Some market data already encodes a deliverable reserve
                    // capability in the explicit product cap, so the generic
                    // deploy-window ramp clamp would double-count it.
                    f64::INFINITY
                } else if matches!(
                    ap.product.kind,
                    surge_network::market::ReserveKind::ReactiveHeadroom
                ) {
                    // Reactive headroom has no ramp-rate constraint —
                    // the Q range is available instantaneously.
                    f64::INFINITY
                } else {
                    g.ramp_limited_mw(&ap.product)
                };
                let phys_cap = if is_offline_quick_start {
                    g_h.pmax.max(0.0)
                } else if matches!(
                    ap.product.kind,
                    surge_network::market::ReserveKind::ReactiveHeadroom
                ) {
                    // For reactive headroom, the physical Q range is
                    // the cap on how much reactive reserve a committed
                    // unit can deliver. Use Q bounds instead of P
                    // bounds.
                    (g_h.qmax - g_h.qmin).max(0.0)
                } else {
                    (g_h.pmax - g_h.pmin).max(0.0)
                };
                col_upper[col] = offer_cap.min(ramp_cap).min(phys_cap) / input.base;
            }

            t_reserves_gen.fetch_add(_t_rgen_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

            let slack_req_mw = ap
                .system_req_indices
                .iter()
                .filter_map(|&idx| input.r_sys_reqs.get(idx))
                .map(|req: &SystemReserveRequirement| req.requirement_mw_for_period(t))
                .sum::<f64>();
            let slack_col = input.layout.col(t, ap.slack_offset);
            col_lower[slack_col] = 0.0;
            col_upper[slack_col] = if slack_req_mw > 0.0 {
                slack_req_mw / input.base
            } else {
                0.0
            };

            // DL reserve bounds are group-level in Phase 3. Offer cap
            // and physical cap sum across member blocks. Qualification
            // is per-member; if any member qualifies we keep the col
            // open with the summed bounds, otherwise pin to 0.
            let _t_rdl_t0 = std::time::Instant::now();
            for (gi, group) in input.reserve_layout.dl_consumer_groups.iter().enumerate() {
                let Some(intra_period_col) = ap.dl_group_reserve_col(gi) else {
                    continue;
                };
                let col = input.layout.col(t, intra_period_col);
                col_lower[col] = 0.0;

                let mut any_qualified = false;
                let mut total_offer_mw = 0.0;
                let mut total_phys_mw = 0.0;
                for &k in &group.member_dl_indices {
                    let dl = input.dl_list[k];
                    let qualified = qualifies_for(
                        &ap.product.qualification,
                        true,
                        false,
                        &dl.qualifications,
                    );
                    if !qualified {
                        continue;
                    }
                    any_qualified = true;
                    let offer_cap = dispatchable_load_reserve_offer_for_period(
                        input.spec,
                        input.dl_orig_idx.get(k).copied().unwrap_or(k),
                        dl,
                        &ap.product.id,
                        t,
                    )
                    .map(|offer| offer.capacity_mw)
                    .unwrap_or(0.0);
                    let phys_cap_member = (dl.p_max_pu - dl.p_min_pu).max(0.0) * input.base;
                    total_offer_mw += offer_cap;
                    total_phys_mw += phys_cap_member;
                }
                if !any_qualified {
                    col_upper[col] = 0.0;
                    continue;
                }
                col_upper[col] = total_offer_mw.min(total_phys_mw) / input.base;
            }

            t_reserves_dlgroup.fetch_add(_t_rdl_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

            let _t_rzonal_t0 = std::time::Instant::now();
            for (zi, req) in ap.zonal_reqs.iter().enumerate() {
                let col = input.layout.col(t, ap.zonal_slack_offset + zi);
                col_lower[col] = 0.0;
                col_upper[col] = zonal_slack_upper_bound_mw(input, t, req) / input.base;
            }
            t_reserves_zonal.fetch_add(_t_rzonal_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        t_reserves.fetch_add(_t_reserves_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

        if input.use_plc {
            for j in 0..n_gen {
                for k in 0..input.n_bp {
                    let lam_idx = input
                        .layout
                        .col(t, input.layout.plc_lambda + j * input.n_bp + k);
                    col_lower[lam_idx] = 0.0;
                    col_upper[lam_idx] = 1.0;
                }
                for m in 0..input.n_sbp {
                    let sbp_idx = input
                        .layout
                        .col(t, input.layout.plc_sos2_binary + j * input.n_sbp + m);
                    col_lower[sbp_idx] = 0.0;
                    col_upper[sbp_idx] = 1.0;
                    if !relax_auxiliary_binaries {
                        integrality[sbp_idx] = VariableDomain::Binary;
                    }
                }
            }
        }

        for &(s, _, gi) in input.storage_gen_local {
            let g = &input.network.generators[gi];
            let sto = g
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            let ch_idx = input.layout.storage_charge_col(t, s);
            let dis_idx = input.layout.storage_discharge_col(t, s);
            let soc_idx = input.layout.storage_soc_col(t, s);

            if sto.dispatch_mode == StorageDispatchMode::SelfSchedule {
                let net = input
                    .spec
                    .storage_self_schedules
                    .and_then(|schedules| schedules.get(&gi))
                    .and_then(|periods| periods.get(t).copied())
                    .unwrap_or(sto.self_schedule_mw);
                let dis_val = net.max(0.0).min(g.discharge_mw_max());
                let ch_val = (-net).max(0.0).min(g.charge_mw_max());
                col_lower[dis_idx] = dis_val;
                col_upper[dis_idx] = dis_val;
                col_lower[ch_idx] = ch_val;
                col_upper[ch_idx] = ch_val;
            } else {
                col_lower[ch_idx] = 0.0;
                col_upper[ch_idx] = g.charge_mw_max();
                col_lower[dis_idx] = 0.0;
                col_upper[dis_idx] = g.discharge_mw_max();
            }

            col_lower[soc_idx] = sto.soc_min_mwh;
            col_upper[soc_idx] = sto.soc_max_mwh;
        }

        for k in 0..input.n_sto_dis_epi {
            let idx = input.layout.col(t, input.layout.dispatch.sto_epi_dis + k);
            col_lower[idx] = f64::NEG_INFINITY;
            col_upper[idx] = f64::INFINITY;
        }
        for k in 0..input.n_sto_ch_epi {
            let idx = input.layout.col(t, input.layout.dispatch.sto_epi_ch + k);
            col_lower[idx] = f64::NEG_INFINITY;
            col_upper[idx] = f64::INFINITY;
        }

        if has_hvdc {
            for (k, hvdc) in input.spec.hvdc_links.iter().enumerate() {
                if hvdc.is_banded() {
                    for (b, band) in hvdc.bands.iter().enumerate() {
                        let idx = input.layout.col(
                            t,
                            input.layout.dispatch.hvdc + input.hvdc_band_offsets[k] + b,
                        );
                        col_lower[idx] = band.p_min_mw / input.base;
                        col_upper[idx] = band.p_max_mw / input.base;
                    }
                } else {
                    let idx = input
                        .layout
                        .col(t, input.layout.dispatch.hvdc + input.hvdc_band_offsets[k]);
                    if let Some(fixed_mw) = input.spec.fixed_hvdc_dispatch_mw_at(t, k) {
                        let fixed_pu = fixed_mw / input.base;
                        col_lower[idx] = fixed_pu;
                        col_upper[idx] = fixed_pu;
                    } else {
                        col_lower[idx] = hvdc.p_dc_min_mw / input.base;
                        col_upper[idx] = hvdc.p_dc_max_mw / input.base;
                    }
                }
            }
        }

        for (k, segments) in pwl_segments_t.iter().enumerate() {
            let eg_idx = input.layout.col(t, input.layout.dispatch.e_g + k);
            if segments.is_some() {
                col_lower[eg_idx] = f64::NEG_INFINITY;
                col_upper[eg_idx] = f64::INFINITY;
            } else {
                col_lower[eg_idx] = 0.0;
                col_upper[eg_idx] = 0.0;
            }
        }

        let _t_dl_t0 = std::time::Instant::now();
        if has_dl {
            for (k, dl) in input.dl_list.iter().enumerate() {
                let (_, p_max, _, _, _, _) = crate::common::costs::resolve_dl_for_period_from_spec(
                    input.dl_orig_idx[k],
                    t,
                    dl,
                    input.spec,
                );
                let idx = input.layout.col(t, input.layout.dispatch.dl + k);
                col_lower[idx] = dl.p_min_pu;
                col_upper[idx] = p_max;
            }
        }
        t_dl_bounds.fetch_add(_t_dl_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

        for (k, &bi) in input.active_vbids.iter().enumerate() {
            let vb = &input.spec.virtual_bids[bi];
            let idx = input.layout.col(t, input.layout.dispatch.vbid + k);
            col_lower[idx] = 0.0;
            col_upper[idx] = if t == vb.period {
                vb.mw_limit / input.base
            } else {
                0.0
            };
        }

        // Pin per-bus & per-segment power-balance slacks to zero when the
        // diagnostic knob is set (bus-balance rows become firm). The
        // presolver strips the pinned cols before the MIP sees them. The
        // pb_agg coupling rows stay harmless: both sides become ≡ 0.
        //
        // When NOT firm, cap `col_upper` at a physically meaningful
        // value rather than `f64::INFINITY`. Previously the unbounded
        // cap let the LP relaxation hallucinate billions-of-MW of
        // slack on stressed networks like 6049-bus D1, producing
        // dummy objectives in the 10^14 range that Gurobi's
        // feasibility pump couldn't round to a viable integer. The
        // physical bound is:
        //   * pb_curtailment_bus[b,t] ≤ total demand at bus b
        //     (fixed_bus_withdrawals + sum DL p_max_pu at bus × base)
        //   * pb_excess_bus[b,t]      ≤ total gen pmax at bus b
        // Multiplied by `PB_SLACK_CAP_FACTOR = 2.0` to leave headroom
        // for rounding, out-of-envelope transients, etc.
        const PB_SLACK_CAP_FACTOR: f64 = 2.0;
        let firm_pb = input.spec.scuc_firm_bus_balance_slacks;
        let firm_thermal = input.spec.scuc_firm_branch_thermal_slacks;
        let skip_bus_pb = input.spec.scuc_disable_bus_power_balance;

        // Per-bus pb slack bounds are only set when the per-bus balance
        // row family is allocated. Under `scuc_disable_bus_power_balance`
        // the `pb_*` column blocks collapse to zero width and these
        // accessors are not valid.
        if !skip_bus_pb {
            // Precompute per-bus physical caps for this period.
            let net_t = &input.hourly_networks[t];
            let bus_withdrawals_mw =
                crate::common::profiles::fixed_bus_withdrawals_mw(net_t, input.spec, t);
            let mut dl_pmax_mw_by_bus: std::collections::HashMap<u32, f64> =
                std::collections::HashMap::new();
            for (k, dl) in input.dl_list.iter().enumerate() {
                let (_, p_max_pu, _, _, _, _) =
                    crate::common::costs::resolve_dl_for_period_from_spec(
                        input.dl_orig_idx[k],
                        t,
                        dl,
                        input.spec,
                    );
                *dl_pmax_mw_by_bus.entry(dl.bus).or_insert(0.0) +=
                    p_max_pu.max(0.0) * input.base;
            }
            let mut gen_pmax_mw_by_bus: std::collections::HashMap<u32, f64> =
                std::collections::HashMap::new();
            for &gi in input.gen_indices.iter() {
                let g = &net_t.generators[gi];
                *gen_pmax_mw_by_bus.entry(g.bus).or_insert(0.0) += g.pmax.max(0.0);
            }
            let mut total_curt_cap_pu = 0.0_f64;
            let mut total_excess_cap_pu = 0.0_f64;

            for bus_idx in 0..input.n_bus {
                let bus_number = input.network.buses[bus_idx].number;
                let load_mw = bus_withdrawals_mw.get(&bus_number).copied().unwrap_or(0.0);
                let dl_mw = dl_pmax_mw_by_bus.get(&bus_number).copied().unwrap_or(0.0);
                let gen_mw = gen_pmax_mw_by_bus.get(&bus_number).copied().unwrap_or(0.0);

                // Curtailment slack: bounded by total demand at this bus.
                // Floor at 1 MW pu so a bus with zero declared demand can
                // still absorb a small rounding residue — otherwise the LP
                // relaxation pegs every zero-demand bus's slack at 0 even
                // when network routing would need a tiny dispatch.
                let curt_cap_mw =
                    ((load_mw.abs() + dl_mw) * PB_SLACK_CAP_FACTOR).max(1.0);
                let curt_cap_pu = curt_cap_mw / input.base;
                total_curt_cap_pu += curt_cap_pu;

                let excess_cap_mw = (gen_mw * PB_SLACK_CAP_FACTOR).max(1.0);
                let excess_cap_pu = excess_cap_mw / input.base;
                total_excess_cap_pu += excess_cap_pu;

                let idx = input.layout.pb_curtailment_bus_col(t, bus_idx);
                col_lower[idx] = 0.0;
                col_upper[idx] = if firm_pb { 0.0 } else { curt_cap_pu };

                let idx = input.layout.pb_excess_bus_col(t, bus_idx);
                col_lower[idx] = 0.0;
                col_upper[idx] = if firm_pb { 0.0 } else { excess_cap_pu };
            }
            // Per-segment caps: tighten to the sum of bus-level caps when
            // that's smaller than the configured penalty-segment `mw_cap`.
            // The pb_agg row enforces Σ seg == Σ bus, so the seg col_upper
            // only bites when the penalty config explicitly caps lower
            // than the physical sum. Keep the config cap as a ceiling for
            // user-driven tightening.
            for (s, &(mw_cap, penalty)) in pb_penalty.curtailment.iter().enumerate() {
                let idx = input.layout.pb_curtailment_seg_col(t, s);
                col_lower[idx] = 0.0;
                let seg_cap_pu = (mw_cap / input.base).min(total_curt_cap_pu);
                col_upper[idx] = if firm_pb { 0.0 } else { seg_cap_pu };
                col_cost[idx] = penalty * input.base * dt_h;
            }
            for (s, &(mw_cap, penalty)) in pb_penalty.excess.iter().enumerate() {
                let idx = input.layout.pb_excess_seg_col(t, s);
                col_lower[idx] = 0.0;
                let seg_cap_pu = (mw_cap / input.base).min(total_excess_cap_pu);
                col_upper[idx] = if firm_pb { 0.0 } else { seg_cap_pu };
                col_cost[idx] = penalty * input.base * dt_h;
            }
        }

        // Thermal-slack `col_upper` is capped at `SLACK_CAP_FACTOR ×
        // branch_rating_pu` rather than `f64::INFINITY`. Motivation:
        // with an unbounded cap, the LP relaxation can dump arbitrary
        // flow into the slack columns — empirically this drove a
        // dual bound of -$1.7×10¹⁴ on 1576-bus D1 (vs a physical
        // incumbent around -$89M), preventing Gurobi from ever closing
        // gap. Capping at `10 × rating` caps the hallucinated slack at
        // (cap × penalty × rating) per branch while leaving plenty of
        // headroom for any realistic overflow the MIP might legitimately
        // want to absorb (10× rated flow is already far beyond any
        // physical violation our N-1 security loop would tolerate).
        const SLACK_CAP_FACTOR: f64 = 10.0;
        let _t_branch_t0 = std::time::Instant::now();
        for row_idx in 0..input.n_branch_flow {
            let branch_idx = input.constrained_branches[row_idx];
            let branch = &input.hourly_networks[t].branches[branch_idx];
            let fmax_pu = (branch.rating_a_mva.max(input.spec.min_rate_a)) / input.base;
            let slack_cap_pu = SLACK_CAP_FACTOR * fmax_pu;

            let lower_idx = input.layout.branch_lower_slack_col(t, row_idx);
            col_lower[lower_idx] = 0.0;
            col_upper[lower_idx] = if firm_thermal { 0.0 } else { slack_cap_pu };
            col_cost[lower_idx] =
                input.spec.thermal_penalty_curve.marginal_cost_at(0.0) * input.base * dt_h;

            let upper_idx = input.layout.branch_upper_slack_col(t, row_idx);
            col_lower[upper_idx] = 0.0;
            col_upper[upper_idx] = if firm_thermal { 0.0 } else { slack_cap_pu };
            col_cost[upper_idx] =
                input.spec.thermal_penalty_curve.marginal_cost_at(0.0) * input.base * dt_h;
        }
        t_branch_slacks.fetch_add(_t_branch_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let _t_flowgate_t0 = std::time::Instant::now();
        for row_idx in 0..input.n_fg_rows {
            // Look up the source flowgate to decide whether this
            // (row, period, side) triple should actually allocate a
            // free slack column or be pinned to zero. Period gating
            // comes from `limit_mw_active_period` (compact N-1 cuts);
            // side gating from `breach_sides` (iterative screener
            // preserves the flow-sign). Pinned columns survive to
            // surge's lp-reduce presolve and get dropped before HiGHS
            // sees them.
            let fg_idx = input.fg_rows.get(row_idx).copied();
            let fg = fg_idx.and_then(|idx| input.network.flowgates.get(idx));
            let period_matches = match fg.and_then(|f| f.limit_mw_active_period) {
                Some(p) => t == p as usize,
                None => true,
            };
            let allocate_lower = period_matches
                && fg.map(|f| f.breach_sides.allocates_lower_slack()).unwrap_or(true);
            let allocate_upper = period_matches
                && fg.map(|f| f.breach_sides.allocates_upper_slack()).unwrap_or(true);

            let is_explicit_ctg_flowgate = input
                .explicit_contingency
                .and_then(|plan| plan.flowgate_row_cases.get(row_idx))
                .copied()
                .flatten()
                .is_some();
            let slack_cost = if is_explicit_ctg_flowgate {
                0.0
            } else {
                input.spec.thermal_penalty_curve.marginal_cost_at(0.0) * input.base * dt_h
            };

            if let Some(lower_idx) = input.layout.flowgate_lower_slack_col_opt(t, row_idx) {
                if allocate_lower {
                    col_lower[lower_idx] = 0.0;
                    col_upper[lower_idx] = f64::INFINITY;
                    col_cost[lower_idx] = slack_cost;
                } else {
                    col_lower[lower_idx] = 0.0;
                    col_upper[lower_idx] = 0.0;
                    col_cost[lower_idx] = 0.0;
                }
            }

            if let Some(upper_idx) = input.layout.flowgate_upper_slack_col_opt(t, row_idx) {
                if allocate_upper {
                    col_lower[upper_idx] = 0.0;
                    col_upper[upper_idx] = f64::INFINITY;
                    col_cost[upper_idx] = slack_cost;
                } else {
                    col_lower[upper_idx] = 0.0;
                    col_upper[upper_idx] = 0.0;
                    col_cost[upper_idx] = 0.0;
                }
            }
        }
        t_flowgate_slacks.fetch_add(_t_flowgate_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

        for row_idx in 0..input.n_iface_rows {
            let lower_idx = input.layout.interface_lower_slack_col(t, row_idx);
            col_lower[lower_idx] = 0.0;
            col_upper[lower_idx] = f64::INFINITY;
            col_cost[lower_idx] =
                input.spec.thermal_penalty_curve.marginal_cost_at(0.0) * input.base * dt_h;

            let upper_idx = input.layout.interface_upper_slack_col(t, row_idx);
            col_lower[upper_idx] = 0.0;
            col_upper[upper_idx] = f64::INFINITY;
            col_cost[upper_idx] =
                input.spec.thermal_penalty_curve.marginal_cost_at(0.0) * input.base * dt_h;
        }

        for j in 0..n_gen {
            let hr_idx = input.layout.headroom_slack_col(t, j);
            col_lower[hr_idx] = 0.0;
            col_upper[hr_idx] = f64::MAX / input.base;
            col_cost[hr_idx] = 1e6 * input.base * dt_h;

            let fr_idx = input.layout.footroom_slack_col(t, j);
            col_lower[fr_idx] = 0.0;
            col_upper[fr_idx] = f64::MAX / input.base;
            col_cost[fr_idx] = 1e6 * input.base * dt_h;

            // Ramp slack columns implement ramp inequalities as soft
            // constraints by default. When `spec.ramp_constraints_hard`
            // is set, the slack columns are pinned to zero so the ramp
            // inequality rows behave as hard constraints. The columns
            // themselves stay allocated so the LP layout indices remain
            // stable across both modes.
            let ramp_slack_upper = if input.spec.ramp_constraints_hard {
                0.0
            } else {
                f64::INFINITY
            };
            let ramp_slack_cost = if input.spec.ramp_constraints_hard {
                0.0
            } else {
                input.spec.ramp_penalty_curve.marginal_cost_at(0.0) * input.base * dt_h
            };

            let ramp_up_idx = input.layout.ramp_up_slack_col(t, j);
            col_lower[ramp_up_idx] = 0.0;
            col_upper[ramp_up_idx] = ramp_slack_upper;
            col_cost[ramp_up_idx] = ramp_slack_cost;

            let ramp_down_idx = input.layout.ramp_down_slack_col(t, j);
            col_lower[ramp_down_idx] = 0.0;
            col_upper[ramp_down_idx] = ramp_slack_upper;
            col_cost[ramp_down_idx] = ramp_slack_cost;
        }

        // Angle difference slack bounds.
        for row_idx in 0..input.layout.n_angle_diff_rows {
            let lower_idx = input.layout.angle_diff_lower_slack_col(t, row_idx);
            col_lower[lower_idx] = 0.0;
            col_upper[lower_idx] = f64::INFINITY;

            let upper_idx = input.layout.angle_diff_upper_slack_col(t, row_idx);
            col_lower[upper_idx] = 0.0;
            col_upper[upper_idx] = f64::INFINITY;
        }

        // Branch on/off binaries `u^on_jt`, `u^su_jt`, `u^sd_jt` per
        // AC branch per period. When `allow_branch_switching` is
        // `true` the columns are free in `{0, 1}` (per-branch, only
        // for switchable branches that carry transition costs) and
        // the security loop adds connectivity cuts on top. When
        // `false` these columns are absent from the LP entirely —
        // the branch state is read directly from the network's
        // static `in_service` flag at row-build time — and the pin
        // pass below is skipped.
        let allow_switching = input.spec.allow_branch_switching;
        if allow_switching {
            for branch_local_idx in 0..input.network.branches.len() {
                let branch = &input.network.branches[branch_local_idx];
                let initial_on = if branch.in_service { 1.0 } else { 0.0 };
                let bc_idx = input.layout.branch_commitment_col(t, branch_local_idx);
                let bs_idx = input.layout.branch_startup_col(t, branch_local_idx);
                let bd_idx = input.layout.branch_shutdown_col(t, branch_local_idx);
                // Per-branch switching: only free binaries for branches that
                // carry non-zero transition costs (connection/disconnection).
                // Branches without costs are pinned to their static in_service
                // state even when the global allow_branch_switching flag is set.
                if branch.is_switchable() && !relax_auxiliary_binaries {
                    col_lower[bc_idx] = 0.0;
                    col_upper[bc_idx] = 1.0;
                    col_lower[bs_idx] = 0.0;
                    col_upper[bs_idx] = 1.0;
                    col_lower[bd_idx] = 0.0;
                    col_upper[bd_idx] = 1.0;
                    integrality[bc_idx] = VariableDomain::Binary;
                    integrality[bs_idx] = VariableDomain::Binary;
                    integrality[bd_idx] = VariableDomain::Binary;
                } else {
                    // Pin to initial state. The start/stop columns stay at 0
                    // since the branch never transitions.
                    col_lower[bc_idx] = initial_on;
                    col_upper[bc_idx] = initial_on;
                    col_lower[bs_idx] = 0.0;
                    col_upper[bs_idx] = 0.0;
                    col_lower[bd_idx] = 0.0;
                    col_upper[bd_idx] = 0.0;
                }
                // Branch transitions carry per-event startup/shutdown
                // costs from the network's `connection_cost` /
                // `disconnection_cost`. These appear in the objective
                // as fixed-event costs (`$/event`, no `dt` scaling).
                // `bc_idx` stays at zero cost — no `c^on` analogue
                // for the on-status column itself.
                col_cost[bc_idx] = 0.0;
                col_cost[bs_idx] = branch.cost_startup;
                col_cost[bd_idx] = branch.cost_shutdown;

                // Switchable-branch flow variable `pf_l`. Bound by
                // `±fmax` in pu so the LP respects the thermal
                // envelope; the Big-M flow definition rows in
                // `build_branch_flow_definition_rows` tie `pf_l` to
                // `b·Δθ` when the branch is on and force it to zero
                // via `pf_l ≤ fmax·u^on` / `pf_l ≥ -fmax·u^on` when
                // it's off.
                let pf_idx = input.layout.branch_flow_col(t, branch_local_idx);
                if branch.is_switchable() {
                    let fmax_pu = branch.rating_a_mva.max(0.0) / input.base;
                    col_lower[pf_idx] = -fmax_pu;
                    col_upper[pf_idx] = fmax_pu;
                } else {
                    // Non-switchable branch: pin pf_l to zero so the
                    // Big-M rows reduce to trivially satisfied no-ops.
                    col_lower[pf_idx] = 0.0;
                    col_upper[pf_idx] = 0.0;
                }
                col_cost[pf_idx] = 0.0;
            }
        }

        if input.is_block_mode {
            let mut flat_idx = 0;
            for blocks in input.gen_blocks {
                for block in blocks {
                    let idx = input.layout.col(t, input.layout.dispatch.block + flat_idx);
                    col_lower[idx] = 0.0;
                    col_upper[idx] = block.width_mw() / input.base;
                    flat_idx += 1;
                }
            }
        }

        if input.has_reg_products {
            for j in 0..n_gen {
                let idx = input.layout.col(t, input.layout.regulation_mode + j);
                col_lower[idx] = 0.0;
                col_upper[idx] = 1.0;
                if !relax_auxiliary_binaries {
                    integrality[idx] = VariableDomain::Binary;
                }
            }
        }

        if input.has_per_block_reserves {
            for (pi, ap) in input.reserve_layout.products.iter().enumerate() {
                let deploy_min = ap.product.deploy_secs / 60.0;
                for (j, blocks) in input.gen_blocks.iter().enumerate() {
                    for (i, block) in blocks.iter().enumerate() {
                        let idx = input.layout.col(
                            t,
                            input.layout.dispatch.block_reserve
                                + pi * input.n_block_vars_per_hour
                                + input.gen_block_start[j]
                                + i,
                        );
                        col_lower[idx] = 0.0;
                        let width_mw = block.width_mw();
                        let ramp_mw = match ap.product.direction {
                            ReserveDirection::Up => {
                                if ap.product.id.starts_with("reg") {
                                    block.reg_ramp_up_mw_per_min
                                } else {
                                    block.ramp_up_mw_per_min
                                }
                            }
                            ReserveDirection::Down => {
                                if ap.product.id.starts_with("reg") {
                                    block.reg_ramp_dn_mw_per_min
                                } else {
                                    block.ramp_dn_mw_per_min
                                }
                            }
                        } * deploy_min;
                        col_upper[idx] = width_mw.min(ramp_mw) / input.base;
                    }
                }
            }
        }

        if has_foz {
            for group in input.foz_groups {
                for k in 0..group.n_segments {
                    let idx = input
                        .layout
                        .col(t, input.layout.foz_delta + group.delta_local_off + k);
                    col_lower[idx] = 0.0;
                    col_upper[idx] = 1.0;
                    if !relax_auxiliary_binaries {
                        integrality[idx] = VariableDomain::Binary;
                    }
                }
                for (z, &max_transit) in group.max_transit.iter().enumerate() {
                    let idx = input
                        .layout
                        .col(t, input.layout.foz_phi + group.phi_local_off + z);
                    col_lower[idx] = 0.0;
                    col_upper[idx] = if max_transit == 0 { 0.0 } else { 1.0 };
                    if !relax_auxiliary_binaries {
                        integrality[idx] = VariableDomain::Binary;
                    }

                    let rho_idx = input
                        .layout
                        .col(t, input.layout.foz_rho + group.rho_local_off + z);
                    col_lower[rho_idx] = 0.0;
                    col_upper[rho_idx] = 1.0;
                    if !relax_auxiliary_binaries {
                        integrality[rho_idx] = VariableDomain::Binary;
                    }
                }
            }
        }

        if has_ph_mode {
            for info in input.ph_mode_infos {
                let mg_idx = input
                    .layout
                    .col(t, input.layout.ph_mode + info.m_gen_local_off);
                col_lower[mg_idx] = 0.0;
                col_upper[mg_idx] = 1.0;
                if !relax_auxiliary_binaries {
                    integrality[mg_idx] = VariableDomain::Binary;
                }

                let mp_idx = input
                    .layout
                    .col(t, input.layout.ph_mode + info.m_pump_local_off);
                col_lower[mp_idx] = 0.0;
                col_upper[mp_idx] = 1.0;
                if !relax_auxiliary_binaries {
                    integrality[mp_idx] = VariableDomain::Binary;
                }
            }
        }
    }

    if !input.cc_plants.is_empty() {
        for plant in input.cc_plants {
            for c in 0..plant.n_configs {
                for t in 0..input.n_hours {
                    let z_idx = cc_z_idx(input.cc_var_base, plant, c, t, input.n_hours);
                    col_lower[z_idx] = 0.0;
                    col_upper[z_idx] = 1.0;
                    if !relax_auxiliary_binaries {
                        integrality[z_idx] = VariableDomain::Binary;
                    }

                    let yu_idx = cc_yup_idx(input.cc_var_base, plant, c, t, input.n_hours);
                    col_lower[yu_idx] = 0.0;
                    col_upper[yu_idx] = 1.0;
                    if !relax_auxiliary_binaries {
                        integrality[yu_idx] = VariableDomain::Binary;
                    }

                    let yd_idx = cc_ydn_idx(input.cc_var_base, plant, c, t, input.n_hours);
                    col_lower[yd_idx] = 0.0;
                    col_upper[yd_idx] = 1.0;
                    if !relax_auxiliary_binaries {
                        integrality[yd_idx] = VariableDomain::Binary;
                    }
                }
            }

            if let Some(c) = plant.initial_active_config {
                for t in 0..plant.initial_config_force_periods.min(input.n_hours) {
                    let z_idx = cc_z_idx(input.cc_var_base, plant, c, t, input.n_hours);
                    col_lower[z_idx] = 1.0;
                }
            }

            for (entry_idx, &gen_j) in plant.pgcc_gen_j.iter().enumerate() {
                let gi = input.gen_indices[gen_j];
                for t in 0..input.n_hours {
                    let pmax_pu = input.hourly_networks[t].generators[gi].pmax / input.base;
                    let idx = cc_pgcc_idx(input.cc_var_base, plant, entry_idx, t, input.n_hours);
                    col_lower[idx] = 0.0;
                    col_upper[idx] = pmax_pu;
                }
            }

            for tr_idx in 0..plant.n_transition_pairs {
                for t in 0..input.n_hours {
                    let idx = cc_ytrans_idx(input.cc_var_base, plant, tr_idx, t, input.n_hours);
                    col_lower[idx] = 0.0;
                    col_upper[idx] = 1.0;
                    if !relax_auxiliary_binaries {
                        integrality[idx] = VariableDomain::Binary;
                    }
                }
            }
        }
    }
    }); // closes per-period parallel body
    tracing::info!(
        stage = "build_bounds.main_per_period_loop",
        secs = _bounds_fn_t0.elapsed().as_secs_f64(),
        gen_bounds_secs = t_gen_bounds.load(Ordering::Relaxed) as f64 / 1e9,
        reserves_secs = t_reserves.load(Ordering::Relaxed) as f64 / 1e9,
        reserves_gen_secs = t_reserves_gen.load(Ordering::Relaxed) as f64 / 1e9,
        reserves_dlgroup_secs = t_reserves_dlgroup.load(Ordering::Relaxed) as f64 / 1e9,
        reserves_zonal_secs = t_reserves_zonal.load(Ordering::Relaxed) as f64 / 1e9,
        dl_bounds_secs = t_dl_bounds.load(Ordering::Relaxed) as f64 / 1e9,
        branch_slacks_secs = t_branch_slacks.load(Ordering::Relaxed) as f64 / 1e9,
        flowgate_slacks_secs = t_flowgate_slacks.load(Ordering::Relaxed) as f64 / 1e9,
        gen_commit_secs = t_gen_commit.load(Ordering::Relaxed) as f64 / 1e9,
        other_secs = t_other.load(Ordering::Relaxed) as f64 / 1e9,
        "SCUC bounds timing"
    );
    let _bounds_post_t0 = Instant::now();

    for (j, &gi) in input.gen_indices.iter().enumerate() {
        if input.cc_member_gen.get(j).copied().unwrap_or(false) {
            continue;
        }
        let g = &input.network.generators[gi];
        let initially_on = input.spec.initial_commitment_at(j).unwrap_or(true);
        let h_on_hr = input.spec.initial_online_hours_at(j).unwrap_or(0.0);

        // Zero-pmax pre-fix for renewable-style units that start OFF.
        //
        // A non-storage generator with `initial_commitment_at(j) =
        // Some(false)` and `g_hourly.pmax = 0` at a period t cannot be
        // committed at that period — it has no physical capacity and
        // its initial state is off, so the LP/MIP has no feasible
        // reason to turn it on. Pin `u[t] = 0` (via col_upper) so
        // Gurobi doesn't branch on a binary whose value is already
        // determined. The downstream transition loop then pins
        // `v[t] = 0` and `w[t] = 0` automatically.
        //
        // Restricted to `initial = Some(false)` because the
        // commitment-transition equation at hour 0 uses the initial
        // commitment as the row RHS — pinning `u[0] = 0` when the
        // initial state is ON (or unknown, which defaults to ON in
        // the row builder) leaves no feasible shutdown path and the
        // LP is infeasible. Storage units are excluded because
        // `pmax = 0` is a legitimate discharge-disabled-only mode.
        if matches!(input.spec.initial_commitment_at(j), Some(false)) && !g.is_storage() {
            for t in 0..input.n_hours {
                let pmax_pu = input.hourly_networks[t].generators[gi].pmax / input.base;
                if pmax_pu <= 1e-9 {
                    let u_idx = input.layout.commitment_col(t, j);
                    col_upper[u_idx] = 0.0;
                }
            }
        }

        if initially_on && h_on_hr > 0.0 {
            let mut_hr = g
                .commitment
                .as_ref()
                .and_then(|c| c.min_up_time_hr)
                .unwrap_or(1.0);
            if h_on_hr < mut_hr {
                let remaining = input.spec.hours_to_periods_ceil_from(0, mut_hr - h_on_hr);
                // Truncate the forced-on window at the first period
                // where the unit is physically unable to be committed:
                // either the derate profile forces it offline, or the
                // hourly pmax drops to zero (renewable curtailment,
                // outage expressed via capacity profile). Letting
                // these conflict with a pinned u=1 creates an
                // infeasible LP relaxation that wastes solver effort.
                let active_end = (0..remaining.min(input.n_hours))
                    .find(|&t| {
                        if is_forced_offline(input.spec, input.network, gi, t) {
                            return true;
                        }
                        let g_hourly = &input.hourly_networks[t].generators[gi];
                        !g.is_storage() && (g_hourly.pmax / input.base) <= 1e-9
                    })
                    .unwrap_or(remaining.min(input.n_hours));
                for t in 0..active_end {
                    let u_idx = input.layout.commitment_col(t, j);
                    col_lower[u_idx] = 1.0;
                }
            }
        } else if !initially_on {
            let mdt_hr = g
                .commitment
                .as_ref()
                .and_then(|c| c.min_down_time_hr)
                .unwrap_or(1.0);
            let h_off_hr = input.spec.initial_offline_hours_at(j).unwrap_or(0.0);
            if h_off_hr > 0.0 && h_off_hr < mdt_hr {
                let remaining = input.spec.hours_to_periods_ceil_from(0, mdt_hr - h_off_hr);
                for t in 0..remaining.min(input.n_hours) {
                    let u_idx = input.layout.commitment_col(t, j);
                    col_upper[u_idx] = 0.0;
                }
            }
        }

        // Zero-no-load-cost must-run bounds pin (P1g refined). When a
        // generator's no-load cost is zero and `pmin <= 0` across every
        // period (and the usual non-storage / non-cc / non-quick-start
        // guardrails hold), committing the unit adds no cost to the
        // objective but strictly increases dispatch flexibility: any
        // energy dispatch the LP would have wanted becomes reachable
        // without paying a commit fee. Pin `u[t]=1` for a contiguous
        // run starting at t=0 and stopping at the first must-off
        // trigger (derate=0 or hourly pmax=0 on non-storage).
        //
        // Split by initial state because the startup-cost check is
        // needed only when the unit actually starts up:
        //   * Initially ON — `v` stays zero across the pinned run, so
        //     no startup cost is incurred regardless of tier pricing.
        //   * Initially OFF — `v[0]=1` fires on the first pinned
        //     period; require every tier cost to be zero so the
        //     startup comes in for free.
        //
        // Excluded cases:
        //   * `g.is_must_run()` — handled by P1h (redundant pin would
        //     fire anyway but we'd double-count the integer-breakdown
        //     diagnostic, so gate against it).
        //   * `g.is_storage()` — storage always goes through the
        //     dedicated storage rows; its u is already pinned via P1h's
        //     static_must_run path.
        //   * `g.quick_start` — quick-start units qualify for
        //     OfflineQuickStart reserves that require u=0; pinning u=1
        //     would trade away that reserve headroom silently.
        //   * Any `CommitmentParams::max_up_time_hr` — running for the
        //     whole horizon could violate it.
        //   * Combined-cycle member (handled by cc_member_gen skip at
        //     the top of this loop).
        let pinnable_zero_cost = !g.is_must_run()
            && !g.is_storage()
            && !g.quick_start
            && g.commitment
                .as_ref()
                .and_then(|c| c.max_up_time_hr)
                .is_none()
            && unit_has_zero_no_load_and_nonpositive_pmin(
                input.spec,
                input.network,
                input.hourly_networks,
                input.base,
                gi,
                input.n_hours,
            )
            && (initially_on
                || unit_has_zero_startup(input.spec, input.network, gi, input.n_hours));
        if pinnable_zero_cost {
            for t in 0..input.n_hours {
                if period_is_must_off(
                    input.spec,
                    input.network,
                    input.hourly_networks,
                    input.base,
                    gi,
                    t,
                ) {
                    // Stop the contiguous run: the first must-off
                    // period and everything after it stays at the
                    // LP's discretion (the bounds at that period are
                    // already pinned to u=0 or the row family handles
                    // it, and subsequent periods can take any path).
                    break;
                }
                let u_idx = input.layout.commitment_col(t, j);
                if col_upper[u_idx] < 0.5 {
                    // Earlier rule pinned off — consistency guard.
                    break;
                }
                col_lower[u_idx] = 1.0;
            }
        }

        // Must-run bounds pin (P1h). The SCUC row family at
        // `scuc::rows::build_commitment_policy_rows` enforces `u=1`
        // via a row constraint for three cases:
        //
        //   1. `generator.is_must_run()` — explicit CommitmentStatus::MustRun.
        //   2. Storage units (forced committed so their charge/discharge
        //      variables remain meaningful).
        //   3. `must_run_units` spec membership (e.g. reactive-support
        //      must-runs promoted from the market adapter).
        //   4. `CommitmentMode::Additional { da_commitment, .. }`
        //      entries — reliability-commitment must-run floors.
        //
        // Converting each of those to a bounds pin lets the downstream
        // transition loop cascade `v[t]=0, w[t]=0` pins (and `d[t,k]=0`
        // startup-tier pins), and Gurobi's presolve drops the now-
        // vacuous row as a trivial bound-implied constraint. The row
        // family still emits the same count of rows so the row-count
        // accounting stays aligned with `commitment_policy_rows`; the
        // rows that matched a bounds pin collapse to
        // `[-BIG_M, BIG_M]` with no triplet at row-build time via the
        // same `is_forced_offline_hour` escape hatch the existing path
        // uses. (Rows for `!forced_offline && !already_pinned` still
        // emit normally, preserving coverage.)
        let static_must_run = g.is_must_run() || g.is_storage();
        let spec_must_run_ext = input
            .spec
            .must_run_units
            .as_ref()
            .is_some_and(|s| s.contains(j));
        let has_da_cmt = matches!(input.spec.commitment, CommitmentMode::Additional { .. });
        if static_must_run || spec_must_run_ext || has_da_cmt {
            for t in 0..input.n_hours {
                let must_on = static_must_run
                    || spec_must_run_ext
                    || matches!(
                        input.spec.commitment,
                        CommitmentMode::Additional { da_commitment, .. }
                            if da_commitment
                                .get(t)
                                .and_then(|p| p.get(j))
                                .copied()
                                .unwrap_or(false)
                    );
                if !must_on {
                    continue;
                }
                // Forced-offline (derate=0 or hourly pmax=0 on a non-
                // storage unit) overrides must-run — the existing row
                // already relaxes those periods, and pinning u=1 there
                // would force an infeasible LP.
                if is_forced_offline(input.spec, input.network, gi, t) {
                    continue;
                }
                if !g.is_storage() {
                    let pmax_pu = input.hourly_networks[t].generators[gi].pmax / input.base;
                    if pmax_pu <= 1e-9 {
                        continue;
                    }
                }
                let u_idx = input.layout.commitment_col(t, j);
                // If an earlier pass pinned this u to 0 (startup-
                // infeasible-at-horizon-start, min-down carryover, or
                // the physical-pmax-zero P1a case), do not flip it —
                // the conflict means the row-level enforcement was
                // already the binding constraint and the bounds must
                // stay consistent.
                if col_upper[u_idx] < 0.5 {
                    continue;
                }
                col_lower[u_idx] = 1.0;
            }
        }

        if trace_commitment_bounds
            && trace_commitment_unit
                .as_deref()
                .is_none_or(|unit_id| unit_id == g.id)
        {
            let u0_idx = input.layout.commitment_col(0, j);
            log_scuc_bounds_trace(format!(
                "scuc_bounds_trace_final unit={} initial_on={} h_on_hr={:.3} h_off_hr={:?} u0_final=[{:.1},{:.1}]",
                g.id,
                initially_on,
                h_on_hr,
                input.spec.initial_offline_hours_at(j),
                col_lower[u0_idx],
                col_upper[u0_idx],
            ));
        }
    }

    for (i, info) in input.dl_activation_infos.iter().enumerate() {
        for t in 0..input.n_hours {
            let idx = dl_act_idx(input.dl_act_var_base, i, t, input.n_hours);
            col_lower[idx] = 0.0;
            col_upper[idx] = 1.0;
            if !relax_auxiliary_binaries {
                integrality[idx] = VariableDomain::Binary;
            }
        }
        for t in 0..info.n_notify.min(input.n_hours) {
            let idx = dl_act_idx(input.dl_act_var_base, i, t, input.n_hours);
            col_upper[idx] = 0.0;
        }
    }

    for rb_idx in 0..input.n_dl_rebound {
        for t in 0..input.n_hours {
            let idx = dl_rebound_idx(input.dl_rebound_var_base, rb_idx, t, input.n_hours);
            col_lower[idx] = 0.0;
            col_upper[idx] = 1e30;
        }
    }

    // Multi-interval energy window slack columns. Each
    // (window, direction) pair gets one non-negative slack column.
    // The cost is set from `spec.energy_window_violation_per_puh ×
    // base` so the LP prices violations consistently. When the spec
    // coefficient is 0.0 the slack is free — the LP will absorb any
    // energy window violation at no cost, a strict relaxation of the
    // hard-constraint behaviour.
    let energy_window_slack_cost = if input.spec.energy_window_constraints_hard {
        0.0
    } else {
        input.spec.energy_window_violation_per_puh * input.base
    };
    for slack_idx in 0..input.energy_window_slack_kinds.len() {
        let col = input.energy_window_slack_base + slack_idx;
        col_lower[col] = 0.0;
        col_upper[col] = if input.spec.energy_window_constraints_hard {
            0.0
        } else {
            f64::INFINITY
        };
        input.col_cost[col] = energy_window_slack_cost;
    }

    if let Some(explicit_ctg) = input.explicit_contingency {
        for case in &explicit_ctg.cases {
            col_lower[case.penalty_col] = 0.0;
            col_upper[case.penalty_col] = f64::INFINITY;
            input.col_cost[case.penalty_col] = 0.0;
        }
        for period in &explicit_ctg.periods {
            if period.case_indices.is_empty() {
                col_lower[period.worst_case_col] = 0.0;
                col_upper[period.worst_case_col] = 0.0;
                col_lower[period.avg_case_col] = 0.0;
                col_upper[period.avg_case_col] = 0.0;
                input.col_cost[period.worst_case_col] = 0.0;
                input.col_cost[period.avg_case_col] = 0.0;
            } else {
                col_lower[period.worst_case_col] = 0.0;
                col_upper[period.worst_case_col] = f64::INFINITY;
                col_lower[period.avg_case_col] = 0.0;
                col_upper[period.avg_case_col] = f64::INFINITY;
            }
        }
    }

    // Option C: cut-slack columns are non-negative with no per-slack
    // cost. Pricing of contingency overload flows through the
    // `case.penalty_col` aggregation emitted by
    // `build_explicit_contingency_objective_rows`, exactly like the
    // Flowgate path handled its contingency slacks (`is_explicit_ctg_flowgate`
    // branch at the per-hour slack loop above sets cost 0 for the
    // same reason).
    for k in 0..input.n_cut_rows {
        let lower_col = input.cut_slack_lower_base + k;
        col_lower[lower_col] = 0.0;
        col_upper[lower_col] = f64::INFINITY;
        input.col_cost[lower_col] = 0.0;

        let upper_col = input.cut_slack_upper_base + k;
        col_lower[upper_col] = 0.0;
        col_upper[upper_col] = f64::INFINITY;
        input.col_cost[upper_col] = 0.0;
    }

    // When commitment states are already fixed by initial conditions, min-up/min-down
    // carryover, forced outages, or additional commitment prefixes, pin the
    // corresponding startup/shutdown binaries too so the MIP does not branch on
    // transitions that are already logically determined.
    for (j, _) in input.gen_indices.iter().enumerate() {
        let mut prior_commitment = Some(input.spec.initial_commitment_at(j).unwrap_or(true));
        for t in 0..input.n_hours {
            let u_idx = input.layout.commitment_col(t, j);
            let v_idx = input.layout.startup_col(t, j);
            let w_idx = input.layout.shutdown_col(t, j);
            let current_commitment = fixed_binary_value(col_lower[u_idx], col_upper[u_idx]);

            if let (Some(prev_on), Some(curr_on)) = (prior_commitment, current_commitment) {
                match (prev_on, curr_on) {
                    (false, false) | (true, true) => {
                        pin_binary_bounds(&mut col_lower, &mut col_upper, v_idx, false);
                        pin_binary_bounds(&mut col_lower, &mut col_upper, w_idx, false);
                    }
                    (false, true) => {
                        pin_binary_bounds(&mut col_lower, &mut col_upper, v_idx, true);
                        pin_binary_bounds(&mut col_lower, &mut col_upper, w_idx, false);
                    }
                    (true, false) => {
                        pin_binary_bounds(&mut col_lower, &mut col_upper, v_idx, false);
                        pin_binary_bounds(&mut col_lower, &mut col_upper, w_idx, true);
                    }
                }
            }

            if fixed_binary_value(col_lower[v_idx], col_upper[v_idx]) == Some(false) {
                for k in 0..input.startup_tier_capacity[j] {
                    let d_idx = input
                        .layout
                        .col(t, input.layout.startup_delta + input.delta_gen_off[j] + k);
                    pin_binary_bounds(&mut col_lower, &mut col_upper, d_idx, false);
                    if trace_commitment_bounds
                        && trace_commitment_unit.as_deref().is_none_or(|unit_id| {
                            unit_id == input.network.generators[input.gen_indices[j]].id
                        })
                    {
                        log_scuc_bounds_trace(format!(
                            "startup_delta_pin_false unit={} t={} k={} v_fixed=false d_idx={} bounds=[{:.1},{:.1}]",
                            input.network.generators[input.gen_indices[j]].id,
                            t,
                            k,
                            d_idx,
                            col_lower[d_idx],
                            col_upper[d_idx],
                        ));
                    }
                }
            }

            prior_commitment = current_commitment;
        }
    }
    tracing::info!(
        stage = "build_bounds.post_hourly_loop",
        secs = _bounds_post_t0.elapsed().as_secs_f64(),
        "SCUC bounds timing"
    );

    ScucBoundsState {
        col_lower,
        col_upper,
        integrality,
    }
}
