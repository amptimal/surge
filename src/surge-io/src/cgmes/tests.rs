// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES parser tests.

use super::helpers::interpolate_rcc;
use super::types::CimVal;
use super::*;
use std::path::PathBuf;
use surge_network::network::Branch;

/// MED-22: Resolve path to CGMES test data.
///
/// Search order:
///   1. `$SURGE_BENCH_DIR/instances/cgmes/<rel>` (explicit override)
///   2. `<CARGO_MANIFEST_DIR>/tests/data/cgmes/<rel>` (legacy in-tree path, kept as fallback)
///
/// Callers must check `.exists()` and skip the test when the path is absent.
fn test_data(rel: &str) -> PathBuf {
    // 1. Explicit environment override
    if let Ok(bench_dir) = std::env::var("SURGE_BENCH_DIR") {
        let p = PathBuf::from(bench_dir).join("instances/cgmes").join(rel);
        if p.exists() {
            return p;
        }
    }
    // 2. Legacy in-tree path (original location; kept so any checked-in fixtures still work)
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    PathBuf::from(manifest).join("tests/data/cgmes").join(rel)
}

fn glob_profile(dir: &PathBuf, suffix: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().contains(suffix))
}

fn roundtrip_v2_profiles(network: &Network) -> (Profiles, Network) {
    let profiles = to_profiles(network, Version::V2_4_15).expect("profiles should render");
    let dir = tempfile::tempdir().expect("tempdir");
    let eq = dir.path().join("rt_EQ.xml");
    let tp = dir.path().join("rt_TP.xml");
    let ssh = dir.path().join("rt_SSH.xml");
    let sv = dir.path().join("rt_SV.xml");
    std::fs::write(&eq, &profiles.eq).expect("write EQ");
    std::fs::write(&tp, &profiles.tp).expect("write TP");
    std::fs::write(&ssh, &profiles.ssh).expect("write SSH");
    std::fs::write(&sv, &profiles.sv).expect("write SV");

    let paths = [eq, tp, ssh, sv];
    let refs: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
    let reparsed = parse_files(&refs).expect("round-trip parse should succeed");
    (profiles, reparsed)
}

fn insert_obj(map: &mut ObjMap, id: &str, class: &str, attrs: &[(&str, CimVal)]) {
    let mut obj = CimObj::new(class);
    for (key, value) in attrs {
        obj.attrs.insert((*key).to_string(), value.clone());
    }
    map.insert(id.to_string(), obj);
}

// -----------------------------------------------------------------------
// Inline minimal tests (no external files required)
// -----------------------------------------------------------------------

// NOTE: uses r##"..."## so rdf:resource="#ID" doesn't terminate the raw string
const MINIMAL_EQ: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.name>Bus1</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.name>Bus2</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_3">
<cim:TopologicalNode.name>Bus3</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_4">
<cim:TopologicalNode.name>Bus4</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <!-- Line Bus1–Bus2: r=10Ω, x=40Ω at 220kV -->
  <cim:ACLineSegment rdf:ID="LINE_12">
<cim:ACLineSegment.r>10.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>40.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0002</cim:ACLineSegment.bch>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_LINE12_1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_LINE12_2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Line Bus2–Bus3 -->
  <cim:ACLineSegment rdf:ID="LINE_23">
<cim:ACLineSegment.r>8.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>30.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.00015</cim:ACLineSegment.bch>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_LINE23_1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_23"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_LINE23_2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_23"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_3"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Transformer Bus3(220kV) → Bus4(110kV), nominal: tap=1.0 in MATPOWER pu convention -->
  <cim:PowerTransformer rdf:ID="XFMR_34">
  </cim:PowerTransformer>
  <cim:PowerTransformerEnd rdf:ID="END_34_1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_34"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:PowerTransformerEnd.r>0.5</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>5.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END_34_2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_34"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_110"/>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="T_XFMR34_1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_34"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_3"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_XFMR34_2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_34"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_4"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Generator on Bus1 -->
  <cim:GeneratingUnit rdf:ID="GU_1">
<cim:GeneratingUnit.maxOperatingP>200.0</cim:GeneratingUnit.maxOperatingP>
<cim:GeneratingUnit.minOperatingP>10.0</cim:GeneratingUnit.minOperatingP>
  </cim:GeneratingUnit>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:RotatingMachine.GeneratingUnit rdf:resource="#GU_1"/>
<cim:RotatingMachine.p>-150.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-30.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Load on Bus3 -->
  <cim:EnergyConsumer rdf:ID="EC_3">
<cim:EnergyConsumer.p>100.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>40.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC3">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_3"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_3"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Shunt at Bus4: b=0.02 S at 110kV → b_pu = 0.02*110²/100 = 2.42 pu -->
  <cim:LinearShuntCompensator rdf:ID="SHC_4">
<cim:ShuntCompensator.normalSections>1</cim:ShuntCompensator.normalSections>
<cim:LinearShuntCompensator.bPerSection>0.02</cim:LinearShuntCompensator.bPerSection>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_110"/>
  </cim:LinearShuntCompensator>
  <cim:Terminal rdf:ID="T_SHC4">
<cim:Terminal.ConductingEquipment rdf:resource="#SHC_4"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_4"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;

/// Minimal 3-winding transformer CGMES dataset:
///   TN_HV (400 kV) → winding 1 → star (internal)
///   TN_MV (220 kV) → winding 2 → star
///   TN_LV (21 kV)  → winding 3 → star
///
/// Expected impedances (z_base_i = ratedU_i² / 100 MVA):
///   W1: r = 1.0 Ω / (400²/100) = 6.25e-4 pu, x = 10.0 / (400²/100) = 6.25e-3 pu
///   W2: r = 0.5 Ω / (220²/100) = 1.033e-3 pu, x = 5.0 / (220²/100) = 1.033e-2 pu
///   W3: r = 0.02 Ω / (21²/100)  = 4.535e-3 pu, x = 0.2 / (21²/100)  = 4.535e-2 pu
///   Mag: b = 0.001 S × (400²/100) = 1.6 pu (on W1 branch only)
const THREE_WINDING_EQ: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_400">
<cim:BaseVoltage.nominalVoltage>400.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_21">
<cim:BaseVoltage.nominalVoltage>21.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_HV">
<cim:TopologicalNode.name>HV_Bus</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_400"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_MV">
<cim:TopologicalNode.name>MV_Bus</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_LV">
<cim:TopologicalNode.name>LV_Bus</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_21"/>
  </cim:TopologicalNode>
  <!-- 3-winding transformer: 400/220/21 kV, 650 MVA -->
  <cim:PowerTransformer rdf:ID="XFMR3W">
  </cim:PowerTransformer>
  <!-- Winding 1 (HV 400 kV): r=1.0 Ω, x=10.0 Ω, b_mag=0.001 S -->
  <cim:PowerTransformerEnd rdf:ID="END3W_1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR3W"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.001</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:PowerTransformerEnd.ratedU>400.0</cim:PowerTransformerEnd.ratedU>
<cim:TransformerEnd.Terminal rdf:resource="#T_3W_1"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_400"/>
  </cim:PowerTransformerEnd>
  <!-- Winding 2 (MV 220 kV): r=0.5 Ω, x=5.0 Ω -->
  <cim:PowerTransformerEnd rdf:ID="END3W_2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR3W"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:PowerTransformerEnd.r>0.5</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>5.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:TransformerEnd.Terminal rdf:resource="#T_3W_2"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
  </cim:PowerTransformerEnd>
  <!-- Winding 3 (LV 21 kV): r=0.02 Ω, x=0.2 Ω -->
  <cim:PowerTransformerEnd rdf:ID="END3W_3">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR3W"/>
<cim:TransformerEnd.endNumber>3</cim:TransformerEnd.endNumber>
<cim:PowerTransformerEnd.r>0.02</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.2</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:PowerTransformerEnd.ratedU>21.0</cim:PowerTransformerEnd.ratedU>
<cim:TransformerEnd.Terminal rdf:resource="#T_3W_3"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_21"/>
  </cim:PowerTransformerEnd>
  <!-- One terminal per winding; ConductingEquipment → PowerTransformer -->
  <cim:Terminal rdf:ID="T_3W_1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR3W"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_3W_2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR3W"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_MV"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_3W_3">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR3W"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_LV"/>
<cim:ACDCTerminal.sequenceNumber>3</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;

#[test]
fn test_cgmes_3winding_star_bus_expansion() {
    // 3 TopologicalNodes + 1 fictitious star bus = 4 buses
    // 3 winding branches (each winding → star)
    let net = parse_str(THREE_WINDING_EQ).unwrap();
    assert_eq!(
        net.n_buses(),
        4,
        "expected 3 TN + 1 star bus, got {}",
        net.n_buses()
    );
    assert_eq!(
        net.n_branches(),
        3,
        "expected 3 winding branches, got {}",
        net.n_branches()
    );

    // Exactly one star bus
    let star_buses: Vec<_> = net
        .buses
        .iter()
        .filter(|b| b.name.starts_with("STAR_"))
        .collect();
    assert_eq!(star_buses.len(), 1, "expected exactly 1 star bus");
    let star_num = star_buses[0].number;
    // Star bus base_kv is set to the highest of the three winding bus kVs (400/220/21 → 400 kV)
    // to prevent division-by-zero in downstream fault analysis.
    assert_eq!(
        star_buses[0].base_kv, 400.0,
        "star bus base_kv should be max winding kV (400 kV) to avoid div-by-zero in fault analysis"
    );

    // All 3 branches connect to the star bus
    let star_branches: Vec<_> = net
        .branches
        .iter()
        .filter(|b| b.to_bus == star_num)
        .collect();
    assert_eq!(
        star_branches.len(),
        3,
        "all 3 winding branches should point to star"
    );
}

#[test]
fn test_cgmes_3winding_impedance_values() {
    let net = parse_str(THREE_WINDING_EQ).unwrap();
    let star_num = net
        .buses
        .iter()
        .find(|b| b.name.starts_with("STAR_"))
        .expect("star bus not found")
        .number;

    // TNs are sorted alphabetically: TN_HV→1, TN_LV→2, TN_MV→3, star→4
    // Winding 1 branch: bus with 400 kV base → star
    let br1 = net
        .branches
        .iter()
        .find(|b| {
            b.to_bus == star_num
                && net
                    .buses
                    .iter()
                    .find(|bus| bus.number == b.from_bus)
                    .map(|bus| (bus.base_kv - 400.0).abs() < 1.0)
                    .unwrap_or(false)
        })
        .expect("HV winding branch not found");

    let z_base1 = 400.0_f64.powi(2) / 100.0; // = 1600 Ω
    assert!(
        (br1.r - 1.0 / z_base1).abs() < 1e-9,
        "W1 r_pu={:.6e} expected={:.6e}",
        br1.r,
        1.0 / z_base1
    );
    assert!(
        (br1.x - 10.0 / z_base1).abs() < 1e-9,
        "W1 x_pu={:.6e} expected={:.6e}",
        br1.x,
        10.0 / z_base1
    );
    // Magnetizing susceptance: b_pu = b_S × ratedU² / base_mva = 0.001 × 160000 / 100 = 1.6
    let b_mag_expected = 0.001 * 400.0_f64.powi(2) / 100.0;
    assert!(
        (br1.b_mag - b_mag_expected).abs() < 1e-9,
        "W1 b_mag={:.6e} expected={:.6e}",
        br1.b_mag,
        b_mag_expected
    );
    // g_mag = 0 (no lossy core)
    assert!(br1.g_mag.abs() < 1e-12, "W1 g_mag should be 0");

    // Winding 2 branch: 220 kV bus → star
    let br2 = net
        .branches
        .iter()
        .find(|b| {
            b.to_bus == star_num
                && net
                    .buses
                    .iter()
                    .find(|bus| bus.number == b.from_bus)
                    .map(|bus| (bus.base_kv - 220.0).abs() < 1.0)
                    .unwrap_or(false)
        })
        .expect("MV winding branch not found");
    let z_base2 = 220.0_f64.powi(2) / 100.0;
    assert!((br2.r - 0.5 / z_base2).abs() < 1e-9, "W2 r_pu mismatch");
    assert!((br2.x - 5.0 / z_base2).abs() < 1e-9, "W2 x_pu mismatch");
    assert!(
        br2.b_mag.abs() < 1e-12,
        "W2 b_mag should be 0 (mag only on W1)"
    );

    // Winding 3 branch: 21 kV bus → star
    let br3 = net
        .branches
        .iter()
        .find(|b| {
            b.to_bus == star_num
                && net
                    .buses
                    .iter()
                    .find(|bus| bus.number == b.from_bus)
                    .map(|bus| (bus.base_kv - 21.0).abs() < 1.0)
                    .unwrap_or(false)
        })
        .expect("LV winding branch not found");
    let z_base3 = 21.0_f64.powi(2) / 100.0;
    assert!((br3.r - 0.02 / z_base3).abs() < 1e-9, "W3 r_pu mismatch");
    assert!((br3.x - 0.2 / z_base3).abs() < 1e-9, "W3 x_pu mismatch");
    assert!(br3.b_mag.abs() < 1e-12, "W3 b_mag should be 0");

    // Nominal taps: ratedU_i == base_kv_i for all windings → tap = 1.0
    assert!(
        (br1.tap - 1.0).abs() < 1e-9,
        "W1 tap should be 1.0 (nominal)"
    );
    assert!(
        (br2.tap - 1.0).abs() < 1e-9,
        "W2 tap should be 1.0 (nominal)"
    );
    assert!(
        (br3.tap - 1.0).abs() < 1e-9,
        "W3 tap should be 1.0 (nominal)"
    );
}

#[test]
fn test_cgmes_minimal_bus_branch_count() {
    let net = parse_str(MINIMAL_EQ).unwrap();
    assert_eq!(net.n_buses(), 4, "buses: {}", net.n_buses());
    assert_eq!(net.n_branches(), 3, "branches: {}", net.n_branches());
}

#[test]
fn test_cgmes_pu_conversion() {
    // Line 1-2: r=10Ω, x=40Ω at 220kV, 100 MVA → z_base=484Ω
    let net = parse_str(MINIMAL_EQ).unwrap();
    let line = net
        .branches
        .iter()
        .find(|b| (b.from_bus == 1 && b.to_bus == 2) || (b.from_bus == 2 && b.to_bus == 1))
        .expect("line 1-2 not found");
    let z_base = 220.0_f64.powi(2) / 100.0;
    assert!(
        (line.r - 10.0 / z_base).abs() < 1e-8,
        "r_pu={} expected={}",
        line.r,
        10.0 / z_base
    );
    assert!((line.x - 40.0 / z_base).abs() < 1e-8, "x_pu={}", line.x);
}

#[test]
fn test_cgmes_transformer_tap() {
    // Nominal 220→110 kV transformer: ratedU1=220, ratedU2=110, base_kv_from=220, base_kv_to=110
    // tap = (ratedU1/ratedU2) * (base_kv_to/base_kv_from) = (220/110) * (110/220) = 1.0
    // A nominal transformer always has tap = 1.0 in MATPOWER per-unit convention.
    let net = parse_str(MINIMAL_EQ).unwrap();
    let xfmr = net
        .branches
        .iter()
        .find(|b| (b.from_bus == 3 && b.to_bus == 4) || (b.from_bus == 4 && b.to_bus == 3))
        .expect("transformer 3-4 not found");
    assert!(
        (xfmr.tap - 1.0).abs() < 1e-6,
        "tap={} expected 1.0 (nominal)",
        xfmr.tap
    );
}

#[test]
fn test_cgmes_generator_and_load() {
    let net = parse_str(MINIMAL_EQ).unwrap();
    assert_eq!(net.generators.len(), 1);
    let g = &net.generators[0];
    assert!((g.p - 150.0).abs() < 1e-3, "pg={}", g.p); // abs(-150)
    assert!((g.pmax - 200.0).abs() < 1e-3, "pmax={}", g.pmax);
    let total_load: f64 = net.total_load_mw();
    assert!(
        (total_load - 100.0).abs() < 1e-3,
        "total load={}",
        total_load
    );
}

#[test]
fn test_cgmes_slack_from_reference_priority() {
    // SM_1 has referencePriority=1 → should be slack
    let net = parse_str(MINIMAL_EQ).unwrap();
    let slack = net
        .buses
        .iter()
        .filter(|b| b.bus_type == BusType::Slack)
        .count();
    assert_eq!(slack, 1, "expected 1 slack");
    // Bus 1 (TN_1, sorted) should be slack since SM is connected there
    let bus1 = net.buses.iter().find(|b| b.number == 1).unwrap();
    assert_eq!(bus1.bus_type, BusType::Slack, "bus 1 should be slack");
}

#[test]
fn test_cgmes_shunt_susceptance() {
    // SHC_4: b=0.02 S at 110kV → b_mvar = 0.02 * 110² = 242 MVAr
    // bus.shunt_susceptance_mvar stores MVAr (like MATPOWER Bs); Y-bus divides by base_mva to get pu.
    let net = parse_str(MINIMAL_EQ).unwrap();
    let bus4 = net
        .buses
        .iter()
        .find(|b| b.base_kv < 115.0 && b.base_kv > 100.0)
        .expect("110kV bus not found");
    let expected = 0.02 * 110.0_f64.powi(2); // = 242 MVAr
    assert!(
        (bus4.shunt_susceptance_mvar - expected).abs() < 1e-6,
        "bs={} expected={}",
        bus4.shunt_susceptance_mvar,
        expected
    );
}

#[test]
fn test_cgmes_base_kv() {
    let net = parse_str(MINIMAL_EQ).unwrap();
    // TN_1,2,3 → 220kV; TN_4 → 110kV (sorted alpha: TN_1→1,TN_2→2,TN_3→3,TN_4→4)
    assert!(
        (net.buses[0].base_kv - 220.0).abs() < 0.1,
        "bus1={}",
        net.buses[0].base_kv
    );
    assert!(
        (net.buses[3].base_kv - 110.0).abs() < 0.1,
        "bus4={}",
        net.buses[3].base_kv
    );
}

#[test]
fn test_cgmes_parse_file() {
    let tmp = std::env::temp_dir().join("surge_cgmes_test.xml");
    std::fs::write(&tmp, MINIMAL_EQ).unwrap();
    let net = parse_files(&[tmp.as_path()]).unwrap();
    assert_eq!(net.n_buses(), 4);
    let _ = std::fs::remove_file(&tmp);
}

// -----------------------------------------------------------------------
// Real-file tests (require tests/data/cgmes/ to exist)
// -----------------------------------------------------------------------

fn load_profiles(dir: &PathBuf) -> Option<Network> {
    let profiles: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "xml").unwrap_or(false))
        .filter(|p| !p.to_string_lossy().contains("DiagramLayout"))
        .collect();
    if profiles.is_empty() {
        return None;
    }
    let refs: Vec<&Path> = profiles.iter().map(|p| p.as_path()).collect();
    parse_files(&refs).ok()
}

#[test]
fn test_case9_cgmes_bus_branch_gen_load() {
    let dir = test_data("case9");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert_eq!(net.n_buses(), 9, "case9 buses: {}", net.n_buses());
    assert_eq!(net.n_branches(), 9, "case9 branches: {}", net.n_branches());
    assert_eq!(
        net.generators.len(),
        3,
        "case9 generators: {}",
        net.generators.len()
    );
    // Total load: 90+100+125 = 315 MW
    let total_load: f64 = net.total_load_mw();
    assert!(
        (total_load - 315.0).abs() < 1.0,
        "case9 load={:.1}",
        total_load
    );
    // Total generation: 72.3+163+85 = 320.3 MW
    let total_gen: f64 = net.generators.iter().map(|g| g.p).sum();
    assert!((total_gen - 320.3).abs() < 1.0, "case9 Pg={:.1}", total_gen);
    // All 345kV
    assert!(net.buses.iter().all(|b| (b.base_kv - 345.0).abs() < 1.0));
    // 1 slack
    let slacks = net
        .buses
        .iter()
        .filter(|b| b.bus_type == BusType::Slack)
        .count();
    assert_eq!(slacks, 1);
}

#[test]
fn test_case9_impedance_values() {
    // Verify line 4-5: r≈0.01695, x≈0.09196, b≈0.1580 pu (MATPOWER reference)
    let dir = test_data("case9");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    // Find a branch between internal buses 4 and 5 — bus numbers depend on TN sort
    // Just verify that all branch x values are > 0 and reasonable (0.001–10 pu)
    for br in &net.branches {
        assert!(br.x > 0.0, "branch {}-{} has x=0", br.from_bus, br.to_bus);
        assert!(
            br.x < 100.0,
            "branch {}-{} has x={} (too large)",
            br.from_bus,
            br.to_bus,
            br.x
        );
        assert!(
            br.r >= 0.0,
            "branch {}-{} has negative r",
            br.from_bus,
            br.to_bus
        );
    }
}

#[test]
fn test_case14_cgmes_bus_branch_counts() {
    let dir = test_data("case14");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert_eq!(net.n_buses(), 14, "case14 buses: {}", net.n_buses());
    // case14 has 20 branches (15 lines + 5 transformers)
    assert_eq!(
        net.n_branches(),
        20,
        "case14 branches: {}",
        net.n_branches()
    );
    assert_eq!(
        net.generators.len(),
        5,
        "case14 generators: {}",
        net.generators.len()
    );
}

#[test]
fn test_case14_shunt_susceptance() {
    // case14 has LinearShuntCompensator at bus 9 with bPerSection=19 S, base=1kV
    // b_pu = 19 * 1² / 100 = 0.19 pu
    let dir = test_data("case14");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    let total_bs: f64 = net.buses.iter().map(|b| b.shunt_susceptance_mvar).sum();
    // Should have some shunt capacitance
    assert!(total_bs > 1.0, "case14 shunt total bs={:.4}", total_bs);
    // Verify exact value: 19 S at 1kV → b_mvar = 19 * 1² = 19 MVAr
    // (bus.shunt_susceptance_mvar stores MVAr; Y-bus divides by base_mva for pu)
    assert!(
        (total_bs - 19.0).abs() < 0.1,
        "case14 total_bs={:.4} expected≈19 MVAr",
        total_bs
    );
}

#[test]
fn test_case14_transformer_taps() {
    // case14 has 5 transformers; taps should be close to 1.0 (±20%)
    let dir = test_data("case14");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    let xfmrs: Vec<&Branch> = net
        .branches
        .iter()
        .filter(|b| b.tap != 0.0 && (b.tap - 1.0).abs() > 1e-4)
        .collect();
    // All taps should be between 0.5 and 2.0
    for xfmr in &xfmrs {
        assert!(
            xfmr.tap > 0.5 && xfmr.tap < 2.0,
            "transformer tap={} (buses {}-{})",
            xfmr.tap,
            xfmr.from_bus,
            xfmr.to_bus
        );
    }
}

#[test]
fn test_case14_load_values() {
    // case14 total load ≈ 259 MW
    let dir = test_data("case14");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    let total_load: f64 = net.total_load_mw();
    assert!(
        (total_load - 259.0).abs() < 5.0,
        "case14 load={:.1} expected≈259",
        total_load
    );
}

#[test]
fn test_case118_parses() {
    let dir = test_data("case118");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert_eq!(net.n_buses(), 118, "case118 buses: {}", net.n_buses());
    assert!(
        net.n_branches() >= 170,
        "case118 branches: {}",
        net.n_branches()
    );
    assert!(
        net.generators.len() >= 50,
        "case118 gens: {}",
        net.generators.len()
    );
    let total_load: f64 = net.total_load_mw();
    // case118 total load ≈ 4242 MW
    assert!(
        total_load > 3000.0 && total_load < 6000.0,
        "case118 load={:.0}",
        total_load
    );
}

#[test]
fn test_case300_parses() {
    let dir = test_data("case300");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert_eq!(net.n_buses(), 300, "case300 buses: {}", net.n_buses());
    assert!(
        net.n_branches() >= 400,
        "case300 branches: {}",
        net.n_branches()
    );
}

#[test]
fn test_ieee9_ppow_parses() {
    let dir = test_data("ieee9_ppow");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert_eq!(net.n_buses(), 9, "ieee9_ppow buses: {}", net.n_buses());
    assert_eq!(
        net.n_branches(),
        9,
        "ieee9_ppow branches: {}",
        net.n_branches()
    );
}

#[test]
fn test_ieee57_ppow_parses() {
    let dir = test_data("ieee57_ppow");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert_eq!(net.n_buses(), 57, "ieee57 buses: {}", net.n_buses());
    assert!(
        net.n_branches() >= 70,
        "ieee57 branches: {}",
        net.n_branches()
    );
}

#[test]
fn test_ieee300_ppow_parses() {
    let dir = test_data("ieee300_ppow");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    // CGMES topology merging reduces 300 buses to ~291 in the bus-branch model
    assert!(
        net.n_buses() >= 291,
        "ieee300_ppow buses: {}",
        net.n_buses()
    );
}

#[test]
fn test_microgrid_be_parses() {
    let dir = test_data("microgrid_be");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    // ENTSO-E MicroGrid BE: 6 buses, known configuration
    assert!(net.n_buses() >= 4, "microgrid_be buses: {}", net.n_buses());
    assert!(
        net.n_branches() >= 2,
        "microgrid_be branches: {}",
        net.n_branches()
    );
}

#[test]
fn test_microgrid_nl_parses() {
    let dir = test_data("microgrid_nl");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    // ENTSO-E MicroGrid NL has 5 TopologicalNodes; 3 remain after island filtering
    assert!(net.n_buses() >= 2, "microgrid_nl buses: {}", net.n_buses());
}

#[test]
fn test_cigremv_parses() {
    let dir = test_data("cigremv");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    // CIGRE MV has 12–15 buses
    assert!(net.n_buses() >= 10, "cigremv buses: {}", net.n_buses());
    assert!(
        net.n_branches() >= 10,
        "cigremv branches: {}",
        net.n_branches()
    );
}

#[test]
fn test_eurostag_parses() {
    let dir = test_data("eurostag_ex1");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert!(net.n_buses() >= 2, "eurostag buses: {}", net.n_buses());
}

#[test]
fn test_multi_profile_merge() {
    // Verify that SSH values override EQ defaults when merging profiles
    let dir = test_data("case9");
    if !dir.exists() {
        return;
    }
    let eq_path = match glob_profile(&dir, "EQ") {
        Some(p) => p,
        None => return,
    };
    let tp_path = match glob_profile(&dir, "TP") {
        Some(p) => p,
        None => return,
    };
    let ssh_path = match glob_profile(&dir, "SSH") {
        Some(p) => p,
        None => return,
    };

    let err = parse_files(&[eq_path.as_path(), tp_path.as_path()]).unwrap_err();
    assert!(matches!(err, Error::MissingSshProfile));
    // Parse EQ+TP+SSH (SSH sets actual Pd/Qd)
    let net_with_ssh =
        parse_files(&[eq_path.as_path(), tp_path.as_path(), ssh_path.as_path()]).unwrap();

    let load_with_ssh: f64 = net_with_ssh.total_load_mw();
    assert!(
        load_with_ssh > 0.0,
        "SSH-backed case should carry non-zero load: {load_with_ssh}"
    );
    assert!(
        (load_with_ssh - 315.0).abs() < 1.0,
        "case9 SSH load={:.1}",
        load_with_ssh
    );
}

#[test]
fn test_sv_voltage_applied() {
    // SV profile sets bus voltages from solved power flow
    let dir = test_data("case9");
    if !dir.exists() {
        return;
    }
    let sv_path = match glob_profile(&dir, "SV") {
        Some(p) => p,
        None => return,
    };
    let eq_path = match glob_profile(&dir, "EQ") {
        Some(p) => p,
        None => return,
    };
    let tp_path = match glob_profile(&dir, "TP") {
        Some(p) => p,
        None => return,
    };

    let ssh_path = match glob_profile(&dir, "SSH") {
        Some(p) => p,
        None => return,
    };

    // Without SV: vm = 1.0 (flat start), but SSH still supplies the operating point.
    let net_flat =
        parse_files(&[eq_path.as_path(), tp_path.as_path(), ssh_path.as_path()]).unwrap();
    // With SV: vm from solved voltages
    let net_sv = parse_files(&[
        eq_path.as_path(),
        tp_path.as_path(),
        ssh_path.as_path(),
        sv_path.as_path(),
    ])
    .unwrap();

    // Voltage magnitudes should be reasonable (0.9–1.1 pu)
    for bus in &net_sv.buses {
        assert!(
            bus.voltage_magnitude_pu > 0.8 && bus.voltage_magnitude_pu < 1.2,
            "bus {} vm={:.4}",
            bus.number,
            bus.voltage_magnitude_pu
        );
    }
    // For case9 pypowsybl SV, all v=345kV / 345kV = 1.0 (SV may be flat-start)
    let avg_vm: f64 = net_sv
        .buses
        .iter()
        .map(|b| b.voltage_magnitude_pu)
        .sum::<f64>()
        / net_sv.n_buses() as f64;
    let _ = (net_flat.buses[0].voltage_magnitude_pu, avg_vm); // both should be ≈1.0
}

#[test]
fn test_missing_base_voltage_reference_is_rejected() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.name>Bus1</cim:TopologicalNode.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_MISSING"/>
  </cim:TopologicalNode>
</rdf:RDF>"##;

    let err = parse_str(xml).unwrap_err();
    match err {
        Error::MissingBaseVoltageReferences { count, examples } => {
            assert!(count >= 1);
            assert!(
                examples
                    .iter()
                    .any(|example| example
                        .contains("TopologicalNode:TN_1 -> BaseVoltage:BV_MISSING"))
            );
            assert!(examples.iter().any(|example| {
                example.contains("TopologicalNode:TN_1 missing resolvable BaseVoltage")
            }));
        }
        other => panic!("expected MissingBaseVoltageReferences, got {other:?}"),
    }
}

#[test]
fn test_generator_voltage_setpoints() {
    // case9 has RegulatingControl.targetValue in SSH (kV) → should map to pu
    let dir = test_data("case9");
    if !dir.exists() {
        return;
    }
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    for g in &net.generators {
        // Vs should be reasonable (0.9–1.15 pu)
        assert!(
            g.voltage_setpoint_pu > 0.85 && g.voltage_setpoint_pu < 1.20,
            "generator on bus {} has vs={:.4}",
            g.bus,
            g.voltage_setpoint_pu
        );
    }
}

#[test]
fn test_all_buses_have_base_kv() {
    // Every bus must have a positive base_kv
    for case in &["case9", "case14", "case118"] {
        let dir = test_data(case);
        if !dir.exists() {
            continue;
        }
        let net = match load_profiles(&dir) {
            Some(n) => n,
            None => continue,
        };
        for bus in &net.buses {
            assert!(
                bus.base_kv > 0.0,
                "{} bus {} has base_kv=0",
                case,
                bus.number
            );
        }
    }
}

#[test]
fn test_no_zero_reactance_branches() {
    // All branches must have |x| > 0 (zero impedance = admittance singularity).
    // Negative x is valid for series capacitors.
    for case in &["case9", "case14", "case118", "case300"] {
        let dir = test_data(case);
        if !dir.exists() {
            continue;
        }
        let net = match load_profiles(&dir) {
            Some(n) => n,
            None => continue,
        };
        for br in &net.branches {
            assert!(
                br.x.abs() > 1e-12,
                "{case}: branch {}-{} has zero reactance (x={})",
                br.from_bus,
                br.to_bus,
                br.x,
            );
        }
    }
}

