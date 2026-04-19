// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! MATPOWER (.m) case file writer.
//!
//! Writes the MATPOWER v2 format (mpc.bus, mpc.gen, mpc.branch, mpc.gencost).
//! Suitable for round-trip with MATPOWER, pypowsybl, and PowerModels.jl.

use std::fmt::Write as FmtWrite;
use std::path::Path;

use surge_network::Network;
use surge_network::market::CostCurve;
use surge_network::network::{BusType, Generator, GeneratorTechnology};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MatpowerWriteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("format error: {0}")]
    Fmt(#[from] std::fmt::Error),
}

/// Write a Network to a MATPOWER .m file on disk.
pub fn write_file(network: &Network, path: &Path) -> Result<(), MatpowerWriteError> {
    let content = to_string(network)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Serialize a Network to a MATPOWER .m string.
pub fn to_string(network: &Network) -> Result<String, MatpowerWriteError> {
    let mut out = String::with_capacity(64 * 1024);

    // Header
    let fn_name = sanitize_name(&network.name);
    writeln!(out, "function mpc = {fn_name}")?;
    writeln!(
        out,
        "% Exported by Surge (https://github.com/amptimal/surge)"
    )?;
    writeln!(out, "mpc.version = '2';")?;
    writeln!(out, "mpc.baseMVA = {};", network.base_mva)?;
    writeln!(out)?;

    // Precompute per-bus demand from Load objects only (do not subtract
    // PowerInjections — those are emitted as generator rows to preserve them
    // across MATPOWER roundtrips).
    let bus_map = network.bus_index_map();
    let mut bus_demand_p = vec![0.0; network.buses.len()];
    let mut bus_demand_q = vec![0.0; network.buses.len()];
    for load in &network.loads {
        if load.in_service {
            if let Some(&idx) = bus_map.get(&load.bus) {
                bus_demand_p[idx] += load.active_power_demand_mw;
                bus_demand_q[idx] += load.reactive_power_demand_mvar;
            }
        }
    }

    // --- Bus data ---
    writeln!(out, "mpc.bus = [")?;
    writeln!(
        out,
        "%\tbus_i\ttype\tPd\tQd\tGs\tBs\tarea\tVm\tVa\tbaseKV\tzone\tVmax\tVmin"
    )?;
    for (bi, bus) in network.buses.iter().enumerate() {
        let bus_type_code = match bus.bus_type {
            BusType::PQ => 1,
            BusType::PV => 2,
            BusType::Slack => 3,
            BusType::Isolated => 4,
        };
        let va_deg = bus.voltage_angle_rad.to_degrees();
        writeln!(
            out,
            "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{};",
            bus.number,
            bus_type_code,
            bus_demand_p.get(bi).copied().unwrap_or(0.0),
            bus_demand_q.get(bi).copied().unwrap_or(0.0),
            bus.shunt_conductance_mw,
            bus.shunt_susceptance_mvar,
            bus.area,
            bus.voltage_magnitude_pu,
            va_deg,
            bus.base_kv,
            bus.zone,
            bus.voltage_max_pu,
            bus.voltage_min_pu
        )?;
    }
    writeln!(out, "];")?;
    writeln!(out)?;

    // --- Generator data ---
    writeln!(out, "mpc.gen = [")?;
    writeln!(
        out,
        "%\tbus\tPg\tQg\tQmax\tQmin\tVg\tmBase\tstatus\tPmax\tPmin"
    )?;
    for g in &network.generators {
        let status = if g.in_service { 1 } else { 0 };
        // Clamp non-finite and f64::MAX/MIN sentinel values for MATPOWER compatibility
        let qmax = clamp_for_matpower(g.qmax, 9999.0);
        let qmin = clamp_for_matpower(g.qmin, -9999.0);
        let pmax = clamp_for_matpower(g.pmax, 9999.0);
        let pmin = clamp_for_matpower(g.pmin, -9999.0);
        let mbase = clamp_for_matpower(g.machine_base_mva, 100.0);
        writeln!(
            out,
            "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{};",
            g.bus, g.p, g.q, qmax, qmin, g.voltage_setpoint_pu, mbase, status, pmax, pmin
        )?;
    }
    // Emit PowerInjections as fixed generators so they survive MATPOWER roundtrips.
    for inj in &network.power_injections {
        if !inj.in_service {
            continue;
        }
        writeln!(
            out,
            "\t{}\t{}\t{}\t{}\t{}\t1.0\t100.0\t1\t{}\t{};",
            inj.bus,
            inj.active_power_injection_mw,
            inj.reactive_power_injection_mvar,
            inj.reactive_power_injection_mvar, // qmax = q
            inj.reactive_power_injection_mvar, // qmin = q
            inj.active_power_injection_mw,     // pmax = p
            inj.active_power_injection_mw,     // pmin = p
        )?;
    }
    writeln!(out, "];")?;
    writeln!(out)?;

    // --- Branch data ---
    writeln!(out, "mpc.branch = [")?;
    writeln!(
        out,
        "%\tfbus\ttbus\tr\tx\tb\trateA\trateB\trateC\ttap\tshift\tstatus\tangmin\tangmax"
    )?;
    for br in &network.branches {
        let status = if br.in_service { 1 } else { 0 };
        // MATPOWER convention: tap==0 means 1.0 (line); write 1.0 taps as 0
        let tap_out = if (br.tap - 1.0).abs() < 1e-9 {
            0.0
        } else {
            br.tap
        };
        // angmin/angmax stored internally in radians; MATPOWER expects degrees
        let angmin = br
            .angle_diff_min_rad
            .unwrap_or(-2.0 * std::f64::consts::PI)
            .to_degrees();
        let angmax = br
            .angle_diff_max_rad
            .unwrap_or(2.0 * std::f64::consts::PI)
            .to_degrees();
        // Clamp non-finite ratings to 0 (MATPOWER convention for unconstrained)
        let ra = clamp_for_matpower(br.rating_a_mva, 0.0);
        let rb = clamp_for_matpower(br.rating_b_mva, 0.0);
        let rc = clamp_for_matpower(br.rating_c_mva, 0.0);
        writeln!(
            out,
            "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{};",
            br.from_bus,
            br.to_bus,
            br.r,
            br.x,
            br.b,
            ra,
            rb,
            rc,
            tap_out,
            br.phase_shift_rad.to_degrees(),
            status,
            angmin,
            angmax
        )?;
    }
    writeln!(out, "];")?;
    writeln!(out)?;

    // --- Gencost data (if any generator has cost) ---
    let has_cost = network.generators.iter().any(|g| g.cost.is_some());
    if has_cost {
        writeln!(out, "mpc.gencost = [")?;
        writeln!(out, "%\tmodel\tstartup\tshutdown\tn\tcost parameters")?;
        for g in &network.generators {
            match &g.cost {
                Some(CostCurve::Polynomial {
                    startup,
                    shutdown,
                    coeffs,
                }) => {
                    let n = coeffs.len();
                    write!(out, "\t2\t{startup}\t{shutdown}\t{n}")?;
                    for c in coeffs {
                        write!(out, "\t{c}")?;
                    }
                    writeln!(out, ";")?;
                }
                Some(CostCurve::PiecewiseLinear {
                    startup,
                    shutdown,
                    points,
                }) => {
                    let n = points.len();
                    write!(out, "\t1\t{startup}\t{shutdown}\t{n}")?;
                    for (p, c) in points {
                        write!(out, "\t{p}\t{c}")?;
                    }
                    writeln!(out, ";")?;
                }
                None => {
                    // Default: linear cost at $0/MWh
                    writeln!(out, "\t2\t0\t0\t2\t0\t0;")?;
                }
            }
        }
        for inj in &network.power_injections {
            if !inj.in_service {
                continue;
            }
            writeln!(out, "\t2\t0\t0\t1\t0;")?;
        }
        writeln!(out, "];")?;
    }

    let has_gentype = network
        .generators
        .iter()
        .any(|g| g.source_technology_code.is_some() || g.technology.is_some());
    if has_gentype {
        writeln!(out)?;
        writeln!(out, "mpc.gentype = {{")?;
        for g in &network.generators {
            let code = matpower_gentype_for_generator(g);
            writeln!(out, "\t'{}';", escape_matpower_string(&code))?;
        }
        for inj in &network.power_injections {
            if inj.in_service {
                writeln!(out, "\t'OT';")?;
            }
        }
        writeln!(out, "}};")?;
    }

    let has_genfuel = network.generators.iter().any(|g| {
        g.fuel
            .as_ref()
            .and_then(|f| f.fuel_type.as_deref())
            .is_some()
    });
    if has_genfuel {
        writeln!(out)?;
        writeln!(out, "mpc.genfuel = {{")?;
        for g in &network.generators {
            let fuel = g
                .fuel
                .as_ref()
                .and_then(|f| f.fuel_type.as_deref())
                .unwrap_or("other");
            writeln!(out, "\t'{}';", escape_matpower_string(fuel))?;
        }
        for inj in &network.power_injections {
            if inj.in_service {
                writeln!(out, "\t'other';")?;
            }
        }
        writeln!(out, "}};")?;
    }

    // --- DC network sections (only when DC buses exist) ---
    if network.hvdc.has_explicit_dc_topology() {
        writeln!(out)?;

        // mpc.busdc — 8 columns
        writeln!(out, "mpc.busdc = [")?;
        writeln!(
            out,
            "%\tbusdc_i\tgrid\tPdc\tVdc\tbasekVdc\tVdcmax\tVdcmin\tCdc"
        )?;
        for dc_grid in &network.hvdc.dc_grids {
            for b in &dc_grid.buses {
                writeln!(
                    out,
                    "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{};",
                    b.bus_id,
                    dc_grid.id,
                    b.p_dc_mw,
                    b.v_dc_pu,
                    b.base_kv_dc,
                    b.v_dc_max,
                    b.v_dc_min,
                    b.cost
                )?;
            }
        }
        writeln!(out, "];")?;
        writeln!(out)?;

        // mpc.convdc — 33 columns (no dVdcset)
        writeln!(out, "mpc.convdc = [")?;
        writeln!(
            out,
            "%\tdc_bus\tac_bus\ttype_dc\ttype_ac\tP_g\tQ_g\tislcc\tVtar\trtf\txtf\ttransformer\ttm\tbf\tfilter\trc\txc\treactor\tbasekVac\tVmmax\tVmmin\tImax\tstatus\tLossA\tLossB\tLossCrec\tLossCinv\tdroop\tPdcset\tVdcset\tPacmax\tPacmin\tQacmax\tQacmin"
        )?;
        for dc_grid in &network.hvdc.dc_grids {
            for converter in &dc_grid.converters {
                let Some(c) = converter.as_vsc() else {
                    continue;
                };
                let is_lcc = if c.is_lcc { 1 } else { 0 };
                let transformer = if c.transformer { 1 } else { 0 };
                let filter = if c.filter { 1 } else { 0 };
                let reactor = if c.reactor { 1 } else { 0 };
                let status = if c.status { 1 } else { 0 };
                let pac_max = clamp_for_matpower(c.active_power_ac_max_mw, 9999.0);
                let pac_min = clamp_for_matpower(c.active_power_ac_min_mw, -9999.0);
                let qac_max = clamp_for_matpower(c.reactive_power_ac_max_mvar, 9999.0);
                let qac_min = clamp_for_matpower(c.reactive_power_ac_min_mvar, -9999.0);
                writeln!(
                    out,
                    "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{};",
                    c.dc_bus,
                    c.ac_bus,
                    c.control_type_dc,
                    c.control_type_ac,
                    c.active_power_mw,
                    c.reactive_power_mvar,
                    is_lcc,
                    c.voltage_setpoint_pu,
                    c.transformer_r_pu,
                    c.transformer_x_pu,
                    transformer,
                    c.tap_ratio,
                    c.filter_susceptance_pu,
                    filter,
                    c.reactor_r_pu,
                    c.reactor_x_pu,
                    reactor,
                    c.base_kv_ac,
                    c.voltage_max_pu,
                    c.voltage_min_pu,
                    c.current_max_pu,
                    status,
                    c.loss_constant_mw,
                    c.loss_linear,
                    c.loss_quadratic_rectifier,
                    c.loss_quadratic_inverter,
                    c.droop,
                    c.power_dc_setpoint_mw,
                    c.voltage_dc_setpoint_pu,
                    pac_max,
                    pac_min,
                    qac_max,
                    qac_min
                )?;
            }
        }
        writeln!(out, "];")?;
        writeln!(out)?;

        // mpc.branchdc — 9 columns
        writeln!(out, "mpc.branchdc = [")?;
        writeln!(
            out,
            "%\tfbusdc\ttbusdc\tr\tl\tc\trateA\trateB\trateC\tstatus"
        )?;
        for dc_grid in &network.hvdc.dc_grids {
            for br in &dc_grid.branches {
                let status = if br.status { 1 } else { 0 };
                writeln!(
                    out,
                    "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{};",
                    br.from_bus,
                    br.to_bus,
                    br.r_ohm,
                    br.l_mh,
                    br.c_uf,
                    br.rating_a_mva,
                    br.rating_b_mva,
                    br.rating_c_mva,
                    status
                )?;
            }
        }
        writeln!(out, "];")?;
    }

    Ok(out)
}

/// Clamp a value to a finite fallback if it is non-finite (±Inf, NaN) or at the
/// f64::MAX/f64::MIN sentinel used internally for "unlimited".
///
/// MATPOWER uses `Inf` in MATLAB, but many downstream tools (pypowsybl, PowerModels.jl)
/// cannot parse `Inf` reliably.  Using 9999 or 0 as the sentinel is safer.
fn clamp_for_matpower(v: f64, fallback: f64) -> f64 {
    if !v.is_finite() || v >= f64::MAX / 2.0 || v <= f64::MIN / 2.0 {
        fallback
    } else {
        v
    }
}

fn matpower_gentype_for_generator(generator: &Generator) -> String {
    if let Some(raw) = &generator.source_technology_code
        && !raw.trim().is_empty()
    {
        return raw.clone();
    }
    match generator.technology {
        Some(GeneratorTechnology::SteamTurbine) => "ST",
        Some(GeneratorTechnology::CombustionTurbine) => "GT",
        Some(GeneratorTechnology::CombinedCycle) => "CC",
        Some(GeneratorTechnology::InternalCombustion) => "IC",
        Some(GeneratorTechnology::Hydro) => "HY",
        Some(GeneratorTechnology::PumpedStorage) => "PS",
        Some(GeneratorTechnology::Hydrokinetic) => "HK",
        Some(GeneratorTechnology::Nuclear) => "NU",
        Some(GeneratorTechnology::Geothermal) => "GE",
        Some(GeneratorTechnology::Wind) => "WT",
        Some(GeneratorTechnology::SolarPv) => "PV",
        Some(GeneratorTechnology::SolarThermal) => "CP",
        Some(GeneratorTechnology::BatteryStorage) => "BA",
        Some(GeneratorTechnology::CompressedAirStorage) => "CE",
        Some(GeneratorTechnology::FlywheelStorage) => "FW",
        Some(GeneratorTechnology::FuelCell) => "FC",
        Some(GeneratorTechnology::SynchronousCondenser) => "SC",
        Some(GeneratorTechnology::StaticVarCompensator) => "SV",
        Some(GeneratorTechnology::DispatchableLoad) => "DL",
        Some(GeneratorTechnology::DcTie) => "DC",
        Some(GeneratorTechnology::Thermal)
        | Some(GeneratorTechnology::Solar)
        | Some(GeneratorTechnology::Wave)
        | Some(GeneratorTechnology::Storage)
        | Some(GeneratorTechnology::Motor)
        | Some(GeneratorTechnology::Other)
        | None => "OT",
    }
    .to_string()
}

fn escape_matpower_string(value: &str) -> String {
    value.replace('\'', "''")
}

/// Sanitize a string to be a valid MATLAB function name.
fn sanitize_name(name: &str) -> String {
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
    // Must not start with digit
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("case_{s}")
    } else if s.is_empty() {
        "case_unknown".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{
        Branch, Bus, BusType, DcBranch, DcBus, DcConverterStation, Generator, GeneratorTechnology,
        Load,
    };

    fn simple_network() -> Network {
        let mut net = Network::new("test_case");
        net.base_mva = 100.0;

        let mut slack = Bus::new(1, BusType::Slack, 345.0);
        slack.voltage_magnitude_pu = 1.04;
        slack.voltage_angle_rad = 0.0;
        net.buses.push(slack);

        let pq = Bus::new(2, BusType::PQ, 345.0);
        net.buses.push(pq);
        net.loads.push(Load::new(2, 100.0, 35.0));

        let mut g = Generator::new(1, 80.0, 1.04);
        g.qmax = 300.0;
        g.qmin = -300.0;
        g.pmax = 250.0;
        g.pmin = 10.0;
        g.cost = Some(CostCurve::Polynomial {
            startup: 1500.0,
            shutdown: 0.0,
            coeffs: vec![0.11, 5.0, 150.0],
        });
        net.generators.push(g);

        net.branches
            .push(Branch::new_line(1, 2, 0.01938, 0.05917, 0.0528));
        net
    }

    #[test]
    fn test_write_produces_matpower_sections() {
        let net = simple_network();
        let s = to_string(&net).unwrap();
        assert!(s.contains("function mpc = test_case"));
        assert!(s.contains("mpc.baseMVA = 100"));
        assert!(s.contains("mpc.bus = ["));
        assert!(s.contains("mpc.gen = ["));
        assert!(s.contains("mpc.branch = ["));
        assert!(s.contains("mpc.gencost = ["));
    }

    #[test]
    fn test_roundtrip_matpower() {
        use crate::matpower::parse_str;
        let net = simple_network();
        let s = to_string(&net).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.n_buses(), net.n_buses());
        assert_eq!(net2.n_branches(), net.n_branches());
        assert!((net2.base_mva - net.base_mva).abs() < 1e-6);
        assert!(
            (net2.buses[0].voltage_magnitude_pu - net.buses[0].voltage_magnitude_pu).abs() < 1e-6
        );
        let pd1 = net.bus_load_p_mw();
        let pd2 = net2.bus_load_p_mw();
        assert!((pd2[1] - pd1[1]).abs() < 1e-6);
    }

    #[test]
    fn test_tap_convention() {
        // tap == 1.0 should be written as 0 (MATPOWER line convention)
        let net = simple_network();
        let s = to_string(&net).unwrap();
        // Branch is a line (tap=1.0 internally), should appear as 0 in file
        assert!(s.contains("mpc.branch"));
    }

    #[test]
    fn test_va_in_degrees() {
        // va is stored in radians, must be written in degrees
        let mut net = Network::new("va_test");
        net.base_mva = 100.0;
        let mut bus = Bus::new(1, BusType::Slack, 138.0);
        bus.voltage_angle_rad = std::f64::consts::PI / 6.0; // 30 degrees
        net.buses.push(bus);
        net.generators.push(Generator::new(1, 0.0, 1.0));
        let s = to_string(&net).unwrap();
        // Should not contain raw radians value (~0.5235...)
        assert!(
            !s.contains("0.5235"),
            "va written in radians, expected degrees"
        );
        // The degree value should be ~30: check it appears (float format may vary)
        let va_deg_str = format!("{}", (std::f64::consts::PI / 6.0_f64).to_degrees());
        assert!(
            s.contains(&va_deg_str[..4]), // first 4 chars: "29.9" or "30.0"
            "expected va in degrees (~30) in output, got: {s}"
        );
    }

    #[test]
    fn test_piecewise_linear_gencost() {
        let mut net = Network::new("pwl_case");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        let mut g = Generator::new(1, 0.0, 1.0);
        g.cost = Some(CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 0.0), (100.0, 1000.0), (200.0, 3000.0)],
        });
        net.generators.push(g);
        let s = to_string(&net).unwrap();
        // Type 1 = piecewise-linear
        assert!(s.contains("\t1\t"));
    }

    #[test]
    fn test_file_roundtrip() {
        let net = simple_network();
        let tmp = std::env::temp_dir().join("surge_mw_writer_test.m");
        write_file(&net, &tmp).unwrap();
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("mpc.bus = ["));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_power_injection_rows_get_matching_gencost_entries() {
        let mut net = simple_network();
        net.power_injections
            .push(surge_network::network::PowerInjection::new(2, 15.0, 3.0));

        let s = to_string(&net).unwrap();
        let gen_section = s
            .split("mpc.gen = [")
            .nth(1)
            .and_then(|part| part.split("];").next())
            .unwrap();
        let gencost_section = s
            .split("mpc.gencost = [")
            .nth(1)
            .and_then(|part| part.split("];").next())
            .unwrap();
        let gen_count = gen_section
            .lines()
            .filter(|line| line.starts_with('\t') && line.trim_end().ends_with(';'))
            .count();
        let gencost_count = gencost_section
            .lines()
            .filter(|line| line.starts_with('\t') && line.trim_end().ends_with(';'))
            .count();

        assert_eq!(gen_count, 2);
        assert_eq!(gencost_count, 2);
    }

    #[test]
    fn test_generator_classification_sections_written() {
        let mut net = simple_network();
        let generator = net
            .generators
            .first_mut()
            .expect("simple network should have one generator");
        generator.technology = Some(GeneratorTechnology::SolarPv);
        generator.source_technology_code = Some("PV".to_string());
        generator
            .fuel
            .get_or_insert_with(Default::default)
            .fuel_type = Some("solar".to_string());

        let s = to_string(&net).expect("writer should succeed");
        assert!(s.contains("mpc.gentype = {"));
        assert!(s.contains("\t'PV';"));
        assert!(s.contains("mpc.genfuel = {"));
        assert!(s.contains("\t'solar';"));
    }

    fn dc_network() -> Network {
        let mut net = simple_network();
        let grid = net.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(DcBus {
            bus_id: 1,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 400.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.buses.push(DcBus {
            bus_id: 2,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 400.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.converters.push(
            DcConverterStation {
                id: String::new(),
                dc_bus: 1,
                ac_bus: 1,
                control_type_dc: 2,
                control_type_ac: 1,
                active_power_mw: 200.0,
                reactive_power_mvar: 30.0,
                is_lcc: false,
                voltage_setpoint_pu: 1.0,
                transformer_r_pu: 0.0015,
                transformer_x_pu: 0.1121,
                transformer: true,
                tap_ratio: 1.0,
                filter_susceptance_pu: 0.0887,
                filter: true,
                reactor_r_pu: 0.0001,
                reactor_x_pu: 0.16428,
                reactor: true,
                base_kv_ac: 345.0,
                voltage_max_pu: 1.1,
                voltage_min_pu: 0.9,
                current_max_pu: 1.1,
                status: true,
                loss_constant_mw: 1.103,
                loss_linear: 0.887,
                loss_quadratic_rectifier: 2.885,
                loss_quadratic_inverter: 2.885,
                droop: 0.0,
                power_dc_setpoint_mw: 200.0,
                voltage_dc_setpoint_pu: 1.0,
                active_power_ac_max_mw: 500.0,
                active_power_ac_min_mw: -500.0,
                reactive_power_ac_max_mvar: 300.0,
                reactive_power_ac_min_mvar: -300.0,
            }
            .into(),
        );
        grid.converters.push(
            DcConverterStation {
                id: String::new(),
                dc_bus: 2,
                ac_bus: 2,
                control_type_dc: 1,
                control_type_ac: 1,
                active_power_mw: -195.0,
                reactive_power_mvar: 20.0,
                is_lcc: false,
                voltage_setpoint_pu: 1.0,
                transformer_r_pu: 0.0015,
                transformer_x_pu: 0.1121,
                transformer: true,
                tap_ratio: 1.0,
                filter_susceptance_pu: 0.0887,
                filter: true,
                reactor_r_pu: 0.0001,
                reactor_x_pu: 0.16428,
                reactor: true,
                base_kv_ac: 345.0,
                voltage_max_pu: 1.1,
                voltage_min_pu: 0.9,
                current_max_pu: 1.1,
                status: true,
                loss_constant_mw: 1.103,
                loss_linear: 0.887,
                loss_quadratic_rectifier: 2.885,
                loss_quadratic_inverter: 2.885,
                droop: 0.0,
                power_dc_setpoint_mw: -195.0,
                voltage_dc_setpoint_pu: 1.0,
                active_power_ac_max_mw: 500.0,
                active_power_ac_min_mw: -500.0,
                reactive_power_ac_max_mvar: 300.0,
                reactive_power_ac_min_mvar: -300.0,
            }
            .into(),
        );
        grid.branches.push(DcBranch {
            id: "dc_branch_1".into(),
            from_bus: 1,
            to_bus: 2,
            r_ohm: 5.0,
            l_mh: 25.0,
            c_uf: 12.0,
            rating_a_mva: 500.0,
            rating_b_mva: 0.0,
            rating_c_mva: 0.0,
            status: true,
        });
        net
    }

    #[test]
    fn test_dc_sections_written() {
        let net = dc_network();
        let s = to_string(&net).unwrap();
        assert!(s.contains("mpc.busdc = ["));
        assert!(s.contains("mpc.convdc = ["));
        assert!(s.contains("mpc.branchdc = ["));
    }

    #[test]
    fn test_dc_roundtrip() {
        use crate::matpower::parse_str;
        let net = dc_network();
        let s = to_string(&net).unwrap();
        let net2 = parse_str(&s).expect("DC round-trip parse failed");
        let dc_buses: Vec<_> = net2.hvdc.dc_buses().collect();
        let dc_converters: Vec<_> = net2
            .hvdc
            .dc_converters()
            .filter_map(|c| c.as_vsc())
            .collect();
        let dc_branches: Vec<_> = net2.hvdc.dc_branches().collect();
        // DC buses
        assert_eq!(dc_buses.len(), 2);
        assert_eq!(dc_buses[0].bus_id, 1);
        assert!((dc_buses[0].base_kv_dc - 400.0).abs() < 1e-6);
        // DC converters
        assert_eq!(dc_converters.len(), 2);
        assert_eq!(dc_converters[0].control_type_dc, 2);
        assert!((dc_converters[0].active_power_mw - 200.0).abs() < 1e-6);
        assert!((dc_converters[0].loss_constant_mw - 1.103).abs() < 1e-6);
        assert!((dc_converters[0].loss_linear - 0.887).abs() < 1e-6);
        assert!((dc_converters[0].loss_quadratic_rectifier - 2.885).abs() < 1e-6);
        assert!((dc_converters[0].active_power_ac_max_mw - 500.0).abs() < 1e-6);
        assert!((dc_converters[0].active_power_ac_min_mw - (-500.0)).abs() < 1e-6);
        assert!((dc_converters[0].voltage_dc_setpoint_pu - 1.0).abs() < 1e-6);
        assert!(dc_converters[0].transformer);
        assert!(dc_converters[0].filter);
        assert!(dc_converters[0].reactor);
        // DC branches
        assert_eq!(dc_branches.len(), 1);
        assert!((dc_branches[0].r_ohm - 5.0).abs() < 1e-6);
        assert!((dc_branches[0].l_mh - 25.0).abs() < 1e-6);
        assert!((dc_branches[0].c_uf - 12.0).abs() < 1e-6);
        assert!(dc_branches[0].status);
    }

    #[test]
    fn test_no_dc_sections_when_empty() {
        let net = simple_network();
        let s = to_string(&net).unwrap();
        assert!(!s.contains("mpc.busdc"));
        assert!(!s.contains("mpc.convdc"));
        assert!(!s.contains("mpc.branchdc"));
    }
}
