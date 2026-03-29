// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::collections::HashMap;
use std::collections::HashSet;

use super::types::{CimObj, ObjMap};

pub(crate) fn parse_optional_f64(obj: &CimObj, key: &str) -> Option<f64> {
    let text = obj.get_text(key)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<f64>() {
        Ok(value) if value.is_finite() => Some(value),
        Ok(_) => {
            tracing::warn!(
                class = obj.class.as_str(),
                field = key,
                value = trimmed,
                "CGMES numeric field parsed to non-finite value; ignoring"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                class = obj.class.as_str(),
                field = key,
                value = trimmed,
                "CGMES numeric field is malformed; ignoring"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Stage 2 — Build lookup indices from object store
// ---------------------------------------------------------------------------

pub(crate) struct CgmesIndices {
    /// equipment_id → list of terminal IDs sorted by sequenceNumber
    pub(crate) eq_terminals: HashMap<String, Vec<String>>,
    /// BaseVoltage mRID → kV
    pub(crate) bv_kv: HashMap<String, f64>,
    /// VoltageLevel mRID → BaseVoltage mRID
    pub(crate) vl_bv: HashMap<String, String>,
    /// TopologicalNode mRID → (v_kv, angle_deg) from SvVoltage.
    /// Each component is optional so missing/malformed values are preserved.
    pub(crate) sv_voltage: HashMap<String, (Option<f64>, Option<f64>)>,
    /// TopologicalNode mRID → bus number (filled during bus building)
    pub(crate) tn_bus: HashMap<String, u32>,
    /// RegulatingControl mRID → targetValue_kv (from SSH, may be 0 if absent)
    pub(crate) rc_target_kv: HashMap<String, f64>,
    /// RegulatingControl mRID → mode URI local-name (e.g. "voltage", "reactivePower").
    /// Used to guard against non-voltage targetValues being treated as kV setpoints.
    pub(crate) rc_mode: HashMap<String, String>,
    /// Sorted list of TopologicalNode mRIDs (deterministic ordering)
    pub(crate) tn_ids: Vec<String>,
    /// ConductingEquipment mRID → normal/PATL thermal rating in MVA (rate_a).
    /// Populated from ApparentPowerLimit, CurrentLimit, and ActivePowerLimit objects
    /// that are tagged PATL/normal, or from the minimum of untyped limits.
    pub(crate) eq_thermal_mva: HashMap<String, f64>,
    /// ConductingEquipment mRID → emergency/TATL thermal rating in MVA (rate_c).
    /// Populated from limits tagged TATL/emergency in OperationalLimitType.
    pub(crate) eq_thermal_mva_emergency: HashMap<String, f64>,
    /// TopologicalNode mRID → (vmin_kv, vmax_kv) from VoltageLimit objects.
    /// Applied to Bus.voltage_min_pu/vmax (in pu) during bus building.
    pub(crate) tn_voltage_limits: HashMap<String, (f64, f64)>,
    /// TransformerEnd mRID → RatioTapChanger mRID (O(1) tap-changer lookup)
    pub(crate) rtc_by_end: HashMap<String, String>,
    /// TransformerEnd mRID → PhaseTapChanger* mRID (O(1) phase-shifter lookup)
    pub(crate) ptc_by_end: HashMap<String, String>,
    /// PowerTransformer mRID → sorted Vec<(endNumber, end_mRID)> (O(1) winding lookup)
    pub(crate) pte_by_xfmr: HashMap<String, Vec<(u32, String)>>,
    /// Duplicate TN mRID → canonical TN mRID (boundary-node deduplication).
    /// Populated during build(), applied in build_network() after bus creation.
    pub(crate) tn_redirect: HashMap<String, String>,
    /// Equipment mRIDs that are isolated via SSH Terminal.connected=false.
    /// Any equipment with at least one disconnected terminal is in this set.
    /// Such equipment is skipped during branch/generator/load building.
    pub(crate) disconnected_eq: std::collections::HashSet<String>,
    /// ReactiveCapabilityCurve mRID → sorted Vec<(p_mw, qmin_mvar, qmax_mvar)>.
    /// p_mw is in IEC sign convention (negative = generating), matching SSH p.
    /// y1value = Qmin, y2value = Qmax from CurveData objects.
    pub(crate) rcc_points: HashMap<String, Vec<(f64, f64, f64)>>,
    /// SynchronousMachine mRID → ReactiveCapabilityCurve mRID.
    /// Built from SynchronousMachine.InitialReactiveCapabilityCurve reference.
    pub(crate) sm_rcc: HashMap<String, String>,
    /// ConductingEquipment mRID → (p_mw, q_mvar) from SvInjection (SV profile).
    /// Each component is optional so missing/malformed values are preserved.
    /// Used as a fallback for load p/q when SSH p/q are absent (e.g. IGM-only files).
    pub(crate) sv_injections: HashMap<String, (Option<f64>, Option<f64>)>,
    /// ConductingEquipment mRID → (qmin_mvar, qmax_mvar) from ReactivePowerLimit objects.
    /// Direction "low" → qmin, "high" / "absoluteValue" → qmax.
    /// Used as Q-limit fallback for generators/converters when SM.maxQ/minQ absent.
    pub(crate) eq_reactive_limits: HashMap<String, (f64, f64)>,
    /// ConductingEquipment mRID → (phase_min_rad, phase_max_rad) from PhaseTapChangerLimit.
    /// Direction "low" → phase_min_rad, "high" → phase_max_rad.
    /// Applied to Branch.phase_min_rad/phase_max_rad for phase-shifting transformers.
    pub(crate) eq_ptc_phase_limits: HashMap<String, (f64, f64)>,
    /// ConductingEquipment mRID → oil temperature limit in °C (OilTemperatureLimit).
    /// Informational — stored per CIM spec, not converted to MVA rating.
    pub(crate) eq_oil_temp_limit_c: HashMap<String, f64>,
    /// ConductingEquipment mRID → winding temperature limit in °C (WindingTemperatureLimit).
    /// Informational — stored per CIM spec, not converted to MVA rating.
    pub(crate) eq_winding_temp_limit_c: HashMap<String, f64>,
    /// SvStatus mRIDs with malformed or missing inService values.
    /// Stored separately so present-but-invalid state can be distinguished from
    /// present-but-valid state at the boundary.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) sv_status_invalid: HashSet<String>,
    /// ConductingEquipment mRID → impedance limit in Ohms (ImpedanceLimit).
    /// Informational — stored per CIM spec, not applied to admittance model.
    pub(crate) eq_impedance_limit_ohm: HashMap<String, f64>,
    /// ACLineSegment mRID → (PerLengthSequenceImpedance mRID, length_km).
    /// Used as impedance fallback when ACLineSegment.r / .x are zero or absent.
    pub(crate) line_per_length_imp: HashMap<String, (String, f64)>,
    /// HvdcLine mRID → (activePowerSetpoint_mw, resistance_ohm, rated_udc_kv).
    /// Each component is optional so missing/malformed values are preserved.
    /// activePowerSetpoint used as P fallback for VsConverter/CsConverter.
    pub(crate) hvdc_line_params: HashMap<String, (Option<f64>, Option<f64>, Option<f64>)>,
    /// ACDCConverter mRID → HvdcLine mRID (for P-setpoint fallback).
    /// Built by traversing ACDCConverterDCTerminal → DCNode → DCTerminal of HvdcLine.
    pub(crate) conv_hvdc: HashMap<String, String>,
    /// ACDCConverter mRID → DCNode mRID (for DC network building).
    /// Built from ACDCConverterDCTerminal: ACDCConverter → DCNode.
    pub(crate) conv_to_dcnode: HashMap<String, String>,
    /// DCNode mRID → Vec<(equipment_mRID, class)> (DCLineSegment, HvdcLine, etc.).
    /// Built from DCTerminal: DCConductingEquipment → DCNode.
    pub(crate) dcnode_to_eq: HashMap<String, Vec<(String, String)>>,
    /// TapChanger mRID → TapChangerControl mRID.
    /// Built from TapChanger.TapChangerControl reference on RTC/PTC objects.
    pub(crate) tc_to_tcc: HashMap<String, String>,
    /// TapChangerControl mRID → (regulating: bool, deadband_kv: f64, regulating_mode: String).
    /// regulating (SSH): true = OLTC actively regulates, false = locked at current step.
    /// deadband_kv (EQ): voltage tolerance band in kV for the controlled quantity.
    /// regulating_mode (EQ): URI suffix — "voltage", "reactivePower", "activePower", etc.
    pub(crate) tcc_params: HashMap<String, (bool, f64, String)>,
    /// PowerTransformerEnd mRID → (g_siemens, b_siemens) from TransformerCoreAdmittance.
    /// Wave 20: When present, overrides PowerTransformerEnd.g/b for magnetizing admittance.
    /// TransformerCoreAdmittance.TransformerEnd links the TCA object to its winding end.
    pub(crate) core_admittance_by_end: HashMap<String, (f64, f64)>,
    /// PowerTransformer mRID → winding star impedances (r1, x1, r2, x2, r3, x3) in Ω.
    /// Wave 21: Populated only for 3-winding transformers that carry TransformerMeshImpedance
    /// objects (one per winding pair). Values are the result of the mesh→star conversion:
    ///   z1 = (z12 + z13 - z23) / 2,  z2 = (z12 + z23 - z13) / 2,  z3 = (z13 + z23 - z12) / 2.
    pub(crate) mesh_imp: HashMap<String, (f64, f64, f64, f64, f64, f64)>,
    /// Wave 24: ACLineSegment mRIDs with at least one open Cut attached.
    /// A Cut with open=true (SSH) splits the segment; we conservatively skip the branch
    /// (treating it as fully disconnected at the cut point).
    pub(crate) cut_open_lines: std::collections::HashSet<String>,
    /// Wave 24: ACLineSegment mRID → sorted Vec<(frac, clamp_tn_id)> from Clamp objects.
    /// frac = Clamp.lengthFromTerminal1 / ACLineSegment.length ∈ [0, 1].
    /// clamp_tn_id is the TopologicalNode mRID of the Clamp's Terminal.
    /// Multiple Clamps are sorted by frac for sequential segment splitting.
    pub(crate) clamp_by_line: HashMap<String, Vec<(f64, String)>>,
    /// ConductingEquipment mRID → Vec<(condition_id, limit_mva, is_emergency)>.
    /// From ConditionalLimit objects: condition-dependent thermal ratings.
    /// Wired into Network.conditional_limits during branch building.
    pub(crate) conditional_thermal_limits: HashMap<String, Vec<(String, f64, bool)>>,
}

impl CgmesIndices {
    pub(crate) fn build(objects: &ObjMap) -> Self {
        // --- equipment → terminals index ---
        let mut eq_terminals: HashMap<String, Vec<(u32, String)>> = HashMap::new();
        for (tid, t) in objects.iter().filter(|(_, o)| o.class == "Terminal") {
            if let Some(eq_id) = t.get_ref("ConductingEquipment") {
                let seq = t
                    .get_text("sequenceNumber")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                eq_terminals
                    .entry(eq_id.to_string())
                    .or_default()
                    .push((seq, tid.clone()));
            }
        }
        let eq_terminals: HashMap<String, Vec<String>> = eq_terminals
            .into_iter()
            .map(|(eq, mut terms)| {
                terms.sort_by_key(|(seq, _)| *seq);
                (eq, terms.into_iter().map(|(_, t)| t).collect())
            })
            .collect();

        // --- BaseVoltage kV ---
        let bv_kv: HashMap<String, f64> = objects
            .iter()
            .filter(|(_, o)| o.class == "BaseVoltage")
            .filter_map(|(id, o)| o.parse_f64("nominalVoltage").map(|kv| (id.clone(), kv)))
            .collect();

        // --- VoltageLevel → BaseVoltage ---
        let vl_bv: HashMap<String, String> = objects
            .iter()
            .filter(|(_, o)| o.class == "VoltageLevel")
            .filter_map(|(id, o)| {
                o.get_ref("BaseVoltage")
                    .map(|bv| (id.clone(), bv.to_string()))
            })
            .collect();

        // --- SvVoltage per TopologicalNode ---
        let mut sv_voltage: HashMap<String, (Option<f64>, Option<f64>)> = HashMap::new();
        for (_, sv) in objects.iter().filter(|(_, o)| o.class == "SvVoltage") {
            if let Some(tn_id) = sv.get_ref("TopologicalNode") {
                let v = parse_optional_f64(sv, "v");
                let a = parse_optional_f64(sv, "angle");
                if v.is_some() || a.is_some() {
                    sv_voltage.insert(tn_id.to_string(), (v, a));
                }
            }
        }

        // --- SvInjection per ConductingEquipment (SV profile fallback for load p/q) ---
        //
        // SvInjection records the actual solved P/Q injection at a ConductingEquipment
        // in the SV scenario. Used as fallback when SSH p/q are absent (e.g. IGM-only files).
        // Sign convention: positive = injection into network (generating), per IEC 61970-456.
        let mut sv_injections: HashMap<String, (Option<f64>, Option<f64>)> = HashMap::new();
        for (_, sv) in objects.iter().filter(|(_, o)| o.class == "SvInjection") {
            if let Some(eq_id) = sv
                .get_ref("TopologicalNode")
                .or_else(|| sv.get_ref("ConductingEquipment"))
            {
                let p =
                    parse_optional_f64(sv, "pInjection").or_else(|| parse_optional_f64(sv, "p"));
                let q =
                    parse_optional_f64(sv, "qInjection").or_else(|| parse_optional_f64(sv, "q"));
                if p.is_some() || q.is_some() {
                    sv_injections.insert(eq_id.to_string(), (p, q));
                }
            }
        }

        // --- RegulatingControl target voltage + mode ---
        // MAJ-04: Collect mode alongside targetValue so gen_vs() can guard against
        // non-voltage setpoints (e.g. reactivePower=50 MVAr misinterpreted as 50 kV).
        let mut rc_target_kv: HashMap<String, f64> = HashMap::new();
        let mut rc_mode: HashMap<String, String> = HashMap::new();
        for (id, o) in objects
            .iter()
            .filter(|(_, o)| o.class == "RegulatingControl")
        {
            if let Some(v) = o.parse_f64("targetValue") {
                rc_target_kv.insert(id.clone(), v);
            }
            // mode is a URI reference; extract the local name after '#' or the last '.'.
            if let Some(mode_uri) = o.get_ref("mode").or_else(|| o.get_text("mode")) {
                let local = mode_uri
                    .rsplit('#')
                    .next()
                    .unwrap_or(mode_uri)
                    .rsplit('.')
                    .next()
                    .unwrap_or(mode_uri);
                rc_mode.insert(id.clone(), local.to_string());
            }
        }

        // --- OperationalLimitSet → ConductingEquipment and TopologicalNode ---
        //
        // CGMES: Two reference patterns are permitted:
        //   (1) OperationalLimitSet.Terminal → Terminal → ConductingEquipment / TopologicalNode
        //   (2) OperationalLimitSet.Equipment → ConductingEquipment (direct, without Terminal)
        //
        // Pattern (2) is used by some CGMES exporters (e.g. for transformer windings or
        // bus-connected limits). We handle it as a fallback when Terminal is absent.
        let mut ops_eq: HashMap<String, String> = HashMap::new();
        let mut ops_tn: HashMap<String, String> = HashMap::new();
        for (ops_id, o) in objects
            .iter()
            .filter(|(_, o)| o.class == "OperationalLimitSet")
        {
            if let Some(term_id) = o.get_ref("Terminal") {
                let Some(term) = objects.get(term_id) else {
                    continue;
                };
                if let Some(eq_id) = term.get_ref("ConductingEquipment") {
                    ops_eq.insert(ops_id.clone(), eq_id.to_string());
                }
                if let Some(tn_id) = term.get_ref("TopologicalNode") {
                    ops_tn.insert(ops_id.clone(), tn_id.to_string());
                }
            } else if let Some(eq_id) = o.get_ref("Equipment") {
                // Pattern 2: direct Equipment reference (no Terminal).
                tracing::debug!(
                    ops_id,
                    eq_id,
                    "OperationalLimitSet uses Equipment reference (no Terminal)"
                );
                ops_eq.insert(ops_id.clone(), eq_id.to_string());
            } else {
                tracing::debug!(
                    ops_id,
                    "OperationalLimitSet has neither Terminal nor Equipment reference; skipping"
                );
            }
        }

        // --- OperationalLimitType direction and limitType ---
        //
        // OperationalLimitType.direction (enum URI suffix):
        //   "high"          → upper bound (vmax or positive thermal limit)
        //   "low"           → lower bound (vmin)
        //   "absoluteValue" → unsigned thermal limit (most common for current/power)
        //
        // OperationalLimitType.limitType (enum URI suffix, often ENTSO-E specific):
        //   "patl"  / "PATL"  → Planned All-Time Limit (continuous normal rating → rate_a)
        //   "tatl"  / "TATL"  → Temporary All-Time Limit (emergency/contingency → rate_c)
        //   "highVoltage"      → upper voltage bound → vmax
        //   "lowVoltage"       → lower voltage bound → vmin
        //
        // We extract both as lowercase suffix strings for easy matching.
        let olt_info: HashMap<String, (String, String)> = objects
            .iter()
            .filter(|(_, o)| o.class == "OperationalLimitType")
            .map(|(id, o)| {
                let direction = o
                    .get_ref("direction")
                    .map(|r| r.rsplit(['#', '.']).next().unwrap_or(r).to_lowercase())
                    .unwrap_or_default();
                let limit_type = o
                    .get_ref("limitType")
                    .map(|r| r.rsplit(['#', '.']).next().unwrap_or(r).to_lowercase())
                    .unwrap_or_default();
                (id.clone(), (direction, limit_type))
            })
            .collect();

        // Helper: resolve OperationalLimitType info from an OperationalLimit object.
        let olt_for = |obj: &CimObj| -> (String, String) {
            obj.get_ref("OperationalLimitType")
                .and_then(|olt_id| olt_info.get(olt_id))
                .cloned()
                .unwrap_or_default()
        };

        // --- OperationalLimit → equipment → MVA rating ---
        //
        // CGMES provides thermal ratings via three limit class families:
        //
        // 1. CurrentLimit.value (Amperes) — most common in transmission CGMES files.
        //    Conversion: MVA = A × kV × √3 / 1000
        //
        // 2. ApparentPowerLimit.value (MVA) — provided directly; no conversion needed.
        //    Preferred when present because there is no voltage ambiguity.
        //
        // 3. ActivePowerLimit.value (MW) — expressed in real power; treated as MVA
        //    for rate_a purposes (conservative; equivalent at unity power factor).
        //
        // Limits tagged PATL/normal go to eq_thermal_mva (rate_a, continuous rating).
        // Limits tagged TATL/emergency go to eq_thermal_mva_emergency (rate_c).
        // Untyped limits go to rate_a (conservative fallback).
        let mut eq_thermal_mva: HashMap<String, f64> = HashMap::new();
        let mut eq_thermal_mva_emergency: HashMap<String, f64> = HashMap::new();

        // Classify a limit into the appropriate map based on OperationalLimitType.
        // Returns: true → TATL/emergency (rate_c), false → PATL/normal (rate_a)
        let is_emergency = |dir: &str, lt: &str| -> bool {
            lt.contains("tatl")
                || lt.contains("emergency")
                || dir.contains("tatl")
                || dir.contains("emergency")
        };

        // Helper that inserts an MVA value into the correct thermal map.
        let mut insert_thermal = |eq_id: String, mva: f64, dir: &str, lt: &str| {
            if is_emergency(dir, lt) {
                let entry = eq_thermal_mva_emergency.entry(eq_id).or_insert(f64::MAX);
                *entry = entry.min(mva);
            } else {
                let entry = eq_thermal_mva.entry(eq_id).or_insert(f64::MAX);
                *entry = entry.min(mva);
            }
        };

        // 1a. ApparentPowerLimit — value is already in MVA.
        for (_, apl) in objects
            .iter()
            .filter(|(_, o)| o.class == "ApparentPowerLimit")
        {
            let ops_id = match apl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            let mva = match apl.parse_f64("value") {
                Some(v) if v > 0.0 => v,
                _ => continue,
            };
            let (dir, lt) = olt_for(apl);
            insert_thermal(eq_id, mva, &dir, &lt);
        }

        // 1b. CurrentLimit — value is in Amps; convert via base_kv.
        // Track which OperationalLimitSet IDs carry CurrentLimit children so that
        // ConditionalLimit values referencing the same OLS can be converted from
        // Amps to MVA (a ConditionalLimit inherits its parent limit class's unit).
        let mut ops_has_current_limit: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for (_, cl) in objects.iter().filter(|(_, o)| o.class == "CurrentLimit") {
            let ops_id = match cl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            ops_has_current_limit.insert(ops_id.to_string());
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            let amps = match cl.parse_f64("value") {
                Some(a) if a > 0.0 => a,
                _ => continue,
            };
            // Resolve base_kv for this equipment's from-terminal
            let term_ids = eq_terminals
                .get(&eq_id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let base_kv = term_ids
                .first()
                .and_then(|tid| objects.get(tid.as_str()))
                .and_then(|t| t.get_ref("TopologicalNode"))
                .and_then(|tn_id| {
                    objects
                        .get(tn_id)
                        .and_then(|tn| tn.get_ref("BaseVoltage"))
                        .and_then(|bv_id| bv_kv.get(bv_id).copied())
                })
                .unwrap_or(0.0);
            if base_kv > 0.0 {
                let mva = amps * base_kv * 3.0_f64.sqrt() / 1000.0;
                let (dir, lt) = olt_for(cl);
                insert_thermal(eq_id, mva, &dir, &lt);
            }
        }

        // 1c. ActivePowerLimit — value is in MW; treated as MVA (conservative at unity pf).
        for (_, apl) in objects
            .iter()
            .filter(|(_, o)| o.class == "ActivePowerLimit")
        {
            let ops_id = match apl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            let mw = match apl.parse_f64("value") {
                Some(v) if v > 0.0 => v,
                _ => continue,
            };
            let (dir, lt) = olt_for(apl);
            insert_thermal(eq_id, mw, &dir, &lt);
        }

        // Replace f64::MAX sentinels with 0.0 (unknown rating)
        for v in eq_thermal_mva.values_mut() {
            if *v == f64::MAX {
                *v = 0.0;
            }
        }
        for v in eq_thermal_mva_emergency.values_mut() {
            if *v == f64::MAX {
                *v = 0.0;
            }
        }

        // --- VoltageLimit → TopologicalNode → (vmin_kv, vmax_kv) ---
        //
        // CGMES IEC 61970-301: VoltageLimit is an OperationalLimit subclass carrying
        // bus voltage bounds in kV. The bound direction (high vs low) is indicated by
        // OperationalLimitType.direction ("high" → vmax; "low" → vmin) or by
        // OperationalLimitType.limitType ("highVoltage" → vmax; "lowVoltage" → vmin).
        // Applied to Bus.voltage_max_pu/vmin during bus building.
        let mut tn_voltage_limits: HashMap<String, (f64, f64)> = HashMap::new();
        for (_, vl) in objects.iter().filter(|(_, o)| o.class == "VoltageLimit") {
            let ops_id = match vl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let tn_id = match ops_tn.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            let kv = match vl.parse_f64("value") {
                Some(v) if v > 0.0 => v,
                _ => continue,
            };
            let (dir, lt) = olt_for(vl);
            let is_high = dir.contains("high") || lt.contains("highvoltage");
            let is_low = dir.contains("low") || lt.contains("lowvoltage");
            let entry = tn_voltage_limits.entry(tn_id).or_insert((0.0, f64::MAX));
            if is_high {
                entry.1 = entry.1.min(kv); // most restrictive upper bound
            } else if is_low {
                entry.0 = entry.0.max(kv); // most restrictive lower bound
            }
            // If direction is unknown, skip — avoids misclassification
        }
        // Replace f64::MAX sentinels (no upper bound found) with 0.0 (will not update bus.voltage_max_pu)
        for (vmin, vmax) in tn_voltage_limits.values_mut() {
            if *vmax == f64::MAX {
                *vmax = 0.0;
            }
            let _ = vmin; // vmin of 0.0 means "no lower limit found" — also harmless
        }

        // --- Terminal.connected=false (SSH) → disconnected equipment set ---
        //
        // CGMES SSH profile: Terminal.connected=false indicates that a terminal is
        // electrically isolated from its TopologicalNode in the steady-state operating
        // scenario. This represents breakers or disconnectors that are open in the SSH
        // scenario (and which may differ from the EQ normalOpen state). Any equipment
        // with at least one disconnected terminal is treated as out-of-service and
        // skipped during branch, generator, and load building.
        //
        // Note: The topology (TN union-find) is built from EQ Switch.open, so topology
        // is already correct. Terminal.connected filters out equipment that is isolated
        // at the scenario level without an explicit Switch object (e.g., bypassed lines).
        let mut disconnected_eq: std::collections::HashSet<String> = objects
            .iter()
            .filter(|(_, o)| o.class == "Terminal")
            .filter(|(_, t)| {
                t.get_text("connected")
                    .map(|s| s.eq_ignore_ascii_case("false"))
                    .unwrap_or(false)
            })
            .filter_map(|(_, t)| t.get_ref("ConductingEquipment").map(|s| s.to_string()))
            .collect();

        // --- Sorted TN list ---
        let mut tn_ids: Vec<String> = objects
            .iter()
            .filter(|(_, o)| o.class == "TopologicalNode")
            .map(|(k, _)| k.clone())
            .collect();
        tn_ids.sort();

        // --- CGMES boundary-node deduplication ---
        //
        // In assembled CGMES models (CGM / merging view) the same physical bus can
        // appear as two or more TopologicalNodes: one per IGM that references that
        // boundary point.  Detect duplicates by matching
        //   (IdentifiedObject.name, BaseVoltage mRID)
        // and merge them: keep the first-seen TN as canonical, redirect the rest.
        // After build_network() creates buses the redirected mRIDs are added to
        // idx.tn_bus so equipment on either TN lands on the same bus.
        // Boundary-node deduplication: in assembled CGMES models (N-region merge)
        // the same physical bus can appear with different mRIDs in each IGM, all
        // sharing the same (name, BaseVoltage). We only merge pairs that appear at
        // most MAX_BOUNDARY_DUP times — if a (name, BV) pair appears hundreds of
        // times it is NOT a boundary node but a naming collision within a large
        // node-breaker model (e.g. TNs all named "1" within each VoltageLevel).
        const MAX_BOUNDARY_DUP: usize = 4;
        let mut tn_canonical: HashMap<(String, String), String> = HashMap::new();
        let mut tn_counts: HashMap<(String, String), usize> = HashMap::new();
        let mut tn_redirect: HashMap<String, String> = HashMap::new();
        for tn_id in &tn_ids {
            let obj = match objects.get(tn_id.as_str()) {
                Some(o) => o,
                None => continue,
            };
            let name = match obj.get_text("name") {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };
            // BaseVoltage: direct ref on the TN, or via ConnectivityNodeContainer → VL → BV.
            let bv_id = obj
                .get_ref("BaseVoltage")
                .map(|s| s.to_string())
                .or_else(|| {
                    obj.get_ref("ConnectivityNodeContainer")
                        .and_then(|vl| vl_bv.get(vl))
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            if bv_id.is_empty() {
                continue;
            }
            let key = (name, bv_id);
            let count = tn_counts.entry(key.clone()).or_insert(0);
            *count += 1;
            if *count > MAX_BOUNDARY_DUP {
                // Too many TNs share this (name, BV) — not a boundary node.
                // Remove any previously-added canonical entry for this key so
                // those TNs are also NOT redirected.
                tn_canonical.remove(&key);
                continue;
            }
            match tn_canonical.get(&key) {
                Some(canonical_id) => {
                    tn_redirect.insert(tn_id.clone(), canonical_id.clone());
                }
                None => {
                    tn_canonical.insert(key, tn_id.clone());
                }
            }
        }
        // Remove redirects for keys that exceeded the duplicate threshold
        // (their canonical entry was removed above but the redirect map may still
        // hold entries from when count was still ≤ MAX_BOUNDARY_DUP).
        tn_redirect.retain(|_, canonical| tn_canonical.values().any(|c| c == canonical));
        if !tn_redirect.is_empty() {
            tracing::debug!(
                count = tn_redirect.len(),
                "CGMES: merged {} duplicate TopologicalNode(s) (boundary-node deduplication)",
                tn_redirect.len()
            );
            tn_ids.retain(|id| !tn_redirect.contains_key(id.as_str()));
        }

        // --- RatioTapChanger by TransformerEnd (O(1) lookup replaces O(n) scan) ---
        let rtc_by_end: HashMap<String, String> = objects
            .iter()
            .filter(|(_, o)| o.class == "RatioTapChanger")
            .filter_map(|(rtc_id, o)| {
                o.get_ref("TransformerEnd")
                    .map(|end_id| (end_id.to_string(), rtc_id.clone()))
            })
            .collect();

        // --- PhaseTapChanger* by TransformerEnd (O(1) lookup replaces O(n) scan) ---
        let ptc_by_end: HashMap<String, String> = objects
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o.class.as_str(),
                    "PhaseTapChanger"
                        | "PhaseTapChangerLinear"
                        | "PhaseTapChangerAsymmetrical"
                        | "PhaseTapChangerSymmetrical"
                        | "PhaseTapChangerTabular"
                        | "PhaseTapChangerNonLinear" // CGMES 3.0 (CIM100) — uses PhaseTapChangerTable
                )
            })
            .filter_map(|(ptc_id, o)| {
                o.get_ref("TransformerEnd")
                    .map(|end_id| (end_id.to_string(), ptc_id.clone()))
            })
            .collect();

        // --- PowerTransformerEnd by PowerTransformer (O(1) winding lookup) ---
        let mut pte_by_xfmr: HashMap<String, Vec<(u32, String)>> = HashMap::new();
        for (end_id, o) in objects
            .iter()
            .filter(|(_, o)| o.class == "PowerTransformerEnd")
        {
            if let Some(xfmr_id) = o.get_ref("PowerTransformer") {
                let n = o
                    .get_text("endNumber")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                pte_by_xfmr
                    .entry(xfmr_id.to_string())
                    .or_default()
                    .push((n, end_id.clone()));
            }
        }
        for ends in pte_by_xfmr.values_mut() {
            ends.sort_by_key(|(n, _)| *n);
        }

        // --- ReactiveCapabilityCurve: P-dependent Q-limit envelope ---
        //
        // CGMES IEC 61970-301 §49: ReactiveCapabilityCurve gives the capability boundary
        // as a set of (P, Qmin, Qmax) points.  CurveData objects carry the point data
        // referenced by RCC mRID via CurveData.Curve.  SynchronousMachine links to its
        // RCC via SynchronousMachine.InitialReactiveCapabilityCurve.
        //
        // xvalue = P in MW (IEC sign: negative = generating, positive = consuming).
        // y1value = Qmin (MVAr), y2value = Qmax (MVAr).
        let mut rcc_points: HashMap<String, Vec<(f64, f64, f64)>> = HashMap::new();
        for (_, cd) in objects.iter().filter(|(_, o)| o.class == "CurveData") {
            let rcc_id = match cd.get_ref("Curve") {
                Some(r) => r.to_string(),
                None => continue,
            };
            let x = match cd.parse_f64("xvalue") {
                Some(v) => v,
                None => continue,
            };
            let y1 = cd.parse_f64("y1value").unwrap_or(-9999.0);
            let y2 = cd.parse_f64("y2value").unwrap_or(9999.0);
            rcc_points.entry(rcc_id).or_default().push((x, y1, y2));
        }
        // Sort each curve by P so binary-search interpolation works correctly.
        for pts in rcc_points.values_mut() {
            pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        }

        // SynchronousMachine mRID → ReactiveCapabilityCurve mRID.
        let sm_rcc: HashMap<String, String> = objects
            .iter()
            .filter(|(_, o)| o.class == "SynchronousMachine")
            .filter_map(|(id, o)| {
                o.get_ref("InitialReactiveCapabilityCurve")
                    .map(|rcc_id| (id.clone(), rcc_id.to_string()))
            })
            .collect();

        // --- Wave 17: ReactivePowerLimit → equipment Q bounds ---
        //
        // CGMES IEC 61970-301: ReactivePowerLimit is an OperationalLimit subclass.
        // value is in MVAr. Direction "high" → qmax, "low" → qmin.
        // Associated with ConductingEquipment via OperationalLimitSet.Equipment.
        // Used as fallback Q limits for generators/converters when SM.maxQ/minQ absent.
        let mut eq_reactive_limits: HashMap<String, (f64, f64)> = HashMap::new();
        for (_, rpl) in objects
            .iter()
            .filter(|(_, o)| o.class == "ReactivePowerLimit")
        {
            let ops_id = match rpl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            let mvar = match rpl.parse_f64("value") {
                Some(v) => v,
                None => continue,
            };
            let (dir, _) = olt_for(rpl);
            let entry = eq_reactive_limits.entry(eq_id).or_insert((-9999.0, 9999.0));
            if dir.contains("high") {
                entry.1 = entry.1.min(mvar);
            } else if dir.contains("low") {
                entry.0 = entry.0.max(mvar);
            }
            // "absoluteValue" direction on ReactivePowerLimit is ambiguous — skip to
            // avoid misclassifying a qmax as a qmin. Most real files use "high"/"low".
        }

        // --- Wave 17: PhaseTapChangerLimit → equipment phase-angle bounds ---
        //
        // CGMES IEC 61970-301: PhaseTapChangerLimit is an OperationalLimit subclass.
        // value is in degrees. Direction "high" → phase_max_rad, "low" → phase_min_rad.
        // Associated with ConductingEquipment (PowerTransformer) via OperationalLimitSet.
        // Applied to Branch.phase_min_rad / phase_max_rad for phase-shifting transformers.
        let mut eq_ptc_phase_limits: HashMap<String, (f64, f64)> = HashMap::new();
        for (_, ptcl) in objects
            .iter()
            .filter(|(_, o)| o.class == "PhaseTapChangerLimit")
        {
            let ops_id = match ptcl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            let deg = match ptcl.parse_f64("value") {
                Some(v) => v,
                None => continue,
            };
            let (dir, _) = olt_for(ptcl);
            let entry = eq_ptc_phase_limits.entry(eq_id).or_insert((-30.0, 30.0));
            if dir.contains("high") {
                entry.1 = entry.1.min(deg);
            } else if dir.contains("low") {
                entry.0 = entry.0.max(deg);
            }
        }

        // --- Wave 17: OilTemperatureLimit → equipment oil temperature (°C, informational) ---
        //
        // CGMES IEC 61970-301: OilTemperatureLimit is an OperationalLimit subclass.
        // value is the insulating oil temperature threshold in °C (NOT MVA).
        // Cannot be converted to MVA without equipment-specific thermal derating curves.
        // Stored per spec (OperationalLimitSet.Equipment → ConductingEquipment → Branch).
        // Take the minimum (most restrictive) value when multiple limits exist.
        let mut eq_oil_temp_limit_c: HashMap<String, f64> = HashMap::new();
        for (_, otl) in objects
            .iter()
            .filter(|(_, o)| o.class == "OilTemperatureLimit")
        {
            let ops_id = match otl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            if let Some(temp_c) = otl.parse_f64("value") {
                let entry = eq_oil_temp_limit_c.entry(eq_id).or_insert(f64::MAX);
                *entry = entry.min(temp_c);
            }
        }
        for v in eq_oil_temp_limit_c.values_mut() {
            if *v == f64::MAX {
                *v = 0.0;
            }
        }

        // --- Wave 17: WindingTemperatureLimit → equipment winding temp (°C, informational) ---
        //
        // Same structure as OilTemperatureLimit. value is winding insulation temperature
        // threshold in °C (NOT MVA). Stored informational per CIM spec.
        let mut eq_winding_temp_limit_c: HashMap<String, f64> = HashMap::new();
        for (_, wtl) in objects
            .iter()
            .filter(|(_, o)| o.class == "WindingTemperatureLimit")
        {
            let ops_id = match wtl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            if let Some(temp_c) = wtl.parse_f64("value") {
                let entry = eq_winding_temp_limit_c.entry(eq_id).or_insert(f64::MAX);
                *entry = entry.min(temp_c);
            }
        }
        for v in eq_winding_temp_limit_c.values_mut() {
            if *v == f64::MAX {
                *v = 0.0;
            }
        }

        // --- Wave 17: ImpedanceLimit → equipment impedance bound (Ω, informational) ---
        //
        // CGMES IEC 61970-301: ImpedanceLimit is an OperationalLimit subclass.
        // value is the impedance magnitude limit in Ohms (|Z| ≤ value).
        // Stored informational per CIM spec. Not applied to admittance model.
        let mut eq_impedance_limit_ohm: HashMap<String, f64> = HashMap::new();
        for (_, il) in objects.iter().filter(|(_, o)| o.class == "ImpedanceLimit") {
            let ops_id = match il.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            if let Some(ohm) = il.parse_f64("value") {
                let entry = eq_impedance_limit_ohm.entry(eq_id).or_insert(f64::MAX);
                *entry = entry.min(ohm);
            }
        }
        for v in eq_impedance_limit_ohm.values_mut() {
            if *v == f64::MAX {
                *v = 0.0;
            }
        }

        // --- Wave 17+: ConditionalLimit → fully parsed and stored ---
        //
        // CGMES IEC 61970-301: ConditionalLimit is an OperationalLimit subclass.
        // It carries:
        //   - normalValue: the limit value under the condition (MVA/A/MW)
        //   - OperationalLimitSet → equipment (via ops_eq)
        //   - OperationalLimitType → direction/limitType (via olt_for)
        //   - Condition: reference to a contingency/operating condition (opaque mRID)
        //
        // We extract the condition_id and limit value, storing them per-equipment
        // in conditional_thermal_limits.  The user activates conditions at runtime
        // via Network::apply_conditional_limits().
        let mut conditional_thermal_limits: HashMap<String, Vec<(String, f64, bool)>> =
            HashMap::new();
        for (_, cl) in objects
            .iter()
            .filter(|(_, o)| o.class == "ConditionalLimit")
        {
            let ops_id = match cl.get_ref("OperationalLimitSet") {
                Some(id) => id,
                None => continue,
            };
            let eq_id = match ops_eq.get(ops_id) {
                Some(id) => id.clone(),
                None => continue,
            };
            let value = match cl
                .parse_f64("normalValue")
                .or_else(|| cl.parse_f64("value"))
            {
                Some(v) if v > 0.0 => v,
                _ => continue,
            };
            let condition_id = match cl.get_ref("Condition") {
                Some(id) => id.to_string(),
                None => continue,
            };
            let (dir, lt) = olt_for(cl);
            let is_emerg = is_emergency(&dir, &lt);

            // ConditionalLimit.normalValue is in the same unit as its parent limit class.
            // If the OLS also carries a CurrentLimit, the value is in Amps and must be
            // converted to MVA using the equipment's terminal base voltage.
            let limit_mva = if ops_has_current_limit.contains(ops_id) {
                // Resolve base_kv via the same terminal→TN→BaseVoltage chain used
                // for regular CurrentLimit objects (see 1b above).
                let term_ids = eq_terminals
                    .get(&eq_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let base_kv = term_ids
                    .first()
                    .and_then(|tid| objects.get(tid.as_str()))
                    .and_then(|t| t.get_ref("TopologicalNode"))
                    .and_then(|tn_id| {
                        objects
                            .get(tn_id)
                            .and_then(|tn| tn.get_ref("BaseVoltage"))
                            .and_then(|bv_id| bv_kv.get(bv_id).copied())
                    })
                    .unwrap_or(0.0);
                if base_kv <= 0.0 {
                    continue; // cannot convert without a valid base voltage
                }
                value * base_kv * 3.0_f64.sqrt() / 1000.0
            } else {
                // ApparentPowerLimit or ActivePowerLimit — already in MVA/MW.
                value
            };
            conditional_thermal_limits.entry(eq_id).or_default().push((
                condition_id,
                limit_mva,
                is_emerg,
            ));
        }
        if !conditional_thermal_limits.is_empty() {
            tracing::info!(
                count = conditional_thermal_limits
                    .values()
                    .map(|v| v.len())
                    .sum::<usize>(),
                equipment = conditional_thermal_limits.len(),
                "CGMES: parsed ConditionalLimit objects → conditional_limits on Network"
            );
        }

        // --- Wave 17: PerLengthSequenceImpedance fallback index ---
        //
        // CGMES IEC 61970-301: ACLineSegment.PerLengthImpedance links to a
        // PerLengthSequenceImpedance (positive-sequence) or PerLengthPhaseImpedance (3-phase).
        // When ACLineSegment.r / .x are zero (some exporters omit totals and only provide
        // per-km values), compute: r_total = r1 × length_km, x_total = x1 × length_km.
        // length is in km per CGMES spec (ConductingEquipment.length attribute).
        let line_per_length_imp: HashMap<String, (String, f64)> = objects
            .iter()
            .filter(|(_, o)| o.class == "ACLineSegment")
            .filter_map(|(id, o)| {
                let plsi_id = o.get_ref("PerLengthImpedance")?;
                let length_km = o.parse_f64("length").unwrap_or(0.0);
                Some((id.clone(), (plsi_id.to_string(), length_km)))
            })
            .collect();

        // --- Wave 17: HvdcLine params index ---
        //
        // CGMES IEC 61970-301: HvdcLine represents the DC link between two HVDC
        // converter stations. Attributes: activePowerSetpoint (MW, SSH profile),
        // r (DC resistance Ω), ratedUdc (rated DC voltage kV).
        // activePowerSetpoint is used as P fallback for VsConverter/CsConverter
        // when SSH ACDCConverter.p is absent.
        let hvdc_line_params: HashMap<String, (Option<f64>, Option<f64>, Option<f64>)> = objects
            .iter()
            .filter(|(_, o)| o.class == "HvdcLine")
            .map(|(id, o)| {
                let p = parse_optional_f64(o, "activePowerSetpoint");
                let r = parse_optional_f64(o, "r");
                let udc = parse_optional_f64(o, "ratedUdc");
                (id.clone(), (p, r, udc))
            })
            .collect();

        // --- Wave 17: ACDCConverter → HvdcLine mapping (for P-setpoint fallback) ---
        //
        // Traverse: ACDCConverterDCTerminal.ACDCConverter → ACDCConverterDCTerminal.DCNode
        //       and: DCTerminal.DCConductingEquipment (HvdcLine) → DCTerminal.DCNode
        // Build: conv_id → hvdc_line_id via shared DCNode.
        //
        // Class hierarchy (CGMES 2.4.15 IEC 61970-301):
        //   ACDCConverterDCTerminal (class) has refs: ACDCConverter, DCNode
        //   DCTerminal (class) has refs: DCConductingEquipment, DCNode
        // --- DC topology indexing ---
        // Build full DCNode connectivity for DC network construction + HvdcLine P-fallback.

        // DCTerminal → (DCConductingEquipment, DCNode): build dcnode_to_eq and dcnode_to_hvdc.
        let mut dcnode_to_hvdc: HashMap<String, String> = HashMap::new();
        let mut dcnode_to_eq: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (_, t) in objects.iter().filter(|(_, o)| o.class == "DCTerminal") {
            if let (Some(eq_id), Some(node_id)) =
                (t.get_ref("DCConductingEquipment"), t.get_ref("DCNode"))
            {
                let eq_class = objects
                    .get(eq_id)
                    .map(|e| e.class.clone())
                    .unwrap_or_default();
                dcnode_to_eq
                    .entry(node_id.to_string())
                    .or_default()
                    .push((eq_id.to_string(), eq_class.clone()));
                if eq_class == "HvdcLine" {
                    dcnode_to_hvdc.insert(node_id.to_string(), eq_id.to_string());
                }
            }
        }

        // ACDCConverterDCTerminal → (ACDCConverter, DCNode): build conv_hvdc and conv_to_dcnode.
        let mut conv_hvdc: HashMap<String, String> = HashMap::new();
        let mut conv_to_dcnode: HashMap<String, String> = HashMap::new();
        for (_, t) in objects
            .iter()
            .filter(|(_, o)| o.class == "ACDCConverterDCTerminal")
        {
            if let (Some(conv_id), Some(node_id)) =
                (t.get_ref("ACDCConverter"), t.get_ref("DCNode"))
            {
                conv_to_dcnode.insert(conv_id.to_string(), node_id.to_string());
                if let Some(hvdc_id) = dcnode_to_hvdc.get(node_id) {
                    conv_hvdc.insert(conv_id.to_string(), hvdc_id.clone());
                }
            }
        }

        // --- Wave 19: TapChangerControl index ---
        //
        // CGMES IEC 61970-301: TapChanger.TapChangerControl (0..1) links any
        // RatioTapChanger or PhaseTapChanger to its TapChangerControl.
        // TapChangerControl.regulating (SSH bool): true = OLTC actively regulates;
        //   false = locked at the SSH/SV step position.
        // TapChangerControl.deadband (EQ float, kV): voltage tolerance band.
        // TapChangerControl.regulatingMode (EQ enum URI): what the tap changer regulates
        //   (voltage, reactivePower, activePower, currentFlow, admittance, etc.).
        let tc_to_tcc: HashMap<String, String> = objects
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o.class.as_str(),
                    "RatioTapChanger"
                        | "PhaseTapChanger"
                        | "PhaseTapChangerLinear"
                        | "PhaseTapChangerAsymmetrical"
                        | "PhaseTapChangerSymmetrical"
                        | "PhaseTapChangerTabular"
                        | "PhaseTapChangerNonLinear"
                )
            })
            .filter_map(|(tc_id, o)| {
                o.get_ref("TapChangerControl")
                    .map(|tcc_id| (tc_id.clone(), tcc_id.to_string()))
            })
            .collect();

        let tcc_params: HashMap<String, (bool, f64, String)> = objects
            .iter()
            .filter(|(_, o)| o.class == "TapChangerControl")
            .map(|(id, o)| {
                // regulating from SSH (merged into object store); absent → default true
                let regulating = o
                    .get_text("regulating")
                    .map(|s| s.eq_ignore_ascii_case("true"))
                    .unwrap_or(true);
                let deadband = o.parse_f64("deadband").unwrap_or(0.0);
                let mode = o
                    .get_ref("regulatingMode")
                    .map(|r| r.rsplit(['#', '.']).next().unwrap_or(r).to_lowercase())
                    .unwrap_or_default();
                (id.clone(), (regulating, deadband, mode))
            })
            .collect();

        // --- Wave 20: TransformerCoreAdmittance index ---
        //
        // CGMES IEC 61970-301: TransformerCoreAdmittance links to a PowerTransformerEnd
        // via TransformerCoreAdmittance.TransformerEnd.  Its g/b (S) specify the positive-
        // sequence shunt admittance at that winding and override PowerTransformerEnd.g/b.
        // g0/b0 are zero-sequence values (stored for future zero-seq model; not yet used).
        let core_admittance_by_end: HashMap<String, (f64, f64)> = objects
            .iter()
            .filter(|(_, o)| o.class == "TransformerCoreAdmittance")
            .filter_map(|(_, o)| {
                let end_id = o.get_ref("TransformerEnd")?.to_string();
                let g = o.parse_f64("g").unwrap_or(0.0);
                let b = o.parse_f64("b").unwrap_or(0.0);
                Some((end_id, (g, b)))
            })
            .collect();

        // --- Wave 21: TransformerMeshImpedance index (3W mesh → star) ---
        //
        // CGMES IEC 61970-301: A 3-winding transformer may specify its leakage
        // impedances as three TransformerMeshImpedance objects — one per winding pair
        // (end1↔end2, end1↔end3, end2↔end3) — rather than as per-winding r/x on each
        // PowerTransformerEnd.  Each TMI has FromTransformerEnd and ToTransformerEnd refs.
        //
        // Mesh → star conversion (r/x are additive in the mesh topology):
        //   r1 = (r12 + r13 - r23) / 2
        //   r2 = (r12 + r23 - r13) / 2
        //   r3 = (r13 + r23 - r12) / 2  (and similarly for x).
        //
        // Result is stored as (r1, x1, r2, x2, r3, x3) in Ω, keyed by transformer mRID.
        // 2W transformers are not affected (they use PowerTransformerEnd.r/x directly).
        let mut tmi_by_xfmr: HashMap<String, Vec<(String, String, f64, f64)>> = HashMap::new();
        for (_, o) in objects
            .iter()
            .filter(|(_, o)| o.class == "TransformerMeshImpedance")
        {
            let from_end = o.get_ref("FromTransformerEnd").map(|s| s.to_string());
            let to_end = o.get_ref("ToTransformerEnd").map(|s| s.to_string());
            let r = o.parse_f64("r").unwrap_or(0.0);
            let x = o.parse_f64("x").unwrap_or(0.0);
            if let (Some(fe), Some(te)) = (from_end, to_end) {
                // Resolve the transformer from the from-end reference.
                if let Some(xfmr_id) = objects
                    .get(fe.as_str())
                    .and_then(|e| e.get_ref("PowerTransformer"))
                {
                    tmi_by_xfmr
                        .entry(xfmr_id.to_string())
                        .or_default()
                        .push((fe, te, r, x));
                }
            }
        }
        let mesh_imp: HashMap<String, (f64, f64, f64, f64, f64, f64)> = tmi_by_xfmr
            .into_iter()
            .filter_map(|(xfmr_id, tmi_list)| {
                if tmi_list.len() != 3 {
                    return None; // incomplete mesh spec — skip
                }
                let ends = pte_by_xfmr.get(xfmr_id.as_str())?;
                if ends.len() < 3 {
                    return None; // not a 3W transformer
                }
                let end1_id = &ends[0].1;
                let end2_id = &ends[1].1;
                let end3_id = &ends[2].1;
                // Rated voltages for each winding end — needed for voltage-base referral.
                let get_rated_u = |end_id: &str| -> f64 {
                    objects
                        .get(end_id)
                        .and_then(|o| o.parse_f64("ratedU"))
                        .unwrap_or(1.0)
                        .max(1.0)
                };
                let u1 = get_rated_u(end1_id);
                let u2 = get_rated_u(end2_id);
                let u3 = get_rated_u(end3_id);
                // Find mesh impedance for each winding pair.
                // CGMES: TMI.r is referred to the FromTransformerEnd voltage base.
                // r12/r13 are at u1 base; r23 is at u2 base.
                // When the pair is found in reverse order (fe=b, te=a) the stored
                // impedance is still at the original from-end base — handled explicitly
                // below by inspecting which end is the from-end.
                let find_pair_at_from_base = |a: &str, b: &str| -> (f64, f64, bool) {
                    tmi_list
                        .iter()
                        .find(|(fe, te, ..)| {
                            (fe.as_str() == a && te.as_str() == b)
                                || (fe.as_str() == b && te.as_str() == a)
                        })
                        .map(|(fe, _, r, x)| {
                            // is_forward: true means the stored value is at `a`'s base;
                            // false means it is at `b`'s base (reversed entry).
                            let is_forward = fe.as_str() == a;
                            (*r, *x, is_forward)
                        })
                        .unwrap_or((0.0, 0.0, true))
                };
                // r12, r13 at end1 base; r23 at end2 base.
                let (r12, x12, _) = find_pair_at_from_base(end1_id, end2_id);
                let (r13, x13, _) = find_pair_at_from_base(end1_id, end3_id);
                let (r23_raw, x23_raw, r23_from_is_end2) = find_pair_at_from_base(end2_id, end3_id);
                // Refer r23 to end1 base.  If the stored entry had from=end2 then it
                // is already at u2; if from=end3 refer via (u3/u1)².
                let r23_ref1 = if r23_from_is_end2 {
                    r23_raw * (u2 / u1).powi(2)
                } else {
                    r23_raw * (u3 / u1).powi(2)
                };
                let x23_ref1 = if r23_from_is_end2 {
                    x23_raw * (u2 / u1).powi(2)
                } else {
                    x23_raw * (u3 / u1).powi(2)
                };
                // Mesh → star at end1 base.
                let r1_ref1 = (r12 + r13 - r23_ref1) / 2.0;
                let x1_ref1 = (x12 + x13 - x23_ref1) / 2.0;
                let r2_ref1 = (r12 + r23_ref1 - r13) / 2.0;
                let x2_ref1 = (x12 + x23_ref1 - x13) / 2.0;
                let r3_ref1 = (r13 + r23_ref1 - r12) / 2.0;
                let x3_ref1 = (x13 + x23_ref1 - x12) / 2.0;
                // Refer each star impedance back to its own winding base.
                // r1 stays at end1 base; r2 at end2; r3 at end3.
                let r1 = r1_ref1;
                let x1 = x1_ref1;
                let r2 = r2_ref1 * (u1 / u2).powi(2);
                let x2 = x2_ref1 * (u1 / u2).powi(2);
                let r3 = r3_ref1 * (u1 / u3).powi(2);
                let x3 = x3_ref1 * (u1 / u3).powi(2);
                Some((xfmr_id, (r1, x1, r2, x2, r3, x3)))
            })
            .collect();

        // --- Wave 28: SvStatus index ---
        //
        // CGMES IEC 61970-301: SvStatus (SV profile) carries the in-service flag for
        // conducting equipment as solved in the network state snapshot.
        // SvStatus.inService (bool) takes precedence over Equipment.normallyInService
        // and Terminal.connected for determining if equipment is energised.
        // A false entry here marks equipment as out-of-service even if terminals are
        // connected (e.g., equipment in a planned outage not reflected in the EQ profile).
        let mut sv_status: HashMap<String, bool> = HashMap::new();
        let mut sv_status_invalid: HashSet<String> = HashSet::new();
        for (_, o) in objects.iter().filter(|(_, o)| o.class == "SvStatus") {
            let Some(eq_id) = o.get_ref("ConductingEquipment").map(str::to_string) else {
                continue;
            };
            match o.get_text("inService") {
                Some(s) if s.eq_ignore_ascii_case("true") || s == "1" => {
                    sv_status.insert(eq_id, true);
                }
                Some(s) if s.eq_ignore_ascii_case("false") || s == "0" => {
                    sv_status.insert(eq_id, false);
                }
                Some(s) => {
                    tracing::warn!(
                        eq_id,
                        raw = s,
                        "CGMES SvStatus.inService is malformed; leaving equipment state unchanged"
                    );
                    sv_status_invalid.insert(eq_id);
                }
                None => {
                    sv_status_invalid.insert(eq_id);
                }
            }
        }
        // Extend disconnected_eq with equipment marked out-of-service by SvStatus.
        // This ensures SvStatus.inService=false is respected even when Terminal.connected=true.
        disconnected_eq.extend(
            sv_status
                .iter()
                .filter(|(_, in_svc)| !**in_svc)
                .map(|(eq_id, _)| eq_id.clone()),
        );

        // --- Wave 24: Cut and Clamp indices ---
        //
        // CGMES IEC 61970-301: `Cut` divides an ACLineSegment at a point.
        // `Cut.ACLineSegment` → parent segment.  `CutAction.open` (SSH bool, merged into
        // the Cut object as `open`) = true means the segment is electrically split at that
        // point.  Positive-sequence bus-branch model: when open, we skip the parent
        // ACLineSegment (conservative: remove it entirely).
        //
        // CGMES IEC 61970-301: `Clamp` is a T-tap on an ACLineSegment.
        // `Clamp.ACLineSegment` → parent segment.
        // `Clamp.lengthFromTerminal1` (km from terminal 1) → split position.
        // `Clamp.Terminal` → the connecting terminal.
        // When present, the segment is split into two branches at each clamp position and
        // an intermediate bus is created for the clamp's terminal.
        let cut_open_lines: std::collections::HashSet<String> = objects
            .iter()
            .filter(|(_, o)| o.class == "Cut")
            .filter(|(_, o)| {
                // open (SSH) = true → segment is split open
                o.get_text("open")
                    .map(|s| s.eq_ignore_ascii_case("true"))
                    .unwrap_or(false)
            })
            .filter_map(|(_, o)| o.get_ref("ACLineSegment").map(|s| s.to_string()))
            .collect();

        // Build clamp_by_line: ACLineSegment mRID → Vec<(frac, clamp_tn_id)> sorted by frac.
        // frac = lengthFromTerminal1 / segment.length.
        // We look up the segment length from the ACLineSegment object.
        let mut clamp_by_line_raw: HashMap<String, Vec<(f64, String)>> = HashMap::new();
        for (_, o) in objects.iter().filter(|(_, o)| o.class == "Clamp") {
            let Some(line_id) = o.get_ref("ACLineSegment").map(|s| s.to_string()) else {
                continue;
            };
            // Resolve the Clamp's Terminal TopologicalNode.
            // The Terminal mRID referenced here: look up Clamp.Terminals (via Terminal.ConductingEquipment)
            // Actually, Clamp itself has a Terminal — find it via eq_terminals or directly.
            // CGMES: Clamp has one Terminal. Find Terminals with ConductingEquipment = this Clamp.
            // (Clamp mRID is the key we'd need, but we don't have it — we're iterating the Clamp obj.)
            // Alternative: check if Clamp object has a direct terminal reference in attrs.
            let clamp_tn_id = o
                .get_ref("Terminal")
                .and_then(|t_id| objects.get(t_id))
                .and_then(|t_obj| t_obj.get_ref("TopologicalNode"))
                .map(|tn| tn.to_string());
            let Some(tn_id) = clamp_tn_id else {
                continue;
            };
            let length_from_t1 = o.parse_f64("lengthFromTerminal1").unwrap_or(0.0);
            // Get total segment length to compute fraction.
            let total_length = objects
                .get(line_id.as_str())
                .and_then(|seg| seg.parse_f64("length"))
                .unwrap_or(0.0);
            let frac = if total_length > 0.0 {
                (length_from_t1 / total_length).clamp(0.0, 1.0)
            } else {
                0.5 // midpoint if length unknown
            };
            clamp_by_line_raw
                .entry(line_id)
                .or_default()
                .push((frac, tn_id));
        }
        // Sort each line's clamps by fraction (ascending) for sequential splitting.
        let mut clamp_by_line = clamp_by_line_raw;
        for clamps in clamp_by_line.values_mut() {
            clamps.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        }

        CgmesIndices {
            eq_terminals,
            bv_kv,
            vl_bv,
            sv_voltage,
            tn_bus: HashMap::new(),
            rc_target_kv,
            rc_mode,
            tn_ids,
            eq_thermal_mva,
            eq_thermal_mva_emergency,
            tn_voltage_limits,
            rtc_by_end,
            ptc_by_end,
            pte_by_xfmr,
            tn_redirect,
            disconnected_eq,
            rcc_points,
            sm_rcc,
            sv_injections,
            eq_reactive_limits,
            eq_ptc_phase_limits,
            eq_oil_temp_limit_c,
            eq_winding_temp_limit_c,
            sv_status_invalid,
            eq_impedance_limit_ohm,
            line_per_length_imp,
            hvdc_line_params,
            conv_hvdc,
            conv_to_dcnode,
            dcnode_to_eq,
            tc_to_tcc,
            tcc_params,
            core_admittance_by_end,
            mesh_imp,
            cut_open_lines,
            clamp_by_line,
            conditional_thermal_limits,
        }
    }

    /// Get terminal IDs for a piece of conducting equipment (sorted by seq#).
    pub(crate) fn terminals(&self, eq_id: &str) -> &[String] {
        self.eq_terminals
            .get(eq_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Resolve TopologicalNode mRID from a Terminal.
    pub(crate) fn terminal_tn<'a>(&self, objects: &'a ObjMap, tid: &str) -> Option<&'a str> {
        objects.get(tid)?.get_ref("TopologicalNode")
    }

    /// Get bus number for a TN mRID.
    pub(crate) fn tn_bus(&self, tn_id: &str) -> Option<u32> {
        self.tn_bus.get(tn_id).copied()
    }

    /// Get base_kv for a BaseVoltage mRID.
    pub(crate) fn bv_kv(&self, bv_id: &str) -> f64 {
        if let Some(&kv) = self.bv_kv.get(bv_id) {
            kv
        } else {
            tracing::warn!(
                "CGMES BaseVoltage mRID '{}' not found; defaulting to 1.0 kV. \
                 Impedance conversions for equipment referencing this BaseVoltage will be wrong. \
                 Ensure the EQ profile is included.",
                bv_id
            );
            1.0
        }
    }

    /// Resolve base_kv from an object's direct BaseVoltage ref, or fall
    /// back through EquipmentContainer (VoltageLevel) → BaseVoltage.
    pub(crate) fn resolve_base_kv(&self, obj: &CimObj) -> f64 {
        // 1. Direct ConductingEquipment.BaseVoltage or TransformerEnd.BaseVoltage
        if let Some(bv_id) = obj.get_ref("BaseVoltage") {
            return self.bv_kv(bv_id);
        }
        // 2. EquipmentContainer → VoltageLevel → BaseVoltage
        if let Some(vl_id) = obj.get_ref("EquipmentContainer")
            && let Some(bv_id) = self.vl_bv.get(vl_id)
        {
            return self.bv_kv(bv_id);
        }
        1.0
    }

    /// Resolve voltage set-point (per-unit) for a SynchronousMachine.
    /// Resolve voltage setpoint (pu) from RegulatingControl.targetValue.
    ///
    /// Returns `Some(vs_pu)` when a voltage-mode RegulatingControl with a valid
    /// targetValue is found; `None` when the machine has no voltage regulation
    /// (no RC, non-voltage mode, or missing targetValue).  Callers should treat
    /// `None` as "this machine does not regulate voltage" — i.e., model it as a
    /// PQ injection rather than a PV generator with a default Vs=1.0.
    pub(crate) fn gen_vs(&self, objects: &ObjMap, sm: &CimObj, bus_base_kv: f64) -> Option<f64> {
        if bus_base_kv <= 0.0 {
            return None;
        }
        // MAJ-04: Only use targetValue as a kV setpoint when the RegulatingControl mode
        // is "voltage".  Other modes (reactivePower, activePower, currentFlow, etc.)
        // have targetValue in completely different units and must NOT be treated as kV.
        let rc_is_voltage_mode = |rc_id: &str| -> bool {
            if let Some(mode) = self.rc_mode.get(rc_id) {
                return mode.eq_ignore_ascii_case("voltage");
            }
            // mode absent from our index — check object store directly.
            if let Some(rc) = objects.get(rc_id) {
                if let Some(mode_uri) = rc.get_ref("mode").or_else(|| rc.get_text("mode")) {
                    let local = mode_uri
                        .rsplit('#')
                        .next()
                        .unwrap_or(mode_uri)
                        .rsplit('.')
                        .next()
                        .unwrap_or(mode_uri);
                    return local.eq_ignore_ascii_case("voltage");
                }
                // mode attribute absent entirely — conservative: do not use as kV.
                return false;
            }
            false
        };
        // SM → RegulatingCondEq.RegulatingControl → RC → targetValue (kV)
        if let Some(rc_id) = sm.get_ref("RegulatingControl")
            && rc_is_voltage_mode(rc_id)
            && let Some(&kv) = self.rc_target_kv.get(rc_id)
            && kv > 0.0
        {
            return Some(kv / bus_base_kv);
        }
        // Fallback: look for RC in object store (in case key stripped differently)
        if let Some(rc_id) = sm.get_ref("RegulatingControl")
            && rc_is_voltage_mode(rc_id)
            && let Some(rc) = objects.get(rc_id)
            && let Some(kv) = rc.parse_f64("targetValue")
            && kv > 0.0
        {
            return Some(kv / bus_base_kv);
        }
        None
    }
}
