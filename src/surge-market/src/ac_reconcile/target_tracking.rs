// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical target-tracking feedback providers.
//!
//! The AC SCED NLP's target-tracking objective has the shape
//!
//! ```text
//! Σ_g [ α_up(g) · max(0, Pg - Pg_target)²
//!     + α_down(g) · max(0, Pg_target - Pg)² ]
//! ```
//!
//! where `Pg_target` comes from the source (DC SCUC) stage. The
//! per-direction coefficients `(α_up, α_down)` can be symmetric (a
//! plain quadratic pullback) or asymmetric (strong pullback in one
//! direction, weak or zero in the other). Asymmetric coefficients let
//! the AC OPF respect economic signals the LP already cleared:
//! generators the LP wanted *higher than pmax* get a strong downward
//! penalty so the NLP doesn't back them off; generators the LP wanted
//! *lower than pmin* get a strong upward penalty so the NLP doesn't
//! crank them up.
//!
//! Two canonical providers populate these coefficients from the
//! source stage's solution:
//!
//! * [`DcReducedCostTargetTracking`] — reads the LP's pmin/pmax bound
//!   shadow prices (`pg_lower:<resource_id>` /
//!   `pg_upper:<resource_id>` constraint results). Exposed as TT-2 in
//!   Python's `runner._collect_dc_reduced_cost_overrides`.
//! * [`LmpMarginalCostTargetTracking`] — computes the LP's economic
//!   arbitrage (`LMP_at_bus − marginal_cost(Pg)`) from per-bus LMPs
//!   and the generator's cost curve. Works for any LP formulation
//!   (PWL, MIP, block mode) where column duals may be noisy. Exposed
//!   as TT-2b in Python's
//!   `runner._collect_lmp_marginal_cost_overrides`.
//!
//! Both implement [`FeedbackProvider`] and can be stacked on a
//! [`crate::RetryPolicy`]. When stacked, existing per-resource
//! overrides are preserved — the later-stacked provider only fills
//! gaps.

use std::collections::HashMap;

use surge_dispatch::request::AcDispatchTargetTrackingPair;
use surge_dispatch::{DispatchError, DispatchRequest, DispatchSolution};
use surge_io::go_c3::types::GoC3DeviceType;
use surge_io::go_c3::{GoC3Context, GoC3Problem};

use crate::ac_refinement::{FeedbackCtx, FeedbackProvider};

// ─── Shared config ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TargetTrackingBoundPenalties {
    pub upward_per_mw2: f64,
    pub downward_per_mw2: f64,
}

/// Per-direction coefficient presets: interior (symmetric),
/// economically-at-pmax (strong downward), economically-at-pmin
/// (strong upward).
#[derive(Clone, Debug)]
pub struct TargetTrackingPenaltyPresets {
    pub interior_per_mw2: f64,
    pub at_pmax: TargetTrackingBoundPenalties,
    pub at_pmin: TargetTrackingBoundPenalties,
}

impl Default for TargetTrackingPenaltyPresets {
    fn default() -> Self {
        // Tracking disabled — all coefficients zero. The AC NLP sees no
        // pullback toward the DC SCUC schedule and is free to redispatch
        // within the pinning + ramp envelope.
        Self {
            interior_per_mw2: 0.0,
            at_pmax: TargetTrackingBoundPenalties {
                upward_per_mw2: 0.0,
                downward_per_mw2: 0.0,
            },
            at_pmin: TargetTrackingBoundPenalties {
                upward_per_mw2: 0.0,
                downward_per_mw2: 0.0,
            },
        }
    }
}

// ─── DC reduced-cost provider (TT-2) ────────────────────────────────

/// Reads the LP's `pg_upper:<rid>` / `pg_lower:<rid>` constraint
/// shadows from the source stage's solution and sets per-direction
/// tracking penalties for each generator pinned at a bound.
///
/// Conservative aggregation across periods: if a generator is pinned
/// in any period it gets the stronger penalty for every period — the
/// AC OPF must not exploit a loose period to undo the LP's intent in
/// another. Interior generators (seen but never pinned) get the
/// symmetric interior default so all gens have consistent tracking.
#[derive(Clone, Debug)]
pub struct DcReducedCostTargetTracking {
    pub shadow_tolerance_dollars_per_mw: f64,
    pub min_pg_mw_for_override: f64,
    pub shadow_scale_dollars_per_mw: Option<f64>,
    pub presets: TargetTrackingPenaltyPresets,
}

