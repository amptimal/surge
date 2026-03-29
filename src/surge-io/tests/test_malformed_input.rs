// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Malformed / corrupt input handling tests for surge-io parsers.
//!
//! These tests verify that parsers return `Err` (not panic) when given
//! invalid, truncated, garbage, or edge-case inputs.
//!
//! Parsers covered: MATPOWER (.m), PSS/E RAW (.raw), IEEE CDF (.cdf),
//! JSON, and the `load` dispatcher.

use std::path::PathBuf;
use tempfile::TempDir;

/// Write string content to a temp file and return its path.
fn write_temp(dir: &TempDir, name: &str, content: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("failed to write temp file");
    path
}

/// Write raw bytes to a temp file and return its path.
fn write_temp_bytes(dir: &TempDir, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("failed to write temp file");
    path
}

/// Assert that calling `f` neither panics nor returns `Ok`.
fn assert_returns_err<T, E>(label: &str, f: impl FnOnce() -> Result<T, E> + std::panic::UnwindSafe)
where
    T: std::fmt::Debug,
    E: std::fmt::Debug,
{
    let outcome = std::panic::catch_unwind(f);
    match outcome {
        Err(_) => {
            panic!("[{label}] parser PANICKED on malformed input — should return Err instead")
        }
        Ok(result) => {
            assert!(
                result.is_err(),
                "[{label}] parser returned Ok on malformed input — should return Err"
            );
        }
    }
}

/// Assert that calling `f` does not panic. Ok or Err is acceptable.
fn assert_no_panic<T>(label: &str, f: impl FnOnce() -> T + std::panic::UnwindSafe) {
    let outcome = std::panic::catch_unwind(f);
    assert!(
        outcome.is_ok(),
        "[{label}] parser PANICKED — should not panic on any input"
    );
}

// =========================================================================
// MATPOWER parser — malformed input tests
// =========================================================================

#[test]
fn test_matpower_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.m", "");
    assert_returns_err("matpower/empty_file", || surge_io::matpower::load(&path));
}

#[test]
fn test_matpower_empty_string() {
    assert_returns_err("matpower/empty_string", || surge_io::matpower::loads(""));
}

#[test]
fn test_matpower_whitespace_only() {
    assert_returns_err("matpower/whitespace_only", || {
        surge_io::matpower::loads("   \n\n  \n")
    });
}

#[test]
fn test_matpower_comments_only() {
    assert_returns_err("matpower/comments_only", || {
        surge_io::matpower::loads("% this is just a comment\n% another comment\n")
    });
}

