// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Reactive reserve plan construction for the AC-OPF NLP.
//!
//! The plan is built once, at [`super::problem::AcOpfProblem::new`], from
//! the network's `market_data.market_rules.reserve_products` (filtered to
//! `kind = Reactive`), the per-zone `zonal_requirements`, and the
//! device-level reactive capability / cost data. It packages everything
//! the NLP residual, Jacobian, objective, and bounds code needs into a
//! single flat struct that is cheap to iterate per hot-path evaluation.
//!
//! ## Variable layout assumptions
//!
//! The plan is paired with an [`AcOpfMapping`] whose layout already
//! allocates the producer and consumer q-reserve columns plus the
//! per-zone shortfall slack columns — see `AcOpfMapping::new` and its
//! `count_reactive_zone_balance_rows` helper. This module just builds
//! the *data* (participants, costs, requirements) that drive the rows
//! and objective terms anchored at those offsets.
//!
//! ## Units
//!
//! Costs are stored per-pu-hr (`cost_per_mvarh × base_mva`) so the
//! objective can multiply by `x[var]` directly without re-scaling in
//! the hot path. Requirements are stored in per-unit (`mvar / base_mva`)
//! so the row residual matches the pu basis of the q-reserve variables.

use surge_network::Network;
use surge_network::market::{
    ReserveDirection, ReserveKind, ReserveProduct, ZonalReserveRequirement,
};

use super::mapping::AcOpfMapping;
use super::pq_curve::{
    PqConstraint, PqDeviceKind, build_pq_constraints, build_pq_linear_constraints,
    build_pq_linear_constraints_consumers,
};

/// Plan data for the AC-OPF reactive reserve blocks (5B).
///
/// Every field is indexed parallel to the variable layout in
/// [`super::mapping::AcOpfMapping`]. When the network has no reactive
/// reserve products, all vectors are empty and the plan is effectively
/// a no-op.
#[derive(Debug, Clone, Default)]
pub(super) struct AcReactiveReservePlan {
    /// One entry per zonal q-reserve balance row. Up-direction rows
    /// come first (indices `0..n_up`), then down-direction rows
    /// (`n_up..n_up + n_down`). Matches the layout in
    /// `AcOpfMapping::zone_q_reserve_balance_row_offset`.
    pub zone_rows: Vec<AcZoneReserveBalance>,
    /// Per-producer reactive-up reserve cost `c^qru_j × dt` in
    /// `$/pu-hr` (indexed by local generator `j`). Zero when no offer
    /// is declared — the producer can still provide reserves (bound
    /// only by headroom), the provision is just free.
    pub producer_q_reserve_up_cost_per_pu_hr: Vec<f64>,
    /// Per-producer reactive-down reserve cost.
    pub producer_q_reserve_down_cost_per_pu_hr: Vec<f64>,
    /// Per-consumer reactive-up reserve cost.
    pub consumer_q_reserve_up_cost_per_pu_hr: Vec<f64>,
    /// Per-consumer reactive-down reserve cost.
    pub consumer_q_reserve_down_cost_per_pu_hr: Vec<f64>,
    /// Per-producer upper bound on the q^qru variable (in pu). When a
    /// producer is in `J^pqe` the bound collapses to 0 (eqs 117). When
    /// a producer has no reactive capability the bound is 0.
    pub producer_q_reserve_up_ub_pu: Vec<f64>,
    /// Per-producer upper bound on q^qrd. Same rules as `_up_ub_pu`.
    pub producer_q_reserve_down_ub_pu: Vec<f64>,
    /// Per-consumer upper bound on q^qru.
    pub consumer_q_reserve_up_ub_pu: Vec<f64>,
    /// Per-consumer upper bound on q^qrd.
    pub consumer_q_reserve_down_ub_pu: Vec<f64>,
}

/// One zonal balance row, up or down direction.
#[derive(Debug, Clone)]
pub(super) struct AcZoneReserveBalance {
    /// Up or down. Retained for introspection / debugging even though
    /// the row assembly reads `shortfall_var` and `participant_cols`
    /// directly, because the direction is implicit in the row position.
    #[allow(dead_code)]
    pub direction: ReserveDirection,
    /// Zone area id (from `Bus.area`). Retained for diagnostics.
    #[allow(dead_code)]
    pub zone_id: usize,
    /// Column indices of all per-device q-reserve variables that sum
    /// into the LHS with coefficient `+1.0`. For an up-direction row
    /// this is the union of producer/consumer q^qru variables in the
    /// zone; for a down-direction row it's the q^qrd variables.
    pub participant_cols: Vec<usize>,
    /// Column index of the non-negative shortfall slack variable
    /// (`q^qru,+_n` or `q^qrd,+_n`) that absorbs any unmet requirement.
    pub shortfall_var: usize,
    /// Lower bound of the row in per-unit:
    /// `requirement_mvar / base_mva`. Upper bound is `+inf`.
    pub requirement_pu: f64,
    /// Shortfall slack cost `c^qru_n × dt × base_mva` in `$/pu-hr`,
    /// applied linearly to the shortfall slack variable in the
    /// objective.
    pub shortfall_cost_per_pu_hr: f64,
}

