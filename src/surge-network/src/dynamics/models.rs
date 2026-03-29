// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Dynamic model types for transient stability analysis.
//!
//! This module defines data structures for electromechanical generator models,
//! excitation systems, turbine-governors, and power system stabilizers loaded
//! from PSS/E `.dyr` files.
//!
//! # Supported models
//!
//! - **Generators**: GENCLS, GENROU, GENSAL
//! - **Exciters**: EXST1, ESST3A, ESDC2A, EXDC2, IEEEX1/IEEEXC1
//! - **Governors**: TGOV1, IEEEG1
//! - **PSS**: IEEEST, ST2CUT
//!
//! # Simplified vs. detailed models (DYN-01)
//!
//! Several models in this module are **standard planning-level simplifications**,
//! suitable for use when detailed dynamic data is unavailable.  For precise
//! transient stability studies, use the detailed models when dynamic test data
//! files (DYRE) provide the necessary parameters.
//!
//! ## Exciter model mapping
//!
//! | Simplified model | Detailed equivalent       | When to upgrade                        |
//! |------------------|---------------------------|----------------------------------------|
//! | SEXS             | AC5A, ESST4B, EXAC1       | Detailed AVR test data available       |
//! | SCRX             | ESST4B, ESST3A            | Static exciter field test data         |
//! | IEEET1           | ESDC2A, EXDC2, IEEEX1     | Rotating exciter field data            |
//!
//! ## Governor model mapping
//!
//! | Simplified model | Detailed equivalent       | When to upgrade                        |
//! |------------------|---------------------------|----------------------------------------|
//! | GAST             | GAST2A, GGOV1             | Gas turbine performance test data      |
//! | TGOV1            | IEEEG1, TGOV5             | Steam turbine test data with reheat    |
//!
//! ## IBR (inverter-based resource) models
//!
//! | Simplified model | Detailed equivalent       | When to upgrade                        |
//! |------------------|---------------------------|----------------------------------------|
//! | REPC_A (simplified) | Full REPC_A with AGC droop | Plant-level control parameters     |
//! | REEC_A (simplified) | Full REEC_A with Kqv droop | Electrical controller test data    |
//! | REGC_A           | (already detailed)        | N/A                                    |
//!
//! The simplified IBR models use constant Pref/Qref (no AGC droop or plant-level
//! voltage regulation), which is acceptable for planning-level studies where the
//! IBR is not the focus.  For studies involving IBR fault ride-through, frequency
//! response, or voltage regulation, use the full model parameters.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level container
// ---------------------------------------------------------------------------

/// Complete dynamic model database parsed from a `.dyr` file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DynamicModel {
    /// Generator dynamic models (GENCLS, GENROU, GENSAL).
    pub generators: Vec<GeneratorDyn>,
    /// Excitation system models (EXST1, ESST3A, ESDC2A, EXDC2, IEEEX1).
    pub exciters: Vec<ExciterDyn>,
    /// Turbine-governor models (TGOV1, IEEEG1).
    pub governors: Vec<GovernorDyn>,
    /// Power system stabilizer models (IEEEST, ST2CUT).
    pub pss: Vec<PssDyn>,
    /// Load dynamic models (CLOD, INDMOT, MOTOR) — Phase 12.
    #[serde(default)]
    pub loads: Vec<LoadDyn>,
    /// FACTS/HVDC dynamic models (CSVGN1, CSTCON, TCSC, CDC4T, VSCDCT) — Phase 13.
    #[serde(default)]
    pub facts: Vec<FACTSDyn>,
    /// Records with unrecognised model names — stored verbatim for diagnostics.
    pub unknown_records: Vec<UnknownDyrRecord>,
    /// Over-excitation limiter models (OEL1B, OEL2C, SCL1C) — Wave 37.
    #[serde(default)]
    pub oels: Vec<OelDyn>,
    /// Under-excitation limiter models (UEL1, UEL2C) — Wave 37.
    #[serde(default)]
    pub uels: Vec<UelDyn>,
    /// Multi-mass torsional shaft models for time-domain SSR simulation.
    #[serde(default)]
    pub shafts: Vec<ShaftDyn>,
}

impl DynamicModel {
    /// Number of generator dynamic records.
    pub fn n_generators(&self) -> usize {
        self.generators.len()
    }

    /// Number of exciter records.
    pub fn n_exciters(&self) -> usize {
        self.exciters.len()
    }

    /// Number of governor records.
    pub fn n_governors(&self) -> usize {
        self.governors.len()
    }

    /// Number of PSS records.
    pub fn n_pss(&self) -> usize {
        self.pss.len()
    }

    /// Number of load dynamic records.
    pub fn n_loads(&self) -> usize {
        self.loads.len()
    }

    /// Total number of recognised dynamic records.
    pub fn total(&self) -> usize {
        self.n_generators()
            + self.n_exciters()
            + self.n_governors()
            + self.n_pss()
            + self.n_loads()
            + self.n_facts()
            + self.oels.len()
            + self.uels.len()
            + self.shafts.len()
    }

    /// Compute supported model coverage.
    /// Returns `(n_supported, n_total, coverage_pct)`.
    pub fn coverage(&self) -> (usize, usize, f64) {
        let n_supported = self.total();
        let n_unknown = self.unknown_records.len();
        let n_total = n_supported + n_unknown;
        let pct = if n_total > 0 {
            n_supported as f64 / n_total as f64 * 100.0
        } else {
            100.0
        };
        (n_supported, n_total, pct)
    }

    /// Number of FACTS/HVDC dynamic records.
    pub fn n_facts(&self) -> usize {
        self.facts.len()
    }

    /// Find the first FACTS/HVDC dynamic record at the given bus with the given device ID.
    pub fn find_facts(&self, bus: u32, device_id: &str) -> Option<&FACTSDyn> {
        self.facts
            .iter()
            .find(|f| f.bus == bus && f.device_id == device_id)
    }

    /// Find the first load dynamic record at the given bus with the given load ID.
    pub fn find_load(&self, bus: u32, load_id: &str) -> Option<&LoadDyn> {
        self.loads
            .iter()
            .find(|l| l.bus == bus && l.load_id == load_id)
    }

    /// Find the first generator dynamic record at the given bus with the given machine ID.
    pub fn find_generator(&self, bus: u32, machine_id: &str) -> Option<&GeneratorDyn> {
        self.generators
            .iter()
            .find(|g| g.bus == bus && g.machine_id == machine_id)
    }

    /// Find the first exciter record at the given bus with the given machine ID.
    pub fn find_exciter(&self, bus: u32, machine_id: &str) -> Option<&ExciterDyn> {
        self.exciters
            .iter()
            .find(|e| e.bus == bus && e.machine_id == machine_id)
    }

    /// Find the first governor record at the given bus with the given machine ID.
    pub fn find_governor(&self, bus: u32, machine_id: &str) -> Option<&GovernorDyn> {
        self.governors
            .iter()
            .find(|g| g.bus == bus && g.machine_id == machine_id)
    }

    /// Find the first PSS record at the given bus with the given machine ID.
    pub fn find_pss(&self, bus: u32, machine_id: &str) -> Option<&PssDyn> {
        self.pss
            .iter()
            .find(|p| p.bus == bus && p.machine_id == machine_id)
    }

    /// Find the first shaft dynamic record at the given bus with the given machine ID.
    pub fn find_shaft(&self, bus: u32, machine_id: &str) -> Option<&ShaftDyn> {
        self.shafts
            .iter()
            .find(|s| s.bus == bus && s.machine_id == machine_id)
    }

    // -----------------------------------------------------------------------
    // USRMDL transparency — summary report & gap analysis
    // -----------------------------------------------------------------------

    /// Build a summary report of unknown/unrecognized dynamic models.
    ///
    /// Groups unknown records by model name, counts occurrences, lists affected
    /// buses, and suggests standard equivalents where known.
    pub fn unknown_model_summary(&self) -> Vec<UnknownModelGroup> {
        use std::collections::BTreeMap;

        let mut groups: BTreeMap<String, UnknownModelGroup> = BTreeMap::new();
        for rec in &self.unknown_records {
            let entry = groups.entry(rec.model_name.clone()).or_insert_with(|| {
                let equiv = crate::dynamics::usrmdl_equiv::suggest_equivalent(&rec.model_name);
                let category = crate::dynamics::usrmdl_equiv::guess_category(&rec.model_name);
                UnknownModelGroup {
                    model_name: rec.model_name.clone(),
                    count: 0,
                    buses: Vec::new(),
                    category: category.label().to_string(),
                    suggested_equivalent: equiv.map(|e| e.suggested.to_string()),
                    suggestion_notes: equiv.map(|e| e.notes.to_string()),
                }
            });
            entry.count += 1;
            if !entry.buses.contains(&rec.bus) {
                entry.buses.push(rec.bus);
            }
        }

        groups.into_values().collect()
    }

    /// Find generators that have a generator model but are missing one or more
    /// of: exciter, governor, or PSS.
    ///
    /// Returns `(bus, machine_id, has_exciter, has_governor, has_pss)` tuples.
    pub fn incomplete_machines(&self) -> Vec<IncompleteMachine> {
        self.generators
            .iter()
            .filter_map(|g| {
                let has_exc = self.find_exciter(g.bus, &g.machine_id).is_some();
                let has_gov = self.find_governor(g.bus, &g.machine_id).is_some();
                let has_pss = self.find_pss(g.bus, &g.machine_id).is_some();
                // Report if missing any component (exciter, governor, or PSS)
                if !has_exc || !has_gov || !has_pss {
                    Some(IncompleteMachine {
                        bus: g.bus,
                        machine_id: g.machine_id.clone(),
                        has_exciter: has_exc,
                        has_governor: has_gov,
                        has_pss,
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Summary of an unrecognized dynamic model group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownModelGroup {
    /// Model name as read from the DYR file.
    pub model_name: String,
    /// Number of records with this model name.
    pub count: usize,
    /// Unique bus numbers where this model appears.
    pub buses: Vec<u32>,
    /// Guessed category (Generator, Exciter, Governor, PSS, etc.).
    pub category: String,
    /// Suggested standard equivalent model name, if known.
    pub suggested_equivalent: Option<String>,
    /// Notes about the suggested mapping.
    pub suggestion_notes: Option<String>,
}

/// A generator with an incomplete dynamic model representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteMachine {
    /// Bus number.
    pub bus: u32,
    /// Machine ID.
    pub machine_id: String,
    /// Whether an exciter model was found.
    pub has_exciter: bool,
    /// Whether a governor model was found.
    pub has_governor: bool,
    /// Whether a PSS model was found.
    pub has_pss: bool,
}

// ---------------------------------------------------------------------------
// Generator dynamic records
// ---------------------------------------------------------------------------

/// A generator dynamic model record (GENCLS, GENROU, or GENSAL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorDyn {
    /// Bus number of the associated static generator.
    pub bus: u32,
    /// Machine ID (matches PSS/E machine ID field, e.g. `"1"`, `"G1"`, `"WND"`).
    pub machine_id: String,
    /// The specific generator model and its parameters.
    pub model: GeneratorModel,
}

/// Discriminated union of supported generator dynamic models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GeneratorModel {
    Gencls(GenclsParams),
    Genrou(GenrouParams),
    Gensal(GensalParams),
    /// REGC_A — IBR inner converter (voltage-source Norton equivalent, no swing equation).
    Regca(RegcaParams),
    Gentpj(GentpjParams),
    Genqec(GenqecParams),
    /// REGCB — enhanced IBR inner converter.
    Regcb(RegcbParams),
    /// WT3G2U — Type 3 (DFIG) wind generator.
    Wt3g2u(Wt3g2uParams),
    /// WT4G1 — Type 4 (full-converter) wind generator.
    Wt4g1(Wt4g1Params),
    /// REGFM_A1 — Grid-forming inverter (droop).
    RegfmA1(RegfmA1Params),
    /// REGFM_B1 — Grid-forming inverter (VSM).
    RegfmB1(RegfmB1Params),
    /// DER_A — Distributed energy resource aggregate.
    Dera(DeraParams),
    /// GENTRA — Third-order transient generator (3 states: δ, ω, E'q).
    Gentra(GentraParams),
    /// GENTPF — Flux-based saturation round-rotor (same state/equations as GENTPJ).
    Gentpf(GentpjParams),
    /// REGCC — Next-gen GFM-capable converter (4 states).
    Regcc(RegccParams),
    /// WT4G2 — Type 4 wind variant GE (2 states, same as WT4G1).
    Wt4g2(Wt4g2Params),
    /// DER_C / DERC — DER_A variant C (3 states).
    Derc(DercParams),
    // Phase 21
    /// GENROA — GENROU with additional AVR interface (Phase 21, same state as GENROU).
    Genroa(GenrouParams),
    /// GENSAA — GENSAL with additional AVR interface (Phase 21, same state as GENSAL).
    Gensaa(GensalParams),
    /// REGFM_C1 — Grid-forming inverter C1 (Phase 21).
    RegfmC1(RegfmC1Params),
    // Phase 22
    /// PVGU1 — WECC 1st-gen photovoltaic converter unit (Phase 22).
    Pvgu1(Pvgu1Params),
    /// PVDG — Distributed/rooftop PV aggregate model (Phase 22).
    Pvdg(PvdgParams),
    // Phase 27
    /// WT3G3 — Type 3 wind generator variant 3 (Phase 27, reuses Wt3g2u dynamics).
    Wt3g3(Wt3g2uParams),
    /// REGCO1 — Grid-following converter generator (Phase 27, 4 states).
    Regco1(Regco1Params),
    /// GENWTG — Alias for GENROU (Phase 27).
    Genwtg(GenrouParams),
    /// GENROE — GENROU with extended saturation (Phase 27, reuses GENROU params).
    Genroe(GenrouParams),
    /// GENSAL3 — Third-order salient-pole generator (Phase 27, 3 dynamic states).
    Gensal3(Gensal3Params),
    // Phase 28
    /// DERP — DER with Protection (Phase 28, 2 states).
    Derp(DerpParams),
    /// REGFM_D1 — WECC hybrid GFM/GFL converter (Phase 28, 8 states).
    RegfmD1(Regfmd1Params),
    // Wave 34
    /// GENSAE — Salient-pole with exponential saturation (reuses GensalParams).
    Gensae(GensalParams),
    // Wave 36: legacy wind and distributed PV
    /// WT1G1 — Type 1 fixed-speed wind generator (Wave 36, IBR equivalent).
    Wt1g1(Wt1g1Params),
    /// WT2G1 — Type 2 variable-slip wind generator (Wave 36, alias Wt1g1Params).
    Wt2g1(Wt1g1Params),
    /// PVD1 — Distributed PV aggregate (Wave 36, alias PvdgParams).
    Pvd1(PvdgParams),
    /// PVDU1 — PV distributed unit (Wave 36, alias Pvgu1Params).
    Pvdu1(Pvgu1Params),
}

// --- GENCLS ------------------------------------------------------------------

/// Classical generator model — swing equation only.
///
/// PSS/E params: `H  D`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenclsParams {
    /// Inertia constant (s).
    pub h: f64,
    /// Damping coefficient (pu).
    pub d: f64,
}

// --- GENROU ------------------------------------------------------------------

/// Round-rotor synchronous generator (full-order, PSS/E GENROU).
///
/// PSS/E params (14 required, Ra optional):
/// `Td0' Td0'' Tq0' Tq0'' H D Xd Xq Xd' Xq' Xd'' Xl S(1.0) S(1.2) [Ra]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenrouParams {
    /// d-axis open-circuit transient time constant (s).
    pub td0_prime: f64,
    /// d-axis open-circuit sub-transient time constant (s).
    pub td0_pprime: f64,
    /// q-axis open-circuit transient time constant (s).
    pub tq0_prime: f64,
    /// q-axis open-circuit sub-transient time constant (s).
    pub tq0_pprime: f64,
    /// Inertia constant (s).
    pub h: f64,
    /// Damping coefficient (pu).
    pub d: f64,
    /// d-axis synchronous reactance (pu).
    pub xd: f64,
    /// q-axis synchronous reactance (pu).
    pub xq: f64,
    /// d-axis transient reactance (pu).
    pub xd_prime: f64,
    /// q-axis transient reactance (pu).
    pub xq_prime: f64,
    /// d-axis sub-transient reactance (pu).
    pub xd_pprime: f64,
    /// Leakage reactance (pu).
    pub xl: f64,
    /// Saturation factor at 1.0 pu.
    pub s1: f64,
    /// Saturation factor at 1.2 pu.
    pub s12: f64,
    /// Armature resistance (pu) — optional trailing field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra: Option<f64>,
}

// --- GENSAL ------------------------------------------------------------------

/// Salient-pole synchronous generator (PSS/E GENSAL).
///
/// PSS/E params (13 required):
/// `Td0' Td0'' Tq0'' H D Xd Xq Xd' Xd'' Xl S(1.0) S(1.2) Xtran`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GensalParams {
    /// d-axis open-circuit transient time constant (s).
    pub td0_prime: f64,
    /// d-axis open-circuit sub-transient time constant (s).
    pub td0_pprime: f64,
    /// q-axis open-circuit sub-transient time constant (s).
    pub tq0_pprime: f64,
    /// Inertia constant (s).
    pub h: f64,
    /// Damping coefficient (pu).
    pub d: f64,
    /// d-axis synchronous reactance (pu).
    pub xd: f64,
    /// q-axis synchronous reactance (pu).
    pub xq: f64,
    /// d-axis transient reactance (pu).
    pub xd_prime: f64,
    /// d-axis sub-transient reactance (pu).
    pub xd_pprime: f64,
    /// Leakage reactance (pu).
    pub xl: f64,
    /// Saturation factor at 1.0 pu.
    pub s1: f64,
    /// Saturation factor at 1.2 pu.
    pub s12: f64,
    /// Transient reactance for saturation (pu).
    pub xtran: f64,
}

// --- REGCA ------------------------------------------------------------------

/// IBR inner converter model (PSS/E REGC_A).
///
/// Models the converter as a controlled current source behind a small reactance.
/// No swing equation — IBR is grid-following (ω = 1.0 always).
///
/// Key params: `Tg Xeq Imax Tfltr`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegcaParams {
    /// Converter current control time constant (s).
    pub tg: f64,
    /// Equivalent reactance for Norton shunt (pu system base) — default 0.02 pu.
    pub x_eq: f64,
    /// Maximum current magnitude limit (pu machine base).
    pub imax: f64,
    /// Voltage filter time constant (s).
    pub tfltr: f64,
    // --- Phase 1: PLL, current ramp, voltage dip parameters ---
    /// PLL proportional gain.
    pub kp_pll: f64,
    /// PLL integral gain.
    pub ki_pll: f64,
    /// Current ramp rate limit (pu/s) for LVACM/current recovery.
    pub rrpwr: f64,
    /// Low-voltage threshold for momentary cessation (pu).
    pub vdip: f64,
    /// High-voltage threshold for momentary cessation (pu).
    pub vup: f64,
}

// ---------------------------------------------------------------------------
// Phase 11: IBR / Wind / GFM / DER generator records
// ---------------------------------------------------------------------------

/// REGCB — enhanced IBR inner converter with IP filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegcbParams {
    pub tg: f64,
    pub x_eq: f64,
    pub imax: f64,
    pub tfltr: f64,
    pub tip: f64,
    /// PLL proportional gain.
    pub kp_pll: f64,
    /// PLL integral gain.
    pub ki_pll: f64,
}

/// WT3G2U — Type 3 (DFIG) wind turbine generator.
///
/// Full DFIG model: RSC controls d/q rotor currents via first-order lags
/// (`t_rotor`). Electrical torque couples through mutual inductance ratio
/// `lm_over_ls`. PLL tracks grid voltage angle with PI gains `kpll`/`kipll`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt3g2uParams {
    /// Current command time constant (s) — lag from exciter ip/iq_ref to ip/iq_cmd.
    pub tg: f64,
    /// Equivalent source reactance (pu).
    pub x_eq: f64,
    /// Maximum current limit (pu).
    pub imax: f64,
    /// Voltage filter time constant (s).
    pub tfltr: f64,
    /// PLL proportional gain.
    pub kpll: f64,
    /// PLL integral gain (rad/s per pu freq error).
    pub kipll: f64,
    /// Rotor inertia constant (s, MWs/MVA).
    pub h_rotor: f64,
    /// Rotor damping coefficient.
    pub d_rotor: f64,
    /// Rotor current time constant (s) — RSC current control lag.
    /// Default 0.02 s if zero or absent.
    pub t_rotor: f64,
    /// Mutual-to-stator inductance ratio Lm/Ls (pu).
    /// Default 0.9 if zero or absent (typical DFIG value).
    pub lm_over_ls: f64,
}

/// WT4G1 — Type 4 (full-converter) wind turbine generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt4g1Params {
    pub tg: f64,
    pub x_eq: f64,
    pub imax: f64,
    /// PLL proportional gain.
    pub kp_pll: f64,
    /// PLL integral gain.
    pub ki_pll: f64,
}

/// REGFM_A1 — Grid-forming inverter (droop control, 9 dynamic states).
///
/// ## States
///
/// 1. δ — angle (rad)
/// 2. ω — speed (pu)
/// 3. i_d — d-axis current command (pu)
/// 4. i_q — q-axis current command (pu)
/// 5. v_filt — voltage measurement filter (pu)
/// 6. x_pll — PLL frequency estimation state
/// 7. x_vi — virtual impedance filter state (pu)
/// 8. x_droop_p — P-f droop integrator (pu)
/// 9. x_droop_q — Q-V droop integrator (pu)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegfmA1Params {
    pub x_eq: f64,
    pub h: f64,
    pub d: f64,
    pub imax: f64,
    /// Current injection time constant (s, default 0.02).
    pub tg: f64,
    /// Voltage measurement filter time constant (s, default 0.02).
    #[serde(default = "default_gfm_tv")]
    pub tv: f64,
    /// PLL time constant (s, default 0.02).
    #[serde(default = "default_gfm_tpll")]
    pub tpll: f64,
    /// Virtual impedance filter time constant (s, default 0.05).
    #[serde(default = "default_gfm_tvi")]
    pub tvi: f64,
    /// P-f droop gain (pu power / pu freq, default 20.0 = 5% droop).
    #[serde(default = "default_gfm_kp_droop")]
    pub kp_droop: f64,
    /// P-f droop integral gain (default 0.0 = pure proportional).
    #[serde(default)]
    pub ki_droop: f64,
    /// Q-V droop gain (pu reactive / pu voltage, default 20.0).
    #[serde(default = "default_gfm_kq_droop")]
    pub kq_droop: f64,
    /// Q-V droop integral gain (default 0.0).
    #[serde(default)]
    pub ki_q: f64,
    /// Virtual resistance (pu, default 0.0).
    #[serde(default)]
    pub r_vi: f64,
    /// Virtual reactance (pu, default 0.1).
    #[serde(default = "default_gfm_x_vi")]
    pub x_vi: f64,
}

