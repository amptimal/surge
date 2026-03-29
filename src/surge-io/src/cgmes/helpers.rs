// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

// ---------------------------------------------------------------------------
// Per-unit conversion and interpolation helpers
// ---------------------------------------------------------------------------

/// Convert an impedance from Ohms to per-unit on a 100 MVA base.
#[inline]
pub(crate) fn ohm_to_pu(ohm: f64, base_kv: f64, base_mva: f64) -> f64 {
    if base_kv <= 0.0 || base_mva <= 0.0 {
        return 0.0;
    }
    ohm * base_mva / (base_kv * base_kv)
}

/// Convert an admittance from Siemens to per-unit on a 100 MVA base.
#[inline]
pub(crate) fn siemens_to_pu(s: f64, base_kv: f64, base_mva: f64) -> f64 {
    if base_mva <= 0.0 {
        return 0.0;
    }
    s * base_kv * base_kv / base_mva
}

/// Interpolate Q-limits from a ReactiveCapabilityCurve at a given P operating point.
///
/// `points` must be sorted by x (P in IEC sign convention: negative = generating).
/// Returns `(qmin_mvar, qmax_mvar)` via linear interpolation.  Clamps to the
/// curve endpoints for P values outside the defined range.
///
/// CGMES CurveData convention: y1value = Qmin, y2value = Qmax.
pub(crate) fn interpolate_rcc(points: &[(f64, f64, f64)], p: f64) -> (f64, f64) {
    match points {
        [] => (-9999.0, 9999.0),
        [(_, y1, y2)] => (*y1, *y2),
        _ => {
            let first = &points[0];
            let last = points
                .last()
                .expect("points has 2+ entries in this match arm");
            if p <= first.0 {
                return (first.1, first.2);
            }
            if p >= last.0 {
                return (last.1, last.2);
            }
            // Binary-search for the segment containing p.
            let i = points.partition_point(|pt| pt.0 <= p);
            let (x0, y1_0, y2_0) = points[i - 1];
            let (x1, y1_1, y2_1) = points[i];
            let t = (p - x0) / (x1 - x0);
            (y1_0 + t * (y1_1 - y1_0), y2_0 + t * (y2_1 - y2_0))
        }
    }
}

/// Interpolate phase shift angle (degrees) from a PhaseTapChangerTablePoint vec.
///
/// Points must be sorted by step (ascending). Returns the angle for the given
/// step position via exact match (±0.5 tolerance) or linear interpolation.
pub(crate) fn ptc_table_angle(pts: &[(f64, f64)], step: f64) -> f64 {
    if pts.is_empty() {
        return 0.0;
    }
    // exact match
    if let Some(&(_, a)) = pts.iter().find(|&&(s, _)| (s - step).abs() < 0.5) {
        return a;
    }
    // linear interpolation
    let idx = pts.partition_point(|&(s, _)| s < step);
    if idx == 0 {
        return pts[0].1;
    }
    if idx >= pts.len() {
        return pts[pts.len() - 1].1;
    }
    let (s0, a0) = pts[idx - 1];
    let (s1, a1) = pts[idx];
    if (s1 - s0).abs() < 1e-9 {
        return a0;
    }
    a0 + (step - s0) * (a1 - a0) / (s1 - s0)
}

/// Interpolate per-unit tap ratio from a RatioTapChangerTablePoint vec.
///
/// Points must be sorted by step (ascending). Returns the ratio for the given
/// step position via exact match (±0.5 tolerance) or linear interpolation.
/// A ratio of 1.0 (neutral step) means no tap change relative to nominal.
pub(crate) fn rtc_table_ratio(pts: &[(f64, f64)], step: f64) -> f64 {
    if pts.is_empty() {
        return 1.0;
    }
    // exact match
    if let Some(&(_, r)) = pts.iter().find(|&&(s, _)| (s - step).abs() < 0.5) {
        return r;
    }
    // linear interpolation
    let idx = pts.partition_point(|&(s, _)| s < step);
    if idx == 0 {
        return pts[0].1;
    }
    if idx >= pts.len() {
        return pts[pts.len() - 1].1;
    }
    let (s0, r0) = pts[idx - 1];
    let (s1, r1) = pts[idx];
    if (s1 - s0).abs() < 1e-9 {
        return r0;
    }
    r0 + (step - s0) * (r1 - r0) / (s1 - s0)
}
