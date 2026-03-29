// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests for PSS/E RAW parser — DC lines, VSC DC, Area Interchange, and FACTS.
//!
//! Uses hand-crafted minimal PSS/E v33 RAW snippets to verify that the four
//! new sections are parsed correctly and stored in the right Network fields.

use surge_io::psse::raw::loads;
use surge_network::network::FactsMode;
use surge_network::network::LccHvdcControlMode;
use surge_network::network::VscHvdcControlMode;

// ---------------------------------------------------------------------------
// Common PSS/E file header + base network up through the transformer section.
//
// The transformer section terminates with a plain "0 / END OF TRANSFORMER DATA"
// (no "BEGIN <next>" suffix) so that skip_to_section("transformer") does not
// accidentally match the "TRANSFORMER" keyword in the end marker.
//
// After this prefix, each test appends the relevant section data, always
// starting with a proper "BEGIN <section>" section-start marker so that
// seek_section can locate each section correctly.
// ---------------------------------------------------------------------------

const PSSE_PREFIX: &str = "\
0, 100.00, 33 / PSS/E TEST CASE
TEST NETWORK
PSS/E 33 SERIES FORMAT
1, 'BUS1', 345.0, 3, 1, 1, 1, 1.0, 0.0
2, 'BUS2', 345.0, 1, 1, 1, 1, 1.0, 0.0
3, 'BUS3', 345.0, 2, 1, 1, 1, 1.0, 0.0
0 / END OF BUS DATA, BEGIN LOAD DATA
1, '1', 1, 1, 1, 0.0, 0.0
2, '1', 1, 1, 1, 200.0, 50.0
0 / END OF LOAD DATA, BEGIN FIXED SHUNT DATA
0 / END OF FIXED SHUNT DATA, BEGIN GENERATOR DATA
3, '1', 250.0, 0.0, 9999.0, 0.0, 1.02, 0, 100.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1, 100.0, 300.0, 0.0
0 / END OF GENERATOR DATA, BEGIN BRANCH DATA
1, 2, '1', 0.005, 0.05, 0.04, 200.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1
2, 3, '1', 0.01, 0.10, 0.02, 200.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1
0 / END OF BRANCH DATA, BEGIN TRANSFORMER DATA
0 / END OF TRANSFORMER DATA\n";

// Suffix that terminates the file after all sections.
const PSSE_SUFFIX: &str = "\
0 / END OF FACTS CONTROL DEVICE DATA, BEGIN SWITCHED SHUNT DATA
0 / END OF SWITCHED SHUNT DATA
Q\n";

/// Build a complete PSS/E RAW string from the common prefix, optional section blocks,
/// and the common suffix.
fn build_psse(sections: &str) -> String {
    format!("{}{}{}", PSSE_PREFIX, sections, PSSE_SUFFIX)
}

// ---------------------------------------------------------------------------
// Area Interchange
// ---------------------------------------------------------------------------

#[test]
fn test_psse_area_schedule_parsed() {
    let sections = "\
0 / END OF TRANSFORMER DATA, BEGIN AREA INTERCHANGE DATA
1, 1, 0.0, 10.0, 'AREA1'
2, 3, 100.0, 5.0, 'AREA2'
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("PSS/E area interchange parse should succeed");

    assert_eq!(
        network.area_schedules.len(),
        2,
        "expected 2 area interchange records"
    );

    let a1 = &network.area_schedules[0];
    assert_eq!(a1.number, 1);
    assert_eq!(a1.slack_bus, 1);
    assert!((a1.p_desired_mw - 0.0).abs() < 1e-6);
    assert!((a1.p_tolerance_mw - 10.0).abs() < 1e-6);
    assert_eq!(a1.name, "AREA1");

    let a2 = &network.area_schedules[1];
    assert_eq!(a2.number, 2);
    assert_eq!(a2.slack_bus, 3);
    assert!((a2.p_desired_mw - 100.0).abs() < 1e-6);
    assert!((a2.p_tolerance_mw - 5.0).abs() < 1e-6);
    assert_eq!(a2.name, "AREA2");
}

// ---------------------------------------------------------------------------
// Two-Terminal DC Lines
// ---------------------------------------------------------------------------

