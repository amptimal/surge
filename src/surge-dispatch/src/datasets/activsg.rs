// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Import helpers for the public Texas A&M ACTIVSg time-series package.
//!
//! The TAMU package is distributed as PowerWorld-style wide CSV exports:
//! load MW, load MVAr, and renewable available MW by generator/bus. This
//! module converts that bundle into `surge-dispatch` request-side profile
//! structures keyed to a specific Surge network.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::request::{
    AcBusLoadProfile, AcBusLoadProfiles, BusLoadProfile, BusLoadProfiles, DispatchProfiles,
    DispatchTimeline, RenewableProfile, RenewableProfiles,
};
use crate::{DispatchError, DispatchRequest};
use chrono::{Duration, NaiveDateTime};
use csv::StringRecord;
use surge_network::Network;
use thiserror::Error;
use tracing::info;

const LOAD_DATA_OFFSET: usize = 5;
const DIRECT_CF_TOLERANCE_DEFAULT: f64 = 1e-6;
const TIMESTAMP_FORMAT: &str = "%m/%d/%Y %I:%M:%S %p";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivsgCase {
    Activsg2000,
    Activsg10k,
}

impl ActivsgCase {
    fn load_mw_filename(self) -> &'static str {
        match self {
            Self::Activsg2000 => "ACTIVISg2000_load_time_series_MW.csv",
            Self::Activsg10k => "ACTIVSg10k_load_time_series_MW.csv",
        }
    }

    fn load_mvar_filename(self) -> &'static str {
        match self {
            Self::Activsg2000 => "ACTIVISg2000_load_time_series_MVAR.csv",
            Self::Activsg10k => "ACTIVSg10k_load_time_series_MVAR.csv",
        }
    }

    fn renewable_filename(self) -> &'static str {
        match self {
            Self::Activsg2000 => "ACTIVISg2000_renewable_time_series_MW.csv",
            Self::Activsg10k => "ACTIVISg10k_renewable_time_series_MW.csv",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingTimestampPolicy {
    Error,
    #[default]
    RepeatPreviousDay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmappedSolarBusPolicy {
    Error,
    #[default]
    DropAndWarn,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActivsgImportOptions {
    pub renewable_timestamp_policy: MissingTimestampPolicy,
    pub unmapped_solar_bus_policy: UnmappedSolarBusPolicy,
    pub capacity_factor_tolerance: f64,
}

impl Default for ActivsgImportOptions {
    fn default() -> Self {
        Self {
            renewable_timestamp_policy: MissingTimestampPolicy::RepeatPreviousDay,
            unmapped_solar_bus_policy: UnmappedSolarBusPolicy::DropAndWarn,
            capacity_factor_tolerance: DIRECT_CF_TOLERANCE_DEFAULT,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActivsgImportReport {
    pub case: ActivsgCase,
    pub periods: usize,
    pub start_timestamp: NaiveDateTime,
    pub end_timestamp: NaiveDateTime,
    pub load_buses: usize,
    pub direct_renewable_generators: usize,
    pub solar_buses_aggregated: usize,
    pub solar_generators_profiled: usize,
    pub generator_pmax_overrides: usize,
    pub dropped_solar_buses: Vec<u32>,
    pub inserted_renewable_timestamps: Vec<NaiveDateTime>,
    pub clipped_capacity_factor_points: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActivsgTimeSeries {
    pub case: ActivsgCase,
    pub timestamps: Vec<NaiveDateTime>,
    pub ac_bus_load_profiles: AcBusLoadProfiles,
    pub renewable_profiles: RenewableProfiles,
    pub generator_pmax_overrides: BTreeMap<String, f64>,
    pub report: ActivsgImportReport,
}

impl ActivsgTimeSeries {
    pub fn periods(&self) -> usize {
        self.timestamps.len()
    }

    pub fn timeline(&self) -> DispatchTimeline {
        DispatchTimeline::hourly(self.periods())
    }

    pub fn timeline_for(&self, periods: usize) -> Result<DispatchTimeline, ActivsgImportError> {
        validate_requested_periods(self.periods(), periods)?;
        Ok(DispatchTimeline::hourly(periods))
    }

    pub fn load_profiles(&self) -> BusLoadProfiles {
        let profiles = self
            .ac_bus_load_profiles
            .profiles
            .iter()
            .map(|profile| BusLoadProfile {
                bus_number: profile.bus_number,
                values_mw: profile
                    .p_mw
                    .clone()
                    .unwrap_or_else(|| vec![0.0; self.periods()]),
            })
            .collect();

        BusLoadProfiles { profiles }
    }

    pub fn ac_dispatch_profiles(&self) -> DispatchProfiles {
        DispatchProfiles {
            ac_bus_load: self.ac_bus_load_profiles.clone(),
            renewable: self.renewable_profiles.clone(),
            ..DispatchProfiles::default()
        }
    }

    pub fn ac_profiles(&self, periods: usize) -> Result<DispatchProfiles, ActivsgImportError> {
        truncate_dispatch_profiles(self.ac_dispatch_profiles(), self.periods(), periods)
    }

    pub fn dc_dispatch_profiles(&self) -> DispatchProfiles {
        DispatchProfiles {
            load: self.load_profiles(),
            renewable: self.renewable_profiles.clone(),
            ..DispatchProfiles::default()
        }
    }

    pub fn dc_profiles(&self, periods: usize) -> Result<DispatchProfiles, ActivsgImportError> {
        truncate_dispatch_profiles(self.dc_dispatch_profiles(), self.periods(), periods)
    }

    pub fn dc_request(&self, periods: usize) -> Result<DispatchRequest, DispatchError> {
        let timeline = self
            .timeline_for(periods)
            .map_err(|error| DispatchError::InvalidInput(error.to_string()))?;
        let profiles = self
            .dc_profiles(periods)
            .map_err(|error| DispatchError::InvalidInput(error.to_string()))?;
        Ok(DispatchRequest::builder()
            .dc()
            .period_by_period()
            .all_committed()
            .timeline(timeline)
            .profiles(profiles)
            .build())
    }

    pub fn ac_request(&self, periods: usize) -> Result<DispatchRequest, DispatchError> {
        let timeline = self
            .timeline_for(periods)
            .map_err(|error| DispatchError::InvalidInput(error.to_string()))?;
        let profiles = self
            .ac_profiles(periods)
            .map_err(|error| DispatchError::InvalidInput(error.to_string()))?;
        Ok(DispatchRequest::builder()
            .ac()
            .period_by_period()
            .all_committed()
            .timeline(timeline)
            .profiles(profiles)
            .build())
    }

    pub fn apply_nameplate_overrides(
        &self,
        network: &mut Network,
    ) -> Result<usize, ActivsgImportError> {
        let mut applied = 0usize;
        for generator in &mut network.generators {
            if let Some(&pmax) = self.generator_pmax_overrides.get(&generator.id) {
                generator.pmax = pmax;
                applied += 1;
            }
        }
        if applied != self.generator_pmax_overrides.len() {
            let missing = self
                .generator_pmax_overrides
                .keys()
                .find(|generator_id| {
                    !network
                        .generators
                        .iter()
                        .any(|g| g.id == generator_id.as_str())
                })
                .cloned()
                .expect("count mismatch implies a missing generator id");
            return Err(ActivsgImportError::NameplateOverrideTargetMissing {
                generator_id: missing,
            });
        }
        Ok(applied)
    }

    pub fn network_with_nameplate_overrides(
        &self,
        network: &Network,
    ) -> Result<Network, ActivsgImportError> {
        let mut adjusted = network.clone();
        self.apply_nameplate_overrides(&mut adjusted)?;
        Ok(adjusted)
    }
}

fn validate_requested_periods(
    available_periods: usize,
    periods: usize,
) -> Result<(), ActivsgImportError> {
    if periods == 0 || periods > available_periods {
        return Err(ActivsgImportError::InvalidRequestedPeriods {
            requested: periods,
            available: available_periods,
        });
    }
    Ok(())
}

fn truncate_dispatch_profiles(
    mut profiles: DispatchProfiles,
    available_periods: usize,
    periods: usize,
) -> Result<DispatchProfiles, ActivsgImportError> {
    validate_requested_periods(available_periods, periods)?;
    for profile in &mut profiles.load.profiles {
        profile.values_mw.truncate(periods);
    }

    for profile in &mut profiles.ac_bus_load.profiles {
        if let Some(p_mw) = &mut profile.p_mw {
            p_mw.truncate(periods);
        }
        if let Some(q_mvar) = &mut profile.q_mvar {
            q_mvar.truncate(periods);
        }
    }

    for profile in &mut profiles.renewable.profiles {
        profile.capacity_factors.truncate(periods);
    }

    for profile in &mut profiles.generator_derates.profiles {
        profile.derate_factors.truncate(periods);
    }

    for profile in &mut profiles.branch_derates.profiles {
        profile.derate_factors.truncate(periods);
    }

    for profile in &mut profiles.hvdc_derates.profiles {
        profile.derate_factors.truncate(periods);
    }

    Ok(profiles)
}

#[derive(Debug, Error)]
pub enum ActivsgImportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    #[error("requested {requested} period(s) but imported horizon has {available}")]
    InvalidRequestedPeriods { requested: usize, available: usize },

    #[error("missing required ACTIVSg file {path}")]
    MissingFile { path: PathBuf },

    #[error("invalid ACTIVSg CSV in {path}: {message}")]
    InvalidCsv { path: PathBuf, message: String },

    #[error("load timestamp mismatch between {left} and {right}")]
    DatasetTimestampMismatch { left: PathBuf, right: PathBuf },

    #[error("renewable timestamps in {path} could not align to target timestamp {timestamp}")]
    MissingTimestampFill {
        path: PathBuf,
        timestamp: NaiveDateTime,
    },

    #[error("timestamp {timestamp} appears in {path} but not in the load timeline")]
    UnexpectedTimestamp {
        path: PathBuf,
        timestamp: NaiveDateTime,
    },

    #[error("unknown bus {bus} referenced in {path}")]
    UnknownBus { path: PathBuf, bus: u32 },

    #[error("unknown generator at bus {bus} machine_id {machine_id} referenced in {path}")]
    UnknownGenerator {
        path: PathBuf,
        bus: u32,
        machine_id: String,
    },

    #[error("no candidate generators remain for solar bus {bus} in {path}")]
    NoSolarCandidates { path: PathBuf, bus: u32 },

    #[error("nameplate override references unknown generator_id {generator_id}")]
    NameplateOverrideTargetMissing { generator_id: String },

    #[error("generator {generator_id} has non-positive pmax {pmax}")]
    NonPositivePmax { generator_id: String, pmax: f64 },

    #[error(
        "available MW {mw} implies capacity factor {capacity_factor} for generator {generator_id} at period {period}, beyond tolerance {tolerance}"
    )]
    CapacityFactorOutOfRange {
        generator_id: String,
        period: usize,
        mw: f64,
        capacity_factor: f64,
        tolerance: f64,
    },
}

struct ParsedBusSeries {
    path: PathBuf,
    timestamps: Vec<NaiveDateTime>,
    values_by_bus: BTreeMap<u32, Vec<f64>>,
}

struct ParsedRenewableSeries {
    path: PathBuf,
    timestamps: Vec<NaiveDateTime>,
    direct_by_key: BTreeMap<(u32, String), Vec<f64>>,
    solar_by_bus_mw: BTreeMap<u32, Vec<f64>>,
}

pub fn read_tamu_activsg_time_series(
    network: &Network,
    root: impl AsRef<Path>,
    case: ActivsgCase,
    options: &ActivsgImportOptions,
) -> Result<ActivsgTimeSeries, ActivsgImportError> {
    let root = root.as_ref();
    let time_series_dir = resolve_time_series_dir(root);

    let load_mw_path = required_file(&time_series_dir, case.load_mw_filename())?;
    let load_mvar_path = required_file(&time_series_dir, case.load_mvar_filename())?;
    let renewable_path = required_file(&time_series_dir, case.renewable_filename())?;

    let network_bus_numbers: HashSet<u32> = network.buses.iter().map(|bus| bus.number).collect();
    let load_mw = read_load_bus_series(&load_mw_path, &network_bus_numbers)?;
    let load_mvar = read_load_bus_series(&load_mvar_path, &network_bus_numbers)?;

    if load_mw.timestamps != load_mvar.timestamps {
        return Err(ActivsgImportError::DatasetTimestampMismatch {
            left: load_mw.path.clone(),
            right: load_mvar.path.clone(),
        });
    }

    let renewable_raw = read_renewable_series(&renewable_path)?;
    let (renewable_aligned, inserted_renewable_timestamps) = align_renewable_series(
        renewable_raw,
        &load_mw.timestamps,
        options.renewable_timestamp_policy,
    )?;

    let ac_bus_load_profiles = build_ac_bus_load_profiles(
        load_mw.timestamps.len(),
        load_mw.values_by_bus,
        load_mvar.values_by_bus,
    );
    let (
        renewable_profiles,
        generator_pmax_overrides,
        direct_renewable_generators,
        solar_buses_aggregated,
        solar_generators_profiled,
        dropped_solar_buses,
        clipped_capacity_factor_points,
    ) = build_renewable_profiles(network, renewable_aligned, options)?;

    let timestamps = load_mw.timestamps;
    let report = ActivsgImportReport {
        case,
        periods: timestamps.len(),
        start_timestamp: *timestamps
            .first()
            .ok_or_else(|| ActivsgImportError::InvalidCsv {
                path: load_mw_path.clone(),
                message: "load dataset contains no data rows".to_string(),
            })?,
        end_timestamp: *timestamps
            .last()
            .ok_or_else(|| ActivsgImportError::InvalidCsv {
                path: load_mw_path.clone(),
                message: "load dataset contains no data rows".to_string(),
            })?,
        load_buses: ac_bus_load_profiles.profiles.len(),
        direct_renewable_generators,
        solar_buses_aggregated,
        solar_generators_profiled,
        generator_pmax_overrides: generator_pmax_overrides.len(),
        dropped_solar_buses,
        inserted_renewable_timestamps,
        clipped_capacity_factor_points,
    };

    info!(
        case = ?case,
        periods = report.periods,
        load_buses = report.load_buses,
        renewable_profiles = renewable_profiles.profiles.len(),
        "imported TAMU ACTIVSg time series"
    );

    Ok(ActivsgTimeSeries {
        case,
        timestamps,
        ac_bus_load_profiles,
        renewable_profiles,
        generator_pmax_overrides,
        report,
    })
}

fn resolve_time_series_dir(root: &Path) -> PathBuf {
    let nested = root.join("Time Series");
    if nested.is_dir() {
        nested
    } else {
        root.to_path_buf()
    }
}

fn required_file(dir: &Path, filename: &str) -> Result<PathBuf, ActivsgImportError> {
    let path = dir.join(filename);
    if path.is_file() {
        Ok(path)
    } else {
        Err(ActivsgImportError::MissingFile { path })
    }
}

fn read_load_bus_series(
    path: &Path,
    network_bus_numbers: &HashSet<u32>,
) -> Result<ParsedBusSeries, ActivsgImportError> {
    let (column_defs, timestamps, values) =
        read_wide_csv(path, parse_load_header, LOAD_DATA_OFFSET)?;
    let mut bus_series = aggregate_columns_by_bus(&column_defs, timestamps.len(), values);

    for bus in bus_series.keys().copied() {
        if !network_bus_numbers.contains(&bus) {
            return Err(ActivsgImportError::UnknownBus {
                path: path.to_path_buf(),
                bus,
            });
        }
    }

    Ok(ParsedBusSeries {
        path: path.to_path_buf(),
        timestamps,
        values_by_bus: std::mem::take(&mut bus_series),
    })
}

fn read_renewable_series(path: &Path) -> Result<ParsedRenewableSeries, ActivsgImportError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(path)?;

    let _category_row = next_record(&mut reader, path, 1)?;
    let header_row = next_record(&mut reader, path, 2)?;
    let headers = header_row.iter().collect::<Vec<_>>();
    if headers.len() < LOAD_DATA_OFFSET {
        return Err(ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("expected at least {LOAD_DATA_OFFSET} columns in renewable header"),
        });
    }

    let mut direct_keys = Vec::new();
    let mut direct_series = Vec::new();
    let mut solar_buses = Vec::new();
    let mut solar_series = Vec::new();
    let mut direct_columns = Vec::new();
    let mut solar_columns = Vec::new();
    let mut direct_index_by_key: HashMap<(u32, String), usize> = HashMap::new();
    let mut solar_index_by_bus: HashMap<u32, usize> = HashMap::new();

    for col_idx in LOAD_DATA_OFFSET..header_row.len() {
        let label = header_row.get(col_idx).map(str::trim).unwrap_or_default();
        if label.is_empty() {
            continue;
        }
        if label.starts_with("Gen ") {
            let key = parse_generator_header(path, label)?;
            let entry_idx = match direct_index_by_key.get(&key) {
                Some(&idx) => idx,
                None => {
                    let idx = direct_keys.len();
                    direct_index_by_key.insert(key.clone(), idx);
                    direct_keys.push(key);
                    direct_series.push(Vec::new());
                    idx
                }
            };
            direct_columns.push((col_idx, entry_idx));
        } else {
            let bus = label
                .parse::<u32>()
                .map_err(|_| ActivsgImportError::InvalidCsv {
                    path: path.to_path_buf(),
                    message: format!("invalid solar bus label `{label}`"),
                })?;
            let entry_idx = match solar_index_by_bus.get(&bus) {
                Some(&idx) => idx,
                None => {
                    let idx = solar_buses.len();
                    solar_index_by_bus.insert(bus, idx);
                    solar_buses.push(bus);
                    solar_series.push(Vec::new());
                    idx
                }
            };
            solar_columns.push((col_idx, entry_idx));
        }
    }

    let mut timestamps = Vec::new();
    for (row_offset, record_result) in reader.records().enumerate() {
        let row_number = row_offset + 3;
        let record = record_result?;
        let timestamp = parse_timestamp(path, row_number, &record)?;
        timestamps.push(timestamp);

        let mut direct_totals = vec![0.0; direct_series.len()];
        let mut solar_totals = vec![0.0; solar_series.len()];

        for (col_idx, series_idx) in &direct_columns {
            direct_totals[*series_idx] += parse_numeric_cell(path, row_number, &record, *col_idx)?;
        }
        for (col_idx, series_idx) in &solar_columns {
            solar_totals[*series_idx] += parse_numeric_cell(path, row_number, &record, *col_idx)?;
        }

        for (series, value) in direct_series.iter_mut().zip(direct_totals) {
            series.push(value);
        }
        for (series, value) in solar_series.iter_mut().zip(solar_totals) {
            series.push(value);
        }
    }

    if timestamps.is_empty() {
        return Err(ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: "renewable dataset contains no data rows".to_string(),
        });
    }

    Ok(ParsedRenewableSeries {
        path: path.to_path_buf(),
        timestamps,
        direct_by_key: direct_keys.into_iter().zip(direct_series).collect(),
        solar_by_bus_mw: solar_buses.into_iter().zip(solar_series).collect(),
    })
}

type WideCsvData<K> = (Vec<K>, Vec<NaiveDateTime>, Vec<Vec<f64>>);

fn read_wide_csv<K, F>(
    path: &Path,
    mut key_parser: F,
    data_offset: usize,
) -> Result<WideCsvData<K>, ActivsgImportError>
where
    K: Clone,
    F: FnMut(&Path, &str) -> Result<K, ActivsgImportError>,
{
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(path)?;

    let _row0 = next_record(&mut reader, path, 1)?;
    let header_row = next_record(&mut reader, path, 2)?;
    if header_row.len() < data_offset {
        return Err(ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("expected at least {data_offset} columns in header"),
        });
    }

    let mut keys = Vec::new();
    let mut columns = Vec::new();
    for col_idx in data_offset..header_row.len() {
        let label = header_row.get(col_idx).map(str::trim).unwrap_or_default();
        if label.is_empty() {
            continue;
        }
        keys.push(key_parser(path, label)?);
        columns.push((col_idx, keys.len() - 1));
    }

    let mut timestamps = Vec::new();
    let mut series = vec![Vec::new(); keys.len()];
    for (row_offset, record_result) in reader.records().enumerate() {
        let row_number = row_offset + 3;
        let record = record_result?;
        timestamps.push(parse_timestamp(path, row_number, &record)?);
        for (col_idx, series_idx) in &columns {
            let value = parse_numeric_cell(path, row_number, &record, *col_idx)?;
            series[*series_idx].push(value);
        }
    }

    if timestamps.is_empty() {
        return Err(ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: "dataset contains no data rows".to_string(),
        });
    }

    Ok((keys, timestamps, series))
}

