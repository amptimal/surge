// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared keyed selector resolution for request normalization.

use std::collections::HashMap;

use surge_network::Network;
use surge_solution::{
    combined_cycle_plant_id, dispatchable_load_resource_id, generator_resource_id,
};

use crate::error::ScedError;
use crate::hvdc::HvdcDispatchLink;
use crate::request::DispatchRequest;

pub(crate) struct ResolveCatalog {
    pub(crate) in_service_gen_indices: Vec<usize>,
    pub(crate) global_gen_by_id: HashMap<String, usize>,
    pub(crate) local_gen_by_id: HashMap<String, usize>,
    pub(crate) dispatchable_load_by_id: HashMap<String, usize>,
    pub(crate) hvdc_by_id: HashMap<String, usize>,
    pub(crate) combined_cycle_plant_by_id: HashMap<String, usize>,
    pub(crate) bus_index_map: HashMap<u32, usize>,
    pub(crate) branch_index_map: HashMap<(u32, u32, String), usize>,
}

impl ResolveCatalog {
    pub(crate) fn from_request(
        request: &DispatchRequest,
        network: &Network,
    ) -> Result<Self, ScedError> {
        let mut global_gen_by_id = HashMap::new();
        let mut local_gen_by_id = HashMap::new();
        let mut in_service_gen_indices = Vec::new();
        for (global_idx, generator) in network.generators.iter().enumerate() {
            if !generator.in_service {
                continue;
            }
            let resource_id = generator_resource_id(generator);
            require_non_empty_key(&resource_id, "generator resource_id")?;
            if global_gen_by_id.contains_key(&resource_id) {
                return Err(ScedError::InvalidInput(format!(
                    "duplicate in-service generator resource_id {}",
                    resource_id
                )));
            }
            let local_idx = in_service_gen_indices.len();
            in_service_gen_indices.push(global_idx);
            global_gen_by_id.insert(resource_id.clone(), global_idx);
            local_gen_by_id.insert(resource_id, local_idx);
        }

        let mut dispatchable_load_by_id = HashMap::new();
        for (dl_index, dispatchable_load) in request.market.dispatchable_loads.iter().enumerate() {
            if !dispatchable_load.in_service {
                continue;
            }
            let resource_id = dispatchable_load_resource_id(dispatchable_load, dl_index);
            require_non_empty_key(&resource_id, "dispatchable load resource_id")?;
            if dispatchable_load_by_id
                .insert(resource_id.clone(), dl_index)
                .is_some()
            {
                return Err(ScedError::InvalidInput(format!(
                    "duplicate dispatchable load resource_id {}",
                    resource_id
                )));
            }
        }

        let mut hvdc_by_id = HashMap::new();
        for (link_index, link) in request.network.hvdc_links.iter().enumerate() {
            let link_id = canonical_hvdc_link_id(link, link_index);
            require_non_empty_key(&link_id, "HVDC link id")?;
            if hvdc_by_id.insert(link_id.clone(), link_index).is_some() {
                return Err(ScedError::InvalidInput(format!(
                    "duplicate HVDC link id {}",
                    link_id
                )));
            }
        }

        let mut combined_cycle_plant_by_id = HashMap::new();
        for (plant_index, plant) in network.market_data.combined_cycle_plants.iter().enumerate() {
            let plant_id = combined_cycle_plant_id(Some(plant), plant_index);
            if combined_cycle_plant_by_id
                .insert(plant_id.clone(), plant_index)
                .is_some()
            {
                return Err(ScedError::InvalidInput(format!(
                    "duplicate combined-cycle plant id {}",
                    plant_id
                )));
            }
        }

        Ok(Self {
            in_service_gen_indices,
            global_gen_by_id,
            local_gen_by_id,
            dispatchable_load_by_id,
            hvdc_by_id,
            combined_cycle_plant_by_id,
            bus_index_map: network.bus_index_map(),
            branch_index_map: network.branch_index_map(),
        })
    }

