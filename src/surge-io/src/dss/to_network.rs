// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Convert a resolved DSS catalog into a `surge_network::Network`.
//!
//! ## Mapping
//!
//! | DSS element         | Surge type        | Notes                                    |
//! |---------------------|-------------------|------------------------------------------|
//! | Circuit             | Bus (Slack)       | Source bus at bus number 1               |
//! | Line                | Branch            | Positive-sequence impedance, per-unit    |
//! | Transformer (2-wdg) | Branch            | Off-nominal tap ratio, %X as reactance   |
//! | Transformer (3-wdg) | 3 Branches        | T-equivalent (star model)                |
//! | Load                | Bus Pd/Qd         | Added to bus demand in MW/MVAr           |
//! | Load (separate obj) | Load              | Also stored in network.loads             |
//! | Generator           | Generator         | PV bus if voltage setpoint given         |
//! | PVSystem            | Generator         | pmin=0, pmax=kva                         |
//! | Storage             | Generator         | pmin=-kw_rated (discharge), pmax=kw      |
//! | Capacitor           | Bus shunt Bs      | Added to bus.shunt_susceptance_mvar (MVAr at V=1 pu)       |
//! | Reactor             | Bus shunt Gs/Bs   | Added to bus.shunt_conductance_mw / bs                     |

use std::collections::HashMap;
use std::path::Path;

use thiserror::Error;

use surge_network::Network;
use surge_network::network::{
    Branch, Bus, BusType, GenType, Generator, GeneratorTechnology, Load, TransformerConnection,
    TransformerData,
};

use super::command::{DssCommand, parse_commands};
use super::lexer::tokenize;
use super::objects::{DssCatalog, DssObject, LengthUnit, WdgConn};
use super::resolve::{build_bus_map, resolve_linecodes, resolve_xfmrcodes, strip_phases};

/// Errors that can occur during DSS parsing or network construction.
#[derive(Error, Debug)]
pub enum DssParseError {
    #[error("I/O error reading '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("missing circuit definition — DSS file must contain a 'New Circuit.*' command")]
    NoCircuit,

    #[error("bus '{0}' not found in bus map")]
    BusNotFound(String),

    #[error("unresolvable reference: {0}")]
    UnresolvedRef(String),
}

/// Parse a .dss file from disk and return a `Network`.
pub fn parse_dss(path: &Path) -> Result<Network, DssParseError> {
    let content = std::fs::read_to_string(path).map_err(|e| DssParseError::Io {
        path: path.to_string_lossy().to_string(),
        source: e,
    })?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    parse_dss_str_with_base(&content, Some(base_dir))
}

/// Parse a .dss script from a string and return a `Network`.
pub fn parse_dss_str(content: &str) -> Result<Network, DssParseError> {
    parse_dss_str_with_base(content, None)
}

/// Core parsing routine — processes DSS commands, resolves references,
/// and maps to a `surge_network::Network`.
fn parse_dss_str_with_base(
    content: &str,
    base_dir: Option<&Path>,
) -> Result<Network, DssParseError> {
    let mut catalog = DssCatalog::new();
    let mut last_obj_idx: Option<usize> = None;
    let mut last_was_circuit = false;
    let mut frequency_hz = 60.0_f64;

    process_dss_content(
        content,
        base_dir,
        0,
        &mut catalog,
        &mut last_obj_idx,
        &mut last_was_circuit,
        &mut frequency_hz,
    )?;

    // ── Cross-reference resolution ───────────────────────────────────────────
    resolve_linecodes(&mut catalog);
    resolve_xfmrcodes(&mut catalog);

    let bus_map = build_bus_map(&catalog);

    // ── Build Network ────────────────────────────────────────────────────────
    build_network(catalog, bus_map, frequency_hz)
}

fn process_dss_content(
    content: &str,
    base_dir: Option<&Path>,
    depth: usize,
    catalog: &mut DssCatalog,
    last_obj_idx: &mut Option<usize>,
    last_was_circuit: &mut bool,
    frequency_hz: &mut f64,
) -> Result<(), DssParseError> {
    if depth > 16 {
        return Err(DssParseError::UnresolvedRef(
            "redirect/compile nesting depth exceeded".to_string(),
        ));
    }

    let tokens = tokenize(content);
    let commands = parse_commands(&tokens);
    for cmd in &commands {
        process_command(
            cmd,
            catalog,
            last_obj_idx,
            last_was_circuit,
            frequency_hz,
            base_dir,
            depth,
        )?;
    }
    Ok(())
}

