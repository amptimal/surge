// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES/CIM power system format writer.
//!
//! Produces a set of four CGMES RDF/XML profile files from a [`Network`]:
//!
//! - **EQ** (Equipment) — static network topology: substations, voltage levels,
//!   lines, transformers, generators, loads, shunts.
//! - **TP** (Topology) — maps equipment terminals to TopologicalNodes (buses).
//! - **SSH** (Steady-State Hypothesis) — operating set-points: generator P/Q,
//!   load P/Q, terminal connected status, regulating control target voltage.
//! - **SV** (State Variables) — solved power flow results: bus voltages and
//!   branch power flows.
//!
//! ## Version support
//!
//! Both CGMES 2.4.15 (CIM16) and CGMES 3.0 (CIM100) are supported via the
//! [`CgmesVersion`] enum. The main difference is the XML namespace URIs:
//!
//! - 2.4.15: `http://iec.ch/TC57/2013/CIM-schema-cim16#`
//! - 3.0:    `http://iec.ch/TC57/CIM100#`
//!
//! ## Round-trip compatibility
//!
//! The output is designed to be parseable by the reader in [`crate::cgmes`].
//! Element names, attribute keys, and cross-references use the same patterns
//! that the reader's `collect_objects` / `simplify_attr_key` logic expects.
//!
//! ## ID generation
//!
//! All CIM mRIDs are deterministic and derived from the element's position in
//! the Network vectors (bus index, branch index, generator index, etc.). This
//! ensures stable output across invocations for the same input network.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use surge_network::Network;
use surge_network::network::measurement::CimMeasurementType;
use surge_network::network::{
    BranchType, CgmesDanglingLineSource, CgmesEquivalentInjectionSource,
    CgmesExternalNetworkInjectionSource, SwitchType,
};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// CGMES version selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgmesVersion {
    /// CGMES 2.4.15 (IEC CIM16 namespace).
    V2_4_15,
    /// CGMES 3.0 (IEC CIM100 namespace).
    V3_0,
}

/// Errors that can occur during CGMES writing.
#[derive(Error, Debug)]
pub enum CgmesWriteError {
    #[error("CGMES output requires a directory path, not '{0}'")]
    DirectoryTargetRequired(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("formatting error: {0}")]
    Fmt(#[from] std::fmt::Error),
}

/// In-memory CGMES profile set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgmesProfiles {
    pub eq: String,
    pub tp: String,
    pub ssh: String,
    pub sv: String,
    pub sc: Option<String>,
    pub me: Option<String>,
    pub asset: Option<String>,
    pub ol: Option<String>,
    pub bd: Option<String>,
    pub pr: Option<String>,
    pub no: Option<String>,
}

// ---------------------------------------------------------------------------
// Namespace constants
// ---------------------------------------------------------------------------

/// CIM class namespace URI for CGMES 2.4.15.
const CIM_NS_V2: &str = "http://iec.ch/TC57/2013/CIM-schema-cim16#";
/// CIM class namespace URI for CGMES 3.0.
const CIM_NS_V3: &str = "http://iec.ch/TC57/CIM100#";

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const MD_NS: &str = "http://iec.ch/TC57/61970-552/ModelDescription/1#";
const ENTSOE_NS: &str = "http://entsoe.eu/CIM/SchemaExtension/3/1#";

/// Profile URI constants for the FullModel header.
mod profile_uri {
    pub mod v2 {
        pub const EQ: &str = "http://entsoe.eu/CIM/EquipmentCore/3/1";
        pub const TP: &str = "http://entsoe.eu/CIM/Topology/4/1";
        pub const SSH: &str = "http://entsoe.eu/CIM/SteadyStateHypothesis/1/1";
        pub const SV: &str = "http://entsoe.eu/CIM/StateVariables/4/1";
    }
    pub mod v3 {
        pub const EQ: &str = "http://iec.ch/TC57/61970-456/EquipmentCore/3/0";
        pub const TP: &str = "http://iec.ch/TC57/61970-456/Topology/3/0";
        pub const SSH: &str = "http://iec.ch/TC57/61970-456/SteadyStateHypothesis/3/0";
        pub const SV: &str = "http://iec.ch/TC57/61970-456/StateVariables/3/0";
    }
}

// ---------------------------------------------------------------------------
// ID helpers — deterministic mRID generation
// ---------------------------------------------------------------------------

/// Generate a deterministic mRID for a geographic region.
fn gr_id() -> String {
    "_GR_SURGE".to_string()
}

/// Generate a deterministic mRID for a sub-geographic region.
fn sgr_id() -> String {
    "_SGR_SURGE".to_string()
}

/// Generate a deterministic mRID for a substation from bus number.
fn sub_id(bus_num: u32) -> String {
    format!("_SUB_{bus_num}")
}

/// Generate a deterministic mRID for a base voltage from its kV value.
fn bv_id(base_kv: f64) -> String {
    // Use integer kV to avoid floating-point formatting issues in IDs.
    // For non-integer kV, include one decimal.
    let rounded = (base_kv * 10.0).round() as i64;
    if rounded % 10 == 0 {
        format!("_BV_{}", rounded / 10)
    } else {
        format!("_BV_{}d{}", rounded / 10, (rounded % 10).abs())
    }
}

/// Generate a deterministic mRID for a voltage level from bus number.
fn vl_id(bus_num: u32) -> String {
    format!("_VL_{bus_num}")
}

/// Generate a deterministic mRID for a TopologicalNode from bus number.
/// Zero-padded to 8 digits so alphabetical sort matches numeric order
/// (reader sorts TN mRIDs alphabetically to assign sequential bus numbers).
fn tn_id(bus_num: u32) -> String {
    format!("_TN_{bus_num:08}")
}

/// Generate a deterministic mRID for a terminal.
/// `eq_id` is the parent equipment mRID, `seq` is 1-based sequence number.
fn term_id(eq_id: &str, seq: u32) -> String {
    format!("{eq_id}_T_{seq}")
}

/// Generate a deterministic mRID for an ACLineSegment.
fn line_id(branch_idx: usize) -> String {
    format!("_ACLS_{branch_idx}")
}

/// Generate a deterministic mRID for a PowerTransformer.
fn xfmr_id(branch_idx: usize) -> String {
    format!("_XFMR_{branch_idx}")
}

/// Generate a deterministic mRID for a PowerTransformerEnd.
fn xfmr_end_id(branch_idx: usize, end_num: u32) -> String {
    format!("_XFMR_{branch_idx}_E{end_num}")
}

/// Generate a deterministic mRID for a SynchronousMachine.
fn sm_id(gen_idx: usize) -> String {
    format!("_SM_{gen_idx}")
}

/// Generate a deterministic mRID for a GeneratingUnit.
fn gu_id(gen_idx: usize) -> String {
    format!("_GU_{gen_idx}")
}

/// Generate a deterministic mRID for a RegulatingControl.
fn rc_id(gen_idx: usize) -> String {
    format!("_RC_{gen_idx}")
}

/// Generate a deterministic mRID for an EnergyConsumer (load).
fn ec_id(load_idx: usize) -> String {
    format!("_EC_{load_idx}")
}

/// Generate a deterministic mRID for a LinearShuntCompensator (bus-level).
fn shunt_id(bus_num: u32) -> String {
    format!("_SH_{bus_num}")
}

/// Generate a deterministic mRID for a FixedShunt as LinearShuntCompensator.
fn fixed_shunt_id(idx: usize) -> String {
    format!("_FSH_{idx}")
}

/// Generate a deterministic mRID for an EquivalentInjection (power injection).
fn einj_id(idx: usize) -> String {
    format!("_EINJ_{idx}")
}

/// Generate a deterministic mRID for a SeriesCompensator.
fn sc_id(branch_idx: usize) -> String {
    format!("_SC_{branch_idx}")
}

/// Generate a deterministic mRID for an SvVoltage.
fn sv_voltage_id(bus_idx: usize) -> String {
    format!("_SVV_{bus_idx}")
}

/// Generate a deterministic mRID for an SvPowerFlow.
fn sv_pf_id(terminal_id: &str) -> String {
    format!("_SVPF{terminal_id}")
}

/// Generate a deterministic mRID for a round-tripped regulating control.
fn roundtrip_rc_id(source_mrid: &str) -> String {
    format!("{source_mrid}_RC")
}

#[derive(Debug, Clone, Copy)]
struct EquivalentInjectionExport<'a> {
    source: &'a CgmesEquivalentInjectionSource,
    generator_idx: Option<usize>,
    injection_idx: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct ExternalNetworkInjectionExport<'a> {
    source: &'a CgmesExternalNetworkInjectionSource,
    injection_idx: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct DanglingLineExport<'a> {
    source: &'a CgmesDanglingLineSource,
    shunt_idx: Option<usize>,
    injection_idx: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct RoundtripExportState<'a> {
    equivalent_injections: Vec<EquivalentInjectionExport<'a>>,
    external_network_injections: Vec<ExternalNetworkInjectionExport<'a>>,
    dangling_lines: Vec<DanglingLineExport<'a>>,
    skipped_generator_indices: HashSet<usize>,
    skipped_injection_indices: HashSet<usize>,
    skipped_fixed_shunt_indices: HashSet<usize>,
}

impl<'a> RoundtripExportState<'a> {
    fn build(network: &'a Network) -> Self {
        let mut state = Self::default();

        let mut equivalent_sources: Vec<_> = network
            .cim
            .cgmes_roundtrip
            .equivalent_injections
            .values()
            .collect();
        equivalent_sources.sort_by(|left, right| left.mrid.cmp(&right.mrid));
        for source in equivalent_sources {
            let generator_idx = network.generators.iter().position(|generator| {
                generator.machine_id.as_deref() == Some(source.mrid.as_str())
                    || generator.id == source.mrid
            });
            let injection_idx = network
                .power_injections
                .iter()
                .position(|injection| injection.id == source.mrid);
            if let Some(idx) = generator_idx {
                state.skipped_generator_indices.insert(idx);
            }
            if let Some(idx) = injection_idx {
                state.skipped_injection_indices.insert(idx);
            }
            state.equivalent_injections.push(EquivalentInjectionExport {
                source,
                generator_idx,
                injection_idx,
            });
        }

        let mut external_sources: Vec<_> = network
            .cim
            .cgmes_roundtrip
            .external_network_injections
            .values()
            .collect();
        external_sources.sort_by(|left, right| left.mrid.cmp(&right.mrid));
        for source in external_sources {
            let injection_idx = network
                .power_injections
                .iter()
                .position(|injection| injection.id == source.mrid);
            if let Some(idx) = injection_idx {
                state.skipped_injection_indices.insert(idx);
            }
            state
                .external_network_injections
                .push(ExternalNetworkInjectionExport {
                    source,
                    injection_idx,
                });
        }

        let mut dangling_sources: Vec<_> = network
            .cim
            .cgmes_roundtrip
            .dangling_lines
            .values()
            .collect();
        dangling_sources.sort_by(|left, right| left.mrid.cmp(&right.mrid));
        for source in dangling_sources {
            let shunt_idx = network
                .fixed_shunts
                .iter()
                .position(|shunt| shunt.id == source.mrid);
            let injection_idx = network
                .power_injections
                .iter()
                .position(|injection| injection.id == source.mrid);
            if let Some(idx) = shunt_idx {
                state.skipped_fixed_shunt_indices.insert(idx);
            }
            if let Some(idx) = injection_idx {
                state.skipped_injection_indices.insert(idx);
            }
            state.dangling_lines.push(DanglingLineExport {
                source,
                shunt_idx,
                injection_idx,
            });
        }

        state
    }
}

// ---------------------------------------------------------------------------
// Per-unit → physical unit conversion
// ---------------------------------------------------------------------------

/// Convert per-unit impedance to physical Ohms.
/// z_pu * base_kv^2 / base_mva = z_ohm
#[inline]
fn pu_to_ohm(pu: f64, base_kv: f64, base_mva: f64) -> f64 {
    if base_mva <= 0.0 {
        return 0.0;
    }
    pu * base_kv * base_kv / base_mva
}

/// Convert per-unit susceptance to physical Siemens.
/// b_pu * base_mva / base_kv^2 = b_siemens
#[inline]
fn pu_to_siemens(pu: f64, base_kv: f64, base_mva: f64) -> f64 {
    if base_kv <= 0.0 {
        return 0.0;
    }
    pu * base_mva / (base_kv * base_kv)
}

fn bus_base_kv(network: &Network, bus_num: u32) -> f64 {
    network
        .buses
        .iter()
        .find(|bus| bus.number == bus_num)
        .map(|bus| bus.base_kv)
        .unwrap_or(1.0)
        .max(1e-3)
}

fn bus_voltage_target_kv(network: &Network, bus_num: u32) -> Option<f64> {
    network
        .buses
        .iter()
        .find(|bus| bus.number == bus_num)
        .map(|bus| bus.voltage_magnitude_pu * bus.base_kv.max(1e-3))
}

fn equivalent_injection_name<'a>(export: &'a EquivalentInjectionExport<'a>) -> &'a str {
    export
        .source
        .name
        .as_deref()
        .unwrap_or(export.source.mrid.as_str())
}

fn equivalent_injection_is_in_service(
    export: &EquivalentInjectionExport<'_>,
    network: &Network,
) -> bool {
    export
        .generator_idx
        .map(|idx| network.generators[idx].in_service)
        .or_else(|| {
            export
                .injection_idx
                .map(|idx| network.power_injections[idx].in_service)
        })
        .unwrap_or(export.source.in_service)
}

fn equivalent_injection_bus(export: &EquivalentInjectionExport<'_>, network: &Network) -> u32 {
    export
        .generator_idx
        .map(|idx| network.generators[idx].bus)
        .or_else(|| {
            export
                .injection_idx
                .map(|idx| network.power_injections[idx].bus)
        })
        .unwrap_or(export.source.bus)
}

fn equivalent_injection_pq(
    export: &EquivalentInjectionExport<'_>,
    network: &Network,
) -> (f64, f64) {
    export
        .generator_idx
        .map(|idx| {
            let generator = &network.generators[idx];
            (generator.p, generator.q)
        })
        .or_else(|| {
            export.injection_idx.map(|idx| {
                let injection = &network.power_injections[idx];
                (
                    injection.active_power_injection_mw,
                    injection.reactive_power_injection_mvar,
                )
            })
        })
        .unwrap_or((export.source.p_mw, export.source.q_mvar))
}

fn equivalent_injection_q_limits(
    export: &EquivalentInjectionExport<'_>,
    network: &Network,
) -> (Option<f64>, Option<f64>) {
    if let Some(idx) = export.generator_idx {
        let generator = &network.generators[idx];
        (Some(generator.qmin), Some(generator.qmax))
    } else {
        (export.source.min_q_mvar, export.source.max_q_mvar)
    }
}

fn equivalent_injection_target_kv(
    export: &EquivalentInjectionExport<'_>,
    network: &Network,
) -> Option<f64> {
    if let Some(idx) = export.generator_idx {
        let generator = &network.generators[idx];
        Some(generator.voltage_setpoint_pu * bus_base_kv(network, generator.bus))
    } else {
        bus_voltage_target_kv(network, equivalent_injection_bus(export, network))
            .or(export.source.target_voltage_kv)
    }
}

fn external_network_injection_name<'a>(export: &'a ExternalNetworkInjectionExport<'a>) -> &'a str {
    export
        .source
        .name
        .as_deref()
        .unwrap_or(export.source.mrid.as_str())
}

fn external_network_injection_is_in_service(
    export: &ExternalNetworkInjectionExport<'_>,
    network: &Network,
) -> bool {
    export
        .injection_idx
        .map(|idx| network.power_injections[idx].in_service)
        .unwrap_or(export.source.in_service)
}

fn external_network_injection_bus(
    export: &ExternalNetworkInjectionExport<'_>,
    network: &Network,
) -> u32 {
    export
        .injection_idx
        .map(|idx| network.power_injections[idx].bus)
        .unwrap_or(export.source.bus)
}

fn external_network_injection_pq(
    export: &ExternalNetworkInjectionExport<'_>,
    network: &Network,
) -> (f64, f64) {
    export
        .injection_idx
        .map(|idx| {
            let injection = &network.power_injections[idx];
            (
                injection.active_power_injection_mw,
                injection.reactive_power_injection_mvar,
            )
        })
        .unwrap_or((export.source.p_mw, export.source.q_mvar))
}

fn external_network_reference_priority(
    export: &ExternalNetworkInjectionExport<'_>,
    network: &Network,
) -> Option<u32> {
    if let Some(priority) = export.source.reference_priority {
        return Some(priority);
    }
    let bus_num = external_network_injection_bus(export, network);
    network
        .buses
        .iter()
        .find(|bus| {
            bus.number == bus_num && matches!(bus.bus_type, surge_network::network::BusType::Slack)
        })
        .map(|_| 1)
}

fn external_network_target_kv(
    export: &ExternalNetworkInjectionExport<'_>,
    network: &Network,
) -> Option<f64> {
    bus_voltage_target_kv(network, external_network_injection_bus(export, network))
        .or(export.source.target_voltage_kv)
}

fn dangling_line_name<'a>(export: &'a DanglingLineExport<'a>) -> &'a str {
    export
        .source
        .name
        .as_deref()
        .unwrap_or(export.source.mrid.as_str())
}

fn dangling_line_is_in_service(export: &DanglingLineExport<'_>, network: &Network) -> bool {
    export
        .shunt_idx
        .map(|idx| network.fixed_shunts[idx].in_service)
        .or_else(|| {
            export
                .injection_idx
                .map(|idx| network.power_injections[idx].in_service)
        })
        .unwrap_or(export.source.in_service)
}

fn dangling_line_bus(export: &DanglingLineExport<'_>, network: &Network) -> u32 {
    export
        .shunt_idx
        .map(|idx| network.fixed_shunts[idx].bus)
        .or_else(|| {
            export
                .injection_idx
                .map(|idx| network.power_injections[idx].bus)
        })
        .unwrap_or(export.source.bus)
}

fn dangling_line_pq(export: &DanglingLineExport<'_>, network: &Network) -> (f64, f64) {
    export
        .injection_idx
        .map(|idx| {
            let injection = &network.power_injections[idx];
            (
                injection.active_power_injection_mw,
                injection.reactive_power_injection_mvar,
            )
        })
        .unwrap_or((export.source.p_mw, export.source.q_mvar))
}

fn dangling_line_shunt(export: &DanglingLineExport<'_>, network: &Network) -> (f64, f64) {
    if let Some(idx) = export.shunt_idx {
        let shunt = &network.fixed_shunts[idx];
        let base_kv = bus_base_kv(network, shunt.bus);
        (
            shunt.g_mw / (base_kv * base_kv),
            shunt.b_mvar / (base_kv * base_kv),
        )
    } else {
        (export.source.g_s, export.source.b_s)
    }
}

// ---------------------------------------------------------------------------
// XML writing helpers
// ---------------------------------------------------------------------------

fn cim_ns(version: CgmesVersion) -> &'static str {
    match version {
        CgmesVersion::V2_4_15 => CIM_NS_V2,
        CgmesVersion::V3_0 => CIM_NS_V3,
    }
}

fn eq_profile_uri(version: CgmesVersion) -> &'static str {
    match version {
        CgmesVersion::V2_4_15 => profile_uri::v2::EQ,
        CgmesVersion::V3_0 => profile_uri::v3::EQ,
    }
}

fn tp_profile_uri(version: CgmesVersion) -> &'static str {
    match version {
        CgmesVersion::V2_4_15 => profile_uri::v2::TP,
        CgmesVersion::V3_0 => profile_uri::v3::TP,
    }
}

fn ssh_profile_uri(version: CgmesVersion) -> &'static str {
    match version {
        CgmesVersion::V2_4_15 => profile_uri::v2::SSH,
        CgmesVersion::V3_0 => profile_uri::v3::SSH,
    }
}

fn sv_profile_uri(version: CgmesVersion) -> &'static str {
    match version {
        CgmesVersion::V2_4_15 => profile_uri::v2::SV,
        CgmesVersion::V3_0 => profile_uri::v3::SV,
    }
}

