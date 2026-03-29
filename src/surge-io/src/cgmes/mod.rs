// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES/CIM power system format reader.
//!
//! Parses CGMES 2.4.15 (cim16 namespace) RDF/XML profile files and assembles
//! them into a `Network`. Multiple profile files (EQ, TP, SSH, SV) can be
//! passed together and are merged before building the network.
//!
//! ## Profile roles
//! - **EQ** — Equipment: buses, lines, transformers, generators, loads, shunts
//! - **TP** — Topology: `Terminal.TopologicalNode` assignments
//! - **SSH** — Steady-State Hypothesis: operating set-points (Pg, Pd, Vs)
//! - **SV** — State Variables: solved voltage/flow results
//!
//! ## Unit conventions
//! CGMES stores impedances in **physical ohms / siemens**, voltages in **kV**.
//! This module converts to per-unit (100 MVA base) using the `BaseVoltage`
//! referenced directly on the conducting equipment.
//!
//! ## Two-pass architecture
//! Stage 1 streams all XML files into a unified `CimObj` hash map keyed by mRID.
//! Objects in later files (SSH/SV) overwrite same-key attributes from earlier
//! files (EQ/TP), implementing the CGMES profile-merge semantics.
//! Stage 2 builds `Network` structs from the merged map using pre-built indices
//! for O(1) equipment→terminal and TN→voltage lookups.
//!
//! ## Security notes (CIM-04)
//! This parser uses **quick-xml 0.37** which does **not** automatically expand
//! user-defined XML entities.  Any `<!ENTITY ...>` declaration in a DOCTYPE is
//! silently ignored.  Consequently, "Billion Laughs" (exponential entity
//! expansion) attacks are not possible through this code path — no DTD
//! processing takes place.  The object-count hard cap (`MAX_CIM_OBJECTS`) guards
//! against heap exhaustion from pathologically large well-formed CIM files.

pub(crate) mod ac_network;
pub(crate) mod areas;
pub(crate) mod asset_info;
pub(crate) mod boundary;
pub(crate) mod dc_network;
pub mod dynamics;
pub(crate) mod error;
pub mod ext;
pub(crate) mod gen_load;
pub(crate) mod geographic;
pub(crate) mod grounding;
pub(crate) mod helpers;
pub(crate) mod indices;
pub(crate) mod load_response;
pub(crate) mod measurement;
pub mod merge;
pub(crate) mod net_ops;
pub(crate) mod op_limits;
pub(crate) mod protection;
pub(crate) mod short_circuit;
pub(crate) mod substation;
pub(crate) mod topology;
pub(crate) mod types;
mod writer;
pub(crate) mod xml_parse;

#[cfg(test)]
mod tests;

pub use error::CgmesError as Error;
pub(crate) use error::CgmesError;
pub(crate) use types::{CimObj, ObjMap, SmBusMap};
pub use writer::{
    CgmesProfiles as Profiles, CgmesVersion as Version, CgmesWriteError as SaveError,
};
pub(crate) use xml_parse::collect_objects;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use surge_network::Network;
use surge_network::network::BusType;

use ac_network::build_network;
use areas::{build_area_schedules, build_regions, build_scheduled_area_transfers};
use asset_info::build_asset_catalog;
use boundary::build_boundary_data;
use dc_network::group_dc_into_grids;
use gen_load::{assign_slack, build_generators_and_loads};
use geographic::build_geo_locations;
use grounding::{build_grounding, build_phase_impedances};
use indices::CgmesIndices;
use load_response::build_load_response_chars;
use measurement::build_measurements;
use net_ops::build_network_operations;
use op_limits::build_operational_limits;
use protection::build_protection_data;
use short_circuit::build_short_circuit_data;
use substation::{build_substation_topology, build_topology_mapping};
use topology::reduce_topology;

/// Load a CGMES dataset from a directory, zip archive, or single XML/CIM file.
pub fn load(path: impl AsRef<Path>) -> Result<Network, Error> {
    let path = path.as_ref();
    if path.is_dir() {
        let files = collect_profile_paths(path)?;
        let refs: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
        return load_all(&refs);
    }

    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "xml" | "cim" => load_all(&[path]),
        "zip" => load_zip_archive(path),
        other => Err(Error::UnsupportedInput(format!(
            "{other}; expected a directory, .xml/.cim file, or .zip archive"
        ))),
    }
}