#[test]
fn test_network_connectivity() {
    // At least one branch per non-isolated bus (connectivity sanity check)
    for case in &["case9", "case14", "case118"] {
        let dir = test_data(case);
        if !dir.exists() {
            continue;
        }
        let net = match load_profiles(&dir) {
            Some(n) => n,
            None => continue,
        };
        let connected_buses: std::collections::HashSet<u32> = net
            .branches
            .iter()
            .flat_map(|b| [b.from_bus, b.to_bus])
            .collect();
        // All buses should appear in at least one branch
        for bus in &net.buses {
            assert!(
                connected_buses.contains(&bus.number),
                "{} bus {} has no connected branches",
                case,
                bus.number
            );
        }
    }
}

// -----------------------------------------------------------------------
// Large-scale real-file tests (require tests/data/cgmes/ with large cases)
// -----------------------------------------------------------------------

fn parse_large(case: &str, expected_buses: usize, expected_branches: usize) {
    let dir = test_data(case);
    if !dir.exists() {
        return;
    } // skip if not downloaded
    let net = match load_profiles(&dir) {
        Some(n) => n,
        None => return,
    };
    assert!(
        net.n_buses() >= expected_buses,
        "{case}: expected >={expected_buses} buses, got {}",
        net.n_buses()
    );
    assert!(
        net.n_branches() >= expected_branches,
        "{case}: expected >={expected_branches} branches, got {}",
        net.n_branches()
    );
    // Sanity: no NaN in impedances
    for br in &net.branches {
        assert!(
            br.x.is_finite() && br.r.is_finite(),
            "{case}: branch {}-{} has non-finite r/x",
            br.from_bus,
            br.to_bus
        );
        assert!(
            br.x.abs() > 1e-12,
            "{case}: branch {}-{} has zero reactance",
            br.from_bus,
            br.to_bus
        );
    }
    // All buses have finite base_kv
    for bus in &net.buses {
        assert!(
            bus.base_kv.is_finite() && bus.base_kv > 0.0,
            "{case}: bus {} has invalid base_kv={}",
            bus.number,
            bus.base_kv
        );
    }
    // Exactly one slack
    let slacks = net
        .buses
        .iter()
        .filter(|b| b.bus_type == BusType::Slack)
        .count();
    assert_eq!(slacks, 1, "{case}: expected 1 slack, got {slacks}");
}

#[test]
fn test_ieee300_pbl_parses() {
    // CGMES topology merging reduces 300 buses to ~291 in the bus-branch model
    parse_large("ieee300_pbl", 291, 400);
}

#[test]
fn test_ieee118_pbl_parses() {
    parse_large("ieee118_pbl", 118, 175);
}

#[test]
fn test_ieee57_pbl_parses() {
    parse_large("ieee57_pbl", 57, 74);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~1.3k buses); run with --ignored"]
fn test_case1354pegase_parses() {
    parse_large("case1354pegase", 1354, 1700);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~1.9k buses); run with --ignored"]
fn test_case1888rte_parses() {
    parse_large("case1888rte", 1888, 2300);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~2.4k buses); run with --ignored"]
fn test_case2383wp_parses() {
    parse_large("case2383wp", 2383, 2800);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~6.5k buses); run with --ignored"]
fn test_case6470rte_parses() {
    parse_large("case6470rte", 6470, 8000);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~6.5k buses); run with --ignored"]
fn test_case6515rte_parses() {
    parse_large("case6515rte", 6515, 8000);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~9.2k buses); run with --ignored"]
fn test_case9241pegase_parses() {
    parse_large("case9241pegase", 9241, 12000);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~2.7k buses); run with --ignored"]
fn test_case2736sp_parses() {
    parse_large("case2736sp", 2736, 3200);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~3k buses); run with --ignored"]
fn test_case3012wp_parses() {
    parse_large("case3012wp", 3012, 3600);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~3.1k buses); run with --ignored"]
fn test_case3120sp_parses() {
    parse_large("case3120sp", 3120, 3600);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~13.7k buses); run with --ignored"]
fn test_case13659pegase_parses() {
    parse_large("case13659pegase", 13659, 18000);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~1.2k buses); run with --ignored"]
fn test_case1197_parses() {
    parse_large("case1197", 1197, 1400);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~2k buses); run with --ignored"]
fn test_activsg2000_parses() {
    parse_large("case_ACTIVSg2000", 2000, 2500);
}

#[test]
#[ignore = "slow: parses large CGMES directory (~10k buses); run with --ignored"]
fn test_activsg10k_parses() {
    parse_large("case_ACTIVSg10k", 10000, 12000);
}

// -----------------------------------------------------------------------
// ConformLoad / NonConformLoad / ExternalNetworkInjection / VsConverter
// -----------------------------------------------------------------------

/// ConformLoad and NonConformLoad must be parsed as loads exactly like EnergyConsumer.
#[test]
fn test_cgmes_conform_load_parsed_as_load() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL12_1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL12_2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Generator on bus 1 (slack) -->
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-200.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- ConformLoad on bus 2: 80 MW, 30 MVAr -->
  <cim:ConformLoad rdf:ID="CL_2">
<cim:EnergyConsumer.p>80.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>30.0</cim:EnergyConsumer.q>
  </cim:ConformLoad>
  <cim:Terminal rdf:ID="T_CL2">
<cim:Terminal.ConductingEquipment rdf:resource="#CL_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- NonConformLoad on bus 2: 20 MW, 10 MVAr -->
  <cim:NonConformLoad rdf:ID="NCL_2">
<cim:EnergyConsumer.p>20.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:NonConformLoad>
  <cim:Terminal rdf:ID="T_NCL2">
<cim:Terminal.ConductingEquipment rdf:resource="#NCL_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 2, "expected 2 buses");
    // Bus 2 total load: ConformLoad 80+j30 + NonConformLoad 20+j10 = 100+j40
    let bus2 = net
        .buses
        .iter()
        .find(|b| b.bus_type != BusType::Slack)
        .unwrap();
    let bus2_pd: f64 = net
        .loads
        .iter()
        .filter(|l| l.bus == bus2.number)
        .map(|l| l.active_power_demand_mw)
        .sum();
    let bus2_qd: f64 = net
        .loads
        .iter()
        .filter(|l| l.bus == bus2.number)
        .map(|l| l.reactive_power_demand_mvar)
        .sum();
    assert!(
        (bus2_pd - 100.0).abs() < 1e-6,
        "ConformLoad+NonConformLoad pd sum wrong: got {}, expected 100.0",
        bus2_pd
    );
    assert!(
        (bus2_qd - 40.0).abs() < 1e-6,
        "ConformLoad+NonConformLoad qd sum wrong: got {}, expected 40.0",
        bus2_qd
    );
    // Should have 2 loads in the load list (one per element)
    assert_eq!(
        net.loads.len(),
        2,
        "expected 2 loads (one per ConformLoad/NonConformLoad)"
    );
}

/// ExternalNetworkInjection with referencePriority > 0 must become the slack bus.
#[test]
fn test_cgmes_external_network_injection_slack() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL12_1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL12_2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- ExternalNetworkInjection on bus 1 with referencePriority=1 → slack -->
  <cim:ExternalNetworkInjection rdf:ID="ENI_1">
<cim:ExternalNetworkInjection.referencePriority>1</cim:ExternalNetworkInjection.referencePriority>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:ExternalNetworkInjection.minQ>-500.0</cim:ExternalNetworkInjection.minQ>
<cim:ExternalNetworkInjection.maxQ>500.0</cim:ExternalNetworkInjection.maxQ>
  </cim:ExternalNetworkInjection>
  <cim:Terminal rdf:ID="T_ENI1">
<cim:Terminal.ConductingEquipment rdf:resource="#ENI_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Load on bus 2 -->
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>20.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 2, "expected 2 buses");
    // The bus connected to ENI_1 (TN_1) must be the slack
    let slack_bus = net.buses.iter().find(|b| b.bus_type == BusType::Slack);
    assert!(
        slack_bus.is_some(),
        "no slack bus found — ExternalNetworkInjection not used"
    );
    // The slack must be the bus for TN_1 (bus_num derived from bus_num_to_idx)
    // We can't directly check mRID here; instead verify bus 2 is PQ (load only)
    let pq_bus = net.buses.iter().find(|b| b.bus_type == BusType::PQ);
    assert!(pq_bus.is_some(), "expected at least one PQ bus");
    let pq_pd: f64 = net
        .loads
        .iter()
        .filter(|l| l.bus == pq_bus.unwrap().number)
        .map(|l| l.active_power_demand_mw)
        .sum();
    assert!((pq_pd - 50.0).abs() < 1e-6, "PQ bus load wrong: {}", pq_pd);
}

#[test]
fn test_cgmes_external_network_injection_roundtrip_preserves_slack_class() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL12_1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL12_2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:ExternalNetworkInjection rdf:ID="ENI_1">
<cim:ExternalNetworkInjection.referencePriority>1</cim:ExternalNetworkInjection.referencePriority>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:ExternalNetworkInjection.minQ>-500.0</cim:ExternalNetworkInjection.minQ>
<cim:ExternalNetworkInjection.maxQ>500.0</cim:ExternalNetworkInjection.maxQ>
  </cim:ExternalNetworkInjection>
  <cim:Terminal rdf:ID="T_ENI1">
<cim:Terminal.ConductingEquipment rdf:resource="#ENI_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>20.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    let (profiles, reparsed) = roundtrip_v2_profiles(&net);

    assert!(
        profiles
            .eq
            .contains("<cim:ExternalNetworkInjection rdf:ID=\"ENI_1\">"),
        "writer should preserve ExternalNetworkInjection class"
    );
    assert!(
        profiles
            .eq
            .contains("ExternalNetworkInjection.referencePriority>1<"),
        "writer should preserve ENI referencePriority"
    );
    assert!(
        !profiles
            .eq
            .contains("<cim:EquivalentInjection rdf:ID=\"ENI_1\">"),
        "writer must not degrade ENI into EquivalentInjection"
    );

    let slack_bus = reparsed
        .buses
        .iter()
        .find(|bus| bus.bus_type == BusType::Slack)
        .expect("round-tripped ENI slack should survive");
    assert_eq!(
        slack_bus.number, 1,
        "ENI-designated slack bus should survive round-trip"
    );
    assert!(
        reparsed
            .cim
            .cgmes_roundtrip
            .external_network_injections
            .contains_key("ENI_1"),
        "round-tripped ENI source object should still be preserved"
    );
}

/// VsConverter P+Q injection must subtract from bus net load (injection = negative load).
#[test]
fn test_cgmes_vsconverter_pq_injection() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL12_1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL12_2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Generator on bus 1 (slack) -->
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-300.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Load on bus 2: 200 MW -->
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- VsConverter on bus 2: injects 150 MW (positive p = injection into AC network) -->
  <cim:VsConverter rdf:ID="VSC_2">
<cim:ACDCConverter.p>150.0</cim:ACDCConverter.p>
<cim:ACDCConverter.q>0.0</cim:ACDCConverter.q>
  </cim:VsConverter>
  <cim:Terminal rdf:ID="T_VSC2">
<cim:Terminal.ConductingEquipment rdf:resource="#VSC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 2, "expected 2 buses");
    // Bus 2: load=200 MW, VsConverter injection=150 MW → net pd = 200 - 150 = 50 MW
    let bus2 = net
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::PQ)
        .unwrap();
    let bus_pd = net.bus_load_p_mw();
    let bus2_idx = net
        .buses
        .iter()
        .position(|b| b.number == bus2.number)
        .unwrap();
    assert!(
        (bus_pd[bus2_idx] - 50.0).abs() < 1e-6,
        "VsConverter injection not subtracted: bus pd={}, expected 50.0",
        bus_pd[bus2_idx]
    );
}

/// VsConverter with `targetPpcc` (no SSH `p`) and loss parameters: the
/// parsed active power should include `idleLoss + switchingLoss × |targetPpcc|`.
#[test]
fn test_cgmes_vsconverter_targetppcc_loss_correction() {
    // Minimal 2-bus network: slack bus 1, load bus 2 with a VsConverter
    // that has no SSH `p` (measured) but has `targetPpcc` and loss params.
    // Expected P at bus 2: pd_load - (targetPpcc + idleLoss + switchingLoss * targetPpcc)
    //   = 200 - (100 + 2.0 + 0.01 * 100) = 200 - 103 = 97 MW
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL12_1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL12_2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-300.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:SynchronousMachine.minQ>-200.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>200.0</cim:SynchronousMachine.maxQ>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- VsConverter: no SSH p, only targetPpcc=100 MW + losses -->
  <cim:VsConverter rdf:ID="VSC_2">
<cim:ACDCConverter.targetPpcc>100.0</cim:ACDCConverter.targetPpcc>
<cim:ACDCConverter.idleLoss>2.0</cim:ACDCConverter.idleLoss>
<cim:ACDCConverter.switchingLoss>0.01</cim:ACDCConverter.switchingLoss>
  </cim:VsConverter>
  <cim:Terminal rdf:ID="T_VSC2">
<cim:Terminal.ConductingEquipment rdf:resource="#VSC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 2, "expected 2 buses");
    let bus2 = net
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::PQ)
        .unwrap();
    // 200 MW load − (100 + 2.0 + 0.01×100) = 200 − 103 = 97 MW net pd
    let expected = 97.0_f64;
    let bus_pd = net.bus_load_p_mw();
    let bus2_idx = net
        .buses
        .iter()
        .position(|b| b.number == bus2.number)
        .unwrap();
    assert!(
        (bus_pd[bus2_idx] - expected).abs() < 1e-6,
        "loss-corrected targetPpcc wrong: bus pd={:.4}, expected {expected:.4}",
        bus_pd[bus2_idx]
    );
}

// ── Wave 3: CGMES 3.0 namespace, diagnostics, x-clamp ───────────────────

/// CGMES 3.0 uses CIM100 namespace (http://iec.ch/TC57/CIM100#) instead of
/// CIM16. Verify our parser handles both identically — is_cim_ns() already
/// accepts both; this test confirms end-to-end.
#[test]
fn test_cgmes_3_0_cim100_namespace_parsed() {
    // Same minimal 2-bus network but with CGMES 3.0 CIM100 namespace.
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-200.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-80.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>80.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("CGMES 3.0 CIM100 parse failed");
    assert_eq!(net.buses.len(), 2, "expected 2 buses from CIM100 file");
    assert_eq!(net.branches.len(), 1, "expected 1 branch from CIM100 file");
    assert!(
        net.buses.iter().any(|b| b.bus_type == BusType::Slack),
        "no slack bus in CIM100 network"
    );
}

/// CGMES 3.0 PowerElectronicsConnection (discharging, p>0) is parsed as a Generator.
/// Uses CIM100 namespace.
#[test]
fn test_cgmes_3_0_power_electronics_connection_generating() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- Slack generator on TN1 -->
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Load on TN2 -->
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>60.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
  <!-- PowerElectronicsConnection on TN2: 40 MW generation (discharging battery) -->
  <!-- BatteryUnit is the DC unit; PEC is the AC-side interface -->
  <cim:BatteryUnit rdf:ID="BATT1">
<cim:BatteryUnit.ratedE>200.0</cim:BatteryUnit.ratedE>
<cim:BatteryUnit.storedE>100.0</cim:BatteryUnit.storedE>
  </cim:BatteryUnit>
  <cim:PowerElectronicsConnection rdf:ID="PEC1">
<cim:PowerElectronicsConnection.p>40.0</cim:PowerElectronicsConnection.p>
<cim:PowerElectronicsConnection.q>5.0</cim:PowerElectronicsConnection.q>
<cim:PowerElectronicsConnection.maxP>50.0</cim:PowerElectronicsConnection.maxP>
<cim:PowerElectronicsConnection.minP>0.0</cim:PowerElectronicsConnection.minP>
  </cim:PowerElectronicsConnection>
  <cim:Terminal rdf:ID="TPEC">
<cim:Terminal.ConductingEquipment rdf:resource="#PEC1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // PEC1 with p=40 MW (injection) → Generator with pg=40 MW.
    let pec_gen = net.generators.iter().find(|g| (g.p - 40.0).abs() < 1e-6);
    assert!(
        pec_gen.is_some(),
        "PEC generator (pg=40) not found. generators={:?}",
        net.generators
    );
    let pec_gen = pec_gen.unwrap();
    assert!(
        (pec_gen.q - 5.0).abs() < 1e-9,
        "PEC qg={} expected=5",
        pec_gen.q
    );
    assert!(
        (pec_gen.pmax - 50.0).abs() < 1e-9,
        "PEC pmax={} expected=50",
        pec_gen.pmax
    );
}

/// CGMES 3.0 PowerElectronicsConnection (charging, p<0) is parsed as a PQ Load.
#[test]
fn test_cgmes_3_0_power_electronics_connection_charging() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- Slack generator -->
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-120.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- PowerElectronicsConnection on TN2: p=-30 MW (charging, absorbing from grid) -->
  <cim:PowerElectronicsConnection rdf:ID="PEC_CHARG">
<cim:PowerElectronicsConnection.p>-30.0</cim:PowerElectronicsConnection.p>
<cim:PowerElectronicsConnection.q>-5.0</cim:PowerElectronicsConnection.q>
  </cim:PowerElectronicsConnection>
  <cim:Terminal rdf:ID="TPEC2">
<cim:Terminal.ConductingEquipment rdf:resource="#PEC_CHARG"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // PEC_CHARG with p=-30 MW (consuming) → Load with pd=30, qd=5.
    let charg_load = net
        .loads
        .iter()
        .find(|l| (l.active_power_demand_mw - 30.0).abs() < 1e-6);
    assert!(
        charg_load.is_some(),
        "PEC charging load (pd=30) not found. loads={:?}",
        net.loads
    );
    let charg_load = charg_load.unwrap();
    assert!(
        (charg_load.reactive_power_demand_mvar - 5.0).abs() < 1e-9,
        "PEC charging qd={} expected=5",
        charg_load.reactive_power_demand_mvar
    );
}

/// CGMES 3.0 DCTopologicalNode and BatteryUnit/PhotovoltaicUnit classes are
/// recognized without errors. These are DC-side topology/metadata classes that
/// do not contribute to positive-sequence power flow — they are silently counted
/// and logged at debug level. The AC network is unaffected.
#[test]
fn test_cgmes_3_0_dc_topology_and_pec_units_recognized() {
    // Network with DCTopologicalNode, BatteryUnit, and PhotovoltaicUnit present.
    // Verify they don't cause parse failures or spurious warnings.
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>0.5</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV110"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-50.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>50.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC2">
<cim:EnergyConsumer.p>100.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TEC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
  <!-- CGMES 3.0 DC-topology node — no AC model, should not cause errors -->
  <cim:DCTopologicalNode rdf:ID="DCN1">
<cim:IdentifiedObject.name>DC Bus 1</cim:IdentifiedObject.name>
  </cim:DCTopologicalNode>
  <cim:DCTopologicalIsland rdf:ID="DCIS1">
<cim:IdentifiedObject.name>DC Island 1</cim:IdentifiedObject.name>
  </cim:DCTopologicalIsland>
  <!-- CGMES 3.0 PowerElectronicsUnit subclasses — DC-side metadata only -->
  <cim:BatteryUnit rdf:ID="BATT2">
<cim:BatteryUnit.ratedE>500.0</cim:BatteryUnit.ratedE>
<cim:BatteryUnit.storedE>250.0</cim:BatteryUnit.storedE>
  </cim:BatteryUnit>
  <cim:PhotovoltaicUnit rdf:ID="PVU1">
<cim:PowerElectronicsUnit.minP>0.0</cim:PowerElectronicsUnit.minP>
<cim:PowerElectronicsUnit.maxP>80.0</cim:PowerElectronicsUnit.maxP>
  </cim:PhotovoltaicUnit>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("CGMES 3.0 with DC topology classes should parse OK");
    // AC network is unaffected by DC topology classes
    assert_eq!(net.buses.len(), 2, "2 AC buses expected");
    assert_eq!(net.branches.len(), 1, "1 AC line expected");
    // No generators or loads from DCTopologicalNode / BatteryUnit / PhotovoltaicUnit
    // (no PEC linking them to an AC bus)
    assert_eq!(net.generators.len(), 1, "only SM1 generator");
    assert_eq!(net.loads.len(), 1, "only EC2 load");
}

/// CGMES 3.0 `PhaseTapChangerNonLinear` uses PhaseTapChangerTable just like
/// PhaseTapChangerTabular.  Verify the angle is interpolated correctly from table.
#[test]
fn test_cgmes_3_0_phase_tap_changer_nonlinear() {
    // 2-bus network with a 2W transformer having PhaseTapChangerNonLinear (CGMES 3.0).
    // Table: step 0 → 0°, step 1 → 5°, step 2 → 10°.  SvTapStep = 1 → shift = 5°.
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:BaseVoltage rdf:ID="BV220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- 2W power transformer -->
  <cim:PowerTransformer rdf:ID="PT1">
<cim:IdentifiedObject.name>PST1</cim:IdentifiedObject.name>
  </cim:PowerTransformer>
  <cim:PowerTransformerEnd rdf:ID="PTE1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#PT1"/>
<cim:PowerTransformerEnd.endNumber>1</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.1</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>1.0</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="PTE2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#PT1"/>
<cim:PowerTransformerEnd.endNumber>2</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="TPT1_1">
<cim:Terminal.ConductingEquipment rdf:resource="#PT1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TPT1_2">
<cim:Terminal.ConductingEquipment rdf:resource="#PT1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- PhaseTapChangerNonLinear (CGMES 3.0) on winding 1 with a table -->
  <cim:PhaseTapChangerTable rdf:ID="PTCT1"/>
  <cim:PhaseTapChangerTablePoint rdf:ID="PTCTP0">
<cim:PhaseTapChangerTablePoint.PhaseTapChangerTable rdf:resource="#PTCT1"/>
<cim:TapChangerTablePoint.step>0</cim:TapChangerTablePoint.step>
<cim:PhaseTapChangerTablePoint.angle>0.0</cim:PhaseTapChangerTablePoint.angle>
<cim:TapChangerTablePoint.ratio>1.0</cim:TapChangerTablePoint.ratio>
  </cim:PhaseTapChangerTablePoint>
  <cim:PhaseTapChangerTablePoint rdf:ID="PTCTP1">
<cim:PhaseTapChangerTablePoint.PhaseTapChangerTable rdf:resource="#PTCT1"/>
<cim:TapChangerTablePoint.step>1</cim:TapChangerTablePoint.step>
<cim:PhaseTapChangerTablePoint.angle>5.0</cim:PhaseTapChangerTablePoint.angle>
<cim:TapChangerTablePoint.ratio>1.0</cim:TapChangerTablePoint.ratio>
  </cim:PhaseTapChangerTablePoint>
  <cim:PhaseTapChangerTablePoint rdf:ID="PTCTP2">
<cim:PhaseTapChangerTablePoint.PhaseTapChangerTable rdf:resource="#PTCT1"/>
<cim:TapChangerTablePoint.step>2</cim:TapChangerTablePoint.step>
<cim:PhaseTapChangerTablePoint.angle>10.0</cim:PhaseTapChangerTablePoint.angle>
<cim:TapChangerTablePoint.ratio>1.0</cim:TapChangerTablePoint.ratio>
  </cim:PhaseTapChangerTablePoint>
  <cim:PhaseTapChangerNonLinear rdf:ID="PTCNL1">
<cim:TapChanger.TransformerEnd rdf:resource="#PTE1"/>
<cim:PhaseTapChangerNonLinear.PhaseTapChangerTable rdf:resource="#PTCT1"/>
<cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>
<cim:TapChanger.lowStep>0</cim:TapChanger.lowStep>
<cim:TapChanger.highStep>2</cim:TapChanger.highStep>
  </cim:PhaseTapChangerNonLinear>
  <!-- SvTapStep: current step = 1 → should give 5° shift -->
  <cim:SvTapStep rdf:ID="STS1">
<cim:SvTapStep.TapChanger rdf:resource="#PTCNL1"/>
<cim:SvTapStep.position>1</cim:SvTapStep.position>
  </cim:SvTapStep>
  <!-- Slack gen and load for bus typing -->
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-50.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>50.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC2">
<cim:EnergyConsumer.p>100.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TEC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("CGMES 3.0 PhaseTapChangerNonLinear parse failed");
    assert_eq!(net.branches.len(), 1, "1 transformer branch expected");
    let br = &net.branches[0];
    // Step 1 → 5° from table; Branch.phase_shift_rad is stored in radians
    let expected_shift_rad = 5.0_f64.to_radians();
    assert!(
        (br.phase_shift_rad - expected_shift_rad).abs() < 0.001,
        "PhaseTapChangerNonLinear shift={} rad expected={expected_shift_rad} rad",
        br.phase_shift_rad
    );
}

// ── Wave 2: ApparentPowerLimit, Terminal.connected, GU.minQ/maxQ ────────

/// ApparentPowerLimit.value (MVA) is used as thermal rating when present.
#[test]
fn test_cgmes_apparent_power_limit() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- OperationalLimitSet with ApparentPowerLimit = 500 MVA -->
  <cim:OperationalLimitSet rdf:ID="OLS_L12">
<cim:OperationalLimitSet.Terminal rdf:resource="#L12_T1"/>
  </cim:OperationalLimitSet>
  <cim:ApparentPowerLimit rdf:ID="APL_L12">
<cim:ApparentPowerLimit.value>500.0</cim:ApparentPowerLimit.value>
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS_L12"/>
  </cim:ApparentPowerLimit>
  <!-- Slack gen and load for connectivity -->
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-200.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    // rate_a should be set to 500 MVA from ApparentPowerLimit
    let br = &net.branches[0];
    assert!(
        (br.rating_a_mva - 500.0).abs() < 1e-6,
        "ApparentPowerLimit not applied: rate_a={}, expected 500",
        br.rating_a_mva
    );
}

/// Terminal.connected=false causes the equipment to be skipped.
#[test]
fn test_cgmes_terminal_connected_false_skips_equipment() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_3">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <!-- Line 1-2: always connected -->
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <!-- Line 1-3: disconnected in SSH (Terminal.connected=false) — should be SKIPPED -->
  <cim:ACLineSegment rdf:ID="L13">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L13_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L13"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.connected>false</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L13_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L13"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_3"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-200.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // L13 has connected=false → skipped → only 1 branch (L12)
    assert_eq!(
        net.branches.len(),
        1,
        "disconnected line L13 should be excluded; branches={:?}",
        net.branches
            .iter()
            .map(|b| (b.from_bus, b.to_bus))
            .collect::<Vec<_>>()
    );
}

/// GeneratingUnit.minQ/maxQ are used as Q-limit fallback when SynchronousMachine
/// does not have minQ/maxQ attributes.
#[test]
fn test_cgmes_generating_unit_q_limits_fallback() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- GeneratingUnit with Q-limits only on the GU (not on SM) -->
  <cim:ThermalGeneratingUnit rdf:ID="GU_1">
<cim:GeneratingUnit.maxOperatingP>400.0</cim:GeneratingUnit.maxOperatingP>
<cim:GeneratingUnit.minOperatingP>50.0</cim:GeneratingUnit.minOperatingP>
<cim:GeneratingUnit.maxQ>150.0</cim:GeneratingUnit.maxQ>
<cim:GeneratingUnit.minQ>-75.0</cim:GeneratingUnit.minQ>
  </cim:ThermalGeneratingUnit>
  <!-- SynchronousMachine: NO minQ/maxQ here — should use GU fallback -->
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.GeneratingUnit rdf:resource="#GU_1"/>
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-200.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let g = net
        .generators
        .iter()
        .find(|g| g.p.abs() > 0.0)
        .expect("generator not found");
    assert!(
        (g.qmax - 150.0).abs() < 1e-6,
        "GU.maxQ fallback not applied: qmax={}, expected 150",
        g.qmax
    );
    assert!(
        (g.qmin - (-75.0)).abs() < 1e-6,
        "GU.minQ fallback not applied: qmin={}, expected -75",
        g.qmin
    );
}

// ── Wave 1: New equipment type tests ────────────────────────────────────

/// SeriesCompensator (positive x = reactor) creates a branch between two buses.
#[test]
fn test_cgmes_series_compensator_reactor() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_400">
<cim:BaseVoltage.nominalVoltage>400.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_A">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_400"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_B">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_400"/>
  </cim:TopologicalNode>
  <!-- SeriesCompensator: r=0 Ω, x=40 Ω (reactor) -->
  <cim:SeriesCompensator rdf:ID="SC_1">
<cim:SeriesCompensator.r>0.0</cim:SeriesCompensator.r>
<cim:SeriesCompensator.x>40.0</cim:SeriesCompensator.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_400"/>
  </cim:SeriesCompensator>
  <cim:Terminal rdf:ID="SC1_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#SC_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_A"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="SC1_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#SC_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_B"/>
  </cim:Terminal>
  <!-- Slack generator on TN_A -->
  <cim:SynchronousMachine rdf:ID="SM_A">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-50.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>50.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SMA">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_A"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_A"/>
  </cim:Terminal>
  <!-- Load on TN_B -->
  <cim:EnergyConsumer rdf:ID="EC_B">
<cim:EnergyConsumer.p>100.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_ECB">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_B"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_B"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 2, "expected 2 buses");
    // The SeriesCompensator should create exactly 1 branch.
    assert_eq!(
        net.branches.len(),
        1,
        "expected 1 branch from SeriesCompensator"
    );
    let br = &net.branches[0];
    // x = 40 Ω at 400 kV, 100 MVA base → x_pu = 40 × 100 / (400²) = 0.025 pu
    assert!(br.r.abs() < 1e-9, "r should be 0");
    let expected_x = 40.0 * 100.0 / (400.0_f64 * 400.0);
    assert!(
        (br.x - expected_x).abs() < 1e-6,
        "x_pu mismatch: got {}, expected {expected_x}",
        br.x
    );
}

/// SeriesCompensator with negative x (capacitor) is preserved with correct sign.
#[test]
fn test_cgmes_series_compensator_capacitor_negative_x() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_500">
<cim:BaseVoltage.nominalVoltage>500.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_A">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_500"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_B">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_500"/>
  </cim:TopologicalNode>
  <!-- Capacitor: x = -25 Ω (negative) -->
  <cim:SeriesCompensator rdf:ID="SC_CAP">
<cim:SeriesCompensator.r>0.0</cim:SeriesCompensator.r>
<cim:SeriesCompensator.x>-25.0</cim:SeriesCompensator.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_500"/>
  </cim:SeriesCompensator>
  <cim:Terminal rdf:ID="SC_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#SC_CAP"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_A"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="SC_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#SC_CAP"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_B"/>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_A">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-20.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>20.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SMA">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_A"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_A"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_B">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_ECB">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_B"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_B"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // x = -25 Ω at 500 kV, 100 MVA → x_pu = -25 × 100 / 250000 = -0.01
    let expected_x = -25.0 * 100.0 / (500.0_f64 * 500.0);
    assert!(
        (br.x - expected_x).abs() < 1e-6,
        "capacitor negative x not preserved: got {}, expected {expected_x}",
        br.x
    );
    assert!(br.x < 0.0, "capacitor x must be negative");
}

/// EquivalentBranch creates a branch using the average of R12/R21 and X12/X21.
#[test]
fn test_cgmes_equivalent_branch() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <!-- EquivalentBranch: R12=2Ω, R21=2Ω, X12=20Ω, X21=22Ω -->
  <cim:EquivalentBranch rdf:ID="EB_1">
