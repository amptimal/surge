// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! COMTRADE .dat (data) file parser.
//!
//! Supports ASCII, BINARY (16-bit), BINARY32, and FLOAT32 formats.

use super::ComtradeError;
use super::types::*;

/// Parse ASCII .dat content.
///
/// Each line: `sample_number, timestamp_µs, a1, a2, ..., d1, d2, ...`
/// Analog values are raw integers/floats; caller must scale with `a*multiplier + offset`.
pub fn parse_dat_ascii(
    text: &str,
    analog_channels: &[AnalogChannel],
    digital_channels: &[DigitalChannel],
) -> Result<Vec<Sample>, ComtradeError> {
    let n_analog = analog_channels.len();
    let n_digital = digital_channels.len();
    let expected_fields = 2 + n_analog + n_digital;

    let mut samples = Vec::new();

    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if fields.len() < expected_fields {
            return Err(ComtradeError::BadDatLine {
                line: line_no + 1,
                expected: expected_fields,
                got: fields.len(),
            });
        }

        let number: u32 = fields[0]
            .parse()
            .map_err(|_| ComtradeError::BadInt("sample_number", fields[0].to_string()))?;
        let timestamp_us: f64 = fields[1]
            .parse()
            .map_err(|_| ComtradeError::BadFloat("timestamp_us", fields[1].to_string()))?;

        let mut analog = Vec::with_capacity(n_analog);
        for (i, ch) in analog_channels.iter().enumerate() {
            let raw: f64 = fields[2 + i]
                .parse()
                .map_err(|_| ComtradeError::BadFloat("analog_value", fields[2 + i].to_string()))?;
            analog.push(raw * ch.multiplier + ch.offset);
        }

        let mut digital = Vec::with_capacity(n_digital);
        for i in 0..n_digital {
            let val: u8 = fields[2 + n_analog + i].parse().map_err(|_| {
                ComtradeError::BadInt("digital_value", fields[2 + n_analog + i].to_string())
            })?;
            digital.push(val != 0);
        }

        samples.push(Sample {
            number,
            timestamp_us,
            analog,
            digital,
        });
    }

    Ok(samples)
}

/// Parse BINARY (16-bit) .dat content.
///
/// Record layout per sample:
///   - u32 sample_number (4 bytes, little-endian)
///   - u32 timestamp_µs (4 bytes, little-endian)
///   - i16 × n_analog (2 bytes each, little-endian)
///   - u16 words for digital channels (ceil(n_digital/16) × 2 bytes, little-endian)
pub fn parse_dat_binary16(
    data: &[u8],
    analog_channels: &[AnalogChannel],
    digital_channels: &[DigitalChannel],
) -> Result<Vec<Sample>, ComtradeError> {
    let n_analog = analog_channels.len();
    let n_digital = digital_channels.len();
    let n_digital_words = n_digital.div_ceil(16);
    let record_size = 4 + 4 + n_analog * 2 + n_digital_words * 2;

    if !data.len().is_multiple_of(record_size) {
        return Err(ComtradeError::BadBinarySize {
            total: data.len(),
            record_size,
        });
    }

    let n_samples = data.len() / record_size;
    let mut samples = Vec::with_capacity(n_samples);

    for i in 0..n_samples {
        let offset = i * record_size;
        let (number, timestamp_us) = read_sample_header(data, offset);

        let mut analog = Vec::with_capacity(n_analog);
        for (j, ch) in analog_channels.iter().enumerate() {
            let a_off = offset + 8 + j * 2;
            let raw = i16::from_le_bytes([data[a_off], data[a_off + 1]]) as f64;
            analog.push(raw * ch.multiplier + ch.offset);
        }

        let digital =
            parse_digital_words(data, offset + 8 + n_analog * 2, n_digital_words, n_digital);

        samples.push(Sample {
            number,
            timestamp_us,
            analog,
            digital,
        });
    }

    Ok(samples)
}

/// Parse BINARY32 .dat content (32-bit signed integers).
pub fn parse_dat_binary32(
    data: &[u8],
    analog_channels: &[AnalogChannel],
    digital_channels: &[DigitalChannel],
) -> Result<Vec<Sample>, ComtradeError> {
    let n_analog = analog_channels.len();
    let n_digital = digital_channels.len();
    let n_digital_words = n_digital.div_ceil(16);
    let record_size = 4 + 4 + n_analog * 4 + n_digital_words * 2;

    if !data.len().is_multiple_of(record_size) {
        return Err(ComtradeError::BadBinarySize {
            total: data.len(),
            record_size,
        });
    }

    let n_samples = data.len() / record_size;
    let mut samples = Vec::with_capacity(n_samples);

    for i in 0..n_samples {
        let offset = i * record_size;
        let (number, timestamp_us) = read_sample_header(data, offset);

        let mut analog = Vec::with_capacity(n_analog);
        for (j, ch) in analog_channels.iter().enumerate() {
            let a_off = offset + 8 + j * 4;
            let raw = i32::from_le_bytes([
                data[a_off],
                data[a_off + 1],
                data[a_off + 2],
                data[a_off + 3],
            ]) as f64;
            analog.push(raw * ch.multiplier + ch.offset);
        }

        let digital =
            parse_digital_words(data, offset + 8 + n_analog * 4, n_digital_words, n_digital);

        samples.push(Sample {
            number,
            timestamp_us,
            analog,
            digital,
        });
    }

    Ok(samples)
}

