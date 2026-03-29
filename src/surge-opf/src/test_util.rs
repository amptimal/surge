// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared test utilities for the surge-opf crate.

/// Global mutex ensuring Ipopt/MUMPS tests never run concurrently.
///
/// MUMPS (the default Ipopt linear solver) is NOT thread-safe.  Any test that
/// calls into Ipopt must hold this lock for the duration of the solve to prevent
/// SIGSEGV under `cargo test` (which defaults to multiple threads).
///
/// Usage in tests:
/// ```ignore
/// let _g = crate::test_util::IPOPT_MUTEX.lock().unwrap();
/// ```
pub(crate) static IPOPT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn data_available() -> bool {
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

pub(crate) fn test_data_dir() -> std::path::PathBuf {
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

pub(crate) fn test_data_path(name: &str) -> std::path::PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::PathBuf::from(p).join(name);
    }
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let bench_path = workspace.join("tests/data").join(name);
    if bench_path.exists() {
        return bench_path;
    }
    // Fall back to examples/cases/ .surge.json.zst bundles.
    let stem = name.trim_end_matches(".m");
    for dir_name in [stem, &format!("ieee{}", stem.trim_start_matches("case"))] {
        let zst_path = workspace.join(format!("examples/cases/{dir_name}/{stem}.surge.json.zst"));
        if zst_path.exists() {
            return zst_path;
        }
    }
    bench_path
}

/// Return the path to a local `.surge.json.zst` case file shipped in `examples/cases/`.
///
/// The directory may be `examples/cases/{stem}/` (e.g. `case9`) or
/// `examples/cases/ieee{num}/` (e.g. `ieee118` for `case118`).
/// Panics if the file cannot be found — intended for cases we know we ship.
pub(crate) fn case_path(stem: &str) -> std::path::PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    for dir_name in [stem, &format!("ieee{}", stem.trim_start_matches("case"))] {
        let p = workspace.join(format!("examples/cases/{dir_name}/{stem}.surge.json.zst"));
        if p.exists() {
            return p;
        }
    }
    panic!(
        "case_path({stem:?}): file not found in examples/cases/{stem}/ or examples/cases/ieee{}/ — \
         is the .surge.json.zst file present?",
        stem.trim_start_matches("case")
    );
}
