// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Supplemental bus coordinate parser.
//!
//! Reads a CSV file with columns `bus_number,latitude,longitude` and
//! populates the geographic fields on an existing `Network`'s buses.

use std::collections::HashMap;
use std::path::Path;

use surge_network::Network;

/// Parse a bus coordinate CSV and apply lat/lon to matching buses in the network.
///
/// CSV format: `bus_number,latitude,longitude` (with optional header row).
/// Lines where the first field cannot be parsed as u32 are treated as headers.
///
/// Returns the number of buses that were updated.
pub fn apply_bus_coordinates(network: &mut Network, csv_path: &Path) -> Result<usize, Error> {
    let content = std::fs::read_to_string(csv_path)
        .map_err(|e| Error::Io(csv_path.display().to_string(), e))?;

    let bus_map: HashMap<u32, usize> = network.bus_index_map();
    let mut updated = 0;

    for (line_no, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 3 {
            continue;
        }

        // Skip header row (first field not a number)
        let bus_number: u32 = match parts[0].trim().parse() {
            Ok(n) => n,
            Err(_) => continue, // likely header
        };

        let latitude: f64 = parts[1].trim().parse().map_err(|_| {
            Error::ParseError(line_no + 1, "latitude".into(), parts[1].trim().into())
        })?;
        let longitude: f64 = parts[2].trim().parse().map_err(|_| {
            Error::ParseError(line_no + 1, "longitude".into(), parts[2].trim().into())
        })?;

        if let Some(&idx) = bus_map.get(&bus_number) {
            network.buses[idx].latitude = Some(latitude);
            network.buses[idx].longitude = Some(longitude);
            updated += 1;
        }
    }

    Ok(updated)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read coordinate file '{0}': {1}")]
    Io(String, std::io::Error),

    #[error("line {0}: failed to parse {1}: '{2}'")]
    ParseError(usize, String, String),
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
    #[allow(dead_code)]
    fn test_data_dir() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
    }

    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn load_case9() -> Network {
        let path = test_data_dir().join("case9.m");
        crate::matpower::load(&path).unwrap()
    }

    #[test]
    fn test_parse_bus_coordinates_csv() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let mut net = load_case9();
        assert!(net.buses[0].latitude.is_none());

        // Write a temp CSV
        let mut tmpfile = NamedTempFile::new().unwrap();
        writeln!(tmpfile, "bus_number,latitude,longitude").unwrap();
        writeln!(tmpfile, "1,30.25,-97.75").unwrap();
        writeln!(tmpfile, "2,31.50,-96.80").unwrap();
        writeln!(tmpfile, "999,40.0,-80.0").unwrap(); // non-existent bus
        tmpfile.flush().unwrap();

        let updated = apply_bus_coordinates(&mut net, tmpfile.path()).unwrap();
        assert_eq!(updated, 2);
        assert_eq!(net.buses[0].latitude, Some(30.25));
        assert_eq!(net.buses[0].longitude, Some(-97.75));

        // Bus 999 should not have matched
        for bus in &net.buses {
            if bus.number == 999 {
                panic!("bus 999 should not exist in case9");
            }
        }
    }

    #[test]
    fn test_bus_geo_serialization_roundtrip() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let mut net = load_case9();
        net.buses[0].latitude = Some(30.25);
        net.buses[0].longitude = Some(-97.75);

        // Serialize to JSON
        let json = serde_json::to_string(&net.buses[0]).unwrap();
        assert!(json.contains("\"latitude\":30.25"));
        assert!(json.contains("\"longitude\":-97.75"));

        // Deserialize back
        let bus: surge_network::network::Bus = serde_json::from_str(&json).unwrap();
        assert_eq!(bus.latitude, Some(30.25));
        assert_eq!(bus.longitude, Some(-97.75));

        // Bus without coordinates should not have lat/lon in JSON
        let json2 = serde_json::to_string(&net.buses[1]).unwrap();
        assert!(!json2.contains("latitude"));
        assert!(!json2.contains("longitude"));
    }
}
