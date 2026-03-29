// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! NERC TPL-001-5.1 compliance report generator.
//!
//! Groups [`ContingencyResult`]s by their [`TplCategory`] and produces a
//! category x violation-type summary matrix suitable for regulatory
//! submission.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use surge_network::network::TplCategory;

use crate::{ContingencyResult, Violation};

/// Per-category violation counts and worst-case metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TplViolationSummary {
    /// Total contingencies in this category.
    pub total: usize,
    /// Contingencies that converged.
    pub converged: usize,
    /// Contingencies that did not converge.
    pub non_convergent: usize,
    /// Contingencies with at least one thermal overload.
    pub thermal_violations: usize,
    /// Contingencies with at least one low-voltage violation.
    pub voltage_low_violations: usize,
    /// Contingencies with at least one high-voltage violation.
    pub voltage_high_violations: usize,
    /// Contingencies with islanding.
    pub islanding_violations: usize,
    /// Contingencies with at least one flowgate overload.
    pub flowgate_violations: usize,
    /// Contingencies with at least one interface overload.
    pub interface_violations: usize,
    /// Worst-case thermal loading (% of limit), across all contingencies in this category.
    pub worst_thermal_loading_pct: f64,
    /// Worst-case voltage deviation (lowest Vm for low-voltage, highest Vm for high-voltage).
    pub worst_voltage_low: f64,
    /// Worst-case high voltage (p.u.).
    pub worst_voltage_high: f64,
}

/// NERC TPL-001 compliance report: event category x violation type matrix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TplReport {
    /// Per-category summaries, ordered by category discriminant.
    pub categories: BTreeMap<String, TplViolationSummary>,
    /// Total contingencies across all categories.
    pub total_contingencies: usize,
    /// Total contingencies with at least one violation.
    pub total_with_violations: usize,
}

/// Human-readable category label for serialization keys and CSV headers.
fn category_label(cat: TplCategory) -> &'static str {
    match cat {
        TplCategory::Unclassified => "Unclassified",
        TplCategory::P1SingleElement => "P1 - Single Element",
        TplCategory::P2SingleWithRAS => "P2 - Single Element + RAS",
        TplCategory::P3GeneratorTrip => "P3 - Generator Trip",
        TplCategory::P4StuckBreaker => "P4 - Stuck Breaker",
        TplCategory::P5DelayedClearing => "P5 - Delayed Clearing",
        TplCategory::P6SameTower => "P6a - Same Tower",
        TplCategory::P6CommonCorridor => "P6b - Common Corridor",
        TplCategory::P6ParallelCircuits => "P6c - Parallel Circuits",
        TplCategory::P7CommonMode => "P7 - Common Mode",
    }
}

/// Generate a TPL-001 compliance report from contingency analysis results.
///
/// Groups results by [`TplCategory`] and counts violation types per category.
pub fn generate_tpl_report(results: &[ContingencyResult]) -> TplReport {
    let mut categories: BTreeMap<String, TplViolationSummary> = BTreeMap::new();
    let mut total_with_violations = 0usize;

    for r in results {
        let label = category_label(r.tpl_category);
        let summary = categories.entry(label.to_string()).or_default();
        summary.total += 1;

        if r.converged {
            summary.converged += 1;
        }

        let has_violation = !r.violations.is_empty();
        if has_violation {
            total_with_violations += 1;
        }

        let mut has_thermal = false;
        let mut has_voltage_low = false;
        let mut has_voltage_high = false;
        let mut has_islanding = false;
        let mut has_flowgate = false;
        let mut has_interface = false;
        let mut has_non_convergent = false;

        for v in &r.violations {
            match v {
                Violation::ThermalOverload { loading_pct, .. } => {
                    has_thermal = true;
                    if *loading_pct > summary.worst_thermal_loading_pct {
                        summary.worst_thermal_loading_pct = *loading_pct;
                    }
                }
                Violation::VoltageLow { vm, .. } => {
                    has_voltage_low = true;
                    if summary.worst_voltage_low == 0.0 || *vm < summary.worst_voltage_low {
                        summary.worst_voltage_low = *vm;
                    }
                }
                Violation::VoltageHigh { vm, .. } => {
                    has_voltage_high = true;
                    if *vm > summary.worst_voltage_high {
                        summary.worst_voltage_high = *vm;
                    }
                }
                Violation::NonConvergent { .. } => {
                    has_non_convergent = true;
                }
                Violation::Islanding { .. } => {
                    has_islanding = true;
                }
                Violation::FlowgateOverload { .. } => {
                    has_flowgate = true;
                }
                Violation::InterfaceOverload { .. } => {
                    has_interface = true;
                }
            }
        }

        if has_thermal {
            summary.thermal_violations += 1;
        }
        if has_voltage_low {
            summary.voltage_low_violations += 1;
        }
        if has_voltage_high {
            summary.voltage_high_violations += 1;
        }
        if has_non_convergent {
            summary.non_convergent += 1;
        }
        if has_islanding {
            summary.islanding_violations += 1;
        }
        if has_flowgate {
            summary.flowgate_violations += 1;
        }
        if has_interface {
            summary.interface_violations += 1;
        }
    }

    TplReport {
        categories,
        total_contingencies: results.len(),
        total_with_violations,
    }
}

