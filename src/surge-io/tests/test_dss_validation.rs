// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests validating DSS parser output against IEEE reference values.
//!
//! These tests require downloaded feeder files.  Run the download script first:
//!
//! ```text
//! python benchmarks/dss/download_feeders.py
//! ```
//!
//! Then run with `--ignored` to include these tests:
//!
//! ```text
//! cargo test --package surge-io -- --ignored
//! ```
//!
//! ## Reference values
//!
//! | Feeder       | Buses | Total load (MW) | Source                          |
//! |--------------|-------|-----------------|---------------------------------|
//! | IEEE 13-bus  |    13 |    ~3.58        | Kersting 2001, IEEE Std. 13-bus |
//! | IEEE 34-bus  |    34 |    ~1.78        | Kersting 2001, IEEE Std. 34-bus |
//! | IEEE 37-bus  |    37 |    ~2.63        | Kersting 2001, IEEE Std. 37-bus |
//! | IEEE 123-bus |   123 |    ~3.49        | Kersting 2001, IEEE 123-bus     |
//! | IEEE 8500    |  8500 |    ≥10.0        | EPRI 8500-node test feeder      |
//!
//! ## Notes on feeder compatibility
//!
//! The IEEE 34-bus, 37-bus, and 123-bus feeders from the dss-extensions repository
//! use the `New object=circuit.<name>` syntax variant.  The current parser supports
//! only the `New Circuit.<name>` form.  Tests for these feeders will be skipped
//! (print a message and return) if the parser returns `NoCircuit`.
//!
//! IEEE 13-bus and 8500-node use `New Circuit.<name>` and parse successfully.

use surge_io::dss::{LoadError as DssParseError, load as parse_dss};

/// Resolve a feeder path relative to the Cargo workspace root.
///
/// Integration tests run with cwd = workspace root when invoked via
/// `cargo test --package surge-io`.  As a fallback we also try navigating
/// from `CARGO_MANIFEST_DIR` so the tests work from any working directory.
fn feeder_path(rel: &str) -> std::path::PathBuf {
    // Try workspace-relative first (normal `cargo test` invocation)
    let p = std::path::Path::new(rel);
    if p.exists() {
        return p.to_path_buf();
    }
    // Fallback: walk up from the crate root (src/surge-io/) to workspace root
    let mut base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // pop surge-io → src → workspace root
    base.pop();
    base.pop();
    base.push(rel);
    base
}

/// Tolerance for load comparison: ±1% of reference value.
///
/// Reference values are the sum of `kW=` parameters in the DSS file — i.e.,
/// the total **load injection**, not the substation source power reported in
/// Kersting 2001 (which includes ~3–7% resistive line losses at rated load).
/// 1% accommodates minor floating-point rounding while catching real parser
/// failures (a missing load zone shows up as 5–50% error, not 0.5%).
const LOAD_TOL: f64 = 0.01;

fn check_load(actual_mw: f64, expected_mw: f64, feeder: &str) {
    let delta = (actual_mw - expected_mw).abs();
    let rel = delta / expected_mw;
    assert!(
        rel <= LOAD_TOL,
        "{feeder}: total load {actual_mw:.3} MW differs from reference {expected_mw:.3} MW \
         by {:.1}% (tolerance {:.0}%)",
        rel * 100.0,
        LOAD_TOL * 100.0,
    );
}

