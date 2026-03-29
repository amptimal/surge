// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! OpenDSS object catalog — strongly-typed structs for each DSS element type.
//!
//! Each struct implements `apply_property(&mut self, key, value)` to handle
//! the OpenDSS named-property syntax. Unknown properties are silently ignored
//! (OpenDSS convention — forward compatibility).

use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────────
// Helper: parse a space- or comma-separated list of f64 values.
// ─────────────────────────────────────────────────────────────────────────────

pub fn parse_f64_list(s: &str) -> Vec<f64> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f64>().ok())
        .collect()
}

pub fn parse_f64(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

pub fn parse_u32(s: &str) -> u32 {
    s.trim().parse::<u32>().unwrap_or(0)
}

pub fn parse_bool(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "yes" | "true" | "y" | "1")
}

/// OpenDSS length unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthUnit {
    Km,
    Mile,
    Meter,
    Ft,
    Kft,
    None,
}

impl LengthUnit {
    /// Multiply by this to convert to km.
    pub fn to_km_factor(self) -> f64 {
        match self {
            LengthUnit::Km => 1.0,
            LengthUnit::Mile => 1.609_344,
            LengthUnit::Meter => 0.001,
            LengthUnit::Ft => 0.000_304_8,
            LengthUnit::Kft => 0.304_8,
            LengthUnit::None => 1.0,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "km" => LengthUnit::Km,
            "mi" | "mile" | "miles" => LengthUnit::Mile,
            "m" | "meter" | "meters" | "metre" | "metres" => LengthUnit::Meter,
            "ft" | "feet" | "foot" => LengthUnit::Ft,
            "kft" => LengthUnit::Kft,
            _ => LengthUnit::None,
        }
    }
}

/// OpenDSS resistance/impedance unit.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum ImpedanceUnit {
    OhmPerKm,
    OhmPerMile,
    OhmPerFt,
    OhmPerKft,
    None,
}

impl ImpedanceUnit {
    /// Multiply by this to convert to Ω/km.
    #[allow(dead_code)]
    pub fn to_ohm_per_km(self) -> f64 {
        match self {
            ImpedanceUnit::OhmPerKm => 1.0,
            ImpedanceUnit::OhmPerMile => 1.0 / 1.609_344,
            ImpedanceUnit::OhmPerFt => 1000.0 / 0.304_8,
            ImpedanceUnit::OhmPerKft => 1000.0 / 304.8,
            ImpedanceUnit::None => 1.0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Circuit (source bus / slack bus definition)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CircuitData {
    pub name: String,
    /// Base kV (line-to-line for 3-phase).
    pub base_kv: f64,
    /// Source voltage per-unit magnitude.
    pub pu: f64,
    /// Source voltage angle in degrees.
    pub angle_deg: f64,
    /// System frequency in Hz.
    pub frequency: f64,
    /// Number of phases.
    pub phases: u8,
    /// Source bus name (default: "SourceBus").
    pub bus: String,
    /// Short-circuit MVA (zero-sequence).
    pub mvasc1: f64,
    pub mvasc3: f64,
}

impl Default for CircuitData {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_kv: 115.0,
            pu: 1.0,
            angle_deg: 0.0,
            frequency: 60.0,
            phases: 3,
            bus: "SourceBus".to_string(),
            mvasc1: 2100.0,
            mvasc3: 2100.0,
        }
    }
}

impl CircuitData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "basekv" => self.base_kv = parse_f64(value),
            "pu" => self.pu = parse_f64(value),
            "angle" => self.angle_deg = parse_f64(value),
            "frequency" => self.frequency = parse_f64(value),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "bus1" | "bus" => self.bus = value.trim().to_string(),
            "mvasc1" => self.mvasc1 = parse_f64(value),
            "mvasc3" => self.mvasc3 = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LineCode
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LineCodeData {
    pub name: String,
    pub phases: u8,
    /// Positive-sequence resistance Ω/km.
    pub r1: f64,
    /// Positive-sequence reactance Ω/km.
    pub x1: f64,
    /// Zero-sequence resistance Ω/km.
    pub r0: f64,
    /// Zero-sequence reactance Ω/km.
    pub x0: f64,
    /// Positive-sequence capacitive susceptance µS/km.
    pub c1: f64,
    /// Zero-sequence capacitive susceptance µS/km.
    pub c0: f64,
    /// Full 3×3 resistance matrix (row-major, Ω/km). Empty = use r1/r0.
    pub rmatrix: Vec<f64>,
    /// Full 3×3 reactance matrix (row-major, Ω/km). Empty = use x1/x0.
    pub xmatrix: Vec<f64>,
    /// Full 3×3 capacitance matrix (row-major, nF/km). Empty = use c1/c0.
    pub cmatrix: Vec<f64>,
    /// Units for r/x values.
    pub units: LengthUnit,
    /// Neutral resistance (Ω/km).
    pub rn: f64,
    /// Neutral reactance (Ω/km).
    pub xn: f64,
    /// Neutral GMR (metres).
    pub gmr_n: f64,
}

impl Default for LineCodeData {
    fn default() -> Self {
        Self {
            name: String::new(),
            phases: 3,
            r1: 0.0,
            x1: 0.0,
            r0: 0.0,
            x0: 0.0,
            c1: 0.0,
            c0: 0.0,
            rmatrix: Vec::new(),
            xmatrix: Vec::new(),
            cmatrix: Vec::new(),
            units: LengthUnit::Km,
            rn: 0.0,
            xn: 0.0,
            gmr_n: 0.0,
        }
    }
}

impl LineCodeData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "nphases" | "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "r1" => self.r1 = parse_f64(value),
            "x1" => self.x1 = parse_f64(value),
            "r0" => self.r0 = parse_f64(value),
            "x0" => self.x0 = parse_f64(value),
            "c1" => self.c1 = parse_f64(value),
            "c0" => self.c0 = parse_f64(value),
            "rmatrix" => self.rmatrix = parse_f64_list(value),
            "xmatrix" => self.xmatrix = parse_f64_list(value),
            "cmatrix" => self.cmatrix = parse_f64_list(value),
            "units" => self.units = LengthUnit::from_str(value),
            "rn" => self.rn = parse_f64(value),
            "xn" => self.xn = parse_f64(value),
            "gmr" | "gmrn" => self.gmr_n = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Line
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LineData {
    pub name: String,
    /// From bus (may include phase spec like "650.1.2.3").
    pub bus1: String,
    /// To bus.
    pub bus2: String,
    pub phases: u8,
    /// LineCode reference name (if any).
    pub linecode: String,
    /// Geometry reference name (if any).
    pub geometry: String,
    /// Line length (in `units`).
    pub length: f64,
    pub units: LengthUnit,
    /// Positive-sequence resistance (Ω/km after conversion).
    pub r1: f64,
    pub x1: f64,
    pub r0: f64,
    pub x0: f64,
    pub c1: f64,
    pub c0: f64,
    /// Full impedance matrix (row-major, Ω/km).
    pub rmatrix: Vec<f64>,
    pub xmatrix: Vec<f64>,
    pub cmatrix: Vec<f64>,
    /// Is this element a switch?
    pub is_switch: bool,
    /// Fault rate (faults per year).
    pub fault_rate: f64,
    /// Percent permanent faults.
    pub pct_perm: f64,
    /// Repair time in hours.
    pub repair: f64,
    /// Normal ampacity rating in A.
    pub norm_amps: f64,
    /// Emergency ampacity in A.
    pub emerg_amps: f64,
}

impl Default for LineData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            bus2: String::new(),
            phases: 3,
            linecode: String::new(),
            geometry: String::new(),
            length: 1.0,
            units: LengthUnit::None,
            r1: 0.001,
            x1: 0.001,
            r0: 0.0,
            x0: 0.0,
            c1: 0.0,
            c0: 0.0,
            rmatrix: Vec::new(),
            xmatrix: Vec::new(),
            cmatrix: Vec::new(),
            is_switch: false,
            fault_rate: 0.1,
            pct_perm: 20.0,
            repair: 3.0,
            norm_amps: 400.0,
            emerg_amps: 600.0,
        }
    }
}

impl LineData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "bus2" => self.bus2 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "linecode" => self.linecode = value.trim().to_string(),
            "geometry" => self.geometry = value.trim().to_string(),
            "length" => self.length = parse_f64(value),
            "units" => self.units = LengthUnit::from_str(value),
            "r1" => self.r1 = parse_f64(value),
            "x1" => self.x1 = parse_f64(value),
            "r0" => self.r0 = parse_f64(value),
            "x0" => self.x0 = parse_f64(value),
            "c1" => self.c1 = parse_f64(value),
            "c0" => self.c0 = parse_f64(value),
            "rmatrix" => self.rmatrix = parse_f64_list(value),
            "xmatrix" => self.xmatrix = parse_f64_list(value),
            "cmatrix" => self.cmatrix = parse_f64_list(value),
            "switch" => self.is_switch = parse_bool(value),
            "faultrate" => self.fault_rate = parse_f64(value),
            "pctperm" => self.pct_perm = parse_f64(value),
            "repair" => self.repair = parse_f64(value),
            "normamps" => self.norm_amps = parse_f64(value),
            "emergamps" => self.emerg_amps = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LineGeometry
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LineGeometryData {
    pub name: String,
    pub n_conds: usize,
    pub n_phases: usize,
    pub x: Vec<f64>,
    pub h: Vec<f64>,
    pub wire: Vec<String>,
    pub units: LengthUnit,
}