    pub(crate) fn n_in_service_generators(&self) -> usize {
        self.in_service_gen_indices.len()
    }

    pub(crate) fn resolve_local_gen(&self, resource_id: &str) -> Option<usize> {
        self.local_gen_by_id
            .get(resource_id)
            .copied()
            .or_else(|| resolve_test_local_gen_index(resource_id, self.n_in_service_generators()))
    }

    pub(crate) fn resolve_global_gen(&self, resource_id: &str) -> Option<usize> {
        self.global_gen_by_id
            .get(resource_id)
            .copied()
            .or_else(|| resolve_test_global_gen_index(resource_id, &self.in_service_gen_indices))
    }

    pub(crate) fn resolve_dispatchable_load(
        &self,
        resource_id: &str,
        n_dispatchable_loads: usize,
    ) -> Option<usize> {
        self.dispatchable_load_by_id
            .get(resource_id)
            .copied()
            .or_else(|| resolve_test_dispatchable_load_index(resource_id, n_dispatchable_loads))
    }

    pub(crate) fn resolve_hvdc(&self, link_id: &str, n_links: usize) -> Option<usize> {
        self.hvdc_by_id
            .get(link_id)
            .copied()
            .or_else(|| resolve_test_hvdc_index(link_id, n_links))
    }

    pub(crate) fn resolve_bus(&self, bus_number: u32, n_buses: usize) -> Option<usize> {
        self.bus_index_map
            .get(&bus_number)
            .copied()
            .or_else(|| resolve_test_bus_index(bus_number, n_buses))
    }

    pub(crate) fn resolve_combined_cycle_plant(
        &self,
        plant_id: &str,
        n_plants: usize,
    ) -> Option<usize> {
        self.combined_cycle_plant_by_id
            .get(plant_id)
            .copied()
            .or_else(|| resolve_test_combined_cycle_plant_index(plant_id, n_plants))
    }
}

pub(crate) fn resolve_combined_cycle_config(config_name: &str, n_configs: usize) -> Option<usize> {
    resolve_test_combined_cycle_config_index(config_name, n_configs)
}

fn require_non_empty_key(key: &str, context: &str) -> Result<(), ScedError> {
    if key.trim().is_empty() {
        return Err(ScedError::InvalidInput(format!(
            "{context} requires a non-empty key"
        )));
    }
    Ok(())
}

fn canonical_hvdc_link_id(link: &HvdcDispatchLink, source_index: usize) -> String {
    if !link.id.is_empty() {
        link.id.clone()
    } else if !link.name.is_empty() {
        link.name.clone()
    } else {
        format!("hvdc:{source_index}")
    }
}

#[cfg(test)]
fn resolve_test_local_gen_index(resource_id: &str, n_in_service: usize) -> Option<usize> {
    let idx = resource_id
        .strip_prefix("__gen_local:")?
        .parse::<usize>()
        .ok()?;
    (idx < n_in_service).then_some(idx)
}

#[cfg(not(test))]
fn resolve_test_local_gen_index(_resource_id: &str, _n_in_service: usize) -> Option<usize> {
    None
}

#[cfg(test)]
fn resolve_test_global_gen_index(
    resource_id: &str,
    in_service_gen_indices: &[usize],
) -> Option<usize> {
    if let Some(idx) = resource_id
        .strip_prefix("__gen_global:")
        .and_then(|value| value.parse::<usize>().ok())
    {
        return in_service_gen_indices.contains(&idx).then_some(idx);
    }
    let local_idx = resolve_test_local_gen_index(resource_id, in_service_gen_indices.len())?;
    in_service_gen_indices.get(local_idx).copied()
}

#[cfg(not(test))]
fn resolve_test_global_gen_index(
    _resource_id: &str,
    _in_service_gen_indices: &[usize],
) -> Option<usize> {
    None
}

