// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! COMTRADE data structures (IEEE C37.111-1991/1999/2013).

use std::collections::HashMap;
use std::fmt;

/// IEEE C37.111 revision year.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevYear {
    /// 1991 — original standard (no time_mult, fewer analog fields)
    Y1991,
    /// 1999 — added time_mult, primary/secondary scaling
    Y1999,
    /// 2013 — added nanosecond timestamps, CFF combined format
    Y2013,
}

impl fmt::Display for RevYear {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RevYear::Y1991 => write!(f, "1991"),
            RevYear::Y1999 => write!(f, "1999"),
            RevYear::Y2013 => write!(f, "2013"),
        }
    }
}

/// Data encoding format for the .dat file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFormat {
    /// ASCII CSV
    Ascii,
    /// 16-bit signed integers (IEEE C37.111 BINARY)
    Binary16,
    /// 32-bit signed integers (IEEE C37.111 BINARY32)
    Binary32,
    /// 32-bit IEEE 754 floats (IEEE C37.111 FLOAT32)
    Float32,
}

impl fmt::Display for DataFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataFormat::Ascii => write!(f, "ASCII"),
            DataFormat::Binary16 => write!(f, "BINARY"),
            DataFormat::Binary32 => write!(f, "BINARY32"),
            DataFormat::Float32 => write!(f, "FLOAT32"),
        }
    }
}

/// CT/VT scaling indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingFlag {
    /// Values are in primary units (e.g., kV, A)
    Primary,
    /// Values are in secondary units (CT/VT secondary)
    Secondary,
}

/// Analog channel definition from the .cfg file.
#[derive(Debug, Clone)]
pub struct AnalogChannel {
    /// 1-based channel index
    pub index: u32,
    /// Channel identifier (e.g., "VA", "IA")
    pub name: String,
    /// Phase identifier ("A", "B", "C", "N", "")
    pub phase: String,
    /// Circuit component name
    pub circuit_component: String,
    /// Engineering units ("kV", "A", "MW", etc.)
    pub units: String,
    /// Multiplier: `value = raw * multiplier + offset`
    pub multiplier: f64,
    /// Offset: `value = raw * multiplier + offset`
    pub offset: f64,
    /// Time skew in microseconds
    pub skew: f64,
    /// Minimum raw data value
    pub min_value: f64,
    /// Maximum raw data value
    pub max_value: f64,
    /// CT/VT primary ratio (1999/2013 only)
    pub primary_ratio: f64,
    /// CT/VT secondary ratio (1999/2013 only)
    pub secondary_ratio: f64,
    /// Primary or secondary scaling (1999/2013 only)
    pub scaling: ScalingFlag,
}

impl AnalogChannel {
    /// Convert a scaled engineering value back to raw: `raw = (value - offset) / multiplier`.
    pub fn to_raw(&self, value: f64) -> f64 {
        if self.multiplier == 0.0 {
            0.0
        } else {
            (value - self.offset) / self.multiplier
        }
    }
}

/// Digital (status) channel definition from the .cfg file.
#[derive(Debug, Clone)]
pub struct DigitalChannel {
    /// 1-based channel index
    pub index: u32,
    /// Channel identifier (e.g., "TRIP_52A")
    pub name: String,
    /// Phase identifier
    pub phase: String,
    /// Circuit component name
    pub circuit_component: String,
    /// Normal (non-operated) state: 0 or 1
    pub normal_state: u8,
}

/// Sampling rate specification.
#[derive(Debug, Clone, Copy)]
pub struct SampleRate {
    /// Sampling frequency in Hz (0 = non-uniform/event-driven)
    pub rate_hz: f64,
    /// Last sample number at this rate (1-based)
    pub last_sample: u32,
}

/// A single time-domain sample (one row in the .dat file).
#[derive(Debug, Clone)]
pub struct Sample {
    /// Sample number (1-based from .dat)
    pub number: u32,
    /// Timestamp in microseconds from recording start
    pub timestamp_us: f64,
    /// Analog channel values, scaled to engineering units
    pub analog: Vec<f64>,
    /// Digital channel values
    pub digital: Vec<bool>,
}

/// Timestamp from COMTRADE .cfg file.
///
/// Format: `dd/mm/yyyy,hh:mm:ss.ssssss` (1999) or nanosecond (2013).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComtradeTimestamp {
    pub day: u32,
    pub month: u32,
    pub year: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: f64,
}

impl ComtradeTimestamp {
    /// Total seconds since midnight.
    pub fn seconds_since_midnight(&self) -> f64 {
        self.hour as f64 * 3600.0 + self.minute as f64 * 60.0 + self.second
    }

    /// Format with microsecond precision (1991/1999).
    pub fn fmt_us(&self) -> String {
        let whole_sec = self.second as u32;
        let frac = self.second - whole_sec as f64;
        format!(
            "{:02}/{:02}/{:04},{:02}:{:02}:{:02}.{:06}",
            self.day,
            self.month,
            self.year,
            self.hour,
            self.minute,
            whole_sec,
            (frac * 1_000_000.0).round() as u64,
        )
    }

