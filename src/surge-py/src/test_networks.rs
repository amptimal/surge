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
///   **Branch thermal ratings: present on all 9 branches** (150 - 300 MVA).
///   Suitable for NERC ATC / transfer-capability studies.
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
///   **Branch thermal ratings: NOT set** (rate_a_mva = 0 on all branches) —
///   the original IEEE 14-bus dataset has no thermal ratings. Use
///   ``case9``, ``case30``, or ``market30`` for transfer-capability
///   studies that need real thermal limits.
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
///   **Branch thermal ratings: present on all 41 branches** (16 - 130 MVA).
///   Good default for ATC / OPF studies that need realistic thermal limits.
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
///   **Branch thermal ratings: present on all 41 branches** (64 - 520 MVA).
///   The richest built-in case: includes storage (battery + pumped hydro),
///   one HVDC link, and three operating areas. Best for SCED / SCUC /
///   ATC workflows.
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
///   **Branch thermal ratings: NOT set** (rate_a_mva = 0 on all branches).
///   OK for power-flow / OPF studies that don't bind thermal limits, but
///   transfer-capability studies will return "unconstrained". Add ratings
///   manually via ``Network.set_branch_rating`` if needed.
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
///   **Branch thermal ratings: NOT set** (rate_a_mva = 0 on all branches) —
///   the canonical IEEE 118-bus dataset ships without thermal ratings.
///   Transfer-capability (``compute_nerc_atc`` / ``compute_ac_atc``) and
///   thermal-based contingency metrics will degrade to "unconstrained" on
///   this case. Use ``market30`` when a thermally-rated case is needed.
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
///   **Branch thermal ratings: NOT set** (rate_a_mva = 0 on all branches).
///   Same caveat as case118: transfer-capability studies will return
///   "unconstrained". Useful for solver-speed and scaling tests where
///   thermal binding isn't important.
#[pyfunction]
pub fn case300() -> PyResult<Network> {
    parse_embedded_zst(CASE300_ZST, "case300")
}

/// Return the names of every built-in case available via
/// :func:`load_builtin_case`.
///
/// The list is stable and sorted by network size ascending so that
/// callers iterating in order start from the smallest case.
///
/// See :func:`builtin_case_rated_flags` for a quick-lookup of which
/// cases have realistic branch thermal ratings.
#[pyfunction]
pub fn list_builtin_cases() -> Vec<&'static str> {
    vec![
        "case9", "case14", "case30", "market30", "case57", "case118", "case300",
    ]
}

/// Report which built-in cases ship with branch thermal ratings.
///
/// Returns a list of ``(name, has_ratings)`` tuples. Cases without
/// ratings will see transfer-capability tools (``compute_nerc_atc``,
/// ``compute_ac_atc``) return "unconstrained" — use a rated case for
/// those workflows.
///
/// As of v0.1.5 the rated cases are ``case9``, ``case30``, and
/// ``market30``; the unrated cases (per their upstream IEEE datasets)
/// are ``case14``, ``case57``, ``case118``, and ``case300``.
#[pyfunction]
pub fn builtin_case_rated_flags() -> Vec<(&'static str, bool)> {
    vec![
        ("case9", true),
        ("case14", false),
        ("case30", true),
        ("market30", true),
        ("case57", false),
        ("case118", false),
        ("case300", false),
    ]
}

/// Load a built-in case by name.
///
/// Args:
///     name: One of the values returned by :func:`list_builtin_cases`
///         (``"case9"``, ``"case14"``, ``"case30"``, ``"market30"``,
///         ``"case57"``, ``"case118"``, ``"case300"``).
///
/// Note:
///     Not every case has branch thermal ratings. See
///     :func:`builtin_case_rated_flags` for a quick reference.
///     Transfer-capability tools will return "unconstrained" on
///     unrated cases.
///
/// Returns:
///     Network: The loaded benchmark network.
///
/// Raises:
///     ValueError: If ``name`` is not a recognized built-in case.
#[pyfunction]
pub fn load_builtin_case(name: &str) -> PyResult<Network> {
    match name {
        "case9" => case9(),
        "case14" => case14(),
        "case30" => case30(),
        "market30" => market30(),
        "case57" => case57(),
        "case118" => case118(),
        "case300" => case300(),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown built-in case {other:?}; available: case9, case14, case30, market30, case57, case118, case300"
        ))),
    }
}
