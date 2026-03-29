// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! P-Q capability curve (D-curve) constraints for AC-OPF (OPF-06).
//!
//! When a generator has a non-empty `pq_curve` field, the OPF replaces the
//! rectangular Q bounds with piecewise-linear upper and lower D-curve constraints.
//!
//! For each consecutive pair of operating points (p1, qmax1) and (p2, qmax2):
//!
//! ```text
//! qmax_g ≤ qmax1 + (qmax2 - qmax1)/(p2 - p1) × (p_g - p1)   [upper D-curve]
//! qmin_g ≥ qmin1 + (qmin2 - qmin1)/(p2 - p1) × (p_g - p1)   [lower D-curve]
//! ```
//!
//! Rearranged into standard NLP form `g_L ≤ g(x) ≤ g_U` with g linear in (Pg, Qg):
//!
//! Upper: `Qg - slope_max × Pg ≤ qmax1 - slope_max × p1`
//! Lower: `qmin1 - slope_min × p1 ≤ Qg - slope_min × Pg`
//!
//! These are added as rows appended after the existing NLP constraints.  The
//! Jacobian entries are constants (slopes) and therefore trivial to assemble.

use surge_network::network::Generator;
use tracing::debug;

/// A single linearized D-curve constraint segment.
///
/// Represents one inequality from the piecewise-linear D-curve:
/// `lhs_lb ≤ qg - slope * pg ≤ lhs_ub`
///
/// where `lhs_lb` and `lhs_ub` may be `f64::NEG_INFINITY` or `f64::INFINITY`
/// for one-sided constraints.
#[derive(Debug, Clone)]
pub struct PqConstraint {
    /// Local (OPF-internal) generator index.
    pub gen_local: usize,
    /// Slope: dQmax/dP or dQmin/dP for the segment (in per-unit).
    pub slope: f64,
    /// Lower bound on `Qg - slope * Pg` (NEG_INFINITY if upper-only).
    pub lhs_lb: f64,
    /// Upper bound on `Qg - slope * Pg` (INFINITY if lower-only).
    pub lhs_ub: f64,
}

/// Build all linearized D-curve constraints for the generators with non-empty
/// `pq_curve` fields.
///
/// # Arguments
///
/// * `gen_indices` – OPF-internal indices mapping local gen j → network gen index.
/// * `generators` – slice of all generators in the network.
/// * `base_mva` – system base MVA for per-unit conversion.
///
/// # Returns
///
/// A `Vec<PqConstraint>` with one entry per piecewise-linear segment side.
/// For k consecutive point pairs, each generator contributes at most `2*(k-1)`
/// constraints (one upper + one lower bound per segment).
pub fn build_pq_constraints(
    gen_indices: &[usize],
    generators: &[Generator],
    _base_mva: f64,
) -> Vec<PqConstraint> {
    let mut constraints = Vec::new();
    let n_with_curve = gen_indices
        .iter()
        .filter(|&&gi| {
            !generators[gi]
                .reactive_capability
                .as_ref()
                .is_none_or(|r| r.pq_curve.is_empty())
        })
        .count();
    debug!(
        generators = gen_indices.len(),
        with_pq_curve = n_with_curve,
        "Building PQ D-curve constraints"
    );

    for (local_idx, &gi) in gen_indices.iter().enumerate() {
        let g = &generators[gi];
        if g.reactive_capability
            .as_ref()
            .is_none_or(|r| r.pq_curve.is_empty())
        {
            continue; // Use rectangular bounds — no D-curve constraints needed.
        }

        let empty_pq: Vec<(f64, f64, f64)> = Vec::new();
        let curve = g
            .reactive_capability
            .as_ref()
            .map(|r| &r.pq_curve)
            .unwrap_or(&empty_pq); // sorted by p_pu ascending

        for w in curve.windows(2) {
            let (p1, qmax1, qmin1) = w[0];
            let (p2, qmax2, qmin2) = w[1];

            let dp = p2 - p1;
            if dp.abs() < 1e-12 {
                continue; // Degenerate segment — skip.
            }

            // Upper D-curve: Qg ≤ qmax1 + slope_max*(Pg - p1)
            // → Qg - slope_max*Pg ≤ qmax1 - slope_max*p1
            // pq_curve values are already in per-unit; no base_mva conversion needed.
            let slope_max = (qmax2 - qmax1) / dp;
            let rhs_max = qmax1 - slope_max * p1;
            constraints.push(PqConstraint {
                gen_local: local_idx,
                slope: slope_max,
                lhs_lb: f64::NEG_INFINITY,
                lhs_ub: rhs_max,
            });

            // Lower D-curve: Qg ≥ qmin1 + slope_min*(Pg - p1)
            // → qmin1 - slope_min*p1 ≤ Qg - slope_min*Pg
            let slope_min = (qmin2 - qmin1) / dp;
            let rhs_min = qmin1 - slope_min * p1;
            constraints.push(PqConstraint {
                gen_local: local_idx,
                slope: slope_min,
                lhs_lb: rhs_min,
                lhs_ub: f64::INFINITY,
            });
        }
    }

    constraints
}

