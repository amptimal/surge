// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Interop helpers for applying canonical HVDC models to `surge-network` networks.

use num_complex::Complex64;
use surge_network::Network;
use surge_network::network::Load;
use tracing::warn;

use crate::HvdcError;
pub use crate::bridge::psse::hvdc_links_from_network as links_from_network;
use crate::solver::hybrid_glue::build_hybrid_mtdc_from_network;
use crate::solver::hybrid_mtdc::solve_hybrid_mtdc;

/// A computed P/Q injection from a single DC-grid converter terminal.
#[derive(Debug, Clone)]
pub struct DcGridInjection {
    pub ac_bus: u32,
    pub p_mw: f64,
    pub q_mvar: f64,
    pub grid_name: String,
    pub converter_id: String,
}

/// Results from the explicit DC-grid solve: converter injections plus DC-network losses.
#[derive(Debug, Clone, Default)]
pub struct DcGridResults {
    pub injections: Vec<DcGridInjection>,
    pub total_dc_loss_mw: f64,
}

fn explicit_dc_results(
    network: &Network,
    ac_voltages: &[Complex64],
) -> Result<DcGridResults, HvdcError> {
    if !network.hvdc.has_explicit_dc_topology() {
        return Ok(DcGridResults::default());
    }

    let hybrid = build_hybrid_mtdc_from_network(network)?;
    let result = solve_hybrid_mtdc(&hybrid, ac_voltages, 50, 1e-6)?;
    let lcc_meta = explicit_converter_metadata(network, true);
    let vsc_meta = explicit_converter_metadata(network, false);

    let mut injections = Vec::new();
    for (lcc, meta) in result.lcc_results.iter().zip(lcc_meta.iter()) {
        injections.push(DcGridInjection {
            ac_bus: lcc.bus_ac,
            p_mw: lcc.p_ac_mw,
            q_mvar: lcc.q_ac_mvar,
            grid_name: meta.grid_name.clone(),
            converter_id: meta.converter_id.clone(),
        });
    }
    for (vsc, meta) in result.vsc_results.iter().zip(vsc_meta.iter()) {
        injections.push(DcGridInjection {
            ac_bus: vsc.bus_ac,
            p_mw: vsc.p_ac_mw,
            q_mvar: vsc.q_ac_mvar,
            grid_name: meta.grid_name.clone(),
            converter_id: meta.converter_id.clone(),
        });
    }

    Ok(DcGridResults {
        injections,
        total_dc_loss_mw: result.total_dc_loss_mw,
    })
}

#[derive(Debug, Clone)]
struct ExplicitConverterMetadata {
    grid_name: String,
    converter_id: String,
}

fn converter_identity(
    grid: &surge_network::network::DcGrid,
    converter_index: usize,
    converter: &surge_network::network::DcConverter,
) -> String {
    let trimmed = converter.id().trim();
    if trimmed.is_empty() {
        format!("dc_grid_{}_converter_{}", grid.id, converter_index + 1)
    } else {
        trimmed.to_string()
    }
}

fn explicit_converter_metadata(network: &Network, lcc: bool) -> Vec<ExplicitConverterMetadata> {
    let mut meta = Vec::new();
    for grid in &network.hvdc.dc_grids {
        let grid_name = grid
            .name
            .clone()
            .unwrap_or_else(|| format!("dc_grid_{}", grid.id));
        for (index, converter) in grid.converters.iter().enumerate() {
            if !converter.is_in_service() || converter.is_lcc() != lcc {
                continue;
            }
            meta.push(ExplicitConverterMetadata {
                grid_name: grid_name.clone(),
                converter_id: converter_identity(grid, index, converter),
            });
        }
    }
    meta
}

/// Solve the explicit DC-grid model with flat AC voltages and return converter injections.
pub fn dc_grid_injections(network: &Network) -> Result<DcGridResults, HvdcError> {
    let max_bus = network
        .buses
        .iter()
        .map(|bus| bus.number as usize)
        .max()
        .unwrap_or(0);
    let flat_ac = vec![Complex64::new(1.0, 0.0); max_bus + 1];
    explicit_dc_results(network, &flat_ac)
}

/// Solve the explicit DC-grid model with actual AC voltages and return converter injections.
pub fn dc_grid_injections_from_voltages(
    network: &Network,
    ac_voltages: &[Complex64],
) -> Result<DcGridResults, HvdcError> {
    explicit_dc_results(network, ac_voltages)
}

/// Apply converter injections directly to the AC network clone.
pub fn apply_dc_grid_injections(
    network: &mut Network,
    injections: &[DcGridInjection],
    tag_loads: bool,
) {
    if injections.is_empty() {
        return;
    }
    let bus_map = network.bus_index_map();
    for injection in injections {
        if !bus_map.contains_key(&injection.ac_bus) {
            warn!(
                "apply_dc_grid_injections: AC bus {} not found in network",
                injection.ac_bus
            );
            continue;
        }
        let mut synthetic = Load::new(injection.ac_bus, -injection.p_mw, -injection.q_mvar);
        synthetic.conforming = false;
        if tag_loads {
            synthetic.id = format!("__dc_grid_converter_{}", injection.converter_id);
        }
        network.loads.push(synthetic);
    }
}

/// Raw PSS/E-style conversion helpers.
pub mod psse {
    pub use crate::bridge::psse::{
        lcc_from_dc_line as lcc_link_from_dc_line, vsc_from_vsc_dc_line as vsc_link_from_dc_line,
    };
}
