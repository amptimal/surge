// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Dispatchable load (demand-response) types and cost models for OPF.
//!
//! # Background
//!
//! Dispatchable loads are the demand-side counterpart to generators.  In modern
//! ISO/RTO markets (ERCOT, PJM, MISO):
//!
//! - **Interruptible loads** (ILs): industrial customers with curtailment contracts,
//!   dispatched by the ISO when the LMP exceeds their curtailment threshold.
//! - **Demand Response Resources** (DRRs): aggregated behind-the-meter loads that
//!   reduce consumption for a price signal.
//! - **Emergency Response Service** (ERS in ERCOT): loads that curtail on emergency
//!   signals at a fixed interruptible price.
//! - **EV charging stations**: flexible load that can shift time-of-use.
//! - **Virtual Power Plants**: aggregated small loads + DER acting as a single
//!   dispatchable resource.
//!
//! # Formulation
//!
//! Standard OPF minimizes generation cost given fixed load:
//! ```text
//! min  Σ C_gen(Pg)
//! s.t. power balance, thermal limits, voltage limits
//! ```
//!
//! With dispatchable loads the problem becomes **social welfare maximization**:
//! ```text
//! min  Σ C_gen(Pg) - Σ U_load(P_served)
//! s.t. power balance: Σ Pg - Σ P_load_fixed - Σ (p_sched - P_served) - P_calc(V) = 0
//!      P_served[i] ∈ [p_min[i], p_max[i]]
//! ```
//!
//! The net load at bus k becomes:
//! ```text
//! P_load_effective[k] = P_load_fixed[k] + Σ (p_sched[i] - P_served[i])
//!                                          i at bus k
//! ```
//! So when `P_served = p_sched` (fully served), the load is normal.
//! When `P_served < p_sched` (curtailed), net load is reduced by the curtailment.
//!
//! # Sign Convention
//!
//! All power quantities use the generator convention (positive = injection):
//! - Loads consume power: they appear as negative injection.
//! - `p_sched_pu` > 0 means the load consumes `p_sched * base_mva` MW.
//! - The OPF variable `P_served` ∈ [p_min, p_max] (both positive).
//! - Curtailment = `p_sched - P_served` (positive when load is curtailed).
//! - Power balance contribution: `P_served - p_sched` (negative when fully served,
//!   less negative when curtailed — i.e., reduced load = increased net injection).

use serde::{Deserialize, Serialize};

use crate::market::EnergyOffer;

/// Per-period parameter override for a dispatchable load.
///
/// Analogous to `OfferSchedule` for generators, this lets
/// the DA/RT market engine supply per-period demand curves that the
/// SCUC/SCED LP uses for cost coefficients and capacity bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlPeriodParams {
    /// Scheduled / baseline real power consumption this period (per-unit).
    pub p_sched_pu: f64,
    /// Maximum real power served this period (per-unit).
    pub p_max_pu: f64,
    /// Cost / benefit model for this period's objective.
    pub cost_model: LoadCostModel,
}

/// Time-varying schedule for a dispatchable load, per period.
///
/// Index into `periods` = period (hour for DA, interval for RT).
/// `None` = use the base [`DispatchableLoad`] fields for that period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DlOfferSchedule {
    /// Per-period overrides.  `periods[t] = None` means fall back to base.
    pub periods: Vec<Option<DlPeriodParams>>,
}

/// Demand-response load archetype, governing the OPF variable structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadArchetype {
    /// P curtailable in `[p_min, p_sched]`, Q follows a fixed power factor.
    ///
    /// Most common archetype for industrial interruptible contracts.  The OPF
    /// chooses how much real power to serve; reactive power follows automatically
    /// via `Q_served = Q_sched * (P_served / p_sched)`.
    Curtailable,

    /// Elastic / price-responsive demand: OPF chooses P in `[p_min, p_max]`.
    ///
    /// Used for loads with a true demand curve (EV charging aggregators,
    /// virtual power plants, demand-side bidding in day-ahead markets).
    /// Typically paired with [`LoadCostModel::QuadraticUtility`] or
    /// [`LoadCostModel::PiecewiseLinear`].
    Elastic,

    /// Interruptible: P ∈ [0, p_sched], continuous relaxation of {0, p_sched}.
    ///
    /// Models loads that are either fully on or fully off (interruptible service
    /// contracts).  The LP/NLP relaxation allows fractional values; round
    /// post-solve if binary decisions are needed.
    Interruptible,

    /// Independent P and Q control.
    ///
    /// Used for STATCOM-like loads, EV chargers with reactive capability, or
    /// any resource that can independently set real and reactive power.
    IndependentPQ,
}