/// Load a CGMES dataset together with an external boundary set.
///
/// Some CGMES files (especially ENTSO-E IGMs) reference `BaseVoltage` and other
/// objects from a separate Boundary Equipment (EQBD) / Boundary Topology (TPBD)
/// profile set.  Without these objects, TopologicalNodes cannot resolve their
/// base voltage and the network degrades to 0-bus or incorrect per-unit values.
///
/// This function collects XML files from both `igm_path` and `boundary_path`
/// (each may be a directory, single file, or zip) and merges them before
/// building the network.  Boundary files are loaded first so that IGM profiles
/// can override attributes on shared mRIDs.
pub fn load_with_boundary(
    igm_path: impl AsRef<Path>,
    boundary_path: impl AsRef<Path>,
) -> Result<Network, Error> {
    let mut tempdirs = Vec::new();
    let mut all_files: Vec<PathBuf> = Vec::new();

    // Collect boundary files first (lower merge priority).
    let bp = boundary_path.as_ref();
    let (bp_tempdirs, bp_files) = collect_profile_inputs(bp)?;
    tempdirs.extend(bp_tempdirs);
    all_files.extend(bp_files);

    // Collect IGM files second (higher merge priority — later files overwrite).
    let ip = igm_path.as_ref();
    let (ip_tempdirs, ip_files) = collect_profile_inputs(ip)?;
    tempdirs.extend(ip_tempdirs);
    all_files.extend(ip_files);

    if all_files.is_empty() {
        return Err(Error::UnsupportedInput(
            "no XML/CIM files found in IGM or boundary paths".to_string(),
        ));
    }

    let refs: Vec<&Path> = all_files.iter().map(PathBuf::as_path).collect();
    load_all(&refs)
}

/// Load and merge one or more CGMES XML profile files.
pub fn load_all(paths: &[&Path]) -> Result<Network, Error> {
    // Parse each profile file independently in parallel, then merge in index order.
    // Later files (SSH, SV) intentionally overwrite attributes from EQ/TP for the
    // same mRID, so we sort by original index before merging.
    let mut parts: Vec<(usize, ObjMap)> = paths
        .par_iter()
        .enumerate()
        .map(|(i, path)| {
            let content = std::fs::read_to_string(path)?;
            let mut objects = ObjMap::new();
            collect_objects(&content, &mut objects)?;
            Ok((i, objects))
        })
        .collect::<Result<Vec<_>, Error>>()?;

    parts.sort_unstable_by_key(|(i, _)| *i);

    let mut objects: ObjMap = HashMap::new();
    for (_, partial) in parts {
        for (id, obj) in partial {
            let entry = objects.entry(id).or_insert_with(|| CimObj::new(&obj.class));
            entry.class = obj.class;
            entry.attrs.extend(obj.attrs);
        }
    }

    build_from_objects(objects)
}

/// Load a single CGMES XML string (single profile or merged document).
pub fn loads(content: &str) -> Result<Network, Error> {
    let mut objects: ObjMap = HashMap::new();
    collect_objects(content, &mut objects)?;
    build_from_objects(objects)
}

/// Save all available CGMES profiles to `output_dir`.
pub fn save(
    network: &Network,
    output_dir: impl AsRef<Path>,
    version: Version,
) -> Result<(), SaveError> {
    writer::write_all_profiles(network, output_dir.as_ref(), version)
}

/// Build all available CGMES profiles in memory.
pub fn to_profiles(network: &Network, version: Version) -> Result<Profiles, SaveError> {
    writer::to_profiles(network, version)
}

#[cfg(test)]
use self::{load_all as parse_files, loads as parse_str};

