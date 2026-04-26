// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Per-period marginal-loss penalty factors for the system-row SCUC path.
//!
//! Used when `scuc_disable_bus_power_balance = true` and the policy
//! selects [`ScucLossTreatment::PenaltyFactors`]. The flow is:
//!
//! 1. **Security iter `k` solves SCUC** with a static or warm-started
//!    loss budget on the system row.
//! 2. **`repair_theta_from_dc_pf`** computes physical angles consistent
//!    with the SCUC dispatch (already runs every iter for screening).
//! 3. **`compute_realized_loss_factors`** (this module) reads those
//!    angles, hits each period's loss-PTDF, and returns a
//!    `LossFactorWarmStart` populated with:
//!    * `dloss_dp[t][bus]` — per-bus marginal loss factor in
//!      distributed-load-slack gauge.
//!    * `total_losses_mw[t]` — realized total system loss at the
//!      repaired theta.
//! 4. **`blend_with_prior`** damps the new state against the prior
//!    iter's cache (asymmetric: ramps up faster than down — under-
//!    commitment costs more than over because AC SCED can't commit new
//!    units to recover).
//! 5. **Magnitude cap** keeps any single LF inside `[-cap, +cap]` (default
//!    `0.15`) — pathological PTDF outliers on weak corridors can produce
//!    unstable LFs that flip commitment back and forth between security
//!    iterations.
//! 6. The blended, capped state is threaded back into
//!    `ScucProblemBuildInput::sys_row_loss_override` for iter `k+1`.
//!
//! Gauge fix (distributed-load slack): `compute_dc_loss_sensitivities`
//! returns LFs against the network's implicit slack reference. We
//! convert to distributed-load gauge by subtracting the load-weighted
//! mean: `LF_distributed[i] = LF_raw[i] − Σ_j (load_share[j] · LF_raw[j])`.
//! After this, `Σ_i load_share[i] · LF_distributed[i] = 0` — i.e. loads
//! collectively have zero net penalty factor, which is the natural
//! reference for a balance row that pays loss out of generation.

use std::collections::HashMap;

use surge_dc::PtdfRows;
use surge_network::Network;
use tracing::warn;

use super::losses::LossFactorWarmStart;

/// Default magnitude cap on any single per-bus loss factor. Pathological
/// PTDF outliers on weak corridors can produce LFs in the 0.3–1.0 range
/// which destabilize commitment between security iterations. 0.15 is
/// loose enough to capture normal locational variation (lossy radials,
/// stressed corridors) and tight enough that commitment-flips driven by
/// LF noise are bounded.
pub(super) const DEFAULT_LF_MAGNITUDE_CAP: f64 = 0.15;

/// Default damping weight on the realized LFs when blending against the
/// prior iter's cache. `α = 0.5` averages the two halves.
pub(super) const DEFAULT_DAMPING_ALPHA: f64 = 0.5;

/// Default upward bias when blending realized → cache. Whenever the new
/// realized LF exceeds the prior, we accept it directly (no damping
/// down) — this implements the asymmetry: under-commitment hurts more
/// than over because AC SCED can't recover. `min` is applied
/// elementwise to keep the upward step bounded in case of a one-period
/// outlier.
pub(super) const DEFAULT_UPWARD_STEP_CAP: f64 = 0.10;