/// Cost / benefit model governing the OPF objective contribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadCostModel {
    /// Linear curtailment cost: `c * (P_sched - P_served)` [$/MWh].
    ///
    /// Represents the Value of Lost Load (VOLL) or interruptible contract price.
    /// OPF will curtail when `LMP > c` (curtailment is cheaper than serving the load).
    /// Objective contribution to minimise: `c * (p_sched - P_served)`.
    /// Gradient d_obj/d_P_served = `-c` (serving more load reduces curtailment cost).
    LinearCurtailment {
        /// Curtailment cost in $/MWh (Value of Lost Load or contract price).
        cost_per_mw: f64,
    },

    /// Quadratic utility / welfare: `U(P) = a*P - b*P²/2`.
    ///
    /// The marginal utility is `MU(P) = a - b*P` (linear demand curve).
    /// OPF minimises `-U(P)` (equivalently, maximises utility minus cost).
    /// At equilibrium: `MU(P_served) = LMP` (price equals marginal value).
    ///
    /// OPF objective contribution (to minimise): `-(a*P - b*P²/2)`.
    /// Gradient d_obj/d_P = `-(a - b*P) = b*P - a`.
    /// Second derivative (for Hessian): `b` (positive = convex minimisation).
    QuadraticUtility {
        /// Choke price in $/MWh (marginal utility at P = 0).
        a: f64,
        /// Slope of marginal utility curve in $/MW²h (demand curve steepness).
        b: f64,
    },

    /// Piecewise-linear utility: list of `(P_breakpoint_MW, marginal_utility_$/MWh)` pairs.
    ///
    /// Breakpoints must be sorted by P in ascending order.  The utility between
    /// adjacent breakpoints is the integral of the linear interpolated MU.
    /// Compatible with LP formulation (no quadratic terms).
    PiecewiseLinear {
        /// Breakpoints: `(P_MW, marginal_utility_$/MWh)`.  Must be sorted by P.
        points: Vec<(f64, f64)>,
    },

    /// Fixed penalty for any curtailment (interruptible contract).
    ///
    /// Similar to `LinearCurtailment` but semantically represents a per-event
    /// interrupt payment rather than a value-of-lost-load.
    /// Objective: `c * (P_sched - P_served)`.
    InterruptPenalty {
        /// Payment to load owner per MW curtailed per hour [$/MWh].
        cost_per_mw: f64,
    },
}

impl LoadCostModel {
    /// Evaluate cost contribution to OPF objective (to **minimise**).
    ///
    /// For utility models, returns the *negative* utility (minimising negative
    /// utility maximises social welfare).  For cost models, returns the positive
    /// curtailment cost.
    ///
    /// `p_served` and `p_sched` are both in per-unit (positive = consuming).
    /// Returns $/hr (consistent with generator cost units in the OPF).
    pub fn objective_contribution(&self, p_served_pu: f64, p_sched_pu: f64, base_mva: f64) -> f64 {
        match self {
            LoadCostModel::LinearCurtailment { cost_per_mw }
            | LoadCostModel::InterruptPenalty { cost_per_mw } => {
                // c * (P_sched - P_served) in $/hr
                cost_per_mw * (p_sched_pu - p_served_pu) * base_mva
            }
            LoadCostModel::QuadraticUtility { a, b } => {
                // Minimise -(a*P - b*P²/2). P in MW = p_served_pu * base_mva.
                let p_mw = p_served_pu * base_mva;
                -(a * p_mw - b * p_mw * p_mw / 2.0)
            }
            LoadCostModel::PiecewiseLinear { points } => {
                if points.len() < 2 {
                    return 0.0;
                }
                let p_mw = p_served_pu * base_mva;
                // Integrate MU(P) from 0 to p_mw = utility value.
                // Objective = -utility (we minimise -U).
                let utility = integrate_pwl_utility(points, p_mw);
                -utility
            }
        }
    }

