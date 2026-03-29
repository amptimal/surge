// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared test utilities for surge-bindings integration tests.

use std::path::PathBuf;

#[allow(dead_code)]
pub fn workspace_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[allow(dead_code)]
pub fn test_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return PathBuf::from(p);
    }
    workspace_root().join("tests/data")
}

#[allow(dead_code)]
pub fn data_available() -> bool {
    test_data_dir().exists()
}

#[allow(dead_code)]
pub fn case_path(stem: &str) -> PathBuf {
    let workspace = workspace_root();
    let direct = workspace.join(format!("examples/cases/{stem}/{stem}.surge.json.zst"));
    if direct.exists() {
        return direct;
    }
    let num = stem.trim_start_matches("case");
    let alt = workspace.join(format!("examples/cases/ieee{num}/{stem}.surge.json.zst"));
    if alt.exists() {
        return alt;
    }
    direct
}

#[allow(dead_code)]
pub fn surge_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_surge-solve"))
}