/// Parse FLOAT32 .dat content (IEEE 754 32-bit floats).
pub fn parse_dat_float32(
    data: &[u8],
    analog_channels: &[AnalogChannel],
    digital_channels: &[DigitalChannel],
) -> Result<Vec<Sample>, ComtradeError> {
    let n_analog = analog_channels.len();
    let n_digital = digital_channels.len();
    let n_digital_words = n_digital.div_ceil(16);
    let record_size = 4 + 4 + n_analog * 4 + n_digital_words * 2;

    if !data.len().is_multiple_of(record_size) {
        return Err(ComtradeError::BadBinarySize {
            total: data.len(),
            record_size,
        });
    }

    let n_samples = data.len() / record_size;
    let mut samples = Vec::with_capacity(n_samples);

    for i in 0..n_samples {
        let offset = i * record_size;
        let (number, timestamp_us) = read_sample_header(data, offset);

        let mut analog = Vec::with_capacity(n_analog);
        for (j, ch) in analog_channels.iter().enumerate() {
            let a_off = offset + 8 + j * 4;
            let raw = f32::from_le_bytes([
                data[a_off],
                data[a_off + 1],
                data[a_off + 2],
                data[a_off + 3],
            ]) as f64;
            analog.push(raw * ch.multiplier + ch.offset);
        }

        let digital =
            parse_digital_words(data, offset + 8 + n_analog * 4, n_digital_words, n_digital);

        samples.push(Sample {
            number,
            timestamp_us,
            analog,
            digital,
        });
    }

    Ok(samples)
}

/// Read the 8-byte sample header (sample_number: u32, timestamp_us: u32).
fn read_sample_header(data: &[u8], offset: usize) -> (u32, f64) {
    let number = u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]);
    let timestamp_raw = u32::from_le_bytes([
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ]);
    (number, timestamp_raw as f64)
}

/// Extract digital channel booleans from packed 16-bit words.
fn parse_digital_words(
    data: &[u8],
    word_offset: usize,
    n_words: usize,
    n_digital: usize,
) -> Vec<bool> {
    let mut digital = Vec::with_capacity(n_digital);
    for w in 0..n_words {
        let w_off = word_offset + w * 2;
        let word = u16::from_le_bytes([data[w_off], data[w_off + 1]]);
        for bit in 0..16 {
            let ch_idx = w * 16 + bit;
            if ch_idx >= n_digital {
                break;
            }
            digital.push((word >> bit) & 1 != 0);
        }
    }
    digital
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_analog_channels(n: usize) -> Vec<AnalogChannel> {
        (0..n)
            .map(|i| AnalogChannel {
                index: (i + 1) as u32,
                name: format!("A{}", i + 1),
                phase: String::new(),
                circuit_component: String::new(),
                units: "V".to_string(),
                multiplier: 1.0,
                offset: 0.0,
                skew: 0.0,
                min_value: -32767.0,
                max_value: 32767.0,
                primary_ratio: 1.0,
                secondary_ratio: 1.0,
                scaling: ScalingFlag::Primary,
            })
            .collect()
    }

    fn make_digital_channels(n: usize) -> Vec<DigitalChannel> {
        (0..n)
            .map(|i| DigitalChannel {
                index: (i + 1) as u32,
                name: format!("D{}", i + 1),
                phase: String::new(),
                circuit_component: String::new(),
                normal_state: 0,
            })
            .collect()
    }

    #[test]
    fn test_parse_dat_ascii_basic() {
        let analog = make_analog_channels(2);
        let digital = make_digital_channels(1);
        let dat = "\
1, 0, 100, 200, 0
2, 250, 150, 250, 1
3, 500, -50, 300, 0
";
        let samples = parse_dat_ascii(dat, &analog, &digital).unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].number, 1);
        assert_eq!(samples[0].timestamp_us, 0.0);
        assert_eq!(samples[0].analog, vec![100.0, 200.0]);
        assert_eq!(samples[0].digital, vec![false]);
        assert_eq!(samples[1].digital, vec![true]);
        assert_eq!(samples[2].analog[0], -50.0);
    }

    #[test]
    fn test_parse_dat_ascii_with_scaling() {
        let mut analog = make_analog_channels(1);
        analog[0].multiplier = 0.5;
        analog[0].offset = 10.0;
        let digital = make_digital_channels(0);
        let dat = "1, 0, 100\n";
        let samples = parse_dat_ascii(dat, &analog, &digital).unwrap();
        // value = 100 * 0.5 + 10.0 = 60.0
        assert_eq!(samples[0].analog[0], 60.0);
    }

    #[test]
    fn test_parse_dat_binary16() {
        let analog = make_analog_channels(1);
        let digital = make_digital_channels(2);

        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&250u32.to_le_bytes());
        data.extend_from_slice(&1000i16.to_le_bytes());
        data.extend_from_slice(&0x0002u16.to_le_bytes());

        let samples = parse_dat_binary16(&data, &analog, &digital).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].number, 1);
        assert_eq!(samples[0].timestamp_us, 250.0);
        assert_eq!(samples[0].analog[0], 1000.0);
        assert_eq!(samples[0].digital, vec![false, true]);
    }

    #[test]
    fn test_parse_dat_binary32() {
        let analog = make_analog_channels(1);
        let digital = make_digital_channels(1);

        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&(-500i32).to_le_bytes());
        data.extend_from_slice(&0x0001u16.to_le_bytes());

        let samples = parse_dat_binary32(&data, &analog, &digital).unwrap();
        assert_eq!(samples[0].analog[0], -500.0);
        assert_eq!(samples[0].digital, vec![true]);
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn test_parse_dat_float32() {
        let analog = make_analog_channels(1);
        let digital = make_digital_channels(0);

        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        let test_val: f32 = 3.140_0;
        data.extend_from_slice(&test_val.to_le_bytes());

        let samples = parse_dat_float32(&data, &analog, &digital).unwrap();
        assert!((samples[0].analog[0] - f64::from(test_val)).abs() < 0.001);
    }
}
