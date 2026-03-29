// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES Asset & Wire Info parser (IEC 61968-11 Asset package).
//!
//! Parses physical conductor, cable, tower geometry, transformer nameplate,
//! and general asset metadata from CIM objects:
//!
//! - **WireInfo** / **OverheadWireInfo** — conductor AC/DC resistance, GMR, ampacity
//! - **CableInfo** / **ConcentricNeutralCableInfo** / **TapeShieldCableInfo** — underground cable
//! - **WireSpacingInfo** + **WirePosition** — tower geometry / conductor positions
//! - **TransformerTankInfo** + **TransformerEndInfo** — transformer nameplate data
//! - **NoLoadTest** / **ShortCircuitTest** — factory test results
//! - **TransformerCoreAdmittance** — core loss data
//! - **Asset** + **ProductAssetModel** — serial number, manufacturer, dates

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::asset::{
    AssetCatalog, AssetMetadata, CableProperties, TransformerInfoData, TransformerWindingInfo,
    WirePosition, WireProperties, WireSpacing,
};
use surge_network::network::time_utils::parse_iso8601;

use super::indices::CgmesIndices;
use super::types::ObjMap;

// ---------------------------------------------------------------------------
// Wire & Conductor Info
// ---------------------------------------------------------------------------

/// Parse base WireInfo fields common to overhead and cable conductors.
fn parse_wire_properties(obj: &super::types::CimObj) -> WireProperties {
    WireProperties {
        name: obj.get_text("name").unwrap_or("").to_string(),
        r_ac75_ohm_per_km: obj.parse_f64("rAC75"),
        r_dc20_ohm_per_km: obj.parse_f64("rDC20"),
        gmr_m: obj.parse_f64("gmr"),
        radius_m: obj.parse_f64("radius"),
        size_description: obj.get_text("sizeDescription").map(|s| s.to_string()),
        material: obj
            .get_text("material")
            .map(|s| strip_cim_enum_prefix(s).to_string()),
        strand_count: obj
            .get_text("strandCount")
            .and_then(|s| s.parse::<u32>().ok()),
        core_strand_count: obj
            .get_text("coreStrandCount")
            .and_then(|s| s.parse::<u32>().ok()),
        rated_current_a: obj.parse_f64("ratedCurrent"),
    }
}

/// Strip CIM enum namespace prefix (e.g., "WireMaterialKind.aluminum" → "aluminum").
fn strip_cim_enum_prefix(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

// ---------------------------------------------------------------------------
// Cable Info
// ---------------------------------------------------------------------------

/// Parse CableInfo / ConcentricNeutralCableInfo / TapeShieldCableInfo.
fn parse_cable_properties(obj: &super::types::CimObj) -> CableProperties {
    CableProperties {
        wire: parse_wire_properties(obj),
        nominal_temperature_c: obj.parse_f64("nominalTemperature"),
        insulation_material: obj
            .get_text("insulationMaterial")
            .map(|s| strip_cim_enum_prefix(s).to_string()),
        insulation_thickness_mm: obj.parse_f64("insulationThickness"),
        outer_jacket_thickness_mm: obj.parse_f64("outerJacketThickness"),
        shield_material: obj
            .get_text("shieldMaterial")
            .map(|s| strip_cim_enum_prefix(s).to_string()),
        diameter_over_insulation_mm: obj.parse_f64("diameterOverInsulation"),
        diameter_over_jacket_mm: obj.parse_f64("diameterOverJacket"),
        diameter_over_screen_mm: obj.parse_f64("diameterOverScreen"),
        is_strand_fill: obj
            .get_text("isStrandFill")
            .map(|s| s.to_lowercase() == "true"),
        // Concentric neutral
        neutral_strand_count: obj
            .get_text("neutralStrandCount")
            .and_then(|s| s.parse::<u32>().ok()),
        neutral_strand_gmr_m: obj.parse_f64("neutralStrandGmr"),
        neutral_strand_radius_m: obj.parse_f64("neutralStrandRadius"),
        neutral_strand_rdc20_ohm_per_km: obj.parse_f64("neutralStrandRDC20"),
        // Tape shield
        tape_thickness_mm: obj.parse_f64("tapeThickness"),
        tape_lap_percent: obj.parse_f64("tapeLap"),
    }
}

// ---------------------------------------------------------------------------
// Wire Spacing & Positions
// ---------------------------------------------------------------------------

/// Collect WirePosition children for a given WireSpacingInfo mRID.
fn collect_wire_positions(objects: &ObjMap, spacing_id: &str) -> Vec<WirePosition> {
    let mut positions: Vec<WirePosition> = objects
        .iter()
        .filter(|(_, o)| o.class == "WirePosition")
        .filter(|(_, o)| {
            o.get_ref("WireSpacingInfo")
                .map(|r| r == spacing_id)
                .unwrap_or(false)
        })
        .map(|(_, o)| WirePosition {
            x_m: o.parse_f64("xCoord").unwrap_or(0.0),
            y_m: o.parse_f64("yCoord").unwrap_or(0.0),
            phase: o
                .get_text("phase")
                .map(|s| strip_cim_enum_prefix(s).to_string()),
            sequence_number: o
                .get_text("sequenceNumber")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0),
        })
        .collect();
    positions.sort_by_key(|p| p.sequence_number);
    positions
}