/// Process a single DSS command, updating the catalog and tracking state.
fn process_command(
    cmd: &DssCommand,
    catalog: &mut DssCatalog,
    last_obj_idx: &mut Option<usize>,
    last_was_circuit: &mut bool,
    frequency_hz: &mut f64,
    base_dir: Option<&Path>,
    depth: usize,
) -> Result<(), DssParseError> {
    match cmd {
        DssCommand::Clear => {
            *catalog = DssCatalog::new();
            *last_obj_idx = None;
        }

        DssCommand::New {
            obj_type,
            obj_name,
            properties,
        }
        | DssCommand::Edit {
            obj_type,
            obj_name,
            properties,
        } => {
            let is_circuit =
                obj_type.to_lowercase() == "circuit" || obj_type.to_lowercase() == "vsource";

            if is_circuit {
                // Circuit is special — it becomes the source bus.
                let mut circ = super::objects::CircuitData {
                    name: obj_name.clone(),
                    ..Default::default()
                };
                for (k, v) in properties {
                    circ.apply_property(k, v);
                }
                catalog.circuit = Some(circ);
                *last_obj_idx = None;
                *last_was_circuit = true;
            } else {
                match DssObject::new_for_type(obj_type) {
                    Some(mut obj) => {
                        *obj.name_mut() = obj_name.clone();
                        // Handle `like=<name>` by cloning the referenced object
                        // before applying explicit properties.
                        let like_name = properties
                            .iter()
                            .find(|(k, _)| k.to_lowercase() == "like")
                            .map(|(_, v)| v);
                        if let Some(base_name) = like_name
                            && let Some(base_obj) = catalog.find(obj_type, base_name).cloned()
                        {
                            obj = base_obj;
                            *obj.name_mut() = obj_name.clone();
                        }
                        for (k, v) in properties {
                            if k.to_lowercase() != "like" {
                                obj.apply_property(k, v);
                            }
                        }
                        // Handle LineGeometry `cond=N x=... h=... wire=...` sequencing.
                        if let DssObject::LineGeometry(ref mut geo) = obj {
                            apply_geometry_cond_properties(geo, properties);
                        }
                        let idx = catalog.upsert(obj_type, obj_name, obj);
                        *last_obj_idx = Some(idx);
                        *last_was_circuit = false;
                    }
                    None => {
                        tracing::debug!("DSS: unknown element type '{}' — skipping", obj_type);
                        *last_obj_idx = None;
                    }
                }
            }
        }

        DssCommand::More { properties } => {
            if *last_was_circuit {
                // Apply continuation properties to the circuit object.
                if let Some(ref mut circ) = catalog.circuit {
                    for (k, v) in properties {
                        circ.apply_property(k, v);
                    }
                }
            } else if last_obj_idx.is_some_and(|i| catalog.get_mut(i).is_some()) {
                let obj = catalog
                    .get_mut(last_obj_idx.expect("last_obj_idx is Some per is_some_and check"))
                    .expect("catalog.get_mut succeeds per is_some_and check");
                for (k, v) in properties {
                    obj.apply_property(k, v);
                }
                // Handle LineGeometry cond property sequencing in continuation.
                if let DssObject::LineGeometry(ref mut geo) = *obj {
                    apply_geometry_cond_properties(geo, properties);
                }
            }
        }

        DssCommand::Set { key, value } => {
            if key.to_lowercase() == "frequency" {
                *frequency_hz = value.parse::<f64>().unwrap_or(*frequency_hz);
            }
        }

        DssCommand::Redirect { path } | DssCommand::Compile { path } => {
            if let Some(base) = base_dir {
                let file_path = base.join(path);
                let content =
                    std::fs::read_to_string(&file_path).map_err(|source| DssParseError::Io {
                        path: file_path.to_string_lossy().to_string(),
                        source,
                    })?;
                let child_base = file_path.parent().unwrap_or(base);
                process_dss_content(
                    &content,
                    Some(child_base),
                    depth + 1,
                    catalog,
                    last_obj_idx,
                    last_was_circuit,
                    frequency_hz,
                )?;
            }
        }

        DssCommand::Solve | DssCommand::Unknown { .. } => {
            // Skip.
        }
    }

    Ok(())
}

