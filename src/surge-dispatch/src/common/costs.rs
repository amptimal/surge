// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared cost-assembly helpers for SCED and SCUC.
//!
//! This module contains functions for computing piecewise-linear (PWL) cost
//! segment representations (epiograph formulation) shared between the single-
//! period SCED and multi-hour SCUC LP/MILP formulations.

use std::borrow::Cow;
use std::collections::HashMap;

use surge_network::Network;
use surge_network::market::{
    CostCurve, DispatchableLoad, LoadCostModel, OfferCurve, OfferSchedule, StartupTier,
};
use surge_network::network::{Generator, StorageDispatchMode, StorageParams};

use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedGeneratorEconomics<'a> {
    pub cost: Cow<'a, CostCurve>,
    pub startup_tiers: Cow<'a, [StartupTier]>,
}

impl<'a> ResolvedGeneratorEconomics<'a> {
    pub fn startup_cost_for_offline_hours(&self, offline_hours: f64) -> f64 {
        if !self.startup_tiers.is_empty() {
            for tier in self.startup_tiers.iter() {
                if offline_hours <= tier.max_offline_hours {
                    return tier.cost;
                }
            }
            return self
                .startup_tiers
                .last()
                .map(|tier| tier.cost)
                .unwrap_or(0.0);
        }

        match self.cost.as_ref() {
            CostCurve::Polynomial { startup, .. } | CostCurve::PiecewiseLinear { startup, .. } => {
                *startup
            }
        }
    }
}

pub(crate) fn active_energy_offer_curve(generator: &Generator) -> Option<&OfferCurve> {
    let offer = generator
        .market
        .as_ref()
        .and_then(|market| market.energy_offer.as_ref())?;
    if offer.mitigation_active {
        offer.mitigated.as_ref().or(Some(&offer.submitted))
    } else {
        Some(&offer.submitted)
    }
}

pub(crate) fn resolve_generator_economics_for_period<'a>(
    gi: usize,
    period: usize,
    generator: &'a Generator,
    offer_schedules: &HashMap<usize, OfferSchedule>,
    gen_pmax: Option<f64>,
) -> Option<ResolvedGeneratorEconomics<'a>> {
    if let Some(schedule) = offer_schedules.get(&gi)
        && let Some(Some(offer_curve)) = schedule.periods.get(period)
    {
        return Some(ResolvedGeneratorEconomics {
            cost: Cow::Owned(offer_curve_to_cost_curve(offer_curve, gen_pmax)),
            startup_tiers: Cow::Owned(offer_curve.startup_tiers.clone()),
        });
    }

    let cost = generator.cost.as_ref()?;
    let startup_tiers = active_energy_offer_curve(generator)
        .map(|curve| Cow::Borrowed(curve.startup_tiers.as_slice()))
        .unwrap_or_else(|| Cow::Borrowed(&[]));

    Some(ResolvedGeneratorEconomics {
        cost: Cow::Borrowed(cost),
        startup_tiers,
    })
}

/// Compute piecewise-linear epiograph segments for a set of `(MW, $/MWh)` points.
///
/// Returns `(slope_pu, intercept)` pairs for each window.  Used for both
/// storage discharge offer curves and storage charge bid curves.
/// Returns `None` for a given window if the MW interval is degenerate (< 1e-20).
pub(crate) fn pwl_curve_segments(points: &[(f64, f64)], base: f64) -> Vec<(f64, f64)> {
    points
        .windows(2)
        .filter_map(|w| {
            let (p0_mw, c0) = w[0];
            let (p1_mw, c1) = w[1];
            let dp_mw = p1_mw - p0_mw;
            if dp_mw.abs() < 1e-20 {
                return None;
            }
            let slope_per_mwh = (c1 - c0) / dp_mw;
            let slope_pu = slope_per_mwh * base;
            let p0_pu = p0_mw / base;
            let intercept = c0 - slope_pu * p0_pu;
            Some((slope_pu, intercept))
        })
        .collect()
}

pub(crate) fn validate_storage_offer_curve_points(
    points: &[(f64, f64)],
    curve_label: &str,
) -> Result<(), String> {
    StorageParams::validate_market_curve_points(points, curve_label)
}

