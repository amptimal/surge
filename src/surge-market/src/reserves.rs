// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical reserve-product constructors.
//!
//! Modern day-ahead / real-time electricity markets share a common
//! reserve taxonomy:
//!
//! * regulation up / down (fast, deployable inside ~5 minutes, up by
//!   headroom and down by footroom on committed units)
//! * synchronized (spinning) reserve — deployable within 10 minutes
//!   from online generators
//! * non-synchronized (supplemental) reserve — deployable within 10
//!   minutes from offline quick-start units
//! * ramping reserve up / down (online/offline variants) — extends the
//!   deploy window to 15 minutes so slower units can participate
//! * reactive reserves (up / down) — mvar-side capability
//!
//! The common zonal-requirement patterns are likewise shared:
//!
//! * a fraction of served load (for regulation-type products)
//! * a fraction of the largest dispatched producer (for contingency-
//!   sized products)
//! * an exogenous time-series requirement (market-operator-set
//!   ramping / reactive reserves)
//!
//! This module exposes a generic builder surface: adapters construct
//! [`ReserveProduct`] instances via [`reserve_product_from_catalog`]
//! and [`ZonalReserveRequirement`] rows via the three
//! `zonal_requirement_from_*` helpers. Nothing here is specific to any
//! one market's field names — format adapters translate data-source
//! field names into the inputs these helpers expect.
//!
//! The reactive-headroom synthetic product ([`reactive_headroom_product`])
//! is the one standard product that typically needs a synthetic
//! definition (there is no exogenous "reactive reserve zone" in most
//! datasets; it is derived from an aggregate MVAr requirement).

use surge_network::market::{
    EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveKind, ReserveProduct,
    ZonalReserveRequirement,
};

/// Abstract description of a reserve product suitable for materializing
/// a [`ReserveProduct`] once the market-side shortfall cost is known.
///
/// Adapters hand one of these to [`reserve_product_from_catalog`] with
/// a demand-curve cost to produce the final product.
#[derive(Debug, Clone)]
pub struct ReserveProductSpec {
    pub id: String,
    pub name: String,
    pub kind: ReserveKind,
    pub direction: ReserveDirection,
    pub deploy_secs: f64,
    pub qualification: QualificationRule,
    pub energy_coupling: EnergyCoupling,
    pub dispatchable_load_energy_coupling: Option<EnergyCoupling>,
    pub shared_limit_products: Vec<String>,
    pub balance_products: Vec<String>,
    /// Whether to apply the ramp-rate×deploy-window physical
    /// deliverability cap on top of the explicit reserve-offer cap.
    /// Markets that already encode deliverable reserve in their
    /// per-product capability fields set this `false`.
    pub apply_deploy_ramp_limit: bool,
}