impl Default for DcReducedCostTargetTracking {
    fn default() -> Self {
        Self {
            shadow_tolerance_dollars_per_mw: 10.0,
            min_pg_mw_for_override: 0.5,
            shadow_scale_dollars_per_mw: None,
            presets: TargetTrackingPenaltyPresets::default(),
        }
    }
}

impl FeedbackProvider for DcReducedCostTargetTracking {
    fn name(&self) -> &str {
        "dc_reduced_cost_target_tracking"
    }

    fn augment(
        &self,
        ctx: &FeedbackCtx,
        request: &mut DispatchRequest,
    ) -> Result<(), DispatchError> {
        let Some(source) = ctx.prior_stage_solution else {
            return Ok(());
        };
        let overrides = collect_dc_reduced_cost_overrides(source, self);
        merge_into_request(request, overrides);
        Ok(())
    }
}

fn collect_dc_reduced_cost_overrides(
    source: &DispatchSolution,
    cfg: &DcReducedCostTargetTracking,
) -> HashMap<String, AcDispatchTargetTrackingPair> {
    use std::collections::HashSet;

    let mut out: HashMap<String, AcDispatchTargetTrackingPair> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for period in source.periods() {
        // Per-period Pg lookup — filter shadow-driven overrides to
        // generators that are actually running.
        let mut pg_by_resource: HashMap<&str, f64> = HashMap::new();
        for r in period.resource_results() {
            seen.insert(r.resource_id.clone());
            pg_by_resource.insert(r.resource_id.as_str(), r.power_mw);
        }

        for cr in period.constraint_results() {
            let Some(shadow) = cr.shadow_price else {
                continue;
            };
            if shadow.abs() < cfg.shadow_tolerance_dollars_per_mw {
                continue;
            }

            let (is_upper, resource_id) =
                if let Some(rid) = cr.constraint_id.strip_prefix("pg_upper:") {
                    (true, rid)
                } else if let Some(rid) = cr.constraint_id.strip_prefix("pg_lower:") {
                    (false, rid)
                } else {
                    continue;
                };
            if resource_id.is_empty() {
                continue;
            }
            let pg_here = pg_by_resource.get(resource_id).copied().unwrap_or(0.0);
            if pg_here.abs() < cfg.min_pg_mw_for_override {
                continue;
            }

            let (mut up, mut down) = if is_upper {
                (
                    cfg.presets.at_pmax.upward_per_mw2,
                    cfg.presets.at_pmax.downward_per_mw2,
                )
            } else {
                (
                    cfg.presets.at_pmin.upward_per_mw2,
                    cfg.presets.at_pmin.downward_per_mw2,
                )
            };
            if let Some(scale) = cfg.shadow_scale_dollars_per_mw {
                if scale > 0.0 {
                    if is_upper {
                        down += scale * shadow.abs();
                    } else {
                        up += scale * shadow.abs();
                    }
                }
            }
            bump(&mut out, resource_id, up, down);
            seen.insert(resource_id.to_string());
        }
    }

    // Fill interior defaults for seen-but-unpinned generators.
    let interior_pair = AcDispatchTargetTrackingPair {
        upward_per_mw2: cfg.presets.interior_per_mw2,
        downward_per_mw2: cfg.presets.interior_per_mw2,
    };
    for resource_id in seen {
        out.entry(resource_id).or_insert_with(|| interior_pair);
    }
    out
}

// ─── LMP − marginal cost provider (TT-2b) ───────────────────────────

