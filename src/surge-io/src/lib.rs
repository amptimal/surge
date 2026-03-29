// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge I/O — canonical network interchange APIs.
//!
//! The crate root is intentionally small:
//!
//! - [`load`] for extension-driven network file loading
//! - [`save`] for extension-driven network file saving
//! - [`loads`] / [`dumps`] for in-memory single-document formats
//!
//! Multi-profile formats and sidecar artifacts live under their own modules:
//!
//! - [`cgmes`] for explicit CGMES load/save/profile generation
//! - [`psse::raw`], [`psse::rawx`], [`psse::dyr`], [`psse::dyd`], [`psse::sequence`]
//! - [`geo`] and [`export`] for coordinate and tabular export utilities

pub mod bin;
pub mod cgmes;
pub mod comtrade;
pub mod dss;
pub mod epc;
pub mod export;
pub mod geo;
pub mod iec62325;
pub mod ieee_cdf;
pub mod json;
pub mod matpower;
pub mod profiles;
pub mod pscad;
pub mod psse;
pub mod saturation_toml;
pub mod scl;
pub mod shaft_toml;
pub mod ucte;
pub mod xiidm;

mod parse_utils;
mod union_find;

use std::path::{Path, PathBuf};

use surge_network::{Network, NetworkError};

/// In-memory single-document formats supported by [`loads`] and [`dumps`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Format {
    Matpower,
    /// PSS/E RAW format.
    ///
    /// `loads()` auto-detects the RAW version from the document content. The
    /// stored version is used by `dumps()` / `save()` when serializing.
    PsseRaw(psse::raw::Version),
    Xiidm,
    Ucte,
    SurgeJson,
    Dss,
    Epc,
}