impl ReserveProductSpec {
    /// Shorthand for the regulation-up spec used across modern markets.
    pub fn regulation_up() -> Self {
        Self {
            id: "reg_up".into(),
            name: "Regulation Up".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Up,
            deploy_secs: 300.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: Some(EnergyCoupling::Footroom),
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the regulation-down spec.
    pub fn regulation_down() -> Self {
        Self {
            id: "reg_down".into(),
            name: "Regulation Down".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Down,
            deploy_secs: 300.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Footroom,
            dispatchable_load_energy_coupling: Some(EnergyCoupling::Headroom),
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the synchronized (spinning) reserve spec.
    pub fn synchronized() -> Self {
        Self {
            id: "syn".into(),
            name: "Synchronized Reserve".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: QualificationRule::Synchronized,
            energy_coupling: EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: Some(EnergyCoupling::Footroom),
            shared_limit_products: vec!["reg_up".into()],
            balance_products: vec!["reg_up".into()],
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the non-synchronized (supplemental) reserve spec.
    pub fn non_synchronized() -> Self {
        Self {
            id: "nsyn".into(),
            name: "Non-Synchronized Reserve".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: QualificationRule::OfflineQuickStart,
            energy_coupling: EnergyCoupling::None,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: vec!["reg_up".into(), "syn".into()],
            balance_products: vec!["reg_up".into(), "syn".into()],
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the online ramping reserve up spec.
    pub fn ramping_up_online() -> Self {
        Self {
            id: "ramp_up_on".into(),
            name: "Ramping Reserve Up (Online)".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Up,
            deploy_secs: 900.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: Some(EnergyCoupling::Footroom),
            shared_limit_products: vec!["reg_up".into(), "syn".into()],
            balance_products: vec!["ramp_up_off".into()],
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the offline ramping reserve up spec.
    pub fn ramping_up_offline() -> Self {
        Self {
            id: "ramp_up_off".into(),
            name: "Ramping Reserve Up (Offline)".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Up,
            deploy_secs: 900.0,
            qualification: QualificationRule::OfflineQuickStart,
            energy_coupling: EnergyCoupling::None,
            dispatchable_load_energy_coupling: Some(EnergyCoupling::Footroom),
            shared_limit_products: vec!["nsyn".into()],
            balance_products: vec!["ramp_up_on".into()],
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the online ramping reserve down spec.
    pub fn ramping_down_online() -> Self {
        Self {
            id: "ramp_down_on".into(),
            name: "Ramping Reserve Down (Online)".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Down,
            deploy_secs: 900.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Footroom,
            dispatchable_load_energy_coupling: Some(EnergyCoupling::Headroom),
            shared_limit_products: vec!["reg_down".into()],
            balance_products: vec!["ramp_down_off".into()],
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the offline ramping reserve down spec.
    pub fn ramping_down_offline() -> Self {
        Self {
            id: "ramp_down_off".into(),
            name: "Ramping Reserve Down (Offline)".into(),
            kind: ReserveKind::Real,
            direction: ReserveDirection::Down,
            deploy_secs: 900.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Footroom,
            dispatchable_load_energy_coupling: Some(EnergyCoupling::Headroom),
            shared_limit_products: Vec::new(),
            balance_products: vec!["ramp_down_on".into()],
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the reactive reserve up spec.
    pub fn reactive_up() -> Self {
        Self {
            id: "q_res_up".into(),
            name: "Reactive Reserve Up".into(),
            kind: ReserveKind::Reactive,
            direction: ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::None,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            apply_deploy_ramp_limit: false,
        }
    }

    /// Shorthand for the reactive reserve down spec.
    pub fn reactive_down() -> Self {
        Self {
            id: "q_res_down".into(),
            name: "Reactive Reserve Down".into(),
            kind: ReserveKind::Reactive,
            direction: ReserveDirection::Down,
            deploy_secs: 600.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::None,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            apply_deploy_ramp_limit: false,
        }
    }
}

/// Materialize a [`ReserveProduct`] from a spec and a shortfall cost
/// (dollars per unit per hour in the market's canonical unit — $/MWh
/// for real, $/MVAr-h for reactive).
pub fn reserve_product_from_catalog(
    spec: &ReserveProductSpec,
    shortfall_cost_per_unit: f64,
) -> ReserveProduct {
    ReserveProduct {
        id: spec.id.clone(),
        name: spec.name.clone(),
        kind: spec.kind,
        apply_deploy_ramp_limit: spec.apply_deploy_ramp_limit,
        direction: spec.direction,
        deploy_secs: spec.deploy_secs,
        qualification: spec.qualification.clone(),
        energy_coupling: spec.energy_coupling,
        dispatchable_load_energy_coupling: spec.dispatchable_load_energy_coupling,
        shared_limit_products: spec.shared_limit_products.clone(),
        balance_products: spec.balance_products.clone(),
        demand_curve: PenaltyCurve::Linear {
            cost_per_unit: shortfall_cost_per_unit,
        },
    }
}

/// Build a [`ZonalReserveRequirement`] whose requirement scales with
/// the served dispatchable-load total in the zone.
///
/// `fraction` is the per-unit scaling coefficient (e.g. 0.05 for
/// "5 % of served load"). `periods` determines the shape of the
/// per-period requirement vector which is initialized to zero because
/// the served-load coefficient drives the endogenous requirement.
pub fn zonal_requirement_from_load_fraction(
    zone_id: usize,
    product_id: impl Into<String>,
    fraction: f64,
    periods: usize,
    participant_buses: Option<Vec<u32>>,
    shortfall_cost_per_unit: f64,
) -> ZonalReserveRequirement {
    ZonalReserveRequirement {
        zone_id,
        product_id: product_id.into(),
        requirement_mw: 0.0,
        per_period_mw: Some(vec![0.0; periods]),
        shortfall_cost_per_unit: Some(shortfall_cost_per_unit),
        served_dispatchable_load_coefficient: Some(fraction),
        largest_generator_dispatch_coefficient: None,
        participant_bus_numbers: normalize_participant_buses(participant_buses),
    }
}

/// Build a [`ZonalReserveRequirement`] whose requirement scales with
/// the largest dispatched generator in the zone. Used for
/// contingency-sized reserve products (syn, nsyn).
pub fn zonal_requirement_from_largest_unit(
    zone_id: usize,
    product_id: impl Into<String>,
    fraction: f64,
    participant_buses: Option<Vec<u32>>,
    shortfall_cost_per_unit: f64,
) -> ZonalReserveRequirement {
    ZonalReserveRequirement {
        zone_id,
        product_id: product_id.into(),
        requirement_mw: 0.0,
        per_period_mw: None,
        shortfall_cost_per_unit: Some(shortfall_cost_per_unit),
        served_dispatchable_load_coefficient: None,
        largest_generator_dispatch_coefficient: Some(fraction),
        participant_bus_numbers: normalize_participant_buses(participant_buses),
    }
}

/// Build a [`ZonalReserveRequirement`] whose requirement is an
/// exogenous per-period MW (or MVAr) time series.
pub fn zonal_requirement_from_series(
    zone_id: usize,
    product_id: impl Into<String>,
    per_period_mw: Vec<f64>,
    participant_buses: Option<Vec<u32>>,
    shortfall_cost_per_unit: f64,
) -> ZonalReserveRequirement {
    let requirement_mw = if per_period_mw.is_empty() {
        0.0
    } else {
        per_period_mw.iter().sum::<f64>() / per_period_mw.len() as f64
    };
    ZonalReserveRequirement {
        zone_id,
        product_id: product_id.into(),
        requirement_mw,
        per_period_mw: Some(per_period_mw),
        shortfall_cost_per_unit: Some(shortfall_cost_per_unit),
        served_dispatchable_load_coefficient: None,
        largest_generator_dispatch_coefficient: None,
        participant_bus_numbers: normalize_participant_buses(participant_buses),
    }
}

/// Synthesize a reactive-headroom product that prices aggregate
/// reactive shortfall when the underlying data does not expose
/// explicit reactive reserve offers.
///
/// The synthetic product is committed-qualified and uncoupled from
/// energy; it serves as a secondary-commitment constraint that forces
/// enough MVAr-capable units online to meet aggregate reactive
/// requirements.
pub fn reactive_headroom_product(shortfall_cost_per_mvar: f64) -> ReserveProduct {
    ReserveProduct {
        id: "q_headroom".to_string(),
        name: "Reactive Headroom (SCUC commitment)".to_string(),
        kind: ReserveKind::ReactiveHeadroom,
        apply_deploy_ramp_limit: false,
        direction: ReserveDirection::Up,
        deploy_secs: 600.0,
        qualification: QualificationRule::Committed,
        energy_coupling: EnergyCoupling::None,
        dispatchable_load_energy_coupling: None,
        shared_limit_products: Vec::new(),
        balance_products: Vec::new(),
        demand_curve: PenaltyCurve::Linear {
            cost_per_unit: shortfall_cost_per_mvar,
        },
    }
}

fn normalize_participant_buses(buses: Option<Vec<u32>>) -> Option<Vec<u32>> {
    match buses {
        None => None,
        Some(mut v) if v.is_empty() => {
            v.clear();
            None
        }
        Some(mut v) => {
            v.sort_unstable();
            Some(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_shorthands_produce_reasonable_defaults() {
        let reg = ReserveProductSpec::regulation_up();
        assert_eq!(reg.id, "reg_up");
        assert_eq!(reg.direction, ReserveDirection::Up);
        assert_eq!(reg.qualification, QualificationRule::Committed);

        let syn = ReserveProductSpec::synchronized();
        assert!(syn.shared_limit_products.contains(&"reg_up".to_string()));
    }

    #[test]
    fn reserve_product_from_catalog_sets_linear_demand_curve() {
        let spec = ReserveProductSpec::regulation_up();
        let p = reserve_product_from_catalog(&spec, 5_000.0);
        assert_eq!(p.id, "reg_up");
        match p.demand_curve {
            PenaltyCurve::Linear { cost_per_unit } => assert_eq!(cost_per_unit, 5_000.0),
            _ => panic!("expected linear demand curve"),
        }
    }

    #[test]
    fn load_fraction_requirement_sets_coefficient_and_zero_series() {
        let req = zonal_requirement_from_load_fraction(
            1,
            "reg_up",
            0.05,
            4,
            Some(vec![10, 7, 7]),
            1_000.0,
        );
        assert_eq!(req.zone_id, 1);
        assert_eq!(req.product_id, "reg_up");
        assert_eq!(req.served_dispatchable_load_coefficient, Some(0.05));
        assert_eq!(req.largest_generator_dispatch_coefficient, None);
        assert_eq!(req.per_period_mw, Some(vec![0.0; 4]));
        // participant buses sorted + deduped (duplicates allowed; sorted)
        assert_eq!(req.participant_bus_numbers, Some(vec![7, 7, 10]));
    }

    #[test]
    fn largest_unit_requirement_leaves_series_none() {
        let req = zonal_requirement_from_largest_unit(2, "syn", 1.0, None, 4_000.0);
        assert_eq!(req.largest_generator_dispatch_coefficient, Some(1.0));
        assert_eq!(req.per_period_mw, None);
    }

    #[test]
    fn series_requirement_computes_mean() {
        let req = zonal_requirement_from_series(
            3,
            "ramp_up_on",
            vec![20.0, 40.0, 60.0, 80.0],
            None,
            500.0,
        );
        assert!((req.requirement_mw - 50.0).abs() < 1e-9);
    }

    #[test]
    fn reactive_headroom_product_has_standard_shape() {
        let p = reactive_headroom_product(1234.5);
        assert_eq!(p.id, "q_headroom");
        assert_eq!(p.kind, ReserveKind::ReactiveHeadroom);
        assert_eq!(p.direction, ReserveDirection::Up);
        match p.demand_curve {
            PenaltyCurve::Linear { cost_per_unit } => assert_eq!(cost_per_unit, 1234.5),
            _ => panic!(),
        }
    }
}