<cim:EquivalentBranch.positiveR12>2.0</cim:EquivalentBranch.positiveR12>
<cim:EquivalentBranch.positiveR21>2.0</cim:EquivalentBranch.positiveR21>
<cim:EquivalentBranch.positiveX12>20.0</cim:EquivalentBranch.positiveX12>
<cim:EquivalentBranch.positiveX21>22.0</cim:EquivalentBranch.positiveX21>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:EquivalentBranch>
  <cim:Terminal rdf:ID="EB1_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#EB_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="EB1_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#EB_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-80.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-30.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>30.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>80.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1, "expected 1 EquivalentBranch");
    let br = &net.branches[0];
    // r = avg(2,2)=2 Ω; x = avg(20,22)=21 Ω; base 220 kV, 100 MVA
    // r_pu = 2×100/220² = 0.004132...; x_pu = 21×100/220² = 0.043388...
    let z_base = 220.0_f64 * 220.0 / 100.0;
    let expected_r = 2.0 / z_base;
    let expected_x = 21.0 / z_base;
    assert!(
        (br.r - expected_r).abs() < 1e-6,
        "r_pu mismatch: got {}, expected {expected_r}",
        br.r
    );
    assert!(
        (br.x - expected_x).abs() < 1e-6,
        "x_pu mismatch: got {}, expected {expected_x}",
        br.x
    );
}

/// StaticVarCompensator q injection reduces bus qd.
#[test]
fn test_cgmes_static_var_compensator() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- SVC injecting 60 MVAr (capacitive) on bus 2 -->
  <cim:StaticVarCompensator rdf:ID="SVC_2">
<cim:StaticVarCompensator.q>60.0</cim:StaticVarCompensator.q>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:StaticVarCompensator>
  <cim:Terminal rdf:ID="T_SVC2">
<cim:Terminal.ConductingEquipment rdf:resource="#SVC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-200.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- Load on bus 2: 200 MW, 100 MVAr -->
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>100.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let bus2 = net
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::PQ)
        .unwrap();
    // qd = load.q(100) - SVC.q(60) = 40 MVAr
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    let bus2_idx = net
        .buses
        .iter()
        .position(|b| b.number == bus2.number)
        .unwrap();
    assert!(
        (bus_qd[bus2_idx] - 40.0).abs() < 1e-6,
        "SVC Q not subtracted correctly: qd={}, expected 40",
        bus_qd[bus2_idx]
    );
    assert!(
        (bus_pd[bus2_idx] - 200.0).abs() < 1e-6,
        "pd should be unchanged: pd={}, expected 200",
        bus_pd[bus2_idx]
    );
}

/// EquivalentShunt adds susceptance to bus shunt (bs).
#[test]
fn test_cgmes_equivalent_shunt() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_110"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- EquivalentShunt: b = 0.001 S on bus 2 -->
  <cim:EquivalentShunt rdf:ID="EQSH_2">
<cim:EquivalentShunt.b>0.001</cim:EquivalentShunt.b>
<cim:EquivalentShunt.g>0.0</cim:EquivalentShunt.g>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_110"/>
  </cim:EquivalentShunt>
  <cim:Terminal rdf:ID="T_EQSH2">
<cim:Terminal.ConductingEquipment rdf:resource="#EQSH_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-20.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>20.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let bus2 = net
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::PQ)
        .unwrap();
    // bs = b × kV² = 0.001 S × 110² kV² = 12.1 MVAr
    let expected_bs = 0.001 * 110.0_f64 * 110.0;
    assert!(
        (bus2.shunt_susceptance_mvar - expected_bs).abs() < 1e-6,
        "EquivalentShunt bs mismatch: got {}, expected {expected_bs}",
        bus2.shunt_susceptance_mvar
    );
}

/// AsynchronousMachine is parsed as a PQ load.
#[test]
fn test_cgmes_asynchronous_machine_as_load() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_11">
<cim:BaseVoltage.nominalVoltage>11.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_HV">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_LV">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_11"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L_HV_LV">
<cim:ACLineSegment.r>0.5</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L_HV_LV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L_HV_LV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_LV"/>
  </cim:Terminal>
  <!-- Slack generator on HV bus -->
  <cim:SynchronousMachine rdf:ID="SM_HV">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-500.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-200.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>200.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_HV">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_HV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
  </cim:Terminal>
  <!-- AsynchronousMachine (large motor): 300 MW, 120 MVAr consumed -->
  <cim:AsynchronousMachine rdf:ID="AM_LV">
<cim:RotatingMachine.p>300.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>120.0</cim:RotatingMachine.q>
  </cim:AsynchronousMachine>
  <cim:Terminal rdf:ID="T_AM_LV">
<cim:Terminal.ConductingEquipment rdf:resource="#AM_LV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_LV"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // Should have 1 Load entry for the AsynchronousMachine
    assert!(
        net.loads
            .iter()
            .any(|l| (l.active_power_demand_mw - 300.0).abs() < 1e-6
                && (l.reactive_power_demand_mvar - 120.0).abs() < 1e-6),
        "AsynchronousMachine not found as PQ load with pd=300, qd=120. loads={:?}",
        net.loads
    );
    // Find the LV bus; it should have pd=300 via Load objects
    let lv_bus = net.buses.iter().find(|b| (b.base_kv - 11.0).abs() < 1.0);
    if let Some(b) = lv_bus {
        let lv_pd: f64 = net
            .loads
            .iter()
            .filter(|l| l.bus == b.number)
            .map(|l| l.active_power_demand_mw)
            .sum();
        assert!(
            (lv_pd - 300.0).abs() < 1e-6,
            "LV bus pd mismatch: got {}, expected 300",
            lv_pd
        );
    }
}

/// ExternalNetworkInjection P/Q is applied to the bus load balance.
#[test]
fn test_cgmes_external_network_injection_pq() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_380">
<cim:BaseVoltage.nominalVoltage>380.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_380"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_380"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_380"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- ExternalNetworkInjection = slack + injects 500 MW, 100 MVAr -->
  <cim:ExternalNetworkInjection rdf:ID="ENI_1">
<cim:ExternalNetworkInjection.referencePriority>1</cim:ExternalNetworkInjection.referencePriority>
<cim:ExternalNetworkInjection.p>500.0</cim:ExternalNetworkInjection.p>
<cim:ExternalNetworkInjection.q>100.0</cim:ExternalNetworkInjection.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:ExternalNetworkInjection>
  <cim:Terminal rdf:ID="T_ENI1">
<cim:Terminal.ConductingEquipment rdf:resource="#ENI_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- Load on bus 2: 500 MW, 100 MVAr -->
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>500.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>100.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // Bus 1 is the slack (ENI with referencePriority=1).
    let bus1 = net
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::Slack)
        .unwrap();
    // ENI p=500 (injection) → net load at bus = -500 (net injection)
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    let bus1_idx = net
        .buses
        .iter()
        .position(|b| b.number == bus1.number)
        .unwrap();
    assert!(
        (bus_pd[bus1_idx] - (-500.0)).abs() < 1e-6,
        "ENI injection not applied: bus1 net pd={}, expected -500",
        bus_pd[bus1_idx]
    );
    assert!(
        (bus_qd[bus1_idx] - (-100.0)).abs() < 1e-6,
        "ENI Q injection not applied: bus1 net qd={}, expected -100",
        bus_qd[bus1_idx]
    );
}

/// StationSupply is parsed as a PQ load.
#[test]
fn test_cgmes_station_supply_as_load() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="L12">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
<cim:ConductingEquipment.BaseVoltage rdf:resource="#BV_220"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="L12_T1">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="L12_T2">
<cim:Terminal.ConductingEquipment rdf:resource="#L12"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-10.0</cim:RotatingMachine.p>
<cim:SynchronousMachine.minQ>-5.0</cim:SynchronousMachine.minQ>
<cim:SynchronousMachine.maxQ>5.0</cim:SynchronousMachine.maxQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- Main load on bus 2 -->
  <cim:EnergyConsumer rdf:ID="EC_2">
<cim:EnergyConsumer.p>8.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>2.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC2">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- StationSupply (auxiliary): 2 MW, 0.5 MVAr -->
  <cim:StationSupply rdf:ID="SS_2">
<cim:StationSupply.p>2.0</cim:StationSupply.p>
<cim:StationSupply.q>0.5</cim:StationSupply.q>
  </cim:StationSupply>
  <cim:Terminal rdf:ID="T_SS2">
<cim:Terminal.ConductingEquipment rdf:resource="#SS_2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let bus2 = net
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::PQ)
        .unwrap();
    // pd = EC(8) + SS(2) = 10 MW; qd = EC(2) + SS(0.5) = 2.5 MVAr
    let bus2_pd: f64 = net
        .loads
        .iter()
        .filter(|l| l.bus == bus2.number)
        .map(|l| l.active_power_demand_mw)
        .sum();
    let bus2_qd: f64 = net
        .loads
        .iter()
        .filter(|l| l.bus == bus2.number)
        .map(|l| l.reactive_power_demand_mvar)
        .sum();
    assert!(
        (bus2_pd - 10.0).abs() < 1e-6,
        "StationSupply pd not accumulated: got {}, expected 10",
        bus2_pd
    );
    assert!(
        (bus2_qd - 2.5).abs() < 1e-6,
        "StationSupply qd not accumulated: got {}, expected 2.5",
        bus2_qd
    );
    // Should have 2 load entries: EnergyConsumer + StationSupply
    assert_eq!(net.loads.len(), 2, "expected 2 loads");
}

// -----------------------------------------------------------------------
// Wave 4 tests
// -----------------------------------------------------------------------

/// Unit test for interpolate_rcc helper — exercises clamping and interpolation.
#[test]
fn test_rcc_interpolation_helper() {
    // 3-point curve: P=-100 → (Qmin=-200, Qmax=200),
    //                P=0   → (Qmin=-300, Qmax=300),
    //                P=100 → (Qmin=-200, Qmax=200)
    let pts: Vec<(f64, f64, f64)> = vec![
        (-100.0, -200.0, 200.0),
        (0.0, -300.0, 300.0),
        (100.0, -200.0, 200.0),
    ];

    // Left clamp
    let (qmin, qmax) = interpolate_rcc(&pts, -150.0);
    assert!((qmin - (-200.0)).abs() < 1e-10, "left clamp qmin");
    assert!((qmax - 200.0).abs() < 1e-10, "left clamp qmax");

    // Right clamp
    let (qmin, qmax) = interpolate_rcc(&pts, 200.0);
    assert!((qmin - (-200.0)).abs() < 1e-10, "right clamp qmin");
    assert!((qmax - 200.0).abs() < 1e-10, "right clamp qmax");

    // Exact middle point
    let (qmin, qmax) = interpolate_rcc(&pts, 0.0);
    assert!((qmin - (-300.0)).abs() < 1e-10, "exact mid qmin");
    assert!((qmax - 300.0).abs() < 1e-10, "exact mid qmax");

    // Midpoint between first two points (t = 0.5)
    // qmin = -200 + 0.5 * (-300 - (-200)) = -250
    // qmax = 200 + 0.5 * (300 - 200) = 250
    let (qmin, qmax) = interpolate_rcc(&pts, -50.0);
    assert!((qmin - (-250.0)).abs() < 1e-10, "interp qmin at -50");
    assert!((qmax - 250.0).abs() < 1e-10, "interp qmax at -50");

    // Empty curve → defaults
    let (qmin, qmax) = interpolate_rcc(&[], 0.0);
    assert!((qmin - (-9999.0)).abs() < 1e-10);
    assert!((qmax - 9999.0).abs() < 1e-10);

    // Single point
    let one = vec![(50.0f64, -100.0f64, 100.0f64)];
    let (qmin, qmax) = interpolate_rcc(&one, -999.0);
    assert!((qmin - (-100.0)).abs() < 1e-10);
    assert!((qmax - 100.0).abs() < 1e-10);
}

/// ReactiveCapabilityCurve from CurveData overrides static SM.maxQ/minQ.
///
/// SSH p = -90 MW (generating 90 MW in IEC convention).
/// RCC: two points at P=-150 (Qmin=-80, Qmax=80) and P=0 (Qmin=-120, Qmax=120).
/// At p_ssh=-90: t = (-90 - (-150)) / (0 - (-150)) = 60/150 = 0.4
///   Qmin = -80 + 0.4*(-120 - (-80)) = -80 - 16 = -96
///   Qmax =  80 + 0.4*(120 -  80)   =  80 + 16 =  96
/// Static SM.maxQ=500 / minQ=-500 should be overridden.
#[test]
fn test_cgmes_reactive_capability_curve_overrides_static_q_limits() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <!-- Topology -->
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <!-- Branch to connect the two buses -->
  <cim:ACLineSegment rdf:ID="LINE_1">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_L1_1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_L1_2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- ReactiveCapabilityCurve and its points -->
  <cim:ReactiveCapabilityCurve rdf:ID="RCC_1">
<cim:IdentifiedObject.name>RCC_G1</cim:IdentifiedObject.name>
  </cim:ReactiveCapabilityCurve>
  <cim:CurveData rdf:ID="CD_1">
<cim:CurveData.xvalue>-150.0</cim:CurveData.xvalue>
<cim:CurveData.y1value>-80.0</cim:CurveData.y1value>
<cim:CurveData.y2value>80.0</cim:CurveData.y2value>
<cim:CurveData.Curve rdf:resource="#RCC_1"/>
  </cim:CurveData>
  <cim:CurveData rdf:ID="CD_2">
<cim:CurveData.xvalue>0.0</cim:CurveData.xvalue>
<cim:CurveData.y1value>-120.0</cim:CurveData.y1value>
<cim:CurveData.y2value>120.0</cim:CurveData.y2value>
<cim:CurveData.Curve rdf:resource="#RCC_1"/>
  </cim:CurveData>
  <!-- SynchronousMachine: static Q limits are wide but RCC should override -->
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.InitialReactiveCapabilityCurve rdf:resource="#RCC_1"/>
<cim:SynchronousMachine.maxQ>500.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-500.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-90.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-30.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- Load -->
  <cim:EnergyConsumer rdf:ID="EC_1">
<cim:EnergyConsumer.p>80.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>20.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC_1">
<cim:Terminal.ConductingEquipment rdf:resource="#EC_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    let g = net.generators.first().expect("no generators");

    // RCC override: at p_ssh=-90, interpolating between (-150,-80,80) and (0,-120,120)
    // t = (-90-(-150))/(0-(-150)) = 60/150 = 0.4
    let expected_qmin = -80.0 + 0.4 * (-120.0 - (-80.0)); // = -96.0
    let expected_qmax = 80.0 + 0.4 * (120.0 - 80.0); // =  96.0
    assert!(
        (g.qmin - expected_qmin).abs() < 1e-6,
        "RCC qmin: got {}, expected {expected_qmin}",
        g.qmin
    );
    assert!(
        (g.qmax - expected_qmax).abs() < 1e-6,
        "RCC qmax: got {}, expected {expected_qmax}",
        g.qmax
    );
    // pq_curve should be populated (2 points)
    assert_eq!(
        g.reactive_capability
            .as_ref()
            .map_or(0, |r| r.pq_curve.len()),
        2,
        "pq_curve should have 2 points"
    );
}

/// PetersenCoil elements are silently skipped (no branches, loads, or generators created).
#[test]
fn test_cgmes_petersen_coil_skipped() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>50.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-50.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-30.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-10.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- PetersenCoil: connected to neutral bus (Phase N), should be silently skipped -->
  <cim:PetersenCoil rdf:ID="PC_1">
<cim:IdentifiedObject.name>PET_COIL_1</cim:IdentifiedObject.name>
<cim:Equipment.EquipmentContainer rdf:resource="#TN_1"/>
<cim:PetersenCoil.xGroundNominal>4.99</cim:PetersenCoil.xGroundNominal>
<cim:PetersenCoil.nominalU>110.0</cim:PetersenCoil.nominalU>
  </cim:PetersenCoil>
  <cim:Terminal rdf:ID="T_PC_1">
<cim:Terminal.ConductingEquipment rdf:resource="#PC_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:Terminal.phases rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#PhaseCode.N"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // PetersenCoil should NOT create any branches or loads
    assert_eq!(
        net.branches.len(),
        0,
        "PetersenCoil must not create branches"
    );
    assert_eq!(net.loads.len(), 0, "PetersenCoil must not create loads");
    // The generator should still be created normally
    assert_eq!(net.generators.len(), 1, "generator should be unaffected");
}

/// BusbarSection elements are silently skipped (transparent to bus/branch model).
#[test]
fn test_cgmes_busbar_section_skipped() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_400"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_400">
<cim:BaseVoltage.nominalVoltage>400.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-10.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- BusbarSection: physical busbar rail, transparent to power flow model -->
  <cim:BusbarSection rdf:ID="BB_1">
<cim:IdentifiedObject.name>BE-Busbar_1</cim:IdentifiedObject.name>
<cim:Equipment.EquipmentContainer rdf:resource="#TN_1"/>
<cim:BusbarSection.ipMax>1000.0</cim:BusbarSection.ipMax>
  </cim:BusbarSection>
  <cim:Terminal rdf:ID="T_BB_1">
<cim:Terminal.ConductingEquipment rdf:resource="#BB_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // BusbarSection must not create branches or loads
    assert_eq!(
        net.branches.len(),
        0,
        "BusbarSection must not create branches"
    );
    assert_eq!(net.loads.len(), 0, "BusbarSection must not create loads");
    assert_eq!(net.generators.len(), 1, "generator should be unaffected");
}

// -----------------------------------------------------------------------
// Wave 5 regression tests for tap-changer loop break→continue fixes
// -----------------------------------------------------------------------

/// When a RatioTapChanger mRID listed in rtc_by_end is NOT present in the
/// object store (e.g., partial CGMES export), the RTC loop must `continue`
/// to the next winding end rather than `break` out of the loop entirely.
/// This test places the RTC on End2 only — a file that omits the End1 RTC
/// object would previously drop End2's tap silently.
///
/// Here we give End1 an RTC reference that resolves but End2 also has one.
/// The loop applies whichever is first found and breaks.  The tap should be
/// non-unity (1 + (step=1 - neutral=2) × 4% = 0.96).
#[test]
fn test_cgmes_rtc_on_end2_only_applies_tap() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <!-- Transformer: 220/110 kV with RTC on End2 only -->
  <cim:PowerTransformer rdf:ID="XFMR_1"/>
  <cim:PowerTransformerEnd rdf:ID="END_1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_XFMR_1"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END_2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_XFMR_2"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<!-- RTC on End2 -->
<cim:TransformerEnd.RatioTapChanger rdf:resource="#RTC_E2"/>
  </cim:PowerTransformerEnd>
  <!-- RTC only on End2; no RTC on End1 -->
  <cim:RatioTapChanger rdf:ID="RTC_E2">
<cim:TapChanger.neutralStep>2</cim:TapChanger.neutralStep>
<cim:TapChanger.step>1</cim:TapChanger.step>
<cim:TapChanger.stepVoltageIncrement>4.0</cim:TapChanger.stepVoltageIncrement>
<cim:RatioTapChanger.TransformerEnd rdf:resource="#END_2"/>
  </cim:RatioTapChanger>
  <cim:Terminal rdf:ID="T_XFMR_1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_XFMR_2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Slack generator -->
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-10.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1, "transformer branch must be created");
    let br = &net.branches[0];
    // MAJ-02: RTC on End2 inverts the tap ratio (MATPOWER convention: tap is End1 ratio).
    // nominal tap = (220/110)*(110/220) = 1.0
    // End2 step-based ratio = 1 + (step=1 - neutral=2) * 4%/100 = 0.96
    // End2 correction: tap /= 0.96  →  tap = 1.0 / 0.96 ≈ 1.04167
    let end2_ratio = 1.0 + (1.0 - 2.0) * 4.0 / 100.0; // 0.96
    let expected_tap = 1.0 / end2_ratio; // ≈ 1.04167
    assert!(
        (br.tap - expected_tap).abs() < 1e-6,
        "RTC on End2 must invert tap (tap /= ratio): got tap={}, expected {expected_tap}",
        br.tap
    );
}

/// A DanglingLine's shunt admittance (b, g in S) must be applied to the
/// connected bus, and its SSH p/q injection must shift pd/qd of that bus.
#[test]
fn test_cgmes_dangling_line_shunt_and_injection() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_100"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_100">
<cim:BaseVoltage.nominalVoltage>100.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <!-- Slack machine on Bus1 -->
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- DanglingLine: b=0.001 S, g=0.0002 S, p=10 MW, q=5 Mvar (generating) -->
  <cim:DanglingLine rdf:ID="DL_1">
<cim:DanglingLine.b>0.001</cim:DanglingLine.b>
<cim:DanglingLine.g>0.0002</cim:DanglingLine.g>
<cim:DanglingLine.p>10.0</cim:DanglingLine.p>
<cim:DanglingLine.q>5.0</cim:DanglingLine.q>
  </cim:DanglingLine>
  <cim:Terminal rdf:ID="T_DL">
<cim:Terminal.ConductingEquipment rdf:resource="#DL_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 1, "only Bus1 expected");
    let bus = &net.buses[0];

    // Shunt: b_mvar = b_S * kV² = 0.001 * 100² = 10 Mvar; g_mw = 0.0002 * 10000 = 2 MW
    assert!(
        (bus.shunt_susceptance_mvar - 10.0).abs() < 1e-6,
        "DanglingLine b must be added to bus.shunt_susceptance_mvar: got {}",
        bus.shunt_susceptance_mvar
    );
    assert!(
        (bus.shunt_conductance_mw - 2.0).abs() < 1e-6,
        "DanglingLine g must be added to bus.shunt_conductance_mw: got {}",
        bus.shunt_conductance_mw
    );
    // SSH P/Q injection: p=10 MW (generating at boundary) → pd reduced by 10
    // DanglingLine p/q are power flowing INTO the line (load convention like EI),
    // so pd -= p → net pd = 0 - 10 = -10; qd = 0 - 5 = -5
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    assert!(
        (bus_pd[0] - (-10.0)).abs() < 1e-3,
        "DanglingLine p=10 MW must reduce pd: got pd={}",
        bus_pd[0]
    );
    assert!(
        (bus_qd[0] - (-5.0)).abs() < 1e-3,
        "DanglingLine q=5 Mvar must reduce qd: got qd={}",
        bus_qd[0]
    );
}

#[test]
fn test_cgmes_dangling_line_roundtrip_preserves_class() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_100"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_100">
<cim:BaseVoltage.nominalVoltage>100.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:DanglingLine rdf:ID="DL_1">
<cim:DanglingLine.r>2.5</cim:DanglingLine.r>
<cim:DanglingLine.x>30.0</cim:DanglingLine.x>
<cim:DanglingLine.b>0.001</cim:DanglingLine.b>
<cim:DanglingLine.g>0.0002</cim:DanglingLine.g>
<cim:DanglingLine.p>10.0</cim:DanglingLine.p>
<cim:DanglingLine.q>5.0</cim:DanglingLine.q>
  </cim:DanglingLine>
  <cim:Terminal rdf:ID="T_DL">
<cim:Terminal.ConductingEquipment rdf:resource="#DL_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    let (profiles, reparsed) = roundtrip_v2_profiles(&net);

    assert!(
        profiles.eq.contains("<cim:DanglingLine rdf:ID=\"DL_1\">"),
        "writer should preserve DanglingLine class"
    );
    assert!(
        profiles.eq.contains("DanglingLine.r>2.5<") && profiles.eq.contains("DanglingLine.x>30<"),
        "writer should preserve DanglingLine series data"
    );
    assert!(
        !profiles
            .eq
            .contains("<cim:LinearShuntCompensator rdf:ID=\"_FSH_0\">"),
        "writer must not degrade DanglingLine into a generic fixed shunt"
    );

    assert!(
        reparsed.fixed_shunts.iter().any(|shunt| shunt.id == "DL_1"),
        "round-tripped DanglingLine shunt contribution should survive"
    );
    assert!(
        reparsed
            .power_injections
            .iter()
            .any(|injection| injection.id == "DL_1"),
        "round-tripped DanglingLine P/Q contribution should survive"
    );
    assert!(
        reparsed
            .cim
            .cgmes_roundtrip
            .dangling_lines
            .contains_key("DL_1"),
        "round-tripped DanglingLine source object should still be preserved"
    );
}

/// A StaticVarCompensator whose SSH Terminal has connected=false must be
/// completely skipped (no shunt contribution to the connected bus).
#[test]
fn test_cgmes_svc_disconnected_terminal_skipped() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_100"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_100">
<cim:BaseVoltage.nominalVoltage>100.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- SVC with large Q but its terminal is disconnected -->
  <cim:StaticVarCompensator rdf:ID="SVC_1">
<cim:StaticVarCompensator.q>50.0</cim:StaticVarCompensator.q>
  </cim:StaticVarCompensator>
  <cim:Terminal rdf:ID="T_SVC">
<cim:Terminal.ConductingEquipment rdf:resource="#SVC_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<!-- SSH: connected = false -->
<cim:ACDCTerminal.connected>false</cim:ACDCTerminal.connected>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 1);
    let bus = &net.buses[0];
    // Bus should have zero shunt — the disconnected SVC must be skipped entirely
    assert!(
        bus.shunt_susceptance_mvar.abs() < 1e-9,
        "Disconnected SVC must not contribute to bus.shunt_susceptance_mvar: got {}",
        bus.shunt_susceptance_mvar
    );
    let bus_qd = net.bus_load_q_mvar();
    assert!(
        bus_qd[0].abs() < 1e-9,
        "Disconnected SVC must not contribute to qd: got {}",
        bus_qd[0]
    );
}

/// SVC with sVCControlMode=voltageControl must produce a PV generator (Pg=0,
/// Q limits from bMin/bMax × V²) rather than a fixed Q injection.
#[test]
fn test_cgmes_svc_voltage_control_creates_generator() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_100"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_100">
<cim:BaseVoltage.nominalVoltage>100.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <!-- Slack machine -->
  <cim:SynchronousMachine rdf:ID="SM_SLACK">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-200.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_SLACK"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- SVC in voltageControl mode: bMax=0.05 S, bMin=-0.02 S at 100 kV bus -->
  <!-- Qmax = 0.05 × 100² = 500 MVAr; Qmin = -0.02 × 100² = -200 MVAr -->
  <cim:StaticVarCompensator rdf:ID="SVC_VC">
<cim:StaticVarCompensator.sVCControlMode
    rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#SVCControlMode.voltageControl"/>
<cim:StaticVarCompensator.bMax>0.05</cim:StaticVarCompensator.bMax>
<cim:StaticVarCompensator.bMin>-0.02</cim:StaticVarCompensator.bMin>
<cim:StaticVarCompensator.q>30.0</cim:StaticVarCompensator.q>
  </cim:StaticVarCompensator>
  <cim:Terminal rdf:ID="T_SVC">
<cim:Terminal.ConductingEquipment rdf:resource="#SVC_VC"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // 2 generators: slack SM + voltage-controlling SVC
    assert_eq!(
        net.generators.len(),
        2,
        "expect SM + SVC generator; got {:?}",
        net.generators.len()
    );
    // Find the SVC generator (Pg = 0, Pmax = 0)
    let svc_gen = net
        .generators
        .iter()
        .find(|g| g.p.abs() < 1e-6 && g.pmax.abs() < 1e-6);
    let svc_gen = svc_gen.expect("SVC voltage-control generator not found");
    assert!(
        (svc_gen.qmax - 500.0).abs() < 1.0,
        "Qmax = bMax × V² = 0.05 × 100² = 500: got {}",
        svc_gen.qmax
    );
    assert!(
        (svc_gen.qmin - (-200.0)).abs() < 1.0,
        "Qmin = bMin × V² = -0.02 × 100² = -200: got {}",
        svc_gen.qmin
    );
    // Bus qd must NOT have the SVC q subtracted (it's now handled by the generator)
    let bus_qd = net.bus_load_q_mvar();
    assert!(
        bus_qd[0].abs() < 1e-6,
        "voltageControl SVC must not subtract q from bus net qd: got {}",
        bus_qd[0]
    );
}

/// EquivalentInjection with controlEnabled=true AND regulationStatus=true must
/// be modeled as a PV generator, not a PQ injection.
#[test]
fn test_cgmes_equivalent_injection_voltage_control() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <!-- Slack machine -->
  <cim:SynchronousMachine rdf:ID="SM_SLACK">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-500.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_SLACK"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- EquivalentInjection with voltage regulation enabled -->
  <cim:EquivalentInjection rdf:ID="ENI_1">
<cim:EquivalentInjection.p>100.0</cim:EquivalentInjection.p>
<cim:EquivalentInjection.q>20.0</cim:EquivalentInjection.q>
<cim:EquivalentInjection.maxQ>300.0</cim:EquivalentInjection.maxQ>
<cim:EquivalentInjection.minQ>-150.0</cim:EquivalentInjection.minQ>
<!-- EQ: controlEnabled = true -->
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<!-- SSH: regulationStatus = true -->
<cim:EquivalentInjection.regulationStatus>true</cim:EquivalentInjection.regulationStatus>
  </cim:EquivalentInjection>
  <cim:Terminal rdf:ID="T_ENI">
<cim:Terminal.ConductingEquipment rdf:resource="#ENI_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // ENI should produce a generator, not a PQ injection
    assert_eq!(
        net.generators.len(),
        2,
        "expect SM + ENI generator; got {}",
        net.generators.len()
    );
    // The ENI generator has Pg = p = 100, qmax = 300, qmin = -150
    let eni_gen = net.generators.iter().find(|g| (g.p - 100.0).abs() < 1.0);
    let eni_gen = eni_gen.expect("ENI voltage-regulating generator not found");
    assert!(
        (eni_gen.qmax - 300.0).abs() < 1e-6,
        "ENI qmax=300: got {}",
        eni_gen.qmax
    );
    assert!(
        (eni_gen.qmin - (-150.0)).abs() < 1e-6,
        "ENI qmin=-150: got {}",
        eni_gen.qmin
    );
    // Bus must NOT have pd/qd subtracted — injection handled via generator
    let bus_pd = net.bus_load_p_mw();
    assert!(
        bus_pd[0].abs() < 1e-6,
        "Voltage-regulating ENI must not add to bus net pd: got {}",
        bus_pd[0]
    );
}

#[test]
fn test_cgmes_equivalent_injection_roundtrip_preserves_regulating_class() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_SLACK">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-500.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_SLACK"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:EquivalentInjection rdf:ID="EI_1">
<cim:EquivalentInjection.p>100.0</cim:EquivalentInjection.p>
<cim:EquivalentInjection.q>20.0</cim:EquivalentInjection.q>
<cim:EquivalentInjection.maxQ>300.0</cim:EquivalentInjection.maxQ>
<cim:EquivalentInjection.minQ>-150.0</cim:EquivalentInjection.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:EquivalentInjection.regulationStatus>true</cim:EquivalentInjection.regulationStatus>
  </cim:EquivalentInjection>
  <cim:Terminal rdf:ID="T_EI">