/// Handle `cond=N x=val h=val wire=name` property groups in LineGeometry.
///
/// OpenDSS uses `cond` as a cursor to set per-conductor properties.
fn apply_geometry_cond_properties(
    geo: &mut super::objects::LineGeometryData,
    props: &[(String, String)],
) {
    let mut current_cond: Option<usize> = None;

    for (k, v) in props {
        match k.to_lowercase().as_str() {
            "cond" => {
                current_cond = v.trim().parse::<usize>().ok().map(|n| n - 1); // convert to 0-based
            }
            "x" => {
                if let Some(idx) = current_cond {
                    let x = v.trim().parse::<f64>().unwrap_or(0.0);
                    let factor = geo.units.to_km_factor() * 1000.0; // to metres
                    geo.set_cond_x(idx, x * factor);
                }
            }
            "h" => {
                if let Some(idx) = current_cond {
                    let h = v.trim().parse::<f64>().unwrap_or(0.0);
                    let factor = geo.units.to_km_factor() * 1000.0;
                    geo.set_cond_h(idx, h * factor);
                }
            }
            "wire" => {
                if let Some(idx) = current_cond {
                    geo.set_cond_wire(idx, v.trim());
                }
            }
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Build Network from resolved catalog
// ─────────────────────────────────────────────────────────────────────────────

fn build_network(
    catalog: DssCatalog,
    bus_map: HashMap<String, u32>,
    frequency_hz: f64,
) -> Result<Network, DssParseError> {
    let circ = catalog.circuit.as_ref().ok_or(DssParseError::NoCircuit)?;

    let mut net = Network::new(&circ.name);
    net.base_mva = 100.0;

    // ── Buses ────────────────────────────────────────────────────────────────
    // Create a Bus for every entry in the bus_map.
    // Use BFS zone propagation to assign correct base_kv to each bus.
    // This correctly handles multi-voltage networks (distribution feeders with
    // substation transformers) where junction buses have no directly attached
    // element that declares a kV.
    let zone_kv = build_zone_base_kv(&catalog, &bus_map, circ);

    let mut buses: Vec<Bus> = {
        let mut v: Vec<(u32, String)> = bus_map
            .iter()
            .map(|(name, &num)| (num, name.clone()))
            .collect();
        v.sort_by_key(|(num, _)| *num);
        v.into_iter()
            .map(|(num, name)| {
                let base_kv = zone_kv
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| infer_base_kv(&name, &catalog, circ));

                let mut bus = Bus::new(num, BusType::PQ, base_kv);
                bus.name = name;
                bus.voltage_max_pu = 1.10;
                bus.voltage_min_pu = 0.90;
                bus
            })
            .collect()
    };

    // Mark source bus as slack.
    let source_name = circ.bus.to_lowercase();
    for bus in &mut buses {
        if bus.name == source_name {
            bus.bus_type = BusType::Slack;
            bus.voltage_magnitude_pu = circ.pu;
            bus.voltage_angle_rad = 0.0;
            bus.base_kv = circ.base_kv;
            break;
        }
    }

    net.buses = buses;

    // ── Lines → Branches ─────────────────────────────────────────────────────
    let base_mva = net.base_mva;

    for obj in &catalog.objects {
        if let DssObject::Line(line) = obj {
            if line.bus1.is_empty() || line.bus2.is_empty() {
                continue;
            }
            let from_name = strip_phases(&line.bus1).to_lowercase();
            let to_name = strip_phases(&line.bus2).to_lowercase();

            let from_num = match bus_map.get(&from_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("Line.{}: bus '{}' not in bus map", line.name, from_name);
                    continue;
                }
            };
            let to_num = match bus_map.get(&to_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("Line.{}: bus '{}' not in bus map", line.name, to_name);
                    continue;
                }
            };

            // Get base kV of the from-bus for per-unit conversion.
            // Prefer the BFS zone map (correctly handles junction buses without
            // directly attached loads/generators).
            let base_kv = zone_kv
                .get(&from_name)
                .copied()
                .unwrap_or_else(|| infer_base_kv_for_line(line, &catalog, circ));
            let z_base = base_kv * base_kv / base_mva;

            // Convert length to km.
            let length_km = line.length * effective_units(line).to_km_factor();

            // Compute per-unit impedance.
            let (r_pu, x_pu, b_pu) = if !line.rmatrix.is_empty() {
                // Use positive-sequence extracted from matrix (diagonal average minus off-diagonal average).
                let (r1, x1, b1) = matrix_to_sequence(
                    &line.rmatrix,
                    &line.xmatrix,
                    &line.cmatrix,
                    line.phases as usize,
                    length_km,
                    z_base,
                    frequency_hz,
                );
                (r1, x1, b1)
            } else {
                // Use explicit sequence values.
                let r = line.r1 * length_km / z_base;
                let x = line.x1 * length_km / z_base;
                // Capacitance: c1 in µS/km → B = c1 * length_km * z_base * 2π f × 10⁻⁶
                let b = if line.c1 > 0.0 {
                    line.c1 * 1e-6 * length_km * z_base * 2.0 * std::f64::consts::PI * frequency_hz
                } else {
                    0.0
                };
                (r, x, b)
            };

            // Guard against degenerate impedance (open switch → very large Z).
            if line.is_switch {
                let mut br = Branch::new_line(from_num, to_num, 1e6, 1e6, 0.0);
                br.in_service = true;
                net.branches.push(br);
                continue;
            }

            let mut branch =
                Branch::new_line(from_num, to_num, r_pu.max(1e-9), x_pu.max(1e-9), b_pu);
            branch.in_service = true;
            branch.rating_a_mva = line.norm_amps * base_kv * 3.0_f64.sqrt() / 1000.0;
            net.branches.push(branch);
        }
    }

    // ── Transformers → Branches ──────────────────────────────────────────────
    for obj in &catalog.objects {
        if let DssObject::Transformer(xfmr) = obj {
            if xfmr.buses.len() < 2 {
                continue;
            }

            let primary_bus = strip_phases(&xfmr.buses[0]).to_lowercase();
            let secondary_bus = strip_phases(&xfmr.buses[1]).to_lowercase();

            let from_num = match bus_map.get(&primary_bus) {
                Some(&n) => n,
                None => {
                    tracing::warn!(
                        "Transformer.{}: primary bus '{}' not in map",
                        xfmr.name,
                        primary_bus
                    );
                    continue;
                }
            };
            let to_num = match bus_map.get(&secondary_bus) {
                Some(&n) => n,
                None => {
                    tracing::warn!(
                        "Transformer.{}: secondary bus '{}' not in map",
                        xfmr.name,
                        secondary_bus
                    );
                    continue;
                }
            };

            let _kv_primary = xfmr.kvs.first().copied().unwrap_or(115.0);
            let kv_secondary = xfmr.kvs.get(1).copied().unwrap_or(12.47);
            let kva = xfmr.kvas.first().copied().unwrap_or(1000.0);

            // Per-unit leakage reactance (from %X, based on transformer MVA base).
            let xfmr_mva = kva / 1000.0;
            let x_pu_xfmr = xfmr.xhl / 100.0; // %Xhl → per-unit on xfmr base

            // Total winding resistance: sum of %R from both windings / 2 (split equally).
            let r_pu_xfmr = xfmr.pct_rs.iter().sum::<f64>() / 100.0 / 2.0;

            // Convert from transformer MVA base to system base.
            let r_pu = r_pu_xfmr * base_mva / xfmr_mva;
            let x_pu = x_pu_xfmr * base_mva / xfmr_mva;

            // Off-nominal tap: primary tap / secondary tap (account for winding voltages).
            let tap1 = xfmr.taps.first().copied().unwrap_or(1.0);
            let tap2 = xfmr.taps.get(1).copied().unwrap_or(1.0);

            // Actual turns ratio vs nominal ratio (kV_primary / kV_secondary).
            // Off-nominal tap: t = (tap1/tap2) * (kV_secondary_base / kV_primary_base).
            // In DSS, base kV for the system is from the Circuit element.
            // We use the transformer's own kV ratings as the "rated" values.
            let tap = tap1 / tap2; // if different, there's an off-nominal ratio

            // Connection: delta on primary → phase shift of -30° (convention).
            let shift: f64 = match (&xfmr.conns.first(), &xfmr.conns.get(1)) {
                (Some(WdgConn::Delta), Some(WdgConn::Wye) | Some(WdgConn::Ln)) => -30.0,
                (Some(WdgConn::Wye) | Some(WdgConn::Ln), Some(WdgConn::Delta)) => 30.0,
                _ => 0.0,
            };

            let connection = match (&xfmr.conns.first(), &xfmr.conns.get(1)) {
                (Some(WdgConn::Delta), Some(WdgConn::Wye) | Some(WdgConn::Ln)) => {
                    TransformerConnection::DeltaWyeG
                }
                (Some(WdgConn::Wye) | Some(WdgConn::Ln), Some(WdgConn::Delta)) => {
                    TransformerConnection::WyeGDelta
                }
                (Some(WdgConn::Delta), Some(WdgConn::Delta)) => TransformerConnection::DeltaDelta,
                _ => TransformerConnection::WyeGWyeG,
            };

            let mut branch = Branch::new_line(
                from_num,
                to_num,
                if r_pu.abs() < 1e-6 { 1e-6 } else { r_pu },
                if x_pu.abs() < 1e-6 {
                    if x_pu < 0.0 { -1e-6 } else { 1e-6 }
                } else {
                    x_pu
                },
                0.0,
            );
            branch.tap = tap;
            branch.phase_shift_rad = shift.to_radians();
            branch
                .transformer_data
                .get_or_insert_with(TransformerData::default)
                .transformer_connection = connection;
            branch.in_service = true;
            net.branches.push(branch);

            // 3-winding: add tertiary branch.
            if xfmr.windings >= 3 && xfmr.buses.len() >= 3 {
                let tertiary_bus = strip_phases(&xfmr.buses[2]).to_lowercase();
                if let Some(&tert_num) = bus_map.get(&tertiary_bus) {
                    let _kv_tert = xfmr.kvs.get(2).copied().unwrap_or(kv_secondary);
                    let kva_tert = xfmr.kvas.get(2).copied().unwrap_or(kva);
                    let xfmr_mva_t = kva_tert / 1000.0;
                    let x_ht_pu = xfmr.xht / 100.0 * base_mva / xfmr_mva_t;
                    let r_tert =
                        xfmr.pct_rs.get(2).copied().unwrap_or(0.5) / 100.0 * base_mva / xfmr_mva_t;
                    let mut br_tert = Branch::new_line(
                        from_num,
                        tert_num,
                        r_tert.max(1e-6),
                        x_ht_pu.max(1e-6),
                        0.0,
                    );
                    br_tert.in_service = true;
                    net.branches.push(br_tert);
                }
            }
        }
    }

    // ── Loads ────────────────────────────────────────────────────────────────
    for obj in &catalog.objects {
        if let DssObject::Load(load) = obj {
            let bus_name = strip_phases(&load.bus1).to_lowercase();
            if bus_name.is_empty() {
                continue;
            }
            let bus_num = match bus_map.get(&bus_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("Load.{}: bus '{}' not found", load.name, bus_name);
                    continue;
                }
            };

            let pd_mw = load.kw / 1000.0;
            let qd_mvar = load.effective_kvar() / 1000.0;

            // Add as explicit Load object.
            net.loads.push(Load::new(bus_num, pd_mw, qd_mvar));
        }
    }

    // ── Generators ──────────────────────────────────────────────────────────
    for obj in &catalog.objects {
        if let DssObject::Generator(dss_gen) = obj {
            let bus_name = strip_phases(&dss_gen.bus1).to_lowercase();
            if bus_name.is_empty() {
                continue;
            }
            let bus_num = match bus_map.get(&bus_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("Generator.{}: bus '{}' not found", dss_gen.name, bus_name);
                    continue;
                }
            };

            let pg_mw = dss_gen.kw / 1000.0;
            let _base_kv_bus = dss_gen.kv;
            let vs = 1.0; // default voltage setpoint

            let mut g = Generator::new(bus_num, pg_mw, vs);
            g.q = dss_gen.kvar / 1000.0;
            g.pmax = dss_gen.kw_max / 1000.0;
            g.pmin = dss_gen.kw_min / 1000.0;
            g.qmax = dss_gen.kvar_max / 1000.0;
            g.qmin = dss_gen.kvar_min / 1000.0;
            g.machine_base_mva = dss_gen.kva / 1000.0;
            g.gen_type = GenType::Synchronous;
            g.technology = Some(GeneratorTechnology::Other);
            g.fuel.get_or_insert_with(Default::default).fuel_type =
                Some("dispatchable".to_string());

            // Mark as PV bus.
            if let Some(bus) = net.buses.iter_mut().find(|b| b.number == bus_num) {
                bus.bus_type = BusType::PV;
            }

            net.generators.push(g);
        }
    }

    // ── PV Systems ──────────────────────────────────────────────────────────
    for obj in &catalog.objects {
        if let DssObject::PvSystem(pv) = obj {
            let bus_name = strip_phases(&pv.bus1).to_lowercase();
            if bus_name.is_empty() {
                continue;
            }
            let bus_num = match bus_map.get(&bus_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("PVSystem.{}: bus '{}' not found", pv.name, bus_name);
                    continue;
                }
            };

            let pg_mw = pv.pmpp * pv.irradiance / 1000.0;

            let mut g = Generator::new(bus_num, pg_mw, 1.0);
            g.pmax = pv.kw_max / 1000.0;
            g.pmin = 0.0;
            g.qmax = pv.kva / 1000.0;
            g.qmin = -(pv.kva / 1000.0);
            g.machine_base_mva = pv.kva / 1000.0;
            g.gen_type = GenType::InverterBased;
            g.technology = Some(GeneratorTechnology::SolarPv);
            g.fuel.get_or_insert_with(Default::default).fuel_type = Some("solar".to_string());

            net.generators.push(g);
        }
    }

    // ── Storage ─────────────────────────────────────────────────────────────
    for obj in &catalog.objects {
        if let DssObject::Storage(stor) = obj {
            let bus_name = strip_phases(&stor.bus1).to_lowercase();
            if bus_name.is_empty() {
                continue;
            }
            let bus_num = match bus_map.get(&bus_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("Storage.{}: bus '{}' not found", stor.name, bus_name);
                    continue;
                }
            };

            let mut g = Generator::new(bus_num, 0.0, 1.0);
            g.pmax = stor.kw_rated / 1000.0;
            g.pmin = -(stor.kw_rated / 1000.0); // can charge (inject negative P)
            g.qmax = stor.kva / 1000.0;
            g.qmin = -(stor.kva / 1000.0);
            g.machine_base_mva = stor.kva / 1000.0;
            g.gen_type = GenType::InverterBased;
            g.technology = Some(GeneratorTechnology::BatteryStorage);
            g.fuel.get_or_insert_with(Default::default).fuel_type = Some("storage".to_string());

            net.generators.push(g);
        }
    }

    // ── Capacitors → Bus shunt ───────────────────────────────────────────────
    for obj in &catalog.objects {
        if let DssObject::Capacitor(cap) = obj {
            let bus_name = strip_phases(&cap.bus1).to_lowercase();
            if bus_name.is_empty() {
                continue;
            }
            let bus_num = match bus_map.get(&bus_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("Capacitor.{}: bus '{}' not found", cap.name, bus_name);
                    continue;
                }
            };

            let q_mvar = cap.total_kvar() / 1000.0;

            if let Some(bus) = net.buses.iter_mut().find(|b| b.number == bus_num) {
                bus.shunt_susceptance_mvar += q_mvar;
            }
        }
    }

    // ── Reactors → Bus shunt ─────────────────────────────────────────────────
    for obj in &catalog.objects {
        if let DssObject::Reactor(react) = obj {
            let bus_name = strip_phases(&react.bus1).to_lowercase();
            if bus_name.is_empty() {
                continue;
            }
            let bus_num = match bus_map.get(&bus_name) {
                Some(&n) => n,
                None => {
                    tracing::warn!("Reactor.{}: bus '{}' not found", react.name, bus_name);
                    continue;
                }
            };

            if react.kvar > 0.0 {
                let q_mvar = react.kvar / 1000.0;
                // Reactor absorbs Q → negative susceptance.
                if let Some(bus) = net.buses.iter_mut().find(|b| b.number == bus_num) {
                    bus.shunt_susceptance_mvar -= q_mvar;
                }
            }
        }
    }

    // Make sure there's at least one slack bus.
    #[allow(clippy::collapsible_if)]
    if net.buses.iter().all(|b| b.bus_type != BusType::Slack) {
        if let Some(b) = net.buses.first_mut() {
            b.bus_type = BusType::Slack;
        }
    }
    Ok(net)
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Determine the effective length unit for a line, falling back to km.
fn effective_units(line: &super::objects::LineData) -> LengthUnit {
    if line.units == LengthUnit::None {
        LengthUnit::Km
    } else {
        line.units
    }
}

