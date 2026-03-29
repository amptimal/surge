// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Typed CGMES source-object state preserved for faithful round-trip export.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Source CGMES objects that must survive lowering into the native solver model.
///
/// These records preserve the original class identity and any class-specific
/// fields the writer cannot recover from the lowered network alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CgmesRoundtripData {
    /// Original `EquivalentInjection` objects keyed by source mRID.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub equivalent_injections: HashMap<String, CgmesEquivalentInjectionSource>,
    /// Original `ExternalNetworkInjection` objects keyed by source mRID.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub external_network_injections: HashMap<String, CgmesExternalNetworkInjectionSource>,
    /// Original `DanglingLine` objects keyed by source mRID.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub dangling_lines: HashMap<String, CgmesDanglingLineSource>,
}

impl CgmesRoundtripData {
    /// Returns `true` when no CGMES source-object state is present.
    pub fn is_empty(&self) -> bool {
        self.equivalent_injections.is_empty()
            && self.external_network_injections.is_empty()
            && self.dangling_lines.is_empty()
    }
}

/// Original CGMES `EquivalentInjection` preserved across import/export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CgmesEquivalentInjectionSource {
    /// Source mRID.
    pub mrid: String,
    /// Source `IdentifiedObject.name`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Connected bus number.
    pub bus: u32,
    /// Imported operating-point active power (MW).
    pub p_mw: f64,
    /// Imported operating-point reactive power (MVAr).
    pub q_mvar: f64,
    /// Imported in-service status.
    pub in_service: bool,
    /// `RegulatingCondEq.controlEnabled`.
    pub control_enabled: bool,
    /// `EquivalentInjection.regulationStatus`.
    pub regulation_status: bool,
    /// `RegulatingControl.targetValue` in kV, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_voltage_kv: Option<f64>,
    /// Imported minimum reactive limit in MVAr, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_q_mvar: Option<f64>,
    /// Imported maximum reactive limit in MVAr, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_q_mvar: Option<f64>,
}

/// Original CGMES `ExternalNetworkInjection` preserved across import/export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CgmesExternalNetworkInjectionSource {
    /// Source mRID.
    pub mrid: String,
    /// Source `IdentifiedObject.name`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Connected bus number.
    pub bus: u32,
    /// Imported operating-point active power (MW).
    pub p_mw: f64,
    /// Imported operating-point reactive power (MVAr).
    pub q_mvar: f64,
    /// Imported in-service status.
    pub in_service: bool,
    /// Imported slack-selection priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_priority: Option<u32>,
    /// `RegulatingCondEq.controlEnabled`.
    pub control_enabled: bool,
    /// `ExternalNetworkInjection.regulationStatus`.
    pub regulation_status: bool,
    /// `RegulatingControl.targetValue` in kV, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_voltage_kv: Option<f64>,
    /// Imported minimum reactive limit in MVAr, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_q_mvar: Option<f64>,
    /// Imported maximum reactive limit in MVAr, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_q_mvar: Option<f64>,
}

/// Original CGMES `DanglingLine` preserved across import/export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CgmesDanglingLineSource {
    /// Source mRID.
    pub mrid: String,
    /// Source `IdentifiedObject.name`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Connected bus number.
    pub bus: u32,
    /// Imported operating-point active power (MW).
    pub p_mw: f64,
    /// Imported operating-point reactive power (MVAr).
    pub q_mvar: f64,
    /// Imported in-service status.
    pub in_service: bool,
    /// Series resistance in ohms, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r_ohm: Option<f64>,
    /// Series reactance in ohms, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_ohm: Option<f64>,
    /// Shunt conductance in Siemens.
    pub g_s: f64,
    /// Shunt susceptance in Siemens.
    pub b_s: f64,
}
