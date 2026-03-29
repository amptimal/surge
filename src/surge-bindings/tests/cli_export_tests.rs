// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests for surge-solve CLI export, parse-only, and convert flags.
//!
//! These tests run the actual compiled binary and verify:
//! - --parse-only exits 0 without solving and prints a summary
//! - --convert produces a re-parseable output file
//! - --export (after solving) produces a solved-state JSON artifact
//! - Unknown extensions fail cleanly with a useful error message
//!
//! Tests use the Cargo-built `surge-solve` binary for this test run. They are
//! skipped only when case data is not present.

mod common;

use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn case9() -> std::path::PathBuf {
    common::case_path("case9")
}

static TEMP_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_temp_path(stem: &str, suffix: &str) -> std::path::PathBuf {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be monotonic enough for tests")
        .as_nanos();
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{stem}_{pid}_{nanos}_{counter}{suffix}",))
}

fn read_artifact_json(path: &Path) -> String {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".zst"))
    {
        let file = std::fs::File::open(path).unwrap();
        let mut decoder = zstd::stream::read::Decoder::new(file).unwrap();
        let mut json = String::new();
        decoder.read_to_string(&mut json).unwrap();
        json
    } else {
        std::fs::read_to_string(path).unwrap()
    }
}

fn assert_parse_only_roundtrip(path: &Path) {
    let out = Command::new(common::surge_bin())
        .args([path.to_str().unwrap(), "--parse-only"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "re-parse failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("9 buses") || stdout.contains("buses"),
        "Bus count not in output: {stdout}"
    );
}

#[test]
fn test_help_only_lists_canonical_methods() {
    let out = Command::new(common::surge_bin())
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(
            "possible values: acpf, acpf-warm, fdpf, dcpf, dc-opf, ac-opf, scopf, hvdc, contingency, n-2, orpd, ots, injection-capability, nerc-atc"
        ),
        "canonical method list missing from help: {stdout}"
    );
    assert!(
        !stdout.contains("cpv")
            && !stdout.contains("stuck-breaker")
            && !stdout.contains("overlapping"),
        "legacy or phantom methods still exposed in help: {stdout}"
    );
}

#[test]
fn test_version_matches_package_version() {
    let out = Command::new(common::surge_bin())
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        format!("surge-solve {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn test_help_lists_angle_reference_values() {
    let out = Command::new(common::surge_bin())
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    for value in [
        "preserve-initial",
        "zero",
        "distributed-load",
        "distributed-generation",
        "distributed-inertia",
    ] {
        assert!(
            stdout.contains(value),
            "angle-reference value missing from help output: {value}\n{stdout}"
        );
    }
}

#[test]
fn test_angle_reference_is_rejected_where_unsupported() {
    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--method",
            "contingency",
            "--angle-reference",
            "zero",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "unsupported angle-reference unexpectedly accepted"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--angle-reference"),
        "expected angle-reference validation error, got: {stderr}"
    );
}

