// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Generic reserve product LP builder.
//!
//! Provides variable allocation, bounds, objective coefficients, constraint
//! triplets, row bounds, and solution extraction for an arbitrary set of
//! reserve products. Replaces the 6 hardcoded ERCOT product blocks in
//! sced.rs and scuc.rs with a single generic loop.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::market::DispatchableLoad;
use surge_network::market::PenaltyCurve;
use surge_network::market::{
    EnergyCoupling, QualificationRule, RampSharingConfig, ReserveDirection, ReserveProduct,
    SystemReserveRequirement, ZonalReserveRequirement, qualifications_can_overlap, qualifies_for,
};
use surge_network::network::Generator;
use surge_sparse::Triplet;

use crate::common::costs::resolve_dl_for_period_from_spec;
use crate::common::network::study_area_for_bus_index;
use crate::common::spec::DispatchProblemSpec;
use crate::result::{ConstraintKind, ConstraintScope};
use crate::solution::RawConstraintPeriodResult;

// ---------------------------------------------------------------------------
// Context struct — packages all data needed by reserve LP functions
// ---------------------------------------------------------------------------

/// Context for building reserve LP constraints.
///
/// Bundles references to network, generator, and storage data without
/// coupling to [`DispatchOptions`] directly.
pub struct ReserveLpCtx<'a> {
    pub spec: &'a DispatchProblemSpec<'a>,
    pub period: usize,
    pub network: &'a Network,
    pub gen_indices: &'a [usize],
    /// Per in-service generator (indexed by local j), true = committed.
    pub committed: Vec<bool>,
    pub generator_area: &'a [usize],
    pub prev_dispatch_mw: Option<&'a [f64]>,
    pub prev_dispatch_mask: Option<&'a [bool]>,
    pub dt_hours: f64,
    pub base: f64,
    pub ramp_sharing: &'a RampSharingConfig,
    /// In-service dispatchable loads that can provide reserves.
    pub dl_list: Vec<&'a DispatchableLoad>,
    /// Original dispatchable-load indices into `spec.dispatchable_loads`.
    pub dl_indices: Vec<usize>,
    /// Per in-service dispatchable load, the period-resolved `p_max_pu`.
    pub dl_pmax_pu: Vec<f64>,
    /// Area/zone assignment per DL (indexed by local k in dl_list).
    pub dl_area: Vec<usize>,
}

impl<'a> ReserveLpCtx<'a> {
    /// Build context from immutable problem data plus explicit runtime state.
    pub fn from_problem(
        network: &'a Network,
        gen_indices: &'a [usize],
        spec: &'a DispatchProblemSpec<'a>,
        period: usize,
        prev_dispatch_mw: Option<&'a [f64]>,
        prev_dispatch_mask: Option<&'a [bool]>,
    ) -> Self {
        let period_spec = spec.period(period);
        let committed: Vec<bool> = (0..gen_indices.len())
            .map(|j| period_spec.is_committed(j))
            .collect();
        let (dl_orig_idx, dl_list): (Vec<usize>, Vec<&DispatchableLoad>) = spec
            .dispatchable_loads
            .iter()
            .enumerate()
            .filter_map(|(idx, dl)| dl.in_service.then_some((idx, dl)))
            .unzip();
        let dl_pmax_pu: Vec<f64> = dl_list
            .iter()
            .enumerate()
            .map(|(k, dl)| {
                let dl_idx = dl_orig_idx.get(k).copied().unwrap_or(k);
                let (_, p_max_pu, _, _, _, _) =
                    resolve_dl_for_period_from_spec(dl_idx, period, dl, spec);
                p_max_pu
            })
            .collect();
        // Map each DL to its area/zone using bus number → area lookup.
        let bus_map = network.bus_index_map();
        let dl_area: Vec<usize> = dl_list
            .iter()
            .map(|dl| {
                bus_map
                    .get(&dl.bus)
                    .and_then(|&idx| study_area_for_bus_index(network, spec, idx))
                    .unwrap_or(0)
            })
            .collect();
        Self {
            spec,
            period,
            network,
            gen_indices,
            committed,
            generator_area: spec.generator_area,
            prev_dispatch_mw,
            prev_dispatch_mask,
            // Reserve costs and shortfall penalties are
            // `dt × c × variable`. Use the actual period duration so
            // non-uniform horizons stay dimensionally correct — the
            // scalar `spec.dt_hours` is a reference value that is
            // wrong for any sub-hourly or mixed-duration horizon.
            dt_hours: spec.period_hours(period),
            base: network.base_mva,
            ramp_sharing: spec.ramp_sharing,
            dl_list,
            dl_indices: dl_orig_idx,
            dl_pmax_pu,
            dl_area,
        }
    }

