// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Raw PSS/E multi-terminal DC records.
//!
//! These structs preserve the RAW section shape long enough for the PSS/E
//! reader to normalize them into the canonical `surge-network` HVDC model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawMtdcSystem {
    pub name: String,
    pub n_converters: u32,
    pub n_dc_buses: u32,
    pub n_dc_links: u32,
    pub control_mode: u32,
    pub dc_voltage_kv: f64,
    pub voltage_mode_switch_kv: f64,
    pub dc_voltage_min_kv: f64,
    pub converters: Vec<RawMtdcConverter>,
    pub dc_buses: Vec<RawMtdcBus>,
    pub dc_links: Vec<RawMtdcLink>,
}

impl Default for RawMtdcSystem {
    fn default() -> Self {
        Self {
            name: String::new(),
            n_converters: 0,
            n_dc_buses: 0,
            n_dc_links: 0,
            control_mode: 1,
            dc_voltage_kv: 0.0,
            voltage_mode_switch_kv: 0.0,
            dc_voltage_min_kv: 0.0,
            converters: Vec::new(),
            dc_buses: Vec::new(),
            dc_links: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawMtdcConverter {
    pub bus: u32,
    pub n_bridges: u32,
    pub alpha_max: f64,
    pub alpha_min: f64,
    pub commutation_resistance_ohm: f64,
    pub commutation_reactance_ohm: f64,
    pub base_voltage_kv: f64,
    pub turns_ratio: f64,
    pub tap: f64,
    pub tap_max: f64,
    pub tap_min: f64,
    pub tap_step: f64,
    pub scheduled_setpoint: f64,
    pub dcpf: f64,
    pub marg: f64,
    pub cnvcod: u32,
}

impl Default for RawMtdcConverter {
    fn default() -> Self {
        Self {
            bus: 0,
            n_bridges: 1,
            alpha_max: 90.0,
            alpha_min: 5.0,
            commutation_resistance_ohm: 0.0,
            commutation_reactance_ohm: 0.0,
            base_voltage_kv: 0.0,
            turns_ratio: 1.0,
            tap: 1.0,
            tap_max: 1.1,
            tap_min: 0.9,
            tap_step: 0.00625,
            scheduled_setpoint: 0.0,
            dcpf: 0.0,
            marg: 0.0,
            cnvcod: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawMtdcBus {
    pub dc_bus: u32,
    pub ac_bus: u32,
    pub area: u32,
    pub zone: u32,
    pub name: String,
    pub idc2: u32,
    pub rgrnd: f64,
    pub owner: u32,
}

impl Default for RawMtdcBus {
    fn default() -> Self {
        Self {
            dc_bus: 0,
            ac_bus: 0,
            area: 1,
            zone: 1,
            name: String::new(),
            idc2: 0,
            rgrnd: 0.0,
            owner: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawMtdcLink {
    pub from_dc_bus: u32,
    pub to_dc_bus: u32,
    pub circuit: String,
    pub metered: u32,
    pub resistance_ohm: f64,
    pub ldc: f64,
}

impl Default for RawMtdcLink {
    fn default() -> Self {
        Self {
            from_dc_bus: 0,
            to_dc_bus: 0,
            circuit: "1".to_string(),
            metered: 1,
            resistance_ohm: 0.0,
            ldc: 0.0,
        }
    }
}
