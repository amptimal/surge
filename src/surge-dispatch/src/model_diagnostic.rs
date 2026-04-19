// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Structured model diagnostic snapshots captured at solve time.
//!
//! When `DispatchRuntime::capture_model_diagnostics` is enabled, each LP/MIP
//! solve stage produces a [`ModelDiagnostic`] that captures the optimization
//! model's structure, binding constraints, active variable bounds, penalty
//! violations, and solver performance — without requiring an LP file dump or
//! ad-hoc tracing.
//!
//! The diagnostics are attached to the [`crate::DispatchSolution`] and
//! serialized as part of the surge-json `"results"` profile.

use serde::{Deserialize, Serialize};

/// Which solve stage produced this diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    /// Time-coupled SCUC (commitment + dispatch).
    ScucCommitment,
    /// Single-period or time-coupled DC SCED.
    ScedDispatch,
    /// SCUC pricing pass (LP relaxation for LMP extraction).
    ScucPricing,
    /// AC SCED period solve.
    AcSced,
    /// SCED-AC Benders master iteration.
    ScedAcBendersMaster,
}

/// Constraint family classification for row grouping.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintFamily {
    PowerBalance,
    BranchThermal,
    Flowgate,
    Interface,
    RampUp,
    RampDown,
    CommitmentHeadroom,
    CommitmentFootroom,
    CommitmentLogic,
    MinUpTime,
    MinDownTime,
    StartupTier,
    ReserveRequirement,
    ReserveCoupling,
    StorageSoc,
    StorageBounds,
    EnergyWindow,
    BranchSwitching,
    PumpedHydro,
    CombinedCycle,
    DispatchableLoad,
    FrequencySecurity,
    ForbiddenZone,
    HvdcRamp,
    ExplicitContingency,
    CommitmentPolicy,
    SystemPolicy,
    Other,
}

impl ConstraintFamily {
    /// Infer the constraint family from a row label string.
    ///
    /// This parses the naming convention used by SCUC/SCED row builders
    /// (e.g. `"h3:power_balance:bus_5"`, `"h3:gen_A:ramp_up"`).
    pub fn from_label(label: &str) -> Self {
        // Strip the hour prefix (e.g. "h3:") if present.
        let body = label.find(':').map(|i| &label[i + 1..]).unwrap_or(label);

        if body.contains("power_balance") {
            Self::PowerBalance
        } else if body.contains("branch_thermal")
            || body.contains("flow_ub")
            || body.contains("flow_lb")
        {
            Self::BranchThermal
        } else if body.contains("flowgate") {
            Self::Flowgate
        } else if body.contains("interface") {
            Self::Interface
        } else if body.contains("ramp_up") || body.contains("startup_ramp") {
            Self::RampUp
        } else if body.contains("ramp_down") || body.contains("shutdown_ramp") {
            Self::RampDown
        } else if body.contains("headroom") {
            Self::CommitmentHeadroom
        } else if body.contains("footroom") {
            Self::CommitmentFootroom
        } else if body.contains("commit_logic") || body.contains("state_transition") {
            Self::CommitmentLogic
        } else if body.contains("min_up") {
            Self::MinUpTime
        } else if body.contains("min_down") {
            Self::MinDownTime
        } else if body.contains("startup_tier") || body.contains("startup_delta") {
            Self::StartupTier
        } else if body.contains("reserve_req") || body.contains("reserve_requirement") {
            Self::ReserveRequirement
        } else if body.contains("reserve_coupling") || body.contains("reserve_cap") {
            Self::ReserveCoupling
        } else if body.contains("soc") || body.contains("state_of_charge") {
            Self::StorageSoc
        } else if body.contains("storage") {
            Self::StorageBounds
        } else if body.contains("energy_window") || body.contains("energy_req") {
            Self::EnergyWindow
        } else if body.contains("branch_switch") || body.contains("branch_state") {
            Self::BranchSwitching
        } else if body.contains("pumped_hydro") || body.contains("ph_") {
            Self::PumpedHydro
        } else if body.contains("cc_") || body.contains("combined_cycle") {
            Self::CombinedCycle
        } else if body.contains("dl_") || body.contains("dispatchable_load") {
            Self::DispatchableLoad
        } else if body.contains("frequency") || body.contains("inertia") || body.contains("rocof") {
            Self::FrequencySecurity
        } else if body.contains("forbidden_zone") || body.contains("foz") {
            Self::ForbiddenZone
        } else if body.contains("hvdc_ramp") {
            Self::HvdcRamp
        } else if body.contains("explicit_ctg") || body.contains("contingency") {
            Self::ExplicitContingency
        } else if body.contains("commit_policy") || body.contains("commitment_constraint") {
            Self::CommitmentPolicy
        } else if body.contains("system_policy") || body.contains("max_starts") {
            Self::SystemPolicy
        } else {
            Self::Other
        }
    }
}