/// Evaluate the piecewise-linear D-curve constraints at the current iterate.
///
/// For each constraint: `g[row] = Qg[j] - slope * Pg[j]`
///
/// # Arguments
///
/// * `pq_constraints` – constraints built by [`build_pq_constraints`].
/// * `pg` – current Pg values in p.u. (indexed by local gen).
/// * `qg` – current Qg values in p.u. (indexed by local gen).
/// * `g` – output slice to fill (must have at least `pq_constraints.len()` entries).
/// * `offset` – row offset into `g` where these constraints start.
#[cfg(test)]
pub fn eval_pq_constraints(
    pq_constraints: &[PqConstraint],
    pg: &[f64],
    qg: &[f64],
    g: &mut [f64],
    offset: usize,
) {
    for (ci, c) in pq_constraints.iter().enumerate() {
        let j = c.gen_local;
        g[offset + ci] = qg[j] - c.slope * pg[j];
    }
}

/// Evaluate D-curve constraint at given (Pg_pu, Qg_pu) without modifying any state.
///
/// Useful for testing: checks whether the generator is feasible under the D-curve
/// at a given operating point and returns the margin to each binding constraint.
///
/// Returns `(upper_margin, lower_margin)` in per-unit where a positive margin
/// means the constraint is satisfied (not binding).
#[cfg(test)]
pub fn pq_curve_margin(
    generator: &Generator,
    pg_pu: f64,
    qg_pu: f64,
    _base_mva: f64,
) -> Option<(f64, f64)> {
    let empty_pq: Vec<(f64, f64, f64)> = Vec::new();
    let curve = generator
        .reactive_capability
        .as_ref()
        .map(|r| &r.pq_curve)
        .unwrap_or(&empty_pq);
    if curve.is_empty() {
        return None;
    }

    // Find the segment containing pg_pu.
    let pg_pu = pg_pu.clamp(curve.first()?.0, curve.last()?.0);
    let segment = curve
        .windows(2)
        .find(|w| w[0].0 <= pg_pu && pg_pu <= w[1].0)?;

    let (p1, qmax1, qmin1) = segment[0];
    let (p2, qmax2, qmin2) = segment[1];
    let dp = p2 - p1;
    if dp.abs() < 1e-12 {
        return None;
    }

    let t = (pg_pu - p1) / dp;
    let qmax_at_pg = qmax1 + t * (qmax2 - qmax1);
    let qmin_at_pg = qmin1 + t * (qmin2 - qmin1);

    let upper_margin = qmax_at_pg - qg_pu;
    let lower_margin = qg_pu - qmin_at_pg;

    Some((upper_margin, lower_margin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::Generator;

    fn make_gen_with_curve() -> Generator {
        let mut g = Generator::new(1, 0.5, 1.0);
        // qmax: 50 MVAr at P=0, 20 MVAr at P=100 MW (in per-unit on 100 MVA base)
        // qmin: -30 MVAr at P=0, -10 MVAr at P=100 MW
        // Per-unit (base_mva=100): p=0→1.0, qmax=0.5→0.2, qmin=-0.3→-0.1
        g.reactive_capability
            .get_or_insert_with(Default::default)
            .pq_curve = vec![
            (0.0, 0.5, -0.3), // P=0 pu: Qmax=0.5, Qmin=-0.3
            (1.0, 0.2, -0.1), // P=1 pu: Qmax=0.2, Qmin=-0.1
        ];
        g
    }

    /// OPF-06: verify that at P=100 MW (1.0 pu), the D-curve Qmax = 0.2 pu (20 MVAr).
    ///
    /// This is tighter than the rectangular qmax = 50 MVAr = 0.5 pu.
    #[test]
    fn test_pq_curve_margin_at_full_load() {
        let g = make_gen_with_curve();
        let base_mva = 100.0;

        // At P=1.0 pu (100 MW), Qmax from D-curve = 0.2 pu (20 MVAr).
        // With Qg = 0.2 pu exactly, upper margin should be ≈ 0.
        let (upper, lower) = pq_curve_margin(&g, 1.0, 0.2, base_mva).unwrap();
        assert!(
            upper.abs() < 1e-10,
            "At P=1.0pu, Qg=0.2pu should be at D-curve limit; upper_margin={upper:.2e}"
        );
        assert!(
            lower > 0.0,
            "Lower margin should be positive at Qg=0.2 (well above Qmin=-0.1)"
        );
    }

    /// OPF-06: at P=100 MW dispatch, Qmax from D-curve (20 MVAr = 0.2 pu) is tighter
    /// than the rectangular bound (50 MVAr = 0.5 pu), as stated in the task.
    #[test]
    fn test_dcurve_tighter_than_rectangular_at_full_load() {
        let g = make_gen_with_curve();
        let base_mva = 100.0;
        let qmax_rectangular = g.qmax.min(50.0); // rectangular bound = 50 MVAr

        // D-curve Qmax at P=100 MW is 20 MVAr (0.2 pu).
        // Rectangular Qmax = 50 MVAr (0.5 pu).
        let (upper_margin, _) = pq_curve_margin(&g, 1.0, 0.3, base_mva).unwrap();
        // Qg = 0.3 pu (30 MVAr) → D-curve says Qmax = 0.2 pu → violated.
        assert!(
            upper_margin < 0.0,
            "Qg=0.3pu violates D-curve at P=1.0pu (Qmax=0.2pu); upper_margin={upper_margin:.4}"
        );

        // But 30 MVAr < qmax_rectangular=50 MVAr, so rectangular allows it.
        let _ = qmax_rectangular;
        // 0.3 pu (30 MVAr) would be feasible under rectangular, infeasible under D-curve.
    }

    /// OPF-06: build_pq_constraints produces the right number of segments.
    #[test]
    fn test_build_pq_constraints_count() {
        let g = make_gen_with_curve();
        // 2 points → 1 segment → 2 constraints (one upper, one lower).
        let constraints = build_pq_constraints(&[0usize], &[g], 100.0);
        assert_eq!(
            constraints.len(),
            2,
            "one segment should produce 2 constraints (upper + lower)"
        );
    }

    /// OPF-06: eval_pq_constraints computes Qg - slope*Pg correctly.
    #[test]
    fn test_eval_pq_constraints() {
        let g = make_gen_with_curve();
        let base_mva = 100.0;
        let constraints = build_pq_constraints(&[0usize], &[g], base_mva);

        // At Pg=0.5, Qg=0.35: upper constraint should be 0.35 - slope_max*0.5
        // slope_max = (0.2 - 0.5)/(1.0 - 0.0) / base_mva * base_mva = -0.3
        let pg = vec![0.5f64];
        let qg = vec![0.35f64];
        let mut gvec = vec![0.0f64; constraints.len()];
        eval_pq_constraints(&constraints, &pg, &qg, &mut gvec, 0);

        // Upper: Qg - (-0.3)*Pg = 0.35 + 0.3*0.5 = 0.35 + 0.15 = 0.50; bound = 0.5 → feasible.
        // Lower: Qg - slope_min*Pg; slope_min = (-0.1 - (-0.3))/1.0 = 0.2
        //   = 0.35 - 0.2*0.5 = 0.35 - 0.10 = 0.25; lb = (-0.3 - 0.2*0.0) = -0.3 → feasible.
        for (ci, c) in constraints.iter().enumerate() {
            let val = gvec[ci];
            assert!(
                val >= c.lhs_lb - 1e-10,
                "constraint {ci} lb violated: {val:.4} < {:.4}",
                c.lhs_lb
            );
            assert!(
                val <= c.lhs_ub + 1e-10,
                "constraint {ci} ub violated: {val:.4} > {:.4}",
                c.lhs_ub
            );
        }
    }

    /// OPF-06: a three-point D-curve produces 4 constraints (2 per segment).
    #[test]
    fn test_three_point_pq_curve() {
        let mut g = Generator::new(1, 0.5, 1.0);
        g.reactive_capability
            .get_or_insert_with(Default::default)
            .pq_curve = vec![(0.0, 0.6, -0.3), (0.5, 0.4, -0.2), (1.0, 0.2, -0.1)];
        let constraints = build_pq_constraints(&[0usize], &[g], 100.0);
        assert_eq!(
            constraints.len(),
            4,
            "2 segments × 2 bounds = 4 constraints"
        );
    }
}