fn default_gfm_tv() -> f64 {
    0.02
}
fn default_gfm_tpll() -> f64 {
    0.02
}
fn default_gfm_tvi() -> f64 {
    0.05
}
fn default_gfm_kp_droop() -> f64 {
    20.0
}
fn default_gfm_kq_droop() -> f64 {
    20.0
}
fn default_gfm_x_vi() -> f64 {
    0.1
}

/// REGFM_B1 — Grid-forming inverter (virtual synchronous machine, 9 dynamic states).
///
/// Same state structure as RegfmA1 but with VSM control philosophy:
/// the swing equation emulates a synchronous machine rather than direct droop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegfmB1Params {
    pub x_eq: f64,
    pub h: f64,
    pub d: f64,
    pub imax: f64,
    /// Current injection time constant (s, default 0.02).
    pub tg: f64,
    /// Voltage measurement filter time constant (s, default 0.02).
    #[serde(default = "default_gfm_tv")]
    pub tv: f64,
    /// PLL time constant (s, default 0.02).
    #[serde(default = "default_gfm_tpll")]
    pub tpll: f64,
    /// Virtual impedance filter time constant (s, default 0.05).
    #[serde(default = "default_gfm_tvi")]
    pub tvi: f64,
    /// P-f droop gain (pu power / pu freq, default 20.0 = 5% droop).
    #[serde(default = "default_gfm_kp_droop")]
    pub kp_droop: f64,
    /// P-f droop integral gain (default 0.0).
    #[serde(default)]
    pub ki_droop: f64,
    /// Q-V droop gain (pu reactive / pu voltage, default 20.0).
    #[serde(default = "default_gfm_kq_droop")]
    pub kq_droop: f64,
    /// Q-V droop integral gain (default 0.0).
    #[serde(default)]
    pub ki_q: f64,
    /// Virtual resistance (pu, default 0.0).
    #[serde(default)]
    pub r_vi: f64,
    /// Virtual reactance (pu, default 0.1).
    #[serde(default = "default_gfm_x_vi")]
    pub x_vi: f64,
}

/// DER_A — Distributed energy resource aggregate model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeraParams {
    pub x_eq: f64,
    pub trf: f64,
    pub imax: f64,
    pub trv: f64,
}

// ---------------------------------------------------------------------------
// Phase 15: Generator variants
// ---------------------------------------------------------------------------

/// GENTRA — Third-order transient generator (3 states: δ, ω, E'q).
///
/// Simplified round-rotor model: classical swing + single transient EMF.
/// No sub-transient dynamics (Xd'' = Xd', Td0'' → ∞).
///
/// PSS/E params: `H D Ra Xd Xd' Td0' Xq`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GentraParams {
    /// Inertia constant (s).
    pub h: f64,
    /// Damping coefficient (pu).
    pub d: f64,
    /// Armature resistance (pu).
    pub ra: f64,
    /// d-axis synchronous reactance (pu).
    pub xd: f64,
    /// d-axis transient reactance (pu).
    pub xd_prime: f64,
    /// d-axis open-circuit transient time constant (s).
    pub td0_prime: f64,
    /// q-axis synchronous reactance (pu).
    pub xq: f64,
    /// Saturation factor at 1.0 pu (0.0 = unsaturated).
    pub s1: f64,
    /// Saturation factor at 1.2 pu (0.0 = unsaturated).
    pub s12: f64,
}

/// REGCC — Next-gen GFM-capable converter (4 states).
///
/// Grid-following converter with PLL state for frequency tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegccParams {
    /// Converter current control time constant (s).
    pub tg: f64,
    /// Equivalent reactance (pu system base).
    pub x_eq: f64,
    /// Maximum current magnitude (pu machine base).
    pub imax: f64,
    /// Voltage filter time constant (s).
    pub tfltr: f64,
    /// PLL time constant (s).
    pub t_pll: f64,
}

/// WT4G2 — Type 4 wind generator variant GE (2 states, same as WT4G1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt4g2Params {
    pub tg: f64,
    pub x_eq: f64,
    pub imax: f64,
    /// PLL proportional gain.
    pub kp_pll: f64,
    /// PLL integral gain.
    pub ki_pll: f64,
}

/// DER_C / DERC — DER_A variant C (3 states: p_rec, q_rec, vfilt).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DercParams {
    pub tp: f64,
    pub tq: f64,
    pub tv: f64,
    pub mbase: f64,
    pub lfac: f64,
    /// Norton equivalent reactance (pu, default 0.02).
    pub x_eq: f64,
}

// ---------------------------------------------------------------------------
// Exciter records
// ---------------------------------------------------------------------------

/// An excitation system dynamic model record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExciterDyn {
    pub bus: u32,
    pub machine_id: String,
    pub model: ExciterModel,
}

/// REEC_D — IBR electrical controller (drives voltage recovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReecdParams {
    /// Voltage deadband lower (pu, typically negative).
    pub dbd1: f64,
    /// Voltage deadband upper (pu, typically positive).
    pub dbd2: f64,
    /// Reactive current PI proportional gain.
    pub kqv: f64,
    /// Reactive current PI integral gain.
    pub kqi: f64,
    /// Voltage measurement filter time constant (s).
    pub trv: f64,
    /// Active power measurement filter time constant (s).
    pub tp: f64,
    /// Maximum reactive current (pu).
    pub iqmax: f64,
    /// Minimum reactive current (pu).
    pub iqmin: f64,
    /// Maximum active current (pu).
    pub ipmax: f64,
    /// Active power ramp rate limit (pu/s).
    pub rrpwr: f64,
    /// Frequency droop gain (down).
    pub ddn: f64,
    /// Frequency droop gain (up).
    pub dup: f64,
    /// Frequency deadband lower (Hz).
    pub fdbd1: f64,
    /// Frequency deadband upper (Hz).
    pub fdbd2: f64,
    /// Voltage dip threshold for momentary cessation (pu).
    pub vdip: f64,
    /// Voltage up threshold for momentary cessation (pu).
    pub vup: f64,
    /// Active power reference (pu).
    pub pref: f64,
    /// Max/min active power (pu).
    pub pmax: f64,
    pub pmin: f64,
}

/// REECCU / REECCU1 — IBR electrical controller, current-unlimited (PI + ramp).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReeccuParams {
    /// Voltage deadband (pu).
    pub dbd1: f64,
    /// Reactive current PI proportional gain.
    pub kqv: f64,
    /// Reactive current PI integral gain.
    pub kqi: f64,
    /// Voltage measurement filter time constant (s).
    pub trv: f64,
    /// Active power measurement filter time constant (s).
    pub tp: f64,
    /// Active power ramp rate limit (pu/s).
    pub rrpwr: f64,
    /// Voltage dip threshold for momentary cessation (pu).
    pub vdip: f64,
    /// Voltage up threshold for momentary cessation (pu).
    pub vup: f64,
    /// Active power reference (pu).
    pub pref: f64,
    /// Max/min active power (pu).
    pub pmax: f64,
    pub pmin: f64,
}

/// REXS — Excitation system with rate feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RexsParams {
    pub te: f64,
    pub tf: f64,
    pub ke: f64,
    pub kf: f64,
    pub efd1: f64,
    pub efd2: f64,
    pub sefd1: f64,
    pub sefd2: f64,
    /// Lead-lag numerator time constant (s). Zero = no lead-lag.
    pub tc: f64,
    /// Lead-lag denominator time constant (s). Zero = no lead-lag.
    pub tb: f64,
}

/// Discriminated union of supported exciter models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ExciterModel {
    Exst1(Exst1Params),
    Esst3a(Esst3aParams),
    Esdc2a(Esdc2aParams),
    Exdc2(Exdc2Params),
    Ieeex1(Ieeex1Params),
    /// SEXS — Simplified Exciter (common in planning studies).
    Sexs(SexsParams),
    /// IEEET1 — IEEE Type 1 rotating exciter (classic 1968 AVR).
    Ieeet1(Ieeet1Params),
    /// SCRX — Simplified Bus-Fed Static Exciter.
    Scrx(ScrxParams),
    /// REEC_A — IBR electrical controller (maps to exciter slot).
    Reeca(ReecaParams),
    Esst1a(Esst1aParams),
    Exac1(Exac1Params),
    /// ESAC1A — same params as EXAC1, different model tag.
    Esac1a(Exac1Params),
    Esac7b(Esac7bParams),
    Esst4b(Esst4bParams),
    /// REEC_D — IBR electrical controller (voltage recovery).
    Reecd(ReecdParams),
    /// REECCU — IBR electrical controller (curtailment).
    Reeccu(ReeccuParams),
    /// REXS — Excitation system with rate feedback.
    Rexs(RexsParams),
    /// ESAC2A — AC2A high-initial-response rotating exciter (Phase 14).
    Esac2a(Esac2aParams),
    /// ESAC5A — AC5A simplified brushless exciter (Phase 14).
    Esac5a(Esac5aParams),
    /// ESST5B — IEEE ST5B static exciter (Phase 15, 3 states).
    Esst5b(Esst5bParams),
    /// EXAC4 / AC4A — IEEE AC4A controlled-rectifier exciter (Phase 15, 2 states).
    Exac4(Exac4Params),
    // Phase 17
    /// ESST6B — IEEE ST6B Static Exciter (Phase 17).
    Esst6b(Esst6bParams),
    /// ESST7B — IEEE ST7B Static Exciter (Phase 17).
    Esst7b(Esst7bParams),
    /// ESAC6A — AC6A Rotating Exciter (Phase 17).
    Esac6a(Esac6aParams),
    /// ESDC1A — DC1A Rotating Exciter (Phase 17).
    Esdc1a(Esdc1aParams),
    /// EXST2 — Static Exciter Type ST2 (Phase 17).
    Exst2(Exst2Params),
    /// AC8B — IEEE AC8B High Initial Response Exciter (Phase 17).
    Ac8b(Ac8bParams),
    /// BBSEX1 — Bus-Branch Static Exciter 1 (Phase 17).
    Bbsex1(Bbsex1Params),
    /// IEEET3 — IEEE Type 3 Rotating Exciter (Phase 17).
    Ieeet3(Ieeet3Params),
    // Phase 19
    /// WT3E1 — Type 3 Wind Electrical Controller (Phase 19).
    Wt3e1(Wt3e1Params),
    /// WT3E2 — Type 3 Wind Electrical Controller Variant 2 (Phase 19).
    Wt3e2(Wt3e2Params),
    /// WT4E1 — Type 4 Wind Electrical Controller (Phase 19).
    Wt4e1(Wt4e1Params),
    /// WT4E2 — Type 4 Wind Electrical Controller Variant 2 (Phase 19).
    Wt4e2(Wt4e1Params),
    /// REPCB — REPCA Variant B (Phase 19).
    Repcb(RepcbParams),
    /// REPCC — REPCA Variant C (Phase 19, same params as REPCB).
    Repcc(RepcbParams),
    // Phase 21
    /// EXST3 — Static Exciter Type ST3 (Phase 21).
    Exst3(Exst3Params),
    /// CBUFR — Buffer-Frequency-Regulated BESS (Phase 21).
    Cbufr(CbufrParams),
    /// CBUFD — Buffer-Frequency-Dependent BESS (Phase 21).
    Cbufd(CbufdParams),
    // Phase 22
    /// PVEU1 — WECC 1st-gen PV electrical control unit (Phase 22, maps to exciter slot).
    Pveu1(Pveu1Params),
    // Phase 23
    /// IEEET2 — IEEE Type 2 rotating-machine exciter (Phase 23).
    Ieeet2(Ieeet2Params),
    /// EXAC2 — IEEE AC2A high initial response rotating exciter (Phase 23).
    Exac2(Exac2Params),
    /// EXAC3 — IEEE AC3A controlled-rectifier exciter (Phase 23).
    Exac3(Exac3Params),
    /// ESAC3A — IEEE 421.5-2005 AC3A exciter update (Phase 23).
    Esac3a(Esac3aParams),
    /// ESST8C — IEEE 421.5-2016 ST8C static exciter (Phase 23).
    Esst8c(Esst8cParams),
    /// ESST9B — IEEE 421.5-2016 ST9B static exciter (Phase 23).
    Esst9b(Esst9bParams),
    /// ESST10C — IEEE 421.5-2016 ST10C static exciter (Phase 23).
    Esst10c(Esst10cParams),
    /// ESDC3A — IEEE 421.5-2005 DC3A rotating-machine exciter (Phase 23).
    Esdc3a(Esdc3aParams),
    // Wave 32
    /// EXDC1 — IEEE Type DC1A rotating-machine exciter (legacy 13-param form).
    Exdc1(Exdc1Params),
    /// ESST2A — IEEE 421.5-2016 Type ST2A static exciter.
    Esst2a(Esst2aParams),
    // Wave 33
    /// EXDC3 — PSS/E non-continuously-acting (relay-type) DC exciter (legacy).
    Exdc3(Exdc3Params),
    // Phase 27
    /// WT3C2 — Type 3 wind electrical controller variant 2 (Phase 27, same params as WT3E1).
    Wt3c2(Wt3e1Params),
    // Wave 35
    /// ESAC7C — IEEE 421.5-2016 AC7C exciter (same structure as ESAC7B, C-series).
    Esac7c(Esac7cParams),
    /// ESDC4C — IEEE 421.5-2016 DC4C exciter (PID + DC rotating, 3 states).
    Esdc4c(Esdc4cParams),
    // Wave 36: new REEC variants
    /// REECBU1 — REEC variant B for Unit (Wave 36, alias ReeccuParams).
    Reecbu1(ReeccuParams),
    /// REECE — Enhanced REECA with voltage ride-through (Wave 36, alias ReecaParams).
    Reece(ReecaParams),
    /// REECEU1 — REECE curtailment variant (Wave 36, alias ReeccuParams).
    Reeceu1(ReeccuParams),
    // Wave 37: IEEE 421.5-2016 C-series AC exciters
    /// ESAC8C — IEEE 421.5-2016 AC8C high-initial-response exciter (alias AC8B structure).
    Esac8c(Ac8bParams),
    /// ESAC9C — IEEE 421.5-2016 AC9C exciter (alias ESAC7B structure).
    Esac9c(Esac7bParams),
    /// ESAC10C — IEEE 421.5-2016 AC10C exciter (alias ESAC7C structure).
    Esac10c(Esac7cParams),
    /// ESAC11C — IEEE 421.5-2016 AC11C high-bandwidth AC exciter (alias AC8B structure).
    Esac11c(Ac8bParams),
}

// --- EXST1 ------------------------------------------------------------------

/// Static exciter (PSS/E EXST1).
///
/// PSS/E params (12 required; klr, ilr optional):
/// `TR VIMAX VIMIN TC TB KA TA VRMAX VRMIN KC KF TF [KLR ILR]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exst1Params {
    pub tr: f64,
    pub vimax: f64,
    pub vimin: f64,
    pub tc: f64,
    pub tb: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kc: f64,
    pub kf: f64,
    pub tf: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub klr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilr: Option<f64>,
}

// --- ESST3A -----------------------------------------------------------------

/// IEEE type ST3A static exciter (PSS/E ESST3A).
///
/// PSS/E params (14 required):
/// `TR VIMAX VIMIN KM TC TB KA TA VRMAX VRMIN KG KP KI VBMAX`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst3aParams {
    pub tr: f64,
    pub vimax: f64,
    pub vimin: f64,
    pub km: f64,
    pub tc: f64,
    pub tb: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kg: f64,
    pub kp: f64,
    pub ki: f64,
    pub vbmax: f64,
}

// --- ESDC2A -----------------------------------------------------------------

/// IEEE type DC2A exciter (PSS/E ESDC2A).
///
/// PSS/E params (12 required):
/// `TR KA TA TB TC VRMAX VRMIN KE TE KF TF1 SWITCH`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esdc2aParams {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub tb: f64,
    pub tc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf1: f64,
    pub switch_: f64,
}

// --- EXDC2 ------------------------------------------------------------------

/// IEEE type DC2 exciter (PSS/E EXDC2) — used in Kundur 4-machine system.
///
/// PSS/E params (12 required; E1, SE1, E2, SE2 optional):
/// `TR KA TA TB TC VRMAX VRMIN KE TE KF TF1 SWITCH [E1 SE1 E2 SE2]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exdc2Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub tb: f64,
    pub tc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf1: f64,
    pub switch_: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub se1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub se2: Option<f64>,
}

// --- IEEEX1 -----------------------------------------------------------------

/// IEEE type AC1A exciter (PSS/E IEEEX1 / IEEEXC1).
///
/// PSS/E params (12 required; E1, SE1, E2, SE2 optional):
/// `TR KA TA TB TC VRMAX VRMIN KE TE KF TF AEX BEX [E1 SE1 E2 SE2]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeex1Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub tb: f64,
    pub tc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub aex: f64,
    pub bex: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub se1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub se2: Option<f64>,
}

// --- SEXS -------------------------------------------------------------------

/// Simplified Exciter (PSS/E SEXS) -- very common in planning studies.
///
/// This is a standard planning-level model for use when detailed AVR test data
/// is unavailable.  For precise transient stability studies, upgrade to a
/// detailed exciter model (AC5A, ESST4B, or EXAC1) when field test data is
/// available.  See the module-level documentation for the full model mapping.
///
/// PSS/E params: `TB TC K TE EMIN EMAX`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SexsParams {
    /// Lead-lag denominator time constant (s).
    pub tb: f64,
    /// Lead-lag numerator time constant (s).
    pub tc: f64,
    /// Exciter gain.
    pub k: f64,
    /// Exciter time constant (s).
    pub te: f64,
    /// Minimum field voltage limit (pu).
    pub emin: f64,
    /// Maximum field voltage limit (pu).
    pub emax: f64,
}

// --- IEEET1 -----------------------------------------------------------------

/// IEEE Type 1 Rotating Exciter (PSS/E IEEET1) — classic 1968 AVR.
///
/// PSS/E params: `TR KA TA KE TE KF TF E1 SE1 E2 SE2 [VRMAX VRMIN]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeet1Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    /// Saturation function reference point 1 (pu).
    pub e1: f64,
    /// Saturation factor at E1.
    pub se1: f64,
    /// Saturation function reference point 2 (pu).
    pub e2: f64,
    /// Saturation factor at E2.
    pub se2: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vrmax: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vrmin: Option<f64>,
}

// --- SCRX -------------------------------------------------------------------

/// Simplified Bus-Fed Static Exciter (PSS/E SCRX).
///
/// This is a standard planning-level model for static exciters when detailed
/// parameters are unavailable.  For precise transient stability studies,
/// upgrade to ESST4B or ESST3A when static exciter field test data is available.
/// See the module-level documentation for the full model mapping.
///
/// PSS/E params: `TR K TE EMIN EMAX [Rcrfd]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrxParams {
    /// Transducer time constant (s).
    pub tr: f64,
    /// Exciter gain.
    pub k: f64,
    /// Exciter time constant (s).
    pub te: f64,
    /// Minimum field voltage (pu).
    pub emin: f64,
    /// Maximum field voltage (pu).
    pub emax: f64,
    /// Ratio of exciter to field current (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rcrfd: Option<f64>,
}

// --- REECA ------------------------------------------------------------------

/// REEC_A -- IBR electrical controller (maps to exciter slot, Phase 8 simplified).
///
/// Simplified to voltage-following / constant power factor mode.
/// Full REEC_A includes Kqv droop, deadband, FRT limits.
///
/// This is a standard planning-level model for IBR electrical controllers.
/// For studies involving IBR fault ride-through, voltage regulation, or reactive
/// power control, use the full REEC_A model with Kqv droop and deadband parameters.
/// See the module-level documentation for the full model mapping.
///
/// Key params: `Trv Kqv Tp Kqp Kqi Vref0 Dbd1 Dbd2 Vdip Vup Iqh1 Iql1 Qmax Qmin`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReecaParams {
    /// Voltage filter time constant (s).
    pub trv: f64,
    /// Reactive power control voltage droop gain.
    pub kqv: f64,
    /// Active current filter time constant (s).
    pub tp: f64,
    /// Reactive power proportional gain.
    pub kqp: f64,
    /// Reactive power integral gain (1/s).
    pub kqi: f64,
    /// Initial voltage reference (pu).
    pub vref0: f64,
    /// Voltage deadband lower limit (pu, negative).
    pub dbd1: f64,
    /// Voltage deadband upper limit (pu, positive).
    pub dbd2: f64,
    /// Low voltage protection threshold (pu).
    pub vdip: f64,
    /// High voltage protection threshold (pu).
    pub vup: f64,
    /// Maximum reactive current (pu).
    pub iqh1: f64,
    /// Minimum reactive current (pu).
    pub iql1: f64,
    /// Maximum reactive power (pu).
    pub qmax: f64,
    /// Minimum reactive power (pu).
    pub qmin: f64,
    /// Active power measurement filter time constant (s).
    pub tpfilt: f64,
    /// Reactive power measurement filter time constant (s).
    pub tqfilt: f64,
    /// Active power ramp rate up limit (pu/s).
    pub rrpwr: f64,
    /// Active power ramp rate down limit (pu/s, negative).
    pub rrpwr_dn: f64,
    /// Current priority flag: 0 = Q priority, 1 = P priority.
    pub pqflag: i32,
    /// Maximum total current magnitude (pu).
    pub imax: f64,
    /// Maximum active current (pu, used with PQFLAG=1).
    pub ipmax: f64,
    /// VDL1 breakpoints: voltage-dependent reactive current limit (Vq, Iq) pairs.
    /// If all zeros or empty, flat iqh1/iql1 limits apply.
    pub vdl1: [(f64, f64); 4],
    /// VDL2 breakpoints: voltage-dependent active current limit (Vp, Ip) pairs.
    /// If all zeros or empty, flat ipmax limit applies.
    pub vdl2: [(f64, f64); 4],
}

// ---------------------------------------------------------------------------
// Governor records
// ---------------------------------------------------------------------------

/// A turbine-governor dynamic model record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorDyn {
    pub bus: u32,
    pub machine_id: String,
    pub model: GovernorModel,
}

/// REPCD — IBR plant power controller (drives active power).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepdcParams {
    pub tp: f64,
    pub kpg: f64,
    pub kig: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub tlag: f64,
}

/// WT3T1 — Type 3 wind turbine drive train.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt3t1Params {
    pub h: f64,
    pub damp: f64,
    pub ka: f64,
    pub theta: f64,
}

/// WT3P1 — Type 3 wind turbine pitch controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt3p1Params {
    pub tp: f64,
    pub kpp: f64,
    pub kip: f64,
    pub pmax: f64,
    pub pmin: f64,
}