/// Build a mapping from SynchronousMachine mRID → `(bus_number, machine_id)`.
///
/// This is used by the CGMES DY profile parser (`cim_dy`) to link dynamic model
/// objects (governor, exciter, PSS) back to their generator bus/machine.
///
/// The `machine_id` is derived from `IdentifiedObject.name` on the SM, or falls
/// back to the mRID string itself when no name is available.
pub(crate) fn build_sm_bus_map(objects: &ObjMap) -> HashMap<String, (u32, String)> {
    // We need the full index to resolve TN→bus. Build it here.
    let mut objects_for_idx = objects.clone();
    reduce_topology(&mut objects_for_idx);
    let idx = CgmesIndices::build(&objects_for_idx);

    let mut sm_bus_map: HashMap<String, (u32, String)> = HashMap::new();

    for (sm_id, obj) in objects_for_idx
        .iter()
        .filter(|(_, o)| o.class == "SynchronousMachine")
    {
        let bus_num = idx
            .terminals(sm_id)
            .iter()
            .find_map(|tid| {
                let tn = idx.terminal_tn(&objects_for_idx, tid)?;
                idx.tn_bus(tn)
            })
            .or_else(|| {
                obj.get_ref("EquipmentContainer").and_then(|vl_id| {
                    idx.tn_ids
                        .iter()
                        .find(|tn_id| {
                            objects_for_idx
                                .get(tn_id.as_str())
                                .and_then(|o2| o2.get_ref("ConnectivityNodeContainer"))
                                .map(|c| c == vl_id)
                                .unwrap_or(false)
                        })
                        .and_then(|tn_id| idx.tn_bus(tn_id))
                })
            });

        if let Some(bus_num) = bus_num {
            // machine_id: prefer IdentifiedObject.name, fall back to mRID
            let machine_id = obj
                .get_text("name")
                .map(|s| s.to_string())
                .unwrap_or_else(|| sm_id.clone());
            sm_bus_map.insert(sm_id.clone(), (bus_num, machine_id));
        }
    }

    sm_bus_map
}

/// Parse CGMES EQ/SSH XML files and return both the Network and the SM mRID → bus map.
///
/// This is a convenience wrapper for callers (like the DY profile parser) that need
/// both the network and the SM map in a single call.
#[allow(dead_code)]
pub(crate) fn parse_files_with_sm_map(paths: &[&Path]) -> Result<(Network, SmBusMap), Error> {
    let mut parts: Vec<(usize, ObjMap)> = paths
        .par_iter()
        .enumerate()
        .map(|(i, path)| {
            let content = std::fs::read_to_string(path)?;
            let mut objects = ObjMap::new();
            collect_objects(&content, &mut objects)?;
            Ok((i, objects))
        })
        .collect::<Result<Vec<_>, Error>>()?;

    parts.sort_unstable_by_key(|(i, _)| *i);

    let mut objects: ObjMap = HashMap::new();
    for (_, partial) in parts {
        for (id, obj) in partial {
            let entry = objects.entry(id).or_insert_with(|| CimObj::new(&obj.class));
            entry.class = obj.class;
            for (k, v) in obj.attrs {
                entry.attrs.insert(k, v);
            }
        }
    }

    let sm_bus_map = build_sm_bus_map(&objects);
    let network = build_from_objects(objects)?;
    Ok((network, sm_bus_map))
}

