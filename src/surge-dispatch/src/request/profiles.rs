// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Time-series profile inputs for dispatch requests.

use schemars::JsonSchema;
use surge_network::market as indexed;

use crate::request::BranchRef;

/// Active-power load profile for one bus.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BusLoadProfile {
    /// External bus number (matches `Bus::number`).
    #[serde(alias = "bus")]
    pub bus_number: u32,
    /// Active-power demand in MW.
    #[serde(alias = "load_mw")]
    pub values_mw: Vec<f64>,
}

/// Collection of active-power bus load profiles.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct BusLoadProfiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<BusLoadProfile>,
}

impl BusLoadProfiles {
    pub(crate) fn to_indexed(&self, n_periods: usize) -> indexed::LoadProfiles {
        indexed::LoadProfiles {
            profiles: self
                .profiles
                .iter()
                .map(|profile| indexed::LoadProfile {
                    bus: profile.bus_number,
                    load_mw: profile.values_mw.clone(),
                })
                .collect(),
            n_timesteps: n_periods,
        }
    }
}

impl From<indexed::LoadProfiles> for BusLoadProfiles {
    fn from(value: indexed::LoadProfiles) -> Self {
        Self {
            profiles: value
                .profiles
                .into_iter()
                .map(|profile| BusLoadProfile {
                    bus_number: profile.bus,
                    values_mw: profile.load_mw,
                })
                .collect(),
        }
    }
}

/// Optional AC-only bus load override profile.
///
/// This augments the standard active-power-only [`BusLoadProfiles`] surface:
///
/// - `p_mw = Some(..)`, `q_mvar = None`: override active load and preserve the
///   base bus reactive power factor.
/// - `p_mw = None`, `q_mvar = Some(..)`: keep active load and override only
///   reactive demand.
/// - `p_mw = Some(..)`, `q_mvar = Some(..)`: override both active and reactive
///   bus demand explicitly.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcBusLoadProfile {
    /// External bus number (matches `Bus::number`).
    #[serde(alias = "bus")]
    pub bus_number: u32,
    /// Optional active-power demand profile in MW.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_mw: Option<Vec<f64>>,
    /// Optional reactive-power demand profile in MVAr.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_mvar: Option<Vec<f64>>,
}

/// Collection of AC-only bus load overrides.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AcBusLoadProfiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<AcBusLoadProfile>,
}

/// Renewable capacity-factor profile keyed by dispatch `resource_id`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenewableProfile {
    #[serde(alias = "generator_id")]
    pub resource_id: String,
    pub capacity_factors: Vec<f64>,
}

/// Collection of renewable capacity-factor profiles.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct RenewableProfiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<RenewableProfile>,
}

impl RenewableProfiles {
    pub(crate) fn to_indexed(&self, n_periods: usize) -> indexed::RenewableProfiles {
        indexed::RenewableProfiles {
            profiles: self
                .profiles
                .iter()
                .map(|profile| indexed::RenewableProfile {
                    generator_id: profile.resource_id.clone(),
                    capacity_factors: profile.capacity_factors.clone(),
                })
                .collect(),
            n_timesteps: n_periods,
        }
    }
}

impl From<indexed::RenewableProfiles> for RenewableProfiles {
    fn from(value: indexed::RenewableProfiles) -> Self {
        Self {
            profiles: value
                .profiles
                .into_iter()
                .map(|profile| RenewableProfile {
                    resource_id: profile.generator_id,
                    capacity_factors: profile.capacity_factors,
                })
                .collect(),
        }
    }
}

/// Generator derate profile keyed by dispatch `resource_id`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeneratorDerateProfile {
    #[serde(alias = "generator_id")]
    pub resource_id: String,
    pub derate_factors: Vec<f64>,
}

/// Collection of generator derate profiles.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct GeneratorDerateProfiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<GeneratorDerateProfile>,
}

impl GeneratorDerateProfiles {
    pub(crate) fn to_indexed(&self, n_periods: usize) -> indexed::GeneratorDerateProfiles {
        indexed::GeneratorDerateProfiles {
            profiles: self
                .profiles
                .iter()
                .map(|profile| indexed::GeneratorDerateProfile {
                    generator_id: profile.resource_id.clone(),
                    derate_factors: profile.derate_factors.clone(),
                })
                .collect(),
            n_timesteps: n_periods,
        }
    }
}