<cim:Terminal.ConductingEquipment rdf:resource="#EI_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    let (profiles, reparsed) = roundtrip_v2_profiles(&net);

    assert!(
        profiles
            .eq
            .contains("<cim:EquivalentInjection rdf:ID=\"EI_1\">"),
        "writer should preserve EquivalentInjection class"
    );
    assert!(
        profiles
            .eq
            .contains("EquivalentInjection.regulationCapability>true<"),
        "writer should preserve regulating EquivalentInjection capability"
    );
    assert!(
        !profiles
            .eq
            .contains("<cim:SynchronousMachine rdf:ID=\"_SM_1\">"),
        "writer must not degrade regulating EquivalentInjection into a second generic SynchronousMachine"
    );

    let ei_gen = reparsed
        .generators
        .iter()
        .find(|generator| generator.machine_id.as_deref() == Some("EI_1"))
        .expect("round-tripped EquivalentInjection generator should survive");
    assert!(
        (ei_gen.qmax - 300.0).abs() < 1e-6 && (ei_gen.qmin + 150.0).abs() < 1e-6,
        "round-tripped regulating EquivalentInjection should preserve Q limits"
    );
    assert!(
        reparsed
            .cim
            .cgmes_roundtrip
            .equivalent_injections
            .contains_key("EI_1"),
        "round-tripped EquivalentInjection source object should still be preserved"
    );
}

/// EquivalentInjection with controlEnabled=false must remain a PQ injection
/// (pd/qd adjusted, no generator created).
#[test]
fn test_cgmes_equivalent_injection_pq_when_control_disabled() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_SLACK">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-300.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_SLACK"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- ENI with controlEnabled=false → PQ injection only -->
  <cim:EquivalentInjection rdf:ID="ENI_PQ">
<cim:EquivalentInjection.p>80.0</cim:EquivalentInjection.p>
<cim:EquivalentInjection.q>15.0</cim:EquivalentInjection.q>
<cim:RegulatingCondEq.controlEnabled>false</cim:RegulatingCondEq.controlEnabled>
<cim:EquivalentInjection.regulationStatus>false</cim:EquivalentInjection.regulationStatus>
  </cim:EquivalentInjection>
  <cim:Terminal rdf:ID="T_ENI">
<cim:Terminal.ConductingEquipment rdf:resource="#ENI_PQ"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // Only the slack SM should be a generator
    assert_eq!(
        net.generators.len(),
        1,
        "only slack SM; ENI must not create generator: got {}",
        net.generators.len()
    );
    // ENI p=80 injection → net bus load reduced by 80
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    assert!(
        (bus_pd[0] - (-80.0)).abs() < 1e-6,
        "ENI p=80 injection must reduce pd to -80: got {}",
        bus_pd[0]
    );
    assert!(
        (bus_qd[0] - (-15.0)).abs() < 1e-6,
        "ENI q=15 injection must reduce qd to -15: got {}",
        bus_qd[0]
    );
}

/// 2-winding transformer with non-zero PowerTransformerEnd.g (magnetizing conductance)
/// must have br.g_mag set on the resulting branch.
#[test]
fn test_cgmes_2winding_transformer_g_mag() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:PowerTransformer rdf:ID="XFMR_1"/>
  <!-- End1: 220kV, r=10 Ω, x=50 Ω, b=0.0001 S, g=0.00005 S (core loss) -->
  <cim:PowerTransformerEnd rdf:ID="END_1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_X1"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>10.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>50.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0001</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.00005</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END_2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_X2"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="T_X1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_X2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Load on Bus2 to avoid isolation -->
  <cim:EnergyConsumer rdf:ID="LOAD_1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_LOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // g = 0.00005 S at 220 kV, 100 MVA → g_pu = 0.00005 × 220² / 100 = 0.0242
    let expected_g_pu = 0.00005 * 220.0 * 220.0 / 100.0;
    assert!(
        (br.g_mag - expected_g_pu).abs() < 1e-6,
        "2-winding g_mag must be set from End1.g: got {}, expected {}",
        br.g_mag,
        expected_g_pu
    );
}

/// PhaseTapChangerAsymmetrical must add windingConnectionAngle to the phase shift.
#[test]
fn test_cgmes_ptc_asymmetrical_winding_angle() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:PowerTransformer rdf:ID="XFMR_PST"/>
  <cim:PowerTransformerEnd rdf:ID="END_1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_PST"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_PST1"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:TransformerEnd.PhaseTapChanger rdf:resource="#PTC_ASYM"/>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END_2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_PST"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_PST2"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <!-- Asymmetrical PTC: windingConnectionAngle=30°, stepPhaseShiftIncrement=2°/step -->
  <!-- step=3, neutralStep=1 → shift = 30 + (3-1)*2 = 34° -->
  <cim:PhaseTapChangerAsymmetrical rdf:ID="PTC_ASYM">
<cim:TapChanger.neutralStep>1</cim:TapChanger.neutralStep>
<cim:TapChanger.step>3</cim:TapChanger.step>
<cim:PhaseTapChanger.stepPhaseShiftIncrement>2.0</cim:PhaseTapChanger.stepPhaseShiftIncrement>
<cim:PhaseTapChangerAsymmetrical.windingConnectionAngle>30.0</cim:PhaseTapChangerAsymmetrical.windingConnectionAngle>
<cim:PhaseTapChanger.TransformerEnd rdf:resource="#END_1"/>
  </cim:PhaseTapChangerAsymmetrical>
  <cim:Terminal rdf:ID="T_PST1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_PST"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_PST2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_PST"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD_1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_LOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // Expected shift: windingConnectionAngle=30 + (step=3 - neutral=1) * stepInc=2 = 34°
    let expected_shift = (30.0 + (3.0 - 1.0) * 2.0_f64).to_radians(); // = 34.0° in radians
    assert!(
        (br.phase_shift_rad - expected_shift).abs() < 1e-6,
        "PhaseTapChangerAsymmetrical shift must include windingConnectionAngle: got {} rad, expected {} rad",
        br.phase_shift_rad,
        expected_shift
    );
}

/// VoltageLimit objects with OperationalLimitType.direction=high/low must be
/// applied to Bus.voltage_max_pu and Bus.voltage_min_pu (in pu).
#[test]
fn test_cgmes_voltage_limits_applied_to_bus() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <!-- OperationalLimitType: direction=high (for vmax) -->
  <cim:OperationalLimitType rdf:ID="OLT_HIGH">
<cim:OperationalLimitType.direction
    rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#OperationalLimitDirectionKind.high"/>
  </cim:OperationalLimitType>
  <!-- OperationalLimitType: direction=low (for vmin) -->
  <cim:OperationalLimitType rdf:ID="OLT_LOW">
<cim:OperationalLimitType.direction
    rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#OperationalLimitDirectionKind.low"/>
  </cim:OperationalLimitType>
  <!-- OperationalLimitSet linked to TN_1 via Terminal T_SM (same bus) -->
  <cim:OperationalLimitSet rdf:ID="OLS_1">
<cim:OperationalLimitSet.Terminal rdf:resource="#T_SM"/>
  </cim:OperationalLimitSet>
  <!-- VoltageLimit: vmax = 242 kV (= 1.1 pu at 220 kV) -->
  <cim:VoltageLimit rdf:ID="VL_MAX">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS_1"/>
<cim:OperationalLimit.OperationalLimitType rdf:resource="#OLT_HIGH"/>
<cim:VoltageLimit.value>231.0</cim:VoltageLimit.value>
  </cim:VoltageLimit>
  <!-- VoltageLimit: vmin = 198 kV (= 0.9 pu at 220 kV) -->
  <cim:VoltageLimit rdf:ID="VL_MIN">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS_1"/>
<cim:OperationalLimit.OperationalLimitType rdf:resource="#OLT_LOW"/>
<cim:VoltageLimit.value>209.0</cim:VoltageLimit.value>
  </cim:VoltageLimit>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 1);
    let bus = &net.buses[0];
    // vmax = 231 kV / 220 kV = 1.05 pu
    let expected_vmax = 231.0 / 220.0;
    assert!(
        (bus.voltage_max_pu - expected_vmax).abs() < 1e-6,
        "VoltageLimit high must set bus.voltage_max_pu: got {}, expected {}",
        bus.voltage_max_pu,
        expected_vmax
    );
    // vmin = 209 kV / 220 kV ≈ 0.95 pu
    let expected_vmin = 209.0 / 220.0;
    assert!(
        (bus.voltage_min_pu - expected_vmin).abs() < 1e-6,
        "VoltageLimit low must set bus.voltage_min_pu: got {}, expected {}",
        bus.voltage_min_pu,
        expected_vmin
    );
}

/// ActivePowerLimit must be parsed and applied as rate_a (like ApparentPowerLimit).
#[test]
fn test_cgmes_active_power_limit_applies_rate_a() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:ACLineSegment rdf:ID="LINE_1">
<cim:Conductor.length>100.0</cim:Conductor.length>
<cim:ACLineSegment.r>5.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>20.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_L1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_L2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD_1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_LOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- OperationalLimitSet on LINE_1 terminal -->
  <cim:OperationalLimitSet rdf:ID="OLS_LINE">
<cim:OperationalLimitSet.Terminal rdf:resource="#T_L1"/>
  </cim:OperationalLimitSet>
  <!-- ActivePowerLimit: 350 MW → rate_a -->
  <cim:ActivePowerLimit rdf:ID="APL_1">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS_LINE"/>
<cim:ActivePowerLimit.value>350.0</cim:ActivePowerLimit.value>
  </cim:ActivePowerLimit>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    assert!(
        (br.rating_a_mva - 350.0).abs() < 1e-6,
        "ActivePowerLimit must set rate_a: got {}",
        br.rating_a_mva
    );
}

/// TATL-tagged ApparentPowerLimit must go to rate_c (emergency), not rate_a (normal).
#[test]
fn test_cgmes_tatl_limit_applies_rate_c() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:ACLineSegment rdf:ID="LINE_1">
<cim:ACLineSegment.r>5.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>20.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_L1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_L2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD_1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_LOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
  <!-- OperationalLimitType: limitType=tatl (emergency) -->
  <cim:OperationalLimitType rdf:ID="OLT_TATL">
<cim:OperationalLimitType.limitType
    rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#LimitTypeKind.tatl"/>
  </cim:OperationalLimitType>
  <!-- OperationalLimitType: limitType=patl (normal) -->
  <cim:OperationalLimitType rdf:ID="OLT_PATL">
<cim:OperationalLimitType.limitType
    rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#LimitTypeKind.patl"/>
  </cim:OperationalLimitType>
  <cim:OperationalLimitSet rdf:ID="OLS_LINE">
<cim:OperationalLimitSet.Terminal rdf:resource="#T_L1"/>
  </cim:OperationalLimitSet>
  <!-- PATL (normal): 400 MVA → rate_a -->
  <cim:ApparentPowerLimit rdf:ID="APL_PATL">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS_LINE"/>
<cim:OperationalLimit.OperationalLimitType rdf:resource="#OLT_PATL"/>
<cim:ApparentPowerLimit.value>400.0</cim:ApparentPowerLimit.value>
  </cim:ApparentPowerLimit>
  <!-- TATL (emergency): 500 MVA → rate_c -->
  <cim:ApparentPowerLimit rdf:ID="APL_TATL">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS_LINE"/>
<cim:OperationalLimit.OperationalLimitType rdf:resource="#OLT_TATL"/>
<cim:ApparentPowerLimit.value>500.0</cim:ApparentPowerLimit.value>
  </cim:ApparentPowerLimit>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    assert!(
        (br.rating_a_mva - 400.0).abs() < 1e-6,
        "PATL (normal) must set rate_a=400: got {}",
        br.rating_a_mva
    );
    assert!(
        (br.rating_c_mva - 500.0).abs() < 1e-6,
        "TATL (emergency) must set rate_c=500: got {}",
        br.rating_c_mva
    );
}

// Wave 11: EnergySource voltage control

#[test]
fn test_cgmes_energy_source_voltage_control_creates_generator() {
    // controlEnabled=true → EnergySource becomes a PV generator
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#"
     xmlns:entsoe="http://entsoe.eu/CIM/SchemaExtension/3/1#">
  <!-- Voltage level: 110 kV -->
  <cim:VoltageLevel rdf:ID="VL1">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV110"/>
  </cim:VoltageLevel>
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <!-- TopologicalNode -->
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>BusA</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- ConnectivityNode -->
  <cim:ConnectivityNode rdf:ID="CN1">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL1"/>
  </cim:ConnectivityNode>
  <!-- Terminal for CN1 -->
  <cim:Terminal rdf:ID="T_CN1_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
  </cim:Terminal>
  <!-- EnergySource with controlEnabled=true -->
  <cim:EnergySource rdf:ID="ES1">
<cim:EnergySource.controlEnabled>true</cim:EnergySource.controlEnabled>
<cim:EnergySource.activePower>-50.0</cim:EnergySource.activePower>
<cim:EnergySource.reactivePower>-10.0</cim:EnergySource.reactivePower>
<cim:EnergySource.maxQ>30.0</cim:EnergySource.maxQ>
<cim:EnergySource.minQ>-30.0</cim:EnergySource.minQ>
<cim:EnergySource.voltageRegulation>true</cim:EnergySource.voltageRegulation>
  </cim:EnergySource>
  <!-- Terminal connecting EnergySource to CN1 -->
  <cim:Terminal rdf:ID="T_ES1">
<cim:Terminal.ConductingEquipment rdf:resource="#ES1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <!-- SvVoltage for TN1 -->
  <cim:SvVoltage rdf:ID="SV1">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN1"/>
<cim:SvVoltage.v>110.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // controlEnabled=true → must produce a generator, not a load
    assert_eq!(
        net.generators.len(),
        1,
        "controlEnabled=true must create a PV generator; got {} generators",
        net.generators.len()
    );
    let generator = &net.generators[0];
    // activePower=-50 (generating convention) → pg=50 MW
    assert!(
        (generator.p - 50.0).abs() < 1.0,
        "pg must be ~50 MW (convention flip); got {}",
        generator.p
    );
    // No load should be added for this source
    let bus_pd = net.bus_load_p_mw();
    assert!(
        bus_pd[0].abs() < 1e-6,
        "pd must be 0 when EnergySource is a PV generator; got {}",
        bus_pd[0]
    );
}

#[test]
fn test_cgmes_energy_source_pq_when_control_disabled() {
    // controlEnabled=false → EnergySource is a fixed P/Q injection (load entry)
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#"
     xmlns:entsoe="http://entsoe.eu/CIM/SchemaExtension/3/1#">
  <cim:VoltageLevel rdf:ID="VL1">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV110"/>
  </cim:VoltageLevel>
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>BusB</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ConnectivityNode rdf:ID="CN1">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL1"/>
  </cim:ConnectivityNode>
  <cim:Terminal rdf:ID="T_CN1_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
  </cim:Terminal>
  <!-- EnergySource with controlEnabled=false → fixed injection -->
  <cim:EnergySource rdf:ID="ES2">
<cim:EnergySource.controlEnabled>false</cim:EnergySource.controlEnabled>
<cim:EnergySource.activePower>-30.0</cim:EnergySource.activePower>
<cim:EnergySource.reactivePower>-5.0</cim:EnergySource.reactivePower>
  </cim:EnergySource>
  <cim:Terminal rdf:ID="T_ES2">
<cim:Terminal.ConductingEquipment rdf:resource="#ES2"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:SvVoltage rdf:ID="SV1">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN1"/>
<cim:SvVoltage.v>110.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // controlEnabled=false → no generator
    assert_eq!(
        net.generators.len(),
        0,
        "controlEnabled=false must NOT create a generator; got {}",
        net.generators.len()
    );
    assert_eq!(
        net.power_injections.len(),
        1,
        "EnergySource must stay explicit"
    );
    assert_eq!(net.power_injections[0].id, "ES2");
    // activePower=-30 (CGMES generating convention) → net bus load = -30 (30 MW injection)
    let bus_pd = net.bus_load_p_mw();
    assert!(
        (bus_pd[0] - (-30.0)).abs() < 1.0,
        "pd must be -30 (30 MW injection as negative load); got {}",
        bus_pd[0]
    );
}

#[test]
fn test_cgmes_node_breaker_external_injection_retained_across_retopology() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:Substation rdf:ID="SUB1"/>
  <cim:VoltageLevel rdf:ID="VL1">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV220"/>
<cim:VoltageLevel.Substation rdf:resource="#SUB1"/>
  </cim:VoltageLevel>
  <cim:ConnectivityNode rdf:ID="CN_A">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL1"/>
  </cim:ConnectivityNode>
  <cim:ConnectivityNode rdf:ID="CN_B">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL1"/>
  </cim:ConnectivityNode>
  <cim:Breaker rdf:ID="BRK1">
<cim:Equipment.EquipmentContainer rdf:resource="#VL1"/>
<cim:Switch.open>false</cim:Switch.open>
  </cim:Breaker>
  <cim:Terminal rdf:ID="T_BRK1_1">
<cim:Terminal.ConductingEquipment rdf:resource="#BRK1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_A"/>
<cim:Terminal.sequenceNumber>1</cim:Terminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_BRK1_2">
<cim:Terminal.ConductingEquipment rdf:resource="#BRK1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_B"/>
<cim:Terminal.sequenceNumber>2</cim:Terminal.sequenceNumber>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM_SLACK">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_SLACK">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_SLACK"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_B"/>
<cim:Terminal.sequenceNumber>1</cim:Terminal.sequenceNumber>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:ExternalNetworkInjection rdf:ID="ENI_1">
<cim:ExternalNetworkInjection.p>40.0</cim:ExternalNetworkInjection.p>
<cim:ExternalNetworkInjection.q>5.0</cim:ExternalNetworkInjection.q>
<cim:RegulatingCondEq.controlEnabled>false</cim:RegulatingCondEq.controlEnabled>
  </cim:ExternalNetworkInjection>
  <cim:Terminal rdf:ID="T_ENI_1">
<cim:Terminal.ConductingEquipment rdf:resource="#ENI_1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_A"/>
<cim:Terminal.sequenceNumber>1</cim:Terminal.sequenceNumber>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
</rdf:RDF>"##;

    let mut net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 1, "closed breaker should merge both CNs");
    assert!((net.buses[0].base_kv - 220.0).abs() < 1e-6);
    assert_eq!(net.power_injections.len(), 1);
    assert_eq!(net.power_injections[0].id, "ENI_1");

    let sm = net.topology.as_mut().expect("NodeBreakerTopology missing");
    assert!(sm.set_switch_state("BRK1", true));

    let rebuilt = surge_topology::rebuild_topology(&net).expect("rebuild_topology failed");
    assert_eq!(rebuilt.buses.len(), 2, "opening BRK1 should split buses");

    let reduction = rebuilt
        .topology
        .as_ref()
        .and_then(surge_network::network::NodeBreakerTopology::current_mapping)
        .expect("topology reduction missing after rebuild");
    let injection_bus = reduction.connectivity_node_to_bus["CN_A"];
    let slack_bus = reduction.connectivity_node_to_bus["CN_B"];

    assert_eq!(rebuilt.power_injections[0].bus, injection_bus);
    let slack = rebuilt
        .generators
        .iter()
        .find(|g| g.machine_id.as_deref() == Some("SM_SLACK"))
        .expect("slack generator missing");
    assert_eq!(slack.bus, slack_bus);

    let host_bus = rebuilt
        .buses
        .iter()
        .find(|bus| bus.number == injection_bus)
        .expect("injection host bus missing");
    let bus_pd = rebuilt.bus_load_p_mw();
    let bus_qd = rebuilt.bus_load_q_mvar();
    let host_idx = rebuilt
        .buses
        .iter()
        .position(|b| b.number == host_bus.number)
        .unwrap();
    assert!((bus_pd[host_idx] + 40.0).abs() < 1e-6);
    assert!((bus_qd[host_idx] + 5.0).abs() < 1e-6);
}

// Wave 12: Tap changer step correctness + NonlinearShuntCompensator

#[test]
fn test_cgmes_rtc_neutralstep_fallback_when_no_ssh() {
    // When SvTapStep (SV) and TapChanger.step (SSH) are both absent, the parser
    // must fall back to neutralStep (EQ) — producing tap=1.0 (no ratio change).
    // neutralStep=5, stepVoltageIncrement=2% → at step=5 (neutral): tap=1.0
    // Previously the fallback was hardcoded 0.0, producing:
    //   tap = 1 + (0 - 5) * 0.02 = 0.90  ← WRONG
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV20">
<cim:BaseVoltage.nominalVoltage>20.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:VoltageLevel rdf:ID="VL110">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV110"/>
  </cim:VoltageLevel>
  <cim:VoltageLevel rdf:ID="VL20">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV20"/>
  </cim:VoltageLevel>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus110</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus20</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV20"/>
  </cim:TopologicalNode>
  <cim:ConnectivityNode rdf:ID="CN1">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL110"/>
  </cim:ConnectivityNode>
  <cim:ConnectivityNode rdf:ID="CN2">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL20"/>
  </cim:ConnectivityNode>
  <cim:Terminal rdf:ID="T1_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN2"/>
  </cim:Terminal>
  <!-- 2-winding transformer 110/20 kV -->
  <cim:PowerTransformer rdf:ID="XFMR1">
<cim:PowerTransformer.EquipmentContainer rdf:resource="#VL110"/>
  </cim:PowerTransformer>
  <!-- End 1: 110 kV, ratedU matches base → nominal tap -->
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:PowerTransformerEnd.endNumber>1</cim:PowerTransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1_tp"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.x>0.1</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <!-- End 2: 20 kV, ratedU matches base → nominal tap -->
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:PowerTransformerEnd.endNumber>2</cim:PowerTransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2_tp"/>
<cim:PowerTransformerEnd.ratedU>20.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <!-- RatioTapChanger: neutralStep=5, stepVoltageIncrement=2%
   No SSH step and no SvTapStep → parser must use neutralStep as default position.
   Expected: tap = 1 + (5 - 5) * 2/100 = 1.0 (no ratio change) -->
  <cim:RatioTapChanger rdf:ID="RTC1">
<cim:RatioTapChanger.TransformerEnd rdf:resource="#END1"/>
<cim:TapChanger.lowStep>0</cim:TapChanger.lowStep>
<cim:TapChanger.highStep>10</cim:TapChanger.highStep>
<cim:TapChanger.neutralStep>5</cim:TapChanger.neutralStep>
<cim:TapChanger.stepVoltageIncrement>2.0</cim:TapChanger.stepVoltageIncrement>
  </cim:RatioTapChanger>
  <!-- Terminals for transformer ends -->
  <cim:Terminal rdf:ID="T_END1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_END2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:SvVoltage rdf:ID="SV1">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN1"/>
<cim:SvVoltage.v>110.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
  <cim:SvVoltage rdf:ID="SV2">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN2"/>
<cim:SvVoltage.v>20.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1, "should have 1 transformer branch");
    let br = &net.branches[0];
    // neutralStep fallback → step=5, neutral=5 → tap ratio = 1.0 (no change)
    assert!(
        (br.tap - 1.0).abs() < 1e-6,
        "RTC with no SSH step must fall back to neutralStep → tap=1.0; got tap={}",
        br.tap
    );
}

#[test]
fn test_cgmes_nonlinear_shunt_compensator_tabular_b() {
    // NonlinearShuntCompensator with 3 tabular points.
    // SSH sections=2 → use point sectionNumber=2 → b=0.004 S at 100 kV → bs = 40 MVAr
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV100">
<cim:BaseVoltage.nominalVoltage>100.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:VoltageLevel rdf:ID="VL1">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV100"/>
  </cim:VoltageLevel>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>BusA</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV100"/>
  </cim:TopologicalNode>
  <cim:ConnectivityNode rdf:ID="CN1">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL1"/>
  </cim:ConnectivityNode>
  <cim:Terminal rdf:ID="T_CN1_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
  </cim:Terminal>
  <!-- NonlinearShuntCompensator: 3 sections, SSH sections=2 -->
  <cim:NonlinearShuntCompensator rdf:ID="NLSC1">
<cim:ShuntCompensator.sections>2</cim:ShuntCompensator.sections>
<cim:ShuntCompensator.normalSections>1</cim:ShuntCompensator.normalSections>
<cim:Equipment.EquipmentContainer rdf:resource="#VL1"/>
  </cim:NonlinearShuntCompensator>
  <cim:Terminal rdf:ID="T_NLSC1">
<cim:Terminal.ConductingEquipment rdf:resource="#NLSC1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <!-- Tabular B points: b is TOTAL at that section count (in Siemens) -->
  <cim:NonlinearShuntCompensatorPoint rdf:ID="NLSC1_P1">
<cim:NonlinearShuntCompensatorPoint.NonlinearShuntCompensator rdf:resource="#NLSC1"/>
<cim:NonlinearShuntCompensatorPoint.sectionNumber>1</cim:NonlinearShuntCompensatorPoint.sectionNumber>
<cim:NonlinearShuntCompensatorPoint.b>0.002</cim:NonlinearShuntCompensatorPoint.b>
<cim:NonlinearShuntCompensatorPoint.g>0.0</cim:NonlinearShuntCompensatorPoint.g>
  </cim:NonlinearShuntCompensatorPoint>
  <cim:NonlinearShuntCompensatorPoint rdf:ID="NLSC1_P2">
<cim:NonlinearShuntCompensatorPoint.NonlinearShuntCompensator rdf:resource="#NLSC1"/>
<cim:NonlinearShuntCompensatorPoint.sectionNumber>2</cim:NonlinearShuntCompensatorPoint.sectionNumber>
<cim:NonlinearShuntCompensatorPoint.b>0.004</cim:NonlinearShuntCompensatorPoint.b>
<cim:NonlinearShuntCompensatorPoint.g>0.0</cim:NonlinearShuntCompensatorPoint.g>
  </cim:NonlinearShuntCompensatorPoint>
  <cim:NonlinearShuntCompensatorPoint rdf:ID="NLSC1_P3">
<cim:NonlinearShuntCompensatorPoint.NonlinearShuntCompensator rdf:resource="#NLSC1"/>
<cim:NonlinearShuntCompensatorPoint.sectionNumber>3</cim:NonlinearShuntCompensatorPoint.sectionNumber>
<cim:NonlinearShuntCompensatorPoint.b>0.006</cim:NonlinearShuntCompensatorPoint.b>
<cim:NonlinearShuntCompensatorPoint.g>0.0</cim:NonlinearShuntCompensatorPoint.g>
  </cim:NonlinearShuntCompensatorPoint>
  <cim:SvVoltage rdf:ID="SV1">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN1"/>
<cim:SvVoltage.v>100.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 1);
    let bus = &net.buses[0];
    // sections=2 → b_total = 0.004 S, base_kv=100 kV
    // bs = 0.004 * 100^2 = 40 MVAr
    assert!(
        (bus.shunt_susceptance_mvar - 40.0).abs() < 1e-6,
        "NonlinearShuntCompensator sections=2 must give bs=40 MVAr; got {}",
        bus.shunt_susceptance_mvar
    );
    assert!(
        bus.shunt_conductance_mw.abs() < 1e-9,
        "gs must be 0; got {}",
        bus.shunt_conductance_mw
    );
}

// Wave 13: SvInjection fallback + OperationalLimitSet.Equipment

#[test]
fn test_cgmes_svinjection_fallback_for_load() {
    // When SSH p/q and EQ pfixed/qfixed are both absent, load p/q should come from
    // SvInjection (SV profile).  SvInjection.pInjection > 0 = consuming load.
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:VoltageLevel rdf:ID="VL1">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV110"/>
  </cim:VoltageLevel>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ConnectivityNode rdf:ID="CN1">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL1"/>
  </cim:ConnectivityNode>
  <cim:Terminal rdf:ID="T_CN1_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
  </cim:Terminal>
  <!-- EnergyConsumer with NO SSH p/q and NO pfixed/qfixed -->
  <cim:EnergyConsumer rdf:ID="EC1">
<cim:Equipment.EquipmentContainer rdf:resource="#VL1"/>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_EC1">
<cim:Terminal.ConductingEquipment rdf:resource="#EC1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <!-- SvInjection for the load's TopologicalNode (p=75, q=20 → consuming convention) -->
  <cim:SvInjection rdf:ID="SVI_TN1">
<cim:SvInjection.TopologicalNode rdf:resource="#TN1"/>
<cim:SvInjection.pInjection>75.0</cim:SvInjection.pInjection>
<cim:SvInjection.qInjection>20.0</cim:SvInjection.qInjection>
  </cim:SvInjection>
  <cim:SvVoltage rdf:ID="SV1">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN1"/>
<cim:SvVoltage.v>110.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 1);
    // SvInjection fallback: pd=75, qd=20
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    assert!(
        (bus_pd[0] - 75.0).abs() < 1e-6,
        "SvInjection fallback must set pd=75; got {}",
        bus_pd[0]
    );
    assert!(
        (bus_qd[0] - 20.0).abs() < 1e-6,
        "SvInjection fallback must set qd=20; got {}",
        bus_qd[0]
    );
}

#[test]
fn test_cgmes_ols_equipment_reference_applies_thermal_limit() {
    // OperationalLimitSet.Equipment (no Terminal) must still resolve equipment
    // and apply thermal limits (rate_a).
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:VoltageLevel rdf:ID="VL1">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV110"/>
  </cim:VoltageLevel>
  <cim:VoltageLevel rdf:ID="VL2">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV110"/>
  </cim:VoltageLevel>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ConnectivityNode rdf:ID="CN1">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL1"/>
  </cim:ConnectivityNode>
  <cim:ConnectivityNode rdf:ID="CN2">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL2"/>
  </cim:ConnectivityNode>
  <cim:Terminal rdf:ID="T1_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN2"/>
  </cim:Terminal>
  <cim:ACLineSegment rdf:ID="L1">
<cim:Equipment.EquipmentContainer rdf:resource="#VL1"/>
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_L1_1">
<cim:Terminal.ConductingEquipment rdf:resource="#L1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_L1_2">
<cim:Terminal.ConductingEquipment rdf:resource="#L1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <!-- OperationalLimitSet uses Equipment reference (Pattern 2, no Terminal) -->
  <cim:OperationalLimitSet rdf:ID="OLS_L1">
<cim:OperationalLimitSet.Equipment rdf:resource="#L1"/>
  </cim:OperationalLimitSet>
  <!-- OperationalLimitType: normal/PATL → rate_a -->
  <cim:OperationalLimitType rdf:ID="OLT_NORMAL">
<cim:OperationalLimitType.direction
    rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#OperationalLimitDirectionKind.absoluteValue"/>
  </cim:OperationalLimitType>
  <cim:ApparentPowerLimit rdf:ID="APL_L1">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS_L1"/>
<cim:OperationalLimit.OperationalLimitType rdf:resource="#OLT_NORMAL"/>
<cim:ApparentPowerLimit.value>350.0</cim:ApparentPowerLimit.value>
  </cim:ApparentPowerLimit>
  <cim:SvVoltage rdf:ID="SV1">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN1"/>
<cim:SvVoltage.v>110.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
  <cim:SvVoltage rdf:ID="SV2">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN2"/>
<cim:SvVoltage.v>110.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1, "should have 1 branch");
    let br = &net.branches[0];
    assert!(
        (br.rating_a_mva - 350.0).abs() < 1e-6,
        "OLS.Equipment pattern must apply rate_a=350; got rate_a={}",
        br.rating_a_mva
    );
}

// Wave 14: Deterministic referencePriority tiebreak