/// Reads per-bus LMPs from the source solution, per-generator
/// marginal cost from a precomputed cost-curve snapshot, and derives
/// per-direction tracking penalties from the arbitrage
/// `LMP_at_bus − MC(Pg)`.
///
/// Unlike [`DcReducedCostTargetTracking`] this works for any LP
/// formulation (PWL, MIP, block mode) where column duals may be
/// noisy or zero. The cost-curve snapshot is built by a format
/// adapter (typically via [`LmpMarginalCostTargetTracking::for_go_c3`]);
/// each period's per-producer cost blocks are stored as
/// `(marginal_cost_per_mwh, block_size_mw)` pairs so the provider is
/// fully self-contained (no `&Problem` lifetime) and cheap to clone.
#[derive(Clone, Debug)]
pub struct LmpMarginalCostTargetTracking {
    pub arbitrage_tolerance_dollars_per_mwh: f64,
    pub min_pg_mw_for_override: f64,
    pub arbitrage_scale_per_dollar_per_mwh: Option<f64>,
    pub presets: TargetTrackingPenaltyPresets,
    /// `resource_id → bus_number` for producer-kind generators.
    pub bus_number_by_resource: HashMap<String, u32>,
    /// Per-resource per-period cost segments in converted units:
    /// `segments[resource_id][period] = Vec<(marginal_cost_$/MWh, block_size_MW)>`.
    pub cost_segments: HashMap<String, Vec<Vec<(f64, f64)>>>,
}

impl LmpMarginalCostTargetTracking {
    /// Construct a provider by sampling the GO C3 problem's
    /// per-period cost curves + the context's bus mapping. Produces
    /// a self-contained, cheap-to-clone, `'static` config.
    pub fn for_go_c3(problem: &GoC3Problem, context: &GoC3Context) -> Self {
        let base_mva = problem.network.general.base_norm_mva.max(1.0);
        let mut bus_number_by_resource = HashMap::new();
        for dev in &problem.network.simple_dispatchable_device {
            if dev.device_type != GoC3DeviceType::Producer {
                continue;
            }
            if let Some(&bus_number) = context.bus_uid_to_number.get(&dev.bus) {
                bus_number_by_resource.insert(dev.uid.clone(), bus_number);
            }
        }

        let mut cost_segments: HashMap<String, Vec<Vec<(f64, f64)>>> = HashMap::new();
        for ts in &problem.time_series_input.simple_dispatchable_device {
            let mut periods: Vec<Vec<(f64, f64)>> = Vec::with_capacity(ts.cost.len());
            for period_blocks in &ts.cost {
                let mut blocks: Vec<(f64, f64)> = Vec::with_capacity(period_blocks.len());
                for block in period_blocks {
                    if block.len() != 2 {
                        continue;
                    }
                    let mc = block[0] / base_mva;
                    let block_size_mw = block[1] * base_mva;
                    blocks.push((mc, block_size_mw));
                }
                periods.push(blocks);
            }
            cost_segments.insert(ts.uid.clone(), periods);
        }

        Self {
            arbitrage_tolerance_dollars_per_mwh: 1.0,
            min_pg_mw_for_override: 0.5,
            arbitrage_scale_per_dollar_per_mwh: None,
            presets: TargetTrackingPenaltyPresets::default(),
            bus_number_by_resource,
            cost_segments,
        }
    }

    fn marginal_cost_at_pg(&self, resource_id: &str, period: usize, pg_mw: f64) -> Option<f64> {
        let periods = self.cost_segments.get(resource_id)?;
        let blocks = periods.get(period)?;
        if blocks.is_empty() {
            return None;
        }
        let mut cumulative_mw = 0.0;
        for (mc, block_size_mw) in blocks {
            if *block_size_mw <= 1e-12 {
                continue;
            }
            let new_cumulative = cumulative_mw + block_size_mw;
            if pg_mw <= new_cumulative + 1e-9 {
                return Some(*mc);
            }
            cumulative_mw = new_cumulative;
        }
        blocks.last().map(|(mc, _)| *mc)
    }
}

impl FeedbackProvider for LmpMarginalCostTargetTracking {
    fn name(&self) -> &str {
        "lmp_marginal_cost_target_tracking"
    }

    fn augment(
        &self,
        ctx: &FeedbackCtx,
        request: &mut DispatchRequest,
    ) -> Result<(), DispatchError> {
        let Some(source) = ctx.prior_stage_solution else {
            return Ok(());
        };
        let overrides = collect_lmp_marginal_cost_overrides(source, self);
        merge_into_request(request, overrides);
        Ok(())
    }
}