#[test]
fn test_psse_dc_line_parsed() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
'DC1', 1, 1.0, 100.0, 345.0, 0.0, 0.0, 0.0, 'I', 0.0, 20, 1.0
1, 2, 80.0, 5.0, 0.0, 5.0, 345.0, 1.0, 1.0, 1.1, 0.9, 0.00625, 1, 1, 1, '1', 0.0
3, 2, 80.0, 17.0, 0.0, 5.0, 345.0, 1.0, 1.0, 1.1, 0.9, 0.00625, 1, 1, 1, '1', 0.0
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("PSS/E DC line parse should succeed");

    assert_eq!(network.hvdc.links.len(), 1, "expected 1 DC line");

    let dc = network.hvdc.links[0].as_lcc().expect("lcc link");
    assert_eq!(dc.name, "DC1");
    assert_eq!(dc.mode, LccHvdcControlMode::PowerControl);
    assert!(
        (dc.resistance_ohm - 1.0).abs() < 1e-6,
        "RDC should be 1.0 ohm"
    );
    assert!(
        (dc.scheduled_setpoint - 100.0).abs() < 1e-6,
        "SETVL should be 100 MW"
    );
    assert!(
        (dc.scheduled_voltage_kv - 345.0).abs() < 1e-6,
        "VSCHD should be 345 kV"
    );
    assert_eq!(dc.ac_dc_iteration_max, 20);
    assert!((dc.ac_dc_iteration_acceleration - 1.0).abs() < 1e-6);

    // Rectifier
    assert_eq!(dc.rectifier.bus, 1);
    assert_eq!(dc.rectifier.n_bridges, 2);
    assert!((dc.rectifier.alpha_max - 80.0).abs() < 1e-6);
    assert!((dc.rectifier.alpha_min - 5.0).abs() < 1e-6);
    assert!((dc.rectifier.commutation_reactance_ohm - 5.0).abs() < 1e-6);
    assert!((dc.rectifier.base_voltage_kv - 345.0).abs() < 1e-6);
    assert!(dc.rectifier.in_service);

    // Inverter
    assert_eq!(dc.inverter.bus, 3);
    assert_eq!(dc.inverter.n_bridges, 2);
    assert!(
        (dc.inverter.alpha_min - 17.0).abs() < 1e-6,
        "GAMMN should be 17°"
    );
    assert!(dc.inverter.in_service);
}

#[test]
fn test_psse_dc_line_blocked_mode() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
'DC_BLK', 0, 0.5, 0.0, 345.0, 0.0, 0.0, 0.0, 'I', 0.0, 20, 1.0
1, 1, 90.0, 5.0, 0.0, 0.0, 345.0, 1.0, 1.0, 1.5, 0.5, 0.00625, 1, 1, 1, '1', 0.0
2, 1, 90.0, 5.0, 0.0, 0.0, 345.0, 1.0, 1.0, 1.5, 0.5, 0.00625, 1, 1, 1, '1', 0.0
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("PSS/E blocked DC line parse should succeed");
    assert_eq!(network.hvdc.links.len(), 1);
    assert_eq!(
        network.hvdc.links[0].as_lcc().expect("lcc link").mode,
        LccHvdcControlMode::Blocked
    );
}

// ---------------------------------------------------------------------------
// VSC DC Lines
// ---------------------------------------------------------------------------

#[test]
fn test_psse_vsc_dc_line_parsed() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
'VSC1', 1, 0.5, 1, 1.0
1, 0, 1, 100.0, 1.0, 2.0, 0.01, 0.5, 200.0, -200.0, 1.1, 0.9, 1, 1, 100.0, 0, 1.0, 0, 1.0
3, 0, 2, -100.0, 1.0, 2.0, 0.01, 0.5, 200.0, -200.0, 1.1, 0.9, 1, 1, 100.0, 0, 1.0, 0, 1.0
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("PSS/E VSC DC line parse should succeed");

    assert_eq!(network.hvdc.links.len(), 1, "expected 1 VSC DC line");

    let vsc = network.hvdc.links[0].as_vsc().expect("vsc link");
    assert_eq!(vsc.name, "VSC1");
    assert_eq!(vsc.mode, VscHvdcControlMode::PowerControl);
    assert!((vsc.resistance_ohm - 0.5).abs() < 1e-6);

    // Converter 1
    assert_eq!(vsc.converter1.bus, 1);
    assert!((vsc.converter1.dc_setpoint - 100.0).abs() < 1e-6);
    assert!((vsc.converter1.loss_constant_mw - 2.0).abs() < 1e-6);
    assert!((vsc.converter1.loss_linear - 0.01).abs() < 1e-6);
    assert!((vsc.converter1.q_max_mvar - 200.0).abs() < 1e-6);
    assert!((vsc.converter1.q_min_mvar - (-200.0)).abs() < 1e-6);
    assert!(vsc.converter1.in_service);

    // Converter 2
    assert_eq!(vsc.converter2.bus, 3);
    assert!((vsc.converter2.dc_setpoint - (-100.0)).abs() < 1e-6);
    assert!(vsc.converter2.in_service);
}