fn build_ac_bus_load_profiles(
    periods: usize,
    p_by_bus: BTreeMap<u32, Vec<f64>>,
    q_by_bus: BTreeMap<u32, Vec<f64>>,
) -> AcBusLoadProfiles {
    let mut buses: Vec<u32> = p_by_bus
        .keys()
        .copied()
        .chain(q_by_bus.keys().copied())
        .collect();
    buses.sort_unstable();
    buses.dedup();

    let profiles = buses
        .into_iter()
        .map(|bus| AcBusLoadProfile {
            bus_number: bus,
            p_mw: Some(
                p_by_bus
                    .get(&bus)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; periods]),
            ),
            q_mvar: Some(
                q_by_bus
                    .get(&bus)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; periods]),
            ),
        })
        .collect();

    let _ = periods;
    AcBusLoadProfiles { profiles }
}

fn aggregate_columns_by_bus(
    buses_by_column: &[u32],
    periods: usize,
    values_by_column: Vec<Vec<f64>>,
) -> BTreeMap<u32, Vec<f64>> {
    let mut aggregated: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
    for (bus, series) in buses_by_column.iter().copied().zip(values_by_column) {
        let totals = aggregated.entry(bus).or_insert_with(|| vec![0.0; periods]);
        for (target, value) in totals.iter_mut().zip(series) {
            *target += value;
        }
    }
    aggregated
}

