// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! JSON format for native Surge network serialization.
//!
//! The native JSON format is a versioned document envelope around the
//! `surge_network::Network` model:
//!
//! ```text
//! {
//!   "format": "surge-json",
//!   "schema_version": "0.1.0",
//!   "meta": { "producer": "surge", "profile": "network" },
//!   "network": { ... }
//! }
//! ```
//!
//! This format is lossless for finite values and preserves `NaN` / infinities
//! through explicit tagged JSON values rather than silently rewriting them.

use std::io::BufReader;
use std::path::Path;

use serde::Serialize;
use serde_value::Value as SerdeValue;
use surge_network::Network;
use surge_solution::{AuditableSolution, SolutionAuditReport};
use thiserror::Error;

pub const SURGE_JSON_FORMAT: &str = "surge-json";
pub const SURGE_JSON_SCHEMA_VERSION: &str = "0.1.0";

const SPECIAL_FLOAT_TAG: &str = "$surge_float";
const SPECIAL_BYTES_TAG: &str = "$surge_bytes";
const SPECIAL_MAP_TAG: &str = "$surge_map";
const FORMAT_FIELD: &str = "format";
const SCHEMA_VERSION_FIELD: &str = "schema_version";
const META_FIELD: &str = "meta";
const NETWORK_FIELD: &str = "network";
const META_PRODUCER_FIELD: &str = "producer";
const META_PROFILE_FIELD: &str = "profile";
const DISPATCH_FIELD: &str = "dispatch";
const SOLUTION_FIELD: &str = "solution";
const AUDIT_FIELD: &str = "audit";
const META_PRODUCER: &str = "surge";
const META_PROFILE_NETWORK: &str = "network";
const META_PROFILE_DISPATCH: &str = "dispatch";
const META_PROFILE_RESULTS: &str = "results";

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("serde-value serialization error: {0}")]
    ValueSerialize(#[from] serde_value::SerializerError),

    #[error("serde-value deserialization error: {0}")]
    ValueDeserialize(#[from] serde_value::DeserializerError),

    #[error("invalid tagged JSON value: {0}")]
    InvalidTaggedValue(String),

    #[error("invalid JSON document: {0}")]
    InvalidDocument(String),

    #[error("solution audit failed: {0}")]
    SolutionAuditFailed(String),
}

/// Load a JSON network file from disk.
pub fn load(path: impl AsRef<Path>) -> Result<Network, Error> {
    parse_file(path.as_ref())
}

/// Load a JSON network from an in-memory string.
pub fn loads(content: &str) -> Result<Network, Error> {
    parse_str(content)
}

/// Save a network to a JSON file.
pub fn save(network: &Network, path: impl AsRef<Path>) -> Result<(), Error> {
    write_file(network, path.as_ref(), false)
}

/// Save a network to a JSON file with pretty formatting.
pub fn save_pretty(network: &Network, path: impl AsRef<Path>) -> Result<(), Error> {
    write_file(network, path.as_ref(), true)
}

/// Serialize a network to a JSON string.
pub fn dumps(network: &Network) -> Result<String, Error> {
    to_string(network, false)
}

/// Serialize a network to a pretty JSON string.
pub fn dumps_pretty(network: &Network) -> Result<String, Error> {
    to_string(network, true)
}

// ─── Multi-profile document API ──────────────────────────────────────────────

/// A surge-json document that may carry a network, dispatch request, and/or
/// dispatch solution.
///
/// The `dispatch` and `solution` fields are opaque `serde_json::Value` because
/// surge-io does not depend on surge-dispatch. Callers deserialize them into
/// the appropriate typed structs (`DispatchRequest`, `DispatchSolution`).
#[derive(Debug, Clone)]
pub struct SurgeDocument {
    /// The network (always present in a valid surge-json document).
    pub network: Network,
    /// Dispatch request data (present when profile is `"dispatch"` or `"results"`).
    pub dispatch: Option<serde_json::Value>,
    /// Dispatch solution data (present when profile is `"results"`).
    pub solution: Option<serde_json::Value>,
}

/// The profile of a surge-json document, inferred from what's present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurgeJsonProfile {
    /// Network only.
    Network,
    /// Network + dispatch request.
    Dispatch,
    /// Network + dispatch request + solution.
    Results,
}

impl SurgeDocument {
    /// Infer the profile from what fields are populated.
    pub fn profile(&self) -> SurgeJsonProfile {
        if self.solution.is_some() {
            SurgeJsonProfile::Results
        } else if self.dispatch.is_some() {
            SurgeJsonProfile::Dispatch
        } else {
            SurgeJsonProfile::Network
        }
    }
}

/// Load a surge-json document that may contain dispatch and/or solution data.
pub fn load_document(path: impl AsRef<Path>) -> Result<SurgeDocument, Error> {
    let path = path.as_ref();
    let file = std::fs::File::open(path)?;
    let json: serde_json::Value = if path_uses_zstd(path) {
        let reader = zstd::stream::read::Decoder::new(file)?;
        serde_json::from_reader(BufReader::new(reader))?
    } else {
        serde_json::from_reader(BufReader::new(file))?
    };
    decode_document_full(json)
}

/// Parse a surge-json document from an in-memory string.
pub fn loads_document(content: &str) -> Result<SurgeDocument, Error> {
    let json: serde_json::Value = serde_json::from_str(content)?;
    decode_document_full(json)
}

/// Save a [`SurgeDocument`] to a JSON file with auto-detected zstd compression.
pub fn save_document(doc: &SurgeDocument, path: impl AsRef<Path>) -> Result<(), Error> {
    let path = path.as_ref();
    let json = encode_document_full(doc)?;
    let file = std::fs::File::create(path)?;
    if path_uses_zstd(path) {
        let mut encoder = zstd::stream::write::Encoder::new(file, 9)?;
        serde_json::to_writer(&mut encoder, &json)?;
        encoder.finish()?;
    } else {
        serde_json::to_writer_pretty(file, &json)?;
    }
    Ok(())
}

/// Serialize a [`SurgeDocument`] to a JSON string.
pub fn dumps_document(doc: &SurgeDocument) -> Result<String, Error> {
    let json = encode_document_full(doc)?;
    Ok(serde_json::to_string_pretty(&json)?)
}

/// Serialize a solution payload and overwrite its `audit` block with the
/// freshly computed exact objective-ledger audit report.
pub fn encode_audited_solution<T>(solution: &T) -> Result<serde_json::Value, Error>
where
    T: Serialize + AuditableSolution,
{
    let audit = solution.computed_solution_audit();
    let mut json = serde_json::to_value(solution)?;
    inject_solution_audit(&mut json, &audit)?;
    Ok(json)
}

/// Serialize a solution payload with a fresh `audit` block and fail fast if
/// the exact objective-ledger audit does not pass.
pub fn encode_checked_audited_solution<T>(solution: &T) -> Result<serde_json::Value, Error>
where
    T: Serialize + AuditableSolution,
{
    let audit = solution.computed_solution_audit();
    let mut json = serde_json::to_value(solution)?;
    inject_solution_audit(&mut json, &audit)?;
    if !audit.audit_passed {
        return Err(Error::SolutionAuditFailed(format_solution_audit_failure(
            &audit,
        )));
    }
    Ok(json)
}

fn parse_file(path: &Path) -> Result<Network, Error> {
    let file = std::fs::File::open(path)?;
    let json: serde_json::Value = if path_uses_zstd(path) {
        let reader = zstd::stream::read::Decoder::new(file)?;
        serde_json::from_reader(BufReader::new(reader))?
    } else {
        serde_json::from_reader(BufReader::new(file))?
    };
    decode_document(json)
}

fn parse_str(content: &str) -> Result<Network, Error> {
    let json: serde_json::Value = serde_json::from_str(content)?;
    decode_document(json)
}

