// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Incremental dispatch block decomposition (DISP-PWR-BLK).
//!
//! Merges cost curve and ramp curve breakpoints into unified dispatch blocks.
//! Each block has a fixed marginal cost and per-block ramp rate, enabling
//! exact piecewise-linear cost and operating-point-dependent ramp constraints
//! in a pure LP formulation (no epiograph or SOS2 variables needed).

use std::collections::BTreeSet;

use surge_network::market::CostCurve;
use surge_network::network::Generator;

/// A unified dispatch block spanning a contiguous MW range.
///
/// Blocks partition [Pmin, Pmax] into segments, each with its own marginal
/// cost (from the cost curve) and ramp rates (from the ramp curve).
/// Monotonic cost ordering guarantees blocks fill bottom-up in the LP.
#[derive(Clone, Debug)]
pub struct DispatchBlock {
    /// Lower MW bound (inclusive).
    pub mw_lo: f64,
    /// Upper MW bound.
    pub mw_hi: f64,
    /// Marginal cost ($/MWh) for output within this block.
    pub marginal_cost: f64,
    /// Ramp-up rate (MW/min) for this operating range.
    pub ramp_up_mw_per_min: f64,
    /// Ramp-down rate (MW/min) for this operating range.
    pub ramp_dn_mw_per_min: f64,
    /// Regulation-mode ramp-up rate (MW/min).
    pub reg_ramp_up_mw_per_min: f64,
    /// Regulation-mode ramp-down rate (MW/min).
    pub reg_ramp_dn_mw_per_min: f64,
}

impl DispatchBlock {
    /// Block width in MW.
    #[inline]
    pub fn width_mw(&self) -> f64 {
        self.mw_hi - self.mw_lo
    }
}

/// Minimum block width in MW. Blocks narrower than this are merged into
/// their left neighbor to avoid degenerate LP variables.
/// Minimum dispatch block width (MW). Blocks narrower than this threshold are
/// merged into their left neighbor to avoid near-degenerate LP variables that
/// can cause numerical issues in the simplex solver.
const MIN_BLOCK_MW: f64 = 0.1;