/// Build a map from bus name → base kV using BFS zone propagation.
///
/// Distribution networks have multiple voltage zones separated by transformers.
/// A simple element-lookup cannot determine the base kV for junction buses
/// (no load/generator directly attached) that lie in a voltage zone different
/// from the source.  BFS through lines and transformers assigns the correct
/// zone voltage to every bus.
///
/// Algorithm:
/// 1. Build an adjacency list from all lines, series reactors, and transformers.
///    - Lines and series reactors: both endpoints share the same kV (ratio = 1.0).
///    - Transformers: endpoints have a kV ratio equal to their winding kV ratings.
/// 2. Seed the BFS queue with the circuit source bus at `circ.base_kv`.
/// 3. For each bus dequeued, propagate its kV to all unvisited neighbours via
///    the edge ratio.  Only newly-assigned buses are enqueued — each bus is
///    processed at most once, giving O(E) time instead of O(passes × E).
/// 4. For any bus not reached by BFS, fall back to element-lookup heuristic.
fn build_zone_base_kv(
    catalog: &DssCatalog,
    bus_map: &std::collections::HashMap<String, u32>,
    circ: &super::objects::CircuitData,
) -> std::collections::HashMap<String, f64> {
    // ── Step 1: build adjacency list ─────────────────────────────────────────
    // Each edge is (neighbour_bus_name, kv_ratio_to_apply).
    // For a line edge (a → b): kv_ratio = 1.0 (same zone).
    // For a transformer edge (winding_i → winding_j): kv_ratio = kv_j / kv_i
    //   (the zone kV scales by the winding voltage ratio).

    // adjacency: bus_name → Vec<(neighbour_name, ratio_neighbour_kv_from_self_kv)>
    let mut adj: std::collections::HashMap<String, Vec<(String, f64)>> =
        std::collections::HashMap::with_capacity(bus_map.len());

    let mut add_edge = |a: String, b: String, ratio_b_from_a: f64| {
        adj.entry(a.clone())
            .or_default()
            .push((b.clone(), ratio_b_from_a));
        adj.entry(b).or_default().push((a, 1.0 / ratio_b_from_a));
    };

    for obj in &catalog.objects {
        match obj {
            DssObject::Line(line) => {
                if line.bus1.is_empty() || line.bus2.is_empty() {
                    continue;
                }
                let b1 = strip_phases(&line.bus1).to_lowercase();
                let b2 = strip_phases(&line.bus2).to_lowercase();
                if b1.is_empty() || b2.is_empty() {
                    continue;
                }
                if !bus_map.contains_key(&b1) || !bus_map.contains_key(&b2) {
                    continue;
                }
                add_edge(b1, b2, 1.0);
            }
            DssObject::Reactor(react) => {
                if react.bus2.is_empty() {
                    continue; // shunt reactor — no propagation
                }
                let b1 = strip_phases(&react.bus1).to_lowercase();
                let b2 = strip_phases(&react.bus2).to_lowercase();
                if b1.is_empty() || b2.is_empty() {
                    continue;
                }
                if !bus_map.contains_key(&b1) || !bus_map.contains_key(&b2) {
                    continue;
                }
                add_edge(b1, b2, 1.0);
            }
            DssObject::Transformer(xfmr) => {
                if xfmr.buses.len() < 2 {
                    continue;
                }
                // Collect (bus_name, rated_kv) for each winding.
                let windings: Vec<(String, f64)> = xfmr
                    .buses
                    .iter()
                    .enumerate()
                    .filter_map(|(i, b)| {
                        let name = strip_phases(b).to_lowercase();
                        let kv = xfmr.kvs.get(i).copied().unwrap_or(0.0);
                        if name.is_empty() || kv <= 0.0 || !bus_map.contains_key(&name) {
                            None
                        } else {
                            Some((name, kv))
                        }
                    })
                    .collect();

                // Add edges between every pair of windings.
                for i in 0..windings.len() {
                    for j in (i + 1)..windings.len() {
                        let (ref bi, kvi) = windings[i];
                        let (ref bj, kvj) = windings[j];
                        // ratio: zone_kv_j = zone_kv_i * (kvj / kvi)
                        add_edge(bi.clone(), bj.clone(), kvj / kvi);
                    }
                }
            }
            _ => {}
        }
    }

    // ── Step 2: BFS from the source bus ──────────────────────────────────────
    let mut zone_kv: std::collections::HashMap<String, f64> =
        std::collections::HashMap::with_capacity(bus_map.len());

    let source = circ.bus.to_lowercase();
    zone_kv.insert(source.clone(), circ.base_kv);

    let mut queue = std::collections::VecDeque::new();
    queue.push_back(source);

    while let Some(bus) = queue.pop_front() {
        let kv_bus = match zone_kv.get(&bus).copied() {
            Some(k) => k,
            None => continue,
        };
        if let Some(neighbours) = adj.get(&bus) {
            for (nb, ratio) in neighbours {
                if !zone_kv.contains_key(nb.as_str()) {
                    let kv_nb = kv_bus * ratio;
                    zone_kv.insert(nb.clone(), kv_nb);
                    queue.push_back(nb.clone());
                }
            }
        }
    }

    // ── Step 3: fall back for any bus not reached by BFS ─────────────────────
    for bus_name in bus_map.keys() {
        if !zone_kv.contains_key(bus_name.as_str()) {
            let kv = infer_base_kv_heuristic(bus_name, catalog, circ);
            zone_kv.insert(bus_name.clone(), kv);
        }
    }

    zone_kv
}