#[test]
fn test_cgmes_reference_priority_tiebreak_picks_highest_kv_bus() {
    // Two SynchronousMachines both have referencePriority=1. The one on the
    // higher-kV bus must be chosen as slack (deterministic, regardless of HashMap order).
    // SM_HV is on 220 kV, SM_LV is on 110 kV → SM_HV must win.
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:VoltageLevel rdf:ID="VL220">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV220"/>
  </cim:VoltageLevel>
  <cim:VoltageLevel rdf:ID="VL110">
<cim:VoltageLevel.BaseVoltage rdf:resource="#BV110"/>
  </cim:VoltageLevel>
  <cim:TopologicalNode rdf:ID="TN_HV">
<cim:IdentifiedObject.name>Bus220</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_LV">
<cim:IdentifiedObject.name>Bus110</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ConnectivityNode rdf:ID="CN_HV">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL220"/>
  </cim:ConnectivityNode>
  <cim:ConnectivityNode rdf:ID="CN_LV">
<cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource="#VL110"/>
  </cim:ConnectivityNode>
  <cim:Terminal rdf:ID="T_HV_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_HV"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_LV_tp">
<cim:Terminal.TopologicalNode rdf:resource="#TN_LV"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_LV"/>
  </cim:Terminal>
  <!-- SM_HV: 220 kV bus, referencePriority=1 -->
  <cim:SynchronousMachine rdf:ID="SM_HV">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>500.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-200.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-30.0</cim:RotatingMachine.q>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_HV">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_HV"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_HV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <!-- SM_LV: 110 kV bus, referencePriority=1 (same as SM_HV) -->
  <cim:SynchronousMachine rdf:ID="SM_LV">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>200.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-10.0</cim:RotatingMachine.q>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM_LV">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_LV"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_LV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_LV"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <!-- Connect the two buses with a line so island detection keeps both -->
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:Equipment.EquipmentContainer rdf:resource="#VL220"/>
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>10.0</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_LINE1_HV">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_HV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_LINE1_LV">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.ConnectivityNode rdf:resource="#CN_LV"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_LV"/>
<cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>
  </cim:Terminal>
  <cim:SvVoltage rdf:ID="SV_HV">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN_HV"/>
<cim:SvVoltage.v>220.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>0.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
  <cim:SvVoltage rdf:ID="SV_LV">
<cim:SvVoltage.TopologicalNode rdf:resource="#TN_LV"/>
<cim:SvVoltage.v>110.0</cim:SvVoltage.v>
<cim:SvVoltage.angle>-5.0</cim:SvVoltage.angle>
  </cim:SvVoltage>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.buses.len(), 2);
    // The 220 kV bus must be slack (highest-kV tiebreak when both have priority=1)
    let slack = net.buses.iter().find(|b| b.bus_type == BusType::Slack);
    assert!(slack.is_some(), "no slack bus found");
    let slack = slack.unwrap();
    assert!(
        (slack.base_kv - 220.0).abs() < 1.0,
        "tiebreak must select 220 kV bus as slack; got base_kv={}",
        slack.base_kv
    );
}

/// Wave 15: When a CGMES exporter splits series impedance across both
/// PowerTransformerEnd objects, the parser must refer End2 values to the
/// End1 side via (ratedU1/ratedU2)² before summing.
///
/// Setup: 220 kV / 110 kV transformer, turns_sq = (220/110)² = 4.
///   End1: r=2 Ω, x=10 Ω
///   End2: r=1 Ω, x=5 Ω  (referred: 4 Ω + 20 Ω)
///   combined: r=6 Ω, x=30 Ω
///   z_base = 220 kV (no RTC) → r_pu = 6/484, x_pu = 30/484
#[test]
fn test_cgmes_2winding_split_impedance_referral() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:PowerTransformer rdf:ID="XFMR_1"/>
  <!-- End1 and End2 each carry half the series impedance (split-impedance export) -->
  <cim:PowerTransformerEnd rdf:ID="END_1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_X1"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>2.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END_2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_X2"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>5.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="T_X1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_X2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD_1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_LOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(
        net.branches.len(),
        1,
        "expected exactly one transformer branch"
    );
    let br = &net.branches[0];

    // turns_sq = (220/110)^2 = 4.0
    // r_combined = 2.0 + 1.0*4.0 = 6.0 Ω  (End2 referred to End1 side)
    // x_combined = 10.0 + 5.0*4.0 = 30.0 Ω
    // z_base_kv = 220 * (110/110) = 220 kV  (no RTC → rated_u1 * to_base_kv/rated_u2)
    // r_pu = 6.0 * 100 / (220^2) = 6/484
    let z_base = 220.0_f64.powi(2) / 100.0; // = 484 Ω
    let expected_r_pu = 6.0 / z_base;
    let expected_x_pu = 30.0 / z_base;
    assert!(
        (br.r - expected_r_pu).abs() < 1e-9,
        "split-impedance referral: r_pu={:.6e} expected={:.6e}",
        br.r,
        expected_r_pu,
    );
    assert!(
        (br.x - expected_x_pu).abs() < 1e-9,
        "split-impedance referral: x_pu={:.6e} expected={:.6e}",
        br.x,
        expected_x_pu,
    );
}

/// Wave 16: When the combined series impedance (after End2 referral) is
/// negative — a known CGMES export artefact — the parser must use abs()
/// rather than clamping to 1e-9 Ω (which would create a near-short-circuit
/// and cause NR divergence).
///
/// Setup: 220/110 kV, turns_sq = 4.
///   End1: r=1 Ω, x=5 Ω
///   End2: r=−3 Ω, x=−8 Ω  (bad exporter; negative resistance/reactance)
///   combined: r=1+(−3)×4=−11 Ω → |−11|=11 Ω
///             x=5+(−8)×4=−27 Ω → |−27|=27 Ω
#[test]
fn test_cgmes_2winding_negative_impedance_artifact_uses_abs() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_1">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_2">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
  </cim:Terminal>
  <cim:PowerTransformer rdf:ID="XFMR_1"/>
  <!-- End2 carries negative r/x — a known CGMES export artefact -->
  <cim:PowerTransformerEnd rdf:ID="END_1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_X1"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>5.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END_2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_1"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_X2"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>-3.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>-8.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="T_X1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_X2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD_1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_LOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(
        net.branches.len(),
        1,
        "expected exactly one transformer branch"
    );
    let br = &net.branches[0];

    // End2 has non-zero impedance → not a star-decomposed 3W winding → abs().
    // turns_sq = 4; r_combined = 1 + (−3)×4 = −11; abs → 11 Ω
    //                           x_combined = 5 + (−8)×4 = −27; abs → 27 Ω
    // z_base_kv = 220 kV → z_base = 220^2/100 = 484 Ω
    let z_base = 220.0_f64.powi(2) / 100.0;
    let expected_r_pu = 11.0 / z_base;
    let expected_x_pu = 27.0 / z_base;
    assert!(
        (br.r - expected_r_pu).abs() < 1e-9,
        "negative impedance artifact abs(): r_pu={:.6e} expected={:.6e}",
        br.r,
        expected_r_pu,
    );
    assert!(
        (br.x - expected_x_pu).abs() < 1e-9,
        "negative impedance artifact abs(): x_pu={:.6e} expected={:.6e}",
        br.x,
        expected_x_pu,
    );
}

/// Star-decomposed 3-winding transformer winding: End1 carries negative impedance
/// from mesh→star conversion, End2 has zero impedance.  Sign must be preserved.
#[test]
fn test_cgmes_2winding_star_winding_negative_impedance_preserved() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:TopologicalNode rdf:ID="TN_HV">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_115"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_STAR">
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_1"/>
  </cim:TopologicalNode>
  <cim:BaseVoltage rdf:ID="BV_115">
<cim:BaseVoltage.nominalVoltage>115.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_1">
<cim:BaseVoltage.nominalVoltage>1.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:SynchronousMachine rdf:ID="SM_1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="T_SM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
  </cim:Terminal>
  <cim:PowerTransformer rdf:ID="XFMR_STAR"/>
  <!-- Star winding: End1 has negative r/x from mesh→star, End2 is zero -->
  <cim:PowerTransformerEnd rdf:ID="END_S1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_STAR"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_XS1"/>
<cim:PowerTransformerEnd.ratedU>115.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>-0.3398825</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>-11.71827575</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END_S2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR_STAR"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T_XS2"/>
<cim:PowerTransformerEnd.ratedU>1.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="T_XS1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_STAR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_HV"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_XS2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR_STAR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_STAR"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD_STAR">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="T_LOAD_STAR">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_STAR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_STAR"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(
        net.branches.len(),
        1,
        "expected exactly one transformer branch"
    );
    let br = &net.branches[0];

    // End2 r/x = 0 → star-decomposed winding → negative sign preserved.
    // r_combined = -0.3398825, x_combined = -11.71827575 (Ω at ratedU=115)
    // z_base_kv = 115 * (1.0/1.0) = 115 → z_base = 115²/100 = 132.25 Ω
    let z_base = 115.0_f64.powi(2) / 100.0;
    let expected_r_pu = -0.3398825 / z_base;
    let expected_x_pu = -11.71827575 / z_base;
    assert!(
        (br.r - expected_r_pu).abs() < 1e-9,
        "star winding sign preserved: r_pu={:.6e} expected={:.6e}",
        br.r,
        expected_r_pu,
    );
    assert!(
        (br.x - expected_x_pu).abs() < 1e-9,
        "star winding sign preserved: x_pu={:.6e} expected={:.6e}",
        br.x,
        expected_x_pu,
    );
}

// ── Wave 17 tests ───────────────────────────────────────────────────────────────

/// PerLengthSequenceImpedance fallback: when ACLineSegment.r/x are zero, use
/// PerLengthSequenceImpedance.r1/x1 × length_km for the impedance.
#[test]
fn test_cgmes_per_length_sequence_impedance_fallback() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>TN1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>TN2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- PerLengthSequenceImpedance: r1=0.2 Ω/km, x1=0.4 Ω/km, bch=2.5e-6 S/km -->
  <cim:PerLengthSequenceImpedance rdf:ID="PLSI1">
<cim:PerLengthSequenceImpedance.r>0.2</cim:PerLengthSequenceImpedance.r>
<cim:PerLengthSequenceImpedance.x>0.4</cim:PerLengthSequenceImpedance.x>
<cim:PerLengthSequenceImpedance.bch>2.5e-6</cim:PerLengthSequenceImpedance.bch>
  </cim:PerLengthSequenceImpedance>
  <!-- ACLineSegment: r=0 x=0 (no direct impedance), length=50 km, refs PLSI1 -->
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>0.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>0.0</cim:ACLineSegment.x>
<cim:Conductor.length>50.0</cim:Conductor.length>
<cim:ACLineSegment.PerLengthImpedance rdf:resource="#PLSI1"/>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>10.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // Expected: r_ohm = 0.2 × 50 = 10 Ω; x_ohm = 0.4 × 50 = 20 Ω; bch = 2.5e-6 × 50 S
    // z_base = 110^2 / 100 = 121 Ω
    let z_base = 110.0_f64.powi(2) / 100.0; // 121 Ω
    let expected_r = 10.0 / z_base;
    let expected_x = 20.0 / z_base;
    assert!(
        (br.r - expected_r).abs() < 1e-9,
        "PLSI fallback r_pu={:.6} expected={:.6}",
        br.r,
        expected_r
    );
    assert!(
        (br.x - expected_x).abs() < 1e-9,
        "PLSI fallback x_pu={:.6} expected={:.6}",
        br.x,
        expected_x
    );
    // b_pu should be non-zero (2.5e-6 × 50 S = 1.25e-4 S)
    let b_s_total = 2.5e-6 * 50.0;
    let expected_b = b_s_total / (1.0 / z_base);
    assert!(
        (br.b - expected_b).abs() < 1e-9,
        "PLSI fallback b_pu={:.6e} expected={:.6e}",
        br.b,
        expected_b
    );
}

/// ReactivePowerLimit: when SM.maxQ/minQ absent, ReactivePowerLimit via
/// OperationalLimitSet sets generator qmin/qmax.
#[test]
fn test_cgmes_reactive_power_limit_fallback_for_generator() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV20">
<cim:BaseVoltage.nominalVoltage>20.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>TN1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV20"/>
  </cim:TopologicalNode>
  <cim:GeneratingUnit rdf:ID="GU1">
<cim:GeneratingUnit.maxOperatingP>200.0</cim:GeneratingUnit.maxOperatingP>
<cim:GeneratingUnit.minOperatingP>0.0</cim:GeneratingUnit.minOperatingP>
<!-- no maxQ / minQ here -->
  </cim:GeneratingUnit>
  <!-- SM without SM.maxQ / SM.minQ — relies on ReactivePowerLimit -->
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.p>-100.0</cim:SynchronousMachine.p>
<cim:SynchronousMachine.q>-30.0</cim:SynchronousMachine.q>
<cim:SynchronousMachine.GeneratingUnit rdf:resource="#GU1"/>
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <!-- OperationalLimitType: qmax direction -->
  <cim:OperationalLimitType rdf:ID="OLT_HIGH">
<cim:OperationalLimitType.direction rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#OperationalLimitDirectionKind.high"/>
  </cim:OperationalLimitType>
  <!-- OperationalLimitType: qmin direction -->
  <cim:OperationalLimitType rdf:ID="OLT_LOW">
<cim:OperationalLimitType.direction rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#OperationalLimitDirectionKind.low"/>
  </cim:OperationalLimitType>
  <!-- OperationalLimitSet on SM1 -->
  <cim:OperationalLimitSet rdf:ID="OLS1">
<cim:OperationalLimitSet.Equipment rdf:resource="#SM1"/>
  </cim:OperationalLimitSet>
  <!-- ReactivePowerLimit: qmax = 120 MVAr -->
  <cim:ReactivePowerLimit rdf:ID="RPL_HIGH">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS1"/>
<cim:OperationalLimit.OperationalLimitType rdf:resource="#OLT_HIGH"/>
<cim:ReactivePowerLimit.value>120.0</cim:ReactivePowerLimit.value>
  </cim:ReactivePowerLimit>
  <!-- ReactivePowerLimit: qmin = -80 MVAr -->
  <cim:ReactivePowerLimit rdf:ID="RPL_LOW">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS1"/>
<cim:OperationalLimit.OperationalLimitType rdf:resource="#OLT_LOW"/>
<cim:ReactivePowerLimit.value>-80.0</cim:ReactivePowerLimit.value>
  </cim:ReactivePowerLimit>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.generators.len(), 1);
    let g = &net.generators[0];
    assert!(
        (g.qmax - 120.0).abs() < 1e-6,
        "ReactivePowerLimit qmax=120 not applied: got {}",
        g.qmax
    );
    assert!(
        (g.qmin - (-80.0)).abs() < 1e-6,
        "ReactivePowerLimit qmin=-80 not applied: got {}",
        g.qmin
    );
}

/// OilTemperatureLimit: temperature in °C stored on Branch.oil_temp_limit_c,
/// NOT applied as an MVA rating.
#[test]
fn test_cgmes_oil_temperature_limit_stored_not_as_mva() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>TN1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>TN2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
<cim:ACLineSegment.bch>0.0</cim:ACLineSegment.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:OperationalLimitSet rdf:ID="OLS1">
<cim:OperationalLimitSet.Equipment rdf:resource="#LINE1"/>
  </cim:OperationalLimitSet>
  <!-- OilTemperatureLimit: 75°C — NOT an MVA value -->
  <cim:OilTemperatureLimit rdf:ID="OTL1">
<cim:OperationalLimit.OperationalLimitSet rdf:resource="#OLS1"/>
<cim:OilTemperatureLimit.value>75.0</cim:OilTemperatureLimit.value>
  </cim:OilTemperatureLimit>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>10.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // Temperature stored on Branch
    assert_eq!(
        br.transformer_data
            .as_ref()
            .and_then(|t| t.oil_temp_limit_c),
        Some(75.0),
        "oil_temp_limit_c should be 75.0°C, got {:?}",
        br.transformer_data
            .as_ref()
            .and_then(|t| t.oil_temp_limit_c)
    );
    // Must NOT have polluted rate_a or rate_b with the temperature value
    assert!(
        br.rating_a_mva < 1.0,
        "rate_a must not be set from temperature (got {})",
        br.rating_a_mva
    );
    assert!(
        br.rating_b_mva < 1.0,
        "rate_b must not be set from temperature (got {})",
        br.rating_b_mva
    );
}

/// HvdcLine activePowerSetpoint fallback: when VsConverter.p is absent,
/// activePowerSetpoint from the linked HvdcLine is used via DC topology.
#[test]
fn test_cgmes_hvdc_line_active_power_setpoint_fallback() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>TN1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- VsConverter with no p or targetPpcc — must fall back to HvdcLine.activePowerSetpoint -->
  <cim:VsConverter rdf:ID="VSC1">
<!-- no ACDCConverter.p, no targetPpcc -->
  </cim:VsConverter>
  <cim:Terminal rdf:ID="T_VSC_AC">
<cim:Terminal.ConductingEquipment rdf:resource="#VSC1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- DC topology: VSC1 → DCNode1 → HvdcLine1 -->
  <cim:DCNode rdf:ID="DCN1"/>
  <cim:ACDCConverterDCTerminal rdf:ID="T_VSC_DC">
<cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC1"/>
<cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN1"/>
  </cim:ACDCConverterDCTerminal>
  <cim:HvdcLine rdf:ID="HVDC1">
<cim:HvdcLine.activePowerSetpoint>200.0</cim:HvdcLine.activePowerSetpoint>
<cim:HvdcLine.r>0.5</cim:HvdcLine.r>
<cim:HvdcLine.ratedUdc>320.0</cim:HvdcLine.ratedUdc>
  </cim:HvdcLine>
  <cim:DCTerminal rdf:ID="T_HVDC_DC">
<cim:DCTerminal.DCConductingEquipment rdf:resource="#HVDC1"/>
<cim:DCTerminal.DCNode rdf:resource="#DCN1"/>
  </cim:DCTerminal>
  <!-- Load to provide the second bus -->
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>250.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // The VsConverter has DC topology (DCNode + HvdcLine), so it is handled
    // by build_dc_network() and NOT subtracted from bus Pd via PQ injection.
    // Bus Pd = raw load = 250 MW.
    let bus_pd = net.bus_load_p_mw();
    let load_bus_idx = bus_pd.iter().position(|&p| p > 0.0).expect("bus with load");
    assert!(
        (bus_pd[load_bus_idx] - 250.0).abs() < 1e-6,
        "Bus Pd should be raw load (converter in DC network): pd={} expected 250.0",
        bus_pd[load_bus_idx]
    );
    // The HvdcLine.activePowerSetpoint is captured in the DcConverterStation.
    let dc_converters: Vec<_> = net
        .hvdc
        .dc_converters()
        .filter_map(|c| c.as_vsc())
        .collect();
    assert_eq!(dc_converters.len(), 1, "One converter in DC network");
    assert!(
        (dc_converters[0].power_dc_setpoint_mw - 200.0).abs() < 1e-6,
        "p_dc_set from HvdcLine.activePowerSetpoint: got {}",
        dc_converters[0].power_dc_setpoint_mw
    );
}

// ── DC topology tests ──────────────────────────────────────────────────────────

/// Point-to-point HVDC: 2 converters on 2 DCNodes connected by a DCLineSegment.
/// Verifies DcBus, DcConverterStation, and DcBranch counts and field values.
#[test]
fn test_cgmes_dc_topology_basic() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_A">
<cim:IdentifiedObject.name>Bus_A</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_B">
<cim:IdentifiedObject.name>Bus_B</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV220"/>
  </cim:TopologicalNode>
  <!-- Two VsConverters -->
  <cim:VsConverter rdf:ID="VSC_R">
<cim:ACDCConverter.p>300.0</cim:ACDCConverter.p>
<cim:ACDCConverter.q>50.0</cim:ACDCConverter.q>
<cim:ACDCConverter.ratedUdc>400.0</cim:ACDCConverter.ratedUdc>
<cim:ACDCConverter.ratedS>500.0</cim:ACDCConverter.ratedS>
  </cim:VsConverter>
  <cim:VsConverter rdf:ID="VSC_I">
<cim:ACDCConverter.p>-290.0</cim:ACDCConverter.p>
<cim:ACDCConverter.q>40.0</cim:ACDCConverter.q>
<cim:ACDCConverter.ratedUdc>400.0</cim:ACDCConverter.ratedUdc>
<cim:ACDCConverter.ratedS>500.0</cim:ACDCConverter.ratedS>
  </cim:VsConverter>
  <!-- AC line connecting the two buses -->
  <cim:ACLineSegment rdf:ID="ACLINE1">
<cim:Conductor.r>1.0</cim:Conductor.r>
<cim:Conductor.x>10.0</cim:Conductor.x>
<cim:Conductor.bch>0.001</cim:Conductor.bch>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_LINE_A">
<cim:Terminal.ConductingEquipment rdf:resource="#ACLINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_A"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_LINE_B">
<cim:Terminal.ConductingEquipment rdf:resource="#ACLINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_B"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- AC terminals for converters -->
  <cim:Terminal rdf:ID="T_VSC_R_AC">
<cim:Terminal.ConductingEquipment rdf:resource="#VSC_R"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_A"/>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T_VSC_I_AC">
<cim:Terminal.ConductingEquipment rdf:resource="#VSC_I"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_B"/>
  </cim:Terminal>
  <!-- DC topology -->
  <cim:DCNode rdf:ID="DCN_R"/>
  <cim:DCNode rdf:ID="DCN_I"/>
  <cim:ACDCConverterDCTerminal rdf:ID="T_VSC_R_DC">
<cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC_R"/>
<cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN_R"/>
  </cim:ACDCConverterDCTerminal>
  <cim:ACDCConverterDCTerminal rdf:ID="T_VSC_I_DC">
<cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC_I"/>
<cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN_I"/>
  </cim:ACDCConverterDCTerminal>
  <cim:DCLineSegment rdf:ID="DCLS1">
<cim:DCLineSegment.resistance>5.0</cim:DCLineSegment.resistance>
<cim:DCLineSegment.inductance>0.025</cim:DCLineSegment.inductance>
<cim:DCLineSegment.capacitance>0.000012</cim:DCLineSegment.capacitance>
  </cim:DCLineSegment>
  <cim:DCTerminal rdf:ID="T_DCLS_R">
<cim:DCTerminal.DCConductingEquipment rdf:resource="#DCLS1"/>
<cim:DCTerminal.DCNode rdf:resource="#DCN_R"/>
  </cim:DCTerminal>
  <cim:DCTerminal rdf:ID="T_DCLS_I">
<cim:DCTerminal.DCConductingEquipment rdf:resource="#DCLS1"/>
<cim:DCTerminal.DCNode rdf:resource="#DCN_I"/>
  </cim:DCTerminal>
  <!-- Dummy load so buses have nonzero demand -->
  <cim:EnergyConsumer rdf:ID="LOAD_A">
<cim:EnergyConsumer.p>100.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>20.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL_A">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_A"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_A"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD_B">
<cim:EnergyConsumer.p>200.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>30.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL_B">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD_B"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN_B"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let dc_buses: Vec<_> = net.hvdc.dc_buses().collect();
    let dc_converters: Vec<_> = net
        .hvdc
        .dc_converters()
        .filter_map(|c| c.as_vsc())
        .collect();
    let dc_branches: Vec<_> = net.hvdc.dc_branches().collect();
    // 2 DC buses, 2 converters, 1 DC branch.
    assert_eq!(dc_buses.len(), 2, "Expected 2 DC buses");
    assert_eq!(dc_converters.len(), 2, "Expected 2 DC converters");
    assert_eq!(dc_branches.len(), 1, "Expected 1 DC branch");
    assert_eq!(net.hvdc.dc_grids.len(), 1, "Expected 1 DC grid");
    // DC branch resistance from DCLineSegment.
    assert!((dc_branches[0].r_ohm - 5.0).abs() < 1e-6);
    assert!((dc_branches[0].l_mh - 25.0).abs() < 1e-6); // 0.025 H → 25 mH
    assert!((dc_branches[0].c_uf - 12.0).abs() < 1e-6); // 0.000012 F → 12 uF
    // base_kv_dc from ratedUdc.
    assert!((dc_buses[0].base_kv_dc - 400.0).abs() < 1e-6);
    // Converters skipped in PQ injection: bus Pd = raw load only.
    let bus_pd = net.bus_load_p_mw();
    let has_bus_a = bus_pd.iter().any(|&p| p > 90.0 && p < 110.0);
    assert!(has_bus_a, "Bus A should have pd=100 (raw load)");
    let has_bus_b = bus_pd.iter().any(|&p| p > 190.0 && p < 210.0);
    assert!(has_bus_b, "Bus B should have pd=200 (raw load)");
    // One converter should be type_dc=2 (Vdc slack, largest rating).
    let n_slack = dc_converters
        .iter()
        .filter(|c| c.control_type_dc == 2)
        .count();
    assert_eq!(n_slack, 1, "Exactly one Vdc slack per grid");
}

/// 3-terminal MTDC: 3 converters on 3 DCNodes, 2 DCLineSegments forming a chain.
/// Verifies grid grouping and type_dc assignment (largest = Vdc slack).
#[test]
fn test_cgmes_dc_topology_mtdc() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV345">
<cim:BaseVoltage.nominalVoltage>345.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV345"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV345"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN3">
<cim:IdentifiedObject.name>Bus3</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV345"/>
  </cim:TopologicalNode>
  <!-- 3 VsConverters: VSC1 (200 MVA), VSC2 (500 MVA), VSC3 (300 MVA) -->
  <cim:VsConverter rdf:ID="VSC1">
<cim:ACDCConverter.p>100.0</cim:ACDCConverter.p>
<cim:ACDCConverter.ratedUdc>500.0</cim:ACDCConverter.ratedUdc>
<cim:ACDCConverter.ratedS>200.0</cim:ACDCConverter.ratedS>
  </cim:VsConverter>
  <cim:VsConverter rdf:ID="VSC2">
<cim:ACDCConverter.p>-200.0</cim:ACDCConverter.p>
<cim:ACDCConverter.ratedUdc>500.0</cim:ACDCConverter.ratedUdc>
<cim:ACDCConverter.ratedS>500.0</cim:ACDCConverter.ratedS>
  </cim:VsConverter>
  <cim:VsConverter rdf:ID="VSC3">
<cim:ACDCConverter.p>100.0</cim:ACDCConverter.p>
<cim:ACDCConverter.ratedUdc>500.0</cim:ACDCConverter.ratedUdc>
<cim:ACDCConverter.ratedS>300.0</cim:ACDCConverter.ratedS>
  </cim:VsConverter>
  <!-- AC lines connecting buses -->
  <cim:ACLineSegment rdf:ID="ACL12"><cim:Conductor.r>1.0</cim:Conductor.r><cim:Conductor.x>10.0</cim:Conductor.x></cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_ACL12_1"><cim:Terminal.ConductingEquipment rdf:resource="#ACL12"/><cim:Terminal.TopologicalNode rdf:resource="#TN1"/><cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber></cim:Terminal>
  <cim:Terminal rdf:ID="T_ACL12_2"><cim:Terminal.ConductingEquipment rdf:resource="#ACL12"/><cim:Terminal.TopologicalNode rdf:resource="#TN2"/><cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber></cim:Terminal>
  <cim:ACLineSegment rdf:ID="ACL23"><cim:Conductor.r>1.0</cim:Conductor.r><cim:Conductor.x>10.0</cim:Conductor.x></cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_ACL23_1"><cim:Terminal.ConductingEquipment rdf:resource="#ACL23"/><cim:Terminal.TopologicalNode rdf:resource="#TN1"/><cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber></cim:Terminal>
  <cim:Terminal rdf:ID="T_ACL23_2"><cim:Terminal.ConductingEquipment rdf:resource="#ACL23"/><cim:Terminal.TopologicalNode rdf:resource="#TN3"/><cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber></cim:Terminal>
  <!-- AC terminals for converters -->
  <cim:Terminal rdf:ID="T1"><cim:Terminal.ConductingEquipment rdf:resource="#VSC1"/><cim:Terminal.TopologicalNode rdf:resource="#TN1"/></cim:Terminal>
  <cim:Terminal rdf:ID="T2"><cim:Terminal.ConductingEquipment rdf:resource="#VSC2"/><cim:Terminal.TopologicalNode rdf:resource="#TN2"/></cim:Terminal>
  <cim:Terminal rdf:ID="T3"><cim:Terminal.ConductingEquipment rdf:resource="#VSC3"/><cim:Terminal.TopologicalNode rdf:resource="#TN3"/></cim:Terminal>
  <!-- DC topology: 3 nodes, chain DCN1—DCN2—DCN3 -->
  <cim:DCNode rdf:ID="DCN1"/><cim:DCNode rdf:ID="DCN2"/><cim:DCNode rdf:ID="DCN3"/>
  <cim:ACDCConverterDCTerminal rdf:ID="DT1"><cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC1"/><cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN1"/></cim:ACDCConverterDCTerminal>
  <cim:ACDCConverterDCTerminal rdf:ID="DT2"><cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC2"/><cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN2"/></cim:ACDCConverterDCTerminal>
  <cim:ACDCConverterDCTerminal rdf:ID="DT3"><cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC3"/><cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN3"/></cim:ACDCConverterDCTerminal>
  <cim:DCLineSegment rdf:ID="DCLS12"><cim:DCLineSegment.resistance>3.0</cim:DCLineSegment.resistance></cim:DCLineSegment>
  <cim:DCTerminal rdf:ID="TDL12_1"><cim:DCTerminal.DCConductingEquipment rdf:resource="#DCLS12"/><cim:DCTerminal.DCNode rdf:resource="#DCN1"/></cim:DCTerminal>
  <cim:DCTerminal rdf:ID="TDL12_2"><cim:DCTerminal.DCConductingEquipment rdf:resource="#DCLS12"/><cim:DCTerminal.DCNode rdf:resource="#DCN2"/></cim:DCTerminal>
  <cim:DCLineSegment rdf:ID="DCLS23"><cim:DCLineSegment.resistance>4.0</cim:DCLineSegment.resistance></cim:DCLineSegment>
  <cim:DCTerminal rdf:ID="TDL23_1"><cim:DCTerminal.DCConductingEquipment rdf:resource="#DCLS23"/><cim:DCTerminal.DCNode rdf:resource="#DCN2"/></cim:DCTerminal>
  <cim:DCTerminal rdf:ID="TDL23_2"><cim:DCTerminal.DCConductingEquipment rdf:resource="#DCLS23"/><cim:DCTerminal.DCNode rdf:resource="#DCN3"/></cim:DCTerminal>
  <!-- Loads -->
  <cim:EnergyConsumer rdf:ID="L1"><cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p></cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL1"><cim:Terminal.ConductingEquipment rdf:resource="#L1"/><cim:Terminal.TopologicalNode rdf:resource="#TN1"/></cim:Terminal>
  <cim:EnergyConsumer rdf:ID="L2"><cim:EnergyConsumer.p>60.0</cim:EnergyConsumer.p></cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL2"><cim:Terminal.ConductingEquipment rdf:resource="#L2"/><cim:Terminal.TopologicalNode rdf:resource="#TN2"/></cim:Terminal>
  <cim:EnergyConsumer rdf:ID="L3"><cim:EnergyConsumer.p>70.0</cim:EnergyConsumer.p></cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL3"><cim:Terminal.ConductingEquipment rdf:resource="#L3"/><cim:Terminal.TopologicalNode rdf:resource="#TN3"/></cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let dc_buses: Vec<_> = net.hvdc.dc_buses().collect();
    let dc_converters: Vec<_> = net
        .hvdc
        .dc_converters()
        .filter_map(|c| c.as_vsc())
        .collect();
    let dc_branches: Vec<_> = net.hvdc.dc_branches().collect();
    assert_eq!(dc_buses.len(), 3, "3 DC buses");
    assert_eq!(dc_converters.len(), 3, "3 converters");
    assert_eq!(dc_branches.len(), 2, "2 DC branches (chain)");
    assert_eq!(net.hvdc.dc_grids.len(), 1, "All DC buses in one grid");
    // VSC2 (500 MVA) should be Vdc slack (type_dc=2).
    let vdc_slack_count = dc_converters
        .iter()
        .filter(|c| c.control_type_dc == 2)
        .count();
    assert_eq!(vdc_slack_count, 1, "One Vdc slack");
    // The Vdc slack should be the one with the largest rating (500 MVA).
    let slack = dc_converters
        .iter()
        .find(|c| c.control_type_dc == 2)
        .unwrap();
    assert!(
        (slack.active_power_mw - (-200.0)).abs() < 1e-6,
        "Vdc slack is VSC2 (p=-200)"
    );
    // base_kv_dc from ratedUdc.
    assert!((dc_buses[0].base_kv_dc - 500.0).abs() < 1e-6);
}

