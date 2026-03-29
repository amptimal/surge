// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E Induction Machine data (Issue #28).

use serde::{Deserialize, Serialize};

/// PSS/E induction machine (motor load) steady-state equivalent-circuit record.
///
/// Parsed from "INDUCTION MACHINE DATA" in PSS/E RAW v35+ files.
/// Uses the standard T-circuit model: Ra+jXa (stator), jXm (magnetizing),
/// R1+jX1 (rotor cage 1), R2+jX2 (rotor cage 2, double-cage only).
///
/// Source: PSS/E v35 Program Operation Manual §5.12.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InductionMachine {
    /// Terminal bus number.
    pub bus: u32,
    /// Machine identifier (up to 2 characters).
    pub id: String,
    /// In-service status.
    pub in_service: bool,
    /// Machine base MVA (0.0 → use system base).
    pub mbase: f64,
    /// Rated terminal kV.
    pub rate_kv: f64,
    /// Active-power set-point (MW when pcode=1, power factor when pcode=2).
    pub pset: f64,
    /// Inertia constant H (MW·s/MVA).
    pub h: f64,
    /// Speed-torque coefficient A (T = A + B*w + D*w^2 + E*w^3).
    pub a: f64,
    /// Speed-torque coefficient B.
    pub b: f64,
    /// Speed-torque coefficient D.
    pub d: f64,
    /// Speed-torque coefficient E.
    pub e: f64,
    /// Locked-rotor torque multiplier F.
    pub f_coeff: f64,
    /// Stator resistance Ra (pu, machine base).
    pub ra: f64,
    /// Stator leakage reactance Xa (pu, machine base).
    pub xa: f64,
    /// Magnetizing reactance Xm (pu, machine base).
    pub xm: f64,
    /// Rotor resistance — first cage R1 (pu, machine base).
    pub r1: f64,
    /// Rotor reactance — first cage X1 (pu, machine base).
    pub x1: f64,
    /// Rotor resistance — second cage R2 (pu; 0 = single-cage).
    pub r2: f64,
    /// Rotor reactance — second cage X2 (pu; 0 = single-cage).
    pub x2: f64,
    /// Leakage reactance X3 (pu, machine base).
    pub x3: f64,
    /// PSS/E area number.
    pub area: u32,
    /// PSS/E zone number.
    pub zone: u32,
    /// PSS/E owner number.
    pub owner: u32,
    /// References Load.id on the same bus. None = bus-level (legacy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_id: Option<String>,
}

impl InductionMachine {
    /// Effective machine base MVA — falls back to `system_base` when mbase == 0.
    pub fn effective_mbase(&self, system_base: f64) -> f64 {
        if self.mbase.abs() < 1e-10 {
            system_base
        } else {
            self.mbase
        }
    }
}
