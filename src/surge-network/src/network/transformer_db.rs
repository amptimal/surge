// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! IEEE C57.12 standard transformer impedance catalog.
//!
//! Provides standard nameplate impedances and X/R ratios for distribution and
//! substation transformers per:
//!
//! - **IEEE C57.12.00-2015** — Standard for liquid-immersed distribution, power,
//!   and regulating transformers (Tables 4, 6, 7)
//! - **IEEE C57.12.01-2015** — Standard for dry-type distribution transformers
//!   (Table 5)
//! - **IEEE C57.96** — Guide for loading mineral-oil-immersed power transformers
//!
//! # Standard Impedance Ranges
//!
//! Per IEEE C57.12.00, the standard percent impedance (Z%) for a given kVA rating
//! is defined as a tolerance band around a nominal value.  The values below
//! represent the nominal center of the tolerance band.
//!
//! ## Important
//! These are *standard design* values, not nameplate test values.  Actual
//! transformer impedances vary ±7.5% (distribution) to ±10% (power) from
//! these nominal values per IEEE C57.12.00 Section 5.4.

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Transformer construction type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransformerType {
    /// Liquid-immersed distribution transformer (≤500 kVA, ≤34.5 kV primary).
    LiquidDistribution,
    /// Liquid-immersed substation/power transformer (>500 kVA).
    LiquidPower,
    /// Dry-type distribution transformer (≤1500 kVA, ≤15 kV).
    DryType,
    /// Three-winding power transformer.
    ThreeWinding,
    /// Auto-transformer (boosted winding).
    AutoTransformer,
}

/// Full transformer specification from the IEEE C57.12 catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformerSpec {
    /// Transformer type.
    pub xfmr_type: TransformerType,
    /// Rated kVA (three-phase equivalent for single-phase banks, or three-phase rating).
    pub kva: f64,
    /// HV winding rated voltage (kV, line-to-line).
    pub hv_kv: f64,
    /// LV winding rated voltage (kV, line-to-line).
    pub lv_kv: f64,
    /// Nominal impedance (%) per IEEE C57.12 — center of tolerance band.
    pub z_pct: f64,
    /// X/R ratio at full load.  Higher for larger transformers.
    pub x_r_ratio: f64,
    /// No-load loss as % of rated kVA (core loss / excitation losses).
    pub no_load_loss_pct: f64,
    /// Full-load loss as % of rated kVA (no-load + winding I²R losses).
    pub full_load_loss_pct: f64,
}

impl TransformerSpec {
    /// Resistance in per-unit on transformer MVA base.
    ///
    /// R_pu = (P_fl_loss_W) / (S_rated_VA) = full_load_loss_pct / 100
    /// Note: R ≈ (Z% / 100) / sqrt(1 + (X/R)²) from the Z and X/R relationship.
    pub fn r_pu(&self) -> f64 {
        let z = self.z_pct / 100.0;
        let xr = self.x_r_ratio;
        // Z = R + jX, Z% = sqrt(R%² + X%²), X/R = xr → R = Z/sqrt(1+xr²)
        z / (1.0 + xr * xr).sqrt()
    }

    /// Reactance in per-unit on transformer MVA base.
    ///
    /// X_pu = R_pu × (X/R)
    pub fn x_pu(&self) -> f64 {
        self.r_pu() * self.x_r_ratio
    }

    /// Efficiency at full load, unity power factor (%).
    pub fn efficiency_full_load(&self) -> f64 {
        let output = 1.0; // pu
        let losses = self.full_load_loss_pct / 100.0;
        100.0 * output / (output + losses)
    }