fn build_from_objects(mut objects: ObjMap) -> Result<Network, Error> {
    reduce_topology(&mut objects);

    if requires_ssh_operating_point(&objects) && !has_ssh_profile_data(&objects) {
        return Err(Error::MissingSshProfile);
    }

    let mut idx = CgmesIndices::build(&objects);
    let missing_base_voltages = collect_missing_base_voltage_references(&objects, &idx);
    if !missing_base_voltages.is_empty() {
        return Err(Error::MissingBaseVoltageReferences {
            count: missing_base_voltages.len(),
            examples: missing_base_voltages.into_iter().take(5).collect(),
        });
    }

    let mut network = build_network(&objects, &mut idx)?;
    build_generators_and_loads(&objects, &idx, &mut network);

    retain_main_island(&mut network);
    assign_slack(&objects, &idx, &mut network);
    // Wave 22: ControlArea → area_schedules
    build_area_schedules(&objects, &mut network);
    // Regions from GeographicalRegion/SubGeographicalRegion + bus.zone
    build_regions(&objects, &idx, &mut network);
    // ScheduledAreaTransfer from TieFlow
    build_scheduled_area_transfers(&objects, &idx, &mut network);
    // Group explicit DC topology into canonical dc_grids
    group_dc_into_grids(&mut network);
    // Wave 25: PerLengthPhaseImpedance + MutualCoupling
    build_phase_impedances(&objects, &mut network);
    // Wave 26: Ground + GroundingImpedance + PetersenCoil
    build_grounding(&objects, &idx, &mut network);
    // Wave 27: LoadResponseCharacteristic (ZIP load model)
    build_load_response_chars(&objects, &idx, &mut network);
    // Wave 29: Geographic Location (GL profile)
    build_geo_locations(&objects, &mut network);
    // Short Circuit (SC) profile: zero-sequence + negative-sequence impedance data
    build_short_circuit_data(&objects, &idx, &mut network);
    // Measurement profile: Analog/Discrete/Accumulator → Network.cim.measurements
    build_measurements(&objects, &idx, &mut network);
    // Asset/WireInfo profile: conductor, cable, tower, transformer nameplate
    build_asset_catalog(&objects, &idx, &mut network);
    // Operational Limits: full IEC 61970-302 hierarchy (PATL/TATL/IATL, all limit kinds)
    build_operational_limits(&objects, &idx, &mut network);
    // Boundary profile (EQBD/BD): BoundaryPoint, ModelAuthoritySet, EquivalentNetwork/Branch/Shunt
    build_boundary_data(&objects, &idx, &mut network);
    // Protection profile: CurrentRelay, DistanceRelay, RecloseSequence, SynchrocheckRelay
    build_protection_data(&objects, &idx, &mut network);
    build_network_operations(&objects, &idx, &mut network);

    // Node-breaker topology preservation: extract NodeBreakerTopology + TopologyMapping.
    network.topology = build_substation_topology(&objects, &idx);
    if let Some(ref mut sm) = network.topology {
        sm.install_mapping(build_topology_mapping(&objects, &idx, sm));
    }
    if let Some((switches, connectivity_nodes, substations)) = network.topology.as_ref().map(|sm| {
        (
            sm.switches.len(),
            sm.connectivity_nodes.len(),
            sm.substations.len(),
        )
    }) {
        network.rebuild_bus_state_from_explicit_equipment();
        tracing::info!(
            switches,
            connectivity_nodes,
            substations,
            "NodeBreakerTopology preserved from CGMES"
        );
    }

    // --- Wire ConditionalLimit data into Network.conditional_limits ---
    //
    // Build a reverse map: equipment mRID → branch index using br.circuit
    // (which we set to the CGMES equipment mRID during branch building).
    if !idx.conditional_thermal_limits.is_empty() {
        let mut eq_to_branch: HashMap<&str, surge_network::network::BranchEquipmentKey> =
            HashMap::new();
        for br in &network.branches {
            if !br.circuit.is_empty() {
                eq_to_branch.insert(
                    &br.circuit,
                    surge_network::network::BranchEquipmentKey::from_branch(br),
                );
            }
        }
        for (eq_id, cond_entries) in &idx.conditional_thermal_limits {
            if let Some(branch_key) = eq_to_branch.get(eq_id.as_str()) {
                let ratings: Vec<surge_network::network::ConditionalRating> = cond_entries
                    .iter()
                    .map(
                        |(cond_id, mva, is_emerg)| surge_network::network::ConditionalRating {
                            condition_id: cond_id.clone(),
                            rating_a_mva: if *is_emerg { 0.0 } else { *mva },
                            rating_c_mva: if *is_emerg { *mva } else { 0.0 },
                        },
                    )
                    .collect();
                network
                    .conditional_limits
                    .insert(branch_key.clone(), ratings);
            }
        }
        if !network.conditional_limits.is_empty() {
            tracing::info!(
                branches = network.conditional_limits.len(),
                conditions = network
                    .conditional_limits
                    .values()
                    .map(|v| v.len())
                    .sum::<usize>(),
                "ConditionalLimit → Network.conditional_limits wired"
            );
        }
    }

    // Post-parse diagnostics: log a concise network summary at INFO level.
    let total_pg: f64 = network
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| g.p)
        .sum();
    let total_pd: f64 = network.total_load_mw();
    let total_qd: f64 = network
        .loads
        .iter()
        .filter(|l| l.in_service)
        .map(|l| l.reactive_power_demand_mvar)
        .sum();
    let n_pv = network
        .buses
        .iter()
        .filter(|b| b.bus_type == BusType::PV)
        .count();
    let n_slack = network
        .buses
        .iter()
        .filter(|b| b.bus_type == BusType::Slack)
        .count();
    let slack_bus = network.buses.iter().find(|b| b.bus_type == BusType::Slack);
    tracing::info!(
        buses = network.buses.len(),
        branches = network.branches.len(),
        generators = network.generators.len(),
        loads = network.loads.len(),
        pv_buses = n_pv,
        slack_buses = n_slack,
        slack_bus_num = slack_bus.map(|b| b.number).unwrap_or(0),
        total_pg_mw = format!("{:.1}", total_pg),
        total_pd_mw = format!("{:.1}", total_pd),
        total_qd_mvar = format!("{:.1}", total_qd),
        balance_mw = format!("{:.1}", total_pg - total_pd),
        "CGMES network parsed",
    );
    Ok(network)
}

