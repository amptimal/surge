// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Explicit contingency objective plan types shared by SCUC and SCED.

use surge_sparse::Triplet;

use super::layout::LpBlock;

/// Compact post-contingency flow constraint for the SCUC LP.
///
/// Each `ContingencyCut` expresses one linear constraint of the
/// form:
///
/// ```text
///   b_monitored · (θ[from_m] − θ[to_m])
///     + lodf · b_contingency · (θ[from_c] − θ[to_c])   ∈ [−limit, +limit]   (pu, at period `period`)
/// ```
///
/// which is the post-(monitored, contingency)-branch flow bounded by
/// the monitored branch's emergency rating. This is the minimum data
/// needed to emit the LP row directly — 24 bytes per cut — versus
/// the ~500-byte `surge_network::network::Flowgate` struct that
/// `solve_explicit_security_dispatch` allocates today just to carry
/// the same information. On 617-bus D1 explicit N-1 (8.6M cuts), the
/// compact representation saves roughly 4 GB of Rust-side heap.
///
/// Branch-branch contingencies populate `contingency_branch_idx` and
/// `lodf`. HVDC contingencies set `contingency_kind = Hvdc` and use
/// `hvdc_link_idx` / `hvdc_coefficient` instead. We keep both
/// variants in one struct (rather than an enum) because it keeps the
/// cut array a flat dense `Vec` and the emitter's hot loop
/// branch-light.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ContingencyCut {
    /// Period (hour) this cut is active in.
    pub period: u32,
    /// Index into the enclosing `ExplicitContingencyCase` list that
    /// this cut belongs to (one case = one contingency element at
    /// one period, many monitored branches per case).
    pub case_index: u32,
    /// Branch index (into the hourly network at `period`) of the
    /// monitored element whose post-contingency flow is being
    /// constrained.
    pub monitored_branch_idx: u32,
    /// Kind of contingency element that, when tripped, produces the
    /// post-contingency flow on `monitored_branch_idx`.
    pub contingency_kind: ContingencyCutKind,
    /// For `Branch` contingencies: branch index of the outaged
    /// element. For `Hvdc` contingencies: HVDC link index in
    /// `spec.hvdc_links`. Unused fields use `u32::MAX` as a sentinel.
    pub contingency_idx: u32,
    /// For `Branch` contingencies: LODF coefficient on the
    /// contingency branch's angle difference. For `HvdcLegacy`
    /// contingencies: PTDF-difference coefficient applied to every
    /// HVDC band variable. For `HvdcBanded`: unused — per-band
    /// coefficients live at `hvdc_band_range.0 .. hvdc_band_range.1`
    /// in the owning `ContingencyCutSet::hvdc_band_coefficients`.
    pub coefficient: f64,
    /// Emergency-rated MW limit of the monitored branch.
    pub limit_mw: f64,
    /// Range `[start, end)` into the owning `ContingencyCutSet`'s
    /// `hvdc_band_coefficients` array. Empty range (`start == end`)
    /// for non-banded cuts (Branch + HvdcLegacy).
    pub hvdc_band_range: (u32, u32),
}

/// Which kind of outaged equipment produces this contingency flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContingencyCutKind {
    /// AC branch outage. `contingency_idx` is a branch index.
    Branch,
    /// HVDC link outage (non-banded). `contingency_idx` is an HVDC
    /// link index; `coefficient` is a single PTDF-difference.
    HvdcLegacy,
    /// Banded HVDC link outage. `contingency_idx` is an HVDC link
    /// index; per-band coefficients live in the auxiliary array
    /// `ContingencyCutSet::hvdc_band_coefficients`.
    HvdcBanded,
}

/// Container for a complete explicit-N-1 cut set plus the auxiliary
/// storage for banded-HVDC coefficients. Kept flat + dense so the
/// borrow as `&[ContingencyCut]` into `DispatchProblemSpec` stays
/// cheap.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct ContingencyCutSet {
    pub cuts: Vec<ContingencyCut>,
    /// Flat `(band_idx, coefficient)` array referenced by
    /// `HvdcBanded` cuts via `ContingencyCut::hvdc_band_range`.
    /// Stored out-of-line so the common `Branch` cuts stay compact.
    pub hvdc_band_coefficients: Vec<(u32, f64)>,
}

