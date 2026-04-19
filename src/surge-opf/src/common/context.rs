// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared validated/indexed network context for OPF solver families.

use std::collections::HashMap;

use surge_network::Network;

use crate::ac::types::AcOpfError;
use crate::dc::island_lmp::{IslandRefs, detect_island_refs};
use crate::dc::opf::DcOpfError;

/// Shared validated/indexed network state reused across OPF formulations.
pub(crate) struct OpfNetworkContext<'a> {
    pub(crate) network: &'a Network,
    pub(crate) n_bus: usize,
    pub(crate) n_branches: usize,
    pub(crate) base_mva: f64,
    pub(crate) bus_map: HashMap<u32, usize>,
    pub(crate) branch_idx_map: HashMap<(u32, u32, String), usize>,
    pub(crate) slack_idx: usize,
    /// Global generator indices in stable in-service order.
    pub(crate) gen_indices: Vec<usize>,
    /// Local in-service generator indices grouped by internal bus index.
    pub(crate) bus_gen_map: Vec<Vec<usize>>,
    /// In-service branch indices in stable network order.
    pub(crate) in_service_branch_indices: Vec<usize>,
    pub(crate) total_load_mw: f64,
    pub(crate) total_capacity_mw: f64,
    pub(crate) island_refs: IslandRefs,
}

impl<'a> OpfNetworkContext<'a> {
    /// Build the shared context and map any validation/indexing failures to DC-OPF errors.
    pub(crate) fn for_dc(network: &'a Network) -> Result<Self, DcOpfError> {
        Self::build(network, false).map_err(DcOpfError::from)
    }

    /// Build the shared context and map any validation/indexing failures to AC-OPF errors.
    pub(crate) fn for_ac(network: &'a Network) -> Result<Self, AcOpfError> {
        Self::build(network, true).map_err(AcOpfError::from)
    }

    fn build(
        network: &'a Network,
        allow_storage_without_cost: bool,
    ) -> Result<Self, OpfContextError> {
        let n_bus = network.n_buses();
        let n_br = network.n_branches();
        let base_mva = network.base_mva;
        let bus_map = network.bus_index_map();
        let branch_idx_map = network.branch_index_map();
        let slack_idx = network
            .slack_bus_index()
            .ok_or(OpfContextError::NoSlackBus)?;

        let gen_indices: Vec<usize> = network
            .generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.in_service)
            .map(|(i, _)| i)
            .collect();

        if gen_indices.is_empty() {
            return Err(OpfContextError::NoGenerators);
        }

        for &gi in &gen_indices {
            let g = &network.generators[gi];
            if allow_storage_without_cost && g.is_storage() {
                continue;
            }
            if g.cost.is_none() {
                return Err(OpfContextError::MissingCost {
                    gen_idx: gi,
                    bus: g.bus,
                });
            }
        }

        let mut bus_gen_map: Vec<Vec<usize>> = vec![Vec::new(); n_bus];
        for (local_idx, &gi) in gen_indices.iter().enumerate() {
            let bus_idx = bus_map[&network.generators[gi].bus];
            bus_gen_map[bus_idx].push(local_idx);
        }

        let in_service_branch_indices: Vec<usize> = network
            .branches
            .iter()
            .enumerate()
            .filter(|(_, br)| br.in_service)
            .map(|(i, _)| i)
            .collect();

        let total_load_mw: f64 = network.total_load_mw();
        let total_capacity_mw: f64 = gen_indices
            .iter()
            .map(|&gi| network.generators[gi].pmax)
            .sum();
        let island_refs = detect_island_refs(network, &bus_map);

        Ok(Self {
            network,
            n_bus,
            n_branches: n_br,
            base_mva,
            bus_map,
            branch_idx_map,
            slack_idx,
            gen_indices,
            bus_gen_map,
            in_service_branch_indices,
            total_load_mw,
            total_capacity_mw,
            island_refs,
        })
    }

    pub(crate) fn constrained_branch_indices(&self, min_rate_a: f64) -> Vec<usize> {
        self.in_service_branch_indices
            .iter()
            .copied()
            .filter(|&idx| self.network.branches[idx].rating_a_mva >= min_rate_a)
            .collect()
    }
}

#[derive(Debug)]
enum OpfContextError {
    NoSlackBus,
    NoGenerators,
    MissingCost { gen_idx: usize, bus: u32 },
}

macro_rules! impl_from_context_error {
    ($target:ty) => {
        impl From<OpfContextError> for $target {
            fn from(value: OpfContextError) -> Self {
                match value {
                    OpfContextError::NoSlackBus => Self::NoSlackBus,
                    OpfContextError::NoGenerators => Self::NoGenerators,
                    OpfContextError::MissingCost { gen_idx, bus } => {
                        Self::MissingCost { gen_idx, bus }
                    }
                }
            }
        }
    };
}

impl_from_context_error!(DcOpfError);
impl_from_context_error!(AcOpfError);