/// Model dimensions and sparsity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelStats {
    pub n_continuous: usize,
    pub n_binary: usize,
    pub n_integer: usize,
    pub n_rows: usize,
    pub n_nonzeros: usize,
    /// Matrix density: `nnz / (n_col * n_row)`.
    pub density: f64,
}

impl ModelStats {
    pub fn n_cols(&self) -> usize {
        self.n_continuous + self.n_binary + self.n_integer
    }
}

/// Aggregate statistics for one constraint family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintFamilyStats {
    pub family: ConstraintFamily,
    pub n_rows: usize,
    /// Number of rows where `|slack| < tolerance`.
    pub n_binding: usize,
    /// Largest `|shadow_price|` among binding rows.
    pub max_abs_shadow_price: f64,
    /// Largest penalty slack value (for penalty constraint families).
    pub max_violation: f64,
}

/// Which side of a variable bound is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundSide {
    Lower,
    Upper,
}

/// A variable that is sitting at one of its bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveBound {
    /// Human-readable variable name (e.g. `"h3:pg:gen_A"`).
    pub variable: String,
    /// Current primal value.
    pub value: f64,
    /// The bound it is at.
    pub bound: f64,
    pub bound_side: BoundSide,
    /// Reduced cost: marginal cost of relaxing this bound by 1 unit.
    pub reduced_cost: f64,
}

/// A constraint that is binding (zero slack).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingConstraint {
    /// Human-readable row name (e.g. `"h3:power_balance:bus_5"`).
    pub name: String,
    pub family: ConstraintFamily,
    /// Shadow price (dual value) in objective units per constraint unit.
    pub shadow_price: f64,
    /// Constraint slack (distance from bound).
    pub slack: f64,
    /// `a_row · x` — the constraint's left-hand side evaluated at the solution.
    pub activity: f64,
}

/// A penalty slack variable with a nonzero value (a soft constraint violation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePenalty {
    /// Human-readable variable name (e.g. `"h7:branch_upper_slack:line_47-48"`).
    pub name: String,
    pub family: ConstraintFamily,
    /// Violation magnitude (MW, MVAr, or pu depending on the constraint).
    pub value: f64,
    /// Penalty cost coefficient for this slack variable.
    pub penalty_cost_coefficient: f64,
    /// Total penalty cost contribution: `value × coefficient`.
    pub penalty_cost: f64,
}

/// Solver performance and process metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SolverStats {
    pub solver_name: String,
    pub solve_time_secs: f64,
    pub iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mip_gap: Option<f64>,
    pub warm_start_used: bool,
    pub objective: f64,
    pub status: String,
}