pub(crate) fn storage_offer_curve_cost(points: &[(f64, f64)], mw: f64) -> f64 {
    StorageParams::market_curve_value(points, mw)
}

/// Resolve the effective PWL epiograph segments for a generator in one period.
///
/// This respects per-period offer schedule overrides before falling back to the
/// network generator's static cost curve.
pub(crate) fn resolve_pwl_gen_segments_for_period(
    network: &Network,
    gen_indices: &[usize],
    offer_schedules: &HashMap<usize, OfferSchedule>,
    period: usize,
    base: f64,
    polynomial_breakpoints: Option<usize>,
) -> Vec<(usize, Vec<(f64, f64)>)> {
    gen_indices
        .iter()
        .enumerate()
        .filter_map(|(j, &gi)| {
            let g = &network.generators[gi];
            if g.is_storage() {
                return None;
            }
            let economics = resolve_generator_economics_for_period(
                gi,
                period,
                g,
                offer_schedules,
                Some(g.pmax),
            )?;
            let cost = economics.cost.as_ref();

            match cost {
                CostCurve::PiecewiseLinear { points, .. } => {
                    let segments = pwl_curve_segments(points, base);
                    if !segments.is_empty() {
                        return Some((j, segments));
                    }
                }
                CostCurve::Polynomial { .. } => {
                    let segments = convex_polynomial_tangent_segments(
                        cost,
                        g.pmin,
                        g.pmax,
                        base,
                        polynomial_breakpoints,
                    )?;
                    if !segments.is_empty() {
                        return Some((j, segments));
                    }
                }
            }

            None
        })
        .collect()
}

fn convex_polynomial_tangent_segments(
    cost: &CostCurve,
    pmin_mw: f64,
    pmax_mw: f64,
    base: f64,
    polynomial_breakpoints: Option<usize>,
) -> Option<Vec<(f64, f64)>> {
    let n_breakpoints = polynomial_breakpoints?.max(2);
    let CostCurve::Polynomial { coeffs, .. } = cost else {
        return None;
    };
    if coeffs.len() < 3 || coeffs[0].abs() <= 1e-20 || !cost.is_convex() {
        return None;
    }
    if pmax_mw <= pmin_mw + 1e-6 {
        return None;
    }

    let mut segments = Vec::with_capacity(n_breakpoints);
    for breakpoint_idx in 0..n_breakpoints {
        let t = breakpoint_idx as f64 / (n_breakpoints - 1) as f64;
        let p_k_mw = pmin_mw + t * (pmax_mw - pmin_mw);
        let slope_mwh = cost.marginal_cost(p_k_mw);
        let intercept = cost.evaluate(p_k_mw) - slope_mwh * p_k_mw;
        segments.push((slope_mwh * base, intercept));
    }
    Some(segments)
}

pub(crate) fn uses_convex_polynomial_pwl(cost: &CostCurve) -> bool {
    matches!(cost, CostCurve::Polynomial { coeffs, .. } if coeffs.len() >= 3 && coeffs[0].abs() > 1e-20)
        && cost.is_convex()
}

type StorageEpiSegments = Vec<(usize, Vec<(f64, f64)>)>;

/// Compute storage discharge offer epiograph segments.
///
/// Returns a vec of `(local_index, Vec<(slope_pu, intercept)>)` for
/// each storage generator in `OfferCurve` dispatch mode that has a discharge
/// offer with a validated explicit origin. Input is a slice of storage-only
/// generators paired with their global generator indices.
pub(crate) fn storage_discharge_epi_segments(
    storage_gens: &[(usize, &Generator)],
    base: f64,
) -> Result<StorageEpiSegments, ScedError> {
    let mut results = Vec::new();
    for (s, &(gi, g)) in storage_gens.iter().enumerate() {
        let Some(sto) = g.storage.as_ref() else {
            continue;
        };
        if sto.dispatch_mode != StorageDispatchMode::OfferCurve {
            continue;
        }
        let Some(points) = sto.discharge_offer.as_deref() else {
            continue;
        };
        validate_storage_offer_curve_points(
            points,
            &format!("storage generator {gi} discharge_offer"),
        )
        .map_err(ScedError::InvalidInput)?;
        let segments = pwl_curve_segments(points, base);
        if !segments.is_empty() {
            results.push((s, segments));
        }
    }
    Ok(results)
}

