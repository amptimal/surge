// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES Protection Equipment parser (IEC 61970-302 Protection package).
//!
//! Parses CIM protection classes:
//! - **CurrentRelay** — overcurrent relay settings (phase, ground, neg-seq)
//! - **DistanceRelay** — distance/impedance relay zone settings
//! - **RecloseSequence** — auto-reclose shot sequences
//! - **SynchrocheckRelay** — synchrocheck relay settings
//! - **ProtectionEquipment** — generic relay base class
//!
//! Terminal references are resolved to bus numbers via `CgmesIndices`.
//! RecloseSequence shots are grouped by their ProtectedSwitch mRID.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::protection::{
    CurrentRelaySettings, DistanceRelaySettings, ProtectionData, RecloseSequenceData, RecloseShot,
    SynchrocheckSettings,
};

use super::indices::CgmesIndices;
use super::types::ObjMap;

/// Resolve the bus number for a protection equipment object.
///
/// Looks up the first Terminal reference and resolves it to a TN → bus number.
fn resolve_bus(objects: &ObjMap, idx: &CgmesIndices, eq_id: &str) -> Option<u32> {
    idx.terminals(eq_id).iter().find_map(|tid| {
        let tn = idx.terminal_tn(objects, tid)?;
        idx.tn_bus(tn)
    })
}