fn has_ssh_profile_data(objects: &ObjMap) -> bool {
    objects.values().any(|o| {
        (o.class == "Terminal" && o.get_text("connected").is_some())
            || ((o.class == "SynchronousMachine"
                || o.class == "EnergyConsumer"
                || o.class == "ConformLoad"
                || o.class == "NonConformLoad"
                || o.class == "EquivalentInjection"
                || o.class == "VsConverter"
                || o.class == "CsConverter")
                && (o.parse_f64("p").is_some() || o.parse_f64("q").is_some()))
            || ((o.class.contains("TapChanger") || o.class == "ShuntCompensator")
                && (o.get_text("step").is_some() || o.get_text("sections").is_some()))
    })
}

fn requires_ssh_operating_point(objects: &ObjMap) -> bool {
    objects.values().any(|o| {
        matches!(
            o.class.as_str(),
            "SynchronousMachine"
                | "EnergyConsumer"
                | "ConformLoad"
                | "NonConformLoad"
                | "EquivalentInjection"
                | "VsConverter"
                | "CsConverter"
                | "ShuntCompensator"
        ) || o.class.contains("TapChanger")
    })
}

fn collect_missing_base_voltage_references(objects: &ObjMap, idx: &CgmesIndices) -> Vec<String> {
    let mut missing = Vec::new();

    for (id, obj) in objects {
        if let Some(base_voltage_id) = obj.get_ref("BaseVoltage")
            && !idx.bv_kv.contains_key(base_voltage_id)
        {
            missing.push(format!(
                "{}:{} -> BaseVoltage:{}",
                obj.class, id, base_voltage_id
            ));
        }

        if obj.class == "TopologicalNode" {
            let direct_ok = obj
                .get_ref("BaseVoltage")
                .is_some_and(|base_voltage_id| idx.bv_kv.contains_key(base_voltage_id));
            let via_voltage_level_ok = obj
                .get_ref("ConnectivityNodeContainer")
                .and_then(|voltage_level_id| idx.vl_bv.get(voltage_level_id))
                .is_some_and(|base_voltage_id| idx.bv_kv.contains_key(base_voltage_id));
            if !direct_ok && !via_voltage_level_ok {
                missing.push(format!(
                    "TopologicalNode:{id} missing resolvable BaseVoltage"
                ));
            }
        }
    }

    missing.sort();
    missing.dedup();
    missing
}

fn is_profile_entry(path_like: &str) -> bool {
    let lower = path_like.to_ascii_lowercase();
    lower.ends_with(".xml") && !lower.contains("diagramlayout")
}

fn collect_profile_paths(path: &Path) -> Result<Vec<PathBuf>, Error> {
    let abs_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&abs_path).map_err(|source| Error::ReadDirectory {
        path: abs_path.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::ReadDirectoryEntry {
            path: abs_path.clone(),
            source,
        })?;
        let entry_path = entry.path();
        if is_profile_entry(&entry_path.to_string_lossy()) {
            files.push(entry_path);
        }
    }
    if files.is_empty() {
        return Err(Error::NoProfiles { path: abs_path });
    }
    files.sort();
    Ok(files)
}

fn collect_profile_inputs(path: &Path) -> Result<(Vec<tempfile::TempDir>, Vec<PathBuf>), Error> {
    if path.is_dir() {
        return Ok((Vec::new(), collect_profile_paths(path)?));
    }

    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "xml" | "cim" => Ok((Vec::new(), vec![path.to_path_buf()])),
        "zip" => {
            let (tmpdir, files) = extract_profile_paths_from_zip(path)?;
            Ok((vec![tmpdir], files))
        }
        _ => Err(Error::UnsupportedInput(format!(
            "{}; expected a directory, .xml/.cim file, or .zip archive",
            path.display()
        ))),
    }
}