impl Default for LineGeometryData {
    fn default() -> Self {
        Self {
            name: String::new(),
            n_conds: 0,
            n_phases: 3,
            x: Vec::new(),
            h: Vec::new(),
            wire: Vec::new(),
            units: LengthUnit::Ft,
        }
    }
}

impl LineGeometryData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "nconds" => {
                self.n_conds = value.trim().parse().unwrap_or(0);
                self.x.resize(self.n_conds, 0.0);
                self.h.resize(self.n_conds, 0.0);
                self.wire.resize(self.n_conds, String::new());
            }
            "nphases" => self.n_phases = value.trim().parse().unwrap_or(3),
            "x" => {
                // `cond=N x=val` sets the N-th conductor's x.
                // We handle this positionally: the most recent `cond` sets the index.
                let vals = parse_f64_list(value);
                for (i, v) in vals.into_iter().enumerate() {
                    if i < self.x.len() {
                        self.x[i] = v;
                    }
                }
            }
            "h" => {
                let vals = parse_f64_list(value);
                for (i, v) in vals.into_iter().enumerate() {
                    if i < self.h.len() {
                        self.h[i] = v;
                    }
                }
            }
            "wire" => {
                let vals: Vec<&str> = value.split_whitespace().collect();
                for (i, v) in vals.into_iter().enumerate() {
                    if i < self.wire.len() {
                        self.wire[i] = v.to_string();
                    }
                }
            }
            "units" => self.units = LengthUnit::from_str(value),
            _ => {}
        }
    }

    /// Apply a positional/indexed property: `cond=N` followed by `x=`, `h=`, `wire=`.
    pub fn set_cond_x(&mut self, cond_idx: usize, x: f64) {
        while self.x.len() <= cond_idx {
            self.x.push(0.0);
        }
        self.x[cond_idx] = x;
    }

    pub fn set_cond_h(&mut self, cond_idx: usize, h: f64) {
        while self.h.len() <= cond_idx {
            self.h.push(0.0);
        }
        self.h[cond_idx] = h;
    }

    pub fn set_cond_wire(&mut self, cond_idx: usize, wire: &str) {
        while self.wire.len() <= cond_idx {
            self.wire.push(String::new());
        }
        self.wire[cond_idx] = wire.to_string();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WireData (overhead wire)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WireDataEntry {
    pub name: String,
    pub gmr: f64,
    pub gmr_units: LengthUnit,
    pub radius: f64,
    pub radius_units: LengthUnit,
    pub rac: f64, // Ω/km
    pub rdc: f64, // Ω/km
    pub r_units: LengthUnit,
    pub ampacity: f64,
    pub norm_amps: f64,
}

impl Default for WireDataEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            gmr: 0.01,
            gmr_units: LengthUnit::Meter,
            radius: 0.01,
            radius_units: LengthUnit::Meter,
            rac: 0.0,
            rdc: 0.0,
            r_units: LengthUnit::Km,
            ampacity: 0.0,
            norm_amps: 0.0,
        }
    }
}

impl WireDataEntry {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "gmr" => self.gmr = parse_f64(value),
            "gmrunits" => self.gmr_units = LengthUnit::from_str(value),
            "radius" => self.radius = parse_f64(value),
            "radiusunits" => self.radius_units = LengthUnit::from_str(value),
            "rac" => self.rac = parse_f64(value),
            "rdc" => self.rdc = parse_f64(value),
            "runits" => self.r_units = LengthUnit::from_str(value),
            "ampacity" | "normamps" => {
                let v = parse_f64(value);
                self.ampacity = v;
                self.norm_amps = v;
            }
            _ => {}
        }
    }

    /// GMR in metres.
    #[allow(dead_code)]
    pub fn gmr_m(&self) -> f64 {
        self.gmr * self.gmr_units.to_km_factor() * 1000.0
    }

    /// Radius in metres.
    #[allow(dead_code)]
    pub fn radius_m(&self) -> f64 {
        self.radius * self.radius_units.to_km_factor() * 1000.0
    }

    /// Rac in Ω/km.
    #[allow(dead_code)]
    pub fn rac_ohm_per_km(&self) -> f64 {
        // r_units gives the length denominator unit.
        // Multiply by the inverse of the length factor to get Ω/km.
        let factor = self.r_units.to_km_factor();
        if factor > 0.0 {
            self.rac / factor
        } else {
            self.rac
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CNData (concentric neutral cable)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CnDataEntry {
    pub name: String,
    pub gmr: f64,
    pub gmr_units: LengthUnit,
    pub radius: f64,
    pub radius_units: LengthUnit,
    pub rac: f64,
    pub r_units: LengthUnit,
    pub insulation_thickness: f64,
    pub ins_units: LengthUnit,
    pub k: u32, // neutral strand count
    pub gmr_strand: f64,
    pub gmr_strand_units: LengthUnit,
    pub radius_strand: f64,
    pub radius_strand_units: LengthUnit,
    pub rac_strand: f64,
    pub r_strand_units: LengthUnit,
}

impl Default for CnDataEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            gmr: 0.005,
            gmr_units: LengthUnit::Meter,
            radius: 0.01,
            radius_units: LengthUnit::Meter,
            rac: 0.0,
            r_units: LengthUnit::Km,
            insulation_thickness: 0.005,
            ins_units: LengthUnit::Meter,
            k: 13,
            gmr_strand: 0.001,
            gmr_strand_units: LengthUnit::Meter,
            radius_strand: 0.001,
            radius_strand_units: LengthUnit::Meter,
            rac_strand: 0.0,
            r_strand_units: LengthUnit::Km,
        }
    }
}

impl CnDataEntry {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "gmr" => self.gmr = parse_f64(value),
            "gmrunits" => self.gmr_units = LengthUnit::from_str(value),
            "radius" => self.radius = parse_f64(value),
            "radiusunits" => self.radius_units = LengthUnit::from_str(value),
            "rac" => self.rac = parse_f64(value),
            "runits" => self.r_units = LengthUnit::from_str(value),
            "insulationthickness" | "insthickness" => {
                self.insulation_thickness = parse_f64(value);
            }
            "insunits" => self.ins_units = LengthUnit::from_str(value),
            "k" => self.k = parse_u32(value),
            "gmrstrand" => self.gmr_strand = parse_f64(value),
            "gmrstrandunits" => self.gmr_strand_units = LengthUnit::from_str(value),
            "radiusstrand" | "rstrand" => self.radius_strand = parse_f64(value),
            "radiusstrandunits" => self.radius_strand_units = LengthUnit::from_str(value),
            "racstrand" => self.rac_strand = parse_f64(value),
            "rstrandunits" => self.r_strand_units = LengthUnit::from_str(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TSData (tape shield cable)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TsDataEntry {
    pub name: String,
    pub gmr: f64,
    pub gmr_units: LengthUnit,
    pub radius: f64,
    pub radius_units: LengthUnit,
    pub rac: f64,
    pub r_units: LengthUnit,
    pub insulation_thickness: f64,
    pub ins_units: LengthUnit,
    pub tape_thickness: f64,
    pub tape_units: LengthUnit,
    pub tape_lap: f64,
}

impl Default for TsDataEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            gmr: 0.005,
            gmr_units: LengthUnit::Meter,
            radius: 0.01,
            radius_units: LengthUnit::Meter,
            rac: 0.0,
            r_units: LengthUnit::Km,
            insulation_thickness: 0.005,
            ins_units: LengthUnit::Meter,
            tape_thickness: 0.0002,
            tape_units: LengthUnit::Meter,
            tape_lap: 20.0,
        }
    }
}

