// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES Measurement profile parser.
//!
//! Parses IEC 61970-301 Measurement package classes:
//! - **Analog** / **AnalogValue** — continuous measurements (P, Q, V, I, frequency)
//! - **Discrete** / **DiscreteValue** — status measurements (switch position, tap position)
//! - **Accumulator** / **AccumulatorValue** — energy counters (MWh)
//! - **MeasurementValueSource** — SCADA / PMU / manual provenance
//!
//! Terminal references are resolved to bus numbers via `CgmesIndices`. For flow
//! measurements, the equipment mRID is used to locate the branch circuit ID.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::measurement::{
    CimMeasurement, CimMeasurementType, MeasurementQuality, MeasurementSource,
};

use super::indices::CgmesIndices;
use super::types::ObjMap;

// ---------------------------------------------------------------------------
// measurementType string → CimMeasurementType mapping
// ---------------------------------------------------------------------------

/// Map CIM `measurementType` string to our enum.
///
/// CIM uses strings like "ThreePhaseActivePower", "LinePosSeqVoltage", etc.
/// We also accept common shorthand names found in practice.
fn classify_measurement_type(s: &str) -> Option<CimMeasurementType> {
    let lower = s.to_lowercase();
    // Reactive power variants — check BEFORE active power (since "reactivepower" contains "activepower")
    if lower.contains("reactivepower") || lower == "q" || lower == "mvar" {
        return Some(CimMeasurementType::ReactivePower);
    }
    // Active power variants
    if lower.contains("activepower") || lower == "p" || lower == "mw" {
        return Some(CimMeasurementType::ActivePower);
    }
    // Voltage magnitude
    if lower.contains("voltage") && !lower.contains("angle") && !lower.contains("phasor") {
        return Some(CimMeasurementType::VoltageMagnitude);
    }
    // Voltage angle
    if lower.contains("voltageangle") || lower.contains("angle") {
        return Some(CimMeasurementType::VoltageAngle);
    }
    // Current magnitude
    if lower.contains("current") && !lower.contains("phasor") {
        return Some(CimMeasurementType::CurrentMagnitude);
    }
    // Frequency
    if lower.contains("frequency") || lower == "f" || lower == "hz" {
        return Some(CimMeasurementType::Frequency);
    }
    // Tap position
    if lower.contains("tapposition") || lower.contains("tap") {
        return Some(CimMeasurementType::TapPosition);
    }
    // Switch status
    if lower.contains("switchposition") || lower.contains("switch") || lower.contains("breaker") {
        return Some(CimMeasurementType::SwitchStatus);
    }
    // Energy accumulator
    if lower.contains("energy") || lower.contains("mwh") || lower.contains("accumulator") {
        return Some(CimMeasurementType::EnergyAccumulator);
    }
    // PMU phasor types
    if lower.contains("pmuvoltagereal") || lower.contains("pmu_vr") {
        return Some(CimMeasurementType::PmuVoltageReal);
    }
    if lower.contains("pmuvoltageimaginary") || lower.contains("pmu_vi") {
        return Some(CimMeasurementType::PmuVoltageImaginary);
    }
    if lower.contains("pmucurrentreal") || lower.contains("pmu_ir") {
        return Some(CimMeasurementType::PmuCurrentReal);
    }
    if lower.contains("pmucurrentimaginary") || lower.contains("pmu_ii") {
        return Some(CimMeasurementType::PmuCurrentImaginary);
    }

    tracing::debug!(
        measurement_type = s,
        "unrecognized CIM measurementType — skipping"
    );
    None
}

/// Map `MeasurementValueSource.name` to our enum.
fn classify_source(name: &str) -> MeasurementSource {
    let lower = name.to_lowercase();
    if lower.contains("pmu") || lower.contains("synchrophasor") {
        MeasurementSource::Pmu
    } else if lower.contains("scada") || lower.contains("ems") || lower.contains("telemetry") {
        MeasurementSource::Scada
    } else if lower.contains("manual") || lower.contains("operator") {
        MeasurementSource::Manual
    } else if lower.contains("calc") || lower.contains("estimate") {
        MeasurementSource::Calculated
    } else {
        MeasurementSource::Other
    }
}

