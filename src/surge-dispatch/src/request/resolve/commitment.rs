// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Commitment-policy normalization helpers.

use std::collections::HashSet;

use surge_network::Network;

use crate::dispatch::{CommitmentMode, IndexedCommitmentOptions};
use crate::error::ScedError;
use crate::request::{CommitmentOptions, CommitmentPolicy, DispatchRequest};

use super::registry::ResolveCatalog;

pub(crate) fn resolve_commitment(
    request: &DispatchRequest,
    network: Option<&Network>,
    catalog: Option<&ResolveCatalog>,
) -> Result<CommitmentMode, ScedError> {
    let context = match (request.formulation, request.coupling) {
        (crate::request::Formulation::Ac, _) => "AC dispatch",
        (_, crate::request::IntervalCoupling::TimeCoupled) => "time-coupled dispatch",
        _ => "period-by-period dispatch",
    };

    let requires_network = match &request.commitment {
        CommitmentPolicy::AllCommitted => false,
        CommitmentPolicy::Fixed(schedule) => !schedule.resources.is_empty(),
        CommitmentPolicy::Optimize(options) => !options.initial_conditions.is_empty(),
        CommitmentPolicy::Additional {
            minimum_commitment,
            options,
        } => !minimum_commitment.is_empty() || !options.initial_conditions.is_empty(),
    };

    let Some(_network) = network else {
        if requires_network {
            return Err(ScedError::InvalidInput(format!(
                "{context} commitment resolution requires a network when keyed resource selectors are used"
            )));
        }
        return Ok(match &request.commitment {
            CommitmentPolicy::AllCommitted => CommitmentMode::AllCommitted,
            CommitmentPolicy::Fixed(_) => CommitmentMode::Fixed {
                commitment: Vec::new(),
                per_period: None,
            },
            CommitmentPolicy::Optimize(options) => {
                CommitmentMode::Optimize(IndexedCommitmentOptions {
                    time_limit_secs: options.time_limit_secs,
                    mip_rel_gap: options.mip_rel_gap,
                    mip_gap_schedule: options.mip_gap_schedule.clone(),
                    disable_warm_start: options.disable_warm_start,
                    ..IndexedCommitmentOptions::default()
                })
            }
            CommitmentPolicy::Additional { options, .. } => CommitmentMode::Additional {
                da_commitment: vec![Vec::new(); request.timeline.periods],
                options: IndexedCommitmentOptions {
                    time_limit_secs: options.time_limit_secs,
                    mip_rel_gap: options.mip_rel_gap,
                    mip_gap_schedule: options.mip_gap_schedule.clone(),
                    disable_warm_start: options.disable_warm_start,
                    ..IndexedCommitmentOptions::default()
                },
            },
        });
    };

    let catalog = catalog.expect("resolve catalog required when network is provided");
    let n_in_service = catalog.n_in_service_generators();

    let resolve_options =
        |options: &CommitmentOptions| -> Result<IndexedCommitmentOptions, ScedError> {
            let mut resolved = IndexedCommitmentOptions {
                time_limit_secs: options.time_limit_secs,
                mip_rel_gap: options.mip_rel_gap,
                mip_gap_schedule: options.mip_gap_schedule.clone(),
                disable_warm_start: options.disable_warm_start,
                ..IndexedCommitmentOptions::default()
            };
            let mut seen_resources = HashSet::new();

            for condition in &options.initial_conditions {
                let Some(local_idx) = catalog.resolve_local_gen(condition.resource_id.as_str())
                else {
                    return Err(ScedError::InvalidInput(format!(
                        "commitment initial_conditions references unknown resource {}",
                        condition.resource_id
                    )));
                };
                if !seen_resources.insert(local_idx) {
                    return Err(ScedError::InvalidInput(format!(
                        "commitment initial_conditions contains duplicate resource {}",
                        condition.resource_id
                    )));
                }
                if let Some(value) = condition.committed {
                    resolved
                        .initial_commitment
                        .get_or_insert_with(|| vec![true; n_in_service])[local_idx] = value;
                    resolved
                        .initial_commitment_mask
                        .get_or_insert_with(|| vec![false; n_in_service])[local_idx] = true;
                }
                if let Some(value) = condition.hours_on {
                    resolved
                        .initial_hours_on
                        .get_or_insert_with(|| vec![0; n_in_service])[local_idx] = value;
                    resolved
                        .initial_hours_on_mask
                        .get_or_insert_with(|| vec![false; n_in_service])[local_idx] = true;
                }
                if let Some(value) = condition.offline_hours {
                    resolved
                        .initial_offline_hours
                        .get_or_insert_with(|| vec![0.0; n_in_service])[local_idx] = value;
                    resolved
                        .initial_offline_hours_mask
                        .get_or_insert_with(|| vec![false; n_in_service])[local_idx] = true;
                }
                if let Some(value) = condition.starts_24h {
                    resolved
                        .initial_starts_24h
                        .get_or_insert_with(|| vec![0; n_in_service])[local_idx] = value;
                    resolved
                        .initial_starts_24h_mask
                        .get_or_insert_with(|| vec![false; n_in_service])[local_idx] = true;
                }
                if let Some(value) = condition.starts_168h {
                    resolved
                        .initial_starts_168h
                        .get_or_insert_with(|| vec![0; n_in_service])[local_idx] = value;
                    resolved
                        .initial_starts_168h_mask
                        .get_or_insert_with(|| vec![false; n_in_service])[local_idx] = true;
                }
                if let Some(value) = condition.energy_mwh_24h {
                    resolved
                        .initial_energy_mwh_24h
                        .get_or_insert_with(|| vec![0.0; n_in_service])[local_idx] = value;
                    resolved
                        .initial_energy_mwh_24h_mask
                        .get_or_insert_with(|| vec![false; n_in_service])[local_idx] = true;
                }
            }
            let mut seen_warm_start_resources = HashSet::new();
            for resource in &options.warm_start_commitment {
                let Some(local_idx) = catalog.resolve_local_gen(resource.resource_id.as_str())
                else {
                    return Err(ScedError::InvalidInput(format!(
                        "commitment warm_start_commitment references unknown resource {}",
                        resource.resource_id
                    )));
                };
                if !seen_warm_start_resources.insert(local_idx) {
                    return Err(ScedError::InvalidInput(format!(
                        "commitment warm_start_commitment contains duplicate resource {}",
                        resource.resource_id
                    )));
                }
                if resource.periods.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "commitment warm_start_commitment schedule for {} has {} periods but request timeline expects {}",
                        resource.resource_id,
                        resource.periods.len(),
                        request.timeline.periods
                    )));
                }
                let schedule = resolved.warm_start_commitment.get_or_insert_with(|| {
                    vec![vec![false; n_in_service]; request.timeline.periods]
                });
                for (period, value) in resource.periods.iter().copied().enumerate() {
                    schedule[period][local_idx] = value;
                }
                resolved
                    .warm_start_commitment_mask
                    .get_or_insert_with(|| vec![false; n_in_service])[local_idx] = true;
            }
            Ok(resolved)
        };

    Ok(match &request.commitment {
        CommitmentPolicy::AllCommitted => CommitmentMode::AllCommitted,
        CommitmentPolicy::Fixed(schedule) => {
            let mut commitment = vec![true; n_in_service];
            let mut per_period_overrides = Vec::new();
            let mut seen_resources = HashSet::new();
            for resource in &schedule.resources {
                let Some(local_idx) = catalog.resolve_local_gen(resource.resource_id.as_str())
                else {
                    return Err(ScedError::InvalidInput(format!(
                        "{context} fixed commitment schedule references unknown resource {}",
                        resource.resource_id
                    )));
                };
                if !seen_resources.insert(local_idx) {
                    return Err(ScedError::InvalidInput(format!(
                        "{context} fixed commitment schedule contains duplicate resource {}",
                        resource.resource_id
                    )));
                }
                commitment[local_idx] = resource.initial;
                if let Some(periods) = &resource.periods {
                    if periods.len() != request.timeline.periods {
                        return Err(ScedError::InvalidInput(format!(
                            "{context} schedule for {} has {} periods but request timeline expects {}",
                            resource.resource_id,
                            periods.len(),
                            request.timeline.periods
                        )));
                    }
                    per_period_overrides.push((local_idx, periods.clone()));
                }
            }
            let per_period = if per_period_overrides.is_empty() {
                None
            } else {
                let mut matrix = vec![commitment.clone(); request.timeline.periods];
                for (local_idx, periods) in per_period_overrides {
                    for (period, value) in periods.into_iter().enumerate() {
                        matrix[period][local_idx] = value;
                    }
                }
                Some(matrix)
            };
            CommitmentMode::Fixed {
                commitment,
                per_period,
            }
        }
        CommitmentPolicy::Optimize(options) => CommitmentMode::Optimize(resolve_options(options)?),
        CommitmentPolicy::Additional {
            minimum_commitment,
            options,
        } => {
            let mut da_commitment = vec![vec![false; n_in_service]; request.timeline.periods];
            let mut seen_resources = HashSet::new();
            for resource in minimum_commitment {
                let Some(local_idx) = catalog.resolve_local_gen(resource.resource_id.as_str())
                else {
                    return Err(ScedError::InvalidInput(format!(
                        "{context} minimum_commitment references unknown resource {}",
                        resource.resource_id
                    )));
                };
                if !seen_resources.insert(local_idx) {
                    return Err(ScedError::InvalidInput(format!(
                        "{context} minimum_commitment contains duplicate resource {}",
                        resource.resource_id
                    )));
                }
                if resource.periods.len() != request.timeline.periods {
                    return Err(ScedError::InvalidInput(format!(
                        "{context} minimum_commitment schedule for {} has {} periods but request timeline expects {}",
                        resource.resource_id,
                        resource.periods.len(),
                        request.timeline.periods
                    )));
                }
                for (period, value) in resource.periods.iter().copied().enumerate() {
                    da_commitment[period][local_idx] = value;
                }
            }
            CommitmentMode::Additional {
                da_commitment,
                options: resolve_options(options)?,
            }
        }
    })
}