impl TsDataEntry {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "gmr" => self.gmr = parse_f64(value),
            "gmrunits" => self.gmr_units = LengthUnit::from_str(value),
            "radius" => self.radius = parse_f64(value),
            "radiusunits" => self.radius_units = LengthUnit::from_str(value),
            "rac" => self.rac = parse_f64(value),
            "runits" => self.r_units = LengthUnit::from_str(value),
            "insulationthickness" | "insthickness" => {
                self.insulation_thickness = parse_f64(value);
            }
            "insunits" => self.ins_units = LengthUnit::from_str(value),
            "tapethickness" => self.tape_thickness = parse_f64(value),
            "tapeunits" => self.tape_units = LengthUnit::from_str(value),
            "tapelap" => self.tape_lap = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Transformer
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum WdgConn {
    Wye,
    Delta,
    Ln, // same as Wye-grounded in DSS
}

impl WdgConn {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "delta" | "d" => WdgConn::Delta,
            "ln" | "wyeg" | "wye-g" | "wye_g" => WdgConn::Ln,
            _ => WdgConn::Wye,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransformerData {
    pub name: String,
    pub phases: u8,
    /// Number of windings (2 or 3).
    pub windings: u8,
    /// Bus for each winding (may include phase spec).
    pub buses: Vec<String>,
    /// Connection type for each winding.
    pub conns: Vec<WdgConn>,
    /// kV for each winding.
    pub kvs: Vec<f64>,
    /// kVA for each winding.
    pub kvas: Vec<f64>,
    /// %R (resistance) for each winding.
    pub pct_rs: Vec<f64>,
    /// High-to-low leakage reactance %.
    pub xhl: f64,
    /// High-to-tertiary leakage reactance %.
    pub xht: f64,
    /// Low-to-tertiary leakage reactance %.
    pub xlt: f64,
    /// Load-loss %.
    pub pct_load_loss: f64,
    /// No-load loss %.
    pub pct_no_load_loss: f64,
    /// Magnetizing current % of rated kVA.
    pub pct_imag: f64,
    /// Winding tap ratios (1.0 = nominal).
    pub taps: Vec<f64>,
    /// XfmrCode reference (overrides explicit values).
    pub xfmrcode: String,
    /// Currently addressed winding (1-based, set by `wdg=N`).
    pub active_wdg: usize,
    /// Normal ampacity rating in A.
    pub norm_amps: f64,
    pub emerg_amps: f64,
}

impl Default for TransformerData {
    fn default() -> Self {
        Self {
            name: String::new(),
            phases: 3,
            windings: 2,
            buses: vec![String::new(), String::new()],
            conns: vec![WdgConn::Wye, WdgConn::Wye],
            kvs: vec![115.0, 12.47],
            kvas: vec![1000.0, 1000.0],
            pct_rs: vec![0.5, 0.5],
            xhl: 7.0,
            xht: 35.0,
            xlt: 30.0,
            pct_load_loss: 0.0,
            pct_no_load_loss: 0.0,
            pct_imag: 0.0,
            taps: vec![1.0, 1.0],
            xfmrcode: String::new(),
            active_wdg: 1,
            norm_amps: 0.0,
            emerg_amps: 0.0,
        }
    }
}

impl TransformerData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "windings" => {
                let w: usize = value.trim().parse().unwrap_or(2);
                self.windings = w as u8;
                self.buses.resize(w, String::new());
                self.conns.resize(w, WdgConn::Wye);
                self.kvs.resize(w, 0.0);
                self.kvas.resize(w, 0.0);
                self.pct_rs.resize(w, 0.5);
                self.taps.resize(w, 1.0);
            }
            "wdg" => {
                self.active_wdg = value.trim().parse().unwrap_or(1);
            }
            "bus" | "buses" => {
                if value.contains(' ') || value.contains(',') {
                    let buses: Vec<&str> = value
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .filter(|s| !s.is_empty())
                        .collect();
                    // Resize to fit all bus specs (handles 3-winding transformers
                    // where buses=[...] has 3 entries but default array length is 2)
                    if buses.len() > self.buses.len() {
                        self.buses.resize(buses.len(), String::new());
                    }
                    for (i, b) in buses.into_iter().enumerate() {
                        self.buses[i] = b.to_string();
                    }
                } else {
                    // Single bus = applies to active winding.
                    let wdg = (self.active_wdg - 1).min(self.buses.len().saturating_sub(1));
                    self.buses[wdg] = value.trim().to_string();
                }
            }
            "conn" | "conns" => {
                if value.contains('[') || value.contains(' ') || value.contains(',') {
                    let vals: Vec<WdgConn> = value
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .filter(|s| !s.is_empty())
                        .map(WdgConn::from_str)
                        .collect();
                    for (i, c) in vals.into_iter().enumerate() {
                        if i < self.conns.len() {
                            self.conns[i] = c;
                        }
                    }
                } else {
                    let wdg = (self.active_wdg - 1).min(self.conns.len().saturating_sub(1));
                    self.conns[wdg] = WdgConn::from_str(value);
                }
            }
            "kv" | "kvs" => {
                let vals = parse_f64_list(value);
                if vals.len() == 1 {
                    let wdg = (self.active_wdg - 1).min(self.kvs.len().saturating_sub(1));
                    self.kvs[wdg] = vals[0];
                } else {
                    for (i, v) in vals.into_iter().enumerate() {
                        if i < self.kvs.len() {
                            self.kvs[i] = v;
                        }
                    }
                }
            }
            "kva" | "kvas" => {
                let vals = parse_f64_list(value);
                if vals.len() == 1 {
                    let wdg = (self.active_wdg - 1).min(self.kvas.len().saturating_sub(1));
                    self.kvas[wdg] = vals[0];
                } else {
                    for (i, v) in vals.into_iter().enumerate() {
                        if i < self.kvas.len() {
                            self.kvas[i] = v;
                        }
                    }
                }
            }
            "%r" | "%rs" | "pctrs" | "xrconst" => {
                let vals = parse_f64_list(value);
                if vals.len() == 1 {
                    let wdg = (self.active_wdg - 1).min(self.pct_rs.len().saturating_sub(1));
                    self.pct_rs[wdg] = vals[0];
                } else {
                    for (i, v) in vals.into_iter().enumerate() {
                        if i < self.pct_rs.len() {
                            self.pct_rs[i] = v;
                        }
                    }
                }
            }
            "xhl" => self.xhl = parse_f64(value),
            "xht" => self.xht = parse_f64(value),
            "xlt" => self.xlt = parse_f64(value),
            "%loadloss" | "pctloadloss" => self.pct_load_loss = parse_f64(value),
            "%noloadloss" | "pctnoloadloss" => self.pct_no_load_loss = parse_f64(value),
            "%imag" | "pctimag" => self.pct_imag = parse_f64(value),
            "tap" | "taps" => {
                let vals = parse_f64_list(value);
                if vals.len() == 1 {
                    let wdg = (self.active_wdg - 1).min(self.taps.len().saturating_sub(1));
                    self.taps[wdg] = vals[0];
                } else {
                    for (i, v) in vals.into_iter().enumerate() {
                        if i < self.taps.len() {
                            self.taps[i] = v;
                        }
                    }
                }
            }
            "xfmrcode" => self.xfmrcode = value.trim().to_string(),
            "normamps" => self.norm_amps = parse_f64(value),
            "emergamps" => self.emerg_amps = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AutoTrans
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct AutoTransData {
    pub transformer: TransformerData,
}

impl AutoTransData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        self.transformer.apply_property(key, value);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// XfmrCode (transformer type library)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct XfmrCodeData {
    pub name: String,
    pub phases: u8,
    pub windings: u8,
    pub conns: Vec<WdgConn>,
    pub kvs: Vec<f64>,
    pub kvas: Vec<f64>,
    pub pct_rs: Vec<f64>,
    pub xhl: f64,
    pub xht: f64,
    pub xlt: f64,
    pub pct_load_loss: f64,
    pub pct_no_load_loss: f64,
    pub pct_imag: f64,
}

impl Default for XfmrCodeData {
    fn default() -> Self {
        Self {
            name: String::new(),
            phases: 3,
            windings: 2,
            conns: vec![WdgConn::Wye, WdgConn::Wye],
            kvs: vec![115.0, 12.47],
            kvas: vec![1000.0, 1000.0],
            pct_rs: vec![0.5, 0.5],
            xhl: 7.0,
            xht: 35.0,
            xlt: 30.0,
            pct_load_loss: 0.0,
            pct_no_load_loss: 0.0,
            pct_imag: 0.0,
        }
    }
}

impl XfmrCodeData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        // Shares most properties with TransformerData.
        match key.to_lowercase().as_str() {
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "windings" => {
                let w: usize = value.trim().parse().unwrap_or(2);
                self.windings = w as u8;
                self.conns.resize(w, WdgConn::Wye);
                self.kvs.resize(w, 0.0);
                self.kvas.resize(w, 0.0);
                self.pct_rs.resize(w, 0.5);
            }
            "kv" | "kvs" => {
                for (i, v) in parse_f64_list(value).into_iter().enumerate() {
                    if i < self.kvs.len() {
                        self.kvs[i] = v;
                    }
                }
            }
            "kva" | "kvas" => {
                for (i, v) in parse_f64_list(value).into_iter().enumerate() {
                    if i < self.kvas.len() {
                        self.kvas[i] = v;
                    }
                }
            }
            "xhl" => self.xhl = parse_f64(value),
            "xht" => self.xht = parse_f64(value),
            "xlt" => self.xlt = parse_f64(value),
            "%loadloss" => self.pct_load_loss = parse_f64(value),
            "%noloadloss" => self.pct_no_load_loss = parse_f64(value),
            "%imag" | "pctimag" => self.pct_imag = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Load
// ─────────────────────────────────────────────────────────────────────────────

/// OpenDSS load models 1–8.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadModel {
    ConstantPQ = 1,
    ConstantZ = 2,
    ConstantPConstantQ = 3,
    ConstantPFixedQ = 4,
    ConstantPConstantXQ = 5,
    Kron = 6,
    Cvr = 7,
    ConstantZ2 = 8,
}

impl LoadModel {
    pub fn from_u32(n: u32) -> Self {
        match n {
            2 => LoadModel::ConstantZ,
            3 => LoadModel::ConstantPConstantQ,
            4 => LoadModel::ConstantPFixedQ,
            5 => LoadModel::ConstantPConstantXQ,
            6 => LoadModel::Kron,
            7 => LoadModel::Cvr,
            8 => LoadModel::ConstantZ2,
            _ => LoadModel::ConstantPQ,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadData {
    pub name: String,
    pub bus1: String,
    pub phases: u8,
    /// Base voltage (kV, line-to-line for 3ph, line-to-neutral for 1ph).
    pub kv: f64,
    /// Real power in kW.
    pub kw: f64,
    /// Reactive power in kVAr.
    pub kvar: f64,
    /// Power factor (used when kvar not specified).
    pub pf: f64,
    pub model: LoadModel,
    /// ZIP coefficients [Zp Ip Pp Zq Iq Pq] for models 8/ZIP.
    pub zipv: Vec<f64>,
    /// Daily load shape name.
    pub daily: String,
    /// Yearly load shape name.
    pub yearly: String,
    /// Duty cycle load shape name.
    pub duty: String,
    pub norm_amps: f64,
    pub emerg_amps: f64,
    /// CVR factor for real power (sensitivity of kW to voltage).
    pub cvrf_watt: f64,
    /// CVR factor for reactive power.
    pub cvrf_var: f64,
    /// Connection type (wye or delta).
    pub conn: WdgConn,
}

impl Default for LoadData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            phases: 3,
            kv: 12.47,
            kw: 0.0,
            kvar: 0.0,
            pf: 0.88,
            model: LoadModel::ConstantPQ,
            zipv: Vec::new(),
            daily: String::new(),
            yearly: String::new(),
            duty: String::new(),
            norm_amps: 0.0,
            emerg_amps: 0.0,
            cvrf_watt: 1.0,
            cvrf_var: 2.0,
            conn: WdgConn::Wye,
        }
    }
}

impl LoadData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "kv" => self.kv = parse_f64(value),
            "kw" => self.kw = parse_f64(value),
            "kvar" => self.kvar = parse_f64(value),
            "pf" => {
                self.pf = parse_f64(value);
                // If kvar not explicitly given yet, compute it.
                if self.kvar == 0.0 && self.pf > 0.0 && self.kw > 0.0 {
                    let sin = (1.0 - self.pf * self.pf).sqrt();
                    self.kvar = self.kw * sin / self.pf;
                }
            }
            "model" => self.model = LoadModel::from_u32(parse_u32(value)),
            "zipv" => self.zipv = parse_f64_list(value),
            "daily" => self.daily = value.trim().to_string(),
            "yearly" => self.yearly = value.trim().to_string(),
            "duty" => self.duty = value.trim().to_string(),
            "normamps" => self.norm_amps = parse_f64(value),
            "emergamps" => self.emerg_amps = parse_f64(value),
            "cvrwatts" | "cvrf_watt" => self.cvrf_watt = parse_f64(value),
            "cvrvars" | "cvrf_var" => self.cvrf_var = parse_f64(value),
            "conn" => self.conn = WdgConn::from_str(value),
            _ => {}
        }
    }

    /// Reactive power in kVAr, computed from kW and pf if not given explicitly.
    pub fn effective_kvar(&self) -> f64 {
        if self.kvar != 0.0 {
            self.kvar
        } else if self.pf > 0.0 && self.pf < 1.0 {
            let sin = (1.0 - self.pf * self.pf).sqrt();
            self.kw * sin / self.pf
        } else {
            0.0
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Generator
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GeneratorData {
    pub name: String,
    pub bus1: String,
    pub phases: u8,
    pub kv: f64,
    pub kw: f64,
    pub kvar: f64,
    pub pf: f64,
    pub kva: f64,
    pub kw_max: f64,
    pub kw_min: f64,
    pub kvar_max: f64,
    pub kvar_min: f64,
    pub daily: String,
    pub yearly: String,
    pub duty: String,
    pub vminpu: f64,
    pub vmaxpu: f64,
}

impl Default for GeneratorData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            phases: 3,
            kv: 12.47,
            kw: 0.0,
            kvar: 0.0,
            pf: 1.0,
            kva: 0.0,
            kw_max: 1000.0,
            kw_min: 0.0,
            kvar_max: 1000.0,
            kvar_min: -1000.0,
            daily: String::new(),
            yearly: String::new(),
            duty: String::new(),
            vminpu: 0.9,
            vmaxpu: 1.1,
        }
    }
}