/// Map CIM validity string to our quality enum.
fn classify_quality(validity: &str) -> MeasurementQuality {
    let lower = validity.to_lowercase();
    // Check bad/invalid BEFORE good/valid (since "invalid" contains "valid")
    if lower.contains("bad") || lower.contains("invalid") {
        MeasurementQuality::Bad
    } else if lower.contains("suspect") || lower.contains("questionable") {
        MeasurementQuality::Suspect
    } else if lower.contains("missing") || lower.contains("old") {
        MeasurementQuality::Missing
    } else {
        // Default: anything containing "good"/"valid" or unrecognized → Good
        MeasurementQuality::Good
    }
}

/// Default sigma (standard deviation) for SE measurement weighting.
///
/// Industry-standard values: voltage ±0.01 pu, power ±0.02 pu, current ±0.05 pu.
fn default_sigma(measurement_type: CimMeasurementType) -> f64 {
    match measurement_type {
        CimMeasurementType::VoltageMagnitude | CimMeasurementType::VoltageAngle => 0.01,
        CimMeasurementType::ActivePower | CimMeasurementType::ReactivePower => 0.02,
        CimMeasurementType::CurrentMagnitude => 0.05,
        CimMeasurementType::PmuVoltageReal | CimMeasurementType::PmuVoltageImaginary => 0.005,
        CimMeasurementType::PmuCurrentReal | CimMeasurementType::PmuCurrentImaginary => 0.01,
        CimMeasurementType::Frequency => 0.001,
        CimMeasurementType::TapPosition => 1.0,
        CimMeasurementType::SwitchStatus => 0.01,
        CimMeasurementType::EnergyAccumulator => 0.05,
    }
}

// ---------------------------------------------------------------------------
// Intermediate structs for the two-pass parse
// ---------------------------------------------------------------------------

/// Parsed measurement definition (Analog / Discrete / Accumulator).
struct MeasDef {
    mrid: String,
    name: String,
    measurement_type: CimMeasurementType,
    terminal_mrid: Option<String>,
    equipment_mrid: Option<String>,
    positive_flow_in: bool,
}