/// Estimate base kV for a bus by looking at what elements directly connect to it.
/// Used as a fallback when BFS zone propagation cannot determine the voltage.
fn infer_base_kv_heuristic(
    bus_name: &str,
    catalog: &DssCatalog,
    circ: &super::objects::CircuitData,
) -> f64 {
    // Check transformers: winding buses tell us the kV for each side.
    for obj in &catalog.objects {
        if let DssObject::Transformer(xfmr) = obj {
            for (i, b) in xfmr.buses.iter().enumerate() {
                if strip_phases(&b.to_lowercase()) == bus_name
                    && xfmr.kvs.get(i).is_some_and(|&kv| kv > 0.0)
                {
                    return xfmr.kvs[i];
                }
            }
        }
    }

    // Check loads and generators.
    for obj in &catalog.objects {
        match obj {
            DssObject::Load(l) if strip_phases(&l.bus1.to_lowercase()) == bus_name => {
                if l.kv > 0.0 {
                    return l.kv;
                }
            }
            DssObject::Generator(g) if strip_phases(&g.bus1.to_lowercase()) == bus_name => {
                if g.kv > 0.0 {
                    return g.kv;
                }
            }
            DssObject::Capacitor(c) if strip_phases(&c.bus1.to_lowercase()) == bus_name => {
                if c.kv > 0.0 {
                    return c.kv;
                }
            }
            _ => {}
        }
    }

    circ.base_kv
}