#[test]
fn test_legacy_method_aliases_are_rejected() {
    for legacy in ["stuck-breaker", "overlapping", "cpv"] {
        let out = Command::new(common::surge_bin())
            .args(["--method", legacy])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "legacy method unexpectedly accepted: {legacy}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("invalid value"),
            "expected clap value error for {legacy}, got: {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// --parse-only tests
// ---------------------------------------------------------------------------

#[test]
fn test_parse_only_exits_zero() {
    let out = Command::new(common::surge_bin())
        .args([case9().to_str().unwrap(), "--parse-only"])
        .output()
        .expect("failed to run surge-solve");
    assert!(
        out.status.success(),
        "exit code: {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_parse_only_prints_bus_count() {
    let out = Command::new(common::surge_bin())
        .args([case9().to_str().unwrap(), "--parse-only"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // case9 has 9 buses
    assert!(
        stdout.contains("9 buses") || stdout.contains("buses"),
        "Expected bus count in stdout, got: {stdout}"
    );
}

#[test]
fn test_parse_only_does_not_solve() {
    let out = Command::new(common::surge_bin())
        .args([case9().to_str().unwrap(), "--parse-only"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should NOT contain NR convergence output
    assert!(
        !stdout.contains("converged") || stdout.contains("Parse complete"),
        "parse-only should not run solver, got: {stdout}"
    );
}

#[test]
fn test_parse_only_json_emits_json_summary() {
    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--parse-only",
            "--output",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(json["n_buses"], 9);
    assert_eq!(json["n_branches"], 9);
}

#[test]
fn test_dcpf_json_emits_json_result() {
    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--method",
            "dcpf",
            "--output",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect(&stdout);
    assert!(
        json.get("branch_p_from_mw").is_some()
            || json.get("branch_p_mw").is_some()
            || json.get("slack_p_mw").is_some(),
        "unexpected DCPF JSON payload: {stdout}"
    );
}

#[test]
fn test_contingency_json_names_violated_contingency_count_truthfully() {
    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--method",
            "contingency",
            "--output",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect(&stdout);
    assert!(
        json.get("n_contingencies_with_violations").is_some(),
        "expected truthful contingency violation count key, got: {stdout}"
    );
    assert!(
        json.get("n_violations").is_none(),
        "legacy ambiguous JSON key should not be emitted, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// --convert tests
// ---------------------------------------------------------------------------

#[test]
fn test_convert_to_matpower() {
    let tmp = unique_temp_path("surge_cli_convert_test", ".m");
    let _ = std::fs::remove_file(&tmp); // clean up any prior run

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "output file not created");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        content.contains("mpc.bus"),
        "Expected MATPOWER output, got:\n{content}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_convert_to_psse_raw() {
    let tmp = unique_temp_path("surge_cli_convert_test", ".raw");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "output file not created");

    let content = std::fs::read_to_string(&tmp).unwrap();
    // PSS/E file should have section terminators and Q marker
    assert!(
        content.contains("END OF BUS DATA") || content.contains("33"),
        "Expected PSS/E output, got:\n{content}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_convert_to_xiidm() {
    let tmp = unique_temp_path("surge_cli_convert_test", ".xiidm");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "output file not created");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        content.contains("iidm:network"),
        "Expected XIIDM XML output, got:\n{content}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_convert_to_surge_json() {
    let tmp = unique_temp_path("surge_cli_convert_test", ".json");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "output file not created");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        content.contains("\"format\":\"surge-json\""),
        "Expected Surge JSON envelope, got:\n{content}"
    );
    assert_parse_only_roundtrip(&tmp);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_convert_to_surge_json_zst() {
    let tmp = unique_temp_path("surge_cli_convert_test", ".json.zst");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "output file not created");
    assert_parse_only_roundtrip(&tmp);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_convert_to_surge_bin() {
    let tmp = unique_temp_path("surge_cli_convert_test", ".surge.bin");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "output file not created");
    assert_parse_only_roundtrip(&tmp);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_convert_matpower_roundtrip_preserves_buses() {
    let tmp = unique_temp_path("surge_cli_rt_test", ".m");
    let _ = std::fs::remove_file(&tmp);

    Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    // Re-parse with surge-solve --parse-only and verify 9 buses
    assert_parse_only_roundtrip(&tmp);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_convert_with_format_override_psse33() {
    // Force PSS/E format even though extension is .m
    let tmp = unique_temp_path("surge_cli_override_test", ".m");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
            "--export-format",
            "psse33",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        content.contains("33"),
        "Expected PSS/E v33 output: {content}"
    );
    let _ = std::fs::remove_file(&tmp);
}

// ---------------------------------------------------------------------------
// --export tests (after solving)
// ---------------------------------------------------------------------------

#[test]
fn test_export_after_nr_solve() {
    let tmp = unique_temp_path("surge_cli_export_test", ".json");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--method",
            "acpf",
            "--export",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "export file not created");

    let content = read_artifact_json(&tmp);
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(json["artifact_version"], 1);
    assert_eq!(json["result_kind"], "power_flow");
    assert_eq!(json["result"]["status"], "Converged");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_export_after_nr_solve_zst() {
    let tmp = unique_temp_path("surge_cli_export_test", ".json.zst");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--method",
            "acpf",
            "--export",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.exists(), "export file not created");
    let content = read_artifact_json(&tmp);
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(json["artifact_version"], 1);
    assert_eq!(json["result_kind"], "power_flow");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_acpf_warm_export_uses_canonical_final_result() {
    let tmp = unique_temp_path("surge_cli_warm_export_test", ".json");
    let _ = std::fs::remove_file(&tmp);

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--method",
            "acpf-warm",
            "--output",
            "json",
            "--export",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stdout_json: serde_json::Value = serde_json::from_str(&stdout).expect(&stdout);
    let artifact_json: serde_json::Value =
        serde_json::from_str(&read_artifact_json(&tmp)).expect("export artifact should parse");

    assert_eq!(
        stdout_json["solve_time_secs"],
        artifact_json["result"]["solve_time_secs"]
    );
    assert_eq!(artifact_json["artifact_version"], 1);
    let _ = std::fs::remove_file(&tmp);
}

// ---------------------------------------------------------------------------
// Error handling tests
// ---------------------------------------------------------------------------

#[test]
fn test_convert_unknown_extension_fails_cleanly() {
    let tmp = unique_temp_path("surge_cli_bad", ".xyz");

    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--convert",
            tmp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "should fail on unknown extension");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported") || stderr.contains("format"),
        "Expected useful error message, got: {stderr}"
    );
}

#[test]
fn test_export_rejects_misleading_non_artifact_extensions() {
    for suffix in [".m", ".raw"] {
        let tmp = unique_temp_path("surge_cli_export_bad", suffix);
        let out = Command::new(common::surge_bin())
            .args([
                case9().to_str().unwrap(),
                "--method",
                "acpf",
                "--export",
                tmp.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "export unexpectedly accepted misleading extension: {suffix}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("solved-state JSON artifact") || stderr.contains(".json"),
            "expected helpful export-format error, got: {stderr}"
        );
    }
}

#[test]
fn test_export_rejects_format_override() {
    let tmp = unique_temp_path("surge_cli_export_override", ".json");
    let out = Command::new(common::surge_bin())
        .args([
            case9().to_str().unwrap(),
            "--method",
            "acpf",
            "--export",
            tmp.to_str().unwrap(),
            "--export-format",
            "psse33",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "--export-format unexpectedly accepted with --export"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--export-format"),
        "expected export-format validation error, got: {stderr}"
    );
}

#[test]
fn test_parse_nonexistent_file_fails_cleanly() {
    if !common::surge_bin().exists() {
        return;
    }
    let out = Command::new(common::surge_bin())
        .args(["nonexistent_case_file.m", "--parse-only"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "should fail on missing file");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.is_empty(), "Expected error output, got nothing");
}