/// GGOV1D — Enhanced GGOV1 with droop deadband.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ggov1dParams {
    pub r: f64,
    pub t_pelec: f64,
    pub maxerr: f64,
    pub minerr: f64,
    pub kpgov: f64,
    pub kigov: f64,
    pub kdgov: f64,
    pub fdbd1: f64,
    pub fdbd2: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub tact: f64,
    pub kturb: f64,
    pub wfnl: f64,
    pub tb: f64,
    pub tc: f64,
    pub flag: f64,
    pub teng: f64,
    pub tfload: f64,
    pub kpload: f64,
    pub kiload: f64,
    pub ldref: f64,
    pub dm: f64,
    pub ropen: f64,
    pub rclose: f64,
    pub kimw: f64,
    pub pmwset: f64,
    pub aset: f64,
    pub ka: f64,
    pub ta: f64,
    pub db: f64,
    pub tsa: f64,
    pub tsb: f64,
    pub rup: f64,
    pub rdown: f64,
    pub load_ref: f64,
}

/// TGOV1N / TGOV1NDB — TGOV1 with null deadband.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tgov1nParams {
    pub r: f64,
    pub dt: f64,
    pub t1: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub t2: f64,
    pub t3: f64,
    pub d: f64,
    pub db: f64,
}

/// Discriminated union of supported governor models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GovernorModel {
    Tgov1(Tgov1Params),
    Ieeeg1(Ieeeg1Params),
    Ggov1(Ggov1Params),
    /// GAST — Gas Turbine Simplified governor (Rowen model).
    Gast(GastParams),
    /// REPC_A — IBR plant controller (maps to governor slot, simplified Phase 8).
    Repca(RepcaParams),
    Hygov(HygovParams),
    Hygovd(HygovdParams),
    Tgov1d(Tgov1dParams),
    Ieeeg1d(Ieeeg1dParams),
    /// WSIEG1 — WECC IEEEG1 (same structure as Ieeeg1).
    Wsieg1(Ieeeg1Params),
    Ieeeg2(Ieeeg2Params),
    /// REPCD — IBR plant power controller.
    Repcd(RepdcParams),
    /// WT3T1 — Type 3 wind drive train.
    Wt3t1(Wt3t1Params),
    /// WT3P1 — Type 3 wind pitch controller.
    Wt3p1(Wt3p1Params),
    /// GGOV1D — Enhanced GGOV1 with droop deadband.
    Ggov1d(Ggov1dParams),
    /// TGOV1N / TGOV1NDB — TGOV1 with null deadband.
    Tgov1n(Tgov1nParams),
    /// CBEST — PSS/E native BESS model (Phase 14).
    Cbest(CbestParams),
    /// CHAAUT — BESS active power controller with frequency droop (Phase 14).
    Chaaut(ChaautParams),
    /// PIDGOV — PID governor for any prime mover (Phase 14).
    Pidgov(PidgovParams),
    /// DEGOV1 — Diesel governor Type 1 (Phase 14).
    Degov1(Degov1Params),
    /// TGOV5 — Multi-reheat steam governor HP+IP (Phase 15, 4 states).
    Tgov5(Tgov5Params),
    /// GAST2A — Advanced Rowen gas turbine with ambient temperature (Phase 15, 4 states).
    Gast2a(Gast2aParams),
    // Phase 18
    /// H6E — Hydro Governor 6 Elements (Phase 18).
    H6e(H6eParams),
    /// WSHYGP — Wind-Synchronous Hydro Governor+Pitch (Phase 18).
    Wshygp(WshygpParams),
    // Phase 25
    /// GGOV2 — GE GGOV1 variant 2 with supplemental load reference input (Phase 25, 4 states).
    Ggov2(Ggov2Params),
    /// GGOV3 — GE GGOV1 variant 3 with washout filter (Phase 25, 4 states).
    Ggov3(Ggov3Params),
    /// WPIDHY — Woodward PID Hydro Governor (Phase 25, 4 states).
    Wpidhy(WpidhyParams),
    /// H6B — Six-State Hydro Governor Variant B (Phase 25, 5 states).
    H6b(H6bParams),
    /// WSHYDD — WSHYGP with speed deadband (Phase 25, 4 states).
    Wshydd(WshyddParams),
    // Phase 28
    /// REPCGFM_C1 — GFM plant Volt/Var controller (Phase 28, 3 states).
    Repcgfmc1(Repcgfmc1Params),
    /// WTDTA1 — Wind turbine two-mass drive-train (Phase 28, 2 states).
    Wtdta1(Wtdta1Params),
    /// WTARA1 — Wind turbine aerodynamic aggregation (Phase 28, 2 states).
    Wtara1(Wtara1Params),
    /// WTPTA1 — Wind turbine pitch angle control (Phase 28, 2 states).
    Wtpta1(Wtpta1Params),
    // Wave 34
    /// IEESGO — IEEE Standard Governor (5-state steam turbine).
    Ieesgo(IeesgoParams),
    /// WTTQA1 — WECC Type 2 Wind Torque Controller (2 states).
    Wttqa1(Wttqa1Params),
    // Wave 35
    /// HYGOV4 — Hydro Governor with Surge Tank (5 states).
    Hygov4(Hygov4Params),
    /// WEHGOV — WECC Enhanced Hydro Governor (4 states).
    Wehgov(WehgovParams),
    /// IEEEG3 — IEEE Type G3 Hydro Governor (3 states).
    Ieeeg3(Ieeeg3Params),
    /// IEEEG4 — IEEE Type G4 Hydro Governor (3 states, lead-lag form).
    Ieeeg4(Ieeeg4Params),
    // Wave 36: combined cycle + steam + wind governors
    /// GOVCT1 — Single-shaft combined cycle turbine governor (Wave 36, 5 states).
    Govct1(Govct1Params),
    /// GOVCT2 — Two-shaft combined cycle turbine governor (7 states: 5 GT + HRSG + ST).
    Govct2(Govct2Params),
    /// TGOV3 — TGOV1 variant with two-reheat steam turbine (Wave 36, 3 states).
    Tgov3(Tgov3Params),
    /// TGOV4 — TGOV1 with IP/LP split (Wave 36, alias Tgov3Params).
    Tgov4(Tgov3Params),
    /// WT2E1 — Type 2 wind electrical controller (Wave 36, 2 states).
    Wt2e1(Wt2e1Params),
    /// WT12T1 — Type 1/2 wind drive train (Wave 36, alias Wt3t1Params).
    Wt12t1(Wt3t1Params),
    /// WT12A1 — Type 1/2 wind aerodynamics (Wave 36, alias Wt3p1Params).
    Wt12a1(Wt3p1Params),
    /// WTAERO — Full aerodynamic Cp(λ,β) wind turbine model (B4).
    Wtaero(WtaeroParams),
}

// --- TGOV1 ------------------------------------------------------------------

/// Steam turbine-governor (PSS/E TGOV1).
///
/// PSS/E params (6 required; DT optional):
/// `R T1 VMAX VMIN T2 T3 [DT]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tgov1Params {
    pub r: f64,
    pub t1: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub t2: f64,
    pub t3: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dt: Option<f64>,
}

// --- IEEEG1 -----------------------------------------------------------------

/// IEEE type G1 turbine-governor (PSS/E IEEEG1).
///
/// PSS/E params (9 required; K1..K8, T5..T7 optional):
/// `K T1 T2 T3 UO UC PMAX PMIN T4 [K1 K2 T5 K3 K4 T6 K5 K6 T7 K7 K8]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeeg1Params {
    pub k: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub uo: f64,
    pub uc: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub t4: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t5: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k3: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k4: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t6: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k5: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k6: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t7: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k7: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k8: Option<f64>,
}

// ---------------------------------------------------------------------------
// PSS records
// ---------------------------------------------------------------------------

/// A power system stabilizer dynamic model record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PssDyn {
    pub bus: u32,
    pub machine_id: String,
    pub model: PssModel,
}

/// Discriminated union of supported PSS models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PssModel {
    Ieeest(IeeestParams),
    St2cut(St2cutParams),
    Pss2a(Pss2aParams),
    Pss2b(Pss2bParams),
    Stab1(Stab1Params),
    /// PSS1A — Single-input single lead-lag PSS (Phase 14).
    Pss1a(Pss1aParams),
    /// STAB2A — WSCC stabilizer variant A, double lead-lag (Phase 15, 3 states).
    Stab2a(Stab2aParams),
    /// PSS4B — Four-band multi-frequency PSS (Phase 15, 4 states).
    Pss4b(Pss4bParams),
    // Phase 18
    /// STAB3 — Three-Band PSS (Phase 18).
    Stab3(Stab3Params),
    /// PSS3B — Three-Input Power System Stabilizer (Phase 18).
    Pss3b(Pss3bParams),
    // Phase 24
    /// PSS2C — PSS2B with ramp-tracking filter on input 2 (Phase 24).
    Pss2c(Pss2cParams),
    /// PSS5 — Five-band multi-frequency PSS (Phase 24).
    Pss5(Pss5Params),
    /// PSS6C — Six-input multi-band PSS (Phase 24).
    Pss6c(Pss6cParams),
    /// PSSSB — WSCC/BPA simple PSS vendor variant B (Phase 24).
    Psssb(PsssbParams),
    /// STAB4 — WSCC stabilizer variant 4 (Phase 24).
    Stab4(Stab4Params),
    /// STAB5 — WSCC stabilizer variant 5 (Phase 24).
    Stab5(Stab5Params),
    // Wave 35
    /// PSS3C — IEEE 421.5-2016 3-band PSS (alias to PSS3B params/dynamics).
    Pss3c(Pss3bParams),
    /// PSS4C — IEEE 421.5-2016 4-band PSS (alias to PSS4B params/dynamics).
    Pss4c(Pss4bParams),
    /// PSS5C — IEEE 421.5-2016 5-band PSS (alias to PSS5 params/dynamics).
    Pss5c(Pss5Params),
    /// PSS7C — IEEE 421.5-2016 7-input multi-band PSS (2 states: washout + lead-lag).
    Pss7c(Pss7cParams),
}

// --- IEEEST -----------------------------------------------------------------

/// IEEE standard PSS (PSS/E IEEEST).
///
/// PSS/E params (13 required; LSMAX, LSMIN, VCU, VCL optional):
/// `A1 A2 A3 A4 A5 A6 T1 T2 T3 T4 T5 T6 KS [LSMAX LSMIN VCU VCL]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IeeestParams {
    pub a1: f64,
    pub a2: f64,
    pub a3: f64,
    pub a4: f64,
    pub a5: f64,
    pub a6: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub t6: f64,
    pub ks: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsmax: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsmin: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcu: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcl: Option<f64>,
}

// --- GGOV1 ------------------------------------------------------------------

/// General-purpose turbine-governor model (PSS/E GGOV1).
///
/// Simplified 3-state implementation capturing the key dynamics of gas turbines
/// and combined-cycle units.  Covers ~40% of ERCOT/SPP generation fleet.
///
/// PSS/E params (17 required):
/// `R RSELECT TPELEC MAXERR MINERR KPGOV KIGOV KDGOV TDGOV VMAX VMIN TSA FSR TSB TSE IANG KCMF KTURB WFNL TB TC TRATE FLAG`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ggov1Params {
    /// Droop (pu) — speed regulation.
    pub r: f64,
    /// Electrical power transducer time constant (s).
    pub tpelec: f64,
    /// Maximum governor output (pu on machine base).
    pub vmax: f64,
    /// Minimum governor output (pu on machine base).
    pub vmin: f64,
    /// Proportional governor gain.
    pub kpgov: f64,
    /// Integral governor gain (1/s).
    pub kigov: f64,
    /// Turbine gain.
    pub kturb: f64,
    /// No-load fuel flow (pu on machine base).
    pub wfnl: f64,
    /// Turbine reheat lead time constant (s).
    pub tb: f64,
    /// Turbine reheat lag time constant (s).
    pub tc: f64,
    /// Rated turbine power (MW, 0 = use machine mbase).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trate: Option<f64>,
    /// Load reference set point (pu) — initialized to Pm0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ldref: Option<f64>,
    /// Damping constant (pu).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dm: Option<f64>,
}

// --- GAST -------------------------------------------------------------------

/// Gas Turbine Simplified governor (PSS/E GAST) -- Rowen model.
///
/// This is a standard planning-level model for gas turbines when detailed
/// performance test data is unavailable.  For precise transient stability
/// studies, upgrade to GAST2A (with ambient temperature effects) or GGOV1
/// (with detailed PID governor) when gas turbine test data is available.
/// See the module-level documentation for the full model mapping.
///
/// PSS/E params: `R T1 T2 T3 AT KT VMIN VMAX`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GastParams {
    /// Droop (speed regulation, pu).
    pub r: f64,
    /// Governor valve time constant (s).
    pub t1: f64,
    /// Turbine time constant (s).
    pub t2: f64,
    /// Exhaust temperature time constant (s).
    pub t3: f64,
    /// Ambient temperature load limit (pu).
    pub at: f64,
    /// Exhaust temperature coefficient.
    pub kt: f64,
    /// Minimum governor output (pu).
    pub vmin: f64,
    /// Maximum governor output (pu).
    pub vmax: f64,
}

// --- REPCA ------------------------------------------------------------------

/// REPC_A -- IBR plant controller (maps to governor slot, Phase 8 simplified).
///
/// Simplified to constant Pref/Qref -- no AGC droop or plant-level control.
///
/// This is a standard planning-level model for IBR plant controllers.
/// For studies involving plant-level AGC response, frequency droop, or
/// coordinated voltage/reactive control, use the full REPC_A model with droop
/// and PI controller parameters.  See the module-level documentation for details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepcaParams {
    // --- Reactive power / voltage control ---
    /// Voltage / reactive power control flag: 0 = Q control, 1 = voltage control.
    pub vrflag: f64,
    /// Reactive power droop gain (pu).
    pub rc: f64,
    /// Reactive power / voltage measurement filter time constant (s).
    pub tfltr: f64,
    /// Voltage PI proportional gain.
    pub kp: f64,
    /// Voltage PI integral gain (1/s).
    pub ki: f64,
    /// Maximum PI output (pu).
    pub vmax: f64,
    /// Minimum PI output (pu).
    pub vmin: f64,
    /// Plant voltage reference (pu).
    pub vref: f64,
    /// Reactive power reference (pu).
    pub qref: f64,
    /// Plant-level reactive power max (pu).
    pub qmax: f64,
    /// Plant-level reactive power min (pu).
    pub qmin: f64,

    // --- Active power / frequency control ---
    /// Frequency deadband lower (Hz, negative).
    pub fdbd1: f64,
    /// Frequency deadband upper (Hz, positive).
    pub fdbd2: f64,
    /// Frequency droop down gain.
    pub ddn: f64,
    /// Frequency droop up gain.
    pub dup: f64,
    /// Active power measurement filter time constant (s).
    pub tp: f64,
    /// Active power PI proportional gain.
    pub kpg: f64,
    /// Active power PI integral gain (1/s).
    pub kig: f64,
    /// Active power reference (pu).
    pub pref: f64,
    /// Maximum active power (pu).
    pub pmax: f64,
    /// Minimum active power (pu).
    pub pmin: f64,
    /// Active power ramp rate (pu/s).
    pub rrpwr: f64,
    /// Voltage measurement filter time constant (s) for frequency control.
    pub tlag: f64,
}

// --- ST2CUT -----------------------------------------------------------------

/// Dual-input PSS (PSS/E ST2CUT).
///
/// PSS/E params (11 required; LSMIN, VCU, VCL optional):
/// `K1 T1 T2 T3 T4 K2 T5 T6 T7 T8 LSMAX [LSMIN VCU VCL]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct St2cutParams {
    pub k1: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub k2: f64,
    pub t5: f64,
    pub t6: f64,
    pub t7: f64,
    pub t8: f64,
    pub lsmax: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsmin: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcu: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcl: Option<f64>,
}

// ---------------------------------------------------------------------------

// --- GENTPJ -----------------------------------------------------------------

/// GENTPJ — flux-based sixth-order round-rotor generator (WECC standard until Dec 2024).
///
/// Identical state equations to GENROU; saturation applied to all flux linkages
/// simultaneously via Se(ψ) = S1*(ψ/1.0)^2 (quadratic), with additional stator
/// current correction term Kii.
///
/// PSS/E params (14 required + Kii optional):
/// `Td0' Td0'' Tq0' Tq0'' H D Xd Xq Xd' Xq' Xd'' Xl S(1.0) S(1.2) [Kii]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GentpjParams {
    pub td0_prime: f64,
    pub td0_pprime: f64,
    pub tq0_prime: f64,
    pub tq0_pprime: f64,
    pub h: f64,
    pub d: f64,
    pub xd: f64,
    pub xq: f64,
    pub xd_prime: f64,
    pub xq_prime: f64,
    pub xd_pprime: f64,
    pub xl: f64,
    pub s1: f64,
    pub s12: f64,
    /// Stator current coefficient for saturation (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kii: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra: Option<f64>,
}

// --- GENQEC -----------------------------------------------------------------

/// GENQEC — sixth-order round-rotor generator with quadratic saturation.
///
/// **Mandatory WECC standard from December 2024.**
/// Quadratic saturation applied simultaneously to all EMF and impedance terms.
/// Same state variables as GENROU.
///
/// PSS/E params (14 required + Ra optional):
/// `Td0' Td0'' Tq0' Tq0'' H D Xd Xq Xd' Xq' Xd'' Xl S(1.0) S(1.2) [Ra]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenqecParams {
    pub td0_prime: f64,
    pub td0_pprime: f64,
    pub tq0_prime: f64,
    pub tq0_pprime: f64,
    pub h: f64,
    pub d: f64,
    pub xd: f64,
    pub xq: f64,
    pub xd_prime: f64,
    pub xq_prime: f64,
    pub xd_pprime: f64,
    pub xl: f64,
    pub s1: f64,
    pub s12: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra: Option<f64>,
}

// --- ESST1A -----------------------------------------------------------------

/// IEEE Type ST1A static exciter (PSS/E ESST1A) — 2005 enhanced standard.
///
/// PSS/E params: `TR VIMAX VIMIN TC TB TC1 TB1 KA TA VAMAX VAMIN VRMAX VRMIN KC KF TF`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst1aParams {
    pub tr: f64,
    pub vimax: f64,
    pub vimin: f64,
    pub tc: f64,
    pub tb: f64,
    pub tc1: f64,
    pub tb1: f64,
    pub ka: f64,
    pub ta: f64,
    pub vamax: f64,
    pub vamin: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kc: f64,
    pub kf: f64,
    pub tf: f64,
    pub klr: f64,
    pub ilr: f64,
}

// --- EXAC1 / ESAC1A ---------------------------------------------------------

/// IEEE Type AC1A rotating exciter (PSS/E EXAC1 / ESAC1A).
///
/// AC alternator-fed rectifier exciter. The rectifier regulation is modelled
/// via the FEX function (IN = KC * Ifd / Ve).
///
/// PSS/E params: `TR TB TC KA TA VRMAX VRMIN TE KF TF KE E1 SE1 E2 SE2 KC`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exac1Params {
    pub tr: f64,
    pub tb: f64,
    pub tc: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub kc: f64,
    pub kd: f64,
    pub ke: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
}

// --- ESAC7B -----------------------------------------------------------------

/// IEEE Type AC7B rotating exciter (PSS/E ESAC7B / AC7B).
///
/// High-performance AC exciter with PI voltage regulator (Hitachi/ABB digital).
///
/// PSS/E params: `TR KPA KIA VRH VRL KPF VFH TF TE KE E1 SE1 E2 SE2 KD KC KL`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esac7bParams {
    pub tr: f64,
    pub kpa: f64,
    pub kia: f64,
    pub vrh: f64,
    pub vrl: f64,
    pub kpf: f64,
    pub vfh: f64,
    pub tf: f64,
    pub te: f64,
    pub ke: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub kd: f64,
    pub kc: f64,
    pub kl: f64,
}

// --- ESST4B -----------------------------------------------------------------

/// IEEE Type ST4B static exciter (PSS/E ESST4B) — dual-loop PI controller.
///
/// PSS/E params: `TR KPR KIR VRMAX VRMIN KPM KIM VMMAX VMMIN KG KP KI VBMAX VGMAX`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst4bParams {
    pub tr: f64,
    pub kpr: f64,
    pub kir: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kpm: f64,
    pub kim: f64,
    pub vmmax: f64,
    pub vmmin: f64,
    pub kg: f64,
    pub kp: f64,
    pub ki: f64,
    pub vbmax: f64,
    pub vgmax: f64,
}

// --- HYGOV -----------------------------------------------------------------

/// WECC hydro turbine governor (PSS/E HYGOV).
///
/// PID servo + penstock water column + turbine output.
///
/// PSS/E params: `R TP VELM TG GMAX GMIN TW At Dturb QNLL`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HygovParams {
    /// Droop (pu).
    pub r: f64,
    /// Pilot valve time constant (s).
    pub tp: f64,
    /// Gate velocity limit (pu/s).
    pub velm: f64,
    /// Gate servo time constant (s).
    pub tg: f64,
    /// Maximum gate position (pu).
    pub gmax: f64,
    /// Minimum gate position (pu).
    pub gmin: f64,
    /// Water time constant (s).
    pub tw: f64,
    /// Turbine gain (pu power / pu gate).
    pub at: f64,
    /// Turbine damping (pu).
    pub dturb: f64,
    /// No-load flow (pu).
    pub qnl: f64,
}

// --- HYGOVD ----------------------------------------------------------------

/// WECC hydro governor with deadband (PSS/E HYGOVD — WECC recommended).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HygovdParams {
    pub r: f64,
    pub tp: f64,
    pub velm: f64,
    pub tg: f64,
    pub gmax: f64,
    pub gmin: f64,
    pub tw: f64,
    pub at: f64,
    pub dturb: f64,
    pub qnl: f64,
    /// Deadband lower limit (pu, negative).
    pub db1: f64,
    /// Deadband upper limit (pu, positive).
    pub db2: f64,
}

// --- TGOV1D ----------------------------------------------------------------

/// Steam turbine governor with speed deadband (PSS/E TGOV1D — WECC recommended).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tgov1dParams {
    pub r: f64,
    pub t1: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub t2: f64,
    pub t3: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dt: Option<f64>,
    /// Deadband lower limit (pu, negative).
    pub db1: f64,
    /// Deadband upper limit (pu, positive).
    pub db2: f64,
}

// --- IEEEG1D ---------------------------------------------------------------

/// IEEE Type G1 steam governor with deadband (PSS/E IEEEG1D — WECC recommended).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeeg1dParams {
    pub k: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub uo: f64,
    pub uc: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub t4: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t5: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k3: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k4: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t6: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k5: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k6: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t7: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k7: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k8: Option<f64>,
    /// Deadband lower limit (pu, negative).
    pub db1: f64,
    /// Deadband upper limit (pu, positive).
    pub db2: f64,
}

// --- IEEEG2 ----------------------------------------------------------------

