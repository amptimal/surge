// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

pub(crate) fn case_path(stem: &str) -> std::path::PathBuf {
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
    direct
}