impl TplReport {
    /// Render the report as CSV text suitable for NERC TPL-001 submission.
    ///
    /// Columns: Category, Total, Converged, Non-Convergent, Thermal, Voltage Low,
    /// Voltage High, Islanding, Flowgate, Interface, Worst Thermal %, Worst V Low,
    /// Worst V High.
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "Category,Total,Converged,Non-Convergent,\
             Thermal,Voltage Low,Voltage High,Islanding,Flowgate,Interface,\
             Worst Thermal %,Worst V Low (pu),Worst V High (pu)\n",
        );
        for (cat, s) in &self.categories {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{:.2},{:.4},{:.4}\n",
                cat,
                s.total,
                s.converged,
                s.non_convergent,
                s.thermal_violations,
                s.voltage_low_violations,
                s.voltage_high_violations,
                s.islanding_violations,
                s.flowgate_violations,
                s.interface_violations,
                s.worst_thermal_loading_pct,
                s.worst_voltage_low,
                s.worst_voltage_high,
            ));
        }
        // Summary row
        let totals = self
            .categories
            .values()
            .fold(TplViolationSummary::default(), |mut acc, s| {
                acc.total += s.total;
                acc.converged += s.converged;
                acc.non_convergent += s.non_convergent;
                acc.thermal_violations += s.thermal_violations;
                acc.voltage_low_violations += s.voltage_low_violations;
                acc.voltage_high_violations += s.voltage_high_violations;
                acc.islanding_violations += s.islanding_violations;
                acc.flowgate_violations += s.flowgate_violations;
                acc.interface_violations += s.interface_violations;
                if s.worst_thermal_loading_pct > acc.worst_thermal_loading_pct {
                    acc.worst_thermal_loading_pct = s.worst_thermal_loading_pct;
                }
                if acc.worst_voltage_low == 0.0
                    || (s.worst_voltage_low > 0.0 && s.worst_voltage_low < acc.worst_voltage_low)
                {
                    acc.worst_voltage_low = s.worst_voltage_low;
                }
                if s.worst_voltage_high > acc.worst_voltage_high {
                    acc.worst_voltage_high = s.worst_voltage_high;
                }
                acc
            });
        out.push_str(&format!(
            "TOTAL,{},{},{},{},{},{},{},{},{},{:.2},{:.4},{:.4}\n",
            totals.total,
            totals.converged,
            totals.non_convergent,
            totals.thermal_violations,
            totals.voltage_low_violations,
            totals.voltage_high_violations,
            totals.islanding_violations,
            totals.flowgate_violations,
            totals.interface_violations,
            totals.worst_thermal_loading_pct,
            totals.worst_voltage_low,
            totals.worst_voltage_high,
        ));
        out
    }
}

