// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Typed serde structs for the GO Competition Challenge 3 JSON format.
//!
//! These structs faithfully mirror the official GO C3 data model so that
//! `serde_json::from_reader::<GoC3Problem>(…)` replaces ad-hoc dict-walking.
//! Field names match the GO C3 JSON keys exactly (snake_case).

use serde::{Deserialize, Serialize};

// ─── Top-level document ──────────────────────────────────────────────────────

/// A complete GO C3 problem file (scenario JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Problem {
    pub network: GoC3Network,
    pub time_series_input: GoC3TimeSeriesInput,
    pub reliability: GoC3Reliability,
}

// ─── Network section ─────────────────────────────────────────────────────────

/// `network` top-level object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Network {
    pub general: GoC3General,
    #[serde(default)]
    pub bus: Vec<GoC3Bus>,
    #[serde(default)]
    pub simple_dispatchable_device: Vec<GoC3Device>,
    #[serde(default)]
    pub ac_line: Vec<GoC3AcLine>,
    #[serde(default)]
    pub two_winding_transformer: Vec<GoC3Transformer>,
    #[serde(default)]
    pub dc_line: Vec<GoC3DcLine>,
    #[serde(default)]
    pub shunt: Vec<GoC3Shunt>,
    #[serde(default)]
    pub active_zonal_reserve: Vec<GoC3ActiveZonalReserve>,
    #[serde(default)]
    pub reactive_zonal_reserve: Vec<GoC3ReactiveZonalReserve>,
    #[serde(default)]
    pub violation_cost: Option<GoC3ViolationCost>,
}

/// `network.general`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3General {
    pub base_norm_mva: f64,
}

// ─── Bus ─────────────────────────────────────────────────────────────────────

/// `network.bus[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Bus {
    pub uid: String,
    #[serde(default)]
    pub base_nom_volt: f64,
    #[serde(default)]
    pub con_loss_factor: f64,
    #[serde(default)]
    pub vm_lb: f64,
    #[serde(default = "default_vm_ub")]
    pub vm_ub: f64,
    #[serde(default)]
    pub initial_status: GoC3BusInitialStatus,
    /// Bus type string from GO C3: `"Slack"`, `"PV"`, `"PQ"`, `"Notused"`, or absent.
    #[serde(default, rename = "type")]
    pub bus_type: Option<String>,
    #[serde(default)]
    pub active_reserve_uids: Vec<String>,
    #[serde(default)]
    pub reactive_reserve_uids: Vec<String>,
}

fn default_vm_ub() -> f64 {
    1.1
}

/// `network.bus[*].initial_status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3BusInitialStatus {
    #[serde(default = "default_one")]
    pub vm: f64,
    #[serde(default)]
    pub va: f64,
}

fn default_one() -> f64 {
    1.0
}

// ─── Simple Dispatchable Device ──────────────────────────────────────────────

/// `network.simple_dispatchable_device[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Device {
    pub uid: String,
    pub bus: String,
    pub device_type: GoC3DeviceType,
    #[serde(default)]
    pub initial_status: GoC3DeviceInitialStatus,

    // ── Commitment costs ──
    #[serde(default)]
    pub on_cost: f64,
    #[serde(default)]
    pub startup_cost: f64,
    #[serde(default)]
    pub shutdown_cost: f64,

    // ── Startup delay tiers: [[cost_adjustment, max_down_hours], …] ──
    #[serde(default)]
    pub startup_states: Vec<Vec<f64>>,

    // ── Startup limits: [[start_period, end_period, max_startups]] ──
    #[serde(default)]
    pub startups_ub: Vec<Vec<f64>>,

    // ── Min up/down time ──
    #[serde(default)]
    pub down_time_lb: f64,
    #[serde(default)]
    pub in_service_time_lb: f64,

    // ── Energy window limits: [[start_period, end_period, limit_mwh]] ──
    #[serde(default)]
    pub energy_req_lb: Vec<Vec<f64>>,
    #[serde(default)]
    pub energy_req_ub: Vec<Vec<f64>>,

    // ── Ramp rates (per-unit on base_norm_mva) ──
    #[serde(default)]
    pub p_ramp_up_ub: f64,
    #[serde(default)]
    pub p_ramp_down_ub: f64,
    #[serde(default)]
    pub p_startup_ramp_ub: f64,
    #[serde(default)]
    pub p_shutdown_ramp_ub: f64,

    // ── Reserve capabilities (per-unit) ──
    #[serde(default)]
    pub p_reg_res_up_ub: f64,
    #[serde(default)]
    pub p_reg_res_down_ub: f64,
    #[serde(default)]
    pub p_syn_res_ub: f64,
    #[serde(default)]
    pub p_nsyn_res_ub: f64,
    #[serde(default)]
    pub p_ramp_res_up_online_ub: f64,
    #[serde(default)]
    pub p_ramp_res_down_online_ub: f64,
    #[serde(default)]
    pub p_ramp_res_up_offline_ub: f64,
    #[serde(default)]
    pub p_ramp_res_down_offline_ub: f64,

    // ── Reactive capability bounds ──
    #[serde(default)]
    pub q_bound_cap: f64,
    #[serde(default)]
    pub q_linear_cap: f64,

    /// Voltage magnitude setpoint (pu). Optional.
    #[serde(default)]
    pub vm_setpoint: Option<f64>,
}