/// Compute realized per-period penalty factors and total losses from a
/// SCUC solution's repaired theta. Returns a `LossFactorWarmStart`
/// suitable for caching into the next security iteration's
/// `sys_row_loss_override`.
///
/// Inputs:
/// * `hourly_networks` — one network per period (with this period's
///   topology and dispatch already applied).
/// * `bus_maps` — one `bus_map` per period, matching the layout used by
///   `repair_theta_from_dc_pf`. Caller passes the same map both in
///   theta extraction and here so `dloss_dp[t][bus_idx]` indexes the
///   right buses.
/// * `theta_by_hour` — `[t][bus_idx]` repaired DC PF angles (radians).
/// * `loss_ptdf_by_hour` — per-period loss-PTDF rows, used as the
///   sensitivity matrix for `compute_dc_loss_sensitivities`. Caller
///   precomputes once per security iter.
///
/// Output: `LossFactorWarmStart` with `dloss_dp` in distributed-load
/// slack gauge and `total_losses_mw` in MW. NOT yet damped or capped —
/// pipe through `blend_with_prior` and `cap_magnitudes` next.
pub(super) fn compute_realized_loss_factors(
    hourly_networks: &[Network],
    bus_maps: &[HashMap<u32, usize>],
    theta_by_hour: &[Vec<f64>],
    loss_ptdf_by_hour: &[PtdfRows],
) -> LossFactorWarmStart {
    let n_hours = hourly_networks.len();
    debug_assert_eq!(bus_maps.len(), n_hours);
    debug_assert_eq!(theta_by_hour.len(), n_hours);
    debug_assert_eq!(loss_ptdf_by_hour.len(), n_hours);

    let mut dloss_dp: Vec<Vec<f64>> = Vec::with_capacity(n_hours);
    let mut total_losses_mw: Vec<f64> = Vec::with_capacity(n_hours);

    for t in 0..n_hours {
        let net = &hourly_networks[t];
        let theta = &theta_by_hour[t];
        let bus_map = &bus_maps[t];
        let n_bus = theta.len();
        if n_bus == 0 {
            dloss_dp.push(Vec::new());
            total_losses_mw.push(0.0);
            continue;
        }

        // Raw single-bus-slack LFs from the realized branch flows.
        let lf_raw = surge_opf::advanced::compute_dc_loss_sensitivities(
            net,
            theta,
            bus_map,
            &loss_ptdf_by_hour[t],
        );

        // Convert to distributed-load slack: subtract load-weighted
        // mean so Σ load_share · LF = 0. This makes the LFs gauge-
        // invariant against the network's implicit slack choice.
        let bus_load_mw = net.bus_load_p_mw_with_map(bus_map);
        let total_load_mw: f64 = bus_load_mw.iter().copied().sum();
        let lf_mean = if total_load_mw > 1e-6 {
            let mut acc = 0.0_f64;
            for i in 0..n_bus {
                let share = bus_load_mw.get(i).copied().unwrap_or(0.0).max(0.0) / total_load_mw;
                acc += share * lf_raw.get(i).copied().unwrap_or(0.0);
            }
            acc
        } else {
            // No load → fall back to uniform mean. (Edge case: a period
            // with all-DR loads where `network.loads` is empty is
            // already covered upstream because `bus_load_p_mw_with_map`
            // is called consistently with the dispatch builder.)
            let n = n_bus as f64;
            lf_raw.iter().sum::<f64>() / n
        };
        let lf_dist: Vec<f64> = lf_raw.iter().map(|&x| x - lf_mean).collect();
        dloss_dp.push(lf_dist);

        // Total realized DC losses in MW.
        let loss_pu = surge_opf::compute_total_dc_losses(net, theta, bus_map);
        total_losses_mw.push(loss_pu * net.base_mva);
    }

    LossFactorWarmStart {
        dloss_dp,
        total_losses_mw,
    }
}

