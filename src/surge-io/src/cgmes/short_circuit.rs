// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES Short Circuit (SC) profile parser.
//!
//! Parses CIM IEC 61970-456 short circuit data and wires zero-sequence and
//! negative-sequence impedance data into the existing `Network` model fields.
//!
//! ## Classes handled
//! - **SynchronousMachineDetailed** → `Generator.r0_pu`, `x0_pu`, `r2_pu`, `x2_pu`
//! - **EquivalentInjection** → `Generator.r0_pu`, `x0_pu`, `r2_pu`, `x2_pu`
//! - **ACLineSegment** → `Branch.r0`, `x0`, `b0` (zero-sequence line impedance)
//! - **PowerTransformerEnd** → `Branch.r0`, `x0`, `zn`, `transformer_connection`
//! - **TransformerStarImpedance** → `Branch.r0`, `x0` (via TransformerEnd linkage)
//!
//! ## Unit conventions
//! - CIM machine data (SynchronousMachineDetailed): already in per-unit on machine
//!   base — used directly (stored as-is in Generator fields).
//! - CIM line/transformer impedances: physical Ohms → per-unit via `ohm_to_pu()`.
//! - CIM susceptances: Siemens → per-unit via `siemens_to_pu()`.

use std::collections::HashMap;

use num_complex::Complex64;
use surge_network::Network;
use surge_network::network::{TransformerConnection, TransformerData, ZeroSeqData};

use super::helpers::{ohm_to_pu, siemens_to_pu};
use super::indices::CgmesIndices;
use super::types::ObjMap;