/// Estimate base kV for a bus — uses the BFS zone map when available,
/// then falls back to heuristic lookup.
fn infer_base_kv(bus_name: &str, catalog: &DssCatalog, circ: &super::objects::CircuitData) -> f64 {
    // Check circuit source bus first (fast path).
    if bus_name == circ.bus.to_lowercase() {
        return circ.base_kv;
    }
    infer_base_kv_heuristic(bus_name, catalog, circ)
}

/// Estimate base kV for a line's from-bus.
fn infer_base_kv_for_line(
    line: &super::objects::LineData,
    catalog: &DssCatalog,
    circ: &super::objects::CircuitData,
) -> f64 {
    infer_base_kv(strip_phases(&line.bus1.to_lowercase()), catalog, circ)
}

/// Extract positive-sequence impedance from a 3×3 (or n×n) phase matrix.
///
/// Uses the standard Fortescue transformation: Z1 = (1/3)(Zaa + 2*Zab)
/// where Zaa is the self impedance and Zab is the mutual.
///
/// The matrix is stored row-major in the lower-triangular DSS format:
///   [Zaa, Zab, Zbb, Zac, Zbc, Zcc] for a 3-phase system.
fn matrix_to_sequence(
    rmat: &[f64],
    xmat: &[f64],
    cmat: &[f64],
    n: usize,
    length_km: f64,
    z_base: f64,
    freq: f64,
) -> (f64, f64, f64) {
    if n == 0 || rmat.is_empty() {
        return (1e-4, 1e-3, 0.0);
    }

    let n = n.min(3);

    // Helper: get element (i,j) from lower-triangular storage.
    let get = |mat: &[f64], i: usize, j: usize| -> f64 {
        let (row, col) = if i >= j { (i, j) } else { (j, i) };
        let idx = row * (row + 1) / 2 + col;
        mat.get(idx).copied().unwrap_or(0.0)
    };

    // Self-impedance average (diagonal).
    let r_self: f64 = (0..n).map(|i| get(rmat, i, i)).sum::<f64>() / n as f64;
    let x_self: f64 = (0..n).map(|i| get(xmat, i, i)).sum::<f64>() / n as f64;

    // Mutual impedance average (off-diagonal pairs).
    let n_pairs = if n > 1 { n * (n - 1) / 2 } else { 1 };
    let mut r_mut_sum = 0.0;
    let mut x_mut_sum = 0.0;
    for i in 0..n {
        for j in (i + 1)..n {
            r_mut_sum += get(rmat, i, j);
            x_mut_sum += get(xmat, i, j);
        }
    }
    let r_mut = r_mut_sum / n_pairs as f64;
    let x_mut = x_mut_sum / n_pairs as f64;

    // Positive-sequence: Z1 = Z_self - Z_mutual.
    let r1 = (r_self - r_mut) * length_km / z_base;
    let x1 = (x_self - x_mut) * length_km / z_base;

    // Shunt susceptance from capacitance matrix (µS/km → p.u.).
    let b_pu = if !cmat.is_empty() {
        let c_self: f64 = (0..n).map(|i| get(cmat, i, i)).sum::<f64>() / n as f64;
        // c_self is in nF/km → S/km = c_self × 1e-9 × 2π f
        let b_s_per_km = c_self * 1e-9 * 2.0 * std::f64::consts::PI * freq;
        b_s_per_km * length_km * z_base
    } else {
        0.0
    };

    (r1.max(1e-9), x1.max(1e-9), b_pu)
}