fn write_file(network: &Network, path: &Path, pretty: bool) -> Result<(), Error> {
    let file = std::fs::File::create(path)?;
    let json = encode_document(network)?;
    if path_uses_zstd(path) {
        let mut encoder = zstd::stream::write::Encoder::new(file, 9)?;
        if pretty {
            serde_json::to_writer_pretty(&mut encoder, &json)?;
        } else {
            serde_json::to_writer(&mut encoder, &json)?;
        }
        encoder.finish()?;
    } else if pretty {
        serde_json::to_writer_pretty(file, &json)?;
    } else {
        serde_json::to_writer(file, &json)?;
    }
    Ok(())
}

fn to_string(network: &Network, pretty: bool) -> Result<String, Error> {
    let json = encode_document(network)?;
    let json = if pretty {
        serde_json::to_string_pretty(&json)?
    } else {
        serde_json::to_string(&json)?
    };
    Ok(json)
}

fn path_uses_zstd(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.to_ascii_lowercase().ends_with(".zst"))
}

fn inject_solution_audit(
    solution_json: &mut serde_json::Value,
    audit: &SolutionAuditReport,
) -> Result<(), Error> {
    let object = solution_json.as_object_mut().ok_or_else(|| {
        Error::InvalidDocument("solution payload must serialize as a JSON object".to_string())
    })?;
    object.insert(AUDIT_FIELD.to_string(), serde_json::to_value(audit)?);
    Ok(())
}

fn format_solution_audit_failure(audit: &SolutionAuditReport) -> String {
    let mut message = format!("{} mismatch(es) detected", audit.ledger_mismatches.len());
    if let Some(first) = audit.ledger_mismatches.first() {
        message.push_str(&format!(
            "; first mismatch: {:?} {} {} (expected {:.6}, actual {:.6})",
            first.scope_kind,
            first.scope_id,
            first.field,
            first.expected_dollars,
            first.actual_dollars,
        ));
    }
    if audit.has_residual_terms {
        message.push_str("; residual terms remain in the objective ledger");
    }
    message
}

fn encode_document(network: &Network) -> Result<serde_json::Value, Error> {
    let mut object = serde_json::Map::new();
    object.insert(
        FORMAT_FIELD.to_string(),
        serde_json::Value::String(SURGE_JSON_FORMAT.to_string()),
    );
    object.insert(
        SCHEMA_VERSION_FIELD.to_string(),
        serde_json::Value::String(SURGE_JSON_SCHEMA_VERSION.to_string()),
    );
    object.insert(META_FIELD.to_string(), encode_meta());
    object.insert(NETWORK_FIELD.to_string(), encode_network(network)?);
    Ok(serde_json::Value::Object(object))
}

fn decode_document(json: serde_json::Value) -> Result<Network, Error> {
    let object = json.as_object().ok_or_else(|| {
        Error::InvalidDocument("expected top-level JSON object document".to_string())
    })?;

    let format = object
        .get(FORMAT_FIELD)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::InvalidDocument(format!("missing or invalid '{FORMAT_FIELD}' field"))
        })?;
    if format != SURGE_JSON_FORMAT {
        return Err(Error::InvalidDocument(format!(
            "unsupported '{FORMAT_FIELD}' value '{format}'"
        )));
    }

    let schema_version = object
        .get(SCHEMA_VERSION_FIELD)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::InvalidDocument(format!("missing or invalid '{SCHEMA_VERSION_FIELD}' field"))
        })?;
    if schema_version != SURGE_JSON_SCHEMA_VERSION {
        return Err(Error::InvalidDocument(format!(
            "unsupported '{SCHEMA_VERSION_FIELD}' value '{schema_version}'"
        )));
    }

    if let Some(meta) = object.get(META_FIELD) {
        validate_meta(meta)?;
    }

    let network = object
        .get(NETWORK_FIELD)
        .cloned()
        .ok_or_else(|| Error::InvalidDocument(format!("missing '{NETWORK_FIELD}' field")))?;

    decode_network(network)
}

fn encode_document_full(doc: &SurgeDocument) -> Result<serde_json::Value, Error> {
    let profile = doc.profile();
    let profile_str = match profile {
        SurgeJsonProfile::Network => META_PROFILE_NETWORK,
        SurgeJsonProfile::Dispatch => META_PROFILE_DISPATCH,
        SurgeJsonProfile::Results => META_PROFILE_RESULTS,
    };

    let mut object = serde_json::Map::new();
    object.insert(
        FORMAT_FIELD.to_string(),
        serde_json::Value::String(SURGE_JSON_FORMAT.to_string()),
    );
    object.insert(
        SCHEMA_VERSION_FIELD.to_string(),
        serde_json::Value::String(SURGE_JSON_SCHEMA_VERSION.to_string()),
    );
    object.insert(
        META_FIELD.to_string(),
        encode_meta_with_profile(profile_str),
    );
    object.insert(NETWORK_FIELD.to_string(), encode_network(&doc.network)?);

    if let Some(ref dispatch) = doc.dispatch {
        object.insert(DISPATCH_FIELD.to_string(), dispatch.clone());
    }
    if let Some(ref solution) = doc.solution {
        object.insert(SOLUTION_FIELD.to_string(), solution.clone());
    }

    Ok(serde_json::Value::Object(object))
}

fn decode_document_full(json: serde_json::Value) -> Result<SurgeDocument, Error> {
    let object = json.as_object().ok_or_else(|| {
        Error::InvalidDocument("expected top-level JSON object document".to_string())
    })?;

    let format = object
        .get(FORMAT_FIELD)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::InvalidDocument(format!("missing or invalid '{FORMAT_FIELD}' field"))
        })?;
    if format != SURGE_JSON_FORMAT {
        return Err(Error::InvalidDocument(format!(
            "unsupported '{FORMAT_FIELD}' value '{format}'"
        )));
    }

    let schema_version = object
        .get(SCHEMA_VERSION_FIELD)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::InvalidDocument(format!("missing or invalid '{SCHEMA_VERSION_FIELD}' field"))
        })?;
    if schema_version != SURGE_JSON_SCHEMA_VERSION {
        return Err(Error::InvalidDocument(format!(
            "unsupported '{SCHEMA_VERSION_FIELD}' value '{schema_version}'"
        )));
    }

    // Validate meta if present, but accept any known profile.
    if let Some(meta) = object.get(META_FIELD) {
        validate_meta_any_profile(meta)?;
    }

    let network_json = object
        .get(NETWORK_FIELD)
        .cloned()
        .ok_or_else(|| Error::InvalidDocument(format!("missing '{NETWORK_FIELD}' field")))?;
    let network = decode_network(network_json)?;

    let dispatch = object.get(DISPATCH_FIELD).cloned();
    let solution = object.get(SOLUTION_FIELD).cloned();

    Ok(SurgeDocument {
        network,
        dispatch,
        solution,
    })
}

fn encode_meta_with_profile(profile: &str) -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    meta.insert(
        META_PRODUCER_FIELD.to_string(),
        serde_json::Value::String(META_PRODUCER.to_string()),
    );
    meta.insert(
        META_PROFILE_FIELD.to_string(),
        serde_json::Value::String(profile.to_string()),
    );
    serde_json::Value::Object(meta)
}

fn validate_meta_any_profile(meta: &serde_json::Value) -> Result<(), Error> {
    let object = meta
        .as_object()
        .ok_or_else(|| Error::InvalidDocument(format!("'{META_FIELD}' must be a JSON object")))?;

    if let Some(producer) = object.get(META_PRODUCER_FIELD) {
        let producer = producer.as_str().ok_or_else(|| {
            Error::InvalidDocument(format!(
                "'{META_FIELD}.{META_PRODUCER_FIELD}' must be a string"
            ))
        })?;
        if producer != META_PRODUCER {
            return Err(Error::InvalidDocument(format!(
                "unsupported '{META_FIELD}.{META_PRODUCER_FIELD}' value '{producer}'"
            )));
        }
    }

    if let Some(profile) = object.get(META_PROFILE_FIELD) {
        let profile = profile.as_str().ok_or_else(|| {
            Error::InvalidDocument(format!(
                "'{META_FIELD}.{META_PROFILE_FIELD}' must be a string"
            ))
        })?;
        if !matches!(
            profile,
            META_PROFILE_NETWORK | META_PROFILE_DISPATCH | META_PROFILE_RESULTS
        ) {
            return Err(Error::InvalidDocument(format!(
                "unsupported '{META_FIELD}.{META_PROFILE_FIELD}' value '{profile}'"
            )));
        }
    }

    Ok(())
}