/// A structured diagnostic snapshot captured after an LP/MIP solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDiagnostic {
    /// Which solve stage produced this snapshot.
    pub stage: DiagnosticStage,

    /// Model dimensions.
    pub model_stats: ModelStats,

    /// Per-constraint-family aggregate statistics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraint_families: Vec<ConstraintFamilyStats>,

    /// Variables at their bounds, ranked by `|reduced_cost|` descending.
    /// Capped to the top 100 by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_bounds: Vec<ActiveBound>,

    /// Binding constraints, ranked by `|shadow_price|` descending.
    /// Capped to the top 200 by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binding_constraints: Vec<BindingConstraint>,

    /// Penalty slack variables with nonzero values, ranked by `penalty_cost`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_penalties: Vec<ActivePenalty>,

    /// Solver performance metadata.
    pub solver_stats: SolverStats,
}

// ─── Builder ─────────────────────────────────────────────────────────────────

/// Maximum number of active-bound entries to retain (ranked by |reduced_cost|).
const MAX_ACTIVE_BOUNDS: usize = 100;
/// Maximum number of binding-constraint entries to retain (ranked by |shadow_price|).
const MAX_BINDING_CONSTRAINTS: usize = 200;

impl ModelDiagnostic {
    /// Build a [`ModelDiagnostic`] from the raw LP/MIP problem and solution.
    ///
    /// `col_names` and `row_labels` provide human-readable names for columns
    /// and rows respectively. If shorter than the actual column/row count,
    /// missing entries are labelled with their index.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        stage: DiagnosticStage,
        n_col: usize,
        n_row: usize,
        n_nz: usize,
        col_lower: &[f64],
        col_upper: &[f64],
        col_cost: &[f64],
        row_lower: &[f64],
        row_upper: &[f64],
        integrality: Option<&[surge_opf::backends::VariableDomain]>,
        x: &[f64],
        row_dual: &[f64],
        col_dual: &[f64],
        objective: f64,
        status: &str,
        solver_name: &str,
        solve_time_secs: f64,
        iterations: u32,
        mip_gap: Option<f64>,
        warm_start_used: bool,
        col_names: &[String],
        row_labels: &[String],
    ) -> Self {
        use surge_opf::backends::VariableDomain;

        let tol = 1e-6;

        // ── Model stats ──
        let (mut n_binary, mut n_integer) = (0usize, 0usize);
        if let Some(domains) = integrality {
            for d in domains {
                match d {
                    VariableDomain::Binary => n_binary += 1,
                    VariableDomain::Integer => n_integer += 1,
                    VariableDomain::Continuous => {}
                }
            }
        }
        let n_continuous = n_col - n_binary - n_integer;
        let density = if n_col > 0 && n_row > 0 {
            n_nz as f64 / (n_col as f64 * n_row as f64)
        } else {
            0.0
        };

        let model_stats = ModelStats {
            n_continuous,
            n_binary,
            n_integer,
            n_rows: n_row,
            n_nonzeros: n_nz,
            density,
        };

        // ── Classify rows by family ──
        let row_families: Vec<ConstraintFamily> = (0..n_row)
            .map(|i| {
                let label = row_labels.get(i).map(String::as_str).unwrap_or("");
                ConstraintFamily::from_label(label)
            })
            .collect();

        // ── Constraint family stats ──
        let mut family_map: std::collections::HashMap<ConstraintFamily, (usize, usize, f64, f64)> =
            std::collections::HashMap::new();

        for (i, family) in row_families.iter().enumerate() {
            let entry = family_map.entry(family.clone()).or_insert((0, 0, 0.0, 0.0));
            entry.0 += 1; // n_rows

            // Compute slack: distance from the tighter bound.
            if i < row_dual.len() {
                let dual = row_dual[i].abs();
                // A row is binding if |dual| > tol (complementary slackness).
                if dual > tol {
                    entry.1 += 1; // n_binding
                    if dual > entry.2 {
                        entry.2 = dual; // max_abs_shadow_price
                    }
                }
            }
        }

        let mut constraint_families: Vec<ConstraintFamilyStats> = family_map
            .into_iter()
            .map(
                |(family, (n_rows, n_binding, max_sp, max_vio))| ConstraintFamilyStats {
                    family,
                    n_rows,
                    n_binding,
                    max_abs_shadow_price: max_sp,
                    max_violation: max_vio,
                },
            )
            .collect();
        constraint_families.sort_by(|a, b| {
            b.max_abs_shadow_price
                .partial_cmp(&a.max_abs_shadow_price)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // ── Binding constraints (top by |shadow_price|) ──
        let mut binding_constraints: Vec<BindingConstraint> = Vec::new();
        #[allow(clippy::needless_range_loop)]
        for i in 0..n_row.min(row_dual.len()) {
            let dual = row_dual[i];
            if dual.abs() > tol {
                let name = row_labels
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("row_{i}"));
                let family = row_families[i].clone();

                // Compute activity as rhs (from the binding bound side).
                let activity = if dual > 0.0 {
                    row_upper.get(i).copied().unwrap_or(0.0)
                } else {
                    row_lower.get(i).copied().unwrap_or(0.0)
                };

                binding_constraints.push(BindingConstraint {
                    name,
                    family,
                    shadow_price: dual,
                    slack: 0.0,
                    activity,
                });
            }
        }
        binding_constraints.sort_by(|a, b| {
            b.shadow_price
                .abs()
                .partial_cmp(&a.shadow_price.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        binding_constraints.truncate(MAX_BINDING_CONSTRAINTS);

        // ── Active bounds (variables at bounds with |reduced_cost| > 0) ──
        let mut active_bounds: Vec<ActiveBound> = Vec::new();
        #[allow(clippy::needless_range_loop)]
        for j in 0..n_col.min(x.len()) {
            let rc = col_dual.get(j).copied().unwrap_or(0.0);
            if rc.abs() <= tol {
                continue;
            }
            let val = x[j];
            let lo = col_lower.get(j).copied().unwrap_or(f64::NEG_INFINITY);
            let hi = col_upper.get(j).copied().unwrap_or(f64::INFINITY);

            let (bound, side) = if (val - lo).abs() < tol && lo.is_finite() {
                (lo, BoundSide::Lower)
            } else if (val - hi).abs() < tol && hi.is_finite() {
                (hi, BoundSide::Upper)
            } else {
                continue;
            };

            let variable = col_names
                .get(j)
                .cloned()
                .unwrap_or_else(|| format!("col_{j}"));

            active_bounds.push(ActiveBound {
                variable,
                value: val,
                bound,
                bound_side: side,
                reduced_cost: rc,
            });
        }
        active_bounds.sort_by(|a, b| {
            b.reduced_cost
                .abs()
                .partial_cmp(&a.reduced_cost.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        active_bounds.truncate(MAX_ACTIVE_BOUNDS);

        // ── Active penalties (slack variables with nonzero value) ──
        let mut active_penalties: Vec<ActivePenalty> = Vec::new();
        #[allow(clippy::needless_range_loop)]
        for j in 0..n_col.min(x.len()) {
            let val = x[j];
            if val.abs() <= tol {
                continue;
            }
            let name = col_names.get(j).map(String::as_str).unwrap_or("");
            if !name.contains("slack")
                && !name.contains("penalty")
                && !name.contains("curtailment")
                && !name.contains("excess")
            {
                continue;
            }
            let cost_coeff = col_cost.get(j).copied().unwrap_or(0.0);
            let family = ConstraintFamily::from_label(name);
            active_penalties.push(ActivePenalty {
                name: name.to_string(),
                family,
                value: val,
                penalty_cost_coefficient: cost_coeff,
                penalty_cost: val * cost_coeff,
            });
        }
        active_penalties.sort_by(|a, b| {
            b.penalty_cost
                .abs()
                .partial_cmp(&a.penalty_cost.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let solver_stats = SolverStats {
            solver_name: solver_name.to_string(),
            solve_time_secs,
            iterations,
            mip_gap,
            warm_start_used,
            objective,
            status: status.to_string(),
        };

        Self {
            stage,
            model_stats,
            constraint_families,
            active_bounds,
            binding_constraints,
            active_penalties,
            solver_stats,
        }
    }
}
