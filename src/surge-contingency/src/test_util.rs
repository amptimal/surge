// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared test helpers for the surge-contingency crate.

use surge_network::Network;

pub fn data_available() -> bool {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::Path::new(&p).exists();
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
        .exists()
}

#[allow(dead_code)]
pub fn test_data_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::PathBuf::from(p);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
}

/// Return the path to a `.surge.json.zst` case file shipped in `examples/cases/`.
///
/// Handles the `ieee118/case118` naming convention automatically.
pub fn case_path(stem: &str) -> std::path::PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let direct = workspace.join(format!("examples/cases/{stem}/{stem}.surge.json.zst"));
    if direct.exists() {
        return direct;
    }
    // case118 lives under ieee118/
    let num = stem.trim_start_matches("case");
    let alt = workspace.join(format!("examples/cases/ieee{num}/{stem}.surge.json.zst"));
    if alt.exists() {
        return alt;
    }
    direct
}

pub fn load_case(name: &str) -> Network {
    let path = case_path(name);
    surge_io::load(&path).unwrap_or_else(|e| panic!("failed to parse {name}: {e}"))
}
