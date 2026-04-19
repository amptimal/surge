// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Stable public id helpers shared by extraction and keyed reporting.

use surge_network::Network;
use surge_network::market::DispatchableLoad;
use surge_network::network::{Branch, Generator};
pub(crate) use surge_solution::{
    combined_cycle_plant_id, default_machine_id, dispatchable_load_resource_id,
    generator_resource_id,
};

use crate::DispatchResource;
use crate::DispatchResourceKind;
use crate::hvdc::HvdcDispatchLink;

pub(crate) fn generator_resource_kind(generator: &Generator) -> DispatchResourceKind {
    if generator.storage.is_some() {
        DispatchResourceKind::Storage
    } else {
        DispatchResourceKind::Generator
    }
}

pub(crate) fn hvdc_link_id(link: &HvdcDispatchLink, source_index: usize) -> String {
    if !link.id.is_empty() {
        link.id.clone()
    } else if !link.name.is_empty() {
        link.name.clone()
    } else {
        format!("hvdc:{source_index}")
    }
}

pub(crate) fn branch_subject_id(branch: &Branch) -> String {
    format!(
        "branch:{}:{}:{}",
        branch.from_bus, branch.to_bus, branch.circuit
    )
}

pub(crate) fn flowgate_subject_id(name: &str, index: usize) -> String {
    if name.is_empty() {
        format!("flowgate:{index}")
    } else {
        name.to_string()
    }
}

pub(crate) fn interface_subject_id(name: &str, index: usize) -> String {
    if name.is_empty() {
        format!("interface:{index}")
    } else {
        name.to_string()
    }
}

pub(crate) fn reserve_requirement_subject_id(product_id: &str, zone_id: Option<usize>) -> String {
    match zone_id {
        Some(zone_id) => format!("reserve:zone:{zone_id}:{product_id}"),
        None => format!("reserve:system:{product_id}"),
    }
}

pub(crate) fn build_resource_catalog(
    network: &Network,
    dispatchable_loads: &[DispatchableLoad],
) -> Vec<DispatchResource> {
    let mut resources: Vec<DispatchResource> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, generator)| generator.in_service)
        .map(|(source_index, generator)| {
            let resource_id = generator_resource_id(generator);
            DispatchResource {
                resource_id,
                kind: generator_resource_kind(generator),
                bus_number: Some(generator.bus),
                machine_id: Some(default_machine_id(generator.machine_id.as_deref())),
                name: None,
                source_index,
            }
        })
        .collect();

    resources.extend(
        dispatchable_loads
            .iter()
            .enumerate()
            .filter(|&(_source_index, dispatchable_load)| dispatchable_load.in_service)
            .map(|(source_index, dispatchable_load)| DispatchResource {
                resource_id: dispatchable_load_resource_id(dispatchable_load, source_index),
                kind: DispatchResourceKind::DispatchableLoad,
                bus_number: Some(dispatchable_load.bus),
                machine_id: None,
                name: None,
                source_index,
            }),
    );

    resources
}
