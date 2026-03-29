// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! COMTRADE (IEEE C37.111) parser and writer for Surge.
//!
//! Supports reading and writing oscillography/waveform data from digital fault
//! recorders, protective relays, and power quality monitors in all three
//! standard revisions (1991, 1999, 2013).
//!
//! ## File types
//!
//! | Extension | Content                                  | Support |
//! |-----------|------------------------------------------|---------|
//! | `.cfg`    | Configuration — channels, rates, times   | R/W     |
//! | `.dat`    | Sample data (ASCII, BINARY, BINARY32, FLOAT32) | R/W |
//! | `.hdr`    | Free-form header text                    | R/W     |
//! | `.inf`    | Additional metadata (key=value)           | R/W     |
//! | `.cff`    | Combined File Format (2013)              | R/W     |
//!
//! ## Usage
//!
//! ```no_run
//! use surge_io::comtrade::{parse_comtrade, parse_comtrade_cff, write_comtrade, write_comtrade_cff};
//! use std::path::Path;
//!
//! // Parse from .cfg (auto-discovers .dat/.hdr/.inf alongside)
//! let record = parse_comtrade(Path::new("fault_record.cfg")).unwrap();
//! println!("{} analog channels, {} samples", record.n_analog(), record.n_samples());
//!
//! // Write back
//! write_comtrade(&record, Path::new("output.cfg")).unwrap();
//! ```

mod cff;
mod cfg;
mod dat;
mod types;
mod writer;

pub use types::{
    AnalogChannel, ComtradeRecord, ComtradeTimestamp, DataFormat, DigitalChannel, RevYear, Sample,
    SampleRate, ScalingFlag,
};
pub use writer::{
    to_cfg_string, to_dat_ascii_string, to_dat_binary, write_comtrade, write_comtrade_cff,
};

use std::path::{Path, PathBuf};

/// Errors from COMTRADE parsing or writing.
#[derive(Debug, thiserror::Error)]
pub enum ComtradeError {
    #[error("I/O error: {0}")]
    Io(String),

    #[error(".cfg file too short ({0} lines, need at least 4)")]
    CfgTooShort(usize),

    #[error("bad revision year: '{0}' (expected 1991, 1999, or 2013)")]
    BadRevYear(String),

    #[error("bad channel count line: '{0}'")]
    BadChannelLine(String),

    #[error("bad analog channel definition: '{0}'")]
    BadAnalogChannel(String),

    #[error("bad digital channel definition: '{0}'")]
    BadDigitalChannel(String),

    #[error("bad data format: '{0}' (expected ASCII, BINARY, BINARY32, or FLOAT32)")]
    BadDataFormat(String),

    #[error("bad timestamp: '{0}'")]
    BadTimestamp(String),