#[cfg(test)]
mod tests_zone_kv {
    use super::*;

    fn benchmark_path(rel: &str) -> std::path::PathBuf {
        let path = std::path::Path::new(rel);
        if path.exists() {
            return path.to_path_buf();
        }

        let mut base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        base.pop();
        base.pop();
        base.push(rel);
        base
    }

    /// Verify that BFS zone propagation correctly assigns 24.9 kV to IEEE-34
    /// distribution buses (not the 69 kV transmission source voltage).
    #[test]
    fn test_zone_kv_ieee34_file() {
        let path = benchmark_path("benchmarks/instances/dss/ieee34/ieee34Mod1.dss");
        if !path.exists() {
            return;
        }
        let net = parse_dss(&path).expect("parse ieee34");

        // Bus 800 is the primary distribution bus at 24.9 kV
        let bus_800 = net.buses.iter().find(|b| b.name == "800");
        assert!(bus_800.is_some(), "bus 800 not found");
        let kv = bus_800.unwrap().base_kv;
        assert!(
            (kv - 24.9).abs() < 1.0,
            "bus 800 should be ~24.9 kV, got {:.4} kV",
            kv
        );

        // Bus 802 propagated from 800 via line
        let bus_802 = net.buses.iter().find(|b| b.name == "802");
        if let Some(b) = bus_802 {
            assert!(
                (b.base_kv - 24.9).abs() < 1.0,
                "bus 802 should be ~24.9 kV, got {:.4} kV",
                b.base_kv
            );
        }
    }
}