// ---------------------------------------------------------------------------
// FACTS Devices
// ---------------------------------------------------------------------------

#[test]
fn test_psse_facts_svc_parsed() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA
'SVC1', 2, 0, 2, 0.0, 0.0, 1.02, 150.0, 1.5, 0.9, 1.1, 200.0, 0.0, 0.0, 100.0, 1, 0.0, 0.0, 1.0, 0\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("PSS/E FACTS SVC parse should succeed");

    assert_eq!(network.facts_devices.len(), 1, "expected 1 FACTS device");

    let facts = &network.facts_devices[0];
    assert_eq!(facts.name, "SVC1");
    assert_eq!(facts.bus_from, 2);
    assert_eq!(facts.bus_to, 0); // shunt-only, no remote bus
    assert_eq!(facts.mode, FactsMode::ShuntOnly);
    assert!(
        (facts.voltage_setpoint_pu - 1.02).abs() < 1e-6,
        "VSET should be 1.02 pu"
    );
    assert!(
        (facts.q_max - 150.0).abs() < 1e-6,
        "SHMX should be 150 MVAr"
    );
    assert!(facts.in_service, "device should be in service");
}

#[test]
fn test_psse_facts_tcsc_parsed() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA
'TCSC1', 1, 2, 1, 50.0, 0.0, 1.0, 0.0, 1.5, 0.9, 1.1, 0.0, 0.0, 0.03, 100.0, 1, 0.0, 0.0, 1.0, 0\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("PSS/E FACTS TCSC parse should succeed");

    assert_eq!(network.facts_devices.len(), 1, "expected 1 FACTS device");

    let facts = &network.facts_devices[0];
    assert_eq!(facts.name, "TCSC1");
    assert_eq!(facts.bus_from, 1);
    assert_eq!(facts.bus_to, 2);
    assert_eq!(facts.mode, FactsMode::SeriesOnly);
    assert!(
        (facts.series_reactance_pu - 0.03).abs() < 1e-6,
        "LINX should be 0.03 pu"
    );
    assert!(
        (facts.p_setpoint_mw - 50.0).abs() < 1e-6,
        "PDES should be 50 MW"
    );
    assert!(facts.in_service);
}

#[test]
fn test_psse_facts_out_of_service() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA
'OOS', 2, 0, 0, 0.0, 0.0, 1.0, 100.0, 1.5, 0.9, 1.1, 0.0, 0.0, 0.0, 100.0, 1, 0.0, 0.0, 1.0, 0\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("PSS/E FACTS OOS parse should succeed");

    assert_eq!(network.facts_devices.len(), 1);
    assert_eq!(network.facts_devices[0].mode, FactsMode::OutOfService);
    assert!(!network.facts_devices[0].in_service);
}

// ---------------------------------------------------------------------------
// Mixed: multiple sections together
// ---------------------------------------------------------------------------

#[test]
fn test_psse_all_dc_facts_sections_together() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN AREA INTERCHANGE DATA
1, 1, 50.0, 10.0, 'ERCOT'
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
'HVDC1', 1, 2.0, 200.0, 500.0, 0.0, 0.0, 0.0, 'I', 0.0, 25, 1.0
1, 2, 80.0, 5.0, 0.0, 8.0, 500.0, 1.0, 1.0, 1.1, 0.9, 0.00625, 1, 1, 1, '1', 0.0
2, 2, 80.0, 17.0, 0.0, 8.0, 500.0, 1.0, 1.0, 1.1, 0.9, 0.00625, 1, 1, 1, '1', 0.0
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA
'SVC_MAIN', 2, 0, 2, 0.0, 0.0, 1.0, 100.0, 1.5, 0.9, 1.1, 0.0, 0.0, 0.0, 100.0, 1, 0.0, 0.0, 1.0, 0\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("Combined DC+FACTS parse should succeed");

    assert_eq!(network.area_schedules.len(), 1);
    assert_eq!(network.area_schedules[0].name, "ERCOT");
    assert!((network.area_schedules[0].p_desired_mw - 50.0).abs() < 1e-6);

    assert_eq!(network.hvdc.links.len(), 1);
    let hvdc = network.hvdc.links[0].as_lcc().expect("lcc link");
    assert_eq!(hvdc.name, "HVDC1");
    assert!((hvdc.scheduled_setpoint - 200.0).abs() < 1e-6);
    assert!((hvdc.scheduled_voltage_kv - 500.0).abs() < 1e-6);

    assert_eq!(
        network
            .hvdc
            .links
            .iter()
            .filter_map(|link| link.as_vsc())
            .count(),
        0
    );

    assert_eq!(network.facts_devices.len(), 1);
    assert_eq!(network.facts_devices[0].name, "SVC_MAIN");
    assert_eq!(network.facts_devices[0].mode, FactsMode::ShuntOnly);
}

