// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! COMTRADE writer — exports `ComtradeRecord` to .cfg + .dat files.
//!
//! Supports ASCII and all binary formats. Optionally writes .cff combined.
//!
//! **Scaling**: `Sample.analog` stores scaled engineering values. The writer
//! un-scales them back to raw values before writing: `raw = (value - offset) / multiplier`.
//! This ensures correct round-trip through read → write → read.

use super::ComtradeError;
use super::types::*;
use std::path::Path;

/// Write a COMTRADE record to .cfg + .dat files.
///
/// The `path` should be the .cfg file path; the .dat file is written alongside
/// with the same stem.
pub fn write_comtrade(record: &ComtradeRecord, path: &Path) -> Result<(), ComtradeError> {
    let cfg_str = to_cfg_string(record);
    let stem = path.with_extension("");
    let dat_path = stem.with_extension("dat");

    std::fs::write(path, cfg_str.as_bytes())
        .map_err(|e| ComtradeError::Io(format!("writing cfg: {e}")))?;

    match record.data_format {
        DataFormat::Ascii => {
            let dat_str = to_dat_ascii_string(record);
            std::fs::write(&dat_path, dat_str.as_bytes())
                .map_err(|e| ComtradeError::Io(format!("writing dat: {e}")))?;
        }
        DataFormat::Binary16 | DataFormat::Binary32 | DataFormat::Float32 => {
            let dat_bytes = to_dat_binary(record);
            std::fs::write(&dat_path, &dat_bytes)
                .map_err(|e| ComtradeError::Io(format!("writing dat: {e}")))?;
        }
    }

    // Write .hdr if present
    if let Some(ref hdr) = record.header_text {
        let hdr_path = stem.with_extension("hdr");
        std::fs::write(&hdr_path, hdr.as_bytes())
            .map_err(|e| ComtradeError::Io(format!("writing hdr: {e}")))?;
    }

    // Write .inf if present
    if let Some(ref info) = record.info {
        let inf_path = stem.with_extension("inf");
        let mut inf_str = String::new();
        for (k, v) in info {
            inf_str.push_str(&format!("{k}={v}\n"));
        }
        std::fs::write(&inf_path, inf_str.as_bytes())
            .map_err(|e| ComtradeError::Io(format!("writing inf: {e}")))?;
    }

    Ok(())
}

/// Write a COMTRADE record as a single .cff (Combined File Format) file.
///
/// CFF is a text-based combined format; only ASCII data can be embedded.
/// Returns `CffBinaryNotSupported` if `record.data_format` is not `Ascii`.
pub fn write_comtrade_cff(record: &ComtradeRecord, path: &Path) -> Result<(), ComtradeError> {
    if record.data_format != DataFormat::Ascii {
        return Err(ComtradeError::CffBinaryNotSupported);
    }
    let mut out = String::new();

    out.push_str("--- file type: cfg ---\n");
    out.push_str(&to_cfg_string(record));

    if let Some(ref hdr) = record.header_text {
        out.push_str("--- file type: hdr ---\n");
        out.push_str(hdr);
        out.push('\n');
    }

    out.push_str("--- file type: dat ---\n");
    out.push_str(&to_dat_ascii_string(record));

    if let Some(ref info) = record.info {
        out.push_str("--- file type: inf ---\n");
        for (k, v) in info {
            out.push_str(&format!("{k}={v}\n"));
        }
    }

    std::fs::write(path, out.as_bytes())
        .map_err(|e| ComtradeError::Io(format!("writing cff: {e}")))?;

    Ok(())
}