fn align_renewable_series(
    raw: ParsedRenewableSeries,
    target_timestamps: &[NaiveDateTime],
    policy: MissingTimestampPolicy,
) -> Result<(ParsedRenewableSeries, Vec<NaiveDateTime>), ActivsgImportError> {
    if raw.timestamps == target_timestamps {
        return Ok((raw, Vec::new()));
    }

    let mut raw_index_by_timestamp = HashMap::new();
    for (idx, timestamp) in raw.timestamps.iter().copied().enumerate() {
        raw_index_by_timestamp.insert(timestamp, idx);
    }

    let mut target_index_by_timestamp = HashMap::new();
    let mut alignment = Vec::with_capacity(target_timestamps.len());
    let mut inserted = Vec::new();
    let mut matched_raw = 0usize;

    for (target_idx, timestamp) in target_timestamps.iter().copied().enumerate() {
        let source = if let Some(&raw_idx) = raw_index_by_timestamp.get(&timestamp) {
            matched_raw += 1;
            AlignmentSource::Raw(raw_idx)
        } else {
            match policy {
                MissingTimestampPolicy::Error => {
                    return Err(ActivsgImportError::MissingTimestampFill {
                        path: raw.path.clone(),
                        timestamp,
                    });
                }
                MissingTimestampPolicy::RepeatPreviousDay => {
                    let source_timestamp = timestamp - Duration::days(1);
                    let source_target_idx = target_index_by_timestamp
                        .get(&source_timestamp)
                        .copied()
                        .ok_or_else(|| ActivsgImportError::MissingTimestampFill {
                            path: raw.path.clone(),
                            timestamp,
                        })?;
                    inserted.push(timestamp);
                    AlignmentSource::Aligned(source_target_idx)
                }
            }
        };
        alignment.push(source);
        target_index_by_timestamp.insert(timestamp, target_idx);
    }

    if matched_raw != raw.timestamps.len() {
        let unexpected = raw
            .timestamps
            .iter()
            .copied()
            .find(|timestamp| !target_index_by_timestamp.contains_key(timestamp))
            .expect("matched count guarantees at least one unexpected timestamp");
        return Err(ActivsgImportError::UnexpectedTimestamp {
            path: raw.path.clone(),
            timestamp: unexpected,
        });
    }

    Ok((
        ParsedRenewableSeries {
            path: raw.path,
            timestamps: target_timestamps.to_vec(),
            direct_by_key: align_series_map(raw.direct_by_key, &alignment),
            solar_by_bus_mw: align_series_map(raw.solar_by_bus_mw, &alignment),
        },
        inserted,
    ))
}