/// IEEE Type G2 hydro governor with dashpot, gate servo, and water column.
///
/// Full block diagram: speed error → dashpot (temporary droop) → gate servo
/// → water column (non-minimum phase) → Pm.
///
/// 3 states: x_dashpot, x_gate, x_water.
///
/// PSS/E params: `K T1 T2 T3 Pmin Pmax At Dturb Qnl`
/// Legacy 4-param format: `K T1 T2 PZ` (PZ mapped to Qnl).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeeg2Params {
    /// Governor gain (1/R permanent droop).
    pub k: f64,
    /// Gate servo time constant Tp (s).
    pub t1: f64,
    /// Water starting time Tw (s).
    pub t2: f64,
    /// Dashpot reset time constant Tr (s). 0 = no dashpot.
    #[serde(default)]
    pub t3: f64,
    /// Temporary droop coefficient Rt. 0 = no temporary droop.
    #[serde(default)]
    pub rt: f64,
    /// Minimum gate position (pu).
    #[serde(default)]
    pub pmin: f64,
    /// Maximum gate position (pu).
    #[serde(default = "Ieeeg2Params::default_pmax")]
    pub pmax: f64,
    /// Turbine gain.
    #[serde(default = "Ieeeg2Params::default_at")]
    pub at: f64,
    /// Turbine damping coefficient.
    #[serde(default)]
    pub dturb: f64,
    /// No-load flow (pu). Legacy `pz` field maps here.
    #[serde(default)]
    pub qnl: f64,
}

impl Ieeeg2Params {
    fn default_pmax() -> f64 {
        1.0
    }
    fn default_at() -> f64 {
        1.0
    }
}

// --- PSS2A -----------------------------------------------------------------

/// IEEE dual-input PSS (PSS/E PSS2A) — IEEE 421.5-2005 standard.
///
/// Two inputs: speed (ω) and electrical power (Pe).
///
/// PSS/E params: `M1 T6 T7 KS2 T8 T9 M2 TW1 TW2 TW3 TW4 T1 T2 T3 T4 KS1 KS3 VSTMAX VSTMIN`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss2aParams {
    /// Input 1 signal selector (1=speed, 2=freq, 3=power).
    pub m1: f64,
    /// Input 1 transducer time constant (s).
    pub t6: f64,
    /// Input 2 transducer time constant (s).
    pub t7: f64,
    /// Input 2 gain.
    pub ks2: f64,
    /// Input 2 ramp tracking filter TC (s).
    pub t8: f64,
    /// Input 2 ramp tracking filter TB (s).
    pub t9: f64,
    /// Input 2 signal selector (1=speed, 2=freq, 3=power).
    pub m2: f64,
    /// Input 1 washout TC 1 (s).
    pub tw1: f64,
    /// Input 1 washout TC 2 (s).
    pub tw2: f64,
    /// Input 2 washout TC 1 (s).
    pub tw3: f64,
    /// Input 2 washout TC 2 (s).
    pub tw4: f64,
    /// Lead-lag 1 numerator TC (s).
    pub t1: f64,
    /// Lead-lag 1 denominator TC (s).
    pub t2: f64,
    /// Lead-lag 2 numerator TC (s).
    pub t3: f64,
    /// Lead-lag 2 denominator TC (s).
    pub t4: f64,
    /// PSS gain (pu/pu).
    pub ks1: f64,
    /// Input signal scaling (usually 1.0).
    pub ks3: f64,
    /// Maximum PSS output (pu).
    pub vstmax: f64,
    /// Minimum PSS output (pu).
    pub vstmin: f64,
}

// --- PSS2B -----------------------------------------------------------------

/// IEEE dual-input PSS enhanced (PSS/E PSS2B) — additional transducer TC.
///
/// Identical to PSS2A with additional time constants T10/T11.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss2bParams {
    pub m1: f64,
    pub t6: f64,
    pub t7: f64,
    pub ks2: f64,
    pub t8: f64,
    pub t9: f64,
    pub m2: f64,
    pub tw1: f64,
    pub tw2: f64,
    pub tw3: f64,
    pub tw4: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub ks1: f64,
    pub ks3: f64,
    pub vstmax: f64,
    pub vstmin: f64,
    /// Additional transducer lead TC (s).
    pub t10: f64,
    /// Additional transducer lag TC (s).
    pub t11: f64,
}

// --- STAB1 -----------------------------------------------------------------

/// WSCC simple stabilizer (PSS/E STAB1).
///
/// Single-input (speed), one washout, one lead-lag.
///
/// PSS/E params: `KS T1 T2 T3 T4 HLIM`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stab1Params {
    /// Stabilizer gain (pu/pu).
    pub ks: f64,
    /// Washout time constant (s).
    pub t1: f64,
    /// Lead-lag denominator time constant (s).
    pub t2: f64,
    /// Lead-lag numerator time constant (s).
    pub t3: f64,
    /// Second lead-lag denominator TC (s).
    pub t4: f64,
    /// PSS output limit (pu, symmetric ±HLIM).
    pub hlim: f64,
}

// ---------------------------------------------------------------------------
// Phase 14: BESS, remaining exciters, governors, PSS
// ---------------------------------------------------------------------------

// --- CBEST -----------------------------------------------------------------

/// CBEST — PSS/E native BESS model (Phase 14, 4 states).
///
/// States: p_cmd, q_cmd, soc, e_dc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CbestParams {
    /// Maximum active power output (pu machine base, typ 1.0).
    pub p_max: f64,
    /// Minimum active power (pu, can be negative = charging, typ -1.0).
    pub p_min: f64,
    /// Maximum reactive power (pu, typ 0.5).
    pub q_max: f64,
    /// Minimum reactive power (pu, typ -0.5).
    pub q_min: f64,
    /// Active power time constant (s, typ 0.05).
    pub tp: f64,
    /// Reactive power time constant (s, typ 0.05).
    pub tq: f64,
    /// Energy capacity (MWh normalized to MVA base).
    pub e_cap: f64,
    /// Machine MVA base.
    pub mbase: f64,
    /// Initial state of charge (0..1, default 0.5).
    pub soc_init: f64,
}

// --- CHAAUT ----------------------------------------------------------------

/// CHAAUT — BESS active power controller with frequency droop (Phase 14, 2 states).
///
/// States: p_cmd, freq_state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaautParams {
    /// Frequency droop gain (typ 20.0).
    pub kf: f64,
    /// Frequency filter time constant (s, typ 0.1).
    pub tf: f64,
    /// Maximum power output (pu).
    pub p_max: f64,
    /// Minimum power output (pu, can be negative).
    pub p_min: f64,
    /// Power time constant (s).
    pub tp: f64,
    /// Machine MVA base.
    pub mbase: f64,
}

// --- ESAC2A ----------------------------------------------------------------

/// ESAC2A — IEEE AC2A high-initial-response rotating exciter (Phase 14, 5 states).
///
/// States: vm (transducer), vr (voltage regulator), ve (exciter EMF), vf (rate feedback), efd.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esac2aParams {
    pub tr: f64,
    pub tb: f64,
    pub tc: f64,
    pub ka: f64,
    pub ta: f64,
    pub vamax: f64,
    pub vamin: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kb: f64,
    pub kc: f64,
    pub kd: f64,
    pub kh: f64,
}

// --- ESAC5A ----------------------------------------------------------------

/// ESAC5A — IEEE AC5A simplified brushless exciter (Phase 14, 2 states).
///
/// States: vr (regulator), efd.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esac5aParams {
    pub ka: f64,
    pub ta: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub vrmax: f64,
    pub vrmin: f64,
}

// --- PSS1A -----------------------------------------------------------------

/// PSS1A — Single-input single lead-lag PSS (Phase 14, 2 states).
///
/// States: x1 (washout), x2 (lead-lag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss1aParams {
    /// PSS gain.
    pub ks: f64,
    /// Washout time constant (s).
    pub t1: f64,
    /// Second washout / lead-lag denominator TC (s).
    pub t2: f64,
    /// Lead-lag numerator TC (s).
    pub t3: f64,
    /// Second lead-lag denominator TC (s).
    pub t4: f64,
    /// Maximum PSS output (pu).
    pub vstmax: f64,
    /// Minimum PSS output (pu).
    pub vstmin: f64,
}

// --- PIDGOV ----------------------------------------------------------------

/// PIDGOV — PID governor for any prime mover (Phase 14, 3 states).
///
/// States: x_int (integrator), x_der (derivative filter), pm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidgovParams {
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
    /// Proportional gain.
    pub kp: f64,
    /// Integral gain (1/s).
    pub ki: f64,
    /// Derivative gain.
    pub kd: f64,
    /// Derivative filter time constant (s).
    pub td: f64,
    /// Power output time constant (s).
    pub tf: f64,
}

// --- DEGOV1 ----------------------------------------------------------------

/// DEGOV1 — Woodward diesel/gas engine governor (6 states).
///
/// Full PID electronic controller + actuator + 3-stage engine dynamics.
///
/// States: x_ecl (derivative filter), x_int (integrator), x_act (actuator),
///         x1 (engine lag 1), x2 (engine lag 2), x3 (engine lag 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Degov1Params {
    /// Droop (pu).
    pub r: f64,
    /// First engine time constant (s).
    pub t1: f64,
    /// Second engine time constant (s).
    pub t2: f64,
    /// Third engine time constant (s).
    pub t3: f64,
    /// Derivative action time constant (s) — PID controller.
    #[serde(default = "default_degov1_t4")]
    pub t4: f64,
    /// Derivative filter time constant (s) — PID controller.
    #[serde(default = "default_degov1_t5")]
    pub t5: f64,
    /// Actuator time constant (s).
    #[serde(default = "default_degov1_t6")]
    pub t6: f64,
    /// Controller integral time constant (s).
    pub td: f64,
    /// Controller gain.
    #[serde(default = "default_degov1_k")]
    pub k: f64,
    /// Maximum actuator output (pu).
    pub vmax: f64,
    /// Minimum actuator output (pu).
    pub vmin: f64,
    /// Actuator rate limit (pu/s).
    #[serde(default = "default_degov1_velm")]
    pub velm: f64,
    /// Ambient temperature load limit (pu) — engine output gain.
    pub at: f64,
    /// Exhaust temperature coefficient — engine cross-coupling.
    pub kt: f64,
}

fn default_degov1_t4() -> f64 {
    0.0
}
fn default_degov1_t5() -> f64 {
    0.01
}
fn default_degov1_t6() -> f64 {
    0.01
}
fn default_degov1_k() -> f64 {
    1.0
}
fn default_degov1_velm() -> f64 {
    99.0
}

// ---------------------------------------------------------------------------
// Phase 13: FACTS/HVDC dynamic model records
// ---------------------------------------------------------------------------

/// A FACTS or HVDC dynamic model record — attaches to a bus (shunt) or two buses (branch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FACTSDyn {
    /// Primary bus number (for shunt devices; rectifier bus for HVDC).
    pub bus: u32,
    /// Device ID string (matches PSS/E device ID).
    pub device_id: String,
    /// The specific FACTS/HVDC model and its parameters.
    pub model: FACTSModel,
    /// Second terminal bus: inverter bus for HVDC, to-bus for series FACTS.
    /// Populated from model params (HVDC) or network topology (TCSC/SSSC/UPFC).
    #[serde(default)]
    pub to_bus: Option<u32>,
}

/// Discriminated union of supported FACTS/HVDC dynamic models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FACTSModel {
    /// CSVGN1 — Static VAr Compensator (most common SVC model).
    Csvgn1(Csvgn1Params),
    /// CSTCON — STATCOM (current-source reactive control).
    Cstcon(CstconParams),
    /// TCSC — Thyristor-Controlled Series Capacitor.
    Tcsc(TcscParams),
    /// CDC4T — Generic LCC HVDC Two-Terminal.
    Cdc4t(Cdc4tParams),
    /// VSCDCT — Generic VSC HVDC Two-Terminal.
    Vscdct(VscdctParams),
    /// CSVGN3 — SVC with slope/droop regulator (Phase 15, 3 states).
    Csvgn3(Csvgn3Params),
    /// CDC7T — LCC HVDC + runback + current order controllers (Phase 15, 6 states).
    Cdc7t(Cdc7tParams),
    // Phase 20
    /// CSVGN4 — SVC with 4 states (adds POD) (Phase 20).
    Csvgn4(Csvgn4Params),
    /// CSVGN5 — SVC with 4 states (voltage support mode) (Phase 20).
    Csvgn5(Csvgn5Params),
    /// CDC6T — LCC HVDC with enhanced controls (Phase 20).
    Cdc6t(Cdc6tParams),
    /// CSTCNT — STATCOM with N controls (4 states) (Phase 20).
    Cstcnt(CstcntParams),
    /// MMC1 — Modular Multilevel Converter (5 states) (Phase 20).
    Mmc1(Mmc1Params),
    // Phase 26
    /// HVDCPLU1 — Siemens HVDC Plus VSC (Phase 26, 6 states, reuses Vscdct layout).
    Hvdcplu1(HvdcPlu1Params),
    /// CSVGN6 — SVC Variant 6 with Auxiliary Inputs (Phase 26, 5 states).
    Csvgn6(Csvgn6Params),
    /// STCON1 — STATCOM with Inner Current Control (Phase 26, 4 states).
    Stcon1(Stcon1Params),
    /// GCSC — Gate-Controlled Series Compensator (Phase 26, 3 states).
    Gcsc(GcscParams),
    /// SSSC — Static Synchronous Series Compensator (Phase 26, 4 states).
    Sssc(SsscParams),
    /// UPFC — Unified Power Flow Controller (Phase 26, 6 states).
    Upfc(UpfcParams),
    /// CDC3T — Three-Terminal LCC HVDC (Phase 26, 8 states).
    Cdc3t(Cdc3tParams),
    // Wave 34: WECC SVC/STATCOM variants
    /// SVSMO1 — WECC Generic SVC voltage regulator (1 state).
    Svsmo1(Svsmo1Params),
    /// SVSMO2 — WECC Generic STATCOM (1 state).
    Svsmo2(Svsmo2Params),
    /// SVSMO3 — WECC Advanced SVC (2 states: b_svc + vr).
    Svsmo3(Svsmo3Params),
}

// --- CSVGN1 -----------------------------------------------------------------

/// CSVGN1 — Static VAr Compensator (SVC) with lead-lag voltage regulator.
///
/// PSS/E params: `t1 t2 t3 t4 t5 k vmax vmin bmax bmin /`
///
/// 3 states: `vr` (regulator), `vfilt` (voltage filter), `b_svc` (susceptance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Csvgn1Params {
    /// Regulator time constant (s, typ 0.05).
    pub t1: f64,
    /// Lead time constant (s, typ 0.0).
    pub t2: f64,
    /// Lag time constant (s, typ 0.1).
    pub t3: f64,
    /// Filter time constant (s, typ 0.04).
    pub t4: f64,
    /// Output time constant (s, typ 0.05).
    pub t5: f64,
    /// Regulator gain (typ 100.0).
    pub k: f64,
    /// Max voltage reference (pu, typ 1.05).
    pub vmax: f64,
    /// Min voltage reference (pu, typ 0.95).
    pub vmin: f64,
    /// Max susceptance (pu, capacitive positive, typ 2.0).
    pub bmax: f64,
    /// Min susceptance (pu, inductive negative, typ -2.0).
    pub bmin: f64,
    /// MVA base.
    pub mbase: f64,
    /// TCR reactor susceptance (pu, inductive). If set, enables firing-angle physics.
    /// B_tcr(α) = b_l * (2*(π-α) + sin(2α)) / π
    #[serde(default)]
    pub b_l: Option<f64>,
    /// TSC fixed capacitor susceptance (pu, capacitive positive). Defaults to bmax.
    #[serde(default)]
    pub b_c: Option<f64>,
    /// Firing angle lag time constant (s). Defaults to t5.
    #[serde(default)]
    pub t_alpha: Option<f64>,
}

// --- CSTCON -----------------------------------------------------------------

/// CSTCON — STATCOM (current-source reactive control).
///
/// PSS/E params: `tr k tiq imax imin /`
///
/// 3 states: `vr` (voltage regulator), `vfilt` (filter), `iq_cmd` (current command).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CstconParams {
    /// Regulator time constant (s, typ 0.02).
    pub tr: f64,
    /// Gain (typ 50.0).
    pub k: f64,
    /// Current command time constant (s, typ 0.01).
    pub tiq: f64,
    /// Max current (pu, typ 1.0).
    pub imax: f64,
    /// Min current (pu, typ -1.0).
    pub imin: f64,
    /// MVA base.
    pub mbase: f64,
    /// DC-link capacitance (pu on mbase, typ 0.1). Defaults to 0.1.
    #[serde(default)]
    pub c_dc: Option<f64>,
    /// DC-link voltage reference (pu, typ 1.0). Defaults to 1.0.
    #[serde(default)]
    pub vdc_ref: Option<f64>,
    /// DC-link voltage PI proportional gain. Defaults to 5.0.
    #[serde(default)]
    pub kp_vdc: Option<f64>,
}

// --- TCSC -------------------------------------------------------------------

/// TCSC — Thyristor-Controlled Series Capacitor.
///
/// PSS/E params: `t1 t2 t3 xmax xmin k /`
///
/// 3 states: `x_tcsc` (reactance), `vfilt` (line current filter), `x_order` (order tracking).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcscParams {
    /// Time constant (s, typ 0.05).
    pub t1: f64,
    /// Time constant (s, typ 0.02).
    pub t2: f64,
    /// Time constant (s, typ 0.1).
    pub t3: f64,
    /// Max reactance (pu, capacitive positive).
    pub xmax: f64,
    /// Min reactance (pu, can be negative = inductive).
    pub xmin: f64,
    /// Control gain.
    pub k: f64,
    /// MVA base.
    pub mbase: f64,
}

// --- CDC4T ------------------------------------------------------------------

/// CDC4T — Generic LCC HVDC Two-Terminal.
///
/// PSS/E params: `setvl vschd tr td alpha_min alpha_max gamma_min rectifier_bus inverter_bus /`
///
/// 6 states: `id`, `vd_r`, `vd_i`, `alpha`, `gamma`, `p_ord`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cdc4tParams {
    /// Power or current setpoint (pu).
    pub setvl: f64,
    /// Scheduled DC voltage (pu, typ 1.0).
    pub vschd: f64,
    /// MVA base.
    pub mbase: f64,
    /// Regulator time constant (s, typ 0.02).
    pub tr: f64,
    /// DC time constant (s, typ 0.01).
    pub td: f64,
    /// Min firing angle (deg, typ 5.0).
    pub alpha_min: f64,
    /// Max firing angle (deg, typ 165.0).
    pub alpha_max: f64,
    /// Min extinction angle (deg, typ 15.0).
    pub gamma_min: f64,
    /// Inverter extinction angle reference (rad). Defaults to `gamma_min` in radians.
    pub gamma_ref: Option<f64>,
    /// Power order ramp rate (pu/s). Defaults to 10.0.
    pub ramp: Option<f64>,
    /// Rectifier current controller proportional gain (rad/pu). Defaults to 0.5.
    #[serde(default)]
    pub kp_alpha: Option<f64>,
    /// Rectifier current controller integral gain (rad/pu/s). Defaults to 20.0.
    #[serde(default)]
    pub ki_alpha: Option<f64>,
    /// Inverter gamma controller integral gain (1/s). Defaults to 20.0.
    #[serde(default)]
    pub ki_gamma: Option<f64>,
    /// Rectifier terminal bus number.
    pub rectifier_bus: u32,
    /// Inverter terminal bus number.
    pub inverter_bus: u32,
    // Phase 2.1: CIGRE control + VDCOL
    /// VDCOL breakpoint voltage (pu DC) — below this, current order is reduced.
    #[serde(default)]
    pub vdcol_v1: Option<f64>,
    /// VDCOL breakpoint voltage upper (pu DC).
    #[serde(default)]
    pub vdcol_v2: Option<f64>,
    /// VDCOL current limit at v1 (pu).
    #[serde(default)]
    pub vdcol_i1: Option<f64>,
    /// VDCOL current limit at v2 (pu, typically 1.0 = full current).
    #[serde(default)]
    pub vdcol_i2: Option<f64>,
    /// Current order filter time constant (s).
    #[serde(default)]
    pub t_iord: Option<f64>,
}

// --- VSCDCT -----------------------------------------------------------------

/// VSCDCT — Generic VSC HVDC Two-Terminal.
///
/// PSS/E params: `p_order vdc_ref t_dc t_ac imax rectifier_bus inverter_bus /`
///
/// 6 states: `id`, `iq_r`, `iq_i`, `vd`, `p_ref`, `q_ref`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VscdctParams {
    /// Power order (pu, positive = rectifier to inverter).
    pub p_order: f64,
    /// DC voltage reference (pu, typ 1.0).
    pub vdc_ref: f64,
    /// DC time constant (s, typ 0.02).
    pub t_dc: f64,
    /// AC time constant (s, typ 0.01).
    pub t_ac: f64,
    /// Max DC current (pu, typ 1.1).
    pub imax: f64,
    /// Reactive power order (pu). Defaults to 0.0.
    pub q_order: Option<f64>,
    /// DC voltage PI proportional gain. Defaults to 2.0.
    #[serde(default)]
    pub kp_vdc: Option<f64>,
    /// DC voltage PI integral gain (1/s). Defaults to 50.0.
    #[serde(default)]
    pub ki_vdc: Option<f64>,
    /// AC voltage/Q PI proportional gain. Defaults to 5.0.
    #[serde(default)]
    pub kp_q: Option<f64>,
    /// AC voltage/Q PI integral gain (1/s). Defaults to 20.0.
    #[serde(default)]
    pub ki_q: Option<f64>,
    /// DC voltage measurement filter time constant (s). Defaults to 0.01.
    #[serde(default)]
    pub t_vdc_filt: Option<f64>,
    /// Inner d-axis current PI proportional gain. Defaults to 1.0.
    #[serde(default)]
    pub kp_id: Option<f64>,
    /// Inner d-axis current PI integral gain (1/s). Defaults to 100.0.
    #[serde(default)]
    pub ki_id: Option<f64>,
    /// Inner q-axis current PI proportional gain. Defaults to 1.0.
    #[serde(default)]
    pub kp_iq: Option<f64>,
    /// Inner q-axis current PI integral gain (1/s). Defaults to 100.0.
    #[serde(default)]
    pub ki_iq: Option<f64>,
    /// MVA base.
    pub mbase: f64,
    /// Rectifier terminal bus number.
    pub rectifier_bus: u32,
    /// Inverter terminal bus number.
    pub inverter_bus: u32,
}

// ---------------------------------------------------------------------------
// Phase 12: Load dynamic model records
// ---------------------------------------------------------------------------

/// A load dynamic model record — attaches to a load bus (not a generator bus).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadDyn {
    /// Bus number of the associated static load.
    pub bus: u32,
    /// Load ID (matches PSS/E load record ID, e.g. `"1"`, `"L1"`).
    pub load_id: String,
    /// The specific load model and its parameters.
    pub model: LoadModel,
}