impl From<indexed::GeneratorDerateProfiles> for GeneratorDerateProfiles {
    fn from(value: indexed::GeneratorDerateProfiles) -> Self {
        Self {
            profiles: value
                .profiles
                .into_iter()
                .map(|profile| GeneratorDerateProfile {
                    resource_id: profile.generator_id,
                    derate_factors: profile.derate_factors,
                })
                .collect(),
        }
    }
}

/// Absolute generator dispatch bounds keyed by dispatch `resource_id`.
///
/// Unlike derates, these bounds specify the per-period physical dispatch window
/// directly in MW and are applied before each SCED/SCUC network snapshot is
/// built. This is the right surface for resources whose availability floor and
/// ceiling both vary over time, such as fixed-profile renewable injections or
/// externally supplied must-take schedules.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeneratorDispatchBoundsProfile {
    #[serde(alias = "generator_id")]
    pub resource_id: String,
    pub p_min_mw: Vec<f64>,
    pub p_max_mw: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_min_mvar: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_max_mvar: Option<Vec<f64>>,
}

/// Collection of absolute generator dispatch bounds.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct GeneratorDispatchBoundsProfiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<GeneratorDispatchBoundsProfile>,
}

/// Branch derate profile keyed by a stable branch selector.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BranchDerateProfile {
    #[serde(flatten)]
    pub branch: BranchRef,
    pub derate_factors: Vec<f64>,
}

/// Collection of branch derate profiles.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct BranchDerateProfiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<BranchDerateProfile>,
}

impl BranchDerateProfiles {
    pub(crate) fn to_indexed(&self, n_periods: usize) -> indexed::BranchDerateProfiles {
        indexed::BranchDerateProfiles {
            profiles: self
                .profiles
                .iter()
                .map(|profile| indexed::BranchDerateProfile {
                    from_bus: profile.branch.from_bus,
                    to_bus: profile.branch.to_bus,
                    circuit: profile.branch.circuit.clone(),
                    derate_factors: profile.derate_factors.clone(),
                })
                .collect(),
            n_timesteps: n_periods,
        }
    }
}

impl From<indexed::BranchDerateProfiles> for BranchDerateProfiles {
    fn from(value: indexed::BranchDerateProfiles) -> Self {
        Self {
            profiles: value
                .profiles
                .into_iter()
                .map(|profile| BranchDerateProfile {
                    branch: BranchRef {
                        from_bus: profile.from_bus,
                        to_bus: profile.to_bus,
                        circuit: profile.circuit,
                    },
                    derate_factors: profile.derate_factors,
                })
                .collect(),
        }
    }
}

/// HVDC derate profile keyed by dispatch `link_id`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HvdcDerateProfile {
    #[serde(alias = "name")]
    pub link_id: String,
    pub derate_factors: Vec<f64>,
}

/// Collection of HVDC derate profiles.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct HvdcDerateProfiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<HvdcDerateProfile>,
}

impl HvdcDerateProfiles {
    pub(crate) fn to_indexed(&self, n_periods: usize) -> indexed::HvdcDerateProfiles {
        indexed::HvdcDerateProfiles {
            profiles: self
                .profiles
                .iter()
                .map(|profile| indexed::HvdcDerateProfile {
                    name: profile.link_id.clone(),
                    derate_factors: profile.derate_factors.clone(),
                })
                .collect(),
            n_timesteps: n_periods,
        }
    }
}

impl From<indexed::HvdcDerateProfiles> for HvdcDerateProfiles {
    fn from(value: indexed::HvdcDerateProfiles) -> Self {
        Self {
            profiles: value
                .profiles
                .into_iter()
                .map(|profile| HvdcDerateProfile {
                    link_id: profile.name,
                    derate_factors: profile.derate_factors,
                })
                .collect(),
        }
    }
}

/// Time-series profiles and derates applied during the study.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct DispatchProfiles {
    pub load: BusLoadProfiles,
    pub ac_bus_load: AcBusLoadProfiles,
    pub renewable: RenewableProfiles,
    pub generator_derates: GeneratorDerateProfiles,
    pub generator_dispatch_bounds: GeneratorDispatchBoundsProfiles,
    pub branch_derates: BranchDerateProfiles,
    pub hvdc_derates: HvdcDerateProfiles,
}
