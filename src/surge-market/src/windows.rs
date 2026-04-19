// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical time-window helpers for power-market constraints.
//!
//! Power-market data sources commonly express constraints that apply
//! over a time window (startup frequency caps, energy requirements,
//! outage windows, etc.) as `(start_hour, end_hour, magnitude)`
//! triples. Mapping those hour-labels onto the solve's period index
//! range is a mechanical but error-prone translation that depends on
//! whether the end hour is inclusive or exclusive and whether the
//! membership rule uses interval starts or interval midpoints.
//!
//! This module provides the canonical translators that convert
//! `(start_hour, end_hour)` windows into `(start_period_idx,
//! end_period_idx)` tuples, under two membership rules:
//!
//! * [`period_range_by_interval_start`] — a period is in the window
//!   if its start time lies inside the window (typical for startup
//!   windows).
//! * [`period_range_by_interval_midpoint`] — a period is in the
//!   window if its midpoint lies inside the window (typical for
//!   energy windows).
//!
//! Both translators also handle the full-day/full-week edge case where
//! a `[0, 24]` or `[0, 168]` span is already-exclusive rather than
//! inclusive hour labels; see [`normalize_end_hour_exclusive`].

/// Return the list of cumulative `(period_start_hour,
/// period_end_hour)` tuples, given a per-period interval-duration
/// series in hours.
pub fn interval_start_end_hours(interval_hours: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let mut starts = Vec::with_capacity(interval_hours.len());
    let mut ends = Vec::with_capacity(interval_hours.len());
    let mut elapsed = 0.0;
    for &duration in interval_hours {
        starts.push(elapsed);
        elapsed += duration;
        ends.push(elapsed);
    }
    (starts, ends)
}

/// Normalize a window end-hour so that subsequent membership tests
/// use an exclusive upper boundary.
///
/// Source data commonly encodes full-day and full-week windows
/// either as inclusive hour labels (`[0, 23]`, `[0, 167]`) or as
/// already-exclusive boundaries (`[0, 24]`, `[0, 168]`). Treat exact
/// 24-hour multiples as exclusive and everything else as inclusive
/// hour labels (add one to produce the exclusive upper boundary).
pub fn normalize_end_hour_exclusive(start_hour: f64, end_hour: f64) -> f64 {
    let span = end_hour - start_hour;
    let rounded = span.round();
    if rounded > 0.0 && (span - rounded).abs() <= 1e-9 && (rounded as i64) % 24 == 0 {
        end_hour
    } else {
        end_hour + 1.0
    }
}

/// Return `(start_period_idx, end_period_idx)` for a window using
/// **interval-start** membership: a period is "in the window" if its
/// start time is within `[start_hour, end_hour_exclusive)`.
///
/// Use this rule for startup windows.
pub fn period_range_by_interval_start(
    interval_hours: &[f64],
    start_hour: f64,
    end_hour: f64,
) -> Option<(usize, usize)> {
    if interval_hours.is_empty() || end_hour + 1e-9 < start_hour {
        return None;
    }
    let (starts, _) = interval_start_end_hours(interval_hours);
    let end_exclusive = normalize_end_hour_exclusive(start_hour, end_hour);
    let covered: Vec<usize> = starts
        .iter()
        .enumerate()
        .filter_map(|(idx, &s)| {
            if s >= start_hour - 1e-9 && s < end_exclusive - 1e-9 {
                Some(idx)
            } else {
                None
            }
        })
        .collect();
    if covered.is_empty() {
        None
    } else {
        Some((*covered.first().unwrap(), *covered.last().unwrap()))
    }
}

/// Return `(start_period_idx, end_period_idx)` for a window using
/// **interval-midpoint** membership: a period is "in the window" if
/// its midpoint is within `(start_hour, end_hour_exclusive]`.
///
/// Use this rule for energy windows.
pub fn period_range_by_interval_midpoint(
    interval_hours: &[f64],
    start_hour: f64,
    end_hour: f64,
) -> Option<(usize, usize)> {
    if interval_hours.is_empty() || end_hour + 1e-9 < start_hour {
        return None;
    }
    let (starts, ends) = interval_start_end_hours(interval_hours);
    let end_exclusive = normalize_end_hour_exclusive(start_hour, end_hour);
    let covered: Vec<usize> = starts
        .iter()
        .zip(ends.iter())
        .enumerate()
        .filter_map(|(idx, (s, e))| {
            let midpoint = 0.5 * (s + e);
            if midpoint > start_hour + 1e-9 && midpoint <= end_exclusive + 1e-9 {
                Some(idx)
            } else {
                None
            }
        })
        .collect();
    if covered.is_empty() {
        None
    } else {
        Some((*covered.first().unwrap(), *covered.last().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_start_end_with_uniform_intervals() {
        let (starts, ends) = interval_start_end_hours(&[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(starts, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(ends, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn normalize_end_hour_full_day() {
        // Exact 24-hour span — treat as exclusive (don't add 1).
        assert!((normalize_end_hour_exclusive(0.0, 24.0) - 24.0).abs() < 1e-9);
        // Inclusive-label span — add 1 for exclusive.
        assert!((normalize_end_hour_exclusive(0.0, 23.0) - 24.0).abs() < 1e-9);
    }

    #[test]
    fn period_range_start_rule() {
        // 24 uniform hours, window [5, 8] inclusive labels → exclusive 9 →
        // periods with start in [5, 9): indices 5, 6, 7, 8
        let hrs = vec![1.0; 24];
        let r = period_range_by_interval_start(&hrs, 5.0, 8.0);
        assert_eq!(r, Some((5, 8)));
    }

    #[test]
    fn period_range_midpoint_rule() {
        // 24 uniform hours. Window (5, 8] exclusive → midpoints at 5.5, 6.5, 7.5
        // → indices 5, 6, 7.
        let hrs = vec![1.0; 24];
        let r = period_range_by_interval_midpoint(&hrs, 5.0, 7.0);
        // Inclusive-label span 5 to 7 → exclusive 8. Midpoints at 5.5, 6.5, 7.5.
        // Match if 5.0 < midpoint <= 8.0 → periods 5, 6, 7.
        assert_eq!(r, Some((5, 7)));
    }
}