#[test]
fn test_matpower_truncated_bus_section() {
    let content = "\
function mpc = case_truncated
mpc.version = '2';
mpc.baseMVA = 100;
mpc.bus = [
    1  3  0  0  0  0  1  1.0  0  345  1  1.1
";
    assert_returns_err("matpower/truncated_bus", || {
        surge_io::matpower::loads(content)
    });
}

#[test]
fn test_matpower_bad_numbers_in_bus() {
    let content = "\
function mpc = case_bad_numbers
mpc.baseMVA = 100;
mpc.bus = [
    1  3  abc  def  0  0  1  1.0  0  345  1  1.1  0.9;
];
mpc.gen = [
    1  0  0  300  -300  1.0  100  1  250  0;
];
mpc.branch = [
    1  1  0.01  0.1  0  100  100  100  0  0  1  -360  360;
];
";
    assert_returns_err("matpower/bad_numbers", || {
        surge_io::matpower::loads(content)
    });
}

#[test]
fn test_matpower_missing_branch_section() {
    let content = "\
function mpc = case_no_branch
mpc.baseMVA = 100;
mpc.bus = [
    1  3  0  0  0  0  1  1.0  0  345  1  1.1  0.9;
];
mpc.gen = [
    1  0  0  300  -300  1.0  100  1  250  0;
];
";
    assert_returns_err("matpower/missing_branch", || {
        surge_io::matpower::loads(content)
    });
}

#[test]
fn test_matpower_missing_bus_section() {
    let content = "\
function mpc = case_no_bus
mpc.baseMVA = 100;
mpc.gen = [
    1  0  0  300  -300  1.0  100  1  250  0;
];
mpc.branch = [
    1  2  0.01  0.1  0  100  100  100  0  0  1  -360  360;
];
";
    assert_returns_err("matpower/missing_bus", || {
        surge_io::matpower::loads(content)
    });
}

#[test]
fn test_matpower_insufficient_bus_columns() {
    let content = "\
function mpc = case_short_bus
mpc.baseMVA = 100;
mpc.bus = [
    1  3  0  0  0;
];
mpc.gen = [
    1  0  0  300  -300  1.0  100  1  250  0;
];
mpc.branch = [
    1  1  0.01  0.1  0  100  100  100  0  0  1  -360  360;
];
";
    assert_returns_err("matpower/insufficient_bus_cols", || {
        surge_io::matpower::loads(content)
    });
}

#[test]
fn test_matpower_binary_garbage() {
    let dir = TempDir::new().unwrap();
    let garbage: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
    let path = write_temp_bytes(&dir, "garbage.m", &garbage);
    assert_returns_err("matpower/binary_garbage", || {
        surge_io::matpower::load(&path)
    });
}

#[test]
fn test_matpower_nonexistent_file() {
    let path = PathBuf::from("/tmp/surge_test_nonexistent_file_12345.m");
    let result = surge_io::matpower::load(&path);
    assert!(result.is_err(), "Nonexistent file should produce an error");
}

#[test]
fn test_matpower_zero_base_mva() {
    let content = "\
function mpc = case_zero_mva
mpc.baseMVA = 0;
mpc.bus = [
    1  3  0  0  0  0  1  1.0  0  345  1  1.1  0.9;
];
mpc.gen = [
    1  0  0  300  -300  1.0  100  1  250  0;
];
mpc.branch = [
    1  1  0.01  0.1  0  100  100  100  0  0  1  -360  360;
];
";
    assert_no_panic("matpower/zero_basemva", || {
        surge_io::matpower::loads(content)
    });
}

#[test]
fn test_matpower_only_function_header() {
    assert_returns_err("matpower/only_function_header", || {
        surge_io::matpower::loads("function mpc = test_case\n")
    });
}

#[test]
fn test_matpower_empty_sections() {
    let content = "\
function mpc = case_empty_sections
mpc.baseMVA = 100;
mpc.bus = [
];
mpc.gen = [
];
mpc.branch = [
];
";
    assert_returns_err("matpower/empty_sections", || {
        surge_io::matpower::loads(content)
    });
}

// =========================================================================
// PSS/E RAW parser — malformed input tests
// =========================================================================

#[test]
fn test_psse_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.raw", "");
    assert_returns_err("psse/empty_file", || surge_io::psse::raw::load(&path));
}

#[test]
fn test_psse_empty_string() {
    assert_returns_err("psse/empty_string", || surge_io::psse::raw::loads(""));
}

#[test]
fn test_psse_only_one_header_line() {
    assert_returns_err("psse/only_one_line", || {
        surge_io::psse::raw::loads("0, 100.0, 33, 0, 0, 60.0\n")
    });
}

#[test]
fn test_psse_binary_garbage() {
    let dir = TempDir::new().unwrap();
    let garbage: Vec<u8> = (0..512).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let path = write_temp_bytes(&dir, "garbage.raw", &garbage);
    assert_returns_err("psse/binary_garbage", || surge_io::psse::raw::load(&path));
}

#[test]
fn test_psse_nonexistent_file() {
    let path = PathBuf::from("/tmp/surge_test_nonexistent_file_12345.raw");
    let result = surge_io::psse::raw::load(&path);
    assert!(
        result.is_err(),
        "Nonexistent PSS/E file should produce an error"
    );
}

// =========================================================================
// IEEE CDF parser — malformed input tests
// =========================================================================

#[test]
fn test_cdf_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.cdf", "");
    assert_returns_err("cdf/empty_file", || surge_io::ieee_cdf::load(&path));
}

#[test]
fn test_cdf_empty_string() {
    assert_returns_err("cdf/empty_string", || surge_io::ieee_cdf::loads(""));
}

#[test]
fn test_cdf_binary_garbage() {
    let dir = TempDir::new().unwrap();
    let garbage: Vec<u8> = (0..512).map(|i| ((i * 11 + 7) % 256) as u8).collect();
    let path = write_temp_bytes(&dir, "garbage.cdf", &garbage);
    assert_returns_err("cdf/binary_garbage", || surge_io::ieee_cdf::load(&path));
}

#[test]
fn test_cdf_nonexistent_file() {
    let path = PathBuf::from("/tmp/surge_test_nonexistent_file_12345.cdf");
    let result = surge_io::ieee_cdf::load(&path);
    assert!(
        result.is_err(),
        "Nonexistent CDF file should produce an error"
    );
}

