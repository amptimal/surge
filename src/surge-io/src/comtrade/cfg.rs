// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! COMTRADE .cfg (configuration) file parser.
//!
//! Supports IEEE C37.111-1991, 1999, and 2013 dialects.

use super::ComtradeError;
use super::types::*;

/// Parsed .cfg content (everything except waveform data).
#[derive(Debug, Clone)]
pub struct CfgData {
    pub station_name: String,
    pub rec_dev_id: String,
    pub rev_year: RevYear,
    pub analog_channels: Vec<AnalogChannel>,
    pub digital_channels: Vec<DigitalChannel>,
    pub frequency: f64,
    pub sample_rates: Vec<SampleRate>,
    pub start_time: ComtradeTimestamp,
    pub trigger_time: ComtradeTimestamp,
    pub data_format: DataFormat,
    pub time_mult: f64,
}

/// Parse a .cfg file from a string.
pub fn parse_cfg(text: &str) -> Result<CfgData, ComtradeError> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 4 {
        return Err(ComtradeError::CfgTooShort(lines.len()));
    }

    let mut pos = 0;

    // --- Line 1: station_name, rec_dev_id [, rev_year] ---
    let line1 = parse_csv(lines[pos]);
    pos += 1;
    let station_name = line1.first().map(|s| s.to_string()).unwrap_or_default();
    let rec_dev_id = line1.get(1).map(|s| s.to_string()).unwrap_or_default();
    let rev_year = match line1.get(2).map(|s| s.as_str()) {
        Some("2013") => RevYear::Y2013,
        Some("1999") => RevYear::Y1999,
        Some("1991") => RevYear::Y1991,
        None | Some("") => RevYear::Y1991, // absent → 1991
        Some(other) => return Err(ComtradeError::BadRevYear(other.to_string())),
    };

    // --- Line 2: TT, ##A, ##D ---
    let line2 = parse_csv(lines[pos]);
    pos += 1;
    if line2.len() < 3 {
        return Err(ComtradeError::BadChannelLine(lines[pos - 1].to_string()));
    }
    let n_analog = parse_channel_count(&line2[1], 'A')?;
    let n_digital = parse_channel_count(&line2[2], 'D')?;

    // --- Analog channel lines ---
    let mut analog_channels = Vec::with_capacity(n_analog);
    for _ in 0..n_analog {
        if pos >= lines.len() {
            return Err(ComtradeError::UnexpectedEof("analog channel definition"));
        }
        analog_channels.push(parse_analog_channel(lines[pos], rev_year)?);
        pos += 1;
    }

    // --- Digital channel lines ---
    let mut digital_channels = Vec::with_capacity(n_digital);
    for _ in 0..n_digital {
        if pos >= lines.len() {
            return Err(ComtradeError::UnexpectedEof("digital channel definition"));
        }
        digital_channels.push(parse_digital_channel(lines[pos])?);
        pos += 1;
    }

    // --- Line frequency ---
    if pos >= lines.len() {
        return Err(ComtradeError::UnexpectedEof("line frequency"));
    }
    let frequency: f64 = lines[pos]
        .trim()
        .parse()
        .map_err(|_| ComtradeError::BadFloat("frequency", lines[pos].to_string()))?;
    pos += 1;

    // --- Number of sampling rates ---
    if pos >= lines.len() {
        return Err(ComtradeError::UnexpectedEof("sampling rate count"));
    }
    let n_rates: usize = lines[pos]
        .trim()
        .parse()
        .map_err(|_| ComtradeError::BadInt("nrates", lines[pos].to_string()))?;
    pos += 1;

    // --- Sampling rate entries ---
    let mut sample_rates = Vec::with_capacity(n_rates.max(1));
    if n_rates == 0 {
        // Non-uniform/event-driven: one entry with rate=0
        sample_rates.push(SampleRate {
            rate_hz: 0.0,
            last_sample: 0,
        });
    } else {
        for _ in 0..n_rates {
            if pos >= lines.len() {
                return Err(ComtradeError::UnexpectedEof("sampling rate entry"));
            }
            let parts = parse_csv(lines[pos]);
            pos += 1;
            let rate_hz: f64 = parts
                .first()
                .ok_or(ComtradeError::UnexpectedEof("sample rate Hz"))?
                .parse()
                .map_err(|_| ComtradeError::BadFloat("sample_rate", parts[0].clone()))?;
            let last_sample: u32 = parts
                .get(1)
                .ok_or(ComtradeError::UnexpectedEof("last_sample"))?
                .parse()
                .map_err(|_| ComtradeError::BadInt("last_sample", parts[1].clone()))?;
            sample_rates.push(SampleRate {
                rate_hz,
                last_sample,
            });
        }
    }

    // --- Start timestamp ---
    if pos >= lines.len() {
        return Err(ComtradeError::UnexpectedEof("start timestamp"));
    }
    let start_time = parse_timestamp(lines[pos])?;
    pos += 1;

    // --- Trigger timestamp ---
    if pos >= lines.len() {
        return Err(ComtradeError::UnexpectedEof("trigger timestamp"));
    }
    let trigger_time = parse_timestamp(lines[pos])?;
    pos += 1;

    // --- Data file type ---
    if pos >= lines.len() {
        return Err(ComtradeError::UnexpectedEof("data file type"));
    }
    let data_format = match lines[pos].trim().to_uppercase().as_str() {
        "ASCII" => DataFormat::Ascii,
        "BINARY" => DataFormat::Binary16,
        "BINARY32" => DataFormat::Binary32,
        "FLOAT32" => DataFormat::Float32,
        other => return Err(ComtradeError::BadDataFormat(other.to_string())),
    };
    pos += 1;

    // --- Time multiplier (1999/2013 only, optional for 1991) ---
    let time_mult = if pos < lines.len() {
        lines[pos].trim().parse::<f64>().unwrap_or(1.0)
    } else {
        1.0
    };

    Ok(CfgData {
        station_name,
        rec_dev_id,
        rev_year,
        analog_channels,
        digital_channels,
        frequency,
        sample_rates,
        start_time,
        trigger_time,
        data_format,
        time_mult,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_csv(line: &str) -> Vec<String> {
    line.split(',').map(|s| s.trim().to_string()).collect()
}

/// Parse channel count like "4A" or "8D". Validates the trailing suffix matches
/// the expected type character.
fn parse_channel_count(s: &str, expected_suffix: char) -> Result<usize, ComtradeError> {
    let s = s.trim();
    let digits = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = &s[digits.len()..];

    // Validate suffix if present
    if !suffix.is_empty() {
        let actual = suffix
            .chars()
            .next()
            .expect("suffix is non-empty")
            .to_ascii_uppercase();
        let expected = expected_suffix.to_ascii_uppercase();
        if actual != expected {
            return Err(ComtradeError::BadChannelLine(format!(
                "expected suffix '{expected}' but got '{actual}' in '{s}'"
            )));
        }
    }

    if digits.is_empty() {
        return Ok(0);
    }
    digits
        .parse()
        .map_err(|_| ComtradeError::BadInt("channel_count", s.to_string()))
}

fn parse_analog_channel(line: &str, rev_year: RevYear) -> Result<AnalogChannel, ComtradeError> {
    let f = parse_csv(line);
    // 1991: 10 fields (An, ch_id, ph, ccbm, uu, a, b, skew, min, max)
    // 1999/2013: 13 fields (+ primary, secondary, PS)
    if f.len() < 10 {
        return Err(ComtradeError::BadAnalogChannel(line.to_string()));
    }

    let index: u32 = f[0]
        .parse()
        .map_err(|_| ComtradeError::BadInt("analog_index", f[0].clone()))?;
    let multiplier: f64 = f[5]
        .parse()
        .map_err(|_| ComtradeError::BadFloat("multiplier", f[5].clone()))?;
    let offset: f64 = f[6]
        .parse()
        .map_err(|_| ComtradeError::BadFloat("offset", f[6].clone()))?;
    let skew: f64 = f[7]
        .parse()
        .map_err(|_| ComtradeError::BadFloat("skew", f[7].clone()))?;
    let min_value: f64 = f[8]
        .parse()
        .map_err(|_| ComtradeError::BadFloat("min_value", f[8].clone()))?;
    let max_value: f64 = f[9]
        .parse()
        .map_err(|_| ComtradeError::BadFloat("max_value", f[9].clone()))?;

    let (primary_ratio, secondary_ratio, scaling) = if rev_year != RevYear::Y1991 && f.len() >= 13 {
        let pr: f64 = f[10]
            .parse()
            .map_err(|_| ComtradeError::BadFloat("primary_ratio", f[10].clone()))?;
        let sr: f64 = f[11]
            .parse()
            .map_err(|_| ComtradeError::BadFloat("secondary_ratio", f[11].clone()))?;
        let sc = match f[12].to_uppercase().as_str() {
            "P" => ScalingFlag::Primary,
            _ => ScalingFlag::Secondary,
        };
        (pr, sr, sc)
    } else {
        (1.0, 1.0, ScalingFlag::Primary)
    };

    Ok(AnalogChannel {
        index,
        name: f[1].clone(),
        phase: f[2].clone(),
        circuit_component: f[3].clone(),
        units: f[4].clone(),
        multiplier,
        offset,
        skew,
        min_value,
        max_value,
        primary_ratio,
        secondary_ratio,
        scaling,
    })
}

fn parse_digital_channel(line: &str) -> Result<DigitalChannel, ComtradeError> {
    let f = parse_csv(line);
    if f.len() < 5 {
        return Err(ComtradeError::BadDigitalChannel(line.to_string()));
    }
    let index: u32 = f[0]
        .parse()
        .map_err(|_| ComtradeError::BadInt("digital_index", f[0].clone()))?;
    let normal_state: u8 = f[4]
        .parse()
        .map_err(|_| ComtradeError::BadInt("normal_state", f[4].clone()))?;

    Ok(DigitalChannel {
        index,
        name: f[1].clone(),
        phase: f[2].clone(),
        circuit_component: f[3].clone(),
        normal_state,
    })
}

/// Parse COMTRADE timestamp: `dd/mm/yyyy,hh:mm:ss.ssssss[sss]`
///
/// The date and time are on the same line separated by comma.
fn parse_timestamp(line: &str) -> Result<ComtradeTimestamp, ComtradeError> {
    let line = line.trim();
    // Split on comma to get date part and time part
    let parts: Vec<&str> = line.splitn(2, ',').collect();
    if parts.len() < 2 {
        return Err(ComtradeError::BadTimestamp(line.to_string()));
    }
    let date_part = parts[0].trim();
    let time_part = parts[1].trim();

    // Date: dd/mm/yyyy
    let dp: Vec<&str> = date_part.split('/').collect();
    if dp.len() != 3 {
        return Err(ComtradeError::BadTimestamp(line.to_string()));
    }
    let day: u32 = dp[0]
        .parse()
        .map_err(|_| ComtradeError::BadTimestamp(line.to_string()))?;
    let month: u32 = dp[1]
        .parse()
        .map_err(|_| ComtradeError::BadTimestamp(line.to_string()))?;
    let year: u32 = dp[2]
        .parse()
        .map_err(|_| ComtradeError::BadTimestamp(line.to_string()))?;

    // Time: hh:mm:ss.ssssss (fractional seconds may vary in precision)
    let tp: Vec<&str> = time_part.splitn(3, ':').collect();
    if tp.len() != 3 {
        return Err(ComtradeError::BadTimestamp(line.to_string()));
    }
    let hour: u32 = tp[0]
        .parse()
        .map_err(|_| ComtradeError::BadTimestamp(line.to_string()))?;
    let minute: u32 = tp[1]
        .parse()
        .map_err(|_| ComtradeError::BadTimestamp(line.to_string()))?;
    let second: f64 = tp[2]
        .parse()
        .map_err(|_| ComtradeError::BadTimestamp(line.to_string()))?;

    Ok(ComtradeTimestamp {
        day,
        month,
        year,
        hour,
        minute,
        second,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_timestamp_basic() {
        let ts = parse_timestamp("07/03/2026,14:30:05.123456").unwrap();
        assert_eq!(ts.day, 7);
        assert_eq!(ts.month, 3);
        assert_eq!(ts.year, 2026);
        assert_eq!(ts.hour, 14);
        assert_eq!(ts.minute, 30);
        assert!((ts.second - 5.123456).abs() < 1e-9);
    }

    #[test]
    fn test_parse_timestamp_nanosecond() {
        let ts = parse_timestamp("07/03/2026,14:30:05.123456789").unwrap();
        assert!((ts.second - 5.123456789).abs() < 1e-12);
    }

    #[test]
    fn test_parse_channel_count() {
        assert_eq!(parse_channel_count("4A", 'A').unwrap(), 4);
        assert_eq!(parse_channel_count("8D", 'D').unwrap(), 8);
        assert_eq!(parse_channel_count("0A", 'A').unwrap(), 0);
    }

    #[test]
    fn test_parse_channel_count_suffix_mismatch() {
        let result = parse_channel_count("4D", 'A');
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("expected suffix 'A'"), "Got: {msg}");
    }

    #[test]
    fn test_parse_cfg_minimal_1999() {
        let cfg_text = "\
STATION_A,DEVICE_1,1999
8,4A,4D
1,VA,A,LINE1,kV,1.0,0.0,0.0,-99999,99999,132.0,0.110,P
2,VB,B,LINE1,kV,1.0,0.0,0.0,-99999,99999,132.0,0.110,P
3,VC,C,LINE1,kV,1.0,0.0,0.0,-99999,99999,132.0,0.110,P
4,IA,A,LINE1,A,1.0,0.0,0.0,-99999,99999,800.0,1.0,P
5,TRIP,,LINE1,0
6,RECLOSE,,LINE1,0
7,PILOT,,LINE1,0
8,BLOCK,,LINE1,1
60
1
4000,100
07/03/2026,14:30:05.000000
07/03/2026,14:30:05.010000
ASCII
1.0
";
        let cfg = parse_cfg(cfg_text).unwrap();
        assert_eq!(cfg.station_name, "STATION_A");
        assert_eq!(cfg.rec_dev_id, "DEVICE_1");
        assert_eq!(cfg.rev_year, RevYear::Y1999);
        assert_eq!(cfg.analog_channels.len(), 4);
        assert_eq!(cfg.digital_channels.len(), 4);
        assert_eq!(cfg.frequency, 60.0);
        assert_eq!(cfg.sample_rates.len(), 1);
        assert_eq!(cfg.sample_rates[0].rate_hz, 4000.0);
        assert_eq!(cfg.sample_rates[0].last_sample, 100);
        assert_eq!(cfg.data_format, DataFormat::Ascii);
        assert_eq!(cfg.time_mult, 1.0);

        // Check analog channel details
        let va = &cfg.analog_channels[0];
        assert_eq!(va.name, "VA");
        assert_eq!(va.phase, "A");
        assert_eq!(va.units, "kV");
        assert_eq!(va.primary_ratio, 132.0);
        assert_eq!(va.secondary_ratio, 0.110);
        assert!(matches!(va.scaling, ScalingFlag::Primary));

        // Check digital channel details
        let trip = &cfg.digital_channels[0];
        assert_eq!(trip.name, "TRIP");
        assert_eq!(trip.normal_state, 0);
        let block = &cfg.digital_channels[3];
        assert_eq!(block.name, "BLOCK");
        assert_eq!(block.normal_state, 1);
    }

    #[test]
    fn test_parse_cfg_1991_no_rev_year() {
        let cfg_text = "\
STATION_B,DEVICE_2
2,1A,1D
1,IA,A,BR1,A,0.5,0.0,0.0,-32767,32767
2,STATUS,,BR1,0
60
1
1200,50
01/01/2020,00:00:00.000000
01/01/2020,00:00:00.005000
ASCII
";
        let cfg = parse_cfg(cfg_text).unwrap();
        assert_eq!(cfg.rev_year, RevYear::Y1991);
        assert_eq!(cfg.analog_channels.len(), 1);
        assert_eq!(cfg.digital_channels.len(), 1);
        assert_eq!(cfg.analog_channels[0].primary_ratio, 1.0); // default for 1991
    }
}