impl GeneratorData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "kv" => self.kv = parse_f64(value),
            "kw" => self.kw = parse_f64(value),
            "kvar" => self.kvar = parse_f64(value),
            "pf" => self.pf = parse_f64(value),
            "kva" => self.kva = parse_f64(value),
            "maxkw" | "kwmax" => self.kw_max = parse_f64(value),
            "minkw" | "kwmin" => self.kw_min = parse_f64(value),
            "maxkvar" | "kvarmax" => self.kvar_max = parse_f64(value),
            "minkvar" | "kvarmin" => self.kvar_min = parse_f64(value),
            "daily" => self.daily = value.trim().to_string(),
            "yearly" => self.yearly = value.trim().to_string(),
            "duty" => self.duty = value.trim().to_string(),
            "vminpu" => self.vminpu = parse_f64(value),
            "vmaxpu" => self.vmaxpu = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PVSystem
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PvSystemData {
    pub name: String,
    pub bus1: String,
    pub phases: u8,
    pub kv: f64,
    pub kva: f64,
    pub kw_max: f64,
    pub pf: f64,
    pub pmpp: f64, // rated peak power in kW at STC
    pub irradiance: f64,
    pub daily: String,
    pub yearly: String,
}

impl Default for PvSystemData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            phases: 3,
            kv: 12.47,
            kva: 500.0,
            kw_max: 500.0,
            pf: 1.0,
            pmpp: 500.0,
            irradiance: 1.0,
            daily: String::new(),
            yearly: String::new(),
        }
    }
}

impl PvSystemData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "kv" => self.kv = parse_f64(value),
            "kva" => self.kva = parse_f64(value),
            "kwrated" | "kwmax" | "pmpp" => {
                let v = parse_f64(value);
                self.kw_max = v;
                self.pmpp = v;
            }
            "pf" => self.pf = parse_f64(value),
            "irradiance" => self.irradiance = parse_f64(value),
            "daily" => self.daily = value.trim().to_string(),
            "yearly" => self.yearly = value.trim().to_string(),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Storage
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StorageData {
    pub name: String,
    pub bus1: String,
    pub phases: u8,
    pub kv: f64,
    pub kva: f64,
    pub kw_rated: f64,
    pub kwh_rated: f64,
    pub kwh_stored: f64,
    pub pct_stored: f64,
    pub pct_charge: f64,
    pub pf: f64,
    pub daily: String,
}

impl Default for StorageData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            phases: 3,
            kv: 12.47,
            kva: 25.0,
            kw_rated: 25.0,
            kwh_rated: 50.0,
            kwh_stored: 50.0,
            pct_stored: 100.0,
            pct_charge: 100.0,
            pf: 1.0,
            daily: String::new(),
        }
    }
}

impl StorageData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "kv" => self.kv = parse_f64(value),
            "kva" => self.kva = parse_f64(value),
            "kwrated" | "kw" => self.kw_rated = parse_f64(value),
            "kwhrated" | "kwh" => self.kwh_rated = parse_f64(value),
            "kwhstored" => self.kwh_stored = parse_f64(value),
            "%stored" | "pctstored" => self.pct_stored = parse_f64(value),
            "%charge" | "pctcharge" => self.pct_charge = parse_f64(value),
            "pf" => self.pf = parse_f64(value),
            "daily" => self.daily = value.trim().to_string(),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Capacitor
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CapacitorData {
    pub name: String,
    pub bus1: String,
    pub bus2: String,
    pub phases: u8,
    /// kVAr per step (can have multiple steps).
    pub kvar: Vec<f64>,
    pub kv: f64,
    pub conn: WdgConn,
}

impl Default for CapacitorData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            bus2: String::new(),
            phases: 3,
            kvar: vec![600.0],
            kv: 12.47,
            conn: WdgConn::Wye,
        }
    }
}

impl CapacitorData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "bus2" => self.bus2 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "kvar" => {
                let vals = parse_f64_list(value);
                if !vals.is_empty() {
                    self.kvar = vals;
                }
            }
            "kv" => self.kv = parse_f64(value),
            "conn" => self.conn = WdgConn::from_str(value),
            _ => {}
        }
    }

    /// Total reactive power in kVAr (sum of all steps).
    pub fn total_kvar(&self) -> f64 {
        self.kvar.iter().sum()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Reactor
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ReactorData {
    pub name: String,
    pub bus1: String,
    pub bus2: String,
    pub phases: u8,
    pub kvar: f64,
    pub kv: f64,
    pub conn: WdgConn,
    pub r: f64, // series resistance Ω
    pub x: f64, // series reactance Ω
}

impl Default for ReactorData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            bus2: String::new(),
            phases: 3,
            kvar: 0.0,
            kv: 12.47,
            conn: WdgConn::Wye,
            r: 0.0,
            x: 0.0,
        }
    }
}

impl ReactorData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "bus2" => self.bus2 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "kvar" => self.kvar = parse_f64(value),
            "kv" => self.kv = parse_f64(value),
            "conn" => self.conn = WdgConn::from_str(value),
            "r" => self.r = parse_f64(value),
            "x" => self.x = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SwtControl
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SwtControlData {
    pub name: String,
    pub element: String,
    pub terminal: u32,
    pub action: String,
    pub locked: bool,
}

impl Default for SwtControlData {
    fn default() -> Self {
        Self {
            name: String::new(),
            element: String::new(),
            terminal: 1,
            action: "close".to_string(),
            locked: false,
        }
    }
}

impl SwtControlData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "element" | "switchedobj" => self.element = value.trim().to_string(),
            "terminal" | "switchedterm" => self.terminal = parse_u32(value),
            "action" => self.action = value.trim().to_lowercase(),
            "lock" | "locked" => self.locked = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Recloser
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RecloserDataEntry {
    pub name: String,
    pub monitored_obj: String,
    pub phase_curve: String,
    pub ground_curve: String,
    pub reclose_intervals: Vec<f64>,
    pub phase_trip: f64,
    pub ground_trip: f64,
}