    #[error("unexpected end of file while reading {0}")]
    UnexpectedEof(&'static str),

    #[error("bad float value for {0}: '{1}'")]
    BadFloat(&'static str, String),

    #[error("bad integer value for {0}: '{1}'")]
    BadInt(&'static str, String),

    #[error(".dat line {line}: expected {expected} fields, got {got}")]
    BadDatLine {
        line: usize,
        expected: usize,
        got: usize,
    },

    #[error("binary .dat size mismatch: {total} bytes not divisible by record size {record_size}")]
    BadBinarySize { total: usize, record_size: usize },

    #[error("CFF file missing required section: {0}")]
    CffMissingSection(&'static str),

    #[error("CFF with binary dat is not supported (binary data cannot be embedded in text CFF)")]
    CffBinaryNotSupported,
}

/// Parse a COMTRADE record from a .cfg file path.
///
/// Auto-discovers the companion .dat file (same stem, case-insensitive extension).
/// Also reads .hdr and .inf files if present alongside.
pub fn parse_comtrade(cfg_path: &Path) -> Result<ComtradeRecord, ComtradeError> {
    let cfg_text = std::fs::read_to_string(cfg_path)
        .map_err(|e| ComtradeError::Io(format!("{}: {e}", cfg_path.display())))?;

    let cfg_data = cfg::parse_cfg(&cfg_text)?;

    let stem = cfg_path.with_extension("");

    // Read .dat file (case-insensitive extension search)
    let dat_path = find_companion(&stem, "dat").ok_or_else(|| {
        ComtradeError::Io(format!("cannot find .dat file for {}", cfg_path.display()))
    })?;

    let samples = match cfg_data.data_format {
        DataFormat::Ascii => {
            let dat_text = std::fs::read_to_string(&dat_path)
                .map_err(|e| ComtradeError::Io(format!("{}: {e}", dat_path.display())))?;
            dat::parse_dat_ascii(
                &dat_text,
                &cfg_data.analog_channels,
                &cfg_data.digital_channels,
            )?
        }
        DataFormat::Binary16 => {
            let dat_bytes = std::fs::read(&dat_path)
                .map_err(|e| ComtradeError::Io(format!("{}: {e}", dat_path.display())))?;
            dat::parse_dat_binary16(
                &dat_bytes,
                &cfg_data.analog_channels,
                &cfg_data.digital_channels,
            )?
        }
        DataFormat::Binary32 => {
            let dat_bytes = std::fs::read(&dat_path)
                .map_err(|e| ComtradeError::Io(format!("{}: {e}", dat_path.display())))?;
            dat::parse_dat_binary32(
                &dat_bytes,
                &cfg_data.analog_channels,
                &cfg_data.digital_channels,
            )?
        }
        DataFormat::Float32 => {
            let dat_bytes = std::fs::read(&dat_path)
                .map_err(|e| ComtradeError::Io(format!("{}: {e}", dat_path.display())))?;
            dat::parse_dat_float32(
                &dat_bytes,
                &cfg_data.analog_channels,
                &cfg_data.digital_channels,
            )?
        }
    };

    // Read optional .hdr (case-insensitive)
    let header_text = find_companion(&stem, "hdr")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });

    // Read optional .inf (case-insensitive)
    let info = find_companion(&stem, "inf")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| cff::parse_inf(&s));

    Ok(ComtradeRecord {
        station_name: cfg_data.station_name,
        rec_dev_id: cfg_data.rec_dev_id,
        rev_year: cfg_data.rev_year,
        frequency: cfg_data.frequency,
        start_time: cfg_data.start_time,
        trigger_time: cfg_data.trigger_time,
        analog_channels: cfg_data.analog_channels,
        digital_channels: cfg_data.digital_channels,
        sample_rates: cfg_data.sample_rates,
        data_format: cfg_data.data_format,
        time_mult: cfg_data.time_mult,
        samples,
        header_text,
        info,
    })
}

/// Parse a COMTRADE record from a .cff (Combined File Format) file.
pub fn parse_comtrade_cff(cff_path: &Path) -> Result<ComtradeRecord, ComtradeError> {
    let text = std::fs::read_to_string(cff_path)
        .map_err(|e| ComtradeError::Io(format!("{}: {e}", cff_path.display())))?;
    cff::parse_cff(&text)
}

/// Parse COMTRADE from in-memory strings/bytes.
///
/// For ASCII format, pass the .dat content as a string via `dat_text`.
/// For binary formats, pass raw bytes via `dat_bytes`.
pub fn parse_comtrade_bytes(
    cfg_text: &str,
    dat_text: Option<&str>,
    dat_bytes: Option<&[u8]>,
) -> Result<ComtradeRecord, ComtradeError> {
    let cfg_data = cfg::parse_cfg(cfg_text)?;

    let samples = match cfg_data.data_format {
        DataFormat::Ascii => {
            let text = dat_text.ok_or(ComtradeError::Io(
                "ASCII format requires dat_text".to_string(),
            ))?;
            dat::parse_dat_ascii(text, &cfg_data.analog_channels, &cfg_data.digital_channels)?
        }
        DataFormat::Binary16 => {
            let bytes = dat_bytes.ok_or(ComtradeError::Io(
                "Binary format requires dat_bytes".to_string(),
            ))?;
            dat::parse_dat_binary16(bytes, &cfg_data.analog_channels, &cfg_data.digital_channels)?
        }
        DataFormat::Binary32 => {
            let bytes = dat_bytes.ok_or(ComtradeError::Io(
                "Binary format requires dat_bytes".to_string(),
            ))?;
            dat::parse_dat_binary32(bytes, &cfg_data.analog_channels, &cfg_data.digital_channels)?
        }
        DataFormat::Float32 => {
            let bytes = dat_bytes.ok_or(ComtradeError::Io(
                "Binary format requires dat_bytes".to_string(),
            ))?;
            dat::parse_dat_float32(bytes, &cfg_data.analog_channels, &cfg_data.digital_channels)?
        }
    };

    Ok(ComtradeRecord {
        station_name: cfg_data.station_name,
        rec_dev_id: cfg_data.rec_dev_id,
        rev_year: cfg_data.rev_year,
        frequency: cfg_data.frequency,
        start_time: cfg_data.start_time,
        trigger_time: cfg_data.trigger_time,
        analog_channels: cfg_data.analog_channels,
        digital_channels: cfg_data.digital_channels,
        sample_rates: cfg_data.sample_rates,
        data_format: cfg_data.data_format,
        time_mult: cfg_data.time_mult,
        samples,
        header_text: None,
        info: None,
    })
}

/// Find a companion file with a case-insensitive extension search.
///
/// Scans the sibling directory for the same stem and an extension matching
/// `ext` with ASCII case-insensitive comparison.
fn find_companion(stem: &Path, ext: &str) -> Option<PathBuf> {
    let target_stem = stem.file_name()?;
    let parent = stem.parent().unwrap_or_else(|| Path::new("."));
    let entries = std::fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_stem() != Some(target_stem) {
            continue;
        }
        let candidate_ext = path.extension().and_then(|value| value.to_str());
        if candidate_ext.is_some_and(|value| value.eq_ignore_ascii_case(ext)) {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_record() -> ComtradeRecord {
        ComtradeRecord {
            station_name: "TEST_SUB".to_string(),
            rec_dev_id: "DFR_1".to_string(),
            rev_year: RevYear::Y1999,
            frequency: 60.0,
            start_time: ComtradeTimestamp {
                day: 1,
                month: 1,
                year: 2025,
                hour: 12,
                minute: 0,
                second: 0.0,
            },
            trigger_time: ComtradeTimestamp {
                day: 1,
                month: 1,
                year: 2025,
                hour: 12,
                minute: 0,
                second: 0.001,
            },
            analog_channels: vec![AnalogChannel {
                index: 1,
                name: "VA".to_string(),
                phase: "A".to_string(),
                circuit_component: "LINE1".to_string(),
                units: "kV".to_string(),
                multiplier: 1.0,
                offset: 0.0,
                skew: 0.0,
                min_value: -99999.0,
                max_value: 99999.0,
                primary_ratio: 132.0,
                secondary_ratio: 0.11,
                scaling: ScalingFlag::Primary,
            }],
            digital_channels: vec![DigitalChannel {
                index: 2,
                name: "TRIP".to_string(),
                phase: String::new(),
                circuit_component: "LINE1".to_string(),
                normal_state: 0,
            }],
            sample_rates: vec![SampleRate {
                rate_hz: 4000.0,
                last_sample: 1,
            }],
            data_format: DataFormat::Ascii,
            time_mult: 1.0,
            samples: vec![Sample {
                number: 1,
                timestamp_us: 0.0,
                analog: vec![100.0],
                digital: vec![false],
            }],
            header_text: Some("Test fault event".to_string()),
            info: Some(HashMap::from([(
                "model".to_string(),
                "SEL-421".to_string(),
            )])),
        }
    }

    #[test]
    fn test_parse_comtrade_finds_mixed_case_companion_extensions() {
        let rec = sample_record();
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("fault.cfg");
        write_comtrade(&rec, &cfg_path).unwrap();

        std::fs::rename(dir.path().join("fault.dat"), dir.path().join("fault.DaT")).unwrap();
        std::fs::rename(dir.path().join("fault.hdr"), dir.path().join("fault.HdR")).unwrap();
        std::fs::rename(dir.path().join("fault.inf"), dir.path().join("fault.InF")).unwrap();

        let parsed = parse_comtrade(&cfg_path).unwrap();
        assert_eq!(parsed.samples.len(), 1);
        assert_eq!(parsed.header_text.as_deref(), Some("Test fault event"));
        assert_eq!(
            parsed
                .info
                .as_ref()
                .and_then(|info| info.get("model").map(String::as_str)),
            Some("SEL-421")
        );
    }
}
