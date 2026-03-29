// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical I/O helpers and adjacent package-layer support types.

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use surge_network::dynamics::DynamicModel as CoreDynamicModel;

use crate::exceptions::{catch_panic, to_io_pyerr};
use crate::network::Network;
use crate::solutions::AcPfResult;

fn wrap_network(inner: surge_network::Network) -> Network {
    let mut inner = inner;
    inner.canonicalize_runtime_identities();
    Network {
        inner: Arc::new(inner),
        oltc_controls: Vec::new(),
        switched_shunts: Vec::new(),
    }
}

fn parse_cgmes_version(version: &str) -> PyResult<surge_io::cgmes::Version> {
    match version.to_ascii_lowercase().as_str() {
        "2" | "2.4.15" | "v2" | "v2_4_15" | "cgmes" => Ok(surge_io::cgmes::Version::V2_4_15),
        "3" | "3.0" | "v3" | "v3_0" | "cgmes3" => Ok(surge_io::cgmes::Version::V3_0),
        other => Err(PyValueError::new_err(format!(
            "unknown CGMES version {other:?}; use '2.4.15' or '3.0'"
        ))),
    }
}

fn load_from_path(path: &str, format: &str) -> PyResult<surge_network::Network> {
    let path = PathBuf::from(path);
    match format {
        "matpower" | "m" => surge_io::matpower::load(&path).map_err(to_io_pyerr),
        "psse" | "raw" | "psse-raw" => surge_io::psse::raw::load(&path).map_err(to_io_pyerr),
        "rawx" | "psse-rawx" => surge_io::psse::rawx::load(&path).map_err(to_io_pyerr),
        "cdf" | "ieee-cdf" => surge_io::ieee_cdf::load(&path).map_err(to_io_pyerr),
        "xiidm" | "iidm" => surge_io::xiidm::load(&path).map_err(to_io_pyerr),
        "ucte" | "uct" => surge_io::ucte::load(&path).map_err(to_io_pyerr),
        "json" | "surge-json" => surge_io::json::load(&path).map_err(to_io_pyerr),
        "bin" | "surge-bin" => surge_io::bin::load(&path).map_err(to_io_pyerr),
        "dss" => surge_io::dss::load(&path).map_err(to_io_pyerr),
        "epc" => surge_io::epc::load(&path).map_err(to_io_pyerr),
        "cgmes" => surge_io::cgmes::load(&path).map_err(to_io_pyerr),
        other => Err(PyValueError::new_err(format!(
            "unknown format {other:?}; use 'matpower', 'psse', 'rawx', 'cdf', 'xiidm', 'ucte', 'surge-json', 'surge-bin', 'dss', 'epc', or 'cgmes'"
        ))),
    }
}

fn load_from_text(content: &str, format: &str) -> PyResult<surge_network::Network> {
    match format {
        "matpower" | "m" => surge_io::matpower::loads(content).map_err(to_io_pyerr),
        "psse" | "raw" | "psse-raw" => surge_io::psse::raw::loads(content).map_err(to_io_pyerr),
        "rawx" | "psse-rawx" => surge_io::psse::rawx::loads(content).map_err(to_io_pyerr),
        "cdf" | "ieee-cdf" => surge_io::ieee_cdf::loads(content).map_err(to_io_pyerr),
        "xiidm" | "iidm" => surge_io::xiidm::loads(content).map_err(to_io_pyerr),
        "ucte" | "uct" => surge_io::ucte::loads(content).map_err(to_io_pyerr),
        "json" | "surge-json" => surge_io::json::loads(content).map_err(to_io_pyerr),
        "dss" => surge_io::dss::loads(content).map_err(to_io_pyerr),
        "epc" => surge_io::epc::loads(content).map_err(to_io_pyerr),
        "cgmes" => surge_io::cgmes::loads(content).map_err(to_io_pyerr),
        "bin" | "surge-bin" => Err(PyValueError::new_err(
            "binary documents require bytes; use surge.io.bin.loads(...)",
        )),
        other => Err(PyValueError::new_err(format!(
            "unknown format {other:?}; use 'matpower', 'psse', 'rawx', 'cdf', 'xiidm', 'ucte', 'surge-json', 'dss', 'epc', or 'cgmes'"
        ))),
    }
}

fn load_from_bytes(content: &[u8], format: &str) -> PyResult<surge_network::Network> {
    match format {
        "bin" | "surge-bin" => surge_io::bin::loads(content).map_err(to_io_pyerr),
        other => Err(PyValueError::new_err(format!(
            "unknown binary format {other:?}; use 'surge-bin'"
        ))),
    }
}

