// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Built-in transmission test networks exposed as Python functions.
//!
//! Standard IEEE/MATPOWER benchmark cases are embedded at compile time via
//! `include_bytes!()` so users can begin solving immediately without needing
//! case files on disk.
//!
//! # Usage
//!
//! ```python
//! import surge
//!
//! net = surge.case9()       # IEEE 9-bus (WSCC 3-generator system)
//! sol = surge.solve_ac_pf(net)
//! print(sol.converged, sol.max_mismatch)
//! ```
//!
//! # Available cases
//!
//! | Function | Buses | Branches | Generators | Source |
//! |----------|-------|----------|-----------|--------|
//! | `case9()` | 9 | 9 | 3 | WSCC 9-bus (Anderson & Fouad) |
//! | `case14()` | 14 | 20 | 5 | IEEE 14-bus |
//! | `case30()` | 30 | 41 | 6 | IEEE 30-bus |
//! | `market30()` | 30 | 41 | 10 | Custom market-enabled IEEE 30-bus derivative |
//! | `case57()` | 57 | 80 | 7 | IEEE 57-bus |
//! | `case118()` | 118 | 186 | 54 | IEEE 118-bus |
//! | `case300()` | 300 | 411 | 69 | IEEE 300-bus |

use pyo3::prelude::*;

use crate::Network;

// Embedded zstd-compressed surge-json case files.
const CASE9_ZST: &[u8] = include_bytes!("../../../examples/cases/case9/case9.surge.json.zst");
const CASE14_ZST: &[u8] = include_bytes!("../../../examples/cases/case14/case14.surge.json.zst");
const CASE30_ZST: &[u8] = include_bytes!("../../../examples/cases/case30/case30.surge.json.zst");
const MARKET30_ZST: &[u8] =
    include_bytes!("../../../examples/cases/market30/market30.surge.json.zst");
const CASE57_ZST: &[u8] = include_bytes!("../../../examples/cases/case57/case57.surge.json.zst");
const CASE118_ZST: &[u8] = include_bytes!("../../../examples/cases/ieee118/case118.surge.json.zst");
const CASE300_ZST: &[u8] = include_bytes!("../../../examples/cases/case300/case300.surge.json.zst");

fn parse_embedded_zst(zst_bytes: &[u8], name: &str) -> PyResult<Network> {
    use std::sync::Arc;
    let json_bytes = zstd::decode_all(zst_bytes).map_err(|e| {
        crate::exceptions::SurgeError::new_err(format!("Failed to decompress {name}: {e}"))
    })?;
    let json_str = std::str::from_utf8(&json_bytes).map_err(|e| {
        crate::exceptions::SurgeError::new_err(format!("Invalid UTF-8 in {name}: {e}"))
    })?;
    let mut net = surge_io::json::loads(json_str).map_err(|e| {
        crate::exceptions::SurgeError::new_err(format!("Failed to load {name}: {e}"))
    })?;
    // Ensure every generator / load / branch has a canonical resource id
    // so downstream market code can rely on ``Generator.resource_id``
    // without needing to canonicalise by hand.
    net.canonicalize_runtime_identities();
    Ok(Network {
        inner: Arc::new(net),
        oltc_controls: Vec::new(),
        switched_shunts: Vec::new(),
    })
}

/// IEEE 9-bus (WSCC 3-machine system).
///
/// The classic Anderson & Fouad 9-bus test system with 3 generators.
/// Commonly used for power systems textbook examples and stability studies.
///
/// Returns:
///   Network: 9 buses, 9 branches, 3 generators, base_mva=100.
#[pyfunction]
pub fn case9() -> PyResult<Network> {
    parse_embedded_zst(CASE9_ZST, "case9")
}

/// IEEE 14-bus system.
///
/// The IEEE 14-bus test system with 5 generators and 11 load buses.
/// Frequently used as a small benchmark for power flow and OPF validation.
///
/// Returns:
///   Network: 14 buses, 20 branches, 5 generators, base_mva=100.
#[pyfunction]
pub fn case14() -> PyResult<Network> {
    parse_embedded_zst(CASE14_ZST, "case14")
}

/// IEEE 30-bus system.
///
/// The IEEE 30-bus test system with 6 generators. Covers a wider range of
/// voltage levels (132 kV and 33 kV) and includes transformer taps.
///
/// Returns:
///   Network: 30 buses, 41 branches, 6 generators, base_mva=100.
#[pyfunction]
pub fn case30() -> PyResult<Network> {
    parse_embedded_zst(CASE30_ZST, "case30")
}

/// Market-enabled 30-bus example.
///
/// A custom IEEE 30-bus derivative with thermal fleet diversity, storage,
/// dispatchable load, HVDC, interfaces, and flowgates for dispatch testing.
///
/// Returns:
///   Network: 30 buses, 41 branches, 10 generators, base_mva=100.
#[pyfunction]
pub fn market30() -> PyResult<Network> {
    parse_embedded_zst(MARKET30_ZST, "market30")
}

/// IEEE 57-bus system.
///
/// The IEEE 57-bus test system with 7 generators. A medium-sized case
/// often used to benchmark AC power flow convergence speed.
///
/// Returns:
///   Network: 57 buses, 80 branches, 7 generators, base_mva=100.
#[pyfunction]
pub fn case57() -> PyResult<Network> {
    parse_embedded_zst(CASE57_ZST, "case57")
}

/// IEEE 118-bus system.
///
/// The IEEE 118-bus test system with 54 generators. One of the most widely
/// cited power flow and OPF benchmarks in the literature.
///
/// Returns:
///   Network: 118 buses, 186 branches, 54 generators, base_mva=100.
#[pyfunction]
pub fn case118() -> PyResult<Network> {
    parse_embedded_zst(CASE118_ZST, "case118")
}

/// IEEE 300-bus system.
///
/// The IEEE 300-bus test system with 69 generators. A larger benchmark
/// for evaluating solver performance on multi-area networks.
///
/// Returns:
///   Network: 300 buses, 411 branches, 69 generators, base_mva=100.
#[pyfunction]
pub fn case300() -> PyResult<Network> {
    parse_embedded_zst(CASE300_ZST, "case300")
}