#[cfg(test)]
fn resolve_test_dispatchable_load_index(
    resource_id: &str,
    n_dispatchable_loads: usize,
) -> Option<usize> {
    let idx = resource_id.strip_prefix("__dl:")?.parse::<usize>().ok()?;
    (idx < n_dispatchable_loads).then_some(idx)
}

#[cfg(not(test))]
fn resolve_test_dispatchable_load_index(
    _resource_id: &str,
    _n_dispatchable_loads: usize,
) -> Option<usize> {
    None
}

#[cfg(test)]
fn resolve_test_hvdc_index(link_id: &str, n_links: usize) -> Option<usize> {
    let idx = link_id.strip_prefix("__hvdc:")?.parse::<usize>().ok()?;
    (idx < n_links).then_some(idx)
}

#[cfg(not(test))]
fn resolve_test_hvdc_index(_link_id: &str, _n_links: usize) -> Option<usize> {
    None
}

#[cfg(test)]
fn resolve_test_bus_index(bus_number: u32, n_buses: usize) -> Option<usize> {
    const TEST_BUS_OFFSET: u32 = 4_000_000_000;
    let idx = bus_number.checked_sub(TEST_BUS_OFFSET)? as usize;
    (idx < n_buses).then_some(idx)
}

#[cfg(not(test))]
fn resolve_test_bus_index(_bus_number: u32, _n_buses: usize) -> Option<usize> {
    None
}

#[cfg(test)]
fn resolve_test_combined_cycle_plant_index(plant_id: &str, n_plants: usize) -> Option<usize> {
    let idx = plant_id.strip_prefix("__cc:")?.parse::<usize>().ok()?;
    (idx < n_plants).then_some(idx)
}

#[cfg(not(test))]
fn resolve_test_combined_cycle_plant_index(_plant_id: &str, _n_plants: usize) -> Option<usize> {
    None
}

#[cfg(test)]
fn resolve_test_combined_cycle_config_index(config_name: &str, n_configs: usize) -> Option<usize> {
    let idx = config_name
        .strip_prefix("__cc_config:")?
        .parse::<usize>()
        .ok()?;
    (idx < n_configs).then_some(idx)
}

#[cfg(not(test))]
fn resolve_test_combined_cycle_config_index(
    _config_name: &str,
    _n_configs: usize,
) -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use surge_network::Network;
    use surge_network::market::DispatchableLoad;
    use surge_network::network::{Bus, BusType, Generator};

    use super::ResolveCatalog;
    use crate::request::DispatchRequest;

    #[test]
    fn resolve_catalog_uses_shared_generator_resource_ids() {
        let mut network = Network::new("resolve_ids");
        network.buses.push(Bus::new(101, BusType::Slack, 138.0));
        let mut generator = Generator::new(101, 25.0, 1.0);
        generator.id.clear();
        generator.machine_id = Some("7".to_string());
        network.generators.push(generator);

        let request = DispatchRequest::default();
        let catalog = ResolveCatalog::from_request(&request, &network)
            .expect("catalog should build with canonical generator ids");

        assert_eq!(catalog.resolve_global_gen("gen:101:7"), Some(0));
        assert_eq!(catalog.resolve_local_gen("gen:101:7"), Some(0));
    }

    #[test]
    fn resolve_catalog_uses_shared_dispatchable_load_resource_ids() {
        let mut network = Network::new("resolve_load_ids");
        network.buses.push(Bus::new(202, BusType::Slack, 138.0));

        let mut request = DispatchRequest::default();
        request
            .market
            .dispatchable_loads
            .push(DispatchableLoad::curtailable(
                202, 10.0, 0.0, 0.0, 100.0, 100.0,
            ));

        let catalog = ResolveCatalog::from_request(&request, &network)
            .expect("catalog should build with canonical load ids");

        assert_eq!(catalog.resolve_dispatchable_load("dl:202:0", 1), Some(0));
    }
}
