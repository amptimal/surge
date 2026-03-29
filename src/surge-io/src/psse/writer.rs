// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E RAW writer — v33 (default) and v35+.
//!
//! When `version >= 35`, the writer appends additional sections:
//!
//! * **GNE Device Data** — empty section (Surge has no GNE model)
//! * **Induction Machine Data** — empty section (Surge stores these separately in
//!   `Network::induction_machines` when parsed; see Issue #28)
//! * **Substation Data** — written from `Network::topology` when present

use std::fmt::Write as FmtWrite;
use std::path::Path;

use surge_network::Network;
use surge_network::network::{BusType, SwitchedShunt};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PsseWriteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("format error: {0}")]
    Fmt(#[from] std::fmt::Error),
}

/// Write a Network to a PSS/E RAW file on disk (v33 by default).
pub fn write_file(network: &Network, path: &Path, version: u32) -> Result<(), PsseWriteError> {
    let content = to_string(network, version)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Serialize a Network to a PSS/E RAW string.
pub fn to_string(network: &Network, version: u32) -> Result<String, PsseWriteError> {
    let mut out = String::with_capacity(64 * 1024);
    let ver = if version == 0 { 33 } else { version };

    // PSS/E RAW header (3 lines)
    writeln!(
        out,
        " 0, {}, {ver}, 0, 0, 60.0   /  PSS/E {ver} Raw Data -- Exported by Surge",
        network.base_mva
    )?;
    writeln!(out, " {}", sanitize_psse_name(&network.name))?;
    writeln!(
        out,
        " Exported by Surge (https://github.com/amptimal/surge)"
    )?;

    // --- Bus Data ---

    for bus in &network.buses {
        let bus_type_code = match bus.bus_type {
            BusType::PQ => 1,
            BusType::PV => 2,
            BusType::Slack => 3,
            BusType::Isolated => 4,
        };
        let va_deg = bus.voltage_angle_rad.to_degrees();
        let name = format_bus_name(&bus.name, bus.number);
        writeln!(
            out,
            " {},{:12},{},{},{},{},{},{:.6},{:.4},1",
            bus.number,
            format!("'{name}'"),
            bus.base_kv,
            bus_type_code,
            1,
            bus.area,
            bus.zone,
            bus.voltage_magnitude_pu,
            va_deg
        )?;
    }
    writeln!(out, " 0 / END OF BUS DATA, BEGIN LOAD DATA")?;

    // --- Load Data ---
    // PSS/E LOAD format: I, ID, STATUS, AREA, ZONE, PL, QL, IP, IQ, YP, YQ, OWNER, SCALE

    if !network.loads.is_empty() {
        let bus_lookup: std::collections::HashMap<u32, &surge_network::network::Bus> =
            network.buses.iter().map(|b| (b.number, b)).collect();
        for load in &network.loads {
            let status: i32 = if load.in_service { 1 } else { 0 };
            let p = load.active_power_demand_mw;
            let q = load.reactive_power_demand_mvar;
            let pl = load.zip_p_power_frac * p;
            let ip = load.zip_p_current_frac * p;
            let yp = load.zip_p_impedance_frac * p;
            let ql = load.zip_q_power_frac * q;
            let iq = load.zip_q_current_frac * q;
            let yq = load.zip_q_impedance_frac * q;
            let (area, zone) = bus_lookup
                .get(&load.bus)
                .map(|b| (b.area, b.zone))
                .unwrap_or((1, 1));
            let scale: i32 = if load.conforming { 1 } else { 0 };
            let id = if load.id.is_empty() { "1 " } else { &load.id };
            let owner = load.owners.first().map(|entry| entry.owner).unwrap_or(1);
            writeln!(
                out,
                " {},'{id}',{status},{area},{zone},{pl:.4},{ql:.4},{ip:.4},{iq:.4},{yp:.4},{yq:.4},{},{scale}",
                load.bus, owner
            )?;
        }
    } else {
        // No explicit Load objects — demand lives exclusively on Load objects now,
        // so nothing to write in this fallback path.
    }
    writeln!(out, " 0 / END OF LOAD DATA, BEGIN FIXED SHUNT DATA")?;

    // --- Fixed Shunt Data --- (for buses with gs or bs)

    for bus in &network.buses {
        if bus.shunt_conductance_mw.abs() > 1e-10 || bus.shunt_susceptance_mvar.abs() > 1e-10 {
            writeln!(
                out,
                " {},'1 ',1,{:.4},{:.4}",
                bus.number, bus.shunt_conductance_mw, bus.shunt_susceptance_mvar
            )?;
        }
    }
    if version >= 36 {
        writeln!(
            out,
            " 0 / END OF FIXED SHUNT DATA, BEGIN VOLTAGE DROOP CONTROL DATA"
        )?;
        for ctrl in &network.metadata.voltage_droop_controls {
            writeln!(
                out,
                " {},'{}',{},{},{:.6},{:.6},{:.6}",
                ctrl.bus,
                ctrl.device_id,
                ctrl.device_type,
                ctrl.regulated_bus,
                ctrl.vdrp,
                ctrl.vmax,
                ctrl.vmin
            )?;
        }
        writeln!(
            out,
            " 0 / END OF VOLTAGE DROOP CONTROL DATA, BEGIN GENERATOR DATA"
        )?;
    } else {
        writeln!(out, " 0 / END OF FIXED SHUNT DATA, BEGIN GENERATOR DATA")?;
    }

    // --- Generator Data ---

    for g in &network.generators {
        let status = if g.in_service { 1 } else { 0 };
        let qmax = clamp_finite(g.qmax, 9999.0);
        let qmin = clamp_finite(g.qmin, -9999.0);
        let pmax = clamp_finite(g.pmax, 9999.0);
        let pmin = clamp_finite(g.pmin, -9999.0);
        let mbase = clamp_finite(g.machine_base_mva, 100.0);
        let mid = g.machine_id.as_deref().unwrap_or("1");
        writeln!(
            out,
            " {},'{:2}',{:.4},{:.4},{:.4},{:.4},{:.6},{},{:.4},0,0,0,0,1.0,{},100,{:.4},{:.4},1",
            g.bus,
            mid,
            g.p,
            g.q,
            qmax,
            qmin,
            g.voltage_setpoint_pu,
            g.bus,
            mbase,
            status,
            pmax,
            pmin
        )?;
    }
    if version >= 36 {
        writeln!(
            out,
            " 0 / END OF GENERATOR DATA, BEGIN SWITCHING DEVICE RATING SET DATA"
        )?;
        for rs in &network.metadata.switching_device_rating_sets {
            write!(
                out,
                " {},{},'{}',{},{:.2},{:.2},{:.2}",
                rs.from_bus, rs.to_bus, rs.circuit, rs.rating_set, rs.rate1, rs.rate2, rs.rate3
            )?;
            for rate in &rs.additional_rates {
                write!(out, ",{rate:.2}")?;
            }
            writeln!(out)?;
        }
        writeln!(
            out,
            " 0 / END OF SWITCHING DEVICE RATING SET DATA, BEGIN BRANCH DATA"
        )?;
    } else {
        writeln!(out, " 0 / END OF GENERATOR DATA, BEGIN BRANCH DATA")?;
    }

    // --- Branch Data ---

    for br in &network.branches {
        let status = if br.in_service { 1 } else { 0 };
        let ckt = if br.circuit.is_empty() {
            "1"
        } else {
            &br.circuit
        };
        let ra = clamp_finite(br.rating_a_mva, 0.0);
        let rb = clamp_finite(br.rating_b_mva, 0.0);
        let rc = clamp_finite(br.rating_c_mva, 0.0);
        if !br.is_transformer() {
            // GI, BI, GJ, BJ are terminal shunt admittances — always zero for
            // standard branches.  The B field already carries total line charging.
            writeln!(
                out,
                " {},{},'{:2}',{:.6},{:.6},{:.6},{:.2},{:.2},{:.2},0,{:.6},0,{:.6},{},1,0,1",
                br.from_bus,
                br.to_bus,
                ckt,
                br.r,
                br.x,
                br.b,
                ra,
                rb,
                rc,
                0.0, // BI — NOT b/2 (B field already distributes charging)
                0.0, // BJ — NOT b/2
                status
            )?;
        }
        // Transformers are written in the TRANSFORMER DATA section below
    }
    if ver >= 34 {
        writeln!(
            out,
            " 0 / END OF BRANCH DATA, BEGIN SYSTEM SWITCHING DEVICE DATA"
        )?;
        writeln!(
            out,
            " 0 / END OF SYSTEM SWITCHING DEVICE DATA, BEGIN TRANSFORMER DATA"
        )?;
    } else {
        writeln!(out, " 0 / END OF BRANCH DATA, BEGIN TRANSFORMER DATA")?;
    }

    // --- Transformer Data ---

    for br in &network.branches {
        if br.is_transformer() {
            let status = if br.in_service { 1 } else { 0 };
            let ckt = if br.circuit.is_empty() {
                "1"
            } else {
                &br.circuit
            };
            let ra = clamp_finite(br.rating_a_mva, 0.0);
            let rb = clamp_finite(br.rating_b_mva, 0.0);
            let rc = clamp_finite(br.rating_c_mva, 0.0);
            // PSS/E 2-winding transformer: 4 records
            // Record 1: from, to, 0 (no star bus), ckt, cw, cz, cm, mag1, mag2, nmetr, name, stat, o1, f1
            writeln!(
                out,
                " {},{},0,'{:2}',1,1,1,{:.6},{:.6},1,'XFMR    ',{},1,1.0",
                br.from_bus, br.to_bus, ckt, br.g_mag, br.b_mag, status
            )?;
            // Record 2: r12, x12, sbase12 (pu on system base)
            writeln!(out, " {:.6},{:.6},{:.4}", br.r, br.x, network.base_mva)?;
            // Record 3: windv1, nomv1, ang1, rata1, ratb1, ratc1, cod1, cont1, rma1, rmi1, vma1, vmi1, ntp1, tab1, cr1, cx1
            writeln!(
                out,
                " {:.6},0,{:.4},{:.2},{:.2},{:.2},0,0,1.1,0.9,1.1,0.9,33,0,0,0",
                br.tap,
                br.phase_shift_rad.to_degrees(),
                ra,
                rb,
                rc
            )?;
            // Record 4: windv2, nomv2
            writeln!(out, " 1.0,0")?;
        }
    }
    writeln!(
        out,
        " 0 / END OF TRANSFORMER DATA, BEGIN AREA INTERCHANGE DATA"
    )?;

    // --- Area Interchange Data ---

    for area in &network.area_schedules {
        let name = truncate_name(&area.name, 12);
        writeln!(
            out,
            " {},{},{:.4},{:.4},'{}'",
            area.number, area.slack_bus, area.p_desired_mw, area.p_tolerance_mw, name
        )?;
    }
    writeln!(
        out,
        " 0 / END OF AREA INTERCHANGE DATA, BEGIN TWO-TERMINAL DC DATA"
    )?;

    // --- Two-Terminal DC Line Data ---

    for link in &network.hvdc.links {
        let Some(dc) = link.as_lcc() else {
            continue;
        };
        let mdc = dc.mode as u32;
        writeln!(
            out,
            " '{}',{},{:.6},{:.4},{:.4},{:.4},{:.6},{:.6},'{}',{:.4},{},{:.4}",
            sanitize_psse_name(&dc.name),
            mdc,
            dc.resistance_ohm,
            dc.scheduled_setpoint,
            dc.scheduled_voltage_kv,
            dc.voltage_mode_switch_kv,
            dc.compounding_resistance_ohm,
            dc.current_margin_ka,
            dc.meter,
            dc.voltage_min_kv,
            dc.ac_dc_iteration_max,
            dc.ac_dc_iteration_acceleration
        )?;
        write_dc_converter(&mut out, &dc.rectifier)?;
        write_dc_converter(&mut out, &dc.inverter)?;
    }
    writeln!(
        out,
        " 0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA"
    )?;

    // --- VSC DC Line Data ---

    for link in &network.hvdc.links {
        let Some(vsc) = link.as_vsc() else {
            continue;
        };
        let mdc = vsc.mode as u32;
        writeln!(
            out,
            " '{}',{},{:.6},1,1.0,0,0.0",
            sanitize_psse_name(&vsc.name),
            mdc,
            vsc.resistance_ohm
        )?;
        write_vsc_converter(&mut out, &vsc.converter1)?;
        write_vsc_converter(&mut out, &vsc.converter2)?;
    }
    writeln!(
        out,
        " 0 / END OF VSC DC LINE DATA, BEGIN IMPEDANCE CORRECTION DATA"
    )?;

    // --- Impedance Correction Data ---

    for table in &network.metadata.impedance_corrections {
        write!(out, " {}", table.number)?;
        for &(t, f) in &table.entries {
            write!(out, ",{:.6},{:.6}", t, f)?;
        }
        writeln!(out)?;
    }
    writeln!(
        out,
        " 0 / END OF IMPEDANCE CORRECTION DATA, BEGIN MULTI-TERMINAL DC DATA"
    )?;

    // --- Multi-Terminal DC Data ---

    for dc_grid in &network.hvdc.dc_grids {
        let dc_buses: Vec<_> = dc_grid.buses.iter().collect();
        let converters: Vec<_> = dc_grid
            .converters
            .iter()
            .filter_map(|converter| converter.as_lcc())
            .collect();
        if converters.is_empty() {
            continue;
        }
        let branches: Vec<_> = dc_grid.branches.iter().collect();

        let mut local_bus_number = std::collections::HashMap::new();
        for (idx, bus) in dc_buses.iter().enumerate() {
            local_bus_number.insert(bus.bus_id, (idx + 1) as u32);
        }

        let dc_voltage_kv = dc_buses.first().map(|bus| bus.base_kv_dc).unwrap_or(500.0);
        writeln!(
            out,
            " '{}',{},{},{},{},{:.4},{:.4},{:.4}",
            sanitize_psse_name(
                dc_grid
                    .name
                    .as_deref()
                    .unwrap_or(&format!("DCGRID-{}", dc_grid.id))
            ),
            converters.len(),
            dc_buses.len(),
            branches.len(),
            1,
            dc_voltage_kv,
            0.0,
            0.0
        )?;
        for converter in &converters {
            writeln!(
                out,
                " {},{},{:.4},{:.4},{:.6},{:.6},{:.4},{:.6},{:.6},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{}",
                converter.ac_bus,
                converter.n_bridges,
                converter.alpha_max_deg,
                converter.alpha_min_deg,
                converter.commutation_resistance_ohm,
                converter.commutation_reactance_ohm,
                converter.base_voltage_kv,
                converter.turns_ratio,
                converter.tap_ratio,
                converter.tap_max,
                converter.tap_min,
                converter.tap_step,
                converter.scheduled_setpoint.abs(),
                converter.power_share_percent,
                converter.current_margin_percent,
                match converter.role {
                    surge_network::network::LccDcConverterRole::Rectifier => 1,
                    surge_network::network::LccDcConverterRole::Inverter => 2,
                }
            )?;
        }
        for bus in &dc_buses {
            let ac_bus = converters
                .iter()
                .find(|converter| converter.dc_bus == bus.bus_id)
                .map(|converter| converter.ac_bus)
                .unwrap_or(0);
            let (area, zone) = if ac_bus > 0 {
                network
                    .buses
                    .iter()
                    .find(|candidate| candidate.number == ac_bus)
                    .map(|candidate| (candidate.area, candidate.zone))
                    .unwrap_or((1, 1))
            } else {
                (1, 1)
            };
            let generated_name = format!("DC-{}", bus.bus_id);
            let name = truncate_name(&generated_name, 12);
            writeln!(
                out,
                " {},{},{},{},'{}',{},{:.6},{}",
                local_bus_number[&bus.bus_id], ac_bus, area, zone, name, 0, bus.r_ground_ohm, 1
            )?;
        }
        for (idx, branch) in branches.iter().enumerate() {
            writeln!(
                out,
                " {},{},'{:2}',{},{:.6},{:.6}",
                local_bus_number[&branch.from_bus],
                local_bus_number[&branch.to_bus],
                idx + 1,
                1,
                branch.r_ohm,
                branch.l_mh
            )?;
        }
    }
    writeln!(
        out,
        " 0 / END OF MULTI-TERMINAL DC DATA, BEGIN MULTI-SECTION LINE DATA"
    )?;

    // --- Multi-Section Line Data ---

    for ms in &network.metadata.multi_section_line_groups {
        write!(
            out,
            " {},{},'{:2}',{}",
            ms.from_bus, ms.to_bus, ms.id, ms.metered_end
        )?;
        for &dum in &ms.dummy_buses {
            write!(out, ",{}", dum)?;
        }
        writeln!(out)?;
    }
    writeln!(out, " 0 / END OF MULTI-SECTION LINE DATA, BEGIN ZONE DATA")?;

    // --- Zone Data ---

    for region in &network.metadata.regions {
        let name = truncate_name(&region.name, 12);
        writeln!(out, " {},'{}'", region.number, name)?;
    }
    writeln!(out, " 0 / END OF ZONE DATA, BEGIN INTER-AREA TRANSFER DATA")?;

    // --- Inter-Area Transfer Data ---

    for xfer in &network.metadata.scheduled_area_transfers {
        writeln!(
            out,
            " {},{},{},{:.4}",
            xfer.from_area, xfer.to_area, xfer.id, xfer.p_transfer_mw
        )?;
    }
    writeln!(
        out,
        " 0 / END OF INTER-AREA TRANSFER DATA, BEGIN OWNER DATA"
    )?;

    // --- Owner Data ---

    for owner in &network.metadata.owners {
        let name = truncate_name(&owner.name, 12);
        writeln!(out, " {},'{}'", owner.number, name)?;
    }
    writeln!(
        out,
        " 0 / END OF OWNER DATA, BEGIN FACTS CONTROL DEVICE DATA"
    )?;

    // --- FACTS Device Data ---

    for f in &network.facts_devices {
        let mode = f.mode as u32;
        writeln!(
            out,
            " '{}',{},{},{},{:.4},{:.4},{:.6},{:.4},1.1,0.9,1.1,99999,99999,{:.6},100,1",
            sanitize_psse_name(&f.name),
            f.bus_from,
            f.bus_to,
            mode,
            f.p_setpoint_mw,
            f.q_setpoint_mvar,
            f.voltage_setpoint_pu,
            clamp_finite(f.q_max, 9999.0),
            f.series_reactance_pu
        )?;
    }
    writeln!(
        out,
        " 0 / END OF FACTS CONTROL DEVICE DATA, BEGIN SWITCHED SHUNT DATA"
    )?;

    // --- Switched Shunt Data ---
    // Group SwitchedShunt objects by bus index, reconstruct PSS/E block format.

    write_switched_shunts(&mut out, network)?;

    if ver >= 35 {
        // v35+ sections not present in v33.
        writeln!(
            out,
            " 0 / END OF SWITCHED SHUNT DATA, BEGIN GNE DEVICE DATA"
        )?;
        // GNE (General Network Element) — Surge has no GNE model; emit empty section.
        writeln!(
            out,
            " 0 / END OF GNE DEVICE DATA, BEGIN INDUCTION MACHINE DATA"
        )?;
        // Induction Machine Data — written from network.induction_machines when present.
        write_induction_machines(&mut out, network)?;
        writeln!(
            out,
            " 0 / END OF INDUCTION MACHINE DATA, BEGIN SUBSTATION DATA"
        )?;
        write_substation_data(&mut out, network)?;
        writeln!(out, " 0 / END OF SUBSTATION DATA")?;
    } else {
        writeln!(out, " 0 / END OF SWITCHED SHUNT DATA")?;
    }

    writeln!(out, "Q")?;

    Ok(out)
}

/// Write one LCC-HVDC converter record (rectifier or inverter).
fn write_dc_converter(
    out: &mut String,
    c: &surge_network::network::LccConverterTerminal,
) -> Result<(), PsseWriteError> {
    let ic = if c.in_service { 1 } else { 0 };
    writeln!(
        out,
        " {},{},{:.4},{:.4},{:.6},{:.6},{:.4},{:.6},{:.6},{:.6},{:.6},{:.6},{},0,0,'1 ',0",
        c.bus,
        c.n_bridges,
        c.alpha_max,
        c.alpha_min,
        c.commutation_resistance_ohm,
        c.commutation_reactance_ohm,
        c.base_voltage_kv,
        c.turns_ratio,
        c.tap,
        c.tap_max,
        c.tap_min,
        c.tap_step,
        ic
    )?;
    Ok(())
}

/// Write one VSC-HVDC converter record.
fn write_vsc_converter(
    out: &mut String,
    c: &surge_network::network::VscConverterTerminal,
) -> Result<(), PsseWriteError> {
    let state = if c.in_service { 1 } else { 0 };
    let mode = c.control_mode as u32;
    writeln!(
        out,
        " {},1,{},{:.4},{:.4},{:.4},{:.4},0,{:.4},{:.4},{:.4},{:.4},1,{}",
        c.bus,
        mode,
        c.dc_setpoint,
        c.ac_setpoint,
        c.loss_constant_mw,
        c.loss_linear,
        c.q_max_mvar,
        c.q_min_mvar,
        c.voltage_max_pu,
        c.voltage_min_pu,
        state
    )?;
    Ok(())
}

/// Write switched shunt records, grouping SwitchedShunt objects by bus.
fn write_switched_shunts(out: &mut String, network: &Network) -> Result<(), PsseWriteError> {
    use std::collections::BTreeMap;

    // Group switched shunts by external bus number (BTreeMap for deterministic order).
    let mut groups: BTreeMap<u32, Vec<&SwitchedShunt>> = BTreeMap::new();
    for ss in &network.controls.switched_shunts {
        groups.entry(ss.bus).or_default().push(ss);
    }

    for group in groups.values() {
        let first = group[0];
        let bus_num = first.bus;
        let vswhi = first.v_target + first.v_band / 2.0;
        let vswlo = first.v_target - first.v_band / 2.0;
        let swrem = if first.bus_regulated != first.bus {
            first.bus_regulated
        } else {
            0
        };

        // Compute BINIT and block list.
        let mut binit = 0.0;
        let mut blocks: Vec<(i32, f64)> = Vec::new();
        for ss in group {
            let b_mvar = ss.b_step * network.base_mva;
            binit += ss.n_active_steps as f64 * b_mvar;
            if ss.n_steps_cap > 0 {
                blocks.push((ss.n_steps_cap, b_mvar));
            }
            if ss.n_steps_react > 0 {
                blocks.push((ss.n_steps_react, -b_mvar));
            }
        }

        // I, MODSW, ADJM, STAT, VSWHI, VSWLO, SWREM, RMPCT, RMIDNT, BINIT, [N1, B1, ...]
        write!(
            out,
            " {},1,0,1,{:.4},{:.4},{},100,'',{:.4}",
            bus_num, vswhi, vswlo, swrem, binit
        )?;
        for (n, b) in &blocks {
            write!(out, ",{},{:.4}", n, b)?;
        }
        writeln!(out)?;
    }

    Ok(())
}

/// Truncate a name to at most `max_len` characters (PSS/E field width limit).
fn truncate_name(name: &str, max_len: usize) -> &str {
    let n = name.trim();
    if n.len() > max_len { &n[..max_len] } else { n }
}

/// Clamp a value to a finite fallback if it is non-finite (±Inf, NaN) or at the
/// f64::MAX/f64::MIN sentinel used internally for "unlimited".
///
/// PSS/E RAW format uses plain text floats — `inf` and `nan` are not valid tokens.
fn clamp_finite(v: f64, fallback: f64) -> f64 {
    if !v.is_finite() || v >= f64::MAX / 2.0 || v <= f64::MIN / 2.0 {
        fallback
    } else {
        v
    }
}

/// Sanitize a network name for the PSS/E header (avoid commas/quotes).
fn sanitize_psse_name(name: &str) -> String {
    name.chars()
        .filter(|&c| c != '\'' && c != '"' && c != '\n')
        .collect()
}

/// Write the INDUCTION MACHINE DATA section (v35+).
///
/// Emits two-line records for each `InductionMachine` in `network.induction_machines`.
fn write_induction_machines(out: &mut String, network: &Network) -> Result<(), PsseWriteError> {
    for m in &network.induction_machines {
        let stat = if m.in_service { 1 } else { 0 };
        // Line 1: I, ID, STAT, SCODE, DCODE, AREA, ZONE, OWNER, TCODE, BCODE,
        //         MBASE, RATEKV, PCODE, PSET, H, A, B, D, E, F
        writeln!(
            out,
            " {},'{:<2}',{},1,3,{},{},{},1,1,{:.4},{:.4},1,{:.4},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
            m.bus,
            m.id,
            stat,
            m.area,
            m.zone,
            m.owner,
            m.mbase,
            m.rate_kv,
            m.pset,
            m.h,
            m.a,
            m.b,
            m.d,
            m.e,
            m.f_coeff
        )?;
        // Line 2: RA, XA, XM, R1, X1, R2, X2, X3, E1, SE1, E2, SE2, IA1, IA2, XAMULT
        writeln!(
            out,
            " {:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},1.0,0.0,1.2,0.0,0.0,0.0,1.0",
            m.ra, m.xa, m.xm, m.r1, m.x1, m.r2, m.x2, m.x3
        )?;
    }
    Ok(())
}

/// Write the SUBSTATION DATA section (v35+).
///
/// Reconstructs PSS/E-style SUBSTATION / NODE / SWITCHING DEVICE / TERMINAL records
/// from the `NodeBreakerTopology` stored on the network (when present).  The IDs stored
/// by the PSS/E parser are in the form `"SUB_{isub}"` and `"SUB_{isub}_N{inode}"`,
/// which lets us recover the original numeric indices.
fn write_substation_data(out: &mut String, network: &Network) -> Result<(), PsseWriteError> {
    let Some(sm) = &network.topology else {
        return Ok(());
    };

    use std::collections::HashMap;

    // O(vl) — maps voltage_level_id → substation_id
    let vl_to_sub: HashMap<&str, &str> = sm
        .voltage_levels
        .iter()
        .map(|vl| (vl.id.as_str(), vl.substation_id.as_str()))
        .collect();

    // O(cn) — group connectivity nodes by substation_id
    let mut cn_by_sub: HashMap<&str, Vec<&surge_network::network::topology::ConnectivityNode>> =
        HashMap::new();
    for cn in &sm.connectivity_nodes {
        let sub_id = vl_to_sub
            .get(cn.voltage_level_id.as_str())
            .copied()
            .unwrap_or("");
        cn_by_sub.entry(sub_id).or_default().push(cn);
    }

    // O(sw) — map cn1_id → substation_id for switch filtering
    let cn_to_sub: HashMap<&str, &str> = sm
        .connectivity_nodes
        .iter()
        .map(|cn| {
            let sub_id = vl_to_sub
                .get(cn.voltage_level_id.as_str())
                .copied()
                .unwrap_or("");
            (cn.id.as_str(), sub_id)
        })
        .collect();

    // O(sw) — group switches by substation_id (keyed on cn1)
    let mut sw_by_sub: HashMap<&str, Vec<&surge_network::network::SwitchDevice>> = HashMap::new();
    for sw in &sm.switches {
        let sub_id = cn_to_sub.get(sw.cn1_id.as_str()).copied().unwrap_or("");
        sw_by_sub.entry(sub_id).or_default().push(sw);
    }

    for sub in &sm.substations {
        // Extract numeric ISUB from "SUB_{n}" or fall back to 0.
        let isub: u32 = sub
            .id
            .strip_prefix("SUB_")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let sub_name = truncate_name(&sub.name, 12);
        writeln!(out, " {isub},'{sub_name}',0.0,0.0,1")?;
        writeln!(out, " 0 / BEGIN SUBSTATION NODE DATA")?;

        // Emit nodes belonging to this substation — O(cn_in_sub).
        let sub_id_prefix = format!("SUB_{isub}_N");
        if let Some(cns) = cn_by_sub.get(sub.id.as_str()) {
            for cn in cns {
                let inode: u32 = cn
                    .id
                    .strip_prefix(&sub_id_prefix)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let node_name = truncate_name(&cn.name, 12);
                writeln!(out, " {inode},'{node_name}',0,1,1.0,0.0")?;
            }
        }
        writeln!(
            out,
            " 0 / END OF SUBSTATION NODE DATA, BEGIN SUBSTATION SWITCHING DEVICE DATA"
        )?;

        // Emit switches belonging to this substation — O(sw_in_sub).
        if let Some(switches) = sw_by_sub.get(sub.id.as_str()) {
            for sw in switches {
                let sw_name = truncate_name(&sw.name, 12);
                let sw_type_code: u32 = match sw.switch_type {
                    surge_network::network::SwitchType::Breaker => 1,
                    surge_network::network::SwitchType::Disconnector => 2,
                    _ => 3,
                };
                let status = if sw.open { 0 } else { 1 };
                writeln!(out, " 0,'{}',0,0,{},{}", sw_name, sw_type_code, status)?;
            }
        }
        writeln!(
            out,
            " 0 / END OF SUBSTATION SWITCHING DEVICE DATA, BEGIN SUBSTATION TERMINAL DATA"
        )?;
        writeln!(out, " 0 / END OF SUBSTATION TERMINAL DATA")?;
    }

    Ok(())
}

/// Format a bus name: use existing name if non-empty, else "BUS_{number}".
fn format_bus_name(name: &str, number: u32) -> String {
    let n = name.trim();
    if n.is_empty() {
        format!("BUS_{number:06}")
    } else if n.len() > 12 {
        n[..12].to_string()
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn simple_network() -> Network {
        let mut net = Network::new("case9");
        net.base_mva = 100.0;
        let mut slack = Bus::new(1, BusType::Slack, 345.0);
        slack.voltage_magnitude_pu = 1.04;
        net.buses.push(slack);
        let pq = Bus::new(2, BusType::PQ, 345.0);
        net.buses.push(pq);
        net.loads.push(Load::new(2, 125.0, 50.0));
        net.generators.push(Generator::new(1, 72.3, 1.04));
        net.branches
            .push(Branch::new_line(1, 2, 0.01938, 0.05917, 0.0528));
        net
    }

    #[test]
    fn test_psse_header() {
        let net = simple_network();
        let s = to_string(&net, 33).unwrap();
        assert!(s.contains("33"));
        assert!(s.contains("END OF BUS DATA"));
        assert!(s.contains("END OF GENERATOR DATA"));
        assert!(s.contains("END OF BRANCH DATA"));
        assert!(s.ends_with("Q\n") || s.ends_with("Q"));
    }

    #[test]
    fn test_psse_bus_count() {
        let net = simple_network();
        let s = to_string(&net, 33).unwrap();
        // Both buses appear in bus section
        assert!(s.contains(" 1,") || s.contains("\n 1,"));
        assert!(s.contains(" 2,") || s.contains("\n 2,"));
    }

    #[test]
    fn test_psse_roundtrip() {
        use crate::psse::parse_str;
        let net = simple_network();
        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.n_buses(), net.n_buses());
        // Generators should survive (1 generator)
        assert_eq!(net2.generators.len(), 1);
    }

    #[test]
    fn test_psse_load_id_and_owner_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::OwnershipEntry;

        let mut net = simple_network();
        net.loads[0].id = "LD1".to_string();
        net.loads[0].owners = vec![OwnershipEntry {
            owner: 7,
            fraction: 1.0,
        }];

        let s = to_string(&net, 33).unwrap();
        assert!(s.contains("'LD1'"));
        assert!(s.contains(",7,1"));

        let parsed = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(parsed.loads.len(), 1);
        assert_eq!(parsed.loads[0].id, "LD1");
        assert_eq!(parsed.loads[0].owners.len(), 1);
        assert_eq!(parsed.loads[0].owners[0].owner, 7);
    }

    #[test]
    fn test_psse_file_write() {
        let net = simple_network();
        let tmp = std::env::temp_dir().join("surge_psse_writer_test.raw");
        write_file(&net, &tmp, 33).unwrap();
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("END OF BUS DATA"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_default_version() {
        let net = simple_network();
        let s = to_string(&net, 0).unwrap();
        // version 0 → default to 33
        assert!(s.contains("33"));
    }

    // -----------------------------------------------------------------------
    // Round-trip tests for all sections
    // -----------------------------------------------------------------------

    #[test]
    fn test_machine_id_roundtrip() {
        use crate::psse::parse_str;
        let mut net = simple_network();
        net.generators[0].machine_id = Some("G1".to_string());
        let mut g2 = Generator::new(1, 50.0, 1.04);
        g2.machine_id = Some("G2".to_string());
        net.generators.push(g2);

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.generators.len(), 2);
        assert_eq!(net2.generators[0].machine_id.as_deref(), Some("G1"));
        assert_eq!(net2.generators[1].machine_id.as_deref(), Some("G2"));
    }

    #[test]
    fn test_circuit_id_roundtrip() {
        use crate::psse::parse_str;
        let mut net = simple_network();
        net.branches[0].circuit = "1".to_string();
        let mut br2 = Branch::new_line(1, 2, 0.02, 0.06, 0.05);
        br2.circuit = "2".to_string();
        net.branches.push(br2);

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.branches.len(), 2);
        assert_eq!(net2.branches[0].circuit, "1");
        assert_eq!(net2.branches[1].circuit, "2");
    }

    #[test]
    fn test_transformer_magnetizing_roundtrip() {
        use crate::psse::parse_str;
        let mut net = simple_network();
        let mut xfmr = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        xfmr.tap = 1.05;
        xfmr.g_mag = 0.001;
        xfmr.b_mag = -0.05;
        xfmr.circuit = "1".to_string();
        net.branches.push(xfmr);

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        // Find the transformer branch
        let xf = net2
            .branches
            .iter()
            .find(|b| b.is_transformer())
            .expect("transformer not found");
        assert!((xf.g_mag - 0.001).abs() < 1e-5, "g_mag={}", xf.g_mag);
        assert!((xf.b_mag - (-0.05)).abs() < 1e-5, "b_mag={}", xf.b_mag);
    }

    #[test]
    fn test_area_schedule_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::AreaSchedule;
        let mut net = simple_network();
        net.area_schedules.push(AreaSchedule {
            number: 1,
            slack_bus: 1,
            p_desired_mw: 150.0,
            p_tolerance_mw: 10.0,
            name: "AREA1".to_string(),
        });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.area_schedules.len(), 1);
        assert_eq!(net2.area_schedules[0].number, 1);
        assert_eq!(net2.area_schedules[0].slack_bus, 1);
        assert!((net2.area_schedules[0].p_desired_mw - 150.0).abs() < 1e-2);
        assert!(net2.area_schedules[0].name.contains("AREA1"));
    }

    #[test]
    fn test_dc_line_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::{LccConverterTerminal, LccHvdcControlMode, LccHvdcLink};
        let mut net = simple_network();
        net.hvdc.push_lcc_link(LccHvdcLink {
            name: "HVDC1".to_string(),
            mode: LccHvdcControlMode::PowerControl,
            resistance_ohm: 5.0,
            scheduled_setpoint: 500.0,
            scheduled_voltage_kv: 400.0,
            voltage_mode_switch_kv: 0.0,
            compounding_resistance_ohm: 0.0,
            current_margin_ka: 0.0,
            meter: 'R',
            voltage_min_kv: 0.0,
            ac_dc_iteration_max: 20,
            ac_dc_iteration_acceleration: 1.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                n_bridges: 2,
                alpha_max: 90.0,
                alpha_min: 5.0,
                commutation_resistance_ohm: 0.5,
                commutation_reactance_ohm: 10.0,
                base_voltage_kv: 345.0,
                turns_ratio: 1.0,
                tap: 1.0,
                tap_max: 1.1,
                tap_min: 0.9,
                tap_step: 0.00625,
                in_service: true,
            },
            inverter: LccConverterTerminal {
                bus: 2,
                n_bridges: 2,
                alpha_max: 90.0,
                alpha_min: 5.0,
                commutation_resistance_ohm: 0.5,
                commutation_reactance_ohm: 10.0,
                base_voltage_kv: 345.0,
                turns_ratio: 1.0,
                tap: 1.0,
                tap_max: 1.1,
                tap_min: 0.9,
                tap_step: 0.00625,
                in_service: true,
            },
        });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        let dc = net2.hvdc.links[0].as_lcc().expect("lcc link");
        assert_eq!(net2.hvdc.links.len(), 1);
        assert!(dc.name.contains("HVDC1"));
        assert!((dc.resistance_ohm - 5.0).abs() < 1e-4);
        assert!((dc.scheduled_setpoint - 500.0).abs() < 1e-2);
        assert_eq!(dc.rectifier.bus, 1);
        assert_eq!(dc.inverter.bus, 2);
    }

    #[test]
    fn test_facts_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::{FactsDevice, FactsMode};
        let mut net = simple_network();
        net.facts_devices.push(FactsDevice {
            name: "SVC1".to_string(),
            bus_from: 1,
            bus_to: 0,
            mode: FactsMode::ShuntOnly,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 50.0,
            voltage_setpoint_pu: 1.02,
            q_max: 200.0,
            series_reactance_pu: 0.05,
            in_service: true,
            ..FactsDevice::default()
        });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.facts_devices.len(), 1);
        let f = &net2.facts_devices[0];
        assert!(f.name.contains("SVC1"));
        assert_eq!(f.bus_from, 1);
        assert_eq!(f.mode, FactsMode::ShuntOnly);
        assert!((f.voltage_setpoint_pu - 1.02).abs() < 1e-4);
    }

    #[test]
    fn test_switched_shunt_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::SwitchedShunt;
        let mut net = simple_network();
        // Add a controlled switched shunt: 3 cap steps × 50 Mvar each,
        // 2 steps active, bus 1, self-regulating.
        net.controls.switched_shunts.push(SwitchedShunt {
            id: "ssh_1".into(),
            bus: 1,
            bus_regulated: 1,
            b_step: 0.5, // 50 Mvar / 100 MVA = 0.5 pu
            n_steps_cap: 3,
            n_steps_react: 0,
            v_target: 1.0,
            v_band: 0.10,
            n_active_steps: 2,
        });

        let s = to_string(&net, 33).unwrap();
        assert!(s.contains("SWITCHED SHUNT"), "section marker missing");

        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(
            net2.controls.switched_shunts.len(),
            1,
            "expected 1 switched shunt, got {}",
            net2.controls.switched_shunts.len()
        );
        let ss = &net2.controls.switched_shunts[0];
        assert_eq!(ss.n_steps_cap, 3);
        // b_step should be 50 Mvar / 100 MVA = 0.5 pu
        assert!(
            (ss.b_step - 0.5).abs() < 0.01,
            "b_step={}, expected ~0.5",
            ss.b_step
        );
        assert_eq!(ss.n_active_steps, 2);
    }

    #[test]
    fn test_region_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::Region;
        let mut net = simple_network();
        net.metadata.regions.push(Region {
            number: 1,
            name: "NORTH".to_string(),
        });
        net.metadata.regions.push(Region {
            number: 2,
            name: "SOUTH".to_string(),
        });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.metadata.regions.len(), 2);
        assert_eq!(net2.metadata.regions[0].number, 1);
        assert!(net2.metadata.regions[0].name.contains("NORTH"));
        assert_eq!(net2.metadata.regions[1].number, 2);
        assert!(net2.metadata.regions[1].name.contains("SOUTH"));
    }

    #[test]
    fn test_owner_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::Owner;
        let mut net = simple_network();
        net.metadata.owners.push(Owner {
            number: 1,
            name: "UTILITY_A".to_string(),
        });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.metadata.owners.len(), 1);
        assert_eq!(net2.metadata.owners[0].number, 1);
        assert!(net2.metadata.owners[0].name.contains("UTILITY_A"));
    }

    #[test]
    fn test_scheduled_area_transfer_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::scheduled_area_transfer::ScheduledAreaTransfer;
        let mut net = simple_network();
        net.metadata
            .scheduled_area_transfers
            .push(ScheduledAreaTransfer {
                from_area: 1,
                to_area: 2,
                id: 1,
                p_transfer_mw: 250.0,
            });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.metadata.scheduled_area_transfers.len(), 1);
        let xfer = &net2.metadata.scheduled_area_transfers[0];
        assert_eq!(xfer.from_area, 1);
        assert_eq!(xfer.to_area, 2);
        assert!((xfer.p_transfer_mw - 250.0).abs() < 1e-2);
    }

    #[test]
    fn test_impedance_correction_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::impedance_correction::ImpedanceCorrectionTable;
        let mut net = simple_network();
        net.metadata
            .impedance_corrections
            .push(ImpedanceCorrectionTable {
                number: 1,
                entries: vec![(0.9, 1.1), (1.0, 1.0), (1.1, 0.95)],
            });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.metadata.impedance_corrections.len(), 1);
        let table = &net2.metadata.impedance_corrections[0];
        assert_eq!(table.number, 1);
        assert_eq!(table.entries.len(), 3);
        assert!((table.entries[0].0 - 0.9).abs() < 1e-4);
        assert!((table.entries[0].1 - 1.1).abs() < 1e-4);
        assert!((table.entries[2].1 - 0.95).abs() < 1e-4);
    }

    #[test]
    fn test_multi_section_line_roundtrip() {
        use crate::psse::parse_str;
        use surge_network::network::multi_section_line::MultiSectionLineGroup;
        let mut net = simple_network();
        // Add a dummy bus for the multi-section line
        net.buses.push(Bus::new(3, BusType::PQ, 345.0));
        net.metadata
            .multi_section_line_groups
            .push(MultiSectionLineGroup {
                from_bus: 1,
                to_bus: 2,
                id: "1".to_string(),
                metered_end: 1,
                dummy_buses: vec![3],
            });

        let s = to_string(&net, 33).unwrap();
        let net2 = parse_str(&s).expect("round-trip parse failed");
        assert_eq!(net2.metadata.multi_section_line_groups.len(), 1);
        let ms = &net2.metadata.multi_section_line_groups[0];
        assert_eq!(ms.from_bus, 1);
        assert_eq!(ms.to_bus, 2);
        assert_eq!(ms.dummy_buses, vec![3]);
    }

    #[test]
    fn test_all_sections_present_in_output() {
        let net = simple_network();
        let s = to_string(&net, 33).unwrap();
        // Verify all section markers exist in order
        assert!(s.contains("END OF BUS DATA"));
        assert!(s.contains("END OF LOAD DATA"));
        assert!(s.contains("END OF FIXED SHUNT DATA"));
        assert!(s.contains("END OF GENERATOR DATA"));
        assert!(s.contains("END OF BRANCH DATA"));
        assert!(s.contains("END OF TRANSFORMER DATA"));
        assert!(s.contains("END OF AREA INTERCHANGE DATA"));
        assert!(s.contains("END OF TWO-TERMINAL DC DATA"));
        assert!(s.contains("END OF VSC DC LINE DATA"));
        assert!(s.contains("END OF IMPEDANCE CORRECTION DATA"));
        assert!(s.contains("END OF MULTI-TERMINAL DC DATA"));
        assert!(s.contains("END OF MULTI-SECTION LINE DATA"));
        assert!(s.contains("END OF ZONE DATA"));
        assert!(s.contains("END OF INTER-AREA TRANSFER DATA"));
        assert!(s.contains("END OF OWNER DATA"));
        assert!(s.contains("END OF FACTS CONTROL DEVICE DATA"));
        assert!(s.contains("END OF SWITCHED SHUNT DATA"));
    }

    /// v35 writer emits GNE, Induction Machine, and Substation section markers.
    #[test]
    fn test_v35_sections_present() {
        let net = simple_network();
        let s = to_string(&net, 35).unwrap();
        assert!(
            s.contains("END OF SYSTEM SWITCHING DEVICE DATA"),
            "v35 output missing System Switching Device section"
        );
        assert!(
            s.contains("END OF GNE DEVICE DATA"),
            "v35 output missing GNE section"
        );
        assert!(
            s.contains("END OF INDUCTION MACHINE DATA"),
            "v35 output missing Induction Machine section"
        );
        assert!(
            s.contains("END OF SUBSTATION DATA"),
            "v35 output missing Substation section"
        );
    }

    /// v33 writer must NOT emit v35+ sections.
    #[test]
    fn test_v33_no_v35_sections() {
        let net = simple_network();
        let s = to_string(&net, 33).unwrap();
        assert!(
            !s.contains("SYSTEM SWITCHING DEVICE"),
            "v33 output should not contain System Switching Device section"
        );
        assert!(
            !s.contains("GNE DEVICE"),
            "v33 output should not contain GNE section"
        );
        assert!(
            !s.contains("INDUCTION MACHINE"),
            "v33 output should not contain Induction Machine section"
        );
        assert!(
            !s.contains("SUBSTATION DATA"),
            "v33 output should not contain Substation section"
        );
    }
}
