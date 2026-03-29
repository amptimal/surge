// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests for the OpenDSS (.dss) parser.
//!
//! All tests use inline DSS strings — no external files required.

use surge_io::dss::loads as parse_dss_str;

fn benchmark_path(rel: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(rel);
    if path.exists() {
        return path.to_path_buf();
    }

    let mut base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    base.pop();
    base.pop();
    base.push(rel);
    base
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: minimal 3-bus circuit
// ─────────────────────────────────────────────────────────────────────────────

/// DSS for a simple source → line → load circuit.
const SIMPLE_DSS: &str = r#"
Clear
New Circuit.test basekv=4.16 pu=1.05 angle=0 frequency=60 phases=3
New Line.L1 Bus1=SourceBus Bus2=650 phases=3 r1=0.1 x1=0.3 length=1 units=mi
New Load.LD1 Bus1=650 phases=3 kv=4.16 kw=1000 kvar=300 model=1
Solve
"#;

#[test]
fn test_dss_simple_circuit() {
    let net = parse_dss_str(SIMPLE_DSS).expect("parse failed");

    // Must have 2 buses: SourceBus and 650.
    assert_eq!(net.n_buses(), 2, "expected 2 buses, got {}", net.n_buses());

    // Must have 1 branch (Line.L1).
    assert_eq!(
        net.n_branches(),
        1,
        "expected 1 branch, got {}",
        net.n_branches()
    );

    // Must have 1 load.
    assert_eq!(
        net.loads.len(),
        1,
        "expected 1 load, got {}",
        net.loads.len()
    );
    let load = &net.loads[0];
    let kw_actual = load.active_power_demand_mw * 1000.0; // MW → kW
    assert!(
        (kw_actual - 1000.0).abs() < 1.0,
        "load kW mismatch: expected 1000, got {:.1}",
        kw_actual
    );

    // Source bus should be slack.
    let slack = net
        .buses
        .iter()
        .find(|b| b.bus_type == surge_network::network::BusType::Slack);
    assert!(slack.is_some(), "no slack bus found");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: LineCode reference
// ─────────────────────────────────────────────────────────────────────────────

const LINECODE_DSS: &str = r#"
Clear
New Circuit.lc_test basekv=12.47
New LineCode.601 nphases=3 r1=0.3465 x1=1.0179 r0=0.6051 x0=1.3294 units=mi
New Line.L1 Bus1=SourceBus Bus2=bus2 linecode=601 length=2 units=mi
New Load.LD1 Bus1=bus2 kv=12.47 kw=400 kvar=130
"#;

#[test]
fn test_dss_linecode_reference() {
    let net = parse_dss_str(LINECODE_DSS).expect("parse failed");

    assert_eq!(net.n_buses(), 2);
    assert_eq!(net.n_branches(), 1);

    let branch = &net.branches[0];
    // r1 = 0.3465 Ω/mi × 2 mi = 0.693 Ω
    // z_base = 12.47² / 100 = 1.5549 Ω
    // r_pu = 0.693 / 1.5549 ≈ 0.4457
    assert!(
        branch.r > 0.0,
        "branch resistance should be positive, got {}",
        branch.r
    );
    assert!(
        branch.x > 0.0,
        "branch reactance should be positive, got {}",
        branch.x
    );
    // The reactance should be substantially larger than resistance (X1/R1 ≈ 2.93).
    assert!(
        branch.x > branch.r,
        "X should exceed R for this cable: r={:.4} x={:.4}",
        branch.r,
        branch.x
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: 2-winding transformer (delta–wye)
// ─────────────────────────────────────────────────────────────────────────────

const TRANSFORMER_DSS: &str = r#"
Clear
New Circuit.xfmr_test basekv=115
New Transformer.T1 phases=3 windings=2
~ buses=[HV_bus LV_bus]
~ conns=[delta wye]
~ kvs=[115 4.16]
~ kvas=[5000 5000]
~ %rs=[1 1]
~ xhl=7
New Load.LD1 Bus1=LV_bus kv=4.16 kw=3000 kvar=1000
"#;

#[test]
fn test_dss_transformer_twowinding() {
    let net = parse_dss_str(TRANSFORMER_DSS).expect("parse failed");

    // 2 buses (HV_bus, LV_bus) + circuit source (HV_bus IS source bus).
    assert!(net.n_buses() >= 2, "expected at least 2 buses");
    assert_eq!(net.n_branches(), 1, "expected 1 transformer branch");

    let branch = &net.branches[0];
    // Delta-Wye → connection type should reflect delta primary.
    assert_ne!(
        branch
            .transformer_data
            .as_ref()
            .map(|t| t.transformer_connection)
            .unwrap_or_default(),
        surge_network::network::TransformerConnection::WyeGWyeG,
        "expected non-WyeGWyeG connection for delta-wye"
    );

    // Reactance should be non-zero.
    assert!(branch.x > 0.0, "transformer reactance must be positive");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: Redirect from a temp file
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_dss_redirect_inline() {
    use std::io::Write;

    // Write a secondary DSS file with a load definition.
    let tmp_dir = std::env::temp_dir();
    let load_file = tmp_dir.join("surge_test_load.dss");
    {
        let mut f = std::fs::File::create(&load_file).expect("create tmp file");
        writeln!(f, "New Load.LD2 Bus1=bus2 kv=12.47 kw=500 kvar=200").unwrap();
    }

    let main_dss = format!(
        r#"
Clear
New Circuit.redirect_test basekv=12.47
New Line.L1 Bus1=SourceBus Bus2=bus2 r1=0.1 x1=0.3 length=1 units=mi
Redirect {}
Solve
"#,
        load_file.to_string_lossy()
    );

    // Parse from the temp directory so Redirect can resolve the path.
    let tmp_main = tmp_dir.join("surge_test_main.dss");
    std::fs::write(&tmp_main, &main_dss).expect("write main dss");

    let net = surge_io::dss::load(&tmp_main).expect("parse failed");

    assert_eq!(
        net.loads.len(),
        1,
        "should have 1 load from redirected file"
    );
    assert_eq!(net.n_buses(), 2);

    // Cleanup.
    let _ = std::fs::remove_file(&load_file);
    let _ = std::fs::remove_file(&tmp_main);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 5: tilde continuation
// ─────────────────────────────────────────────────────────────────────────────

const TILDE_DSS: &str = r#"
Clear
New Circuit.tilde_test basekv=12.47
New Line.L1 Bus1=SourceBus Bus2=bus2
~ phases=3
~ r1=0.2
~ x1=0.8
~ length=0.5
~ units=mi
New Load.LD1 Bus1=bus2 kv=12.47 kw=200 kvar=60
"#;

#[test]
fn test_dss_tilde_continuation() {
    let net = parse_dss_str(TILDE_DSS).expect("parse failed");

    assert_eq!(net.n_branches(), 1, "expected 1 branch");

    let branch = &net.branches[0];
    // Verify that the continuation properties were applied.
    assert!(
        branch.r > 0.0,
        "r should be positive after continuation, got {}",
        branch.r
    );
    assert!(
        branch.x > branch.r,
        "x should exceed r, got r={:.4} x={:.4}",
        branch.r,
        branch.x
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 6: IEEE 13-bus feeder structure
//
// The IEEE 13-bus feeder is the standard distribution system test case.
// Reference: W. H. Kersting, "Radial distribution test feeders," 2001.
//
// Key data:
// - Substation at bus 650, source voltage = 2.4009/sqrt(3) = 115/4.16 area
//   (the 13-bus feeder uses 4.16 kV nominal)
// - 13 buses total
// - Mix of spot loads and distributed loads
// ─────────────────────────────────────────────────────────────────────────────

const IEEE13_DSS: &str = r#"
Clear
New Circuit.ieee13 basekv=115 pu=1.0001 phases=3 bus1=SourceBus

! Substation transformer 115 kV -> 4.16 kV
New Transformer.Sub phases=3 windings=2
~ buses=[SourceBus, 650]
~ conns=[delta, wye]
~ kvs=[115, 4.16]
~ kvas=[5000, 5000]
~ %rs=[1, 1]
~ xhl=8

! Voltage regulator (modelled as ideal transformer with tap)
New Transformer.Reg1 phases=3 windings=2
~ buses=[650, rg60]
~ conns=[wye, wye]
~ kvs=[2.4019, 2.4019]
~ kvas=[1666, 1666]
~ %rs=[0.01, 0.01]
~ xhl=0.01

! Line configurations (positive-sequence from IEEE 13-bus feeder data)
New LineCode.601 nphases=3 r1=0.3465 x1=1.0179 r0=0.6051 x0=1.3294 units=mi
New LineCode.602 nphases=3 r1=0.7526 x1=1.1765 r0=1.3294 x0=1.3294 units=mi
New LineCode.603 nphases=2 r1=1.3294 x1=1.3471 r0=1.3294 x0=1.3471 units=mi
New LineCode.604 nphases=2 r1=1.3294 x1=1.3471 r0=1.3294 x0=1.3471 units=mi
New LineCode.605 nphases=1 r1=1.3292 x1=1.3475 r0=1.3292 x0=1.3475 units=mi
New LineCode.606 nphases=3 r1=0.7982 x1=0.4463 r0=0.7982 x0=0.4463 units=mi
New LineCode.607 nphases=1 r1=1.3425 x1=0.5124 r0=1.3425 x0=0.5124 units=mi

! Distribution lines
New Line.L650_632  Bus1=rg60    Bus2=632   linecode=601 length=0.3788 units=mi
New Line.L632_670  Bus1=632     Bus2=670   linecode=601 length=0.1667 units=mi
New Line.L670_671  Bus1=670     Bus2=671   linecode=601 length=0.1250 units=mi
New Line.L671_680  Bus1=671     Bus2=680   linecode=601 length=0.1000 units=mi
New Line.L632_633  Bus1=632     Bus2=633   linecode=602 length=0.1667 units=mi
New Line.L633_634  Bus1=633     Bus2=634   linecode=602 length=0.0500 units=mi
New Line.L671_684  Bus1=671     Bus2=684   linecode=604 length=0.3000 units=mi
New Line.L684_611  Bus1=684     Bus2=611   linecode=605 length=0.3000 units=mi
New Line.L684_652  Bus1=684     Bus2=652   linecode=607 length=0.8000 units=mi
New Line.L692_675  Bus1=692     Bus2=675   linecode=606 length=0.5000 units=mi
New Line.L671_692  Bus1=671     Bus2=692   linecode=601 length=0.0010 units=mi

! Spot loads (kW, kVAr) — from IEEE 13-bus feeder specification
New Load.S634a  Bus1=634  phases=1 kv=0.277 kw=160  kvar=110 model=1
New Load.S634b  Bus1=634  phases=1 kv=0.277 kw=120  kvar=90  model=1
New Load.S634c  Bus1=634  phases=1 kv=0.277 kw=120  kvar=90  model=1
New Load.S645   Bus1=645  phases=1 kv=2.4   kw=170  kvar=125 model=1
New Load.S646   Bus1=646  phases=1 kv=2.4   kw=230  kvar=132 model=2
New Load.S652   Bus1=652  phases=1 kv=2.4   kw=128  kvar=86  model=2
New Load.S671a  Bus1=671  phases=1 kv=2.4   kw=385  kvar=220 model=1
New Load.S671b  Bus1=671  phases=1 kv=2.4   kw=385  kvar=220 model=1
New Load.S671c  Bus1=671  phases=1 kv=2.4   kw=385  kvar=220 model=1
New Load.S675a  Bus1=675  phases=1 kv=2.4   kw=485  kvar=190 model=1
New Load.S675b  Bus1=675  phases=1 kv=2.4   kw=68   kvar=60  model=1
New Load.S675c  Bus1=675  phases=1 kv=2.4   kw=290  kvar=212 model=1
New Load.S692   Bus1=692  phases=1 kv=2.4   kw=170  kvar=151 model=5
New Load.S611   Bus1=611  phases=1 kv=2.4   kw=170  kvar=80  model=5
New Load.D671   Bus1=671  phases=1 kv=2.4   kw=100  kvar=0   model=1

! Shunt capacitor bank
New Capacitor.Cap1 Bus1=675 phases=3 kvar=600 kv=4.16
New Capacitor.Cap2 Bus1=611 phases=1 kvar=100 kv=2.4

Solve
"#;

#[test]
fn test_dss_ieee13_structure() {
    let net = parse_dss_str(IEEE13_DSS).expect("IEEE 13-bus parse failed");

    // Count buses: SourceBus, 650, rg60, 632, 670, 671, 680, 633, 634, 684, 611, 652, 692, 675, 645, 646 = 16
    // (The feeder has 13 load buses + source + 650 + rg60 = 16 total nodes)
    // Depending on load buses referenced, we expect >= 13.
    let n_buses = net.n_buses();
    assert!(
        n_buses >= 13,
        "expected >= 13 buses for IEEE 13-bus feeder, got {}",
        n_buses
    );

    // Lines: 11 distribution lines + 2 transformers.
    let n_branches = net.n_branches();
    assert!(
        n_branches >= 11,
        "expected >= 11 branches, got {}",
        n_branches
    );

    // Loads: 15 spot loads + 1 distributed.
    let n_loads = net.loads.len();
    assert!(n_loads >= 10, "expected >= 10 loads, got {}", n_loads);

    // Total load ≈ 3500 kW (from IEEE 13-bus feeder specification).
    let total_kw: f64 = net
        .loads
        .iter()
        .map(|l| l.active_power_demand_mw * 1000.0)
        .sum();
    assert!(
        total_kw > 2500.0 && total_kw < 5000.0,
        "total load should be ~3500 kW, got {:.0} kW",
        total_kw
    );

    // Slack bus must exist.
    let has_slack = net
        .buses
        .iter()
        .any(|b| b.bus_type == surge_network::network::BusType::Slack);
    assert!(has_slack, "no slack bus found in IEEE 13-bus network");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 7: Capacitor converts to shunt susceptance
// ─────────────────────────────────────────────────────────────────────────────

const CAPACITOR_DSS: &str = r#"
Clear
New Circuit.cap_test basekv=12.47
New Line.L1 Bus1=SourceBus Bus2=bus1 r1=0.1 x1=0.3 length=1 units=mi
New Capacitor.Cap1 Bus1=bus1 phases=3 kvar=600 kv=12.47
"#;

#[test]
fn test_dss_capacitor_shunt() {
    let net = parse_dss_str(CAPACITOR_DSS).expect("parse failed");
    assert_eq!(net.n_buses(), 2);

    let bus1 = net.buses.iter().find(|b| b.name == "bus1");
    assert!(bus1.is_some(), "bus1 not found");
    let bus1 = bus1.unwrap();
    assert!(
        (bus1.shunt_susceptance_mvar - 0.6).abs() < 1e-9,
        "600 kvar capacitor should import as 0.6 MVAr, got {}",
        bus1.shunt_susceptance_mvar
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 8: Generator creates PV bus
// ─────────────────────────────────────────────────────────────────────────────

const GENERATOR_DSS: &str = r#"
Clear
New Circuit.gen_test basekv=12.47
New Line.L1 Bus1=SourceBus Bus2=genbus r1=0.1 x1=0.3 length=1 units=mi
New Generator.G1 Bus1=genbus phases=3 kv=12.47 kw=500 kvar=200 kva=625
"#;

#[test]
fn test_dss_generator_pv_bus() {
    let net = parse_dss_str(GENERATOR_DSS).expect("parse failed");
    assert_eq!(net.generators.len(), 1, "expected 1 generator");

    let generator = &net.generators[0];
    let pg_kw = generator.p * 1000.0;
    assert!(
        (pg_kw - 500.0).abs() < 1.0,
        "generator output should be 500 kW, got {:.1}",
        pg_kw
    );

    // The bus where the generator is connected should be PV.
    let genbus = net.buses.iter().find(|b| b.number == generator.bus);
    assert!(genbus.is_some(), "generator bus not found");
    assert_eq!(
        genbus.unwrap().bus_type,
        surge_network::network::BusType::PV,
        "generator bus should be PV type"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 9: Unknown object type is skipped gracefully
// ─────────────────────────────────────────────────────────────────────────────

const UNKNOWN_TYPE_DSS: &str = r#"
Clear
New Circuit.unk_test basekv=12.47
New Line.L1 Bus1=SourceBus Bus2=bus2 r1=0.1 x1=0.3 length=1 units=mi
New UnknownElement.XX1 some_prop=some_val
New Load.LD1 Bus1=bus2 kv=12.47 kw=100 kvar=30
"#;

#[test]
fn test_dss_unknown_type_graceful() {
    // Should not panic or error — unknown types are warned and skipped.
    let result = parse_dss_str(UNKNOWN_TYPE_DSS);
    assert!(result.is_ok(), "should not error on unknown element type");
    let net = result.unwrap();
    assert_eq!(net.loads.len(), 1, "load should still be parsed");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 10: Matrix impedance line
// ─────────────────────────────────────────────────────────────────────────────

const MATRIX_LINE_DSS: &str = r#"
Clear
New Circuit.matrix_test basekv=12.47
New Line.L1 Bus1=SourceBus Bus2=bus2 phases=3 length=1 units=mi
~ rmatrix=[0.3465 0.1560 0.1580 0.3375 0.1535 0.3414]
~ xmatrix=[1.0179 0.5017 0.4236 1.0478 0.3849 1.0348]
New Load.LD1 Bus1=bus2 kv=12.47 kw=200 kvar=60
"#;

#[test]
fn test_dss_matrix_line() {
    let net = parse_dss_str(MATRIX_LINE_DSS).expect("parse failed");
    assert_eq!(net.n_branches(), 1);
    let branch = &net.branches[0];
    assert!(
        branch.r > 0.0 && branch.x > 0.0,
        "matrix line should yield positive R and X, got r={} x={}",
        branch.r,
        branch.x
    );
}

// Temporary debug test to inspect DSS network structure
#[test]
fn debug_ieee13_base_kv() {
    let path = benchmark_path("benchmarks/instances/dss/ieee13/IEEE13Nodeckt.dss");
    if !path.exists() {
        return;
    }
    let net = surge_io::dss::load(&path).expect("parse");
    println!("base_mva: {}", net.base_mva);
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    for (i, b) in net.buses.iter().enumerate() {
        println!(
            "  Bus {:4} name={:15} type={:?} base_kv={:.4} pd={:.4} qd={:.4}",
            b.number, b.name, b.bus_type, b.base_kv, bus_pd[i], bus_qd[i]
        );
    }
    for br in &net.branches {
        let from_bus = net.buses.iter().find(|b| b.number == br.from_bus);
        let from_kv = from_bus.map(|b| b.base_kv).unwrap_or(0.0);
        let z_base = from_kv * from_kv / net.base_mva;
        println!(
            "  br {}->{} r_pu={:.6} x_pu={:.6} r_ohm={:.4} x_ohm={:.4} from_kv={:.3}",
            br.from_bus,
            br.to_bus,
            br.r,
            br.x,
            br.r * z_base,
            br.x * z_base,
            from_kv
        );
    }
}

// Debug test for IEEE-34 base kV and impedance values
#[test]
fn debug_ieee34_base_kv() {
    let path = benchmark_path("benchmarks/instances/dss/ieee34/ieee34Mod1.dss");
    if !path.exists() {
        return;
    }
    let net = surge_io::dss::load(&path).expect("parse ieee34");
    println!("base_mva: {}", net.base_mva);
    println!("n_buses: {}", net.buses.len());
    let bus_pd = net.bus_load_p_mw();
    let bus_qd = net.bus_load_q_mvar();
    for (i, b) in net.buses.iter().enumerate().take(5) {
        println!(
            "  Bus {:4} name={:15} type={:?} base_kv={:.4} pd={:.4} qd={:.4}",
            b.number, b.name, b.bus_type, b.base_kv, bus_pd[i], bus_qd[i]
        );
    }
    println!("n_branches: {}", net.branches.len());
    for br in net.branches.iter().take(5) {
        let from_bus = net.buses.iter().find(|b| b.number == br.from_bus);
        let from_kv = from_bus.map(|b| b.base_kv).unwrap_or(0.0);
        let z_base = from_kv * from_kv / net.base_mva;
        println!(
            "  br {}->{} r_pu={:.6} r_ohm={:.4} from_kv={:.3}",
            br.from_bus,
            br.to_bus,
            br.r,
            br.r * z_base,
            from_kv
        );
    }
}

// Debug transformer kV propagation for IEEE-34
#[test]
fn debug_ieee34_zone_kv() {
    let path = benchmark_path("benchmarks/instances/dss/ieee34/ieee34Mod1.dss");
    if !path.exists() {
        return;
    }

    // Parse DSS to get raw catalog, then manually check what zone_kv should be
    // We use the public API to check the resulting network
    let net = surge_io::dss::load(&path).expect("parse ieee34");

    // Find all buses and their base_kv
    let mut kv_freq: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for b in &net.buses {
        let key = format!("{:.2}", b.base_kv);
        *kv_freq.entry(key).or_insert(0) += 1;
    }
    println!("base_kv frequency distribution:");
    let mut kv_vec: Vec<_> = kv_freq.iter().collect();
    kv_vec.sort_by_key(|(k, _)| k.parse::<f64>().unwrap_or(0.0) as i64);
    for (kv, cnt) in &kv_vec {
        println!("  {}: {} buses", kv, cnt);
    }

    // Find specific buses
    for name in &["800", "802", "808", "814", "814r", "888", "890"] {
        if let Some(b) = net.buses.iter().find(|b| b.name == *name) {
            println!("  Bus {:6} base_kv={:.4}", name, b.base_kv);
        }
    }
}
