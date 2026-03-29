// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! OpenDSS (.dss) script writer.
//!
//! Converts a `surge_network::Network` into an OpenDSS-compatible script file.
//! The output script is structured as:
//!
//! 1. `Clear` — reset the DSS engine
//! 2. `New Circuit.*` — define the source (slack) bus
//! 3. `New Line.*` — transmission lines (branches with tap ~= 1.0 and shift ~= 0.0)
//! 4. `New Transformer.*` — branches with off-nominal tap or phase shift
//! 5. `New Load.*` — load elements
//! 6. `New Generator.*` — generator elements
//! 7. `New Capacitor.*` / `New Reactor.*` — bus shunt admittance
//! 8. `Set VoltageBases=[...]` + `CalcVoltageBases` + `Solve`
//!
//! ## Per-unit to physical unit conversion
//!
//! The Network model stores impedances in per-unit (on system base_mva) and
//! powers in MW/MVAr. OpenDSS expects:
//! - Line impedance in ohms/unit-length (we use `Length=1 Units=none` with total ohms)
//! - Load/generator power in kW/kvar
//! - Transformer impedance as %X on the transformer's own kVA base
//!
//! Conversion: `Z_ohms = Z_pu * base_kv^2 / base_mva`

use std::collections::BTreeSet;
use std::fmt::Write as FmtWrite;
use std::path::Path;

use surge_network::Network;
use surge_network::network::{BusType, TransformerConnection};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DssWriteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("format error: {0}")]
    Fmt(#[from] std::fmt::Error),
    #[error("network has no slack bus — cannot determine circuit source")]
    NoSlackBus,
}