/// Parsed measurement value (AnalogValue / DiscreteValue / AccumulatorValue).
struct MeasVal {
    meas_mrid: String,
    value: f64,
    source_mrid: Option<String>,
    quality: MeasurementQuality,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Build CIM measurements from the CGMES object store and wire into the network.
///
/// # Parsing strategy
/// 1. First pass: collect Analog/Discrete/Accumulator definitions
/// 2. Second pass: collect AnalogValue/DiscreteValue/AccumulatorValue → link to definitions
/// 3. Third pass: collect MeasurementValueSource → source name map
/// 4. Resolve Terminal → bus number via CgmesIndices
/// 5. For flow measurements, resolve Terminal → branch via equipment mRID
/// 6. Build `Vec<CimMeasurement>` and set on `network.cim.measurements`
pub(crate) fn build_measurements(objects: &ObjMap, idx: &CgmesIndices, network: &mut Network) {
    // ── Pass 1: Measurement definitions ─────────────────────────────────────
    let mut defs: HashMap<String, MeasDef> = HashMap::new();

    for (mrid, obj) in objects.iter() {
        let is_analog = obj.class == "Analog";
        let is_discrete = obj.class == "Discrete";
        let is_accumulator = obj.class == "Accumulator";

        if !is_analog && !is_discrete && !is_accumulator {
            continue;
        }

        let type_str = obj.get_text("measurementType").unwrap_or("");
        let measurement_type = if is_discrete {
            // Discrete measurements: map to TapPosition or SwitchStatus
            if type_str.to_lowercase().contains("tap") {
                Some(CimMeasurementType::TapPosition)
            } else {
                Some(CimMeasurementType::SwitchStatus)
            }
        } else if is_accumulator {
            Some(CimMeasurementType::EnergyAccumulator)
        } else {
            classify_measurement_type(type_str)
        };

        let measurement_type = match measurement_type {
            Some(mt) => mt,
            None => continue,
        };

        let name = obj.get_text("name").unwrap_or("").to_string();

        let terminal_mrid = obj.get_ref("Terminal").map(|s| s.to_string());
        let equipment_mrid = obj.get_ref("PowerSystemResource").map(|s| s.to_string());

        let positive_flow_in = obj
            .get_text("positiveFlowIn")
            .map(|s| s.to_lowercase() == "true")
            .unwrap_or(true);

        defs.insert(
            mrid.clone(),
            MeasDef {
                mrid: mrid.clone(),
                name,
                measurement_type,
                terminal_mrid,
                equipment_mrid,
                positive_flow_in,
            },
        );
    }

    if defs.is_empty() {
        return;
    }

    // ── Pass 2: Measurement values ──────────────────────────────────────────
    let mut vals: Vec<MeasVal> = Vec::new();

    for (_vid, obj) in objects.iter() {
        let (meas_ref_key, is_float) = match obj.class.as_str() {
            "AnalogValue" => ("Analog", true),
            "DiscreteValue" => ("Discrete", false),
            "AccumulatorValue" => ("Accumulator", false),
            _ => continue,
        };

        let meas_mrid = match obj.get_ref(meas_ref_key) {
            Some(r) => r.to_string(),
            None => continue,
        };

        // Skip values whose definition was not parsed (e.g., unrecognized type)
        if !defs.contains_key(&meas_mrid) {
            continue;
        }

        let value = if is_float {
            match obj.parse_f64("value") {
                Some(v) => v,
                None => continue, // skip measurements with no value
            }
        } else {
            // Discrete/Accumulator: integer value
            match obj.get_text("value").and_then(|s| s.parse::<i64>().ok()) {
                Some(v) => v as f64,
                None => continue,
            }
        };

        let source_mrid = obj.get_ref("MeasurementValueSource").map(|s| s.to_string());

        let quality = obj
            .get_text("quality")
            .or_else(|| obj.get_text("validity"))
            .map(classify_quality)
            .unwrap_or(MeasurementQuality::Good);

        vals.push(MeasVal {
            meas_mrid,
            value,
            source_mrid,
            quality,
        });
    }

    // ── Pass 3: MeasurementValueSource → name map ───────────────────────────
    let source_names: HashMap<String, MeasurementSource> = objects
        .iter()
        .filter(|(_, o)| o.class == "MeasurementValueSource")
        .map(|(id, obj)| {
            let name = obj.get_text("name").unwrap_or("Other");
            (id.clone(), classify_source(name))
        })
        .collect();

    // ── Build equipment → branch circuit lookup ─────────────────────────────
    // Branch.circuit stores the equipment mRID in CGMES-parsed networks.
    let eq_to_circuit: HashMap<&str, &str> = network
        .branches
        .iter()
        .filter(|br| !br.circuit.is_empty())
        .map(|br| (br.circuit.as_str(), br.circuit.as_str()))
        .collect();

    // ── Resolve and assemble CimMeasurements ────────────────────────────────
    // Group values by measurement mRID. If multiple values exist for one
    // definition (time series), take the last one (most recent).
    let mut val_by_meas: HashMap<&str, &MeasVal> = HashMap::new();
    for v in &vals {
        val_by_meas.insert(&v.meas_mrid, v);
    }

    let mut measurements: Vec<CimMeasurement> = Vec::new();

    for (meas_mrid, def) in &defs {
        let val = match val_by_meas.get(meas_mrid.as_str()) {
            Some(v) => *v,
            None => continue, // definition without value — skip
        };

        // Resolve Terminal → bus number
        let bus = def.terminal_mrid.as_deref().and_then(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });

        let bus = match bus {
            Some(b) => b,
            None => {
                tracing::debug!(
                    mrid = %def.mrid,
                    name = %def.name,
                    "measurement terminal could not be resolved to a bus — skipping"
                );
                continue;
            }
        };

        // For flow/current measurements, try to resolve equipment → branch circuit
        let is_flow = matches!(
            def.measurement_type,
            CimMeasurementType::ActivePower
                | CimMeasurementType::ReactivePower
                | CimMeasurementType::CurrentMagnitude
                | CimMeasurementType::PmuCurrentReal
                | CimMeasurementType::PmuCurrentImaginary
        );

        let branch_circuit = if is_flow {
            def.equipment_mrid
                .as_deref()
                .and_then(|eq| eq_to_circuit.get(eq).copied())
                .map(|s| s.to_string())
        } else {
            None
        };

        // Determine from-end: check if the terminal is the first terminal of the equipment
        let from_end = if is_flow {
            def.equipment_mrid
                .as_deref()
                .and_then(|eq_id| {
                    let terms = idx.terminals(eq_id);
                    let tid = def.terminal_mrid.as_deref()?;
                    terms.first().map(|first| first == tid)
                })
                .unwrap_or(def.positive_flow_in)
        } else {
            def.positive_flow_in
        };

        // Resolve source
        let source = val
            .source_mrid
            .as_deref()
            .and_then(|sid| source_names.get(sid).copied())
            .unwrap_or(MeasurementSource::Scada);

        let sigma = default_sigma(def.measurement_type);

        measurements.push(CimMeasurement {
            mrid: def.mrid.clone(),
            name: def.name.clone(),
            measurement_type: def.measurement_type,
            bus,
            branch_circuit,
            from_end,
            value: val.value,
            sigma,
            enabled: true,
            source,
            quality: val.quality,
            terminal_mrid: def.terminal_mrid.clone(),
        });
    }