fn save_to_path(
    network: &surge_network::Network,
    path: &str,
    format: &str,
    version: Option<u32>,
) -> PyResult<()> {
    let path = PathBuf::from(path);
    match format {
        "matpower" | "m" => surge_io::matpower::save(network, &path).map_err(to_io_pyerr),
        "psse" | "raw" | "psse-raw" => {
            surge_io::psse::raw::save(network, &path, version.unwrap_or(33)).map_err(to_io_pyerr)
        }
        "xiidm" | "iidm" => surge_io::xiidm::save(network, &path).map_err(to_io_pyerr),
        "ucte" | "uct" => surge_io::ucte::save(network, &path).map_err(to_io_pyerr),
        "json" | "surge-json" => surge_io::json::save(network, &path).map_err(to_io_pyerr),
        "bin" | "surge-bin" => surge_io::bin::save(network, &path).map_err(to_io_pyerr),
        "dss" => surge_io::dss::save(network, &path).map_err(to_io_pyerr),
        "epc" => surge_io::epc::save(network, &path).map_err(to_io_pyerr),
        other => Err(PyValueError::new_err(format!(
            "unknown or unsupported save format {other:?}; use 'matpower', 'psse', 'xiidm', 'ucte', 'surge-json', 'surge-bin', 'dss', or 'epc'"
        ))),
    }
}

fn dump_to_string(
    network: &surge_network::Network,
    format: &str,
    version: Option<u32>,
) -> PyResult<String> {
    match format {
        "matpower" | "m" => surge_io::matpower::dumps(network).map_err(to_io_pyerr),
        "psse" | "raw" | "psse-raw" => {
            surge_io::psse::raw::dumps(network, version.unwrap_or(33)).map_err(to_io_pyerr)
        }
        "xiidm" | "iidm" => surge_io::xiidm::dumps(network).map_err(to_io_pyerr),
        "ucte" | "uct" => surge_io::ucte::dumps(network).map_err(to_io_pyerr),
        "json" | "surge-json" => surge_io::json::dumps(network).map_err(to_io_pyerr),
        "dss" => surge_io::dss::dumps(network).map_err(to_io_pyerr),
        "epc" => surge_io::epc::dumps(network).map_err(to_io_pyerr),
        other => Err(PyValueError::new_err(format!(
            "unknown or unsupported dump format {other:?}; use 'matpower', 'psse', 'xiidm', 'ucte', 'surge-json', 'dss', or 'epc'"
        ))),
    }
}

fn dump_to_bytes(network: &surge_network::Network, format: &str) -> PyResult<Vec<u8>> {
    match format {
        "bin" | "surge-bin" => surge_io::bin::dumps(network).map_err(to_io_pyerr),
        other => Err(PyValueError::new_err(format!(
            "unknown binary dump format {other:?}; use 'surge-bin'"
        ))),
    }
}

/// Return the Surge library version string.
#[pyfunction]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Internal package helper for canonical load-profile CSV reads.
#[pyfunction(name = "_io_profiles_read_load_csv")]
pub fn io_profiles_read_load_csv(path: &str) -> PyResult<std::collections::HashMap<u32, Vec<f64>>> {
    catch_panic("_io_profiles_read_load_csv", || {
        let profiles = surge_io::profiles::read_load_profiles_csv(PathBuf::from(path).as_path())
            .map_err(to_io_pyerr)?;
        Ok(profiles
            .profiles
            .into_iter()
            .map(|profile| (profile.bus, profile.load_mw))
            .collect())
    })
}

/// Internal package helper for canonical renewable-profile CSV reads.
#[pyfunction(name = "_io_profiles_read_renewable_csv")]
pub fn io_profiles_read_renewable_csv(
    path: &str,
) -> PyResult<std::collections::HashMap<String, Vec<f64>>> {
    catch_panic("_io_profiles_read_renewable_csv", || {
        let profiles =
            surge_io::profiles::read_renewable_profiles_csv(PathBuf::from(path).as_path())
                .map_err(to_io_pyerr)?;
        Ok(profiles
            .profiles
            .into_iter()
            .map(|profile| (profile.generator_id, profile.capacity_factors))
            .collect())
    })
}

