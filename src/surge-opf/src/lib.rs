// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge OPF — Optimal Power Flow solvers
//!
//! Provides DC-OPF, AC-OPF, SCOPF, and switching/voltage-control solvers with
//! a small top-level API surface:
//! - **HiGHS** (canonical default LP): Production-grade LP/QP/MIP, always compiled in, MIT license
//! - **Canonical default NLP policy**: best available runtime NLP backend for the solve class
//! - **Ipopt**: Interior-point NLP for AC-OPF, runtime-detected via dlopen, EPL license
//! - **Gurobi**: Commercial LP/QP/MIP, runtime-detected via dlopen, requires valid license
//! - **COPT**: Commercial LP/QP/MIP and NLP, runtime-detected; NLP uses the
//!   standalone Surge shim and `surge-py` wheels can bundle it automatically
//! - Public top-level exports are the intended solve/runtime/spec entrypoints
//! - Lower-level backend implementations remain available under [`crate::backends`]
//! - No Cargo feature flags needed — all backends are always compiled in; commercial solvers
//!   are detected at runtime via libloading
//!
//! Canonical public naming:
//! - `DcOpfOptions`, `DcOpfRuntime`, `DcOpfResult`
//! - `AcOpfOptions`, `AcOpfRuntime`, `AcOpfResult`
//! - `ScopfOptions`, `ScopfRuntime`, `ScopfResult`
//! - specialist studies under [`crate::switching`]

// ── Module tree ──────────────────────────────────────────────────────────
/// AC optimal power flow (nonlinear NLP formulation).
pub mod ac;
/// Pluggable LP/QP/MIP and NLP solver backends.
pub mod backends;
pub(crate) mod common;
/// DC optimal power flow (linear/QP formulation).
pub mod dc;
/// Discrete round-and-check verification for transformer taps and switched shunts.
pub mod discrete;
/// Nonlinear programming trait and types used by NLP backends.
pub mod nlp;
/// Security-constrained optimal power flow (SCOPF).
pub mod security;
/// Topology optimization (OTS, ORPD).
pub mod switching;

// ── Public re-exports ────────────────────────────────────────────────────

/// Advanced lower-level helpers, sparse builders, and formulation internals.
///
/// These APIs are public for power users and adjacent Surge crates, but are
/// intentionally segregated from the ergonomic crate root.
pub mod advanced {
    pub use crate::dc::{
        IslandRefs, compute_dc_loss_sensitivities, decompose_lmp_lossless,
        decompose_lmp_with_losses, detect_island_refs, find_split_ref_bus, fix_island_theta_bounds,
        solve_dc_opf_lp, solve_dc_opf_lp_with_runtime, triplets_to_csc,
    };
}

// Topology optimization (OTS, ORPD)
pub use switching::{
    OrpdObjective, OrpdOptions, OrpdResult, OtsFormulation, OtsOptions, OtsResult, OtsRuntime,
    SwitchableSet, solve_orpd, solve_ots, solve_ots_with_runtime,
};

// AC-OPF
pub use ac::{
    AcObjectiveTargetTracking, AcOpfBendersSubproblem, AcOpfError, AcOpfOptions, AcOpfRuntime,
    BendersCut, DiscreteMode, WarmStart, compute_ac_marginal_loss_factors, solve_ac_opf,
    solve_ac_opf_subproblem, solve_ac_opf_with_runtime,
};

// DC-OPF
pub use dc::{
    DcOpfError, DcOpfOptions, DcOpfResult, DcOpfRuntime, HvdcOpfLink, compute_total_dc_losses,
    solve_dc_opf, solve_dc_opf_with_runtime,
};

// SCOPF
pub use security::{
    BindingContingency, ContingencyViolation, FailedContingencyEvaluation, ScopfAcSettings,
    ScopfCorrectiveSettings, ScopfError, ScopfFormulation, ScopfMode, ScopfOptions, ScopfResult,
    ScopfRuntime, ScopfScreeningPolicy, ScopfScreeningStats, ScopfWarmStart, ThermalRating,
    solve_scopf, solve_scopf_with_runtime,
};

#[cfg(test)]
pub(crate) mod test_util;
