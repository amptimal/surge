// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Network Operations data types — switching plans, outage scheduling, and crew dispatch.
//!
//! These types model the IEC 61968 / CIM Operations & Maintenance domain:
//! - **SwitchingPlan** — ordered sequence of switching steps for planned outages
//! - **OutageRecord** — planned or forced equipment outages with cause classification
//! - **OutageScheduleData** — scheduling horizon containing multiple outage references
//! - **CrewRecord** — field crew assignments and dispatch status
//! - **WorkTaskRecord** — maintenance work tasks linked to crews and outages

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of action performed in a switching step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitchingStepKind {
    Open,
    Close,
    Energize,
    DeEnergize,
    Ground,
    Unground,
}

/// A single step within a switching plan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwitchingStep {
    /// Order of execution within the parent switching plan.
    pub sequence_number: u32,
    /// Action to perform (open, close, energize, etc.).
    pub kind: Option<SwitchingStepKind>,
    /// mRID of the switch device to operate (if applicable).
    pub switch_mrid: Option<String>,
    /// mRID of the equipment being switched (if applicable).
    pub equipment_mrid: Option<String>,
    /// Free-text description of this step.
    pub description: Option<String>,
    /// Whether this step can be executed in any order relative to siblings.
    pub is_free_sequence: bool,
    /// Timestamp when this step was actually executed.
    pub executed_date_time: Option<DateTime<Utc>>,
}

/// An ordered sequence of switching actions for a planned outage or restoration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwitchingPlan {
    /// Unique CIM mRID.
    pub mrid: String,
    /// Human-readable name.
    pub name: String,
    /// Purpose or reason for the switching plan.
    pub purpose: Option<String>,
    /// Planned start time.
    pub planned_start: Option<DateTime<Utc>>,
    /// Planned end time.
    pub planned_end: Option<DateTime<Utc>>,
    /// Timestamp when the plan was approved.
    pub approved_date_time: Option<DateTime<Utc>>,
    /// Ordered switching steps (sorted by `sequence_number`).
    pub steps: Vec<SwitchingStep>,
}

/// Classification of the cause of an outage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutageCause {
    Maintenance,
    Construction,
    Repair,
    Testing,
    Environmental,
    ForcedEquipment,
    ForcedWeather,
    ForcedProtection,
    Other,
}

/// A planned or forced equipment outage record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutageRecord {
    /// Unique CIM mRID.
    pub mrid: String,
    /// Human-readable name.
    pub name: String,
    /// Whether this is a planned (true) or forced (false) outage.
    pub is_planned: bool,
    /// Cause classification.
    pub cause: Option<OutageCause>,
    /// mRIDs of affected equipment.
    pub equipment_mrids: Vec<String>,
    /// Planned start time.
    pub planned_start: Option<DateTime<Utc>>,
    /// Planned end time.
    pub planned_end: Option<DateTime<Utc>>,
    /// Actual start time.
    pub actual_start: Option<DateTime<Utc>>,
    /// Actual end time.
    pub actual_end: Option<DateTime<Utc>>,
    /// Timestamp when the outage was cancelled (if applicable).
    pub cancelled_date_time: Option<DateTime<Utc>>,
    /// Estimated restoration time.
    pub estimated_restore: Option<DateTime<Utc>>,
    /// Name of the area affected by this outage.
    pub area_name: Option<String>,
}

/// A scheduling horizon containing references to multiple outage records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutageScheduleData {
    /// Unique CIM mRID.
    pub mrid: String,
    /// Human-readable name.
    pub name: String,
    /// Start of the scheduling horizon.
    pub horizon_start: Option<DateTime<Utc>>,
    /// End of the scheduling horizon.
    pub horizon_end: Option<DateTime<Utc>>,
    /// mRIDs of outage records within this schedule.
    pub outages: Vec<String>,
}

/// Dispatch status of a field crew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrewStatus {
    Available,
    Dispatched,
    EnRoute,
    OnSite,
    Released,
}

/// A field crew record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrewRecord {
    /// Unique CIM mRID.
    pub mrid: String,
    /// Human-readable name.
    pub name: String,
    /// Type of crew (e.g., "Line", "Substation", "Transmission").
    pub crew_type: Option<String>,
    /// Current dispatch status.
    pub status: Option<CrewStatus>,
}

/// Classification of a maintenance work task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkTaskKind {
    Install,
    Remove,
    Inspect,
    Repair,
    Replace,
}

/// Execution status of a work task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkTaskStatus {
    Scheduled,
    Dispatched,
    InProgress,
    Completed,
    Cancelled,
}

/// A maintenance work task linked to a crew and/or outage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkTaskRecord {
    /// Unique CIM mRID.
    pub mrid: String,
    /// Human-readable name.
    pub name: String,
    /// mRID of the assigned crew.
    pub crew_mrid: Option<String>,
    /// mRID of the associated outage.
    pub outage_mrid: Option<String>,
    /// Scheduled start time.
    pub scheduled_start: Option<DateTime<Utc>>,
    /// Scheduled end time.
    pub scheduled_end: Option<DateTime<Utc>>,
    /// Kind of work to be performed.
    pub task_kind: Option<WorkTaskKind>,
    /// Numeric priority (lower = higher priority).
    pub priority: Option<u32>,
    /// Current execution status.
    pub status: Option<WorkTaskStatus>,
}

/// Aggregate container for all network operations data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkOperationsData {
    /// Switching plans with ordered steps.
    pub switching_plans: Vec<SwitchingPlan>,
    /// Planned and forced outage records.
    pub outage_records: Vec<OutageRecord>,
    /// Outage scheduling horizons.
    pub outage_schedules: Vec<OutageScheduleData>,
    /// Field crew records.
    pub crews: Vec<CrewRecord>,
    /// Maintenance work tasks.
    pub work_tasks: Vec<WorkTaskRecord>,
}

impl NetworkOperationsData {
    /// Returns `true` if all collections are empty.
    pub fn is_empty(&self) -> bool {
        self.switching_plans.is_empty()
            && self.outage_records.is_empty()
            && self.outage_schedules.is_empty()
            && self.crews.is_empty()
            && self.work_tasks.is_empty()
    }
}