    /// Gradient of objective contribution w.r.t. `p_served_pu`.
    ///
    /// Used in gradient evaluation (AC-OPF objective gradient).
    /// Units: $/hr / (pu) = $/hr * base_mva / MW.
    pub fn d_obj_d_p(&self, p_served_pu: f64, base_mva: f64) -> f64 {
        match self {
            LoadCostModel::LinearCurtailment { cost_per_mw }
            | LoadCostModel::InterruptPenalty { cost_per_mw } => {
                // d/dP_pu [c * (p_sched - P_served) * base] = -c * base
                -cost_per_mw * base_mva
            }
            LoadCostModel::QuadraticUtility { a, b } => {
                // d/dP_pu [-(a*P_mw - b*P_mw²/2)] where P_mw = P_pu * base
                // = -(a - b*P_mw) * base = (b*P_mw - a) * base
                let p_mw = p_served_pu * base_mva;
                (b * p_mw - a) * base_mva
            }
            LoadCostModel::PiecewiseLinear { points } => {
                if points.len() < 2 {
                    return 0.0;
                }
                let p_mw = p_served_pu * base_mva;
                // d(-utility)/dP_pu = -MU(P) * base
                let mu = interpolate_marginal_utility(points, p_mw);
                -mu * base_mva
            }
        }
    }

    /// Second derivative of objective w.r.t. `p_served_pu` (for Hessian).
    ///
    /// Non-zero only for [`LoadCostModel::QuadraticUtility`]: `b * base_mva²`.
    /// All other models have zero second derivative (linear in P_served).
    pub fn d2_obj_d_p2(&self, base_mva: f64) -> f64 {
        match self {
            LoadCostModel::QuadraticUtility { b, .. } => b * base_mva * base_mva,
            _ => 0.0,
        }
    }

    /// Whether this cost model contributes a nonzero quadratic (Hessian) term.
    pub fn has_quadratic_term(&self) -> bool {
        matches!(self, LoadCostModel::QuadraticUtility { .. })
    }

    /// Linear objective coefficient for DC-OPF (LP/QP).
    ///
    /// For LP-compatible models (LinearCurtailment, InterruptPenalty):
    ///   returns `-cost_per_mw * base_mva` (coefficient on P_served_pu in objective).
    ///
    /// For QP models (QuadraticUtility): returns the linear coefficient `-a * base_mva`.
    /// The quadratic term must be added to the Hessian separately.
    ///
    /// For PiecewiseLinear: returns 0.0 (pwl epiograph constraints handle it).
    pub fn dc_linear_obj_coeff(&self, base_mva: f64) -> f64 {
        match self {
            LoadCostModel::LinearCurtailment { cost_per_mw }
            | LoadCostModel::InterruptPenalty { cost_per_mw } => {
                // Objective: c*(p_sched - P_served)*base → linear coeff on P_served = -c*base
                -cost_per_mw * base_mva
            }
            LoadCostModel::QuadraticUtility { a, .. } => {
                // Objective: -(a*P_mw - b*P_mw²/2) → linear coeff = -a*base
                -a * base_mva
            }
            LoadCostModel::PiecewiseLinear { .. } => 0.0,
        }
    }

    /// Quadratic diagonal coefficient for DC-OPF (QP Hessian).
    ///
    /// For [`LoadCostModel::QuadraticUtility`]: `b * base_mva²` (full quadratic coefficient,
    /// HiGHS applies 0.5× internally for the symmetric ½ x'Hx convention).
    pub fn dc_quadratic_obj_coeff(&self, base_mva: f64) -> f64 {
        match self {
            LoadCostModel::QuadraticUtility { b, .. } => b * base_mva * base_mva,
            _ => 0.0,
        }
    }
}