// ---------------------------------------------------------------------------
// Transformer Info
// ---------------------------------------------------------------------------

/// Collect TransformerEndInfo children for a given TransformerTankInfo mRID.
fn collect_winding_infos(objects: &ObjMap, tank_id: &str) -> Vec<TransformerWindingInfo> {
    let mut windings: Vec<TransformerWindingInfo> = objects
        .iter()
        .filter(|(_, o)| o.class == "TransformerEndInfo")
        .filter(|(_, o)| {
            o.get_ref("TransformerTankInfo")
                .map(|r| r == tank_id)
                .unwrap_or(false)
        })
        .map(|(_, o)| TransformerWindingInfo {
            end_number: o
                .get_text("endNumber")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0),
            rated_s_mva: o.parse_f64("ratedS").map(|va| va / 1e6),
            rated_u_kv: o.parse_f64("ratedU").map(|v| v / 1e3),
            r_ohm: o.parse_f64("r"),
            connection_kind: o
                .get_text("connectionKind")
                .map(|s| strip_cim_enum_prefix(s).to_string()),
            insulation_u_kv: o.parse_f64("insulationU").map(|v| v / 1e3),
            short_term_s_mva: o.parse_f64("shortTermS").map(|va| va / 1e6),
        })
        .collect();
    windings.sort_by_key(|w| w.end_number);
    windings
}

/// Find NoLoadTest data for a TransformerTankInfo or its parent PowerTransformerInfo.
fn find_no_load_test(objects: &ObjMap, tank_id: &str) -> (Option<f64>, Option<f64>) {
    for (_, o) in objects.iter().filter(|(_, o)| o.class == "NoLoadTest") {
        let energised_end = o
            .get_ref("EnergisedEnd")
            .or_else(|| o.get_ref("TransformerTankInfo"));
        if energised_end.map(|r| r == tank_id).unwrap_or(false) {
            return (o.parse_f64("loss"), o.parse_f64("excitingCurrent"));
        }
    }
    (None, None)
}

/// Find ShortCircuitTest data for a TransformerTankInfo.
fn find_short_circuit_test(objects: &ObjMap, tank_id: &str) -> (Option<f64>, Option<f64>) {
    for (_, o) in objects
        .iter()
        .filter(|(_, o)| o.class == "ShortCircuitTest")
    {
        let energised_end = o
            .get_ref("EnergisedEnd")
            .or_else(|| o.get_ref("TransformerTankInfo"));
        if energised_end.map(|r| r == tank_id).unwrap_or(false) {
            return (o.parse_f64("loss"), o.parse_f64("leakageImpedance"));
        }
    }
    (None, None)
}

// ---------------------------------------------------------------------------
// Asset Metadata
// ---------------------------------------------------------------------------