/// Load a power system case file.
#[pyfunction]
pub fn load(path: &str) -> PyResult<Network> {
    catch_panic("load", || {
        let inner = surge_io::load(path).map_err(to_io_pyerr)?;
        Ok(wrap_network(inner))
    })
}

/// Save a network to a file using extension-based auto-detection.
#[pyfunction]
pub fn save(network: &Network, path: &str) -> PyResult<()> {
    catch_panic("save", || {
        surge_io::save(&network.inner, path).map_err(to_io_pyerr)
    })
}

/// Internal package helper for explicit format file loads.
#[pyfunction(name = "_load_as")]
pub fn load_as(path: &str, format: &str) -> PyResult<Network> {
    catch_panic("_load_as", || {
        Ok(wrap_network(load_from_path(path, format)?))
    })
}

/// Internal package helper for explicit format text loads.
#[pyfunction(name = "_loads")]
pub fn loads(content: &str, format: &str) -> PyResult<Network> {
    catch_panic("_loads", || {
        Ok(wrap_network(load_from_text(content, format)?))
    })
}

/// Internal package helper for explicit format byte loads.
#[pyfunction(name = "_loads_bytes")]
pub fn loads_bytes(content: &[u8], format: &str) -> PyResult<Network> {
    catch_panic("_loads_bytes", || {
        Ok(wrap_network(load_from_bytes(content, format)?))
    })
}

/// Internal package helper for explicit format file saves.
#[pyfunction(name = "_save_as")]
#[pyo3(signature = (network, path, format, version = None))]
pub fn save_as(network: &Network, path: &str, format: &str, version: Option<u32>) -> PyResult<()> {
    catch_panic("_save_as", || {
        save_to_path(&network.inner, path, format, version)
    })
}

/// Internal package helper for explicit format string dumps.
#[pyfunction(name = "_dumps")]
#[pyo3(signature = (network, format, version = None))]
pub fn dumps(network: &Network, format: &str, version: Option<u32>) -> PyResult<String> {
    catch_panic("_dumps", || dump_to_string(&network.inner, format, version))
}

/// Internal package helper for explicit format byte dumps.
#[pyfunction(name = "_dumps_bytes")]
pub fn dumps_bytes(network: &Network, format: &str) -> PyResult<Vec<u8>> {
    catch_panic("_dumps_bytes", || dump_to_bytes(&network.inner, format))
}

/// Internal package helper for Surge JSON file saves with explicit pretty control.
#[pyfunction(name = "_io_json_save")]
#[pyo3(signature = (network, path, pretty = false))]
pub fn io_json_save(network: &Network, path: &str, pretty: bool) -> PyResult<()> {
    catch_panic("_io_json_save", || {
        let path = PathBuf::from(path);
        if pretty {
            surge_io::json::save_pretty(&network.inner, &path).map_err(to_io_pyerr)
        } else {
            surge_io::json::save(&network.inner, &path).map_err(to_io_pyerr)
        }
    })
}

/// Internal package helper for Surge JSON string dumps with explicit pretty control.
#[pyfunction(name = "_io_json_dumps")]
#[pyo3(signature = (network, pretty = false))]
pub fn io_json_dumps(network: &Network, pretty: bool) -> PyResult<String> {
    catch_panic("_io_json_dumps", || {
        if pretty {
            surge_io::json::dumps_pretty(&network.inner).map_err(to_io_pyerr)
        } else {
            surge_io::json::dumps(&network.inner).map_err(to_io_pyerr)
        }
    })
}

#[pyclass(name = "_CgmesProfiles", get_all, skip_from_py_object)]
#[derive(Clone)]
pub struct CgmesProfiles {
    eq: String,
    tp: String,
    ssh: String,
    sv: String,
    sc: Option<String>,
    me: Option<String>,
    asset: Option<String>,
    ol: Option<String>,
    bd: Option<String>,
    pr: Option<String>,
    no: Option<String>,
}

/// Internal package helper for explicit CGMES directory saves.
#[pyfunction(name = "_io_cgmes_save")]
#[pyo3(signature = (network, output_dir, version = "2.4.15"))]
pub fn io_cgmes_save(network: &Network, output_dir: &str, version: &str) -> PyResult<()> {
    catch_panic("_io_cgmes_save", || {
        let version = parse_cgmes_version(version)?;
        surge_io::cgmes::save(&network.inner, output_dir, version).map_err(to_io_pyerr)
    })
}