#[cfg(test)]
mod tests_truthfulness {
    use super::*;

    #[test]
    fn test_nested_redirect_and_compile_follow_child_base_dir() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("sub");
        std::fs::create_dir_all(&subdir).unwrap();

        std::fs::write(
            dir.path().join("main.dss"),
            "New Circuit.main basekv=12.47 bus1=source\nRedirect sub/child.dss\n",
        )
        .unwrap();
        std::fs::write(subdir.join("child.dss"), "Compile grandchild.dss\n").unwrap();
        std::fs::write(
            subdir.join("grandchild.dss"),
            "New Line.L1 bus1=source.1 bus2=load.1 phases=1 r1=0.1 x1=0.2 length=1\n",
        )
        .unwrap();

        let net = parse_dss(&dir.path().join("main.dss")).expect("nested includes should resolve");
        assert_eq!(net.n_buses(), 2);
        assert_eq!(net.n_branches(), 1);
        assert!(net.branches[0].r > 0.0);
        assert!(net.branches[0].x > 0.0);
    }

    #[test]
    fn test_like_clones_base_object_properties() {
        let dss = r#"
New Circuit.main basekv=12.47 bus1=source
New Line.Base bus1=source.1 bus2=load.1 phases=1 r1=0.1 x1=0.2 length=1 normamps=300
New Line.Clone like=Base
"#;

        let net = parse_dss_str(dss).expect("like= clone should parse");
        assert_eq!(net.n_branches(), 2);
        assert!((net.branches[0].r - net.branches[1].r).abs() < 1e-12);
        assert!((net.branches[0].x - net.branches[1].x).abs() < 1e-12);
        assert!((net.branches[0].rating_a_mva - net.branches[1].rating_a_mva).abs() < 1e-12);
    }
}