pub(crate) fn encode_meta() -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    meta.insert(
        META_PRODUCER_FIELD.to_string(),
        serde_json::Value::String(META_PRODUCER.to_string()),
    );
    meta.insert(
        META_PROFILE_FIELD.to_string(),
        serde_json::Value::String(META_PROFILE_NETWORK.to_string()),
    );
    serde_json::Value::Object(meta)
}

pub(crate) fn validate_meta(meta: &serde_json::Value) -> Result<(), Error> {
    let object = meta
        .as_object()
        .ok_or_else(|| Error::InvalidDocument(format!("'{META_FIELD}' must be a JSON object")))?;

    if let Some(producer) = object.get(META_PRODUCER_FIELD) {
        let producer = producer.as_str().ok_or_else(|| {
            Error::InvalidDocument(format!(
                "'{META_FIELD}.{META_PRODUCER_FIELD}' must be a string"
            ))
        })?;
        if producer != META_PRODUCER {
            return Err(Error::InvalidDocument(format!(
                "unsupported '{META_FIELD}.{META_PRODUCER_FIELD}' value '{producer}'"
            )));
        }
    }

    if let Some(profile) = object.get(META_PROFILE_FIELD) {
        let profile = profile.as_str().ok_or_else(|| {
            Error::InvalidDocument(format!(
                "'{META_FIELD}.{META_PROFILE_FIELD}' must be a string"
            ))
        })?;
        if profile != META_PROFILE_NETWORK {
            return Err(Error::InvalidDocument(format!(
                "unsupported '{META_FIELD}.{META_PROFILE_FIELD}' value '{profile}'"
            )));
        }
    }

    Ok(())
}

pub(crate) fn encode_network(network: &Network) -> Result<serde_json::Value, Error> {
    let value = serde_value::to_value(network)?;
    value_to_json(value)
}

pub(crate) fn decode_network(json: serde_json::Value) -> Result<Network, Error> {
    let json = migrate_phase_shift_deg_to_rad(json);
    let json = migrate_bus_demand_to_loads(json)?;
    let json = migrate_legacy_market_layout(json)?;
    let value = json_to_value(json)?;
    let network: Network = value.deserialize_into()?;
    Ok(network)
}

/// Migrate legacy `phase_shift_deg`, `phase_min_deg`, `phase_max_deg`, and
/// `phase_step_deg` branch fields (stored in degrees) to their `_rad`
/// counterparts (stored in radians).
///
/// Old JSON bundles serialised these values in degrees.  The struct fields have
/// been renamed to `_rad` (with serde aliases for the old names), but the alias
/// alone cannot convert the unit.  This migration rewrites the JSON *before*
/// serde deserialisation so the values are in radians when they land on the new
/// fields.
fn migrate_phase_shift_deg_to_rad(mut json: serde_json::Value) -> serde_json::Value {
    let branches = json
        .as_object_mut()
        .and_then(|o| o.get_mut("branches"))
        .and_then(|v| v.as_array_mut());
    if let Some(branches) = branches {
        for br in branches.iter_mut() {
            if let Some(obj) = br.as_object_mut() {
                migrate_deg_field(obj, "phase_shift_deg", "phase_shift_rad");
                migrate_deg_field(obj, "phase_min_deg", "phase_min_rad");
                migrate_deg_field(obj, "phase_max_deg", "phase_max_rad");
                migrate_deg_field(obj, "phase_step_deg", "phase_step_rad");
            }
        }
    }
    json
}

/// Remove `old_key` (degrees), convert the value to radians, and insert as
/// `new_key`.  If `new_key` already exists or `old_key` is absent, do nothing.
fn migrate_deg_field(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    old_key: &str,
    new_key: &str,
) {
    if obj.contains_key(new_key) {
        return; // already migrated
    }
    if let Some(val) = obj.remove(old_key) {
        if let Some(deg) = val.as_f64() {
            let rad = deg.to_radians();
            obj.insert(new_key.to_string(), serde_json::Value::from(rad));
        } else {
            // Not a number — put it back so serde can report the error.
            obj.insert(old_key.to_string(), val);
        }
    }
}

/// Migrate legacy `active_power_demand_mw` / `reactive_power_demand_mvar` fields
/// from Bus objects into Load objects.
///
/// Old JSON files stored demand on buses; the new model stores it exclusively
/// on `Load` objects. Some legacy files carry *both* representations, often with
/// the same values duplicated. We therefore:
/// - drop duplicate legacy demand when explicit load(s) already match it
/// - seed a single zero-valued load when exactly one explicit load exists but is empty
/// - create a synthetic load when none exists
/// - reject inconsistent mixed-format cases instead of silently double-counting
fn migrate_bus_demand_to_loads(mut json: serde_json::Value) -> Result<serde_json::Value, Error> {
    let root = match json.as_object_mut() {
        Some(o) => o,
        None => return Ok(json),
    };

    let mut loads_by_bus = std::collections::HashMap::<u32, Vec<(usize, f64, f64)>>::new();
    if let Some(loads) = root.get("loads").and_then(|v| v.as_array()) {
        for (idx, load) in loads.iter().enumerate() {
            if let Some(bus) = load.get("bus").and_then(|v| v.as_u64()) {
                let pd = load
                    .get("active_power_demand_mw")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let qd = load
                    .get("reactive_power_demand_mvar")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                loads_by_bus
                    .entry(bus as u32)
                    .or_default()
                    .push((idx, pd, qd));
            }
        }
    }

    let mut load_updates: Vec<(usize, f64, f64)> = Vec::new();
    let mut synthetic_loads: Vec<serde_json::Value> = Vec::new();
    if let Some(buses) = root.get_mut("buses").and_then(|v| v.as_array_mut()) {
        for bus_val in buses.iter_mut() {
            if let Some(bus_obj) = bus_val.as_object_mut() {
                let pd = bus_obj
                    .get("active_power_demand_mw")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let qd = bus_obj
                    .get("reactive_power_demand_mvar")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let bus_number = bus_obj.get("number").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                // Remove legacy fields regardless.
                bus_obj.remove("active_power_demand_mw");
                bus_obj.remove("reactive_power_demand_mvar");

                if pd.abs() > 1e-12 || qd.abs() > 1e-12 {
                    let matches_legacy = |existing_pd: f64, existing_qd: f64| {
                        (existing_pd - pd).abs() <= 1e-9 && (existing_qd - qd).abs() <= 1e-9
                    };
                    match loads_by_bus.get(&bus_number).map(Vec::as_slice) {
                        Some([(idx, existing_pd, existing_qd)]) => {
                            if matches_legacy(*existing_pd, *existing_qd) {
                                continue;
                            }
                            if existing_pd.abs() <= 1e-12 && existing_qd.abs() <= 1e-12 {
                                load_updates.push((*idx, pd, qd));
                            } else {
                                return Err(Error::InvalidDocument(format!(
                                    "legacy bus demand on bus {bus_number} conflicts with existing explicit load data"
                                )));
                            }
                        }
                        Some(indices) if indices.len() > 1 => {
                            let total_pd: f64 = indices.iter().map(|(_, p, _)| *p).sum();
                            let total_qd: f64 = indices.iter().map(|(_, _, q)| *q).sum();
                            if matches_legacy(total_pd, total_qd) {
                                continue;
                            }
                            return Err(Error::InvalidDocument(format!(
                                "legacy bus demand on bus {bus_number} conflicts with {} explicit loads already on the bus",
                                indices.len()
                            )));
                        }
                        _ => {
                            let mut load = serde_json::Map::new();
                            load.insert("bus".to_string(), serde_json::json!(bus_number));
                            load.insert(
                                "id".to_string(),
                                serde_json::json!(format!("__migrated_{}", bus_number)),
                            );
                            load.insert(
                                "active_power_demand_mw".to_string(),
                                serde_json::json!(pd),
                            );
                            load.insert(
                                "reactive_power_demand_mvar".to_string(),
                                serde_json::json!(qd),
                            );
                            load.insert("in_service".to_string(), serde_json::json!(true));
                            synthetic_loads.push(serde_json::Value::Object(load));
                        }
                    }
                }
            }
        }
    }

    if !load_updates.is_empty() {
        let loads = root
            .get_mut("loads")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| Error::InvalidDocument("missing 'loads' field".to_string()))?;
        for (idx, pd, qd) in load_updates {
            let Some(load_obj) = loads.get_mut(idx).and_then(|v| v.as_object_mut()) else {
                return Err(Error::InvalidDocument(format!(
                    "legacy bus demand migration failed because load index {idx} is not an object"
                )));
            };
            let existing_pd = load_obj
                .get("active_power_demand_mw")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let existing_qd = load_obj
                .get("reactive_power_demand_mvar")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            load_obj.insert(
                "active_power_demand_mw".to_string(),
                serde_json::json!(existing_pd + pd),
            );
            load_obj.insert(
                "reactive_power_demand_mvar".to_string(),
                serde_json::json!(existing_qd + qd),
            );
        }
    }

    // Append synthetic loads.
    if !synthetic_loads.is_empty() {
        let loads_value = root.entry("loads").or_insert_with(|| serde_json::json!([]));
        let Some(loads) = loads_value.as_array_mut() else {
            return Err(Error::InvalidDocument(
                "legacy demand migration requires `loads` to be an array".to_string(),
            ));
        };
        loads.extend(synthetic_loads);
    }

    Ok(json)
}