    if !measurements.is_empty() {
        tracing::info!(
            count = measurements.len(),
            analog = defs
                .values()
                .filter(|d| !matches!(
                    d.measurement_type,
                    CimMeasurementType::TapPosition
                        | CimMeasurementType::SwitchStatus
                        | CimMeasurementType::EnergyAccumulator
                ))
                .count(),
            discrete = defs
                .values()
                .filter(|d| matches!(
                    d.measurement_type,
                    CimMeasurementType::TapPosition | CimMeasurementType::SwitchStatus
                ))
                .count(),
            accumulator = defs
                .values()
                .filter(|d| matches!(d.measurement_type, CimMeasurementType::EnergyAccumulator))
                .count(),
            "CGMES Measurement profile → Network.cim.measurements"
        );
    }

    network.cim.measurements = measurements;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgmes::types::{CimObj, CimVal};

    /// Helper to insert a CIM object into the map.
    fn insert_obj(map: &mut ObjMap, id: &str, class: &str, attrs: &[(&str, CimVal)]) {
        let mut obj = CimObj::new(class);
        for (k, v) in attrs {
            obj.attrs.insert(k.to_string(), v.clone());
        }
        map.insert(id.to_string(), obj);
    }

    fn text(s: &str) -> CimVal {
        CimVal::Text(s.to_string())
    }

    fn refval(s: &str) -> CimVal {
        CimVal::Ref(s.to_string())
    }

    #[test]
    fn test_classify_measurement_type() {
        assert_eq!(
            classify_measurement_type("ThreePhaseActivePower"),
            Some(CimMeasurementType::ActivePower)
        );
        assert_eq!(
            classify_measurement_type("ThreePhaseReactivePower"),
            Some(CimMeasurementType::ReactivePower)
        );
        assert_eq!(
            classify_measurement_type("LinePosSeqVoltage"),
            Some(CimMeasurementType::VoltageMagnitude)
        );
        assert_eq!(
            classify_measurement_type("LineCurrentMagnitude"),
            Some(CimMeasurementType::CurrentMagnitude)
        );
        assert_eq!(
            classify_measurement_type("Frequency"),
            Some(CimMeasurementType::Frequency)
        );
        assert_eq!(
            classify_measurement_type("SwitchPosition"),
            Some(CimMeasurementType::SwitchStatus)
        );
        assert_eq!(classify_measurement_type("UnknownThing"), None);
    }

    #[test]
    fn test_classify_source() {
        assert_eq!(classify_source("SCADA"), MeasurementSource::Scada);
        assert_eq!(classify_source("PMU_Station"), MeasurementSource::Pmu);
        assert_eq!(classify_source("ManualEntry"), MeasurementSource::Manual);
        assert_eq!(classify_source("Calculated"), MeasurementSource::Calculated);
        assert_eq!(classify_source("FooBar"), MeasurementSource::Other);
    }

    #[test]
    fn test_classify_quality() {
        assert_eq!(classify_quality("GOOD"), MeasurementQuality::Good);
        assert_eq!(classify_quality("SUSPECT"), MeasurementQuality::Suspect);
        assert_eq!(classify_quality("INVALID"), MeasurementQuality::Bad);
        assert_eq!(classify_quality("OLD_DATA"), MeasurementQuality::Missing);
    }