    /// Efficiency at half load, 0.8 power factor (%).
    pub fn efficiency_half_load_08pf(&self) -> f64 {
        let pf = 0.8;
        let half_load = 0.5;
        // I²R losses scale as load², no-load loss is constant.
        let i2r_loss_pct =
            (self.full_load_loss_pct - self.no_load_loss_pct) * half_load * half_load;
        let total_loss_pct = self.no_load_loss_pct + i2r_loss_pct;
        let output = half_load * pf;
        100.0 * output / (output + total_loss_pct / 100.0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Static catalog table
// ─────────────────────────────────────────────────────────────────────────────

struct XfmrRecord {
    xfmr_type: TransformerType,
    kva: f64,
    hv_kv: f64,
    lv_kv: f64,
    z_pct: f64,
    x_r_ratio: f64,
    no_load_loss_pct: f64,
    full_load_loss_pct: f64,
}

impl XfmrRecord {
    #[allow(clippy::too_many_arguments)]
    const fn new(
        xfmr_type: TransformerType,
        kva: f64,
        hv_kv: f64,
        lv_kv: f64,
        z_pct: f64,
        x_r_ratio: f64,
        no_load_loss_pct: f64,
        full_load_loss_pct: f64,
    ) -> Self {
        Self {
            xfmr_type,
            kva,
            hv_kv,
            lv_kv,
            z_pct,
            x_r_ratio,
            no_load_loss_pct,
            full_load_loss_pct,
        }
    }
    fn to_spec(&self) -> TransformerSpec {
        TransformerSpec {
            xfmr_type: self.xfmr_type,
            kva: self.kva,
            hv_kv: self.hv_kv,
            lv_kv: self.lv_kv,
            z_pct: self.z_pct,
            x_r_ratio: self.x_r_ratio,
            no_load_loss_pct: self.no_load_loss_pct,
            full_load_loss_pct: self.full_load_loss_pct,
        }
    }
}

/// IEEE C57.12.00-2015 standard transformer catalog.
///
/// Sources:
/// - IEEE C57.12.00 Table 4 (distribution transformer impedances)
/// - IEEE C57.12.00 Table 6 (power transformer impedances)
/// - IEEE C57.12.01 Table 5 (dry-type transformer impedances)
/// - ANSI/IEEE C37.010 (typical X/R ratios for fault current)
static TRANSFORMER_CATALOG: &[XfmrRecord] = &[
    // ── Single-phase liquid distribution transformers ─────────────────────────
    // Z% per IEEE C57.12.00 Table 4; X/R per typical manufacturer data
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        25.0,
        12.47,
        0.240,
        2.5,
        1.5,
        0.39,
        1.68,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        37.5,
        12.47,
        0.240,
        2.5,
        1.7,
        0.33,
        1.45,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        50.0,
        12.47,
        0.240,
        2.5,
        2.0,
        0.28,
        1.28,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        75.0,
        12.47,
        0.240,
        2.5,
        2.2,
        0.24,
        1.09,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        100.0,
        12.47,
        0.240,
        2.5,
        2.5,
        0.20,
        0.95,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        167.0,
        12.47,
        0.240,
        2.5,
        3.0,
        0.16,
        0.80,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        250.0,
        12.47,
        0.240,
        2.5,
        3.5,
        0.13,
        0.68,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        333.0,
        12.47,
        0.240,
        2.5,
        4.0,
        0.11,
        0.59,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        500.0,
        12.47,
        0.240,
        2.5,
        4.5,
        0.09,
        0.50,
    ),
    // ── Three-phase liquid distribution transformers ──────────────────────────
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        75.0,
        12.47,
        0.480,
        3.5,
        2.0,
        0.27,
        1.50,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        150.0,
        12.47,
        0.480,
        3.5,
        2.5,
        0.22,
        1.21,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        225.0,
        12.47,
        0.480,
        3.5,
        3.0,
        0.19,
        1.07,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        300.0,
        12.47,
        0.480,
        3.5,
        3.5,
        0.17,
        0.96,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        500.0,
        12.47,
        0.480,
        3.5,
        4.0,
        0.14,
        0.83,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        750.0,
        12.47,
        0.480,
        4.0,
        4.5,
        0.12,
        0.72,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        1000.0,
        12.47,
        0.480,
        4.5,
        5.0,
        0.10,
        0.62,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        1500.0,
        12.47,
        0.480,
        5.0,
        5.5,
        0.09,
        0.55,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        2000.0,
        12.47,
        0.480,
        5.5,
        6.0,
        0.08,
        0.49,
    ),
    XfmrRecord::new(
        TransformerType::LiquidDistribution,
        2500.0,
        12.47,
        0.480,
        5.5,
        6.5,
        0.07,
        0.44,
    ),
    // ── Dry-type distribution transformers (IEEE C57.12.01) ──────────────────
    XfmrRecord::new(
        TransformerType::DryType,
        15.0,
        0.480,
        0.208,
        4.5,
        2.0,
        0.52,
        3.00,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        30.0,
        0.480,
        0.208,
        4.5,
        2.0,
        0.45,
        2.50,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        45.0,
        0.480,
        0.208,
        4.5,
        2.5,
        0.40,
        2.20,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        75.0,
        0.480,
        0.208,
        4.5,
        3.0,
        0.35,
        1.90,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        112.5,
        0.480,
        0.208,
        4.5,
        3.5,
        0.30,
        1.70,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        150.0,
        0.480,
        0.208,
        4.5,
        4.0,
        0.27,
        1.55,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        225.0,
        0.480,
        0.208,
        4.5,
        5.0,
        0.24,
        1.40,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        300.0,
        0.480,
        0.208,
        4.5,
        5.5,
        0.21,
        1.28,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        500.0,
        0.480,
        0.208,
        4.5,
        6.0,
        0.18,
        1.10,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        750.0,
        0.480,
        0.208,
        5.75,
        7.0,
        0.15,
        0.96,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        1000.0,
        0.480,
        0.208,
        5.75,
        7.5,
        0.13,
        0.86,
    ),
    XfmrRecord::new(
        TransformerType::DryType,
        1500.0,
        0.480,
        0.208,
        5.75,
        8.0,
        0.11,
        0.75,
    ),
    // ── Substation / power transformers (IEEE C57.12.00 Table 6) ─────────────
    // Z% for power transformers: 5.5–8% depending on HV voltage class
    XfmrRecord::new(
        TransformerType::LiquidPower,
        5000.0,
        34.5,
        4.160,
        5.5,
        8.0,
        0.05,
        0.34,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        7500.0,
        34.5,
        4.160,
        5.5,
        9.0,
        0.04,
        0.29,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        10000.0,
        69.0,
        13.8,
        6.0,
        10.0,
        0.04,
        0.27,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        15000.0,
        69.0,
        13.8,
        6.5,
        11.0,
        0.03,
        0.23,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        20000.0,
        115.0,
        13.8,
        7.0,
        12.0,
        0.03,
        0.20,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        25000.0,
        115.0,
        13.8,
        7.0,
        14.0,
        0.02,
        0.17,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        30000.0,
        138.0,
        13.8,
        7.5,
        16.0,
        0.02,
        0.15,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        40000.0,
        138.0,
        13.8,
        8.0,
        18.0,
        0.02,
        0.14,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        50000.0,
        230.0,
        13.8,
        8.0,
        20.0,
        0.01,
        0.12,
    ),
    XfmrRecord::new(
        TransformerType::LiquidPower,
        100000.0,
        345.0,
        138.0,
        8.5,
        25.0,
        0.01,
        0.10,
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// Standard impedance functions (without full catalog lookup)
// ─────────────────────────────────────────────────────────────────────────────

/// Return the standard IEEE C57.12 impedance (%) for a transformer of the given
/// type and kVA rating, without requiring a voltage match.
///
/// This implements the impedance values from IEEE C57.12.00-2015 Table 4
/// (distribution) and Table 6 (power).
pub fn standard_impedance_pct(xfmr_type: TransformerType, kva: f64) -> f64 {
    match xfmr_type {
        TransformerType::LiquidDistribution => {
            if kva <= 500.0 {
                2.5
            } else if kva <= 2500.0 {
                5.5
            } else {
                6.0
            }
        }
        TransformerType::DryType => {
            if kva <= 500.0 {
                4.5
            } else {
                5.75
            }
        }
        TransformerType::LiquidPower => {
            if kva <= 15_000.0 {
                6.0
            } else if kva <= 50_000.0 {
                7.5
            } else {
                8.0
            }
        }
        TransformerType::ThreeWinding => 8.0,
        TransformerType::AutoTransformer => {
            // Autotransformer: effective impedance reduced by turns ratio factor
            6.0
        }
    }
}

/// Standard X/R ratio for fault current calculations.
///
/// Per IEEE C37.010, Table 1 and engineering practice.
pub fn standard_x_r_ratio(xfmr_type: TransformerType, kva: f64) -> f64 {
    match xfmr_type {
        TransformerType::LiquidDistribution => {
            if kva <= 100.0 {
                2.0
            } else if kva <= 500.0 {
                3.5
            } else if kva <= 1500.0 {
                5.5
            } else {
                7.0
            }
        }
        TransformerType::DryType => {
            if kva <= 150.0 {
                3.0
            } else if kva <= 500.0 {
                5.0
            } else {
                8.0
            }
        }
        TransformerType::LiquidPower => {
            if kva <= 10_000.0 {
                10.0
            } else if kva <= 30_000.0 {
                15.0
            } else {
                20.0
            }
        }
        TransformerType::ThreeWinding | TransformerType::AutoTransformer => 15.0,
    }
}

/// Return the standard impedance in per-unit on the transformer's own MVA base.
pub fn standard_impedance_pu(xfmr_type: TransformerType, kva: f64) -> f64 {
    standard_impedance_pct(xfmr_type, kva) / 100.0
}

/// Return the next standard kVA rating at or above the given value.
///
/// Standard ratings per ANSI/IEEE C57.12.
pub fn next_standard_kva(kva: f64) -> f64 {
    // Standard distribution ratings (1-phase and 3-phase)
    const STANDARD_KVAS: &[f64] = &[
        10.0, 15.0, 25.0, 37.5, 50.0, 75.0, 100.0, 112.5, 150.0, 167.0, 225.0, 250.0, 333.0, 300.0,
        500.0, 750.0, 1000.0, 1500.0, 2000.0, 2500.0, 3000.0, 3750.0, 5000.0, 7500.0, 10000.0,
        12500.0, 15000.0, 20000.0, 25000.0, 30000.0, 40000.0, 50000.0, 60000.0, 75000.0, 100000.0,
    ];
    STANDARD_KVAS
        .iter()
        .copied()
        .find(|&r| r >= kva)
        .unwrap_or(kva * 1.25)
}

// ─────────────────────────────────────────────────────────────────────────────
// Catalog lookup
// ─────────────────────────────────────────────────────────────────────────────

/// Look up a transformer from the IEEE C57.12 catalog by type, kVA rating,
/// and primary voltage.
///
/// Finds the entry with the closest kVA to the requested value among matching
/// type entries.  The secondary voltage is not matched — use this for impedance
/// selection only.
///
/// # Returns
/// `Some(TransformerSpec)` if a matching entry exists, `None` if the catalog
/// contains no entries for this type.
pub fn lookup_transformer(
    xfmr_type: TransformerType,
    kva: f64,
    _hv_kv: f64,
) -> Option<TransformerSpec> {
    TRANSFORMER_CATALOG
        .iter()
        .filter(|r| r.xfmr_type == xfmr_type)
        .min_by(|a, b| {
            (a.kva - kva)
                .abs()
                .partial_cmp(&(b.kva - kva).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|r| r.to_spec())
}

/// List all catalog entries for a given transformer type.
pub fn list_transformers_by_type(xfmr_type: TransformerType) -> Vec<TransformerSpec> {
    TRANSFORMER_CATALOG
        .iter()
        .filter(|r| r.xfmr_type == xfmr_type)
        .map(|r| r.to_spec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distribution_impedance() {
        // Per IEEE C57.12.00: distribution transformers ≤500kVA → 2.5%
        let spec = lookup_transformer(TransformerType::LiquidDistribution, 500.0, 12.47)
            .expect("500 kVA dist xfmr should be in catalog");
        assert!((spec.z_pct - 2.5).abs() < 0.1);
    }

    #[test]
    fn test_pu_impedance_relationship() {
        let spec = lookup_transformer(TransformerType::LiquidDistribution, 500.0, 12.47)
            .expect("should exist");
        // Z_pu = sqrt(R_pu² + X_pu²) should equal z_pct/100
        let z_pu_check = (spec.r_pu().powi(2) + spec.x_pu().powi(2)).sqrt();
        assert!((z_pu_check - spec.z_pct / 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_dry_type_lookup() {
        let spec = lookup_transformer(TransformerType::DryType, 150.0, 0.48)
            .expect("150 kVA dry-type should be in catalog");
        assert!(
            spec.z_pct >= 4.0 && spec.z_pct <= 6.0,
            "Dry-type Z%={}",
            spec.z_pct
        );
        assert!(spec.x_r_ratio > 1.0);
    }

    #[test]
    fn test_power_transformer_higher_xr() {
        let dist = lookup_transformer(TransformerType::LiquidDistribution, 500.0, 12.47).unwrap();
        let power = lookup_transformer(TransformerType::LiquidPower, 10000.0, 69.0).unwrap();
        assert!(
            power.x_r_ratio > dist.x_r_ratio,
            "Power xfmr X/R ({}) should exceed distribution X/R ({})",
            power.x_r_ratio,
            dist.x_r_ratio
        );
    }

    #[test]
    fn test_standard_impedance_pct() {
        assert!(
            (standard_impedance_pct(TransformerType::LiquidDistribution, 100.0) - 2.5).abs() < 0.01
        );
        assert!((standard_impedance_pct(TransformerType::DryType, 300.0) - 4.5).abs() < 0.01);
    }

    #[test]
    fn test_next_standard_kva() {
        assert!((next_standard_kva(90.0) - 100.0).abs() < 0.01);
        assert!((next_standard_kva(100.0) - 100.0).abs() < 0.01);
        assert!((next_standard_kva(501.0) - 750.0).abs() < 0.01);
    }

    #[test]
    fn test_efficiency_full_load() {
        let spec = lookup_transformer(TransformerType::LiquidDistribution, 500.0, 12.47).unwrap();
        let eff = spec.efficiency_full_load();
        // Distribution transformers typically 98–99.5% efficient
        assert!(
            eff > 98.0 && eff < 100.0,
            "Efficiency should be 98–100%, got {eff:.2}%"
        );
    }
}