/// Migrate legacy flat market/dispatch fields into the current nested model.
///
/// Older Surge JSON files stored:
/// - dispatch data directly on `Generator` objects (`commitment_status`,
///   `reserve_offers`, `emission_rates`, ramp curves, etc.)
/// - dispatchable loads / pumped hydro / combined-cycle plants as top-level
///   fields on `Network` rather than under `network.market_data`
/// - dispatchable-load bus references as `bus_idx` (array index) rather than
///   canonical external bus numbers
/// - pumped-hydro generator references as `gen_index` rather than
///   `GeneratorRef { bus, id }`
fn migrate_legacy_market_layout(mut json: serde_json::Value) -> Result<serde_json::Value, Error> {
    let Some(root) = json.as_object_mut() else {
        return Ok(json);
    };

    migrate_legacy_generator_fields(root)?;
    migrate_legacy_dispatchable_loads(root)?;
    migrate_legacy_pumped_hydro_units(root)?;
    migrate_legacy_market_sections(root)?;

    Ok(json)
}

fn migrate_legacy_generator_fields(
    root: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), Error> {
    let Some(generators) = root
        .get_mut("generators")
        .and_then(|value| value.as_array_mut())
    else {
        return Ok(());
    };

    for (idx, generator) in generators.iter_mut().enumerate() {
        let Some(generator_obj) = generator.as_object_mut() else {
            return Err(Error::InvalidDocument(format!(
                "legacy generator migration failed because generator index {idx} is not an object"
            )));
        };

        migrate_legacy_generator_type(generator_obj);

        let fault_data = take_legacy_fields(
            generator_obj,
            &[
                ("xs", "xs"),
                ("x2_pu", "x2_pu"),
                ("r2_pu", "r2_pu"),
                ("x0_pu", "x0_pu"),
                ("r0_pu", "r0_pu"),
                ("zn", "zn"),
            ],
        );
        let inverter = take_legacy_fields(
            generator_obj,
            &[
                ("s_rated_mva", "s_rated_mva"),
                ("p_available_mw", "p_available_mw"),
                ("curtailable", "curtailable"),
                ("grid_forming", "grid_forming"),
                ("inverter_loss_a_mw", "inverter_loss_a_mw"),
                ("inverter_loss_b", "inverter_loss_b_pu"),
                ("inverter_loss_b_pu", "inverter_loss_b_pu"),
            ],
        );
        let commitment = take_legacy_fields(
            generator_obj,
            &[
                ("commitment_status", "status"),
                ("p_ecomin", "p_ecomin"),
                ("p_ecomax", "p_ecomax"),
                ("p_emergency_min", "p_emergency_min"),
                ("p_emergency_max", "p_emergency_max"),
                ("p_reg_min", "p_reg_min"),
                ("p_reg_max", "p_reg_max"),
                ("min_up_time_hr", "min_up_time_hr"),
                ("min_down_time_hr", "min_down_time_hr"),
                ("max_up_time_hr", "max_up_time_hr"),
                ("min_run_at_pmin_hr", "min_run_at_pmin_hr"),
                ("max_starts_per_day", "max_starts_per_day"),
                ("max_starts_per_week", "max_starts_per_week"),
                ("max_energy_mwh_per_day", "max_energy_mwh_per_day"),
                ("shutdown_ramp_mw_per_min", "shutdown_ramp_mw_per_min"),
                ("startup_ramp_mw_per_min", "startup_ramp_mw_per_min"),
                ("forbidden_zones", "forbidden_zones"),
                ("hours_online", "hours_online"),
                ("hours_offline", "hours_offline"),
            ],
        );
        let ramping = take_legacy_fields(
            generator_obj,
            &[
                ("ramp_up_curve", "ramp_up_curve"),
                ("ramp_down_curve", "ramp_down_curve"),
                ("emergency_ramp_up_curve", "emergency_ramp_up_curve"),
                ("emergency_ramp_down_curve", "emergency_ramp_down_curve"),
                ("reg_ramp_up_curve", "reg_ramp_up_curve"),
                ("reg_ramp_down_curve", "reg_ramp_down_curve"),
            ],
        );
        let fuel = take_legacy_fields(
            generator_obj,
            &[
                ("fuel_type", "fuel_type"),
                ("heat_rate_btu_mwh", "heat_rate_btu_mwh"),
                ("primary_fuel", "primary_fuel"),
                ("backup_fuel", "backup_fuel"),
                ("fuel_switch_time_min", "fuel_switch_time_min"),
                ("on_backup_fuel", "on_backup_fuel"),
                ("emission_rates", "emission_rates"),
            ],
        );
        let market = take_legacy_fields(
            generator_obj,
            &[
                ("energy_offer", "energy_offer"),
                ("reserve_offers", "reserve_offers"),
                ("qualifications", "qualifications"),
            ],
        );
        let reactive_capability = take_legacy_fields(generator_obj, &[("pq_curve", "pq_curve")]);

        merge_legacy_generator_group(generator_obj, "fault_data", fault_data)?;
        merge_legacy_generator_group(generator_obj, "inverter", inverter)?;
        merge_legacy_generator_group(generator_obj, "commitment", commitment)?;
        merge_legacy_generator_group(generator_obj, "ramping", ramping)?;
        merge_legacy_generator_group(generator_obj, "fuel", fuel)?;
        merge_legacy_generator_group(generator_obj, "market", market)?;
        merge_legacy_generator_group(generator_obj, "reactive_capability", reactive_capability)?;
    }

    Ok(())
}

fn migrate_legacy_generator_type(generator_obj: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(legacy_type) = generator_obj
        .get("gen_type")
        .and_then(|value| value.as_str())
    else {
        return;
    };
    let replacement = match legacy_type {
        "Wind" => {
            generator_obj
                .entry("technology".to_string())
                .or_insert_with(|| serde_json::json!("Wind"));
            Some("InverterBased")
        }
        "Solar" => {
            generator_obj
                .entry("technology".to_string())
                .or_insert_with(|| serde_json::json!("SolarPv"));
            Some("InverterBased")
        }
        "InverterOther" => Some("InverterBased"),
        _ => None,
    };
    if let Some(value) = replacement {
        generator_obj.insert("gen_type".to_string(), serde_json::json!(value));
    }
}