fn extract_profile_paths_from_zip(path: &Path) -> Result<(tempfile::TempDir, Vec<PathBuf>), Error> {
    use std::io::Read as _;

    let file = std::fs::File::open(path).map_err(|source| Error::OpenArchive {
        path: path.to_path_buf(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|source| Error::ReadArchive {
        path: path.to_path_buf(),
        source,
    })?;
    let tmpdir = tempfile::TempDir::new().map_err(Error::CreateTempDir)?;
    let mut extracted_paths = std::collections::HashSet::new();
    let mut xml_paths = Vec::new();

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|source| Error::ReadArchiveEntry {
                path: path.to_path_buf(),
                source,
            })?;
        let entry_name = entry.name().to_string();
        if !is_profile_entry(&entry_name) {
            continue;
        }
        let rel_path = entry
            .enclosed_name()
            .ok_or_else(|| Error::InvalidArchiveEntryPath {
                archive_path: path.to_path_buf(),
                entry_name: entry_name.clone(),
            })?;
        if !extracted_paths.insert(rel_path.to_path_buf()) {
            return Err(Error::DuplicateArchiveEntryPath {
                archive_path: path.to_path_buf(),
                entry_name: rel_path.display().to_string(),
            });
        }
        let out_path = tmpdir.path().join(rel_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::ExtractArchiveEntry {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|source| Error::ExtractArchiveEntry {
                path: path.to_path_buf(),
                source,
            })?;
        std::fs::write(&out_path, &buf).map_err(|source| Error::ExtractArchiveEntry {
            path: out_path.clone(),
            source,
        })?;
        xml_paths.push(out_path);
    }

    if xml_paths.is_empty() {
        return Err(Error::NoProfiles {
            path: path.to_path_buf(),
        });
    }

    xml_paths.sort();
    Ok((tmpdir, xml_paths))
}

fn load_zip_archive(path: &Path) -> Result<Network, Error> {
    let (tmpdir, xml_paths) = extract_profile_paths_from_zip(path)?;
    let refs: Vec<&Path> = xml_paths.iter().map(PathBuf::as_path).collect();
    let network = load_all(&refs)?;
    let _keep_tmpdir_alive = tmpdir;
    Ok(network)
}

/// Keep only the largest connected island of the network.
///
/// CGMES models (especially assembled CGM/RealGrid) can contain multiple
/// disconnected AC sub-networks:
/// - The main synchronous zone (the AC network we want to solve)
/// - DC busbar stubs (HVDC converter DC sides have no AC connectivity)
/// - Modeling artifacts (TNs defined in TP but with no equipment attached)
///
/// These disconnected islands cause rank-deficient B-matrices in the AC power
/// flow solver. We retain only the largest connected component (by bus count)
/// and remove all other islands, logging the topology for diagnostics.
fn retain_main_island(network: &mut Network) {
    use std::collections::{HashMap, HashSet, VecDeque};

    if network.buses.is_empty() {
        return;
    }

    // Build adjacency list from branches.
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for br in &network.branches {
        adj.entry(br.from_bus).or_default().push(br.to_bus);
        adj.entry(br.to_bus).or_default().push(br.from_bus);
    }

    // BFS to find all connected components.
    let all_buses: HashSet<u32> = network.buses.iter().map(|b| b.number).collect();
    let mut unvisited = all_buses.clone();
    let mut components: Vec<HashSet<u32>> = Vec::new();

    while let Some(&seed) = unvisited.iter().next() {
        let mut comp: HashSet<u32> = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(seed);
        comp.insert(seed);
        unvisited.remove(&seed);
        while let Some(node) = queue.pop_front() {
            for &nb in adj.get(&node).map(|v| v.as_slice()).unwrap_or(&[]) {
                if unvisited.remove(&nb) {
                    comp.insert(nb);
                    queue.push_back(nb);
                }
            }
        }
        components.push(comp);
    }

    if components.len() <= 1 {
        return;
    }

    let largest_idx = components
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| c.len())
        .map(|(i, _)| i)
        .unwrap_or(0);

    let main_island = &components[largest_idx];
    let total_buses = network.buses.len();
    let n_islands = components.len();

    let mut island_sizes: Vec<usize> = components
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != largest_idx)
        .map(|(_, c)| c.len())
        .collect();
    island_sizes.sort_unstable_by(|a, b| b.cmp(a));

    let n_removed: usize = island_sizes.iter().sum();

    tracing::warn!(
        n_islands,
        main_island_buses = main_island.len(),
        removed_buses = n_removed,
        total_buses,
        "CGMES: {} disconnected AC islands detected; retaining largest ({} buses), \
         removing {} buses in {} smaller island(s) (sizes: {:?})",
        n_islands,
        main_island.len(),
        n_removed,
        n_islands - 1,
        island_sizes,
    );

    let dropped_gens: Vec<u32> = network
        .generators
        .iter()
        .filter(|g| !main_island.contains(&g.bus))
        .map(|g| g.bus)
        .collect();
    if !dropped_gens.is_empty() {
        tracing::debug!(
            count = dropped_gens.len(),
            buses = ?dropped_gens,
            "CGMES retain_main_island: dropping generators on disconnected island buses"
        );
    }
    let dropped_loads: Vec<u32> = network
        .loads
        .iter()
        .filter(|l| !main_island.contains(&l.bus))
        .map(|l| l.bus)
        .collect();
    if !dropped_loads.is_empty() {
        tracing::debug!(
            count = dropped_loads.len(),
            buses = ?dropped_loads,
            "CGMES retain_main_island: dropping loads on disconnected island buses"
        );
    }

    // Label buses with island IDs (0 = largest/main, 1+ = smaller).
    let mut sorted_islands: Vec<(usize, &HashSet<u32>)> = components.iter().enumerate().collect();
    sorted_islands.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    for (rank, (_, island)) in sorted_islands.iter().enumerate() {
        for bus in &mut network.buses {
            if island.contains(&bus.number) {
                bus.island_id = rank as u32;
            }
        }
    }

    network.buses.retain(|b| main_island.contains(&b.number));
    network
        .branches
        .retain(|br| main_island.contains(&br.from_bus) && main_island.contains(&br.to_bus));
    network.generators.retain(|g| main_island.contains(&g.bus));
    network.loads.retain(|l| main_island.contains(&l.bus));

    network
        .induction_machines
        .retain(|im| main_island.contains(&im.bus));
    network
        .cim
        .grounding_impedances
        .retain(|gi| main_island.contains(&gi.bus));
    network.metadata.multi_section_line_groups.retain(|g| {
        main_island.contains(&g.from_bus)
            && main_island.contains(&g.to_bus)
            && g.dummy_buses.iter().all(|b| main_island.contains(b))
    });

    network.hvdc.links.retain(|link| match link {
        surge_network::network::HvdcLink::Lcc(link) => {
            main_island.contains(&link.rectifier.bus) && main_island.contains(&link.inverter.bus)
        }
        surge_network::network::HvdcLink::Vsc(link) => {
            main_island.contains(&link.converter1.bus) && main_island.contains(&link.converter2.bus)
        }
    });
    for dc_grid in &mut network.hvdc.dc_grids {
        dc_grid
            .converters
            .retain(|converter| main_island.contains(&converter.ac_bus()));
        let retained_dc_buses: std::collections::HashSet<u32> = dc_grid
            .converters
            .iter()
            .map(|converter| converter.dc_bus())
            .collect();
        dc_grid
            .buses
            .retain(|dc_bus| retained_dc_buses.contains(&dc_bus.bus_id));
        dc_grid.branches.retain(|branch| {
            retained_dc_buses.contains(&branch.from_bus)
                && retained_dc_buses.contains(&branch.to_bus)
        });
    }
    network.hvdc.dc_grids.retain(|dc_grid| !dc_grid.is_empty());

    network.conditional_limits.clear();
    network.controls.switched_shunts.clear();
    network.controls.switched_shunts_opf.clear();
    network.market_data.dispatchable_loads.clear();
    network.facts_devices.clear();
    network.controls.oltc_specs.clear();
    network.controls.par_specs.clear();
    network.flowgates.clear();
    network.interfaces.clear();
    network.nomograms.clear();
    network.cim.operational_limits.limit_sets.clear();
}
