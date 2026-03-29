// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! External format adapters and AC-MTDC integration.

pub mod mtdc_builder;
pub mod psse;

pub(crate) fn lcc_commutation_reactance_pu(
    commutation_reactance_ohm: f64,
    n_bridges: u32,
    base_voltage_kv: f64,
    base_mva: f64,
) -> Option<f64> {
    if n_bridges == 0 || base_voltage_kv <= 0.0 || base_mva <= 0.0 {
        return None;
    }
    let z_base = base_voltage_kv * base_voltage_kv / base_mva;
    Some(commutation_reactance_ohm * n_bridges as f64 / z_base)
}