fn take_legacy_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    mappings: &[(&str, &str)],
) -> serde_json::Map<String, serde_json::Value> {
    let mut nested = serde_json::Map::new();
    for (old_key, new_key) in mappings {
        if let Some(value) = obj.remove(*old_key) {
            nested.entry((*new_key).to_string()).or_insert(value);
        }
    }
    nested
}

fn merge_legacy_generator_group(
    generator_obj: &mut serde_json::Map<String, serde_json::Value>,
    group_key: &str,
    legacy_fields: serde_json::Map<String, serde_json::Value>,
) -> Result<(), Error> {
    if legacy_fields.is_empty() {
        return Ok(());
    }
    let nested = generator_obj
        .entry(group_key.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let nested_obj = nested.as_object_mut().ok_or_else(|| {
        Error::InvalidDocument(format!(
            "legacy generator migration requires '{group_key}' to be an object"
        ))
    })?;
    for (key, value) in legacy_fields {
        nested_obj.entry(key).or_insert(value);
    }
    Ok(())
}

fn migrate_legacy_dispatchable_loads(
    root: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), Error> {
    let Some(dispatchable_loads) = root.remove("dispatchable_loads") else {
        return Ok(());
    };
    let Some(loads) = dispatchable_loads.as_array() else {
        return Err(Error::InvalidDocument(
            "legacy market migration requires 'dispatchable_loads' to be an array".to_string(),
        ));
    };

    let bus_numbers = bus_numbers_by_index(root)?;
    let mut migrated = Vec::with_capacity(loads.len());
    for (idx, load) in loads.iter().enumerate() {
        let Some(load_obj) = load.as_object() else {
            return Err(Error::InvalidDocument(format!(
                "legacy dispatchable-load migration failed because resource index {idx} is not an object"
            )));
        };
        let mut load_obj = load_obj.clone();
        if !load_obj.contains_key("bus")
            && let Some(bus_idx) = load_obj.remove("bus_idx")
        {
            let Some(bus_idx) = bus_idx.as_u64() else {
                return Err(Error::InvalidDocument(format!(
                    "legacy dispatchable-load bus_idx at index {idx} must be an unsigned integer"
                )));
            };
            let Some(&bus_number) = bus_numbers.get(bus_idx as usize) else {
                return Err(Error::InvalidDocument(format!(
                    "legacy dispatchable-load bus_idx {bus_idx} is out of range"
                )));
            };
            load_obj.insert("bus".to_string(), serde_json::json!(bus_number));
        }
        migrated.push(serde_json::Value::Object(load_obj));
    }

    insert_market_data_section(
        root,
        "dispatchable_loads",
        serde_json::Value::Array(migrated),
    )
}

fn migrate_legacy_pumped_hydro_units(
    root: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), Error> {
    let Some(pumped_hydro_units) = root.remove("pumped_hydro_units") else {
        return Ok(());
    };
    let Some(units) = pumped_hydro_units.as_array() else {
        return Err(Error::InvalidDocument(
            "legacy market migration requires 'pumped_hydro_units' to be an array".to_string(),
        ));
    };

    let generator_refs = generator_refs_by_index(root)?;
    let mut migrated = Vec::with_capacity(units.len());
    for (idx, unit) in units.iter().enumerate() {
        let Some(unit_obj) = unit.as_object() else {
            return Err(Error::InvalidDocument(format!(
                "legacy pumped-hydro migration failed because unit index {idx} is not an object"
            )));
        };
        let mut unit_obj = unit_obj.clone();
        if !unit_obj.contains_key("generator")
            && let Some(gen_index) = unit_obj.remove("gen_index")
        {
            let Some(gen_index) = gen_index.as_u64() else {
                return Err(Error::InvalidDocument(format!(
                    "legacy pumped-hydro gen_index at unit {idx} must be an unsigned integer"
                )));
            };
            let Some((bus, id)) = generator_refs.get(gen_index as usize) else {
                return Err(Error::InvalidDocument(format!(
                    "legacy pumped-hydro gen_index {gen_index} is out of range"
                )));
            };
            unit_obj.insert(
                "generator".to_string(),
                serde_json::json!({ "bus": bus, "id": id }),
            );
        }
        migrated.push(serde_json::Value::Object(unit_obj));
    }

    insert_market_data_section(
        root,
        "pumped_hydro_units",
        serde_json::Value::Array(migrated),
    )
}

fn migrate_legacy_market_sections(
    root: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), Error> {
    for section in [
        "combined_cycle_plants",
        "outage_schedule",
        "reserve_zones",
        "ambient",
        "emission_policy",
        "market_rules",
    ] {
        if let Some(value) = root.remove(section) {
            insert_market_data_section(root, section, value)?;
        }
    }
    Ok(())
}

fn insert_market_data_section(
    root: &mut serde_json::Map<String, serde_json::Value>,
    section: &str,
    value: serde_json::Value,
) -> Result<(), Error> {
    let market_data = root
        .entry("market_data".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let market_data_obj = market_data.as_object_mut().ok_or_else(|| {
        Error::InvalidDocument(
            "legacy market migration requires 'market_data' to be an object".to_string(),
        )
    })?;
    market_data_obj.entry(section.to_string()).or_insert(value);
    Ok(())
}

fn bus_numbers_by_index(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<u32>, Error> {
    let Some(buses) = root.get("buses").and_then(|value| value.as_array()) else {
        return Ok(Vec::new());
    };

    let mut numbers = Vec::with_capacity(buses.len());
    for (idx, bus) in buses.iter().enumerate() {
        let Some(number) = bus.get("number").and_then(|value| value.as_u64()) else {
            return Err(Error::InvalidDocument(format!(
                "legacy market migration requires bus index {idx} to carry an unsigned 'number'"
            )));
        };
        numbers.push(number as u32);
    }
    Ok(numbers)
}

fn generator_refs_by_index(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<(u32, String)>, Error> {
    let Some(generators) = root.get("generators").and_then(|value| value.as_array()) else {
        return Ok(Vec::new());
    };

    let mut refs = Vec::with_capacity(generators.len());
    for (idx, generator) in generators.iter().enumerate() {
        let Some(bus) = generator.get("bus").and_then(|value| value.as_u64()) else {
            return Err(Error::InvalidDocument(format!(
                "legacy market migration requires generator index {idx} to carry an unsigned 'bus'"
            )));
        };
        let id = generator
            .get("id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                Error::InvalidDocument(format!(
                    "legacy market migration requires generator index {idx} to carry a string 'id'"
                ))
            })?
            .to_string();
        refs.push((bus as u32, id));
    }
    Ok(refs)
}

fn value_to_json(value: SerdeValue) -> Result<serde_json::Value, Error> {
    use serde_json::{Map, Number, Value};

    fn special_float(value: &str) -> Value {
        Value::Object(Map::from_iter([(
            SPECIAL_FLOAT_TAG.to_string(),
            Value::String(value.to_string()),
        )]))
    }

    fn special_bytes(bytes: Vec<u8>) -> Value {
        Value::Object(Map::from_iter([(
            SPECIAL_BYTES_TAG.to_string(),
            Value::Array(
                bytes
                    .into_iter()
                    .map(|byte| Value::Number(Number::from(byte)))
                    .collect(),
            ),
        )]))
    }

    fn special_map(entries: Vec<Value>) -> Value {
        Value::Object(Map::from_iter([(
            SPECIAL_MAP_TAG.to_string(),
            Value::Array(entries),
        )]))
    }

    fn map_key_to_string(key: SerdeValue) -> Result<String, Error> {
        Ok(match key {
            SerdeValue::Bool(value) => value.to_string(),
            SerdeValue::U8(value) => value.to_string(),
            SerdeValue::U16(value) => value.to_string(),
            SerdeValue::U32(value) => value.to_string(),
            SerdeValue::U64(value) => value.to_string(),
            SerdeValue::I8(value) => value.to_string(),
            SerdeValue::I16(value) => value.to_string(),
            SerdeValue::I32(value) => value.to_string(),
            SerdeValue::I64(value) => value.to_string(),
            SerdeValue::F32(value) => value.to_string(),
            SerdeValue::F64(value) => value.to_string(),
            SerdeValue::Char(value) => value.to_string(),
            SerdeValue::String(value) => value,
            other => {
                return Err(Error::InvalidTaggedValue(format!(
                    "unsupported map key value {other:?}"
                )));
            }
        })
    }

    Ok(match value {
        SerdeValue::Bool(value) => Value::Bool(value),
        SerdeValue::U8(value) => Value::Number(Number::from(value)),
        SerdeValue::U16(value) => Value::Number(Number::from(value)),
        SerdeValue::U32(value) => Value::Number(Number::from(value)),
        SerdeValue::U64(value) => Value::Number(Number::from(value)),
        SerdeValue::I8(value) => Value::Number(Number::from(value)),
        SerdeValue::I16(value) => Value::Number(Number::from(value)),
        SerdeValue::I32(value) => Value::Number(Number::from(value)),
        SerdeValue::I64(value) => Value::Number(Number::from(value)),
        SerdeValue::F32(value) => {
            if value.is_finite() {
                Value::Number(Number::from_f64(value as f64).expect("finite f32 is JSON-safe"))
            } else if value.is_nan() {
                special_float("NaN")
            } else if value.is_sign_positive() {
                special_float("Infinity")
            } else {
                special_float("-Infinity")
            }
        }
        SerdeValue::F64(value) => {
            if value.is_finite() {
                Value::Number(Number::from_f64(value).expect("finite f64 is JSON-safe"))
            } else if value.is_nan() {
                special_float("NaN")
            } else if value.is_sign_positive() {
                special_float("Infinity")
            } else {
                special_float("-Infinity")
            }
        }
        SerdeValue::Char(value) => Value::String(value.to_string()),
        SerdeValue::String(value) => Value::String(value),
        SerdeValue::Unit | SerdeValue::Option(None) => Value::Null,
        SerdeValue::Option(Some(value)) | SerdeValue::Newtype(value) => value_to_json(*value)?,
        SerdeValue::Seq(values) => Value::Array(
            values
                .into_iter()
                .map(value_to_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SerdeValue::Map(values) => {
            let all_string_keys = values
                .keys()
                .all(|key| matches!(key, SerdeValue::String(_)));
            if all_string_keys {
                let mut object = Map::with_capacity(values.len());
                for (key, value) in values {
                    object.insert(map_key_to_string(key)?, value_to_json(value)?);
                }
                Value::Object(object)
            } else {
                let mut entries = Vec::with_capacity(values.len());
                for (key, value) in values {
                    entries.push(Value::Array(vec![
                        value_to_json(key)?,
                        value_to_json(value)?,
                    ]));
                }
                special_map(entries)
            }
        }
        SerdeValue::Bytes(bytes) => special_bytes(bytes),
    })
}

fn json_to_value(value: serde_json::Value) -> Result<SerdeValue, Error> {
    use serde_json::Value;

    fn parse_special_float(value: &str) -> Result<SerdeValue, Error> {
        match value {
            "NaN" => Ok(SerdeValue::F64(f64::NAN)),
            "Infinity" => Ok(SerdeValue::F64(f64::INFINITY)),
            "-Infinity" => Ok(SerdeValue::F64(f64::NEG_INFINITY)),
            other => Err(Error::InvalidTaggedValue(format!(
                "unknown special float marker {other}"
            ))),
        }
    }

    Ok(match value {
        Value::Null => SerdeValue::Option(None),
        Value::Bool(value) => SerdeValue::Bool(value),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                SerdeValue::I64(value)
            } else if let Some(value) = value.as_u64() {
                SerdeValue::U64(value)
            } else if let Some(value) = value.as_f64() {
                SerdeValue::F64(value)
            } else {
                return Err(Error::InvalidTaggedValue(
                    "unsupported JSON number representation".to_string(),
                ));
            }
        }
        Value::String(value) => SerdeValue::String(value),
        Value::Array(values) => SerdeValue::Seq(
            values
                .into_iter()
                .map(json_to_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Object(mut object) => {
            if object.len() == 1 {
                if let Some(Value::String(value)) = object.remove(SPECIAL_FLOAT_TAG) {
                    return parse_special_float(&value);
                }
                if let Some(Value::Array(values)) = object.remove(SPECIAL_BYTES_TAG) {
                    let mut bytes = Vec::with_capacity(values.len());
                    for value in values {
                        let Value::Number(number) = value else {
                            return Err(Error::InvalidTaggedValue(
                                "byte tag must contain only numbers".to_string(),
                            ));
                        };
                        let Some(value) = number.as_u64() else {
                            return Err(Error::InvalidTaggedValue(
                                "byte tag numbers must be unsigned integers".to_string(),
                            ));
                        };
                        bytes.push(u8::try_from(value).map_err(|_| {
                            Error::InvalidTaggedValue(format!(
                                "byte tag value {value} is out of range"
                            ))
                        })?);
                    }
                    return Ok(SerdeValue::Bytes(bytes));
                }
                if let Some(Value::Array(entries)) = object.remove(SPECIAL_MAP_TAG) {
                    let mut map = std::collections::BTreeMap::new();
                    for entry in entries {
                        let Value::Array(mut pair) = entry else {
                            return Err(Error::InvalidTaggedValue(
                                "map tag must contain [key, value] pairs".to_string(),
                            ));
                        };
                        if pair.len() != 2 {
                            return Err(Error::InvalidTaggedValue(
                                "map tag pairs must contain exactly two values".to_string(),
                            ));
                        }
                        let value = json_to_value(pair.pop().expect("pair length checked"))?;
                        let key = json_to_value(pair.pop().expect("pair length checked"))?;
                        map.insert(key, value);
                    }
                    return Ok(SerdeValue::Map(map));
                }
            }

            let mut map = std::collections::BTreeMap::new();
            for (key, value) in object {
                map.insert(SerdeValue::String(key), json_to_value(value)?);
            }
            SerdeValue::Map(map)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use surge_network::network::generator::{CommitmentStatus, GenType, GeneratorTechnology};
    use surge_network::network::{Branch, Bus, BusType, Generator};
    use surge_solution::{
        AuditableSolution, ObjectiveLedgerMismatch, ObjectiveLedgerScopeKind, SolutionAuditReport,
    };

    #[derive(Clone, Serialize)]
    struct FakeAuditedSolution {
        total_cost: f64,
        #[serde(default)]
        audit: SolutionAuditReport,
        #[serde(skip)]
        computed_audit: SolutionAuditReport,
    }

    impl AuditableSolution for FakeAuditedSolution {
        fn computed_solution_audit(&self) -> SolutionAuditReport {
            self.computed_audit.clone()
        }
    }

    #[test]
    fn test_roundtrip() {
        let mut network = Network::new("test_json");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network.buses.push(Bus::new(2, BusType::PQ, 138.0));
        network.generators.push(Generator::new(1, 100.0, 1.06));
        network
            .branches
            .push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));

        let json_str = to_string(&network, false).expect("failed to serialize");
        assert!(json_str.contains(SURGE_JSON_FORMAT));
        assert!(json_str.contains(SURGE_JSON_SCHEMA_VERSION));
        assert!(json_str.contains(META_FIELD));
        let parsed = parse_str(&json_str).expect("failed to parse");

        assert_eq!(parsed.name, "test_json");
        assert_eq!(parsed.base_mva, 100.0);
        assert_eq!(parsed.n_buses(), 2);
        assert_eq!(parsed.generators.len(), 1);
        assert_eq!(parsed.n_branches(), 1);
        assert!((parsed.buses[0].base_kv - 138.0).abs() < 1e-10);
    }

    #[test]
    fn test_legacy_bus_demand_duplicate_with_existing_load_is_dropped() {
        let mut network = Network::new("merge_test");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network
            .loads
            .push(surge_network::network::Load::new(1, 75.0, 30.0));

        let json_str = to_string(&network, false).expect("failed to serialize");
        let mut doc: serde_json::Value = serde_json::from_str(&json_str).expect("valid json");
        let network_obj = doc
            .get_mut("network")
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized document should contain a network object");
        let buses = network_obj
            .get_mut("buses")
            .and_then(serde_json::Value::as_array_mut)
            .expect("serialized network should contain buses");
        buses[0]
            .as_object_mut()
            .expect("bus entry should be an object")
            .insert(
                "active_power_demand_mw".to_string(),
                serde_json::json!(75.0),
            );
        buses[0]
            .as_object_mut()
            .expect("bus entry should be an object")
            .insert(
                "reactive_power_demand_mvar".to_string(),
                serde_json::json!(30.0),
            );

        let parsed =
            parse_str(&doc.to_string()).expect("duplicate legacy demand should be ignored");
        assert_eq!(parsed.loads.len(), 1);
        assert!((parsed.loads[0].active_power_demand_mw - 75.0).abs() < 1e-10);
        assert!((parsed.loads[0].reactive_power_demand_mvar - 30.0).abs() < 1e-10);
    }

    #[test]
    fn test_legacy_bus_demand_conflicting_with_existing_load_errors() {
        let mut network = Network::new("merge_test_conflict");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network
            .loads
            .push(surge_network::network::Load::new(1, 25.0, 10.0));

        let json_str = to_string(&network, false).expect("failed to serialize");
        let mut doc: serde_json::Value = serde_json::from_str(&json_str).expect("valid json");
        let network_obj = doc
            .get_mut("network")
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized document should contain a network object");
        let buses = network_obj
            .get_mut("buses")
            .and_then(serde_json::Value::as_array_mut)
            .expect("serialized network should contain buses");
        buses[0]
            .as_object_mut()
            .expect("bus entry should be an object")
            .insert(
                "active_power_demand_mw".to_string(),
                serde_json::json!(75.0),
            );
        buses[0]
            .as_object_mut()
            .expect("bus entry should be an object")
            .insert(
                "reactive_power_demand_mvar".to_string(),
                serde_json::json!(30.0),
            );

        let err =
            parse_str(&doc.to_string()).expect_err("conflicting mixed-format demand should error");
        assert!(
            err.to_string()
                .contains("conflicts with existing explicit load data"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_legacy_bus_demand_rejects_non_array_loads_field() {
        let mut network = Network::new("bad-loads-shape");
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut doc = encode_document(&network).expect("serialize document");
        let network_obj = doc
            .get_mut("network")
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized network should contain an object");
        let buses = network_obj
            .get_mut("buses")
            .and_then(serde_json::Value::as_array_mut)
            .expect("serialized network should contain buses");
        let bus = buses[0]
            .as_object_mut()
            .expect("serialized bus should be an object");
        bus.insert(
            "active_power_demand_mw".to_string(),
            serde_json::json!(10.0),
        );
        bus.insert(
            "reactive_power_demand_mvar".to_string(),
            serde_json::json!(5.0),
        );
        network_obj.insert("loads".to_string(), serde_json::json!({}));

        let err = parse_str(&doc.to_string()).expect_err("non-array loads should be rejected");
        assert!(matches!(err, Error::InvalidDocument(msg) if msg.contains("loads")));
    }

    #[test]
    fn test_legacy_flat_generator_dispatch_fields_are_migrated() {
        let mut network = Network::new("legacy_generator_fields");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network.generators.push(Generator::new(1, 50.0, 1.0));

        let mut doc = encode_document(&network).expect("serialize document");
        let network_obj = doc
            .get_mut("network")
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized document should contain a network object");
        let generator = network_obj
            .get_mut("generators")
            .and_then(serde_json::Value::as_array_mut)
            .and_then(|generators| generators.first_mut())
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized network should contain a generator object");
        generator.insert(
            "commitment_status".to_string(),
            serde_json::json!("MustRun"),
        );
        generator.insert("min_up_time_hr".to_string(), serde_json::json!(4.0));
        generator.insert("hours_online".to_string(), serde_json::json!(3.0));
        generator.insert("ramp_up_curve".to_string(), serde_json::json!([[0.0, 6.0]]));
        generator.insert(
            "reserve_offers".to_string(),
            serde_json::json!([{ "product_id": "spin", "capacity_mw": 20.0, "cost_per_mwh": 4.0 }]),
        );
        generator.insert(
            "qualifications".to_string(),
            serde_json::json!({ "spin": true, "reg_up": false }),
        );
        generator.insert("fuel_type".to_string(), serde_json::json!("gas"));
        generator.insert(
            "emission_rates".to_string(),
            serde_json::json!({ "co2": 0.42, "nox": 0.01, "so2": 0.0, "pm25": 0.0 }),
        );
        generator.insert("curtailable".to_string(), serde_json::json!(true));
        generator.insert("grid_forming".to_string(), serde_json::json!(true));
        generator.insert("inverter_loss_a_mw".to_string(), serde_json::json!(0.5));
        generator.insert("inverter_loss_b".to_string(), serde_json::json!(0.02));

        let parsed =
            parse_str(&doc.to_string()).expect("legacy flat generator fields should migrate");
        let generator = &parsed.generators[0];
        let commitment = generator
            .commitment
            .as_ref()
            .expect("commitment fields should be nested during migration");
        assert_eq!(commitment.status, CommitmentStatus::MustRun);
        assert_eq!(commitment.min_up_time_hr, Some(4.0));
        assert!((commitment.hours_online - 3.0).abs() < 1e-9);
        assert_eq!(
            generator
                .ramping
                .as_ref()
                .expect("ramp fields should migrate")
                .ramp_up_curve,
            vec![(0.0, 6.0)]
        );
        let market = generator
            .market
            .as_ref()
            .expect("market fields should migrate");
        assert_eq!(market.reserve_offers.len(), 1);
        assert_eq!(market.qualifications.get("spin"), Some(&true));
        let fuel = generator.fuel.as_ref().expect("fuel fields should migrate");
        assert_eq!(fuel.fuel_type.as_deref(), Some("gas"));
        assert!((fuel.emission_rates.co2 - 0.42).abs() < 1e-9);
        let inverter = generator
            .inverter
            .as_ref()
            .expect("legacy inverter fields should migrate");
        assert!(inverter.curtailable);
        assert!(inverter.grid_forming);
        assert!((inverter.inverter_loss_a_mw - 0.5).abs() < 1e-9);
        assert!((inverter.inverter_loss_b_pu - 0.02).abs() < 1e-9);
    }

    #[test]
    fn test_legacy_generator_type_is_narrowed_to_electrical_class() {
        let mut network = Network::new("legacy_generator_type");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network.generators.push(Generator::new(1, 50.0, 1.0));

        let mut doc = encode_document(&network).expect("serialize document");
        let network_obj = doc
            .get_mut("network")
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized document should contain a network object");
        let generator = network_obj
            .get_mut("generators")
            .and_then(serde_json::Value::as_array_mut)
            .and_then(|generators| generators.first_mut())
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized network should contain a generator object");
        generator.insert("gen_type".to_string(), serde_json::json!("Wind"));

        let parsed = parse_str(&doc.to_string()).expect("legacy generator type should migrate");
        let generator = &parsed.generators[0];
        assert_eq!(generator.gen_type, GenType::InverterBased);
        assert_eq!(generator.technology, Some(GeneratorTechnology::Wind));
    }

    #[test]
    fn test_legacy_flat_market_sections_are_nested_under_market_data() {
        let mut network = Network::new("legacy_market_sections");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network.buses.push(Bus::new(2, BusType::PQ, 138.0));
        network
            .generators
            .push(Generator::with_id("gen_a", 1, 60.0, 1.0));
        network
            .generators
            .push(Generator::with_id("gen_b", 2, 40.0, 1.0));

        let mut doc = encode_document(&network).expect("serialize document");
        let network_obj = doc
            .get_mut("network")
            .and_then(serde_json::Value::as_object_mut)
            .expect("serialized document should contain a network object");
        network_obj.insert(
            "dispatchable_loads".to_string(),
            serde_json::json!([{
                "bus_idx": 1,
                "p_sched_pu": 0.2,
                "q_sched_pu": 0.0,
                "p_min_pu": 0.0,
                "p_max_pu": 0.2,
                "q_min_pu": 0.0,
                "q_max_pu": 0.0,
                "archetype": "Curtailable",
                "cost_model": { "LinearCurtailment": { "cost_per_mw": 100.0 } },
                "fixed_power_factor": true,
                "in_service": true,
                "resource_id": "legacy_dr"
            }]),
        );
        network_obj.insert(
            "pumped_hydro_units".to_string(),
            serde_json::json!([{
                "name": "legacy_ph",
                "gen_index": 1,
                "variable_speed": false,
                "pump_mw_fixed": 0.0,
                "pump_mw_min": 20.0,
                "pump_mw_max": 80.0,
                "mode_transition_min": 5.0,
                "condenser_capable": false,
                "forbidden_zone": null,
                "upper_reservoir_mwh": 500.0,
                "lower_reservoir_mwh": 1.7976931348623157e308,
                "soc_initial_mwh": 250.0,
                "soc_min_mwh": 50.0,
                "soc_max_mwh": 450.0,
                "efficiency_generate": 0.9,
                "efficiency_pump": 0.88,
                "head_curve": [],
                "n_units": 1,
                "shared_penstock_mw_max": null,
                "min_release_mw": 0.0,
                "ramp_rate_mw_per_min": null,
                "startup_time_gen_min": 5.0,
                "startup_time_pump_min": 10.0,
                "startup_cost": 200.0
            }]),
        );
        network_obj.insert(
            "combined_cycle_plants".to_string(),
            serde_json::json!([{
                "name": "legacy_cc",
                "configs": [{
                    "name": "GT_ONLY",
                    "gen_indices": [0],
                    "p_min_mw": 20.0,
                    "p_max_mw": 80.0,
                    "heat_rate_curve": [],
                    "energy_offer": null,
                    "ramp_up_curve": [],
                    "ramp_down_curve": [],
                    "no_load_cost": 0.0,
                    "min_up_time_hr": 1.0,
                    "min_down_time_hr": 1.0
                }],
                "transitions": [],
                "active_config": "GT_ONLY",
                "hours_in_config": 2.0,
                "duct_firing_capable": false
            }]),
        );

        let parsed = parse_str(&doc.to_string()).expect("legacy market sections should migrate");
        assert_eq!(parsed.market_data.dispatchable_loads.len(), 1);
        assert_eq!(parsed.market_data.dispatchable_loads[0].bus, 2);
        assert_eq!(
            parsed.market_data.dispatchable_loads[0].resource_id,
            "legacy_dr"
        );
        assert_eq!(parsed.market_data.pumped_hydro_units.len(), 1);
        assert_eq!(parsed.market_data.pumped_hydro_units[0].generator.bus, 2);
        assert_eq!(
            parsed.market_data.pumped_hydro_units[0].generator.id,
            "gen_b"
        );
        assert_eq!(parsed.market_data.combined_cycle_plants.len(), 1);
        assert_eq!(
            parsed.market_data.combined_cycle_plants[0].name,
            "legacy_cc"
        );
    }

    #[test]
    fn test_file_roundtrip() {
        let mut network = Network::new("file_test");
        network.buses.push(Bus::new(1, BusType::Slack, 345.0));
        network.generators.push(Generator::new(1, 50.0, 1.04));
        network
            .branches
            .push(Branch::new_line(1, 1, 0.0, 0.01, 0.0));

        let tmp = std::env::temp_dir().join("surge_test_roundtrip.surge.json");
        write_file(&network, &tmp, false).expect("failed to write");
        let parsed = parse_file(&tmp).expect("failed to read");
        assert_eq!(parsed.name, "file_test");
        assert_eq!(parsed.n_buses(), 1);

        // Cleanup
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_non_finite_values_roundtrip() {
        let mut network = Network::new("non_finite");
        network.buses.push(Bus::new(1, BusType::Slack, 345.0));
        let mut generator = Generator::new(1, 50.0, 1.04);
        generator.pmax = f64::INFINITY;
        generator.qmin = f64::NEG_INFINITY;
        network.generators.push(generator);

        let json = to_string(&network, false).expect("non-finite values should serialize");
        assert!(json.contains(SPECIAL_FLOAT_TAG));

        let round_tripped = parse_str(&json).expect("non-finite values should deserialize");
        assert!(round_tripped.generators[0].pmax.is_infinite());
        assert!(round_tripped.generators[0].pmax.is_sign_positive());
        assert!(round_tripped.generators[0].qmin.is_infinite());
        assert!(round_tripped.generators[0].qmin.is_sign_negative());
    }

    #[test]
    fn test_zstd_file_roundtrip() {
        let mut network = Network::new("zstd_json");
        network.buses.push(Bus::new(1, BusType::Slack, 345.0));
        let tmp = std::env::temp_dir().join("surge_test_roundtrip.surge.json.zst");
        save(&network, &tmp).expect("failed to save zstd json");
        let parsed = load(&tmp).expect("failed to load zstd json");
        assert_eq!(parsed.name, "zstd_json");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_missing_document_metadata_is_rejected() {
        let result = parse_str("{\"base_mva\":100.0}");
        assert!(result.is_err(), "bare network JSON should be rejected");
    }

    #[test]
    fn test_unknown_schema_version_is_rejected() {
        let result = parse_str(
            r#"{
                "format": "surge-json",
                "schema_version": "999.0.0",
                "network": {}
            }"#,
        );
        assert!(result.is_err(), "unknown schema version should be rejected");
    }

    #[test]
    fn test_invalid_meta_profile_is_rejected() {
        let result = parse_str(
            r#"{
                "format": "surge-json",
                "schema_version": "0.1.0",
                "meta": { "producer": "surge", "profile": "solution" },
                "network": {}
            }"#,
        );
        assert!(result.is_err(), "unknown meta profile should be rejected");
    }

    #[test]
    fn test_encode_audited_solution_overwrites_stale_audit_block() {
        let solution = FakeAuditedSolution {
            total_cost: 123.0,
            audit: SolutionAuditReport {
                audit_passed: false,
                ..Default::default()
            },
            computed_audit: SolutionAuditReport::from_mismatches(Vec::new()),
        };

        let json = encode_audited_solution(&solution).expect("audit injection should succeed");
        let audit = json
            .get("audit")
            .and_then(serde_json::Value::as_object)
            .expect("encoded solution should carry an audit object");
        assert_eq!(
            audit
                .get("audit_passed")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            audit
                .get("schema_version")
                .and_then(serde_json::Value::as_str),
            Some(surge_solution::SOLUTION_AUDIT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn test_encode_checked_audited_solution_rejects_failed_audit() {
        let mismatch = ObjectiveLedgerMismatch {
            scope_kind: ObjectiveLedgerScopeKind::DispatchSolution,
            scope_id: "summary".to_string(),
            field: "total_cost".to_string(),
            expected_dollars: 10.0,
            actual_dollars: 11.0,
            difference: 1.0,
        };
        let solution = FakeAuditedSolution {
            total_cost: 11.0,
            audit: SolutionAuditReport::default(),
            computed_audit: SolutionAuditReport::from_mismatches(vec![mismatch]),
        };

        let err = encode_checked_audited_solution(&solution).expect_err("audit must fail fast");
        assert!(
            err.to_string().contains("solution audit failed"),
            "unexpected error: {err}"
        );
    }
}