    fn prev_dispatch_at(&self, gen_idx: usize) -> Option<f64> {
        let values = self.prev_dispatch_mw?;
        if let Some(mask) = self.prev_dispatch_mask
            && !mask.get(gen_idx).copied().unwrap_or(false)
        {
            return None;
        }
        values.get(gen_idx).copied()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ResolvedReserveOffer {
    pub capacity_mw: f64,
    pub cost_per_mwh: f64,
}

fn scheduled_reserve_offer(
    schedule: &crate::request::ReserveOfferSchedule,
    product_id: &str,
    period: usize,
) -> Option<ResolvedReserveOffer> {
    schedule
        .periods
        .get(period)
        .or_else(|| schedule.periods.last())
        .and_then(|offers| offers.iter().find(|offer| offer.product_id == product_id))
        .map(|offer| ResolvedReserveOffer {
            capacity_mw: offer.capacity_mw,
            cost_per_mwh: offer.cost_per_mwh,
        })
}

pub(crate) fn generator_reserve_offer_for_period(
    spec: &DispatchProblemSpec<'_>,
    global_gen_idx: usize,
    generator: &Generator,
    product_id: &str,
    period: usize,
) -> Option<ResolvedReserveOffer> {
    spec.gen_reserve_offer_schedules
        .get(&global_gen_idx)
        .and_then(|schedule| scheduled_reserve_offer(schedule, product_id, period))
        .or_else(|| {
            generator
                .reserve_offer(product_id)
                .map(|offer| ResolvedReserveOffer {
                    capacity_mw: offer.capacity_mw,
                    cost_per_mwh: offer.cost_per_mwh,
                })
        })
}

pub(crate) fn dispatchable_load_reserve_offer_for_period(
    spec: &DispatchProblemSpec<'_>,
    dl_idx: usize,
    dispatchable_load: &DispatchableLoad,
    product_id: &str,
    period: usize,
) -> Option<ResolvedReserveOffer> {
    spec.dl_reserve_offer_schedules
        .get(&dl_idx)
        .and_then(|schedule| scheduled_reserve_offer(schedule, product_id, period))
        .or_else(|| {
            dispatchable_load
                .reserve_offer(product_id)
                .map(|offer| ResolvedReserveOffer {
                    capacity_mw: offer.capacity_mw,
                    cost_per_mwh: offer.cost_per_mwh,
                })
        })
}

// ---------------------------------------------------------------------------
// Active product — a product that has a non-zero system or zonal requirement
// ---------------------------------------------------------------------------

/// An active reserve product with pre-computed LP metadata.
#[derive(Debug)]
pub(crate) struct ActiveProduct {
    /// Index into the products vec.
    #[allow(dead_code)]
    pub product_idx: usize,
    /// Product definition (cloned for ownership).
    pub product: ReserveProduct,
    /// Indices of matching system requirement entries, when present.
    pub system_req_indices: Vec<usize>,
    /// Maximum system requirement represented by this layout in MW.
    ///
    /// SCED period layouts set this to the current period requirement. SCUC
    /// uses the horizon max so slack variables remain large enough for any hour.
    pub system_req_cap_mw: f64,
    /// Indices of all system requirement entries contributing to this
    /// product's balance row, including cumulative substitutions.
    pub system_balance_req_indices: Vec<usize>,
    /// Maximum cumulative system requirement represented by this layout in MW.
    pub system_balance_cap_mw: f64,
    /// Indices of reserve products whose awards contribute to this product's
    /// balance row, including `self`.
    pub balance_product_indices: Vec<usize>,
    /// Column offset of R_p[0] in the LP (gen reserve vars).
    ///
    /// The `R_p` block is sized to `gen_participation.len()` — only
    /// generators listed in `gen_participation` get a reserve column.
    /// Use [`ActiveProduct::gen_reserve_col`] to map a local generator
    /// index (into the enclosing `gen_indices`) to its column, handling
    /// non-participating gens by returning `None`.
    pub gen_var_offset: usize,
    /// Ordered list of local generator indices that participate in this
    /// product. The reserve column for `gen_participation[k]` is
    /// `gen_var_offset + k`. Gens not in this list contribute no
    /// variables, no row coefficients, and no rows — previously they
    /// existed with `col_upper = 0` and were stripped by HiGHS presolve,
    /// now they're absent from the LP entirely.
    pub gen_participation: Vec<usize>,
    /// Inverse map: local gen index → position within `gen_participation`,
    /// or `None` if the gen doesn't participate. Length = `n_gen`.
    /// Used by [`ActiveProduct::gen_reserve_col`] for O(1) lookup.
    pub gen_col_local: Vec<Option<usize>>,
    /// Column offset of R_dl_p[0] in the LP (DR reserve vars).
    ///
    /// The `R_dl_p` block is sized to
    /// `dl_group_participation.len()` — one reserve column per
    /// participating consumer group, NOT per individual DL block.
    /// When multiple price-decomposed blocks belong to the same
    /// physical consumer (via
    /// [`DispatchableLoad::reserve_group`]), they share a single
    /// reserve award and contribute to the per-product energy-coupling
    /// row as a summed dispatch. Use
    /// [`ActiveProduct::dl_group_reserve_col`] for the O(1) sparse
    /// lookup by group index.
    pub dl_var_offset: usize,
    /// Ordered list of consumer-group indices (into the layout's
    /// `dl_consumer_groups`) that participate in this product.
    pub dl_group_participation: Vec<usize>,
    /// Inverse map: consumer-group index → position within
    /// `dl_group_participation`, or `None` if the group doesn't
    /// participate. Length = `n_groups` (total groups in the layout,
    /// not n_dl).
    pub dl_group_col_local: Vec<Option<usize>>,
    /// Column offset of first penalty slack variable (system shortfall).
    ///
    /// For `PiecewiseLinear` demand curves, there are `n_penalty_slacks`
    /// consecutive slack variables starting here, one per segment with
    /// increasing penalty costs. Convexity (non-decreasing slopes) ensures
    /// the LP fills segments in order without explicit sequencing constraints.
    pub slack_offset: usize,
    /// Number of penalty slack variables for this product.
    ///
    /// 1 for `Linear`/`Quadratic` curves, N for `PiecewiseLinear` with N segments.
    pub n_penalty_slacks: usize,
    /// Number of zonal requirements for this product.
    pub n_zonal: usize,
    /// Column offset of first zonal slack variable.
    pub zonal_slack_offset: usize,
    /// Zonal requirements for this product.
    pub zonal_reqs: Vec<ActiveZonalRequirement>,
}

/// Active zonal reserve requirement metadata for one product and zone.
#[derive(Debug, Clone)]
pub(crate) struct ActiveZonalRequirement {
    /// Zone identifier — matches generator/load area ids.
    pub zone_id: usize,
    /// Index of the matching zonal requirement entry.
    pub req_idx: usize,
    /// Maximum requirement represented by this layout in MW.
    pub cap_mw: f64,
    /// Indices of all zonal requirement entries contributing to this
    /// product-zone balance row, including cumulative substitutions.
    pub balance_req_indices: Vec<usize>,
    /// Maximum cumulative requirement represented by this balance row in MW.
    pub balance_cap_mw: f64,
    /// Optional zone-specific shortfall penalty override in $/MW.
    pub shortfall_cost_per_unit: Option<f64>,
    /// Optional coefficient on served dispatchable-load MW in the zone.
    pub served_dispatchable_load_coefficient: Option<f64>,
    /// Optional coefficient on each generator dispatch MW in the zone.
    ///
    /// When present, the reserve formulation adds one requirement row per
    /// in-zone generator so that the reserve award must cover the largest
    /// dispatched generator in the zone without introducing extra LP vars.
    pub largest_generator_dispatch_coefficient: Option<f64>,
    /// Optional explicit bus membership for this reserve zone.
    pub participant_bus_numbers: Option<Vec<u32>>,
    /// HashSet view of `participant_bus_numbers` for O(1) membership
    /// checks. Hot loops like `zonal_slack_upper_bound_mw` (called per
    /// product × zonal_req × period with 16,000+ DLs) hit
    /// `participants.contains(&bus_number)` on this set — O(1) with
    /// the set vs O(P_zone) when going through the Vec. Always `Some`
    /// when `participant_bus_numbers` is `Some` (built at layout
    /// construction time).
    pub participant_bus_set: Option<HashSet<u32>>,
    /// Cumulative coefficient on served dispatchable-load MW for this
    /// product-zone balance row, including balance-product substitutions.
    pub balance_served_dispatchable_load_coefficient: Option<f64>,
    /// Cumulative coefficient on largest generator dispatch MW for this
    /// product-zone balance row, including balance-product substitutions.
    pub balance_largest_generator_dispatch_coefficient: Option<f64>,
    /// Number of in-zone generator rows induced by
    /// `largest_generator_dispatch_coefficient`.
    pub largest_generator_row_count: usize,
}

impl ActiveProduct {
    /// Map a local generator index (into the enclosing `gen_indices`) to
    /// its reserve column, or `None` if the gen doesn't participate in
    /// this product.
    #[inline]
    pub fn gen_reserve_col(&self, local_gen_idx: usize) -> Option<usize> {
        self.gen_col_local
            .get(local_gen_idx)
            .copied()
            .flatten()
            .map(|offset_in_block| self.gen_var_offset + offset_in_block)
    }

    /// Map a consumer-group index (into the enclosing layout's
    /// `dl_consumer_groups`) to its reserve column, or `None` if the
    /// group doesn't participate in this product. All DL members of
    /// the same group share this single column.
    #[inline]
    pub fn dl_group_reserve_col(&self, group_idx: usize) -> Option<usize> {
        self.dl_group_col_local
            .get(group_idx)
            .copied()
            .flatten()
            .map(|offset_in_block| self.dl_var_offset + offset_in_block)
    }
}

impl ActiveZonalRequirement {
    fn includes_bus_number(&self, bus_number: u32, fallback_area: Option<usize>) -> bool {
        match &self.participant_bus_set {
            Some(set) => set.contains(&bus_number),
            None => fallback_area.unwrap_or(0) == self.zone_id,
        }
    }

    fn row_count(&self) -> usize {
        self.largest_generator_row_count.max(1)
    }
}

// ---------------------------------------------------------------------------
// ReserveLpLayout — variable and row layout for all active reserve products
// ---------------------------------------------------------------------------

/// Complete LP layout for generic reserve products.
pub struct ReserveLpLayout {
    /// Active products with their LP offsets.
    pub(crate) products: Vec<ActiveProduct>,
    /// Consumer groups used for DL-side reserve aggregation. Indexed
    /// by the group index used throughout `ActiveProduct::dl_group_*`.
    /// When every DL is a singleton (no shared `reserve_group`), the
    /// groups vector has length equal to `n_dl` and the layout
    /// reduces to per-block semantics.
    pub dl_consumer_groups: Vec<DlConsumerGroup>,
    /// Total number of reserve-related LP variables added.
    pub n_reserve_vars: usize,
    /// Total number of reserve-related LP rows added.
    pub n_reserve_rows: usize,
    /// Number of ramp sharing rows (0 if no prev_dispatch or sharing disabled).
    pub n_ramp_sharing_rows: usize,
    /// Aggregate cross-product headroom rows (0 or n_gen).
    pub n_cross_headroom_rows: usize,
    /// Aggregate cross-product footroom rows (0 or n_gen).
    pub n_cross_footroom_rows: usize,
    /// Aggregate cross-product dispatchable-load headroom rows (0 or n_dl).
    pub n_dl_cross_headroom_rows: usize,
    /// Aggregate cross-product dispatchable-load footroom rows (0 or n_dl).
    pub n_dl_cross_footroom_rows: usize,
    /// Shared absolute reserve capability rows (one per product ladder, per generator).
    pub n_shared_limit_rows: usize,
    /// Shared absolute reserve capability rows for dispatchable loads.
    pub n_dl_shared_limit_rows: usize,
    /// Row offset where ramp sharing rows begin (relative to reserve_row_base).
    #[allow(dead_code)]
    pub ramp_sharing_row_offset: usize,
}

#[inline]
fn dl_energy_coupling(product: &ReserveProduct) -> EnergyCoupling {
    product
        .dispatchable_load_energy_coupling
        .unwrap_or(product.energy_coupling)
}

fn zonal_requirement_upper_bound_mw(req: &ActiveZonalRequirement, ctx: &ReserveLpCtx<'_>) -> f64 {
    let served_dl_cap_mw = req
        .balance_served_dispatchable_load_coefficient
        .unwrap_or(0.0)
        * ctx
            .dl_pmax_pu
            .iter()
            .enumerate()
            .filter(|(k, _)| {
                ctx.dl_list
                    .get(*k)
                    .is_some_and(|dl| req.includes_bus_number(dl.bus, ctx.dl_area.get(*k).copied()))
            })
            .map(|(_, p_max_pu)| p_max_pu.max(0.0) * ctx.base)
            .sum::<f64>();
    let largest_gen_cap_mw = req
        .balance_largest_generator_dispatch_coefficient
        .unwrap_or(0.0)
        * ctx
            .gen_indices
            .iter()
            .enumerate()
            .filter(|(j, gi)| {
                req.includes_bus_number(
                    ctx.network.generators[**gi].bus,
                    ctx.generator_area.get(*j).copied(),
                )
            })
            .map(|(_, &gi)| ctx.network.generators[gi].pmax.max(0.0))
            .fold(0.0, f64::max);
    (req.balance_cap_mw + served_dl_cap_mw + largest_gen_cap_mw).max(0.0)
}

fn zonal_largest_generator_row_count(
    zone_id: usize,
    participant_bus_numbers: Option<&[u32]>,
    generator_area: &[usize],
    generator_bus_numbers: &[u32],
) -> usize {
    generator_bus_numbers
        .iter()
        .enumerate()
        .filter(|(j, bus_number)| {
            crate::common::network::zonal_participant_bus_matches(
                zone_id,
                participant_bus_numbers,
                **bus_number,
                generator_area.get(*j).copied(),
            )
        })
        .count()
}

// ---------------------------------------------------------------------------
// Participation computation — which resources actually participate in each product
// ---------------------------------------------------------------------------

/// Whether a resource COULD qualify for a product under some commitment
/// state. This is the conservative superset used at layout build time,
/// before we know the MIP's commitment decisions. A resource that can't
/// qualify under any commitment state is never a participant and its
/// reserve variable can be omitted from the LP entirely.
///
/// Matches the precision of [`qualifies_for`] but ORs the
/// `is_committed = true` and `is_committed = false` branches so the
/// layout stays safe: if there's ANY commitment state in which the
/// resource would qualify, it's kept as a participant.
fn could_qualify_for(
    rule: &QualificationRule,
    is_quick_start: bool,
    qualifications: &surge_network::market::QualificationMap,
) -> bool {
    qualifies_for(rule, true, is_quick_start, qualifications)
        || qualifies_for(rule, false, is_quick_start, qualifications)
}

/// True iff generator `gi` participates in reserve product `product`:
/// it could qualify (via [`could_qualify_for`]) AND has a nonzero offer
/// capacity in at least one period. Reactive-kind products are always
/// non-participating in the DC SCUC LP (handled in the AC-OPF NLP).
fn gen_participates_in_product(
    product: &ReserveProduct,
    generator: &Generator,
    gi: usize,
    spec: &DispatchProblemSpec<'_>,
    n_periods: usize,
) -> bool {
    if matches!(product.kind, surge_network::market::ReserveKind::Reactive) {
        return false;
    }
    let empty_quals = surge_network::market::QualificationMap::default();
    let qualifications = generator
        .market
        .as_ref()
        .map(|m| &m.qualifications)
        .unwrap_or(&empty_quals);
    if !could_qualify_for(
        &product.qualification,
        generator.quick_start,
        qualifications,
    ) {
        return false;
    }
    (0..n_periods).any(|t| {
        generator_reserve_offer_for_period(spec, gi, generator, &product.id, t)
            .map(|offer| offer.capacity_mw > 1e-12)
            .unwrap_or(false)
    })
}

/// Compute, for each reserve product, the ordered list of local gen
/// indices (into `gen_indices`) that participate. The result is a
/// `Vec` of length `products.len()`, preserving the caller-provided
/// product order. Callers pass this into [`build_layout`] to allocate
/// reserve columns only for participating `(product, gen)` pairs.
///
/// Participation is stable across periods — a gen participates iff it
/// has a nonzero offer in ANY period. Periods where the gen's offer
/// happens to be zero still see the col (with `col_upper = 0` in that
/// specific period's bounds pass); this keeps the LP layout identical
/// across periods in the SCED per-period path.
pub fn compute_gen_participation(
    products: &[ReserveProduct],
    spec: &DispatchProblemSpec<'_>,
    network: &Network,
    gen_indices: &[usize],
    n_periods: usize,
) -> Vec<Vec<usize>> {
    products
        .iter()
        .map(|product| {
            (0..gen_indices.len())
                .filter(|&j| {
                    let gi = gen_indices[j];
                    let g = &network.generators[gi];
                    gen_participates_in_product(product, g, gi, spec, n_periods)
                })
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Dispatchable-load consumer groups — share a single reserve column
// across multiple price-decomposed DL blocks of one physical consumer
// ---------------------------------------------------------------------------

/// A reserve-side aggregation of dispatchable-load blocks that belong
/// to the same physical consumer. When a consumer is decomposed into
/// multiple price blocks (as the GO C3 adapter does), the blocks share
/// the same reserve offer cost and reserve capability — one LP reserve
/// column per `(product, group)` suffices, rather than one per
/// `(product, block)`. Energy-coupling rows sum the member blocks'
/// served-power variables instead of constraining each block
/// individually.
#[derive(Debug, Clone)]
pub struct DlConsumerGroup {
    /// Grouping key — typically a consumer UID carried through the
    /// caller's adapter via `DispatchableLoad::reserve_group`. `None`
    /// indicates the member DL stands alone and forms a singleton
    /// group.
    pub key: Option<String>,
    /// Local DL indices (into the caller's `dl_list`) that belong to
    /// this group. Ordered by first-seen insertion, stable across
    /// re-runs given the same input.
    pub member_dl_indices: Vec<usize>,
    /// Canonical bus for the group — all members are expected to live
    /// at the same physical bus. Taken from the first member.
    pub canonical_bus: u32,
}

/// Partition `dl_list` into consumer groups by the
/// [`DispatchableLoad::reserve_group`] field. DLs with `None`
/// `reserve_group` become singleton groups (one block = one group).
/// First-seen order is preserved so the returned `groups` slice is
/// deterministic given the input.
pub fn compute_dl_consumer_groups(dl_list: &[&DispatchableLoad]) -> Vec<DlConsumerGroup> {
    let mut groups: Vec<DlConsumerGroup> = Vec::with_capacity(dl_list.len());
    let mut key_to_group: HashMap<String, usize> = HashMap::new();
    for (k, dl) in dl_list.iter().enumerate() {
        match &dl.reserve_group {
            Some(key) => {
                if let Some(&idx) = key_to_group.get(key) {
                    debug_assert_eq!(
                        groups[idx].canonical_bus, dl.bus,
                        "reserve_group members must share a bus ({} != {} for group {})",
                        groups[idx].canonical_bus, dl.bus, key,
                    );
                    groups[idx].member_dl_indices.push(k);
                } else {
                    let idx = groups.len();
                    key_to_group.insert(key.clone(), idx);
                    groups.push(DlConsumerGroup {
                        key: Some(key.clone()),
                        member_dl_indices: vec![k],
                        canonical_bus: dl.bus,
                    });
                }
            }
            None => {
                groups.push(DlConsumerGroup {
                    key: None,
                    member_dl_indices: vec![k],
                    canonical_bus: dl.bus,
                });
            }
        }
    }
    groups
}

/// Split a consumer group's total reserve award (in MW) across its
/// member DL blocks via uniform pro-rata. Returns a vector of length
/// `group.member_dl_indices.len()` whose entries sum exactly to
/// `group_award_mw`. The last entry absorbs any float residual so the
/// sum is bit-exact.
///
/// Rationale: every member of a group shares the same
/// `cost_per_mwh` for a given product (the adapter pro-rates the
/// consumer's offer across blocks by size while keeping the $/MWh
/// rate constant). Any partition of the group award across members
/// preserves total reserve cost exactly. Uniform is the simplest
/// extraction that handles singletons as a no-op.
pub(crate) fn prorata_group_award_uniform(
    group: &DlConsumerGroup,
    group_award_mw: f64,
) -> Vec<f64> {
    let n = group.member_dl_indices.len();
    match n {
        0 => Vec::new(),
        1 => vec![group_award_mw],
        _ => {
            let mut shares = vec![group_award_mw / n as f64; n];
            // Put the residual (last-digit float drift) on the last
            // block so Σ shares == group_award_mw exactly.
            let partial: f64 = shares.iter().take(n - 1).sum();
            shares[n - 1] = group_award_mw - partial;
            shares
        }
    }
}

/// True iff dispatchable load `dl` participates in reserve product
/// `product`: it could qualify (dispatchable loads are treated as
/// "committed" for qualification purposes — they're always present as
/// a curtailable load) AND has a nonzero offer capacity in at least
/// one period. Reactive-kind products are skipped at DC SCUC layout.
fn dl_participates_in_product(
    product: &ReserveProduct,
    dl: &DispatchableLoad,
    dl_idx: usize,
    spec: &DispatchProblemSpec<'_>,
    n_periods: usize,
) -> bool {
    if matches!(product.kind, surge_network::market::ReserveKind::Reactive) {
        return false;
    }
    // DLs are always "committed" in the load sense (they exist as a
    // curtailable resource); `is_quick_start = false` for dispatchable
    // loads — there's no offline-quick-start concept on the DL side.
    // Use the full `qualifies_for` (not `could_qualify_for`) because
    // DL commitment isn't a MIP decision and the simpler call suffices.
    if !qualifies_for(&product.qualification, true, false, &dl.qualifications) {
        return false;
    }
    (0..n_periods).any(|t| {
        dispatchable_load_reserve_offer_for_period(spec, dl_idx, dl, &product.id, t)
            .map(|offer| offer.capacity_mw > 1e-12)
            .unwrap_or(false)
    })
}

/// Compute, for each reserve product, the ordered list of consumer-
/// group indices (into `groups`) that participate. A group
/// participates in a product iff ANY of its member DL blocks
/// participates — this mirrors the per-block semantics from Phase 2
/// because every block that could offer is now aggregated under its
/// group's single reserve variable.
pub fn compute_dl_group_participation(
    products: &[ReserveProduct],
    spec: &DispatchProblemSpec<'_>,
    dl_list: &[&DispatchableLoad],
    dl_orig_idx: &[usize],
    groups: &[DlConsumerGroup],
    n_periods: usize,
) -> Vec<Vec<usize>> {
    products
        .iter()
        .map(|product| {
            groups
                .iter()
                .enumerate()
                .filter_map(|(gi, group)| {
                    let any_participates = group.member_dl_indices.iter().any(|&k| {
                        let dl = dl_list[k];
                        let orig = dl_orig_idx.get(k).copied().unwrap_or(k);
                        dl_participates_in_product(product, dl, orig, spec, n_periods)
                    });
                    any_participates.then_some(gi)
                })
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Row-family participation counts (Phase 4)
// ---------------------------------------------------------------------------

/// Count generators that participate in at least one active product
/// satisfying `selector`. A gen j participates if any selected ap has
/// `ap.gen_col_local[j].is_some()`. Used to size cross-headroom /
/// cross-footroom rows, which are redundant with the pg col bound
/// for a gen that never appears in any coupled product's reserve
/// column.
fn count_gens_participating_in_coupling<F>(
    active: &[ActiveProduct],
    n_gen: usize,
    selector: F,
) -> usize
where
    F: Fn(&ActiveProduct) -> bool,
{
    (0..n_gen)
        .filter(|&j| {
            active
                .iter()
                .any(|ap| selector(ap) && ap.gen_col_local.get(j).copied().flatten().is_some())
        })
        .count()
}

/// Consumer-group analog of [`count_gens_participating_in_coupling`]
/// for DL-side cross rows.
fn count_groups_participating_in_coupling<F>(
    active: &[ActiveProduct],
    n_groups: usize,
    selector: F,
) -> usize
where
    F: Fn(&ActiveProduct) -> bool,
{
    (0..n_groups)
        .filter(|&gi| {
            active.iter().any(|ap| {
                selector(ap) && ap.dl_group_col_local.get(gi).copied().flatten().is_some()
            })
        })
        .count()
}

/// Count generators where the shared-limit row for `ap` carries at
/// least one reserve-col term: gen j contributes iff `ap.gen_col_local
/// [j].is_some()` OR any shared_limit_product whose qualification
/// overlaps ap's has `gen_col_local[j].is_some()`. Rows where all
/// reserve coefficients are zero reduce to `0 ≤ offer_cap` and are
/// dropped.
fn count_shared_limit_gens(ap: &ActiveProduct, all: &[ActiveProduct], n_gen: usize) -> usize {
    (0..n_gen)
        .filter(|&j| {
            if ap.gen_col_local.get(j).copied().flatten().is_some() {
                return true;
            }
            ap.product.shared_limit_products.iter().any(|shared_id| {
                all.iter()
                    .find(|candidate| {
                        candidate.product.id == *shared_id
                            && qualifications_can_overlap(
                                &ap.product.qualification,
                                &candidate.product.qualification,
                            )
                    })
                    .and_then(|sp| sp.gen_col_local.get(j).copied().flatten())
                    .is_some()
            })
        })
        .count()
}

/// Consumer-group analog of [`count_shared_limit_gens`] for DL
/// shared-limit rows.
fn count_shared_limit_groups(ap: &ActiveProduct, all: &[ActiveProduct], n_groups: usize) -> usize {
    (0..n_groups)
        .filter(|&gi| {
            if ap.dl_group_col_local.get(gi).copied().flatten().is_some() {
                return true;
            }
            ap.product.shared_limit_products.iter().any(|shared_id| {
                all.iter()
                    .find(|candidate| {
                        candidate.product.id == *shared_id
                            && qualifications_can_overlap(
                                &ap.product.qualification,
                                &candidate.product.qualification,
                            )
                    })
                    .and_then(|sp| sp.dl_group_col_local.get(gi).copied().flatten())
                    .is_some()
            })
        })
        .count()
}

// ---------------------------------------------------------------------------
// Build the layout
// ---------------------------------------------------------------------------

/// Build the reserve LP layout from products and requirements.
///
/// `gen_participation_by_product` is per-product ordered lists of
/// local generator indices that actually participate in that product.
/// `dl_consumer_groups` is the reserve-side aggregation of DL blocks
/// (built once via [`compute_dl_consumer_groups`]); every DL block
/// belongs to exactly one group. `dl_group_participation_by_product`
/// lists, per product, which group indices participate (built via
/// [`compute_dl_group_participation`]).
///
/// The reserve column block for product `p` is sized to
/// `gen_participation[p].len() + dl_group_participation[p].len() +
/// slacks + zonal_slacks`. Groups with multiple members share a
/// single reserve variable — energy-coupling and balance rows sum the
/// members' served-power variables rather than treating each block as
/// an independent provider.
///
/// Callers that don't want sparsification can pass dense defaults
/// (`(0..n_gen).collect()` / `(0..n_groups).collect()`); pass
/// singletons as the group list (one member per group) for
/// block-level behavior identical to Phase 2.
#[allow(clippy::too_many_arguments)]
pub fn build_layout(
    products: &[ReserveProduct],
    sys_reqs: &[SystemReserveRequirement],
    zonal_reqs: &[ZonalReserveRequirement],
    ramp_sharing: &RampSharingConfig,
    generator_area: &[usize],
    generator_bus_numbers: &[u32],
    n_gen: usize,
    n_storage: usize,
    n_dl: usize,
    var_base: usize,
    has_prev_dispatch: bool,
    gen_participation_by_product: &[Vec<usize>],
    dl_consumer_groups: &[DlConsumerGroup],
    dl_group_participation_by_product: &[Vec<usize>],
) -> ReserveLpLayout {
    debug_assert_eq!(generator_bus_numbers.len(), n_gen);
    debug_assert_eq!(
        gen_participation_by_product.len(),
        products.len(),
        "gen_participation_by_product must have one entry per product"
    );
    debug_assert_eq!(
        dl_group_participation_by_product.len(),
        products.len(),
        "dl_group_participation_by_product must have one entry per product"
    );
    // Verify that the caller's group partition covers every DL exactly
    // once. Cheap in release (len check), more rigorous in debug.
    let dl_members_covered: usize = dl_consumer_groups
        .iter()
        .map(|g| g.member_dl_indices.len())
        .sum();
    debug_assert_eq!(
        dl_members_covered, n_dl,
        "dl_consumer_groups must partition all n_dl blocks (covered {}, expected {})",
        dl_members_covered, n_dl,
    );
    let n_groups = dl_consumer_groups.len();
    let mut active: Vec<ActiveProduct> = Vec::new();
    let mut var_cursor = var_base;
    let sys_req_by_product: HashMap<&str, Vec<(usize, f64)>> =
        sys_reqs
            .iter()
            .enumerate()
            .fold(HashMap::new(), |mut acc, (idx, req)| {
                let active_req_mw = req
                    .per_period_mw
                    .as_ref()
                    .map(|v| v.iter().copied().fold(req.requirement_mw, f64::max))
                    .unwrap_or(req.requirement_mw);
                acc.entry(req.product_id.as_str())
                    .or_default()
                    .push((idx, active_req_mw));
                acc
            });
    let mut zonal_reqs_by_product: HashMap<&str, Vec<ActiveZonalRequirement>> =
        zonal_reqs.iter().fold(HashMap::new(), |mut acc, req| {
            let active_req_mw = req
                .per_period_mw
                .as_ref()
                .map(|v| v.iter().copied().fold(req.requirement_mw, f64::max))
                .unwrap_or(req.requirement_mw);
            acc.entry(req.product_id.as_str())
                .or_default()
                .push(ActiveZonalRequirement {
                    zone_id: req.zone_id,
                    req_idx: 0,
                    cap_mw: active_req_mw,
                    balance_req_indices: Vec::new(),
                    balance_cap_mw: active_req_mw,
                    shortfall_cost_per_unit: req.shortfall_cost_per_unit,
                    served_dispatchable_load_coefficient: req.served_dispatchable_load_coefficient,
                    largest_generator_dispatch_coefficient: req
                        .largest_generator_dispatch_coefficient,
                    participant_bus_numbers: req.participant_bus_numbers.clone(),
                    participant_bus_set: req
                        .participant_bus_numbers
                        .as_ref()
                        .map(|v| v.iter().copied().collect()),
                    balance_served_dispatchable_load_coefficient: req
                        .served_dispatchable_load_coefficient,
                    balance_largest_generator_dispatch_coefficient: req
                        .largest_generator_dispatch_coefficient,
                    largest_generator_row_count: req
                        .largest_generator_dispatch_coefficient
                        .filter(|coeff| *coeff > 0.0)
                        .map(|_| {
                            zonal_largest_generator_row_count(
                                req.zone_id,
                                req.participant_bus_numbers.as_deref(),
                                generator_area,
                                generator_bus_numbers,
                            )
                        })
                        .unwrap_or(0),
                });
            acc
        });

    for (idx, req) in zonal_reqs.iter().enumerate() {
        if let Some(product_reqs) = zonal_reqs_by_product.get_mut(req.product_id.as_str()) {
            if let Some(active_req) = product_reqs
                .iter_mut()
                .find(|candidate| candidate.zone_id == req.zone_id && candidate.req_idx == 0)
            {
                active_req.req_idx = idx;
            }
        }
    }

    let mut required_product_ids: HashSet<String> = products
        .iter()
        .filter_map(|product| {
            let system_req_cap_mw = sys_req_by_product
                .get(product.id.as_str())
                .map(|reqs| reqs.iter().map(|(_, req)| *req).sum::<f64>())
                .unwrap_or(0.0);
            let has_zonal = zonal_reqs_by_product
                .get(product.id.as_str())
                .map(|reqs| !reqs.is_empty())
                .unwrap_or(false);
            (system_req_cap_mw > 0.0 || has_zonal).then_some(product.id.clone())
        })
        .collect();
    loop {
        let mut changed = false;
        for product in products {
            if !required_product_ids.contains(product.id.as_str()) {
                continue;
            }
            for dep in product
                .shared_limit_products
                .iter()
                .chain(product.balance_products.iter())
            {
                if required_product_ids.insert(dep.clone()) {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    for (pi, product) in products.iter().enumerate() {
        // Reactive reserves couple to `Qg`, which the DC SCUC LP does
        // not have. Skip per-device `Reactive` products at SCUC layout
        // time so the LP stays consistent; the AC-OPF NLP picks them
        // up separately via `q_reserve_up` / `q_reserve_down`
        // variables. `ReactiveHeadroom` products ARE kept — they only
        // use the commitment binary `u^on_jt` to enforce an aggregate
        // Q-range constraint at the zonal level and don't need
        // per-device Q dispatch.
        if matches!(product.kind, surge_network::market::ReserveKind::Reactive) {
            continue;
        }
        // Use the max requirement across all periods to decide if this product
        // is active (needs LP variables and rows). The per-period values are used
        // at constraint-building time for the actual RHS.
        let system_req_indices = sys_req_by_product
            .get(product.id.as_str())
            .cloned()
            .unwrap_or_default();
        let system_req_cap_mw = system_req_indices.iter().map(|(_, req)| *req).sum::<f64>();

        let zonals = zonal_reqs_by_product
            .get(product.id.as_str())
            .cloned()
            .unwrap_or_default();

        if !required_product_ids.contains(product.id.as_str()) {
            continue;
        }

        // Sparse gen participation for this product. Callers compute
        // participation via `compute_gen_participation` (based on
        // qualification + nonzero offer); this block is sized
        // accordingly. Build the inverse lookup so row builders can
        // translate `local_gen_idx -> col` in O(1).
        let gen_participation = gen_participation_by_product[pi].clone();
        let mut gen_col_local: Vec<Option<usize>> = vec![None; n_gen];
        for (offset_in_block, &local_j) in gen_participation.iter().enumerate() {
            debug_assert!(
                local_j < n_gen,
                "gen_participation contains out-of-range local index"
            );
            debug_assert!(
                gen_col_local[local_j].is_none(),
                "gen_participation must be unique; duplicate local index"
            );
            gen_col_local[local_j] = Some(offset_in_block);
        }

        let gen_var_offset = var_cursor;
        var_cursor += gen_participation.len();

        // Sparse DL participation — at CONSUMER GROUP level. One
        // reserve column per participating group, regardless of how
        // many price-decomposed member blocks the group contains.
        let dl_group_participation = dl_group_participation_by_product[pi].clone();
        let mut dl_group_col_local: Vec<Option<usize>> = vec![None; n_groups];
        for (offset_in_block, &group_idx) in dl_group_participation.iter().enumerate() {
            debug_assert!(
                group_idx < n_groups,
                "dl_group_participation contains out-of-range group index"
            );
            debug_assert!(
                dl_group_col_local[group_idx].is_none(),
                "dl_group_participation must be unique; duplicate group index"
            );
            dl_group_col_local[group_idx] = Some(offset_in_block);
        }

        let dl_var_offset = var_cursor;
        var_cursor += dl_group_participation.len();

        // Penalty slack variables: one per segment for PiecewiseLinear curves,
        // one for Linear/Quadratic. Each segment has its own cost in the
        // objective, enabling stepped ORDC pricing.
        let n_penalty_slacks = match &product.demand_curve {
            PenaltyCurve::PiecewiseLinear { segments } => segments.len().max(1),
            _ => 1,
        };
        let slack_offset = var_cursor;
        var_cursor += n_penalty_slacks;

        let n_zonal = zonals.len();
        let zonal_slack_offset = var_cursor;
        var_cursor += n_zonal;

        // Storage generators participate through their gen_var_offset entries —
        // no separate storage reserve variables are allocated.
        let _ = n_storage;

        active.push(ActiveProduct {
            product_idx: pi,
            product: product.clone(),
            system_req_indices: system_req_indices.iter().map(|(idx, _)| *idx).collect(),
            system_req_cap_mw,
            system_balance_req_indices: Vec::new(),
            system_balance_cap_mw: 0.0,
            balance_product_indices: Vec::new(),
            gen_var_offset,
            gen_participation,
            gen_col_local,
            dl_var_offset,
            dl_group_participation,
            dl_group_col_local,
            slack_offset,
            n_penalty_slacks,
            n_zonal,
            zonal_slack_offset,
            zonal_reqs: zonals,
        });
    }

    let active_idx_by_product_id: HashMap<String, usize> = active
        .iter()
        .enumerate()
        .map(|(idx, ap)| (ap.product.id.clone(), idx))
        .collect();
    let own_system_req_indices_by_product: HashMap<String, Vec<usize>> = active
        .iter()
        .map(|ap| (ap.product.id.clone(), ap.system_req_indices.clone()))
        .collect();
    let own_system_req_cap_by_product: HashMap<String, f64> = active
        .iter()
        .map(|ap| (ap.product.id.clone(), ap.system_req_cap_mw))
        .collect();

    for ap in &mut active {
        let mut balance_product_indices = Vec::new();
        if let Some(&self_idx) = active_idx_by_product_id.get(ap.product.id.as_str()) {
            balance_product_indices.push(self_idx);
        }
        for dep in &ap.product.balance_products {
            let Some(&dep_idx) = active_idx_by_product_id.get(dep) else {
                continue;
            };
            if !balance_product_indices.contains(&dep_idx) {
                balance_product_indices.push(dep_idx);
            }
        }
        ap.balance_product_indices = balance_product_indices;
        // `balance_products` defines which cleared awards can satisfy THIS
        // product's requirement. The substitution ladder lives only on the
        // left-hand side of the balance row:
        //
        //   self + balance_products >= self_requirement
        //
        // The right-hand side must stay anchored to the product's own base
        // requirement and dynamic coefficients. Summing subordinate product
        // requirements here overstates cumulative ladders such as
        // `reg_up + syn >= syn_req` and `reg_up + syn + nsyn >= nsyn_req`.
        ap.system_balance_req_indices = own_system_req_indices_by_product
            .get(ap.product.id.as_str())
            .cloned()
            .unwrap_or_default();
        ap.system_balance_cap_mw = own_system_req_cap_by_product
            .get(ap.product.id.as_str())
            .copied()
            .unwrap_or(0.0);

        for req in &mut ap.zonal_reqs {
            req.balance_req_indices = vec![req.req_idx];
            req.balance_cap_mw = req.cap_mw;
            req.balance_served_dispatchable_load_coefficient =
                req.served_dispatchable_load_coefficient;
            req.balance_largest_generator_dispatch_coefficient =
                req.largest_generator_dispatch_coefficient;
            req.largest_generator_row_count = req
                .balance_largest_generator_dispatch_coefficient
                .filter(|coeff| *coeff > 0.0)
                .map(|_| {
                    zonal_largest_generator_row_count(
                        req.zone_id,
                        req.participant_bus_numbers.as_deref(),
                        generator_area,
                        generator_bus_numbers,
                    )
                })
                .unwrap_or(0);
        }
    }

    let n_reserve_vars = var_cursor - var_base;

    let n_headroom_products = active
        .iter()
        .filter(|ap| ap.product.energy_coupling == EnergyCoupling::Headroom)
        .count();
    let n_footroom_products = active
        .iter()
        .filter(|ap| ap.product.energy_coupling == EnergyCoupling::Footroom)
        .count();
    let n_dl_headroom_products = active
        .iter()
        .filter(|ap| dl_energy_coupling(&ap.product) == EnergyCoupling::Headroom)
        .count();
    let n_dl_footroom_products = active
        .iter()
        .filter(|ap| dl_energy_coupling(&ap.product) == EnergyCoupling::Footroom)
        .count();
    // Phase 4: sparse row emission. For every row family whose count
    // was previously pinned to `n_gen` or `n_groups`, restrict to the
    // members whose reserve columns actually participate in the
    // family's products — a gen/group that doesn't participate in
    // ANY of the relevant products has only a `pg ≤ pmax` /
    // `0 ≤ offer_cap` residual, which is already implied by the col
    // bounds (pg's physical limits; reserve col upper is 0).
    //
    // Cross-headroom rows: union of gens that participate in ≥1
    // headroom product. Cross-footroom analogously. Skipped entirely
    // unless ≥2 headroom (footroom) products exist — a single
    // coupled product is captured by the per-product energy-coupling
    // row.
    let gens_in_any_headroom_product = count_gens_participating_in_coupling(&active, n_gen, |ap| {
        ap.product.energy_coupling == EnergyCoupling::Headroom
    });
    let gens_in_any_footroom_product = count_gens_participating_in_coupling(&active, n_gen, |ap| {
        ap.product.energy_coupling == EnergyCoupling::Footroom
    });
    let groups_in_any_dl_headroom_product =
        count_groups_participating_in_coupling(&active, n_groups, |ap| {
            dl_energy_coupling(&ap.product) == EnergyCoupling::Headroom
        });
    let groups_in_any_dl_footroom_product =
        count_groups_participating_in_coupling(&active, n_groups, |ap| {
            dl_energy_coupling(&ap.product) == EnergyCoupling::Footroom
        });
    let n_cross_headroom_rows = if n_headroom_products > 1 {
        gens_in_any_headroom_product
    } else {
        0
    };
    let n_cross_footroom_rows = if n_footroom_products > 1 {
        gens_in_any_footroom_product
    } else {
        0
    };
    let n_dl_cross_headroom_rows = if n_dl_headroom_products > 1 {
        groups_in_any_dl_headroom_product
    } else {
        0
    };
    let n_dl_cross_footroom_rows = if n_dl_footroom_products > 1 {
        groups_in_any_dl_footroom_product
    } else {
        0
    };
    // Shared-limit rows: per (ap-with-shared, member) iff the member
    // has a reserve col in ap OR in any of ap's shared_limit_products
    // whose qualifications can overlap ap's. A member with no such
    // col contributes no terms to the row — the row collapses to
    // `0 ≤ offer_cap`, already tautological.
    let n_shared_limit_rows: usize = active
        .iter()
        .filter(|ap| !ap.product.shared_limit_products.is_empty())
        .map(|ap| count_shared_limit_gens(ap, &active, n_gen))
        .sum();
    let n_dl_shared_limit_rows: usize = active
        .iter()
        .filter(|ap| !ap.product.shared_limit_products.is_empty())
        .map(|ap| count_shared_limit_groups(ap, &active, n_groups))
        .sum();

    let mut n_reserve_rows = n_cross_headroom_rows
        + n_cross_footroom_rows
        + n_dl_cross_headroom_rows
        + n_dl_cross_footroom_rows
        + n_shared_limit_rows
        + n_dl_shared_limit_rows;
    for ap in &active {
        // Per-product energy-coupling rows emit only for members whose
        // reserve col actually participates in this product — a
        // non-participant gen's row would be `pg ≤ pmax`, already
        // implied by col bounds.
        match ap.product.energy_coupling {
            EnergyCoupling::Headroom | EnergyCoupling::Footroom => {
                n_reserve_rows += ap.gen_participation.len();
            }
            EnergyCoupling::None => {}
        }
        match dl_energy_coupling(&ap.product) {
            EnergyCoupling::Headroom | EnergyCoupling::Footroom => {
                // One energy-coupling row per participating consumer
                // group, summing member-block dispatches on the LHS.
                n_reserve_rows += ap.dl_group_participation.len();
            }
            EnergyCoupling::None => {}
        }
        if ap.system_balance_cap_mw > 0.0 {
            n_reserve_rows += 1;
        }
        n_reserve_rows += ap
            .zonal_reqs
            .iter()
            .map(ActiveZonalRequirement::row_count)
            .sum::<usize>();
    }

    let has_up = active
        .iter()
        .any(|ap| ap.product.direction == ReserveDirection::Up);
    let has_down = active
        .iter()
        .any(|ap| ap.product.direction == ReserveDirection::Down);
    let n_ramp_sharing_rows =
        if has_prev_dispatch && ramp_sharing.sharing_ratio < 1.0 && (has_up || has_down) {
            let mut rows = 0;
            if has_up {
                rows += n_gen;
            }
            if has_down {
                rows += n_gen;
            }
            rows
        } else {
            0
        };

    let ramp_sharing_row_offset = n_reserve_rows;
    let total_rows = n_reserve_rows + n_ramp_sharing_rows;

    // Participation instrumentation. Emit a one-line summary per product
    // at info level so post-mortems / profile harnesses can see how much
    // of the dense `(n_gen, n_dl)` grid actually lights up. Dense-layout
    // runs would report `allocated_gens == n_gen` for every product;
    // after Phase 1 the gen side is sparse, after Phase 2 the DL side
    // joins it.
    if !active.is_empty() {
        let total_gens_considered = active.len() * n_gen;
        let total_gens_allocated: usize = active.iter().map(|ap| ap.gen_participation.len()).sum();
        let total_groups_considered = active.len() * n_groups;
        let total_groups_allocated: usize = active
            .iter()
            .map(|ap| ap.dl_group_participation.len())
            .sum();
        let n_multi_block_groups = dl_consumer_groups
            .iter()
            .filter(|g| g.member_dl_indices.len() > 1)
            .count();
        tracing::info!(
            n_active_products = active.len(),
            n_gen,
            n_dl,
            n_dl_consumer_groups = n_groups,
            n_multi_block_groups,
            total_gens_considered,
            total_gens_allocated,
            total_gens_stripped = total_gens_considered.saturating_sub(total_gens_allocated),
            total_groups_considered,
            total_groups_allocated,
            total_groups_stripped = total_groups_considered.saturating_sub(total_groups_allocated),
            "Reserve layout: participation summary"
        );
        for ap in &active {
            tracing::info!(
                product = %ap.product.id,
                direction = ?ap.product.direction,
                kind = ?ap.product.kind,
                gen_allocated = ap.gen_participation.len(),
                gen_stripped = n_gen.saturating_sub(ap.gen_participation.len()),
                dl_group_allocated = ap.dl_group_participation.len(),
                dl_group_stripped = n_groups.saturating_sub(ap.dl_group_participation.len()),
                n_zonal = ap.n_zonal,
                "Reserve layout: product participation"
            );
        }
    }

    ReserveLpLayout {
        products: active,
        dl_consumer_groups: dl_consumer_groups.to_vec(),
        n_reserve_vars,
        n_reserve_rows: total_rows,
        n_ramp_sharing_rows,
        n_cross_headroom_rows,
        n_cross_footroom_rows,
        n_dl_cross_headroom_rows,
        n_dl_cross_footroom_rows,
        n_shared_limit_rows,
        n_dl_shared_limit_rows,
        ramp_sharing_row_offset,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_layout_for_period(
    products: &[ReserveProduct],
    sys_reqs: &[SystemReserveRequirement],
    zonal_reqs: &[ZonalReserveRequirement],
    ramp_sharing: &RampSharingConfig,
    generator_area: &[usize],
    generator_bus_numbers: &[u32],
    n_gen: usize,
    n_storage: usize,
    n_dl: usize,
    var_base: usize,
    has_prev_dispatch: bool,
    period: usize,
    gen_participation_by_product: &[Vec<usize>],
    dl_consumer_groups: &[DlConsumerGroup],
    dl_group_participation_by_product: &[Vec<usize>],
) -> ReserveLpLayout {
    let localized_sys_reqs: Vec<SystemReserveRequirement> = sys_reqs
        .iter()
        .map(|req| SystemReserveRequirement {
            product_id: req.product_id.clone(),
            requirement_mw: req.requirement_mw_for_period(period),
            per_period_mw: None,
        })
        .collect();
    let localized_zonal_reqs: Vec<ZonalReserveRequirement> = zonal_reqs
        .iter()
        .map(|req| ZonalReserveRequirement {
            zone_id: req.zone_id,
            product_id: req.product_id.clone(),
            requirement_mw: req.requirement_mw_for_period(period),
            per_period_mw: None,
            shortfall_cost_per_unit: req.shortfall_cost_per_unit,
            served_dispatchable_load_coefficient: req.served_dispatchable_load_coefficient,
            largest_generator_dispatch_coefficient: req.largest_generator_dispatch_coefficient,
            participant_bus_numbers: req.participant_bus_numbers.clone(),
        })
        .collect();

    build_layout(
        products,
        &localized_sys_reqs,
        &localized_zonal_reqs,
        ramp_sharing,
        generator_area,
        generator_bus_numbers,
        n_gen,
        n_storage,
        n_dl,
        var_base,
        has_prev_dispatch,
        gen_participation_by_product,
        dl_consumer_groups,
        dl_group_participation_by_product,
    )
}

// ---------------------------------------------------------------------------
// Variable bounds
// ---------------------------------------------------------------------------

/// Set column bounds for all reserve variables.
pub fn set_bounds(
    layout: &ReserveLpLayout,
    col_lower: &mut [f64],
    col_upper: &mut [f64],
    ctx: &ReserveLpCtx,
) {
    let base = ctx.base;

    for ap in &layout.products {
        for (j, &gi) in ctx.gen_indices.iter().enumerate() {
            // Non-participants have no allocated column — nothing to bound.
            let Some(col) = ap.gen_reserve_col(j) else {
                continue;
            };
            let g = &ctx.network.generators[gi];
            let is_committed = ctx.committed.get(j).copied().unwrap_or(true);

            col_lower[col] = 0.0;

            let empty_quals = Default::default();
            let qualifications = g
                .market
                .as_ref()
                .map(|m| &m.qualifications)
                .unwrap_or(&empty_quals);
            let qualified = qualifies_for(
                &ap.product.qualification,
                is_committed,
                g.quick_start,
                qualifications,
            );

            if !qualified {
                // Gen is a layout-time participant but its *current*
                // commitment state disqualifies it. Pin to 0 just like
                // the dense path did.
                col_upper[col] = 0.0;
                continue;
            }

            let offer_cap =
                generator_reserve_offer_for_period(ctx.spec, gi, g, &ap.product.id, ctx.period)
                    .map(|offer| offer.capacity_mw)
                    .unwrap_or(0.0);
            let is_offline_quick_start = matches!(
                ap.product.qualification,
                QualificationRule::OfflineQuickStart | QualificationRule::QuickStart
            );
            let ramp_cap = if is_offline_quick_start {
                // For offline quick-start products, the product-specific reserve
                // capability already represents the deliverable reserve limit.
                offer_cap
            } else if !ap.product.apply_deploy_ramp_limit {
                // Some market data sources already publish a per-product,
                // deliverable reserve capability (distinct `p_syn_res_ub`,
                // `p_reg_res_up_ub`, etc. fields). Do not clamp those
                // offers a second time by the generic deploy-window ramp cap.
                f64::INFINITY
            } else {
                g.ramp_limited_mw(&ap.product)
            };
            let phys_cap = if is_offline_quick_start {
                g.pmax.max(0.0)
            } else {
                (g.pmax - g.pmin).max(0.0)
            };

            col_upper[col] = offer_cap.min(ramp_cap).min(phys_cap) / base;
        }

        // DR reserve variable bounds — one column per consumer group.
        // Offer cap and physical cap are summed across member blocks;
        // all members of a group share the same `cost_per_mwh` (by
        // construction in the adapter), so aggregating is exact.
        for (gi, group) in layout.dl_consumer_groups.iter().enumerate() {
            let Some(col) = ap.dl_group_reserve_col(gi) else {
                continue;
            };
            col_lower[col] = 0.0;

            let mut any_qualified = false;
            let mut total_offer_mw = 0.0;
            let mut total_phys_mw = 0.0;
            for &k in &group.member_dl_indices {
                let dl = ctx.dl_list[k];
                // DR is always "committed" (load is present and can
                // be curtailed). Per-member qualification check for
                // product-specific rules.
                let qualified =
                    qualifies_for(&ap.product.qualification, true, false, &dl.qualifications);
                if !qualified {
                    continue;
                }
                any_qualified = true;
                let offer_cap = dispatchable_load_reserve_offer_for_period(
                    ctx.spec,
                    ctx.dl_indices.get(k).copied().unwrap_or(k),
                    dl,
                    &ap.product.id,
                    ctx.period,
                )
                .map(|offer| offer.capacity_mw)
                .unwrap_or(0.0);
                let phys_cap =
                    (ctx.dl_pmax_pu.get(k).copied().unwrap_or(dl.p_max_pu) - dl.p_min_pu).max(0.0)
                        * base;
                total_offer_mw += offer_cap;
                total_phys_mw += phys_cap;
            }

            if !any_qualified {
                col_upper[col] = 0.0;
                continue;
            }
            col_upper[col] = total_offer_mw.min(total_phys_mw) / base;
        }

        // System penalty slack bounds — one per segment for PiecewiseLinear,
        // one for Linear/Quadratic. Each segment's slack is bounded by the
        // segment width (in p.u.), ensuring the LP fills cheaper segments first.
        if ap.system_balance_cap_mw <= 0.0 {
            for i in 0..ap.n_penalty_slacks {
                col_lower[ap.slack_offset + i] = 0.0;
                col_upper[ap.slack_offset + i] = 0.0;
            }
        } else {
            match &ap.product.demand_curve {
                PenaltyCurve::PiecewiseLinear { segments } => {
                    let mut prev_max_mw = 0.0_f64;
                    for (i, seg) in segments.iter().enumerate() {
                        col_lower[ap.slack_offset + i] = 0.0;
                        let seg_width_mw = if seg.max_violation.is_infinite() {
                            (ap.system_balance_cap_mw - prev_max_mw).max(0.0)
                        } else {
                            (seg.max_violation - prev_max_mw).max(0.0)
                        };
                        col_upper[ap.slack_offset + i] = seg_width_mw / base;
                        prev_max_mw = if seg.max_violation.is_infinite() {
                            ap.system_balance_cap_mw
                        } else {
                            seg.max_violation
                        };
                    }
                }
                _ => {
                    col_lower[ap.slack_offset] = 0.0;
                    col_upper[ap.slack_offset] = ap.system_balance_cap_mw / base;
                }
            }
        }

        // Zonal penalty slack bounds
        for (zi, req) in ap.zonal_reqs.iter().enumerate() {
            let col = ap.zonal_slack_offset + zi;
            col_lower[col] = 0.0;
            col_upper[col] = zonal_requirement_upper_bound_mw(req, ctx) / base;
        }
    }
}

// ---------------------------------------------------------------------------
// Objective coefficients
// ---------------------------------------------------------------------------

/// Set objective coefficients for reserve variables.
pub fn set_objective(layout: &ReserveLpLayout, col_cost: &mut [f64], ctx: &ReserveLpCtx) {
    let base = ctx.base;
    // Reserve cost coefficients are `$/MWh × pu` columns. The full
    // per-period contribution is `rate × base × dt_h`; without the
    // `dt_h` factor the optimum is wrong on any non-1h horizon.
    let dt_h = ctx.dt_hours;
    let pu_h = base * dt_h;

    for ap in &layout.products {
        for (j, &gi) in ctx.gen_indices.iter().enumerate() {
            // Non-participants have no column, so no cost to assign.
            let Some(col) = ap.gen_reserve_col(j) else {
                continue;
            };
            let g = &ctx.network.generators[gi];
            let cost =
                generator_reserve_offer_for_period(ctx.spec, gi, g, &ap.product.id, ctx.period)
                    .map(|offer| offer.cost_per_mwh)
                    .unwrap_or(0.0);
            if cost > 0.0 {
                col_cost[col] = cost * pu_h;
            }
        }

        // DR reserve offer costs — per group. All members of a group
        // share the same `cost_per_mwh` for a product (the adapter
        // propagates a single consumer-level rate across blocks), so
        // take the first non-zero offer as the group's cost.
        for (gi, group) in layout.dl_consumer_groups.iter().enumerate() {
            let Some(col) = ap.dl_group_reserve_col(gi) else {
                continue;
            };
            let cost = group
                .member_dl_indices
                .iter()
                .find_map(|&k| {
                    let dl = ctx.dl_list[k];
                    dispatchable_load_reserve_offer_for_period(
                        ctx.spec,
                        ctx.dl_indices.get(k).copied().unwrap_or(k),
                        dl,
                        &ap.product.id,
                        ctx.period,
                    )
                    .map(|offer| offer.cost_per_mwh)
                    .filter(|c| *c > 0.0)
                })
                .unwrap_or(0.0);
            if cost > 0.0 {
                col_cost[col] = cost * pu_h;
            }
        }

        // System penalty slack cost from demand curve. For PiecewiseLinear
        // each segment gets its own cost (stepped ORDC). Slack variables are
        // in pu, so the per-period coefficient is `rate ($/pu·h) × dt_h`,
        // which equals `rate ($/MW) × base × dt_h` after the unit normalization
        // already baked into the curve.
        match &ap.product.demand_curve {
            PenaltyCurve::PiecewiseLinear { segments } => {
                for (i, seg) in segments.iter().enumerate() {
                    col_cost[ap.slack_offset + i] = seg.cost_per_unit.max(0.0) * pu_h;
                }
            }
            _ => {
                let penalty = ap.product.demand_curve.marginal_cost_at(0.0).max(0.0) * pu_h;
                col_cost[ap.slack_offset] = penalty;
            }
        }

        // Zonal slack costs use the highest penalty tier (conservative).
        let default_zonal_penalty_per_mwh = match &ap.product.demand_curve {
            PenaltyCurve::PiecewiseLinear { segments } => segments
                .last()
                .map(|s| s.cost_per_unit)
                .unwrap_or(0.0)
                .max(0.0),
            _ => ap.product.demand_curve.marginal_cost_at(0.0).max(0.0),
        };
        for (zi, req) in ap.zonal_reqs.iter().enumerate() {
            let rate_per_mwh = req
                .shortfall_cost_per_unit
                .unwrap_or(default_zonal_penalty_per_mwh)
                .max(0.0);
            col_cost[ap.zonal_slack_offset + zi] = rate_per_mwh * pu_h;
        }
    }
}

// ---------------------------------------------------------------------------
// Constraint triplets and row bounds
// ---------------------------------------------------------------------------

/// Build constraint triplets and row bounds for reserve products.
///
/// Returns `(triplets, row_lower, row_upper)` for the reserve constraint block.
pub fn build_constraints(
    layout: &ReserveLpLayout,
    row_base: usize,
    pg_offset: usize,
    dl_offset: usize,
    ctx: &ReserveLpCtx,
) -> (Vec<Triplet<f64>>, Vec<f64>, Vec<f64>) {
    let n_gen = ctx.gen_indices.len();
    let _n_dl = ctx.dl_list.len();
    let base = ctx.base;

    let n_total_rows = layout.n_reserve_rows;
    let mut triplets: Vec<Triplet<f64>> = Vec::new();
    let mut row_lower = vec![f64::NEG_INFINITY; n_total_rows];
    let mut row_upper = vec![f64::INFINITY; n_total_rows];

    let mut row_cursor = row_base;

    if layout.n_cross_headroom_rows > 0 {
        // Emit only for gens that participate in ≥1 headroom product
        // — a non-participant would have only the pg coefficient,
        // reducing the row to `pg ≤ pmax`, already enforced by the
        // pg col bound.
        let mut emitted = 0;
        for (j, &gi) in ctx.gen_indices.iter().enumerate() {
            let participates = layout.products.iter().any(|ap| {
                ap.product.energy_coupling == EnergyCoupling::Headroom
                    && ap.gen_reserve_col(j).is_some()
            });
            if !participates {
                continue;
            }
            let row = row_cursor + emitted;
            triplets.push(Triplet {
                row,
                col: pg_offset + j,
                val: 1.0,
            });
            for ap in &layout.products {
                if ap.product.energy_coupling == EnergyCoupling::Headroom {
                    if let Some(col) = ap.gen_reserve_col(j) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                }
            }
            row_lower[row - row_base] = f64::NEG_INFINITY;
            row_upper[row - row_base] = ctx.network.generators[gi].pmax / base;
            emitted += 1;
        }
        debug_assert_eq!(emitted, layout.n_cross_headroom_rows);
        row_cursor += layout.n_cross_headroom_rows;
    }

    if layout.n_cross_footroom_rows > 0 {
        let mut emitted = 0;
        for (j, &gi) in ctx.gen_indices.iter().enumerate() {
            let participates = layout.products.iter().any(|ap| {
                ap.product.energy_coupling == EnergyCoupling::Footroom
                    && ap.gen_reserve_col(j).is_some()
            });
            if !participates {
                continue;
            }
            let row = row_cursor + emitted;
            let is_committed = ctx.committed.get(j).copied().unwrap_or(true);
            triplets.push(Triplet {
                row,
                col: pg_offset + j,
                val: -1.0,
            });
            for ap in &layout.products {
                if ap.product.energy_coupling == EnergyCoupling::Footroom {
                    if let Some(col) = ap.gen_reserve_col(j) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                }
            }
            row_lower[row - row_base] = f64::NEG_INFINITY;
            row_upper[row - row_base] = if is_committed {
                -ctx.network.generators[gi].pmin / base
            } else {
                0.0
            };
            emitted += 1;
        }
        debug_assert_eq!(emitted, layout.n_cross_footroom_rows);
        row_cursor += layout.n_cross_footroom_rows;
    }

    let n_groups = layout.dl_consumer_groups.len();
    if layout.n_dl_cross_headroom_rows > 0 {
        let mut emitted = 0;
        for (gi, group) in layout.dl_consumer_groups.iter().enumerate() {
            let participates = layout.products.iter().any(|ap| {
                dl_energy_coupling(&ap.product) == EnergyCoupling::Headroom
                    && ap.dl_group_reserve_col(gi).is_some()
            });
            if !participates {
                continue;
            }
            let row = row_cursor + emitted;
            let mut pmax_sum_pu = 0.0;
            for &k in &group.member_dl_indices {
                triplets.push(Triplet {
                    row,
                    col: dl_offset + k,
                    val: 1.0,
                });
                let dl = ctx.dl_list[k];
                pmax_sum_pu += ctx.dl_pmax_pu.get(k).copied().unwrap_or(dl.p_max_pu);
            }
            for ap in &layout.products {
                if dl_energy_coupling(&ap.product) == EnergyCoupling::Headroom {
                    if let Some(col) = ap.dl_group_reserve_col(gi) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                }
            }
            row_lower[row - row_base] = f64::NEG_INFINITY;
            row_upper[row - row_base] = pmax_sum_pu;
            emitted += 1;
        }
        debug_assert_eq!(emitted, layout.n_dl_cross_headroom_rows);
        row_cursor += layout.n_dl_cross_headroom_rows;
    }

    if layout.n_dl_cross_footroom_rows > 0 {
        let mut emitted = 0;
        for (gi, group) in layout.dl_consumer_groups.iter().enumerate() {
            let participates = layout.products.iter().any(|ap| {
                dl_energy_coupling(&ap.product) == EnergyCoupling::Footroom
                    && ap.dl_group_reserve_col(gi).is_some()
            });
            if !participates {
                continue;
            }
            let row = row_cursor + emitted;
            let mut pmin_sum_pu = 0.0;
            for &k in &group.member_dl_indices {
                triplets.push(Triplet {
                    row,
                    col: dl_offset + k,
                    val: -1.0,
                });
                pmin_sum_pu += ctx.dl_list[k].p_min_pu;
            }
            for ap in &layout.products {
                if dl_energy_coupling(&ap.product) == EnergyCoupling::Footroom {
                    if let Some(col) = ap.dl_group_reserve_col(gi) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                }
            }
            row_lower[row - row_base] = f64::NEG_INFINITY;
            row_upper[row - row_base] = -pmin_sum_pu;
            emitted += 1;
        }
        debug_assert_eq!(emitted, layout.n_dl_cross_footroom_rows);
        row_cursor += layout.n_dl_cross_footroom_rows;
    }

    for ap in &layout.products {
        if ap.product.shared_limit_products.is_empty() {
            continue;
        }
        // Emit shared-limit row only for gens that contribute ≥1
        // reserve-col term (either ap or a qualifying shared
        // product). Non-contributing gens would have `0 ≤ offer_cap`,
        // which is tautological.
        let shared_products_for_ap: Vec<&ActiveProduct> = ap
            .product
            .shared_limit_products
            .iter()
            .filter_map(|shared_id| {
                layout.products.iter().find(|candidate| {
                    candidate.product.id == *shared_id
                        && qualifications_can_overlap(
                            &ap.product.qualification,
                            &candidate.product.qualification,
                        )
                })
            })
            .collect();
        let mut emitted = 0;
        for (j, _) in ctx.gen_indices.iter().enumerate() {
            let contributes = ap.gen_reserve_col(j).is_some()
                || shared_products_for_ap
                    .iter()
                    .any(|sp| sp.gen_reserve_col(j).is_some());
            if !contributes {
                continue;
            }
            let row = row_cursor + emitted;
            if let Some(col) = ap.gen_reserve_col(j) {
                triplets.push(Triplet { row, col, val: 1.0 });
            }
            for sp in &shared_products_for_ap {
                if let Some(col) = sp.gen_reserve_col(j) {
                    triplets.push(Triplet { row, col, val: 1.0 });
                }
            }
            let is_committed = ctx.committed.get(j).copied().unwrap_or(true);
            let offer_cap = generator_reserve_offer_for_period(
                ctx.spec,
                ctx.gen_indices[j],
                &ctx.network.generators[ctx.gen_indices[j]],
                &ap.product.id,
                ctx.period,
            )
            .map(|offer| offer.capacity_mw)
            .unwrap_or(0.0)
                / base;
            row_lower[row - row_base] = f64::NEG_INFINITY;
            row_upper[row - row_base] = match ap.product.qualification {
                QualificationRule::OfflineQuickStart => {
                    if is_committed {
                        0.0
                    } else {
                        offer_cap
                    }
                }
                QualificationRule::QuickStart => offer_cap,
                _ => {
                    if is_committed {
                        offer_cap
                    } else {
                        0.0
                    }
                }
            };
            emitted += 1;
        }
        row_cursor += emitted;
    }

    for ap in &layout.products {
        if ap.product.shared_limit_products.is_empty() {
            continue;
        }
        let shared_products_for_ap: Vec<&ActiveProduct> = ap
            .product
            .shared_limit_products
            .iter()
            .filter_map(|shared_id| {
                layout.products.iter().find(|candidate| {
                    candidate.product.id == *shared_id
                        && qualifications_can_overlap(
                            &ap.product.qualification,
                            &candidate.product.qualification,
                        )
                })
            })
            .collect();
        let mut emitted = 0;
        for (gi, group) in layout.dl_consumer_groups.iter().enumerate() {
            let contributes = ap.dl_group_reserve_col(gi).is_some()
                || shared_products_for_ap
                    .iter()
                    .any(|sp| sp.dl_group_reserve_col(gi).is_some());
            if !contributes {
                continue;
            }
            let row = row_cursor + emitted;
            if let Some(col) = ap.dl_group_reserve_col(gi) {
                triplets.push(Triplet { row, col, val: 1.0 });
            }
            for sp in &shared_products_for_ap {
                if let Some(col) = sp.dl_group_reserve_col(gi) {
                    triplets.push(Triplet { row, col, val: 1.0 });
                }
            }
            let total_offer_mw: f64 = group
                .member_dl_indices
                .iter()
                .map(|&k| {
                    let dl = ctx.dl_list[k];
                    dispatchable_load_reserve_offer_for_period(
                        ctx.spec,
                        ctx.dl_indices.get(k).copied().unwrap_or(k),
                        dl,
                        &ap.product.id,
                        ctx.period,
                    )
                    .map(|offer| offer.capacity_mw)
                    .unwrap_or(0.0)
                })
                .sum();
            row_lower[row - row_base] = f64::NEG_INFINITY;
            row_upper[row - row_base] = total_offer_mw / base;
            emitted += 1;
        }
        row_cursor += emitted;
    }

    for ap in &layout.products {
        // --- Energy coupling rows ---
        match ap.product.energy_coupling {
            EnergyCoupling::Headroom => {
                // Per-product headroom rows emit only for gens that
                // actually have a reserve col for this product. A
                // non-participant's row would be `pg ≤ pmax`, already
                // implied by the pg col bound.
                for (offset, &j) in ap.gen_participation.iter().enumerate() {
                    let gi = ctx.gen_indices[j];
                    let row = row_cursor + offset;
                    if let Some(col) = ap.gen_reserve_col(j) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                    triplets.push(Triplet {
                        row,
                        col: pg_offset + j,
                        val: 1.0,
                    });
                    row_lower[row - row_base] = f64::NEG_INFINITY;
                    row_upper[row - row_base] = ctx.network.generators[gi].pmax / base;
                }
                row_cursor += ap.gen_participation.len();
            }
            EnergyCoupling::Footroom => {
                for (offset, &j) in ap.gen_participation.iter().enumerate() {
                    let gi = ctx.gen_indices[j];
                    let row = row_cursor + offset;
                    let is_committed = ctx.committed.get(j).copied().unwrap_or(true);
                    if let Some(col) = ap.gen_reserve_col(j) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                    triplets.push(Triplet {
                        row,
                        col: pg_offset + j,
                        val: -1.0,
                    });
                    row_lower[row - row_base] = f64::NEG_INFINITY;
                    // When uncommitted, pg=0 and r_dn=0, so relax bound to 0
                    // to avoid infeasibility when pmin > 0.
                    row_upper[row - row_base] = if is_committed {
                        -ctx.network.generators[gi].pmin / base
                    } else {
                        0.0
                    };
                }
                row_cursor += ap.gen_participation.len();
            }
            EnergyCoupling::None => {}
        }

        match dl_energy_coupling(&ap.product) {
            EnergyCoupling::Headroom => {
                // Group-level headroom, emitted only for groups that
                // participate in THIS product:
                //   r_group + Σ_{m ∈ group} p_served[m] ≤ Σ pmax[m]
                // A non-participant group would have no r_group
                // coefficient; the row would collapse to the served
                // envelope `Σ p_served ≤ Σ pmax`, already implied by
                // per-block p_served col bounds.
                for (offset, &gi) in ap.dl_group_participation.iter().enumerate() {
                    let group = &layout.dl_consumer_groups[gi];
                    let row = row_cursor + offset;
                    if let Some(col) = ap.dl_group_reserve_col(gi) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                    let mut pmax_sum_pu = 0.0;
                    for &k in &group.member_dl_indices {
                        triplets.push(Triplet {
                            row,
                            col: dl_offset + k,
                            val: 1.0,
                        });
                        let dl = ctx.dl_list[k];
                        pmax_sum_pu += ctx.dl_pmax_pu.get(k).copied().unwrap_or(dl.p_max_pu);
                    }
                    row_lower[row - row_base] = f64::NEG_INFINITY;
                    row_upper[row - row_base] = pmax_sum_pu;
                }
                row_cursor += ap.dl_group_participation.len();
            }
            EnergyCoupling::Footroom => {
                // Group-level footroom:
                //   r_group − Σ_{m ∈ group} p_served[m] ≤ −Σ pmin[m]
                for (offset, &gi) in ap.dl_group_participation.iter().enumerate() {
                    let group = &layout.dl_consumer_groups[gi];
                    let row = row_cursor + offset;
                    if let Some(col) = ap.dl_group_reserve_col(gi) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                    let mut pmin_sum_pu = 0.0;
                    for &k in &group.member_dl_indices {
                        triplets.push(Triplet {
                            row,
                            col: dl_offset + k,
                            val: -1.0,
                        });
                        pmin_sum_pu += ctx.dl_list[k].p_min_pu;
                    }
                    row_lower[row - row_base] = f64::NEG_INFINITY;
                    row_upper[row - row_base] = -pmin_sum_pu;
                }
                row_cursor += ap.dl_group_participation.len();
            }
            EnergyCoupling::None => {}
        }

        // --- System requirement row ---
        if ap.system_balance_cap_mw > 0.0 {
            let row = row_cursor;
            for balance_idx in &ap.balance_product_indices {
                let Some(balance_product) = layout.products.get(*balance_idx) else {
                    continue;
                };
                for j in 0..n_gen {
                    if let Some(col) = balance_product.gen_reserve_col(j) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                }
                // DR reserve variables contribute to system
                // requirement — one term per participating group.
                for gi in 0..n_groups {
                    if let Some(col) = balance_product.dl_group_reserve_col(gi) {
                        triplets.push(Triplet { row, col, val: 1.0 });
                    }
                }
            }
            // All penalty slacks contribute to the system requirement row.
            for i in 0..ap.n_penalty_slacks {
                triplets.push(Triplet {
                    row,
                    col: ap.slack_offset + i,
                    val: 1.0,
                });
            }
            row_lower[row - row_base] = ap.system_balance_cap_mw / base;
            row_upper[row - row_base] = f64::INFINITY;
            row_cursor += 1;
        }

        // --- Zonal requirement rows ---
        for (zi, req) in ap.zonal_reqs.iter().enumerate() {
            let zone_gen_indices: Vec<usize> = ctx
                .gen_indices
                .iter()
                .enumerate()
                .filter_map(|(j, _)| {
                    req.includes_bus_number(
                        ctx.network.generators[ctx.gen_indices[j]].bus,
                        ctx.generator_area.get(j).copied(),
                    )
                    .then_some(j)
                })
                .collect();
            // Zone membership for DLs: check the DL block's bus. For
            // the served-DL coefficient term, we still emit per-block
            // coefficients (one `dl_offset + k` triplet per in-zone
            // block) because the served-power variables remain
            // per-block. For the reserve-award term we emit one
            // coefficient per in-zone CONSUMER GROUP.
            let zone_dl_indices: Vec<usize> = ctx
                .dl_list
                .iter()
                .enumerate()
                .filter_map(|(k, _)| {
                    req.includes_bus_number(ctx.dl_list[k].bus, ctx.dl_area.get(k).copied())
                        .then_some(k)
                })
                .collect();
            let zone_group_indices: Vec<usize> = layout
                .dl_consumer_groups
                .iter()
                .enumerate()
                .filter_map(|(gi, group)| {
                    let canonical_k = group.member_dl_indices[0];
                    req.includes_bus_number(
                        group.canonical_bus,
                        ctx.dl_area.get(canonical_k).copied(),
                    )
                    .then_some(gi)
                })
                .collect();
            let zonal_rhs = req.balance_cap_mw / base;
            // NOTE: the peak-gen and served-DL coefficients are dimensionless
            // fractions (e.g. 0.03 for "3% of served consumer MW"). The row
            // couples them to decision variables `pg_j` and `dl_k` that are
            // already in per-unit, so the coefficients must stay unscaled —
            // dividing by `base` here collapses the enforced requirement to
            // 1/base of its intended value. See `zonal_requirement_mw_for_period`
            // which treats these coefficients as dimensionless on the display
            // side, and `zonal_requirement_upper_bound_mw` which multiplies
            // them against MW quantities directly.
            let largest_coeff = req
                .balance_largest_generator_dispatch_coefficient
                .unwrap_or(0.0);
            let served_dl_coeff = req
                .balance_served_dispatchable_load_coefficient
                .unwrap_or(0.0);
            let has_peak_rows = largest_coeff > 0.0 && !zone_gen_indices.is_empty();

            let mut emit_row = |peak_gen_local: Option<usize>, row: usize| {
                for balance_idx in &ap.balance_product_indices {
                    let Some(balance_product) = layout.products.get(*balance_idx) else {
                        continue;
                    };
                    for &j in &zone_gen_indices {
                        if let Some(col) = balance_product.gen_reserve_col(j) {
                            triplets.push(Triplet { row, col, val: 1.0 });
                        }
                    }
                    for &gi in &zone_group_indices {
                        if let Some(col) = balance_product.dl_group_reserve_col(gi) {
                            triplets.push(Triplet { row, col, val: 1.0 });
                        }
                    }
                }
                for &k in &zone_dl_indices {
                    if served_dl_coeff > 0.0 {
                        triplets.push(Triplet {
                            row,
                            col: dl_offset + k,
                            val: -served_dl_coeff,
                        });
                    }
                }
                if let Some(j) = peak_gen_local {
                    triplets.push(Triplet {
                        row,
                        col: pg_offset + j,
                        val: -largest_coeff,
                    });
                }
                triplets.push(Triplet {
                    row,
                    col: ap.zonal_slack_offset + zi,
                    val: 1.0,
                });
                row_lower[row - row_base] = zonal_rhs;
                row_upper[row - row_base] = f64::INFINITY;
            };

            if has_peak_rows {
                for &peak_gen_local in &zone_gen_indices {
                    emit_row(Some(peak_gen_local), row_cursor);
                    row_cursor += 1;
                }
            } else {
                emit_row(None, row_cursor);
                row_cursor += 1;
            }
        }
    }

    // --- Ramp sharing constraints ---
    if layout.n_ramp_sharing_rows > 0 {
        let alpha = ctx.ramp_sharing.sharing_ratio;
        let coeff = 1.0 - alpha;

        let has_up = layout
            .products
            .iter()
            .any(|ap| ap.product.direction == ReserveDirection::Up);
        let has_down = layout
            .products
            .iter()
            .any(|ap| ap.product.direction == ReserveDirection::Down);

        if has_up {
            for (j, &gi) in ctx.gen_indices.iter().enumerate() {
                let row = row_cursor;
                let g = &ctx.network.generators[gi];

                triplets.push(Triplet {
                    row,
                    col: pg_offset + j,
                    val: 1.0,
                });

                for ap in &layout.products {
                    if ap.product.direction == ReserveDirection::Up {
                        if let Some(col) = ap.gen_reserve_col(j) {
                            triplets.push(Triplet {
                                row,
                                col,
                                val: coeff,
                            });
                        }
                    }
                }

                row_lower[row - row_base] = f64::NEG_INFINITY;
                row_upper[row - row_base] = if let Some(prev_pg_mw) = ctx.prev_dispatch_at(j) {
                    let ramp_up_mw = g
                        .ramp_up_mw_per_min()
                        .map(|r| r * 60.0 * ctx.dt_hours)
                        .unwrap_or(f64::INFINITY);
                    (prev_pg_mw + ramp_up_mw) / base
                } else {
                    f64::INFINITY
                };

                row_cursor += 1;
            }
        }

        if has_down {
            for (j, &gi) in ctx.gen_indices.iter().enumerate() {
                let row = row_cursor;
                let g = &ctx.network.generators[gi];

                triplets.push(Triplet {
                    row,
                    col: pg_offset + j,
                    val: -1.0,
                });

                for ap in &layout.products {
                    if ap.product.direction == ReserveDirection::Down {
                        if let Some(col) = ap.gen_reserve_col(j) {
                            triplets.push(Triplet {
                                row,
                                col,
                                val: coeff,
                            });
                        }
                    }
                }

                row_lower[row - row_base] = f64::NEG_INFINITY;
                row_upper[row - row_base] = if let Some(prev_pg_mw) = ctx.prev_dispatch_at(j) {
                    let ramp_dn_mw = g
                        .ramp_down_mw_per_min()
                        .map(|r| r * 60.0 * ctx.dt_hours)
                        .unwrap_or(f64::INFINITY);
                    (-prev_pg_mw + ramp_dn_mw) / base
                } else {
                    f64::INFINITY
                };

                row_cursor += 1;
            }
        }
    }

    (triplets, row_lower, row_upper)
}

// ---------------------------------------------------------------------------
// Solution extraction
// ---------------------------------------------------------------------------

/// Extracted reserve results from the LP solution.
#[derive(Clone, Debug)]
pub struct ReserveResults {
    /// Per-product per-generator awards (MW). Key: product_id.
    pub awards: HashMap<String, Vec<f64>>,
    /// Per-product per-DL awards (MW). Key: product_id.
    pub dl_awards: HashMap<String, Vec<f64>>,
    /// Per-product clearing price ($/MWh). Key: product_id.
    pub prices: HashMap<String, f64>,
    /// Per-product total provided (MW). Key: product_id.
    pub provided: HashMap<String, f64>,
    /// Per-product unmet system requirement (MW). Key: product_id.
    pub shortfall: HashMap<String, f64>,
    /// Per-(zone_id:product_id) clearing prices. Key: "zone_id:product_id".
    pub zonal_prices: HashMap<String, f64>,
    /// Per-(zone_id:product_id) unmet zonal requirement (MW).
    pub zonal_shortfall: HashMap<String, f64>,
}

/// Extract reserve results from LP solution.
///
/// `row_dual` is optional: when `None`, award quantities, provided, and
/// shortfall are still computed from the primal solution, but market
/// prices and zonal prices default to `0.0` without being read. Callers
/// that don't need prices (e.g. the skip-repricing path after a MIP
/// solve) can pass `None` to avoid allocating and passing a zero-filled
/// `Vec<f64>` of `n_row` length.
#[allow(clippy::too_many_arguments)]
pub fn extract_results(
    layout: &ReserveLpLayout,
    sol_x: &[f64],
    row_dual: Option<&[f64]>,
    row_base: usize,
    n_gen: usize,
    _n_storage: usize,
    n_dl: usize,
    base: f64,
) -> ReserveResults {
    let mut awards = HashMap::new();
    let mut dl_awards = HashMap::new();
    let mut prices = HashMap::new();
    let mut provided = HashMap::new();
    let mut shortfall = HashMap::new();
    let mut zonal_prices = HashMap::new();
    let mut zonal_shortfall = HashMap::new();

    let mut row_cursor = row_base;

    row_cursor += layout.n_cross_headroom_rows
        + layout.n_cross_footroom_rows
        + layout.n_dl_cross_headroom_rows
        + layout.n_dl_cross_footroom_rows
        + layout.n_shared_limit_rows
        + layout.n_dl_shared_limit_rows;

    for ap in &layout.products {
        // Non-participating gens have no column and implicitly 0 award.
        // Fill a dense per-gen vector so downstream consumers (keyed by
        // gen index) don't need to know about sparse layout.
        let gen_mw: Vec<f64> = (0..n_gen)
            .map(|j| match ap.gen_reserve_col(j) {
                Some(col) => sol_x[col] * base,
                None => 0.0,
            })
            .collect();
        let gen_total: f64 = gen_mw.iter().sum();

        // DR reserve awards — the LP carries one award per consumer
        // GROUP. We publish a dense per-block vector (keyed by DL
        // index) for downstream consumers by splitting each group's
        // total award across its member blocks. All members of a
        // group share the same `cost_per_mwh` for a given product
        // (the adapter pro-rates the consumer's offer across blocks
        // by size, keeping a single per-MWh rate), so any partition
        // that sums to the group award preserves total reserve cost
        // exactly. We use uniform pro-rata: the group award is split
        // equally across member blocks. For singleton groups this is
        // identical to the legacy per-block read.
        let mut dl_mw: Vec<f64> = vec![0.0; n_dl];
        for (gi, group) in layout.dl_consumer_groups.iter().enumerate() {
            let Some(col) = ap.dl_group_reserve_col(gi) else {
                continue;
            };
            let group_award_mw = sol_x[col] * base;
            let shares = prorata_group_award_uniform(group, group_award_mw);
            for (offset, &k) in group.member_dl_indices.iter().enumerate() {
                dl_mw[k] = shares[offset];
            }
        }
        let dl_total: f64 = dl_mw.iter().sum();

        provided.insert(ap.product.id.clone(), gen_total + dl_total);
        awards.insert(ap.product.id.clone(), gen_mw);
        if n_dl > 0 {
            dl_awards.insert(ap.product.id.clone(), dl_mw);
        }

        // Skip energy coupling rows — counts match what
        // `build_layout` emits: one row per participating gen /
        // group, not per n_gen / n_dl.
        match ap.product.energy_coupling {
            EnergyCoupling::Headroom | EnergyCoupling::Footroom => {
                row_cursor += ap.gen_participation.len();
            }
            EnergyCoupling::None => {}
        }
        match dl_energy_coupling(&ap.product) {
            EnergyCoupling::Headroom | EnergyCoupling::Footroom => {
                row_cursor += ap.dl_group_participation.len();
            }
            EnergyCoupling::None => {}
        }

        // System requirement rows are modeled as lower bounds
        // (`sum awards + slack >= requirement`). The LP backend reports active
        // lower-bound rows with a negative dual, so publish the flipped sign as
        // the market scarcity price.
        if ap.system_balance_cap_mw > 0.0 {
            let price = match row_dual {
                Some(d) => -d[row_cursor] / base,
                None => 0.0,
            };
            prices.insert(ap.product.id.clone(), price);
            let unmet_mw: f64 = (0..ap.n_penalty_slacks)
                .map(|i| sol_x[ap.slack_offset + i] * base)
                .sum();
            shortfall.insert(ap.product.id.clone(), unmet_mw);
            row_cursor += 1;
        } else {
            prices.insert(ap.product.id.clone(), 0.0);
            shortfall.insert(ap.product.id.clone(), 0.0);
        }

        // Zonal prices
        for (zi, req) in ap.zonal_reqs.iter().enumerate() {
            let price = match row_dual {
                Some(d) => {
                    let zonal_dual = (0..req.row_count())
                        .map(|offset| d[row_cursor + offset])
                        .sum::<f64>();
                    -zonal_dual / base
                }
                None => 0.0,
            };
            let key = format!("{}:{}", req.zone_id, ap.product.id);
            zonal_prices.insert(key.clone(), price);
            zonal_shortfall.insert(key, sol_x[ap.zonal_slack_offset + zi] * base);
            row_cursor += req.row_count();
        }
    }

    ReserveResults {
        awards,
        dl_awards,
        prices,
        provided,
        shortfall,
        zonal_prices,
        zonal_shortfall,
    }
}

/// Extract binding reserve-coupling diagnostics from reserve rows.
///
/// These rows are not market-clearing requirements themselves, but their duals can
/// materially affect energy pricing. Exposing them makes hidden reserve-opportunity-cost
/// paths visible in SCED/SCUC diagnostics.
pub fn extract_constraint_results(
    layout: &ReserveLpLayout,
    row_dual: &[f64],
    row_base: usize,
    ctx: &ReserveLpCtx,
    dual_tol: f64,
) -> Vec<RawConstraintPeriodResult> {
    let n_gen = ctx.gen_indices.len();
    let base = ctx.base;
    let mut results = Vec::new();
    let mut row_cursor = row_base;

    // Resolve the canonical resource_id for a DL consumer group (first
    // member's resource_id). Returns `None` only if the group has no
    // members, which should never happen.
    let group_resource_id = |gi: usize| -> Option<&str> {
        layout
            .dl_consumer_groups
            .get(gi)
            .and_then(|g| g.member_dl_indices.first().copied())
            .and_then(|m| ctx.dl_list.get(m))
            .map(|dl| dl.resource_id.as_str())
    };

    // Cross-headroom rows are emitted only for gens that participate in
    // ≥1 Headroom product when there are ≥2 Headroom products (Phase 4).
    if layout.n_cross_headroom_rows > 0 {
        let mut local = 0usize;
        for (j, &gi) in ctx.gen_indices.iter().enumerate() {
            let participates = layout.products.iter().any(|ap| {
                ap.product.energy_coupling == EnergyCoupling::Headroom
                    && ap.gen_col_local.get(j).copied().flatten().is_some()
            });
            if !participates {
                continue;
            }
            let shadow_price = -row_dual[row_cursor + local] / base;
            if shadow_price.abs() > dual_tol {
                results.push(RawConstraintPeriodResult {
                    constraint_id: format!(
                        "reserve:coupling:aggregate:headroom:{}",
                        ctx.network.generators[gi].id
                    ),
                    kind: ConstraintKind::ReserveCoupling,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(shadow_price),
                    ..Default::default()
                });
            }
            local += 1;
        }
        row_cursor += local;
    }

    if layout.n_cross_footroom_rows > 0 {
        let mut local = 0usize;
        for (j, &gi) in ctx.gen_indices.iter().enumerate() {
            let participates = layout.products.iter().any(|ap| {
                ap.product.energy_coupling == EnergyCoupling::Footroom
                    && ap.gen_col_local.get(j).copied().flatten().is_some()
            });
            if !participates {
                continue;
            }
            let shadow_price = -row_dual[row_cursor + local] / base;
            if shadow_price.abs() > dual_tol {
                results.push(RawConstraintPeriodResult {
                    constraint_id: format!(
                        "reserve:coupling:aggregate:footroom:{}",
                        ctx.network.generators[gi].id
                    ),
                    kind: ConstraintKind::ReserveCoupling,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(shadow_price),
                    ..Default::default()
                });
            }
            local += 1;
        }
        row_cursor += local;
    }

    if layout.n_dl_cross_headroom_rows > 0 {
        let mut local = 0usize;
        for gi in 0..layout.dl_consumer_groups.len() {
            let participates = layout.products.iter().any(|ap| {
                dl_energy_coupling(&ap.product) == EnergyCoupling::Headroom
                    && ap.dl_group_col_local.get(gi).copied().flatten().is_some()
            });
            if !participates {
                continue;
            }
            let shadow_price = -row_dual[row_cursor + local] / base;
            if shadow_price.abs() > dual_tol {
                if let Some(rid) = group_resource_id(gi) {
                    results.push(RawConstraintPeriodResult {
                        constraint_id: format!("reserve:coupling:aggregate:headroom:{}", rid),
                        kind: ConstraintKind::ReserveCoupling,
                        scope: ConstraintScope::Resource,
                        shadow_price: Some(shadow_price),
                        ..Default::default()
                    });
                }
            }
            local += 1;
        }
        row_cursor += local;
    }

    if layout.n_dl_cross_footroom_rows > 0 {
        let mut local = 0usize;
        for gi in 0..layout.dl_consumer_groups.len() {
            let participates = layout.products.iter().any(|ap| {
                dl_energy_coupling(&ap.product) == EnergyCoupling::Footroom
                    && ap.dl_group_col_local.get(gi).copied().flatten().is_some()
            });
            if !participates {
                continue;
            }
            let shadow_price = -row_dual[row_cursor + local] / base;
            if shadow_price.abs() > dual_tol {
                if let Some(rid) = group_resource_id(gi) {
                    results.push(RawConstraintPeriodResult {
                        constraint_id: format!("reserve:coupling:aggregate:footroom:{}", rid),
                        kind: ConstraintKind::ReserveCoupling,
                        scope: ConstraintScope::Resource,
                        shadow_price: Some(shadow_price),
                        ..Default::default()
                    });
                }
            }
            local += 1;
        }
        row_cursor += local;
    }

    // Shared-limit rows (Phase 4): per product, emit one row per gen /
    // group where either the owning product or an overlapping shared
    // product has a reserve column for that resource.
    for ap in &layout.products {
        if ap.product.shared_limit_products.is_empty() {
            continue;
        }
        let mut local = 0usize;
        for (j, &gi) in ctx.gen_indices.iter().enumerate() {
            let owns = ap.gen_col_local.get(j).copied().flatten().is_some();
            let shared = ap.product.shared_limit_products.iter().any(|shared_id| {
                layout.products.iter().any(|sp| {
                    sp.product.id == *shared_id
                        && sp.gen_col_local.get(j).copied().flatten().is_some()
                })
            });
            if !(owns || shared) {
                continue;
            }
            let shadow_price = -row_dual[row_cursor + local] / base;
            if shadow_price.abs() > dual_tol {
                results.push(RawConstraintPeriodResult {
                    constraint_id: format!(
                        "reserve:shared_limit:{}:{}",
                        ap.product.id, ctx.network.generators[gi].id
                    ),
                    kind: ConstraintKind::ReserveCoupling,
                    scope: ConstraintScope::Resource,
                    shadow_price: Some(shadow_price),
                    ..Default::default()
                });
            }
            local += 1;
        }
        row_cursor += local;
    }

    for ap in &layout.products {
        if ap.product.shared_limit_products.is_empty() {
            continue;
        }
        let mut local = 0usize;
        for gi in 0..layout.dl_consumer_groups.len() {
            let owns = ap.dl_group_col_local.get(gi).copied().flatten().is_some();
            let shared = ap.product.shared_limit_products.iter().any(|shared_id| {
                layout.products.iter().any(|sp| {
                    sp.product.id == *shared_id
                        && sp.dl_group_col_local.get(gi).copied().flatten().is_some()
                })
            });
            if !(owns || shared) {
                continue;
            }
            let shadow_price = -row_dual[row_cursor + local] / base;
            if shadow_price.abs() > dual_tol {
                if let Some(rid) = group_resource_id(gi) {
                    results.push(RawConstraintPeriodResult {
                        constraint_id: format!("reserve:shared_limit:{}:{}", ap.product.id, rid),
                        kind: ConstraintKind::ReserveCoupling,
                        scope: ConstraintScope::Resource,
                        shadow_price: Some(shadow_price),
                        ..Default::default()
                    });
                }
            }
            local += 1;
        }
        row_cursor += local;
    }

    // Per-product energy-coupling rows (Phase 4): one row per
    // participating gen / DL group for this specific product.
    for ap in &layout.products {
        match ap.product.energy_coupling {
            EnergyCoupling::Headroom => {
                for (local, &j) in ap.gen_participation.iter().enumerate() {
                    let gi = ctx.gen_indices[j];
                    let shadow_price = -row_dual[row_cursor + local] / base;
                    if shadow_price.abs() > dual_tol {
                        results.push(RawConstraintPeriodResult {
                            constraint_id: format!(
                                "reserve:coupling:{}:headroom:{}",
                                ap.product.id, ctx.network.generators[gi].id
                            ),
                            kind: ConstraintKind::ReserveCoupling,
                            scope: ConstraintScope::Resource,
                            shadow_price: Some(shadow_price),
                            ..Default::default()
                        });
                    }
                }
                row_cursor += ap.gen_participation.len();
            }
            EnergyCoupling::Footroom => {
                for (local, &j) in ap.gen_participation.iter().enumerate() {
                    let gi = ctx.gen_indices[j];
                    let shadow_price = -row_dual[row_cursor + local] / base;
                    if shadow_price.abs() > dual_tol {
                        results.push(RawConstraintPeriodResult {
                            constraint_id: format!(
                                "reserve:coupling:{}:footroom:{}",
                                ap.product.id, ctx.network.generators[gi].id
                            ),
                            kind: ConstraintKind::ReserveCoupling,
                            scope: ConstraintScope::Resource,
                            shadow_price: Some(shadow_price),
                            ..Default::default()
                        });
                    }
                }
                row_cursor += ap.gen_participation.len();
            }
            EnergyCoupling::None => {}
        }
        match dl_energy_coupling(&ap.product) {
            EnergyCoupling::Headroom => {
                for (local, &gi) in ap.dl_group_participation.iter().enumerate() {
                    let shadow_price = -row_dual[row_cursor + local] / base;
                    if shadow_price.abs() > dual_tol {
                        if let Some(rid) = group_resource_id(gi) {
                            results.push(RawConstraintPeriodResult {
                                constraint_id: format!(
                                    "reserve:coupling:{}:headroom:{}",
                                    ap.product.id, rid
                                ),
                                kind: ConstraintKind::ReserveCoupling,
                                scope: ConstraintScope::Resource,
                                shadow_price: Some(shadow_price),
                                ..Default::default()
                            });
                        }
                    }
                }
                row_cursor += ap.dl_group_participation.len();
            }
            EnergyCoupling::Footroom => {
                for (local, &gi) in ap.dl_group_participation.iter().enumerate() {
                    let shadow_price = -row_dual[row_cursor + local] / base;
                    if shadow_price.abs() > dual_tol {
                        if let Some(rid) = group_resource_id(gi) {
                            results.push(RawConstraintPeriodResult {
                                constraint_id: format!(
                                    "reserve:coupling:{}:footroom:{}",
                                    ap.product.id, rid
                                ),
                                kind: ConstraintKind::ReserveCoupling,
                                scope: ConstraintScope::Resource,
                                shadow_price: Some(shadow_price),
                                ..Default::default()
                            });
                        }
                    }
                }
                row_cursor += ap.dl_group_participation.len();
            }
            EnergyCoupling::None => {}
        }

        if ap.system_balance_cap_mw > 0.0 {
            row_cursor += 1;
        }
        row_cursor += ap
            .zonal_reqs
            .iter()
            .map(ActiveZonalRequirement::row_count)
            .sum::<usize>();
    }

    if layout.n_ramp_sharing_rows > 0 {
        let has_up = layout
            .products
            .iter()
            .any(|ap| ap.product.direction == ReserveDirection::Up);
        let has_down = layout
            .products
            .iter()
            .any(|ap| ap.product.direction == ReserveDirection::Down);

        if has_up {
            for (j, &gi) in ctx.gen_indices.iter().enumerate() {
                let shadow_price = -row_dual[row_cursor + j] / base;
                if shadow_price.abs() > dual_tol {
                    results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "reserve:ramp_sharing:up:{}",
                            ctx.network.generators[gi].id
                        ),
                        kind: ConstraintKind::ReserveRampSharing,
                        scope: ConstraintScope::Resource,
                        shadow_price: Some(shadow_price),
                        ..Default::default()
                    });
                }
            }
            row_cursor += n_gen;
        }

        if has_down {
            for (j, &gi) in ctx.gen_indices.iter().enumerate() {
                let shadow_price = -row_dual[row_cursor + j] / base;
                if shadow_price.abs() > dual_tol {
                    results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "reserve:ramp_sharing:down:{}",
                            ctx.network.generators[gi].id
                        ),
                        kind: ConstraintKind::ReserveRampSharing,
                        scope: ConstraintScope::Resource,
                        shadow_price: Some(shadow_price),
                        ..Default::default()
                    });
                }
            }
        }
    }

    results
}

// ---------------------------------------------------------------------------
// Reserve product helpers
// ---------------------------------------------------------------------------

/// Get reserve products, system requirements, and zonal requirements from options.
///
/// When `reserve_products` is non-empty, uses them directly. Otherwise
/// populates ERCOT default product definitions with the penalty curve
/// from `options.penalty_config.reserve`.
pub fn resolve_reserve_config(
    spec: &DispatchProblemSpec<'_>,
) -> (
    Vec<ReserveProduct>,
    Vec<SystemReserveRequirement>,
    Vec<ZonalReserveRequirement>,
) {
    if !spec.reserve_products.is_empty() {
        return (
            spec.reserve_products.to_vec(),
            spec.system_reserve_requirements.to_vec(),
            spec.zonal_reserve_requirements.to_vec(),
        );
    }

    let mut products = ReserveProduct::ercot_defaults();
    let reserve_curve = spec.reserve_penalty_curve.clone();
    for p in &mut products {
        p.demand_curve = reserve_curve.clone();
    }

    (
        products,
        spec.system_reserve_requirements.to_vec(),
        spec.zonal_reserve_requirements.to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::market::PenaltyCurve;

    fn make_test_products() -> Vec<ReserveProduct> {
        vec![
            ReserveProduct {
                id: "spin".into(),
                name: "Spin".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: surge_network::market::QualificationRule::Synchronized,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
            ReserveProduct {
                id: "reg_dn".into(),
                name: "Reg Down".into(),
                direction: ReserveDirection::Down,
                deploy_secs: 300.0,
                qualification: surge_network::market::QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Footroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
        ]
    }

    fn make_generator_bus_numbers(n_gen: usize) -> Vec<u32> {
        (1..=n_gen as u32).collect()
    }

    /// Dense gen-participation vector for tests that pre-date Phase 1
    /// sparsification: every generator participates in every product.
    /// Reproduces the pre-Phase-1 layout exactly.
    fn dense_gen_participation(products: &[ReserveProduct], n_gen: usize) -> Vec<Vec<usize>> {
        products.iter().map(|_| (0..n_gen).collect()).collect()
    }

    /// Dense DL-participation vector — analog of `dense_gen_participation`
    /// for tests written before Phase 2 sparsification.
    fn dense_dl_participation(products: &[ReserveProduct], n_dl: usize) -> Vec<Vec<usize>> {
        products.iter().map(|_| (0..n_dl).collect()).collect()
    }

    #[test]
    fn test_layout_no_products() {
        let sharing = RampSharingConfig::default();
        let generator_area = vec![0; 10];
        let layout = build_layout(
            &[],
            &[],
            &[],
            &sharing,
            &generator_area,
            &make_generator_bus_numbers(10),
            10,
            0,
            0,
            100,
            false,
            &[],
            &[],
            &[],
        );
        assert_eq!(layout.products.len(), 0);
        assert_eq!(layout.n_reserve_vars, 0);
        assert_eq!(layout.n_reserve_rows, 0);
    }

    #[test]
    fn test_layout_one_product() {
        let products = vec![make_test_products()[0].clone()];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 500.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();
        let generator_area = vec![0; 5];
        let participation = dense_gen_participation(&products, 5);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &generator_area,
            &make_generator_bus_numbers(5),
            5,
            0,
            0,
            100,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );
        assert_eq!(layout.products.len(), 1);
        assert_eq!(layout.n_reserve_vars, 6); // 5 gen + 1 slack
        assert_eq!(layout.n_reserve_rows, 6); // 5 headroom + 1 sys req
    }

    #[test]
    fn test_layout_with_dl() {
        let products = vec![make_test_products()[0].clone()];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 500.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();

        // 5 gen + 2 DL + 1 slack = 8 vars
        let generator_area = vec![0; 5];
        let participation = dense_gen_participation(&products, 5);
        // Two singleton DL groups (one block per group) — reproduces
        // the pre-Phase-3 per-block layout.
        let dl_groups: Vec<DlConsumerGroup> = (0..2)
            .map(|k| DlConsumerGroup {
                key: None,
                member_dl_indices: vec![k],
                canonical_bus: k as u32 + 1,
            })
            .collect();
        let dl_group_participation = dense_dl_participation(&products, 2);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &generator_area,
            &make_generator_bus_numbers(5),
            5,
            0,
            2,
            100,
            false,
            &participation,
            &dl_groups,
            &dl_group_participation,
        );
        assert_eq!(layout.products.len(), 1);
        assert_eq!(layout.n_reserve_vars, 8); // 5 gen + 2 DL + 1 slack
        // 5 gen headroom (one per generator) + 2 DL headroom (one per
        // dispatchable load — spin has `energy_coupling = Headroom` and
        // `dispatchable_load_energy_coupling = None`, so
        // `dl_energy_coupling` defaults to the producer coupling and
        // emits a DL-side row per load) + 1 sys req slack row = 8 rows.
        assert_eq!(layout.n_reserve_rows, 8);
    }

    #[test]
    fn test_layout_with_storage() {
        let products = vec![make_test_products()[0].clone()];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 500.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();

        // n_storage=2 passed but storage participates through gen_var_offset, not
        // separate reserve vars — so n_reserve_vars is still 5 gen + 1 slack = 6.
        let generator_area = vec![0; 5];
        let participation = dense_gen_participation(&products, 5);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &generator_area,
            &make_generator_bus_numbers(5),
            5,
            2,
            0,
            100,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );
        assert_eq!(layout.products.len(), 1);
        assert_eq!(layout.n_reserve_vars, 6); // 5 gen + 1 slack (no separate storage vars)
    }

    #[test]
    fn test_layout_with_zonals() {
        let products = vec![make_test_products()[0].clone()];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 500.0,
            per_period_mw: None,
        }];
        let zonal_reqs = vec![
            ZonalReserveRequirement {
                zone_id: 1,
                product_id: "spin".into(),
                requirement_mw: 200.0,
                per_period_mw: None,
                shortfall_cost_per_unit: None,
                served_dispatchable_load_coefficient: None,
                largest_generator_dispatch_coefficient: None,
                participant_bus_numbers: None,
            },
            ZonalReserveRequirement {
                zone_id: 2,
                product_id: "spin".into(),
                requirement_mw: 100.0,
                per_period_mw: None,
                shortfall_cost_per_unit: None,
                served_dispatchable_load_coefficient: None,
                largest_generator_dispatch_coefficient: None,
                participant_bus_numbers: None,
            },
        ];
        let sharing = RampSharingConfig::default();

        let participation = dense_gen_participation(&products, 3);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &zonal_reqs,
            &sharing,
            &[1, 1, 2],
            &[1, 2, 3],
            3,
            0,
            0,
            50,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );
        assert_eq!(layout.products.len(), 1);
        let ap = &layout.products[0];
        assert_eq!(ap.n_zonal, 2);
        assert_eq!(layout.n_reserve_vars, 6); // 3 gen + 1 sys slack + 2 zonal slacks
        assert_eq!(layout.n_reserve_rows, 6); // 3 headroom + 1 sys + 2 zonal
    }

    #[test]
    fn test_explicit_participant_zonal_peak_rows_match_participating_generators() {
        let products = vec![make_test_products()[0].clone()];
        let zonal_reqs = vec![ZonalReserveRequirement {
            zone_id: 7,
            product_id: "spin".into(),
            requirement_mw: 100.0,
            per_period_mw: None,
            shortfall_cost_per_unit: None,
            served_dispatchable_load_coefficient: None,
            largest_generator_dispatch_coefficient: Some(1.0),
            participant_bus_numbers: Some(vec![101, 103]),
        }];
        let sharing = RampSharingConfig::default();

        let participation = dense_gen_participation(&products, 3);
        let layout = build_layout(
            &products,
            &[],
            &zonal_reqs,
            &sharing,
            &[0, 0, 0],
            &[101, 102, 103],
            3,
            0,
            0,
            50,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );

        let zonal_req = &layout.products[0].zonal_reqs[0];
        assert_eq!(zonal_req.largest_generator_row_count, 2);
        assert_eq!(zonal_req.row_count(), 2);
        assert_eq!(layout.n_reserve_rows, 5); // 3 headroom + 2 zonal
    }

    #[test]
    fn test_balance_products_only_expand_lhs_substitution_ladder() {
        let products = vec![
            ReserveProduct {
                id: "reg_up".into(),
                name: "Reg Up".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 300.0,
                qualification: surge_network::market::QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
            ReserveProduct {
                id: "syn".into(),
                name: "Syn".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: surge_network::market::QualificationRule::Synchronized,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: vec!["reg_up".into()],
                balance_products: vec!["reg_up".into()],
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
            ReserveProduct {
                id: "nsyn".into(),
                name: "Non-Syn".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: surge_network::market::QualificationRule::OfflineQuickStart,
                energy_coupling: EnergyCoupling::None,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: vec!["reg_up".into(), "syn".into()],
                balance_products: vec!["reg_up".into(), "syn".into()],
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
        ];
        let sys_reqs = vec![
            SystemReserveRequirement {
                product_id: "reg_up".into(),
                requirement_mw: 5.0,
                per_period_mw: None,
            },
            SystemReserveRequirement {
                product_id: "syn".into(),
                requirement_mw: 10.0,
                per_period_mw: None,
            },
            SystemReserveRequirement {
                product_id: "nsyn".into(),
                requirement_mw: 30.0,
                per_period_mw: None,
            },
        ];
        let zonal_reqs = vec![
            ZonalReserveRequirement {
                zone_id: 1,
                product_id: "reg_up".into(),
                requirement_mw: 5.0,
                per_period_mw: None,
                shortfall_cost_per_unit: None,
                served_dispatchable_load_coefficient: Some(0.01),
                largest_generator_dispatch_coefficient: Some(0.1),
                participant_bus_numbers: None,
            },
            ZonalReserveRequirement {
                zone_id: 1,
                product_id: "syn".into(),
                requirement_mw: 10.0,
                per_period_mw: None,
                shortfall_cost_per_unit: None,
                served_dispatchable_load_coefficient: Some(0.02),
                largest_generator_dispatch_coefficient: Some(0.2),
                participant_bus_numbers: None,
            },
            ZonalReserveRequirement {
                zone_id: 1,
                product_id: "nsyn".into(),
                requirement_mw: 30.0,
                per_period_mw: None,
                shortfall_cost_per_unit: None,
                served_dispatchable_load_coefficient: Some(0.03),
                largest_generator_dispatch_coefficient: Some(0.3),
                participant_bus_numbers: None,
            },
        ];
        let sharing = RampSharingConfig::default();
        let generator_area = vec![1; 2];

        let participation = dense_gen_participation(&products, 2);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &zonal_reqs,
            &sharing,
            &generator_area,
            &make_generator_bus_numbers(2),
            2,
            0,
            0,
            0,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );

        let syn = layout
            .products
            .iter()
            .find(|ap| ap.product.id == "syn")
            .expect("syn product");
        let nsyn = layout
            .products
            .iter()
            .find(|ap| ap.product.id == "nsyn")
            .expect("nsyn product");

        assert_eq!(syn.balance_product_indices.len(), 2);
        assert_eq!(syn.system_balance_req_indices, vec![1]);
        assert!((syn.system_balance_cap_mw - 10.0).abs() < 1e-9);
        assert_eq!(syn.zonal_reqs[0].balance_req_indices, vec![1]);
        assert!((syn.zonal_reqs[0].balance_cap_mw - 10.0).abs() < 1e-9);
        assert_eq!(
            syn.zonal_reqs[0].balance_served_dispatchable_load_coefficient,
            Some(0.02)
        );
        assert_eq!(
            syn.zonal_reqs[0].balance_largest_generator_dispatch_coefficient,
            Some(0.2)
        );

        assert_eq!(nsyn.balance_product_indices.len(), 3);
        assert_eq!(nsyn.system_balance_req_indices, vec![2]);
        assert!((nsyn.system_balance_cap_mw - 30.0).abs() < 1e-9);
        assert_eq!(nsyn.zonal_reqs[0].balance_req_indices, vec![2]);
        assert!((nsyn.zonal_reqs[0].balance_cap_mw - 30.0).abs() < 1e-9);
        assert_eq!(
            nsyn.zonal_reqs[0].balance_served_dispatchable_load_coefficient,
            Some(0.03)
        );
        assert_eq!(
            nsyn.zonal_reqs[0].balance_largest_generator_dispatch_coefficient,
            Some(0.3)
        );
    }

    /// Proves that non-block-mode reserve variable bounds enforce ramp rate × deploy time.
    /// The bug report claims `r[g] + Pg[g] ≤ Pmax[g]` is the only constraint and that
    /// deploy_secs is ignored. In fact, `set_bounds()` applies `ramp_limited_mw()` which
    /// computes `ramp_rate × deploy_min` as a variable upper bound on r[g], so the LP
    /// sees `r[g] ≤ 30 MW` (3 MW/min × 10 min), not `r[g] ≤ 400 MW` (pmax - pg).
    #[test]
    fn test_nonblock_reserve_bound_respects_ramp_rate() {
        use surge_network::market::reserve::{QualificationRule, ReserveOffer};
        use surge_network::network::generator::Generator;

        // 500 MW coal unit, pmin=100, ramp = 3 MW/min (flat curve)
        let g = Generator {
            pmax: 500.0,
            pmin: 100.0,
            p: 100.0,
            in_service: true,
            ramping: Some(surge_network::network::RampingParams {
                ramp_up_curve: vec![(100.0, 3.0)], // 3 MW/min everywhere
                ..Default::default()
            }),
            market: Some(surge_network::network::MarketParams {
                reserve_offers: vec![ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: 999.0, // large offer cap — should NOT be the binding limit
                    cost_per_mwh: 5.0,
                }],
                ..Default::default()
            }),
            ..Generator::default()
        };

        let mut network = Network::default();
        network.generators.push(g);

        // 10-minute spinning reserve (deploy_secs = 600)
        let products = vec![ReserveProduct {
            id: "spin".into(),
            name: "Spin".into(),
            direction: ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        }];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 500.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();
        let gen_indices = vec![0usize];
        let gen_area = vec![0usize];

        let participation = dense_gen_participation(&products, 1);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &gen_area,
            &make_generator_bus_numbers(1),
            1,
            0,
            0,
            0,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );

        let sharing = RampSharingConfig::default();
        let options = crate::legacy::DispatchOptions::default();
        let spec = DispatchProblemSpec::from_options(&options);
        let ctx = ReserveLpCtx {
            spec: &spec,
            period: 0,
            network: &network,
            gen_indices: &gen_indices,
            committed: vec![true],
            generator_area: &gen_area,
            prev_dispatch_mw: None,
            prev_dispatch_mask: None,
            dt_hours: 1.0,
            base: 100.0,
            ramp_sharing: &sharing,
            dl_list: vec![],
            dl_indices: vec![],
            dl_pmax_pu: vec![],
            dl_area: vec![],
        };

        let n_vars = layout.n_reserve_vars;
        let mut col_lower = vec![0.0; n_vars];
        let mut col_upper = vec![f64::INFINITY; n_vars];
        set_bounds(&layout, &mut col_lower, &mut col_upper, &ctx);

        // The reserve variable is at gen_var_offset + 0
        let r_upper_pu = col_upper[layout.products[0].gen_var_offset];
        let r_upper_mw = r_upper_pu * 100.0; // base = 100

        // Ramp-limited: 3 MW/min × 10 min = 30 MW
        // Headroom alone would allow pmax - pmin = 400 MW
        // The variable bound must be the binding constraint at 30 MW
        assert!(
            (r_upper_mw - 30.0).abs() < 1e-6,
            "Expected 30 MW ramp limit, got {} MW — deploy_secs IS enforced via set_bounds()",
            r_upper_mw,
        );
    }

    #[test]
    fn test_nonblock_reserve_bound_can_skip_deploy_ramp_limit_when_offer_is_deliverable() {
        use surge_network::market::reserve::{QualificationRule, ReserveOffer};
        use surge_network::network::generator::Generator;

        let g = Generator {
            pmax: 500.0,
            pmin: 100.0,
            p: 100.0,
            in_service: true,
            ramping: Some(surge_network::network::RampingParams {
                ramp_up_curve: vec![(100.0, 3.0)],
                ..Default::default()
            }),
            market: Some(surge_network::network::MarketParams {
                reserve_offers: vec![ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: 20.0,
                    cost_per_mwh: 5.0,
                }],
                ..Default::default()
            }),
            ..Generator::default()
        };

        let mut network = Network::default();
        network.generators.push(g);

        let products = vec![ReserveProduct {
            id: "spin".into(),
            name: "Spin".into(),
            direction: ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: false,
            demand_curve: PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        }];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 500.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();
        let gen_indices = vec![0usize];
        let gen_area = vec![0usize];

        let participation = dense_gen_participation(&products, 1);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &gen_area,
            &make_generator_bus_numbers(1),
            1,
            0,
            0,
            0,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );

        let options = crate::legacy::DispatchOptions::default();
        let spec = DispatchProblemSpec::from_options(&options);
        let ctx = ReserveLpCtx {
            spec: &spec,
            period: 0,
            network: &network,
            gen_indices: &gen_indices,
            committed: vec![true],
            generator_area: &gen_area,
            prev_dispatch_mw: None,
            prev_dispatch_mask: None,
            dt_hours: 1.0,
            base: 100.0,
            ramp_sharing: &sharing,
            dl_list: vec![],
            dl_indices: vec![],
            dl_pmax_pu: vec![],
            dl_area: vec![],
        };

        let n_vars = layout.n_reserve_vars;
        let mut col_lower = vec![0.0; n_vars];
        let mut col_upper = vec![f64::INFINITY; n_vars];
        set_bounds(&layout, &mut col_lower, &mut col_upper, &ctx);

        let r_upper_pu = col_upper[layout.products[0].gen_var_offset];
        let r_upper_mw = r_upper_pu * 100.0;
        assert!(
            (r_upper_mw - 20.0).abs() < 1e-6,
            "Expected explicit deliverable cap to win, got {r_upper_mw} MW",
        );
    }

    #[test]
    fn test_quickstart_offline_reserve_uses_offline_deliverable_capacity() {
        use surge_network::market::reserve::{QualificationRule, ReserveOffer};
        use surge_network::network::generator::Generator;

        let g = Generator {
            pmax: 55.0,
            pmin: 22.0,
            p: 0.0,
            in_service: true,
            quick_start: true,
            ramping: Some(surge_network::network::RampingParams {
                ramp_up_curve: vec![(0.0, 1.0)],
                ..Default::default()
            }),
            market: Some(surge_network::network::MarketParams {
                reserve_offers: vec![ReserveOffer {
                    product_id: "nspin".into(),
                    capacity_mw: 37.0,
                    cost_per_mwh: 5.0,
                }],
                ..Default::default()
            }),
            ..Generator::default()
        };

        let mut network = Network::default();
        network.generators.push(g);

        let products = vec![ReserveProduct {
            id: "nspin".into(),
            name: "Non-Spin".into(),
            direction: ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: QualificationRule::QuickStart,
            energy_coupling: EnergyCoupling::None,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        }];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "nspin".into(),
            requirement_mw: 37.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();
        let gen_indices = vec![0usize];
        let gen_area = vec![0usize];

        let participation = dense_gen_participation(&products, 1);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &gen_area,
            &make_generator_bus_numbers(1),
            1,
            0,
            0,
            0,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );
        let options = crate::legacy::DispatchOptions::default();
        let spec = DispatchProblemSpec::from_options(&options);
        let ctx = ReserveLpCtx {
            spec: &spec,
            period: 0,
            network: &network,
            gen_indices: &gen_indices,
            committed: vec![false],
            generator_area: &gen_area,
            prev_dispatch_mw: None,
            prev_dispatch_mask: None,
            dt_hours: 1.0,
            base: 100.0,
            ramp_sharing: &sharing,
            dl_list: vec![],
            dl_indices: vec![],
            dl_pmax_pu: vec![],
            dl_area: vec![],
        };

        let mut col_lower = vec![0.0; layout.n_reserve_vars];
        let mut col_upper = vec![f64::INFINITY; layout.n_reserve_vars];
        set_bounds(&layout, &mut col_lower, &mut col_upper, &ctx);

        let r_upper_pu = col_upper[layout.products[0].gen_var_offset];
        let r_upper_mw = r_upper_pu * 100.0;
        assert!(
            (r_upper_mw - 37.0).abs() < 1e-6,
            "expected offline quick-start reserve to use 37 MW offer/offline capability, got {r_upper_mw}"
        );
    }

    #[test]
    fn test_quickstart_shared_limit_rows_allow_offline_awards() {
        use surge_network::market::reserve::{QualificationRule, ReserveOffer};
        use surge_network::network::generator::Generator;

        let g = Generator {
            pmax: 55.0,
            pmin: 22.0,
            p: 0.0,
            in_service: true,
            quick_start: true,
            ramping: Some(surge_network::network::RampingParams {
                ramp_up_curve: vec![(0.0, 1.0)],
                ..Default::default()
            }),
            market: Some(surge_network::network::MarketParams {
                reserve_offers: vec![
                    ReserveOffer {
                        product_id: "rru".into(),
                        capacity_mw: 37.0,
                        cost_per_mwh: 0.0,
                    },
                    ReserveOffer {
                        product_id: "reg_up".into(),
                        capacity_mw: 0.0,
                        cost_per_mwh: 0.0,
                    },
                ],
                ..Default::default()
            }),
            ..Generator::default()
        };

        let mut network = Network::default();
        network.generators.push(g);

        let products = vec![
            ReserveProduct {
                id: "reg_up".into(),
                name: "Reg Up".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 300.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
            ReserveProduct {
                id: "rru".into(),
                name: "Ramp Up".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 900.0,
                qualification: QualificationRule::QuickStart,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: vec!["reg_up".into()],
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
        ];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "rru".into(),
            requirement_mw: 37.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();
        let gen_indices = vec![0usize];
        let gen_area = vec![0usize];

        let participation = dense_gen_participation(&products, 1);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &gen_area,
            &make_generator_bus_numbers(1),
            1,
            0,
            0,
            0,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );
        let options = crate::legacy::DispatchOptions::default();
        let spec = DispatchProblemSpec::from_options(&options);
        let ctx = ReserveLpCtx {
            spec: &spec,
            period: 0,
            network: &network,
            gen_indices: &gen_indices,
            committed: vec![false],
            generator_area: &gen_area,
            prev_dispatch_mw: None,
            prev_dispatch_mask: None,
            dt_hours: 1.0,
            base: 100.0,
            ramp_sharing: &sharing,
            dl_list: vec![],
            dl_indices: vec![],
            dl_pmax_pu: vec![],
            dl_area: vec![],
        };

        let (_, _, row_upper) = build_constraints(&layout, 0, 10, 20, &ctx);
        // `build_constraints` emits rows in this order for the fixture:
        //   row 0: cross-headroom row (p_g + Σ headroom ≤ pmax) — `reg_up`
        //          and `rru` are both headroom products so
        //          `n_cross_headroom_rows = 1` per generator.
        //   row 1: shared-limit row for `rru` (rru + reg_up ≤ offer_cap).
        // The shared-limit row is the one this test exists to lock down,
        // so skip past the cross-headroom row when picking the index.
        assert_eq!(layout.n_cross_headroom_rows, 1, "fixture layout changed");
        let shared_row_upper_pu = row_upper[layout.n_cross_headroom_rows];
        let shared_row_upper_mw = shared_row_upper_pu * 100.0;
        assert!(
            (shared_row_upper_mw - 37.0).abs() < 1e-6,
            "expected QuickStart shared-limit row to preserve offline 37 MW award cap, got {shared_row_upper_mw}"
        );
    }

    #[test]
    fn test_offline_shared_limit_row_excludes_committed_only_products() {
        use surge_network::market::reserve::{QualificationRule, ReserveOffer};
        use surge_network::network::generator::Generator;

        let g = Generator {
            pmax: 55.0,
            pmin: 22.0,
            p: 22.0,
            in_service: true,
            quick_start: true,
            market: Some(surge_network::network::MarketParams {
                reserve_offers: vec![
                    ReserveOffer {
                        product_id: "reg_up".into(),
                        capacity_mw: 20.0,
                        cost_per_mwh: 5.0,
                    },
                    ReserveOffer {
                        product_id: "nsyn".into(),
                        capacity_mw: 37.0,
                        cost_per_mwh: 0.0,
                    },
                ],
                ..Default::default()
            }),
            ..Generator::default()
        };

        let mut network = Network::default();
        network.generators.push(g);

        let products = vec![
            ReserveProduct {
                id: "reg_up".into(),
                name: "Reg Up".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 300.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
            ReserveProduct {
                id: "nsyn".into(),
                name: "Non-Spin".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::OfflineQuickStart,
                energy_coupling: EnergyCoupling::None,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: vec!["reg_up".into()],
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            },
        ];
        let sys_reqs = vec![SystemReserveRequirement {
            product_id: "nsyn".into(),
            requirement_mw: 37.0,
            per_period_mw: None,
        }];
        let sharing = RampSharingConfig::default();
        let gen_indices = vec![0usize];
        let gen_area = vec![0usize];

        let participation = dense_gen_participation(&products, 1);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &gen_area,
            &make_generator_bus_numbers(1),
            1,
            0,
            0,
            0,
            false,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );
        let options = crate::legacy::DispatchOptions::default();
        let spec = DispatchProblemSpec::from_options(&options);
        let ctx = ReserveLpCtx {
            spec: &spec,
            period: 0,
            network: &network,
            gen_indices: &gen_indices,
            committed: vec![true],
            generator_area: &gen_area,
            prev_dispatch_mw: None,
            prev_dispatch_mask: None,
            dt_hours: 1.0,
            base: 100.0,
            ramp_sharing: &sharing,
            dl_list: vec![],
            dl_indices: vec![],
            dl_pmax_pu: vec![],
            dl_area: vec![],
        };

        let (triplets, _, row_upper) = build_constraints(&layout, 0, 10, 20, &ctx);
        let reg_up_col = layout.products[0].gen_var_offset;
        let nsyn_col = layout.products[1].gen_var_offset;
        let shared_row = row_upper
            .iter()
            .enumerate()
            .find_map(|(row, upper)| {
                (*upper == 0.0
                    && triplets
                        .iter()
                        .any(|triplet| triplet.row == row && triplet.col == nsyn_col))
                .then_some(row)
            })
            .expect("expected an offline-only shared-limit row for nsyn");
        let shared_cols: Vec<usize> = triplets
            .iter()
            .filter(|triplet| triplet.row == shared_row)
            .map(|triplet| triplet.col)
            .collect();

        assert_eq!(
            row_upper[shared_row], 0.0,
            "offline-only shared row should deactivate online"
        );
        assert!(
            shared_cols.contains(&nsyn_col),
            "offline row must still cap nsyn itself"
        );
        assert!(
            !shared_cols.contains(&reg_up_col),
            "offline-only shared-limit row must not pull committed-only reg_up into a committed-state zero cap",
        );
    }

    #[test]
    fn test_layout_ramp_sharing() {
        let products = make_test_products(); // up + down
        let sys_reqs = vec![
            SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 500.0,
                per_period_mw: None,
            },
            SystemReserveRequirement {
                product_id: "reg_dn".into(),
                requirement_mw: 200.0,
                per_period_mw: None,
            },
        ];
        let sharing = RampSharingConfig { sharing_ratio: 0.5 };

        let generator_area = vec![0; 3];
        let participation = dense_gen_participation(&products, 3);
        let layout = build_layout(
            &products,
            &sys_reqs,
            &[],
            &sharing,
            &generator_area,
            &make_generator_bus_numbers(3),
            3,
            0,
            0,
            50,
            true,
            &participation,
            &[],
            &dense_dl_participation(&products, 0),
        );
        assert_eq!(layout.n_ramp_sharing_rows, 6); // 3 up + 3 down
    }
}