impl Default for RecloserDataEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            monitored_obj: String::new(),
            phase_curve: String::new(),
            ground_curve: String::new(),
            reclose_intervals: vec![0.5, 2.0, 10.0],
            phase_trip: 0.0,
            ground_trip: 0.0,
        }
    }
}

impl RecloserDataEntry {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "monitoredobj" => self.monitored_obj = value.trim().to_string(),
            "phasecurve" => self.phase_curve = value.trim().to_string(),
            "groundcurve" => self.ground_curve = value.trim().to_string(),
            "reclosing" | "reclosedelays" => {
                self.reclose_intervals = parse_f64_list(value);
            }
            "phasetrip" => self.phase_trip = parse_f64(value),
            "groundtrip" => self.ground_trip = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VSConverter
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct VSConverterData {
    pub name: String,
    pub bus: String,
    pub phases: u8,
    pub kv: f64,
    pub kw: f64,
    pub kvar: f64,
    pub kwmax: f64,
    pub kvarmax: f64,
    pub kvarmin: f64,
}

impl Default for VSConverterData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus: String::new(),
            phases: 3,
            kv: 12.47,
            kw: 0.0,
            kvar: 0.0,
            kwmax: 500.0,
            kvarmax: 500.0,
            kvarmin: -500.0,
        }
    }
}

impl VSConverterData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" | "bus" => self.bus = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "kv" => self.kv = parse_f64(value),
            "kw" => self.kw = parse_f64(value),
            "kvar" => self.kvar = parse_f64(value),
            "kwmax" => self.kwmax = parse_f64(value),
            "kvarmax" => self.kvarmax = parse_f64(value),
            "kvarmin" => self.kvarmin = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fault
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FaultData {
    pub name: String,
    pub bus1: String,
    pub bus2: String,
    pub phases: u8,
    pub r: f64,
    pub on_time: f64,
}

impl Default for FaultData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            bus2: String::new(),
            phases: 3,
            r: 0.0001,
            on_time: 0.0,
        }
    }
}

impl FaultData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "bus2" => self.bus2 = value.trim().to_string(),
            "phases" => self.phases = value.trim().parse().unwrap_or(3),
            "r" => self.r = parse_f64(value),
            "ontime" => self.on_time = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GicLine / GicTransformer (geomagnetically-induced current)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GicLineData {
    pub name: String,
    pub bus1: String,
    pub bus2: String,
    pub r: f64, // DC resistance Ω
    pub volts: f64,
    pub angle: f64,
    pub frequency: f64,
}

impl Default for GicLineData {
    fn default() -> Self {
        Self {
            name: String::new(),
            bus1: String::new(),
            bus2: String::new(),
            r: 1.0,
            volts: 0.0,
            angle: 0.0,
            frequency: 0.0,
        }
    }
}

impl GicLineData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "bus2" => self.bus2 = value.trim().to_string(),
            "r" => self.r = parse_f64(value),
            "volts" => self.volts = parse_f64(value),
            "angle" => self.angle = parse_f64(value),
            "frequency" => self.frequency = parse_f64(value),
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct GicTransformerData {
    pub name: String,
    pub xfmr: String,
    pub r1: f64,
    pub r2: f64,
}

impl Default for GicTransformerData {
    fn default() -> Self {
        Self {
            name: String::new(),
            xfmr: String::new(),
            r1: 0.5,
            r2: 0.5,
        }
    }
}

impl GicTransformerData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "xfmr" | "transformer" => self.xfmr = value.trim().to_string(),
            "r1" => self.r1 = parse_f64(value),
            "r2" => self.r2 = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VoltageRegulator (voltage regulator / LTC)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct VoltageRegulatorData {
    pub name: String,
    pub transformer: String,
    pub winding: u32,
    pub vreg: f64,
    pub band: f64,
    pub pt_ratio: f64,
    pub ct_prim: f64,
    pub r: f64,
    pub x: f64,
}

impl Default for VoltageRegulatorData {
    fn default() -> Self {
        Self {
            name: String::new(),
            transformer: String::new(),
            winding: 2,
            vreg: 120.0,
            band: 3.0,
            pt_ratio: 60.0,
            ct_prim: 300.0,
            r: 0.0,
            x: 0.0,
        }
    }
}

impl VoltageRegulatorData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "transformer" | "xfmr" => self.transformer = value.trim().to_string(),
            "winding" => self.winding = parse_u32(value),
            "vreg" => self.vreg = parse_f64(value),
            "band" => self.band = parse_f64(value),
            "ptratio" => self.pt_ratio = parse_f64(value),
            "ctprim" => self.ct_prim = parse_f64(value),
            "r" => self.r = parse_f64(value),
            "x" => self.x = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LoadShape data
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct LoadShapeData {
    pub name: String,
    pub n_pts: usize,
    pub interval_h: f64,
    pub mult: Vec<f64>,
    pub q_mult: Vec<f64>,
    pub hours: Vec<f64>,
    pub normalise: bool,
}