/// Build ProductAssetModel mRID → (manufacturer, modelNumber) lookup.
fn build_product_model_map(objects: &ObjMap) -> HashMap<String, (Option<String>, Option<String>)> {
    objects
        .iter()
        .filter(|(_, o)| o.class == "ProductAssetModel")
        .map(|(id, o)| {
            let manufacturer = o
                .get_text("manufacturerName")
                .or_else(|| o.get_text("manufacturer"))
                .map(|s| s.to_string());
            let model_number = o.get_text("modelNumber").map(|s| s.to_string());
            (id.clone(), (manufacturer, model_number))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Build the asset catalog from the CGMES object store and wire into the network.
///
/// Single-pass over the ObjMap, classifying objects by CIM class and building
/// the appropriate asset data structures.
pub(crate) fn build_asset_catalog(objects: &ObjMap, _idx: &CgmesIndices, network: &mut Network) {
    let mut catalog = AssetCatalog::default();

    // Pre-build product model lookup for asset metadata
    let product_models = build_product_model_map(objects);

    for (mrid, obj) in objects.iter() {
        match obj.class.as_str() {
            // Wire/conductor info (overhead)
            "WireInfo" | "OverheadWireInfo" => {
                let props = parse_wire_properties(obj);
                tracing::debug!(mrid, name = %props.name, "parsed WireInfo");
                catalog.wire_infos.insert(mrid.clone(), props);
            }

            // Cable info (underground)
            "CableInfo" | "ConcentricNeutralCableInfo" | "TapeShieldCableInfo" => {
                let props = parse_cable_properties(obj);
                tracing::debug!(mrid, name = %props.wire.name, class = %obj.class, "parsed CableInfo");
                catalog.cable_infos.insert(mrid.clone(), props);
            }

            // Wire spacing / tower geometry
            "WireSpacingInfo" => {
                let positions = collect_wire_positions(objects, mrid);
                let spacing = WireSpacing {
                    name: obj.get_text("name").unwrap_or("").to_string(),
                    is_cable: obj
                        .get_text("isCable")
                        .map(|s| s.to_lowercase() == "true")
                        .unwrap_or(false),
                    phase_wire_count: obj
                        .get_text("phaseWireCount")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(1),
                    phase_wire_spacing_m: obj.parse_f64("phaseWireSpacing"),
                    positions,
                };
                tracing::debug!(
                    mrid,
                    name = %spacing.name,
                    n_positions = spacing.positions.len(),
                    "parsed WireSpacingInfo"
                );
                catalog.wire_spacings.insert(mrid.clone(), spacing);
            }

            // Transformer tank info
            "TransformerTankInfo" => {
                let windings = collect_winding_infos(objects, mrid);
                let (no_load_loss_w, exciting_current_pct) = find_no_load_test(objects, mrid);
                let (short_circuit_loss_w, leakage_impedance_pct) =
                    find_short_circuit_test(objects, mrid);

                let info = TransformerInfoData {
                    name: obj.get_text("name").unwrap_or("").to_string(),
                    windings,
                    no_load_loss_w,
                    exciting_current_pct,
                    short_circuit_loss_w,
                    leakage_impedance_pct,
                };
                tracing::debug!(
                    mrid,
                    name = %info.name,
                    n_windings = info.windings.len(),
                    "parsed TransformerTankInfo"
                );
                catalog.transformer_infos.insert(mrid.clone(), info);
            }

            // General asset metadata
            "Asset" => {
                let equipment_mrid = match obj
                    .get_ref("PowerSystemResources")
                    .or_else(|| obj.get_ref("PowerSystemResource"))
                {
                    Some(eq) => eq.to_string(),
                    None => continue,
                };

                let product_model_ref = obj
                    .get_ref("AssetInfo")
                    .or_else(|| obj.get_ref("ProductAssetModel"));

                let (manufacturer, model_number) = product_model_ref
                    .and_then(|pm_id| product_models.get(pm_id))
                    .cloned()
                    .unwrap_or((None, None));

                let meta = AssetMetadata {
                    equipment_mrid: equipment_mrid.clone(),
                    serial_number: obj.get_text("serialNumber").map(|s| s.to_string()),
                    manufacturer,
                    model_number,
                    manufactured_date: obj.get_text("manufacturedDate").and_then(parse_iso8601),
                    installation_date: obj
                        .get_text("installationDate")
                        .or_else(|| obj.get_text("lifecycle.installationDate"))
                        .and_then(parse_iso8601),
                    retired_date: obj
                        .get_text("retiredDate")
                        .or_else(|| obj.get_text("lifecycle.retiredDate"))
                        .and_then(parse_iso8601),
                };
                tracing::debug!(
                    mrid,
                    equipment = %meta.equipment_mrid,
                    "parsed Asset metadata"
                );
                catalog.asset_metadata.insert(equipment_mrid, meta);
            }

            _ => {}
        }
    }

    if !catalog.is_empty() {
        tracing::info!(
            wire_infos = catalog.wire_infos.len(),
            cable_infos = catalog.cable_infos.len(),
            wire_spacings = catalog.wire_spacings.len(),
            transformer_infos = catalog.transformer_infos.len(),
            asset_metadata = catalog.asset_metadata.len(),
            "CGMES Asset profile → Network.cim.asset_catalog"
        );
        network.cim.asset_catalog = catalog;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgmes::indices::CgmesIndices;
    use crate::cgmes::types::{CimObj, CimVal};

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
    fn test_parse_wire_info() {
        let mut objects: ObjMap = HashMap::new();
        insert_obj(
            &mut objects,
            "wire1",
            "WireInfo",
            &[
                ("name", text("Drake 795 kcmil")),
                ("rAC75", text("0.0726")),
                ("rDC20", text("0.0580")),
                ("gmr", text("0.01143")),
                ("radius", text("0.01407")),
                ("sizeDescription", text("795 kcmil")),
                ("material", text("WireMaterialKind.acsr")),
                ("strandCount", text("26")),
                ("coreStrandCount", text("7")),
                ("ratedCurrent", text("907.0")),
            ],
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert_eq!(network.cim.asset_catalog.wire_infos.len(), 1);
        let wire = &network.cim.asset_catalog.wire_infos["wire1"];
        assert_eq!(wire.name, "Drake 795 kcmil");
        assert!((wire.r_ac75_ohm_per_km.unwrap() - 0.0726).abs() < 1e-9);
        assert!((wire.r_dc20_ohm_per_km.unwrap() - 0.0580).abs() < 1e-9);
        assert!((wire.gmr_m.unwrap() - 0.01143).abs() < 1e-9);
        assert!((wire.radius_m.unwrap() - 0.01407).abs() < 1e-9);
        assert_eq!(wire.size_description.as_deref(), Some("795 kcmil"));
        assert_eq!(wire.material.as_deref(), Some("acsr"));
        assert_eq!(wire.strand_count, Some(26));
        assert_eq!(wire.core_strand_count, Some(7));
        assert!((wire.rated_current_a.unwrap() - 907.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_overhead_wire_info() {
        let mut objects: ObjMap = HashMap::new();
        insert_obj(
            &mut objects,
            "ohw1",
            "OverheadWireInfo",
            &[
                ("name", text("Dove")),
                ("rAC75", text("0.1094")),
                ("gmr", text("0.00814")),
                ("ratedCurrent", text("620.0")),
            ],
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert_eq!(network.cim.asset_catalog.wire_infos.len(), 1);
        let wire = &network.cim.asset_catalog.wire_infos["ohw1"];
        assert_eq!(wire.name, "Dove");
        assert!((wire.r_ac75_ohm_per_km.unwrap() - 0.1094).abs() < 1e-9);
    }

    #[test]
    fn test_parse_concentric_neutral_cable() {
        let mut objects: ObjMap = HashMap::new();
        insert_obj(
            &mut objects,
            "cnc1",
            "ConcentricNeutralCableInfo",
            &[
                ("name", text("1/0 Al CN")),
                ("rAC75", text("0.607")),
                ("rDC20", text("0.541")),
                ("gmr", text("0.00427")),
                ("radius", text("0.00554")),
                ("material", text("WireMaterialKind.aluminum")),
                ("ratedCurrent", text("200.0")),
                ("nominalTemperature", text("90.0")),
                (
                    "insulationMaterial",
                    text("CableConstructionKind.crosslinkedPolyethylene"),
                ),
                ("insulationThickness", text("5.59")),
                ("outerJacketThickness", text("1.27")),
                ("diameterOverInsulation", text("23.9")),
                ("diameterOverJacket", text("28.9")),
                ("neutralStrandCount", text("13")),
                ("neutralStrandGmr", text("0.00208")),
                ("neutralStrandRadius", text("0.00233")),
                ("neutralStrandRDC20", text("3.519")),
            ],
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert_eq!(network.cim.asset_catalog.cable_infos.len(), 1);
        let cable = &network.cim.asset_catalog.cable_infos["cnc1"];
        assert_eq!(cable.wire.name, "1/0 Al CN");
        assert_eq!(cable.wire.material.as_deref(), Some("aluminum"));
        assert!((cable.nominal_temperature_c.unwrap() - 90.0).abs() < 1e-9);
        assert_eq!(
            cable.insulation_material.as_deref(),
            Some("crosslinkedPolyethylene")
        );
        assert!((cable.insulation_thickness_mm.unwrap() - 5.59).abs() < 1e-9);
        assert_eq!(cable.neutral_strand_count, Some(13));
        assert!((cable.neutral_strand_gmr_m.unwrap() - 0.00208).abs() < 1e-9);
    }

    #[test]
    fn test_parse_tape_shield_cable() {
        let mut objects: ObjMap = HashMap::new();
        insert_obj(
            &mut objects,
            "tsc1",
            "TapeShieldCableInfo",
            &[
                ("name", text("2/0 Cu TS")),
                ("rAC75", text("0.341")),
                ("ratedCurrent", text("280.0")),
                ("tapeThickness", text("0.254")),
                ("tapeLap", text("25.0")),
            ],
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert_eq!(network.cim.asset_catalog.cable_infos.len(), 1);
        let cable = &network.cim.asset_catalog.cable_infos["tsc1"];
        assert_eq!(cable.wire.name, "2/0 Cu TS");
        assert!((cable.tape_thickness_mm.unwrap() - 0.254).abs() < 1e-9);
        assert!((cable.tape_lap_percent.unwrap() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_wire_spacing_with_positions() {
        let mut objects: ObjMap = HashMap::new();
        insert_obj(
            &mut objects,
            "ws1",
            "WireSpacingInfo",
            &[
                ("name", text("500kV H-frame")),
                ("isCable", text("false")),
                ("phaseWireCount", text("2")),
                ("phaseWireSpacing", text("0.457")),
            ],
        );
        // Three phase positions + neutral
        insert_obj(
            &mut objects,
            "wp_a",
            "WirePosition",
            &[
                ("WireSpacingInfo", refval("ws1")),
                ("xCoord", text("-6.096")),
                ("yCoord", text("19.812")),
                ("phase", text("SinglePhaseKind.A")),
                ("sequenceNumber", text("1")),
            ],
        );
        insert_obj(
            &mut objects,
            "wp_b",
            "WirePosition",
            &[
                ("WireSpacingInfo", refval("ws1")),
                ("xCoord", text("0.0")),
                ("yCoord", text("19.812")),
                ("phase", text("SinglePhaseKind.B")),
                ("sequenceNumber", text("2")),
            ],
        );
        insert_obj(
            &mut objects,
            "wp_c",
            "WirePosition",
            &[
                ("WireSpacingInfo", refval("ws1")),
                ("xCoord", text("6.096")),
                ("yCoord", text("19.812")),
                ("phase", text("SinglePhaseKind.C")),
                ("sequenceNumber", text("3")),
            ],
        );
        insert_obj(
            &mut objects,
            "wp_n",
            "WirePosition",
            &[
                ("WireSpacingInfo", refval("ws1")),
                ("xCoord", text("0.0")),
                ("yCoord", text("15.24")),
                ("phase", text("SinglePhaseKind.N")),
                ("sequenceNumber", text("4")),
            ],
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert_eq!(network.cim.asset_catalog.wire_spacings.len(), 1);
        let ws = &network.cim.asset_catalog.wire_spacings["ws1"];
        assert_eq!(ws.name, "500kV H-frame");
        assert!(!ws.is_cable);
        assert_eq!(ws.phase_wire_count, 2);
        assert!((ws.phase_wire_spacing_m.unwrap() - 0.457).abs() < 1e-9);
        assert_eq!(ws.positions.len(), 4);
        // Sorted by sequence_number
        assert_eq!(ws.positions[0].phase.as_deref(), Some("A"));
        assert!((ws.positions[0].x_m - (-6.096)).abs() < 1e-9);
        assert_eq!(ws.positions[3].phase.as_deref(), Some("N"));
        assert!((ws.positions[3].y_m - 15.24).abs() < 1e-9);
    }

    #[test]
    fn test_parse_transformer_tank_info_with_tests() {
        let mut objects: ObjMap = HashMap::new();
        insert_obj(
            &mut objects,
            "tank1",
            "TransformerTankInfo",
            &[("name", text("230/115 kV 200 MVA"))],
        );
        // Winding 1 (HV)
        insert_obj(
            &mut objects,
            "end1",
            "TransformerEndInfo",
            &[
                ("TransformerTankInfo", refval("tank1")),
                ("endNumber", text("1")),
                ("ratedS", text("200000000")), // 200 MVA in VA
                ("ratedU", text("230000")),    // 230 kV in V
                ("r", text("0.45")),
                ("connectionKind", text("WindingConnection.Y")),
                ("insulationU", text("550000")), // 550 kV BIL
            ],
        );
        // Winding 2 (LV)
        insert_obj(
            &mut objects,
            "end2",
            "TransformerEndInfo",
            &[
                ("TransformerTankInfo", refval("tank1")),
                ("endNumber", text("2")),
                ("ratedS", text("200000000")),
                ("ratedU", text("115000")),
                ("r", text("0.11")),
                ("connectionKind", text("WindingConnection.D")),
                ("shortTermS", text("250000000")),
            ],
        );
        // No-load test
        insert_obj(
            &mut objects,
            "nlt1",
            "NoLoadTest",
            &[
                ("EnergisedEnd", refval("tank1")),
                ("loss", text("85000")),
                ("excitingCurrent", text("0.35")),
            ],
        );
        // Short-circuit test
        insert_obj(
            &mut objects,
            "sct1",
            "ShortCircuitTest",
            &[
                ("EnergisedEnd", refval("tank1")),
                ("loss", text("450000")),
                ("leakageImpedance", text("12.5")),
            ],
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert_eq!(network.cim.asset_catalog.transformer_infos.len(), 1);
        let ti = &network.cim.asset_catalog.transformer_infos["tank1"];
        assert_eq!(ti.name, "230/115 kV 200 MVA");
        assert_eq!(ti.windings.len(), 2);

        // Winding 1
        assert_eq!(ti.windings[0].end_number, 1);
        assert!((ti.windings[0].rated_s_mva.unwrap() - 200.0).abs() < 1e-9);
        assert!((ti.windings[0].rated_u_kv.unwrap() - 230.0).abs() < 1e-9);
        assert!((ti.windings[0].r_ohm.unwrap() - 0.45).abs() < 1e-9);
        assert_eq!(ti.windings[0].connection_kind.as_deref(), Some("Y"));

        // Winding 2
        assert_eq!(ti.windings[1].end_number, 2);
        assert!((ti.windings[1].rated_u_kv.unwrap() - 115.0).abs() < 1e-9);
        assert_eq!(ti.windings[1].connection_kind.as_deref(), Some("D"));
        assert!((ti.windings[1].short_term_s_mva.unwrap() - 250.0).abs() < 1e-9);

        // Test data
        assert!((ti.no_load_loss_w.unwrap() - 85000.0).abs() < 1e-9);
        assert!((ti.exciting_current_pct.unwrap() - 0.35).abs() < 1e-9);
        assert!((ti.short_circuit_loss_w.unwrap() - 450000.0).abs() < 1e-9);
        assert!((ti.leakage_impedance_pct.unwrap() - 12.5).abs() < 1e-9);
    }

    #[test]
    fn test_parse_asset_metadata() {
        let mut objects: ObjMap = HashMap::new();
        // ProductAssetModel
        insert_obj(
            &mut objects,
            "pam1",
            "ProductAssetModel",
            &[
                ("manufacturerName", text("ABB")),
                ("modelNumber", text("TFX-500")),
            ],
        );
        // Asset referencing equipment and product model
        insert_obj(
            &mut objects,
            "asset1",
            "Asset",
            &[
                ("PowerSystemResources", refval("xfmr1")),
                ("ProductAssetModel", refval("pam1")),
                ("serialNumber", text("SN-2024-001")),
                ("manufacturedDate", text("2024-03-15")),
                ("installationDate", text("2024-06-01")),
            ],
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert_eq!(network.cim.asset_catalog.asset_metadata.len(), 1);
        let meta = &network.cim.asset_catalog.asset_metadata["xfmr1"];
        assert_eq!(meta.equipment_mrid, "xfmr1");
        assert_eq!(meta.serial_number.as_deref(), Some("SN-2024-001"));
        assert_eq!(meta.manufacturer.as_deref(), Some("ABB"));
        assert_eq!(meta.model_number.as_deref(), Some("TFX-500"));
        assert_eq!(
            meta.manufactured_date.unwrap().to_rfc3339(),
            "2024-03-15T00:00:00+00:00"
        );
        assert_eq!(
            meta.installation_date.unwrap().to_rfc3339(),
            "2024-06-01T00:00:00+00:00"
        );
        assert!(meta.retired_date.is_none());
    }

    #[test]
    fn test_empty_catalog_skipped() {
        let objects: ObjMap = HashMap::new();
        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_asset_catalog(&objects, &idx, &mut network);

        assert!(network.cim.asset_catalog.is_empty());
    }

    #[test]
    fn test_strip_cim_enum_prefix() {
        assert_eq!(strip_cim_enum_prefix("WireMaterialKind.acsr"), "acsr");
        assert_eq!(strip_cim_enum_prefix("SinglePhaseKind.A"), "A");
        assert_eq!(strip_cim_enum_prefix("aluminum"), "aluminum");
        assert_eq!(strip_cim_enum_prefix(""), "");
    }
}
