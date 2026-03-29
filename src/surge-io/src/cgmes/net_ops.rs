// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES Network Operations parser.
//!
//! Parses IEC 61968 Operations & Maintenance domain classes:
//! - **SwitchingPlan** + **SwitchingAction** (switching step sequences)
//! - **PlannedOutage** / **ForcedOutage** (equipment outage records)
//! - **OutageSchedule** (scheduling horizons grouping outages)
//! - **Crew** + **CrewType** (field crew dispatch)
//! - **WorkTask** (maintenance work items linked to crews and outages)

use surge_network::Network;
use surge_network::network::net_ops::{
    CrewRecord, CrewStatus, NetworkOperationsData, OutageCause, OutageRecord, OutageScheduleData,
    SwitchingPlan, SwitchingStep, SwitchingStepKind, WorkTaskKind, WorkTaskRecord, WorkTaskStatus,
};
use surge_network::network::time_utils::parse_iso8601;

use super::indices::CgmesIndices;
use super::types::ObjMap;

// ---------------------------------------------------------------------------
// Enum mapping helpers
// ---------------------------------------------------------------------------

fn parse_switching_step_kind(s: &str) -> Option<SwitchingStepKind> {
    let lower = s.to_lowercase();
    if lower.contains("open") {
        Some(SwitchingStepKind::Open)
    } else if lower.contains("close") {
        Some(SwitchingStepKind::Close)
    } else if lower.contains("deenergize") || lower.contains("de-energize") {
        // DeEnergize must be checked before Energize
        Some(SwitchingStepKind::DeEnergize)
    } else if lower.contains("energize") {
        Some(SwitchingStepKind::Energize)
    } else if lower.contains("unground") {
        // Unground must be checked before Ground
        Some(SwitchingStepKind::Unground)
    } else if lower.contains("ground") {
        Some(SwitchingStepKind::Ground)
    } else {
        None
    }
}

fn parse_outage_cause(s: &str) -> Option<OutageCause> {
    let lower = s.to_lowercase();
    if lower.contains("maintenance") {
        Some(OutageCause::Maintenance)
    } else if lower.contains("construction") {
        Some(OutageCause::Construction)
    } else if lower.contains("repair") {
        Some(OutageCause::Repair)
    } else if lower.contains("test") {
        Some(OutageCause::Testing)
    } else if lower.contains("environment") {
        Some(OutageCause::Environmental)
    } else if lower.contains("forcedequipment") || lower.contains("equipment failure") {
        Some(OutageCause::ForcedEquipment)
    } else if lower.contains("forcedweather") || lower.contains("weather") {
        Some(OutageCause::ForcedWeather)
    } else if lower.contains("forcedprotection") || lower.contains("protection") {
        Some(OutageCause::ForcedProtection)
    } else {
        Some(OutageCause::Other)
    }
}

fn parse_crew_status(s: &str) -> Option<CrewStatus> {
    let lower = s.to_lowercase();
    if lower.contains("available") {
        Some(CrewStatus::Available)
    } else if lower.contains("enroute") || lower.contains("en route") {
        Some(CrewStatus::EnRoute)
    } else if lower.contains("onsite") || lower.contains("on site") || lower.contains("on-site") {
        Some(CrewStatus::OnSite)
    } else if lower.contains("released") {
        Some(CrewStatus::Released)
    } else if lower.contains("dispatched") {
        Some(CrewStatus::Dispatched)
    } else {
        None
    }
}

fn parse_work_task_kind(s: &str) -> Option<WorkTaskKind> {
    let lower = s.to_lowercase();
    if lower.contains("install") {
        Some(WorkTaskKind::Install)
    } else if lower.contains("remove") {
        Some(WorkTaskKind::Remove)
    } else if lower.contains("inspect") {
        Some(WorkTaskKind::Inspect)
    } else if lower.contains("repair") {
        Some(WorkTaskKind::Repair)
    } else if lower.contains("replace") {
        Some(WorkTaskKind::Replace)
    } else {
        None
    }
}

