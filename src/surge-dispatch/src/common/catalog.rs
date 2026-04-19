// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared solve-time resource catalogs and index maps.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::market::DispatchableLoad;

/// Shared index catalog for in-service resources used by dispatch internals.
#[derive(Debug, Clone, Default)]
pub(crate) struct DispatchCatalog {
    pub in_service_gen_indices: Vec<usize>,
    pub global_to_local_gen: HashMap<usize, usize>,
    pub storage_gen_indices: Vec<usize>,
    pub global_to_local_storage: HashMap<usize, usize>,
    pub active_dispatchable_load_indices: Vec<usize>,
    pub global_to_local_dispatchable_load: HashMap<usize, usize>,
}

impl DispatchCatalog {
    pub fn from_network(network: &Network, dispatchable_loads: &[DispatchableLoad]) -> Self {
        let in_service_gen_indices: Vec<usize> = network
            .generators
            .iter()
            .enumerate()
            .filter_map(|(idx, generator)| generator.in_service.then_some(idx))
            .collect();
        let global_to_local_gen: HashMap<usize, usize> = in_service_gen_indices
            .iter()
            .enumerate()
            .map(|(local_idx, &global_idx)| (global_idx, local_idx))
            .collect();

        let storage_gen_indices: Vec<usize> = in_service_gen_indices
            .iter()
            .copied()
            .filter(|&global_idx| network.generators[global_idx].is_storage())
            .collect();
        let global_to_local_storage: HashMap<usize, usize> = storage_gen_indices
            .iter()
            .enumerate()
            .map(|(local_idx, &global_idx)| (global_idx, local_idx))
            .collect();

        let active_dispatchable_load_indices: Vec<usize> = dispatchable_loads
            .iter()
            .enumerate()
            .filter_map(|(idx, dl)| dl.in_service.then_some(idx))
            .collect();
        let global_to_local_dispatchable_load: HashMap<usize, usize> =
            active_dispatchable_load_indices
                .iter()
                .enumerate()
                .map(|(local_idx, &global_idx)| (global_idx, local_idx))
                .collect();

        Self {
            in_service_gen_indices,
            global_to_local_gen,
            storage_gen_indices,
            global_to_local_storage,
            active_dispatchable_load_indices,
            global_to_local_dispatchable_load,
        }
    }

    pub fn local_gen_index(&self, global_idx: usize) -> Option<usize> {
        self.global_to_local_gen.get(&global_idx).copied()
    }

    pub fn local_storage_index(&self, global_idx: usize) -> Option<usize> {
        self.global_to_local_storage.get(&global_idx).copied()
    }

    pub fn local_dispatchable_load_index(&self, global_idx: usize) -> Option<usize> {
        self.global_to_local_dispatchable_load
            .get(&global_idx)
            .copied()
    }
}
