// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! COMTRADE .cff (Combined File Format) parser — IEEE C37.111-2013.
//!
//! A CFF file contains all sections (cfg, dat, hdr, inf) in a single file,
//! delimited by section headers of the form `--- file type: xxx ---`.

use super::ComtradeError;
use super::cfg::parse_cfg;
use super::dat::parse_dat_ascii;
use super::types::*;
use std::collections::HashMap;

/// Parse a CFF combined file from a string.
pub fn parse_cff(text: &str) -> Result<ComtradeRecord, ComtradeError> {
    let mut cfg_section = String::new();
    let mut dat_section = String::new();
    let mut hdr_section = String::new();
    let mut inf_section = String::new();
    let mut current_section: Option<&str> = None;

    for line in text.lines() {
        let trimmed = line.trim().to_lowercase();
        if trimmed.starts_with("--- file type:") && trimmed.ends_with("---") {
            // Extract section type
            let inner = trimmed
                .trim_start_matches("--- file type:")
                .trim_end_matches("---")
                .trim();
            current_section = match inner {
                "cfg" => Some("cfg"),
                "dat" => Some("dat"),
                "hdr" => Some("hdr"),
                "inf" => Some("inf"),
                _ => None,
            };
            continue;
        }

        match current_section {
            Some("cfg") => {
                cfg_section.push_str(line);
                cfg_section.push('\n');
            }
            Some("dat") => {
                dat_section.push_str(line);
                dat_section.push('\n');
            }
            Some("hdr") => {
                hdr_section.push_str(line);
                hdr_section.push('\n');
            }
            Some("inf") => {
                inf_section.push_str(line);
                inf_section.push('\n');
            }
            _ => {}
        }
    }

    if cfg_section.is_empty() {
        return Err(ComtradeError::CffMissingSection("cfg"));
    }
    if dat_section.is_empty() {
        return Err(ComtradeError::CffMissingSection("dat"));
    }

    let cfg = parse_cfg(&cfg_section)?;

    // CFF only supports ASCII dat within the combined file
    if cfg.data_format != DataFormat::Ascii {
        return Err(ComtradeError::CffBinaryNotSupported);
    }

    let samples = parse_dat_ascii(&dat_section, &cfg.analog_channels, &cfg.digital_channels)?;

    let header_text = if hdr_section.trim().is_empty() {
        None
    } else {
        Some(hdr_section.trim().to_string())
    };

    let info = if inf_section.trim().is_empty() {
        None
    } else {
        Some(parse_inf(&inf_section))
    };

    Ok(ComtradeRecord {
        station_name: cfg.station_name,
        rec_dev_id: cfg.rec_dev_id,
        rev_year: cfg.rev_year,
        frequency: cfg.frequency,
        start_time: cfg.start_time,
        trigger_time: cfg.trigger_time,
        analog_channels: cfg.analog_channels,
        digital_channels: cfg.digital_channels,
        sample_rates: cfg.sample_rates,
        data_format: cfg.data_format,
        time_mult: cfg.time_mult,
        samples,
        header_text,
        info,
    })
}

/// Parse .inf content as key=value pairs.
pub(crate) fn parse_inf(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            map.insert(key.trim().to_string(), val.trim().to_string());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cff_basic() {
        let cff = "\
--- file type: cfg ---
SUB_X,DFR_1,2013
3,2A,1D
1,VA,A,LINE1,kV,1.0,0.0,0.0,-99999,99999,132.0,0.110,P
2,IA,A,LINE1,A,1.0,0.0,0.0,-99999,99999,800.0,1.0,P
3,TRIP,,LINE1,0
60
1
4000,4
01/01/2025,10:00:00.000000
01/01/2025,10:00:00.001000
ASCII
1.0
--- file type: hdr ---
Fault event on Line 1, Phase A to ground
--- file type: dat ---
1, 0, 100, 500, 0
2, 250, 110, 520, 0
3, 500, -200, 2500, 1
4, 750, -180, 2400, 1
--- file type: inf ---
relay_model=SEL-421
firmware=R150
";
        let rec = parse_cff(cff).unwrap();
        assert_eq!(rec.station_name, "SUB_X");
        assert_eq!(rec.rev_year, RevYear::Y2013);
        assert_eq!(rec.n_analog(), 2);
        assert_eq!(rec.n_digital(), 1);
        assert_eq!(rec.n_samples(), 4);
        assert_eq!(
            rec.header_text.as_deref(),
            Some("Fault event on Line 1, Phase A to ground")
        );
        let info = rec.info.as_ref().unwrap();
        assert_eq!(info.get("relay_model").unwrap(), "SEL-421");
        assert_eq!(info.get("firmware").unwrap(), "R150");
    }

    #[test]
    fn test_parse_cff_missing_cfg() {
        let cff = "\
--- file type: dat ---
1, 0, 100, 0
";
        let result = parse_cff(cff);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_inf() {
        let text = "key1=value1\nkey2 = value 2\n# comment\n\n;another comment\nkey3=val3";
        let map = parse_inf(text);
        assert_eq!(map.get("key1").unwrap(), "value1");
        assert_eq!(map.get("key2").unwrap(), "value 2");
        assert_eq!(map.get("key3").unwrap(), "val3");
        assert_eq!(map.len(), 3);
    }
}
