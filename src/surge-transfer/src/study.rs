// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::sync::Arc;

use surge_dc::{DcPfOptions, PreparedDcStudy};
use surge_network::Network;

use crate::dfax::PreparedTransferModel;
use crate::error::TransferError;
use crate::injection::{InjectionCapabilityMap, InjectionCapabilityOptions};
use crate::types::{
    AcAtcRequest, AcAtcResult, AfcRequest, AfcResult, MultiTransferRequest, MultiTransferResult,
    NercAtcRequest, NercAtcResult,
};

/// Owning transfer study surface for Rust and Python callers.
///
/// Caches base-case DC flows so they aren't re-solved per study.
/// Each method call does re-factorize the B' matrix because
/// `PreparedDcStudy<'a>` borrows `&'a Network` and cannot be cached
/// alongside the owned `Arc<Network>`. For batch work where this
/// overhead matters, use [`PreparedTransferModel`]
/// with a borrowed `&Network` directly.
pub struct TransferStudy {
    network: Arc<Network>,
    base_flows_pu: Vec<f64>,
}

impl TransferStudy {
    pub fn new(network: &Network) -> Result<Self, TransferError> {
        let network = Arc::new(network.clone());
        let mut dc_model = PreparedDcStudy::new(network.as_ref())?;
        let base_flows_pu = dc_model.solve(&DcPfOptions::default())?.branch_p_flow;
        Ok(Self {
            network,
            base_flows_pu,
        })
    }

    pub fn network(&self) -> &Network {
        self.network.as_ref()
    }

    fn prepared_model(&self) -> Result<PreparedTransferModel<'_>, TransferError> {
        PreparedTransferModel::from_base_state(self.network.as_ref(), self.base_flows_pu.clone())
            .map_err(Into::into)
    }

    pub fn compute_nerc_atc(
        &self,
        request: &NercAtcRequest,
    ) -> Result<NercAtcResult, TransferError> {
        let mut prepared = self.prepared_model()?;
        prepared.compute_nerc_atc(request)
    }

    pub fn compute_ac_atc(&self, request: &AcAtcRequest) -> Result<AcAtcResult, TransferError> {
        crate::ac_atc::compute_ac_atc(self.network.as_ref(), request)
    }

    pub fn compute_afc(&self, request: &AfcRequest) -> Result<Vec<AfcResult>, TransferError> {
        let mut prepared = self.prepared_model()?;
        prepared.compute_afc(request)
    }

    pub fn compute_multi_transfer(
        &self,
        request: &MultiTransferRequest,
    ) -> Result<MultiTransferResult, TransferError> {
        let mut prepared = self.prepared_model()?;
        prepared.compute_multi_transfer(request)
    }

    pub fn compute_injection_capability(
        &self,
        options: &InjectionCapabilityOptions,
    ) -> Result<InjectionCapabilityMap, TransferError> {
        let mut prepared = self.prepared_model()?;
        prepared.compute_injection_capability(options)
    }
}
