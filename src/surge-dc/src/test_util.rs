// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared test utilities for surge-dc unit tests.

use surge_network::Network;

/// Early-return from a test if the external test data directory is not present.
///
/// Usage: place `skip_if_no_data!();` at the top of any test that requires
/// `.m` case files from the `surge-bench` repo.
macro_rules! skip_if_no_data {
    () => {
        if !$crate::test_util::data_available() {
            eprintln!("SKIP: tests/data not present — set SURGE_TEST_DATA or clone surge-bench");
            return;
        }
    };
}
pub(crate) use skip_if_no_data;

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
    let num = stem.trim_start_matches("case");
    let alt = workspace.join(format!("examples/cases/ieee{num}/{stem}.surge.json.zst"));
    if alt.exists() {
        return alt;
    }
    // Fall back to MATPOWER .m files in tests/data/
    let matpower = workspace.join(format!("tests/data/{stem}.m"));
    if matpower.exists() {
        return matpower;
    }
    direct
}

pub fn load_net(name: &str) -> Network {
    surge_io::load(case_path(name)).expect("failed to parse case")
}