    /// Format with nanosecond precision (2013).
    pub fn fmt_ns(&self) -> String {
        let whole_sec = self.second as u32;
        let frac = self.second - whole_sec as f64;
        format!(
            "{:02}/{:02}/{:04},{:02}:{:02}:{:02}.{:09}",
            self.day,
            self.month,
            self.year,
            self.hour,
            self.minute,
            whole_sec,
            (frac * 1_000_000_000.0).round() as u64,
        )
    }
}

impl fmt::Display for ComtradeTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Default to microsecond precision (1999 format); callers needing
        // nanosecond precision for 2013 should use fmt_ns() directly.
        write!(f, "{}", self.fmt_us())
    }
}

/// A complete COMTRADE record (all data from .cfg + .dat + optional .hdr/.inf).
#[derive(Debug, Clone)]
pub struct ComtradeRecord {
    /// Station/substation name
    pub station_name: String,
    /// Recording device identifier
    pub rec_dev_id: String,
    /// IEEE C37.111 revision year
    pub rev_year: RevYear,
    /// System frequency in Hz (50 or 60)
    pub frequency: f64,
    /// Recording start time
    pub start_time: ComtradeTimestamp,
    /// Trigger (fault inception) time
    pub trigger_time: ComtradeTimestamp,
    /// Analog channel definitions
    pub analog_channels: Vec<AnalogChannel>,
    /// Digital channel definitions
    pub digital_channels: Vec<DigitalChannel>,
    /// Sampling rate specifications
    pub sample_rates: Vec<SampleRate>,
    /// Data file format
    pub data_format: DataFormat,
    /// Time multiplier (1999/2013; 1.0 for 1991)
    pub time_mult: f64,
    /// Waveform samples
    pub samples: Vec<Sample>,
    /// Optional header text (.hdr file content)
    pub header_text: Option<String>,
    /// Optional info key-value pairs (.inf file content)
    pub info: Option<HashMap<String, String>>,
}

impl ComtradeRecord {
    /// Number of analog channels.
    pub fn n_analog(&self) -> usize {
        self.analog_channels.len()
    }

    /// Number of digital channels.
    pub fn n_digital(&self) -> usize {
        self.digital_channels.len()
    }

    /// Number of samples.
    pub fn n_samples(&self) -> usize {
        self.samples.len()
    }

    /// Total recording duration in seconds.
    pub fn duration_s(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let last = &self.samples[self.samples.len() - 1];
        let first = &self.samples[0];
        (last.timestamp_us - first.timestamp_us) * self.time_mult * 1e-6
    }

    /// Time vector in seconds (relative to first sample).
    pub fn timestamps_s(&self) -> Vec<f64> {
        if self.samples.is_empty() {
            return vec![];
        }
        let t0 = self.samples[0].timestamp_us;
        self.samples
            .iter()
            .map(|s| (s.timestamp_us - t0) * self.time_mult * 1e-6)
            .collect()
    }

    /// Extract analog data for a single channel by 0-based index.
    pub fn analog_data(&self, channel_idx: usize) -> Vec<f64> {
        self.samples.iter().map(|s| s.analog[channel_idx]).collect()
    }

    /// Extract analog data for a channel by name (first match).
    pub fn analog_data_by_name(&self, name: &str) -> Option<Vec<f64>> {
        let idx = self.analog_channels.iter().position(|c| c.name == name)?;
        Some(self.analog_data(idx))
    }

    /// Extract digital data for a single channel by 0-based index.
    pub fn digital_data(&self, channel_idx: usize) -> Vec<bool> {
        self.samples
            .iter()
            .map(|s| s.digital[channel_idx])
            .collect()
    }

    /// Find the first sample where a digital channel transitions from its normal
    /// state, returning the timestamp in seconds relative to the first sample.
    ///
    /// If the channel is already in a non-normal state at the first sample, that
    /// sample's time (0.0) is returned — the transition occurred before recording
    /// started.
    pub fn digital_transition_time_s(&self, channel_name: &str) -> Option<f64> {
        let ch_idx = self
            .digital_channels
            .iter()
            .position(|c| c.name == channel_name)?;
        let normal = self.digital_channels[ch_idx].normal_state != 0;
        let t0 = self.samples.first()?.timestamp_us;
        for s in &self.samples {
            if s.digital[ch_idx] != normal {
                return Some((s.timestamp_us - t0) * self.time_mult * 1e-6);
            }
        }
        None
    }

    /// Validate internal consistency: channel counts match sample dimensions.
    pub fn validate(&self) -> Result<(), String> {
        let na = self.analog_channels.len();
        let nd = self.digital_channels.len();
        for (i, s) in self.samples.iter().enumerate() {
            if s.analog.len() != na {
                return Err(format!(
                    "sample {}: expected {} analog values, got {}",
                    s.number,
                    na,
                    s.analog.len()
                ));
            }
            if s.digital.len() != nd {
                return Err(format!(
                    "sample {}: expected {} digital values, got {}",
                    s.number,
                    nd,
                    s.digital.len()
                ));
            }
            if i > 0 && s.timestamp_us < self.samples[i - 1].timestamp_us {
                return Err(format!(
                    "sample {}: timestamp {} < previous {}",
                    s.number,
                    s.timestamp_us,
                    self.samples[i - 1].timestamp_us
                ));
            }
        }
        Ok(())
    }
}