/// Integrate piecewise-linear marginal utility from P=0 to P=p_mw.
fn integrate_pwl_utility(points: &[(f64, f64)], p_mw: f64) -> f64 {
    let mut utility = 0.0;
    let mut prev_p = 0.0;
    let mut prev_mu = if !points.is_empty() { points[0].1 } else { 0.0 };

    // Extend from 0 to first breakpoint using first MU value
    if !points.is_empty() && points[0].0 > 0.0 {
        let end = p_mw.min(points[0].0);
        utility += prev_mu * end;
        if p_mw <= points[0].0 {
            return utility;
        }
        prev_p = points[0].0;
    }

    for &(bp, mu) in points {
        if bp <= prev_p {
            prev_mu = mu;
            continue;
        }
        let end = p_mw.min(bp);
        // Trapezoid integration over [prev_p, end]
        let mid_mu = prev_mu + (mu - prev_mu) * (end - prev_p) / (bp - prev_p);
        utility += (prev_mu + mid_mu) / 2.0 * (end - prev_p);
        if p_mw <= bp {
            return utility;
        }
        prev_p = bp;
        prev_mu = mu;
    }
    // Beyond last breakpoint: assume MU = last value (flat)
    if p_mw > prev_p {
        utility += prev_mu * (p_mw - prev_p);
    }
    utility
}

/// Interpolate marginal utility at a given P_mw from PWL breakpoints.
fn interpolate_marginal_utility(points: &[(f64, f64)], p_mw: f64) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    if p_mw <= points[0].0 {
        return points[0].1;
    }
    for i in 1..points.len() {
        let (p0, mu0) = points[i - 1];
        let (p1, mu1) = points[i];
        if p_mw <= p1 {
            let t = (p_mw - p0) / (p1 - p0);
            return mu0 + t * (mu1 - mu0);
        }
    }
    // Beyond last breakpoint: use last MU value
    points.last().map(|&(_, mu)| mu).unwrap_or(0.0)
}

/// A dispatchable load resource at a specific bus.
///
/// Represents any demand-side resource that can vary its consumption in response
/// to price signals or ISO dispatch instructions.  Compatible archetypes include
/// interruptible loads, demand response resources, EV charging aggregators, and
/// virtual power plants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchableLoad {
    /// External bus number this resource is attached to.
    pub bus: u32,

    /// Scheduled / baseline real power consumption (per-unit, positive = consuming).
    pub p_sched_pu: f64,

    /// Scheduled / baseline reactive power consumption (per-unit, positive = consuming).
    pub q_sched_pu: f64,

    /// Minimum real power served (per-unit).
    ///
    /// - Curtailable: `p_min < p_sched` (partial curtailment allowed).
    /// - Interruptible: 0.0 (full curtailment allowed).
    /// - Elastic: may be 0.0 or greater.
    pub p_min_pu: f64,

    /// Maximum real power served (per-unit).
    ///
    /// - Curtailable / Interruptible: typically equals `p_sched_pu`.
    /// - Elastic: may exceed `p_sched_pu` (load can increase above baseline).
    pub p_max_pu: f64,

    /// Minimum reactive power served (per-unit).
    ///
    /// For fixed-power-factor loads: `q_sched * p_min / p_sched`.
    /// For IndependentPQ: independently bounded.
    pub q_min_pu: f64,

    /// Maximum reactive power served (per-unit).
    ///
    /// For fixed-power-factor loads: `q_sched_pu`.
    /// For IndependentPQ: independently bounded.
    pub q_max_pu: f64,

    /// Demand-response archetype — governs variable structure and constraints.
    pub archetype: LoadArchetype,

    /// Cost / benefit model for objective function.
    pub cost_model: LoadCostModel,

    /// Fixed power factor flag.
    ///
    /// When `true`, reactive power tracks real power proportionally:
    ///   `Q_served = q_sched_pu / p_sched_pu * P_served`
    ///
    /// Implemented as an equality constraint in AC-OPF, or by substituting
    /// `Q_served` as a function of `P_served` (eliminating one variable).
    ///
    /// Automatically `false` for `IndependentPQ` archetype.
    pub fixed_power_factor: bool,

    /// Whether this resource is active in the OPF.
    ///
    /// When `false`, the resource is treated as a fixed load at `p_sched_pu`.
    pub in_service: bool,

    /// Stable user-facing identifier for this demand-response resource.
    ///
    /// When empty, callers may canonicalize it from network context.
    #[serde(default)]
    pub resource_id: String,

    // -----------------------------------------------------------------------
    // Market product metadata (informational — does not affect OPF math)
    // -----------------------------------------------------------------------
    /// ISO/RTO market product type, e.g., "ECRSS", "NSRS", "RRS", "ERCOT_DR".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_type: Option<String>,

    /// Dispatch notification lead time in minutes.
    ///
    /// Distinguishes real-time (< 10 min) from day-ahead (> 60 min) resources.
    /// Informational — affects market product eligibility, not OPF math.
    #[serde(default)]
    pub dispatch_notification_minutes: f64,

    /// Minimum dispatch duration in hours (e.g., 1.0 for ERS in ERCOT).
    ///
    /// Informational — affects market product eligibility, not OPF math.
    #[serde(default)]
    pub min_duration_hours: f64,
    /// Customer baseline load (MW).  When present, curtailment is measured as
    /// `baseline_mw - p_served_mw` instead of `p_sched - p_served`.
    /// Used for settlement.  Dispatch optimization still uses `p_sched`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_mw: Option<f64>,
    /// Fraction of curtailed energy that rebounds as additional load after
    /// curtailment ends (0.0–1.0).  Zero means no rebound.
    #[serde(default)]
    pub rebound_fraction: f64,
    /// Number of periods over which rebound energy is spread.
    #[serde(default)]
    pub rebound_periods: usize,

    // --- market ---
    /// Energy offer for market clearing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_offer: Option<EnergyOffer>,
    /// Reserve offers keyed by product ID (generic reserve model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserve_offers: Vec<crate::market::reserve::ReserveOffer>,
    /// Custom qualification flags for reserve products.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub qualifications: crate::market::reserve::QualificationMap,
}