/// Device type: `"producer"` or `"consumer"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoC3DeviceType {
    Producer,
    Consumer,
}

/// `network.simple_dispatchable_device[*].initial_status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3DeviceInitialStatus {
    #[serde(default)]
    pub on_status: i32,
    #[serde(default)]
    pub p: f64,
    #[serde(default)]
    pub q: f64,
    #[serde(default)]
    pub accu_up_time: f64,
    #[serde(default)]
    pub accu_down_time: f64,
}

// ─── AC Line ─────────────────────────────────────────────────────────────────

/// `network.ac_line[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3AcLine {
    pub uid: String,
    pub fr_bus: String,
    pub to_bus: String,
    #[serde(default)]
    pub r: f64,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub b: f64,
    #[serde(default)]
    pub g: f64,
    #[serde(default)]
    pub mva_ub_nom: f64,
    #[serde(default)]
    pub mva_ub_sht: Option<f64>,
    #[serde(default)]
    pub mva_ub_em: f64,
    #[serde(default)]
    pub initial_status: GoC3BranchInitialStatus,
    #[serde(default)]
    pub connection_cost: f64,
    #[serde(default)]
    pub disconnection_cost: f64,
    /// Flag (0 or 1): when 1, per-side shunt equations (GO §4.8) apply.
    #[serde(default)]
    pub additional_shunt: i32,
    // Per-side shunt values (used when additional_shunt == 1).
    #[serde(default)]
    pub g_fr: f64,
    #[serde(default)]
    pub b_fr: f64,
    #[serde(default)]
    pub g_to: f64,
    #[serde(default)]
    pub b_to: f64,
}

/// `network.two_winding_transformer[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Transformer {
    pub uid: String,
    pub fr_bus: String,
    pub to_bus: String,
    #[serde(default)]
    pub r: f64,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub b: f64,
    #[serde(default)]
    pub g: f64,
    #[serde(default)]
    pub mva_ub_nom: f64,
    #[serde(default)]
    pub mva_ub_sht: Option<f64>,
    #[serde(default)]
    pub mva_ub_em: f64,
    #[serde(default)]
    pub initial_status: GoC3TransformerInitialStatus,
    #[serde(default)]
    pub tm_lb: Option<f64>,
    #[serde(default)]
    pub tm_ub: Option<f64>,
    #[serde(default)]
    pub ta_lb: Option<f64>,
    #[serde(default)]
    pub ta_ub: Option<f64>,
    #[serde(default)]
    pub connection_cost: f64,
    #[serde(default)]
    pub disconnection_cost: f64,
    #[serde(default)]
    pub additional_shunt: i32,
    #[serde(default)]
    pub g_fr: f64,
    #[serde(default)]
    pub b_fr: f64,
    #[serde(default)]
    pub g_to: f64,
    #[serde(default)]
    pub b_to: f64,
}

/// `initial_status` for AC line (on_status only).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3BranchInitialStatus {
    #[serde(default = "default_on")]
    pub on_status: i32,
}

fn default_on() -> i32 {
    1
}

/// `initial_status` for transformer (on_status + tap/phase).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3TransformerInitialStatus {
    #[serde(default = "default_on")]
    pub on_status: i32,
    #[serde(default = "default_one")]
    pub tm: f64,
    #[serde(default)]
    pub ta: f64,
}

impl Default for GoC3TransformerInitialStatus {
    fn default() -> Self {
        Self {
            on_status: 1,
            tm: 1.0,
            ta: 0.0,
        }
    }
}

// ─── DC Line ─────────────────────────────────────────────────────────────────