/// Verify loss parameter conversion from CGMES attributes (idleLoss, switchingLoss,
/// resistiveLoss) to Surge convention (loss_a, loss_b, loss_c).
#[test]
fn test_cgmes_dc_loss_conversion() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV400">
<cim:BaseVoltage.nominalVoltage>400.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV400"/>
  </cim:TopologicalNode>
  <cim:VsConverter rdf:ID="VSC1">
<cim:ACDCConverter.p>100.0</cim:ACDCConverter.p>
<cim:ACDCConverter.ratedUdc>320.0</cim:ACDCConverter.ratedUdc>
<cim:ACDCConverter.ratedS>600.0</cim:ACDCConverter.ratedS>
<cim:ACDCConverter.idleLoss>2.5</cim:ACDCConverter.idleLoss>
<cim:ACDCConverter.switchingLoss>0.5</cim:ACDCConverter.switchingLoss>
<cim:ACDCConverter.resistiveLoss>0.003</cim:ACDCConverter.resistiveLoss>
  </cim:VsConverter>
  <cim:Terminal rdf:ID="T1"><cim:Terminal.ConductingEquipment rdf:resource="#VSC1"/><cim:Terminal.TopologicalNode rdf:resource="#TN1"/></cim:Terminal>
  <cim:DCNode rdf:ID="DCN1"/>
  <cim:ACDCConverterDCTerminal rdf:ID="DT1"><cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC1"/><cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN1"/></cim:ACDCConverterDCTerminal>
  <cim:EnergyConsumer rdf:ID="L1"><cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p></cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL1"><cim:Terminal.ConductingEquipment rdf:resource="#L1"/><cim:Terminal.TopologicalNode rdf:resource="#TN1"/></cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let dc_converters: Vec<_> = net
        .hvdc
        .dc_converters()
        .filter_map(|c| c.as_vsc())
        .collect();
    assert_eq!(dc_converters.len(), 1);
    let c = dc_converters[0];
    // loss_a = idleLoss (MW) = 2.5
    assert!(
        (c.loss_constant_mw - 2.5).abs() < 1e-6,
        "loss_a={}",
        c.loss_constant_mw
    );
    // loss_b = switchingLoss * ratedS / ratedUdc = 0.5 * 600 / 320 = 0.9375
    let expected_b = 0.5 * 600.0 / 320.0;
    assert!(
        (c.loss_linear - expected_b).abs() < 1e-6,
        "loss_b={} expected {}",
        c.loss_linear,
        expected_b
    );
    // loss_c = resistiveLoss * ratedUdc^2 / ratedS = 0.003 * 320^2 / 600 = 0.512
    let expected_c = 0.003 * 320.0 * 320.0 / 600.0;
    assert!(
        (c.loss_quadratic_rectifier - expected_c).abs() < 1e-6,
        "loss_c_rec={} expected {}",
        c.loss_quadratic_rectifier,
        expected_c
    );
    assert!(
        (c.loss_quadratic_inverter - expected_c).abs() < 1e-6,
        "loss_c_inv={} expected {}",
        c.loss_quadratic_inverter,
        expected_c
    );
}

/// VsConverter with no DC topology (no DCNode/ACDCConverterDCTerminal) falls back
/// to PQ injection — DC network remains empty.
#[test]
fn test_cgmes_dc_no_dc_topology_fallback() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV220"/>
  </cim:TopologicalNode>
  <cim:VsConverter rdf:ID="VSC_NODC">
<cim:ACDCConverter.p>150.0</cim:ACDCConverter.p>
<cim:ACDCConverter.q>30.0</cim:ACDCConverter.q>
  </cim:VsConverter>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#VSC_NODC"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <!-- No DCNode, no ACDCConverterDCTerminal — old PQ injection path -->
  <cim:EnergyConsumer rdf:ID="L1">
<cim:EnergyConsumer.p>400.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>80.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL1">
<cim:Terminal.ConductingEquipment rdf:resource="#L1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // No DC topology → DC network empty.
    assert_eq!(
        net.hvdc.dc_bus_count(),
        0,
        "No DC buses without DC topology"
    );
    assert_eq!(
        net.hvdc.dc_converter_count(),
        0,
        "No DC converters without DC topology"
    );
    assert_eq!(
        net.hvdc.dc_branch_count(),
        0,
        "No DC branches without DC topology"
    );
    // Converter handled via PQ injection: bus pd = 400 - 150 = 250 MW.
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    assert!(
        (bus_pd[0] - 250.0).abs() < 1e-6,
        "PQ injection fallback: pd={} expected 250.0",
        bus_pd[0]
    );
    assert!(
        (bus_qd[0] - 50.0).abs() < 1e-6,
        "PQ injection fallback: qd={} expected 50.0",
        bus_qd[0]
    );
}

/// HvdcLine without DCLineSegment creates a synthetic DcBranch using HvdcLine.r.
#[test]
fn test_cgmes_dc_hvdcline_synthetic_branch() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV400">
<cim:BaseVoltage.nominalVoltage>400.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN_A">
<cim:IdentifiedObject.name>Bus_A</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV400"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN_B">
<cim:IdentifiedObject.name>Bus_B</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV400"/>
  </cim:TopologicalNode>
  <!-- Two converters -->
  <cim:VsConverter rdf:ID="VSC_R">
<cim:ACDCConverter.p>200.0</cim:ACDCConverter.p>
<cim:ACDCConverter.ratedUdc>500.0</cim:ACDCConverter.ratedUdc>
  </cim:VsConverter>
  <cim:VsConverter rdf:ID="VSC_I">
<cim:ACDCConverter.p>-195.0</cim:ACDCConverter.p>
<cim:ACDCConverter.ratedUdc>500.0</cim:ACDCConverter.ratedUdc>
  </cim:VsConverter>
  <cim:ACLineSegment rdf:ID="ACL_AB"><cim:Conductor.r>1.0</cim:Conductor.r><cim:Conductor.x>10.0</cim:Conductor.x></cim:ACLineSegment>
  <cim:Terminal rdf:ID="T_ACL_A"><cim:Terminal.ConductingEquipment rdf:resource="#ACL_AB"/><cim:Terminal.TopologicalNode rdf:resource="#TN_A"/><cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber></cim:Terminal>
  <cim:Terminal rdf:ID="T_ACL_B"><cim:Terminal.ConductingEquipment rdf:resource="#ACL_AB"/><cim:Terminal.TopologicalNode rdf:resource="#TN_B"/><cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber></cim:Terminal>
  <cim:Terminal rdf:ID="T_R"><cim:Terminal.ConductingEquipment rdf:resource="#VSC_R"/><cim:Terminal.TopologicalNode rdf:resource="#TN_A"/></cim:Terminal>
  <cim:Terminal rdf:ID="T_I"><cim:Terminal.ConductingEquipment rdf:resource="#VSC_I"/><cim:Terminal.TopologicalNode rdf:resource="#TN_B"/></cim:Terminal>
  <!-- DC topology: 2 nodes, NO DCLineSegment, only HvdcLine connecting them -->
  <cim:DCNode rdf:ID="DCN_R"/>
  <cim:DCNode rdf:ID="DCN_I"/>
  <cim:ACDCConverterDCTerminal rdf:ID="DT_R"><cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC_R"/><cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN_R"/></cim:ACDCConverterDCTerminal>
  <cim:ACDCConverterDCTerminal rdf:ID="DT_I"><cim:ACDCConverterDCTerminal.ACDCConverter rdf:resource="#VSC_I"/><cim:ACDCConverterDCTerminal.DCNode rdf:resource="#DCN_I"/></cim:ACDCConverterDCTerminal>
  <cim:HvdcLine rdf:ID="HVDC1">
<cim:HvdcLine.activePowerSetpoint>200.0</cim:HvdcLine.activePowerSetpoint>
<cim:HvdcLine.r>3.5</cim:HvdcLine.r>
<cim:HvdcLine.ratedUdc>500.0</cim:HvdcLine.ratedUdc>
  </cim:HvdcLine>
  <!-- HvdcLine DCTerminals connecting to both DC nodes -->
  <cim:DCTerminal rdf:ID="T_HVDC_R">
<cim:DCTerminal.DCConductingEquipment rdf:resource="#HVDC1"/>
<cim:DCTerminal.DCNode rdf:resource="#DCN_R"/>
  </cim:DCTerminal>
  <cim:DCTerminal rdf:ID="T_HVDC_I">
<cim:DCTerminal.DCConductingEquipment rdf:resource="#HVDC1"/>
<cim:DCTerminal.DCNode rdf:resource="#DCN_I"/>
  </cim:DCTerminal>
  <!-- Loads -->
  <cim:EnergyConsumer rdf:ID="L_A"><cim:EnergyConsumer.p>100.0</cim:EnergyConsumer.p></cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL_A"><cim:Terminal.ConductingEquipment rdf:resource="#L_A"/><cim:Terminal.TopologicalNode rdf:resource="#TN_A"/></cim:Terminal>
  <cim:EnergyConsumer rdf:ID="L_B"><cim:EnergyConsumer.p>100.0</cim:EnergyConsumer.p></cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL_B"><cim:Terminal.ConductingEquipment rdf:resource="#L_B"/><cim:Terminal.TopologicalNode rdf:resource="#TN_B"/></cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    let dc_buses: Vec<_> = net.hvdc.dc_buses().collect();
    let dc_converters: Vec<_> = net
        .hvdc
        .dc_converters()
        .filter_map(|c| c.as_vsc())
        .collect();
    let dc_branches: Vec<_> = net.hvdc.dc_branches().collect();
    assert_eq!(dc_buses.len(), 2);
    assert_eq!(dc_converters.len(), 2);
    // DcBranch created from HvdcLine.r (no DCLineSegment).
    assert_eq!(dc_branches.len(), 1, "Synthetic branch from HvdcLine");
    assert!(
        (dc_branches[0].r_ohm - 3.5).abs() < 1e-6,
        "r_ohm from HvdcLine.r"
    );
    // l_mh and c_uf should be 0 for synthetic HvdcLine branch.
    assert!((dc_branches[0].l_mh).abs() < 1e-6);
    assert!((dc_branches[0].c_uf).abs() < 1e-6);
    // base_kv_dc from HvdcLine.ratedUdc = 500 kV.
    assert!((dc_buses[0].base_kv_dc - 500.0).abs() < 1e-6);
}

// ── Wave 18 tests ───────────────────────────────────────────────────────────────

/// phaseAngleClock: Dyn11 transformer (end1 clock=0, end2 clock=11) adds −30° to shift.
/// IEC vector group Dyn11: secondary leads primary by 30° → shift = (11−0)×30 = 330° = −30°.
#[test]
fn test_cgmes_phase_angle_clock_dyn11() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV20">
<cim:BaseVoltage.nominalVoltage>20.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>TN1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>TN2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV20"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR1"/>
  <!-- End1: 110 kV, clock=0 (Delta) -->
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>5.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:PowerTransformerEnd.phaseAngleClock>0</cim:PowerTransformerEnd.phaseAngleClock>
  </cim:PowerTransformerEnd>
  <!-- End2: 20 kV, clock=11 (Yn) → Dyn11: (11-0)×30 = 330° -->
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2"/>
<cim:PowerTransformerEnd.ratedU>20.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:PowerTransformerEnd.phaseAngleClock>11</cim:PowerTransformerEnd.phaseAngleClock>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // (11 - 0) × 30° = 330° shift contribution from phaseAngleClock
    let expected_shift = 330.0_f64.to_radians();
    assert!(
        (br.phase_shift_rad - expected_shift).abs() < 1e-6,
        "Dyn11 phaseAngleClock: shift={} rad expected={} rad",
        br.phase_shift_rad,
        expected_shift
    );
}

/// phaseAngleClock: YNd1 transformer (end1 clock=0, end2 clock=1) adds +30° to shift.
#[test]
fn test_cgmes_phase_angle_clock_ynd1() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV20">
<cim:BaseVoltage.nominalVoltage>20.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>TN1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>TN2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV20"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR1"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>5.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:PowerTransformerEnd.phaseAngleClock>0</cim:PowerTransformerEnd.phaseAngleClock>
  </cim:PowerTransformerEnd>
  <!-- end2 clock=1 → (1-0)×30 = +30° -->
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2"/>
<cim:PowerTransformerEnd.ratedU>20.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
<cim:PowerTransformerEnd.phaseAngleClock>1</cim:PowerTransformerEnd.phaseAngleClock>
  </cim:PowerTransformerEnd>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    assert!(
        (br.phase_shift_rad - 30.0_f64.to_radians()).abs() < 1e-6,
        "YNd1 phaseAngleClock: shift={} rad expected={} rad",
        br.phase_shift_rad,
        30.0_f64.to_radians()
    );
}

// ── Wave 19 tests ───────────────────────────────────────────────────────────────

/// TapChangerControl.regulating=false: RTC tap locked at SSH step without warning.
/// When TCC.regulating=false the tap changer is manually fixed.  If TapChanger.step
/// (SSH) is present it takes priority; the TCC flag only suppresses spurious warnings
/// when no SSH/SV step is available.
///
/// Setup: RTC neutralStep=5, stepVoltageIncrement=2%, SSH step=3, TCC.regulating=false.
/// Expected: tap = 1 + (3 − 5) × 0.02 = 0.96  (SSH step still used when present).
#[test]
fn test_cgmes_tap_changer_control_regulating_false_uses_ssh_step() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV20">
<cim:BaseVoltage.nominalVoltage>20.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus110</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus20</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV20"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR1"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:PowerTransformerEnd.endNumber>1</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.x>0.1</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:PowerTransformerEnd.endNumber>2</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>20.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <!-- RTC manually locked at step=3 (SSH), neutral=5, increment=2%
   Expected tap = 1 + (3 - 5) * 0.02 = 0.96 -->
  <cim:RatioTapChanger rdf:ID="RTC1">
<cim:RatioTapChanger.TransformerEnd rdf:resource="#END1"/>
<cim:TapChanger.lowStep>0</cim:TapChanger.lowStep>
<cim:TapChanger.highStep>10</cim:TapChanger.highStep>
<cim:TapChanger.neutralStep>5</cim:TapChanger.neutralStep>
<cim:TapChanger.stepVoltageIncrement>2.0</cim:TapChanger.stepVoltageIncrement>
<cim:TapChanger.step>3.0</cim:TapChanger.step>
<cim:TapChanger.TapChangerControl rdf:resource="#TCC1"/>
  </cim:RatioTapChanger>
  <!-- TapChangerControl.regulating=false → tap manually fixed, no OLTC action -->
  <cim:TapChangerControl rdf:ID="TCC1">
<cim:RegulatingControl.regulating>false</cim:RegulatingControl.regulating>
<cim:TapChangerControl.deadband>0.5</cim:TapChangerControl.deadband>
  </cim:TapChangerControl>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1, "should have 1 transformer branch");
    let br = &net.branches[0];
    // SSH step=3, neutral=5, stepVoltageIncrement=2%:
    // tap = 1 + (3 - 5) * 0.02 = 0.96
    let expected_tap = 1.0 + (3.0 - 5.0) * 0.02;
    assert!(
        (br.tap - expected_tap).abs() < 1e-6,
        "TCC.regulating=false: SSH step must be used; tap={} expected={}",
        br.tap,
        expected_tap
    );
}

// ── Wave 20 tests ───────────────────────────────────────────────────────────────

/// TransformerCoreAdmittance overrides PowerTransformerEnd.b/g for magnetizing admittance.
/// PTE has b=0, g=0. TCA linked to end1 provides b=0.005 S, g=0.001 S.
/// Expected b_mag ≠ 0; end1 PTE values must be ignored when TCA is present.
#[test]
fn test_cgmes_transformer_core_admittance_overrides_end_bg() {
    let base_kv = 110.0_f64;
    let base_mva = 100.0_f64;
    // z_base = base_kv^2 / base_mva
    let z_base = base_kv * base_kv / base_mva;
    let tca_b_s = 0.005_f64; // Siemens
    let tca_g_s = 0.001_f64;
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV20">
<cim:BaseVoltage.nominalVoltage>20.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>HV</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>LV</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV20"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR1"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:PowerTransformerEnd.endNumber>1</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.x>0.1</cim:PowerTransformerEnd.x>
<!-- PTE b/g = 0 — TCA must override these -->
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<cim:PowerTransformerEnd.g>0.0</cim:PowerTransformerEnd.g>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR1"/>
<cim:PowerTransformerEnd.endNumber>2</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>20.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <!-- TransformerCoreAdmittance linked to END1: b=0.005 S, g=0.001 S -->
  <cim:TransformerCoreAdmittance rdf:ID="TCA1">
<cim:TransformerCoreAdmittance.TransformerEnd rdf:resource="#END1"/>
<cim:TransformerCoreAdmittance.b>0.005</cim:TransformerCoreAdmittance.b>
<cim:TransformerCoreAdmittance.g>0.001</cim:TransformerCoreAdmittance.g>
  </cim:TransformerCoreAdmittance>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>10.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1, "must have 1 transformer branch");
    let br = &net.branches[0];
    // TCA.b = 0.005 S @ z_base (110^2/100 = 121 Ω) → b_pu = 0.005 * 121 = 0.605 pu
    let expected_b_pu = tca_b_s * z_base;
    assert!(
        (br.b_mag - expected_b_pu).abs() < 1e-9,
        "TransformerCoreAdmittance: b_mag={} expected={}",
        br.b_mag,
        expected_b_pu
    );
    // TCA.g = 0.001 S → g_mag = 0.001 * 121 = 0.121 pu
    let expected_g_pu = tca_g_s * z_base;
    assert!(
        (br.g_mag - expected_g_pu).abs() < 1e-9,
        "TransformerCoreAdmittance: g_mag={} expected={}",
        br.g_mag,
        expected_g_pu
    );
}

// ── Wave 22 tests ───────────────────────────────────────────────────────────────

/// ControlArea objects are parsed into network.area_schedules (Wave 22).
#[test]
fn test_cgmes_control_area_parsed_to_area_schedules() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- Two ControlAreas with scheduled interchange and tolerance -->
  <cim:ControlArea rdf:ID="CA1">
<cim:IdentifiedObject.name>AreaNorth</cim:IdentifiedObject.name>
<cim:ControlArea.netInterchange>100.0</cim:ControlArea.netInterchange>
<cim:ControlArea.pTolerance>5.0</cim:ControlArea.pTolerance>
  </cim:ControlArea>
  <cim:ControlArea rdf:ID="CA2">
<cim:IdentifiedObject.name>AreaSouth</cim:IdentifiedObject.name>
<cim:ControlArea.netInterchange>-100.0</cim:ControlArea.netInterchange>
<cim:ControlArea.pTolerance>5.0</cim:ControlArea.pTolerance>
  </cim:ControlArea>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>0.01</cim:ACLineSegment.r>
<cim:ACLineSegment.x>0.1</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    assert_eq!(
        net.area_schedules.len(),
        2,
        "2 ControlAreas → 2 area_schedules"
    );
    // Find AreaNorth.
    let north = net
        .area_schedules
        .iter()
        .find(|a| a.name == "AreaNorth")
        .expect("AreaNorth not found");
    assert!(
        (north.p_desired_mw - 100.0).abs() < 1e-6,
        "AreaNorth netInterchange=100"
    );
    assert!(
        (north.p_tolerance_mw - 5.0).abs() < 1e-6,
        "AreaNorth pTolerance=5"
    );
    let south = net
        .area_schedules
        .iter()
        .find(|a| a.name == "AreaSouth")
        .expect("AreaSouth not found");
    assert!(
        (south.p_desired_mw - (-100.0)).abs() < 1e-6,
        "AreaSouth netInterchange=-100"
    );
}

// ── Wave 24 tests ───────────────────────────────────────────────────────────────

/// Cut with open=true: the parent ACLineSegment must be skipped (disconnected).
#[test]
fn test_cgmes_cut_open_disconnects_line() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>0.01</cim:ACLineSegment.r>
<cim:ACLineSegment.x>0.1</cim:ACLineSegment.x>
<cim:Conductor.length>50.0</cim:Conductor.length>
  </cim:ACLineSegment>
  <!-- Cut is open (SSH): LINE1 must be treated as disconnected -->
  <cim:Cut rdf:ID="CUT1">
<cim:Cut.ACLineSegment rdf:resource="#LINE1"/>
<cim:Cut.open>true</cim:Cut.open>
<cim:Cut.lengthFromTerminal1>25.0</cim:Cut.lengthFromTerminal1>
  </cim:Cut>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>10.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // Open Cut → LINE1 skipped → 0 branches
    assert_eq!(
        net.branches.len(),
        0,
        "open Cut must disconnect the parent ACLineSegment: got {} branches",
        net.branches.len()
    );
}

// ── Wave 27 tests ───────────────────────────────────────────────────────────────

/// LoadResponseCharacteristic ZIP coefficients are set on per-Load fields.
#[test]
fn test_cgmes_load_response_characteristic_stored() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- ZIP load model: 20% Z, 30% I, 50% P for both P and Q components -->
  <cim:LoadResponseCharacteristic rdf:ID="LRC1">
<cim:LoadResponseCharacteristic.pConstantImpedance>20.0</cim:LoadResponseCharacteristic.pConstantImpedance>
<cim:LoadResponseCharacteristic.pConstantCurrent>30.0</cim:LoadResponseCharacteristic.pConstantCurrent>
<cim:LoadResponseCharacteristic.pConstantPower>50.0</cim:LoadResponseCharacteristic.pConstantPower>
<cim:LoadResponseCharacteristic.qConstantImpedance>20.0</cim:LoadResponseCharacteristic.qConstantImpedance>
<cim:LoadResponseCharacteristic.qConstantCurrent>30.0</cim:LoadResponseCharacteristic.qConstantCurrent>
<cim:LoadResponseCharacteristic.qConstantPower>50.0</cim:LoadResponseCharacteristic.qConstantPower>
  </cim:LoadResponseCharacteristic>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>80.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>20.0</cim:EnergyConsumer.q>
<cim:EnergyConsumer.LoadResponse rdf:resource="#LRC1"/>
  </cim:EnergyConsumer>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>0.01</cim:ACLineSegment.r>
<cim:ACLineSegment.x>0.1</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // The load at TN2 should have its ZIP coefficients set (as fractions [0,1]).
    assert!(!net.loads.is_empty(), "loads should be populated");
    let load = &net.loads[0];
    assert!(
        (load.zip_p_impedance_frac - 0.2).abs() < 1e-6,
        "pConstantImpedance=20% → 0.2: got {}",
        load.zip_p_impedance_frac
    );
    assert!(
        (load.zip_p_current_frac - 0.3).abs() < 1e-6,
        "pConstantCurrent=30% → 0.3: got {}",
        load.zip_p_current_frac
    );
    assert!(
        (load.zip_p_power_frac - 0.5).abs() < 1e-6,
        "pConstantPower=50% → 0.5: got {}",
        load.zip_p_power_frac
    );
    assert!(
        (load.zip_q_impedance_frac - 0.2).abs() < 1e-6,
        "qConstantImpedance=20% → 0.2: got {}",
        load.zip_q_impedance_frac
    );
    assert!(
        (load.zip_q_current_frac - 0.3).abs() < 1e-6,
        "qConstantCurrent=30% → 0.3: got {}",
        load.zip_q_current_frac
    );
    assert!(
        (load.zip_q_power_frac - 0.5).abs() < 1e-6,
        "qConstantPower=50% → 0.5: got {}",
        load.zip_q_power_frac
    );
}

// ── Wave 28 tests ───────────────────────────────────────────────────────────────

/// SvStatus.inService=false disconnects equipment even when Terminal.connected is not false.
#[test]
fn test_cgmes_sv_status_out_of_service_disconnects_equipment() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>0.01</cim:ACLineSegment.r>
<cim:ACLineSegment.x>0.1</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <!-- SvStatus marks LINE1 as out of service in the SV profile -->
  <cim:SvStatus rdf:ID="SVS1">
<cim:SvStatus.ConductingEquipment rdf:resource="#LINE1"/>
<cim:SvStatus.inService>false</cim:SvStatus.inService>
  </cim:SvStatus>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
<!-- Note: connected not set to false — SvStatus must override -->
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>10.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // SvStatus.inService=false → LINE1 added to disconnected_eq → branch skipped
    assert_eq!(
        net.branches.len(),
        0,
        "SvStatus.inService=false must disconnect LINE1: got {} branches",
        net.branches.len()
    );
}

#[test]
fn test_cgmes_sv_status_malformed_is_flagged_invalid() {
    let mut objects: ObjMap = ObjMap::new();
    insert_obj(
        &mut objects,
        "EQ1",
        "ACLineSegment",
        &[("name", CimVal::Text("Line1".into()))],
    );
    insert_obj(
        &mut objects,
        "TN1",
        "TopologicalNode",
        &[("name", CimVal::Text("TN1".into()))],
    );
    insert_obj(
        &mut objects,
        "T1",
        "Terminal",
        &[
            ("ConductingEquipment", CimVal::Ref("EQ1".into())),
            ("TopologicalNode", CimVal::Ref("TN1".into())),
            ("sequenceNumber", CimVal::Text("1".into())),
        ],
    );
    insert_obj(
        &mut objects,
        "SV1",
        "SvStatus",
        &[
            ("ConductingEquipment", CimVal::Ref("EQ1".into())),
            ("inService", CimVal::Text("maybe".into())),
        ],
    );

    let idx = super::indices::CgmesIndices::build(&objects);
    assert!(
        idx.sv_status_invalid.contains("EQ1"),
        "malformed SvStatus.inService should be tracked separately"
    );
    assert!(
        !idx.disconnected_eq.contains("EQ1"),
        "malformed SvStatus.inService must not disconnect equipment"
    );
}

#[test]
fn test_cgmes_optional_numeric_fields_preserve_none() {
    let mut objects: ObjMap = ObjMap::new();
    insert_obj(
        &mut objects,
        "TN1",
        "TopologicalNode",
        &[("name", CimVal::Text("TN1".into()))],
    );
    insert_obj(
        &mut objects,
        "SV_V1",
        "SvVoltage",
        &[
            ("TopologicalNode", CimVal::Ref("TN1".into())),
            ("v", CimVal::Text("110.0".into())),
            ("angle", CimVal::Text("not-a-number".into())),
        ],
    );
    insert_obj(
        &mut objects,
        "EQ1",
        "SvInjection",
        &[
            ("TopologicalNode", CimVal::Ref("TN1".into())),
            ("pInjection", CimVal::Text("not-a-number".into())),
            ("qInjection", CimVal::Text("5.0".into())),
        ],
    );
    insert_obj(
        &mut objects,
        "HV1",
        "HvdcLine",
        &[
            ("activePowerSetpoint", CimVal::Text("not-a-number".into())),
            ("r", CimVal::Text("0.5".into())),
        ],
    );

    let idx = super::indices::CgmesIndices::build(&objects);

    assert_eq!(
        idx.sv_voltage.get("TN1").copied(),
        Some((Some(110.0), None)),
        "SvVoltage should preserve malformed angle as None"
    );
    assert_eq!(
        idx.sv_injections.get("TN1").copied(),
        Some((None, Some(5.0))),
        "SvInjection should preserve malformed pInjection as None"
    );
    assert_eq!(
        idx.hvdc_line_params.get("HV1").copied(),
        Some((None, Some(0.5), None)),
        "HvdcLine should preserve optional numeric fields instead of flattening to 0.0"
    );
}

#[test]
fn test_cgmes_malformed_switch_state_stays_open() {
    let mut objects: ObjMap = ObjMap::new();
    insert_obj(
        &mut objects,
        "CN1",
        "ConnectivityNode",
        &[("name", CimVal::Text("CN1".into()))],
    );
    insert_obj(
        &mut objects,
        "CN2",
        "ConnectivityNode",
        &[("name", CimVal::Text("CN2".into()))],
    );
    insert_obj(
        &mut objects,
        "BRK1",
        "Breaker",
        &[("open", CimVal::Text("maybe".into()))],
    );
    insert_obj(
        &mut objects,
        "T1",
        "Terminal",
        &[
            ("ConductingEquipment", CimVal::Ref("BRK1".into())),
            ("ConnectivityNode", CimVal::Ref("CN1".into())),
            ("sequenceNumber", CimVal::Text("1".into())),
        ],
    );
    insert_obj(
        &mut objects,
        "T2",
        "Terminal",
        &[
            ("ConductingEquipment", CimVal::Ref("BRK1".into())),
            ("ConnectivityNode", CimVal::Ref("CN2".into())),
            ("sequenceNumber", CimVal::Text("2".into())),
        ],
    );

    super::topology::reduce_topology(&mut objects);

    let tn_count = objects
        .values()
        .filter(|obj| obj.class == "TopologicalNode")
        .count();
    assert_eq!(
        tn_count, 2,
        "malformed switch state must leave the breaker open"
    );
}

// ── Wave 29 tests ───────────────────────────────────────────────────────────────

/// GL profile Location + PositionPoint → geo_locations stored on Network.
#[test]
fn test_cgmes_geo_location_stored() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>0.01</cim:ACLineSegment.r>
<cim:ACLineSegment.x>0.1</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <!-- GL profile: Location linked to LINE1, two PositionPoints -->
  <cim:Location rdf:ID="LOC1">
<cim:Location.PowerSystemResource rdf:resource="#LINE1"/>
  </cim:Location>
  <cim:PositionPoint rdf:ID="PP1">
<cim:PositionPoint.Location rdf:resource="#LOC1"/>
<cim:PositionPoint.sequenceNumber>1</cim:PositionPoint.sequenceNumber>
<cim:PositionPoint.xPosition>-97.5</cim:PositionPoint.xPosition>
<cim:PositionPoint.yPosition>30.2</cim:PositionPoint.yPosition>
  </cim:PositionPoint>
  <cim:PositionPoint rdf:ID="PP2">
