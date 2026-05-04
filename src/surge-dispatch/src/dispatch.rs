// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Unified dispatch solver — SCED, SCUC, and all multi-period variants.
//!
//! The canonical entry point is [`solve_dispatch`], which routes a typed
//! [`crate::request::DispatchRequest`] onto the appropriate internal engine:
//!
//! | Dispatch mode | Routing |
//! |---|---|
//! | DC period-by-period dispatch | `Dc + PeriodByPeriod + AllCommitted/Fixed` |
//! | DC time-coupled dispatch | `Dc + TimeCoupled + AllCommitted/Fixed` |
//! | DC commitment optimization | `Dc + TimeCoupled + Optimize/Additional` |
//! | AC period-by-period dispatch | `Ac + PeriodByPeriod + AllCommitted/Fixed` |
//!
//! [`crate::request::DispatchRequest`] is the preferred public API. This file
//! now centers canonical routing, runtime state, and unified dispatch results.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use surge_network::Network;

/// Side-channel for capturing the caller-side wall of
/// `solve_scuc_with_problem_spec` (including the function-return
/// epilogue that fn-local instrumentation can't see) so it can be
/// folded into the phase_timings block after the routing `match`
/// returns its result. AtomicU64 holds the f64 bits. This is a
/// process-global — concurrent solves would race, but the runner
/// is single-threaded per process.
static SCUC_EXTERNAL_WALL_BITS: AtomicU64 = AtomicU64::new(0);
static AC_SCED_THREAD_POOLS: OnceLock<Mutex<HashMap<usize, Arc<rayon::ThreadPool>>>> =
    OnceLock::new();
use surge_network::market::DispatchableLoad;
use surge_opf::AcOpfOptions;
use surge_solution::{ObjectiveBucket, ObjectiveTerm, ObjectiveTermKind, OpfSolution};
use tracing::info;

use crate::common::catalog::DispatchCatalog;
use crate::common::costs::{
    resolve_generator_economics_for_period, storage_offer_curve_cost,
    validate_storage_offer_curve_points,
};
use crate::common::reserves::{
    dispatchable_load_reserve_offer_for_period, generator_reserve_offer_for_period,
};
use crate::common::runtime::{DispatchPeriodContext, SequentialDispatchState};
use crate::common::spec::DispatchProblemSpec;
use crate::economics::{
    aggregate_terms, bucket_total, filter_terms_for_subject, reserve_shortfall_cost,
    resource_energy_cost, resource_no_load_cost, resource_reserve_costs, resource_shutdown_cost,
    resource_startup_cost, sum_terms,
};
use crate::error::ScedError;
use crate::ids::{AreaId, ZoneId};
use crate::model::DispatchModel;
use crate::request::{
    CommitmentPolicyKind, DispatchRequest, DispatchSolveOptions, Formulation,
    NormalizedDispatchRequest, PreparedDispatchRequest, SecurityEmbedding,
};
use crate::result as keyed;
use crate::result::{
    DispatchBus, DispatchDiagnostics, DispatchResource, DispatchResourceKind, DispatchStudy,
    DispatchSummary,
};
use crate::sced::ac::AcScedPeriodArtifacts;
use crate::solution::{
    RawBusPeriodResult, RawConstraintPeriodResult, RawDispatchPeriodResult,
    RawEmissionsPeriodResult, RawFrequencyPeriodResult, RawHvdcPeriodResult,
    RawReservePeriodResult, RawResourcePeriodResult,
};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// How multi-period intervals are coupled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum Horizon {
    /// Solve each interval independently. Thread ramp/SoC state between
    /// intervals sequentially. O(n) individual solves. Greedy — cannot
    /// anticipate future load/price changes.
    #[default]
    Sequential,
    /// Solve all intervals in one monolithic LP/MILP. Ramp and SoC
    /// constraints couple intervals inside the optimization. Globally
    /// optimal inter-temporal dispatch at higher computational cost.
    TimeCoupled,
}

/// How generator commitment is determined.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) enum CommitmentMode {
    /// All in-service generators are committed. Classic SCED.
    /// No commitment variables created. Pure LP/QP.
    #[default]
    AllCommitted,

    /// Commitment schedule provided externally (e.g., from a prior SCUC run,
    /// operator decision, or outage schedule). No commitment variables created.
    Fixed {
        /// Per-generator commitment (true=on). Length = n_gen_in_service.
        commitment: Vec<bool>,
        /// Per-period overrides. When Some, `commitment` is overridden
        /// by `per_period[t]` at period `t`. For modeling planned outages
        /// mid-horizon.
        per_period: Option<Vec<Vec<bool>>>,
    },

    /// Full MILP commitment optimization (= SCUC).
    /// u[g,t], v[g,t], w[g,t] are integer variables.
    Optimize(IndexedCommitmentOptions),

    /// RUC additional commitment: DA commitments are locked on, SCUC can only
    /// commit *additional* units for reliability — never decommit DA-cleared units.
    /// `da_commitment[t][j]` = true means generator j is DA-committed in period t
    /// (in-service order).
    Additional {
        /// Per-period DA commitment schedule. `da_commitment[t][j]` = true forces
        /// u[g,t] = 1 for generator j in period t. Length: n_periods × n_gen_in_service.
        da_commitment: Vec<Vec<bool>>,
        /// Commitment options for the additional units being optimized.
        options: IndexedCommitmentOptions,
    },
}

/// Options specific to commitment optimization (used by Optimize / SCUC).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct IndexedCommitmentOptions {
    /// Initial commitment status (true = on) per generator index.
    /// If None, all generators start committed.
    pub initial_commitment: Option<Vec<bool>>,
    /// Internal sparse mask for `initial_commitment`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) initial_commitment_mask: Option<Vec<bool>>,
    /// Initial hours-on per generator (positive=on, negative=off).
    pub initial_hours_on: Option<Vec<i32>>,
    /// Internal sparse mask for `initial_hours_on`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) initial_hours_on_mask: Option<Vec<bool>>,
    /// Hours offline before horizon start (for startup cost tier selection).
    pub initial_offline_hours: Option<Vec<f64>>,
    /// Internal sparse mask for `initial_offline_hours`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) initial_offline_hours_mask: Option<Vec<bool>>,
    /// MIP solver time limit in seconds.
    pub time_limit_secs: Option<f64>,
    /// Relative MIP optimality gap (e.g. 0.01 = 1%).
    pub mip_rel_gap: Option<f64>,
    /// Optional time-varying MIP gap schedule (piecewise-constant, keyed by
    /// wall-clock seconds). See [`surge_opf::backends::MipGapSchedule`] for
    /// semantics. Solver backends that support progress callbacks (Gurobi
    /// today, HiGHS to follow) tighten `mip_rel_gap` dynamically and
    /// record a per-solve [`surge_opf::backends::MipTrace`]. Backends
    /// without callback support ignore the schedule and fall back to the
    /// static `mip_rel_gap` / `time_limit_secs` safety net.
    pub mip_gap_schedule: Option<Vec<(f64, f64)>>,

    /// When `true`, `solve_problem` skips the whole warm-start pipeline
    /// (`try_build_mip_primal_start`) and hands the SCUC MIP to the
    /// solver cold. Saves the ~6 helper LP/MIP pre-solves on cases the
    /// MIP can nail at the root; default `false` preserves current
    /// warm-start-on behavior.
    pub disable_warm_start: bool,
    /// Optional per-period commitment warm-start hint (true = on).
    pub warm_start_commitment: Option<Vec<Vec<bool>>>,
    /// Internal sparse mask for `warm_start_commitment`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) warm_start_commitment_mask: Option<Vec<bool>>,
    /// Startups that occurred in the 24h before horizon start (per generator).
    /// Reduces the max_starts_per_day budget for early-horizon windows.
    pub initial_starts_24h: Option<Vec<u32>>,
    /// Internal sparse mask for `initial_starts_24h`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) initial_starts_24h_mask: Option<Vec<bool>>,
    /// Startups that occurred in the 168h before horizon start (per generator).
    /// Reduces the max_starts_per_week budget for early-horizon windows.
    pub initial_starts_168h: Option<Vec<u32>>,
    /// Internal sparse mask for `initial_starts_168h`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) initial_starts_168h_mask: Option<Vec<bool>>,
    /// Energy (MWh) consumed in the 24h before horizon start (per generator).
    /// Reduces the max_energy_mwh_per_day budget for early-horizon windows.
    pub initial_energy_mwh_24h: Option<Vec<f64>>,
    /// Internal sparse mask for `initial_energy_mwh_24h`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) initial_energy_mwh_24h_mask: Option<Vec<bool>>,
    /// Legacy SCUC-only alias kept only for local compatibility tests.
    #[cfg(test)]
    pub(crate) n_cost_segments: usize,
    /// Legacy step-size alias kept only for local compatibility tests.
    #[cfg(test)]
    pub(crate) step_size_hours: Option<f64>,
}

#[allow(clippy::derivable_impls)]
impl Default for IndexedCommitmentOptions {
    fn default() -> Self {
        Self {
            initial_commitment: None,
            initial_commitment_mask: None,
            initial_hours_on: None,
            initial_hours_on_mask: None,
            initial_offline_hours: None,
            initial_offline_hours_mask: None,
            time_limit_secs: None,
            mip_rel_gap: None,
            mip_gap_schedule: None,
            disable_warm_start: false,
            warm_start_commitment: None,
            warm_start_commitment_mask: None,
            initial_starts_24h: None,
            initial_starts_24h_mask: None,
            initial_starts_168h: None,
            initial_starts_168h_mask: None,
            initial_energy_mwh_24h: None,
            initial_energy_mwh_24h_mask: None,
            #[cfg(test)]
            n_cost_segments: 5,
            #[cfg(test)]
            step_size_hours: None,
        }
    }
}

fn masked_value_at<T: Copy>(values: Option<&[T]>, mask: Option<&[bool]>, idx: usize) -> Option<T> {
    let values = values?;
    if let Some(mask) = mask
        && !mask.get(idx).copied().unwrap_or(false)
    {
        return None;
    }
    values.get(idx).copied()
}

fn masked_values_present<T>(values: Option<&[T]>, mask: Option<&[bool]>) -> bool {
    match (values, mask) {
        (Some(_), None) => true,
        (Some(_), Some(mask)) => mask.iter().any(|&present| present),
        (None, _) => false,
    }
}

impl IndexedCommitmentOptions {
    pub(crate) fn initial_commitment_at(&self, idx: usize) -> Option<bool> {
        masked_value_at(
            self.initial_commitment.as_deref(),
            self.initial_commitment_mask.as_deref(),
            idx,
        )
    }

    pub(crate) fn initial_hours_on_at(&self, idx: usize) -> Option<i32> {
        masked_value_at(
            self.initial_hours_on.as_deref(),
            self.initial_hours_on_mask.as_deref(),
            idx,
        )
    }

    pub(crate) fn initial_offline_hours_at(&self, idx: usize) -> Option<f64> {
        masked_value_at(
            self.initial_offline_hours.as_deref(),
            self.initial_offline_hours_mask.as_deref(),
            idx,
        )
    }

    pub(crate) fn initial_starts_24h_at(&self, idx: usize) -> Option<u32> {
        masked_value_at(
            self.initial_starts_24h.as_deref(),
            self.initial_starts_24h_mask.as_deref(),
            idx,
        )
    }

    pub(crate) fn warm_start_commitment_at(&self, period: usize, idx: usize) -> Option<bool> {
        let values = self.warm_start_commitment.as_deref()?;
        if let Some(mask) = self.warm_start_commitment_mask.as_deref()
            && !mask.get(idx).copied().unwrap_or(false)
        {
            return None;
        }
        values.get(period).and_then(|row| row.get(idx)).copied()
    }

    pub(crate) fn initial_starts_168h_at(&self, idx: usize) -> Option<u32> {
        masked_value_at(
            self.initial_starts_168h.as_deref(),
            self.initial_starts_168h_mask.as_deref(),
            idx,
        )
    }

    pub(crate) fn initial_energy_mwh_24h_at(&self, idx: usize) -> Option<f64> {
        masked_value_at(
            self.initial_energy_mwh_24h.as_deref(),
            self.initial_energy_mwh_24h_mask.as_deref(),
            idx,
        )
    }
}

/// Initial dispatch state for sequential or horizon-start solves.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct IndexedDispatchInitialState {
    /// Previous-period dispatch in MW (one per in-service generator).
    /// Used for ramp constraints at the first solved period.
    pub prev_dispatch_mw: Option<Vec<f64>>,
    /// Internal sparse mask for `prev_dispatch_mw`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) prev_dispatch_mask: Option<Vec<bool>>,
    /// Previous HVDC dispatch in MW (one per HVDC link).
    /// Used for HVDC ramp constraints at the first solved period.
    pub prev_hvdc_dispatch_mw: Option<Vec<f64>>,
    /// Internal sparse mask for `prev_hvdc_dispatch_mw`; `None` means all entries set.
    #[serde(skip)]
    pub(crate) prev_hvdc_dispatch_mask: Option<Vec<bool>>,
    /// Per-generator SoC override (MWh), keyed by generator index in `network.generators`.
    /// Overrides the storage unit's configured initial SoC.
    pub storage_soc_override: Option<HashMap<usize, f64>>,
}

impl IndexedDispatchInitialState {
    pub(crate) fn has_prev_dispatch(&self) -> bool {
        masked_values_present(
            self.prev_dispatch_mw.as_deref(),
            self.prev_dispatch_mask.as_deref(),
        )
    }

    pub(crate) fn prev_dispatch_at(&self, idx: usize) -> Option<f64> {
        masked_value_at(
            self.prev_dispatch_mw.as_deref(),
            self.prev_dispatch_mask.as_deref(),
            idx,
        )
    }

    #[allow(dead_code)]
    pub(crate) fn has_prev_hvdc_dispatch(&self) -> bool {
        masked_values_present(
            self.prev_hvdc_dispatch_mw.as_deref(),
            self.prev_hvdc_dispatch_mask.as_deref(),
        )
    }

    pub(crate) fn prev_hvdc_dispatch_at(&self, idx: usize) -> Option<f64> {
        masked_value_at(
            self.prev_hvdc_dispatch_mw.as_deref(),
            self.prev_hvdc_dispatch_mask.as_deref(),
            idx,
        )
    }
}

// ---------------------------------------------------------------------------
// IndexedCommitmentConstraint — stability Benders cuts injected into SCUC
// ---------------------------------------------------------------------------

/// A single term in a [`IndexedCommitmentConstraint`]: a generator index paired with
/// its constraint coefficient.
///
/// `gen_index` is the position within the in-service generator list (same
/// ordering as `RawDispatchSolution::commitment`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedCommitmentTerm {
    /// Index into the in-service generator list.
    pub gen_index: usize,
    /// Constraint coefficient for this generator's binary commitment variable.
    pub coeff: f64,
}

// ---------------------------------------------------------------------------
// Pumped-hydro constraint structs
// ---------------------------------------------------------------------------

/// Head-dependent Pmax curve for a pumped-hydro storage unit.
///
/// Breakpoints `(soc_mwh, pmax_mw)` define a piecewise-linear curve.
/// At each SCUC hour, the discharge variable is constrained to lie below the
/// concave envelope of the curve evaluated at that hour's end-of-interval
/// storage SOC state variable. No new binary variables are needed because the
/// Pmax-vs-SOC curve is concave (higher head -> more power): the set of linear
/// tangent-plane constraints forms a valid LP relaxation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedPhHeadCurve {
    /// Global generator index in `Network.generators`.
    pub gen_index: usize,
    /// Piecewise-linear breakpoints sorted by ascending SOC: `(soc_mwh, pmax_mw)`.
    pub breakpoints: Vec<(f64, f64)>,
}

/// Mode-transition constraints for a pumped-hydro storage unit.
///
/// Adds binary mode variables `m_gen[s,t]` and `m_pump[s,t]` to the SCUC LP,
/// linked to the existing `dis[s,t]` and `ch[s,t]` storage variables via Big-M
/// constraints.  Minimum run-time and mode-transition delay constraints prevent
/// operationally infeasible rapid mode switching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedPhModeConstraint {
    /// Global generator index in `Network.generators`.
    pub gen_index: usize,
    /// Minimum consecutive periods in generation mode once started.
    pub min_gen_run_periods: usize,
    /// Minimum consecutive periods in pump mode once started.
    pub min_pump_run_periods: usize,
    /// Minimum idle periods between switching from pump mode to gen mode.
    pub pump_to_gen_periods: usize,
    /// Minimum idle periods between switching from gen mode to pump mode.
    pub gen_to_pump_periods: usize,
    /// Maximum pump starts allowed per day.
    pub max_pump_starts: Option<u32>,
}

// ---------------------------------------------------------------------------
// IndexedCommitmentConstraint — stability Benders cuts injected into SCUC
// ---------------------------------------------------------------------------

/// A linear constraint on commitment binaries, injected by stability-
/// constrained Benders decomposition or other external cut generators.
///
/// Encodes: `Σ terms[i].coeff × u[terms[i].gen_index, hour] >= lower_bound`
///
/// When `penalty_cost` is `Some`, the constraint is soft: a slack variable
/// with the given penalty ($/unit) is added to ensure MILP feasibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedCommitmentConstraint {
    /// Human-readable constraint name (for logging/reporting).
    pub name: String,
    /// Period index this constraint applies to (0-based).
    ///
    /// Period length is controlled by the request `dt_hours`.
    pub period_idx: usize,
    /// Generator-coefficient pairs. `gen_index` is the index into the
    /// in-service generator list.
    pub terms: Vec<IndexedCommitmentTerm>,
    /// Right-hand side: the constraint is `Σ coeff × u[g,t] >= lower_bound`.
    pub lower_bound: f64,
    /// If `Some`, the constraint is soft with this penalty cost per unit of
    /// violation. If `None`, the constraint is hard.
    pub penalty_cost: Option<f64>,
}

/// Absolute startup-count limit for one generator over a solved-period window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedStartupWindowLimit {
    /// Index into the in-service generator list.
    pub gen_index: usize,
    /// Inclusive start period index.
    pub start_period_idx: usize,
    /// Inclusive end period index.
    pub end_period_idx: usize,
    /// Maximum in-window startups.
    pub max_startups: u32,
}

/// Absolute energy budget for one generator over a solved-period window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedEnergyWindowLimit {
    /// Index into the in-service generator list.
    pub gen_index: usize,
    /// Inclusive start period index.
    pub start_period_idx: usize,
    /// Inclusive end period index.
    pub end_period_idx: usize,
    /// Optional minimum in-window energy in MWh.
    pub min_energy_mwh: Option<f64>,
    /// Optional maximum in-window energy in MWh.
    pub max_energy_mwh: Option<f64>,
}

/// Indexed (post-resource-resolution) form of [`crate::request::PeakDemandCharge`].
///
/// One auxiliary `peak_mw ≥ 0` column is allocated per entry, with rows
/// `peak_mw ≥ pg[t]` for every `t ∈ period_indices` and a linear
/// objective term `charge_per_mw * peak_mw`. Used by the SCUC builder
/// to encode coincident-peak demand charges (e.g. ERCOT 4-CP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedPeakDemandCharge {
    /// Caller-supplied identifier — surfaced in diagnostics.
    pub name: String,
    /// Index into the in-service generator list whose dispatch is
    /// being capped by the peak variable. Storage and dispatchable
    /// loads aren't supported (use the generator's `pmin = -charge`
    /// pattern for storage; dispatchable-load peak charges can be
    /// added if there's a use case).
    pub gen_index: usize,
    /// Period indices over which the peak is taken. Validated to be
    /// in `0..n_periods`, non-empty, and de-duplicated.
    pub period_indices: Vec<usize>,
    /// Linear cost coefficient (`$ / MW`) applied to the peak variable.
    pub charge_per_mw: f64,
}

// ---------------------------------------------------------------------------
// RawDispatchSolution — the unified result type
// ---------------------------------------------------------------------------

/// Unified dispatch result.
///
/// Contains per-period dispatch results and optional commitment output.
/// Single-period solves return `periods` of length 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawDispatchSolution {
    /// Study metadata and classification.
    #[serde(default)]
    pub study: DispatchStudy,
    /// Explicit public resource catalog for the solved case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<DispatchResource>,
    /// Explicit public bus catalog for the solved case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buses: Vec<DispatchBus>,
    /// Derived summary totals over the detailed period-level output.
    #[serde(default)]
    pub summary: DispatchSummary,
    /// Solver/process diagnostics kept separate from core market output.
    #[serde(default)]
    pub diagnostics: DispatchDiagnostics,
    /// Per-period dispatch results. Length = `n_periods`.
    pub periods: Vec<RawDispatchPeriodResult>,

    // ── Commitment output (populated when commitment != AllCommitted/Fixed) ──
    /// Commitment schedule: `[t][g]` (true = on). None when commitment was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment: Option<Vec<Vec<bool>>>,
    /// Startup events: `[t][g]`. None when commitment was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<Vec<Vec<bool>>>,
    /// Shutdown events: `[t][g]`. None when commitment was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<Vec<Vec<bool>>>,
    /// Per-generator startup cost across horizon. None when no commitment decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_costs: Option<Vec<f64>>,
    /// Operating cost only (excluding startup/shutdown). None when no commitment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operating_cost: Option<f64>,
    /// Total startup cost. None when no commitment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_cost_total: Option<f64>,
    /// Storage SoC trajectories: `[gen_index][period]` in MWh. Empty when no storage.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub storage_soc: HashMap<usize, Vec<f64>>,
    /// Per-bus DC loss allocation (MW) per period: `[t][bus_idx]`.
    /// Losses are allocated proportional to bus load.
    /// Empty when loss factors are disabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_loss_allocation_mw: Vec<Vec<f64>>,
    /// Bus voltage angles (radians) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_angles_rad: Vec<Vec<f64>>,
    /// Bus voltage magnitudes (per-unit) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_voltage_pu: Vec<Vec<f64>>,
    /// Generator reactive dispatch (MVAr) per period: `[t][g]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generator_q_mvar: Vec<Vec<f64>>,
    /// Per-bus reactive-power balance slack (positive, MVAr) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_q_slack_pos_mvar: Vec<Vec<f64>>,
    /// Per-bus reactive-power balance slack (negative, MVAr) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_q_slack_neg_mvar: Vec<Vec<f64>>,
    /// Per-bus active-power balance slack (positive, MW) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_p_slack_pos_mw: Vec<Vec<f64>>,
    /// Per-bus active-power balance slack (negative, MW) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_p_slack_neg_mw: Vec<Vec<f64>>,
    /// Per-branch AC thermal slack from-side (MVA) per period: `[t][branch_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thermal_limit_slack_from_mva: Vec<Vec<f64>>,
    /// Per-branch AC thermal slack to-side (MVA) per period: `[t][branch_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thermal_limit_slack_to_mva: Vec<Vec<f64>>,
    /// Per-bus voltage-magnitude high slack (pu) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_vm_slack_high_pu: Vec<Vec<f64>>,
    /// Per-bus voltage-magnitude low slack (pu) per period: `[t][bus_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_vm_slack_low_pu: Vec<Vec<f64>>,
    /// Per-branch angle-difference high slack (rad) per period: `[t][branch_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_diff_slack_high_rad: Vec<Vec<f64>>,
    /// Per-branch angle-difference low slack (rad) per period: `[t][branch_idx]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_diff_slack_low_rad: Vec<Vec<f64>>,
    /// AC penalty rates for computing penalty_dollars on AC slack results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_p_balance_penalty_per_mw: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_q_balance_penalty_per_mvar: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_thermal_penalty_per_mva: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_voltage_penalty_per_pu: Option<f64>,
    /// AC penalty rate for angle-difference slacks ($/rad).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_angle_penalty_per_rad: Option<f64>,
    /// System inertia H (seconds) at the dispatch point when computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_inertia_s: Option<f64>,
    /// Estimated initial RoCoF (Hz/s) for the configured event when computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_rocof_hz_per_s: Option<f64>,
    /// Whether configured frequency-security constraints are satisfied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_secure: Option<bool>,
    /// CO2 shadow price in $/MWh per in-service generator.
    /// Empty when no carbon price is active or the formulation does not compute it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub co2_shadow_price: Vec<f64>,
    /// Regulation mode per hour: `[t][g]` (true = in regulation mode).
    /// Empty when no regulation products are active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regulation: Vec<Vec<bool>>,
    /// Cleared branch commitment state `[t][branch_local_idx]` per
    /// period for AC branches. Populated by the SCUC extraction path
    /// when `allow_branch_switching = true`; empty otherwise (the LP
    /// bounds pin every branch to its static `in_service` flag). The
    /// security loop reads this to run the connectivity check against
    /// the LP's actual switching pattern rather than the static
    /// topology.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branch_commitment_state: Vec<Vec<bool>>,
    /// Combined cycle configuration schedule: `cc_config_schedule[t][p]` is the
    /// active configuration name for CC plant `p` at hour `t`, or `None` if offline.
    /// Empty when no CC plants exist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc_config_schedule: Vec<Vec<Option<String>>>,
    /// Total combined cycle transition cost across all hours.
    #[serde(default)]
    pub cc_transition_cost: f64,
    /// Per-plant combined cycle transition costs across the horizon.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc_transition_costs: Vec<f64>,

    /// Model diagnostic snapshots captured during solve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_diagnostics: Vec<crate::model_diagnostic::ModelDiagnostic>,

    /// Augmented flowgate name list — set by the security loop when
    /// the inner SCUC network has more flowgates than the caller's
    /// `network` (because security cuts get added per iteration).
    /// Indexed by the inner network's flowgate position so that
    /// `aux_flowgate_names[idx]` aligns with
    /// `period.flowgate_shadow_prices[idx]`. Empty when the security
    /// loop did not augment the network (so caller can use
    /// `network.flowgates[idx].name` directly).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aux_flowgate_names: Vec<String>,

    /// Final loss-factor state from the SCUC refinement iteration —
    /// `dloss_dp[t][bus]` and `total_losses_mw[t]`. Internal plumbing
    /// used by the security loop to warm-start the next iteration's
    /// SCUC solve with the converged loss sensitivities (skipping
    /// the lossless-MIP pass). `#[serde(skip)]` because it's runtime
    /// state, not a persisted solution artifact; downstream consumers
    /// use `bus_loss_allocation_mw` for the user-facing loss data.
    #[serde(skip)]
    pub scuc_final_loss_warm_start: Option<crate::scuc::losses::LossFactorWarmStart>,
}

// ---------------------------------------------------------------------------
// Result conversion helpers
// ---------------------------------------------------------------------------

impl RawDispatchSolution {
    /// Create a dispatch-only result (no commitment decision).
    fn dispatch_only(
        periods: Vec<RawDispatchPeriodResult>,
        total_cost: f64,
        solve_time_secs: f64,
        iterations: u32,
        storage_soc: HashMap<usize, Vec<f64>>,
    ) -> Self {
        Self {
            study: DispatchStudy::default(),
            resources: Vec::new(),
            buses: Vec::new(),
            summary: DispatchSummary {
                total_cost,
                ..Default::default()
            },
            diagnostics: DispatchDiagnostics {
                ac_sced_period_timings: Vec::new(),
                iterations,
                solve_time_secs,
                ..Default::default()
            },
            periods,
            commitment: None,
            startup: None,
            shutdown: None,
            startup_costs: None,
            operating_cost: None,
            startup_cost_total: None,
            storage_soc,
            bus_angles_rad: Vec::new(),
            bus_voltage_pu: Vec::new(),
            generator_q_mvar: Vec::new(),
            bus_q_slack_pos_mvar: Vec::new(),
            bus_q_slack_neg_mvar: Vec::new(),
            bus_p_slack_pos_mw: Vec::new(),
            bus_p_slack_neg_mw: Vec::new(),
            thermal_limit_slack_from_mva: Vec::new(),
            thermal_limit_slack_to_mva: Vec::new(),
            bus_vm_slack_high_pu: Vec::new(),
            bus_vm_slack_low_pu: Vec::new(),
            angle_diff_slack_high_rad: Vec::new(),
            angle_diff_slack_low_rad: Vec::new(),
            ac_p_balance_penalty_per_mw: None,
            ac_q_balance_penalty_per_mvar: None,
            ac_thermal_penalty_per_mva: None,
            ac_voltage_penalty_per_pu: None,
            ac_angle_penalty_per_rad: None,
            system_inertia_s: None,
            estimated_rocof_hz_per_s: None,
            frequency_secure: None,
            co2_shadow_price: Vec::new(),
            regulation: Vec::new(),
            branch_commitment_state: Vec::new(),
            cc_config_schedule: Vec::new(),
            cc_transition_cost: 0.0,
            cc_transition_costs: Vec::new(),
            model_diagnostics: Vec::new(),
            aux_flowgate_names: Vec::new(),
            bus_loss_allocation_mw: Vec::new(),
            scuc_final_loss_warm_start: None,
        }
    }
}

fn default_machine_id(machine_id: Option<&str>) -> String {
    machine_id.unwrap_or("1").to_string()
}

fn dispatchable_load_resource_id(
    dispatchable_load: &DispatchableLoad,
    source_index: usize,
) -> String {
    if dispatchable_load.resource_id.is_empty() {
        format!("dl:{}:{source_index}", dispatchable_load.bus)
    } else {
        dispatchable_load.resource_id.clone()
    }
}

pub(crate) fn hvdc_link_id(link: &crate::hvdc::HvdcDispatchLink, source_index: usize) -> String {
    if !link.id.is_empty() {
        link.id.clone()
    } else if !link.name.is_empty() {
        link.name.clone()
    } else {
        format!("hvdc:{source_index}")
    }
}

pub(crate) fn build_bus_catalog(network: &Network) -> Vec<DispatchBus> {
    network
        .buses
        .iter()
        .map(|bus| DispatchBus {
            bus_number: bus.number,
            name: bus.name.clone(),
            area: AreaId::from(bus.area),
            zone: ZoneId::from(bus.zone),
        })
        .collect()
}

fn zonal_requirement_mw_for_period(
    requirement: &surge_network::market::ZonalReserveRequirement,
    period_idx: usize,
    period: &RawDispatchPeriodResult,
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
    active_dispatchable_loads: &[(usize, &DispatchableLoad)],
    in_service_gen_bus_numbers: &[u32],
    participant_set: Option<&HashSet<u32>>,
) -> f64 {
    // Fast bus-match helpers that use the precomputed `participant_set`
    // (O(1) contains) in preference to `requirement.participant_bus_numbers`
    // (O(N) linear scan). When the requirement has no explicit participant
    // list, both variants fall back to the same `zone_id == study_area`
    // check used by `zonal_participant_bus_matches`.
    let bus_matches_gen_bus = |bus_number: u32, fallback_area: Option<usize>| -> bool {
        match participant_set {
            Some(set) => set.contains(&bus_number),
            None => fallback_area.unwrap_or(0) == requirement.zone_id,
        }
    };
    let bus_matches_dl_bus = |bus_number: u32| -> bool {
        match participant_set {
            Some(set) => set.contains(&bus_number),
            None => {
                let fallback_area =
                    crate::common::network::study_area_for_bus(network, problem_spec, bus_number);
                fallback_area.unwrap_or(0) == requirement.zone_id
            }
        }
    };

    let base_requirement_mw = requirement.requirement_mw_for_period(period_idx);
    let served_dispatchable_load_mw = requirement
        .served_dispatchable_load_coefficient
        .map(|coeff| {
            coeff
                * period
                    .dr_results
                    .loads
                    .iter()
                    .enumerate()
                    .filter_map(|(k, load_result)| {
                        active_dispatchable_loads
                            .get(k)
                            .filter(|(_, dl)| bus_matches_dl_bus(dl.bus))
                            .map(|_| load_result.p_served_pu * network.base_mva)
                    })
                    .sum::<f64>()
        })
        .unwrap_or(0.0);
    let largest_generator_dispatch_mw = requirement
        .largest_generator_dispatch_coefficient
        .map(|coeff| {
            coeff
                * period
                    .pg_mw
                    .iter()
                    .enumerate()
                    .filter_map(|(j, pg_mw)| {
                        bus_matches_gen_bus(
                            in_service_gen_bus_numbers
                                .get(j)
                                .copied()
                                .unwrap_or_default(),
                            problem_spec.generator_area.get(j).copied(),
                        )
                        .then_some((*pg_mw).max(0.0))
                    })
                    .fold(0.0, f64::max)
        })
        .unwrap_or(0.0);

    (base_requirement_mw + served_dispatchable_load_mw + largest_generator_dispatch_mw).max(0.0)
}

fn reserve_balance_product_ids<'a>(
    product_id: &'a str,
    reserve_products_by_id: &HashMap<&'a str, &'a surge_network::market::ReserveProduct>,
) -> Vec<&'a str> {
    let mut ids = vec![product_id];
    if let Some(product) = reserve_products_by_id.get(product_id) {
        for dep in &product.balance_products {
            let dep_id = dep.as_str();
            if !ids.contains(&dep_id) {
                ids.push(dep_id);
            }
        }
    }
    ids
}

fn system_balance_requirement_mw_for_period(
    product_id: &str,
    period_idx: usize,
    problem_spec: &DispatchProblemSpec<'_>,
    reserve_products_by_id: &HashMap<&str, &surge_network::market::ReserveProduct>,
) -> f64 {
    reserve_balance_product_ids(product_id, reserve_products_by_id)
        .into_iter()
        .map(|balance_product_id| {
            problem_spec
                .system_reserve_requirements
                .iter()
                .filter(|requirement| requirement.product_id == balance_product_id)
                .map(|requirement| requirement.requirement_mw_for_period(period_idx))
                .sum::<f64>()
        })
        .sum::<f64>()
}

fn zonal_balance_requirement_mw_for_period(
    product_id: &str,
    zone_id: usize,
    period_idx: usize,
    period: &RawDispatchPeriodResult,
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
    active_dispatchable_loads: &[(usize, &DispatchableLoad)],
    in_service_gen_bus_numbers: &[u32],
    reserve_products_by_id: &HashMap<&str, &surge_network::market::ReserveProduct>,
    zonal_participant_sets: &[Option<HashSet<u32>>],
) -> f64 {
    reserve_balance_product_ids(product_id, reserve_products_by_id)
        .into_iter()
        .map(|balance_product_id| {
            problem_spec
                .zonal_reserve_requirements
                .iter()
                .enumerate()
                .filter(|(_, requirement)| {
                    requirement.product_id == balance_product_id && requirement.zone_id == zone_id
                })
                .map(|(req_idx, requirement)| {
                    let participant_set =
                        zonal_participant_sets.get(req_idx).and_then(|s| s.as_ref());
                    zonal_requirement_mw_for_period(
                        requirement,
                        period_idx,
                        period,
                        network,
                        problem_spec,
                        active_dispatchable_loads,
                        in_service_gen_bus_numbers,
                        participant_set,
                    )
                })
                .sum::<f64>()
        })
        .sum::<f64>()
}

fn commitment_policy_kind(commitment: &CommitmentMode) -> CommitmentPolicyKind {
    match commitment {
        CommitmentMode::AllCommitted => CommitmentPolicyKind::AllCommitted,
        CommitmentMode::Fixed { .. } => CommitmentPolicyKind::Fixed,
        CommitmentMode::Optimize(_) => CommitmentPolicyKind::Optimize,
        CommitmentMode::Additional { .. } => CommitmentPolicyKind::Additional,
    }
}

fn attach_public_catalogs_and_solve_metadata(
    result: &mut RawDispatchSolution,
    network: &Network,
    normalized: &NormalizedDispatchRequest,
) {
    result.study = DispatchStudy {
        formulation: normalized.formulation,
        coupling: normalized.coupling,
        commitment: commitment_policy_kind(&normalized.commitment),
        periods: result.periods.len(),
        security_enabled: normalized.security.is_some(),
        stage: None,
    };
    result.resources =
        crate::report_ids::build_resource_catalog(network, &normalized.input.dispatchable_loads);
    result.buses = build_bus_catalog(network);
}

fn commitment_source_for_period(
    normalized: &NormalizedDispatchRequest,
    period: usize,
    gen_index: usize,
) -> Option<keyed::CommitmentSource> {
    Some(match &normalized.commitment {
        CommitmentMode::AllCommitted => keyed::CommitmentSource::AllCommitted,
        CommitmentMode::Fixed { .. } => keyed::CommitmentSource::Fixed,
        CommitmentMode::Optimize(_) => keyed::CommitmentSource::Optimized,
        CommitmentMode::Additional { da_commitment, .. } => {
            if da_commitment
                .get(period)
                .and_then(|row| row.get(gen_index))
                .copied()
                .unwrap_or(false)
            {
                keyed::CommitmentSource::DayAhead
            } else {
                keyed::CommitmentSource::Additional
            }
        }
    })
}

fn commitment_state_matrix(
    result: &RawDispatchSolution,
    normalized: &NormalizedDispatchRequest,
    n_in_service_gens: usize,
) -> Vec<Vec<Option<bool>>> {
    match &normalized.commitment {
        CommitmentMode::AllCommitted => {
            vec![vec![Some(true); n_in_service_gens]; result.study.periods]
        }
        CommitmentMode::Fixed {
            commitment,
            per_period,
        } => (0..result.study.periods)
            .map(|t| {
                per_period
                    .as_ref()
                    .and_then(|rows| rows.get(t))
                    .unwrap_or(commitment)
                    .iter()
                    .copied()
                    .map(Some)
                    .collect()
            })
            .collect(),
        CommitmentMode::Optimize(_) | CommitmentMode::Additional { .. } => result
            .commitment
            .clone()
            .map(|rows| {
                rows.into_iter()
                    .map(|row| row.into_iter().map(Some).collect())
                    .collect()
            })
            .unwrap_or_else(|| vec![vec![None; n_in_service_gens]; result.study.periods]),
    }
}

fn derived_transition_matrix(
    commitment: &[Vec<Option<bool>>],
    explicit: Option<&[Vec<bool>]>,
    initial_commitment_at: impl Fn(usize) -> bool,
    predicate: impl Fn(bool, bool) -> bool,
) -> Vec<Vec<Option<bool>>> {
    if let Some(explicit_rows) = explicit {
        return explicit_rows
            .iter()
            .map(|row| row.iter().copied().map(Some).collect())
            .collect();
    }
    commitment
        .iter()
        .enumerate()
        .map(|(t, row)| {
            row.iter()
                .enumerate()
                .map(|(j, &curr)| {
                    if t == 0 {
                        curr.map(|curr| predicate(initial_commitment_at(j), curr))
                    } else {
                        match (commitment[t - 1][j], curr) {
                            (Some(prev), Some(curr)) => Some(predicate(prev, curr)),
                            _ => None,
                        }
                    }
                })
                .collect()
        })
        .collect()
}

fn effective_gen_co2_rates_by_global_index(
    network: &Network,
    normalized: &NormalizedDispatchRequest,
    catalog: &DispatchCatalog,
) -> HashMap<usize, f64> {
    let mut rates = HashMap::new();
    for &gi in &catalog.in_service_gen_indices {
        let generator = &network.generators[gi];
        let local_gen_idx = catalog
            .local_gen_index(gi)
            .expect("in-service generator should have a local catalog index");
        let rate = normalized
            .input
            .emission_profile
            .as_ref()
            .map(|profile| profile.rate_for(local_gen_idx))
            .unwrap_or(
                generator
                    .fuel
                    .as_ref()
                    .map(|f| f.emission_rates.co2)
                    .unwrap_or(0.0),
            );
        rates.insert(gi, rate);
    }
    rates
}

fn effective_operating_costs_for_period(
    generator: &surge_network::network::Generator,
    gen_index: usize,
    period: usize,
    power_mw: f64,
    committed: bool,
    spec: &DispatchProblemSpec<'_>,
    is_scuc_route: bool,
) -> (Option<f64>, Option<f64>) {
    let mut offer_cost_buf = None;
    let cost = crate::common::costs::resolve_cost_for_period_from_spec(
        gen_index,
        period,
        generator,
        spec,
        &mut offer_cost_buf,
        Some(generator.pmax),
    );
    if !committed {
        return (Some(0.0), None);
    }
    if spec.is_block_mode() {
        let blocks = crate::common::blocks::build_dispatch_blocks(generator);
        let fills = crate::common::blocks::decompose_into_blocks(power_mw, generator.pmin, &blocks);
        let incremental_cost: f64 = fills
            .iter()
            .zip(blocks.iter())
            .map(|(fill_mw, block)| fill_mw * block.marginal_cost)
            .sum();
        return (
            Some(incremental_cost.max(0.0)),
            Some(cost.evaluate(generator.pmin.max(0.0)).max(0.0)),
        );
    }
    match cost {
        surge_network::market::CostCurve::Polynomial { coeffs, .. } => {
            let no_load = coeffs.last().copied().unwrap_or(0.0);
            if is_scuc_route
                && coeffs.len() >= 3
                && !(spec.use_pwl_generator_costs() && cost.is_convex())
            {
                return (Some(coeffs[1] * power_mw.max(0.0)), Some(no_load));
            }
            if is_scuc_route && coeffs.len() == 2 {
                return (Some(coeffs[0] * power_mw.max(0.0)), Some(no_load));
            }
            if is_scuc_route && coeffs.len() == 1 {
                return (Some(0.0), Some(no_load));
            }
            let total = cost.evaluate(power_mw.max(0.0));
            (Some((total - no_load).max(0.0)), Some(no_load))
        }
        surge_network::market::CostCurve::PiecewiseLinear { points, .. } => {
            let no_load = points.first().map(|(_, total)| *total).unwrap_or(0.0);
            let total = cost.evaluate(power_mw.max(0.0));
            (Some((total - no_load).max(0.0)), Some(no_load))
        }
    }
}

fn storage_operating_cost_for_period(
    generator: &surge_network::network::Generator,
    power_mw: f64,
) -> Option<f64> {
    let storage = generator.storage.as_ref()?;
    Some(match storage.dispatch_mode {
        surge_network::network::StorageDispatchMode::CostMinimization => {
            if power_mw >= 0.0 {
                power_mw * (storage.variable_cost_per_mwh + storage.degradation_cost_per_mwh)
            } else {
                (-power_mw) * storage.degradation_cost_per_mwh
            }
        }
        surge_network::network::StorageDispatchMode::OfferCurve => {
            if power_mw > 0.0 {
                storage
                    .discharge_offer
                    .as_deref()
                    .map(|points| storage_offer_curve_cost(points, power_mw))
                    .unwrap_or(0.0)
            } else if power_mw < 0.0 {
                storage
                    .charge_bid
                    .as_deref()
                    .map(|points| storage_offer_curve_cost(points, -power_mw))
                    .unwrap_or(0.0)
            } else {
                0.0
            }
        }
        surge_network::network::StorageDispatchMode::SelfSchedule => {
            if power_mw > 0.0 {
                storage
                    .discharge_offer
                    .as_deref()
                    .map(|points| storage_offer_curve_cost(points, power_mw))
                    .unwrap_or(0.0)
            } else if power_mw < 0.0 {
                storage
                    .charge_bid
                    .as_deref()
                    .map(|points| storage_offer_curve_cost(points, -power_mw))
                    .unwrap_or(0.0)
            } else {
                0.0
            }
        }
    })
}

fn startup_costs_by_period_and_generator(
    network: &Network,
    catalog: &DispatchCatalog,
    startup: &[Vec<Option<bool>>],
    commitment: &[Vec<Option<bool>>],
    problem_spec: &DispatchProblemSpec<'_>,
) -> Vec<Vec<Option<f64>>> {
    let n_periods = startup.len();
    let n_gens = catalog.in_service_gen_indices.len();
    let mut startup_costs = vec![vec![None; n_gens]; n_periods];
    let mut offline_hours_before_period: Vec<f64> = catalog
        .in_service_gen_indices
        .iter()
        .enumerate()
        .map(|(local_gen_idx, _)| {
            let initially_on = problem_spec
                .initial_commitment_at(local_gen_idx)
                .unwrap_or(true);
            if initially_on {
                0.0
            } else {
                problem_spec
                    .initial_offline_hours_at(local_gen_idx)
                    .unwrap_or(f64::INFINITY)
            }
        })
        .collect();

    for (t, startup_costs_t) in startup_costs.iter_mut().enumerate().take(n_periods) {
        for (local_gen_idx, &global_gen_idx) in catalog.in_service_gen_indices.iter().enumerate() {
            let did_start = startup
                .get(t)
                .and_then(|row| row.get(local_gen_idx))
                .copied()
                .flatten()
                .unwrap_or(false);
            let is_committed = commitment
                .get(t)
                .and_then(|row| row.get(local_gen_idx))
                .copied()
                .flatten()
                .unwrap_or(false);

            if did_start {
                let generator = &network.generators[global_gen_idx];
                if let Some(economics) = resolve_generator_economics_for_period(
                    global_gen_idx,
                    t,
                    generator,
                    problem_spec.offer_schedules,
                    Some(generator.pmax),
                ) {
                    startup_costs_t[local_gen_idx] =
                        Some(economics.startup_cost_for_offline_hours(
                            offline_hours_before_period[local_gen_idx],
                        ));
                }
            }

            if is_committed {
                offline_hours_before_period[local_gen_idx] = 0.0;
            } else {
                offline_hours_before_period[local_gen_idx] += problem_spec.period_hours(t);
            }
        }
    }

    startup_costs
}

fn hvdc_delivered_mw(link: &crate::hvdc::HvdcDispatchLink, total_mw: f64, bands_mw: &[f64]) -> f64 {
    if total_mw.abs() < 1e-9 {
        return 0.0;
    }
    if !bands_mw.is_empty() && bands_mw.len() == link.bands.len() {
        let delivered: f64 = bands_mw
            .iter()
            .zip(link.bands.iter())
            .map(|(mw, band)| mw * (1.0 - band.loss_b_frac))
            .sum();
        delivered - total_mw.signum() * link.loss_a_mw
    } else {
        total_mw * (1.0 - link.loss_b_frac) - total_mw.signum() * link.loss_a_mw
    }
}

fn derive_par_results_for_period(
    network: &Network,
    normalized: &NormalizedDispatchRequest,
    period_angles: Option<&[f64]>,
) -> Vec<surge_solution::ParResult> {
    let Some(period_angles) = period_angles else {
        return Vec::new();
    };
    if normalized.input.par_setpoints.is_empty() {
        return Vec::new();
    }
    let bus_index_by_number: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(idx, bus)| (bus.number, idx))
        .collect();

    normalized
        .input
        .par_setpoints
        .iter()
        .map(|setpoint| {
            let maybe_result = network.branches.iter().find(|branch| {
                branch.in_service
                    && branch.from_bus == setpoint.from_bus
                    && branch.to_bus == setpoint.to_bus
                    && branch.circuit == setpoint.circuit
            });
            if let Some(branch) = maybe_result
                && let (Some(&from_idx), Some(&to_idx)) = (
                    bus_index_by_number.get(&setpoint.from_bus),
                    bus_index_by_number.get(&setpoint.to_bus),
                )
            {
                let b_dc = branch.b_dc();
                let implied_shift_rad = if b_dc.abs() > 1e-20 {
                    period_angles[from_idx]
                        - period_angles[to_idx]
                        - setpoint.target_mw / (network.base_mva * b_dc)
                } else {
                    0.0
                };
                let implied_shift_deg = implied_shift_rad.to_degrees();
                let opf_ctrl = branch.opf_control.as_ref();
                let phase_min_deg = opf_ctrl
                    .map(|c| c.phase_min_rad.to_degrees())
                    .unwrap_or(-30.0);
                let phase_max_deg = opf_ctrl
                    .map(|c| c.phase_max_rad.to_degrees())
                    .unwrap_or(30.0);
                let within_limits =
                    implied_shift_deg >= phase_min_deg && implied_shift_deg <= phase_max_deg;
                surge_solution::ParResult {
                    from_bus: setpoint.from_bus,
                    to_bus: setpoint.to_bus,
                    circuit: setpoint.circuit.clone(),
                    target_mw: setpoint.target_mw,
                    implied_shift_deg,
                    within_limits,
                }
            } else {
                surge_solution::ParResult {
                    from_bus: setpoint.from_bus,
                    to_bus: setpoint.to_bus,
                    circuit: setpoint.circuit.clone(),
                    target_mw: setpoint.target_mw,
                    implied_shift_deg: 0.0,
                    within_limits: false,
                }
            }
        })
        .collect()
}

fn attach_keyed_period_views(
    result: &mut RawDispatchSolution,
    network: &Network,
    normalized: &NormalizedDispatchRequest,
) {
    let catalog = DispatchCatalog::from_network(network, &normalized.input.dispatchable_loads);
    let in_service_gens: Vec<(usize, &surge_network::network::Generator)> = catalog
        .in_service_gen_indices
        .iter()
        .map(|&gi| (gi, &network.generators[gi]))
        .collect();
    let n_in_service_gens = catalog.in_service_gen_indices.len();
    let commitment = commitment_state_matrix(result, normalized, n_in_service_gens);
    let problem_spec = DispatchProblemSpec::from_request(&normalized.input, &normalized.commitment);
    let startup = derived_transition_matrix(
        &commitment,
        result.startup.as_deref(),
        |j| problem_spec.initial_commitment_at(j).unwrap_or(true),
        |prev, curr| !prev && curr,
    );
    let shutdown = derived_transition_matrix(
        &commitment,
        result.shutdown.as_deref(),
        |j| problem_spec.initial_commitment_at(j).unwrap_or(true),
        |prev, curr| prev && !curr,
    );
    let gen_resource_ids: Vec<String> = in_service_gens
        .iter()
        .map(|(_, generator)| {
            if generator.id.is_empty() {
                let machine_id = default_machine_id(generator.machine_id.as_deref());
                if generator.storage.is_some() {
                    format!("storage:{}:{machine_id}", generator.bus)
                } else {
                    format!("gen:{}:{machine_id}", generator.bus)
                }
            } else {
                generator.id.clone()
            }
        })
        .collect();
    let in_service_gen_bus_numbers: Vec<u32> = in_service_gens
        .iter()
        .map(|(_, generator)| generator.bus)
        .collect();
    let active_dispatchable_loads: Vec<(usize, &surge_network::market::DispatchableLoad)> = catalog
        .active_dispatchable_load_indices
        .iter()
        .map(|&idx| (idx, &normalized.input.dispatchable_loads[idx]))
        .collect();
    let dl_resource_ids: Vec<String> = active_dispatchable_loads
        .iter()
        .map(|(idx, dl)| dispatchable_load_resource_id(dl, *idx))
        .collect();
    let gen_co2_rates = effective_gen_co2_rates_by_global_index(network, normalized, &catalog);
    let is_scuc_route = matches!(normalized.formulation, Formulation::Dc)
        && matches!(normalized.horizon, Horizon::TimeCoupled);
    let startup_costs = startup_costs_by_period_and_generator(
        network,
        &catalog,
        &startup,
        &commitment,
        &problem_spec,
    );
    let reserve_products_by_id: HashMap<&str, &surge_network::market::ReserveProduct> = normalized
        .input
        .reserve_products
        .iter()
        .map(|product| (product.id.as_str(), product))
        .collect();
    // Precompute HashSet<u32> views of each zonal reserve requirement's
    // `participant_bus_numbers`. The inner loops in the zonal reserve
    // result builder (`provided_mw`, `zonal_requirement_mw_for_period`)
    // run `participants.contains(&bus_number)` — O(P_zone) per check —
    // against every gen and every DL, for every zone, every period.
    // With 4224-bus scenarios this was ~112s of pure overhead. Hoisting
    // the conversion to HashSets collapses the per-check cost to O(1).
    let zonal_participant_sets: Vec<Option<HashSet<u32>>> = normalized
        .input
        .zonal_reserve_requirements
        .iter()
        .map(|req| {
            req.participant_bus_numbers
                .as_ref()
                .map(|p| p.iter().copied().collect::<HashSet<u32>>())
        })
        .collect();
    let mut total_energy_cost = 0.0;
    let mut total_reserve_cost = 0.0;
    let mut total_no_load_cost = 0.0;
    let mut total_startup_cost = 0.0;
    let mut startup_costs_by_generator = vec![0.0; n_in_service_gens];

    // Per-section timing accumulators for `attach_keyed_period_views`
    // sub-phase instrumentation. See the `info!` at the end of the
    // period loop for where these are reported.
    let mut akpv_gen_secs = 0.0_f64;
    let mut akpv_dl_secs = 0.0_f64;
    let mut akpv_reserve_secs = 0.0_f64;
    let mut akpv_branch_secs = 0.0_f64;
    let mut akpv_flowgate_secs = 0.0_f64;
    let mut akpv_interface_secs = 0.0_f64;
    let mut akpv_reserve_build_secs = 0.0_f64;
    let mut akpv_reserve_system_secs = 0.0_f64;
    let mut akpv_reserve_zonal_secs = 0.0_f64;
    let mut akpv_post_reserve_secs = 0.0_f64;

    // Pre-compute constraint_id strings for the thermal/flowgate/
    // interface shadow-price loops. Previously these were built inline
    // via `network.branches.iter().filter(...).nth(idx)` per iter —
    // an O(N²) trap that accounted for ~90s on 4224-bus SCUC (more
    // than half the non-MIP overhead). Hoisting them outside the
    // period loop reduces the work from O(N_periods × N² ) to O(N).
    let branch_constraint_ids: Vec<String> = network
        .branches
        .iter()
        .filter(|branch| branch.in_service && branch.rating_a_mva >= normalized.input.min_rate_a)
        .map(|branch| {
            format!(
                "branch:{}:{}:{}",
                branch.from_bus, branch.to_bus, branch.circuit
            )
        })
        .collect();
    // ``period.flowgate_shadow_prices`` and ``period.flowgate_*_slack``
    // are sized to the inner LP network's flowgate list. When the
    // security loop augmented the caller's network with N-1 cuts,
    // those cuts won't appear in the outer ``network.flowgates`` —
    // the loop attaches the augmented name list as
    // ``result.aux_flowgate_names`` so the constraint IDs stay
    // meaningful (``N1_t{period}_{ctg_from}_{ctg_to}_{mon_from}_{mon_to}``
    // or ``HVDC_N1_t...``). Falls back to the unfiltered caller-network
    // names for non-security paths.
    let flowgate_constraint_ids: Vec<String> = if !result.aux_flowgate_names.is_empty() {
        result.aux_flowgate_names.clone()
    } else {
        network.flowgates.iter().map(|fg| fg.name.clone()).collect()
    };

    for (t, period) in result.periods.iter_mut().enumerate() {
        let period_spec = problem_spec.period(t);
        let dt_h = period_spec.interval_hours();
        if period.par_results.is_empty() {
            period.par_results = derive_par_results_for_period(
                network,
                normalized,
                result.bus_angles_rad.get(t).map(Vec::as_slice),
            );
        }
        let mut resource_results = Vec::new();
        let mut bus_injection_mw: HashMap<u32, f64> = HashMap::new();
        let mut bus_withdrawals_mw =
            crate::common::profiles::fixed_bus_withdrawals_mw(network, &problem_spec, t);
        let mut bus_q_injection_mvar: HashMap<u32, f64> = HashMap::new();
        let mut bus_withdrawals_q_mvar =
            crate::common::profiles::fixed_bus_withdrawals_mvar(network, &problem_spec, t);
        let mut emissions_by_resource = HashMap::new();
        let reactive_results_available = result
            .generator_q_mvar
            .get(t)
            .is_some_and(|values| !values.is_empty())
            || result
                .bus_voltage_pu
                .get(t)
                .is_some_and(|values| !values.is_empty());

        let _akpv_gen_t0 = std::time::Instant::now();
        for (j, ((gi, generator), resource_id)) in in_service_gens
            .iter()
            .zip(gen_resource_ids.iter())
            .enumerate()
        {
            let reserve_awards: HashMap<String, f64> = period
                .reserve_awards
                .iter()
                .filter_map(|(product_id, awards)| {
                    awards
                        .get(j)
                        .copied()
                        .map(|award| (product_id.clone(), award))
                })
                .collect();
            let reserve_costs: HashMap<String, f64> = reserve_awards
                .iter()
                .filter_map(|(product_id, &award)| {
                    let offer_cost = generator_reserve_offer_for_period(
                        &problem_spec,
                        *gi,
                        generator,
                        product_id,
                        t,
                    )?
                    .cost_per_mwh;
                    Some((product_id.clone(), offer_cost * award))
                })
                .collect();
            let committed_state = commitment
                .get(t)
                .and_then(|row| row.get(j))
                .copied()
                .flatten();
            let startup_cost = startup_costs
                .get(t)
                .and_then(|row| row.get(j))
                .copied()
                .flatten();
            let (charge_mw, discharge_mw, storage_soc_mwh) = if generator.storage.is_some() {
                let storage_local = catalog
                    .local_storage_index(*gi)
                    .expect("storage generator should have a local storage catalog index");
                let charge_mw = period
                    .storage_charge_mw
                    .get(storage_local)
                    .copied()
                    .unwrap_or(0.0);
                let discharge_mw = period
                    .storage_discharge_mw
                    .get(storage_local)
                    .copied()
                    .unwrap_or(0.0);
                let storage_soc_mwh = period.storage_soc_mwh.get(storage_local).copied();
                (Some(charge_mw), Some(discharge_mw), storage_soc_mwh)
            } else {
                (None, None, None)
            };
            let power_mw = if generator.storage.is_some() {
                discharge_mw.unwrap_or(0.0) - charge_mw.unwrap_or(0.0)
            } else {
                period.pg_mw.get(j).copied().unwrap_or(0.0)
            };
            let (energy_cost, no_load_cost) = if generator.storage.is_some() {
                (storage_operating_cost_for_period(generator, power_mw), None)
            } else {
                effective_operating_costs_for_period(
                    generator,
                    *gi,
                    t,
                    power_mw.max(0.0),
                    committed_state.unwrap_or(power_mw.abs() > 1e-9),
                    &problem_spec,
                    is_scuc_route,
                )
            };
            *bus_injection_mw.entry(generator.bus).or_insert(0.0) += power_mw;
            let q_mvar = result
                .generator_q_mvar
                .get(t)
                .and_then(|row| row.get(j))
                .copied();
            if let Some(q_mvar) = q_mvar {
                *bus_q_injection_mvar.entry(generator.bus).or_insert(0.0) += q_mvar;
            }
            let co2_t = gen_co2_rates
                .get(gi)
                .copied()
                .map(|rate| power_mw.max(0.0) * rate * period_spec.interval_hours());
            if let Some(co2_t) = co2_t {
                emissions_by_resource.insert(resource_id.clone(), co2_t);
            }
            resource_results.push(RawResourcePeriodResult {
                resource_id: resource_id.clone(),
                power_mw,
                commitment: committed_state,
                commitment_source: committed_state
                    .and_then(|_| commitment_source_for_period(normalized, t, j)),
                startup: startup.get(t).and_then(|row| row.get(j)).copied().flatten(),
                shutdown: shutdown
                    .get(t)
                    .and_then(|row| row.get(j))
                    .copied()
                    .flatten(),
                energy_cost,
                no_load_cost,
                startup_cost,
                reserve_awards,
                reserve_costs: reserve_costs.clone(),
                regulation: result.regulation.get(t).and_then(|row| row.get(j)).copied(),
                storage_soc_mwh,
                co2_t,
                q_mvar,
                charge_mw,
                discharge_mw,
                served_q_mvar: None,
                curtailed_mw: None,
                curtailment_pct: None,
                lmp_at_bus: None,
                net_curtailment_benefit: None,
            });
            total_energy_cost += energy_cost.unwrap_or(0.0);
            total_no_load_cost += no_load_cost.unwrap_or(0.0);
            total_startup_cost += startup_cost.unwrap_or(0.0);
            total_reserve_cost += reserve_costs.values().sum::<f64>();
            startup_costs_by_generator[j] += startup_cost.unwrap_or(0.0);
        }
        akpv_gen_secs += _akpv_gen_t0.elapsed().as_secs_f64();

        let _akpv_dl_t0 = std::time::Instant::now();
        for (global_dl_idx, dl) in &active_dispatchable_loads {
            let Some(local_dl_idx) = catalog.local_dispatchable_load_index(*global_dl_idx) else {
                continue;
            };
            let Some(load_result) = period.dr_results.loads.get(local_dl_idx) else {
                continue;
            };
            let resource_id = dl_resource_ids
                .get(local_dl_idx)
                .cloned()
                .unwrap_or_else(|| dispatchable_load_resource_id(dl, *global_dl_idx));
            let served_mw = load_result.p_served_pu * network.base_mva;
            let served_q_mvar = load_result.q_served_pu * network.base_mva;
            let curtailed_mw = load_result.p_curtailed_pu * network.base_mva;
            let reserve_awards: HashMap<String, f64> = period
                .dr_reserve_awards
                .iter()
                .filter_map(|(product_id, awards)| {
                    awards
                        .get(local_dl_idx)
                        .copied()
                        .map(|award| (product_id.clone(), award))
                })
                .collect();
            let reserve_costs: HashMap<String, f64> = reserve_awards
                .iter()
                .filter_map(|(product_id, &award)| {
                    let offer_cost = dispatchable_load_reserve_offer_for_period(
                        &problem_spec,
                        *global_dl_idx,
                        dl,
                        product_id,
                        t,
                    )?
                    .cost_per_mwh;
                    Some((product_id.clone(), offer_cost * award))
                })
                .collect();
            let reserve_cost_total = reserve_costs.values().sum::<f64>();
            *bus_withdrawals_mw.entry(load_result.bus).or_insert(0.0) += served_mw;
            *bus_withdrawals_q_mvar.entry(load_result.bus).or_insert(0.0) += served_q_mvar;
            resource_results.push(RawResourcePeriodResult {
                resource_id,
                power_mw: -served_mw,
                commitment: None,
                commitment_source: None,
                startup: None,
                shutdown: None,
                energy_cost: Some(load_result.cost_contribution),
                no_load_cost: None,
                startup_cost: None,
                reserve_awards,
                reserve_costs,
                regulation: None,
                storage_soc_mwh: None,
                co2_t: None,
                q_mvar: None,
                charge_mw: None,
                discharge_mw: None,
                served_q_mvar: Some(served_q_mvar),
                curtailed_mw: Some(curtailed_mw),
                curtailment_pct: Some(load_result.curtailment_pct),
                lmp_at_bus: Some(load_result.lmp_at_bus),
                net_curtailment_benefit: Some(load_result.net_curtailment_benefit),
            });
            total_energy_cost += load_result.cost_contribution;
            total_reserve_cost += reserve_cost_total;
        }
        akpv_dl_secs += _akpv_dl_t0.elapsed().as_secs_f64();

        let _akpv_bus_results_t0 = std::time::Instant::now();
        let mut bus_hvdc_delta_mw: HashMap<u32, f64> = HashMap::new();
        let hvdc_results: Vec<RawHvdcPeriodResult> = normalized
            .input
            .hvdc_links
            .iter()
            .enumerate()
            .map(|(k, link)| {
                let total_mw = period.hvdc_dispatch_mw.get(k).copied().unwrap_or(0.0);
                let band_dispatch = period
                    .hvdc_band_dispatch_mw
                    .get(k)
                    .cloned()
                    .unwrap_or_default();
                let delivered_mw = hvdc_delivered_mw(link, total_mw, &band_dispatch);
                *bus_hvdc_delta_mw.entry(link.from_bus).or_insert(0.0) -= total_mw;
                *bus_hvdc_delta_mw.entry(link.to_bus).or_insert(0.0) += delivered_mw;
                RawHvdcPeriodResult {
                    link_id: hvdc_link_id(link, k),
                    name: link.name.clone(),
                    mw: total_mw,
                    delivered_mw,
                    band_results: band_dispatch
                        .iter()
                        .enumerate()
                        .map(
                            |(band_index, &mw)| crate::solution::RawHvdcBandPeriodResult {
                                band_id: link
                                    .bands
                                    .get(band_index)
                                    .map(|band| band.id.clone())
                                    .unwrap_or_else(|| format!("band:{band_index}")),
                                mw,
                            },
                        )
                        .collect(),
                }
            })
            .collect();

        let bus_results: Vec<RawBusPeriodResult> = network
            .buses
            .iter()
            .enumerate()
            .map(|(bus_index, bus)| RawBusPeriodResult {
                bus_number: bus.number,
                lmp: period.lmp.get(bus_index).copied().unwrap_or(0.0),
                mec: period.lmp_energy.get(bus_index).copied().unwrap_or(0.0),
                mcc: period.lmp_congestion.get(bus_index).copied().unwrap_or(0.0),
                mlc: period.lmp_loss.get(bus_index).copied().unwrap_or(0.0),
                q_lmp: period.q_lmp.get(bus_index).copied(),
                angle_rad: result
                    .bus_angles_rad
                    .get(t)
                    .and_then(|angles| angles.get(bus_index))
                    .copied(),
                voltage_pu: result
                    .bus_voltage_pu
                    .get(t)
                    .and_then(|voltages| voltages.get(bus_index))
                    .copied(),
                net_injection_mw: bus_injection_mw.get(&bus.number).copied().unwrap_or(0.0)
                    + bus_hvdc_delta_mw.get(&bus.number).copied().unwrap_or(0.0)
                    - bus_withdrawals_mw.get(&bus.number).copied().unwrap_or(0.0),
                withdrawals_mw: bus_withdrawals_mw.get(&bus.number).copied().unwrap_or(0.0),
                loss_allocation_mw: result
                    .bus_loss_allocation_mw
                    .get(t)
                    .and_then(|v| v.get(bus_index))
                    .copied()
                    .unwrap_or(0.0),
                net_reactive_injection_mvar: reactive_results_available.then(|| {
                    bus_q_injection_mvar
                        .get(&bus.number)
                        .copied()
                        .unwrap_or(0.0)
                        - bus_withdrawals_q_mvar
                            .get(&bus.number)
                            .copied()
                            .unwrap_or(0.0)
                }),
                withdrawals_mvar: reactive_results_available.then(|| {
                    bus_withdrawals_q_mvar
                        .get(&bus.number)
                        .copied()
                        .unwrap_or(0.0)
                }),
                q_slack_pos_mvar: result
                    .bus_q_slack_pos_mvar
                    .get(t)
                    .and_then(|v| v.get(bus_index))
                    .copied(),
                q_slack_neg_mvar: result
                    .bus_q_slack_neg_mvar
                    .get(t)
                    .and_then(|v| v.get(bus_index))
                    .copied(),
                p_slack_pos_mw: result
                    .bus_p_slack_pos_mw
                    .get(t)
                    .and_then(|v| v.get(bus_index))
                    .copied(),
                p_slack_neg_mw: result
                    .bus_p_slack_neg_mw
                    .get(t)
                    .and_then(|v| v.get(bus_index))
                    .copied(),
            })
            .collect();

        akpv_reserve_secs += _akpv_bus_results_t0.elapsed().as_secs_f64();

        let _akpv_freq_t0 = std::time::Instant::now();
        let frequency_results = if !normalized
            .input
            .frequency_security
            .generator_h_values
            .is_empty()
        {
            let gen_indices: Vec<usize> = in_service_gens.iter().map(|(gi, _)| *gi).collect();
            Some(RawFrequencyPeriodResult {
                system_inertia_s: crate::sced::frequency::compute_system_inertia(
                    &period.pg_mw,
                    &gen_indices,
                    network,
                    &problem_spec,
                ),
                estimated_rocof_hz_per_s: crate::sced::frequency::compute_estimated_rocof(
                    &period.pg_mw,
                    &gen_indices,
                    network,
                    &problem_spec,
                ),
                frequency_secure: crate::sced::frequency::check_frequency_security(
                    &period.pg_mw,
                    &gen_indices,
                    network,
                    &problem_spec,
                ),
            })
        } else {
            None
        };

        akpv_interface_secs += _akpv_freq_t0.elapsed().as_secs_f64();

        let _akpv_reserve_build_t0 = std::time::Instant::now();
        let reserve_results_system =
            normalized
                .input
                .system_reserve_requirements
                .iter()
                .map(|requirement| {
                    let shortfall_mw = period
                        .reserve_shortfall
                        .get(&requirement.product_id)
                        .copied()
                        .unwrap_or(0.0);
                    let shortfall_cost = shortfall_mw
                        * reserve_products_by_id
                            .get(requirement.product_id.as_str())
                            .map(|p| p.demand_curve.marginal_cost_at(0.0))
                            .unwrap_or(0.0)
                        * dt_h;
                    RawReservePeriodResult {
                        product_id: requirement.product_id.clone(),
                        scope: keyed::ReserveScope::System,
                        zone_id: None,
                        requirement_mw: system_balance_requirement_mw_for_period(
                            &requirement.product_id,
                            t,
                            &problem_spec,
                            &reserve_products_by_id,
                        ),
                        provided_mw: reserve_balance_product_ids(
                            requirement.product_id.as_str(),
                            &reserve_products_by_id,
                        )
                        .into_iter()
                        .map(|balance_product_id| {
                            period
                                .reserve_provided
                                .get(balance_product_id)
                                .copied()
                                .unwrap_or(0.0)
                        })
                        .sum::<f64>(),
                        shortfall_mw,
                        clearing_price: period
                            .reserve_prices
                            .get(&requirement.product_id)
                            .copied()
                            .unwrap_or(0.0),
                        shortfall_cost,
                    }
                });
        let reserve_results_zonal = normalized
            .input
            .zonal_reserve_requirements
            .iter()
            .enumerate()
            .map(|(req_idx, requirement)| {
                let participant_set = zonal_participant_sets.get(req_idx).and_then(|s| s.as_ref());
                let bus_matches_gen = |j: usize| -> bool {
                    let bus_number = in_service_gen_bus_numbers
                        .get(j)
                        .copied()
                        .unwrap_or_default();
                    let fallback_area = normalized.input.generator_area.get(j).copied();
                    match participant_set {
                        Some(set) => set.contains(&bus_number),
                        None => fallback_area.unwrap_or(0) == requirement.zone_id,
                    }
                };
                let bus_matches_dl_bus = |bus_number: u32| -> bool {
                    match participant_set {
                        Some(set) => set.contains(&bus_number),
                        None => {
                            let fallback_area = crate::common::network::study_area_for_bus(
                                network,
                                &problem_spec,
                                bus_number,
                            );
                            fallback_area.unwrap_or(0) == requirement.zone_id
                        }
                    }
                };
                let key = format!("{}:{}", requirement.zone_id, requirement.product_id);
                let shortfall = period
                    .zonal_reserve_shortfall
                    .get(&key)
                    .copied()
                    .unwrap_or(0.0);
                RawReservePeriodResult {
                    product_id: requirement.product_id.clone(),
                    scope: keyed::ReserveScope::Zone,
                    zone_id: Some(requirement.zone_id),
                    requirement_mw: zonal_balance_requirement_mw_for_period(
                        &requirement.product_id,
                        requirement.zone_id,
                        t,
                        period,
                        network,
                        &problem_spec,
                        &active_dispatchable_loads,
                        &in_service_gen_bus_numbers,
                        &reserve_products_by_id,
                        &zonal_participant_sets,
                    ),
                    provided_mw: reserve_balance_product_ids(
                        requirement.product_id.as_str(),
                        &reserve_products_by_id,
                    )
                    .into_iter()
                    .map(|balance_product_id| {
                        let gen_mw: f64 = period
                            .reserve_awards
                            .get(balance_product_id)
                            .map(|awards| {
                                awards
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(j, award)| bus_matches_gen(j).then_some(*award))
                                    .sum()
                            })
                            .unwrap_or(0.0);
                        let dl_mw: f64 = period
                            .dr_reserve_awards
                            .get(balance_product_id)
                            .map(|awards| {
                                awards
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(k, award)| {
                                        active_dispatchable_loads
                                            .get(k)
                                            .filter(|(_, dl)| bus_matches_dl_bus(dl.bus))
                                            .map(|_| *award)
                                    })
                                    .sum()
                            })
                            .unwrap_or(0.0);
                        gen_mw + dl_mw
                    })
                    .sum::<f64>(),
                    shortfall_mw: shortfall,
                    clearing_price: period
                        .zonal_reserve_prices
                        .get(&key)
                        .copied()
                        .unwrap_or(0.0),
                    shortfall_cost: shortfall
                        * reserve_products_by_id
                            .get(requirement.product_id.as_str())
                            .map(|p| p.demand_curve.marginal_cost_at(0.0))
                            .unwrap_or(0.0)
                        * dt_h,
                }
            });
        let reserve_results: Vec<RawReservePeriodResult> = {
            let _t0 = std::time::Instant::now();
            let sys: Vec<RawReservePeriodResult> = reserve_results_system.collect();
            akpv_reserve_system_secs += _t0.elapsed().as_secs_f64();
            let _t1 = std::time::Instant::now();
            let zonal: Vec<RawReservePeriodResult> = reserve_results_zonal.collect();
            akpv_reserve_zonal_secs += _t1.elapsed().as_secs_f64();
            sys.into_iter().chain(zonal).collect()
        };
        akpv_reserve_build_secs += _akpv_reserve_build_t0.elapsed().as_secs_f64();

        let _akpv_post_reserve_t0 = std::time::Instant::now();
        let mut constraint_results = std::mem::take(&mut period.constraint_results);
        for reserve in &reserve_results {
            if reserve.requirement_mw.abs() < 1e-9
                && reserve.provided_mw.abs() < 1e-9
                && reserve.shortfall_mw.abs() < 1e-9
                && reserve.clearing_price.abs() < 1e-9
            {
                continue;
            }
            let constraint_id = match reserve.zone_id {
                Some(zone_id) => format!("reserve:zone:{zone_id}:{}", reserve.product_id),
                None => format!("reserve:system:{}", reserve.product_id),
            };
            let reserve_penalty_rate = reserve_products_by_id
                .get(reserve.product_id.as_str())
                .map(|product| product.demand_curve.marginal_cost_at(0.0));
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id,
                kind: keyed::ConstraintKind::ReserveRequirement,
                scope: match reserve.scope {
                    keyed::ReserveScope::System => keyed::ConstraintScope::System,
                    keyed::ReserveScope::Zone => keyed::ConstraintScope::Zone,
                },
                shadow_price: Some(reserve.clearing_price),
                slack_mw: Some(reserve.shortfall_mw),
                penalty_cost: reserve_penalty_rate,
                penalty_dollars: reserve_penalty_rate
                    .map(|rate| reserve.shortfall_mw * rate * dt_h),
            });
        }
        // Only emit shadow-price entries for constraints whose dual
        // is materially non-zero. On large security-constrained SCUC
        // runs the augmented network can carry tens of thousands of
        // contingency cuts; emitting an entry per (cut × period)
        // produces hundreds of thousands of rows in
        // ``constraint_results`` and dominates Rust→Python
        // serialization wall time. The dual is the ONLY information
        // these entries carry — when it's effectively zero the row
        // is noise. Slack-positive entries (from ``scuc/extract.rs``
        // and ``sced/extract.rs``) follow the same pattern: emit
        // only when there's something to report.
        const SHADOW_PRICE_TOL: f64 = 1e-6;
        let _akpv_branch_t0 = std::time::Instant::now();
        for (idx, price) in period.branch_shadow_prices.iter().copied().enumerate() {
            if price.abs() <= SHADOW_PRICE_TOL {
                continue;
            }
            let constraint_id = branch_constraint_ids
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("branch:{idx}"));
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id,
                kind: keyed::ConstraintKind::BranchThermal,
                scope: keyed::ConstraintScope::Branch,
                shadow_price: Some(price),
                ..Default::default()
            });
        }
        akpv_branch_secs += _akpv_branch_t0.elapsed().as_secs_f64();

        let _akpv_flowgate_t0 = std::time::Instant::now();
        for (idx, price) in period.flowgate_shadow_prices.iter().copied().enumerate() {
            if price.abs() <= SHADOW_PRICE_TOL {
                continue;
            }
            let constraint_id = flowgate_constraint_ids
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("flowgate:{idx}"));
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id,
                kind: keyed::ConstraintKind::Flowgate,
                scope: keyed::ConstraintScope::Flowgate,
                shadow_price: Some(price),
                ..Default::default()
            });
        }
        akpv_flowgate_secs += _akpv_flowgate_t0.elapsed().as_secs_f64();
        for (idx, price) in period.interface_shadow_prices.iter().copied().enumerate() {
            if price.abs() <= SHADOW_PRICE_TOL {
                continue;
            }
            let constraint_id = network
                .interfaces
                .get(idx)
                .map(|iface| iface.name.clone())
                .unwrap_or_else(|| format!("interface:{idx}"));
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id,
                kind: keyed::ConstraintKind::Interface,
                scope: keyed::ConstraintScope::Interface,
                shadow_price: Some(price),
                ..Default::default()
            });
        }
        if period.power_balance_violation.curtailment_mw > 0.0 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: "power_balance_curtailment".to_string(),
                kind: keyed::ConstraintKind::PowerBalance,
                scope: keyed::ConstraintScope::System,
                slack_mw: Some(period.power_balance_violation.curtailment_mw),
                penalty_cost: normalized
                    .input
                    .power_balance_penalty
                    .curtailment
                    .first()
                    .map(|(_, price)| *price),
                ..Default::default()
            });
        }
        if period.power_balance_violation.excess_mw > 0.0 {
            constraint_results.push(RawConstraintPeriodResult {
                constraint_id: "power_balance_excess".to_string(),
                kind: keyed::ConstraintKind::PowerBalance,
                scope: keyed::ConstraintScope::System,
                slack_mw: Some(period.power_balance_violation.excess_mw),
                penalty_cost: normalized
                    .input
                    .power_balance_penalty
                    .excess
                    .first()
                    .map(|(_, price)| *price),
                ..Default::default()
            });
        }

        // --- AC slack → ConstraintPeriodResult entries ---
        // AC P-balance slacks (from NLP bus active-power balance relaxation).
        if let Some(p_pos) = result.bus_p_slack_pos_mw.get(t) {
            for (bus_idx, &slack_mw) in p_pos.iter().enumerate() {
                if slack_mw > 1e-4 {
                    let bus_number = network.buses[bus_idx].number;
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!("power_balance:bus:{bus_number}:ac_pos"),
                        kind: keyed::ConstraintKind::PowerBalance,
                        scope: keyed::ConstraintScope::Bus,
                        slack_mw: Some(slack_mw),
                        penalty_cost: result.ac_p_balance_penalty_per_mw,
                        penalty_dollars: result
                            .ac_p_balance_penalty_per_mw
                            .map(|rate| slack_mw * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(p_neg) = result.bus_p_slack_neg_mw.get(t) {
            for (bus_idx, &slack_mw) in p_neg.iter().enumerate() {
                if slack_mw > 1e-4 {
                    let bus_number = network.buses[bus_idx].number;
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!("power_balance:bus:{bus_number}:ac_neg"),
                        kind: keyed::ConstraintKind::PowerBalance,
                        scope: keyed::ConstraintScope::Bus,
                        slack_mw: Some(slack_mw),
                        penalty_cost: result.ac_p_balance_penalty_per_mw,
                        penalty_dollars: result
                            .ac_p_balance_penalty_per_mw
                            .map(|rate| slack_mw * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }
        // AC Q-balance slacks (from NLP bus reactive-power balance relaxation).
        if let Some(q_pos) = result.bus_q_slack_pos_mvar.get(t) {
            for (bus_idx, &slack_mvar) in q_pos.iter().enumerate() {
                if slack_mvar > 1e-4 {
                    let bus_number = network.buses[bus_idx].number;
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!("reactive_balance:bus:{bus_number}:pos"),
                        kind: keyed::ConstraintKind::ReactiveBalance,
                        scope: keyed::ConstraintScope::Bus,
                        slack_mw: Some(slack_mvar),
                        penalty_cost: result.ac_q_balance_penalty_per_mvar,
                        penalty_dollars: result
                            .ac_q_balance_penalty_per_mvar
                            .map(|rate| slack_mvar * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(q_neg) = result.bus_q_slack_neg_mvar.get(t) {
            for (bus_idx, &slack_mvar) in q_neg.iter().enumerate() {
                if slack_mvar > 1e-4 {
                    let bus_number = network.buses[bus_idx].number;
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!("reactive_balance:bus:{bus_number}:neg"),
                        kind: keyed::ConstraintKind::ReactiveBalance,
                        scope: keyed::ConstraintScope::Bus,
                        slack_mw: Some(slack_mvar),
                        penalty_cost: result.ac_q_balance_penalty_per_mvar,
                        penalty_dollars: result
                            .ac_q_balance_penalty_per_mvar
                            .map(|rate| slack_mvar * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }
        // AC thermal slacks (from NLP branch apparent-power limit relaxation).
        if let Some(from_slacks) = result.thermal_limit_slack_from_mva.get(t) {
            for (branch_idx, &slack_mva) in from_slacks.iter().enumerate() {
                if slack_mva > 1e-4 {
                    let branch = &network.branches[branch_idx];
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "branch:{}:{}:{}:ac_thermal_from",
                            branch.from_bus, branch.to_bus, branch.circuit
                        ),
                        kind: keyed::ConstraintKind::BranchThermal,
                        scope: keyed::ConstraintScope::Branch,
                        slack_mw: Some(slack_mva),
                        penalty_cost: result.ac_thermal_penalty_per_mva,
                        penalty_dollars: result
                            .ac_thermal_penalty_per_mva
                            .map(|rate| slack_mva * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(to_slacks) = result.thermal_limit_slack_to_mva.get(t) {
            for (branch_idx, &slack_mva) in to_slacks.iter().enumerate() {
                if slack_mva > 1e-4 {
                    let branch = &network.branches[branch_idx];
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "branch:{}:{}:{}:ac_thermal_to",
                            branch.from_bus, branch.to_bus, branch.circuit
                        ),
                        kind: keyed::ConstraintKind::BranchThermal,
                        scope: keyed::ConstraintScope::Branch,
                        slack_mw: Some(slack_mva),
                        penalty_cost: result.ac_thermal_penalty_per_mva,
                        penalty_dollars: result
                            .ac_thermal_penalty_per_mva
                            .map(|rate| slack_mva * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }

        // AC voltage-magnitude slacks (from NLP voltage bound relaxation).
        if let Some(high_slacks) = result.bus_vm_slack_high_pu.get(t) {
            for (bus_idx, &slack_pu) in high_slacks.iter().enumerate() {
                if slack_pu > 1e-6 {
                    let bus_number = network.buses[bus_idx].number;
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!("voltage_bound:bus:{bus_number}:high"),
                        kind: keyed::ConstraintKind::VoltageBound,
                        scope: keyed::ConstraintScope::Bus,
                        slack_mw: Some(slack_pu),
                        penalty_cost: result.ac_voltage_penalty_per_pu,
                        penalty_dollars: result
                            .ac_voltage_penalty_per_pu
                            .map(|rate| slack_pu * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(low_slacks) = result.bus_vm_slack_low_pu.get(t) {
            for (bus_idx, &slack_pu) in low_slacks.iter().enumerate() {
                if slack_pu > 1e-6 {
                    let bus_number = network.buses[bus_idx].number;
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!("voltage_bound:bus:{bus_number}:low"),
                        kind: keyed::ConstraintKind::VoltageBound,
                        scope: keyed::ConstraintScope::Bus,
                        slack_mw: Some(slack_pu),
                        penalty_cost: result.ac_voltage_penalty_per_pu,
                        penalty_dollars: result
                            .ac_voltage_penalty_per_pu
                            .map(|rate| slack_pu * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }

        // AC angle-difference slacks (from NLP angle-bound relaxation).
        if let Some(high_slacks) = result.angle_diff_slack_high_rad.get(t) {
            for (branch_idx, &slack_rad) in high_slacks.iter().enumerate() {
                if slack_rad > 1e-6 {
                    let branch = &network.branches[branch_idx];
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "angle_diff:{}:{}:{}:ac_high",
                            branch.from_bus, branch.to_bus, branch.circuit
                        ),
                        kind: keyed::ConstraintKind::AngleDifference,
                        scope: keyed::ConstraintScope::Branch,
                        slack_mw: Some(slack_rad),
                        penalty_cost: result.ac_angle_penalty_per_rad,
                        penalty_dollars: result
                            .ac_angle_penalty_per_rad
                            .map(|rate| slack_rad * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(low_slacks) = result.angle_diff_slack_low_rad.get(t) {
            for (branch_idx, &slack_rad) in low_slacks.iter().enumerate() {
                if slack_rad > 1e-6 {
                    let branch = &network.branches[branch_idx];
                    constraint_results.push(RawConstraintPeriodResult {
                        constraint_id: format!(
                            "angle_diff:{}:{}:{}:ac_low",
                            branch.from_bus, branch.to_bus, branch.circuit
                        ),
                        kind: keyed::ConstraintKind::AngleDifference,
                        scope: keyed::ConstraintScope::Branch,
                        slack_mw: Some(slack_rad),
                        penalty_cost: result.ac_angle_penalty_per_rad,
                        penalty_dollars: result
                            .ac_angle_penalty_per_rad
                            .map(|rate| slack_rad * rate * dt_h),
                        ..Default::default()
                    });
                }
            }
        }

        period.resource_results = resource_results;
        period.bus_results = bus_results;
        period.reserve_results = reserve_results;
        period.constraint_results = constraint_results;
        period.hvdc_results = hvdc_results;
        period.emissions_results = Some(RawEmissionsPeriodResult {
            total_co2_t: period.co2_t,
            by_resource_t: emissions_by_resource,
        });
        period.frequency_results = frequency_results;
        akpv_post_reserve_secs += _akpv_post_reserve_t0.elapsed().as_secs_f64();
    }

    result.commitment = Some(
        commitment
            .iter()
            .map(|row| row.iter().map(|state| state.unwrap_or(false)).collect())
            .collect(),
    );
    result.startup = Some(
        startup
            .iter()
            .map(|row| row.iter().map(|state| state.unwrap_or(false)).collect())
            .collect(),
    );
    result.shutdown = Some(
        shutdown
            .iter()
            .map(|row| row.iter().map(|state| state.unwrap_or(false)).collect())
            .collect(),
    );
    result.startup_costs = Some(startup_costs_by_generator);
    result.startup_cost_total = Some(total_startup_cost);
    if result.operating_cost.is_none() {
        result.operating_cost = Some(total_energy_cost + total_no_load_cost + total_reserve_cost);
    }
    let total_co2_t: f64 = result.periods.iter().map(|period| period.co2_t).sum();
    result.summary = DispatchSummary {
        total_cost: result.summary.total_cost,
        total_energy_cost,
        total_reserve_cost,
        total_no_load_cost,
        total_startup_cost,
        total_co2_t,
        ..Default::default()
    };
    result.diagnostics = DispatchDiagnostics {
        iterations: result.diagnostics.iterations,
        solve_time_secs: result.diagnostics.solve_time_secs,
        phase_timings: result.diagnostics.phase_timings.clone(),
        pricing_converged: result.diagnostics.pricing_converged,
        penalty_slack_values: result.diagnostics.penalty_slack_values.clone(),
        security: result.diagnostics.security.clone(),
        sced_ac_benders: result.diagnostics.sced_ac_benders.clone(),
        ac_sced_period_timings: result.diagnostics.ac_sced_period_timings.clone(),
        ac_opf_stats: result.diagnostics.ac_opf_stats.clone(),
        commitment_mip_trace: result.diagnostics.commitment_mip_trace.clone(),
    };
    info!(
        stage = "attach_keyed_period_views.sub_phases",
        gen_loop_secs = akpv_gen_secs,
        dl_loop_secs = akpv_dl_secs,
        bus_hvdc_bus_results_secs = akpv_reserve_secs,
        frequency_secs = akpv_interface_secs,
        reserve_build_secs = akpv_reserve_build_secs,
        reserve_system_secs = akpv_reserve_system_secs,
        reserve_zonal_secs = akpv_reserve_zonal_secs,
        post_reserve_secs = akpv_post_reserve_secs,
        branch_constraint_id_secs = akpv_branch_secs,
        flowgate_constraint_id_secs = akpv_flowgate_secs,
        n_branch_constraint_ids = branch_constraint_ids.len(),
        n_flowgate_constraint_ids = flowgate_constraint_ids.len(),
        n_zonal_reserve_reqs = normalized.input.zonal_reserve_requirements.len(),
        n_system_reserve_reqs = normalized.input.system_reserve_requirements.len(),
        "attach_keyed_period_views sub-phase timings"
    );
}

fn infer_resource_kind(
    raw: &RawResourcePeriodResult,
    resource: Option<&DispatchResource>,
) -> DispatchResourceKind {
    resource.map(|resource| resource.kind).unwrap_or_else(|| {
        if raw.served_q_mvar.is_some()
            || raw.curtailed_mw.is_some()
            || raw.curtailment_pct.is_some()
            || raw.lmp_at_bus.is_some()
            || raw.net_curtailment_benefit.is_some()
        {
            DispatchResourceKind::DispatchableLoad
        } else if raw.charge_mw.is_some()
            || raw.discharge_mw.is_some()
            || raw.storage_soc_mwh.is_some()
        {
            DispatchResourceKind::Storage
        } else {
            DispatchResourceKind::Generator
        }
    })
}

fn map_resource_period_result(
    raw: RawResourcePeriodResult,
    resource: Option<&DispatchResource>,
    resource_terms: &[ObjectiveTerm],
    suppress_fallback_costs: bool,
) -> keyed::ResourcePeriodResult {
    let kind = infer_resource_kind(&raw, resource);
    let has_exact_terms = !resource_terms.is_empty();
    // Dispatchable-load resource rows should always report the exact variable
    // objective ledger. Falling back to the raw curtailment-cost field flips
    // the sign semantics for fully curtailed blocks because the LP variable
    // term is zero while the raw field still contains the positive constant
    // curtailment cost.
    let use_exact_only = matches!(kind, DispatchResourceKind::DispatchableLoad);
    let detail = match kind {
        DispatchResourceKind::Generator => {
            keyed::ResourcePeriodDetail::Generator(keyed::GeneratorPeriodDetail {
                commitment: raw.commitment,
                commitment_source: raw.commitment_source,
                startup: raw.startup,
                shutdown: raw.shutdown,
                regulation: raw.regulation,
                q_mvar: raw.q_mvar,
            })
        }
        DispatchResourceKind::Storage => {
            keyed::ResourcePeriodDetail::Storage(keyed::StoragePeriodDetail {
                commitment: raw.commitment,
                commitment_source: raw.commitment_source,
                startup: raw.startup,
                shutdown: raw.shutdown,
                regulation: raw.regulation,
                q_mvar: raw.q_mvar,
                charge_mw: raw.charge_mw.unwrap_or(0.0),
                discharge_mw: raw.discharge_mw.unwrap_or(0.0),
                soc_mwh: raw.storage_soc_mwh,
            })
        }
        DispatchResourceKind::DispatchableLoad => {
            keyed::ResourcePeriodDetail::DispatchableLoad(keyed::DispatchableLoadPeriodDetail {
                served_p_mw: (-raw.power_mw).max(0.0),
                served_q_mvar: raw.served_q_mvar,
                curtailed_mw: raw.curtailed_mw.unwrap_or(0.0),
                curtailment_pct: raw.curtailment_pct.unwrap_or(0.0),
                lmp_at_bus: raw.lmp_at_bus.unwrap_or(0.0),
                net_curtailment_benefit: raw.net_curtailment_benefit.unwrap_or(0.0),
            })
        }
    };

    let objective_cost = if has_exact_terms || use_exact_only {
        sum_terms(resource_terms)
    } else if suppress_fallback_costs {
        0.0
    } else {
        raw.energy_cost.unwrap_or(0.0)
            + raw.no_load_cost.unwrap_or(0.0)
            + raw.startup_cost.unwrap_or(0.0)
            + raw.reserve_costs.values().sum::<f64>()
    };
    let energy_cost_exact = resource_energy_cost(resource_terms);
    let no_load_cost_exact = resource_no_load_cost(resource_terms);
    let startup_cost_exact = resource_startup_cost(resource_terms);
    let shutdown_cost_exact = resource_shutdown_cost(resource_terms);
    let reserve_costs_exact = resource_reserve_costs(resource_terms);

    keyed::ResourcePeriodResult {
        resource_id: raw.resource_id,
        kind,
        bus_number: resource.and_then(|resource| resource.bus_number),
        power_mw: raw.power_mw,
        objective_cost,
        energy_cost: if has_exact_terms || use_exact_only {
            Some(energy_cost_exact)
        } else if suppress_fallback_costs {
            None
        } else {
            raw.energy_cost
        },
        no_load_cost: if has_exact_terms || use_exact_only {
            (no_load_cost_exact.abs() > 1e-9).then_some(no_load_cost_exact)
        } else if suppress_fallback_costs {
            None
        } else {
            raw.no_load_cost
        },
        startup_cost: if has_exact_terms || use_exact_only {
            (startup_cost_exact.abs() > 1e-9).then_some(startup_cost_exact)
        } else if suppress_fallback_costs {
            None
        } else {
            raw.startup_cost
        },
        shutdown_cost: if has_exact_terms || use_exact_only {
            (shutdown_cost_exact.abs() > 1e-9).then_some(shutdown_cost_exact)
        } else {
            None
        },
        reserve_awards: raw.reserve_awards,
        reserve_costs: if has_exact_terms || use_exact_only {
            reserve_costs_exact
        } else if suppress_fallback_costs {
            HashMap::new()
        } else {
            raw.reserve_costs
        },
        objective_terms: aggregate_terms(resource_terms),
        co2_t: raw.co2_t,
        detail,
    }
}

fn new_resource_horizon_result(resource: &DispatchResource) -> keyed::ResourceHorizonResult {
    keyed::ResourceHorizonResult {
        resource_id: resource.resource_id.clone(),
        kind: resource.kind,
        objective_cost: 0.0,
        total_energy_cost: 0.0,
        total_no_load_cost: 0.0,
        total_startup_cost: 0.0,
        total_shutdown_cost: 0.0,
        total_reserve_cost: 0.0,
        total_co2_t: 0.0,
        objective_terms: Vec::new(),
        co2_shadow_price_per_mwh: None,
        commitment_schedule: None,
        startup_schedule: None,
        shutdown_schedule: None,
        regulation_schedule: None,
        storage_soc_mwh: None,
    }
}

fn combined_cycle_member_resource_ids(network: &Network) -> HashSet<String> {
    let mut ids = HashSet::new();
    for plant in &network.market_data.combined_cycle_plants {
        for config in &plant.configs {
            for &gen_idx in &config.gen_indices {
                if let Some(generator) = network.generators.get(gen_idx) {
                    ids.insert(crate::report_ids::generator_resource_id(generator));
                }
            }
        }
    }
    ids
}

fn apply_objective_totals_to_resource_summary(
    summary: &mut keyed::ResourceHorizonResult,
    resource_terms: &[ObjectiveTerm],
) {
    summary.objective_cost = sum_terms(resource_terms);
    summary.total_energy_cost = resource_energy_cost(resource_terms);
    summary.total_no_load_cost = resource_no_load_cost(resource_terms);
    summary.total_startup_cost = resource_startup_cost(resource_terms);
    summary.total_shutdown_cost = resource_shutdown_cost(resource_terms);
    summary.total_reserve_cost = bucket_total(resource_terms, ObjectiveBucket::Reserve);
    summary.objective_terms = aggregate_terms(resource_terms);
}

fn apply_objective_totals_to_summary(
    summary: &mut keyed::DispatchSummary,
    objective_terms: &[ObjectiveTerm],
    total_cost: f64,
    total_co2_t: f64,
) {
    summary.total_cost = total_cost;
    summary.total_energy_cost = bucket_total(objective_terms, ObjectiveBucket::Energy);
    summary.total_reserve_cost = bucket_total(objective_terms, ObjectiveBucket::Reserve);
    summary.total_no_load_cost = bucket_total(objective_terms, ObjectiveBucket::NoLoad);
    summary.total_startup_cost = bucket_total(objective_terms, ObjectiveBucket::Startup);
    summary.total_shutdown_cost = bucket_total(objective_terms, ObjectiveBucket::Shutdown);
    summary.total_tracking_cost = bucket_total(objective_terms, ObjectiveBucket::Tracking);
    summary.total_adder_cost = bucket_total(objective_terms, ObjectiveBucket::Adder);
    summary.total_other_cost = bucket_total(objective_terms, ObjectiveBucket::Other);
    summary.total_penalty_cost = bucket_total(objective_terms, ObjectiveBucket::Penalty);
    summary.total_co2_t = total_co2_t;
    summary.objective_terms = aggregate_terms(objective_terms);
}

fn apply_raw_resource_schedules(
    resources: &[DispatchResource],
    commitment: Option<&Vec<Vec<bool>>>,
    startup: Option<&Vec<Vec<bool>>>,
    shutdown: Option<&Vec<Vec<bool>>>,
    regulation: &[Vec<bool>],
    storage_soc: &HashMap<usize, Vec<f64>>,
    co2_shadow_price: &[f64],
    summaries_by_id: &mut HashMap<String, keyed::ResourceHorizonResult>,
) {
    let generator_resources: Vec<&DispatchResource> = resources
        .iter()
        .filter(|resource| resource.kind != DispatchResourceKind::DispatchableLoad)
        .collect();

    for (generator_index, resource) in generator_resources.iter().enumerate() {
        let Some(summary) = summaries_by_id.get_mut(&resource.resource_id) else {
            continue;
        };

        if let Some(commitment) = commitment {
            summary.commitment_schedule = Some(
                commitment
                    .iter()
                    .filter_map(|row| row.get(generator_index).copied())
                    .collect(),
            );
        }
        if let Some(startup) = startup {
            summary.startup_schedule = Some(
                startup
                    .iter()
                    .filter_map(|row| row.get(generator_index).copied())
                    .collect(),
            );
        }
        if let Some(shutdown) = shutdown {
            summary.shutdown_schedule = Some(
                shutdown
                    .iter()
                    .filter_map(|row| row.get(generator_index).copied())
                    .collect(),
            );
        }
        if !regulation.is_empty() {
            summary.regulation_schedule = Some(
                regulation
                    .iter()
                    .filter_map(|row| row.get(generator_index).copied())
                    .collect(),
            );
        }
        if let Some(&shadow_price) = co2_shadow_price.get(generator_index) {
            summary.co2_shadow_price_per_mwh = Some(shadow_price);
        }
        if resource.kind == DispatchResourceKind::Storage
            && let Some(soc_schedule) = storage_soc.get(&resource.source_index)
        {
            summary.storage_soc_mwh = Some(soc_schedule.clone());
        }
    }
}

fn build_combined_cycle_results(
    network: &Network,
    cc_config_schedule: &[Vec<Option<String>>],
    objective_terms: &[ObjectiveTerm],
) -> Vec<keyed::CombinedCyclePlantResult> {
    let n_plants = network
        .market_data
        .combined_cycle_plants
        .len()
        .max(cc_config_schedule.first().map(Vec::len).unwrap_or(0));

    (0..n_plants)
        .map(|plant_index| {
            let plant = network.market_data.combined_cycle_plants.get(plant_index);
            let plant_id = crate::report_ids::combined_cycle_plant_id(plant, plant_index);
            let name = plant
                .map(|plant| plant.name.clone())
                .unwrap_or_else(|| plant_id.clone());
            let plant_terms = aggregate_terms(&filter_terms_for_subject(
                objective_terms,
                crate::ObjectiveSubjectKind::CombinedCyclePlant,
                &plant_id,
            ));
            let transition_cost = plant_terms
                .iter()
                .filter(|term| term.kind == ObjectiveTermKind::CombinedCycleTransition)
                .map(|term| term.dollars)
                .sum();
            keyed::CombinedCyclePlantResult {
                plant_id,
                name,
                active_configuration_schedule: cc_config_schedule
                    .iter()
                    .map(|configs| configs.get(plant_index).cloned().flatten())
                    .collect(),
                objective_cost: sum_terms(&plant_terms),
                transition_cost,
                objective_terms: plant_terms,
            }
        })
        .collect()
}

fn build_penalty_summary(periods: &[keyed::DispatchPeriodResult]) -> keyed::PenaltySummary {
    let mut summary = keyed::PenaltySummary::default();
    for period in periods {
        for term in period.objective_terms() {
            match term.kind {
                ObjectiveTermKind::PowerBalancePenalty => {
                    summary.power_balance_p_total_mw += term.quantity.unwrap_or(0.0);
                    summary.power_balance_p_total_cost += term.dollars;
                }
                ObjectiveTermKind::ReactiveBalancePenalty => {
                    summary.power_balance_q_total_mvar += term.quantity.unwrap_or(0.0);
                    summary.power_balance_q_total_cost += term.dollars;
                }
                ObjectiveTermKind::VoltagePenalty => {
                    summary.voltage_total_pu += term.quantity.unwrap_or(0.0);
                    summary.voltage_total_cost += term.dollars;
                }
                ObjectiveTermKind::AngleDifferencePenalty => {
                    summary.angle_total_rad += term.quantity.unwrap_or(0.0);
                    summary.angle_total_cost += term.dollars;
                }
                ObjectiveTermKind::ThermalLimitPenalty => {
                    summary.thermal_total_mw += term.quantity.unwrap_or(0.0);
                    summary.thermal_total_cost += term.dollars;
                }
                ObjectiveTermKind::FlowgatePenalty | ObjectiveTermKind::InterfacePenalty => {
                    summary.flowgate_total_mw += term.quantity.unwrap_or(0.0);
                    summary.flowgate_total_cost += term.dollars;
                }
                ObjectiveTermKind::RampPenalty => {
                    summary.ramp_total_mw += term.quantity.unwrap_or(0.0);
                    summary.ramp_total_cost += term.dollars;
                }
                ObjectiveTermKind::ReserveShortfall
                | ObjectiveTermKind::ReactiveReserveShortfall => {
                    summary.reserve_shortfall_total_mw += term.quantity.unwrap_or(0.0);
                    summary.reserve_shortfall_total_cost += term.dollars;
                }
                ObjectiveTermKind::CommitmentCapacityPenalty => {
                    summary.headroom_footroom_total_mw += term.quantity.unwrap_or(0.0);
                    summary.headroom_footroom_total_cost += term.dollars;
                }
                ObjectiveTermKind::EnergyWindowPenalty => {
                    summary.energy_window_total_cost += term.dollars;
                }
                _ => {}
            }
        }
    }
    summary.total_penalty_cost = summary.power_balance_p_total_cost
        + summary.power_balance_q_total_cost
        + summary.voltage_total_cost
        + summary.angle_total_cost
        + summary.thermal_total_cost
        + summary.flowgate_total_cost
        + summary.ramp_total_cost
        + summary.reserve_shortfall_total_cost
        + summary.headroom_footroom_total_cost
        + summary.energy_window_total_cost;
    summary
}

fn emit_public_keyed_solution(
    result: RawDispatchSolution,
    network: &Network,
) -> keyed::DispatchSolution {
    let RawDispatchSolution {
        study,
        resources,
        buses,
        summary,
        diagnostics,
        periods: raw_periods,
        commitment,
        startup,
        shutdown,
        startup_costs: _,
        operating_cost: _,
        startup_cost_total: _,
        storage_soc,
        bus_angles_rad: _,
        bus_voltage_pu: _,
        generator_q_mvar: _,
        system_inertia_s: _,
        estimated_rocof_hz_per_s: _,
        frequency_secure: _,
        co2_shadow_price,
        regulation,
        branch_commitment_state,
        cc_config_schedule,
        cc_transition_cost: _,
        cc_transition_costs: _,
        model_diagnostics,
        aux_flowgate_names: _,
        bus_q_slack_pos_mvar: _,
        bus_q_slack_neg_mvar: _,
        bus_p_slack_pos_mw: _,
        bus_p_slack_neg_mw: _,
        thermal_limit_slack_from_mva: _,
        thermal_limit_slack_to_mva: _,
        bus_vm_slack_high_pu: _,
        bus_vm_slack_low_pu: _,
        angle_diff_slack_high_rad: _,
        angle_diff_slack_low_rad: _,
        ac_p_balance_penalty_per_mw: _,
        ac_q_balance_penalty_per_mvar: _,
        ac_thermal_penalty_per_mva: _,
        ac_voltage_penalty_per_pu: _,
        ac_angle_penalty_per_rad: _,
        bus_loss_allocation_mw: _,
        scuc_final_loss_warm_start: _,
    } = result;

    let resource_meta_by_id: HashMap<&str, &DispatchResource> = resources
        .iter()
        .map(|resource| (resource.resource_id.as_str(), resource))
        .collect();
    let cc_member_resource_ids = combined_cycle_member_resource_ids(network);
    let mut summaries_by_id: HashMap<String, keyed::ResourceHorizonResult> = resources
        .iter()
        .map(|resource| {
            (
                resource.resource_id.clone(),
                new_resource_horizon_result(resource),
            )
        })
        .collect();
    let mut resource_terms_by_id: HashMap<String, Vec<ObjectiveTerm>> = HashMap::new();
    let mut all_objective_terms = Vec::new();

    let periods = raw_periods
        .into_iter()
        .enumerate()
        .map(|(period_index, period)| {
            let mut period_resource_terms: HashMap<String, Vec<ObjectiveTerm>> = HashMap::new();
            for term in &period.objective_terms {
                all_objective_terms.push(term.clone());
                if term.subject_kind == crate::ObjectiveSubjectKind::Resource {
                    resource_terms_by_id
                        .entry(term.subject_id.clone())
                        .or_default()
                        .push(term.clone());
                    period_resource_terms
                        .entry(term.subject_id.clone())
                        .or_default()
                        .push(term.clone());
                }
            }
            let resource_results: Vec<keyed::ResourcePeriodResult> = period
                .resource_results
                .into_iter()
                .map(|resource_result| {
                    let suppress_fallback_costs =
                        cc_member_resource_ids.contains(resource_result.resource_id.as_str());
                    let resource_meta = resource_meta_by_id
                        .get(resource_result.resource_id.as_str())
                        .copied();
                    let exact_terms = period_resource_terms
                        .remove(resource_result.resource_id.as_str())
                        .unwrap_or_default();
                    let keyed_result = map_resource_period_result(
                        resource_result,
                        resource_meta,
                        &exact_terms,
                        suppress_fallback_costs,
                    );
                    if let Some(summary) = summaries_by_id.get_mut(&keyed_result.resource_id) {
                        summary.total_co2_t += keyed_result.co2_t.unwrap_or(0.0);
                    }
                    keyed_result
                })
                .collect();

            let mut keyed_period = keyed::DispatchPeriodResult::empty(period_index);
            keyed_period.total_cost = period.total_cost;
            keyed_period.co2_t = period.co2_t;
            keyed_period.objective_terms = period.objective_terms.clone();
            keyed_period.resource_results = resource_results;
            keyed_period.bus_results = period
                .bus_results
                .into_iter()
                .map(|bus| keyed::BusPeriodResult {
                    bus_number: bus.bus_number,
                    lmp: bus.lmp,
                    mec: bus.mec,
                    mcc: bus.mcc,
                    mlc: bus.mlc,
                    q_lmp: bus.q_lmp,
                    angle_rad: bus.angle_rad,
                    voltage_pu: bus.voltage_pu,
                    net_injection_mw: bus.net_injection_mw,
                    withdrawals_mw: bus.withdrawals_mw,
                    loss_allocation_mw: bus.loss_allocation_mw,
                    net_reactive_injection_mvar: bus.net_reactive_injection_mvar,
                    withdrawals_mvar: bus.withdrawals_mvar,
                    q_slack_pos_mvar: bus.q_slack_pos_mvar,
                    q_slack_neg_mvar: bus.q_slack_neg_mvar,
                    p_slack_pos_mw: bus.p_slack_pos_mw,
                    p_slack_neg_mw: bus.p_slack_neg_mw,
                })
                .collect();
            keyed_period.reserve_results = period
                .reserve_results
                .into_iter()
                .map(|reserve| {
                    let zone_id = reserve.zone_id.map(ZoneId::from);
                    let subject_id = match reserve.zone_id {
                        Some(zone_id) => format!("reserve:zone:{zone_id}:{}", reserve.product_id),
                        None => format!("reserve:system:{}", reserve.product_id),
                    };
                    let shortfall_cost =
                        reserve_shortfall_cost(&keyed_period.objective_terms, &subject_id);
                    keyed::ReservePeriodResult {
                        product_id: reserve.product_id,
                        scope: reserve.scope,
                        zone_id,
                        requirement_mw: reserve.requirement_mw,
                        provided_mw: reserve.provided_mw,
                        shortfall_mw: reserve.shortfall_mw,
                        clearing_price: reserve.clearing_price,
                        shortfall_cost: if shortfall_cost.abs() > 1e-9 {
                            shortfall_cost
                        } else {
                            reserve.shortfall_cost
                        },
                    }
                })
                .collect();
            keyed_period.constraint_results = period
                .constraint_results
                .into_iter()
                .map(|constraint| keyed::ConstraintPeriodResult {
                    constraint_id: constraint.constraint_id,
                    kind: constraint.kind,
                    scope: constraint.scope,
                    shadow_price: constraint.shadow_price,
                    slack_mw: constraint.slack_mw,
                    penalty_cost: constraint.penalty_cost,
                    penalty_dollars: constraint.penalty_dollars,
                })
                .collect();
            keyed_period.hvdc_results = period
                .hvdc_results
                .into_iter()
                .map(|hvdc| keyed::HvdcPeriodResult {
                    link_id: hvdc.link_id,
                    name: hvdc.name,
                    mw: hvdc.mw,
                    delivered_mw: hvdc.delivered_mw,
                    band_results: hvdc
                        .band_results
                        .into_iter()
                        .map(|band| keyed::HvdcBandPeriodResult {
                            band_id: band.band_id,
                            mw: band.mw,
                        })
                        .collect(),
                })
                .collect();
            keyed_period.tap_dispatch = period.tap_dispatch;
            keyed_period.phase_dispatch = period.phase_dispatch;
            keyed_period.switched_shunt_dispatch = period.switched_shunt_dispatch;
            keyed_period.branch_commitment_state = branch_commitment_state
                .get(period_index)
                .cloned()
                .unwrap_or_default();
            keyed_period.virtual_bid_results = period.virtual_bid_results;
            keyed_period.par_results = period.par_results;
            keyed_period.power_balance_violation = period.power_balance_violation;
            keyed_period.emissions_results =
                period
                    .emissions_results
                    .map(|emissions| keyed::EmissionsPeriodResult {
                        total_co2_t: emissions.total_co2_t,
                        by_resource_t: emissions.by_resource_t,
                    });
            keyed_period.frequency_results =
                period
                    .frequency_results
                    .map(|frequency| keyed::FrequencyPeriodResult {
                        system_inertia_s: frequency.system_inertia_s,
                        estimated_rocof_hz_per_s: frequency.estimated_rocof_hz_per_s,
                        frequency_secure: frequency.frequency_secure,
                    });
            keyed_period.sced_ac_benders_eta_dollars_per_hour =
                period.sced_ac_benders_eta_dollars_per_hour;
            keyed_period
        })
        .collect::<Vec<_>>();

    apply_raw_resource_schedules(
        &resources,
        commitment.as_ref(),
        startup.as_ref(),
        shutdown.as_ref(),
        &regulation,
        &storage_soc,
        &co2_shadow_price,
        &mut summaries_by_id,
    );

    for resource in &resources {
        if let Some(summary) = summaries_by_id.get_mut(&resource.resource_id) {
            let resource_terms = resource_terms_by_id
                .get(&resource.resource_id)
                .cloned()
                .unwrap_or_default();
            apply_objective_totals_to_resource_summary(summary, &resource_terms);
        }
    }

    let total_co2_t: f64 = periods.iter().map(keyed::DispatchPeriodResult::co2_t).sum();
    let mut summary = summary;
    apply_objective_totals_to_summary(
        &mut summary,
        &all_objective_terms,
        periods
            .iter()
            .map(keyed::DispatchPeriodResult::total_cost)
            .sum(),
        total_co2_t,
    );

    let mut solution = keyed::DispatchSolution::new(
        study,
        resources.clone(),
        buses,
        summary,
        keyed::DispatchDiagnostics {
            iterations: diagnostics.iterations,
            solve_time_secs: diagnostics.solve_time_secs,
            phase_timings: diagnostics.phase_timings.clone(),
            pricing_converged: diagnostics.pricing_converged,
            penalty_slack_values: diagnostics.penalty_slack_values,
            security: diagnostics.security,
            sced_ac_benders: diagnostics.sced_ac_benders.clone(),
            ac_sced_period_timings: diagnostics.ac_sced_period_timings,
            ac_opf_stats: diagnostics.ac_opf_stats,
            commitment_mip_trace: diagnostics.commitment_mip_trace,
        },
        periods,
        resources
            .iter()
            .filter_map(|resource| summaries_by_id.remove(&resource.resource_id))
            .collect(),
        build_combined_cycle_results(network, &cc_config_schedule, &all_objective_terms),
    );
    solution.model_diagnostics = model_diagnostics;

    solution.penalty_summary = build_penalty_summary(solution.periods());
    solution.summary.total_penalty_cost = solution.penalty_summary.total_penalty_cost;
    solution.refresh_audit();

    solution
}

fn push_storage_soc_period(
    network: &Network,
    storage_soc: &mut HashMap<usize, Vec<f64>>,
    soc_by_storage_order: &[f64],
) {
    let storage_gi: Vec<usize> = network
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service && g.is_storage())
        .map(|(gi, _)| gi)
        .collect();

    for (s, &gi) in storage_gi.iter().enumerate() {
        if let Some(&soc) = soc_by_storage_order.get(s) {
            storage_soc.entry(gi).or_default().push(soc);
        }
    }
}

pub(crate) struct SequentialDispatchAccumulator {
    wall_start: Instant,
    periods: Vec<RawDispatchPeriodResult>,
    total_cost: f64,
    iterations: u32,
    storage_soc: HashMap<usize, Vec<f64>>,
    bus_angles_rad: Vec<Vec<f64>>,
    bus_voltage_pu: Vec<Vec<f64>>,
    generator_q_mvar: Vec<Vec<f64>>,
    bus_q_slack_pos_mvar: Vec<Vec<f64>>,
    bus_q_slack_neg_mvar: Vec<Vec<f64>>,
    bus_p_slack_pos_mw: Vec<Vec<f64>>,
    bus_p_slack_neg_mw: Vec<Vec<f64>>,
    thermal_limit_slack_from_mva: Vec<Vec<f64>>,
    thermal_limit_slack_to_mva: Vec<Vec<f64>>,
    bus_vm_slack_high_pu: Vec<Vec<f64>>,
    bus_vm_slack_low_pu: Vec<Vec<f64>>,
    angle_diff_slack_high_rad: Vec<Vec<f64>>,
    angle_diff_slack_low_rad: Vec<Vec<f64>>,
    storage_gi: Vec<usize>,
    exact_dispatch_overrides_by_period: Vec<Vec<Option<f64>>>,
    state: SequentialDispatchState,
    ac_sced_period_timings: Vec<crate::sced::ac::AcScedPeriodTimings>,
    ac_opf_stats: Vec<crate::sced::ac::AcOpfStats>,
}

impl SequentialDispatchAccumulator {
    pub(crate) fn new(network: &Network, problem_spec: DispatchProblemSpec<'_>) -> Self {
        let storage_gi = network
            .generators
            .iter()
            .enumerate()
            .filter(|(_, g)| g.in_service && g.is_storage())
            .map(|(gi, _)| gi)
            .collect();
        let exact_dispatch_overrides_by_period =
            exact_generator_dispatch_overrides_by_period(network, problem_spec);

        Self {
            wall_start: Instant::now(),
            periods: Vec::with_capacity(problem_spec.n_periods),
            total_cost: 0.0,
            iterations: 0,
            storage_soc: HashMap::new(),
            bus_angles_rad: Vec::with_capacity(problem_spec.n_periods),
            bus_voltage_pu: Vec::with_capacity(problem_spec.n_periods),
            generator_q_mvar: Vec::with_capacity(problem_spec.n_periods),
            bus_q_slack_pos_mvar: Vec::new(),
            bus_q_slack_neg_mvar: Vec::new(),
            bus_p_slack_pos_mw: Vec::new(),
            bus_p_slack_neg_mw: Vec::new(),
            thermal_limit_slack_from_mva: Vec::new(),
            thermal_limit_slack_to_mva: Vec::new(),
            bus_vm_slack_high_pu: Vec::new(),
            bus_vm_slack_low_pu: Vec::new(),
            angle_diff_slack_high_rad: Vec::new(),
            angle_diff_slack_low_rad: Vec::new(),
            storage_gi,
            exact_dispatch_overrides_by_period,
            state: SequentialDispatchState::from_initial_state(problem_spec.initial_state),
            ac_sced_period_timings: Vec::with_capacity(problem_spec.n_periods),
            ac_opf_stats: Vec::with_capacity(problem_spec.n_periods),
        }
    }

    pub(crate) fn period_context<'a>(
        &'a self,
        period: usize,
        next_period_commitment: Option<&'a [bool]>,
    ) -> DispatchPeriodContext<'a> {
        self.state.period_context(period, next_period_commitment)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_period(
        &mut self,
        network: &Network,
        period: RawDispatchPeriodResult,
        hvdc_dispatch_mw: Option<Vec<f64>>,
        iterations: u32,
        bus_angles_rad: Vec<f64>,
        bus_voltage_pu: Vec<f64>,
        generator_q_mvar: Vec<f64>,
        bus_q_slack_pos_mvar: Vec<f64>,
        bus_q_slack_neg_mvar: Vec<f64>,
        bus_p_slack_pos_mw: Vec<f64>,
        bus_p_slack_neg_mw: Vec<f64>,
        thermal_limit_slack_from_mva: Vec<f64>,
        thermal_limit_slack_to_mva: Vec<f64>,
        vm_slack_high_pu: Vec<f64>,
        vm_slack_low_pu: Vec<f64>,
        angle_diff_slack_high_rad: Vec<f64>,
        angle_diff_slack_low_rad: Vec<f64>,
    ) {
        let period_idx = self.periods.len();
        self.state.prev_dispatch_mw = Some(self.next_prev_dispatch(period_idx, &period.pg_mw));
        self.state.prev_dispatch_mask = None;
        if let Some(hvdc_dispatch_mw) = hvdc_dispatch_mw
            && !hvdc_dispatch_mw.is_empty()
        {
            self.state.prev_hvdc_dispatch_mw = Some(hvdc_dispatch_mw);
            self.state.prev_hvdc_dispatch_mask = None;
        }
        if !period.storage_soc_mwh.is_empty() {
            let mut soc_map = HashMap::new();
            for (s, &gi) in self.storage_gi.iter().enumerate() {
                if let Some(&soc) = period.storage_soc_mwh.get(s) {
                    soc_map.insert(gi, soc);
                }
            }
            self.state.storage_soc_override = Some(soc_map);
            push_storage_soc_period(network, &mut self.storage_soc, &period.storage_soc_mwh);
        }
        self.total_cost += period.total_cost;
        self.iterations = self.iterations.saturating_add(iterations);
        self.bus_angles_rad.push(bus_angles_rad);
        self.bus_voltage_pu.push(bus_voltage_pu);
        self.generator_q_mvar.push(generator_q_mvar);
        if !bus_q_slack_pos_mvar.is_empty() {
            self.bus_q_slack_pos_mvar.push(bus_q_slack_pos_mvar);
        }
        if !bus_q_slack_neg_mvar.is_empty() {
            self.bus_q_slack_neg_mvar.push(bus_q_slack_neg_mvar);
        }
        if !bus_p_slack_pos_mw.is_empty() {
            self.bus_p_slack_pos_mw.push(bus_p_slack_pos_mw);
        }
        if !bus_p_slack_neg_mw.is_empty() {
            self.bus_p_slack_neg_mw.push(bus_p_slack_neg_mw);
        }
        if !thermal_limit_slack_from_mva.is_empty() {
            self.thermal_limit_slack_from_mva
                .push(thermal_limit_slack_from_mva);
        }
        if !thermal_limit_slack_to_mva.is_empty() {
            self.thermal_limit_slack_to_mva
                .push(thermal_limit_slack_to_mva);
        }
        if !vm_slack_high_pu.is_empty() {
            self.bus_vm_slack_high_pu.push(vm_slack_high_pu);
        }
        if !vm_slack_low_pu.is_empty() {
            self.bus_vm_slack_low_pu.push(vm_slack_low_pu);
        }
        if !angle_diff_slack_high_rad.is_empty() {
            self.angle_diff_slack_high_rad
                .push(angle_diff_slack_high_rad);
        }
        if !angle_diff_slack_low_rad.is_empty() {
            self.angle_diff_slack_low_rad.push(angle_diff_slack_low_rad);
        }
        self.periods.push(period);
    }

    fn next_prev_dispatch(&self, period_idx: usize, solved_pg_mw: &[f64]) -> Vec<f64> {
        let mut merged = solved_pg_mw.to_vec();
        let Some(overrides) = self.exact_dispatch_overrides_by_period.get(period_idx) else {
            return merged;
        };
        for (idx, maybe_exact_mw) in overrides.iter().enumerate() {
            if let Some(exact_mw) = maybe_exact_mw
                && let Some(target) = merged.get_mut(idx)
            {
                *target = *exact_mw;
            }
        }
        merged
    }

    pub(crate) fn finish(self) -> RawDispatchSolution {
        let mut result = RawDispatchSolution::dispatch_only(
            self.periods,
            self.total_cost,
            self.wall_start.elapsed().as_secs_f64(),
            self.iterations,
            self.storage_soc,
        );
        result.diagnostics.ac_sced_period_timings = self.ac_sced_period_timings;
        result.diagnostics.ac_opf_stats = self.ac_opf_stats;
        result.bus_angles_rad = self.bus_angles_rad;
        result.bus_voltage_pu = self.bus_voltage_pu;
        result.generator_q_mvar = self.generator_q_mvar;
        result.bus_q_slack_pos_mvar = self.bus_q_slack_pos_mvar;
        result.bus_q_slack_neg_mvar = self.bus_q_slack_neg_mvar;
        result.bus_p_slack_pos_mw = self.bus_p_slack_pos_mw;
        result.bus_p_slack_neg_mw = self.bus_p_slack_neg_mw;
        result.thermal_limit_slack_from_mva = self.thermal_limit_slack_from_mva;
        result.thermal_limit_slack_to_mva = self.thermal_limit_slack_to_mva;
        result.bus_vm_slack_high_pu = self.bus_vm_slack_high_pu;
        result.bus_vm_slack_low_pu = self.bus_vm_slack_low_pu;
        result.angle_diff_slack_high_rad = self.angle_diff_slack_high_rad;
        result.angle_diff_slack_low_rad = self.angle_diff_slack_low_rad;
        result
    }
}

fn exact_generator_dispatch_overrides_by_period(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
) -> Vec<Vec<Option<f64>>> {
    let in_service_generator_ids: Vec<&str> = network
        .generators
        .iter()
        .filter(|generator| generator.in_service)
        .map(|generator| generator.id.as_str())
        .collect();
    let profile_by_resource_id: HashMap<&str, &crate::request::GeneratorDispatchBoundsProfile> =
        problem_spec
            .generator_dispatch_bounds
            .profiles
            .iter()
            .map(|profile| (profile.resource_id.as_str(), profile))
            .collect();

    (0..problem_spec.n_periods)
        .map(|period_idx| {
            in_service_generator_ids
                .iter()
                .map(|resource_id| {
                    let profile = profile_by_resource_id.get(resource_id)?;
                    let p_min_mw = profile.p_min_mw.get(period_idx).copied()?;
                    let p_max_mw = profile.p_max_mw.get(period_idx).copied()?;
                    ((p_max_mw - p_min_mw).abs() <= 1e-9).then_some(p_min_mw)
                })
                .collect()
        })
        .collect()
}

/// Decompose an AC SCED LMP vector into (energy, congestion, loss) components.
///
/// Uses AC marginal loss factors computed from the AC operating point
/// (`vm`, `va`) via [`compute_ac_marginal_loss_factors`]. Falls back to a
/// pure-energy decomposition (`mlc = 0`, `mcc = lmp - mec`) if the loss
/// factor solve fails (e.g. multi-island networks where the J^T solve is
/// singular). The fallback is reported as a warning and does not block
/// dispatch result emission.
fn decompose_ac_sced_lmp(
    network: &Network,
    lmp: &[f64],
    vm: &[f64],
    va: &[f64],
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let bus_map = network.bus_index_map();
    let island_refs = surge_opf::advanced::detect_island_refs(network, &bus_map);

    // AC MLF requires a slack bus and a non-singular Jacobian. If the
    // network has no slack bus or the J^T solve fails (e.g. some HVDC
    // multi-island layouts), fall back to the lossless decomposition.
    let mlf_result = network
        .slack_bus_index()
        .ok_or_else(|| "no slack bus".to_string())
        .and_then(|slack_idx| {
            surge_opf::compute_ac_marginal_loss_factors(network, va, vm, slack_idx)
        });

    match mlf_result {
        Ok(mlf) => surge_opf::advanced::decompose_lmp_with_losses(lmp, &mlf, &island_refs),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "AC SCED LMP decomposition: marginal loss factor solve failed; \
                 falling back to lossless decomposition (mlc = 0)"
            );
            surge_opf::advanced::decompose_lmp_lossless(lmp, &island_refs)
        }
    }
}

fn ac_sced_period_to_dispatch_result(
    network: &Network,
    sol: crate::sced::ac::AcScedPeriodSolution,
) -> RawDispatchSolution {
    let (lmp_energy, lmp_congestion, lmp_loss) =
        decompose_ac_sced_lmp(network, &sol.lmp, &sol.bus_voltage_pu, &sol.bus_angle_rad);
    let (reserve_awards, dr_reserve_awards) = build_ac_reactive_reserve_awards(&sol);
    let period = RawDispatchPeriodResult {
        pg_mw: sol.pg_mw,
        lmp: sol.lmp,
        q_lmp: sol.q_lmp,
        lmp_energy,
        lmp_congestion,
        lmp_loss,
        total_cost: sol.total_cost,
        branch_shadow_prices: sol.branch_shadow_prices,
        flowgate_shadow_prices: sol.flowgate_shadow_prices,
        interface_shadow_prices: sol.interface_shadow_prices,
        dr_results: sol.dr_results,
        hvdc_dispatch_mw: sol.hvdc_dispatch_mw,
        hvdc_band_dispatch_mw: sol.hvdc_band_dispatch_mw,
        tap_dispatch: sol.tap_dispatch,
        phase_dispatch: sol.phase_dispatch,
        switched_shunt_dispatch: sol.switched_shunt_dispatch,
        storage_charge_mw: sol
            .storage_net_mw
            .iter()
            .map(|&mw| (-mw).max(0.0))
            .collect(),
        storage_discharge_mw: sol.storage_net_mw.iter().map(|&mw| mw.max(0.0)).collect(),
        storage_soc_mwh: sol.storage_soc_mwh.clone(),
        reserve_awards,
        dr_reserve_awards,
        objective_terms: sol.objective_terms,
        ..RawDispatchPeriodResult::default()
    };
    let mut storage_soc = HashMap::new();
    if !sol.storage_soc_mwh.is_empty() {
        push_storage_soc_period(network, &mut storage_soc, &sol.storage_soc_mwh);
    }
    let mut result = RawDispatchSolution::dispatch_only(
        vec![period],
        sol.total_cost,
        sol.solve_time_secs,
        sol.iterations,
        storage_soc,
    );
    result.bus_angles_rad = vec![sol.bus_angle_rad];
    result.bus_voltage_pu = vec![sol.bus_voltage_pu];
    result.generator_q_mvar = vec![sol.qg_mvar];
    if !sol.bus_q_slack_pos_mvar.is_empty() {
        result.bus_q_slack_pos_mvar = vec![sol.bus_q_slack_pos_mvar];
    }
    if !sol.bus_q_slack_neg_mvar.is_empty() {
        result.bus_q_slack_neg_mvar = vec![sol.bus_q_slack_neg_mvar];
    }
    if !sol.bus_p_slack_pos_mw.is_empty() {
        result.bus_p_slack_pos_mw = vec![sol.bus_p_slack_pos_mw];
    }
    if !sol.bus_p_slack_neg_mw.is_empty() {
        result.bus_p_slack_neg_mw = vec![sol.bus_p_slack_neg_mw];
    }
    if !sol.thermal_limit_slack_from_mva.is_empty() {
        result.thermal_limit_slack_from_mva = vec![sol.thermal_limit_slack_from_mva];
    }
    if !sol.thermal_limit_slack_to_mva.is_empty() {
        result.thermal_limit_slack_to_mva = vec![sol.thermal_limit_slack_to_mva];
    }
    if !sol.vm_slack_high_pu.is_empty() {
        result.bus_vm_slack_high_pu = vec![sol.vm_slack_high_pu];
    }
    if !sol.vm_slack_low_pu.is_empty() {
        result.bus_vm_slack_low_pu = vec![sol.vm_slack_low_pu];
    }
    if !sol.angle_diff_slack_high_rad.is_empty() {
        result.angle_diff_slack_high_rad = vec![sol.angle_diff_slack_high_rad];
    }
    if !sol.angle_diff_slack_low_rad.is_empty() {
        result.angle_diff_slack_low_rad = vec![sol.angle_diff_slack_low_rad];
    }
    result
}

fn build_ac_reactive_reserve_awards(
    sol: &crate::sced::ac::AcScedPeriodSolution,
) -> (HashMap<String, Vec<f64>>, HashMap<String, Vec<f64>>) {
    let mut reserve_awards: HashMap<String, Vec<f64>> = HashMap::new();
    if !sol.producer_q_reserve_up_mvar.is_empty() {
        reserve_awards.insert(
            "q_res_up".to_string(),
            sol.producer_q_reserve_up_mvar.clone(),
        );
    }
    if !sol.producer_q_reserve_down_mvar.is_empty() {
        reserve_awards.insert(
            "q_res_down".to_string(),
            sol.producer_q_reserve_down_mvar.clone(),
        );
    }

    let mut dr_reserve_awards: HashMap<String, Vec<f64>> = HashMap::new();
    if !sol.consumer_q_reserve_up_mvar.is_empty() {
        dr_reserve_awards.insert(
            "q_res_up".to_string(),
            sol.consumer_q_reserve_up_mvar.clone(),
        );
    }
    if !sol.consumer_q_reserve_down_mvar.is_empty() {
        dr_reserve_awards.insert(
            "q_res_down".to_string(),
            sol.consumer_q_reserve_down_mvar.clone(),
        );
    }

    (reserve_awards, dr_reserve_awards)
}

// ---------------------------------------------------------------------------
// solve_dispatch — the unified entry point
// ---------------------------------------------------------------------------

/// Solve a dispatch problem.
pub fn solve_dispatch(
    model: &DispatchModel,
    request: &DispatchRequest,
) -> Result<keyed::DispatchSolution, ScedError> {
    solve_dispatch_with_options(model, request, &DispatchSolveOptions::default())
}

/// Solve a dispatch problem with process-local execution options.
pub fn solve_dispatch_with_options(
    model: &DispatchModel,
    request: &DispatchRequest,
    solve_options: &DispatchSolveOptions,
) -> Result<keyed::DispatchSolution, ScedError> {
    let t = Instant::now();
    let prepared = prepare_dispatch_request(model, request, solve_options)?;
    let prepare_request_secs = t.elapsed().as_secs_f64();

    let t_raw = Instant::now();
    let raw = solve_prepared_dispatch_raw(model, &prepared)?;
    let solve_prepared_raw_total_secs = t_raw.elapsed().as_secs_f64();

    let t = Instant::now();
    let mut keyed = emit_public_keyed_solution(raw, model.network());
    let emit_keyed_secs = t.elapsed().as_secs_f64();

    // Patch pipeline-level phase timings onto the diagnostics so the
    // full wall-clock breakdown (prepare → solve → emit) is visible to
    // callers reading the returned solution. The SCUC/SCED extract path
    // already populates the per-stage internals.
    {
        let diagnostics = keyed.diagnostics_mut();
        let timings = diagnostics
            .phase_timings
            .get_or_insert_with(Default::default);
        timings.prepare_request_secs = prepare_request_secs;
        timings.emit_keyed_secs = emit_keyed_secs;
        timings.solve_prepared_raw_total_secs = solve_prepared_raw_total_secs;
    }
    Ok(keyed)
}

pub(crate) fn prepare_dispatch_request(
    model: &DispatchModel,
    request: &DispatchRequest,
    solve_options: &DispatchSolveOptions,
) -> Result<PreparedDispatchRequest, ScedError> {
    let network = model.network();
    let normalized = request.resolve_with_options(network, solve_options)?;
    validate_dispatch_request_inputs(network, &normalized)?;
    Ok(PreparedDispatchRequest {
        request: request.clone(),
        normalized,
    })
}

pub(crate) fn solve_prepared_dispatch(
    model: &DispatchModel,
    prepared: &PreparedDispatchRequest,
) -> Result<keyed::DispatchSolution, ScedError> {
    let result = solve_prepared_dispatch_raw(model, prepared)?;
    Ok(emit_public_keyed_solution(result, model.network()))
}

fn solve_prepared_dispatch_raw(
    model: &DispatchModel,
    prepared: &PreparedDispatchRequest,
) -> Result<RawDispatchSolution, ScedError> {
    let network = model.network();
    let normalized = &prepared.normalized;
    let mut problem_spec_secs: f64 = 0.0;
    let mut result = if let Some(security) = normalized.security.clone() {
        let security_mode = security.embedding;
        let security_dispatch = security
            .into_security_dispatch_spec(normalized.input.clone(), normalized.commitment.clone());
        match security_mode {
            SecurityEmbedding::ExplicitContingencies => {
                let t = Instant::now();
                let r = crate::scuc::security::solve_explicit_security_dispatch(
                    network,
                    &security_dispatch,
                );
                SCUC_EXTERNAL_WALL_BITS
                    .store(t.elapsed().as_secs_f64().to_bits(), Ordering::SeqCst);
                r
            }
            SecurityEmbedding::IterativeScreening => {
                crate::scuc::security::solve_security_dispatch(network, &security_dispatch)
            }
        }
    } else {
        let t = Instant::now();
        let problem_spec = normalized.problem_spec();
        problem_spec_secs = t.elapsed().as_secs_f64();
        info!(
            formulation = ?normalized.formulation,
            horizon = ?normalized.horizon,
            commitment = match &normalized.commitment {
                CommitmentMode::AllCommitted => "AllCommitted",
                CommitmentMode::Fixed { .. } => "Fixed",
                CommitmentMode::Optimize(_) => "Optimize",
                CommitmentMode::Additional { .. } => "Additional",
            },
            n_periods = normalized.input.n_periods,
            buses = network.n_buses(),
            "dispatch: routing solve"
        );

        match (
            &normalized.formulation,
            &normalized.horizon,
            &normalized.commitment,
        ) {
            (Formulation::Dc, Horizon::Sequential, CommitmentMode::AllCommitted)
            | (Formulation::Dc, Horizon::Sequential, CommitmentMode::Fixed { .. }) => {
                dispatch_dc_sequential(network, problem_spec)
            }
            (Formulation::Dc, Horizon::TimeCoupled, CommitmentMode::AllCommitted)
            | (Formulation::Dc, Horizon::TimeCoupled, CommitmentMode::Fixed { .. }) => {
                dispatch_dc_time_coupled(network, problem_spec)
            }
            (Formulation::Dc, Horizon::TimeCoupled, CommitmentMode::Optimize(_))
            | (Formulation::Dc, Horizon::TimeCoupled, CommitmentMode::Additional { .. }) => {
                let t_scuc_call = Instant::now();
                let r = crate::scuc::solve_scuc_with_problem_spec(network, problem_spec);
                // Caller-side wall includes the Rust function return
                // epilogue (move of the returned RawDispatchSolution,
                // destructors of function-scoped locals like
                // `model_plan`) that fn-local instrumentation can't see.
                SCUC_EXTERNAL_WALL_BITS.store(
                    t_scuc_call.elapsed().as_secs_f64().to_bits(),
                    Ordering::SeqCst,
                );
                r
            }
            (Formulation::Dc, Horizon::Sequential, CommitmentMode::Optimize(_))
            | (Formulation::Dc, Horizon::Sequential, CommitmentMode::Additional { .. }) => {
                Err(ScedError::SolverError(
                    "commitment optimization requires TimeCoupled horizon".to_string(),
                ))
            }
            (Formulation::Ac, Horizon::Sequential, CommitmentMode::AllCommitted)
            | (Formulation::Ac, Horizon::Sequential, CommitmentMode::Fixed { .. }) => {
                dispatch_ac_sequential(
                    network,
                    problem_spec,
                    &normalized.ac_opf,
                    &normalized.ac_opf_runtime,
                )
            }
            (Formulation::Ac, Horizon::TimeCoupled, _) => Err(ScedError::SolverError(
                "AC time-coupled dispatch requires stacked NLP — not yet implemented".to_string(),
            )),
            (Formulation::Ac, Horizon::Sequential, CommitmentMode::Optimize(_))
            | (Formulation::Ac, Horizon::Sequential, CommitmentMode::Additional { .. }) => {
                Err(ScedError::SolverError(
                    "commitment optimization with AC formulation is not supported".to_string(),
                ))
            }
        }
    }?;
    let t = Instant::now();
    attach_public_catalogs_and_solve_metadata(&mut result, network, normalized);
    let attach_catalogs_secs = t.elapsed().as_secs_f64();

    let t = Instant::now();
    attach_keyed_period_views(&mut result, network, normalized);
    let attach_views_secs = t.elapsed().as_secs_f64();

    // Fold the post-solve attach wall times into the phase_timings block
    // so callers can see where the non-optimizer wall went.
    let scuc_external_secs = f64::from_bits(SCUC_EXTERNAL_WALL_BITS.swap(0, Ordering::SeqCst));
    {
        let timings = result
            .diagnostics
            .phase_timings
            .get_or_insert_with(Default::default);
        timings.problem_spec_secs = problem_spec_secs;
        timings.attach_public_catalogs_secs = attach_catalogs_secs;
        timings.attach_keyed_period_views_secs = attach_views_secs;
        timings.solve_scuc_external_secs = scuc_external_secs;
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) fn solve_dispatch_raw(
    network: &Network,
    request: &DispatchRequest,
) -> Result<RawDispatchSolution, ScedError> {
    let model = DispatchModel::prepare(network)?;
    let prepared = prepare_dispatch_request(&model, request, &DispatchSolveOptions::default())?;
    solve_prepared_dispatch_raw(&model, &prepared)
}

fn validate_dispatch_request_inputs(
    network: &Network,
    normalized: &NormalizedDispatchRequest,
) -> Result<(), ScedError> {
    let problem_spec = normalized.problem_spec();
    let catalog = DispatchCatalog::from_network(network, &normalized.input.dispatchable_loads);
    let n_in_service_gens = catalog.in_service_gen_indices.len();
    let n_buses = network.buses.len();

    validate_load_profiles(network, &problem_spec)?;
    validate_ac_bus_load_profiles(network, &problem_spec)?;
    validate_generator_dispatch_economics(network, &problem_spec, &catalog)?;
    validate_generator_profile_targets(network, &problem_spec)?;
    validate_branch_derate_profiles(network, &problem_spec)?;
    validate_hvdc_derate_profiles(network, &problem_spec)?;
    validate_initial_state(network, &problem_spec, n_in_service_gens)?;
    validate_reserve_configuration(network, &problem_spec)?;
    validate_virtual_bids(network, &problem_spec)?;
    validate_area_configuration(&problem_spec, n_in_service_gens, n_buses)?;
    validate_security_screening(network, normalized)?;
    validate_commitment_inputs(
        network,
        &normalized.commitment,
        problem_spec.n_periods,
        &catalog.in_service_gen_indices,
    )?;
    validate_startup_window_limits(&problem_spec, n_in_service_gens)?;
    validate_energy_window_limits(&problem_spec, n_in_service_gens)?;
    validate_peak_demand_charges(&problem_spec, n_in_service_gens)?;
    validate_commitment_constraints(&problem_spec, n_in_service_gens)?;
    validate_storage_inputs(network, &problem_spec)?;
    Ok(())
}

fn validate_generator_dispatch_economics(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
    catalog: &DispatchCatalog,
) -> Result<(), ScedError> {
    for &global_gen_idx in &catalog.in_service_gen_indices {
        let generator = &network.generators[global_gen_idx];
        if generator.is_storage() {
            validate_storage_dispatch_economics(global_gen_idx, generator)?;
            continue;
        }

        let missing_period = (0..problem_spec.n_periods).find(|&period| {
            resolve_generator_economics_for_period(
                global_gen_idx,
                period,
                generator,
                problem_spec.offer_schedules,
                Some(generator.pmax),
            )
            .is_none()
        });

        if let Some(period) = missing_period {
            if generator.cost.is_none() {
                return if problem_spec.offer_schedules.contains_key(&global_gen_idx) {
                    Err(ScedError::InvalidInput(format!(
                        "generator {global_gen_idx} (bus {}) is missing dispatch economics for period {period}; provide Generator.cost or an offer_schedules override for every period",
                        generator.bus
                    )))
                } else {
                    Err(ScedError::MissingCost {
                        gen_idx: global_gen_idx,
                        bus: generator.bus,
                    })
                };
            }
        }
    }
    Ok(())
}

fn validate_storage_dispatch_economics(
    global_gen_idx: usize,
    generator: &surge_network::network::Generator,
) -> Result<(), ScedError> {
    let Some(storage) = generator.storage.as_ref() else {
        return Ok(());
    };

    match storage.dispatch_mode {
        surge_network::network::StorageDispatchMode::CostMinimization => Ok(()),
        surge_network::network::StorageDispatchMode::SelfSchedule => {
            if let Some(points) = storage.discharge_offer.as_deref() {
                validate_storage_offer_curve_points(
                    points,
                    &format!("storage generator {global_gen_idx} discharge_offer"),
                )
                .map_err(ScedError::InvalidInput)?;
            }
            if let Some(points) = storage.charge_bid.as_deref() {
                validate_storage_offer_curve_points(
                    points,
                    &format!("storage generator {global_gen_idx} charge_bid"),
                )
                .map_err(ScedError::InvalidInput)?;
            }
            Ok(())
        }
        surge_network::network::StorageDispatchMode::OfferCurve => {
            if let Some(points) = storage.discharge_offer.as_deref() {
                validate_storage_offer_curve_points(
                    points,
                    &format!("storage generator {global_gen_idx} discharge_offer"),
                )
                .map_err(ScedError::InvalidInput)?;
            }
            if let Some(points) = storage.charge_bid.as_deref() {
                validate_storage_offer_curve_points(
                    points,
                    &format!("storage generator {global_gen_idx} charge_bid"),
                )
                .map_err(ScedError::InvalidInput)?;
            }
            if generator.pmax > 0.0 && storage.discharge_offer.is_none() {
                return Err(ScedError::InvalidInput(format!(
                    "storage generator {global_gen_idx} in OfferCurve mode requires discharge_offer when pmax > 0"
                )));
            }
            if generator.pmin < 0.0 && storage.charge_bid.is_none() {
                return Err(ScedError::InvalidInput(format!(
                    "storage generator {global_gen_idx} in OfferCurve mode requires charge_bid when pmin < 0"
                )));
            }
            Ok(())
        }
    }
}

fn validate_exact_length(
    context: &str,
    actual_len: usize,
    expected_len: usize,
) -> Result<(), ScedError> {
    if actual_len != expected_len {
        return Err(ScedError::InvalidInput(format!(
            "{context} length {actual_len} does not match n_periods {expected_len}"
        )));
    }
    Ok(())
}

fn validate_profile_collection_length(
    context: &str,
    declared_timesteps: usize,
    expected_timesteps: usize,
) -> Result<(), ScedError> {
    if declared_timesteps != expected_timesteps {
        return Err(ScedError::InvalidInput(format!(
            "{context}.n_timesteps {declared_timesteps} does not match n_periods {expected_timesteps}"
        )));
    }
    Ok(())
}

fn validate_load_profiles(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    if !problem_spec.load_profiles.profiles.is_empty() {
        validate_profile_collection_length(
            "load_profiles",
            problem_spec.load_profiles.n_timesteps,
            problem_spec.n_periods,
        )?;
    }
    let bus_map = network.bus_index_map();
    let mut seen = HashSet::new();
    for profile in &problem_spec.load_profiles.profiles {
        if !seen.insert(profile.bus) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate load profile for bus {}",
                profile.bus
            )));
        }
        if !bus_map.contains_key(&profile.bus) {
            return Err(ScedError::InvalidInput(format!(
                "unknown bus {} in load_profiles",
                profile.bus
            )));
        }
        validate_exact_length(
            &format!("load profile for bus {}", profile.bus),
            profile.load_mw.len(),
            problem_spec.n_periods,
        )?;
        for (period, &load_mw) in profile.load_mw.iter().enumerate() {
            if !load_mw.is_finite() {
                return Err(ScedError::InvalidInput(format!(
                    "load profile for bus {} has non-finite value at period {}",
                    profile.bus, period
                )));
            }
        }
    }
    Ok(())
}

fn validate_ac_bus_load_profiles(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    let bus_map = network.bus_index_map();
    let active_load_profile_buses: HashSet<u32> = problem_spec
        .load_profiles
        .profiles
        .iter()
        .map(|profile| profile.bus)
        .collect();
    let mut seen = HashSet::new();
    for profile in &problem_spec.ac_bus_load_profiles.profiles {
        if !seen.insert(profile.bus_number) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate AC bus load profile for bus {}",
                profile.bus_number
            )));
        }
        if !bus_map.contains_key(&profile.bus_number) {
            return Err(ScedError::InvalidInput(format!(
                "unknown bus {} in ac_bus_load_profiles",
                profile.bus_number
            )));
        }
        if profile.p_mw.is_none() && profile.q_mvar.is_none() {
            return Err(ScedError::InvalidInput(format!(
                "AC bus load profile for bus {} must provide at least one of p_mw or q_mvar",
                profile.bus_number
            )));
        }
        if profile.p_mw.is_some() && active_load_profile_buses.contains(&profile.bus_number) {
            return Err(ScedError::InvalidInput(format!(
                "AC bus load profile for bus {} duplicates the active-power source from load_profiles; use either load_profiles or ac_bus_load_profiles.p_mw for that bus",
                profile.bus_number
            )));
        }
        if let Some(p_mw) = profile.p_mw.as_ref() {
            validate_exact_length(
                &format!("ac_bus_load_profiles[bus={}].p_mw", profile.bus_number),
                p_mw.len(),
                problem_spec.n_periods,
            )?;
            for (period, &value_mw) in p_mw.iter().enumerate() {
                if !value_mw.is_finite() {
                    return Err(ScedError::InvalidInput(format!(
                        "ac_bus_load_profiles[bus={}].p_mw[{period}] must be finite",
                        profile.bus_number
                    )));
                }
            }
        }
        if let Some(q_mvar) = profile.q_mvar.as_ref() {
            validate_exact_length(
                &format!("ac_bus_load_profiles[bus={}].q_mvar", profile.bus_number),
                q_mvar.len(),
                problem_spec.n_periods,
            )?;
            for (period, &value_mvar) in q_mvar.iter().enumerate() {
                if !value_mvar.is_finite() {
                    return Err(ScedError::InvalidInput(format!(
                        "ac_bus_load_profiles[bus={}].q_mvar[{period}] must be finite",
                        profile.bus_number
                    )));
                }
            }
        }
    }

    Ok(())
}

fn validate_generator_profile_targets(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    let gen_index_by_id = network.gen_index_by_id();

    if !problem_spec.gen_derate_profiles.profiles.is_empty() {
        validate_profile_collection_length(
            "gen_derate_profiles",
            problem_spec.gen_derate_profiles.n_timesteps,
            problem_spec.n_periods,
        )?;
    }
    let mut seen_derates = HashSet::new();
    for profile in &problem_spec.gen_derate_profiles.profiles {
        if !seen_derates.insert(profile.generator_id.clone()) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate generator derate profile for generator_id {}",
                profile.generator_id
            )));
        }
        if !gen_index_by_id.contains_key(&profile.generator_id) {
            return Err(ScedError::InvalidInput(format!(
                "unknown generator_id {} in generator derate profiles",
                profile.generator_id
            )));
        }
        validate_exact_length(
            &format!(
                "generator derate profile for generator_id {}",
                profile.generator_id
            ),
            profile.derate_factors.len(),
            problem_spec.n_periods,
        )?;
        for (period, &derate) in profile.derate_factors.iter().enumerate() {
            if !(0.0..=1.0).contains(&derate) {
                return Err(ScedError::InvalidInput(format!(
                    "invalid generator derate factor {} for generator_id {} at period {}; expected 0.0..=1.0",
                    derate, profile.generator_id, period
                )));
            }
        }
    }

    if !problem_spec.renewable_profiles.profiles.is_empty() {
        validate_profile_collection_length(
            "renewable_profiles",
            problem_spec.renewable_profiles.n_timesteps,
            problem_spec.n_periods,
        )?;
    }
    let mut seen_renewables = HashSet::new();
    for profile in &problem_spec.renewable_profiles.profiles {
        if !seen_renewables.insert(profile.generator_id.clone()) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate renewable profile for generator_id {}",
                profile.generator_id
            )));
        }
        for (period, &capacity_factor) in profile.capacity_factors.iter().enumerate() {
            if !(0.0..=1.0).contains(&capacity_factor) {
                return Err(ScedError::InvalidInput(format!(
                    "invalid renewable capacity factor {} for generator_id {} at period {}; expected 0.0..=1.0",
                    capacity_factor, profile.generator_id, period
                )));
            }
        }
        if !gen_index_by_id.contains_key(&profile.generator_id) {
            return Err(ScedError::InvalidInput(format!(
                "unknown generator_id {} in renewable profiles",
                profile.generator_id
            )));
        }
        validate_exact_length(
            &format!(
                "renewable profile for generator_id {}",
                profile.generator_id
            ),
            profile.capacity_factors.len(),
            problem_spec.n_periods,
        )?;
    }

    Ok(())
}

fn validate_branch_derate_profiles(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    if !problem_spec.branch_derate_profiles.profiles.is_empty() {
        validate_profile_collection_length(
            "branch_derate_profiles",
            problem_spec.branch_derate_profiles.n_timesteps,
            problem_spec.n_periods,
        )?;
    }
    let branch_map = network.branch_index_map();
    let mut seen = HashSet::new();
    for profile in &problem_spec.branch_derate_profiles.profiles {
        let key = (profile.from_bus, profile.to_bus, profile.circuit.clone());
        if !seen.insert(key.clone()) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate branch derate profile for branch ({}, {}, {})",
                profile.from_bus, profile.to_bus, profile.circuit
            )));
        }
        if !branch_map.contains_key(&key) {
            return Err(ScedError::InvalidInput(format!(
                "unknown branch ({}, {}, {}) in branch_derate_profiles",
                profile.from_bus, profile.to_bus, profile.circuit
            )));
        }
        validate_exact_length(
            &format!(
                "branch derate profile for branch ({}, {}, {})",
                profile.from_bus, profile.to_bus, profile.circuit
            ),
            profile.derate_factors.len(),
            problem_spec.n_periods,
        )?;
        for (period, &derate) in profile.derate_factors.iter().enumerate() {
            // Factors below 0 are illegal. Factors above 1 act as uprates
            // (e.g. relaxing a thermal limit to absorb DC-stage slack
            // before AC SCED takes over). Reject NaN/negative only.
            if !derate.is_finite() || derate < 0.0 {
                return Err(ScedError::InvalidInput(format!(
                    "invalid branch derate factor {} for branch ({}, {}, {}) at period {}; expected finite >= 0.0",
                    derate, profile.from_bus, profile.to_bus, profile.circuit, period
                )));
            }
        }
    }
    Ok(())
}

fn validate_hvdc_derate_profiles(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    if !problem_spec.hvdc_derate_profiles.profiles.is_empty() {
        validate_profile_collection_length(
            "hvdc_derate_profiles",
            problem_spec.hvdc_derate_profiles.n_timesteps,
            problem_spec.n_periods,
        )?;
    }
    let dc_names: HashSet<&str> = network
        .hvdc
        .links
        .iter()
        .filter_map(|link| link.as_lcc().map(|line| line.name.as_str()))
        .collect();
    let mut seen = HashSet::new();
    for profile in &problem_spec.hvdc_derate_profiles.profiles {
        if !seen.insert(profile.name.clone()) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate hvdc derate profile for line {}",
                profile.name
            )));
        }
        if !dc_names.contains(profile.name.as_str()) {
            return Err(ScedError::InvalidInput(format!(
                "unknown HVDC line {} in hvdc_derate_profiles",
                profile.name
            )));
        }
        validate_exact_length(
            &format!("HVDC derate profile for line {}", profile.name),
            profile.derate_factors.len(),
            problem_spec.n_periods,
        )?;
        for (period, &derate) in profile.derate_factors.iter().enumerate() {
            if !(0.0..=1.0).contains(&derate) {
                return Err(ScedError::InvalidInput(format!(
                    "invalid HVDC derate factor {} for line {} at period {}; expected 0.0..=1.0",
                    derate, profile.name, period
                )));
            }
        }
    }
    Ok(())
}

fn validate_initial_state(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
    n_in_service_gens: usize,
) -> Result<(), ScedError> {
    if let Some(prev_dispatch_mw) = problem_spec.initial_state.prev_dispatch_mw.as_ref() {
        if prev_dispatch_mw.len() != n_in_service_gens {
            return Err(ScedError::InvalidInput(format!(
                "prev_dispatch_mw length {} does not match in-service generator count {}",
                prev_dispatch_mw.len(),
                n_in_service_gens
            )));
        }
        if let Some(mask) = problem_spec.initial_state.prev_dispatch_mask.as_ref()
            && mask.len() != n_in_service_gens
        {
            return Err(ScedError::InvalidInput(format!(
                "prev_dispatch_mw mask length {} does not match in-service generator count {}",
                mask.len(),
                n_in_service_gens
            )));
        }
        for gen_idx in 0..n_in_service_gens {
            if let Some(pg_mw) = problem_spec.initial_state.prev_dispatch_at(gen_idx)
                && !pg_mw.is_finite()
            {
                return Err(ScedError::InvalidInput(format!(
                    "prev_dispatch_mw has non-finite value at generator {}",
                    gen_idx
                )));
            }
        }
    }
    if let Some(prev_hvdc_dispatch_mw) = problem_spec.initial_state.prev_hvdc_dispatch_mw.as_ref() {
        let expected_len = problem_spec.hvdc_links.len();
        if prev_hvdc_dispatch_mw.len() != expected_len {
            return Err(ScedError::InvalidInput(format!(
                "prev_hvdc_dispatch_mw length {} does not match hvdc_links count {}",
                prev_hvdc_dispatch_mw.len(),
                expected_len
            )));
        }
        if let Some(mask) = problem_spec.initial_state.prev_hvdc_dispatch_mask.as_ref()
            && mask.len() != expected_len
        {
            return Err(ScedError::InvalidInput(format!(
                "prev_hvdc_dispatch_mw mask length {} does not match hvdc_links count {}",
                mask.len(),
                expected_len
            )));
        }
        for link_idx in 0..expected_len {
            if let Some(dispatch_mw) = problem_spec.initial_state.prev_hvdc_dispatch_at(link_idx)
                && !dispatch_mw.is_finite()
            {
                return Err(ScedError::InvalidInput(format!(
                    "prev_hvdc_dispatch_mw has non-finite value at HVDC link {}",
                    link_idx
                )));
            }
        }
    }
    if let Some(storage_soc_override) = problem_spec.initial_state.storage_soc_override.as_ref() {
        for (&gen_idx, &soc_mwh) in storage_soc_override {
            let Some(generator) = network.generators.get(gen_idx) else {
                return Err(ScedError::InvalidInput(format!(
                    "storage_soc_override references unknown generator index {}",
                    gen_idx
                )));
            };
            if !generator.is_storage() {
                return Err(ScedError::InvalidInput(format!(
                    "storage_soc_override generator index {} is not a storage unit",
                    gen_idx
                )));
            }
            if !soc_mwh.is_finite() {
                return Err(ScedError::InvalidInput(format!(
                    "storage_soc_override for generator index {} is non-finite",
                    gen_idx
                )));
            }
        }
    }
    Ok(())
}

fn validate_reserve_configuration(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    let mut product_ids = HashSet::new();
    let mut system_requirement_product_ids = HashSet::new();
    let bus_map = network.bus_index_map();
    for product in problem_spec.reserve_products {
        if product.id.is_empty() {
            return Err(ScedError::InvalidInput(
                "reserve_products entries require a non-empty id".to_string(),
            ));
        }
        if !product_ids.insert(product.id.as_str()) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate reserve product id {}",
                product.id
            )));
        }
    }
    for product in problem_spec.reserve_products {
        let mut seen_balance = HashSet::new();
        for dep in &product.balance_products {
            if dep.is_empty() {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} has an empty balance_products entry",
                    product.id
                )));
            }
            if dep == &product.id {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} cannot list itself in balance_products",
                    product.id
                )));
            }
            if !product_ids.contains(dep.as_str()) {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} balance_products references unknown product_id {}",
                    product.id, dep
                )));
            }
            if !seen_balance.insert(dep.as_str()) {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} balance_products contains duplicate product_id {}",
                    product.id, dep
                )));
            }
        }
        let mut seen_shared = HashSet::new();
        for dep in &product.shared_limit_products {
            if dep.is_empty() {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} has an empty shared_limit_products entry",
                    product.id
                )));
            }
            if dep == &product.id {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} cannot list itself in shared_limit_products",
                    product.id
                )));
            }
            if !product_ids.contains(dep.as_str()) {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} shared_limit_products references unknown product_id {}",
                    product.id, dep
                )));
            }
            if !seen_shared.insert(dep.as_str()) {
                return Err(ScedError::InvalidInput(format!(
                    "reserve product {} shared_limit_products contains duplicate product_id {}",
                    product.id, dep
                )));
            }
        }
    }

    for requirement in problem_spec.system_reserve_requirements {
        if !system_requirement_product_ids.insert(requirement.product_id.as_str()) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate system reserve requirement for product {}",
                requirement.product_id
            )));
        }
        if !product_ids.contains(requirement.product_id.as_str()) {
            return Err(ScedError::InvalidInput(format!(
                "system_reserve_requirements references unknown product_id {}",
                requirement.product_id
            )));
        }
        if !(requirement.requirement_mw.is_finite() && requirement.requirement_mw >= 0.0) {
            return Err(ScedError::InvalidInput(format!(
                "system reserve requirement for product {} must be finite and >= 0, got {}",
                requirement.product_id, requirement.requirement_mw
            )));
        }
        if let Some(per_period_mw) = requirement.per_period_mw.as_ref() {
            validate_exact_length(
                &format!(
                    "system reserve per-period requirement for product {}",
                    requirement.product_id
                ),
                per_period_mw.len(),
                problem_spec.n_periods,
            )?;
            for (period, &requirement_mw) in per_period_mw.iter().enumerate() {
                if !(requirement_mw.is_finite() && requirement_mw >= 0.0) {
                    return Err(ScedError::InvalidInput(format!(
                        "system reserve requirement for product {} has invalid value {} at period {}; expected finite and >= 0",
                        requirement.product_id, requirement_mw, period
                    )));
                }
            }
        }
    }

    for requirement in problem_spec.zonal_reserve_requirements {
        if !product_ids.contains(requirement.product_id.as_str()) {
            return Err(ScedError::InvalidInput(format!(
                "zonal_reserve_requirements references unknown product_id {}",
                requirement.product_id
            )));
        }
        if !(requirement.requirement_mw.is_finite() && requirement.requirement_mw >= 0.0) {
            return Err(ScedError::InvalidInput(format!(
                "zonal reserve requirement for product {} in zone {} must be finite and >= 0, got {}",
                requirement.product_id, requirement.zone_id, requirement.requirement_mw
            )));
        }
        if let Some(per_period_mw) = requirement.per_period_mw.as_ref() {
            validate_exact_length(
                &format!(
                    "zonal reserve per-period requirement for product {} zone {}",
                    requirement.product_id, requirement.zone_id
                ),
                per_period_mw.len(),
                problem_spec.n_periods,
            )?;
            for (period, &requirement_mw) in per_period_mw.iter().enumerate() {
                if !(requirement_mw.is_finite() && requirement_mw >= 0.0) {
                    return Err(ScedError::InvalidInput(format!(
                        "zonal reserve requirement for product {} zone {} has invalid value {} at period {}; expected finite and >= 0",
                        requirement.product_id, requirement.zone_id, requirement_mw, period
                    )));
                }
            }
        }
        if let Some(cost) = requirement.shortfall_cost_per_unit
            && !(cost.is_finite() && cost >= 0.0)
        {
            return Err(ScedError::InvalidInput(format!(
                "zonal reserve shortfall cost for product {} in zone {} must be finite and >= 0, got {}",
                requirement.product_id, requirement.zone_id, cost
            )));
        }
        if let Some(coeff) = requirement.served_dispatchable_load_coefficient
            && !(coeff.is_finite() && coeff >= 0.0)
        {
            return Err(ScedError::InvalidInput(format!(
                "zonal reserve served_dispatchable_load_coefficient for product {} in zone {} must be finite and >= 0, got {}",
                requirement.product_id, requirement.zone_id, coeff
            )));
        }
        if let Some(coeff) = requirement.largest_generator_dispatch_coefficient
            && !(coeff.is_finite() && coeff >= 0.0)
        {
            return Err(ScedError::InvalidInput(format!(
                "zonal reserve largest_generator_dispatch_coefficient for product {} in zone {} must be finite and >= 0, got {}",
                requirement.product_id, requirement.zone_id, coeff
            )));
        }
        if let Some(participant_bus_numbers) = requirement.participant_bus_numbers.as_ref() {
            let mut seen_buses = HashSet::new();
            for &bus_number in participant_bus_numbers {
                if !bus_map.contains_key(&bus_number) {
                    return Err(ScedError::InvalidInput(format!(
                        "zonal reserve requirement for product {} zone {} references unknown participant bus {}",
                        requirement.product_id, requirement.zone_id, bus_number
                    )));
                }
                if !seen_buses.insert(bus_number) {
                    return Err(ScedError::InvalidInput(format!(
                        "zonal reserve requirement for product {} zone {} contains duplicate participant bus {}",
                        requirement.product_id, requirement.zone_id, bus_number
                    )));
                }
            }
        }
    }

    Ok(())
}

fn validate_virtual_bids(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    let bus_map = network.bus_index_map();
    let mut seen: HashSet<(&str, u32, usize)> = HashSet::new();
    for bid in problem_spec.virtual_bids {
        if !seen.insert((bid.position_id.as_str(), bid.bus, bid.period)) {
            return Err(ScedError::InvalidInput(format!(
                "duplicate virtual bid position_id {} at bus {} period {}",
                bid.position_id, bid.bus, bid.period
            )));
        }
        if !bus_map.contains_key(&bid.bus) {
            return Err(ScedError::InvalidInput(format!(
                "virtual bid {} references unknown bus {}",
                bid.position_id, bid.bus
            )));
        }
        if bid.period >= problem_spec.n_periods {
            return Err(ScedError::InvalidInput(format!(
                "virtual bid {} period {} is out of range for n_periods {}",
                bid.position_id, bid.period, problem_spec.n_periods
            )));
        }
        if !(bid.mw_limit.is_finite() && bid.mw_limit >= 0.0) {
            return Err(ScedError::InvalidInput(format!(
                "virtual bid {} has invalid mw_limit {}; expected finite and >= 0",
                bid.position_id, bid.mw_limit
            )));
        }
        if !bid.price_per_mwh.is_finite() {
            return Err(ScedError::InvalidInput(format!(
                "virtual bid {} has non-finite price_per_mwh {}",
                bid.position_id, bid.price_per_mwh
            )));
        }
    }
    Ok(())
}

fn validate_area_configuration(
    problem_spec: &DispatchProblemSpec<'_>,
    n_in_service_gens: usize,
    n_buses: usize,
) -> Result<(), ScedError> {
    if !problem_spec.generator_area.is_empty()
        && problem_spec.generator_area.len() != n_in_service_gens
    {
        return Err(ScedError::InvalidInput(format!(
            "generator_area length {} does not match in-service generator count {}",
            problem_spec.generator_area.len(),
            n_in_service_gens
        )));
    }
    if !problem_spec.load_area.is_empty() && problem_spec.load_area.len() != n_buses {
        return Err(ScedError::InvalidInput(format!(
            "load_area length {} does not match bus count {}",
            problem_spec.load_area.len(),
            n_buses
        )));
    }
    if problem_spec
        .regulation_eligible
        .is_some_and(|flags| flags.len() != n_in_service_gens)
    {
        return Err(ScedError::InvalidInput(format!(
            "regulation_eligible length {} does not match in-service generator count {}",
            problem_spec
                .regulation_eligible
                .expect("regulation_eligible checked is_some_and above")
                .len(),
            n_in_service_gens
        )));
    }
    if !problem_spec.zonal_reserve_requirements.is_empty()
        && problem_spec.generator_area.is_empty()
        && problem_spec
            .zonal_reserve_requirements
            .iter()
            .any(|req| !req.has_explicit_participant_buses())
    {
        return Err(ScedError::InvalidInput(
            "zonal_reserve_requirements require generator_area to be provided".to_string(),
        ));
    }
    if let Some(tie_line_limits) = problem_spec.tie_line_limits {
        if problem_spec.load_area.is_empty() {
            return Err(ScedError::InvalidInput(
                "tie_line_limits require load_area to be provided".to_string(),
            ));
        }
        let known_areas: HashSet<usize> = problem_spec.load_area.iter().copied().collect();
        for (&(from_area, to_area), &limit_mw) in &tie_line_limits.limits_mw {
            if from_area == to_area {
                return Err(ScedError::InvalidInput(format!(
                    "tie-line limit ({from_area}, {to_area}) must span two distinct areas"
                )));
            }
            if !(limit_mw.is_finite() && limit_mw >= 0.0) {
                return Err(ScedError::InvalidInput(format!(
                    "tie-line limit ({from_area}, {to_area}) must be finite and >= 0, got {limit_mw}"
                )));
            }
            if !known_areas.contains(&from_area) || !known_areas.contains(&to_area) {
                return Err(ScedError::InvalidInput(format!(
                    "tie-line limit ({from_area}, {to_area}) references an area that is not present in load_area"
                )));
            }
        }
    }
    Ok(())
}

fn validate_security_screening(
    network: &Network,
    normalized: &NormalizedDispatchRequest,
) -> Result<(), ScedError> {
    let Some(security) = normalized.security.as_ref() else {
        return Ok(());
    };

    if !security.violation_tolerance_pu.is_finite() || security.violation_tolerance_pu < 0.0 {
        return Err(ScedError::InvalidInput(format!(
            "security.violation_tolerance_pu must be finite and >= 0, got {}",
            security.violation_tolerance_pu
        )));
    }
    if security.embedding == SecurityEmbedding::IterativeScreening
        && security.max_iterations > 0
        && security.max_cuts_per_iteration == 0
    {
        return Err(ScedError::InvalidInput(
            "security.max_cuts_per_iteration must be > 0 when security.max_iterations is nonzero"
                .to_string(),
        ));
    }

    let mut seen_branch_indices = HashSet::new();
    for &branch_idx in &security.contingency_branches {
        if branch_idx >= network.branches.len() {
            return Err(ScedError::InvalidInput(format!(
                "security.contingency_branches references unknown branch index {}",
                branch_idx
            )));
        }
        if !seen_branch_indices.insert(branch_idx) {
            return Err(ScedError::InvalidInput(format!(
                "security.contingency_branches contains duplicate branch index {}",
                branch_idx
            )));
        }
    }

    let mut seen_hvdc_indices = HashSet::new();
    for &hvdc_idx in &security.hvdc_contingency_indices {
        if hvdc_idx >= normalized.input.hvdc_links.len() {
            return Err(ScedError::InvalidInput(format!(
                "security.hvdc_contingency_indices references unknown HVDC link index {}",
                hvdc_idx
            )));
        }
        if !seen_hvdc_indices.insert(hvdc_idx) {
            return Err(ScedError::InvalidInput(format!(
                "security.hvdc_contingency_indices contains duplicate HVDC link index {}",
                hvdc_idx
            )));
        }
    }

    Ok(())
}

fn validate_commitment_vector_length(
    context: &str,
    values: &[bool],
    expected_len: usize,
) -> Result<(), ScedError> {
    if values.len() != expected_len {
        return Err(ScedError::InvalidInput(format!(
            "{context} length {} does not match in-service generator count {}",
            values.len(),
            expected_len
        )));
    }
    Ok(())
}

fn validate_commitment_option_length<T>(
    context: &str,
    values: &[T],
    expected_len: usize,
) -> Result<(), ScedError> {
    if values.len() != expected_len {
        return Err(ScedError::InvalidInput(format!(
            "{context} length {} does not match in-service generator count {}",
            values.len(),
            expected_len
        )));
    }
    Ok(())
}

fn validate_sparse_mask_length(
    context: &str,
    mask: Option<&[bool]>,
    expected_len: usize,
) -> Result<(), ScedError> {
    if let Some(mask) = mask
        && mask.len() != expected_len
    {
        return Err(ScedError::InvalidInput(format!(
            "{context} mask length {} does not match in-service generator count {}",
            mask.len(),
            expected_len
        )));
    }
    Ok(())
}

fn validate_storage_commitment_mask(
    context: &str,
    schedule: &[bool],
    network: &Network,
    in_service_gen_indices: &[usize],
) -> Result<(), ScedError> {
    for (local_idx, &network_gen_idx) in in_service_gen_indices.iter().enumerate() {
        if network.generators[network_gen_idx].is_storage() && !schedule[local_idx] {
            return Err(ScedError::InvalidInput(format!(
                "{context} cannot turn storage generator {} off; storage units are continuously dispatchable in the dispatch model",
                local_idx
            )));
        }
    }
    Ok(())
}

fn validate_commitment_options(
    options: &IndexedCommitmentOptions,
    expected_len: usize,
) -> Result<(), ScedError> {
    if let Some(initial_commitment) = options.initial_commitment.as_deref() {
        validate_commitment_vector_length("initial_commitment", initial_commitment, expected_len)?;
    }
    validate_sparse_mask_length(
        "initial_commitment",
        options.initial_commitment_mask.as_deref(),
        expected_len,
    )?;
    if let Some(initial_hours_on) = options.initial_hours_on.as_deref() {
        validate_commitment_option_length("initial_hours_on", initial_hours_on, expected_len)?;
    }
    validate_sparse_mask_length(
        "initial_hours_on",
        options.initial_hours_on_mask.as_deref(),
        expected_len,
    )?;
    if let Some(initial_offline_hours) = options.initial_offline_hours.as_deref() {
        validate_commitment_option_length(
            "initial_offline_hours",
            initial_offline_hours,
            expected_len,
        )?;
    }
    validate_sparse_mask_length(
        "initial_offline_hours",
        options.initial_offline_hours_mask.as_deref(),
        expected_len,
    )?;
    if let Some(initial_starts_24h) = options.initial_starts_24h.as_deref() {
        validate_commitment_option_length("initial_starts_24h", initial_starts_24h, expected_len)?;
    }
    validate_sparse_mask_length(
        "initial_starts_24h",
        options.initial_starts_24h_mask.as_deref(),
        expected_len,
    )?;
    if let Some(initial_starts_168h) = options.initial_starts_168h.as_deref() {
        validate_commitment_option_length(
            "initial_starts_168h",
            initial_starts_168h,
            expected_len,
        )?;
    }
    validate_sparse_mask_length(
        "initial_starts_168h",
        options.initial_starts_168h_mask.as_deref(),
        expected_len,
    )?;
    if let Some(initial_energy_mwh_24h) = options.initial_energy_mwh_24h.as_deref() {
        validate_commitment_option_length(
            "initial_energy_mwh_24h",
            initial_energy_mwh_24h,
            expected_len,
        )?;
    }
    validate_sparse_mask_length(
        "initial_energy_mwh_24h",
        options.initial_energy_mwh_24h_mask.as_deref(),
        expected_len,
    )?;
    Ok(())
}

fn validate_commitment_inputs(
    network: &Network,
    commitment: &CommitmentMode,
    n_periods: usize,
    in_service_gen_indices: &[usize],
) -> Result<(), ScedError> {
    let n_in_service_gens = in_service_gen_indices.len();
    match commitment {
        CommitmentMode::AllCommitted => Ok(()),
        CommitmentMode::Fixed {
            commitment,
            per_period,
        } => {
            validate_commitment_vector_length(
                "fixed commitment schedule",
                commitment,
                n_in_service_gens,
            )?;
            validate_storage_commitment_mask(
                "fixed commitment schedule",
                commitment,
                network,
                in_service_gen_indices,
            )?;
            if let Some(per_period) = per_period {
                if per_period.len() != n_periods {
                    return Err(ScedError::InvalidInput(format!(
                        "fixed per_period commitment length {} does not match n_periods {}",
                        per_period.len(),
                        n_periods
                    )));
                }
                for (period, schedule) in per_period.iter().enumerate() {
                    validate_commitment_vector_length(
                        &format!("fixed commitment schedule for period {}", period),
                        schedule,
                        n_in_service_gens,
                    )?;
                    validate_storage_commitment_mask(
                        &format!("fixed commitment schedule for period {}", period),
                        schedule,
                        network,
                        in_service_gen_indices,
                    )?;
                }
            }
            Ok(())
        }
        CommitmentMode::Optimize(options) => {
            validate_commitment_options(options, n_in_service_gens)
        }
        CommitmentMode::Additional {
            da_commitment,
            options,
        } => {
            if da_commitment.len() != n_periods {
                return Err(ScedError::InvalidInput(format!(
                    "additional commitment schedule length {} does not match n_periods {}",
                    da_commitment.len(),
                    n_periods
                )));
            }
            for (period, schedule) in da_commitment.iter().enumerate() {
                validate_commitment_vector_length(
                    &format!("additional commitment schedule for period {}", period),
                    schedule,
                    n_in_service_gens,
                )?;
            }
            validate_commitment_options(options, n_in_service_gens)
        }
    }
}

fn validate_commitment_constraints(
    problem_spec: &DispatchProblemSpec<'_>,
    n_in_service_gens: usize,
) -> Result<(), ScedError> {
    for constraint in problem_spec.commitment_constraints {
        if constraint.period_idx >= problem_spec.n_periods {
            return Err(ScedError::InvalidInput(format!(
                "commitment constraint {} references period {} outside n_periods {}",
                constraint.name, constraint.period_idx, problem_spec.n_periods
            )));
        }
        for term in &constraint.terms {
            if term.gen_index >= n_in_service_gens {
                return Err(ScedError::InvalidInput(format!(
                    "commitment constraint {} references generator {} outside in-service generator count {}",
                    constraint.name, term.gen_index, n_in_service_gens
                )));
            }
        }
    }
    Ok(())
}

fn validate_startup_window_limits(
    problem_spec: &DispatchProblemSpec<'_>,
    n_in_service_gens: usize,
) -> Result<(), ScedError> {
    for limit in problem_spec.startup_window_limits {
        if limit.gen_index >= n_in_service_gens {
            return Err(ScedError::InvalidInput(format!(
                "startup_window_limit references generator {} outside in-service generator count {}",
                limit.gen_index, n_in_service_gens
            )));
        }
        if limit.start_period_idx > limit.end_period_idx {
            return Err(ScedError::InvalidInput(format!(
                "startup_window_limit for generator {} has start_period_idx {} after end_period_idx {}",
                limit.gen_index, limit.start_period_idx, limit.end_period_idx
            )));
        }
        if limit.end_period_idx >= problem_spec.n_periods {
            return Err(ScedError::InvalidInput(format!(
                "startup_window_limit for generator {} references end period {} outside n_periods {}",
                limit.gen_index, limit.end_period_idx, problem_spec.n_periods
            )));
        }
    }
    Ok(())
}

fn validate_peak_demand_charges(
    problem_spec: &DispatchProblemSpec<'_>,
    n_in_service_gens: usize,
) -> Result<(), ScedError> {
    for charge in problem_spec.peak_demand_charges {
        if charge.gen_index >= n_in_service_gens {
            return Err(ScedError::InvalidInput(format!(
                "peak_demand_charge {:?} references generator {} outside in-service generator count {}",
                charge.name, charge.gen_index, n_in_service_gens
            )));
        }
        if charge.period_indices.is_empty() {
            return Err(ScedError::InvalidInput(format!(
                "peak_demand_charge {:?} has empty period_indices",
                charge.name
            )));
        }
        for &period in &charge.period_indices {
            if period >= problem_spec.n_periods {
                return Err(ScedError::InvalidInput(format!(
                    "peak_demand_charge {:?} references period {} outside n_periods {}",
                    charge.name, period, problem_spec.n_periods
                )));
            }
        }
        if !charge.charge_per_mw.is_finite() {
            return Err(ScedError::InvalidInput(format!(
                "peak_demand_charge {:?} has non-finite charge_per_mw {}",
                charge.name, charge.charge_per_mw
            )));
        }
    }
    Ok(())
}

fn validate_energy_window_limits(
    problem_spec: &DispatchProblemSpec<'_>,
    n_in_service_gens: usize,
) -> Result<(), ScedError> {
    for limit in problem_spec.energy_window_limits {
        if limit.gen_index >= n_in_service_gens {
            return Err(ScedError::InvalidInput(format!(
                "energy_window_limit references generator {} outside in-service generator count {}",
                limit.gen_index, n_in_service_gens
            )));
        }
        if limit.start_period_idx > limit.end_period_idx {
            return Err(ScedError::InvalidInput(format!(
                "energy_window_limit for generator {} has start_period_idx {} after end_period_idx {}",
                limit.gen_index, limit.start_period_idx, limit.end_period_idx
            )));
        }
        if limit.end_period_idx >= problem_spec.n_periods {
            return Err(ScedError::InvalidInput(format!(
                "energy_window_limit for generator {} references end period {} outside n_periods {}",
                limit.gen_index, limit.end_period_idx, problem_spec.n_periods
            )));
        }
        if limit.min_energy_mwh.is_none() && limit.max_energy_mwh.is_none() {
            return Err(ScedError::InvalidInput(format!(
                "energy_window_limit for generator {} must specify at least one bound",
                limit.gen_index
            )));
        }
        if let (Some(min_energy), Some(max_energy)) = (limit.min_energy_mwh, limit.max_energy_mwh)
            && min_energy > max_energy + 1e-9
        {
            return Err(ScedError::InvalidInput(format!(
                "energy_window_limit for generator {} has min_energy_mwh {} above max_energy_mwh {}",
                limit.gen_index, min_energy, max_energy
            )));
        }
    }
    Ok(())
}

fn validate_storage_inputs(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Result<(), ScedError> {
    for curve in problem_spec.ph_head_curves {
        let Some(generator) = network.generators.get(curve.gen_index) else {
            return Err(ScedError::InvalidInput(format!(
                "ph_head_curves references unknown generator index {}",
                curve.gen_index
            )));
        };
        if !generator.is_storage() {
            return Err(ScedError::InvalidInput(format!(
                "ph_head_curves generator index {} is not a storage unit",
                curve.gen_index
            )));
        }
        if curve.breakpoints.len() < 2 {
            return Err(ScedError::InvalidInput(format!(
                "ph_head_curves for generator index {} requires at least two breakpoints",
                curve.gen_index
            )));
        }
        let mut prev_soc = None::<f64>;
        let mut prev_slope = None::<f64>;
        let mut prev_pmax = None::<f64>;
        for (point_idx, &(soc_mwh, pmax_mw)) in curve.breakpoints.iter().enumerate() {
            if !(soc_mwh.is_finite() && pmax_mw.is_finite()) {
                return Err(ScedError::InvalidInput(format!(
                    "ph_head_curves[gen_index={}].breakpoints[{point_idx}] must be finite",
                    curve.gen_index
                )));
            }
            if pmax_mw < 0.0 {
                return Err(ScedError::InvalidInput(format!(
                    "ph_head_curves[gen_index={}].breakpoints[{point_idx}] has negative pmax_mw {}",
                    curve.gen_index, pmax_mw
                )));
            }
            if let Some(prev_soc_val) = prev_soc {
                if soc_mwh <= prev_soc_val {
                    return Err(ScedError::InvalidInput(format!(
                        "ph_head_curves for generator index {} must have strictly increasing soc_mwh breakpoints",
                        curve.gen_index
                    )));
                }
            }
            if let Some(prev_pmax_val) = prev_pmax
                && pmax_mw + 1e-9 < prev_pmax_val
            {
                return Err(ScedError::InvalidInput(format!(
                    "ph_head_curves for generator index {} must be nondecreasing in pmax_mw",
                    curve.gen_index
                )));
            }
            if point_idx > 0 {
                let (prev_soc_val, prev_pmax_val) = curve.breakpoints[point_idx - 1];
                let slope = (pmax_mw - prev_pmax_val) / (soc_mwh - prev_soc_val);
                if let Some(prev_slope_val) = prev_slope
                    && slope > prev_slope_val + 1e-9
                {
                    return Err(ScedError::InvalidInput(format!(
                        "ph_head_curves for generator index {} must be concave",
                        curve.gen_index
                    )));
                }
                prev_slope = Some(slope);
            }
            prev_soc = Some(soc_mwh);
            prev_pmax = Some(pmax_mw);
        }
    }
    for constraint in problem_spec.ph_mode_constraints {
        let Some(generator) = network.generators.get(constraint.gen_index) else {
            return Err(ScedError::InvalidInput(format!(
                "ph_mode_constraints references unknown generator index {}",
                constraint.gen_index
            )));
        };
        if !generator.is_storage() {
            return Err(ScedError::InvalidInput(format!(
                "ph_mode_constraints generator index {} is not a storage unit",
                constraint.gen_index
            )));
        }
    }

    if let Some(storage_self_schedules) = problem_spec.storage_self_schedules {
        for (&gen_index, periods) in storage_self_schedules {
            let Some(generator) = network.generators.get(gen_index) else {
                return Err(ScedError::InvalidInput(format!(
                    "storage_self_schedules references unknown generator index {}",
                    gen_index
                )));
            };
            if !generator.is_storage() {
                return Err(ScedError::InvalidInput(format!(
                    "storage_self_schedules generator index {} is not a storage unit",
                    gen_index
                )));
            }
            validate_exact_length(
                &format!("storage_self_schedules[{gen_index}]"),
                periods.len(),
                problem_spec.n_periods,
            )?;
            for (period, &value_mw) in periods.iter().enumerate() {
                if !value_mw.is_finite() {
                    return Err(ScedError::InvalidInput(format!(
                        "storage_self_schedules[{gen_index}][{period}] must be finite"
                    )));
                }
            }
        }
    }

    let reserve_product_ids: std::collections::HashSet<&str> = problem_spec
        .reserve_products
        .iter()
        .map(|product| product.id.as_str())
        .collect();
    for (&gen_index, products) in problem_spec.storage_reserve_soc_impact {
        let Some(generator) = network.generators.get(gen_index) else {
            return Err(ScedError::InvalidInput(format!(
                "storage_reserve_soc_impact references unknown generator index {}",
                gen_index
            )));
        };
        if !generator.is_storage() {
            return Err(ScedError::InvalidInput(format!(
                "storage_reserve_soc_impact generator index {} is not a storage unit",
                gen_index
            )));
        }
        for (product_id, periods) in products {
            if !reserve_product_ids.contains(product_id.as_str()) {
                return Err(ScedError::InvalidInput(format!(
                    "storage_reserve_soc_impact for generator {} references unknown reserve product_id {}",
                    gen_index, product_id
                )));
            }
            validate_exact_length(
                &format!("storage_reserve_soc_impact[{gen_index}][{product_id}]"),
                periods.len(),
                problem_spec.n_periods,
            )?;
            for (period, &impact) in periods.iter().enumerate() {
                if !impact.is_finite() {
                    return Err(ScedError::InvalidInput(format!(
                        "storage_reserve_soc_impact[{gen_index}][{product_id}][{period}] must be finite"
                    )));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DC Sequential path
// ---------------------------------------------------------------------------

fn dispatch_dc_sequential(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
) -> Result<RawDispatchSolution, ScedError> {
    let n_periods = problem_spec.n_periods;

    if n_periods == 1 {
        // Single-period: direct call, no profile application needed for period 0
        // when profiles are empty
        let net = apply_profiles(network, &problem_spec, 0);
        let sol = crate::sced::solve::solve_sced_with_problem_spec(
            &net,
            problem_spec,
            DispatchPeriodContext::initial(problem_spec.initial_state),
        )?;
        return Ok(sol);
    }

    // Multi-period sequential
    let mut accumulator = SequentialDispatchAccumulator::new(network, problem_spec);

    for t in 0..n_periods {
        let net_t = apply_profiles(network, &problem_spec, t);
        let period_spec = problem_spec.period(t);
        let sol = crate::sced::solve::solve_sced_with_problem_spec(
            &net_t,
            problem_spec,
            accumulator.period_context(t, period_spec.next_fixed_commitment()),
        )?;
        let bus_angles_rad = sol.bus_angles_rad.first().cloned().unwrap_or_default();
        let dispatch = sol
            .periods
            .into_iter()
            .next()
            .expect("single-period SCED should return exactly one dispatch period");
        let hvdc_dispatch_mw =
            (!dispatch.hvdc_dispatch_mw.is_empty()).then(|| dispatch.hvdc_dispatch_mw.clone());
        accumulator.record_period(
            &net_t,
            dispatch,
            hvdc_dispatch_mw,
            sol.diagnostics.iterations,
            bus_angles_rad,
            Vec::new(),
            Vec::new(),
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
    }

    Ok(accumulator.finish())
}

// ---------------------------------------------------------------------------
// DC TimeCoupled path (dispatch-only, no commitment)
// ---------------------------------------------------------------------------

fn dispatch_dc_time_coupled(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
) -> Result<RawDispatchSolution, ScedError> {
    // For the time-coupled dispatch-only case, we reuse the SCUC builder.
    // CommitmentMode::Fixed is passed through directly so solve_scuc can pin
    // all binary variables and solve as a pure LP (no MILP).
    // AllCommitted is converted to Fixed with all-true commitment.
    let fixed_commitment_override = match problem_spec.commitment {
        CommitmentMode::Fixed { .. } => None,
        CommitmentMode::AllCommitted
        | CommitmentMode::Additional { .. }
        | CommitmentMode::Optimize(_) => Some(CommitmentMode::Fixed {
            commitment: vec![true; network.generators.iter().filter(|g| g.in_service).count()],
            per_period: None,
        }),
    };
    let scuc_spec = fixed_commitment_override
        .as_ref()
        .map_or(problem_spec, |commitment| {
            problem_spec.with_commitment(commitment)
        });

    crate::scuc::solve_scuc_with_problem_spec(network, scuc_spec)
}

// ---------------------------------------------------------------------------
// AC Sequential path
// ---------------------------------------------------------------------------

/// Returns true when the user has opted into the SCED post-contingency
/// LODF cut loop via `SURGE_SCED_SECURITY_CUTS=1` (or `true`/`on`/`yes`).
///
/// Gating the feature on an env var keeps the default behaviour
/// unchanged and lets callers benchmark the security loop one scenario
/// at a time without baking a flag into the public request API. Once
/// the loop is validated more broadly this will be promoted to a typed
/// field on `DispatchProblemSpec` and the env path retired.
fn sced_security_cuts_enabled_from_env() -> bool {
    matches!(
        std::env::var("SURGE_SCED_SECURITY_CUTS")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "on" | "yes")
    )
}

// ---------------------------------------------------------------------------
// AC-SCED per-period helpers (sequential + parallel branches share these).
// ---------------------------------------------------------------------------

/// Owned per-period context for the parallel AC-SCED branch.
///
/// `DispatchPeriodContext<'a>` carries borrowed slices that, in the
/// sequential branch, point into [`SequentialDispatchAccumulator::state`]
/// — a structure mutated by `record_period`. The parallel branch
/// pre-stages each period's inputs into this owned form, then borrows
/// fresh references inside the worker closure.
struct OwnedAcScedPeriodContext {
    period: usize,
    prev_dispatch_mw: Option<Vec<f64>>,
    next_period_commitment: Option<Vec<bool>>,
}

impl OwnedAcScedPeriodContext {
    fn borrow(&self) -> DispatchPeriodContext<'_> {
        DispatchPeriodContext {
            period: self.period,
            prev_dispatch_mw: self.prev_dispatch_mw.as_deref(),
            prev_dispatch_mask: None,
            prev_hvdc_dispatch_mw: None,
            prev_hvdc_dispatch_mask: None,
            // Parallel mode is gated off when storage is present, so the
            // SoC override is always None here.
            storage_soc_override: None,
            next_period_commitment: self.next_period_commitment.as_deref(),
        }
    }
}

/// Per-period anchor dispatch trajectory used by the parallel branch as
/// the source of `prev_dispatch_mw` for ramp constraints.
///
/// For each period and each in-service generator, returns the midpoint
/// of `generator_dispatch_bounds[period]`. In a two-stage reconcile
/// pipeline these bounds are tightly pinned around the DC SCUC dispatch,
/// so the midpoint reproduces the DC anchor that the sequential ramp
/// constraint would have read from the prior period's solved AC dispatch.
/// Wider bounds make this a heuristic — falls back to the device
/// initial dispatch when no per-period bound is available.
fn parallel_period_anchor_dispatch(
    network: &Network,
    problem_spec: &DispatchProblemSpec<'_>,
) -> Vec<Vec<f64>> {
    let n_periods = problem_spec.n_periods;
    let in_service_ids: Vec<&str> = network
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| g.id.as_str())
        .collect();
    let bounds_by_id: HashMap<&str, &crate::request::GeneratorDispatchBoundsProfile> = problem_spec
        .generator_dispatch_bounds
        .profiles
        .iter()
        .map(|p| (p.resource_id.as_str(), p))
        .collect();
    let initial_dispatch = problem_spec.initial_state.prev_dispatch_mw.as_deref();

    (0..n_periods)
        .map(|t| {
            in_service_ids
                .iter()
                .enumerate()
                .map(|(j, id)| {
                    if let Some(profile) = bounds_by_id.get(*id)
                        && let (Some(p_min), Some(p_max)) = (
                            profile.p_min_mw.get(t).copied(),
                            profile.p_max_mw.get(t).copied(),
                        )
                    {
                        return 0.5 * (p_min + p_max);
                    }
                    initial_dispatch
                        .and_then(|d| d.get(j).copied())
                        .unwrap_or(0.0)
                })
                .collect()
        })
        .collect()
}

/// Pre-stage owned per-period contexts for the parallel branch.
///
/// `prev_dispatch_mw` for period 0 comes from
/// `problem_spec.initial_state.prev_dispatch_mw` (the device initial
/// conditions); for period t > 0 it comes from `anchor_dispatch[t-1]`.
/// `next_period_commitment` is materialised by cloning whatever
/// `period.next_fixed_commitment()` returns for the period.
fn build_owned_period_contexts_for_parallel(
    problem_spec: &DispatchProblemSpec<'_>,
    anchor_dispatch: &[Vec<f64>],
) -> Vec<OwnedAcScedPeriodContext> {
    let n_periods = problem_spec.n_periods;
    let initial_prev = problem_spec.initial_state.prev_dispatch_mw.clone();
    (0..n_periods)
        .map(|t| {
            let prev_dispatch_mw = if t == 0 {
                initial_prev.clone()
            } else {
                anchor_dispatch.get(t - 1).cloned()
            };
            let next_period_commitment = problem_spec
                .period(t)
                .next_fixed_commitment()
                .map(|s| s.to_vec());
            OwnedAcScedPeriodContext {
                period: t,
                prev_dispatch_mw,
                next_period_commitment,
            }
        })
        .collect()
}

/// Solve a single AC-SCED period.
///
/// Routes to either the security-cuts wrapper (when the env var opt-in
/// is set) or the standard problem-spec solver. Wraps both error paths
/// in a `ScedError::SolverError` carrying the period index so callers
/// can attribute failures.
fn solve_ac_sced_one_period(
    t: usize,
    net_t: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &AcOpfOptions,
    ac_opf_runtime: &surge_opf::AcOpfRuntime,
    period_context: DispatchPeriodContext<'_>,
    prev_solution: Option<&OpfSolution>,
    security_cuts_enabled: bool,
) -> Result<AcScedPeriodArtifacts, ScedError> {
    if security_cuts_enabled {
        let cfg = crate::common::security::SecurityConfig::enabled_with_defaults();
        crate::sced::security::solve_ac_sced_with_security_cuts_artifacts(
            net_t,
            problem_spec,
            ac_opf,
            ac_opf_runtime,
            period_context,
            prev_solution,
            &cfg,
        )
    } else {
        crate::sced::ac::solve_ac_sced_with_problem_spec_artifacts(
            net_t,
            problem_spec,
            ac_opf,
            ac_opf_runtime,
            period_context,
            prev_solution,
        )
    }
    .map_err(|error| ScedError::SolverError(format!("AC-SCED period {t}: {error}")))
}

/// Fold a solved AC-SCED period into the sequential accumulator.
///
/// Decomposes the LMP into (energy, congestion, loss) components and
/// builds the AC reactive reserve award maps, then calls
/// [`SequentialDispatchAccumulator::record_period`] with every per-bus
/// and per-generator vector the downstream consumers expect.
fn record_ac_sced_period_into_accumulator(
    accumulator: &mut SequentialDispatchAccumulator,
    net_t: &Network,
    artifacts: AcScedPeriodArtifacts,
) {
    accumulator.ac_sced_period_timings.push(artifacts.timings);
    if let Some(stats) = artifacts.opf_stats {
        accumulator.ac_opf_stats.push(stats);
    }
    let sol = artifacts.period_solution;
    let (lmp_energy, lmp_congestion, lmp_loss) =
        decompose_ac_sced_lmp(net_t, &sol.lmp, &sol.bus_voltage_pu, &sol.bus_angle_rad);
    let (reserve_awards, dr_reserve_awards) = build_ac_reactive_reserve_awards(&sol);
    accumulator.record_period(
        net_t,
        RawDispatchPeriodResult {
            pg_mw: sol.pg_mw,
            lmp: sol.lmp,
            q_lmp: sol.q_lmp,
            lmp_energy,
            lmp_congestion,
            lmp_loss,
            total_cost: sol.total_cost,
            branch_shadow_prices: sol.branch_shadow_prices,
            flowgate_shadow_prices: sol.flowgate_shadow_prices,
            interface_shadow_prices: sol.interface_shadow_prices,
            dr_results: sol.dr_results,
            hvdc_dispatch_mw: sol.hvdc_dispatch_mw.clone(),
            hvdc_band_dispatch_mw: sol.hvdc_band_dispatch_mw,
            storage_charge_mw: sol
                .storage_net_mw
                .iter()
                .map(|&mw| (-mw).max(0.0))
                .collect(),
            storage_discharge_mw: sol.storage_net_mw.iter().map(|&mw| mw.max(0.0)).collect(),
            storage_soc_mwh: sol.storage_soc_mwh,
            reserve_awards,
            dr_reserve_awards,
            tap_dispatch: sol.tap_dispatch,
            phase_dispatch: sol.phase_dispatch,
            switched_shunt_dispatch: sol.switched_shunt_dispatch,
            objective_terms: sol.objective_terms,
            ..RawDispatchPeriodResult::default()
        },
        Some(sol.hvdc_dispatch_mw),
        sol.iterations,
        sol.bus_angle_rad,
        sol.bus_voltage_pu,
        sol.qg_mvar,
        sol.bus_q_slack_pos_mvar,
        sol.bus_q_slack_neg_mvar,
        sol.bus_p_slack_pos_mw,
        sol.bus_p_slack_neg_mw,
        sol.thermal_limit_slack_from_mva,
        sol.thermal_limit_slack_to_mva,
        sol.vm_slack_high_pu,
        sol.vm_slack_low_pu,
        sol.angle_diff_slack_high_rad,
        sol.angle_diff_slack_low_rad,
    );
}

fn ac_sced_thread_pool(concurrency: usize) -> Result<Arc<rayon::ThreadPool>, ScedError> {
    let pools = AC_SCED_THREAD_POOLS.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = pools.lock().map_err(|_| {
            ScedError::SolverError("AC-SCED parallel: thread-pool cache poisoned".to_string())
        })?;
        if let Some(pool) = guard.get(&concurrency) {
            return Ok(Arc::clone(pool));
        }
    }

    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(concurrency)
            .thread_name(|i| format!("ac-sced-{i}"))
            .build()
            .map_err(|err| {
                ScedError::SolverError(format!(
                    "AC-SCED parallel: failed to build rayon pool ({concurrency} threads): {err}"
                ))
            })?,
    );
    let mut guard = pools.lock().map_err(|_| {
        ScedError::SolverError("AC-SCED parallel: thread-pool cache poisoned".to_string())
    })?;
    Ok(Arc::clone(guard.entry(concurrency).or_insert(pool)))
}

fn run_ac_sced_periods_sequential(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &AcOpfOptions,
    ac_opf_runtime: &surge_opf::AcOpfRuntime,
    security_cuts_enabled: bool,
    accumulator: &mut SequentialDispatchAccumulator,
) -> Result<(), ScedError> {
    let mut prev_solution: Option<OpfSolution> = None;
    for t in 0..problem_spec.n_periods {
        let net_t = apply_profiles(network, &problem_spec, t);
        let period_spec = problem_spec.period(t);
        let period_context = accumulator.period_context(t, period_spec.next_fixed_commitment());
        let artifacts = solve_ac_sced_one_period(
            t,
            &net_t,
            problem_spec,
            ac_opf,
            ac_opf_runtime,
            period_context,
            prev_solution.as_ref(),
            security_cuts_enabled,
        )?;
        let opf_solution = artifacts.opf_solution.clone();
        record_ac_sced_period_into_accumulator(accumulator, &net_t, artifacts);
        prev_solution = Some(opf_solution);
    }
    Ok(())
}

fn dispatch_ac_sequential(
    network: &Network,
    problem_spec: DispatchProblemSpec<'_>,
    ac_opf: &AcOpfOptions,
    ac_opf_runtime: &surge_opf::AcOpfRuntime,
) -> Result<RawDispatchSolution, ScedError> {
    let n_periods = problem_spec.n_periods;

    // SCED-AC Benders opt-in: when the runtime contains an
    // `orchestration` block, run the Benders decomposition loop instead
    // of the target-tracking AC reconciliation path. The orchestrator
    // reuses the DC SCED LP for the master and calls
    // `surge_opf::ac::solve_ac_opf_subproblem` for the fixed-Pg
    // subproblem, accumulating cuts between iterations.
    if problem_spec.sced_ac_benders.orchestration.is_some() {
        let mut diagnostics = crate::sced_ac_benders::BendersDiagnostics::default();
        let mut result = crate::sced_ac_benders::solve_sced_sequence_benders(
            network,
            problem_spec,
            ac_opf,
            ac_opf_runtime,
            &mut diagnostics,
        )?;
        // Attach the Benders diagnostics to the dispatch result so callers
        // can inspect the convergence trajectory.
        result.diagnostics.sced_ac_benders = Some(diagnostics);
        result.ac_p_balance_penalty_per_mw =
            Some(ac_opf.bus_active_power_balance_slack_penalty_per_mw).filter(|&v| v > 0.0);
        result.ac_q_balance_penalty_per_mvar =
            Some(ac_opf.bus_reactive_power_balance_slack_penalty_per_mvar).filter(|&v| v > 0.0);
        result.ac_thermal_penalty_per_mva =
            Some(ac_opf.thermal_limit_slack_penalty_per_mva).filter(|&v| v > 0.0);
        result.ac_voltage_penalty_per_pu =
            Some(ac_opf.voltage_magnitude_slack_penalty_per_pu).filter(|&v| v > 0.0);
        result.ac_angle_penalty_per_rad =
            Some(ac_opf.angle_difference_slack_penalty_per_rad).filter(|&v| v > 0.0);
        return Ok(result);
    }

    if n_periods == 1 {
        let net = apply_profiles(network, &problem_spec, 0);
        let context = DispatchPeriodContext::initial(problem_spec.initial_state);
        // Opt-in LODF post-contingency cut loop for the AC SCED. Off by
        // default and gated behind the `SURGE_SCED_SECURITY_CUTS` env var
        // (a no-op for every existing caller). When enabled, the loop
        // generates `Flowgate` constraints for every post-contingency
        // overload the AC solution introduces and re-solves until the
        // post-contingency state is clean. See `sced/security.rs` for
        // the full algorithm and `common/security.rs::SecurityConfig`
        // for the tuning knobs. Will be promoted to a typed config
        // field on `DispatchProblemSpec` once more widely validated.
        let sol = if sced_security_cuts_enabled_from_env() {
            let cfg = crate::common::security::SecurityConfig::enabled_with_defaults();
            crate::sced::security::solve_ac_sced_with_security_cuts(
                &net,
                problem_spec,
                ac_opf,
                ac_opf_runtime,
                context,
                &cfg,
            )
        } else {
            crate::sced::ac::solve_ac_sced_with_problem_spec(
                &net,
                problem_spec,
                ac_opf,
                ac_opf_runtime,
                context,
            )
        }
        .map_err(|error| ScedError::SolverError(format!("AC-SCED period 0: {error}")))?;
        let mut result = ac_sced_period_to_dispatch_result(&net, sol);
        result.ac_p_balance_penalty_per_mw =
            Some(ac_opf.bus_active_power_balance_slack_penalty_per_mw).filter(|&v| v > 0.0);
        result.ac_q_balance_penalty_per_mvar =
            Some(ac_opf.bus_reactive_power_balance_slack_penalty_per_mvar).filter(|&v| v > 0.0);
        result.ac_thermal_penalty_per_mva =
            Some(ac_opf.thermal_limit_slack_penalty_per_mva).filter(|&v| v > 0.0);
        result.ac_voltage_penalty_per_pu =
            Some(ac_opf.voltage_magnitude_slack_penalty_per_pu).filter(|&v| v > 0.0);
        result.ac_angle_penalty_per_rad =
            Some(ac_opf.angle_difference_slack_penalty_per_rad).filter(|&v| v > 0.0);
        return Ok(result);
    }

    // Multi-period: apply per-bus profiles each period, same as DC path.
    let mut accumulator = SequentialDispatchAccumulator::new(network, problem_spec);
    let security_cuts_enabled = sced_security_cuts_enabled_from_env();

    // Storage SoC continuity is fed period-to-period via
    // `accumulator.state.storage_soc_override`. The parallel path bypasses
    // that threading, so refuse to parallelize when any in-service storage
    // is present and fall back to sequential.
    let has_storage = network
        .generators
        .iter()
        .any(|g| g.in_service && g.is_storage());
    let raw_concurrency = problem_spec.ac_sced_period_concurrency.unwrap_or(0);
    let concurrency = if has_storage && raw_concurrency >= 2 {
        tracing::warn!(
            requested = raw_concurrency,
            "AC-SCED parallel mode disabled: in-service storage requires sequential SoC threading"
        );
        1
    } else {
        raw_concurrency.max(1)
    };

    if concurrency <= 1 {
        run_ac_sced_periods_sequential(
            network,
            problem_spec,
            ac_opf,
            ac_opf_runtime,
            security_cuts_enabled,
            &mut accumulator,
        )?;
    } else {
        // Parallel per-period AC SCED. Pre-stage all per-period inputs so
        // each worker has self-contained data, then collect every period's
        // outcome (success or failure) before deciding whether to error.
        let anchor_dispatch = parallel_period_anchor_dispatch(network, &problem_spec);
        let owned_contexts =
            build_owned_period_contexts_for_parallel(&problem_spec, &anchor_dispatch);

        let pool = ac_sced_thread_pool(concurrency)?;

        info!(
            n_periods = n_periods,
            concurrency = concurrency,
            "AC-SCED running periods in parallel"
        );

        type ParallelOutcome = (usize, Result<(Network, AcScedPeriodArtifacts), ScedError>);
        let parallel_t0 = std::time::Instant::now();
        let outcomes: Vec<ParallelOutcome> = pool.install(|| {
            use rayon::prelude::*;
            (0..n_periods)
                .into_par_iter()
                .map(|t| {
                    let span = tracing::info_span!("ac_sced_period", period = t);
                    let _enter = span.enter();
                    let period_t0 = std::time::Instant::now();
                    let started_offset = parallel_t0.elapsed().as_secs_f64();
                    let net_t = apply_profiles(network, &problem_spec, t);
                    let context = owned_contexts[t].borrow();
                    let result = solve_ac_sced_one_period(
                        t,
                        &net_t,
                        problem_spec,
                        ac_opf,
                        ac_opf_runtime,
                        context,
                        // No AC→AC warm-start in parallel mode; each period
                        // falls back to the per-period AC PF warm-start
                        // built inside `sequential_ac_runtime_candidates`.
                        None,
                        security_cuts_enabled,
                    )
                    .map(|artifacts| (net_t, artifacts));
                    let elapsed = period_t0.elapsed().as_secs_f64();
                    let finished_offset = parallel_t0.elapsed().as_secs_f64();
                    info!(
                        period = t,
                        thread = ?std::thread::current().id(),
                        start_offset_s = started_offset,
                        elapsed_s = elapsed,
                        finish_offset_s = finished_offset,
                        ok = result.is_ok(),
                        "AC-SCED period (parallel)"
                    );
                    (t, result)
                })
                .collect()
        });
        info!(
            n_periods = n_periods,
            wall_s = parallel_t0.elapsed().as_secs_f64(),
            "AC-SCED parallel batch finished"
        );

        // Custom collector: run all periods, gather every error, surface
        // them together rather than short-circuiting on the first failure.
        let mut artifacts_by_period: Vec<Option<(Network, AcScedPeriodArtifacts)>> =
            (0..n_periods).map(|_| None).collect();
        let mut failures: Vec<(usize, ScedError)> = Vec::new();
        for (t, outcome) in outcomes {
            match outcome {
                Ok(pair) => artifacts_by_period[t] = Some(pair),
                Err(err) => failures.push((t, err)),
            }
        }
        if !failures.is_empty() {
            failures.sort_by_key(|(t, _)| *t);
            for (t, err) in &failures {
                tracing::error!(
                    period = *t,
                    error = %err,
                    "AC-SCED parallel period failure"
                );
            }
            let detail = failures
                .iter()
                .map(|(t, e)| format!("period {t}: {e}"))
                .collect::<Vec<_>>()
                .join("; ");
            tracing::warn!(
                failed_periods = failures.len(),
                n_periods,
                detail = %detail,
                "AC-SCED parallel failed; retrying failed periods sequentially with neighbor warm-starts"
            );
            let mut partial_failures: Vec<(usize, ScedError)> = Vec::new();
            for (t, _) in &failures {
                let net_t = apply_profiles(network, &problem_spec, *t);
                let context = owned_contexts[*t].borrow();
                let prev_solution_owned = if *t > 0 {
                    artifacts_by_period[*t - 1]
                        .as_ref()
                        .map(|(_, artifacts)| artifacts.opf_solution.clone())
                } else {
                    None
                };
                match solve_ac_sced_one_period(
                    *t,
                    &net_t,
                    problem_spec,
                    ac_opf,
                    ac_opf_runtime,
                    context,
                    prev_solution_owned.as_ref(),
                    security_cuts_enabled,
                ) {
                    Ok(artifacts) => {
                        tracing::info!(period = *t, "AC-SCED partial hybrid retry succeeded");
                        artifacts_by_period[*t] = Some((net_t, artifacts));
                    }
                    Err(err) => {
                        tracing::error!(
                            period = *t,
                            error = %err,
                            "AC-SCED partial hybrid retry failed"
                        );
                        partial_failures.push((*t, err));
                    }
                }
            }

            if partial_failures.is_empty() {
                tracing::info!(
                    retried_periods = failures.len(),
                    "AC-SCED partial hybrid retry recovered all failed periods"
                );
                for slot in artifacts_by_period.iter_mut().take(n_periods) {
                    let (net_t, artifacts) = slot.take().expect("missing artifacts for period");
                    record_ac_sced_period_into_accumulator(&mut accumulator, &net_t, artifacts);
                }
            } else {
                let partial_detail = partial_failures
                    .iter()
                    .map(|(t, e)| format!("period {t}: {e}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                tracing::warn!(
                    failed_periods = partial_failures.len(),
                    detail = %partial_detail,
                    "AC-SCED partial hybrid retry failed; retrying full sequence sequentially with AC warm-starts"
                );
                let mut retry_accumulator =
                    SequentialDispatchAccumulator::new(network, problem_spec);
                run_ac_sced_periods_sequential(
                    network,
                    problem_spec,
                    ac_opf,
                    ac_opf_runtime,
                    security_cuts_enabled,
                    &mut retry_accumulator,
                )
                .map_err(|retry_error| {
                    ScedError::SolverError(format!(
                        "AC-SCED hybrid retry failed after parallel failure: \
                         parallel {} of {n_periods} period(s) failed: {detail}; \
                         partial retry failed: {partial_detail}; \
                         sequential retry error: {retry_error}",
                        failures.len()
                    ))
                })?;
                accumulator = retry_accumulator;
            }
        } else {
            // Fold per-period artifacts into the accumulator in chronological
            // order. Single-threaded — the accumulator's state mutation is
            // for downstream sequential consumers (DC routing, ramp ledger),
            // not for AC-SCED feedback.
            for slot in artifacts_by_period.iter_mut().take(n_periods) {
                let (net_t, artifacts) = slot.take().expect("missing artifacts for period");
                record_ac_sced_period_into_accumulator(&mut accumulator, &net_t, artifacts);
            }
        }
    }

    let mut result = accumulator.finish();
    result.ac_p_balance_penalty_per_mw =
        Some(ac_opf.bus_active_power_balance_slack_penalty_per_mw).filter(|&v| v > 0.0);
    result.ac_q_balance_penalty_per_mvar =
        Some(ac_opf.bus_reactive_power_balance_slack_penalty_per_mvar).filter(|&v| v > 0.0);
    result.ac_thermal_penalty_per_mva =
        Some(ac_opf.thermal_limit_slack_penalty_per_mva).filter(|&v| v > 0.0);
    result.ac_voltage_penalty_per_pu =
        Some(ac_opf.voltage_magnitude_slack_penalty_per_pu).filter(|&v| v > 0.0);
    result.ac_angle_penalty_per_rad =
        Some(ac_opf.angle_difference_slack_penalty_per_rad).filter(|&v| v > 0.0);
    Ok(result)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Apply load, derate, and renewable profiles to a network snapshot for a given period.
pub(crate) fn apply_profiles(
    base: &Network,
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) -> Network {
    let mut net = base.clone();
    crate::common::profiles::apply_ac_time_series_profiles(&mut net, spec, period);
    apply_period_generator_economics(&mut net, spec, period);
    net
}

fn apply_period_generator_economics(
    net: &mut Network,
    spec: &DispatchProblemSpec<'_>,
    period: usize,
) {
    if spec.offer_schedules.is_empty() {
        return;
    }

    for (global_gen_idx, generator) in net.generators.iter_mut().enumerate() {
        if !generator.in_service || generator.is_storage() {
            continue;
        }

        let Some(economics) = resolve_generator_economics_for_period(
            global_gen_idx,
            period,
            generator,
            spec.offer_schedules,
            Some(generator.pmax),
        ) else {
            continue;
        };

        generator.cost = Some(economics.cost.into_owned());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    use crate::config::emissions::{CarbonPrice, EmissionProfile, MustRunUnits};
    use crate::legacy::DispatchOptions;
    use crate::request::{
        CommitmentSchedule, DispatchInput, DispatchRequest, IntervalCoupling,
        NormalizedDispatchRequest,
    };
    use crate::solution::RawDispatchPeriodResult;

    fn solve_dispatch(
        network: &surge_network::Network,
        request: &DispatchRequest,
    ) -> Result<RawDispatchSolution, crate::error::ScedError> {
        super::solve_dispatch_raw(network, request)
    }

    fn solve_keyed_dispatch(
        network: &surge_network::Network,
        request: &DispatchRequest,
    ) -> Result<crate::result::DispatchSolution, crate::error::ScedError> {
        let model = DispatchModel::prepare(network)?;
        super::solve_dispatch(&model, request)
    }

    fn data_available() -> bool {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::Path::new(&p).exists();
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .exists()
    }

    fn test_data_path(name: &str) -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p).join(name);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .join(name)
    }

    fn market30_case_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/cases/market30/market30.surge.json.zst")
    }

    fn load_market30_network() -> surge_network::Network {
        surge_io::load(market30_case_path()).expect("load market30 network")
    }

    fn market30_generator_index(network: &surge_network::Network, machine_id: &str) -> usize {
        network
            .generators
            .iter()
            .position(|generator| generator.machine_id.as_deref() == Some(machine_id))
            .unwrap_or_else(|| panic!("generator {machine_id} should exist in market30"))
    }

    fn market30_bus_areas(network: &surge_network::Network) -> HashMap<u32, usize> {
        network
            .buses
            .iter()
            .map(|bus| (bus.number, bus.area as usize))
            .collect()
    }

    fn market30_base_loads_by_bus(network: &surge_network::Network) -> BTreeMap<u32, f64> {
        let mut by_bus = BTreeMap::new();
        for load in network.loads.iter().filter(|load| load.in_service) {
            *by_bus.entry(load.bus).or_insert(0.0) += load.active_power_demand_mw;
        }
        by_bus
    }

    fn market30_uniform_load_profiles(
        network: &surge_network::Network,
        scales: &[f64],
    ) -> surge_network::market::LoadProfiles {
        let profiles = market30_base_loads_by_bus(network)
            .into_iter()
            .map(|(bus, base_mw)| surge_network::market::LoadProfile {
                bus,
                load_mw: scales.iter().map(|scale| base_mw * scale).collect(),
            })
            .collect();
        surge_network::market::LoadProfiles {
            profiles,
            n_timesteps: scales.len(),
        }
    }

    fn market30_area_boosted_load_profiles(
        network: &surge_network::Network,
        system_scales: &[f64],
        boosted_area: usize,
        area_scales: &[f64],
    ) -> surge_network::market::LoadProfiles {
        assert_eq!(
            system_scales.len(),
            area_scales.len(),
            "system and area scales must have the same length"
        );
        let bus_areas = market30_bus_areas(network);
        let profiles = market30_base_loads_by_bus(network)
            .into_iter()
            .map(|(bus, base_mw)| {
                let area = bus_areas.get(&bus).copied().unwrap_or_default();
                let load_mw = system_scales
                    .iter()
                    .enumerate()
                    .map(|(period, system_scale)| {
                        let area_scale = if area == boosted_area {
                            area_scales[period]
                        } else {
                            1.0
                        };
                        base_mw * system_scale * area_scale
                    })
                    .collect();
                surge_network::market::LoadProfile { bus, load_mw }
            })
            .collect();
        surge_network::market::LoadProfiles {
            profiles,
            n_timesteps: system_scales.len(),
        }
    }

    fn market30_renewable_profiles(
        network: &surge_network::Network,
        wind_cf: &[f64],
        solar_cf: &[f64],
    ) -> surge_network::market::RenewableProfiles {
        assert_eq!(
            wind_cf.len(),
            solar_cf.len(),
            "wind and solar profiles must have the same length"
        );
        surge_network::market::RenewableProfiles {
            n_timesteps: wind_cf.len(),
            profiles: vec![
                surge_network::market::RenewableProfile {
                    generator_id: network.generators[market30_generator_index(network, "W1")]
                        .id
                        .clone(),
                    capacity_factors: wind_cf.to_vec(),
                },
                surge_network::market::RenewableProfile {
                    generator_id: network.generators[market30_generator_index(network, "S1")]
                        .id
                        .clone(),
                    capacity_factors: solar_cf.to_vec(),
                },
            ],
        }
    }

    fn market30_request_hvdc_link(
        network: &surge_network::Network,
    ) -> crate::hvdc::HvdcDispatchLink {
        let vsc = network
            .hvdc
            .links
            .iter()
            .find_map(|link| link.as_vsc())
            .expect("market30 should contain a VSC HVDC link");
        let observed_transfer = vsc
            .converter1
            .dc_setpoint
            .abs()
            .max(vsc.converter2.dc_setpoint.abs())
            .max(1.0);
        crate::hvdc::HvdcDispatchLink {
            id: String::new(),
            name: vsc.name.clone(),
            from_bus: vsc.converter1.bus,
            to_bus: vsc.converter2.bus,
            p_dc_min_mw: 0.0,
            p_dc_max_mw: observed_transfer * 2.0,
            loss_a_mw: vsc.converter1.loss_constant_mw + vsc.converter2.loss_constant_mw,
            loss_b_frac: vsc.converter1.loss_linear + vsc.converter2.loss_linear,
            ramp_mw_per_min: 0.0,
            cost_per_mwh: 0.0,
            bands: vec![],
        }
    }

    fn resource_result<'a>(
        period: &'a crate::solution::RawDispatchPeriodResult,
        resource_id: &str,
    ) -> &'a crate::solution::RawResourcePeriodResult {
        period
            .resource_results
            .iter()
            .find(|resource| resource.resource_id == resource_id)
            .unwrap_or_else(|| panic!("resource result {resource_id} should be present"))
    }

    fn test_local_resource_id(local_idx: usize) -> String {
        format!("__gen_local:{local_idx}")
    }

    fn test_global_resource_id(global_idx: usize) -> String {
        format!("__gen_global:{global_idx}")
    }

    fn test_dispatchable_load_id(dispatchable_load_idx: usize) -> String {
        format!("__dl:{dispatchable_load_idx}")
    }

    fn test_hvdc_link_id(link_idx: usize) -> String {
        format!("__hvdc:{link_idx}")
    }

    fn test_bus_number(bus_idx: usize) -> u32 {
        4_000_000_000u32 + bus_idx as u32
    }

    fn request_initial_state_from_legacy(
        initial_state: IndexedDispatchInitialState,
    ) -> crate::request::DispatchInitialState {
        let previous_resource_dispatch = initial_state
            .prev_dispatch_mw
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(local_idx, mw)| crate::request::ResourceDispatchPoint {
                resource_id: test_local_resource_id(local_idx),
                mw,
            })
            .collect();
        let previous_hvdc_dispatch = initial_state
            .prev_hvdc_dispatch_mw
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(link_idx, mw)| crate::request::HvdcDispatchPoint {
                link_id: test_hvdc_link_id(link_idx),
                mw,
            })
            .collect();
        let storage_soc_overrides = initial_state
            .storage_soc_override
            .unwrap_or_default()
            .into_iter()
            .map(|(global_idx, soc_mwh)| crate::request::StorageSocOverride {
                resource_id: test_global_resource_id(global_idx),
                soc_mwh,
            })
            .collect();
        crate::request::DispatchInitialState {
            previous_resource_dispatch,
            previous_hvdc_dispatch,
            storage_soc_overrides,
        }
    }

    fn request_emission_profile_from_legacy(
        emission_profile: Option<EmissionProfile>,
    ) -> Option<crate::request::EmissionProfile> {
        emission_profile.map(|profile| crate::request::EmissionProfile {
            resources: profile
                .rates_tonnes_per_mwh
                .into_iter()
                .enumerate()
                .map(
                    |(local_idx, rate_tonnes_per_mwh)| crate::request::ResourceEmissionRate {
                        resource_id: test_local_resource_id(local_idx),
                        rate_tonnes_per_mwh,
                    },
                )
                .collect(),
        })
    }

    fn request_must_run_units_from_legacy(
        must_run_units: Option<MustRunUnits>,
    ) -> Option<crate::request::MustRunUnits> {
        must_run_units.map(|units| crate::request::MustRunUnits {
            resource_ids: units
                .unit_indices
                .into_iter()
                .map(test_local_resource_id)
                .collect(),
        })
    }

    fn request_commitment_options_from_legacy(
        options: IndexedCommitmentOptions,
    ) -> crate::request::CommitmentOptions {
        let max_len = [
            options
                .initial_commitment
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default(),
            options
                .initial_hours_on
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default(),
            options
                .initial_offline_hours
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default(),
            options
                .initial_starts_24h
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default(),
            options
                .initial_starts_168h
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default(),
            options
                .initial_energy_mwh_24h
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default(),
            options
                .warm_start_commitment_mask
                .as_ref()
                .map(Vec::len)
                .unwrap_or_else(|| {
                    options
                        .warm_start_commitment
                        .as_ref()
                        .and_then(|periods| periods.first().map(Vec::len))
                        .unwrap_or_default()
                }),
        ]
        .into_iter()
        .max()
        .unwrap_or_default();
        let mut initial_conditions = Vec::new();
        for local_idx in 0..max_len {
            let committed = options
                .initial_commitment
                .as_ref()
                .and_then(|values| values.get(local_idx))
                .copied();
            let hours_on = options
                .initial_hours_on
                .as_ref()
                .and_then(|values| values.get(local_idx))
                .copied();
            let offline_hours = options
                .initial_offline_hours
                .as_ref()
                .and_then(|values| values.get(local_idx))
                .copied();
            let starts_24h = options
                .initial_starts_24h
                .as_ref()
                .and_then(|values| values.get(local_idx))
                .copied();
            let starts_168h = options
                .initial_starts_168h
                .as_ref()
                .and_then(|values| values.get(local_idx))
                .copied();
            let energy_mwh_24h = options
                .initial_energy_mwh_24h
                .as_ref()
                .and_then(|values| values.get(local_idx))
                .copied();
            if committed.is_none()
                && hours_on.is_none()
                && offline_hours.is_none()
                && starts_24h.is_none()
                && starts_168h.is_none()
                && energy_mwh_24h.is_none()
            {
                continue;
            }
            initial_conditions.push(crate::request::CommitmentInitialCondition {
                resource_id: test_local_resource_id(local_idx),
                committed,
                hours_on,
                offline_hours,
                starts_24h,
                starts_168h,
                energy_mwh_24h,
            });
        }
        let warm_start_commitment = if masked_values_present(
            options
                .warm_start_commitment
                .as_ref()
                .and_then(|periods| periods.first().map(Vec::as_slice)),
            options.warm_start_commitment_mask.as_deref(),
        ) {
            let periods = options.warm_start_commitment.as_ref();
            Some(
                (0..max_len)
                    .filter(|&local_idx| {
                        options
                            .warm_start_commitment_mask
                            .as_ref()
                            .is_none_or(|mask| mask.get(local_idx).copied().unwrap_or(false))
                    })
                    .map(|local_idx| crate::request::ResourcePeriodCommitment {
                        resource_id: test_local_resource_id(local_idx),
                        periods: periods
                            .map(|rows| {
                                rows.iter()
                                    .map(|row| row.get(local_idx).copied().unwrap_or(false))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect(),
            )
        } else {
            None
        };
        crate::request::CommitmentOptions {
            initial_conditions,
            warm_start_commitment: warm_start_commitment.unwrap_or_default(),
            time_limit_secs: options.time_limit_secs,
            mip_rel_gap: options.mip_rel_gap,
            mip_gap_schedule: options.mip_gap_schedule.clone(),
            disable_warm_start: options.disable_warm_start,
        }
    }

    fn fixed_schedule(
        initial_commitment: Vec<bool>,
        per_period: Option<Vec<Vec<bool>>>,
    ) -> CommitmentSchedule {
        let resources = initial_commitment
            .into_iter()
            .enumerate()
            .map(
                |(local_idx, initial)| crate::request::ResourceCommitmentSchedule {
                    resource_id: test_local_resource_id(local_idx),
                    initial,
                    periods: per_period
                        .as_ref()
                        .map(|matrix| matrix.iter().map(|period| period[local_idx]).collect()),
                },
            )
            .collect();
        CommitmentSchedule { resources }
    }

    fn minimum_commitment(
        da_commitment: Vec<Vec<bool>>,
    ) -> Vec<crate::request::ResourcePeriodCommitment> {
        let resource_count = da_commitment.first().map(Vec::len).unwrap_or_default();
        (0..resource_count)
            .map(|local_idx| crate::request::ResourcePeriodCommitment {
                resource_id: test_local_resource_id(local_idx),
                periods: da_commitment
                    .iter()
                    .map(|period| period[local_idx])
                    .collect(),
            })
            .collect()
    }

    fn legacy_options_from_normalized_request(
        normalized: &NormalizedDispatchRequest,
        initial_state: IndexedDispatchInitialState,
    ) -> DispatchOptions {
        DispatchOptions {
            formulation: normalized.formulation,
            horizon: normalized.horizon,
            commitment: normalized.commitment.clone(),
            n_periods: normalized.input.n_periods,
            dt_hours: normalized.input.dt_hours,
            load_profiles: normalized.input.load_profiles.clone(),
            ac_bus_load_profiles: normalized.input.ac_bus_load_profiles.clone(),
            renewable_profiles: normalized.input.renewable_profiles.clone(),
            gen_derate_profiles: normalized.input.gen_derate_profiles.clone(),
            generator_dispatch_bounds: normalized.input.generator_dispatch_bounds.clone(),
            branch_derate_profiles: normalized.input.branch_derate_profiles.clone(),
            hvdc_derate_profiles: normalized.input.hvdc_derate_profiles.clone(),
            initial_state,
            tolerance: normalized.input.tolerance,
            run_pricing: normalized.input.run_pricing,
            ac_relax_committed_pmin_to_zero: normalized.input.ac_relax_committed_pmin_to_zero,
            enforce_thermal_limits: normalized.input.enforce_thermal_limits,
            min_rate_a: normalized.input.min_rate_a,
            enforce_flowgates: normalized.input.enforce_flowgates,
            max_nomogram_iter: normalized.input.max_nomogram_iter,
            par_setpoints: normalized.input.par_setpoints.clone(),
            reserve_products: normalized.input.reserve_products.clone(),
            system_reserve_requirements: normalized.input.system_reserve_requirements.clone(),
            zonal_reserve_requirements: normalized.input.zonal_reserve_requirements.clone(),
            ramp_sharing: normalized.input.ramp_sharing.clone(),
            co2_cap_t: normalized.input.co2_cap_t,
            co2_price_per_t: normalized.input.co2_price_per_t,
            emission_profile: normalized.input.emission_profile.clone(),
            carbon_price: normalized.input.carbon_price,
            storage_self_schedules: normalized.input.storage_self_schedules.clone(),
            fixed_hvdc_dispatch_mw: normalized.input.fixed_hvdc_dispatch_mw.clone(),
            fixed_hvdc_dispatch_q_fr_mvar: normalized.input.fixed_hvdc_dispatch_q_fr_mvar.clone(),
            fixed_hvdc_dispatch_q_to_mvar: normalized.input.fixed_hvdc_dispatch_q_to_mvar.clone(),
            ac_hvdc_warm_start_p_mw: normalized.input.ac_hvdc_warm_start_p_mw.clone(),
            ac_hvdc_warm_start_q_fr_mvar: normalized.input.ac_hvdc_warm_start_q_fr_mvar.clone(),
            ac_hvdc_warm_start_q_to_mvar: normalized.input.ac_hvdc_warm_start_q_to_mvar.clone(),
            storage_reserve_soc_impact: normalized.input.storage_reserve_soc_impact.clone(),
            offer_schedules: normalized.input.offer_schedules.clone(),
            dl_offer_schedules: normalized.input.dl_offer_schedules.clone(),
            gen_reserve_offer_schedules: normalized.input.gen_reserve_offer_schedules.clone(),
            dl_reserve_offer_schedules: normalized.input.dl_reserve_offer_schedules.clone(),
            cc_config_offers: normalized.input.cc_config_offers.clone(),
            hvdc_links: normalized.input.hvdc_links.clone(),
            tie_line_limits: normalized.input.tie_line_limits.clone(),
            generator_area: normalized.input.generator_area.clone(),
            load_area: normalized.input.load_area.clone(),
            must_run_units: normalized.input.must_run_units.clone(),
            frequency_security: normalized.input.frequency_security.clone(),
            dispatchable_loads: normalized.input.dispatchable_loads.clone(),
            virtual_bids: normalized.input.virtual_bids.clone(),
            power_balance_penalty: normalized.input.power_balance_penalty.clone(),
            penalty_config: normalized.input.penalty_config.clone(),
            generator_cost_modeling: normalized.input.generator_cost_modeling.clone(),
            use_loss_factors: normalized.input.use_loss_factors,
            max_loss_factor_iters: normalized.input.max_loss_factor_iters,
            loss_factor_tol: normalized.input.loss_factor_tol,
            ac_opf: normalized.ac_opf.clone(),
            enforce_forbidden_zones: normalized.input.enforce_forbidden_zones,
            foz_max_transit_periods: normalized.input.foz_max_transit_periods,
            enforce_shutdown_deloading: normalized.input.enforce_shutdown_deloading,
            offline_commitment_trajectories: normalized.input.offline_commitment_trajectories,
            ramp_mode: normalized.input.ramp_mode.clone(),
            ramp_constraints_hard: normalized.input.ramp_constraints_hard,
            energy_window_constraints_hard: normalized.input.energy_window_constraints_hard,
            energy_window_violation_per_puh: normalized.input.energy_window_violation_per_puh,
            allow_branch_switching: normalized.input.allow_branch_switching,
            branch_switching_big_m_factor: normalized.input.branch_switching_big_m_factor,
            regulation_eligible: normalized.input.regulation_eligible.clone(),
            startup_window_limits: normalized.input.startup_window_limits.clone(),
            energy_window_limits: normalized.input.energy_window_limits.clone(),
            commitment_constraints: normalized.input.commitment_constraints.clone(),
            peak_demand_charges: normalized.input.peak_demand_charges.clone(),
            ph_head_curves: normalized.input.ph_head_curves.clone(),
            ph_mode_constraints: normalized.input.ph_mode_constraints.clone(),
            ac_generator_warm_start_p_mw: normalized.input.ac_generator_warm_start_p_mw.clone(),
            ac_generator_warm_start_q_mvar: normalized.input.ac_generator_warm_start_q_mvar.clone(),
            ac_bus_warm_start_vm_pu: normalized.input.ac_bus_warm_start_vm_pu.clone(),
            ac_bus_warm_start_va_rad: normalized.input.ac_bus_warm_start_va_rad.clone(),
            ac_dispatchable_load_warm_start_p_mw: normalized
                .input
                .ac_dispatchable_load_warm_start_p_mw
                .clone(),
            ac_dispatchable_load_warm_start_q_mvar: normalized
                .input
                .ac_dispatchable_load_warm_start_q_mvar
                .clone(),
            ac_target_tracking: normalized.input.ac_target_tracking.clone(),
            sced_ac_benders: normalized.input.sced_ac_benders.clone(),
            lp_solver: normalized.input.lp_solver.clone(),
        }
    }

    fn request_from_input(
        formulation: Formulation,
        coupling: IntervalCoupling,
        commitment: crate::request::CommitmentPolicy,
        input: DispatchInput,
    ) -> DispatchRequest {
        let state = crate::request::DispatchState {
            initial: request_initial_state_from_legacy(input.initial_state),
        };
        let market = crate::request::DispatchMarket {
            reserve_products: input.reserve_products,
            system_reserve_requirements: input.system_reserve_requirements,
            zonal_reserve_requirements: input.zonal_reserve_requirements,
            ramp_sharing: input.ramp_sharing,
            co2_cap_t: input.co2_cap_t,
            co2_price_per_t: input.co2_price_per_t,
            emission_profile: request_emission_profile_from_legacy(input.emission_profile),
            carbon_price: input.carbon_price,
            storage_self_schedules: input
                .storage_self_schedules
                .unwrap_or_default()
                .into_iter()
                .map(
                    |(global_idx, values_mw)| crate::request::StoragePowerSchedule {
                        resource_id: test_global_resource_id(global_idx),
                        values_mw,
                    },
                )
                .collect(),
            storage_reserve_soc_impacts: input
                .storage_reserve_soc_impact
                .into_iter()
                .flat_map(|(global_idx, impacts)| {
                    impacts
                        .into_iter()
                        .map(move |(product_id, values_mwh_per_mw)| {
                            crate::request::StorageReserveSocImpact {
                                resource_id: test_global_resource_id(global_idx),
                                product_id,
                                values_mwh_per_mw,
                            }
                        })
                })
                .collect(),
            generator_offer_schedules: input
                .offer_schedules
                .into_iter()
                .map(
                    |(global_idx, schedule)| crate::request::GeneratorOfferSchedule {
                        resource_id: test_global_resource_id(global_idx),
                        schedule,
                    },
                )
                .collect(),
            dispatchable_load_offer_schedules: input
                .dl_offer_schedules
                .into_iter()
                .map(
                    |(dl_idx, schedule)| crate::request::DispatchableLoadOfferSchedule {
                        resource_id: test_dispatchable_load_id(dl_idx),
                        schedule,
                    },
                )
                .collect(),
            generator_reserve_offer_schedules: input
                .gen_reserve_offer_schedules
                .into_iter()
                .map(
                    |(global_idx, schedule)| crate::request::GeneratorReserveOfferSchedule {
                        resource_id: test_global_resource_id(global_idx),
                        schedule,
                    },
                )
                .collect(),
            dispatchable_load_reserve_offer_schedules: input
                .dl_reserve_offer_schedules
                .into_iter()
                .map(
                    |(dl_idx, schedule)| crate::request::DispatchableLoadReserveOfferSchedule {
                        resource_id: test_dispatchable_load_id(dl_idx),
                        schedule,
                    },
                )
                .collect(),
            combined_cycle_offer_schedules: input
                .cc_config_offers
                .into_iter()
                .enumerate()
                .flat_map(|(plant_idx, schedules)| {
                    schedules
                        .into_iter()
                        .enumerate()
                        .map(move |(config_idx, schedule)| {
                            crate::request::CombinedCycleConfigOfferSchedule {
                                plant_id: format!("__cc:{plant_idx}"),
                                config_name: format!("__cc_config:{config_idx}"),
                                schedule,
                            }
                        })
                })
                .collect(),
            tie_line_limits: input.tie_line_limits,
            resource_area_assignments: input
                .generator_area
                .into_iter()
                .enumerate()
                .map(
                    |(local_idx, area_id)| crate::request::ResourceAreaAssignment {
                        resource_id: test_local_resource_id(local_idx),
                        area_id: area_id.into(),
                    },
                )
                .collect(),
            bus_area_assignments: input
                .load_area
                .into_iter()
                .enumerate()
                .map(|(bus_idx, area_id)| crate::request::BusAreaAssignment {
                    bus_number: test_bus_number(bus_idx),
                    area_id: area_id.into(),
                })
                .collect(),
            must_run_units: request_must_run_units_from_legacy(input.must_run_units),
            frequency_security: input.frequency_security,
            dispatchable_loads: input.dispatchable_loads,
            virtual_bids: input.virtual_bids,
            power_balance_penalty: input.power_balance_penalty,
            penalty_config: input.penalty_config,
            generator_cost_modeling: input.generator_cost_modeling,
            regulation_eligibility: input
                .regulation_eligible
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(
                    |(local_idx, eligible)| crate::request::ResourceEligibility {
                        resource_id: test_local_resource_id(local_idx),
                        eligible,
                    },
                )
                .collect(),
            startup_window_limits: input
                .startup_window_limits
                .into_iter()
                .map(|limit| crate::request::ResourceStartupWindowLimit {
                    resource_id: test_local_resource_id(limit.gen_index),
                    start_period_idx: limit.start_period_idx,
                    end_period_idx: limit.end_period_idx,
                    max_startups: limit.max_startups,
                })
                .collect(),
            energy_window_limits: input
                .energy_window_limits
                .into_iter()
                .map(|limit| crate::request::ResourceEnergyWindowLimit {
                    resource_id: test_local_resource_id(limit.gen_index),
                    start_period_idx: limit.start_period_idx,
                    end_period_idx: limit.end_period_idx,
                    min_energy_mwh: limit.min_energy_mwh,
                    max_energy_mwh: limit.max_energy_mwh,
                })
                .collect(),
            peak_demand_charges: input
                .peak_demand_charges
                .into_iter()
                .map(|charge| crate::request::PeakDemandCharge {
                    name: charge.name,
                    resource_id: test_local_resource_id(charge.gen_index),
                    period_indices: charge.period_indices,
                    charge_per_mw: charge.charge_per_mw,
                })
                .collect(),
            commitment_constraints: input
                .commitment_constraints
                .into_iter()
                .map(|constraint| crate::request::CommitmentConstraint {
                    name: constraint.name,
                    period_idx: constraint.period_idx,
                    terms: constraint
                        .terms
                        .into_iter()
                        .map(|term| crate::request::CommitmentTerm {
                            resource_id: test_local_resource_id(term.gen_index),
                            coeff: term.coeff,
                        })
                        .collect(),
                    lower_bound: constraint.lower_bound,
                    penalty_cost: constraint.penalty_cost,
                })
                .collect(),
        };
        let network = crate::request::DispatchNetwork {
            thermal_limits: crate::request::ThermalLimitPolicy {
                enforce: input.enforce_thermal_limits,
                min_rate_a: input.min_rate_a,
            },
            flowgates: crate::request::FlowgatePolicy {
                enabled: input.enforce_flowgates,
                max_nomogram_iterations: input.max_nomogram_iter,
            },
            par_setpoints: input.par_setpoints,
            hvdc_links: input.hvdc_links,
            loss_factors: crate::request::LossFactorPolicy {
                enabled: input.use_loss_factors,
                max_iterations: input.max_loss_factor_iters,
                tolerance: input.loss_factor_tol,
                warm_start_mode: Default::default(),
                scuc_loss_treatment: Default::default(),
            },
            forbidden_zones: crate::request::ForbiddenZonePolicy {
                enabled: input.enforce_forbidden_zones,
                max_transit_periods: input.foz_max_transit_periods,
            },
            commitment_transitions: crate::request::CommitmentTransitionPolicy {
                shutdown_deloading: input.enforce_shutdown_deloading,
                trajectory_mode: if input.offline_commitment_trajectories {
                    crate::request::CommitmentTrajectoryMode::OfflineTrajectory
                } else {
                    crate::request::CommitmentTrajectoryMode::InlineDeloading
                },
            },
            ramping: crate::request::RampPolicy {
                mode: input.ramp_mode,
                enforcement: if input.ramp_constraints_hard {
                    crate::request::ConstraintEnforcement::Hard
                } else {
                    crate::request::ConstraintEnforcement::Soft
                },
            },
            energy_windows: crate::request::EnergyWindowPolicy {
                enforcement: if input.energy_window_constraints_hard {
                    crate::request::ConstraintEnforcement::Hard
                } else {
                    crate::request::ConstraintEnforcement::Soft
                },
                penalty_per_puh: input.energy_window_violation_per_puh,
            },
            topology_control: crate::request::TopologyControlPolicy {
                mode: if input.allow_branch_switching {
                    crate::request::TopologyControlMode::Switchable
                } else {
                    crate::request::TopologyControlMode::Fixed
                },
                branch_switching_big_m_factor: input.branch_switching_big_m_factor,
            },
            security: None,
            ph_head_curves: input
                .ph_head_curves
                .into_iter()
                .map(|curve| crate::request::PhHeadCurve {
                    resource_id: test_global_resource_id(curve.gen_index),
                    breakpoints: curve.breakpoints,
                })
                .collect(),
            ph_mode_constraints: input
                .ph_mode_constraints
                .into_iter()
                .map(|constraint| crate::request::PhModeConstraint {
                    resource_id: test_global_resource_id(constraint.gen_index),
                    min_gen_run_periods: constraint.min_gen_run_periods,
                    min_pump_run_periods: constraint.min_pump_run_periods,
                    pump_to_gen_periods: constraint.pump_to_gen_periods,
                    gen_to_pump_periods: constraint.gen_to_pump_periods,
                    max_pump_starts: constraint.max_pump_starts,
                })
                .collect(),
        };
        DispatchRequest {
            formulation,
            coupling,
            commitment,
            timeline: crate::request::DispatchTimeline {
                periods: input.n_periods,
                interval_hours: input.dt_hours,
                interval_hours_by_period: input.period_hours.clone(),
            },
            profiles: crate::request::DispatchProfiles {
                load: input.load_profiles.into(),
                ac_bus_load: input.ac_bus_load_profiles,
                renewable: input.renewable_profiles.into(),
                generator_derates: input.gen_derate_profiles.into(),
                generator_dispatch_bounds: input.generator_dispatch_bounds,
                branch_derates: input.branch_derate_profiles.into(),
                hvdc_derates: input.hvdc_derate_profiles.into(),
            },
            state,
            market,
            network,
            runtime: crate::request::DispatchRuntime {
                tolerance: input.tolerance,
                run_pricing: input.run_pricing,
                ac_relax_committed_pmin_to_zero: input.ac_relax_committed_pmin_to_zero,
                ac_opf: None,
                fixed_hvdc_dispatch: input
                    .fixed_hvdc_dispatch_mw
                    .iter()
                    .map(|(link_idx, p_mw)| {
                        let q_fr_mvar = input
                            .fixed_hvdc_dispatch_q_fr_mvar
                            .get(link_idx)
                            .cloned()
                            .unwrap_or_default();
                        let q_to_mvar = input
                            .fixed_hvdc_dispatch_q_to_mvar
                            .get(link_idx)
                            .cloned()
                            .unwrap_or_default();
                        crate::request::HvdcPeriodPowerSeries {
                            link_id: test_hvdc_link_id(*link_idx),
                            p_mw: p_mw.clone(),
                            q_fr_mvar,
                            q_to_mvar,
                        }
                    })
                    .collect(),
                ac_dispatch_warm_start: crate::request::AcDispatchWarmStart {
                    buses: input
                        .ac_bus_warm_start_vm_pu
                        .iter()
                        .map(|(bus_idx, vm_pu)| crate::request::BusPeriodVoltageSeries {
                            bus_number: (*bus_idx as u32) + 1,
                            va_rad: input
                                .ac_bus_warm_start_va_rad
                                .get(bus_idx)
                                .cloned()
                                .unwrap_or_else(|| vec![0.0; vm_pu.len()]),
                            vm_pu: vm_pu.clone(),
                        })
                        .collect(),
                    hvdc_links: input
                        .ac_hvdc_warm_start_p_mw
                        .iter()
                        .map(|(link_idx, p_mw)| {
                            let q_fr_mvar = input
                                .ac_hvdc_warm_start_q_fr_mvar
                                .get(link_idx)
                                .cloned()
                                .unwrap_or_default();
                            let q_to_mvar = input
                                .ac_hvdc_warm_start_q_to_mvar
                                .get(link_idx)
                                .cloned()
                                .unwrap_or_default();
                            crate::request::HvdcPeriodPowerSeries {
                                link_id: test_hvdc_link_id(*link_idx),
                                p_mw: p_mw.clone(),
                                q_fr_mvar,
                                q_to_mvar,
                            }
                        })
                        .collect(),
                    ..Default::default()
                },
                ac_target_tracking: input.ac_target_tracking.clone(),
                sced_ac_benders: input.sced_ac_benders.clone(),
                capture_model_diagnostics: false,
                scuc_firm_bus_balance_slacks: false,
                scuc_firm_branch_thermal_slacks: false,
                scuc_disable_bus_power_balance: false,
                ac_sced_period_concurrency: None,
            },
        }
    }

    fn period_by_period_request(input: DispatchInput) -> DispatchRequest {
        request_from_input(
            Formulation::Dc,
            IntervalCoupling::PeriodByPeriod,
            crate::request::CommitmentPolicy::AllCommitted,
            input,
        )
    }

    fn ac_period_by_period_request(input: DispatchInput) -> DispatchRequest {
        request_from_input(
            Formulation::Ac,
            IntervalCoupling::PeriodByPeriod,
            crate::request::CommitmentPolicy::AllCommitted,
            input,
        )
    }

    fn period_by_period_fixed_request(
        input: DispatchInput,
        schedule: CommitmentSchedule,
    ) -> DispatchRequest {
        request_from_input(
            Formulation::Dc,
            IntervalCoupling::PeriodByPeriod,
            crate::request::CommitmentPolicy::Fixed(schedule),
            input,
        )
    }

    fn ac_period_by_period_fixed_request(
        input: DispatchInput,
        schedule: CommitmentSchedule,
    ) -> DispatchRequest {
        request_from_input(
            Formulation::Ac,
            IntervalCoupling::PeriodByPeriod,
            crate::request::CommitmentPolicy::Fixed(schedule),
            input,
        )
    }

    fn time_coupled_request(input: DispatchInput) -> DispatchRequest {
        request_from_input(
            Formulation::Dc,
            IntervalCoupling::TimeCoupled,
            crate::request::CommitmentPolicy::AllCommitted,
            input,
        )
    }

    fn time_coupled_fixed_request(
        input: DispatchInput,
        schedule: CommitmentSchedule,
    ) -> DispatchRequest {
        request_from_input(
            Formulation::Dc,
            IntervalCoupling::TimeCoupled,
            crate::request::CommitmentPolicy::Fixed(schedule),
            input,
        )
    }

    fn scuc_request(input: DispatchInput, options: IndexedCommitmentOptions) -> DispatchRequest {
        request_from_input(
            Formulation::Dc,
            IntervalCoupling::TimeCoupled,
            crate::request::CommitmentPolicy::Optimize(request_commitment_options_from_legacy(
                options,
            )),
            input,
        )
    }

    fn additional_commitment_request(
        input: DispatchInput,
        da_commitment: Vec<Vec<bool>>,
        options: IndexedCommitmentOptions,
    ) -> DispatchRequest {
        request_from_input(
            Formulation::Dc,
            IntervalCoupling::TimeCoupled,
            crate::request::CommitmentPolicy::Additional {
                minimum_commitment: minimum_commitment(da_commitment),
                options: request_commitment_options_from_legacy(options),
            },
            input,
        )
    }

    #[test]
    fn dispatch_only_preserves_period_shape() {
        let period = RawDispatchPeriodResult {
            pg_mw: vec![10.0, 20.0, 30.0],
            total_cost: 123.0,
            ..Default::default()
        };

        let result =
            RawDispatchSolution::dispatch_only(vec![period], 123.0, 0.01, 7, HashMap::new());

        assert_eq!(result.periods.len(), 1);
        assert_eq!(result.periods[0].pg_mw.len(), 3);
    }

    #[test]
    fn ac_sced_dispatch_result_preserves_discrete_control_dispatch() {
        let net = one_bus_reserve_test_network();
        let result = ac_sced_period_to_dispatch_result(
            &net,
            crate::sced::ac::AcScedPeriodSolution {
                pg_mw: vec![10.0],
                qg_mvar: vec![2.0],
                lmp: vec![30.0],
                q_lmp: Vec::new(),
                bus_voltage_pu: vec![1.0],
                bus_angle_rad: vec![0.0],
                storage_soc_mwh: Vec::new(),
                storage_net_mw: Vec::new(),
                dr_results: Default::default(),
                hvdc_dispatch_mw: Vec::new(),
                hvdc_band_dispatch_mw: Vec::new(),
                tap_dispatch: vec![(1, 1.04, 1.05)],
                phase_dispatch: vec![(1, 0.08, 0.1)],
                switched_shunt_dispatch: vec![(
                    "switched_shunt_opf_1_1".to_string(),
                    1,
                    0.02,
                    0.03,
                )],
                producer_q_reserve_up_mvar: Vec::new(),
                producer_q_reserve_down_mvar: Vec::new(),
                consumer_q_reserve_up_mvar: Vec::new(),
                consumer_q_reserve_down_mvar: Vec::new(),
                zone_q_reserve_up_shortfall_mvar: Vec::new(),
                zone_q_reserve_down_shortfall_mvar: Vec::new(),
                total_cost: 100.0,
                objective_terms: Vec::new(),
                branch_shadow_prices: Vec::new(),
                flowgate_shadow_prices: Vec::new(),
                interface_shadow_prices: Vec::new(),
                bus_q_slack_pos_mvar: Vec::new(),
                bus_q_slack_neg_mvar: Vec::new(),
                bus_p_slack_pos_mw: Vec::new(),
                bus_p_slack_neg_mw: Vec::new(),
                thermal_limit_slack_from_mva: Vec::new(),
                thermal_limit_slack_to_mva: Vec::new(),
                vm_slack_high_pu: Vec::new(),
                vm_slack_low_pu: Vec::new(),
                angle_diff_slack_high_rad: Vec::new(),
                angle_diff_slack_low_rad: Vec::new(),
                solve_time_secs: 0.01,
                iterations: 1,
            },
        );

        assert_eq!(result.periods.len(), 1);
        assert_eq!(result.periods[0].tap_dispatch, vec![(1, 1.04, 1.05)]);
        assert_eq!(result.periods[0].phase_dispatch, vec![(1, 0.08, 0.1)]);
        assert_eq!(
            result.periods[0].switched_shunt_dispatch,
            vec![("switched_shunt_opf_1_1".to_string(), 1, 0.02, 0.03)]
        );
    }

    #[test]
    fn ac_reactive_reserve_awards_are_mapped_into_dispatch_result() {
        let net = one_bus_reserve_test_network();
        let result = ac_sced_period_to_dispatch_result(
            &net,
            crate::sced::ac::AcScedPeriodSolution {
                pg_mw: vec![10.0, 0.0],
                qg_mvar: vec![2.0, 0.0],
                lmp: vec![30.0],
                q_lmp: Vec::new(),
                bus_voltage_pu: vec![1.0],
                bus_angle_rad: vec![0.0],
                storage_soc_mwh: Vec::new(),
                storage_net_mw: Vec::new(),
                dr_results: Default::default(),
                hvdc_dispatch_mw: Vec::new(),
                hvdc_band_dispatch_mw: Vec::new(),
                tap_dispatch: Vec::new(),
                phase_dispatch: Vec::new(),
                switched_shunt_dispatch: Vec::new(),
                producer_q_reserve_up_mvar: vec![3.0, 0.0],
                producer_q_reserve_down_mvar: vec![1.5, 0.0],
                consumer_q_reserve_up_mvar: vec![2.0],
                consumer_q_reserve_down_mvar: vec![0.5],
                zone_q_reserve_up_shortfall_mvar: Vec::new(),
                zone_q_reserve_down_shortfall_mvar: Vec::new(),
                total_cost: 100.0,
                objective_terms: Vec::new(),
                branch_shadow_prices: Vec::new(),
                flowgate_shadow_prices: Vec::new(),
                interface_shadow_prices: Vec::new(),
                bus_q_slack_pos_mvar: Vec::new(),
                bus_q_slack_neg_mvar: Vec::new(),
                bus_p_slack_pos_mw: Vec::new(),
                bus_p_slack_neg_mw: Vec::new(),
                thermal_limit_slack_from_mva: Vec::new(),
                thermal_limit_slack_to_mva: Vec::new(),
                vm_slack_high_pu: Vec::new(),
                vm_slack_low_pu: Vec::new(),
                angle_diff_slack_high_rad: Vec::new(),
                angle_diff_slack_low_rad: Vec::new(),
                solve_time_secs: 0.01,
                iterations: 1,
            },
        );

        assert_eq!(
            result.periods[0].reserve_awards.get("q_res_up"),
            Some(&vec![3.0, 0.0])
        );
        assert_eq!(
            result.periods[0].reserve_awards.get("q_res_down"),
            Some(&vec![1.5, 0.0])
        );
        assert_eq!(
            result.periods[0].dr_reserve_awards.get("q_res_up"),
            Some(&vec![2.0])
        );
        assert_eq!(
            result.periods[0].dr_reserve_awards.get("q_res_down"),
            Some(&vec![0.5])
        );
    }

    #[test]
    fn public_periods_include_branch_and_control_state() {
        let net = one_bus_reserve_test_network();
        let mut raw = RawDispatchSolution::dispatch_only(
            vec![RawDispatchPeriodResult {
                tap_dispatch: vec![(0, 1.01, 1.02)],
                phase_dispatch: vec![(0, 0.03, 0.04)],
                switched_shunt_dispatch: vec![(
                    "switched_shunt_opf_1_1".to_string(),
                    1,
                    0.01,
                    0.02,
                )],
                ..RawDispatchPeriodResult::default()
            }],
            0.0,
            0.01,
            1,
            HashMap::new(),
        );
        raw.branch_commitment_state = vec![vec![false, true]];

        let keyed = emit_public_keyed_solution(raw, &net);
        assert_eq!(keyed.periods[0].branch_commitment_state, vec![false, true]);
        assert_eq!(keyed.periods[0].tap_dispatch, vec![(0, 1.01, 1.02)]);
        assert_eq!(keyed.periods[0].phase_dispatch, vec![(0, 0.03, 0.04)]);
        assert_eq!(
            keyed.periods[0].switched_shunt_dispatch,
            vec![("switched_shunt_opf_1_1".to_string(), 1, 0.01, 0.02)]
        );
    }

    #[test]
    fn solve_dispatch_populates_public_catalogs() {
        let net = one_bus_reserve_test_network();
        let result = solve_dispatch(&net, &period_by_period_request(Default::default())).unwrap();

        assert_eq!(result.study.formulation, Formulation::Dc);
        assert_eq!(result.study.coupling, IntervalCoupling::PeriodByPeriod);
        assert_eq!(result.study.commitment, CommitmentPolicyKind::AllCommitted);
        assert_eq!(result.buses.len(), 1);
        assert_eq!(result.buses[0].bus_number, 1);
        assert_eq!(result.resources.len(), 2);
        assert!(
            result
                .resources
                .iter()
                .all(|resource| resource.bus_number == Some(1)),
            "expected one-bus resources to map to bus 1"
        );
    }

    fn two_gen_carbon_test_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("dispatch_two_gen_carbon_test");
        net.base_mva = 100.0;

        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 100.0, 0.0));

        let mut dirty = Generator::new(1, 0.0, 100.0);
        dirty.pmin = 0.0;
        dirty.pmax = 100.0;
        dirty.in_service = true;
        dirty.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        dirty.fuel.get_or_insert_default().emission_rates.co2 = 0.5;

        let mut clean = Generator::new(1, 0.0, 100.0);
        clean.pmin = 0.0;
        clean.pmax = 100.0;
        clean.in_service = true;
        clean.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        clean.fuel.get_or_insert_default().emission_rates.co2 = 0.0;

        net.generators = vec![dirty, clean];
        net
    }

    fn one_bus_reserve_test_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::{CostCurve, ReserveOffer};
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("dispatch_one_bus_reserve_test");
        net.base_mva = 100.0;

        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 80.0, 0.0));

        let mut g0 = Generator::new(1, 50.0, 1.0);
        g0.pmin = 50.0;
        g0.pmax = 300.0;
        g0.in_service = true;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        g0.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "spin".into(),
                capacity_mw: 50.0,
                cost_per_mwh: 0.0,
            });

        let mut g1 = Generator::new(1, 50.0, 1.0);
        g1.pmin = 50.0;
        g1.pmax = 200.0;
        g1.in_service = true;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        g1.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "spin".into(),
                capacity_mw: 30.0,
                cost_per_mwh: 0.0,
            });

        net.generators = vec![g0, g1];
        net
    }

    fn one_bus_storage_test_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, StorageParams};

        let mut net = Network::new("dispatch_one_bus_storage_test");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.generators.push(Generator {
            bus: 1,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                startup: 0.0,
                shutdown: 0.0,
                coeffs: vec![0.0],
            }),
            storage: Some(StorageParams {
                charge_efficiency: 1.0,
                discharge_efficiency: 1.0,
                energy_capacity_mwh: 20.0,
                soc_initial_mwh: 10.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 20.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: surge_network::network::StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        });
        net
    }

    fn three_bus_flowgate_test_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("dispatch_flowgate_pricing_test");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses = vec![b1, b2, b3];
        net.loads.push(Load::new(2, 150.0, 0.0));

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 200.0;
        br12.circuit = "1".to_string();
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 200.0;
        br23.circuit = "1".to_string();
        net.branches = vec![br12, br23];

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 250.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        let mut g2 = Generator::new(3, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 250.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators = vec![g1, g2];
        net
    }

    fn two_bus_loss_test_network() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("dispatch_loss_pricing_test");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.loads.push(Load::new(2, 50.0, 0.0));

        let mut br = Branch::new_line(1, 2, 0.01, 0.05, 0.0);
        br.rating_a_mva = 200.0;
        br.circuit = "1".to_string();
        net.branches.push(br);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g1);
        net
    }

    /// Verify solve_dispatch(Dc, Sequential, AllCommitted, 1 period) matches
    /// the old solve_sced path exactly.
    #[test]
    fn test_dispatch_dc_single_period_matches_sced() {
        if !data_available() {
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

        // Direct solver path
        let old_opts = DispatchOptions::default();
        let old_sol = crate::sced::solve_sced(&net, &old_opts).unwrap();

        // New path
        let new_sol = solve_dispatch(&net, &period_by_period_request(Default::default())).unwrap();

        assert_eq!(new_sol.study.periods, 1);
        assert_eq!(new_sol.periods.len(), 1);
        assert!(
            (new_sol.summary.total_cost - old_sol.dispatch.total_cost).abs() < 1e-6,
            "cost mismatch: new={} old={}",
            new_sol.summary.total_cost,
            old_sol.dispatch.total_cost
        );

        // LMP match
        for (i, (new_lmp, old_lmp)) in new_sol.periods[0]
            .lmp
            .iter()
            .zip(old_sol.dispatch.lmp.iter())
            .enumerate()
        {
            assert!(
                (new_lmp - old_lmp).abs() < 1e-6,
                "LMP mismatch at bus {i}: new={new_lmp} old={old_lmp}"
            );
        }
    }

    /// Verify sequential multi-period dispatch matches the test-only SCED compatibility path.
    #[test]
    fn test_dispatch_dc_multi_period_sequential() {
        if !data_available() {
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

        let request = period_by_period_request(DispatchInput {
            n_periods: 4,
            ..DispatchInput::default()
        });
        let sol = solve_dispatch(&net, &request).unwrap();
        let normalized = request.normalize().expect("normalize canonical request");
        assert_eq!(sol.study.periods, 4);
        assert_eq!(sol.periods.len(), 4);
        assert!(sol.summary.total_cost > 0.0);

        let mut old_initial_state = IndexedDispatchInitialState::default();
        let storage_indices: Vec<usize> = net
            .generators
            .iter()
            .enumerate()
            .filter_map(|(gi, g)| (g.in_service && g.storage.is_some()).then_some(gi))
            .collect();

        for t in 0..4 {
            let net_t = apply_profiles(
                &net,
                &DispatchProblemSpec::from_request(&normalized.input, &normalized.commitment),
                t,
            );
            let mut old_options =
                legacy_options_from_normalized_request(&normalized, old_initial_state.clone());
            old_options.n_periods = 1;
            old_options.load_profiles = Default::default();
            old_options.renewable_profiles = Default::default();
            old_options.gen_derate_profiles = Default::default();
            old_options.branch_derate_profiles = Default::default();
            old_options.hvdc_derate_profiles = Default::default();
            let old_sol = crate::sced::solve_sced(&net_t, &old_options).unwrap();
            let new_period = &sol.periods[t];

            assert!(
                (new_period.total_cost - old_sol.dispatch.total_cost).abs() < 1e-6,
                "period {t}: cost mismatch new={} old={}",
                new_period.total_cost,
                old_sol.dispatch.total_cost
            );
            assert_eq!(
                new_period.pg_mw.len(),
                old_sol.dispatch.pg_mw.len(),
                "period {t}: generator count mismatch"
            );
            for (gi, (new_pg, old_pg)) in new_period
                .pg_mw
                .iter()
                .zip(old_sol.dispatch.pg_mw.iter())
                .enumerate()
            {
                assert!(
                    (new_pg - old_pg).abs() < 1e-6,
                    "period {t}: pg mismatch at generator {gi}: new={new_pg} old={old_pg}"
                );
            }
            for (bi, (new_lmp, old_lmp)) in new_period
                .lmp
                .iter()
                .zip(old_sol.dispatch.lmp.iter())
                .enumerate()
            {
                assert!(
                    (new_lmp - old_lmp).abs() < 1e-6,
                    "period {t}: LMP mismatch at bus {bi}: new={new_lmp} old={old_lmp}"
                );
            }

            old_initial_state.prev_dispatch_mw = Some(old_sol.dispatch.pg_mw.clone());
            old_initial_state.prev_hvdc_dispatch_mw =
                (!old_sol.dispatch.hvdc_dispatch_mw.is_empty())
                    .then_some(old_sol.dispatch.hvdc_dispatch_mw.clone());
            if !storage_indices.is_empty() {
                let mut storage_soc_override = HashMap::with_capacity(storage_indices.len());
                for (storage_slot, &gi) in storage_indices.iter().enumerate() {
                    if let Some(&soc) = old_sol.dispatch.storage_soc_mwh.get(storage_slot) {
                        storage_soc_override.insert(gi, soc);
                    }
                }
                old_initial_state.storage_soc_override = Some(storage_soc_override);
            }
        }

        let cost0 = sol.periods[0].pg_mw.iter().sum::<f64>();
        for t in 1..4 {
            let cost_t = sol.periods[t].pg_mw.iter().sum::<f64>();
            assert!(
                (cost_t - cost0).abs() < 1.0,
                "dispatch mismatch between periods"
            );
        }
    }

    #[test]
    fn test_sequential_accumulator_tracks_iterations() {
        use surge_network::Network;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("sequential_iteration_accumulator");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.generators.push(Generator::new(1, 0.0, 1.0));

        let request = period_by_period_fixed_request(
            DispatchInput {
                n_periods: 2,
                ..DispatchInput::default()
            },
            fixed_schedule(vec![true], None),
        );
        let normalized = request
            .resolve_with_options(&net, &crate::request::DispatchSolveOptions::default())
            .expect("request should normalize");
        let problem_spec =
            DispatchProblemSpec::from_request(&normalized.input, &normalized.commitment);
        let mut accumulator = SequentialDispatchAccumulator::new(&net, problem_spec);

        accumulator.record_period(
            &net,
            RawDispatchPeriodResult {
                pg_mw: vec![50.0],
                total_cost: 100.0,
                ..RawDispatchPeriodResult::default()
            },
            None,
            3,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        accumulator.record_period(
            &net,
            RawDispatchPeriodResult {
                pg_mw: vec![60.0],
                total_cost: 120.0,
                ..RawDispatchPeriodResult::default()
            },
            None,
            4,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );

        let result = accumulator.finish();
        assert_eq!(result.diagnostics.iterations, 7);
    }

    #[test]
    fn test_sequential_accumulator_carries_exact_dispatch_profile_as_prev_dispatch() {
        use surge_network::Network;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("sequential_exact_dispatch_override");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        let mut generator = Generator::new(1, 0.0, 1.0);
        generator.id = "g0".into();
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        net.generators.push(generator);

        let request = period_by_period_fixed_request(
            DispatchInput {
                n_periods: 2,
                generator_dispatch_bounds: crate::request::GeneratorDispatchBoundsProfiles {
                    profiles: vec![crate::request::GeneratorDispatchBoundsProfile {
                        resource_id: "g0".into(),
                        p_min_mw: vec![10.0, 20.0],
                        p_max_mw: vec![10.0, 20.0],
                        q_min_mvar: None,
                        q_max_mvar: None,
                    }],
                },
                ..DispatchInput::default()
            },
            fixed_schedule(vec![true], None),
        );
        let normalized = request
            .resolve_with_options(&net, &crate::request::DispatchSolveOptions::default())
            .expect("request should normalize");
        let problem_spec =
            DispatchProblemSpec::from_request(&normalized.input, &normalized.commitment);
        let mut accumulator = SequentialDispatchAccumulator::new(&net, problem_spec);

        accumulator.record_period(
            &net,
            RawDispatchPeriodResult {
                pg_mw: vec![7.0],
                total_cost: 100.0,
                ..RawDispatchPeriodResult::default()
            },
            None,
            1,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );

        let next_context = accumulator.period_context(1, None);
        assert_eq!(next_context.prev_dispatch_at(0), Some(10.0));
    }

    /// Verify solve_dispatch with Optimize commitment routes to SCUC.
    #[test]
    fn test_dispatch_dc_time_coupled_optimize() {
        if !data_available() {
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 4,
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions {
                    n_cost_segments: 5,
                    ..IndexedCommitmentOptions::default()
                },
            ),
        )
        .unwrap();
        assert_eq!(sol.study.periods, 4);
        // SCUC should produce commitment schedule
        assert!(sol.commitment.is_some());
        let commitment = sol.commitment.as_ref().unwrap();
        assert_eq!(commitment.len(), 4); // 4 periods
        assert!(sol.summary.total_cost > 0.0);
    }

    /// Verify canonical SCUC routing matches the direct SCUC compatibility path.
    #[test]
    fn test_dispatch_dc_scuc_matches_compatibility_path() {
        if !data_available() {
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

        let request = scuc_request(
            DispatchInput {
                n_periods: 2,
                ..DispatchInput::default()
            },
            IndexedCommitmentOptions {
                n_cost_segments: 5,
                ..IndexedCommitmentOptions::default()
            },
        );
        let new_sol = solve_dispatch(&net, &request).unwrap();
        let normalized = request.normalize().expect("normalize SCUC request");
        let old_options = legacy_options_from_normalized_request(
            &normalized,
            IndexedDispatchInitialState::default(),
        );
        let old_sol = crate::scuc::solve::solve_scuc_with_problem_spec(
            &net,
            DispatchProblemSpec::from_options(&old_options),
        )
        .unwrap();

        assert_eq!(new_sol.study.periods, 2);
        assert_eq!(new_sol.periods.len(), old_sol.periods.len());
        assert!(
            (new_sol.summary.total_cost - old_sol.summary.total_cost).abs() < 1e-6,
            "SCUC cost mismatch: new={} old={}",
            new_sol.summary.total_cost,
            old_sol.summary.total_cost
        );
        assert_eq!(new_sol.commitment, old_sol.commitment);
        assert_eq!(new_sol.startup, old_sol.startup);
        assert_eq!(new_sol.shutdown, old_sol.shutdown);
        assert_eq!(
            new_sol.diagnostics.pricing_converged,
            old_sol.diagnostics.pricing_converged
        );
        assert_eq!(
            new_sol.diagnostics.penalty_slack_values,
            old_sol.diagnostics.penalty_slack_values
        );

        for (t, (new_period, old_period)) in new_sol
            .periods
            .iter()
            .zip(old_sol.periods.iter())
            .enumerate()
        {
            assert_eq!(
                new_period.pg_mw.len(),
                old_period.pg_mw.len(),
                "period {t}: generator count mismatch"
            );
            for (gi, (new_pg, old_pg)) in new_period
                .pg_mw
                .iter()
                .zip(old_period.pg_mw.iter())
                .enumerate()
            {
                assert!(
                    (new_pg - old_pg).abs() < 1e-6,
                    "period {t}: pg mismatch at generator {gi}: new={new_pg} old={old_pg}"
                );
            }
            for (bi, (new_lmp, old_lmp)) in
                new_period.lmp.iter().zip(old_period.lmp.iter()).enumerate()
            {
                assert!(
                    (new_lmp - old_lmp).abs() < 1e-6,
                    "period {t}: LMP mismatch at bus {bi}: new={new_lmp} old={old_lmp}"
                );
            }
        }
    }

    /// Verify canonical SCUC routing preserves soft commitment-cut slack extraction.
    #[test]
    fn test_dispatch_dc_scuc_extracts_penalty_slack_values() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut network = Network::new("single_gen_scuc_test");
        network.base_mva = 100.0;

        let bus = Bus::new(1, BusType::Slack, 138.0);
        network.buses.push(bus);
        network.loads.push(Load::new(1, 50.0, 0.0));

        let mut generator = Generator::new(1, 0.0, 1.0);
        generator.in_service = true;
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0],
        });
        network.generators.push(generator);

        let solution = solve_dispatch(
            &network,
            &scuc_request(
                DispatchInput {
                    n_periods: 1,
                    enforce_thermal_limits: false,
                    commitment_constraints: vec![IndexedCommitmentConstraint {
                        name: "soft_cut".into(),
                        period_idx: 0,
                        terms: vec![IndexedCommitmentTerm {
                            gen_index: 0,
                            coeff: 1.0,
                        }],
                        lower_bound: 2.0,
                        penalty_cost: Some(100.0),
                    }],
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .expect("soft commitment cut should solve");

        assert_eq!(solution.diagnostics.penalty_slack_values.len(), 1);
        assert!(
            (solution.diagnostics.penalty_slack_values[0] - 1.0).abs() < 1e-6,
            "expected one unit of slack, got {:?}",
            solution.diagnostics.penalty_slack_values
        );
    }

    /// Verify canonical additional-commitment routing matches the direct SCUC path.
    #[test]
    fn test_dispatch_dc_additional_commitment_matches_compatibility_path() {
        if !data_available() {
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

        let da_commitment = vec![vec![true; 3], vec![true, true, false]];
        let request = additional_commitment_request(
            DispatchInput {
                n_periods: 2,
                ..DispatchInput::default()
            },
            da_commitment.clone(),
            IndexedCommitmentOptions {
                n_cost_segments: 5,
                ..IndexedCommitmentOptions::default()
            },
        );
        let new_sol = solve_dispatch(&net, &request).unwrap();
        let normalized = request
            .resolve_with_options(&net, &crate::request::DispatchSolveOptions::default())
            .expect("normalize additional-commitment request");
        let old_options = legacy_options_from_normalized_request(
            &normalized,
            IndexedDispatchInitialState::default(),
        );
        let old_sol = crate::scuc::solve::solve_scuc_with_problem_spec(
            &net,
            DispatchProblemSpec::from_options(&old_options),
        )
        .unwrap();

        assert!(
            (new_sol.summary.total_cost - old_sol.summary.total_cost).abs() < 1e-6,
            "additional commitment cost mismatch: new={} old={}",
            new_sol.summary.total_cost,
            old_sol.summary.total_cost
        );
        assert_eq!(new_sol.commitment, old_sol.commitment);
        assert_eq!(new_sol.startup, old_sol.startup);
        assert_eq!(new_sol.shutdown, old_sol.shutdown);
        for (t, committed) in new_sol
            .commitment
            .as_ref()
            .expect("canonical additional commitment should expose commitment schedule")
            .iter()
            .enumerate()
        {
            for (gi, forced_on) in da_commitment[t].iter().copied().enumerate() {
                if forced_on {
                    assert!(
                        committed[gi],
                        "period {t}: generator {gi} should remain committed by DA schedule"
                    );
                }
            }
        }
    }

    /// Verify canonical SCUC exposes pricing convergence and CO2 shadow prices.
    #[test]
    fn test_dispatch_dc_scuc_exposes_pricing_and_co2_metadata() {
        let net = two_gen_carbon_test_network();

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 1,
                    enforce_thermal_limits: false,
                    carbon_price: Some(CarbonPrice::new(50.0)),
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        assert_eq!(sol.study.periods, 1);
        assert_eq!(sol.periods.len(), 1);
        assert_eq!(sol.co2_shadow_price.len(), 2);
        assert_eq!(sol.diagnostics.pricing_converged, Some(true));
        assert!(
            (sol.co2_shadow_price[0] - 25.0).abs() < 0.1,
            "dirty unit CO2 shadow price should be ~25 $/MWh, got {}",
            sol.co2_shadow_price[0]
        );
        assert!(
            sol.co2_shadow_price[1].abs() < 1e-9,
            "clean unit CO2 shadow price should be ~0, got {}",
            sol.co2_shadow_price[1]
        );
        assert!(
            sol.periods[0].pg_mw[0] > sol.periods[0].pg_mw[1],
            "dirty generator should still dominate at $50/t because 10 + 25 < 40: dirty={} clean={}",
            sol.periods[0].pg_mw[0],
            sol.periods[0].pg_mw[1]
        );
    }

    /// Verify canonical SCUC routing exposes reserve awards on the public result.
    #[test]
    fn test_dispatch_dc_scuc_exposes_reserve_awards() {
        let net = one_bus_reserve_test_network();
        let reserve_product = surge_network::market::ReserveProduct {
            id: "spin".into(),
            name: "Spinning Reserve".into(),
            direction: surge_network::market::ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: surge_network::market::QualificationRule::Committed,
            energy_coupling: surge_network::market::EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: surge_network::market::PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        };

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    reserve_products: vec![reserve_product],
                    system_reserve_requirements: vec![
                        surge_network::market::SystemReserveRequirement {
                            product_id: "spin".into(),
                            requirement_mw: 30.0,
                            per_period_mw: None,
                        },
                    ],
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        assert_eq!(sol.study.periods, 2);
        for t in 0..2 {
            let spin_awards = sol.periods[t]
                .reserve_awards
                .get("spin")
                .expect("spin awards should be present on canonical result");
            let total_reserve: f64 = spin_awards.iter().sum();
            assert!(
                total_reserve >= 29.9,
                "period {t}: reserve={total_reserve:.1} MW, required 30 MW"
            );
        }
    }

    #[test]
    fn test_dispatch_dc_scuc_exposes_reserve_awards_without_pricing() {
        let net = one_bus_reserve_test_network();
        let reserve_product = surge_network::market::ReserveProduct {
            id: "spin".into(),
            name: "Spinning Reserve".into(),
            direction: surge_network::market::ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: surge_network::market::QualificationRule::Committed,
            energy_coupling: surge_network::market::EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: surge_network::market::PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        };

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    period_hours: vec![1.0, 1.0],
                    run_pricing: false,
                    reserve_products: vec![reserve_product],
                    system_reserve_requirements: vec![
                        surge_network::market::SystemReserveRequirement {
                            product_id: "spin".into(),
                            requirement_mw: 30.0,
                            per_period_mw: None,
                        },
                    ],
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        assert_eq!(sol.study.periods, 2);
        for t in 0..2 {
            let spin_awards = sol.periods[t]
                .reserve_awards
                .get("spin")
                .expect("spin awards should be present on canonical result even without repricing");
            let total_reserve: f64 = spin_awards.iter().sum();
            assert!(
                total_reserve >= 29.9,
                "period {t}: reserve={total_reserve:.1} MW, required 30 MW"
            );
        }
    }

    #[test]
    fn test_dispatch_dc_scuc_fixed_commitment_pricing_preserves_reserve_clearing() {
        let net = one_bus_reserve_test_network();
        let reserve_product = surge_network::market::ReserveProduct {
            id: "spin".into(),
            name: "Spinning Reserve".into(),
            direction: surge_network::market::ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: surge_network::market::QualificationRule::Committed,
            energy_coupling: surge_network::market::EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: surge_network::market::PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        };

        let sol = solve_dispatch(
            &net,
            &time_coupled_fixed_request(
                DispatchInput {
                    n_periods: 2,
                    reserve_products: vec![reserve_product],
                    system_reserve_requirements: vec![
                        surge_network::market::SystemReserveRequirement {
                            product_id: "spin".into(),
                            requirement_mw: 30.0,
                            per_period_mw: None,
                        },
                    ],
                    ..DispatchInput::default()
                },
                fixed_schedule(vec![true, true], None),
            ),
        )
        .unwrap();

        assert_eq!(sol.study.periods, 2);
        assert_eq!(sol.diagnostics.pricing_converged, Some(true));
        for t in 0..2 {
            let spin_awards = sol.periods[t]
                .reserve_awards
                .get("spin")
                .expect("spin awards should be preserved through fixed-commitment repricing");
            let total_reserve: f64 = spin_awards.iter().sum();
            assert!(
                total_reserve >= 29.9,
                "period {t}: repriced fixed-commitment reserve={total_reserve:.1} MW, required 30 MW"
            );
            let spin_result = sol.periods[t]
                .reserve_results
                .iter()
                .find(|reserve| {
                    reserve.scope == crate::result::ReserveScope::System
                        && reserve.product_id == "spin"
                })
                .expect("spin reserve result should be present");
            assert!(
                spin_result.provided_mw >= 29.9,
                "period {t}: repriced fixed-commitment provided reserve should stay nonzero, got {:.1}",
                spin_result.provided_mw
            );
        }
    }

    #[test]
    fn test_dispatch_reserve_balance_products_allow_cumulative_substitution() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveOffer,
            ReserveProduct, SystemReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        for generator in &mut net.generators {
            generator
                .market
                .get_or_insert_default()
                .reserve_offers
                .clear();
        }
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "reg_up".into(),
                capacity_mw: 100.0,
                cost_per_mwh: 0.0,
            });

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![
                    ReserveProduct {
                        id: "reg_up".into(),
                        name: "Reg Up".into(),
                        direction: ReserveDirection::Up,
                        deploy_secs: 300.0,
                        qualification: QualificationRule::Committed,
                        energy_coupling: EnergyCoupling::Headroom,
                        dispatchable_load_energy_coupling: None,
                        shared_limit_products: Vec::new(),
                        balance_products: Vec::new(),
                        kind: surge_network::market::ReserveKind::Real,
                        apply_deploy_ramp_limit: true,
                        demand_curve: PenaltyCurve::Linear {
                            cost_per_unit: 1000.0,
                        },
                    },
                    ReserveProduct {
                        id: "syn".into(),
                        name: "Synchronized Reserve".into(),
                        direction: ReserveDirection::Up,
                        deploy_secs: 600.0,
                        qualification: QualificationRule::Synchronized,
                        energy_coupling: EnergyCoupling::Headroom,
                        dispatchable_load_energy_coupling: None,
                        shared_limit_products: Vec::new(),
                        balance_products: vec!["reg_up".into()],
                        kind: surge_network::market::ReserveKind::Real,
                        apply_deploy_ramp_limit: true,
                        demand_curve: PenaltyCurve::Linear {
                            cost_per_unit: 1000.0,
                        },
                    },
                ],
                system_reserve_requirements: vec![
                    SystemReserveRequirement {
                        product_id: "reg_up".into(),
                        requirement_mw: 50.0,
                        per_period_mw: None,
                    },
                    SystemReserveRequirement {
                        product_id: "syn".into(),
                        requirement_mw: 50.0,
                        per_period_mw: None,
                    },
                ],
                ..DispatchInput::default()
            }),
        )
        .expect("dispatch with cumulative reserve balance should solve");

        let reg_up = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::System
                    && reserve.product_id == "reg_up"
            })
            .expect("expected reg_up system reserve result");
        assert_eq!(reg_up.requirement_mw, 50.0);
        assert_eq!(reg_up.provided_mw, 100.0);
        assert_eq!(reg_up.shortfall_mw, 0.0);

        let syn = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::System && reserve.product_id == "syn"
            })
            .expect("expected syn system reserve result");
        assert_eq!(syn.requirement_mw, 100.0);
        assert_eq!(syn.provided_mw, 100.0);
        assert_eq!(syn.shortfall_mw, 0.0);
    }

    #[test]
    fn test_dispatch_dc_scuc_dispatchable_load_down_reserve_uses_period_schedule_pmax() {
        use std::collections::HashMap;

        use surge_network::market::{
            DispatchableLoad, DlOfferSchedule, DlPeriodParams, EnergyCoupling, LoadCostModel,
            PenaltyCurve, QualificationRule, ReserveDirection, ReserveOffer, ReserveProduct,
            SystemReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        for generator in &mut net.generators {
            generator
                .market
                .get_or_insert_default()
                .reserve_offers
                .clear();
        }

        let mut dl = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 0.0, net.base_mva);
        dl.reserve_offers.push(ReserveOffer {
            product_id: "reg_down".into(),
            capacity_mw: 50.0,
            cost_per_mwh: 0.0,
        });

        let request = scuc_request(
            DispatchInput {
                n_periods: 2,
                period_hours: vec![1.0, 1.0],
                dispatchable_loads: vec![dl],
                dl_offer_schedules: HashMap::from([(
                    0usize,
                    DlOfferSchedule {
                        periods: vec![
                            Some(DlPeriodParams {
                                p_sched_pu: 0.5,
                                p_max_pu: 0.5,
                                q_sched_pu: Some(0.0),
                                q_min_pu: Some(0.0),
                                q_max_pu: Some(0.0),
                                pq_linear_equality: None,
                                pq_linear_upper: None,
                                pq_linear_lower: None,
                                cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 0.0 },
                            }),
                            Some(DlPeriodParams {
                                p_sched_pu: 0.1,
                                p_max_pu: 0.1,
                                q_sched_pu: Some(0.0),
                                q_min_pu: Some(0.0),
                                q_max_pu: Some(0.0),
                                pq_linear_equality: None,
                                pq_linear_upper: None,
                                pq_linear_lower: None,
                                cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 0.0 },
                            }),
                        ],
                    },
                )]),
                reserve_products: vec![ReserveProduct {
                    id: "reg_down".into(),
                    name: "Reg Down".into(),
                    direction: ReserveDirection::Down,
                    deploy_secs: 300.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Footroom,
                    dispatchable_load_energy_coupling: Some(EnergyCoupling::Headroom),
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                system_reserve_requirements: vec![SystemReserveRequirement {
                    product_id: "reg_down".into(),
                    requirement_mw: 20.0,
                    per_period_mw: None,
                }],
                ..DispatchInput::default()
            },
            IndexedCommitmentOptions::default(),
        );
        let result = solve_dispatch(&net, &request)
            .expect("SCUC with period-varying dispatchable-load reserve bounds should solve");

        let period0 = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::System
                    && reserve.product_id == "reg_down"
            })
            .expect("expected period-0 reg_down system reserve result");
        assert!(period0.provided_mw >= 20.0);
        assert_eq!(period0.shortfall_mw, 0.0);

        let period1 = result.periods[1]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::System
                    && reserve.product_id == "reg_down"
            })
            .expect("expected period-1 reg_down system reserve result");
        assert_eq!(period1.provided_mw, 10.0);
        assert_eq!(period1.shortfall_mw, 10.0);
    }

    #[test]
    fn test_dispatch_dc_sced_exposes_reserve_shortfall() {
        let net = one_bus_reserve_test_network();
        let reserve_product = surge_network::market::ReserveProduct {
            id: "spin".into(),
            name: "Spinning Reserve".into(),
            direction: surge_network::market::ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: surge_network::market::QualificationRule::Committed,
            energy_coupling: surge_network::market::EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: surge_network::market::PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        };

        let sol = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![reserve_product],
                system_reserve_requirements: vec![
                    surge_network::market::SystemReserveRequirement {
                        product_id: "spin".into(),
                        requirement_mw: 200.0,
                        per_period_mw: None,
                    },
                ],
                zonal_reserve_requirements: vec![surge_network::market::ZonalReserveRequirement {
                    zone_id: 1,
                    product_id: "spin".into(),
                    requirement_mw: 200.0,
                    per_period_mw: None,
                    shortfall_cost_per_unit: None,
                    served_dispatchable_load_coefficient: None,
                    largest_generator_dispatch_coefficient: None,
                    participant_bus_numbers: None,
                }],
                generator_area: vec![1, 1],
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let period = &sol.periods[0];
        let system_shortfall = period.reserve_shortfall.get("spin").copied().unwrap_or(0.0);
        let zonal_shortfall = period
            .zonal_reserve_shortfall
            .get("1:spin")
            .copied()
            .unwrap_or(0.0);

        assert!(
            (system_shortfall - 120.0).abs() < 1e-6,
            "expected 120 MW system shortfall, got {system_shortfall}"
        );
        assert!(
            (zonal_shortfall - 120.0).abs() < 1e-6,
            "expected 120 MW zonal shortfall, got {zonal_shortfall}"
        );
    }

    #[test]
    fn test_dispatch_dc_scuc_exposes_reserve_shortfall() {
        let net = one_bus_reserve_test_network();
        let reserve_product = surge_network::market::ReserveProduct {
            id: "spin".into(),
            name: "Spinning Reserve".into(),
            direction: surge_network::market::ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: surge_network::market::QualificationRule::Committed,
            energy_coupling: surge_network::market::EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: surge_network::market::PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        };

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    reserve_products: vec![reserve_product],
                    system_reserve_requirements: vec![
                        surge_network::market::SystemReserveRequirement {
                            product_id: "spin".into(),
                            requirement_mw: 200.0,
                            per_period_mw: None,
                        },
                    ],
                    zonal_reserve_requirements: vec![
                        surge_network::market::ZonalReserveRequirement {
                            zone_id: 1,
                            product_id: "spin".into(),
                            requirement_mw: 200.0,
                            per_period_mw: None,
                            shortfall_cost_per_unit: None,
                            served_dispatchable_load_coefficient: None,
                            largest_generator_dispatch_coefficient: None,
                            participant_bus_numbers: None,
                        },
                    ],
                    generator_area: vec![1, 1],
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        for (t, period) in sol.periods.iter().enumerate() {
            let system_shortfall = period.reserve_shortfall.get("spin").copied().unwrap_or(0.0);
            let zonal_shortfall = period
                .zonal_reserve_shortfall
                .get("1:spin")
                .copied()
                .unwrap_or(0.0);
            assert!(
                (system_shortfall - 150.0).abs() < 1e-6,
                "period {t}: expected 150 MW system shortfall, got {system_shortfall}"
            );
            assert!(
                (zonal_shortfall - 150.0).abs() < 1e-6,
                "period {t}: expected 150 MW zonal shortfall, got {zonal_shortfall}"
            );
        }
    }

    /// Verify canonical SCUC routing exposes flowgate shadow prices on the public result.
    #[test]
    fn test_dispatch_dc_scuc_exposes_flowgate_shadow_prices() {
        let mut net = three_bus_flowgate_test_network();
        net.flowgates.push(surge_network::network::Flowgate {
            name: "FG_12_tight".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 50.0,
            limit_reverse_mw: 0.0,
            in_service: true,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });
        net.flowgates.push(surge_network::network::Flowgate {
            name: "FG_12_slack".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 9999.0,
            limit_reverse_mw: 0.0,
            in_service: true,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    enforce_thermal_limits: false,
                    enforce_flowgates: true,
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        for t in 0..2 {
            assert_eq!(sol.periods[t].flowgate_shadow_prices.len(), 2);
            assert!(
                sol.periods[t].flowgate_shadow_prices[0] > 1e-4,
                "period {t}: binding flowgate should have positive shadow price, got {}",
                sol.periods[t].flowgate_shadow_prices[0]
            );
            assert!(
                sol.periods[t].flowgate_shadow_prices[1].abs() < 1e-4,
                "period {t}: slack flowgate should have near-zero shadow price, got {}",
                sol.periods[t].flowgate_shadow_prices[1]
            );
        }
    }

    /// Verify canonical SCUC routing exposes branch thermal shadow prices on the public result.
    #[test]
    fn test_dispatch_dc_scuc_exposes_branch_shadow_prices() {
        let mut net = three_bus_flowgate_test_network();
        net.branches[0].rating_a_mva = 50.0;

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    enforce_thermal_limits: true,
                    enforce_flowgates: false,
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        for t in 0..2 {
            assert_eq!(sol.periods[t].branch_shadow_prices.len(), 2);
            assert!(
                sol.periods[t].branch_shadow_prices[0] > 1e-4,
                "period {t}: binding branch should have positive shadow price, got {}",
                sol.periods[t].branch_shadow_prices[0]
            );
            assert!(
                sol.periods[t].branch_shadow_prices[1].abs() < 1e-4,
                "period {t}: slack branch should have near-zero shadow price, got {}",
                sol.periods[t].branch_shadow_prices[1]
            );
            assert!(
                sol.periods[t].constraint_results.iter().any(|constraint| {
                    constraint.kind == crate::result::ConstraintKind::BranchThermal
                        && constraint.shadow_price.unwrap_or(0.0).abs() > 1e-4
                }),
                "period {t}: branch thermal shadow price should be promoted into constraint_results"
            );
        }
    }

    /// Verify canonical SCUC routing exposes interface shadow prices on the public result.
    #[test]
    fn test_dispatch_dc_scuc_exposes_interface_shadow_prices() {
        let mut net = three_bus_flowgate_test_network();
        net.interfaces.push(surge_network::network::Interface {
            name: "IF_12".to_string(),
            members: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            limit_forward_mw: 50.0,
            limit_reverse_mw: 50.0,
            in_service: true,
            limit_forward_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
        });

        let sol = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    enforce_thermal_limits: false,
                    enforce_flowgates: true,
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        for t in 0..2 {
            assert_eq!(sol.periods[t].interface_shadow_prices.len(), 1);
            assert!(
                sol.periods[t].interface_shadow_prices[0] > 1e-4,
                "period {t}: binding interface should have positive shadow price, got {}",
                sol.periods[t].interface_shadow_prices[0]
            );
        }

        let sol_without_flowgates = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    enforce_thermal_limits: false,
                    enforce_flowgates: false,
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();
        assert!(
            sol_without_flowgates.periods[0]
                .interface_shadow_prices
                .is_empty(),
            "interface shadow prices should be empty when enforce_flowgates=false"
        );
    }

    /// Issue 2: Sequential multi-period SCED must thread storage SoC between
    /// periods.  Without the fix, every period starts from the same initial SoC.
    #[test]
    fn test_sequential_sced_threads_storage_soc() {
        use surge_network::Network;
        use surge_network::market::{CostCurve, LoadProfile, LoadProfiles};
        use surge_network::network::{
            Bus, BusType, Generator, Load, StorageDispatchMode, StorageParams,
        };

        let mut net = Network::new("soc_thread_test");
        net.base_mva = 100.0;
        let b = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b);
        net.loads.push(Load::new(1, 100.0, 0.0));

        // Fixed 100 MW must-run generator
        let mut g0 = Generator::new(1, 100.0, 1.0);
        g0.pmin = 100.0;
        g0.pmax = 100.0;
        g0.in_service = true;
        g0.commitment.get_or_insert_default().status =
            surge_network::network::CommitmentStatus::MustRun;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        // Battery: 50 MW / 200 MWh, eta=1.0, initial SoC = 100 MWh
        let bess = Generator {
            bus: 1,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            storage: Some(StorageParams {
                charge_efficiency: 1.0,
                discharge_efficiency: 1.0,
                energy_capacity_mwh: 200.0,
                soc_initial_mwh: 100.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 200.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        };
        net.generators.push(bess);

        // 4 periods: load = [80, 80, 80, 80] MW with gen fixed at 100 MW.
        // Excess 20 MW each period → battery should charge 20 MW/period.
        // SoC should progress: 120, 140, 160, 180.
        // Without SoC threading, all periods would produce SoC = 120 (restarting from 100).
        let sol = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                n_periods: 4,
                dt_hours: 1.0,
                enforce_thermal_limits: false,
                load_profiles: LoadProfiles {
                    profiles: vec![LoadProfile {
                        bus: 1,
                        load_mw: vec![80.0, 80.0, 80.0, 80.0],
                    }],
                    n_timesteps: 4,
                },
                ..DispatchInput::default()
            }),
        )
        .unwrap();
        assert_eq!(sol.periods.len(), 4);

        let expected_soc = [120.0, 140.0, 160.0, 180.0];
        for (t, expected) in expected_soc.iter().enumerate() {
            let soc = sol.periods[t].storage_soc_mwh[0];
            assert!(
                (soc - expected).abs() < 1e-6,
                "period {t}: expected SoC {expected:.1}, got {soc:.6}",
            );
        }
    }

    #[test]
    fn test_solve_dispatch_rejects_unknown_generator_targeted_profile_id() {
        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            n_periods: 1,
            renewable_profiles: surge_network::market::RenewableProfiles {
                profiles: vec![surge_network::market::RenewableProfile {
                    generator_id: "missing-generator".to_string(),
                    capacity_factors: vec![0.5],
                }],
                n_timesteps: 1,
            },
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request).unwrap_err();
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("unknown generator_id missing-generator")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_duplicate_generator_targeted_profiles() {
        let mut network = one_bus_reserve_test_network();
        network.canonicalize_generator_ids();
        let generator_id = network.generators[0].id.clone();
        let request = period_by_period_request(DispatchInput {
            n_periods: 1,
            renewable_profiles: surge_network::market::RenewableProfiles {
                profiles: vec![
                    surge_network::market::RenewableProfile {
                        generator_id: generator_id.clone(),
                        capacity_factors: vec![0.8],
                    },
                    surge_network::market::RenewableProfile {
                        generator_id,
                        capacity_factors: vec![0.7],
                    },
                ],
                n_timesteps: 1,
            },
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request).unwrap_err();
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("duplicate renewable profile")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_short_profile_vectors() {
        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            n_periods: 2,
            load_profiles: surge_network::market::LoadProfiles {
                profiles: vec![surge_network::market::LoadProfile {
                    bus: 1,
                    load_mw: vec![50.0],
                }],
                n_timesteps: 2,
            },
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request).expect_err("expected invalid load profile");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("load profile for bus 1 length 1 does not match n_periods 2")
        ));
    }

    #[test]
    fn test_apply_profiles_preserves_reactive_demand_for_active_power_profile() {
        use surge_network::market::{LoadProfile, LoadProfiles};
        use surge_network::network::Load;

        let mut network = one_bus_reserve_test_network();
        network.loads = vec![Load::new(1, 60.0, 30.0), Load::new(1, 40.0, 10.0)];
        let input = DispatchInput {
            n_periods: 1,
            load_profiles: LoadProfiles {
                profiles: vec![LoadProfile {
                    bus: 1,
                    load_mw: vec![50.0],
                }],
                n_timesteps: 1,
            },
            ..DispatchInput::default()
        };
        let commitment = CommitmentMode::AllCommitted;
        let spec = DispatchProblemSpec::from_request(&input, &commitment);

        let profiled = apply_profiles(&network, &spec, 0);
        let total_p: f64 = profiled
            .loads
            .iter()
            .map(|load| load.active_power_demand_mw)
            .sum();
        let total_q: f64 = profiled
            .loads
            .iter()
            .map(|load| load.reactive_power_demand_mvar)
            .sum();

        assert!(
            (total_p - 50.0).abs() < 1e-9,
            "expected 50 MW, got {total_p}"
        );
        assert!(
            (total_q - 20.0).abs() < 1e-9,
            "expected 20 MVAr, got {total_q}"
        );
    }

    #[test]
    fn test_apply_profiles_allows_explicit_reactive_override() {
        use surge_network::network::Load;

        let mut network = one_bus_reserve_test_network();
        network.loads = vec![Load::new(1, 60.0, 30.0), Load::new(1, 40.0, 10.0)];
        let input = DispatchInput {
            n_periods: 1,
            ac_bus_load_profiles: crate::request::AcBusLoadProfiles {
                profiles: vec![crate::request::AcBusLoadProfile {
                    bus_number: 1,
                    p_mw: None,
                    q_mvar: Some(vec![12.0]),
                }],
            },
            ..DispatchInput::default()
        };
        let commitment = CommitmentMode::AllCommitted;
        let spec = DispatchProblemSpec::from_request(&input, &commitment);

        let profiled = apply_profiles(&network, &spec, 0);
        let total_p: f64 = profiled
            .loads
            .iter()
            .map(|load| load.active_power_demand_mw)
            .sum();
        let total_q: f64 = profiled
            .loads
            .iter()
            .map(|load| load.reactive_power_demand_mvar)
            .sum();

        assert!(
            (total_p - 100.0).abs() < 1e-9,
            "expected 100 MW, got {total_p}"
        );
        assert!(
            (total_q - 12.0).abs() < 1e-9,
            "expected 12 MVAr, got {total_q}"
        );
    }

    #[test]
    fn test_apply_profiles_preserves_existing_load_records() {
        use surge_network::network::{Load, LoadClass, LoadConnection};

        let mut network = one_bus_reserve_test_network();
        let mut load0 = Load::new(1, 60.0, 30.0);
        load0.id = "L0".to_string();
        load0.zip_p_current_frac = 0.2;
        load0.freq_sensitivity_p_pct_per_hz = 1.5;
        load0.load_class = Some(LoadClass::Industrial);
        load0.connection = LoadConnection::Delta;
        load0.shedding_priority = Some(2);
        load0.conforming = false;

        let mut load1 = Load::new(1, 40.0, 10.0);
        load1.id = "L1".to_string();
        load1.zip_q_current_frac = 0.3;
        load1.freq_sensitivity_q_pct_per_hz = 0.7;
        load1.load_class = Some(LoadClass::Commercial);
        load1.connection = LoadConnection::WyeUngrounded;

        let mut outaged = Load::new(1, 5.0, 2.0);
        outaged.id = "OUT".to_string();
        outaged.in_service = false;

        network.loads = vec![load0.clone(), load1.clone(), outaged.clone()];
        let input = DispatchInput {
            n_periods: 1,
            load_profiles: surge_network::market::LoadProfiles {
                profiles: vec![surge_network::market::LoadProfile {
                    bus: 1,
                    load_mw: vec![50.0],
                }],
                n_timesteps: 1,
            },
            ..DispatchInput::default()
        };
        let commitment = CommitmentMode::AllCommitted;
        let spec = DispatchProblemSpec::from_request(&input, &commitment);

        let profiled = apply_profiles(&network, &spec, 0);

        assert_eq!(
            profiled.loads.len(),
            3,
            "profiled case should preserve load rows"
        );
        assert_eq!(profiled.loads[0].id, "L0");
        assert_eq!(
            profiled.loads[0].zip_p_current_frac,
            load0.zip_p_current_frac
        );
        assert_eq!(
            profiled.loads[0].freq_sensitivity_p_pct_per_hz,
            load0.freq_sensitivity_p_pct_per_hz
        );
        assert_eq!(profiled.loads[0].load_class, load0.load_class);
        assert_eq!(profiled.loads[0].connection, load0.connection);
        assert_eq!(profiled.loads[0].shedding_priority, load0.shedding_priority);
        assert_eq!(profiled.loads[0].conforming, load0.conforming);

        assert_eq!(profiled.loads[1].id, "L1");
        assert_eq!(
            profiled.loads[1].zip_q_current_frac,
            load1.zip_q_current_frac
        );
        assert_eq!(
            profiled.loads[1].freq_sensitivity_q_pct_per_hz,
            load1.freq_sensitivity_q_pct_per_hz
        );
        assert_eq!(profiled.loads[1].load_class, load1.load_class);
        assert_eq!(profiled.loads[1].connection, load1.connection);

        assert_eq!(profiled.loads[2].id, "OUT");
        assert!(!profiled.loads[2].in_service);
        assert_eq!(
            profiled.loads[2].active_power_demand_mw,
            outaged.active_power_demand_mw
        );
        assert_eq!(
            profiled.loads[2].reactive_power_demand_mvar,
            outaged.reactive_power_demand_mvar
        );

        let total_p: f64 = profiled
            .loads
            .iter()
            .filter(|load| load.in_service)
            .map(|load| load.active_power_demand_mw)
            .sum();
        let total_q: f64 = profiled
            .loads
            .iter()
            .filter(|load| load.in_service)
            .map(|load| load.reactive_power_demand_mvar)
            .sum();

        assert!(
            (total_p - 50.0).abs() < 1e-9,
            "expected 50 MW, got {total_p}"
        );
        assert!(
            (total_q - 20.0).abs() < 1e-9,
            "expected 20 MVAr, got {total_q}"
        );
    }

    #[test]
    fn test_solve_dispatch_rejects_ac_bus_load_profiles_for_dc_request() {
        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            n_periods: 1,
            ac_bus_load_profiles: crate::request::AcBusLoadProfiles {
                profiles: vec![crate::request::AcBusLoadProfile {
                    bus_number: 1,
                    p_mw: Some(vec![70.0]),
                    q_mvar: Some(vec![15.0]),
                }],
            },
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request).expect_err("expected invalid AC load input");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("AC bus load profiles are only valid for AC dispatch")
        ));
    }

    #[test]
    fn test_apply_profiles_sets_generator_dispatch_bounds() {
        let mut network = one_bus_reserve_test_network();
        network.generators[0].id = "gen_0".to_string();
        network.generators[0].pmin = 0.0;
        network.generators[0].pmax = 100.0;
        network.generators[0].p = 75.0;

        let input = DispatchInput {
            n_periods: 2,
            generator_dispatch_bounds: crate::request::GeneratorDispatchBoundsProfiles {
                profiles: vec![crate::request::GeneratorDispatchBoundsProfile {
                    resource_id: "gen_0".to_string(),
                    p_min_mw: vec![10.0, 40.0],
                    p_max_mw: vec![50.0, 60.0],
                    q_min_mvar: Some(vec![-5.0, -10.0]),
                    q_max_mvar: Some(vec![15.0, 20.0]),
                }],
            },
            ..DispatchInput::default()
        };
        let commitment = CommitmentMode::AllCommitted;
        let spec = DispatchProblemSpec::from_request(&input, &commitment);

        let profiled_t0 = apply_profiles(&network, &spec, 0);
        assert!(
            (profiled_t0.generators[0].pmin - 10.0).abs() < 1e-9,
            "expected period-0 pmin=10, got {}",
            profiled_t0.generators[0].pmin
        );
        assert!(
            (profiled_t0.generators[0].pmax - 50.0).abs() < 1e-9,
            "expected period-0 pmax=50, got {}",
            profiled_t0.generators[0].pmax
        );
        assert!(
            (profiled_t0.generators[0].p - 50.0).abs() < 1e-9,
            "expected period-0 dispatch clamp to 50, got {}",
            profiled_t0.generators[0].p
        );
        assert!(
            (profiled_t0.generators[0].qmin + 5.0).abs() < 1e-9,
            "expected period-0 qmin=-5, got {}",
            profiled_t0.generators[0].qmin
        );
        assert!(
            (profiled_t0.generators[0].qmax - 15.0).abs() < 1e-9,
            "expected period-0 qmax=15, got {}",
            profiled_t0.generators[0].qmax
        );

        let profiled_t1 = apply_profiles(&network, &spec, 1);
        assert!(
            (profiled_t1.generators[0].pmin - 40.0).abs() < 1e-9,
            "expected period-1 pmin=40, got {}",
            profiled_t1.generators[0].pmin
        );
        assert!(
            (profiled_t1.generators[0].pmax - 60.0).abs() < 1e-9,
            "expected period-1 pmax=60, got {}",
            profiled_t1.generators[0].pmax
        );
        assert!(
            (profiled_t1.generators[0].p - 60.0).abs() < 1e-9,
            "expected period-1 dispatch clamp to 60, got {}",
            profiled_t1.generators[0].p
        );
        assert!(
            (profiled_t1.generators[0].qmin + 10.0).abs() < 1e-9,
            "expected period-1 qmin=-10, got {}",
            profiled_t1.generators[0].qmin
        );
        assert!(
            (profiled_t1.generators[0].qmax - 20.0).abs() < 1e-9,
            "expected period-1 qmax=20, got {}",
            profiled_t1.generators[0].qmax
        );
    }

    #[test]
    fn test_solve_dispatch_rejects_invalid_security_contingency_index() {
        let network = one_bus_reserve_test_network();
        let request = DispatchRequest {
            formulation: crate::Formulation::Dc,
            coupling: crate::request::IntervalCoupling::TimeCoupled,
            network: crate::request::DispatchNetwork {
                security: Some(crate::request::SecurityPolicy {
                    branch_contingencies: vec![crate::request::BranchRef {
                        from_bus: 99,
                        to_bus: 100,
                        circuit: "1".to_string(),
                    }],
                    ..Default::default()
                }),
                ..crate::request::DispatchNetwork::default()
            },
            ..DispatchRequest::default()
        };

        let error =
            solve_dispatch(&network, &request).expect_err("expected invalid security input");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("security.branch_contingencies references unknown branch (99, 100, 1)")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_invalid_pumped_hydro_head_curve() {
        let network = one_bus_storage_test_network();
        let request = period_by_period_request(DispatchInput {
            ph_head_curves: vec![IndexedPhHeadCurve {
                gen_index: 0,
                breakpoints: vec![(0.0, 10.0)],
            }],
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request).expect_err("expected invalid head curve");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("ph_head_curves for generator index 0 requires at least two breakpoints")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_mismatched_prev_dispatch_length() {
        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            initial_state: IndexedDispatchInitialState {
                prev_dispatch_mw: Some(vec![10.0, 20.0, 30.0]),
                ..IndexedDispatchInitialState::default()
            },
            ..DispatchInput::default()
        });

        let error =
            solve_dispatch(&network, &request).expect_err("expected invalid prev dispatch input");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains(
                    "previous_resource_dispatch references unknown dispatch resource __gen_local:2"
                )
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_unknown_reserve_requirement_product() {
        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            system_reserve_requirements: vec![surge_network::market::SystemReserveRequirement {
                product_id: "ghost".to_string(),
                requirement_mw: 10.0,
                per_period_mw: None,
            }],
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request)
            .expect_err("expected invalid reserve requirement product");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("system_reserve_requirements references unknown product_id ghost")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_duplicate_system_reserve_requirements() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveProduct,
            SystemReserveRequirement,
        };

        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            reserve_products: vec![ReserveProduct {
                id: "spin".to_string(),
                name: "Spinning Reserve".to_string(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            }],
            system_reserve_requirements: vec![
                SystemReserveRequirement {
                    product_id: "spin".to_string(),
                    requirement_mw: 10.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "spin".to_string(),
                    requirement_mw: 20.0,
                    per_period_mw: None,
                },
            ],
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request)
            .expect_err("expected duplicate system reserve requirement to be rejected");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("duplicate system reserve requirement for product spin")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_out_of_range_virtual_bid_period() {
        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            n_periods: 2,
            virtual_bids: vec![surge_network::market::VirtualBid {
                position_id: "bid-1".to_string(),
                bus: 1,
                period: 2,
                mw_limit: 5.0,
                price_per_mwh: 20.0,
                direction: surge_network::market::VirtualBidDirection::Inc,
                in_service: true,
            }],
            ..DispatchInput::default()
        });

        let error =
            solve_dispatch(&network, &request).expect_err("expected invalid virtual bid period");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("virtual bid bid-1 period 2 is out of range for n_periods 2")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_invalid_generator_area_length() {
        let network = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            generator_area: vec![1, 2, 3],
            ..DispatchInput::default()
        });

        let error =
            solve_dispatch(&network, &request).expect_err("expected invalid generator_area");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains(
                    "resource_area_assignments references unknown supply resource __gen_local:2"
                )
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_fixed_per_period_storage_off() {
        let network = one_bus_storage_test_network();
        let request = period_by_period_fixed_request(
            DispatchInput::default(),
            fixed_schedule(vec![true], Some(vec![vec![false]])),
        );

        let error = solve_dispatch(&network, &request)
            .expect_err("expected invalid fixed storage commitment override");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("fixed commitment schedule for period 0 cannot turn storage generator 0 off")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_invalid_storage_self_schedule_length() {
        let network = one_bus_storage_test_network();
        let request = period_by_period_request(DispatchInput {
            n_periods: 2,
            storage_self_schedules: Some(HashMap::from([(0, vec![5.0])])),
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request)
            .expect_err("expected invalid storage self-schedule length");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("storage_self_schedules[0] length 1 does not match n_periods 2")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_storage_offer_curve_without_explicit_origin() {
        let mut network = one_bus_storage_test_network();
        let storage = network.generators[0]
            .storage
            .as_mut()
            .expect("expected storage test network to include storage");
        storage.dispatch_mode = surge_network::network::StorageDispatchMode::OfferCurve;
        storage.discharge_offer = Some(vec![(50.0, 2000.0)]);
        storage.charge_bid = Some(vec![(0.0, 0.0), (50.0, 1500.0)]);

        let error = solve_dispatch(
            &network,
            &period_by_period_request(DispatchInput::default()),
        )
        .expect_err("expected invalid storage offer curve to be rejected");

        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("discharge_offer")
                    && message.contains("explicit origin")
        ));
    }

    #[test]
    fn test_solve_dispatch_rejects_unknown_storage_reserve_soc_impact_product() {
        let network = one_bus_storage_test_network();
        let request = period_by_period_request(DispatchInput {
            storage_reserve_soc_impact: HashMap::from([(
                0,
                HashMap::from([(String::from("ghost"), vec![1.0])]),
            )]),
            ..DispatchInput::default()
        });

        let error = solve_dispatch(&network, &request)
            .expect_err("expected invalid storage reserve soc impact product");
        assert!(matches!(
            error,
            ScedError::InvalidInput(message)
                if message.contains("storage_reserve_soc_impact for generator 0 references unknown reserve product_id ghost")
        ));
    }

    #[test]
    fn test_dispatch_dc_sequential_fixed_commitment_honors_per_period_overrides() {
        let net = two_gen_carbon_test_network();
        let result = solve_dispatch(
            &net,
            &period_by_period_fixed_request(
                DispatchInput {
                    n_periods: 2,
                    ..DispatchInput::default()
                },
                fixed_schedule(
                    vec![true, true],
                    Some(vec![vec![true, true], vec![false, true]]),
                ),
            ),
        )
        .unwrap();

        assert!(
            result.periods[0].pg_mw[0] > 90.0,
            "cheap unit should serve period 0 when committed"
        );
        assert!(
            result.periods[1].pg_mw[0].abs() < 1e-6,
            "period 1 fixed-off unit should not dispatch, got {:.3}",
            result.periods[1].pg_mw[0]
        );
        assert!(
            result.periods[1].pg_mw[1] > 90.0,
            "remaining committed unit should pick up load in period 1, got {:.3}",
            result.periods[1].pg_mw[1]
        );
    }

    #[test]
    fn test_dispatch_dc_sced_enforces_system_reserve_per_period() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveProduct,
            SystemReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                n_periods: 2,
                reserve_products: vec![ReserveProduct {
                    id: "spin".into(),
                    name: "Spinning Reserve".into(),
                    direction: ReserveDirection::Up,
                    deploy_secs: 600.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: None,
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                system_reserve_requirements: vec![SystemReserveRequirement {
                    product_id: "spin".into(),
                    requirement_mw: 0.0,
                    per_period_mw: Some(vec![0.0, 50.0]),
                }],
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let reserve_t0 = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::System && reserve.product_id == "spin"
            })
            .expect("period 0 system reserve result");
        let reserve_t1 = result.periods[1]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::System && reserve.product_id == "spin"
            })
            .expect("period 1 system reserve result");

        assert!(
            reserve_t0.shortfall_mw.abs() < 1e-9,
            "period 0 should not enforce reserve when requirement is 0 MW"
        );
        assert!(
            (reserve_t1.shortfall_mw - 50.0).abs() < 1e-9,
            "period 1 should enforce the 50 MW reserve requirement, got {:.3}",
            reserve_t1.shortfall_mw
        );
    }

    #[test]
    fn test_sequential_sced_uses_period_specific_storage_reserve_soc_impact() {
        use surge_network::Network;
        use surge_network::market::{
            CostCurve, EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection,
            ReserveOffer, ReserveProduct, SystemReserveRequirement,
        };
        use surge_network::network::{
            Bus, BusType, CommitmentStatus, Generator, Load, MarketParams, StorageDispatchMode,
            StorageParams,
        };

        let mut net = Network::new("dispatch_soc_reserve_impact_period_test");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 100.0, 0.0));

        let mut must_run = Generator::new(1, 100.0, 1.0);
        must_run.pmin = 100.0;
        must_run.pmax = 100.0;
        must_run.commitment.get_or_insert_default().status = CommitmentStatus::MustRun;
        must_run.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(must_run);

        net.generators.push(Generator {
            bus: 1,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            market: Some(MarketParams {
                reserve_offers: vec![ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: 50.0,
                    cost_per_mwh: 2.0,
                }],
                ..Default::default()
            }),
            storage: Some(StorageParams {
                charge_efficiency: 1.0,
                discharge_efficiency: 1.0,
                energy_capacity_mwh: 20.0,
                soc_initial_mwh: 10.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 20.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        });

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                n_periods: 2,
                reserve_products: vec![ReserveProduct {
                    id: "spin".into(),
                    name: "Spinning Reserve".into(),
                    direction: ReserveDirection::Up,
                    deploy_secs: 600.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: None,
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                system_reserve_requirements: vec![SystemReserveRequirement {
                    product_id: "spin".into(),
                    requirement_mw: 40.0,
                    per_period_mw: None,
                }],
                storage_reserve_soc_impact: HashMap::from([(
                    1,
                    HashMap::from([(String::from("spin"), vec![0.0, 1.0])]),
                )]),
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let award_t0 = result.periods[0]
            .reserve_awards
            .get("spin")
            .and_then(|awards| awards.get(1))
            .copied()
            .unwrap_or(0.0);
        let award_t1 = result.periods[1]
            .reserve_awards
            .get("spin")
            .and_then(|awards| awards.get(1))
            .copied()
            .unwrap_or(0.0);

        assert!(
            award_t0 > 35.0,
            "period 0 should see little/no SoC reserve coupling, got {:.3}",
            award_t0
        );
        assert!(
            award_t1 < 12.0,
            "period 1 should respect the tighter SoC reserve impact, got {:.3}",
            award_t1
        );
    }

    /// Verify DC time-coupled all-committed dispatch works.
    #[test]
    fn test_dispatch_dc_time_coupled_all_committed() {
        if !data_available() {
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

        let sol = solve_dispatch(
            &net,
            &time_coupled_request(DispatchInput {
                n_periods: 4,
                ..DispatchInput::default()
            }),
        )
        .unwrap();
        assert_eq!(sol.study.periods, 4);
        assert!(sol.summary.total_cost > 0.0);
        // Time-coupled should commit all generators (through SCUC with forced commitment)
        assert!(sol.commitment.is_some());
    }

    #[test]
    fn test_dispatch_dc_time_coupled_preserves_lmp_orientation() {
        if !data_available() {
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

        let seq = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                n_periods: 2,
                ..DispatchInput::default()
            }),
        )
        .unwrap();
        let tc = solve_dispatch(
            &net,
            &time_coupled_request(DispatchInput {
                n_periods: 2,
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        for period in 0..2 {
            let seq_lmp = seq.periods[period].lmp[0];
            let tc_lmp = tc.periods[period].lmp[0];
            assert!(
                seq_lmp * tc_lmp > 0.0,
                "period {period} LMP sign should match between sequential ({seq_lmp}) and time-coupled ({tc_lmp})"
            );
            assert!(
                (seq_lmp - tc_lmp).abs() < 1e-6,
                "period {period} LMP should match between sequential ({seq_lmp}) and time-coupled ({tc_lmp})"
            );
        }
    }

    #[test]
    fn test_dispatch_dc_time_coupled_congested_lmp_matches_sequential() {
        let mut net = three_bus_flowgate_test_network();
        net.branches[0].rating_a_mva = 50.0;

        let seq = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                n_periods: 2,
                enforce_thermal_limits: true,
                ..DispatchInput::default()
            }),
        )
        .unwrap();
        let tc = solve_dispatch(
            &net,
            &time_coupled_request(DispatchInput {
                n_periods: 2,
                enforce_thermal_limits: true,
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        for period in 0..2 {
            for (bus_idx, ((seq_lmp, tc_lmp), (seq_mcc, tc_mcc))) in seq.periods[period]
                .lmp
                .iter()
                .zip(tc.periods[period].lmp.iter())
                .zip(
                    seq.periods[period]
                        .lmp_congestion
                        .iter()
                        .zip(tc.periods[period].lmp_congestion.iter()),
                )
                .enumerate()
            {
                assert!(
                    (seq_lmp - tc_lmp).abs() < 1e-6,
                    "period {period} bus {bus_idx} congested LMP should match sequential ({seq_lmp}) and time-coupled ({tc_lmp})"
                );
                assert!(
                    (seq_mcc - tc_mcc).abs() < 1e-6,
                    "period {period} bus {bus_idx} congestion component should match sequential ({seq_mcc}) and time-coupled ({tc_mcc})"
                );
            }
            assert!(
                tc.periods[period].branch_shadow_prices[0] > 1e-4,
                "period {period}: binding branch should still expose a positive shadow price"
            );
        }
    }

    #[test]
    fn test_dispatch_dc_losses_present_in_sequential_and_time_coupled() {
        let net = two_bus_loss_test_network();

        let seq = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                n_periods: 2,
                use_loss_factors: true,
                ..DispatchInput::default()
            }),
        )
        .unwrap();
        let tc = solve_dispatch(
            &net,
            &time_coupled_request(DispatchInput {
                n_periods: 2,
                use_loss_factors: true,
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        for result in [&seq, &tc] {
            for (period_idx, period) in result.periods.iter().enumerate() {
                assert_eq!(
                    period.lmp_loss.len(),
                    net.n_buses(),
                    "period {period_idx}: loss component should have one entry per bus"
                );
                assert!(
                    period.lmp_loss.iter().any(|loss| loss.abs() > 1e-6),
                    "period {period_idx}: expected non-zero LMP loss component, got {:?}",
                    period.lmp_loss
                );
                for bus_idx in 0..net.n_buses() {
                    let sum = period.lmp_energy[bus_idx]
                        + period.lmp_congestion[bus_idx]
                        + period.lmp_loss[bus_idx];
                    assert!(
                        (period.lmp[bus_idx] - sum).abs() < 0.01,
                        "period {period_idx} bus {bus_idx}: LMP decomposition should sum to total"
                    );
                }
            }
        }
    }

    #[test]
    fn test_time_coupled_fixed_resource_costs_reconcile_with_period_total() {
        use surge_network::market::CostCurve;

        let mut net = one_bus_reserve_test_network();
        // Raise load above total pmin (50+50=100) so both-committed dispatch is feasible.
        net.loads[0].active_power_demand_mw = 150.0;
        net.generators[0].cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.25, 20.0, 0.0],
        });

        let request = time_coupled_fixed_request(
            DispatchInput {
                n_periods: 2,
                ..DispatchInput::default()
            },
            fixed_schedule(vec![true; 2], None),
        );
        let result = solve_dispatch(&net, &request).unwrap();

        assert!(result.commitment.is_some());
        for period in &result.periods {
            let detailed_total: f64 = period
                .resource_results
                .iter()
                .map(|resource| {
                    resource.energy_cost.unwrap_or(0.0)
                        + resource.no_load_cost.unwrap_or(0.0)
                        + resource.startup_cost.unwrap_or(0.0)
                        + resource.reserve_costs.values().sum::<f64>()
                })
                .sum();
            assert!((detailed_total - period.total_cost).abs() < 1e-6);
        }
    }

    #[test]
    fn test_keyed_objective_ledger_reconciles_for_dc_dispatch() {
        let net = one_bus_reserve_test_network();
        let result =
            solve_keyed_dispatch(&net, &period_by_period_request(DispatchInput::default()))
                .unwrap();

        let mismatches = result.objective_ledger_mismatches();
        assert!(
            mismatches.is_empty(),
            "expected clean objective ledger, got {mismatches:#?}"
        );
        assert!(
            result
                .periods()
                .iter()
                .all(|period| period.objective_ledger_is_consistent())
        );
        assert!(
            result.periods()[0]
                .resource_results()
                .iter()
                .any(|resource| !resource.objective_terms.is_empty())
        );
    }

    #[test]
    fn test_keyed_objective_ledger_reconciles_for_scuc_dispatch() {
        let net = one_bus_reserve_test_network();
        let result = solve_keyed_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 2,
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .unwrap();

        let mismatches = result.objective_ledger_mismatches();
        assert!(
            mismatches.is_empty(),
            "expected clean SCUC objective ledger, got {mismatches:#?}"
        );
    }

    #[test]
    fn test_keyed_objective_ledger_reconciles_for_ac_dispatch() {
        let mut net = one_bus_reserve_test_network();
        net.generators[1].in_service = false;
        let result = solve_keyed_dispatch(
            &net,
            &ac_period_by_period_request(DispatchInput {
                n_periods: 2,
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let mismatches = result.objective_ledger_mismatches();
        assert!(
            mismatches.is_empty(),
            "expected clean AC objective ledger, got {mismatches:#?}"
        );
        assert_eq!(result.periods().len(), 2);
    }

    #[test]
    fn test_ac_fixed_commitment_dispatch_reconciles_exactly() {
        let mut net = one_bus_reserve_test_network();
        net.generators[1].in_service = false;

        let result = solve_keyed_dispatch(
            &net,
            &ac_period_by_period_fixed_request(
                DispatchInput {
                    n_periods: 1,
                    ac_generator_warm_start_p_mw: HashMap::from([(0usize, vec![50.0])]),
                    ac_target_tracking: crate::request::AcDispatchTargetTracking {
                        generator_p_penalty_per_mw2: 20.0,
                        ..crate::request::AcDispatchTargetTracking::default()
                    },
                    ..DispatchInput::default()
                },
                fixed_schedule(vec![true], None),
            ),
        )
        .unwrap();

        assert!(
            result.objective_ledger_is_consistent(),
            "expected clean AC fixed-commitment ledger, got {:#?}",
            result.objective_ledger_mismatches()
        );
    }

    #[test]
    fn test_period_by_period_power_balance_penalty_respects_piecewise_subhourly_cost() {
        let mut net = one_bus_reserve_test_network();
        net.generators[0].pmin = 0.0;
        net.generators[0].pmax = 0.0;
        net.generators[1].in_service = false;
        net.loads[0].active_power_demand_mw = 80.0;

        let request = period_by_period_request(DispatchInput {
            dt_hours: 0.5,
            power_balance_penalty: crate::request::PowerBalancePenalty {
                curtailment: vec![(50.0, 1_000.0), (f64::MAX, 3_000.0)],
                excess: vec![(f64::MAX, 100.0)],
            },
            ..DispatchInput::default()
        });
        let result = solve_dispatch(&net, &request).unwrap();
        let period = &result.periods[0];
        let expected_cost = (50.0 * 1_000.0 + 30.0 * 3_000.0) * 0.5;

        assert!((period.total_cost - expected_cost).abs() < 1e-6);
        assert!((period.power_balance_violation.curtailment_cost - expected_cost).abs() < 1e-6);
    }

    #[test]
    fn test_reserve_shortfall_is_promoted_into_constraint_results() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveProduct,
            SystemReserveRequirement,
        };

        let net = one_bus_reserve_test_network();
        let request = period_by_period_request(DispatchInput {
            reserve_products: vec![ReserveProduct {
                id: "spin".into(),
                name: "Spinning Reserve".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::Synchronized,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            }],
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 500.0,
                per_period_mw: None,
            }],
            ..DispatchInput::default()
        });
        let result = solve_dispatch(&net, &request).unwrap();
        let period = &result.periods[0];
        let reserve = period
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.product_id == "spin" && reserve.scope == crate::result::ReserveScope::System
            })
            .expect("system reserve result should be present");
        let constraint = period
            .constraint_results
            .iter()
            .find(|constraint| constraint.constraint_id == "reserve:system:spin")
            .expect("reserve shortfall should be promoted into constraint results");

        assert!(reserve.shortfall_mw > 0.0);
        assert_eq!(
            constraint.kind,
            crate::result::ConstraintKind::ReserveRequirement
        );
        assert_eq!(constraint.scope, crate::result::ConstraintScope::System);
        assert_eq!(constraint.slack_mw, Some(reserve.shortfall_mw));
        assert_eq!(constraint.shadow_price, Some(reserve.clearing_price));
    }

    #[test]
    fn test_dispatchable_load_keyed_results_follow_active_input_order() {
        use surge_network::market::{
            DispatchableLoad, EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection,
            ReserveOffer, ReserveProduct, SystemReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        let mut dl0 = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 30.0, net.base_mva);
        dl0.reserve_offers.push(ReserveOffer {
            product_id: "spin".into(),
            capacity_mw: 0.0,
            cost_per_mwh: 11.0,
        });
        let mut dl1 = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 30.0, net.base_mva);
        dl1.in_service = false;
        dl1.reserve_offers.push(ReserveOffer {
            product_id: "spin".into(),
            capacity_mw: 50.0,
            cost_per_mwh: 111.0,
        });
        let mut dl2 = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 30.0, net.base_mva);
        dl2.reserve_offers.push(ReserveOffer {
            product_id: "spin".into(),
            capacity_mw: 50.0,
            cost_per_mwh: 222.0,
        });

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                dispatchable_loads: vec![dl0, dl1, dl2],
                reserve_products: vec![ReserveProduct {
                    id: "spin".into(),
                    name: "Spinning Reserve".into(),
                    direction: ReserveDirection::Up,
                    deploy_secs: 600.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: None,
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                system_reserve_requirements: vec![SystemReserveRequirement {
                    product_id: "spin".into(),
                    requirement_mw: 50.0,
                    per_period_mw: None,
                }],
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let period = &result.periods[0];
        let dl_result = period
            .resource_results
            .iter()
            .find(|resource| resource.resource_id == "dl:1:2")
            .expect("expected active third dispatchable load to appear with stable fallback id");
        assert_eq!(dl_result.reserve_awards.get("spin").copied(), Some(50.0));
        assert_eq!(dl_result.reserve_costs.get("spin").copied(), Some(11100.0));
        assert_eq!(result.summary.total_reserve_cost, 11100.0);
    }

    #[test]
    fn test_dispatchable_load_keyed_result_prefers_exact_zero_over_raw_curtailment_cost() {
        let raw = crate::solution::RawResourcePeriodResult {
            resource_id: "sd_191::blk:03".to_string(),
            power_mw: 0.0,
            energy_cost: Some(9_214.150_248_989_423),
            served_q_mvar: Some(0.0),
            curtailed_mw: Some(92.141_502_489_894_24),
            curtailment_pct: Some(100.0),
            lmp_at_bus: Some(0.0),
            net_curtailment_benefit: Some(-9_214.150_248_989_423),
            ..Default::default()
        };
        let resource = crate::result::DispatchResource {
            resource_id: raw.resource_id.clone(),
            kind: crate::result::DispatchResourceKind::DispatchableLoad,
            bus_number: Some(23),
            machine_id: None,
            name: None,
            source_index: 0,
        };

        let keyed = map_resource_period_result(raw, Some(&resource), &[], false);

        assert_eq!(
            keyed.kind,
            crate::result::DispatchResourceKind::DispatchableLoad
        );
        assert_eq!(keyed.objective_cost, 0.0);
        assert_eq!(keyed.energy_cost, Some(0.0));
        assert!(
            keyed.objective_terms.is_empty(),
            "fully curtailed load should not invent a nonzero exact term"
        );
    }

    #[test]
    fn test_zonal_reserve_results_report_actual_cleared_mw() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveOffer,
            ReserveProduct, ZonalReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "spin".into(),
                capacity_mw: 50.0,
                cost_per_mwh: -5.0,
            });

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![ReserveProduct {
                    id: "spin".into(),
                    name: "Spinning Reserve".into(),
                    direction: ReserveDirection::Up,
                    deploy_secs: 600.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: None,
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                zonal_reserve_requirements: vec![ZonalReserveRequirement {
                    zone_id: 1,
                    product_id: "spin".into(),
                    requirement_mw: 10.0,
                    per_period_mw: None,
                    shortfall_cost_per_unit: None,
                    served_dispatchable_load_coefficient: None,
                    largest_generator_dispatch_coefficient: None,
                    participant_bus_numbers: None,
                }],
                generator_area: vec![1, 1],
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let zonal = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::Zone && reserve.product_id == "spin"
            })
            .expect("expected zonal reserve result");
        assert!(zonal.provided_mw > zonal.requirement_mw);
        assert_eq!(zonal.provided_mw, 50.0);
        assert_eq!(zonal.shortfall_mw, 0.0);
    }

    #[test]
    fn test_zonal_dispatchable_load_reserve_uses_load_area_override() {
        use surge_network::market::{
            DispatchableLoad, EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection,
            ReserveOffer, ReserveProduct, ZonalReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        net.buses[0].area = 0;
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();

        let mut dl = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 30.0, net.base_mva);
        dl.reserve_offers.push(ReserveOffer {
            product_id: "spin".into(),
            capacity_mw: 50.0,
            cost_per_mwh: 0.0,
        });

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                dispatchable_loads: vec![dl],
                reserve_products: vec![ReserveProduct {
                    id: "spin".into(),
                    name: "Spinning Reserve".into(),
                    direction: ReserveDirection::Up,
                    deploy_secs: 600.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: Some(EnergyCoupling::Footroom),
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                zonal_reserve_requirements: vec![ZonalReserveRequirement {
                    zone_id: 1,
                    product_id: "spin".into(),
                    requirement_mw: 50.0,
                    per_period_mw: None,
                    shortfall_cost_per_unit: None,
                    served_dispatchable_load_coefficient: None,
                    largest_generator_dispatch_coefficient: None,
                    participant_bus_numbers: None,
                }],
                generator_area: vec![0, 0],
                load_area: vec![1],
                ..DispatchInput::default()
            }),
        )
        .expect("dispatch with zonal DR reserve should solve");

        let dl_result = result.periods[0]
            .resource_results
            .iter()
            .find(|resource| resource.resource_id == "dl:1:0")
            .expect("dispatchable load resource result should be present");
        assert_eq!(dl_result.reserve_awards.get("spin").copied(), Some(50.0));

        let zonal = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::Zone && reserve.product_id == "spin"
            })
            .expect("expected zonal reserve result");
        assert_eq!(zonal.zone_id, Some(1));
        assert_eq!(zonal.provided_mw, 50.0);
        assert_eq!(zonal.shortfall_mw, 0.0);
    }

    #[test]
    fn test_zonal_reserve_explicit_participant_buses_allow_multi_zone_membership() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveProduct,
            ZonalReserveRequirement,
        };

        let net = one_bus_reserve_test_network();
        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![ReserveProduct {
                    id: "spin".into(),
                    name: "Spinning Reserve".into(),
                    direction: ReserveDirection::Up,
                    deploy_secs: 600.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: None,
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                zonal_reserve_requirements: vec![
                    ZonalReserveRequirement {
                        zone_id: 1,
                        product_id: "spin".into(),
                        requirement_mw: 50.0,
                        per_period_mw: None,
                        shortfall_cost_per_unit: None,
                        served_dispatchable_load_coefficient: None,
                        largest_generator_dispatch_coefficient: None,
                        participant_bus_numbers: Some(vec![1]),
                    },
                    ZonalReserveRequirement {
                        zone_id: 2,
                        product_id: "spin".into(),
                        requirement_mw: 50.0,
                        per_period_mw: None,
                        shortfall_cost_per_unit: None,
                        served_dispatchable_load_coefficient: None,
                        largest_generator_dispatch_coefficient: None,
                        participant_bus_numbers: Some(vec![1]),
                    },
                ],
                generator_area: vec![0, 0],
                ..DispatchInput::default()
            }),
        )
        .expect("dispatch with explicit multi-zone reserve membership should solve");

        let total_spin_award: f64 = result.periods[0]
            .resource_results
            .iter()
            .map(|resource| resource.reserve_awards.get("spin").copied().unwrap_or(0.0))
            .sum();
        assert!(total_spin_award >= 50.0);

        let zonal_results: Vec<_> = result.periods[0]
            .reserve_results
            .iter()
            .filter(|reserve| {
                reserve.scope == crate::result::ReserveScope::Zone && reserve.product_id == "spin"
            })
            .collect();
        assert_eq!(zonal_results.len(), 2);
        assert!(
            zonal_results
                .iter()
                .all(|reserve| reserve.provided_mw == total_spin_award)
        );
        assert!(
            zonal_results
                .iter()
                .all(|reserve| reserve.shortfall_mw == 0.0)
        );
    }

    #[test]
    fn test_zonal_reserve_explicit_participant_buses_require_known_unique_buses() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveProduct,
            ZonalReserveRequirement,
        };

        let reserve_product = ReserveProduct {
            id: "spin".into(),
            name: "Spinning Reserve".into(),
            direction: ReserveDirection::Up,
            deploy_secs: 600.0,
            qualification: QualificationRule::Committed,
            energy_coupling: EnergyCoupling::Headroom,
            dispatchable_load_energy_coupling: None,
            shared_limit_products: Vec::new(),
            balance_products: Vec::new(),
            kind: surge_network::market::ReserveKind::Real,
            apply_deploy_ramp_limit: true,
            demand_curve: PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
        };
        let net = one_bus_reserve_test_network();

        let duplicate_err = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![reserve_product.clone()],
                zonal_reserve_requirements: vec![ZonalReserveRequirement {
                    zone_id: 1,
                    product_id: "spin".into(),
                    requirement_mw: 50.0,
                    per_period_mw: None,
                    shortfall_cost_per_unit: None,
                    served_dispatchable_load_coefficient: None,
                    largest_generator_dispatch_coefficient: None,
                    participant_bus_numbers: Some(vec![1, 1]),
                }],
                generator_area: vec![0, 0],
                ..DispatchInput::default()
            }),
        )
        .expect_err("duplicate explicit participant buses should fail validation");
        assert!(
            duplicate_err
                .to_string()
                .contains("contains duplicate participant bus 1")
        );

        let unknown_bus_err = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![reserve_product],
                zonal_reserve_requirements: vec![ZonalReserveRequirement {
                    zone_id: 1,
                    product_id: "spin".into(),
                    requirement_mw: 50.0,
                    per_period_mw: None,
                    shortfall_cost_per_unit: None,
                    served_dispatchable_load_coefficient: None,
                    largest_generator_dispatch_coefficient: None,
                    participant_bus_numbers: Some(vec![999]),
                }],
                generator_area: vec![0, 0],
                ..DispatchInput::default()
            }),
        )
        .expect_err("unknown explicit participant buses should fail validation");
        assert!(
            unknown_bus_err
                .to_string()
                .contains("references unknown participant bus 999")
        );
    }

    #[test]
    fn test_zonal_requirement_tracks_served_dispatchable_load() {
        use surge_network::market::{
            DispatchableLoad, EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection,
            ReserveOffer, ReserveProduct, ZonalReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        net.buses[0].area = 1;
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();

        let mut dl = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 30.0, net.base_mva);
        dl.reserve_offers.push(ReserveOffer {
            product_id: "reg_up".into(),
            capacity_mw: 50.0,
            cost_per_mwh: 0.0,
        });

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                dispatchable_loads: vec![dl],
                reserve_products: vec![ReserveProduct {
                    id: "reg_up".into(),
                    name: "Reg Up".into(),
                    direction: ReserveDirection::Up,
                    deploy_secs: 300.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: Some(EnergyCoupling::Footroom),
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                zonal_reserve_requirements: vec![ZonalReserveRequirement {
                    zone_id: 1,
                    product_id: "reg_up".into(),
                    requirement_mw: 0.0,
                    per_period_mw: None,
                    shortfall_cost_per_unit: None,
                    served_dispatchable_load_coefficient: Some(1.0),
                    largest_generator_dispatch_coefficient: None,
                    participant_bus_numbers: None,
                }],
                generator_area: vec![0, 0],
                load_area: vec![1],
                ..DispatchInput::default()
            }),
        )
        .expect("dispatch with endogenous load-based zonal reserve should solve");

        let zonal = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::Zone && reserve.product_id == "reg_up"
            })
            .expect("expected zonal reserve result");
        assert_eq!(zonal.requirement_mw, 50.0);
        assert_eq!(zonal.provided_mw, 50.0);
        assert_eq!(zonal.shortfall_mw, 0.0);
    }

    #[test]
    fn test_zonal_requirement_tracks_largest_generator_dispatch() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveOffer, ReserveProduct,
            ZonalReserveRequirement,
        };

        let mut net = one_bus_reserve_test_network();
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .clear();
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "syn".into(),
                capacity_mw: 50.0,
                cost_per_mwh: 0.0,
            });

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![ReserveProduct {
                    id: "syn".into(),
                    name: "Syn".into(),
                    direction: surge_network::market::ReserveDirection::Up,
                    deploy_secs: 600.0,
                    qualification: QualificationRule::Committed,
                    energy_coupling: EnergyCoupling::Headroom,
                    dispatchable_load_energy_coupling: None,
                    shared_limit_products: Vec::new(),
                    balance_products: Vec::new(),
                    kind: surge_network::market::ReserveKind::Real,
                    apply_deploy_ramp_limit: true,
                    demand_curve: PenaltyCurve::Linear {
                        cost_per_unit: 1000.0,
                    },
                }],
                zonal_reserve_requirements: vec![ZonalReserveRequirement {
                    zone_id: 1,
                    product_id: "syn".into(),
                    requirement_mw: 0.0,
                    per_period_mw: None,
                    shortfall_cost_per_unit: None,
                    served_dispatchable_load_coefficient: None,
                    largest_generator_dispatch_coefficient: Some(1.0),
                    participant_bus_numbers: None,
                }],
                generator_area: vec![1, 1],
                ..DispatchInput::default()
            }),
        )
        .expect("dispatch with endogenous generator-based zonal reserve should solve");

        let zonal = result.periods[0]
            .reserve_results
            .iter()
            .find(|reserve| {
                reserve.scope == crate::result::ReserveScope::Zone && reserve.product_id == "syn"
            })
            .expect("expected zonal reserve result");
        assert_eq!(zonal.requirement_mw, 50.0);
        assert_eq!(zonal.provided_mw, 50.0);
        assert_eq!(zonal.shortfall_mw, 0.0);
    }

    #[test]
    fn test_bus_results_use_period_load_profiles() {
        use surge_network::market::{LoadProfile, LoadProfiles};

        let net = one_bus_reserve_test_network();
        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                n_periods: 2,
                load_profiles: LoadProfiles {
                    n_timesteps: 2,
                    profiles: vec![LoadProfile {
                        bus: 1,
                        load_mw: vec![0.0, 100.0],
                    }],
                },
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let bus0 = result.periods[0]
            .bus_results
            .iter()
            .find(|bus| bus.bus_number == 1)
            .expect("period 0 bus result");
        let bus1 = result.periods[1]
            .bus_results
            .iter()
            .find(|bus| bus.bus_number == 1)
            .expect("period 1 bus result");

        assert_eq!(bus0.withdrawals_mw, 0.0);
        assert_eq!(bus1.withdrawals_mw, 100.0);
    }

    #[test]
    fn test_bus_results_use_ac_bus_load_active_override() {
        use surge_network::network::Load;

        let mut net = one_bus_reserve_test_network();
        net.generators[1].in_service = false;
        net.loads = vec![Load::new(1, 80.0, 20.0)];

        let result = solve_dispatch(
            &net,
            &ac_period_by_period_request(DispatchInput {
                n_periods: 1,
                ac_bus_load_profiles: crate::request::AcBusLoadProfiles {
                    profiles: vec![crate::request::AcBusLoadProfile {
                        bus_number: 1,
                        p_mw: Some(vec![50.0]),
                        q_mvar: None,
                    }],
                },
                ..DispatchInput::default()
            }),
        )
        .expect("AC dispatch should solve with active bus-load override");

        let bus = result.periods[0]
            .bus_results
            .iter()
            .find(|bus| bus.bus_number == 1)
            .expect("bus result should be present");

        assert!(
            (bus.withdrawals_mw - 50.0).abs() < 1e-6,
            "withdrawals_mw should reflect AC active override, got {}",
            bus.withdrawals_mw
        );
        assert!(
            bus.net_injection_mw.abs() < 1e-6,
            "single-bus AC case should remain balanced, got {}",
            bus.net_injection_mw
        );
    }

    #[test]
    fn test_dispatch_ac_fixed_commitment_honors_shutdown_deloading() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("ac_shutdown_deloading");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 250.0, 20.0));

        let mut slow = Generator::new(1, 0.0, 1.0);
        slow.pmin = 50.0;
        slow.pmax = 300.0;
        slow.commitment
            .get_or_insert_default()
            .shutdown_ramp_mw_per_min = Some(2.0);
        slow.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(slow);

        let mut peaker = Generator::new(1, 0.0, 1.0);
        peaker.pmin = 0.0;
        peaker.pmax = 400.0;
        peaker.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0],
        });
        net.generators.push(peaker);

        let schedule = fixed_schedule(
            vec![true, true],
            Some(vec![vec![true, true], vec![false, true]]),
        );

        let without_deload = solve_dispatch(
            &net,
            &ac_period_by_period_fixed_request(
                DispatchInput {
                    n_periods: 2,
                    enforce_shutdown_deloading: false,
                    ..DispatchInput::default()
                },
                schedule.clone(),
            ),
        )
        .expect("AC dispatch without deloading should solve");

        let with_deload = solve_dispatch(
            &net,
            &ac_period_by_period_fixed_request(
                DispatchInput {
                    n_periods: 2,
                    enforce_shutdown_deloading: true,
                    ..DispatchInput::default()
                },
                schedule,
            ),
        )
        .expect("AC dispatch with deloading should solve");

        assert!(
            without_deload.periods[0].pg_mw[0] > 121.0,
            "without deloading the slow unit should dispatch above its shutdown cap, got {:.2}",
            without_deload.periods[0].pg_mw[0]
        );
        assert!(
            with_deload.periods[0].pg_mw[0] <= 120.1,
            "with deloading the slow unit should be capped at its shutdown ramp, got {:.2}",
            with_deload.periods[0].pg_mw[0]
        );
    }

    #[test]
    fn test_storage_resource_costs_reconcile_into_summaries() {
        use surge_network::market::CostCurve;
        use surge_network::network::{Generator, StorageDispatchMode, StorageParams};

        let mut net = one_bus_reserve_test_network();
        // Decommit gen 1 so load (80 MW) is feasible with gen 0 (pmin=50) + storage (pmax=20).
        net.generators[1].in_service = false;
        let mut storage = Generator::new(1, 0.0, 1.0);
        storage.machine_id = Some("S1".to_string());
        storage.pmin = -20.0;
        storage.pmax = 20.0;
        storage.in_service = true;
        storage.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0],
        });
        storage.storage = Some(StorageParams {
            charge_efficiency: 1.0,
            discharge_efficiency: 1.0,
            energy_capacity_mwh: 100.0,
            soc_initial_mwh: 50.0,
            soc_min_mwh: 0.0,
            soc_max_mwh: 100.0,
            variable_cost_per_mwh: 0.0,
            degradation_cost_per_mwh: 5.0,
            dispatch_mode: StorageDispatchMode::CostMinimization,
            self_schedule_mw: 0.0,
            discharge_offer: None,
            charge_bid: None,
            max_c_rate_charge: None,
            max_c_rate_discharge: None,
            chemistry: None,
            discharge_foldback_soc_mwh: None,
            charge_foldback_soc_mwh: None,
            daily_cycle_limit: None,
        });
        net.generators.push(storage);

        let result = solve_dispatch(&net, &period_by_period_request(DispatchInput::default()))
            .expect("storage case should solve");

        let storage_row = result.periods[0]
            .resource_results
            .iter()
            .find(|resource| resource.resource_id == "gen_1_3")
            .expect("storage resource row should be present");

        assert!(storage_row.power_mw > 0.0);
        assert!(storage_row.energy_cost.unwrap_or(0.0) > 0.0);
        assert!((result.summary.total_cost - result.summary.total_energy_cost).abs() < 1e-6);
    }

    #[test]
    fn test_storage_cost_min_without_generator_cost_solves() {
        use surge_network::network::Load;

        let mut net = one_bus_storage_test_network();
        net.loads.push(Load::new(1, 10.0, 0.0));
        net.generators[0].cost = None;

        let result = solve_dispatch(&net, &period_by_period_request(DispatchInput::default()))
            .expect("storage economics should solve without a dummy generator cost curve");

        assert_eq!(result.periods.len(), 1);
        assert_eq!(result.periods[0].resource_results.len(), 1);
        assert!(
            result.periods[0].resource_results[0].power_mw > 9.9,
            "storage should discharge to serve load, got {:?}",
            result.periods[0].resource_results
        );
    }

    #[test]
    fn test_ac_dispatch_uses_offer_schedule_when_generator_cost_missing() {
        use surge_network::Network;
        use surge_network::market::{OfferCurve, OfferSchedule};
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("ac_offer_schedule_only");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 40.0, 5.0));

        let mut generator = Generator::new(1, 40.0, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.qmin = -50.0;
        generator.qmax = 50.0;
        generator.in_service = true;
        generator.cost = None;
        net.generators.push(generator);

        let request = ac_period_by_period_request(DispatchInput {
            n_periods: 1,
            ac_bus_load_profiles: crate::request::AcBusLoadProfiles {
                profiles: vec![crate::request::AcBusLoadProfile {
                    bus_number: 1,
                    p_mw: Some(vec![40.0]),
                    q_mvar: Some(vec![5.0]),
                }],
            },
            offer_schedules: HashMap::from([(
                0,
                OfferSchedule {
                    periods: vec![Some(OfferCurve {
                        segments: vec![(50.0, 10.0), (100.0, 10.0)],
                        no_load_cost: 0.0,
                        startup_tiers: vec![],
                    })],
                },
            )]),
            ..DispatchInput::default()
        });

        let result = solve_dispatch(&net, &request)
            .expect("AC dispatch should use offer schedules when generator.cost is missing");

        assert_eq!(result.periods.len(), 1);
        assert!(
            result.periods[0].resource_results[0].power_mw > 39.0,
            "generator should serve the 40 MW load from the scheduled offer"
        );
    }

    #[test]
    fn test_ac_fixed_multiperiod_dispatch_uses_offer_schedule_when_generator_cost_missing() {
        use surge_network::Network;
        use surge_network::market::{OfferCurve, OfferSchedule};
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("ac_offer_schedule_fixed_multiperiod");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 40.0, 5.0));

        let mut generator = Generator::new(1, 40.0, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.qmin = -50.0;
        generator.qmax = 50.0;
        generator.in_service = true;
        generator.cost = None;
        net.generators.push(generator);

        let request = ac_period_by_period_fixed_request(
            DispatchInput {
                n_periods: 2,
                period_hours: vec![1.0, 1.0],
                ac_bus_load_profiles: crate::request::AcBusLoadProfiles {
                    profiles: vec![crate::request::AcBusLoadProfile {
                        bus_number: 1,
                        p_mw: Some(vec![40.0, 45.0]),
                        q_mvar: Some(vec![5.0, 6.0]),
                    }],
                },
                offer_schedules: HashMap::from([(
                    0,
                    OfferSchedule {
                        periods: vec![
                            Some(OfferCurve {
                                segments: vec![(50.0, 10.0), (100.0, 10.0)],
                                no_load_cost: 0.0,
                                startup_tiers: vec![],
                            }),
                            Some(OfferCurve {
                                segments: vec![(50.0, 12.0), (100.0, 12.0)],
                                no_load_cost: 0.0,
                                startup_tiers: vec![],
                            }),
                        ],
                    },
                )]),
                ..DispatchInput::default()
            },
            fixed_schedule(vec![true], Some(vec![vec![true], vec![true]])),
        );

        let result = solve_dispatch(&net, &request).expect(
            "fixed multi-period AC dispatch should use offer schedules when generator.cost is missing",
        );

        assert_eq!(result.periods.len(), 2);
        assert!(result.periods[0].resource_results[0].power_mw > 39.0);
        assert!(result.periods[1].resource_results[0].power_mw > 44.0);
    }

    #[test]
    fn test_offer_schedule_startup_tiers_drive_public_startup_costs() {
        use surge_network::Network;
        use surge_network::market::{CostCurve, OfferCurve, OfferSchedule, StartupTier};
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("scheduled_startup_tiers");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 0.0));

        let mut generator = Generator::new(1, 0.0, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.in_service = true;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(generator);

        let request = scuc_request(
            DispatchInput {
                n_periods: 1,
                offer_schedules: HashMap::from([(
                    0,
                    OfferSchedule {
                        periods: vec![Some(OfferCurve {
                            segments: vec![(50.0, 10.0), (100.0, 10.0)],
                            no_load_cost: 0.0,
                            startup_tiers: vec![StartupTier {
                                max_offline_hours: f64::INFINITY,
                                cost: 4000.0,
                                sync_time_min: 0.0,
                            }],
                        })],
                    },
                )]),
                ..DispatchInput::default()
            },
            IndexedCommitmentOptions {
                initial_commitment: Some(vec![false]),
                initial_offline_hours: Some(vec![1.0]),
                step_size_hours: Some(1.0),
                ..IndexedCommitmentOptions::default()
            },
        );

        let result =
            solve_dispatch(&net, &request).expect("scheduled startup tier case should solve");

        assert!(result.startup.as_ref().is_some_and(|startup| startup[0][0]));
        assert!((result.summary.total_startup_cost - 4000.0).abs() < 1e-6);
        assert!((result.startup_cost_total.unwrap_or_default() - 4000.0).abs() < 1e-6);
        assert!((result.summary.total_cost - 4800.0).abs() < 1e-6);

        let resource = result.periods[0]
            .resource_results
            .iter()
            .find(|resource| resource.power_mw > 1.0)
            .expect("expected committed generator resource result");
        assert_eq!(resource.startup, Some(true));
        assert_eq!(resource.startup_cost, Some(4000.0));
    }

    #[test]
    fn test_fixed_dispatch_period_zero_startup_is_derived_from_initial_commitment() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("fixed_period_zero_startup");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 0.0));

        let mut generator = Generator::new(1, 0.0, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 100.0;
        generator.in_service = true;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 500.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(generator);

        let request = period_by_period_fixed_request(
            DispatchInput::default(),
            fixed_schedule(vec![false], Some(vec![vec![true]])),
        );

        let result = solve_dispatch(&net, &request)
            .expect("fixed dispatch should derive period-0 startup from initial commitment");

        assert!(result.startup.as_ref().is_some_and(|startup| startup[0][0]));
        assert_eq!(result.periods[0].resource_results[0].startup, Some(true));
        assert_eq!(
            result.periods[0].resource_results[0].startup_cost,
            Some(500.0)
        );
        assert_eq!(result.startup_cost_total, Some(500.0));
        assert!((result.summary.total_startup_cost - 500.0).abs() < 1e-6);
    }

    #[test]
    fn test_emissions_results_follow_in_service_generator_order() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("emissions_indexing");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 0.0));

        let mut out_of_service = Generator::new(1, 0.0, 1.0);
        out_of_service.pmin = 0.0;
        out_of_service.pmax = 100.0;
        out_of_service.in_service = false;
        out_of_service.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![1.0, 0.0],
        });

        let mut marginal = Generator::new(1, 0.0, 1.0);
        marginal.pmin = 0.0;
        marginal.pmax = 100.0;
        marginal.in_service = true;
        marginal.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut expensive = Generator::new(1, 0.0, 1.0);
        expensive.pmin = 0.0;
        expensive.pmax = 100.0;
        expensive.in_service = true;
        expensive.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0],
        });

        net.generators = vec![out_of_service, marginal, expensive];

        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                emission_profile: Some(EmissionProfile {
                    rates_tonnes_per_mwh: vec![0.25, 0.75],
                }),
                ..DispatchInput::default()
            }),
        )
        .expect("emissions case should solve");

        let dispatched = result.periods[0]
            .resource_results
            .iter()
            .find(|resource| resource.power_mw > 1.0)
            .expect("expected one dispatched in-service generator");
        let expected_co2 = dispatched.power_mw * 0.25;
        let emissions = result.periods[0]
            .emissions_results
            .as_ref()
            .expect("expected emissions results");

        assert!((result.periods[0].co2_t - expected_co2).abs() < 1e-6);
        assert!((dispatched.co2_t.unwrap_or_default() - expected_co2).abs() < 1e-6);
        assert!(
            (emissions
                .by_resource_t
                .get(&dispatched.resource_id)
                .copied()
                .unwrap_or_default()
                - expected_co2)
                .abs()
                < 1e-6
        );
        assert!((emissions.total_co2_t - expected_co2).abs() < 1e-6);
    }

    #[test]
    fn test_reserve_constraint_penalty_comes_from_product_curve() {
        use surge_network::market::{
            EnergyCoupling, PenaltyCurve, QualificationRule, ReserveDirection, ReserveProduct,
            SystemReserveRequirement,
        };

        let net = one_bus_reserve_test_network();
        let result = solve_dispatch(
            &net,
            &period_by_period_request(DispatchInput {
                reserve_products: vec![
                    ReserveProduct {
                        id: "spin".into(),
                        name: "Spinning Reserve".into(),
                        direction: ReserveDirection::Up,
                        deploy_secs: 600.0,
                        qualification: QualificationRule::Committed,
                        energy_coupling: EnergyCoupling::Headroom,
                        dispatchable_load_energy_coupling: None,
                        shared_limit_products: Vec::new(),
                        balance_products: Vec::new(),
                        kind: surge_network::market::ReserveKind::Real,
                        apply_deploy_ramp_limit: true,
                        demand_curve: PenaltyCurve::Linear {
                            cost_per_unit: 1000.0,
                        },
                    },
                    ReserveProduct {
                        id: "reg_up".into(),
                        name: "Reg Up".into(),
                        direction: ReserveDirection::Up,
                        deploy_secs: 300.0,
                        qualification: QualificationRule::Committed,
                        energy_coupling: EnergyCoupling::Headroom,
                        dispatchable_load_energy_coupling: None,
                        shared_limit_products: Vec::new(),
                        balance_products: Vec::new(),
                        kind: surge_network::market::ReserveKind::Real,
                        apply_deploy_ramp_limit: true,
                        demand_curve: PenaltyCurve::Linear {
                            cost_per_unit: 2500.0,
                        },
                    },
                ],
                system_reserve_requirements: vec![
                    SystemReserveRequirement {
                        product_id: "spin".into(),
                        requirement_mw: 500.0,
                        per_period_mw: None,
                    },
                    SystemReserveRequirement {
                        product_id: "reg_up".into(),
                        requirement_mw: 500.0,
                        per_period_mw: None,
                    },
                ],
                ..DispatchInput::default()
            }),
        )
        .unwrap();

        let period = &result.periods[0];
        let spin = period
            .constraint_results
            .iter()
            .find(|constraint| constraint.constraint_id == "reserve:system:spin")
            .expect("expected spin reserve constraint");
        let reg_up = period
            .constraint_results
            .iter()
            .find(|constraint| constraint.constraint_id == "reserve:system:reg_up")
            .expect("expected reg_up reserve constraint");

        assert_eq!(spin.penalty_cost, Some(1000.0));
        assert_eq!(reg_up.penalty_cost, Some(2500.0));
    }

    #[test]
    fn test_scuc_must_run_units_use_in_service_index_space() {
        use crate::config::emissions::MustRunUnits;
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, Load};

        let mut net = Network::new("scuc_must_run_in_service_index");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 80.0, 0.0));

        let mut out_of_service = Generator::new(1, 0.0, 1.0);
        out_of_service.in_service = false;
        out_of_service.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![1.0, 0.0],
        });

        let mut cheap = Generator::new(1, 0.0, 1.0);
        cheap.pmin = 0.0;
        cheap.pmax = 100.0;
        cheap.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut must_run = Generator::new(1, 0.0, 1.0);
        must_run.pmin = 50.0;
        must_run.pmax = 100.0;
        must_run.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![100.0, 0.0],
        });

        net.generators = vec![out_of_service, cheap, must_run];

        let result = solve_dispatch(
            &net,
            &scuc_request(
                DispatchInput {
                    n_periods: 1,
                    enforce_thermal_limits: false,
                    must_run_units: Some(MustRunUnits {
                        unit_indices: vec![1],
                    }),
                    ..DispatchInput::default()
                },
                IndexedCommitmentOptions::default(),
            ),
        )
        .expect("SCUC with external must-run should solve");

        assert!(
            result.commitment.as_ref().is_some_and(|rows| rows[0][1]),
            "the second in-service generator should be forced on by MustRunUnits"
        );
        assert!(
            result.periods[0].pg_mw[1] >= 49.9,
            "the must-run unit should dispatch at or above pmin, got {:.2}",
            result.periods[0].pg_mw[1]
        );
    }

    #[test]
    fn test_market30_scuc_multihour_tracks_real_fleet_state() {
        let network = load_market30_network();
        let load_scales = vec![0.60, 0.55, 0.50, 0.60, 1.10, 1.35, 1.20, 0.75];
        let wind_cf = vec![0.55, 0.60, 0.65, 0.50, 0.20, 0.10, 0.15, 0.30];
        let solar_cf = vec![0.0, 0.10, 0.35, 0.60, 0.65, 0.20, 0.0, 0.0];

        let request = scuc_request(
            DispatchInput {
                n_periods: load_scales.len(),
                load_profiles: market30_uniform_load_profiles(&network, &load_scales),
                renewable_profiles: market30_renewable_profiles(&network, &wind_cf, &solar_cf),
                enforce_thermal_limits: false,
                enforce_flowgates: false,
                ..DispatchInput::default()
            },
            IndexedCommitmentOptions::default(),
        );

        let result =
            solve_dispatch(&network, &request).expect("multi-hour market30 SCUC should solve");
        let keyed = solve_keyed_dispatch(&network, &request)
            .expect("multi-hour market30 keyed SCUC should solve");
        assert!(
            keyed.objective_ledger_is_consistent(),
            "market30 multi-hour SCUC should reconcile exactly: {:#?}",
            keyed.objective_ledger_mismatches()
        );

        let nuclear_idx = market30_generator_index(&network, "G4");
        let bess_idx = market30_generator_index(&network, "B1");
        let ph_idx = market30_generator_index(&network, "PH");
        let cc_gt_idx = market30_generator_index(&network, "CC1_GT");
        let cc_st_idx = market30_generator_index(&network, "CC1_ST");
        let commitment = result
            .commitment
            .as_ref()
            .expect("SCUC should expose a commitment schedule");
        let bess_id = network.generators[bess_idx].id.clone();
        let ph_id = network.generators[ph_idx].id.clone();

        assert_eq!(result.periods.len(), load_scales.len());
        assert!(
            commitment.iter().all(|hour| hour[nuclear_idx]),
            "market30 nuclear unit should remain committed in every hour"
        );

        assert_eq!(result.cc_config_schedule.len(), load_scales.len());
        let cc_configs: Vec<&str> = result
            .cc_config_schedule
            .iter()
            .filter_map(|hour| hour.first().and_then(|config| config.as_deref()))
            .collect();
        assert!(
            cc_configs.contains(&"CC_FULL"),
            "market30 combined-cycle plant should eventually use CC_FULL: {cc_configs:?}"
        );
        assert!(
            cc_configs
                .iter()
                .all(|config| matches!(*config, "GT_ONLY" | "CC_FULL")),
            "market30 CC schedule should only use named plant configurations: {cc_configs:?}"
        );
        assert!(
            commitment
                .iter()
                .all(|hour| !hour[cc_st_idx] || hour[cc_gt_idx]),
            "market30 steam section should never commit without the gas turbine"
        );

        let bess_soc = result
            .storage_soc
            .get(&bess_idx)
            .expect("market30 BESS SoC should be tracked");
        let ph_soc = result
            .storage_soc
            .get(&ph_idx)
            .expect("market30 pumped hydro SoC should be tracked");
        assert_eq!(bess_soc.len(), load_scales.len());
        assert_eq!(ph_soc.len(), load_scales.len());
        assert!(
            bess_soc
                .iter()
                .all(|soc| *soc >= 15.0 - 1e-6 && *soc <= 190.0 + 1e-6)
        );
        assert!(
            ph_soc
                .iter()
                .all(|soc| *soc >= 80.0 - 1e-6 && *soc <= 760.0 + 1e-6)
        );

        let storage_moves = result.periods.iter().any(|period| {
            resource_result(period, &bess_id).power_mw.abs() > 1.0
                || resource_result(period, &ph_id).power_mw.abs() > 1.0
        });
        assert!(
            storage_moves,
            "market30 storage fleet should move in at least one SCUC hour"
        );
    }

    #[test]
    fn test_market30_scuc_reserves_respect_qualifications_and_quick_start() {
        use surge_network::market::{ReserveProduct, SystemReserveRequirement};

        let network = load_market30_network();
        let n_periods = 6;
        let load_scales = vec![1.0; n_periods];
        let wind_cf = vec![0.18; n_periods];
        let solar_cf = vec![0.0; n_periods];
        let reserve_products: Vec<ReserveProduct> = ReserveProduct::ercot_defaults()
            .into_iter()
            .filter(|product| matches!(product.id.as_str(), "spin" | "reg_up" | "reg_dn" | "nspin"))
            .collect();

        let request = scuc_request(
            DispatchInput {
                n_periods,
                load_profiles: market30_uniform_load_profiles(&network, &load_scales),
                renewable_profiles: market30_renewable_profiles(&network, &wind_cf, &solar_cf),
                reserve_products,
                system_reserve_requirements: vec![
                    SystemReserveRequirement {
                        product_id: "spin".into(),
                        requirement_mw: 25.0,
                        per_period_mw: None,
                    },
                    SystemReserveRequirement {
                        product_id: "reg_up".into(),
                        requirement_mw: 10.0,
                        per_period_mw: None,
                    },
                    SystemReserveRequirement {
                        product_id: "reg_dn".into(),
                        requirement_mw: 10.0,
                        per_period_mw: None,
                    },
                    SystemReserveRequirement {
                        product_id: "nspin".into(),
                        requirement_mw: 30.0,
                        per_period_mw: None,
                    },
                ],
                enforce_thermal_limits: false,
                enforce_flowgates: false,
                ..DispatchInput::default()
            },
            IndexedCommitmentOptions::default(),
        );

        let result = solve_dispatch(&network, &request)
            .expect("market30 reserve co-optimization should solve");
        let keyed = solve_keyed_dispatch(&network, &request)
            .expect("market30 reserve keyed co-optimization should solve");
        assert!(
            keyed.objective_ledger_is_consistent(),
            "market30 reserve co-optimization should reconcile exactly: {:#?}",
            keyed.objective_ledger_mismatches()
        );

        let g3_id = network.generators[market30_generator_index(&network, "G3")]
            .id
            .clone();
        let g5_id = network.generators[market30_generator_index(&network, "G5")]
            .id
            .clone();
        let g4_id = network.generators[market30_generator_index(&network, "G4")]
            .id
            .clone();
        let w1_id = network.generators[market30_generator_index(&network, "W1")]
            .id
            .clone();
        let s1_id = network.generators[market30_generator_index(&network, "S1")]
            .id
            .clone();
        let b1_id = network.generators[market30_generator_index(&network, "B1")]
            .id
            .clone();
        let ph_id = network.generators[market30_generator_index(&network, "PH")]
            .id
            .clone();

        let mut quick_start_nspin = 0.0;
        let mut storage_regulating = false;

        for period in &result.periods {
            let nspin_result = period
                .reserve_results
                .iter()
                .find(|reserve| {
                    reserve.scope == crate::result::ReserveScope::System
                        && reserve.product_id == "nspin"
                })
                .expect("market30 should expose the nspin reserve result");
            assert!(
                nspin_result.provided_mw >= 29.0,
                "nspin requirement should be met from quick-start supply, got {:.2} MW",
                nspin_result.provided_mw
            );

            let g4 = resource_result(period, &g4_id);
            let w1 = resource_result(period, &w1_id);
            let s1 = resource_result(period, &s1_id);
            assert!(
                g4.reserve_awards.values().all(|award| award.abs() < 1e-9)
                    && w1.reserve_awards.values().all(|award| award.abs() < 1e-9)
                    && s1.reserve_awards.values().all(|award| award.abs() < 1e-9),
                "units without reserve offers in market30 should not clear positive reserves"
            );

            let g3 = resource_result(period, &g3_id);
            let g5 = resource_result(period, &g5_id);
            quick_start_nspin += g3.reserve_awards.get("nspin").copied().unwrap_or(0.0)
                + g5.reserve_awards.get("nspin").copied().unwrap_or(0.0);
            assert_eq!(g3.reserve_awards.get("reg_up").copied().unwrap_or(0.0), 0.0);
            assert_eq!(g3.reserve_awards.get("reg_dn").copied().unwrap_or(0.0), 0.0);
            assert_eq!(g5.reserve_awards.get("reg_up").copied().unwrap_or(0.0), 0.0);
            assert_eq!(g5.reserve_awards.get("reg_dn").copied().unwrap_or(0.0), 0.0);

            let b1 = resource_result(period, &b1_id);
            let ph = resource_result(period, &ph_id);
            storage_regulating |= b1.reserve_awards.get("spin").copied().unwrap_or(0.0) > 0.0
                || b1.reserve_awards.get("reg_up").copied().unwrap_or(0.0) > 0.0
                || b1.reserve_awards.get("reg_dn").copied().unwrap_or(0.0) > 0.0
                || ph.reserve_awards.get("spin").copied().unwrap_or(0.0) > 0.0
                || ph.reserve_awards.get("reg_up").copied().unwrap_or(0.0) > 0.0
                || ph.reserve_awards.get("reg_dn").copied().unwrap_or(0.0) > 0.0;
        }

        assert!(
            quick_start_nspin > 1.0,
            "market30 quick-start CTs should provide non-spin reserves"
        );
        assert!(
            storage_regulating,
            "market30 storage resources should participate in spin/reg reserves"
        );
    }

    #[test]
    fn test_market30_sced_congestion_dr_and_request_hvdc_link_interact() {
        let mut network = load_market30_network();
        let g5_idx = market30_generator_index(&network, "G5");
        let bess_idx = market30_generator_index(&network, "B1");
        let ph_idx = market30_generator_index(&network, "PH");
        let nuclear_idx = market30_generator_index(&network, "G4");
        let hvdc_request = market30_request_hvdc_link(&network);
        network.flowgates[0].limit_mw = 15.0;
        network.interfaces[0].limit_forward_mw = 30.0;
        network.interfaces[0].limit_reverse_mw = 30.0;
        network.generators[nuclear_idx].p = 60.0;
        network.generators[nuclear_idx].pmax = 60.0;

        let request = period_by_period_fixed_request(
            DispatchInput {
                n_periods: 1,
                load_profiles: market30_area_boosted_load_profiles(&network, &[1.05], 3, &[2.20]),
                renewable_profiles: market30_renewable_profiles(&network, &[0.10], &[0.0]),
                dispatchable_loads: network.market_data.dispatchable_loads.clone(),
                hvdc_links: vec![hvdc_request.clone()],
                initial_state: IndexedDispatchInitialState {
                    storage_soc_override: Some(HashMap::from([(bess_idx, 15.0), (ph_idx, 80.0)])),
                    ..IndexedDispatchInitialState::default()
                },
                enforce_thermal_limits: false,
                enforce_flowgates: true,
                ..DispatchInput::default()
            },
            fixed_schedule(
                vec![true, true, true, true, true, false, true, true, true, true],
                None,
            ),
        );

        let result =
            solve_keyed_dispatch(&network, &request).expect("congested market30 SCED should solve");
        assert!(
            result.objective_ledger_is_consistent(),
            "market30 congestion/HVDC regression should reconcile exactly: {:#?}",
            result.objective_ledger_mismatches()
        );
        let period = &result.periods[0];

        assert_eq!(period.hvdc_results.len(), 1);
        assert!(
            period.hvdc_results[0].mw >= hvdc_request.p_dc_min_mw - 1e-6
                && period.hvdc_results[0].mw <= hvdc_request.p_dc_max_mw + 1e-6,
            "market30 request-side HVDC result should stay within the configured dispatch range"
        );
        assert!(
            period.constraint_results.iter().any(|constraint| {
                (constraint.kind == crate::result::ConstraintKind::Flowgate
                    || constraint.kind == crate::result::ConstraintKind::Interface)
                    && constraint.shadow_price.unwrap_or(0.0).abs() > 1e-4
            }),
            "market30 congestion test should bind either the native flowgate or interface"
        );

        let served_mw: HashMap<u32, f64> = period
            .resource_results
            .iter()
            .filter(|resource| {
                resource.kind == crate::result::DispatchResourceKind::DispatchableLoad
            })
            .filter_map(|resource| Some((resource.bus_number?, -resource.power_mw)))
            .collect();
        let curtailed = period
            .resource_results
            .iter()
            .filter(|resource| {
                resource.kind == crate::result::DispatchResourceKind::DispatchableLoad
            })
            .any(|resource| match &resource.detail {
                crate::result::ResourcePeriodDetail::DispatchableLoad(detail) => {
                    detail.curtailed_mw > 0.1
                }
                _ => false,
            });
        assert!(
            curtailed,
            "congested market30 case should curtail at least one dispatchable load, served={served_mw:?}"
        );
        assert!(
            served_mw.get(&30).copied().unwrap_or_default() < 14.9,
            "the $80/MWh bus-30 load should curtail under area-3 scarcity, served={served_mw:?}"
        );
        assert!(
            served_mw.get(&7).copied().unwrap_or_default() > 29.9,
            "the area-1 bus-7 DR load should stay served while the constrained area-3 loads curtail, served={served_mw:?}"
        );
        assert!(
            served_mw.get(&21).copied().unwrap_or_default() < 19.9,
            "the area-3 bus-21 interruptible load should also curtail once the constrained pocket is short, served={served_mw:?}"
        );

        let bus1 = period.bus(1).expect("bus 1 should be present");
        let bus30 = period.bus(30).expect("bus 30 should be present");
        assert!(
            bus30.lmp > bus1.lmp + 1e-3,
            "congested market30 case should separate bus-30 and bus-1 LMPs, got {:.3} vs {:.3}",
            bus30.lmp,
            bus1.lmp
        );
        assert!(
            bus30.lmp >= 79.0,
            "bus-30 LMP should rise toward the marginal DR value, got {:.3}",
            bus30.lmp
        );

        let g5_id = network.generators[g5_idx].id.clone();
        assert!(
            period
                .resource(&g5_id)
                .and_then(|resource| match &resource.detail {
                    crate::result::ResourcePeriodDetail::Generator(detail) => detail.commitment,
                    crate::result::ResourcePeriodDetail::Storage(detail) => detail.commitment,
                    crate::result::ResourcePeriodDetail::DispatchableLoad(_) => None,
                })
                == Some(false),
            "fixed commitment should keep G5 offline in the congestion regression"
        );
    }

    #[test]
    fn test_request_fixed_hvdc_dispatch_clamps_sced_transfer() {
        let network = load_market30_network();
        let hvdc_request = market30_request_hvdc_link(&network);
        let target_mw = 25.0;

        let request = period_by_period_request(DispatchInput {
            n_periods: 1,
            hvdc_links: vec![hvdc_request],
            fixed_hvdc_dispatch_mw: HashMap::from([(0usize, vec![target_mw])]),
            ..DispatchInput::default()
        });

        let result =
            solve_keyed_dispatch(&network, &request).expect("SCED with fixed HVDC dispatch");
        assert!(
            result.objective_ledger_is_consistent(),
            "fixed-HVDC regression should reconcile exactly: {:#?}",
            result.objective_ledger_mismatches()
        );
        assert_eq!(result.periods.len(), 1);
        assert_eq!(result.periods[0].hvdc_results.len(), 1);
        assert!(
            (result.periods[0].hvdc_results[0].mw - target_mw).abs() <= 1e-6,
            "expected fixed HVDC schedule to clamp dispatch to {target_mw} MW, got {}",
            result.periods[0].hvdc_results[0].mw,
        );
    }

    #[test]
    fn test_market30_carbon_price_shifts_dispatch_and_emissions() {
        let network = load_market30_network();
        let load_scales = [1.25];
        let zero_renewables = [0.0];
        let bess_idx = market30_generator_index(&network, "B1");
        let ph_idx = market30_generator_index(&network, "PH");
        let coal_id = network.generators[market30_generator_index(&network, "G1")]
            .id
            .clone();

        let baseline_request = period_by_period_request(DispatchInput {
            n_periods: 1,
            load_profiles: market30_uniform_load_profiles(&network, &load_scales),
            renewable_profiles: market30_renewable_profiles(
                &network,
                &zero_renewables,
                &zero_renewables,
            ),
            initial_state: IndexedDispatchInitialState {
                storage_soc_override: Some(HashMap::from([(bess_idx, 15.0), (ph_idx, 80.0)])),
                ..IndexedDispatchInitialState::default()
            },
            enforce_thermal_limits: false,
            enforce_flowgates: false,
            ..DispatchInput::default()
        });
        let carbon_request = period_by_period_request(DispatchInput {
            carbon_price: Some(CarbonPrice::new(150.0)),
            ..DispatchInput {
                n_periods: 1,
                load_profiles: market30_uniform_load_profiles(&network, &load_scales),
                renewable_profiles: market30_renewable_profiles(
                    &network,
                    &zero_renewables,
                    &zero_renewables,
                ),
                initial_state: IndexedDispatchInitialState {
                    storage_soc_override: Some(HashMap::from([(bess_idx, 15.0), (ph_idx, 80.0)])),
                    ..IndexedDispatchInitialState::default()
                },
                enforce_thermal_limits: false,
                enforce_flowgates: false,
                ..DispatchInput::default()
            }
        });

        let baseline = solve_keyed_dispatch(&network, &baseline_request)
            .expect("baseline market30 dispatch should solve");
        let carbon = solve_keyed_dispatch(&network, &carbon_request)
            .expect("carbon-priced market30 dispatch should solve");
        assert!(
            baseline.objective_ledger_is_consistent(),
            "baseline market30 carbon case should reconcile exactly: {:#?}",
            baseline.objective_ledger_mismatches()
        );
        assert!(
            carbon.objective_ledger_is_consistent(),
            "carbon-priced market30 case should reconcile exactly: {:#?}",
            carbon.objective_ledger_mismatches()
        );

        let baseline_coal = baseline.periods[0]
            .resource(&coal_id)
            .expect("baseline coal resource should be present")
            .power_mw;
        let carbon_coal = carbon.periods[0]
            .resource(&coal_id)
            .expect("carbon coal resource should be present")
            .power_mw;

        assert!(
            carbon.summary.total_co2_t + 1e-6 < baseline.summary.total_co2_t,
            "carbon price should lower total market30 emissions: baseline {:.3}t vs carbon {:.3}t",
            baseline.summary.total_co2_t,
            carbon.summary.total_co2_t
        );
        assert!(
            carbon_coal + 1e-6 < baseline_coal,
            "carbon price should back down the coal unit: baseline {:.3} MW vs carbon {:.3} MW",
            baseline_coal,
            carbon_coal
        );
        assert!(
            carbon.periods[0].emissions_results.is_some(),
            "carbon-priced market30 dispatch should expose emissions rollups"
        );
    }
}