/// Write a Network to an OpenDSS .dss file on disk.
pub fn write_dss(network: &Network, path: &Path) -> Result<(), DssWriteError> {
    let content = to_dss_string(network)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Serialize a Network to an OpenDSS script string.
pub fn to_dss_string(network: &Network) -> Result<String, DssWriteError> {
    let mut out = String::with_capacity(32 * 1024);

    // Find the slack bus to use as the circuit source.
    let slack_bus = network
        .buses
        .iter()
        .find(|b| b.bus_type == BusType::Slack)
        .or_else(|| network.buses.first())
        .ok_or(DssWriteError::NoSlackBus)?;

    let base_mva = network.base_mva;

    // Build a bus-number-to-name map. If a bus has a name, use it; otherwise
    // use the bus number as the name.
    let bus_name = |bus_num: u32| -> String {
        network
            .buses
            .iter()
            .find(|b| b.number == bus_num)
            .map(|b| {
                if b.name.is_empty() {
                    format!("bus{}", b.number)
                } else {
                    // Sanitize: DSS bus names cannot contain spaces (they're
                    // used as tokens in "Bus1=name" syntax). Replace spaces
                    // with underscores and trim.
                    b.name.trim().replace(' ', "_")
                }
            })
            .unwrap_or_else(|| format!("bus{}", bus_num))
    };

    let bus_base_kv = |bus_num: u32| -> f64 {
        network
            .buses
            .iter()
            .find(|b| b.number == bus_num)
            .map(|b| b.base_kv)
            .unwrap_or(1.0)
    };

    // ── Header ──────────────────────────────────────────────────────────────
    writeln!(
        out,
        "! OpenDSS script exported by Surge (https://github.com/amptimal/surge)"
    )?;
    writeln!(out, "! Network: {}", network.name)?;
    writeln!(out, "! Base MVA: {}", base_mva)?;
    writeln!(out)?;
    writeln!(out, "Clear")?;
    writeln!(out)?;

    // ── Circuit (source / slack bus) ────────────────────────────────────────
    let circuit_name = sanitize_dss_name(&network.name);
    let source_bus_name = bus_name(slack_bus.number);
    writeln!(
        out,
        "New Circuit.{} Bus1={} BasekV={:.4} pu={:.6} phases=3",
        circuit_name, source_bus_name, slack_bus.base_kv, slack_bus.voltage_magnitude_pu,
    )?;
    writeln!(out)?;

    // ── Lines (branches where tap ~= 1.0 and shift ~= 0.0) ────────────────
    let lines: Vec<_> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service && !br.is_transformer())
        .collect();

    if !lines.is_empty() {
        writeln!(
            out,
            "! ── Lines ────────────────────────────────────────────────────"
        )?;
    }
    for (i, br) in &lines {
        let from_name = bus_name(br.from_bus);
        let to_name = bus_name(br.to_bus);
        let from_kv = bus_base_kv(br.from_bus);

        // Convert per-unit impedance to ohms: Z_ohm = Z_pu * (kV^2 / base_mva)
        let z_base = from_kv * from_kv / base_mva;
        let r_ohm = br.r * z_base;
        let x_ohm = br.x * z_base;

        // Line charging susceptance: b_pu → nanofarads not needed; DSS accepts
        // B1 in per-unit-length (we use Length=1 Units=none, so B1 = total b/2).
        // Actually DSS Line element expects B1 as total positive-sequence
        // susceptance in per-unit-length of the line's own base. Since we set
        // Length=1 and Units=none, and R1/X1 are already total ohms, we express
        // B1 as total Mvar at 1 kV, i.e. susceptance in Siemens.
        // b_pu (system base) = B_siemens * z_base, so B_siemens = b_pu / z_base.
        let b_siemens = br.b / z_base;

        // Use C1 (nF) or B1 (µS)? DSS Line accepts B1 in µS/unit-length.
        // Since Length=1, B1 = total µS.
        let b_us = b_siemens * 1e6;

        let line_name = format!("line_{}_{}", br.from_bus, br.to_bus);
        // Use a unique suffix if there are parallel lines.
        let line_name = if lines
            .iter()
            .filter(|(_, b)| {
                (b.from_bus == br.from_bus && b.to_bus == br.to_bus)
                    || (b.from_bus == br.to_bus && b.to_bus == br.from_bus)
            })
            .count()
            > 1
        {
            format!("{}_{}", line_name, i)
        } else {
            line_name
        };

        write!(
            out,
            "New Line.{} Bus1={} Bus2={} R1={:.8} X1={:.8}",
            line_name, from_name, to_name, r_ohm, x_ohm,
        )?;
        if b_us.abs() > 1e-12 {
            write!(out, " B1={:.8}", b_us)?;
        }
        writeln!(out, " Length=1 Units=none")?;
    }
    if !lines.is_empty() {
        writeln!(out)?;
    }

    // ── Transformers (branches with off-nominal tap or phase shift) ─────────
    let xfmrs: Vec<_> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service && br.is_transformer())
        .collect();

    if !xfmrs.is_empty() {
        writeln!(
            out,
            "! ── Transformers ──────────────────────────────────────────────"
        )?;
    }
    for (i, br) in &xfmrs {
        let from_name = bus_name(br.from_bus);
        let to_name = bus_name(br.to_bus);
        let from_kv = bus_base_kv(br.from_bus);
        let to_kv = bus_base_kv(br.to_bus);

        // Transformer rating: use rate_a if available, otherwise use base_mva.
        let kva = if br.rating_a_mva > 0.0 {
            br.rating_a_mva * 1000.0 // rate_a is in MVA, convert to kVA
        } else {
            base_mva * 1000.0
        };

        // Convert per-unit impedance (system base) to percent on transformer base.
        // Z_pu_sys = Z_pu_xfmr * (S_base / S_xfmr)
        // Z_pu_xfmr = Z_pu_sys * (S_xfmr / S_base)
        let xfmr_mva = kva / 1000.0;
        let x_pct = br.x * (xfmr_mva / base_mva) * 100.0;
        let r_pct = br.r * (xfmr_mva / base_mva) * 100.0;

        let xfmr_name = format!("xfmr_{}_{}", br.from_bus, br.to_bus);
        let xfmr_name = if xfmrs
            .iter()
            .filter(|(_, b)| {
                (b.from_bus == br.from_bus && b.to_bus == br.to_bus)
                    || (b.from_bus == br.to_bus && b.to_bus == br.from_bus)
            })
            .count()
            > 1
        {
            format!("{}_{}", xfmr_name, i)
        } else {
            xfmr_name
        };

        // Determine winding connections from TransformerConnection.
        let xfmr_conn = br
            .transformer_data
            .as_ref()
            .map(|t| t.transformer_connection)
            .unwrap_or_default();
        let (conn1, conn2) = match xfmr_conn {
            TransformerConnection::DeltaWyeG => ("delta", "wye"),
            TransformerConnection::WyeGDelta => ("wye", "delta"),
            TransformerConnection::DeltaDelta => ("delta", "delta"),
            TransformerConnection::WyeGWye | TransformerConnection::WyeGWyeG => ("wye", "wye"),
        };

        writeln!(
            out,
            "New Transformer.{name} Windings=2 Buses=[{b1}, {b2}] \
             Conns=[{c1}, {c2}] kVs=[{kv1:.4}, {kv2:.4}] \
             kVAs=[{kva:.1}, {kva:.1}] \
             XHL={xhl:.6} %Rs=[{r1:.6}, {r2:.6}] \
             Taps=[{t1:.6}, 1.0]",
            name = xfmr_name,
            b1 = from_name,
            b2 = to_name,
            c1 = conn1,
            c2 = conn2,
            kv1 = from_kv,
            kv2 = to_kv,
            kva = kva,
            xhl = x_pct,
            r1 = r_pct / 2.0,
            r2 = r_pct / 2.0,
            t1 = br.tap,
        )?;
    }
    if !xfmrs.is_empty() {
        writeln!(out)?;
    }

    // ── Loads ────────────────────────────────────────────────────────────────
    // Prefer explicit Load objects (network.loads). If the loads vec is empty,
    // fall back: if no Load objects exist, nothing to write.
    let has_explicit_loads = !network.loads.is_empty();

    if has_explicit_loads {
        let active_loads: Vec<_> = network
            .loads
            .iter()
            .filter(|l| {
                l.in_service
                    && (l.active_power_demand_mw.abs() > 1e-9
                        || l.reactive_power_demand_mvar.abs() > 1e-9)
            })
            .collect();

        if !active_loads.is_empty() {
            writeln!(
                out,
                "! ── Loads ────────────────────────────────────────────────────"
            )?;
        }
        let mut load_counter: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        for load in &active_loads {
            let bn = bus_name(load.bus);
            let kv = bus_base_kv(load.bus);
            // kW = MW * 1000, kvar = MVAr * 1000
            let kw = load.active_power_demand_mw * 1000.0;
            let kvar = load.reactive_power_demand_mvar * 1000.0;

            let count = load_counter.entry(load.bus).or_insert(0);
            *count += 1;
            let load_name = if *count > 1 {
                format!("load_{}_{}", load.bus, count)
            } else {
                format!("load_{}", load.bus)
            };

            writeln!(
                out,
                "New Load.{} Bus1={} kW={:.4} kvar={:.4} kV={:.4} Model=1",
                load_name, bn, kw, kvar, kv,
            )?;
        }
        if !active_loads.is_empty() {
            writeln!(out)?;
        }
    } else {
        // No Load objects — nothing to write.
        if false {
            writeln!(out)?;
        }
    }

    // ── Generators ──────────────────────────────────────────────────────────
    let active_gens: Vec<_> = network.generators.iter().filter(|g| g.in_service).collect();

    // Skip the generator that corresponds to the slack bus circuit source —
    // OpenDSS models the slack bus as the Circuit element, not a separate
    // Generator. We emit Generator elements only for non-slack generators.
    // If there is only one generator on the slack bus, skip it.
    let slack_bus_num = slack_bus.number;
    let n_gens_on_slack = active_gens
        .iter()
        .filter(|g| g.bus == slack_bus_num)
        .count();

    let gens_to_emit: Vec<_> = active_gens
        .iter()
        .enumerate()
        .filter(|(idx, g)| {
            // Skip the first generator on the slack bus (it is the circuit source).
            if g.bus == slack_bus_num && n_gens_on_slack >= 1 {
                // Find the index of the first gen on the slack bus.
                let first_slack_gen_idx = active_gens
                    .iter()
                    .position(|gg| gg.bus == slack_bus_num)
                    .unwrap_or(usize::MAX);
                *idx != first_slack_gen_idx
            } else {
                true
            }
        })
        .map(|(_, g)| *g)
        .collect();

    if !gens_to_emit.is_empty() {
        writeln!(
            out,
            "! ── Generators ────────────────────────────────────────────────"
        )?;
    }
    let mut gen_counter: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for g in &gens_to_emit {
        let bn = bus_name(g.bus);
        let kv = bus_base_kv(g.bus);
        let kw = g.p * 1000.0;
        let kvar = g.q * 1000.0;

        let count = gen_counter.entry(g.bus).or_insert(0);
        *count += 1;
        let gen_name = if *count > 1 {
            format!("gen_{}_{}", g.bus, count)
        } else {
            format!("gen_{}", g.bus)
        };

        write!(
            out,
            "New Generator.{} Bus1={} kW={:.4} kvar={:.4} kV={:.4} Model=1",
            gen_name, bn, kw, kvar, kv,
        )?;

        // Clamp extreme pmax/pmin for DSS compatibility.
        let pmax = if g.pmax < 1e9 { g.pmax } else { g.p * 2.0 };
        let pmin = if g.pmin > -1e9 { g.pmin } else { 0.0 };
        if pmax.is_finite() && pmax > 0.0 {
            write!(out, " Maxkw={:.4}", pmax * 1000.0)?;
        }
        if pmin.is_finite() {
            write!(out, " Minkw={:.4}", pmin * 1000.0)?;
        }
        writeln!(out)?;
    }
    if !gens_to_emit.is_empty() {
        writeln!(out)?;
    }

    // ── Shunt capacitors and reactors (from bus.shunt_conductance_mw / bus.shunt_susceptance_mvar) ────────────────
    let shunt_buses: Vec<_> = network
        .buses
        .iter()
        .filter(|b| b.shunt_conductance_mw.abs() > 1e-9 || b.shunt_susceptance_mvar.abs() > 1e-9)
        .collect();

    if !shunt_buses.is_empty() {
        writeln!(
            out,
            "! ── Shunts ────────────────────────────────────────────────────"
        )?;
    }
    for bus in &shunt_buses {
        let bn = bus_name(bus.number);

        // bus.shunt_susceptance_mvar is shunt susceptance in MVAr injected at V=1.0 p.u. (on system base).
        // Positive bs = capacitive (inject reactive power) → Capacitor
        // Negative bs = inductive (absorb reactive power) → Reactor
        //
        // bus.shunt_conductance_mw is shunt conductance in MW demanded at V=1.0 p.u.
        // Positive gs = absorbs real power → Reactor with R component
        // (DSS Reactor can represent both R and X; for pure G shunt, we
        //  approximate with a Reactor at the bus.)

        if bus.shunt_susceptance_mvar > 1e-9 {
            // Capacitive shunt: bs (MVAr at 1.0 pu) → kvar
            let kvar = bus.shunt_susceptance_mvar * 1000.0; // bs is already in MVAr-equivalent
            writeln!(
                out,
                "New Capacitor.shunt_{} Bus1={} kvar={:.4} kV={:.4}",
                bus.number, bn, kvar, bus.base_kv,
            )?;
        } else if bus.shunt_susceptance_mvar < -1e-9 {
            // Inductive shunt: negative bs → reactor absorbing kvar
            let kvar = (-bus.shunt_susceptance_mvar) * 1000.0;
            writeln!(
                out,
                "New Reactor.shunt_{} Bus1={} kvar={:.4} kV={:.4}",
                bus.number, bn, kvar, bus.base_kv,
            )?;
        }

        if bus.shunt_conductance_mw.abs() > 1e-9 {
            // Conductance shunt: gs (MW demanded at 1.0 pu) → approximate as a
            // small constant-impedance load. DSS doesn't have a pure G shunt
            // element; a Load with Model=2 (constant-Z) is the closest match.
            let kw = bus.shunt_conductance_mw * 1000.0;
            writeln!(
                out,
                "New Load.gshunt_{} Bus1={} kW={:.4} kvar=0 kV={:.4} Model=2",
                bus.number, bn, kw, bus.base_kv,
            )?;
        }
    }
    if !shunt_buses.is_empty() {
        writeln!(out)?;
    }

    // ── Voltage bases and solve ─────────────────────────────────────────────
    let mut voltage_bases: BTreeSet<OrderedF64> = BTreeSet::new();
    for bus in &network.buses {
        if bus.base_kv > 0.0 {
            voltage_bases.insert(OrderedF64(bus.base_kv));
        }
    }

    if !voltage_bases.is_empty() {
        let vbases: Vec<String> = voltage_bases
            .iter()
            .map(|v| format!("{:.4}", v.0))
            .collect();
        writeln!(out, "Set VoltageBases=[{}]", vbases.join(", "))?;
        writeln!(out, "CalcVoltageBases")?;
    }

    writeln!(out, "Solve")?;

    Ok(out)
}