<cim:PositionPoint.Location rdf:resource="#LOC1"/>
<cim:PositionPoint.sequenceNumber>2</cim:PositionPoint.sequenceNumber>
<cim:PositionPoint.xPosition>-97.3</cim:PositionPoint.xPosition>
<cim:PositionPoint.yPosition>30.4</cim:PositionPoint.yPosition>
  </cim:PositionPoint>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>10.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // LINE1 should have 2 position points in geo_locations.
    let coords = net
        .cim
        .geo_locations
        .get("LINE1")
        .expect("LINE1 geo_location not found");
    assert_eq!(coords.len(), 2, "LINE1 should have 2 position points");
    // First point: (-97.5, 30.2), second: (-97.3, 30.4)
    assert!(
        (coords[0].x - (-97.5)).abs() < 1e-9,
        "PP1 x={}",
        coords[0].x
    );
    assert!((coords[0].y - 30.2).abs() < 1e-9, "PP1 y={}", coords[0].y);
    assert!(
        (coords[1].x - (-97.3)).abs() < 1e-9,
        "PP2 x={}",
        coords[1].x
    );
    assert!((coords[1].y - 30.4).abs() < 1e-9, "PP2 y={}", coords[1].y);
}

// ── Wave 21 tests ───────────────────────────────────────────────────────────────

/// TransformerMeshImpedance mesh→star conversion for a 3-winding transformer.
/// Three TMI objects (one per winding pair) provide mesh impedances. The builder
/// converts them to star values and uses those for each winding branch.
///
/// Mesh values (Ω, each at FromTransformerEnd base):
///   r12=10 x12=20  (from=END1, u1=110kV)
///   r13=6  x13=12  (from=END1, u1=110kV)
///   r23=4  x23=8   (from=END2, u2=33kV)
///
/// Refer r23 to u1 base:  r23_ref1 = 4×(33/110)² = 0.36  x23_ref1 = 8×0.09 = 0.72
/// Mesh→star at u1 base:
///   r1_ref1=(10+6-0.36)/2=7.82  r2_ref1=(10+0.36-6)/2=2.18  r3_ref1=(6+0.36-10)/2=-1.82
/// Refer back to own winding bases:
///   r1=7.82Ω (u1=110)  r2=2.18×(110/33)²=24.222Ω (u2=33)  r3=-1.82×(110/11)²=-182Ω (u3=11)
#[test]
fn test_cgmes_transformer_mesh_impedance_star_conversion() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV33">
<cim:BaseVoltage.nominalVoltage>33.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV11">
<cim:BaseVoltage.nominalVoltage>11.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>HV</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>MV</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV33"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN3">
<cim:IdentifiedObject.name>LV</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV11"/>
  </cim:TopologicalNode>
  <!-- Slack generator on TN1 -->
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-100.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <!-- 3-winding transformer -->
  <cim:PowerTransformer rdf:ID="XFMR3W"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR3W"/>
<cim:PowerTransformerEnd.endNumber>1</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<!-- per-winding r/x set to 0 — TMI values must override -->
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.01</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR3W"/>
<cim:PowerTransformerEnd.endNumber>2</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>33.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.01</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END3">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR3W"/>
<cim:PowerTransformerEnd.endNumber>3</cim:PowerTransformerEnd.endNumber>
<cim:PowerTransformerEnd.ratedU>11.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.01</cim:PowerTransformerEnd.x>
  </cim:PowerTransformerEnd>
  <!-- Terminals linking transformer to buses -->
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR3W"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR3W"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T3">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR3W"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN3"/>
<cim:ACDCTerminal.sequenceNumber>3</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- TransformerMeshImpedance: 3 pairs. r/x in Ω. -->
  <!-- Pair END1↔END2: r12=10 Ω, x12=20 Ω -->
  <cim:TransformerMeshImpedance rdf:ID="TMI12">
<cim:TransformerMeshImpedance.FromTransformerEnd rdf:resource="#END1"/>
<cim:TransformerMeshImpedance.ToTransformerEnd rdf:resource="#END2"/>
<cim:TransformerMeshImpedance.r>10.0</cim:TransformerMeshImpedance.r>
<cim:TransformerMeshImpedance.x>20.0</cim:TransformerMeshImpedance.x>
  </cim:TransformerMeshImpedance>
  <!-- Pair END1↔END3: r13=6 Ω, x13=12 Ω -->
  <cim:TransformerMeshImpedance rdf:ID="TMI13">
<cim:TransformerMeshImpedance.FromTransformerEnd rdf:resource="#END1"/>
<cim:TransformerMeshImpedance.ToTransformerEnd rdf:resource="#END3"/>
<cim:TransformerMeshImpedance.r>6.0</cim:TransformerMeshImpedance.r>
<cim:TransformerMeshImpedance.x>12.0</cim:TransformerMeshImpedance.x>
  </cim:TransformerMeshImpedance>
  <!-- Pair END2↔END3: r23=4 Ω, x23=8 Ω -->
  <cim:TransformerMeshImpedance rdf:ID="TMI23">
<cim:TransformerMeshImpedance.FromTransformerEnd rdf:resource="#END2"/>
<cim:TransformerMeshImpedance.ToTransformerEnd rdf:resource="#END3"/>
<cim:TransformerMeshImpedance.r>4.0</cim:TransformerMeshImpedance.r>
<cim:TransformerMeshImpedance.x>8.0</cim:TransformerMeshImpedance.x>
  </cim:TransformerMeshImpedance>
  <!-- Loads on TN2 and TN3 -->
  <cim:EnergyConsumer rdf:ID="LOAD2">
<cim:EnergyConsumer.p>30.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>5.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL2">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD2"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD3">
<cim:EnergyConsumer.p>20.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>3.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TL3">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD3"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN3"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // 3W transformer expands to 3 branches (each winding → star bus) + 1 star bus.
    // The star bus is internal, so we expect 4 buses (TN1/TN2/TN3 + star).
    assert_eq!(
        net.branches.len(),
        3,
        "3W transformer must expand to 3 branches"
    );

    // Voltage bases: u1=110, u2=33, u3=11 kV
    // r23_ref1 = 4 × (33/110)² = 0.36 Ω
    // Mesh→star at u1 base: r1_ref1=7.82 r2_ref1=2.18 r3_ref1=-1.82
    // Refer to own bases:   r1=7.82Ω(u1) r2=2.18×(110/33)²=24.222Ω(u2) r3=-1.82×(110/11)²=-182Ω(u3)
    //
    // Branch 0: winding-1 (bus1→star), rated_u1=110 kV
    //   r1_pu = 7.82 / (110²/100) = 7.82 / 121 ≈ 0.064628
    //   x1_pu = 15.64 / 121 ≈ 0.129256
    let u1 = 110.0_f64;
    let u2 = 33.0_f64;
    let r23_ref1 = 4.0 * (u2 / u1).powi(2);
    let x23_ref1 = 8.0 * (u2 / u1).powi(2);
    let r1_ref1 = (10.0 + 6.0 - r23_ref1) / 2.0;
    let x1_ref1 = (20.0 + 12.0 - x23_ref1) / 2.0;
    let r2_ref1 = (10.0 + r23_ref1 - 6.0) / 2.0;
    let x2_ref1 = (20.0 + x23_ref1 - 12.0) / 2.0;
    let z_base1 = u1 * u1 / 100.0;
    let z_base2 = u2 * u2 / 100.0;
    let expected_r1_pu = r1_ref1 / z_base1;
    let expected_x1_pu = x1_ref1 / z_base1;
    // r2 at u2 base: r2_ref1 × (u1/u2)²; then divide by z_base2
    // = r2_ref1 × (u1/u2)² / (u2²/100) = r2_ref1 × u1² / u2^4 × 100
    // Equivalently: r2_ref1 / z_base1 (ohm_to_pu at u1, then re-base is not needed since
    // ohm_to_pu(rm2, u2) with rm2 already referred to u2 gives the right pu)
    let expected_r2_pu = r2_ref1 * (u1 / u2).powi(2) / z_base2;
    let expected_x2_pu = x2_ref1 * (u1 / u2).powi(2) / z_base2;
    let br1 = &net.branches[0];
    assert!(
        (br1.r - expected_r1_pu).abs() < 1e-6,
        "winding-1 r_pu={:.6} expected={:.6}",
        br1.r,
        expected_r1_pu
    );
    assert!(
        (br1.x - expected_x1_pu).abs() < 1e-6,
        "winding-1 x_pu={:.6} expected={:.6}",
        br1.x,
        expected_x1_pu
    );

    // Branch 1: winding-2 (bus2→star), rated_u2=33 kV
    // r2 = 2.18 × (110/33)² ≈ 24.222 Ω  at u2 base
    // r2_pu = 24.222 / (33²/100) ≈ 2.224
    let br2 = &net.branches[1];
    assert!(
        (br2.r - expected_r2_pu).abs() < 1e-4,
        "winding-2 r_pu={:.6} expected={:.6}",
        br2.r,
        expected_r2_pu
    );
    assert!(
        (br2.x - expected_x2_pu).abs() < 1e-4,
        "winding-2 x_pu={:.6} expected={:.6}",
        br2.x,
        expected_x2_pu
    );
}

// ── Wave 23 tests ───────────────────────────────────────────────────────────────

/// FrequencyConverter is modelled as a load on bus1 and a generator on bus2.
/// The device's p/q values are read from the FrequencyConverter object.
#[test]
fn test_cgmes_frequency_converter_load_gen_pair() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV50">
<cim:BaseVoltage.nominalVoltage>50.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV60">
<cim:BaseVoltage.nominalVoltage>60.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TNA">
<cim:IdentifiedObject.name>BusA</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV50"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TNB">
<cim:IdentifiedObject.name>BusB</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV60"/>
  </cim:TopologicalNode>
  <!-- Slack generator on TNA -->
  <cim:SynchronousMachine rdf:ID="SMA">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-80.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSMA">
<cim:Terminal.ConductingEquipment rdf:resource="#SMA"/>
<cim:Terminal.TopologicalNode rdf:resource="#TNA"/>
  </cim:Terminal>
  <!-- ACLineSegment connecting TNA and TNB so both stay in the main island -->
  <cim:ACLineSegment rdf:ID="LINE_FC">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TLF1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_FC"/>
<cim:Terminal.TopologicalNode rdf:resource="#TNA"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TLF2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE_FC"/>
<cim:Terminal.TopologicalNode rdf:resource="#TNB"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <!-- Load on TNB to keep the bus meaningful -->
  <cim:EnergyConsumer rdf:ID="LOADB">
<cim:EnergyConsumer.p>30.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>5.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLB">
<cim:Terminal.ConductingEquipment rdf:resource="#LOADB"/>
<cim:Terminal.TopologicalNode rdf:resource="#TNB"/>
  </cim:Terminal>
  <!-- FrequencyConverter: 50 MW from TNA (bus1) to TNB (bus2) -->
  <cim:FrequencyConverter rdf:ID="FC1">
<cim:FrequencyConverter.p>50.0</cim:FrequencyConverter.p>
<cim:FrequencyConverter.q>10.0</cim:FrequencyConverter.q>
  </cim:FrequencyConverter>
  <cim:Terminal rdf:ID="TFC1">
<cim:Terminal.ConductingEquipment rdf:resource="#FC1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TNA"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TFC2">
<cim:Terminal.ConductingEquipment rdf:resource="#FC1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TNB"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // FrequencyConverter with p=50 MW: load on bus1, generator on bus2.
    // Bus1 (TNA) should have an extra load of pd=50 MW.
    // Bus2 (TNB) should have a generator with pg=50 MW.
    let load = net.loads.iter().find(|l| l.active_power_demand_mw == 50.0);
    assert!(
        load.is_some(),
        "FC load (pd=50) not found: loads={:?}",
        net.loads
    );
    let load = load.unwrap();
    assert!(
        (load.reactive_power_demand_mvar - 10.0).abs() < 1e-9,
        "FC load qd={} expected=10",
        load.reactive_power_demand_mvar
    );

    // Generator on bus2 injects pg=50 MW, qg=-10 MVAr (mirror).
    let fc_gen = net.generators.iter().find(|g| (g.p - 50.0).abs() < 1e-6);
    assert!(fc_gen.is_some(), "FC generator (pg=50) not found");
    let fc_gen = fc_gen.unwrap();
    assert!(
        (fc_gen.q - (-10.0)).abs() < 1e-9,
        "FC gen qg={} expected=-10",
        fc_gen.q
    );
}

// ── Wave 26 tests ───────────────────────────────────────────────────────────────

/// GroundingImpedance and Ground are stored in network.cim.grounding_impedances.
/// Ground: x=0, GroundingImpedance: x from attribute.
#[test]
fn test_cgmes_grounding_impedances_stored() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV110"/>
  </cim:TopologicalNode>
  <!-- Minimal topology: generator on TN1, load on TN2, line between them -->
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:SynchronousMachine.maxQ>9999.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-9999.0</cim:SynchronousMachine.minQ>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:ACLineSegment rdf:ID="LINE1">
<cim:ACLineSegment.r>1.0</cim:ACLineSegment.r>
<cim:ACLineSegment.x>5.0</cim:ACLineSegment.x>
  </cim:ACLineSegment>
  <cim:Terminal rdf:ID="TL1">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="TL2">
<cim:Terminal.ConductingEquipment rdf:resource="#LINE1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>40.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>5.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
  <!-- Ground: solid earth on TN1 (x=0) -->
  <cim:Ground rdf:ID="GND1"/>
  <cim:Terminal rdf:ID="TGND">
<cim:Terminal.ConductingEquipment rdf:resource="#GND1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <!-- GroundingImpedance: 50 Ω neutral reactor on TN2 -->
  <cim:GroundingImpedance rdf:ID="GI1">
<cim:GroundingImpedance.x>50.0</cim:GroundingImpedance.x>
  </cim:GroundingImpedance>
  <cim:Terminal rdf:ID="TGI">
<cim:Terminal.ConductingEquipment rdf:resource="#GI1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;
    let net = parse_str(xml).expect("parse failed");
    // Two grounding entries: Ground (x=0) and GroundingImpedance (x=50).
    assert_eq!(
        net.cim.grounding_impedances.len(),
        2,
        "expected 2 grounding entries, got {:?}",
        net.cim.grounding_impedances
    );
    // Ground entry: x = 0, no min/max.
    let ground_entry = net
        .cim
        .grounding_impedances
        .iter()
        .find(|gi| gi.x_ohm.abs() < 1e-9);
    assert!(
        ground_entry.is_some(),
        "Ground solid-earth entry (x=0) not found"
    );
    let ground_entry = ground_entry.unwrap();
    assert!(ground_entry.x_min_ohm.is_none());
    assert!(ground_entry.x_max_ohm.is_none());
    // GroundingImpedance entry: x = 50, no min/max.
    let gi_entry = net
        .cim
        .grounding_impedances
        .iter()
        .find(|gi| (gi.x_ohm - 50.0).abs() < 1e-9);
    assert!(
        gi_entry.is_some(),
        "GroundingImpedance (x=50) entry not found"
    );
    let gi_entry = gi_entry.unwrap();
    assert!(gi_entry.x_min_ohm.is_none());
    assert!(gi_entry.x_max_ohm.is_none());
}

// -----------------------------------------------------------------------
// MAJ-01: RTC z_base must use ratedU1, not base_kv
// -----------------------------------------------------------------------

/// MAJ-01 regression: transformer with RTC must convert impedance using ratedU1 as z-base,
/// not the system BaseVoltage (base_kv).  The r/x (Ω) values per IEC 61970-301 are referred
/// to the winding's own ratedU, so:
///   r_pu = r_ohm * base_mva / ratedU1²
///
/// In this test: ratedU1=220 kV, r_ohm=48.4 Ω, base_mva=100 → r_pu = 48.4*100/220² = 0.1 pu.
/// If the bug were present (using base_kv=220 → same value here since they match), so we use
/// ratedU1=110 kV with base_kv=220 kV to make the two paths distinct:
///   correct (ratedU1=110):  r_pu = 2.0*100/110² = 0.01653 pu
///   wrong   (base_kv=220):  r_pu = 2.0*100/220² = 0.004132 pu  (4× too small)
#[test]
fn test_cgmes_rtc_impedance_base_uses_rated_u1() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <!-- Transformer: End1 is at 110 kV off-nominal on a 220 kV bus (ratedU1=110≠base_kv=220).
   All impedance in End1.  RTC on End1 → has_rtc=true path. -->
  <cim:PowerTransformer rdf:ID="XFMR"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>2.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>20.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<!-- RTC on End1 → has_rtc_on_this_xfmr=true -->
<cim:TransformerEnd.RatioTapChanger rdf:resource="#RTC1"/>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_110"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
  </cim:PowerTransformerEnd>
  <cim:RatioTapChanger rdf:ID="RTC1">
<cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>
<cim:TapChanger.step>0</cim:TapChanger.step>
<cim:TapChanger.stepVoltageIncrement>2.0</cim:TapChanger.stepVoltageIncrement>
<cim:RatioTapChanger.TransformerEnd rdf:resource="#END1"/>
  </cim:RatioTapChanger>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1, "one transformer branch");
    let br = &net.branches[0];
    // MAJ-01: z_base = ratedU1 = 110 kV (not base_kv=220 kV)
    // r_pu = 2.0 * 100 / 110² = 0.016529...
    let expected_r = 2.0 * 100.0 / (110.0_f64 * 110.0);
    assert!(
        (br.r - expected_r).abs() < 1e-6,
        "MAJ-01: r_pu must use ratedU1 as z-base: got r={}, expected {expected_r}",
        br.r
    );
    // If the bug were active (base_kv=220): r_pu = 2.0*100/220² = 0.004132 — 4× too small.
    let buggy_r = 2.0 * 100.0 / (220.0_f64 * 220.0);
    assert!(
        (br.r - buggy_r).abs() > 1e-4,
        "MAJ-01: r_pu must NOT equal the buggy base_kv formula ({})",
        buggy_r
    );
}

// -----------------------------------------------------------------------
// MAJ-02: RTC on End1 multiplies tap; End2 divides tap
// -----------------------------------------------------------------------

/// MAJ-02 regression: RTC physically on End1 must multiply tap (standard direction).
/// nominal tap = (ratedU1/ratedU2)*(to_base/from_base) = (220/110)*(110/220) = 1.0
/// End1 ratio = 1 + (step=3 - neutral=0) × 2% = 1.06
/// Expected tap = 1.0 * 1.06 = 1.06
#[test]
fn test_cgmes_rtc_on_end1_multiplies_tap() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<!-- RTC on End1 -->
<cim:TransformerEnd.RatioTapChanger rdf:resource="#RTC_E1"/>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_110"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
  </cim:PowerTransformerEnd>
  <cim:RatioTapChanger rdf:ID="RTC_E1">
<cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>
<cim:TapChanger.step>3</cim:TapChanger.step>
<cim:TapChanger.stepVoltageIncrement>2.0</cim:TapChanger.stepVoltageIncrement>
<cim:RatioTapChanger.TransformerEnd rdf:resource="#END1"/>
  </cim:RatioTapChanger>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // nominal tap = (220/110)*(110/220) = 1.0
    // End1 step ratio = 1 + (3 - 0) * 2/100 = 1.06
    // tap *= 1.06  →  expected tap = 1.06
    let expected_tap = 1.0 * (1.0 + (3.0 - 0.0) * 2.0 / 100.0);
    assert!(
        (br.tap - expected_tap).abs() < 1e-6,
        "MAJ-02: End1 RTC must multiply tap: got {}, expected {expected_tap}",
        br.tap
    );
}

/// MAJ-02 regression: RTC physically on End2 must divide tap (inverted direction).
/// nominal tap = (220/110)*(110/220) = 1.0
/// End2 ratio = 1 + (step=3 - neutral=0) × 2% = 1.06
/// Expected tap = 1.0 / 1.06 ≈ 0.94340
#[test]
fn test_cgmes_rtc_on_end2_divides_tap() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:BaseVoltage rdf:ID="BV_110">
<cim:BaseVoltage.nominalVoltage>110.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_110"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_110"/>
<cim:PowerTransformerEnd.ratedU>110.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<!-- RTC on End2 only -->
<cim:TransformerEnd.RatioTapChanger rdf:resource="#RTC_E2"/>
  </cim:PowerTransformerEnd>
  <cim:RatioTapChanger rdf:ID="RTC_E2">
<cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>
<cim:TapChanger.step>3</cim:TapChanger.step>
<cim:TapChanger.stepVoltageIncrement>2.0</cim:TapChanger.stepVoltageIncrement>
<cim:RatioTapChanger.TransformerEnd rdf:resource="#END2"/>
  </cim:RatioTapChanger>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // nominal tap = (220/110)*(110/220) = 1.0
    // End2 step ratio = 1 + (3 - 0) * 2/100 = 1.06
    // tap /= 1.06  →  expected tap ≈ 0.94340
    let end2_ratio = 1.0 + (3.0 - 0.0) * 2.0 / 100.0;
    let expected_tap = 1.0 / end2_ratio;
    assert!(
        (br.tap - expected_tap).abs() < 1e-6,
        "MAJ-02: End2 RTC must divide tap (tap /= ratio): got {}, expected {expected_tap}",
        br.tap
    );
}

// -----------------------------------------------------------------------
// MAJ-03: PTC on End2 negates the shift angle
// -----------------------------------------------------------------------

/// MAJ-03 regression: PTC physically on End2 must negate the computed phase shift.
/// step=2, neutral=0, stepPhaseShiftIncrement=3 deg/step → raw shift = (2-0)*3 = 6°.
/// End2 correction: shift = -6°.
#[test]
fn test_cgmes_ptc_on_end2_negates_shift() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<!-- PTC on End2 only (PhaseTapChangerSymmetrical = generic branch) -->
<cim:TransformerEnd.PhaseTapChanger rdf:resource="#PTC_E2"/>
  </cim:PowerTransformerEnd>
  <cim:PhaseTapChangerSymmetrical rdf:ID="PTC_E2">
<cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>
<cim:TapChanger.step>2</cim:TapChanger.step>
<cim:PhaseTapChanger.stepPhaseShiftIncrement>3.0</cim:PhaseTapChanger.stepPhaseShiftIncrement>
<cim:PhaseTapChanger.TransformerEnd rdf:resource="#END2"/>
  </cim:PhaseTapChangerSymmetrical>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // PTC on End2: raw shift = (2-0)*3 = 6°, negated → -6°
    let expected_shift = (-6.0_f64).to_radians();
    assert!(
        (br.phase_shift_rad - expected_shift).abs() < 1e-9,
        "MAJ-03: PTC on End2 must negate shift: got {}, expected {expected_shift}",
        br.phase_shift_rad
    );
}

/// MAJ-03 regression: PTC physically on End1 must NOT negate shift.
/// step=2, neutral=0, stepPhaseShiftIncrement=3 deg/step → shift = +6°.
#[test]
fn test_cgmes_ptc_on_end1_does_not_negate_shift() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_220">
<cim:BaseVoltage.nominalVoltage>220.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:TopologicalNode rdf:ID="TN2">
<cim:IdentifiedObject.name>Bus2</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_220"/>
  </cim:TopologicalNode>
  <cim:PowerTransformer rdf:ID="XFMR"/>
  <cim:PowerTransformerEnd rdf:ID="END1">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T1"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>1.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>10.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
<!-- PTC on End1 -->
<cim:TransformerEnd.PhaseTapChanger rdf:resource="#PTC_E1"/>
  </cim:PowerTransformerEnd>
  <cim:PowerTransformerEnd rdf:ID="END2">
<cim:PowerTransformerEnd.PowerTransformer rdf:resource="#XFMR"/>
<cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>
<cim:TransformerEnd.Terminal rdf:resource="#T2"/>
<cim:TransformerEnd.BaseVoltage rdf:resource="#BV_220"/>
<cim:PowerTransformerEnd.ratedU>220.0</cim:PowerTransformerEnd.ratedU>
<cim:PowerTransformerEnd.r>0.0</cim:PowerTransformerEnd.r>
<cim:PowerTransformerEnd.x>0.0</cim:PowerTransformerEnd.x>
<cim:PowerTransformerEnd.b>0.0</cim:PowerTransformerEnd.b>
  </cim:PowerTransformerEnd>
  <cim:PhaseTapChangerSymmetrical rdf:ID="PTC_E1">
<cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>
<cim:TapChanger.step>2</cim:TapChanger.step>
<cim:PhaseTapChanger.stepPhaseShiftIncrement>3.0</cim:PhaseTapChanger.stepPhaseShiftIncrement>
<cim:PhaseTapChanger.TransformerEnd rdf:resource="#END1"/>
  </cim:PhaseTapChangerSymmetrical>
  <cim:Terminal rdf:ID="T1">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
<cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:Terminal rdf:ID="T2">
<cim:Terminal.ConductingEquipment rdf:resource="#XFMR"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
<cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>
  </cim:Terminal>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-50.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <cim:EnergyConsumer rdf:ID="LOAD1">
<cim:EnergyConsumer.p>50.0</cim:EnergyConsumer.p>
<cim:EnergyConsumer.q>0.0</cim:EnergyConsumer.q>
  </cim:EnergyConsumer>
  <cim:Terminal rdf:ID="TLOAD">
<cim:Terminal.ConductingEquipment rdf:resource="#LOAD1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN2"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert_eq!(net.branches.len(), 1);
    let br = &net.branches[0];
    // PTC on End1: shift = (2-0)*3 = +6° (no negation)
    let expected_shift = 6.0_f64.to_radians();
    assert!(
        (br.phase_shift_rad - expected_shift).abs() < 1e-9,
        "MAJ-03: PTC on End1 must NOT negate shift: got {}, expected {expected_shift}",
        br.phase_shift_rad
    );
}

// -----------------------------------------------------------------------
// MAJ-04: RegulatingControl mode must be "voltage" to use targetValue as kV
// -----------------------------------------------------------------------

/// MAJ-04 regression: when RegulatingControl.mode = "reactivePower", the targetValue
/// is in MVAr and must NOT be used as a kV setpoint.  Expected generator Vs = 1.0 pu.
#[test]
fn test_cgmes_regulating_control_mode_reactive_power_ignored() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_10">
<cim:BaseVoltage.nominalVoltage>10.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_10"/>
  </cim:TopologicalNode>
  <!-- RegulatingControl with mode=reactivePower and targetValue=50 (MVAr, NOT kV) -->
  <cim:RegulatingControl rdf:ID="RC1">
<cim:RegulatingControl.targetValue>50.0</cim:RegulatingControl.targetValue>
<cim:RegulatingControl.mode rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#RegulatingControlModeKind.reactivePower"/>
  </cim:RegulatingControl>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-30.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-5.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:RegulatingCondEq.RegulatingControl rdf:resource="#RC1"/>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert!(!net.generators.is_empty(), "generator must be created");
    let g = &net.generators[0];
    // mode=reactivePower → targetValue=50 is in MVAr, NOT kV → Vs must be 1.0 pu (flat start)
    assert!(
        (g.voltage_setpoint_pu - 1.0).abs() < 1e-9,
        "MAJ-04: mode=reactivePower must not be used as kV; expected Vs=1.0, got {}",
        g.voltage_setpoint_pu
    );
}

/// MAJ-04 regression: when RegulatingControl.mode = "voltage", the targetValue IS kV
/// and must be used as the setpoint.  targetValue=10.5 kV, base_kv=10 → Vs = 1.05 pu.
#[test]
fn test_cgmes_regulating_control_mode_voltage_used() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_10">
<cim:BaseVoltage.nominalVoltage>10.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_10"/>
  </cim:TopologicalNode>
  <!-- RegulatingControl with mode=voltage and targetValue=10.5 kV -->
  <cim:RegulatingControl rdf:ID="RC1">
<cim:RegulatingControl.targetValue>10.5</cim:RegulatingControl.targetValue>
<cim:RegulatingControl.mode rdf:resource="http://iec.ch/TC57/2013/CIM-schema-cim16#RegulatingControlModeKind.voltage"/>
  </cim:RegulatingControl>
  <cim:SynchronousMachine rdf:ID="SM1">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-30.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-5.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
<cim:RegulatingCondEq.RegulatingControl rdf:resource="#RC1"/>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM1">
<cim:Terminal.ConductingEquipment rdf:resource="#SM1"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    assert!(!net.generators.is_empty(), "generator must be created");
    let g = &net.generators[0];
    // mode=voltage → targetValue=10.5 kV / 10 kV base = 1.05 pu
    let expected_vs = 10.5 / 10.0;
    assert!(
        (g.voltage_setpoint_pu - expected_vs).abs() < 1e-9,
        "MAJ-04: mode=voltage must use targetValue as kV setpoint; expected Vs={expected_vs}, got {}",
        g.voltage_setpoint_pu
    );
}

// -----------------------------------------------------------------------
// MAJ-05: controlEnabled=false for non-motor generators → explicit fixed-output generator
// -----------------------------------------------------------------------

/// MAJ-05 regression: non-regulating SynchronousMachine must remain explicit
/// generator equipment so node-breaker retopology can move it exactly.
#[test]
fn test_cgmes_sm_control_disabled_remains_explicit_generator() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:cim="http://iec.ch/TC57/2013/CIM-schema-cim16#">
  <cim:BaseVoltage rdf:ID="BV_10">
<cim:BaseVoltage.nominalVoltage>10.0</cim:BaseVoltage.nominalVoltage>
  </cim:BaseVoltage>
  <cim:TopologicalNode rdf:ID="TN1">
<cim:IdentifiedObject.name>Bus1</cim:IdentifiedObject.name>
<cim:TopologicalNode.BaseVoltage rdf:resource="#BV_10"/>
  </cim:TopologicalNode>
  <!-- Slack generator needed to anchor the island -->
  <cim:SynchronousMachine rdf:ID="SM_SLACK">
<cim:SynchronousMachine.referencePriority>1</cim:SynchronousMachine.referencePriority>
<cim:RotatingMachine.p>-80.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>0.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>200.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-200.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM_SLACK">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_SLACK"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
  <!-- Non-motor generator with controlEnabled=false → must be PQ injection -->
  <cim:SynchronousMachine rdf:ID="SM_OFF">
<cim:RotatingMachine.p>-30.0</cim:RotatingMachine.p>
<cim:RotatingMachine.q>-10.0</cim:RotatingMachine.q>
<cim:SynchronousMachine.maxQ>100.0</cim:SynchronousMachine.maxQ>
<cim:SynchronousMachine.minQ>-100.0</cim:SynchronousMachine.minQ>
<cim:RegulatingCondEq.controlEnabled>false</cim:RegulatingCondEq.controlEnabled>
  </cim:SynchronousMachine>
  <cim:Terminal rdf:ID="TSM_OFF">
<cim:Terminal.ConductingEquipment rdf:resource="#SM_OFF"/>
<cim:Terminal.TopologicalNode rdf:resource="#TN1"/>
  </cim:Terminal>
</rdf:RDF>"##;

    let net = parse_str(xml).expect("parse failed");
    // Both machines remain explicit generators; only the slack machine regulates voltage.
    assert_eq!(
        net.generators.len(),
        2,
        "MAJ-05: SM with controlEnabled=false must remain explicit; got {} generators",
        net.generators.len()
    );
    let sm_off = net
        .generators
        .iter()
        .find(|g| g.machine_id.as_deref() == Some("SM_OFF"))
        .expect("SM_OFF generator missing");
    assert!(!sm_off.voltage_regulated);
    assert!((sm_off.p - 30.0).abs() < 1e-6);
    assert!((sm_off.q - 10.0).abs() < 1e-6);

    // Fixed-P/Q machine output should not be hidden in bus aggregates.
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    assert!(
        bus_pd[0].abs() < 1e-6,
        "MAJ-05: explicit SM output must not be baked into bus pd; got pd={}",
        bus_pd[0]
    );
    assert!(
        bus_qd[0].abs() < 1e-6,
        "MAJ-05: explicit SM output must not be baked into bus qd; got qd={}",
        bus_qd[0]
    );
}

