// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Prepared dispatch model.
//!
//! This is the explicit boundary between a mutable/raw [`surge_network::Network`]
//! and a dispatch-ready study model with canonical generator identities.

use surge_network::Network;

use crate::{
    DispatchError, DispatchRequest, DispatchSolution, DispatchSolveOptions,
    PreparedDispatchRequest, solve_dispatch, solve_dispatch_with_options,
};

/// Dispatch-ready network model with canonical generator identities.
///
/// Create this once with [`DispatchModel::prepare`] and reuse it across
/// multiple dispatch requests. Preparation clones the source network, assigns
/// canonical generator ids where needed, and validates the resulting model.
#[derive(Debug, Clone)]
pub struct DispatchModel {
    network: Network,
}

impl DispatchModel {
    /// Prepare a dispatch model from a raw network.
    pub fn prepare(network: &Network) -> Result<Self, DispatchError> {
        let mut network = network.clone();
        network.canonicalize_generator_ids();
        network
            .validate()
            .map_err(|error| DispatchError::InvalidInput(format!("invalid network: {error}")))?;
        Ok(Self { network })
    }

    /// Access the validated canonical network used by dispatch.
    pub fn network(&self) -> &Network {
        &self.network
    }

    /// Prepare and fully validate a request against this model.
    pub fn prepare_request(
        &self,
        request: &DispatchRequest,
    ) -> Result<PreparedDispatchRequest, DispatchError> {
        self.prepare_request_with_options(request, &DispatchSolveOptions::default())
    }

    /// Prepare and fully validate a request against this model using process-local options.
    pub fn prepare_request_with_options(
        &self,
        request: &DispatchRequest,
        solve_options: &DispatchSolveOptions,
    ) -> Result<PreparedDispatchRequest, DispatchError> {
        crate::dispatch::prepare_dispatch_request(self, request, solve_options)
    }

    /// Fully validate a request against this model without solving it.
    pub fn validate_request(&self, request: &DispatchRequest) -> Result<(), DispatchError> {
        self.prepare_request(request).map(|_| ())
    }

    /// Solve a dispatch study against this prepared model.
    pub fn solve(&self, request: &DispatchRequest) -> Result<DispatchSolution, DispatchError> {
        solve_dispatch(self, request)
    }

    /// Solve a previously prepared request against this model.
    pub fn solve_prepared(
        &self,
        prepared: &PreparedDispatchRequest,
    ) -> Result<DispatchSolution, DispatchError> {
        crate::dispatch::solve_prepared_dispatch(self, prepared)
    }

    /// Solve a dispatch study with process-local execution options.
    pub fn solve_with_options(
        &self,
        request: &DispatchRequest,
        solve_options: &DispatchSolveOptions,
    ) -> Result<DispatchSolution, DispatchError> {
        solve_dispatch_with_options(self, request, solve_options)
    }
}