fn parse_work_task_status(s: &str) -> Option<WorkTaskStatus> {
    let lower = s.to_lowercase();
    if lower.contains("scheduled") {
        Some(WorkTaskStatus::Scheduled)
    } else if lower.contains("inprogress") || lower.contains("in progress") {
        Some(WorkTaskStatus::InProgress)
    } else if lower.contains("completed") || lower.contains("complete") {
        Some(WorkTaskStatus::Completed)
    } else if lower.contains("cancelled") || lower.contains("canceled") {
        Some(WorkTaskStatus::Cancelled)
    } else if lower.contains("dispatched") {
        Some(WorkTaskStatus::Dispatched)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Main builder
// ---------------------------------------------------------------------------

/// Build `NetworkOperationsData` from the CGMES object store and attach it to
/// `network.cim.network_operations`.
pub(crate) fn build_network_operations(
    objects: &ObjMap,
    _idx: &CgmesIndices,
    network: &mut Network,
) {
    let mut data = NetworkOperationsData::default();

    // --- SwitchingPlan + SwitchingAction/SwitchingStep ---
    build_switching_plans(objects, &mut data);

    // --- PlannedOutage / ForcedOutage ---
    build_outage_records(objects, &mut data);

    // --- OutageSchedule ---
    build_outage_schedules(objects, &mut data);

    // --- Crew + CrewType ---
    build_crews(objects, &mut data);

    // --- WorkTask ---
    build_work_tasks(objects, &mut data);

    if !data.is_empty() {
        tracing::info!(
            switching_plans = data.switching_plans.len(),
            outage_records = data.outage_records.len(),
            outage_schedules = data.outage_schedules.len(),
            crews = data.crews.len(),
            work_tasks = data.work_tasks.len(),
            "CGMES NetworkOperations parsed"
        );
        network.cim.network_operations = data;
    }
}

// ---------------------------------------------------------------------------
// Sub-builders
// ---------------------------------------------------------------------------

fn build_switching_plans(objects: &ObjMap, data: &mut NetworkOperationsData) {
    use std::collections::HashMap;

    // Collect SwitchingAction/SwitchingStep objects grouped by parent SwitchingPlan mRID.
    let mut steps_by_plan: HashMap<String, Vec<SwitchingStep>> = HashMap::new();

    for (_id, obj) in objects.iter() {
        let is_action = obj.class == "SwitchingAction" || obj.class == "SwitchingStep";
        if !is_action {
            continue;
        }

        // Parent plan reference: SwitchingAction.SwitchingPlan or SwitchingStep.SwitchingPlan
        let plan_id = obj
            .get_ref("SwitchingPlan")
            .or_else(|| obj.get_ref("SwitchingStepGroup"))
            .unwrap_or("")
            .to_string();
        if plan_id.is_empty() {
            continue;
        }

        let seq = obj
            .get_text("sequenceNumber")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);

        let kind = obj
            .get_text("kind")
            .or_else(|| obj.get_text("switchingStepType"))
            .and_then(parse_switching_step_kind);

        let switch_mrid = obj.get_ref("OperatedSwitch").map(|s| s.to_string());
        let equipment_mrid = obj
            .get_ref("PowerSystemResources")
            .or_else(|| obj.get_ref("Equipment"))
            .map(|s| s.to_string());

        let description = obj.get_text("description").map(|s| s.to_string());
        let is_free_sequence = obj
            .get_text("isFreeSequence")
            .map(|s| s == "true")
            .unwrap_or(false);
        let executed_date_time = obj.get_text("executedDateTime").and_then(parse_iso8601);

        let step = SwitchingStep {
            sequence_number: seq,
            kind,
            switch_mrid,
            equipment_mrid,
            description,
            is_free_sequence,
            executed_date_time,
        };

        steps_by_plan.entry(plan_id).or_default().push(step);
    }

    // Now build SwitchingPlan objects.
    for (id, obj) in objects.iter() {
        if obj.class != "SwitchingPlan" {
            continue;
        }

        let name = obj.get_text("name").unwrap_or("").to_string();
        let purpose = obj.get_text("purpose").map(|s| s.to_string());
        let planned_start = obj
            .get_text("plannedPeriod.start")
            .or_else(|| obj.get_text("plannedStart"))
            .and_then(parse_iso8601);
        let planned_end = obj
            .get_text("plannedPeriod.end")
            .or_else(|| obj.get_text("plannedEnd"))
            .and_then(parse_iso8601);
        let approved_date_time = obj.get_text("approvedDateTime").and_then(parse_iso8601);

        let mut steps = steps_by_plan.remove(id).unwrap_or_default();
        steps.sort_by_key(|s| s.sequence_number);

        data.switching_plans.push(SwitchingPlan {
            mrid: id.clone(),
            name,
            purpose,
            planned_start,
            planned_end,
            approved_date_time,
            steps,
        });
    }
}

fn build_outage_records(objects: &ObjMap, data: &mut NetworkOperationsData) {
    for (id, obj) in objects.iter() {
        let is_planned = obj.class == "PlannedOutage";
        let is_forced = obj.class == "ForcedOutage";
        let is_outage = obj.class == "Outage";
        if !is_planned && !is_forced && !is_outage {
            continue;
        }

        let name = obj.get_text("name").unwrap_or("").to_string();
        let cause = obj
            .get_text("causeKind")
            .or_else(|| obj.get_text("cause"))
            .and_then(parse_outage_cause);

        let planned_start = obj
            .get_text("plannedPeriod.start")
            .or_else(|| obj.get_text("plannedStart"))
            .and_then(parse_iso8601);
        let planned_end = obj
            .get_text("plannedPeriod.end")
            .or_else(|| obj.get_text("plannedEnd"))
            .and_then(parse_iso8601);
        let actual_start = obj
            .get_text("actualPeriod.start")
            .or_else(|| obj.get_text("actualStart"))
            .and_then(parse_iso8601);
        let actual_end = obj
            .get_text("actualPeriod.end")
            .or_else(|| obj.get_text("actualEnd"))
            .and_then(parse_iso8601);
        let cancelled_date_time = obj.get_text("cancelledDateTime").and_then(parse_iso8601);
        let estimated_restore = obj
            .get_text("estimatedPeriod.end")
            .or_else(|| obj.get_text("estimatedRestoreDateTime"))
            .and_then(parse_iso8601);
        let area_name = obj
            .get_text("communityDescriptor")
            .or_else(|| obj.get_text("areaName"))
            .map(|s| s.to_string());

        // Collect equipment references: Outage.Equipments or OutagePlan.Equipment
        let mut equipment_mrids = Vec::new();
        if let Some(eq_ref) = obj.get_ref("Equipments") {
            equipment_mrids.push(eq_ref.to_string());
        }
        if let Some(eq_ref) = obj.get_ref("Equipment") {
            equipment_mrids.push(eq_ref.to_string());
        }

        // Also scan for OutageEquipment objects referencing this outage.
        for (_, oe_obj) in objects.iter() {
            if (oe_obj.class == "OutageEquipment" || oe_obj.class == "ClearanceAction")
                && oe_obj.get_ref("Outage") == Some(id.as_str())
                && let Some(eq_ref) = oe_obj.get_ref("Equipment")
            {
                equipment_mrids.push(eq_ref.to_string());
            }
        }

        data.outage_records.push(OutageRecord {
            mrid: id.clone(),
            name,
            is_planned: is_planned || is_outage,
            cause,
            equipment_mrids,
            planned_start,
            planned_end,
            actual_start,
            actual_end,
            cancelled_date_time,
            estimated_restore,
            area_name,
        });
    }
}

fn build_outage_schedules(objects: &ObjMap, data: &mut NetworkOperationsData) {
    for (id, obj) in objects.iter() {
        if obj.class != "OutageSchedule" {
            continue;
        }

        let name = obj.get_text("name").unwrap_or("").to_string();
        let horizon_start = obj
            .get_text("schedulePeriod.start")
            .or_else(|| obj.get_text("horizonStart"))
            .and_then(parse_iso8601);
        let horizon_end = obj
            .get_text("schedulePeriod.end")
            .or_else(|| obj.get_text("horizonEnd"))
            .and_then(parse_iso8601);

        // Collect outage references: scan outage records that reference this schedule.
        let mut outage_ids = Vec::new();
        for (outage_id, outage_obj) in objects.iter() {
            let is_outage = outage_obj.class == "PlannedOutage"
                || outage_obj.class == "ForcedOutage"
                || outage_obj.class == "Outage";
            if is_outage && outage_obj.get_ref("OutageSchedule") == Some(id.as_str()) {
                outage_ids.push(outage_id.clone());
            }
        }

        data.outage_schedules.push(OutageScheduleData {
            mrid: id.clone(),
            name,
            horizon_start,
            horizon_end,
            outages: outage_ids,
        });
    }
}

fn build_crews(objects: &ObjMap, data: &mut NetworkOperationsData) {
    // Build CrewType name lookup.
    let crew_type_names: std::collections::HashMap<&str, &str> = objects
        .iter()
        .filter(|(_, o)| o.class == "CrewType")
        .filter_map(|(id, o)| Some((id.as_str(), o.get_text("name")?)))
        .collect();

    for (id, obj) in objects.iter() {
        if obj.class != "Crew" {
            continue;
        }

        let name = obj.get_text("name").unwrap_or("").to_string();
        let crew_type = obj
            .get_ref("CrewType")
            .and_then(|ct_id| crew_type_names.get(ct_id))
            .map(|s| s.to_string());
        let status = obj
            .get_text("status")
            .or_else(|| obj.get_text("status.value"))
            .and_then(parse_crew_status);

        data.crews.push(CrewRecord {
            mrid: id.clone(),
            name,
            crew_type,
            status,
        });
    }
}

fn build_work_tasks(objects: &ObjMap, data: &mut NetworkOperationsData) {
    for (id, obj) in objects.iter() {
        if obj.class != "WorkTask" {
            continue;
        }

        let name = obj.get_text("name").unwrap_or("").to_string();
        let crew_mrid = obj.get_ref("Crew").map(|s| s.to_string());
        let outage_mrid = obj.get_ref("Outage").map(|s| s.to_string());
        let scheduled_start = obj
            .get_text("scheduleParameterInfo.scheduledStartTime")
            .or_else(|| obj.get_text("scheduledStart"))
            .and_then(parse_iso8601);
        let scheduled_end = obj
            .get_text("scheduleParameterInfo.scheduledEndTime")
            .or_else(|| obj.get_text("scheduledEnd"))
            .and_then(parse_iso8601);
        let task_kind = obj
            .get_text("taskKind")
            .or_else(|| obj.get_text("kind"))
            .and_then(parse_work_task_kind);
        let priority = obj
            .get_text("priority")
            .or_else(|| obj.get_text("priority.rank"))
            .and_then(|s| s.parse::<u32>().ok());
        let status = obj
            .get_text("status")
            .or_else(|| obj.get_text("status.value"))
            .and_then(parse_work_task_status);

        data.work_tasks.push(WorkTaskRecord {
            mrid: id.clone(),
            name,
            crew_mrid,
            outage_mrid,
            scheduled_start,
            scheduled_end,
            task_kind,
            priority,
            status,
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgmes::types::{CimObj, CimVal};
    use std::collections::HashMap;

    fn make_obj(class: &str, attrs: &[(&str, &str)]) -> CimObj {
        let mut obj = CimObj::new(class);
        for &(k, v) in attrs {
            obj.attrs.insert(k.to_string(), CimVal::Text(v.to_string()));
        }
        obj
    }

    fn make_obj_with_refs(class: &str, texts: &[(&str, &str)], refs: &[(&str, &str)]) -> CimObj {
        let mut obj = CimObj::new(class);
        for &(k, v) in texts {
            obj.attrs.insert(k.to_string(), CimVal::Text(v.to_string()));
        }
        for &(k, v) in refs {
            obj.attrs.insert(k.to_string(), CimVal::Ref(v.to_string()));
        }
        obj
    }

    #[test]
    fn test_switching_plan_with_steps() {
        let mut objects: ObjMap = HashMap::new();

        objects.insert(
            "plan1".to_string(),
            make_obj(
                "SwitchingPlan",
                &[
                    ("name", "Outage Plan A"),
                    ("purpose", "Transformer maintenance"),
                ],
            ),
        );

        objects.insert(
            "step1".to_string(),
            make_obj_with_refs(
                "SwitchingAction",
                &[("sequenceNumber", "2"), ("kind", "Open")],
                &[("SwitchingPlan", "plan1"), ("OperatedSwitch", "sw1")],
            ),
        );

        objects.insert(
            "step2".to_string(),
            make_obj_with_refs(
                "SwitchingAction",
                &[("sequenceNumber", "1"), ("kind", "DeEnergize")],
                &[("SwitchingPlan", "plan1"), ("PowerSystemResources", "eq1")],
            ),
        );

        let idx = super::super::indices::CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_network_operations(&objects, &idx, &mut network);

        assert_eq!(network.cim.network_operations.switching_plans.len(), 1);
        let plan = &network.cim.network_operations.switching_plans[0];
        assert_eq!(plan.mrid, "plan1");
        assert_eq!(plan.name, "Outage Plan A");
        assert_eq!(plan.purpose.as_deref(), Some("Transformer maintenance"));
        assert_eq!(plan.steps.len(), 2);
        // Steps should be sorted by sequence_number.
        assert_eq!(plan.steps[0].sequence_number, 1);
        assert_eq!(plan.steps[0].kind, Some(SwitchingStepKind::DeEnergize));
        assert_eq!(plan.steps[1].sequence_number, 2);
        assert_eq!(plan.steps[1].kind, Some(SwitchingStepKind::Open));
        assert_eq!(plan.steps[1].switch_mrid.as_deref(), Some("sw1"));
    }

    #[test]
    fn test_planned_outage_record() {
        let mut objects: ObjMap = HashMap::new();

        objects.insert(
            "outage1".to_string(),
            make_obj(
                "PlannedOutage",
                &[
                    ("name", "Line 138kV Maintenance"),
                    ("causeKind", "maintenance"),
                    ("plannedPeriod.start", "2026-04-01T08:00:00Z"),
                    ("plannedPeriod.end", "2026-04-01T18:00:00Z"),
                ],
            ),
        );

        let idx = super::super::indices::CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_network_operations(&objects, &idx, &mut network);

        assert_eq!(network.cim.network_operations.outage_records.len(), 1);
        let rec = &network.cim.network_operations.outage_records[0];
        assert!(rec.is_planned);
        assert_eq!(rec.cause, Some(OutageCause::Maintenance));
        assert_eq!(
            rec.planned_start.unwrap().to_rfc3339(),
            "2026-04-01T08:00:00+00:00"
        );
    }

    #[test]
    fn test_forced_outage_record() {
        let mut objects: ObjMap = HashMap::new();

        objects.insert(
            "outage2".to_string(),
            make_obj(
                "ForcedOutage",
                &[
                    ("name", "Transformer Trip"),
                    ("causeKind", "forcedEquipment"),
                    ("actualPeriod.start", "2026-03-10T14:30:00Z"),
                ],
            ),
        );

        let idx = super::super::indices::CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_network_operations(&objects, &idx, &mut network);

        assert_eq!(network.cim.network_operations.outage_records.len(), 1);
        let rec = &network.cim.network_operations.outage_records[0];
        assert!(!rec.is_planned);
        assert_eq!(rec.cause, Some(OutageCause::ForcedEquipment));
        assert_eq!(
            rec.actual_start.unwrap().to_rfc3339(),
            "2026-03-10T14:30:00+00:00"
        );
    }

    #[test]
    fn test_outage_schedule() {
        let mut objects: ObjMap = HashMap::new();

        objects.insert(
            "sched1".to_string(),
            make_obj(
                "OutageSchedule",
                &[
                    ("name", "Q2 2026 Schedule"),
                    ("schedulePeriod.start", "2026-04-01"),
                    ("schedulePeriod.end", "2026-06-30"),
                ],
            ),
        );

        objects.insert(
            "outage_a".to_string(),
            make_obj_with_refs(
                "PlannedOutage",
                &[("name", "Outage A")],
                &[("OutageSchedule", "sched1")],
            ),
        );

        let idx = super::super::indices::CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_network_operations(&objects, &idx, &mut network);

        assert_eq!(network.cim.network_operations.outage_schedules.len(), 1);
        let sched = &network.cim.network_operations.outage_schedules[0];
        assert_eq!(sched.name, "Q2 2026 Schedule");
        assert_eq!(sched.outages.len(), 1);
        assert_eq!(sched.outages[0], "outage_a");
    }

    #[test]
    fn test_crew_with_type_lookup() {
        let mut objects: ObjMap = HashMap::new();

        objects.insert(
            "ct1".to_string(),
            make_obj("CrewType", &[("name", "Line Crew")]),
        );

        objects.insert(
            "crew1".to_string(),
            make_obj_with_refs(
                "Crew",
                &[("name", "Crew Alpha"), ("status", "enRoute")],
                &[("CrewType", "ct1")],
            ),
        );

        let idx = super::super::indices::CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_network_operations(&objects, &idx, &mut network);

        assert_eq!(network.cim.network_operations.crews.len(), 1);
        let crew = &network.cim.network_operations.crews[0];
        assert_eq!(crew.name, "Crew Alpha");
        assert_eq!(crew.crew_type.as_deref(), Some("Line Crew"));
        assert_eq!(crew.status, Some(CrewStatus::EnRoute));
    }

    #[test]
    fn test_work_task() {
        let mut objects: ObjMap = HashMap::new();

        objects.insert(
            "wt1".to_string(),
            make_obj_with_refs(
                "WorkTask",
                &[
                    ("name", "Replace CT"),
                    ("taskKind", "replace"),
                    ("priority", "2"),
                    ("status", "scheduled"),
                    ("scheduledStart", "2026-04-15T06:00:00Z"),
                ],
                &[("Crew", "crew1"), ("Outage", "outage1")],
            ),
        );

        let idx = super::super::indices::CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_network_operations(&objects, &idx, &mut network);

        assert_eq!(network.cim.network_operations.work_tasks.len(), 1);
        let wt = &network.cim.network_operations.work_tasks[0];
        assert_eq!(wt.name, "Replace CT");
        assert_eq!(wt.task_kind, Some(WorkTaskKind::Replace));
        assert_eq!(wt.priority, Some(2));
        assert_eq!(wt.status, Some(WorkTaskStatus::Scheduled));
        assert_eq!(wt.crew_mrid.as_deref(), Some("crew1"));
        assert_eq!(wt.outage_mrid.as_deref(), Some("outage1"));
    }

    #[test]
    fn test_empty_objects_produces_no_data() {
        let objects: ObjMap = HashMap::new();
        let idx = super::super::indices::CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_network_operations(&objects, &idx, &mut network);

        assert!(network.cim.network_operations.is_empty());
    }

    #[test]
    fn test_switching_step_kind_parsing() {
        assert_eq!(
            parse_switching_step_kind("Open"),
            Some(SwitchingStepKind::Open)
        );
        assert_eq!(
            parse_switching_step_kind("close"),
            Some(SwitchingStepKind::Close)
        );
        assert_eq!(
            parse_switching_step_kind("DeEnergize"),
            Some(SwitchingStepKind::DeEnergize)
        );
        assert_eq!(
            parse_switching_step_kind("Energize"),
            Some(SwitchingStepKind::Energize)
        );
        assert_eq!(
            parse_switching_step_kind("Ground"),
            Some(SwitchingStepKind::Ground)
        );
        assert_eq!(
            parse_switching_step_kind("Unground"),
            Some(SwitchingStepKind::Unground)
        );
        assert_eq!(parse_switching_step_kind("unknown"), None);
    }
}