/// Discriminated union of supported load dynamic models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LoadModel {
    /// CLOD — PSS/E composite load (large motor, small motor, discharge lighting, non-conforming).
    Clod(ClodParams),
    /// INDMOT — Generic 3rd-order induction motor aggregate.
    Indmot(IndmotParams),
    /// MOTOR — Single-phase induction motor / AC compressor (2nd-order).
    Motor(MotorParams),
    /// CMPLDW — Composite load model with motors (Phase 16).
    Cmpldw(CmpldwParams),
    /// CMPLDWG — CMPLDW with embedded generation (Phase 16).
    Cmpldwg(CmpldwgParams),
    /// CMLDBLU2 — Composite load simplified blue model (Phase 16).
    Cmldblu2(Cmldblu2Params),
    /// CMLDARU2 — Composite load ARU2 model (Phase 16).
    Cmldaru2(Cmldblu2Params),
    /// MOTORW — Type W induction motor (Phase 16).
    Motorw(MotorwParams),
    /// CIM5 — Current injection motor model 5th order (Phase 16).
    Cim5(Cim5Params),
    // Phase 27
    /// LCFB1 — Load compensator with frequency bias (Phase 27, 2 states).
    Lcfb1(Lcfb1Params),
    /// LDFRAL — Dynamic load frequency regulation (Phase 27, 2 states).
    Ldfral(LdfralParams),
    /// FRQTPLT — Frequency relay trip (Phase 27, 1 state + bool flag).
    Frqtplt(FrqtpltParams),
    /// LVSHBL — Low-voltage shunt block (Phase 27, 1 state + bool flag).
    Lvshbl(LvshblParams),
    // Wave 34: Additional load variants
    /// CIM6 — 6th-order induction motor (extends CIM5 with q-axis transient state).
    Cim6(Cim6Params),
    /// CIMW — Composite Wind Induction Motor (alias to INDMOT dynamics).
    Cimw(IndmotParams),
    /// EXTL — External Load (simplified composite, 2 states).
    Extl(ExtlParams),
    /// IEELAR — IEEE Load Aggregation (alias to EXTL structure).
    Ieelar(ExtlParams),
    /// CMLDOWU2 — CLM Owner variant (alias to CMPLDW).
    Cmldowu2(CmpldwParams),
    /// CMLDXNU2 — CLM Zone variant (alias to CMPLDW).
    Cmldxnu2(CmpldwParams),
    /// CMLDALU2 — CLM All-utilities variant (alias to CMPLDW).
    Cmldalu2(CmpldwParams),
    /// CMLDBLU2W — CLM Blue with wind (alias to CMLDBLU2).
    Cmldblu2w(Cmldblu2Params),
    /// CMLDARU2W — CLM ARU2 with wind (alias to CMLDARU2).
    Cmldaru2w(Cmldblu2Params),
    // Wave 35: Generator protection relays
    /// VTGTPAT — Voltage-Time Generator Protection Trip (continuous).
    Vtgtpat(VtgtpatParams),
    /// VTGDCAT — Voltage-Time Discrete Generator Protection (alias to VTGTPAT dynamics).
    Vtgdcat(VtgtpatParams),
    /// FRQTPAT — Frequency-Time Generator Protection Trip (continuous).
    Frqtpat(FrqtpatParams),
    /// FRQDCAT — Frequency-Time Discrete Generator Protection (alias to FRQTPAT dynamics).
    Frqdcat(FrqtpatParams),
    // Wave 36: protection models
    /// DISTR1 — Distance relay (line protection, Wave 36).
    Distr1(Distr1Params),
    /// BFR50 — Breaker failure relay (ANSI 50BF).
    Bfr50(Bfr50Params),
    /// LVSHC1 — Low voltage shunt capacitor (Wave 36, alias LvshblParams).
    Lvshc1(LvshblParams),
    // Wave 7 (B10): additional protection relay models
    /// 87T — Transformer differential relay.
    TransDiff87(TransDiff87Params),
    /// 87L — Line differential relay (trips branch, not generator).
    LineDiff87l(LineDiff87lParams),
    /// 79 — Automatic recloser (trips + auto-reclose sequence).
    Recloser79(Recloser79Params),
    // Wave 37: CLM DG variants
    /// CMLDDGU2 — CMPLDW with embedded distributed generation.
    Cmlddgu2(CmpldwParams),
    /// CMLDDGGU2 — CMPLDWG with embedded distributed generation.
    Cmlddggu2(CmpldwgParams),
    /// CMLDOWDGU2 — CMLDOWU2 with embedded distributed generation.
    Cmldowdgu2(CmpldwParams),
    /// CMLDXNDGU2 — CMLDXNU2 with embedded distributed generation.
    Cmldxndgu2(CmpldwParams),
    /// UVLS1 — Under-Voltage Load Shedding relay (single stage, per-bus).
    Uvls1(Uvls1Params),
}

// --- CLOD -------------------------------------------------------------------

/// PSS/E composite load model (CLOD).
///
/// Models a mix of load components at a load bus: large motors, small motors,
/// discharge lighting, and non-conforming constant-power loads.
///
/// PSS/E params: `lfac rfrac xfrac lfrac nfrac dsli tv tf vtd vtu ftd ftu td`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClodParams {
    /// MVA base (from load record; used to scale power).
    pub mbase: f64,
    /// Load factor — fraction of total bus load this model represents (0..1].
    pub lfac: f64,
    /// Large motor fraction (of modelled load).
    pub rfrac: f64,
    /// Small motor fraction (of modelled load).
    pub xfrac: f64,
    /// Discharge lighting fraction (of modelled load).
    pub lfrac_dl: f64,
    /// Non-conforming constant power fraction (of modelled load).
    pub nfrac: f64,
    /// Discharge lighting initial state.
    pub dsli: f64,
    /// Voltage transient time constant (s) — motor slip recovery (typ 0.025 s).
    pub tv: f64,
    /// Frequency transient time constant (s) — lighting recovery (typ 0.02 s).
    pub tf: f64,
    /// Low voltage trip threshold (pu, typ 0.75).
    pub vtd: f64,
    /// High voltage trip threshold (pu, typ 1.2).
    pub vtu: f64,
    /// Low frequency trip threshold (Hz, typ 57.5).
    pub ftd: f64,
    /// High frequency trip threshold (Hz, typ 61.5).
    pub ftu: f64,
    /// Trip delay (s, typ 0.05).
    pub td: f64,
}

// --- INDMOT -----------------------------------------------------------------

/// Generic 3rd-order induction motor aggregate (PSS/E INDMOT).
///
/// Single-cage rotor with slip dynamics — represents a group of similar
/// induction motors aggregated to a single equivalent.
///
/// PSS/E params: `h d ra xs xr xm rr mbase lfac`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndmotParams {
    /// Inertia constant (s).
    pub h: f64,
    /// Damping coefficient.
    pub d: f64,
    /// Stator resistance (pu machine base).
    pub ra: f64,
    /// Stator leakage reactance (pu machine base).
    pub xs: f64,
    /// Rotor leakage reactance (pu machine base).
    pub xr: f64,
    /// Magnetizing reactance (pu machine base).
    pub xm: f64,
    /// Rotor resistance (pu machine base).
    pub rr: f64,
    /// Transient time constant: `t0p = (xr + xm) / (omega0 * rr)` (s).
    pub t0p: f64,
    /// Transient reactance: `x0p = xs + xr*xm/(xr+xm)` (pu machine base).
    pub x0p: f64,
    /// MVA base of the motor aggregate.
    pub mbase: f64,
    /// Load fraction at this bus (0..1].
    pub lfac: f64,
    /// Initial (rated) slip — computed during init from power flow solution.
    pub slip0: f64,
    /// Initial electrical torque — computed during init for torque normalization.
    pub te0: f64,
}

// --- MOTOR ------------------------------------------------------------------

/// Single-phase induction motor / AC compressor — 2nd-order (PSS/E MOTOR).
///
/// Simplified model capturing the dominant dynamics of residential AC
/// compressors and single-phase induction motors.
///
/// PSS/E params: `h ra xs x0p t0p mbase lfac`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotorParams {
    /// Inertia constant (s).
    pub h: f64,
    /// Stator resistance (pu machine base).
    pub ra: f64,
    /// Stator+rotor synchronous reactance (pu machine base).
    pub xs: f64,
    /// Transient reactance (pu machine base).
    pub x0p: f64,
    /// Transient time constant (s).
    pub t0p: f64,
    /// MVA base.
    pub mbase: f64,
    /// Load fraction at this bus (0..1].
    pub lfac: f64,
}

// Unknown / fallback
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Phase 15: Exciter param structs
// ---------------------------------------------------------------------------

/// ESST5B — IEEE ST5B static exciter (Phase 15, 3 states: vr, vfilt, efd).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst5bParams {
    pub tr: f64,
    pub kc: f64,
    pub kf: f64,
    pub tf: f64,
    pub ka: f64,
    pub tb: f64,
    pub tc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub t1: f64,
    pub t2: f64,
}

/// EXAC4 / AC4A — IEEE AC4A controlled-rectifier exciter (Phase 15, 2 states: vr, efd).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exac4Params {
    pub tr: f64,
    pub tc: f64,
    pub tb: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kc: f64,
}

// ---------------------------------------------------------------------------
// Phase 15: Governor param structs
// ---------------------------------------------------------------------------

/// TGOV5 — Multi-reheat steam governor HP+IP (Phase 15, 4 states).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tgov5Params {
    /// Droop (speed regulation, pu).
    pub r: f64,
    /// Governor valve time constant (s).
    pub t1: f64,
    /// HP stage time constant (s).
    pub t2: f64,
    /// IP+LP stage time constant (s).
    pub t3: f64,
    /// Output time constant (s).
    pub t4: f64,
    /// HP power fraction.
    pub k1: f64,
    /// IP power fraction.
    pub k2: f64,
    /// LP power fraction.
    pub k3: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
}

/// GAST2A — Advanced Rowen gas turbine governor (Phase 15, 4 states).
///
/// Extension of GAST with ambient temperature (radiation loss) state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gast2aParams {
    /// Droop (speed regulation, pu).
    pub r: f64,
    /// Governor valve time constant (s).
    pub t1: f64,
    /// Turbine output time constant (s).
    pub t2: f64,
    /// Exhaust temperature time constant (s).
    pub t3: f64,
    /// Ambient temperature time constant (s).
    pub t4: f64,
    /// Ambient temperature load limit (pu).
    pub at: f64,
    /// Exhaust temperature coefficient.
    pub kt: f64,
    /// Minimum governor output (pu).
    pub vmin: f64,
    /// Maximum governor output (pu).
    pub vmax: f64,
}

// ---------------------------------------------------------------------------
// Phase 15: PSS param structs
// ---------------------------------------------------------------------------

/// STAB2A — WSCC stabilizer variant A (Phase 15, 3 states: x1, x2, x3).
///
/// Double lead-lag washout stabilizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stab2aParams {
    /// Stabilizer gain.
    pub ks: f64,
    /// Washout time constant (s).
    pub t1: f64,
    /// Lead-lag 1 numerator (s).
    pub t2: f64,
    /// Lead-lag 1 denominator (s).
    pub t3: f64,
    /// Lead-lag 2 numerator (s).
    pub t4: f64,
    /// Lead-lag 2 denominator (s).
    pub t5: f64,
    /// Output limit (pu, symmetric ±HLIM).
    pub hlim: f64,
}

/// PSS4B — Four-band multi-frequency PSS (Phase 15, 4 states).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss4bParams {
    /// Low-band gain.
    pub kl: f64,
    /// High-band gain.
    pub kh: f64,
    /// Low-band washout time constant (s).
    pub tw1: f64,
    /// High-band washout time constant (s).
    pub tw2: f64,
    /// Lead-lag 1 numerator (s).
    pub t1: f64,
    /// Lead-lag 1 denominator (s).
    pub t2: f64,
    /// Lead-lag 2 numerator (s).
    pub t3: f64,
    /// Lead-lag 2 denominator (s).
    pub t4: f64,
    /// Maximum PSS output (pu).
    pub vstmax: f64,
    /// Minimum PSS output (pu).
    pub vstmin: f64,
}

// ---------------------------------------------------------------------------
// Phase 15: FACTS param structs
// ---------------------------------------------------------------------------

/// CSVGN3 — SVC with slope/droop regulator (Phase 15, 3 states, same as CSVGN1 + slope).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Csvgn3Params {
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub k: f64,
    /// Slope (droop) coefficient: Vref = Vref_base + slope*b_svc.
    pub slope: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub bmax: f64,
    pub bmin: f64,
    pub mbase: f64,
}

/// CDC7T — LCC HVDC + runback + current order controllers (Phase 15, 6 states).
///
/// Extends CDC4T with runback rate and current order max.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cdc7tParams {
    pub setvl: f64,
    pub vschd: f64,
    pub mbase: f64,
    pub tr: f64,
    pub td: f64,
    pub alpha_min: f64,
    pub alpha_max: f64,
    pub gamma_min: f64,
    pub rectifier_bus: u32,
    pub inverter_bus: u32,
    /// Runback rate (pu/s).
    pub runback_rate: f64,
    /// Maximum current order (pu).
    pub current_order_max: f64,
}

// ---------------------------------------------------------------------------
// Phase 16: Composite Load param structs
// ---------------------------------------------------------------------------

/// Per-motor circuit parameters for WECC CMPLDW composite load model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmpldwMotorParams {
    /// Stator resistance (pu).
    pub ra: f64,
    /// Magnetizing reactance (pu).
    pub xm: f64,
    /// Rotor resistance (pu).
    pub r1: f64,
    /// Rotor reactance (pu).
    pub x1: f64,
    /// Second cage rotor resistance (pu, 0 = single-cage).
    pub r2: f64,
    /// Second cage rotor reactance (pu, 0 = single-cage).
    pub x2: f64,
    /// Motor inertia constant H (s). Default 0.5.
    pub h: f64,
    /// Voltage trip threshold (pu). Motor trips below this voltage.
    pub vtr: f64,
    /// Motor load torque exponent (0=constant torque, 2=fan/pump).
    pub etrq: f64,
}

impl Default for CmpldwMotorParams {
    fn default() -> Self {
        Self {
            ra: 0.0,
            xm: 3.0,
            r1: 0.04,
            x1: 0.1,
            r2: 0.04,
            x2: 0.1,
            h: 0.5,
            vtr: 0.0,
            etrq: 0.0,
        }
    }
}

/// CMPLDW — Full WECC composite load model with 3 motors (Phase 4.1, 10 states).
///
/// The model represents a composite load bus with three induction motor types
/// (A=large industrial, B=small commercial, C=A/C compressor) plus static ZIP
/// and electronic load fractions. Each motor has independent slip/flux dynamics.
///
/// # States (10 total)
/// - Motor A: slip_a, ed_a, eq_a (3)
/// - Motor B: slip_b, ed_b, eq_b (3)
/// - Motor C: slip_c, ed_c, eq_c (3)
/// - Voltage filter: vfilt (1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmpldwParams {
    /// Load fraction: Motor A (large 3-phase industrial).
    pub lfma: f64,
    /// Load fraction: Motor B (small 3-phase commercial).
    pub lfmb: f64,
    /// Load fraction: Motor C (1-phase A/C compressor).
    pub lfmc: f64,
    /// Static load P coefficient 1.
    pub kp1: f64,
    /// Static load P exponent 1.
    pub np1: f64,
    /// Static load P coefficient 2.
    pub kp2: f64,
    /// Static load P exponent 2.
    pub np2: f64,
    /// Static load Q coefficient 1.
    pub kq1: f64,
    /// Static load Q exponent 1.
    pub nq1: f64,
    /// Static load Q coefficient 2.
    pub kq2: f64,
    /// Static load Q exponent 2.
    pub nq2: f64,
    // Legacy single-motor fields (kept for backward compat with existing DYR files).
    // Used as Motor A defaults when per-motor params are None.
    pub ra: f64,
    pub xm: f64,
    pub r1: f64,
    pub x1: f64,
    pub r2: f64,
    pub x2: f64,
    pub vtr1: f64,
    pub vtr2: f64,
    pub mbase: f64,
    /// Per-motor parameters for Motor A (overrides ra/xm/r1/x1 if present).
    #[serde(default)]
    pub motor_a: Option<CmpldwMotorParams>,
    /// Per-motor parameters for Motor B.
    #[serde(default)]
    pub motor_b: Option<CmpldwMotorParams>,
    /// Per-motor parameters for Motor C.
    #[serde(default)]
    pub motor_c: Option<CmpldwMotorParams>,
    /// Voltage filter time constant (s). Default 0.02.
    #[serde(default = "default_tv")]
    pub tv: f64,
    /// Electronic load fraction. Default 0.0.
    #[serde(default)]
    pub fel: f64,
    /// Frequency sensitivity of P (pu/Hz). Default 0.0.
    #[serde(default)]
    pub pfreq: f64,
}

fn default_tv() -> f64 {
    0.02
}

impl CmpldwParams {
    /// Get Motor A parameters (uses per-motor if set, else legacy single-motor fields).
    pub fn motor_a_params(&self) -> CmpldwMotorParams {
        self.motor_a.clone().unwrap_or(CmpldwMotorParams {
            ra: self.ra,
            xm: self.xm,
            r1: self.r1,
            x1: self.x1,
            r2: self.r2,
            x2: self.x2,
            h: 0.5,
            vtr: self.vtr1,
            etrq: 0.0,
        })
    }

    /// Get Motor B parameters (uses per-motor if set, else default small motor).
    pub fn motor_b_params(&self) -> CmpldwMotorParams {
        self.motor_b.clone().unwrap_or(CmpldwMotorParams {
            ra: self.ra,
            xm: self.xm * 1.5, // higher Xm for small motor
            r1: self.r1 * 1.2,
            x1: self.x1 * 1.2,
            r2: self.r2 * 1.2,
            x2: self.x2 * 1.2,
            h: 0.3, // lower inertia
            vtr: self.vtr2,
            etrq: 0.0,
        })
    }

    /// Get Motor C parameters (uses per-motor if set, else default A/C compressor).
    pub fn motor_c_params(&self) -> CmpldwMotorParams {
        self.motor_c.clone().unwrap_or(CmpldwMotorParams {
            ra: self.ra * 0.8,
            xm: self.xm * 2.0, // higher Xm for single-phase
            r1: self.r1 * 0.8,
            x1: self.x1 * 0.8,
            r2: self.r2 * 0.8,
            x2: self.x2 * 0.8,
            h: 0.1, // very low inertia (compressor)
            vtr: self.vtr2,
            etrq: 2.0, // fan/pump torque
        })
    }
}

/// CMPLDWG — CMPLDW with embedded generation (Phase 16, 10 states).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmpldwgParams {
    pub lfma: f64,
    pub lfmb: f64,
    pub lfmc: f64,
    pub kp1: f64,
    pub np1: f64,
    pub kp2: f64,
    pub np2: f64,
    pub kq1: f64,
    pub nq1: f64,
    pub kq2: f64,
    pub nq2: f64,
    pub ra: f64,
    pub xm: f64,
    pub r1: f64,
    pub x1: f64,
    pub r2: f64,
    pub x2: f64,
    pub vtr1: f64,
    pub vtr2: f64,
    pub mbase: f64,
    pub gen_mw: f64,
    /// Per-motor parameters for Motor A.
    #[serde(default)]
    pub motor_a: Option<CmpldwMotorParams>,
    /// Per-motor parameters for Motor B.
    #[serde(default)]
    pub motor_b: Option<CmpldwMotorParams>,
    /// Per-motor parameters for Motor C.
    #[serde(default)]
    pub motor_c: Option<CmpldwMotorParams>,
    /// Voltage filter time constant (s). Default 0.02.
    #[serde(default = "default_tv")]
    pub tv: f64,
    /// Electronic load fraction. Default 0.0.
    #[serde(default)]
    pub fel: f64,
    /// Frequency sensitivity of P (pu/Hz). Default 0.0.
    #[serde(default)]
    pub pfreq: f64,
}

impl CmpldwgParams {
    /// Get Motor A parameters.
    pub fn motor_a_params(&self) -> CmpldwMotorParams {
        self.motor_a.clone().unwrap_or(CmpldwMotorParams {
            ra: self.ra,
            xm: self.xm,
            r1: self.r1,
            x1: self.x1,
            r2: self.r2,
            x2: self.x2,
            h: 0.5,
            vtr: self.vtr1,
            etrq: 0.0,
        })
    }
    /// Get Motor B parameters.
    pub fn motor_b_params(&self) -> CmpldwMotorParams {
        self.motor_b.clone().unwrap_or(CmpldwMotorParams {
            ra: self.ra,
            xm: self.xm * 1.5,
            r1: self.r1 * 1.2,
            x1: self.x1 * 1.2,
            r2: self.r2 * 1.2,
            x2: self.x2 * 1.2,
            h: 0.3,
            vtr: self.vtr2,
            etrq: 0.0,
        })
    }
    /// Get Motor C parameters.
    pub fn motor_c_params(&self) -> CmpldwMotorParams {
        self.motor_c.clone().unwrap_or(CmpldwMotorParams {
            ra: self.ra * 0.8,
            xm: self.xm * 2.0,
            r1: self.r1 * 0.8,
            x1: self.x1 * 0.8,
            r2: self.r2 * 0.8,
            x2: self.x2 * 0.8,
            h: 0.1,
            vtr: self.vtr2,
            etrq: 2.0,
        })
    }
}

/// CMLDBLU2 — Composite load simplified blue model (Phase 16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cmldblu2Params {
    pub t1: f64,
    pub t2: f64,
    pub k1: f64,
    pub k2: f64,
    pub pf: f64,
    pub kp: f64,
    pub kq: f64,
    pub vmin: f64,
    pub vmax: f64,
    pub mbase: f64,
}

/// CMLDARU2 — Composite load ARU2 model (Phase 16, same params as CMLDBLU2).
pub type Cmldaru2Params = Cmldblu2Params;

/// MOTORW — Type W induction motor (Phase 16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotorwParams {
    pub ra: f64,
    pub xm: f64,
    pub r1: f64,
    pub x1: f64,
    pub r2: f64,
    pub x2: f64,
    pub h: f64,
    pub vtr1: f64,
    pub vtr2: f64,
    pub mbase: f64,
}

/// CIM5 — Current injection motor model 5th order (Phase 16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cim5Params {
    pub ra: f64,
    pub xs: f64,
    pub xm: f64,
    pub xr1: f64,
    pub xr2: f64,
    pub rr1: f64,
    pub rr2: f64,
    pub h: f64,
    pub e1: f64,
    pub s1: f64,
    pub e2: f64,
    pub s2: f64,
    pub mbase: f64,
}

// ---------------------------------------------------------------------------
// Phase 17: Exciter param structs
// ---------------------------------------------------------------------------

/// ESST6B — IEEE ST6B Static Exciter (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst6bParams {
    pub tr: f64,
    pub ilr: f64,
    pub klr: f64,
    pub ka: f64,
    pub ta: f64,
    pub kc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kff: f64,
    pub kgff: f64,
    pub t1: f64,
    pub t2: f64,
}

/// ESST7B — IEEE ST7B Static Exciter (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst7bParams {
    pub tr: f64,
    pub kpa: f64,
    pub kia: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kpff: f64,
    pub kh: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub kl: f64,
}

