// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Generic reserve product model.
//!
//! All ancillary service / reserve products are structurally identical:
//! a directional capacity requirement deliverable within a time window,
//! with qualification filters, a demand/penalty curve for shortage pricing,
//! and energy coupling rules.
//!
//! ERCOT's 6 products, PJM's synchronized/non-synchronized, CAISO's products —
//! all are different configurations of [`ReserveProduct`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::market::PenaltyCurve;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Direction of capacity held in reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReserveDirection {
    Up,
    Down,
}

/// Qualification rule — which resources can provide this product.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum QualificationRule {
    /// Any committed (online) unit.
    Committed,
    /// Must be synchronized to grid (online + breaker closed).
    Synchronized,
    /// Quick-start offline units (can reach output within deployment window).
    QuickStart,
    /// Frequency-responsive (governor droop active, synchronous machines).
    FrequencyResponsive,
    /// Custom: unit must have this flag name set true in its qualification map.
    Custom(String),
}

/// Energy coupling mode — how this product interacts with energy dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnergyCoupling {
    /// R\[g\] + Pg <= Pmax (reserve eats into headroom). Used by spin, reg-up, ECRS.
    Headroom,
    /// R\[g\] <= Pg - Pmin (reserve eats into downward room). Used by reg-down.
    Footroom,
    /// No energy coupling — just a capacity bound. Used by non-spin (offline units).
    None,
}

// ---------------------------------------------------------------------------
// ReserveProduct — the core definition
// ---------------------------------------------------------------------------

/// Definition of a reserve product.
///
/// All reserve products are structurally identical: a directional capacity
/// requirement deliverable within a time window, with qualification filters,
/// a demand/penalty curve for shortage pricing, and energy coupling rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveProduct {
    /// Unique identifier (e.g. "spin", "reg_up", "ecrs", "pjm_sync").
    pub id: String,
    /// Human-readable name (e.g. "Spinning Reserve", "Regulation Up").
    pub name: String,
    /// Up or Down.
    pub direction: ReserveDirection,
    /// Deployment window in seconds. Reserve must be deliverable within this time.
    /// Used for ramp-reserve coupling: R\[g\] <= ramp_rate\[g\] * (deploy_secs / 60).
    pub deploy_secs: f64,
    /// Who can provide this product.
    pub qualification: QualificationRule,
    /// How this product couples with the energy dispatch variable.
    pub energy_coupling: EnergyCoupling,
    /// Demand/penalty curve for shortage pricing.
    /// A single-segment linear curve is equivalent to a flat penalty.
    pub demand_curve: PenaltyCurve,
}

