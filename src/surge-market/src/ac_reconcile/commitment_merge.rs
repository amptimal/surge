// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Commitment augmentation — merge extra must-run schedules onto the
//! stage's fixed commitment.
//!
//! The canonical workflow already handles "extract stage-1 commitment
//! → pin stage-2 to it" via [`crate::workflow::extract_commitment_schedule`]
//! and the executor's `CommitmentPolicy::Fixed` assignment. This
//! helper is the per-stage-2 augmentation on top of that:
//! voltage-support generators (DC-line reactive-support synthetics,
//! `producer_static` must-run generators, and similar) may need to
//! stay online for AC-OPF voltage regulation even if the DC SCUC
//! cleared them off.
//!
//! Semantics: `committed_merged[p] = committed_source[p] OR
//! committed_augmented[p]`. New entries are added wholesale; existing
//! entries get OR-merged period-wise.

use surge_dispatch::{
    CommitmentPolicy, CommitmentSchedule, DispatchRequest, ResourceCommitmentSchedule,
};

pub fn merge_commitment_augmentation(
    request: &mut DispatchRequest,
    augmentation: &[ResourceCommitmentSchedule],
) {
    if augmentation.is_empty() {
        return;
    }
    let CommitmentPolicy::Fixed(schedule) = request.commitment().clone() else {
        // Only meaningful for a Fixed-commitment stage. If the stage
        // is still Optimize/etc., silently skip — a badly configured
        // workflow, not something to panic on.
        return;
    };
    let mut resources = schedule.resources;
    let mut index_by_resource: std::collections::HashMap<String, usize> = resources
        .iter()
        .enumerate()
        .map(|(i, r)| (r.resource_id.clone(), i))
        .collect();

    for aug in augmentation {
        if let Some(&idx) = index_by_resource.get(&aug.resource_id) {
            let existing = &mut resources[idx];
            let existing_periods = existing.periods.clone().unwrap_or_default();
            let merged_periods: Vec<bool> = match &aug.periods {
                Some(aug_periods) => {
                    let n = existing_periods.len().max(aug_periods.len());
                    (0..n)
                        .map(|i| {
                            let lhs = existing_periods.get(i).copied().unwrap_or(false);
                            let rhs = aug_periods.get(i).copied().unwrap_or(false);
                            lhs || rhs
                        })
                        .collect()
                }
                None => existing_periods,
            };
            existing.periods = Some(merged_periods);
            existing.initial = existing.initial || aug.initial;
        } else {
            resources.push(aug.clone());
            index_by_resource.insert(aug.resource_id.clone(), resources.len() - 1);
        }
    }

    request.set_commitment(CommitmentPolicy::Fixed(CommitmentSchedule { resources }));
}