/// `network.dc_line[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3DcLine {
    pub uid: String,
    pub fr_bus: String,
    pub to_bus: String,
    #[serde(default)]
    pub pdc_ub: f64,
    #[serde(default)]
    pub initial_status: GoC3DcLineInitialStatus,
    #[serde(default)]
    pub qdc_fr_lb: Option<f64>,
    #[serde(default)]
    pub qdc_fr_ub: Option<f64>,
    #[serde(default)]
    pub qdc_to_lb: Option<f64>,
    #[serde(default)]
    pub qdc_to_ub: Option<f64>,
}

/// `network.dc_line[*].initial_status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3DcLineInitialStatus {
    #[serde(default)]
    pub pdc_fr: f64,
    #[serde(default)]
    pub qdc_fr: f64,
    #[serde(default)]
    pub qdc_to: f64,
}

// ─── Shunt ───────────────────────────────────────────────────────────────────

/// `network.shunt[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Shunt {
    pub uid: String,
    pub bus: String,
    #[serde(default)]
    pub gs: f64,
    #[serde(default)]
    pub bs: f64,
    #[serde(default)]
    pub initial_status: GoC3ShuntInitialStatus,
    #[serde(default)]
    pub step_lb: Option<f64>,
    #[serde(default)]
    pub step_ub: Option<f64>,
}

/// `network.shunt[*].initial_status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3ShuntInitialStatus {
    #[serde(default)]
    pub step: i32,
}

// ─── Reserve zones ───────────────────────────────────────────────────────────

/// `network.active_zonal_reserve[*]`.
///
/// Reserve product fields use uppercase keys to match the GO C3 JSON exactly
/// (e.g. `"SYN"`, `"REG_UP"`, `"NSYN"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GoC3ActiveZonalReserve {
    pub uid: String,
    #[serde(default)]
    pub SYN: f64,
    #[serde(default)]
    pub SYN_vio_cost: f64,
    #[serde(default)]
    pub NSYN: f64,
    #[serde(default)]
    pub NSYN_vio_cost: f64,
    #[serde(default)]
    pub REG_UP: f64,
    #[serde(default)]
    pub REG_UP_vio_cost: f64,
    #[serde(default)]
    pub REG_DOWN: f64,
    #[serde(default)]
    pub REG_DOWN_vio_cost: f64,
    #[serde(default)]
    pub RAMPING_RESERVE_UP_vio_cost: f64,
    #[serde(default)]
    pub RAMPING_RESERVE_DOWN_vio_cost: f64,
}

/// `network.reactive_zonal_reserve[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GoC3ReactiveZonalReserve {
    pub uid: String,
    #[serde(default)]
    pub REACT_UP_vio_cost: f64,
    #[serde(default)]
    pub REACT_DOWN_vio_cost: f64,
}

/// `network.violation_cost`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3ViolationCost {
    #[serde(default)]
    pub e_vio_cost: f64,
    #[serde(default)]
    pub p_bus_vio_cost: f64,
    #[serde(default)]
    pub q_bus_vio_cost: f64,
    #[serde(default)]
    pub s_vio_cost: f64,
}

// ─── Time series input ───────────────────────────────────────────────────────

/// `time_series_input` top-level object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3TimeSeriesInput {
    pub general: GoC3TimeSeriesGeneral,
    #[serde(default)]
    pub simple_dispatchable_device: Vec<GoC3DeviceTimeSeries>,
    #[serde(default)]
    pub active_zonal_reserve: Vec<GoC3ActiveReserveTimeSeries>,
    #[serde(default)]
    pub reactive_zonal_reserve: Vec<GoC3ReactiveReserveTimeSeries>,
}

/// `time_series_input.general`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3TimeSeriesGeneral {
    pub time_periods: usize,
    pub interval_duration: Vec<f64>,
}

