// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC commitment-cut validation and row assembly.

use surge_sparse::Triplet;

use super::layout::ScucLayout;
use crate::common::layout::LpBlock;
use crate::common::spec::DispatchProblemSpec;
use crate::dispatch::IndexedCommitmentTerm;
use crate::error::ScedError;

const BIG_M: f64 = 1e30;
const PENALTY_SLACK_UPPER: f64 = 1e10;

pub(super) struct ScucCommitmentCut<'a> {
    pub period_idx: usize,
    pub terms: &'a [IndexedCommitmentTerm],
    pub lower_bound: f64,
    pub penalty_cost: Option<f64>,
    pub slack_offset: Option<usize>,
}

pub(super) fn normalize_commitment_cuts<'a>(
    spec: &'a DispatchProblemSpec<'a>,
    n_hours: usize,
    n_gen: usize,
) -> Result<Vec<ScucCommitmentCut<'a>>, ScedError> {
    let mut next_slack_offset = 0usize;

    spec.commitment_constraints
        .iter()
        .map(|constraint| {
            if constraint.period_idx >= n_hours {
                return Err(ScedError::InvalidInput(format!(
                    "commitment constraint '{}' period_idx {} out of range for n_hours={n_hours}",
                    constraint.name, constraint.period_idx
                )));
            }

            if let Some(term) = constraint.terms.iter().find(|term| term.gen_index >= n_gen) {
                return Err(ScedError::InvalidInput(format!(
                    "commitment constraint '{}' term gen_index {} out of range for n_gen={n_gen}",
                    constraint.name, term.gen_index
                )));
            }

            if let Some(penalty_cost) = constraint.penalty_cost
                && penalty_cost < 0.0
            {
                return Err(ScedError::InvalidInput(format!(
                    "commitment constraint '{}' penalty_cost must be nonnegative",
                    constraint.name
                )));
            }

            let slack_offset = constraint.penalty_cost.map(|_| {
                let offset = next_slack_offset;
                next_slack_offset += 1;
                offset
            });

            Ok(ScucCommitmentCut {
                period_idx: constraint.period_idx,
                terms: &constraint.terms,
                lower_bound: constraint.lower_bound,
                penalty_cost: constraint.penalty_cost,
                slack_offset,
            })
        })
        .collect()
}

pub(super) fn penalty_slack_count(cuts: &[ScucCommitmentCut<'_>]) -> usize {
    cuts.iter().filter(|cut| cut.slack_offset.is_some()).count()
}

pub(super) fn commitment_cut_rows(cuts: &[ScucCommitmentCut<'_>]) -> usize {
    cuts.len()
}

pub(super) struct ScucCommitmentCutRowsInput<'a> {
    pub cuts: &'a [ScucCommitmentCut<'a>],
    pub layout: &'a ScucLayout,
    pub penalty_slack_base: usize,
    pub row_base: usize,
}

pub(super) fn build_commitment_cut_rows(input: ScucCommitmentCutRowsInput<'_>) -> LpBlock {
    let n_rows = commitment_cut_rows(input.cuts);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };

    for (local_row, cut) in input.cuts.iter().enumerate() {
        let row = input.row_base + local_row;
        for term in cut.terms {
            push_triplet(
                &mut block.triplets,
                row,
                input.layout.commitment_col(cut.period_idx, term.gen_index),
                term.coeff,
            );
        }
        if let Some(slack_offset) = cut.slack_offset {
            push_triplet(
                &mut block.triplets,
                row,
                input.penalty_slack_base + slack_offset,
                1.0,
            );
        }
        block.row_lower[local_row] = cut.lower_bound;
        block.row_upper[local_row] = BIG_M;
    }

    block
}

pub(super) fn apply_penalty_slack_columns(
    cuts: &[ScucCommitmentCut<'_>],
    penalty_slack_base: usize,
    col_cost: &mut [f64],
    col_lower: &mut [f64],
    col_upper: &mut [f64],
) {
    for cut in cuts {
        let (Some(slack_offset), Some(penalty_cost)) = (cut.slack_offset, cut.penalty_cost) else {
            continue;
        };
        let slack_col = penalty_slack_base + slack_offset;
        col_lower[slack_col] = 0.0;
        col_upper[slack_col] = PENALTY_SLACK_UPPER;
        col_cost[slack_col] = penalty_cost;
    }
}

fn push_triplet(triplets: &mut Vec<Triplet<f64>>, row: usize, col: usize, val: f64) {
    triplets.push(Triplet { row, col, val });
}