#[derive(Debug, Clone, Copy)]
enum AlignmentSource {
    Raw(usize),
    Aligned(usize),
}

fn apply_alignment(values: Vec<f64>, alignment: &[AlignmentSource]) -> Vec<f64> {
    let mut aligned = Vec::with_capacity(alignment.len());
    for source in alignment {
        match *source {
            AlignmentSource::Raw(idx) => aligned.push(values[idx]),
            AlignmentSource::Aligned(idx) => aligned.push(aligned[idx]),
        }
    }
    aligned
}

fn align_series_map<K: Ord>(
    series_map: BTreeMap<K, Vec<f64>>,
    alignment: &[AlignmentSource],
) -> BTreeMap<K, Vec<f64>> {
    series_map
        .into_iter()
        .map(|(key, values)| (key, apply_alignment(values, alignment)))
        .collect()
}

type RenewableBuildOutput = (
    RenewableProfiles,
    BTreeMap<String, f64>,
    usize,
    usize,
    usize,
    Vec<u32>,
    usize,
);

fn build_renewable_profiles(
    network: &Network,
    renewable: ParsedRenewableSeries,
    options: &ActivsgImportOptions,
) -> Result<RenewableBuildOutput, ActivsgImportError> {
    let mut generator_by_source_key: HashMap<(u32, String), (String, f64)> = HashMap::new();
    let mut generators_by_bus: HashMap<u32, Vec<(String, f64)>> = HashMap::new();
    for generator in &network.generators {
        let machine_id = machine_id_key(generator.machine_id.as_deref());
        generator_by_source_key.insert(
            (generator.bus, machine_id),
            (generator.id.clone(), generator.pmax),
        );
        generators_by_bus
            .entry(generator.bus)
            .or_default()
            .push((generator.id.clone(), generator.pmax));
    }
    for generators in generators_by_bus.values_mut() {
        generators.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let mut clipped_points = 0usize;
    let mut profiles = Vec::new();
    let mut direct_target_ids = HashSet::new();
    let mut generator_pmax_overrides = BTreeMap::new();

    for ((bus, machine_id), mw_series) in renewable.direct_by_key {
        let (generator_id, pmax) = generator_by_source_key
            .get(&(bus, machine_id.clone()))
            .cloned()
            .ok_or_else(|| ActivsgImportError::UnknownGenerator {
                path: renewable.path.clone(),
                bus,
                machine_id,
            })?;
        direct_target_ids.insert(generator_id.clone());
        let observed_max_mw = mw_series.iter().copied().fold(0.0_f64, f64::max);
        let adjusted_pmax = pmax.max(observed_max_mw);
        maybe_record_pmax_override(
            &mut generator_pmax_overrides,
            &generator_id,
            pmax,
            adjusted_pmax,
        );
        let capacity_factors = availability_to_capacity_factors(
            &generator_id,
            &mw_series,
            adjusted_pmax,
            options.capacity_factor_tolerance,
            &mut clipped_points,
        )?;
        profiles.push(RenewableProfile {
            resource_id: generator_id,
            capacity_factors,
        });
    }
    let direct_renewable_generators = profiles.len();

    let mut dropped_solar_buses = Vec::new();
    let mut solar_buses_aggregated = 0usize;
    let mut solar_generators_profiled = 0usize;

    for (bus, total_mw_series) in renewable.solar_by_bus_mw {
        let candidates: Vec<(String, f64)> = generators_by_bus
            .get(&bus)
            .into_iter()
            .flat_map(|generators| generators.iter())
            .filter(|(generator_id, _)| !direct_target_ids.contains(generator_id))
            .cloned()
            .collect();

        if candidates.is_empty() {
            match options.unmapped_solar_bus_policy {
                UnmappedSolarBusPolicy::Error => {
                    return Err(ActivsgImportError::NoSolarCandidates {
                        path: renewable.path.clone(),
                        bus,
                    });
                }
                UnmappedSolarBusPolicy::DropAndWarn => {
                    dropped_solar_buses.push(bus);
                    continue;
                }
            }
        }

        let total_pmax: f64 = candidates.iter().map(|(_, pmax)| *pmax).sum();
        if !(total_pmax.is_finite() && total_pmax > 0.0) {
            return Err(ActivsgImportError::NonPositivePmax {
                generator_id: format!("solar_bus_{bus}"),
                pmax: total_pmax,
            });
        }
        let observed_max_mw = total_mw_series.iter().copied().fold(0.0_f64, f64::max);
        let adjusted_total_pmax = total_pmax.max(observed_max_mw);
        let scale = adjusted_total_pmax / total_pmax;
        let bus_profile_id = format!("solar_bus_{bus}");
        let capacity_factors = availability_to_capacity_factors(
            &bus_profile_id,
            &total_mw_series,
            adjusted_total_pmax,
            options.capacity_factor_tolerance,
            &mut clipped_points,
        )?;

        solar_buses_aggregated += 1;
        solar_generators_profiled += candidates.len();
        for (generator_id, pmax) in candidates {
            maybe_record_pmax_override(
                &mut generator_pmax_overrides,
                &generator_id,
                pmax,
                pmax * scale,
            );
            profiles.push(RenewableProfile {
                resource_id: generator_id,
                capacity_factors: capacity_factors.clone(),
            });
        }
    }

    profiles.sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
    profiles.dedup_by(|a, b| a.resource_id == b.resource_id);

    Ok((
        RenewableProfiles { profiles },
        generator_pmax_overrides,
        direct_renewable_generators,
        solar_buses_aggregated,
        solar_generators_profiled,
        dropped_solar_buses,
        clipped_points,
    ))
}

fn availability_to_capacity_factors(
    generator_id: &str,
    mw_series: &[f64],
    pmax: f64,
    tolerance: f64,
    clipped_points: &mut usize,
) -> Result<Vec<f64>, ActivsgImportError> {
    if !(pmax.is_finite() && pmax > 0.0) {
        return Err(ActivsgImportError::NonPositivePmax {
            generator_id: generator_id.to_string(),
            pmax,
        });
    }

    let mut capacity_factors = Vec::with_capacity(mw_series.len());
    for (period, &mw) in mw_series.iter().enumerate() {
        let mut capacity_factor = if mw <= 0.0 { 0.0 } else { mw / pmax };
        if capacity_factor < -tolerance || capacity_factor > 1.0 + tolerance {
            return Err(ActivsgImportError::CapacityFactorOutOfRange {
                generator_id: generator_id.to_string(),
                period,
                mw,
                capacity_factor,
                tolerance,
            });
        }
        if !(0.0..=1.0).contains(&capacity_factor) {
            *clipped_points += 1;
            capacity_factor = capacity_factor.clamp(0.0, 1.0);
        }
        capacity_factors.push(capacity_factor);
    }
    Ok(capacity_factors)
}

fn maybe_record_pmax_override(
    overrides: &mut BTreeMap<String, f64>,
    generator_id: &str,
    current_pmax: f64,
    adjusted_pmax: f64,
) {
    if adjusted_pmax > current_pmax + DIRECT_CF_TOLERANCE_DEFAULT {
        overrides
            .entry(generator_id.to_string())
            .and_modify(|existing| *existing = existing.max(adjusted_pmax))
            .or_insert(adjusted_pmax);
    }
}

fn next_record<R: std::io::Read>(
    reader: &mut csv::Reader<R>,
    path: &Path,
    row_number: usize,
) -> Result<StringRecord, ActivsgImportError> {
    let mut record = StringRecord::new();
    if reader.read_record(&mut record)? {
        Ok(record)
    } else {
        Err(ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("missing row {row_number}"),
        })
    }
}