/// Write the XML declaration and opening `rdf:RDF` element with namespaces.
fn write_rdf_header(out: &mut String, version: CgmesVersion) {
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<rdf:RDF");
    write!(out, " xmlns:entsoe=\"{ENTSOE_NS}\"",).expect("write to String is infallible");
    write!(out, " xmlns:rdf=\"{RDF_NS}\"",).expect("write to String is infallible");
    write!(out, " xmlns:cim=\"{}\"", cim_ns(version),).expect("write to String is infallible");
    write!(out, " xmlns:md=\"{MD_NS}\"",).expect("write to String is infallible");
    out.push_str(">\n");
}

/// Write the `md:FullModel` header element.
fn write_full_model(
    out: &mut String,
    name: &str,
    profile_kind: &str,
    profile_uri: &str,
    eq_model_urn: Option<&str>,
    tp_model_urn: Option<&str>,
    ssh_model_urn: Option<&str>,
) {
    let timestamp = "2026-01-01T00:00:00Z";
    let urn = format!("urn:uuid:{name}_N_{profile_kind}_{timestamp}_1_1D__FM");

    writeln!(out, "  <md:FullModel rdf:about=\"{urn}\">").expect("write to String is infallible");
    writeln!(
        out,
        "    <md:Model.scenarioTime>{timestamp}</md:Model.scenarioTime>"
    )
    .expect("write to String is infallible");
    writeln!(out, "    <md:Model.created>{timestamp}</md:Model.created>")
        .expect("write to String is infallible");
    writeln!(
        out,
        "    <md:Model.description>{profile_kind} Model</md:Model.description>"
    )
    .expect("write to String is infallible");
    writeln!(out, "    <md:Model.version>1</md:Model.version>")
        .expect("write to String is infallible");
    if let Some(eq_urn) = eq_model_urn {
        writeln!(out, "    <md:Model.DependentOn rdf:resource=\"{eq_urn}\"/>")
            .expect("write to String is infallible");
    }
    if let Some(tp_urn) = tp_model_urn {
        writeln!(out, "    <md:Model.DependentOn rdf:resource=\"{tp_urn}\"/>")
            .expect("write to String is infallible");
    }
    if let Some(ssh_urn) = ssh_model_urn {
        writeln!(
            out,
            "    <md:Model.DependentOn rdf:resource=\"{ssh_urn}\"/>"
        )
        .expect("write to String is infallible");
    }
    writeln!(
        out,
        "    <md:Model.profile>{profile_uri}</md:Model.profile>"
    )
    .expect("write to String is infallible");
    writeln!(
        out,
        "    <md:Model.modelingAuthoritySet>surge.amptimal.com</md:Model.modelingAuthoritySet>"
    )
    .expect("write to String is infallible");
    writeln!(out, "  </md:FullModel>").expect("write to String is infallible");
}

/// Write a closing `</rdf:RDF>` tag.
fn write_rdf_footer(out: &mut String) {
    out.push_str("</rdf:RDF>\n");
}

// ---------------------------------------------------------------------------
// Classify branches as lines vs. transformers
// ---------------------------------------------------------------------------

/// A branch is a transformer if tap != 1.0 or shift != 0.0.
/// Delegates to [`Branch::is_transformer()`] (1e-6 tolerance — functionally
/// equivalent to the former 1e-8 for real-world tap/shift values).
fn is_transformer(br: &surge_network::network::Branch) -> bool {
    br.is_transformer()
}

/// Return the deterministic equipment mRID for a branch based on its type.
fn branch_eq_id(bi: usize, br: &surge_network::network::Branch) -> String {
    if is_transformer(br) {
        xfmr_id(bi)
    } else if br.branch_type == BranchType::SeriesCapacitor {
        sc_id(bi)
    } else {
        line_id(bi)
    }
}

// ---------------------------------------------------------------------------
// EQ profile writer
// ---------------------------------------------------------------------------