/// ESAC6A — AC6A Rotating Exciter (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esac6aParams {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub tk: f64,
    pub tb: f64,
    pub tc: f64,
    pub vamax: f64,
    pub vamin: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub te: f64,
    pub kh: f64,
    pub kf: f64,
    pub tf: f64,
    pub kc: f64,
    pub kd: f64,
    pub ke: f64,
}

/// ESDC1A — DC1A Rotating Exciter (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esdc1aParams {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub kf: f64,
    pub tf: f64,
    pub ke: f64,
    pub te: f64,
    pub se1: f64,
    pub e1: f64,
    pub se2: f64,
    pub e2: f64,
    pub vrmax: f64,
    pub vrmin: f64,
}

/// EXST2 — Static Exciter Type ST2 (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exst2Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kc: f64,
    pub ki: f64,
    pub ke: f64,
    pub te: f64,
}

/// AC8B — IEEE AC8B High Initial Response Exciter (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ac8bParams {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub kc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kd: f64,
    pub ke: f64,
    pub te: f64,
    pub pid_kp: f64,
    pub pid_ki: f64,
    pub pid_kd: f64,
}

/// BBSEX1 — Bus-Branch Static Exciter 1 (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bbsex1Params {
    pub t1r: f64,
    pub t2r: f64,
    pub t3r: f64,
    pub t4r: f64,
    pub t1i: f64,
    pub t2i: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
}

/// IEEET3 — IEEE Type 3 Rotating Exciter (Phase 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeet3Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kf: f64,
    pub tf: f64,
    pub ke: f64,
    pub te: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub kp: f64,
    pub ki: f64,
    pub kc: f64,
}

// ---------------------------------------------------------------------------
// Phase 18: Governor + PSS param structs
// ---------------------------------------------------------------------------

/// H6E — Hydro Governor 6 Elements (Phase 18).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H6eParams {
    pub r: f64,
    pub tr: f64,
    pub tf: f64,
    pub tg: f64,
    pub tw: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub dt: f64,
    pub pmax: f64,
    pub pmin: f64,
}

/// WSHYGP — Wind-Synchronous Hydro Governor+Pitch (Phase 18).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WshygpParams {
    pub r: f64,
    pub tf: f64,
    pub tg: f64,
    pub tw: f64,
    pub kd: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub kp: f64,
    pub ki: f64,
}

/// STAB3 — Three-Band PSS (Phase 18).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stab3Params {
    pub ks: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub t6: f64,
    pub vstmax: f64,
    pub vstmin: f64,
}

/// PSS3B — Three-Input Power System Stabilizer (Phase 18).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss3bParams {
    pub a1: f64,
    pub a2: f64,
    pub a3: f64,
    pub a4: f64,
    pub a5: f64,
    pub a6: f64,
    pub a7: f64,
    pub a8: f64,
    pub vsi1max: f64,
    pub vsi1min: f64,
    pub vsi2max: f64,
    pub vsi2min: f64,
    pub vstmax: f64,
    pub vstmin: f64,
}

// ---------------------------------------------------------------------------
// Phase 19: IBR Wind Controller param structs (go in exciter slot)
// ---------------------------------------------------------------------------

/// WT3E1 — Type 3 Wind Electrical Controller (Phase 19).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt3e1Params {
    pub kpv: f64,
    pub kiv: f64,
    pub kqv: f64,
    pub xd: f64,
    pub kpq: f64,
    pub kiq: f64,
    pub tpe: f64,
    pub pmin: f64,
    pub pmax: f64,
    pub qmin: f64,
    pub qmax: f64,
    pub imax: f64,
    /// Voltage measurement filter time constant (s, default 0.05).
    pub tv: f64,
}

/// WT3E2 — Type 3 Wind Electrical Controller Variant 2 (Phase 19).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt3e2Params {
    pub kpv: f64,
    pub kiv: f64,
    pub kqv: f64,
    pub xd: f64,
    pub kpq: f64,
    pub kiq: f64,
    pub tpe: f64,
    pub pmin: f64,
    pub pmax: f64,
    pub qmin: f64,
    pub qmax: f64,
    pub imax: f64,
    pub tiq: f64,
    /// Voltage measurement filter time constant (s, default 0.05).
    pub tv: f64,
}

/// WT4E1 — Type 4 Wind Electrical Controller (Phase 19).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt4e1Params {
    pub kpv: f64,
    pub kiv: f64,
    pub tpe: f64,
    pub pmin: f64,
    pub pmax: f64,
    pub qmin: f64,
    pub qmax: f64,
    pub imax: f64,
}

/// WT4E2 — Type 4 Wind Electrical Controller Variant 2 (Phase 19, same as WT4E1).
pub type Wt4e2Params = Wt4e1Params;

/// REPCB — REPCA Variant B (Phase 19).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepcbParams {
    pub tp: f64,
    pub tfltr: f64,
    pub kp: f64,
    pub ki: f64,
    pub tft: f64,
    pub tfv: f64,
    pub qmax: f64,
    pub qmin: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub kc: f64,
    pub refs: f64,
}

// ---------------------------------------------------------------------------
// Phase 20: FACTS param structs
// ---------------------------------------------------------------------------

/// CSVGN4 — SVC with 4 states (adds POD) (Phase 20).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Csvgn4Params {
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub k: f64,
    pub slope: f64,
    pub kpod: f64,
    pub tpod: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub bmax: f64,
    pub bmin: f64,
    pub mbase: f64,
}

/// CSVGN5 — SVC with 4 states (voltage support mode) (Phase 20).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Csvgn5Params {
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub k: f64,
    pub kv: f64,
    pub kpod: f64,
    pub tpod: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub bmax: f64,
    pub bmin: f64,
    pub mbase: f64,
}

/// CDC6T — LCC HVDC with enhanced controls (Phase 20).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cdc6tParams {
    pub setvl: f64,
    pub vschd: f64,
    pub mbase: f64,
    pub tr: f64,
    pub td: f64,
    pub alpha_min: f64,
    pub alpha_max: f64,
    pub gamma_min: f64,
    pub rectifier_bus: u32,
    pub inverter_bus: u32,
    pub i_limit: f64,
}

/// CSTCNT — STATCOM with N controls (4 states) (Phase 20).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CstcntParams {
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub ka: f64,
    pub ta: f64,
    pub iqmax: f64,
    pub iqmin: f64,
    pub mbase: f64,
}

/// MMC1 — Modular Multilevel Converter (5 states) (Phase 20).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mmc1Params {
    pub tr: f64,
    pub kp_v: f64,
    pub ki_v: f64,
    pub kp_i: f64,
    pub ki_i: f64,
    pub vdc: f64,
    pub larm: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub qmax: f64,
    pub qmin: f64,
    pub mbase: f64,
}

// ---------------------------------------------------------------------------
// Phase 21: EXST3 + BESS param structs
// ---------------------------------------------------------------------------

/// EXST3 — Static Exciter Type ST3 (Phase 21).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exst3Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub tb: f64,
    pub tc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub kc: f64,
    pub ki: f64,
    pub km: f64,
    pub vmmax: f64,
    pub vmmin: f64,
    pub xm: f64,
}

/// CBUFR — Buffer-Frequency-Regulated BESS (Phase 21).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CbufrParams {
    pub kf: f64,
    pub tf: f64,
    pub tp: f64,
    pub p_base: f64,
    pub p_min: f64,
    pub p_max: f64,
    pub e_cap: f64,
    /// Initial state of charge (0..1, default 0.5).
    pub soc_init: f64,
}

/// CBUFD — Buffer-Frequency-Dependent BESS (Phase 21).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CbufdParams {
    pub kf: f64,
    pub tf: f64,
    pub tp: f64,
    pub tq: f64,
    pub p_base: f64,
    pub p_min: f64,
    pub p_max: f64,
    pub q_base: f64,
    pub q_min: f64,
    pub q_max: f64,
    pub e_cap: f64,
    /// Initial state of charge (0..1, default 0.5).
    pub soc_init: f64,
}

/// REGFM_C1 — Grid-forming inverter C1 (Phase 21).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegfmC1Params {
    pub kd: f64,
    pub ki: f64,
    pub kq: f64,
    pub tg: f64,
    pub ddn: f64,
    pub dup: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub qmax: f64,
    pub qmin: f64,
    pub mbase: f64,
}

// ---------------------------------------------------------------------------
// Phase 22: Solar PV Models
// ---------------------------------------------------------------------------

/// PVGU1 — WECC 1st-gen photovoltaic converter unit.
/// Norton current injection, reuses RegcaState (4 fields: ip_cmd, iq_cmd, v_filt, x_eq/pm0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pvgu1Params {
    pub lvplsw: f64,
    pub rrpwr: f64,
    pub brkpt: f64,
    pub zerox: f64,
    pub lvpl1: f64,
    pub volim: f64,
    pub lvpnt1: f64,
    pub lvpnt0: f64,
    pub iolim: f64,
    pub tfltr: f64,
    pub khv: f64,
    pub iqrmax: f64,
    pub iqrmin: f64,
    pub accel: f64,
    pub vsmax: f64,
    pub mbase: f64,
}

/// PVEU1 — WECC 1st-gen PV electrical control unit (maps to exciter slot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pveu1Params {
    pub tiq: f64,
    pub dflag: f64,
    pub vref0: f64,
    pub tv: f64,
    pub dbd: f64,
    pub kqv: f64,
    pub iqhl: f64,
    pub iqll: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub qmax: f64,
    pub qmin: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub tpord: f64,
    pub mbase: f64,
}

/// PVDG — Distributed/rooftop PV aggregate model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PvdgParams {
    pub tp: f64,
    pub tq: f64,
    pub vtrip1: f64,
    pub vtrip2: f64,
    pub vtrip3: f64,
    pub ftrip1: f64,
    pub ftrip2: f64,
    pub pmax: f64,
    pub qmax: f64,
    pub qmin: f64,
    pub mbase: f64,
}

// ---------------------------------------------------------------------------
// Phase 23: Exciter param structs
// ---------------------------------------------------------------------------

/// IEEET2 — IEEE Type 2 rotating-machine exciter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeet2Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ke: f64,
    pub te: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub kf: f64,
    pub tf: f64,
}

/// EXAC2 — IEEE AC2A high initial response rotating exciter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exac2Params {
    pub tr: f64,
    pub tb: f64,
    pub tc: f64,
    pub ka: f64,
    pub ta: f64,
    pub vamax: f64,
    pub vamin: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub ke: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub kc: f64,
    pub kd: f64,
    pub kh: f64,
}

/// EXAC3 — IEEE AC3A controlled-rectifier exciter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exac3Params {
    pub tr: f64,
    pub kc: f64,
    pub ki: f64,
    pub vmin: f64,
    pub vmax: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub ka: f64,
    pub ta: f64,
    pub efdn: f64,
}

/// ESAC3A — IEEE 421.5-2005 AC3A exciter update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esac3aParams {
    pub tr: f64,
    pub tb: f64,
    pub tc: f64,
    pub ka: f64,
    pub ta: f64,
    pub vamax: f64,
    pub vamin: f64,
    pub te: f64,
    pub ke: f64,
    pub kf1: f64,
    pub tf: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub kc: f64,
    pub kd: f64,
    pub ki: f64,
    pub efdn: f64,
    pub kn: f64,
    pub vfemax: f64,
}

/// ESST8C — IEEE 421.5-2016 ST8C static exciter (PID voltage regulator).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst8cParams {
    pub tr: f64,
    pub kpr: f64,
    pub kir: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ka: f64,
    pub ta: f64,
    pub kc: f64,
    pub vbmax: f64,
    pub xl: f64,
    pub kf: f64,
    pub tf: f64,
}

/// ESST9B — IEEE 421.5-2016 ST9B static exciter (simplified ST8C variant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst9bParams {
    pub tr: f64,
    pub kpa: f64,
    pub kia: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ka: f64,
    pub ta: f64,
    pub vbmax: f64,
    pub kc: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
}

/// ESST10C — IEEE 421.5-2016 ST10C static exciter (multi-stage PI with UEL/OEL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst10cParams {
    pub tr: f64,
    pub kpa: f64,
    pub kia: f64,
    pub kpb: f64,
    pub kib: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ka: f64,
    pub ta: f64,
    pub vbmax: f64,
    pub kc: f64,
    pub t1: f64,
    pub t2: f64,
}

/// ESDC3A — IEEE 421.5-2005 DC3A rotating-machine exciter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esdc3aParams {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub te: f64,
    pub ke: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
    pub kp: f64,
    pub ki: f64,
    pub kf: f64,
    pub tf: f64,
}

// ---------------------------------------------------------------------------
// Wave 32: EXDC1 + ESST2A param structs
// ---------------------------------------------------------------------------

/// EXDC1 — IEEE Type DC1A rotating-machine exciter (legacy 13-param form).
///
/// PSS/E format: TR KA TA VRMAX VRMIN KE TE KF TF E1 SE1 E2 SE2
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exdc1Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
}

/// ESST2A — IEEE 421.5-2016 Type ST2A static exciter.
///
/// PSS/E format: TR KA TA TB TC KE TE KF TF VRMAX VRMIN EFD1 SE1 EFD2 SE2 KC KP KI \[TP\]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esst2aParams {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub tb: f64,
    pub tc: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub e1: f64,  // EFD1 — first saturation breakpoint (pu)
    pub se1: f64, // SE(EFD1)
    pub e2: f64,  // EFD2 — second saturation breakpoint (pu)
    pub se2: f64, // SE(EFD2)
    pub kc: f64,  // rectifier loading factor
    pub kp: f64,  // potential circuit gain (terminal voltage)
    pub ki: f64,  // current circuit gain
    pub tp: f64,  // potential-circuit transducer time constant (s)
}

// Wave 33: EXDC3 param struct
// ---------------------------------------------------------------------------

/// EXDC3 — PSS/E non-continuously-acting (relay-type) DC exciter.
///
/// PSS/E format: TR KV TSTALL TCON TB TC VRMAX VRMIN VEFF TLIM VLIM KE TE
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exdc3Params {
    pub tr: f64,
    pub kv: f64,     // regulator deadband threshold (pu)
    pub tstall: f64, // stalling/feedback time constant (s)
    pub tcon: f64,   // relay control time constant (s)
    pub tb: f64,
    pub tc: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub veff: f64,
    pub tlim: f64,
    pub vlim: f64,
    pub ke: f64,
    pub te: f64,
}

// ---------------------------------------------------------------------------
// Phase 24: PSS variant param structs
// ---------------------------------------------------------------------------

/// PSS2C — PSS2B with ramp-tracking filter on input 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss2cParams {
    pub m1: f64,
    pub t6: f64,
    pub m2: f64,
    pub t7: f64,
    pub tw1: f64,
    pub tw2: f64,
    pub tw3: f64,
    pub tw4: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t8: f64,
    pub t9: f64,
    pub n: i32,
    pub ks1: f64,
    pub ks2: f64,
    pub ks3: f64,
    pub vstmax: f64,
    pub vstmin: f64,
}

/// PSS5 — Five-band multi-frequency power system stabilizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss5Params {
    pub kl: f64,
    pub km: f64,
    pub kh: f64,
    pub tw1: f64,
    pub tw2: f64,
    pub tw3: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub t6: f64,
    pub vstmax: f64,
    pub vstmin: f64,
}

/// PSS6C — Six-input multi-band PSS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss6cParams {
    pub kl: f64,
    pub km: f64,
    pub kh: f64,
    pub kl2: f64,
    pub km2: f64,
    pub kh2: f64,
    pub tw1: f64,
    pub tw2: f64,
    pub tw3: f64,
    pub tw4: f64,
    pub tw5: f64,
    pub tw6: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub vstmax: f64,
    pub vstmin: f64,
}

/// PSSSB — WSCC/BPA simple power system stabilizer (vendor variant B).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PsssbParams {
    pub ks: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub t6: f64,
    pub tw: f64,
    pub vstmax: f64,
    pub vstmin: f64,
}

/// STAB4 — WSCC power system stabilizer variant 4.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stab4Params {
    pub ks: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub t6: f64,
    pub t7: f64,
    pub t8: f64,
    pub hlim: f64,
}

/// STAB5 — WSCC power system stabilizer variant 5.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stab5Params {
    pub ks: f64,
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub t6: f64,
    pub t7: f64,
    pub t8: f64,
    pub t9: f64,
    pub t10: f64,
    pub hlim: f64,
}

// ---------------------------------------------------------------------------
// Phase 25: Governor variant param structs
// ---------------------------------------------------------------------------

/// GGOV2 — GE GGOV1 variant 2 with supplemental load reference input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ggov2Params {
    pub r: f64,
    pub rselect: f64,
    pub tpelec: f64,
    pub maxerr: f64,
    pub minerr: f64,
    pub kpgov: f64,
    pub kigov: f64,
    pub kdgov: f64,
    pub tdgov: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub tact: f64,
    pub kturb: f64,
    pub wfnl: f64,
    pub tb: f64,
    pub tc: f64,
    pub flag: f64,
    pub teng: f64,
    pub tfload: f64,
    pub kpload: f64,
    pub kiload: f64,
    pub ldref: f64,
    pub dm: f64,
    pub ropen: f64,
    pub rclose: f64,
    pub kimw: f64,
    pub pmwset: f64,
    pub aset: f64,
    pub ka: f64,
    pub ta: f64,
    pub db: f64,
    pub tsa: f64,
    pub tsb: f64,
    pub rup: f64,
    pub rdown: f64,
    pub pmax: f64,
    pub pmin: f64,
}

/// GGOV3 — GE GGOV1 variant 3 with washout filter on speed signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ggov3Params {
    pub r: f64,
    pub rselect: f64,
    pub tpelec: f64,
    pub maxerr: f64,
    pub minerr: f64,
    pub kpgov: f64,
    pub kigov: f64,
    pub kdgov: f64,
    pub tdgov: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub tact: f64,
    pub kturb: f64,
    pub wfnl: f64,
    pub tb: f64,
    pub tc: f64,
    pub flag: f64,
    pub teng: f64,
    pub tfload: f64,
    pub kpload: f64,
    pub kiload: f64,
    pub ldref: f64,
    pub dm: f64,
    pub ropen: f64,
    pub rclose: f64,
    pub kimw: f64,
    pub pmwset: f64,
    pub aset: f64,
    pub ka: f64,
    pub ta: f64,
    pub db: f64,
    pub tsa: f64,
    pub tsb: f64,
    pub tw: f64,
    pub rup: f64,
    pub rdown: f64,
    pub pmax: f64,
    pub pmin: f64,
}

/// WPIDHY — Woodward PID Hydro Governor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpidhyParams {
    pub gatmax: f64,
    pub gatmin: f64,
    pub reg: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub ta: f64,
    pub tb: f64,
    pub tw: f64,
    pub at: f64,
    pub dturb: f64,
    pub gmax: f64,
    pub gmin: f64,
    pub pmax: f64,
    pub pmin: f64,
}

/// H6B — Six-State Hydro Governor Variant B.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H6bParams {
    pub tg: f64, // governor time constant
    pub tp: f64, // pilot valve time constant
    pub uo: f64, // max gate opening rate
    pub uc: f64, // max gate closing rate
    pub pmax: f64,
    pub pmin: f64,
    pub beta: f64,  // turbine gain
    pub tw: f64,    // water starting time
    pub dbinf: f64, // dead band inferior
    pub dbsup: f64, // dead band superior
}

/// WSHYDD — WSHYGP with speed deadband.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WshyddParams {
    pub r: f64,
    pub tf: f64,
    pub tg: f64,
    pub tw: f64,
    pub db: f64, // speed deadband (pu)
    pub kd: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub kp: f64,
    pub ki: f64,
}

// ---------------------------------------------------------------------------
// Phase 26: HVDC/FACTS Advanced param structs
// ---------------------------------------------------------------------------

/// HVDCPLU1 — PSS/E LCC (line-commutated converter) HVDC two-terminal model.
///
/// Implements 6-pulse bridge firing-angle / extinction-angle physics with constant-current
/// control at the rectifier, CEA control at the inverter, VDCOL, and a DC circuit ODE.
/// This replaces the former VSC proxy (which was wrong physics).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvdcPlu1Params {
    /// Scheduled DC power (pu on mbase).
    pub setvl: f64,
    /// Scheduled DC voltage (pu on system base).
    pub vschd: f64,
    /// MVA base of the HVDC link.
    pub mbase: f64,
    /// Commutation reactance at rectifier (pu).
    pub xcr: f64,
    /// Commutation reactance at inverter (pu).
    pub xci: f64,
    /// DC line resistance (pu).
    pub rdc: f64,
    /// DC circuit time constant (s).
    pub td: f64,
    /// Measurement / control filter time constant (s).
    pub tr: f64,
    /// Minimum firing angle at rectifier (rad).
    pub alpha_min: f64,
    /// Maximum firing angle at rectifier (rad).
    pub alpha_max: f64,
    /// Minimum extinction angle at inverter (rad).
    pub gamma_min: f64,
    /// CC control proportional gain.
    pub kp_id: f64,
    /// CC control integral gain.
    pub ki_id: f64,
    /// Power order ramp time constant (s).
    pub t_ramp: f64,
    /// Max power (pu).
    pub pmax: f64,
    /// Min power (pu, usually 0 or small positive).
    pub pmin: f64,
    /// Rectifier AC bus number.
    pub rectifier_bus: u32,
    /// Inverter AC bus number.
    pub inverter_bus: u32,
}

/// CSVGN6 — SVC Variant 6 with Auxiliary Inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Csvgn6Params {
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub k: f64,
    pub k_aux: f64,
    pub t_aux: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub bmax: f64,
    pub bmin: f64,
}

/// STCON1 — STATCOM with Inner Current Control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stcon1Params {
    pub tr: f64,
    pub kp: f64,
    pub ki: f64,
    pub kp_i: f64,
    pub ki_i: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub iqmax: f64,
    pub iqmin: f64,
    pub mbase: f64,
}

/// GCSC — Gate-Controlled Series Compensator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcscParams {
    pub tr: f64,
    pub kp: f64,
    pub ki: f64,
    pub xmax: f64,
    pub xmin: f64,
    pub mbase: f64,
}

/// SSSC — Static Synchronous Series Compensator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsscParams {
    pub tr: f64,
    pub kp: f64,
    pub ki: f64,
    pub kp_i: f64,
    pub ki_i: f64,
    pub vqmax: f64,
    pub vqmin: f64,
    pub mbase: f64,
}

/// UPFC — Unified Power Flow Controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpfcParams {
    pub tr: f64,
    pub kp_p: f64,
    pub ki_p: f64,
    pub kp_q: f64,
    pub ki_q: f64,
    pub kp_v: f64,
    pub ki_v: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub qmax: f64,
    pub qmin: f64,
    pub mbase: f64,
}

/// CDC3T — Three-Terminal LCC HVDC (extends CDC4T with third terminal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cdc3tParams {
    pub tr: f64,
    pub kp1: f64,
    pub ki1: f64,
    pub kp2: f64,
    pub ki2: f64,
    pub kp3: f64,
    pub ki3: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub mbase: f64,
}