fn parse_load_header(path: &Path, label: &str) -> Result<u32, ActivsgImportError> {
    let parts: Vec<&str> = label.split_whitespace().collect();
    if parts.len() != 4 || parts[0] != "Bus" || !parts[2].starts_with('#') {
        return Err(ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("invalid load header `{label}`"),
        });
    }
    parts[1]
        .parse::<u32>()
        .map_err(|_| ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("invalid load bus in header `{label}`"),
        })
}

fn parse_generator_header(path: &Path, label: &str) -> Result<(u32, String), ActivsgImportError> {
    let parts: Vec<&str> = label.split_whitespace().collect();
    let machine_id = match parts.as_slice() {
        ["Gen", bus, machine_id, "MW"] | ["Gen", bus, machine_id, "Max", "MW"] => (
            (*bus).parse::<u32>(),
            machine_id.trim_start_matches('#').to_string(),
        ),
        _ => {
            return Err(ActivsgImportError::InvalidCsv {
                path: path.to_path_buf(),
                message: format!("invalid generator header `{label}`"),
            });
        }
    };
    let (bus, machine_id) = machine_id;
    Ok((
        bus.map_err(|_| ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("invalid generator bus in header `{label}`"),
        })?,
        machine_id,
    ))
}

fn parse_timestamp(
    path: &Path,
    row_number: usize,
    record: &StringRecord,
) -> Result<NaiveDateTime, ActivsgImportError> {
    let date = record.get(0).map(str::trim).unwrap_or_default();
    let time = record.get(1).map(str::trim).unwrap_or_default();
    NaiveDateTime::parse_from_str(&format!("{date} {time}"), TIMESTAMP_FORMAT).map_err(|_| {
        ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("invalid timestamp `{date} {time}` at row {row_number}"),
        }
    })
}

