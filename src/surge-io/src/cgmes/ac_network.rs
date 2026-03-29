// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES AC network builder: `build_network`.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::power_injection::PowerInjectionKind;
use surge_network::network::{
    Branch, BranchOpfControl, BranchType, Bus, BusType, CgmesDanglingLineSource, FixedShunt,
    Generator, PowerInjection, ShuntType, TransformerData,
};

use super::CgmesError;
use super::helpers::{ohm_to_pu, ptc_table_angle, rtc_table_ratio, siemens_to_pu};
use super::indices::CgmesIndices;
use super::types::ObjMap;

pub(crate) fn build_network(
    objects: &ObjMap,
    idx: &mut CgmesIndices,
) -> Result<Network, CgmesError> {
    let base_mva = 100.0_f64;
    let mut network = Network::new("cgmes_network");
    network.base_mva = base_mva;

    if idx.tn_ids.is_empty() {
        return Err(CgmesError::NoTopology);
    }

    // --- Build buses from TopologicalNodes ---
    for (i, tn_id) in idx.tn_ids.iter().enumerate() {
        let bus_num = (i + 1) as u32;
        idx.tn_bus.insert(tn_id.clone(), bus_num);

        let tn_obj = objects.get(tn_id);

        // base_kv: TN.BaseVoltage (TP), or fallback VL via ConnectivityNodeContainer
        let base_kv = tn_obj
            .map(|o| {
                if let Some(bv_id) = o.get_ref("BaseVoltage") {
                    return idx.bv_kv(bv_id);
                }
                // ConnectivityNodeContainer → VoltageLevel → BaseVoltage
                if let Some(vl_id) = o.get_ref("ConnectivityNodeContainer")
                    && let Some(bv_id) = idx.vl_bv.get(vl_id)
                {
                    return idx.bv_kv(bv_id);
                }
                // No BaseVoltage resolved — will fall back to 1.0 kV.  Log a warning
                // because this affects slack bus selection (highest-kV heuristic) and
                // per-unit impedance conversion.  Common cause: VoltageLevel is in the
                // TP profile but not EQ, or BaseVoltage link is broken.
                tracing::warn!(
                    tn_id,
                    "TopologicalNode: could not resolve BaseVoltage — defaulting to 1.0 kV. \
                     Check that VoltageLevel/BaseVoltage objects are present in EQ profile."
                );
                1.0
            })
            .unwrap_or(1.0);

        let name = tn_obj
            .and_then(|o| o.get_text("name"))
            .unwrap_or(tn_id)
            .to_string();

        // Initial V from SvVoltage (kV → pu)
        let (v_kv, angle_deg) = idx.sv_voltage.get(tn_id).cloned().unwrap_or((None, None));
        let vm = match v_kv {
            Some(v_kv) if v_kv > 0.0 && base_kv > 0.0 => v_kv / base_kv,
            Some(_) | None => 1.0,
        };
        let va = angle_deg.unwrap_or(0.0).to_radians();

        let mut bus = Bus::new(bus_num, BusType::PQ, base_kv);
        bus.name = name;
        bus.voltage_magnitude_pu = vm;
        bus.voltage_angle_rad = va;
        // Apply VoltageLimit bounds (kV → pu) from OperationalLimitSet.
        if base_kv > 1e-3
            && let Some(&(vmin_kv, vmax_kv)) = idx.tn_voltage_limits.get(tn_id)
        {
            if vmin_kv > 1e-9 {
                bus.voltage_min_pu = vmin_kv / base_kv;
            }
            if vmax_kv > 1e-9 {
                bus.voltage_max_pu = vmax_kv / base_kv;
            }
        }
        network.buses.push(bus);
    }

    // Register redirected TN mRIDs in tn_bus so that equipment connected to
    // duplicate (boundary) nodes finds the correct canonical bus number.
    for (dup_id, canonical_id) in &idx.tn_redirect {
        if let Some(&bus_num) = idx.tn_bus.get(canonical_id.as_str()) {
            idx.tn_bus.insert(dup_id.clone(), bus_num);
        }
    }

    // Pre-built index: bus number → position in network.buses.
    // Replaces O(n) Vec::iter().find() with O(1) HashMap lookup throughout
    // the rest of build_network.  Updated when star buses are added for
    // 3-winding transformers.
    let mut bus_num_to_idx: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    // --- ACLineSegment → line branches ---
    let line_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "ACLineSegment")
        .map(|(k, _)| k.clone())
        .collect();

    for line_id in &line_ids {
        // Skip equipment whose terminal is disconnected in the SSH scenario.
        if idx.disconnected_eq.contains(line_id.as_str()) {
            continue;
        }
        // Wave 24: Cut — if any Cut attached to this segment is open, skip the branch.
        // An open Cut splits the segment at its point; in the bus-branch model we remove
        // the branch entirely (conservative approach: both halves treated as de-energised).
        if idx.cut_open_lines.contains(line_id.as_str()) {
            tracing::debug!(line_id, "ACLineSegment skipped: open Cut found (Wave 24)");
            continue;
        }
        let obj = &objects[line_id];
        let terms = idx.terminals(line_id);
        if terms.len() < 2 {
            continue;
        }

        let tn1 = match idx.terminal_tn(objects, &terms[0]) {
            Some(t) => t.to_string(),
            None => continue,
        };
        let tn2 = match idx.terminal_tn(objects, &terms[1]) {
            Some(t) => t.to_string(),
            None => continue,
        };
        let from = match idx.tn_bus(&tn1) {
            Some(n) => n,
            None => continue,
        };
        let to = match idx.tn_bus(&tn2) {
            Some(n) => n,
            None => continue,
        };
        // Both terminals in the same TopologicalNode — closed switch or bus coupler
        // within a single electrical bus (node-breaker model). Skip it.
        if from == to {
            continue;
        }

        // base_kv: prefer ConductingEquipment.BaseVoltage on the line.
        // Only fall back to from-bus kV when resolve_base_kv returns the default 1.0
        // (meaning no BaseVoltage ref was found). Using .max() would corrupt impedances
        // for lines whose rated kV is lower than their connected bus kV.
        let resolved_kv = idx.resolve_base_kv(obj);
        let base_kv = if resolved_kv > 1.0 {
            resolved_kv
        } else {
            bus_num_to_idx
                .get(&from)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0)
        };

        // CGMES provides line impedance via two patterns:
        //   (1) Direct: ACLineSegment.r / .x (total Ohms) — preferred.
        //   (2) Derived: ACLineSegment.length × PerLengthSequenceImpedance.r1/x1/b1ch —
        //       used as fallback when direct r/x are zero (some exporters omit totals).
        //
        // Per CGMES IEC 61970-301: PerLengthSequenceImpedance.r1 is in Ω/km (positive-seq),
        // b1ch is in S/km (positive-seq half-line charging susceptance per km).
        let r_direct = obj.parse_f64("r").unwrap_or(0.0);
        let x_direct = obj.parse_f64("x").unwrap_or(0.0);
        let b_direct = obj.parse_f64("bch").unwrap_or(0.0);
        let g_direct = obj.parse_f64("gch").unwrap_or(0.0);

        // PerLengthSequenceImpedance fallback: used when direct values are absent/zero.
        let (r_ohm, x_ohm_raw, b_s, g_s) = if r_direct == 0.0 && x_direct == 0.0 {
            if let Some((plsi_id, length_km)) = idx.line_per_length_imp.get(line_id.as_str()) {
                if let Some(plsi) = objects.get(plsi_id.as_str()) {
                    let r1 = plsi.parse_f64("r").unwrap_or(0.0); // Ω/km, positive-seq
                    let x1 = plsi.parse_f64("x").unwrap_or(0.0); // Ω/km, positive-seq
                    let b1 = plsi.parse_f64("bch").unwrap_or(0.0); // S/km, half-line charging
                    let g1 = plsi.parse_f64("gch").unwrap_or(0.0); // S/km, half-line conductance
                    tracing::debug!(
                        line_id,
                        plsi_id,
                        length_km,
                        r1,
                        x1,
                        "ACLineSegment: using PerLengthSequenceImpedance fallback"
                    );
                    (
                        r1 * length_km,
                        x1 * length_km,
                        b1 * length_km,
                        g1 * length_km,
                    )
                } else {
                    (r_direct, x_direct, b_direct, g_direct)
                }
            } else {
                (r_direct, x_direct, b_direct, g_direct)
            }
        } else {
            (r_direct, x_direct, b_direct, g_direct)
        };

        // Allow negative x (series capacitors). Only clamp when |x| is near zero
        // to avoid branch admittance singularities; preserve sign for capacitors.
        let x_ohm = if x_ohm_raw.abs() < 1e-9 {
            1e-9
        } else {
            x_ohm_raw
        };

        // Cross-voltage-level ACLineSegment: when an ACLineSegment connects buses at
        // significantly different nominal voltages (a pypowsybl export artifact where some
        // transformer-like branches are exported as lines), OpenLoadFlow builds the correct
        // mixed-base Ybus by normalising impedances to the TO-bus base voltage and applying
        // an implicit turns-ratio tap = V_nom_to / V_nom_from on the from-bus terminal.
        // Without this correction the to-bus diagonal Y_tt is scaled by (V_from/V_to)^2
        // relative to the physically correct value, causing large voltage errors.
        let from_bus_kv = bus_num_to_idx
            .get(&from)
            .and_then(|&i| network.buses.get(i))
            .map(|b| b.base_kv)
            .unwrap_or(base_kv);
        let to_bus_kv = bus_num_to_idx
            .get(&to)
            .and_then(|&i| network.buses.get(i))
            .map(|b| b.base_kv)
            .unwrap_or(from_bus_kv);
        let implicit_tap =
            if from_bus_kv > 0.0 && to_bus_kv > 0.0 && (from_bus_kv / to_bus_kv - 1.0).abs() > 0.02
            {
                to_bus_kv / from_bus_kv
            } else {
                1.0
            };
        // Use the to-bus base for impedance normalisation when an implicit tap is applied.
        let z_base_kv = if implicit_tap != 1.0 {
            to_bus_kv
        } else {
            base_kv
        };

        let r_pu = ohm_to_pu(r_ohm, z_base_kv, base_mva);
        // Preserve negative x for series capacitors modeled as ACLineSegments.
        // Only clamp when |x_pu| is near zero to avoid admittance singularity.
        let x_pu_raw = ohm_to_pu(x_ohm, z_base_kv, base_mva);
        let x_pu = if x_pu_raw.abs() < 1e-6 {
            if x_pu_raw < 0.0 { -1e-6 } else { 1e-6 }
        } else {
            x_pu_raw
        };
        let b_pu = siemens_to_pu(b_s, z_base_kv, base_mva);
        let g_pu = siemens_to_pu(g_s, z_base_kv, base_mva);

        // Thermal limits: PATL → rate_a (continuous), TATL → rate_c (emergency)
        let rate_a = idx
            .eq_thermal_mva
            .get(line_id.as_str())
            .copied()
            .unwrap_or(0.0);
        let rate_c = idx
            .eq_thermal_mva_emergency
            .get(line_id.as_str())
            .copied()
            .unwrap_or(0.0);

        let mut br = Branch::new_line(from, to, r_pu, x_pu, b_pu);
        br.rating_a_mva = rate_a;
        br.rating_c_mva = rate_c;
        br.circuit = line_id.clone(); // store CGMES equipment mRID for conditional limit lookup
        br.g_pi = g_pu; // line charging conductance from CGMES ACLineSegment.gch
        if implicit_tap != 1.0 {
            br.tap = implicit_tap;
        }
        // Wave 17: informational limits stored per CIM spec
        if let Some(&t) = idx.eq_oil_temp_limit_c.get(line_id.as_str()) {
            br.transformer_data
                .get_or_insert_with(TransformerData::default)
                .oil_temp_limit_c = Some(t);
        }
        if let Some(&t) = idx.eq_winding_temp_limit_c.get(line_id.as_str()) {
            br.transformer_data
                .get_or_insert_with(TransformerData::default)
                .winding_temp_limit_c = Some(t);
        }
        if let Some(&z) = idx.eq_impedance_limit_ohm.get(line_id.as_str()) {
            br.transformer_data
                .get_or_insert_with(TransformerData::default)
                .impedance_limit_ohm = Some(z);
        }

        // Wave 24: Clamp — split the line into multiple branches at each clamp point.
        // Each Clamp introduces a T-tap: an intermediate bus with a terminal that
        // connects to other equipment. We split the total r/x/b proportionally.
        if let Some(clamps) = idx.clamp_by_line.get(line_id.as_str()) {
            // For each clamp (sorted by frac), split the remaining segment.
            // clamps is already sorted by frac (ascending).
            let mut prev_frac = 0.0_f64;
            let mut prev_bus = from;
            let mut did_split = false;
            for (frac, clamp_tn_id) in clamps {
                let clamp_bus = match idx.tn_bus(clamp_tn_id) {
                    Some(n) => n,
                    None => continue, // clamp TN not resolved → skip this clamp
                };
                let seg_frac = frac - prev_frac; // fraction of total for this sub-segment
                if seg_frac <= 0.0 || seg_frac > 1.0 {
                    continue;
                }
                let mut seg = Branch::new_line(
                    prev_bus,
                    clamp_bus,
                    r_pu * seg_frac,
                    x_pu * seg_frac,
                    b_pu * seg_frac,
                );
                seg.rating_a_mva = br.rating_a_mva;
                seg.rating_c_mva = br.rating_c_mva;
                network.branches.push(seg);
                prev_frac = *frac;
                prev_bus = clamp_bus;
                did_split = true;
            }
            if did_split {
                // Final segment: from last clamp to the `to` bus.
                let remain = 1.0 - prev_frac;
                if remain > 1e-9 {
                    let mut seg =
                        Branch::new_line(prev_bus, to, r_pu * remain, x_pu * remain, b_pu * remain);
                    seg.rating_a_mva = br.rating_a_mva;
                    seg.rating_c_mva = br.rating_c_mva;
                    network.branches.push(seg);
                }
                tracing::debug!(
                    line_id,
                    n_clamps = clamps.len(),
                    "ACLineSegment split into {} sub-branches by Clamp(s)",
                    clamps.len() + 1
                );
                continue; // `br` already replaced by segments above
            }
        }
        network.branches.push(br);
    }

    // --- SeriesCompensator → series branch (reactor or capacitor) ---
    //
    // CGMES IEC 61970-301 §14: SeriesCompensator represents a series reactor
    // (positive x) or series capacitor (negative x) connected between two buses.
    // Fields: r (Ω), x (Ω) at the BaseVoltage base. No shunt charging.
    // Series capacitors have negative x — we allow it (same guard as ACLineSegment).
    let sc_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "SeriesCompensator")
        .map(|(k, _)| k.clone())
        .collect();

    for sc_id in &sc_ids {
        if idx.disconnected_eq.contains(sc_id.as_str()) {
            continue;
        }
        let obj = &objects[sc_id];
        let terms = idx.terminals(sc_id);
        if terms.len() < 2 {
            continue;
        }
        let tn1 = match idx.terminal_tn(objects, &terms[0]) {
            Some(t) => t.to_string(),
            None => continue,
        };
        let tn2 = match idx.terminal_tn(objects, &terms[1]) {
            Some(t) => t.to_string(),
            None => continue,
        };
        let from = match idx.tn_bus(&tn1) {
            Some(n) => n,
            None => continue,
        };
        let to = match idx.tn_bus(&tn2) {
            Some(n) => n,
            None => continue,
        };
        if from == to {
            continue;
        }

        let resolved_kv = idx.resolve_base_kv(obj);
        let base_kv = if resolved_kv > 1.0 {
            resolved_kv
        } else {
            bus_num_to_idx
                .get(&from)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0)
        };

        let r_ohm = obj.parse_f64("r").unwrap_or(0.0);
        let x_ohm_raw = obj.parse_f64("x").unwrap_or(1e-6);
        let x_ohm = if x_ohm_raw.abs() < 1e-9 {
            1e-9
        } else {
            x_ohm_raw
        };

        let r_pu = ohm_to_pu(r_ohm, base_kv, base_mva);
        let x_pu = ohm_to_pu(x_ohm, base_kv, base_mva);

        let rate_a = idx
            .eq_thermal_mva
            .get(sc_id.as_str())
            .copied()
            .unwrap_or(0.0);
        let rate_c = idx
            .eq_thermal_mva_emergency
            .get(sc_id.as_str())
            .copied()
            .unwrap_or(0.0);

        let mut br = Branch::new_line(from, to, r_pu, x_pu, 0.0);
        br.rating_a_mva = rate_a;
        br.rating_c_mva = rate_c;
        br.circuit = sc_id.clone();
        br.branch_type = BranchType::SeriesCapacitor;
        network.branches.push(br);
        tracing::debug!(
            sc_id,
            from,
            to,
            r_pu,
            x_pu,
            "SeriesCompensator added as branch"
        );
    }

    // --- EquivalentBranch → equivalent network branch ---
    //
    // CGMES IEC 61970-301 §38: EquivalentBranch represents a condensed external
    // network as a Thevenin-equivalent two-terminal branch. Fields: positiveR12 (Ω),
    // positiveX12 (Ω), positiveR21 (Ω), positiveX21 (Ω) — the two directions may
    // differ slightly (unsymmetric equivalents). For steady-state AC power flow we
    // use the average of R12/R21 and X12/X21 (symmetric approximation).
    let eb_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "EquivalentBranch")
        .map(|(k, _)| k.clone())
        .collect();

    for eb_id in &eb_ids {
        if idx.disconnected_eq.contains(eb_id.as_str()) {
            continue;
        }
        let obj = &objects[eb_id];
        let terms = idx.terminals(eb_id);
        if terms.len() < 2 {
            continue;
        }
        let tn1 = match idx.terminal_tn(objects, &terms[0]) {
            Some(t) => t.to_string(),
            None => continue,
        };
        let tn2 = match idx.terminal_tn(objects, &terms[1]) {
            Some(t) => t.to_string(),
            None => continue,
        };
        let from = match idx.tn_bus(&tn1) {
            Some(n) => n,
            None => continue,
        };
        let to = match idx.tn_bus(&tn2) {
            Some(n) => n,
            None => continue,
        };
        if from == to {
            continue;
        }

        let resolved_kv = idx.resolve_base_kv(obj);
        let base_kv = if resolved_kv > 1.0 {
            resolved_kv
        } else {
            bus_num_to_idx
                .get(&from)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0)
        };

        // Use average of the two direction impedances for symmetric approximation.
        let r12 = obj.parse_f64("positiveR12").unwrap_or(0.0);
        let r21 = obj.parse_f64("positiveR21").unwrap_or(r12);
        let x12 = obj.parse_f64("positiveX12").unwrap_or(1e-6);
        let x21 = obj.parse_f64("positiveX21").unwrap_or(x12);
        let r_ohm = (r12 + r21) * 0.5;
        let x_ohm_raw = (x12 + x21) * 0.5;
        let x_ohm = if x_ohm_raw.abs() < 1e-9 {
            1e-9
        } else {
            x_ohm_raw
        };

        let r_pu = ohm_to_pu(r_ohm, base_kv, base_mva);
        let x_pu = ohm_to_pu(x_ohm, base_kv, base_mva);

        let rate_a = idx
            .eq_thermal_mva
            .get(eb_id.as_str())
            .copied()
            .unwrap_or(0.0);
        let rate_c = idx
            .eq_thermal_mva_emergency
            .get(eb_id.as_str())
            .copied()
            .unwrap_or(0.0);

        let mut br = Branch::new_line(from, to, r_pu, x_pu, 0.0);
        br.rating_a_mva = rate_a;
        br.rating_c_mva = rate_c;
        br.circuit = eb_id.clone();
        network.branches.push(br);
        tracing::debug!(
            eb_id,
            from,
            to,
            r_pu,
            x_pu,
            "EquivalentBranch added as branch"
        );
    }

    // --- DanglingLine → shunt + P/Q injection at boundary bus ---
    //
    // CGMES IEC 61970-301 §14 / ENTSO-E common grid model: DanglingLine represents
    // a transmission line with ONE physical terminal and one open boundary terminal.
    // In single-IGM (unmerged) mode the external network is not explicitly modelled;
    // the SSH profile provides p/q (the power exchange at the boundary).
    //
    // Modelling: apply the line's shunt charging admittance (b/g) to the connected
    // bus and treat SSH p/q as a PQ injection (same sign convention as
    // EquivalentInjection: p>0 = injection into network → subtract from bus pd).
    // The series impedance (r/x) has no second terminal so no branch is created.
    let dl_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "DanglingLine")
        .map(|(k, _)| k.clone())
        .collect();

    for dl_id in &dl_ids {
        if idx.disconnected_eq.contains(dl_id.as_str()) {
            continue;
        }
        let obj = &objects[dl_id];

        // Resolve the single physical terminal → bus.
        let bus_num = idx.terminals(dl_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let bus_num = match bus_num {
            Some(n) => n,
            None => continue,
        };

        // base_kv for per-unit conversion.
        let resolved_kv = idx.resolve_base_kv(obj);
        let base_kv = if resolved_kv > 1.0 {
            resolved_kv
        } else {
            bus_num_to_idx
                .get(&bus_num)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0)
        };

        let p = obj.parse_f64("p").unwrap_or(0.0);
        let q = obj.parse_f64("q").unwrap_or(0.0);
        let b_s = obj.parse_f64("b").unwrap_or(0.0); // Siemens (total line B)
        let g_s = obj.parse_f64("g").unwrap_or(0.0);
        network.cim.cgmes_roundtrip.dangling_lines.insert(
            dl_id.clone(),
            CgmesDanglingLineSource {
                mrid: dl_id.clone(),
                name: obj.get_text("name").map(str::to_string),
                bus: bus_num,
                p_mw: p,
                q_mvar: q,
                in_service: true,
                r_ohm: obj.parse_f64("r"),
                x_ohm: obj.parse_f64("x"),
                g_s,
                b_s,
            },
        );

        // Shunt charging admittance from the line half-π model.
        let b_mvar = b_s * base_kv * base_kv;
        let g_mw = g_s * base_kv * base_kv;
        if let Some(&i) = bus_num_to_idx.get(&bus_num) {
            network.buses[i].shunt_susceptance_mvar += b_mvar;
            network.buses[i].shunt_conductance_mw += g_mw;
        }
        if g_mw.abs() > 1e-9 || b_mvar.abs() > 1e-9 {
            network.fixed_shunts.push(FixedShunt {
                bus: bus_num,
                id: dl_id.clone(),
                shunt_type: if b_mvar < 0.0 {
                    ShuntType::Reactor
                } else {
                    ShuntType::Capacitor
                },
                g_mw,
                b_mvar,
                in_service: true,
                rated_kv: Some(base_kv),
                rated_mvar: Some(b_mvar.abs()),
            });
        }

        // SSH p/q: power exchange at boundary (p>0 = injection into network).
        if p.abs() > 1e-9 || q.abs() > 1e-9 {
            network.power_injections.push(PowerInjection {
                bus: bus_num,
                id: dl_id.clone(),
                kind: PowerInjectionKind::Boundary,
                active_power_injection_mw: p,
                reactive_power_injection_mvar: q,
                in_service: true,
            });
        }
        tracing::debug!(
            dl_id,
            bus_num,
            b_mvar,
            p,
            q,
            "DanglingLine: shunt + P/Q applied"
        );
    }

    // --- SvTapStep: actual tap position from SV profile (overrides EQ nominal step) ---
    let sv_tap_step: HashMap<String, f64> = objects
        .iter()
        .filter(|(_, o)| o.class == "SvTapStep")
        .filter_map(|(_, sv)| {
            let tc_id = sv.get_ref("TapChanger")?.to_string();
            let pos = sv.parse_f64("position")?;
            Some((tc_id, pos))
        })
        .collect();

    // --- PhaseTapChangerTable: table_id → sorted Vec<(step, angle_deg)> ---
    let mut ptc_tables: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    for (_, pt) in objects
        .iter()
        .filter(|(_, o)| o.class == "PhaseTapChangerTablePoint")
    {
        if let Some(table_id) = pt.get_ref("PhaseTapChangerTable") {
            let step = pt.parse_f64("step").unwrap_or(0.0);
            let angle = pt.parse_f64("angle").unwrap_or(0.0);
            ptc_tables
                .entry(table_id.to_string())
                .or_default()
                .push((step, angle));
        }
    }
    for pts in ptc_tables.values_mut() {
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    // --- RatioTapChangerTable: table_id → sorted Vec<(step, ratio)> ---
    let mut rtc_tables: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    for (_, pt) in objects
        .iter()
        .filter(|(_, o)| o.class == "RatioTapChangerTablePoint")
    {
        if let Some(table_id) = pt.get_ref("RatioTapChangerTable") {
            let step = pt.parse_f64("step").unwrap_or(0.0);
            let ratio = pt.parse_f64("ratio").unwrap_or(1.0);
            rtc_tables
                .entry(table_id.to_string())
                .or_default()
                .push((step, ratio));
        }
    }
    for pts in rtc_tables.values_mut() {
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    // --- PowerTransformer → transformer branches ---
    let xfmr_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "PowerTransformer")
        .map(|(k, _)| k.clone())
        .collect();

    // Counter for fictitious star-bus numbers (3-winding expansion).
    // Star buses are numbered above all real TopologicalNode buses.
    let mut next_star_bus: u32 = network.buses.iter().map(|b| b.number).max().unwrap_or(0);

    for xfmr_id in &xfmr_ids {
        if idx.disconnected_eq.contains(xfmr_id.as_str()) {
            continue;
        }
        // PowerTransformerEnd objects sorted by endNumber — pre-indexed in O(1).
        let ends = idx
            .pte_by_xfmr
            .get(xfmr_id.as_str())
            .cloned()
            .unwrap_or_default();
        if ends.len() < 2 {
            continue;
        }
        if ends.len() > 3 {
            tracing::warn!(
                "PowerTransformer {xfmr_id} has {} windings; ≥4 winding models \
                 are not supported. Skipping.",
                ends.len()
            );
            continue;
        }

        // --- 3-winding transformer: star-bus (Γ-equivalent) expansion ---
        if ends.len() == 3 {
            // Each winding becomes a two-winding branch from the winding bus to
            // a fictitious internal star bus.  Impedances are in Ω at each
            // winding's ratedU base and are converted to p.u. on the 100 MVA
            // system base.  Magnetizing admittance (b, g) is applied only to
            // the winding-1 branch, consistent with the PSS/E convention.
            let end1_obj = &objects[&ends[0].1];
            let end2_obj = &objects[&ends[1].1];
            let end3_obj = &objects[&ends[2].1];

            // Resolve terminal → TopologicalNode → bus for each winding.
            macro_rules! resolve_winding_bus {
                ($end_obj:expr, $fallback_idx:expr) => {{
                    let tid = $end_obj.get_ref("Terminal").map(|s| s.to_string());
                    match tid
                        .as_deref()
                        .and_then(|tid| idx.terminal_tn(objects, tid))
                        .and_then(|tn| idx.tn_bus(tn))
                    {
                        Some(n) => n,
                        None => {
                            let terms = idx.terminals(xfmr_id);
                            match terms
                                .get($fallback_idx)
                                .and_then(|tid| idx.terminal_tn(objects, tid))
                                .and_then(|tn| idx.tn_bus(tn))
                            {
                                Some(n) => n,
                                None => {
                                    tracing::warn!(
                                        xfmr_id,
                                        winding = $fallback_idx + 1,
                                        "3-winding transformer: could not resolve bus; skipping"
                                    );
                                    continue;
                                }
                            }
                        }
                    }
                }};
            }

            let bus1 = resolve_winding_bus!(end1_obj, 0);
            let bus2 = resolve_winding_bus!(end2_obj, 1);
            let bus3 = resolve_winding_bus!(end3_obj, 2);

            // Base voltages at each winding bus (for off-nominal tap calculation).
            let bkv1 = bus_num_to_idx
                .get(&bus1)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0);
            let bkv2 = bus_num_to_idx
                .get(&bus2)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0);
            let bkv3 = bus_num_to_idx
                .get(&bus3)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0);

            // Rated voltages (kV) — used as impedance base and nominal tap reference.
            let rated_u1 = end1_obj.parse_f64("ratedU").unwrap_or(bkv1).max(1e-3);
            let rated_u2 = end2_obj.parse_f64("ratedU").unwrap_or(bkv2).max(1e-3);
            let rated_u3 = end3_obj.parse_f64("ratedU").unwrap_or(bkv3).max(1e-3);

            // Winding impedances: Ω at each winding's ratedU base → p.u. (100 MVA).
            // Wave 21: If a TransformerMeshImpedance is present for this transformer,
            // use the mesh→star converted r/x values instead of per-winding PowerTransformerEnd.r/x.
            // mesh_imp stores (r1, x1, r2, x2, r3, x3) in Ω after star conversion.
            let (r1_pu, x1_pu, r2_pu, x2_pu, r3_pu, x3_pu) =
                if let Some(&(rm1, xm1, rm2, xm2, rm3, xm3)) = idx.mesh_imp.get(xfmr_id.as_str()) {
                    tracing::debug!(
                        xfmr_id,
                        rm1,
                        xm1,
                        rm2,
                        xm2,
                        rm3,
                        xm3,
                        "3W transformer: using TransformerMeshImpedance star values"
                    );
                    {
                        // Mesh-to-star conversion can produce negative impedances;
                        // preserve sign but guard against zero.
                        let clamp_x = |x: f64, base_kv: f64| {
                            let x_safe = if x.abs() < 1e-9 { 1e-6 } else { x };
                            let pu = ohm_to_pu(x_safe, base_kv, base_mva);
                            if pu.abs() < 1e-6 {
                                if pu < 0.0 { -1e-6 } else { 1e-6 }
                            } else {
                                pu
                            }
                        };
                        (
                            ohm_to_pu(rm1, rated_u1, base_mva),
                            clamp_x(xm1, rated_u1),
                            ohm_to_pu(rm2, rated_u2, base_mva),
                            clamp_x(xm2, rated_u2),
                            ohm_to_pu(rm3, rated_u3, base_mva),
                            clamp_x(xm3, rated_u3),
                        )
                    }
                } else {
                    (
                        ohm_to_pu(end1_obj.parse_f64("r").unwrap_or(0.0), rated_u1, base_mva),
                        ohm_to_pu(
                            end1_obj.parse_f64("x").unwrap_or(1e-6).max(1e-9),
                            rated_u1,
                            base_mva,
                        )
                        .max(1e-6),
                        ohm_to_pu(end2_obj.parse_f64("r").unwrap_or(0.0), rated_u2, base_mva),
                        ohm_to_pu(
                            end2_obj.parse_f64("x").unwrap_or(1e-6).max(1e-9),
                            rated_u2,
                            base_mva,
                        )
                        .max(1e-6),
                        ohm_to_pu(end3_obj.parse_f64("r").unwrap_or(0.0), rated_u3, base_mva),
                        ohm_to_pu(
                            end3_obj.parse_f64("x").unwrap_or(1e-6).max(1e-9),
                            rated_u3,
                            base_mva,
                        )
                        .max(1e-6),
                    )
                };

            // Magnetizing admittance from winding 1 only (PSS/E convention: all shunt
            // admittance placed on the primary winding branch End1→star).
            // Wave 20: TransformerCoreAdmittance overrides PowerTransformerEnd.b/g when present.
            let (g1_s_raw, b1_s_raw) = idx
                .core_admittance_by_end
                .get(ends[0].1.as_str())
                .copied()
                .unwrap_or_else(|| {
                    (
                        end1_obj.parse_f64("g").unwrap_or(0.0),
                        end1_obj.parse_f64("b").unwrap_or(0.0),
                    )
                });
            let b1_pu = siemens_to_pu(b1_s_raw, rated_u1, base_mva);
            let g1_pu = siemens_to_pu(g1_s_raw, rated_u1, base_mva);
            // Warn if End2/End3 carry non-zero b/g that we are discarding.
            let b2_s = end2_obj.parse_f64("b").unwrap_or(0.0);
            let g2_s = end2_obj.parse_f64("g").unwrap_or(0.0);
            let b3_s = end3_obj.parse_f64("b").unwrap_or(0.0);
            let g3_s = end3_obj.parse_f64("g").unwrap_or(0.0);
            if b2_s.abs() > 1e-12 || g2_s.abs() > 1e-12 || b3_s.abs() > 1e-12 || g3_s.abs() > 1e-12
            {
                tracing::warn!(
                    xfmr_id,
                    b2 = b2_s,
                    g2 = g2_s,
                    b3 = b3_s,
                    g3 = g3_s,
                    "3-winding transformer End2/End3 magnetizing admittance discarded (star model uses End1 only)"
                );
            }

            // Off-nominal tap ratios: ratedU_i / base_kv_i.
            let mut tap1 = if bkv1 > 0.0 { rated_u1 / bkv1 } else { 1.0 };
            let mut tap2 = if bkv2 > 0.0 { rated_u2 / bkv2 } else { 1.0 };
            let mut tap3 = if bkv3 > 0.0 { rated_u3 / bkv3 } else { 1.0 };

            // Apply RatioTapChanger for each winding independently.
            let apply_rtc = |end_id: &str, tap: &mut f64| {
                let Some(rtc_id) = idx.rtc_by_end.get(end_id) else {
                    return;
                };
                let Some(rtc_obj) = objects.get(rtc_id.as_str()) else {
                    return;
                };
                // Step lookup priority: (1) SvTapStep.position (SV), (2) TapChanger.step
                // (SSH — merged into object store), (3) TapChanger.neutralStep (EQ fallback).
                // Using neutralStep as the final fallback ensures tap=1.0 when SSH is absent
                // rather than the incorrect 0.0 default.
                let step = sv_tap_step
                    .get(rtc_id.as_str())
                    .copied()
                    .or_else(|| rtc_obj.parse_f64("step"))
                    .unwrap_or_else(|| {
                        rtc_obj.parse_f64("neutralStep").unwrap_or_else(|| {
                            tracing::warn!(
                                rtc_id,
                                "neutralStep missing in 3-winding RTC; defaulting step=0"
                            );
                            0.0
                        })
                    });
                let table_ratio = rtc_obj
                    .get_ref("RatioTapChangerTable")
                    .and_then(|tid| rtc_tables.get(tid))
                    .map(|pts| rtc_table_ratio(pts, step));
                if let Some(ratio) = table_ratio {
                    *tap *= ratio;
                } else {
                    let neutral = rtc_obj.parse_f64("neutralStep").unwrap_or_else(|| {
                        tracing::warn!(
                            rtc_id,
                            "neutralStep missing in 3-winding RTC; defaulting neutral=0"
                        );
                        0.0
                    });
                    let step_pct = rtc_obj
                        .parse_f64("stepVoltageIncrement")
                        .or_else(|| rtc_obj.parse_f64("voltageStepIncrement"))
                        .unwrap_or(0.0);
                    if step_pct.abs() > 0.0 {
                        *tap *= 1.0 + (step - neutral) * step_pct / 100.0;
                    }
                }
            };
            apply_rtc(&ends[0].1, &mut tap1);
            apply_rtc(&ends[1].1, &mut tap2);
            apply_rtc(&ends[2].1, &mut tap3);

            // Extract per-winding discrete tap step data from RTC.
            let extract_rtc_step = |end_id: &str| -> (f64, f64, f64) {
                let Some(rtc_id) = idx.rtc_by_end.get(end_id) else {
                    return (0.0, 0.9, 1.1);
                };
                let Some(rtc_obj) = objects.get(rtc_id.as_str()) else {
                    return (0.0, 0.9, 1.1);
                };
                let step_pct = rtc_obj
                    .parse_f64("stepVoltageIncrement")
                    .or_else(|| rtc_obj.parse_f64("voltageStepIncrement"))
                    .unwrap_or(0.0);
                if step_pct.abs() < 1e-12 {
                    return (0.0, 0.9, 1.1);
                }
                let neutral = rtc_obj.parse_f64("neutralStep").unwrap_or(0.0);
                let low = rtc_obj.parse_f64("lowStep").unwrap_or(0.0);
                let high = rtc_obj.parse_f64("highStep").unwrap_or(0.0);
                let mut t_min = 1.0 + (low - neutral) * step_pct / 100.0;
                let mut t_max = 1.0 + (high - neutral) * step_pct / 100.0;
                if t_min > t_max {
                    std::mem::swap(&mut t_min, &mut t_max);
                }
                (step_pct / 100.0, t_min, t_max)
            };
            let (ts1, tmin1, tmax1) = extract_rtc_step(&ends[0].1);
            let (ts2, tmin2, tmax2) = extract_rtc_step(&ends[1].1);
            let (ts3, tmin3, tmax3) = extract_rtc_step(&ends[2].1);

            // Create the fictitious internal star bus.
            // Use highest winding kV to avoid division-by-zero in downstream fault analysis.
            next_star_bus += 1;
            let star_bus_num = next_star_bus;
            let mut star_bus =
                Bus::new(star_bus_num, BusType::PQ, bkv1.max(bkv2).max(bkv3).max(1.0));
            star_bus.name = format!("STAR_{bus1}_{bus2}_{bus3}");
            bus_num_to_idx.insert(star_bus_num, network.buses.len());
            network.buses.push(star_bus);

            // Branch 1: winding-1 bus → star (carries magnetizing admittance).
            let mut br1 = Branch::new_line(bus1, star_bus_num, r1_pu, x1_pu, 0.0);
            br1.tap = tap1;
            br1.b_mag = b1_pu;
            br1.g_mag = g1_pu;
            br1.branch_type = BranchType::Transformer3W;
            if ts1 > 1e-12 {
                let ctrl = br1
                    .opf_control
                    .get_or_insert_with(BranchOpfControl::default);
                ctrl.tap_step = ts1;
                ctrl.tap_min = tmin1;
                ctrl.tap_max = tmax1;
            }
            network.branches.push(br1);

            // Branch 2: winding-2 bus → star.
            let mut br2 = Branch::new_line(bus2, star_bus_num, r2_pu, x2_pu, 0.0);
            br2.tap = tap2;
            br2.branch_type = BranchType::Transformer3W;
            if ts2 > 1e-12 {
                let ctrl = br2
                    .opf_control
                    .get_or_insert_with(BranchOpfControl::default);
                ctrl.tap_step = ts2;
                ctrl.tap_min = tmin2;
                ctrl.tap_max = tmax2;
            }
            network.branches.push(br2);

            // Branch 3: winding-3 bus → star.
            let mut br3 = Branch::new_line(bus3, star_bus_num, r3_pu, x3_pu, 0.0);
            br3.tap = tap3;
            br3.branch_type = BranchType::Transformer3W;
            if ts3 > 1e-12 {
                let ctrl = br3
                    .opf_control
                    .get_or_insert_with(BranchOpfControl::default);
                ctrl.tap_step = ts3;
                ctrl.tap_min = tmin3;
                ctrl.tap_max = tmax3;
            }
            network.branches.push(br3);

            tracing::debug!(
                bus1,
                bus2,
                bus3,
                star_bus = star_bus_num,
                r1_pu,
                r2_pu,
                r3_pu,
                "3-winding transformer expanded to star topology"
            );
            continue;
        }

        let end1 = &objects[&ends[0].1];
        let end2 = &objects[&ends[1].1];

        // Resolve from-bus (End1) and to-bus (End2) via each End's own Terminal
        // reference rather than the unordered terminal list from the index.
        // TransformerEnd.Terminal is required (1..1) in CGMES 2.4.15; if it is
        // absent we fall back to the generic terminal list (pre-existing behaviour).
        let term1_id = end1.get_ref("Terminal").map(|s| s.to_string());
        let term2_id = end2.get_ref("Terminal").map(|s| s.to_string());

        let tn1 = match term1_id
            .as_deref()
            .and_then(|tid| idx.terminal_tn(objects, tid))
        {
            Some(t) => t.to_string(),
            None => {
                // Fallback: use the unordered terminal list (may mis-order End1/End2)
                let terms = idx.terminals(xfmr_id);
                if terms.len() < 2 {
                    continue;
                }
                match idx.terminal_tn(objects, &terms[0]) {
                    Some(t) => t.to_string(),
                    None => continue,
                }
            }
        };
        let tn2 = match term2_id
            .as_deref()
            .and_then(|tid| idx.terminal_tn(objects, tid))
        {
            Some(t) => t.to_string(),
            None => {
                let terms = idx.terminals(xfmr_id);
                if terms.len() < 2 {
                    continue;
                }
                match idx.terminal_tn(objects, &terms[1]) {
                    Some(t) => t.to_string(),
                    None => continue,
                }
            }
        };
        let from = match idx.tn_bus(&tn1) {
            Some(n) => n,
            None => continue,
        };
        let to = match idx.tn_bus(&tn2) {
            Some(n) => n,
            None => continue,
        };
        // Both windings on the same TopologicalNode → internal bus-coupler; skip.
        if from == to {
            continue;
        }

        // Impedance base: use End1 BaseVoltage.
        // Only fall back to from-bus kV when resolve_base_kv returns the default 1.0.
        let resolved_kv = idx.resolve_base_kv(end1);
        let base_kv = if resolved_kv > 1.0 {
            resolved_kv
        } else {
            network
                .buses
                .iter()
                .find(|b| b.number == from)
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1.0)
        };

        // Nominal tap ratio (MATPOWER convention: tap = (ratedU1/ratedU2) * (base_kv_to/base_kv_from))
        // A nominal transformer (ratedU matches bus base voltages) gives tap = 1.0.
        // Off-nominal: ratedU1 = actual_tap × base_kv_from → tap = actual_tap.
        // Computed first because the turns ratio is needed for End2 impedance referral.
        let rated_u1 = end1.parse_f64("ratedU").unwrap_or(base_kv).max(1e-3);
        let to_base_kv = bus_num_to_idx
            .get(&to)
            .and_then(|&i| network.buses.get(i))
            .map(|b| b.base_kv)
            .unwrap_or(1.0);
        let rated_u2 = end2.parse_f64("ratedU").unwrap_or(to_base_kv);

        // CGMES may split the series impedance between End1 and End2.
        // Refer End2 values to the End1 side using the winding turns ratio squared:
        //   z_total_end1 = z_end1 + z_end2 × (ratedU1 / ratedU2)²
        // When exporters put all impedance in End1 (End2.r/x = 0) this is a no-op.
        let turns_sq = if rated_u2 > 0.0 {
            (rated_u1 / rated_u2).powi(2)
        } else {
            1.0
        };
        let r1_ohm = end1.parse_f64("r").unwrap_or(0.0);
        let x1_ohm = end1.parse_f64("x").unwrap_or(0.0);
        let r2_ohm = end2.parse_f64("r").unwrap_or(0.0);
        let x2_ohm = end2.parse_f64("x").unwrap_or(0.0);
        let r_combined = r1_ohm + r2_ohm * turns_sq;
        let x_combined = x1_ohm + x2_ohm * turns_sq;
        // Pre-decomposed 3-winding star windings: End1 carries negative impedance
        // from the mesh→star transformation, End2 has zero impedance.  Preserve sign.
        // Artifact case: both End1 and End2 contribute, and End2's negative values
        // make the combined impedance negative → use abs() (data error).
        let is_star_winding = r2_ohm.abs() < 1e-12 && x2_ohm.abs() < 1e-12;
        if r_combined < 0.0 || x_combined < 0.0 {
            if is_star_winding {
                tracing::debug!(
                    xfmr_id,
                    r_ohm = r_combined,
                    x_ohm = x_combined,
                    "star-decomposed 3W winding: preserving negative impedance sign"
                );
            } else {
                tracing::debug!(
                    xfmr_id,
                    r_ohm = r_combined,
                    x_ohm = x_combined,
                    "negative combined impedance from End2 contribution; using abs()"
                );
            }
        }
        let r_ohm = if is_star_winding {
            r_combined
        } else {
            r_combined.abs()
        };
        let x_ohm = if x_combined.abs() < 1e-9 {
            1e-6
        } else if is_star_winding {
            x_combined
        } else {
            x_combined.abs()
        };
        // Magnetizing admittance: use TransformerCoreAdmittance if present (Wave 20),
        // otherwise fall back to PowerTransformerEnd.b / .g directly.
        // CGMES: TransformerCoreAdmittance.TransformerEnd ref points to the winding end
        // that carries the shunt admittance (always the primary, end1, by convention).
        let (g_s, b_s) = idx
            .core_admittance_by_end
            .get(ends[0].1.as_str())
            .copied()
            .unwrap_or_else(|| {
                (
                    end1.parse_f64("g").unwrap_or(0.0),
                    end1.parse_f64("b").unwrap_or(0.0),
                )
            });

        // CGMES impedance convention detection (per-transformer):
        //
        // Convention A — MATPOWER-style exports (e.g. case9/case14/case118/ieee14_ppow):
        //   Impedances are stored as z_pu_matpower × (ratedU1²/S_base).  Tap ratio is
        //   encoded in ratedU1 (off-nominal ratedU1 ≠ base_kv).  There is NO
        //   RatioTapChanger object.  Decode with ratedU1 as z-base.
        //
        // Convention B — physical-kV CGMES (e.g. eurostag, real-world networks):
        //   Impedances are physical Ω at system base voltage.  Tap changes are explicit
        //   RatioTapChanger objects.  Decode with base_kv as z-base.
        //
        // Discriminator: presence of a RatioTapChanger on this transformer.
        let has_rtc_on_this_xfmr = ends
            .iter()
            .any(|(_, end_id)| idx.rtc_by_end.contains_key(end_id.as_str()));
        // Convention A (no RTC): CGMES stores impedances in Ω referred to ratedU2.
        // OpenLoadFlow converts to pu using nom_v2 (to_base_kv), not ratedU2.
        // The effective z_base is rated_u1 * (to_base_kv / rated_u2) so that the
        // resulting x_pu matches OLF's: x_ohm*(ratedU2/ratedU1)^2 * base_mva/to_base_kv^2.
        // When ratedU2 == to_base_kv this reduces to rated_u1 (the old formula).
        //
        // MAJ-01: Convention B (with RTC) — IEC 61970-301 §6.4.3.7: PowerTransformerEnd.r/x
        // are always referred to that winding's own ratedU, NOT to the system BaseVoltage.
        // Use rated_u1 as z_base here, matching the no-RTC path.
        let z_base_kv = if has_rtc_on_this_xfmr {
            rated_u1
        } else if rated_u2 > 0.0 && to_base_kv > 0.0 {
            rated_u1 * (to_base_kv / rated_u2)
        } else {
            rated_u1
        };
        let r_pu = ohm_to_pu(r_ohm, z_base_kv, base_mva);
        let x_pu_raw = ohm_to_pu(x_ohm, z_base_kv, base_mva);
        // Prevent zero impedance (short circuit) but preserve negative sign.
        let x_pu = if x_pu_raw.abs() < 1e-6 {
            if x_pu_raw < 0.0 { -1e-6 } else { 1e-6 }
        } else {
            x_pu_raw
        };
        let b_pu = siemens_to_pu(b_s, z_base_kv, base_mva);
        let g_pu = siemens_to_pu(g_s, z_base_kv, base_mva);
        let mut tap = if rated_u1 > 0.0 && rated_u2 > 0.0 && base_kv > 0.0 && to_base_kv > 0.0 {
            (rated_u1 / rated_u2) * (to_base_kv / base_kv)
        } else {
            1.0
        };

        // Discrete step data extracted from RTC/PTC for Branch fields.
        let mut rtc_tap_step_pu = 0.0_f64;
        let mut rtc_tap_min = 0.9_f64;
        let mut rtc_tap_max = 1.1_f64;
        let mut ptc_phase_step_deg = 0.0_f64;
        let mut ptc_phase_min_deg = -30.0_f64;
        let mut ptc_phase_max_deg = 30.0_f64;

        // Apply RatioTapChanger (check both ends)
        // MAJ-02: MATPOWER convention defines `tap` as the End1 (from-bus) ratio.
        // An RTC on End2 contributes the reciprocal: tap /= ratio instead of tap *= ratio.
        for (end_idx, end_id) in [&ends[0].1, &ends[1].1].iter().enumerate() {
            let Some(rtc_id) = idx.rtc_by_end.get(end_id.as_str()) else {
                continue;
            };
            let Some(rtc_obj) = objects.get(rtc_id.as_str()) else {
                // RTC object missing from object store — skip this end and try the other.
                continue;
            };
            // Step lookup priority: (1) SvTapStep.position (SV), (2) TapChanger.step
            // (SSH — merged into object store), (3) TapChanger.neutralStep (EQ fallback).
            //
            // Wave 19 — TapChangerControl.regulating (SSH):
            //   true (default)  → OLTC actively regulates; use SV/SSH/neutral priority.
            //   false           → tap locked; if SvTapStep absent, use neutralStep directly
            //                     (the SSH step should also reflect the locked position,
            //                      but some files omit it for non-regulating changers).
            let tcc_regulating = idx
                .tc_to_tcc
                .get(rtc_id.as_str())
                .and_then(|tcc_id| idx.tcc_params.get(tcc_id.as_str()))
                .map(|(reg, _, mode)| {
                    if !*reg {
                        tracing::debug!(
                            rtc_id,
                            mode,
                            "RTC TapChangerControl.regulating=false: tap locked"
                        );
                    }
                    *reg
                })
                .unwrap_or(true);
            let step = sv_tap_step
                .get(rtc_id.as_str())
                .copied()
                .or_else(|| rtc_obj.parse_f64("step"))
                .or_else(|| {
                    // If locked and no SV/SSH step, fall to neutralStep without warning.
                    if !tcc_regulating {
                        rtc_obj.parse_f64("neutralStep")
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| {
                    rtc_obj.parse_f64("neutralStep").unwrap_or_else(|| {
                        tracing::warn!(
                            rtc_id,
                            "neutralStep missing in 2-winding RTC; defaulting step=0"
                        );
                        0.0
                    })
                });
            // Prefer table lookup (RatioTapChangerTable) over linear formula.
            // Table ratio is a per-unit multiplier applied on top of the nominal tap.
            // MAJ-02: End1 RTC → tap *= ratio; End2 RTC → tap /= ratio (MATPOWER convention).
            let table_ratio = rtc_obj
                .get_ref("RatioTapChangerTable")
                .and_then(|tid| rtc_tables.get(tid))
                .map(|pts| rtc_table_ratio(pts, step));
            if let Some(ratio) = table_ratio {
                if end_idx == 0 {
                    tap *= ratio;
                } else {
                    tap /= ratio;
                }
            } else {
                let neutral = rtc_obj.parse_f64("neutralStep").unwrap_or_else(|| {
                    tracing::warn!(
                        rtc_id,
                        "neutralStep missing in 2-winding RTC; defaulting neutral=0"
                    );
                    0.0
                });
                let step_pct = rtc_obj
                    .parse_f64("stepVoltageIncrement")
                    .or_else(|| rtc_obj.parse_f64("voltageStepIncrement"))
                    .unwrap_or(0.0);
                if step_pct.abs() > 0.0 {
                    let ratio = 1.0 + (step - neutral) * step_pct / 100.0;
                    if end_idx == 0 {
                        tap *= ratio;
                    } else {
                        tap /= ratio;
                    }
                }
            }
            // Compute discrete tap step size from RTC attributes.
            // stepVoltageIncrement is in % per step; convert to p.u.
            let step_pct_for_branch = rtc_obj
                .parse_f64("stepVoltageIncrement")
                .or_else(|| rtc_obj.parse_f64("voltageStepIncrement"))
                .unwrap_or(0.0);
            if step_pct_for_branch.abs() > 1e-12 {
                rtc_tap_step_pu = step_pct_for_branch / 100.0;
                // Derive tap_min/tap_max from lowStep/highStep/neutralStep.
                let low = rtc_obj.parse_f64("lowStep").unwrap_or(0.0);
                let high = rtc_obj.parse_f64("highStep").unwrap_or(0.0);
                let neutral = rtc_obj.parse_f64("neutralStep").unwrap_or(0.0);
                rtc_tap_min = 1.0 + (low - neutral) * step_pct_for_branch / 100.0;
                rtc_tap_max = 1.0 + (high - neutral) * step_pct_for_branch / 100.0;
                if rtc_tap_min > rtc_tap_max {
                    std::mem::swap(&mut rtc_tap_min, &mut rtc_tap_max);
                }
            }
            break;
        }

        // PhaseTapChanger (phase-shifting transformers)
        // MAJ-03: MATPOWER convention defines phase shift from End1 perspective.
        // A PTC on End2 must negate the angle: shift = -X instead of +X.
        let mut shift = 0.0_f64;
        for (ptc_end_idx, end_id) in [&ends[0].1, &ends[1].1].iter().enumerate() {
            let Some(ptc_id) = idx.ptc_by_end.get(end_id.as_str()) else {
                continue;
            };
            let Some(ptc_obj) = objects.get(ptc_id.as_str()) else {
                // PTC object missing from object store — skip this end and try the other.
                continue;
            };
            // Step lookup priority: (1) SvTapStep.position (SV), (2) TapChanger.step
            // (SSH — merged into object store), (3) TapChanger.neutralStep (EQ fallback).
            // Wave 19: if TapChangerControl.regulating=false, tap is locked.
            let ptc_tcc_regulating = idx
                .tc_to_tcc
                .get(ptc_id.as_str())
                .and_then(|tcc_id| idx.tcc_params.get(tcc_id.as_str()))
                .map(|(reg, _, mode)| {
                    if !*reg {
                        tracing::debug!(
                            ptc_id,
                            mode,
                            "PTC TapChangerControl.regulating=false: phase locked"
                        );
                    }
                    *reg
                })
                .unwrap_or(true);
            let step = sv_tap_step
                .get(ptc_id.as_str())
                .copied()
                .or_else(|| ptc_obj.parse_f64("step"))
                .or_else(|| {
                    if !ptc_tcc_regulating {
                        ptc_obj.parse_f64("neutralStep")
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| {
                    ptc_obj.parse_f64("neutralStep").unwrap_or_else(|| {
                        tracing::warn!(ptc_id, "neutralStep missing in PTC; defaulting step=0");
                        0.0
                    })
                });
            let neutral = ptc_obj.parse_f64("neutralStep").unwrap_or_else(|| {
                tracing::warn!(ptc_id, "neutralStep missing in PTC; defaulting neutral=0");
                0.0
            });

            if ptc_obj.class == "PhaseTapChangerTabular"
                || ptc_obj.class == "PhaseTapChangerNonLinear"
            {
                // Tabular/NonLinear PTC: angle from interpolated PhaseTapChangerTablePoint.
                // CGMES 3.0 PhaseTapChangerNonLinear uses the same PhaseTapChangerTable
                // structure as PhaseTapChangerTabular (table with step→angle points).
                if let Some(table_id) = ptc_obj.get_ref("PhaseTapChangerTable")
                    && let Some(pts) = ptc_tables.get(table_id)
                {
                    shift = ptc_table_angle(pts, step);
                }
            } else if ptc_obj.class == "PhaseTapChangerAsymmetrical" {
                // Asymmetrical PTC: base angle (windingConnectionAngle) plus per-step increment.
                // CGMES IEC 61970-301: angle = windingConnectionAngle + (step-neutralStep)*stepPhaseShiftIncrement
                let step_deg = ptc_obj.parse_f64("stepPhaseShiftIncrement").unwrap_or(0.0);
                let winding_angle = ptc_obj.parse_f64("windingConnectionAngle").unwrap_or(0.0);
                shift = winding_angle + (step - neutral) * step_deg;
            } else if ptc_obj.class == "PhaseTapChangerLinear" {
                // Linear PTC: simultaneous phase shift AND voltage ratio adjustment.
                // CGMES: stepPhaseShiftIncrement (deg/step) + stepVoltageIncrement (% per step).
                let step_deg = ptc_obj.parse_f64("stepPhaseShiftIncrement").unwrap_or(0.0);
                let step_volt = ptc_obj.parse_f64("stepVoltageIncrement").unwrap_or(0.0);
                shift = (step - neutral) * step_deg;
                if step_volt.abs() > 1e-12 {
                    tap *= 1.0 + (step - neutral) * step_volt / 100.0;
                }
            } else {
                // PhaseTapChangerSymmetrical or generic PhaseTapChanger:
                // angle = (step - neutralStep) × stepPhaseShiftIncrement [degrees]
                let step_deg = ptc_obj.parse_f64("stepPhaseShiftIncrement").unwrap_or(0.0);
                shift = (step - neutral) * step_deg;
            }
            // MAJ-03: Negate shift when the PTC is physically on End2 (index 1).
            // MATPOWER convention measures shift from the End1 (from-bus) perspective.
            if ptc_end_idx == 1 {
                shift = -shift;
            }
            // Extract discrete phase step size from PTC for Branch.phase_step_rad.
            // stepPhaseShiftIncrement is in degrees/step for all PTC subtypes.
            let ptc_step = ptc_obj.parse_f64("stepPhaseShiftIncrement").unwrap_or(0.0);
            if ptc_step.abs() > 1e-12 {
                ptc_phase_step_deg = ptc_step.abs();
                let low = ptc_obj.parse_f64("lowStep").unwrap_or(0.0);
                let high = ptc_obj.parse_f64("highStep").unwrap_or(0.0);
                ptc_phase_min_deg = (low - neutral) * ptc_step;
                ptc_phase_max_deg = (high - neutral) * ptc_step;
                if ptc_phase_min_deg > ptc_phase_max_deg {
                    std::mem::swap(&mut ptc_phase_min_deg, &mut ptc_phase_max_deg);
                }
            }
            break;
        }

        // Wave 18: phaseAngleClock — IEC vector group clock position.
        //
        // CGMES IEC 61970-301: PowerTransformerEnd.phaseAngleClock is an integer 0–11
        // representing the IEC vector group clock notation (e.g. Dyn11, YNd1).
        // Each clock unit = 30°. The phase shift contribution is:
        //   Δshift = (clock_end2 - clock_end1) × 30°
        //
        // Convention: positive clock = lagging (end2 lags end1 by clock × 30°).
        // This matches IEC 60076-1: Dyn11 → end2 leads by 330° = −30°; Dyn1 → end2 lags 30°.
        // CGMES attribute: PowerTransformerEnd.phaseAngleClock (integer, optional).
        let clock1 = end1.parse_f64("phaseAngleClock").unwrap_or(0.0) as i32;
        let clock2 = end2.parse_f64("phaseAngleClock").unwrap_or(0.0) as i32;
        let clock_shift_deg = ((clock2 - clock1) as f64) * 30.0;
        if clock_shift_deg.abs() > 1e-9 {
            shift += clock_shift_deg;
            tracing::debug!(
                xfmr_id,
                clock1,
                clock2,
                clock_shift_deg,
                "phaseAngleClock contribution added to shift"
            );
        }

        let rate_a = idx
            .eq_thermal_mva
            .get(xfmr_id.as_str())
            .copied()
            .unwrap_or(0.0);
        let rate_c = idx
            .eq_thermal_mva_emergency
            .get(xfmr_id.as_str())
            .copied()
            .unwrap_or(0.0);

        let mut br = Branch::new_line(from, to, r_pu, x_pu, 0.0);
        br.tap = tap;
        br.phase_shift_rad = shift.to_radians();
        // CGMES PowerTransformerEnd.b/g (or TransformerCoreAdmittance.b/g, Wave 20) →
        // transformer magnetizing admittance (PSS/E MAG2/MAG1). Stored in b_mag/g_mag,
        // not in the π-model line charging (br.b = 0 for transformers).
        if b_pu.abs() > 1e-12 {
            br.b_mag = b_pu;
        }
        if g_pu.abs() > 1e-12 {
            br.g_mag = g_pu;
        }
        br.rating_a_mva = rate_a;
        br.rating_c_mva = rate_c;
        br.circuit = xfmr_id.clone();
        // Apply discrete step sizes extracted from RTC/PTC.
        if rtc_tap_step_pu > 1e-12 {
            let ctrl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
            ctrl.tap_step = rtc_tap_step_pu;
            ctrl.tap_min = rtc_tap_min;
            ctrl.tap_max = rtc_tap_max;
        }
        if ptc_phase_step_deg > 1e-12 {
            let ctrl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
            ctrl.phase_step_rad = ptc_phase_step_deg.to_radians();
            ctrl.phase_min_rad = ptc_phase_min_deg.to_radians();
            ctrl.phase_max_rad = ptc_phase_max_deg.to_radians();
        }
        // Wave 17: PhaseTapChangerLimit → phase angle operational bounds on this transformer.
        // Applied only when the transformer has a phase shifter (shift ≠ 0 is not required;
        // the limit is on the equipment, not the current operating state).
        // NOTE: This overrides PTC-derived bounds when explicit limits are present.
        if let Some(&(phase_min_deg, phase_max_deg)) = idx.eq_ptc_phase_limits.get(xfmr_id.as_str())
        {
            let ctrl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
            ctrl.phase_min_rad = phase_min_deg.to_radians();
            ctrl.phase_max_rad = phase_max_deg.to_radians();
        }
        // Wave 17: informational temperature and impedance limits stored per CIM spec
        if let Some(&t) = idx.eq_oil_temp_limit_c.get(xfmr_id.as_str()) {
            br.transformer_data
                .get_or_insert_with(TransformerData::default)
                .oil_temp_limit_c = Some(t);
        }
        if let Some(&t) = idx.eq_winding_temp_limit_c.get(xfmr_id.as_str()) {
            br.transformer_data
                .get_or_insert_with(TransformerData::default)
                .winding_temp_limit_c = Some(t);
        }
        if let Some(&z) = idx.eq_impedance_limit_ohm.get(xfmr_id.as_str()) {
            br.transformer_data
                .get_or_insert_with(TransformerData::default)
                .impedance_limit_ohm = Some(z);
        }
        br.branch_type = BranchType::Transformer;
        network.branches.push(br);
    }

    // --- LinearShuntCompensator → bus shunt susceptance ---
    let shunt_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "LinearShuntCompensator" || o.class == "ShuntCompensator")
        .map(|(k, _)| k.clone())
        .collect();

    for sh_id in &shunt_ids {
        let obj = &objects[sh_id];
        let terms = idx.terminals(sh_id);
        tracing::debug!(sh_id, n_terms = terms.len(), "shunt compensator found");
        let bus_num = terms
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                // Fallback: EquipmentContainer → VoltageLevel → TN
                obj.get_ref("EquipmentContainer").and_then(|vl_id| {
                    idx.tn_ids
                        .iter()
                        .find(|tn_id| {
                            objects
                                .get(tn_id.as_str())
                                .and_then(|o| o.get_ref("ConnectivityNodeContainer"))
                                .map(|c| c == vl_id)
                                .unwrap_or(false)
                        })
                        .and_then(|tn_id| idx.tn_bus(tn_id))
                })
            });

        if let Some(bus_num) = bus_num {
            let base_kv = idx.resolve_base_kv(obj).max(
                bus_num_to_idx
                    .get(&bus_num)
                    .and_then(|&i| network.buses.get(i))
                    .map(|b| b.base_kv)
                    .unwrap_or(1.0),
            );

            // SSH provides actual operating section count via `sections`; EQ provides design
            // value via `normalSections`. Prefer SSH value when present and positive.
            let sections = obj
                .parse_f64("sections")
                .filter(|&v| v > 0.0)
                .unwrap_or_else(|| obj.parse_f64("normalSections").unwrap_or(1.0));
            let b_per_section = obj.parse_f64("bPerSection").unwrap_or(0.0);
            let g_per_section = obj.parse_f64("gPerSection").unwrap_or(0.0);

            let b_s = b_per_section * sections;
            let g_s = g_per_section * sections;

            // Bus.shunt_susceptance_mvar/gs store MVAr/MW at V=1 pu (same convention as MATPOWER Bs/Gs).
            // The Y-bus assembly divides by base_mva to convert to per-unit.
            // Formula: b_mvar = B[S] * base_kv[kV]^2  (S·kV² = MVAr at 1 pu)
            // Do NOT use siemens_to_pu (which already divides by base_mva) — that
            // would cause a double-division by base_mva in the Y-bus.
            let b_mvar = b_s * base_kv * base_kv;
            let g_mw = g_s * base_kv * base_kv;

            tracing::debug!(sh_id, bus_num, b_mvar, g_mw, "shunt applied to bus");

            if let Some(&i) = bus_num_to_idx.get(&bus_num) {
                network.buses[i].shunt_susceptance_mvar += b_mvar;
                network.buses[i].shunt_conductance_mw += g_mw;
            }
            network.fixed_shunts.push(FixedShunt {
                bus: bus_num,
                id: sh_id.clone(),
                shunt_type: if b_mvar < 0.0 {
                    ShuntType::Reactor
                } else {
                    ShuntType::Capacitor
                },
                g_mw,
                b_mvar,
                in_service: true,
                rated_kv: Some(base_kv),
                rated_mvar: Some(b_mvar.abs()),
            });
        } else {
            tracing::warn!(sh_id, "shunt compensator: could not resolve bus number");
        }
    }

    // --- NonlinearShuntCompensator → bus shunt susceptance via tabular B(sections) ---
    //
    // CGMES IEC 61970-301: NonlinearShuntCompensator defines a piece-wise constant B-V
    // curve via NonlinearShuntCompensatorPoint entries.  Each point has sectionNumber
    // and the total b (S) and g (S) at that section count.  We read the SSH `sections`
    // value (or normalSections from EQ) and look up the closest row in the table.
    {
        // Build: nlsc_mrid → Vec<(section_number, b_total_S, g_total_S)>
        let mut nlsc_pts: HashMap<String, Vec<(i64, f64, f64)>> = HashMap::new();
        for (_, obj) in objects
            .iter()
            .filter(|(_, o)| o.class == "NonlinearShuntCompensatorPoint")
        {
            let Some(nlsc_ref) = obj.get_ref("NonlinearShuntCompensator") else {
                continue;
            };
            let sec_n = obj.parse_f64("sectionNumber").unwrap_or(0.0) as i64;
            let b = obj.parse_f64("b").unwrap_or(0.0);
            let g = obj.parse_f64("g").unwrap_or(0.0);
            nlsc_pts
                .entry(nlsc_ref.to_string())
                .or_default()
                .push((sec_n, b, g));
        }
        let nlsc_ids: Vec<String> = objects
            .iter()
            .filter(|(_, o)| o.class == "NonlinearShuntCompensator")
            .map(|(k, _)| k.clone())
            .collect();

        for nlsc_id in &nlsc_ids {
            if idx.disconnected_eq.contains(nlsc_id.as_str()) {
                continue;
            }
            let obj = &objects[nlsc_id];
            let terms = idx.terminals(nlsc_id);
            let bus_num = terms.iter().find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            });
            let Some(bus_num) = bus_num else {
                tracing::warn!(
                    nlsc_id,
                    "NonlinearShuntCompensator: could not resolve bus number"
                );
                continue;
            };
            let base_kv = idx.resolve_base_kv(obj).max(
                bus_num_to_idx
                    .get(&bus_num)
                    .and_then(|&i| network.buses.get(i))
                    .map(|b| b.base_kv)
                    .unwrap_or(1.0),
            );
            // SSH `sections` count (or EQ normalSections as fallback).
            let sections = obj
                .parse_f64("sections")
                .filter(|&v| v > 0.0)
                .unwrap_or_else(|| obj.parse_f64("normalSections").unwrap_or(1.0))
                as i64;

            // Look up the tabular point at this section count; fall back to the nearest.
            let (b_s, g_s) = if let Some(pts) = nlsc_pts.get(nlsc_id.as_str()) {
                pts.iter()
                    .find(|&&(n, _, _)| n == sections)
                    .or_else(|| pts.iter().min_by_key(|&&(n, _, _)| (n - sections).abs()))
                    .map(|&(_, b, g)| (b, g))
                    .unwrap_or((0.0, 0.0))
            } else {
                tracing::warn!(
                    nlsc_id,
                    "NonlinearShuntCompensator: no NonlinearShuntCompensatorPoint entries found; skipping"
                );
                continue;
            };

            let b_mvar = b_s * base_kv * base_kv;
            let g_mw = g_s * base_kv * base_kv;
            tracing::debug!(
                nlsc_id,
                bus_num,
                b_mvar,
                g_mw,
                sections,
                "NonlinearShuntCompensator applied to bus"
            );
            if let Some(&i) = bus_num_to_idx.get(&bus_num) {
                network.buses[i].shunt_susceptance_mvar += b_mvar;
                network.buses[i].shunt_conductance_mw += g_mw;
            }
            network.fixed_shunts.push(FixedShunt {
                bus: bus_num,
                id: nlsc_id.clone(),
                shunt_type: if b_mvar < 0.0 {
                    ShuntType::Reactor
                } else {
                    ShuntType::Capacitor
                },
                g_mw,
                b_mvar,
                in_service: true,
                rated_kv: Some(base_kv),
                rated_mvar: Some(b_mvar.abs()),
            });
        }
    }

    // --- StaticVarCompensator → voltage-controlling generator or fixed Q injection ---
    //
    // CGMES IEC 61970-301 §26: StaticVarCompensator is a dynamic reactive power
    // compensation device. The SSH attribute `sVCControlMode` determines the model:
    //
    //   voltageControl  → SVC regulates bus voltage; model as a generator (PV bus)
    //                     with Q limits from bMin/bMax (EQ, in Siemens):
    //                       Qmax =  bMax × V² (MVAr, capacitive, positive)
    //                       Qmin =  bMin × V² (MVAr, may be negative if bMin < 0)
    //                     Voltage setpoint from RegulatingControl.targetValue (kV).
    //   reactiveControl → SVC holds SSH q (MVAr) as a fixed Q injection (PQ bus).
    //   off / absent    → Same as reactiveControl.
    let svc_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "StaticVarCompensator")
        .map(|(k, _)| k.clone())
        .collect();

    for svc_id in &svc_ids {
        if idx.disconnected_eq.contains(svc_id.as_str()) {
            continue;
        }
        let obj = &objects[svc_id];
        let bus_num = idx
            .terminals(svc_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                obj.get_ref("EquipmentContainer").and_then(|vl_id| {
                    idx.tn_ids
                        .iter()
                        .find(|tn_id| {
                            objects
                                .get(tn_id.as_str())
                                .and_then(|o| o.get_ref("ConnectivityNodeContainer"))
                                .map(|c| c == vl_id)
                                .unwrap_or(false)
                        })
                        .and_then(|tn_id| idx.tn_bus(tn_id))
                })
            });

        let bus_num = match bus_num {
            Some(n) => n,
            None => {
                tracing::warn!(svc_id, "StaticVarCompensator: could not resolve bus number");
                continue;
            }
        };

        // Determine control mode from SSH sVCControlMode attribute.
        let ctrl_mode = obj.get_ref("sVCControlMode").unwrap_or("");
        let is_voltage_ctrl = ctrl_mode.ends_with("voltageControl");

        if is_voltage_ctrl {
            // Voltage-regulating SVC: model as a generator (PV bus).
            // Q limits from EQ bMin/bMax (Siemens) scaled by base_kv².
            let base_kv = bus_num_to_idx
                .get(&bus_num)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1e-3);
            let b_max = obj.parse_f64("bMax").unwrap_or(0.0);
            let b_min = obj.parse_f64("bMin").unwrap_or(0.0);
            // bMax ≥ 0 (capacitive limit), bMin ≤ 0 (inductive limit) per IEC.
            // Qmax = bMax × V², Qmin = bMin × V² (both in MVAr).
            let qmax = if b_max.abs() > 1e-12 {
                b_max * base_kv * base_kv
            } else {
                9999.0
            };
            let qmin = if b_min.abs() > 1e-12 {
                b_min * base_kv * base_kv
            } else {
                -9999.0
            };
            let vs = idx.gen_vs(objects, obj, base_kv).unwrap_or(1.0);
            let q_ssh = obj.parse_f64("q").unwrap_or(0.0);
            let mut svc_gen = Generator::new(bus_num, 0.0, vs); // SVC: Pg = 0
            svc_gen.q = q_ssh; // warm start from SSH operating point
            svc_gen.qmax = qmax;
            svc_gen.qmin = qmin;
            svc_gen.pmax = 0.0;
            svc_gen.pmin = 0.0;
            svc_gen.machine_base_mva = base_mva;
            network.generators.push(svc_gen);
            tracing::debug!(
                svc_id,
                bus_num,
                qmin,
                qmax,
                vs,
                "StaticVarCompensator voltage control → generator (PV bus)"
            );
        } else {
            // reactiveControl or off: fixed Q injection at bus.
            // SSH q: positive = capacitive = injection (reduces net bus qd).
            let q = obj.parse_f64("q").unwrap_or(0.0);
            if q.abs() > 1e-9 {
                network.power_injections.push(PowerInjection {
                    bus: bus_num,
                    id: svc_id.clone(),
                    kind: PowerInjectionKind::Compensator,
                    active_power_injection_mw: 0.0,
                    reactive_power_injection_mvar: q,
                    in_service: true,
                });
            }
            tracing::debug!(
                svc_id,
                bus_num,
                q,
                "StaticVarCompensator q injected into bus"
            );
        }
    }

    // --- EquivalentShunt → bus shunt admittance (condensed network equivalent) ---
    //
    // CGMES IEC 61970-301 §38: EquivalentShunt represents a shunt admittance
    // equivalent of an external network. Fields: b (S), g (S) — same units as
    // LinearShuntCompensator.bPerSection. Same conversion: MVAr = B × kV².
    let eqsh_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "EquivalentShunt")
        .map(|(k, _)| k.clone())
        .collect();

    for eqsh_id in &eqsh_ids {
        if idx.disconnected_eq.contains(eqsh_id.as_str()) {
            continue;
        }
        let obj = &objects[eqsh_id];
        let bus_num = idx
            .terminals(eqsh_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                obj.get_ref("EquipmentContainer").and_then(|vl_id| {
                    idx.tn_ids
                        .iter()
                        .find(|tn_id| {
                            objects
                                .get(tn_id.as_str())
                                .and_then(|o| o.get_ref("ConnectivityNodeContainer"))
                                .map(|c| c == vl_id)
                                .unwrap_or(false)
                        })
                        .and_then(|tn_id| idx.tn_bus(tn_id))
                })
            });

        let bus_num = match bus_num {
            Some(n) => n,
            None => {
                tracing::warn!(eqsh_id, "EquivalentShunt: could not resolve bus number");
                continue;
            }
        };

        let base_kv = idx.resolve_base_kv(obj).max(
            bus_num_to_idx
                .get(&bus_num)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0),
        );

        let b_s = obj.parse_f64("b").unwrap_or(0.0);
        let g_s = obj.parse_f64("g").unwrap_or(0.0);
        let b_mvar = b_s * base_kv * base_kv;
        let g_mw = g_s * base_kv * base_kv;

        if let Some(&i) = bus_num_to_idx.get(&bus_num) {
            network.buses[i].shunt_susceptance_mvar += b_mvar;
            network.buses[i].shunt_conductance_mw += g_mw;
        }
        network.fixed_shunts.push(FixedShunt {
            bus: bus_num,
            id: eqsh_id.clone(),
            shunt_type: if b_mvar < 0.0 {
                ShuntType::Reactor
            } else {
                ShuntType::Capacitor
            },
            g_mw,
            b_mvar,
            in_service: true,
            rated_kv: Some(base_kv),
            rated_mvar: Some(b_mvar.abs()),
        });
        tracing::debug!(
            eqsh_id,
            bus_num,
            b_mvar,
            g_mw,
            "EquivalentShunt applied to bus"
        );
    }
    Ok(network)
}
