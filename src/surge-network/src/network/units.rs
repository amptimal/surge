// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Per-unit conversion utilities for power system quantities.

/// Base impedance in ohms: Z_base = kV² / MVA.
#[inline]
pub fn z_base_ohm(base_kv: f64, base_mva: f64) -> f64 {
    base_kv * base_kv / base_mva
}

/// Convert impedance from ohms to per-unit.
///
/// # Panics (debug only)
/// Debug-asserts that `ohm` is finite to catch NaN/Inf propagation early.
#[inline]
pub fn ohm_to_pu(ohm: f64, base_kv: f64, base_mva: f64) -> f64 {
    debug_assert!(ohm.is_finite(), "ohm_to_pu: ohm must be finite, got {ohm}");
    ohm / z_base_ohm(base_kv, base_mva)
}

/// Convert impedance from per-unit to ohms.
#[inline]
pub fn pu_to_ohm(pu: f64, base_kv: f64, base_mva: f64) -> f64 {
    pu * z_base_ohm(base_kv, base_mva)
}

/// Convert admittance from physical units (S) to per-unit.
///
/// Y_pu = Y_physical × Z_base = Y_physical × kV² / MVA.
#[inline]
pub fn y_to_pu(y: f64, base_kv: f64, base_mva: f64) -> f64 {
    y * z_base_ohm(base_kv, base_mva)
}

/// Convert admittance from per-unit to physical units (S).
///
/// Y_physical = Y_pu / Z_base = Y_pu × MVA / kV².
#[inline]
pub fn pu_to_y(pu: f64, base_kv: f64, base_mva: f64) -> f64 {
    pu / z_base_ohm(base_kv, base_mva)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_z_base() {
        // 138 kV, 100 MVA → Z_base = 138² / 100 = 190.44 Ω
        let z = z_base_ohm(138.0, 100.0);
        assert!((z - 190.44).abs() < 0.01);
    }

    #[test]
    fn test_ohm_pu_roundtrip() {
        let ohm_val = 19.044;
        let pu = ohm_to_pu(ohm_val, 138.0, 100.0);
        let back = pu_to_ohm(pu, 138.0, 100.0);
        assert!((back - ohm_val).abs() < 1e-10);
    }

    #[test]
    fn test_y_pu_roundtrip() {
        let y_phys = 0.00525;
        let pu = y_to_pu(y_phys, 138.0, 100.0);
        let back = pu_to_y(pu, 138.0, 100.0);
        assert!((back - y_phys).abs() < 1e-10);
    }
}
