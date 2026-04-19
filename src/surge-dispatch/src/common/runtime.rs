// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared per-period execution context for sequential dispatch paths.

use std::collections::HashMap;

use surge_network::network::Generator;

use crate::dispatch::IndexedDispatchInitialState;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DispatchPeriodContext<'a> {
    pub period: usize,
    pub prev_dispatch_mw: Option<&'a [f64]>,
    pub prev_dispatch_mask: Option<&'a [bool]>,
    pub prev_hvdc_dispatch_mw: Option<&'a [f64]>,
    pub prev_hvdc_dispatch_mask: Option<&'a [bool]>,
    pub storage_soc_override: Option<&'a HashMap<usize, f64>>,
    pub next_period_commitment: Option<&'a [bool]>,
}

impl<'a> DispatchPeriodContext<'a> {
    pub fn initial(initial_state: &'a IndexedDispatchInitialState) -> Self {
        Self {
            period: 0,
            prev_dispatch_mw: initial_state.prev_dispatch_mw.as_deref(),
            prev_dispatch_mask: initial_state.prev_dispatch_mask.as_deref(),
            prev_hvdc_dispatch_mw: initial_state.prev_hvdc_dispatch_mw.as_deref(),
            prev_hvdc_dispatch_mask: initial_state.prev_hvdc_dispatch_mask.as_deref(),
            storage_soc_override: initial_state.storage_soc_override.as_ref(),
            next_period_commitment: None,
        }
    }

    pub fn has_prev_dispatch(&self) -> bool {
        match (self.prev_dispatch_mw, self.prev_dispatch_mask) {
            (Some(_), None) => true,
            (Some(_), Some(mask)) => mask.iter().any(|&present| present),
            (None, _) => false,
        }
    }

    pub fn prev_dispatch_at(&self, idx: usize) -> Option<f64> {
        let values = self.prev_dispatch_mw?;
        if let Some(mask) = self.prev_dispatch_mask
            && !mask.get(idx).copied().unwrap_or(false)
        {
            return None;
        }
        values.get(idx).copied()
    }

    pub fn prev_hvdc_dispatch_at(&self, idx: usize) -> Option<f64> {
        let values = self.prev_hvdc_dispatch_mw?;
        if let Some(mask) = self.prev_hvdc_dispatch_mask
            && !mask.get(idx).copied().unwrap_or(false)
        {
            return None;
        }
        values.get(idx).copied()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SequentialDispatchState {
    pub prev_dispatch_mw: Option<Vec<f64>>,
    pub prev_dispatch_mask: Option<Vec<bool>>,
    pub prev_hvdc_dispatch_mw: Option<Vec<f64>>,
    pub prev_hvdc_dispatch_mask: Option<Vec<bool>>,
    pub storage_soc_override: Option<HashMap<usize, f64>>,
}

impl SequentialDispatchState {
    pub fn from_initial_state(initial_state: &IndexedDispatchInitialState) -> Self {
        Self {
            prev_dispatch_mw: initial_state.prev_dispatch_mw.clone(),
            prev_dispatch_mask: initial_state.prev_dispatch_mask.clone(),
            prev_hvdc_dispatch_mw: initial_state.prev_hvdc_dispatch_mw.clone(),
            prev_hvdc_dispatch_mask: initial_state.prev_hvdc_dispatch_mask.clone(),
            storage_soc_override: initial_state.storage_soc_override.clone(),
        }
    }

    pub fn period_context<'a>(
        &'a self,
        period: usize,
        next_period_commitment: Option<&'a [bool]>,
    ) -> DispatchPeriodContext<'a> {
        DispatchPeriodContext {
            period,
            prev_dispatch_mw: self.prev_dispatch_mw.as_deref(),
            prev_dispatch_mask: self.prev_dispatch_mask.as_deref(),
            prev_hvdc_dispatch_mw: self.prev_hvdc_dispatch_mw.as_deref(),
            prev_hvdc_dispatch_mask: self.prev_hvdc_dispatch_mask.as_deref(),
            storage_soc_override: self.storage_soc_override.as_ref(),
            next_period_commitment,
        }
    }
}

pub(crate) fn effective_storage_soc_mwh(
    storage_soc_override: Option<&HashMap<usize, f64>>,
    gen_index: usize,
    generator: &Generator,
) -> f64 {
    if let Some(overrides) = storage_soc_override
        && let Some(&soc) = overrides.get(&gen_index)
    {
        return soc;
    }
    generator
        .storage
        .as_ref()
        .map(|s| s.soc_initial_mwh)
        .unwrap_or(0.0)
}