// ---------------------------------------------------------------------------
// Phase 27: Generator / Load / Protection param structs
// ---------------------------------------------------------------------------

/// REGCO1 — Grid-following converter generator (4 states).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regco1Params {
    pub tr: f64,
    pub kp_v: f64,
    pub ki_v: f64,
    pub kp_i: f64,
    pub ki_i: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub iqmax: f64,
    pub iqmin: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub mbase: f64,
}

/// GENSAL3 — Third-order salient-pole synchronous generator (3 dynamic states).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gensal3Params {
    pub td0_prime: f64,
    pub h: f64,
    pub d: f64,
    pub xd: f64,
    pub xq: f64,
    pub xd_prime: f64,
    pub xl: f64,
    pub s1: f64,
    pub s12: f64,
}

/// LCFB1 — Load compensator with frequency bias (2 states).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lcfb1Params {
    pub tc: f64,
    pub tb: f64,
    pub kf: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub mbase: f64,
}

/// LDFRAL — Dynamic load frequency regulation (2 states).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LdfralParams {
    pub tc: f64,
    pub tb: f64,
    pub kf: f64,
    pub kp: f64,
    pub pmax: f64,
    pub pmin: f64,
    pub mbase: f64,
}

/// FRQTPLT — Frequency relay trip (1 state + bool flag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrqtpltParams {
    pub tf: f64,
    pub fmin: f64,
    pub fmax: f64,
    pub p_trip: f64,
}

/// LVSHBL — Low-voltage shunt block (1 state + bool flag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LvshblParams {
    pub tv: f64,
    pub vmin: f64,
    pub p_block: f64,
}

// ---------------------------------------------------------------------------

/// UVLS1 — Under-Voltage Load Shedding relay (single stage, per-bus).
/// Multi-stage UVLS via multiple UVLS1 records at the same bus.
/// DYR: UVLS1 bus 'id' tv vmin t_delay p_shed v_reconnect t_reconnect /
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Uvls1Params {
    /// Voltage measurement filter time constant (seconds).
    pub tv: f64,
    /// Undervoltage threshold (pu).
    pub vmin: f64,
    /// Trip delay after continuous undervoltage (seconds).
    pub t_delay: f64,
    /// Fraction of load to shed (0.0–1.0).
    pub p_shed: f64,
    /// Voltage threshold for reconnection (pu); 0.0 disables reconnection.
    pub v_reconnect: f64,
    /// Reconnection delay (seconds).
    pub t_reconnect: f64,
}

// ---------------------------------------------------------------------------

/// A `.dyr` record with an unrecognised model name — stored verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownDyrRecord {
    /// Bus number extracted from the record (0 if the bus field was not numeric).
    pub bus: u32,
    /// Raw model name as read from the file.
    pub model_name: String,
    /// Machine ID string.
    pub machine_id: String,
    /// All numeric parameter tokens from the record.
    pub params: Vec<f64>,
}

// ---------------------------------------------------------------------------
// Phase 28: REPCGFM_C1 / DERP / REGFM_D1 / WTDTA1 / WTARA1 / WTPTA1
// ---------------------------------------------------------------------------

/// REPCGFM_C1 — GFM plant-level Volt/Var controller (3 states).
///
/// Plant-level companion to REGFM_C1. Integrating PI loops for voltage and
/// reactive power, plus a frequency droop state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repcgfmc1Params {
    pub kp_v: f64,
    pub ki_v: f64,
    pub vmax: f64,
    pub vmin: f64,
    pub kp_q: f64,
    pub ki_q: f64,
    pub qmax: f64,
    pub qmin: f64,
    pub tlag: f64,
    pub fdroop: f64,
    pub dbd1: f64,
    pub dbd2: f64,
}

/// DERP — DER with Protection (2 states).
///
/// DER_A variant with explicit frequency and voltage protection relay
/// trip logic on top of the DERA inverter output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerpParams {
    pub x_eq: f64,
    pub trf: f64,
    pub imax: f64,
    pub trv: f64,
    /// PLL filter time constant (s).
    pub tpll: f64,
    /// Lower frequency trip threshold (pu).
    pub flow: f64,
    /// Upper frequency trip threshold (pu).
    pub fhigh: f64,
    /// Lower voltage trip threshold (pu).
    pub vlow: f64,
    /// Upper voltage trip threshold (pu).
    pub vhigh: f64,
    /// Protection trip time constant (s).
    pub trip: f64,
    /// Reconnect time constant (s).
    pub treconnect: f64,
}

/// REGFM_D1 — WECC Sep-2025 hybrid GFM/GFL converter (8 states).
///
/// Droop-based voltage/frequency forming with current-limit handoff to GFL
/// mode. Structurally extends REGFM_C1 with 2 extra VOC + anti-windup states.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regfmd1Params {
    pub rrv: f64,
    pub lrv: f64,
    pub kpv: f64,
    pub kiv: f64,
    pub kpg: f64,
    pub kig: f64,
    pub kdroop: f64,
    pub kvir: f64,
    pub kfir: f64,
    pub imax: f64,
    pub dpf: f64,
    pub dqf: f64,
    pub x_eq: f64,
    pub mbase: f64,
    /// PLL tracking filter time constant (s, default 0.02).
    pub tpll: f64,
    /// Voltage measurement filter time constant (s, default 0.02).
    pub tv: f64,
}

/// WTDTA1 — Wind turbine two-mass drive-train (2 states).
///
/// ωr (rotor speed deviation) and θtwist (shaft twist angle).
/// Works alongside REGCA/WT3G2U generators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wtdta1Params {
    /// Inertia constant of the rotor (s).
    pub h: f64,
    /// Shaft damping coefficient (pu).
    pub dshaft: f64,
    /// Shaft stiffness coefficient (pu/rad).
    pub kshaft: f64,
    /// Second mass damping.
    pub d2: f64,
}

/// WTARA1 — Wind turbine aerodynamic aggregation (2 states).
///
/// State: Paero (aerodynamic power), Pmech (mechanical power output).
/// Cp-lambda power curve simplified as first-order lag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wtara1Params {
    /// Aerodynamic power gain.
    pub ka: f64,
    /// Aerodynamic time constant (s).
    pub ta: f64,
    /// Mechanical power gain.
    pub km: f64,
    /// Mechanical time constant (s).
    pub tm: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
}

/// WTPTA1 — Wind turbine pitch angle control (2 states).
///
/// States: θcmd (pitch command), θact (actual pitch, rate-limited servo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wtpta1Params {
    /// Proportional gain of pitch PI controller.
    pub kpp: f64,
    /// Integral gain of pitch PI controller.
    pub kip: f64,
    /// Maximum pitch angle (deg).
    pub theta_max: f64,
    /// Minimum pitch angle (deg).
    pub theta_min: f64,
    /// Maximum pitch rate (deg/s).
    pub rate_max: f64,
    /// Minimum pitch rate (deg/s) — typically negative.
    pub rate_min: f64,
    /// Servo time constant (s).
    pub te: f64,
    /// Pitch-to-power gain (pu MW per deg).
    pub kp_pitch: f64,
}

/// Cp(λ,β) lookup table for full aerodynamic wind turbine models.
///
/// Power coefficient Cp is tabulated as a function of tip-speed ratio λ
/// (lambda) and blade pitch angle β (beta). Values are stored row-major
/// in `cp_values[i_lambda * n_beta + i_beta]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpTable {
    /// Tip-speed ratio breakpoints (dimensionless).
    pub lambda_bp: Vec<f64>,
    /// Pitch angle breakpoints (degrees).
    pub beta_bp: Vec<f64>,
    /// Cp values, row-major: `[n_lambda × n_beta]`.
    pub cp_values: Vec<f64>,
}

impl CpTable {
    /// Bilinear interpolation of Cp at given (lambda, beta).
    pub fn interpolate(&self, lambda: f64, beta: f64) -> f64 {
        let (il, il1, fl) = Self::find_bracket(&self.lambda_bp, lambda);
        let (ib, ib1, fb) = Self::find_bracket(&self.beta_bp, beta);
        let nb = self.beta_bp.len();

        let c00 = self.cp_values[il * nb + ib];
        let c01 = self.cp_values[il * nb + ib1];
        let c10 = self.cp_values[il1 * nb + ib];
        let c11 = self.cp_values[il1 * nb + ib1];

        let c0 = c00 + fb * (c01 - c00);
        let c1 = c10 + fb * (c11 - c10);
        (c0 + fl * (c1 - c0)).max(0.0)
    }

    /// NREL 5-MW reference turbine Cp table (public domain data).
    ///
    /// Simplified 8×6 table covering λ ∈ [2, 16] and β ∈ [0, 25]°.
    pub fn nrel_5mw() -> Self {
        let lambda_bp = vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0];
        let beta_bp = vec![0.0, 5.0, 10.0, 15.0, 20.0, 25.0];
        #[rustfmt::skip]
        let cp_values = vec![
            // β=0     β=5    β=10   β=15   β=20   β=25
            0.10,  0.06,  0.02,  0.01,  0.00,  0.00,  // λ=2
            0.30,  0.20,  0.10,  0.04,  0.01,  0.00,  // λ=4
            0.42,  0.32,  0.18,  0.08,  0.03,  0.01,  // λ=6
            0.48,  0.38,  0.22,  0.11,  0.04,  0.01,  // λ=8
            0.44,  0.34,  0.20,  0.10,  0.04,  0.01,  // λ=10
            0.35,  0.26,  0.15,  0.07,  0.03,  0.01,  // λ=12
            0.22,  0.16,  0.09,  0.04,  0.02,  0.01,  // λ=14
            0.10,  0.07,  0.04,  0.02,  0.01,  0.00,  // λ=16
        ];
        Self {
            lambda_bp,
            beta_bp,
            cp_values,
        }
    }

    /// Find bracketing indices and interpolation fraction.
    fn find_bracket(bp: &[f64], val: f64) -> (usize, usize, f64) {
        if bp.len() < 2 {
            return (0, 0, 0.0);
        }
        if val <= bp[0] {
            return (0, 0, 0.0);
        }
        if val >= bp[bp.len() - 1] {
            let n = bp.len() - 1;
            return (n, n, 0.0);
        }
        // Binary search for bracket.
        let pos = bp.partition_point(|&x| x <= val);
        let i = if pos > 0 { pos - 1 } else { 0 };
        let i1 = (i + 1).min(bp.len() - 1);
        let span = bp[i1] - bp[i];
        let frac = if span.abs() > 1e-15 {
            (val - bp[i]) / span
        } else {
            0.0
        };
        (i, i1, frac)
    }
}

/// WTAERO — Full aerodynamic wind turbine model with Cp(λ,β) table.
///
/// Computes aerodynamic power from wind speed, rotor speed, and pitch angle
/// using a tabulated power coefficient surface. Optionally models the
/// two-mass drive train (rotor + generator) with shaft flexibility.
///
/// States (single-mass): p_aero (aerodynamic power, filtered).
/// States (two-mass): omega_r (rotor speed), theta_tw (shaft twist), p_aero.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WtaeroParams {
    /// Air density (kg/m³). Default: 1.225 (ISA sea level).
    pub rho: f64,
    /// Rotor radius (m).
    pub r_rotor: f64,
    /// Gear ratio (generator_speed / rotor_speed).
    pub gear_ratio: f64,
    /// Cp(λ,β) lookup table.
    pub cp_table: CpTable,
    /// Base wind speed for steady-state initialization (m/s).
    pub v_wind_base: f64,
    /// Generator MVA base (for per-unit conversion).
    pub mbase_mw: f64,
    /// Two-mass rotor inertia (s). None = single-mass.
    pub h_rotor: Option<f64>,
    /// Shaft stiffness (pu/rad). Required for two-mass.
    pub k_shaft: Option<f64>,
    /// Shaft damping (pu). Required for two-mass.
    pub d_shaft: Option<f64>,
}

// ---------------------------------------------------------------------------
// Wave 34: New generator, governor, load, FACTS param structs
// ---------------------------------------------------------------------------

/// IEESGO — IEEE Standard Governor (simplified 5-state steam turbine governor).
///
/// PSS/E params: `T1 T2 T3 T4 T5 T6 K1 K2 K3 PMAX PMIN`
///
/// T1=lead TC, T2=lag TC (lead-lag), T3=valve TC,
/// T4=HP TC, T5=reheat TC, T6=LP TC; K1+K2+K3=1 (HP/IP/LP fractions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IeesgoParams {
    pub t1: f64,
    pub t2: f64,
    pub t3: f64,
    pub t4: f64,
    pub t5: f64,
    pub t6: f64,
    pub k1: f64,
    pub k2: f64,
    pub k3: f64,
    pub pmax: f64,
    pub pmin: f64,
}

/// WTTQA1 — WECC Type 2 Wind Torque Controller (2 states, governor slot).
///
/// PSS/E params: `Kp Ki Tp Pmax Pmin`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wttqa1Params {
    pub kp: f64,
    pub ki: f64,
    pub tp: f64,
    pub pmax: f64,
    pub pmin: f64,
    /// Torque mode flag: 0 = speed error mode, 1 = power error mode.
    pub tflag: i32,
    /// Speed reference filter time constant (s).
    pub twref: f64,
    /// Maximum torque (pu).
    pub temax: f64,
    /// Minimum torque (pu).
    pub temin: f64,
    /// Speed-power lookup breakpoints: (power, speed_ref) pairs.
    /// Piecewise linear mapping from measured power to speed reference.
    pub spl: [(f64, f64); 4],
}

/// CIM6 — 6th-order induction motor (extends CIM5 with q-axis transient).
///
/// Uses same 3-state structure as CIM5 (slip, ed_prime, eq_prime) but adds
/// `tq0p` and `xq_prime` for q-axis transient dynamics.
///
/// PSS/E params: `RA XS XM XR1 XR2 RR1 RR2 [H E1 S1 E2 S2 MBASE TQ0P XQP]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cim6Params {
    pub ra: f64,
    pub xs: f64,
    pub xm: f64,
    pub xr1: f64,
    pub xr2: f64,
    pub rr1: f64,
    pub rr2: f64,
    pub h: f64,
    pub e1: f64,
    pub s1: f64,
    pub e2: f64,
    pub s2: f64,
    pub mbase: f64,
    pub tq0p: f64,
    pub xq_prime: f64,
}

/// EXTL — External Load (simplified composite 2-state load model).
///
/// Models voltage/frequency-dependent load with first-order filters.
///
/// PSS/E params: `Tp Tq Kpv Kqv Kpf Kqf mbase lfac`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtlParams {
    pub tp: f64,
    pub tq: f64,
    pub kpv: f64,
    pub kqv: f64,
    pub kpf: f64,
    pub kqf: f64,
    pub mbase: f64,
    pub lfac: f64,
}

/// SVSMO1 — WECC Generic SVC voltage regulator (1-state, FACTS slot).
///
/// Simple first-order SVC model.  PSS/E params: `Tr K Ta Bmin Bmax`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Svsmo1Params {
    pub tr: f64,
    pub k: f64,
    pub ta: f64,
    pub b_min: f64,
    pub b_max: f64,
}

/// SVSMO2 — WECC Generic STATCOM (1-state, FACTS slot).
///
/// Similar to CSTCON. PSS/E params: `Tr K Ta IqMin IqMax`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Svsmo2Params {
    pub tr: f64,
    pub k: f64,
    pub ta: f64,
    pub iq_min: f64,
    pub iq_max: f64,
}

/// SVSMO3 — WECC Advanced SVC (2-state: b_svc + vr, FACTS slot).
///
/// Lead-lag voltage regulator driving susceptance.
/// PSS/E params: `Tr Ka Ta Tb Bmin Bmax`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Svsmo3Params {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub tb: f64,
    pub b_min: f64,
    pub b_max: f64,
}

// ---------------------------------------------------------------------------
// Wave 35: New param structs
// ---------------------------------------------------------------------------

// --- Generator protection relays -------------------------------------------

/// VTGTPAT / VTGDCAT — Voltage-Time Generator Protection Trip params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VtgtpatParams {
    /// Voltage filter time constant (s).
    pub tv: f64,
    /// Trip voltage threshold (pu, below which generator trips).
    pub vtrip: f64,
    /// Reset voltage threshold (pu, above which relay resets).
    pub vreset: f64,
}

/// FRQTPAT / FRQDCAT — Frequency-Time Generator Protection Trip params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrqtpatParams {
    /// Frequency filter time constant (s).
    pub tf: f64,
    /// High-frequency trip threshold (pu, above which generator trips).
    pub ftrip_hi: f64,
    /// Low-frequency trip threshold (pu, below which generator trips).
    pub ftrip_lo: f64,
    /// Reset frequency threshold (pu).
    pub freset: f64,
}

// --- Hydro governors -------------------------------------------------------

/// HYGOV4 — Hydro Governor with Surge Tank (5 states).
///
/// PSS/E params: `R TF TG TR HDAM TW QNL AT DG GMAX GMIN TS KS PMAX PMIN`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hygov4Params {
    /// Governor time constant (pilot valve, s).
    pub tr: f64,
    /// Pilot filter time constant (s).
    pub tf: f64,
    /// Turbine damping (pu).
    pub dturb: f64,
    /// Head at zero flow (pu).
    pub hdam: f64,
    /// Penstock water time constant (s).
    pub tw: f64,
    /// No-load flow at nominal head (pu).
    pub qnl: f64,
    /// Turbine gain (pu).
    pub at: f64,
    /// Governor servo gain (pu/pu).
    pub dg: f64,
    /// Maximum gate position (pu).
    pub gmax: f64,
    /// Minimum gate position (pu).
    pub gmin: f64,
    /// Surge tank time constant (s).
    pub ts: f64,
    /// Surge tank orifice loss coefficient (pu).
    pub ks: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
}

/// WEHGOV — WECC Enhanced Hydro Governor (4 states).
///
/// PSS/E params: `R TR TF TG TW AT DTURB QNL GMAX GMIN DBD1 DBD2 PMAX PMIN`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WehgovParams {
    /// Droop (pu).
    pub r: f64,
    /// Governor filter time constant (s).
    pub tr: f64,
    /// Pilot filter time constant (s).
    pub tf: f64,
    /// Gate servo time constant (s).
    pub tg: f64,
    /// Penstock water time constant (s).
    pub tw: f64,
    /// Turbine gain (pu).
    pub at: f64,
    /// Turbine damping (pu).
    pub dturb: f64,
    /// No-load flow (pu).
    pub qnl: f64,
    /// Maximum gate position (pu).
    pub gmax: f64,
    /// Minimum gate position (pu).
    pub gmin: f64,
    /// Speed deadband lower limit (pu, negative).
    pub dbd1: f64,
    /// Speed deadband upper limit (pu, positive).
    pub dbd2: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
}

/// IEEEG3 — IEEE Type G3 Hydro Governor (3 states).
///
/// PSS/E params: `TG TP UO UC PMAX PMIN TW AT DTURB QNL`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeeg3Params {
    /// Gate servo time constant (s).
    pub tg: f64,
    /// Pilot valve time constant (s).
    pub tp: f64,
    /// Maximum gate opening rate (pu/s).
    pub uo: f64,
    /// Maximum gate closing rate (pu/s, negative in PSS/E).
    pub uc: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
    /// Water time constant (s).
    pub tw: f64,
    /// Turbine gain (pu).
    pub at: f64,
    /// Turbine damping (pu).
    pub dturb: f64,
    /// No-load flow (pu).
    pub qnl: f64,
}

/// IEEEG4 — IEEE Type G4 Hydro Governor (3 states, lead-lag form).
///
/// PSS/E params: `T1 T2 T3 KI PMAX PMIN TW AT DTURB QNL`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ieeeg4Params {
    /// Lead-lag time constant 1 (s).
    pub t1: f64,
    /// Lead-lag time constant 2 (s).
    pub t2: f64,
    /// Lead-lag time constant 3 (s).
    pub t3: f64,
    /// Integral gain (pu/pu/s).
    pub ki: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
    /// Water time constant (s).
    pub tw: f64,
    /// Turbine gain (pu).
    pub at: f64,
    /// Turbine damping (pu).
    pub dturb: f64,
    /// No-load flow (pu).
    pub qnl: f64,
}

// --- IEEE 421.5-2016 exciters (C-series) -----------------------------------

/// ESAC7C — IEEE 421.5-2016 AC7C exciter params (6 states).
///
/// Structurally identical to ESAC7B; used as a C-series alias.
/// PSS/E params same as ESAC7B.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esac7cParams {
    pub tr: f64,
    pub kpr: f64,
    pub kir: f64,
    pub kdr: f64,
    pub tdr: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ka: f64,
    pub ta: f64,
    pub kp: f64,
    pub kl: f64,
    pub te: f64,
    pub ke: f64,
    pub vfemax: f64,
    pub vemin: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
}

/// ESDC4C — IEEE 421.5-2016 DC4C exciter params (3 states).
///
/// PSS/E params: `TR KA TA KPR KIR KDR TDR VRMAX VRMIN KE TE KF TF E1 SE1 E2 SE2`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esdc4cParams {
    pub tr: f64,
    pub ka: f64,
    pub ta: f64,
    pub kpr: f64,
    pub kir: f64,
    pub kdr: f64,
    pub tdr: f64,
    pub vrmax: f64,
    pub vrmin: f64,
    pub ke: f64,
    pub te: f64,
    pub kf: f64,
    pub tf: f64,
    pub e1: f64,
    pub se1: f64,
    pub e2: f64,
    pub se2: f64,
}

// --- IEEE 421.5-2016 PSS (C-series) ----------------------------------------