fn push_triplet(triplets: &mut Vec<Triplet<f64>>, row: usize, col: usize, val: f64) {
    triplets.push(Triplet { row, col, val });
}

/// Per-contingency-case bookkeeping: which period, penalty column, and
/// flowgate slack columns belong to this case.
pub(crate) struct ExplicitContingencyCasePlan {
    pub case_index: usize,
    pub period: usize,
    pub penalty_col: usize,
    /// Each entry is `(flowgate_lower_slack_col, flowgate_upper_slack_col)`.
    pub flowgate_slack_cols: Vec<(usize, usize)>,
}

/// Per-period worst-case and average-case columns.
pub(crate) struct ExplicitContingencyPeriodPlan {
    pub case_indices: Vec<usize>,
    pub worst_case_col: usize,
    pub avg_case_col: usize,
}

/// Complete explicit contingency objective plan.
pub(crate) struct ExplicitContingencyObjectivePlan {
    pub case_penalty_base: usize,
    pub worst_case_base: usize,
    pub avg_case_base: usize,
    pub cases: Vec<ExplicitContingencyCasePlan>,
    pub periods: Vec<ExplicitContingencyPeriodPlan>,
    /// Maps each flowgate row index to its owning case index (or `None` for
    /// non-contingency flowgates).
    pub flowgate_row_cases: Vec<Option<usize>>,
}

/// Count the number of LP rows needed for the explicit contingency
/// objective block.
pub(crate) fn explicit_contingency_objective_rows(
    plan: Option<&ExplicitContingencyObjectivePlan>,
) -> usize {
    let Some(plan) = plan else {
        return 0;
    };
    plan.cases.len()
        + plan
            .periods
            .iter()
            .map(|period| {
                if period.case_indices.is_empty() {
                    0
                } else {
                    period.case_indices.len() + 1
                }
            })
            .sum::<usize>()
}

pub(crate) struct ExplicitContingencyObjectiveRowsInput<'a> {
    pub plan: &'a ExplicitContingencyObjectivePlan,
    pub thermal_penalty_curve: &'a surge_network::market::PenaltyCurve,
    pub period_hours: &'a dyn Fn(usize) -> f64,
    pub row_base: usize,
    pub base: f64,
}

pub(crate) fn build_explicit_contingency_objective_rows(
    input: ExplicitContingencyObjectiveRowsInput<'_>,
) -> LpBlock {
    let n_rows = explicit_contingency_objective_rows(Some(input.plan));
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    for case in &input.plan.cases {
        let row = input.row_base + local_row;
        let dt_h = (input.period_hours)(case.period);
        let overload_penalty = input.thermal_penalty_curve.marginal_cost_at(0.0) * dt_h;
        push_triplet(&mut block.triplets, row, case.penalty_col, -1.0);
        for &(lower_slack_col, upper_slack_col) in &case.flowgate_slack_cols {
            push_triplet(
                &mut block.triplets,
                row,
                lower_slack_col,
                overload_penalty * input.base,
            );
            push_triplet(
                &mut block.triplets,
                row,
                upper_slack_col,
                overload_penalty * input.base,
            );
        }
        block.row_lower[local_row] = f64::NEG_INFINITY;
        block.row_upper[local_row] = 0.0;
        local_row += 1;
    }

    for period in &input.plan.periods {
        if period.case_indices.is_empty() {
            continue;
        }

        for &case_index in &period.case_indices {
            let case = &input.plan.cases[case_index];
            let row = input.row_base + local_row;
            push_triplet(&mut block.triplets, row, case.penalty_col, 1.0);
            push_triplet(&mut block.triplets, row, period.worst_case_col, -1.0);
            block.row_lower[local_row] = f64::NEG_INFINITY;
            block.row_upper[local_row] = 0.0;
            local_row += 1;
        }

        let row = input.row_base + local_row;
        push_triplet(&mut block.triplets, row, period.avg_case_col, 1.0);
        let avg_weight = 1.0 / period.case_indices.len() as f64;
        for &case_index in &period.case_indices {
            let case = &input.plan.cases[case_index];
            push_triplet(&mut block.triplets, row, case.penalty_col, -avg_weight);
        }
        block.row_lower[local_row] = 0.0;
        block.row_upper[local_row] = 0.0;
        local_row += 1;
    }

    debug_assert_eq!(local_row, n_rows);
    block
}