/// Build protection equipment data from CGMES objects and attach to Network.
pub(crate) fn build_protection_data(objects: &ObjMap, idx: &CgmesIndices, network: &mut Network) {
    let mut data = ProtectionData::default();

    // --- CurrentRelay ---
    for (id, obj) in objects.iter().filter(|(_, o)| o.class == "CurrentRelay") {
        let name = obj.get_text("name").unwrap_or_default().to_string();
        let bus = resolve_bus(objects, idx, id);
        let protected_switch_mrid = obj.get_ref("ProtectedSwitch").map(|s| s.to_string());
        let inverse_time = obj
            .get_text("inverseTimeFlag")
            .map(|s| s == "true")
            .unwrap_or(false);
        let directional = obj
            .get_text("powerDirectionFlag")
            .map(|s| s == "true")
            .unwrap_or(false);

        data.current_relays.push(CurrentRelaySettings {
            mrid: id.clone(),
            name,
            phase_pickup_a: obj.parse_f64("currentLimit1"),
            ground_pickup_a: obj.parse_f64("currentLimit2"),
            neg_seq_pickup_a: obj.parse_f64("currentLimit3"),
            phase_time_dial_s: obj.parse_f64("timeDelay1"),
            ground_time_dial_s: obj.parse_f64("timeDelay2"),
            neg_seq_time_dial_s: obj.parse_f64("timeDelay3"),
            inverse_time,
            directional,
            bus,
            protected_switch_mrid,
        });
    }

    // --- DistanceRelay ---
    for (id, obj) in objects.iter().filter(|(_, o)| o.class == "DistanceRelay") {
        let name = obj.get_text("name").unwrap_or_default().to_string();
        let bus = resolve_bus(objects, idx, id);
        let protected_switch_mrid = obj.get_ref("ProtectedSwitch").map(|s| s.to_string());

        data.distance_relays.push(DistanceRelaySettings {
            mrid: id.clone(),
            name,
            forward_reach_ohm: obj.parse_f64("forwardReach"),
            forward_blind_ohm: obj.parse_f64("forwardBlind"),
            backward_reach_ohm: obj.parse_f64("backwardReach"),
            backward_blind_ohm: obj.parse_f64("backwardBlind"),
            mho_angle_deg: obj.parse_f64("operationPhaseAngle"),
            zero_seq_rx_ratio: obj.parse_f64("zeroSeqRXRatio"),
            zero_seq_reach_ohm: obj.parse_f64("zeroSeqReach"),
            bus,
            protected_switch_mrid,
        });
    }

    // --- RecloseSequence → group by ProtectedSwitch ---
    let mut reclose_map: HashMap<String, Vec<RecloseShot>> = HashMap::new();
    for (_id, obj) in objects.iter().filter(|(_, o)| o.class == "RecloseSequence") {
        let switch_mrid = match obj.get_ref("ProtectedSwitch") {
            Some(s) => s.to_string(),
            None => continue,
        };
        let step = obj.parse_f64("recloseStep").unwrap_or(1.0) as u32;
        let delay_s = obj.parse_f64("recloseDelay").unwrap_or(0.0);
        reclose_map
            .entry(switch_mrid)
            .or_default()
            .push(RecloseShot { step, delay_s });
    }
    for (switch_mrid, mut shots) in reclose_map {
        shots.sort_by_key(|s| s.step);
        data.reclose_sequences.push(RecloseSequenceData {
            protected_switch_mrid: switch_mrid,
            shots,
        });
    }

    // --- SynchrocheckRelay ---
    for (id, obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "SynchrocheckRelay")
    {
        let name = obj.get_text("name").unwrap_or_default().to_string();
        let bus = resolve_bus(objects, idx, id);
        let protected_switch_mrid = obj.get_ref("ProtectedSwitch").map(|s| s.to_string());

        data.synchrocheck_relays.push(SynchrocheckSettings {
            mrid: id.clone(),
            name,
            max_angle_diff_deg: obj.parse_f64("maxAngleDiff"),
            max_freq_diff_hz: obj.parse_f64("maxFreqDiff"),
            max_volt_diff_pu: obj.parse_f64("maxVoltDiff"),
            bus,
            protected_switch_mrid,
        });
    }

    // --- ProtectionEquipment (generic base class) ---
    // Generic relays without a specialized subclass. Map to CurrentRelaySettings
    // using highLimit/lowLimit/relayDelayTime.
    for (id, obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "ProtectionEquipment")
    {
        let name = obj.get_text("name").unwrap_or_default().to_string();
        let bus = resolve_bus(objects, idx, id);
        let protected_switch_mrid = obj.get_ref("ProtectedSwitch").map(|s| s.to_string());
        let directional = obj
            .get_text("powerDirectionFlag")
            .map(|s| s == "true")
            .unwrap_or(false);

        data.current_relays.push(CurrentRelaySettings {
            mrid: id.clone(),
            name,
            phase_pickup_a: obj.parse_f64("highLimit"),
            ground_pickup_a: obj.parse_f64("lowLimit"),
            neg_seq_pickup_a: None,
            phase_time_dial_s: obj.parse_f64("relayDelayTime"),
            ground_time_dial_s: None,
            neg_seq_time_dial_s: None,
            inverse_time: false,
            directional,
            bus,
            protected_switch_mrid,
        });
    }

    if !data.is_empty() {
        tracing::info!(
            current_relays = data.current_relays.len(),
            distance_relays = data.distance_relays.len(),
            reclose_sequences = data.reclose_sequences.len(),
            synchrocheck_relays = data.synchrocheck_relays.len(),
            "Protection equipment parsed from CGMES"
        );
        network.cim.protection_data = data;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgmes::types::{CimObj, CimVal};

    fn text(s: &str) -> CimVal {
        CimVal::Text(s.to_string())
    }

    fn refval(s: &str) -> CimVal {
        CimVal::Ref(s.to_string())
    }

    /// Helper: build a minimal ObjMap + CgmesIndices with a single TN->bus mapping.
    fn make_test_env() -> (ObjMap, CgmesIndices) {
        let mut objects: ObjMap = HashMap::new();

        // Create a TopologicalNode
        let mut tn = CimObj::new("TopologicalNode");
        tn.attrs.insert("name".into(), text("TN1"));
        objects.insert("tn1".into(), tn);

        // Create a Terminal linking equipment to the TN
        let mut term = CimObj::new("Terminal");
        term.attrs.insert("TopologicalNode".into(), refval("tn1"));
        term.attrs
            .insert("ConductingEquipment".into(), refval("relay1"));
        term.attrs.insert("sequenceNumber".into(), text("1"));
        objects.insert("term1".into(), term);

        // Build indices manually
        let mut idx = CgmesIndices::build(&objects);
        // Assign TN -> bus number
        idx.tn_bus.insert("tn1".into(), 1);

        (objects, idx)
    }

    #[test]
    fn test_parse_current_relay() {
        let (mut objects, idx) = make_test_env();

        let mut relay = CimObj::new("CurrentRelay");
        relay.attrs.insert("name".into(), text("OC_Relay_1"));
        relay.attrs.insert("currentLimit1".into(), text("400.0"));
        relay.attrs.insert("currentLimit2".into(), text("100.0"));
        relay.attrs.insert("timeDelay1".into(), text("0.5"));
        relay.attrs.insert("timeDelay2".into(), text("1.0"));
        relay.attrs.insert("inverseTimeFlag".into(), text("true"));
        relay.attrs.insert("ProtectedSwitch".into(), refval("brk1"));
        objects.insert("relay1".into(), relay);

        let mut network = Network::default();
        build_protection_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.protection_data.current_relays.len(), 1);
        let r = &network.cim.protection_data.current_relays[0];
        assert_eq!(r.name, "OC_Relay_1");
        assert_eq!(r.phase_pickup_a, Some(400.0));
        assert_eq!(r.ground_pickup_a, Some(100.0));
        assert_eq!(r.phase_time_dial_s, Some(0.5));
        assert!(r.inverse_time);
        assert_eq!(r.bus, Some(1));
        assert_eq!(r.protected_switch_mrid.as_deref(), Some("brk1"));
    }

    #[test]
    fn test_parse_distance_relay() {
        let (mut objects, idx) = make_test_env();

        let mut relay = CimObj::new("DistanceRelay");
        relay.attrs.insert("name".into(), text("Z_Relay_1"));
        relay.attrs.insert("forwardReach".into(), text("12.5"));
        relay.attrs.insert("backwardReach".into(), text("3.0"));
        relay
            .attrs
            .insert("operationPhaseAngle".into(), text("75.0"));
        relay.attrs.insert("zeroSeqRXRatio".into(), text("2.5"));
        objects.insert("relay1".into(), relay);

        let mut network = Network::default();
        build_protection_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.protection_data.distance_relays.len(), 1);
        let d = &network.cim.protection_data.distance_relays[0];
        assert_eq!(d.forward_reach_ohm, Some(12.5));
        assert_eq!(d.backward_reach_ohm, Some(3.0));
        assert_eq!(d.mho_angle_deg, Some(75.0));
        assert_eq!(d.zero_seq_rx_ratio, Some(2.5));
        assert_eq!(d.bus, Some(1));
    }

    #[test]
    fn test_parse_reclose_sequence() {
        let (mut objects, idx) = make_test_env();

        // Two reclose shots for the same switch
        let mut rs1 = CimObj::new("RecloseSequence");
        rs1.attrs.insert("ProtectedSwitch".into(), refval("brk1"));
        rs1.attrs.insert("recloseStep".into(), text("1"));
        rs1.attrs.insert("recloseDelay".into(), text("0.3"));
        objects.insert("rs1".into(), rs1);

        let mut rs2 = CimObj::new("RecloseSequence");
        rs2.attrs.insert("ProtectedSwitch".into(), refval("brk1"));
        rs2.attrs.insert("recloseStep".into(), text("2"));
        rs2.attrs.insert("recloseDelay".into(), text("15.0"));
        objects.insert("rs2".into(), rs2);

        let mut network = Network::default();
        build_protection_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.protection_data.reclose_sequences.len(), 1);
        let seq = &network.cim.protection_data.reclose_sequences[0];
        assert_eq!(seq.protected_switch_mrid, "brk1");
        assert_eq!(seq.shots.len(), 2);
        assert_eq!(seq.shots[0].step, 1);
        assert!((seq.shots[0].delay_s - 0.3).abs() < 1e-12);
        assert_eq!(seq.shots[1].step, 2);
        assert!((seq.shots[1].delay_s - 15.0).abs() < 1e-12);
    }

    #[test]
    fn test_parse_synchrocheck_relay() {
        let (mut objects, idx) = make_test_env();

        let mut relay = CimObj::new("SynchrocheckRelay");
        relay.attrs.insert("name".into(), text("SC_Relay_1"));
        relay.attrs.insert("maxAngleDiff".into(), text("20.0"));
        relay.attrs.insert("maxFreqDiff".into(), text("0.2"));
        relay.attrs.insert("maxVoltDiff".into(), text("0.1"));
        relay.attrs.insert("ProtectedSwitch".into(), refval("brk2"));
        objects.insert("relay1".into(), relay);

        let mut network = Network::default();
        build_protection_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.protection_data.synchrocheck_relays.len(), 1);
        let sc = &network.cim.protection_data.synchrocheck_relays[0];
        assert_eq!(sc.name, "SC_Relay_1");
        assert_eq!(sc.max_angle_diff_deg, Some(20.0));
        assert_eq!(sc.max_freq_diff_hz, Some(0.2));
        assert_eq!(sc.max_volt_diff_pu, Some(0.1));
        assert_eq!(sc.bus, Some(1));
    }

    #[test]
    fn test_parse_protection_equipment_generic() {
        let (mut objects, idx) = make_test_env();

        let mut pe = CimObj::new("ProtectionEquipment");
        pe.attrs.insert("name".into(), text("GenericRelay"));
        pe.attrs.insert("highLimit".into(), text("500.0"));
        pe.attrs.insert("lowLimit".into(), text("50.0"));
        pe.attrs.insert("relayDelayTime".into(), text("0.1"));
        pe.attrs.insert("powerDirectionFlag".into(), text("true"));
        objects.insert("relay1".into(), pe);

        let mut network = Network::default();
        build_protection_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.protection_data.current_relays.len(), 1);
        let r = &network.cim.protection_data.current_relays[0];
        assert_eq!(r.name, "GenericRelay");
        assert_eq!(r.phase_pickup_a, Some(500.0));
        assert_eq!(r.ground_pickup_a, Some(50.0));
        assert_eq!(r.phase_time_dial_s, Some(0.1));
        assert!(r.directional);
    }

    #[test]
    fn test_empty_protection_data() {
        let (objects, idx) = make_test_env();
        let mut network = Network::default();
        build_protection_data(&objects, &idx, &mut network);

        // No protection equipment -> protection_data stays default/empty
        assert!(network.cim.protection_data.is_empty());
    }

    #[test]
    fn test_reclose_no_switch_skipped() {
        let (mut objects, idx) = make_test_env();

        // RecloseSequence without ProtectedSwitch -> should be skipped
        let mut rs = CimObj::new("RecloseSequence");
        rs.attrs.insert("recloseStep".into(), text("1"));
        rs.attrs.insert("recloseDelay".into(), text("0.5"));
        objects.insert("rs_orphan".into(), rs);

        let mut network = Network::default();
        build_protection_data(&objects, &idx, &mut network);
        assert!(network.cim.protection_data.reclose_sequences.is_empty());
    }
}
