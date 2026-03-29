// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical HVDC solution result types.
//!
//! All stable HVDC solvers (sequential, block-coupled, hybrid MTDC)
//! return [`HvdcSolution`] containing typed station and DC-bus results.

use serde::{Deserialize, Serialize};

/// LCC-specific operating point detail.
///
/// Populated for Line-Commutated Converter stations; `None` for VSC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvdcLccDetail {
    /// Firing angle α in degrees (rectifier mode).
    pub alpha_deg: f64,
    /// Extinction angle γ in degrees (inverter mode; 0.0 for rectifier).
    pub gamma_deg: f64,
    /// DC current in per-unit (magnitude).
    pub i_dc_pu: f64,
    /// Commutation power factor cos(φ).
    pub power_factor: f64,
}

/// HVDC converter technology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HvdcTechnology {
    Lcc,
    Vsc,
}

/// Per-converter operating point result.
///
/// Each `HvdcStationSolution` represents one converter station at one AC bus.
/// For point-to-point HVDC links, the solution contains two converters
/// (rectifier and inverter). For multi-terminal DC networks, there is one
/// converter per station.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvdcStationSolution {
    /// Optional asset or link name when available from the input model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Converter technology.
    pub technology: HvdcTechnology,
    /// AC bus number at this converter terminal.
    pub ac_bus: u32,
    /// DC bus number for explicit DC-network solves.
    ///
    /// `None` for point-to-point link solves that do not carry explicit DC bus identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dc_bus: Option<u32>,
    /// Active power at the AC terminal in MW.
    ///
    /// Sign convention: negative = drawn from AC (rectifier);
    /// positive = injected into AC (inverter).
    pub p_ac_mw: f64,
    /// Reactive power at the AC terminal in MVAR.
    ///
    /// LCC converters always absorb reactive power (q_ac_mvar ≤ 0).
    /// VSC converters can inject or absorb.
    pub q_ac_mvar: f64,
    /// Active power at the DC terminal in MW.
    ///
    /// Sign convention: positive = injected into the DC network;
    /// negative = drawn from the DC network.
    pub p_dc_mw: f64,
    /// DC bus voltage in per-unit. For point-to-point links without explicit
    /// DC modeling, this is 1.0.
    pub v_dc_pu: f64,
    /// Converter losses in MW (switching + conduction + transformer).
    pub converter_loss_mw: f64,
    /// LCC-specific operating detail (firing angle, DC current, power factor).
    /// `None` for VSC converters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lcc_detail: Option<HvdcLccDetail>,
    /// Whether this converter's iteration converged.
    pub converged: bool,
}

impl HvdcStationSolution {
    /// Per-station power balance error: `|P_ac + P_dc + P_loss|` in MW.
    ///
    /// Should be near zero for a converged solution.
    ///
    /// **Convention**: For point-to-point LCC links, DC line losses (I²R)
    /// are attributed entirely to the rectifier-side `converter_loss_mw`
    /// field (the inverter reports `converter_loss_mw = 0.0`). This means
    /// the rectifier station's balance includes the full DC line loss while
    /// the inverter station's balance reflects only its terminal power.
    pub fn power_balance_error_mw(&self) -> f64 {
        (self.p_ac_mw + self.p_dc_mw + self.converter_loss_mw).abs()
    }
}

/// Solved DC-bus voltage result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvdcDcBusSolution {
    /// External DC bus number.
    pub dc_bus: u32,
    /// DC voltage magnitude in per-unit.
    pub voltage_pu: f64,
}

/// Solution method used or requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HvdcMethod {
    /// Auto-detect: choose the best solver based on network topology.
    ///
    /// Routing logic:
    /// - Point-to-point links only (no explicit `dc_grids`) → Sequential
    /// - Explicit DC-grid topology with only VSC converters → BlockCoupled
    /// - Explicit DC-grid topology with LCC converters present → Hybrid
    ///
    #[default]
    Auto,
    /// Sequential AC-DC iteration: run AC PF, update converter models, repeat.
    Sequential,
    /// Block-coupled AC/DC outer iteration with optional sensitivity correction.
    BlockCoupled,
    /// Hybrid AC/DC solve for mixed LCC + VSC explicit DC topology.
    Hybrid,
}

/// Complete HVDC power flow solution.
///
/// Returned by all canonical HVDC solver paths via [`crate::solve_hvdc`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvdcSolution {
    /// Per-converter operating points.
    pub stations: Vec<HvdcStationSolution>,
    /// DC bus voltages for explicit DC-network solves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dc_buses: Vec<HvdcDcBusSolution>,
    /// Sum of converter losses in MW.
    pub total_converter_loss_mw: f64,
    /// Sum of explicit DC-network losses in MW.
    pub total_dc_network_loss_mw: f64,
    /// Sum of all HVDC losses in MW.
    pub total_loss_mw: f64,
    /// Number of outer/Newton iterations taken.
    pub iterations: u32,
    /// True if the solve converged.
    pub converged: bool,
    /// Solution method actually used.
    pub method: HvdcMethod,
}

#[cfg(test)]
mod tests {
    use super::{HvdcStationSolution, HvdcTechnology};

    #[test]
    fn power_balance_error_is_zero_for_rectifier_and_inverter_conventions() {
        let rectifier = HvdcStationSolution {
            name: Some("LCC-A".into()),
            technology: HvdcTechnology::Lcc,
            ac_bus: 1,
            dc_bus: Some(11),
            p_ac_mw: -102.0,
            q_ac_mvar: 0.0,
            p_dc_mw: 100.0,
            v_dc_pu: 1.0,
            converter_loss_mw: 2.0,
            lcc_detail: None,
            converged: true,
        };
        let inverter = HvdcStationSolution {
            name: Some("LCC-A".into()),
            technology: HvdcTechnology::Lcc,
            ac_bus: 2,
            dc_bus: Some(12),
            p_ac_mw: 98.0,
            q_ac_mvar: 0.0,
            p_dc_mw: -100.0,
            v_dc_pu: 1.0,
            converter_loss_mw: 2.0,
            lcc_detail: None,
            converged: true,
        };

        assert!(rectifier.power_balance_error_mw() < 1e-12);
        assert!(inverter.power_balance_error_mw() < 1e-12);
    }
}
