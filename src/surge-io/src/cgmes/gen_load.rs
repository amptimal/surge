// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES generator, load, and slack builder functions.

use std::collections::HashMap;

use super::dc_network::build_dc_network;
use super::helpers::interpolate_rcc;
use super::indices::CgmesIndices;
use super::types::ObjMap;
use surge_network::Network;
use surge_network::network::power_injection::PowerInjectionKind;
use surge_network::network::{
    BusType, CgmesEquivalentInjectionSource, CgmesExternalNetworkInjectionSource, GenType,
    Generator, Load, PowerInjection,
};

fn push_fixed_injection(
    network: &mut Network,
    bus_num: u32,
    id: &str,
    kind: PowerInjectionKind,
    p_mw: f64,
    q_mvar: f64,
) {
    network.power_injections.push(PowerInjection {
        bus: bus_num,
        id: id.to_string(),
        kind,
        active_power_injection_mw: p_mw,
        reactive_power_injection_mvar: q_mvar,
        in_service: true,
    });
}

pub(crate) fn build_generators_and_loads(
    objects: &ObjMap,
    idx: &CgmesIndices,
    network: &mut Network,
) {
    let base_mva = network.base_mva;

    // Log counts of equipment types that are intentionally not modelled in positive-sequence
    // steady-state power flow, with the specific engineering justification for each.
    //
    // ── Zero-sequence / neutral-point equipment ──────────────────────────────────────
    // PetersenCoil:            arc-suppression coil at transformer neutral.  In balanced
    //                          positive-sequence steady-state, neutral current is identically
    //                          zero — this device carries no current and has no PF impact.
    // Ground:                  neutral earthing resistance.  Same reason: no neutral current
    //                          in balanced operation.
    // GroundingImpedance:      neutral-point grounding impedance.  Same reason.
    //
    // ── Transparent topology elements (bus-branch model with TP profile) ─────────────
    // BusbarSection:           a physical busbar rail.  All terminals connected to it share
    //                          the same TopologicalNode in the TP profile, so they already
    //                          map to the same bus — no additional modelling needed.
    // Jumper:                  zero-impedance normally-closed switch.  In the bus-branch
    //                          model the TP profile already reflects its closed state:
    //                          both endpoints are in the same TopologicalNode.
    // Junction:                zero-impedance coupling point.  Identical reasoning to Jumper.
    //
    // ── DC-side pure topology (AC boundary handled by VsConverter / CsConverter) ──────
    // DCNode / DCTerminal:     pure DC topology nodes with no AC boundary.
    // ACDCConverter:           abstract base class; concrete subclasses VsConverter /
    //                          CsConverter are handled separately and inject P+Q at their
    //                          AC terminal.
    // DCConverterUnit:         HVDC station container — grouping only, no model.
    // DCBusbar:                DC busbar rail — DC-side topology only.
    // DCGround:                DC earthing — DC-side only.
    //
    // ── Electromagnetic coupling ─────────────────────────────────────────────────────
    // MutualCoupling:          inductive coupling between parallel circuits.  For transposed
    //                          transmission lines, positive-sequence mutual coupling is zero
    //                          by symmetry.  This is the standard assumption in all major
    //                          positive-sequence PF solvers (MATPOWER, PowerModels, pypowsybl).
    //
    // ── Operational limit subtypes ────────────────────────────────────────────────────
    // ReactivePowerLimit:      implemented (Wave 17) — Q-limit fallback for generators.
    // PhaseTapChangerLimit:    implemented (Wave 17) — phase_min/max_deg on Branch.
    // OilTemperatureLimit:     implemented (Wave 17) — stored as °C on Branch.oil_temp_limit_c.
    // WindingTemperatureLimit: implemented (Wave 17) — stored as °C on Branch.winding_temp_limit_c.
    // ImpedanceLimit:          implemented (Wave 17) — stored as Ω on Branch.impedance_limit_ohm.
    // ConditionalLimit:        fully wired — condition_id + limit_mva stored in Network.conditional_limits.
    //
    // NOTE: The following classes ARE modelled elsewhere in this file and must NOT
    // appear here: ACLineSegment (lines), PowerTransformer (branches), SynchronousMachine
    // (generators), EnergyConsumer/ConformLoad/NonConformLoad (loads),
    // LinearShuntCompensator / NonlinearShuntCompensator / StaticVarCompensator (shunts),
    // SeriesCompensator (series branch), DanglingLine, EquivalentBranch / EquivalentShunt /
    // EquivalentInjection, VsConverter / CsConverter (HVDC AC injection),
    // AsynchronousMachine, ExternalNetworkInjection, EnergySource, StationSupply,
    // HvdcLine (Wave 17 — activePowerSetpoint fallback implemented),
    // PerLengthSequenceImpedance (Wave 17 — fallback implemented).
    //
    // ── Implemented in Waves 20–29 (not modelled in positive-seq PF but data stored) ──────
    // ControlArea:             Wave 22 — parsed into network.area_schedules.
    // TransformerCoreAdmittance: Wave 20 — overrides PowerTransformerEnd.b/g.
    // TransformerMeshImpedance:  Wave 21 — mesh→star for 3W transformers.
    // FrequencyConverter:      Wave 23 — modelled as coupled load+generator (see below).
    // Clamp / Cut:             Wave 24 — Clamp splits line; open Cut disconnects line.
    // PerLengthPhaseImpedance: Wave 25 — stored in network.cim.per_length_phase_impedances.
    // MutualCoupling:          Wave 25 — stored in network.cim.mutual_couplings.
    // Ground:                  Wave 26 — stored in network.cim.grounding_impedances (x=0).
    // GroundingImpedance:      Wave 26 — stored in network.cim.grounding_impedances.
    // PetersenCoil:            Wave 26 — stored in network.cim.grounding_impedances.
    // LoadResponseCharacteristic: Wave 27 — ZIP coefficients set on per-Load fields.
    // SvStatus:                Wave 28 — extends disconnected_eq for out-of-service eq.
    // Location / PositionPoint: Wave 29 — stored in network.cim.geo_locations.
    //
    // Classes still not modelled in positive-seq bus-branch PF (engineering justifications):
    for cls in [
        // transparent topology (already resolved by TP profile union-find)
        // BusbarSection: physical busbar rail. All terminals share same TopologicalNode.
        // Jumper / Junction: zero-impedance connections. TP profile merges them into one TN.
        "BusbarSection",
        "Jumper",
        "Junction",
        // DC-side pure topology (AC boundary handled by VsConverter / CsConverter)
        // DCNode / DCTerminal: pure DC topology nodes — no AC admittance contribution.
        // ACDCConverter: abstract base class — VsConverter/CsConverter handled separately.
        // DCConverterUnit: HVDC station container (grouping only, no model).
        // DCBusbar: DC-side topology (validated in build_dc_network).
        // DCGround: ground return resistance → DcBus.r_ground_ohm (monopole/asymmetric bipole).
        "DCNode",
        "DCTerminal",
        "ACDCConverter",
        "DCConverterUnit",
        "DCBusbar",
        "DCGround",
        // DC network equipment — modelled in build_dc_network():
        // DCLineSegment: series resistance → DcBranch.r_ohm (used in DC PF I²R losses).
        // DCSwitch / DCBreaker: open state → DcBranch.status=false (DC topology switching).
        // DCShunt: filter ESR → DcBus.g_shunt_siemens (shunt losses in DC KCL).
        // DCSeriesDevice: smoothing reactor R → added to DcBranch.r_ohm.
        "DCLineSegment",
        "DCSwitch",
        "DCBreaker",
        "DCShunt",
        "DCSeriesDevice",
        // CGMES 3.0 (CIM100) DC topology classes — DC boundary handled by VsConverter/CsConverter.
        // DCTopologicalNode: DC bus in CGMES 3.0 topology (analogous to TopologicalNode for AC).
        // DCTopologicalIsland: grouping of DC topology nodes (analogous to TopologicalIsland).
        "DCTopologicalNode",
        "DCTopologicalIsland",
        // CGMES 3.0 PowerElectronicsUnit DC-side metadata — AC side handled by PowerElectronicsConnection.
        // BatteryUnit: ratedE/storedE are energy capacity metadata, not relevant for positive-seq PF.
        // PhotovoltaicUnit: DC-side PV array metadata; PEC handles AC injection.
        // PowerElectronicsUnit: abstract base class for BatteryUnit / PhotovoltaicUnit.
        "BatteryUnit",
        "PhotovoltaicUnit",
        "PowerElectronicsUnit",
    ] {
        let n = objects.values().filter(|o| o.class == cls).count();
        if n > 0 {
            tracing::debug!(
                count = n,
                class = cls,
                "CGMES equipment type not modelled in positive-sequence PF (see justification in build_generators_and_loads)"
            );
        }
    }

    // Pre-built index for O(1) bus mutation (replaces O(n) iter_mut().find() per equipment).
    let bus_num_to_idx: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    // --- SynchronousMachine → Generator ---
    let sm_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "SynchronousMachine")
        .map(|(k, _)| k.clone())
        .collect();

    for sm_id in &sm_ids {
        if idx.disconnected_eq.contains(sm_id.as_str()) {
            continue;
        }
        let sm = &objects[sm_id];

        let bus_num = idx
            .terminals(sm_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                sm.get_ref("EquipmentContainer").and_then(|vl_id| {
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
            None => continue,
        };

        // SSH: p is negative for generator output (IEC convention), positive for motor.
        let p_ssh = sm.parse_f64("p").unwrap_or(0.0);
        let q_ssh = sm.parse_f64("q").unwrap_or(0.0);

        // Determine operating mode from SSH attribute (URI fragment ends with the mode name).
        let mode = sm.get_ref("operatingMode").unwrap_or("");
        let kind = sm.get_ref("type").unwrap_or("");
        let is_motor = mode.ends_with(".motor")
            || mode.ends_with("#motor")
            || (kind.contains("generatorOrMotor") && p_ssh > 1e-6);
        let is_condenser = mode.ends_with(".condenser")
            || mode.ends_with("#condenser")
            || mode.ends_with("synchronousCondenser");

        if is_motor {
            // Motor machine: consuming positive P (IEC convention).
            // If the machine has voltage regulation enabled (controlEnabled=true), model it
            // as a generator with negative scheduled Pg so that assign_slack() can mark its
            // bus as PV and the NR can regulate voltage — matching OpenLoadFlow behaviour.
            // If voltage regulation is disabled (controlEnabled=false or absent), fall back
            // to the simpler PQ-load treatment.
            let control_enabled = sm
                .get_text("controlEnabled")
                .map(|s| s == "true")
                .unwrap_or(false);
            if control_enabled {
                let pg = -p_ssh; // negative = consuming active power
                let qg = -q_ssh; // initial reactive (NR will adjust for PV buses)
                let bus_base_kv = bus_num_to_idx
                    .get(&bus_num)
                    .and_then(|&i| network.buses.get(i))
                    .map(|b| b.base_kv)
                    .unwrap_or(1.0);
                let vs = idx.gen_vs(objects, sm, bus_base_kv).unwrap_or(1.0);
                let qmax = sm.parse_f64("maxQ").unwrap_or(9999.0);
                let qmin = sm.parse_f64("minQ").unwrap_or(-9999.0);
                let mut g = Generator::new(bus_num, pg, vs);
                g.machine_id = Some(sm_id.clone());
                g.q = qg;
                g.qmax = qmax;
                g.qmin = qmin;
                g.pmax = 0.0; // motors do not generate active power
                g.pmin = pg.min(0.0);
                g.machine_base_mva = base_mva;
                network.generators.push(g);
            } else {
                // No voltage regulation — model as PQ load.
                network.loads.push(Load {
                    bus: bus_num,
                    active_power_demand_mw: p_ssh,
                    reactive_power_demand_mvar: q_ssh,
                    in_service: true,
                    conforming: true,
                    id: sm_id.clone(),
                    ..Load::new(0, 0.0, 0.0)
                });
            }
            continue;
        }

        let pg = if is_condenser { 0.0 } else { -p_ssh };
        let qg = -q_ssh; // q also negated per IEC convention

        // MAJ-05: CGMES SynchronousMachine.controlEnabled (SSH, default=true for generators).
        // When false, the machine does not participate in voltage regulation and should be
        // modelled as a PQ injection (negative load) rather than a PV generator.
        // Default is true per CGMES convention for generating machines.
        let control_enabled = sm
            .get_text("controlEnabled")
            .map(|s| s == "true")
            .unwrap_or(true);
        if !control_enabled {
            // Model as fixed P/Q generator injection without voltage regulation.
            let mut g = Generator::new(bus_num, pg, 1.0);
            g.machine_id = Some(sm_id.clone());
            g.q = qg;
            g.qmax = qg.max(sm.parse_f64("maxQ").unwrap_or(qg));
            g.qmin = qg.min(sm.parse_f64("minQ").unwrap_or(qg));
            g.pmax = pg.max(sm.parse_f64("maxOperatingP").unwrap_or(pg));
            g.pmin = pg.min(sm.parse_f64("minOperatingP").unwrap_or(pg));
            g.machine_base_mva = base_mva;
            g.voltage_regulated = false;
            network.generators.push(g);
            continue;
        }

        let bus_base_kv = bus_num_to_idx
            .get(&bus_num)
            .and_then(|&i| network.buses.get(i))
            .map(|b| b.base_kv)
            .unwrap_or(1.0);

        // Vs from RegulatingControl.targetValue (kV) / base_kv when present.
        // Machines with controlEnabled=true still participate in voltage regulation even
        // when the file omits an explicit RegulatingControl; in that case we fall back to
        // a neutral 1.0 pu setpoint.
        let vs = idx.gen_vs(objects, sm, bus_base_kv).unwrap_or(1.0);

        // Pmax/Pmin/qmax/qmin from GeneratingUnit + direct fields
        let gu_id = sm.get_ref("GeneratingUnit");
        let (pmax, pmin) = gu_id
            .and_then(|id| objects.get(id))
            .map(|gu| {
                // Validate GeneratingUnit subclass; warn on unrecognized types.
                const KNOWN_GU_CLASSES: &[&str] = &[
                    "GeneratingUnit", "ThermalGeneratingUnit", "NuclearGeneratingUnit",
                    "HydroGeneratingUnit", "WindGeneratingUnit", "PhotovoltaicGeneratingUnit",
                    "OtherGeneratingUnit", "StorageUnit",
                    "SolarGeneratingUnit", "WaveGeneratingUnit",
                ];
                if !KNOWN_GU_CLASSES.contains(&gu.class.as_str()) {
                    tracing::warn!(
                        sm_id, gu_class = %gu.class,
                        "SynchronousMachine references unrecognized GeneratingUnit subclass; P-limits accepted"
                    );
                }
                let pmax = gu
                    .parse_f64("maxOperatingP")
                    .or_else(|| gu.parse_f64("nominalP"))
                    .unwrap_or(9999.0);
                let pmin = gu.parse_f64("minOperatingP").unwrap_or(0.0);
                // Guard: malformed CGMES may have pmin > pmax; clamp to valid range.
                (pmax.max(pmin), pmin.min(pmax))
            })
            .unwrap_or((9999.0, 0.0));

        // Q-limits: prefer SynchronousMachine.maxQ/minQ (EQ, per-machine design limits).
        // Fall back to GeneratingUnit.maxQ/minQ (some CGMES files only put Q-limits on
        // the GeneratingUnit object rather than the SynchronousMachine). If neither is
        // present, fall back to ReactivePowerLimit from OperationalLimitSet (Wave 17).
        // Last resort: wide defaults (±9999 MVAr = effectively unlimited).
        let gu_obj = gu_id.and_then(|id| objects.get(id));
        let rpl_fallback = idx.eq_reactive_limits.get(sm_id.as_str()).copied();
        let qmax = sm
            .parse_f64("maxQ")
            .or_else(|| gu_obj.and_then(|gu| gu.parse_f64("maxQ")))
            .or_else(|| rpl_fallback.map(|(_, qmax)| qmax))
            .unwrap_or(9999.0);
        let qmin = sm
            .parse_f64("minQ")
            .or_else(|| gu_obj.and_then(|gu| gu.parse_f64("minQ")))
            .or_else(|| rpl_fallback.map(|(qmin, _)| qmin))
            .unwrap_or(-9999.0);

        // Override Q-limits with ReactiveCapabilityCurve when available.
        // The RCC gives the P-dependent capability envelope; interpolate at the SSH
        // P operating point.  p_ssh is in IEC sign convention (negative=generating),
        // which matches CurveData.xvalue — so we can pass it directly.
        let (qmin, qmax) = if let Some(rcc_id) = idx.sm_rcc.get(sm_id.as_str())
            && let Some(pts) = idx.rcc_points.get(rcc_id.as_str())
            && !pts.is_empty()
        {
            let (rcc_qmin, rcc_qmax) = interpolate_rcc(pts, p_ssh);
            tracing::debug!(
                sm_id,
                p_ssh,
                rcc_qmin,
                rcc_qmax,
                "RCC Q-limits applied (overrides static SM.maxQ/minQ)"
            );
            (rcc_qmin, rcc_qmax)
        } else {
            (qmin, qmax)
        };

        let mut g = Generator::new(bus_num, pg, vs);
        g.machine_id = Some(sm_id.clone());
        g.q = qg;
        g.pmax = pmax;
        g.pmin = pmin;
        g.qmax = qmax;
        g.qmin = qmin;
        g.machine_base_mva = base_mva;

        // Populate pq_curve from the full RCC for OPF use.
        // Convention: (p_pu, qmax_pu, qmin_pu) — P in generator sign (positive=generating).
        if let Some(rcc_id) = idx.sm_rcc.get(sm_id.as_str())
            && let Some(pts) = idx.rcc_points.get(rcc_id.as_str())
        {
            g.reactive_capability
                .get_or_insert_with(Default::default)
                .pq_curve = pts
                .iter()
                .map(|&(p_mw, qmin_mvar, qmax_mvar)| {
                    // Flip IEC sign (neg=gen → pos=gen) and normalise to per-unit.
                    let p_pu = (-p_mw) / base_mva;
                    let qmax_pu = qmax_mvar / base_mva;
                    let qmin_pu = qmin_mvar / base_mva;
                    (p_pu, qmax_pu, qmin_pu)
                })
                .collect();
        }

        network.generators.push(g);
    }

    // --- EnergyConsumer / ConformLoad / NonConformLoad → Load + bus Pd/Qd ---
    // All three classes represent loads. ConformLoad and NonConformLoad are used
    // in European grid models (ENTSO-E CGMES) as alternatives to EnergyConsumer.
    // They share the same SSH attributes (p, q) and EQ attributes (pfixed, qfixed).
    let ec_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| {
            matches!(
                o.class.as_str(),
                "EnergyConsumer" | "ConformLoad" | "NonConformLoad"
            )
        })
        .map(|(k, _)| k.clone())
        .collect();

    for ec_id in &ec_ids {
        if idx.disconnected_eq.contains(ec_id.as_str()) {
            continue;
        }
        let ec = &objects[ec_id];

        // Capture both bus_num and the TN mRID (needed for SvInjection lookup).
        let mut tn_for_svinj: Option<String> = None;
        let bus_num = idx
            .terminals(ec_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                let bn = idx.tn_bus(tn)?;
                tn_for_svinj = Some(tn.to_string());
                Some(bn)
            })
            .or_else(|| {
                ec.get_ref("EquipmentContainer").and_then(|vl_id| {
                    idx.tn_ids
                        .iter()
                        .find(|tn_id| {
                            objects
                                .get(tn_id.as_str())
                                .and_then(|o| o.get_ref("ConnectivityNodeContainer"))
                                .map(|c| c == vl_id)
                                .unwrap_or(false)
                        })
                        .and_then(|tn_id| {
                            let bn = idx.tn_bus(tn_id)?;
                            tn_for_svinj = Some(tn_id.clone());
                            Some(bn)
                        })
                })
            });

        let bus_num = match bus_num {
            Some(n) => n,
            None => continue,
        };

        // Lookup priority: (1) SSH p/q, (2) EQ pfixed/qfixed, (3) SvInjection fallback.
        // SvInjection (SV profile, keyed by TopologicalNode mRID) is used as a fallback
        // when SSH p/q and EQ pfixed/qfixed are both absent (e.g. IGM-only CGMES files).
        // NOTE: SvInjection is a nodal total; when multiple loads share a TN, each load
        // may independently fall back to the same SvInjection value — this is intentional
        // since the fallback only triggers when per-equipment SSH values are fully absent.
        let ssh_p = ec.parse_f64("p");
        let ssh_q = ec.parse_f64("q");
        let (sv_p, sv_q) = tn_for_svinj
            .as_deref()
            .and_then(|tn| idx.sv_injections.get(tn))
            .cloned()
            .unwrap_or((None, None));
        let pd = ssh_p
            .or_else(|| ec.parse_f64("pfixed"))
            .or(sv_p)
            .unwrap_or(0.0);
        let qd = ssh_q
            .or_else(|| ec.parse_f64("qfixed"))
            .or(sv_q)
            .unwrap_or(0.0);
        network.loads.push(Load {
            bus: bus_num,
            active_power_demand_mw: pd,
            reactive_power_demand_mvar: qd,
            in_service: true,
            conforming: true,
            id: ec_id.clone(),
            ..Load::new(0, 0.0, 0.0)
        });
    }

    // --- EquivalentInjection → voltage-regulating generator or PQ injection ---
    //
    // CGMES IEC 61970-301 §38: EquivalentInjection models an equivalent of an
    // external network or subsystem. Like SynchronousMachine it inherits from
    // RegulatingCondEq; when `controlEnabled=true` (EQ) AND `regulationStatus=true`
    // (SSH), the ENI is voltage-regulating and should be modeled as a PV generator
    // so the Newton-Raphson can hold bus voltage.  When either flag is false (or
    // absent), fall back to a fixed P/Q injection (PQ treatment).
    //
    // Q-limits from EQ `maxQ`/`minQ` (MVAr); voltage setpoint from
    // RegulatingControl.targetValue (kV) via the shared gen_vs() helper.
    let ei_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "EquivalentInjection")
        .map(|(k, _)| k.clone())
        .collect();

    for ei_id in &ei_ids {
        if idx.disconnected_eq.contains(ei_id.as_str()) {
            continue;
        }
        let ei = &objects[ei_id];
        let bus_num = idx.terminals(ei_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let bus_num = match bus_num {
            Some(n) => n,
            None => continue,
        };

        // SSH p/q: positive = injection into network.
        let p = ei.parse_f64("p").unwrap_or(0.0);
        let q = ei.parse_f64("q").unwrap_or(0.0);
        let base_kv = bus_num_to_idx
            .get(&bus_num)
            .and_then(|&i| network.buses.get(i))
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
            .max(1e-3);

        // Voltage-regulation flags: controlEnabled (EQ) + regulationStatus (SSH).
        let control_enabled = ei
            .get_text("controlEnabled")
            .map(|s| s == "true")
            .unwrap_or(false);
        let regulation_status = ei
            .get_text("regulationStatus")
            .map(|s| s == "true")
            .unwrap_or(false);
        let target_voltage_kv = idx.gen_vs(objects, ei, base_kv).map(|vs| vs * base_kv);
        let min_q_mvar = ei.parse_f64("minQ");
        let max_q_mvar = ei.parse_f64("maxQ");

        network.cim.cgmes_roundtrip.equivalent_injections.insert(
            ei_id.clone(),
            CgmesEquivalentInjectionSource {
                mrid: ei_id.clone(),
                name: ei.get_text("name").map(str::to_string),
                bus: bus_num,
                p_mw: p,
                q_mvar: q,
                in_service: true,
                control_enabled,
                regulation_status,
                target_voltage_kv,
                min_q_mvar,
                max_q_mvar,
            },
        );

        if control_enabled && regulation_status {
            // Voltage-regulating ENI: model as a generator (PV bus).
            let vs = idx.gen_vs(objects, ei, base_kv).unwrap_or(1.0);
            let qmax = max_q_mvar.unwrap_or(9999.0);
            let qmin = min_q_mvar.unwrap_or(-9999.0);
            let mut ei_gen = Generator::new(bus_num, p, vs);
            ei_gen.machine_id = Some(ei_id.clone());
            ei_gen.q = q;
            ei_gen.qmax = qmax;
            ei_gen.qmin = qmin;
            ei_gen.pmax = p.abs().max(9999.0);
            ei_gen.pmin = -p.abs();
            ei_gen.machine_base_mva = base_mva;
            network.generators.push(ei_gen);
            tracing::debug!(
                ei_id,
                bus_num,
                p,
                q,
                vs,
                qmin,
                qmax,
                "EquivalentInjection voltage-regulating → generator (PV bus)"
            );
        } else {
            // Non-regulating: fixed P/Q injection at bus (PQ treatment).
            push_fixed_injection(
                network,
                bus_num,
                ei_id,
                PowerInjectionKind::Equivalent,
                p,
                q,
            );
            tracing::debug!(ei_id, bus_num, p, q, "EquivalentInjection PQ injection");
        }
    }

    // --- EnergySource → voltage-regulating generator or PQ injection ---
    //
    // EnergySource is used in some CGMES files to represent small generators, battery
    // inverters, or mixed injection/consumption elements.
    //
    // SSH activePower convention: positive = consuming from network (load), negative = injection.
    //
    // When `controlEnabled=true` (EQ), EnergySource is voltage-regulating: model it as a
    // generator (PV bus) with Q limits from maxQ/minQ, mirroring SynchronousMachine motor
    // handling. Voltage setpoint from RegulatingControl.targetValue (kV).
    //
    // When controlEnabled is absent or false: fixed P/Q injection → adjust bus pd/qd.
    let es_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "EnergySource")
        .map(|(k, _)| k.clone())
        .collect();

    for es_id in &es_ids {
        let es = &objects[es_id];
        let bus_num = idx
            .terminals(es_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                es.get_ref("EquipmentContainer").and_then(|vl_id| {
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
            None => continue,
        };

        let p = es.parse_f64("activePower").unwrap_or(0.0);
        let q = es.parse_f64("reactivePower").unwrap_or(0.0);

        let control_enabled = es
            .get_text("controlEnabled")
            .map(|s| s == "true")
            .unwrap_or(false);

        if control_enabled {
            // Voltage-regulating EnergySource: model as a generator (PV bus).
            // activePower sign convention: negative = generating, so pg = -p
            let base_kv = bus_num_to_idx
                .get(&bus_num)
                .and_then(|&i| network.buses.get(i))
                .map(|b| b.base_kv)
                .unwrap_or(1.0)
                .max(1e-3);
            let vs = idx.gen_vs(objects, es, base_kv).unwrap_or(1.0);
            let qmax = es.parse_f64("maxQ").unwrap_or(9999.0);
            let qmin = es.parse_f64("minQ").unwrap_or(-9999.0);
            let pg = -p; // negate: activePower < 0 (generating) → pg > 0
            let mut es_gen = Generator::new(bus_num, pg, vs);
            es_gen.machine_id = Some(es_id.clone());
            es_gen.q = -q;
            es_gen.qmax = qmax;
            es_gen.qmin = qmin;
            es_gen.pmax = pg.max(9999.0);
            es_gen.pmin = pg.min(0.0);
            es_gen.machine_base_mva = base_mva;
            network.generators.push(es_gen);
            tracing::debug!(
                es_id,
                bus_num,
                pg,
                vs,
                qmin,
                qmax,
                "EnergySource voltage-regulating → generator (PV bus)"
            );
        } else {
            // Non-regulating: fixed P/Q at bus (PQ treatment).
            // activePower: positive = consuming from network, negative = injecting.
            // Preserve the device explicitly so node-breaker retopology can move it.
            push_fixed_injection(network, bus_num, es_id, PowerInjectionKind::Other, -p, -q);
            tracing::debug!(es_id, bus_num, p, q, "EnergySource PQ injection");
        }
    }

    // --- DC network topology (multi-terminal DC grids) ---
    //
    // When CGMES data contains DCNode/DCTopologicalNode objects with converters
    // that have resolvable DC terminals, build the full DC network model
    // (DcBus/DcConverterStation/DcBranch) for joint AC-DC OPF.
    // Converters handled by the DC network model are returned in a set so we
    // skip them in the PQ injection loop below (avoids double-counting).
    let dc_handled_convs = build_dc_network(objects, idx, network, &bus_num_to_idx);

    // --- VsConverter / CsConverter (HVDC converters) → injection at AC terminal ---
    //
    // HVDC converters inject/absorb active and reactive power at their AC point of
    // common coupling (PCC). The SSH profile provides:
    //   - ACDCConverter.p / ACDCConverter.q : actual operating point (MW/MVAr)
    //   - targetPpcc                         : active power setpoint at PCC (MW)
    //   - VsConverter.targetQpcc             : reactive power setpoint at PCC (MVAr)
    //
    // Positive p/q = injection into network (same convention as EquivalentInjection).
    // The AC terminal is the first Terminal of the converter (sequenceNumber=1 or
    // whichever connects to an AC TopologicalNode, not a DCNode).
    //
    // Voltage-regulating VsConverters (qPccControl=voltagePcc) hold the PCC bus
    // voltage at targetUpcc (kV) by controlling reactive power.  We model these as
    // Generator objects so the NR engine treats the bus as PV.  Non-regulating
    // converters and CsConverters are modelled as PQ injections (subtract from pd/qd).
    let conv_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| matches!(o.class.as_str(), "VsConverter" | "CsConverter"))
        .map(|(k, _)| k.clone())
        .collect();

    for conv_id in &conv_ids {
        // Skip converters already handled by the DC network model.
        if dc_handled_convs.contains(conv_id) {
            continue;
        }
        let conv = &objects[conv_id];
        // Resolve AC terminal: use the Terminal (not ACDCConverterDCTerminal) that connects
        // to an AC TopologicalNode (has a tn_bus mapping).
        let bus_num = idx.terminals(conv_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let bus_num = match bus_num {
            Some(n) => n,
            None => continue,
        };

        // Prefer actual operating point (p/q) over setpoints.
        // Wave 17: fall back to HvdcLine.activePowerSetpoint when SSH p and targetPpcc
        // are both absent. The HvdcLine is found via DC topology traversal (conv_hvdc).
        let hvdc_p_fallback = idx
            .conv_hvdc
            .get(conv_id.as_str())
            .and_then(|hvdc_id| idx.hvdc_line_params.get(hvdc_id.as_str()))
            .and_then(|&(p, _, _)| p);
        // SSH ACDCConverter.p is the measured AC-side power and already includes losses.
        // When it is absent, targetPpcc is the scheduled setpoint (DC-side equivalent),
        // so we add converter losses: P_ac = P_target + sign(P) × (idleLoss + switchingLoss × |P|).
        // idleLoss (MW): no-load fixed loss.  switchingLoss (pu of rated power): proportional loss.
        let idle_loss = super::indices::parse_optional_f64(conv, "idleLoss").unwrap_or(0.0);
        let switching_loss =
            super::indices::parse_optional_f64(conv, "switchingLoss").unwrap_or(0.0);
        let p = super::indices::parse_optional_f64(conv, "p")
            .or_else(|| {
                super::indices::parse_optional_f64(conv, "targetPpcc").map(|target| {
                    let sign = if target >= 0.0 { 1.0 } else { -1.0 };
                    target + sign * (idle_loss + switching_loss * target.abs())
                })
            })
            .or(hvdc_p_fallback)
            .unwrap_or(0.0);
        let q = super::indices::parse_optional_f64(conv, "q")
            .or_else(|| super::indices::parse_optional_f64(conv, "targetQpcc"))
            .unwrap_or(0.0);

        let has_dc_topology = idx.conv_to_dcnode.contains_key(conv_id.as_str());
        if conv.class == "VsConverter" && has_dc_topology {
            tracing::debug!(
                conv_id,
                bus_num,
                "CGMES converter has partial DC topology but was not normalized into a DcGrid; falling back to AC-side PQ injection"
            );
        }
        // CsConverter (LCC thyristor) and any unresolved VsConverter are modeled as
        // plain PQ injections on the AC side.
        push_fixed_injection(
            network,
            bus_num,
            conv_id,
            PowerInjectionKind::Converter,
            p,
            q,
        );
    }

    // --- AsynchronousMachine → PQ load (induction motor) ---
    //
    // CGMES IEC 61970-301 §17: AsynchronousMachine represents an induction motor
    // (or generator in some wind turbine configurations). For steady-state power
    // flow purposes, motors consuming power are modelled as PQ loads.
    // SSH attributes: p (MW), q (MVAr) — positive = consuming from network (motor).
    // This matches EnergyConsumer convention; no sign flip needed.
    let am_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "AsynchronousMachine")
        .map(|(k, _)| k.clone())
        .collect();

    for am_id in &am_ids {
        if idx.disconnected_eq.contains(am_id.as_str()) {
            continue;
        }
        let am = &objects[am_id];
        let bus_num = idx
            .terminals(am_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                am.get_ref("EquipmentContainer").and_then(|vl_id| {
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
            None => continue,
        };

        // SSH p/q: positive = consuming (motor). Same convention as EnergyConsumer.
        let pd = am.parse_f64("p").unwrap_or(0.0);
        let qd = am.parse_f64("q").unwrap_or(0.0);
        network.loads.push(Load {
            bus: bus_num,
            active_power_demand_mw: pd,
            reactive_power_demand_mvar: qd,
            in_service: true,
            conforming: true,
            id: am_id.clone(),
            ..Load::new(0, 0.0, 0.0)
        });
        tracing::debug!(
            am_id,
            bus_num,
            pd,
            qd,
            "AsynchronousMachine added as PQ load"
        );
    }

    // --- ExternalNetworkInjection → P/Q injection at boundary bus ---
    //
    // CGMES IEC 61970-301 §41: ExternalNetworkInjection represents the external
    // network connection at a boundary bus. It is the standard ENTSO-E slack
    // indicator (referencePriority). The SSH profile also provides p/q operating
    // points. We inject these into the bus so the power balance is correct regardless
    // of whether this machine is the slack bus.
    // SSH convention: p > 0 = generation (injection into network → subtract from pd).
    // Note: the slack role is handled separately in assign_slack(); here we only
    // process the P/Q injection.
    let eni_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "ExternalNetworkInjection")
        .map(|(k, _)| k.clone())
        .collect();

    for eni_id in &eni_ids {
        if idx.disconnected_eq.contains(eni_id.as_str()) {
            continue;
        }
        let eni = &objects[eni_id];
        let bus_num = idx.terminals(eni_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let bus_num = match bus_num {
            Some(n) => n,
            None => continue,
        };

        // SSH p/q: positive = generation = injection into network.
        let p = eni.parse_f64("p").unwrap_or(0.0);
        let q = eni.parse_f64("q").unwrap_or(0.0);
        let base_kv = bus_num_to_idx
            .get(&bus_num)
            .and_then(|&i| network.buses.get(i))
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
            .max(1e-3);
        let reference_priority = eni
            .get_text("referencePriority")
            .and_then(|s| s.parse::<u32>().ok());
        let control_enabled = eni
            .get_text("controlEnabled")
            .map(|s| s == "true")
            .unwrap_or(false);
        let regulation_status = eni
            .get_text("regulationStatus")
            .map(|s| s == "true")
            .unwrap_or(false);
        let target_voltage_kv = idx.gen_vs(objects, eni, base_kv).map(|vs| vs * base_kv);

        network
            .cim
            .cgmes_roundtrip
            .external_network_injections
            .insert(
                eni_id.clone(),
                CgmesExternalNetworkInjectionSource {
                    mrid: eni_id.clone(),
                    name: eni.get_text("name").map(str::to_string),
                    bus: bus_num,
                    p_mw: p,
                    q_mvar: q,
                    in_service: true,
                    reference_priority,
                    control_enabled,
                    regulation_status,
                    target_voltage_kv,
                    min_q_mvar: eni.parse_f64("minQ"),
                    max_q_mvar: eni.parse_f64("maxQ"),
                },
            );

        if p.abs() > 1e-9 || q.abs() > 1e-9 {
            push_fixed_injection(network, bus_num, eni_id, PowerInjectionKind::Boundary, p, q);
            tracing::debug!(
                eni_id,
                bus_num,
                p,
                q,
                "ExternalNetworkInjection P/Q applied"
            );
        }
    }

    // --- StationSupply → auxiliary station load ---
    //
    // CGMES IEC 61970-301 §14: StationSupply represents auxiliary power consumption
    // at a substation (e.g., control systems, pumps, heating). It is a specialisation
    // of EnergyConsumer with the same SSH p/q attributes. Treat as a PQ load.
    let ss_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "StationSupply")
        .map(|(k, _)| k.clone())
        .collect();

    for ss_id in &ss_ids {
        if idx.disconnected_eq.contains(ss_id.as_str()) {
            continue;
        }
        let ss = &objects[ss_id];
        let bus_num = idx
            .terminals(ss_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(objects, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                ss.get_ref("EquipmentContainer").and_then(|vl_id| {
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
            None => continue,
        };

        let pd = ss
            .parse_f64("p")
            .or_else(|| ss.parse_f64("pfixed"))
            .unwrap_or(0.0);
        let qd = ss
            .parse_f64("q")
            .or_else(|| ss.parse_f64("qfixed"))
            .unwrap_or(0.0);
        network.loads.push(Load {
            bus: bus_num,
            active_power_demand_mw: pd,
            reactive_power_demand_mvar: qd,
            in_service: true,
            conforming: true,
            id: ss_id.clone(),
            ..Load::new(0, 0.0, 0.0)
        });
        tracing::debug!(ss_id, bus_num, pd, qd, "StationSupply added as PQ load");
    }

    // --- FrequencyConverter → coupled P/Q injection pair (Wave 23) ---
    //
    // CGMES IEC 61970-301: FrequencyConverter is an AC-to-AC frequency converter
    // connecting two AC systems at different frequencies (e.g. 50 Hz ↔ 60 Hz).
    // It has exactly two Terminals, each at a different bus (AC system).
    //
    // Power-flow model: ideal P transfer (P_in ≈ P_out, losses neglected).
    // The device injects P+Q at each AC terminal independently:
    //   Bus 1: load with P_import (consuming P from system 1)
    //   Bus 2: generator with P_export (injecting P into system 2)
    // Q is typically 0 (no reactive exchange in balanced steady-state) unless
    // the file provides a RegulatingControl target.
    //
    // SSH attributes:
    //   FrequencyConverter.operatingMode: string enum ("rectifier"/"inverter")
    //   p (MW) and q (MVAr) on each Terminal from SSH RegulatingControl or SvPowerFlow.
    let fc_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "FrequencyConverter")
        .map(|(k, _)| k.clone())
        .collect();

    for fc_id in &fc_ids {
        if idx.disconnected_eq.contains(fc_id.as_str()) {
            continue;
        }
        let terms = idx.terminals(fc_id);
        if terms.len() < 2 {
            continue;
        }
        // Resolve both AC terminals to buses.
        let bus1 = terms
            .first()
            .and_then(|t| idx.terminal_tn(objects, t).and_then(|tn| idx.tn_bus(tn)));
        let bus2 = terms
            .get(1)
            .and_then(|t| idx.terminal_tn(objects, t).and_then(|tn| idx.tn_bus(tn)));
        let (Some(bus1), Some(bus2)) = (bus1, bus2) else {
            continue;
        };

        let fc_obj = &objects[fc_id];
        // Use direct p/q if available; fall back to 0.
        let p_mw = fc_obj.parse_f64("p").unwrap_or(0.0);
        let q_mvar = fc_obj.parse_f64("q").unwrap_or(0.0);

        // Model as: bus1 absorbs P (load), bus2 injects P (generator).
        // This approximates the AC-AC conversion without modelling losses.
        if p_mw.abs() > 1e-6 {
            network.loads.push(Load {
                bus: bus1,
                active_power_demand_mw: p_mw,
                reactive_power_demand_mvar: q_mvar,
                in_service: true,
                conforming: true,
                id: fc_id.clone(),
                ..Load::new(0, 0.0, 0.0)
            });

            // Generator on bus2 injects the same P (lossless approximation).
            use surge_network::network::Generator;
            let mut fc_gen = Generator::new(bus2, p_mw, 0.0);
            fc_gen.machine_id = Some(fc_id.clone());
            fc_gen.p = p_mw;
            fc_gen.q = -q_mvar; // mirror Q balance
            fc_gen.in_service = true;
            network.generators.push(fc_gen);
        }
        tracing::debug!(
            fc_id,
            bus1,
            bus2,
            p_mw,
            q_mvar,
            "FrequencyConverter: load on bus1, generator on bus2 (Wave 23)"
        );
    }

    // --- PowerElectronicsConnection → Generator/Load (CGMES 3.0) ---
    //
    // CGMES 3.0 (CIM100) introduces `PowerElectronicsConnection` (PEC) as the AC-side
    // interface for inverter-based resources (IBR): battery storage (BatteryUnit),
    // solar PV (PhotovoltaicUnit), and wind (WindGeneratingUnit via PEC instead of SM).
    //
    // Each PEC has:
    //   - Terminals → AC bus connection
    //   - `PowerElectronicsUnit` refs → BatteryUnit/PhotovoltaicUnit (DC side)
    //   - SSH `p` (MW), `q` (MVAr) setpoints (convention: p > 0 = injection into grid)
    //   - `maxP` / `minP` (EQ): active power limits (positive for generators)
    //   - `ratedS` (VA): rated apparent power
    //
    // Power-flow model: PEC with p > 0 (discharging battery / generating PV) → Generator.
    //   PEC with p < 0 (charging battery) → Load (|p|, |q| as PQ load on the bus).
    //   PEC with p ≈ 0 → skip.
    //
    // NOTE: BatteryUnit.ratedE and storedE are metadata (energy capacity / SoC in MWh).
    // They do not affect the steady-state PF; stored for future dispatch/UC use.
    // Build reverse map: PEC mRID → GenType from associated PowerElectronicsUnit subclass.
    // PowerElectronicsUnit.PowerElectronicsConnection references the parent PEC.
    let pec_gen_type: HashMap<String, GenType> = {
        let mut m = HashMap::new();
        for (id, obj) in objects.iter() {
            let gt = match obj.class.as_str() {
                "PhotovoltaicUnit" => Some(GenType::Solar),
                "WindGeneratingUnit" => Some(GenType::Wind),
                "BatteryUnit" => Some(GenType::InverterOther),
                "PowerElectronicsUnit" => Some(GenType::InverterOther),
                _ => None,
            };
            if let Some(gt) = gt {
                if let Some(pec_id) = obj.get_ref("PowerElectronicsConnection") {
                    m.insert(pec_id.to_string(), gt);
                }
                // Some models store the reference under EquipmentContainer
                if let Some(pec_id) = obj.get_ref("EquipmentContainer") {
                    // Only insert if the EquipmentContainer actually points to a PEC
                    if let Some(pec_obj) = objects.get(pec_id)
                        && pec_obj.class == "PowerElectronicsConnection"
                    {
                        m.entry(pec_id.to_string()).or_insert(gt);
                    }
                }
                // Also try matching by mRID suffix for BatteryUnit with inline PEC ref
                let _ = id; // suppress unused warning
            }
        }
        m
    };

    let pec_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "PowerElectronicsConnection")
        .map(|(k, _)| k.clone())
        .collect();

    for pec_id in &pec_ids {
        if idx.disconnected_eq.contains(pec_id.as_str()) {
            continue;
        }
        let pec = &objects[pec_id];
        let bus_num = idx.terminals(pec_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let bus_num = match bus_num {
            Some(n) => n,
            None => {
                tracing::warn!(
                    pec_id,
                    "PowerElectronicsConnection: no terminal bus found; skipping"
                );
                continue;
            }
        };

        // SSH p/q: positive = injection into grid (generation).
        let p_mw = pec.parse_f64("p").unwrap_or(0.0);
        let q_mvar = pec.parse_f64("q").unwrap_or(0.0);

        // EQ power limits: maxP/minP on the PEC itself (positive convention).
        let p_max = pec.parse_f64("maxP").unwrap_or(f64::MAX);
        let p_min = pec.parse_f64("minP").unwrap_or(0.0);

        // Voltage setpoint: look up RegulatingControl.targetValue (kV) for the PEC.
        // RegulatingCondEq.RegulatingControl → RegulatingControl → targetValue (kV).
        let v_set_kv = pec
            .get_ref("RegulatingControl")
            .and_then(|rc_id| idx.rc_target_kv.get(rc_id).copied());
        // Convert kV to pu using bus base_kv; fall back to 1.0 pu if absent.
        let base_kv = bus_num_to_idx
            .get(&bus_num)
            .map(|&i| network.buses[i].base_kv)
            .unwrap_or(1.0);
        let v_set = if let Some(kv) = v_set_kv {
            if base_kv > 0.0 { kv / base_kv } else { 1.0 }
        } else {
            1.0
        };

        if p_mw.abs() < 1e-6 && q_mvar.abs() < 1e-6 {
            tracing::debug!(
                pec_id,
                bus_num,
                "PowerElectronicsConnection: p≈0,q≈0; skipping"
            );
            continue;
        }

        if p_mw >= 0.0 {
            // Generating (discharging battery, PV/wind producing) → Generator.
            use surge_network::network::Generator;
            let mut pec_gen = Generator::new(bus_num, p_mw, v_set);
            pec_gen.machine_id = Some(pec_id.clone());
            pec_gen.q = q_mvar;
            pec_gen.pmax = p_max;
            pec_gen.pmin = p_min;
            pec_gen.in_service = true;
            // Set gen_type from associated PowerElectronicsUnit subclass
            pec_gen.gen_type = pec_gen_type
                .get(pec_id.as_str())
                .copied()
                .unwrap_or(GenType::InverterOther);
            if let Some(&i) = bus_num_to_idx.get(&bus_num) {
                let b = &mut network.buses[i];
                if b.bus_type == BusType::PQ {
                    b.bus_type = BusType::PV;
                    b.voltage_magnitude_pu = v_set;
                }
            }
            network.generators.push(pec_gen);
            tracing::debug!(
                pec_id,
                bus_num,
                p_mw,
                q_mvar,
                "PowerElectronicsConnection → Generator (CGMES 3.0)"
            );
        } else {
            // Consuming (charging battery) → PQ load.
            let pd = -p_mw; // load convention: pd > 0
            let qd = -q_mvar;
            network.loads.push(Load {
                bus: bus_num,
                active_power_demand_mw: pd,
                reactive_power_demand_mvar: qd,
                in_service: true,
                conforming: true,
                id: pec_id.clone(),
                ..Load::new(0, 0.0, 0.0)
            });
            tracing::debug!(
                pec_id,
                bus_num,
                pd,
                qd,
                "PowerElectronicsConnection → Load/charging (CGMES 3.0)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Stage 5 — Assign slack bus
// ---------------------------------------------------------------------------

pub(crate) fn assign_slack(objects: &ObjMap, idx: &CgmesIndices, network: &mut Network) {
    // Already has a slack? Done.
    if network.buses.iter().any(|b| b.bus_type == BusType::Slack) {
        return;
    }

    // Mark generator buses as PV — only if the generator is voltage-regulating
    // AND has reactive power range.  A machine with qmax == qmin cannot regulate
    // voltage; a machine with voltage_regulated=false does not participate.
    let gen_buses_pv: std::collections::HashSet<u32> = network
        .generators
        .iter()
        .filter(|g| g.in_service && g.voltage_regulated && (g.qmax - g.qmin) > 1e-6)
        .map(|g| g.bus)
        .collect();
    for bus in network.buses.iter_mut() {
        if gen_buses_pv.contains(&bus.number) {
            bus.bus_type = BusType::PV;
        }
    }

    // Try lowest non-zero referencePriority from EQ (ENTSO-E slack indicator).
    // referencePriority is an EQ attribute (design-time slack designation); the merged
    // object map correctly carries it even when the SSH profile is also loaded.
    // Both SynchronousMachine and ExternalNetworkInjection can serve as slack.
    // ExternalNetworkInjection is the standard slack marker in IGM files where
    // the slack is the external grid connection rather than a local generator.
    //
    // When multiple candidates share the same minimum priority (all valid per CGMES),
    // we break ties deterministically: (1) highest base_kv bus, (2) lowest bus number.
    // This matches the PV-bus fallback convention and produces stable results regardless
    // of HashMap iteration order.
    let slack_eq = {
        let candidates: Vec<(u32, String)> = objects
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o.class.as_str(),
                    "SynchronousMachine" | "ExternalNetworkInjection"
                )
            })
            .filter_map(|(id, o)| {
                let p = o
                    .get_text("referencePriority")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                if p > 0 { Some((p, id.clone())) } else { None }
            })
            .collect();

        let min_p = candidates.iter().map(|(p, _)| *p).min();
        if let Some(min_p) = min_p {
            // Among equal-priority candidates pick by (highest base_kv, lowest bus_num).
            candidates
                .into_iter()
                .filter(|(p, _)| *p == min_p)
                .filter_map(|(_, id)| {
                    let bus_num = idx.terminals(&id).iter().find_map(|tid| {
                        let tn = idx.terminal_tn(objects, tid)?;
                        idx.tn_bus(tn)
                    })?;
                    let base_kv = network
                        .buses
                        .iter()
                        .find(|b| b.number == bus_num)
                        .map(|b| b.base_kv)
                        .unwrap_or(0.0);
                    Some((id, bus_num, base_kv))
                })
                .max_by(|(_, bn_a, kv_a), (_, bn_b, kv_b)| {
                    kv_a.total_cmp(kv_b).then_with(|| bn_b.cmp(bn_a)) // lower bus_num wins → .reverse()
                })
                .map(|(id, _, _)| id)
        } else {
            None
        }
    };

    if let Some(eq_id) = slack_eq {
        let bus_num = idx.terminals(&eq_id).iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        if let Some(n) = bus_num
            && let Some(bus) = network.buses.iter_mut().find(|b| b.number == n)
        {
            bus.bus_type = BusType::Slack;
            tracing::debug!(
                eq_id,
                bus_num = n,
                base_kv = bus.base_kv,
                "CGMES slack bus selected via referencePriority"
            );
            // For ExternalNetworkInjection, apply voltage setpoint from
            // RegulatingControl.targetValue (kV) if present.
            if let Some(eq_obj) = objects.get(eq_id.as_str())
                && eq_obj.class == "ExternalNetworkInjection"
            {
                let base_kv = bus.base_kv.max(1e-3);
                if let Some(vs) = idx.gen_vs(objects, eq_obj, base_kv) {
                    bus.voltage_magnitude_pu = vs;
                }
            }
            return;
        }
    }

    // Fallback: PV bus with highest base_kv (the largest transmission voltage is the
    // best slack candidate).  Among equal-kV buses, lower bus number wins (deterministic,
    // matches MATPOWER convention: bus 1 or the lowest-numbered high-kV bus is often slack).
    // Select PV bus with highest base_kv as slack; break ties by lowest bus number
    // (lower bus number = standard slack convention, matching MATPOWER case1/bus 1).
    // NOTE: the secondary sort must use ascending order (a < b → a wins) so that the
    // lowest-numbered bus is chosen when multiple equal-kV buses exist.
    let best_pv = network
        .buses
        .iter()
        .enumerate()
        .filter(|(_, b)| b.bus_type == BusType::PV)
        .max_by(|(_, a), (_, b)| {
            a.base_kv
                .partial_cmp(&b.base_kv)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.number.cmp(&b.number).reverse())
        })
        .map(|(i, _)| i);
    if let Some(idx) = best_pv {
        let bus = &network.buses[idx];
        tracing::debug!(
            bus_num = bus.number,
            base_kv = bus.base_kv,
            "CGMES slack bus selected via highest-kV PV bus (no referencePriority found)"
        );
        network.buses[idx].bus_type = BusType::Slack;
        return;
    }

    // Last resort: bus 1 (no PV buses found — network may have no voltage-regulating
    // generators or all generators have zero Q-range; log a warning so the user
    // knows the slack assignment may be suboptimal).
    if !network.buses.is_empty() {
        let bus0 = &network.buses[0];
        tracing::warn!(
            bus_num = bus0.number,
            bus_name = %bus0.name,
            "CGMES: no PV buses found — no generators with Q-range. \
             Assigning bus {} as PQ-type slack (last resort). \
             Check generator minQ/maxQ attributes in EQ profile.",
            bus0.number,
        );
        network.buses[0].bus_type = BusType::Slack;
    }
}