// =========================================================================
// load dispatcher — malformed input tests
// =========================================================================

#[test]
fn test_load_empty_matpower() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.m", "");
    let result = surge_io::load(&path);
    assert!(result.is_err(), "load should fail on empty .m file");
}

#[test]
fn test_load_empty_psse() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.raw", "");
    let result = surge_io::load(&path);
    assert!(result.is_err(), "load should fail on empty .raw file");
}

#[test]
fn test_load_nonexistent() {
    let result = surge_io::load(std::path::Path::new("/tmp/surge_does_not_exist_42.m"));
    assert!(result.is_err(), "load should fail on nonexistent file");
}

// =========================================================================
// CDF truncation test (M-1)
// =========================================================================

#[test]
fn test_cdf_truncated_branch_section() {
    // Header says 20 branches but file ends after BUS DATA with no branch data.
    let content = "\
 08/19/93 UW ARCHIVE           100.0  1993 W IEEE 14 Bus Test Case
BUS DATA FOLLOWS                            14 ITEMS
    1 Bus 1     HV  1  1  3 1.060    0.0      0.0      0.0    232.4   -16.9   0.0     0.0     1.060     0.0     0.0      0.0     0.0   0.0 0.0
    2 Bus 2     HV  1  1  2 1.045   -4.98    21.7     12.7     40.0    42.4  40.0     0.0     1.045    50.0   -40.0      0.0     0.0   0.0 0.0
-999
BRANCH DATA FOLLOWS                         20 ITEMS
";
    let result = surge_io::ieee_cdf::loads(content);
    assert!(
        result.is_err(),
        "Truncated CDF should return Err, not a partial network"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("truncat")
            || err_msg.contains("terminat")
            || err_msg.contains("incomplete"),
        "Error should mention truncation: {err_msg}"
    );
}

#[test]
fn test_cdf_truncated_bus_section() {
    // BUS DATA section starts but file ends before -999 terminator.
    let content = "\
 08/19/93 UW ARCHIVE           100.0  1993 W IEEE 14 Bus Test Case
BUS DATA FOLLOWS                            14 ITEMS
    1 Bus 1     HV  1  1  3 1.060    0.0      0.0      0.0    232.4   -16.9   0.0     0.0     1.060     0.0     0.0      0.0     0.0   0.0 0.0
";
    let result = surge_io::ieee_cdf::loads(content);
    assert!(
        result.is_err(),
        "CDF with truncated bus section should return Err"
    );
}

// =========================================================================
// UCTE parser — malformed input tests
// =========================================================================

#[test]
fn test_ucte_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.uct", "");
    assert_no_panic("ucte/empty_file", || surge_io::ucte::load(&path));
}

#[test]
fn test_ucte_empty_string() {
    assert_no_panic("ucte/empty_string", || surge_io::ucte::loads(""));
}

#[test]
fn test_ucte_binary_garbage() {
    let dir = TempDir::new().unwrap();
    let garbage: Vec<u8> = (0..512).map(|i| ((i * 13 + 3) % 256) as u8).collect();
    let path = write_temp_bytes(&dir, "garbage.uct", &garbage);
    assert_no_panic("ucte/binary_garbage", || surge_io::ucte::load(&path));
}

// =========================================================================
// EPC parser — malformed input tests
// =========================================================================

#[test]
fn test_epc_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.epc", "");
    assert_no_panic("epc/empty_file", || surge_io::epc::load(&path));
}

#[test]
fn test_epc_empty_string() {
    assert_no_panic("epc/empty_string", || surge_io::epc::loads(""));
}

#[test]
fn test_epc_binary_garbage() {
    let dir = TempDir::new().unwrap();
    let garbage: Vec<u8> = (0..512).map(|i| ((i * 17 + 5) % 256) as u8).collect();
    let path = write_temp_bytes(&dir, "garbage.epc", &garbage);
    assert_no_panic("epc/binary_garbage", || surge_io::epc::load(&path));
}

// =========================================================================
// DSS parser — malformed input tests
// =========================================================================

#[test]
fn test_dss_empty_string() {
    assert_returns_err("dss/empty_string", || surge_io::dss::loads(""));
}

#[test]
fn test_dss_invalid_syntax() {
    assert_no_panic("dss/invalid_syntax", || {
        surge_io::dss::loads("not a valid dss file at all @@#$%")
    });
}

// =========================================================================
// DYR parser — malformed input tests
// =========================================================================

