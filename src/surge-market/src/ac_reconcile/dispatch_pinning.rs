// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Bandable-subset producer dispatch pinning.
//!
//! The canonical [`crate::workflow::pin_generator_dispatch_bounds`]
//! helper applies a uniform band around stage-1's dispatch to every
//! generator. That works for small synthetic tests but not for real
//! AC SCED convergence: the NLP needs most producers *hard-pinned* at
//! the DC target (so reactive flows don't reshuffle active power) and
//! a narrow *bandable subset* (slack-bus generators + the most Q-
//! capable non-slack generators) with a symmetric P band so they can
//! absorb reactive-corner mismatches.
//!
//! This helper replaces the canonical pin with a richer formulation:
//!
//! * `producer` resources get a per-period band `[target-b, target+b]`
//!   when they're in [`ProducerDispatchPinning::bandable_producer_resource_ids`]
//!   and a tight pin `[target, target]` otherwise.
//! * `producer_static` resources (renewables, synthetic HVDC
//!   terminal-Q generators, etc.) get pinned to `[0, 0]` — their P
//!   is modelled elsewhere (load injections).
//! * For bandable producers, upward/downward reserve awards shrink
//!   the available band so the AC pass can't re-use headroom the DC
//!   pass already committed to active reserves.
//! * When `relax_pmin` is true (second-pass fallback), the lower bound
//!   floors at zero instead of the physical per-period p_min.
//! * Startup/shutdown trajectory periods (committed==false but target>0)
//!   use a special single-point pin that matches the Python
//!   `startup_shutdown_pin` path.
//!
//! The pinning applies to the request's existing
//! `generator_dispatch_bounds` profiles. Each profile's per-period
//! `p_min_mw` / `p_max_mw` are overwritten in place.

use std::collections::{HashMap, HashSet};

use surge_dispatch::{DispatchRequest, DispatchSolution, ResourcePeriodDetail};

/// Per-producer ramp-rate pair in MW per hour. Used by
/// [`ProducerDispatchPinning::ramp_limits_mw_per_hr`] to tighten the
/// per-period band so the AC SCED doesn't introduce inter-period
/// ramp violations the source stage's dispatch didn't have.
#[derive(Clone, Copy, Debug)]
pub struct RampLimits {
    pub ramp_up_mw_per_hr: f64,
    pub ramp_down_mw_per_hr: f64,
}

/// Producer dispatch-pinning configuration.
#[derive(Clone, Debug, Default)]
pub struct ProducerDispatchPinning {
    /// Resource IDs classified as producers with real P dispatch.
    pub producer_resource_ids: HashSet<String>,

    /// Resource IDs classified as producer_static (renewables /
    /// synthetic reactive gens). Pinned to `[0, 0]` for P.
    pub producer_static_resource_ids: HashSet<String>,

    /// The bandable subset — receives a symmetric P band around the
    /// source stage's dispatch. Everything else in
    /// `producer_resource_ids` gets tight-pinned.
    pub bandable_producer_resource_ids: HashSet<String>,

    /// Band width = `clamp(|target| * band_fraction, band_floor_mw,
    /// band_cap_mw)`. Applied symmetrically around target.
    pub band_fraction: f64,
    pub band_floor_mw: f64,
    pub band_cap_mw: f64,

    /// Up-direction active reserve products whose awards count
    /// against the upper band (shrink headroom). For example
    /// `{"reg_up", "syn", "ramp_up_on"}`.
    pub up_reserve_product_ids: HashSet<String>,

    /// Down-direction active reserve products (shrink footroom).
    pub down_reserve_product_ids: HashSet<String>,

    /// When true, the reserve-award shrink runs for bandable
    /// producers. Tight-pinned producers always skip it (shrink
    /// would be a no-op on zero-width bounds).
    pub apply_reserve_shrink: bool,

    /// Per-producer ramp limits (MW per hour). When present, the
    /// per-period band is intersected with the inter-period ramp
    /// envelope from the source stage's neighbouring dispatches —
    /// `[dc_p[t-1] − ramp_down·dt[t], dc_p[t-1] + ramp_up·dt[t]]`
    /// from the previous period and the symmetric pair from the next.
    /// Without this tightening the AC SCED's per-period band can
    /// move `p[t]` enough to violate inter-period ramp constraints
    /// the source stage's solution did not violate. Resources
    /// missing from this map keep the unconditional band.
    pub ramp_limits_mw_per_hr: HashMap<String, RampLimits>,

    /// Second-pass fallback: floor p_min at 0 instead of the physical
    /// per-period p_min. Applied to every producer.
    pub relax_pmin: bool,

    /// Per-resource relax_pmin override — applies even when global
    /// `relax_pmin` is false.
    pub relax_pmin_for_resources: HashSet<String>,

    /// Anchor set — resources that bypass all band-narrowing logic for
    /// this pinning pass and keep their full physical `[P_min, P_max]`.
    /// Used by the retry grid's last-ditch anchor-widest-Q fallback so
    /// a handful of high-Q-range generators can absorb the imbalance
    /// when the narrow-band and wide-band attempts both failed.
    pub anchor_resource_ids: HashSet<String>,
}