    #[test]
    fn test_build_measurements_analog() {
        let mut objects: ObjMap = HashMap::new();

        // TopologicalNode
        insert_obj(
            &mut objects,
            "tn1",
            "TopologicalNode",
            &[("name", text("TN1"))],
        );

        // Terminal linked to equipment and TN
        insert_obj(
            &mut objects,
            "term1",
            "Terminal",
            &[
                ("ConductingEquipment", refval("line1")),
                ("TopologicalNode", refval("tn1")),
                ("sequenceNumber", text("1")),
            ],
        );

        // Analog measurement definition
        insert_obj(
            &mut objects,
            "meas1",
            "Analog",
            &[
                ("name", text("Line1_P")),
                ("measurementType", text("ThreePhaseActivePower")),
                ("Terminal", refval("term1")),
                ("PowerSystemResource", refval("line1")),
                ("positiveFlowIn", text("true")),
            ],
        );

        // AnalogValue
        insert_obj(
            &mut objects,
            "val1",
            "AnalogValue",
            &[("Analog", refval("meas1")), ("value", text("150.5"))],
        );

        // MeasurementValueSource
        insert_obj(
            &mut objects,
            "src1",
            "MeasurementValueSource",
            &[("name", text("SCADA"))],
        );

        // Build indices and a minimal network with one bus
        let mut idx = CgmesIndices::build(&objects);
        idx.tn_bus.insert("tn1".to_string(), 1);

        let mut network = Network::default();
        network.buses.push(surge_network::network::Bus {
            number: 1,
            ..Default::default()
        });
        // Add a branch with circuit = equipment mRID
        network.branches.push(surge_network::network::Branch {
            from_bus: 1,
            to_bus: 2,
            circuit: "line1".to_string(),
            ..Default::default()
        });

        build_measurements(&objects, &idx, &mut network);

        assert_eq!(network.cim.measurements.len(), 1);
        let m = &network.cim.measurements[0];
        assert_eq!(m.mrid, "meas1");
        assert_eq!(m.name, "Line1_P");
        assert_eq!(m.measurement_type, CimMeasurementType::ActivePower);
        assert_eq!(m.bus, 1);
        assert_eq!(m.branch_circuit.as_deref(), Some("line1"));
        assert!(m.from_end);
        assert!((m.value - 150.5).abs() < 1e-9);
        assert_eq!(m.sigma, 0.02); // default for power
        assert!(m.enabled);
        assert_eq!(m.source, MeasurementSource::Scada);
        assert_eq!(m.quality, MeasurementQuality::Good);
    }