/// Internal package helper for in-memory CGMES profile generation.
#[pyfunction(name = "_io_cgmes_to_profiles")]
#[pyo3(signature = (network, version = "2.4.15"))]
pub fn io_cgmes_to_profiles(network: &Network, version: &str) -> PyResult<CgmesProfiles> {
    catch_panic("_io_cgmes_to_profiles", || {
        let version = parse_cgmes_version(version)?;
        let profiles =
            surge_io::cgmes::to_profiles(&network.inner, version).map_err(to_io_pyerr)?;
        Ok(CgmesProfiles {
            eq: profiles.eq,
            tp: profiles.tp,
            ssh: profiles.ssh,
            sv: profiles.sv,
            sc: profiles.sc,
            me: profiles.me,
            asset: profiles.asset,
            ol: profiles.ol,
            bd: profiles.bd,
            pr: profiles.pr,
            no: profiles.no,
        })
    })
}

/// Internal package helper for CSV network export.
#[pyfunction(name = "_io_export_write_network_csv")]
pub fn io_export_write_network_csv(network: &Network, output_dir: &str) -> PyResult<()> {
    catch_panic("_io_export_write_network_csv", || {
        surge_io::export::write_network_csv(&network.inner, &PathBuf::from(output_dir))
            .map_err(to_io_pyerr)
    })
}

/// Internal package helper for solution snapshot export.
#[pyfunction(name = "_io_export_write_solution_snapshot")]
pub fn io_export_write_solution_snapshot(
    solution: &AcPfResult,
    network: &Network,
    output_path: &str,
) -> PyResult<()> {
    catch_panic("_io_export_write_solution_snapshot", || {
        surge_io::export::write_solution_snapshot(
            &solution.inner,
            &network.inner,
            &PathBuf::from(output_path),
        )
        .map_err(to_io_pyerr)
    })
}

/// Internal package helper for bus coordinate enrichment.
#[pyfunction(name = "_io_geo_apply_bus_coordinates")]
pub fn io_geo_apply_bus_coordinates(network: &mut Network, csv_path: &str) -> PyResult<usize> {
    catch_panic("_io_geo_apply_bus_coordinates", || {
        surge_io::geo::apply_bus_coordinates(
            Arc::make_mut(&mut network.inner),
            &PathBuf::from(csv_path),
        )
        .map_err(to_io_pyerr)
    })
}

#[pyclass(name = "_SeqStats", get_all, skip_from_py_object)]
#[derive(Clone)]
pub struct SeqStats {
    machines_updated: usize,
    branches_updated: usize,
    transformers_updated: usize,
    mutual_couplings: usize,
    skipped_records: usize,
}

#[pymethods]
impl SeqStats {
    fn __repr__(&self) -> String {
        format!(
            "SeqStats(machines_updated={}, branches_updated={}, transformers_updated={}, mutual_couplings={}, skipped_records={})",
            self.machines_updated,
            self.branches_updated,
            self.transformers_updated,
            self.mutual_couplings,
            self.skipped_records,
        )
    }
}

/// Internal package helper for applying PSS/E sequence data from a file.
#[pyfunction(name = "_io_psse_sequence_apply")]
pub fn io_psse_sequence_apply(network: &mut Network, path: &str) -> PyResult<SeqStats> {
    catch_panic("_io_psse_sequence_apply", || {
        let net = Arc::make_mut(&mut network.inner);
        let stats = surge_io::psse::sequence::apply(net, path).map_err(to_io_pyerr)?;
        Ok(SeqStats {
            machines_updated: stats.machines_updated,
            branches_updated: stats.branches_updated,
            transformers_updated: stats.transformers_updated,
            mutual_couplings: stats.mutual_couplings,
            skipped_records: stats.skipped_records,
        })
    })
}

/// Internal package helper for applying PSS/E sequence data from a string.
#[pyfunction(name = "_io_psse_sequence_apply_text")]
pub fn io_psse_sequence_apply_text(network: &mut Network, content: &str) -> PyResult<SeqStats> {
    catch_panic("_io_psse_sequence_apply_text", || {
        let net = Arc::make_mut(&mut network.inner);
        let stats = surge_io::psse::sequence::apply_text(net, content).map_err(to_io_pyerr)?;
        Ok(SeqStats {
            machines_updated: stats.machines_updated,
            branches_updated: stats.branches_updated,
            transformers_updated: stats.transformers_updated,
            mutual_couplings: stats.mutual_couplings,
            skipped_records: stats.skipped_records,
        })
    })
}