#[test]
fn test_dyr_empty_string() {
    let result = surge_io::psse::dyr::loads("");
    // Empty DYR should return empty vec or an error, but NOT panic
    assert!(
        result.is_ok() || result.is_err(),
        "DYR parser should not panic on empty input"
    );
}

#[test]
fn test_dyr_binary_garbage() {
    let garbage =
        String::from_utf8_lossy(&(0..256u16).map(|i| (i % 128) as u8).collect::<Vec<_>>())
            .to_string();
    assert_no_panic("dyr/binary_garbage", || {
        surge_io::psse::dyr::loads(&garbage)
    });
}

#[test]
fn test_dyr_invalid_records() {
    let content = "999 'INVALID' 1 2 3 /\n";
    assert_no_panic("dyr/invalid_records", || {
        surge_io::psse::dyr::loads(content)
    });
}

// =========================================================================
// RAWX parser — malformed input tests
// =========================================================================

#[test]
fn test_rawx_empty_string() {
    assert_returns_err("rawx/empty_string", || surge_io::psse::rawx::loads(""));
}

#[test]
fn test_rawx_binary_garbage() {
    let dir = TempDir::new().unwrap();
    let garbage: Vec<u8> = (0..512).map(|i| ((i * 19 + 7) % 256) as u8).collect();
    let path = write_temp_bytes(&dir, "garbage.rawx", &garbage);
    assert_no_panic("rawx/binary_garbage", || surge_io::psse::rawx::load(&path));
}

// =========================================================================
// XIIDM parser — malformed input tests
// =========================================================================

#[test]
fn test_xiidm_empty_string() {
    assert_no_panic("xiidm/empty_string", || surge_io::xiidm::loads(""));
}

#[test]
fn test_xiidm_invalid_xml() {
    assert_returns_err("xiidm/invalid_xml", || {
        surge_io::xiidm::loads("<not><valid></xiidm>")
    });
}

#[test]
fn test_xiidm_empty_network() {
    let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_0" id="test" caseDate="2024-01-01T00:00:00.000Z" forecastDistance="0" sourceFormat="test">
</iidm:network>"#;
    assert_no_panic("xiidm/empty_network", || surge_io::xiidm::loads(content));
}

// =========================================================================
// SEQ parser — malformed input tests
// =========================================================================

#[test]
fn test_seq_empty_string() {
    let mut net = surge_network::Network::new("test");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        surge_io::psse::sequence::apply_text(&mut net, "")
    }));
    assert!(
        result.is_ok(),
        "[seq/empty_string] parser PANICKED — should not panic on any input"
    );
}

#[test]
fn test_seq_binary_garbage() {
    let mut net = surge_network::Network::new("test");
    let garbage =
        String::from_utf8_lossy(&(0..256u16).map(|i| (i % 128) as u8).collect::<Vec<_>>())
            .to_string();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        surge_io::psse::sequence::apply_text(&mut net, &garbage)
    }));
    assert!(
        result.is_ok(),
        "[seq/binary_garbage] parser PANICKED — should not panic on any input"
    );
}

// =========================================================================
// CGMES parser — malformed input tests
// =========================================================================

#[test]
fn test_cgmes_empty_string() {
    assert_returns_err("cgmes/empty_string", || surge_io::cgmes::loads(""));
}

#[test]
fn test_cgmes_invalid_xml() {
    assert_no_panic("cgmes/invalid_xml", || {
        surge_io::cgmes::loads("<not valid xml at all>")
    });
}

#[test]
fn test_cgmes_empty_dir() {
    let dir = TempDir::new().unwrap();
    let refs: Vec<&std::path::Path> = vec![dir.path()];
    assert_returns_err("cgmes/empty_dir", || surge_io::cgmes::load_all(&refs));
}

// =========================================================================
// JSON parser — malformed input tests
// =========================================================================

#[test]
fn test_json_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "empty.json", "");
    let result = surge_io::json::load(&path);
    assert!(result.is_err(), "Empty JSON file should produce an error");
}

#[test]
fn test_json_invalid_json() {
    let dir = TempDir::new().unwrap();
    let path = write_temp(&dir, "invalid.json", "{not valid json!!!}");
    let result = surge_io::json::load(&path);
    assert!(result.is_err(), "Invalid JSON should produce an error");
}

#[test]
fn test_json_nonexistent() {
    let path = PathBuf::from("/tmp/surge_test_nonexistent_42.json");
    let result = surge_io::json::load(&path);
    assert!(
        result.is_err(),
        "Nonexistent JSON file should produce an error"
    );
}
