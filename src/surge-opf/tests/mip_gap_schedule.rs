// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Tests for the solver-agnostic MIP progress-monitor types.
//!
//! These live as an integration test (separate crate) so they remain
//! runnable even when other `surge-opf` internal test modules fail to
//! compile for unrelated reasons.

use surge_opf::backends::{MipEventKind, MipGapSchedule, MipProgressMonitor, MipTerminationReason};

#[test]
fn schedule_rejects_empty() {
    assert!(MipGapSchedule::new(vec![]).is_err());
}

#[test]
fn schedule_rejects_non_finite_or_negative() {
    assert!(MipGapSchedule::new(vec![(f64::NAN, 0.1)]).is_err());
    assert!(MipGapSchedule::new(vec![(-1.0, 0.1)]).is_err());
    assert!(MipGapSchedule::new(vec![(0.0, -0.1)]).is_err());
}

#[test]
fn schedule_sorts_breakpoints() {
    let sched = MipGapSchedule::new(vec![(30.0, 1e-2), (0.0, 1e-5), (10.0, 1e-4)]).unwrap();
    assert_eq!(
        sched.breakpoints,
        vec![(0.0, 1e-5), (10.0, 1e-4), (30.0, 1e-2)]
    );
}

#[test]
fn schedule_target_step_function() {
    let sched = MipGapSchedule::new(vec![(0.0, 1e-5), (10.0, 1e-4), (20.0, 1e-3)]).unwrap();
    assert_eq!(sched.target_at(-1.0), None);
    assert_eq!(sched.target_at(0.0), Some(1e-5));
    assert_eq!(sched.target_at(9.99), Some(1e-5));
    assert_eq!(sched.target_at(10.0), Some(1e-4));
    assert_eq!(sched.target_at(19.99), Some(1e-4));
    assert_eq!(sched.target_at(20.0), Some(1e-3));
    assert_eq!(sched.target_at(500.0), Some(1e-3));
    assert!((sched.max_gap() - 1e-3).abs() < 1e-12);
    // The solver's static MIPGap safety net uses min_gap() so auto-
    // termination can only fire at a gap the callback would always accept.
    assert!((sched.min_gap() - 1e-5).abs() < 1e-12);
    assert!((sched.final_deadline_secs() - 20.0).abs() < 1e-12);
}

#[test]
fn monitor_terminates_when_gap_meets_target() {
    let sched = MipGapSchedule::new(vec![(0.0, 0.5), (10.0, 0.1)]).unwrap();
    let mut mon = MipProgressMonitor::new(sched);

    // t=0, gap ≈ 0.9 > 0.5 → continue.
    assert!(!mon.tick(0.0, 100.0, 10.0));
    assert!(!mon.has_terminated());

    // t=1, still too wide.
    assert!(!mon.tick(1.0, 100.0, 10.0));

    // t=2, gap = 20/80 = 0.25 ≤ 0.5 (current target) → terminate.
    assert!(mon.tick(2.0, 80.0, 60.0));
    assert!(mon.has_terminated());
}

#[test]
fn monitor_does_not_terminate_before_first_breakpoint() {
    let sched = MipGapSchedule::new(vec![(5.0, 0.5)]).unwrap();
    let mut mon = MipProgressMonitor::new(sched);
    assert!(!mon.tick(0.0, 100.0, 99.9));
    assert!(!mon.tick(4.9, 100.0, 99.99));
    assert!(mon.tick(5.0, 100.0, 99.9));
}

#[test]
fn monitor_records_incumbent_and_bound_events() {
    let sched = MipGapSchedule::new(vec![(0.0, 1e-6)]).unwrap();
    let mut mon = MipProgressMonitor::new(sched);

    assert!(!mon.tick(0.0, 1000.0, 500.0));
    assert!(!mon.tick(1.0, 900.0, 500.0));
    assert!(!mon.tick(2.0, 900.0, 800.0));

    let trace = mon.into_trace(Some(60.0), MipTerminationReason::TimeLimit, 2.0, Some(0.11));
    assert!(trace.events.len() >= 3);
    assert!(
        trace
            .events
            .iter()
            .any(|e| e.kind == MipEventKind::NewIncumbent)
    );
    assert!(
        trace
            .events
            .iter()
            .any(|e| e.kind == MipEventKind::BoundImproved)
    );
    assert_eq!(trace.terminated_by, MipTerminationReason::TimeLimit);
}

#[test]
fn monitor_ignores_non_finite_ticks() {
    let sched = MipGapSchedule::new(vec![(0.0, 1e-3)]).unwrap();
    let mut mon = MipProgressMonitor::new(sched);
    assert!(!mon.tick(f64::NAN, 100.0, 99.0));
    assert!(!mon.tick(0.0, f64::INFINITY, 99.0));
    assert!(!mon.tick(0.0, 100.0, f64::NAN));
    let trace = mon.into_trace(None, MipTerminationReason::Other, 0.0, None);
    assert!(trace.events.is_empty());
}

#[test]
fn relative_gap_finite_at_zero_incumbent() {
    let g = MipProgressMonitor::relative_gap(0.0, 0.0);
    assert!(g.is_finite());
}