impl fmt::Display for TplReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_csv())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(
        id: &str,
        tpl_category: TplCategory,
        violations: Vec<Violation>,
        converged: bool,
    ) -> ContingencyResult {
        ContingencyResult {
            id: id.to_string(),
            label: id.to_string(),
            status: if converged {
                crate::ContingencyStatus::Converged
            } else {
                crate::ContingencyStatus::NonConverged
            },
            converged,
            iterations: if converged { 4 } else { 0 },
            violations,
            n_islands: 1,
            tpl_category,
            ..Default::default()
        }
    }

    #[test]
    fn test_empty_report() {
        let report = generate_tpl_report(&[]);
        assert_eq!(report.total_contingencies, 0);
        assert_eq!(report.total_with_violations, 0);
        assert!(report.categories.is_empty());
    }

    #[test]
    fn test_single_category_grouping() {
        let results = vec![
            make_result("b1", TplCategory::P1SingleElement, vec![], true),
            make_result(
                "b2",
                TplCategory::P1SingleElement,
                vec![Violation::ThermalOverload {
                    branch_idx: 0,
                    from_bus: 1,
                    to_bus: 2,
                    loading_pct: 115.0,
                    flow_mw: 115.0,
                    flow_mva: 115.0,
                    limit_mva: 100.0,
                }],
                true,
            ),
        ];
        let report = generate_tpl_report(&results);
        assert_eq!(report.total_contingencies, 2);
        assert_eq!(report.total_with_violations, 1);
        let p1 = &report.categories["P1 - Single Element"];
        assert_eq!(p1.total, 2);
        assert_eq!(p1.converged, 2);
        assert_eq!(p1.thermal_violations, 1);
        assert!((p1.worst_thermal_loading_pct - 115.0).abs() < 1e-6);
    }

    #[test]
    fn test_multi_category_report() {
        let results = vec![
            make_result("p1_clean", TplCategory::P1SingleElement, vec![], true),
            make_result(
                "p4_thermal",
                TplCategory::P4StuckBreaker,
                vec![Violation::ThermalOverload {
                    branch_idx: 5,
                    from_bus: 10,
                    to_bus: 20,
                    loading_pct: 130.0,
                    flow_mw: 130.0,
                    flow_mva: 130.0,
                    limit_mva: 100.0,
                }],
                true,
            ),
            make_result(
                "p6_voltage",
                TplCategory::P6ParallelCircuits,
                vec![Violation::VoltageLow {
                    bus_number: 42,
                    vm: 0.88,
                    limit: 0.95,
                }],
                true,
            ),
            make_result(
                "p1_diverge",
                TplCategory::P1SingleElement,
                vec![Violation::NonConvergent {
                    max_mismatch: 1.5,
                    iterations: 20,
                }],
                false,
            ),
        ];
        let report = generate_tpl_report(&results);
        assert_eq!(report.total_contingencies, 4);
        assert_eq!(report.total_with_violations, 3);
        assert_eq!(report.categories.len(), 3);

        let p1 = &report.categories["P1 - Single Element"];
        assert_eq!(p1.total, 2);
        assert_eq!(p1.converged, 1);
        assert_eq!(p1.non_convergent, 1);

        let p4 = &report.categories["P4 - Stuck Breaker"];
        assert_eq!(p4.thermal_violations, 1);
        assert!((p4.worst_thermal_loading_pct - 130.0).abs() < 1e-6);

        let p6 = &report.categories["P6c - Parallel Circuits"];
        assert_eq!(p6.voltage_low_violations, 1);
        assert!((p6.worst_voltage_low - 0.88).abs() < 1e-6);
    }

    #[test]
    fn test_csv_output() {
        let results = vec![make_result(
            "b1",
            TplCategory::P1SingleElement,
            vec![Violation::ThermalOverload {
                branch_idx: 0,
                from_bus: 1,
                to_bus: 2,
                loading_pct: 110.5,
                flow_mw: 110.5,
                flow_mva: 110.5,
                limit_mva: 100.0,
            }],
            true,
        )];
        let report = generate_tpl_report(&results);
        let csv = report.to_csv();
        assert!(csv.starts_with("Category,"));
        assert!(csv.contains("P1 - Single Element"));
        assert!(csv.contains("110.50"));
        assert!(csv.contains("TOTAL"));
        // Verify it has header + 1 data row + 1 total row
        let line_count = csv.lines().count();
        assert_eq!(line_count, 3);
    }
}