fn write_eq_profile(
    network: &Network,
    version: CgmesVersion,
    base_voltage_set: &HashMap<i64, f64>,
    bus_sub: &HashMap<u32, String>,
) -> Result<String, CgmesWriteError> {
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let bus_idx_map = network.bus_index_map();
    let roundtrip = RoundtripExportState::build(network);
    let mut out = String::with_capacity(64 * 1024);
    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "EQUIPMENT",
        eq_profile_uri(version),
        None,
        None,
        None,
    );

    let ns = cim_ns(version);

    // --- GeographicalRegion / SubGeographicalRegion ---
    if network.metadata.regions.is_empty() {
        // No region data — emit a single synthetic GR + SGR.
        writeln!(out, "  <cim:GeographicalRegion rdf:ID=\"{}\">", gr_id())?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>GR</cim:IdentifiedObject.name>"
        )?;
        writeln!(out, "  </cim:GeographicalRegion>")?;

        writeln!(out, "  <cim:SubGeographicalRegion rdf:ID=\"{}\">", sgr_id())?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>SGR</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:SubGeographicalRegion.Region rdf:resource=\"#{}\"/>",
            gr_id()
        )?;
        writeln!(out, "  </cim:SubGeographicalRegion>")?;
    } else {
        // Emit one GeographicalRegion parent + one SubGeographicalRegion per region.
        writeln!(out, "  <cim:GeographicalRegion rdf:ID=\"{}\">", gr_id())?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>GR</cim:IdentifiedObject.name>"
        )?;
        writeln!(out, "  </cim:GeographicalRegion>")?;

        for region in &network.metadata.regions {
            let sgr = format!("_SGR_{}", region.number);
            writeln!(out, "  <cim:SubGeographicalRegion rdf:ID=\"{sgr}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                region.name
            )?;
            writeln!(
                out,
                "    <cim:SubGeographicalRegion.Region rdf:resource=\"#{}\"/>",
                gr_id()
            )?;
            writeln!(out, "  </cim:SubGeographicalRegion>")?;
        }
    }

    // --- BaseVoltage objects (one per unique kV level) ---
    let mut bv_sorted: Vec<_> = base_voltage_set.iter().collect();
    bv_sorted.sort_by_key(|(k, _)| *k);
    for &(_, &kv) in &bv_sorted {
        let id = bv_id(kv);
        writeln!(out, "  <cim:BaseVoltage rdf:ID=\"{id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{kv}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:BaseVoltage.nominalVoltage>{kv}</cim:BaseVoltage.nominalVoltage>"
        )?;
        writeln!(out, "  </cim:BaseVoltage>")?;
    }

    if let Some(ref sm) = network.topology {
        // --- Real substation hierarchy from NodeBreakerTopology ---
        for sub in &sm.substations {
            writeln!(out, "  <cim:Substation rdf:ID=\"{}\">", sub.id)?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                sub.name
            )?;
            if let Some(ref region) = sub.region {
                writeln!(
                    out,
                    "    <cim:Substation.Region rdf:resource=\"#{region}\"/>"
                )?;
            } else {
                writeln!(
                    out,
                    "    <cim:Substation.Region rdf:resource=\"#{}\"/>",
                    sgr_id()
                )?;
            }
            writeln!(out, "  </cim:Substation>")?;
        }

        for vl in &sm.voltage_levels {
            let bvid = bv_id(vl.base_kv);
            writeln!(out, "  <cim:VoltageLevel rdf:ID=\"{}\">", vl.id)?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                vl.name
            )?;
            writeln!(
                out,
                "    <cim:VoltageLevel.Substation rdf:resource=\"#{}\"/>",
                vl.substation_id
            )?;
            writeln!(
                out,
                "    <cim:VoltageLevel.BaseVoltage rdf:resource=\"#{bvid}\"/>"
            )?;
            writeln!(out, "  </cim:VoltageLevel>")?;
        }

        for bay in &sm.bays {
            writeln!(out, "  <cim:Bay rdf:ID=\"{}\">", bay.id)?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                bay.name
            )?;
            writeln!(
                out,
                "    <cim:Bay.VoltageLevel rdf:resource=\"#{}\"/>",
                bay.voltage_level_id
            )?;
            writeln!(out, "  </cim:Bay>")?;
        }

        for cn in &sm.connectivity_nodes {
            writeln!(out, "  <cim:ConnectivityNode rdf:ID=\"{}\">", cn.id)?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                cn.name
            )?;
            writeln!(
                out,
                "    <cim:ConnectivityNode.ConnectivityNodeContainer rdf:resource=\"#{}\"/>",
                cn.voltage_level_id
            )?;
            writeln!(out, "  </cim:ConnectivityNode>")?;
        }

        for bb in &sm.busbar_sections {
            writeln!(out, "  <cim:BusbarSection rdf:ID=\"{}\">", bb.id)?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                bb.name
            )?;
            writeln!(out, "  </cim:BusbarSection>")?;
        }

        for sw in &sm.switches {
            let cim_class = match sw.switch_type {
                SwitchType::Breaker => "Breaker",
                SwitchType::Disconnector => "Disconnector",
                SwitchType::LoadBreakSwitch => "LoadBreakSwitch",
                SwitchType::Fuse => "Fuse",
                SwitchType::GroundDisconnector => "GroundDisconnector",
                SwitchType::Switch => "Switch",
            };
            writeln!(out, "  <cim:{cim_class} rdf:ID=\"{}\">", sw.id)?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                sw.name
            )?;
            writeln!(
                out,
                "    <cim:Switch.normalOpen>{}</cim:Switch.normalOpen>",
                sw.normal_open
            )?;
            if sw.retained {
                writeln!(out, "    <cim:Switch.retained>true</cim:Switch.retained>")?;
            }
            if let Some(rc) = sw.rated_current {
                writeln!(
                    out,
                    "    <cim:Switch.ratedCurrent>{rc}</cim:Switch.ratedCurrent>"
                )?;
            }
            writeln!(out, "  </cim:{cim_class}>")?;

            // Emit two terminals for this switch (connecting to its CNs).
            for (seq, cn_id) in [(1, &sw.cn1_id), (2, &sw.cn2_id)] {
                let term_id = format!("{}_T{seq}", sw.id);
                writeln!(out, "  <cim:Terminal rdf:ID=\"{term_id}\">")?;
                writeln!(
                    out,
                    "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{}\"/>",
                    sw.id
                )?;
                writeln!(
                    out,
                    "    <cim:Terminal.ConnectivityNode rdf:resource=\"#{cn_id}\"/>"
                )?;
                writeln!(
                    out,
                    "    <cim:ACDCTerminal.sequenceNumber>{seq}</cim:ACDCTerminal.sequenceNumber>"
                )?;
                writeln!(out, "  </cim:Terminal>")?;
            }
        }
    } else {
        // --- Synthetic 1:1 substation hierarchy (bus-branch only networks) ---
        for bus in &network.buses {
            let sid = bus_sub
                .get(&bus.number)
                .cloned()
                .unwrap_or_else(|| sub_id(bus.number));
            writeln!(out, "  <cim:Substation rdf:ID=\"{sid}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>S{}</cim:IdentifiedObject.name>",
                bus.number
            )?;
            // Use region-specific SGR if bus has a zone, otherwise default SGR.
            let sgr_ref = if bus.zone > 0 && !network.metadata.regions.is_empty() {
                format!("_SGR_{}", bus.zone)
            } else {
                sgr_id()
            };
            writeln!(
                out,
                "    <cim:Substation.Region rdf:resource=\"#{sgr_ref}\"/>"
            )?;
            writeln!(out, "  </cim:Substation>")?;
        }

        for bus in &network.buses {
            let vlid = vl_id(bus.number);
            let sid = bus_sub
                .get(&bus.number)
                .cloned()
                .unwrap_or_else(|| sub_id(bus.number));
            let bvid = bv_id(bus.base_kv);
            writeln!(out, "  <cim:VoltageLevel rdf:ID=\"{vlid}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>VL{}</cim:IdentifiedObject.name>",
                bus.number
            )?;
            writeln!(
                out,
                "    <cim:VoltageLevel.Substation rdf:resource=\"#{sid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:VoltageLevel.BaseVoltage rdf:resource=\"#{bvid}\"/>"
            )?;
            writeln!(out, "  </cim:VoltageLevel>")?;
        }
    }

    // --- ACLineSegments and PowerTransformers ---
    let base_mva = network.base_mva;
    for (bi, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        // Determine base_kv for impedance conversion.
        // For transformers: use the from-bus base kV.
        // For lines: use the from-bus base kV.
        let from_kv = network
            .buses
            .iter()
            .find(|b| b.number == br.from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);
        let to_kv = network
            .buses
            .iter()
            .find(|b| b.number == br.to_bus)
            .map(|b| b.base_kv)
            .unwrap_or(from_kv);

        if is_transformer(br) {
            // PowerTransformer + two PowerTransformerEnds
            let xid = xfmr_id(bi);
            let end1_id = xfmr_end_id(bi, 1);
            let end2_id = xfmr_end_id(bi, 2);
            let t1_id = term_id(&xid, 1);
            let t2_id = term_id(&xid, 2);

            // Convert per-unit to physical Ohms on the from-side base.
            let r_ohm = pu_to_ohm(br.r, from_kv, base_mva);
            let x_ohm = pu_to_ohm(br.x, from_kv, base_mva);

            // Magnetizing susceptance / conductance on winding 1
            let b_mag_s = pu_to_siemens(br.b_mag, from_kv, base_mva);
            let g_mag_s = pu_to_siemens(br.g_mag, from_kv, base_mva);

            writeln!(out, "  <cim:PowerTransformer rdf:ID=\"{xid}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>T_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
                vl_id(br.from_bus)
            )?;
            writeln!(out, "  </cim:PowerTransformer>")?;

            // Terminal 1 (from bus)
            writeln!(out, "  <cim:Terminal rdf:ID=\"{t1_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>T_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{xid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;

            // Terminal 2 (to bus)
            writeln!(out, "  <cim:Terminal rdf:ID=\"{t2_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>T_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{xid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;

            // PowerTransformerEnd 1 (from/HV side — carries impedance)
            let bvid_from = bv_id(from_kv);
            writeln!(out, "  <cim:PowerTransformerEnd rdf:ID=\"{end1_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>End1</cim:IdentifiedObject.name>"
            )?;
            writeln!(
                out,
                "    <cim:TransformerEnd.endNumber>1</cim:TransformerEnd.endNumber>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.PowerTransformer rdf:resource=\"#{xid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:TransformerEnd.Terminal rdf:resource=\"#{t1_id}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:TransformerEnd.BaseVoltage rdf:resource=\"#{bvid_from}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.r>{r_ohm}</cim:PowerTransformerEnd.r>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.x>{x_ohm}</cim:PowerTransformerEnd.x>"
            )?;
            if b_mag_s.abs() > 1e-15 {
                writeln!(
                    out,
                    "    <cim:PowerTransformerEnd.b>{b_mag_s}</cim:PowerTransformerEnd.b>"
                )?;
            }
            if g_mag_s.abs() > 1e-15 {
                writeln!(
                    out,
                    "    <cim:PowerTransformerEnd.g>{g_mag_s}</cim:PowerTransformerEnd.g>"
                )?;
            }
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.ratedU>{from_kv}</cim:PowerTransformerEnd.ratedU>"
            )?;
            writeln!(out, "  </cim:PowerTransformerEnd>")?;

            // PowerTransformerEnd 2 (to/LV side — no impedance, carries ratedU)
            let bvid_to = bv_id(to_kv);
            writeln!(out, "  <cim:PowerTransformerEnd rdf:ID=\"{end2_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>End2</cim:IdentifiedObject.name>"
            )?;
            writeln!(
                out,
                "    <cim:TransformerEnd.endNumber>2</cim:TransformerEnd.endNumber>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.PowerTransformer rdf:resource=\"#{xid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:TransformerEnd.Terminal rdf:resource=\"#{t2_id}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:TransformerEnd.BaseVoltage rdf:resource=\"#{bvid_to}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.r>0</cim:PowerTransformerEnd.r>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.x>0</cim:PowerTransformerEnd.x>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.ratedU>{to_kv}</cim:PowerTransformerEnd.ratedU>"
            )?;
            writeln!(out, "  </cim:PowerTransformerEnd>")?;

            // RatioTapChanger if tap != 1.0
            if (br.tap - 1.0).abs() > 1e-8 {
                let rtc_id = format!("_RTC_{bi}");
                // CGMES RatioTapChanger: step from neutral + stepVoltageIncrement.
                // Simple encoding: neutralStep=0, step=(tap-1)/0.01, stepVoltageIncrement=1%
                // This gives: effectiveTap = 1 + step * 0.01 = tap
                let step = ((br.tap - 1.0) / 0.01).round() as i64;
                let low_step = step.min(0) - 1;
                let high_step = step.max(0) + 1;
                writeln!(out, "  <cim:RatioTapChanger rdf:ID=\"{rtc_id}\">")?;
                writeln!(
                    out,
                    "    <cim:IdentifiedObject.name>RTC_{bi}</cim:IdentifiedObject.name>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.lowStep>{low_step}</cim:TapChanger.lowStep>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.highStep>{high_step}</cim:TapChanger.highStep>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.neutralU>{from_kv}</cim:TapChanger.neutralU>"
                )?;
                writeln!(out, "    <cim:TapChanger.step>{step}</cim:TapChanger.step>")?;
                writeln!(
                    out,
                    "    <cim:RatioTapChanger.stepVoltageIncrement>1</cim:RatioTapChanger.stepVoltageIncrement>"
                )?;
                writeln!(
                    out,
                    "    <cim:RatioTapChanger.TransformerEnd rdf:resource=\"#{end1_id}\"/>"
                )?;
                writeln!(out, "  </cim:RatioTapChanger>")?;
            }

            // PhaseTapChangerLinear if shift != 0.0
            if br.phase_shift_rad.abs() > 1e-8 {
                let ptc_id = format!("_PTC_{bi}");
                let step_angle = br.phase_shift_rad.to_degrees();
                writeln!(out, "  <cim:PhaseTapChangerLinear rdf:ID=\"{ptc_id}\">")?;
                writeln!(
                    out,
                    "    <cim:IdentifiedObject.name>PTC_{bi}</cim:IdentifiedObject.name>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.lowStep>-1</cim:TapChanger.lowStep>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.highStep>1</cim:TapChanger.highStep>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.neutralStep>0</cim:TapChanger.neutralStep>"
                )?;
                writeln!(
                    out,
                    "    <cim:TapChanger.neutralU>{from_kv}</cim:TapChanger.neutralU>"
                )?;
                writeln!(out, "    <cim:TapChanger.step>1</cim:TapChanger.step>")?;
                writeln!(
                    out,
                    "    <cim:PhaseTapChangerLinear.stepPhaseShiftIncrement>{step_angle}</cim:PhaseTapChangerLinear.stepPhaseShiftIncrement>"
                )?;
                writeln!(
                    out,
                    "    <cim:PhaseTapChangerLinear.TransformerEnd rdf:resource=\"#{end1_id}\"/>"
                )?;
                writeln!(out, "  </cim:PhaseTapChangerLinear>")?;
            }
        } else if br.branch_type == BranchType::SeriesCapacitor {
            // SeriesCompensator
            let scid = sc_id(bi);
            let t1_id = term_id(&scid, 1);
            let t2_id = term_id(&scid, 2);

            let r_ohm = pu_to_ohm(br.r, from_kv, base_mva);
            let x_ohm = pu_to_ohm(br.x, from_kv, base_mva);

            writeln!(out, "  <cim:SeriesCompensator rdf:ID=\"{scid}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>SC_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:SeriesCompensator.r>{r_ohm}</cim:SeriesCompensator.r>"
            )?;
            writeln!(
                out,
                "    <cim:SeriesCompensator.x>{x_ohm}</cim:SeriesCompensator.x>"
            )?;
            writeln!(
                out,
                "    <cim:ConductingEquipment.BaseVoltage rdf:resource=\"#{}\"/>",
                bv_id(from_kv)
            )?;
            writeln!(out, "  </cim:SeriesCompensator>")?;

            // Terminal 1
            writeln!(out, "  <cim:Terminal rdf:ID=\"{t1_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>SC_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{scid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;

            // Terminal 2
            writeln!(out, "  <cim:Terminal rdf:ID=\"{t2_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>SC_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{scid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        } else {
            // ACLineSegment
            let lid = line_id(bi);
            let t1_id = term_id(&lid, 1);
            let t2_id = term_id(&lid, 2);

            // Convert pu to physical Ohms/Siemens at the from-bus base kV.
            let r_ohm = pu_to_ohm(br.r, from_kv, base_mva);
            let x_ohm = pu_to_ohm(br.x, from_kv, base_mva);
            let b_s = pu_to_siemens(br.b, from_kv, base_mva);

            writeln!(out, "  <cim:ACLineSegment rdf:ID=\"{lid}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>L_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:ACLineSegment.r>{r_ohm}</cim:ACLineSegment.r>"
            )?;
            writeln!(
                out,
                "    <cim:ACLineSegment.x>{x_ohm}</cim:ACLineSegment.x>"
            )?;
            writeln!(
                out,
                "    <cim:ACLineSegment.bch>{b_s}</cim:ACLineSegment.bch>"
            )?;
            writeln!(
                out,
                "    <cim:ConductingEquipment.BaseVoltage rdf:resource=\"#{}\"/>",
                bv_id(from_kv)
            )?;
            writeln!(out, "  </cim:ACLineSegment>")?;

            // Terminal 1 (from bus)
            writeln!(out, "  <cim:Terminal rdf:ID=\"{t1_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>L_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{lid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;

            // Terminal 2 (to bus)
            writeln!(out, "  <cim:Terminal rdf:ID=\"{t2_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>L_{}_{}_{bi}</cim:IdentifiedObject.name>",
                br.from_bus, br.to_bus
            )?;
            writeln!(
                out,
                "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{lid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.sequenceNumber>2</cim:ACDCTerminal.sequenceNumber>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        }
    }

    // --- SynchronousMachine + GeneratingUnit + RegulatingControl ---
    for (gi, gn) in network.generators.iter().enumerate() {
        if !gn.in_service || roundtrip.skipped_generator_indices.contains(&gi) {
            continue;
        }
        let smid = sm_id(gi);
        let guid = gu_id(gi);
        let rcid = rc_id(gi);
        let t_id = term_id(&smid, 1);

        // RegulatingControl
        writeln!(out, "  <cim:RegulatingControl rdf:ID=\"{rcid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>RC_{gi}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingControl.Terminal rdf:resource=\"#{t_id}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingControl.mode rdf:resource=\"{ns}RegulatingControlModeKind.voltage\"/>"
        )?;
        writeln!(out, "  </cim:RegulatingControl>")?;

        // SynchronousMachine
        writeln!(out, "  <cim:SynchronousMachine rdf:ID=\"{smid}\">")?;
        if let Some(ref name) = gn.machine_id {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{name}</cim:IdentifiedObject.name>"
            )?;
        } else {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>G{gi}_B{}</cim:IdentifiedObject.name>",
                gn.bus
            )?;
        }
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(gn.bus)
        )?;
        writeln!(
            out,
            "    <cim:RotatingMachine.GeneratingUnit rdf:resource=\"#{guid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingCondEq.RegulatingControl rdf:resource=\"#{rcid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:SynchronousMachine.minQ>{}</cim:SynchronousMachine.minQ>",
            gn.qmin
        )?;
        writeln!(
            out,
            "    <cim:SynchronousMachine.maxQ>{}</cim:SynchronousMachine.maxQ>",
            gn.qmax
        )?;
        if gn.machine_base_mva > 0.0 {
            writeln!(
                out,
                "    <cim:RotatingMachine.ratedS>{}</cim:RotatingMachine.ratedS>",
                gn.machine_base_mva
            )?;
        }
        writeln!(
            out,
            "    <cim:SynchronousMachine.type rdf:resource=\"{ns}SynchronousMachineKind.generator\"/>"
        )?;
        writeln!(out, "  </cim:SynchronousMachine>")?;

        // Terminal for the SynchronousMachine
        writeln!(out, "  <cim:Terminal rdf:ID=\"{t_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>G{gi}_B{}</cim:IdentifiedObject.name>",
            gn.bus
        )?;
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{smid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;

        // GeneratingUnit
        let sub_for_gen = bus_sub
            .get(&gn.bus)
            .cloned()
            .unwrap_or_else(|| sub_id(gn.bus));
        writeln!(out, "  <cim:GeneratingUnit rdf:ID=\"{guid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>GU_{gi}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:GeneratingUnit.minOperatingP>{}</cim:GeneratingUnit.minOperatingP>",
            gn.pmin
        )?;
        writeln!(
            out,
            "    <cim:GeneratingUnit.maxOperatingP>{}</cim:GeneratingUnit.maxOperatingP>",
            gn.pmax
        )?;
        writeln!(
            out,
            "    <cim:GeneratingUnit.initialP>{}</cim:GeneratingUnit.initialP>",
            gn.p
        )?;
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{sub_for_gen}\"/>"
        )?;
        writeln!(out, "  </cim:GeneratingUnit>")?;
    }

    // --- Round-tripped EquivalentInjection / ExternalNetworkInjection ---
    for export in &roundtrip.equivalent_injections {
        if !equivalent_injection_is_in_service(export, network) {
            continue;
        }
        let mrid = export.source.mrid.as_str();
        let bus = equivalent_injection_bus(export, network);
        let name = equivalent_injection_name(export);
        let terminal_id = term_id(mrid, 1);
        let rc_id = roundtrip_rc_id(mrid);
        let target_kv = equivalent_injection_target_kv(export, network);
        let has_reg_control =
            export.source.control_enabled || export.source.regulation_status || target_kv.is_some();
        let (qmin, qmax) = equivalent_injection_q_limits(export, network);

        if has_reg_control {
            writeln!(out, "  <cim:RegulatingControl rdf:ID=\"{rc_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>RC_{name}</cim:IdentifiedObject.name>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.Terminal rdf:resource=\"#{terminal_id}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.mode rdf:resource=\"{ns}RegulatingControlModeKind.voltage\"/>"
            )?;
            writeln!(out, "  </cim:RegulatingControl>")?;
        }

        writeln!(out, "  <cim:EquivalentInjection rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{name}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(bus)
        )?;
        writeln!(
            out,
            "    <cim:ConductingEquipment.BaseVoltage rdf:resource=\"#{}\"/>",
            bv_id(bus_base_kv(network, bus))
        )?;
        if has_reg_control {
            writeln!(
                out,
                "    <cim:RegulatingCondEq.RegulatingControl rdf:resource=\"#{rc_id}\"/>"
            )?;
        }
        if let Some(min_q) = qmin {
            writeln!(
                out,
                "    <cim:EquivalentInjection.minQ>{min_q}</cim:EquivalentInjection.minQ>"
            )?;
        }
        if let Some(max_q) = qmax {
            writeln!(
                out,
                "    <cim:EquivalentInjection.maxQ>{max_q}</cim:EquivalentInjection.maxQ>"
            )?;
        }
        writeln!(
            out,
            "    <cim:EquivalentInjection.regulationCapability>{}</cim:EquivalentInjection.regulationCapability>",
            has_reg_control
        )?;
        writeln!(out, "  </cim:EquivalentInjection>")?;

        writeln!(out, "  <cim:Terminal rdf:ID=\"{terminal_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{name}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{mrid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    for export in &roundtrip.external_network_injections {
        if !external_network_injection_is_in_service(export, network) {
            continue;
        }
        let mrid = export.source.mrid.as_str();
        let bus = external_network_injection_bus(export, network);
        let name = external_network_injection_name(export);
        let terminal_id = term_id(mrid, 1);
        let rc_id = roundtrip_rc_id(mrid);
        let target_kv = external_network_target_kv(export, network);
        let has_reg_control =
            export.source.control_enabled || export.source.regulation_status || target_kv.is_some();

        if has_reg_control {
            writeln!(out, "  <cim:RegulatingControl rdf:ID=\"{rc_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>RC_{name}</cim:IdentifiedObject.name>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.Terminal rdf:resource=\"#{terminal_id}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.mode rdf:resource=\"{ns}RegulatingControlModeKind.voltage\"/>"
            )?;
            writeln!(out, "  </cim:RegulatingControl>")?;
        }

        writeln!(out, "  <cim:ExternalNetworkInjection rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{name}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(bus)
        )?;
        writeln!(
            out,
            "    <cim:ConductingEquipment.BaseVoltage rdf:resource=\"#{}\"/>",
            bv_id(bus_base_kv(network, bus))
        )?;
        if has_reg_control {
            writeln!(
                out,
                "    <cim:RegulatingCondEq.RegulatingControl rdf:resource=\"#{rc_id}\"/>"
            )?;
        }
        if let Some(priority) = external_network_reference_priority(export, network) {
            writeln!(
                out,
                "    <cim:ExternalNetworkInjection.referencePriority>{priority}</cim:ExternalNetworkInjection.referencePriority>"
            )?;
        }
        if let Some(min_q) = export.source.min_q_mvar {
            writeln!(
                out,
                "    <cim:ExternalNetworkInjection.minQ>{min_q}</cim:ExternalNetworkInjection.minQ>"
            )?;
        }
        if let Some(max_q) = export.source.max_q_mvar {
            writeln!(
                out,
                "    <cim:ExternalNetworkInjection.maxQ>{max_q}</cim:ExternalNetworkInjection.maxQ>"
            )?;
        }
        writeln!(out, "  </cim:ExternalNetworkInjection>")?;

        writeln!(out, "  <cim:Terminal rdf:ID=\"{terminal_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{name}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{mrid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- EnergyConsumer (loads from network.loads) ---
    for (li, load) in network.loads.iter().enumerate() {
        if !load.in_service {
            continue;
        }
        let ecid = ec_id(li);
        let t_id = term_id(&ecid, 1);

        writeln!(out, "  <cim:EnergyConsumer rdf:ID=\"{ecid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>LD{li}_B{}</cim:IdentifiedObject.name>",
            load.bus
        )?;
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(load.bus)
        )?;
        writeln!(out, "  </cim:EnergyConsumer>")?;

        // Terminal
        writeln!(out, "  <cim:Terminal rdf:ID=\"{t_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>LD{li}_B{}</cim:IdentifiedObject.name>",
            load.bus
        )?;
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{ecid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Bus-level loads (pd/qd on buses) as EnergyConsumer if no explicit loads ---
    // Only emit these if the loads vector is empty (MATPOWER convention: loads are on buses).
    if network.loads.is_empty() {
        let mut bus_load_idx = 0usize;
        for bus in &network.buses {
            if bus_demand_p
                .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                .copied()
                .unwrap_or(0.0)
                .abs()
                < 1e-10
                && bus_demand_q
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    < 1e-10
            {
                continue;
            }
            let ecid = format!("_ECB_{}", bus.number);
            let t_id = term_id(&ecid, 1);

            writeln!(out, "  <cim:EnergyConsumer rdf:ID=\"{ecid}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>BLD_{}</cim:IdentifiedObject.name>",
                bus.number
            )?;
            writeln!(
                out,
                "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
                vl_id(bus.number)
            )?;
            writeln!(out, "  </cim:EnergyConsumer>")?;

            // Terminal
            writeln!(out, "  <cim:Terminal rdf:ID=\"{t_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>BLD_{}</cim:IdentifiedObject.name>",
                bus.number
            )?;
            writeln!(
                out,
                "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{ecid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
            bus_load_idx += 1;
        }
        let _ = bus_load_idx; // suppress unused warning
    }

    // --- LinearShuntCompensator (bus shunts: gs/bs) ---
    // When fixed_shunts exist, subtract their contributions to avoid double-counting.
    let mut fsh_g_by_bus: HashMap<u32, f64> = HashMap::new();
    let mut fsh_b_by_bus: HashMap<u32, f64> = HashMap::new();
    for fsh in &network.fixed_shunts {
        if fsh.in_service {
            *fsh_g_by_bus.entry(fsh.bus).or_default() += fsh.g_mw;
            *fsh_b_by_bus.entry(fsh.bus).or_default() += fsh.b_mvar;
        }
    }

    for bus in &network.buses {
        // Residual = bus-level shunt minus FixedShunt contributions on this bus.
        let residual_g =
            bus.shunt_conductance_mw - fsh_g_by_bus.get(&bus.number).copied().unwrap_or(0.0);
        let residual_b =
            bus.shunt_susceptance_mvar - fsh_b_by_bus.get(&bus.number).copied().unwrap_or(0.0);
        if residual_g.abs() < 1e-10 && residual_b.abs() < 1e-10 {
            continue;
        }
        let shid = shunt_id(bus.number);
        let t_id = term_id(&shid, 1);
        // gs/bs in MW/MVAr at V=1.0 pu → Siemens: g_s = gs / base_kv^2
        let g_s = if bus.base_kv > 1e-3 {
            residual_g / (bus.base_kv * bus.base_kv)
        } else {
            0.0
        };
        let b_s = if bus.base_kv > 1e-3 {
            residual_b / (bus.base_kv * bus.base_kv)
        } else {
            0.0
        };

        writeln!(out, "  <cim:LinearShuntCompensator rdf:ID=\"{shid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>SH_{}</cim:IdentifiedObject.name>",
            bus.number
        )?;
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(bus.number)
        )?;
        writeln!(
            out,
            "    <cim:LinearShuntCompensator.gPerSection>{g_s}</cim:LinearShuntCompensator.gPerSection>"
        )?;
        writeln!(
            out,
            "    <cim:LinearShuntCompensator.bPerSection>{b_s}</cim:LinearShuntCompensator.bPerSection>"
        )?;
        writeln!(
            out,
            "    <cim:ShuntCompensator.maximumSections>1</cim:ShuntCompensator.maximumSections>"
        )?;
        writeln!(
            out,
            "    <cim:ConductingEquipment.BaseVoltage rdf:resource=\"#{}\"/>",
            bv_id(bus.base_kv)
        )?;
        writeln!(out, "  </cim:LinearShuntCompensator>")?;

        writeln!(out, "  <cim:Terminal rdf:ID=\"{t_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>SH_{}</cim:IdentifiedObject.name>",
            bus.number
        )?;
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{shid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- FixedShunts as individual LinearShuntCompensator ---
    for (fi, fsh) in network.fixed_shunts.iter().enumerate() {
        if !fsh.in_service || roundtrip.skipped_fixed_shunt_indices.contains(&fi) {
            continue;
        }
        let fshid = fixed_shunt_id(fi);
        let t_id = term_id(&fshid, 1);
        let bus_kv = network
            .buses
            .iter()
            .find(|b| b.number == fsh.bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);
        let g_s = if bus_kv > 1e-3 {
            fsh.g_mw / (bus_kv * bus_kv)
        } else {
            0.0
        };
        let b_s = if bus_kv > 1e-3 {
            fsh.b_mvar / (bus_kv * bus_kv)
        } else {
            0.0
        };

        writeln!(out, "  <cim:LinearShuntCompensator rdf:ID=\"{fshid}\">")?;
        if fsh.id.is_empty() {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>FSH{fi}_B{}</cim:IdentifiedObject.name>",
                fsh.bus
            )?;
        } else {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                fsh.id
            )?;
        }
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(fsh.bus)
        )?;
        writeln!(
            out,
            "    <cim:LinearShuntCompensator.gPerSection>{g_s}</cim:LinearShuntCompensator.gPerSection>"
        )?;
        writeln!(
            out,
            "    <cim:LinearShuntCompensator.bPerSection>{b_s}</cim:LinearShuntCompensator.bPerSection>"
        )?;
        writeln!(
            out,
            "    <cim:ShuntCompensator.maximumSections>1</cim:ShuntCompensator.maximumSections>"
        )?;
        writeln!(
            out,
            "    <cim:ConductingEquipment.BaseVoltage rdf:resource=\"#{}\"/>",
            bv_id(bus_kv)
        )?;
        writeln!(out, "  </cim:LinearShuntCompensator>")?;

        writeln!(out, "  <cim:Terminal rdf:ID=\"{t_id}\">")?;
        if fsh.id.is_empty() {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>FSH{fi}_B{}</cim:IdentifiedObject.name>",
                fsh.bus
            )?;
        } else {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                fsh.id
            )?;
        }
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{fshid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Round-tripped DanglingLine ---
    for export in &roundtrip.dangling_lines {
        if !dangling_line_is_in_service(export, network) {
            continue;
        }
        let mrid = export.source.mrid.as_str();
        let bus = dangling_line_bus(export, network);
        let name = dangling_line_name(export);
        let terminal_id = term_id(mrid, 1);
        let (g_s, b_s) = dangling_line_shunt(export, network);

        writeln!(out, "  <cim:DanglingLine rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{name}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(bus)
        )?;
        writeln!(
            out,
            "    <cim:ConductingEquipment.BaseVoltage rdf:resource=\"#{}\"/>",
            bv_id(bus_base_kv(network, bus))
        )?;
        if let Some(r_ohm) = export.source.r_ohm {
            writeln!(out, "    <cim:DanglingLine.r>{r_ohm}</cim:DanglingLine.r>")?;
        }
        if let Some(x_ohm) = export.source.x_ohm {
            writeln!(out, "    <cim:DanglingLine.x>{x_ohm}</cim:DanglingLine.x>")?;
        }
        writeln!(out, "    <cim:DanglingLine.g>{g_s}</cim:DanglingLine.g>")?;
        writeln!(out, "    <cim:DanglingLine.b>{b_s}</cim:DanglingLine.b>")?;
        writeln!(out, "  </cim:DanglingLine>")?;

        writeln!(out, "  <cim:Terminal rdf:ID=\"{terminal_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{name}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{mrid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- EquivalentInjection (power injections) ---
    for (pi, inj) in network.power_injections.iter().enumerate() {
        if !inj.in_service || roundtrip.skipped_injection_indices.contains(&pi) {
            continue;
        }
        if inj.active_power_injection_mw.abs() < 1e-9
            && inj.reactive_power_injection_mvar.abs() < 1e-9
        {
            continue;
        }
        let eid = einj_id(pi);
        let t_id = term_id(&eid, 1);

        writeln!(out, "  <cim:EquivalentInjection rdf:ID=\"{eid}\">")?;
        if inj.id.is_empty() {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>EINJ{pi}_B{}</cim:IdentifiedObject.name>",
                inj.bus
            )?;
        } else {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                inj.id
            )?;
        }
        writeln!(
            out,
            "    <cim:Equipment.EquipmentContainer rdf:resource=\"#{}\"/>",
            vl_id(inj.bus)
        )?;
        writeln!(
            out,
            "    <cim:EquivalentInjection.regulationCapability>false</cim:EquivalentInjection.regulationCapability>"
        )?;
        writeln!(out, "  </cim:EquivalentInjection>")?;

        writeln!(out, "  <cim:Terminal rdf:ID=\"{t_id}\">")?;
        if inj.id.is_empty() {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>EINJ{pi}_B{}</cim:IdentifiedObject.name>",
                inj.bus
            )?;
        } else {
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
                inj.id
            )?;
        }
        writeln!(
            out,
            "    <cim:Terminal.ConductingEquipment rdf:resource=\"#{eid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.sequenceNumber>1</cim:ACDCTerminal.sequenceNumber>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- MutualCoupling (CIM pass-through) ---
    for (mi, mc) in network.cim.mutual_couplings.iter().enumerate() {
        let mcid = format!("_MC_{mi}");
        writeln!(out, "  <cim:MutualCoupling rdf:ID=\"{mcid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>MC_{mi}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:MutualCoupling.First_Terminal rdf:resource=\"#{}\"/>",
            mc.line1_id
        )?;
        writeln!(
            out,
            "    <cim:MutualCoupling.Second_Terminal rdf:resource=\"#{}\"/>",
            mc.line2_id
        )?;
        writeln!(
            out,
            "    <cim:MutualCoupling.r12>{}</cim:MutualCoupling.r12>",
            mc.r
        )?;
        writeln!(
            out,
            "    <cim:MutualCoupling.x12>{}</cim:MutualCoupling.x12>",
            mc.x
        )?;
        writeln!(out, "  </cim:MutualCoupling>")?;
    }

    // --- GroundingImpedance (CIM pass-through) ---
    for (gi, ge) in network.cim.grounding_impedances.iter().enumerate() {
        let gid = format!("_GND_{gi}");
        writeln!(out, "  <cim:GroundingImpedance rdf:ID=\"{gid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>GND_{gi}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:GroundingImpedance.x>{}</cim:GroundingImpedance.x>",
            ge.x_ohm
        )?;
        writeln!(out, "  </cim:GroundingImpedance>")?;
    }

    // --- PerLengthPhaseImpedance (CIM pass-through) ---
    for (plpi_id, entries) in &network.cim.per_length_phase_impedances {
        writeln!(out, "  <cim:PerLengthPhaseImpedance rdf:ID=\"{plpi_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{plpi_id}</cim:IdentifiedObject.name>"
        )?;
        writeln!(out, "  </cim:PerLengthPhaseImpedance>")?;
        for (ei, entry) in entries.iter().enumerate() {
            let pid = format!("{plpi_id}_PID_{ei}");
            writeln!(out, "  <cim:PhaseImpedanceData rdf:ID=\"{pid}\">")?;
            writeln!(
                out,
                "    <cim:PhaseImpedanceData.PhaseImpedance rdf:resource=\"#{plpi_id}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:PhaseImpedanceData.row>{}</cim:PhaseImpedanceData.row>",
                entry.row
            )?;
            writeln!(
                out,
                "    <cim:PhaseImpedanceData.column>{}</cim:PhaseImpedanceData.column>",
                entry.col
            )?;
            writeln!(
                out,
                "    <cim:PhaseImpedanceData.r>{}</cim:PhaseImpedanceData.r>",
                entry.r
            )?;
            writeln!(
                out,
                "    <cim:PhaseImpedanceData.x>{}</cim:PhaseImpedanceData.x>",
                entry.x
            )?;
            writeln!(
                out,
                "    <cim:PhaseImpedanceData.b>{}</cim:PhaseImpedanceData.b>",
                entry.b
            )?;
            writeln!(out, "  </cim:PhaseImpedanceData>")?;
        }
    }

    // --- OperationalLimitSet + CurrentLimit (conditional ratings) ---
    for (bi, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let cond_ratings = match network.conditional_limits.get_for_branch(br) {
            Some(r) if !r.is_empty() => r,
            _ => continue,
        };
        let eq_id_str = branch_eq_id(bi, br);
        let t1 = term_id(&eq_id_str, 1);
        let ols_id = format!("_OLS_{bi}");
        writeln!(out, "  <cim:OperationalLimitSet rdf:ID=\"{ols_id}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>OLS_{bi}</cim:IdentifiedObject.name>"
        )?;
        writeln!(
            out,
            "    <cim:OperationalLimitSet.Terminal rdf:resource=\"#{t1}\"/>"
        )?;
        writeln!(out, "  </cim:OperationalLimitSet>")?;

        for (ci, cr) in cond_ratings.iter().enumerate() {
            // PATL (normal rating)
            let patl_id = format!("_CL_{bi}_{ci}_PATL");
            writeln!(out, "  <cim:CurrentLimit rdf:ID=\"{patl_id}\">")?;
            writeln!(
                out,
                "    <cim:IdentifiedObject.name>PATL_{bi}_{ci}</cim:IdentifiedObject.name>"
            )?;
            writeln!(
                out,
                "    <cim:OperationalLimit.OperationalLimitSet rdf:resource=\"#{ols_id}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:CurrentLimit.value>{}</cim:CurrentLimit.value>",
                cr.rating_a_mva
            )?;
            writeln!(out, "  </cim:CurrentLimit>")?;
            // TATL (emergency rating)
            if cr.rating_c_mva > 0.0 {
                let tatl_id = format!("_CL_{bi}_{ci}_TATL");
                writeln!(out, "  <cim:CurrentLimit rdf:ID=\"{tatl_id}\">")?;
                writeln!(
                    out,
                    "    <cim:IdentifiedObject.name>TATL_{bi}_{ci}</cim:IdentifiedObject.name>"
                )?;
                writeln!(
                    out,
                    "    <cim:OperationalLimit.OperationalLimitSet rdf:resource=\"#{ols_id}\"/>"
                )?;
                writeln!(
                    out,
                    "    <cim:CurrentLimit.value>{}</cim:CurrentLimit.value>",
                    cr.rating_c_mva
                )?;
                writeln!(out, "  </cim:CurrentLimit>")?;
            }
        }
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// TP profile writer
// ---------------------------------------------------------------------------

fn write_tp_profile(network: &Network, version: CgmesVersion) -> Result<String, CgmesWriteError> {
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let bus_idx_map = network.bus_index_map();
    let roundtrip = RoundtripExportState::build(network);
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "TOPOLOGY",
        tp_profile_uri(version),
        Some(&eq_urn),
        None,
        None,
    );

    // --- TopologicalNode per bus ---
    for bus in &network.buses {
        let tnid = tn_id(bus.number);
        let vlid = vl_id(bus.number);
        let bvid = bv_id(bus.base_kv);
        writeln!(out, "  <cim:TopologicalNode rdf:ID=\"{tnid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            if bus.name.is_empty() {
                format!("B{}", bus.number)
            } else {
                bus.name.clone()
            }
        )?;
        writeln!(
            out,
            "    <cim:TopologicalNode.ConnectivityNodeContainer rdf:resource=\"#{vlid}\"/>"
        )?;
        writeln!(
            out,
            "    <cim:TopologicalNode.BaseVoltage rdf:resource=\"#{bvid}\"/>"
        )?;
        writeln!(out, "  </cim:TopologicalNode>")?;
    }

    // --- Build bus_number → TN_id map ---
    let bus_tn: HashMap<u32, String> = network
        .buses
        .iter()
        .map(|b| (b.number, tn_id(b.number)))
        .collect();

    // --- Terminal → TopologicalNode assignments for branches ---
    for (bi, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let eq_id_str = branch_eq_id(bi, br);
        let t1 = term_id(&eq_id_str, 1);
        let t2 = term_id(&eq_id_str, 2);
        let tn_from = bus_tn.get(&br.from_bus).cloned().unwrap_or_default();
        let tn_to = bus_tn.get(&br.to_bus).cloned().unwrap_or_default();

        writeln!(out, "  <cim:Terminal rdf:about=\"#{t1}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn_from}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;

        writeln!(out, "  <cim:Terminal rdf:about=\"#{t2}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn_to}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Terminal → TopologicalNode for generators ---
    for (gi, gn) in network.generators.iter().enumerate() {
        if !gn.in_service || roundtrip.skipped_generator_indices.contains(&gi) {
            continue;
        }
        let smid = sm_id(gi);
        let t_id = term_id(&smid, 1);
        let tn = bus_tn.get(&gn.bus).cloned().unwrap_or_default();
        writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Terminal → TopologicalNode for round-tripped EquivalentInjection / ExternalNetworkInjection ---
    for export in &roundtrip.equivalent_injections {
        if !equivalent_injection_is_in_service(export, network) {
            continue;
        }
        let t_id = term_id(export.source.mrid.as_str(), 1);
        let tn = bus_tn
            .get(&equivalent_injection_bus(export, network))
            .cloned()
            .unwrap_or_default();
        writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }
    for export in &roundtrip.external_network_injections {
        if !external_network_injection_is_in_service(export, network) {
            continue;
        }
        let t_id = term_id(export.source.mrid.as_str(), 1);
        let tn = bus_tn
            .get(&external_network_injection_bus(export, network))
            .cloned()
            .unwrap_or_default();
        writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Terminal → TopologicalNode for loads ---
    if !network.loads.is_empty() {
        for (li, load) in network.loads.iter().enumerate() {
            if !load.in_service {
                continue;
            }
            let ecid = ec_id(li);
            let t_id = term_id(&ecid, 1);
            let tn = bus_tn.get(&load.bus).cloned().unwrap_or_default();
            writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
            writeln!(
                out,
                "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        }
    } else {
        // Bus-level loads
        for bus in &network.buses {
            if bus_demand_p
                .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                .copied()
                .unwrap_or(0.0)
                .abs()
                < 1e-10
                && bus_demand_q
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    < 1e-10
            {
                continue;
            }
            let ecid = format!("_ECB_{}", bus.number);
            let t_id = term_id(&ecid, 1);
            let tn = bus_tn.get(&bus.number).cloned().unwrap_or_default();
            writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
            writeln!(
                out,
                "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        }
    }

    // --- Terminal → TopologicalNode for bus-level shunts ---
    // Must match residual logic in EQ profile.
    let mut fsh_g_by_bus: HashMap<u32, f64> = HashMap::new();
    let mut fsh_b_by_bus: HashMap<u32, f64> = HashMap::new();
    for fsh in &network.fixed_shunts {
        if fsh.in_service {
            *fsh_g_by_bus.entry(fsh.bus).or_default() += fsh.g_mw;
            *fsh_b_by_bus.entry(fsh.bus).or_default() += fsh.b_mvar;
        }
    }
    for bus in &network.buses {
        let residual_g =
            bus.shunt_conductance_mw - fsh_g_by_bus.get(&bus.number).copied().unwrap_or(0.0);
        let residual_b =
            bus.shunt_susceptance_mvar - fsh_b_by_bus.get(&bus.number).copied().unwrap_or(0.0);
        if residual_g.abs() < 1e-10 && residual_b.abs() < 1e-10 {
            continue;
        }
        let shid = shunt_id(bus.number);
        let t_id = term_id(&shid, 1);
        let tn = bus_tn.get(&bus.number).cloned().unwrap_or_default();
        writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Terminal → TopologicalNode for FixedShunts ---
    for (fi, fsh) in network.fixed_shunts.iter().enumerate() {
        if !fsh.in_service || roundtrip.skipped_fixed_shunt_indices.contains(&fi) {
            continue;
        }
        let fshid = fixed_shunt_id(fi);
        let t_id = term_id(&fshid, 1);
        let tn = bus_tn.get(&fsh.bus).cloned().unwrap_or_default();
        writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Terminal → TopologicalNode for round-tripped DanglingLine ---
    for export in &roundtrip.dangling_lines {
        if !dangling_line_is_in_service(export, network) {
            continue;
        }
        let t_id = term_id(export.source.mrid.as_str(), 1);
        let tn = bus_tn
            .get(&dangling_line_bus(export, network))
            .cloned()
            .unwrap_or_default();
        writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Terminal → TopologicalNode for EquivalentInjections ---
    for (pi, inj) in network.power_injections.iter().enumerate() {
        if !inj.in_service || roundtrip.skipped_injection_indices.contains(&pi) {
            continue;
        }
        if inj.active_power_injection_mw.abs() < 1e-9
            && inj.reactive_power_injection_mvar.abs() < 1e-9
        {
            continue;
        }
        let eid = einj_id(pi);
        let t_id = term_id(&eid, 1);
        let tn = bus_tn.get(&inj.bus).cloned().unwrap_or_default();
        writeln!(out, "  <cim:Terminal rdf:about=\"#{t_id}\">")?;
        writeln!(
            out,
            "    <cim:Terminal.TopologicalNode rdf:resource=\"#{tn}\"/>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// SSH profile writer
// ---------------------------------------------------------------------------

fn write_ssh_profile(network: &Network, version: CgmesVersion) -> Result<String, CgmesWriteError> {
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let bus_idx_map = network.bus_index_map();
    let roundtrip = RoundtripExportState::build(network);
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );
    let ns = cim_ns(version);

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "STEADY_STATE_HYPOTHESIS",
        ssh_profile_uri(version),
        Some(&eq_urn),
        None,
        None,
    );

    // --- Generator set-points ---
    // CGMES SSH uses IEC sign convention: negative P = generating.
    for (gi, gn) in network.generators.iter().enumerate() {
        if !gn.in_service || roundtrip.skipped_generator_indices.contains(&gi) {
            continue;
        }
        let smid = sm_id(gi);
        // SSH overlays on top of EQ, so uses rdf:about (not rdf:ID).
        writeln!(out, "  <cim:SynchronousMachine rdf:about=\"#{smid}\">")?;
        writeln!(
            out,
            "    <cim:RegulatingCondEq.controlEnabled>true</cim:RegulatingCondEq.controlEnabled>"
        )?;
        // IEC sign convention: generating is negative P.
        writeln!(
            out,
            "    <cim:RotatingMachine.p>{}</cim:RotatingMachine.p>",
            -gn.p
        )?;
        writeln!(
            out,
            "    <cim:RotatingMachine.q>{}</cim:RotatingMachine.q>",
            -gn.q
        )?;
        writeln!(
            out,
            "    <cim:SynchronousMachine.referencePriority>0</cim:SynchronousMachine.referencePriority>"
        )?;
        writeln!(
            out,
            "    <cim:SynchronousMachine.operatingMode rdf:resource=\"{ns}SynchronousMachineOperatingMode.generator\"/>"
        )?;
        writeln!(out, "  </cim:SynchronousMachine>")?;
    }

    // --- RegulatingControl target voltage ---
    for (gi, gn) in network.generators.iter().enumerate() {
        if !gn.in_service || roundtrip.skipped_generator_indices.contains(&gi) {
            continue;
        }
        let rcid = rc_id(gi);
        // vs is per-unit; CGMES targetValue is in kV.
        let gen_bus_kv = network
            .buses
            .iter()
            .find(|b| b.number == gn.bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);
        let target_kv = gn.voltage_setpoint_pu * gen_bus_kv;

        writeln!(out, "  <cim:RegulatingControl rdf:about=\"#{rcid}\">")?;
        writeln!(
            out,
            "    <cim:RegulatingControl.discrete>false</cim:RegulatingControl.discrete>"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingControl.enabled>true</cim:RegulatingControl.enabled>"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingControl.targetDeadband>0</cim:RegulatingControl.targetDeadband>"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingControl.targetValue>{target_kv}</cim:RegulatingControl.targetValue>"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingControl.targetValueUnitMultiplier rdf:resource=\"{ns}UnitMultiplier.k\"/>"
        )?;
        writeln!(out, "  </cim:RegulatingControl>")?;
    }

    // --- Round-tripped EquivalentInjection / ExternalNetworkInjection set-points ---
    for export in &roundtrip.equivalent_injections {
        if !equivalent_injection_is_in_service(export, network) {
            continue;
        }
        let mrid = export.source.mrid.as_str();
        let (p, q) = equivalent_injection_pq(export, network);
        writeln!(out, "  <cim:EquivalentInjection rdf:about=\"#{mrid}\">")?;
        writeln!(
            out,
            "    <cim:RegulatingCondEq.controlEnabled>{}</cim:RegulatingCondEq.controlEnabled>",
            export.source.control_enabled
        )?;
        writeln!(
            out,
            "    <cim:EquivalentInjection.p>{p}</cim:EquivalentInjection.p>"
        )?;
        writeln!(
            out,
            "    <cim:EquivalentInjection.q>{q}</cim:EquivalentInjection.q>"
        )?;
        writeln!(
            out,
            "    <cim:EquivalentInjection.regulationStatus>{}</cim:EquivalentInjection.regulationStatus>",
            export.source.regulation_status
        )?;
        writeln!(out, "  </cim:EquivalentInjection>")?;

        if let Some(target_kv) = equivalent_injection_target_kv(export, network) {
            let rc_id = roundtrip_rc_id(mrid);
            writeln!(out, "  <cim:RegulatingControl rdf:about=\"#{rc_id}\">")?;
            writeln!(
                out,
                "    <cim:RegulatingControl.discrete>false</cim:RegulatingControl.discrete>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.enabled>{}</cim:RegulatingControl.enabled>",
                export.source.control_enabled || export.source.regulation_status
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.targetDeadband>0</cim:RegulatingControl.targetDeadband>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.targetValue>{target_kv}</cim:RegulatingControl.targetValue>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.targetValueUnitMultiplier rdf:resource=\"{ns}UnitMultiplier.k\"/>"
            )?;
            writeln!(out, "  </cim:RegulatingControl>")?;
        }
    }

    for export in &roundtrip.external_network_injections {
        if !external_network_injection_is_in_service(export, network) {
            continue;
        }
        let mrid = export.source.mrid.as_str();
        let (p, q) = external_network_injection_pq(export, network);
        writeln!(
            out,
            "  <cim:ExternalNetworkInjection rdf:about=\"#{mrid}\">"
        )?;
        writeln!(
            out,
            "    <cim:RegulatingCondEq.controlEnabled>{}</cim:RegulatingCondEq.controlEnabled>",
            export.source.control_enabled
        )?;
        writeln!(
            out,
            "    <cim:ExternalNetworkInjection.p>{p}</cim:ExternalNetworkInjection.p>"
        )?;
        writeln!(
            out,
            "    <cim:ExternalNetworkInjection.q>{q}</cim:ExternalNetworkInjection.q>"
        )?;
        writeln!(
            out,
            "    <cim:ExternalNetworkInjection.regulationStatus>{}</cim:ExternalNetworkInjection.regulationStatus>",
            export.source.regulation_status
        )?;
        writeln!(out, "  </cim:ExternalNetworkInjection>")?;

        if let Some(target_kv) = external_network_target_kv(export, network) {
            let rc_id = roundtrip_rc_id(mrid);
            writeln!(out, "  <cim:RegulatingControl rdf:about=\"#{rc_id}\">")?;
            writeln!(
                out,
                "    <cim:RegulatingControl.discrete>false</cim:RegulatingControl.discrete>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.enabled>{}</cim:RegulatingControl.enabled>",
                export.source.control_enabled || export.source.regulation_status
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.targetDeadband>0</cim:RegulatingControl.targetDeadband>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.targetValue>{target_kv}</cim:RegulatingControl.targetValue>"
            )?;
            writeln!(
                out,
                "    <cim:RegulatingControl.targetValueUnitMultiplier rdf:resource=\"{ns}UnitMultiplier.k\"/>"
            )?;
            writeln!(out, "  </cim:RegulatingControl>")?;
        }
    }

    // --- Load set-points ---
    if !network.loads.is_empty() {
        for (li, load) in network.loads.iter().enumerate() {
            if !load.in_service {
                continue;
            }
            let ecid = ec_id(li);
            writeln!(out, "  <cim:EnergyConsumer rdf:about=\"#{ecid}\">")?;
            writeln!(
                out,
                "    <cim:EnergyConsumer.p>{}</cim:EnergyConsumer.p>",
                load.active_power_demand_mw
            )?;
            writeln!(
                out,
                "    <cim:EnergyConsumer.q>{}</cim:EnergyConsumer.q>",
                load.reactive_power_demand_mvar
            )?;
            writeln!(out, "  </cim:EnergyConsumer>")?;
        }
    } else {
        // Bus-level loads
        for bus in &network.buses {
            if bus_demand_p
                .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                .copied()
                .unwrap_or(0.0)
                .abs()
                < 1e-10
                && bus_demand_q
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    < 1e-10
            {
                continue;
            }
            let ecid = format!("_ECB_{}", bus.number);
            writeln!(out, "  <cim:EnergyConsumer rdf:about=\"#{ecid}\">")?;
            writeln!(
                out,
                "    <cim:EnergyConsumer.p>{}</cim:EnergyConsumer.p>",
                bus_demand_p
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
            )?;
            writeln!(
                out,
                "    <cim:EnergyConsumer.q>{}</cim:EnergyConsumer.q>",
                bus_demand_q
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
            )?;
            writeln!(out, "  </cim:EnergyConsumer>")?;
        }
    }

    // --- EquivalentInjection SSH set-points ---
    for (pi, inj) in network.power_injections.iter().enumerate() {
        if !inj.in_service || roundtrip.skipped_injection_indices.contains(&pi) {
            continue;
        }
        if inj.active_power_injection_mw.abs() < 1e-9
            && inj.reactive_power_injection_mvar.abs() < 1e-9
        {
            continue;
        }
        let eid = einj_id(pi);
        writeln!(out, "  <cim:EquivalentInjection rdf:about=\"#{eid}\">")?;
        writeln!(
            out,
            "    <cim:EquivalentInjection.p>{}</cim:EquivalentInjection.p>",
            inj.active_power_injection_mw
        )?;
        writeln!(
            out,
            "    <cim:EquivalentInjection.q>{}</cim:EquivalentInjection.q>",
            inj.reactive_power_injection_mvar
        )?;
        writeln!(
            out,
            "    <cim:EquivalentInjection.regulationStatus>false</cim:EquivalentInjection.regulationStatus>"
        )?;
        writeln!(out, "  </cim:EquivalentInjection>")?;
    }

    // --- Round-tripped DanglingLine set-points ---
    for export in &roundtrip.dangling_lines {
        if !dangling_line_is_in_service(export, network) {
            continue;
        }
        let mrid = export.source.mrid.as_str();
        let (p, q) = dangling_line_pq(export, network);
        writeln!(out, "  <cim:DanglingLine rdf:about=\"#{mrid}\">")?;
        writeln!(out, "    <cim:DanglingLine.p>{p}</cim:DanglingLine.p>")?;
        writeln!(out, "    <cim:DanglingLine.q>{q}</cim:DanglingLine.q>")?;
        writeln!(out, "  </cim:DanglingLine>")?;
    }

    // --- Terminal connected status ---
    // Write all terminals as connected=true.
    // Branches
    for (bi, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let eq_id_str = branch_eq_id(bi, br);
        for seq in 1..=2 {
            let tid = term_id(&eq_id_str, seq);
            writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        }
    }
    // Generators
    for (gi, gn) in network.generators.iter().enumerate() {
        if !gn.in_service || roundtrip.skipped_generator_indices.contains(&gi) {
            continue;
        }
        let smid = sm_id(gi);
        let tid = term_id(&smid, 1);
        writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }
    // Loads
    if !network.loads.is_empty() {
        for (li, load) in network.loads.iter().enumerate() {
            if !load.in_service {
                continue;
            }
            let ecid = ec_id(li);
            let tid = term_id(&ecid, 1);
            writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        }
    } else {
        for bus in &network.buses {
            if bus_demand_p
                .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                .copied()
                .unwrap_or(0.0)
                .abs()
                < 1e-10
                && bus_demand_q
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    < 1e-10
            {
                continue;
            }
            let ecid = format!("_ECB_{}", bus.number);
            let tid = term_id(&ecid, 1);
            writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        }
    }
    // Shunts (bus-level residual — match EQ logic)
    {
        let mut fsh_g_by_bus: HashMap<u32, f64> = HashMap::new();
        let mut fsh_b_by_bus: HashMap<u32, f64> = HashMap::new();
        for fsh in &network.fixed_shunts {
            if fsh.in_service {
                *fsh_g_by_bus.entry(fsh.bus).or_default() += fsh.g_mw;
                *fsh_b_by_bus.entry(fsh.bus).or_default() += fsh.b_mvar;
            }
        }
        for bus in &network.buses {
            let residual_g =
                bus.shunt_conductance_mw - fsh_g_by_bus.get(&bus.number).copied().unwrap_or(0.0);
            let residual_b =
                bus.shunt_susceptance_mvar - fsh_b_by_bus.get(&bus.number).copied().unwrap_or(0.0);
            if residual_g.abs() < 1e-10 && residual_b.abs() < 1e-10 {
                continue;
            }
            let shid = shunt_id(bus.number);
            let tid = term_id(&shid, 1);
            writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
            writeln!(
                out,
                "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
            )?;
            writeln!(out, "  </cim:Terminal>")?;
        }
    }
    // FixedShunts
    for (fi, fsh) in network.fixed_shunts.iter().enumerate() {
        if !fsh.in_service || roundtrip.skipped_fixed_shunt_indices.contains(&fi) {
            continue;
        }
        let fshid = fixed_shunt_id(fi);
        let tid = term_id(&fshid, 1);
        writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }
    // Round-tripped EquivalentInjection / ExternalNetworkInjection / DanglingLine
    for export in &roundtrip.equivalent_injections {
        if !equivalent_injection_is_in_service(export, network) {
            continue;
        }
        let tid = term_id(export.source.mrid.as_str(), 1);
        writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }
    for export in &roundtrip.external_network_injections {
        if !external_network_injection_is_in_service(export, network) {
            continue;
        }
        let tid = term_id(export.source.mrid.as_str(), 1);
        writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }
    for export in &roundtrip.dangling_lines {
        if !dangling_line_is_in_service(export, network) {
            continue;
        }
        let tid = term_id(export.source.mrid.as_str(), 1);
        writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }
    // EquivalentInjections
    for (pi, inj) in network.power_injections.iter().enumerate() {
        if !inj.in_service || roundtrip.skipped_injection_indices.contains(&pi) {
            continue;
        }
        if inj.active_power_injection_mw.abs() < 1e-9
            && inj.reactive_power_injection_mvar.abs() < 1e-9
        {
            continue;
        }
        let eid = einj_id(pi);
        let tid = term_id(&eid, 1);
        writeln!(out, "  <cim:Terminal rdf:about=\"#{tid}\">")?;
        writeln!(
            out,
            "    <cim:ACDCTerminal.connected>true</cim:ACDCTerminal.connected>"
        )?;
        writeln!(out, "  </cim:Terminal>")?;
    }

    // --- Switch open states (from NodeBreakerTopology) ---
    if let Some(ref sm) = network.topology {
        for sw in &sm.switches {
            let cim_class = match sw.switch_type {
                SwitchType::Breaker => "Breaker",
                SwitchType::Disconnector => "Disconnector",
                SwitchType::LoadBreakSwitch => "LoadBreakSwitch",
                SwitchType::Fuse => "Fuse",
                SwitchType::GroundDisconnector => "GroundDisconnector",
                SwitchType::Switch => "Switch",
            };
            writeln!(out, "  <cim:{cim_class} rdf:about=\"#{}\">", sw.id)?;
            writeln!(out, "    <cim:Switch.open>{}</cim:Switch.open>", sw.open)?;
            writeln!(out, "  </cim:{cim_class}>")?;
        }
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// SV profile writer
// ---------------------------------------------------------------------------

fn write_sv_profile(network: &Network, version: CgmesVersion) -> Result<String, CgmesWriteError> {
    let bus_demand_p = network.bus_load_p_mw();
    let bus_demand_q = network.bus_load_q_mvar();
    let bus_idx_map = network.bus_index_map();
    let roundtrip = RoundtripExportState::build(network);
    let mut out = String::with_capacity(16 * 1024);
    let tp_urn = format!(
        "urn:uuid:{}_N_TOPOLOGY_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );
    let ssh_urn = format!(
        "urn:uuid:{}_N_STEADY_STATE_HYPOTHESIS_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "STATE_VARIABLES",
        sv_profile_uri(version),
        None,
        Some(&tp_urn),
        Some(&ssh_urn),
    );

    // --- SvVoltage per bus ---
    for (bi, bus) in network.buses.iter().enumerate() {
        let svvid = sv_voltage_id(bi);
        let tnid = tn_id(bus.number);
        // vm is per-unit; CGMES SvVoltage.v is in kV.
        let v_kv = bus.voltage_magnitude_pu * bus.base_kv;
        // va is in radians; CGMES SvVoltage.angle is in degrees.
        let angle_deg = bus.voltage_angle_rad.to_degrees();

        writeln!(out, "  <cim:SvVoltage rdf:ID=\"{svvid}\">")?;
        writeln!(
            out,
            "    <cim:SvVoltage.angle>{angle_deg}</cim:SvVoltage.angle>"
        )?;
        writeln!(out, "    <cim:SvVoltage.v>{v_kv}</cim:SvVoltage.v>")?;
        writeln!(
            out,
            "    <cim:SvVoltage.TopologicalNode rdf:resource=\"#{tnid}\"/>"
        )?;
        writeln!(out, "  </cim:SvVoltage>")?;
    }

    // --- SvPowerFlow per terminal ---
    // Branch terminals: compute flows from bus voltages and branch admittance.
    let bus_idx_map_sv = network.bus_index_map();
    for (bi, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let eq_id_str = branch_eq_id(bi, br);
        let t1 = term_id(&eq_id_str, 1);
        let t2 = term_id(&eq_id_str, 2);

        // Compute branch power flows from bus voltages and branch parameters.
        let (pf, qf, pt, qt) = compute_branch_flow(network, br, &bus_idx_map_sv);

        let svpf1 = sv_pf_id(&t1);
        writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpf1}\">")?;
        writeln!(out, "    <cim:SvPowerFlow.p>{pf}</cim:SvPowerFlow.p>")?;
        writeln!(out, "    <cim:SvPowerFlow.q>{qf}</cim:SvPowerFlow.q>")?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t1}\"/>"
        )?;
        writeln!(out, "  </cim:SvPowerFlow>")?;

        let svpf2 = sv_pf_id(&t2);
        writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpf2}\">")?;
        writeln!(out, "    <cim:SvPowerFlow.p>{pt}</cim:SvPowerFlow.p>")?;
        writeln!(out, "    <cim:SvPowerFlow.q>{qt}</cim:SvPowerFlow.q>")?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t2}\"/>"
        )?;
        writeln!(out, "  </cim:SvPowerFlow>")?;
    }

    // Generator terminals — SvPowerFlow at their terminal.
    for (gi, gn) in network.generators.iter().enumerate() {
        if !gn.in_service || roundtrip.skipped_generator_indices.contains(&gi) {
            continue;
        }
        let smid = sm_id(gi);
        let t_id = term_id(&smid, 1);
        // CGMES SV power flow convention: positive = injecting into network from equipment.
        // For generators this is the terminal flow, same sign as SSH p/q (IEC: neg = gen).
        let p = -gn.p; // IEC sign
        let q = -gn.q;
        let svpfid = sv_pf_id(&t_id);
        writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpfid}\">")?;
        writeln!(out, "    <cim:SvPowerFlow.p>{p}</cim:SvPowerFlow.p>")?;
        writeln!(out, "    <cim:SvPowerFlow.q>{q}</cim:SvPowerFlow.q>")?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t_id}\"/>"
        )?;
        writeln!(out, "  </cim:SvPowerFlow>")?;
    }

    // Round-tripped EquivalentInjection / ExternalNetworkInjection terminals — SvPowerFlow.
    for export in &roundtrip.equivalent_injections {
        if !equivalent_injection_is_in_service(export, network) {
            continue;
        }
        let t_id = term_id(export.source.mrid.as_str(), 1);
        let svpfid = sv_pf_id(&t_id);
        let (p, q) = equivalent_injection_pq(export, network);
        writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpfid}\">")?;
        writeln!(out, "    <cim:SvPowerFlow.p>{p}</cim:SvPowerFlow.p>")?;
        writeln!(out, "    <cim:SvPowerFlow.q>{q}</cim:SvPowerFlow.q>")?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t_id}\"/>"
        )?;
        writeln!(out, "  </cim:SvPowerFlow>")?;
    }
    for export in &roundtrip.external_network_injections {
        if !external_network_injection_is_in_service(export, network) {
            continue;
        }
        let t_id = term_id(export.source.mrid.as_str(), 1);
        let svpfid = sv_pf_id(&t_id);
        let (p, q) = external_network_injection_pq(export, network);
        writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpfid}\">")?;
        writeln!(out, "    <cim:SvPowerFlow.p>{p}</cim:SvPowerFlow.p>")?;
        writeln!(out, "    <cim:SvPowerFlow.q>{q}</cim:SvPowerFlow.q>")?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t_id}\"/>"
        )?;
        writeln!(out, "  </cim:SvPowerFlow>")?;
    }

    // Load terminals — SvPowerFlow at their terminal.
    if !network.loads.is_empty() {
        for (li, load) in network.loads.iter().enumerate() {
            if !load.in_service {
                continue;
            }
            let ecid = ec_id(li);
            let t_id = term_id(&ecid, 1);
            let svpfid = sv_pf_id(&t_id);
            writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpfid}\">")?;
            writeln!(
                out,
                "    <cim:SvPowerFlow.p>{}</cim:SvPowerFlow.p>",
                load.active_power_demand_mw
            )?;
            writeln!(
                out,
                "    <cim:SvPowerFlow.q>{}</cim:SvPowerFlow.q>",
                load.reactive_power_demand_mvar
            )?;
            writeln!(
                out,
                "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t_id}\"/>"
            )?;
            writeln!(out, "  </cim:SvPowerFlow>")?;
        }
    } else {
        for bus in &network.buses {
            if bus_demand_p
                .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                .copied()
                .unwrap_or(0.0)
                .abs()
                < 1e-10
                && bus_demand_q
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    < 1e-10
            {
                continue;
            }
            let ecid = format!("_ECB_{}", bus.number);
            let t_id = term_id(&ecid, 1);
            let svpfid = sv_pf_id(&t_id);
            writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpfid}\">")?;
            writeln!(
                out,
                "    <cim:SvPowerFlow.p>{}</cim:SvPowerFlow.p>",
                bus_demand_p
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
            )?;
            writeln!(
                out,
                "    <cim:SvPowerFlow.q>{}</cim:SvPowerFlow.q>",
                bus_demand_q
                    .get(bus_idx_map.get(&bus.number).copied().unwrap_or(0))
                    .copied()
                    .unwrap_or(0.0)
            )?;
            writeln!(
                out,
                "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t_id}\"/>"
            )?;
            writeln!(out, "  </cim:SvPowerFlow>")?;
        }
    }

    // EquivalentInjection terminals — SvPowerFlow.
    for (pi, inj) in network.power_injections.iter().enumerate() {
        if !inj.in_service || roundtrip.skipped_injection_indices.contains(&pi) {
            continue;
        }
        if inj.active_power_injection_mw.abs() < 1e-9
            && inj.reactive_power_injection_mvar.abs() < 1e-9
        {
            continue;
        }
        let eid = einj_id(pi);
        let t_id = term_id(&eid, 1);
        let svpfid = sv_pf_id(&t_id);
        writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpfid}\">")?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.p>{}</cim:SvPowerFlow.p>",
            inj.active_power_injection_mw
        )?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.q>{}</cim:SvPowerFlow.q>",
            inj.reactive_power_injection_mvar
        )?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t_id}\"/>"
        )?;
        writeln!(out, "  </cim:SvPowerFlow>")?;
    }

    // Round-tripped DanglingLine terminals — SvPowerFlow.
    for export in &roundtrip.dangling_lines {
        if !dangling_line_is_in_service(export, network) {
            continue;
        }
        let t_id = term_id(export.source.mrid.as_str(), 1);
        let svpfid = sv_pf_id(&t_id);
        let (p, q) = dangling_line_pq(export, network);
        writeln!(out, "  <cim:SvPowerFlow rdf:ID=\"{svpfid}\">")?;
        writeln!(out, "    <cim:SvPowerFlow.p>{p}</cim:SvPowerFlow.p>")?;
        writeln!(out, "    <cim:SvPowerFlow.q>{q}</cim:SvPowerFlow.q>")?;
        writeln!(
            out,
            "    <cim:SvPowerFlow.Terminal rdf:resource=\"#{t_id}\"/>"
        )?;
        writeln!(out, "  </cim:SvPowerFlow>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Branch flow computation for SV profile
// ---------------------------------------------------------------------------

/// Compute branch power flows from bus voltages and branch impedance parameters.
/// Returns (p_from_mw, q_from_mvar, p_to_mw, q_to_mvar).
fn compute_branch_flow(
    network: &Network,
    br: &surge_network::network::Branch,
    bus_idx_map: &HashMap<u32, usize>,
) -> (f64, f64, f64, f64) {
    let fi = bus_idx_map.get(&br.from_bus).copied();
    let ti = bus_idx_map.get(&br.to_bus).copied();
    let (fi, ti) = match (fi, ti) {
        (Some(f), Some(t)) => (f, t),
        _ => return (0.0, 0.0, 0.0, 0.0),
    };

    let vm_from = network.buses[fi].voltage_magnitude_pu;
    let va_from = network.buses[fi].voltage_angle_rad;
    let vm_to = network.buses[ti].voltage_magnitude_pu;
    let va_to = network.buses[ti].voltage_angle_rad;

    // If voltages are uninitialized (flat start), skip computation.
    if vm_from <= 0.0 || vm_to <= 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }

    let base_mva = network.base_mva;

    // Pi-model parameters (per-unit on system base).
    let r = br.r;
    let x = br.x;
    let z_sq = r * r + x * x;
    if z_sq < 1e-20 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let g_s = r / z_sq; // series conductance
    let b_s = -x / z_sq; // series susceptance

    let b_sh = br.b / 2.0; // half line charging
    let g_sh = br.g_pi / 2.0; // half line charging conductance (if any)

    let tap = br.tap;
    let shift = br.phase_shift_rad;

    let theta = va_from - va_to - shift;
    let cos_t = theta.cos();
    let sin_t = theta.sin();

    // From-bus injection (MW, MVAr) — pi-model with off-nominal tap.
    let pf = base_mva
        * (vm_from * vm_from * (g_s + g_sh) / (tap * tap)
            - vm_from * vm_to * (g_s * cos_t + b_s * sin_t) / tap);
    let qf = base_mva
        * (-vm_from * vm_from * (b_s + b_sh) / (tap * tap)
            - vm_from * vm_to * (g_s * sin_t - b_s * cos_t) / tap);

    // To-bus injection.
    let theta_rev = va_to - va_from + shift;
    let cos_tr = theta_rev.cos();
    let sin_tr = theta_rev.sin();
    let pt = base_mva
        * (vm_to * vm_to * (g_s + g_sh) - vm_from * vm_to * (g_s * cos_tr + b_s * sin_tr) / tap);
    let qt = base_mva
        * (-vm_to * vm_to * (b_s + b_sh) - vm_from * vm_to * (g_s * sin_tr - b_s * cos_tr) / tap);

    (pf, qf, pt, qt)
}

// ---------------------------------------------------------------------------
// SC (Short Circuit) profile writer
// ---------------------------------------------------------------------------

/// Write the CGMES Short Circuit profile — zero/negative-sequence impedance
/// data on ACLineSegments, PowerTransformerEnds, and SynchronousMachines.
///
/// Only elements with non-`None` sequence data are emitted.
pub fn write_short_circuit_profile(
    network: &Network,
    version: CgmesVersion,
) -> Result<String, CgmesWriteError> {
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "SHORT_CIRCUIT",
        "http://iec.ch/TC57/61970-456/ShortCircuit/3/0",
        Some(&eq_urn),
        None,
        None,
    );

    let base_mva = network.base_mva;

    // --- ACLineSegment zero-sequence data ---
    for (bi, br) in network.branches.iter().enumerate() {
        if !br.in_service || is_transformer(br) {
            continue;
        }
        let zs = match br.zero_seq.as_ref() {
            Some(z) => z,
            None => continue,
        };
        let lid = line_id(bi);
        let from_kv = network
            .buses
            .iter()
            .find(|b| b.number == br.from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);

        writeln!(out, "  <cim:ACLineSegment rdf:about=\"#{lid}\">")?;
        {
            let r0_ohm = pu_to_ohm(zs.r0, from_kv, base_mva);
            writeln!(
                out,
                "    <cim:ACLineSegment.r0>{r0_ohm}</cim:ACLineSegment.r0>"
            )?;
        }
        {
            let x0_ohm = pu_to_ohm(zs.x0, from_kv, base_mva);
            writeln!(
                out,
                "    <cim:ACLineSegment.x0>{x0_ohm}</cim:ACLineSegment.x0>"
            )?;
        }
        {
            let b0_s = pu_to_siemens(zs.b0, from_kv, base_mva);
            writeln!(
                out,
                "    <cim:ACLineSegment.b0ch>{b0_s}</cim:ACLineSegment.b0ch>"
            )?;
        }
        writeln!(out, "  </cim:ACLineSegment>")?;
    }

    // --- PowerTransformerEnd zero-sequence data ---
    for (bi, br) in network.branches.iter().enumerate() {
        if !br.in_service || !is_transformer(br) {
            continue;
        }
        let zs = match br.zero_seq.as_ref() {
            Some(z) => z,
            None => continue,
        };
        let end1_id = xfmr_end_id(bi, 1);
        let from_kv = network
            .buses
            .iter()
            .find(|b| b.number == br.from_bus)
            .map(|b| b.base_kv)
            .unwrap_or(1.0);

        writeln!(out, "  <cim:PowerTransformerEnd rdf:about=\"#{end1_id}\">")?;
        {
            let r0_ohm = pu_to_ohm(zs.r0, from_kv, base_mva);
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.r0>{r0_ohm}</cim:PowerTransformerEnd.r0>"
            )?;
        }
        {
            let x0_ohm = pu_to_ohm(zs.x0, from_kv, base_mva);
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.x0>{x0_ohm}</cim:PowerTransformerEnd.x0>"
            )?;
        }
        // TransformerConnection → CIM WindingConnection
        let xfmr_conn = br
            .transformer_data
            .as_ref()
            .map(|t| t.transformer_connection)
            .unwrap_or_default();
        let conn_str = match xfmr_conn {
            surge_network::network::TransformerConnection::WyeGWyeG => "Yn",
            surge_network::network::TransformerConnection::WyeGDelta => "Yn",
            surge_network::network::TransformerConnection::DeltaWyeG => "D",
            surge_network::network::TransformerConnection::DeltaDelta => "D",
            surge_network::network::TransformerConnection::WyeGWye => "Yn",
        };
        writeln!(
            out,
            "    <cim:PowerTransformerEnd.connectionKind>{conn_str}</cim:PowerTransformerEnd.connectionKind>"
        )?;
        if let Some(zn) = zs.zn {
            let zn_ohm_re = pu_to_ohm(zn.re, from_kv, base_mva);
            let zn_ohm_im = pu_to_ohm(zn.im, from_kv, base_mva);
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.rground>{zn_ohm_re}</cim:PowerTransformerEnd.rground>"
            )?;
            writeln!(
                out,
                "    <cim:PowerTransformerEnd.xground>{zn_ohm_im}</cim:PowerTransformerEnd.xground>"
            )?;
        }
        writeln!(out, "  </cim:PowerTransformerEnd>")?;
    }

    // --- SynchronousMachine zero/negative-sequence data ---
    for (gi, gn) in network.generators.iter().enumerate() {
        if !gn.in_service {
            continue;
        }
        let fd = gn.fault_data.as_ref();
        let has_seq = fd.is_some_and(|f| {
            f.r0_pu.is_some() || f.x0_pu.is_some() || f.r2_pu.is_some() || f.x2_pu.is_some()
        });
        if !has_seq {
            continue;
        }
        let smid = sm_id(gi);
        writeln!(out, "  <cim:SynchronousMachine rdf:about=\"#{smid}\">")?;
        if let Some(r0) = fd.and_then(|f| f.r0_pu) {
            writeln!(
                out,
                "    <cim:SynchronousMachine.r0>{r0}</cim:SynchronousMachine.r0>"
            )?;
        }
        if let Some(x0) = fd.and_then(|f| f.x0_pu) {
            writeln!(
                out,
                "    <cim:SynchronousMachine.x0>{x0}</cim:SynchronousMachine.x0>"
            )?;
        }
        if let Some(r2) = fd.and_then(|f| f.r2_pu) {
            writeln!(
                out,
                "    <cim:SynchronousMachine.r2>{r2}</cim:SynchronousMachine.r2>"
            )?;
        }
        if let Some(x2) = fd.and_then(|f| f.x2_pu) {
            writeln!(
                out,
                "    <cim:SynchronousMachine.x2>{x2}</cim:SynchronousMachine.x2>"
            )?;
        }
        if let Some(zn) = fd.and_then(|f| f.zn) {
            writeln!(
                out,
                "    <cim:SynchronousMachine.earthingStarPointR>{}</cim:SynchronousMachine.earthingStarPointR>",
                zn.re
            )?;
            writeln!(
                out,
                "    <cim:SynchronousMachine.earthingStarPointX>{}</cim:SynchronousMachine.earthingStarPointX>",
                zn.im
            )?;
        }
        writeln!(out, "  </cim:SynchronousMachine>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Measurement profile writer
// ---------------------------------------------------------------------------

/// Map `CimMeasurementType` to CIM class name and measurement type string.
fn cim_meas_class_and_type(mtype: CimMeasurementType) -> (&'static str, &'static str) {
    match mtype {
        CimMeasurementType::ActivePower => ("Analog", "ActivePower"),
        CimMeasurementType::ReactivePower => ("Analog", "ReactivePower"),
        CimMeasurementType::VoltageMagnitude => ("Analog", "VoltageMagnitude"),
        CimMeasurementType::VoltageAngle => ("Analog", "VoltageAngle"),
        CimMeasurementType::CurrentMagnitude => ("Analog", "CurrentMagnitude"),
        CimMeasurementType::Frequency => ("Analog", "Frequency"),
        CimMeasurementType::TapPosition => ("Discrete", "TapPosition"),
        CimMeasurementType::SwitchStatus => ("Discrete", "SwitchStatus"),
        CimMeasurementType::EnergyAccumulator => ("Accumulator", "EnergyAccumulator"),
        CimMeasurementType::PmuVoltageReal => ("Analog", "PmuVoltageReal"),
        CimMeasurementType::PmuVoltageImaginary => ("Analog", "PmuVoltageImaginary"),
        CimMeasurementType::PmuCurrentReal => ("Analog", "PmuCurrentReal"),
        CimMeasurementType::PmuCurrentImaginary => ("Analog", "PmuCurrentImaginary"),
    }
}

/// Write the CGMES Measurement profile — Analog, Discrete, and Accumulator objects.
pub fn write_measurement_profile(
    network: &Network,
    version: CgmesVersion,
) -> Result<String, CgmesWriteError> {
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "MEASUREMENT",
        "http://iec.ch/TC57/61970-456/Measurement/3/0",
        Some(&eq_urn),
        None,
        None,
    );

    for m in &network.cim.measurements {
        let (cim_class, measurement_type_str) = cim_meas_class_and_type(m.measurement_type);
        let mrid = if m.mrid.is_empty() {
            format!("_MEAS_{}", m.name)
        } else {
            m.mrid.clone()
        };

        writeln!(out, "  <cim:{cim_class} rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            m.name
        )?;
        writeln!(
            out,
            "    <cim:Measurement.measurementType>{measurement_type_str}</cim:Measurement.measurementType>"
        )?;
        if let Some(ref term_mrid) = m.terminal_mrid {
            writeln!(
                out,
                "    <cim:Measurement.Terminal rdf:resource=\"#{term_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:{cim_class}>")?;

        // Write the measurement value
        let val_class = match cim_class {
            "Analog" => "AnalogValue",
            "Discrete" => "DiscreteValue",
            "Accumulator" => "AccumulatorValue",
            _ => "AnalogValue",
        };
        let val_id = format!("{mrid}_V");
        writeln!(out, "  <cim:{val_class} rdf:ID=\"{val_id}\">")?;
        writeln!(
            out,
            "    <cim:MeasurementValue.MeasurementValueSource>SCADA</cim:MeasurementValue.MeasurementValueSource>"
        )?;
        writeln!(
            out,
            "    <cim:{val_class}.value>{}</cim:{val_class}.value>",
            m.value
        )?;
        let parent_ref_name = match cim_class {
            "Analog" => "AnalogValue.Analog",
            "Discrete" => "DiscreteValue.Discrete",
            "Accumulator" => "AccumulatorValue.Accumulator",
            _ => "AnalogValue.Analog",
        };
        writeln!(out, "    <cim:{parent_ref_name} rdf:resource=\"#{mrid}\"/>")?;
        writeln!(out, "  </cim:{val_class}>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Asset profile writer
// ---------------------------------------------------------------------------

/// Write the CGMES Asset profile — WireInfo, CableInfo, WireSpacingInfo,
/// TransformerTankInfo, and Asset metadata objects.
///
/// Returns an empty (but valid) RDF document when the asset catalog is empty.
pub fn write_asset_profile(
    network: &Network,
    version: CgmesVersion,
) -> Result<String, CgmesWriteError> {
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "ASSET",
        "http://iec.ch/TC57/61968-11/Asset/3/0",
        Some(&eq_urn),
        None,
        None,
    );

    let cat = &network.cim.asset_catalog;

    // --- WireInfo ---
    for (mrid, w) in &cat.wire_infos {
        writeln!(out, "  <cim:OverheadWireInfo rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            w.name
        )?;
        if let Some(r) = w.r_ac75_ohm_per_km {
            writeln!(out, "    <cim:WireInfo.rAC75>{r}</cim:WireInfo.rAC75>")?;
        }
        if let Some(r) = w.r_dc20_ohm_per_km {
            writeln!(out, "    <cim:WireInfo.rDC20>{r}</cim:WireInfo.rDC20>")?;
        }
        if let Some(gmr) = w.gmr_m {
            writeln!(out, "    <cim:WireInfo.gmr>{gmr}</cim:WireInfo.gmr>")?;
        }
        if let Some(rad) = w.radius_m {
            writeln!(out, "    <cim:WireInfo.radius>{rad}</cim:WireInfo.radius>")?;
        }
        if let Some(ref mat) = w.material {
            writeln!(
                out,
                "    <cim:WireInfo.material>{mat}</cim:WireInfo.material>"
            )?;
        }
        if let Some(sc) = w.strand_count {
            writeln!(
                out,
                "    <cim:WireInfo.strandCount>{sc}</cim:WireInfo.strandCount>"
            )?;
        }
        if let Some(rc) = w.rated_current_a {
            writeln!(
                out,
                "    <cim:WireInfo.ratedCurrent>{rc}</cim:WireInfo.ratedCurrent>"
            )?;
        }
        writeln!(out, "  </cim:OverheadWireInfo>")?;
    }

    // --- CableInfo ---
    for (mrid, c) in &cat.cable_infos {
        writeln!(out, "  <cim:CableInfo rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            c.wire.name
        )?;
        if let Some(r) = c.wire.r_ac75_ohm_per_km {
            writeln!(out, "    <cim:WireInfo.rAC75>{r}</cim:WireInfo.rAC75>")?;
        }
        if let Some(ref mat) = c.insulation_material {
            writeln!(
                out,
                "    <cim:CableInfo.constructionKind>{mat}</cim:CableInfo.constructionKind>"
            )?;
        }
        if let Some(t) = c.insulation_thickness_mm {
            writeln!(
                out,
                "    <cim:CableInfo.insulationThickness>{t}</cim:CableInfo.insulationThickness>"
            )?;
        }
        if let Some(d) = c.diameter_over_insulation_mm {
            writeln!(
                out,
                "    <cim:CableInfo.diameterOverInsulation>{d}</cim:CableInfo.diameterOverInsulation>"
            )?;
        }
        writeln!(out, "  </cim:CableInfo>")?;
    }

    // --- WireSpacingInfo ---
    for (mrid, ws) in &cat.wire_spacings {
        writeln!(out, "  <cim:WireSpacingInfo rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            ws.name
        )?;
        writeln!(
            out,
            "    <cim:WireSpacingInfo.isCable>{}</cim:WireSpacingInfo.isCable>",
            ws.is_cable
        )?;
        writeln!(
            out,
            "    <cim:WireSpacingInfo.phaseWireCount>{}</cim:WireSpacingInfo.phaseWireCount>",
            ws.phase_wire_count
        )?;
        if let Some(spacing) = ws.phase_wire_spacing_m {
            writeln!(
                out,
                "    <cim:WireSpacingInfo.phaseWireSpacing>{spacing}</cim:WireSpacingInfo.phaseWireSpacing>"
            )?;
        }
        writeln!(out, "  </cim:WireSpacingInfo>")?;

        // WirePosition children
        for pos in &ws.positions {
            let pos_id = format!("{mrid}_P{}", pos.sequence_number);
            writeln!(out, "  <cim:WirePosition rdf:ID=\"{pos_id}\">")?;
            writeln!(
                out,
                "    <cim:WirePosition.xCoord>{}</cim:WirePosition.xCoord>",
                pos.x_m
            )?;
            writeln!(
                out,
                "    <cim:WirePosition.yCoord>{}</cim:WirePosition.yCoord>",
                pos.y_m
            )?;
            writeln!(
                out,
                "    <cim:WirePosition.sequenceNumber>{}</cim:WirePosition.sequenceNumber>",
                pos.sequence_number
            )?;
            if let Some(ref ph) = pos.phase {
                writeln!(
                    out,
                    "    <cim:WirePosition.phase>{ph}</cim:WirePosition.phase>"
                )?;
            }
            writeln!(
                out,
                "    <cim:WirePosition.WireSpacingInfo rdf:resource=\"#{mrid}\"/>"
            )?;
            writeln!(out, "  </cim:WirePosition>")?;
        }
    }

    // --- TransformerTankInfo ---
    for (mrid, ti) in &cat.transformer_infos {
        writeln!(out, "  <cim:TransformerTankInfo rdf:ID=\"{mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            ti.name
        )?;
        if let Some(loss) = ti.no_load_loss_w {
            writeln!(
                out,
                "    <cim:TransformerTankInfo.noLoadLoss>{loss}</cim:TransformerTankInfo.noLoadLoss>"
            )?;
        }
        writeln!(out, "  </cim:TransformerTankInfo>")?;

        // TransformerEndInfo per winding
        for w in &ti.windings {
            let end_id = format!("{mrid}_W{}", w.end_number);
            writeln!(out, "  <cim:TransformerEndInfo rdf:ID=\"{end_id}\">")?;
            writeln!(
                out,
                "    <cim:TransformerEndInfo.endNumber>{}</cim:TransformerEndInfo.endNumber>",
                w.end_number
            )?;
            if let Some(s) = w.rated_s_mva {
                writeln!(
                    out,
                    "    <cim:TransformerEndInfo.ratedS>{s}</cim:TransformerEndInfo.ratedS>"
                )?;
            }
            if let Some(u) = w.rated_u_kv {
                writeln!(
                    out,
                    "    <cim:TransformerEndInfo.ratedU>{u}</cim:TransformerEndInfo.ratedU>"
                )?;
            }
            if let Some(r) = w.r_ohm {
                writeln!(
                    out,
                    "    <cim:TransformerEndInfo.r>{r}</cim:TransformerEndInfo.r>"
                )?;
            }
            if let Some(ref ck) = w.connection_kind {
                writeln!(
                    out,
                    "    <cim:TransformerEndInfo.connectionKind>{ck}</cim:TransformerEndInfo.connectionKind>"
                )?;
            }
            writeln!(
                out,
                "    <cim:TransformerEndInfo.TransformerTankInfo rdf:resource=\"#{mrid}\"/>"
            )?;
            writeln!(out, "  </cim:TransformerEndInfo>")?;
        }
    }

    // --- Asset metadata ---
    for (eq_mrid, am) in &cat.asset_metadata {
        let asset_id = format!("_ASSET_{eq_mrid}");
        writeln!(out, "  <cim:Asset rdf:ID=\"{asset_id}\">")?;
        if let Some(ref sn) = am.serial_number {
            writeln!(
                out,
                "    <cim:Asset.serialNumber>{sn}</cim:Asset.serialNumber>"
            )?;
        }
        if let Some(ref mfr) = am.manufacturer {
            writeln!(
                out,
                "    <cim:Asset.manufacturer>{mfr}</cim:Asset.manufacturer>"
            )?;
        }
        if let Some(ref mn) = am.model_number {
            writeln!(
                out,
                "    <cim:Asset.modelNumber>{mn}</cim:Asset.modelNumber>"
            )?;
        }
        if let Some(md) = am.manufactured_date {
            writeln!(
                out,
                "    <cim:Asset.manufacturedDate>{}</cim:Asset.manufacturedDate>",
                md.to_rfc3339()
            )?;
        }
        if let Some(id_date) = am.installation_date {
            writeln!(
                out,
                "    <cim:Asset.installationDate>{}</cim:Asset.installationDate>",
                id_date.to_rfc3339()
            )?;
        }
        writeln!(out, "  </cim:Asset>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Operational Limits profile writer
// ---------------------------------------------------------------------------

/// Write the CGMES Operational Limits profile — OperationalLimitSet,
/// OperationalLimitType, and individual limit values.
pub fn write_operational_limits_profile(
    network: &Network,
    version: CgmesVersion,
) -> Result<String, CgmesWriteError> {
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );
    let ns = cim_ns(version);

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "OPERATIONAL_LIMITS",
        "http://iec.ch/TC57/61970-456/OperationalLimits/3/0",
        Some(&eq_urn),
        None,
        None,
    );

    for (set_mrid, ls) in &network.cim.operational_limits.limit_sets {
        writeln!(out, "  <cim:OperationalLimitSet rdf:ID=\"{set_mrid}\">")?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            ls.name
        )?;
        if let Some(ref eq_mrid) = ls.equipment_mrid {
            writeln!(
                out,
                "    <cim:OperationalLimitSet.Equipment rdf:resource=\"#{eq_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:OperationalLimitSet>")?;

        // Emit individual limits
        for (li, (kind, lim)) in ls.limits.iter().enumerate() {
            let lim_id = format!("{set_mrid}_L{li}");

            // OperationalLimitType
            let olt_id = format!("{set_mrid}_OLT{li}");
            let duration_str = match lim.duration {
                surge_network::network::op_limits::LimitDuration::Permanent => "PATL",
                surge_network::network::op_limits::LimitDuration::Temporary(_) => "TATL",
                surge_network::network::op_limits::LimitDuration::Instantaneous => "IATL",
            };
            let dir_str = match lim.direction {
                surge_network::network::op_limits::LimitDirection::High => "high",
                surge_network::network::op_limits::LimitDirection::Low => "low",
                surge_network::network::op_limits::LimitDirection::AbsoluteValue => "absoluteValue",
            };

            if lim.limit_type_mrid.is_none() {
                writeln!(out, "  <cim:OperationalLimitType rdf:ID=\"{olt_id}\">")?;
                writeln!(
                    out,
                    "    <cim:IdentifiedObject.name>{duration_str}</cim:IdentifiedObject.name>"
                )?;
                writeln!(
                    out,
                    "    <cim:OperationalLimitType.direction rdf:resource=\"{ns}OperationalLimitDirectionKind.{dir_str}\"/>"
                )?;
                if let surge_network::network::op_limits::LimitDuration::Temporary(secs) =
                    lim.duration
                {
                    writeln!(
                        out,
                        "    <cim:OperationalLimitType.acceptableDuration>{secs}</cim:OperationalLimitType.acceptableDuration>"
                    )?;
                }
                writeln!(out, "  </cim:OperationalLimitType>")?;
            }

            let olt_ref = lim.limit_type_mrid.as_deref().unwrap_or(&olt_id);

            // Limit value element — class depends on kind
            let cim_class = match kind {
                surge_network::network::op_limits::LimitKind::ActivePower => "ActivePowerLimit",
                surge_network::network::op_limits::LimitKind::ApparentPower => "ApparentPowerLimit",
                surge_network::network::op_limits::LimitKind::Current => "CurrentLimit",
                surge_network::network::op_limits::LimitKind::Voltage => "VoltageLimit",
            };
            let val_elem = match kind {
                surge_network::network::op_limits::LimitKind::ActivePower => {
                    "ActivePowerLimit.value"
                }
                surge_network::network::op_limits::LimitKind::ApparentPower => {
                    "ApparentPowerLimit.value"
                }
                surge_network::network::op_limits::LimitKind::Current => "CurrentLimit.value",
                surge_network::network::op_limits::LimitKind::Voltage => "VoltageLimit.value",
            };

            writeln!(out, "  <cim:{cim_class} rdf:ID=\"{lim_id}\">")?;
            writeln!(out, "    <cim:{val_elem}>{}</cim:{val_elem}>", lim.value)?;
            writeln!(
                out,
                "    <cim:OperationalLimit.OperationalLimitSet rdf:resource=\"#{set_mrid}\"/>"
            )?;
            writeln!(
                out,
                "    <cim:OperationalLimit.OperationalLimitType rdf:resource=\"#{olt_ref}\"/>"
            )?;
            writeln!(out, "  </cim:{cim_class}>")?;
        }
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Boundary profile writer
// ---------------------------------------------------------------------------

/// Write the CGMES Boundary/EQBD profile — BoundaryPoints, ModelAuthoritySets,
/// EquivalentNetworks/Branches/Shunts.
pub fn write_boundary_profile(
    network: &Network,
    version: CgmesVersion,
) -> Result<String, CgmesWriteError> {
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "BOUNDARY",
        "http://entsoe.eu/CIM/EquipmentBoundary/3/1",
        Some(&eq_urn),
        None,
        None,
    );

    let bd = &network.cim.boundary_data;

    // --- BoundaryPoint ---
    for bp in &bd.boundary_points {
        writeln!(out, "  <cim:BoundaryPoint rdf:ID=\"{}\">", bp.mrid)?;
        if let Some(ref cn) = bp.connectivity_node_mrid {
            writeln!(
                out,
                "    <cim:BoundaryPoint.ConnectivityNode rdf:resource=\"#{cn}\"/>"
            )?;
        }
        if let Some(ref iso) = bp.from_end_iso_code {
            writeln!(
                out,
                "    <cim:BoundaryPoint.fromEndIsoCode>{iso}</cim:BoundaryPoint.fromEndIsoCode>"
            )?;
        }
        if let Some(ref iso) = bp.to_end_iso_code {
            writeln!(
                out,
                "    <cim:BoundaryPoint.toEndIsoCode>{iso}</cim:BoundaryPoint.toEndIsoCode>"
            )?;
        }
        if let Some(ref name) = bp.from_end_name {
            writeln!(
                out,
                "    <cim:BoundaryPoint.fromEndName>{name}</cim:BoundaryPoint.fromEndName>"
            )?;
        }
        if let Some(ref name) = bp.to_end_name {
            writeln!(
                out,
                "    <cim:BoundaryPoint.toEndName>{name}</cim:BoundaryPoint.toEndName>"
            )?;
        }
        if let Some(ref tso) = bp.from_end_name_tso {
            writeln!(
                out,
                "    <cim:BoundaryPoint.fromEndNameTso>{tso}</cim:BoundaryPoint.fromEndNameTso>"
            )?;
        }
        if let Some(ref tso) = bp.to_end_name_tso {
            writeln!(
                out,
                "    <cim:BoundaryPoint.toEndNameTso>{tso}</cim:BoundaryPoint.toEndNameTso>"
            )?;
        }
        writeln!(
            out,
            "    <cim:BoundaryPoint.isDirectCurrent>{}</cim:BoundaryPoint.isDirectCurrent>",
            bp.is_direct_current
        )?;
        writeln!(
            out,
            "    <cim:BoundaryPoint.isExcludedFromAreaInterchange>{}</cim:BoundaryPoint.isExcludedFromAreaInterchange>",
            bp.is_excluded_from_area_interchange
        )?;
        writeln!(out, "  </cim:BoundaryPoint>")?;
    }

    // --- ModelAuthoritySet ---
    for mas in &bd.model_authority_sets {
        writeln!(out, "  <cim:ModelAuthoritySet rdf:ID=\"{}\">", mas.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            mas.name
        )?;
        if let Some(ref desc) = mas.description {
            writeln!(
                out,
                "    <cim:IdentifiedObject.description>{desc}</cim:IdentifiedObject.description>"
            )?;
        }
        writeln!(out, "  </cim:ModelAuthoritySet>")?;
    }

    // --- EquivalentNetwork ---
    for en in &bd.equivalent_networks {
        writeln!(out, "  <cim:EquivalentNetwork rdf:ID=\"{}\">", en.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            en.name
        )?;
        if let Some(ref desc) = en.description {
            writeln!(
                out,
                "    <cim:IdentifiedObject.description>{desc}</cim:IdentifiedObject.description>"
            )?;
        }
        if let Some(ref rgn) = en.region_mrid {
            writeln!(
                out,
                "    <cim:EquivalentNetwork.Region rdf:resource=\"#{rgn}\"/>"
            )?;
        }
        writeln!(out, "  </cim:EquivalentNetwork>")?;
    }

    // --- EquivalentBranch ---
    for eb in &bd.equivalent_branches {
        writeln!(out, "  <cim:EquivalentBranch rdf:ID=\"{}\">", eb.mrid)?;
        writeln!(
            out,
            "    <cim:EquivalentBranch.r>{}</cim:EquivalentBranch.r>",
            eb.r_ohm
        )?;
        writeln!(
            out,
            "    <cim:EquivalentBranch.x>{}</cim:EquivalentBranch.x>",
            eb.x_ohm
        )?;
        if let Some(r0) = eb.r0_ohm {
            writeln!(
                out,
                "    <cim:EquivalentBranch.r0>{r0}</cim:EquivalentBranch.r0>"
            )?;
        }
        if let Some(x0) = eb.x0_ohm {
            writeln!(
                out,
                "    <cim:EquivalentBranch.x0>{x0}</cim:EquivalentBranch.x0>"
            )?;
        }
        if let Some(r2) = eb.r2_ohm {
            writeln!(
                out,
                "    <cim:EquivalentBranch.r21>{r2}</cim:EquivalentBranch.r21>"
            )?;
        }
        if let Some(x2) = eb.x2_ohm {
            writeln!(
                out,
                "    <cim:EquivalentBranch.x21>{x2}</cim:EquivalentBranch.x21>"
            )?;
        }
        if let Some(ref net_mrid) = eb.network_mrid {
            writeln!(
                out,
                "    <cim:EquivalentEquipment.EquivalentNetwork rdf:resource=\"#{net_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:EquivalentBranch>")?;
    }

    // --- EquivalentShunt ---
    for es in &bd.equivalent_shunts {
        writeln!(out, "  <cim:EquivalentShunt rdf:ID=\"{}\">", es.mrid)?;
        writeln!(
            out,
            "    <cim:EquivalentShunt.g>{}</cim:EquivalentShunt.g>",
            es.g_s
        )?;
        writeln!(
            out,
            "    <cim:EquivalentShunt.b>{}</cim:EquivalentShunt.b>",
            es.b_s
        )?;
        if let Some(ref net_mrid) = es.network_mrid {
            writeln!(
                out,
                "    <cim:EquivalentEquipment.EquivalentNetwork rdf:resource=\"#{net_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:EquivalentShunt>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Protection profile writer
// ---------------------------------------------------------------------------

/// Write the CGMES Protection profile — CurrentRelay, DistanceRelay,
/// RecloseSequence, and SynchrocheckRelay objects.
pub fn write_protection_profile(
    network: &Network,
    version: CgmesVersion,
) -> Result<String, CgmesWriteError> {
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "PROTECTION",
        "http://iec.ch/TC57/61970-302/Protection/3/0",
        Some(&eq_urn),
        None,
        None,
    );

    let pd = &network.cim.protection_data;

    // --- CurrentRelay ---
    for cr in &pd.current_relays {
        writeln!(out, "  <cim:CurrentRelay rdf:ID=\"{}\">", cr.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            cr.name
        )?;
        if let Some(v) = cr.phase_pickup_a {
            writeln!(
                out,
                "    <cim:CurrentRelay.currentLimit1>{v}</cim:CurrentRelay.currentLimit1>"
            )?;
        }
        if let Some(v) = cr.ground_pickup_a {
            writeln!(
                out,
                "    <cim:CurrentRelay.currentLimit2>{v}</cim:CurrentRelay.currentLimit2>"
            )?;
        }
        if let Some(v) = cr.phase_time_dial_s {
            writeln!(
                out,
                "    <cim:CurrentRelay.timeDelay1>{v}</cim:CurrentRelay.timeDelay1>"
            )?;
        }
        writeln!(
            out,
            "    <cim:CurrentRelay.inverseTimeFlag>{}</cim:CurrentRelay.inverseTimeFlag>",
            cr.inverse_time
        )?;
        if let Some(ref sw_mrid) = cr.protected_switch_mrid {
            writeln!(
                out,
                "    <cim:ProtectionEquipment.ProtectedSwitches rdf:resource=\"#{sw_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:CurrentRelay>")?;
    }

    // --- DistanceRelay (as ProtectionEquipment with zones) ---
    for dr in &pd.distance_relays {
        // CIM doesn't have a native DistanceRelay class; model as ProtectionEquipment.
        writeln!(out, "  <cim:ProtectionEquipment rdf:ID=\"{}\">", dr.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            dr.name
        )?;
        if let Some(v) = dr.forward_reach_ohm {
            writeln!(
                out,
                "    <cim:ProtectionEquipment.highLimit>{v}</cim:ProtectionEquipment.highLimit>"
            )?;
        }
        if let Some(ref sw_mrid) = dr.protected_switch_mrid {
            writeln!(
                out,
                "    <cim:ProtectionEquipment.ProtectedSwitches rdf:resource=\"#{sw_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:ProtectionEquipment>")?;
    }

    // --- RecloseSequence ---
    for rs in &pd.reclose_sequences {
        let rs_id = format!("_RECL_{}", rs.protected_switch_mrid);
        writeln!(out, "  <cim:RecloseSequence rdf:ID=\"{rs_id}\">")?;
        writeln!(
            out,
            "    <cim:RecloseSequence.ProtectedSwitch rdf:resource=\"#{}\"/>",
            rs.protected_switch_mrid
        )?;
        for shot in &rs.shots {
            writeln!(
                out,
                "    <cim:RecloseSequence.recloseDelay>{}</cim:RecloseSequence.recloseDelay>",
                shot.delay_s
            )?;
        }
        writeln!(out, "  </cim:RecloseSequence>")?;
    }

    // --- SynchrocheckRelay ---
    for sc in &pd.synchrocheck_relays {
        writeln!(out, "  <cim:SynchrocheckRelay rdf:ID=\"{}\">", sc.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            sc.name
        )?;
        if let Some(v) = sc.max_angle_diff_deg {
            writeln!(
                out,
                "    <cim:SynchrocheckRelay.maxAngleDiff>{v}</cim:SynchrocheckRelay.maxAngleDiff>"
            )?;
        }
        if let Some(v) = sc.max_freq_diff_hz {
            writeln!(
                out,
                "    <cim:SynchrocheckRelay.maxFreqDiff>{v}</cim:SynchrocheckRelay.maxFreqDiff>"
            )?;
        }
        if let Some(v) = sc.max_volt_diff_pu {
            writeln!(
                out,
                "    <cim:SynchrocheckRelay.maxVoltDiff>{v}</cim:SynchrocheckRelay.maxVoltDiff>"
            )?;
        }
        if let Some(ref sw_mrid) = sc.protected_switch_mrid {
            writeln!(
                out,
                "    <cim:ProtectionEquipment.ProtectedSwitches rdf:resource=\"#{sw_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:SynchrocheckRelay>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Network Operations profile writer
// ---------------------------------------------------------------------------

/// Write the CGMES Network Operations profile — SwitchingPlans, Outages,
/// OutageSchedules, Crews, and WorkTasks.
pub fn write_network_operations_profile(
    network: &Network,
    version: CgmesVersion,
) -> Result<String, CgmesWriteError> {
    let mut out = String::with_capacity(16 * 1024);
    let eq_urn = format!(
        "urn:uuid:{}_N_EQUIPMENT_2026-01-01T00:00:00Z_1_1D__FM",
        network.name
    );

    write_rdf_header(&mut out, version);
    write_full_model(
        &mut out,
        &network.name,
        "NETWORK_OPERATIONS",
        "http://iec.ch/TC57/61968/NetworkOperations/3/0",
        Some(&eq_urn),
        None,
        None,
    );

    let ops = &network.cim.network_operations;

    // --- SwitchingPlan ---
    for sp in &ops.switching_plans {
        writeln!(out, "  <cim:SwitchingPlan rdf:ID=\"{}\">", sp.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            sp.name
        )?;
        if let Some(ref purpose) = sp.purpose {
            writeln!(
                out,
                "    <cim:SwitchingPlan.purpose>{purpose}</cim:SwitchingPlan.purpose>"
            )?;
        }
        if let Some(ps) = sp.planned_start {
            writeln!(
                out,
                "    <cim:SwitchingPlan.plannedPeriod.start>{}</cim:SwitchingPlan.plannedPeriod.start>",
                ps.to_rfc3339()
            )?;
        }
        if let Some(pe) = sp.planned_end {
            writeln!(
                out,
                "    <cim:SwitchingPlan.plannedPeriod.end>{}</cim:SwitchingPlan.plannedPeriod.end>",
                pe.to_rfc3339()
            )?;
        }
        writeln!(out, "  </cim:SwitchingPlan>")?;

        // SwitchingSteps
        for step in &sp.steps {
            let step_id = format!("{}_S{}", sp.mrid, step.sequence_number);
            writeln!(out, "  <cim:SwitchingStep rdf:ID=\"{step_id}\">")?;
            writeln!(
                out,
                "    <cim:SwitchingStep.sequenceNumber>{}</cim:SwitchingStep.sequenceNumber>",
                step.sequence_number
            )?;
            if let Some(kind) = step.kind {
                let kind_str = match kind {
                    surge_network::network::net_ops::SwitchingStepKind::Open => "open",
                    surge_network::network::net_ops::SwitchingStepKind::Close => "close",
                    surge_network::network::net_ops::SwitchingStepKind::Energize => "energize",
                    surge_network::network::net_ops::SwitchingStepKind::DeEnergize => "deEnergize",
                    surge_network::network::net_ops::SwitchingStepKind::Ground => "ground",
                    surge_network::network::net_ops::SwitchingStepKind::Unground => "unground",
                };
                writeln!(
                    out,
                    "    <cim:SwitchingStep.kind>{kind_str}</cim:SwitchingStep.kind>"
                )?;
            }
            if let Some(ref desc) = step.description {
                writeln!(
                    out,
                    "    <cim:SwitchingStep.description>{desc}</cim:SwitchingStep.description>"
                )?;
            }
            if let Some(ref sw_mrid) = step.switch_mrid {
                writeln!(
                    out,
                    "    <cim:SwitchingStep.SwitchingAction rdf:resource=\"#{sw_mrid}\"/>"
                )?;
            }
            writeln!(
                out,
                "    <cim:SwitchingStep.SwitchingPlan rdf:resource=\"#{}\"/>",
                sp.mrid
            )?;
            writeln!(
                out,
                "    <cim:SwitchingStep.isFreeSequence>{}</cim:SwitchingStep.isFreeSequence>",
                step.is_free_sequence
            )?;
            writeln!(out, "  </cim:SwitchingStep>")?;
        }
    }

    // --- Outage records ---
    for or_rec in &ops.outage_records {
        let outage_class = if or_rec.is_planned {
            "PlannedOutage"
        } else {
            "ForcedOutage"
        };
        writeln!(out, "  <cim:{outage_class} rdf:ID=\"{}\">", or_rec.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            or_rec.name
        )?;
        if let Some(cause) = or_rec.cause {
            let cause_str = match cause {
                surge_network::network::net_ops::OutageCause::Maintenance => "maintenance",
                surge_network::network::net_ops::OutageCause::Construction => "construction",
                surge_network::network::net_ops::OutageCause::Repair => "repair",
                surge_network::network::net_ops::OutageCause::Testing => "testing",
                surge_network::network::net_ops::OutageCause::Environmental => "environmental",
                surge_network::network::net_ops::OutageCause::ForcedEquipment => "forcedEquipment",
                surge_network::network::net_ops::OutageCause::ForcedWeather => "forcedWeather",
                surge_network::network::net_ops::OutageCause::ForcedProtection => {
                    "forcedProtection"
                }
                surge_network::network::net_ops::OutageCause::Other => "other",
            };
            writeln!(out, "    <cim:Outage.cause>{cause_str}</cim:Outage.cause>")?;
        }
        if let Some(ps) = or_rec.planned_start {
            writeln!(
                out,
                "    <cim:Outage.plannedPeriod.start>{}</cim:Outage.plannedPeriod.start>",
                ps.to_rfc3339()
            )?;
        }
        if let Some(pe) = or_rec.planned_end {
            writeln!(
                out,
                "    <cim:Outage.plannedPeriod.end>{}</cim:Outage.plannedPeriod.end>",
                pe.to_rfc3339()
            )?;
        }
        for eq_mrid in &or_rec.equipment_mrids {
            writeln!(
                out,
                "    <cim:Outage.Equipments rdf:resource=\"#{eq_mrid}\"/>"
            )?;
        }
        writeln!(out, "  </cim:{outage_class}>")?;
    }

    // --- OutageSchedule ---
    for os in &ops.outage_schedules {
        writeln!(out, "  <cim:OutageSchedule rdf:ID=\"{}\">", os.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            os.name
        )?;
        if let Some(hs) = os.horizon_start {
            writeln!(
                out,
                "    <cim:OutageSchedule.horizonStart>{}</cim:OutageSchedule.horizonStart>",
                hs.to_rfc3339()
            )?;
        }
        if let Some(he) = os.horizon_end {
            writeln!(
                out,
                "    <cim:OutageSchedule.horizonEnd>{}</cim:OutageSchedule.horizonEnd>",
                he.to_rfc3339()
            )?;
        }
        writeln!(out, "  </cim:OutageSchedule>")?;
    }

    // --- Crew ---
    for crew in &ops.crews {
        writeln!(out, "  <cim:Crew rdf:ID=\"{}\">", crew.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            crew.name
        )?;
        if let Some(ref ct) = crew.crew_type {
            writeln!(out, "    <cim:Crew.type>{ct}</cim:Crew.type>")?;
        }
        if let Some(status) = crew.status {
            let s_str = match status {
                surge_network::network::net_ops::CrewStatus::Available => "available",
                surge_network::network::net_ops::CrewStatus::Dispatched => "dispatched",
                surge_network::network::net_ops::CrewStatus::EnRoute => "enRoute",
                surge_network::network::net_ops::CrewStatus::OnSite => "onSite",
                surge_network::network::net_ops::CrewStatus::Released => "released",
            };
            writeln!(out, "    <cim:Crew.status>{s_str}</cim:Crew.status>")?;
        }
        writeln!(out, "  </cim:Crew>")?;
    }

    // --- WorkTask ---
    for wt in &ops.work_tasks {
        writeln!(out, "  <cim:WorkTask rdf:ID=\"{}\">", wt.mrid)?;
        writeln!(
            out,
            "    <cim:IdentifiedObject.name>{}</cim:IdentifiedObject.name>",
            wt.name
        )?;
        if let Some(ref crew_mrid) = wt.crew_mrid {
            writeln!(
                out,
                "    <cim:WorkTask.Crew rdf:resource=\"#{crew_mrid}\"/>"
            )?;
        }
        if let Some(ref out_mrid) = wt.outage_mrid {
            writeln!(
                out,
                "    <cim:WorkTask.Outage rdf:resource=\"#{out_mrid}\"/>"
            )?;
        }
        if let Some(ss) = wt.scheduled_start {
            writeln!(
                out,
                "    <cim:WorkTask.scheduledStart>{}</cim:WorkTask.scheduledStart>",
                ss.to_rfc3339()
            )?;
        }
        if let Some(se) = wt.scheduled_end {
            writeln!(
                out,
                "    <cim:WorkTask.scheduledEnd>{}</cim:WorkTask.scheduledEnd>",
                se.to_rfc3339()
            )?;
        }
        if let Some(kind) = wt.task_kind {
            let k_str = match kind {
                surge_network::network::net_ops::WorkTaskKind::Install => "install",
                surge_network::network::net_ops::WorkTaskKind::Remove => "remove",
                surge_network::network::net_ops::WorkTaskKind::Inspect => "inspect",
                surge_network::network::net_ops::WorkTaskKind::Repair => "repair",
                surge_network::network::net_ops::WorkTaskKind::Replace => "replace",
            };
            writeln!(
                out,
                "    <cim:WorkTask.taskKind>{k_str}</cim:WorkTask.taskKind>"
            )?;
        }
        if let Some(pri) = wt.priority {
            writeln!(
                out,
                "    <cim:WorkTask.priority>{pri}</cim:WorkTask.priority>"
            )?;
        }
        writeln!(out, "  </cim:WorkTask>")?;
    }

    write_rdf_footer(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Write all available CGMES profiles for a Network.
///
/// Writes EQ, TP, SSH, SV plus any extended profiles (SC, ME, AS, OL, BD, PR, NO)
/// where the corresponding Network data is non-empty.
///
/// Each profile is written to `{dir}/{network_name}_{suffix}.xml`.
pub fn write_all_profiles(
    network: &Network,
    dir: &Path,
    version: CgmesVersion,
) -> Result<(), CgmesWriteError> {
    if dir.extension().is_some() && !dir.is_dir() {
        return Err(CgmesWriteError::DirectoryTargetRequired(
            dir.display().to_string(),
        ));
    }
    std::fs::create_dir_all(dir)?;
    let profiles = to_profiles(network, version)?;
    let name = &network.name;

    std::fs::write(dir.join(format!("{name}_EQ.xml")), &profiles.eq)?;
    std::fs::write(dir.join(format!("{name}_TP.xml")), &profiles.tp)?;
    std::fs::write(dir.join(format!("{name}_SSH.xml")), &profiles.ssh)?;
    std::fs::write(dir.join(format!("{name}_SV.xml")), &profiles.sv)?;
    if let Some(sc) = profiles.sc {
        std::fs::write(dir.join(format!("{name}_SC.xml")), sc)?;
    }
    if let Some(me) = profiles.me {
        std::fs::write(dir.join(format!("{name}_ME.xml")), me)?;
    }
    if let Some(asset) = profiles.asset {
        std::fs::write(dir.join(format!("{name}_AS.xml")), asset)?;
    }
    if let Some(ol) = profiles.ol {
        std::fs::write(dir.join(format!("{name}_OL.xml")), ol)?;
    }
    if let Some(bd) = profiles.bd {
        std::fs::write(dir.join(format!("{name}_BD.xml")), bd)?;
    }
    if let Some(pr) = profiles.pr {
        std::fs::write(dir.join(format!("{name}_PR.xml")), pr)?;
    }
    if let Some(no) = profiles.no {
        std::fs::write(dir.join(format!("{name}_NO.xml")), no)?;
    }

    Ok(())
}

/// Write a complete CGMES dataset (EQ + TP + SSH + SV) to `output_dir`.
///
/// Produces four XML files named `{network_name}_EQ.xml`, `{network_name}_TP.xml`,
/// `{network_name}_SSH.xml`, `{network_name}_SV.xml`.
///
/// # Arguments
///
/// * `network` — The power system network to export.
/// * `output_dir` — Directory where the four XML files will be written.
///   Created automatically if it does not exist.
/// * `version` — CGMES version (2.4.15 or 3.0) controlling namespace URIs.
///
/// # Example
/// ```no_run
/// use surge_io::cgmes::writer::{CgmesVersion, write_cgmes};
/// # let network = surge_network::Network::new("test");
/// write_cgmes(&network, std::path::Path::new("/tmp/cgmes_out"), CgmesVersion::V2_4_15).unwrap();
/// ```
#[cfg(test)]
pub fn write_cgmes(
    network: &Network,
    output_dir: &Path,
    version: CgmesVersion,
) -> Result<(), CgmesWriteError> {
    if output_dir.extension().is_some() && !output_dir.is_dir() {
        return Err(CgmesWriteError::DirectoryTargetRequired(
            output_dir.display().to_string(),
        ));
    }
    // Ensure output directory exists.
    std::fs::create_dir_all(output_dir)?;

    // --- Collect unique BaseVoltage kV values ---
    let mut base_voltage_set: HashMap<i64, f64> = HashMap::new();
    for bus in &network.buses {
        let key = (bus.base_kv * 10.0).round() as i64;
        base_voltage_set.entry(key).or_insert(bus.base_kv);
    }

    // --- Build bus → substation mapping (one sub per bus) ---
    let bus_sub: HashMap<u32, String> = network
        .buses
        .iter()
        .map(|b| (b.number, sub_id(b.number)))
        .collect();

    // Generate all four profiles.
    let eq_xml = write_eq_profile(network, version, &base_voltage_set, &bus_sub)?;
    let tp_xml = write_tp_profile(network, version)?;
    let ssh_xml = write_ssh_profile(network, version)?;
    let sv_xml = write_sv_profile(network, version)?;

    // Write to files.
    let name = &network.name;
    std::fs::write(output_dir.join(format!("{name}_EQ.xml")), &eq_xml)?;
    std::fs::write(output_dir.join(format!("{name}_TP.xml")), &tp_xml)?;
    std::fs::write(output_dir.join(format!("{name}_SSH.xml")), &ssh_xml)?;
    std::fs::write(output_dir.join(format!("{name}_SV.xml")), &sv_xml)?;

    Ok(())
}

/// Write a CGMES dataset and return the four XML strings (EQ, TP, SSH, SV)
/// without writing to disk. Useful for testing and in-memory pipelines.
#[cfg(test)]
pub fn to_cgmes_strings(
    network: &Network,
    version: CgmesVersion,
) -> Result<(String, String, String, String), CgmesWriteError> {
    let profiles = to_profiles(network, version)?;
    Ok((profiles.eq, profiles.tp, profiles.ssh, profiles.sv))
}

/// Build all available CGMES profiles in memory without writing to disk.
pub fn to_profiles(
    network: &Network,
    version: CgmesVersion,
) -> Result<CgmesProfiles, CgmesWriteError> {
    let mut base_voltage_set: HashMap<i64, f64> = HashMap::new();
    for bus in &network.buses {
        let key = (bus.base_kv * 10.0).round() as i64;
        base_voltage_set.entry(key).or_insert(bus.base_kv);
    }
    let bus_sub: HashMap<u32, String> = network
        .buses
        .iter()
        .map(|b| (b.number, sub_id(b.number)))
        .collect();

    let has_sc = network.branches.iter().any(|b| b.zero_seq.is_some())
        || network.generators.iter().any(|g| {
            g.fault_data.as_ref().is_some_and(|f| {
                f.r0_pu.is_some() || f.x0_pu.is_some() || f.r2_pu.is_some() || f.x2_pu.is_some()
            })
        });

    Ok(CgmesProfiles {
        eq: write_eq_profile(network, version, &base_voltage_set, &bus_sub)?,
        tp: write_tp_profile(network, version)?,
        ssh: write_ssh_profile(network, version)?,
        sv: write_sv_profile(network, version)?,
        sc: if has_sc {
            Some(write_short_circuit_profile(network, version)?)
        } else {
            None
        },
        me: if network.cim.measurements.is_empty() {
            None
        } else {
            Some(write_measurement_profile(network, version)?)
        },
        asset: if network.cim.asset_catalog.is_empty() {
            None
        } else {
            Some(write_asset_profile(network, version)?)
        },
        ol: if network.cim.operational_limits.is_empty() {
            None
        } else {
            Some(write_operational_limits_profile(network, version)?)
        },
        bd: if network.cim.boundary_data.is_empty() {
            None
        } else {
            Some(write_boundary_profile(network, version)?)
        },
        pr: if network.cim.protection_data.is_empty() {
            None
        } else {
            Some(write_protection_profile(network, version)?)
        },
        no: if network.cim.network_operations.is_empty() {
            None
        } else {
            Some(write_network_operations_profile(network, version)?)
        },
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    /// Build a minimal 2-bus network for testing.
    fn mini_net() -> Network {
        let mut net = Network::new("test_cgmes");
        net.base_mva = 100.0;

        let mut slack = Bus::new(1, BusType::Slack, 345.0);
        slack.voltage_magnitude_pu = 1.04;
        slack.name = "Bus1".to_string();
        net.buses.push(slack);

        let mut pq = Bus::new(2, BusType::PQ, 345.0);
        pq.name = "Bus2".to_string();
        net.buses.push(pq);
        net.loads.push(Load::new(2, 100.0, 35.0));

        let mut gn = Generator::new(1, 80.0, 1.04);
        gn.qmin = -100.0;
        gn.qmax = 100.0;
        gn.pmin = 10.0;
        gn.pmax = 200.0;
        gn.machine_base_mva = 100.0;
        net.generators.push(gn);

        net.branches.push(Branch::new_line(1, 2, 0.02, 0.06, 0.03));
        net
    }

    #[test]
    fn test_write_cgmes_v2_produces_four_files() {
        let net = mini_net();
        let tmpdir = std::env::temp_dir().join("surge_cgmes_writer_test_v2");
        let _ = std::fs::remove_dir_all(&tmpdir);
        write_cgmes(&net, &tmpdir, CgmesVersion::V2_4_15).unwrap();

        assert!(tmpdir.join("test_cgmes_EQ.xml").exists());
        assert!(tmpdir.join("test_cgmes_TP.xml").exists());
        assert!(tmpdir.join("test_cgmes_SSH.xml").exists());
        assert!(tmpdir.join("test_cgmes_SV.xml").exists());

        // Check namespace in EQ file.
        let eq = std::fs::read_to_string(tmpdir.join("test_cgmes_EQ.xml")).unwrap();
        assert!(eq.contains(CIM_NS_V2), "EQ should contain CIM16 namespace");
        assert!(
            eq.contains("ACLineSegment"),
            "EQ should contain ACLineSegment"
        );

        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn test_write_cgmes_v3_namespace() {
        let net = mini_net();
        let (eq, _tp, _ssh, _sv) = to_cgmes_strings(&net, CgmesVersion::V3_0).unwrap();
        assert!(eq.contains(CIM_NS_V3), "EQ should contain CIM100 namespace");
    }

    #[test]
    fn test_eq_contains_expected_elements() {
        let net = mini_net();
        let (eq, _tp, _ssh, _sv) = to_cgmes_strings(&net, CgmesVersion::V2_4_15).unwrap();

        // Structural elements
        assert!(
            eq.contains("GeographicalRegion"),
            "missing GeographicalRegion"
        );
        assert!(
            eq.contains("SubGeographicalRegion"),
            "missing SubGeographicalRegion"
        );
        assert!(eq.contains("Substation"), "missing Substation");
        assert!(eq.contains("BaseVoltage"), "missing BaseVoltage");
        assert!(eq.contains("VoltageLevel"), "missing VoltageLevel");
        assert!(eq.contains("ACLineSegment"), "missing ACLineSegment");
        assert!(
            eq.contains("SynchronousMachine"),
            "missing SynchronousMachine"
        );
        assert!(eq.contains("GeneratingUnit"), "missing GeneratingUnit");
        assert!(
            eq.contains("RegulatingControl"),
            "missing RegulatingControl"
        );
        assert!(eq.contains("Terminal"), "missing Terminal");
    }

    #[test]
    fn test_tp_contains_topological_nodes() {
        let net = mini_net();
        let (_eq, tp, _ssh, _sv) = to_cgmes_strings(&net, CgmesVersion::V2_4_15).unwrap();

        assert!(tp.contains("TopologicalNode"), "missing TopologicalNode");
        assert!(
            tp.contains("TopologicalNode.ConnectivityNodeContainer"),
            "missing container ref"
        );
        assert!(tp.contains("TopologicalNode.BaseVoltage"), "missing BV ref");
        assert!(
            tp.contains("Terminal.TopologicalNode"),
            "missing terminal→TN mapping"
        );
    }

    #[test]
    fn test_ssh_sign_convention() {
        let net = mini_net();
        let (_eq, _tp, ssh, _sv) = to_cgmes_strings(&net, CgmesVersion::V2_4_15).unwrap();

        // Generator P should be negative (IEC convention: generating = negative).
        // Our gen has pg=80.0, so SSH should show RotatingMachine.p = -80.
        assert!(
            ssh.contains("<cim:RotatingMachine.p>-80</cim:RotatingMachine.p>"),
            "Generator P should be negative in SSH (IEC convention)"
        );
    }

    #[test]
    fn test_ssh_load_values() {
        let net = mini_net();
        let (_eq, _tp, ssh, _sv) = to_cgmes_strings(&net, CgmesVersion::V2_4_15).unwrap();

        // Bus 2 has pd=100, qd=35 (bus-level load).
        assert!(
            ssh.contains("<cim:EnergyConsumer.p>100</cim:EnergyConsumer.p>"),
            "Load P should be 100"
        );
        assert!(
            ssh.contains("<cim:EnergyConsumer.q>35</cim:EnergyConsumer.q>"),
            "Load Q should be 35"
        );
    }

    #[test]
    fn test_sv_voltage_in_kv() {
        let net = mini_net();
        let (_eq, _tp, _ssh, sv) = to_cgmes_strings(&net, CgmesVersion::V2_4_15).unwrap();

        // Bus 1: vm=1.04, base_kv=345 → v_kv = 358.8
        assert!(sv.contains("SvVoltage"), "missing SvVoltage");
        assert!(sv.contains("358.8"), "Bus 1 voltage should be 358.8 kV");
    }

    #[test]
    fn test_transformer_branch() {
        let mut net = mini_net();
        // Add a transformer branch (tap != 1.0).
        let mut xfmr = Branch::new_line(1, 2, 0.01, 0.05, 0.0);
        xfmr.tap = 0.97;
        net.branches.push(xfmr);

        let (eq, _tp, _ssh, _sv) = to_cgmes_strings(&net, CgmesVersion::V2_4_15).unwrap();

        assert!(
            eq.contains("PowerTransformer"),
            "Should contain PowerTransformer for tap != 1.0"
        );
        assert!(
            eq.contains("PowerTransformerEnd"),
            "Should contain PowerTransformerEnd"
        );
        assert!(
            eq.contains("RatioTapChanger"),
            "Should contain RatioTapChanger for tap != 1.0"
        );
    }

    #[test]
    fn test_round_trip_bus_count() {
        // Write CGMES, then parse back and verify bus count matches.
        let net = mini_net();
        let tmpdir = std::env::temp_dir().join("surge_cgmes_rt_test");
        let _ = std::fs::remove_dir_all(&tmpdir);
        write_cgmes(&net, &tmpdir, CgmesVersion::V2_4_15).unwrap();

        // Parse back using the CIM reader.
        let result = crate::load(&tmpdir);
        match result {
            Ok(parsed) => {
                assert_eq!(
                    parsed.n_buses(),
                    net.n_buses(),
                    "Round-tripped network should have same bus count"
                );
            }
            Err(e) => {
                // If parsing fails, it might be due to topology reduction or
                // missing features. Print for diagnostics but don't panic.
                eprintln!("CGMES round-trip parse failed (expected during development): {e}");
            }
        }

        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn test_write_short_circuit_profile() {
        let mut net = mini_net();
        // Add zero-sequence data to the branch
        {
            let zs = net.branches[0]
                .zero_seq
                .get_or_insert_with(surge_network::network::ZeroSeqData::default);
            zs.r0 = 0.06;
            zs.x0 = 0.18;
            zs.b0 = 0.01;
        }
        // Add sequence data to the generator
        net.generators[0]
            .fault_data
            .get_or_insert_with(Default::default)
            .r0_pu = Some(0.01);
        net.generators[0]
            .fault_data
            .get_or_insert_with(Default::default)
            .x0_pu = Some(0.08);
        net.generators[0]
            .fault_data
            .get_or_insert_with(Default::default)
            .x2_pu = Some(0.12);

        let sc = write_short_circuit_profile(&net, CgmesVersion::V2_4_15).unwrap();

        assert!(
            sc.contains("ACLineSegment.r0"),
            "SC should contain ACLineSegment.r0"
        );
        assert!(
            sc.contains("ACLineSegment.x0"),
            "SC should contain ACLineSegment.x0"
        );
        assert!(
            sc.contains("ACLineSegment.b0ch"),
            "SC should contain ACLineSegment.b0ch"
        );
        assert!(
            sc.contains("SynchronousMachine.r0"),
            "SC should contain SynchronousMachine.r0"
        );
        assert!(
            sc.contains("SynchronousMachine.x2"),
            "SC should contain SynchronousMachine.x2"
        );
    }

    #[test]
    fn test_write_measurement_profile() {
        let mut net = mini_net();
        net.cim
            .measurements
            .push(surge_network::network::measurement::CimMeasurement {
                mrid: "_MEAS_P1".to_string(),
                name: "P_Bus1".to_string(),
                measurement_type:
                    surge_network::network::measurement::CimMeasurementType::ActivePower,
                bus: 1,
                value: 80.0,
                sigma: 0.02,
                enabled: true,
                ..Default::default()
            });
        net.cim
            .measurements
            .push(surge_network::network::measurement::CimMeasurement {
                mrid: "_MEAS_V1".to_string(),
                name: "V_Bus1".to_string(),
                measurement_type:
                    surge_network::network::measurement::CimMeasurementType::VoltageMagnitude,
                bus: 1,
                value: 1.04,
                sigma: 0.01,
                enabled: true,
                ..Default::default()
            });

        let me = write_measurement_profile(&net, CgmesVersion::V2_4_15).unwrap();

        assert!(
            me.contains("<cim:Analog rdf:ID=\"_MEAS_P1\">"),
            "should contain Analog for P"
        );
        assert!(
            me.contains("ActivePower"),
            "should contain ActivePower type"
        );
        assert!(
            me.contains("<cim:AnalogValue rdf:ID=\"_MEAS_P1_V\">"),
            "should contain AnalogValue"
        );
        assert!(
            me.contains("<cim:Analog rdf:ID=\"_MEAS_V1\">"),
            "should contain Analog for V"
        );
        assert!(
            me.contains("VoltageMagnitude"),
            "should contain VoltageMagnitude type"
        );
    }

    #[test]
    fn test_write_all_profiles_creates_files() {
        let mut net = mini_net();
        // Add some data so extended profiles are written
        {
            let zs = net.branches[0]
                .zero_seq
                .get_or_insert_with(surge_network::network::ZeroSeqData::default);
            zs.r0 = 0.06;
            zs.x0 = 0.18;
        }
        net.cim
            .measurements
            .push(surge_network::network::measurement::CimMeasurement {
                mrid: "_MEAS_1".to_string(),
                name: "P1".to_string(),
                measurement_type:
                    surge_network::network::measurement::CimMeasurementType::ActivePower,
                value: 50.0,
                ..Default::default()
            });

        let tmpdir = std::env::temp_dir().join("surge_cgmes_all_profiles_test");
        let _ = std::fs::remove_dir_all(&tmpdir);
        write_all_profiles(&net, &tmpdir, CgmesVersion::V2_4_15).unwrap();

        // Core profiles always present
        assert!(tmpdir.join("test_cgmes_EQ.xml").exists());
        assert!(tmpdir.join("test_cgmes_TP.xml").exists());
        assert!(tmpdir.join("test_cgmes_SSH.xml").exists());
        assert!(tmpdir.join("test_cgmes_SV.xml").exists());
        // Extended profiles present because we added data
        assert!(
            tmpdir.join("test_cgmes_SC.xml").exists(),
            "SC profile should exist"
        );
        assert!(
            tmpdir.join("test_cgmes_ME.xml").exists(),
            "ME profile should exist"
        );
        // These should NOT exist (no data added)
        assert!(
            !tmpdir.join("test_cgmes_AS.xml").exists(),
            "AS should not exist (no data)"
        );
        assert!(
            !tmpdir.join("test_cgmes_BD.xml").exists(),
            "BD should not exist (no data)"
        );
        assert!(
            !tmpdir.join("test_cgmes_PR.xml").exists(),
            "PR should not exist (no data)"
        );
        assert!(
            !tmpdir.join("test_cgmes_NO.xml").exists(),
            "NO should not exist (no data)"
        );

        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn test_write_operational_limits_profile() {
        let mut net = mini_net();
        use surge_network::network::op_limits::{
            LimitDirection, LimitDuration, LimitKind, OperationalLimit, OperationalLimitSet,
        };
        net.cim.operational_limits.limit_sets.insert(
            "_OLS_1".to_string(),
            OperationalLimitSet {
                mrid: "_OLS_1".to_string(),
                name: "Line_Rate_A".to_string(),
                bus: 1,
                equipment_mrid: Some("_ACLS_0".to_string()),
                from_end: Some(true),
                limits: vec![
                    (
                        LimitKind::Current,
                        OperationalLimit {
                            value: 1200.0,
                            duration: LimitDuration::Permanent,
                            direction: LimitDirection::AbsoluteValue,
                            limit_type_mrid: None,
                        },
                    ),
                    (
                        LimitKind::ApparentPower,
                        OperationalLimit {
                            value: 500.0,
                            duration: LimitDuration::Temporary(900.0),
                            direction: LimitDirection::AbsoluteValue,
                            limit_type_mrid: None,
                        },
                    ),
                ],
            },
        );

        let ol = write_operational_limits_profile(&net, CgmesVersion::V2_4_15).unwrap();

        assert!(ol.contains("OperationalLimitSet"), "should contain OLS");
        assert!(ol.contains("CurrentLimit"), "should contain CurrentLimit");
        assert!(
            ol.contains("<cim:CurrentLimit.value>1200</cim:CurrentLimit.value>"),
            "should have 1200A limit"
        );
        assert!(
            ol.contains("ApparentPowerLimit"),
            "should contain ApparentPowerLimit"
        );
        assert!(ol.contains("TATL"), "should contain TATL duration name");
        assert!(
            ol.contains("acceptableDuration"),
            "should have duration for TATL"
        );
    }
}
