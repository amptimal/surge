// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::fmt;

use serde::{Deserialize, Serialize};

// ── Transfer path and flowgate definitions ───────────────────────────────────

/// Directional transfer path used by ATC, AFC, and simultaneous transfer studies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferPath {
    /// Human-readable path name.
    pub name: String,
    /// External bus numbers injecting power.
    pub source_buses: Vec<u32>,
    /// External bus numbers withdrawing power.
    pub sink_buses: Vec<u32>,
}

impl TransferPath {
    pub fn new(name: impl Into<String>, source_buses: Vec<u32>, sink_buses: Vec<u32>) -> Self {
        Self {
            name: name.into(),
            source_buses,
            sink_buses,
        }
    }
}

/// Flowgate definition for AFC studies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Flowgate {
    /// Human-readable flowgate name.
    pub name: String,
    /// Monitored branch index in the network.
    pub monitored_branch: usize,
    /// Optional contingency branch index for N-1 flowgates.
    pub contingency_branch: Option<usize>,
    /// Base-case N-0 flowgate rating in MW.
    pub normal_rating_mw: f64,
    /// Optional post-contingency N-1 rating in MW.
    ///
    /// When absent, the normal rating is reused for the N-1 state.
    pub contingency_rating_mw: Option<f64>,
}

impl Flowgate {
    pub fn new(
        name: impl Into<String>,
        monitored_branch: usize,
        contingency_branch: Option<usize>,
        normal_rating_mw: f64,
        contingency_rating_mw: Option<f64>,
    ) -> Self {
        Self {
            name: name.into(),
            monitored_branch,
            contingency_branch,
            normal_rating_mw,
            contingency_rating_mw,
        }
    }

    pub fn effective_contingency_rating_mw(&self) -> f64 {
        self.contingency_rating_mw.unwrap_or(self.normal_rating_mw)
    }
}

// ── NERC ATC margins and options ─────────────────────────────────────────────

/// NERC ATC margin parameters.
///
/// Defaults follow the repo's conservative ATC screening convention rather than a
/// published MOD-029/MOD-030 utility-specific tariff profile:
/// - TRM = 5 % of TTC (uncertainty allowance).
/// - CBM = 0 MW (no firm capacity service reservation).
/// - ETC = 0 MW (no pre-existing commitments).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AtcMargins {
    /// Transmission Reliability Margin as a fraction of TTC (0.0–1.0).
    /// Default: 0.05 (5 %).
    pub trm_fraction: f64,
    /// Capacity Benefit Margin in MW (absolute). Default: 0.0.
    pub cbm_mw: f64,
    /// Existing Transmission Commitments in MW (absolute). Default: 0.0.
    pub etc_mw: f64,
}

impl Default for AtcMargins {
    fn default() -> Self {
        Self {
            trm_fraction: 0.05,
            cbm_mw: 0.0,
            etc_mw: 0.0,
        }
    }
}

/// Options for NERC ATC screening.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AtcOptions {
    /// Branches monitored for transfer headroom. `None` uses all in-service rated branches.
    pub monitored_branches: Option<Vec<usize>>,
    /// Outage branches for the N-1 pass. `None` disables N-1 screening.
    pub contingency_branches: Option<Vec<usize>>,
    /// NERC margin deductions.
    pub margins: AtcMargins,
}

// ── Request types ────────────────────────────────────────────────────────────

/// Canonical request for NERC ATC studies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NercAtcRequest {
    pub path: TransferPath,
    #[serde(default)]
    pub options: AtcOptions,
}

/// Canonical request for AC-aware ATC studies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcAtcRequest {
    pub path: TransferPath,
    pub v_min_pu: f64,
    pub v_max_pu: f64,
}

impl AcAtcRequest {
    pub fn new(path: TransferPath, v_min_pu: f64, v_max_pu: f64) -> Self {
        Self {
            path,
            v_min_pu,
            v_max_pu,
        }
    }
}

/// Canonical request for AFC studies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AfcRequest {
    pub path: TransferPath,
    pub flowgates: Vec<Flowgate>,
}

/// Canonical request for simultaneous transfer studies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiTransferRequest {
    pub paths: Vec<TransferPath>,
    pub weights: Option<Vec<f64>>,
    pub max_transfer_mw: Option<Vec<f64>>,
}

// ── Result types ─────────────────────────────────────────────────────────────

/// Explicit cause of the limiting thermal headroom returned by NERC ATC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NercAtcLimitCause {
    /// No monitored branch constrains the transfer.
    Unconstrained,
    /// The limiting constraint is a base-case thermal limit on a monitored branch.
    BasecaseThermal { monitored_branch: usize },
    /// The limiting constraint is a post-contingency thermal limit.
    ContingencyThermal {
        monitored_branch: usize,
        contingency_branch: usize,
    },
    /// ATC/TTC was forced to zero because an outage invalidated the linear
    /// outage model (for example, islanding / infinite outage sensitivity).
    FailClosedOutage { contingency_branch: usize },
}