/// `time_series_input.simple_dispatchable_device[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3DeviceTimeSeries {
    pub uid: String,
    #[serde(default)]
    pub p_lb: Vec<f64>,
    #[serde(default)]
    pub p_ub: Vec<f64>,
    #[serde(default)]
    pub q_lb: Vec<f64>,
    #[serde(default)]
    pub q_ub: Vec<f64>,
    #[serde(default)]
    pub on_status_lb: Vec<f64>,
    #[serde(default)]
    pub on_status_ub: Vec<f64>,
    /// Piecewise-linear cost blocks per period.
    /// `cost[period]` is an array of `[mw_limit_pu, cost_per_mwh]` pairs.
    #[serde(default)]
    pub cost: Vec<Vec<Vec<f64>>>,

    // ── Reserve costs per period ──
    #[serde(default)]
    pub p_syn_res_cost: Vec<f64>,
    #[serde(default)]
    pub p_nsyn_res_cost: Vec<f64>,
    #[serde(default)]
    pub p_reg_res_up_cost: Vec<f64>,
    #[serde(default)]
    pub p_reg_res_down_cost: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_up_online_cost: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_up_offline_cost: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_down_online_cost: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_down_offline_cost: Vec<f64>,
    #[serde(default)]
    pub q_res_up_cost: Vec<f64>,
    #[serde(default)]
    pub q_res_down_cost: Vec<f64>,
}

/// `time_series_input.active_zonal_reserve[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GoC3ActiveReserveTimeSeries {
    pub uid: String,
    #[serde(default)]
    pub SYN: Vec<f64>,
    #[serde(default)]
    pub NSYN: Vec<f64>,
    #[serde(default)]
    pub REG_UP: Vec<f64>,
    #[serde(default)]
    pub REG_DOWN: Vec<f64>,
    #[serde(default)]
    pub RAMPING_RESERVE_UP: Vec<f64>,
    #[serde(default)]
    pub RAMPING_RESERVE_DOWN: Vec<f64>,
}

/// `time_series_input.reactive_zonal_reserve[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GoC3ReactiveReserveTimeSeries {
    pub uid: String,
    #[serde(default)]
    pub REACT_UP: Vec<f64>,
    #[serde(default)]
    pub REACT_DOWN: Vec<f64>,
}

// ─── Reliability ─────────────────────────────────────────────────────────────

/// `reliability` top-level object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Reliability {
    #[serde(default)]
    pub contingency: Vec<GoC3Contingency>,
}

/// `reliability.contingency[*]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Contingency {
    pub uid: String,
    #[serde(default)]
    pub components: Vec<String>,
}

// ─── Solution output ─────────────────────────────────────────────────────────

/// The full GO C3 solution document (`time_series_output`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3Solution {
    pub time_series_output: GoC3TimeSeriesOutput,
}

/// `time_series_output` containing all per-element results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3TimeSeriesOutput {
    #[serde(default)]
    pub bus: Vec<GoC3BusSolution>,
    #[serde(default)]
    pub simple_dispatchable_device: Vec<GoC3DeviceSolution>,
    #[serde(default)]
    pub ac_line: Vec<GoC3AcLineSolution>,
    #[serde(default)]
    pub two_winding_transformer: Vec<GoC3TransformerSolution>,
    #[serde(default)]
    pub dc_line: Vec<GoC3DcLineSolution>,
    #[serde(default)]
    pub shunt: Vec<GoC3ShuntSolution>,
}

/// Per-bus time-series solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3BusSolution {
    pub uid: String,
    pub vm: Vec<f64>,
    pub va: Vec<f64>,
}

/// Per-device time-series solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3DeviceSolution {
    pub uid: String,
    pub on_status: Vec<i32>,
    pub p_on: Vec<f64>,
    pub q: Vec<f64>,
    // ── Reserve awards ──
    #[serde(default)]
    pub p_reg_res_up: Vec<f64>,
    #[serde(default)]
    pub p_reg_res_down: Vec<f64>,
    #[serde(default)]
    pub p_syn_res: Vec<f64>,
    #[serde(default)]
    pub p_nsyn_res: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_up_online: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_down_online: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_up_offline: Vec<f64>,
    #[serde(default)]
    pub p_ramp_res_down_offline: Vec<f64>,
    #[serde(default)]
    pub q_res_up: Vec<f64>,
    #[serde(default)]
    pub q_res_down: Vec<f64>,
}

/// Per-AC-line time-series solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3AcLineSolution {
    pub uid: String,
    pub on_status: Vec<i32>,
}

/// Per-transformer time-series solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3TransformerSolution {
    pub uid: String,
    pub on_status: Vec<i32>,
    #[serde(default)]
    pub tm: Vec<f64>,
    #[serde(default)]
    pub ta: Vec<f64>,
}

/// Per-DC-line time-series solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3DcLineSolution {
    pub uid: String,
    pub pdc_fr: Vec<f64>,
    pub qdc_fr: Vec<f64>,
    pub qdc_to: Vec<f64>,
}

/// Per-shunt time-series solution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoC3ShuntSolution {
    pub uid: String,
    pub step: Vec<i32>,
}