/// Damp realized LFs + losses against a prior iter's cache.
///
/// Asymmetric: when realized > prior we accept the realized value
/// directly (capped by `upward_step_cap`); when realized ≤ prior we
/// blend `(1−α)·prior + α·realized`. Rationale: over-budgeting losses
/// just commits an extra cheap unit AC SCED can dispatch down;
/// under-budgeting forces SCED into bus-balance slack penalty because
/// it can't commit new units. So we ramp up fast, ramp down slow.
///
/// Operates on both `dloss_dp` (per-bus LFs) and `total_losses_mw`
/// (per-period scalars) with the same rule.
///
/// `alpha`: damping weight on the realized side (0 = freeze prior, 1 =
/// trust realized). Typical 0.5.
///
/// `upward_step_cap`: when realized exceeds prior, the new value is
/// `min(realized, prior + upward_step_cap)`. Bounds outlier upward
/// steps so a single bad period can't whipsaw commitment.
pub(super) fn blend_with_prior(
    realized: &LossFactorWarmStart,
    prior: Option<&LossFactorWarmStart>,
    alpha: f64,
    upward_step_cap: f64,
) -> LossFactorWarmStart {
    let alpha = alpha.clamp(0.0, 1.0);
    let one_minus_alpha = 1.0 - alpha;

    let prior_populated = prior.map(|p| p.is_populated()).unwrap_or(false);
    if !prior_populated {
        // No prior — accept realized as-is. Caller still applies
        // `cap_magnitudes` downstream.
        return realized.clone();
    }
    let prior = prior.expect("prior_populated");

    let n_hours = realized.dloss_dp.len();
    if n_hours != prior.dloss_dp.len() || n_hours != realized.total_losses_mw.len() {
        // Shape mismatch (e.g. caller passed a stale cache from a
        // different horizon). Drop the prior and trust realized.
        warn!(
            n_hours_realized = n_hours,
            n_hours_prior = prior.dloss_dp.len(),
            "PenaltyFactors: prior cache shape mismatch — using realized as-is"
        );
        return realized.clone();
    }

    let mut dloss_dp: Vec<Vec<f64>> = Vec::with_capacity(n_hours);
    let mut total_losses_mw: Vec<f64> = Vec::with_capacity(n_hours);
    for t in 0..n_hours {
        let r = &realized.dloss_dp[t];
        let p = &prior.dloss_dp[t];
        let mut blended = Vec::with_capacity(r.len());
        for (i, &realized_i) in r.iter().enumerate() {
            let prior_i = p.get(i).copied().unwrap_or(0.0);
            blended.push(asymmetric_blend(
                realized_i,
                prior_i,
                alpha,
                one_minus_alpha,
                upward_step_cap,
            ));
        }
        dloss_dp.push(blended);

        let realized_loss = realized.total_losses_mw[t];
        let prior_loss = prior.total_losses_mw.get(t).copied().unwrap_or(0.0);
        // Use a wider absolute step cap on total loss MW than on per-bus
        // LFs — the loss MW is on a different scale (MW, not unitless)
        // and the LFs are already capped per-bus elsewhere.
        let abs_step_cap = (prior_loss.abs() * 0.5).max(upward_step_cap * 1000.0);
        total_losses_mw.push(asymmetric_blend(
            realized_loss,
            prior_loss,
            alpha,
            one_minus_alpha,
            abs_step_cap,
        ));
    }

    LossFactorWarmStart {
        dloss_dp,
        total_losses_mw,
    }
}

#[inline]
fn asymmetric_blend(
    realized: f64,
    prior: f64,
    alpha: f64,
    one_minus_alpha: f64,
    upward_step_cap: f64,
) -> f64 {
    if realized > prior {
        // Ramp up fast: jump to realized but cap the per-iter step.
        let max_allowed = prior + upward_step_cap;
        realized.min(max_allowed)
    } else {
        // Ramp down slow: damped blend toward realized.
        one_minus_alpha * prior + alpha * realized
    }
}

/// Clamp every per-bus LF magnitude to `[-cap, +cap]` in place. Returns
/// the count of entries that hit either rail (a useful signal — large
/// counts mean the cap is biting and the LFs are noisy or the network
/// has stressed corridors).
pub(super) fn cap_magnitudes(state: &mut LossFactorWarmStart, cap: f64) -> usize {
    let cap = cap.abs();
    let mut hits = 0usize;
    for period_lfs in state.dloss_dp.iter_mut() {
        for lf in period_lfs.iter_mut() {
            if *lf > cap {
                *lf = cap;
                hits += 1;
            } else if *lf < -cap {
                *lf = -cap;
                hits += 1;
            }
        }
    }
    hits
}