// -----------------------------------------------------------------------
// Integration tests for CGMES extension profiles (Phase 12)
// -----------------------------------------------------------------------

/// Helper: build a minimal Network with 2 buses, 1 line branch, 1 transformer branch, 1 generator.
fn make_test_network() -> surge_network::Network {
    use surge_network::network::{Bus, BusType, Generator};

    let mut net = surge_network::Network::new("test_ext");
    net.buses.push(Bus {
        number: 1,
        name: "Bus1".to_string(),
        base_kv: 220.0,
        bus_type: BusType::Slack,
        voltage_magnitude_pu: 1.0,
        voltage_angle_rad: 0.0,
        ..Default::default()
    });
    net.buses.push(Bus {
        number: 2,
        name: "Bus2".to_string(),
        base_kv: 110.0,
        bus_type: BusType::PQ,
        voltage_magnitude_pu: 1.0,
        voltage_angle_rad: 0.0,
        ..Default::default()
    });
    // Line branch (not a transformer)
    net.branches.push(Branch {
        from_bus: 1,
        to_bus: 2,
        r: 0.01,
        x: 0.1,
        b: 0.02,
        rating_a_mva: 100.0,
        tap: 1.0,
        phase_shift_rad: 0.0,
        in_service: true,
        circuit: "LINE_1_2".to_string(),
        ..Default::default()
    });
    // Transformer branch
    net.branches.push(Branch {
        from_bus: 1,
        to_bus: 2,
        r: 0.005,
        x: 0.05,
        b: 0.0,
        rating_a_mva: 200.0,
        tap: 1.05,
        phase_shift_rad: 0.0,
        in_service: true,
        circuit: "XFMR_1_2".to_string(),
        ..Default::default()
    });
    net.generators.push(Generator {
        bus: 1,
        p: 100.0,
        q: 20.0,
        pmax: 200.0,
        pmin: 10.0,
        in_service: true,
        ..Default::default()
    });
    net
}

#[test]
fn test_short_circuit_roundtrip() {
    use super::writer::{CgmesVersion, write_short_circuit_profile};
    use num_complex::Complex64;

    let mut net = make_test_network();

    // Add zero/neg sequence data on the line branch
    {
        let zs = net.branches[0]
            .zero_seq
            .get_or_insert_with(surge_network::network::ZeroSeqData::default);
        zs.r0 = 0.03;
        zs.x0 = 0.3;
        zs.b0 = 0.06;
    }

    // Add zero-sequence data on the transformer branch + connection
    {
        let zs = net.branches[1]
            .zero_seq
            .get_or_insert_with(surge_network::network::ZeroSeqData::default);
        zs.r0 = 0.015;
        zs.x0 = 0.15;
        zs.zn = Some(Complex64::new(0.001, 0.01));
    }
    net.branches[1]
        .transformer_data
        .get_or_insert_with(surge_network::network::TransformerData::default)
        .transformer_connection = surge_network::network::TransformerConnection::WyeGDelta;

    // Add sequence data on the generator
    net.generators[0]
        .fault_data
        .get_or_insert_with(Default::default)
        .r0_pu = Some(0.005);
    net.generators[0]
        .fault_data
        .get_or_insert_with(Default::default)
        .x0_pu = Some(0.15);
    net.generators[0]
        .fault_data
        .get_or_insert_with(Default::default)
        .r2_pu = Some(0.01);
    net.generators[0]
        .fault_data
        .get_or_insert_with(Default::default)
        .x2_pu = Some(0.18);

    let xml = write_short_circuit_profile(&net, CgmesVersion::V2_4_15).unwrap();

    // ACLineSegment zero-sequence
    assert!(
        xml.contains("ACLineSegment.r0"),
        "SC profile should contain ACLineSegment.r0"
    );
    assert!(
        xml.contains("ACLineSegment.x0"),
        "SC profile should contain ACLineSegment.x0"
    );
    assert!(
        xml.contains("ACLineSegment.b0ch"),
        "SC profile should contain ACLineSegment.b0ch"
    );

    // PowerTransformerEnd with connection kind
    assert!(
        xml.contains("PowerTransformerEnd.connectionKind"),
        "SC profile should contain PowerTransformerEnd.connectionKind"
    );
    assert!(
        xml.contains("PowerTransformerEnd.r0"),
        "SC profile should contain PowerTransformerEnd.r0"
    );
    assert!(
        xml.contains("PowerTransformerEnd.rground"),
        "SC profile should contain grounding impedance"
    );

    // SynchronousMachine sequence data
    assert!(
        xml.contains("SynchronousMachine.r0"),
        "SC profile should contain SynchronousMachine.r0"
    );
    assert!(
        xml.contains("SynchronousMachine.x2"),
        "SC profile should contain SynchronousMachine.x2"
    );
}

#[test]
fn test_measurement_roundtrip() {
    use super::writer::{CgmesVersion, write_measurement_profile};
    use surge_network::network::measurement::{
        CimMeasurement, CimMeasurementType, MeasurementSource,
    };

    let mut net = make_test_network();

    // Voltage measurement (Analog)
    net.cim.measurements.push(CimMeasurement {
        mrid: "MEAS_V1".to_string(),
        name: "Bus1_Voltage".to_string(),
        measurement_type: CimMeasurementType::VoltageMagnitude,
        bus: 1,
        value: 220.5,
        sigma: 0.5,
        enabled: true,
        source: MeasurementSource::Scada,
        ..Default::default()
    });

    // Power flow measurement (Analog)
    net.cim.measurements.push(CimMeasurement {
        mrid: "MEAS_P1".to_string(),
        name: "Line1_ActivePower".to_string(),
        measurement_type: CimMeasurementType::ActivePower,
        bus: 1,
        value: 95.0,
        sigma: 1.0,
        enabled: true,
        source: MeasurementSource::Scada,
        ..Default::default()
    });

    // Switch status measurement (Discrete)
    net.cim.measurements.push(CimMeasurement {
        mrid: "MEAS_SW1".to_string(),
        name: "Breaker1_Status".to_string(),
        measurement_type: CimMeasurementType::SwitchStatus,
        bus: 1,
        value: 1.0,
        sigma: 0.0,
        enabled: true,
        source: MeasurementSource::Scada,
        ..Default::default()
    });

    let xml = write_measurement_profile(&net, CgmesVersion::V2_4_15).unwrap();

    // Analog elements for voltage and power
    assert!(
        xml.contains("<cim:Analog rdf:ID=\"MEAS_V1\">"),
        "should contain Analog for voltage measurement"
    );
    assert!(
        xml.contains("VoltageMagnitude"),
        "should contain VoltageMagnitude measurementType"
    );
    assert!(
        xml.contains("ActivePower"),
        "should contain ActivePower measurementType"
    );

    // Discrete element for switch status
    assert!(
        xml.contains("<cim:Discrete rdf:ID=\"MEAS_SW1\">"),
        "should contain Discrete for switch status"
    );
    assert!(
        xml.contains("SwitchStatus"),
        "should contain SwitchStatus measurementType"
    );

    // Value elements
    assert!(
        xml.contains("AnalogValue"),
        "should contain AnalogValue elements"
    );
    assert!(
        xml.contains("DiscreteValue"),
        "should contain DiscreteValue elements"
    );
}

#[test]
fn test_operational_limits_complete_hierarchy() {
    use super::writer::{CgmesVersion, write_operational_limits_profile};
    use surge_network::network::op_limits::{
        LimitDirection, LimitDuration, LimitKind, OperationalLimit, OperationalLimitSet,
    };

    let mut net = make_test_network();

    // PATL with MW limit
    let patl_limit = OperationalLimit {
        value: 500.0,
        duration: LimitDuration::Permanent,
        direction: LimitDirection::High,
        limit_type_mrid: None,
    };

    // TATL with MVA limit (15 min)
    let tatl_limit = OperationalLimit {
        value: 600.0,
        duration: LimitDuration::Temporary(900.0),
        direction: LimitDirection::High,
        limit_type_mrid: None,
    };

    // IATL with Current limit
    let iatl_limit = OperationalLimit {
        value: 2000.0,
        duration: LimitDuration::Instantaneous,
        direction: LimitDirection::AbsoluteValue,
        limit_type_mrid: None,
    };

    // Voltage limit
    let voltage_limit = OperationalLimit {
        value: 245.0,
        duration: LimitDuration::Permanent,
        direction: LimitDirection::High,
        limit_type_mrid: None,
    };

    let ls = OperationalLimitSet {
        mrid: "OLS_LINE1".to_string(),
        name: "Line1 Limits".to_string(),
        bus: 1,
        equipment_mrid: Some("LINE_1_2".to_string()),
        from_end: Some(true),
        limits: vec![
            (LimitKind::ActivePower, patl_limit),
            (LimitKind::ApparentPower, tatl_limit),
            (LimitKind::Current, iatl_limit),
            (LimitKind::Voltage, voltage_limit),
        ],
    };

    net.cim
        .operational_limits
        .limit_sets
        .insert("OLS_LINE1".to_string(), ls);

    let xml = write_operational_limits_profile(&net, CgmesVersion::V2_4_15).unwrap();

    assert!(
        xml.contains("OperationalLimitSet"),
        "should contain OperationalLimitSet"
    );
    assert!(
        xml.contains("OperationalLimitType"),
        "should contain OperationalLimitType"
    );
    assert!(
        xml.contains("ActivePowerLimit"),
        "should contain ActivePowerLimit"
    );
    assert!(
        xml.contains("ApparentPowerLimit"),
        "should contain ApparentPowerLimit"
    );
    assert!(xml.contains("CurrentLimit"), "should contain CurrentLimit");
    assert!(xml.contains("VoltageLimit"), "should contain VoltageLimit");
    // PATL/TATL/IATL duration names
    assert!(xml.contains("PATL"), "should contain PATL duration label");
    assert!(xml.contains("TATL"), "should contain TATL duration label");
    assert!(xml.contains("IATL"), "should contain IATL duration label");
    // Temporary duration value
    assert!(
        xml.contains("acceptableDuration>900<"),
        "TATL should have acceptableDuration=900"
    );
}

#[test]
fn test_boundary_with_equivalents() {
    use super::writer::{CgmesVersion, write_boundary_profile};
    use surge_network::network::boundary::{
        BoundaryData, BoundaryPoint, EquivalentBranchData, EquivalentNetworkData,
        EquivalentShuntData, ModelAuthoritySet,
    };

    let mut net = make_test_network();

    net.cim.boundary_data = BoundaryData {
        boundary_points: vec![BoundaryPoint {
            mrid: "BP_DE_FR_1".to_string(),
            connectivity_node_mrid: Some("CN_BORDER_1".to_string()),
            from_end_iso_code: Some("DE".to_string()),
            to_end_iso_code: Some("FR".to_string()),
            from_end_name: Some("TenneT".to_string()),
            to_end_name: Some("RTE".to_string()),
            from_end_name_tso: Some("TENNET".to_string()),
            to_end_name_tso: Some("RTE".to_string()),
            is_direct_current: false,
            is_excluded_from_area_interchange: false,
            bus: Some(1),
        }],
        model_authority_sets: vec![ModelAuthoritySet {
            mrid: "MAS_TENNET".to_string(),
            name: "TenneT TSO".to_string(),
            description: Some("TenneT Germany".to_string()),
            members: vec!["LINE_1_2".to_string()],
        }],
        equivalent_networks: vec![EquivalentNetworkData {
            mrid: "EQNET_FR".to_string(),
            name: "France Equivalent".to_string(),
            description: Some("RTE external equivalent".to_string()),
            region_mrid: Some("RGN_FR".to_string()),
        }],
        equivalent_branches: vec![EquivalentBranchData {
            mrid: "EQBR_1".to_string(),
            network_mrid: Some("EQNET_FR".to_string()),
            r_ohm: 5.0,
            x_ohm: 50.0,
            r0_ohm: Some(15.0),
            x0_ohm: Some(150.0),
            r2_ohm: Some(5.5),
            x2_ohm: Some(55.0),
            from_bus: Some(1),
            to_bus: Some(2),
        }],
        equivalent_shunts: vec![EquivalentShuntData {
            mrid: "EQSH_1".to_string(),
            network_mrid: Some("EQNET_FR".to_string()),
            g_s: 0.001,
            b_s: 0.05,
            bus: Some(1),
        }],
    };

    let xml = write_boundary_profile(&net, CgmesVersion::V2_4_15).unwrap();

    // BoundaryPoint with ISO codes
    assert!(
        xml.contains("BoundaryPoint rdf:ID=\"BP_DE_FR_1\""),
        "should contain BoundaryPoint"
    );
    assert!(
        xml.contains("fromEndIsoCode>DE<"),
        "should contain from ISO code DE"
    );
    assert!(
        xml.contains("toEndIsoCode>FR<"),
        "should contain to ISO code FR"
    );

    // EquivalentBranch with impedances
    assert!(
        xml.contains("EquivalentBranch rdf:ID=\"EQBR_1\""),
        "should contain EquivalentBranch"
    );
    assert!(
        xml.contains("EquivalentBranch.r>5<"),
        "should contain r=5 ohm"
    );
    assert!(
        xml.contains("EquivalentBranch.x>50<"),
        "should contain x=50 ohm"
    );

    // EquivalentShunt
    assert!(
        xml.contains("EquivalentShunt rdf:ID=\"EQSH_1\""),
        "should contain EquivalentShunt"
    );

    // ModelAuthoritySet
    assert!(
        xml.contains("ModelAuthoritySet"),
        "should contain ModelAuthoritySet"
    );

    // EquivalentNetwork
    assert!(
        xml.contains("EquivalentNetwork"),
        "should contain EquivalentNetwork"
    );
}

#[test]
fn test_protection_relay_settings() {
    use super::writer::{CgmesVersion, write_protection_profile};
    use surge_network::network::protection::{
        CurrentRelaySettings, DistanceRelaySettings, ProtectionData, RecloseSequenceData,
        RecloseShot, SynchrocheckSettings,
    };

    let mut net = make_test_network();

    net.cim.protection_data = ProtectionData {
        current_relays: vec![CurrentRelaySettings {
            mrid: "CR_1".to_string(),
            name: "OC_Relay_Bus1".to_string(),
            phase_pickup_a: Some(600.0),
            ground_pickup_a: Some(200.0),
            neg_seq_pickup_a: Some(150.0),
            phase_time_dial_s: Some(0.3),
            ground_time_dial_s: Some(0.5),
            neg_seq_time_dial_s: Some(0.4),
            inverse_time: true,
            directional: false,
            bus: Some(1),
            protected_switch_mrid: Some("BRK_1".to_string()),
        }],
        distance_relays: vec![DistanceRelaySettings {
            mrid: "DR_1".to_string(),
            name: "Dist_Relay_Bus1".to_string(),
            forward_reach_ohm: Some(25.0),
            forward_blind_ohm: Some(5.0),
            backward_reach_ohm: Some(8.0),
            backward_blind_ohm: Some(2.0),
            mho_angle_deg: Some(75.0),
            zero_seq_rx_ratio: Some(2.5),
            zero_seq_reach_ohm: Some(30.0),
            bus: Some(1),
            protected_switch_mrid: Some("BRK_1".to_string()),
        }],
        reclose_sequences: vec![RecloseSequenceData {
            protected_switch_mrid: "BRK_1".to_string(),
            shots: vec![
                RecloseShot {
                    step: 1,
                    delay_s: 0.5,
                },
                RecloseShot {
                    step: 2,
                    delay_s: 15.0,
                },
                RecloseShot {
                    step: 3,
                    delay_s: 60.0,
                },
            ],
        }],
        synchrocheck_relays: vec![SynchrocheckSettings {
            mrid: "SC_1".to_string(),
            name: "Synchrocheck_Bus1".to_string(),
            max_angle_diff_deg: Some(20.0),
            max_freq_diff_hz: Some(0.2),
            max_volt_diff_pu: Some(0.05),
            bus: Some(1),
            protected_switch_mrid: Some("BRK_1".to_string()),
        }],
    };

    let xml = write_protection_profile(&net, CgmesVersion::V2_4_15).unwrap();

    // CurrentRelay
    assert!(
        xml.contains("CurrentRelay rdf:ID=\"CR_1\""),
        "should contain CurrentRelay"
    );
    assert!(
        xml.contains("currentLimit1>600<"),
        "should contain phase pickup 600A"
    );
    assert!(
        xml.contains("inverseTimeFlag>true<"),
        "should contain inverse time flag"
    );

    // DistanceRelay (as ProtectionEquipment)
    assert!(
        xml.contains("ProtectionEquipment rdf:ID=\"DR_1\""),
        "should contain ProtectionEquipment for distance relay"
    );
    assert!(
        xml.contains("highLimit>25<"),
        "should contain forward reach 25 ohm"
    );

    // RecloseSequence
    assert!(
        xml.contains("RecloseSequence"),
        "should contain RecloseSequence"
    );
    assert!(
        xml.contains("recloseDelay>0.5<"),
        "should contain first reclose delay 0.5s"
    );

    // SynchrocheckRelay
    assert!(
        xml.contains("SynchrocheckRelay rdf:ID=\"SC_1\""),
        "should contain SynchrocheckRelay"
    );
    assert!(
        xml.contains("maxAngleDiff>20<"),
        "should contain maxAngleDiff 20 deg"
    );
    assert!(
        xml.contains("maxFreqDiff>0.2<"),
        "should contain maxFreqDiff 0.2 Hz"
    );
}

#[test]
fn test_network_operations_switching_and_outages() {
    use super::writer::{CgmesVersion, write_network_operations_profile};
    use surge_network::network::net_ops::{
        CrewRecord, CrewStatus, NetworkOperationsData, OutageCause, OutageRecord,
        OutageScheduleData, SwitchingPlan, SwitchingStep, SwitchingStepKind, WorkTaskKind,
        WorkTaskRecord, WorkTaskStatus,
    };
    use surge_network::network::time_utils::parse_iso8601;

    let mut net = make_test_network();

    net.cim.network_operations = NetworkOperationsData {
        switching_plans: vec![SwitchingPlan {
            mrid: "SP_1".to_string(),
            name: "Maintenance Plan 1".to_string(),
            purpose: Some("Transformer maintenance".to_string()),
            planned_start: parse_iso8601("2026-03-15T08:00:00Z"),
            planned_end: parse_iso8601("2026-03-15T16:00:00Z"),
            approved_date_time: None,
            steps: vec![
                SwitchingStep {
                    sequence_number: 1,
                    kind: Some(SwitchingStepKind::Open),
                    switch_mrid: Some("BRK_1".to_string()),
                    equipment_mrid: None,
                    description: Some("Open breaker 1".to_string()),
                    is_free_sequence: false,
                    executed_date_time: None,
                },
                SwitchingStep {
                    sequence_number: 2,
                    kind: Some(SwitchingStepKind::Ground),
                    switch_mrid: None,
                    equipment_mrid: None,
                    description: Some("Ground bus section".to_string()),
                    is_free_sequence: false,
                    executed_date_time: None,
                },
            ],
        }],
        outage_records: vec![OutageRecord {
            mrid: "OUT_1".to_string(),
            name: "XFMR Maintenance Outage".to_string(),
            is_planned: true,
            cause: Some(OutageCause::Maintenance),
            equipment_mrids: vec!["XFMR_1_2".to_string()],
            planned_start: parse_iso8601("2026-03-15T08:00:00Z"),
            planned_end: parse_iso8601("2026-03-15T16:00:00Z"),
            actual_start: None,
            actual_end: None,
            cancelled_date_time: None,
            estimated_restore: None,
            area_name: None,
        }],
        outage_schedules: vec![OutageScheduleData {
            mrid: "OSCHED_1".to_string(),
            name: "Spring 2026 Schedule".to_string(),
            horizon_start: parse_iso8601("2026-03-01T00:00:00Z"),
            horizon_end: parse_iso8601("2026-05-31T23:59:59Z"),
            outages: vec!["OUT_1".to_string()],
        }],
        crews: vec![CrewRecord {
            mrid: "CREW_1".to_string(),
            name: "Line Crew Alpha".to_string(),
            crew_type: Some("Transmission".to_string()),
            status: Some(CrewStatus::Available),
        }],
        work_tasks: vec![WorkTaskRecord {
            mrid: "WT_1".to_string(),
            name: "Replace transformer bushings".to_string(),
            crew_mrid: Some("CREW_1".to_string()),
            outage_mrid: Some("OUT_1".to_string()),
            scheduled_start: parse_iso8601("2026-03-15T09:00:00Z"),
            scheduled_end: parse_iso8601("2026-03-15T15:00:00Z"),
            task_kind: Some(WorkTaskKind::Replace),
            priority: Some(1),
            status: Some(WorkTaskStatus::Scheduled),
        }],
    };

    let xml = write_network_operations_profile(&net, CgmesVersion::V2_4_15).unwrap();

    // SwitchingPlan with SwitchingStep children
    assert!(
        xml.contains("SwitchingPlan rdf:ID=\"SP_1\""),
        "should contain SwitchingPlan"
    );
    assert!(
        xml.contains("SwitchingStep.kind>open<"),
        "should contain open switching step"
    );
    assert!(
        xml.contains("SwitchingStep.kind>ground<"),
        "should contain ground switching step"
    );
    assert!(
        xml.contains("sequenceNumber>1<"),
        "should contain sequence number 1"
    );
    assert!(
        xml.contains("sequenceNumber>2<"),
        "should contain sequence number 2"
    );

    // PlannedOutage
    assert!(
        xml.contains("PlannedOutage rdf:ID=\"OUT_1\""),
        "should contain PlannedOutage"
    );
    assert!(
        xml.contains("Outage.cause>maintenance<"),
        "should contain maintenance cause"
    );

    // Crew
    assert!(
        xml.contains("Crew rdf:ID=\"CREW_1\""),
        "should contain Crew"
    );
    assert!(
        xml.contains("Crew.status>available<"),
        "should contain available status"
    );

    // WorkTask
    assert!(
        xml.contains("WorkTask rdf:ID=\"WT_1\""),
        "should contain WorkTask"
    );
    assert!(
        xml.contains("taskKind>replace<"),
        "should contain replace task kind"
    );
}

#[test]
fn test_merge_preserves_extension_data() {
    use super::merge::merge_networks;
    use surge_network::network::boundary::{BoundaryData, BoundaryPoint};
    use surge_network::network::measurement::{
        CimMeasurement, CimMeasurementType, MeasurementSource,
    };

    // Net1: has measurements
    let mut net1 = make_test_network();
    net1.name = "net1".to_string();
    net1.cim.measurements.push(CimMeasurement {
        mrid: "M1".to_string(),
        name: "Voltage_Bus1".to_string(),
        measurement_type: CimMeasurementType::VoltageMagnitude,
        bus: 1,
        value: 221.0,
        sigma: 0.5,
        enabled: true,
        source: MeasurementSource::Scada,
        ..Default::default()
    });

    // Net2: has boundary data
    let mut net2 = make_test_network();
    net2.name = "net2".to_string();
    net2.cim.boundary_data = BoundaryData {
        boundary_points: vec![BoundaryPoint {
            mrid: "BP_NET2".to_string(),
            connectivity_node_mrid: Some("CN_BORDER_NET2".to_string()),
            from_end_iso_code: Some("AT".to_string()),
            to_end_iso_code: Some("CZ".to_string()),
            from_end_name: None,
            to_end_name: None,
            from_end_name_tso: None,
            to_end_name_tso: None,
            is_direct_current: false,
            is_excluded_from_area_interchange: false,
            bus: Some(1),
        }],
        ..Default::default()
    };

    let (merged, report) = merge_networks(vec![net1, net2]).unwrap();

    assert_eq!(report.input_count, 2);

    // Measurements from net1 preserved
    assert!(
        !merged.cim.measurements.is_empty(),
        "merged network should have measurements from net1"
    );
    assert!(
        merged.cim.measurements.iter().any(|m| m.mrid == "M1"),
        "measurement M1 from net1 should be in merged network"
    );

    // Boundary data from net2 preserved
    assert!(
        !merged.cim.boundary_data.boundary_points.is_empty(),
        "merged network should have boundary points from net2"
    );
    assert!(
        merged
            .cim
            .boundary_data
            .boundary_points
            .iter()
            .any(|bp| bp.mrid == "BP_NET2"),
        "boundary point BP_NET2 from net2 should be in merged network"
    );
}

#[test]
fn test_write_all_profiles_skips_empty() {
    use super::writer::{CgmesVersion, write_all_profiles};

    let mut net = make_test_network();

    // Only populate measurements — all other extension fields stay empty
    net.cim
        .measurements
        .push(surge_network::network::measurement::CimMeasurement {
            mrid: "MEAS_V".to_string(),
            name: "Bus1_V".to_string(),
            measurement_type:
                surge_network::network::measurement::CimMeasurementType::VoltageMagnitude,
            bus: 1,
            value: 220.0,
            sigma: 0.5,
            enabled: true,
            source: surge_network::network::measurement::MeasurementSource::Scada,
            ..Default::default()
        });

    let dir = tempfile::tempdir().unwrap();
    write_all_profiles(&net, dir.path(), CgmesVersion::V2_4_15).unwrap();

    // Core profiles always written
    assert!(
        dir.path().join("test_ext_EQ.xml").exists(),
        "EQ profile should always be written"
    );
    assert!(
        dir.path().join("test_ext_TP.xml").exists(),
        "TP profile should always be written"
    );
    assert!(
        dir.path().join("test_ext_SSH.xml").exists(),
        "SSH profile should always be written"
    );
    assert!(
        dir.path().join("test_ext_SV.xml").exists(),
        "SV profile should always be written"
    );

    // Measurement profile should exist (non-empty)
    assert!(
        dir.path().join("test_ext_ME.xml").exists(),
        "ME profile should be written when measurements present"
    );

    // Empty extension profiles should NOT exist
    assert!(
        !dir.path().join("test_ext_SC.xml").exists(),
        "SC profile should NOT be written when no sequence data"
    );
    assert!(
        !dir.path().join("test_ext_AS.xml").exists(),
        "AS profile should NOT be written when asset catalog empty"
    );
    assert!(
        !dir.path().join("test_ext_OL.xml").exists(),
        "OL profile should NOT be written when op limits empty"
    );
    assert!(
        !dir.path().join("test_ext_BD.xml").exists(),
        "BD profile should NOT be written when boundary data empty"
    );
    assert!(
        !dir.path().join("test_ext_PR.xml").exists(),
        "PR profile should NOT be written when protection data empty"
    );
    assert!(
        !dir.path().join("test_ext_NO.xml").exists(),
        "NO profile should NOT be written when network ops empty"
    );
}

#[test]
fn test_load_with_boundary_accepts_zip_archive() {
    use super::writer::{CgmesVersion, write_all_profiles};
    use std::io::Write as _;

    let igm_net = make_test_network();
    let boundary_net = make_test_network();

    let igm_dir = tempfile::tempdir().unwrap();
    let boundary_dir = tempfile::tempdir().unwrap();
    write_all_profiles(&igm_net, igm_dir.path(), CgmesVersion::V2_4_15).unwrap();
    write_all_profiles(&boundary_net, boundary_dir.path(), CgmesVersion::V2_4_15).unwrap();

    let boundary_zip = boundary_dir.path().join("boundary.zip");
    let file = std::fs::File::create(&boundary_zip).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for name in [
        "test_ext_EQ.xml",
        "test_ext_TP.xml",
        "test_ext_SSH.xml",
        "test_ext_SV.xml",
    ] {
        let content = std::fs::read(boundary_dir.path().join(name)).unwrap();
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&content).unwrap();
    }
    zip.finish().unwrap();

    let merged = load_with_boundary(igm_dir.path(), &boundary_zip).unwrap();
    assert_eq!(merged.n_buses(), igm_net.n_buses());
    assert_eq!(merged.n_branches(), igm_net.n_branches());
    assert!(
        !merged.buses.is_empty(),
        "zip-backed boundary load should succeed"
    );
}

#[test]
fn test_iec62325_market_document_integration() {
    use crate::iec62325::parse_market_document;

    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Publication_MarketDocument xmlns="urn:iec62325.351:tc57wg16:451-1:publicationdocument:7:3">
  <mRID>DOC_001</mRID>
  <type>A44</type>
  <sender_MarketParticipant>
    <mRID>SENDER_1</mRID>
    <name>ENTSO-E</name>
  </sender_MarketParticipant>
  <receiver_MarketParticipant>
    <mRID>RECV_1</mRID>
    <name>Amptimal</name>
  </receiver_MarketParticipant>
  <TimeSeries>
    <mRID>TS_001</mRID>
    <businessType>A01</businessType>
    <in_Domain.mRID>10YDE-VE-------2</in_Domain.mRID>
    <out_Domain.mRID>10YFR-RTE------C</out_Domain.mRID>
    <quantity_Measure_Unit.name>MAW</quantity_Measure_Unit.name>
    <Period>
      <timeInterval>
        <start>2026-03-10T00:00Z</start>
        <end>2026-03-11T00:00Z</end>
      </timeInterval>
      <resolution>PT60M</resolution>
      <Point>
        <position>1</position>
        <quantity>1500.5</quantity>
      </Point>
      <Point>
        <position>2</position>
        <quantity>1450.0</quantity>
      </Point>
      <Point>
        <position>3</position>
        <quantity>1600.2</quantity>
      </Point>
    </Period>
  </TimeSeries>
  <TimeSeries>
    <mRID>TS_002</mRID>
    <businessType>A01</businessType>
    <Period>
      <resolution>PT15M</resolution>
      <Point>
        <position>1</position>
        <quantity>800.0</quantity>
        <price.amount>45.50</price.amount>
      </Point>
    </Period>
  </TimeSeries>
</Publication_MarketDocument>"#;

    let data = parse_market_document(xml).unwrap();

    // Document-level metadata
    assert_eq!(
        data.document_mrid.as_deref(),
        Some("DOC_001"),
        "document mRID should be DOC_001"
    );
    assert_eq!(
        data.document_type.as_deref(),
        Some("A44"),
        "document type should be A44"
    );

    // Sender/receiver
    assert!(data.sender.is_some(), "should have sender");
    assert_eq!(data.sender.as_ref().unwrap().name, "ENTSO-E");
    assert!(data.receiver.is_some(), "should have receiver");
    assert_eq!(data.receiver.as_ref().unwrap().name, "Amptimal");

    // Time series count
    assert_eq!(data.time_series.len(), 2, "should have 2 time series");

    // First time series: 3 points
    let ts1 = &data.time_series[0];
    assert_eq!(ts1.mrid, "TS_001");
    assert_eq!(ts1.periods.len(), 1, "TS_001 should have 1 period");
    assert_eq!(
        ts1.periods[0].points.len(),
        3,
        "TS_001 period should have 3 points"
    );
    assert_eq!(ts1.periods[0].points[0].position, 1);
    assert!(
        (ts1.periods[0].points[0].quantity.unwrap() - 1500.5).abs() < 1e-6,
        "first point quantity should be 1500.5"
    );
    assert!(
        (ts1.periods[0].points[2].quantity.unwrap() - 1600.2).abs() < 1e-6,
        "third point quantity should be 1600.2"
    );

    // Second time series: 1 point with price
    let ts2 = &data.time_series[1];
    assert_eq!(ts2.mrid, "TS_002");
    assert_eq!(ts2.periods[0].points.len(), 1);
    assert!(
        (ts2.periods[0].points[0].price.unwrap() - 45.50).abs() < 1e-6,
        "point price should be 45.50"
    );
}