    #[test]
    fn test_build_measurements_discrete() {
        let mut objects: ObjMap = HashMap::new();

        insert_obj(
            &mut objects,
            "tn1",
            "TopologicalNode",
            &[("name", text("TN1"))],
        );
        insert_obj(
            &mut objects,
            "term1",
            "Terminal",
            &[
                ("ConductingEquipment", refval("sw1")),
                ("TopologicalNode", refval("tn1")),
                ("sequenceNumber", text("1")),
            ],
        );
        insert_obj(
            &mut objects,
            "disc1",
            "Discrete",
            &[
                ("name", text("SW1_Status")),
                ("measurementType", text("SwitchPosition")),
                ("Terminal", refval("term1")),
            ],
        );
        insert_obj(
            &mut objects,
            "dval1",
            "DiscreteValue",
            &[("Discrete", refval("disc1")), ("value", text("1"))],
        );

        let mut idx = CgmesIndices::build(&objects);
        idx.tn_bus.insert("tn1".to_string(), 5);

        let mut network = Network::default();
        network.buses.push(surge_network::network::Bus {
            number: 5,
            ..Default::default()
        });

        build_measurements(&objects, &idx, &mut network);

        assert_eq!(network.cim.measurements.len(), 1);
        let m = &network.cim.measurements[0];
        assert_eq!(m.measurement_type, CimMeasurementType::SwitchStatus);
        assert_eq!(m.bus, 5);
        assert!((m.value - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_build_measurements_no_value_skipped() {
        let mut objects: ObjMap = HashMap::new();

        insert_obj(
            &mut objects,
            "tn1",
            "TopologicalNode",
            &[("name", text("TN1"))],
        );
        insert_obj(
            &mut objects,
            "term1",
            "Terminal",
            &[
                ("ConductingEquipment", refval("gen1")),
                ("TopologicalNode", refval("tn1")),
                ("sequenceNumber", text("1")),
            ],
        );
        // Analog definition with no corresponding AnalogValue
        insert_obj(
            &mut objects,
            "meas_no_val",
            "Analog",
            &[
                ("name", text("Gen1_P")),
                ("measurementType", text("ThreePhaseActivePower")),
                ("Terminal", refval("term1")),
            ],
        );

        let mut idx = CgmesIndices::build(&objects);
        idx.tn_bus.insert("tn1".to_string(), 1);

        let mut network = Network::default();
        network.buses.push(surge_network::network::Bus {
            number: 1,
            ..Default::default()
        });

        build_measurements(&objects, &idx, &mut network);

        // No value → measurement should be skipped
        assert!(network.cim.measurements.is_empty());
    }

    #[test]
    fn test_build_measurements_voltage() {
        let mut objects: ObjMap = HashMap::new();

        insert_obj(
            &mut objects,
            "tn1",
            "TopologicalNode",
            &[("name", text("TN1"))],
        );
        insert_obj(
            &mut objects,
            "term1",
            "Terminal",
            &[
                ("ConductingEquipment", refval("bus1_eq")),
                ("TopologicalNode", refval("tn1")),
                ("sequenceNumber", text("1")),
            ],
        );
        insert_obj(
            &mut objects,
            "vmeas",
            "Analog",
            &[
                ("name", text("Bus1_V")),
                ("measurementType", text("LinePosSeqVoltage")),
                ("Terminal", refval("term1")),
            ],
        );
        insert_obj(
            &mut objects,
            "vval",
            "AnalogValue",
            &[("Analog", refval("vmeas")), ("value", text("230.5"))],
        );

        let mut idx = CgmesIndices::build(&objects);
        idx.tn_bus.insert("tn1".to_string(), 10);

        let mut network = Network::default();
        network.buses.push(surge_network::network::Bus {
            number: 10,
            ..Default::default()
        });

        build_measurements(&objects, &idx, &mut network);

        assert_eq!(network.cim.measurements.len(), 1);
        let m = &network.cim.measurements[0];
        assert_eq!(m.measurement_type, CimMeasurementType::VoltageMagnitude);
        assert_eq!(m.bus, 10);
        assert!((m.value - 230.5).abs() < 1e-9);
        assert_eq!(m.sigma, 0.01); // default for voltage
        // Voltage is not a flow measurement, so no branch_circuit
        assert!(m.branch_circuit.is_none());
    }

    #[test]
    fn test_build_measurements_accumulator() {
        let mut objects: ObjMap = HashMap::new();

        insert_obj(
            &mut objects,
            "tn1",
            "TopologicalNode",
            &[("name", text("TN1"))],
        );
        insert_obj(
            &mut objects,
            "term1",
            "Terminal",
            &[
                ("ConductingEquipment", refval("meter1")),
                ("TopologicalNode", refval("tn1")),
                ("sequenceNumber", text("1")),
            ],
        );
        insert_obj(
            &mut objects,
            "acc1",
            "Accumulator",
            &[
                ("name", text("Meter1_Energy")),
                ("measurementType", text("EnergyFlow")),
                ("Terminal", refval("term1")),
            ],
        );
        insert_obj(
            &mut objects,
            "aval1",
            "AccumulatorValue",
            &[("Accumulator", refval("acc1")), ("value", text("12345"))],
        );

        let mut idx = CgmesIndices::build(&objects);
        idx.tn_bus.insert("tn1".to_string(), 3);

        let mut network = Network::default();
        network.buses.push(surge_network::network::Bus {
            number: 3,
            ..Default::default()
        });

        build_measurements(&objects, &idx, &mut network);

        assert_eq!(network.cim.measurements.len(), 1);
        let m = &network.cim.measurements[0];
        assert_eq!(m.measurement_type, CimMeasurementType::EnergyAccumulator);
        assert!((m.value - 12345.0).abs() < 1e-9);
    }
}