/// Build unified dispatch blocks by merging cost curve and ramp curve breakpoints.
///
/// The breakpoint union includes:
/// - Cost curve breakpoints (PWL) or {Pmin, Pmax} (polynomial/none)
/// - Ramp-up curve breakpoints (after monotonicity enforcement)
/// - Ramp-down curve breakpoints
/// - Regulation capacity limits (p_reg_min, p_reg_max) for mode switching
///
/// Each block gets:
/// - Marginal cost evaluated at the block's midpoint on the cost curve
/// - Ramp rates evaluated at the block's midpoint on the ramp curves
///
/// Blocks narrower than 0.1 MW are merged into their left neighbor.
pub fn build_dispatch_blocks(generator: &Generator) -> Vec<DispatchBlock> {
    let pmin = generator.pmin;
    let pmax = generator.pmax;

    if pmax - pmin < 1e-6 {
        // Degenerate: pmin ≈ pmax — single zero-width block
        return vec![DispatchBlock {
            mw_lo: pmin,
            mw_hi: pmax,
            marginal_cost: marginal_cost_at(&generator.cost, pmin),
            ramp_up_mw_per_min: generator.ramp_up_at_mw(pmin).unwrap_or(f64::MAX),
            ramp_dn_mw_per_min: generator.ramp_down_at_mw(pmin).unwrap_or(f64::MAX),
            reg_ramp_up_mw_per_min: generator.reg_ramp_up_at_mw(pmin).unwrap_or(f64::MAX),
            reg_ramp_dn_mw_per_min: generator.reg_ramp_down_at_mw(pmin).unwrap_or(f64::MAX),
        }];
    }

    // 1. Collect all breakpoint MW values into a sorted set
    let mut bps = BTreeSet::<OrderedF64>::new();
    bps.insert(OrderedF64(pmin));
    bps.insert(OrderedF64(pmax));

    // Cost curve breakpoints
    if let Some(CostCurve::PiecewiseLinear { points, .. }) = &generator.cost {
        for &(mw, _) in points {
            if mw > pmin + 1e-9 && mw < pmax - 1e-9 {
                bps.insert(OrderedF64(mw));
            }
        }
    }

    // Ramp curve breakpoints (from all curve types)
    let add_curve_bps = |bps: &mut BTreeSet<OrderedF64>, curve: &[(f64, f64)]| {
        for &(mw, _) in curve {
            if mw > pmin + 1e-9 && mw < pmax - 1e-9 {
                bps.insert(OrderedF64(mw));
            }
        }
    };

    if let Some(ref r) = generator.ramping {
        add_curve_bps(&mut bps, &r.ramp_up_curve);
        add_curve_bps(&mut bps, &r.ramp_down_curve);
        add_curve_bps(&mut bps, &r.reg_ramp_up_curve);
        add_curve_bps(&mut bps, &r.reg_ramp_down_curve);
    }

    // Operating mode boundaries
    if let Some(rp) = generator.commitment.as_ref().and_then(|c| c.p_reg_min)
        && rp > pmin + 1e-9
        && rp < pmax - 1e-9
    {
        bps.insert(OrderedF64(rp));
    }
    if let Some(rp) = generator.commitment.as_ref().and_then(|c| c.p_reg_max)
        && rp > pmin + 1e-9
        && rp < pmax - 1e-9
    {
        bps.insert(OrderedF64(rp));
    }

    // 2. Build blocks between consecutive breakpoints
    let sorted: Vec<f64> = bps.into_iter().map(|x| x.0).collect();
    let mut blocks: Vec<DispatchBlock> = sorted
        .windows(2)
        .map(|w| {
            let (lo, hi) = (w[0], w[1]);
            let mid = (lo + hi) / 2.0;
            DispatchBlock {
                mw_lo: lo,
                mw_hi: hi,
                marginal_cost: marginal_cost_at(&generator.cost, mid),
                ramp_up_mw_per_min: generator.ramp_up_at_mw(mid).unwrap_or(f64::MAX),
                ramp_dn_mw_per_min: generator.ramp_down_at_mw(mid).unwrap_or(f64::MAX),
                reg_ramp_up_mw_per_min: generator.reg_ramp_up_at_mw(mid).unwrap_or(f64::MAX),
                reg_ramp_dn_mw_per_min: generator.reg_ramp_down_at_mw(mid).unwrap_or(f64::MAX),
            }
        })
        .collect();

    // 3. Merge narrow blocks (< MIN_BLOCK_MW) into left neighbor
    merge_narrow_blocks(&mut blocks);

    blocks
}

