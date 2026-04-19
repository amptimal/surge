// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical piecewise offer-curve construction.
//!
//! Standard day-ahead/real-time markets quote energy offers as a piecewise
//! linear curve: an ordered list of `(block_size, marginal_cost)` pairs
//! ordered by ascending marginal cost. The block sizes are typically given
//! in per-unit of system base and must be converted to MW at curve-build
//! time.
//!
//! This module provides the canonical constructor that turns raw
//! block data into the cumulative-MW/segment-cost form that
//! [`OfferCurve`] expects. The converter is pure and has no opinion
//! about where the input blocks came from, so every market adapter
//! consumes it instead of reinventing the loop.

use surge_network::market::{OfferCurve, StartupTier};

/// Convert per-unit piecewise cost blocks to cumulative-MW segments.
///
/// The input is an ordered list of `(marginal_cost_per_mwh, block_size_pu)`
/// pairs, sorted by ascending marginal cost. The output is an ordered
/// list of `(cumulative_mw, marginal_cost_per_mwh)` segments that match
/// the representation [`OfferCurve::segments`] uses (each segment's
/// first element is the cumulative MW at the end of that block).
///
/// Blocks with fewer than two entries are silently skipped to stay
/// compatible with datasets that leave degenerate entries in their
/// tables.
pub fn piecewise_cost_to_segments(blocks: &[Vec<f64>], base_mva: f64) -> Vec<(f64, f64)> {
    let mut cumulative = 0.0;
    let mut segments = Vec::new();
    let base = base_mva.max(1.0);
    for block in blocks {
        if block.len() < 2 {
            continue;
        }
        let block_size_mw = block[1] * base_mva;
        if block_size_mw <= 1e-12 {
            // Some datasets emit zero-size blocks; skip them so the
            // piecewise curve only carries strictly increasing
            // cumulative-MW breakpoints.
            continue;
        }
        // Block costs are expressed in `$/pu-hr`; the canonical LP
        // expects `$/MWh`. Convert by dividing by system base.
        let marginal_value = block[0] / base;
        cumulative += block_size_mw;
        segments.push((cumulative, marginal_value));
    }
    segments
}

/// Assemble a complete [`OfferCurve`] from piecewise cost blocks, a
/// no-load cost, and a list of startup tiers.
///
/// The three components compose directly onto [`OfferCurve`] without
/// further manipulation; this is a thin convenience so adapters need
/// not reference the field layout.
pub fn build_offer_curve(
    blocks: &[Vec<f64>],
    no_load_cost: f64,
    startup_tiers: Vec<StartupTier>,
    base_mva: f64,
) -> OfferCurve {
    OfferCurve {
        segments: piecewise_cost_to_segments(blocks, base_mva),
        no_load_cost,
        startup_tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piecewise_cost_to_segments_accumulates_mw() {
        // Two blocks: (cost=1000 $/pu-hr, size=0.5 pu), (cost=2000, size=0.3)
        // with base_mva = 100 → $/MWh = cost/100 → 10, 20.
        let blocks = vec![vec![1000.0, 0.5], vec![2000.0, 0.3]];
        let segs = piecewise_cost_to_segments(&blocks, 100.0);
        assert_eq!(segs.len(), 2);
        // cumulative mw: 50, then 80
        assert!((segs[0].0 - 50.0).abs() < 1e-9);
        assert!((segs[0].1 - 10.0).abs() < 1e-9);
        assert!((segs[1].0 - 80.0).abs() < 1e-9);
        assert!((segs[1].1 - 20.0).abs() < 1e-9);
    }

    #[test]
    fn piecewise_cost_to_segments_skips_degenerate_blocks() {
        let blocks = vec![vec![1000.0, 1.0], vec![2000.0], vec![3000.0, 0.5]];
        let segs = piecewise_cost_to_segments(&blocks, 100.0);
        assert_eq!(segs.len(), 2);
        assert!((segs[0].0 - 100.0).abs() < 1e-9);
        assert!((segs[1].0 - 150.0).abs() < 1e-9);
    }

    #[test]
    fn build_offer_curve_preserves_no_load_and_startup() {
        let blocks = vec![vec![1200.0, 0.4]];
        let tiers = vec![StartupTier {
            max_offline_hours: 8.0,
            cost: 1500.0,
            sync_time_min: 0.0,
        }];
        let curve = build_offer_curve(&blocks, 300.0, tiers.clone(), 100.0);
        assert_eq!(curve.segments.len(), 1);
        assert!((curve.no_load_cost - 300.0).abs() < 1e-9);
        assert_eq!(curve.startup_tiers.len(), 1);
        assert_eq!(curve.startup_tiers[0].max_offline_hours, 8.0);
    }
}