#[pyclass(name = "_DynamicModel", skip_from_py_object)]
#[derive(Clone)]
pub struct DynamicModel {
    pub(crate) inner: Arc<CoreDynamicModel>,
}

#[pymethods]
impl DynamicModel {
    #[new]
    fn py_new() -> Self {
        Self {
            inner: Arc::new(CoreDynamicModel::default()),
        }
    }

    #[getter]
    fn generator_count(&self) -> usize {
        self.inner.n_generators()
    }

    #[getter]
    fn exciter_count(&self) -> usize {
        self.inner.n_exciters()
    }

    #[getter]
    fn governor_count(&self) -> usize {
        self.inner.n_governors()
    }

    #[getter]
    fn pss_count(&self) -> usize {
        self.inner.n_pss()
    }

    #[getter]
    fn load_count(&self) -> usize {
        self.inner.n_loads()
    }

    #[getter]
    fn facts_count(&self) -> usize {
        self.inner.n_facts()
    }

    #[getter]
    fn unknown_record_count(&self) -> usize {
        self.inner.unknown_records.len()
    }

    fn coverage(&self) -> (usize, usize, f64) {
        self.inner.coverage()
    }

    fn __repr__(&self) -> String {
        let (supported, total, pct) = self.inner.coverage();
        format!(
            "DynamicModel(total={}, supported={}, coverage={:.1}%)",
            total, supported, pct
        )
    }
}

/// Internal package helper for loading a PSS/E DYR file.
#[pyfunction(name = "_io_psse_dyr_load")]
pub fn io_psse_dyr_load(path: &str) -> PyResult<DynamicModel> {
    catch_panic("_io_psse_dyr_load", || {
        let inner = surge_io::psse::dyr::load(path).map_err(to_io_pyerr)?;
        Ok(DynamicModel {
            inner: Arc::new(inner),
        })
    })
}

/// Internal package helper for loading PSS/E DYR content from a string.
#[pyfunction(name = "_io_psse_dyr_loads")]
pub fn io_psse_dyr_loads(content: &str) -> PyResult<DynamicModel> {
    catch_panic("_io_psse_dyr_loads", || {
        let inner = surge_io::psse::dyr::loads(content).map_err(to_io_pyerr)?;
        Ok(DynamicModel {
            inner: Arc::new(inner),
        })
    })
}

/// Internal package helper for saving a PSS/E DYR model.
#[pyfunction(name = "_io_psse_dyr_save")]
pub fn io_psse_dyr_save(model: &DynamicModel, path: &str) -> PyResult<()> {
    catch_panic("_io_psse_dyr_save", || {
        surge_io::psse::dyr::save(&model.inner, path).map_err(to_io_pyerr)
    })
}

/// Internal package helper for serializing a PSS/E DYR model.
#[pyfunction(name = "_io_psse_dyr_dumps")]
pub fn io_psse_dyr_dumps(model: &DynamicModel) -> PyResult<String> {
    catch_panic("_io_psse_dyr_dumps", || {
        surge_io::psse::dyr::dumps(&model.inner).map_err(to_io_pyerr)
    })
}

/// Internal package helper for unit conversion.
#[pyfunction(name = "_units_ohm_to_pu")]
#[pyo3(signature = (ohm, base_kv, base_mva = 100.0))]
pub fn ohm_to_pu(ohm: f64, base_kv: f64, base_mva: f64) -> PyResult<f64> {
    catch_panic("_units_ohm_to_pu", || {
        if !ohm.is_finite() {
            return Err(PyValueError::new_err(format!(
                "ohm must be finite, got {ohm}"
            )));
        }
        if !base_kv.is_finite() || base_kv <= 0.0 {
            return Err(PyValueError::new_err(format!(
                "base_kv must be a finite positive number, got {base_kv}"
            )));
        }
        if !base_mva.is_finite() || base_mva <= 0.0 {
            return Err(PyValueError::new_err(format!(
                "base_mva must be a finite positive number, got {base_mva}"
            )));
        }
        let z_base = base_kv * base_kv / base_mva;
        Ok(ohm / z_base)
    })
}