/// Compute storage charge bid epiograph segments.
///
/// Returns a vec of `(local_index, Vec<(slope_pu, intercept)>)` for
/// each storage generator in `OfferCurve` dispatch mode that has a charge bid
/// with a validated explicit origin. Input is a slice of storage-only
/// generators paired with their global generator indices.
pub(crate) fn storage_charge_epi_segments(
    storage_gens: &[(usize, &Generator)],
    base: f64,
) -> Result<StorageEpiSegments, ScedError> {
    let mut results = Vec::new();
    for (s, &(gi, g)) in storage_gens.iter().enumerate() {
        let Some(sto) = g.storage.as_ref() else {
            continue;
        };
        if sto.dispatch_mode != StorageDispatchMode::OfferCurve {
            continue;
        }
        let Some(points) = sto.charge_bid.as_deref() else {
            continue;
        };
        validate_storage_offer_curve_points(points, &format!("storage generator {gi} charge_bid"))
            .map_err(ScedError::InvalidInput)?;
        let segments = pwl_curve_segments(points, base);
        if !segments.is_empty() {
            results.push((s, segments));
        }
    }
    Ok(results)
}

/// Convert an [`OfferCurve`] (market offer format) to a [`CostCurve`] (solver format).
///
/// `OfferCurve` has segments `(MW, $/MWh)` — marginal prices at MW breakpoints.
/// `CostCurve::PiecewiseLinear` has points `(MW, $/hr)` — total cost at breakpoints.
///
/// The conversion integrates the step-function marginal offer into cumulative cost:
///   `cost(MW_k) = no_load + Σ_{i=0}^{k-1} (MW_{i+1} - MW_i) × price_i`
///
/// When `pmax` is provided, segments beyond the generator's current capacity are
/// clipped. This prevents infeasible epiograph constraints when renewable profiles
/// derate a generator's pmax below the offer curve's maximum MW.
pub(crate) fn offer_curve_to_cost_curve(offer: &OfferCurve, pmax: Option<f64>) -> CostCurve {
    if offer.segments.is_empty() {
        return CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0],
        };
    }

    // Single segment: treat as linear cost (marginal_price × MW + no_load)
    if offer.segments.len() == 1 {
        let (_, price) = offer.segments[0];
        return CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![price, offer.no_load_cost],
        };
    }

    let cap = pmax.unwrap_or(f64::INFINITY);

    // Generator fully derated — return flat zero-cost curve.
    if cap <= 0.0 {
        return CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, offer.no_load_cost],
        };
    }

    // Multi-segment: build piecewise-linear total cost curve.
    // The first point is the no-load cost at 0 MW, so the first segment's
    // marginal price is integrated over [0, MW_0].
    let mut points = Vec::with_capacity(offer.segments.len() + 1);
    let mut cumulative_cost = offer.no_load_cost;
    points.push((0.0, cumulative_cost));

    for i in 0..offer.segments.len() {
        let (mw, price) = offer.segments[i];
        let mw_clipped = mw.min(cap);
        let prev_mw = if i == 0 {
            0.0
        } else {
            offer.segments[i - 1].0.min(cap)
        };
        if mw_clipped > prev_mw {
            cumulative_cost += (mw_clipped - prev_mw) * price;
            points.push((mw_clipped, cumulative_cost));
        }
        if mw_clipped >= cap {
            break;
        }
    }

    CostCurve::PiecewiseLinear {
        startup: 0.0,
        shutdown: 0.0,
        points,
    }
}

/// Resolve the effective [`CostCurve`] for a generator at a given period.
///
/// Checks `offer_schedules` first; if a per-period offer curve exists, converts
/// it to a `CostCurve`. Otherwise falls back to the generator's static `g.cost`.
pub(crate) fn resolve_cost_for_period<'a>(
    gi: usize,
    period: usize,
    generator: &'a Generator,
    offer_schedules: &HashMap<usize, OfferSchedule>,
    offer_cost_buf: &'a mut Option<CostCurve>,
    gen_pmax: Option<f64>,
) -> &'a CostCurve {
    let economics =
        resolve_generator_economics_for_period(gi, period, generator, offer_schedules, gen_pmax)
            .expect("generator cost should be set for dispatch");

    match economics.cost {
        Cow::Borrowed(cost) => cost,
        Cow::Owned(cost) => {
            *offer_cost_buf = Some(cost);
            offer_cost_buf
                .as_ref()
                .expect("offer_cost_buf just assigned to Some")
        }
    }
}