// ---------------------------------------------------------------------------
// Backward compatibility: files without DC/FACTS sections still parse
// ---------------------------------------------------------------------------

#[test]
fn test_psse_without_dc_sections_still_parses() {
    // A minimal v33 file with no DC/FACTS/area interchange sections at all.
    // The three header lines are: case record, title, subtitle.
    let raw = "\
0, 100.00, 33 / PSS/E TEST CASE
MINIMAL TEST
SUBTITLE LINE
1, 'BUS1', 138.0, 3, 1, 1, 1, 1.0, 0.0
2, 'BUS2', 138.0, 1, 1, 1, 1, 1.0, 0.0
0 / END OF BUS DATA, BEGIN LOAD DATA
2, '1', 1, 1, 1, 50.0, 10.0
0 / END OF LOAD DATA, BEGIN FIXED SHUNT DATA
0 / END OF FIXED SHUNT DATA, BEGIN GENERATOR DATA
1, '1', 60.0, 0.0, 9999.0, 0.0, 1.0, 0, 100.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1, 100.0, 9999.0, 0.0
0 / END OF GENERATOR DATA, BEGIN BRANCH DATA
1, 2, '1', 0.01, 0.1, 0.02, 200.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1
0 / END OF BRANCH DATA, BEGIN TRANSFORMER DATA
0 / END OF TRANSFORMER DATA
0 / END OF SWITCHED SHUNT DATA
Q
";

    let network = loads(raw).expect("Minimal PSS/E without DC sections should parse");

    // New fields should be empty (not panic or fail)
    assert!(
        network.hvdc.links.is_empty(),
        "no HVDC links in minimal case"
    );
    assert!(
        network.area_schedules.is_empty(),
        "no area interchange in minimal case"
    );
    assert!(
        network.facts_devices.is_empty(),
        "no FACTS devices in minimal case"
    );

    // Core power flow data should still be correct
    assert_eq!(network.buses.len(), 2, "expected 2 buses");
    assert_eq!(network.branches.len(), 1, "expected 1 branch");
    assert_eq!(network.generators.len(), 1, "expected 1 generator");
}

// ---------------------------------------------------------------------------
// Core network data integrity: DC sections don't corrupt base data
// ---------------------------------------------------------------------------

#[test]
fn test_psse_dc_sections_dont_corrupt_base_network() {
    let sections = "\
0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA
'DC1', 1, 1.0, 100.0, 345.0, 0.0, 0.0, 0.0, 'I', 0.0, 20, 1.0
1, 2, 80.0, 5.0, 0.0, 5.0, 345.0, 1.0, 1.0, 1.1, 0.9, 0.00625, 1, 1, 1, '1', 0.0
3, 2, 80.0, 17.0, 0.0, 5.0, 345.0, 1.0, 1.0, 1.1, 0.9, 0.00625, 1, 1, 1, '1', 0.0
0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA
0 / END OF VSC DC LINE DATA, BEGIN FACTS DEVICE DATA\n";

    let raw = build_psse(sections);
    let network = loads(&raw).expect("should parse");

    // Verify base network is intact
    assert_eq!(network.buses.len(), 3, "3 buses expected");
    assert_eq!(network.branches.len(), 2, "2 branches expected");
    assert_eq!(network.generators.len(), 1, "1 generator expected");

    // Verify DC line was also parsed
    assert_eq!(network.hvdc.links.len(), 1, "1 DC line expected");
}