/// Internal package helper for network composition.
#[pyfunction(name = "_compose_merge_networks")]
#[pyo3(signature = (net1, net2, tie_buses = None))]
pub fn merge_networks(
    net1: &Network,
    net2: &Network,
    tie_buses: Option<Vec<(u32, u32)>>,
) -> PyResult<Network> {
    catch_panic("_compose_merge_networks", || {
        let n1 = &net1.inner;
        let n2 = &net2.inner;

        let max_bus_n1 = n1.buses.iter().map(|b| b.number).max().unwrap_or(0);
        let offset = ((max_bus_n1 / 10000) + 1) * 10000;
        let tie_map: std::collections::HashMap<u32, u32> =
            tie_buses.unwrap_or_default().into_iter().collect();

        let remap = |bus: u32| -> u32 { tie_map.get(&bus).copied().unwrap_or(bus + offset) };

        let mut merged = surge_network::Network::new(&format!("{}+{}", n1.name, n2.name));
        merged.base_mva = n1.base_mva;
        merged.freq_hz = n1.freq_hz;
        merged.buses = n1.buses.clone();

        for bus in &n2.buses {
            if let Some(&target_bus) = tie_map.get(&bus.number) {
                // Loads are aggregated via Load objects (remapped below).
                // Only aggregate bus-level shunt fields here.
                if let Some(target) = merged.buses.iter_mut().find(|b| b.number == target_bus) {
                    target.shunt_conductance_mw += bus.shunt_conductance_mw;
                    target.shunt_susceptance_mvar += bus.shunt_susceptance_mvar;
                }
            } else {
                let mut cloned = bus.clone();
                cloned.number = remap(bus.number);
                merged.buses.push(cloned);
            }
        }

        merged.branches = n1.branches.clone();
        for branch in &n2.branches {
            let mut cloned = branch.clone();
            cloned.from_bus = remap(branch.from_bus);
            cloned.to_bus = remap(branch.to_bus);
            merged.branches.push(cloned);
        }

        merged.generators = n1.generators.clone();
        for generator in &n2.generators {
            let mut cloned = generator.clone();
            cloned.bus = remap(generator.bus);
            merged.generators.push(cloned);
        }

        merged.loads = n1.loads.clone();
        for load in &n2.loads {
            let mut cloned = load.clone();
            cloned.bus = remap(load.bus);
            merged.loads.push(cloned);
        }

        merged.area_schedules = n1.area_schedules.clone();
        merged
            .area_schedules
            .extend(n2.area_schedules.iter().cloned());

        merged.hvdc.links = n1.hvdc.links.clone();
        for link in &n2.hvdc.links {
            let mut cloned = link.clone();
            match &mut cloned {
                surge_network::network::HvdcLink::Lcc(line) => {
                    line.rectifier.bus = remap(line.rectifier.bus);
                    line.inverter.bus = remap(line.inverter.bus);
                }
                surge_network::network::HvdcLink::Vsc(vsc) => {
                    vsc.converter1.bus = remap(vsc.converter1.bus);
                    vsc.converter2.bus = remap(vsc.converter2.bus);
                }
            }
            merged.hvdc.links.push(cloned);
        }

        merged.hvdc.dc_grids = n1.hvdc.dc_grids.clone();
        let dc_grid_offset = merged.hvdc.next_dc_grid_id() - 1;
        let dc_bus_offset = merged.hvdc.next_dc_bus_id() - 1;
        for grid in &n2.hvdc.dc_grids {
            let mut cloned = grid.clone();
            cloned.id += dc_grid_offset;
            let mut dc_bus_map = std::collections::HashMap::new();
            for bus in &mut cloned.buses {
                let new_bus = bus.bus_id + dc_bus_offset;
                dc_bus_map.insert(bus.bus_id, new_bus);
                bus.bus_id = new_bus;
            }
            for converter in &mut cloned.converters {
                *converter.ac_bus_mut() = remap(converter.ac_bus());
                if let Some(&dc_bus) = dc_bus_map.get(&converter.dc_bus()) {
                    *converter.dc_bus_mut() = dc_bus;
                }
            }
            for branch in &mut cloned.branches {
                if let Some(&from_bus) = dc_bus_map.get(&branch.from_bus) {
                    branch.from_bus = from_bus;
                }
                if let Some(&to_bus) = dc_bus_map.get(&branch.to_bus) {
                    branch.to_bus = to_bus;
                }
            }
            merged.hvdc.dc_grids.push(cloned);
        }

        merged.fixed_shunts = n1.fixed_shunts.clone();
        for shunt in &n2.fixed_shunts {
            let mut cloned = shunt.clone();
            cloned.bus = remap(shunt.bus);
            merged.fixed_shunts.push(cloned);
        }

        Ok(wrap_network(merged))
    })
}