pub(crate) fn resolve_cost_for_period_from_spec<'a>(
    gi: usize,
    period: usize,
    generator: &'a Generator,
    spec: &DispatchProblemSpec<'_>,
    offer_cost_buf: &'a mut Option<CostCurve>,
    gen_pmax: Option<f64>,
) -> &'a CostCurve {
    resolve_cost_for_period(
        gi,
        period,
        generator,
        spec.offer_schedules,
        offer_cost_buf,
        gen_pmax,
    )
}

pub(crate) fn resolve_dl_for_period_from_spec<'a>(
    dl_index: usize,
    period: usize,
    dl: &'a DispatchableLoad,
    spec: &'a DispatchProblemSpec<'_>,
) -> (f64, f64, f64, f64, f64, &'a LoadCostModel) {
    if let Some(schedule) = spec.dl_offer_schedules.get(&dl_index)
        && let Some(Some(params)) = schedule.periods.get(period)
    {
        (
            params.p_sched_pu,
            params.p_max_pu,
            params.q_sched_pu.unwrap_or(dl.q_sched_pu),
            params.q_min_pu.unwrap_or(dl.q_min_pu),
            params.q_max_pu.unwrap_or(dl.q_max_pu),
            &params.cost_model,
        )
    } else {
        (
            dl.p_sched_pu,
            dl.p_max_pu,
            dl.q_sched_pu,
            dl.q_min_pu,
            dl.q_max_pu,
            &dl.cost_model,
        )
    }
}

pub(crate) fn exact_dispatchable_load_objective_dollars(
    cost_model: &LoadCostModel,
    served_pu: f64,
    linear_coeff: f64,
    base_mva: f64,
    dt_h: f64,
) -> f64 {
    let quadratic_coeff = cost_model.dc_quadratic_obj_coeff(base_mva) * dt_h;
    served_pu * linear_coeff + 0.5 * quadratic_coeff * served_pu * served_pu
}

/// Compute the effective per-generator CO2 emission price scalar.
///
/// Returns `options.carbon_price.price_per_tonne` if set, otherwise falls back
/// to `options.co2_price_per_t`.
pub(crate) fn effective_co2_price(spec: &DispatchProblemSpec<'_>) -> f64 {
    spec.carbon_price
        .map(|cp| cp.price_per_tonne)
        .unwrap_or(spec.co2_price_per_t)
}

/// Compute per-generator effective CO2 emission rates (tCO2/MWh).
///
/// `EmissionProfile` overrides the per-generator `emission_rates.co2` field
/// when provided. Returns a `Vec<f64>` of length `gen_indices.len()`.
pub(crate) fn effective_co2_rates(
    network: &Network,
    gen_indices: &[usize],
    spec: &DispatchProblemSpec<'_>,
) -> Vec<f64> {
    gen_indices
        .iter()
        .enumerate()
        .map(|(j, &gi)| {
            spec.emission_profile
                .map(|ep| ep.rate_for(j))
                .unwrap_or_else(|| {
                    network.generators[gi]
                        .fuel
                        .as_ref()
                        .map(|f| f.emission_rates.co2)
                        .unwrap_or(0.0)
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::offer_curve_to_cost_curve;
    use surge_network::market::{CostCurve, OfferCurve};

    #[test]
    fn test_offer_curve_to_cost_curve_integrates_first_segment() {
        let curve = OfferCurve {
            segments: vec![(125.0, -25.0), (250.0, 0.0)],
            no_load_cost: 150.0,
            startup_tiers: Vec::new(),
        };

        let cost = offer_curve_to_cost_curve(&curve, None);
        let CostCurve::PiecewiseLinear { points, .. } = cost else {
            panic!("multi-segment offer should convert to piecewise-linear cost");
        };

        assert_eq!(points.len(), 3);
        assert_eq!(points[0], (0.0, 150.0));
        assert_eq!(points[1], (125.0, -2975.0));
        assert_eq!(points[2], (250.0, -2975.0));
    }
}
