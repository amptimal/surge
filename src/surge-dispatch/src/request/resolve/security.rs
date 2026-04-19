// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Security-screening normalization helpers.

use surge_network::Network;

use crate::error::ScedError;
use crate::request::{DispatchRequest, ResolvedSecurityScreening};

use super::registry::ResolveCatalog;

pub(crate) fn resolve_security(
    request: &DispatchRequest,
    network: Option<&Network>,
    catalog: Option<&ResolveCatalog>,
) -> Result<Option<ResolvedSecurityScreening>, ScedError> {
    let Some(security) = &request.network.security else {
        return Ok(None);
    };

    let requires_network =
        !security.branch_contingencies.is_empty() || !security.hvdc_contingencies.is_empty();
    let Some(_network) = network else {
        if requires_network {
            return Err(ScedError::InvalidInput(
                "security screening with keyed contingencies requires a network".to_string(),
            ));
        }
        return Ok(Some(ResolvedSecurityScreening {
            embedding: security.embedding,
            max_iterations: security.max_iterations,
            violation_tolerance_pu: security.violation_tolerance_pu,
            max_cuts_per_iteration: security.max_cuts_per_iteration,
            contingency_branches: Vec::new(),
            hvdc_contingency_indices: Vec::new(),
            preseed_count_per_period: security.preseed_count_per_period,
            preseed_method: security.preseed_method,
        }));
    };

    let catalog = catalog.expect("resolve catalog required when network is provided");
    let mut contingency_branches = Vec::with_capacity(security.branch_contingencies.len());
    let mut seen_branch_indices = std::collections::HashSet::new();
    for branch in &security.branch_contingencies {
        let key = (branch.from_bus, branch.to_bus, branch.circuit.clone());
        let Some(&branch_idx) = catalog.branch_index_map.get(&key) else {
            return Err(ScedError::InvalidInput(format!(
                "security.branch_contingencies references unknown branch ({}, {}, {})",
                branch.from_bus, branch.to_bus, branch.circuit
            )));
        };
        if !seen_branch_indices.insert(branch_idx) {
            return Err(ScedError::InvalidInput(format!(
                "security.branch_contingencies contains duplicate branch ({}, {}, {})",
                branch.from_bus, branch.to_bus, branch.circuit
            )));
        }
        contingency_branches.push(branch_idx);
    }

    let mut hvdc_contingency_indices = Vec::with_capacity(security.hvdc_contingencies.len());
    let mut seen_hvdc_indices = std::collections::HashSet::new();
    for link in &security.hvdc_contingencies {
        let Some(link_idx) =
            catalog.resolve_hvdc(link.link_id.as_str(), request.network.hvdc_links.len())
        else {
            return Err(ScedError::InvalidInput(format!(
                "security.hvdc_contingencies references unknown link_id {}",
                link.link_id
            )));
        };
        if !seen_hvdc_indices.insert(link_idx) {
            return Err(ScedError::InvalidInput(format!(
                "security.hvdc_contingencies contains duplicate link_id {}",
                link.link_id
            )));
        }
        hvdc_contingency_indices.push(link_idx);
    }

    Ok(Some(ResolvedSecurityScreening {
        embedding: security.embedding,
        max_iterations: security.max_iterations,
        violation_tolerance_pu: security.violation_tolerance_pu,
        max_cuts_per_iteration: security.max_cuts_per_iteration,
        contingency_branches,
        hvdc_contingency_indices,
        preseed_count_per_period: security.preseed_count_per_period,
        preseed_method: security.preseed_method,
    }))
}
