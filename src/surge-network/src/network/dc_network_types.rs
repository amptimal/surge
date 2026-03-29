// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC network types for multi-terminal DC (MTDC) grid modeling.
//!
//! These types represent the DC network topology: buses, converters, and branches.
//! Populated by any parser (MATPOWER, CGMES, PSS/E) and consumed by the AC-DC OPF solver.

use serde::{Deserialize, Serialize};

/// A DC bus in a multi-terminal DC network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DcBus {
    /// DC bus number.
    pub bus_id: u32,
    /// DC power demand at this bus in MW (Pdc).
    pub p_dc_mw: f64,
    /// DC voltage setpoint in per-unit (Vdc).
    pub v_dc_pu: f64,
    /// DC base voltage in kV.
    pub base_kv_dc: f64,
    /// Maximum DC voltage in per-unit.
    pub v_dc_max: f64,
    /// Minimum DC voltage in per-unit.
    pub v_dc_min: f64,
    /// DC bus cost coefficient (Cdc, for OPF).
    pub cost: f64,
    /// Total shunt conductance at this DC bus (siemens).
    ///
    /// From CGMES `DCShunt` objects: represents DC filter bank ESR losses.
    /// At DC steady-state, only the resistive component matters (ωC = 0).
    /// Shunt current: I_shunt = G_shunt × V_dc.  Loss = G_shunt × V_dc².
    #[serde(default)]
    pub g_shunt_siemens: f64,
    /// Ground return resistance at this DC bus (ohms).
    ///
    /// From CGMES `DCGround` or PSS/E `MtdcBus.rgrnd`.
    /// Models the earth electrode resistance for monopole HVDC or asymmetric
    /// bipole operation.  G_ground = 1/R_ground adds to the DC KCL equation
    /// as a path from this bus to V=0 (earth reference).
    /// 0.0 = no ground return path (symmetric bipole or metallic return).
    #[serde(default)]
    pub r_ground_ohm: f64,
}

/// A DC converter station connecting AC and DC networks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DcConverterStation {
    /// Stable converter identifier within the enclosing DC grid.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// DC bus number this converter connects to.
    pub dc_bus: u32,
    /// AC bus number this converter connects to.
    pub ac_bus: u32,
    /// DC-side control type: 1=P, 2=Vdc, 3=droop.
    pub control_type_dc: u32,
    /// AC-side control type: 1=PQ, 2=PV.
    pub control_type_ac: u32,
    /// Active power generation setpoint (MW).
    pub active_power_mw: f64,
    /// Reactive power generation setpoint (MVAr).
    pub reactive_power_mvar: f64,
    /// True if this is an LCC converter.
    pub is_lcc: bool,
    /// AC voltage target (pu).
    pub voltage_setpoint_pu: f64,
    /// Transformer resistance (pu).
    pub transformer_r_pu: f64,
    /// Transformer reactance (pu).
    pub transformer_x_pu: f64,
    /// True if converter has a transformer.
    pub transformer: bool,
    /// Transformer tap ratio.
    pub tap_ratio: f64,
    /// Filter susceptance (pu).
    pub filter_susceptance_pu: f64,
    /// True if converter has a filter.
    pub filter: bool,
    /// Reactor resistance (pu).
    pub reactor_r_pu: f64,
    /// Reactor reactance (pu).
    pub reactor_x_pu: f64,
    /// True if converter has a reactor.
    pub reactor: bool,
    /// Base AC voltage (kV).
    pub base_kv_ac: f64,
    /// Maximum AC voltage magnitude (pu).
    pub voltage_max_pu: f64,
    /// Minimum AC voltage magnitude (pu).
    pub voltage_min_pu: f64,
    /// Maximum current rating (pu).
    pub current_max_pu: f64,
    /// In-service status.
    pub status: bool,
    /// Constant loss coefficient (MW).
    pub loss_constant_mw: f64,
    /// Linear loss coefficient.
    pub loss_linear: f64,
    /// Quadratic loss coefficient for rectifier operation.
    pub loss_quadratic_rectifier: f64,
    /// Quadratic loss coefficient for inverter operation.
    pub loss_quadratic_inverter: f64,
    /// Droop coefficient (MW/pu).
    pub droop: f64,
    /// DC power setpoint (MW).
    pub power_dc_setpoint_mw: f64,
    /// DC voltage setpoint (pu).
    pub voltage_dc_setpoint_pu: f64,
    /// Maximum AC active power (MW).
    pub active_power_ac_max_mw: f64,
    /// Minimum AC active power (MW).
    pub active_power_ac_min_mw: f64,
    /// Maximum AC reactive power (MVAr).
    pub reactive_power_ac_max_mvar: f64,
    /// Minimum AC reactive power (MVAr).
    pub reactive_power_ac_min_mvar: f64,
}

/// A DC branch (cable/line) connecting two DC buses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DcBranch {
    /// Stable branch identifier within the enclosing DC grid.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// From DC bus number.
    pub from_bus: u32,
    /// To DC bus number.
    pub to_bus: u32,
    /// DC cable resistance (ohms).
    pub r_ohm: f64,
    /// DC cable inductance (mH).
    pub l_mh: f64,
    /// DC cable capacitance (uF).
    pub c_uf: f64,
    /// Rate A (MW).
    pub rating_a_mva: f64,
    /// Rate B (MW).
    pub rating_b_mva: f64,
    /// Rate C (MW).
    pub rating_c_mva: f64,
    /// In-service status.
    pub status: bool,
}