/// Build the reactive reserve plan for an AC-OPF problem instance.
///
/// `dispatchable_load_indices` is the list of global dispatchable-load
/// indices that the AC OPF is treating as native variables — parallel
/// to the mapping's `dl_var(k)` slots.
///
/// When the network has no reactive reserve products, returns an empty
/// plan whose every vector has the right length but all zeros.
pub(super) fn build_reactive_reserve_plan(
    network: &Network,
    mapping: &AcOpfMapping,
    dispatchable_load_indices: &[usize],
) -> AcReactiveReservePlan {
    // Fast path: no reactive reserves active at the mapping level
    // means the zone rows are empty and the per-device vectors are
    // still allocated (size 0) since there are no producer/consumer
    // q-reserve columns either.
    if !mapping.reactive_reserves_active() {
        return AcReactiveReservePlan {
            producer_q_reserve_up_cost_per_pu_hr: vec![0.0; mapping.n_producer_q_reserve],
            producer_q_reserve_down_cost_per_pu_hr: vec![0.0; mapping.n_producer_q_reserve],
            consumer_q_reserve_up_cost_per_pu_hr: vec![0.0; mapping.n_consumer_q_reserve],
            consumer_q_reserve_down_cost_per_pu_hr: vec![0.0; mapping.n_consumer_q_reserve],
            producer_q_reserve_up_ub_pu: vec![0.0; mapping.n_producer_q_reserve],
            producer_q_reserve_down_ub_pu: vec![0.0; mapping.n_producer_q_reserve],
            consumer_q_reserve_up_ub_pu: vec![0.0; mapping.n_consumer_q_reserve],
            consumer_q_reserve_down_ub_pu: vec![0.0; mapping.n_consumer_q_reserve],
            ..Default::default()
        };
    }

    let base_mva = network.base_mva;
    let market_rules = network
        .market_data
        .market_rules
        .as_ref()
        .expect("reactive_reserves_active implies market_rules is Some");

    // Reactive products indexed by id for fast lookup.
    let reactive_products_by_id: std::collections::HashMap<&str, &ReserveProduct> = market_rules
        .reserve_products
        .iter()
        .filter(|p| matches!(p.kind, ReserveKind::Reactive))
        .map(|p| (p.id.as_str(), p))
        .collect();

    // Per-producer / per-consumer variable upper bounds and costs.
    let mut plan = AcReactiveReservePlan {
        producer_q_reserve_up_cost_per_pu_hr: vec![0.0; mapping.n_producer_q_reserve],
        producer_q_reserve_down_cost_per_pu_hr: vec![0.0; mapping.n_producer_q_reserve],
        consumer_q_reserve_up_cost_per_pu_hr: vec![0.0; mapping.n_consumer_q_reserve],
        consumer_q_reserve_down_cost_per_pu_hr: vec![0.0; mapping.n_consumer_q_reserve],
        producer_q_reserve_up_ub_pu: vec![0.0; mapping.n_producer_q_reserve],
        producer_q_reserve_down_ub_pu: vec![0.0; mapping.n_producer_q_reserve],
        consumer_q_reserve_up_ub_pu: vec![0.0; mapping.n_consumer_q_reserve],
        consumer_q_reserve_down_ub_pu: vec![0.0; mapping.n_consumer_q_reserve],
        zone_rows: Vec::new(),
    };

    // Headroom upper bounds for producers.
    //
    // The natural upper bound on q^qru_j is `qmax_pu - qmin_pu` (the
    // widest possible reactive move). The per-device row (eq 112)
    // further tightens this to `qmax_pu - Qg_pu_at_optimum`, so the
    // column bound is a conservative outer envelope. For producers in
    // `J^pqe` (pq_linear_equality), eqs (117) / (127) force the
    // reserve to zero — we collapse the column bound to `[0, 0]`.
    for (j, &gi) in mapping.gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        let is_pqe = g
            .reactive_capability
            .as_ref()
            .is_some_and(|rc| rc.pq_linear_equality.is_some());
        if is_pqe {
            plan.producer_q_reserve_up_ub_pu[j] = 0.0;
            plan.producer_q_reserve_down_ub_pu[j] = 0.0;
            continue;
        }
        // Clamp absurd sentinel values from raw data.
        let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
        let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
        let headroom_pu = ((qmax - qmin).max(0.0)) / base_mva;
        plan.producer_q_reserve_up_ub_pu[j] = headroom_pu;
        plan.producer_q_reserve_down_ub_pu[j] = headroom_pu;

        // Per-device reserve cost: read reactive offers if present.
        // `Generator.market` wraps offers in an `Option<MarketParams>`
        // so non-market generators have no reserve offers at all.
        let gen_reserve_offers = g
            .market
            .as_ref()
            .map(|m| m.reserve_offers.as_slice())
            .unwrap_or(&[]);
        for offer in gen_reserve_offers {
            let Some(product) = reactive_products_by_id.get(offer.product_id.as_str()) else {
                continue;
            };
            // Cost units: `offer.cost_per_mwh` is ($/MVAr-hr) for
            // reactive products (the field is named "per_mwh" for
            // consistency with the real-power side, but the unit scales
            // with the product's physical quantity). Convert to
            // `$/pu-hr` by multiplying by `base_mva` so the objective
            // can just multiply by `x[var]` directly.
            let cost_per_pu_hr = offer.cost_per_mwh * base_mva;
            match product.direction {
                ReserveDirection::Up => {
                    plan.producer_q_reserve_up_cost_per_pu_hr[j] = cost_per_pu_hr;
                }
                ReserveDirection::Down => {
                    plan.producer_q_reserve_down_cost_per_pu_hr[j] = cost_per_pu_hr;
                }
            }
        }
    }

    // Headroom upper bounds for consumers.
    for (k, &dl_global_idx) in dispatchable_load_indices.iter().enumerate() {
        let dl = &network.market_data.dispatchable_loads[dl_global_idx];
        let is_pqe = dl.pq_linear_equality.is_some();
        if is_pqe {
            plan.consumer_q_reserve_up_ub_pu[k] = 0.0;
            plan.consumer_q_reserve_down_ub_pu[k] = 0.0;
            continue;
        }
        let headroom_pu = (dl.q_max_pu - dl.q_min_pu).max(0.0);
        plan.consumer_q_reserve_up_ub_pu[k] = headroom_pu;
        plan.consumer_q_reserve_down_ub_pu[k] = headroom_pu;

        for offer in &dl.reserve_offers {
            let Some(product) = reactive_products_by_id.get(offer.product_id.as_str()) else {
                continue;
            };
            let cost_per_pu_hr = offer.cost_per_mwh * base_mva;
            match product.direction {
                ReserveDirection::Up => {
                    plan.consumer_q_reserve_up_cost_per_pu_hr[k] = cost_per_pu_hr;
                }
                ReserveDirection::Down => {
                    plan.consumer_q_reserve_down_cost_per_pu_hr[k] = cost_per_pu_hr;
                }
            }
        }
    }

    // Zonal balance rows — walk `reserve_zones` and, per
    // `(zone, requirement)`, build the participant list from the
    // producer/consumer blocks whose device bus lies in that zone.
    //
    // The participant lookup is keyed by the integer area ID on the
    // bus — the area field carries reserve-zone membership end-to-end.
    //
    // We emit UP rows first, then DOWN rows, matching the variable
    // layout in `AcOpfMapping::zone_q_reserve_up_shortfall_offset`.
    let mut up_rows: Vec<AcZoneReserveBalance> = Vec::new();
    let mut down_rows: Vec<AcZoneReserveBalance> = Vec::new();

    for zone in &network.market_data.reserve_zones {
        for req in &zone.zonal_requirements {
            let Some(product) = reactive_products_by_id.get(req.product_id.as_str()) else {
                continue;
            };
            let participants = collect_zone_participants(
                network,
                mapping,
                dispatchable_load_indices,
                req,
                product.direction,
            );
            let requirement_pu = req.requirement_mw_for_period(0) / base_mva;
            #[allow(clippy::unnecessary_lazy_evaluations)]
            let shortfall_cost_per_pu_hr = req
                .shortfall_cost_per_unit
                .or_else(|| match &product.demand_curve {
                    surge_network::market::PenaltyCurve::Linear { cost_per_unit } => {
                        Some(*cost_per_unit)
                    }
                    _ => None,
                })
                .unwrap_or(0.0)
                * base_mva;

            let row = AcZoneReserveBalance {
                direction: product.direction,
                zone_id: req.zone_id,
                participant_cols: participants,
                // Fill shortfall_var after we know the final row index
                // in the direction-specific list.
                shortfall_var: 0,
                requirement_pu,
                shortfall_cost_per_pu_hr,
            };
            match product.direction {
                ReserveDirection::Up => up_rows.push(row),
                ReserveDirection::Down => down_rows.push(row),
            }
        }
    }

    // Sanity: the counts must match the mapping. If they don't, we
    // have a discrepancy between the counting pass in
    // `AcOpfMapping::new` and the building pass here — which would
    // corrupt variable/row indexing. Panic early so the bug is
    // localized rather than silently producing wrong residuals.
    assert_eq!(
        up_rows.len(),
        mapping.n_zone_q_reserve_up_shortfall,
        "reactive reserve plan up-row count mismatch with mapping"
    );
    assert_eq!(
        down_rows.len(),
        mapping.n_zone_q_reserve_down_shortfall,
        "reactive reserve plan down-row count mismatch with mapping"
    );

    for (i, row) in up_rows.iter_mut().enumerate() {
        row.shortfall_var = mapping.zone_q_reserve_up_shortfall_var(i);
    }
    for (i, row) in down_rows.iter_mut().enumerate() {
        row.shortfall_var = mapping.zone_q_reserve_down_shortfall_var(i);
    }

    plan.zone_rows.extend(up_rows);
    plan.zone_rows.extend(down_rows);
    plan
}