/// Generate .cfg file content as a string.
///
/// Respects `rev_year`: 1991 omits rev_year from line 1 and writes 10-field
/// analog channels; 1999/2013 writes 13-field analog channels.
pub fn to_cfg_string(record: &ComtradeRecord) -> String {
    let mut s = String::new();
    let n_total = record.n_analog() + record.n_digital();

    // Line 1: station, device [, rev_year]
    match record.rev_year {
        RevYear::Y1991 => {
            // 1991: no rev_year field
            s.push_str(&format!("{},{}\n", record.station_name, record.rec_dev_id));
        }
        _ => {
            s.push_str(&format!(
                "{},{},{}\n",
                record.station_name, record.rec_dev_id, record.rev_year
            ));
        }
    }

    // Line 2: TT, ##A, ##D
    s.push_str(&format!(
        "{},{}A,{}D\n",
        n_total,
        record.n_analog(),
        record.n_digital()
    ));

    // Analog channels
    for ch in &record.analog_channels {
        match record.rev_year {
            RevYear::Y1991 => {
                // 1991: 10 fields only (no primary/secondary/PS)
                s.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{},{}\n",
                    ch.index,
                    ch.name,
                    ch.phase,
                    ch.circuit_component,
                    ch.units,
                    ch.multiplier,
                    ch.offset,
                    ch.skew,
                    ch.min_value,
                    ch.max_value,
                ));
            }
            _ => {
                // 1999/2013: 13 fields
                s.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                    ch.index,
                    ch.name,
                    ch.phase,
                    ch.circuit_component,
                    ch.units,
                    ch.multiplier,
                    ch.offset,
                    ch.skew,
                    ch.min_value,
                    ch.max_value,
                    ch.primary_ratio,
                    ch.secondary_ratio,
                    match ch.scaling {
                        ScalingFlag::Primary => "P",
                        ScalingFlag::Secondary => "S",
                    },
                ));
            }
        }
    }

    // Digital channels
    for ch in &record.digital_channels {
        s.push_str(&format!(
            "{},{},{},{},{}\n",
            ch.index, ch.name, ch.phase, ch.circuit_component, ch.normal_state,
        ));
    }

    // Frequency
    s.push_str(&format!("{}\n", record.frequency));

    // Sample rates
    s.push_str(&format!("{}\n", record.sample_rates.len()));
    for sr in &record.sample_rates {
        s.push_str(&format!("{},{}\n", sr.rate_hz, sr.last_sample));
    }

    // Timestamps — use nanosecond precision for 2013
    match record.rev_year {
        RevYear::Y2013 => {
            s.push_str(&format!("{}\n", record.start_time.fmt_ns()));
            s.push_str(&format!("{}\n", record.trigger_time.fmt_ns()));
        }
        _ => {
            s.push_str(&format!("{}\n", record.start_time.fmt_us()));
            s.push_str(&format!("{}\n", record.trigger_time.fmt_us()));
        }
    }

    // Data format
    s.push_str(&format!("{}\n", record.data_format));

    // Time multiplier (1999/2013 only; omit for 1991)
    match record.rev_year {
        RevYear::Y1991 => {}
        _ => {
            s.push_str(&format!("{}\n", record.time_mult));
        }
    }

    s
}

/// Generate ASCII .dat file content as a string.
///
/// Un-scales analog values back to raw before writing so that the reader
/// can correctly re-apply `raw * multiplier + offset`.
pub fn to_dat_ascii_string(record: &ComtradeRecord) -> String {
    let mut s = String::new();
    for sample in &record.samples {
        s.push_str(&format!("{},{}", sample.number, sample.timestamp_us as i64));
        for (i, &v) in sample.analog.iter().enumerate() {
            let raw = record.analog_channels[i].to_raw(v);
            // Write raw as float to preserve precision for fractional multipliers
            if raw == raw.round() && raw.abs() < i64::MAX as f64 {
                s.push_str(&format!(",{}", raw as i64));
            } else {
                s.push_str(&format!(",{raw}"));
            }
        }
        for &d in &sample.digital {
            s.push_str(&format!(",{}", if d { 1 } else { 0 }));
        }
        s.push('\n');
    }
    s
}