fn collect_lmp_marginal_cost_overrides(
    source: &DispatchSolution,
    cfg: &LmpMarginalCostTargetTracking,
) -> HashMap<String, AcDispatchTargetTrackingPair> {
    let mut out: HashMap<String, AcDispatchTargetTrackingPair> = HashMap::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (period_idx, period) in source.periods().iter().enumerate() {
        let mut lmp_by_bus: HashMap<u32, f64> = HashMap::new();
        for b in period.bus_results() {
            lmp_by_bus.insert(b.bus_number, b.lmp);
        }

        for r in period.resource_results() {
            seen.insert(r.resource_id.clone());
            let pg_mw = r.power_mw;
            if pg_mw.abs() < cfg.min_pg_mw_for_override {
                continue;
            }
            let Some(&bus_number) = cfg.bus_number_by_resource.get(&r.resource_id) else {
                continue;
            };
            let Some(&lmp) = lmp_by_bus.get(&bus_number) else {
                continue;
            };
            let Some(mc) = cfg.marginal_cost_at_pg(&r.resource_id, period_idx, pg_mw) else {
                continue;
            };
            let arbitrage = lmp - mc;
            if arbitrage > cfg.arbitrage_tolerance_dollars_per_mwh {
                let up = cfg.presets.at_pmax.upward_per_mw2;
                let mut down = cfg.presets.at_pmax.downward_per_mw2;
                if let Some(scale) = cfg.arbitrage_scale_per_dollar_per_mwh {
                    if scale > 0.0 {
                        down += scale * arbitrage;
                    }
                }
                bump(&mut out, &r.resource_id, up, down);
            } else if arbitrage < -cfg.arbitrage_tolerance_dollars_per_mwh {
                let mut up = cfg.presets.at_pmin.upward_per_mw2;
                let down = cfg.presets.at_pmin.downward_per_mw2;
                if let Some(scale) = cfg.arbitrage_scale_per_dollar_per_mwh {
                    if scale > 0.0 {
                        up += scale * arbitrage.abs();
                    }
                }
                bump(&mut out, &r.resource_id, up, down);
            }
        }
    }

    let interior_pair = AcDispatchTargetTrackingPair {
        upward_per_mw2: cfg.presets.interior_per_mw2,
        downward_per_mw2: cfg.presets.interior_per_mw2,
    };
    for resource_id in seen {
        out.entry(resource_id).or_insert_with(|| interior_pair);
    }
    out
}

// ─── Shared helpers ─────────────────────────────────────────────────

fn bump(
    out: &mut HashMap<String, AcDispatchTargetTrackingPair>,
    resource_id: &str,
    upward: f64,
    downward: f64,
) {
    let entry = out
        .entry(resource_id.to_string())
        .or_insert(AcDispatchTargetTrackingPair {
            upward_per_mw2: 0.0,
            downward_per_mw2: 0.0,
        });
    if upward > entry.upward_per_mw2 {
        entry.upward_per_mw2 = upward;
    }
    if downward > entry.downward_per_mw2 {
        entry.downward_per_mw2 = downward;
    }
}

fn merge_into_request(
    request: &mut DispatchRequest,
    overrides: HashMap<String, AcDispatchTargetTrackingPair>,
) {
    if overrides.is_empty() {
        return;
    }
    let runtime = request.runtime_mut();
    let existing = &mut runtime
        .ac_target_tracking
        .generator_p_coefficients_overrides_by_id;
    // Caller-supplied (existing) entries win; provider only fills gaps.
    for (resource_id, pair) in overrides {
        existing.entry(resource_id).or_insert(pair);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interior_preset_fills_gaps() {
        let presets = TargetTrackingPenaltyPresets::default();
        assert_eq!(presets.interior_per_mw2, 0.0);
        assert_eq!(presets.at_pmax.upward_per_mw2, 0.0);
        assert_eq!(presets.at_pmax.downward_per_mw2, 0.0);
        assert_eq!(presets.at_pmin.upward_per_mw2, 0.0);
        assert_eq!(presets.at_pmin.downward_per_mw2, 0.0);
    }

    #[test]
    fn bump_takes_max_per_direction() {
        let mut out = HashMap::new();
        bump(&mut out, "g1", 100.0, 200.0);
        bump(&mut out, "g1", 50.0, 500.0);
        bump(&mut out, "g1", 300.0, 100.0);
        let pair = out.get("g1").unwrap();
        assert_eq!(pair.upward_per_mw2, 300.0);
        assert_eq!(pair.downward_per_mw2, 500.0);
    }
}
