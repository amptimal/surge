// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Stable solution/report identifiers shared across dispatch and OPF.

use surge_network::market::{CombinedCyclePlant, DispatchableLoad};
use surge_network::network::Generator;

pub fn default_machine_id(machine_id: Option<&str>) -> String {
    machine_id.unwrap_or("1").to_string()
}

pub fn generator_resource_id(generator: &Generator) -> String {
    if generator.id.is_empty() {
        let machine_id = default_machine_id(generator.machine_id.as_deref());
        if generator.storage.is_some() {
            format!("storage:{}:{machine_id}", generator.bus)
        } else {
            format!("gen:{}:{machine_id}", generator.bus)
        }
    } else {
        generator.id.clone()
    }
}

pub fn dispatchable_load_resource_id(
    dispatchable_load: &DispatchableLoad,
    source_index: usize,
) -> String {
    if dispatchable_load.resource_id.is_empty() {
        format!("dl:{}:{source_index}", dispatchable_load.bus)
    } else {
        dispatchable_load.resource_id.clone()
    }
}

pub fn combined_cycle_plant_id(plant: Option<&CombinedCyclePlant>, plant_index: usize) -> String {
    plant.map_or_else(
        || format!("combined_cycle:{plant_index}"),
        |plant| {
            if !plant.id.is_empty() {
                plant.id.clone()
            } else if !plant.name.is_empty() {
                plant.name.clone()
            } else {
                format!("combined_cycle:{plant_index}")
            }
        },
    )
}
