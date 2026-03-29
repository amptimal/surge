// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES grounding, phase impedance, and mutual coupling builder functions.

use surge_network::Network;
use surge_network::network::grounding::GroundingEntry;
use surge_network::network::model::{MutualCoupling, PhaseImpedanceEntry};

use super::indices::CgmesIndices;
use super::types::ObjMap;

// ---------------------------------------------------------------------------
// Wave 25 — PerLengthPhaseImpedance + MutualCoupling
// ---------------------------------------------------------------------------

pub(crate) fn build_phase_impedances(objects: &ObjMap, network: &mut Network) {
    // PerLengthPhaseImpedance -> matrix entries from PhaseImpedanceData.
    for (pid_id, pid_obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "PhaseImpedanceData")
    {
        let Some(plpi_id) = pid_obj.get_ref("PhaseImpedance").map(|s| s.to_string()) else {
            continue;
        };
        let row = pid_obj
            .get_text("row")
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);
        let col = pid_obj
            .get_text("column")
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);
        let r = pid_obj.parse_f64("r").unwrap_or(0.0);
        let x = pid_obj.parse_f64("x").unwrap_or(0.0);
        let b = pid_obj.parse_f64("b").unwrap_or(0.0);
        network
            .cim
            .per_length_phase_impedances
            .entry(plpi_id)
            .or_default()
            .push(PhaseImpedanceEntry { row, col, r, x, b });
        let _ = pid_id; // used only for iteration
    }

    // MutualCoupling -> inter-line mutual impedance pairs.
    for (_, mc_obj) in objects.iter().filter(|(_, o)| o.class == "MutualCoupling") {
        let Some(t1) = mc_obj.get_ref("First_Terminal").map(|s| s.to_string()) else {
            continue;
        };
        let Some(t2) = mc_obj.get_ref("Second_Terminal").map(|s| s.to_string()) else {
            continue;
        };
        let r0 = mc_obj.parse_f64("r0").unwrap_or(0.0);
        let x0 = mc_obj.parse_f64("x0").unwrap_or(0.0);
        network.cim.mutual_couplings.push(MutualCoupling {
            line1_id: t1,
            line2_id: t2,
            r: r0,
            x: x0,
        });
    }
}

// ---------------------------------------------------------------------------
// Wave 26 — Ground + GroundingImpedance + PetersenCoil
// ---------------------------------------------------------------------------

/// Parse neutral-point grounding equipment into `Network.cim.grounding_impedances`.
///
/// CGMES IEC 61970-301:
/// - `Ground`: solid earthing point. Connected to a bus via a Terminal.
///   Zero impedance (x_ohm = 0). Carries no current in balanced operation.
/// - `GroundingImpedance.x` (EQ, Ohm): neutral grounding reactor connected to a
///   transformer winding via a Terminal.
/// - `PetersenCoil.xGroundNominal` (EQ, Ohm): arc-suppression (Petersen) coil.
///   Connected to transformer neutral. Tuned to cancel zero-seq capacitive current.
///   `PetersenCoil.xGroundMin` / `xGroundMax` give the tuning range.
///
/// All entries are stored as [`GroundingEntry`] structs. These do not affect positive-
/// sequence power flow but are needed for zero-sequence network construction.
pub(crate) fn build_grounding(objects: &ObjMap, idx: &CgmesIndices, network: &mut Network) {
    // Ground: solid earth (x = 0 Ohm).
    for (gnd_id, _gnd_obj) in objects.iter().filter(|(_, o)| o.class == "Ground") {
        let terms = idx.terminals(gnd_id);
        let Some(tn_id) = terms.first().and_then(|t| idx.terminal_tn(objects, t)) else {
            continue;
        };
        let Some(bus_num) = idx.tn_bus(tn_id) else {
            continue;
        };
        network.cim.grounding_impedances.push(GroundingEntry {
            bus: bus_num,
            x_ohm: 0.0,
            x_min_ohm: None,
            x_max_ohm: None,
        });
        tracing::debug!(
            gnd_id,
            bus_num,
            "Ground: solid earthing (x=0) stored (Wave 26)"
        );
    }

    // GroundingImpedance: neutral reactor (x_ohm from GroundingImpedance.x).
    for (gi_id, gi_obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "GroundingImpedance")
    {
        let terms = idx.terminals(gi_id);
        let Some(tn_id) = terms.first().and_then(|t| idx.terminal_tn(objects, t)) else {
            continue;
        };
        let Some(bus_num) = idx.tn_bus(tn_id) else {
            continue;
        };
        let x_ohm = gi_obj.parse_f64("x").unwrap_or(0.0);
        network.cim.grounding_impedances.push(GroundingEntry {
            bus: bus_num,
            x_ohm,
            x_min_ohm: None,
            x_max_ohm: None,
        });
        tracing::debug!(gi_id, bus_num, x_ohm, "GroundingImpedance stored (Wave 26)");
    }

    // PetersenCoil: arc-suppression coil (use xGroundNominal as the effective x_ohm).
    for (pc_id, pc_obj) in objects.iter().filter(|(_, o)| o.class == "PetersenCoil") {
        let terms = idx.terminals(pc_id);
        let Some(tn_id) = terms.first().and_then(|t| idx.terminal_tn(objects, t)) else {
            continue;
        };
        let Some(bus_num) = idx.tn_bus(tn_id) else {
            continue;
        };
        let x_ohm = pc_obj.parse_f64("xGroundNominal").unwrap_or(0.0);
        let x_min_ohm = pc_obj.parse_f64("xGroundMin");
        let x_max_ohm = pc_obj.parse_f64("xGroundMax");
        network.cim.grounding_impedances.push(GroundingEntry {
            bus: bus_num,
            x_ohm,
            x_min_ohm,
            x_max_ohm,
        });
        tracing::debug!(
            pc_id,
            bus_num,
            x_ohm,
            ?x_min_ohm,
            ?x_max_ohm,
            "PetersenCoil stored as grounding impedance (Wave 26)"
        );
    }
}