/// Sanitize a network name for use as a DSS Circuit name.
fn sanitize_dss_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "surge_network".to_string()
    } else if s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("case_{}", s)
    } else {
        s
    }
}

/// Wrapper around f64 that implements Ord for use in BTreeSet.
/// NaN values are treated as equal and sort last.
#[derive(Clone, Copy)]
struct OrderedF64(f64);

impl PartialEq for OrderedF64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrderedF64 {}

impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn simple_network() -> Network {
        let mut net = Network::new("test_case");
        net.base_mva = 100.0;

        let mut slack = Bus::new(1, BusType::Slack, 138.0);
        slack.voltage_magnitude_pu = 1.04;
        slack.voltage_angle_rad = 0.0;
        net.buses.push(slack);

        let pq = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(pq);

        net.generators.push(Generator::new(1, 71.6, 1.04));

        net.branches
            .push(Branch::new_line(1, 2, 0.01938, 0.05917, 0.0528));

        net.loads.push(Load::new(2, 21.7, 12.7));

        net
    }

    fn network_with_transformer() -> Network {
        let mut net = Network::new("xfmr_case");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        let bus2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(bus2);

        net.generators.push(Generator::new(1, 50.0, 1.0));

        // Transformer: tap=0.978, shift=0
        let mut br = Branch::new_line(1, 2, 0.0, 0.20912, 0.0);
        br.tap = 0.978;
        br.rating_a_mva = 100.0;
        net.branches.push(br);

        net.loads.push(Load::new(2, 40.0, 15.0));

        net
    }

    fn network_with_shunts() -> Network {
        let mut net = Network::new("shunt_case");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        let mut bus2 = Bus::new(2, BusType::PQ, 138.0);
        bus2.shunt_susceptance_mvar = 1.9; // 1.9 MVAr capacitive shunt
        net.buses.push(bus2);

        net.generators.push(Generator::new(1, 10.0, 1.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));

        net
    }

    #[test]
    fn test_write_produces_dss_structure() {
        let net = simple_network();
        let s = to_dss_string(&net).unwrap();
        assert!(s.contains("Clear"), "should contain Clear command");
        assert!(
            s.contains("New Circuit."),
            "should contain circuit definition"
        );
        assert!(s.contains("New Line."), "should contain line definition");
        assert!(s.contains("New Load."), "should contain load definition");
        assert!(
            s.contains("CalcVoltageBases"),
            "should contain CalcVoltageBases"
        );
        assert!(s.contains("Solve"), "should contain Solve command");
    }

    #[test]
    fn test_circuit_uses_slack_bus() {
        let net = simple_network();
        let s = to_dss_string(&net).unwrap();
        // Slack bus is bus 1, base_kv=138.0
        assert!(
            s.contains("BasekV=138.0"),
            "circuit should use slack bus kV"
        );
        assert!(s.contains("pu=1.04"), "circuit should use slack bus vm");
    }

    #[test]
    fn test_line_impedance_conversion() {
        let net = simple_network();
        let s = to_dss_string(&net).unwrap();
        // Z_base = 138^2 / 100 = 190.44
        // R_ohm = 0.01938 * 190.44 = 3.6907...
        // X_ohm = 0.05917 * 190.44 = 11.2727...
        assert!(s.contains("R1="), "should have R1 parameter");
        assert!(s.contains("X1="), "should have X1 parameter");
    }

    #[test]
    fn test_load_kw_kvar() {
        let net = simple_network();
        let s = to_dss_string(&net).unwrap();
        // Load: 21.7 MW = 21700 kW, 12.7 MVAr = 12700 kvar
        assert!(s.contains("kW=21700.0"), "load kW should be 21700");
        assert!(s.contains("kvar=12700.0"), "load kvar should be 12700");
    }

    #[test]
    fn test_transformer_written() {
        let net = network_with_transformer();
        let s = to_dss_string(&net).unwrap();
        assert!(
            s.contains("New Transformer."),
            "should contain transformer definition"
        );
        assert!(s.contains("Taps=[0.978"), "should include tap ratio");
        assert!(s.contains("XHL="), "should include XHL parameter");
    }

    #[test]
    fn test_capacitor_shunt() {
        let net = network_with_shunts();
        let s = to_dss_string(&net).unwrap();
        assert!(
            s.contains("New Capacitor.shunt_2"),
            "should create capacitor for positive bs"
        );
        assert!(s.contains("kvar=1900.0"), "capacitor kvar should be 1900");
    }

    #[test]
    fn test_voltage_bases_set() {
        let net = simple_network();
        let s = to_dss_string(&net).unwrap();
        assert!(
            s.contains("Set VoltageBases=[138.0"),
            "should set voltage bases"
        );
    }

    #[test]
    fn test_file_write() {
        let net = simple_network();
        let tmp = std::env::temp_dir().join("surge_dss_writer_test.dss");
        write_dss(&net, &tmp).unwrap();
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("New Circuit."));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_empty_name_uses_bus_number() {
        let mut net = Network::new("test");
        net.base_mva = 100.0;
        let mut b = Bus::new(42, BusType::Slack, 138.0);
        b.name = String::new(); // empty name
        net.buses.push(b);
        net.generators.push(Generator::new(42, 10.0, 1.0));
        let s = to_dss_string(&net).unwrap();
        assert!(
            s.contains("bus42"),
            "empty bus name should fall back to bus<number>"
        );
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_dss_name("my-case.v2"), "my_case_v2");
        assert_eq!(sanitize_dss_name("3bus"), "case_3bus");
        assert_eq!(sanitize_dss_name(""), "surge_network");
        assert_eq!(sanitize_dss_name("valid_name"), "valid_name");
    }
}