/// Wire short circuit profile data into an already-built `Network`.
///
/// Must be called **after** `build_network()` + `build_generators_and_loads()` so
/// that all branches and generators exist. Matches CIM objects to Network elements
/// via equipment mRID (branches use `br.circuit`) and bus number (generators).
pub(crate) fn build_short_circuit_data(
    objects: &ObjMap,
    idx: &CgmesIndices,
    network: &mut Network,
) {
    let base_mva = network.base_mva;

    // --- Build lookup maps ---

    // equipment mRID → branch index (br.circuit stores the CGMES equipment mRID)
    let eq_to_br: HashMap<String, usize> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| !br.circuit.is_empty())
        .map(|(i, br)| (br.circuit.clone(), i))
        .collect();

    // bus number → list of generator indices (multiple generators can share a bus)
    let mut bus_to_gens: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, g) in network.generators.iter().enumerate() {
        bus_to_gens.entry(g.bus).or_default().push(i);
    }

    // --- SynchronousMachine / SynchronousMachineDetailed → Generator zero/neg seq ---
    //
    // The SC profile may provide r0, x0, r2, x2Subtransient on objects whose class
    // is SynchronousMachine or SynchronousMachineDetailed (the latter is a CIM subclass
    // that carries the extra SC fields). After profile merge, either class name may
    // appear. We check both and extract SC attributes when present.
    let mut sc_gen_count = 0u32;
    for (sm_id, sm) in objects
        .iter()
        .filter(|(_, o)| o.class == "SynchronousMachine" || o.class == "SynchronousMachineDetailed")
    {
        // Check if any SC attribute is present — skip if none.
        let r0 = sm.parse_f64("r0");
        let x0 = sm.parse_f64("x0");
        let r2 = sm.parse_f64("r2");
        let x2 = sm.parse_f64("x2Subtransient");
        if r0.is_none() && x0.is_none() && r2.is_none() && x2.is_none() {
            continue;
        }

        // Resolve SM → bus number via terminal.
        let bus_num = idx.terminals(sm_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let Some(bus_num) = bus_num else { continue };

        // Find generator(s) at this bus and apply SC data.
        if let Some(gen_indices) = bus_to_gens.get(&bus_num) {
            for &gi in gen_indices {
                let g = &mut network.generators[gi];
                if let Some(v) = r0 {
                    g.fault_data.get_or_insert_with(Default::default).r0_pu = Some(v);
                }
                if let Some(v) = x0 {
                    g.fault_data.get_or_insert_with(Default::default).x0_pu = Some(v);
                }
                if let Some(v) = r2 {
                    g.fault_data.get_or_insert_with(Default::default).r2_pu = Some(v);
                }
                if let Some(v) = x2 {
                    g.fault_data.get_or_insert_with(Default::default).x2_pu = Some(v);
                }
                sc_gen_count += 1;
            }
            tracing::debug!(
                sm_id,
                bus_num,
                ?r0,
                ?x0,
                ?r2,
                ?x2,
                "SynchronousMachine SC data → Generator"
            );
        }
    }

    // --- EquivalentInjection → Generator zero/neg seq ---
    //
    // EquivalentInjection carries r0/x0/r2/x2 in Ohms. These need conversion to
    // per-unit on the system base before storing in the Generator fields.
    let mut sc_ei_count = 0u32;
    for (ei_id, ei) in objects
        .iter()
        .filter(|(_, o)| o.class == "EquivalentInjection")
    {
        let r0_ohm = ei.parse_f64("r0");
        let x0_ohm = ei.parse_f64("x0");
        let r2_ohm = ei.parse_f64("r2");
        let x2_ohm = ei.parse_f64("x2");
        if r0_ohm.is_none() && x0_ohm.is_none() && r2_ohm.is_none() && x2_ohm.is_none() {
            continue;
        }

        let bus_num = idx.terminals(ei_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let Some(bus_num) = bus_num else { continue };

        // Base kV for Ohm→pu conversion.
        let base_kv = network
            .buses
            .iter()
            .find(|b| b.number == bus_num)
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
            .max(1e-3);

        if let Some(gen_indices) = bus_to_gens.get(&bus_num) {
            for &gi in gen_indices {
                let g = &mut network.generators[gi];
                if let Some(v) = r0_ohm {
                    g.fault_data.get_or_insert_with(Default::default).r0_pu =
                        Some(ohm_to_pu(v, base_kv, base_mva));
                }
                if let Some(v) = x0_ohm {
                    g.fault_data.get_or_insert_with(Default::default).x0_pu =
                        Some(ohm_to_pu(v, base_kv, base_mva));
                }
                if let Some(v) = r2_ohm {
                    g.fault_data.get_or_insert_with(Default::default).r2_pu =
                        Some(ohm_to_pu(v, base_kv, base_mva));
                }
                if let Some(v) = x2_ohm {
                    g.fault_data.get_or_insert_with(Default::default).x2_pu =
                        Some(ohm_to_pu(v, base_kv, base_mva));
                }
                sc_ei_count += 1;
            }
            tracing::debug!(
                ei_id,
                bus_num,
                ?r0_ohm,
                ?x0_ohm,
                ?r2_ohm,
                ?x2_ohm,
                "EquivalentInjection SC data → Generator"
            );
        }
    }

    // --- ACLineSegment → Branch zero-sequence impedance ---
    //
    // Zero-sequence attributes (r0, x0, b0ch, g0ch) may appear on the same
    // ACLineSegment object from the SC profile. Convert Ohms/Siemens → pu.
    let mut sc_line_count = 0u32;
    for (line_id, line) in objects.iter().filter(|(_, o)| o.class == "ACLineSegment") {
        let r0_ohm = line.parse_f64("r0");
        let x0_ohm = line.parse_f64("x0");
        let b0_s = line.parse_f64("b0ch");
        let g0_s = line.parse_f64("g0ch");
        if r0_ohm.is_none() && x0_ohm.is_none() && b0_s.is_none() && g0_s.is_none() {
            continue;
        }

        let Some(&br_idx) = eq_to_br.get(line_id.as_str()) else {
            continue;
        };

        // Base kV: use the from-bus base_kv (same as line impedance conversion).
        let from_bus = network.branches[br_idx].from_bus;
        let base_kv = network
            .buses
            .iter()
            .find(|b| b.number == from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
            .max(1e-3);

        let br = &mut network.branches[br_idx];
        if r0_ohm.is_some() || x0_ohm.is_some() || b0_s.is_some() || g0_s.is_some() {
            let zs = br.zero_seq.get_or_insert_with(ZeroSeqData::default);
            if let Some(v) = r0_ohm {
                zs.r0 = ohm_to_pu(v, base_kv, base_mva);
            }
            if let Some(v) = x0_ohm {
                zs.x0 = ohm_to_pu(v, base_kv, base_mva);
            }
            if let Some(v) = b0_s {
                zs.b0 = siemens_to_pu(v, base_kv, base_mva);
            }
            // g0ch → gi0 (from-bus end shunt conductance, zero-seq)
            if let Some(v) = g0_s {
                zs.gi0 = siemens_to_pu(v, base_kv, base_mva) * 0.5;
                zs.gj0 = zs.gi0; // symmetric split for lines
            }
        }
        sc_line_count += 1;
        tracing::debug!(
            line_id,
            br_idx,
            ?r0_ohm,
            ?x0_ohm,
            ?b0_s,
            "ACLineSegment SC data → Branch zero-seq"
        );
    }

    // --- PowerTransformerEnd → Branch zero-seq + transformer_connection ---
    //
    // Each PowerTransformerEnd may carry: r0 (Ohm), x0 (Ohm), connectionKind
    // (WindingConnection enum), grounded (bool), rground (Ohm), xground (Ohm).
    // For 2-winding transformers, we set the branch r0/x0 from the HV winding
    // (end 1) and determine transformer_connection from both windings' connectionKind.
    let mut sc_xfmr_count = 0u32;
    for (xfmr_id, _) in objects
        .iter()
        .filter(|(_, o)| o.class == "PowerTransformer")
    {
        let ends = match idx.pte_by_xfmr.get(xfmr_id.as_str()) {
            Some(e) if e.len() >= 2 => e,
            _ => continue,
        };

        // For 2-winding: use end1 (HV) r0/x0 for the branch.
        // For 3-winding: each star-branch gets its winding's r0/x0 — handled below.
        let end1_id = &ends[0].1;
        let end2_id = &ends[1].1;
        let end1 = match objects.get(end1_id) {
            Some(o) => o,
            None => continue,
        };
        let end2 = match objects.get(end2_id) {
            Some(o) => o,
            None => continue,
        };

        // Determine transformer_connection from connectionKind of both windings.
        let conn1 = parse_winding_connection(end1.get_text("connectionKind"));
        let conn2 = parse_winding_connection(end2.get_text("connectionKind"));
        let grounded1 = end1
            .get_text("grounded")
            .map(|s| s == "true")
            .unwrap_or(false);
        let grounded2 = end2
            .get_text("grounded")
            .map(|s| s == "true")
            .unwrap_or(false);

        let xfmr_conn = derive_transformer_connection(conn1, grounded1, conn2, grounded2);

        // Grounding impedance: rground + j*xground in Ohm → pu.
        let rg1 = end1.parse_f64("rground").unwrap_or(0.0);
        let xg1 = end1.parse_f64("xground").unwrap_or(0.0);
        let rg2 = end2.parse_f64("rground").unwrap_or(0.0);
        let xg2 = end2.parse_f64("xground").unwrap_or(0.0);

        // Zero-sequence impedance from end1 (HV winding).
        let r0_ohm = end1.parse_f64("r0");
        let x0_ohm = end1.parse_f64("x0");

        // Find the branch for this transformer.
        let Some(&br_idx) = eq_to_br.get(xfmr_id.as_str()) else {
            // For 3-winding, multiple branches share the xfmr_id — handled via star-bus
            // pattern. The circuit field contains xfmr_id for the first branch only;
            // for star-bus branches, we need a different lookup (below).
            continue;
        };

        let from_bus = network.branches[br_idx].from_bus;
        let base_kv = network
            .buses
            .iter()
            .find(|b| b.number == from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
            .max(1e-3);

        let br = &mut network.branches[br_idx];
        br.transformer_data
            .get_or_insert_with(TransformerData::default)
            .transformer_connection = xfmr_conn;

        if r0_ohm.is_some() || x0_ohm.is_some() {
            let zs = br.zero_seq.get_or_insert_with(ZeroSeqData::default);
            if let Some(v) = r0_ohm {
                zs.r0 = ohm_to_pu(v, base_kv, base_mva);
            }
            if let Some(v) = x0_ohm {
                zs.x0 = ohm_to_pu(v, base_kv, base_mva);
            }
        }

        // Grounding impedance on primary side.
        if grounded1 && (rg1.abs() > 1e-12 || xg1.abs() > 1e-12) {
            let zs = br.zero_seq.get_or_insert_with(ZeroSeqData::default);
            zs.zn = Some(Complex64::new(
                ohm_to_pu(rg1, base_kv, base_mva),
                ohm_to_pu(xg1, base_kv, base_mva),
            ));
        }

        // Secondary grounding: store in the branch zn field as well if primary has none.
        // For a single-branch model, zn typically represents the grounded-wye side.
        if grounded2
            && (rg2.abs() > 1e-12 || xg2.abs() > 1e-12)
            && br.zero_seq.as_ref().and_then(|z| z.zn).is_none()
        {
            let to_bus = br.to_bus;
            let to_kv = network
                .buses
                .iter()
                .find(|b| b.number == to_bus)
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1e-3);
            let zs = br.zero_seq.get_or_insert_with(ZeroSeqData::default);
            zs.zn = Some(Complex64::new(
                ohm_to_pu(rg2, to_kv, base_mva),
                ohm_to_pu(xg2, to_kv, base_mva),
            ));
        }

        sc_xfmr_count += 1;
        tracing::debug!(
            xfmr_id,
            br_idx,
            ?xfmr_conn,
            ?r0_ohm,
            ?x0_ohm,
            rg1,
            xg1,
            "PowerTransformerEnd SC data → Branch"
        );
    }

    // --- TransformerStarImpedance → Branch zero-seq ---
    //
    // TransformerStarImpedance provides r0/x0 in Ohm and links to a TransformerEnd
    // via starImpedanceEnd. We trace: TSI → TransformerEnd → PowerTransformer → branch.
    let mut sc_tsi_count = 0u32;
    for (tsi_id, tsi) in objects
        .iter()
        .filter(|(_, o)| o.class == "TransformerStarImpedance")
    {
        let r0_ohm = tsi.parse_f64("r0");
        let x0_ohm = tsi.parse_f64("x0");
        if r0_ohm.is_none() && x0_ohm.is_none() {
            continue;
        }

        // Link TSI → TransformerEnd → PowerTransformer.
        let Some(te_id) = tsi
            .get_ref("TransformerEnd")
            .or_else(|| tsi.get_ref("starImpedanceEnd"))
        else {
            continue;
        };
        let Some(te) = objects.get(te_id) else {
            continue;
        };
        let Some(xfmr_id) = te.get_ref("PowerTransformer") else {
            continue;
        };
        let Some(&br_idx) = eq_to_br.get(xfmr_id) else {
            continue;
        };

        let from_bus = network.branches[br_idx].from_bus;
        let base_kv = network
            .buses
            .iter()
            .find(|b| b.number == from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
            .max(1e-3);

        let br = &mut network.branches[br_idx];
        if r0_ohm.is_some() || x0_ohm.is_some() {
            let zs = br.zero_seq.get_or_insert_with(ZeroSeqData::default);
            if let Some(v) = r0_ohm {
                zs.r0 = ohm_to_pu(v, base_kv, base_mva);
            }
            if let Some(v) = x0_ohm {
                zs.x0 = ohm_to_pu(v, base_kv, base_mva);
            }
        }
        sc_tsi_count += 1;
        tracing::debug!(
            tsi_id,
            xfmr_id,
            br_idx,
            ?r0_ohm,
            ?x0_ohm,
            "TransformerStarImpedance SC data → Branch zero-seq"
        );
    }

    // --- Summary ---
    let total = sc_gen_count + sc_ei_count + sc_line_count + sc_xfmr_count + sc_tsi_count;
    if total > 0 {
        tracing::info!(
            generators = sc_gen_count,
            equiv_injections = sc_ei_count,
            lines = sc_line_count,
            transformers = sc_xfmr_count,
            star_impedances = sc_tsi_count,
            "CGMES Short Circuit profile data wired into Network"
        );
    }
}

/// Parse a CIM WindingConnection URI local-name into a simplified enum.
///
/// CIM WindingConnection values (IEC 61970-301):
/// - `Y` / `Yn` → Wye (grounded determined by `grounded` attribute)
/// - `D` → Delta
/// - `Z` / `Zn` → Zigzag
/// - `A` → Auto
fn parse_winding_connection(kind: Option<&str>) -> WindingKind {
    match kind {
        Some(s) => {
            let local = s.rsplit('.').next().unwrap_or(s);
            let local = local.rsplit('#').next().unwrap_or(local);
            match local {
                "D" | "delta" | "Delta" => WindingKind::Delta,
                "Y" | "wye" | "Wye" => WindingKind::Wye,
                "Yn" | "wyeGrounded" | "WyeGrounded" | "wyeN" => WindingKind::WyeGrounded,
                "Z" | "zigzag" | "Zigzag" => WindingKind::Zigzag,
                "Zn" | "zigzagGrounded" | "ZigzagGrounded" | "zigzagN" => {
                    WindingKind::ZigzagGrounded
                }
                "A" | "auto" | "Auto" | "autotransformer" => WindingKind::Auto,
                _ => {
                    tracing::debug!(
                        kind = s,
                        "Unknown WindingConnection kind; defaulting to Wye"
                    );
                    WindingKind::Wye
                }
            }
        }
        None => WindingKind::Wye,
    }
}

/// Simplified winding kind for transformer connection derivation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum WindingKind {
    Wye,
    WyeGrounded,
    Delta,
    Zigzag,
    ZigzagGrounded,
    Auto,
}

/// Derive the `TransformerConnection` from the two windings' connection kinds
/// and grounding status.
fn derive_transformer_connection(
    kind1: WindingKind,
    grounded1: bool,
    kind2: WindingKind,
    grounded2: bool,
) -> TransformerConnection {
    let is_gnd_wye1 = kind1 == WindingKind::WyeGrounded
        || (kind1 == WindingKind::Wye && grounded1)
        || kind1 == WindingKind::ZigzagGrounded
        || (kind1 == WindingKind::Auto && grounded1);
    let is_delta1 = kind1 == WindingKind::Delta;
    let is_gnd_wye2 = kind2 == WindingKind::WyeGrounded
        || (kind2 == WindingKind::Wye && grounded2)
        || kind2 == WindingKind::ZigzagGrounded
        || (kind2 == WindingKind::Auto && grounded2);
    let is_delta2 = kind2 == WindingKind::Delta;

    match (is_gnd_wye1, is_delta1, is_gnd_wye2, is_delta2) {
        (true, _, true, _) => TransformerConnection::WyeGWyeG,
        (true, _, _, true) => TransformerConnection::WyeGDelta,
        (_, true, true, _) => TransformerConnection::DeltaWyeG,
        (_, true, _, true) => TransformerConnection::DeltaDelta,
        (true, _, _, _) => TransformerConnection::WyeGWye, // grounded primary, ungrounded secondary
        (_, _, true, _) => TransformerConnection::WyeGWye, // ungrounded primary, grounded secondary (conservative)
        _ => TransformerConnection::DeltaDelta,            // no grounded wye on either side
    }
}
