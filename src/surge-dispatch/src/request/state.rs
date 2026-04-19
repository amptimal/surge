// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Initial and carried state for dispatch requests.

use schemars::JsonSchema;

/// Previous dispatch point for one resource.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceDispatchPoint {
    pub resource_id: String,
    pub mw: f64,
}

/// Previous dispatch point for one HVDC link.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HvdcDispatchPoint {
    pub link_id: String,
    pub mw: f64,
}

/// Initial storage state override for one storage resource.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageSocOverride {
    pub resource_id: String,
    pub soc_mwh: f64,
}

/// Initial dispatch state for sequential or horizon-start solves.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct DispatchInitialState {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_resource_dispatch: Vec<ResourceDispatchPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_hvdc_dispatch: Vec<HvdcDispatchPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_soc_overrides: Vec<StorageSocOverride>,
}

/// Initial state carried into the study.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct DispatchState {
    pub initial: DispatchInitialState,
}