/// Summary statistics over a `LossFactorWarmStart` for telemetry.
/// `(min, max, mean, p95_abs)` per period for `dloss_dp`, plus the
/// per-period total loss MW.
#[derive(Debug, Clone)]
pub(super) struct LossFactorStats {
    pub per_period: Vec<PeriodLossFactorStats>,
}

#[derive(Debug, Clone)]
pub(super) struct PeriodLossFactorStats {
    /// Period index this row applies to. Read by callers that flatten
    /// `LossFactorStats.per_period` into telemetry vectors keyed by
    /// period; harmless on direct iteration.
    #[allow(dead_code)]
    pub period: usize,
    pub lf_min: f64,
    pub lf_max: f64,
    /// Mean per-bus LF over the period. Not currently surfaced in the
    /// `SecurityIterationLossTelemetry` JSON, but kept for future
    /// diagnostic extension and tests; drop the `allow` when consumed.
    #[allow(dead_code)]
    pub lf_mean: f64,
    pub lf_p95_abs: f64,
    pub total_losses_mw: f64,
}

pub(super) fn summarize(state: &LossFactorWarmStart) -> LossFactorStats {
    let mut per_period = Vec::with_capacity(state.dloss_dp.len());
    for (t, lfs) in state.dloss_dp.iter().enumerate() {
        let total_losses_mw = state.total_losses_mw.get(t).copied().unwrap_or(0.0);
        if lfs.is_empty() {
            per_period.push(PeriodLossFactorStats {
                period: t,
                lf_min: 0.0,
                lf_max: 0.0,
                lf_mean: 0.0,
                lf_p95_abs: 0.0,
                total_losses_mw,
            });
            continue;
        }
        let mut lf_min = f64::INFINITY;
        let mut lf_max = f64::NEG_INFINITY;
        let mut lf_sum = 0.0_f64;
        let mut abs_sorted: Vec<f64> = lfs.iter().map(|x| x.abs()).collect();
        for &x in lfs.iter() {
            if x < lf_min {
                lf_min = x;
            }
            if x > lf_max {
                lf_max = x;
            }
            lf_sum += x;
        }
        abs_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = abs_sorted.len();
        let p95_idx = ((n as f64) * 0.95).floor() as usize;
        let p95_idx = p95_idx.min(n - 1);
        per_period.push(PeriodLossFactorStats {
            period: t,
            lf_min,
            lf_max,
            lf_mean: lf_sum / (n as f64),
            lf_p95_abs: abs_sorted[p95_idx],
            total_losses_mw,
        });
    }
    LossFactorStats { per_period }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_from(dloss: Vec<Vec<f64>>, losses_mw: Vec<f64>) -> LossFactorWarmStart {
        LossFactorWarmStart {
            dloss_dp: dloss,
            total_losses_mw: losses_mw,
        }
    }

    #[test]
    fn cap_magnitudes_clamps_both_rails() {
        let mut s = ws_from(vec![vec![0.5, -0.3, 0.05, -0.2, 0.0]], vec![10.0]);
        let hits = cap_magnitudes(&mut s, 0.15);
        assert_eq!(hits, 3);
        assert!((s.dloss_dp[0][0] - 0.15).abs() < 1e-12);
        assert!((s.dloss_dp[0][1] + 0.15).abs() < 1e-12);
        assert!((s.dloss_dp[0][2] - 0.05).abs() < 1e-12);
        assert!((s.dloss_dp[0][3] + 0.15).abs() < 1e-12);
    }

    #[test]
    fn asymmetric_blend_ramps_up_fast_down_slow() {
        // realized > prior: jump to realized, capped by step.
        assert!((asymmetric_blend(0.20, 0.10, 0.5, 0.5, 0.10) - 0.20).abs() < 1e-12);
        assert!((asymmetric_blend(0.30, 0.10, 0.5, 0.5, 0.10) - 0.20).abs() < 1e-12);
        // realized < prior: damped blend.
        assert!((asymmetric_blend(0.05, 0.10, 0.5, 0.5, 0.10) - 0.075).abs() < 1e-12);
    }

    #[test]
    fn blend_with_prior_no_prior_returns_realized() {
        let realized = ws_from(vec![vec![0.05, 0.10]], vec![5.0]);
        let blended = blend_with_prior(&realized, None, 0.5, 0.10);
        assert_eq!(blended.dloss_dp, realized.dloss_dp);
        assert_eq!(blended.total_losses_mw, realized.total_losses_mw);
    }

    #[test]
    fn blend_with_prior_shape_mismatch_falls_back_to_realized() {
        let realized = ws_from(vec![vec![0.05]], vec![5.0]);
        let prior = ws_from(vec![vec![0.0], vec![0.0]], vec![1.0, 2.0]); // n_hours=2 vs realized's 1
        let blended = blend_with_prior(&realized, Some(&prior), 0.5, 0.10);
        assert_eq!(blended.dloss_dp, realized.dloss_dp);
    }

    #[test]
    fn realized_lfs_satisfy_distributed_load_gauge() {
        // 3-bus radial loss case: gen at slack bus 1, loads at 2 and 3.
        // After distributed-load slack normalization, the load-share-
        // weighted average of LFs must be (close to) zero — that's the
        // gauge condition we're imposing.
        use surge_dc::{PtdfRequest, compute_ptdf, solve_dc};
        use surge_network::Network;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("pf-3bus");
        net.buses.extend([
            Bus::new(1, BusType::Slack, 230.0),
            Bus::new(2, BusType::PQ, 230.0),
            Bus::new(3, BusType::PQ, 230.0),
        ]);
        net.generators.push(Generator::new(1, 120.0, 1.0));
        net.loads.push(Load::new(2, 70.0, 0.0));
        net.loads.push(Load::new(3, 50.0, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.02, 0.10, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.03, 0.12, 0.0));

        let bus_map = net.bus_index_map();
        let theta = solve_dc(&net).expect("DC PF").theta;
        let monitored: Vec<usize> = (0..net.n_branches()).collect();
        let ptdf = compute_ptdf(&net, &PtdfRequest::for_branches(&monitored)).expect("PTDF");

        let realized = compute_realized_loss_factors(
            std::slice::from_ref(&net),
            std::slice::from_ref(&bus_map),
            std::slice::from_ref(&theta),
            std::slice::from_ref(&ptdf),
        );

        assert_eq!(realized.dloss_dp.len(), 1);
        assert_eq!(realized.total_losses_mw.len(), 1);

        // Distributed-load gauge: Σ load_share[i] · LF[i] = 0.
        let bus_load = net.bus_load_p_mw_with_map(&bus_map);
        let total_load: f64 = bus_load.iter().sum();
        let weighted: f64 = bus_load
            .iter()
            .zip(realized.dloss_dp[0].iter())
            .map(|(&l, &lf)| (l / total_load) * lf)
            .sum();
        assert!(
            weighted.abs() < 1e-10,
            "distributed-load-weighted LFs must sum to ~0; got {weighted}",
        );

        // Realized losses must be positive on a network with non-zero r.
        assert!(
            realized.total_losses_mw[0] > 0.0,
            "expected positive realized loss; got {}",
            realized.total_losses_mw[0]
        );
    }

    #[test]
    fn summarize_p95_abs_picks_high_magnitude() {
        let s = ws_from(
            vec![vec![
                0.01, -0.02, 0.03, -0.04, 0.05, -0.06, 0.07, -0.08, 0.09, -0.10,
            ]],
            vec![12.5],
        );
        let stats = summarize(&s);
        assert_eq!(stats.per_period.len(), 1);
        let p = &stats.per_period[0];
        assert!((p.lf_min + 0.10).abs() < 1e-12);
        assert!((p.lf_max - 0.09).abs() < 1e-12);
        assert!((p.total_losses_mw - 12.5).abs() < 1e-12);
        // p95 of |x| among 10 sorted abs: 0.95*10 = 9.5 → idx 9 → 0.10
        assert!((p.lf_p95_abs - 0.10).abs() < 1e-12);
    }
}
