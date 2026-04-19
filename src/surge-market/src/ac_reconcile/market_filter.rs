// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Reactive-reserves-only market filtering.
//!
//! The AC SCED stage only re-clears reactive reserves; active reserves
//! were cleared by the source (SCUC) stage and their awards are
//! preserved through the dispatch pinning headroom shrink. This helper
//! strips active reserve products, requirements, and offer schedules
//! whose `product_id` is not in the `keep_product_ids` set.

use std::collections::HashSet;

use surge_dispatch::DispatchRequest;

/// Filter the market's reserve catalog, requirements, and offer
/// schedules to the given set of product IDs. Products and everything
/// keyed to them (requirements, generator/load offer schedules) are
/// dropped.
pub fn apply_reactive_reserve_filter(
    request: &mut DispatchRequest,
    keep_product_ids: &HashSet<String>,
) {
    let market = request.market_mut();

    market
        .reserve_products
        .retain(|p| keep_product_ids.contains(&p.id));

    // After reserve_products was filtered, recompute the set of IDs
    // that are actually present in the market (caller's `keep` may
    // include IDs the request never had).
    let kept: HashSet<String> = market
        .reserve_products
        .iter()
        .map(|p| p.id.clone())
        .collect();

    market
        .system_reserve_requirements
        .retain(|r| kept.contains(&r.product_id));
    market
        .zonal_reserve_requirements
        .retain(|r| kept.contains(&r.product_id));

    for schedule in market.generator_reserve_offer_schedules.iter_mut() {
        for period in schedule.schedule.periods.iter_mut() {
            period.retain(|offer| kept.contains(&offer.product_id));
        }
    }
    market
        .generator_reserve_offer_schedules
        .retain(|s| s.schedule.periods.iter().any(|p| !p.is_empty()));

    for schedule in market.dispatchable_load_reserve_offer_schedules.iter_mut() {
        for period in schedule.schedule.periods.iter_mut() {
            period.retain(|offer| kept.contains(&offer.product_id));
        }
    }
    market
        .dispatchable_load_reserve_offer_schedules
        .retain(|s| s.schedule.periods.iter().any(|p| !p.is_empty()));
}