impl LoadShapeData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "npts" => self.n_pts = value.trim().parse().unwrap_or(0),
            "interval" => self.interval_h = parse_f64(value),
            "mult" => self.mult = parse_f64_list(value),
            "qmult" => self.q_mult = parse_f64_list(value),
            "hour" => self.hours = parse_f64_list(value),
            "normalize" | "normalise" => self.normalise = parse_bool(value),
            "csvfile" => {
                // Not handled (file references require runtime resolution).
                tracing::debug!("LoadShape.csvfile not supported in inline DSS parsing");
            }
            _ => {}
        }
    }

    #[allow(dead_code)]
    pub fn to_load_shape(&self) -> crate::dss::LoadShape {
        crate::dss::LoadShape {
            name: self.name.clone(),
            n_pts: self.n_pts,
            interval_h: self.interval_h,
            mult: self.mult.clone(),
            q_mult: self.q_mult.clone(),
            hours: self.hours.clone(),
            normalise: self.normalise,
            mean: None,
            std_dev: None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CapControl — capacitor switching controller
// ─────────────────────────────────────────────────────────────────────────────

/// Capacitor switching control element (voltage, kvar, current, or time trigger).
#[derive(Debug, Clone, Default)]
pub struct CapControlData {
    pub name: String,
    /// Monitored element (line or transformer name).
    pub element: String,
    /// Terminal of monitored element.
    pub terminal: u32,
    /// Name of the controlled capacitor.
    pub capacitor: String,
    /// Control type: "voltage", "kvar", "current", "time", "pf".
    pub cap_control_type: String,
    /// ON setting (voltage, kvar, current, or PF threshold).
    pub on_setting: f64,
    /// OFF setting.
    pub off_setting: f64,
    /// CT ratio.
    pub ct_ratio: f64,
    /// PT ratio.
    pub pt_ratio: f64,
    /// Delay (seconds) before switching ON.
    pub delay: f64,
    /// Delay (seconds) before switching OFF.
    pub delay_off: f64,
    /// Dead time after a trip (seconds).
    pub dead_time: f64,
    /// Whether control is enabled.
    pub enabled: bool,
    /// Voltage override threshold (pu).
    pub v_max: f64,
    pub v_min: f64,
}

impl CapControlData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "element" => self.element = value.trim().to_string(),
            "terminal" => self.terminal = parse_u32(value),
            "capacitor" => self.capacitor = value.trim().to_string(),
            "type" => self.cap_control_type = value.trim().to_lowercase(),
            "onsetting" => self.on_setting = parse_f64(value),
            "offsetting" => self.off_setting = parse_f64(value),
            "ctratio" => self.ct_ratio = parse_f64(value),
            "ptratio" => self.pt_ratio = parse_f64(value),
            "delay" => self.delay = parse_f64(value),
            "delayoff" => self.delay_off = parse_f64(value),
            "deadtime" => self.dead_time = parse_f64(value),
            "enabled" => self.enabled = parse_bool(value),
            "vmax" => self.v_max = parse_f64(value),
            "vmin" => self.v_min = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Relay — protective relay
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct RelayData {
    pub name: String,
    pub monitored_obj: String,
    pub monitored_term: u32,
    pub switched_obj: String,
    pub switched_term: u32,
    /// Relay type: "current", "voltage", "frequency", "47" (neg-seq), "46" (neg-seq I).
    pub relay_type: String,
    pub phase_curve: String,
    pub ground_curve: String,
    pub phase_trip: f64,
    pub ground_trip: f64,
    pub phase_inst: f64,
    pub ground_inst: f64,
    pub td_phase: f64,
    pub td_ground: f64,
    pub ct_ratio: f64,
    pub reset_time: f64,
    pub shots: u32,
    pub enabled: bool,
}

impl RelayData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "monitoredobj" => self.monitored_obj = value.trim().to_string(),
            "monitoredterm" => self.monitored_term = parse_u32(value),
            "switchedobj" => self.switched_obj = value.trim().to_string(),
            "switchedterm" => self.switched_term = parse_u32(value),
            "type" => self.relay_type = value.trim().to_lowercase(),
            "phasecurve" => self.phase_curve = value.trim().to_string(),
            "groundcurve" => self.ground_curve = value.trim().to_string(),
            "phasetrip" => self.phase_trip = parse_f64(value),
            "groundtrip" => self.ground_trip = parse_f64(value),
            "phaseinst" => self.phase_inst = parse_f64(value),
            "groundinst" => self.ground_inst = parse_f64(value),
            "tdphase" => self.td_phase = parse_f64(value),
            "tdground" => self.td_ground = parse_f64(value),
            "ctratio" => self.ct_ratio = parse_f64(value),
            "reset" | "resettime" => self.reset_time = parse_f64(value),
            "shots" => self.shots = parse_u32(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fuse — protective fuse
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct FuseData {
    pub name: String,
    pub monitored_obj: String,
    pub monitored_term: u32,
    pub switched_obj: String,
    pub switched_term: u32,
    pub fuse_curve: String,
    pub rated_current: f64,
    pub delay: f64,
    pub enabled: bool,
}

impl FuseData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "monitoredobj" => self.monitored_obj = value.trim().to_string(),
            "monitoredterm" => self.monitored_term = parse_u32(value),
            "switchedobj" => self.switched_obj = value.trim().to_string(),
            "switchedterm" => self.switched_term = parse_u32(value),
            "fusecurve" => self.fuse_curve = value.trim().to_string(),
            "ratedcurrent" => self.rated_current = parse_f64(value),
            "delay" => self.delay = parse_f64(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Isource — current source injection
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct IsourceData {
    pub name: String,
    pub bus1: String,
    pub phases: u32,
    pub amps: f64,
    pub angle: f64,
    pub frequency: f64,
    pub daily: String,
    pub yearly: String,
}

impl IsourceData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "phases" => self.phases = parse_u32(value),
            "amps" => self.amps = parse_f64(value),
            "angle" => self.angle = parse_f64(value),
            "frequency" => self.frequency = parse_f64(value),
            "daily" => self.daily = value.trim().to_string(),
            "yearly" => self.yearly = value.trim().to_string(),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GrowthShape — annual load growth multipliers
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct GrowthShapeData {
    pub name: String,
    pub n_pts: usize,
    pub year: Vec<f64>,
    pub mult: Vec<f64>,
}

impl GrowthShapeData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "npts" => self.n_pts = value.trim().parse().unwrap_or(0),
            "year" => self.year = parse_f64_list(value),
            "mult" => self.mult = parse_f64_list(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// XYCurve — generic X-Y curve data
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct XYCurveData {
    pub name: String,
    pub n_pts: usize,
    pub x_array: Vec<f64>,
    pub y_array: Vec<f64>,
}

impl XYCurveData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "npts" => self.n_pts = value.trim().parse().unwrap_or(0),
            "xarray" | "points" => self.x_array = parse_f64_list(value),
            "yarray" => self.y_array = parse_f64_list(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TCC_Curve — Time-Current Characteristic curve
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TccCurveData {
    pub name: String,
    pub n_pts: usize,
    pub c_array: Vec<f64>,
    pub t_array: Vec<f64>,
}

impl TccCurveData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "npts" => self.n_pts = value.trim().parse().unwrap_or(0),
            "c_array" | "carray" => self.c_array = parse_f64_list(value),
            "t_array" | "tarray" => self.t_array = parse_f64_list(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Spectrum — harmonic spectrum definition
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SpectrumData {
    pub name: String,
    pub n_harms: usize,
    pub harmonic: Vec<f64>,
    pub pct_mag: Vec<f64>,
    pub angle: Vec<f64>,
}

impl SpectrumData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "numharm" | "numharms" => self.n_harms = value.trim().parse().unwrap_or(0),
            "harmonic" => self.harmonic = parse_f64_list(value),
            "%mag" | "pctmag" => self.pct_mag = parse_f64_list(value),
            "angle" => self.angle = parse_f64_list(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LineSpacing / SpacingCode — conductor geometry
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct LineSpacingData {
    pub name: String,
    pub n_conds: u32,
    pub n_phases: u32,
    pub x: Vec<f64>,
    pub h: Vec<f64>,
    pub units: String,
}

impl LineSpacingData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "nconds" => self.n_conds = parse_u32(value),
            "nphases" => self.n_phases = parse_u32(value),
            "x" => self.x = parse_f64_list(value),
            "h" => self.h = parse_f64_list(value),
            "units" => self.units = value.trim().to_lowercase(),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// InvControl — IEEE 1547 smart inverter control
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct InvControlData {
    pub name: String,
    /// Comma-separated list of DER element names to control.
    pub der_list: Vec<String>,
    /// Control mode: "voltvar", "voltwatt", "varwatt", "dynamicreaccurr", etc.
    pub mode: String,
    /// Name of the XYCurve for volt-var.
    pub vvc_curve1: String,
    /// Name of the XYCurve for volt-watt.
    pub voltwatt_curve: String,
    /// Hysteresis offset (pu).
    pub hysteresis_offset: f64,
    /// Voltage set point (pu).
    pub voltage_curvex_ref: String,
    /// Rate of change limit (%/sec).
    pub ramp_rate: f64,
    pub enabled: bool,
}

impl InvControlData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "derlist" => {
                self.der_list = value
                    .trim_matches(|c: char| c == '[' || c == ']')
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "mode" => self.mode = value.trim().to_lowercase(),
            "vvc_curve1" => self.vvc_curve1 = value.trim().to_string(),
            "voltwatt_curve" => self.voltwatt_curve = value.trim().to_string(),
            "hysteresis_offset" => self.hysteresis_offset = parse_f64(value),
            "voltage_curvex_ref" => self.voltage_curvex_ref = value.trim().to_lowercase(),
            "ramprate" | "ramp_rate" => self.ramp_rate = parse_f64(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ExpControl — exponential volt-var control for DER
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ExpControlData {
    pub name: String,
    pub der_list: Vec<String>,
    pub vreg: f64,
    pub slope: f64,
    pub v_ref: f64,
    pub enabled: bool,
}

impl ExpControlData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "derlist" | "pvsystemlist" => {
                self.der_list = value
                    .trim_matches(|c: char| c == '[' || c == ']')
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "vreg" => self.vreg = parse_f64(value),
            "slope" => self.slope = parse_f64(value),
            "vref" => self.v_ref = parse_f64(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// StorageController — battery charge/discharge strategy
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct StorageControllerData {
    pub name: String,
    pub element: String,
    pub terminal: u32,
    pub element_list: Vec<String>,
    pub mode_charge: String,
    pub mode_discharge: String,
    pub kw_target: f64,
    pub pct_kw_band: f64,
    pub time_charge_trigger: f64,
    pub time_discharge_trigger: f64,
    pub pct_reserve: f64,
    pub enabled: bool,
}

impl StorageControllerData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "element" => self.element = value.trim().to_string(),
            "terminal" => self.terminal = parse_u32(value),
            "elementlist" => {
                self.element_list = value
                    .trim_matches(|c: char| c == '[' || c == ']')
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "modecharge" => self.mode_charge = value.trim().to_lowercase(),
            "modedischarge" | "modedisch" => self.mode_discharge = value.trim().to_lowercase(),
            "kwtarget" => self.kw_target = parse_f64(value),
            "%kwband" | "pctkwband" => self.pct_kw_band = parse_f64(value),
            "timechargetrigger" => self.time_charge_trigger = parse_f64(value),
            "timedischtrigger" | "timedischargetrigger" => {
                self.time_discharge_trigger = parse_f64(value);
            }
            "%reserve" | "pctreserve" => self.pct_reserve = parse_f64(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Monitor — data recorder
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MonitorData {
    pub name: String,
    pub element: String,
    pub terminal: u32,
    /// Mode: 0=V/I, 1=P/Q, 2=tap, 3=state variables, etc.
    pub mode: u32,
    pub enabled: bool,
}

impl MonitorData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "element" => self.element = value.trim().to_string(),
            "terminal" => self.terminal = parse_u32(value),
            "mode" => self.mode = parse_u32(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EnergyMeter — energy tracking
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct EnergyMeterData {
    pub name: String,
    pub element: String,
    pub terminal: u32,
    pub enabled: bool,
}

impl EnergyMeterData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "element" => self.element = value.trim().to_string(),
            "terminal" => self.terminal = parse_u32(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sensor — measurement point
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SensorData {
    pub name: String,
    pub element: String,
    pub terminal: u32,
    pub kv_base: f64,
    pub enabled: bool,
}

impl SensorData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "element" => self.element = value.trim().to_string(),
            "terminal" => self.terminal = parse_u32(value),
            "kvbase" => self.kv_base = parse_f64(value),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PriceShape — economic dispatch price curve
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct PriceShapeData {
    pub name: String,
    pub n_pts: usize,
    pub interval_h: f64,
    pub price: Vec<f64>,
    pub hours: Vec<f64>,
}

impl PriceShapeData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "npts" => self.n_pts = value.trim().parse().unwrap_or(0),
            "interval" => self.interval_h = parse_f64(value),
            "price" => self.price = parse_f64_list(value),
            "hour" => self.hours = parse_f64_list(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TempShape — thermal derating shape
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TempShapeData {
    pub name: String,
    pub n_pts: usize,
    pub interval_h: f64,
    pub temp: Vec<f64>,
    pub hours: Vec<f64>,
}

impl TempShapeData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "npts" => self.n_pts = value.trim().parse().unwrap_or(0),
            "interval" => self.interval_h = parse_f64(value),
            "temp" => self.temp = parse_f64_list(value),
            "hour" => self.hours = parse_f64_list(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// IndMach012 — induction machine (sequence model)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct IndMach012Data {
    pub name: String,
    pub bus1: String,
    pub phases: u32,
    pub kv: f64,
    pub kw: f64,
    pub pf: f64,
    pub slip: f64,
    /// Positive-sequence stator resistance (pu).
    pub rs: f64,
    /// Positive-sequence stator reactance (pu).
    pub xs: f64,
    /// Positive-sequence rotor resistance (pu).
    pub rr: f64,
    /// Positive-sequence rotor reactance (pu).
    pub xr: f64,
    /// Magnetizing reactance (pu).
    pub xm: f64,
    pub h: f64,
}

impl IndMach012Data {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "phases" => self.phases = parse_u32(value),
            "kv" => self.kv = parse_f64(value),
            "kw" => self.kw = parse_f64(value),
            "pf" => self.pf = parse_f64(value),
            "slip" => self.slip = parse_f64(value),
            "rs" => self.rs = parse_f64(value),
            "xs" => self.xs = parse_f64(value),
            "rr" => self.rr = parse_f64(value),
            "xr" => self.xr = parse_f64(value),
            "xm" => self.xm = parse_f64(value),
            "h" => self.h = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UPFC — unified power flow controller
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct UpfcData {
    pub name: String,
    pub bus1: String,
    pub bus2: String,
    pub phases: u32,
    pub kv: f64,
    pub ref_kv: f64,
    pub pf: f64,
    pub xs: f64,
    pub loss_curve: String,
    pub enabled: bool,
}

impl UpfcData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "bus2" => self.bus2 = value.trim().to_string(),
            "phases" => self.phases = parse_u32(value),
            "kv" => self.kv = parse_f64(value),
            "refkv" => self.ref_kv = parse_f64(value),
            "pf" => self.pf = parse_f64(value),
            "xs" => self.xs = parse_f64(value),
            "losscurve" => self.loss_curve = value.trim().to_string(),
            "enabled" => self.enabled = parse_bool(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GICsource — GIC voltage source
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct GicSourceData {
    pub name: String,
    pub bus1: String,
    pub bus2: String,
    pub volts: f64,
    pub angle: f64,
    pub frequency: f64,
    pub phases: u32,
    pub en_north: f64,
    pub en_east: f64,
    pub lat1: f64,
    pub lon1: f64,
    pub lat2: f64,
    pub lon2: f64,
}

impl GicSourceData {
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match key.to_lowercase().as_str() {
            "bus1" => self.bus1 = value.trim().to_string(),
            "bus2" => self.bus2 = value.trim().to_string(),
            "volts" => self.volts = parse_f64(value),
            "angle" => self.angle = parse_f64(value),
            "frequency" => self.frequency = parse_f64(value),
            "phases" => self.phases = parse_u32(value),
            "en" | "ennorth" => self.en_north = parse_f64(value),
            "ee" | "eneast" => self.en_east = parse_f64(value),
            "lat1" => self.lat1 = parse_f64(value),
            "lon1" => self.lon1 = parse_f64(value),
            "lat2" => self.lat2 = parse_f64(value),
            "lon2" => self.lon2 = parse_f64(value),
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Main DssObject enum
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed DSS element (any type).
#[derive(Debug, Clone)]
pub enum DssObject {
    Circuit(CircuitData),
    Line(LineData),
    LineCode(LineCodeData),
    LineGeometry(LineGeometryData),
    WireData(WireDataEntry),
    CnData(CnDataEntry),
    TsData(TsDataEntry),
    Transformer(TransformerData),
    AutoTrans(AutoTransData),
    XfmrCode(XfmrCodeData),
    Load(LoadData),
    Generator(GeneratorData),
    PvSystem(PvSystemData),
    Storage(StorageData),
    Capacitor(CapacitorData),
    Reactor(ReactorData),
    SwtControl(SwtControlData),
    Recloser(RecloserDataEntry),
    VsConverter(VSConverterData),
    VoltageRegulator(VoltageRegulatorData),
    LoadShape(LoadShapeData),
    Fault(FaultData),
    GicLine(GicLineData),
    GicTransformer(GicTransformerData),
    // Priority 1: Power flow critical
    CapControl(CapControlData),
    Relay(RelayData),
    Fuse(FuseData),
    Isource(IsourceData),
    // Priority 2: Shape & library elements
    GrowthShape(GrowthShapeData),
    XYCurve(XYCurveData),
    TccCurve(TccCurveData),
    Spectrum(SpectrumData),
    LineSpacing(LineSpacingData),
    // Priority 3: Advanced power conversion / control
    InvControl(InvControlData),
    ExpControl(ExpControlData),
    StorageController(StorageControllerData),
    IndMach012(IndMach012Data),
    Upfc(UpfcData),
    GicSource(GicSourceData),
    // Priority 4: Monitoring & reporting
    Monitor(MonitorData),
    EnergyMeter(EnergyMeterData),
    Sensor(SensorData),
    PriceShape(PriceShapeData),
    TempShape(TempShapeData),
}

impl DssObject {
    /// Get the mutable name field for any object.
    pub fn name_mut(&mut self) -> &mut String {
        match self {
            DssObject::Circuit(d) => &mut d.name,
            DssObject::Line(d) => &mut d.name,
            DssObject::LineCode(d) => &mut d.name,
            DssObject::LineGeometry(d) => &mut d.name,
            DssObject::WireData(d) => &mut d.name,
            DssObject::CnData(d) => &mut d.name,
            DssObject::TsData(d) => &mut d.name,
            DssObject::Transformer(d) => &mut d.name,
            DssObject::AutoTrans(d) => &mut d.transformer.name,
            DssObject::XfmrCode(d) => &mut d.name,
            DssObject::Load(d) => &mut d.name,
            DssObject::Generator(d) => &mut d.name,
            DssObject::PvSystem(d) => &mut d.name,
            DssObject::Storage(d) => &mut d.name,
            DssObject::Capacitor(d) => &mut d.name,
            DssObject::Reactor(d) => &mut d.name,
            DssObject::SwtControl(d) => &mut d.name,
            DssObject::Recloser(d) => &mut d.name,
            DssObject::VsConverter(d) => &mut d.name,
            DssObject::VoltageRegulator(d) => &mut d.name,
            DssObject::LoadShape(d) => &mut d.name,
            DssObject::Fault(d) => &mut d.name,
            DssObject::GicLine(d) => &mut d.name,
            DssObject::GicTransformer(d) => &mut d.name,
            DssObject::CapControl(d) => &mut d.name,
            DssObject::Relay(d) => &mut d.name,
            DssObject::Fuse(d) => &mut d.name,
            DssObject::Isource(d) => &mut d.name,
            DssObject::GrowthShape(d) => &mut d.name,
            DssObject::XYCurve(d) => &mut d.name,
            DssObject::TccCurve(d) => &mut d.name,
            DssObject::Spectrum(d) => &mut d.name,
            DssObject::LineSpacing(d) => &mut d.name,
            DssObject::InvControl(d) => &mut d.name,
            DssObject::ExpControl(d) => &mut d.name,
            DssObject::StorageController(d) => &mut d.name,
            DssObject::IndMach012(d) => &mut d.name,
            DssObject::Upfc(d) => &mut d.name,
            DssObject::GicSource(d) => &mut d.name,
            DssObject::Monitor(d) => &mut d.name,
            DssObject::EnergyMeter(d) => &mut d.name,
            DssObject::Sensor(d) => &mut d.name,
            DssObject::PriceShape(d) => &mut d.name,
            DssObject::TempShape(d) => &mut d.name,
        }
    }

    /// Get the name field.
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        match self {
            DssObject::Circuit(d) => &d.name,
            DssObject::Line(d) => &d.name,
            DssObject::LineCode(d) => &d.name,
            DssObject::LineGeometry(d) => &d.name,
            DssObject::WireData(d) => &d.name,
            DssObject::CnData(d) => &d.name,
            DssObject::TsData(d) => &d.name,
            DssObject::Transformer(d) => &d.name,
            DssObject::AutoTrans(d) => &d.transformer.name,
            DssObject::XfmrCode(d) => &d.name,
            DssObject::Load(d) => &d.name,
            DssObject::Generator(d) => &d.name,
            DssObject::PvSystem(d) => &d.name,
            DssObject::Storage(d) => &d.name,
            DssObject::Capacitor(d) => &d.name,
            DssObject::Reactor(d) => &d.name,
            DssObject::SwtControl(d) => &d.name,
            DssObject::Recloser(d) => &d.name,
            DssObject::VsConverter(d) => &d.name,
            DssObject::VoltageRegulator(d) => &d.name,
            DssObject::LoadShape(d) => &d.name,
            DssObject::Fault(d) => &d.name,
            DssObject::GicLine(d) => &d.name,
            DssObject::GicTransformer(d) => &d.name,
            DssObject::CapControl(d) => &d.name,
            DssObject::Relay(d) => &d.name,
            DssObject::Fuse(d) => &d.name,
            DssObject::Isource(d) => &d.name,
            DssObject::GrowthShape(d) => &d.name,
            DssObject::XYCurve(d) => &d.name,
            DssObject::TccCurve(d) => &d.name,
            DssObject::Spectrum(d) => &d.name,
            DssObject::LineSpacing(d) => &d.name,
            DssObject::InvControl(d) => &d.name,
            DssObject::ExpControl(d) => &d.name,
            DssObject::StorageController(d) => &d.name,
            DssObject::IndMach012(d) => &d.name,
            DssObject::Upfc(d) => &d.name,
            DssObject::GicSource(d) => &d.name,
            DssObject::Monitor(d) => &d.name,
            DssObject::EnergyMeter(d) => &d.name,
            DssObject::Sensor(d) => &d.name,
            DssObject::PriceShape(d) => &d.name,
            DssObject::TempShape(d) => &d.name,
        }
    }

    /// Apply a named property to this object. Unknown keys are silently ignored.
    pub fn apply_property(&mut self, key: &str, value: &str) {
        match self {
            DssObject::Circuit(d) => d.apply_property(key, value),
            DssObject::Line(d) => d.apply_property(key, value),
            DssObject::LineCode(d) => d.apply_property(key, value),
            DssObject::LineGeometry(d) => d.apply_property(key, value),
            DssObject::WireData(d) => d.apply_property(key, value),
            DssObject::CnData(d) => d.apply_property(key, value),
            DssObject::TsData(d) => d.apply_property(key, value),
            DssObject::Transformer(d) => d.apply_property(key, value),
            DssObject::AutoTrans(d) => d.apply_property(key, value),
            DssObject::XfmrCode(d) => d.apply_property(key, value),
            DssObject::Load(d) => d.apply_property(key, value),
            DssObject::Generator(d) => d.apply_property(key, value),
            DssObject::PvSystem(d) => d.apply_property(key, value),
            DssObject::Storage(d) => d.apply_property(key, value),
            DssObject::Capacitor(d) => d.apply_property(key, value),
            DssObject::Reactor(d) => d.apply_property(key, value),
            DssObject::SwtControl(d) => d.apply_property(key, value),
            DssObject::Recloser(d) => d.apply_property(key, value),
            DssObject::VsConverter(d) => d.apply_property(key, value),
            DssObject::VoltageRegulator(d) => d.apply_property(key, value),
            DssObject::LoadShape(d) => d.apply_property(key, value),
            DssObject::Fault(d) => d.apply_property(key, value),
            DssObject::GicLine(d) => d.apply_property(key, value),
            DssObject::GicTransformer(d) => d.apply_property(key, value),
            DssObject::CapControl(d) => d.apply_property(key, value),
            DssObject::Relay(d) => d.apply_property(key, value),
            DssObject::Fuse(d) => d.apply_property(key, value),
            DssObject::Isource(d) => d.apply_property(key, value),
            DssObject::GrowthShape(d) => d.apply_property(key, value),
            DssObject::XYCurve(d) => d.apply_property(key, value),
            DssObject::TccCurve(d) => d.apply_property(key, value),
            DssObject::Spectrum(d) => d.apply_property(key, value),
            DssObject::LineSpacing(d) => d.apply_property(key, value),
            DssObject::InvControl(d) => d.apply_property(key, value),
            DssObject::ExpControl(d) => d.apply_property(key, value),
            DssObject::StorageController(d) => d.apply_property(key, value),
            DssObject::IndMach012(d) => d.apply_property(key, value),
            DssObject::Upfc(d) => d.apply_property(key, value),
            DssObject::GicSource(d) => d.apply_property(key, value),
            DssObject::Monitor(d) => d.apply_property(key, value),
            DssObject::EnergyMeter(d) => d.apply_property(key, value),
            DssObject::Sensor(d) => d.apply_property(key, value),
            DssObject::PriceShape(d) => d.apply_property(key, value),
            DssObject::TempShape(d) => d.apply_property(key, value),
        }
    }

    /// Create a new default object of the given DSS type name.
    /// Returns None for unknown types (caller should warn and skip).
    pub fn new_for_type(type_name: &str) -> Option<DssObject> {
        match type_name.to_lowercase().as_str() {
            "circuit" | "vsource" => Some(DssObject::Circuit(CircuitData::default())),
            "line" => Some(DssObject::Line(LineData::default())),
            "linecode" => Some(DssObject::LineCode(LineCodeData::default())),
            "linegeometry" => Some(DssObject::LineGeometry(LineGeometryData::default())),
            "wiredata" | "wire" => Some(DssObject::WireData(WireDataEntry::default())),
            "cndata" | "cn" => Some(DssObject::CnData(CnDataEntry::default())),
            "tsdata" | "ts" => Some(DssObject::TsData(TsDataEntry::default())),
            "transformer" => Some(DssObject::Transformer(TransformerData::default())),
            "autotrans" => Some(DssObject::AutoTrans(AutoTransData::default())),
            "xfmrcode" => Some(DssObject::XfmrCode(XfmrCodeData::default())),
            "load" => Some(DssObject::Load(LoadData::default())),
            "generator" => Some(DssObject::Generator(GeneratorData::default())),
            "pvsystem" => Some(DssObject::PvSystem(PvSystemData::default())),
            "storage" => Some(DssObject::Storage(StorageData::default())),
            "capacitor" => Some(DssObject::Capacitor(CapacitorData::default())),
            "reactor" => Some(DssObject::Reactor(ReactorData::default())),
            "swtcontrol" => Some(DssObject::SwtControl(SwtControlData::default())),
            "recloser" => Some(DssObject::Recloser(RecloserDataEntry::default())),
            "vsconverter" => Some(DssObject::VsConverter(VSConverterData::default())),
            "regcontrol" => Some(DssObject::VoltageRegulator(VoltageRegulatorData::default())),
            "loadshape" => Some(DssObject::LoadShape(LoadShapeData::default())),
            "fault" => Some(DssObject::Fault(FaultData::default())),
            "gicline" => Some(DssObject::GicLine(GicLineData::default())),
            "gictransformer" => Some(DssObject::GicTransformer(GicTransformerData::default())),
            // Priority 1: Power flow critical
            "capcontrol" => Some(DssObject::CapControl(CapControlData::default())),
            "relay" => Some(DssObject::Relay(RelayData::default())),
            "fuse" => Some(DssObject::Fuse(FuseData::default())),
            "isource" => Some(DssObject::Isource(IsourceData::default())),
            // Priority 2: Shape & library elements
            "growthshape" => Some(DssObject::GrowthShape(GrowthShapeData::default())),
            "xycurve" => Some(DssObject::XYCurve(XYCurveData::default())),
            "tcc_curve" => Some(DssObject::TccCurve(TccCurveData::default())),
            "spectrum" => Some(DssObject::Spectrum(SpectrumData::default())),
            "linespacing" | "spacingcode" => {
                Some(DssObject::LineSpacing(LineSpacingData::default()))
            }
            // Priority 3: Advanced power conversion / control
            "invcontrol" => Some(DssObject::InvControl(InvControlData::default())),
            "expcontrol" => Some(DssObject::ExpControl(ExpControlData::default())),
            "storagecontroller" => Some(DssObject::StorageController(
                StorageControllerData::default(),
            )),
            "indmach012" => Some(DssObject::IndMach012(IndMach012Data::default())),
            "upfc" => Some(DssObject::Upfc(UpfcData::default())),
            "gicsource" => Some(DssObject::GicSource(GicSourceData::default())),
            // Priority 4: Monitoring & reporting
            "monitor" => Some(DssObject::Monitor(MonitorData::default())),
            "energymeter" => Some(DssObject::EnergyMeter(EnergyMeterData::default())),
            "sensor" => Some(DssObject::Sensor(SensorData::default())),
            "priceshape" => Some(DssObject::PriceShape(PriceShapeData::default())),
            "tempshape" | "tshape" => Some(DssObject::TempShape(TempShapeData::default())),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Object catalog — keyed by "Type.name" (case-insensitive)
// ─────────────────────────────────────────────────────────────────────────────

/// A collection of all parsed DSS objects, indexed by `"type.name"`.
#[derive(Debug, Default)]
pub struct DssCatalog {
    /// All objects by canonical key `"type.name"` (lowercase).
    pub objects: Vec<DssObject>,
    /// Optional circuit (only one per file).
    pub circuit: Option<CircuitData>,
    /// Index for fast lookup by type+name.
    pub index: HashMap<String, usize>,
}

impl DssCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update an object. Returns the object's index.
    pub fn upsert(&mut self, type_name: &str, obj_name: &str, obj: DssObject) -> usize {
        let key = format!("{}.{}", type_name.to_lowercase(), obj_name.to_lowercase());
        if let Some(&idx) = self.index.get(&key) {
            self.objects[idx] = obj;
            idx
        } else {
            let idx = self.objects.len();
            self.objects.push(obj);
            self.index.insert(key, idx);
            idx
        }
    }

    /// Get object by index.
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut DssObject> {
        self.objects.get_mut(idx)
    }

    /// Find an object by type+name (case-insensitive).
    #[allow(dead_code)]
    pub fn find(&self, type_name: &str, obj_name: &str) -> Option<&DssObject> {
        let key = format!("{}.{}", type_name.to_lowercase(), obj_name.to_lowercase());
        self.index.get(&key).and_then(|&i| self.objects.get(i))
    }

    /// Iterate objects of a given type (case-insensitive prefix filter).
    #[allow(dead_code)]
    pub fn iter_type(&self, type_name: &str) -> impl Iterator<Item = &DssObject> {
        let prefix = format!("{}.", type_name.to_lowercase());
        self.index
            .iter()
            .filter(move |(k, _)| k.starts_with(&prefix))
            .map(|(_, &i)| &self.objects[i])
    }
}
