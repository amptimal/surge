// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared context produced by the GO C3 adapter.
//!
//! [`GoC3Context`] is the Rust equivalent of Python
//! `markets/go_c3/adapter.py::AdapterContext`. It is accumulated across the
//! structural → enrichment → reserves → commitment pipeline and preserves
//! everything the dispatch-request builder and solution exporter need:
//!
//! * **UID↔index maps** — translate GO C3 string UIDs to Surge's numeric
//!   bus numbers and resource IDs both ways.
//! * **Initial state** — per-branch/transformer/DC-line state captured at
//!   `to_network` time so the request builder can emit initial-condition
//!   rows without re-reading the problem.
//! * **Synthetic resources** — resource IDs the adapter invented (e.g. DC
//!   line reactive-support "producer_static" generators) and their
//!   scheduling metadata.
//! * **Policy-driven decisions** — the slack bus the adapter picked, which
//!   generators were flagged as voltage-regulating, which reserve products
//!   were registered.
//! * **Issue log** — structured warnings and errors for the solve report.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::issues::{GoC3Issue, GoC3IssueSeverity};

/// Classification of a GO C3 device after it has been mapped into Surge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoC3DeviceKind {
    /// Regular producer with time-varying active power bounds.
    Producer,
    /// Producer that has zero active-power capability across the horizon.
    /// Kept in the network for topology/voltage bookkeeping but excluded
    /// from active dispatch and commitment optimization.
    ProducerStatic,
    /// Consumer (load).
    Consumer,
}

/// Initial state of an AC line captured at network-build time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcLineInitialState {
    pub on_status: i32,
}

/// Initial state of a transformer captured at network-build time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransformerInitialState {
    pub on_status: i32,
    pub tm: f64,
    pub ta: f64,
}

/// Initial state of a DC line captured at network-build time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DcLineInitialState {
    pub pdc_fr: f64,
    pub qdc_fr: f64,
    pub qdc_to: f64,
}

/// Per-DC-line reactive-power bounds (pu) captured from the problem input.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DcLineReactiveBounds {
    pub qdc_fr_lb: f64,
    pub qdc_fr_ub: f64,
    pub qdc_to_lb: f64,
    pub qdc_to_ub: f64,
}

/// Resource IDs for the synthetic "reactive support producer" generators
/// the adapter adds at each terminal of a DC line when running in AC mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DcLineReactiveSupportResources {
    /// Resource ID of the `fr`-terminal reactive-support generator.
    pub fr: String,
    /// Resource ID of the `to`-terminal reactive-support generator.
    pub to: String,
}

/// Branch reference — enough to uniquely identify a branch in the built
/// network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchRef {
    pub from_bus: u32,
    pub to_bus: u32,
    pub circuit: String,
}