fn parse_numeric_cell(
    path: &Path,
    row_number: usize,
    record: &StringRecord,
    col_idx: usize,
) -> Result<f64, ActivsgImportError> {
    let raw = record.get(col_idx).map(str::trim).unwrap_or_default();
    raw.parse::<f64>()
        .map_err(|_| ActivsgImportError::InvalidCsv {
            path: path.to_path_buf(),
            message: format!("invalid numeric value `{raw}` at row {row_number}, column {col_idx}"),
        })
}

fn machine_id_key(machine_id: Option<&str>) -> String {
    machine_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("1")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::request::{BusLoadProfile, BusLoadProfiles, RenewableProfile, RenewableProfiles};
    use crate::{
        CommitmentPolicy, DispatchModel, DispatchNetwork, DispatchProfiles, DispatchRequest,
        DispatchTimeline, FlowgatePolicy, Formulation, IntervalCoupling, ThermalLimitPolicy,
        solve_dispatch,
    };
    use chrono::Datelike;
    use surge_network::market::CostCurve;
    use surge_network::network::{Generator, Load};

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("surge-activsg-{name}-{nanos}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write test csv");
    }

    fn generator(bus: u32, machine_id: &str, pmax: f64) -> Generator {
        let mut generator = Generator {
            bus,
            machine_id: Some(machine_id.to_string()),
            pmax,
            ..Generator::default()
        };
        generator.id = format!("gen_{bus}_{machine_id}");
        generator
    }

    fn build_test_network() -> Network {
        let mut network = Network::default();
        network.buses = vec![
            surge_network::network::Bus {
                number: 1001,
                ..Default::default()
            },
            surge_network::network::Bus {
                number: 1002,
                ..Default::default()
            },
            surge_network::network::Bus {
                number: 1011,
                ..Default::default()
            },
            surge_network::network::Bus {
                number: 1062,
                ..Default::default()
            },
            surge_network::network::Bus {
                number: 10691,
                ..Default::default()
            },
            surge_network::network::Bus {
                number: 20463,
                ..Default::default()
            },
            surge_network::network::Bus {
                number: 74214,
                ..Default::default()
            },
        ];
        network.loads = vec![Load::new(1001, 10.0, 2.0), Load::new(1002, 20.0, 4.0)];
        network.loads[0].id = "1".to_string();
        network.loads[1].id = "1".to_string();
        network.generators = vec![
            generator(1011, "1", 10.0),
            generator(1062, "1", 20.0),
            generator(10691, "1", 100.0),
            generator(20463, "1", 50.0),
            generator(20463, "2", 50.0),
            generator(74214, "1", 40.0),
            generator(74214, "2", 30.0),
            generator(74214, "3", 30.0),
        ];
        network
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn activsg_case_path(case: ActivsgCase) -> PathBuf {
        match case {
            ActivsgCase::Activsg2000 => {
                repo_root().join("examples/cases/case_ACTIVSg2000/case_ACTIVSg2000.surge.json.zst")
            }
            ActivsgCase::Activsg10k => {
                repo_root().join("examples/cases/case_ACTIVSg10k/case_ACTIVSg10k.surge.json.zst")
            }
        }
    }

    fn activsg_time_series_root() -> PathBuf {
        if let Ok(path) = std::env::var("SURGE_ACTIVSG_TIME_SERIES") {
            return PathBuf::from(path);
        }
        repo_root().join("research/test-cases/data/ACTIVSg_Time_Series")
    }

    fn activsg_data_available() -> bool {
        let root = activsg_time_series_root();
        root.exists()
            && activsg_case_path(ActivsgCase::Activsg2000).exists()
            && activsg_case_path(ActivsgCase::Activsg10k).exists()
    }

    fn first_n_dc_profiles(imported: &ActivsgTimeSeries, periods: usize) -> DispatchProfiles {
        assert!(
            imported.periods() >= periods,
            "expected at least {periods} periods, got {}",
            imported.periods()
        );
        DispatchProfiles {
            load: BusLoadProfiles {
                profiles: imported
                    .load_profiles()
                    .profiles
                    .into_iter()
                    .map(|profile| BusLoadProfile {
                        bus_number: profile.bus_number,
                        values_mw: profile.values_mw[..periods].to_vec(),
                    })
                    .collect(),
            },
            renewable: RenewableProfiles {
                profiles: imported
                    .renewable_profiles
                    .profiles
                    .iter()
                    .map(|profile| RenewableProfile {
                        resource_id: profile.resource_id.clone(),
                        capacity_factors: profile.capacity_factors[..periods].to_vec(),
                    })
                    .collect(),
            },
            ..DispatchProfiles::default()
        }
    }

    fn assign_smoke_test_costs(network: &mut Network) {
        for (idx, generator) in network.generators.iter_mut().enumerate() {
            if generator.in_service {
                generator.cost = Some(CostCurve::Polynomial {
                    startup: 0.0,
                    shutdown: 0.0,
                    coeffs: vec![10.0 + idx as f64 * 1e-3, 0.0],
                });
            }
        }
    }

    fn run_activsg2000_dc_dispatch_smoke(periods: usize) {
        let network = surge_io::load(activsg_case_path(ActivsgCase::Activsg2000))
            .expect("load refreshed ACTIVSg2000 case");
        let imported = read_tamu_activsg_time_series(
            &network,
            activsg_time_series_root(),
            ActivsgCase::Activsg2000,
            &ActivsgImportOptions::default(),
        )
        .expect("import ACTIVSg2000 time series");
        let mut adjusted_network = imported
            .network_with_nameplate_overrides(&network)
            .expect("apply renewable nameplate overrides");
        assign_smoke_test_costs(&mut adjusted_network);
        let request = DispatchRequest::builder()
            .formulation(Formulation::Dc)
            .coupling(IntervalCoupling::PeriodByPeriod)
            .commitment(CommitmentPolicy::AllCommitted)
            .timeline(DispatchTimeline {
                periods,
                interval_hours: 1.0,
                interval_hours_by_period: Vec::new(),
            })
            .profiles(first_n_dc_profiles(&imported, periods))
            .network(DispatchNetwork {
                thermal_limits: ThermalLimitPolicy {
                    enforce: false,
                    ..ThermalLimitPolicy::default()
                },
                flowgates: FlowgatePolicy {
                    enabled: false,
                    ..FlowgatePolicy::default()
                },
                ..DispatchNetwork::default()
            })
            .build();

        let model =
            DispatchModel::prepare(&adjusted_network).expect("prepare ACTIVSg2000 dispatch model");
        let solution = solve_dispatch(&model, &request).expect("solve ACTIVSg2000 DC dispatch");

        assert_eq!(solution.study().periods, periods);
        assert_eq!(solution.periods().len(), periods);
        assert!(solution.summary().total_cost.is_finite());
        assert!(solution.summary().total_cost > 0.0);
        assert_eq!(
            solution.periods()[0].bus_results().len(),
            adjusted_network.buses.len()
        );
        assert!(
            solution
                .periods()
                .iter()
                .all(|period| period.total_cost().is_finite() && period.total_cost() >= 0.0)
        );
        let period0_withdrawals: f64 = solution.periods()[0]
            .bus_results()
            .iter()
            .map(|bus| bus.withdrawals_mw)
            .sum();
        assert!(period0_withdrawals > 0.0);
        assert_eq!(
            imported.report.generator_pmax_overrides,
            imported.generator_pmax_overrides.len()
        );
    }

    #[test]
    fn imports_2000_style_direct_profiles() {
        let dir = unique_test_dir("case2000");
        write_file(
            &dir.join("ACTIVISg2000_load_time_series_MW.csv"),
            "PWOPFTimePoint,,,,,,\nDate,Time,Num Load,Total MW Load,Total Mvar Load,Bus 1001 #1 MW,Bus 1002 #1 MW\n1/1/2016,12:00:00 AM,2,30,0,10,20\n1/1/2016,1:00:00 AM,2,32,0,11,21\n",
        );
        write_file(
            &dir.join("ACTIVISg2000_load_time_series_MVAR.csv"),
            "PWOPFTimePoint,,,,,,\nDate,Time,Num Load,Total MW Load,Total Mvar Load,Bus 1001 #1 MVAR,Bus 1002 #1 MVAR\n1/1/2016,12:00:00 AM,2,30,6,2,4\n1/1/2016,1:00:00 AM,2,32,7,3,4\n",
        );
        write_file(
            &dir.join("ACTIVISg2000_renewable_time_series_MW.csv"),
            "PWOPFTimePoint,,,,,Solar,Wind\nDate,Time,Num Renewable,Total solar Gen,Total wind Gen,Gen 1011 #1 MW,Gen 1062 #1 MW\n1/1/2016,12:00:00 AM,2,5,10,5,10\n1/1/2016,1:00:00 AM,2,6,12,6,12\n",
        );

        let network = build_test_network();
        let imported = read_tamu_activsg_time_series(
            &network,
            &dir,
            ActivsgCase::Activsg2000,
            &ActivsgImportOptions::default(),
        )
        .expect("import 2000 dataset");

        assert_eq!(imported.periods(), 2);
        assert_eq!(imported.ac_bus_load_profiles.profiles.len(), 2);
        assert_eq!(
            imported.ac_bus_load_profiles.profiles[0]
                .p_mw
                .as_ref()
                .unwrap(),
            &vec![10.0, 11.0]
        );
        assert_eq!(
            imported.ac_bus_load_profiles.profiles[0]
                .q_mvar
                .as_ref()
                .unwrap(),
            &vec![2.0, 3.0]
        );

        let renewable = &imported.renewable_profiles.profiles;
        assert_eq!(renewable.len(), 2);
        assert_eq!(renewable[0].resource_id, "gen_1011_1");
        assert_eq!(renewable[0].capacity_factors, vec![0.5, 0.6]);
        assert_eq!(renewable[1].resource_id, "gen_1062_1");
        assert_eq!(renewable[1].capacity_factors, vec![0.5, 0.6]);
        assert!(imported.generator_pmax_overrides.is_empty());

        let dc_profiles = imported.dc_dispatch_profiles();
        assert_eq!(dc_profiles.load.profiles.len(), 2);
        assert_eq!(dc_profiles.load.profiles[0].values_mw.len(), 2);
    }

    #[test]
    fn imports_10k_bus_only_solar_by_aggregating_per_bus() {
        let dir = unique_test_dir("case10k");
        write_file(
            &dir.join("ACTIVSg10k_load_time_series_MW.csv"),
            "PWOPFTimePoint,,,,,,\nDate,Time,Num Load,Total MW Load,Total Mvar Load,Bus 1001 #1 MW,Bus 1002 #1 MW\n1/1/2016,12:00:00 AM,2,30,0,10,20\n1/1/2016,1:00:00 AM,2,30,0,12,18\n",
        );
        write_file(
            &dir.join("ACTIVSg10k_load_time_series_MVAR.csv"),
            "PWOPFTimePoint,,,,,,\nDate,Time,Num Load,Total MW Load,Total Mvar Load,Bus 1001 #1 MVAR,Bus 1002 #1 MVAR\n1/1/2016,12:00:00 AM,2,6,0,2,4\n1/1/2016,1:00:00 AM,2,5,0,2,3\n",
        );
        write_file(
            &dir.join("ACTIVISg10k_renewable_time_series_MW.csv"),
            "PWOPFTimePoint,,,,,Wind,Solar,Solar,Solar,Solar,Solar\nDate,Time,Num Renewable,Total wind Gen,Total solar Gen,Gen 10691 #1 Max MW,20463,20463,20463,74214,74214\n1/1/2016,12:00:00 AM,6,30,35,30,10,5,5,7,8\n1/1/2016,1:00:00 AM,6,40,70,40,20,10,10,15,15\n",
        );

        let network = build_test_network();
        let imported = read_tamu_activsg_time_series(
            &network,
            &dir,
            ActivsgCase::Activsg10k,
            &ActivsgImportOptions::default(),
        )
        .expect("import 10k dataset");

        let renewable: BTreeMap<_, _> = imported
            .renewable_profiles
            .profiles
            .iter()
            .map(|profile| {
                (
                    profile.resource_id.clone(),
                    profile.capacity_factors.clone(),
                )
            })
            .collect();

        assert_eq!(renewable["gen_10691_1"], vec![0.3, 0.4]);
        assert_eq!(renewable["gen_20463_1"], vec![0.2, 0.4]);
        assert_eq!(renewable["gen_20463_2"], vec![0.2, 0.4]);
        assert_eq!(renewable["gen_74214_1"], vec![0.15, 0.3]);
        assert_eq!(renewable["gen_74214_2"], vec![0.15, 0.3]);
        assert_eq!(renewable["gen_74214_3"], vec![0.15, 0.3]);
        assert_eq!(imported.report.solar_buses_aggregated, 2);
        assert_eq!(imported.report.solar_generators_profiled, 5);
        assert!(imported.generator_pmax_overrides.is_empty());
    }

    #[test]
    fn direct_renewable_observed_max_can_raise_nameplate() {
        let dir = unique_test_dir("override");
        write_file(
            &dir.join("ACTIVISg2000_load_time_series_MW.csv"),
            "PWOPFTimePoint,,,,,,\nDate,Time,Num Load,Total MW Load,Total Mvar Load,Bus 1011 #1 MW\n1/1/2016,12:00:00 AM,1,0,0,0\n1/1/2016,1:00:00 AM,1,0,0,0\n",
        );
        write_file(
            &dir.join("ACTIVISg2000_load_time_series_MVAR.csv"),
            "PWOPFTimePoint,,,,,,\nDate,Time,Num Load,Total MW Load,Total Mvar Load,Bus 1011 #1 MVAR\n1/1/2016,12:00:00 AM,1,0,0,0\n1/1/2016,1:00:00 AM,1,0,0,0\n",
        );
        write_file(
            &dir.join("ACTIVISg2000_renewable_time_series_MW.csv"),
            "PWOPFTimePoint,,,,,Wind\nDate,Time,Num Renewable,Total solar Gen,Total wind Gen,Gen 1011 #1 MW\n1/1/2016,12:00:00 AM,1,0,6,6\n1/1/2016,1:00:00 AM,1,0,3,3\n",
        );

        let mut network = Network::default();
        network.buses = vec![surge_network::network::Bus {
            number: 1011,
            ..Default::default()
        }];
        network.generators = vec![generator(1011, "1", 5.0)];

        let imported = read_tamu_activsg_time_series(
            &network,
            &dir,
            ActivsgCase::Activsg2000,
            &ActivsgImportOptions::default(),
        )
        .expect("import dataset with override");

        assert_eq!(
            imported.generator_pmax_overrides.get("gen_1011_1").copied(),
            Some(6.0)
        );
        assert_eq!(
            imported.renewable_profiles.profiles[0].capacity_factors,
            vec![1.0, 0.5]
        );

        let adjusted = imported
            .network_with_nameplate_overrides(&network)
            .expect("apply overrides");
        assert_eq!(adjusted.generators[0].pmax, 6.0);
    }

    #[test]
    fn repeat_previous_day_alignment_fills_missing_leap_day_hours() {
        let path = PathBuf::from("renewable.csv");
        let target_timestamps: Vec<NaiveDateTime> = (0..49)
            .map(|hour| {
                NaiveDateTime::parse_from_str("02/28/2016 12:00:00 AM", TIMESTAMP_FORMAT)
                    .expect("parse base")
                    + Duration::hours(hour)
            })
            .collect();
        let raw_timestamps: Vec<NaiveDateTime> = target_timestamps
            .iter()
            .copied()
            .filter(|timestamp| timestamp.date().day() != 29)
            .collect();
        let raw = ParsedRenewableSeries {
            path: path.clone(),
            timestamps: raw_timestamps,
            direct_by_key: BTreeMap::from([(
                (10691, "1".to_string()),
                (0..25).map(|value| value as f64).collect(),
            )]),
            solar_by_bus_mw: BTreeMap::new(),
        };

        let (aligned, inserted) = align_renewable_series(
            raw,
            &target_timestamps,
            MissingTimestampPolicy::RepeatPreviousDay,
        )
        .expect("align leap day");

        let values = aligned
            .direct_by_key
            .get(&(10691, "1".to_string()))
            .expect("aligned values");
        assert_eq!(values.len(), 49);
        assert_eq!(inserted.len(), 24);
        assert_eq!(values[24], values[0]);
        assert_eq!(values[30], values[6]);
        assert_eq!(values[48], 24.0);
    }

    #[test]
    fn activsg2000_single_interval_dc_dispatch_smoke() {
        if !activsg_data_available() {
            eprintln!(
                "SKIP: ACTIVSg time-series data not present; set SURGE_ACTIVSG_TIME_SERIES or populate research/test-cases/data/ACTIVSg_Time_Series/"
            );
            return;
        }
        run_activsg2000_dc_dispatch_smoke(1);
    }

    #[test]
    fn activsg2000_two_interval_dc_dispatch_smoke() {
        if !activsg_data_available() {
            eprintln!(
                "SKIP: ACTIVSg time-series data not present; set SURGE_ACTIVSG_TIME_SERIES or populate research/test-cases/data/ACTIVSg_Time_Series/"
            );
            return;
        }
        run_activsg2000_dc_dispatch_smoke(2);
    }
}
