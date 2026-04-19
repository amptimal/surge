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
    /// Quick-start units that are currently offline.
    OfflineQuickStart,
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

/// Whether a reserve product secures real or reactive power.
///
/// Surge's reserve infrastructure is real-power-first because the DC
/// SCUC LP carries no reactive variables. Reactive-power reserves
/// couple to `Qg` rather than `Pg` and are cleared in the AC-OPF NLP
/// rather than the DC SCUC LP — the SCUC reserve builder filters
/// reactive products out at layout time so the LP layout stays
/// consistent, and the AC-OPF reads the same `ReserveProduct`
/// definitions and adds dedicated `q_reserve_up` / `q_reserve_down`
/// variables plus per-device Q-headroom rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReserveKind {
    /// Real-power reserve (reg-up, reg-down, spinning, non-spinning,
    /// on/off ramping, etc.). Default so serde round-trips on legacy
    /// reserve-product files without the kind tag.
    #[default]
    Real,
    /// Reactive-power reserve. Per-device Q-reserve variables and their
    /// headroom rows are cleared exclusively in the AC-OPF NLP — the
    /// SCUC LP filters these out at layout time.
    Reactive,
    /// Aggregate reactive-power headroom for SCUC commitment.
    ///
    /// Zonal reactive-reserve requirements ask for enough Q range on
    /// the committed fleet to cover `q^qru,min + q^qrd,min`. The DC
    /// SCUC cannot clear Q reserves (no `Qg` variable) but it can
    /// ensure the committed fleet has enough Q range by constraining:
    ///
    ///   `Σ (q_max_j − q_min_j) · u^on_jt ≥ q^qru,min_nt + q^qrd,min_nt`
    ///
    /// per zone per period. This kind signals the SCUC bounds code to
    /// derive the per-device physical cap from the reactive range
    /// `(qmax − qmin)` instead of the real-power range `(pmax − pmin)`.
    /// Adapters create a synthetic product with this kind when
    /// reactive zonal requirements are present.
    ReactiveHeadroom,
}

fn default_apply_deploy_ramp_limit() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
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
    /// Whether this product secures real or reactive power. Defaults to
    /// `Real` so serde round-trips on product definitions that predate
    /// the reactive-reserve kind tag. See [`ReserveKind`] for how surge
    /// routes the two kinds through the SCUC LP versus the AC OPF NLP.
    #[serde(default)]
    pub kind: ReserveKind,
    /// Whether the solver should impose an additional `ramp_rate × deploy_window`
    /// capability cap on top of the explicit reserve-offer quantity.
    ///
    /// Keep this enabled for markets where reserve offers are bids and
    /// physical deliverability still needs to be derived from the
    /// unit's ramp curve. Disable it when the source data's per-product
    /// capability field already encodes deliverable reserve (e.g. a
    /// format that publishes separate `p_syn_res_ub`, `p_reg_res_up_ub`,
    /// etc. fields).
    #[serde(
        default = "default_apply_deploy_ramp_limit",
        skip_serializing_if = "is_true"
    )]
    pub apply_deploy_ramp_limit: bool,
    /// Up or Down.
    pub direction: ReserveDirection,
    /// Deployment window in seconds. Reserve must be deliverable within this time.
    /// Used for ramp-reserve coupling: R\[g\] <= ramp_rate\[g\] * (deploy_secs / 60).
    pub deploy_secs: f64,
    /// Who can provide this product.
    pub qualification: QualificationRule,
    /// How this product couples with the energy dispatch variable.
    pub energy_coupling: EnergyCoupling,
    /// Optional energy-coupling mode for dispatchable loads.
    ///
    /// When omitted, dispatchable loads use `energy_coupling`. Some
    /// markets require symmetric but opposite load-side coupling for
    /// specific products — e.g. load-side regulation-up consumes
    /// footroom to `p_min` while generator-side regulation-up consumes
    /// headroom to `p_max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatchable_load_energy_coupling: Option<EnergyCoupling>,
    /// Reserve products that share this product's absolute capability limit.
    ///
    /// When non-empty, the model enforces
    /// `sum(shared_limit_products) + self <= capacity(self)` for online products,
    /// or the offline analogue for offline-only quick-start products.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_limit_products: Vec<String>,
    /// Reserve products whose cleared awards contribute to this product's
    /// balance requirement.
    ///
    /// When empty, only `self.id` contributes. This supports cumulative
    /// substitution ladders where higher-quality reserves can cover
    /// lower-quality balance requirements (e.g. a `reg_up + syn`
    /// balance or a `reg_up + syn + nsyn` balance).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub balance_products: Vec<String>,
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
                apply_deploy_ramp_limit: true,
                deploy_secs: 300.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                demand_curve: default_curve.clone(),
                kind: ReserveKind::Real,
            },
            ReserveProduct {
                id: "reg_dn".into(),
                name: "Regulation Down".into(),
                direction: ReserveDirection::Down,
                apply_deploy_ramp_limit: true,
                deploy_secs: 300.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Footroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                demand_curve: default_curve.clone(),
                kind: ReserveKind::Real,
            },
            ReserveProduct {
                id: "spin".into(),
                name: "Spinning Reserve".into(),
                direction: ReserveDirection::Up,
                apply_deploy_ramp_limit: true,
                deploy_secs: 600.0,
                qualification: QualificationRule::Synchronized,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                demand_curve: default_curve.clone(),
                kind: ReserveKind::Real,
            },
            ReserveProduct {
                id: "nspin".into(),
                name: "Non-Spinning Reserve".into(),
                direction: ReserveDirection::Up,
                apply_deploy_ramp_limit: true,
                deploy_secs: 1800.0,
                qualification: QualificationRule::QuickStart,
                energy_coupling: EnergyCoupling::None,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                demand_curve: default_curve.clone(),
                kind: ReserveKind::Real,
            },
            ReserveProduct {
                id: "ecrs".into(),
                name: "ERCOT Contingency Reserve".into(),
                direction: ReserveDirection::Up,
                apply_deploy_ramp_limit: true,
                deploy_secs: 600.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                demand_curve: default_curve.clone(),
                kind: ReserveKind::Real,
            },
            ReserveProduct {
                id: "rrs".into(),
                name: "Responsive Reserve".into(),
                direction: ReserveDirection::Up,
                apply_deploy_ramp_limit: true,
                deploy_secs: 600.0,
                qualification: QualificationRule::FrequencyResponsive,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                demand_curve: default_curve,
                kind: ReserveKind::Real,
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
    /// Zone identifier for this reserve requirement.
    ///
    /// When `participant_bus_numbers` is not provided, callers typically
    /// interpret this as a study-area id and infer membership from
    /// `generator_area` / bus area. When explicit participant buses are
    /// provided, `zone_id` remains the stable identifier used for result keys
    /// and reporting, while membership comes from `participant_bus_numbers`.
    pub zone_id: usize,
    /// Which product this requirement applies to (matches `ReserveProduct::id`).
    pub product_id: String,
    /// Base minimum MW of this product required in this zone.
    ///
    /// This exogenous base requirement is combined with any dynamic terms below.
    pub requirement_mw: f64,
    /// Per-period override for the exogenous base requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_period_mw: Option<Vec<f64>>,
    /// Optional linear shortfall penalty override for this zone and product.
    ///
    /// When `None`, the solver falls back to the product demand curve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortfall_cost_per_unit: Option<f64>,
    /// Optional coefficient on served dispatchable-load power in this zone.
    ///
    /// The effective requirement becomes:
    /// `base_requirement + served_dispatchable_load_coefficient * Σ served_dl`.
    /// Callers can embed any fixed-load contribution into `requirement_mw` or
    /// `per_period_mw`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_dispatchable_load_coefficient: Option<f64>,
    /// Optional coefficient on the largest generator dispatch in this zone.
    ///
    /// The effective requirement becomes:
    /// `base_requirement + largest_generator_dispatch_coefficient * peak_pg_zone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub largest_generator_dispatch_coefficient: Option<f64>,
    /// Optional explicit bus membership for this reserve zone.
    ///
    /// When present, the zonal reserve rows sum every producer / load
    /// whose bus number appears here, instead of inferring membership
    /// from one scalar area assignment. This supports formulations
    /// where the same bus may belong to multiple reserve zones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_bus_numbers: Option<Vec<u32>>,
}