/// Decompose a total MW dispatch into per-block fills.
///
/// Fills blocks bottom-up (guaranteed correct by monotonic cost ordering).
/// `p_mw` is the total dispatch in MW, `pmin` is the generator's Pmin.
/// Returns fills in MW, same length as `blocks`.
pub fn decompose_into_blocks(p_mw: f64, pmin: f64, blocks: &[DispatchBlock]) -> Vec<f64> {
    let mut remaining = p_mw - pmin;
    blocks
        .iter()
        .map(|b| {
            let width = b.width_mw();
            let fill = remaining.clamp(0.0, width);
            remaining -= fill;
            fill
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Evaluate marginal cost from a cost curve at a given MW point.
fn marginal_cost_at(cost: &Option<CostCurve>, p_mw: f64) -> f64 {
    match cost {
        Some(curve) => curve.marginal_cost(p_mw),
        None => 0.0,
    }
}

/// Merge blocks narrower than MIN_BLOCK_MW into their left neighbor.
/// The merged block inherits the wider range and recalculates nothing —
/// the left neighbor's rates/cost are used (since the narrow block is negligible).
fn merge_narrow_blocks(blocks: &mut Vec<DispatchBlock>) {
    let mut i = 1; // never merge block 0
    while i < blocks.len() {
        if blocks[i].width_mw() < MIN_BLOCK_MW {
            // Extend left neighbor's upper bound
            blocks[i - 1].mw_hi = blocks[i].mw_hi;
            blocks.remove(i);
        } else {
            i += 1;
        }
    }
    // Edge case: if block 0 is narrow and there's a block 1, merge into block 1
    if blocks.len() > 1 && blocks[0].width_mw() < MIN_BLOCK_MW {
        blocks[1].mw_lo = blocks[0].mw_lo;
        blocks.remove(0);
    }
}

/// Newtype for f64 that implements Ord for use in BTreeSet.
/// NaN-safe: treats NaN as equal and less than all other values.
#[derive(Clone, Copy, PartialEq)]
struct OrderedF64(f64);

impl Eq for OrderedF64 {}

impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gen(
        pmin: f64,
        pmax: f64,
        cost: Option<CostCurve>,
        ramp_up: Vec<(f64, f64)>,
        ramp_dn: Vec<(f64, f64)>,
    ) -> Generator {
        use surge_network::network::RampingParams;
        let ramping = if ramp_up.is_empty() && ramp_dn.is_empty() {
            None
        } else {
            Some(RampingParams {
                ramp_up_curve: ramp_up,
                ramp_down_curve: ramp_dn,
                ..Default::default()
            })
        };
        Generator {
            pmin,
            pmax,
            cost,
            ramping,
            ..Default::default()
        }
    }

    // ── build_dispatch_blocks ──

    #[test]
    fn test_no_curves_single_block() {
        let g = make_gen(100.0, 500.0, None, vec![], vec![]);
        let blocks = build_dispatch_blocks(&g);
        assert_eq!(blocks.len(), 1);
        assert!((blocks[0].mw_lo - 100.0).abs() < 1e-10);
        assert!((blocks[0].mw_hi - 500.0).abs() < 1e-10);
        assert_eq!(blocks[0].marginal_cost, 0.0);
        assert_eq!(blocks[0].ramp_up_mw_per_min, f64::MAX);
    }

    #[test]
    fn test_pwl_cost_creates_blocks() {
        // PWL cost with breakpoints at 100, 300, 500
        // Slopes: [100,300] = (2000-0)/200 = 10 $/MWh, [300,500] = (6000-2000)/200 = 20 $/MWh
        let cost = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(100.0, 0.0), (300.0, 2000.0), (500.0, 6000.0)],
        };
        let g = make_gen(100.0, 500.0, Some(cost), vec![], vec![]);
        let blocks = build_dispatch_blocks(&g);

        assert_eq!(blocks.len(), 2);
        assert!((blocks[0].mw_lo - 100.0).abs() < 1e-10);
        assert!((blocks[0].mw_hi - 300.0).abs() < 1e-10);
        assert!((blocks[0].marginal_cost - 10.0).abs() < 1e-10);

        assert!((blocks[1].mw_lo - 300.0).abs() < 1e-10);
        assert!((blocks[1].mw_hi - 500.0).abs() < 1e-10);
        assert!((blocks[1].marginal_cost - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_ramp_curve_adds_breakpoints() {
        // No cost curve, but ramp curve has breakpoint at 350
        let g = make_gen(
            200.0,
            500.0,
            None,
            vec![(200.0, 8.0), (350.0, 12.0)],
            vec![],
        );
        let blocks = build_dispatch_blocks(&g);

        assert_eq!(blocks.len(), 2);
        assert!((blocks[0].mw_lo - 200.0).abs() < 1e-10);
        assert!((blocks[0].mw_hi - 350.0).abs() < 1e-10);
        assert!((blocks[0].ramp_up_mw_per_min - 8.0).abs() < 1e-10);

        assert!((blocks[1].mw_lo - 350.0).abs() < 1e-10);
        assert!((blocks[1].mw_hi - 500.0).abs() < 1e-10);
        assert!((blocks[1].ramp_up_mw_per_min - 12.0).abs() < 1e-10);
    }

    #[test]
    fn test_merged_cost_and_ramp_breakpoints() {
        // Cost breakpoints: 200, 350, 500
        // Ramp breakpoints: 200, 400
        // Union: {200, 350, 400, 500} → 3 blocks
        let cost = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(200.0, 0.0), (350.0, 1500.0), (500.0, 4500.0)],
        };
        let g = make_gen(
            200.0,
            500.0,
            Some(cost),
            vec![(200.0, 8.0), (400.0, 12.0)],
            vec![],
        );
        let blocks = build_dispatch_blocks(&g);

        assert_eq!(blocks.len(), 3);

        // Block 0: [200, 350] — cost slope = 10 $/MWh, ramp = 8.0
        assert!((blocks[0].mw_lo - 200.0).abs() < 1e-10);
        assert!((blocks[0].mw_hi - 350.0).abs() < 1e-10);
        assert!((blocks[0].marginal_cost - 10.0).abs() < 1e-10);
        assert!((blocks[0].ramp_up_mw_per_min - 8.0).abs() < 1e-10);

        // Block 1: [350, 400] — cost slope = 20 $/MWh, ramp = 8.0 (midpoint 375 < 400)
        assert!((blocks[1].mw_lo - 350.0).abs() < 1e-10);
        assert!((blocks[1].mw_hi - 400.0).abs() < 1e-10);
        assert!((blocks[1].marginal_cost - 20.0).abs() < 1e-10);
        assert!((blocks[1].ramp_up_mw_per_min - 8.0).abs() < 1e-10);

        // Block 2: [400, 500] — cost slope = 20 $/MWh, ramp = 12.0
        assert!((blocks[2].mw_lo - 400.0).abs() < 1e-10);
        assert!((blocks[2].mw_hi - 500.0).abs() < 1e-10);
        assert!((blocks[2].marginal_cost - 20.0).abs() < 1e-10);
        assert!((blocks[2].ramp_up_mw_per_min - 12.0).abs() < 1e-10);
    }

    #[test]
    fn test_polynomial_cost_blocks() {
        // Quadratic cost: 0.1*P^2 + 5*P + 100
        // Marginal cost = 0.2*P + 5
        // With ramp breakpoints at 200 and 400:
        // Block [200,400]: midpoint 300, marginal = 0.2*300 + 5 = 65
        // Block [400,500]: midpoint 450, marginal = 0.2*450 + 5 = 95
        let cost = CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.1, 5.0, 100.0],
        };
        let g = make_gen(
            200.0,
            500.0,
            Some(cost),
            vec![(200.0, 8.0), (400.0, 12.0)],
            vec![],
        );
        let blocks = build_dispatch_blocks(&g);

        assert_eq!(blocks.len(), 2);
        assert!((blocks[0].marginal_cost - 65.0).abs() < 1e-10);
        assert!((blocks[1].marginal_cost - 95.0).abs() < 1e-10);
    }

    #[test]
    fn test_reg_limits_add_breakpoints() {
        use surge_network::network::CommitmentParams;
        let mut g = make_gen(200.0, 500.0, None, vec![], vec![]);
        g.commitment = Some(CommitmentParams {
            p_reg_min: Some(250.0),
            p_reg_max: Some(450.0),
            ..Default::default()
        });

        let blocks = build_dispatch_blocks(&g);
        // Breakpoints: {200, 250, 450, 500} → 3 blocks
        assert_eq!(blocks.len(), 3);
        assert!((blocks[0].mw_lo - 200.0).abs() < 1e-10);
        assert!((blocks[0].mw_hi - 250.0).abs() < 1e-10);
        assert!((blocks[1].mw_lo - 250.0).abs() < 1e-10);
        assert!((blocks[1].mw_hi - 450.0).abs() < 1e-10);
        assert!((blocks[2].mw_lo - 450.0).abs() < 1e-10);
        assert!((blocks[2].mw_hi - 500.0).abs() < 1e-10);
    }

    #[test]
    fn test_narrow_blocks_merged() {
        // Create breakpoints that would produce a narrow block
        // Pmin=200, Pmax=500, ramp bp at 200.05 (only 0.05 MW from Pmin)
        let g = make_gen(
            200.0,
            500.0,
            None,
            vec![(200.0, 8.0), (200.05, 12.0)],
            vec![],
        );
        let blocks = build_dispatch_blocks(&g);
        // The 0.05 MW block should be merged — expect 1 block
        assert_eq!(blocks.len(), 1);
        assert!((blocks[0].mw_lo - 200.0).abs() < 1e-10);
        assert!((blocks[0].mw_hi - 500.0).abs() < 1e-10);
    }

    // ── decompose_into_blocks ──

    #[test]
    fn test_decompose_at_pmin() {
        let blocks = vec![
            DispatchBlock {
                mw_lo: 200.0,
                mw_hi: 350.0,
                marginal_cost: 10.0,
                ramp_up_mw_per_min: 8.0,
                ramp_dn_mw_per_min: 6.0,
                reg_ramp_up_mw_per_min: f64::MAX,
                reg_ramp_dn_mw_per_min: f64::MAX,
            },
            DispatchBlock {
                mw_lo: 350.0,
                mw_hi: 500.0,
                marginal_cost: 20.0,
                ramp_up_mw_per_min: 12.0,
                ramp_dn_mw_per_min: 10.0,
                reg_ramp_up_mw_per_min: f64::MAX,
                reg_ramp_dn_mw_per_min: f64::MAX,
            },
        ];

        // At Pmin = 200: all blocks empty
        let fills = decompose_into_blocks(200.0, 200.0, &blocks);
        assert!((fills[0]).abs() < 1e-10);
        assert!((fills[1]).abs() < 1e-10);
    }

    #[test]
    fn test_decompose_mid_range() {
        let blocks = vec![
            DispatchBlock {
                mw_lo: 200.0,
                mw_hi: 350.0,
                marginal_cost: 10.0,
                ramp_up_mw_per_min: 8.0,
                ramp_dn_mw_per_min: 6.0,
                reg_ramp_up_mw_per_min: f64::MAX,
                reg_ramp_dn_mw_per_min: f64::MAX,
            },
            DispatchBlock {
                mw_lo: 350.0,
                mw_hi: 500.0,
                marginal_cost: 20.0,
                ramp_up_mw_per_min: 12.0,
                ramp_dn_mw_per_min: 10.0,
                reg_ramp_up_mw_per_min: f64::MAX,
                reg_ramp_dn_mw_per_min: f64::MAX,
            },
        ];

        // At 400 MW: first block full (150), second block partial (50)
        let fills = decompose_into_blocks(400.0, 200.0, &blocks);
        assert!((fills[0] - 150.0).abs() < 1e-10);
        assert!((fills[1] - 50.0).abs() < 1e-10);
    }

    #[test]
    fn test_decompose_at_pmax() {
        let blocks = vec![
            DispatchBlock {
                mw_lo: 200.0,
                mw_hi: 350.0,
                marginal_cost: 10.0,
                ramp_up_mw_per_min: 8.0,
                ramp_dn_mw_per_min: 6.0,
                reg_ramp_up_mw_per_min: f64::MAX,
                reg_ramp_dn_mw_per_min: f64::MAX,
            },
            DispatchBlock {
                mw_lo: 350.0,
                mw_hi: 500.0,
                marginal_cost: 20.0,
                ramp_up_mw_per_min: 12.0,
                ramp_dn_mw_per_min: 10.0,
                reg_ramp_up_mw_per_min: f64::MAX,
                reg_ramp_dn_mw_per_min: f64::MAX,
            },
        ];

        // At Pmax = 500: both blocks full
        let fills = decompose_into_blocks(500.0, 200.0, &blocks);
        assert!((fills[0] - 150.0).abs() < 1e-10);
        assert!((fills[1] - 150.0).abs() < 1e-10);
    }

    #[test]
    fn test_total_block_width_equals_range() {
        // Verify that sum of block widths = Pmax - Pmin for various configs
        let cost = CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(100.0, 0.0), (250.0, 1500.0), (400.0, 4500.0)],
        };
        let mut g = make_gen(
            100.0,
            400.0,
            Some(cost),
            vec![(100.0, 5.0), (200.0, 10.0), (350.0, 8.0)],
            vec![(100.0, 4.0), (300.0, 9.0)],
        );
        g.commitment = Some(surge_network::network::CommitmentParams {
            p_reg_min: Some(150.0),
            p_reg_max: Some(380.0),
            ..Default::default()
        });

        let blocks = build_dispatch_blocks(&g);
        let total_width: f64 = blocks.iter().map(|b| b.width_mw()).sum();
        assert!(
            (total_width - 300.0).abs() < 1e-10,
            "Total width {total_width} != 300.0"
        );

        // Verify contiguity
        for i in 1..blocks.len() {
            assert!(
                (blocks[i].mw_lo - blocks[i - 1].mw_hi).abs() < 1e-10,
                "Gap between blocks {} and {}: {} vs {}",
                i - 1,
                i,
                blocks[i - 1].mw_hi,
                blocks[i].mw_lo
            );
        }
    }
}
