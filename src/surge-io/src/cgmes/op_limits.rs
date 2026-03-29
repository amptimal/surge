// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES Operational Limits (OL) full hierarchy parser.
//!
//! Parses the complete IEC 61970-302 OperationalLimits package:
//! - **OperationalLimitSet** — container attached to a Terminal or Equipment
//! - **OperationalLimitType** — duration (PATL/TATL/IATL) and direction
//! - **ActivePowerLimit** — MW limits
//! - **ApparentPowerLimit** — MVA limits
//! - **CurrentLimit** — Ampere limits (converted to MVA via base_kv)
//! - **VoltageLimit** — kV limits
//!
//! The resulting `OperationalLimits` structure is stored on `Network.cim.operational_limits`
//! alongside the existing `rate_a/rate_b/rate_c` and `vmin/vmax` fields which remain
//! the primary solver inputs.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::op_limits::{
    LimitDirection, LimitDuration, LimitKind, OperationalLimit, OperationalLimitSet,
};

use super::indices::CgmesIndices;
use super::types::{CimObj, ObjMap};

/// Build the full operational limits model from parsed CGMES objects.
///
/// This is called after `CgmesIndices::build()` and after buses/branches have been
/// assigned internal bus numbers, so Terminal → bus resolution is available.
pub(crate) fn build_operational_limits(
    objects: &ObjMap,
    idx: &CgmesIndices,
    network: &mut Network,
) {
    // -----------------------------------------------------------------------
    // Pass 1: Collect OperationalLimitType → (duration, direction, mrid)
    // -----------------------------------------------------------------------
    let olt_map: HashMap<&str, (LimitDuration, LimitDirection)> = objects
        .iter()
        .filter(|(_, o)| o.class == "OperationalLimitType")
        .map(|(id, o)| {
            let duration = parse_duration(o);
            let direction = parse_direction(o);
            (id.as_str(), (duration, direction))
        })
        .collect();

    // -----------------------------------------------------------------------
    // Pass 2: Collect OperationalLimitSet → (terminal_id, equipment_mrid, name)
    // -----------------------------------------------------------------------
    struct OlsInfo {
        name: String,
        terminal_id: Option<String>,
        equipment_mrid: Option<String>,
    }

    let mut ols_map: HashMap<&str, OlsInfo> = HashMap::new();

    for (ols_id, o) in objects
        .iter()
        .filter(|(_, o)| o.class == "OperationalLimitSet")
    {
        let name = o.get_text("name").unwrap_or_default().to_string();

        let (terminal_id, equipment_mrid) = if let Some(term_id) = o.get_ref("Terminal") {
            let eq_mrid = objects
                .get(term_id)
                .and_then(|t| t.get_ref("ConductingEquipment"))
                .map(|s| s.to_string());
            (Some(term_id.to_string()), eq_mrid)
        } else if let Some(eq_id) = o.get_ref("Equipment") {
            (None, Some(eq_id.to_string()))
        } else {
            (None, None)
        };

        ols_map.insert(
            ols_id.as_str(),
            OlsInfo {
                name,
                terminal_id,
                equipment_mrid,
            },
        );
    }

    // -----------------------------------------------------------------------
    // Pass 3: Collect all limit values and attach to their sets
    // -----------------------------------------------------------------------
    // Accumulate limits per OLS mRID
    let mut limits_by_ols: HashMap<&str, Vec<(LimitKind, OperationalLimit)>> = HashMap::new();

    // 3a. ApparentPowerLimit
    for (_, obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "ApparentPowerLimit")
    {
        if let Some((ols_id, limit)) = parse_limit_value(obj, LimitKind::ApparentPower, &olt_map) {
            limits_by_ols
                .entry(ols_id)
                .or_default()
                .push((LimitKind::ApparentPower, limit));
        }
    }

    // 3b. ActivePowerLimit
    for (_, obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "ActivePowerLimit")
    {
        if let Some((ols_id, limit)) = parse_limit_value(obj, LimitKind::ActivePower, &olt_map) {
            limits_by_ols
                .entry(ols_id)
                .or_default()
                .push((LimitKind::ActivePower, limit));
        }
    }

    // 3c. CurrentLimit — stored in Amps (raw); conversion to MVA is caller's job
    for (_, obj) in objects.iter().filter(|(_, o)| o.class == "CurrentLimit") {
        if let Some((ols_id, limit)) = parse_limit_value(obj, LimitKind::Current, &olt_map) {
            limits_by_ols
                .entry(ols_id)
                .or_default()
                .push((LimitKind::Current, limit));
        }
    }

    // 3d. VoltageLimit
    for (_, obj) in objects.iter().filter(|(_, o)| o.class == "VoltageLimit") {
        if let Some((ols_id, limit)) = parse_limit_value(obj, LimitKind::Voltage, &olt_map) {
            limits_by_ols
                .entry(ols_id)
                .or_default()
                .push((LimitKind::Voltage, limit));
        }
    }

    // -----------------------------------------------------------------------
    // Pass 4: Resolve Terminal → bus number, build OperationalLimitSet structs
    // -----------------------------------------------------------------------
    let eq_terminals = &idx.eq_terminals;

    for (ols_id, info) in &ols_map {
        let limits = match limits_by_ols.remove(ols_id) {
            Some(v) if !v.is_empty() => v,
            _ => continue, // skip sets with no limits
        };

        // Resolve bus number from terminal
        let bus = info
            .terminal_id
            .as_deref()
            .and_then(|tid| {
                let term = objects.get(tid)?;
                let tn_id = term.get_ref("TopologicalNode")?;
                idx.tn_bus(tn_id)
            })
            .unwrap_or(0);

        // Determine from_end: terminal sequence number 1 = from, 2 = to
        let from_end = info.terminal_id.as_deref().and_then(|tid| {
            let eq_mrid = info.equipment_mrid.as_deref()?;
            let terms = eq_terminals.get(eq_mrid)?;
            if terms.len() >= 2 {
                let pos = terms.iter().position(|t| t == tid)?;
                Some(pos == 0)
            } else {
                None
            }
        });

        let ols = OperationalLimitSet {
            mrid: ols_id.to_string(),
            name: info.name.clone(),
            bus,
            equipment_mrid: info.equipment_mrid.clone(),
            from_end,
            limits,
        };

        network
            .cim
            .operational_limits
            .limit_sets
            .insert(ols_id.to_string(), ols);
    }

    if !network.cim.operational_limits.is_empty() {
        tracing::info!(
            limit_sets = network.cim.operational_limits.limit_sets.len(),
            total_limits = network.cim.operational_limits.total_limit_count(),
            "Operational limits parsed from CGMES"
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse the duration category from an `OperationalLimitType` object.
fn parse_duration(obj: &CimObj) -> LimitDuration {
    // Check isInfiniteDuration flag
    if let Some(s) = obj.get_text("isInfiniteDuration")
        && s.eq_ignore_ascii_case("true")
    {
        return LimitDuration::Permanent;
    }

    // Check acceptableDuration
    if let Some(dur) = obj.parse_f64("acceptableDuration") {
        if dur <= 0.0 || dur < 1.0 {
            return LimitDuration::Instantaneous;
        }
        if dur >= 1e9 {
            return LimitDuration::Permanent;
        }
        return LimitDuration::Temporary(dur);
    }

    // Check limitType for PATL/TATL/IATL keywords
    let lt = obj
        .get_ref("limitType")
        .or_else(|| obj.get_text("limitType"))
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if lt.contains("patl") || lt.contains("permanent") || lt.contains("normal") {
        return LimitDuration::Permanent;
    }
    if lt.contains("iatl") || lt.contains("instantaneous") {
        return LimitDuration::Instantaneous;
    }
    if lt.contains("tatl") || lt.contains("temporary") || lt.contains("emergency") {
        // No specific duration available, use 900s (15 min) as default TATL
        return LimitDuration::Temporary(900.0);
    }

    // Default: treat as permanent (PATL)
    LimitDuration::Permanent
}

/// Parse the direction from an `OperationalLimitType` object.
fn parse_direction(obj: &CimObj) -> LimitDirection {
    let dir = obj
        .get_ref("direction")
        .map(|r| r.rsplit(['#', '.']).next().unwrap_or(r).to_lowercase())
        .unwrap_or_default();

    // Also check limitType for voltage direction hints
    let lt = obj
        .get_ref("limitType")
        .or_else(|| obj.get_text("limitType"))
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if dir.contains("high") || lt.contains("high") {
        LimitDirection::High
    } else if dir.contains("low") || lt.contains("low") {
        LimitDirection::Low
    } else {
        LimitDirection::AbsoluteValue
    }
}

/// Parse a single limit value object and return (ols_mrid, OperationalLimit).
fn parse_limit_value<'a>(
    obj: &'a CimObj,
    _kind: LimitKind,
    olt_map: &HashMap<&str, (LimitDuration, LimitDirection)>,
) -> Option<(&'a str, OperationalLimit)> {
    let ols_id = obj.get_ref("OperationalLimitSet")?;
    let value = obj
        .parse_f64("value")
        .filter(|v| v.is_finite() && *v > 0.0)?;

    let (duration, direction, limit_type_mrid) =
        if let Some(olt_id) = obj.get_ref("OperationalLimitType") {
            let (dur, dir) = olt_map
                .get(olt_id)
                .copied()
                .unwrap_or((LimitDuration::Permanent, LimitDirection::AbsoluteValue));
            (dur, dir, Some(olt_id.to_string()))
        } else {
            (
                LimitDuration::Permanent,
                LimitDirection::AbsoluteValue,
                None,
            )
        };

    Some((
        ols_id,
        OperationalLimit {
            value,
            duration,
            direction,
            limit_type_mrid,
        },
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgmes::types::{CimObj, CimVal};

    /// Helper: create a minimal CIM object with the given class and attributes.
    fn cim_obj(class: &str, attrs: &[(&str, CimVal)]) -> CimObj {
        let mut o = CimObj::new(class);
        for (k, v) in attrs {
            o.attrs.insert(k.to_string(), v.clone());
        }
        o
    }

    fn text(s: &str) -> CimVal {
        CimVal::Text(s.to_string())
    }

    fn reference(s: &str) -> CimVal {
        CimVal::Ref(s.to_string())
    }

    #[test]
    fn test_parse_duration_patl() {
        let obj = cim_obj(
            "OperationalLimitType",
            &[("isInfiniteDuration", text("true"))],
        );
        assert_eq!(parse_duration(&obj), LimitDuration::Permanent);
    }

    #[test]
    fn test_parse_duration_tatl_with_seconds() {
        let obj = cim_obj(
            "OperationalLimitType",
            &[("acceptableDuration", text("1200"))],
        );
        assert_eq!(parse_duration(&obj), LimitDuration::Temporary(1200.0));
    }

    #[test]
    fn test_parse_duration_iatl() {
        let obj = cim_obj("OperationalLimitType", &[("acceptableDuration", text("0"))]);
        assert_eq!(parse_duration(&obj), LimitDuration::Instantaneous);
    }

    #[test]
    fn test_parse_duration_from_limit_type_keyword() {
        let obj = cim_obj(
            "OperationalLimitType",
            &[("limitType", reference("http://example.com#patl"))],
        );
        assert_eq!(parse_duration(&obj), LimitDuration::Permanent);

        let obj2 = cim_obj(
            "OperationalLimitType",
            &[("limitType", reference("http://example.com#tatl"))],
        );
        assert_eq!(parse_duration(&obj2), LimitDuration::Temporary(900.0));
    }

    #[test]
    fn test_parse_direction() {
        let high = cim_obj(
            "OperationalLimitType",
            &[("direction", reference("http://iec.ch#high"))],
        );
        assert_eq!(parse_direction(&high), LimitDirection::High);

        let low = cim_obj(
            "OperationalLimitType",
            &[("direction", reference("http://iec.ch#low"))],
        );
        assert_eq!(parse_direction(&low), LimitDirection::Low);

        let abs = cim_obj(
            "OperationalLimitType",
            &[("direction", reference("http://iec.ch#absoluteValue"))],
        );
        assert_eq!(parse_direction(&abs), LimitDirection::AbsoluteValue);
    }

    #[test]
    fn test_parse_limit_value_apparent_power() {
        let olt_map: HashMap<&str, (LimitDuration, LimitDirection)> = HashMap::from([(
            "olt1",
            (LimitDuration::Permanent, LimitDirection::AbsoluteValue),
        )]);

        let obj = cim_obj(
            "ApparentPowerLimit",
            &[
                ("OperationalLimitSet", reference("ols1")),
                ("OperationalLimitType", reference("olt1")),
                ("value", text("500.0")),
            ],
        );

        let (ols_id, limit) = parse_limit_value(&obj, LimitKind::ApparentPower, &olt_map).unwrap();
        assert_eq!(ols_id, "ols1");
        assert!((limit.value - 500.0).abs() < 1e-9);
        assert_eq!(limit.duration, LimitDuration::Permanent);
        assert_eq!(limit.direction, LimitDirection::AbsoluteValue);
        assert_eq!(limit.limit_type_mrid.as_deref(), Some("olt1"));
    }

    #[test]
    fn test_parse_limit_value_missing_set_returns_none() {
        let olt_map: HashMap<&str, (LimitDuration, LimitDirection)> = HashMap::new();
        let obj = cim_obj("ActivePowerLimit", &[("value", text("100.0"))]);
        assert!(parse_limit_value(&obj, LimitKind::ActivePower, &olt_map).is_none());
    }

    #[test]
    fn test_operational_limits_accessors() {
        let mut ol = surge_network::network::op_limits::OperationalLimits::default();
        assert!(ol.is_empty());
        assert_eq!(ol.total_limit_count(), 0);

        ol.limit_sets.insert(
            "set1".to_string(),
            surge_network::network::op_limits::OperationalLimitSet {
                mrid: "set1".to_string(),
                name: "Set 1".to_string(),
                bus: 1,
                equipment_mrid: Some("eq1".to_string()),
                from_end: Some(true),
                limits: vec![
                    (
                        LimitKind::ApparentPower,
                        OperationalLimit {
                            value: 500.0,
                            duration: LimitDuration::Permanent,
                            direction: LimitDirection::AbsoluteValue,
                            limit_type_mrid: None,
                        },
                    ),
                    (
                        LimitKind::ApparentPower,
                        OperationalLimit {
                            value: 600.0,
                            duration: LimitDuration::Temporary(900.0),
                            direction: LimitDirection::AbsoluteValue,
                            limit_type_mrid: None,
                        },
                    ),
                ],
            },
        );

        assert!(!ol.is_empty());
        assert_eq!(ol.total_limit_count(), 2);
        assert_eq!(ol.sets_for_equipment("eq1").count(), 1);
        assert_eq!(ol.sets_for_equipment("eq_other").count(), 0);
        assert_eq!(ol.sets_for_bus(1).count(), 1);
        assert_eq!(ol.sets_for_bus(99).count(), 0);
    }
}