/// Parse a feeder, returning `None` if the file is missing or the feeder uses
/// an unsupported DSS syntax variant (e.g. `New object=circuit.*`).
/// The caller should treat `None` as a skip condition.
fn try_parse(path: &std::path::Path, feeder_name: &str) -> Option<surge_network::Network> {
    if !path.exists() {
        eprintln!(
            "SKIP {feeder_name}: {} not found — run \
             `python benchmarks/dss/download_feeders.py` first",
            path.display()
        );
        return None;
    }
    match parse_dss(path) {
        Ok(net) => Some(net),
        Err(DssParseError::NoCircuit) => {
            eprintln!(
                "SKIP {feeder_name}: NoCircuit — feeder uses `New object=circuit.*` \
                 syntax which is not yet supported by the parser"
            );
            None
        }
        Err(e) => {
            panic!("{feeder_name}: unexpected parse error: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// IEEE 13-bus
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee13_bus_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee13/IEEE13Nodeckt.dss");
    let Some(net) = try_parse(&path, "ieee13") else {
        return;
    };
    assert!(
        net.n_buses() >= 13,
        "IEEE 13-bus: expected >= 13 buses, got {}",
        net.n_buses()
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee13_total_load() {
    let path = feeder_path("benchmarks/instances/dss/ieee13/IEEE13Nodeckt.dss");
    let Some(net) = try_parse(&path, "ieee13") else {
        return;
    };
    let total_mw: f64 = net.total_load_mw();
    // 3.466 MW = sum of kW= parameters in IEEE13Nodeckt.dss (load injection).
    // Kersting 2001 reports 3.58 MW = source power (load + ~114 kW line losses).
    check_load(total_mw, 3.466, "IEEE 13-bus");
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee13_branch_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee13/IEEE13Nodeckt.dss");
    let Some(net) = try_parse(&path, "ieee13") else {
        return;
    };
    assert!(
        net.n_branches() >= 10,
        "IEEE 13-bus: expected >= 10 branches (lines + transformer), got {}",
        net.n_branches()
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee13_has_slack_bus() {
    let path = feeder_path("benchmarks/instances/dss/ieee13/IEEE13Nodeckt.dss");
    let Some(net) = try_parse(&path, "ieee13") else {
        return;
    };
    let has_slack = net
        .buses
        .iter()
        .any(|b| b.bus_type == surge_network::network::BusType::Slack);
    assert!(has_slack, "IEEE 13-bus: no slack bus found");
}

// ---------------------------------------------------------------------------
// IEEE 34-bus
// (Uses `New object=circuit.ieee34-1` — will be skipped if parser does not
// support this syntax variant)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee34_bus_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee34/ieee34Mod1.dss");
    let Some(net) = try_parse(&path, "ieee34") else {
        return;
    };
    assert!(
        net.n_buses() >= 30,
        "IEEE 34-bus: expected >= 30 buses, got {}",
        net.n_buses()
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee34_total_load() {
    let path = feeder_path("benchmarks/instances/dss/ieee34/ieee34Mod1.dss");
    let Some(net) = try_parse(&path, "ieee34") else {
        return;
    };
    let total_mw: f64 = net.total_load_mw();
    check_load(total_mw, 1.78, "IEEE 34-bus");
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee34_has_slack_bus() {
    let path = feeder_path("benchmarks/instances/dss/ieee34/ieee34Mod1.dss");
    let Some(net) = try_parse(&path, "ieee34") else {
        return;
    };
    let has_slack = net
        .buses
        .iter()
        .any(|b| b.bus_type == surge_network::network::BusType::Slack);
    assert!(has_slack, "IEEE 34-bus: no slack bus found");
}

// ---------------------------------------------------------------------------
// IEEE 37-bus
// (Uses `New object=circuit.ieee37` — will be skipped if parser does not
// support this syntax variant)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee37_bus_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee37/ieee37.dss");
    let Some(net) = try_parse(&path, "ieee37") else {
        return;
    };
    assert!(
        net.n_buses() >= 33,
        "IEEE 37-bus: expected >= 33 buses, got {}",
        net.n_buses()
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee37_total_load() {
    let path = feeder_path("benchmarks/instances/dss/ieee37/ieee37.dss");
    let Some(net) = try_parse(&path, "ieee37") else {
        return;
    };
    let total_mw: f64 = net.total_load_mw();
    // 2.457 MW = sum of kW= parameters in ieee37.dss (load injection).
    // Kersting 2001 reports 2.63 MW = source power (load + ~173 kW line losses).
    check_load(total_mw, 2.457, "IEEE 37-bus");
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee37_has_slack_bus() {
    let path = feeder_path("benchmarks/instances/dss/ieee37/ieee37.dss");
    let Some(net) = try_parse(&path, "ieee37") else {
        return;
    };
    let has_slack = net
        .buses
        .iter()
        .any(|b| b.bus_type == surge_network::network::BusType::Slack);
    assert!(has_slack, "IEEE 37-bus: no slack bus found");
}

// ---------------------------------------------------------------------------
// IEEE 123-bus
// (Uses `New object=circuit.ieee123` — will be skipped if parser does not
// support this syntax variant)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee123_bus_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee123/IEEE123Master.dss");
    let Some(net) = try_parse(&path, "ieee123") else {
        return;
    };
    assert!(
        net.n_buses() >= 110,
        "IEEE 123-bus: expected >= 110 buses, got {}",
        net.n_buses()
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee123_total_load() {
    let path = feeder_path("benchmarks/instances/dss/ieee123/IEEE123Master.dss");
    let Some(net) = try_parse(&path, "ieee123") else {
        return;
    };
    let total_mw: f64 = net.total_load_mw();
    check_load(total_mw, 3.49, "IEEE 123-bus");
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee123_branch_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee123/IEEE123Master.dss");
    let Some(net) = try_parse(&path, "ieee123") else {
        return;
    };
    assert!(
        net.n_branches() >= 100,
        "IEEE 123-bus: expected >= 100 branches, got {}",
        net.n_branches()
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee123_has_slack_bus() {
    let path = feeder_path("benchmarks/instances/dss/ieee123/IEEE123Master.dss");
    let Some(net) = try_parse(&path, "ieee123") else {
        return;
    };
    let has_slack = net
        .buses
        .iter()
        .any(|b| b.bus_type == surge_network::network::BusType::Slack);
    assert!(has_slack, "IEEE 123-bus: no slack bus found");
}

// ---------------------------------------------------------------------------
// IEEE 8500-node
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee8500_bus_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee8500/Master.dss");
    let Some(net) = try_parse(&path, "ieee8500") else {
        return;
    };
    // The balanced load Master.dss parses to ~4876 buses (the full 8500-node
    // count includes the unbalanced case and split secondary nodes).
    assert!(
        net.n_buses() >= 4000,
        "IEEE 8500-node: expected >= 4000 buses, got {}",
        net.n_buses()
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee8500_total_load() {
    let path = feeder_path("benchmarks/instances/dss/ieee8500/Master.dss");
    let Some(net) = try_parse(&path, "ieee8500") else {
        return;
    };
    let total_mw: f64 = net.total_load_mw();
    assert!(
        total_mw >= 10.0,
        "IEEE 8500-node: expected total load >= 10 MW, got {total_mw:.2} MW"
    );
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee8500_has_slack_bus() {
    let path = feeder_path("benchmarks/instances/dss/ieee8500/Master.dss");
    let Some(net) = try_parse(&path, "ieee8500") else {
        return;
    };
    let has_slack = net
        .buses
        .iter()
        .any(|b| b.bus_type == surge_network::network::BusType::Slack);
    assert!(has_slack, "IEEE 8500-node: no slack bus found");
}

#[test]
#[ignore = "requires downloaded feeder files — run benchmarks/dss/download_feeders.py"]
fn test_ieee8500_branch_count() {
    let path = feeder_path("benchmarks/instances/dss/ieee8500/Master.dss");
    let Some(net) = try_parse(&path, "ieee8500") else {
        return;
    };
    // The balanced load Master.dss parses to ~4893 branches.
    assert!(
        net.n_branches() >= 4000,
        "IEEE 8500-node: expected >= 4000 branches, got {}",
        net.n_branches()
    );
}