/// Generate binary .dat content.
///
/// Un-scales analog values back to raw before encoding.
pub fn to_dat_binary(record: &ComtradeRecord) -> Vec<u8> {
    let n_digital = record.n_digital();
    let n_digital_words = n_digital.div_ceil(16);
    let mut buf = Vec::new();

    for sample in &record.samples {
        buf.extend_from_slice(&sample.number.to_le_bytes());
        buf.extend_from_slice(&(sample.timestamp_us as u32).to_le_bytes());

        match record.data_format {
            DataFormat::Binary16 => {
                for (i, &v) in sample.analog.iter().enumerate() {
                    let raw = record.analog_channels[i].to_raw(v);
                    buf.extend_from_slice(&(raw as i16).to_le_bytes());
                }
            }
            DataFormat::Binary32 => {
                for (i, &v) in sample.analog.iter().enumerate() {
                    let raw = record.analog_channels[i].to_raw(v);
                    buf.extend_from_slice(&(raw as i32).to_le_bytes());
                }
            }
            DataFormat::Float32 => {
                for (i, &v) in sample.analog.iter().enumerate() {
                    let raw = record.analog_channels[i].to_raw(v);
                    buf.extend_from_slice(&(raw as f32).to_le_bytes());
                }
            }
            DataFormat::Ascii => {
                // ASCII is handled by to_dat_ascii_string(); if called here,
                // write nothing for analog (header + digital words still emitted).
            }
        }

        // Pack digital channels into 16-bit words
        for w in 0..n_digital_words {
            let mut word: u16 = 0;
            for bit in 0..16 {
                let ch_idx = w * 16 + bit;
                if ch_idx < n_digital && sample.digital[ch_idx] {
                    word |= 1 << bit;
                }
            }
            buf.extend_from_slice(&word.to_le_bytes());
        }
    }

    buf
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
                last_sample: 3,
            }],
            data_format: DataFormat::Ascii,
            time_mult: 1.0,
            samples: vec![
                Sample {
                    number: 1,
                    timestamp_us: 0.0,
                    analog: vec![100.0],
                    digital: vec![false],
                },
                Sample {
                    number: 2,
                    timestamp_us: 250.0,
                    analog: vec![110.0],
                    digital: vec![false],
                },
                Sample {
                    number: 3,
                    timestamp_us: 500.0,
                    analog: vec![-200.0],
                    digital: vec![true],
                },
            ],
            header_text: Some("Test fault event".to_string()),
            info: Some(HashMap::from([(
                "model".to_string(),
                "SEL-421".to_string(),
            )])),
        }
    }

    #[test]
    fn test_cfg_string_roundtrip() {
        let rec = sample_record();
        let cfg_str = to_cfg_string(&rec);
        let cfg = super::super::cfg::parse_cfg(&cfg_str).unwrap();
        assert_eq!(cfg.station_name, "TEST_SUB");
        assert_eq!(cfg.rev_year, RevYear::Y1999);
        assert_eq!(cfg.analog_channels.len(), 1);
        assert_eq!(cfg.digital_channels.len(), 1);
        assert_eq!(cfg.frequency, 60.0);
        assert_eq!(cfg.time_mult, 1.0);
    }

    #[test]
    fn test_cfg_string_1991_omits_rev_year() {
        let mut rec = sample_record();
        rec.rev_year = RevYear::Y1991;
        let cfg_str = to_cfg_string(&rec);
        let first_line = cfg_str.lines().next().unwrap();
        // 1991: only station,device — no third field
        assert_eq!(first_line, "TEST_SUB,DFR_1");
        // Analog channel should have 10 fields
        let analog_line = cfg_str.lines().nth(2).unwrap();
        let field_count = analog_line.split(',').count();
        assert_eq!(field_count, 10, "1991 analog should have 10 fields");
        // No time_mult line: after data format, the file should end
        let cfg = super::super::cfg::parse_cfg(&cfg_str).unwrap();
        assert_eq!(cfg.rev_year, RevYear::Y1991);
    }

    #[test]
    fn test_cfg_string_2013_nanosecond_timestamps() {
        let mut rec = sample_record();
        rec.rev_year = RevYear::Y2013;
        rec.start_time.second = 5.123456789;
        let cfg_str = to_cfg_string(&rec);
        assert!(
            cfg_str.contains("123456789"),
            "2013 should use nanosecond precision"
        );
    }

    #[test]
    fn test_dat_ascii_roundtrip() {
        let rec = sample_record();
        let dat_str = to_dat_ascii_string(&rec);
        let samples = super::super::dat::parse_dat_ascii(
            &dat_str,
            &rec.analog_channels,
            &rec.digital_channels,
        )
        .unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].analog[0], 100.0);
        assert!(samples[2].digital[0]);
    }

    #[test]
    fn test_dat_ascii_roundtrip_with_scaling() {
        // BUG 1 regression test: multiplier != 1 and offset != 0 must round-trip
        let mut rec = sample_record();
        rec.analog_channels[0].multiplier = 0.5;
        rec.analog_channels[0].offset = 10.0;
        // Sample.analog stores scaled values: value = raw * 0.5 + 10
        // e.g., raw=100 → value=60, raw=200 → value=110
        rec.samples[0].analog[0] = 60.0; // raw = (60-10)/0.5 = 100
        rec.samples[1].analog[0] = 110.0; // raw = (110-10)/0.5 = 200
        rec.samples[2].analog[0] = -90.0; // raw = (-90-10)/0.5 = -200

        let dat_str = to_dat_ascii_string(&rec);
        let samples = super::super::dat::parse_dat_ascii(
            &dat_str,
            &rec.analog_channels,
            &rec.digital_channels,
        )
        .unwrap();
        assert!(
            (samples[0].analog[0] - 60.0).abs() < 1e-10,
            "Expected 60.0, got {}",
            samples[0].analog[0]
        );
        assert!(
            (samples[1].analog[0] - 110.0).abs() < 1e-10,
            "Expected 110.0, got {}",
            samples[1].analog[0]
        );
        assert!(
            (samples[2].analog[0] - (-90.0)).abs() < 1e-10,
            "Expected -90.0, got {}",
            samples[2].analog[0]
        );
    }

    #[test]
    fn test_dat_ascii_roundtrip_fractional_multiplier() {
        // Fractional raw values must survive round-trip via float output
        let mut rec = sample_record();
        rec.analog_channels[0].multiplier = 0.3;
        rec.analog_channels[0].offset = 0.0;
        rec.samples[0].analog[0] = 3.3; // raw = 3.3 / 0.3 = 11.0

        let dat_str = to_dat_ascii_string(&rec);
        let samples = super::super::dat::parse_dat_ascii(
            &dat_str,
            &rec.analog_channels,
            &rec.digital_channels,
        )
        .unwrap();
        assert!(
            (samples[0].analog[0] - 3.3).abs() < 1e-10,
            "Expected 3.3, got {}",
            samples[0].analog[0]
        );
    }

    #[test]
    fn test_write_read_files() {
        let rec = sample_record();
        let dir = std::env::temp_dir().join("surge_comtrade_writer_test");
        let _ = std::fs::create_dir_all(&dir);
        let cfg_path = dir.join("test.cfg");

        write_comtrade(&rec, &cfg_path).unwrap();

        assert!(cfg_path.exists());
        assert!(dir.join("test.dat").exists());
        assert!(dir.join("test.hdr").exists());
        assert!(dir.join("test.inf").exists());

        let rec2 = super::super::parse_comtrade(&cfg_path).unwrap();
        assert_eq!(rec2.station_name, "TEST_SUB");
        assert_eq!(rec2.n_samples(), 3);
        assert_eq!(rec2.samples[0].analog[0], 100.0);
        assert_eq!(rec2.samples[2].analog[0], -200.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_read_files_with_scaling() {
        // Full file round-trip with non-trivial scaling
        let mut rec = sample_record();
        rec.analog_channels[0].multiplier = 2.0;
        rec.analog_channels[0].offset = 5.0;
        rec.samples[0].analog[0] = 25.0; // raw = (25-5)/2 = 10
        rec.samples[1].analog[0] = 45.0; // raw = (45-5)/2 = 20
        rec.samples[2].analog[0] = -15.0; // raw = (-15-5)/2 = -10

        let dir = std::env::temp_dir().join("surge_comtrade_scaling_test");
        let _ = std::fs::create_dir_all(&dir);
        let cfg_path = dir.join("test.cfg");

        write_comtrade(&rec, &cfg_path).unwrap();
        let rec2 = super::super::parse_comtrade(&cfg_path).unwrap();

        assert!(
            (rec2.samples[0].analog[0] - 25.0).abs() < 1e-10,
            "Expected 25.0, got {}",
            rec2.samples[0].analog[0]
        );
        assert!(
            (rec2.samples[1].analog[0] - 45.0).abs() < 1e-10,
            "Expected 45.0, got {}",
            rec2.samples[1].analog[0]
        );
        assert!(
            (rec2.samples[2].analog[0] - (-15.0)).abs() < 1e-10,
            "Expected -15.0, got {}",
            rec2.samples[2].analog[0]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_cff() {
        let rec = sample_record();
        let dir = std::env::temp_dir().join("surge_comtrade_cff_test");
        let _ = std::fs::create_dir_all(&dir);
        let cff_path = dir.join("test.cff");

        write_comtrade_cff(&rec, &cff_path).unwrap();
        assert!(cff_path.exists());

        let rec2 = super::super::parse_comtrade_cff(&cff_path).unwrap();
        assert_eq!(rec2.station_name, "TEST_SUB");
        assert_eq!(rec2.n_samples(), 3);
        assert_eq!(rec2.header_text.as_deref(), Some("Test fault event"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_cff_rejects_binary() {
        let mut rec = sample_record();
        rec.data_format = DataFormat::Binary16;
        let dir = std::env::temp_dir().join("surge_comtrade_cff_binary_test");
        let _ = std::fs::create_dir_all(&dir);
        let cff_path = dir.join("test.cff");

        let result = write_comtrade_cff(&rec, &cff_path);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("binary"),
            "should mention binary in error message"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_binary16_roundtrip() {
        let mut rec = sample_record();
        rec.data_format = DataFormat::Binary16;
        let bytes = to_dat_binary(&rec);
        let samples = super::super::dat::parse_dat_binary16(
            &bytes,
            &rec.analog_channels,
            &rec.digital_channels,
        )
        .unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].analog[0], 100.0);
        assert!(samples[2].digital[0]);
    }

    #[test]
    fn test_binary16_roundtrip_with_scaling() {
        let mut rec = sample_record();
        rec.data_format = DataFormat::Binary16;
        rec.analog_channels[0].multiplier = 2.0;
        rec.analog_channels[0].offset = 5.0;
        rec.samples[0].analog[0] = 25.0; // raw = (25-5)/2 = 10

        let bytes = to_dat_binary(&rec);
        let samples = super::super::dat::parse_dat_binary16(
            &bytes,
            &rec.analog_channels,
            &rec.digital_channels,
        )
        .unwrap();
        assert_eq!(samples[0].analog[0], 25.0);
    }

    #[test]
    fn test_binary32_roundtrip_with_scaling() {
        let mut rec = sample_record();
        rec.data_format = DataFormat::Binary32;
        rec.analog_channels[0].multiplier = 0.5;
        rec.analog_channels[0].offset = 10.0;
        rec.samples[0].analog[0] = 60.0; // raw = (60-10)/0.5 = 100

        let bytes = to_dat_binary(&rec);
        let samples = super::super::dat::parse_dat_binary32(
            &bytes,
            &rec.analog_channels,
            &rec.digital_channels,
        )
        .unwrap();
        assert_eq!(samples[0].analog[0], 60.0);
    }

    #[test]
    fn test_validate_ok() {
        let rec = sample_record();
        assert!(rec.validate().is_ok());
    }

    #[test]
    fn test_validate_mismatch() {
        let mut rec = sample_record();
        rec.samples[0].analog.push(999.0); // extra analog value
        assert!(rec.validate().is_err());
    }
}
