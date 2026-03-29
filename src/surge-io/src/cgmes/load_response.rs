// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES LoadResponseCharacteristic (ZIP load model) builder.

use std::collections::HashMap;

use surge_network::Network;

use super::indices::CgmesIndices;
use super::types::ObjMap;

// ---------------------------------------------------------------------------
// Wave 27 — LoadResponseCharacteristic (ZIP load model)
// ---------------------------------------------------------------------------

/// Parse CGMES `LoadResponseCharacteristic` ZIP model coefficients and set
/// them directly on each `Load` in `network.loads`.
///
/// CGMES IEC 61970-301 LoadResponseCharacteristic:
/// - `pConstantImpedance` (%), `pConstantCurrent` (%), `pConstantPower` (%) -- P components.
///   These must sum to 100. Default (when absent): 0 / 0 / 100 (constant power).
/// - `qConstantImpedance`, `qConstantCurrent`, `qConstantPower` -- same for Q.
/// - `pVoltageExponent`, `qVoltageExponent` -- exponent for exponential load model
///   (alternative to ZIP; stored as-is for future use).
/// - `EnergyConsumer.LoadResponse` or `ConformLoad.LoadResponseCharacteristic` ->
///   reference from load to characteristic.
///
/// CGMES values are in percent (0-100); they are converted to fractions [0,1]
/// when stored on the `Load` struct fields (pz, pi_frac, pp, qz, qi, qp).
pub(crate) fn build_load_response_chars(
    objects: &ObjMap,
    idx: &CgmesIndices,
    network: &mut Network,
) {
    // Build LRC mRID -> (pZ, pI, pP, qZ, qI, qP) table (still in percent).
    let lrc_params: HashMap<String, (f64, f64, f64, f64, f64, f64)> = objects
        .iter()
        .filter(|(_, o)| o.class == "LoadResponseCharacteristic")
        .map(|(lrc_id, o)| {
            let pz = o.parse_f64("pConstantImpedance").unwrap_or(0.0);
            let pi = o.parse_f64("pConstantCurrent").unwrap_or(0.0);
            let pp = o.parse_f64("pConstantPower").unwrap_or(100.0 - pz - pi);
            let qz = o.parse_f64("qConstantImpedance").unwrap_or(0.0);
            let qi = o.parse_f64("qConstantCurrent").unwrap_or(0.0);
            let qp = o.parse_f64("qConstantPower").unwrap_or(100.0 - qz - qi);
            (lrc_id.clone(), (pz, pi, pp, qz, qi, qp))
        })
        .collect();

    // Map bus_num -> Vec of load indices for efficient lookup.
    let mut bus_to_load_idxs: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, load) in network.loads.iter().enumerate() {
        bus_to_load_idxs.entry(load.bus).or_default().push(i);
    }

    // Link loads to their characteristics via EnergyConsumer.LoadResponse (or
    // ConformLoad.LoadResponseCharacteristic -- both attribute names used in the wild).
    for (ec_id, ec_obj) in objects.iter().filter(|(_, o)| {
        matches!(
            o.class.as_str(),
            "EnergyConsumer" | "ConformLoad" | "NonConformLoad"
        )
    }) {
        let lrc_id = ec_obj
            .get_ref("LoadResponse")
            .or_else(|| ec_obj.get_ref("LoadResponseCharacteristic"));
        let Some(lrc_id) = lrc_id else {
            continue;
        };
        let Some(&(pz, pi, pp, qz, qi, qp)) = lrc_params.get(lrc_id) else {
            continue;
        };
        // Resolve load bus via terminals.
        let terms = idx.terminals(ec_id);
        let Some(tn_id) = terms.first().and_then(|t| idx.terminal_tn(objects, t)) else {
            continue;
        };
        let Some(bus_num) = idx.tn_bus(tn_id) else {
            continue;
        };
        // Set ZIP fractions on all loads at this bus (convert percent -> fraction).
        if let Some(load_idxs) = bus_to_load_idxs.get(&bus_num) {
            for &li in load_idxs {
                network.loads[li].zip_p_impedance_frac = pz / 100.0;
                network.loads[li].zip_p_current_frac = pi / 100.0;
                network.loads[li].zip_p_power_frac = pp / 100.0;
                network.loads[li].zip_q_impedance_frac = qz / 100.0;
                network.loads[li].zip_q_current_frac = qi / 100.0;
                network.loads[li].zip_q_power_frac = qp / 100.0;
            }
        }
        tracing::debug!(
            ec_id,
            bus_num,
            lrc_id,
            "LoadResponseCharacteristic ZIP set on loads (Wave 27)"
        );
    }
}