impl DispatchableLoad {
    /// Create a curtailable load with linear curtailment cost.
    ///
    /// # Arguments
    /// - `bus`: external bus number
    /// - `p_sched_mw`, `q_sched_mvar`: scheduled consumption (MW / MVAr)
    /// - `p_min_mw`: minimum served real power (MW)
    /// - `cost_per_mwh`: Value of Lost Load or interruptible contract price ($/MWh)
    /// - `base_mva`: system MVA base for per-unit conversion
    pub fn curtailable(
        bus: u32,
        p_sched_mw: f64,
        q_sched_mvar: f64,
        p_min_mw: f64,
        cost_per_mwh: f64,
        base_mva: f64,
    ) -> Self {
        let pf_ratio = if p_sched_mw.abs() > 1e-10 {
            q_sched_mvar / p_sched_mw
        } else {
            0.0
        };
        let p_sched_pu = p_sched_mw / base_mva;
        let q_sched_pu = q_sched_mvar / base_mva;
        let p_min_pu = p_min_mw / base_mva;
        let q_min_pu = p_min_pu * pf_ratio;
        Self {
            bus,
            p_sched_pu,
            q_sched_pu,
            p_min_pu,
            p_max_pu: p_sched_pu,
            q_min_pu,
            q_max_pu: q_sched_pu,
            archetype: LoadArchetype::Curtailable,
            cost_model: LoadCostModel::LinearCurtailment {
                cost_per_mw: cost_per_mwh,
            },
            fixed_power_factor: true,
            in_service: true,
            resource_id: String::new(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            energy_offer: None,
            reserve_offers: Vec::new(),
            qualifications: std::collections::HashMap::new(),
        }
    }

    /// Create an elastic load with quadratic utility function.
    ///
    /// Demand curve: `MU(P) = a - b*P` where P is in MW.
    /// At equilibrium: `MU(P_served) = LMP`.
    pub fn elastic(
        bus: u32,
        p_min_mw: f64,
        p_max_mw: f64,
        a_choke_price: f64,
        b_slope: f64,
        base_mva: f64,
    ) -> Self {
        let p_min_pu = p_min_mw / base_mva;
        let p_max_pu = p_max_mw / base_mva;
        // Reactive bounds: assume unity power factor (Q = 0)
        Self {
            bus,
            p_sched_pu: p_max_pu,
            q_sched_pu: 0.0,
            p_min_pu,
            p_max_pu,
            q_min_pu: 0.0,
            q_max_pu: 0.0,
            archetype: LoadArchetype::Elastic,
            cost_model: LoadCostModel::QuadraticUtility {
                a: a_choke_price,
                b: b_slope,
            },
            fixed_power_factor: false,
            in_service: true,
            resource_id: String::new(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            energy_offer: None,
            reserve_offers: Vec::new(),
            qualifications: std::collections::HashMap::new(),
        }
    }

    /// Create an interruptible load with interrupt penalty.
    pub fn interruptible(
        bus: u32,
        p_sched_mw: f64,
        q_sched_mvar: f64,
        cost_per_mwh: f64,
        base_mva: f64,
    ) -> Self {
        let p_sched_pu = p_sched_mw / base_mva;
        let q_sched_pu = q_sched_mvar / base_mva;
        Self {
            bus,
            p_sched_pu,
            q_sched_pu,
            p_min_pu: 0.0,
            p_max_pu: p_sched_pu,
            q_min_pu: 0.0,
            q_max_pu: q_sched_pu,
            archetype: LoadArchetype::Interruptible,
            cost_model: LoadCostModel::InterruptPenalty {
                cost_per_mw: cost_per_mwh,
            },
            fixed_power_factor: true,
            in_service: true,
            resource_id: String::new(),
            product_type: None,
            dispatch_notification_minutes: 0.0,
            min_duration_hours: 0.0,
            baseline_mw: None,
            rebound_fraction: 0.0,
            rebound_periods: 0,
            energy_offer: None,
            reserve_offers: Vec::new(),
            qualifications: std::collections::HashMap::new(),
        }
    }

    /// Get the reserve offer for a specific product, if any.
    pub fn reserve_offer(&self, product_id: &str) -> Option<&crate::market::reserve::ReserveOffer> {
        self.reserve_offers
            .iter()
            .find(|o| o.product_id == product_id)
    }

    /// Net injection contribution at the bus at scheduled level (negative = load consuming).
    #[inline]
    pub fn p_injection_sched(&self) -> f64 {
        -self.p_sched_pu
    }

    /// Net reactive injection at the bus at scheduled level.
    #[inline]
    pub fn q_injection_sched(&self) -> f64 {
        -self.q_sched_pu
    }

    /// Fixed-power-factor ratio Q/P (0 if p_sched = 0).
    #[inline]
    pub fn pf_ratio(&self) -> f64 {
        if self.p_sched_pu.abs() > 1e-10 {
            self.q_sched_pu / self.p_sched_pu
        } else {
            0.0
        }
    }
}

/// Post-solve dispatch result for a single dispatchable load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadDispatchResult {
    /// External bus number this dispatch result applies to.
    pub bus: u32,

    /// Real power served at optimal dispatch (per-unit, positive = consuming).
    pub p_served_pu: f64,

    /// Reactive power served at optimal dispatch (per-unit, positive = consuming).
    pub q_served_pu: f64,

    /// Real power curtailed: `p_sched - p_served` (per-unit, ≥ 0).
    pub p_curtailed_pu: f64,

    /// Curtailment percentage: `(p_curtailed / p_sched) * 100`.
    pub curtailment_pct: f64,

    /// Objective cost contribution ($/hr), consistent with generator costs.
    pub cost_contribution: f64,

    /// LMP at this bus from the OPF solution ($/MWh).
    pub lmp_at_bus: f64,

    /// Net economic benefit of curtailment: `(LMP - curtailment_cost) * MW_curtailed`.
    ///
    /// Positive when curtailment is economically beneficial (LMP > curtailment cost).
    pub net_curtailment_benefit: f64,
}

impl LoadDispatchResult {
    /// Build a dispatch result from OPF solution values.
    ///
    /// # Arguments
    /// - `dl`: the dispatchable load resource
    /// - `p_served_pu`: optimal P_served from OPF (per-unit)
    /// - `q_served_pu`: optimal Q_served from OPF (per-unit)
    /// - `lmp`: LMP at the load's bus ($/MWh)
    /// - `base_mva`: system MVA base
    pub fn from_solution(
        dl: &DispatchableLoad,
        p_served_pu: f64,
        q_served_pu: f64,
        lmp: f64,
        base_mva: f64,
    ) -> Self {
        let baseline_pu = dl
            .baseline_mw
            .map(|mw| mw / base_mva)
            .unwrap_or(dl.p_sched_pu);
        let p_curtailed_pu = (baseline_pu - p_served_pu).max(0.0);
        let curtailment_pct = if baseline_pu.abs() > 1e-12 {
            p_curtailed_pu / baseline_pu * 100.0
        } else {
            0.0
        };
        let cost_contribution =
            dl.cost_model
                .objective_contribution(p_served_pu, dl.p_sched_pu, base_mva);

        // Net benefit of curtailment: (LMP - curtailment_cost) * MW_curtailed
        // When LMP > curtailment_cost, it is cheaper to curtail and buy less energy.
        let curtailment_cost = match &dl.cost_model {
            LoadCostModel::LinearCurtailment { cost_per_mw }
            | LoadCostModel::InterruptPenalty { cost_per_mw } => *cost_per_mw,
            LoadCostModel::QuadraticUtility { a, b } => {
                // Effective curtailment cost ≈ average marginal utility in [p_served, p_sched]
                let p_served_mw = p_served_pu * base_mva;
                let p_sched_mw = dl.p_sched_pu * base_mva;
                let mu_served = a - b * p_served_mw;
                let mu_sched = a - b * p_sched_mw;
                (mu_served + mu_sched) / 2.0
            }
            LoadCostModel::PiecewiseLinear { .. } => lmp, // use LMP as proxy
        };
        let p_curtailed_mw = p_curtailed_pu * base_mva;
        let net_curtailment_benefit = (lmp - curtailment_cost) * p_curtailed_mw;

        Self {
            bus: dl.bus,
            p_served_pu,
            q_served_pu,
            p_curtailed_pu,
            curtailment_pct,
            cost_contribution,
            lmp_at_bus: lmp,
            net_curtailment_benefit,
        }
    }
}

/// Full demand-response dispatch results from an OPF solve.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DemandResponseResults {
    /// Per-load dispatch outcomes.
    pub loads: Vec<LoadDispatchResult>,