/// PSS7C — IEEE 421.5-2016 multi-band PSS (6 states).
///
/// Three frequency bands (low / intermediate / high), each with a washout
/// filter and a lead-lag compensator.  Output is the gain-weighted sum of
/// all three bands, clamped to `[vstmin, vstmax]`.
///
/// DYR params (extended): `KL TWL T1L T2L  KI TWI T1I T2I  KH TWH T1H T2H  VSTMAX VSTMIN`
///
/// Legacy 9-param form (`KSS TW1 TW2 T1 T2 T3 T4 VSMAX VSMIN`) is still
/// accepted — the old fields are mapped to the intermediate band and the
/// low/high bands default to zero gain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pss7cParams {
    // --- legacy fields (kept for DYR round-trip / serde compat) -------------
    /// Legacy overall gain — mapped to intermediate band if per-band gains
    /// are all zero.
    #[serde(default)]
    pub kss: f64,
    /// Legacy washout time constant 1 (s).
    #[serde(default)]
    pub tw1: f64,
    /// Legacy washout time constant 2 (s).
    #[serde(default)]
    pub tw2: f64,
    /// Legacy lead time constant 1 (s).
    #[serde(default)]
    pub t1: f64,
    /// Legacy lag time constant 1 (s).
    #[serde(default)]
    pub t2: f64,
    /// Legacy lead time constant 2 (s).
    #[serde(default)]
    pub t3: f64,
    /// Legacy lag time constant 2 (s).
    #[serde(default)]
    pub t4: f64,
    /// Maximum PSS output (pu)  — legacy alias for `vstmax`.
    #[serde(default = "default_vsmax")]
    pub vsmax: f64,
    /// Minimum PSS output (pu)  — legacy alias for `vstmin`.
    #[serde(default = "default_vsmin")]
    pub vsmin: f64,

    // --- per-band parameters (multi-band PSS) ------------------------------
    /// Low-frequency band gain (pu).
    #[serde(default)]
    pub kl: f64,
    /// Low-frequency washout time constant (s).
    #[serde(default = "default_tw")]
    pub tw_l: f64,
    /// Low-frequency lead time constant (s).
    #[serde(default)]
    pub t1_l: f64,
    /// Low-frequency lag time constant (s).
    #[serde(default = "default_tlag")]
    pub t2_l: f64,

    /// Intermediate-frequency band gain (pu).
    #[serde(default)]
    pub ki: f64,
    /// Intermediate-frequency washout time constant (s).
    #[serde(default = "default_tw")]
    pub tw_i: f64,
    /// Intermediate-frequency lead time constant (s).
    #[serde(default)]
    pub t1_i: f64,
    /// Intermediate-frequency lag time constant (s).
    #[serde(default = "default_tlag")]
    pub t2_i: f64,

    /// High-frequency band gain (pu).
    #[serde(default)]
    pub kh: f64,
    /// High-frequency washout time constant (s).
    #[serde(default = "default_tw")]
    pub tw_h: f64,
    /// High-frequency lead time constant (s).
    #[serde(default)]
    pub t1_h: f64,
    /// High-frequency lag time constant (s).
    #[serde(default = "default_tlag")]
    pub t2_h: f64,

    /// Multi-band maximum PSS output (pu).
    #[serde(default = "default_vsmax")]
    pub vstmax: f64,
    /// Multi-band minimum PSS output (pu).
    #[serde(default = "default_vsmin")]
    pub vstmin: f64,
}

fn default_vsmax() -> f64 {
    0.1
}
fn default_vsmin() -> f64 {
    -0.1
}
fn default_tw() -> f64 {
    10.0
}
fn default_tlag() -> f64 {
    0.04
}

impl Pss7cParams {
    /// Effective per-band parameters.  When the new per-band gains are all
    /// zero **and** `kss != 0`, fall back to the legacy single-band mapping
    /// (intermediate band only).
    pub fn effective_bands(&self) -> Pss7cBands {
        if self.kl == 0.0 && self.ki == 0.0 && self.kh == 0.0 && self.kss != 0.0 {
            // Legacy mode: map kss → intermediate band.
            Pss7cBands {
                kl: 0.0,
                tw_l: self.tw1,
                t1_l: 0.0,
                t2_l: 0.04,
                ki: self.kss,
                tw_i: self.tw1,
                t1_i: self.t1,
                t2_i: self.t2,
                kh: 0.0,
                tw_h: self.tw2,
                t1_h: 0.0,
                t2_h: 0.04,
                vstmax: self.vsmax,
                vstmin: self.vsmin,
            }
        } else {
            Pss7cBands {
                kl: self.kl,
                tw_l: self.tw_l,
                t1_l: self.t1_l,
                t2_l: self.t2_l,
                ki: self.ki,
                tw_i: self.tw_i,
                t1_i: self.t1_i,
                t2_i: self.t2_i,
                kh: self.kh,
                tw_h: self.tw_h,
                t1_h: self.t1_h,
                t2_h: self.t2_h,
                vstmax: self.vstmax,
                vstmin: self.vstmin,
            }
        }
    }
}

/// Resolved per-band parameters for PSS7C evaluation.
#[derive(Debug, Clone)]
pub struct Pss7cBands {
    pub kl: f64,
    pub tw_l: f64,
    pub t1_l: f64,
    pub t2_l: f64,
    pub ki: f64,
    pub tw_i: f64,
    pub t1_i: f64,
    pub t2_i: f64,
    pub kh: f64,
    pub tw_h: f64,
    pub t1_h: f64,
    pub t2_h: f64,
    pub vstmax: f64,
    pub vstmin: f64,
}

// ---------------------------------------------------------------------------
// Wave 36: New governor, generator, exciter, and load model structs
// ---------------------------------------------------------------------------

// --- Combined Cycle Governors ---

/// GOVCT1 — Single-shaft combined cycle turbine governor (common in ERCOT/WECC).
///
/// PSS/E params: `R T1 VMAX VMIN T2 T3 K1 K2 K3 T4 T5 T6 K7 K8 PMAX PMIN [TD]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Govct1Params {
    /// Speed regulation (droop, pu).
    pub r: f64,
    /// Governor time constant (s).
    pub t1: f64,
    /// Maximum valve position (pu).
    pub vmax: f64,
    /// Minimum valve position (pu).
    pub vmin: f64,
    /// Lead time constant (s).
    pub t2: f64,
    /// Lag time constant (s).
    pub t3: f64,
    /// HP turbine fraction.
    pub k1: f64,
    /// LP1 turbine fraction.
    pub k2: f64,
    /// LP2 turbine fraction (= 1-k1-k2).
    pub k3: f64,
    /// HP turbine time constant (s).
    pub t4: f64,
    /// LP1 time constant (s).
    pub t5: f64,
    /// LP2 time constant (s).
    pub t6: f64,
    /// Gas turbine coefficient 1.
    pub k7: f64,
    /// Gas turbine coefficient 2.
    pub k8: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
    /// Governor deadband (optional, default 0).
    #[serde(default)]
    pub td: f64,
}

/// GOVCT2 — Two-shaft combined cycle gas turbine governor (7 states).
///
/// Models a gas turbine (GT) driving one generator plus a steam turbine (ST)
/// driven by heat recovery from the GT exhaust via an HRSG.
///
/// PSS/E params: `R T1 VMAX VMIN T2 T3 K1 K2 K3 T4 T5 T6 K7 K8 PMAX PMIN [TD T_HRSG K_ST T_ST]`
///
/// States x1–x5 are the gas turbine (identical to GOVCT1).
/// State x_hrsg captures HRSG steam generation dynamics.
/// State x_st captures steam turbine output dynamics.
/// Total Pm = Pm_gt + x_st.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Govct2Params {
    // --- Gas turbine fields (same as Govct1Params) ---
    /// Speed regulation (droop, pu).
    pub r: f64,
    /// Governor time constant (s).
    pub t1: f64,
    /// Maximum valve position (pu).
    pub vmax: f64,
    /// Minimum valve position (pu).
    pub vmin: f64,
    /// Lead time constant (s).
    pub t2: f64,
    /// Lag time constant (s).
    pub t3: f64,
    /// HP turbine fraction.
    pub k1: f64,
    /// LP1 turbine fraction.
    pub k2: f64,
    /// LP2 turbine fraction (= 1-k1-k2).
    pub k3: f64,
    /// HP turbine time constant (s).
    pub t4: f64,
    /// LP1 time constant (s).
    pub t5: f64,
    /// LP2 time constant (s).
    pub t6: f64,
    /// Gas turbine coefficient 1.
    pub k7: f64,
    /// Gas turbine coefficient 2.
    pub k8: f64,
    /// Maximum power output (pu).
    pub pmax: f64,
    /// Minimum power output (pu).
    pub pmin: f64,
    /// Governor deadband (optional, default 0).
    #[serde(default)]
    pub td: f64,
    // --- Steam turbine / HRSG fields (GOVCT2-specific) ---
    /// HRSG time constant (s) — typically 60-120s.
    pub t_hrsg: f64,
    /// Steam-to-gas power ratio — typically 0.33-0.5.
    pub k_st: f64,
    /// Steam turbine lag time constant (s) — typically 5-15s.
    pub t_st: f64,
}

// --- Advanced Steam Governors ---

/// TGOV3 — TGOV1 variant with two-reheat steam turbine (3 states).
///
/// PSS/E params: `R T1 VMAX VMIN T2 T3 DT KD`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tgov3Params {
    /// Speed regulation (droop, pu).
    pub r: f64,
    /// Governor time constant (s).
    pub t1: f64,
    /// Maximum valve position (pu).
    pub vmax: f64,
    /// Minimum valve position (pu).
    pub vmin: f64,
    /// First reheat time constant (s).
    pub t2: f64,
    /// Second reheat time constant (s).
    pub t3: f64,
    /// Turbine damping (pu).
    pub dt: f64,
    /// Derivative gain.
    pub kd: f64,
}

// --- Legacy Wind Generator Params ---

/// WT1G1 / WT2G1 — Type 1/2 induction-machine wind generator (3rd-order model).
///
/// PSS/E DYR record: `H D RA X_EQ IMAX`
///
/// WT1G1 is a squirrel-cage induction generator (Type 1) directly coupled to
/// the grid with no power electronics.  WT2G1 adds external rotor resistance
/// control via the WT2E1 governor.
///
/// The 3rd-order model tracks transient EMFs (E'_d, E'_q) and rotor slip:
/// ```text
/// dE'_q/dt = -s·ω_s·E'_d - (E'_q + (X - X')·I_d) / T'_0
/// dE'_d/dt =  s·ω_s·E'_q - (E'_d - (X - X')·I_q) / T'_0
/// ds/dt    = (T_e - T_m) / (2H)
/// ```
///
/// # Decomposition from PSS/E X_EQ
///
/// PSS/E provides only the transient reactance X' = X_EQ.  The full IM circuit
/// parameters (Xs, Xm, Xr, Rr) are derived using standard decomposition defaults:
/// - Xs  = 0.10 · X'           (stator leakage)
/// - Xm  = 3.0                 (magnetizing reactance)
/// - Xr  = Xs                  (rotor leakage ≈ stator leakage)
/// - Rr  = 0.01                (rotor resistance)
/// - X   = Xs + Xm             (open-circuit reactance)
/// - T'_0 = (Xm + Xr) / (ω_s · Rr)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt1g1Params {
    /// Inertia constant (s).
    pub h: f64,
    /// Damping coefficient (pu).
    pub d: f64,
    /// Stator resistance Rs (pu).  PSS/E field `RA`.
    pub ra: f64,
    /// Transient reactance X' (pu).  PSS/E field `X_EQ`.
    /// Equal to Xs + Xm·Xr/(Xm+Xr).
    pub x_eq: f64,
    /// Current limit (pu).
    pub imax: f64,
    /// Stator leakage reactance (pu).  Derived: 0.10 × X_EQ.
    pub xs: f64,
    /// Magnetizing reactance (pu).  Default 3.0.
    pub xm: f64,
    /// Rotor leakage reactance (pu).  Derived ≈ Xs.
    pub xr: f64,
    /// Rotor resistance (pu).  Default 0.01.
    pub rr: f64,
}

/// WT2E1 — Type 2 wind electrical controller (governor slot).
///
/// PSS/E params: `KP KI PMAX PMIN TE`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wt2e1Params {
    /// Proportional gain.
    pub kp: f64,
    /// Integral gain.
    pub ki: f64,
    /// Maximum active power (pu).
    pub pmax: f64,
    /// Minimum active power (pu).
    pub pmin: f64,
    /// Electrical control time constant (s).
    pub te: f64,
}

/// DISTR1 — Distance relay (line protection, attaches to bus in load slot).
///
/// PSS/E params: `Z1 Z2 T1 T2 MBASE LFAC`
///
/// Extended with Zone 3, Mho circle angle, protected branch info, and
/// measurement filter time constant for proper impedance-measuring relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Distr1Params {
    /// Zone 1 impedance magnitude (pu).
    pub z1: f64,
    /// Zone 2 impedance magnitude (pu).
    pub z2: f64,
    /// Zone 3 impedance magnitude (pu). Default: 1.5 × z2.
    pub z3: f64,
    /// Zone 1 trip delay (s). Typically ~0 (instantaneous).
    pub t1: f64,
    /// Zone 2 trip delay (s). Typically 0.3–0.5 s.
    pub t2: f64,
    /// Zone 3 trip delay (s). Typically 0.8–1.2 s.
    pub t3: f64,
    /// Mho circle reach angle (degrees). Typical: 60–85°.
    pub reach_angle_deg: f64,
    /// MVA base of protected element.
    pub mbase: f64,
    /// Load fraction this relay monitors.
    pub lfac: f64,
    /// From-bus of the protected branch.
    pub branch_from: u32,
    /// To-bus of the protected branch.
    pub branch_to: u32,
    /// Protected branch resistance (pu).
    pub branch_r: f64,
    /// Protected branch reactance (pu).
    pub branch_x: f64,
    /// Measurement filter time constant (s). Default: 0.02 s (1.2 cycles).
    pub tf: f64,
}

/// BFR50 — Breaker failure relay (ANSI 50BF).
///
/// DYR params: `T_BFR  I_SUP  BRANCH_IDX`
///
/// Monitors the breaker on the protected branch. When a trip command is active
/// and current exceeds the supervision threshold for `t_bfr` seconds, the BFR
/// issues a backup trip to adjacent generators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bfr50Params {
    /// BFR timer duration (s). Typical: 5-10 cycles (83-167ms @ 60Hz).
    pub t_bfr: f64,
    /// Current supervision threshold (pu). BFR only active if I > this.
    pub i_sup: f64,
    /// Internal branch index of the monitored breaker.
    pub branch_idx: usize,
}

// ---------------------------------------------------------------------------
// Wave 7 (B10): Additional protection relay parameter types
// ---------------------------------------------------------------------------

/// 87T — Transformer differential relay.
///
/// Compares high-side and low-side currents of a transformer. Trips when the
/// differential current `|I_H - I_L/ratio|` exceeds the restraint characteristic
/// `slope × I_restraint + I_pickup`.
///
/// DYR params: `SLOPE1 SLOPE2 I_PICKUP HARMONIC_RESTRAINT FROM_BUS TO_BUS CKT TURNS_RATIO TF`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransDiff87Params {
    /// Slope 1 of restraint characteristic (typical 0.2–0.3).
    pub slope1: f64,
    /// Slope 2 (high-current region, typical 0.6–0.8).
    pub slope2: f64,
    /// Minimum pickup current (pu).
    pub i_pickup: f64,
    /// 2nd harmonic blocking ratio (typical 0.15–0.20). Trip blocked if
    /// 2nd-harmonic content > ratio × fundamental.
    pub harmonic_restraint: f64,
    /// High-side bus number.
    pub from_bus: u32,
    /// Low-side bus number.
    pub to_bus: u32,
    /// Circuit identifier.
    pub circuit: String,
    /// Turns ratio (high-side / low-side rated voltage).
    pub turns_ratio: f64,
    /// Measurement filter time constant (s). Default: 0.01 (10ms).
    pub tf: f64,
}

/// 87L — Line differential relay.
///
/// Compares currents at both ends of a transmission line. Trips the branch
/// (not a generator) when the differential current exceeds the restraint.
///
/// DYR params: `SLOPE1 SLOPE2 I_PICKUP FROM_BUS TO_BUS CKT TF`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineDiff87lParams {
    /// Slope 1 of restraint characteristic (typical 0.2–0.3).
    pub slope1: f64,
    /// Slope 2 (high-current region, typical 0.6–0.8).
    pub slope2: f64,
    /// Minimum pickup current (pu).
    pub i_pickup: f64,
    /// From-bus of protected line.
    pub from_bus: u32,
    /// To-bus of protected line.
    pub to_bus: u32,
    /// Circuit identifier.
    pub circuit: String,
    /// Measurement filter time constant (s). Default: 0.01 (10ms).
    pub tf: f64,
}

/// 79 — Automatic recloser.
///
/// After a relay trips a branch, the recloser waits a dead-time delay then
/// recloses the branch. If the fault persists, it re-trips and repeats up to
/// `max_attempts` times before locking out.
///
/// DYR params: `DEAD1 DEAD2 DEAD3 MAX_ATTEMPTS FROM_BUS TO_BUS CKT RESET_TIME`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recloser79Params {
    /// First reclose dead-time delay (s).
    pub dead_time_1: f64,
    /// Second reclose dead-time delay (s).
    pub dead_time_2: f64,
    /// Third reclose dead-time delay (s).
    pub dead_time_3: f64,
    /// Maximum reclose attempts before lockout (1–3).
    pub max_attempts: u32,
    /// From-bus of reclosable branch.
    pub from_bus: u32,
    /// To-bus of reclosable branch.
    pub to_bus: u32,
    /// Circuit identifier.
    pub circuit: String,
    /// Time after successful reclose to reset attempt counter (s).
    pub reset_time: f64,
}

// ---------------------------------------------------------------------------
// Wave 37: OEL/UEL Limiter types
// ---------------------------------------------------------------------------

/// OEL1B — Over-Excitation Limiter Type 1B (inverse-time ramp limiter).
///
/// PSS/E params: `IFDMAX IFDLIM VRMAX VAMIN KRAMP TFF`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Oel1bParams {
    /// Maximum continuous field current (pu) — ramp starts above this.
    pub ifdmax: f64,
    /// Instantaneous trip limit (pu).
    pub ifdlim: f64,
    /// Maximum regulator output (pu).
    pub vrmax: f64,
    /// Minimum amplifier output (pu, negative).
    pub vamin: f64,
    /// Limiter ramp rate (pu/s).
    pub kramp: f64,
    /// Field current filter time constant (s).
    pub tff: f64,
}

/// OEL2C — Over-Excitation Limiter Type 2C (fixed-current with time delay).
///
/// PSS/E params: `IFDMAX T_OEL VAMIN VRMAX K_OEL`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Oel2cParams {
    /// Maximum field current (pu).
    pub ifdmax: f64,
    /// Time delay before limiting (s).
    pub t_oel: f64,
    /// Minimum output (pu, negative).
    pub vamin: f64,
    /// Maximum regulator output (pu).
    pub vrmax: f64,
    /// Gain.
    pub k_oel: f64,
}

/// SCL1C — Stator Current Limiter Type 1C.
///
/// PSS/E params: `IRATED KR TR VCLMAX VCLMIN`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scl1cParams {
    /// Rated stator current (pu).
    pub irated: f64,
    /// Gain.
    pub kr: f64,
    /// Current filter time constant (s).
    pub tr: f64,
    /// Maximum clamp output (pu).
    pub vclmax: f64,
    /// Minimum clamp output (pu, negative).
    pub vclmin: f64,
}

/// UEL1 — Under-Excitation Limiter Type 1 (single-input integrator).
///
/// PSS/E params: `KUL TU1 VUCMAX VUCMIN KUR`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Uel1Params {
    /// UEL gain.
    pub kul: f64,
    /// UEL time constant (s).
    pub tu1: f64,
    /// Maximum UEL output (pu).
    pub vucmax: f64,
    /// Minimum UEL output (pu).
    pub vucmin: f64,
    /// Reactive power sensitivity.
    pub kur: f64,
}

/// UEL2C — Under-Excitation Limiter Type 2C (P-Q plane limiter).
///
/// PSS/E params: `KUL TU1 TU2 TU3 TU4 VUIMAX VUIMIN P0 Q0`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Uel2cParams {
    /// UEL gain.
    pub kul: f64,
    /// Filter time constant 1 (s).
    pub tu1: f64,
    /// Filter time constant 2 (s).
    pub tu2: f64,
    /// Filter time constant 3 (s).
    pub tu3: f64,
    /// Filter time constant 4 (s).
    pub tu4: f64,
    /// Maximum integrator output (pu).
    pub vuimax: f64,
    /// Minimum integrator output (pu).
    pub vuimin: f64,
    /// Reference active power (pu).
    pub p0: f64,
    /// Reference reactive power (pu).
    pub q0: f64,
}

/// OEL dynamic record — over-excitation limiter attached to a generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OelDyn {
    /// Bus number.
    pub bus: u32,
    /// Machine ID string (matches the generator record).
    pub machine_id: String,
    /// OEL model and parameters.
    pub model: OelModel,
}

/// UEL dynamic record — under-excitation limiter attached to a generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UelDyn {
    /// Bus number.
    pub bus: u32,
    /// Machine ID string (matches the generator record).
    pub machine_id: String,
    /// UEL model and parameters.
    pub model: UelModel,
}

/// Discriminated union of OEL models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OelModel {
    /// OEL1B — inverse-time ramp limiter.
    Oel1b(Oel1bParams),
    /// OEL2C — fixed current + time delay limiter.
    Oel2c(Oel2cParams),
    /// OEL3C — alias to OEL2C (C-series 2016 standard).
    Oel3c(Oel2cParams),
    /// OEL4C — alias to OEL2C (C-series 2016 standard).
    Oel4c(Oel2cParams),
    /// OEL5C — alias to OEL2C (C-series 2016 standard).
    Oel5c(Oel2cParams),
    /// SCL1C — stator current limiter (uses OEL slot).
    Scl1c(Scl1cParams),
}

/// Discriminated union of UEL models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UelModel {
    /// UEL1 — single-input integrator UEL.
    Uel1(Uel1Params),
    /// UEL2C — P-Q plane limiter (two-state filter).
    Uel2c(Uel2cParams),
}

/// Shaft dynamic model assignment — keyed by (bus, machine_id).
/// Follows the same pattern as ExciterDyn, GovernorDyn, PssDyn, OelDyn, UelDyn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShaftDyn {
    /// Bus number of the associated generator.
    pub bus: u32,
    /// Machine ID (matches the generator record).
    pub machine_id: String,
    /// N-mass torsional shaft model.
    pub model: crate::dynamics::shaft::ShaftModel,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_dm() -> DynamicModel {
        DynamicModel::default()
    }

    #[test]
    fn test_coverage_all_supported() {
        let mut dm = empty_dm();
        dm.generators.push(GeneratorDyn {
            bus: 1,
            machine_id: "1".into(),
            model: GeneratorModel::Gencls(GenclsParams { h: 3.0, d: 0.0 }),
        });
        let (n_sup, n_tot, pct) = dm.coverage();
        assert_eq!(n_sup, 1);
        assert_eq!(n_tot, 1);
        assert!((pct - 100.0).abs() < 1e-10);
    }

    #[test]
    fn test_coverage_with_unknown() {
        let mut dm = empty_dm();
        dm.generators.push(GeneratorDyn {
            bus: 1,
            machine_id: "1".into(),
            model: GeneratorModel::Gencls(GenclsParams { h: 3.0, d: 0.0 }),
        });
        dm.unknown_records.push(UnknownDyrRecord {
            bus: 2,
            model_name: "GENCC".into(),
            machine_id: "1".into(),
            params: vec![1.0, 2.0],
        });
        let (n_sup, n_tot, pct) = dm.coverage();
        assert_eq!(n_sup, 1);
        assert_eq!(n_tot, 2);
        assert!((pct - 50.0).abs() < 1e-10);
    }

    #[test]
    fn test_coverage_empty() {
        let dm = empty_dm();
        let (n_sup, n_tot, pct) = dm.coverage();
        assert_eq!(n_sup, 0);
        assert_eq!(n_tot, 0);
        assert!((pct - 100.0).abs() < 1e-10);
    }
}