/// Errors from [`load`] and [`loads`].
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(
        "unsupported input format: '{0}'. Supported: directory, .m, .raw, .rawx, .cdf, .xiidm/.iidm, .uct/.ucte, .xml/.cim, .zip, .epc, .dss, .surge.json, .surge.json.zst, .json, .json.zst, .surge.bin"
    )]
    UnsupportedFormat(String),

    #[error(transparent)]
    Cgmes(#[from] cgmes::Error),
    #[error(transparent)]
    Matpower(#[from] matpower::LoadError),
    #[error(transparent)]
    PsseRaw(#[from] psse::raw::LoadError),
    #[error(transparent)]
    Rawx(#[from] psse::rawx::LoadError),
    #[error(transparent)]
    Cdf(#[from] ieee_cdf::Error),
    #[error(transparent)]
    Xiidm(#[from] xiidm::Error),
    #[error(transparent)]
    Ucte(#[from] ucte::LoadError),
    #[error(transparent)]
    Epc(#[from] epc::LoadError),
    #[error(transparent)]
    Dss(#[from] dss::LoadError),
    #[error(transparent)]
    Json(#[from] json::Error),
    #[error(transparent)]
    Bin(#[from] bin::Error),
    #[error(transparent)]
    InvalidNetwork(#[from] NetworkError),
}

/// Errors from [`save`] and [`dumps`].
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(
        "directory target {path} requires an explicit module; use surge_io::cgmes::save for CGMES output"
    )]
    DirectoryTarget { path: PathBuf },

    #[error("CGMES output is explicit; use surge_io::cgmes::save for '{path}'")]
    ExplicitCgmesTarget { path: PathBuf },

    #[error(
        "unsupported export format: '{0}'. Supported: .m, .raw, .epc, .xiidm/.iidm, .dss, .uct/.ucte, .surge.json, .surge.json.zst, .json, .json.zst, .surge.bin. Use surge_io::cgmes::save for CGMES."
    )]
    UnsupportedFormat(String),

    #[error(transparent)]
    Matpower(#[from] matpower::SaveError),
    #[error(transparent)]
    PsseRaw(#[from] psse::raw::SaveError),
    #[error(transparent)]
    Xiidm(#[from] xiidm::Error),
    #[error(transparent)]
    Json(#[from] json::Error),
    #[error(transparent)]
    Bin(#[from] bin::Error),
    #[error(transparent)]
    Dss(#[from] dss::SaveError),
    #[error(transparent)]
    Epc(#[from] epc::SaveError),
    #[error(transparent)]
    Ucte(#[from] ucte::SaveError),
}

fn lowercase_filename(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn canonical_format_name(path: &Path) -> String {
    let filename = lowercase_filename(path);
    if filename.ends_with(".surge.json.zst") {
        ".surge.json.zst".to_string()
    } else if filename.ends_with(".json.zst") {
        ".json.zst".to_string()
    } else if filename.ends_with(".surge.json") {
        ".surge.json".to_string()
    } else if filename.ends_with(".json") {
        ".json".to_string()
    } else if filename.ends_with(".surge.bin") {
        ".surge.bin".to_string()
    } else {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_ascii_lowercase()))
            .unwrap_or_default()
    }
}

fn finalize_loaded_network(mut network: Network) -> Result<Network, LoadError> {
    // Canonicalize runtime-facing identities once at the I/O boundary so all
    // downstream solver/study APIs see a stable network contract.
    network.canonicalize_runtime_identities();
    network.validate()?;
    Ok(network)
}

/// Load a network file, auto-detecting the format from the file extension.
///
/// Supported inputs:
/// - Directory of CGMES `.xml` files (ENTSO-E multi-profile bundle)
/// - `.m` — MATPOWER
/// - `.raw` — PSS/E RAW
/// - `.rawx` — PSS/E RAWX (JSON)
/// - `.cdf` — IEEE CDF
/// - `.xiidm`, `.iidm` — PowSyBl XIIDM
/// - `.uct`, `.ucte` — UCTE-DEF
/// - `.xml`, `.cim` — CGMES/CIM
/// - `.zip` — CGMES multi-profile bundle packaged as a zip archive
/// - `.epc` — GE PSLF EPC
/// - `.dss` — OpenDSS
/// - `.surge.json`, `.json` — Surge JSON
/// - `.surge.json.zst`, `.json.zst` — zstd-compressed Surge JSON
/// - `.surge.bin` — Surge binary
///
/// # Example
///
/// ```no_run
/// use surge_io::load;
///
/// let net = load("examples/cases/ieee118/case118.surge.json.zst").unwrap();
/// println!("{} buses, {} branches", net.buses.len(), net.branches.len());
/// ```
pub fn load(path: impl AsRef<Path>) -> Result<Network, LoadError> {
    let path = path.as_ref();
    let network = if path.is_dir() {
        Ok(cgmes::load(path)?)
    } else {
        let format_name = canonical_format_name(path);

        tracing::info!(
            path = %path.display(),
            format = format_name.as_str(),
            "parsing case file"
        );

        match format_name.as_str() {
            ".m" => Ok(matpower::load(path)?),
            ".raw" => Ok(psse::raw::load(path)?),
            ".rawx" => Ok(psse::rawx::load(path)?),
            ".cdf" => Ok(ieee_cdf::load(path)?),
            ".xiidm" | ".iidm" => Ok(xiidm::load(path)?),
            ".uct" | ".ucte" => Ok(ucte::load(path)?),
            ".xml" | ".cim" | ".zip" => Ok(cgmes::load(path)?),
            ".epc" => Ok(epc::load(path)?),
            ".dss" => Ok(dss::load(path)?),
            ".surge.json" | ".surge.json.zst" | ".json" | ".json.zst" => Ok(json::load(path)?),
            ".surge.bin" => Ok(bin::load(path)?),
            _ => Err(LoadError::UnsupportedFormat(format_name.clone())),
        }
    };

    let network = network.and_then(finalize_loaded_network);

    if let Ok(ref net) = network {
        tracing::info!(
            buses = net.n_buses(),
            branches = net.branches.len(),
            generators = net.generators.len(),
            "case file parsed"
        );
    }

    network
}

/// Parse an in-memory network document.
pub fn loads(content: &str, format: Format) -> Result<Network, LoadError> {
    match format {
        Format::Matpower => Ok(matpower::loads(content)?),
        Format::PsseRaw(_) => Ok(psse::raw::loads(content)?),
        Format::Xiidm => Ok(xiidm::loads(content)?),
        Format::Ucte => Ok(ucte::loads(content)?),
        Format::SurgeJson => Ok(json::loads(content)?),
        Format::Dss => Ok(dss::loads(content)?),
        Format::Epc => Ok(epc::loads(content)?),
    }
    .and_then(finalize_loaded_network)
}

/// Save a network to a file, auto-detecting the format from the file extension.
///
/// Supported outputs: `.m`, `.raw`, `.xiidm`, `.iidm`, `.dss`, `.epc`,
/// `.uct`, `.ucte`, `.surge.json`, `.surge.json.zst`, `.json`, `.json.zst`,
/// `.surge.bin`.
///
/// CGMES output is explicit and directory-based. Use [`cgmes::save`] instead.
pub fn save(network: &Network, path: impl AsRef<Path>) -> Result<(), SaveError> {
    let path = path.as_ref();
    if path.is_dir() {
        return Err(SaveError::DirectoryTarget {
            path: path.to_path_buf(),
        });
    }

    let format_name = canonical_format_name(path);

    match format_name.as_str() {
        ".m" => matpower::save(network, path)?,
        ".raw" => psse::raw::save(network, path, psse::raw::Version::V33)?,
        ".xiidm" | ".iidm" => xiidm::save(network, path)?,
        ".surge.json" | ".surge.json.zst" | ".json" | ".json.zst" => json::save(network, path)?,
        ".surge.bin" => bin::save(network, path)?,
        ".dss" => dss::save(network, path)?,
        ".epc" => epc::save(network, path)?,
        ".uct" | ".ucte" => ucte::save(network, path)?,
        ".xml" | ".cim" | ".zip" => {
            return Err(SaveError::ExplicitCgmesTarget {
                path: path.to_path_buf(),
            });
        }
        _ => return Err(SaveError::UnsupportedFormat(format_name)),
    }

    Ok(())
}

/// Serialize a network into an in-memory document.
pub fn dumps(network: &Network, format: Format) -> Result<String, SaveError> {
    match format {
        Format::Matpower => Ok(matpower::dumps(network)?),
        Format::PsseRaw(version) => Ok(psse::raw::dumps(network, version)?),
        Format::Xiidm => Ok(xiidm::dumps(network)?),
        Format::Ucte => Ok(ucte::dumps(network)?),
        Format::SurgeJson => Ok(json::dumps(network)?),
        Format::Dss => Ok(dss::dumps(network)?),
        Format::Epc => Ok(epc::dumps(network)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn mini_network() -> Network {
        let mut net = Network::new("case_mini");
        net.base_mva = 100.0;

        let mut slack = Bus::new(1, BusType::Slack, 345.0);
        slack.voltage_magnitude_pu = 1.04;
        net.buses.push(slack);

        let pq = Bus::new(2, BusType::PQ, 345.0);
        net.buses.push(pq);
        net.loads.push(Load::new(2, 100.0, 35.0));

        net.generators.push(Generator::new(1, 80.0, 1.04));
        net.branches.push(Branch::new_line(1, 2, 0.02, 0.06, 0.03));
        net
    }

    fn write_zip(entries: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bundle.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in entries {
            zip.start_file(name, options).unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        (dir, zip_path)
    }

    #[test]
    fn test_load_matpower_extension_routes() {
        let result = load("nonexistent.m");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_psse_extension_routes() {
        let result = load("nonexistent.raw");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_xiidm_extension_routes() {
        let result = load("nonexistent.xiidm");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_ucte_extension_routes() {
        let result = load("nonexistent.ucte");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_json_extension_routes() {
        let result = load("nonexistent.surge.json");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_json_zst_extension_routes() {
        let result = load("nonexistent.surge.json.zst");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_bin_extension_routes() {
        let result = load("nonexistent.surge.bin");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_unknown_extension_errors() {
        let result = load("file.xyz");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unsupported input format"), "Got: {msg}");
    }

    #[test]
    fn test_load_cgmes_directory() {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let workspace = PathBuf::from(&manifest)
            .join("../..")
            .join("tests/data/cgmes/case14");
        let path = match std::fs::canonicalize(&workspace) {
            Ok(path) => path,
            Err(_) => return,
        };

        let result = load(&path);
        match result {
            Ok(net) => assert_eq!(net.n_buses(), 14),
            Err(err) => panic!("load on CGMES directory failed: {err}"),
        }
    }

    #[test]
    fn test_load_zip_rejects_unsafe_paths() {
        let (_dir, zip_path) = write_zip(&[("../EQ.xml", "<xml />")]);
        let err = load(&zip_path).unwrap_err();
        match err {
            LoadError::Cgmes(cgmes::Error::InvalidArchiveEntryPath { .. }) => {}
            other => panic!("expected invalid archive path error, got: {other}"),
        }
    }

    #[test]
    fn test_load_zip_skips_diagramlayout_case_insensitively() {
        let (_dir, zip_path) = write_zip(&[("profiles/diagramlayout.xml", "<DiagramLayout />")]);
        let err = load(&zip_path).unwrap_err();
        match err {
            LoadError::Cgmes(cgmes::Error::NoProfiles { .. }) => {}
            other => panic!("expected no CGMES profiles error, got: {other}"),
        }
    }

    #[test]
    fn test_save_m_extension_roundtrip() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_save_test.m");
        save(&net, &tmp).unwrap();
        let net2 = load(&tmp).unwrap();
        assert_eq!(net2.n_buses(), net.n_buses());
        assert_eq!(net2.n_branches(), net.n_branches());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_save_raw_extension_roundtrip() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_save_test.raw");
        save(&net, &tmp).unwrap();
        let contents = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            contents
                .lines()
                .next()
                .unwrap_or_default()
                .contains("PSS/E 33 Raw Data"),
            "generic .raw save should emit version 33, got: {}",
            contents.lines().next().unwrap_or_default()
        );
        let net2 = load(&tmp).unwrap();
        assert_eq!(net2.n_buses(), net.n_buses());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_save_xiidm_roundtrip() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_save_test.xiidm");
        save(&net, &tmp).unwrap();
        let net2 = load(&tmp).unwrap();
        assert_eq!(net2.n_buses(), net.n_buses());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_save_json_extension_roundtrip() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_save_test.surge.json");
        save(&net, &tmp).unwrap();
        let net2 = load(&tmp).unwrap();
        assert_eq!(net2.n_buses(), net.n_buses());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_save_json_zst_extension_roundtrip() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_save_test.surge.json.zst");
        save(&net, &tmp).unwrap();
        let net2 = load(&tmp).unwrap();
        assert_eq!(net2.n_buses(), net.n_buses());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_save_bin_extension_roundtrip() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_save_test.surge.bin");
        save(&net, &tmp).unwrap();
        let net2 = load(&tmp).unwrap();
        assert_eq!(net2.n_buses(), net.n_buses());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_save_rejects_cgmes_file_target() {
        let net = mini_network();
        let tmp = std::env::temp_dir().join("surge_save_test.xml");
        let result = save(&net, &tmp);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("surge_io::cgmes::save"), "Got: {msg}");
    }

    #[test]
    fn test_loads_canonicalizes_runtime_ids() {
        let mut net = mini_network();
        net.generators[0].id = "  ".to_string();
        let mut switched_shunt =
            surge_network::network::SwitchedShunt::capacitor_only(2, 0.1, 2, 1.0);
        switched_shunt.id = " ".to_string();
        net.controls.switched_shunts.push(switched_shunt);

        let json = json::dumps(&net).expect("serialize network");
        let loaded = loads(&json, Format::SurgeJson).expect("loads should canonicalize");

        assert_eq!(loaded.generators[0].id, "gen_1_1");
        assert_eq!(loaded.controls.switched_shunts[0].id, "switched_shunt_2_1");
    }

    #[test]
    fn test_loads_rejects_invalid_area_schedule_contract() {
        let mut net = mini_network();
        net.area_schedules
            .push(surge_network::network::AreaSchedule {
                number: 1,
                slack_bus: 999,
                p_desired_mw: 10.0,
                p_tolerance_mw: 5.0,
                name: "bad".to_string(),
            });

        let json = json::dumps(&net).expect("serialize network");
        let err = loads(&json, Format::SurgeJson).unwrap_err();
        assert!(matches!(
            err,
            LoadError::InvalidNetwork(NetworkError::InvalidAreaScheduleSlackBus {
                area: 1,
                slack_bus: 999
            })
        ));
    }
}
