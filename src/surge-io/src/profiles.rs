// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical time-series profile readers.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use surge_network::market::{LoadProfile, LoadProfiles, RenewableProfile, RenewableProfiles};
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum ProfileIoError {
    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("missing required CSV column `{column}`")]
    MissingColumn { column: &'static str },

    #[error("{dataset} CSV contains no rows")]
    EmptyCsv { dataset: &'static str },

    #[error("duplicate {dataset} row for `{key}` at period {period}")]
    DuplicatePoint {
        dataset: &'static str,
        key: String,
        period: usize,
    },

    #[error("missing {dataset} value for `{key}` at period {period}")]
    MissingPoint {
        dataset: &'static str,
        key: String,
        period: usize,
    },

    #[error("invalid capacity factor {value} for `{key}` at period {period}; expected 0.0..=1.0")]
    InvalidCapacityFactor {
        key: String,
        period: usize,
        value: f64,
    },

    #[error("invalid data: {message}")]
    InvalidData { message: String },
}

#[derive(Debug, Deserialize)]
struct LoadRow {
    hour: usize,
    bus: u32,
    load_mw: f64,
}

#[derive(Debug, Deserialize)]
struct RenewableRow {
    hour: usize,
    generator_id: String,
    capacity_factor: f64,
}

/// Read long-format load profiles from CSV.
///
/// Expected columns: `hour,bus,load_mw`
pub fn read_load_profiles_csv(path: &Path) -> Result<LoadProfiles, ProfileIoError> {
    info!(path = %path.display(), "read_load_profiles_csv: reading load profiles");
    let mut reader = csv::Reader::from_path(path)?;
    let headers = reader.headers()?.clone();
    require_columns(&headers, &["hour", "bus", "load_mw"])?;

    let mut bus_data: HashMap<u32, HashMap<usize, f64>> = HashMap::new();
    let mut max_hour: Option<usize> = None;

    for result in reader.deserialize() {
        let row: LoadRow = result?;
        let previous = bus_data
            .entry(row.bus)
            .or_default()
            .insert(row.hour, row.load_mw);
        if previous.is_some() {
            return Err(ProfileIoError::DuplicatePoint {
                dataset: "load profile",
                key: format!("bus {}", row.bus),
                period: row.hour,
            });
        }
        max_hour = Some(max_hour.map_or(row.hour, |max_hour| max_hour.max(row.hour)));
    }

    let n_timesteps = max_hour
        .map(|hour| hour + 1)
        .ok_or(ProfileIoError::EmptyCsv {
            dataset: "load profile",
        })?;

    let mut buses: Vec<u32> = bus_data.keys().copied().collect();
    buses.sort_unstable();

    let mut profiles = Vec::with_capacity(buses.len());
    for bus in buses {
        let hour_map = bus_data
            .remove(&bus)
            .expect("bus key collected from existing map");
        for hour in 0..n_timesteps {
            if !hour_map.contains_key(&hour) {
                return Err(ProfileIoError::MissingPoint {
                    dataset: "load profile",
                    key: format!("bus {}", bus),
                    period: hour,
                });
            }
        }
        let mut load_mw = vec![0.0; n_timesteps];
        for (hour, value) in hour_map {
            load_mw[hour] = value;
        }
        profiles.push(LoadProfile { bus, load_mw });
    }

    info!(
        n_timesteps = n_timesteps,
        n_buses = profiles.len(),
        "read_load_profiles_csv: loaded successfully"
    );
    Ok(LoadProfiles {
        profiles,
        n_timesteps,
    })
}

/// Read long-format renewable capacity factor profiles from CSV.
///
/// Expected columns: `hour,generator_id,capacity_factor`
pub fn read_renewable_profiles_csv(path: &Path) -> Result<RenewableProfiles, ProfileIoError> {
    info!(
        path = %path.display(),
        "read_renewable_profiles_csv: reading renewable profiles"
    );
    let mut reader = csv::Reader::from_path(path)?;
    let headers = reader.headers()?.clone();
    require_columns(&headers, &["hour", "generator_id", "capacity_factor"])?;

    let mut generator_data: HashMap<String, HashMap<usize, f64>> = HashMap::new();
    let mut max_hour: Option<usize> = None;

    for result in reader.deserialize() {
        let row: RenewableRow = result?;
        let key = row.generator_id.trim().to_string();
        if key.is_empty() {
            return Err(ProfileIoError::InvalidData {
                message: format!(
                    "renewable profile row at period {} has empty generator_id",
                    row.hour
                ),
            });
        }
        if !(0.0..=1.0).contains(&row.capacity_factor) {
            return Err(ProfileIoError::InvalidCapacityFactor {
                key: format!("generator {key}"),
                period: row.hour,
                value: row.capacity_factor,
            });
        }
        let previous = generator_data
            .entry(key.clone())
            .or_default()
            .insert(row.hour, row.capacity_factor);
        if previous.is_some() {
            return Err(ProfileIoError::DuplicatePoint {
                dataset: "renewable profile",
                key: format!("generator {key}"),
                period: row.hour,
            });
        }
        max_hour = Some(max_hour.map_or(row.hour, |max_hour| max_hour.max(row.hour)));
    }

    let n_timesteps = max_hour
        .map(|hour| hour + 1)
        .ok_or(ProfileIoError::EmptyCsv {
            dataset: "renewable profile",
        })?;

    let mut keys: Vec<String> = generator_data.keys().cloned().collect();
    keys.sort();

    let mut profiles = Vec::with_capacity(keys.len());
    for key in keys {
        let hour_map = generator_data
            .remove(&key)
            .expect("generator key collected from existing map");
        for hour in 0..n_timesteps {
            if !hour_map.contains_key(&hour) {
                return Err(ProfileIoError::MissingPoint {
                    dataset: "renewable profile",
                    key: format!("generator {key}"),
                    period: hour,
                });
            }
        }
        let mut capacity_factors = vec![0.0; n_timesteps];
        for (hour, value) in hour_map {
            capacity_factors[hour] = value;
        }
        profiles.push(RenewableProfile {
            generator_id: key,
            capacity_factors,
        });
    }

    info!(
        n_timesteps = n_timesteps,
        n_generators = profiles.len(),
        "read_renewable_profiles_csv: loaded successfully"
    );
    Ok(RenewableProfiles {
        profiles,
        n_timesteps,
    })
}

fn require_columns(
    headers: &csv::StringRecord,
    required: &[&'static str],
) -> Result<(), ProfileIoError> {
    for column in required {
        if !headers.iter().any(|header| header == *column) {
            return Err(ProfileIoError::MissingColumn { column });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn data_available() -> bool {
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

    use super::*;

    #[allow(dead_code)]
    fn test_data_path(name: &str) -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p).join(name);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .join(name)
    }

    #[test]
    fn test_read_load_profiles_csv() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let profiles = read_load_profiles_csv(&test_data_path("rts96/load_24h.csv")).unwrap();

        assert_eq!(profiles.n_timesteps, 24);
        assert!(!profiles.profiles.is_empty());

        let bus101 = profiles
            .profiles
            .iter()
            .find(|p| p.bus == 101)
            .expect("should have bus 101");
        assert_eq!(bus101.load_mw.len(), 24);
        assert!((bus101.load_mw[0] - 108.0).abs() < 1e-6);
        assert!((bus101.load_mw[9] - 121.0).abs() < 1e-6);
    }

    #[test]
    fn test_read_renewable_profiles_csv() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let profiles =
            read_renewable_profiles_csv(&test_data_path("rts96/renewable_24h.csv")).unwrap();

        assert_eq!(profiles.n_timesteps, 24);
        assert_eq!(profiles.profiles.len(), 2);

        let solar = profiles
            .profiles
            .iter()
            .find(|p| p.generator_id == "gen_101")
            .expect("should have solar profile for gen_101");
        assert!((solar.capacity_factors[0] - 0.0).abs() < 1e-6);
        assert!((solar.capacity_factors[12] - 0.95).abs() < 1e-6);
    }
}