    /// Total real power served across all dispatchable loads (MW).
    pub total_served_mw: f64,

    /// Total real power curtailed across all dispatchable loads (MW).
    pub total_curtailed_mw: f64,

    /// Total cost contribution to OPF objective ($/hr).
    pub total_cost: f64,

    /// Consumer surplus: value received by load owners minus payments ($/hr).
    pub consumer_surplus: f64,
}

impl DemandResponseResults {
    /// Build aggregate results from per-load dispatch.
    pub fn from_load_results(loads: Vec<LoadDispatchResult>, base_mva: f64) -> Self {
        let total_served_mw = loads.iter().map(|r| r.p_served_pu * base_mva).sum();
        let total_curtailed_mw = loads.iter().map(|r| r.p_curtailed_pu * base_mva).sum();
        let total_cost = loads.iter().map(|r| r.cost_contribution).sum();
        let consumer_surplus = loads
            .iter()
            .map(|r| {
                // CS = utility received - cost paid = (U(P_served) + curtailment_value) - LMP * P_served
                // Simplified: surplus = net_curtailment_benefit (economic gain from curtailment)
                r.net_curtailment_benefit.max(0.0)
            })
            .sum();
        Self {
            loads,
            total_served_mw,
            total_curtailed_mw,
            total_cost,
            consumer_surplus,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_curtailment_objective() {
        let model = LoadCostModel::LinearCurtailment { cost_per_mw: 50.0 };
        // p_sched = 0.15 pu, p_served = 0.10 pu, base = 100 MVA
        // curtailment = 0.05 pu * 100 = 5 MW → cost = 50 * 5 = 250 $/hr
        let obj = model.objective_contribution(0.10, 0.15, 100.0);
        assert!((obj - 250.0).abs() < 1e-10, "got {obj}");
    }

    #[test]
    fn test_quadratic_utility_objective() {
        let _model = LoadCostModel::QuadraticUtility { a: 60.0, b: 100.0 };
        // U(P) = a*P - b*P²/2, P_mw = 0.10*100 = 10 MW
        // U(10) = 60*10 - 100*100/2 = 600 - 5000 = ... wait, b in $/MW²h
        // U(10) = 60*10 - 100*10*10/2 = 600 - 5000 ... that's negative
        // Actually for a = 60, b = 100, MU(0) = 60, MU(0.6) = 0 → equilibrium at 0.6 MW
        // So p_max should be ~0.006 pu. Let's test with a=60, b=1 (more reasonable scale)
        let model2 = LoadCostModel::QuadraticUtility { a: 60.0, b: 1.0 };
        let p = 0.10; // pu → 10 MW
        let base = 100.0;
        let p_mw = p * base;
        // U(10) = 60*10 - 1*100/2 = 600 - 50 = 550 $/hr
        let expected = -(60.0 * p_mw - 1.0 * p_mw * p_mw / 2.0);
        let obj = model2.objective_contribution(p, p, base);
        assert!(
            (obj - expected).abs() < 1e-6,
            "expected {expected}, got {obj}"
        );
    }

    #[test]
    fn test_quadratic_utility_gradient() {
        let model = LoadCostModel::QuadraticUtility { a: 50.0, b: 100.0 };
        // d(-U)/dP_pu = (b*P_mw - a) * base
        let p = 0.002; // pu → 0.2 MW
        let base = 100.0;
        let p_mw = p * base;
        let expected = (100.0 * p_mw - 50.0) * base;
        let grad = model.d_obj_d_p(p, base);
        assert!(
            (grad - expected).abs() < 1e-6,
            "expected {expected}, got {grad}"
        );
    }

    #[test]
    fn test_quadratic_hessian() {
        let model = LoadCostModel::QuadraticUtility { a: 50.0, b: 100.0 };
        let base = 100.0;
        // d²(-U)/dP_pu² = b * base² = 100 * 10000 = 1e6
        let h = model.d2_obj_d_p2(base);
        assert!((h - 100.0 * 100.0 * 100.0).abs() < 1e-6, "got {h}");
    }

    #[test]
    fn test_linear_curtailment_gradient() {
        let model = LoadCostModel::LinearCurtailment { cost_per_mw: 50.0 };
        let grad = model.d_obj_d_p(0.15, 100.0);
        // d/dP [c * (p_sched - P) * base] = -c * base = -50 * 100 = -5000
        assert!((grad - (-5000.0)).abs() < 1e-10, "got {grad}");
    }

    #[test]
    fn test_curtailable_constructor() {
        let dl = DispatchableLoad::curtailable(3, 150.0, 30.0, 50.0, 50.0, 100.0);
        assert_eq!(dl.bus, 3);
        assert!((dl.p_sched_pu - 1.5).abs() < 1e-10);
        assert!((dl.p_min_pu - 0.5).abs() < 1e-10);
        assert!((dl.q_sched_pu - 0.3).abs() < 1e-10);
        assert!(dl.fixed_power_factor);
        assert!(matches!(dl.archetype, LoadArchetype::Curtailable));
    }

    #[test]
    fn test_pf_ratio() {
        let dl = DispatchableLoad::curtailable(0, 100.0, 50.0, 0.0, 50.0, 100.0);
        assert!((dl.pf_ratio() - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_dispatch_result_net_benefit() {
        let dl = DispatchableLoad::curtailable(5, 150.0, 0.0, 50.0, 40.0, 100.0);
        // LMP = 60 $/MWh, curtailment cost = 40 $/MWh → beneficial to curtail
        // p_served = 0.5 pu (50 MW), p_sched = 1.5 pu (150 MW)
        // curtailment = 100 MW → net benefit = (60-40)*100 = 2000 $/hr
        let result = LoadDispatchResult::from_solution(&dl, 0.5, 0.0, 60.0, 100.0);
        assert!((result.p_curtailed_pu - 1.0).abs() < 1e-10);
        assert!((result.curtailment_pct - (100.0 / 150.0 * 100.0)).abs() < 1e-6);
        assert!(result.net_curtailment_benefit > 0.0);
    }
}