impl ZonalReserveRequirement {
    /// Effective exogenous base requirement MW for a given period.
    pub fn requirement_mw_for_period(&self, period: usize) -> f64 {
        self.per_period_mw
            .as_ref()
            .and_then(|v| v.get(period).or_else(|| v.last()).copied())
            .unwrap_or(self.requirement_mw)
    }

    /// Whether this requirement carries explicit bus membership.
    pub fn has_explicit_participant_buses(&self) -> bool {
        self.participant_bus_numbers.is_some()
    }

    /// Test whether `bus_number` is explicitly in this reserve zone.
    pub fn includes_participant_bus_number(&self, bus_number: u32) -> bool {
        self.participant_bus_numbers
            .as_ref()
            .is_some_and(|buses| buses.contains(&bus_number))
    }
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
        QualificationRule::OfflineQuickStart => is_quick_start && !is_committed,
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

fn qualification_states(rule: &QualificationRule) -> (bool, bool) {
    match rule {
        QualificationRule::Committed
        | QualificationRule::Synchronized
        | QualificationRule::FrequencyResponsive
        | QualificationRule::Custom(_) => (true, false),
        QualificationRule::QuickStart => (true, true),
        QualificationRule::OfflineQuickStart => (false, true),
    }
}

/// Whether two qualification rules can be active simultaneously for the same
/// resource in at least one commitment state.
pub fn qualifications_can_overlap(lhs: &QualificationRule, rhs: &QualificationRule) -> bool {
    let (lhs_committed, lhs_offline) = qualification_states(lhs);
    let (rhs_committed, rhs_offline) = qualification_states(rhs);
    (lhs_committed && rhs_committed) || (lhs_offline && rhs_offline)
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
    fn test_qualifies_offline_quick_start() {
        let q = HashMap::new();
        assert!(qualifies_for(
            &QualificationRule::OfflineQuickStart,
            false,
            true,
            &q
        ));
        assert!(!qualifies_for(
            &QualificationRule::OfflineQuickStart,
            true,
            true,
            &q
        ));
        assert!(!qualifies_for(
            &QualificationRule::OfflineQuickStart,
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
    fn test_qualification_overlap_filters_mutually_exclusive_states() {
        assert!(qualifications_can_overlap(
            &QualificationRule::Committed,
            &QualificationRule::Synchronized,
        ));
        assert!(qualifications_can_overlap(
            &QualificationRule::OfflineQuickStart,
            &QualificationRule::QuickStart,
        ));
        assert!(!qualifications_can_overlap(
            &QualificationRule::OfflineQuickStart,
            &QualificationRule::Committed,
        ));
        assert!(!qualifications_can_overlap(
            &QualificationRule::OfflineQuickStart,
            &QualificationRule::FrequencyResponsive,
        ));
    }

    #[test]
    fn test_ramp_sharing_default() {
        let cfg = RampSharingConfig::default();
        assert_eq!(cfg.sharing_ratio, 0.0);
    }
}