/// Build every `PqConstraint` row the AC-OPF NLP needs: D-curve,
/// linear p-q linking (both directions, both device families), and
/// flat q-headroom with reactive-reserve coupling.
///
/// The returned Vec is contiguous at `mapping.pq_con_offset` and its
/// length equals `mapping.n_pq_cons`. Upper-bound rows pick up the
/// producer/consumer q^qru column (coefficient `+1`), lower-bound rows
/// pick up q^qrd (coefficient `-1`), and equality rows (pqe) do not
/// couple any reserve because eqs (117)-(118) / (127)-(128) force
/// both reserve variables to zero via the column bound.
///
/// `enforce_capability_curves` is the pre-existing AC-OPF toggle; when
/// it is `false` the function returns an empty Vec (all devices fall
/// back to flat box Q bounds and no reactive reserves are modelled).
pub(super) fn build_pq_rows_with_q_reserves(
    network: &Network,
    mapping: &AcOpfMapping,
    dispatchable_load_indices: &[usize],
    enforce_capability_curves: bool,
) -> Vec<PqConstraint> {
    if !enforce_capability_curves {
        return Vec::new();
    }

    let mut rows: Vec<PqConstraint> = Vec::new();

    // 1. Producer D-curve rows (OPF-06).
    rows.extend(build_pq_constraints(
        &mapping.gen_indices,
        &network.generators,
        network.base_mva,
    ));

    // 2. Producer linear p-q linking rows.
    rows.extend(build_pq_linear_constraints(
        &mapping.gen_indices,
        &network.generators,
    ));

    // 3. Consumer linear p-q linking rows.
    let dl_refs: Vec<&surge_network::market::DispatchableLoad> = dispatchable_load_indices
        .iter()
        .map(|&idx| &network.market_data.dispatchable_loads[idx])
        .collect();
    rows.extend(build_pq_linear_constraints_consumers(&dl_refs));

    // 4. Flat q-headroom rows for producers and consumers. Only
    //    emitted when reactive reserves are active. The device-level
    //    indicator (`u^on + Σu^su + Σu^sd`) collapses to a known
    //    integer in the per-period AC reconcile; devices with
    //    `u^on = 0` are filtered out of `gen_indices`, so for every
    //    entry here `indicator = 1` and the RHS is just
    //    `qmax_pu` / `qmin_pu`.
    if mapping.reactive_reserves_active() {
        let base_mva = network.base_mva;

        for (j, &gi) in mapping.gen_indices.iter().enumerate() {
            let g = &network.generators[gi];
            let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
            let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
            let qmin_pu = qmin / base_mva;
            let qmax_pu = qmax / base_mva;
            let qru_col = mapping.producer_q_reserve_up_var(j);
            let qrd_col = mapping.producer_q_reserve_down_var(j);
            // Eq 112: Qg + q^qru <= qmax*indicator   (indicator = 1)
            rows.push(PqConstraint {
                kind: PqDeviceKind::Producer,
                device_local: j,
                slope: 0.0,
                lhs_lb: f64::NEG_INFINITY,
                lhs_ub: qmax_pu,
                q_reserve_var: Some(qru_col),
                q_reserve_sign: 1.0,
            });
            // Eq 113: Qg - q^qrd >= qmin*indicator
            rows.push(PqConstraint {
                kind: PqDeviceKind::Producer,
                device_local: j,
                slope: 0.0,
                lhs_lb: qmin_pu,
                lhs_ub: f64::INFINITY,
                q_reserve_var: Some(qrd_col),
                q_reserve_sign: -1.0,
            });
        }
        for (k, &dl_idx) in dispatchable_load_indices.iter().enumerate() {
            let dl = &network.market_data.dispatchable_loads[dl_idx];
            let qmin_pu = dl.q_min_pu;
            let qmax_pu = dl.q_max_pu;
            let qru_col = mapping.consumer_q_reserve_up_var(k);
            let qrd_col = mapping.consumer_q_reserve_down_var(k);
            // Eq 122: q_dl + q^qrd <= qmax*indicator  (consumer: qrd
            // eats into headroom, opposite sign role vs producer)
            rows.push(PqConstraint {
                kind: PqDeviceKind::Consumer,
                device_local: k,
                slope: 0.0,
                lhs_lb: f64::NEG_INFINITY,
                lhs_ub: qmax_pu,
                q_reserve_var: Some(qrd_col),
                q_reserve_sign: 1.0,
            });
            // Eq 123: q_dl - q^qru >= qmin*indicator
            rows.push(PqConstraint {
                kind: PqDeviceKind::Consumer,
                device_local: k,
                slope: 0.0,
                lhs_lb: qmin_pu,
                lhs_ub: f64::INFINITY,
                q_reserve_var: Some(qru_col),
                q_reserve_sign: -1.0,
            });
        }
    }

    // 5. Post-process: attach q-reserve coupling to every D-curve /
    //    linear-link upper-bound row and lower-bound row that targets
    //    a device with q-reserves allocated. Equality rows (lhs_lb ==
    //    lhs_ub, finite on both sides) skip coupling — for those the
    //    q-reserve variables are pinned to 0 via the column bound and
    //    the row would be over-constrained if we also added it here.
    //
    //    Indices `0..n_pq_legacy` are the rows built in steps 1-3
    //    (before the flat headroom rows); they're the only ones
    //    eligible for post-hoc coupling. Flat-headroom rows already
    //    carry their coupling directly.
    let n_pq_legacy = rows.len()
        - if mapping.reactive_reserves_active() {
            2 * mapping.n_producer_q_reserve + 2 * mapping.n_consumer_q_reserve
        } else {
            0
        };
    if mapping.reactive_reserves_active() {
        for row in rows.iter_mut().take(n_pq_legacy) {
            // Equality rows: skip.
            if row.lhs_lb.is_finite() && row.lhs_ub.is_finite() {
                continue;
            }
            let (up_col, dn_col) = match row.kind {
                PqDeviceKind::Producer => (
                    mapping.producer_q_reserve_up_var(row.device_local),
                    mapping.producer_q_reserve_down_var(row.device_local),
                ),
                PqDeviceKind::Consumer => (
                    mapping.consumer_q_reserve_up_var(row.device_local),
                    mapping.consumer_q_reserve_down_var(row.device_local),
                ),
            };
            // Upper-bound row: `q_dev - slope*p_dev + q^reserve_up ≤ ub`.
            //   Producer upper rows use q^qru (sign +1).
            //   Consumer upper rows use q^qrd (sign +1) per eqs 122/124.
            if row.lhs_ub.is_finite() && row.lhs_lb == f64::NEG_INFINITY {
                let col = match row.kind {
                    PqDeviceKind::Producer => up_col,
                    PqDeviceKind::Consumer => dn_col,
                };
                row.q_reserve_var = Some(col);
                row.q_reserve_sign = 1.0;
            }
            // Lower-bound row: `q_dev - slope*p_dev - q^reserve_down ≥ lb`.
            //   Producer lower rows use q^qrd (sign −1).
            //   Consumer lower rows use q^qru (sign −1) per eqs 123/125.
            if row.lhs_lb.is_finite() && row.lhs_ub == f64::INFINITY {
                let col = match row.kind {
                    PqDeviceKind::Producer => dn_col,
                    PqDeviceKind::Consumer => up_col,
                };
                row.q_reserve_var = Some(col);
                row.q_reserve_sign = -1.0;
            }
        }
    }

    rows
}