/// Apply the bandable-subset producer dispatch pinning to the
/// request's `generator_dispatch_bounds` profiles, using the source
/// stage's solution for dispatch targets + reserve awards + commitment
/// flags.
pub fn apply_producer_dispatch_pinning(
    request: &mut DispatchRequest,
    source_solution: &DispatchSolution,
    config: &ProducerDispatchPinning,
) {
    let timeline = request.timeline().clone();
    let periods = timeline.periods;
    let source_periods = source_solution.periods();

    // Per-period interval duration in hours. The timeline carries an
    // explicit `interval_hours_by_period` for non-uniform horizons
    // (for example, sub-hourly day-ahead cases) and falls back to
    // `interval_hours` (uniform) when the per-period vector is empty.
    let dt_for_period = |period_idx: usize| -> f64 {
        timeline
            .interval_hours_by_period
            .get(period_idx)
            .copied()
            .filter(|v| *v > 0.0)
            .unwrap_or(timeline.interval_hours)
    };

    // Build per-resource per-period dispatch targets + reserve awards
    // + commitment flags from the source solution.
    let dispatch_targets = collect_source_dispatch_targets(source_periods);

    let profiles = request.profiles_mut();
    for entry in profiles.generator_dispatch_bounds.profiles.iter_mut() {
        let resource_id = entry.resource_id.clone();
        let is_producer = config.producer_resource_ids.contains(&resource_id);
        let is_producer_static = config.producer_static_resource_ids.contains(&resource_id);
        if !is_producer && !is_producer_static {
            continue;
        }

        let relax_this_resource =
            config.relax_pmin || config.relax_pmin_for_resources.contains(&resource_id);
        let is_bandable = config.bandable_producer_resource_ids.contains(&resource_id);
        // Anchors bypass all narrowing — they keep the physical envelope
        // so the AC NLP has full `[P_min, P_max]` flexibility for each
        // period. Runs before the other branches so anchors win even if
        // they happen to also be in the bandable set.
        let is_anchor = config.anchor_resource_ids.contains(&resource_id);

        let existing_min = entry.p_min_mw.clone();
        let existing_max = entry.p_max_mw.clone();

        if is_anchor && !is_producer_static {
            // Leave the profile's P bounds untouched.
            continue;
        }

        let mut new_min: Vec<f64> = Vec::with_capacity(periods);
        let mut new_max: Vec<f64> = Vec::with_capacity(periods);

        for period_idx in 0..periods {
            let physical_lb = existing_min.get(period_idx).copied().unwrap_or(0.0);
            let physical_ub_raw = existing_max.get(period_idx).copied().unwrap_or(0.0);
            let physical_ub = physical_ub_raw.max(physical_lb);

            if is_producer_static {
                new_min.push(0.0);
                new_max.push(0.0);
                continue;
            }

            // Producer path: read target/commitment/awards from source.
            let source_entry = dispatch_targets.get(&(resource_id.clone(), period_idx));
            let target_p_mw = source_entry.map(|e| e.power_mw.max(0.0)).unwrap_or(0.0);
            let committed = source_entry.map(|e| e.committed).unwrap_or(false);
            let startup_shutdown = source_entry
                .map(|e| e.startup || e.shutdown)
                .unwrap_or(false);
            let offline_trajectory_active = !committed && target_p_mw > 1e-9;
            let is_trajectory_period = startup_shutdown || offline_trajectory_active;

            if is_trajectory_period {
                let floor = if relax_this_resource || offline_trajectory_active {
                    0.0
                } else {
                    physical_lb
                };
                let ceiling = physical_ub.max(floor);
                let target_clamped = target_p_mw.clamp(floor, ceiling);
                new_min.push(target_clamped);
                new_max.push(target_clamped);
                continue;
            }

            if !is_bandable {
                // Tight pin at DC target, clamped into the physical
                // envelope. When the pre-pinned request has already
                // narrowed `[p_min, p_max]` below the DC target (e.g.
                // winner-roundtrip diagnostics), respecting the envelope
                // is required — escaping it would reintroduce the very
                // drift the pre-pin was built to prevent.
                let floor = if relax_this_resource {
                    0.0
                } else {
                    physical_lb
                };
                let ceiling = physical_ub.max(floor);
                let target_clamped = target_p_mw.clamp(floor, ceiling);
                new_min.push(target_clamped);
                new_max.push(target_clamped);
                continue;
            }

            // Bandable producer: symmetric band around target.
            let band_mw = (target_p_mw.abs() * config.band_fraction)
                .max(config.band_floor_mw)
                .min(config.band_cap_mw);
            let floor = if relax_this_resource {
                0.0
            } else {
                physical_lb
            };
            let mut lower = (target_p_mw - band_mw).max(floor);
            let mut upper = (target_p_mw + band_mw).min(physical_ub);

            if config.apply_reserve_shrink {
                let mut up_award_mw = 0.0;
                let mut down_award_mw = 0.0;
                if let Some(entry) = source_entry {
                    for (product_id, award_mw) in &entry.reserve_awards {
                        if *award_mw <= 0.0 {
                            continue;
                        }
                        if config.up_reserve_product_ids.contains(product_id) {
                            up_award_mw += award_mw;
                        } else if config.down_reserve_product_ids.contains(product_id) {
                            down_award_mw += award_mw;
                        }
                    }
                }
                if up_award_mw > 0.0 {
                    upper = upper.min(physical_ub - up_award_mw);
                }
                if down_award_mw > 0.0 {
                    lower = lower.max(physical_lb + down_award_mw);
                }
            }

            // Ramp-aware band tightening. The AC SCED is solved
            // period-by-period and doesn't enforce inter-period ramp
            // constraints itself, so a wide band can introduce ramp
            // violations the source-stage solution didn't have. Pin
            // p[t] into the envelope reachable from the source's
            // p[t-1] and p[t+1] under the unit's ramp limits, so
            // re-solving p[t] cannot violate ramp from either
            // neighbour's source dispatch.
            //
            // This is conservative — it doesn't account for the
            // neighbour's own band drift — but on ramp-binding
            // generators the source dispatch sits exactly at the ramp
            // boundary, so the tightening collapses the band to a
            // single point (the source target itself), which is
            // exactly the behaviour we want.
            if let Some(limits) = config.ramp_limits_mw_per_hr.get(&resource_id) {
                let dt_curr = dt_for_period(period_idx);
                if period_idx > 0 {
                    let prev_target = dispatch_targets
                        .get(&(resource_id.clone(), period_idx - 1))
                        .map(|e| e.power_mw.max(0.0));
                    if let Some(prev) = prev_target {
                        let max_from_prev = prev + limits.ramp_up_mw_per_hr * dt_curr;
                        let min_from_prev = prev - limits.ramp_down_mw_per_hr * dt_curr;
                        upper = upper.min(max_from_prev);
                        lower = lower.max(min_from_prev);
                    }
                }
                if period_idx + 1 < periods {
                    let next_target = dispatch_targets
                        .get(&(resource_id.clone(), period_idx + 1))
                        .map(|e| e.power_mw.max(0.0));
                    if let Some(next) = next_target {
                        let dt_next = dt_for_period(period_idx + 1);
                        // For p[t+1] = next to be reachable from p[t]:
                        //   next - p[t] <= ramp_up · dt[t+1]  → p[t] >= next − ramp_up·dt_next
                        //   p[t] - next <= ramp_down · dt[t+1] → p[t] <= next + ramp_down·dt_next
                        let max_from_next = next + limits.ramp_down_mw_per_hr * dt_next;
                        let min_from_next = next - limits.ramp_up_mw_per_hr * dt_next;
                        upper = upper.min(max_from_next);
                        lower = lower.max(min_from_next);
                    }
                }
            }

            // Final clip + degeneracy guard. The guard clamps the
            // target into the envelope instead of letting `lower`
            // escape `physical_ub` — preserving a pre-pinned narrow
            // envelope set by an earlier helper.
            let floor = if relax_this_resource {
                0.0
            } else {
                physical_lb
            };
            let ceiling = physical_ub.max(floor);
            lower = lower.clamp(floor, ceiling);
            upper = upper.clamp(floor, ceiling);
            if upper < lower {
                let mid = target_p_mw.clamp(floor, ceiling);
                lower = mid;
                upper = mid;
            }
            new_min.push(lower);
            new_max.push(upper);
        }

        entry.p_min_mw = new_min;
        entry.p_max_mw = new_max;
    }
}

struct SourceDispatchEntry {
    power_mw: f64,
    committed: bool,
    startup: bool,
    shutdown: bool,
    reserve_awards: Vec<(String, f64)>,
}

fn collect_source_dispatch_targets(
    periods: &[surge_dispatch::DispatchPeriodResult],
) -> HashMap<(String, usize), SourceDispatchEntry> {
    let mut out = HashMap::new();
    for (period_idx, period) in periods.iter().enumerate() {
        for r in period.resource_results() {
            let (committed, startup, shutdown) =
                if let ResourcePeriodDetail::Generator(detail) = &r.detail {
                    (
                        detail.commitment.unwrap_or(false),
                        detail.startup.unwrap_or(false),
                        detail.shutdown.unwrap_or(false),
                    )
                } else {
                    (false, false, false)
                };
            let awards: Vec<(String, f64)> = r
                .reserve_awards
                .iter()
                .map(|(product_id, mw)| (product_id.clone(), *mw))
                .collect();
            out.insert(
                (r.resource_id.clone(), period_idx),
                SourceDispatchEntry {
                    power_mw: r.power_mw,
                    committed,
                    startup,
                    shutdown,
                    reserve_awards: awards,
                },
            );
        }
    }
    out
}