impl NercAtcLimitCause {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Unconstrained => "unconstrained",
            Self::BasecaseThermal { .. } => "basecase_thermal",
            Self::ContingencyThermal { .. } => "contingency_thermal",
            Self::FailClosedOutage { .. } => "fail_closed_outage",
        }
    }

    pub fn monitored_branch(self) -> Option<usize> {
        match self {
            Self::BasecaseThermal { monitored_branch }
            | Self::ContingencyThermal {
                monitored_branch, ..
            } => Some(monitored_branch),
            Self::Unconstrained | Self::FailClosedOutage { .. } => None,
        }
    }

    pub fn contingency_branch(self) -> Option<usize> {
        match self {
            Self::ContingencyThermal {
                contingency_branch, ..
            }
            | Self::FailClosedOutage { contingency_branch } => Some(contingency_branch),
            Self::Unconstrained | Self::BasecaseThermal { .. } => None,
        }
    }
}

impl fmt::Display for NercAtcLimitCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind())
    }
}

/// NERC Available Transfer Capability result (MOD-029/MOD-030).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NercAtcResult {
    /// **ATC** in MW (NERC definition): TTC − TRM − CBM − ETC, clamped to 0.
    /// This is the headroom available for new transactions.
    pub atc_mw: f64,

    /// **TTC** (Total Transfer Capability) in MW: raw thermal headroom before
    /// NERC margin deductions. `atc_mw = max(0, ttc_mw − trm_mw − cbm_mw − etc_mw)`.
    pub ttc_mw: f64,

    /// **TRM** applied in MW (Transmission Reliability Margin).
    pub trm_mw: f64,

    /// **CBM** applied in MW (Capacity Benefit Margin).
    pub cbm_mw: f64,

    /// **ETC** applied in MW (Existing Transmission Commitments).
    pub etc_mw: f64,

    /// Explicit cause of the limiting thermal headroom.
    pub limit_cause: NercAtcLimitCause,

    /// Indices of the monitored branches, in the same order as `transfer_ptdf`.
    /// When the caller supplied explicit `monitored_branches`, this is that list;
    /// otherwise it is all in-service branches with positive thermal ratings.
    pub monitored_branches: Vec<usize>,

    /// Transfer PTDFs: sensitivity of each monitored branch flow (MW) to a
    /// 1 MW injection at `source_bus` with a matching withdrawal at `sink_bus`.
    ///
    /// Length equals `monitored_branches.len()`.
    pub transfer_ptdf: Vec<f64>,

    /// `true` when any generator at a bus adjacent to the transfer path is
    /// operating with |Q_g| > 0.70 × Qmax.
    ///
    /// This is a soft warning: the DC model does not model reactive power, but
    /// in a full AC analysis a binding Q-limit could reduce voltage and cause
    /// the thermal rating to bind at a lower transfer level than predicted here.
    pub reactive_margin_warning: bool,
}

impl NercAtcResult {
    pub fn binding_branch(&self) -> Option<usize> {
        self.limit_cause.monitored_branch()
    }

    pub fn binding_contingency(&self) -> Option<usize> {
        self.limit_cause.contingency_branch()
    }
}

/// Which constraint class binds the AC-aware ATC result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcAtcLimitingConstraint {
    Thermal,
    Voltage,
}

impl AcAtcLimitingConstraint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thermal => "thermal",
            Self::Voltage => "voltage",
        }
    }
}

impl fmt::Display for AcAtcLimitingConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcAtcResult {
    /// Available Transfer Capability in MW: the binding constraint from
    /// thermal or voltage limits, whichever is smaller.
    pub atc_mw: f64,
    /// Thermal ATC from DC PTDF (same methodology as `compute_nerc_atc`
    /// but without NERC margin deductions, representing raw TTC).
    pub thermal_limit_mw: f64,
    /// Voltage-limited ATC in MW: the maximum transfer such that all bus
    /// voltages remain in `[v_min_pu, v_max_pu]`. `f64::INFINITY` if the
    /// wide voltage band is never violated.
    pub voltage_limit_mw: f64,
    /// Index of the bus that limits the voltage-constrained transfer.
    /// `None` if the voltage band is not binding.
    pub limiting_bus: Option<usize>,
    /// Index of the branch that limits the thermal-constrained transfer.
    /// `None` if the thermal limit is not binding.
    pub binding_branch: Option<usize>,
    /// Which constraint class is binding.
    pub limiting_constraint: AcAtcLimitingConstraint,
}

/// Available Flowgate Capability for a single [`Flowgate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AfcResult {
    /// Flowgate name (copied from [`Flowgate::name`]).
    pub flowgate_name: String,
    /// Available capability in the same units as the supplied ratings.
    pub afc_mw: f64,
    /// Index of the branch that limits the flowgate.
    pub binding_branch: usize,
    /// Index of the contingency that causes the binding constraint (None if N-0 binding).
    pub binding_contingency: Option<usize>,
}

/// Result of a simultaneous multi-interface transfer analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiTransferResult {
    /// Per-interface transfer capability (MW), same order as input interfaces.
    pub transfer_mw: Vec<f64>,
    /// Per-interface binding branch index (None if unconstrained).
    pub binding_branch: Vec<Option<usize>>,
    /// Objective value (sum of weighted transfers).
    pub total_weighted_transfer: f64,
}