impl ReserveProduct {
    /// ERCOT default 6-product set.
    pub fn ercot_defaults() -> Vec<ReserveProduct> {
        let default_curve = PenaltyCurve::Linear {
            cost_per_unit: 1000.0,
        };
        vec![
            ReserveProduct {
                id: "reg_up".into(),
                name: "Regulation Up".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 300.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                demand_curve: default_curve.clone(),
            },
            ReserveProduct {
                id: "reg_dn".into(),
                name: "Regulation Down".into(),
                direction: ReserveDirection::Down,
                deploy_secs: 300.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Footroom,
                demand_curve: default_curve.clone(),
            },
            ReserveProduct {
                id: "spin".into(),
                name: "Spinning Reserve".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::Synchronized,
                energy_coupling: EnergyCoupling::Headroom,
                demand_curve: default_curve.clone(),
            },
            ReserveProduct {
                id: "nspin".into(),
                name: "Non-Spinning Reserve".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 1800.0,
                qualification: QualificationRule::QuickStart,
                energy_coupling: EnergyCoupling::None,
                demand_curve: default_curve.clone(),
            },
            ReserveProduct {
                id: "ecrs".into(),
                name: "ERCOT Contingency Reserve".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                demand_curve: default_curve.clone(),
            },
            ReserveProduct {
                id: "rrs".into(),
                name: "Responsive Reserve".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::FrequencyResponsive,
                energy_coupling: EnergyCoupling::Headroom,
                demand_curve: default_curve,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Per-resource offer
// ---------------------------------------------------------------------------

/// A per-resource offer for a specific reserve product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveOffer {
    /// Must match a `ReserveProduct::id`.
    pub product_id: String,
    /// Maximum MW this resource can provide for this product.
    pub capacity_mw: f64,
    /// Offer price ($/MW-hr).
    pub cost_per_mwh: f64,
}

/// Per-resource qualification flags for custom qualification rules.
/// Maps product_id or flag_name to qualified (true/false).
pub type QualificationMap = HashMap<String, bool>;

// ---------------------------------------------------------------------------
// Requirements
// ---------------------------------------------------------------------------

/// System-wide requirement for a reserve product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemReserveRequirement {
    /// Which product (matches `ReserveProduct::id`).
    pub product_id: String,
    /// System-wide minimum MW (used for all periods when `per_period_mw` is None).
    pub requirement_mw: f64,
    /// Per-period override. When `Some`, `per_period_mw[t]` is used for period `t`
    /// instead of `requirement_mw`. Falls back to last element for `t >= len`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_period_mw: Option<Vec<f64>>,
}

impl SystemReserveRequirement {
    /// Effective requirement MW for a given period.
    pub fn requirement_mw_for_period(&self, period: usize) -> f64 {
        self.per_period_mw
            .as_ref()
            .and_then(|v| v.get(period).or_else(|| v.last()).copied())
            .unwrap_or(self.requirement_mw)
    }
}

/// A zonal requirement for any reserve product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonalReserveRequirement {
    /// Zone identifier — matches values in `generator_area`.
    pub zone_id: usize,
    /// Which product this requirement applies to (matches `ReserveProduct::id`).
    pub product_id: String,
    /// Minimum MW of this product required in this zone.
    pub requirement_mw: f64,
}

// ---------------------------------------------------------------------------
// Ramp sharing
// ---------------------------------------------------------------------------

/// Ramp sharing configuration for cross-product ramp feasibility.
///
/// Controls how reserve awards interact with energy ramp constraints:
/// - `sharing_ratio = 0.0`: Strict — each product's ramp fully additive (conservative).
/// - `sharing_ratio = 1.0`: Full sharing — total ramp available to all (aggressive).
/// - Intermediate: blended.
///
/// LP constraint (up direction, per generator):
/// ```text
/// Pg[g] + (1 - α) * Σ_{up products} R_p[g] ≤ prev_pg[g] + ramp_up_mw
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampSharingConfig {
    /// Sharing ratio in \[0.0, 1.0\].
    pub sharing_ratio: f64,
}

impl Default for RampSharingConfig {
    fn default() -> Self {
        Self { sharing_ratio: 0.0 }
    }
}

// ---------------------------------------------------------------------------
// Qualification helper
// ---------------------------------------------------------------------------

/// Check whether a resource qualifies for a reserve product.
///
/// # Arguments
/// - `rule`: the product's qualification rule
/// - `is_committed`: whether the resource is currently online
/// - `is_quick_start`: whether the resource can start within the deployment window
/// - `qualifications`: custom qualification flags on the resource
pub fn qualifies_for(
    rule: &QualificationRule,
    is_committed: bool,
    is_quick_start: bool,
    qualifications: &QualificationMap,
) -> bool {
    match rule {
        QualificationRule::Committed => is_committed,
        QualificationRule::Synchronized => is_committed,
        QualificationRule::QuickStart => is_quick_start || is_committed,
        QualificationRule::FrequencyResponsive => {
            is_committed
                && qualifications
                    .get("freq_responsive")
                    .copied()
                    .unwrap_or(false)
        }
        QualificationRule::Custom(flag) => {
            is_committed && qualifications.get(flag).copied().unwrap_or(false)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ercot_defaults_has_6_products() {
        let products = ReserveProduct::ercot_defaults();
        assert_eq!(products.len(), 6);
        let ids: Vec<&str> = products.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"reg_up"));
        assert!(ids.contains(&"reg_dn"));
        assert!(ids.contains(&"spin"));
        assert!(ids.contains(&"nspin"));
        assert!(ids.contains(&"ecrs"));
        assert!(ids.contains(&"rrs"));
    }

    #[test]
    fn test_ercot_defaults_directions() {
        let products = ReserveProduct::ercot_defaults();
        for p in &products {
            match p.id.as_str() {
                "reg_dn" => assert_eq!(p.direction, ReserveDirection::Down),
                _ => assert_eq!(p.direction, ReserveDirection::Up),
            }
        }
    }

    #[test]
    fn test_ercot_defaults_energy_coupling() {
        let products = ReserveProduct::ercot_defaults();
        for p in &products {
            match p.id.as_str() {
                "reg_dn" => assert_eq!(p.energy_coupling, EnergyCoupling::Footroom),
                "nspin" => assert_eq!(p.energy_coupling, EnergyCoupling::None),
                _ => assert_eq!(p.energy_coupling, EnergyCoupling::Headroom),
            }
        }
    }

    #[test]
    fn test_qualifies_committed() {
        let q = HashMap::new();
        assert!(qualifies_for(
            &QualificationRule::Committed,
            true,
            false,
            &q
        ));
        assert!(!qualifies_for(
            &QualificationRule::Committed,
            false,
            false,
            &q
        ));
    }

    #[test]
    fn test_qualifies_quick_start() {
        let q = HashMap::new();
        // Quick-start units qualify even offline
        assert!(qualifies_for(
            &QualificationRule::QuickStart,
            false,
            true,
            &q
        ));
        // Committed units also qualify
        assert!(qualifies_for(
            &QualificationRule::QuickStart,
            true,
            false,
            &q
        ));
        // Neither → no
        assert!(!qualifies_for(
            &QualificationRule::QuickStart,
            false,
            false,
            &q
        ));
    }

    #[test]
    fn test_qualifies_freq_responsive() {
        let mut q = HashMap::new();
        // Online but no flag → no
        assert!(!qualifies_for(
            &QualificationRule::FrequencyResponsive,
            true,
            false,
            &q
        ));
        // Online + flag → yes
        q.insert("freq_responsive".to_string(), true);
        assert!(qualifies_for(
            &QualificationRule::FrequencyResponsive,
            true,
            false,
            &q
        ));
        // Offline + flag → no (must be committed)
        assert!(!qualifies_for(
            &QualificationRule::FrequencyResponsive,
            false,
            false,
            &q
        ));
    }

    #[test]
    fn test_qualifies_custom() {
        let mut q = HashMap::new();
        q.insert("my_custom_flag".to_string(), true);
        assert!(qualifies_for(
            &QualificationRule::Custom("my_custom_flag".to_string()),
            true,
            false,
            &q
        ));
        assert!(!qualifies_for(
            &QualificationRule::Custom("other_flag".to_string()),
            true,
            false,
            &q
        ));
    }

    #[test]
    fn test_ramp_sharing_default() {
        let cfg = RampSharingConfig::default();
        assert_eq!(cfg.sharing_ratio, 0.0);
    }
}
