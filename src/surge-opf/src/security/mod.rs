// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Unified SCOPF entry point.
//!
//! Dispatches to the appropriate solver based on [`ScopfOptions::formulation`]
//! and [`ScopfOptions::mode`]:
//!
//! | Formulation | Mode | Algorithm |
//! |---|---|---|
//! | DC | Preventive | Iterative cutting-plane (LODF + LP) |
//! | DC | Corrective | Extensive-form LP with per-contingency redispatch |
//! | AC | Preventive | Benders decomposition (AC-OPF master + NR subproblems) |
//! | AC | Corrective | Not yet supported |

pub(crate) mod ac;
pub(crate) mod dc;
pub(crate) mod dc_contingencies;
pub(crate) mod dc_model;
pub(crate) mod dc_support;
pub mod types;

use surge_network::Network;

pub use self::types::*;

/// Solve Security-Constrained Optimal Power Flow.
///
/// Single entry point for all SCOPF variants. Select the problem via
/// `options.formulation` (DC or AC) and `options.mode` (Preventive or Corrective).
pub fn solve_scopf(network: &Network, options: &ScopfOptions) -> Result<ScopfResult, ScopfError> {
    solve_scopf_with_runtime(network, options, &ScopfRuntime::default())
}

/// Solve Security-Constrained Optimal Power Flow with explicit runtime controls.
pub fn solve_scopf_with_runtime(
    network: &Network,
    options: &ScopfOptions,
    runtime: &ScopfRuntime,
) -> Result<ScopfResult, ScopfError> {
    let mut network = network.clone();
    network.canonicalize_runtime_identities();
    network
        .validate()
        .map_err(|e| ScopfError::InvalidNetwork(e.to_string()))?;
    let has_corridor_constraints = !network.flowgates.is_empty() || !network.interfaces.is_empty();
    let context = ScopfRunContext {
        runtime: runtime.clone(),
    };
    match (options.formulation, options.mode) {
        (ScopfFormulation::Dc, ScopfMode::Preventive) => {
            self::dc::solve_dc_preventive_with_context(&network, options, &context)
                .map_err(ScopfError::from)
        }
        (ScopfFormulation::Dc, ScopfMode::Corrective) => {
            if options.enforce_flowgates && has_corridor_constraints {
                return Err(ScopfError::UnsupportedSecurityConstraint {
                    detail: "DC corrective SCOPF does not yet model flowgate/interface security constraints; use preventive SCOPF or base-case OPF for corridor enforcement".to_string(),
                });
            }
            self::dc::solve_dc_corrective_with_context(&network, options, &context)
                .map_err(ScopfError::from)
        }
        (ScopfFormulation::Ac, ScopfMode::Preventive) => {
            self::ac::solve_ac_preventive_with_context(&network, options, &context)
        }
        (ScopfFormulation::Ac, ScopfMode::Corrective) => Err(ScopfError::UnsupportedCombination {
            formulation: ScopfFormulation::Ac,
            mode: ScopfMode::Corrective,
        }),
    }
}