/// Shared context produced by the adapter pipeline.
///
/// Construction order, roughly following the Python adapter:
///
/// 1. `to_network` in `network.rs` populates UID maps and initial state.
/// 2. `enrich.rs` layers on operational metadata (generator bounds,
///    commitment params, slack inference).
/// 3. `reserves.rs` registers reserve products and wires device offers.
/// 4. `voltage.rs` marks voltage-regulating resources and records the
///    slack fallback.
/// 5. `consumers.rs` (policy-dependent) may add dispatchable load resource
///    IDs.
/// 6. `hvdc_q.rs` (AC mode only) adds DC line reactive-support resources.
///
/// The dispatch-request builder (`surge-dispatch::go_c3`) reads this struct
/// and the original `GoC3Problem` to produce a `DispatchRequest`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoC3Context {
    // ── UID ↔ index maps ────────────────────────────────────────────────
    pub bus_uid_to_number: HashMap<String, u32>,
    pub bus_number_to_uid: HashMap<u32, String>,
    pub device_uid_to_id: HashMap<String, (u32, String)>,
    pub branch_uid_to_ref: HashMap<String, BranchRef>,
    /// Integer circuit ID assigned by the structural pass. Python stores
    /// this as `int` in its `AdapterContext.branch_circuit_by_uid`; we
    /// serialize as `u32` for parity with the Python adapter's downstream
    /// consumers (some of which compare against `int` literals).
    pub branch_circuit_by_uid: HashMap<String, u32>,
    pub branch_local_index_by_uid: HashMap<String, usize>,
    pub dc_line_uid_to_name: HashMap<String, String>,
    pub shunt_uid_to_id: HashMap<String, (u32, String)>,

    // ── Per-device classification ───────────────────────────────────────
    pub device_kind_by_uid: HashMap<String, GoC3DeviceKind>,

    // ── Initial state (captured once, read many) ────────────────────────
    pub ac_line_initial: HashMap<String, AcLineInitialState>,
    pub transformer_initial: HashMap<String, TransformerInitialState>,
    pub dc_line_initial: HashMap<String, DcLineInitialState>,
    pub dc_line_q_bounds: HashMap<String, DcLineReactiveBounds>,

    // ── Shunt / transformer bounds ──────────────────────────────────────
    pub shunt_initial_steps: HashMap<String, i32>,
    pub shunt_step_bounds: HashMap<String, (i32, i32)>,
    pub switched_shunt_control_id_to_uid: BTreeMap<String, String>,
    pub transformer_tap_bounds: HashMap<String, (f64, f64)>,

    // ── Synthetic resources (DC line reactive support) ──────────────────
    pub dc_line_reactive_support_resource_ids: HashMap<String, DcLineReactiveSupportResources>,
    /// Map from a synthetic resource ID → (dc_line_uid, output_slot) where
    /// `output_slot` is `"qdc_fr"` or `"qdc_to"`. Used by the solution
    /// exporter to route AC reactive awards back to DC line terminals.
    pub dc_line_reactive_support_resource_to_output: HashMap<String, (String, String)>,
    /// Per-synthetic-resource per-period commitment schedule so the SCUC
    /// request can pin these resources on only during periods when the
    /// underlying DC line has non-zero reactive capability.
    pub internal_support_commitment_schedule: HashMap<String, Vec<bool>>,

    // ── Consumer modeling (phase 2f) ────────────────────────────────────
    pub device_fixed_p_series_pu: HashMap<String, Vec<f64>>,
    pub consumer_dispatchable_resource_ids_by_uid: HashMap<String, Vec<String>>,

    // ── Reserve products ────────────────────────────────────────────────
    /// Ordered list of reserve product IDs (`"reg_up"`, `"reg_down"`,
    /// `"syn"`, `"nsyn"`, `"ramp_res_up"`, `"ramp_res_down"`,
    /// `"reactive_up"`, `"reactive_down"`).
    pub reserve_product_ids: Vec<String>,

    // ── Voltage regulation decisions ────────────────────────────────────
    /// Resource IDs (producer UIDs) the adapter marked as voltage-regulating
    /// after consulting the policy's `preserve_ac_voltage_controls` rule.
    pub explicit_voltage_regulating_resource_ids: HashSet<String>,
    /// Subset of the above that have an *explicit* GO C3 `vm_setpoint`
    /// (rather than inheriting from the bus). Retained for diagnostics.
    pub go_explicit_voltage_regulating_resource_ids: HashSet<String>,
    /// Bus numbers the adapter picked as Slack (usually one).
    pub slack_bus_numbers: Vec<u32>,

    // ── Adapter issue log ───────────────────────────────────────────────
    pub issues: Vec<GoC3Issue>,
}

impl GoC3Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an adapter issue.
    pub fn add_issue(
        &mut self,
        severity: GoC3IssueSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.issues.push(GoC3Issue {
            severity,
            code: code.into(),
            message: message.into(),
        });
    }

    /// Record an informational issue.
    pub fn add_info(&mut self, code: impl Into<String>, message: impl Into<String>) {
        self.issues.push(GoC3Issue::info(code, message));
    }

    /// Record a warning.
    pub fn add_warning(&mut self, code: impl Into<String>, message: impl Into<String>) {
        self.issues.push(GoC3Issue::warning(code, message));
    }

    /// Record an error.
    pub fn add_error(&mut self, code: impl Into<String>, message: impl Into<String>) {
        self.issues.push(GoC3Issue::error(code, message));
    }

    /// Count issues by severity.
    pub fn issue_counts(&self) -> (usize, usize, usize) {
        let mut info = 0;
        let mut warn = 0;
        let mut err = 0;
        for issue in &self.issues {
            match issue.severity {
                GoC3IssueSeverity::Info => info += 1,
                GoC3IssueSeverity::Warning => warn += 1,
                GoC3IssueSeverity::Error => err += 1,
            }
        }
        (info, warn, err)
    }
}
