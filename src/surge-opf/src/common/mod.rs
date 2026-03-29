// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared internal preprocessing and indexing helpers for OPF solver families.

pub(crate) mod context;

/// Generates the common error variants shared by all OPF error types.
///
/// Invoke as: `opf_common_errors!(DcOpfError { ...extra variants... });`
/// The macro emits the `#[derive(Debug, thiserror::Error)]` enum with the five
/// shared variants plus any extra variants supplied by the caller.
macro_rules! opf_common_errors {
    ($name:ident { $($extra:tt)* }) => {
        #[derive(Debug, thiserror::Error)]
        pub enum $name {
            /// Network failed validation (e.g. missing buses, invalid topology).
            #[error("invalid network: {0}")]
            InvalidNetwork(String),

            /// No slack (reference) bus found in the network.
            #[error("no slack bus in network")]
            NoSlackBus,

            /// No in-service generators available for dispatch.
            #[error("no in-service generators")]
            NoGenerators,

            /// A generator is missing its cost curve (required for objective function).
            #[error("generator {gen_idx} (bus {bus}) has no cost curve")]
            MissingCost { gen_idx: usize, bus: u32 },

            /// The solver encountered an internal error.
            #[error("solver failed: {0}")]
            SolverError(String),

            $($extra)*
        }
    };
}

pub(crate) use opf_common_errors;

/// Generates `From<$source> for $target` covering the five common OPF error
/// variants plus caller-supplied extra arms for type-specific variants.
///
/// Usage:
/// ```ignore
/// impl_opf_error_from!(DcOpfError => ScopfError {
///     DcOpfError::InsufficientCapacity { load_mw, capacity_mw }
///         => ScopfError::InsufficientCapacity { load_mw, capacity_mw },
///     DcOpfError::NotConverged { iterations }
///         => ScopfError::NotConverged { iterations },
/// });
/// ```
macro_rules! impl_opf_error_from {
    ($source:ident => $target:ident { $($extra_arm:tt)* }) => {
        impl From<$source> for $target {
            fn from(e: $source) -> Self {
                match e {
                    $source::InvalidNetwork(s) => $target::InvalidNetwork(s),
                    $source::NoSlackBus => $target::NoSlackBus,
                    $source::NoGenerators => $target::NoGenerators,
                    $source::MissingCost { gen_idx, bus } => $target::MissingCost { gen_idx, bus },
                    $source::SolverError(s) => $target::SolverError(s),
                    $($extra_arm)*
                }
            }
        }
    };
}

pub(crate) use impl_opf_error_from;