/// Collect the variable column indices of every q-reserve participant
/// in a given zone for the given direction.
///
/// The device set is every in-service producer and consumer whose bus
/// area matches `req.zone_id`. The coefficient is always `+1.0` so we
/// return bare column indices.
fn collect_zone_participants(
    network: &Network,
    mapping: &AcOpfMapping,
    dispatchable_load_indices: &[usize],
    req: &ZonalReserveRequirement,
    direction: ReserveDirection,
) -> Vec<usize> {
    let mut cols: Vec<usize> = Vec::new();

    // Producers.
    for (j, &gi) in mapping.gen_indices.iter().enumerate() {
        let g = &network.generators[gi];
        let fallback_area = network
            .buses
            .iter()
            .find(|b| b.number == g.bus)
            .map(|b| b.area as usize);
        if req.has_explicit_participant_buses() {
            if !req.includes_participant_bus_number(g.bus) {
                continue;
            }
        } else if fallback_area.unwrap_or(0) != req.zone_id {
            continue;
        }
        match direction {
            ReserveDirection::Up => cols.push(mapping.producer_q_reserve_up_var(j)),
            ReserveDirection::Down => cols.push(mapping.producer_q_reserve_down_var(j)),
        }
    }

    // Consumers.
    for (k, &dl_global_idx) in dispatchable_load_indices.iter().enumerate() {
        let dl = &network.market_data.dispatchable_loads[dl_global_idx];
        let fallback_area = network
            .buses
            .iter()
            .find(|b| b.number == dl.bus)
            .map(|b| b.area as usize);
        if req.has_explicit_participant_buses() {
            if !req.includes_participant_bus_number(dl.bus) {
                continue;
            }
        } else if fallback_area.unwrap_or(0) != req.zone_id {
            continue;
        }
        match direction {
            ReserveDirection::Up => cols.push(mapping.consumer_q_reserve_up_var(k)),
            ReserveDirection::Down => cols.push(mapping.consumer_q_reserve_down_var(k)),
        }
    }

    cols
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ac::problem::AcOpfProblem;
    use crate::ac::types::{AcOpfOptions, AcOpfRunContext};
    use surge_network::Network;
    use surge_network::market::{
        CostCurve, DispatchableLoad, EnergyCoupling, MarketRules, PenaltyCurve, QualificationRule,
        ReserveDirection, ReserveKind, ReserveProduct, ReserveZone, ZonalReserveRequirement,
    };
    use surge_network::network::{Bus, BusType, Generator, Load, PqLinearLink, ReactiveCapability};

    /// Build a minimal 1-bus 1-generator 1-consumer network with a
    /// single reactive-up reserve zone. Used as the base fixture for
    /// the 5B reactive-reserve test suite.
    ///
    /// Topology:
    ///   * Bus 1 is the slack with `area = 7` so the reactive zone
    ///     (zone_id = 7) matches it.
    ///   * `gen0` has `qmax = 50`, `qmin = -50` MVAr, `pmax = 100`.
    ///   * A dispatchable load at the same bus.
    ///   * Market rules carry one `ReserveProduct` with
    ///     `kind = Reactive` and direction `Up`, and a
    ///     `ZonalReserveRequirement` of `15 MVAr` in the zone.
    ///
    /// The fixture is deliberately feasible (headroom = 100 MVAr,
    /// requirement = 15 MVAr) so any structural test can bind on it.
    fn q_reserves_one_bus_fixture() -> Network {
        let mut net = Network::new("q_reserves_one_bus");
        net.base_mva = 100.0;
        let mut bus = Bus::new(1, BusType::Slack, 138.0);
        bus.area = 7;
        net.buses.push(bus);
        net.loads.push(Load::new(1, 30.0, 10.0));

        let mut generator = Generator::new(1, 40.0, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.qmin = -50.0;
        generator.qmax = 50.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![5.0, 0.0],
        });
        net.generators.push(generator);

        let mut dl = DispatchableLoad::curtailable(1, 10.0, 0.0, 0.0, 3.0, net.base_mva);
        dl.resource_id = "dl0".into();
        net.market_data.dispatchable_loads.push(dl);

        net.market_data.market_rules = Some(MarketRules {
            voll: 9000.0,
            reserve_products: vec![ReserveProduct {
                id: "q_res_up".to_string(),
                name: "Reactive Reserve Up".to_string(),
                kind: ReserveKind::Reactive,
                apply_deploy_ramp_limit: true,
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::None,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            }],
            system_reserve_requirements: Vec::new(),
        });
        net.market_data.reserve_zones.push(ReserveZone {
            name: "Z1".to_string(),
            zonal_requirements: vec![ZonalReserveRequirement {
                zone_id: 7,
                product_id: "q_res_up".to_string(),
                requirement_mw: 15.0,
                per_period_mw: None,
                shortfall_cost_per_unit: None,
                served_dispatchable_load_coefficient: None,
                largest_generator_dispatch_coefficient: None,
                participant_bus_numbers: None,
            }],
        });
        net
    }

    /// Baseline network without reactive reserves — used to verify
    /// `reactive_reserves_active() == false` and that the mapping
    /// collapses to zero q-reserve blocks.
    fn no_q_reserves_network() -> Network {
        let mut net = q_reserves_one_bus_fixture();
        net.market_data.market_rules = None;
        net.market_data.reserve_zones.clear();
        net
    }

    /// The mapping MUST activate q-reserve blocks when the network
    /// has at least one reactive reserve product.
    #[test]
    fn test_mapping_allocates_q_reserve_blocks_when_reactive_product_exists() {
        let net = q_reserves_one_bus_fixture();
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new should succeed for the 1-bus fixture");
        let m = &problem.mapping;
        assert!(m.reactive_reserves_active());
        assert_eq!(m.n_producer_q_reserve, 1);
        assert_eq!(m.n_consumer_q_reserve, 1);
        assert_eq!(m.n_zone_q_reserve_up_shortfall, 1);
        assert_eq!(m.n_zone_q_reserve_down_shortfall, 0);
        // Offsets must be contiguous — producer q-reserves follow Qg,
        // consumer q-reserves follow dl_q, zone shortfall slacks live
        // at the end of the variable vector.
        assert_eq!(m.producer_q_reserve_up_offset, m.qg_offset + m.n_gen);
        assert_eq!(
            m.producer_q_reserve_down_offset,
            m.producer_q_reserve_up_offset + m.n_producer_q_reserve
        );
        assert_eq!(m.consumer_q_reserve_up_offset, m.dl_q_offset + m.n_dl);
        assert_eq!(
            m.consumer_q_reserve_down_offset,
            m.consumer_q_reserve_up_offset + m.n_consumer_q_reserve
        );
        assert_eq!(m.zone_q_reserve_up_shortfall_offset + 1, m.n_var);
    }

    /// Sanity: networks without reactive products keep the
    /// pre-5B mapping layout — every new block has size 0.
    #[test]
    fn test_mapping_without_reactive_products_has_no_q_reserve_blocks() {
        let net = no_q_reserves_network();
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new should succeed without reactive reserves");
        let m = &problem.mapping;
        assert!(!m.reactive_reserves_active());
        assert_eq!(m.n_producer_q_reserve, 0);
        assert_eq!(m.n_consumer_q_reserve, 0);
        assert_eq!(m.n_zone_q_reserve_up_shortfall, 0);
        assert_eq!(m.n_zone_q_reserve_down_shortfall, 0);
        assert!(problem.reactive_reserve_plan.zone_rows.is_empty());
    }

    /// Producers and consumers in the `J^pqe` class (rigid p-q
    /// linking equality) must have `q^qru = q^qrd = 0`. The plan
    /// collapses the column upper bound to zero.
    #[test]
    fn test_pqe_device_forces_q_reserve_bounds_to_zero() {
        let mut net = q_reserves_one_bus_fixture();
        net.generators[0].reactive_capability = Some(ReactiveCapability {
            pq_linear_equality: Some(PqLinearLink {
                q_at_p_zero_pu: 0.10,
                beta: 0.05,
            }),
            ..ReactiveCapability::default()
        });
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new should succeed with pqe producer");
        assert_eq!(
            problem.reactive_reserve_plan.producer_q_reserve_up_ub_pu,
            vec![0.0]
        );
        assert_eq!(
            problem.reactive_reserve_plan.producer_q_reserve_down_ub_pu,
            vec![0.0]
        );
    }

    /// The zonal balance row collects every in-service producer and
    /// consumer in the zone as a participant.
    #[test]
    fn test_zone_balance_row_includes_all_zone_participants() {
        let net = q_reserves_one_bus_fixture();
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new should succeed for the 1-bus fixture");
        let plan = &problem.reactive_reserve_plan;
        assert_eq!(plan.zone_rows.len(), 1);
        let zone = &plan.zone_rows[0];
        assert_eq!(zone.direction, ReserveDirection::Up);
        assert_eq!(zone.zone_id, 7);
        // Requirement in pu is `15 MVAr / 100 MVA base = 0.15 pu`.
        assert!((zone.requirement_pu - 0.15).abs() < 1e-12);
        // 1 producer + 1 consumer in the zone → 2 participants.
        assert_eq!(zone.participant_cols.len(), 2);
        assert!(
            zone.participant_cols
                .contains(&problem.mapping.producer_q_reserve_up_var(0))
        );
        assert!(
            zone.participant_cols
                .contains(&problem.mapping.consumer_q_reserve_up_var(0))
        );
        // Shortfall cost is 1000 × base_mva = 100_000 $/pu-hr.
        assert!((zone.shortfall_cost_per_pu_hr - 100_000.0).abs() < 1e-9);
    }

    #[test]
    fn test_zone_balance_row_uses_explicit_participant_buses_over_area_membership() {
        let mut net = q_reserves_one_bus_fixture();
        net.buses[0].area = 0;
        net.market_data.reserve_zones[0].zonal_requirements[0].participant_bus_numbers =
            Some(vec![1]);

        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new should honor explicit reserve participants");
        let zone = &problem.reactive_reserve_plan.zone_rows[0];
        assert_eq!(zone.zone_id, 7);
        assert_eq!(zone.participant_cols.len(), 2);
        assert!(
            zone.participant_cols
                .contains(&problem.mapping.producer_q_reserve_up_var(0))
        );
        assert!(
            zone.participant_cols
                .contains(&problem.mapping.consumer_q_reserve_up_var(0))
        );
    }

    /// The flat q-headroom rows are present for every producer and
    /// consumer when reactive reserves are active. Two rows per
    /// producer (up + down) plus two rows per consumer.
    #[test]
    fn test_pq_con_block_contains_flat_headroom_rows_with_q_reserve_coupling() {
        let net = q_reserves_one_bus_fixture();
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new should succeed for the 1-bus fixture");
        let flat_rows: Vec<_> = problem
            .pq_constraints
            .iter()
            .filter(|c| c.slope == 0.0 && c.q_reserve_var.is_some())
            .collect();
        // 1 producer × 2 directions + 1 consumer × 2 directions = 4 rows.
        assert_eq!(flat_rows.len(), 4);
        // Producer upper (eq 112): q + qru ≤ qmax_pu = 0.5
        let pr_upper = flat_rows
            .iter()
            .find(|r| r.kind == PqDeviceKind::Producer && r.q_reserve_sign > 0.0)
            .expect("producer upper flat row should exist");
        assert!((pr_upper.lhs_ub - 0.5).abs() < 1e-12);
        assert_eq!(pr_upper.lhs_lb, f64::NEG_INFINITY);
        assert_eq!(
            pr_upper.q_reserve_var,
            Some(problem.mapping.producer_q_reserve_up_var(0))
        );
        // Producer lower (eq 113): q - qrd ≥ qmin_pu = -0.5
        let pr_lower = flat_rows
            .iter()
            .find(|r| r.kind == PqDeviceKind::Producer && r.q_reserve_sign < 0.0)
            .expect("producer lower flat row should exist");
        assert!((pr_lower.lhs_lb - (-0.5)).abs() < 1e-12);
        assert_eq!(pr_lower.lhs_ub, f64::INFINITY);
    }

    /// Producers with `pq_linear_upper` AND reactive reserves share
    /// a single coupled row that links `Qg + q^qru − β·Pg ≤ q0_ub`.
    /// The row is post-processed in `build_pq_rows_with_q_reserves`
    /// to attach the q-reserve variable; the flat headroom row is
    /// emitted in addition.
    #[test]
    fn test_pqmax_producer_row_couples_q_reserve_up() {
        let mut net = q_reserves_one_bus_fixture();
        net.generators[0].reactive_capability = Some(ReactiveCapability {
            pq_linear_upper: Some(PqLinearLink {
                q_at_p_zero_pu: 0.40,
                beta: -0.10,
            }),
            ..ReactiveCapability::default()
        });
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new should succeed for pqmax producer");
        // Find the linear-link upper row (non-zero slope, upper bound finite).
        let linked = problem
            .pq_constraints
            .iter()
            .find(|c| {
                c.slope != 0.0
                    && c.lhs_ub.is_finite()
                    && c.lhs_lb == f64::NEG_INFINITY
                    && c.kind == PqDeviceKind::Producer
            })
            .expect("pq_linear_upper row should exist");
        assert!((linked.lhs_ub - 0.40).abs() < 1e-12);
        assert!((linked.slope - (-0.10)).abs() < 1e-12);
        assert_eq!(
            linked.q_reserve_var,
            Some(problem.mapping.producer_q_reserve_up_var(0))
        );
        assert!(linked.q_reserve_sign > 0.0);
    }
}
